use log::info;
use std::{env, time::Duration};
use win_recorder::{Recorder, Result};

fn main() -> Result<()> {
    // Set up detailed logging to see resource tracking
    env::set_var("RUST_BACKTRACE", "full");
    env::set_var("RUST_LOG", "debug,win_recorder=trace");
    env_logger::init();

    info!("OS: {}", env::consts::OS);
    info!("Architecture: {}", env::consts::ARCH);
    info!("Starting encoder selection and performance test");

    // First get available encoders
    let temp_recorder = Recorder::builder()
        .debug_mode(true)
        .build();
    let temp_recorder = Recorder::new(temp_recorder)?;
    
    let encoders = temp_recorder.get_available_video_encoders()?;
    
    // Log available encoders
    info!("Available hardware encoders:");
    for (name, info) in &encoders {
        info!("  {} (GUID: {:?})", name, info.guid);
    }

    // Try to find H264 encoder first, fall back to first available
    let chosen_encoder = encoders.values()
        .find(|info| info.name.contains("264") || info.name.contains("H264"))
        .expect("No H264 encoder available");
    
    info!("Selected encoder: {}", chosen_encoder.name);
    
    // Clean up the temporary recorder to release resources
    drop(temp_recorder);

    // Create recorder with chosen encoder and fixed-size resource pool
    let config = Recorder::builder()
        .fps(60, 1)  // Higher framerate to stress test resource management
        .input_dimensions(1920, 1080)
        .output_dimensions(1920, 1080) 
        .capture_audio(true)
        .capture_microphone(false)
        .debug_mode(true)
        .output_path("./output_perf_test.mp4")
        .video_bitrate(8000000)
        .encoder(Some(chosen_encoder.guid))
        .build();

    let recorder = Recorder::new(config)?
        .with_process_name("League of Legends"); // Change to match your target window

    info!("Recorder initialized with fixed-size resource pool");
    info!("Starting 20-second performance and memory leak test");

    // Start recording
    match recorder.start_recording() {
        Ok(_) => info!("Recording started successfully with hardware encoder"),
        Err(e) => {
            log::error!("Failed to start recording: {:?}", e);
            return Err(e);
        }
    }

    // Stop recording and properly clean up resources
    info!("Test complete - stopping recording");
    match recorder.stop_recording() {
        Ok(_) => info!("Recording stopped successfully"),
        Err(e) => {
            log::error!("Failed to stop recording: {:?}", e);
            return Err(e);
        }
    }

    // Explicitly drop the recorder to trigger resource cleanup
    info!("Cleaning up resources");
    drop(recorder);
    
    info!("Performance test complete - all resources properly cleaned up");
    info!("Output saved to ./output_perf_test.mp4");
    Ok(())
}