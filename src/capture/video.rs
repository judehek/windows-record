use log::{debug, info, trace, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SendError, Sender};
use std::sync::Arc;
use std::sync::{Barrier, Mutex};
use std::time::{Duration, Instant};
use windows::core::Error;
use windows::core::{ComInterface, Error as WindowsError, Result};
use windows::Win32::Foundation::{BOOL, HWND};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::{IDXGIOutputDuplication, IDXGIResource};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use super::dxgi::{create_blank_dxgi_texture, setup_dxgi_duplication};
use super::window::{is_window_valid, get_window_title, get_window_rect};
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
    /// Window position (x, y)
    position: Option<(i32, i32)>,
    /// Window size (width, height)
    size: Option<(u32, u32)>,
    /// Last time we checked window rect
    last_rect_check: Instant,
    /// Time between window rect checks
    rect_check_interval: Duration,
}

impl WindowTracker {
    /// Create a new window tracker
    fn new(hwnd: HWND, process_name: &str) -> Self {
        Self::new_with_exact_match(hwnd, process_name, false)
    }
    
    /// Create a new window tracker with option for exact matching
    fn new_with_exact_match(hwnd: HWND, process_name: &str, use_exact_match: bool) -> Self {
        // Try to get initial window rect
        let (position, size) = if let Some((x, y, width, height)) = get_window_rect(hwnd) {
            (Some((x, y)), Some((width, height)))
        } else {
            (None, None)
        };
        
        Self {
            hwnd,
            process_name: process_name.to_string(),
            last_check: Instant::now(),
            check_interval: Duration::from_secs(2), // Check every 2 seconds
            ever_focused: false,
            use_exact_match,
            position,
            size,
            last_rect_check: Instant::now(),
            rect_check_interval: Duration::from_millis(500), // Check window rect every 500ms
        }
    }
    
    /// Update the window position and size
    fn update_window_rect(&mut self) {
        let now = Instant::now();
        
        // Don't check too frequently
        if now.duration_since(self.last_rect_check) < self.rect_check_interval {
            return;
        }
        
        self.last_rect_check = now;
        
        if let Some((x, y, width, height)) = get_window_rect(self.hwnd) {
            self.position = Some((x, y));
            self.size = Some((width, height));
        }
    }
    
    /// Get the current window position
    fn get_position(&self) -> Option<(i32, i32)> {
        self.position
    }
    
    /// Get the current window size
    fn get_size(&self) -> Option<(u32, u32)> {
        self.size
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
    window_info_sender: Sender<(Option<(i32, i32)>, Option<(u32, u32)>)>,
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
    // Create pool textures with RENDER_TARGET flag for cursor overlay
    let texture_pool = TexturePool::new(
        device.clone(),
        10, // Capacity
        input_width,
        input_height,
        DXGI_FORMAT_B8G8R8A8_UNORM,
        D3D11_USAGE_DEFAULT.0.try_into().unwrap(),
        (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0.try_into().unwrap(),
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
        
        // Update window position and size and send the info
        window_tracker.update_window_rect();
        let position = window_tracker.get_position();
        let size = window_tracker.get_size();
        
        // Only send window info if we have both position and size
        if position.is_some() && size.is_some() {
            if let Err(e) = window_info_sender.send((position, size)) {
                warn!("Failed to send window position/size: {:?}", e);
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

// Draw cursor using GDI
unsafe fn draw_cursor_gdi(
    texture: &ID3D11Texture2D,
) -> Result<()> {
    use windows::Win32::Graphics::Dxgi::IDXGISurface1;
    use windows::Win32::Graphics::Gdi::DeleteObject;
    use windows::Win32::UI::WindowsAndMessaging::{CURSORINFO, CURSOR_SHOWING, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo};
    use windows::Win32::Foundation::{BOOL, POINT};
    use std::mem::size_of;
    use log::{debug, error, trace};

    // Get the surface interface from the texture
    let surface: IDXGISurface1 = texture.cast()?;
    
    // Prepare cursor info structure
    let mut cursor_info = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    
    // Get the current cursor info
    let cursor_present = GetCursorInfo(&mut cursor_info as *mut CURSORINFO);
    
    // If cursor is not showing or we couldn't get info, just return
    if !cursor_present.as_bool() || (cursor_info.flags.0 & CURSOR_SHOWING.0 != CURSOR_SHOWING.0) {
        debug!("Cursor is not visible, skipping drawing");
        return Ok(());
    }
    
    // Get cursor hotspot
    let mut icon_info = Default::default();
    let result = GetIconInfo(cursor_info.hCursor, &mut icon_info);
    
    if !result.as_bool() {
        error!("Failed to get icon info");
        return Err(Error::from_win32());
    }
    
    let hotspot_x = icon_info.xHotspot as i32;
    let hotspot_y = icon_info.yHotspot as i32;
    
    // Clean up icon info resources
    if !icon_info.hbmMask.is_invalid() {
        DeleteObject(icon_info.hbmMask);
    }
    if !icon_info.hbmColor.is_invalid() {
        DeleteObject(icon_info.hbmColor);
    }
    
    // Get DC from surface
    let hdc = surface.GetDC(BOOL::from(false))?;
    
    // Draw cursor using GDI
    let result = DrawIconEx(
        hdc,
        cursor_info.ptScreenPos.x - hotspot_x,
        cursor_info.ptScreenPos.y - hotspot_y,
        cursor_info.hCursor,
        0, 0, 0, None, DI_NORMAL,
    );
    
    if !result.as_bool() {
        error!("Failed to draw cursor with GDI");
    }
    
    // Release DC
    surface.ReleaseDC(None)?;
    
    Ok(())
}

// Now let's update the process_frame function to use our new cursor drawing approach
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
    
    // Process the frame with context lock
    {
        let context = context_mutex.lock().unwrap();
        
        if let Some(resource) = resource.as_ref() {
            // Acquire a texture from the pool
            let pooled_texture = texture_pool.acquire().map_err(|e| {
                log::error!("Failed to acquire texture from pool: {:?}", e);
                FrameError::TexturePoolError
            })?;
            
            // Get the source texture from the resource
            let source_texture: ID3D11Texture2D = resource.cast()?;
            
            if should_show_content {
                // Copy content from source to pooled texture
                context.CopyResource(&pooled_texture, &source_texture);
                
                // Copy from pooled to staging texture
                context.CopyResource(staging_texture, &pooled_texture);
            } else {
                // Window not in focus, just use blank screen
                context.CopyResource(staging_texture, blank_texture);
            }
            
            // Return the pooled texture to the pool
            texture_pool.release(pooled_texture);
        }
        
        // Context lock is automatically dropped at the end of this scope
    }
    
    // Draw cursor if needed (outside the context lock)
    if capture_cursor && should_show_content && resource.is_some() {
        // Use our GDI-based cursor drawing approach
        if let Err(e) = draw_cursor_gdi(staging_texture) {
            debug!("Failed to draw cursor: {:?}", e);
        }
    }
    
    // Release the frame
    if let Some(resource) = resource {
        let source_texture: ID3D11Texture2D = resource.cast()?;
        drop(source_texture);
        duplication.ReleaseFrame()?;
    }
    
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