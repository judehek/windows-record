mod audio;
mod dxgi;
mod video;
mod window;
mod microphone;

pub use audio::collect_audio;
pub use microphone::collect_microphone;
pub use video::collect_frames;
pub use window::{find_window_by_substring, is_window_valid, get_window_title};
