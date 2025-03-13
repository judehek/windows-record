use std::collections::VecDeque;
use windows::core::Result;
use windows::Win32::Media::MediaFoundation::{IMFSample, MFCreateSample, MFCreateMemoryBuffer};
use std::sync::Arc;
use crate::types::SendableSample;
use log::{error, info, debug, trace};

pub struct AudioMixer {
    system_audio_queue: VecDeque<SendableSample>,
    microphone_queue: VecDeque<SendableSample>,
    sample_rate: u32,
    channels: u16,
    // Volume
    system_volume: f32,
    microphone_volume: f32,
    // Important for process_next_sample
    both_sources_active: bool,
}

impl AudioMixer {
    pub fn new(sample_rate: u32, bits_per_sample: u16, channels: u16, both_sources_active: bool) -> Self {
        info!("Creating AudioMixer: sample_rate={}, bits_per_sample={}, channels={}", 
              sample_rate, bits_per_sample, channels);
        Self {
            system_audio_queue: VecDeque::new(),
            microphone_queue: VecDeque::new(),
            sample_rate,
            channels,
            // Default volume levels (neutral/1.0)
            system_volume: 1.0,
            microphone_volume: 1.0,
            both_sources_active,
        }
    }

    // Add methods to set volume levels
    pub fn set_system_volume(&mut self, gain: f32) {
        // Clamp gain between 0.0 and 2.0 (allow amplification)
        self.system_volume = gain.max(0.0).min(2.0);
        info!("System audio gain set to {:.2}", self.system_volume);
    }

    pub fn set_microphone_volume(&mut self, gain: f32) {
        // Clamp gain between 0.0 and 2.0 (allow amplification)
        self.microphone_volume = gain.max(0.0).min(2.0);
        info!("Microphone gain set to {:.2}", self.microphone_volume);
    }

    pub fn add_system_audio(&mut self, sample: SendableSample) {
        trace!("Adding system audio sample to queue (queue size: {})", self.system_audio_queue.len());
        self.system_audio_queue.push_back(sample);
    }

    pub fn add_microphone_audio(&mut self, sample: SendableSample) {
        trace!("Adding microphone audio sample to queue (queue size: {})", self.microphone_queue.len());
        self.microphone_queue.push_back(sample);
    }

    pub unsafe fn process_next_sample(&mut self) -> Option<Result<Arc<IMFSample>>> {
        debug!("Processing next sample - sys queue: {}, mic queue: {}", 
               self.system_audio_queue.len(), self.microphone_queue.len());
    
        // If both sources are active, we need samples from both to mix
        if self.both_sources_active {
            // If either queue is empty, return None to wait for the other source
            if self.system_audio_queue.is_empty() || self.microphone_queue.is_empty() {
                debug!("Both sources active but waiting for samples from both queues to prevent desync");
                return None;
            }
            
            // We have samples from both sources - let's mix them
            debug!("Mixing system and microphone audio");
            let sys_sample = self.system_audio_queue.pop_front().unwrap();
            let mic_sample = self.microphone_queue.pop_front().unwrap();
            
            // Mix the samples
            match self.mix_samples(&sys_sample.sample, &mic_sample.sample) {
                Ok(mixed) => {
                    debug!("Successfully mixed audio samples");
                    Some(Ok(Arc::new(mixed)))
                },
                Err(e) => {
                    error!("Error mixing audio samples: {:?}", e);
                    Some(Err(e))
                },
            }
        } else {
            // Only one source is active, process samples normally
            
            // Handle cases where only one source is available
            if self.system_audio_queue.is_empty() && self.microphone_queue.is_empty() {
                debug!("Both audio queues empty, returning None");
                return None;
            }
            
            // If only microphone audio is available, apply volume and return
            if self.system_audio_queue.is_empty() && !self.microphone_queue.is_empty() {
                debug!("Only microphone audio available, applying volume");
                let mic_sample = self.microphone_queue.pop_front().unwrap();
                
                // If volume is 1.0 (default), no need to process
                if (self.microphone_volume - 1.0).abs() < 0.001 {
                    debug!("Using default microphone volume, passing through");
                    return Some(Ok(mic_sample.sample.clone()));
                }
                
                // Apply microphone volume
                match self.apply_volume_to_sample(&mic_sample.sample, self.microphone_volume) {
                    Ok(processed) => {
                        debug!("Successfully applied volume to microphone sample");
                        return Some(Ok(Arc::new(processed)));
                    },
                    Err(e) => {
                        error!("Error applying volume to microphone sample: {:?}", e);
                        return Some(Err(e));
                    }
                }
            }
            
            // If only system audio is available, apply volume and return
            if !self.system_audio_queue.is_empty() && self.microphone_queue.is_empty() {
                debug!("Only system audio available, applying volume");
                let sys_sample = self.system_audio_queue.pop_front().unwrap();
                
                // If volume is 1.0 (default), no need to process
                if (self.system_volume - 1.0).abs() < 0.001 {
                    debug!("Using default system volume, passing through");
                    return Some(Ok(sys_sample.sample.clone()));
                }
                
                // Apply system volume
                match self.apply_volume_to_sample(&sys_sample.sample, self.system_volume) {
                    Ok(processed) => {
                        debug!("Successfully applied volume to system sample");
                        return Some(Ok(Arc::new(processed)));
                    },
                    Err(e) => {
                        error!("Error applying volume to system sample: {:?}", e);
                        return Some(Err(e));
                    }
                }
            }
            
            // This path should not be reachable due to the checks above,
            // but just in case, return None
            debug!("Unexpected state in process_next_sample");
            None
        }
    }

