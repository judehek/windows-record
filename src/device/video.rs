use crate::Result;
use log::{debug, warn}; // Changed info to debug for less noise
use windows::{
    core::{ComInterface, GUID, PWSTR}, // Added ComInterface
    Win32::{
        Media::MediaFoundation::{
            IMFActivate,
            IMFTransform,
            MFMediaType_Video,
            MFTEnumEx,
            MFT_FRIENDLY_NAME_Attribute,
            MFVideoFormat_H264,
            MFVideoFormat_HEVC,
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE,
            MFT_ENUM_FLAG_SORTANDFILTER,
            MFT_ENUM_FLAG_TRANSCODE_ONLY, // Added specific flags
            MFT_REGISTER_TYPE_INFO,
            MF_E_NOT_FOUND,
        },
        System::Com::CoTaskMemFree,
    },
};
// Removed HashSet import
use crate::device::ensure_com_initialized;

/// Represents a video encoder option discovered on the system
#[derive(Debug, Clone)] // Removed PartialEq, Eq, Hash as IMFActivate doesn't derive them
pub struct VideoEncoder {
    /// The activation object for the encoder's Media Foundation Transform (MFT).
    /// Can be used later to create the actual IMFTransform.
    activate: IMFActivate, // Store IMFActivate directly
    /// Human-readable name for the encoder
    pub name: String,
    /// The specific type enum corresponding to the output format
    pub encoder_type: VideoEncoderType,
    // Removed output_format_guid, can be inferred from encoder_type if needed
}

/// Available video encoder types that the recorder supports querying for
#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum VideoEncoderType {
    /// H.264 Advanced Video Coding
    H264,
    /// H.265 High Efficiency Video Coding
    HEVC,
}

impl Default for VideoEncoderType {
    fn default() -> Self {
        Self::H264
    }
}

impl VideoEncoderType {
    /// Gets the corresponding Media Foundation Video Format GUID
    pub fn get_guid(&self) -> GUID {
        // Make this public
        match self {
            VideoEncoderType::H264 => MFVideoFormat_H264,
            VideoEncoderType::HEVC => MFVideoFormat_HEVC,
        }
    }

    /// Tries to create a VideoEncoderType from a GUID
    fn from_guid(guid: &GUID) -> Option<Self> {
        if guid == &MFVideoFormat_H264 {
            Some(Self::H264)
        } else if guid == &MFVideoFormat_HEVC {
            Some(Self::HEVC)
        } else {
            None // Unknown
        }
    }
}

// Moved create_transform to impl VideoEncoder below

impl VideoEncoder {
    // Added impl block for VideoEncoder
    /// Creates the underlying Media Foundation Transform (IMFTransform) for this encoder.
    pub fn create_transform(&self) -> Result<IMFTransform> {
        Ok(unsafe { self.activate.ActivateObject()? })
    }
}

// SAFETY: VideoEncoder holds an IMFActivate, which is designed for marshaling.
// We ensure it's only activated (used) within the thread it's sent to.
unsafe impl Send for VideoEncoder {}

