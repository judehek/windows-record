mod config;
mod inner;

use self::config::RecorderConfig;
use self::inner::RecorderInner;
use crate::error::{RecorderError, Result};
use crate::logger::{setup_logger, LoggerConfig}; // Add this import
use log::info;
use std::cell::RefCell;
use std::io;
use std::path::Path; // Add this import // Add this import

pub struct Recorder {
    rec_inner: RefCell<Option<RecorderInner>>,
    config: RefCell<RecorderConfig>,
    process_name: RefCell<Option<String>>,
}

impl Recorder {
    pub fn new(fps_num: u32, fps_den: u32, screen_width: u32, screen_height: u32) -> Result<Self> {
        let recorder = Self {
            rec_inner: RefCell::new(None),
            config: RefCell::new(RecorderConfig::new(
                fps_num,
                fps_den,
                screen_width,
                screen_height,
            )),
            process_name: RefCell::new(None),
        };

        Ok(recorder)
    }

    pub fn set_configs(
        &self,
        fps_den: Option<u32>,
        fps_num: Option<u32>,
        screen_width: Option<u32>,
        screen_height: Option<u32>,
    ) {
        let mut config = self.config.borrow_mut();
        config.update(fps_den, fps_num, screen_width, screen_height);
    }

    pub fn set_process_name(&self, proc_name: &str) {
        *self.process_name.borrow_mut() = Some(proc_name.to_string());
    }

    pub fn start_recording(&self, filename: &str) -> Result<()> {
        info!("Starting recording to file: {}", filename);
        let config = self.config.borrow();
        let process_name = self.process_name.borrow();
        let mut rec_inner = self.rec_inner.borrow_mut();

        let Some(ref proc_name) = *process_name else {
            return Err(RecorderError::NoProcessSpecified);
        };

        *rec_inner = Some(
            RecorderInner::init(filename, &config, proc_name)
                .map_err(|e| RecorderError::FailedToStart(e.to_string()))?,
        );

        Ok(())
    }

    pub fn stop_recording(&self) -> Result<()> {
        info!("Stopping recording");
        let rec_inner = self.rec_inner.borrow();

        let Some(ref inner) = *rec_inner else {
            return Err(RecorderError::NoRecorderBound);
        };

        inner.stop()
    }

    pub fn set_capture_audio(&self, capture_audio: bool) {
        self.config.borrow_mut().set_capture_audio(capture_audio);
    }

    pub fn set_capture_microphone(&self, capture_microphone: bool) {
        self.config
            .borrow_mut()
            .set_capture_microphone(capture_microphone);
    }

    pub fn is_audio_capture_enabled(&self) -> bool {
        self.config.borrow().capture_audio()
    }

    pub fn set_log_directory<P: AsRef<Path>>(&self, dir: P) -> Result<()> {
        let mut config = self.config.borrow_mut();
        let log_config = LoggerConfig::default().with_log_dir(dir);

        match setup_logger(log_config.clone()) {
            Ok(_) => {
                config.set_log_config(log_config);
                Ok(())
            }
            Err(e) => {
                // Ignore "already initialized" errors
                if e.to_string().contains("already initialized") {
                    Ok(())
                } else {
                    Err(RecorderError::LoggerError(e.to_string()))
                }
            }
        }
    }

    pub fn disable_logging(&self) -> Result<()> {
        let mut config = self.config.borrow_mut();
        config.disable_logging();

        match setup_logger(LoggerConfig::default().disable_logging()) {
            Ok(_) => Ok(()),
            Err(e) => {
                // Ignore "already initialized" errors
                if e.to_string().contains("already initialized") {
                    Ok(())
                } else {
                    Err(RecorderError::LoggerError(e.to_string()))
                }
            }
        }
    }
}
