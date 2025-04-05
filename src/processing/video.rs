use log::{debug, info, warn};
use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};
use windows::core::{ComInterface, Result};
use windows::Win32::Foundation::{FALSE, RECT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

// Create helper function for setting up source rectangle using IMFVideoProcessorControl
// Renamed function to reflect the method used
unsafe fn set_video_processor_source_rectangle(
    control: &IMFVideoProcessorControl, // Takes the control interface
    input_width: u32,
    input_height: u32,
    window_x: i32,
    window_y: i32,
    window_width: u32,
    window_height: u32,
) -> Result<bool> {
    // Log the comparison values
    info!("Window dimensions <= input dimensions, applying source rectangle");
    info!(
        "Comparing - window: {}x{}, input: {}x{}",
        window_width, window_height, input_width, input_height
    );

    // Calculate the actual pixel values to use
    let left = window_x;
    let top = window_y;
    // RECT uses right and bottom, not width and height directly
    let right = window_x + window_width as i32;
    let bottom = window_y + window_height as i32;

    // Clamp values just in case
    let final_left = left.max(0);
    let final_top = top.max(0);
    let final_right = right.min(input_width as i32);
    let final_bottom = bottom.min(input_height as i32);

    // Create RECT structure
    let source_rect = RECT {
        left: final_left,
        top: final_top,
        right: final_right,
        bottom: final_bottom,
    };

    info!(
        "Setting source rect via IMFVideoProcessorControl: left={}, top={}, right={}, bottom={}",
        source_rect.left, source_rect.top, source_rect.right, source_rect.bottom
    );

    match control.SetSourceRectangle(Some(&source_rect)) {
        Ok(_) => {
            info!("Successfully set source rectangle via IMFVideoProcessorControl");
            Ok(true)
        }
        Err(e) => {
            warn!(
                "Failed to set source rectangle via IMFVideoProcessorControl: {:?}",
                e
            );
            Ok(false) // Return false on failure, no error propogation
        }
    }
}

pub unsafe fn setup_video_converter(
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    window_position: Arc<Mutex<Option<(i32, i32)>>>,
    window_size: Arc<Mutex<Option<(u32, u32)>>>,
) -> Result<IMFTransform> {
    // Create converter
    let converter: IMFTransform =
        CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER)?;

    fn set_common_attributes(media_type: &IMFMediaType, is_progressive: bool) -> Result<()> {
        unsafe {
            let interlace_mode = if is_progressive {
                MFVideoInterlace_Progressive.0
            } else {
                MFVideoInterlace_MixedInterlaceOrProgressive.0
            };

            media_type.SetUINT32(&MF_MT_INTERLACE_MODE, interlace_mode.try_into().unwrap())?;
            media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
        }
        Ok(())
    }
    // Set output type first (REQUIRED)
    let output_type: IMFMediaType = MFCreateMediaType()?;
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    set_common_attributes(&output_type, true)?;
    output_type.SetUINT64(
        &MF_MT_FRAME_SIZE,
        ((output_width as u64) << 32) | (output_height as u64),
    )?;
    output_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, output_width as u32)?;
    converter.SetOutputType(0, &output_type, 0)?;

    // Set input media type (BGRA)
    let input_type: IMFMediaType = MFCreateMediaType()?;
    input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
    set_common_attributes(&input_type, true)?;
    input_type.SetUINT64(
        &MF_MT_FRAME_SIZE,
        ((input_width as u64) << 32) | (input_height as u64),
    )?;
    input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, (input_width * 4) as u32)?;
    converter.SetInputType(0, &input_type, 0)?;

    // Video Processor Control Interface
    let video_control: Option<IMFVideoProcessorControl> = converter.cast().ok();
    // Keep attributes for other settings like Async
    let attributes = converter.GetAttributes()?;

    // Configure the converter to use source/destination rectangles based on window position and size
    let window_pos_lock = window_position.lock().unwrap();
    let window_size_lock = window_size.lock().unwrap();

    info!(
        "Video converter setup: input: {}x{}, output: {}x{}",
        input_width, input_height, output_width, output_height
    );

    info!(
        "Video converter setup: Window position: {:?}, Window size: {:?}",
        *window_pos_lock, *window_size_lock
    );

    if let (Some((window_x, window_y)), Some((window_width, window_height))) =
        (*window_pos_lock, *window_size_lock)
    {
        info!(
            "Setting up converter with initial window info - position: [{}, {}], size: {}x{}",
            window_x, window_y, window_width, window_height
        );

        if window_width <= input_width && window_height <= input_height {
            // Use the correct interface if available
            if let Some(ref control) = video_control {
                set_video_processor_source_rectangle(
                    control,
                    input_width,
                    input_height,
                    window_x,
                    window_y,
                    window_width,
                    window_height,
                )?;
            } else {
                warn!("IMFVideoProcessorControl interface not found on the MFT. Cannot set source rectangle.");
            }
        } else {
            info!("Initial window size exceeds input dimensions - window: {}x{}, input: {}x{}, using full input area",
                window_width, window_height, input_width, input_height);
            // Optionally reset source rect to full frame
            // if let Some(ref control) = video_control {
            //    let full_rect = RECT { left: 0, top: 0, right: input_width as i32, bottom: input_height as i32 };
            //    control.SetSourceRectangle(0, &full_rect)?;
            // }
        }
    } else {
        info!("No window position/size available at setup, using default full frame");
        // Optionally set source rect to full frame explicitly
        // if let Some(ref control) = video_control {
        //     let full_rect = RECT { left: 0, top: 0, right: input_width as i32, bottom: input_height as i32 };
        //     control.SetSourceRectangle(0, &full_rect)?;
        // }
    }

    // Initialize the converter - only flush once at the beginning instead of each frame
    // MFT_MESSAGE_NOTIFY_BEGIN_STREAMING might be better
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
    //converter.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
    //converter.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

    // Try enabling async mode
    let result = attributes.SetUINT32(&MF_TRANSFORM_ASYNC, 1);
    if result.is_ok() {
        info!("Async processing enabled successfully");
    } else {
        info!("Transform doesn't support async processing");
    }

    Ok(converter)
}

