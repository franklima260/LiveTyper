//! A small append-only file logger behind the `log` facade.
//!
//! The log holds errors, events and timings. It must never hold dictated text.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

use log::{LevelFilter, Log, Metadata, Record};
#[cfg(windows)]
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::paths;

/// At start-up, a log that is larger than this moves to `livetyper.old.log`.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

struct FileLogger {
    file: Mutex<File>,
}

impl Log for FileLogger {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        #[cfg(windows)]
        let line = windows_line(record);
        #[cfg(target_os = "linux")]
        let line = linux_line(record);
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

#[cfg(windows)]
fn windows_line(record: &Record) -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {:<5} {}\r\n",
        t.wYear,
        t.wMonth,
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond,
        t.wMilliseconds,
        record.level(),
        record.args()
    )
}

#[cfg(target_os = "linux")]
fn linux_line(record: &Record) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "{}.{:03} {:<5} {}\n",
        now.as_secs(),
        now.subsec_millis(),
        record.level(),
        record.args()
    )
}

/// Starts file logging if the platform log directory is writable.
pub fn init() {
    let path = paths::log_file();
    let _ = fs::create_dir_all(paths::local_dir());
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = fs::rename(&path, path.with_extension("old.log"));
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let logger = Box::leak(Box::new(FileLogger {
        file: Mutex::new(file),
    }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}
