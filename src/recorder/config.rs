use std::path::PathBuf;

use windows::core::GUID;

#[derive(Clone)]
pub struct RecorderConfig {
    // Video settings
    fps_num: u32,
    fps_den: u32,
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    video_bitrate: u32,
    encoder: Option<GUID>,
    
    // Audio settings
    capture_audio: bool,
    capture_microphone: bool,
    microphone_volume: Option<f32>,
    system_volume: Option<f32>,
    audio_source: AudioSource,
    
    // Output settings
    output_path: PathBuf,
    debug_mode: bool,
}

#[derive(Clone, Default)]
pub enum AudioSource {
    #[default]
    Desktop,
    ActiveWindow,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            fps_num: 60,
            fps_den: 1,
            input_width: 1920,
            input_height: 1080,
            output_width: 1920,
            output_height: 1080,
            capture_audio: true,
            capture_microphone: false,
            output_path: PathBuf::from("."),
            debug_mode: false,
            video_bitrate: 5000000,
            microphone_volume: None,
            encoder: None,
            audio_source: AudioSource::ActiveWindow,
            system_volume: None,
        }
    }
}

impl RecorderConfig {
    pub fn builder() -> RecorderConfigBuilder {
        RecorderConfigBuilder::default()
    }

    // Updated getter methods
    pub fn fps_num(&self) -> u32 { self.fps_num }
    pub fn fps_den(&self) -> u32 { self.fps_den }
    pub fn input_width(&self) -> u32 { self.input_width }
    pub fn input_height(&self) -> u32 { self.input_height }
    pub fn output_width(&self) -> u32 { self.output_width }
    pub fn output_height(&self) -> u32 { self.output_height }
    pub fn capture_audio(&self) -> bool { self.capture_audio }
    pub fn capture_microphone(&self) -> bool { self.capture_microphone }
    pub fn output_path(&self) -> &PathBuf { &self.output_path }
    pub fn debug_mode(&self) -> bool { self.debug_mode }
    pub fn video_bitrate(&self) -> u32 { self.video_bitrate }
    pub fn microphone_volume(&self) -> Option<f32> { self.microphone_volume }
    pub fn encoder(&self) -> Option<GUID> { self.encoder }
    pub fn audio_source(&self) -> &AudioSource { &self.audio_source }
    pub fn system_volume(&self) -> Option<f32> { self.system_volume }
}

#[derive(Default)]
pub struct RecorderConfigBuilder {
    config: RecorderConfig,
}

impl RecorderConfigBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fps(mut self, num: u32, den: u32) -> Self {
        self.config.fps_num = num;
        self.config.fps_den = den;
        self
    }

    // Updated dimension methods
    pub fn input_dimensions(mut self, width: u32, height: u32) -> Self {
        self.config.input_width = width;
        self.config.input_height = height;
        self
    }

    pub fn output_dimensions(mut self, width: u32, height: u32) -> Self {
        self.config.output_width = width;
        self.config.output_height = height;
        self
    }

    pub fn capture_audio(mut self, enabled: bool) -> Self {
        self.config.capture_audio = enabled;
        self
    }

    pub fn capture_microphone(mut self, enabled: bool) -> Self {
        self.config.capture_microphone = enabled;
        self
    }

    pub fn output_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.config.output_path = path.into();
        self
    }

    pub fn debug_mode(mut self, enabled: bool) -> Self {
        self.config.debug_mode = enabled;
        self
    }

    pub fn video_bitrate(mut self, video_bitrate: u32) -> Self {
        self.config.video_bitrate = video_bitrate;
        self
    }
    
    pub fn microphone_volume(mut self, volume: impl Into<Option<f32>>) -> Self {
        self.config.microphone_volume = volume.into();
        self
    }
    
    pub fn encoder(mut self, encoder: impl Into<Option<GUID>>) -> Self {
        self.config.encoder = encoder.into();
        self
    }
    
    pub fn audio_source(mut self, source: AudioSource) -> Self {
        self.config.audio_source = source;
        self
    }
    
    pub fn system_volume(mut self, volume: impl Into<Option<f32>>) -> Self {
        self.config.system_volume = volume.into();
        self
    }

    pub fn build(self) -> RecorderConfig {
        self.config
    }
}