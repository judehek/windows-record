use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::collections::{VecDeque, HashMap};
use log::{debug, trace, info, error};
use windows::core::{ComInterface, Interface, Result, HRESULT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::Win32::Media::MediaFoundation::{IMFSample, IMFSinkWriter, MFCreateSample, MFCreateDXGISurfaceBuffer};
use windows::Win32::Foundation::TRUE;
pub mod safe_wrapper;

/// A wrapper for IMFSample that can be sent between threads
pub struct SendableSample {
    pub sample: Arc<IMFSample>,
    texture_ptr: Option<usize>,
    pool: Option<Arc<SamplePool>>,
}

impl SendableSample {
    /// Create a new SendableSample without pool tracking
    pub fn new(sample: IMFSample) -> Self {
        Self {
            sample: Arc::new(sample),
            texture_ptr: None,
            pool: None,
        }
    }
    
    /// Create a new SendableSample with pool tracking for auto-return
    pub fn new_pooled(sample: IMFSample, texture: &ID3D11Texture2D, pool: Arc<SamplePool>) -> Self {
        Self {
            sample: Arc::new(sample),
            texture_ptr: Some(texture as *const _ as usize),
            pool: Some(pool),
        }
    }
}

impl std::ops::Deref for SendableSample {
    type Target = Arc<IMFSample>;
    
    fn deref(&self) -> &Self::Target {
        &self.sample
    }
}

/// Allow SendableSample to be sent between threads
unsafe impl Send for SendableSample {}
unsafe impl Sync for SendableSample {}

/// When SendableSample is dropped, return the sample to the pool if it was pooled
impl Drop for SendableSample {
    fn drop(&mut self) {
        if let (Some(pool), Some(texture_ptr)) = (&self.pool, self.texture_ptr) {
            // Only return to pool if we're the last reference to this sample
            if Arc::strong_count(&self.sample) == 1 {
                // Use a thread-local to track whether we're already inside a drop to prevent cycles
                thread_local! {
                    static IN_DROP: std::cell::RefCell<bool> = std::cell::RefCell::new(false);
                }
                
                IN_DROP.with(|in_drop| {
                    let already_dropping = *in_drop.borrow();
                    if !already_dropping {
                        *in_drop.borrow_mut() = true;
                        
                        let texture_ptr_copy = texture_ptr;
                        let pool_clone = pool.clone();
                        
                        // Have to clone the IMFSample from the Arc since we can't move out of self.sample
                        // during drop (we don't own self)
                        let sample_clone = unsafe { self.sample.as_ref().clone() };
                        
                        // Return the sample to the pool
                        unsafe { 
                            if let Err(e) = pool_clone.release_no_texture(texture_ptr_copy, sample_clone) {
                                error!("Failed to return sample to pool: {:?}", e);
                            }
                        }
                        
                        *in_drop.borrow_mut() = false;
                    }
                });
            } else {
                trace!("Not releasing sample to pool - {} references remaining", 
                      Arc::strong_count(&self.sample) - 1);
            }
        }
    }
}

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

/// A thread-safe pool that manages texture-to-sample bindings
/// This maintains a mapping between texture pointers and IMFSample objects
pub struct SamplePool {
    /// Mutex-protected map of texture pointers to available IMFSample objects
    samples: Mutex<HashMap<usize, Vec<IMFSample>>>,
    /// FPS value used for sample duration
    fps_num: u32,
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

impl SamplePool {
    /// Create a new sample pool
    pub fn new(fps_num: u32, initial_capacity: usize) -> Self {
        info!("Initializing SamplePool with FPS: {}", fps_num);
        
        Self {
            samples: Mutex::new(HashMap::with_capacity(initial_capacity)),
            fps_num,
            #[cfg(debug_assertions)]
            created_count: std::sync::atomic::AtomicU32::new(0),
            #[cfg(debug_assertions)]
            acquired_count: std::sync::atomic::AtomicU32::new(0),
            #[cfg(debug_assertions)]
            released_count: std::sync::atomic::AtomicU32::new(0),
        }
    }
    
    /// Get a unique ID for a texture to use as a hash key
    fn get_texture_id(texture: &ID3D11Texture2D) -> usize {
        texture as *const _ as usize
    }
    
    /// Create a new Media Foundation sample from a texture
    unsafe fn create_sample_for_texture(texture: &ID3D11Texture2D, fps_num: u32) -> Result<IMFSample> {
        // Cast the texture to an IDXGISurface
        let surface: IDXGISurface = texture.cast()?;
        
        // Create a new Media Foundation sample
        let sample: IMFSample = MFCreateSample()?;
        
        // Create a DXGI buffer from the surface
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &surface, 0, TRUE)?;
        
        // Add the buffer to the sample
        sample.AddBuffer(&buffer)?;
        
        // Set the sample duration based on the frame rate
        sample.SetSampleDuration(10_000_000 / fps_num as i64)?;
        
        // Explicitly release the surface to avoid a reference leak
        // The buffer still maintains its reference to the underlying resource
        drop(surface);
        
        Ok(sample)
    }
    
    /// Acquire or create a sample for the given texture
    pub unsafe fn acquire_for_texture(&self, texture: &ID3D11Texture2D) -> Result<IMFSample> {
        let texture_id = Self::get_texture_id(texture);
        let mut samples_map = self.samples.lock().unwrap();
        
        // Get or create the entry for this texture
        let samples_for_texture = samples_map.entry(texture_id).or_insert_with(Vec::new);
        
        let sample = if let Some(sample) = samples_for_texture.pop() {
            #[cfg(debug_assertions)]
            trace!("SamplePool: Reusing sample for texture {:p}", texture as *const _);
            sample
        } else {
            // No existing sample for this texture, create a new one
            #[cfg(debug_assertions)]
            debug!("SamplePool: Creating new sample for texture {:p}", texture as *const _);
            
            let sample = Self::create_sample_for_texture(texture, self.fps_num)?;
            
            #[cfg(debug_assertions)] {
                self.created_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            
            sample
        };
        
        #[cfg(debug_assertions)]
        {
            let acquired = self.acquired_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let created = self.created_count.load(std::sync::atomic::Ordering::SeqCst);
            let released = self.released_count.load(std::sync::atomic::Ordering::SeqCst);
            let active = acquired - released + 1;
            
            if active % 30 == 0 { // Log every 30 frames to avoid excessive logging
                info!("SamplePool stats - created: {}, acquired: {}, released: {}, active: {}", 
                      created, acquired + 1, released, active);
            }
        }
        
        Ok(sample)
    }
    
    /// Return a sample to the pool for reuse with the same texture
    pub unsafe fn release_sample(&self, texture: &ID3D11Texture2D, sample: IMFSample) -> Result<()> {
        let texture_id = Self::get_texture_id(texture);
        self.release_no_texture(texture_id, sample)
    }
    
    /// Return a sample to the pool using just the texture ID
    /// This is used by SendableSample's Drop implementation
    pub unsafe fn release_no_texture(&self, texture_id: usize, sample: IMFSample) -> Result<()> {
        #[cfg(debug_assertions)]
        trace!("SamplePool: Returning sample for texture ID {} to pool", texture_id);
        
        let mut samples_map = self.samples.lock().unwrap();
        let samples_for_texture = samples_map.entry(texture_id).or_insert_with(Vec::new);
        samples_for_texture.push(sample);
        
        #[cfg(debug_assertions)] {
            self.released_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let acquired = self.acquired_count.load(std::sync::atomic::Ordering::SeqCst);
            let released = self.released_count.load(std::sync::atomic::Ordering::SeqCst);
            let active = acquired - released;
            
            if active % 30 == 0 || active <= 0 {
                debug!("SamplePool: After release - {} samples still active", active);
            }
        }
        
        Ok(())
    }
    
    /// Set the timestamp for a sample
    pub unsafe fn set_sample_time(&self, sample: &IMFSample, frame_count: u64) -> Result<()> {
        let frame_time = (frame_count as i64 * 10_000_000i64 / self.fps_num as i64) as i64;
        sample.SetSampleTime(frame_time)?;
        Ok(())
    }
}

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

