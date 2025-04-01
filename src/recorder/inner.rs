use log::{error, info};
use windows::Win32::System::Performance::QueryPerformanceCounter;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::RwLock; // Replace RefCell with RwLock
use std::thread::JoinHandle;
use windows::core::{ComInterface, Result};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;

use super::config::RecorderConfig;
use crate::capture::{collect_audio, get_frames, collect_microphone, get_window_by_string, get_window_by_exact_string};
use crate::device::{get_audio_input_device_by_name, get_video_encoder_by_type};
use crate::error::RecorderError;
use crate::processing::{media, process_samples};
use crate::types::{SendableSample, SendableWriter, ReplayBuffer};

pub struct RecorderInner {
    recording: Arc<AtomicBool>,
    collect_video_handle: RwLock<Option<JoinHandle<Result<()>>>>,
    process_handle: RwLock<Option<JoinHandle<Result<()>>>>,
    collect_audio_handle: RwLock<Option<JoinHandle<Result<()>>>>,
    collect_microphone_handle: RwLock<Option<JoinHandle<Result<()>>>>,
    replay_buffer: RwLock<Option<Arc<ReplayBuffer>>>,
    config: RecorderConfig,
}

impl RecorderInner {
    pub fn init(config: &RecorderConfig, process_name: &str) -> Result<Self> {
        info!("Starting init() with process: {}", process_name);
        // By default, use substring matching
        Self::init_with_exact_match(config, process_name, false)
    }
    
