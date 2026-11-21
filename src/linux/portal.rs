//! Wayland shortcuts and text insertion through the desktop portals.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use glib::variant::{Handle, ObjectPath};
use gtk4::{gio, glib, prelude::*};

use crate::hotkey::KeyEvent;

const BUS: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
const CLIPBOARD: &str = "org.freedesktop.portal.Clipboard";
const TEXT: &str = "text/plain;charset=utf-8";
const TALK: &str = "talk";
type Options = HashMap<&'static str, glib::Variant>;
type Results = HashMap<String, glib::Variant>;

fn token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("livetyper{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

async fn call(
    bus: &gio::DBusConnection,
    interface: &str,
    method: &str,
    args: glib::Variant,
) -> Result<glib::Variant, String> {
    bus.call_future(
        Some(BUS),
        PATH,
        interface,
        method,
        Some(&args),
        None,
        gio::DBusCallFlags::NONE,
        5000,
    )
    .await
    .map_err(|error| format!("{method}: {error}"))
}

pub async fn connect(app_id: &str) -> Result<gio::DBusConnection, String> {
    // GTK also uses portals on the shared session bus. Use our own connection so the
    // registered application identity is established before its first portal call.
    let address = gio::dbus_address_get_for_bus_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|error| error.to_string())?;
    let bus = gio::DBusConnection::for_address_future(
        &address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
    )
    .await
    .map_err(|error| error.to_string())?;
    call(
        &bus,
        "org.freedesktop.host.portal.Registry",
        "Register",
        (app_id, Options::new()).to_variant(),
    )
    .await
    .map_err(|error| format!("{error}. Install the LiveTyper desktop entry and try again."))?;
    Ok(bus)
}

async fn request(
    bus: &gio::DBusConnection,
    interface: &str,
    method: &str,
    args: impl FnOnce(Options) -> glib::Variant,
) -> Result<Results, String> {
    let handle = token();
    let sender = bus.unique_name().ok_or("No D-Bus unique name")?;
    let sender = sender.trim_start_matches(':').replace('.', "_");
    let path = format!("{PATH}/request/{sender}/{handle}");
    // Subscribe before sending the request. The portal may respond before the method reply.
    let responses = bus.receive_signal(
        Some(BUS),
        Some(REQUEST),
        Some("Response"),
        Some(&path),
        None,
        gio::DBusSignalFlags::NONE,
        |signal| signal.parameters.clone(),
    );
    let reply = call(
        bus,
        interface,
        method,
        args(Options::from([("handle_token", handle.to_variant())])),
    )
    .await?;
    let (actual_path,) = reply
        .get::<(ObjectPath,)>()
        .ok_or("Invalid portal request handle")?;
    if actual_path.as_str() != path {
        return Err(format!("{method}: unexpected portal request handle"));
    }
    let (status, results) = std::pin::pin!(responses)
        .next()
        .await
        .and_then(|reply| reply.get::<(u32, Results)>())
        .ok_or("Invalid portal response")?;
    match status {
        0 => Ok(results),
        1 => Err(format!("{method}: permission cancelled")),
        _ => Err(format!("{method}: request rejected")),
    }
}

struct Session {
    bus: gio::DBusConnection,
    path: ObjectPath,
}

impl Session {
    async fn create(bus: &gio::DBusConnection, interface: &str) -> Result<Self, String> {
        let results = request(bus, interface, "CreateSession", |mut options| {
            options.insert("session_handle_token", token().to_variant());
            (options,).to_variant()
        })
        .await?;
        // The portal currently returns the handle as a string for compatibility.
        let value = results
            .get("session_handle")
            .ok_or("Missing portal session handle")?;
        let path = ObjectPath::try_from(value.str().ok_or("Invalid portal session handle")?)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            bus: bus.clone(),
            path,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.bus.call(
            Some(BUS),
            &self.path,
            SESSION,
            "Close",
            None,
            None,
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
            |_| {},
        );
    }
}

#[derive(Default)]
struct HeldKey {
    pressed: bool,
    sequence: u64,
}

