//! The Talk Key and the Escape key, through a low-level keyboard hook.
//!
//! The hook sees each key before other apps. Thus the Talk Key works in any key order (for example
//! Ctrl, Space, then Shift), and another app with a similar shortcut does not get it first. The
//! hook removes the Talk Key's main key from the input while LiveTyper uses it, and also Escape
//! during a Dictation. Modifier keys always go on to the other apps.
//!
//! The hook runs on its own thread with a message loop. It must return fast, so it only posts events.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use global_hotkey::hotkey::{HotKey, Modifiers};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    SetWindowsHookExW, TranslateMessage, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    TalkDown,
    TalkUp,
    Escape,
}

pub struct Hotkeys;

/// True during a Dictation: then Escape belongs to LiveTyper.
static ESCAPE_ON: AtomicBool = AtomicBool::new(false);
type Handler = Box<dyn Fn(KeyEvent) + Send + Sync>;
static HANDLER: OnceLock<Handler> = OnceLock::new();

thread_local! {
    static KEYBOARD: RefCell<Option<Keyboard>> = const { RefCell::new(None) };
}

impl Hotkeys {
    /// `on_event` runs on the hook thread. On an error, returns a message for the user (the
    /// details go to the log).
    pub fn new(
        talk_key: &str,
        on_event: impl Fn(KeyEvent) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let chord = Chord::parse(talk_key)?;
        if HANDLER.set(Box::new(on_event)).is_err() {
            return Err("The Talk Key is already set up.".into());
        }
        let (result, started) = mpsc::channel();
        thread::Builder::new()
            .name("keys".into())
            .spawn(move || {
                KEYBOARD.with_borrow_mut(|k| *k = Some(Keyboard::new(chord)));
                let hook = unsafe {
                    GetModuleHandleW(None).and_then(|module| {
                        SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), Some(module.into()), 0)
                    })
                };
                let _ = result.send(hook.map(|_| ()).map_err(|e| e.to_string()));
                let mut msg = MSG::default();
                unsafe {
                    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        match started.recv().map_err(|e| e.to_string()).and_then(|r| r) {
            Ok(()) => Ok(Hotkeys),
            Err(e) => {
                log::error!("keyboard hook: {e}");
                Err(format!(
                    "The Talk Key {talk_key} does not work: the keyboard hook cannot start."
                ))
            }
        }
    }

    /// Escape belongs to LiveTyper only during a Dictation.
    pub fn set_escape(&mut self, on: bool) {
        ESCAPE_ON.store(on, Ordering::Relaxed);
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up = matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP);
        // Keys that apps send (for example the paste keys) do not change what the user holds.
        let injected = info.flags.0 & LLKHF_INJECTED.0 != 0;
        if (down || up) && !injected {
            let escape_on = ESCAPE_ON.load(Ordering::Relaxed);
            let result = KEYBOARD.with_borrow_mut(|k| {
                k.as_mut()
                    .map(|k| k.key(info.vkCode as u16, down, escape_on))
            });
            if let Some((event, swallow)) = result {
                if let (Some(event), Some(handler)) = (event, HANDLER.get()) {
                    handler(event);
                }
                if swallow {
                    return LRESULT(1);
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

const VK_ESCAPE: u16 = 0x1B;

/// The modifier keys, each as a bit.
const CTRL: u8 = 1;
const SHIFT: u8 = 2;
const ALT: u8 = 4;
const WIN: u8 = 8;

/// The Talk Key: the main key and the exact set of modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    key: u16,
    modifiers: u8,
}

impl Chord {
    pub fn parse(text: &str) -> Result<Chord, String> {
        let hotkey: HotKey = text
            .parse()
            .map_err(|_| format!("\"{text}\" is not a key combination."))?;
        let name = format!("{:?}", hotkey.key);
        let key = virtual_key(&name)
            .ok_or_else(|| format!("The key {name} in \"{text}\" cannot be the Talk Key."))?;
        let mods = hotkey.mods;
        // `global-hotkey` can give the Windows key as SUPER (an old name) or as META.
        #[allow(deprecated)]
        let win = Modifiers::SUPER | Modifiers::META;
        let mut modifiers = 0;
        for (flag, bit) in [
            (Modifiers::CONTROL, CTRL),
            (Modifiers::SHIFT, SHIFT),
            (Modifiers::ALT, ALT),
            (win, WIN),
        ] {
            if mods.intersects(flag) {
                modifiers |= bit;
            }
        }
        Ok(Chord { key, modifiers })
    }
}

/// The modifier bit of a virtual key, if it is a modifier (left, right, or either).
fn modifier_bit(vk: u16) -> Option<u8> {
    match vk {
        0x11 | 0xA2 | 0xA3 => Some(CTRL),
        0x10 | 0xA0 | 0xA1 => Some(SHIFT),
        0x12 | 0xA4 | 0xA5 => Some(ALT),
        0x5B | 0x5C => Some(WIN),
        _ => None,
    }
}

/// The virtual-key code of a key name as `global-hotkey` gives it ("KeyA", "Digit1", "F9", "Space").
fn virtual_key(name: &str) -> Option<u16> {
    let single = |rest: &str, first: char, base: u16| -> Option<u16> {
        let mut chars = rest.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        let offset = (c as u32).checked_sub(first as u32)? as u16;
        Some(base + offset)
    };
    if let Some(rest) = name.strip_prefix("Key") {
        return single(rest, 'A', 0x41).filter(|&vk| vk <= 0x5A);
    }
    if let Some(rest) = name.strip_prefix("Digit") {
        return single(rest, '0', 0x30).filter(|&vk| vk <= 0x39);
    }
    if let Some(vk) = name
        .strip_prefix("Numpad")
        .and_then(|rest| single(rest, '0', 0x60))
        .filter(|&vk| vk <= 0x69)
    {
        return Some(vk);
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1);
    }
    Some(match name {
        "Space" => 0x20,
        "Enter" => 0x0D,
        "Tab" => 0x09,
        "Backspace" => 0x08,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "Pause" => 0x13,
        "ScrollLock" => 0x91,
        "Backquote" => 0xC0,
        "Minus" => 0xBD,
        "Equal" => 0xBB,
        "BracketLeft" => 0xDB,
        "BracketRight" => 0xDD,
        "Backslash" => 0xDC,
        "Semicolon" => 0xBA,
        "Quote" => 0xDE,
        "Comma" => 0xBC,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        "NumpadAdd" => 0x6B,
        "NumpadSubtract" => 0x6D,
        "NumpadMultiply" => 0x6A,
        "NumpadDivide" => 0x6F,
        "NumpadDecimal" => 0x6E,
        _ => return None,
    })
}

/// What the user holds, from the key events. Pure, so that the tests can use it.
struct Keyboard {
    chord: Chord,
    /// The modifiers that are down, as bits. Left and right keys count separately.
    left_right: [bool; 8],
    key_down: bool,
    /// The Talk Key is down: LiveTyper sent TalkDown and waits for the main key to go up.
    active: bool,
    /// The main key's down event did not go on to the other apps, so its up event must not either.
    key_hidden: bool,
    escape_hidden: bool,
}

impl Keyboard {
    fn new(chord: Chord) -> Self {
        Keyboard {
            chord,
            left_right: [false; 8],
            key_down: false,
            active: false,
            key_hidden: false,
            escape_hidden: false,
        }
    }

    fn modifiers(&self) -> u8 {
        let mut bits = 0;
        for (i, &down) in self.left_right.iter().enumerate() {
            if down {
                bits |= 1 << (i / 2);
            }
        }
        bits
    }

    /// The index in `left_right` of a modifier key.
    fn side(vk: u16) -> Option<usize> {
        let bit = modifier_bit(vk)?;
        let right = matches!(vk, 0xA1 | 0xA3 | 0xA5 | 0x5C);
        Some(bit.trailing_zeros() as usize * 2 + usize::from(right))
    }

    /// Handles one key event of the user. Returns the event for LiveTyper, and true if the key
    /// event must not go on to the other apps.
    fn key(&mut self, vk: u16, down: bool, escape_on: bool) -> (Option<KeyEvent>, bool) {
        if vk == VK_ESCAPE && vk != self.chord.key {
            if down && escape_on {
                self.escape_hidden = true;
                return (Some(KeyEvent::Escape), true);
            }
            if !down && self.escape_hidden {
                self.escape_hidden = false;
                return (None, true);
            }
            return (None, false);
        }
        if let Some(side) = Self::side(vk) {
            self.left_right[side] = down;
            return (self.check_start(false), false);
        }
        if vk != self.chord.key {
            return (None, false);
        }
        if down {
            let repeat = self.key_down;
            self.key_down = true;
            if self.active {
                // A key repeat while the user holds the Talk Key.
                return (None, true);
            }
            if repeat {
                // The main key was already down (and went on to the other apps).
                return (self.check_start(false), false);
            }
            let event = self.check_start(true);
            (event, event.is_some())
        } else {
            self.key_down = false;
            let hidden = self.key_hidden;
            self.key_hidden = false;
            if self.active {
                self.active = false;
                return (Some(KeyEvent::TalkUp), hidden);
            }
            (None, hidden)
        }
    }

    /// Starts the Talk Key when the main key and exactly the chord's modifiers are down.
    /// `by_key` is true when the main key's down event completes it.
    fn check_start(&mut self, by_key: bool) -> Option<KeyEvent> {
        if self.active || !self.key_down || self.modifiers() != self.chord.modifiers {
            return None;
        }
        self.active = true;
        self.key_hidden = by_key;
        Some(KeyEvent::TalkDown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: u16 = 0x20;
    const LCTRL: u16 = 0xA2;
    const LSHIFT: u16 = 0xA0;
    const RSHIFT: u16 = 0xA1;
    const LALT: u16 = 0xA4;

    fn keyboard() -> Keyboard {
        Keyboard::new(Chord::parse("Ctrl+Shift+Space").unwrap())
    }

    #[test]
    fn talk_keys_parse() {
        assert_eq!(
            Chord::parse("Ctrl+Shift+Space"),
            Ok(Chord {
                key: SPACE,
                modifiers: CTRL | SHIFT
            })
        );
        assert_eq!(Chord::parse("F9").map(|c| c.key), Ok(0x78));
        assert_eq!(
            Chord::parse("Ctrl+Alt+D").map(|c| (c.key, c.modifiers)),
            Ok((0x44, CTRL | ALT))
        );
        assert_eq!(Chord::parse("Ctrl+1").map(|c| c.key), Ok(0x31));
        assert!(Chord::parse("Ctrl+Nothing").is_err());
    }

    #[test]
    fn modifiers_first_then_the_main_key() {
        let mut k = keyboard();
        assert_eq!(k.key(LCTRL, true, false), (None, false));
        assert_eq!(k.key(LSHIFT, true, false), (None, false));
        // The main key starts the Talk Key and does not go on to the other apps.
        assert_eq!(k.key(SPACE, true, false), (Some(KeyEvent::TalkDown), true));
        assert_eq!(k.key(SPACE, true, false), (None, true));
        assert_eq!(k.key(SPACE, false, false), (Some(KeyEvent::TalkUp), true));
        // With the modifiers still down, a new push starts again (a follow-up Dictation).
        assert_eq!(k.key(SPACE, true, false), (Some(KeyEvent::TalkDown), true));
        assert_eq!(k.key(SPACE, false, false), (Some(KeyEvent::TalkUp), true));
    }

    #[test]
    fn the_main_key_before_a_modifier_also_starts() {
        let mut k = keyboard();
        k.key(LCTRL, true, false);
        // Ctrl+Space alone is another app's shortcut: it goes on.
        assert_eq!(k.key(SPACE, true, false), (None, false));
        // Shift completes the Talk Key.
        assert_eq!(
            k.key(LSHIFT, true, false),
            (Some(KeyEvent::TalkDown), false)
        );
        // The repeats of the held main key no longer go on.
        assert_eq!(k.key(SPACE, true, false), (None, true));
        // Its down event went on, so its up event goes on too.
        assert_eq!(k.key(SPACE, false, false), (Some(KeyEvent::TalkUp), false));
    }

    #[test]
    fn a_modifier_release_does_not_end_the_talk_key() {
        let mut k = keyboard();
        k.key(LCTRL, true, false);
        k.key(LSHIFT, true, false);
        k.key(SPACE, true, false);
        assert_eq!(k.key(LSHIFT, false, false), (None, false));
        assert_eq!(k.key(SPACE, false, false), (Some(KeyEvent::TalkUp), true));
    }

    #[test]
    fn other_modifiers_or_missing_ones_do_not_start() {
        let mut k = keyboard();
        k.key(LCTRL, true, false);
        k.key(LSHIFT, true, false);
        k.key(LALT, true, false);
        assert_eq!(k.key(SPACE, true, false), (None, false));
        assert_eq!(k.key(SPACE, false, false), (None, false));
        k.key(LALT, false, false);
        k.key(LSHIFT, false, false);
        assert_eq!(k.key(SPACE, true, false), (None, false));
    }

    #[test]
    fn left_and_right_shift_count_separately() {
        let mut k = keyboard();
        k.key(LCTRL, true, false);
        k.key(LSHIFT, true, false);
        k.key(RSHIFT, true, false);
        k.key(LSHIFT, false, false);
        // Right Shift is still down.
        assert_eq!(k.key(SPACE, true, false), (Some(KeyEvent::TalkDown), true));
    }

    #[test]
    fn escape_belongs_to_livetyper_only_during_a_dictation() {
        let mut k = keyboard();
        assert_eq!(k.key(VK_ESCAPE, true, false), (None, false));
        assert_eq!(k.key(VK_ESCAPE, false, false), (None, false));
        // Also with Ctrl+Shift down (Windows would open Task Manager).
        k.key(LCTRL, true, true);
        k.key(LSHIFT, true, true);
        assert_eq!(k.key(VK_ESCAPE, true, true), (Some(KeyEvent::Escape), true));
        assert_eq!(k.key(VK_ESCAPE, false, false), (None, true));
    }
}
