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

    #[error("Generic error: {0}")]
    Generic(String),

    #[error("Failed to start the recording process, reason: {0}")]
    FailedToStart(String),

    #[error("Failed to stop the recording process")]
    FailedToStop,

    #[error("Called to stop when there is no recorder configured")]
    NoRecorderBound,

    #[error("Called to stop when the recorder is already stopped")]
    RecorderAlreadyStopped,

    #[error("No process specified for the recorder")]
    NoProcessSpecified,

    #[error("Logger error: {0}")]
    LoggerError(String),
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
