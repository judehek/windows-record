use crate::Result; use log::{info, warn};
use windows::{
    core::{GUID, PWSTR},
    Win32::{
        Media::MediaFoundation::{
            IMFActivate, MFMediaType_Video, MFTEnumEx, MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_H264, MFVideoFormat_HEVC, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_ALL, MFT_ENUM_FLAG_SORTANDFILTER, MFT_REGISTER_TYPE_INFO, MF_E_NOT_FOUND// Use sorting/filtering to potentially get preferred encoders first
        }, System::Com::CoTaskMemFree
    },
};
use std::collections::HashSet;
use crate::device::ensure_com_initialized;

/// Represents a video encoder option discovered on the system
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VideoEncoder {
    /// Unique identifier for the *output format* (e.g., H.264, HEVC) this encoder supports (not encoder's CLSID, but the format)
    pub output_format_guid: GUID,
    /// Human-readable name for the encoder
    pub name: String,
    /// The specific type enum corresponding to the output_format_guid
    pub encoder_type: VideoEncoderType,
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
    fn get_guid(&self) -> GUID {
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

/// Returns a list of available video encoders found that match the specified types
pub fn enumerate_video_encoders() -> Result<Vec<VideoEncoder>> {
    info!("Starting video encoder enumeration");

    // Ensure COM is initialized for the current thread
    info!("Ensuring COM is initialized");
    let _com_guard = ensure_com_initialized()?;
    info!("COM initialization successful");

    let mut available_encoders = Vec::new();
    // HS to prevent adding duplicates
    let mut found_encoders = HashSet::<(String, GUID)>::new();
    info!("Initialized collections for tracking encoders");

    let types_to_check = vec![VideoEncoderType::H264, VideoEncoderType::HEVC];
    info!("Will check for encoder types: {:?}", types_to_check);

    for encoder_type in types_to_check {
        let output_format_guid = encoder_type.get_guid();
        info!("Checking for encoder type: {:?} with GUID: {:?}", encoder_type, output_format_guid);

        let output_type_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: output_format_guid,
        };
        info!("Created output type info with MajorType: {:?}, SubType: {:?}",
              MFMediaType_Video, output_format_guid);

        let mut p_activate_array_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        info!("Initialized p_activate_array_ptr to null and count to 0");

        info!("About to call MFTEnumEx with category: MFT_CATEGORY_VIDEO_ENCODER and flags: MFT_ENUM_FLAG_ALL | MFT_ENUM_FLAG_SORTANDFILTER");
        let enum_result: windows::core::Result<()> = unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_ALL | MFT_ENUM_FLAG_SORTANDFILTER, // Pass flags directly
                None,                       // pInputType: No input type constraint
                Some(&output_type_info),    // pOutputType: Constrain to H.264 or HEVC output
                &mut p_activate_array_ptr,  // Pass the address OF p_activate_array_ptr
                &mut count,                 // Receives the count
            )
        };

        info!("MFTEnumEx call completed with result: {:?}, count: {}", enum_result, count);
        info!("p_activate_array_ptr is null: {}", p_activate_array_ptr.is_null());

        if let Err(e) = &enum_result {
            if e.code() == MF_E_NOT_FOUND {
                info!("No encoders found for type: {:?} ({:?})", encoder_type, output_format_guid);
                continue; // Continue to the next encoder type
            } else {
                // Log the specific error but let the `?` propagate it
                warn!("MFTEnumEx failed with error: {:?}", e);
            }
        }
        // Propagate errors
        enum_result?;
        if count > 0 && !p_activate_array_ptr.is_null() {
             info!("Found {} encoders for type: {:?}", count, encoder_type);

            let activates_slice: &[Option<IMFActivate>] = unsafe {
                core::slice::from_raw_parts(p_activate_array_ptr, count as usize)
            };
            info!("Created slice from p_activate_array_ptr with length: {}", activates_slice.len());


            for (i, activate_opt) in activates_slice.iter().enumerate() {
                info!("Processing encoder {} of {}", i + 1, count);

                if let Some(activate) = activate_opt {
                    info!("Found valid IMFActivate at index {}", i);

                    // Now 'activate' is an IMFActivate
                    let mut name_ptr: PWSTR = PWSTR::null();
                    info!("About to call GetAllocatedString for MFT_FRIENDLY_NAME_Attribute");
                    let name_result = unsafe {
                        activate.GetAllocatedString(
                            &MFT_FRIENDLY_NAME_Attribute,
                            &mut name_ptr,
                            std::ptr::null_mut(),
                        )
                    };

                    info!("GetAllocatedString result: {:?}, name_ptr is null: {}",
                         name_result, name_ptr.is_null());

                    if name_result.is_ok() && !name_ptr.is_null() {
                        let encoder_name = unsafe { name_ptr.to_string() }.unwrap_or_default();
                        unsafe { CoTaskMemFree(Some(name_ptr.as_ptr() as *const _)) };
                        info!("Retrieved encoder name: '{}' and freed name_ptr memory", encoder_name);


                        if !encoder_name.is_empty() {
                            let key = (encoder_name.clone(), output_format_guid);
                            let is_new = found_encoders.insert(key);
                            if is_new {
                                info!("Adding new encoder: '{}' for type: {:?}", encoder_name, encoder_type);
                                available_encoders.push(VideoEncoder {
                                    output_format_guid,
                                    name: encoder_name,
                                    encoder_type,
                                });
                            } else {
                                info!("Encoder '{}' for type: {:?} already in the list (different GUID or duplicate listing), skipping add",
                                      encoder_name, encoder_type);
                            }
                        } else {
                            warn!("Empty encoder name retrieved, skipping add");
                        }
                    } else {
                        warn!("Failed to get encoder name, GetAllocatedString result: {:?}", name_result);
                    }
                    info!("Finished processing encoder at index {}", i);
                } else {
                     warn!("Found NULL IMFActivate entry at index {}, skipping", i);
                     // Shouldn't happen
                }
            }

            info!("About to free memory for the encoder array pointed to by p_activate_array_ptr");
            unsafe { CoTaskMemFree(Some(p_activate_array_ptr as *const _)) };
            info!("Freed memory for the encoder array");

            p_activate_array_ptr = std::ptr::null_mut();
            info!("Set p_activate_array_ptr back to null");

        } else if count == 0 {
             info!("No encoders found for type: {:?} (count returned 0)", encoder_type);
        } else {
            warn!("MFTEnumEx returned Ok with count {} but the activate pointer is null. This is unexpected.", count);
        }
        info!("Completed processing for encoder type: {:?}", encoder_type);
    }

    info!("Found {} unique video encoders.", available_encoders.len());
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
pub fn get_preferred_video_encoder_by_type(
    encoder_type: VideoEncoderType,
) -> Option<VideoEncoder> {
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
    get_preferred_video_encoder_by_type(encoder_type)
        .ok_or_else(|| {
            log::error!("No video encoder found for type: {:?}", encoder_type);
            crate::error::RecorderError::Generic(format!("No video encoder found for type: {:?}", encoder_type))
        })
}