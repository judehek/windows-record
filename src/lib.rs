pub mod capture;
pub mod error;
pub mod processing;
pub mod recorder;
pub mod types;

pub use error::{RecorderError, Result};
pub use recorder::Recorder;
