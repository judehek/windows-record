use std::ptr;

use windows::core::{Result, GUID};
use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVE_FORMAT_PCM};
use windows::Win32::Media::MediaFoundation::*;

pub unsafe fn create_sink_writer(
    output_path: &str,
    fps_num: u32,
    fps_den: u32,
    output_width: u32,
    output_height: u32,
    capture_audio: bool,
    capture_microphone: bool,
    video_bitrate: u32,
    video_encoder_id: &GUID,
) -> Result<IMFSinkWriter> {
    // Create and configure attributes
    let attributes = create_sink_attributes()?;

    // Create sink writer
    let sink_writer: IMFSinkWriter = MFCreateSinkWriterFromURL(
        &windows::core::HSTRING::from(output_path),
        None,
        attributes.as_ref(),
    )?;

    let mut current_stream_index = 0;

    // Configure video stream (always stream index 0)
    configure_video_stream(&sink_writer, fps_num, fps_den, output_width, output_height, video_bitrate, video_encoder_id)?;
    current_stream_index += 1;

    // Configure a single audio stream if either audio source is enabled
    if capture_audio || capture_microphone {
        // Use a new function that configures a stream suitable for mixed audio
        configure_mixed_audio_stream(&sink_writer, current_stream_index)?;
        // current_stream_index += 1;
    }

    Ok(sink_writer)
}

unsafe fn configure_mixed_audio_stream(
    sink_writer: &IMFSinkWriter,
    stream_index: u32,
) -> Result<()> {
    // Create output type 
    let audio_output_type = create_audio_output_type()?;

    // Create input type suitable for mixed audio (stereo, 16-bit, 44100Hz)
    let audio_input_type = create_mixed_audio_input_type()?;

    // Add stream and set input type
    sink_writer.AddStream(&audio_output_type)?;
    sink_writer.SetInputMediaType(stream_index, &audio_input_type, None)?;

    Ok(())
}

unsafe fn create_mixed_audio_input_type() -> Result<IMFMediaType> {
    let input_type: IMFMediaType = MFCreateMediaType()?;
    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM.try_into().unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };

    MFInitMediaTypeFromWaveFormatEx(
        &input_type,
        &wave_format,
        std::mem::size_of::<windows::Win32::Media::Audio::WAVEFORMATEX>()
            .try_into()
            .unwrap(),
    )?;

    Ok(input_type)
}

unsafe fn create_sink_attributes() -> Result<Option<IMFAttributes>> {
    let mut attributes: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attributes, 0)?;

    if let Some(attrs) = &attributes {
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
    }

    Ok(attributes)
}

unsafe fn configure_video_stream(
    sink_writer: &IMFSinkWriter,
    fps_num: u32,
    fps_den: u32,
    output_width: u32,
    output_height: u32,
    video_bitrate: u32,
    video_encoder_id: &GUID,
) -> Result<()> {
    // Create output media type
    let video_output_type = create_video_output_type(fps_num, fps_den, output_width, output_height, video_encoder_id)?;

    // Create input media type
    let video_input_type = create_video_input_type(fps_num, fps_den, output_width, output_height)?;

    // Configure encoder with default settings
    let config_attrs = create_encoder_config(video_bitrate)?;

    // First add stream, then set input type
    sink_writer.AddStream(&video_output_type)?;
    sink_writer.SetInputMediaType(0, &video_input_type, config_attrs.as_ref())?;

    Ok(())
}

unsafe fn create_video_output_type(
    fps_num: u32,
    fps_den: u32,
    output_width: u32,
    output_height: u32,
    video_encoder_id: &GUID,
) -> Result<IMFMediaType> {
    let output_type: IMFMediaType = MFCreateMediaType()?;
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    output_type.SetGUID(&MF_MT_SUBTYPE, video_encoder_id)?;
    output_type.SetUINT64(
        &MF_MT_FRAME_RATE,
        ((fps_num as u64) << 32) | (fps_den as u64),
    )?;
    output_type.SetUINT64(&MF_MT_FRAME_SIZE, ((output_width as u64) << 32) | (output_height as u64))?;
    output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1u64)?;
    output_type.SetUINT32(
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0.try_into().unwrap(),
    )?;
    // Set the appropriate profile based on the encoder type
    if video_encoder_id == &MFVideoFormat_H264 {
        output_type.SetUINT32(
            &MF_MT_VIDEO_PROFILE,
            eAVEncH264VProfile_High.0.try_into().unwrap(),
        )?;
    } else if video_encoder_id == &MFVideoFormat_HEVC {
        // HEVC/H.265 uses different profile constants
        // Use a common profile, typically Main profile for HEVC
        output_type.SetUINT32(
            &MF_MT_VIDEO_PROFILE,
            1, // Main profile for HEVC
        )?;
    }

    Ok(output_type)
}

unsafe fn create_video_input_type(
    fps_num: u32,
    fps_den: u32,
    output_width: u32,
    output_height: u32,
) -> Result<IMFMediaType> {
    let input_type: IMFMediaType = MFCreateMediaType()?;
    input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    input_type.SetUINT64(
        &MF_MT_FRAME_RATE,
        ((fps_num as u64) << 32) | (fps_den as u64),
    )?;
    input_type.SetUINT64(&MF_MT_FRAME_SIZE, ((output_width as u64) << 32) | (output_height as u64))?;
    input_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    input_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)?;
    input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, output_width as u32)?;

    Ok(input_type)
}

unsafe fn create_encoder_config(video_bitrate: u32) -> Result<Option<IMFAttributes>> {
    let mut config_attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut config_attrs, 0)?;

    if let Some(attrs) = &config_attrs {
        attrs.SetUINT32(
            &CODECAPI_AVEncCommonRateControlMode,
            eAVEncCommonRateControlMode_GlobalVBR.0.try_into().unwrap(),
        )?;
        attrs.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, video_bitrate)?;
        attrs.SetUINT32(&CODECAPI_AVEncMPVDefaultBPictureCount, 0)?;
        attrs.SetUINT32(&CODECAPI_AVEncCommonQuality, 70)?;
        attrs.SetUINT32(&CODECAPI_AVEncCommonLowLatency, 1)?;
    }

    Ok(config_attrs)
}

unsafe fn create_audio_output_type() -> Result<IMFMediaType> {
    let output_type: IMFMediaType = MFCreateMediaType()?;
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
    output_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
    output_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
    output_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 44100)?;
    output_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
    output_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 16000)?;

    Ok(output_type)
}

// The create_dxgi_sample function has been moved to SamplePool in types/mod.rs

pub unsafe fn init_media_foundation() -> Result<()> {
    use windows::Win32::System::Com::*;

    CoInitializeEx(Some(ptr::null()), COINIT_MULTITHREADED)?;
    MFStartup(MF_VERSION, MFSTARTUP_FULL)?;

    Ok(())
}

pub unsafe fn shutdown_media_foundation() -> Result<()> {
    use windows::Win32::System::Com::*;

    MFShutdown()?;
    CoUninitialize();

    Ok(())
}