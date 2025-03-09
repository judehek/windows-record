use std::collections::VecDeque;
use windows::core::Result;
use windows::Win32::Media::MediaFoundation::{IMFSample, IMFMediaBuffer, MFCreateSample, MFCreateMemoryBuffer};
use std::sync::Arc;
use crate::types::SendableSample;
use log::{error, warn, info, debug, trace};

// Samplerate API imports
use libsamplerate::{SRC_DATA, SRC_SINC_BEST_QUALITY, src_new, src_process, src_delete, src_float_to_short_array, src_short_to_float_array};

pub struct AudioMixer {
    system_audio_queue: VecDeque<SendableSample>,
    microphone_queue: VecDeque<SendableSample>,
    sample_rate: u32,
    bits_per_sample: u16,
    channels: u16,
}

impl AudioMixer {
    pub fn new(sample_rate: u32, bits_per_sample: u16, channels: u16) -> Self {
        info!("Creating AudioMixer: sample_rate={}, bits_per_sample={}, channels={}", 
              sample_rate, bits_per_sample, channels);
        Self {
            system_audio_queue: VecDeque::new(),
            microphone_queue: VecDeque::new(),
            sample_rate,
            bits_per_sample,
            channels,
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

    pub unsafe fn process_next_sample(&mut self) -> Option<Result<Arc<IMFSample>>> {
        debug!("Processing next sample - sys queue: {}, mic queue: {}", 
               self.system_audio_queue.len(), self.microphone_queue.len());
        
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
        
        // We have samples from both sources - let's mix them
        debug!("Mixing system and microphone audio");
        let sys_sample = self.system_audio_queue.pop_front().unwrap();
        let mic_sample = self.microphone_queue.pop_front().unwrap();
        
        // Mix the samples and wrap the result in an Arc
        match self.mix_samples(&sys_sample.0, &mic_sample.0) {
            Some(Ok(mixed)) => {
                debug!("Successfully mixed audio samples");
                Some(Ok(Arc::new(mixed)))
            },
            Some(Err(e)) => {
                error!("Error mixing audio samples: {:?}", e);
                Some(Err(e))
            },
            None => {
                warn!("Failed to mix samples, returned None");
                None
            },
        }
    }

    unsafe fn mix_samples(&self, sys_sample: &IMFSample, mic_sample: &IMFSample) -> Option<Result<IMFSample>> {
        // Just delegate to our implementation in create_mixed_buffer
        Some(self.create_mixed_buffer(sys_sample, mic_sample))
    }

    unsafe fn create_mixed_buffer(&self, sys_sample: &IMFSample, mic_sample: &IMFSample) -> Result<IMFSample> {
        debug!("Creating mixed buffer");
        
        // Get sample time and duration from system audio (our reference timing)
        let sample_time = match sys_sample.GetSampleTime() {
            Ok(time) => {
                trace!("Sample time: {}", time);
                time
            },
            Err(e) => {
                error!("Failed to get sample time: {:?}", e);
                return Err(e);
            }
        };
        
        let sample_duration = match sys_sample.GetSampleDuration() {
            Ok(duration) => {
                trace!("Sample duration: {}", duration);
                duration
            },
            Err(e) => {
                error!("Failed to get sample duration: {:?}", e);
                return Err(e);
            }
        };
        
        // Get the buffers from both samples
        let sys_buffer = match sys_sample.GetBufferByIndex(0) {
            Ok(buffer) => buffer,
            Err(e) => {
                error!("Failed to get system buffer: {:?}", e);
                return Err(e);
            }
        };
        
        let mic_buffer = match mic_sample.GetBufferByIndex(0) {
            Ok(buffer) => buffer,
            Err(e) => {
                error!("Failed to get microphone buffer: {:?}", e);
                return Err(e);
            }
        };
        
        // Get the data from the buffers
        let mut sys_data: *mut u8 = std::ptr::null_mut();
        let mut mic_data: *mut u8 = std::ptr::null_mut();
        let mut sys_length: u32 = 0;
        let mut mic_length: u32 = 0;
        
        if let Err(e) = sys_buffer.Lock(&mut sys_data, None, Some(&mut sys_length)) {
            error!("Failed to lock system buffer: {:?}", e);
            return Err(e);
        }
        
        if let Err(e) = mic_buffer.Lock(&mut mic_data, None, Some(&mut mic_length)) {
            error!("Failed to lock microphone buffer: {:?}", e);
            sys_buffer.Unlock()?;
            return Err(e);
        }
        
        debug!("Buffer sizes - System: {} bytes, Microphone: {} bytes", sys_length, mic_length);
        trace!("Buffer addresses - System: {:p}, Microphone: {:p}", sys_data, mic_data);
        
        // Determine output size based on system audio buffer
        let output_size = sys_length as usize;
        
        // Create a new sample and buffer for the mixed audio
        let output_sample = match MFCreateSample() {
            Ok(sample) => sample,
            Err(e) => {
                error!("Failed to create output sample: {:?}", e);
                sys_buffer.Unlock()?;
                mic_buffer.Unlock()?;
                return Err(e);
            }
        };
        
        let output_buffer = match MFCreateMemoryBuffer(output_size as u32) {
            Ok(buffer) => buffer,
            Err(e) => {
                error!("Failed to create output buffer: {:?}", e);
                sys_buffer.Unlock()?;
                mic_buffer.Unlock()?;
                return Err(e);
            }
        };
        
        let mut output_data: *mut u8 = std::ptr::null_mut();
        let mut output_max_length: u32 = 0;
        
        if let Err(e) = output_buffer.Lock(&mut output_data, Some(&mut output_max_length), None) {
            error!("Failed to lock output buffer: {:?}", e);
            sys_buffer.Unlock()?;
            mic_buffer.Unlock()?;
            return Err(e);
        }
        
        trace!("Output buffer - Address: {:p}, Max Length: {}", output_data, output_max_length);
        
        // Mix the PCM audio
        let mix_result = self.mix_pcm_audio(
            sys_data, 
            mic_data, 
            output_data, 
            sys_length, 
            mic_length, 
            output_size
        );
        
        // Unlock buffers
        sys_buffer.Unlock()?;
        mic_buffer.Unlock()?;
        
        if let Err(e) = mix_result {
            error!("Error during audio mixing: {:?}", e);
            output_buffer.Unlock()?;
            return Err(e);
        }
        
        if let Err(e) = output_buffer.SetCurrentLength(output_size as u32) {
            error!("Failed to set output buffer length: {:?}", e);
            output_buffer.Unlock()?;
            return Err(e);
        }
        
        output_buffer.Unlock()?;
        
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
        
        debug!("Successfully created mixed audio sample");
        Ok(output_sample)
    }
    
    // Simplified mixing function that assumes both inputs are 16-bit PCM
    unsafe fn mix_pcm_audio(
        &self,
        sys_data: *mut u8,     // System audio (16-bit PCM)
        mic_data: *mut u8,     // Microphone audio (16-bit PCM)
        output_data: *mut u8,  // Output buffer (16-bit PCM)
        sys_length: u32,
        mic_length: u32,
        output_size: usize
    ) -> Result<()> {
        // Calculate frame counts (both should be 16-bit PCM)
        let bytes_per_sample = 2; // 16-bit PCM = 2 bytes per sample
        let sys_frame_count = sys_length as usize / (bytes_per_sample * self.channels as usize);
        let mic_frame_count = mic_length as usize / (bytes_per_sample * self.channels as usize);
        
        debug!("Frame counts - System: {}, Microphone: {}", sys_frame_count, mic_frame_count);
        
        // Access the audio data as 16-bit PCM
        let sys_samples = std::slice::from_raw_parts(
            sys_data as *const i16,
            sys_length as usize / 2
        );
        
        let mic_samples = std::slice::from_raw_parts(
            mic_data as *const i16,
            mic_length as usize / 2
        );
        
        // Diagnostic information
        if !sys_samples.is_empty() {
            let sys_stats = calculate_stats_i16(&sys_samples[..std::cmp::min(1000, sys_samples.len())]);
            debug!("SYSTEM AUDIO STATS - Min: {}, Max: {}, Avg: {:.1}, AbsMax: {}, NonZero: {:.1}%", 
           sys_stats.min, sys_stats.max, sys_stats.avg, sys_stats.abs_max, sys_stats.percent_non_zero);
        }
        
        if !mic_samples.is_empty() {
            let mic_stats = calculate_stats_i16(&mic_samples[..std::cmp::min(1000, mic_samples.len())]);
            debug!("MIC AUDIO STATS - Min: {}, Max: {}, Avg: {:.1}, AbsMax: {}, NonZero: {:.1}%", 
                   mic_stats.min, mic_stats.max, mic_stats.avg, mic_stats.abs_max, mic_stats.percent_non_zero);
        }
        
        // Resample microphone audio if needed
        let mic_samples_to_use = if mic_frame_count == sys_frame_count {
            debug!("No resampling needed for microphone audio");
            mic_samples
        } else {
            // Perform resampling
            let resampled = self.resample_audio(
                mic_data,
                mic_length,
                mic_frame_count,
                sys_frame_count
            )?;
            
            if !resampled.is_empty() {
                &resampled.clone()
            } else {
                // Fallback if resampling failed
                debug!("Resampling failed, using a slice of the original data");
                let max_samples = std::cmp::min(mic_length, sys_length) as usize / 2;
                &mic_samples[..max_samples]
            }
        };
        
        // Get output as mutable slice of i16
        let output_samples = std::slice::from_raw_parts_mut(
            output_data as *mut i16, 
            output_size / 2
        );
        
        // Mix the audio (both PCM16)
        let mix_len = std::cmp::min(
            sys_samples.len(),
            mic_samples_to_use.len()
        );
        
        debug!("Mixing {} samples to PCM16 output", mix_len);
        
        // Fixed mix ratios
        let sys_ratio = 0.50;  // 65% system audio
        let mic_ratio = 0.50;  // 35% microphone
        
        debug!("Using mix with System: {:.2}, Microphone: {:.2}", sys_ratio, mic_ratio);
        
        // Mix directly in integer space with scaled ratios
        // Convert floating-point ratios to integer weights (out of 100)
        let sys_weight = (sys_ratio * 100.0) as i32;
        let mic_weight = (mic_ratio * 100.0) as i32;
        
        let mut clipped_count = 0;
        for i in 0..mix_len {
            // Get system and mic values
            let sys_val = if i < sys_samples.len() { sys_samples[i] as i32 } else { 0 };
            let mic_val = if i < mic_samples_to_use.len() { mic_samples_to_use[i] as i32 } else { 0 };
            
            // Mix with weights
            let mixed_val = (sys_val * sys_weight + mic_val * mic_weight) / 100;

            if mixed_val < i16::MIN as i32 || mixed_val > i16::MAX as i32 {
                clipped_count += 1;
                if clipped_count <= 10 {  // Log only first 10 clipped samples
                    trace!("CLIPPED: sample[{}] - sys={}, mic={}, mixed={}, clipped to={}", 
                           i, sys_val, mic_val, mixed_val, 
                           mixed_val.clamp(i16::MIN as i32, i16::MAX as i32));
                }
            }
            
            // Clamp to i16 range
            let clamped_result = mixed_val.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            
            // Write to output
            if i < output_samples.len() {
                output_samples[i] = clamped_result;
            }
        }

        if clipped_count > 0 {
            warn!("Audio clipping detected in {} of {} samples ({:.2}%)", 
                  clipped_count, mix_len, (clipped_count as f64 / mix_len as f64) * 100.0);
        }

        let mixed_stats = calculate_stats_i16(&output_samples[..std::cmp::min(1000, output_samples.len())]);
        debug!("MIXED AUDIO STATS - Min: {}, Max: {}, Avg: {:.1}, AbsMax: {}, NonZero: {:.1}%, Clipped: {:.2}%", 
       mixed_stats.min, mixed_stats.max, mixed_stats.avg, mixed_stats.abs_max, 
       mixed_stats.percent_non_zero, mixed_stats.percent_clipped);
        
        // Fill any remaining output space with zeros
        for i in mix_len..output_samples.len() {
            output_samples[i] = 0;
        }
        
        debug!("PCM16 mixing completed successfully");
        Ok(())
    }
    
    // Helper function to resample audio
    unsafe fn resample_audio(
        &self,
        input_data: *mut u8,
        input_length: u32,
        input_frames: usize,
        output_frames: usize
    ) -> Result<Vec<i16>> {
        debug!("Resampling audio (input frames: {}, output frames: {})", 
               input_frames, output_frames);
        
        // Calculate resampling ratio
        let src_ratio = output_frames as f64 / input_frames as f64;
        debug!("Resampling ratio: {:.4}", src_ratio);
        
        // Convert PCM16 samples to float for the resampler
        let mut input_samples_f32 = vec![0.0f32; input_length as usize / 2];
        src_short_to_float_array(
            input_data as *const i16, 
            input_samples_f32.as_mut_ptr(), 
            input_length as i32 / 2
        );
        
        // Prepare output buffer for resampled data
        let out_samples = output_frames * self.channels as usize;
        let mut resampled_f32 = vec![0.0f32; out_samples];
        
        // Create SRC_DATA structure for resampling
        let mut src_data = SRC_DATA {
            data_in: input_samples_f32.as_ptr(),
            data_out: resampled_f32.as_mut_ptr(),
            input_frames: input_frames as i32,
            output_frames: output_frames as i32,
            input_frames_used: 0,
            output_frames_gen: 0,
            end_of_input: 1, // Last call
            src_ratio,
        };
        
        // Create resampler
        let mut error = 0;
        let src_state = src_new(SRC_SINC_BEST_QUALITY as i32, self.channels as i32, &mut error);
        
        if src_state.is_null() {
            error!("Failed to create resampler, error code: {}", error);
            return Ok(Vec::new());
        }
        
        debug!("Successfully created resampler");
        
        // Process resampling
        let error = src_process(src_state, &mut src_data);
        
        // Clean up
        src_delete(src_state);
        
        if error != 0 {
            warn!("Resampling failed with error code: {}", error);
            return Ok(Vec::new());
        }
        
        debug!("Resampling successful - Input frames used: {}, Output frames generated: {}", 
               src_data.input_frames_used, src_data.output_frames_gen);
        
        // Convert resampled float data back to i16
        let mut resampled_i16 = vec![0i16; src_data.output_frames_gen as usize * self.channels as usize];
        src_float_to_short_array(
            resampled_f32.as_ptr(), 
            resampled_i16.as_mut_ptr(), 
            src_data.output_frames_gen as i32 * self.channels as i32
        );
        
        Ok(resampled_i16)
    }
}

// Helper struct for audio statistics
struct AudioStats {
    min: i16,
    max: i16,
    avg: f64,
    abs_max: i16,
    percent_non_zero: f64,
    percent_clipped: f64,
}

// Helper function to calculate stats from i16 samples
fn calculate_stats_i16(samples: &[i16]) -> AudioStats {
    let mut min = i16::MAX;
    let mut max = i16::MIN;
    let mut sum: i64 = 0;
    let mut abs_max: i16 = 0;
    let mut non_zero = 0;
    let mut clipped = 0;  // Count samples at or very near the limits
    
    let clip_threshold = 32700;  // Very close to i16::MAX (32767)
    
    for &s in samples {
        sum += s as i64;
        if s < min { min = s; }
        if s > max { max = s; }
        
        // Safe abs calculation that handles i16::MIN
        let abs_s = if s == i16::MIN { 
            32767 // Closest we can get to abs(i16::MIN) without overflow
        } else { 
            s.abs()
        };
        
        if abs_s > abs_max { abs_max = abs_s; }
        if s != 0 { non_zero += 1; }
        
        // Check for clipping (values very close to limits)
        if abs_s >= clip_threshold {
            clipped += 1;
        }
    }
    
    let avg = sum as f64 / samples.len() as f64;
    let percent_non_zero = (non_zero as f64 / samples.len() as f64) * 100.0;
    let percent_clipped = (clipped as f64 / samples.len() as f64) * 100.0;
    
    AudioStats {
        min,
        max,
        avg,
        abs_max,
        percent_non_zero,
        percent_clipped
    }
}