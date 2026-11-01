# Local English speech-to-text models for a CPU-only Windows dictation app

Research date: 2026-11-01. Requirement: real time on a CPU only (laptop with integrated graphics), English, no network at runtime.

WER is the Open ASR Leaderboard average on the classic 8-dataset set (no punctuation, no case). The 2026 leaderboard set (`english_short_latest.csv`) gives numbers about 0.4 points higher for the same model.

## Comparison

| Candidate | WER avg | Size / RAM | CPU speed | Streaming type | Punct. and caps | License | Windows runtime |
|---|---|---|---|---|---|---|---|
| NVIDIA Nemotron Speech Streaming EN 0.6B (Mar 2026) | 6.93 (1.12 s chunk), 7.07 (0.56 s), 7.67 (0.16 s), 8.43 (0.08 s) | 0.6B; int8 ONNX 464–632 MB; about 1 GB RAM | Ryzen 7500F, sherpa-onnx int8: RTF 0.067 at 1.12 s; 97 ms per audio second at 560 ms; 275 ms at 160 ms (4 threads). 1 thread at 560 ms: RTF 0.16 | True streaming (cache-aware RNNT), no re-decode | Yes | NVIDIA Open Model License (commercial OK) | sherpa-onnx ≥1.13.4: C, C++, C#, Python, Rust, Go, JS; NeMo-Speech.cpp (GGUF) |
| Moonshine v2 Streaming Tiny / Small / Medium (Feb 2026) | 12.00 / 7.84 / 6.65 | 34M / 123M / 245M; < 1 GB | Latency per update on x86 (CPU not named): 69 / 165 / 269 ms | Streaming encoder; decoder re-runs on the current line about every 500 ms; line fixed at a pause | Not verified | MIT | C++ on ONNX Runtime; C API, Python, Swift, Java; VAD and line events |
| Parakeet TDT 0.6B v2 (May 2025) / v3 (Aug 2025) | 6.05 / 6.32 | 0.6B; int8 about 600 MB; ≥ 2 GB RAM | Intel 255H: 3.85 s audio in 174 ms; about 0.4–0.6 s for a 10 s window | Offline; chunked streaming falls to 9.2–12.8 WER | Yes | CC-BY-4.0 | sherpa-onnx, onnx-asr, NeMo-Speech.cpp |
| Parakeet realtime EOU 120M | 9.30 | 120M | No CPU data | True streaming + end-of-utterance | No | NVIDIA Open Model License | NeMo only (CUDA) |
| Whisper (whisper.cpp / faster-whisper) | small.en 8.59, turbo 7.83, distil-large-v3.5 7.21, large-v3 7.44 | tiny 273 MB … large 3.9 GB | Whisper Small about 3.4 s latency on x86 | Offline, 30 s window; needs a re-decode wrapper | Yes | MIT | whisper.cpp (Vulkan/OpenVINO iGPU path), whisper.net, whisper-rs |
| sherpa-onnx streaming Zipformer (2023) | Not verified | Tens of MB | Very fast | True streaming | No | Apache-2.0 | sherpa-onnx |
| Kyutai STT 1B / 2.6B | 2.6B: 6.40 | 1B / 2.6B | No CPU path documented | True streaming | Not documented | CC-BY-4.0 | Poor fit on Windows CPU |
| Vosk en-us-0.22 | Not on leaderboard | 1.8 GB (small: 40 MB) | Real time on CPU (old claim) | True streaming (Kaldi) | No | Apache-2.0 | C, Python, C#, Java |
| Windows.Media.SpeechRecognition | Not published | Built-in | — | Streaming | Yes | OS | Dictation uses a web service. Not usable offline. |

Watch item: IBM granite-speech-5.0-470m-turboctc (Oct 2026), Apache-2.0, punctuation and caps, 6.84 on the 2026 set. No ONNX or Windows runtime yet. Streaming is not claimed.

## Recommendation from the research

1. Nemotron Speech Streaming EN 0.6B, int8, through sherpa-onnx, 560 ms chunk. Best accuracy among true streaming models, with punctuation and caps. Append about one chunk of silence at key release, or the last words can get lost.
2. Moonshine v2 Small or Medium Streaming. MIT, lighter, native line events and VAD. Punctuation and Windows packaging need a test.
3. Hybrid: a streaming model for the live draft, then one Parakeet TDT v2 pass on the full utterance at key release (about 0.5 s per 10 s of audio). Best final WER.

Trade-off: a true streaming model processes each frame one time, so the cost stays flat and the draft is stable. An offline model in a re-decode loop costs more as the window grows, the words at the window edge flicker, and edge accuracy falls a lot. One final offline pass gives the full-context benefit at a lower CPU cost.

## Open facts

- No measurements exist on a 4-core 2020 laptop CPU for any candidate. Benchmark on the Minimum Device.
- DirectML and OpenVINO iGPU paths are not documented for the sherpa-onnx models. whisper.cpp has Vulkan and OpenVINO.
- Older than mid-2025: Vosk tables, Zipformer English models, faster-whisper benchmark, whisper_streaming latency.

## Sources

- https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b
- https://k2-fsa.github.io/sherpa/onnx/nemo/nemotron-streaming.html
- https://softcery.com/lab/hosting-nemotron-asr-streaming-on-a-cpu
- https://openwhispr.com/blog/local-streaming-speech-to-text
- https://arxiv.org/html/2604.14493v2
- https://moonshine-voice.readthedocs.io/en/latest/moonshine-vs-whisper/
- https://moonshine-voice.readthedocs.io/en/latest/using/transcription/
- https://huggingface.co/UsefulSensors/moonshine-streaming-medium
- https://arxiv.org/abs/2602.12241
- https://github.com/moonshine-ai/moonshine
- https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2
- https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3
- https://huggingface.co/nvidia/parakeet_realtime_eou_120m-v1
- https://k2-fsa.github.io/sherpa/onnx/pretrained_models/offline-transducer/nemo-transducer-models.html
- https://github.com/likeablob/parakeet-api
- https://huggingface.co/datasets/hf-audio/open-asr-leaderboard-results/blob/main/english_short_latest.csv
- https://arxiv.org/pdf/2510.06961
- https://github.com/k2-fsa/sherpa-onnx
- https://github.com/ggml-org/whisper.cpp
- https://github.com/SYSTRAN/faster-whisper
- https://github.com/ufal/whisper_streaming
- https://github.com/kyutai-labs/delayed-streams-modeling
- https://alphacephei.com/vosk/models
- https://huggingface.co/ibm-granite/granite-speech-5.0-470m-turboctc
- https://learn.microsoft.com/en-us/windows/apps/develop/input/speech-recognition
