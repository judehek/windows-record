mod config;
mod inner;

// Re-export public types from config
pub use self::config::{RecorderConfig, RecorderConfigBuilder, AudioSource};

use self::inner::RecorderInner;
use crate::error::{RecorderError, Result};
use log::{info, debug};
use std::sync::RwLock;
use windows::Win32::Foundation::{BOOL, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, IsWindowVisible};

pub struct Recorder {
    rec_inner: RwLock<Option<RecorderInner>>,
    config: RecorderConfig,
    process_name: RwLock<Option<String>>,
    use_exact_match: RwLock<bool>,
}

impl Recorder {
    // Create a new recorder instance with configuration
    pub fn new(config: RecorderConfig) -> Result<Self> {
        Ok(Self {
            rec_inner: RwLock::new(None),
            config,
            process_name: RwLock::new(None),
            use_exact_match: RwLock::new(false),
        })
    }

    // Get a configuration builder to create a new configuration
    pub fn builder() -> RecorderConfigBuilder {
        RecorderConfig::builder()
    }

    // Set the process name to record
    pub fn with_process_name(self, proc_name: &str) -> Self {
        if let Ok(mut process_name) = self.process_name.write() {
            *process_name = Some(proc_name.to_string());
        }
        self
    }
    
    /// Set exact matching for window titles (case-sensitive)
    /// When true, the window title must match exactly
    /// When false (default), any window containing the substring will match
    pub fn with_exact_match(self, use_exact: bool) -> Self {
        if let Ok(mut exact_match) = self.use_exact_match.write() {
            *exact_match = use_exact;
        }
        self
    }

    // Begin recording
    pub fn start_recording(&self) -> Result<()> {
        if self.config.debug_mode() {
            info!("Starting recording to file: {}", self.config.output_path().display());
        }
    
        // Read the process_name and use_exact_match with read locks
        let process_name_guard = self.process_name.read().map_err(|_| 
            RecorderError::Generic("Failed to acquire read lock on process_name".to_string()))?;
        
        let use_exact_match = *self.use_exact_match.read().map_err(|_| 
            RecorderError::Generic("Failed to acquire read lock on use_exact_match".to_string()))?;
        
        // Get a write lock for rec_inner
        let mut rec_inner = self.rec_inner.write().map_err(|_| 
            RecorderError::Generic("Failed to acquire write lock on rec_inner".to_string()))?;
    
        let Some(ref proc_name) = *process_name_guard else {
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
    
        // We need to modify the inner::RecorderInner::init to take use_exact_match directly
        // For now we'll set it in TLS but we should update the function signature
        thread_local! {
            static USE_EXACT_MATCH: std::cell::RefCell<bool> = std::cell::RefCell::new(false);
        }
        
        USE_EXACT_MATCH.with(|cell| {
            *cell.borrow_mut() = use_exact_match;
        });
    
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

        let rec_inner = self.rec_inner.read().map_err(|_| 
            RecorderError::Generic("Failed to acquire read lock on rec_inner".to_string()))?;

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
        
        let rec_inner = self.rec_inner.read().map_err(|_| 
            RecorderError::Generic("Failed to acquire read lock on rec_inner".to_string()))?;
        
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