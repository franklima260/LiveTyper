//! Pastes text into the Target Text Box through the clipboard, and restores the user's clipboard.
//!
//! The algorithm is a port of Handy's `src-tauri/src/paste_tx/windows.rs` (MIT, cjpais/Handy):
//! 1. Save a copy of every clipboard format.
//! 2. Put the text on the clipboard with delayed rendering, excluded from clipboard history.
//! 3. Send Ctrl+V (Ctrl+Shift+V for terminals).
//! 4. The target asks for the text (`WM_RENDERFORMAT`): this is the receipt.
//! 5. 200 ms after the last receipt, restore the saved clipboard, but only if nobody changed it.
//!
//! No receipt in 1 s means that the paste failed (for example, an administrator window): the text
//! then stays on the clipboard, so that the user can push Ctrl+V.

use std::cell::RefCell;
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardOwner,
    GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::{
    CF_BITMAP, CF_DSPBITMAP, CF_DSPENHMETAFILE, CF_DSPMETAFILEPICT, CF_DSPTEXT, CF_ENHMETAFILE,
    CF_METAFILEPICT, CF_OWNERDISPLAY, CF_PALETTE, CF_UNICODETEXT,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_CONTROL, VK_SHIFT, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClassNameW,
    GetForegroundWindow, GetWindowThreadProcessId, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_DESTROYCLIPBOARD,
    WM_RENDERALLFORMATS, WM_RENDERFORMAT, WNDCLASSW,
};
use windows::core::{Error, PCWSTR, Result, w};

use crate::terminals;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteResult {
    Pasted,
    /// The target did not take the text. The text stays on the clipboard.
    NotPasted,
}

const QUIET_PERIOD: Duration = Duration::from_millis(200);
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_FORMAT_BYTES: usize = 64 * 1024 * 1024;

/// Pastes on a new thread and calls `done` with the result.
pub fn start(text: String, done: impl FnOnce(PasteResult) + Send + 'static) {
    thread::Builder::new()
        .name("paste".into())
        .spawn(move || {
            let result = paste(&text).unwrap_or_else(|e| {
                log::error!("paste: {e}");
                PasteResult::NotPasted
            });
            done(result);
        })
        .expect("spawn paste thread");
}

/// State that the window procedure shares with the paste loop (same thread).
#[derive(Default)]
struct Transaction {
    text: Vec<u16>,
    receipt: Option<Instant>,
    ownership_lost: bool,
    expected_sequence: u32,
}

thread_local! {
    static TRANSACTION: RefCell<Transaction> = RefCell::default();
}

struct SavedFormat {
    format: u32,
    bytes: Vec<u8>,
}

fn paste(text: &str) -> Result<PasteResult> {
    let target = unsafe { GetForegroundWindow() };
    let terminal = terminals::is_terminal(&class_name(target), &exe_path(target));
    let hwnd = create_window()?;
    TRANSACTION.with_borrow_mut(|t| {
        *t = Transaction {
            text: text.encode_utf16().chain([0]).collect(),
            ..Default::default()
        }
    });

    let result = (|| {
        let saved = save_clipboard(hwnd)?;
        publish_delayed(hwnd)?;
        send_paste_keys(terminal);
        let sent = Instant::now();

        let pasted = loop {
            pump_messages(hwnd);
            let (receipt, lost) = TRANSACTION.with_borrow(|t| (t.receipt, t.ownership_lost));
            if lost {
                break receipt.is_some();
            }
            match receipt {
                Some(at) if at.elapsed() >= QUIET_PERIOD => break true,
                None if sent.elapsed() >= RECEIPT_TIMEOUT => break false,
                _ => thread::sleep(Duration::from_millis(5)),
            }
        };

        let (lost, expected) = TRANSACTION.with_borrow(|t| (t.ownership_lost, t.expected_sequence));
        let unchanged = !lost && unsafe { GetClipboardSequenceNumber() } == expected;
        if pasted {
            if unchanged {
                restore_clipboard(hwnd, &saved)?;
            }
            Ok(PasteResult::Pasted)
        } else {
            if unchanged {
                // Render the text now, so that it stays on the clipboard after this window closes.
                publish_text(hwnd)?;
            }
            Ok(PasteResult::NotPasted)
        }
    })();
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    log::info!("paste: {:?} (terminal: {terminal})", result.as_ref().ok());
    result
}

