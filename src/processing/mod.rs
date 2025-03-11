pub mod audio;
pub mod media;
pub mod video;

use audio::AudioMixer;
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
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    device: Arc<ID3D11Device>,
    capture_audio: bool,
    capture_microphone: bool,
    system_volume: Option<f32>,
    microphone_volume: Option<f32>,
) -> Result<()> {
    info!("Starting sample processing");

    unsafe {
        windows::Win32::System::Threading::SetThreadPriority(
            windows::Win32::System::Threading::GetCurrentThread(),
            windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL,
        );
    }

    // Calculate stream indices based on our new approach
    let video_stream_index = 0;
    
    // Now we only have one audio stream index if either audio source is enabled
    let audio_stream_index = if capture_audio || capture_microphone {
        Some(1)
    } else {
        None
    };

    info!(
        "Stream indices - Video: {}, Audio: {:?}",
        video_stream_index, audio_stream_index
    );

    // Create audio mixer if we need to mix audio
    let mut audio_mixer = if capture_audio || capture_microphone {
        let mut mixer = AudioMixer::new(44100, 16, 2, capture_audio && capture_microphone);
        
        // Set the volume/gain levels from parameters, using default of 1.0 if None
        let sys_vol = system_volume.unwrap_or(1.0);
        let mic_vol = microphone_volume.unwrap_or(1.0);
        
        mixer.set_system_volume(sys_vol);
        mixer.set_microphone_volume(mic_vol);
        
        info!(
            "Audio mixer created with system gain: {:.2}, microphone gain: {:.2}",
            sys_vol, mic_vol
        );
        
        Some(mixer)
    } else {
        None
    };

    // Updated to use input/output dimensions
    let converter = unsafe { 
        video::setup_video_converter(
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
                let converted = unsafe {
                    video::convert_bgra_to_nv12(
                        &device, 
                        &converter, 
                        &*samp.0, 
                        output_width,
                        output_height
                    )?
                };
                unsafe { writer.0.WriteSample(video_stream_index, &converted)? };

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

        // Process audio samples from system audio
        if !audio_disconnected && capture_audio {
            match rec_audio.try_recv() {
                Ok(audio_samp) => {
                    had_work = true;
                    
                    if let Some(mixer) = &mut audio_mixer {
                        // Add to mixer if we need to mix
                        mixer.add_system_audio(audio_samp);
                    } else if let Some(stream_index) = audio_stream_index {
                        // Write directly if no mixing needed
                        let write_start = std::time::Instant::now();
                        unsafe { writer.0.WriteSample(stream_index, &*audio_samp.0)? };
                        debug!(
                            "Process audio sample written in {:?}",
                            write_start.elapsed()
                        );
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(e) => {
                    error!("Audio channel disconnected: {:?}", e);
                    audio_disconnected = true;
                }
            }
        }

        // Process microphone samples
        if !microphone_disconnected && capture_microphone {
            match rec_microphone.try_recv() {
                Ok(mic_samp) => {
                    had_work = true;
                    
                    if let Some(mixer) = &mut audio_mixer {
                        // Add to mixer if we need to mix
                        mixer.add_microphone_audio(mic_samp);
                    } else if let Some(stream_index) = audio_stream_index {
                        // Write directly if no mixing needed
                        let write_start = std::time::Instant::now();
                        unsafe { writer.0.WriteSample(stream_index, &*mic_samp.0)? };
                        debug!("Microphone sample written in {:?}", write_start.elapsed());
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    error!("Microphone channel disconnected");
                    microphone_disconnected = true;
                }
            }
        }

        // Process any available mixed samples
        if let Some(mixer) = &mut audio_mixer {
            if let Some(stream_index) = audio_stream_index {
                // Process mixed samples until there are none available
                while let Some(mixed_result) = unsafe { mixer.process_next_sample() } {
                    had_work = true;
                    match mixed_result {
                        Ok(mixed_sample) => {
                            let write_start = std::time::Instant::now();
                            // Write the mixed sample
                            unsafe { writer.0.WriteSample(stream_index, &*mixed_sample)? };
                            debug!("Mixed audio sample written in {:?}", write_start.elapsed());
                        }
                        Err(e) => {
                            error!("Error mixing audio samples: {:?}", e);
                        }
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
