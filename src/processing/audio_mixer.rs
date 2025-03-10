use std::collections::VecDeque;
use windows::core::Result;
use windows::Win32::Media::MediaFoundation::{IMFSample, MFCreateSample, MFCreateMemoryBuffer};
use std::sync::Arc;
use crate::types::SendableSample;
use log::{error, warn, info, debug, trace};

// Enhanced SendableSample that stores both the IMFSample and its timestamp
pub struct TimestampedSample(Arc<IMFSample>, i64);

static mut GOOD_MATCH_COUNT: usize = 0;
static mut NO_MATCH_COUNT: usize = 0;
static mut SYS_ONLY_COUNT: usize = 0;
static mut MIC_ONLY_COUNT: usize = 0;
static mut CALL_COUNT: u32 = 0;
static mut TOTAL_PROCESSED_DURATION: i64 = 0;

static mut SYSTEM_TOTAL_DURATION: i64 = 0;
static mut MIC_TOTAL_DURATION: i64 = 0;
static mut TIMESTAMP_DRIFT: i64 = 0;

static mut LAST_SYS_TIME: i64 = 0;
static mut LAST_MIC_TIME: i64 = 0;

static mut MIN_TIMESTAMP: i64 = i64::MAX;
static mut MAX_TIMESTAMP: i64 = 0;
static mut REAL_TIMELINE_DURATION: i64 = 0;

pub struct AudioMixer {
    system_audio_queue: VecDeque<TimestampedSample>,
    microphone_queue: VecDeque<TimestampedSample>,
    sample_rate: u32,
    channels: u16,
    max_buffer_size: usize,  // Maximum number of samples to buffer
    max_time_diff_ns: i64,   // Maximum acceptable time difference in 100ns units
}

impl AudioMixer {
    pub fn new(sample_rate: u32, bits_per_sample: u16, channels: u16) -> Self {
        info!("Creating AudioMixer: sample_rate={}, bits_per_sample={}, channels={}", 
              sample_rate, bits_per_sample, channels);
        
        // Default to buffering 10 samples, and a max difference of 20ms (200,000 in 100ns units)
        Self {
            system_audio_queue: VecDeque::new(),
            microphone_queue: VecDeque::new(),
            sample_rate,
            channels,
            max_buffer_size: 10,
            max_time_diff_ns: 300_000,
        }
    }

    // Configure buffer size and timing parameters
    pub fn set_buffer_params(&mut self, max_buffer_size: usize, max_time_diff_ms: f64) {
        self.max_buffer_size = max_buffer_size;
        // Convert ms to 100ns units (Windows Media Foundation time units)
        self.max_time_diff_ns = (max_time_diff_ms * 10_000.0) as i64;
        info!("Set buffer params: max_buffer_size={}, max_time_diff_ms={}, max_time_diff_ns={}", 
              max_buffer_size, max_time_diff_ms, self.max_time_diff_ns);
    }

    // Add system audio sample with timestamp extraction
    pub unsafe fn add_system_audio(&mut self, sample: SendableSample) {
        // Extract timestamp from the sample
        let timestamp = match sample.0.GetSampleTime() {
            Ok(time) => time,
            Err(e) => {
                error!("Failed to get system sample timestamp: {:?}", e);
                0 // Default to 0 if we can't get the timestamp
            }
        };

        if let Ok(duration) = sample.0.GetSampleDuration() {
            SYSTEM_TOTAL_DURATION += duration;
        }
        LAST_SYS_TIME = timestamp;

        // Create timestamped sample and add to queue
        let timestamped = TimestampedSample(sample.0, timestamp);
        trace!("Adding system audio sample to queue (queue size: {}, timestamp: {})", 
               self.system_audio_queue.len(), timestamp);
        
        self.system_audio_queue.push_back(timestamped);
        
        // Trim buffer if it grows too large
        self.trim_queues();
    }

