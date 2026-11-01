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
    escape: HotKey,
    escape_registered: bool,
}

impl Hotkeys {
    /// `on_event` runs on the UI thread or on the library's key-release thread.
    pub fn new(
        talk_key: &str,
        on_event: impl Fn(KeyEvent) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
        let talk: HotKey = talk_key
            .parse()
            .map_err(|_| format!("\"{talk_key}\" is not a key combination."))?;
        manager.register(talk).map_err(|e| {
            format!("The Talk Key {talk_key} cannot register. Another app may use it. ({e})")
        })?;
        let escape = HotKey::new(None, Code::Escape);
        let (talk_id, escape_id) = (talk.id(), escape.id());
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            let key = match (event.id(), event.state()) {
                (id, HotKeyState::Pressed) if id == talk_id => KeyEvent::TalkDown,
                (id, HotKeyState::Released) if id == talk_id => KeyEvent::TalkUp,
                (id, HotKeyState::Pressed) if id == escape_id => KeyEvent::Escape,
                _ => return,
            };
            on_event(key);
        }));
        Ok(Hotkeys {
            manager,
            escape,
            escape_registered: false,
        })
    }

    /// Escape belongs to LiveTyper only during a Dictation.
    pub fn set_escape(&mut self, on: bool) {
        if on == self.escape_registered {
            return;
        }
        let result = if on {
            self.manager.register(self.escape)
        } else {
            self.manager.unregister(self.escape)
        };
        match result {
            Ok(()) => self.escape_registered = on,
            Err(e) => log::error!("escape key (on = {on}): {e}"),
        }
    }
}
