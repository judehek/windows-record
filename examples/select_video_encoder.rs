use std::time::Duration;
use std::{env, io};
use log::{error, info};
use win_recorder::{enumerate_video_encoders, VideoEncoder, VideoEncoderType, Recorder, Result, RecorderConfig};

fn main() -> Result<()> {
    // Set up logging to see resource tracking in debug builds
    env::set_var("RUST_BACKTRACE", "full");
    env::set_var("RUST_LOG", "info,win_recorder=info");
    env_logger::init();

    info!("OS: {}", env::consts::OS);
    info!("Architecture: {}", env::consts::ARCH);
    info!("Application started");
    
    // Get list of available video encoders
    let encoders = enumerate_video_encoders()?;
    
    info!("Available video encoders:");
    for (i, encoder) in encoders.iter().enumerate() {
        info!("{}. {}", i + 1, encoder.name);
    }
    
    // Prompt user to select an encoder
    println!("Enter encoder number (or 0 for default): ");
    let mut input = String::new();
    io::stdin().read_line(&mut input).expect("Failed to read input");
    let encoder_idx: usize = input.trim().parse().unwrap_or(0);
    
    // Get selected encoder type or default
    let selected_encoder_type = if encoder_idx > 0 && encoder_idx <= encoders.len() {
        match encoders[encoder_idx - 1].name.as_str() {
            "H.264 (AVC)" => VideoEncoderType::H264,
            "H.265 (HEVC)" => VideoEncoderType::HEVC,
            _ => VideoEncoderType::default(),
        }
    } else {
        VideoEncoderType::default()
    };
    
    let encoder_name = match selected_encoder_type {
        VideoEncoderType::H264 => "H.264 (AVC)",
        VideoEncoderType::HEVC => "H.265 (HEVC)",
    };
    
    println!("Selected encoder: {}", encoder_name);
    
    // Create a recorder with the selected encoder
    let config = RecorderConfig::builder()
        .fps(30, 1)
        .input_dimensions(1920, 1080)
        .output_dimensions(1920, 1080)
        .capture_audio(true)
        .capture_microphone(false)
        .video_encoder(selected_encoder_type)
        .output_path("encoder_test.mp4")
        .build();
    
    // Create and start the recorder
    let recorder = Recorder::new(config)?
        .with_process_name("League of Legends (TM) Client");

    info!("Starting recording with {} encoder.", encoder_name);

    // Short delay before starting recording
    std::thread::sleep(Duration::from_secs(1));
    info!("Starting recording");

    // Start recording
    match recorder.start_recording() {
        Ok(_) => info!("Recording started successfully"),
        Err(e) => {
            error!("Failed to start recording: {:?}", e);
            return Err(e);
        }
    }
    
    // Record for 10 seconds
    info!("Recording for 10 seconds...");
    std::thread::sleep(Duration::from_secs(10));

    // Stop recording and properly clean up resources
    info!("Stopping recording");
    match recorder.stop_recording() {
        Ok(_) => info!("Recording stopped successfully"),
        Err(e) => {
            error!("Failed to stop recording: {:?}", e);
            return Err(e);
        }
    }

    drop(recorder);
    
    Ok(())
}