    // Add microphone audio sample with timestamp extraction
    pub unsafe fn add_microphone_audio(&mut self, sample: SendableSample) {
        // Extract timestamp from the sample
        let timestamp = match sample.0.GetSampleTime() {
            Ok(time) => time,
            Err(e) => {
                error!("Failed to get microphone sample timestamp: {:?}", e);
                0 // Default to 0 if we can't get the timestamp
            }
        };

        if let Ok(duration) = sample.0.GetSampleDuration() {
            MIC_TOTAL_DURATION += duration;
        }

        LAST_MIC_TIME = timestamp;

        // Create timestamped sample and add to queue
        let timestamped = TimestampedSample(sample.0, timestamp);
        trace!("Adding microphone audio sample to queue (queue size: {}, timestamp: {})", 
               self.microphone_queue.len(), timestamp);
        
        self.microphone_queue.push_back(timestamped);
        
        // Trim buffer if it grows too large
        self.trim_queues();
    }

    // Trim queues if they exceed max buffer size
    fn trim_queues(&mut self) {
        while self.system_audio_queue.len() > self.max_buffer_size {
            if let Some(sample) = self.system_audio_queue.pop_front() {
                debug!("Trimming oldest system audio sample (timestamp: {})", sample.1);
            }
        }
        
        while self.microphone_queue.len() > self.max_buffer_size {
            if let Some(sample) = self.microphone_queue.pop_front() {
                debug!("Trimming oldest microphone audio sample (timestamp: {})", sample.1);
            }
        }
    }

    // Find the best matching pair of samples based on timestamps
    fn find_best_sample_match(&self) -> Option<(usize, usize)> {
        if self.system_audio_queue.is_empty() || self.microphone_queue.is_empty() {
            return None;
        }
        
        let mut best_match = None;
        let mut smallest_diff = i64::MAX;
        
        // Find the pair of samples with the closest timestamps
        for (sys_idx, sys_sample) in self.system_audio_queue.iter().enumerate() {
            for (mic_idx, mic_sample) in self.microphone_queue.iter().enumerate() {
                let diff = (sys_sample.1 - mic_sample.1).abs();
                
                // If we find a better match, update our tracking
                if diff < smallest_diff {
                    smallest_diff = diff;
                    best_match = Some((sys_idx, mic_idx));
                    
                    // If the difference is very small, we can stop early
                    if diff < 1000 { // Less than 0.1ms difference
                        return best_match;
                    }
                }
            }
        }
        
        // Check if the best match is within acceptable time difference
        if let Some((sys_idx, mic_idx)) = best_match {
            let sys_time = self.system_audio_queue[sys_idx].1;
            let mic_time = self.microphone_queue[mic_idx].1;
            let current_drift = sys_time - mic_time;

            // Log significant changes in drift
            if (current_drift - unsafe {TIMESTAMP_DRIFT}).abs() > 50_000 {  // 5ms change
                info!("Timestamp drift changed: previous={}ms, current={}ms, delta={}ms",
                    unsafe {TIMESTAMP_DRIFT as f64 / 10_000.0 },
                    current_drift as f64 / 10_000.0,
                    (current_drift - unsafe {TIMESTAMP_DRIFT}) as f64 / 10_000.0);
                unsafe { TIMESTAMP_DRIFT = current_drift };
            }
            let diff = (self.system_audio_queue[sys_idx].1 - self.microphone_queue[mic_idx].1).abs();
            
            if diff <= self.max_time_diff_ns {
                return best_match;
            } else {
                debug!("Best match time difference ({}) exceeds maximum allowable ({})", 
                       diff, self.max_time_diff_ns);
                return None;
            }
        }
        
        None
    }