    pub fn init_with_exact_match(config: &RecorderConfig, process_name: &str, use_exact_match: bool) -> Result<Self> {
        info!("Initializing recorder for process: {} with exact match: {}", 
              process_name, use_exact_match);
        info!("Config details - fps: {}/{}, input: {}x{}, output: {}x{}, audio: {}, mic: {}",
              config.fps_num(), config.fps_den(),
              config.input_width(), config.input_height(),
              config.output_width(), config.output_height(),
              config.capture_audio(), config.capture_microphone());

        // Clone the necessary values from config at the start
        let fps_num = config.fps_num();
        let fps_den = config.fps_den();
        let input_width = config.input_width();
        let input_height = config.input_height();
        let output_width = config.output_width();
        let output_height = config.output_height();
        let capture_audio = config.capture_audio();
        let capture_microphone = config.capture_microphone();
        let video_bitrate = config.video_bitrate();
        let system_volume = config.system_volume();
        let microphone_volume = config.microphone_volume();
        info!("Config values cloned successfully");
        
        info!("Checking for microphone device configuration");
        let microphone_device = if let Some(device_name) = config.microphone_device() {
            info!("Attempting to get device ID for microphone: '{}'", device_name);
            match get_audio_input_device_by_name(Some(device_name)) {
                Ok(device_id) => {
                    info!("Found device ID for '{}': {}", device_name, device_id);
                    Some(device_id)
                },
                Err(e) => {
                    info!("Could not get device ID for '{}', using default: {:?}", device_name, e);
                    None
                }
            }
        } else {
            info!("No microphone device specified, using system default");
            None
        };

        
        // Create replay buffer if enabled
        info!("Checking if replay buffer is enabled");
        let replay_buffer = if config.enable_replay_buffer() {
            info!("Replay buffer is enabled with {} seconds", config.replay_buffer_seconds());
            let buffer_duration = std::time::Duration::from_secs(config.replay_buffer_seconds() as u64);
            let fps = fps_num as f64 / fps_den as f64;
            
            // Estimate the number of frames and audio samples in the buffer
            let video_frames = (fps * buffer_duration.as_secs_f64()) as usize;
            let audio_samples = if capture_audio || capture_microphone {
                // Audio at 44.1kHz, assume ~10 packets per second for buffer capacity
                (10.0 * buffer_duration.as_secs_f64()) as usize
            } else {
                0
            };
            
            info!("Creating replay buffer for {} seconds ({} video frames, {} audio samples)",
                 config.replay_buffer_seconds(), video_frames, audio_samples);
            
            let buffer = Some(Arc::new(ReplayBuffer::new(buffer_duration, video_frames, audio_samples)));
            info!("Replay buffer created successfully");
            buffer
        } else {
            info!("Replay buffer is disabled");
            None
        };

        // Parse out path string from PathBuf
        info!("Getting output path string");
        let output_path = config.output_path()
            .to_str()
            .ok_or_else(|| RecorderError::FailedToStart("Invalid path string".to_string()))?;
        info!("Output path resolved to: {}", output_path);

        let recording = Arc::new(AtomicBool::new(true));
        let mut collect_video_handle: Option<JoinHandle<Result<()>>> = None;
        let mut process_handle: Option<JoinHandle<Result<()>>> = None;
        let mut collect_audio_handle: Option<JoinHandle<Result<()>>> = None;
        let mut collect_microphone_handle: Option<JoinHandle<Result<()>>> = None;

        unsafe {
            // Initialize Media Foundation
            info!("Initializing Media Foundation");
            media::init_media_foundation()?;
            info!("Media Foundation initialized successfully");

            // Get the video encoder
            info!("Getting video encoder: {:?}", config.video_encoder());
            let video_encoder = get_video_encoder_by_type(config.video_encoder())?;
            info!("Video encoder obtained: {:?}", video_encoder.id);
            
            // Create and configure media sink
            info!("Creating media sink writer for path: {}", output_path);
            let media_sink = media::create_sink_writer(
                output_path,
                fps_num,
                fps_den,
                output_width,
                output_height,
                capture_audio,
                capture_microphone,
                video_bitrate,
                &video_encoder.id,
            )?;
            info!("Media sink writer created successfully");

            // Find target window with exact match if specified
            info!("Finding target window with name: {} (exact match: {})", process_name, use_exact_match);
            let hwnd = if use_exact_match {
                info!("Using exact string matching for window");
                get_window_by_exact_string(process_name)
            } else {
                info!("Using substring matching for window");
                get_window_by_string(process_name)
            }.ok_or_else(|| RecorderError::FailedToStart("No window found".to_string()))?;
            info!("Found window with handle: {:?}", hwnd);

            // Get the process ID
            let mut process_id: u32 = 0;
            info!("Getting process ID for window");
            windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                hwnd,
                Some(&mut process_id),
            );
            info!("Process ID: {}", process_id);

            // Initialize recording
            info!("Beginning writing to media sink");
            media_sink.BeginWriting()?;
            info!("BeginWriting successful");
            let sendable_sink = SendableWriter(Arc::new(media_sink));
            info!("SendableWriter created");

            // Set up channels
            info!("Setting up communication channels");
            let (sender_video, receiver_video) = channel::<SendableSample>();
            info!("Video channel created");
            let (sender_audio, receiver_audio) = channel::<SendableSample>();
            info!("Audio channel created");
            let (sender_microphone, receiver_microphone) = channel::<SendableSample>();
            info!("Microphone channel created");

            // Create D3D11 device and context
            info!("Creating D3D11 device and context");
            let (device, context) = create_d3d11_device()?;
            info!("D3D11 device and context created");
            let device = Arc::new(device);
            info!("D3D11 device wrapped in Arc");
            let context_mutex = Arc::new(std::sync::Mutex::new(context));
            info!("D3D11 context wrapped in mutex");

            // Set up synchronization barrier
            let barrier_count = if capture_audio { 1 } else { 0 } + if capture_microphone { 1 } else { 0 } + 1;
            info!("Creating synchronization barrier with {} threads", barrier_count);
            let barrier = Arc::new(Barrier::new(barrier_count));
            info!("Synchronization barrier created");

            // Start video capture thread
            info!("Starting video capture thread");
            let rec_clone = recording.clone();
            let dev_clone = device.clone();
            let barrier_clone = barrier.clone();
            let process_name_clone = process_name.to_string();
            // Copy the capture_cursor value before using it in the thread
            let capture_cursor = config.capture_cursor();
            collect_video_handle = Some(std::thread::spawn(move || {
                info!("Video capture thread started");
                let result = get_frames(
                    sender_video,
                    rec_clone,
                    hwnd,
                    &process_name_clone,
                    fps_num,
                    fps_den,
                    input_width,
                    input_height,
                    barrier_clone,
                    dev_clone,
                    context_mutex,
                    use_exact_match,
                    capture_cursor,
                );
                info!("Video capture thread completed with result: {:?}", result.is_ok());
                result
            }));
            info!("Video capture thread spawned");

            let mut start_qpc_i64: i64 = 0;
            info!("Getting performance counter for timestamp synchronization");
            QueryPerformanceCounter(&mut start_qpc_i64);
            let shared_start_qpc = start_qpc_i64 as u64;
            info!("Performance counter value: {}", shared_start_qpc);

            // Start audio capture thread if enabled
            if capture_audio {
                info!("Starting audio capture thread");
                let rec_clone = recording.clone();
                let barrier_clone = barrier.clone();
                let audio_source_clone = config.audio_source().clone();
                info!("Audio source: {:?}", audio_source_clone);
                collect_audio_handle = Some(std::thread::spawn(move || {
                    info!("Audio capture thread started");
                    let result = collect_audio(
                        sender_audio, 
                        rec_clone, 
                        process_id, 
                        barrier_clone, 
                        Some(shared_start_qpc), 
                        &audio_source_clone
                    );
                    info!("Audio capture thread completed with result: {:?}", result.is_ok());
                    result
                }));
                info!("Audio capture thread spawned");
            } else {
                info!("Audio capture disabled, skipping audio thread");
            }

            // Start microphone capture thread if enabled
            if capture_microphone {
                info!("Starting microphone capture thread");
                let rec_clone = recording.clone();
                let barrier_clone = barrier.clone();
                let device_clone = microphone_device.clone();
                info!("Using microphone device: {:?}", device_clone);
                collect_microphone_handle = Some(std::thread::spawn(move || {
                    info!("Microphone capture thread started");
                    let result = collect_microphone(
                        sender_microphone, 
                        rec_clone, 
                        barrier_clone, 
                        Some(shared_start_qpc), 
                        device_clone.as_deref()
                    );
                    info!("Microphone capture thread completed with result: {:?}", result.is_ok());
                    result
                }));
                info!("Microphone capture thread spawned");
            } else {
                info!("Microphone capture disabled, skipping microphone thread");
            }

            // Start processing thread
            info!("Starting sample processing thread");
            let rec_clone = recording.clone();
            let buffer_clone = replay_buffer.clone();
            process_handle = Some(std::thread::spawn(move || {
                info!("Processing thread started");
                let result = process_samples(
                    sendable_sink,
                    receiver_video,
                    receiver_audio,
                    receiver_microphone,
                    rec_clone,
                    input_width,     // Capture dimensions
                    input_height,    // Capture dimensions
                    output_width,    // Target dimensions
                    output_height,   // Target dimensions
                    device,
                    capture_audio,
                    capture_microphone,
                    system_volume,
                    microphone_volume,
                    buffer_clone,
                );
                info!("Processing thread completed with result: {:?}", result.is_ok());
                result
            }));
            info!("Processing thread spawned");
        }

