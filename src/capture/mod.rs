mod audio;
mod dxgi;
mod video;
mod window;
mod microphone;

pub use audio::collect_audio;
pub use microphone::collect_microphone;
pub use video::get_frames;
pub use window::{WindowSearchOptions, get_window_by_text, list_all_visible_windows};
