use std::time::Duration;
use std::{env, io};
use log::{error, info};
use windows_record::{
    enumerate_video_encoders, get_preferred_video_encoder_by_type, Recorder, RecorderConfig, RecorderError, Result, VideoEncoderType
};

fn main() -> Result<()> {
    // Set up logging to see resource tracking in debug builds
    env::set_var("RUST_BACKTRACE", "full");
    env::set_var("RUST_LOG", "info,windows_record=info");
    env_logger::init();
    
    // Get list of available video encoders
    let encoders = enumerate_video_encoders()?;
    
    info!("Available video encoders:");
    for (i, encoder) in encoders.iter().enumerate() {
        info!("{}. {} ({:?})", i + 1, encoder.name, encoder.encoder_type);
    }
    
    // Prompt user to select an encoder
    println!("Enter encoder number (or 0 for default): ");
    let mut input = String::new();
    io::stdin().read_line(&mut input).expect("Failed to read input");
    let encoder_idx: usize = input.trim().parse().unwrap_or(0);
    
    // Get selected encoder or default
    let selected_encoder = if encoder_idx > 0 && encoder_idx <= encoders.len() {
        encoders[encoder_idx - 1].clone()
    } else {
        // Try to get a preferred encoder, otherwise error out
        get_preferred_video_encoder_by_type(VideoEncoderType::H264)
            .or_else(|| get_preferred_video_encoder_by_type(VideoEncoderType::HEVC))
            .ok_or_else(|| {
                error!("No suitable video encoders found on the system");
                RecorderError::Generic(
                    "No suitable video encoders found on the system".to_string()
                )
            })?
    };
    
    info!("Selected encoder: {} ({:?})", selected_encoder.name, selected_encoder.encoder_type);
    
    // Create a recorder with the selected encoder
    let config = RecorderConfig::builder()
        .fps(30, 1)
        .input_dimensions(1920, 1080)
        .output_dimensions(1920, 1080)
        .capture_audio(true)
        .capture_microphone(false)
        .video_encoder(selected_encoder.encoder_type) // Use the encoder type from selected encoder
        .video_encoder_name(&selected_encoder.name)   // Add this line to specify encoder by name
        .output_path("encoder_test.mp4")
        .build();
    
    // Create and start the recorder
    let recorder = Recorder::new(config)?
        .with_process_name("Chrome");

    info!("Starting recording with {} encoder.", selected_encoder.name);

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
    
    // Record for 30 seconds
    info!("Recording for 30 seconds...");
    std::thread::sleep(Duration::from_secs(30));

    // Stop recording
    recorder.stop_recording()?;
    
    Ok(())
}