        info!("All threads initialized and running");
        info!("Recorder initialized successfully");
        Ok(Self {
            recording,
            collect_video_handle: RwLock::new(collect_video_handle),
            process_handle: RwLock::new(process_handle),
            collect_audio_handle: RwLock::new(collect_audio_handle),
            collect_microphone_handle: RwLock::new(collect_microphone_handle),
            replay_buffer: RwLock::new(replay_buffer),
            config: config.clone(),
        })
    }

    pub fn stop(&self) -> std::result::Result<(), RecorderError> {
        info!("Stop method called");
        if !self.recording.load(Ordering::Relaxed) {
            info!("Recorder already stopped, returning error");
            return Err(RecorderError::RecorderAlreadyStopped);
        }

        info!("Setting recording flag to false");
        self.recording.store(false, Ordering::Relaxed);
        info!("Recording flag set to false");

        // Join all threads and handle any errors
        // Updated to use RwLock instead of RefCell
        let mut handles = Vec::new();
        
        info!("Acquiring video thread handle");
        if let Ok(mut lock) = self.collect_video_handle.write() {
            if let Some(handle) = lock.take() {
                info!("Video thread handle acquired");
                handles.push(("Frame", handle));
            } else {
                info!("No video thread handle found");
            }
        } else {
            info!("Failed to acquire write lock for video thread handle");
        }
        
        info!("Acquiring audio thread handle");
        if let Ok(mut lock) = self.collect_audio_handle.write() {
            if let Some(handle) = lock.take() {
                info!("Audio thread handle acquired");
                handles.push(("Audio", handle));
            } else {
                info!("No audio thread handle found");
            }
        } else {
            info!("Failed to acquire write lock for audio thread handle");
        }
        
        info!("Acquiring microphone thread handle");
        if let Ok(mut lock) = self.collect_microphone_handle.write() {
            if let Some(handle) = lock.take() {
                info!("Microphone thread handle acquired");
                handles.push(("Microphone", handle));
            } else {
                info!("No microphone thread handle found");
            }
        } else {
            info!("Failed to acquire write lock for microphone thread handle");
        }
        
        info!("Acquiring processing thread handle");
        if let Ok(mut lock) = self.process_handle.write() {
            if let Some(handle) = lock.take() {
                info!("Processing thread handle acquired");
                handles.push(("Process", handle));
            } else {
                info!("No processing thread handle found");
            }
        } else {
            info!("Failed to acquire write lock for processing thread handle");
        }

        info!("Waiting for {} thread(s) to join", handles.len());
        for (name, handle) in handles.into_iter() {
            info!("Joining {} thread", name);
            if let Err(e) = handle
                .join()
                .map_err(|_| RecorderError::Generic(format!("{} Handle join failed", name)))?
            {
                error!("{} thread error: {:?}", name, e);
                info!("{} thread joined with error", name);
            } else {
                info!("{} thread joined successfully", name);
            }
        }

        info!("All threads joined, stop completed successfully");
        Ok(())
    }
    
    /// Save the content of the replay buffer to a file
    pub fn save_replay(&self, output_path: &str) -> std::result::Result<(), RecorderError> {
        info!("Saving replay buffer to {}", output_path);
        
        // Updated to use RwLock instead of RefCell
        info!("Acquiring read lock for replay buffer");
        let replay_buffer = self.replay_buffer.read()
            .map_err(|_| RecorderError::Generic("Failed to acquire replay buffer lock".to_string()))?;
        info!("Replay buffer lock acquired");
            
        let buffer = replay_buffer.as_ref().ok_or_else(|| {
            info!("Replay buffer is not enabled");
            RecorderError::Generic("Replay buffer is not enabled".to_string())
        })?;
        
        info!("Replay buffer reference obtained");
        
        // Get the current time range from the buffer
        info!("Getting current duration from replay buffer");
        let duration = buffer.current_duration();
        info!("Replay buffer current duration: {:?}", duration);
        if duration.as_secs() == 0 {
            info!("Replay buffer is empty, returning error");
            return Err(RecorderError::Generic("Replay buffer is empty".to_string()));
        }
        
        info!("Replay buffer contains {} seconds of data", duration.as_secs_f64());
        
        unsafe {
            // Get the oldest timestamp in the buffer
            info!("Getting oldest timestamp from buffer");
            let oldest_timestamp = *buffer.oldest_timestamp.lock().unwrap();
            info!("Oldest timestamp: {}", oldest_timestamp);
            
            // Get all video and audio samples from the buffer (within the time range)
            info!("Retrieving samples from buffer");
            let now = std::time::Instant::now();
            info!("Retrieving video samples");
            let video_samples = buffer.get_video_samples(oldest_timestamp, i64::MAX);
            info!("Retrieved {} video samples", video_samples.len());
            info!("Retrieving audio samples");
            let audio_samples = buffer.get_audio_samples(oldest_timestamp, i64::MAX);
            info!("Retrieved {} audio samples", audio_samples.len());
            info!("Retrieved {} video frames and {} audio samples in {:?}",
                video_samples.len(), audio_samples.len(), now.elapsed());
            
            if video_samples.is_empty() {
                info!("No video frames in replay buffer, returning error");
                return Err(RecorderError::Generic("No video frames in replay buffer".to_string()));
            }
    
            info!("Getting video encoder for replay file");
            let video_encoder = get_video_encoder_by_type(self.config.video_encoder())?;
            info!("Video encoder obtained: {:?}", video_encoder.id);
                
            info!("Creating sink writer for replay file");
            let media_sink = media::create_sink_writer(
                output_path,
                self.config.fps_num(),
                self.config.fps_den(),
                self.config.output_width(),
                self.config.output_height(),
                self.config.capture_audio(),
                self.config.capture_microphone(),
                self.config.video_bitrate(),
                &video_encoder.id,
            )?;
            info!("Created sink writer for replay file");
            
            // Begin writing
            info!("Beginning writing to replay file");
            media_sink.BeginWriting()?;
            info!("Begin writing successful");
            
            // Define stream indices
            let video_stream_index = 0;
            info!("Video stream index: {}", video_stream_index);
            let audio_stream_index = if !audio_samples.is_empty() { 1 } else { 0 };
            info!("Audio stream index: {}", audio_stream_index);
            
            // Find the earliest timestamp to use as a reference for normalization
            let earliest_timestamp = if !video_samples.is_empty() {
                video_samples[0].1
            } else if !audio_samples.is_empty() {
                audio_samples[0].1
            } else {
                oldest_timestamp
            };
            
            info!("Using earliest timestamp for normalization: {}", earliest_timestamp);
            
            // Write video samples with normalized timestamps
            info!("Writing {} video frames to replay file", video_samples.len());
            for (i, (sample, timestamp)) in video_samples.iter().enumerate() {
                if i % 50 == 0 || i == video_samples.len() - 1 {
                    info!("Writing video frame {}/{}", i + 1, video_samples.len());
                }
                
                // Calculate normalized timestamp (relative to the earliest timestamp)
                let normalized_timestamp = timestamp - earliest_timestamp;
                
                // Set the normalized timestamp directly on the sample
                sample.SetSampleTime(normalized_timestamp)?;
                
                // Write the sample with the normalized timestamp
                info!("Writing audio sample with timestamp: {}", normalized_timestamp);
                    media_sink.WriteSample(audio_stream_index, &***sample)?;
            }
            info!("Finished writing all video frames");
            
            // Write audio samples with normalized timestamps
            if !audio_samples.is_empty() {
                info!("Writing {} audio samples to replay file", audio_samples.len());
                for (i, (sample, timestamp)) in audio_samples.iter().enumerate() {
                    if i % 50 == 0 || i == audio_samples.len() - 1 {
                        info!("Writing audio sample {}/{}", i + 1, audio_samples.len());
                    }
                    
                    // Calculate normalized timestamp (relative to the earliest timestamp)
                    let normalized_timestamp = timestamp - earliest_timestamp;
                    
                    // Set the normalized timestamp directly on the sample
                    sample.SetSampleTime(normalized_timestamp)?;
                    
                    // Write the sample with the normalized timestamp
                    media_sink.WriteSample(audio_stream_index, &***sample)?;
                }
                info!("Finished writing all audio samples");
            }
            
            // Finalize the media sink
            info!("Finalizing media sink");
            media_sink.Finalize()?;
            info!("Media sink finalized");
            info!("Replay buffer saved to {} in {:?}", output_path, now.elapsed());
        }
        
        info!("save_replay completed successfully");
        Ok(())
    }
}

