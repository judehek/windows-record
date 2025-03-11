use std::time::Duration;
use std::{env, io};
use log::{error, info};
use win_recorder::{enumerate_audio_input_devices, AudioInputDevice, Recorder, Result, RecorderConfig};

fn main() -> Result<()> {
    // Set up logging to see resource tracking in debug builds
    env::set_var("RUST_BACKTRACE", "full");
    env::set_var("RUST_LOG", "info,win_recorder=info");
    env_logger::init();

    info!("OS: {}", env::consts::OS);
    info!("Architecture: {}", env::consts::ARCH);
    info!("Application started");
    
    // Get list of available audio input devices
    let devices = enumerate_audio_input_devices()?;
    
    info!("Available audio input devices:");
    for (i, device) in devices.iter().enumerate() {
        info!("{}. {}", i + 1, device.name);
    }
    
    // Prompt user to select a device
    println!("Enter device number (or 0 for default): ");
    let mut input = String::new();
    io::stdin().read_line(&mut input).expect("Failed to read input");
    let device_idx: usize = input.trim().parse().unwrap_or(0);
    
    // Get selected device or None for default
    let selected_device: Option<&AudioInputDevice> = if device_idx > 0 && device_idx <= devices.len() {
        Some(&devices[device_idx - 1])
    } else {
        None
    };
    
    if let Some(device) = selected_device {
        println!("Selected device: {}", device.name);
    } else {
        println!("Using default device");
    }
    
    // Create a recorder with the selected device
    let config = RecorderConfig::builder()
        .fps(30, 1)
        .input_dimensions(1920, 1080)
        .output_dimensions(1920, 1080)
        .capture_audio(true)
        .capture_microphone(true)
        .microphone_device(selected_device.map(|d| d.name.clone()))
        .output_path("device.mp4")
        .build();
    
    // Create and start the recorder
    let recorder = Recorder::new(config)?
        .with_process_name("League of Legends (TM) Client");

    info!("Starting recording with{} microphone device.", 
        if selected_device.is_some() { " selected" } else { " default" });

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