impl HeldKey {
    fn control_released(&mut self, sequence: u64, control_held: bool) -> Option<KeyEvent> {
        if self.sequence != sequence || control_held {
            return None;
        }
        self.update(false)
    }
}

impl HeldKey {
    fn update(&mut self, pressed: bool) -> Option<KeyEvent> {
        if self.pressed == pressed {
            return None;
        }
        self.pressed = pressed;
        if pressed {
            self.sequence += 1;
        }
        Some(if pressed {
            KeyEvent::TalkDown
        } else {
            KeyEvent::TalkUp
        })
    }
}

pub struct Shortcuts {
    held: Rc<RefCell<HeldKey>>,
    check_control: bool,
    control_unavailable_for: Rc<Cell<Option<u64>>>,
    control_query: Rc<Cell<bool>>,
    _signals: gio::SignalSubscription,
    _closed: gio::SignalSubscription,
    _session: Session,
}

impl Shortcuts {
    pub async fn new(
        bus: &gio::DBusConnection,
        trigger: &str,
        on_event: impl Fn(KeyEvent) + 'static,
    ) -> Result<Self, String> {
        let session = Session::create(bus, SHORTCUTS).await?;
        let held = Rc::new(RefCell::new(HeldKey::default()));
        let handler = Rc::new(on_event);
        let closed = bus.subscribe_to_signal(
            Some(BUS),
            Some(SESSION),
            Some("Closed"),
            Some(&session.path),
            None,
            gio::DBusSignalFlags::NONE,
            {
                let held = held.clone();
                let handler = handler.clone();
                move |_| {
                    if held.borrow_mut().update(false).is_some() {
                        handler(KeyEvent::Escape);
                    }
                    log::warn!(
                        "The Talk Key portal session closed. Restart LiveTyper to reconnect."
                    );
                }
            },
        );
        let talk_session = session.path.clone();
        let signal_held = held.clone();
        let signals = bus.subscribe_to_signal(
            Some(BUS),
            Some(SHORTCUTS),
            None,
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let pressed = match signal.signal_name {
                    "Activated" => true,
                    "Deactivated" => false,
                    _ => return,
                };
                if signal.parameters.child_get::<ObjectPath>(0) == talk_session
                    && signal.parameters.child_get::<String>(1) == TALK
                    && let Some(event) = signal_held.borrow_mut().update(pressed)
                {
                    handler(event);
                }
            },
        );
        let shortcuts = vec![(
            TALK,
            Options::from([
                ("description", "Hold to dictate with LiveTyper".to_variant()),
                ("preferred_trigger", trigger.to_variant()),
            ]),
        )];
        let response = request(bus, SHORTCUTS, "BindShortcuts", |options| {
            (&session.path, shortcuts, "", options).to_variant()
        })
        .await?;
        let shortcuts = response
            .get("shortcuts")
            .and_then(|value| value.get::<Vec<(String, Results)>>());
        let binding = shortcuts.as_ref().and_then(|shortcuts| {
            shortcuts.iter().find_map(|(id, properties)| {
                (id == TALK)
                    .then(|| properties.get("trigger_description")?.str())
                    .flatten()
                    .filter(|description| !description.is_empty())
            })
        });
        let Some(binding) = binding else {
            return Err(
                "Bind a Talk Key for LiveTyper in GNOME Settings and restart LiveTyper.".into(),
            );
        };
        Ok(Self {
            held,
            check_control: is_gnome_control_space(binding),
            control_unavailable_for: Rc::default(),
            control_query: Rc::default(),
            _signals: signals,
            _closed: closed,
            _session: session,
        })
    }

    pub fn poll_control(
        &self,
        shell: &gio::DBusConnection,
        on_release: impl FnOnce(KeyEvent) + 'static,
    ) {
        if !self.check_control || self.control_query.get() {
            return;
        }
        let sequence = {
            let held = self.held.borrow();
            if !held.pressed || self.control_unavailable_for.get() == Some(held.sequence) {
                return;
            }
            held.sequence
        };
        self.control_query.set(true);
        let shell = shell.clone();
        let held = self.held.clone();
        let control_unavailable_for = self.control_unavailable_for.clone();
        let control_query = self.control_query.clone();
        glib::spawn_future_local(async move {
            let control = super::pad::talk_control_held(&shell).await;
            control_query.set(false);
            if let Some(control_held) = control {
                if let Some(event) = held.borrow_mut().control_released(sequence, control_held) {
                    on_release(event);
                }
            } else {
                control_unavailable_for.set(Some(sequence));
            }
        });
    }
}

