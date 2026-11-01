# Survey of open-source local dictation apps and Rust crates

Research date: 2026-11-01. Handy and OpenWhispr are in a separate file. Method: GitHub metadata, READMEs, code and issues, and the crates.io API. Nothing was installed or run, so quality claims come from docs and code only.

## Apps

| App | License / activity | Platforms, stack, engine | Live text while speaking | Insert / idle / cloud | Ideas to take |
|---|---|---|---|---|---|
| [QuillKey](https://github.com/alivesan/quillkey) | No license file; 0 stars; early | Windows; Electron + NeMo-Speech.cpp + Nemotron on CUDA (GPU only) | Yes; non-activating overlay with streaming partials | Paste; keeps all clipboard formats, restores after 600 ms only if the sequence number did not change; refuses paste if focus changed or the target is a password field (UI Automation) | Nearest match: target validation, safe clipboard restore |
| [Voltip](https://github.com/sunerpy/voltip) | AGPL-3.0; v0.2.1 (2026-10-29) | Rust/Tauri; streaming Zipformer preview, then Qwen3-ASR or SenseVoice/Paraformer final | Yes; two models, like our plan | Default release uses a cloud service | Floating pill with level, live text and result; output as block, by sentence, or from the stream |
| [Hex](https://github.com/anomalyco/hex) | MIT; active; Windows beta | Rust, WASAPI, transcribe.cpp; Vulkan GPU on Windows x64 | No live drafts on Windows | Generation-safe clipboard restore; click-through HUD; swallows the chord; "release mic while idle" | Double-tap lock, Paste Last Dictation, Escape cancels, deadline-bounded stream with batch fallback; [Windows doc](https://github.com/anomalyco/hex/blob/main/docs/windows.md) |
| [VoxType](https://github.com/zkwi/VoxType) | MIT; Rust/Tauri; Windows | Cloud ASR | Live captions | Final-result gate: captions are feedback only; paste waits for the final event | The gate rule |
| [WinSTT](https://github.com/dahshury/WinSTT) | MIT; 34 stars | Windows; Tauri + `ort`; optional NeMo-Speech CPU runtime | Real-time preview | Needs AVX2 | Reuse the finished stream as the final text |
| [VoiceInk](https://github.com/Beingpax/VoiceInk) | NOASSERTION; v2.21 | macOS; whisper.cpp, FluidAudio (Parakeet) | Yes (mostly cloud streaming) | Skips restore if the clipboard changed; transient pasteboard | Committed segments vs tail; batch fallback |
| [FluidVoice](https://github.com/altic-dev/FluidVoice) | GPL-3.0; macOS | Nemotron 3.5, Parakeet realtime EOU | Yes | Optional analytics | Overlay size setting |
| [Buzz](https://github.com/chidiwilliams/buzz) | MIT | Python/Qt; Whisper | Re-transcribes 5 s batches, cut at silence | No paste | Cut chunks at low-RMS points |
| [Vibe](https://github.com/thewh1teagle/vibe) | MIT; v3.3.0 | Tauri; whisper.cpp, Parakeet, Nemotron 3.5 | File transcriber | – | In-repo `nemotron-rs` (GGML Nemotron streaming in Rust, not on crates.io) |
| [OmniDictate](https://github.com/gurjar1/OmniDictate) | CC BY-NC 4.0 | Windows; Python, faster-whisper | Phrase by phrase | Types into the app | – |
| [WhisperWriter](https://github.com/savbell/whisper-writer) | GPL-3.0; stale | Python, faster-whisper | No | Types with pynput | Four recording modes |
| [Whispering / Epicenter](https://github.com/EpicenterHQ/epicenter) | NOASSERTION | Now a voice-notes app; no dictation into other apps | No | – | – |
| [nerd-dictation](https://github.com/ideasman42/nerd-dictation) | GPL-3.0; Linux; Vosk | – | Types partials into the target and fixes them with backspaces | – | Shows why not to type partials into the target |

Others on Windows: [Chirp](https://github.com/Whamp/chirp), [Murmure](https://github.com/Kieirra/murmure) (AGPL, no telemetry), [VoiceFlow](https://github.com/infiniV/VoiceFlow), [SayIt](https://github.com/crosswk/SayIt), [CapsWriter-Offline](https://github.com/HaujetZhao/CapsWriter-Offline), [Glimpse](https://github.com/glimpse-hq/Glimpse) (PostHog telemetry).

Reference code:
- sherpa-onnx [two-pass microphone example](https://github.com/k2-fsa/sherpa-onnx/blob/master/python-api-examples/two-pass-speech-recognition-from-microphone.py): streaming first pass, non-streaming second pass.
- sherpa-onnx Rust example `parakeet_tdt_simulate_streaming_microphone.rs` ([examples](https://github.com/k2-fsa/sherpa-onnx/tree/master/rust-api-examples/examples)): cpal + Silero VAD, re-decodes the open VAD segment every 0.2 s.
- [whisper_streaming](https://github.com/ufal/whisper_streaming) is replaced by [SimulStreaming](https://github.com/ufal/SimulStreaming).

## Crates

| Crate | Status | Windows | Use |
|---|---|---|---|
| `sherpa-onnx` 2.0.1 (official) | Active | Yes, static linking | Streaming Nemotron/Zipformer, offline Parakeet, Silero VAD. `sherpa-rs` is deprecated. |
| `parakeet-rs` 0.4.4 | Very active | Yes (`ort`, optional DirectML) | Nemotron streaming, EOU 120M, unified offline + streaming |
| `transcribe-rs` 0.3.12 | Active | Yes | Offline Parakeet, Moonshine, Whisper |
| `whisper-rs` 0.16 | Moved to Codeberg | Yes | Only for Whisper |
| NeMo-Speech.cpp bindings | None on crates.io | – | Own FFI needed |
| `cpal` 0.18.2 | Active | WASAPI | Default device fails after a device change ([#1402](https://github.com/RustAudio/cpal/issues/1402)); no device notifications ([#373](https://github.com/RustAudio/cpal/issues/373)). `wasapi` 0.26 gives direct control. |
| `earshot` 1.2.2, `voice_activity_detector` 0.2.1 | Active | Yes | VAD. `webrtc-vad` unmaintained. |
| `handy-keys` 0.3.4 (MIT) | Active | `WH_KEYBOARD_LL`; blocks hotkeys; modifier-only hotkeys | Hold-to-talk that swallows keys |
| `win-hotkeys` 0.5.1 | Last push 2025-05 | LL hook | Alternative |
| `global-hotkey` 0.9.0 | Active | RegisterHotKey; no modifier-only hotkeys | Simple chords only |
| `rdev` | No release since 2023; Windows `grab` bug ([#123](https://github.com/Narsil/rdev/issues/123)) | – | Avoid |
| `enigo` 0.6.1 | Active repo | SendInput | Send Ctrl+V (raw SendInput also easy) |
| `arboard` 3.7.0 | Active | `exclude_from_history/cloud/monitoring`; text and images only | Write our text |
| `clipboard-win` 5.4.3 | Active | Raw formats | Full snapshot of all formats |
| `windows` | – | – | Overlay styles: `WS_EX_NOACTIVATE`, `WS_EX_TOPMOST`, `WS_EX_TOOLWINDOW`, optional `WS_EX_TRANSPARENT` |

## Summary

1. Few apps do local live preview with correction on Windows (QuillKey: GPU only, early; Voltip: cloud by default; WinSTT: very small). No app does the full LiveTyper design. Quality of all of them is unverified.
2. Patterns: keep committed text separate from the changing tail; freeze each VAD segment when it ends and re-decode only the open segment; final-result gate; deadline with batch fallback on release; record the target at key-down and check before paste; refuse password fields; Escape cancels; double-tap lock; Paste Last Dictation; click-through overlay that never takes focus; release the mic when idle (excludes a pre-roll buffer).
3. Crates: `sherpa-onnx` and/or `parakeet-rs`; `cpal` or `wasapi`; `earshot` or sherpa VAD; `handy-keys`; `arboard` + `clipboard-win`; `enigo` or raw SendInput.
4. Windows pitfalls: clipboard races ([Murmure #418](https://github.com/Kieirra/murmure/issues/418)) — restore only if `GetClipboardSequenceNumber` is unchanged, after 300–600 ms, and exclude from Win+V history; overlay focus stealing ([Handy #315](https://github.com/cjpais/Handy/issues/315), [#2070](https://github.com/cjpais/Handy/issues/2070)); UIPI blocks SendInput into elevated windows; Ctrl+Space clashes with IMEs and AltGr arrives as Ctrl+Alt ([Murmure #444](https://github.com/Kieirra/murmure/issues/444), [#79](https://github.com/Kieirra/murmure/issues/79)); LL hook callbacks must return quickly; Defender and SmartScreen flag unsigned builds ([Handy #1891](https://github.com/cjpais/Handy/issues/1891), [OpenWhispr #1096](https://github.com/OpenWhispr/openwhispr/issues/1096)); prebuilt ORT/ggml needs AVX2 ([OpenWhispr #2356](https://github.com/OpenWhispr/openwhispr/issues/2356)); Bluetooth mics need exclusive mode off ([OpenWhispr #1138](https://github.com/OpenWhispr/openwhispr/issues/1138)); default-device changes break cpal; mic start can take 1–2 s.

## Not verified

NOASSERTION licenses (Epicenter, VoiceInk, Moonshine); real CPU latency and idle RAM of each app; live-preview quality; Amical and Speech Note behavior; SimulStreaming license.
