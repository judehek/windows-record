use log::{error, info};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread::JoinHandle;
use windows::core::{ComInterface, Result};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;

use super::config::RecorderConfig;
use crate::capture::{collect_audio, collect_frames, collect_microphone, find_window_by_substring};
use crate::error::RecorderError;
use crate::processing::{media, process_samples};
use crate::recorder::utils::get_encoder_guid;
use crate::types::{SendableSample, SendableWriter};

pub struct RecorderInner {
    recording: Arc<AtomicBool>,
    collect_video_handle: RefCell<Option<JoinHandle<Result<()>>>>,
    process_handle: RefCell<Option<JoinHandle<Result<()>>>>,
    collect_audio_handle: RefCell<Option<JoinHandle<Result<()>>>>,
    collect_microphone_handle: RefCell<Option<JoinHandle<Result<()>>>>,
}

impl RecorderInner {
    pub fn init(config: &RecorderConfig, process_name: &str) -> Result<Self> {
        info!("Initializing recorder for process: {}", process_name);

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
        let encoder_guid = config.encoder();


        // Parse out path string from PathBuf
        let output_path = config.output_path()
            .to_str()
            .ok_or_else(|| RecorderError::FailedToStart("Invalid path string".to_string()))?;

        let recording = Arc::new(AtomicBool::new(true));
        let mut collect_video_handle: Option<JoinHandle<Result<()>>> = None;
        let mut process_handle: Option<JoinHandle<Result<()>>> = None;
        let mut collect_audio_handle: Option<JoinHandle<Result<()>>> = None;
        let mut collect_microphone_handle: Option<JoinHandle<Result<()>>> = None;

        unsafe {
            // Initialize Media Foundation
            media::init_media_foundation()?;

            // Create and configure media sink
            let media_sink = media::create_sink_writer(
                output_path,
                fps_num,
                fps_den,
                output_width,
                output_height,
                capture_audio,
                capture_microphone,
                video_bitrate,
                encoder_guid,
            )?;

            // Find target window
            let hwnd = find_window_by_substring(process_name)
                .ok_or_else(|| RecorderError::FailedToStart("No window found".to_string()))?;

            // Get the process ID
            let mut process_id: u32 = 0;
            windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                hwnd,
                Some(&mut process_id),
            );
            info!("Process ID: {}", process_id);

            // Initialize recording
            media_sink.BeginWriting()?;
            let sendable_sink = SendableWriter(Arc::new(media_sink));

            // Set up channels
            let (sender_video, receiver_video) = channel::<SendableSample>();
            let (sender_audio, receiver_audio) = channel::<SendableSample>();
            let (sender_microphone, receiver_microphone) = channel::<SendableSample>(); // Moved outside if block

            // Create D3D11 device and context
            let (device, context) = create_d3d11_device()?;
            let device = Arc::new(device);
            let context_mutex = Arc::new(std::sync::Mutex::new(context));

            // Set up synchronization barrier
            let barrier = Arc::new(Barrier::new(
                if capture_audio { 1 } else { 0 } + if capture_microphone { 1 } else { 0 } + 1,
            ));

            // Start video capture thread
            let rec_clone = recording.clone();
            let dev_clone = device.clone();
            let barrier_clone = barrier.clone();
            collect_video_handle = Some(std::thread::spawn(move || {
                collect_frames(
                    sender_video,
                    rec_clone,
                    hwnd,
                    fps_num,
                    fps_den,
                    input_width,
                    input_height,
                    barrier_clone,
                    dev_clone,
                    context_mutex,
                )
            }));

            // Start audio capture thread if enabled
            if capture_audio {
                let rec_clone = recording.clone();
                let barrier_clone = barrier.clone();
                collect_audio_handle = Some(std::thread::spawn(move || {
                    collect_audio(sender_audio, rec_clone, process_id, barrier_clone)
                }));
            }

            // Start microphone capture thread if enabled
            if capture_microphone {
                let rec_clone = recording.clone();
                let barrier_clone = barrier.clone();
                collect_microphone_handle = Some(std::thread::spawn(move || {
                    collect_microphone(sender_microphone, rec_clone, barrier_clone)
                }));
            }

            // Start processing thread
            let rec_clone = recording.clone();
            process_handle = Some(std::thread::spawn(move || {
                process_samples(
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
                )
            }));
        }

        info!("Recorder initialized successfully");
        Ok(Self {
            recording,
            collect_video_handle: RefCell::new(collect_video_handle),
            process_handle: RefCell::new(process_handle),
            collect_audio_handle: RefCell::new(collect_audio_handle),
            collect_microphone_handle: RefCell::new(collect_microphone_handle),
        })
    }

    pub fn stop(&self) -> std::result::Result<(), RecorderError> {
        info!("Stopping recorder");
        if !self.recording.load(Ordering::Relaxed) {
            return Err(RecorderError::RecorderAlreadyStopped);
        }

        self.recording.store(false, Ordering::Relaxed);

        // Join all threads and handle any errors
        let handles = [
            ("Frame", self.collect_video_handle.borrow_mut().take()),
            ("Audio", self.collect_audio_handle.borrow_mut().take()),
            (
                "Microphone",
                self.collect_microphone_handle.borrow_mut().take(),
            ),
            ("Process", self.process_handle.borrow_mut().take()),
        ];

        for (name, handle) in handles.into_iter() {
            if let Some(handle) = handle {
                if let Err(e) = handle
                    .join()
                    .map_err(|_| RecorderError::Generic(format!("{} Handle join failed", name)))?
                {
                    error!("{} thread error: {:?}", name, e);
                }
            }
        }

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
    let feature_levels = [
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
        D3D_FEATURE_LEVEL_9_3,
        D3D_FEATURE_LEVEL_9_2,
        D3D_FEATURE_LEVEL_9_1,
    ];

    let mut device = None;
    let mut context = None;
    
    // Base flags always include BGRA support
    let mut flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
    
    // In debug builds, try to use debug layer
    #[cfg(debug_assertions)]
    {
        flags |= D3D11_CREATE_DEVICE_DEBUG;
    }

    // Try to create device with debug layer first
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
        if e.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_SDK_COMPONENT_MISSING {
            info!("Debug layer not available, falling back to non-debug creation");
            flags &= !D3D11_CREATE_DEVICE_DEBUG;
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
        } else {
            error!("Failed to create D3D11 device: {:?}", e);
            return Err(e);
        }
    }

    let device = device.unwrap();
    let context = context.unwrap();

    // Enable multi-threading
    let multithread: ID3D11Multithread = device.cast()?;
    multithread.SetMultithreadProtected(true);
    
    #[cfg(debug_assertions)]
    {
        // Try to enable resource tracking via debug interface
        let mut debug: Option<ID3D11Debug> = None;
        if device.QueryInterface(&mut debug).is_ok() {
            if let Some(debug_ref) = &debug {
                info!("D3D11 Debug interface available - resource tracking enabled");
                
                // Enable debug info tracking
                let mut info_queue: Option<ID3D11InfoQueue> = None;
                if device.QueryInterface(&mut info_queue).is_ok() {
                    if let Some(info_queue) = &info_queue {
                        // Configure info queue to break on D3D11 errors
                        info_queue.SetBreakOnSeverity(D3D11_MESSAGE_SEVERITY_ERROR, true);
                        info_queue.SetBreakOnSeverity(D3D11_MESSAGE_SEVERITY_CORRUPTION, true);
                        info!("D3D11 Info Queue configured for error tracking");
                    }
                }
            }
        } else {
            info!("D3D11 Debug interface not available");
        }
    }

    Ok((device, context))
}
