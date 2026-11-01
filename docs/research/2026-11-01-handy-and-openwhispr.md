# Handy and OpenWhispr compared with the LiveTyper design

Research date: 2026-11-01.

Short answer from the research: do not fork either app. Build LiveTyper as our own Rust/Win32 app and reuse Handy's crates, most of all **transcribe-cpp** (MIT). It already runs both of our models in GGUF form on CPU: Nemotron Speech Streaming EN 0.6B with real streaming, and Parakeet TDT 0.6B v3 in batch mode. It can replace NeMo-Speech.cpp or sherpa-onnx.

## Handy

- Repo: https://github.com/cjpais/Handy. MIT. About 36k stars. Last release v0.9.8 (2026-10-03). Windows x64/ARM64, macOS, Linux; Windows is first-class.
- Stack: Rust + Tauri 2 + React in WebView2. Audio `handy-recorder` (WASAPI). VAD: Silero v6.2 GGUF through transcribe.cpp, plus `earshot`. Hotkeys `handy-keys` (`HotkeyManager::new_with_blocking`; key swallowing not verified). Settings in `settings_store.json`. Models from Hugging Face (`handy-computer/*-gguf`) with SHA-256 checks ([catalog.json](https://github.com/cjpais/Handy/blob/main/src-tauri/src/catalog/catalog.json)).
- Text insertion ([clipboard.rs](https://github.com/cjpais/Handy/blob/main/src-tauri/src/clipboard.rs)): CtrlV, CtrlShiftV, ShiftInsert, Direct (enigo), None, or a script. One paste key for all apps. Optional "reliable paste" in [paste_tx/windows.rs](https://github.com/cjpais/Handy/blob/main/src-tauri/src/paste_tx/windows.rs): delayed clipboard rendering, waits for `WM_RENDERFORMAT` as a receipt, restores the full clipboard, and uses `GetClipboardSequenceNumber` to never overwrite a newer user copy. Off by default.
- Overlay ([overlay.rs](https://github.com/cjpais/Handy/blob/main/src-tauri/src/overlay.rs)): WebView2 window, `HWND_TOPMOST` + `SWP_NOACTIVATE`. Styles None / Minimal / Live.
- Engines: since v0.9.0 all models run through [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp) (MIT): Whisper, Parakeet, Nemotron Streaming EN, Nemotron 3.5, Moonshine, Canary, Qwen3-ASR and more. CPU on all platforms, Vulkan on Windows x64, CUDA/ROCm from source.
- Live text: yes, since v0.9.0. Streaming models show committed and tentative text ([live.rs](https://github.com/cjpais/Handy/blob/main/src-tauri/src/managers/transcription/live.rs), [policy.rs](https://github.com/cjpais/Handy/blob/main/src-tauri/src/managers/transcription/policy.rs)). If the stream fails, it decodes the full recording again in batch. **No second recast model**: at most one model worker at a time.
- Idle: unload timeout Never / Immediately / 2 / 5 (default) / 10 / 15 / 60 min. On `main`, [engine_supervisor](https://github.com/cjpais/Handy/blob/main/src-tauri/src/engine_supervisor/mod.rs) runs inference in a child process, so unload returns all memory. Measured: about 0.4% of one core idle ([#1371](https://github.com/cjpais/Handy/issues/1371)); WebView2 renderer at about 92% of one core with the lid closed (#1371); WebView2 timer-resolution churn while locked ([#2162](https://github.com/cjpais/Handy/issues/2162)).
- Network: update checks on by default (can be turned off); model downloads; optional LLM post-processing. No telemetry found.
- Windows issues: freeze with admin target apps ([#434](https://github.com/cjpais/Handy/issues/434)); hotkey lost after lock/unlock ([#1327](https://github.com/cjpais/Handy/issues/1327)); Defender false positive ([#1891](https://github.com/cjpais/Handy/issues/1891)); WebView2 start failure ([#2320](https://github.com/cjpais/Handy/issues/2320)); old clipboard pasted instead of the text ([#502](https://github.com/cjpais/Handy/issues/502)).

## OpenWhispr

- Repo: https://github.com/OpenWhispr/openwhispr. MIT. About 10.5k stars. v1.11.0 (2026-10-29). Windows, macOS, Linux, mobile.
- Stack: Electron, React 19, Node, better-sqlite3. Native C helpers: [windows-key-listener.c](https://github.com/OpenWhispr/openwhispr/blob/main/resources/windows-key-listener.c) (`WH_KEYBOARD_LL`, does not swallow the key); [windows-fast-paste.c](https://github.com/OpenWhispr/openwhispr/blob/main/resources/windows-fast-paste.c) (SendInput; Ctrl+Shift+V for terminals by window class or exe name; brings the target window to the front). Clipboard restore after a fixed 500 ms ([clipboard.js](https://github.com/OpenWhispr/openwhispr/blob/main/src/helpers/clipboard.js)).
- Engines: whisper.cpp server, sherpa-onnx 1.13.4 WebSocket sidecar ([parakeetWsServer.js](https://github.com/OpenWhispr/openwhispr/blob/main/src/helpers/parakeetWsServer.js)) for Parakeet v3 and Nemotron, llama.cpp, and many cloud providers.
- Live text: yes ([blog post](https://openwhispr.com/blog/local-streaming-speech-to-text), v1.7.6). Nemotron INT8 streaming partials, replaced in place; chunk 80 ms to 1.12 s. No second decode; one batch decode only if the stream fails. Stream WER 6.93%, best batch 5.91%.
- Idle: the speech server is pre-warmed at startup; no idle unload for speech-to-text.
- Network: auto-updater, sign-in, cloud sync, OAuth connectors, consent-gated analytics, server-controlled experiments. Proving "zero network" would be hard.
- Windows issues: paste over about 200 characters fails silently ([#829](https://github.com/OpenWhispr/openwhispr/issues/829)); whisper-server crashes without AVX2 ([#2356](https://github.com/OpenWhispr/openwhispr/issues/2356)) and with 0xC0000409 ([#1208](https://github.com/OpenWhispr/openwhispr/issues/1208)); Defender false positives ([#318](https://github.com/OpenWhispr/openwhispr/issues/318), [#1096](https://github.com/OpenWhispr/openwhispr/issues/1096)); hotkey state bugs ([#364](https://github.com/OpenWhispr/openwhispr/issues/364)); Right Alt hotkey made Chrome lose focus ([#2983](https://github.com/OpenWhispr/openwhispr/issues/2983)).

## Reusable parts

| Item | Use | Changes we need |
|---|---|---|
| transcribe-cpp 0.4.x ([Rust bindings](https://github.com/handy-computer/transcribe.cpp/tree/main/bindings/rust/transcribe-cpp), [streaming example](https://github.com/handy-computer/transcribe.cpp/blob/main/bindings/rust/transcribe-cpp/examples/streaming.rs)) | Nemotron streaming (`stream()`, `feed()`, `finalize()`) with committed and tentative text; lookahead 0 to 1040 ms. Parakeet v3 offline for each recast. | CPU-only build (no `vulkan` feature) to stay small. Own recast and merge logic. Both models loaded: about 1.2–1.5 GB at Q8. |
| `handy-computer` GGUF models on Hugging Face | Nemotron EN Q8_0 730 MB; Parakeet TDT v3 740 MB | NVIDIA Open Model License. One download. |
| Measured CPU speed (Ryzen 7 4750U laptop, Nemotron EN) | 743 ms for 11 s audio (14.8× real time); streaming latency p50 about 206 ms | Close to the Minimum Device. |
| handy-recorder (MIT, WASAPI, 16 kHz mono f32) | Mic capture and level bar | Young (0.1.0). |
| handy-keys (MIT) | Hold-to-talk with blocking | Check key suppression; a raw `WH_KEYBOARD_LL` hook is an alternative. |
| `paste_tx/windows.rs` | Receipt-based paste and clipboard restore | Remove Tauri and enigo; pure Win32. |
| `engine_supervisor/` | Child-process inference; unload returns all memory | Optional; must hold 2 models. |
| Terminal lists in `windows-fast-paste.c` | Ctrl+Shift+V for terminals | Copy the class and exe lists. |

## Assessment

- Fork: no. Handy depends on Tauri/WebView2, allows one model at a time, costs more when idle, and its overlay is a web window. OpenWhispr is a large Electron app with cloud features, no idle unload and no recast.
- Build our own and reuse crates: yes. transcribe-cpp for both models, handy-recorder, Handy's receipt-based paste, OpenWhispr's terminal rules. Own Win32 Scratch Pad, tray and idle unload.
- Learn: batch fallback when the stream fails; flush padding for the last chunk; 10 s finalize idle timeout; the Windows issues above.

## Not verified

RAM for both models together on CPU; OpenWhispr idle RAM; handy-keys key suppression; transcribe-cpp CPU-only build with MSVC on Windows; Handy process isolation is only on `main`.
