//! Reads back what the Target Text Box shows after a paste, for the Text Trace only.
//!
//! It listens on the AT-SPI accessibility bus for which object has the focus (no text), and after
//! a traced paste reads the characters around the cursor. Text that the user did not dictate is
//! hidden before it is kept (see `trace::ReadBack::from_text`). It never changes accessibility
//! settings: an app that does not expose its text gives "unavailable".

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::{gio, glib, prelude::*};

use crate::trace::{CONTEXT_CHARS, ReadBack};

const TEXT: &str = "org.a11y.atspi.Text";
const TIMEOUT_MS: i32 = 1000;

pub struct FocusTracker {
    bus: gio::DBusConnection,
    focused: Rc<RefCell<Option<(String, String)>>>,
    _subscription: gio::SignalSubscription,
}

impl FocusTracker {
    pub async fn new(session: &gio::DBusConnection) -> Result<FocusTracker, String> {
        let address = session
            .call_future(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                "org.a11y.Bus",
                "GetAddress",
                None,
                Some(glib::VariantTy::new("(s)").unwrap()),
                gio::DBusCallFlags::NONE,
                TIMEOUT_MS,
            )
            .await
            .map_err(|e| format!("no accessibility bus: {e}"))?
            .get::<(String,)>()
            .ok_or("unexpected accessibility bus address")?
            .0;
        let bus = gio::DBusConnection::for_address_future(
            &address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
        )
        .await
        .map_err(|e| format!("cannot connect to the accessibility bus: {e}"))?;

        // Ask apps to send focus events. Newer AT-SPI registries take extra arguments.
        let event = "object:state-changed:focused";
        let registered = bus
            .call_future(
                Some("org.a11y.atspi.Registry"),
                "/org/a11y/atspi/registry",
                "org.a11y.atspi.Registry",
                "RegisterEvent",
                Some(&(event, Vec::<String>::new(), "").to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                TIMEOUT_MS,
            )
            .await;
        if registered.is_err() {
            bus.call_future(
                Some("org.a11y.atspi.Registry"),
                "/org/a11y/atspi/registry",
                "org.a11y.atspi.Registry",
                "RegisterEvent",
                Some(&(event,).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                TIMEOUT_MS,
            )
            .await
            .map_err(|e| {
                format!("the AT-SPI registry is not running (accessibility is off): {e}")
            })?;
        }

        let focused: Rc<RefCell<Option<(String, String)>>> = Rc::default();
        let store = focused.clone();
        let subscription = bus.subscribe_to_signal(
            None,
            Some("org.a11y.atspi.Event.Object"),
            Some("StateChanged"),
            None,
            Some("focused"),
            gio::DBusSignalFlags::NONE,
            move |signal| {
                // (siiva{sv}): kind, detail1 (1 = gained focus), detail2, value, properties.
                let gained = signal
                    .parameters
                    .try_child_value(1)
                    .and_then(|v| v.get::<i32>())
                    == Some(1);
                if gained {
                    store.replace(Some((
                        signal.sender_name.to_owned(),
                        signal.object_path.to_owned(),
                    )));
                }
            },
        );
        Ok(FocusTracker {
            bus,
            focused,
            _subscription: subscription,
        })
    }

    /// Reads the text around the cursor of the focused object, `sent_chars` being the length of the
    /// pasted text in characters.
    pub async fn read_back(&self, sent_chars: usize) -> ReadBack {
        let Some((name, path)) = self.focused.borrow().clone() else {
            return ReadBack::Unavailable(
                "the focused app did not report its text box over AT-SPI".into(),
            );
        };
        let caret = match self.property_i32(&name, &path, "CaretOffset").await {
            Ok(caret) if caret >= 0 => caret,
            Ok(_) => return ReadBack::Unavailable("the focused object has no text cursor".into()),
            Err(e) => {
                return ReadBack::Unavailable(format!("the focused object exposes no text ({e})"));
            }
        };
        let count = self
            .property_i32(&name, &path, "CharacterCount")
            .await
            .unwrap_or(caret + 2);
        let start = (caret - (sent_chars + CONTEXT_CHARS) as i32).max(0);
        let end = (caret + 2).min(count.max(caret));
        let text = self
            .bus
            .call_future(
                Some(&name),
                &path,
                TEXT,
                "GetText",
                Some(&(start, end).to_variant()),
                Some(glib::VariantTy::new("(s)").unwrap()),
                gio::DBusCallFlags::NONE,
                TIMEOUT_MS,
            )
            .await
            .ok()
            .and_then(|reply| reply.get::<(String,)>());
        match text {
            Some((text,)) => ReadBack::from_text(&text, (caret - start) as usize, sent_chars),
            None => ReadBack::Unavailable("the focused app did not return its text".into()),
        }
    }

    async fn property_i32(&self, name: &str, path: &str, property: &str) -> Result<i32, String> {
        let reply = self
            .bus
            .call_future(
                Some(name),
                path,
                "org.freedesktop.DBus.Properties",
                "Get",
                Some(&(TEXT, property).to_variant()),
                Some(glib::VariantTy::new("(v)").unwrap()),
                gio::DBusCallFlags::NONE,
                TIMEOUT_MS,
            )
            .await
            .map_err(|e| e.to_string())?;
        reply
            .child_value(0)
            .as_variant()
            .and_then(|v| v.get::<i32>())
            .ok_or_else(|| format!("{property} is not a number"))
    }
}
