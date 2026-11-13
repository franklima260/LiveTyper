// No console window in release builds. Debug builds keep it, so the log also shows there.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
mod audio;
#[cfg(windows)]
mod autostart;
mod dictation;
mod draft;
mod engine;
#[cfg(windows)]
mod hotkey;
#[cfg(target_os = "linux")]
#[path = "linux_hotkey.rs"]
mod hotkey;
#[cfg(windows)]
mod icon;
#[cfg(target_os = "linux")]
mod linux;
mod logging;
mod meter;
#[cfg(test)]
mod model_tests;
#[cfg(windows)]
mod paste;
mod paths;
mod resample;
#[cfg(windows)]
mod scratch_pad;
mod settings;
#[cfg(windows)]
mod terminals;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
#[cfg(windows)]
use windows::Win32::System::Threading::CreateMutexW;
#[cfg(windows)]
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
#[cfg(windows)]
use windows::core::w;

#[cfg(windows)]
fn main() {
    let Some(_instance) = single_instance() else {
        return;
    };
    // Sharp text on high-DPI screens; the Scratch Pad scales itself.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    logging::init();
    log::info!("LiveTyper {} started", env!("CARGO_PKG_VERSION"));

    let (settings, warning) = settings::load_or_create();
    autostart::apply(settings.start_with_windows);

    if let Err(e) = app::run(settings, warning) {
        log::error!("app: {e}");
    }
    log::info!("LiveTyper stopped");
}

/// Returns a handle that marks this process as the running LiveTyper, or `None` if one is already running.
/// The handle must stay open until the process ends.
#[cfg(windows)]
fn single_instance() -> Option<HANDLE> {
    let handle = unsafe { CreateMutexW(None, true, w!("Local\\LiveTyper.SingleInstance")) }.ok()?;
    (unsafe { GetLastError() } != ERROR_ALREADY_EXISTS).then_some(handle)
}

#[cfg(target_os = "linux")]
fn main() {
    logging::init();
    log::info!("LiveTyper {} started", env!("CARGO_PKG_VERSION"));
    let (settings, warning) = settings::load_or_create();
    if let Err(error) = linux::run(settings, warning) {
        log::error!("app: {error}");
        eprintln!("LiveTyper: {error}");
        std::process::exit(1);
    }
}
