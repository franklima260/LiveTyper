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
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    ERROR_SUCCESS, GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, SetLastError, WPARAM,
};
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
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
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
/// The longest time to wait for the user to release Alt, Win or Shift before the paste keys.
const MODIFIER_WAIT: Duration = Duration::from_millis(300);

/// Pastes on a new thread and calls `done` with the result.
///
/// `done` can come late: reading the user's clipboard can block while a hung app renders it (see
/// `save_clipboard`). Thus the caller must not wait for `done` without a time limit.
pub fn start(text: String, done: impl FnOnce(PasteResult) + Send + 'static) {
    thread::Builder::new()
        .name("paste".into())
        .spawn(move || {
            // Two pastes at the same time would save and restore each other's clipboard.
            // A paste that hangs keeps the flag, so a later paste gives up after a short wait.
            let started = Instant::now();
            while BUSY.swap(true, Ordering::Acquire) {
                if started.elapsed() >= BUSY_WAIT {
                    log::error!("paste: an earlier paste still runs; this paste is skipped");
                    done(PasteResult::NotPasted);
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            let result = paste(&text).unwrap_or_else(|e| {
                log::error!("paste: {e}");
                PasteResult::NotPasted
            });
            BUSY.store(false, Ordering::Release);
            done(result);
        })
        .expect("spawn paste thread");
}

/// Set while a paste runs.
static BUSY: AtomicBool = AtomicBool::new(false);
/// The longest time a paste waits for an earlier paste to end.
const BUSY_WAIT: Duration = Duration::from_secs(3);

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
    let result = transact(hwnd, terminal);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    log::info!("paste: {:?} (terminal: {terminal})", result.as_ref().ok());
    result
}

/// Does the steps of the paste. On each error path, the text goes on the clipboard if possible,
/// so that "Not pasted. Push Ctrl+V." is true.
fn transact(hwnd: HWND, terminal: bool) -> Result<PasteResult> {
    let saved = match save_clipboard(hwnd) {
        Ok(saved) => saved,
        Err(e) => {
            log::error!("paste: cannot save the clipboard: {e}");
            publish_text(hwnd)?;
            return Ok(PasteResult::NotPasted);
        }
    };
    if let Err(e) = publish_delayed(hwnd) {
        log::error!("paste: cannot publish the text: {e}");
        // The publish can fail after EmptyClipboard. If the text also cannot go on the
        // clipboard, put back the user's clipboard, so that it is not empty.
        return match publish_text(hwnd) {
            Ok(()) => Ok(PasteResult::NotPasted),
            Err(e) => {
                let _ = restore_clipboard(hwnd, &saved);
                Err(e)
            }
        };
    }
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
        // If the restore fails, the text stays on the clipboard. The paste is done all the same.
        if unchanged && let Err(e) = restore_clipboard(hwnd, &saved) {
            log::error!("paste: cannot restore the clipboard: {e}");
        }
        Ok(PasteResult::Pasted)
    } else {
        if unchanged {
            // Render the text now, so that it stays on the clipboard after this window closes.
            publish_text(hwnd)?;
        }
        Ok(PasteResult::NotPasted)
    }
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
                if let Err(e) = set_clipboard_bytes(u32::from(CF_UNICODETEXT.0), as_bytes(&text)) {
                    log::error!("paste: cannot render the text: {e}");
                }
                // The render does not change the sequence number (see the test
                // `sequence_number_on_render`), thus `expected_sequence` stays correct.
                TRANSACTION.with_borrow_mut(|t| t.receipt = Some(Instant::now()));
            }
            LRESULT(0)
        }
        WM_RENDERALLFORMATS => {
            unsafe {
                if OpenClipboard(Some(hwnd)).is_ok() {
                    if GetClipboardOwner().is_ok_and(|owner| owner == hwnd) {
                        let text = TRANSACTION.with_borrow(|t| t.text.clone());
                        let _ = set_clipboard_bytes(u32::from(CF_UNICODETEXT.0), as_bytes(&text));
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

/// Copies every clipboard format that is memory.
///
/// GetClipboardData can block for a long time: when the clipboard owner uses delayed rendering
/// and does not answer, Windows waits for it. There is no time limit here, because the paste must
/// save the clipboard before it changes it. The caller of `start` has its own time limit.
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
    let zero = 0u32.to_le_bytes();
    for name in [
        w!("CanIncludeInClipboardHistory"),
        w!("CanUploadToCloudClipboard"),
        w!("ExcludeClipboardContentFromMonitorProcessing"),
    ] {
        let _ = set_clipboard_bytes(unsafe { RegisterClipboardFormatW(name) }, &zero);
    }
}

fn publish_delayed(hwnd: HWND) -> Result<()> {
    open_clipboard(hwnd)?;
    unsafe {
        let result = EmptyClipboard().and_then(|()| {
            set_privacy_formats();
            // A null handle means delayed rendering: Windows asks for the text with WM_RENDERFORMAT.
            // Success also gives null, thus the windows crate gives an Err from GetLastError. Only a
            // nonzero error is a failure. Clear the error first: an earlier call (for example a
            // retry in `open_clipboard`) can leave ERROR_ACCESS_DENIED.
            SetLastError(ERROR_SUCCESS);
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
            set_clipboard_bytes(u32::from(CF_UNICODETEXT.0), as_bytes(&text))
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
                let _ = set_clipboard_bytes(item.format, &item.bytes);
            }
        });
        let _ = CloseClipboard();
        result
    }
}

/// Puts a copy of `bytes` on the open clipboard. If the clipboard does not take the copy, this
/// frees it.
fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> Result<()> {
    let memory = global_copy(bytes)?;
    unsafe {
        SetClipboardData(format, Some(HANDLE(memory.0)))
            .map(|_| ())
            .inspect_err(|_| {
                let _ = GlobalFree(Some(memory));
            })
    }
}

fn global_copy(bytes: &[u8]) -> Result<HGLOBAL> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let data = GlobalLock(memory);
        if data.is_null() {
            let error = Error::from_thread();
            let _ = GlobalFree(Some(memory));
            return Err(error);
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

/// Modifier keys that are down.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct HeldKeys {
    ctrl: bool,
    shift: bool,
    alt: bool,
    win: bool,
}

fn held_keys() -> HeldKeys {
    // The high bit (a negative value) means that the key is down.
    let down = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(i32::from(vk.0)) } < 0;
    HeldKeys {
        ctrl: down(VK_CONTROL),
        shift: down(VK_SHIFT),
        alt: down(VK_MENU),
        win: down(VK_LWIN) || down(VK_RWIN),
    }
}

/// True when a held key changes the meaning of the paste keys. Shift is part of the terminal
/// paste keys, thus it is not a problem for a terminal.
fn must_wait(held: HeldKeys, terminal: bool) -> bool {
    held.alt || held.win || (held.shift && !terminal)
}

/// The key events of the paste, as (key, up). The paste does not push or release a modifier that
/// the user holds, so that Windows does not think that it is up while the user holds it.
fn paste_keys(terminal: bool, held: HeldKeys) -> Vec<(VIRTUAL_KEY, bool)> {
    let mut modifiers = Vec::new();
    if !held.ctrl {
        modifiers.push(VK_CONTROL);
    }
    if terminal && !held.shift {
        modifiers.push(VK_SHIFT);
    }
    let mut keys: Vec<_> = modifiers.iter().map(|&vk| (vk, false)).collect();
    keys.extend([(VK_V, false), (VK_V, true)]);
    keys.extend(modifiers.iter().rev().map(|&vk| (vk, true)));
    keys
}

fn send_paste_keys(terminal: bool) {
    let start = Instant::now();
    let mut held = held_keys();
    while must_wait(held, terminal) && start.elapsed() < MODIFIER_WAIT {
        thread::sleep(Duration::from_millis(10));
        held = held_keys();
    }
    if must_wait(held, terminal) {
        log::warn!("paste keys: modifiers are still down: {held:?}");
    }
    let inputs: Vec<INPUT> = paste_keys(terminal, held)
        .into_iter()
        .map(|(vk, up)| key(vk, up))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::ERROR_ACCESS_DENIED;

    const NONE: HeldKeys = HeldKeys {
        ctrl: false,
        shift: false,
        alt: false,
        win: false,
    };

    #[test]
    fn no_wait_when_no_modifier_is_down() {
        assert!(!must_wait(NONE, false));
        assert!(!must_wait(NONE, true));
    }

    #[test]
    fn wait_for_alt_and_win() {
        for held in [
            HeldKeys { alt: true, ..NONE },
            HeldKeys { win: true, ..NONE },
        ] {
            assert!(must_wait(held, false));
            assert!(must_wait(held, true));
        }
    }

    #[test]
    fn wait_for_shift_except_in_a_terminal() {
        let held = HeldKeys {
            shift: true,
            ..NONE
        };
        assert!(must_wait(held, false));
        assert!(!must_wait(held, true));
    }

    #[test]
    fn ctrl_does_not_cause_a_wait() {
        assert!(!must_wait(HeldKeys { ctrl: true, ..NONE }, false));
    }

    #[test]
    fn ctrl_v() {
        assert_eq!(
            paste_keys(false, NONE),
            [
                (VK_CONTROL, false),
                (VK_V, false),
                (VK_V, true),
                (VK_CONTROL, true)
            ]
        );
    }

    #[test]
    fn ctrl_shift_v_in_a_terminal() {
        assert_eq!(
            paste_keys(true, NONE),
            [
                (VK_CONTROL, false),
                (VK_SHIFT, false),
                (VK_V, false),
                (VK_V, true),
                (VK_SHIFT, true),
                (VK_CONTROL, true)
            ]
        );
    }

    #[test]
    fn held_ctrl_is_not_pushed_or_released() {
        let held = HeldKeys { ctrl: true, ..NONE };
        assert_eq!(paste_keys(false, held), [(VK_V, false), (VK_V, true)]);
        assert_eq!(
            paste_keys(true, held),
            [
                (VK_SHIFT, false),
                (VK_V, false),
                (VK_V, true),
                (VK_SHIFT, true)
            ]
        );
    }

    #[test]
    fn held_shift_in_a_terminal_is_not_pushed_or_released() {
        let held = HeldKeys {
            ctrl: true,
            shift: true,
            ..NONE
        };
        assert_eq!(paste_keys(true, held), [(VK_V, false), (VK_V, true)]);
    }

    /// An experiment on the real clipboard. It puts back the user's clipboard at the end.
    /// Run it with `cargo test sequence_number_on_render -- --ignored --nocapture`.
    ///
    /// It publishes delayed text, then another thread reads the text, which causes
    /// WM_RENDERFORMAT on this thread. Results on Windows 10 22H2:
    /// - A delayed SetClipboardData after a stale ERROR_ACCESS_DENIED gives Err(ACCESS_DENIED).
    ///   Thus `publish_delayed` must clear the error first. (In `publish_delayed`, the calls
    ///   before SetClipboardData can clear the error too, but nothing promises that.)
    /// - The sequence number after the render (clipboard open, and after close) is the same as
    ///   after the publish. Thus WM_RENDERFORMAT does not read the sequence number again.
    #[test]
    #[ignore = "uses the real clipboard"]
    fn sequence_number_on_render() {
        const TEXT: &str = "LiveTyper sequence test";
        let hwnd = create_window().unwrap();
        TRANSACTION.with_borrow_mut(|t| {
            *t = Transaction {
                text: TEXT.encode_utf16().chain([0]).collect(),
                ..Default::default()
            }
        });
        let saved = save_clipboard(hwnd).unwrap();
        let before = unsafe { GetClipboardSequenceNumber() };

        // A successful delayed SetClipboardData does not clear the thread error: without
        // SetLastError(ERROR_SUCCESS), a stale error from an earlier call looks like a failure.
        let stale = unsafe {
            open_clipboard(hwnd).unwrap();
            EmptyClipboard().unwrap();
            SetLastError(ERROR_ACCESS_DENIED);
            let stale = SetClipboardData(u32::from(CF_UNICODETEXT.0), None);
            let _ = CloseClipboard();
            stale
        };
        eprintln!("delayed SetClipboardData after a stale error: {stale:?}");
        assert_eq!(stale.unwrap_err().code(), ERROR_ACCESS_DENIED.to_hresult());

        unsafe { SetLastError(ERROR_ACCESS_DENIED) };
        publish_delayed(hwnd).expect("a stale error must not fail the publish");
        let published = TRANSACTION.with_borrow(|t| t.expected_sequence);

        let reader = thread::spawn(|| unsafe {
            let mut opened = false;
            for _ in 0..50 {
                if OpenClipboard(None).is_ok() {
                    opened = true;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            assert!(opened, "cannot open the clipboard");
            let handle = GetClipboardData(u32::from(CF_UNICODETEXT.0)).unwrap();
            let rendered = GetClipboardSequenceNumber();
            let memory = HGLOBAL(handle.0);
            let data = GlobalLock(memory) as *const u16;
            let len = (0..).take_while(|&i| *data.add(i) != 0).count();
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(data, len));
            let _ = GlobalUnlock(memory);
            let _ = CloseClipboard();
            (text, rendered, GetClipboardSequenceNumber())
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !reader.is_finished() && Instant::now() < deadline {
            pump_messages(hwnd);
            thread::sleep(Duration::from_millis(1));
        }
        let (text, rendered, closed) = reader.join().unwrap();
        let receipt = TRANSACTION.with_borrow(|t| t.receipt.is_some());

        restore_clipboard(hwnd, &saved).unwrap();
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        eprintln!(
            "sequence: before publish {before}, after publish {published}, \
             after render {rendered}, after close {closed}"
        );
        assert_eq!(text, TEXT);
        assert!(receipt);
        assert_eq!(rendered, published);
        assert_eq!(closed, published);
    }
}
