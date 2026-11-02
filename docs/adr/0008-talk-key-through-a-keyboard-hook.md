# The Talk Key through a low-level keyboard hook

LiveTyper reads the Talk Key with a low-level keyboard hook (`WH_KEYBOARD_LL`), not with `RegisterHotKey`. The hook sees each key before other apps. The Talk Key starts when its main key and exactly its modifiers are down, in any order, and it ends when the main key goes up. While LiveTyper uses the main key, the hook removes it from the input. It also removes Escape during a Dictation. Modifier keys always go on to the other apps.

`RegisterHotKey` fires only when the main key goes down while the modifiers are already down. In fast back-to-back Dictations, the user often pushed Space a moment before Shift. Windows then saw Ctrl+Space, and another app with that shortcut got it. In one test, 20 of 36 pushes did not reach LiveTyper. Also, `RegisterHotKey` cannot take Ctrl+Shift+Escape (Windows uses it for Task Manager), so Escape did not cancel while the user held Ctrl+Shift+Space.

## Considered Options

- `RegisterHotKey` (through `global-hotkey`). Rejected for the reasons above. LiveTyper still uses `global-hotkey` to read the `talk_key` setting.

## Consequences

- An app that has the same shortcut no longer gets it while LiveTyper runs.
- If the user pushes the main key before the last modifier, the other apps see that main key with the first modifiers for a moment. For example, Ctrl+Space goes on to the other apps before Shift completes Ctrl+Shift+Space.
- LiveTyper marks the paste keys that it sends, and the hook ignores them. Keys that other programs send (a test script, a remote desktop) count as the user's keys.
- The hook must return fast: it only posts events to the UI thread. Windows removes a hook that is too slow.
