use std::path::PathBuf;
use std::ptr;

use windows::core::{ComInterface, Result, GUID};
use windows::Win32::Foundation::TRUE;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::Win32::Media::MediaFoundation::*;

pub unsafe fn create_sink_writer(
    output_path: &str,
    fps_num: u32,
    fps_den: u32,
    s_width: u32,
    s_height: u32,
    capture_audio: bool,
    capture_microphone: bool,
    video_bitrate: u32,
    encoder_guid: Option<GUID>,
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
    configure_video_stream(&sink_writer, fps_num, fps_den, s_width, s_height, video_bitrate, encoder_guid)?;
    current_stream_index += 1;

    // Configure process audio stream
    if capture_audio {
        configure_audio_stream(&sink_writer, current_stream_index, false)?;
        current_stream_index += 1;
    }

    // Configure microphone stream
    if capture_microphone {
        configure_audio_stream(&sink_writer, current_stream_index, true)?;
        // current_stream_index += 1; // Only needed if we more streams after this
    }

    Ok(sink_writer)
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
    width: u32,
    height: u32,
    video_bitrate: u32,
    encoder_guid: Option<GUID>,
) -> Result<()> {
    // Create output media type
    let video_output_type = create_video_output_type(fps_num, fps_den, width, height)?;

    // Create input media type
    let video_input_type = create_video_input_type(fps_num, fps_den, width, height)?;

    // Configure encoder
    let config_attrs = create_encoder_config(video_bitrate)?;

    // First add stream, then set input type
    sink_writer.AddStream(&video_output_type)?;
    sink_writer.SetInputMediaType(0, &video_input_type, config_attrs.as_ref())?;

    Ok(())
}

unsafe fn configure_audio_stream(
    sink_writer: &IMFSinkWriter,
    stream_index: u32,
    is_microphone: bool,
) -> Result<()> {
    // Create output type (same for both)
    let audio_output_type = create_audio_output_type()?;

    // Create appropriate input type based on source
    let audio_input_type = if is_microphone {
        create_microphone_input_type()?
    } else {
        create_system_audio_input_type()?
    };

    // First add stream, then set input type
    sink_writer.AddStream(&audio_output_type)?;
    sink_writer.SetInputMediaType(stream_index, &audio_input_type, None)?;

    Ok(())
}

unsafe fn create_video_output_type(
    fps_num: u32,
    fps_den: u32,
    width: u32,
    height: u32,
) -> Result<IMFMediaType> {
    let output_type: IMFMediaType = MFCreateMediaType()?;
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
    output_type.SetUINT64(
        &MF_MT_FRAME_RATE,
        ((fps_num as u64) << 32) | (fps_den as u64),
    )?;
    output_type.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
    output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1u64)?;
    output_type.SetUINT32(
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0.try_into().unwrap(),
    )?;
    output_type.SetUINT32(
        &MF_MT_VIDEO_PROFILE,
        eAVEncH264VProfile_High.0.try_into().unwrap(),
    )?;

    Ok(output_type)
}

unsafe fn create_video_input_type(
    fps_num: u32,
    fps_den: u32,
    width: u32,
    height: u32,
) -> Result<IMFMediaType> {
    let input_type: IMFMediaType = MFCreateMediaType()?;
    input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    input_type.SetUINT64(
        &MF_MT_FRAME_RATE,
        ((fps_num as u64) << 32) | (fps_den as u64),
    )?;
    input_type.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
    input_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    input_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)?;
    input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, width as u32)?;

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
    output_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 12000)?;

    Ok(output_type)
}

unsafe fn create_system_audio_input_type() -> Result<IMFMediaType> {
    let input_type: IMFMediaType = MFCreateMediaType()?;
    let wav_format = windows::Win32::Media::Audio::WAVEFORMATEX {
        wFormatTag: windows::Win32::Media::Audio::WAVE_FORMAT_PCM
            .try_into()
            .unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };

    MFInitMediaTypeFromWaveFormatEx(
        &input_type,
        &wav_format,
        std::mem::size_of::<windows::Win32::Media::Audio::WAVEFORMATEX>()
            .try_into()
            .unwrap(),
    )?;

    Ok(input_type)
}

unsafe fn create_microphone_input_type() -> Result<IMFMediaType> {
    let input_type: IMFMediaType = MFCreateMediaType()?;
    let wav_format = windows::Win32::Media::Audio::WAVEFORMATEX {
        wFormatTag: 3u16,        // WAVE_FORMAT_IEEE_FLOAT = 3
        nChannels: 1,            // Mono from microphone
        nSamplesPerSec: 48000,   // Microphone sample rate
        nAvgBytesPerSec: 192000, // 48000 * 4 bytes * 1 channel
        nBlockAlign: 4,          // 32 bits = 4 bytes per sample
        wBitsPerSample: 32,      // 32-bit float
        cbSize: 0,
    };

    MFInitMediaTypeFromWaveFormatEx(
        &input_type,
        &wav_format,
        std::mem::size_of::<windows::Win32::Media::Audio::WAVEFORMATEX>()
            .try_into()
            .unwrap(),
    )?;

    Ok(input_type)
}

pub unsafe fn create_dxgi_sample(texture: &ID3D11Texture2D, fps_num: u32) -> Result<IMFSample> {
    let surface: IDXGISurface = texture.cast()?;

    let sample: IMFSample = MFCreateSample()?;
    let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &surface, 0, TRUE)?;

    sample.AddBuffer(&buffer)?;
    sample.SetSampleDuration(10_000_000 / fps_num as i64)?;

    Ok(sample)
}

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
