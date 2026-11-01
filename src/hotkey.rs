//! The Talk Key and the Escape key, through `global-hotkey` (RegisterHotKey: other apps do not get them).
//!
//! The manager must live on the UI thread, which runs the Win32 message loop.

use global_hotkey::hotkey::{Code, HotKey};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    TalkDown,
    TalkUp,
    Escape,
}

pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    /// The Escape keys, and for each one, whether it is registered now.
    escapes: Vec<(HotKey, bool)>,
}

impl Hotkeys {
    /// `on_event` runs on the UI thread or on the library's key-release thread.
    /// On an error, returns a message for the user (the details go to the log).
    pub fn new(
        talk_key: &str,
        on_event: impl Fn(KeyEvent) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
        let talk: HotKey = talk_key
            .parse()
            .map_err(|_| format!("\"{talk_key}\" is not a key combination."))?;
        manager.register(talk).map_err(|e| {
            log::error!("talk key {talk_key}: {e}");
            format!(
                "The Talk Key {talk_key} does not work. Another app uses it, for example \
                 PowerToys Peek. Set a different talk_key in the settings file (Open settings \
                 in the tray menu), then restart LiveTyper."
            )
        })?;
        // RegisterHotKey needs the exact modifiers. While the user holds the Talk Key, its
        // modifiers are down, so Escape also registers with them (for example Ctrl+Escape).
        let mut escapes = vec![HotKey::new(None, Code::Escape)];
        if !talk.mods.is_empty() {
            escapes.push(HotKey::new(Some(talk.mods), Code::Escape));
        }
        escapes.retain(|escape| escape.id() != talk.id());
        let talk_id = talk.id();
        let escape_ids: Vec<u32> = escapes.iter().map(HotKey::id).collect();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            let key = match (event.id(), event.state()) {
                (id, HotKeyState::Pressed) if id == talk_id => KeyEvent::TalkDown,
                (id, HotKeyState::Released) if id == talk_id => KeyEvent::TalkUp,
                (id, HotKeyState::Pressed) if escape_ids.contains(&id) => KeyEvent::Escape,
                _ => return,
            };
            on_event(key);
        }));
        Ok(Hotkeys {
            manager,
            escapes: escapes.into_iter().map(|escape| (escape, false)).collect(),
        })
    }

    /// Escape belongs to LiveTyper only during a Dictation. If one Escape key cannot register,
    /// the others still work.
    pub fn set_escape(&mut self, on: bool) {
        for (escape, registered) in &mut self.escapes {
            if *registered == on {
                continue;
            }
            let result = if on {
                self.manager.register(*escape)
            } else {
                self.manager.unregister(*escape)
            };
            match result {
                Ok(()) => *registered = on,
                Err(e) => log::error!("escape key {escape} (on = {on}): {e}"),
            }
        }
    }
}