// GNOME 50 formats a single binding as the translated "Press %s" with the
// GTK accelerator unchanged. Match the bound key, since GNOME may have kept
// a user's earlier binding instead of accepting our preferred trigger.
fn is_gnome_control_space(description: &str) -> bool {
    let mut words = description.split_whitespace();
    words.next().is_some()
        && words
            .next()
            .is_some_and(|key| key.eq_ignore_ascii_case("<Control>space"))
        && words.next().is_none()
}

#[derive(Clone, Default)]
struct Selection {
    text: String,
    received: Rc<Cell<bool>>,
}

pub struct PasteSession {
    session: Session,
    closed: Rc<Cell<bool>>,
    selection: Rc<RefCell<Selection>>,
    _closed_signal: gio::SignalSubscription,
    _transfer: gio::SignalSubscription,
}

impl PasteSession {
    pub async fn new(bus: &gio::DBusConnection) -> Result<Self, String> {
        let session = Session::create(bus, REMOTE).await?;
        let token_file = crate::paths::local_dir().join("gnome-paste-token");
        let restore_token = std::fs::read_to_string(&token_file).ok();
        request(bus, REMOTE, "SelectDevices", |mut options| {
            options.insert("types", 1u32.to_variant()); // Keyboard
            options.insert("persist_mode", 2u32.to_variant()); // Until revoked
            if let Some(token) = &restore_token {
                options.insert("restore_token", token.to_variant());
            }
            (&session.path, options).to_variant()
        })
        .await?;
        call(
            bus,
            CLIPBOARD,
            "RequestClipboard",
            (&session.path, Options::new()).to_variant(),
        )
        .await?;
        let granted = request(bus, REMOTE, "Start", |options| {
            (&session.path, "", options).to_variant()
        })
        .await?;
        // Restore tokens are single-use. Store the replacement before the paste,
        // even if the target later rejects the clipboard transfer.
        if let Some(token) = granted.get("restore_token").and_then(glib::Variant::str) {
            let file = gio::File::for_path(&token_file);
            if let Err(error) = file.replace_contents(
                token.as_bytes(),
                None,
                false,
                gio::FileCreateFlags::PRIVATE | gio::FileCreateFlags::REPLACE_DESTINATION,
                gio::Cancellable::NONE,
            ) {
                log::warn!("Could not remember GNOME paste permission: {error}");
            }
        } else if restore_token.is_some()
            && let Err(error) = std::fs::remove_file(&token_file)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("Could not remove the expired GNOME paste token: {error}");
        }
        if granted
            .get("devices")
            .and_then(glib::Variant::get::<u32>)
            .unwrap_or_default()
            & 1
            == 0
            || granted
                .get("clipboard_enabled")
                .and_then(glib::Variant::get::<bool>)
                != Some(true)
        {
            return Err(
                "GNOME did not grant keyboard and clipboard access for automatic paste.".into(),
            );
        }

