use log::{debug, error};
use std::mem::ManuallyDrop;
use windows::core::{ComInterface, Result};
use windows::Win32::Foundation::FALSE;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

pub unsafe fn setup_video_converter(
    device: &ID3D11Device,
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
) -> Result<IMFTransform> {
    // Create converter
    let converter: IMFTransform =
        CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER)?;

    // Set output type first (REQUIRED)
    let output_type: IMFMediaType = MFCreateMediaType()?;
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    output_type.SetUINT32(
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0.try_into().unwrap(),
    )?;
    output_type.SetUINT64(&MF_MT_FRAME_SIZE, ((output_width as u64) << 32) | (output_height as u64))?;
    output_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, output_width as u32)?;
    output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
    converter.SetOutputType(0, &output_type, 0)?;
    
    // Set input media type (BGRA)
    let input_type: IMFMediaType = MFCreateMediaType()?;
    input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
    input_type.SetUINT32(
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0.try_into().unwrap(),
    )?;
    input_type.SetUINT64(&MF_MT_FRAME_SIZE, ((input_width as u64) << 32) | (input_height as u64))?;
    input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
    input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, (input_width * 4) as u32)?;
    converter.SetInputType(0, &input_type, 0)?;

    Ok(converter)
}

pub unsafe fn convert_bgra_to_nv12(
    device: &ID3D11Device,
    converter: &IMFTransform,
    in_sample: &IMFSample,
    output_width: u32,
    output_height: u32,
) -> Result<IMFSample> {
    let duration = in_sample.GetSampleDuration()?;
    let time = in_sample.GetSampleTime()?;
    debug!("Processing frame at time: {}, duration: {}", time, duration);

    // Create NV12 texture and output sample
    let (nv12_texture, output_sample) = create_nv12_output(device, output_width, output_height)?;

    // Process the frame
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
    converter.ProcessInput(0, in_sample, 0)?;
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;

    let mut output = MFT_OUTPUT_DATA_BUFFER {
        pSample: ManuallyDrop::new(Some(output_sample)),
        dwStatus: 0,
        pEvents: ManuallyDrop::new(None),
        dwStreamID: 0,
    };

    let mut output_slice = std::slice::from_mut(&mut output);
    let mut status: u32 = 0;

    if let Err(e) = converter.ProcessOutput(0, output_slice, &mut status) {
        device.GetDeviceRemovedReason()?;
        return Err(e);
    }

    ManuallyDrop::drop(&mut output_slice[0].pEvents);
    let final_sample = ManuallyDrop::take(&mut output_slice[0].pSample)
        .ok_or(windows::core::Error::from_win32())?;

    Ok(final_sample)
}

unsafe fn create_nv12_output(
    device: &ID3D11Device,
    output_width: u32,
    output_height: u32,
) -> Result<(ID3D11Texture2D, IMFSample)> {
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;

    // Create NV12 texture
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
        BindFlags: D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
        CPUAccessFlags: D3D11_CPU_ACCESS_FLAG(0),
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
    };

    let mut nv12_texture = None;
    device.CreateTexture2D(&nv12_desc, None, Some(&mut nv12_texture))?;
    let nv12_texture = nv12_texture.unwrap();

    // Create output sample
    let output_sample: IMFSample = MFCreateSample()?;

    // Cast to IDXGISurface instead of ID3D11Resource
    let nv12_surface: IDXGISurface = nv12_texture.cast()?;
    let output_buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &nv12_surface, 0, FALSE)?;

    output_sample.AddBuffer(&output_buffer)?;

    Ok((nv12_texture, output_sample))
}