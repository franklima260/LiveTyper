//! Observe the Talk Key on physical keyboards without grabbing them from the compositor.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use evdev::{Device, EventSummary, KeyCode};
use global_hotkey::hotkey::{HotKey, Modifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    TalkDown,
    TalkUp,
    Escape,
}

const CTRL: u8 = 1;
const SHIFT: u8 = 2;
const ALT: u8 = 4;
const SUPER: u8 = 8;

#[derive(Debug, Clone, Copy)]
pub struct Chord {
    key: KeyCode,
    modifiers: u8,
}

impl Chord {
    pub fn parse(text: &str) -> Result<Self, String> {
        let hotkey: HotKey = text
            .parse()
            .map_err(|_| format!("\"{text}\" is not a key combination."))?;
        let name = format!("{:?}", hotkey.key);
        let key = linux_key(&name)
            .ok_or_else(|| format!("The key {name} in \"{text}\" cannot be the Talk Key."))?;
        #[allow(deprecated)]
        let super_key = Modifiers::SUPER | Modifiers::META;
        let mut modifiers = 0;
        for (flag, bit) in [
            (Modifiers::CONTROL, CTRL),
            (Modifiers::SHIFT, SHIFT),
            (Modifiers::ALT, ALT),
            (super_key, SUPER),
        ] {
            if hotkey.mods.intersects(flag) {
                modifiers |= bit;
            }
        }
        Ok(Self { key, modifiers })
    }
}

fn linux_key(name: &str) -> Option<KeyCode> {
    let special = match name {
        "Backquote" => "GRAVE",
        "Equal" => "EQUAL",
        "BracketLeft" => "LEFTBRACE",
        "BracketRight" => "RIGHTBRACE",
        "Quote" => "APOSTROPHE",
        "NumpadAdd" => "KPPLUS",
        "NumpadSubtract" => "KPMINUS",
        "NumpadMultiply" => "KPASTERISK",
        "NumpadDivide" => "KPSLASH",
        "NumpadDecimal" => "KPDOT",
        name if name.starts_with("Numpad") => {
            return format!("KEY_KP{}", &name[6..]).parse().ok();
        }
        name => name
            .strip_prefix("Key")
            .or_else(|| name.strip_prefix("Digit"))
            .or_else(|| name.strip_prefix("Arrow"))
            .unwrap_or(name),
    };
    format!("KEY_{}", special.to_ascii_uppercase()).parse().ok()
}

fn modifier(key: KeyCode) -> u8 {
    match key {
        KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL => CTRL,
        KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT => SHIFT,
        KeyCode::KEY_LEFTALT | KeyCode::KEY_RIGHTALT => ALT,
        KeyCode::KEY_LEFTMETA | KeyCode::KEY_RIGHTMETA => SUPER,
        _ => 0,
    }
}

#[derive(Default)]
struct Signals {
    escape: AtomicBool,
    modifiers_held: AtomicBool,
    stop: AtomicBool,
}

pub struct Hotkeys {
    signals: Arc<Signals>,
    listener: Option<JoinHandle<()>>,
}

impl Hotkeys {
    pub fn new(
        talk_key: &str,
        on_event: impl Fn(KeyEvent) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let chord = Chord::parse(talk_key)?;
        let mut keyboards = HashMap::new();
        scan_keyboards(&mut keyboards, chord);
        if keyboards.is_empty() {
            return Err("No readable physical keyboard in /dev/input. Check the keyboard device permissions.".into());
        }

        let signals = Arc::new(Signals::default());
        signals
            .modifiers_held
            .store(modifiers(&keyboards) != 0, Ordering::Release);
        let thread_signals = signals.clone();
        let listener = thread::Builder::new()
            .name("keys".into())
            .spawn(move || listen(keyboards, chord, &thread_signals, on_event))
            .map_err(|e| format!("Cannot start the Talk Key listener: {e}"))?;
        Ok(Self {
            signals,
            listener: Some(listener),
        })
    }

    pub fn set_escape(&self, enabled: bool) {
        self.signals.escape.store(enabled, Ordering::Release);
    }

    pub fn modifiers_released(&self) -> bool {
        !self.signals.modifiers_held.load(Ordering::Acquire)
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.signals.stop.store(true, Ordering::Release);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
    }
}

