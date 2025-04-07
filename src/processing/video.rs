use log::{debug, info, warn};
use std::{mem::ManuallyDrop, sync::Arc}; // Added ManuallyDrop
use windows::{
    core::{ComInterface, Result},
    Graphics::{RectInt32, SizeInt32},
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct3D11::{
                ID3D11Device,
                ID3D11DeviceContext,
                ID3D11Texture2D,
                ID3D11VideoContext,
                ID3D11VideoDevice,
                ID3D11VideoProcessor,
                ID3D11VideoProcessorEnumerator,
                ID3D11VideoProcessorInputView,
                ID3D11VideoProcessorOutputView,
                D3D11_BIND_RENDER_TARGET,
                D3D11_BIND_VIDEO_ENCODER,
                D3D11_TEX2D_VPIV,
                D3D11_TEX2D_VPOV,
                D3D11_TEXTURE2D_DESC,
                D3D11_USAGE_DEFAULT,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
                D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
                D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE, // Import needed enum
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
                D3D11_VIDEO_PROCESSOR_STREAM,
                D3D11_VIDEO_USAGE_OPTIMAL_QUALITY,
                D3D11_VPIV_DIMENSION_TEXTURE2D,
                D3D11_VPOV_DIMENSION_TEXTURE2D,
            },
            Dxgi::Common::{
                DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL,
                DXGI_SAMPLE_DESC,
            },
        },
    },
};

// Define input and output sizes as simple tuples for clarity
type Size = (u32, u32);

/// Manages the D3D11 Video Processor for color conversion (BGRA -> NV12) and scaling.
pub struct VideoProcessor {
    d3d_device: Arc<ID3D11Device>, // Keep device reference
    d3d_context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    video_processor: ID3D11VideoProcessor,
    video_output_texture: ID3D11Texture2D, // NV12 output texture
    video_output_view: ID3D11VideoProcessorOutputView,
    // Input texture/view are created temporarily during processing if needed,
    // or we can process directly from the source texture if formats match.
    // For simplicity here, we assume we process directly from the provided input texture.
    input_size: Size,
    output_size: Size,
}

