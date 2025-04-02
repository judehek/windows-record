use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};
use log::{info, warn, debug};
use windows::core::{ComInterface, Result};
use windows::Win32::Foundation::FALSE;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

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

    // Helper function to set common attributes
    fn set_common_attributes(media_type: &IMFMediaType, is_progressive: bool) -> Result<()> {
        unsafe {
            let interlace_mode = if is_progressive {
                MFVideoInterlace_Progressive.0
            } else {
                MFVideoInterlace_MixedInterlaceOrProgressive.0
            };
            
            media_type.SetUINT32(
                &MF_MT_INTERLACE_MODE,
                interlace_mode.try_into().unwrap(),
            )?;
            media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
        }
        Ok(())
    }

    // Set output type first (REQUIRED)
    let output_type: IMFMediaType = MFCreateMediaType()?;
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    set_common_attributes(&output_type, true)?;
    output_type.SetUINT64(&MF_MT_FRAME_SIZE, ((output_width as u64) << 32) | (output_height as u64))?;
    output_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, output_width as u32)?;
    converter.SetOutputType(0, &output_type, 0)?;
    
    // Set input media type (BGRA)
    let input_type: IMFMediaType = MFCreateMediaType()?;
    input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
    set_common_attributes(&input_type, true)?;
    input_type.SetUINT64(&MF_MT_FRAME_SIZE, ((input_width as u64) << 32) | (input_height as u64))?;
    input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, (input_width * 4) as u32)?;
    converter.SetInputType(0, &input_type, 0)?;

    // Get MFT attributes for setting source/destination rectangles
    let attributes = converter.GetAttributes()?;
    
    // Configure the converter to use source/destination rectangles based on window position and size
    let window_pos_lock = window_position.lock().unwrap();
    let window_size_lock = window_size.lock().unwrap();
    
    // Log initial values of input, output dimensions
    info!("Video converter setup: input: {}x{}, output: {}x{}", 
    input_width, input_height, output_width, output_height);

    info!("Video converter setup: Window position: {:?}, Window size: {:?}", 
    *window_pos_lock, *window_size_lock);

    if let (Some((window_x, window_y)), Some((window_width, window_height))) = 
        (*window_pos_lock, *window_size_lock) {
        info!("Setting up converter with window info - position: [{}, {}], size: {}x{}", 
            window_x, window_y, window_width, window_height);

        // Calculate the source rectangle within the captured screen
        // (if the window is smaller than the captured area, we need to crop)
        if window_width <= input_width && window_height <= input_height {
        // Log the comparison values
        info!("Window dimensions <= input dimensions, applying source rectangle");
        info!("Comparing - window: {}x{}, input: {}x{}", 
                window_width, window_height, input_width, input_height);
        
        // MFVideoNormalizedRect has values from 0.0 to 1.0
        let src_x = window_x as f32 / input_width as f32;
        let src_y = window_y as f32 / input_height as f32;
        let src_width = window_width as f32 / input_width as f32;
        let src_height = window_height as f32 / input_height as f32;
        
        // Log pre-clamped values
        info!("Pre-clamped normalized values - x: {}, y: {}, width: {}, height: {}", 
                src_x, src_y, src_width, src_height);
        
        // Clamp values to ensure they're within the valid range
        let src_x = src_x.max(0.0).min(1.0);
        let src_y = src_y.max(0.0).min(1.0);
        let src_right = (src_x + src_width).max(0.0).min(1.0);
        let src_bottom = (src_y + src_height).max(0.0).min(1.0);
        
        // Log if any clamping was applied
        if src_x != window_x as f32 / input_width as f32 ||
            src_y != window_y as f32 / input_height as f32 ||
            src_right != src_x + src_width ||
            src_bottom != src_y + src_height {
            info!("Clamping was applied to source rectangle values");
            info!("Original src_right would be: {}, src_bottom would be: {}", 
                    src_x + src_width, src_y + src_height);
        }
        
        let source_rect = MFVideoNormalizedRect {
            left: src_x,
            top: src_y,
            right: src_right,
            bottom: src_bottom,
        };
        
        info!("Setting source rect: left={}, top={}, right={}, bottom={}", 
                source_rect.left, source_rect.top, source_rect.right, source_rect.bottom);
        
        // Calculate the actual pixel dimensions this represents
        let pixel_x = (src_x * input_width as f32) as i32;
        let pixel_y = (src_y * input_height as f32) as i32;
        let pixel_width = ((src_right - src_x) * input_width as f32) as u32;
        let pixel_height = ((src_bottom - src_y) * input_height as f32) as u32;
        info!("Source rect in pixels: x={}, y={}, width={}, height={}", 
                pixel_x, pixel_y, pixel_width, pixel_height);
        
        // Set the source rectangle on the converter - convert struct to bytes
        let rect_bytes = unsafe { 
            std::slice::from_raw_parts(
                &source_rect as *const MFVideoNormalizedRect as *const u8,
                std::mem::size_of::<MFVideoNormalizedRect>()
            )
        };
        
        info!("Setting geometric aperture attribute with {} bytes", rect_bytes.len());
        match attributes.SetBlob(&MF_MT_GEOMETRIC_APERTURE, rect_bytes) {
            Ok(_) => info!("Successfully set geometric aperture"),
            Err(e) => info!("Failed to set geometric aperture: {:?}", e),
        }
    } else {
        info!("Window size exceeds input dimensions - window: {}x{}, input: {}x{}, using full input area", 
            window_width, window_height, input_width, input_height);
    }
    } else {
        info!("No window position/size available, using default full frame");
    }

    // Initialize the converter - only flush once at the beginning instead of each frame
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
    
    // Try enabling async mode
    let result = attributes.SetUINT32(&MF_TRANSFORM_ASYNC, 1);
    if result.is_ok() {
        info!("Async processing enabled successfully");
    } else {
        info!("Transform doesn't support async processing");
    }

    Ok(converter)
}

