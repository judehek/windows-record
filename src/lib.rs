// Private modules
mod capture;
mod error;
mod processing;
mod recorder;
mod types;

pub use error::{RecorderError, Result};
pub use recorder::{Recorder, RecorderConfig, RecorderConfigBuilder, AudioSource};