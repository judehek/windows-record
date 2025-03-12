# Windows Window Recorder

## Overview

This is a high-performance window recorder built using the `windows` crate in Rust. It leverages DirectX 11, desktop duplication, and Windows Media Foundation to efficiently record windows without any yellow border. The recorder automatically blacks out content when the target window is not in focus.

## Features

- High-performance recording with GPU acceleration (up to 60+ fps)
- Efficient D3D11_USAGE_DEFAULT texture management with resource pooling
- Proper DirectX and Media Foundation resource lifecycle management
- H.264 and H.265 support
- System audio and microphone capture
- Customizable resolution and bitrate settings
- Replay buffer functionality to save recent gameplay/activity
- Debug instrumentation for resource tracking
- Abstracted interface that hides all `unsafe` code from users

## Performance Optimizations

- Fixed-size texture pool to minimize GPU memory allocations
- Explicit reference tracking for DirectX resources
- Proper cleanup of Media Foundation resources
- Thread-safe design for concurrent capture and encoding
- Minimal overhead with zero-copy GPU texture handling
- Efficient resource recycling to prevent memory leaks during long recordings

## Usage Example

```rust
use win_recorder::{Recorder, Result};

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
use win_recorder::{Recorder, Result};

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

## Todo

- Multi-monitor support with automatic display detection
- More robust audio sync
- The wave audio wave format is currently hard coded (device agnostic)
