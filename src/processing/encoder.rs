use crate::device::VideoEncoder as DeviceVideoEncoder;
use crate::types::{SendableDxgiDeviceManager, TexturePool}; // Assuming TexturePool is needed for input samples
use log::{debug, error, info, warn};
use std::{
    mem::ManuallyDrop, // Import ManuallyDrop
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};
use windows::Foundation::TimeSpan;
use windows::{
    core::{ComInterface, Error, Interface, Result, GUID}, // Added GUID
    Win32::{
        Foundation::E_NOTIMPL,
        Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D}, // For textures
        Media::MediaFoundation::{
            IMFAttributes,
            IMFMediaEventGenerator,
            IMFMediaType,
            IMFSample,
            IMFTransform,
            METransformHaveOutput,
            METransformNeedInput,
            MFCreateDXGISurfaceBuffer,
            MFCreateMediaType,
            MFCreateSample,
            MFMediaType_Video,
            MFVideoFormat_H264,
            MFVideoFormat_HEVC,
            MFVideoFormat_NV12,
            MFVideoInterlace_Progressive,
            MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
            MFT_INPUT_STREAM_INFO,
            MFT_MESSAGE_COMMAND_FLUSH,
            MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
            MFT_MESSAGE_NOTIFY_END_OF_STREAM,
            MFT_MESSAGE_NOTIFY_END_STREAMING,
            MFT_MESSAGE_NOTIFY_START_OF_STREAM,
            MFT_MESSAGE_SET_D3D_MANAGER,
            MFT_OUTPUT_DATA_BUFFER,
            MFT_OUTPUT_STREAM_INFO,
            MFT_SET_TYPE_TEST_ONLY,
            MF_EVENT_TYPE,
            MF_E_INVALIDMEDIATYPE,
            MF_E_NO_MORE_TYPES,
            MF_E_TRANSFORM_NEED_MORE_INPUT, // Import specific error code
            MF_E_TRANSFORM_TYPE_NOT_SET,
            MF_MT_ALL_SAMPLES_INDEPENDENT,
            MF_MT_AVG_BITRATE,
            MF_MT_FRAME_RATE,
            MF_MT_FRAME_SIZE,
            MF_MT_INTERLACE_MODE,
            MF_MT_MAJOR_TYPE,
            MF_MT_PIXEL_ASPECT_RATIO,
            MF_MT_SUBTYPE,
            MF_TRANSFORM_ASYNC_UNLOCK,
        },
    },
};

