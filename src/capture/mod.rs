mod audio;
mod dxgi;
mod video;
pub mod window;
mod microphone;
mod monitor;

pub use audio::collect_audio;
pub use microphone::collect_microphone;
pub use video::get_frames;
pub use window::{get_window_by_string, get_window_by_exact_string};
pub use monitor::{get_primary_monitor_resolution, get_window_monitor_resolution};
