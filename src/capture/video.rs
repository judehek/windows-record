use log::{debug, info, trace, warn};
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

use super::dxgi::{create_blank_dxgi_texture, setup_dxgi_duplication, process_cursor, CursorInfo};
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
    capture_cursor: bool,
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
        
        // Check if we need to recreate the duplication interface
        if duplication_result.is_err() {
            info!("Recreating DXGI duplication interface after previous failure");
            duplication_result = setup_dxgi_duplication(&device);
            if let Err(e) = &duplication_result {
                warn!("Failed to recreate DXGI duplication interface: {:?}", e);
                // Wait a bit before trying again to avoid spinning too fast
                spin_sleep::sleep(Duration::from_millis(100));
                continue;
            }
            info!("DXGI duplication interface recreated successfully");
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
            capture_cursor,
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
                        // Mark the duplication interface as invalid and recreate it on the next loop iteration
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

/// Render cursor onto the texture using D3D11 context
unsafe fn draw_cursor(
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
    cursor_info: &CursorInfo,
) -> Result<()> {
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    
    // Early return if cursor is not visible or no shape is available
    if !cursor_info.visible || cursor_info.shape.is_none() {
        return Ok(());
    }
    
    // Get texture description to know dimensions
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    texture.GetDesc(&mut desc);
    
    let (cursor_x, cursor_y) = cursor_info.position;
    let (hotspot_x, hotspot_y) = cursor_info.hotspot;
    
    // Adjust cursor position based on hotspot
    let cursor_x = cursor_x - hotspot_x as i32;
    let cursor_y = cursor_y - hotspot_y as i32;
    
    // Draw cursor based on type
    if let Some(shape) = &cursor_info.shape {
        match shape {
            super::dxgi::CursorShape::Color(data, width, height) |
            super::dxgi::CursorShape::MaskedColor(data, width, height) => {
                // Create a texture for the cursor
                let cursor_desc = D3D11_TEXTURE2D_DESC {
                    Width: *width,
                    Height: *height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE,
                    CPUAccessFlags: D3D11_CPU_ACCESS_FLAG(0),
                    MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
                };
                
                // Create initial data
                let stride = *width * 4; // BGRA format = 4 bytes per pixel
                let initial_data = D3D11_SUBRESOURCE_DATA {
                    pSysMem: data.as_ptr() as *const _,
                    SysMemPitch: stride,
                    SysMemSlicePitch: 0,
                };
                
                // Get device from context
                let device = context.GetDevice()?;
                
                // Create cursor texture
                let mut cursor_texture = None;
                device.CreateTexture2D(&cursor_desc, Some(&initial_data), Some(&mut cursor_texture))?;
                let cursor_texture = cursor_texture.unwrap();
                
                // Calculate destination coordinates (clamping to screen edges)
                let dest_x = cursor_x.max(0).min(desc.Width as i32);
                let dest_y = cursor_y.max(0).min(desc.Height as i32);
                
                // Calculate copy region (taking screen boundaries into account)
                let copy_width = (*width).min(desc.Width - dest_x as u32);
                let copy_height = (*height).min(desc.Height - dest_y as u32);
                
                if copy_width > 0 && copy_height > 0 {
                    // Create box for source and destination regions
                    let src_box = D3D11_BOX {
                        left: 0,
                        top: 0,
                        front: 0,
                        right: copy_width,
                        bottom: copy_height,
                        back: 1,
                    };
                    
                    // Copy cursor texture onto the frame texture
                    context.CopySubresourceRegion(
                        texture,
                        0,
                        dest_x as u32,
                        dest_y as u32,
                        0,
                        &cursor_texture,
                        0,
                        Some(&src_box),
                    );
                }
            },
            super::dxgi::CursorShape::Monochrome(data, width, height) => {
                // Monochrome cursors would require more complex processing
                // to convert them to BGRA format for rendering
                trace!("Monochrome cursor rendering not implemented");
            }
        }
    }
    
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
    capture_cursor: bool,
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
    
    duplication.AcquireNextFrame(16, &mut info, &mut resource)?;
    
    // Process cursor information if enabled
    let cursor_info = if capture_cursor {
        match process_cursor(duplication, &info) {
            Ok(info) => Some(info),
            Err(e) => {
                debug!("Failed to process cursor: {:?}", e);
                None
            }
        }
    } else {
        None
    };
    
    let context = context_mutex.lock().unwrap();
    
    if let Some(resource) = resource {
        // Acquire a texture from the pool rather than creating a new one every time
        let pooled_texture = texture_pool.acquire().map_err(|e| {
            log::error!("Failed to acquire texture from pool: {:?}", e);
            // Convert to WindowsError first if needed, or just use TexturePoolError variant
            FrameError::TexturePoolError
        })?;
        
        // Get the source texture from the resource
        let source_texture: ID3D11Texture2D = resource.cast()?;
        
        if should_show_content {
            // Copy content from source to pooled texture
            context.CopyResource(&pooled_texture, &source_texture);
            
            // Render cursor on top of content if available
            if let Some(cursor_info) = &cursor_info {
                if cursor_info.visible {
                    // Draw cursor on the pooled texture
                    if let Err(e) = draw_cursor(&context, &pooled_texture, cursor_info) {
                        debug!("Failed to draw cursor: {:?}", e);
                    }
                }
            }
            
            // Then copy from pooled to staging texture
            context.CopyResource(staging_texture, &pooled_texture);
        } else {
            // Window not in focus, just use blank screen
            context.CopyResource(staging_texture, blank_texture);
        }
        
        // Release the original texture and frame
        drop(source_texture);
        duplication.ReleaseFrame()?;
        
        // Return the pooled texture to the pool when done
        texture_pool.release(pooled_texture);
    }
    
    drop(context);
    
    // Handle frame timing and duplication
    while *accumulated_delay >= frame_duration {
        debug!("Duping a frame to catch up");
        send_frame(staging_texture, frame_count, send, sample_pool)
            .map_err(|_| FrameError::ChannelClosed)?;
        *next_frame_time += frame_duration;
        *accumulated_delay -= frame_duration;
        *num_duped += 1;
    }
    
    send_frame(staging_texture, frame_count, send, sample_pool)
        .map_err(|_| FrameError::ChannelClosed)?;
    *next_frame_time += frame_duration;
    
    let current_time = Instant::now();
    handle_frame_timing(current_time, *next_frame_time, accumulated_delay);
    
    Ok(())
}

unsafe fn send_frame(
    texture: &ID3D11Texture2D,
    frame_count: u64,
    send: &Sender<SendableSample>,
    sample_pool: &Arc<SamplePool>,
) -> Result<()> {
    // Get a sample from the pool instead of creating a new one each time
    let samp = sample_pool.acquire_for_texture(texture)?;
    
    // Set the sample time based on frame count
    sample_pool.set_sample_time(&samp, frame_count)?;
    
    // Create a pooled SendableSample that will return the sample to the pool when dropped
    let sendable = SendableSample::new_pooled(samp, texture, sample_pool.clone());
    
    // Send the sample and return to pool if fails
    match send.send(sendable) {
        Ok(_) => Ok(()),
        Err(e) => {
            trace!("Failed to send frame (channel closed), sample will be returned to pool");
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