    pub unsafe fn process_next_sample(&mut self) -> Option<Result<Arc<IMFSample>>> {

        if self.system_audio_queue.is_empty() || self.microphone_queue.is_empty() {
            // Optionally, you can wait a short duration here if needed
            return None;
        }
    
        // Increment call count
        CALL_COUNT += 1;
    
        debug!("Processing next sample - sys queue: {}, mic queue: {}", 
               self.system_audio_queue.len(), self.microphone_queue.len());
    
        // Log queue sizes periodically
        if CALL_COUNT % 50 == 0 {
            info!("Call #{}: Queue sizes - sys: {}, mic: {}, total duration: {:.3}s, paths: good={}, no_match={}, sys_only={}, mic_only={}", 
                CALL_COUNT, 
                self.system_audio_queue.len(), 
                self.microphone_queue.len(),
                TOTAL_PROCESSED_DURATION as f64 / 10_000_000.0,
                GOOD_MATCH_COUNT,
                NO_MATCH_COUNT,
                SYS_ONLY_COUNT,
                MIC_ONLY_COUNT);
        }
        
        // Handle empty queue cases
        if self.system_audio_queue.is_empty() && self.microphone_queue.is_empty() {
            debug!("Both audio queues empty, returning None");
            return None;
        }
        
        // If either queue is empty, just return from the non-empty one
        if self.system_audio_queue.is_empty() {
            debug!("Only microphone audio available, passing through");
            let mic_sample = self.microphone_queue.pop_front()?;
            
            // Track mic-only samples
            MIC_ONLY_COUNT += 1;
            
            // Add duration tracking
            if let Ok(time) = mic_sample.0.GetSampleTime() {
                if let Ok(duration) = mic_sample.0.GetSampleDuration() {
                    // Update timeline bounds
                    if time < unsafe { MIN_TIMESTAMP } {
                        unsafe { MIN_TIMESTAMP = time; }
                    }
                    let end_time = time + duration;
                    if end_time > unsafe { MAX_TIMESTAMP } {
                        unsafe { MAX_TIMESTAMP = end_time; }
                        unsafe { REAL_TIMELINE_DURATION = MAX_TIMESTAMP - MIN_TIMESTAMP; }
                    }
                    
                    // Still track total for debugging
                    TOTAL_PROCESSED_DURATION += duration;
                    
                    if CALL_COUNT % 100 == 0 || MIC_ONLY_COUNT < 5 {
                        debug!("Mic-only sample #{}: duration={}, timeline={:.3}s, total accumulated={:.3}s", 
                             MIC_ONLY_COUNT, duration, 
                             REAL_TIMELINE_DURATION as f64 / 10_000_000.0,
                             TOTAL_PROCESSED_DURATION as f64 / 10_000_000.0);
                    }
                }
            }
            
            return Some(Ok(mic_sample.0));
        }
        
        if self.microphone_queue.is_empty() {
            debug!("Only system audio available, passing through");
            let sys_sample = self.system_audio_queue.pop_front()?;
            
            // Track system-only samples
            SYS_ONLY_COUNT += 1;
            
            // Add duration tracking
            if let Ok(time) = sys_sample.0.GetSampleTime() {
                if let Ok(duration) = sys_sample.0.GetSampleDuration() {
                    // Update timeline bounds
                    if time < unsafe { MIN_TIMESTAMP } {
                        unsafe { MIN_TIMESTAMP = time; }
                    }
                    let end_time = time + duration;
                    if end_time > unsafe { MAX_TIMESTAMP } {
                        unsafe { MAX_TIMESTAMP = end_time; }
                        unsafe { REAL_TIMELINE_DURATION = MAX_TIMESTAMP - MIN_TIMESTAMP; }
                    }
                    
                    // Still track total for debugging
                    TOTAL_PROCESSED_DURATION += duration;
                    
                    if CALL_COUNT % 100 == 0 || SYS_ONLY_COUNT < 5 {
                        debug!("Sys-only sample #{}: duration={}, timeline={:.3}s, total accumulated={:.3}s", 
                             SYS_ONLY_COUNT, duration, 
                             REAL_TIMELINE_DURATION as f64 / 10_000_000.0,
                             TOTAL_PROCESSED_DURATION as f64 / 10_000_000.0);
                    }
                }
            }
            
            return Some(Ok(sys_sample.0));
        }
        
        // Try to find the best matching pair of samples
        if let Some((sys_idx, mic_idx)) = self.find_best_sample_match() {
            debug!("Found matching samples at indices: sys={}, mic={}", sys_idx, mic_idx);
            
            // Track good matches
            GOOD_MATCH_COUNT += 1;
            
            // Get the timestamp difference for logging
            let sys_time = self.system_audio_queue[sys_idx].1;
            let mic_time = self.microphone_queue[mic_idx].1;
            debug!("Sample times - Sys: {}, Mic: {}, Diff: {}", 
                   sys_time, mic_time, sys_time - mic_time);
            
            // Remove the samples from the queues
            // Note: We need to remove mic_idx first if mic_idx < sys_idx to ensure
            // indices remain valid after the first removal
            let (sys_sample, mic_sample) = if mic_idx < sys_idx {
                let mic = self.microphone_queue.remove(mic_idx).unwrap();
                let sys = self.system_audio_queue.remove(sys_idx - 1).unwrap(); // Adjust index
                (sys, mic)
            } else {
                let sys = self.system_audio_queue.remove(sys_idx).unwrap();
                let mic = self.microphone_queue.remove(mic_idx).unwrap();
                (sys, mic)
            };
            
            // Mix the samples
            match self.mix_samples(&sys_sample.0, &mic_sample.0) {
                Ok(mixed) => {
                    // Get the duration and add to total
                    if let Ok(duration) = mixed.GetSampleDuration() {
                        TOTAL_PROCESSED_DURATION += duration;
                        if CALL_COUNT % 100 == 0 || GOOD_MATCH_COUNT < 5 {
                            debug!("Good match #{}: duration={}, total={:.3}s", 
                                 GOOD_MATCH_COUNT, duration, 
                                 TOTAL_PROCESSED_DURATION as f64 / 10_000_000.0);
                        }
                    }
                    
                    debug!("Successfully mixed audio samples");
                    Some(Ok(Arc::new(mixed)))
                },
                Err(e) => {
                    error!("Error mixing audio samples: {:?}", e);
                    Some(Err(e))
                },
            }
        } else {
            // If no good match found, use the oldest samples from each queue
            debug!("No good timestamp match found, using oldest samples");
            
            // Track no-match cases
            NO_MATCH_COUNT += 1;
            
            let sys_sample = self.system_audio_queue.pop_front().unwrap();
            let mic_sample = self.microphone_queue.pop_front().unwrap();
            
            // Log the timestamp difference
            debug!("Sample times - Sys: {}, Mic: {}, Diff: {}", 
                   sys_sample.1, mic_sample.1, sys_sample.1 - mic_sample.1);
            
            // Mix the samples
            match self.mix_samples(&sys_sample.0, &mic_sample.0) {
                Ok(mixed) => {
                    if let Ok(duration) = mixed.GetSampleDuration() {
                        TOTAL_PROCESSED_DURATION += duration;
                        if CALL_COUNT % 100 == 0 || NO_MATCH_COUNT < 5 {
                            debug!("No-match #{}: duration={}, total={:.3}s", 
                                 NO_MATCH_COUNT, duration, 
                                 TOTAL_PROCESSED_DURATION as f64 / 10_000_000.0);
                        }
                    }
                    debug!("Successfully mixed audio samples");
                    Some(Ok(Arc::new(mixed)))
                },
                Err(e) => {
                    error!("Error mixing audio samples: {:?}", e);
                    Some(Err(e))
                },
            }
        }
    }

