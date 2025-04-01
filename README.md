# Windows Record

Windows Record is an efficient Rust library designed for seamless window recording. Using the Windows Desktop Duplication API, it captures window content directly without the yellow recording border caused by the WGC API. The library prioritizes performance and ease-of-use.

## Key Features

- No yellow border during capture
- Simple, builder pattern
- Built-in audio support
- Replay buffering (similar to ShadowPlay)
- Fully configurable

## Installation
```
cargo add windows-record
```

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

The recorder offers extensive configuration options through its builder pattern:

### Minimal Configuration

You can create a minimal configuration like this:

```rust
let config = Recorder::builder()
    .output_path("output.mp4")
    .build();
```

This will use all default settings and just specify the output file.

### Video Settings
- `fps(num, den)` - Set frame rate (default: 30/1)
- `input_dimensions(width, height)` - Set input resolution (default: 1920x1080)
- `output_dimensions(width, height)` - Set output resolution (default: 1920x1080)
- `video_bitrate(bitrate)` - Set video bitrate in bits per second (default: 5,000,000)
- `video_encoder(encoder)` - Set video encoder (default: H264, options: H264, HEVC)

### Audio Settings
- `capture_audio(enabled)` - Enable/disable system audio capture (default: true)
- `capture_microphone(enabled)` - Enable/disable microphone capture (default: false)
- `microphone_volume(volume)` - Set microphone volume (0.0-1.0, default: None)
- `system_volume(volume)` - Set system audio volume (0.0-1.0, default: None)
- `audio_source(source)` - Set audio source (default: ActiveWindow, options: Desktop, ActiveWindow)
- `microphone_device(device_name)` - Set specific microphone device (default: None)

### Output Settings
- `output_path(path)` - Set recording output path (default: current directory)
- `debug_mode(enabled)` - Enable debug logging and diagnostics (default: false)

### Replay Buffer Settings
- `enable_replay_buffer(enabled)` - Enable replay buffer feature (default: false)
- `replay_buffer_seconds(seconds)` - Set replay buffer duration in seconds (default: 30)

## Limitations

- Windows only
- You must record on the same GPU you wish to capture frames on

## Todo

- More robust audio sync
- Audio format configurability

- cursor recording (currently having issues with gdi compatability and nv12 output type)