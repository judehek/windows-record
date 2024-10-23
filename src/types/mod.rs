use std::sync::Arc;
use windows::Win32::Media::MediaFoundation::{IMFSample, IMFSinkWriter};
pub mod safe_wrapper;

pub struct SendableSample(pub Arc<IMFSample>);
unsafe impl Send for SendableSample {}
unsafe impl Sync for SendableSample {}

#[derive(Clone)]
pub struct SendableWriter(pub Arc<IMFSinkWriter>);
unsafe impl Send for SendableWriter {}
unsafe impl Sync for SendableWriter {}

use std::time::Duration;

pub fn duration_to_hns(duration: Duration) -> i64 {
    // Convert Duration to 100-nanosecond intervals (hns)
    duration.as_nanos() as i64 / 100
}

pub fn hns_to_duration(hns: i64) -> Duration {
    Duration::from_nanos((hns * 100) as u64)
}

#[derive(Debug, Clone, Copy)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
}

impl VideoConfig {
    pub fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Self {
        Self {
            width,
            height,
            fps_num,
            fps_den,
        }
    }

    pub fn frame_duration(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 * self.fps_den as u64 / self.fps_num as u64)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AudioConfig {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
}

impl AudioConfig {
    pub fn new(channels: u16, sample_rate: u32, bits_per_sample: u16) -> Self {
        Self {
            channels,
            sample_rate,
            bits_per_sample,
        }
    }

    pub fn bytes_per_sample(&self) -> u32 {
        (self.channels as u32 * self.bits_per_sample as u32) / 8
    }

    pub fn bytes_per_second(&self) -> u32 {
        self.sample_rate * self.bytes_per_sample()
    }
}
