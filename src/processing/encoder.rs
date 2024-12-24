use log::{error, info};
use windows::core::{ComInterface, Error, IUnknown, Interface, Result, GUID};
use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use crate::error::RecorderError;
use std::collections::HashMap;
use std::ffi::c_void;

#[derive(Debug, Clone)]
pub struct EncoderInfo {
    pub name: String,
    pub guid: GUID,
    pub media_type: MediaType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaType {
    Video,
    Audio,
}

pub(crate) fn ensure_mf_initialized() -> Result<()> {
    unsafe {
        info!("Initializing COM...");
        let hr = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED
        );
        if let Err(e) = hr {
            error!("COM initialization failed: {:?}", e);
            return Err(e);
        }
        info!("COM initialized successfully");
        
        info!("Starting Media Foundation...");
        let hr = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        if let Err(e) = hr {
            error!("Media Foundation startup failed: {:?}", e);
            return Err(e);
        }
        info!("Media Foundation started successfully");
    }
    Ok(())
}

pub fn get_available_encoders() -> Result<HashMap<String, EncoderInfo>> {
    unsafe {
        info!("Starting encoder enumeration...");
        let mut encoders = HashMap::new();
        
        // Try different flag combinations for video encoders
        let flag_combinations = [
            MFT_ENUM_FLAG_ALL,
            MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0),
            MFT_ENUM_FLAG(MFT_ENUM_FLAG_ASYNCMFT.0),
            MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SYNCMFT.0),
        ];

        for &flags in &flag_combinations {
            info!("Trying video enumeration with flags: {:?}", flags.0);
            if let Ok(video_encoders) = enumerate_transforms(&MFT_CATEGORY_VIDEO_ENCODER, flags) {
                if !video_encoders.is_empty() {
                    info!("Found {} video encoders with flags {:?}", video_encoders.len(), flags.0);
                    for (encoder, clsid) in video_encoders {
                        if let Ok(info) = get_encoder_info(&encoder, clsid, MediaType::Video) {
                            encoders.insert(info.name.clone(), info);
                        }
                    }
                    break;  // Found encoders, no need to try other flags
                }
            }
        }

        // Similar for audio encoders
        for &flags in &[MFT_ENUM_FLAG_SYNCMFT, MFT_ENUM_FLAG_ASYNCMFT] {
            info!("Trying audio enumeration with flags: {:?}", flags.0);
            if let Ok(audio_encoders) = enumerate_transforms(&MFT_CATEGORY_AUDIO_ENCODER, flags) {
                if !audio_encoders.is_empty() {
                    info!("Found {} audio encoders with flags {:?}", audio_encoders.len(), flags.0);
                    for (encoder, clsid) in audio_encoders {
                        if let Ok(info) = get_encoder_info(&encoder, clsid, MediaType::Audio) {
                            encoders.insert(info.name.clone(), info);
                        }
                    }
                    break;
                }
            }
        }

        info!("Total encoders found: {}", encoders.len());
        Ok(encoders)
    }
}

unsafe fn enumerate_transforms(category: &GUID, flags: MFT_ENUM_FLAG) -> Result<Vec<(IMFTransform, GUID)>> {
    let mut transforms = Vec::new();
    let mut p_count: u32 = 0;
    let mut p_array: *mut Option<IMFActivate> = std::ptr::null_mut();
    
    let hr = MFTEnumEx(*category, flags, None, None, &mut p_array, &mut p_count);
    if let Err(e) = hr {
        error!("MFTEnumEx failed: {:?}", e);
        return Err(e);
    }

    let result = unsafe {
        if p_array.is_null() || p_count == 0 {
            return Ok(Vec::new());
        }

        if (p_array as usize) % std::mem::align_of::<Option<IMFActivate>>() != 0 {
            error!("Misaligned pointer received from MFTEnumEx");
            return Err(Error::from(E_POINTER));
        }

        for i in 0..p_count {
            if i >= p_count { break; }
            
            let activate_ptr = p_array.add(i as usize);
            if activate_ptr.is_null() {
                continue;
            }

            let activate_opt = &*activate_ptr;
            
            if let Some(activate) = activate_opt {
                // Get the CLSID first
                match activate.GetGUID(&MFT_TRANSFORM_CLSID_Attribute) {
                    Ok(clsid) => {
                        // Try to activate and cast in one block
                        if let Ok(unknown) = activate.ActivateObject::<IUnknown>() {
                            if let Ok(transform) = unknown.cast::<IMFTransform>() {
                                transforms.push((transform, clsid));
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to get CLSID for transform {}: {:?}", i, e);
                        continue;
                    }
                }
            }
        }

        Ok(transforms)
    };

    if !p_array.is_null() {
        unsafe {
            CoTaskMemFree(Some(p_array as *const c_void));
        }
    }

    result
}

unsafe fn get_encoder_info(transform: &IMFTransform, clsid: GUID, media_type: MediaType) -> Result<EncoderInfo> {
    info!("Beginning get_encoder_info for transform");
    
    // Try to get attributes, but don't fail if not implemented
    let friendly_name = match transform.GetAttributes() {
        Ok(attr) => {
            info!("Successfully got attributes");
            // Try to get the friendly name
            let mut buffer = vec![0u16; 256];
            let mut length: u32 = 0;
            
            match attr.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buffer, Some(&mut length)) {
                Ok(_) => {
                    if length > 0 {
                        String::from_utf16_lossy(&buffer[..length as usize])
                    } else {
                        format!("{:?} Encoder", media_type)
                    }
                },
                Err(_) => format!("{:?} Encoder", media_type)
            }
        },
        Err(e) => {
            // If attributes aren't implemented, generate a default name
            if e.code() == windows::core::HRESULT(0x80004001u32 as i32) { // E_NOTIMPL
                info!("GetAttributes not implemented, using default name");
                format!("{:?} Encoder", media_type)
            } else {
                error!("Failed to get attributes: {:?}", e);
                return Err(e);
            }
        }
    };

    Ok(EncoderInfo {
        name: friendly_name,
        guid: clsid,  // Use the CLSID we stored during enumeration
        media_type,
    })
}