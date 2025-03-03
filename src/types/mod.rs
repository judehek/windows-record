use std::sync::{Arc, Mutex};
use log::{debug, trace, info};
use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Media::MediaFoundation::{IMFSample, IMFSinkWriter};
pub mod safe_wrapper;

pub struct SendableSample(pub Arc<IMFSample>);
unsafe impl Send for SendableSample {}
unsafe impl Sync for SendableSample {}

#[derive(Clone)]
pub struct SendableWriter(pub Arc<IMFSinkWriter>);
unsafe impl Send for SendableWriter {}
unsafe impl Sync for SendableWriter {}

/// A thread-safe pool of reusable D3D11 textures
pub struct TexturePool {
    textures: Mutex<Vec<ID3D11Texture2D>>,
    device: Arc<ID3D11Device>,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    usage: u32,
    bind_flags: u32,
    cpu_access: u32,
    misc_flags: u32,
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
    pub fn new(
        device: Arc<ID3D11Device>,
        capacity: usize,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        usage: u32,
        bind_flags: u32,
        cpu_access: u32,
        misc_flags: u32,
    ) -> Result<Self> {
        let mut textures = Vec::with_capacity(capacity);
        
        // Pre-allocate textures
        for i in 0..capacity {
            let texture = unsafe { Self::create_texture(
                &device, width, height, format, usage, bind_flags, cpu_access, misc_flags
            )? };
            
            #[cfg(debug_assertions)]
            debug!("TexturePool: Created initial texture #{} at {:p}", i, &texture as *const _);
            
            textures.push(texture);
        }
        
        info!("TexturePool initialized with {} textures of {}x{}", capacity, width, height);
        
        Ok(Self {
            textures: Mutex::new(textures),
            device,
            width,
            height,
            format,
            usage,
            bind_flags,
            cpu_access,
            misc_flags,
            #[cfg(debug_assertions)]
            created_count: std::sync::atomic::AtomicU32::new(capacity as u32),
            #[cfg(debug_assertions)]
            acquired_count: std::sync::atomic::AtomicU32::new(0),
            #[cfg(debug_assertions)]
            released_count: std::sync::atomic::AtomicU32::new(0),
        })
    }
    
    /// Acquire a texture from the pool or create a new one if the pool is empty
    pub fn acquire(&self) -> Result<ID3D11Texture2D> {
        let mut textures = self.textures.lock().unwrap();
        
        let texture = if let Some(texture) = textures.pop() {
            #[cfg(debug_assertions)]
            trace!("TexturePool: Reusing texture from pool at {:p}", &texture as *const _);
            texture
        } else {
            // If pool is empty, create a new texture
            #[cfg(debug_assertions)]
            debug!("TexturePool depleted, creating new texture");
            
            let texture = unsafe { Self::create_texture(
                &self.device, self.width, self.height, self.format, 
                self.usage, self.bind_flags, self.cpu_access, self.misc_flags
            )? };
            
            #[cfg(debug_assertions)] {
                self.created_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                debug!("TexturePool: Created new texture at {:p}", &texture as *const _);
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
    
    /// Return a texture to the pool for reuse
    pub fn release(&self, texture: ID3D11Texture2D) {
        #[cfg(debug_assertions)]
        trace!("TexturePool: Returning texture to pool at {:p}", &texture as *const _);
        
        let mut textures = self.textures.lock().unwrap();
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

use std::time::Duration;

pub fn duration_to_hns(duration: Duration) -> i64 {
    // Convert Duration to 100-nanosecond intervals (hns)
    duration.as_nanos() as i64 / 100
}

pub fn hns_to_duration(hns: i64) -> Duration {
    Duration::from_nanos((hns * 100) as u64)
}

#[derive(Debug, Clone, Copy)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
}

impl VideoConfig {
    pub fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Self {
        Self {
            width,
            height,
            fps_num,
            fps_den,
        }
    }

    pub fn frame_duration(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 * self.fps_den as u64 / self.fps_num as u64)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AudioConfig {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
}

impl AudioConfig {
    pub fn new(channels: u16, sample_rate: u32, bits_per_sample: u16) -> Self {
        Self {
            channels,
            sample_rate,
            bits_per_sample,
        }
    }

    pub fn bytes_per_sample(&self) -> u32 {
        (self.channels as u32 * self.bits_per_sample as u32) / 8
    }

    pub fn bytes_per_second(&self) -> u32 {
        self.sample_rate * self.bytes_per_sample()
    }
}
