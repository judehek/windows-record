use log::info;
use std::{env, io, time::Duration};
use win_recorder::{Recorder, Result};

fn main() -> Result<()> {
    env::set_var("RUST_BACKTRACE", "full");

    // create recorder with default logging enabled
    let rec = Recorder::new(30, 1, 1920, 1080)?;

    // optionally disable logging
    // rec.disable_logging()?;

    // or set a custom log directory
    // rec.set_log_directory("./logs")?;

    // log system information (these will only show if logging is enabled)
    info!("OS: {}", env::consts::OS);
    info!("Architecture: {}", env::consts::ARCH);
    info!("Application started");

    rec.set_process_name("League of Legends");
    info!("Set process name to League of Legends");

    rec.set_capture_audio(true);
    info!(
        "Audio capture is {}",
        if rec.is_audio_capture_enabled() {
            "enabled"
        } else {
            "disabled"
        }
    );

    std::thread::sleep(Duration::from_secs(3));
    info!("Starting recording");

    let res = rec.start_recording("output.mp4");
    match &res {
        Ok(_) => info!("Recording started successfully"),
        Err(e) => log::error!("Failed to start recording: {:?}", e),
    }

    std::thread::sleep(Duration::from_secs(10));
    info!("Stopping recording");

    let res2 = rec.stop_recording();
    match &res2 {
        Ok(_) => info!("Recording stopped successfully"),
        Err(e) => log::error!("Failed to stop recording: {:?}", e),
    }

    info!("Application finished");
    Ok(())
}
