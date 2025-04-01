use log::{debug, trace};
use windows::core::{ComInterface, Result};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Foundation::*;

/// Information about cursor in the frame
#[derive(Clone, Debug)]
pub struct CursorInfo {
    pub visible: bool,
    pub position: (i32, i32),
    pub shape: Option<CursorShape>,
    pub hotspot: (u32, u32),
}

/// Cursor shape type
#[derive(Clone, Debug)]
pub enum CursorShape {
    Monochrome(Vec<u8>, u32, u32),
    Color(Vec<u8>, u32, u32),
    MaskedColor(Vec<u8>, u32, u32),
}

impl Default for CursorInfo {
    fn default() -> Self {
        Self {
            visible: false,
            position: (0, 0),
            shape: None,
            hotspot: (0, 0),
        }
    }
}

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
    debug!("  - Desktop image capture supported: {}", !desc.DesktopImageInSystemMemory.as_bool());
    debug!("  - Cursor capture supported: {}", desc.DesktopImageInSystemMemory.as_bool());

    Ok(duplication)
}

/// Process cursor from DXGI frame info
pub unsafe fn process_cursor(
    duplication: &IDXGIOutputDuplication,
    frame_info: &DXGI_OUTDUPL_FRAME_INFO,
) -> Result<CursorInfo> {
    let mut cursor_info = CursorInfo::default();
    
    // Check if cursor is visible in this frame
    cursor_info.visible = frame_info.LastMouseUpdateTime > 0 || frame_info.PointerShapeBufferSize > 0;
    
    // If cursor position was updated
    if frame_info.LastMouseUpdateTime > 0 {
        if frame_info.PointerPosition.Visible.as_bool() {
            cursor_info.position = (
                frame_info.PointerPosition.Position.x,
                frame_info.PointerPosition.Position.y,
            );
            cursor_info.visible = true;
        } else {
            cursor_info.visible = false;
        }
    }
    
    // If cursor shape was updated
    if frame_info.PointerShapeBufferSize > 0 {
        let buffer_size = frame_info.PointerShapeBufferSize as usize;
        let mut buffer = vec![0u8; buffer_size];
        let mut shape_info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        
        // Get the pointer shape
        let mut required_size = 0;
        duplication.GetFramePointerShape(
            buffer_size as u32,
            buffer.as_mut_ptr() as *mut _,
            &mut required_size,
            &mut shape_info,
        )?;
        
        // Extract hotspot information
        cursor_info.hotspot = (
            shape_info.HotSpot.x.try_into().unwrap_or(0),
            shape_info.HotSpot.y.try_into().unwrap_or(0)
        );
        
        // Process shape based on type
        let width = shape_info.Width;
        let height = shape_info.Height;
        
        match shape_info.Type {
            // Convert the numeric type values to the corresponding enum constants
            1 => { // DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME = 1
                trace!("Cursor shape type: Monochrome, size: {}x{}", width, height);
                cursor_info.shape = Some(CursorShape::Monochrome(buffer, width, height));
            }
            2 => { // DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR = 2
                trace!("Cursor shape type: Color, size: {}x{}", width, height);
                cursor_info.shape = Some(CursorShape::Color(buffer, width, height));
            }
            4 => { // DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR = 4
                trace!("Cursor shape type: Masked Color, size: {}x{}", width, height);
                cursor_info.shape = Some(CursorShape::MaskedColor(buffer, width, height));
            }
            _ => {
                debug!("Unknown cursor shape type: {}", shape_info.Type);
                cursor_info.shape = None;
            }
        }
    }
    
    Ok(cursor_info)
}

pub unsafe fn create_blank_dxgi_texture(
    device: &ID3D11Device,
    input_width: u32,
    input_height: u32,
) -> Result<(ID3D11Texture2D, IDXGIResource)> {
    use windows::Win32::Graphics::Direct3D11::*;
    use log::debug;

    debug!("Creating blank DXGI texture with dimensions {}x{}", input_width, input_height);
    
    // Add RENDER_TARGET flag to allow using the texture as a render target
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
        BindFlags: D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, // Add RENDER_TARGET flag
        CPUAccessFlags: D3D11_CPU_ACCESS_FLAG(0),
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
    };

    let mut texture = None;
    device.CreateTexture2D(&desc, None, Some(&mut texture))?;

    let texture = texture.unwrap();
    let dxgi_resource: IDXGIResource = texture.cast()?;

    Ok((texture, dxgi_resource))
}

pub unsafe fn create_staging_texture(
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