/// A circular buffer to store recent video and audio samples for replay functionality
pub struct ReplayBuffer {
    /// Maximum duration to keep in the buffer
    max_duration: Duration,
    /// Video samples with their timestamps
    video_samples: Mutex<VecDeque<(SendableSample, i64)>>, // (sample, timestamp)
    /// Audio samples with their timestamps
    audio_samples: Mutex<VecDeque<(SendableSample, i64)>>, // (sample, timestamp)
    /// Current buffer size in memory (approximate)
    size_bytes: Mutex<usize>,
    /// Time of the oldest sample in the buffer
    pub oldest_timestamp: Mutex<i64>,
    /// Current size limits for video and audio
    video_limit: usize,
    audio_limit: usize,
}

impl ReplayBuffer {
    /// Create a new replay buffer with a specified duration
    pub fn new(max_duration: Duration, initial_video_limit: usize, initial_audio_limit: usize) -> Self {
        info!("Creating replay buffer with max duration of {:?}", max_duration);
        Self {
            max_duration,
            video_samples: Mutex::new(VecDeque::with_capacity(initial_video_limit)),
            audio_samples: Mutex::new(VecDeque::with_capacity(initial_audio_limit)),
            size_bytes: Mutex::new(0),
            oldest_timestamp: Mutex::new(0),
            video_limit: initial_video_limit,
            audio_limit: initial_audio_limit,
        }
    }

