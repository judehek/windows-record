mod config;
mod inner;

// Re-export public types from config
pub use self::config::{RecorderConfig, RecorderConfigBuilder, AudioSource};

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
    // Create a new recorder instance with configuration
    pub fn new(config: RecorderConfig) -> Result<Self> {
        Ok(Self {
            rec_inner: RefCell::new(None),
            config,
            process_name: RefCell::new(None),
        })
    }

    // Get a configuration builder to create a new configuration
    pub fn builder() -> RecorderConfigBuilder {
        RecorderConfig::builder()
    }

    // Set the process name to record
    pub fn with_process_name(self, proc_name: &str) -> Self {
        *self.process_name.borrow_mut() = Some(proc_name.to_string());
        self
    }

    // Begin recording
    pub fn start_recording(&self) -> Result<()> {
        if self.config.debug_mode() {
            info!("Starting recording to file: {}", self.config.output_path().display());
        }
    
        let process_name = self.process_name.borrow();
        let mut rec_inner = self.rec_inner.borrow_mut();
    
        let Some(ref proc_name) = *process_name else {
            return Err(RecorderError::NoProcessSpecified);
        };
    
        *rec_inner = Some(
            RecorderInner::init(&self.config, proc_name)
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
    
    /// Save the content of the replay buffer to a file
    pub fn save_replay(&self, output_path: &str) -> Result<()> {
        if !self.config.enable_replay_buffer() {
            return Err(RecorderError::Generic("Replay buffer is not enabled".to_string()));
        }
        
        let rec_inner = self.rec_inner.borrow();
        
        let Some(ref inner) = *rec_inner else {
            return Err(RecorderError::NoRecorderBound);
        };
        
        inner.save_replay(output_path)
    }
}