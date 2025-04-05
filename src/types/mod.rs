use log::{debug, error, info, trace};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::core::{ComInterface, Result};
use windows::Win32::Foundation::TRUE;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::Win32::Media::MediaFoundation::{
    IMFSample, IMFSinkWriter, MFCreateDXGISurfaceBuffer, MFCreateSample,
};
pub mod safe_wrapper;
pub mod texture_pool;

// Re-export TexturePool
pub use texture_pool::TexturePool;

/// A wrapper for IMFSample that can be sent between threads
pub struct SendableSample {
    pub sample: Arc<IMFSample>,
    pool: Option<Arc<SamplePool>>,
}

impl SendableSample {
    /// Create a new SendableSample without pool tracking
    pub fn new(sample: IMFSample) -> Self {
        Self {
            sample: Arc::new(sample),
            pool: None,
        }
    }

    /// Create a new SendableSample with pool tracking for auto-return
    pub fn new_pooled(sample: IMFSample, pool: Arc<SamplePool>) -> Self {
        #[cfg(debug_assertions)]
        trace!("Creating pooled SendableSample");

        Self {
            sample: Arc::new(sample),
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
        if let Some(pool) = &self.pool {
            // Only return to pool if we're the last reference to this sample
            if Arc::strong_count(&self.sample) == 1 {
                // Create local copies to avoid borrowing issues in the closure
                let pool_clone = pool.clone();
                let sample_clone = self.sample.as_ref().clone();

                // Use a thread-local to track whether we're already inside a drop to prevent cycles
                thread_local! {
                    static IN_DROP: std::cell::RefCell<bool> = std::cell::RefCell::new(false);
                }

                IN_DROP.with(|in_drop| {
                    let already_dropping = *in_drop.borrow();
                    if !already_dropping {
                        *in_drop.borrow_mut() = true;

                        // Return the sample to the pool
                        if let Err(e) = pool_clone.release_sample(sample_clone) {
                            error!("Failed to return sample to pool: {:?}", e);
                        } else {
                            #[cfg(debug_assertions)]
                            trace!("Successfully returned sample to pool");
                        }

                        *in_drop.borrow_mut() = false;
                    }
                });
            } else if cfg!(debug_assertions) {
                // Only log in debug mode and at a reasonable rate
                if Arc::strong_count(&self.sample) % 100 == 0 {
                    trace!(
                        "Not releasing sample to pool - {} references remaining",
                        Arc::strong_count(&self.sample) - 1
                    );
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct SendableWriter(pub Arc<IMFSinkWriter>);
unsafe impl Send for SendableWriter {}
unsafe impl Sync for SendableWriter {}

/// A thread-safe pool of reusable IMFSample objects
/// This maintains a simple pool of IMFSample objects that can be used with any texture
pub struct SamplePool {
    /// Mutex-protected vector of available IMFSample objects
    samples: Mutex<Vec<IMFSample>>,
    /// FPS value used for sample duration
    pub fps_num: u32,
    // Tracking for debug purposes
    #[cfg(debug_assertions)]
    created_count: std::sync::atomic::AtomicU32,
    #[cfg(debug_assertions)]
    acquired_count: std::sync::atomic::AtomicU32,
    #[cfg(debug_assertions)]
    released_count: std::sync::atomic::AtomicU32,
}

impl SamplePool {
    /// Create a new sample pool
    pub fn new(fps_num: u32, initial_capacity: usize) -> Self {
        info!("Initializing SamplePool with FPS: {}", fps_num);

        Self {
            samples: Mutex::new(Vec::with_capacity(initial_capacity)),
            fps_num,
            #[cfg(debug_assertions)]
            created_count: std::sync::atomic::AtomicU32::new(0),
            #[cfg(debug_assertions)]
            acquired_count: std::sync::atomic::AtomicU32::new(0),
            #[cfg(debug_assertions)]
            released_count: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Acquire a sample from the pool or create a new one if the pool is empty
    pub fn acquire_sample(&self) -> Result<IMFSample> {
        let mut samples = self.samples.lock().unwrap();

        let sample = if let Some(sample) = samples.pop() {
            #[cfg(debug_assertions)]
            trace!("SamplePool: Reusing sample from pool");
            sample
        } else {
            // If pool is empty, create a new empty sample
            #[cfg(debug_assertions)]
            debug!("SamplePool: Creating new sample");

            // Create a new Media Foundation sample
            let sample: IMFSample = unsafe { MFCreateSample() }?;

            #[cfg(debug_assertions)]
            {
                self.created_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }

            sample
        };

        #[cfg(debug_assertions)]
        {
            let acquired = self
                .acquired_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let created = self.created_count.load(std::sync::atomic::Ordering::SeqCst);
            let released = self
                .released_count
                .load(std::sync::atomic::Ordering::SeqCst);
            let active = acquired - released + 1;

            // Only log at larger intervals
            if acquired % 300 == 0 {
                info!(
                    "SamplePool stats - created: {}, acquired: {}, released: {}, active: {}",
                    created,
                    acquired + 1,
                    released,
                    active
                );
            }
        }

        Ok(sample)
    }

    /// Return a sample to the pool for reuse
    pub fn release_sample(&self, sample: IMFSample) -> Result<()> {
        #[cfg(debug_assertions)]
        trace!("SamplePool: Returning sample to pool");

        // Clear all buffers from the sample before returning it to the pool
        unsafe { sample.RemoveAllBuffers()? };

        let mut samples = self.samples.lock().unwrap();
        samples.push(sample);

        #[cfg(debug_assertions)]
        {
            let released = self
                .released_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            // Only log occasionally to avoid spamming
            if released % 300 == 0 {
                let acquired = self
                    .acquired_count
                    .load(std::sync::atomic::Ordering::SeqCst);
                let active = acquired - (released + 1);
                debug!(
                    "SamplePool: After release - {} samples still active",
                    active
                );
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
    pub fn new(
        max_duration: Duration,
        initial_video_limit: usize,
        initial_audio_limit: usize,
    ) -> Self {
        info!(
            "Creating replay buffer with max duration of {:?}",
            max_duration
        );
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
    fn trim_buffer(
        &self,
        samples: &mut VecDeque<(SendableSample, i64)>,
        latest_timestamp: i64,
    ) -> Result<()> {
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
