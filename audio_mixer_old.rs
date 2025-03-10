use std::collections::VecDeque;
use windows::core::Result;
use windows::Win32::Media::MediaFoundation::{IMFSample, MFCreateSample, MFCreateMemoryBuffer};
use std::sync::Arc;
use crate::types::SendableSample;
use log::{error, warn, info, debug, trace};

pub struct AudioMixer {
    system_audio_queue: VecDeque<SendableSample>,
    microphone_queue: VecDeque<SendableSample>,
    sample_rate: u32,
    channels: u16,
    max_time_diff_ns: i64,  // Maximum allowable time difference in nanoseconds
    buffer_duration_ns: i64, // Maximum buffer size in time (to prevent unbounded growth)
}

impl AudioMixer {
    pub fn new(sample_rate: u32, bits_per_sample: u16, channels: u16) -> Self {
        info!("Creating AudioMixer: sample_rate={}, bits_per_sample={}, channels={}", 
              sample_rate, bits_per_sample, channels);
        Self {
            system_audio_queue: VecDeque::new(),
            microphone_queue: VecDeque::new(),
            sample_rate,
            channels,
            max_time_diff_ns: 10 * 1000 * 1000,
            // Keep up to 500ms of audio in buffer before discarding
            buffer_duration_ns: 500 * 1000 * 1000,
        }
    }

    pub fn add_system_audio(&mut self, sample: SendableSample) {
        trace!("Adding system audio sample to queue (queue size: {})", self.system_audio_queue.len());
        self.system_audio_queue.push_back(sample);
    }

    pub fn add_microphone_audio(&mut self, sample: SendableSample) {
        trace!("Adding microphone audio sample to queue (queue size: {})", self.microphone_queue.len());
        self.microphone_queue.push_back(sample);
    }

    pub unsafe fn get_sample_timestamp(&self, sample: &IMFSample) -> Result<i64> {
        match sample.GetSampleTime() {
            Ok(time) => Ok(time),
            Err(e) => {
                error!("Failed to get sample time: {:?}", e);
                Err(e)
            }
        }
    }

    unsafe fn find_matching_samples(&mut self) -> Option<Result<(SendableSample, SendableSample)>> {
        if self.system_audio_queue.is_empty() || self.microphone_queue.is_empty() {
            return None;
        }
    
        // Get timestamps for samples at the front of both queues
        let sys_sample = &self.system_audio_queue[0];
        let mic_sample = &self.microphone_queue[0];
        
        let sys_time = match self.get_sample_timestamp(&sys_sample.0) {
            Ok(time) => time,
            Err(e) => return Some(Err(e)),
        };
        
        let mic_time = match self.get_sample_timestamp(&mic_sample.0) {
            Ok(time) => time,
            Err(e) => return Some(Err(e)),
        };

        // Log the timestamps of the samples we're trying to match
        info!("Sample timing - System: {}ns, Microphone: {}ns, Difference: {}ns", 
        sys_time, mic_time, (sys_time - mic_time).abs());
        
        let time_diff = (sys_time - mic_time).abs();
        
        // If the timestamps are close enough, use these samples
        if time_diff <= self.max_time_diff_ns {
            // Remove and return the matched samples
            let sys = self.system_audio_queue.pop_front().unwrap();
            let mic = self.microphone_queue.pop_front().unwrap();
            return Some(Ok((sys, mic)));
        }
        
        // If system audio is ahead, we need to find/wait for a matching microphone sample
        if sys_time > mic_time {
            // Search for a microphone sample that's closer in time
            for i in 1..self.microphone_queue.len() {
                let next_mic_sample = &self.microphone_queue[i];
                let next_mic_time = match self.get_sample_timestamp(&next_mic_sample.0) {
                    Ok(time) => time,
                    Err(e) => return Some(Err(e)),
                };
                
                let next_diff = (sys_time - next_mic_time).abs();
                if next_diff <= self.max_time_diff_ns {
                    // Found a better match
                    let sys = self.system_audio_queue.pop_front().unwrap();
                    // Remove all microphone samples up to and including the matching one
                    let mic = self.microphone_queue.remove(i).unwrap();
                    // Discard earlier microphone samples that we're skipping
                    for _ in 0..i {
                        self.microphone_queue.pop_front();
                    }
                    debug!("Found matching mic sample at index {}, time diff: {}ns", i, next_diff);
                    return Some(Ok((sys, mic)));
                }
            }
            
            // If we couldn't find a matching microphone sample, discard the oldest system audio
            // to prevent unbounded queue growth
            if self.system_audio_queue.len() > 20 {  // Arbitrary limit to prevent large searches
                warn!("Discarding system audio sample with timestamp {} due to no matching mic sample", sys_time);
                self.system_audio_queue.pop_front();
            }
        } else {
            // Microphone is ahead, try to find a matching system audio sample
            // Similar logic as above but for system audio queue
            for i in 1..self.system_audio_queue.len() {
                let next_sys_sample = &self.system_audio_queue[i];
                let next_sys_time = match self.get_sample_timestamp(&next_sys_sample.0) {
                    Ok(time) => time,
                    Err(e) => return Some(Err(e)),
                };
                
                let next_diff = (next_sys_time - mic_time).abs();
                if next_diff <= self.max_time_diff_ns {
                    // Found a better match
                    // Remove all system samples up to and including the matching one
                    let sys = self.system_audio_queue.remove(i).unwrap();
                    let mic = self.microphone_queue.pop_front().unwrap();
                    // Discard earlier system samples
                    for _ in 0..i {
                        self.system_audio_queue.pop_front();
                    }
                    debug!("Found matching sys sample at index {}, time diff: {}ns", i, next_diff);
                    return Some(Ok((sys, mic)));
                }
            }
            
            // If we couldn't find a matching system sample, discard the oldest microphone sample
            if self.microphone_queue.len() > 20 {
                warn!("Discarding microphone sample with timestamp {} due to no matching system audio", mic_time);
                self.microphone_queue.pop_front();
            }
        }
        
        // No suitable match found yet, return None to wait for more samples
        None
    }