pub unsafe fn convert_bgra_to_nv12(
    device: &ID3D11Device,
    converter: &IMFTransform,
    sample: &IMFSample,
    output_width: u32,
    output_height: u32,
) -> Result<IMFSample> {
    let duration = sample.GetSampleDuration()?;
    let time = sample.GetSampleTime()?;

    // Create NV12 texture and output sample
    let (nv12_texture, output_sample) = create_nv12_output(device, output_width, output_height)?;

    // Process the frame - removed unnecessary flush between frames
    converter.ProcessInput(0, sample, 0)?;

    let mut output = MFT_OUTPUT_DATA_BUFFER {
        pSample: ManuallyDrop::new(Some(output_sample)),
        dwStatus: 0,
        pEvents: ManuallyDrop::new(None),
        dwStreamID: 0,
    };

    let output_slice = std::slice::from_mut(&mut output);
    let mut status: u32 = 0;

    let result = converter.ProcessOutput(0, output_slice, &mut status);
    
    // Extract the sample before any error handling to ensure proper resource cleanup
    let final_sample = if result.is_ok() {
        ManuallyDrop::drop(&mut output_slice[0].pEvents);
        ManuallyDrop::take(&mut output_slice[0].pSample)
            .ok_or(windows::core::Error::from_win32())?
    } else {
        // Clean up resources
        if let Some(sample) = ManuallyDrop::take(&mut output_slice[0].pSample) {
            drop(sample);
        }
        ManuallyDrop::drop(&mut output_slice[0].pEvents);
        drop(nv12_texture);
        
        // Check for device removal
        device.GetDeviceRemovedReason()?;
        return Err(result.unwrap_err());
    };
    
    // Make sure to copy the timestamp and duration from the input sample to the output sample
    final_sample.SetSampleTime(time)?;
    final_sample.SetSampleDuration(duration)?;
    
    // Release the texture as it's no longer needed
    drop(nv12_texture);

    Ok(final_sample)
}

unsafe fn create_nv12_output(
    device: &ID3D11Device,
    output_width: u32,
    output_height: u32,
) -> Result<(ID3D11Texture2D, IMFSample)> {
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;

    // Create NV12 texture with optimized flags
    let nv12_desc = D3D11_TEXTURE2D_DESC {
        Width: output_width,
        Height: output_height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET,
        CPUAccessFlags: D3D11_CPU_ACCESS_FLAG(0),
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
    };

    let mut nv12_texture = None;
    device.CreateTexture2D(&nv12_desc, None, Some(&mut nv12_texture))?;
    let nv12_texture = nv12_texture.unwrap();

    // Create output sample
    let output_sample: IMFSample = MFCreateSample()?;

    let nv12_surface: IDXGISurface = nv12_texture.cast()?;
    
    let output_buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &nv12_surface, 0, FALSE)?;

    // Add the buffer to the sample
    output_sample.AddBuffer(&output_buffer)?;
    
    // Explicitly release the surface reference after adding the buffer
    drop(nv12_surface);
    drop(output_buffer);

    Ok((nv12_texture, output_sample))
}

// Helper function to flush the converter when changing formats or at stream boundaries
pub unsafe fn flush_converter(converter: &IMFTransform) -> Result<()> {
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
    Ok(())
}