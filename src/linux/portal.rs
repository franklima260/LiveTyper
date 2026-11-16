//! Calls to the XDG desktop portal, which asks the user before it gives an app the Talk Key or
//! lets it paste.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use gtk4::{gio, glib, prelude::*};

pub const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

pub fn options<const N: usize>(
    entries: [(&str, glib::Variant); N],
) -> HashMap<String, glib::Variant> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// Calls a portal method and waits for its Request::Response signal, in the thread-default
/// main context. The wait includes the time the user takes to answer the desktop's dialog.
pub fn request(
    connection: &gio::DBusConnection,
    interface: &str,
    method: &str,
    args: impl FnOnce(&str) -> glib::Variant,
) -> Result<HashMap<String, glib::Variant>, String> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let token = format!("livetyper{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let sender = connection
        .unique_name()
        .ok_or("no D-Bus name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("{PORTAL_PATH}/request/{sender}/{token}");
    let answer = Rc::new(RefCell::new(None));
    let _response = {
        let answer = answer.clone();
        connection.subscribe_to_signal(
            None,
            Some("org.freedesktop.portal.Request"),
            Some("Response"),
            Some(&path),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                *answer.borrow_mut() = signal
                    .parameters
                    .get::<(u32, HashMap<String, glib::Variant>)>();
            },
        )
    };
    connection
        .call_sync(
            Some(PORTAL),
            PORTAL_PATH,
            interface,
            method,
            Some(&args(&token)),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
        )
        .map_err(|e| e.to_string())?;
    let context = glib::MainContext::thread_default().unwrap_or_else(glib::MainContext::default);
    while answer.borrow().is_none() {
        context.iteration(true);
    }
    match answer.take() {
        Some((0, results)) => Ok(results),
        Some((1, _)) => Err("cancelled".into()),
        Some((code, _)) => Err(format!("failed with code {code}")),
        None => unreachable!(),
    }
}

/// Creates a portal session and returns its object path.
pub fn create_session(
    connection: &gio::DBusConnection,
    interface: &str,
) -> Result<glib::variant::ObjectPath, String> {
    let results = request(connection, interface, "CreateSession", |token| {
        (options([
            ("handle_token", token.to_variant()),
            ("session_handle_token", token.to_variant()),
        ]),)
            .to_variant()
    })?;
    let handle = results
        .get("session_handle")
        .and_then(|handle| handle.str().map(str::to_owned))
        .ok_or("the portal returned no session")?;
    glib::variant::ObjectPath::try_from(handle).map_err(|e| e.to_string())
}

pub fn close_session(connection: &gio::DBusConnection, session: &str) {
    let _ = connection.call_sync(
        Some(PORTAL),
        session,
        "org.freedesktop.portal.Session",
        "Close",
        None,
        None,
        gio::DBusCallFlags::NONE,
        1000,
        gio::Cancellable::NONE,
    );
}