    unsafe fn discard_stale_samples(&mut self) {
        //info!("Starting discard_stale_samples check");
        
        if self.system_audio_queue.is_empty() || self.microphone_queue.is_empty() {
            //info!("Either system audio queue or microphone queue is empty, skipping");
            return;
        }
        
        // Get the newest sample time from either queue
        let newest_sys_time = match self.get_sample_timestamp(&self.system_audio_queue.back().unwrap().0) {
            Ok(time) => {
                info!("Newest system audio sample timestamp: {}", time);
                time
            },
            Err(e) => {
                info!("Failed to get timestamp for newest system audio sample: {:?}", e);
                return;
            },
        };
        
        let newest_mic_time = match self.get_sample_timestamp(&self.microphone_queue.back().unwrap().0) {
            Ok(time) => {
                info!("Newest microphone sample timestamp: {}", time);
                time
            },
            Err(e) => {
                info!("Failed to get timestamp for newest microphone sample: {:?}", e);
                return;
            },
        };
        
        let newest_time = newest_sys_time.max(newest_mic_time);
        let oldest_allowed_time = newest_time - self.buffer_duration_ns;
        info!("Newest overall timestamp: {}, oldest allowed: {}, buffer duration: {} ns", 
              newest_time, oldest_allowed_time, self.buffer_duration_ns);
        
        // Discard system audio samples that are too old
        let initial_sys_count = self.system_audio_queue.len();
        let mut discarded_sys_count = 0;
        
        while !self.system_audio_queue.is_empty() {
            let sample_time = match self.get_sample_timestamp(&self.system_audio_queue[0].0) {
                Ok(time) => time,
                Err(e) => {
                    info!("Failed to get timestamp for oldest system audio sample: {:?}", e);
                    break;
                },
            };
            
            if sample_time < oldest_allowed_time {
                warn!("Discarding stale system audio sample: {} is older than cutoff {}", 
                      sample_time, oldest_allowed_time);
                self.system_audio_queue.pop_front();
                discarded_sys_count += 1;
            } else {
                break;
            }
        }
        
        // Discard microphone samples that are too old
        let initial_mic_count = self.microphone_queue.len();
        let mut discarded_mic_count = 0;
        
        while !self.microphone_queue.is_empty() {
            let sample_time = match self.get_sample_timestamp(&self.microphone_queue[0].0) {
                Ok(time) => time,
                Err(e) => {
                    info!("Failed to get timestamp for oldest microphone sample: {:?}", e);
                    break;
                },
            };
            
            if sample_time < oldest_allowed_time {
                warn!("Discarding stale microphone sample: {} is older than cutoff {}", 
                      sample_time, oldest_allowed_time);
                self.microphone_queue.pop_front();
                discarded_mic_count += 1;
            } else {
                break;
            }
        }
        
        info!("Finished discard_stale_samples: discarded {}/{} system audio samples, {}/{} microphone samples", 
              discarded_sys_count, initial_sys_count, discarded_mic_count, initial_mic_count);
    }

