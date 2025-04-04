use std::ptr;

use log::{error, info};
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
    video_encoder_guid: &GUID,
) -> Result<IMFSinkWriter> {
    info!("create_sink_writer - Starting with path: {}", output_path);
    info!("create_sink_writer - Parameters: fps={}/{}, resolution={}x{}, audio={}, mic={}, bitrate={}, encoder={:?}", 
          fps_num, fps_den, output_width, output_height, capture_audio, capture_microphone, video_bitrate, video_encoder_guid);

    // Create and configure attributes
    info!("create_sink_writer - Creating sink attributes");
    let attributes = create_sink_attributes()?;
    info!("create_sink_writer - Sink attributes created successfully");

    // Create sink writer
    info!(
        "create_sink_writer - Creating sink writer from URL: {}",
        output_path
    );
    let sink_writer: IMFSinkWriter = MFCreateSinkWriterFromURL(
        &windows::core::HSTRING::from(output_path),
        None,
        attributes.as_ref(),
    )?;
    info!("create_sink_writer - Sink writer created successfully");

    let mut current_stream_index = 0;

    // Configure video stream (always stream index 0)
    info!(
        "create_sink_writer - Configuring video stream at index {}",
        current_stream_index
    );
    configure_video_stream(
        &sink_writer,
        fps_num,
        fps_den,
        output_width,
        output_height,
        video_bitrate,
        video_encoder_guid,
    )?;
    info!("create_sink_writer - Video stream configured successfully");
    current_stream_index += 1;

    // Configure a single audio stream if either audio source is enabled
    if capture_audio || capture_microphone {
        info!("create_sink_writer - Audio capture enabled, configuring mixed audio stream at index {}", current_stream_index);
        // Use a new function that configures a stream suitable for mixed audio
        configure_mixed_audio_stream(&sink_writer, current_stream_index)?;
        info!("create_sink_writer - Mixed audio stream configured successfully");
        // current_stream_index += 1;
    } else {
        info!("create_sink_writer - Audio capture disabled, skipping audio stream configuration");
    }

    info!("create_sink_writer - Completed successfully");
    Ok(sink_writer)
}

unsafe fn configure_mixed_audio_stream(
    sink_writer: &IMFSinkWriter,
    stream_index: u32,
) -> Result<()> {
    info!(
        "configure_mixed_audio_stream - Starting for stream index {}",
        stream_index
    );

    // Create output type
    info!("configure_mixed_audio_stream - Creating audio output type");
    let audio_output_type = create_audio_output_type()?;
    info!("configure_mixed_audio_stream - Audio output type created successfully");

    // Create input type suitable for mixed audio (stereo, 16-bit, 44100Hz)
    info!("configure_mixed_audio_stream - Creating mixed audio input type");
    let audio_input_type = create_mixed_audio_input_type()?;
    info!("configure_mixed_audio_stream - Mixed audio input type created successfully");

    // Add stream and set input type
    info!("configure_mixed_audio_stream - Adding audio stream to sink writer");
    sink_writer.AddStream(&audio_output_type)?;
    info!("configure_mixed_audio_stream - Audio stream added successfully");

    info!(
        "configure_mixed_audio_stream - Setting input media type for stream {}",
        stream_index
    );
    sink_writer.SetInputMediaType(stream_index, &audio_input_type, None)?;
    info!("configure_mixed_audio_stream - Input media type set successfully");

    info!("configure_mixed_audio_stream - Completed successfully");
    Ok(())
}

