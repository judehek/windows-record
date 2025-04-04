use log::{debug, info, trace, warn};
use std::sync::{Arc, Mutex};
use windows::core::{ComInterface, Result};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::*;

// Important notes about texture creation in Direct3D 11:
//
// 1. For textures that will be used with GDI (GetDC/ReleaseDC), these requirements must be met:
//    - Must include the D3D11_RESOURCE_MISC_GDI_COMPATIBLE flag
//    - Must include the D3D11_BIND_RENDER_TARGET flag
//    - Cannot use D3D11_USAGE_STAGING (incompatible with GDI)
//
// 2. For staging textures (CPU access):
//    - Must use D3D11_USAGE_STAGING
//    - Cannot use any bind flags
//    - Cannot use D3D11_RESOURCE_MISC_GDI_COMPATIBLE
//
// These rules are enforced by the D3D11 API and will result in errors if violated.

/// A thread-safe pool of reusable D3D11 textures with specialized textures for different purposes
pub struct TexturePool {
    /// Device used to create textures
    device: Arc<ID3D11Device>,
    /// Width of textures in the pool
    width: u32,
    /// Height of textures in the pool
    height: u32,
    /// Format of textures in the pool
    format: DXGI_FORMAT,
    /// Pool of acquisition textures (used for frame acquisition)
    acquisition_textures: Mutex<Vec<ID3D11Texture2D>>,
    /// Single blank texture (used for blank frames when window not in focus)
    blank_texture: Mutex<Option<ID3D11Texture2D>>,
    /// Single conversion texture (used for format conversion e.g. BGRA to NV12)
    conversion_texture: Mutex<Option<ID3D11Texture2D>>,
    
    // Tracking for debug purposes
    #[cfg(debug_assertions)]
    created_count: std::sync::atomic::AtomicU32,
    #[cfg(debug_assertions)]
    acquired_count: std::sync::atomic::AtomicU32,
    #[cfg(debug_assertions)]
    released_count: std::sync::atomic::AtomicU32,
}

