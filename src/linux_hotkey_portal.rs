//! The Talk Key through the GlobalShortcuts portal (GNOME 48 or newer, KDE Plasma 6). The desktop
//! owns the key, so LiveTyper needs no access to /dev/input and sees no other keys.

use std::cell::RefCell;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use global_hotkey::hotkey::{HotKey, Modifiers};
use gtk4::{gio, glib, prelude::*};

use super::KeyEvent;
use crate::linux::portal::{PORTAL_PATH, close_session, create_session, options, request};

const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const TALK: &str = "talk";
/// Quiet time after the last repeat of the shortcut that counts as its release.
const RELEASE_MARGIN: Duration = Duration::from_millis(70);
const RELEASE_CHECK: Duration = Duration::from_millis(20);

/// The portal knows a host app by its systemd unit, app-<ID>-<N>.scope, and its desktop file
/// (https://systemd.io/DESKTOP_ENVIRONMENTS/). GNOME starts apps from the app grid and from
/// autostart that way. Otherwise, for example from a terminal, start again in such a scope.
pub fn start_in_app_scope(app_id: &str) -> Result<(), String> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let unit = cgroup.trim_end().rsplit('/').next().unwrap_or_default();
    if unit.starts_with("app-") && unit.contains(app_id) {
        return Ok(());
    }
    log::info!("starting again in a systemd scope for the GlobalShortcuts portal");
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let error = Command::new("systemd-run")
        .args(["--user", "--scope", "--quiet", "--collect"])
        .arg(format!("--unit=app-{app_id}-{}.scope", std::process::id()))
        .arg(exe)
        .args(std::env::args_os().skip(1))
        .exec();
    Err(format!("systemd-run: {error}"))
}

pub struct PortalKey {
    connection: gio::DBusConnection,
    session: String,
    _signals: gio::SignalSubscription,
    release_check: Option<glib::SourceId>,
}

impl PortalKey {
    /// Binds the Talk Key. The first time, the desktop asks the user to allow it.
    pub fn bind(talk_key: &str, on_event: impl Fn(KeyEvent) + 'static) -> Result<Self, String> {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .map_err(|e| e.to_string())?;
        let session_path = create_session(&connection, SHORTCUTS)?;
        let session = session_path.as_str().to_owned();

        let mut shortcut = options([("description", "Talk Key: hold it and speak".to_variant())]);
        if let Some(trigger) = trigger(talk_key) {
            shortcut.insert("preferred_trigger".into(), trigger.to_variant());
        }
        log::info!("asking the desktop for the Talk Key ({talk_key})");
        let bound = request(&connection, SHORTCUTS, "BindShortcuts", |token| {
            (
                session_path,
                vec![(TALK.to_owned(), shortcut)],
                "",
                options([("handle_token", token.to_variant())]),
            )
                .to_variant()
        })
        .map_err(|e| format!("the desktop did not bind the Talk Key ({e})"))?;
        let described = bound
            .get("shortcuts")
            .map(|shortcuts| shortcuts.print(false).to_string())
            .unwrap_or_default();
        log::info!("Talk Key bound through the portal: {described}");

        let key = Rc::new(RefCell::new(Release::new(KeyRepeat::from_settings())));
        let on_event = Rc::new(on_event);
        let signals = {
            let (key, on_event, session) = (key.clone(), on_event.clone(), session.clone());
            connection.subscribe_to_signal(
                None,
                Some(SHORTCUTS),
                None,
                Some(PORTAL_PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    let params = signal.parameters;
                    if params.n_children() < 2
                        || params.child_value(0).str() != Some(session.as_str())
                        || params.child_value(1).str() != Some(TALK)
                    {
                        return;
                    }
                    let event = match signal.signal_name {
                        "Activated" => key.borrow_mut().activated(Instant::now()),
                        "Deactivated" => key.borrow_mut().deactivated(),
                        _ => None,
                    };
                    if let Some(event) = event {
                        on_event(event);
                    }
                },
            )
        };
        let release_check = glib::timeout_add_local(RELEASE_CHECK, move || {
            let event = key.borrow_mut().poll(Instant::now());
            if let Some(event) = event {
                on_event(event);
            }
            glib::ControlFlow::Continue
        });
        Ok(Self {
            connection,
            session,
            _signals: signals,
            release_check: Some(release_check),
        })
    }
}

impl Drop for PortalKey {
    fn drop(&mut self) {
        if let Some(source) = self.release_check.take() {
            source.remove();
        }
        close_session(&self.connection, &self.session);
    }
}

/// The talk_key setting as a shortcut of the XDG shortcuts format, such as "CTRL+space".
/// The desktop suggests it; the user can choose another key.
fn trigger(talk_key: &str) -> Option<String> {
    let hotkey: HotKey = talk_key.parse().ok()?;
    let name = format!("{:?}", hotkey.key);
    let key = match name.as_str() {
        "Space" => "space".to_owned(),
        "Enter" => "Return".to_owned(),
        "Backquote" => "grave".to_owned(),
        "Insert" | "Pause" | "Home" | "End" | "Tab" => name.clone(),
        "ScrollLock" => "Scroll_Lock".to_owned(),
        "PageUp" => "Page_Up".to_owned(),
        "PageDown" => "Page_Down".to_owned(),
        _ if name.len() == 4 && name.starts_with("Key") => name[3..].to_ascii_lowercase(),
        _ if name.len() == 6 && name.starts_with("Digit") => name[5..].to_owned(),
        _ if name.starts_with('F') && name[1..].parse::<u8>().is_ok() => name.clone(),
        _ => return None,
    };
    #[allow(deprecated)]
    let super_key = Modifiers::SUPER | Modifiers::META;
    let mut parts: Vec<String> = [
        (Modifiers::CONTROL, "CTRL"),
        (Modifiers::ALT, "ALT"),
        (Modifiers::SHIFT, "SHIFT"),
        (super_key, "LOGO"),
    ]
    .into_iter()
    .filter(|(flag, _)| hotkey.mods.intersects(*flag))
    .map(|(_, part)| part.to_owned())
    .collect();
    parts.push(key);
    Some(parts.join("+"))
}