unsafe fn create_mixed_audio_input_type() -> Result<IMFMediaType> {
    info!("create_mixed_audio_input_type - Starting");
    let input_type: IMFMediaType = MFCreateMediaType()?;
    info!("create_mixed_audio_input_type - Media type created");

    info!("create_mixed_audio_input_type - Creating WAVEFORMATEX structure");
    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM.try_into().unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };

    info!("create_mixed_audio_input_type - Initializing media type from WAVEFORMATEX");
    MFInitMediaTypeFromWaveFormatEx(
        &input_type,
        &wave_format,
        std::mem::size_of::<windows::Win32::Media::Audio::WAVEFORMATEX>()
            .try_into()
            .unwrap(),
    )?;
    info!("create_mixed_audio_input_type - Media type initialized successfully");

    info!("create_mixed_audio_input_type - Completed successfully");
    Ok(input_type)
}

unsafe fn create_sink_attributes() -> Result<Option<IMFAttributes>> {
    info!("create_sink_attributes - Starting");
    let mut attributes: Option<IMFAttributes> = None;
    info!("create_sink_attributes - Creating attributes");
    MFCreateAttributes(&mut attributes, 0)?;
    info!("create_sink_attributes - Attributes created");

    if let Some(attrs) = &attributes {
        info!("create_sink_attributes - Setting MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS to 1");
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        info!("create_sink_attributes - Setting MF_SINK_WRITER_DISABLE_THROTTLING to 1");
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        info!("create_sink_attributes - All attributes set successfully");
    } else {
        info!("create_sink_attributes - Attributes object is None, skipping settings");
    }

    info!("create_sink_attributes - Completed successfully");
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
    info!("configure_video_stream - Starting with fps={}/{}, resolution={}x{}, bitrate={}, encoder={:?}", 
          fps_num, fps_den, output_width, output_height, video_bitrate, video_encoder_id);

    // Create output media type
    info!("configure_video_stream - Creating video output type");
    let video_output_type = create_video_output_type(
        fps_num,
        fps_den,
        output_width,
        output_height,
        video_encoder_id,
    )?;
    info!("configure_video_stream - Video output type created successfully");

    // Create input media type
    info!("configure_video_stream - Creating video input type");
    let video_input_type = create_video_input_type(fps_num, fps_den, output_width, output_height)?;
    info!("configure_video_stream - Video input type created successfully");

    // Configure encoder with default settings
    info!(
        "configure_video_stream - Creating encoder configuration with bitrate {}",
        video_bitrate
    );
    let config_attrs = create_encoder_config(video_bitrate)?;
    info!("configure_video_stream - Encoder configuration created successfully");

    // First add stream, then set input type
    info!("configure_video_stream - Adding video stream to sink writer");
    sink_writer.AddStream(&video_output_type)?;
    info!("configure_video_stream - Video stream added successfully");

    info!("configure_video_stream - Setting input media type for stream 0");
    match sink_writer.SetInputMediaType(0, &video_input_type, config_attrs.as_ref()) {
        Ok(_) => info!("Input media type set successfully"),
        Err(e) => error!("Failed to set input media type: {:?}", e),
    }
    info!("configure_video_stream - Input media type set successfully");

    info!("configure_video_stream - Completed successfully");
    Ok(())
}

