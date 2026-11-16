//! Paste the Draft from the clipboard with Ctrl+V through the RemoteDesktop portal, for accounts
//! without ydotool. The desktop asks once, and a restore token keeps the permission. Each paste
//! opens a short session, so the desktop shows its remote-control icon only while pasting.
//!
//! LiveTyper sends Ctrl+V and does not type the text: a Ctrl that the user still holds after
//! Ctrl+Space then changes nothing, and any character works.

use std::path::PathBuf;

use gtk4::{gio, glib, prelude::*};

use super::portal::{PORTAL, PORTAL_PATH, close_session, create_session, options, request};
use crate::paths;

const REMOTE_DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";
const KEYBOARD: u32 = 1;
const PERSIST_UNTIL_REVOKED: u32 = 2;
const XK_CONTROL_L: i32 = 0xffe3;
const XK_V: i32 = 0x76;

/// True until the user allowed pasting and the desktop kept the permission.
pub fn needs_permission() -> bool {
    !token_file().exists()
}

/// Sends Ctrl+V to the focused app. Blocks while the desktop asks the user, so call it from a
/// worker thread.
pub fn paste() -> Result<(), String> {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
                .map_err(|e| e.to_string())?;
            let session = create_session(&connection, REMOTE_DESKTOP)?;
            let result = paste_in(&connection, &session);
            close_session(&connection, session.as_str());
            result
        })
        .map_err(|e| e.to_string())?
}

fn paste_in(
    connection: &gio::DBusConnection,
    session: &glib::variant::ObjectPath,
) -> Result<(), String> {
    request(connection, REMOTE_DESKTOP, "SelectDevices", |token| {
        let mut select = options([
            ("handle_token", token.to_variant()),
            ("types", KEYBOARD.to_variant()),
            ("persist_mode", PERSIST_UNTIL_REVOKED.to_variant()),
        ]);
        if let Ok(saved) = std::fs::read_to_string(token_file()) {
            select.insert("restore_token".into(), saved.trim().to_variant());
        }
        (session, select).to_variant()
    })?;
    let started = request(connection, REMOTE_DESKTOP, "Start", |token| {
        (session, "", options([("handle_token", token.to_variant())])).to_variant()
    })?;
    // A restore token works once: keep the new one for the next paste.
    match started.get("restore_token").and_then(|token| token.str()) {
        Some(token) => {
            let file = token_file();
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(error) = std::fs::write(&file, token) {
                log::warn!("cannot keep the paste permission: {error}");
            }
        }
        None => {
            let _ = std::fs::remove_file(token_file());
        }
    }
    for (keysym, pressed) in [
        (XK_CONTROL_L, 1u32),
        (XK_V, 1),
        (XK_V, 0),
        (XK_CONTROL_L, 0),
    ] {
        connection
            .call_sync(
                Some(PORTAL),
                PORTAL_PATH,
                REMOTE_DESKTOP,
                "NotifyKeyboardKeysym",
                Some(&(session, options([]), keysym, pressed).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                1000,
                gio::Cancellable::NONE,
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn token_file() -> PathBuf {
    paths::local_dir().join("remote-desktop-token")
}