    unsafe fn mix_samples(&self, sys_sample: &IMFSample, mic_sample: &IMFSample) -> Result<IMFSample> {
        // Get timing info from both samples for logging
        let sys_time = sys_sample.GetSampleTime()?;
        let sys_duration = sys_sample.GetSampleDuration()?;
        let mic_time = mic_sample.GetSampleTime()?;
        let mic_duration = mic_sample.GetSampleDuration()?;
        
        // Log timing differences
        /*info!("Sample durations - Sys: {}ns ({}ms), Mic: {}ns ({}ms), Ratio: {:.4}", 
            sys_duration, sys_duration as f64 / 10_000.0,
            mic_duration, mic_duration as f64 / 10_000.0,
            sys_duration as f64 / mic_duration as f64);*/
        
        // Get sample time and duration from system audio for the output
        let sample_time = sys_sample.GetSampleTime()?;
        let sample_duration = sys_sample.GetSampleDuration()?;
        
        // Get the buffers from both samples
        let sys_buffer = sys_sample.GetBufferByIndex(0)?;
        let mic_buffer = mic_sample.GetBufferByIndex(0)?;
        
        // Get the data from the buffers
        let mut sys_data: *mut u8 = std::ptr::null_mut();
        let mut mic_data: *mut u8 = std::ptr::null_mut();
        let mut sys_length: u32 = 0;
        let mut mic_length: u32 = 0;
        
        // Lock and process buffers
        sys_buffer.Lock(&mut sys_data, None, Some(&mut sys_length))?;
        
        let result = (|| {
            // Lock microphone buffer
            if let Err(e) = mic_buffer.Lock(&mut mic_data, None, Some(&mut mic_length)) {
                error!("Failed to lock microphone buffer: {:?}", e);
                return Err(e);
            }
            
            debug!("Buffer sizes - System: {} bytes, Microphone: {} bytes", sys_length, mic_length);
            
            // Create a new sample and buffer for the mixed audio
            let output_sample = match MFCreateSample() {
                Ok(sample) => sample,
                Err(e) => {
                    error!("Failed to create output sample: {:?}", e);
                    let _ = mic_buffer.Unlock(); // Ignore unlock error here
                    return Err(e);
                }
            };
            
            let output_buffer = match MFCreateMemoryBuffer(sys_length) {
                Ok(buffer) => buffer,
                Err(e) => {
                    error!("Failed to create output buffer: {:?}", e);
                    let _ = mic_buffer.Unlock(); // Ignore unlock error here
                    return Err(e);
                }
            };
            
            // Lock output buffer
            let mut output_data: *mut u8 = std::ptr::null_mut();
            if let Err(e) = output_buffer.Lock(&mut output_data, None, None) {
                error!("Failed to lock output buffer: {:?}", e);
                let _ = mic_buffer.Unlock(); // Ignore unlock error here
                return Err(e);
            }
            
            // Mix the audio
            if let Err(e) = self.mix_pcm_audio(
                sys_data, 
                mic_data, 
                output_data, 
                sys_length, 
                mic_length
            ) {
                error!("Failed to mix audio: {:?}", e);
                let _ = mic_buffer.Unlock(); // Ignore unlock error here
                let _ = output_buffer.Unlock(); // Ignore unlock error here
                return Err(e);
            }
            
            // Unlock microphone buffer
            if let Err(e) = mic_buffer.Unlock() {
                error!("Failed to unlock microphone buffer: {:?}", e);
                let _ = output_buffer.Unlock(); // Ignore unlock error here
                return Err(e);
            }
            
            // Set buffer length and unlock output buffer
            if let Err(e) = output_buffer.SetCurrentLength(sys_length) {
                error!("Failed to set output buffer length: {:?}", e);
                let _ = output_buffer.Unlock(); // Ignore unlock error here
                return Err(e);
            }
            
            if let Err(e) = output_buffer.Unlock() {
                error!("Failed to unlock output buffer: {:?}", e);
                return Err(e);
            }
            
            // Add buffer to sample and set timing info
            if let Err(e) = output_sample.AddBuffer(&output_buffer) {
                error!("Failed to add buffer to output sample: {:?}", e);
                return Err(e);
            }
            
            if let Err(e) = output_sample.SetSampleTime(sample_time) {
                error!("Failed to set output sample time: {:?}", e);
                return Err(e);
            }
            
            if let Err(e) = output_sample.SetSampleDuration(sample_duration) {
                error!("Failed to set output sample duration: {:?}", e);
                return Err(e);
            }
            
            Ok(output_sample)
        })();
        
        // Always unlock system buffer
        let sys_unlock_result = sys_buffer.Unlock();
        
        // Return the mixing result or propagate error
        match (result, sys_unlock_result) {
            (Ok(output_sample), Ok(())) => Ok(output_sample),
            (Err(e), _) => Err(e),
            (_, Err(e)) => Err(e),
        }
    }
    
