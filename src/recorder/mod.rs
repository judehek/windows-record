mod config;
mod inner;

use self::config::{RecorderConfig, RecorderConfigBuilder};
use self::inner::RecorderInner;
use crate::error::{RecorderError, Result};
use log::info;
use std::cell::RefCell;

pub struct Recorder {
    rec_inner: RefCell<Option<RecorderInner>>,
    config: RecorderConfig,
    process_name: RefCell<Option<String>>,
}

impl Recorder {
    /// Create a new recorder instance with configuration
    pub fn new(config: RecorderConfig) -> Result<Self> {
        Ok(Self {
            rec_inner: RefCell::new(None),
            config,
            process_name: RefCell::new(None),
        })
    }

    /// Get a configuration builder to create a new configuration
    pub fn builder() -> RecorderConfigBuilder {
        RecorderConfig::builder()
    }

    /// Set the process name to record
    pub fn with_process_name(mut self, proc_name: &str) -> Self {
        *self.process_name.borrow_mut() = Some(proc_name.to_string());
        self
    }

    /// Start recording to the specified file
    pub fn start_recording(&self, filename: &str) -> Result<()> {
        if self.config.debug_mode() {
            info!("Starting recording to file: {}", filename);
        }

        let process_name = self.process_name.borrow();
        let mut rec_inner = self.rec_inner.borrow_mut();

        let Some(ref proc_name) = *process_name else {
            return Err(RecorderError::NoProcessSpecified);
        };

        *rec_inner = Some(
            RecorderInner::init(filename, &self.config, proc_name)
                .map_err(|e| RecorderError::FailedToStart(e.to_string()))?,
        );

        Ok(())
    }

    /// Stop the current recording
    pub fn stop_recording(&self) -> Result<()> {
        if self.config.debug_mode() {
            info!("Stopping recording");
        }

        let rec_inner = self.rec_inner.borrow();

        let Some(ref inner) = *rec_inner else {
            return Err(RecorderError::NoRecorderBound);
        };

        inner.stop()
    }

    /// Get the current configuration
    pub fn config(&self) -> &RecorderConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recorder_creation() {
        let config = Recorder::builder()
            .fps(60, 1)
            .dimensions(1920, 1080)
            .capture_audio(true)
            .build();

        let recorder = Recorder::new(config).unwrap();
        assert!(recorder.config().capture_audio());
        assert!(!recorder.config().capture_microphone());
    }

    #[test]
    fn test_process_name_setting() {
        let config = Recorder::builder()
            .fps(60, 1)
            .build();

        let recorder = Recorder::new(config)
            .unwrap()
            .with_process_name("test_process");

        assert!(recorder.start_recording("test.mp4").is_ok());
    }

    #[test]
    fn test_recording_without_process_name() {
        let config = Recorder::builder()
            .fps(60, 1)
            .build();

        let recorder = Recorder::new(config).unwrap();
        assert!(matches!(
            recorder.start_recording("test.mp4"),
            Err(RecorderError::NoProcessSpecified)
        ));
    }
}