impl Drop for RecorderInner {
    fn drop(&mut self) {
        unsafe {
            #[cfg(debug_assertions)]
            log::info!("RecorderInner is being dropped, cleaning up resources");
            
            // Ensure recording flag is set to false to terminate threads
            if self.recording.load(std::sync::atomic::Ordering::Relaxed) {
                #[cfg(debug_assertions)]
                log::warn!("Recording flag was still true during drop; setting to false");
                self.recording.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            
            #[cfg(debug_assertions)]
            log::info!("Shutting down Media Foundation");
            
            let _ = media::shutdown_media_foundation();
            
            #[cfg(debug_assertions)]
            log::info!("RecorderInner cleanup complete");
        }
    }
}

unsafe fn create_d3d11_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    info!("Creating D3D11 device");
    let feature_levels = [
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
        D3D_FEATURE_LEVEL_9_3,
        D3D_FEATURE_LEVEL_9_2,
        D3D_FEATURE_LEVEL_9_1,
    ];
    info!("Feature levels defined");

    let mut device = None;
    let mut context = None;
    
    // Base flags
    let mut flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
    info!("Base D3D11 creation flags: {:?}", flags);
    
    // In debug builds, try to use debug layer
    #[cfg(debug_assertions)]
    {
        info!("Adding debug layer flag in debug build");
        flags |= D3D11_CREATE_DEVICE_DEBUG;
        info!("D3D11 creation flags with debug: {:?}", flags);
    }

