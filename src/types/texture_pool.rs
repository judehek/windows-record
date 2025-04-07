// src/types/texture_pool.rs (or wherever TexturePool is defined)

use log::{debug, info, trace, warn};
use std::sync::{Arc, Mutex};
use windows::core::{ComInterface, Result};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, D3D11_BIND_FLAG, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_FLAG, D3D11_RESOURCE_MISC_FLAG,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};

/// A thread-safe pool of reusable D3D11 textures, specialized for a single format and purpose.
#[derive(Debug)]
pub struct TexturePool {
    device: Arc<ID3D11Device>,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    usage: u32,
    bind_flags: u32,
    cpu_access: u32,
    misc_flags: u32,
    textures: Mutex<Vec<ID3D11Texture2D>>, // Simple pool of textures
    capacity: usize,                       // Remember capacity for logging/debug

    // Tracking for debug purposes (Optional but helpful)
    #[cfg(debug_assertions)]
    created_count: std::sync::atomic::AtomicUsize,
    #[cfg(debug_assertions)]
    acquired_count: std::sync::atomic::AtomicUsize,
    #[cfg(debug_assertions)]
    released_count: std::sync::atomic::AtomicUsize,
}

impl TexturePool {
    /// Creates a new texture pool for textures with specific characteristics.
    pub fn new(
        device: Arc<ID3D11Device>,
        capacity: usize,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        usage: D3D11_USAGE,
        bind_flags: D3D11_BIND_FLAG,
        cpu_access: D3D11_CPU_ACCESS_FLAG,
        misc_flags: D3D11_RESOURCE_MISC_FLAG,
    ) -> Result<Self> {
        let mut textures = Vec::with_capacity(capacity);
        // Pre-allocate textures
        for i in 0..capacity {
            let texture = unsafe {
                Self::create_texture(
                    &device,
                    width,
                    height,
                    format,
                    usage.0 as u32,
                    bind_flags.0 as u32,
                    cpu_access.0 as u32,
                    misc_flags.0 as u32,
                )?
            };
            #[cfg(debug_assertions)]
            debug!(
                "TexturePool: Created initial texture #{} at {:p} (Format: {:?}, Misc: {:?})",
                i, &texture as *const _, format, misc_flags
            );
            textures.push(texture);
        }

        info!(
            "TexturePool initialized: Capacity={}, Size={}x{}, Format={:?}, Usage={:?}, Bind={:?}, CPU={:?}, Misc={:?}",
            capacity, width, height, format, usage, bind_flags, cpu_access, misc_flags
        );

        Ok(Self {
            device,
            width,
            height,
            format,
            usage: usage.0 as u32,
            bind_flags: bind_flags.0 as u32,
            cpu_access: cpu_access.0 as u32,
            misc_flags: misc_flags.0 as u32,
            textures: Mutex::new(textures),
            capacity,
            #[cfg(debug_assertions)]
            created_count: std::sync::atomic::AtomicUsize::new(capacity),
            #[cfg(debug_assertions)]
            acquired_count: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(debug_assertions)]
            released_count: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Acquire a texture from the pool, creating a new one if empty.
    pub fn acquire_texture(&self) -> Result<ID3D11Texture2D> {
        let mut textures = self.textures.lock().unwrap();

        let texture = if let Some(texture) = textures.pop() {
            #[cfg(debug_assertions)]
            trace!(
                "TexturePool: Reusing texture from pool at {:p}",
                &texture as *const _
            );
            texture
        } else {
            warn!(
                "TexturePool ({}x{} {:?}) depleted (capacity {}), creating new texture.",
                self.width, self.height, self.format, self.capacity
            );
            let texture = unsafe {
                Self::create_texture(
                    &self.device,
                    self.width,
                    self.height,
                    self.format,
                    self.usage,
                    self.bind_flags,
                    self.cpu_access,
                    self.misc_flags,
                )?
            };
            #[cfg(debug_assertions)]
            {
                let total_created = self
                    .created_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                debug!(
                    "TexturePool: Created new texture at {:p} (total created: {})",
                    &texture as *const _, total_created
                );
            }
            texture
        };

        #[cfg(debug_assertions)]
        {
            let acquired = self
                .acquired_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let created = self.created_count.load(std::sync::atomic::Ordering::SeqCst);
            let released = self
                .released_count
                .load(std::sync::atomic::Ordering::SeqCst);
            let active = acquired - released;

            if active % 30 == 0 || active == 1 {
                // Log periodically or on first acquisition
                debug!(
                    "TexturePool ({:?}) stats - created: {}, acquired: {}, released: {}, active: {}",
                    self.format, created, acquired, released, active
                );
            }
        }
        Ok(texture)
    }

    /// Return a texture to the pool for reuse.
    pub fn release_texture(&self, texture: ID3D11Texture2D) {
        #[cfg(debug_assertions)]
        trace!(
            "TexturePool: Returning texture to pool at {:p}",
            &texture as *const _
        );

        let mut textures = self.textures.lock().unwrap();
        // Optional: Check if pool is already full to avoid unbounded growth?
        // if textures.len() < self.capacity {
        textures.push(texture);
        // } else {
        //     warn!("TexturePool release ignored, pool already at capacity {}", self.capacity);
        //     // Texture will be dropped here if not pushed
        // }

        #[cfg(debug_assertions)]
        {
            let released = self
                .released_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let acquired = self
                .acquired_count
                .load(std::sync::atomic::Ordering::SeqCst);
            let active = acquired - released;
            if active == 0 || active % 30 == 0 {
                debug!(
                    "TexturePool ({:?}) stats - After release - active: {}",
                    self.format, active
                );
            }
        }
    }

    /// Helper function to create a single D3D11 texture.
    unsafe fn create_texture(
        device: &ID3D11Device,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        usage: u32,
        bind_flags: u32,
        cpu_access: u32,
        misc_flags: u32,
    ) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE(usage as i32),
            BindFlags: D3D11_BIND_FLAG(bind_flags as i32),
            CPUAccessFlags: D3D11_CPU_ACCESS_FLAG(cpu_access as i32),
            MiscFlags: D3D11_RESOURCE_MISC_FLAG(misc_flags as i32),
        };

        let mut texture = None;
        // This is where the E_INVALIDARG was likely happening
        device.CreateTexture2D(&desc, None, Some(&mut texture))?;

        Ok(texture.unwrap())
    }

    // Public getter for capacity if needed elsewhere
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

// Optional: Implement Drop to ensure textures are released (though Arc/Mutex should handle it)
impl Drop for TexturePool {
    fn drop(&mut self) {
        info!("Dropping TexturePool ({:?})", self.format);
        // Textures inside the Mutex<Vec<>> will be dropped automatically when the Vec drops.
    }
}
