use log::{debug, error};
use std::mem::ManuallyDrop;
use windows::core::{ComInterface, Result};
use windows::Win32::Foundation::FALSE;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

pub unsafe fn setup_video_converter(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<IMFTransform> {
    // Create converter using CreateInstance instead of factory
    let converter: IMFTransform =
        CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER)?;

    // Set input media type (B8G8R8A8_UNORM)
    let input_type: IMFMediaType = MFCreateMediaType()?;
    input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
    input_type.SetUINT32(
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0.try_into().unwrap(),
    )?;
    input_type.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
    input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
    input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, (width * 4) as u32)?;
    converter.SetInputType(0, &input_type, 0)?;

    // Set output media type (NV12)
    let output_type: IMFMediaType = MFCreateMediaType()?;
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    output_type.SetUINT32(
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0.try_into().unwrap(),
    )?;
    output_type.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
    output_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, width as u32)?;
    output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
    converter.SetOutputType(0, &output_type, 0)?;

    Ok(converter)
}

pub unsafe fn convert_bgra_to_nv12(
    device: &ID3D11Device,
    converter: &IMFTransform,
    in_sample: &IMFSample,
    width: u32,
    height: u32,
) -> Result<IMFSample> {
    let duration = in_sample.GetSampleDuration()?;
    let time = in_sample.GetSampleTime()?;
    debug!("Processing frame at time: {}, duration: {}", time, duration);

    // Create NV12 texture and output sample
    let (nv12_texture, output_sample) = create_nv12_output(device, width, height)?;

    // Process the frame
    process_frame(converter, in_sample, &output_sample)?;

    Ok(output_sample)
}

unsafe fn create_nv12_output(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(ID3D11Texture2D, IMFSample)> {
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;

    // Create NV12 texture
    let nv12_desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: D3D11_BIND_FLAG(0),
        CPUAccessFlags: D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE,
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
    };

    let mut nv12_texture = None;
    device.CreateTexture2D(&nv12_desc, None, Some(&mut nv12_texture))?;
    let nv12_texture = nv12_texture.unwrap();

    // Create output sample
    let output_sample: IMFSample = MFCreateSample()?;

    // First cast to ID3D11Resource, then use that interface for creating the buffer
    let nv12_resource: ID3D11Resource = nv12_texture.cast()?;
    let output_buffer = MFCreateDXGISurfaceBuffer(&ID3D11Resource::IID, &nv12_resource, 0, FALSE)?;

    output_sample.AddBuffer(&output_buffer)?;

    Ok((nv12_texture, output_sample))
}

unsafe fn process_frame(
    converter: &IMFTransform,
    input_sample: &IMFSample,
    output_sample: &IMFSample,
) -> Result<()> {
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
    converter.ProcessInput(0, input_sample, 0)?;
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;

    let mut output = MFT_OUTPUT_DATA_BUFFER {
        pSample: ManuallyDrop::new(Some(output_sample.clone())),
        dwStatus: 0,
        pEvents: ManuallyDrop::new(None),
        dwStreamID: 0,
    };

    let mut status: u32 = 0;
    let result = converter.ProcessOutput(0, std::slice::from_mut(&mut output), &mut status);

    ManuallyDrop::drop(&mut output.pEvents);
    ManuallyDrop::into_inner(output.pSample);

    if let Err(e) = result {
        error!("Frame processing failed: {:?}", e);
        return Err(e);
    }

    Ok(())
}
