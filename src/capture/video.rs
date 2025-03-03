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
use windows::Win32::Graphics::Dxgi::{IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use super::dxgi::{create_blank_dxgi_texture, setup_dxgi_duplication};
use super::window::{find_window_by_substring, is_window_valid, get_window_title};
use crate::processing::media::create_dxgi_sample;
use crate::types::{SendableSample, TexturePool};

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
}

impl WindowTracker {
    /// Create a new window tracker
    fn new(hwnd: HWND, process_name: &str) -> Self {
        Self {
            hwnd,
            process_name: process_name.to_string(),
            last_check: Instant::now(),
            check_interval: Duration::from_secs(2), // Check every 2 seconds
            ever_focused: false,
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
        debug!("Window handle no longer valid, attempting to find '{}' again", self.process_name);
        if let Some(new_hwnd) = find_window_by_substring(&self.process_name) {
            debug!("Found window again with new handle: {:?}", new_hwnd);
            self.hwnd = new_hwnd;
            return true;
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
}

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

pub unsafe fn collect_frames(
    send: Sender<SendableSample>,
    recording: Arc<AtomicBool>,
    hwnd: HWND,
    process_name: &str, // Added process_name for window tracking
    fps_num: u32,
    fps_den: u32,
    input_width: u32,
    input_height: u32,
    started: Arc<Barrier>,
    device: Arc<ID3D11Device>,
    context_mutex: Arc<Mutex<ID3D11DeviceContext>>,
) -> Result<()> {
    info!("Starting frame collection for window: '{}'", get_window_title(hwnd));
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);

    // Create window tracker to handle focus and window validity
    let mut window_tracker = WindowTracker::new(hwnd, process_name);

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

    // Signal that we're ready
    started.wait();

    // Initialize duplication
    let mut duplication_result = setup_dxgi_duplication(&device);
    if duplication_result.is_ok() {
        // Add a small delay to ensure the duplication interface is ready
        spin_sleep::sleep(Duration::from_millis(50));
    }
    
    // Main recording loop
    while recording.load(Ordering::Relaxed) {
        // Periodically check if window is still valid
        if !window_tracker.ensure_valid_window() {
            // Window is no longer valid, try to find it again
            warn!("Window no longer valid, attempting to find '{}'", process_name);
            if let Some(new_hwnd) = find_window_by_substring(process_name) {
                info!("Found window '{}' again, continuing recording", process_name);
                window_tracker = WindowTracker::new(new_hwnd, process_name);
            } else {
                // Can't find window, wait and retry
                warn!("Window '{}' not found, will retry", process_name);
                spin_sleep::sleep(Duration::from_secs(1));
                continue;
            }
        }

        // Ensure we have a valid duplication interface
        if duplication_result.is_err() {
            warn!("No valid duplication interface, attempting to create one");
            duplication_result = setup_dxgi_duplication(&device);
            if duplication_result.is_err() {
                spin_sleep::sleep(Duration::from_millis(100));
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
        ) {
            Ok(_) => {
                frame_count += 1;
                trace!("Collected frame {}", frame_count);
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
    window_tracker: &mut WindowTracker, // Updated to take window_tracker instead of raw hwnd
    fps_num: u32,
    send: &Sender<SendableSample>,
    frame_count: u64,
    next_frame_time: &mut Instant,
    frame_duration: Duration,
    accumulated_delay: &mut Duration,
    num_duped: &mut u64,
    _texture_pool: &Arc<TexturePool>, // Unused for now, will be used for future optimizations
) -> std::result::Result<(), FrameError> {
    let mut resource: Option<IDXGIResource> = None;
    let mut info = windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_FRAME_INFO::default();

    // Check if window is focused using our tracker
    let is_window_focused = window_tracker.is_focused();
    
    // Only show content when window is focused
    // Note: We used to check "ever_focused" but that caused issues with alt-tabbing
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

    let context = context_mutex.lock().unwrap();
    if let Some(resource) = resource {
        let texture: ID3D11Texture2D = resource.cast()?;

        if should_show_content {
            // Window is in focus, display the actual content
            context.CopyResource(staging_texture, &texture);
        } else {
            // Window is not in focus, display a black screen
            context.CopyResource(staging_texture, blank_texture);
        }

        // Explicitly release the texture before releasing the frame
        drop(texture);
        duplication.ReleaseFrame()?;
    }
    drop(context);

    // Handle frame timing and duplication
    while *accumulated_delay >= frame_duration {
        debug!("Duping a frame to catch up");
        send_frame(staging_texture, fps_num, frame_count, send)
            .map_err(|_| FrameError::ChannelClosed)?;
        *next_frame_time += frame_duration;
        *accumulated_delay -= frame_duration;
        *num_duped += 1;
    }

    send_frame(staging_texture, fps_num, frame_count, send)
        .map_err(|_| FrameError::ChannelClosed)?;
    *next_frame_time += frame_duration;

    let current_time = Instant::now();
    handle_frame_timing(current_time, *next_frame_time, accumulated_delay);

    Ok(())
}

unsafe fn send_frame(
    texture: &ID3D11Texture2D,
    fps_num: u32,
    frame_count: u64,
    send: &Sender<SendableSample>,
) -> Result<()> {
    // Create a Media Foundation sample from the texture
    let samp = create_dxgi_sample(texture, fps_num)?;
    samp.SetSampleTime((frame_count as i64 * 10_000_000i64 / fps_num as i64) as i64)?;
    
    // Wrap the sample in an Arc for thread-safety
    // The Arc will ensure the sample is properly released when all references are gone
    send.send(SendableSample(Arc::new(samp)))
        .map_err(|_| Error::from_win32())?;
    
    Ok(())
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

unsafe fn create_staging_texture(
    device: &ID3D11Device,
    input_width: u32,
    input_height: u32,
) -> Result<ID3D11Texture2D> {
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;

    let desc = D3D11_TEXTURE2D_DESC {
        Width: input_width,
        Height: input_height,
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

    let mut staging_texture = None;
    device.CreateTexture2D(&desc, None, Some(&mut staging_texture))?;
    Ok(staging_texture.unwrap())
}