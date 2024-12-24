use windows::core::{Result, GUID};
use crate::processing::encoder::get_available_encoders;

pub unsafe fn get_encoder_guid(requested_encoder: Option<&str>) -> Result<Option<GUID>> {
    match requested_encoder {
        Some(encoder_name) => {
            get_available_encoders().map(|encoders| {
                if let Some(encoder_info) = encoders.get(encoder_name) {
                    Some(encoder_info.guid)
                } else {
                    log::warn!(
                        "Requested encoder '{}' not found, falling back to default H.264 encoder",
                        encoder_name
                    );
                    None
                }
            })
        }
        None => Ok(None),
    }
}