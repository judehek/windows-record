pub mod audio;
pub mod media;
pub mod video;

use log::{debug, error, info};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use windows::core::Result;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

use crate::types::{SendableSample, SendableWriter};

pub fn process_samples(
    writer: SendableWriter,
    rec_video: Receiver<SendableSample>,
    rec_audio: Receiver<SendableSample>,
    rec_microphone: Receiver<SendableSample>,
    recording: Arc<AtomicBool>,
    width: u32,
    height: u32,
    device: Arc<ID3D11Device>,
    capture_audio: bool,
    capture_microphone: bool,
) -> Result<()> {
    info!("Starting sample processing");

    unsafe {
        windows::Win32::System::Threading::SetThreadPriority(
            windows::Win32::System::Threading::GetCurrentThread(),
            windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL,
        );
    }

    let converter = unsafe { video::setup_video_converter(&device, width, height) }?;
    info!("Video processor transform created and configured");

    let mut frame_count = 0;
    let start_time = std::time::Instant::now();

    let mut microphone_disconnected = false;
    let mut audio_disconnected = false;

    while recording.load(Ordering::Relaxed) {
        // Process video samples - video is required, so we break on error
        match rec_video.try_recv() {
            Ok(samp) => {
                let start = std::time::Instant::now();

                let converted = unsafe {
                    video::convert_bgra_to_nv12(&device, &converter, &*samp.0, width, height)?
                };
                debug!(
                    "Video frame {} converted in {:?}",
                    frame_count,
                    start.elapsed()
                );

                let write_start = std::time::Instant::now();
                unsafe { writer.0.WriteSample(0, &converted)? };
                debug!(
                    "Video frame {} written in {:?}",
                    frame_count,
                    write_start.elapsed()
                );

                frame_count += 1;
                if frame_count % 100 == 0 {
                    info!(
                        "Processed {} frames in {:?}",
                        frame_count,
                        start_time.elapsed()
                    );
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(e) => {
                error!("Error receiving video sample: {:?}", e);
                break;
            }
        }

        // Process audio samples if enabled and not disconnected
        if capture_audio && !audio_disconnected {
            match rec_audio.try_recv() {
                Ok(audio_samp) => {
                    let write_start = std::time::Instant::now();
                    unsafe { writer.0.WriteSample(1, &*audio_samp.0)? };
                    debug!(
                        "Process audio sample written in {:?}",
                        write_start.elapsed()
                    );
                }
                Err(TryRecvError::Empty) => {}
                Err(e) => {
                    error!("Audio channel disconnected: {:?}", e);
                    audio_disconnected = true;
                }
            }
        }

        // Process microphone samples if enabled and not disconnected
        if capture_microphone && !microphone_disconnected {
            match rec_microphone.try_recv() {
                Ok(mic_samp) => {
                    let write_start = std::time::Instant::now();
                    unsafe { writer.0.WriteSample(2, &*mic_samp.0)? };
                    debug!("Microphone sample written in {:?}", write_start.elapsed());
                }
                Err(TryRecvError::Empty) => {}
                Err(e) => {
                    error!("Microphone channel disconnected: {:?}", e);
                    microphone_disconnected = true;
                }
            }
        }

        // Small sleep to prevent tight loop
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    info!(
        "Sample processing finished. Processed {} frames in {:?}",
        frame_count,
        start_time.elapsed()
    );
    unsafe { writer.0.Finalize()? };
    Ok(())
}