/// Key repeat timing: the delay before the first repeat, and the interval after it.
#[derive(Clone, Copy)]
struct KeyRepeat {
    delay: Duration,
    interval: Duration,
}

impl KeyRepeat {
    /// GNOME's key repeat timing. Other desktops send Deactivated, so they need none.
    fn from_settings() -> Option<Self> {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        if !desktop.split(':').any(|name| name == "GNOME") {
            return None;
        }
        let schema = gio::SettingsSchemaSource::default()?
            .lookup("org.gnome.desktop.peripherals.keyboard", true)?;
        let settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
        if !settings.boolean("repeat") {
            log::warn!(
                "Key repeat is off: the Talk Key may stay down until GNOME reports its release."
            );
            return None;
        }
        Some(Self {
            delay: Duration::from_millis(settings.uint("delay").into()),
            interval: Duration::from_millis(settings.uint("repeat-interval").into()),
        })
    }
}

/// GNOME sends Activated again for each key repeat while the whole shortcut is held, but it does
/// not send Deactivated for a chord such as Ctrl+Space. So the end of the repeats also counts as
/// the release.
struct Release {
    repeat: Option<KeyRepeat>,
    /// The last Activated, and whether it was a repeat.
    down: Option<(Instant, bool)>,
}

impl Release {
    fn new(repeat: Option<KeyRepeat>) -> Self {
        Self { repeat, down: None }
    }

    fn activated(&mut self, now: Instant) -> Option<KeyEvent> {
        let first = self.down.is_none();
        self.down = Some((now, !first));
        first.then_some(KeyEvent::TalkDown)
    }

    fn deactivated(&mut self) -> Option<KeyEvent> {
        self.down.take().map(|_| KeyEvent::TalkUp)
    }

    fn poll(&mut self, now: Instant) -> Option<KeyEvent> {
        let repeat = self.repeat?;
        let (last, repeating) = self.down?;
        let quiet = if repeating {
            repeat.interval
        } else {
            repeat.delay
        } + RELEASE_MARGIN;
        if now.saturating_duration_since(last) <= quiet {
            return None;
        }
        self.down = None;
        Some(KeyEvent::TalkUp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GNOME: KeyRepeat = KeyRepeat {
        delay: Duration::from_millis(500),
        interval: Duration::from_millis(30),
    };

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn talk_key_setting_becomes_an_xdg_shortcut() {
        assert_eq!(trigger("Ctrl+Space").as_deref(), Some("CTRL+space"));
        assert_eq!(trigger("Ctrl+Shift+F9").as_deref(), Some("CTRL+SHIFT+F9"));
        assert_eq!(trigger("Ctrl+Alt+D").as_deref(), Some("CTRL+ALT+d"));
        assert_eq!(trigger("F9").as_deref(), Some("F9"));
        assert_eq!(trigger("not a key"), None);
    }

    #[test]
    fn the_end_of_the_repeats_releases_a_held_chord() {
        let start = Instant::now();
        let mut key = Release::new(Some(GNOME));
        assert_eq!(key.activated(start), Some(KeyEvent::TalkDown));
        // No repeat yet: wait for the repeat delay.
        assert_eq!(key.poll(ms(start, 400)), None);
        assert_eq!(key.activated(ms(start, 500)), None);
        assert_eq!(key.activated(ms(start, 530)), None);
        assert_eq!(key.poll(ms(start, 600)), None);
        assert_eq!(key.poll(ms(start, 640)), Some(KeyEvent::TalkUp));
        assert_eq!(key.poll(ms(start, 700)), None);
    }

    #[test]
    fn a_short_press_releases_after_the_repeat_delay() {
        let start = Instant::now();
        let mut key = Release::new(Some(GNOME));
        key.activated(start);
        assert_eq!(key.poll(ms(start, 560)), None);
        assert_eq!(key.poll(ms(start, 580)), Some(KeyEvent::TalkUp));
    }

    #[test]
    fn deactivated_releases_at_once_and_only_once() {
        let start = Instant::now();
        let mut key = Release::new(Some(GNOME));
        key.activated(start);
        assert_eq!(key.deactivated(), Some(KeyEvent::TalkUp));
        assert_eq!(key.deactivated(), None);
        assert_eq!(key.poll(ms(start, 2000)), None);
        assert_eq!(key.activated(ms(start, 2100)), Some(KeyEvent::TalkDown));
    }

    #[test]
    fn without_key_repeat_only_deactivated_releases() {
        let start = Instant::now();
        let mut key = Release::new(None);
        key.activated(start);
        assert_eq!(key.poll(ms(start, 5000)), None);
        assert_eq!(key.deactivated(), Some(KeyEvent::TalkUp));
    }
}
