use log::info;
use std::{env, time::Duration};
use win_recorder::{AudioSource, Recorder, Result};

fn main() -> Result<()> {
    // Set up logging to see resource tracking in debug builds
    env::set_var("RUST_BACKTRACE", "full");
    env::set_var("RUST_LOG", "info,win_recorder=info");
    env_logger::init();

    info!("OS: {}", env::consts::OS);
    info!("Architecture: {}", env::consts::ARCH);
    info!("Application started");

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
    // For this example, use a window that's currently open on your system
    let recorder = Recorder::new(config)?
        .with_process_name("Chrome");

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
    
    // Stop recording and properly clean up resources
    info!("Stopping recording");
    match recorder.stop_recording() {
        Ok(_) => info!("Recording stopped successfully"),
        Err(e) => {
            log::error!("Failed to stop recording: {:?}", e);
            return Err(e);
        }
    }

    // Explicitly drop the recorder to trigger resource cleanup
    drop(recorder);
    
    info!("Application finished - all resources properly cleaned up");
    Ok(())
}