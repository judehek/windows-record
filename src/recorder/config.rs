use crate::logger::LoggerConfig;

#[derive(Clone)]
pub struct RecorderConfig {
    fps_num: u32,
    fps_den: u32,
    screen_width: u32,
    screen_height: u32,
    capture_audio: bool,
    capture_microphone: bool,
    log_config: Option<LoggerConfig>,
}

impl RecorderConfig {
    pub fn new(fps_num: u32, fps_den: u32, screen_width: u32, screen_height: u32) -> Self {
        Self {
            fps_num,
            fps_den,
            screen_width,
            screen_height,
            capture_audio: true,
            capture_microphone: false,
            log_config: Some(LoggerConfig::default()),
        }
    }

    pub fn update(
        &mut self,
        fps_den: Option<u32>,
        fps_num: Option<u32>,
        screen_width: Option<u32>,
        screen_height: Option<u32>,
    ) {
        if let Some(den) = fps_den {
            self.fps_den = den;
        }
        if let Some(num) = fps_num {
            self.fps_num = num;
        }
        if let Some(width) = screen_width {
            self.screen_width = width;
        }
        if let Some(height) = screen_height {
            self.screen_height = height;
        }
    }

    pub fn fps_num(&self) -> u32 {
        self.fps_num
    }
    pub fn fps_den(&self) -> u32 {
        self.fps_den
    }
    pub fn screen_width(&self) -> u32 {
        self.screen_width
    }
    pub fn screen_height(&self) -> u32 {
        self.screen_height
    }
    pub fn capture_audio(&self) -> bool {
        self.capture_audio
    }
    pub fn capture_microphone(&self) -> bool {
        self.capture_microphone
    }

    // Add this getter method for log_config
    pub fn log_config(&self) -> Option<LoggerConfig> {
        self.log_config.clone()
    }

    pub fn set_capture_audio(&mut self, capture_audio: bool) {
        self.capture_audio = capture_audio;
    }

    pub fn set_capture_microphone(&mut self, capture_microphone: bool) {
        self.capture_microphone = capture_microphone;
    }

    pub fn set_log_config(&mut self, config: LoggerConfig) {
        self.log_config = Some(config);
    }

    pub fn disable_logging(&mut self) {
        self.log_config = None;
    }
}
