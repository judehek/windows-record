use log::{debug, error, info, trace, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SendError, Sender};
use std::sync::Arc;
use std::sync::{Barrier, Mutex};
use std::time::{Duration, Instant};
use windows::core::Error;
use windows::core::{ComInterface, Error as WindowsError, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::{IDXGIOutputDuplication, IDXGIResource};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use super::dxgi::{create_blank_dxgi_texture, setup_dxgi_duplication};
use super::window::{is_window_valid, get_window_title};
use crate::capture::dxgi::create_staging_texture;
use crate::types::{SendableSample, TexturePool, SamplePool};

/// Struct to manage window target state
struct WindowTracker {
    /// The original target window handle
    hwnd: HWND,
    /// The process name to search for if the window handle becomes invalid
    process_name: String,
    /// The last time we checked if the window is still valid
    last_check: Instant,
    /// Time between validity checks (to avoid checking every frame)
    check_interval: Duration,
    /// Whether we have seen the window in focus at least once
    ever_focused: bool,
    /// Whether to use exact matching
    use_exact_match: bool,
}

impl WindowTracker {
    /// Create a new window tracker
    fn new(hwnd: HWND, process_name: &str) -> Self {
        Self::new_with_exact_match(hwnd, process_name, false)
    }
    
    /// Create a new window tracker with option for exact matching
    fn new_with_exact_match(hwnd: HWND, process_name: &str, use_exact_match: bool) -> Self {
        Self {
            hwnd,
            process_name: process_name.to_string(),
            last_check: Instant::now(),
            check_interval: Duration::from_secs(2), // Check every 2 seconds
            ever_focused: false,
            use_exact_match,
        }
    }
    
    /// Check if the window is currently in focus
    fn is_focused(&mut self) -> bool {
        let foreground_window = unsafe { GetForegroundWindow() };
        let is_target_window = foreground_window == self.hwnd;
        
        if is_target_window {
            // If window is now in focus, remember this
            self.ever_focused = true;
        }
        
        is_target_window
    }
    
    /// Ensure the window handle is still valid, and try to find it again if needed
    fn ensure_valid_window(&mut self) -> bool {
        let now = Instant::now();
        
        // Don't check too frequently
        if now.duration_since(self.last_check) < self.check_interval {
            return true;
        }
        
        self.last_check = now;
        
        // If the window is still valid, we're good
        if is_window_valid(self.hwnd) {
            return true;
        }
        
        // If not, try to find the window again
        if self.use_exact_match {
            debug!("Window handle no longer valid, attempting to find '{}' again with exact match", 
                self.process_name);
            
            if let Some(new_hwnd) = super::window::get_window_by_exact_string(&self.process_name) {
                debug!("Found window again with new handle: {:?}", new_hwnd);
                self.hwnd = new_hwnd;
                return true;
            }
        } else {
            debug!("Window handle no longer valid, attempting to find '{}' again with substring match", 
                self.process_name);
            
            if let Some(new_hwnd) = super::window::get_window_by_string(&self.process_name) {
                debug!("Found window again with new handle: {:?}", new_hwnd);
                self.hwnd = new_hwnd;
                return true;
            }
        }
        
        debug!("Failed to find window '{}'", self.process_name);
        false
    }
}

#[derive(Debug)]
enum FrameError {
    SendError(SendError<SendableSample>),
    WindowsError(WindowsError),
    ChannelClosed,
    TexturePoolError,
}

// Keep your existing impls unchanged
impl From<SendError<SendableSample>> for FrameError {
    fn from(err: SendError<SendableSample>) -> Self {
        FrameError::SendError(err)
    }
}

impl From<WindowsError> for FrameError {
    fn from(err: WindowsError) -> Self {
        FrameError::WindowsError(err)
    }
}

pub unsafe fn get_frames(
    send: Sender<SendableSample>,
    recording: Arc<AtomicBool>,
    hwnd: HWND,
    process_name: &str,
    fps_num: u32,
    fps_den: u32,
    input_width: u32,
    input_height: u32,
    started: Arc<Barrier>,
    device: Arc<ID3D11Device>,
    context_mutex: Arc<Mutex<ID3D11DeviceContext>>,
    use_exact_match: bool,
) -> Result<()> {
    info!("Starting frame collection for window: '{}'", get_window_title(hwnd));
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);

    // Create window tracker to handle focus and window validity
    let mut window_tracker = WindowTracker::new_with_exact_match(hwnd, process_name, use_exact_match);

    let frame_duration = Duration::from_nanos(1_000_000_000 * fps_den as u64 / fps_num as u64);
    let mut next_frame_time = Instant::now();
    let mut frame_count = 0;
    let mut accumulated_delay = Duration::ZERO;
    let mut num_duped = 0;

    // Create staging texture once and reuse
    let staging_texture = create_staging_texture(&device, input_width, input_height)?;
    let (blank_texture, _blank_resource) = create_blank_dxgi_texture(&device, input_width, input_height)?;

    // Initialize texture pool for reusable textures (for Media Foundation samples)
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Direct3D11::*;

    // Create a pool with capacity of 10 textures - adjust based on expected frame rate and processing time
    let texture_pool = TexturePool::new(
        device.clone(),
        10, // Capacity
        input_width,
        input_height,
        DXGI_FORMAT_B8G8R8A8_UNORM,
        D3D11_USAGE_DEFAULT.0.try_into().unwrap(),
        D3D11_BIND_SHADER_RESOURCE.0.try_into().unwrap(),
        0, // CPU access flags
        0, // Misc flags
    )?;
    let texture_pool = Arc::new(texture_pool);
    
    // Create a pool for IMFSample objects that are bound to the textures
    let sample_pool = SamplePool::new(fps_num, 10);
    let sample_pool = Arc::new(sample_pool);

    // Signal that we're ready
    started.wait();

    // Initialize duplication
    let mut duplication_result = setup_dxgi_duplication(&device);
    
    // Main recording loop
    while recording.load(Ordering::Relaxed) {
        // Periodically check if window is still valid
        if !window_tracker.ensure_valid_window() {
            // Window is no longer valid, try to find it again
            warn!("Window no longer valid, attempting to find '{}'", process_name);
            if let Some(new_hwnd) = if use_exact_match {
                super::window::get_window_by_exact_string(process_name)
            } else {
                super::window::get_window_by_string(process_name)
            } {
                info!("Found window '{}' again, continuing recording", process_name);
                window_tracker = WindowTracker::new_with_exact_match(new_hwnd, process_name, use_exact_match);
            } else {
                // Can't find window, wait and retry
                warn!("Window '{}' not found, will retry", process_name);
                spin_sleep::sleep(Duration::from_millis(1));
                continue;
            }
        }
        
        let duplication = duplication_result.as_ref().unwrap();
        
        match process_frame(
            duplication,
            &context_mutex,
            &staging_texture,
            &blank_texture,
            &mut window_tracker,
            fps_num,
            &send,
            frame_count,
            &mut next_frame_time,
            frame_duration,
            &mut accumulated_delay,
            &mut num_duped,
            &texture_pool,
            &sample_pool,
        ) {
            Ok(_) => {
                frame_count += 1;
                //trace!("Collected frame {}", frame_count);
            }
            Err(e) => match e {
                FrameError::SendError(_) | FrameError::ChannelClosed => {
                    warn!("Channel closed or receiver disconnected, stopping frame collection");
                    break;
                }
                FrameError::WindowsError(e) => {
                    if e.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAIT_TIMEOUT {
                        continue;
                    }
                    
                    // Handle "keyed mutex abandoned" and access lost errors
                    if e.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_ACCESS_LOST {
                        warn!("DXGI access lost (possibly keyed mutex abandoned), recreating duplication interface");
                        // Mark the duplication interface as invalid
                        duplication_result = Err(e);
                        continue;
                    }
                    
                    // For other errors, return as before
                    return Err(e);
                }
                FrameError::TexturePoolError => {
                    // Handle texture pool error - log and continue
                    warn!("Texture pool error occurred, trying to continue");
                    continue;
                }
            },
        }
    }

    info!(
        "Frame collection finished. Number of duped frames: {}",
        num_duped
    );
    Ok(())
}