    // Try to create device with debug layer first
    info!("Attempting to create D3D11 device with current flags");
    let result = D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        None,
        flags,
        Some(&feature_levels),
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        Some(&mut context),
    );

    // If debug layer is not available, retry without it
    if let Err(e) = result {
        info!("D3D11 device creation failed with error: {:?}", e);
        if e.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_SDK_COMPONENT_MISSING {
            info!("Debug layer not available, falling back to non-debug creation");
            flags &= !D3D11_CREATE_DEVICE_DEBUG;
            info!("New flags without debug: {:?}", flags);
            info!("Retrying D3D11 device creation without debug flag");
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            info!("D3D11 device created successfully without debug flag");
        } else {
            error!("Failed to create D3D11 device: {:?}", e);
            return Err(e);
        }
    } else {
        info!("D3D11 device created successfully on first attempt");
    }

    let device = device.unwrap();
    info!("D3D11 device unwrapped");
    let context = context.unwrap();
    info!("D3D11 context unwrapped");

    // Enable multi-threading
    info!("Enabling multi-threading on D3D11 device");
    let multithread: ID3D11Multithread = device.cast()?;
    multithread.SetMultithreadProtected(true);
    info!("Multi-threading enabled on D3D11 device");

    #[cfg(debug_assertions)]
    {
        info!("Checking for debug interfaces in debug build");
        // Try to enable resource tracking via debug interface
        if let Ok(_debug) = device.cast::<ID3D11Debug>() {
            info!("D3D11 Debug interface available - resource tracking enabled");
            
            // Enable debug info tracking
            info!("Attempting to get info queue interface");
            if let Ok(info_queue) = device.cast::<ID3D11InfoQueue>() {
                info!("Info queue interface acquired, configuring error tracking");
                // Configure info queue to break on D3D11 errors
                info_queue.SetBreakOnSeverity(D3D11_MESSAGE_SEVERITY_ERROR, true)?;
                info!("Break on error severity enabled");
                info_queue.SetBreakOnSeverity(D3D11_MESSAGE_SEVERITY_CORRUPTION, true)?;
                info!("Break on corruption severity enabled");
                info!("D3D11 Info Queue configured for error tracking");
            } else {
                info!("Info queue interface not available");
            }
        } else {
            info!("D3D11 Debug interface not available");
        }
    }

    info!("D3D11 device creation completed successfully");
    Ok((device, context))
}