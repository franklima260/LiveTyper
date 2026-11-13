//! Start with Windows, in one of two ways (the `startup_method` setting):
//! - a value under `HKCU\...\CurrentVersion\Run` that points to this exe;
//! - a shortcut `LiveTyper.lnk` in the user's Startup folder.
//!
//! The app keeps the chosen way and removes the other one, so that Windows never starts it twice.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IPersistFile,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Startup, IShellLinkW, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath, ShellLink,
};
use windows::core::{HSTRING, Interface, PCWSTR, w};

use crate::settings::StartupMethod;

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE_NAME: PCWSTR = w!("LiveTyper");
const SHORTCUT_NAME: &str = "LiveTyper.lnk";

/// Adds the chosen way to start with Windows and removes the other one, or removes both if
/// `enabled` is false.
pub fn apply(enabled: bool, method: StartupMethod) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    // A build in Cargo's target folder must not register itself, or Windows would start a stale dev build.
    if exe
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("target"))
    {
        log::info!("autostart not changed: development build");
        return Ok(());
    }
    match (enabled, method) {
        (true, StartupMethod::Registry) => {
            set_run_value(Some(&exe))?;
            set_startup_shortcut(None)
        }
        (true, StartupMethod::StartupFolder) => {
            set_startup_shortcut(Some(&exe))?;
            set_run_value(None)
        }
        (false, _) => {
            let registry = set_run_value(None);
            let shortcut = set_startup_shortcut(None);
            registry.and(shortcut)
        }
    }
}

/// Writes the Run value for `exe`, or removes it for `None`.
fn set_run_value(exe: Option<&Path>) -> Result<(), String> {
    let result = match exe {
        Some(exe) => {
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
        }
        None => match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME) } {
            e if e == ERROR_FILE_NOT_FOUND => return Ok(()),
            e => e,
        },
    };
    result.ok().map_err(|e| e.message())
}

/// Writes the Startup folder shortcut to `exe`, or removes it for `None`.
fn set_startup_shortcut(exe: Option<&Path>) -> Result<(), String> {
    let link = startup_folder()
        .map_err(|e| e.message())?
        .join(SHORTCUT_NAME);
    let result = match exe {
        Some(exe) => write_shortcut(&link, exe).map_err(|e| e.message()),
        None => match std::fs::remove_file(&link) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        },
    };
    result.map_err(|e| format!("{}: {e}", link.display()))
}

fn startup_folder() -> windows::core::Result<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Startup, KNOWN_FOLDER_FLAG(0), None)?;
        let folder = path.to_string().map(PathBuf::from);
        CoTaskMemFree(Some(path.0 as *const _));
        Ok(folder?)
    }
}

fn write_shortcut(link: &Path, exe: &Path) -> windows::core::Result<()> {
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| {
            let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shell_link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            if let Some(dir) = exe.parent() {
                shell_link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            shell_link.SetDescription(w!("LiveTyper"))?;
            shell_link
                .cast::<IPersistFile>()?
                .Save(&HSTRING::from(link.as_os_str()), true)
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}
