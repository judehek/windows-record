use log::{error, info};
use windows::core::{ComInterface, Result, GUID};
use windows::Win32::Foundation::CO_E_ALREADYINITIALIZED;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use std::collections::HashMap;
use std::ffi::c_void;

/// Structure holding information about a video encoder
#[derive(Debug, Clone)]
pub struct VideoEncoder {
    pub name: String,
    pub guid: GUID,
    pub is_hardware: bool,
    pub vendor_id: Option<String>,
}

/// Define known video encoders
pub const KNOWN_ENCODERS: &[GUID] = &[
    // AMD Hardware Encoders
    GUID::from_values(0xADC9BC80, 0x0F41, 0x46C6, [0xAB, 0x75, 0xD6, 0x93, 0xD7, 0x93, 0x59, 0x7D]), // AMDh264
    GUID::from_values(0x5FD65104, 0xA924, 0x4835, [0xAB, 0x71, 0x09, 0xA2, 0x23, 0xE3, 0xE3, 0x7B]), // AMDh265
    
    // NVIDIA Hardware Encoders
    GUID::from_values(0x60F44560, 0x5A20, 0x4857, [0xBF, 0x41, 0x4E, 0xC5, 0xB4, 0x3F, 0x79, 0xED]), // NvencH264
    GUID::from_values(0x62F44560, 0x5A20, 0x4857, [0xBF, 0x41, 0x4E, 0xC5, 0xB4, 0x3F, 0x79, 0xED]), // NvencH265
    
    // Intel QuickSync Encoders
    GUID::from_values(0x4BE8D3C0, 0x0515, 0x4A37, [0xAD, 0x55, 0xE4, 0xBF, 0x61, 0x61, 0x2D, 0x19]), // IntelH264
    GUID::from_values(0x4BE8D3C1, 0x0515, 0x4A37, [0xAD, 0x55, 0xE4, 0xBF, 0x61, 0x61, 0x2D, 0x19]), // IntelH265
    
    // Microsoft Software H264 (CPU fallback)
    GUID::from_values(0x6CA50344, 0x051A, 0x4DED, [0x97, 0x79, 0xA4, 0x33, 0x05, 0x16, 0x5E, 0x35]), // H264 Encoder MFT
];

