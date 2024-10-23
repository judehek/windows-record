mod audio;
mod dxgi;
mod video;
mod window;

pub use audio::collect_audio;
pub use video::collect_frames;
pub use window::find_window_by_substring;
