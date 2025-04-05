pub mod audio;
pub mod media;
pub mod video;

use audio::AudioMixer;
use log::{debug, error, info, trace, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use windows::core::Result;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

use crate::types::{ReplayBuffer, SendableSample, SendableWriter, TexturePool};

pub fn process_samples(
    writer: SendableWriter,
    rec_video: Receiver<SendableSample>,
    rec_audio: Receiver<SendableSample>,
    rec_microphone: Receiver<SendableSample>,
    rec_window_info: Receiver<(Option<(i32, i32)>, Option<(u32, u32)>)>,
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
    replay_buffer: Option<Arc<ReplayBuffer>>,
    initial_window_position: Option<(i32, i32)>,
    initial_window_size: Option<(u32, u32)>,
    texture_pool: Arc<TexturePool>,
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

    // Create a mutex to store current window position and size with the initial values
    info!("Initializing window position mutex with: {:?}", initial_window_position);
    let window_position = Arc::new(Mutex::new(initial_window_position));
    info!("Initializing window size mutex with: {:?}", initial_window_size);
    let window_size = Arc::new(Mutex::new(initial_window_size));

    // Flag to track window changes
    let window_changed = Arc::new(AtomicBool::new(false));
    let window_changed_clone = window_changed.clone();

    // Create a thread to handle window info updates
    let window_position_clone = window_position.clone();
    let window_size_clone = window_size.clone();
    let recording_clone = recording.clone();

    std::thread::spawn(move || {
        info!("Window info monitoring thread started");
        while recording_clone.load(Ordering::Relaxed) {
            match rec_window_info.try_recv() {
                Ok((pos, size)) => {
                    let mut position_changed = false;
                    let mut size_changed = false;

                    // Update window position if received
                    if let Some(pos_value) = pos {
                        let mut lock = window_position_clone.lock().unwrap();
                        position_changed = *lock != Some(pos_value);
                        if position_changed {
                            info!(
                                "Processing: Window position changed to: [{}, {}]",
                                pos_value.0, pos_value.1
                            );
                            *lock = Some(pos_value);
                        }
                    }

                    // Update window size if received
                    if let Some(size_value) = size {
                        let mut lock = window_size_clone.lock().unwrap();
                        size_changed = *lock != Some(size_value);
                        if size_changed {
                            info!(
                                "Processing: Window size changed to: {}x{}",
                                size_value.0, size_value.1
                            );
                            *lock = Some(size_value);
                        }
                    }

                    // If either position or size changed, mark for converter update
                    if position_changed || size_changed {
                        info!("Processing: Window position or size changed, marking for converter update");
                        window_changed_clone.store(true, Ordering::SeqCst);
                    }
                }
                Err(TryRecvError::Empty) => {
                    // No new window info yet, wait a bit
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(TryRecvError::Disconnected) => {
                    debug!("Window info channel disconnected");
                    break;
                }
            }
        }
        info!("Window info monitoring thread finished");
    });

    let converter = unsafe {
        video::setup_video_converter(
            input_width,
            input_height,
            output_width,
            output_height,
            window_position.clone(),
            window_size.clone(),
        )
    }?;
    info!("Video processor transform created and configured");

    let mut frame_count = 0;
    let start_time = std::time::Instant::now();

    let mut microphone_disconnected = false;
    let mut audio_disconnected = false;

    // Timestamp to track when we last checked for window changes
    let mut last_window_check = std::time::Instant::now();
    let window_check_interval = std::time::Duration::from_millis(500); // Check every 500ms

    while recording.load(Ordering::Relaxed) {
        let mut had_work = false;

        // Check if window has changed and update converter if needed
        let now = std::time::Instant::now();
        if now.duration_since(last_window_check) >= window_check_interval {
            last_window_check = now;

            if window_changed.swap(false, Ordering::SeqCst) {
                info!("Main thread: Detected window change, updating video converter");
                let current_pos = *window_position.lock().unwrap();
                let current_size = *window_size.lock().unwrap();

                unsafe {
                    if let Err(e) = video::update_video_converter(
                        &converter,
                        input_width,
                        input_height,
                        current_pos,
                        current_size,
                    ) {
                        warn!("Failed to update video converter: {:?}", e);
                    }
                }
            }
        }

        // Process video samples - video is required
        match rec_video.try_recv() {
            Ok(samp) => {
                had_work = true;
                // Extract timestamp for the replay buffer
                let timestamp: i64 = unsafe { samp.sample.GetSampleTime() }?;

                // Convert and write to file as usual
                let converted = unsafe {
                    video::convert_bgra_to_nv12(
                        &device,
                        &converter,
                        &*samp.sample,
                        output_width,
                        output_height,
                        &texture_pool,
                    )?
                };
                // Add to replay buffer if enabled
                if let Some(buffer) = &replay_buffer {
                    // Clone the IMFSample directly
                    buffer.add_video_sample(SendableSample::new(converted.clone()), timestamp)?;
                }
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

                    // Extract timestamp for the replay buffer
                    let timestamp: i64 = unsafe { audio_samp.sample.GetSampleTime() }?;

                    // Only add to replay buffer if we're NOT mixing (otherwise we'll add the mixed sample later)
                    if audio_mixer.is_none() {
                        if let Some(buffer) = &replay_buffer {
                            // Clone the IMFSample from the Arc
                            let cloned_sample = audio_samp.sample.as_ref().clone();
                            buffer
                                .add_audio_sample(SendableSample::new(cloned_sample), timestamp)?;
                        }
                    }

                    if let Some(mixer) = &mut audio_mixer {
                        // Add to mixer if we need to mix
                        mixer.add_system_audio(audio_samp);
                    } else if let Some(stream_index) = audio_stream_index {
                        // Write directly if no mixing needed
                        let write_start = std::time::Instant::now();
                        unsafe { writer.0.WriteSample(stream_index, &*audio_samp.sample)? };
                        debug!(
                            "Process audio sample written in {:?}",
                            write_start.elapsed()
                        );
                    }
                }
                // Error handling remains the same
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

                    // Extract timestamp for the replay buffer
                    let timestamp: i64 = unsafe { mic_samp.sample.GetSampleTime() }?;

                    // Only add to replay buffer if we're NOT mixing
                    if audio_mixer.is_none() {
                        if let Some(buffer) = &replay_buffer {
                            // Clone the IMFSample from the Arc
                            let cloned_sample = mic_samp.sample.as_ref().clone();
                            buffer
                                .add_audio_sample(SendableSample::new(cloned_sample), timestamp)?;
                        }
                    }

                    // Rest remains the same
                    if let Some(mixer) = &mut audio_mixer {
                        mixer.add_microphone_audio(mic_samp);
                    } else if let Some(stream_index) = audio_stream_index {
                        let write_start = std::time::Instant::now();
                        unsafe { writer.0.WriteSample(stream_index, &*mic_samp.sample)? };
                        debug!("Microphone sample written in {:?}", write_start.elapsed());
                    }
                }
                // Error handling remains the same
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    // This could be because there's no microphone device
                    info!("Microphone channel disconnected - no microphone data will be included");
                    microphone_disconnected = true;
                    
                    // If we're supposed to be mixing, update the AudioMixer to not wait for mic data
                    if let Some(mixer) = &mut audio_mixer {
                        if capture_audio && capture_microphone {
                            info!("Updating AudioMixer to no longer wait for microphone samples");
                            mixer.set_both_sources_active(false);
                        }
                    }
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

                            // Add mixed sample to replay buffer with current timestamp
                            if let Some(buffer) = &replay_buffer {
                                // Get timestamp from the sample if possible or use system time
                                let timestamp = unsafe {
                                    mixed_sample.GetSampleTime().unwrap_or_else(|_| {
                                        // Fallback to system time if GetSampleTime fails
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_nanos()
                                            as i64
                                    })
                                };
                                // Extract the IMFSample from the Arc
                                let cloned_sample = mixed_sample.as_ref().clone();
                                buffer.add_audio_sample(
                                    SendableSample::new(cloned_sample),
                                    timestamp,
                                )?;
                            }

                            // Write the mixed sample
                            unsafe { writer.0.WriteSample(stream_index, &*mixed_sample)? };
                            trace!("Mixed audio sample written in {:?}", write_start.elapsed());
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