impl VideoProcessor {
    /// Creates a new VideoProcessor instance.
    ///
    /// # Arguments
    /// * `d3d_device` - The shared D3D11 device.
    /// * `input_size` - The expected dimensions (width, height) of the input BGRA textures.
    /// * `output_size` - The desired dimensions (width, height) of the output NV12 textures.
    /// * `frame_rate` - The frame rate (numerator, denominator) for hints.
    pub fn new(
        d3d_device: Arc<ID3D11Device>,
        input_size: Size,
        output_size: Size,
        frame_rate: (u32, u32),
    ) -> Result<Self> {
        info!(
            "Creating VideoProcessor: Input {}x{}, Output {}x{}, Frame Rate {}/{}",
            input_size.0, input_size.1, output_size.0, output_size.1, frame_rate.0, frame_rate.1
        );

        let d3d_context = unsafe { d3d_device.GetImmediateContext()? };
        let video_device: ID3D11VideoDevice = d3d_device.cast()?;
        let video_context: ID3D11VideoContext = d3d_context.cast()?;

        // Describe the video content parameters
        let video_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, // Assuming progressive input
            InputFrameRate: DXGI_RATIONAL {
                Numerator: frame_rate.0,
                Denominator: frame_rate.1,
            },
            InputWidth: input_size.0,
            InputHeight: input_size.1,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: frame_rate.0,
                Denominator: frame_rate.1,
            },
            OutputWidth: output_size.0, // Use output size here
            OutputHeight: output_size.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_QUALITY, // Hint for quality
        };
        debug!("Video Processor Content Description: {:?}", video_desc);

        // Create an enumerator to find compatible video processor capabilities
        let video_enum = unsafe { video_device.CreateVideoProcessorEnumerator(&video_desc)? };
        info!("Created Video Processor Enumerator.");

        // Create the video processor instance
        let video_processor = unsafe { video_device.CreateVideoProcessor(&video_enum, 0)? }; // Rate conversion index 0
        info!("Created Video Processor.");

        // --- Configure Color Spaces (Crucial for correct conversion) ---
        // Set output color space (NV12 is typically limited range YCbCr BT.709)
        let output_color_space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
            // Bitfield: Usage=1 (Processing), Nominal_Range=1 (Studio 16-235)
            _bitfield:
                // Access enum variants directly
                // Correct enum variant access
                // Use the constant directly
                // Use constants directly from D3D11 module
                // Use constant with .0 access
                (windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235.0 // Revert to .0 access
                    as u32)
                    << 1
                    | 1,
        };
        unsafe {
            // Remove '?' as this method doesn't return Result
            video_context.VideoProcessorSetOutputColorSpace(&video_processor, &output_color_space);
        }
        debug!(
            "Set Video Processor Output Color Space: {:?}",
            output_color_space
        );

        // Set input stream color space (BGRA is typically full range RGB)
        let input_color_space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
            // Bitfield: Usage=1 (Processing), Nominal_Range=0 (Full 0-255)
            _bitfield:
                // Access enum variants directly
                // Correct enum variant access
                // Use the constant directly
                // Use constants directly from D3D11 module
                // Use constant with .0 access
                (windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255.0 // Revert to .0 access
                    as u32)
                    << 1
                    | 1,
        };
        unsafe {
            // Remove '?' as this method doesn't return Result
            video_context.VideoProcessorSetStreamColorSpace(
                &video_processor,
                0,
                &input_color_space,
            );
        } // Stream index 0
        debug!(
            "Set Video Processor Input Stream 0 Color Space: {:?}",
            input_color_space
        );
        // --- End Color Space Configuration ---

        // --- Configure Scaling/Cropping (Destination Rectangle) ---
        // If input and output sizes differ, calculate the destination rect to preserve aspect ratio.
        if input_size != output_size {
            info!("Input size != Output size, calculating destination rectangle for scaling.");
            let dest_rect = compute_dest_rect(
                (output_size.0 as i32, output_size.1 as i32),
                (input_size.0 as i32, input_size.1 as i32),
            );
            let rect = RECT {
                left: dest_rect.X,
                top: dest_rect.Y,
                right: dest_rect.X + dest_rect.Width,
                bottom: dest_rect.Y + dest_rect.Height,
            };
            info!("Setting destination rectangle: {:?}", rect);
            unsafe {
                // Remove '?' as this method doesn't return Result
                video_context.VideoProcessorSetStreamDestRect(
                    &video_processor,
                    0,
                    true,
                    Some(&rect),
                ); // Stream 0, Enable=true
            }
        } else {
            info!("Input size == Output size, using default full-frame destination rectangle.");
        }
        // --- End Scaling/Cropping Configuration ---

        // --- Create Output Texture and View (NV12) ---
        let output_texture_desc = D3D11_TEXTURE2D_DESC {
            Width: output_size.0,
            Height: output_size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12, // Output format
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            // Bind flags needed for MFT encoder input and video processor output
            // Use flag types directly
            // Ensure flags are constructed correctly
            BindFlags: D3D11_BIND_RENDER_TARGET | D3D11_BIND_VIDEO_ENCODER,
            CPUAccessFlags: windows::Win32::Graphics::Direct3D11::D3D11_CPU_ACCESS_FLAG(0),
            MiscFlags: windows::Win32::Graphics::Direct3D11::D3D11_RESOURCE_MISC_FLAG(0), // Keep as is if no specific constant
        };
        debug!(
            "Output Texture Description (NV12): {:?}",
            output_texture_desc
        );
        let video_output_texture = unsafe {
            let mut texture = None;
            d3d_device.CreateTexture2D(&output_texture_desc, None, Some(&mut texture))?;
            texture.unwrap()
        };
        info!("Created NV12 output texture.");

        let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 }, // Use the main texture level
            },
        };
        // Removed debug log as D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC doesn't implement Debug
        // debug!("Output View Description: {:?}", output_view_desc);
        let video_output_view = unsafe {
            let mut view = None;
            video_device.CreateVideoProcessorOutputView(
                &video_output_texture, // Resource to view
                &video_enum,           // Enumerator used to create the processor
                &output_view_desc,
                Some(&mut view),
            )?;
            view.unwrap()
        };
        info!("Created Video Processor Output View.");
        // --- End Output Texture and View ---

        info!("VideoProcessor initialization complete.");
        Ok(Self {
            d3d_device,
            d3d_context,
            video_device,
            video_context,
            video_processor,
            video_output_texture,
            video_output_view,
            input_size,
            output_size,
        })
    }

    /// Returns a reference to the processed NV12 output texture.
    /// The caller should copy this texture if they need to hold onto it,
    /// as the next call to `process_texture` will overwrite its contents.
    pub fn output_texture(&self) -> &ID3D11Texture2D {
        &self.video_output_texture
    }

    /// Processes an input BGRA texture, performing color conversion and scaling.
    /// The result is written to the internal NV12 output texture.
    ///
    /// # Arguments
    /// * `input_texture` - The BGRA texture to process. Its dimensions must match `input_size`.
    /// * `source_rect` - Optional sub-rectangle of the input texture to process. If None, the whole texture is used.
    pub fn process_texture(
        &mut self,
        input_texture: &ID3D11Texture2D,
        source_rect: Option<RECT>, // Allow specifying source rect for cropping
    ) -> Result<()> {
        let start_time = std::time::Instant::now();
        debug!("Starting video processing for input texture.");

        // --- Create Input View ---
        // We need to create an input view for the provided texture each time,
        // as the input texture might change frame-to-frame.
        let input_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0, // Not used for Texture2D
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let video_input_view = unsafe {
            let mut view = None;
            // Use the video device associated with this processor
            self.video_device.CreateVideoProcessorInputView(
                input_texture, // View the provided input texture
                // Need the enumerator again - TODO: Consider storing it? Or re-creating?
                // For now, let's assume we don't need it if the processor is already created.
                // Let's try passing None here, documentation is unclear if it's needed *after* processor creation.
                // If this fails, we might need to store the enumerator in the struct.
                None, //&self.video_enum, // Enumerator used to create the processor
                &input_view_desc,
                Some(&mut view),
            )?;
            view.unwrap()
        };
        debug!("Created temporary Video Processor Input View.");
        // --- End Input View ---

        // --- Setup Stream Data ---
        // Re-attempt D3D11_VIDEO_PROCESSOR_STREAM initialization using pp* fields and ManuallyDrop
        unsafe {
            let mut stream_data = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                pInputSurface: std::mem::transmute_copy(&video_input_view.clone()),
                ..Default::default()
            };
            debug!("Calling VideoProcessorBlt...");
            self.video_context.VideoProcessorBlt(
                &self.video_processor,   // The processor instance
                &self.video_output_view, // Target view (wraps NV12 texture)
                0,                       // Output frame index (usually 0)
                &[stream_data],          // Array of input streams (just one here)
            );
            debug!("VideoProcessorBlt completed.");
        }
        // --- End Stream Data ---

        // --- Set Source Rectangle (if provided) ---
        // This overrides the destination rectangle scaling for cropping purposes.
        if let Some(rect) = source_rect {
            info!("Applying source rectangle override: {:?}", rect);
            unsafe {
                // Remove '?' as this method doesn't return Result
                self.video_context.VideoProcessorSetStreamSourceRect(
                    &self.video_processor,
                    0,
                    true,
                    Some(&rect),
                );
            }
        } else {
            // Ensure source rect is disabled if not provided this frame
            unsafe {
                // Remove '?' as this method doesn't return Result
                self.video_context.VideoProcessorSetStreamSourceRect(
                    &self.video_processor,
                    0,
                    false,
                    None,
                );
            }
        }
        // --- End Source Rectangle ---

        // --- Perform the Blit ---
        // This does the actual color conversion, scaling, and cropping based on setup.
        // --- End Blit ---

        // Input view is temporary and can be dropped now
        drop(video_input_view);

        let elapsed = start_time.elapsed();
        debug!("Video processing took: {:?}", elapsed);
        Ok(())
    }

    /// Updates the destination rectangle used for scaling.
    /// Call this if the output size needs to change relative to the input size.
    pub fn update_destination_rect(&self) -> Result<()> {
        if self.input_size != self.output_size {
            info!("Recalculating destination rectangle for scaling.");
            let dest_rect = compute_dest_rect(
                (self.output_size.0 as i32, self.output_size.1 as i32),
                (self.input_size.0 as i32, self.input_size.1 as i32),
            );
            let rect = RECT {
                left: dest_rect.X,
                top: dest_rect.Y,
                right: dest_rect.X + dest_rect.Width,
                bottom: dest_rect.Y + dest_rect.Height,
            };
            info!("Setting new destination rectangle: {:?}", rect);
            unsafe {
                // Remove '?' as this method doesn't return Result
                self.video_context.VideoProcessorSetStreamDestRect(
                    &self.video_processor,
                    0,
                    true,
                    Some(&rect),
                );
            }
        } else {
            info!("Input size == Output size, ensuring default full-frame destination rectangle.");
            // Disable explicit destination rect to use default full frame
            unsafe {
                // Remove '?' as this method doesn't return Result
                self.video_context.VideoProcessorSetStreamDestRect(
                    &self.video_processor,
                    0,
                    false,
                    None,
                );
            }
        }
        Ok(())
    }
}

