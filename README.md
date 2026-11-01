# LiveTyper

LiveTyper is a local Windows app for Dictation. You speak, and it types your words into any text box. It shows each word while you speak.

LiveTyper is 100% local. It makes no network connections while it runs: no cloud speech service, no telemetry and no update checks ([ADR 0001](docs/adr/0001-no-network-at-runtime.md)). The model download is a separate one-time step that you start.

## How a Dictation works

1. Put the caret in a text box. This box becomes the Target Text Box.
2. Hold the Talk Key (Ctrl+Space). Speak.
3. The Scratch Pad shows the Draft while you speak. The Scratch Pad never takes the focus.
4. New words show in grey. These are Unchecked Words. After a Recast checks them, they change to white.
5. Release the Talk Key. LiveTyper pastes the Draft into the Target Text Box.

Other behavior:

- **Cancel:** push Escape during a Dictation. LiveTyper pastes nothing.
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

## Requirements

- Windows 10 or Windows 11, x64.
- A CPU with AVX2. The Minimum Device is an x86-64 laptop from 2020 or newer, with 4 CPU cores, 8 GB RAM and integrated graphics only.
- Approximately 2.3 GB of RAM while the models are in memory.
- 1.5 GB of disk space for the two models.
- The Microsoft Visual C++ Redistributable (x64).

## Build from source

1. Install [rustup](https://rustup.rs). Set the MSVC toolchain as the default:

   ```
   rustup default stable-x86_64-pc-windows-msvc
   ```

2. Install Visual Studio Build Tools 2019 with the "Desktop development with C++" workload. This workload includes CMake.
3. Make sure that the `CMAKE` path in `.cargo/config.toml` is correct for your PC. Change it if your Build Tools are in a different folder.
4. Build the app:

   ```
   cargo build --release
   ```

The app is `target\release\livetyper.exe`. The first build also builds transcribe-cpp. On the dev PC, it takes approximately 1.5 minutes.

`.cargo/config.toml` also sets the CPU floor for the build (AVX2, not the CPUs of the build PC). Thus, the exe also runs on other PCs.

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
| `talk_key` | `"Ctrl+Space"` | The Talk Key. Examples: `"Ctrl+Space"`, `"Ctrl+Alt+D"`, `"F9"`. |
| `scratch_pad_x` | not set | Scratch Pad position: the x value of the center of its bottom edge, in screen pixels. |
| `scratch_pad_y` | not set | Scratch Pad position: the y value of the center of its bottom edge, in screen pixels. Without `scratch_pad_x` and `scratch_pad_y`, the Scratch Pad shows at the bottom center of the main screen. |
| `scratch_pad_width` | `640` | Scratch Pad width, in pixels. |
| `microphone` | `""` | The microphone name, as Windows shows it. Empty = the Windows default microphone. |
| `models_dir` | `""` | The folder with the model files. Empty = `%LOCALAPPDATA%\LiveTyper\models`. |
| `unload_after_min` | `10` | Remove the models from memory after this number of minutes without a Dictation. `0` = never. |
| `lookahead_ms` | `480` | The lookahead of the live model, in ms. Use `0`, `80`, `480` or `1040`. Less lookahead shows words sooner, but uses more CPU. |
| `recast_interval_ms` | `500` | The time between two Recasts, in ms. |
| `recast_window_s` | `15` | The length of the latest audio that each Recast decodes, in s. Minimum `2`. Words before this window do not change again. |
| `release_deadline_ms` | `2000` | After you release the Talk Key, the maximum time to wait for the last Recast, in ms. Then LiveTyper pastes the current Draft. |
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

## Testing

Run the unit tests. They need no models and no microphone:

```
cargo test
```

Run the tests that need the models. Download the models first:

```
cargo test -- --ignored
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
- [ADR 0006: Approximate times for live words](docs/adr/0006-approximate-times-for-live-words.md)

The words of the project are in [GLOSSARY.md](GLOSSARY.md).

## Credits and licenses

- [transcribe-cpp](https://crates.io/crates/transcribe-cpp) (MIT): the speech runtime.
- [Handy](https://github.com/cjpais/Handy) (MIT): `src/paste.rs` is a port of its reliable paste algorithm for Windows.
- [OpenWhispr](https://github.com/OpenWhispr/openwhispr) (MIT): `src/terminals.rs` uses its lists of terminal window classes and exe names.
- NVIDIA Nemotron Speech Streaming EN 0.6B: NVIDIA Open Model License.
- NVIDIA Parakeet TDT 0.6B v3: CC-BY-4.0.
- The GGUF files of the two models are conversions by the Handy project (`handy-computer` on Hugging Face).
