use log::{debug, error, info, warn};
use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};
use windows::core::{AsImpl, ComInterface, Interface, Result}; // Ensure AsImpl is imported for as_raw()
use windows::Win32::Foundation::{FALSE, RECT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGISurface; // Needed for create_output_sample_from_texture
use windows::Win32::Media::MediaFoundation::*; // Imports most MF types
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

// --- Import your Sendable Wrapper ---
use crate::types::SendableDxgiDeviceManager;

// Helper function for setting the source rectangle on the Video Processor MFT
// This function remains unchanged internally.
unsafe fn set_video_processor_source_rectangle(
    control: &IMFVideoProcessorControl,
    input_width: u32,
    input_height: u32,
    window_x: i32,
    window_y: i32,
    window_width: u32,
    window_height: u32,
) -> Result<bool> {
    info!(
        "Window dimensions {}x{} <= input dimensions {}x{}, applying source rectangle",
        window_width, window_height, input_width, input_height
    );

    // Calculate the actual pixel values to use
    let left = window_x;
    let top = window_y;
    // RECT uses right and bottom, not width and height directly
    let right = window_x + window_width as i32;
    let bottom = window_y + window_height as i32;

    // Clamp values to ensure they are within the input dimensions
    let final_left = left.max(0);
    let final_top = top.max(0);
    // Ensure right/bottom are strictly less than width/height if needed,
    // but min() against input dimensions is usually correct for RECT.
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
            Ok(false) // Indicate failure but don't stop the process
        }
    }
}

