//! The tray icon, its menu, and the UI thread's message loop.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, IDI_APPLICATION, LoadIconW, MF_SEPARATOR,
    MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SW_SHOWNORMAL, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_DESTROY,
    WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};
use windows::core::{HSTRING, Result, w};

use crate::paths;

const WM_TRAY: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;

const CMD_OPEN_SETTINGS: usize = 1;
const CMD_OPEN_LOG: usize = 2;
const CMD_QUIT: usize = 3;

/// Explorer sends this message when it starts again; the icon must then be added again.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Creates the hidden owner window and the tray icon, then runs the message loop until Quit.
pub fn run(startup_warning: Option<&str>) -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("LiveTyperTray");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Relaxed,
        );

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("LiveTyper"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        add_icon(hwnd)?;
        if let Some(text) = startup_warning {
            show_warning(hwnd, text);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            if let Err(e) = add_icon(hwnd) {
                log::error!("tray icon after Explorer restart: {e}");
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

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

fn add_icon(hwnd: HWND) -> Result<()> {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION)? };
    copy_wide(&mut data.szTip, "LiveTyper");
    unsafe { Shell_NotifyIconW(NIM_ADD, &data).ok() }
}

fn show_warning(hwnd: HWND, text: &str) {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_INFO;
    data.dwInfoFlags = NIIF_WARNING;
    copy_wide(&mut data.szInfoTitle, "LiveTyper");
    copy_wide(&mut data.szInfo, text);
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_SETTINGS, w!("Open settings"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_LOG, w!("Open log"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit"));

        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        // Without this, the menu does not close when the user clicks somewhere else.
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
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

        match command.0 as usize {
            CMD_OPEN_SETTINGS => open_file(&paths::settings_file()),
            CMD_OPEN_LOG => open_file(&paths::log_file()),
            CMD_QUIT => {
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

/// Opens a file in its default app, or in Notepad if no app is set for its type (for example `.toml`).
fn open_file(path: &Path) {
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
