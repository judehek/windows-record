// Private modules
mod capture;
mod device;
mod error;
mod processing;
mod recorder;
mod types;

pub use device::audio::{AudioInputDevice, enumerate_audio_input_devices};
pub use device::video::{VideoEncoder, VideoEncoderType, enumerate_video_encoders};
pub use error::{RecorderError, Result};
pub use recorder::{Recorder, RecorderConfig, RecorderConfigBuilder, AudioSource};