struct Keyboard {
    device: Device,
    held: HashSet<KeyCode>,
}

fn scan_keyboards(keyboards: &mut HashMap<PathBuf, Keyboard>, chord: Chord) {
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if keyboards.contains_key(&path) || !physical_event_device(&path) {
            continue;
        }
        let Ok(device) = Device::open(&path) else {
            continue;
        };
        let name = device.name().unwrap_or_default().to_ascii_lowercase();
        if ["dotool", "wtype", "xdotool"]
            .iter()
            .any(|tool| name.contains(tool))
        {
            continue;
        }
        let is_keyboard = device.supported_keys().is_some_and(|keys| {
            keys.contains(chord.key)
                || keys.contains(KeyCode::KEY_A) && keys.contains(KeyCode::KEY_Z)
        });
        if !is_keyboard || device.set_nonblocking(true).is_err() {
            continue;
        }
        let held = device
            .get_key_state()
            .map(|keys| keys.iter().filter(|&key| modifier(key) != 0).collect())
            .unwrap_or_default();
        log::info!("Talk Key observes {} ({})", path.display(), name);
        keyboards.insert(path, Keyboard { device, held });
    }
}

fn physical_event_device(path: &Path) -> bool {
    let Some(name) = path
        .file_name()
        .filter(|name| name.as_encoded_bytes().starts_with(b"event"))
    else {
        return false;
    };
    Path::new("/sys/class/input")
        .join(name)
        .join("device")
        .canonicalize()
        .is_ok_and(|device| !device.starts_with("/sys/devices/virtual/input"))
}

fn modifiers(keyboards: &HashMap<PathBuf, Keyboard>) -> u8 {
    keyboards
        .values()
        .flat_map(|keyboard| &keyboard.held)
        .fold(0, |bits, &key| bits | modifier(key))
}

fn listen(
    mut keyboards: HashMap<PathBuf, Keyboard>,
    chord: Chord,
    signals: &Signals,
    on_event: impl Fn(KeyEvent),
) {
    let mut active = None;
    let mut last_scan = Instant::now();
    while !signals.stop.load(Ordering::Acquire) {
        if last_scan.elapsed() >= Duration::from_secs(1) {
            scan_keyboards(&mut keyboards, chord);
            last_scan = Instant::now();
        }

        let mut changes = Vec::new();
        keyboards.retain(|path, keyboard| match keyboard.device.fetch_events() {
            Ok(events) => {
                changes.extend(events.filter_map(|event| {
                    if let EventSummary::Key(_, key, value @ (0 | 1)) = event.destructure()
                        && (key == chord.key || key == KeyCode::KEY_ESC || modifier(key) != 0)
                    {
                        return Some((event.timestamp(), path.clone(), key, value == 1));
                    }
                    None
                }));
                true
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                true
            }
            Err(err) => {
                log::info!("Talk Key lost {}: {err}", path.display());
                false
            }
        });

        if active
            .as_ref()
            .is_some_and(|path| !keyboards.contains_key(path))
        {
            active = None;
            on_event(KeyEvent::Escape);
        }
        signals
            .modifiers_held
            .store(modifiers(&keyboards) != 0, Ordering::Release);

        // A modifier and the Talk Key can arrive from different keyboards in one poll.
        changes.sort_by_key(|(time, _, _, _)| *time);
        for (_, path, key, down) in changes {
            let held = &mut keyboards
                .get_mut(&path)
                .expect("keyboard supplied event")
                .held;
            if down {
                held.insert(key);
            } else {
                held.remove(&key);
            }
            let mods = modifiers(&keyboards);
            signals.modifiers_held.store(mods != 0, Ordering::Release);
            if key == chord.key && !down && active.as_ref() == Some(&path) {
                active = None;
                on_event(KeyEvent::TalkUp);
            } else if key == KeyCode::KEY_ESC
                && key != chord.key
                && down
                && (active.is_some() || signals.escape.load(Ordering::Acquire))
            {
                on_event(KeyEvent::Escape);
            } else if active.is_none()
                && down
                && mods == chord.modifiers
                && let Some((source, _)) =
                    keyboards.iter().find(|(_, k)| k.held.contains(&chord.key))
            {
                active = Some(source.clone());
                on_event(KeyEvent::TalkDown);
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}
