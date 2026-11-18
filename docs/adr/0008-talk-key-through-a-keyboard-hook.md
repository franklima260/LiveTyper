# The Talk Key through a low-level keyboard hook

On Windows, LiveTyper reads the Talk Key with a low-level keyboard hook (`WH_KEYBOARD_LL`), not with `RegisterHotKey`. The hook sees each key before other apps. The Talk Key starts when its main key and exactly its modifiers are down, in any order, and it ends when the main key goes up. While LiveTyper uses the main key, the hook removes it from the input. It also removes Escape during a Dictation. Modifier keys always go on to the other apps.

`RegisterHotKey` fires only when the main key goes down while the modifiers are already down. In fast back-to-back Dictations, the user often pushed Space a moment before Shift. Windows then saw Ctrl+Space, and another app with that shortcut got it. In one test, 20 of 36 pushes did not reach LiveTyper. Also, `RegisterHotKey` cannot take Ctrl+Shift+Escape (Windows uses it for Task Manager), so Escape did not cancel while the user held Ctrl+Shift+Space.

## Considered Options

- `RegisterHotKey` (through `global-hotkey`). Rejected for the reasons above. LiveTyper still uses `global-hotkey` to read the `talk_key` setting.

## Consequences

- An app that has the same shortcut no longer gets it while LiveTyper runs.
- If the user pushes the main key before the last modifier, the other apps see that main key with the first modifiers for a moment. For example, Ctrl+Space goes on to the other apps before Shift completes Ctrl+Shift+Space.
- LiveTyper marks the paste keys that it sends, and the hook ignores them. Keys that other programs send (a test script, a remote desktop) count as the user's keys.
- The hook must return fast: it only posts events to the UI thread. Windows removes a hook that is too slow.


## Linux desktop integration

KDE Wayland uses the physical keyboard through evdev. Ubuntu GNOME uses the GlobalShortcuts portal for the Talk Key and the RemoteDesktop and Clipboard portals for paste. The bundled GNOME Shell extension renders the Scratch Pad, handles Escape during Dictation, and checks focus before paste.

GNOME 50 can omit the Ctrl+Space release when Ctrl comes up first. While that binding is held, the app queries the extension for Ctrl state and ends the take when Ctrl is released. Space-first releases use the portal event. The probe uses the binding returned by GNOME, and a response from an earlier take cannot release a newer one. Without the extension, a single-key binding such as F9 is the documented workaround. See issue #7 for the reproduction and hardware retest.
