# LiveTyper

LiveTyper is a local dictation app for Windows, with a KDE Wayland preview for Linux. You speak, and it types your words into any text box. It shows each word while you speak.

LiveTyper is 100% local. It makes no network connections while it runs: no cloud speech service, no telemetry and no update checks ([ADR 0001](docs/adr/0001-no-network-at-runtime.md)). The model download is a separate one-time step that you start.

## How a Dictation works

1. Put the caret in a text box. This box becomes the Target Text Box.
2. Hold the Talk Key (default Ctrl+Space; see `talk_key` in [Settings](#settings)). Speak.
3. The Scratch Pad shows the Draft while you speak. The Scratch Pad never takes the focus.
4. New words show in grey. These are Unchecked Words. About 1.5 s after you speak a word, a Recast checks it, and it changes to white. A white word does not change again while you speak.
5. Release the Talk Key. LiveTyper pastes the Draft into the Target Text Box.

Other behavior:

- **Start of a word before the push:** LiveTyper keeps the last 3 s of microphone audio in memory. A Dictation also gets the start of a word that you began a moment before you pushed the Talk Key, back to the last short pause (at most 1.5 s). It never gets words of the last Dictation.
- **End of the last word after the release:** if you still speak when you release the Talk Key, LiveTyper records until the word ends (at most 0.4 s).
- **Follow-up Dictation:** you can push the Talk Key again at once, also while the last Dictation still pastes. The Scratch Pad shows at once and the microphone records; the words show after the last paste. A Follow-up Dictation into the same window within 30 s starts with a space.
- **Key order:** the keys of the Talk Key work in any order. While LiveTyper uses the Talk Key, other apps do not get it.
- **Cancel:** push Escape during a Dictation, also while you hold the Talk Key. LiveTyper pastes nothing.
- **Short tap:** a Talk Key push shorter than 300 ms does nothing.
- **"Not pasted. Push Ctrl+V.":** some windows do not accept the paste (for example, a window that runs as administrator). The Draft then stays on the clipboard. Push Ctrl+V to paste it.
- **Paste Last Dictation:** this tray menu item pastes the last Draft again. LiveTyper keeps this text in memory only.
- **Terminals:** LiveTyper pastes with Ctrl+Shift+V in terminal windows.
- **First Dictation:** the models load on the first Dictation. The Scratch Pad shows "loading models…". LiveTyper keeps the audio, so you lose no words.
- **Idle:** LiveTyper removes the models from memory after 10 minutes without a Dictation.

The tray icon shows the state:

| Icon color | State |
|---|---|
| White | Idle |
| Red | Recording: you hold the Talk Key |
| Amber | Finishing: the last Recast and the paste |

The tray menu has these items: Paste Last Dictation, Open settings, Open log, Quit.

### Privacy

- With the default `microphone_open = "always"`, the microphone is open all the time while LiveTyper runs, and Windows shows the microphone icon. The audio stays in memory only: LiveTyper keeps the last 3 s, and decodes nothing before you push the Talk Key. Set `microphone_open` to `"after_use"` or `"dictation"` to change this ([ADR 0009](docs/adr/0009-open-microphone-with-pre-roll.md)).
- The log never contains dictated text.
- The paste puts the Draft on the clipboard for a moment, and then puts back what was on the clipboard before. Windows clipboard history does not keep the Draft.

## Install on a PC

LiveTyper has no installer. The app is one file, `livetyper.exe` (about 4 MB), and it writes its settings file itself on the first start.

### Requirements

- Windows 10 or Windows 11, x64.
- A CPU with AVX2. The Minimum Device is an x86-64 laptop from 2020 or newer, with 4 CPU cores, 8 GB RAM and integrated graphics only.
- Approximately 2.3 GB of RAM while the models are in memory.
- 1.5 GB of disk space for the two models.
- The [Microsoft Visual C++ Redistributable (x64)](https://aka.ms/vs/17/release/vc_redist.x64.exe). Many PCs already have it.

### Steps

1. Get `livetyper.exe`: build it ([Build from source](#build-from-source)), or copy it from a PC that built it (`target\release\livetyper.exe`).
2. Put it in a folder that is not under a folder named `target`, for example `%LOCALAPPDATA%\Programs\LiveTyper\`. (LiveTyper does not register a build in Cargo's `target` folder to start with Windows.)
3. Download the models one time (see [Model setup](#model-setup-one-time)). Copy `scripts\download-models.ps1` to the PC for this step, or copy the `models` folder from another PC.
4. Start `livetyper.exe`. The tray icon shows. With `start_with_windows = true` (the default), LiveTyper starts again when you sign in to Windows.
5. Use Open settings in the tray menu to change the Talk Key or other settings, then restart LiveTyper (Quit in the tray menu, then start it again).

To remove LiveTyper: set `start_with_windows = false` and start LiveTyper one time (or delete the `LiveTyper` value in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`), quit it, and delete the exe and the folders in [Files](#files).

## Build from source

1. Install [rustup](https://rustup.rs). Set the MSVC toolchain as the default:

   ```
   rustup default stable-x86_64-pc-windows-msvc
   ```

   If another Rust (for example the scoop `rust-gnu` package) is on the PATH, put `%USERPROFILE%\.cargo\bin` first.

2. Install Visual Studio Build Tools 2019 or newer with the "Desktop development with C++" workload. This workload includes CMake.
3. Make sure CMake is on PATH in the Visual Studio developer command prompt. If needed, set the `CMAKE` environment variable to the full path to `cmake.exe`.
4. Build the app from PowerShell or a Command Prompt (not from Git Bash or MSYS2: there, CMake selects the wrong compiler):

   ```
   cargo build --release
   ```

The app is `target\release\livetyper.exe`. The first build also builds transcribe-cpp. On the dev PC, it takes approximately 1.5 minutes.

`.cargo/config.toml` also sets the CPU floor for the build (AVX2, not the CPUs of the build PC). Thus, the exe also runs on other PCs.

## CachyOS and KDE Wayland preview

LiveTyper uses the same Nemotron and Parakeet models on Windows and Linux. On KDE Wayland the Scratch Pad shows the live draft, checked words, and a segmented microphone peak meter without taking keyboard focus.

1. Install a stable Rust toolchain, CMake, a C/C++ toolchain, pkg-config, ALSA, GTK4, gtk4-layer-shell, and ydotool. The preview targets x86-64 with AVX2 and a Wayland compositor that supports layer shell.
2. Start a user-accessible `ydotoold` service. The ydotool client uses `YDOTOOL_SOCKET` if set, or its standard session socket. The account also needs read access to the physical keyboard's evdev device. The preview observes the configured Talk Key and Escape without grabbing the device from the desktop.
3. Download the models and build the app.

   ```sh
   bash scripts/download-models.sh
   cargo build --release --locked
   ```

4. Run `target/release/livetyper` from a terminal. Hold the Talk Key while a text field is focused. If Voxtype is also running, give LiveTyper its own chord, for example `talk_key = "Ctrl+Shift+F9"`, then restart LiveTyper. Use Escape to cancel a recording and Ctrl+C in the launch terminal to exit.

Linux settings are in `${XDG_CONFIG_HOME:-~/.config}/livetyper/settings.toml`. Models and the log are in `${XDG_DATA_HOME:-~/.local/share}/livetyper/`. The Scratch Pad is anchored at the bottom of the screen.

The preview sends basic text through ydotool to the currently focused app after the modifiers are released. Keep focus in the intended text field through insertion. For text the keyboard driver cannot enter or when the driver reports an error, LiveTyper copies the draft through Plasma’s clipboard and prompts you to paste it. Clipboard history can retain that text. Automatic keyboard insertion requires the plain US layout. The Linux preview runs from the terminal and uses the desktop's shortcut handling alongside its Talk Key.

## Model setup (one time)

Run this script one time:

```
powershell -ExecutionPolicy Bypass -File scripts\download-models.ps1
```

The script downloads two files from Hugging Face into `%LOCALAPPDATA%\LiveTyper\models`:

| File | Size | Use |
|---|---|---|
| `nemotron-speech-streaming-en-0.6b-Q8_0.gguf` | 730 MB | Live words |
| `parakeet-tdt-0.6b-v3-Q8_0.gguf` | 740 MB | Recasts |

The script checks the SHA-256 value of each file. It continues a download that stopped. This script is the only step that uses the network.

To use a different folder, add `-ModelsDir <folder>`. Then set `models_dir` in the settings to the same folder.

## Settings

The settings file is `%APPDATA%\LiveTyper\settings.toml`. LiveTyper writes this file with the default values when it does not exist. To open it, use Open settings in the tray menu. Restart LiveTyper after a change.

A key that is not in the file takes its default value. An unknown key or an incorrect value is an error. Then LiveTyper uses all default values, shows a message, and writes the error to the log.

| Key | Default | Meaning |
|---|---|---|
| `talk_key` | `"Ctrl+Space"` | The Talk Key. Examples: `"Ctrl+Space"`, `"Ctrl+Shift+Space"`, `"Ctrl+Alt+D"`, `"F9"`. LiveTyper takes this key before other apps, so an app that uses the same shortcut (for example PowerToys Peek, or a keyboard language switch) does not get it while LiveTyper runs. |
| `scratch_pad_x` | not set | Scratch Pad position: the x value of the center of its bottom edge, in screen pixels. |
| `scratch_pad_y` | not set | Scratch Pad position: the y value of the center of its bottom edge, in screen pixels. Without `scratch_pad_x` and `scratch_pad_y`, the Scratch Pad shows at the bottom center of the main screen. |
| `scratch_pad_width` | `640` | Scratch Pad width, in pixels. |
| `microphone` | `""` | The microphone name, as Windows shows it. Empty = the Windows default microphone. |
| `microphone_open` | `"always"` | When the microphone is open. `"always"`: all the time while LiveTyper runs. `"after_use"`: from a Dictation until `microphone_open_after_use_min` minutes after the last one; use this with a Bluetooth headset, which sounds worse while its microphone is open. `"dictation"`: only while you hold the Talk Key; then a Dictation does not get the start of a word before the push or the end of a word after the release. |
| `microphone_open_after_use_min` | `5` | For `"after_use"`: the minutes without a Dictation after which the microphone closes. Minimum `1`. |
| `models_dir` | `""` | The folder with the model files. Empty = `%LOCALAPPDATA%\LiveTyper\models`. |
| `unload_after_min` | `10` | Remove the models from memory after this number of minutes without a Dictation. `0` = never. |
| `lookahead_ms` | `480` | The lookahead of the live model, in ms. Use `0`, `80`, `480` or `1040`. Less lookahead shows words sooner, but uses more CPU. |
| `recast_interval_ms` | `500` | The time between two Recasts, in ms. |
| `recast_window_s` | `15` | The length of the latest audio that each Recast decodes, in s. Minimum `2`. Words before this window do not change again. |
| `release_deadline_ms` | Windows `2000`, Linux `10000` | After you release the Talk Key, the maximum time to wait for the last Recast, in ms. Then LiveTyper pastes the current Draft. |
| `min_hold_ms` | `300` | A shorter Talk Key push, in ms, does nothing. |
| `live_threads` | `0` | CPU threads for the live model. `0` = automatic. |
| `recast_threads` | `0` | CPU threads for the Recast model. `0` = automatic. |
| `start_with_windows` | `true` | Start LiveTyper when you sign in to Windows. |

## Files

| Path | Contents |
|---|---|
| `%APPDATA%\LiveTyper\settings.toml` | The settings. |
| `%LOCALAPPDATA%\LiveTyper\livetyper.log` | Errors, events and timings. The log never contains dictated text. At start-up, a log larger than 1 MB moves to `livetyper.old.log`. |
| `%LOCALAPPDATA%\LiveTyper\models\` | The model files (default folder). |
| `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | The `start_with_windows` value. |

Each Dictation writes one line to the log: the audio length, where the speech starts, where the first word showed, the number of words, the model load time, the Recast times, and the paste result. The lines "pre-roll" and "post-roll" give the audio that a Dictation got from before the push and after the release.

## Testing

Run the unit tests. They need no models and no microphone:

```
cargo test
```

Run the tests that need the models (download the models first) or the real microphone. A test that needs the models and does not find them writes "SKIP" and passes:

```
cargo test -- --ignored --nocapture
```

Some of these tests in detail:

- **Live writing test:** plays a 30 s clip into the engine at the speed of speech, and checks that the words show one or two at a time, that the Recasts check them soon, and that no checked (white) word changes before the release. It writes a video of the Scratch Pad with the audio, and a timeline, into `target\live-writing\` (the video needs `ffmpeg` on the PATH).

  ```
  cargo test live_writing -- --ignored --nocapture
  ```

- **Short Dictations:** how soon the first word of a short Dictation shows, and its final text.

  ```
  cargo test short_dictations -- --ignored --nocapture
  ```

- **Microphone:** how the real microphone starts, and the three `microphone_open` values with the real microphone. No audio is saved.

  ```
  cargo test microphone_ -- --ignored --nocapture
  ```

Make test audio with the Windows built-in voice. The script writes 16 kHz mono WAV files into `tests\fixtures\`:

```
powershell -ExecutionPolicy Bypass -File scripts\make-test-audio.ps1
```

Test the full app without speaking. Set `LIVETYPER_TEST_AUDIO` to a 16 kHz mono 16-bit WAV file. During a Dictation, LiveTyper then plays this file in place of the microphone:

```
$env:LIVETYPER_TEST_AUDIO = "tests\fixtures\tts-15s.wav"
cargo run --release
```

`scripts\e2e-test.ps1` tests the release build end to end with real key events: it starts LiveTyper with `LIVETYPER_TEST_AUDIO`, opens a Notepad, holds the Talk Key, and reads the pasted text. Quit a running LiveTyper first. See the help at the top of the script.

Measure the model speed with the spike example. The results of the first run are in [docs/research/2026-11-01-spike-results.md](docs/research/2026-11-01-spike-results.md):

```
cargo run --release --example spike -- --right 6 --live-threads 1 --recast-threads 3 tests\fixtures\tts-15s.wav
```

## Design decisions

- [ADR 0001: No network connections at runtime](docs/adr/0001-no-network-at-runtime.md)
- [ADR 0002: A streaming model for live words, and a second model for the recast loop](docs/adr/0002-streaming-model-plus-recast-loop.md)
- [ADR 0003: Paste the final Draft; do not type live into the Target Text Box](docs/adr/0003-paste-final-draft-not-live-keystrokes.md)
- [ADR 0004: NeMo-Speech.cpp as the speech runtime](docs/adr/0004-nemo-speech-cpp-runtime.md) (superseded)
- [ADR 0005: transcribe-cpp as the speech runtime](docs/adr/0005-transcribe-cpp-runtime.md)
- [ADR 0006: Approximate times for live words](docs/adr/0006-approximate-times-for-live-words.md) (partly replaced by ADR 0007)
- [ADR 0007: A Recast checks only settled audio](docs/adr/0007-recast-checks-only-settled-audio.md)
- [ADR 0008: The Talk Key through a low-level keyboard hook](docs/adr/0008-talk-key-through-a-keyboard-hook.md)
- [ADR 0009: The microphone stays open, with a pre-roll and a post-roll](docs/adr/0009-open-microphone-with-pre-roll.md)

The words of the project are in [GLOSSARY.md](GLOSSARY.md).

## Credits and licenses

- [transcribe-cpp](https://crates.io/crates/transcribe-cpp) (MIT): the speech runtime.
- [Handy](https://github.com/cjpais/Handy) (MIT): `src/paste.rs` is a port of its reliable paste algorithm for Windows.
- [OpenWhispr](https://github.com/OpenWhispr/openwhispr) (MIT): `src/terminals.rs` uses its lists of terminal window classes and exe names.
- NVIDIA Nemotron Speech Streaming EN 0.6B: NVIDIA Open Model License.
- NVIDIA Parakeet TDT 0.6B v3: CC-BY-4.0.
- The GGUF files of the two models are conversions by the Handy project (`handy-computer` on Hugging Face).