    // Mixing function with fixed 50/50 mix
    // Modify the mix_pcm_audio function to add detailed logging

unsafe fn mix_pcm_audio(
    &self,
    sys_data: *mut u8,     // System audio (16-bit PCM)
    mic_data: *mut u8,     // Microphone audio (16-bit PCM)
    output_data: *mut u8,  // Output buffer (16-bit PCM)
    sys_length: u32,
    mic_length: u32
) -> Result<()> {
    // Define mixing ratios (can be adjusted as needed)
    let sys_ratio = 0.5;
    let mic_ratio = 0.5;
    
    // Access the audio data as 16-bit PCM
    let sys_samples = std::slice::from_raw_parts(
        sys_data as *const i16,
        sys_length as usize / 2
    );
    
    let mic_samples = std::slice::from_raw_parts(
        mic_data as *const i16,
        mic_length as usize / 2
    );
    
    // Get output as mutable slice of i16
    let output_samples = std::slice::from_raw_parts_mut(
        output_data as *mut i16, 
        sys_length as usize / 2  // Use system length for output
    );
    
    // Determine how many samples to mix
    let mix_len = std::cmp::min(
        sys_samples.len(),
        std::cmp::min(
            mic_samples.len(),
            output_samples.len()
        )
    );
    
    // Log current timestamp and mixing details
    let current_timestamp = unsafe { LAST_SYS_TIME };
    info!("Mixing audio at timestamp {}ms ({:.3}s)", 
           current_timestamp as f64 / 10_000.0,
           current_timestamp as f64 / 10_000_000.0);
    
    // Log detailed mixing statistics
    let sample_rate_float = self.sample_rate as f64;
    let duration_seconds = mix_len as f64 / sample_rate_float;
    let samples_per_ms = sample_rate_float / 1000.0;
    
    info!("Writing {} mixed samples ({:.2}ms audio at {}Hz, {} channels)",
           mix_len,
           duration_seconds * 1000.0,
           self.sample_rate,
           self.channels);
    
    // Audio level statistics
    let mut sys_max: i16 = 0;
    let mut mic_max: i16 = 0;
    let mut output_max: i16 = 0;
    let mut sys_sum: f64 = 0.0;
    let mut mic_sum: f64 = 0.0;
    let mut output_sum: f64 = 0.0;
    
    // Log a sample of the audio data at regular intervals
    let log_interval = (sample_rate_float / 10.0) as usize; // Log every 0.1 seconds of audio
    let log_count = (mix_len / log_interval).max(1);
    
    debug!("Mixing {} samples with ratio system:{:.1} mic:{:.1}", 
           mix_len, sys_ratio, mic_ratio);
    
    // Mix the samples
    for i in 0..mix_len {
        // Update audio level statistics
        sys_max = sys_max.max(sys_samples[i].abs());
        mic_max = mic_max.max(mic_samples[i].abs());
        sys_sum += sys_samples[i].abs() as f64;
        mic_sum += mic_samples[i].abs() as f64;
        
        // Simple weighted average
        let mixed_val = (sys_samples[i] as f32 * sys_ratio + 
                        mic_samples[i] as f32 * mic_ratio) as i32;
        
        // Clamp to i16 range
        let clamped = mixed_val.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        
        // Update output statistics
        output_max = output_max.max(clamped.abs());
        output_sum += clamped.abs() as f64;
        
        // Write to output
        output_samples[i] = clamped;
        
        // Log sample values at intervals
        if log_count > 0 && i % (mix_len / log_count) == 0 {
            let timestamp_ms = current_timestamp as f64 / 10_000.0 + (i as f64 / samples_per_ms);
            
            info!("Sample at {:.3}ms: sys={}, mic={}, out={}", 
                 timestamp_ms,
                 sys_samples[i],
                 mic_samples[i],
                 clamped);
        }
    }
    
    // If sys_samples is longer than mix_len, fill the rest with system audio
    if sys_samples.len() > mix_len {
        info!("Adding {} additional system-only samples", sys_samples.len() - mix_len);
        
        for i in mix_len..std::cmp::min(sys_samples.len(), output_samples.len()) {
            // Reduce volume slightly since we're not mixing
            let scaled = (sys_samples[i] as f32 * 0.8) as i16;
            output_samples[i] = scaled;
            output_max = output_max.max(scaled.abs());
        }
    }
    
    // Calculate average levels
    let sys_avg = if mix_len > 0 { sys_sum / mix_len as f64 } else { 0.0 };
    let mic_avg = if mix_len > 0 { mic_sum / mix_len as f64 } else { 0.0 };
    let output_avg = if mix_len > 0 { output_sum / mix_len as f64 } else { 0.0 };
    
    // Log audio level statistics
    /*info!("Audio levels - System: avg={:.1}, peak={} ({}%), Mic: avg={:.1}, peak={} ({}%), Output: avg={:.1}, peak={} ({}%)",
         sys_avg, 
         sys_max, 
         (sys_max as f64 / i16::MAX as f64 * 100.0) as i32,
         mic_avg, 
         mic_max, 
         (mic_max as f64 / i16::MAX as f64 * 100.0) as i32,
         output_avg,
         output_max,
         (output_max as f64 / i16::MAX as f64 * 100.0) as i32);*/
    
    // Log end timestamp
    let end_timestamp = current_timestamp + (duration_seconds * 10_000_000.0) as i64;
    info!("Completed mixing audio from {}ms to {}ms (duration: {:.2}ms)", 
           current_timestamp as f64 / 10_000.0,
           end_timestamp as f64 / 10_000.0,
           duration_seconds * 1000.0);
    
    debug!("PCM mixing completed successfully");
    Ok(())
}

