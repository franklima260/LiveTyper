# M0 speed spike results (dev PC)

Date: 2026-11-01.

**Setup**
- Tool: `examples/spike.rs`.
- Runtime: transcribe-cpp 0.4.1, CPU only, MSVC build, AVX2 floor (`.cargo/config.toml`).
- Models: Q8_0 GGUF.
- Audio: Windows TTS clips from `scripts/make-test-audio.ps1` (4.5, 12.5, 25.5 and 57.2 s). The TTS voice is clean, so these numbers are for speed only. They do not show real accuracy.
- PC: Ryzen 9 5900X (12 cores, 24 threads), 64 GB RAM.
- **The test laptop numbers are still missing.** The gate decision needs them.

## Build and load

- **Build:** rustup `stable-x86_64-pc-windows-msvc` (rustc 1.99.1) builds transcribe-cpp with VS Build Tools 2019 and its CMake 3.20. The first build takes 1.5 min, and the spike exe is 3.7 MB.
- **Load time:** about 200 ms per model. The weights are memory-mapped.
- **Memory with both models:**

  | When | Working set | Private |
  |---|---|---|
  | After load | 1.05 GB | 0.72 GB |
  | After the first decode | 2.27 GB | 1.0 GB |

  This is more than the 1.5–2 GB estimate in ADR 0002.

## Nemotron live stream

**Lookahead** (`att_context_right`) on this PC, with the library's default thread count:

| Right context | Lookahead | First text at | Speed (x real time) | Text |
|---|---|---|---|---|
| 13 (model default) | 1040 ms | 1.1 s | 14–16x | reference |
| **6** | **480 ms** | **0.5 s** | **10–11x** | same as 13 |
| 1 | 80 ms | 0.5–0.6 s | 4x | same |
| 0 | 0 ms | 0.4–0.6 s | 2x | same, one period missing |

**Decision: use right context 6.** It gives the first word in half the time and loses no accuracy. Make it a setting.

**No word timestamps from the stream:** `snapshot()` returns 0 words, both before and after `finalize()`. So `draft.rs` must align the stream words with the Recast words by their text, not by time. The Recast (Parakeet) does give word timestamps.

## Parakeet Recast

Time for one Recast, by window length. The "4 threads" columns simulate a 4-core laptop on this faster CPU.

| Window | Default threads | 4 threads (live 2 + recast 2) | 4 threads (live 1 + recast 3) |
|---|---|---|---|
| 5 s | 0.24–0.28 s | 0.66–0.68 s | 0.45 s |
| 15 s | 0.67–0.71 s | 2.0 s | 1.37 s |
| 30 s | 1.31 s | 4.1 s | 2.8 s |

## Both models together in real time (the app's loop)

Audio comes in real time. Nemotron decodes every 100 ms, and a Recast of the last 30 s runs every 0.5 s.

| Threads | Live lag p95 | Recast while speaking (mean / max) | Last Recast after release |
|---|---|---|---|
| Default (24) | 0.1 s | 0.85 s / 1.5 s | 1.4 s |
| Live 2 + recast 2 | 0.1 s | 1.95 s / 4.1 s | 4.1 s |
| Live 1 + recast 3 | 0.18 s | 1.45 s / 2.9 s | 2.8 s |

(The numbers are for the 57 s clip, with right context 6 for the 4-thread rows.)

## Gate (dev PC only)

- **Live words:** pass. Nemotron decodes 3–6x faster than real time, even with 1–2 threads, and the live lag stays at 0.1–0.2 s.
- **30 s Recast under about 2 s with 4 threads:** fail. It takes 2.8–4.1 s here, and the Minimum Device will be slower. With a 30 s window, the 2 s limit on release will cut the last Recast for Dictations that are longer than about 15 s.
- **Options without a design change** (as the plan says):
  - a 10–15 s recast window (setting `recast_window_s`);
  - more threads for the Recast than for the stream;
  - a smaller Parakeet quant (Q5_K_M or Q4_K_M), not tested yet;
  - Vulkan on the integrated graphics, deferred.
- **Next:** run the same spike on the test laptop:

  ```
  spike.exe --right 6 --live-threads 1 --recast-threads 3 tts-15s.wav tts-60s.wav
  ```

  Then repeat with `--live-threads 2 --recast-threads 6` if the laptop has 8 threads.
