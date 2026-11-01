// No console window in release builds. Debug builds keep it, so the log also shows there.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod logging;
mod paths;
mod settings;
mod tray;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

fn main() {
    let Some(_instance) = single_instance() else {
        return;
    };
    logging::init();
    log::info!("LiveTyper {} started", env!("CARGO_PKG_VERSION"));

    let (settings, warning) = settings::load_or_create();
    autostart::apply(settings.start_with_windows);

    if let Err(e) = tray::run(warning.as_deref()) {
        log::error!("tray: {e}");
    }
    log::info!("LiveTyper stopped");
}

/// Returns a handle that marks this process as the running LiveTyper, or `None` if one is already running.
/// The handle must stay open until the process ends.
fn single_instance() -> Option<HANDLE> {
    let handle = unsafe { CreateMutexW(None, true, w!("Local\\LiveTyper.SingleInstance")) }.ok()?;
    (unsafe { GetLastError() } != ERROR_ALREADY_EXISTS).then_some(handle)
}
