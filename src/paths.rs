//! Where LiveTyper keeps its files. Settings roam with the user; the log and models stay on this PC.

use std::path::PathBuf;

#[cfg(windows)]
fn known_dir(var: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("LiveTyper")
}

/// `%APPDATA%\LiveTyper`
#[cfg(windows)]
pub fn roaming_dir() -> PathBuf {
    known_dir("APPDATA")
}

/// `%LOCALAPPDATA%\LiveTyper`
#[cfg(windows)]
pub fn local_dir() -> PathBuf {
    known_dir("LOCALAPPDATA")
}

pub fn settings_file() -> PathBuf {
    roaming_dir().join("settings.toml")
}

pub fn log_file() -> PathBuf {
    local_dir().join("livetyper.log")
}

#[cfg(target_os = "linux")]
fn xdg_dir(variable: &str, home_relative: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(home_relative)))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("livetyper")
}

#[cfg(target_os = "linux")]
pub fn roaming_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

#[cfg(target_os = "linux")]
pub fn local_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}
