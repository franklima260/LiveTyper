//! The tray icon and its menu.

use std::path::Path;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, PostMessageW, SW_SHOWNORMAL, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP, WM_NULL,
};
use windows::core::{HSTRING, Result, w};

/// The tray icon sends this message to the owner window.
pub const WM_TRAY: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    PasteLast,
    OpenSettings,
    OpenLog,
    Quit,
}

const COMMANDS: [MenuCommand; 4] = [
    MenuCommand::PasteLast,
    MenuCommand::OpenSettings,
    MenuCommand::OpenLog,
    MenuCommand::Quit,
];

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        ..Default::default()
    }
}

fn copy_wide(dest: &mut [u16], text: &str) {
    let len = dest.len() - 1; // keep the terminating zero
    for (d, s) in dest.iter_mut().zip(text.encode_utf16().take(len)) {
        *d = s;
    }
}

pub fn add_icon(hwnd: HWND, icon: HICON, tip: &str) -> Result<()> {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = icon;
    copy_wide(&mut data.szTip, tip);
    unsafe { Shell_NotifyIconW(NIM_ADD, &data).ok() }
}

pub fn set_icon(hwnd: HWND, icon: HICON, tip: &str) {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_ICON | NIF_TIP;
    data.hIcon = icon;
    copy_wide(&mut data.szTip, tip);
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

pub fn remove_icon(hwnd: HWND) {
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd)) };
}

pub fn show_warning(hwnd: HWND, text: &str) {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_INFO;
    data.dwInfoFlags = NIIF_WARNING;
    copy_wide(&mut data.szInfoTitle, "LiveTyper");
    copy_wide(&mut data.szInfo, text);
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

/// Shows the menu at the mouse and returns the chosen command.
pub fn show_menu(hwnd: HWND, can_paste_last: bool) -> Option<MenuCommand> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let id = |c: MenuCommand| COMMANDS.iter().position(|&x| x == c).unwrap() + 1;
        let paste_flags = if can_paste_last {
            MF_STRING
        } else {
            MF_STRING | MF_GRAYED
        };
        let _ = AppendMenuW(
            menu,
            paste_flags,
            id(MenuCommand::PasteLast),
            w!("Paste Last Dictation"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            id(MenuCommand::OpenSettings),
            w!("Open settings"),
        );
        let _ = AppendMenuW(menu, MF_STRING, id(MenuCommand::OpenLog), w!("Open log"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, id(MenuCommand::Quit), w!("Quit"));

        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        // Without this, the menu does not close when the user clicks somewhere else.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        COMMANDS.get((chosen.0 as usize).checked_sub(1)?).copied()
    }
}

/// Opens a file in its default app, or in Notepad if no app is set for its type (for example `.toml`).
pub fn open_file(path: &Path) {
    let file = HSTRING::from(path);
    unsafe {
        if ShellExecuteW(None, w!("open"), &file, None, None, SW_SHOWNORMAL).0 as isize > 32 {
            return;
        }
        if ShellExecuteW(
            None,
            w!("open"),
            w!("notepad.exe"),
            &file,
            None,
            SW_SHOWNORMAL,
        )
        .0 as isize
            <= 32
        {
            log::error!("cannot open {}", path.display());
        }
    }
}