// Re-import helper functions if they were moved or are needed here
// Assuming MFSetAttributeRatio and MFSetAttributeSize might be in media.rs
// Define helpers locally as they are not in media.rs
fn pack_2_u32_as_u64(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

#[allow(non_snake_case)]
unsafe fn MFSetAttributeSize(
    attributes: &IMFAttributes,
    key: &GUID,
    width: u32,
    height: u32,
) -> Result<()> {
    attributes.SetUINT64(key, pack_2_u32_as_u64(width, height))
}

#[allow(non_snake_case)]
unsafe fn MFSetAttributeRatio(
    attributes: &IMFAttributes,
    key: &GUID,
    numerator: u32,
    denominator: u32,
) -> Result<()> {
    attributes.SetUINT64(key, pack_2_u32_as_u64(numerator, denominator))
}
// Removed incorrect import: use super::media::{MFSetAttributeRatio, MFSetAttributeSize};

// --- Structs for Sample Data ---

/// Represents an input sample for the video encoder (NV12 texture + timestamp).
pub struct VideoEncoderInputSample {
    timestamp: TimeSpan,
    texture: ID3D11Texture2D, // This should be the NV12 texture from VideoProcessor
}

impl VideoEncoderInputSample {
    pub fn new(timestamp: TimeSpan, texture: ID3D11Texture2D) -> Self {
        Self { timestamp, texture }
    }
}

/// Represents an output sample from the video encoder (encoded data).
pub struct VideoEncoderOutputSample {
    sample: IMFSample, // Contains the encoded H.264/HEVC data
}

impl VideoEncoderOutputSample {
    pub fn sample(&self) -> &IMFSample {
        &self.sample
    }
}

// --- Main VideoEncoder Struct ---

pub struct VideoEncoder {
    inner: Option<VideoEncoderInner>, // Holds state for the encoding thread
    output_type: IMFMediaType,        // Keep track of the configured output type
    started: AtomicBool,
    should_stop: Arc<AtomicBool>,
    encoder_thread_handle: Option<JoinHandle<Result<()>>>,
    frame_count: Arc<std::sync::atomic::AtomicU64>, // Add frame counter
}

// --- Inner Struct for Thread State ---

struct VideoEncoderInner {
    transform: IMFTransform,
    event_generator: IMFMediaEventGenerator,
    input_stream_id: u32,
    output_stream_id: u32,
    dxgi_manager: Arc<SendableDxgiDeviceManager>, // Keep manager alive

    // Callbacks for interaction
    sample_requested_callback:
        Option<Box<dyn Send + FnMut() -> Result<Option<VideoEncoderInputSample>>>>,
    sample_rendered_callback: Option<Box<dyn Send + FnMut(VideoEncoderOutputSample) -> Result<()>>>,

    should_stop: Arc<AtomicBool>,
    frame_count: Arc<std::sync::atomic::AtomicU64>, // Add frame counter here too
}

// --- VideoEncoder Implementation ---

impl VideoEncoder {
    /// Creates a new VideoEncoder instance using the provided device encoder.
    pub fn new(
        device_encoder: &DeviceVideoEncoder, // Use the struct from device::video
        d3d_device: Arc<ID3D11Device>,       // Shared D3D device
        dxgi_manager: Arc<SendableDxgiDeviceManager>, // Shared DXGI manager
        input_resolution: (u32, u32),        // NV12 input size (from VideoProcessor)
        output_resolution: (u32, u32),       // Final encoded size
        bit_rate: u32,                       // Target bitrate in bps
        frame_rate: (u32, u32),              // Target frame rate (num, den)
    ) -> Result<Self> {
        info!(
            "Creating VideoEncoder: Name='{}', Type={:?}, Input={}x{}, Output={}x{}, Bitrate={}, FPS={}/{}",
            device_encoder.name,
            device_encoder.encoder_type,
            input_resolution.0,
            input_resolution.1,
            output_resolution.0,
            output_resolution.1,
            bit_rate,
            frame_rate.0,
            frame_rate.1
        );

        // 1. Create the MFT instance from the activation object
        let transform = device_encoder.create_transform()?;
        info!(
            "Created IMFTransform for encoder '{}'.",
            device_encoder.name
        );

        // 2. Get necessary interfaces and attributes
        let event_generator: IMFMediaEventGenerator = transform.cast()?;
        let attributes = unsafe { transform.GetAttributes()? };

        // 3. (Optional) Try enabling async unlock if supported by the MFT
        match unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) } {
            Ok(_) => debug!("Async unlock enabled for encoder MFT."),
            Err(_) => debug!("Encoder MFT does not support async unlock."),
        }
        // Note: We are not setting MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS here,
        // assuming the MFT selected via MFT_ENUM_FLAG_HARDWARE is inherently hardware-based.

        // 4. Associate DXGI Device Manager (CRITICAL for hardware acceleration)
        // We need the raw pointer temporarily.
        let manager_ptr = dxgi_manager.as_raw();
        match unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager_ptr as usize) }
        {
            Ok(_) => info!("Associated DXGI Device Manager with encoder MFT."),
            Err(e) => {
                warn!("Failed to associate DXGI Device Manager with encoder MFT: {:?}. Hardware encoding might fail.", e);
                // Depending on requirements, might want to return Err(e) here.
            }
        }

        // 5. Determine Stream IDs
        // Similar logic to the example encoder setup
        let mut number_of_input_streams = 0;
        let mut number_of_output_streams = 0;
        unsafe {
            transform.GetStreamCount(&mut number_of_input_streams, &mut number_of_output_streams)?
        };
        if number_of_input_streams == 0 || number_of_output_streams == 0 {
            error!("Encoder MFT reported 0 input or output streams.");
            return Err(Error::new(
                windows::Win32::Foundation::E_UNEXPECTED,
                "Encoder MFT has 0 streams".into(),
            ));
        }
        debug!(
            "Encoder Stream Counts: Input={}, Output={}",
            number_of_input_streams, number_of_output_streams
        );

        let (input_stream_id, output_stream_id) = {
            let mut input_stream_ids = vec![0u32; number_of_input_streams as usize];
            let mut output_stream_ids = vec![0u32; number_of_output_streams as usize];
            let result =
                unsafe { transform.GetStreamIDs(&mut input_stream_ids, &mut output_stream_ids) };
            match result {
                Ok(_) => (input_stream_ids[0], output_stream_ids[0]), // Assume first streams
                Err(e) if e.code() == E_NOTIMPL => {
                    debug!("GetStreamIDs returned E_NOTIMPL, assuming stream IDs are 0.");
                    (0, 0) // Default to 0 if not implemented (common case)
                }
                Err(e) => {
                    error!("Failed to get stream IDs: {:?}", e);
                    return Err(e);
                }
            }
        };
        info!(
            "Using Input Stream ID: {}, Output Stream ID: {}",
            input_stream_id, output_stream_id
        );

        // 6. Configure Output Media Type (H.264 or HEVC)
        let output_format_guid = device_encoder.encoder_type.get_guid(); // Get GUID from device encoder type
        let output_type = unsafe {
            let media_type = MFCreateMediaType()?;
            let attrs: IMFAttributes = media_type.cast()?; // Cast needed for helpers

            media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            media_type.SetGUID(&MF_MT_SUBTYPE, &output_format_guid)?;
            media_type.SetUINT32(&MF_MT_AVG_BITRATE, bit_rate)?;
            MFSetAttributeSize(
                &attrs,
                &MF_MT_FRAME_SIZE,
                output_resolution.0,
                output_resolution.1,
            )?;
            MFSetAttributeRatio(&attrs, &MF_MT_FRAME_RATE, frame_rate.0, frame_rate.1)?;
            MFSetAttributeRatio(&attrs, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?; // Assume square pixels
            media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?; // Important for streaming/editing

            // Attempt to set the output type on the MFT
            transform.SetOutputType(output_stream_id, &media_type, 0)?;
            info!(
                "Successfully set Output Type ({:?}) on encoder MFT.",
                device_encoder.encoder_type
            );
            media_type // Return the configured type
        };

        // 7. Configure Input Media Type (NV12)
        // Find a compatible NV12 input type supported by the MFT.
        let input_type_set = unsafe {
            let mut type_index = 0;
            loop {
                match transform.GetInputAvailableType(input_stream_id, type_index) {
                    Ok(available_type) => {
                        // Check if it's NV12
                        match available_type.GetGUID(&MF_MT_SUBTYPE) {
                            Ok(subtype) if subtype == MFVideoFormat_NV12 => {
                                debug!("Found supported NV12 input type at index {}.", type_index);
                                // Configure the details (frame size, frame rate)
                                let attrs: IMFAttributes = available_type.cast()?;
                                MFSetAttributeSize(
                                    &attrs,
                                    &MF_MT_FRAME_SIZE,
                                    input_resolution.0,
                                    input_resolution.1,
                                )?;
                                MFSetAttributeRatio(
                                    &attrs,
                                    &MF_MT_FRAME_RATE,
                                    frame_rate.0,
                                    frame_rate.1,
                                )?;
                                MFSetAttributeRatio(&attrs, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
                                available_type.SetUINT32(
                                    &MF_MT_INTERLACE_MODE,
                                    MFVideoInterlace_Progressive.0 as u32,
                                )?;

                                // Test if the MFT accepts this fully configured type
                                match transform.SetInputType(
                                    input_stream_id,
                                    &available_type,
                                    MFT_SET_TYPE_TEST_ONLY.0 as u32,
                                ) {
                                    Ok(_) => {
                                        // Test succeeded, now set it permanently
                                        transform.SetInputType(
                                            input_stream_id,
                                            &available_type,
                                            0,
                                        )?;
                                        info!("Successfully set Input Type (NV12) on encoder MFT.");
                                        break true; // Found and set compatible type
                                    }
                                    Err(e) => {
                                        warn!("MFT rejected configured NV12 type (Test): {:?}. Trying next available type.", e);
                                        type_index += 1; // Try next available type format
                                    }
                                }
                            }
                            _ => {
                                // Not NV12, try next available type
                                type_index += 1;
                            }
                        }
                    }
                    Err(e) if e.code() == MF_E_NO_MORE_TYPES => {
                        error!("Encoder MFT does not support any suitable NV12 input format.");
                        break false; // No compatible NV12 type found
                    }
                    Err(e) => {
                        error!("Error getting available input type {}: {:?}", type_index, e);
                        return Err(e); // Propagate unexpected errors
                    }
                }
            }
        };

        if !input_type_set {
            return Err(Error::new(
                MF_E_TRANSFORM_TYPE_NOT_SET,
                "Failed to find and set a compatible NV12 input type on the encoder MFT.".into(),
            ));
        }

        // 8. Prepare Inner State for Encoding Thread
        let should_stop = Arc::new(AtomicBool::new(false));
        let frame_count = Arc::new(std::sync::atomic::AtomicU64::new(0)); // Initialize counter
        let inner = VideoEncoderInner {
            transform,
            event_generator,
            input_stream_id,
            output_stream_id,
            dxgi_manager,                    // Move ownership into inner
            sample_requested_callback: None, // To be set later
            sample_rendered_callback: None,  // To be set later
            should_stop: should_stop.clone(),
            frame_count: frame_count.clone(), // Clone Arc for inner
        };

        info!("VideoEncoder setup complete for '{}'.", device_encoder.name);
        Ok(Self {
            inner: Some(inner),
            output_type, // Store the configured output type
            started: AtomicBool::new(false),
            should_stop,
            encoder_thread_handle: None,
            frame_count, // Store Arc in outer
        })
    }

    // TODO: Implement start, stop, set_sample_requested_callback, set_sample_rendered_callback
    // TODO: Implement the encoding loop (VideoEncoderInner::encode)
}

