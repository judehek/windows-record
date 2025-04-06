use log::{debug, error, info, trace, warn};
use windows::core::{ComInterface, Result};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::*;

pub unsafe fn setup_dxgi_duplication(device: &ID3D11Device) -> Result<IDXGIOutputDuplication> {
    // Get DXGI device
    let dxgi_device: IDXGIDevice = device.cast()?;

    // Get adapter
    let dxgi_adapter: IDXGIAdapter = dxgi_device.GetAdapter()?;

    // Get output
    let output = dxgi_adapter.EnumOutputs(0)?;
    let output1: IDXGIOutput1 = output.cast()?;

    // Create duplication with flag to include cursor
    let duplication = output1.DuplicateOutput(device)?;

    // Log cursor capabilities
    let mut desc = DXGI_OUTDUPL_DESC::default();
    duplication.GetDesc(&mut desc);
    debug!("DXGI Output Duplication Description:");
    debug!(
        "  - Desktop image capture supported: {}",
        !desc.DesktopImageInSystemMemory.as_bool()
    );
    debug!(
        "  - Cursor capture supported: {}",
        desc.DesktopImageInSystemMemory.as_bool()
    );

    Ok(duplication)
}
