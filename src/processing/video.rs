use std::mem::ManuallyDrop;
use log::{info, warn};
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

    // Initialize the converter - only flush once at the beginning instead of each frame
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
    
    // Try enabling async mode
    match converter.GetAttributes() {
        Ok(attrs) => {
            // Try to set async mode - this may fail if not supported
            let result = attrs.SetUINT32(&MF_TRANSFORM_ASYNC, 1);
            if result.is_ok() {
                info!("Async processing enabled successfully");
            } else {
                info!("Transform doesn't support async processing");
            }
        },
        Err(_) => {
            // This transform doesn't support the attributes interface
            warn!("Transform doesn't support attributes interface");
        }
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