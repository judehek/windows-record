use thiserror::Error;
use windows::core;
use windows::core::Error as WindowsError;
use windows::core::HRESULT;
use windows::core::HSTRING;

pub type Result<T> = std::result::Result<T, RecorderError>;

#[derive(Debug, Error)]
pub enum RecorderError {
    #[error("Windows API error: {0}")]
    Windows(#[from] core::Error),

    #[error("Generic Error: {0}")]
    Generic(String),

    #[error("Failed to Start the Recording Process, reason: {0}")]
    FailedToStart(String),

    #[error("Failed to Stop the Recording Process")]
    FailedToStop,

    #[error("Called to Stop when there is no Recorder Configured")]
    NoRecorderBound,

    #[error("Called to Stop when the Recorder is Already Stopped")]
    RecorderAlreadyStopped,

    #[error("No Process Specified for the Recorder")]
    NoProcessSpecified,
}

impl From<RecorderError> for WindowsError {
    fn from(err: RecorderError) -> Self {
        match err {
            // For Windows errors, we can pass through the original error
            RecorderError::Windows(e) => e,
            // For other errors, we create a new WindowsError with a custom HRESULT
            // Using 0x80004005 (E_FAIL) as a generic error code
            _ => WindowsError::new(
                HRESULT(-2147467259), // 0x80004005
                HSTRING::from(err.to_string()),
            ),
        }
    }
}