// TODO: Implement Drop for VideoEncoder to ensure thread cleanup?

// --- Encoding Loop Logic (Placeholder) ---
impl VideoEncoderInner {
    // This function will run on the encoder thread
    fn encode(&mut self) -> Result<()> {
        info!("Encoder thread starting.");
        // Simplified loop structure based on example

        // Send initial messages
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            // Request first input immediately
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?; // Flush first? Or request input? Example requests input.
                                                                // Let's try requesting input first. The MFT should signal NEED_MORE_INPUT.
                                                                // self.transform.ProcessInput(self.input_stream_id, &MFCreateSample()?, 0)?; // Send dummy sample? No, wait for event.
        }
        info!("Encoder MFT streaming started.");

        let mut needs_input = true; // Assume we need input initially
        let mut processing_input = false; // Are we currently processing an input sample?

        loop {
            if self.should_stop.load(Ordering::SeqCst) {
                info!("Encoder stop requested.");
                break;
            }

            // --- Get Output ---
            // Always try to get output first if we aren't blocked on input
            if !needs_input {
                match self.try_get_output() {
                    Ok(true) => {
                        // Got output, continue loop to check for more output
                        continue;
                    }
                    Ok(false) => {
                        // No output available right now
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                        debug!("Encoder needs more input.");
                        needs_input = true;
                        processing_input = false; // No longer processing the last input
                    }
                    Err(e) => {
                        error!("Error during ProcessOutput: {:?}", e);
                        // Maybe check device removed reason?
                        return Err(e); // Propagate error
                    }
                }
            }

            // --- Process Input ---
            if needs_input && !processing_input {
                match self.process_next_input() {
                    Ok(true) => {
                        // Successfully submitted input
                        needs_input = false; // MFT might have output now or need more input later
                        processing_input = true; // Mark that we are processing this input
                    }
                    Ok(false) => {
                        // No more input samples available (EOS signaled)
                        info!("End of input stream signaled to encoder.");
                        // Signal EOS to MFT
                        unsafe {
                            self.transform
                                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
                        }
                        // Now just drain output
                        needs_input = false;
                        processing_input = false; // Not processing new input anymore
                                                  // TODO: Need a state to indicate we are draining
                    }
                    Err(e) => {
                        error!("Error processing input sample: {:?}", e);
                        return Err(e); // Propagate error
                    }
                }
            }

            // If we didn't get output and couldn't process input, maybe wait briefly?
            // Or rely on callbacks/events if using async MFT?
            // For synchronous, a small sleep might prevent busy-waiting if the pipeline stalls.
            // However, the example doesn't sleep here, relying on the MFT event mechanism implicitly.
            // Let's stick to the event-driven approach for now. If ProcessOutput didn't yield
            // and ProcessInput didn't run or returned EOS, the loop might spin without progress
            // if the MFT doesn't signal NEED_MORE_INPUT immediately after consuming input.
            // The example uses GetEvent, let's try that.

            // --- Use GetEvent (Alternative to simple loop check) ---
            /*
            match self.get_next_event() {
                Ok(event_type) => {
                    match event_type {
                        METransformNeedInput => {
                            if self.process_next_input()? == false {
                                // EOS
                                unsafe { self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?; }
                                // TODO: Enter draining state
                            }
                        }
                        METransformHaveOutput => {
                            self.try_get_output()?;
                        }
                        _ => {
                            warn!("Encoder thread received unhandled event type: {:?}", event_type);
                        }
                    }
                }
                Err(e) => {
                     error!("Error getting MFT event: {:?}", e);
                     return Err(e);
                }
            }
            */
            // Sticking with the simpler loop for now, assuming ProcessOutput will signal NEED_MORE_INPUT correctly.
        } // End loop

