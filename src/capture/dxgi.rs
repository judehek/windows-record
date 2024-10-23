use windows::core::{ComInterface, Result};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

pub unsafe fn setup_dxgi_duplication(device: &ID3D11Device) -> Result<IDXGIOutputDuplication> {
    // Get DXGI device
    let dxgi_device: IDXGIDevice = device.cast()?;

    // Get adapter
    let dxgi_adapter: IDXGIAdapter = dxgi_device.GetAdapter()?;

    // Get output
    let output = dxgi_adapter.EnumOutputs(0)?;
    let output1: IDXGIOutput1 = output.cast()?;

    // Create duplication
    let duplication = output1.DuplicateOutput(device)?;

    Ok(duplication)
}

pub unsafe fn create_blank_dxgi_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(ID3D11Texture2D, IDXGIResource)> {
    use windows::Win32::Graphics::Direct3D11::*;

    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_FLAG(40),
        CPUAccessFlags: D3D11_CPU_ACCESS_FLAG(0),
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(10496),
    };

    let mut texture = None;
    device.CreateTexture2D(&desc, None, Some(&mut texture))?;

    let texture = texture.unwrap();
    let dxgi_resource: IDXGIResource = texture.cast()?;

    Ok((texture, dxgi_resource))
}
