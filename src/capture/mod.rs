mod audio;
mod dxgi;
mod video;
pub mod window;
mod microphone;

pub use audio::collect_audio;
pub use microphone::collect_microphone;
pub use video::get_frames;
pub use window::{get_window_by_string, get_window_by_exact_string};