/// Initialize Media Foundation
fn ensure_mf_initialized() -> Result<()> {
    unsafe {
        // Initialize COM if not already initialized
        let coinit_result = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Err(e) = coinit_result {
            if e.code() != CO_E_ALREADYINITIALIZED {
                return Err(e);
            }
            info!("COM already initialized");
        }
        
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

/// Enumerate all available video encoders
pub fn enumerate_video_encoders() -> Result<HashMap<String, VideoEncoder>> {
    ensure_mf_initialized()?;
    
    unsafe {
        info!("Starting video encoder enumeration...");
        let mut encoders = HashMap::new();
        
        let enum_flags = MFT_ENUM_FLAG_HARDWARE
            | MFT_ENUM_FLAG_SYNCMFT
            | MFT_ENUM_FLAG_ASYNCMFT
            | MFT_ENUM_FLAG_SORTANDFILTER
            | MFT_ENUM_FLAG_ALL;

        let mut p_count: u32 = 0;
        let mut p_array: *mut Option<IMFActivate> = std::ptr::null_mut();
        
        match MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            enum_flags,
            None,
            None,
            &mut p_array,
            &mut p_count,
        ) {
            Ok(_) => {
                info!("Found {} video encoders", p_count);
                
                if !p_array.is_null() && p_count > 0 {
                    for i in 0..p_count {
                        let activate_ptr = p_array.add(i as usize);
                        if !activate_ptr.is_null() {
                            if let Some(activate) = &*activate_ptr {
                                if let Ok(attrs) = activate.cast::<IMFAttributes>() {
                                    if let Ok(clsid) = activate.GetGUID(&MFT_TRANSFORM_CLSID_Attribute) {
                                        // Only process if it's in our allowed list
                                        if KNOWN_ENCODERS.contains(&clsid) {
                                            let mut name_parts = Vec::new();
                                            let mut vendor_id = None;
                                            let mut is_hardware = false;
                                            
                                            // Get friendly name
                                            let mut buffer = vec![0u16; 256];
                                            let mut length: u32 = 0;
                                            if let Ok(_) = attrs.GetString(
                                                &MFT_FRIENDLY_NAME_Attribute,
                                                &mut buffer,
                                                Some(&mut length)
                                            ) {
                                                if length > 0 {
                                                    let friendly_name = String::from_utf16_lossy(&buffer[..length as usize]);
                                                    name_parts.push(friendly_name);
                                                }
                                            }
                                            
                                            // Get hardware vendor ID
                                            let mut buffer = vec![0u16; 256];
                                            let mut length: u32 = 0;
                                            if let Ok(_) = attrs.GetString(
                                                &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute,
                                                &mut buffer,
                                                Some(&mut length)
                                            ) {
                                                if length > 0 {
                                                    let vendor = String::from_utf16_lossy(&buffer[..length as usize]);
                                                    vendor_id = Some(vendor.clone());
                                                    name_parts.push(format!("Vendor: {}", vendor));
                                                }
                                            }
                                            
                                            // Check if hardware encoder
                                            if let Ok(flags) = attrs.GetUINT32(&MF_TRANSFORM_FLAGS_Attribute) {
                                                if flags & MFT_ENUM_FLAG_HARDWARE.0 as u32 != 0 {
                                                    is_hardware = true;
                                                    name_parts.push("Hardware".to_string());
                                                }
                                            }
                                            
                                            let name = if name_parts.is_empty() {
                                                format!("Video Encoder (GUID: {:?})", clsid)
                                            } else {
                                                name_parts.join(" | ")
                                            };
                                            
                                            encoders.insert(name.clone(), VideoEncoder {
                                                name,
                                                guid: clsid,
                                                is_hardware,
                                                vendor_id,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                
                if !p_array.is_null() {
                    CoTaskMemFree(Some(p_array as *const c_void));
                }
            }
            Err(e) => {
                error!("Failed to enumerate video encoders: {:?}", e);
                return Err(e);
            }
        }

        if encoders.is_empty() {
            error!("No suitable encoders found");
        } else {
            info!("Found {} suitable encoders", encoders.len());
            for (name, info) in &encoders {
                info!("Available encoder: {} (Hardware: {})", name, info.is_hardware);
            }
        }

        Ok(encoders)
    }
}

/// Get a video encoder by name, falling back to default if not found
pub fn get_video_encoder_by_name(encoder_name: Option<&str>) -> Result<GUID> {
    // If no encoder is specified, return the default encoder GUID
    if encoder_name.is_none() {
        return get_default_video_encoder();
    }
    
    // If an encoder name is specified, find the encoder with that name
    let encoders = enumerate_video_encoders()?;
    let name = encoder_name.unwrap();
    
    for (key, encoder) in encoders {
        if key.to_lowercase().contains(&name.to_lowercase()) {
            info!("Selected encoder: {}", key);
            return Ok(encoder.guid);
        }
    }
    
    // If the specified encoder name is not found, fall back to default
    info!("Specified encoder '{}' not found, using default", name);
    get_default_video_encoder()
}

/// Get the default video encoder (preferring hardware H264)
pub fn get_default_video_encoder() -> Result<GUID> {
    let encoders = enumerate_video_encoders()?;
    
    // Priority order:
    // 1. Hardware H264 encoder
    // 2. Any hardware encoder
    // 3. Software H264 encoder
    // 4. First available encoder
    
    // Try to find a hardware H264 encoder
    for (_, encoder) in &encoders {
        if encoder.is_hardware && encoder.name.to_lowercase().contains("h264") {
            info!("Selected default hardware H264 encoder: {}", encoder.name);
            return Ok(encoder.guid);
        }
    }
    
    // Try to find any hardware encoder
    for (_, encoder) in &encoders {
        if encoder.is_hardware {
            info!("Selected default hardware encoder: {}", encoder.name);
            return Ok(encoder.guid);
        }
    }
    
    // Try to find a software H264 encoder
    for (_, encoder) in &encoders {
        if !encoder.is_hardware && encoder.name.to_lowercase().contains("h264") {
            info!("Selected default software H264 encoder: {}", encoder.name);
            return Ok(encoder.guid);
        }
    }
    
    // Just take the first available
    if let Some((_, encoder)) = encoders.iter().next() {
        info!("Selected first available encoder: {}", encoder.name);
        return Ok(encoder.guid);
    }
    
    // If we got here, we can't find any encoder - return Microsoft's software encoder as a last resort
    error!("No encoders found, falling back to Microsoft Software H264 encoder");
    Ok(GUID::from_values(0x6CA50344, 0x051A, 0x4DED, [0x97, 0x79, 0xA4, 0x33, 0x05, 0x16, 0x5E, 0x35]))
}