        // --- Cleanup ---
        info!("Encoder thread finishing. Sending EOS/EndStreaming messages.");
        unsafe {
            // Might have already sent END_OF_STREAM if input source ended first
            // Sending again might be okay or might error; depends on MFT.
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0); // Ignore error if already sent
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?; // Final flush
        }
        info!("Encoder thread finished cleanup.");
        Ok(())
    }

    /// Tries to get an output sample from the MFT. Returns Ok(true) if output was processed, Ok(false) if no output pending.
    fn try_get_output(&mut self) -> Result<bool> {
        let output_sample = unsafe {
            // Create a sample to receive the output
            let sample = MFCreateSample()?;
            // Create a buffer structure
            // Ensure ManuallyDrop wraps Option<IMFSample> and Option<IMFCollection>
            // Ensure ManuallyDrop wraps Option<IMFSample> and Option<IMFCollection>
            // Ensure ManuallyDrop wraps Option<IMFSample> and Option<IMFCollection>
            let mut output_buffer = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.output_stream_id,
                pSample: ManuallyDrop::new(Some(sample)),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None), // This expects Option<IMFCollection>
            };
            // ProcessOutput expects a slice
            let mut output_buffers = [output_buffer];
            let mut status_flags = 0;

            match self
                .transform
                .ProcessOutput(0, &mut output_buffers, &mut status_flags)
            {
                Ok(_) => {
                    // Success, take back ownership of the sample
                    ManuallyDrop::drop(&mut output_buffers[0].pEvents); // Drop events
                    ManuallyDrop::take(&mut output_buffers[0].pSample)
                }
                Err(e) => {
                    // Clean up the sample we created if ProcessOutput failed
                    ManuallyDrop::drop(&mut output_buffers[0].pEvents);
                    let _ = ManuallyDrop::take(&mut output_buffers[0].pSample); // Drop sample
                    return Err(e); // Return the error (could be NEED_MORE_INPUT)
                }
            }
        };

        // If sample is None here, it means ProcessOutput succeeded but didn't produce a sample? Unexpected.
        if let Some(sample) = output_sample {
            debug!("Encoder produced output sample.");
            let output_data = VideoEncoderOutputSample { sample };
            // Send it via the callback
            if let Some(callback) = self.sample_rendered_callback.as_mut() {
                callback(output_data)?;
                Ok(true) // Indicate output was processed
            } else {
                warn!("Encoder produced output, but no rendered callback is set!");
                Ok(true) // Still counts as processed output
            }
        } else {
            warn!("ProcessOutput succeeded but returned no sample.");
            Ok(false) // No output processed this time
        }
    }

    /// Requests the next input sample via callback and submits it to the MFT. Returns Ok(false) if EOS.
    fn process_next_input(&mut self) -> Result<bool> {
        if let Some(callback) = self.sample_requested_callback.as_mut() {
            match callback() {
                Ok(Some(input_sample)) => {
                    // Create an MF sample wrapping the input texture
                    let mf_input_sample = unsafe {
                        let sample = MFCreateSample()?;
                        let buffer = MFCreateDXGISurfaceBuffer(
                            &ID3D11Texture2D::IID,
                            &input_sample.texture,
                            0,
                            false,
                        )?;
                        sample.AddBuffer(&buffer)?;
                        sample.SetSampleTime(input_sample.timestamp.Duration)?;
                        // TODO: Set duration? Or is it inferred?
                        sample
                    };
                    // Add unsafe block for GetSampleTime
                    debug!("Submitting input sample with time {} to encoder.", unsafe {
                        mf_input_sample.GetSampleTime()?
                    });
                    // Submit to MFT
                    unsafe {
                        self.transform
                            .ProcessInput(self.input_stream_id, &mf_input_sample, 0)?
                    };
                    Ok(true) // Input submitted
                }
                Ok(None) => {
                    // End of stream from input source
                    Ok(false)
                }
                Err(e) => {
                    error!("Sample requested callback failed: {:?}", e);
                    Err(e) // Propagate error
                }
            }
        } else {
            warn!("Encoder needs input, but no requested callback is set!");
            // Treat as EOS if no callback is available? Or error?
            // Let's treat as EOS for now.
            Ok(false)
        }
    }

    // Helper to get next MFT event (alternative loop structure)
    /*
    fn get_next_event(&mut self) -> Result<MF_EVENT_TYPE> {
        let event = unsafe { self.event_generator.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0))? };
        Ok(MF_EVENT_TYPE(event.GetType()? as i32))
    }
    */
}