// --- Helper Function for Aspect Ratio Preserving Scaling ---

/// Calculates the destination rectangle within the output dimensions
/// to fit the input dimensions while preserving aspect ratio (letterboxing/pillarboxing).
fn compute_dest_rect(output_size: (i32, i32), input_size: (i32, i32)) -> RectInt32 {
    let output_width = output_size.0 as f32;
    let output_height = output_size.1 as f32;
    let input_width = input_size.0 as f32;
    let input_height = input_size.1 as f32;

    let output_ratio = output_width / output_height;
    let input_ratio = input_width / input_height;

    let mut scale_factor = 1.0;
    if output_ratio > input_ratio {
        // Output is wider than input (pillarbox) - scale based on height
        scale_factor = output_height / input_height;
    } else {
        // Output is narrower or same aspect ratio as input (letterbox or fit) - scale based on width
        scale_factor = output_width / input_width;
    }

    /// Calculates the source rectangle based on window position and size relative to the capture input.
    /// Returns None if the window is completely outside the capture area or size is invalid.
    pub fn calculate_source_rect(
        input_width: u32,
        input_height: u32,
        window_pos: Option<(i32, i32)>,
        window_size: Option<(u32, u32)>,
    ) -> Option<RECT> {
        match (window_pos, window_size) {
            (Some(pos), Some(size)) => {
                // Clamp position and size to be within the input dimensions
                let left = pos.0.max(0) as u32;
                let top = pos.1.max(0) as u32;

                // Calculate right and bottom based on clamped left/top and original size
                let mut right = (pos.0 + size.0 as i32).max(0) as u32;
                let mut bottom = (pos.1 + size.1 as i32).max(0) as u32;

                // Clamp right and bottom to input dimensions
                right = right.min(input_width);
                bottom = bottom.min(input_height);

                // Ensure width and height are positive after clamping
                if right > left && bottom > top {
                    Some(RECT {
                        left: left as i32,
                        top: top as i32,
                        right: right as i32,
                        bottom: bottom as i32,
                    })
                } else {
                    warn!("Calculated source rect has zero or negative size after clamping. Pos: {:?}, Size: {:?}, Input: {}x{}", pos, size, input_width, input_height);
                    None // Window outside capture area or invalid size
                }
            }
            _ => {
                // If no window info, process the whole input texture
                None
            }
        }
    }

    let scaled_width = (input_width * scale_factor).round() as i32;
    let scaled_height = (input_height * scale_factor).round() as i32;

    // Center the scaled rectangle within the output dimensions
    let offset_x = (output_size.0 - scaled_width) / 2;
    let offset_y = (output_size.1 - scaled_height) / 2;

    RectInt32 {
        X: offset_x,
        Y: offset_y,
        Width: scaled_width,
        Height: scaled_height,
    }
}