    // Method to apply volume to a single audio sample
    unsafe fn apply_volume_to_sample(&self, sample: &IMFSample, volume: f32) -> Result<IMFSample> {
        debug!("Applying volume {:.2} to sample", volume);
        
        // Get sample time and duration
        let sample_time = sample.GetSampleTime()?;
        let sample_duration = sample.GetSampleDuration()?;
        
        // Get the buffer from the sample
        let buffer = sample.GetBufferByIndex(0)?;
        
        // Get the data from the buffer
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut length: u32 = 0;
        
        // Lock buffer
        buffer.Lock(&mut data, None, Some(&mut length))?;
        
        // Create new sample and buffer for processed audio
        let output_sample = MFCreateSample()?;
        let output_buffer = MFCreateMemoryBuffer(length)?;
        
        // Lock output buffer
        let mut output_data: *mut u8 = std::ptr::null_mut();
        output_buffer.Lock(&mut output_data, None, None)?;
        
        // Access audio data as 16-bit PCM
        let samples = std::slice::from_raw_parts(
            data as *const i16,
            length as usize / 2
        );
        
        // Get output as mutable slice of i16
        let output_samples = std::slice::from_raw_parts_mut(
            output_data as *mut i16, 
            length as usize / 2
        );
        
        // Apply volume to each sample
        for i in 0..samples.len() {
            // Apply volume
            let adjusted = (samples[i] as f32 * volume) as i32;
            
            // Clamp to i16 range to prevent clipping
            output_samples[i] = adjusted.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        }
        
        // Unlock input buffer
        let input_unlock_result = buffer.Unlock();
        
        // Set output buffer length and unlock
        output_buffer.SetCurrentLength(length)?;
        output_buffer.Unlock()?;
        
        // Check if unlocking input buffer failed
        if let Err(e) = input_unlock_result {
            return Err(e);
        }
        
        // Add buffer to sample and set timing info
        output_sample.AddBuffer(&output_buffer)?;
        output_sample.SetSampleTime(sample_time)?;
        output_sample.SetSampleDuration(sample_duration)?;
        
        debug!("Volume application completed successfully");
        Ok(output_sample)
    }

    unsafe fn mix_samples(&self, sys_sample: &IMFSample, mic_sample: &IMFSample) -> Result<IMFSample> {
        debug!("Creating mixed buffer");
        
        // Get sample time and duration from system audio
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
    
    // Mixing function with volume controls allowing amplification
    unsafe fn mix_pcm_audio(
        &self,
        sys_data: *mut u8,     // System audio (16-bit PCM)
        mic_data: *mut u8,     // Microphone audio (16-bit PCM)
        output_data: *mut u8,  // Output buffer (16-bit PCM)
        sys_length: u32,
        mic_length: u32
    ) -> Result<()> {
        debug!("Mixing with system volume:{:.2} mic volume:{:.2}", 
               self.system_volume, self.microphone_volume);
        
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
        
        // Mix the samples with gain controls
        for i in 0..mix_len {
            // Apply gains to each source - this can amplify the signal
            let mixed_val = (sys_samples[i] as f32 * self.system_volume + 
                            mic_samples[i] as f32 * self.microphone_volume) as i32;
            
            // Clamp to i16 range to prevent clipping
            let clamped = mixed_val.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            
            // Write to output
            output_samples[i] = clamped;
        }
        
        // If sys_samples is longer than mic_samples, fill the rest with system audio
        if sys_samples.len() > mix_len {
            for i in mix_len..std::cmp::min(sys_samples.len(), output_samples.len()) {
                // Apply system gain to remaining samples
                let val = (sys_samples[i] as f32 * self.system_volume) as i32;
                output_samples[i] = val.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            }
        }
        
        debug!("PCM mixing completed successfully");
        Ok(())
    }
}