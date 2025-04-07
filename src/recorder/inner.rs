use log::{debug, error, info, warn};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1, DXGI_OUTPUT_DESC};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::RwLock;
use std::thread::JoinHandle;
use windows::core::{ComInterface, Result};
use windows::Win32::Foundation::{HWND, RECT, POINT};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::System::Performance::QueryPerformanceCounter;

use super::config::RecorderConfig;
use crate::capture::window::get_window_rect;
use crate::capture::{
    collect_audio, collect_microphone, get_frames, get_window_by_exact_string, get_window_by_string,
};
use crate::device::get_audio_input_device_by_name;
use crate::error::RecorderError;
use crate::processing::{media, process_samples};
use crate::types::{ReplayBuffer, SendableDxgiDeviceManager, SendableSample, SendableWriter};

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

    pub fn init_with_exact_match(
        config: &RecorderConfig,
        process_name: &str,
        use_exact_match: bool,
    ) -> Result<Self> {
        info!(
            "Initializing recorder for process: {} with exact match: {}",
            process_name, use_exact_match
        );
        // Find target window
        info!(
            "Finding target window with name: {} (exact match: {})",
            process_name, use_exact_match
        );
        let hwnd = if use_exact_match {
            info!("Using exact string matching for window");
            get_window_by_exact_string(process_name)
        } else {
            info!("Using substring matching for window");
            get_window_by_string(process_name)
        }
        .ok_or_else(|| RecorderError::FailedToStart("No window found".to_string()))?;
        info!("Found window with handle: {:?}", hwnd);

        // Determine input resolution (auto-detect if not specified)
        let (actual_input_width, actual_input_height) =
            match (config.input_width(), config.input_height()) {
                (Some(width), Some(height)) => {
                    info!(
                        "Using user-specified input dimensions: {}x{}",
                        width, height
                    );
                    (width, height)
                }
                _ => {
                    info!("Input dimensions not specified, auto-detecting from monitor");
                    let (width, height) = crate::capture::get_window_monitor_resolution(hwnd);
                    info!("Auto-detected monitor resolution: {}x{}", width, height);
                    (width, height)
                }
            };

        info!(
            "Config details - fps: {}/{}, input: {}x{}, output: {}x{}, audio: {}, mic: {}",
            config.fps_num(),
            config.fps_den(),
            actual_input_width,
            actual_input_height,
            config.output_width(),
            config.output_height(),
            config.capture_audio(),
            config.capture_microphone()
        );

        // Clone the necessary values from config at the start
        let fps_num = config.fps_num();
        let fps_den = config.fps_den();
        let input_width = actual_input_width;
        let input_height = actual_input_height;
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
            info!(
                "Attempting to get device ID for microphone: '{}'",
                device_name
            );
            match get_audio_input_device_by_name(Some(device_name)) {
                Ok(device_id) => {
                    info!("Found device ID for '{}': {}", device_name, device_id);
                    Some(device_id)
                }
                Err(e) => {
                    info!(
                        "Could not get device ID for '{}', using default: {:?}",
                        device_name, e
                    );
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
            info!(
                "Replay buffer is enabled with {} seconds",
                config.replay_buffer_seconds()
            );
            let buffer_duration =
                std::time::Duration::from_secs(config.replay_buffer_seconds() as u64);
            let fps = fps_num as f64 / fps_den as f64;

            // Estimate the number of frames and audio samples in the buffer
            let video_frames = (fps * buffer_duration.as_secs_f64()) as usize;
            let audio_samples = if capture_audio || capture_microphone {
                // Audio at 44.1kHz, assume ~10 packets per second for buffer capacity
                (10.0 * buffer_duration.as_secs_f64()) as usize
            } else {
                0
            };

            info!(
                "Creating replay buffer for {} seconds ({} video frames, {} audio samples)",
                config.replay_buffer_seconds(),
                video_frames,
                audio_samples
            );

            let buffer = Some(Arc::new(ReplayBuffer::new(
                buffer_duration,
                video_frames,
                audio_samples,
            )));
            info!("Replay buffer created successfully");
            buffer
        } else {
            info!("Replay buffer is disabled");
            None
        };

        // Parse out path string from PathBuf
        info!("Getting output path string");
        let output_path = config
            .output_path()
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
            info!("Getting video encoder");
            let video_encoder = if let Some(encoder_name) = config.video_encoder_name() {
                info!("Looking for encoder by name: '{}'", encoder_name);
                match crate::device::get_video_encoder_by_name(encoder_name) {
                    Some(encoder) => {
                        info!(
                            "Found encoder by name: '{}' ({:?})",
                            encoder_name, encoder.encoder_type
                        );
                        encoder
                    }
                    None => {
                        info!("Encoder '{}' not found, falling back to type", encoder_name);
                        crate::device::get_video_encoder_by_type(*config.video_encoder())?
                    }
                }
            } else {
                info!("Looking for encoder by type: {:?}", config.video_encoder());
                crate::device::get_video_encoder_by_type(*config.video_encoder())?
            };
            info!(
                "Video encoder obtained: {} ({:?})",
                video_encoder.name, video_encoder.encoder_type
            );

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
                &video_encoder.output_format_guid, // Use output_format_guid instead of id
            )?;
            info!("Media sink writer created successfully");

            // Window handle already found at the beginning

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

            // Get initial window position and size before creating channels
            info!("Getting initial window position and size");
            let initial_window_position: Option<(i32, i32)>;
            let initial_window_size: Option<(u32, u32)>;

            if let Some((x, y, width, height)) = get_window_rect(hwnd) {
                info!(
                    "Initial window rect - Position: [{}, {}], Size: {}x{}",
                    x, y, width, height
                );
                initial_window_position = Some((x, y));
                initial_window_size = Some((width, height));
            } else {
                info!("Failed to get initial window rect, starting with None values");
                initial_window_position = None;
                initial_window_size = None;
            }

            let (sender_window_info, receiver_window_info) =
                channel::<(Option<(i32, i32)>, Option<(u32, u32)>)>();
            info!(
                "Window info channel created with initial position: {:?}, initial size: {:?}",
                initial_window_position, initial_window_size
            );

            // Create D3D11 device and context specifically for the window's adapter
            info!("Creating D3D11 device and context for the window's adapter");
            let (device, context) = create_d3d11_device_for_window(hwnd)?;
            info!("D3D11 device and context created for window's adapter");
            let device = Arc::new(device);
            info!("D3D11 device wrapped in Arc");
            let context_mutex = Arc::new(std::sync::Mutex::new(context));
            info!("D3D11 context wrapped in mutex");
            // --- NEW: Create and Setup DXGI Device Manager ---
            let dxgi_device_manager: IMFDXGIDeviceManager = {
                let mut reset_token: u32 = 0;
                let mut manager_option: Option<IMFDXGIDeviceManager> = None;
                
                // Call with both required arguments
                unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut manager_option)? };
                
                // Unwrap the option to get the actual manager
                let manager = manager_option.unwrap();
                
                // Reset the manager with the created device
                manager.ResetDevice(&*device, reset_token)?;
                
                info!("DXGI Device Manager created and associated with D3D11 device.");
                manager
            };

            // Set up synchronization barrier
            // Always include video thread (1) plus audio if enabled
            // The microphone thread will wait on the barrier even if it fails,
            // so we need to include it in the count if microphone capture is enabled
            let barrier_count =
                1 + // Video thread always included
                (if capture_audio { 1 } else { 0 }) + 
                (if capture_microphone { 1 } else { 0 });
                
            info!(
                "Creating synchronization barrier with {} threads",
                barrier_count
            );
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
                    sender_window_info,
                );
                info!(
                    "Video capture thread completed with result: {:?}",
                    result.is_ok()
                );
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
                        &audio_source_clone,
                    );
                    info!(
                        "Audio capture thread completed with result: {:?}",
                        result.is_ok()
                    );
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
                        device_clone.as_deref(),
                    );
                    
                    // Check for the specific "Element not found" error (0x80070490)
                    if let Err(e) = &result {
                        // Convert HSTRING to String for comparison
                        let error_message = e.code().message().to_string_lossy();
                        if error_message.contains("Element not found") {
                            info!("No microphone device found (Error {:X}). Recording will continue without microphone.", e.code().0);
                            // Return Ok to allow recording to continue without microphone
                            return Ok(());
                        }
                    }
                    
                    info!(
                        "Microphone capture thread completed with result: {:?}",
                        result.is_ok()
                    );
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
            let initial_pos = initial_window_position;
            let initial_size = initial_window_size;
            let device_clone_for_processing = device.clone();
            // --- NEW: Clone the DXGI Device Manager for the processing thread ---
            let sendable_dxgi_manager = SendableDxgiDeviceManager(dxgi_device_manager);
            let dxgi_manager_clone = Arc::new(sendable_dxgi_manager);
            // --- End NEW ---

            // Create the texture pool for processing using the same dimensions as the capture
            info!("Creating NV12 texture pool for video processing output");
            let processing_texture_pool = crate::types::TexturePool::new(
                device.clone(), // Use the Arc'd device
                3,                  // Pool capacity (e.g., 3 textures)
                output_width,       // Use OUTPUT dimensions
                output_height,
                DXGI_FORMAT_NV12,   // Format is NV12
                D3D11_USAGE_DEFAULT,// Default GPU usage
                D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, // Needed for MFT output
                D3D11_CPU_ACCESS_FLAG(0), // No CPU access needed
                D3D11_RESOURCE_MISC_FLAG(0), // NO GDI compatibility for NV12!
            )?;
            let processing_texture_pool = Arc::new(processing_texture_pool);
            info!("Created NV12 processing texture pool successfully.");
            let processing_texture_pool_clone = processing_texture_pool.clone();
            
            process_handle = Some(std::thread::spawn(move || {
                info!("Processing thread started");
                let result = process_samples(
                    sendable_sink,
                    receiver_video,
                    receiver_audio,
                    receiver_microphone,
                    receiver_window_info,
                    rec_clone,
                    input_width,   // Capture dimensions
                    input_height,  // Capture dimensions
                    output_width,  // Target dimensions
                    output_height, // Target dimensions
                    device_clone_for_processing,
                    dxgi_manager_clone,
                    capture_audio,
                    capture_microphone,
                    system_volume,
                    microphone_volume,
                    buffer_clone,
                    initial_pos,  // Initial window position
                    initial_size, // Initial window size
                    processing_texture_pool_clone, // Texture pool for processing
                );
                info!(
                    "Processing thread completed with result: {:?}",
                    result.is_ok()
                );
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

        info!("Acquiring read lock for replay buffer");
        let replay_buffer = self.replay_buffer.read().map_err(|_| {
            RecorderError::Generic("Failed to acquire replay buffer lock".to_string())
        })?;
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

        info!(
            "Replay buffer contains {} seconds of data",
            duration.as_secs_f64()
        );

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
            info!(
                "Retrieved {} video frames and {} audio samples in {:?}",
                video_samples.len(),
                audio_samples.len(),
                now.elapsed()
            );

            if video_samples.is_empty() {
                info!("No video frames in replay buffer, returning error");
                return Err(RecorderError::Generic(
                    "No video frames in replay buffer".to_string(),
                ));
            }

            info!("Getting video encoder for replay file");
            let video_encoder = if let Some(encoder_name) = self.config.video_encoder_name() {
                info!("Looking for encoder by name: '{}'", encoder_name);
                match crate::device::get_video_encoder_by_name(encoder_name) {
                    Some(encoder) => {
                        info!(
                            "Found encoder by name: '{}' ({:?})",
                            encoder_name, encoder.encoder_type
                        );
                        encoder
                    }
                    None => {
                        info!("Encoder '{}' not found, falling back to type", encoder_name);
                        crate::device::get_video_encoder_by_type(*self.config.video_encoder())?
                    }
                }
            } else {
                info!(
                    "Looking for encoder by type: {:?}",
                    self.config.video_encoder()
                );
                crate::device::get_video_encoder_by_type(*self.config.video_encoder())?
            };
            info!(
                "Video encoder obtained: {} ({:?})",
                video_encoder.name, video_encoder.encoder_type
            );

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
                &video_encoder.output_format_guid, // Use output_format_guid instead of id
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

            info!(
                "Using earliest timestamp for normalization: {}",
                earliest_timestamp
            );

            // Write video samples with normalized timestamps
            info!(
                "Writing {} video frames to replay file",
                video_samples.len()
            );
            for (i, (sample, timestamp)) in video_samples.iter().enumerate() {
                if i % 50 == 0 || i == video_samples.len() - 1 {
                    info!("Writing video frame {}/{}", i + 1, video_samples.len());
                }

                // Calculate normalized timestamp (relative to the earliest timestamp)
                let normalized_timestamp = timestamp - earliest_timestamp;

                // Set the normalized timestamp directly on the sample
                sample.SetSampleTime(normalized_timestamp)?;

                // Write the sample with the normalized timestamp
                info!(
                    "Writing audio sample with timestamp: {}",
                    normalized_timestamp
                );
                media_sink.WriteSample(audio_stream_index, &***sample)?;
            }
            info!("Finished writing all video frames");

            // Write audio samples with normalized timestamps
            if !audio_samples.is_empty() {
                info!(
                    "Writing {} audio samples to replay file",
                    audio_samples.len()
                );
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
            info!(
                "Replay buffer saved to {} in {:?}",
                output_path,
                now.elapsed()
            );
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
                self.recording
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            }

            #[cfg(debug_assertions)]
            log::info!("Shutting down Media Foundation");

            let _ = media::shutdown_media_foundation();

            #[cfg(debug_assertions)]
            log::info!("RecorderInner cleanup complete");
        }
    }
}

