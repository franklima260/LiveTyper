//! Start with Windows: a value under `HKCU\...\CurrentVersion\Run` that points to this exe.

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, w};

const RUN_KEY: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE_NAME: windows::core::PCWSTR = w!("LiveTyper");

/// Adds or removes the Run value to match the setting.
pub fn apply(enabled: bool) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // A build in Cargo's target folder must not register itself, or Windows would start a stale dev build.
    if exe
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("target"))
    {
        log::info!("autostart not changed: development build");
        return;
    }

    let result = if enabled {
        let command: Vec<u16> = format!("\"{}\"", exe.display())
            .encode_utf16()
            .chain([0])
            .collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE_NAME,
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * 2) as u32,
            )
        }
    } else {
        match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME) } {
            e if e == ERROR_FILE_NOT_FOUND => return,
            e => e,
        }
    };
    if result.is_err() {
        log::error!(
            "autostart (enabled = {enabled}): {}",
            HSTRING::from(result.to_hresult().message())
        );
    }
}
