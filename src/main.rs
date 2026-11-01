// No console window in release builds. Debug builds keep it, so the log also shows there.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod autostart;
mod dictation;
mod draft;
mod engine;
mod hotkey;
mod icon;
mod logging;
#[cfg(test)]
mod model_tests;
mod paste;
mod paths;
mod resample;
mod scratch_pad;
mod settings;
mod terminals;
mod tray;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::core::w;

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
fn single_instance() -> Option<HANDLE> {
    let handle = unsafe { CreateMutexW(None, true, w!("Local\\LiveTyper.SingleInstance")) }.ok()?;
    (unsafe { GetLastError() } != ERROR_ALREADY_EXISTS).then_some(handle)
}
