use log::info;
use std::{env, time::Duration};
use win_recorder::{Recorder, Result};

fn main() -> Result<()> {
    // Set up logging to see resource tracking in debug builds
    env::set_var("RUST_BACKTRACE", "full");
    env::set_var("RUST_LOG", "debug,win_recorder=trace");
    env_logger::init(); 

    info!("OS: {}", env::consts::OS);
    info!("Architecture: {}", env::consts::ARCH);
    info!("Application started");

    // Create recorder with optimal configuration for resource management
    let config = Recorder::builder()
        .fps(30, 1)
        .input_dimensions(1920, 1080)  
        .output_dimensions(1920, 1080)
        .capture_audio(true)
        .capture_microphone(false)
        .debug_mode(true)  // Enable debug logging
        .output_path("output.mp4")
        .build();

    // Create the recorder with your target window name
    // For this example, use a window that's currently open on your system
    let recorder = Recorder::new(config)?
        .with_process_name("League of Legends (TM) Client");  // Change to match your target window

    // Short delay before starting recording
    std::thread::sleep(Duration::from_secs(2));
    info!("Starting recording");

    // Start recording
    match recorder.start_recording() {
        Ok(_) => info!("Recording started successfully"),
        Err(e) => {
            log::error!("Failed to start recording: {:?}", e);
            return Err(e);
        }
    }

    // Record for 10 seconds - long enough to test memory usage over time
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
    
    // Force a GC to clean up any unreferenced resources
    //std::mem::drop(std::mem::take_mut(&mut Vec::<()>::new()));
    
    info!("Application finished - all resources properly cleaned up");
    Ok(())
}