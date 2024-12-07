use log::{debug, error, info};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Barrier;
use windows::core::{implement, IUnknown, PWSTR};
use windows::core::{ComInterface, Result};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{GetLastError, E_FAIL};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::*;

use crate::capture::audio::create_audio_sample;
use crate::types::SendableSample;

pub unsafe fn collect_microphone(
    send: Sender<SendableSample>,
    recording: Arc<AtomicBool>,
    started: Arc<Barrier>,
) -> Result<()> {
    info!("Starting microphone capture initialization");

    // 1. Set and verify thread priority
    let priority_result = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    if !priority_result.as_bool() {
        info!("Failed to set thread priority: {}", GetLastError().0);
    }

    // 2. Create device enumerator with detailed error handling
    info!("Creating device enumerator");
    let device_enumerator: IMMDeviceEnumerator =
        match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            Ok(enumerator) => {
                info!("Successfully created device enumerator");
                enumerator
            }
            Err(e) => {
                error!("Failed to create device enumerator: {:?}", e);
                return Err(e);
            }
        };

    // 3. Get default input device with error checking
    info!("Getting default audio endpoint");
    let input_device = match device_enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) {
        Ok(device) => {
            info!("Successfully got default audio endpoint");
            device
        }
        Err(e) => {
            error!("Failed to get default audio endpoint: {:?}", e);
            return Err(e);
        }
    };

    info!("Getting device properties");
    let property_store = match input_device.OpenPropertyStore(STGM_READ) {
        Ok(store) => {
            info!("Successfully opened property store");
            store
        }
        Err(e) => {
            error!("Failed to open property store: {:?}", e);
            return Err(e);
        }
    };

    // Get friendly name property
    let name_property = match property_store.GetValue(&PKEY_Device_FriendlyName) {
        Ok(prop) => {
            info!("Successfully got device name property");
            prop
        }
        Err(e) => {
            error!("Failed to get device name property: {:?}", e);
            return Err(e);
        }
    };

    // Convert friendly name to string and log
    unsafe {
        if name_property.Anonymous.Anonymous.vt == VT_LPWSTR {
            let wide_str: PWSTR = name_property.Anonymous.Anonymous.Anonymous.pwszVal;
            match wide_str.to_string() {
                Ok(device_name) => info!("Using microphone: {}", device_name),
                Err(_) => error!("Failed to convert device name to string"),
            }
        } else {
            error!("Unexpected property variant type for device name");
        }
    }

    // 5. Create wave format
    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM.try_into().unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };

    // 6. Activate audio client with error handling
    info!("Activating audio client");
    let audio_client: IAudioClient = match input_device.Activate(CLSCTX_ALL, None) {
        Ok(client) => {
            info!("Successfully activated audio client");
            client
        }
        Err(e) => {
            error!("Failed to activate audio client: {:?}", e);
            return Err(e);
        }
    };

    // 7. Initialize audio client with detailed error handling
    info!("Initializing audio client");
    match audio_client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        0,
        0,
        &wave_format,
        None,
    ) {
        Ok(_) => info!("Successfully initialized audio client"),
        Err(e) => {
            error!("Failed to initialize audio client: {:?}", e);
            return Err(e);
        }
    }

    // 8. Get capture client with error handling
    info!("Getting capture client");
    let capture_client: IAudioCaptureClient = match audio_client.GetService() {
        Ok(client) => {
            info!("Successfully got capture client");
            client
        }
        Err(e) => {
            error!("Failed to get capture client: {:?}", e);
            return Err(e);
        }
    };

    // Calculate timing parameters
    let packet_duration =
        std::time::Duration::from_nanos((1000000000.0 / wave_format.nSamplesPerSec as f64) as u64);
    let packet_duration_hns = packet_duration.as_nanos() as i64 / 100;

    // 9. Start the audio client with error handling
    info!("Starting audio client");
    match audio_client.Start() {
        Ok(_) => info!("Successfully started audio client"),
        Err(e) => {
            error!("Failed to start audio client: {:?}", e);
            return Err(e);
        }
    }

    // Wait for other threads
    info!("Waiting at barrier");
    started.wait();
    info!("Passed barrier, starting capture loop");

    // Statistics for debugging
    let mut total_packets = 0;
    let mut empty_packets = 0;
    let start_time = std::time::Instant::now();
    let mut last_stats_time = start_time;

    // Main capture loop
    while recording.load(Ordering::Relaxed) {
        let next_packet_size = match capture_client.GetNextPacketSize() {
            Ok(size) => size,
            Err(e) => {
                error!("Failed to get next packet size: {:?}", e);
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
                    total_packets += 1;

                    if (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) == 0
                        && num_frames_available > 0
                    {
                        match create_audio_sample(
                            buffer,
                            num_frames_available,
                            &wave_format,
                            device_position as i64,
                            packet_duration_hns,
                        ) {
                            Ok(sample) => {
                                if let Err(e) = send.send(SendableSample(Arc::new(sample))) {
                                    error!("Failed to send microphone sample: {:?}", e);
                                    return Err(E_FAIL.into());
                                }
                            }
                            Err(e) => {
                                error!("Failed to create microphone sample: {:?}", e);
                                return Err(e);
                            }
                        }
                    }

                    match capture_client.ReleaseBuffer(num_frames_available) {
                        Ok(_) => {}
                        Err(e) => {
                            error!("Failed to release buffer: {:?}", e);
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to get buffer: {:?}", e);
                    return Err(e);
                }
            }
        } else {
            empty_packets += 1;
        }

        // Log statistics every 5 seconds
        let now = std::time::Instant::now();
        if now.duration_since(last_stats_time).as_secs() >= 5 {
            info!(
                "Microphone capture stats - Total packets: {}, Empty packets: {}, Time elapsed: {:?}",
                total_packets,
                empty_packets,
                now.duration_since(start_time)
            );
            last_stats_time = now;
        }

        // Small sleep to prevent tight loop
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    info!("Stopping microphone capture");
    info!(
        "Final stats - Total packets: {}, Empty packets: {}, Time elapsed: {:?}",
        total_packets,
        empty_packets,
        std::time::Instant::now().duration_since(start_time)
    );

    match audio_client.Stop() {
        Ok(_) => info!("Successfully stopped audio client"),
        Err(e) => error!("Error stopping audio client: {:?}", e),
    }

    Ok(())
}
