# NeMo-Speech.cpp as the speech runtime

Superseded by ADR 0005.

We use NeMo-Speech.cpp (NVIDIA, version 0.1.0, 2026), not the more mature sherpa-onnx. NeMo-Speech.cpp has a Vulkan backend that can use the integrated graphics of the Minimum Device, and sherpa-onnx has no documented path for this. The costs: a young library, a small Rust wrapper over its C API, and Parakeet v3 instead of v2 as the recast model (WER 6.18 against 6.02), because NeMo-Speech.cpp supports only v3.
