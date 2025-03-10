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
use StructuredStorage::PROPVARIANT;

use crate::types::SendableSample;

#[derive(Clone)]
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct WASAPIActivateAudioInterfaceCompletionHandler {
    inner: Arc<(std::sync::Mutex<InnerHandler>, std::sync::Condvar)>,
}

struct InnerHandler {
    punk_audio_interface: Option<IUnknown>,
    done: bool,
}

impl WASAPIActivateAudioInterfaceCompletionHandler {
    unsafe fn new() -> Self {
        Self {
            inner: Arc::new((
                std::sync::Mutex::new(InnerHandler {
                    punk_audio_interface: None,
                    done: false,
                }),
                std::sync::Condvar::new(),
            )),
        }
    }

    pub unsafe fn get_activate_result(&self) -> Result<IAudioClient> {
        let mut inner = self.inner.0.lock().unwrap();
        while !inner.done {
            inner = self.inner.1.wait(inner).unwrap();
        }
        inner.punk_audio_interface.as_ref().unwrap().cast()
    }
}

impl IActivateAudioInterfaceCompletionHandler_Impl
    for WASAPIActivateAudioInterfaceCompletionHandler
{
    fn ActivateCompleted(
        &self,
        activate_operation: Option<&IActivateAudioInterfaceAsyncOperation>,
    ) -> Result<()> {
        unsafe {
            let mut activate_result = E_UNEXPECTED;
            let mut inner = self.inner.0.lock().unwrap();
            activate_operation
                .unwrap()
                .GetActivateResult(&mut activate_result, &mut inner.punk_audio_interface)?;
            inner.done = true;
            self.inner.1.notify_all();
        }
        Ok(())
    }
}

pub unsafe fn collect_audio(
    send: Sender<SendableSample>,
    recording: Arc<AtomicBool>,
    proc_id: u32,
    started: Arc<Barrier>,
    shared_start_qpc: Option<u64>,
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

    let audio_client = match setup_audio_client(proc_id, &wave_format) {
        Ok(client) => client,
        Err(e) => {
            info!("Failed to setup audio client: {:?}", e);
            return Err(e);
        }
    };

    let capture_client: IAudioCaptureClient = match audio_client.GetService() {
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
    let start_qpc = match shared_start_qpc {
        Some(qpc) => {
            info!("Using shared QPC start time: {}", qpc);
            qpc
        },
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

    match audio_client.Start() {
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
            zero_packet_count = 0; // Reset counter when we get data

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
                    // Validate QPC timing
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

                        match create_audio_sample(
                            buffer,
                            num_frames_available,
                            &wave_format,
                            time_hns,
                            packet_duration_hns,
                        ) {
                            Ok(sample) => {
                                if let Err(e) = send.send(SendableSample(Arc::new(sample))) {
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
                // Log every ~1 second of no data
                info!(
                    "No audio data received for {} consecutive checks",
                    zero_packet_count
                );
                zero_packet_count = 0; // Reset to avoid spam
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    info!(
        "Recording stopped. Total packets processed: {}",
        total_packets
    );
    match audio_client.Stop() {
        Ok(_) => info!("Audio client stopped successfully"),
        Err(e) => info!("Error stopping audio client: {:?}", e),
    }

    Ok(())
}

unsafe fn setup_audio_client(proc_id: u32, wave_format: &WAVEFORMATEX) -> Result<IAudioClient> {
    let activation_params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: proc_id,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };

    let mut prop_variant = PROPVARIANT::default();
    (*prop_variant.Anonymous.Anonymous).vt = VT_BLOB;
    (*prop_variant.Anonymous.Anonymous).Anonymous.blob.cbSize =
        std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
    (*prop_variant.Anonymous.Anonymous).Anonymous.blob.pBlobData =
        &activation_params as *const _ as *mut _;

    let handler = WASAPIActivateAudioInterfaceCompletionHandler::new();
    let handler_interface: IActivateAudioInterfaceCompletionHandler = handler.clone().into();

    ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&mut prop_variant),
        &handler_interface,
    )?;

    let audio_client = handler.get_activate_result()?;

    audio_client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        300000,
        0,
        wave_format,
        None,
    )?;

    Ok(audio_client)
}

// Updated create_audio_sample to match microphone code's buffer handling
unsafe fn create_audio_sample(
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

    // Format-specific processing based on bit depth
    if wave_format.wBitsPerSample == 32 {
        // 32-bit float handling
        let src = std::slice::from_raw_parts(
            buffer as *const f32,
            num_frames as usize * num_channels,
        );
        let dst = std::slice::from_raw_parts_mut(
            buffer_data as *mut f32,
            num_frames as usize * num_channels,
        );

        // Copy the samples
        std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), src.len());
    } else if wave_format.wBitsPerSample == 16 {
        // 16-bit integer handling
        let src = std::slice::from_raw_parts(
            buffer as *const i16,
            num_frames as usize * num_channels,
        );
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
                debug!(
                    "First few audio frames (L/R pairs): {:?}",
                    &src[..std::cmp::min(8 * num_channels, src.len())]
                );
            }
        } else {
            // Single channel, straight copy
            std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr(), src.len());
        }
    } else {
        // For non-16-bit and non-32-bit formats, fall back to byte copy
        std::ptr::copy_nonoverlapping(buffer, buffer_data, buffer_size);
    }

    media_buffer.SetCurrentLength(buffer_size as u32)?;
    media_buffer.Unlock()?;

    sample.AddBuffer(&media_buffer)?;
    sample.SetSampleTime(time_hns)?;
    sample.SetSampleDuration(packet_duration_hns)?;

    Ok(sample)
}