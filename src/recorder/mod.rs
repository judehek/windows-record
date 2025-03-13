mod config;
mod inner;

// Re-export public types from config
pub use self::config::{RecorderConfig, RecorderConfigBuilder, AudioSource};

use self::inner::RecorderInner;
use crate::{capture::{list_all_visible_windows, WindowSearchOptions}, error::{RecorderError, Result}};
use log::info;
use std::cell::RefCell;

pub struct Recorder {
    rec_inner: RefCell<Option<RecorderInner>>,
    config: RecorderConfig,
    process_name: RefCell<Option<String>>,
    window_search_options: RefCell<WindowSearchOptions>,
}

impl Recorder {
    // Create a new recorder instance with configuration
    pub fn new(config: RecorderConfig) -> Result<Self> {
        Ok(Self {
            rec_inner: RefCell::new(None),
            config,
            process_name: RefCell::new(None),
            window_search_options: RefCell::new(WindowSearchOptions::default()),
        })
    }

    // Get a configuration builder to create a new configuration
    pub fn builder() -> RecorderConfigBuilder {
        RecorderConfig::builder()
    }

    // Keep the original method for backward compatibility
    pub fn with_process_name(self, proc_name: &str) -> Self {
        *self.process_name.borrow_mut() = Some(proc_name.to_string());
        self
    }

    // Customize window search options
    pub fn with_window_search_options(self, options: WindowSearchOptions) -> Self {
        *self.window_search_options.borrow_mut() = options;
        self
    }

    pub fn case_sensitive(self, value: bool) -> Self {
        self.window_search_options.borrow_mut().case_sensitive = value;
        self
    }

    pub fn exact_match(self, value: bool) -> Self {
        self.window_search_options.borrow_mut().exact_match = value;
        self
    }

    // Helper method to list all visible windows for debugging
    pub fn list_available_windows() -> Vec<String> {
        list_all_visible_windows().into_iter().collect()
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