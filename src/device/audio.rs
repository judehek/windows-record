use log::info;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::CO_E_ALREADYINITIALIZED;
use std::ffi::OsString;
use std::os::windows::prelude::OsStringExt;
use windows::core::Result;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

/// Structure holding information about an audio input device
#[derive(Debug, Clone)]
pub struct AudioInputDevice {
    pub id: String,
    pub name: String,
}

/// Enumerate all available audio input devices
pub fn enumerate_audio_input_devices() -> Result<Vec<AudioInputDevice>> {
    unsafe {
        // Initialize COM if not already initialized
        let coinit_result = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Err(e) = coinit_result {
            if e.code() != CO_E_ALREADYINITIALIZED {
                return Err(e);
            }
            info!("COM already initialized");
        }

        // Create device enumerator
        let enumerator: IMMDeviceEnumerator = 
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

        // Enumerate all audio capture devices
        let collection = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let count = collection.GetCount()?;
        
        let mut devices = Vec::new();
        
        for i in 0..count {
            if let Ok(device) = collection.Item(i) {
                // Get device ID
                if let Ok(id_ptr) = device.GetId() {
                    let id_wide_slice = id_ptr.as_wide();
                    let device_id = OsString::from_wide(id_wide_slice).to_string_lossy().to_string();
                    
                    // Get device name from property store
                    if let Ok(props) = device.OpenPropertyStore(STGM_READ) {
                        let prop_value = props.GetValue(&PKEY_Device_FriendlyName)?;
                        let name_wide_slice = prop_value.Anonymous.Anonymous.Anonymous.pwszVal.as_wide();
                        let device_name = OsString::from_wide(name_wide_slice).to_string_lossy().to_string();
                        
                        devices.push(AudioInputDevice {
                            id: device_id,
                            name: device_name,
                        });
                    }
                }
            }
        }
        
        Ok(devices)
    }
}

/// Get the currently selected audio input device ID from the config, or default device if not specified
pub fn get_audio_input_device_by_name(device_name: Option<&str>) -> Result<String> {
    // If no device is specified, return the default device ID
    if device_name.is_none() {
        return get_default_audio_input_device_id();
    }
    
    // If a device name is specified, find the device with that name
    let devices = enumerate_audio_input_devices()?;
    let name = device_name.unwrap();
    
    for device in devices {
        if device.name == name {
            return Ok(device.id);
        }
    }
    
    // If the specified device name is not found, fall back to default
    info!("Specified audio input device '{}' not found, using default", name);
    get_default_audio_input_device_id()
}

/// Get the default audio input device ID
pub fn get_default_audio_input_device_id() -> Result<String> {
    unsafe {
        // Initialize COM if not already initialized
        let coinit_result = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Err(e) = coinit_result {
            if e.code() != CO_E_ALREADYINITIALIZED {
                return Err(e);
            }
        }

        // Create device enumerator
        let enumerator: IMMDeviceEnumerator = 
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            
        // Get default device
        let device = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?;
        
        // Get device ID
        let id_ptr = device.GetId()?;
        let id_wide_slice = id_ptr.as_wide();
        let device_id = OsString::from_wide(id_wide_slice).to_string_lossy().to_string();
        
        Ok(device_id)
    }
}