/// Returns a list of available video encoders found that match the specified types.
/// Prefers hardware encoders suitable for transcoding.
pub fn enumerate_video_encoders() -> Result<Vec<VideoEncoder>> {
    debug!("Starting video encoder enumeration");

    // Ensure COM is initialized for the current thread
    debug!("Ensuring COM is initialized");
    let _com_guard = ensure_com_initialized()?;
    debug!("COM initialization successful");

    let mut available_encoders: Vec<VideoEncoder> = Vec::new(); // Add type annotation
                                                                // Removed HashSet for deduplication, relying on flags and later checks if needed.
    debug!("Initialized collections for tracking encoders");

    let types_to_check = vec![VideoEncoderType::H264, VideoEncoderType::HEVC];
    debug!("Will check for encoder types: {:?}", types_to_check);

    for encoder_type in types_to_check {
        let output_format_guid = encoder_type.get_guid();
        debug!(
            "Checking for encoder type: {:?} with GUID: {:?}",
            encoder_type, output_format_guid
        );

        let output_type_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: output_format_guid,
        };
        debug!(
            "Created output type info with MajorType: {:?}, SubType: {:?}",
            MFMediaType_Video, output_format_guid
        );

        let mut p_activate_array_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        debug!("Initialized p_activate_array_ptr to null and count to 0");

        // Use more specific flags like the example
        let flags =
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_TRANSCODE_ONLY | MFT_ENUM_FLAG_SORTANDFILTER;
        debug!(
            "About to call MFTEnumEx with category: MFT_CATEGORY_VIDEO_ENCODER and flags: {:?}",
            flags
        );
        let enum_result: windows::core::Result<()> = unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                None,                    // pInputType: No input type constraint
                Some(&output_type_info), // pOutputType: Constrain to H.264 or HEVC output
                &mut p_activate_array_ptr,
                &mut count,
            )
        };

        debug!(
            "MFTEnumEx call completed with result: {:?}, count: {}",
            enum_result, count
        );
        debug!(
            "p_activate_array_ptr is null: {}",
            p_activate_array_ptr.is_null()
        );

        if let Err(e) = &enum_result {
            if e.code() == MF_E_NOT_FOUND {
                debug!(
                    "No hardware encoders found for type: {:?} ({:?})",
                    encoder_type, output_format_guid
                );
                continue; // No hardware encoders found for this type, try next
            } else {
                // Log the specific error but let the `?` propagate it
                warn!("MFTEnumEx failed with error: {:?}", e);
            }
        }
        // Propagate errors
        enum_result?;
        if count > 0 && !p_activate_array_ptr.is_null() {
            debug!(
                "Found {} potential encoders for type: {:?}",
                count, encoder_type
            );

            let activates_slice: &[Option<IMFActivate>] =
                unsafe { core::slice::from_raw_parts(p_activate_array_ptr, count as usize) };
            debug!(
                "Created slice from p_activate_array_ptr with length: {}",
                activates_slice.len()
            );

            for (i, activate_opt) in activates_slice.iter().enumerate() {
                debug!("Processing encoder {} of {}", i + 1, count);

                if let Some(activate) = activate_opt {
                    debug!("Found valid IMFActivate at index {}", i);
                    let activate = activate.clone(); // Clone to take ownership

                    // Get Friendly Name
                    let mut name_ptr: PWSTR = PWSTR::null();
                    debug!("About to call GetAllocatedString for MFT_FRIENDLY_NAME_Attribute");
                    let name_result = unsafe {
                        activate.GetAllocatedString(
                            &MFT_FRIENDLY_NAME_Attribute,
                            &mut name_ptr,
                            std::ptr::null_mut(),
                        )
                    };

                    debug!(
                        "GetAllocatedString result: {:?}, name_ptr is null: {}",
                        name_result,
                        name_ptr.is_null()
                    );

                    if name_result.is_ok() && !name_ptr.is_null() {
                        let encoder_name = unsafe { name_ptr.to_string() }.unwrap_or_default();
                        unsafe { CoTaskMemFree(Some(name_ptr.as_ptr() as *const _)) }; // Free the allocated string
                        debug!("Retrieved encoder name: '{}'", encoder_name);

                        if !encoder_name.is_empty() {
                            // Simple check: Add if name is not already present for this type.
                            // More robust deduplication might be needed if names aren't unique across types/flags.
                            // Explicitly check for duplicates before adding
                            let mut already_exists = false;
                            for existing_encoder in &available_encoders {
                                // Remove incorrect type annotation syntax
                                if existing_encoder.name == encoder_name
                                    && existing_encoder.encoder_type == encoder_type
                                {
                                    already_exists = true;
                                    break;
                                }
                            }

                            if !already_exists {
                                debug!(
                                    "Adding new encoder: '{}' for type: {:?}",
                                    encoder_name, encoder_type
                                );
                                available_encoders.push(VideoEncoder {
                                    activate, // Store the cloned IMFActivate
                                    name: encoder_name,
                                    encoder_type,
                                });
                            } else {
                                debug!(
                                    "Encoder '{}' for type {:?} already found, skipping.",
                                    encoder_name, encoder_type
                                );
                            }
                        } else {
                            warn!("Empty encoder name retrieved, skipping add");
                        }
                    } else {
                        warn!(
                            "Failed to get encoder name, GetAllocatedString result: {:?}",
                            name_result
                        );
                    }
                    debug!("Finished processing encoder at index {}", i);
                } else {
                    warn!("Found NULL IMFActivate entry at index {}, skipping", i);
                }
            }

            // Free the array allocated by MFTEnumEx
            debug!("About to free memory for the encoder array pointed to by p_activate_array_ptr");
            unsafe { CoTaskMemFree(Some(p_activate_array_ptr as *const _)) };
            debug!("Freed memory for the encoder array");

            // No need to null the pointer, it goes out of scope
        } else if count == 0 {
            debug!(
                "No encoders found for type: {:?} (count returned 0)",
                encoder_type
            );
        } else {
            // This case (count > 0 but ptr is null) should ideally not happen if MFTEnumEx succeeded.
            warn!("MFTEnumEx returned Ok with count {} but the activate pointer is null. This is unexpected.", count);
        }
        debug!("Completed processing for encoder type: {:?}", encoder_type);
    }

    debug!("Found {} unique video encoders.", available_encoders.len());
    Ok(available_encoders)
}

/// Gets the first available video encoder matching the specified name.
pub fn get_video_encoder_by_name(name: &str) -> Option<VideoEncoder> {
    match enumerate_video_encoders() {
        Ok(encoders) => encoders.into_iter().find(|encoder| encoder.name == name),
        Err(e) => {
            eprintln!("Error enumerating video encoders: {:?}", e);
            None
        }
    }
}

/// Gets the *first available* video encoder matching the specified type (e.g., H.264 or HEVC).
pub fn get_preferred_video_encoder_by_type(encoder_type: VideoEncoderType) -> Option<VideoEncoder> {
    match enumerate_video_encoders() {
        Ok(encoders) => encoders
            .into_iter()
            .find(|encoder| encoder.encoder_type == encoder_type),
        Err(e) => {
            eprintln!("Error enumerating video encoders: {:?}", e);
            None
        }
    }
}

/// Alias for get_preferred_video_encoder_by_type for backward compatibility.
pub fn get_video_encoder_by_type(
    encoder_type: VideoEncoderType,
) -> std::result::Result<VideoEncoder, crate::error::RecorderError> {
    get_preferred_video_encoder_by_type(encoder_type).ok_or_else(|| {
        log::error!("No video encoder found for type: {:?}", encoder_type);
        crate::error::RecorderError::Generic(format!(
            "No video encoder found for type: {:?}",
            encoder_type
        ))
    })
}