unsafe fn create_video_output_type(
    fps_num: u32,
    fps_den: u32,
    output_width: u32,
    output_height: u32,
    video_encoder_id: &GUID,
) -> Result<IMFMediaType> {
    info!(
        "create_video_output_type - Starting with fps={}/{}, resolution={}x{}, encoder={:?}",
        fps_num, fps_den, output_width, output_height, video_encoder_id
    );

    info!("create_video_output_type - Creating media type");
    let output_type: IMFMediaType = MFCreateMediaType()?;
    info!("create_video_output_type - Media type created");

    info!("create_video_output_type - Setting MF_MT_MAJOR_TYPE to MFMediaType_Video");
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    info!("create_video_output_type - Setting MF_MT_SUBTYPE to encoder format");
    output_type.SetGUID(&MF_MT_SUBTYPE, video_encoder_id)?;

    info!(
        "create_video_output_type - Setting MF_MT_FRAME_RATE to {}/{}",
        fps_num, fps_den
    );
    output_type.SetUINT64(
        &MF_MT_FRAME_RATE,
        ((fps_num as u64) << 32) | (fps_den as u64),
    )?;

    info!(
        "create_video_output_type - Setting MF_MT_FRAME_SIZE to {}x{}",
        output_width, output_height
    );
    output_type.SetUINT64(
        &MF_MT_FRAME_SIZE,
        ((output_width as u64) << 32) | (output_height as u64),
    )?;

    info!("create_video_output_type - Setting MF_MT_PIXEL_ASPECT_RATIO to 1:1");
    output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1u64)?;

    info!("create_video_output_type - Setting MF_MT_INTERLACE_MODE to Progressive");
    output_type.SetUINT32(
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0.try_into().unwrap(),
    )?;

    // Set the appropriate profile based on the encoder type
    if video_encoder_id == &MFVideoFormat_H264 {
        info!("create_video_output_type - Detected H264 encoder, setting profile to High");
        output_type.SetUINT32(
            &MF_MT_VIDEO_PROFILE,
            eAVEncH264VProfile_High.0.try_into().unwrap(),
        )?;
    } else if video_encoder_id == &MFVideoFormat_HEVC {
        // HEVC/H.265 uses different profile constants
        // Use a common profile, typically Main profile for HEVC
        info!("create_video_output_type - Detected HEVC encoder, setting profile to Main (1)");
        output_type.SetUINT32(
            &MF_MT_VIDEO_PROFILE,
            1, // Main profile for HEVC
        )?;
    } else {
        info!("create_video_output_type - Unknown encoder type, no specific profile set");
    }

    info!("create_video_output_type - Completed successfully");
    Ok(output_type)
}

unsafe fn create_video_input_type(
    fps_num: u32,
    fps_den: u32,
    output_width: u32,
    output_height: u32,
) -> Result<IMFMediaType> {
    info!(
        "create_video_input_type - Starting with fps={}/{}, resolution={}x{}",
        fps_num, fps_den, output_width, output_height
    );

    info!("create_video_input_type - Creating media type");
    let input_type: IMFMediaType = MFCreateMediaType()?;
    info!("create_video_input_type - Media type created");

    info!("create_video_input_type - Setting MF_MT_MAJOR_TYPE to MFMediaType_Video");
    input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    info!("create_video_input_type - Setting MF_MT_SUBTYPE to MFVideoFormat_NV12");
    input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;

    info!(
        "create_video_input_type - Setting MF_MT_FRAME_RATE to {}/{}",
        fps_num, fps_den
    );
    input_type.SetUINT64(
        &MF_MT_FRAME_RATE,
        ((fps_num as u64) << 32) | (fps_den as u64),
    )?;

    info!(
        "create_video_input_type - Setting MF_MT_FRAME_SIZE to {}x{}",
        output_width, output_height
    );
    input_type.SetUINT64(
        &MF_MT_FRAME_SIZE,
        ((output_width as u64) << 32) | (output_height as u64),
    )?;

    info!("create_video_input_type - Setting MF_MT_INTERLACE_MODE to Progressive");
    input_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;

    info!("create_video_input_type - Setting MF_MT_ALL_SAMPLES_INDEPENDENT to 1");
    input_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;

    info!("create_video_input_type - Setting MF_MT_PIXEL_ASPECT_RATIO to 1:1");
    input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)?;

    info!(
        "create_video_input_type - Setting MF_MT_DEFAULT_STRIDE to {}",
        output_width
    );
    input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, output_width as u32)?;

    info!("create_video_input_type - Completed successfully");
    Ok(input_type)
}

