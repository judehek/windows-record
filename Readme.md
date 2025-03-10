# Windows Window Recorder

## Overview

This is a high-performance window recorder built using the `windows` crate in Rust. It leverages DirectX 11, desktop duplication, and Windows Media Foundation to efficiently record windows without any yellow border. The recorder automatically blacks out content when the target window is not in focus.

## Features

- High-performance recording with GPU acceleration (up to 60+ fps)
- Efficient D3D11_USAGE_DEFAULT texture management with resource pooling
- Proper DirectX and Media Foundation resource lifecycle management
- H.264 hardware encoding supporting MP4 files
- System audio and microphone capture
- Customizable resolution and bitrate settings
- Hardware encoder selection
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
        .fps(60, 1)
        .input_dimensions(1920, 1080)
        .output_dimensions(1920, 1080)
        .capture_audio(true)
        .output_path("output.mp4")
        .build();

    // Initialize with target window
    let recorder = Recorder::new(config)?
        .with_process_name("Your Game Window Name");

    // Start recording
    recorder.start_recording()?;
    
    // Record for desired duration
    std::thread::sleep(std::time::Duration::from_secs(30));
    
    // Stop recording and clean up resources
    recorder.stop_recording()?;
    
    Ok(())
}
```

## Todo

- Additional codec options beyond H.264
- Multi-monitor support with automatic display detection
- More robust audio sync
- Audio input selection
- Allowing recording system audio vs single window
- The wave audio wave format is currently hard coded
