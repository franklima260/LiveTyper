# Best local English speech-to-text models, with no compute limits

Research date: 2026-11-01. Goals in order: words as fast as possible, live context corrections (recast loop), best final accuracy with punctuation and capitals. Compute limits ignored (RTX 3080 10 GB class).

"Latest WER" is the Open ASR Leaderboard English short-form set `english_short_latest.csv`, version 30-10-2026 (11 sets). Leaderboard rank in brackets. Classic model-card numbers use other sets and do not compare. RTFx is batched H200 throughput, not single-stream delay. VRAM values are estimates (params × 2 bytes).

| Model (release) | Latest WER | Params / VRAM | Streaming type | Speed | P&C | License | Windows runtime |
|---|---|---|---|---|---|---|---|
| Nemotron Speech Streaming EN 0.6B (Mar 2026) | 6.60 (#38) | 0.6B; Q8 GGUF about 0.6 GB; CPU OK | True streaming, 80–1120 ms chunks | 160 ms chunks: 2.3 ms/chunk on RTX 4090, 27 ms/chunk on CPU (NeMo-Speech.cpp) | Yes | NVIDIA Open Model License | NeMo-Speech.cpp (Windows CUDA/Vulkan/CPU, C API); sherpa-onnx ≥1.13.4 |
| Nemotron 3.5 ASR Streaming 0.6B (Jun 2026) | 9.75 | 0.6B | True streaming | RTFx 1343 | Yes | NVIDIA Open | As above |
| Voxtral Mini 4B Realtime 2602 | 8.44 | 4B; about 8 GB | True streaming, 80–2400 ms delay | RTFx 103 | Yes | Apache 2.0 | vLLM (WSL), transformers |
| Moonshine v2 Medium Streaming (Feb 2026) | Not on board; vendor 6.65 (unverified) | 245M; CPU OK | True streaming | Vendor 269 ms | Yes | Not verified | ONNX |
| Kyutai STT 2.6B (Jun 2025) | Not on board; 6.40 classic | 2.6B; about 6 GB | Streaming, fixed 2.5 s delay | – | Yes | CC-BY 4.0 | Rust/Python |
| Qwen3-ASR-1.7B (Jan 2026) | 5.40 (#11) | about 2B; about 5 GB | Offline (streaming only in vLLM) | RTFx 819 | Yes (not verified) | Apache 2.0 | transformers, vLLM (WSL); sherpa-onnx has 0.6B only |
| Granite Speech 5.0 2B (Oct 2026) | 5.68 (#12) | 2B; about 5 GB | Offline, LLM-based | RTFx 545 | Yes | Apache 2.0 | transformers, vLLM; keyword biasing |
| Cohere Transcribe 03-2026 | 5.77 (#14) | 2B; about 5 GB | Offline, AED | RTFx 902 | Yes | Apache 2.0 | transformers, vLLM |
| Canary-Qwen 2.5B (Jul 2025) | 5.94 (#21) | 2.5B; about 6 GB | Offline, LLM-based, clips ≤ 40 s | RTFx 862 | Yes | CC-BY 4.0 | NeMo only |
| Parakeet TDT 0.6B v2 / v3 (2025) | 6.02 / 6.18 | 0.6B; about 1.2 GB; CPU good | Offline TDT, not autoregressive | RTFx 5988 / 6132 | Yes | CC-BY 4.0 | sherpa-onnx, NeMo-Speech.cpp (v3), parakeet.cpp |
| Granite 5.0 470M TurboCTC (2026) | 6.38 (EN revision); multilingual revision self-reports 5.51 | 0.47B; CPU OK | Offline CTC | RTFx 21174 | Yes | Apache 2.0 | transformers, mlx |
| Nemotron 3.5 Transcribe 30B-A4B (Oct 2026) | 5.33 (#9) | 34B total, 4.3B active; needs about 80 GB | Offline, LLM-based | RTFx 462 | Yes | OpenMDW-1.1 | vLLM, transformers |

Older: Whisper large-v3 7.16, v3-turbo 7.64; Voxtral Small 24B 6.38, Voxtral Mini 3B 7.25 (Jul 2025).

Estimated delay for one 5–10 s window on an RTX 3080 (not measured): CTC/TDT 0.6B about 10–40 ms; LLM-based 2B about 100–400 ms.

## Hallucination risk

LLM-based models (Qwen3-ASR, Granite 2B, Canary-Qwen, Voxtral, Nemotron Transcribe) can invent, repeat or complete words when a window cuts a word, or holds silence or very short audio. Cohere Transcribe (AED) has a lower risk. CTC/TDT models (Parakeet, TurboCTC) have almost none. Mitigations: cut recast windows at VAD pauses, no windows under about 1 s, and only commit words that two recast passes agree on.

## Recommended pairing from the research

- First choice: Nemotron Speech Streaming EN 0.6B (NeMo-Speech.cpp, CUDA) for live words; Qwen3-ASR-1.7B (transformers, CUDA, BF16) for the recast loop and the final pass.
- Alternative: Nemotron Streaming for live words; Parakeet TDT 0.6B v2 for the live recast loop (native, no hallucination, near-zero decode time); one final pass with Qwen3-ASR-1.7B or Granite Speech 5.0 2B at key release.
- Watch: Nemotron 3.5 Transcribe (5.33, needs ≥ 24–80 GB GPU); Granite TurboCTC multilingual (5.51 self-reported, non-autoregressive).

## Not verified

All VRAM values and RTX 3080 delays; Moonshine numbers and license; Qwen3-ASR punctuation and streaming details; Nemotron Transcribe on 10 GB; Qwen3-ASR in llama.cpp.

## Sources

- https://huggingface.co/spaces/hf-audio/open_asr_leaderboard
- https://huggingface.co/spaces/hf-audio/open_asr_leaderboard/raw/main/init.py
- https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b
- https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b
- https://huggingface.co/nvidia/Nemotron-3.5-Transcribe-30B-A4B
- https://github.com/nvidia/nemo-speech.cpp
- https://github.com/NVIDIA/NeMo-Speech.cpp/releases/tag/v0.1.0
- https://k2-fsa.github.io/sherpa/onnx/nemo/nemotron-streaming.html
- https://k2-fsa.github.io/sherpa/onnx/qwen3-asr/index.html
- https://openwhispr.com/blog/local-streaming-speech-to-text
- https://huggingface.co/Qwen/Qwen3-ASR-1.7B
- https://github.com/QwenLM/Qwen3-ASR
- https://huggingface.co/ibm-granite/granite-speech-5.0-2b
- https://huggingface.co/ibm-granite/granite-speech-5.0-470m-turboctc
- https://huggingface.co/CohereLabs/cohere-transcribe-03-2026
- https://huggingface.co/nvidia/canary-qwen-2.5b
- https://huggingface.co/mistralai/Voxtral-Mini-4B-Realtime-2602
- https://huggingface.co/kyutai/stt-2.6b-en
- https://arxiv.org/abs/2602.12241
- https://github.com/mudler/parakeet.cpp