    /// Add a video sample to the buffer
    pub fn add_video_sample(&self, sample: SendableSample, timestamp: i64) -> Result<()> {
        let mut samples = self.video_samples.lock().unwrap();
        
        // Add new sample
        samples.push_back((sample, timestamp));
        
        // Update oldest timestamp if this is the only sample
        if samples.len() == 1 {
            let mut oldest = self.oldest_timestamp.lock().unwrap();
            *oldest = timestamp;
        }
        
        // Remove old samples if we exceed the limit
        self.trim_buffer(&mut samples, timestamp)?;
        
        Ok(())
    }

    /// Add an audio sample to the buffer
    pub fn add_audio_sample(&self, sample: SendableSample, timestamp: i64) -> Result<()> {
        let mut samples = self.audio_samples.lock().unwrap();
        
        // Add new sample
        samples.push_back((sample, timestamp));
        
        // Update oldest timestamp if needed
        if timestamp < *self.oldest_timestamp.lock().unwrap() {
            let mut oldest = self.oldest_timestamp.lock().unwrap();
            *oldest = timestamp;
        }
        
        // Remove old samples if we exceed the limit
        self.trim_buffer(&mut samples, timestamp)?;
        
        Ok(())
    }

    /// Remove samples that are too old
    fn trim_buffer(&self, samples: &mut VecDeque<(SendableSample, i64)>, latest_timestamp: i64) -> Result<()> {
        let cutoff_timestamp = latest_timestamp - duration_to_hns(self.max_duration);
        
        // Remove samples older than the cutoff
        while let Some((_, timestamp)) = samples.front() {
            if *timestamp < cutoff_timestamp {
                samples.pop_front();
            } else {
                break;
            }
        }
        
        // Update oldest timestamp
        if samples.is_empty() {
            let mut oldest = self.oldest_timestamp.lock().unwrap();
            *oldest = latest_timestamp;
        } else if let Some((_, timestamp)) = samples.front() {
            let mut oldest = self.oldest_timestamp.lock().unwrap();
            *oldest = *timestamp;
        }
        
        Ok(())
    }

    /// Get a list of video samples within a time range
    pub fn get_video_samples(&self, start_time: i64, end_time: i64) -> Vec<(SendableSample, i64)> {
        let samples = self.video_samples.lock().unwrap();
        samples
            .iter()
            .filter(|(_, timestamp)| *timestamp >= start_time && *timestamp <= end_time)
            .map(|(sample, timestamp)| {
                // Create a new SendableSample without pool tracking, since we don't want to return these to the pool
                let sample_clone = unsafe { sample.sample.as_ref().clone() };
                let new_sample = SendableSample::new(sample_clone);
                (new_sample, *timestamp)
            })
            .collect()
    }

    /// Get a list of audio samples within a time range
    pub fn get_audio_samples(&self, start_time: i64, end_time: i64) -> Vec<(SendableSample, i64)> {
        let samples = self.audio_samples.lock().unwrap();
        samples
            .iter()
            .filter(|(_, timestamp)| *timestamp >= start_time && *timestamp <= end_time)
            .map(|(sample, timestamp)| {
                // Create a new SendableSample without pool tracking, since we don't want to return these to the pool
                let sample_clone = unsafe { sample.sample.as_ref().clone() };
                let new_sample = SendableSample::new(sample_clone);
                (new_sample, *timestamp)
            })
            .collect()
    }

    /// Get the buffer's current duration
    pub fn current_duration(&self) -> Duration {
        let video_samples = self.video_samples.lock().unwrap();
        let audio_samples = self.audio_samples.lock().unwrap();
        
        if video_samples.is_empty() && audio_samples.is_empty() {
            return Duration::from_secs(0);
        }
        
        let mut latest_timestamp = 0;
        let oldest_timestamp = *self.oldest_timestamp.lock().unwrap();
        
        // Find the latest timestamp from video samples
        if let Some((_, timestamp)) = video_samples.back() {
            latest_timestamp = *timestamp;
        }
        
        // Check if the latest audio timestamp is newer
        if let Some((_, timestamp)) = audio_samples.back() {
            if *timestamp > latest_timestamp {
                latest_timestamp = *timestamp;
            }
        }
        
        // Convert the difference to duration
        hns_to_duration(latest_timestamp - oldest_timestamp)
    }

    /// Clear the buffer
    pub fn clear(&self) {
        let mut video_samples = self.video_samples.lock().unwrap();
        let mut audio_samples = self.audio_samples.lock().unwrap();
        let mut size_bytes = self.size_bytes.lock().unwrap();
        
        video_samples.clear();
        audio_samples.clear();
        *size_bytes = 0;
        
        debug!("Replay buffer cleared");
    }
}