unsafe fn process_frame(
    duplication: &IDXGIOutputDuplication,
    context_mutex: &Arc<Mutex<ID3D11DeviceContext>>,
    staging_texture: &ID3D11Texture2D,
    blank_texture: &ID3D11Texture2D,
    window_tracker: &mut WindowTracker,
    fps_num: u32,
    send: &Sender<SendableSample>,
    frame_count: u64,
    next_frame_time: &mut Instant,
    frame_duration: Duration,
    accumulated_delay: &mut Duration,
    num_duped: &mut u64,
    texture_pool: &Arc<TexturePool>,
    sample_pool: &Arc<SamplePool>,
) -> std::result::Result<(), FrameError> {
    let mut resource: Option<IDXGIResource> = None;
    let mut info = windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_FRAME_INFO::default();
    
    // Check if window is focused using our tracker
    let is_window_focused = window_tracker.is_focused();
    
    // Only show content when window is focused
    let should_show_content = is_window_focused;
    
    // Log state changes for debugging
    static mut LAST_FOCUS_STATE: Option<bool> = None;
    let focus_changed = unsafe { LAST_FOCUS_STATE != Some(is_window_focused) };
    
    if focus_changed {
        if is_window_focused {
            info!("Window '{}' is now in focus - displaying window content", window_tracker.process_name);
            if !window_tracker.ever_focused {
                info!("Window focused for the first time - recording will now show content");
            }
        } else {
            info!("Window '{}' lost focus - displaying black screen", window_tracker.process_name);
        }
        unsafe { LAST_FOCUS_STATE = Some(is_window_focused); }
    }
    
    info!("Attempting to acquire next frame");
    let acquire_result = duplication.AcquireNextFrame(16, &mut info, &mut resource);
    if let Err(ref e) = acquire_result {
        error!("AcquireNextFrame failed with error: {:?}", e);
        return Err(FrameError::WindowsError(e.clone()));
    }
    info!("Successfully acquired frame");
    
    info!("Attempting to lock context mutex");
    let context_lock_result = context_mutex.lock();
    if let Err(ref e) = context_lock_result {
        error!("Failed to lock context mutex: {:?}", e);
        // We should return an error here, but you'll need to add a new FrameError variant
        // For now, let's just panic with the error message
        panic!("Failed to lock context mutex: {:?}", e);
    }
    let context = context_lock_result.unwrap();
    info!("Successfully locked context mutex");
    
    if let Some(resource) = resource {
        info!("Processing acquired resource");
        
        // Acquire a texture from the pool
        info!("Attempting to acquire texture from pool");
        let pooled_texture_result = texture_pool.acquire();
        if let Err(ref e) = pooled_texture_result {
            error!("Failed to acquire texture from pool: {:?}", e);
            // Release the frame before returning error
            let release_result = duplication.ReleaseFrame();
            if let Err(ref e) = release_result {
                error!("Additionally failed to release frame: {:?}", e);
            }
            return Err(FrameError::TexturePoolError);
        }
        let pooled_texture = pooled_texture_result.map_err(|e| {
            error!("Texture pool error: {:?}", e);
            FrameError::TexturePoolError
        })?;
        info!("Successfully acquired texture from pool");
        
        // Get the source texture from the resource
        info!("Casting resource to texture");
        let source_texture_result = resource.cast::<ID3D11Texture2D>();
        if let Err(ref e) = source_texture_result {
            error!("Failed to cast resource to texture: {:?}", e);
            return Err(FrameError::WindowsError(e.clone()));
        }
        let source_texture: ID3D11Texture2D = source_texture_result?;
        info!("Successfully cast resource to texture");
        
        if should_show_content {
            info!("Copying source texture to pooled texture (window in focus)");
            context.CopyResource(&pooled_texture, &source_texture);
            
            info!("Copying pooled texture to staging texture");
            context.CopyResource(staging_texture, &pooled_texture);
        } else {
            info!("Copying blank texture to staging texture (window not in focus)");
            context.CopyResource(staging_texture, blank_texture);
        }
        
        // Release the original texture and frame
        info!("Dropping source texture");
        drop(source_texture);
        
        info!("Attempting to release frame");
        let release_result = duplication.ReleaseFrame();
        if let Err(ref e) = release_result {
            error!("ReleaseFrame failed with error: {:?}", e);
            return Err(FrameError::WindowsError(e.clone()));
        }
        info!("Successfully released frame");
        
        // Return the pooled texture to the pool when done
        info!("Releasing pooled texture back to pool");
        texture_pool.release(pooled_texture);
        info!("Successfully released pooled texture");
    } else {
        info!("No resource acquired, using blank screen");
        context.CopyResource(staging_texture, blank_texture);
    }
    
    info!("Dropping context lock");
    drop(context);
    info!("Context lock dropped");
    
    // Handle frame timing and duplication
    info!("Handling frame timing");
    while *accumulated_delay >= frame_duration {
        info!("Duping a frame to catch up");
        let send_result = send_frame(staging_texture, frame_count, send, sample_pool);
        if let Err(ref e) = send_result {
            error!("Failed to send duplicate frame: {:?}", e);
            return Err(FrameError::ChannelClosed);
        }
        *next_frame_time += frame_duration;
        *accumulated_delay -= frame_duration;
        *num_duped += 1;
    }
    
    info!("Sending normal frame");
    let send_result = send_frame(staging_texture, frame_count, send, sample_pool);
    if let Err(ref e) = send_result {
        error!("Failed to send normal frame: {:?}", e);
        return Err(FrameError::ChannelClosed);
    }
    *next_frame_time += frame_duration;
    
    let current_time = Instant::now();
    info!("Handling final frame timing adjustments");
    handle_frame_timing(current_time, *next_frame_time, accumulated_delay);
    info!("Frame processing complete");
    
    Ok(())
}

