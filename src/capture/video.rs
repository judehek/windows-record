use log::{debug, info, trace, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SendError, Sender};
use std::sync::Arc;
use std::sync::{Barrier, Mutex};
use std::time::{Duration, Instant};
use windows::core::Error;
use windows::core::{ComInterface, Error as WindowsError, Result};
use windows::Win32::Foundation::{BOOL, HWND, TRUE};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::{IDXGIOutputDuplication, IDXGIResource, IDXGISurface};
use windows::Win32::Media::MediaFoundation::MFCreateDXGISurfaceBuffer;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use super::dxgi::setup_dxgi_duplication;
use super::window::{get_window_rect, get_window_title, is_window_valid};
use crate::types::{SamplePool, SendableSample, TexturePool};

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
            info!(
                "WindowTracker: Initial window rect for '{}' - Position: [{}, {}], Size: {}x{}",
                process_name, x, y, width, height
            );
            (Some((x, y)), Some((width, height)))
        } else {
            info!(
                "WindowTracker: Failed to get initial window rect for '{}'",
                process_name
            );
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
            // Check if values have changed before logging
            let position_changed = self.position != Some((x, y));
            let size_changed = self.size != Some((width, height));

            if position_changed || size_changed {
                info!(
                    "WindowTracker: Window '{}' updated - Position: [{}, {}], Size: {}x{}",
                    self.process_name, x, y, width, height
                );
            }

            self.position = Some((x, y));
            self.size = Some((width, height));
        } else {
            debug!(
                "WindowTracker: Failed to get window rect for '{}'",
                self.process_name
            );
        }
    }

    /// Get the current window position
    fn get_position(&self) -> Option<(i32, i32)> {
        trace!(
            "WindowTracker: Getting position for '{}': {:?}",
            self.process_name,
            self.position
        );
        self.position
    }

    /// Get the current window size
    fn get_size(&self) -> Option<(u32, u32)> {
        trace!(
            "WindowTracker: Getting size for '{}': {:?}",
            self.process_name,
            self.size
        );
        self.size
    }

    /// Check if the window is currently in focus
    fn is_focused(&mut self) -> bool {
        let foreground_window = unsafe { GetForegroundWindow() };
        let is_target_window = foreground_window == self.hwnd;

        log::debug!(
            "Foreground window: {:?}, Target window: {:?}, Is target in focus: {}",
            foreground_window,
            self.hwnd,
            is_target_window
        );

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
            debug!(
                "Window handle no longer valid, attempting to find '{}' again with exact match",
                self.process_name
            );

            if let Some(new_hwnd) = super::window::get_window_by_exact_string(&self.process_name) {
                debug!("Found window again with new handle: {:?}", new_hwnd);
                self.hwnd = new_hwnd;
                return true;
            }
        } else {
            debug!(
                "Window handle no longer valid, attempting to find '{}' again with substring match",
                self.process_name
            );

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
    info!(
        "Starting frame collection for window: '{}'",
        get_window_title(hwnd)
    );
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);

    // Create window tracker to handle focus and window validity
    let mut window_tracker =
        WindowTracker::new_with_exact_match(hwnd, process_name, use_exact_match);

    let frame_duration = Duration::from_nanos(1_000_000_000 * fps_den as u64 / fps_num as u64);
    let mut next_frame_time = Instant::now();
    let mut frame_count = 0;
    let mut accumulated_delay = Duration::ZERO;
    let mut num_duped = 0;

    // Initialize texture pool for reusable textures
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;

    // Create a pool with capacity of 1 acquisition textures - adjust based on expected frame rate and processing time
    let texture_pool = TexturePool::new(
        device.clone(),
        5, // Acquisition capacity
        input_width,
        input_height,
        DXGI_FORMAT_B8G8R8A8_UNORM,
    )?;
    let texture_pool = Arc::new(texture_pool);

    // Create a pool for IMFSample objects that are bound to the textures
    let sample_pool = SamplePool::new(fps_num, 10);
    let sample_pool = Arc::new(sample_pool);

    // Signal that we're ready
    started.wait();

    // Directly use duplication for the window using our device (which was created for this window's adapter)
    let mut duplication_result = unsafe { super::dxgi::setup_dxgi_duplication_for_window(&device, hwnd) };

    // Main recording loop
    while recording.load(Ordering::Relaxed) {
        // Periodically check if window is still valid
        if !window_tracker.ensure_valid_window() {
            // Window is no longer valid, try to find it again
            warn!(
                "Window no longer valid, attempting to find '{}'",
                process_name
            );
            if let Some(new_hwnd) = if use_exact_match {
                super::window::get_window_by_exact_string(process_name)
            } else {
                super::window::get_window_by_string(process_name)
            } {
                info!(
                    "Found window '{}' again, continuing recording",
                    process_name
                );
                window_tracker =
                    WindowTracker::new_with_exact_match(new_hwnd, process_name, use_exact_match);
                
                // Recreate the duplication for the new window
                info!("Recreating DXGI duplication for new window");
                duplication_result = unsafe { super::dxgi::setup_dxgi_duplication_for_window(&device, new_hwnd) };
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
            //info!("Capture: Sending window info for '{}' - Position: {:?}, Size: {:?}",
            //     process_name, position, size);
            if let Err(e) = window_info_sender.send((position, size)) {
                warn!("Failed to send window position/size: {:?}", e);
            }
        } else {
            if position.is_none() {
                debug!("Capture: Window position is None for '{}'", process_name);
            }
            if size.is_none() {
                debug!("Capture: Window size is None for '{}'", process_name);
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
unsafe fn draw_cursor_gdi(texture: &ID3D11Texture2D) -> Result<()> {
    use log::{debug, error, trace, warn};
    use std::mem::size_of;
    use windows::Win32::Foundation::{BOOL, POINT};
    use windows::Win32::Graphics::Direct3D11::{
        ID3D11Resource, ID3D11Texture2D, D3D11_RESOURCE_MISC_GDI_COMPATIBLE,
    };
    use windows::Win32::Graphics::Dxgi::IDXGISurface1;
    use windows::Win32::Graphics::Gdi::DeleteObject;
    use windows::Win32::UI::WindowsAndMessaging::{
        DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL,
    };

    // First, check if the texture has the GDI_COMPATIBLE flag
    let resource: ID3D11Resource = texture.cast()?;
    let mut desc = Default::default();
    texture.GetDesc(&mut desc);

    if desc.MiscFlags.0 & D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0
        != D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0
    {
        warn!("Texture does not have GDI_COMPATIBLE flag, cursor drawing will fail");
        return Err(Error::from_win32().into());
    }

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
        0,
        0,
        0,
        None,
        DI_NORMAL,
    );

    if !result.as_bool() {
        error!("Failed to draw cursor with GDI");
    }

    // Release DC
    surface.ReleaseDC(None)?;

    Ok(())
}

unsafe fn process_frame(
    duplication: &IDXGIOutputDuplication,
    context_mutex: &Arc<Mutex<ID3D11DeviceContext>>,
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
            info!(
                "Window '{}' is now in focus - displaying window content",
                window_tracker.process_name
            );
            if !window_tracker.ever_focused {
                info!("Window focused for the first time - recording will now show content");
            }
        } else {
            info!(
                "Window '{}' lost focus - displaying black screen",
                window_tracker.process_name
            );
        }
        unsafe {
            LAST_FOCUS_STATE = Some(is_window_focused);
        }
    }

    duplication.AcquireNextFrame(16, &mut info, &mut resource)?;

    // We'll track the texture that holds the final frame
    let mut final_texture: Option<ID3D11Texture2D> = None;

    // Process the frame with context lock
    {
        let mut context = context_mutex.lock().unwrap();

        if let Some(resource) = resource.as_ref() {
            if should_show_content {
                // Get the source texture from the resource
                let source_texture: ID3D11Texture2D = resource.cast()?;

                // Acquire a texture from the pool for this frame
                let pooled_texture = texture_pool.acquire_acquisition_texture().map_err(|e| {
                    log::error!("Failed to acquire texture from pool: {:?}", e);
                    FrameError::TexturePoolError
                })?;

                // Copy content from source to pooled texture
                context.CopyResource(&pooled_texture, &source_texture);

                // For cursor drawing, we need to release the context lock
                if capture_cursor {
                    // Drop the context lock before GDI operations to avoid deadlocks
                    drop(context);

                    // Draw cursor on the pooled texture (which has GDI_COMPATIBLE flag)
                    if let Err(e) = draw_cursor_gdi(&pooled_texture) {
                        debug!("Failed to draw cursor: {:?}", e);
                    }

                    // Re-acquire the context
                    context = context_mutex.lock().unwrap();
                }

                // Remember this texture for later use in send_frame
                final_texture = Some(pooled_texture);
            } else {
                // Window not in focus, use blank screen
                let blank_texture = texture_pool.get_blank_texture().map_err(|e| {
                    log::error!("Failed to get blank texture from pool: {:?}", e);
                    FrameError::TexturePoolError
                })?;

                // Remember the blank texture for later use in send_frame
                final_texture = Some(blank_texture.clone());
            }
        }

        // Context lock is automatically dropped at the end of this scope
    }

    // Release the frame
    if let Some(resource) = resource {
        let source_texture: ID3D11Texture2D = resource.cast()?;
        drop(source_texture);
        duplication.ReleaseFrame()?;
    }

    // Get the texture that contains our final frame
    if let Some(texture) = final_texture {
        // Handle frame timing and duplication
        while *accumulated_delay >= frame_duration {
            debug!("Duping a frame to catch up");
            send_frame(&texture, frame_count, send, sample_pool)
                .map_err(|_| FrameError::ChannelClosed)?;
            *next_frame_time += frame_duration;
            *accumulated_delay -= frame_duration;
            *num_duped += 1;
        }

        // Send the normal frame
        send_frame(&texture, frame_count, send, sample_pool)
            .map_err(|_| FrameError::ChannelClosed)?;

        // If this was an acquisition texture (not the blank texture), return it to the pool
        if should_show_content {
            texture_pool.release_acquisition_texture(texture);
        }
    } else {
        warn!("No texture was prepared for this frame!");
    }
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
    // Get a sample from the pool
    let sample = sample_pool.acquire_sample()?;

    // Get the surface interface from the texture
    let surface: IDXGISurface = texture.cast()?;

    // Create a DXGI buffer from the surface
    let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &surface, 0, TRUE)?;

    // Clear any previous buffers and add the new one
    sample.RemoveAllBuffers()?;
    sample.AddBuffer(&buffer)?;

    // Release the surface interface (buffer still holds reference to underlying resource)
    drop(surface);

    // Set the sample time and duration
    sample_pool.set_sample_time(&sample, frame_count)?;
    sample.SetSampleDuration(10_000_000 / sample_pool.fps_num as i64)?;

    // Create a pooled SendableSample that will return the sample to the pool when dropped
    let sendable = SendableSample::new_pooled(sample, sample_pool.clone());

    // Send the sample (it will be returned to pool via Drop if sending fails)
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