/// Updates the video converter with new window position and size
pub unsafe fn update_video_converter(
    converter: &IMFTransform,
    input_width: u32,
    input_height: u32,
    window_position: Option<(i32, i32)>,
    window_size: Option<(u32, u32)>,
) -> Result<bool> {
    info!(
        "Updating video converter with new window info - Position: {:?}, Size: {:?}",
        window_position, window_size
    );

    // Video Processor Control Interface
    let video_control: Option<IMFVideoProcessorControl> = converter.cast().ok();

    if video_control.is_none() {
        warn!("IMFVideoProcessorControl interface not found on the MFT during update. Cannot set source rectangle.");
        return Ok(false); // Cannot perform the update
    }
    let control = video_control.unwrap();

    // Only update if we have both position and size
    if let (Some((window_x, window_y)), Some((window_width, window_height))) =
        (window_position, window_size)
    {
        // If the window is smaller than the captured area, we need to crop
        if window_width <= input_width && window_height <= input_height {
            return set_video_processor_source_rectangle(
                &control,
                input_width,
                input_height,
                window_x,
                window_y,
                window_width,
                window_height,
            );
        } else {
            info!("Window size exceeds input dimensions, using full input area");
            // Reset to full frame if necessary
            let full_rect = RECT {
                left: 0,
                top: 0,
                right: input_width as i32,
                bottom: input_height as i32,
            };

            match control.SetSourceRectangle(Some(&full_rect)) {
                Ok(_) => info!("Reset source rectangle to full input frame."),
                Err(e) => warn!("Failed to reset source rectangle: {:?}", e),
            }
            return Ok(true); // Indicate that any change was attempted/made (resetting)
        }
    } else {
        info!("Cannot update converter: Missing window position or size");
    }

    Ok(false) // Indicate no change was made
}