    pub unsafe fn process_next_sample(&mut self) -> Option<Result<Arc<IMFSample>>> {
        debug!("Processing next sample - sys queue: {}, mic queue: {}", 
               self.system_audio_queue.len(), self.microphone_queue.len());
        
        // Manage buffer first to prevent unbounded growth
        self.discard_stale_samples();
        
        // If either queue is empty, just return from the non-empty one
        if self.system_audio_queue.is_empty() && !self.microphone_queue.is_empty() {
            debug!("Only microphone audio available, passing through");
            return Some(Ok(self.microphone_queue.pop_front()?.0));
        }
        
        if !self.system_audio_queue.is_empty() && self.microphone_queue.is_empty() {
            debug!("Only system audio available, passing through");
            return Some(Ok(self.system_audio_queue.pop_front()?.0));
        }
        
        if self.system_audio_queue.is_empty() && self.microphone_queue.is_empty() {
            debug!("Both audio queues empty, returning None");
            return None;
        }
        
        // Both queues have samples - find matching pair based on timestamps
        match self.find_matching_samples() {
            Some(Ok((sys_sample, mic_sample))) => {
                // Mix the samples
                match self.mix_samples(&sys_sample.0, &mic_sample.0) {
                    Ok(mixed) => {
                        debug!("Successfully mixed synchronized audio samples");
                        Some(Ok(Arc::new(mixed)))
                    },
                    Err(e) => {
                        error!("Error mixing audio samples: {:?}", e);
                        Some(Err(e))
                    },
                }
            },
            Some(Err(e)) => Some(Err(e)),
            None => {
                // No matching samples found yet, need to wait for more samples
                None
            }
        }
    }

    unsafe fn mix_samples(&self, sys_sample: &IMFSample, mic_sample: &IMFSample) -> Result<IMFSample> {
        debug!("Creating mixed buffer");
        
        // Get sample time and duration from system audio
        let sys_sample_time = sys_sample.GetSampleTime()?;
        let mic_sample_time = mic_sample.GetSampleTime()?;

        let sys_sample_duration = sys_sample.GetSampleDuration()?;
        let mic_sample_duration = mic_sample.GetSampleDuration()?;

        let sample_time = (sys_sample_time + mic_sample_time) / 2;
        let sample_duration = (sys_sample_duration + mic_sample_duration) / 2;
        
        // Get the buffers from both samples
        let sys_buffer = sys_sample.GetBufferByIndex(0)?;
        let mic_buffer = mic_sample.GetBufferByIndex(0)?;
        
        // Get the data from the buffers
        let mut sys_data: *mut u8 = std::ptr::null_mut();
        let mut mic_data: *mut u8 = std::ptr::null_mut();
        let mut sys_length: u32 = 0;
        let mut mic_length: u32 = 0;
        
        // Lock and process buffers without using catch_unwind
        sys_buffer.Lock(&mut sys_data, None, Some(&mut sys_length))?;
        
        // Let's use a simpler approach without catch_unwind
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
    
    // Simplified mixing function with fixed 50/50 mix
    unsafe fn mix_pcm_audio(
        &self,
        sys_data: *mut u8,
        mic_data: *mut u8,
        output_data: *mut u8,
        sys_length: u32,
        mic_length: u32
    ) -> Result<()> {
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
        
        debug!("Mixing {} samples with ratio system:{:.1} mic:{:.1}", 
               mix_len, sys_ratio, mic_ratio);
        
        // Mix the samples
        for i in 0..mix_len {
            // Simple weighted average
            let mixed_val = (sys_samples[i] as f32 * sys_ratio + 
                            mic_samples[i] as f32 * mic_ratio) as i32;
            
            // Clamp to i16 range
            let clamped = mixed_val.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            
            // Write to output
            output_samples[i] = clamped;
        }
        
        // If sys_samples is longer than mic_samples, fill the rest with system audio
        if sys_samples.len() > mix_len {
            for i in mix_len..std::cmp::min(sys_samples.len(), output_samples.len()) {
                // Reduce volume slightly since we're not mixing
                output_samples[i] = (sys_samples[i] as f32 * 0.8) as i16;
            }
        }
        
        debug!("PCM mixing completed successfully");
        Ok(())
    }
}