use std::fs::File;
use std::io::{self, Write};
use std::sync::Mutex;
use chrono::Local;
use env_logger::{Builder, Target};
use lazy_static::lazy_static;
use log::{error, info, LevelFilter};

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
    file: SyncFile,
}

impl MultiWriter {
    fn new(file: File) -> Self {
        MultiWriter {
            file: SyncFile(Mutex::new(file)),
        }
    }
}

impl Write for MultiWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let file_result = self.file.write(buf);
        let stdout_result = io::stdout().lock().write(buf);
        
        match (file_result, stdout_result) {
            (Ok(file_len), Ok(stdout_len)) => Ok(file_len.max(stdout_len)),
            (Ok(len), Err(_)) | (Err(_), Ok(len)) => Ok(len),
            (Err(e), Err(_)) => Err(e),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()?;
        io::stdout().flush()
    }
}

lazy_static! {
    static ref LOGGER: Mutex<Option<Mutex<MultiWriter>>> = Mutex::new(None);
}

pub fn setup_logger() -> io::Result<()> {
    // Create a timestamp for the log file name
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let log_file_name = format!("application_log_{}.txt", timestamp);

    // Open the log file
    let file = File::create(log_file_name)?;
    let multi_writer = MultiWriter::new(file);

    // Store the logger globally
    *LOGGER.lock().unwrap() = Some(Mutex::new(multi_writer));

    // Create a custom logger
    let mut builder = Builder::new();
    builder.filter_level(LevelFilter::Debug);  // Set to Debug level
    
    // Use our custom MultiWriter
    builder.target(Target::Pipe(Box::new(CustomWrite)));

    builder.format(|buf, record| {
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        writeln!(buf, "{} [{}] - {}", timestamp, record.level(), record.args())
    });

    // Initialize the logger
    builder.init();

    // Set up the panic hook
    std::panic::set_hook(Box::new(|panic_info| {
        error!("PANIC: {}", panic_info);
        if let Some(location) = panic_info.location() {
            error!("PANIC occurred in file '{}' at line {}", location.file(), location.line());
        }
        
        // Ensure all logs are flushed
        if let Some(ref writer) = *LOGGER.lock().unwrap() {
            let _ = writer.lock().unwrap().flush();
        }
    }));

    info!("Logger initialized with output to file and stdout");
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