pub unsafe fn convert_bgra_to_nv12(
    device: &ID3D11Device,
    converter: &IMFTransform,
    sample: &IMFSample,
    output_width: u32,
    output_height: u32,
    texture_pool: &crate::types::TexturePool,
) -> Result<IMFSample> {
    let duration = sample.GetSampleDuration()?;
    let time = sample.GetSampleTime()?;

    // Get the conversion texture from the pool
    let nv12_texture = texture_pool.get_conversion_texture()?;

    // Create output sample from the texture
    let output_sample = create_output_sample_from_texture(&nv12_texture)?;

    // Process the frame
    converter.ProcessInput(0, sample, 0)?;

    let mut output_data_buffer = MFT_OUTPUT_DATA_BUFFER {
        pSample: ManuallyDrop::new(Some(output_sample)), // Pass ownership
        dwStatus: 0,
        pEvents: ManuallyDrop::new(None),
        dwStreamID: 0,
    };

    // Get a mutable slice reference to the MFT_OUTPUT_DATA_BUFFER
    let output_buffers = std::slice::from_mut(&mut output_data_buffer);
    let mut status: u32 = 0; // MFT_PROCESS_OUTPUT_STATUS

    // ProcessOutput
    // Needs pointer to MFT_OUTPUT_DATA_BUFFER array, pointer to status
    let result = converter.ProcessOutput(0, output_buffers, &mut status);

    // Correctly retrieve the sample or handle error
    let final_sample = match result {
        Ok(_) => {
            // Success, take the sample back from ManuallyDrop
            ManuallyDrop::drop(&mut output_buffers[0].pEvents); // Drop events if any
            ManuallyDrop::take(&mut output_buffers[0].pSample).ok_or_else(|| {
                windows::core::Error::new(
                    MF_E_TRANSFORM_NEED_MORE_INPUT,
                    "ProcessOutput succeeded but sample was None".into(),
                )
            })? // Should not happen if Ok
        }
        Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
            // This is expected if the MFT needs more input before producing output
            debug!("ProcessOutput returned MF_E_TRANSFORM_NEED_MORE_INPUT");
            // Clean up the sample we allocated, as it wasn't used
            if let Some(s) = ManuallyDrop::take(&mut output_buffers[0].pSample) {
                drop(s);
            }
            ManuallyDrop::drop(&mut output_buffers[0].pEvents);
            // No need to drop the texture explicitly as it's from the pool
            return Err(e); // Propagate the error code
        }
        Err(e) => {
            // Other errors
            warn!("ProcessOutput failed: {:?}", e);
            // Clean up the allocated sample
            if let Some(s) = ManuallyDrop::take(&mut output_buffers[0].pSample) {
                drop(s);
            }
            ManuallyDrop::drop(&mut output_buffers[0].pEvents);
            // No need to drop the texture explicitly as it's from the pool

            // Check for device removal
            device.GetDeviceRemovedReason()?; // This will return the error if device removed
            return Err(e); // Propagate the original error
        }
    };

    // Copy timestamp and duration
    final_sample.SetSampleTime(time)?;
    final_sample.SetSampleDuration(duration)?;

    // No need to drop the texture explicitly as it's from the pool
    // The texture will be reused for the next frame

    Ok(final_sample)
}

/// Create an IMFSample from an existing texture
unsafe fn create_output_sample_from_texture(texture: &ID3D11Texture2D) -> Result<IMFSample> {
    use windows::Win32::Foundation::FALSE;
    use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
    use windows::Win32::Graphics::Dxgi::IDXGISurface;
    use windows::Win32::Media::MediaFoundation::{MFCreateDXGISurfaceBuffer, MFCreateSample};

    // Create output sample
    let output_sample: IMFSample = MFCreateSample()?;

    // Cast the texture to IDXGISurface
    let surface: IDXGISurface = texture.cast()?;

    // Create a buffer from the DXGI surface
    let output_buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &surface, 0, FALSE)?;

    // Add the buffer to the sample. The sample now holds a reference to the buffer (and thus the surface/texture)
    output_sample.AddBuffer(&output_buffer)?;

    // Explicitly release the surface and buffer references obtained here,
    // as the sample holds its own reference now.
    drop(surface);
    drop(output_buffer);

    Ok(output_sample)
}