/// Sets up the Media Foundation Video Processor MFT for color conversion (BGRA->NV12)
/// and associates it with the D3D11 device via the DXGI Device Manager.
pub unsafe fn setup_video_converter(
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    dxgi_device_manager: &SendableDxgiDeviceManager, // Use the Sendable wrapper type
    window_position: Arc<Mutex<Option<(i32, i32)>>>,
    window_size: Arc<Mutex<Option<(u32, u32)>>>,
) -> Result<IMFTransform> {
    info!(
        "Setting up video converter: Input {}x{}, Output {}x{}",
        input_width, input_height, output_width, output_height
    );

    // 1. Create the Video Processor MFT instance
    let converter: IMFTransform =
        CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER)?;
    info!("Video Processor MFT (CLSID_VideoProcessorMFT) created.");

    // 2. --- CRITICAL: Associate the DXGI Device Manager ---
    // Send the MFT_MESSAGE_SET_D3D_MANAGER message *early*.
    // Use dxgi_device_manager.as_raw() because SendableDxgiDeviceManager implements Deref.
    match converter.ProcessMessage(
        MFT_MESSAGE_SET_D3D_MANAGER,
        dxgi_device_manager.as_raw() as usize,
    ) {
        Ok(_) => info!("Successfully associated DXGI Device Manager with Video Processor MFT."),
        Err(e) => {
            // This is a significant warning! Hardware acceleration might fail.
            warn!("Failed to associate DXGI Device Manager with Video Processor MFT: {:?}. Hardware acceleration might not be available or efficient.", e);
            // Consider returning the error if hardware acceleration is mandatory:
            // return Err(e);
        }
    }
    // --- End DXGI Manager Association ---

    // Helper for setting common media type attributes (Progressive, PAR 1:1)
    // This internal helper function remains unchanged.
    fn set_common_attributes(media_type: &IMFMediaType, is_progressive: bool) -> Result<()> {
        unsafe {
            let interlace_mode = if is_progressive {
                MFVideoInterlace_Progressive.0
            } else {
                MFVideoInterlace_MixedInterlaceOrProgressive.0
            };

            media_type.SetUINT32(&MF_MT_INTERLACE_MODE, interlace_mode.try_into().unwrap())?;
            // Set Pixel Aspect Ratio to 1:1
            media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)?;
        }
        Ok(())
    }

    // 3. Set Output Media Type (NV12) - Must be set before Input
    debug!("Setting Video Processor MFT Output Type (NV12)");
    let output_type: IMFMediaType = MFCreateMediaType()?;
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    set_common_attributes(&output_type, true)?; // Assuming progressive scan
    output_type.SetUINT64(
        &MF_MT_FRAME_SIZE,
        ((output_width as u64) << 32) | (output_height as u64),
    )?;
    // For NV12, the default stride is usually equal to the width.
    // Media Foundation might calculate it, but setting it explicitly is safer.
    output_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, output_width)?;
    converter.SetOutputType(0, &output_type, 0)?;
    debug!("Video Processor MFT Output Type set.");

    // 4. Set Input Media Type (ARGB32 - assuming BGRA input from DXGI)
    debug!("Setting Video Processor MFT Input Type (ARGB32 for BGRA)");
    let input_type: IMFMediaType = MFCreateMediaType()?;
    input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    // MFVideoFormat_ARGB32 is commonly used for DXGI_FORMAT_B8G8R8A8_UNORM inputs in MFTs.
    input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
    set_common_attributes(&input_type, true)?; // Assuming progressive scan
    input_type.SetUINT64(
        &MF_MT_FRAME_SIZE,
        ((input_width as u64) << 32) | (input_height as u64),
    )?;
    // Stride for BGRA (4 bytes per pixel)
    input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, input_width * 4)?;
    converter.SetInputType(0, &input_type, 0)?;
    debug!("Video Processor MFT Input Type set.");

    // 5. Get Control Interfaces and Attributes (IMFVideoProcessorControl, IMFAttributes)
    let video_control: Option<IMFVideoProcessorControl> = converter.cast().ok();
    let attributes = converter.GetAttributes()?; // Needed for MF_TRANSFORM_ASYNC
    if video_control.is_none() {
        // This prevents dynamic cropping/source rect changes.
        warn!("IMFVideoProcessorControl interface not found on the Video Processor MFT. Dynamic cropping will not be available.");
    }

    // 6. Configure Source Rectangle (Initial Setup based on current window state)
    info!("Configuring initial source rectangle based on window info.");
    {
        // Scope for mutex locks
        let window_pos_lock = window_position.lock().unwrap();
        let window_size_lock = window_size.lock().unwrap();

        info!(
            "Video converter setup: Current Window position: {:?}, Current Window size: {:?}",
            *window_pos_lock, *window_size_lock
        );

        if let (Some((window_x, window_y)), Some((window_width, window_height))) =
            (*window_pos_lock, *window_size_lock)
        {
            // We only need to set the source rect if the window is *smaller* than the input capture area
            if window_width <= input_width && window_height <= input_height {
                if let Some(ref control) = video_control {
                    // Attempt to set the source rectangle to crop the input
                    let _ = set_video_processor_source_rectangle(
                        control,
                        input_width,
                        input_height,
                        window_x,
                        window_y,
                        window_width,
                        window_height,
                    ); // Ignore bool result here, logging handled inside
                } // else: warning already logged if control is None
            } else {
                // Window is larger than or equal to input, use the full input frame
                info!("Initial window size {}x{} >= input dimensions {}x{}, using full input area (default source rect)",
                    window_width, window_height, input_width, input_height);
                // No need to set source rect, default is full frame.
                // Could explicitly reset if desired:
                // if let Some(ref control) = video_control {
                //     let full_rect = RECT { left: 0, top: 0, right: input_width as i32, bottom: input_height as i32 };
                //     let _ = control.SetSourceRectangle(Some(&full_rect));
                // }
            }
        } else {
            // No valid window info available at setup, default to full frame
            info!("No valid window position/size available at setup, using default full frame source rect");
        }
    } // Mutex locks released here
    info!("Initial source rectangle configured.");

    // 7. Initialize the Converter State (Send initial stream messages)
    // Using NOTIFY_BEGIN_STREAMING and NOTIFY_START_OF_STREAM is standard practice.
    debug!("Sending MFT_MESSAGE_NOTIFY_BEGIN_STREAMING to Video Processor MFT.");
    converter.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
    debug!("Sending MFT_MESSAGE_NOTIFY_START_OF_STREAM to Video Processor MFT.");
    converter.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
    info!("Video Processor MFT stream started.");

    // 8. Try Enabling Async Processing Mode (Optional, may not be supported)
    // Allows ProcessInput/ProcessOutput calls to potentially return pending results,
    // requiring more complex handling (not fully implemented in convert_bgra_to_nv12 below).
    match attributes.SetUINT32(&MF_TRANSFORM_ASYNC, 1) {
         Ok(_) => info!("Async processing enabled successfully on Video Processor MFT."),
         Err(_) => info!("Video Processor MFT does not support async processing (MF_TRANSFORM_ASYNC). Using synchronous mode."),
    }

    info!("Video converter setup complete.");
    Ok(converter)
}