    pub unsafe fn flush_queues(&mut self) -> (i64, usize, usize, usize, usize, usize, i64, i64, i64, i64, i64) {
        // Clear both queues
        let sys_count = self.system_audio_queue.len();
        let mic_count = self.microphone_queue.len();
        
        self.system_audio_queue.clear();
        self.microphone_queue.clear();
        
        // Return diagnostics (all counters)
        let (
            total_duration, 
            real_timeline,
            good_matches, 
            no_matches, 
            sys_only, 
            mic_only, 
            call_count,
            system_total_duration,
            mic_total_duration,
            timestamp_drift,
            last_sys_time,
            last_mic_time
        ) = (
            TOTAL_PROCESSED_DURATION,
            REAL_TIMELINE_DURATION,
            GOOD_MATCH_COUNT,
            NO_MATCH_COUNT,
            SYS_ONLY_COUNT, 
            MIC_ONLY_COUNT,
            CALL_COUNT,
            SYSTEM_TOTAL_DURATION,
            MIC_TOTAL_DURATION,
            TIMESTAMP_DRIFT,
            LAST_SYS_TIME,
            LAST_MIC_TIME
        );
        
        info!("Audio mixer flushed - Stats summary: real timeline={:.3}s, accumulated duration={:.3}s, calls={}", 
              real_timeline as f64 / 10_000_000.0,
              total_duration as f64 / 10_000_000.0, 
              call_count);
              
        info!("  Sample counts: good_matches={}, no_matches={}, sys_only={}, mic_only={}, leftover={}",
              good_matches, no_matches, sys_only, mic_only, sys_count + mic_count);
              
        info!("  Timing stats: min_time={}ms, max_time={}ms, system_total={:.3}s, mic_total={:.3}s, drift={}ms",
              MIN_TIMESTAMP as f64 / 10_000.0,
              MAX_TIMESTAMP as f64 / 10_000.0,
              system_total_duration as f64 / 10_000_000.0,
              mic_total_duration as f64 / 10_000_000.0,
              timestamp_drift as f64 / 10_000.0);
              
        info!("  Last timestamps: system={}ms, mic={}ms",
              last_sys_time as f64 / 10_000.0,
              last_mic_time as f64 / 10_000.0);
        
        (
            real_timeline,  // Return real timeline duration instead of accumulated
            good_matches, 
            no_matches, 
            sys_only, 
            mic_only, 
            call_count.try_into().unwrap(),
            system_total_duration,
            mic_total_duration,
            timestamp_drift,
            last_sys_time,
            last_mic_time
        )
    }
}