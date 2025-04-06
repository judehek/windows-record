use log::{debug, error, info, trace, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SendError, Sender};
use std::sync::Arc;
use std::sync::{Barrier, Mutex};
use std::time::{Duration, Instant};
use windows::core::Error;
use windows::core::{ComInterface, Error as WindowsError, Result};
use windows::Win32::Foundation::{BOOL, HWND, TRUE};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutputDuplication, IDXGIResource, IDXGISurface, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO,
};
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
        10, // Acquisition capacity
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

    // The device was already created for the correct adapter in inner.rs, so just set up duplication
    // We can use the simpler setup since the device already knows which adapter to use
    let mut duplication_result = unsafe { super::dxgi::setup_dxgi_duplication(&device) };

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
                // Since we're using the same device (which was created for the correct adapter),
                // we can use the simpler duplication setup
                info!("Recreating DXGI duplication");
                duplication_result = unsafe { super::dxgi::setup_dxgi_duplication(&device) };
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
    // 1. Check Focus & Log Focus Change
    let is_window_focused = window_tracker.is_focused();
    static mut LAST_FOCUS_STATE: Option<bool> = None; // Keep static focus tracking
    let focus_changed = unsafe { LAST_FOCUS_STATE != Some(is_window_focused) };
    if focus_changed {
        info!(
            "Window '{}' focus state changed: {}",
            window_tracker.process_name,
            if is_window_focused {
                "Focused"
            } else {
                "Unfocused"
            }
        );
        unsafe {
            LAST_FOCUS_STATE = Some(is_window_focused);
        }
    }

    // Variables to hold results through the steps
    let mut final_texture: Option<ID3D11Texture2D> = None;
    let mut needs_release_to_pool = false; // Track if texture came from acquisition pool
    let mut acquired_resource_holder: Option<IDXGIResource> = None; // Temp holder for resource
                                                                    // *** Track if AcquireNextFrame returned Ok(_) ***
    let mut frame_acquire_returned_ok = false;
    let mut frame_has_content = false; // Track if AcquireNextFrame returned a valid resource

    // --- Main Logic ---
    if is_window_focused {
        // 2. Acquire Frame Attempt
        trace!("Window focused, attempting to acquire frame.");
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let acquire_result =
            duplication.AcquireNextFrame(16, &mut info, &mut acquired_resource_holder);

        // *** Store Ok status directly ***
        frame_acquire_returned_ok = acquire_result.is_ok();

        match acquire_result {
            Ok(_) => {
                // frame_acquire_returned_ok is already true
                if acquired_resource_holder.is_some() {
                    trace!("Acquired new frame resource.");
                    frame_has_content = true;
                } else {
                    // This is the S_OK + timeout case (resource is None)
                    trace!("AcquireNextFrame returned S_OK but timed out (resource is None).");
                    frame_has_content = false;
                }
            }
            Err(e) => {
                // frame_acquire_returned_ok is already false
                let code = e.code();
                if code == DXGI_ERROR_ACCESS_LOST
                    || code == DXGI_ERROR_DEVICE_REMOVED
                    || code == DXGI_ERROR_DEVICE_RESET
                {
                    warn!("DXGI access/device lost during AcquireNextFrame ({:?}), signaling recreation needed.", code);
                    return Err(FrameError::WindowsError(e));
                } else if code == DXGI_ERROR_WAIT_TIMEOUT {
                    // Explicitly handle the DXGI_ERROR_WAIT_TIMEOUT *error* case
                    trace!("AcquireNextFrame returned error: DXGI_ERROR_WAIT_TIMEOUT.");
                    // frame_acquire_returned_ok remains false
                    frame_has_content = false;
                } else {
                    // Other unexpected errors
                    error!(
                        "Unexpected error during AcquireNextFrame: {:?}. Code: {:?}",
                        e,
                        e.code()
                    );
                    return Err(FrameError::WindowsError(e));
                }
            }
        }

        // 3. Copy Frame Resource (only if content exists)
        if frame_has_content {
            if let Some(ref acquired_resource) = acquired_resource_holder {
                // Lock context only for the copy operation
                let mut context_guard = context_mutex.lock().unwrap();
                match acquired_resource.cast::<ID3D11Texture2D>() {
                    Ok(source_texture) => {
                        match texture_pool.acquire_acquisition_texture() {
                            Ok(pooled_texture) => {
                                trace!("Copying acquired frame to pooled texture.");
                                context_guard.CopyResource(&pooled_texture, &source_texture);
                                final_texture = Some(pooled_texture);
                                needs_release_to_pool = true;
                            }
                            Err(pool_err) => {
                                error!("Failed to acquire texture from pool: {:?}", pool_err);
                                drop(context_guard);
                                drop(acquired_resource_holder);
                                // Only release if acquire *was* Ok
                                if frame_acquire_returned_ok {
                                    if let Err(rel_err) = duplication.ReleaseFrame() {
                                        warn!("Error releasing frame after texture pool failure: {:?}", rel_err);
                                        // If release fails here, maybe propagate *that* error instead?
                                        // return Err(rel_err.into()); // Or just continue and return pool error
                                    }
                                }
                                return Err(FrameError::TexturePoolError);
                            }
                        }
                    }
                    Err(cast_err) => {
                        error!(
                            "Failed to cast acquired resource to ID3D11Texture2D: {:?}",
                            cast_err
                        );
                        drop(context_guard);
                        drop(acquired_resource_holder);
                        if frame_acquire_returned_ok {
                            if let Err(rel_err) = duplication.ReleaseFrame() {
                                warn!("Error releasing frame after cast failure: {:?}", rel_err);
                            }
                        }
                        return Err(cast_err.into());
                    }
                }
                // Context lock drops here
            } else {
                error!("Inconsistent state: frame_has_content is true but acquired_resource_holder is None.");
                frame_has_content = false; // Correct state
            }
        }

        // Drop the temporary resource holder reference - content is now in final_texture (if successful)
        drop(acquired_resource_holder);

        // 4. Release DXGI Frame (*** MODIFIED CONDITION ***)
        // Only call ReleaseFrame if AcquireNextFrame returned Ok(_)
        if frame_acquire_returned_ok {
            trace!("Attempting to release DXGI frame (since AcquireNextFrame returned Ok).");
            match duplication.ReleaseFrame() {
                Ok(_) => {
                    trace!("DXGI frame released successfully.");
                }
                Err(e) => {
                    // This could still happen (e.g., INVALID_CALL even after S_OK acquire)
                    // Propagate it for recreation.
                    error!(
                        "duplication.ReleaseFrame() failed after Ok acquire: {:?}. Code: {:?}",
                        e,
                        e.code()
                    );
                    // Rely on SendableSample Drop to release texture if it exists
                    return Err(e.into()); // Propagate the error
                }
            }
        } else {
            // This now covers the DXGI_ERROR_WAIT_TIMEOUT case and other AcquireNextFrame errors
            trace!("Skipping ReleaseFrame because AcquireNextFrame returned an error.");
        }

        // 5. Draw Cursor (AFTER ReleaseFrame attempt, if applicable and content exists)
        // Keep the context lock attempt around GDI for now, just in case
        if capture_cursor && frame_has_content {
            if let Some(ref tex_to_draw_on) = final_texture {
                trace!("Drawing cursor onto prepared frame.");
                debug!("Acquiring D3D context lock BEFORE GDI draw...");
                let _gdi_context_guard = context_mutex.lock().unwrap(); // Lock D3D context
                debug!(" -> D3D context lock acquired for GDI.");
                if let Err(e) = draw_cursor_gdi(tex_to_draw_on) {
                    debug!("Failed to draw cursor using GDI: {:?}", e);
                }
                debug!(" -> Releasing D3D context lock AFTER GDI draw.");
                // D3D context lock released here (_gdi_context_guard goes out of scope)
            } else {
                warn!("Capture cursor is true and frame had content, but final_texture is None - skipping cursor draw.");
            }
        }
        // End of focused path logic
    } else {
        // --- Unfocused Path: Use Blank Frame ---
        trace!("Window unfocused, using blank frame.");
        final_texture = Some(texture_pool.get_blank_texture().map_err(|e| {
            error!("Failed to get blank texture from pool: {:?}", e);
            FrameError::TexturePoolError
        })?);
        needs_release_to_pool = false; // Blank texture isn't released back to acquisition pool
    }

    // 6. Frame Sending and Timing Logic (Common path)
    if let Some(texture_to_send) = final_texture {
        // Handle frame timing duplication BEFORE sending the current frame
        while *accumulated_delay >= frame_duration {
            debug!(
                "Duping a frame to catch up (accumulated delay: {:?})",
                *accumulated_delay
            );
            // Use the *same* texture_to_send for duplication
            match send_frame(&texture_to_send, frame_count, send, sample_pool) {
                Ok(_) => {
                    *next_frame_time += frame_duration;
                    *accumulated_delay -= frame_duration;
                    *num_duped += 1;
                }
                Err(_) => {
                    warn!("Channel closed during frame duplication, stopping.");
                    // Rely on SendableSample's Drop for pool return on error.
                    return Err(FrameError::ChannelClosed); // Return error, Drop impls will handle cleanup
                }
            }
        }

        // Send the actual current frame
        trace!(
            "Sending frame {} ({}).",
            frame_count,
            if needs_release_to_pool {
                "Captured"
            } else {
                "Blank"
            } // Log based on pool origin
        );
        match send_frame(&texture_to_send, frame_count, send, sample_pool) {
            Ok(_) => {
                // Success! Rely on SendableSample Drop to release texture back to pool when done.
                trace!(
                    "Frame {} sent, SendableSample will release resources.",
                    frame_count
                );
            }
            Err(_) => {
                warn!("Channel closed during frame sending, stopping.");
                // Error sending. SendableSample's Drop will handle releasing the texture if needed.
                return Err(FrameError::ChannelClosed);
            }
        }
    } else {
        // No frame was prepared (e.g., focused window but AcquireNextFrame timed out or failed early)
        trace!(
            "No final texture prepared for frame {}, skipping send.",
            frame_count
        );
    }

    // 7. Advance Frame Timing
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
