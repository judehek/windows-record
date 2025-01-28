pub mod audio;
pub mod media;
pub mod video;
pub mod encoder;

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
    input_width: u32,   // Changed parameter name to be explicit
    input_height: u32,  // Changed parameter name to be explicit
    output_width: u32,  // Added output width
    output_height: u32, // Added output height
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

    // Calculate stream indices the same way as create_sink_writer
    let video_stream_index = 0;
    let mut current_stream_index = 1;
    let audio_stream_index = if capture_audio {
        let index = current_stream_index;
        current_stream_index += 1;
        Some(index)
    } else {
        None
    };
    let microphone_stream_index = if capture_microphone {
        Some(current_stream_index)
    } else {
        None
    };

    info!(
        "Stream indices - Video: {}, Audio: {:?}, Microphone: {:?}",
        video_stream_index, audio_stream_index, microphone_stream_index
    );

    // Updated to use input/output dimensions
    let converter = unsafe { 
        video::setup_video_converter(
            &device, 
            input_width, 
            input_height, 
            output_width, 
            output_height
        )
    }?;
    info!("Video processor transform created and configured");

    let mut frame_count = 0;
    let start_time = std::time::Instant::now();

    let mut microphone_disconnected = false;
    let mut audio_disconnected = false;

    while recording.load(Ordering::Relaxed) {
        let mut had_work = false;

        // Process video samples - video is required
        match rec_video.try_recv() {
            Ok(samp) => {
                had_work = true;
                let start = std::time::Instant::now();

                let converted = unsafe {
                    video::convert_bgra_to_nv12(
                        &device, 
                        &converter, 
                        &*samp.0, 
                        output_width,  // Changed to output dimensions
                        output_height
                    )?
                };
                debug!(
                    "Video frame {} converted in {:?}",
                    frame_count,
                    start.elapsed()
                );

                let write_start = std::time::Instant::now();
                unsafe { writer.0.WriteSample(video_stream_index, &converted)? };
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
        if let Some(stream_index) = audio_stream_index {
            if !audio_disconnected {
                match rec_audio.try_recv() {
                    Ok(audio_samp) => {
                        had_work = true;
                        let write_start = std::time::Instant::now();
                        unsafe { writer.0.WriteSample(stream_index, &*audio_samp.0)? };
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
        }

        // Process microphone samples if enabled and not disconnected
        if let Some(stream_index) = microphone_stream_index {
            if !microphone_disconnected {
                match rec_microphone.try_recv() {
                    Ok(mic_samp) => {
                        had_work = true;
                        // Debug: Peek at the sample values
                        unsafe {
                            if let Ok(media_buffer) = mic_samp.0.GetBufferByIndex(0) {
                                let mut buffer = std::ptr::null_mut();
                                let mut max_length = 0u32;
                                let mut current_length = 0u32;

                                if let Ok(()) = media_buffer.Lock(
                                    &mut buffer,
                                    Some(&mut max_length),
                                    Some(&mut current_length),
                                ) {

                                    let _ = media_buffer.Unlock();
                                }
                            }
                        }
                        let write_start = std::time::Instant::now();
                        unsafe { writer.0.WriteSample(stream_index, &*mic_samp.0)? };
                        debug!("Microphone sample written in {:?}", write_start.elapsed());
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        error!("Microphone channel disconnected");
                        microphone_disconnected = true;
                    }
                }
            }
        }

        if !had_work {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    info!(
        "Sample processing finished. Processed {} frames in {:?}",
        frame_count,
        start_time.elapsed()
    );
    unsafe { writer.0.Finalize()? };
    Ok(())
}