/// Updates the source rectangle of the video converter based on new window position and size.
/// This function remains unchanged internally.
pub unsafe fn update_video_converter(
    converter: &IMFTransform,
    input_width: u32,
    input_height: u32,
    window_position: Option<(i32, i32)>,
    window_size: Option<(u32, u32)>,
) -> Result<bool> {
    info!(
        "Attempting to update video converter source rectangle - New Position: {:?}, New Size: {:?}",
        window_position, window_size
    );

    // Get the IMFVideoProcessorControl interface from the converter
    let video_control: Option<IMFVideoProcessorControl> = converter.cast().ok();

    // If the interface isn't available, we can't update the source rect
    if video_control.is_none() {
        warn!("IMFVideoProcessorControl interface not found on the MFT during update. Cannot set source rectangle.");
        return Ok(false); // Indicate no update was possible
    }
    let control = video_control.unwrap(); // We know it's Some now

    // Only proceed if we have valid position and size data
    if let (Some((window_x, window_y)), Some((window_width, window_height))) =
        (window_position, window_size)
    {
        // Check if cropping is needed (window smaller than input)
        if window_width <= input_width && window_height <= input_height {
            debug!("Window size is within input bounds, applying source rectangle for cropping.");
            // Call the helper function to set the rectangle
            return set_video_processor_source_rectangle(
                &control,
                input_width,
                input_height,
                window_x,
                window_y,
                window_width,
                window_height,
            ); // Return the result (bool indicating success/failure of setting)
        } else {
            // Window is larger than or same size as input, ensure we use the full input frame
            info!("Window size {}x{} >= input dimensions {}x{}, ensuring full input frame is used (resetting source rectangle).",
                  window_width, window_height, input_width, input_height);
            // Define the rectangle covering the entire input frame
            let full_rect = RECT {
                left: 0,
                top: 0,
                right: input_width as i32,
                bottom: input_height as i32,
            };
            // Attempt to set the source rectangle to the full frame
            match control.SetSourceRectangle(Some(&full_rect)) {
                Ok(_) => info!("Reset source rectangle to full input frame successfully."),
                Err(e) => warn!(
                    "Failed to reset source rectangle to full input frame: {:?}",
                    e
                ),
            }
            // Indicate that an update attempt (resetting) was made
            return Ok(true);
        }
    } else {
        // We didn't receive valid window position or size
        info!("Cannot update converter source rectangle: Missing window position or size data.");
    }

    // Indicate that no change was made because input data was missing
    Ok(false)
}

/// Performs the color conversion from BGRA to NV12 using the Video Processor MFT.
/// Assumes the MFT is configured and associated with the DXGI Device Manager.
/// This function remains unchanged internally.
pub unsafe fn convert_bgra_to_nv12(
    device: &ID3D11Device,    // Needed for GetDeviceRemovedReason on error
    converter: &IMFTransform, // The configured Video Processor MFT
    sample: &IMFSample,       // Input sample containing BGRA texture buffer
    output_width: u32,
    output_height: u32,
    texture_pool: &crate::types::TexturePool, // Pool for output NV12 textures
) -> Result<IMFSample> {
    debug!("Converting BGRA sample to NV12...");
    let start_time = std::time::Instant::now();

    // --- Prepare Input ---
    // Get timestamp and duration from input sample
    let duration = sample.GetSampleDuration()?;
    let time = sample.GetSampleTime()?;
    debug!("Input sample time: {}, duration: {}", time, duration);

    // --- Prepare Output ---
    // Get a reusable NV12 texture from the pool for the output
    let nv12_texture = texture_pool.acquire_texture()?; // This texture is DXGI_FORMAT_NV12
    debug!("Acquired NV12 output texture from pool.");

    // Create an IMFSample wrapping the output NV12 texture
    let output_sample = create_output_sample_from_texture(&nv12_texture)?;
    debug!("Created output IMFSample wrapping NV12 texture.");

    // --- Process Input ---
    // Send the input BGRA sample to the converter MFT
    debug!("Calling ProcessInput on Video Processor MFT...");
    converter.ProcessInput(0, sample, 0)?; // Stream ID 0, no flags
    debug!("ProcessInput succeeded.");

    // --- Process Output ---
    // Prepare the structure to receive the output sample
    let mut output_data_buffer = MFT_OUTPUT_DATA_BUFFER {
        pSample: ManuallyDrop::new(Some(output_sample)), // Give ownership to the struct (temporarily)
        dwStatus: 0,                                     // Output status flags for this buffer
        pEvents: ManuallyDrop::new(None),                // No events expected
        dwStreamID: 0,                                   // Output stream ID (should be 0)
    };

    // ProcessOutput requires a slice of output buffer structs
    let output_buffers = std::slice::from_mut(&mut output_data_buffer);
    let mut status_flags: u32 = 0; // Overall status flags for the ProcessOutput call

    debug!("Calling ProcessOutput on Video Processor MFT...");
    // Attempt to get the processed output frame
    let result = converter.ProcessOutput(
        0,                 // Flags (e.g., MFT_PROCESS_OUTPUT_DISCARD_WHEN_NO_BUFFER) - 0 is typical
        output_buffers,    // Slice of output buffers
        &mut status_flags, // Receives overall status
    );
    debug!("ProcessOutput returned: {:?}", result);

    // --- Handle Output Result ---
    // `final_sample` will hold the successfully processed sample
    let final_sample = match result {
        Ok(_) => {
            // Successfully processed output!
            debug!("ProcessOutput succeeded. Status flags: {}", status_flags);
            // Clean up the events field (should be None anyway)
            ManuallyDrop::drop(&mut output_buffers[0].pEvents);
            // Take back ownership of the sample from ManuallyDrop.
            // If ProcessOutput succeeded, pSample should contain our processed sample.
            ManuallyDrop::take(&mut output_buffers[0].pSample).ok_or_else(|| {
                // This case should be unlikely if ProcessOutput returned Ok.
                warn!("ProcessOutput succeeded but pSample was None in MFT_OUTPUT_DATA_BUFFER!");
                windows::core::Error::new(
                    MF_E_UNEXPECTED, // Or another suitable error code
                    "ProcessOutput Ok but sample missing".into(),
                )
            })? // Propagate error if sample is unexpectedly None
        }
        Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
            // This is a common and expected case for MFTs. It means the MFT needs
            // more input data before it can produce an output frame.
            debug!("ProcessOutput returned MF_E_TRANSFORM_NEED_MORE_INPUT. MFT needs more input.");
            // Clean up the output sample we allocated, as it wasn't filled by the MFT.
            // Take ownership back from ManuallyDrop and let it drop.
            if let Some(unused_sample) = ManuallyDrop::take(&mut output_buffers[0].pSample) {
                drop(unused_sample);
                debug!("Dropped unused allocated output sample.");
            }
            // Clean up events field
            ManuallyDrop::drop(&mut output_buffers[0].pEvents);
            // Return the specific error code to the caller, signaling more input is needed.
            return Err(e);
        }
        Err(e) => {
            // An unexpected error occurred during processing.
            warn!("ProcessOutput failed with unexpected error: {:?}", e);
            // Clean up the allocated output sample
            if let Some(unused_sample) = ManuallyDrop::take(&mut output_buffers[0].pSample) {
                drop(unused_sample);
                debug!("Dropped unused allocated output sample after error.");
            }
            // Clean up events field
            ManuallyDrop::drop(&mut output_buffers[0].pEvents);

            // Check if the error was due to the D3D device being removed.
            // This helps diagnose hardware issues or driver crashes.
            match device.GetDeviceRemovedReason() {
                Ok(_) => {} // Device is fine, the error was something else.
                Err(removed_reason) => {
                    error!("D3D11 Device Removed! Reason: {:?}", removed_reason);
                    // Return the device removed error as it's likely the root cause.
                    return Err(removed_reason);
                }
            }
            // If device wasn't removed, return the original ProcessOutput error.
            return Err(e);
        }
    };

    // --- Finalize Output Sample ---
    // Copy the original timestamp and duration to the processed sample
    final_sample.SetSampleTime(time)?;
    final_sample.SetSampleDuration(duration)?;
    debug!("Set output sample time: {}, duration: {}", time, duration);

    // The NV12 texture is implicitly managed by the texture pool.
    // When the `final_sample` (and its buffer) eventually goes out of scope
    // and its reference count drops to zero, the underlying texture reference
    // is released. If it came from the pool, the pool might reuse it later.

    let elapsed = start_time.elapsed();
    debug!("BGRA to NV12 conversion took: {:?}", elapsed);

    Ok(final_sample)
}