unsafe fn create_encoder_config(video_bitrate: u32) -> Result<Option<IMFAttributes>> {
    info!(
        "create_encoder_config - Starting with bitrate {}",
        video_bitrate
    );

    let mut config_attrs: Option<IMFAttributes> = None;
    info!("create_encoder_config - Creating attributes");
    MFCreateAttributes(&mut config_attrs, 0)?;
    info!("create_encoder_config - Attributes created");

    if let Some(attrs) = &config_attrs {
        info!("create_encoder_config - Setting CODECAPI_AVEncCommonRateControlMode to GlobalVBR");
        attrs.SetUINT32(
            &CODECAPI_AVEncCommonRateControlMode,
            eAVEncCommonRateControlMode_GlobalVBR.0.try_into().unwrap(),
        )?;

        info!(
            "create_encoder_config - Setting CODECAPI_AVEncCommonMeanBitRate to {}",
            video_bitrate
        );
        attrs.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, video_bitrate)?;

        info!("create_encoder_config - Setting CODECAPI_AVEncMPVDefaultBPictureCount to 0");
        attrs.SetUINT32(&CODECAPI_AVEncMPVDefaultBPictureCount, 0)?;

        info!("create_encoder_config - Setting CODECAPI_AVEncCommonQuality to 70");
        attrs.SetUINT32(&CODECAPI_AVEncCommonQuality, 70)?;

        info!("create_encoder_config - Setting CODECAPI_AVEncCommonLowLatency to 1");
        attrs.SetUINT32(&CODECAPI_AVEncCommonLowLatency, 1)?;

        info!("create_encoder_config - All encoder attributes set successfully");
    } else {
        info!("create_encoder_config - Attributes object is None, skipping settings");
    }

    info!("create_encoder_config - Completed successfully");
    Ok(config_attrs)
}

unsafe fn create_audio_output_type() -> Result<IMFMediaType> {
    info!("create_audio_output_type - Starting");

    info!("create_audio_output_type - Creating media type");
    let output_type: IMFMediaType = MFCreateMediaType()?;
    info!("create_audio_output_type - Media type created");

    info!("create_audio_output_type - Setting MF_MT_MAJOR_TYPE to MFMediaType_Audio");
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;

    info!("create_audio_output_type - Setting MF_MT_SUBTYPE to MFAudioFormat_AAC");
    output_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;

    info!("create_audio_output_type - Setting MF_MT_AUDIO_BITS_PER_SAMPLE to 16");
    output_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;

    info!("create_audio_output_type - Setting MF_MT_AUDIO_SAMPLES_PER_SECOND to 44100");
    output_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 44100)?;

    info!("create_audio_output_type - Setting MF_MT_AUDIO_NUM_CHANNELS to 2");
    output_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;

    info!("create_audio_output_type - Setting MF_MT_AUDIO_AVG_BYTES_PER_SECOND to 16000");
    output_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 16000)?;

    info!("create_audio_output_type - Completed successfully");
    Ok(output_type)
}

// The create_dxgi_sample function has been moved to SamplePool in types/mod.rs

pub unsafe fn init_media_foundation() -> Result<()> {
    info!("init_media_foundation - Starting");

    use windows::Win32::System::Com::*;

    info!("init_media_foundation - Initializing COM with COINIT_MULTITHREADED");
    CoInitializeEx(Some(ptr::null()), COINIT_MULTITHREADED)?;
    info!("init_media_foundation - COM initialized successfully");

    info!("init_media_foundation - Starting Media Foundation with MF_VERSION and MFSTARTUP_FULL");
    MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    info!("init_media_foundation - Media Foundation started successfully");

    info!("init_media_foundation - Completed successfully");
    Ok(())
}

pub unsafe fn shutdown_media_foundation() -> Result<()> {
    info!("shutdown_media_foundation - Starting");

    use windows::Win32::System::Com::*;

    info!("shutdown_media_foundation - Shutting down Media Foundation");
    MFShutdown()?;
    info!("shutdown_media_foundation - Media Foundation shut down successfully");

    info!("shutdown_media_foundation - Uninitializing COM");
    CoUninitialize();
    info!("shutdown_media_foundation - COM uninitialized");

    info!("shutdown_media_foundation - Completed successfully");
    Ok(())
}
