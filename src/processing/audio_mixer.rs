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
        
        // Mix based on bit depth
        let mix_result = if self.bits_per_sample == 16 {
            debug!("Mixing 16-bit PCM audio");
            self.mix_pcm16(
                sys_data, 
                mic_data, 
                output_data, 
                sys_length, 
                mic_length, 
                output_size
            )
        } else if self.bits_per_sample == 32 {
            debug!("Mixing 32-bit float PCM audio");
            self.mix_pcm32(
                sys_data, 
                mic_data, 
                output_data, 
                sys_length, 
                mic_length, 
                output_size
            )
        } else {
            warn!("Unsupported bit depth: {}, falling back to direct copy", self.bits_per_sample);
            // Unsupported bit depth, just copy system audio as fallback
            std::ptr::copy_nonoverlapping(sys_data, output_data, output_size);
            Ok(())
        };
        
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

    // Mix 16-bit PCM audio with proper resampling if needed
    unsafe fn mix_pcm16(
        &self,
        sys_data: *mut u8,
        mic_data: *mut u8,
        output_data: *mut u8,
        sys_length: u32,
        mic_length: u32,
        output_size: usize
    ) -> Result<()> {
        // Calculate frame counts
        let sys_frame_count = sys_length as usize / (2 * self.channels as usize);
        let mic_frame_count = mic_length as usize / (2 * self.channels as usize);
        
        debug!("Frame counts - System: {}, Microphone: {}", sys_frame_count, mic_frame_count);
        
        // Convert raw pointers to slices for system audio
        let sys_samples = std::slice::from_raw_parts(
            sys_data as *const i16, 
            sys_length as usize / 2
        );
        
        // Check if first few system audio samples look valid
        trace!("First 5 system audio samples: {:?}", 
               &sys_samples.iter().take(5).collect::<Vec<&i16>>());
        
        // Temporary storage for resampled data
        let mut resampled_mic_samples: Vec<i16>;
        
        // Determine which mic samples to use (original or resampled)
        let mic_samples_to_use = if mic_frame_count == sys_frame_count {
            debug!("No resampling needed for microphone audio");
            let samples = std::slice::from_raw_parts(mic_data as *const i16, mic_length as usize / 2);
            trace!("First 5 microphone audio samples: {:?}", 
                   &samples.iter().take(5).collect::<Vec<&i16>>());
            samples
        } else {
            debug!("Resampling microphone audio (mic frames: {}, sys frames: {})", 
                   mic_frame_count, sys_frame_count);
            
            // First convert i16 samples to f32 for the resampler
            let mut mic_samples_f32 = vec![0.0f32; mic_length as usize / 2];
            src_short_to_float_array(
                mic_data as *const i16, 
                mic_samples_f32.as_mut_ptr(), 
                mic_length as i32 / 2
            );
            
            trace!("First 5 mic samples converted to float: {:?}", 
                   &mic_samples_f32.iter().take(5).collect::<Vec<&f32>>());
            
            // Calculate the src_ratio
            let src_ratio = sys_frame_count as f64 / mic_frame_count as f64;
            debug!("Resampling ratio: {:.4}", src_ratio);
            
            // Prepare output buffer for resampled data
            let out_frames = sys_frame_count * self.channels as usize;
            let mut resampled_f32 = vec![0.0f32; out_frames];
            
            // Create SRC_DATA structure for resampling
            let mut src_data = SRC_DATA {
                data_in: mic_samples_f32.as_ptr(),
                data_out: resampled_f32.as_mut_ptr(),
                input_frames: mic_frame_count as i32,
                output_frames: sys_frame_count as i32,
                input_frames_used: 0,
                output_frames_gen: 0,
                end_of_input: 1, // Last call
                src_ratio: src_ratio,
            };
            
            // Create resampler
            let mut error = 0;
            let src_state = src_new(SRC_SINC_BEST_QUALITY as i32, self.channels as i32, &mut error);
            
            if !src_state.is_null() {
                debug!("Successfully created resampler");
                // Process resampling
                let error = src_process(src_state, &mut src_data);
                
                // Clean up
                src_delete(src_state);
                
                if error == 0 {
                    debug!("Resampling successful - Input frames used: {}, Output frames generated: {}", 
                           src_data.input_frames_used, src_data.output_frames_gen);
                    
                    // Convert resampled float data back to i16
                    resampled_mic_samples = vec![0i16; src_data.output_frames_gen as usize * self.channels as usize];
                    src_float_to_short_array(
                        resampled_f32.as_ptr(), 
                        resampled_mic_samples.as_mut_ptr(), 
                        src_data.output_frames_gen as i32 * self.channels as i32
                    );
                    
                    trace!("First 5 resampled mic samples: {:?}", 
                          &resampled_mic_samples.iter().take(5).collect::<Vec<&i16>>());
                    
                    &resampled_mic_samples
                } else {
                    warn!("Resampling failed with error code: {}, falling back to original", error);
                    // Resampling failed, use original with caution
                    let max_samples = std::cmp::min(mic_length, sys_length) as usize / 2;
                    let samples = std::slice::from_raw_parts(mic_data as *const i16, max_samples);
                    trace!("First 5 mic samples (fallback): {:?}", 
                           &samples.iter().take(5).collect::<Vec<&i16>>());
                    samples
                }
            } else {
                error!("Failed to create resampler, error code: {}", error);
                // Resampler creation failed, use original with caution
                let max_samples = std::cmp::min(mic_length, sys_length) as usize / 2;
                let samples = std::slice::from_raw_parts(mic_data as *const i16, max_samples);
                trace!("First 5 mic samples (fallback after resampler creation failure): {:?}", 
                       &samples.iter().take(5).collect::<Vec<&i16>>());
                samples
            }
        };
        
        // Get output as mutable slice
        let output_samples = std::slice::from_raw_parts_mut(
            output_data as *mut i16, 
            output_size / 2
        );
        
        // Mix the audio: 70% system, 30% mic
        let mix_len = std::cmp::min(sys_samples.len(), mic_samples_to_use.len());
        debug!("Mixing {} samples", mix_len);
        
        // Check sample ranges before mixing
        let sys_peak = sys_samples.iter()
            .map(|&s| (s as i32).abs() as f32)
            .fold(1.0f32, |a, b| a.max(b));
        let mic_peak = mic_samples_to_use.iter()
            .map(|&s| (s as i32).abs() as f32)
            .fold(1.0f32, |a, b| a.max(b));

        
        for i in 0..mix_len {
            // Convert to float, mix, then back to i16 with clipping
            let sys_val = (sys_samples[i] as f32) / sys_peak;
            let mic_val = (mic_samples_to_use[i] as f32) / mic_peak;
            
            // Mix with weights
            let mixed_val = (sys_val * 0.7) + (mic_val * 0.3);
            
            output_samples[i] = (mixed_val * 32767.0).clamp(-32768.0, 32767.0) as i16;
        }
        
        // If we don't have enough mic samples, fill the rest with system audio
        if mix_len < sys_samples.len() {
            debug!("Filling remaining {} samples with system audio", sys_samples.len() - mix_len);
            for i in mix_len..sys_samples.len() {
                output_samples[i] = sys_samples[i];
            }
        }
        
        // Check output sample range
        let out_min_max = output_samples.iter().take(100)
            .fold((i16::MAX, i16::MIN), |(min, max), &v| (min.min(v), max.max(v)));
        debug!("Output sample range (first 100): {:?}", out_min_max);
        
        debug!("PCM16 mixing completed successfully");
        Ok(())
    }
    
    // Mix 32-bit float PCM audio with proper resampling if needed
    unsafe fn mix_pcm32(
        &self,
        sys_data: *mut u8,
        mic_data: *mut u8,
        output_data: *mut u8,
        sys_length: u32,
        mic_length: u32,
        output_size: usize
    ) -> Result<()> {
        // Calculate frame counts
        let sys_frame_count = sys_length as usize / (4 * self.channels as usize);
        let mic_frame_count = mic_length as usize / (4 * self.channels as usize);
        
        debug!("Frame counts - System: {}, Microphone: {}", sys_frame_count, mic_frame_count);
        
        // Convert raw pointers to slices for system audio
        let sys_samples = std::slice::from_raw_parts(
            sys_data as *const f32, 
            sys_length as usize / 4
        );
        
        // Check if first few system audio samples look valid
        trace!("First 5 system audio samples: {:?}", 
               &sys_samples.iter().take(5).collect::<Vec<&f32>>());
        
        // Handle microphone audio (possibly resampling)
        let mic_samples_f32 = std::slice::from_raw_parts(
            mic_data as *const f32, 
            mic_length as usize / 4
        );
        
        trace!("First 5 microphone audio samples: {:?}", 
               &mic_samples_f32.iter().take(5).collect::<Vec<&f32>>());
        
        // Temporary storage for resampled data
        let mut resampled_f32: Vec<f32>;
        
        // Determine which mic samples to use (original or resampled)
        let mic_samples_to_use = if mic_frame_count == sys_frame_count {
            debug!("No resampling needed for microphone audio");
            mic_samples_f32
        } else {
            debug!("Resampling microphone audio (mic frames: {}, sys frames: {})", 
                   mic_frame_count, sys_frame_count);
            
            // Calculate the src_ratio
            let src_ratio = sys_frame_count as f64 / mic_frame_count as f64;
            debug!("Resampling ratio: {:.4}", src_ratio);
            
            // Prepare output buffer for resampled data
            let out_frames = sys_frame_count * self.channels as usize;
            resampled_f32 = vec![0.0f32; out_frames];
            
            // Create SRC_DATA structure for resampling
            let mut src_data = SRC_DATA {
                data_in: mic_samples_f32.as_ptr(),
                data_out: resampled_f32.as_mut_ptr(),
                input_frames: mic_frame_count as i32,
                output_frames: sys_frame_count as i32,
                input_frames_used: 0,
                output_frames_gen: 0,
                end_of_input: 1, // Last call
                src_ratio: src_ratio,
            };
            
            // Create resampler
            let mut error = 0;
            let src_state = src_new(SRC_SINC_BEST_QUALITY as i32, self.channels as i32, &mut error);
            
            if !src_state.is_null() {
                debug!("Successfully created resampler");
                // Process resampling
                let error = src_process(src_state, &mut src_data);
                
                // Clean up
                src_delete(src_state);
                
                if error == 0 {
                    debug!("Resampling successful - Input frames used: {}, Output frames generated: {}", 
                           src_data.input_frames_used, src_data.output_frames_gen);
                    
                    // Resize to actual output size
                    resampled_f32.truncate(src_data.output_frames_gen as usize * self.channels as usize);
                    trace!("First 5 resampled mic samples: {:?}", 
                           &resampled_f32.iter().take(5).collect::<Vec<&f32>>());
                    &resampled_f32
                } else {
                    warn!("Resampling failed with error code: {}, falling back to original", error);
                    // Resampling failed, use original with caution
                    let max_samples = std::cmp::min(mic_length, sys_length) as usize / 4;
                    std::slice::from_raw_parts(mic_data as *const f32, max_samples)
                }
            } else {
                error!("Failed to create resampler, error code: {}", error);
                // Resampler creation failed, use original with caution
                let max_samples = std::cmp::min(mic_length, sys_length) as usize / 4;
                std::slice::from_raw_parts(mic_data as *const f32, max_samples)
            }
        };
        
        // Get output as mutable slice
        let output_samples = std::slice::from_raw_parts_mut(
            output_data as *mut f32, 
            output_size / 4
        );
        
        // Mix the audio: 70% system, 30% mic
        let mix_len = std::cmp::min(sys_samples.len(), mic_samples_to_use.len());
        debug!("Mixing {} samples", mix_len);
        
        // Check sample ranges before mixing
        let sys_min_max = sys_samples.iter().take(100)
            .fold((f32::MAX, f32::MIN), |(min, max), &v| (min.min(v), max.max(v)));
        let mic_min_max = mic_samples_to_use.iter().take(100)
            .fold((f32::MAX, f32::MIN), |(min, max), &v| (min.min(v), max.max(v)));
        debug!("Sample ranges (first 100) - System: {:?}, Microphone: {:?}", sys_min_max, mic_min_max);
        
        for i in 0..mix_len {
            // Mix with weights and clip
            let mixed_val = (sys_samples[i] * 0.7) + (mic_samples_to_use[i] * 0.3);
            
            // Clip to [-1.0, 1.0]
            output_samples[i] = if mixed_val > 1.0 {
                trace!("Clipping at index {}: {}", i, mixed_val);
                1.0
            } else if mixed_val < -1.0 {
                trace!("Clipping at index {}: {}", i, mixed_val);
                -1.0
            } else {
                mixed_val
            };
        }
        
        // If we don't have enough mic samples, fill the rest with system audio
        if mix_len < sys_samples.len() {
            debug!("Filling remaining {} samples with system audio", sys_samples.len() - mix_len);
            for i in mix_len..sys_samples.len() {
                output_samples[i] = sys_samples[i];
            }
        }
        
        // Check output sample range
        let out_min_max = output_samples.iter().take(100)
            .fold((f32::MAX, f32::MIN), |(min, max), &v| (min.min(v), max.max(v)));
        debug!("Output sample range (first 100): {:?}", out_min_max);
        
        debug!("PCM32 mixing completed successfully");
        Ok(())
    }
}