/// Creates an IMFSample containing an IMFMediaBuffer that wraps a given D3D11 texture.
/// This is used to prepare both input (BGRA) and output (NV12) samples for MFT processing.
/// This function remains unchanged internally.
unsafe fn create_output_sample_from_texture(texture: &ID3D11Texture2D) -> Result<IMFSample> {
    // 1. Create an empty IMFSample object.
    let output_sample: IMFSample = MFCreateSample()?;

    // 2. Get the IDXGISurface interface from the D3D11 texture.
    // This is necessary for MFCreateDXGISurfaceBuffer.
    let surface: IDXGISurface = texture.cast()?;

    // 3. Create an IMFMediaBuffer that directly references the DXGI surface (the texture).
    // This avoids copying the texture data to CPU memory.
    // The `FALSE` argument indicates the buffer should not own the surface;
    // the sample will hold a reference, and the original texture owner manages its lifetime.
    let output_buffer = MFCreateDXGISurfaceBuffer(
        &ID3D11Texture2D::IID, // Specifies the type of the interface pointer being wrapped
        &surface,              // The surface interface to wrap
        0,                     // Index of the subresource (usually 0 for 2D textures)
        FALSE,                 // Buffer does not own the surface
    )?;

    // 4. Add the DXGI surface buffer to the sample.
    // The sample now contains the media data represented by the texture on the GPU.
    output_sample.AddBuffer(&output_buffer)?;

    // 5. Release local references to the surface and buffer.
    // The `output_sample` now holds its own references to the buffer (and transitively, the surface/texture).
    // Dropping these local variables prevents double-freeing.
    drop(surface);
    drop(output_buffer);

    // Return the sample containing the GPU texture buffer.
    Ok(output_sample)
}
