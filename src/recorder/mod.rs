mod config;
mod inner;

// Re-export public types from config
pub use self::config::{RecorderConfig, RecorderConfigBuilder, AudioSource};

use self::inner::RecorderInner;
use crate::error::{RecorderError, Result};
use log::{info, debug};
use std::cell::RefCell;
use windows::Win32::Foundation::{BOOL, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, IsWindowVisible};

pub struct Recorder {
    rec_inner: RefCell<Option<RecorderInner>>,
    config: RecorderConfig,
    process_name: RefCell<Option<String>>,
    use_exact_match: RefCell<bool>,
}

impl Recorder {
    // Create a new recorder instance with configuration
    pub fn new(config: RecorderConfig) -> Result<Self> {
        Ok(Self {
            rec_inner: RefCell::new(None),
            config,
            process_name: RefCell::new(None),
            use_exact_match: RefCell::new(false),
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
    
    /// Set exact matching for window titles (case-sensitive)
    /// When true, the window title must match exactly
    /// When false (default), any window containing the substring will match
    pub fn with_exact_match(self, use_exact: bool) -> Self {
        *self.use_exact_match.borrow_mut() = use_exact;
        self
    }

    // Begin recording
    pub fn start_recording(&self) -> Result<()> {
        if self.config.debug_mode() {
            info!("Starting recording to file: {}", self.config.output_path().display());
        }
    
        let process_name = self.process_name.borrow();
        let use_exact_match = *self.use_exact_match.borrow();
        let mut rec_inner = self.rec_inner.borrow_mut();
    
        let Some(ref proc_name) = *process_name else {
            return Err(RecorderError::NoProcessSpecified);
        };
        
        // If debug mode is enabled, print all window titles for debugging
        if self.config.debug_mode() {
            info!("Searching for windows with{} match: '{}'",
                 if use_exact_match { " exact" } else { " substring" }, 
                 proc_name);
            
            unsafe {
                info!("Available windows:");
                windows::Win32::UI::WindowsAndMessaging::EnumWindows(
                    Some(debug_window_enum_callback),
                    LPARAM(0), // We don't need to pass the search string
                );
            }
        }
    
        // Use the new init_with_exact_match function to pass the flag directly
        *rec_inner = Some(
            RecorderInner::init_with_exact_match(&self.config, proc_name, use_exact_match)
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

/// Debug callback to list all window titles
unsafe extern "system" fn debug_window_enum_callback(hwnd: windows::Win32::Foundation::HWND, lparam: LPARAM) -> BOOL {
    // Skip windows that aren't visible
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1); // Continue enumeration
    }
    
    let mut text: [u16; 512] = [0; 512];
    let length = GetWindowTextW(hwnd, &mut text);
    let window_text = String::from_utf16_lossy(&text[..length as usize]);
    
    // Skip empty titles
    if !window_text.is_empty() {
        debug!("Window title: '{}'", window_text);
    }
    
    BOOL(1) // Continue enumeration
}