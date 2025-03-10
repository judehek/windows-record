use std::io;
use std::path::PathBuf;
use windows::core::Result;
use win_recorder::{enumerate_audio_input_devices, AudioInputDevice, Recorder, RecorderConfig};

fn main() -> Result<()> {
    // Enable debug logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    
    // Get list of available audio input devices
    let devices = enumerate_audio_input_devices()?;
    
    println!("Available audio input devices:");
    for (i, device) in devices.iter().enumerate() {
        println!("{}. {}", i + 1, device.name);
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
        .fps(60, 1)
        .input_dimensions(1920, 1080)
        .output_dimensions(1920, 1080)
        .capture_audio(true)
        .capture_microphone(true)
        .microphone_device(selected_device.map(|d| d.name.clone()))
        .output_path(PathBuf::from("recording.mp4"))
        .build();
    
    // Prompt user for the window to record
    println!("Enter process name or window title to record: ");
    let mut window_name = String::new();
    io::stdin().read_line(&mut window_name).expect("Failed to read input");
    let window_name = window_name.trim();
    
    // Create and start the recorder
    let mut recorder = Recorder::new(&config)?;
    
    println!("Recording started with{} microphone device. Press Enter to stop...", 
        if selected_device.is_some() { " selected" } else { " default" });
    
    let mut input = String::new();
    io::stdin().read_line(&mut input).expect("Failed to read input");
    
    // Stop the recording
    recorder.stop()?;
    println!("Recording stopped");
    
    Ok(())
}