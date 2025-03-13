# Windows Record

## Overview

Windows Record is an efficient Rust library designed for seamless window recording. Using the Windows Desktop Duplication API, it captures window content directly without the yellow recording border caused by the WGC API. The library prioritizes performance and ease-of-use.

## Key Features

- No yellow border during capture
- Simple, builder pattern
- Built-in audio support
- Replay buffering (similar to ShadowPlay)
- Fully configurable

## Usage Example

```rust
use windows_record::{Recorder, Result};

fn main() -> Result<()> {
    // Create recorder with builder pattern
    let config = Recorder::builder()
        .fps(30, 1)
        .input_dimensions(1920, 1080)
        .output_dimensions(1920, 1080)
        .capture_audio(true)
        .output_path("output.mp4")
        .build();

    // Initialize with target window
    let recorder = Recorder::new(config)?
        .with_process_name("Your Window Name");

    // Start recording
    recorder.start_recording()?;
    
    // Record for desired duration
    std::thread::sleep(std::time::Duration::from_secs(30));
    
    // Stop recording and clean up resources
    recorder.stop_recording()?;
    
    Ok(())
}
```

## Replay Buffer

The replay buffer feature allows you to continuously record in the background and save only the last N seconds when something interesting happens. This is useful for gameplay recording and similar scenarios where you want to capture events after they happen.

```rust
use windows_record::{Recorder, Result};

fn main() -> Result<()> {
    // Create recorder with replay buffer enabled
    let config = Recorder::builder()
        .fps(30, 1)
        .enable_replay_buffer(true)      // Enable replay buffer
        .replay_buffer_seconds(30)        // Keep last 30 seconds
        .output_path("regular_recording.mp4")
        .build();

    // Initialize with target window
    let recorder = Recorder::new(config)?
        .with_process_name("Your Window Name");

    // Start recording with buffer
    recorder.start_recording()?;
    
    // ... Something interesting happens ...
    
    // Save the replay buffer to a file
    recorder.save_replay("replay_clip.mp4")?;
    
    // Continue recording or stop
    recorder.stop_recording()?;
    
    Ok(())
}
```

See the `examples/replay_buffer.rs` file for a complete example of how to use the replay buffer functionality.

## Configuration
use windows_record::{Recorder, AudioSource, VideoEncoderType, Result};

fn main() -> Result<()> {
    let config = Recorder::builder()
        // Video settings
        .fps(60, 1)
        .input_dimensions(1920, 1080)
        .output_dimensions(1920, 1080)
        .video_bitrate(8000000)
        .video_encoder(VideoEncoderType::Nvenc)
        
        // Audio settings
        .capture_audio(true)
        .capture_microphone(true)
        .audio_source(AudioSource::Desktop)
        .system_volume(Some(0.8))
        .microphone_volume(Some(0.7))
        .microphone_device(Some("Microphone (HD Audio Device)"))
        
        // Output settings
        .output_path("recordings/gameplay.mp4")
        .debug_mode(false)
        
        // Replay buffer
        .enable_replay_buffer(true)
        .replay_buffer_seconds(60)
        
        .build();
        
    // Initialize recorder
    let recorder = Recorder::new(config)?
        .with_process_name("Game Window");
        
    // Start recording
    recorder.start_recording()?;
    
    Ok(())
}

## Limitations

- Windows only
- You must record on the same GPU you wish to capture frames on

## Todo

- Improve 60+ fps recording
- More robust audio sync
- Audio format configurability
