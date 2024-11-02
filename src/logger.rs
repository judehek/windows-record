use chrono::Local;
use env_logger::{Builder, Target};
use lazy_static::lazy_static;
use log::{error, info, LevelFilter};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct LoggerConfig {
    enabled: bool,
    log_dir: Option<PathBuf>,
    log_level: LevelFilter,
}

impl Default for LoggerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            log_dir: None,
            log_level: LevelFilter::Debug,
        }
    }
}

impl LoggerConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_log_dir<P: AsRef<Path>>(mut self, dir: P) -> Self {
        self.log_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    pub fn with_log_level(mut self, level: LevelFilter) -> Self {
        self.log_level = level;
        self
    }

    pub fn disable_logging(mut self) -> Self {
        self.enabled = false;
        self
    }
}

struct SyncFile(Mutex<File>);

impl Write for SyncFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

struct MultiWriter {
    file: Option<SyncFile>,
    write_to_stdout: bool,
}

impl MultiWriter {
    fn new(file: Option<File>, write_to_stdout: bool) -> Self {
        MultiWriter {
            file: file.map(|f| SyncFile(Mutex::new(f))),
            write_to_stdout,
        }
    }
}

impl Write for MultiWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut max_written = 0;

        if let Some(ref mut file) = self.file {
            if let Ok(written) = file.write(buf) {
                max_written = written;
            }
        }

        if self.write_to_stdout {
            if let Ok(written) = io::stdout().lock().write(buf) {
                max_written = max_written.max(written);
            }
        }

        if max_written > 0 {
            Ok(max_written)
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "Failed to write to any output",
            ))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut failed = false;

        if let Some(ref mut file) = self.file {
            if file.flush().is_err() {
                failed = true;
            }
        }

        if self.write_to_stdout {
            if io::stdout().flush().is_err() {
                failed = true;
            }
        }

        if failed {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "Failed to flush one or more outputs",
            ))
        } else {
            Ok(())
        }
    }
}

lazy_static! {
    static ref LOGGER: Mutex<Option<Mutex<MultiWriter>>> = Mutex::new(None);
}

pub fn setup_logger(config: LoggerConfig) -> io::Result<()> {
    if !config.enabled {
        return Ok(());
    }

    let file = if let Some(log_dir) = config.log_dir {
        // Create the directory if it doesn't exist
        std::fs::create_dir_all(&log_dir)?;

        // Create a timestamp for the log file name
        let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
        let log_file_name = format!("application_log_{}.txt", timestamp);
        let log_file_path = log_dir.join(log_file_name);

        Some(File::create(log_file_path)?)
    } else {
        None
    };

    let multi_writer = MultiWriter::new(file, true);

    // Store the logger globally
    *LOGGER.lock().unwrap() = Some(Mutex::new(multi_writer));

    // Create a custom logger
    let mut builder = Builder::new();
    builder.filter_level(config.log_level);

    // Use our custom MultiWriter
    builder.target(Target::Pipe(Box::new(CustomWrite)));

    builder.format(|buf, record| {
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        writeln!(
            buf,
            "{} [{}] - {}",
            timestamp,
            record.level(),
            record.args()
        )
    });

    // Initialize the logger
    builder.init();

    // Set up the panic hook
    std::panic::set_hook(Box::new(|panic_info| {
        error!("PANIC: {}", panic_info);
        if let Some(location) = panic_info.location() {
            error!(
                "PANIC occurred in file '{}' at line {}",
                location.file(),
                location.line()
            );
        }

        // Ensure all logs are flushed
        if let Some(ref writer) = *LOGGER.lock().unwrap() {
            let _ = writer.lock().unwrap().flush();
        }
    }));

    info!("Logger initialized");
    Ok(())
}

struct CustomWrite;

impl io::Write for CustomWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(ref writer) = *LOGGER.lock().unwrap() {
            let mut writer = writer.lock().unwrap();
            writer.write(buf)
        } else {
            Ok(0)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(ref writer) = *LOGGER.lock().unwrap() {
            let mut writer = writer.lock().unwrap();
            writer.flush()
        } else {
            Ok(())
        }
    }
}
