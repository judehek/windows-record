use log::{debug, info};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Barrier;
use windows::core::{implement, IUnknown};
use windows::core::{ComInterface, Result};
use windows::Win32::Foundation::{GetLastError, E_FAIL};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::*;

use crate::types::SendableSample;

pub unsafe fn collect_microphone(
    send: Sender<SendableSample>,
    recording: Arc<AtomicBool>,
    started: Arc<Barrier>,
) -> Result<()> {
    // Set thread priority
    let priority_result = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    if priority_result.as_bool() == false {
        info!("Failed to set thread priority: {}", GetLastError().0);
    }

    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM.try_into().unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };

    // Get default input device
    let device_enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

    let input_device = device_enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?;

    // Activate the audio client
    let audio_client: IAudioClient = input_device.Activate(CLSCTX_ALL, None)?;

    // Initialize audio client
    audio_client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        0,
        0,
        &wave_format,
        None,
    )?;

    let capture_client: IAudioCaptureClient = audio_client.GetService()?;

    let packet_duration =
        std::time::Duration::from_nanos((1000000000.0 / wave_format.nSamplesPerSec as f64) as u64);
    let packet_duration_hns = packet_duration.as_nanos() as i64 / 100;

    match audio_client.Start() {
        Ok(_) => info!("Microphone audio client started successfully"),
        Err(e) => {
            info!("Failed to start microphone audio client: {:?}", e);
            return Err(e);
        }
    }

    started.wait();

    while recording.load(Ordering::Relaxed) {
        let next_packet_size = match capture_client.GetNextPacketSize() {
            Ok(size) => size,
            Err(e) => {
                info!("Failed to get next microphone packet size: {:?}", e);
                return Err(e);
            }
        };

        if next_packet_size > 0 {
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
                    if (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) == 0 {
                        // Create and send audio sample
                        match crate::capture::audio::create_audio_sample(
                            buffer,
                            num_frames_available,
                            &wave_format,
                            device_position as i64,
                            packet_duration_hns,
                        ) {
                            Ok(sample) => {
                                if let Err(e) = send.send(SendableSample(Arc::new(sample))) {
                                    info!("Failed to send microphone sample: {:?}", e);
                                    return Err(E_FAIL.into());
                                }
                            }
                            Err(e) => {
                                info!("Failed to create microphone sample: {:?}", e);
                                return Err(e);
                            }
                        }
                    }

                    match capture_client.ReleaseBuffer(num_frames_available) {
                        Ok(_) => {}
                        Err(e) => {
                            info!("Failed to release microphone buffer: {:?}", e);
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    info!("Failed to get microphone buffer: {:?}", e);
                    return Err(e);
                }
            }
        } else {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    info!("Microphone recording stopped");
    match audio_client.Stop() {
        Ok(_) => info!("Microphone audio client stopped successfully"),
        Err(e) => info!("Error stopping microphone audio client: {:?}", e),
    }

    Ok(())
}