impl TexturePool {
    /// Create a new texture pool with the specified parameters
    /// 
    /// This texture pool manages several types of textures:
    /// - Acquisition textures: Used for capturing frames (with GDI compatibility for cursor overlay)
    /// - Blank texture: Used for blank frames when window is not in focus (with GDI compatibility) 
    /// - Conversion texture: Used for format conversion (BGRA to NV12)
    ///
    /// Note: For cursor drawing with GDI, textures need D3D11_RESOURCE_MISC_GDI_COMPATIBLE flag
    pub fn new(
        device: Arc<ID3D11Device>,
        acquisition_capacity: usize,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
    ) -> Result<Self> {
        use windows::Win32::Graphics::Direct3D11::*;
        
        // Create acquisition textures
        let mut textures = Vec::with_capacity(acquisition_capacity);
        
        // IMPORTANT: For cursor drawing with GDI, textures need D3D11_RESOURCE_MISC_GDI_COMPATIBLE flag
        // Pre-allocate acquisition textures with render target capability and GDI compatibility
        for i in 0..acquisition_capacity {
            let texture = unsafe { Self::create_texture(
                &device,
                width,
                height,
                format,
                D3D11_USAGE_DEFAULT.0 as u32,
                (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32,
                0, // CPU access flags
                D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32, // Add GDI compatibility
            )? };
            
            #[cfg(debug_assertions)]
            debug!("TexturePool: Created initial acquisition texture #{} at {:p}", i, &texture as *const _);
            
            textures.push(texture);
        }
        
        // Create a blank texture with GDI compatibility
        // Note: D3D11_RESOURCE_MISC_GDI_COMPATIBLE requires D3D11_BIND_RENDER_TARGET flag
        let blank_texture = unsafe { Self::create_texture(
            &device,
            width,
            height,
            format,
            D3D11_USAGE_DEFAULT.0 as u32,
            (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32, // Add RENDER_TARGET for GDI compatibility
            0, // CPU access flags
            D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32, // Add GDI compatibility
        )? };
        
        #[cfg(debug_assertions)]
        debug!("TexturePool: Created blank texture at {:p}", &blank_texture as *const _);
        
        // We no longer need a staging texture - using acquisition textures directly
        
        // Create a conversion texture (NV12 format) for video processing
        let conversion_texture = unsafe { Self::create_texture(
            &device,
            width,
            height,
            DXGI_FORMAT_NV12,
            D3D11_USAGE_DEFAULT.0 as u32,
            (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32,
            0, // CPU access flags
            0, // Misc flags
        )? };
        
        #[cfg(debug_assertions)]
        debug!("TexturePool: Created conversion texture at {:p}", &conversion_texture as *const _);
        
        info!("TexturePool initialized with {} acquisition textures of {}x{}", 
              acquisition_capacity, width, height);
        
        Ok(Self {
            device,
            width,
            height,
            format,
            acquisition_textures: Mutex::new(textures),
            blank_texture: Mutex::new(Some(blank_texture)),
            conversion_texture: Mutex::new(Some(conversion_texture)),
            #[cfg(debug_assertions)]
            created_count: std::sync::atomic::AtomicU32::new((acquisition_capacity + 2) as u32), // Reduced by 1 (no staging texture)
            #[cfg(debug_assertions)]
            acquired_count: std::sync::atomic::AtomicU32::new(0),
            #[cfg(debug_assertions)]
            released_count: std::sync::atomic::AtomicU32::new(0),
        })
    }
    
    /// Acquire an acquisition texture from the pool or create a new one if the pool is empty
    pub fn acquire_acquisition_texture(&self) -> Result<ID3D11Texture2D> {
        let mut textures = self.acquisition_textures.lock().unwrap();
        
        let texture = if let Some(texture) = textures.pop() {
            #[cfg(debug_assertions)]
            trace!("TexturePool: Reusing acquisition texture from pool at {:p}", &texture as *const _);
            texture
        } else {
            // If pool is empty, create a new texture
            #[cfg(debug_assertions)]
            debug!("TexturePool acquisition texture pool depleted, creating new one");
            
            use windows::Win32::Graphics::Direct3D11::*;
            
            let texture = unsafe { Self::create_texture(
                &self.device, 
                self.width, 
                self.height, 
                self.format,
                D3D11_USAGE_DEFAULT.0 as u32,
                (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32,
                0, // CPU access flags
                D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32, // Add GDI compatibility
            )? };
            
            #[cfg(debug_assertions)] {
                self.created_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                debug!("TexturePool: Created new acquisition texture at {:p}", &texture as *const _);
            }
            
            texture
        };
        
        #[cfg(debug_assertions)]
        {
            let acquired = self.acquired_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let created = self.created_count.load(std::sync::atomic::Ordering::SeqCst);
            let released = self.released_count.load(std::sync::atomic::Ordering::SeqCst);
            let active = acquired - released + 1;
            
            if active % 30 == 0 { // Log every 30 frames to avoid excessive logging
                info!("TexturePool stats - created: {}, acquired: {}, released: {}, active: {}", 
                      created, acquired + 1, released, active);
            }
        }
        
        Ok(texture)
    }
    
    /// Return an acquisition texture to the pool for reuse
    pub fn release_acquisition_texture(&self, texture: ID3D11Texture2D) {
        #[cfg(debug_assertions)]
        trace!("TexturePool: Returning acquisition texture to pool at {:p}", &texture as *const _);
        
        let mut textures = self.acquisition_textures.lock().unwrap();
        textures.push(texture);
        
        #[cfg(debug_assertions)] {
            self.released_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let acquired = self.acquired_count.load(std::sync::atomic::Ordering::SeqCst);
            let released = self.released_count.load(std::sync::atomic::Ordering::SeqCst);
            let active = acquired - released;
            
            if active % 30 == 0 || active <= 0 { // Log every 30 frames or when pool is empty
                debug!("TexturePool: After release - {} textures still active", active);
            }
        }
    }
    
    /// Get the blank texture (create if not exists)
    pub fn get_blank_texture(&self) -> Result<ID3D11Texture2D> {
        let mut blank_texture_lock = self.blank_texture.lock().unwrap();
        
        // If blank texture is None (was taken without being returned), create a new one
        if blank_texture_lock.is_none() {
            warn!("TexturePool: Blank texture was not properly returned, creating a new one");
            
            use windows::Win32::Graphics::Direct3D11::*;
            
            let texture = unsafe { Self::create_texture(
                &self.device, 
                self.width, 
                self.height, 
                self.format,
                D3D11_USAGE_DEFAULT.0 as u32,
                (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32, // Add RENDER_TARGET for GDI compatibility
                0, // CPU access flags
                D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32, // Add GDI compatibility
            )? };
            
            #[cfg(debug_assertions)] {
                self.created_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                debug!("TexturePool: Created replacement blank texture at {:p}", &texture as *const _);
            }
            
            *blank_texture_lock = Some(texture);
        }
        
        // Clone the texture interface to return it (increases ref count)
        // This way, the original stays in the pool
        let blank_texture = blank_texture_lock.as_ref().unwrap().clone();
        
        #[cfg(debug_assertions)]
        trace!("TexturePool: Providing blank texture at {:p}", &blank_texture as *const _);
        
        Ok(blank_texture)
    }
    
    // Removed get_staging_texture since we no longer need it
    
    /// Get the conversion texture (create if not exists)
    pub fn get_conversion_texture(&self) -> Result<ID3D11Texture2D> {
        let mut conversion_texture_lock = self.conversion_texture.lock().unwrap();
        
        // If conversion texture is None (was taken without being returned), create a new one
        if conversion_texture_lock.is_none() {
            warn!("TexturePool: Conversion texture was not properly returned, creating a new one");
            
            use windows::Win32::Graphics::Direct3D11::*;
            
            let texture = unsafe { Self::create_texture(
                &self.device, 
                self.width, 
                self.height, 
                DXGI_FORMAT_NV12,
                D3D11_USAGE_DEFAULT.0 as u32,
                (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32,
                0, // CPU access flags
                0, // Misc flags
            )? };
            
            #[cfg(debug_assertions)] {
                self.created_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                debug!("TexturePool: Created replacement conversion texture at {:p}", &texture as *const _);
            }
            
            *conversion_texture_lock = Some(texture);
        }
        
        // Clone the texture interface to return it (increases ref count)
        // This way, the original stays in the pool
        let conversion_texture = conversion_texture_lock.as_ref().unwrap().clone();
        
        #[cfg(debug_assertions)]
        trace!("TexturePool: Providing conversion texture at {:p}", &conversion_texture as *const _);
        
        Ok(conversion_texture)
    }
    
    /// Create a new texture with the specified parameters
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
        use windows::Win32::Graphics::Direct3D11::*;
        
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
        device.CreateTexture2D(&desc, None, Some(&mut texture))?;
        
        Ok(texture.unwrap())
    }
}