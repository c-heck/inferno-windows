//! Diagnostic log at %LOCALAPPDATA%\InfernoArmAsio\inferno.log (previous run kept as inferno.log.old).
use std::{
    fs::{self, File},
    io::Write,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 32 * 1024 * 1024;

struct FileLogger {
    file: Mutex<(File, u64)>,
    level: log::LevelFilter,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let line = format!(
            "{}.{:06} {:5} {}: {}\n",
            now.as_secs(),
            now.subsec_micros(),
            record.level(),
            record.target(),
            record.args()
        );
        if let Ok(mut guard) = self.file.lock() {
            let (file, written) = &mut *guard;
            if *written < MAX_BYTES && file.write_all(line.as_bytes()).is_ok() {
                *written += line.len() as u64;
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock() {
            let _ = guard.0.flush();
        }
    }
}

/// Installs the file logger once per process; later calls are no-ops.
pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Some(base) = std::env::var_os("LOCALAPPDATA") else {
            return;
        };
        let dir = std::path::PathBuf::from(base).join("InfernoArmAsio");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("inferno.log");
        let _ = fs::rename(&path, dir.join("inferno.log.old"));
        let Ok(file) = File::create(&path) else {
            return;
        };
        let level = match std::env::var("INFERNO_ASIO_LOG").as_deref() {
            Ok("debug") => log::LevelFilter::Debug,
            Ok("trace") => log::LevelFilter::Trace,
            Ok("warn") => log::LevelFilter::Warn,
            _ => log::LevelFilter::Info,
        };
        let logger = Box::leak(Box::new(FileLogger {
            file: Mutex::new((file, 0)),
            level,
        }));
        if log::set_logger(logger).is_ok() {
            log::set_max_level(level);
            log::info!("Inferno ASIO log started (level {level})");
        }
    });
}
