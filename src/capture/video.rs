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
use crate::processing::media::create_dxgi_sample;
use crate::types::SendableSample;

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
    fps_num: u32,
    fps_den: u32,
    input_width: u32,
    input_height: u32,
    started: Arc<Barrier>,
    device: Arc<ID3D11Device>,
    context_mutex: Arc<Mutex<ID3D11DeviceContext>>,
) -> Result<()> {
    info!("Starting frame collection");
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);

    let frame_duration = Duration::from_nanos(1_000_000_000 * fps_den as u64 / fps_num as u64);
    let mut next_frame_time = Instant::now();
    let mut frame_count = 0;
    let mut accumulated_delay = Duration::ZERO;
    let mut num_duped = 0;

    // Create staging texture once and reuse
    let staging_texture = create_staging_texture(&device, input_width, input_height)?;
    let (blank_texture, _blank_resource) = create_blank_dxgi_texture(&device, input_width, input_height)?;

    started.wait();

    // Initialize duplication
    let mut duplication_result = setup_dxgi_duplication(&device);
    
    while recording.load(Ordering::Relaxed) {
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
            hwnd,
            fps_num,
            &send,
            frame_count,
            &mut next_frame_time,
            frame_duration,
            &mut accumulated_delay,
            &mut num_duped,
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
    hwnd: HWND,
    fps_num: u32,
    send: &Sender<SendableSample>,
    frame_count: u64,
    next_frame_time: &mut Instant,
    frame_duration: Duration,
    accumulated_delay: &mut Duration,
    num_duped: &mut u64,
) -> std::result::Result<(), FrameError> {
    let mut resource: Option<IDXGIResource> = None;
    let mut info = windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_FRAME_INFO::default();

    let foreground_window = GetForegroundWindow();
    let is_target_window = foreground_window == hwnd;

    duplication.AcquireNextFrame(16, &mut info, &mut resource)?;

    let context = context_mutex.lock().unwrap();
    if let Some(resource) = resource {
        let texture: ID3D11Texture2D = resource.cast()?;

        if is_target_window {
            context.CopyResource(staging_texture, &texture);
        } else {
            context.CopyResource(staging_texture, blank_texture);
        }

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
    let samp = create_dxgi_sample(texture, fps_num)?;
    samp.SetSampleTime((frame_count as i64 * 10_000_000i64 / fps_num as i64) as i64)?;
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
        Usage: D3D11_USAGE_STAGING,
        BindFlags: D3D11_BIND_FLAG(0),
        CPUAccessFlags: D3D11_CPU_ACCESS_READ,
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
    };

    let mut staging_texture = None;
    device.CreateTexture2D(&desc, None, Some(&mut staging_texture))?;
    Ok(staging_texture.unwrap())
}