        let closed = Rc::new(Cell::new(false));
        let selection = Rc::new(RefCell::new(Selection::default()));
        let closed_signal = bus.subscribe_to_signal(
            Some(BUS),
            Some(SESSION),
            Some("Closed"),
            Some(&session.path),
            None,
            gio::DBusSignalFlags::NONE,
            {
                let closed = closed.clone();
                move |_| closed.set(true)
            },
        );
        let transfer = bus.subscribe_to_signal(
            Some(BUS),
            Some(CLIPBOARD),
            Some("SelectionTransfer"),
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            {
                let selection = selection.clone();
                let active_session = session.path.clone();
                move |signal| {
                    let Some((session, mime, serial)) =
                        signal.parameters.get::<(ObjectPath, String, u32)>()
                    else {
                        return;
                    };
                    if session != active_session {
                        return;
                    }
                    let bus = signal.connection.clone();
                    let Selection { text, received } = selection.borrow().clone();
                    glib::spawn_future_local(async move {
                        let written =
                            mime == TEXT && write_selection(&bus, &session, serial, text).await;
                        if let Err(error) = call(
                            &bus,
                            CLIPBOARD,
                            "SelectionWriteDone",
                            (&session, serial, written).to_variant(),
                        )
                        .await
                        {
                            log::warn!("Clipboard transfer: {error}");
                        } else if written {
                            received.set(true);
                        }
                    });
                }
            },
        );
        Ok(Self {
            session,
            closed,
            selection,
            _closed_signal: closed_signal,
            _transfer: transfer,
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    /// Pastes `text` as it is; `follow_up_text` decides about a space before it.
    pub async fn paste(
        &self,
        text: String,
        verify_focus: impl Future<Output = Result<(), String>>,
    ) -> Result<(), String> {
        let Session { bus, path } = &self.session;
        let received = Rc::new(Cell::new(false));
        self.selection.replace(Selection {
            text,
            received: received.clone(),
        });
        call(
            bus,
            CLIPBOARD,
            "SetSelection",
            (path, Options::from([("mime_types", [TEXT].to_variant())])).to_variant(),
        )
        .await?;

        // The selection is registered before the target receives the Wayland
        // clipboard offer. Give it time to process the offer before Ctrl+V.
        glib::timeout_future(Duration::from_millis(50)).await;
        verify_focus.await?;

        // Evdev keycodes for left Ctrl and V, emitted by the compositor.
        let mut result = Ok(());
        for (key, pressed) in [(29i32, 1u32), (47, 1), (47, 0), (29, 0)] {
            if result.is_err() && pressed == 1 {
                continue;
            }
            let event = call(
                bus,
                REMOTE,
                "NotifyKeyboardKeycode",
                (path, Options::new(), key, pressed).to_variant(),
            )
            .await;
            if result.is_ok() {
                result = event.map(|_| ());
            }
        }
        result?;
        let deadline = Instant::now() + Duration::from_secs(1);
        while !received.get() && !self.is_closed() && Instant::now() < deadline {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
        if !received.get() {
            return Err("The target did not request the dictated text.".into());
        }
        // A clipboard manager can request the selection before the editor.
        // Keep serving requests briefly after the first successful transfer.
        glib::timeout_future(Duration::from_millis(150)).await;
        Ok(())
    }
}

/// A Dictation that starts this soon after the last paste into the same window is a follow-up.
pub const FOLLOW_UP_WINDOW: Duration = Duration::from_secs(30);

/// The text to paste, with a space before it if it follows the last paste into the same window,
/// and the facts of that decision for the Text Trace.
pub fn follow_up_text(
    text: &str,
    target: Option<u32>,
    started: Instant,
    last_paste: Option<(u32, Instant)>,
) -> (String, crate::trace::FollowUp) {
    let same_target = target
        .zip(last_paste)
        .is_some_and(|(target, (last, _))| target == last);
    let since = last_paste.map(|(_, at)| started.saturating_duration_since(at));
    let follow_up = same_target && since.is_some_and(|since| since < FOLLOW_UP_WINDOW);
    let space_added = follow_up && needs_space_before(text);
    let text = if space_added {
        format!(" {text}")
    } else {
        text.to_owned()
    };
    let facts = crate::trace::FollowUp {
        same_target,
        seconds_since_last: since.map(|since| since.as_secs_f64()),
        window_seconds: FOLLOW_UP_WINDOW.as_secs_f64(),
        space_added,
    };
    (text, facts)
}

fn needs_space_before(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|c| !c.is_whitespace() && !".,;:!?)]}'\"".contains(c))
}

async fn write_selection(
    bus: &gio::DBusConnection,
    path: &ObjectPath,
    serial: u32,
    text: String,
) -> bool {
    let Ok((reply, Some(fds))) = bus
        .call_with_unix_fd_list_future(
            Some(BUS),
            PATH,
            CLIPBOARD,
            "SelectionWrite",
            Some(&(path, serial).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::UnixFDList>,
        )
        .await
    else {
        return false;
    };
    let Some((Handle(index),)) = reply.get::<(Handle,)>() else {
        return false;
    };
    let Ok(fd) = fds.get(index) else {
        return false;
    };
    gio::spawn_blocking(move || std::fs::File::from(fd).write_all(text.as_bytes()).is_ok())
        .await
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_up_space_only_for_the_same_window_soon_after() {
        let at = Instant::now();
        let started = at + Duration::from_secs(5);
        let (text, facts) = follow_up_text("Next.", Some(7), started, Some((7, at)));
        assert_eq!(text, " Next.");
        assert!(facts.same_target && facts.space_added);
        assert_eq!(facts.seconds_since_last, Some(5.0));
        let (text, facts) = follow_up_text("Next.", Some(8), started, Some((7, at)));
        assert_eq!(text, "Next.");
        assert!(!facts.same_target && !facts.space_added);
        let late = at + FOLLOW_UP_WINDOW + Duration::from_secs(1);
        assert_eq!(
            follow_up_text("Next.", Some(7), late, Some((7, at))).0,
            "Next."
        );
        assert_eq!(
            follow_up_text(", then", Some(7), started, Some((7, at))).0,
            ", then"
        );
        let (text, facts) = follow_up_text("First", None, started, None);
        assert_eq!(text, "First");
        assert_eq!(facts.seconds_since_last, None);
    }

    #[test]
    fn one_press_and_release_for_a_repeating_hold() {
        let mut key = HeldKey::default();
        assert_eq!(key.update(false), None);
        assert_eq!(key.update(true), Some(KeyEvent::TalkDown));
        for _ in 0..100 {
            assert_eq!(key.update(true), None);
        }
        assert_eq!(key.update(false), Some(KeyEvent::TalkUp));
        assert_eq!(key.update(false), None);
        assert_eq!(key.update(true), Some(KeyEvent::TalkDown));
    }
    #[test]
    fn control_space_release_orders_end_once_with_or_without_repeats() {
        for repeats in [0, 5] {
            for control_first in [false, true] {
                let mut key = HeldKey::default();
                assert_eq!(key.update(true), Some(KeyEvent::TalkDown));
                let press = key.sequence;
                assert_eq!(key.control_released(press, true), None);
                for _ in 0..repeats {
                    assert_eq!(key.update(true), None);
                    assert_eq!(key.control_released(press, true), None);
                }
                if control_first {
                    assert_eq!(key.control_released(press, false), Some(KeyEvent::TalkUp));
                    assert_eq!(key.update(false), None);
                } else {
                    assert_eq!(key.update(false), Some(KeyEvent::TalkUp));
                    assert_eq!(key.control_released(press, false), None);
                }
                assert_eq!(key.update(true), Some(KeyEvent::TalkDown));
            }
        }
    }

    #[test]
    fn a_stale_shell_sample_cannot_release_a_new_take() {
        let mut key = HeldKey::default();
        key.update(true);
        let stale = key.sequence;
        key.update(false);
        key.update(true);
        assert_eq!(key.control_released(stale, false), None);
        assert_eq!(key.update(false), Some(KeyEvent::TalkUp));
    }

    #[test]
    fn control_probe_is_restricted_to_the_bound_control_space_accelerator() {
        assert!(is_gnome_control_space("Press <Control>space"));
        assert!(is_gnome_control_space("Appuyez <Control>space"));
        assert!(!is_gnome_control_space("Press F9"));
        assert!(!is_gnome_control_space("Press <Shift><Control>F10"));
        assert!(!is_gnome_control_space("Press <Control>space or F9"));
    }
}
