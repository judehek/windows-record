use log::{debug, info};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Barrier;
use windows::core::{implement, IUnknown};
use windows::core::{ComInterface, Result};
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::*;

use crate::types::SendableSample;

#[derive(Clone)]
#[implement(IMMNotificationClient)]
struct AudioEndpointVolumeCallback;

impl IMMNotificationClient_Impl for AudioEndpointVolumeCallback {
    fn OnDeviceStateChanged(
        &self,
        _device_id: &windows::core::PCWSTR,
        _new_state: u32,
    ) -> Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _device_id: &windows::core::PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _device_id: &windows::core::PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(
        &self,
        _flow: EDataFlow,
        _role: ERole,
        _default_device_id: &windows::core::PCWSTR,
    ) -> Result<()> {
        Ok(())
    }
    fn OnPropertyValueChanged(
        &self,
        _device_id: &windows::core::PCWSTR,
        _key: &windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY,
    ) -> Result<()> {
        Ok(())
    }
}

pub unsafe fn collect_microphone(
    send: Sender<SendableSample>,
    recording: Arc<AtomicBool>,
    started: Arc<Barrier>,
) -> Result<()> {
    // Validate thread priority setting
    let priority_result = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    if priority_result.as_bool() == false {
        info!("Failed to set thread priority: {}", GetLastError().0);
    }

    // Get and validate QPC frequency
    let mut qpc_freq = 0;
    if !QueryPerformanceFrequency(&mut qpc_freq).as_bool() {
        return Err(E_FAIL.into());
    }
    if qpc_freq <= 0 {
        info!("Invalid QPC frequency: {}", qpc_freq);
        return Err(E_FAIL.into());
    }
    let ticks_to_hns = 10000000.0 / qpc_freq as f64;
    info!(
        "QPC frequency: {}, ticks_to_hns: {}",
        qpc_freq, ticks_to_hns
    );

    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM.try_into().unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };

    let microphone_client = match setup_microphone_client(&wave_format) {
        Ok(client) => client,
        Err(e) => {
            info!("Failed to setup audio client: {:?}", e);
            return Err(e);
        }
    };
    info!("setup!");

    let capture_client: IAudioCaptureClient = match microphone_client.GetService() {
        Ok(client) => client,
        Err(e) => {
            info!("Failed to get capture client: {:?}", e);
            return Err(e);
        }
    };

    let packet_duration =
        std::time::Duration::from_nanos((1000000000.0 / wave_format.nSamplesPerSec as f64) as u64);
    let packet_duration_hns = packet_duration.as_nanos() as i64 / 100;

    // Get and validate initial QPC value
    let mut start_qpc_i64: i64 = 0;
    if !QueryPerformanceCounter(&mut start_qpc_i64).as_bool() {
        info!("Failed to get initial QPC value");
        return Err(E_FAIL.into());
    }
    if start_qpc_i64 <= 0 {
        info!("Invalid initial QPC value: {}", start_qpc_i64);
        return Err(E_FAIL.into());
    }
    let start_qpc = start_qpc_i64 as u64;
    info!("Initial QPC value: {}", start_qpc);

    // Track timing statistics
    let mut last_packet_time = start_qpc;
    let mut zero_packet_count = 0;
    let mut total_packets = 0;

    match microphone_client.Start() {
        Ok(_) => info!("Audio client started successfully"),
        Err(e) => {
            info!("Failed to start audio client: {:?}", e);
            return Err(e);
        }
    }

    started.wait();

    while recording.load(Ordering::Relaxed) {
        let next_packet_size = match capture_client.GetNextPacketSize() {
            Ok(size) => size,
            Err(e) => {
                info!("Failed to get next packet size: {:?}", e);
                return Err(e);
            }
        };

        if next_packet_size > 0 {
            total_packets += 1;
            zero_packet_count = 0;

            let mut buffer = ptr::null_mut();
            let mut num_frames_available = 0;
            let mut flags = 0;
            let mut device_position = 0;
            let mut qpc_position: u64 = 0;

            match capture_client.GetBuffer(
                &mut buffer,
                &mut num_frames_available,
                &mut flags,
                Some(&mut device_position),
                Some(&mut qpc_position as *mut u64),
            ) {
                Ok(_) => {
                    if qpc_position <= last_packet_time {
                        info!(
                            "QPC time went backwards: current={}, last={}",
                            qpc_position, last_packet_time
                        );
                    }
                    last_packet_time = qpc_position;

                    if (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) == 0 {
                        let relative_qpc = qpc_position - start_qpc;
                        let time_hns = (relative_qpc as f64 * ticks_to_hns) as i64;

                        match create_microphone_sample(
                            buffer,
                            num_frames_available,
                            &wave_format,
                            time_hns,
                            packet_duration_hns,
                        ) {
                            Ok(sample) => {
                                if let Err(e) = send.send(SendableSample(Arc::new(sample))) {
                                    info!("Failed to send audio sample: {:?}", e);
                                    return Err(E_FAIL.into());
                                }
                            }
                            Err(e) => {
                                info!("Failed to create audio sample: {:?}", e);
                                return Err(e);
                            }
                        }
                    }

                    match capture_client.ReleaseBuffer(num_frames_available) {
                        Ok(_) => {}
                        Err(e) => {
                            info!("Failed to release buffer: {:?}", e);
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    info!("Failed to get buffer: {:?}", e);
                    return Err(e);
                }
            }
        } else {
            zero_packet_count += 1;
            if zero_packet_count >= 1000 {
                info!(
                    "No audio data received for {} consecutive checks",
                    zero_packet_count
                );
                zero_packet_count = 0;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    info!(
        "Recording stopped. Total packets processed: {}",
        total_packets
    );
    match microphone_client.Stop() {
        Ok(_) => info!("Audio client stopped successfully"),
        Err(e) => info!("Error stopping audio client: {:?}", e),
    }

    Ok(())
}

unsafe fn setup_microphone_client(wave_format: &WAVEFORMATEX) -> Result<IAudioClient> {
    // Initialize COM if not already initialized
    let coinit_result = CoInitializeEx(None, COINIT_MULTITHREADED);
    match coinit_result {
        Ok(_) => info!("COM initialized successfully"),
        Err(e) => {
            // Don't fail on CO_E_ALREADYINITIALIZED
            if e.code() != CO_E_ALREADYINITIALIZED {
                return Err(e);
            }
            info!("COM already initialized: {:?}", e);
        }
    }

    // Initialize Media Foundation
    let mf_result = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    if let Err(e) = mf_result {
        info!("Media Foundation initialization failed: {:?}", e);
        return Err(e);
    }
    info!("Media Foundation initialized successfully");

    // Create device enumerator with explicit error handling
    info!("Creating device enumerator...");
    let enumerator: IMMDeviceEnumerator =
        match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            Ok(enum_) => enum_,
            Err(e) => {
                info!("Failed to create device enumerator: {:?}", e);
                return Err(e);
            }
        };
    info!("Device enumerator created");

    // Get default audio endpoint with explicit error handling
    info!("Getting default audio endpoint...");
    let device = match enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) {
        Ok(dev) => dev,
        Err(e) => {
            info!("Failed to get default audio endpoint: {:?}", e);
            return Err(e);
        }
    };
    info!("Got default audio endpoint");

    // Set up the callback
    info!("Creating callback...");
    let callback = AudioEndpointVolumeCallback;
    let callback_interface: IMMNotificationClient = callback.into();

    info!("Registering endpoint notification callback...");
    if let Err(e) = enumerator.RegisterEndpointNotificationCallback(&callback_interface) {
        info!("Failed to register callback: {:?}", e);
        return Err(e);
    }
    info!("Callback registered");

    // Activate audio client with explicit error handling
    info!("Activating audio client...");
    let audio_client: IAudioClient = match device.Activate(CLSCTX_ALL, None) {
        Ok(client) => client,
        Err(e) => {
            info!("Failed to activate audio client: {:?}", e);
            return Err(e);
        }
    };
    info!("Audio client activated");

    // Get device period with explicit error handling
    info!("Getting device period...");
    let mut default_period = 0;
    let mut minimum_period = 0;
    if let Err(e) =
        audio_client.GetDevicePeriod(Some(&mut default_period), Some(&mut minimum_period))
    {
        info!("Failed to get device period: {:?}", e);
        return Err(e);
    }
    info!(
        "Device periods - default: {}, minimum: {}",
        default_period, minimum_period
    );

    // Initialize audio client with proper flags and buffer duration
    info!("Initializing audio client...");
    let init_result = audio_client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, // Add event callback flag
        default_period * 2,                // Double the buffer duration for safety
        0,
        wave_format,
        None,
    );
    info!("fdgfdgf");

    match init_result {
        Ok(_) => {
            info!("Audio client initialized successfully");

            // Create and set event
            let event = CreateEventW(None, false, false, None)?;
            audio_client.SetEventHandle(event)?;

            // Get the actual buffer size
            let buffer_size = audio_client.GetBufferSize()?;
            info!("Buffer size: {} frames", buffer_size);

            Ok(audio_client)
        }
        Err(e) => {
            info!("Failed to initialize audio client: {:?}", e);
            Err(e)
        }
    }
}

unsafe fn create_microphone_sample(
    buffer: *mut u8,
    num_frames: u32,
    wave_format: &WAVEFORMATEX,
    time_hns: i64,
    packet_duration_hns: i64,
) -> Result<IMFSample> {
    // Add validation
    info!("checking if buffer is null");
    if buffer.is_null() {
        return Err(E_POINTER.into());
    }

    let buffer_size = num_frames as usize * wave_format.nBlockAlign as usize;
    info!(
        "Creating slice with buffer: {:?}, frames: {}, size: {}",
        buffer, num_frames, buffer_size
    );

    // Check for potential overflow
    if buffer_size > isize::MAX as usize {
        return Err(E_INVALIDARG.into());
    }

    // Verify pointer alignment
    if (buffer as usize) % std::mem::align_of::<u8>() != 0 {
        return Err(E_INVALIDARG.into());
    }

    let audio_data = std::slice::from_raw_parts(buffer, buffer_size);

    let sample: IMFSample = MFCreateSample()?;
    let media_buffer: IMFMediaBuffer = MFCreateMemoryBuffer(buffer_size as u32)?;

    let mut buffer_ptr: *mut u8 = ptr::null_mut();
    let mut max_length = 0;
    let mut current_length = 0;

    media_buffer.Lock(
        &mut buffer_ptr,
        Some(&mut max_length),
        Some(&mut current_length),
    )?;
    ptr::copy_nonoverlapping(audio_data.as_ptr(), buffer_ptr, buffer_size);
    media_buffer.SetCurrentLength(buffer_size as u32)?;
    media_buffer.Unlock()?;

    sample.AddBuffer(&media_buffer)?;
    sample.SetSampleTime(time_hns)?;
    sample.SetSampleDuration(num_frames as i64 * packet_duration_hns)?;

    Ok(sample)
}
