use crate::Result;
use windows::core::GUID;

/// Represents a video encoder option
#[derive(Debug, Clone)]
pub struct VideoEncoder {
    /// Unique identifier for the encoder
    pub id: GUID,
    /// Human-readable name for the encoder
    pub name: String,
}

/// Available video encoders that the recorder supports
#[derive(Debug, Clone, PartialEq)]
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

/// Returns a list of available video encoders
pub fn enumerate_video_encoders() -> Result<Vec<VideoEncoder>> {
    use windows::Win32::Media::MediaFoundation::{
        MFVideoFormat_H264, MFVideoFormat_HEVC,
    };

    // These are the video encoders we currently support
    // In a real implementation, this might query the system for available encoders
    let encoders = vec![
        VideoEncoder {
            id: MFVideoFormat_H264,
            name: "H.264 (AVC)".to_string(),
        },
        VideoEncoder {
            id: MFVideoFormat_HEVC,
            name: "H.265 (HEVC)".to_string(),
        },
    ];

    Ok(encoders)
}

/// Gets a video encoder by its name
pub fn get_video_encoder_by_name(name: &str) -> Option<VideoEncoder> {
    match enumerate_video_encoders() {
        Ok(encoders) => encoders.into_iter().find(|encoder| encoder.name == name),
        Err(_) => None,
    }
}

/// Gets a video encoder by its type
pub fn get_video_encoder_by_type(encoder_type: &VideoEncoderType) -> Result<VideoEncoder> {
    use windows::Win32::Media::MediaFoundation::{
        MFVideoFormat_H264, MFVideoFormat_HEVC,
    };

    let encoder = match encoder_type {
        VideoEncoderType::H264 => VideoEncoder {
            id: MFVideoFormat_H264,
            name: "H.264 (AVC)".to_string(),
        },
        VideoEncoderType::HEVC => VideoEncoder {
            id: MFVideoFormat_HEVC,
            name: "H.265 (HEVC)".to_string(),
        },
    };

    Ok(encoder)
}