// --- Public Methods ---
impl VideoEncoder {
    pub fn try_start(&mut self) -> Result<bool> {
        if self
            .started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let mut inner = self.inner.take().expect("Encoder inner state missing");

            // Ensure callbacks are set before starting thread
            if inner.sample_requested_callback.is_none() || inner.sample_rendered_callback.is_none()
            {
                error!(
                    "Encoder cannot start: Sample requested and/or rendered callbacks are not set."
                );
                // Put inner state back
                self.inner = Some(inner);
                self.started.store(false, Ordering::SeqCst); // Reset started flag
                return Err(Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "Encoder callbacks not set".into(),
                ));
            }

            info!("Starting encoder thread...");
            self.encoder_thread_handle = Some(thread::spawn(move || {
                // TODO: Consider MFStartup/MFShutdown per thread? Or assume process-wide?
                // The example did MFStartup in the thread. Let's follow that for now.
                // unsafe { windows::Win32::Media::MediaFoundation::MFStartup( crate::media::MF_VERSION, windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL)? };

                let result = inner.encode(); // Run the encoding loop

                // TODO: MFShutdown?
                // unsafe { windows::Win32::Media::MediaFoundation::MFShutdown()? };

                if let Err(e) = &result {
                    error!("Encoder thread exited with error: {:?}", e);
                } else {
                    info!("Encoder thread finished successfully.");
                }
                result // Return result from thread
            }));
            Ok(true)
        } else {
            warn!("Encoder already started.");
            Ok(false) // Already started
        }
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.started.load(Ordering::SeqCst) {
            info!("Stopping encoder...");
            // Signal the thread to stop
            if self
                .should_stop
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                // Wait for the thread to complete
                if let Some(handle) = self.encoder_thread_handle.take() {
                    match handle.join() {
                        Ok(thread_result) => {
                            info!("Encoder thread joined.");
                            // Propagate error from thread if it occurred
                            thread_result?;
                        }
                        Err(e) => {
                            error!("Failed to join encoder thread: {:?}", e);
                            // Convert panic info to an Error?
                            return Err(Error::new(
                                windows::Win32::Foundation::E_FAIL,
                                "Failed to join encoder thread".into(),
                            ));
                        }
                    }
                }
                self.started.store(false, Ordering::SeqCst); // Mark as stopped
                info!("Encoder stopped.");
            } else {
                // Already signaled to stop
                debug!("Encoder stop already signaled.");
            }
        } else {
            debug!("Encoder already stopped or not started.");
        }
        Ok(())
    }

    pub fn set_sample_requested_callback<F>(&mut self, callback: F)
    where
        F: 'static + Send + FnMut() -> Result<Option<VideoEncoderInputSample>>,
    {
        if let Some(inner) = self.inner.as_mut() {
            inner.sample_requested_callback = Some(Box::new(callback));
            debug!("Sample requested callback set.");
        } else {
            warn!("Cannot set sample requested callback: Encoder already started or failed initialization.");
        }
    }

    pub fn set_sample_rendered_callback<F>(&mut self, callback: F)
    where
        F: 'static + Send + FnMut(VideoEncoderOutputSample) -> Result<()>,
    {
        if let Some(inner) = self.inner.as_mut() {
            inner.sample_rendered_callback = Some(Box::new(callback));
            debug!("Sample rendered callback set.");
        } else {
            warn!("Cannot set sample rendered callback: Encoder already started or failed initialization.");
        }
    }
    pub fn output_type(&self) -> &IMFMediaType {
        &self.output_type
    }

    /// Returns the number of frames successfully encoded.
    pub fn frame_count(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }

    /// Checks if the encoder thread has finished (e.g., due to panic or completion).
    pub fn is_finished(&self) -> bool {
        self.encoder_thread_handle
            .as_ref()
            .map_or(true, |h| h.is_finished())
    }
}

// Ensure Inner is Send (needed for thread::spawn)
unsafe impl Send for VideoEncoderInner {}

// TODO: Implement Drop for VideoEncoder to ensure stop() is called?
impl Drop for VideoEncoder {
    fn drop(&mut self) {
        if self.started.load(Ordering::SeqCst) {
            warn!("VideoEncoder dropped while still running. Attempting to stop...");
            if let Err(e) = self.stop() {
                error!("Error stopping encoder during drop: {:?}", e);
            }
        }
    }
}