unsafe fn send_frame(
    texture: &ID3D11Texture2D,
    frame_count: u64,
    send: &Sender<SendableSample>,
    sample_pool: &Arc<SamplePool>,
) -> Result<()> {
    info!("Acquiring sample from pool for frame {}", frame_count);
    let sample_result = sample_pool.acquire_for_texture(texture);
    if let Err(ref e) = sample_result {
        error!("Failed to acquire sample from pool: {:?}", e);
        return Err(Error::from_win32());
    }
    let samp = sample_result?;
    info!("Successfully acquired sample");
    
    info!("Setting sample time for frame {}", frame_count);
    let set_time_result = sample_pool.set_sample_time(&samp, frame_count);
    if let Err(ref e) = set_time_result {
        error!("Failed to set sample time: {:?}", e);
        return Err(Error::from_win32());
    }
    info!("Successfully set sample time");
    
    info!("Creating sendable sample");
    let sendable = SendableSample::new_pooled(samp, texture, sample_pool.clone());
    
    info!("Sending frame {} to channel", frame_count);
    match send.send(sendable) {
        Ok(_) => {
            info!("Successfully sent frame {}", frame_count);
            Ok(())
        },
        Err(e) => {
            error!("Failed to send frame {} (channel closed): {:?}", frame_count, e);
            Err(Error::from_win32())
        }
    }
}

fn handle_frame_timing(
    current_time: Instant,
    next_frame_time: Instant,
    accumulated_delay: &mut Duration,
) {
    if current_time > next_frame_time {
        let overrun = current_time.duration_since(next_frame_time);
        *accumulated_delay += overrun;
    } else {
        let sleep_time = next_frame_time.duration_since(current_time);
        spin_sleep::sleep(sleep_time);
    }
}