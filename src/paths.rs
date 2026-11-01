//! Where LiveTyper keeps its files. Settings roam with the user; the log and models stay on this PC.

use std::path::PathBuf;

fn known_dir(var: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("LiveTyper")
}

/// `%APPDATA%\LiveTyper`
pub fn roaming_dir() -> PathBuf {
    known_dir("APPDATA")
}

/// `%LOCALAPPDATA%\LiveTyper`
pub fn local_dir() -> PathBuf {
    known_dir("LOCALAPPDATA")
}

pub fn settings_file() -> PathBuf {
    roaming_dir().join("settings.toml")
}

pub fn log_file() -> PathBuf {
    local_dir().join("livetyper.log")
}
