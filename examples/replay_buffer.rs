use std::io::{self, Write};
use std::path::PathBuf;
use std::{env};
use log::{error, info};
use windows_record::{AudioSource, Recorder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env::set_var("RUST_BACKTRACE", "full");
    env::set_var("RUST_LOG", "info,windows_record=info");
    env_logger::init();
    
    info!("=== Replay Buffer Example ===");
    info!("This example demonstrates the replay buffer functionality.");
    info!("- It will start a recording with the replay buffer enabled.");
    info!("- Press 'S' and 'return' to save the last 10 seconds as a replay.");
    info!("- Press 'Q' and 'return' to quit the program.");
    
    // Get process name to record
    print!("Enter process name to record (e.g., 'notepad'): ");
    io::stdout().flush()?;
    
    let mut process_name = String::new();
    io::stdin().read_line(&mut process_name)?;
    let process_name = process_name.trim();
    
    if process_name.is_empty() {
        info!("No process name provided. Exiting.");
        return Ok(());
    }
    
    // Create recorder with replay buffer enabled
    let recorder = Recorder::builder()
        .fps(30, 1)
        .output_path(PathBuf::from("recording.mp4"))
        .capture_audio(true)
        .audio_source(AudioSource::Desktop)
        .capture_microphone(true)
        .enable_replay_buffer(true)
        .replay_buffer_seconds(10) // Keep last 10 seconds in buffer
        .build();
        
    let recorder = Recorder::new(recorder)?.with_process_name(process_name);
    
    info!("Starting recording with replay buffer enabled...");
    recorder.start_recording()?;
    
    info!("Recording started! Press 'S' to save replay, 'Q' to quit.");
    
    // Setup input handling for keypresses
    let mut replay_count = 0;
    
    loop {
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        
        match input.trim().to_uppercase().as_str() {
            "S" => {
                replay_count += 1;
                let replay_path = format!("replay_{}.mp4", replay_count);
                info!("Saving replay to {}", replay_path);
                
                match recorder.save_replay(&replay_path) {
                    Ok(_) => info!("Replay saved successfully!"),
                    Err(e) => error!("Failed to save replay: {}", e),
                }
            }
            "Q" => {
                info!("Stopping recording...");
                break;
            }
            _ => {
                info!("Unknown command. Press 'S' to save replay, 'Q' to quit.");
            }
        }
    }
    
    recorder.stop_recording()?;
    info!("Recording stopped. Replays saved: {}", replay_count);
    
    Ok(())
}