fn create_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("LiveTyperPaste");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        // Registering a second time fails harmlessly.
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_RENDERFORMAT => {
            // The clipboard is already open by the app that asks for the text.
            if wparam.0 as u32 == u32::from(CF_UNICODETEXT.0) {
                let text = TRANSACTION.with_borrow(|t| t.text.clone());
                if let Ok(memory) = global_copy(as_bytes(&text)) {
                    unsafe {
                        let _ =
                            SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(memory.0)));
                    }
                }
                TRANSACTION.with_borrow_mut(|t| {
                    t.receipt = Some(Instant::now());
                    t.expected_sequence = unsafe { GetClipboardSequenceNumber() };
                });
            }
            LRESULT(0)
        }
        WM_RENDERALLFORMATS => {
            unsafe {
                if OpenClipboard(Some(hwnd)).is_ok() {
                    if GetClipboardOwner().is_ok_and(|owner| owner == hwnd) {
                        let text = TRANSACTION.with_borrow(|t| t.text.clone());
                        if let Ok(memory) = global_copy(as_bytes(&text)) {
                            let _ = SetClipboardData(
                                u32::from(CF_UNICODETEXT.0),
                                Some(HANDLE(memory.0)),
                            );
                        }
                    }
                    let _ = CloseClipboard();
                }
            }
            LRESULT(0)
        }
        WM_DESTROYCLIPBOARD => {
            TRANSACTION.with_borrow_mut(|t| t.ownership_lost = true);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn pump_messages(hwnd: HWND) {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Another app can hold the clipboard open for a short time.
fn open_clipboard(hwnd: HWND) -> Result<()> {
    let mut last = Error::empty();
    for _ in 0..20 {
        match unsafe { OpenClipboard(Some(hwnd)) } {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(last)
}

/// Formats that are GDI handles or display-only, which cannot be copied as memory.
fn skip_format(format: u32) -> bool {
    [
        CF_BITMAP,
        CF_METAFILEPICT,
        CF_PALETTE,
        CF_ENHMETAFILE,
        CF_OWNERDISPLAY,
        CF_DSPTEXT,
        CF_DSPBITMAP,
        CF_DSPMETAFILEPICT,
        CF_DSPENHMETAFILE,
    ]
    .iter()
    .any(|f| u32::from(f.0) == format)
        || (0x200..=0x3FF).contains(&format) // private and GDI-object ranges
}

fn save_clipboard(hwnd: HWND) -> Result<Vec<SavedFormat>> {
    open_clipboard(hwnd)?;
    let mut saved = Vec::new();
    unsafe {
        let mut format = EnumClipboardFormats(0);
        while format != 0 {
            if !skip_format(format)
                && let Ok(handle) = GetClipboardData(format)
            {
                let memory = HGLOBAL(handle.0);
                let size = GlobalSize(memory);
                let data = GlobalLock(memory);
                if !data.is_null() && size > 0 && size <= MAX_FORMAT_BYTES {
                    let bytes = std::slice::from_raw_parts(data as *const u8, size).to_vec();
                    saved.push(SavedFormat { format, bytes });
                }
                if !data.is_null() {
                    let _ = GlobalUnlock(memory);
                }
            }
            format = EnumClipboardFormats(format);
        }
        let _ = CloseClipboard();
    }
    Ok(saved)
}

/// Marks the clipboard content as private: not in Win+V history, not synced to the cloud.
fn set_privacy_formats() {
    unsafe {
        let zero = 0u32.to_le_bytes();
        for name in [
            w!("CanIncludeInClipboardHistory"),
            w!("CanUploadToCloudClipboard"),
        ] {
            if let Ok(memory) = global_copy(&zero) {
                let _ = SetClipboardData(RegisterClipboardFormatW(name), Some(HANDLE(memory.0)));
            }
        }
        if let Ok(memory) = global_copy(&zero) {
            let format =
                RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
            let _ = SetClipboardData(format, Some(HANDLE(memory.0)));
        }
    }
}

fn publish_delayed(hwnd: HWND) -> Result<()> {
    open_clipboard(hwnd)?;
    unsafe {
        let result = EmptyClipboard().and_then(|()| {
            set_privacy_formats();
            // A null handle means delayed rendering: Windows asks for the text with WM_RENDERFORMAT.
            match SetClipboardData(u32::from(CF_UNICODETEXT.0), None) {
                Ok(_) => Ok(()),
                Err(e) if e.code().is_ok() => Ok(()),
                Err(e) => Err(e),
            }
        });
        let _ = CloseClipboard();
        result?;
    }
    TRANSACTION.with_borrow_mut(|t| t.expected_sequence = unsafe { GetClipboardSequenceNumber() });
    Ok(())
}

fn publish_text(hwnd: HWND) -> Result<()> {
    let text = TRANSACTION.with_borrow(|t| t.text.clone());
    open_clipboard(hwnd)?;
    unsafe {
        let result = EmptyClipboard().and_then(|()| {
            set_privacy_formats();
            let memory = global_copy(as_bytes(&text))?;
            SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(memory.0))).map(|_| ())
        });
        let _ = CloseClipboard();
        result
    }
}

fn restore_clipboard(hwnd: HWND, saved: &[SavedFormat]) -> Result<()> {
    open_clipboard(hwnd)?;
    unsafe {
        let result = EmptyClipboard().map(|()| {
            for item in saved {
                if let Ok(memory) = global_copy(&item.bytes) {
                    let _ = SetClipboardData(item.format, Some(HANDLE(memory.0)));
                }
            }
        });
        let _ = CloseClipboard();
        result
    }
}

fn global_copy(bytes: &[u8]) -> Result<HGLOBAL> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let data = GlobalLock(memory);
        if data.is_null() {
            return Err(Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data as *mut u8, bytes.len());
        let _ = GlobalUnlock(memory);
        Ok(memory)
    }
}

fn as_bytes(text: &[u16]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(text.as_ptr() as *const u8, text.len() * 2) }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                ..Default::default()
            },
        },
    }
}

fn send_paste_keys(terminal: bool) {
    let mut inputs = vec![key(VK_CONTROL, false)];
    if terminal {
        inputs.push(key(VK_SHIFT, false));
    }
    inputs.extend([key(VK_V, false), key(VK_V, true)]);
    if terminal {
        inputs.push(key(VK_SHIFT, true));
    }
    inputs.push(key(VK_CONTROL, true));
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        log::error!("paste keys: {} of {} sent", sent, inputs.len());
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..len.max(0) as usize])
}

fn exe_path(hwnd: HWND) -> String {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return String::new();
    };
    let mut buffer = [0u16; 1024];
    let mut len = buffer.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(process);
    }
    match ok {
        Ok(()) => String::from_utf16_lossy(&buffer[..len as usize]),
        Err(_) => String::new(),
    }
}
