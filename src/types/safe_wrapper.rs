use std::sync::Arc;
use windows::core::Result;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;

pub struct D3D11DeviceContext {
    inner: Arc<ID3D11DeviceContext>,
}

impl D3D11DeviceContext {
    pub fn new(context: ID3D11DeviceContext) -> Self {
        Self {
            inner: Arc::new(context),
        }
    }

    pub unsafe fn copy_resource(
        &self,
        dst: &ID3D11Texture2D,
        src: &ID3D11Texture2D
    ) {
        self.inner.CopyResource(dst, src);
    }
}

pub struct DxgiOutputDuplication {
    inner: IDXGIOutputDuplication,
}

impl DxgiOutputDuplication {
    pub fn new(duplication: IDXGIOutputDuplication) -> Self {
        Self {
            inner: duplication,
        }
    }

    pub unsafe fn acquire_next_frame(
        &self,
        timeout_ms: u32,
        frame_info: &mut DXGI_OUTDUPL_FRAME_INFO,
        resource: &mut Option<IDXGIResource>,
    ) -> Result<()> {
        self.inner.AcquireNextFrame(timeout_ms, frame_info, resource)
    }

    pub unsafe fn release_frame(&self) -> Result<()> {
        self.inner.ReleaseFrame()
    }
}