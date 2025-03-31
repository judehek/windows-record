use log::info;
use std::{env, time::Duration};
use windows_record::{AudioSource, Recorder, Result};

fn main() -> Result<()> {
    // Set up logging to see resource tracking in debug builds
    env::set_var("RUST_BACKTRACE", "full");
    env::set_var("RUST_LOG", "info,windows_record=info");
    env_logger::init();

    // Create recorder
    let config = Recorder::builder()
        .fps(30, 1)
        .input_dimensions(1920, 1080)  
        .output_dimensions(1920, 1080)
        .capture_audio(true)
        .capture_microphone(true)
        .audio_source(AudioSource::Desktop)
        .microphone_volume(1.0)
        .system_volume(1.0)
        .debug_mode(true)
        .output_path("output.mp4")
        .build();

    // Create the recorder with your target window name
    let recorder = Recorder::new(config)?
        .with_process_name("League of Legends");

    // Short delay before starting recording
    std::thread::sleep(Duration::from_secs(1));
    info!("Starting recording");

    // Start recording
    match recorder.start_recording() {
        Ok(_) => info!("Recording started successfully"),
        Err(e) => {
            log::error!("Failed to start recording: {:?}", e);
            return Err(e);
        }
    }

    // Record for 10 seconds
    info!("Recording for 10 seconds...");
    std::thread::sleep(Duration::from_secs(10));
    
    // Stop recording
    info!("Stopping recording");
    recorder.stop_recording()?;
    
    info!("Application finished - all resources properly cleaned up");
    Ok(())
}