/// Creates a D3D11 device for a specific window
/// This ensures the device is created on the correct adapter for the window
unsafe fn create_d3d11_device_for_window(
    hwnd: HWND,
) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    info!("Attempting to create D3D11 device for window: {:?}", hwnd);
    let feature_levels = [
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
        D3D_FEATURE_LEVEL_9_3,
        D3D_FEATURE_LEVEL_9_2,
        D3D_FEATURE_LEVEL_9_1,
    ];

    // Base flags
    let mut creation_flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;

    // --- Add Debug Flag in Debug Builds ---
    if cfg!(debug_assertions) {
        creation_flags |= D3D11_CREATE_DEVICE_DEBUG;
        info!("D3D11 Debug Layer ENABLED (debug build). Ensure Graphics Tools are installed.");
    } else {
        info!("D3D11 Debug Layer DISABLED (release build).");
    }
    // --- End Debug Flag ---


    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;

    // Create DXGI Factory to enumerate adapters
    let dxgi_factory: IDXGIFactory1 = CreateDXGIFactory1()?;
    debug!("DXGI Factory created.");

    // Get the monitor the window is primarily on
    // MONITOR_DEFAULTTONEAREST is often better than DEFAULTTOPRIMARY if the window spans monitors
    let window_monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    debug!("Window monitor handle: {:?}", window_monitor);

    // Get window center point for adapter matching (fallback if monitor handle match fails)
    let mut window_rect = RECT::default();
    let window_center = if GetWindowRect(hwnd, &mut window_rect).as_bool() {
        debug!("Window Rect: {:?}", window_rect);
        POINT {
            x: (window_rect.left + window_rect.right) / 2,
            y: (window_rect.top + window_rect.bottom) / 2,
        }
    } else {
        warn!("Failed to get window rect, using origin (0,0) for adapter matching.");
        POINT { x: 0, y: 0 }
    };
    debug!("Window center point: {:?}", window_center);


    // Try to find the matching adapter for the window
    let mut adapter_index = 0;
    let mut target_adapter: Option<IDXGIAdapter> = None; // Store the adapter directly

    info!("Enumerating adapters to find match for window's monitor...");
    loop {
        let adapter_result = dxgi_factory.EnumAdapters(adapter_index);
        match adapter_result {
            Ok(adapter) => {
                let mut desc = Default::default();
                if adapter.GetDesc(&mut desc).is_ok() {
                     let description = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string();
                     debug!(" Checking Adapter {}: {}", adapter_index, description);
                }


                let mut output_index = 0;
                loop {
                    match adapter.EnumOutputs(output_index) {
                        Ok(output) => {
                            let mut output_desc = DXGI_OUTPUT_DESC::default();
                            if output.GetDesc(&mut output_desc).is_ok() {
                                debug!("   Checking Output {}: Monitor={:?}, DesktopCoordinates={:?}", output_index, output_desc.Monitor, output_desc.DesktopCoordinates);
                                // Check 1: Direct monitor handle comparison
                                if output_desc.Monitor == window_monitor {
                                    info!(
                                        "   Found matching adapter ({}) via Monitor Handle.",
                                         adapter_index
                                    );
                                    target_adapter = Some(adapter); // Clone or transfer ownership? EnumAdapters gives a ref counted ptr
                                    break; // Found adapter for this output
                                }

                                // Check 2: Window center point within output desktop coordinates
                                let rect = output_desc.DesktopCoordinates;
                                if window_center.x >= rect.left
                                    && window_center.x < rect.right
                                    && window_center.y >= rect.top
                                    && window_center.y < rect.bottom
                                {
                                    info!(
                                        "   Found matching adapter ({}) via Window Center Point.",
                                        adapter_index
                                    );
                                    target_adapter = Some(adapter);
                                    break; // Found adapter for this output
                                }
                            }
                            // Implicitly drops `output` COM pointer here
                            output_index += 1;
                        }
                        Err(e) => {
                            if e.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_NOT_FOUND {
                                debug!("   No more outputs on adapter {}.", adapter_index);
                            } else {
                                warn!("   Error enumerating outputs on adapter {}: {:?}", adapter_index, e);
                            }
                             break; // No more outputs on this adapter
                        }
                    }
                } // End output loop

                // If we found a matching adapter, break the outer loop
                if target_adapter.is_some() {
                    break;
                }

                // Implicitly drops `adapter` COM pointer here
                adapter_index += 1;
            }
            Err(e) => {
                if e.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_NOT_FOUND {
                     info!("Finished enumerating adapters ({} total checked).", adapter_index);
                } else {
                     warn!("Error enumerating adapters: {:?}", e);
                }
                break; // No more adapters
            }
        }
    } // End adapter loop

    // Try to create device on the target adapter first
    if let Some(adapter) = target_adapter.as_ref() { // Borrow adapter for the creation call
        info!("Attempting device creation on targeted adapter.");
        let result = D3D11CreateDevice(
            Some(adapter), // Pass the specific adapter
            D3D_DRIVER_TYPE_UNKNOWN, // Must be UNKNOWN when specifying an adapter
            None, // No software module
            creation_flags, // Use the flags (with potential DEBUG flag)
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None, // Don't need feature level output
            Some(&mut context),
        );

        // Check if creation on target adapter succeeded
        if result.is_ok() {
             info!("Successfully created D3D11 device on targeted adapter.");
        } else {
            warn!(
                "Failed to create device on targeted adapter ({:?}), falling back to default.",
                result.err().unwrap() // Log the error
            );
            device = None; // Ensure they are None for the fallback logic
            context = None;
        }
    } else {
        info!("No specific adapter found for window's monitor/location. Using default adapter.");
    }

    // If we still don't have a device (either no target adapter or creation failed), create with default adapter
    if device.is_none() {
        info!("Attempting device creation on default hardware adapter.");
        D3D11CreateDevice(
            None, // Use default adapter
            D3D_DRIVER_TYPE_HARDWARE, // Request hardware adapter
            None, // No software module
            creation_flags, // Use the flags (with potential DEBUG flag)
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None, // Don't need feature level output
            Some(&mut context),
        )?; // Propagate error if default creation fails
         info!("Successfully created D3D11 device on default adapter.");
    }

    // Unwrap device and context - guaranteed to be Some if we reached here without error
    let device_unwrapped = device.unwrap();
    let context_unwrapped = context.unwrap();

    // Enable multi-threading protection on the device
    debug!("Enabling D3D11 multithread protection.");
    let multithread: ID3D11Multithread = device_unwrapped.cast()?;
    multithread.SetMultithreadProtected(true); // Check result
    debug!("Multithread protection set.");

    info!("D3D11 device and context creation complete.");
    Ok((device_unwrapped, context_unwrapped))
}