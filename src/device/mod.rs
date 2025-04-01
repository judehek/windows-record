pub mod audio;
pub mod video;

pub use audio::*;
pub use video::*;

use windows::Win32::{Foundation::CO_E_ALREADYINITIALIZED, System::Com::{CoInitializeEx, COINIT_MULTITHREADED}};
use log::info;

fn ensure_com_initialized() -> windows::core::Result<()> {
    let coinit_result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if let Err(e) = coinit_result {
        if e.code() != CO_E_ALREADYINITIALIZED {
            return Err(e);
        }
        info!("COM already initialized");
    }
    Ok(())
}