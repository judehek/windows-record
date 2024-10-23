use windows::core::Result;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;

pub unsafe fn process_audio_sample(
    sample: &IMFSample,
    stream_index: u32,
    writer: &IMFSinkWriter,
) -> Result<()> {
    writer.WriteSample(stream_index, sample)?;
    Ok(())
}

pub fn calculate_audio_buffer_size(num_frames: u32, channels: u16, bits_per_sample: u16) -> usize {
    (num_frames as usize * channels as usize * bits_per_sample as usize) / 8
}
