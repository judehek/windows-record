use log::info;
use std::{env, time::Duration};
use win_recorder::{Recorder, Result};

fn main() -> Result<()> {
    env::set_var("RUST_BACKTRACE", "full");

    // Create recorder using builder pattern
    let config = Recorder::builder()
        .fps(30, 1)
        .dimensions(1920, 1080)
        .capture_audio(true)
        .capture_microphone(true)
        .debug_mode(true)  // Enables logging
        .output_dir(Some("./logs"))
        .build();

    let recorder = Recorder::new(config)?
        .with_process_name("League of Legends");

    // Log system information
    info!("OS: {}", env::consts::OS);
    info!("Architecture: {}", env::consts::ARCH);
    info!("Application started");

    std::thread::sleep(Duration::from_secs(3));
    info!("Starting recording");

    let res = recorder.start_recording("output.mp4");
    match &res {
        Ok(_) => info!("Recording started successfully"),
        Err(e) => log::error!("Failed to start recording: {:?}", e),
    }

    std::thread::sleep(Duration::from_secs(10));
    info!("Stopping recording");

    let res2 = recorder.stop_recording();
    match &res2 {
        Ok(_) => info!("Recording stopped successfully"),
        Err(e) => log::error!("Failed to stop recording: {:?}", e),
    }

    info!("Application finished");
    Ok(())
}