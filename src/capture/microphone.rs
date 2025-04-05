use log::{debug, error, info, trace};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Barrier;
use windows::core::implement;
use windows::core::Result;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaBuffer, IMFSample, MFCreateMemoryBuffer, MFCreateSample,
};
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
    shared_start_qpc: Option<u64>,
    device_id: Option<&str>,
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

    // First get the microphone client to obtain its format
    let microphone_client = match setup_microphone_client_for_device(device_id) {
        Ok(client) => client,
        Err(e) => {
            info!("Failed to setup audio client: {:?}", e);
            
            // Wait on the barrier even when we can't get a microphone
            // This prevents the other threads from waiting forever
            info!("Waiting on barrier before returning from microphone thread with error");
            started.wait();
            info!("Barrier passed, microphone thread will now exit with error");
            
            return Err(e);
        }
    };
    // Get the device's mix format
    /*let mix_format_ptr = microphone_client.GetMixFormat()?;
    let wave_format = *mix_format_ptr;
    let format_tag = wave_format.wFormatTag;
    let channels = wave_format.nChannels;
    let samples_per_sec = wave_format.nSamplesPerSec;
    let bits_per_sample = wave_format.wBitsPerSample;
    let block_align = wave_format.nBlockAlign;
    let avg_bytes_per_sec = wave_format.nAvgBytesPerSec;

    info!("Mix Format:");
    info!("  Format Tag: {}", format_tag);
    info!("  Channels: {}", channels);
    info!("  Samples Per Second: {} Hz", samples_per_sec);
    info!("  Bits Per Sample: {}", bits_per_sample);
    info!("  Block Align: {}", block_align);
    info!("  Average Bytes Per Second: {}", avg_bytes_per_sec);*/

    // Use hard-coded format instead of getting from device
    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM.try_into().unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };

    let capture_client: IAudioCaptureClient = match microphone_client.GetService() {
        Ok(client) => client,
        Err(e) => {
            info!("Failed to get capture client: {:?}", e);
            return Err(e);
        }
    };

    // Calculate packet duration based on hard-coded sample rate
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
    let start_qpc = match shared_start_qpc {
        Some(qpc) => {
            info!("Using shared QPC start time: {}", qpc);
            qpc
        }
        None => {
            let mut start_qpc_i64: i64 = 0;
            if !QueryPerformanceCounter(&mut start_qpc_i64).as_bool() {
                info!("Failed to get initial QPC value");
                return Err(E_FAIL.into());
            }
            if start_qpc_i64 <= 0 {
                info!("Invalid initial QPC value: {}", start_qpc_i64);
                return Err(E_FAIL.into());
            }
            let qpc = start_qpc_i64 as u64;
            info!("Generated new QPC start time: {}", qpc);
            qpc
        }
    };
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
                                if let Err(e) = send.send(SendableSample::new(sample)) {
                                    info!("Failed to send audio sample, receiver likely dropped: {:?}", e);
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

unsafe fn setup_microphone_client_for_device(device_id: Option<&str>) -> Result<IAudioClient> {
    // Initialize COM if not already initialized
    let coinit_result = CoInitializeEx(None, COINIT_MULTITHREADED);
    match coinit_result {
        Ok(_) => info!("COM initialized successfully"),
        Err(e) => {
            if e.code() != CO_E_ALREADYINITIALIZED {
                return Err(e);
            }
            info!("COM already initialized: {:?}", e);
        }
    }

    let enumerator: IMMDeviceEnumerator =
        match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            Ok(enum_) => enum_,
            Err(e) => {
                info!("Failed to create device enumerator: {:?}", e);
                return Err(e);
            }
        };

    // Get device either by ID or default
    let device = if let Some(id) = device_id {
        info!("Using specified audio input device ID: {}", id);
        match enumerator.GetDevice(windows::core::PCWSTR::from_raw(
            windows::core::HSTRING::from(id).as_ptr(),
        )) {
            Ok(dev) => dev,
            Err(e) => {
                info!(
                    "Failed to get device with ID {}: {:?}. Falling back to default.",
                    id, e
                );
                match enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) {
                    Ok(dev) => dev,
                    Err(e) => {
                        // Check if error is "Element not found" (0x80070490) which indicates no microphone
                        // Convert HSTRING to String for comparison
                        let error_message = e.code().message().to_string_lossy();
                        if error_message.contains("Element not found") {
                            info!("No microphone found on system. Error code: {:X} (Element not found)", e.code().0);
                        } else {
                            error!("Failed to get default audio endpoint: {:?}", e);
                        }
                        return Err(e);
                    }
                }
            }
        }
    } else {
        info!("Using default audio input device");
        match enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) {
            Ok(dev) => dev,
            Err(e) => {
                // Check if error is "Element not found" (0x80070490) which indicates no microphone
                // Convert HSTRING to String for comparison
                let error_message = e.code().message().to_string_lossy();
                if error_message.contains("Element not found") {
                    info!("No microphone found on system. Error code: {:X} (Element not found)", e.code().0);
                } else {
                    error!("Failed to get default audio endpoint: {:?}", e);
                }
                return Err(e);
            }
        }
    };

    // Log selected device info
    if let Ok(id_ptr) = device.GetId() {
        let id = id_ptr.to_string().unwrap_or_default();
        info!("Selected audio input device ID: {}", id);
    }

    if let Ok(props) = device.OpenPropertyStore(STGM_READ) {
        if let Ok(prop_value) = props.GetValue(&PKEY_Device_FriendlyName) {
            let name = prop_value
                .Anonymous
                .Anonymous
                .Anonymous
                .pwszVal
                .to_string()
                .unwrap_or_default();
            info!("Selected audio input device name: {}", name);
        }
    }

    let callback = AudioEndpointVolumeCallback;
    let callback_interface: IMMNotificationClient = callback.into();
    if let Err(e) = enumerator.RegisterEndpointNotificationCallback(&callback_interface) {
        info!("Failed to register callback: {:?}", e);
        return Err(e);
    }

    let audio_client: IAudioClient = match device.Activate(CLSCTX_ALL, None) {
        Ok(client) => client,
        Err(e) => {
            info!("Failed to activate audio client: {:?}", e);
            return Err(e);
        }
    };

    let mut default_period = 0;
    let mut minimum_period = 0;
    if let Err(e) =
        audio_client.GetDevicePeriod(Some(&mut default_period), Some(&mut minimum_period))
    {
        info!("Failed to get device period: {:?}", e);
        return Err(e);
    }

    // Create a hard-coded format that matches the system audio format
    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM.try_into().unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400, // nSamplesPerSec * nBlockAlign (44100 * 4)
        nBlockAlign: 4,          // nChannels * wBitsPerSample/8 (2 * 16/8)
        wBitsPerSample: 16,
        cbSize: 0,
    };

    let init_result = audio_client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        300000,
        0,
        &wave_format, // Use our hard-coded format instead of mix_format_ptr
        None,
    );

    match init_result {
        Ok(_) => {
            let event = CreateEventW(None, false, false, None)?;
            audio_client.SetEventHandle(event)?;
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
    if buffer.is_null() {
        return Err(E_POINTER.into());
    }

    let bytes_per_sample = wave_format.wBitsPerSample as usize / 8;
    let num_channels = wave_format.nChannels as usize;
    let block_align = wave_format.nBlockAlign as usize;

    let buffer_size = num_frames as usize * block_align;

    let sample: IMFSample = MFCreateSample()?.into();
    let media_buffer: IMFMediaBuffer = MFCreateMemoryBuffer(buffer_size as u32)?.into();

    let mut buffer_data = std::ptr::null_mut();
    let mut max_length = 0u32;
    let mut current_length = 0u32;

    media_buffer.Lock(
        &mut buffer_data,
        Some(&mut max_length as *mut u32),
        Some(&mut current_length as *mut u32),
    )?;
    if wave_format.wBitsPerSample == 32 {
        let src =
            std::slice::from_raw_parts(buffer as *const f32, num_frames as usize * num_channels);
        let dst = std::slice::from_raw_parts_mut(
            buffer_data as *mut f32,
            num_frames as usize * num_channels,
        );

        // Copy the samples
        std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), src.len());
    } else if wave_format.wBitsPerSample == 16 {
        let src =
            std::slice::from_raw_parts(buffer as *const i16, num_frames as usize * num_channels);
        let dst = std::slice::from_raw_parts_mut(
            buffer_data as *mut i16,
            num_frames as usize * num_channels,
        );

        // For stereo, interleave both channels properly
        if num_channels == 2 {
            for i in 0..num_frames as usize {
                for c in 0..num_channels {
                    dst[i * num_channels + c] = src[i * num_channels + c];
                }
            }

            // Debug first few frames
            if num_frames > 0 {
                trace!(
                    "First few frames (L/R pairs): {:?}",
                    &src[..std::cmp::min(8 * num_channels, src.len())]
                );
            }
        } else {
            // Single channel, straight copy
            std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), src.len());
        }
    } else {
        // For non-16-bit formats, fall back to byte copy
        std::ptr::copy_nonoverlapping(buffer, buffer_data, buffer_size);
    }

    media_buffer.SetCurrentLength(buffer_size as u32)?;
    media_buffer.Unlock()?;

    sample.AddBuffer(&media_buffer)?;
    sample.SetSampleTime(time_hns)?;

    sample.SetSampleDuration(packet_duration_hns)?;

    Ok(sample)
}
