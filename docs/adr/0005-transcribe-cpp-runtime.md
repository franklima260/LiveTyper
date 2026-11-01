# transcribe-cpp as the speech runtime

Supersedes ADR 0004.

We use transcribe-cpp (MIT, crate version 0.4.1, the runtime of the Handy app), not NeMo-Speech.cpp or sherpa-onnx. It runs both of our models (Nemotron Speech Streaming EN with committed and tentative text, and Parakeet TDT v3), it has ready Rust bindings, and it has the same Vulkan path for integrated graphics. It is also measured on a 2020 laptop CPU (Ryzen 7 4750U: 11 s of audio in 0.74 s). sherpa-onnx was rejected because ONNX Runtime makes the app larger. NeMo-Speech.cpp was rejected because we would have to write and keep our own Rust bindings.

Nobody has compared the speed of transcribe-cpp and NeMo-Speech.cpp on the same PC. Because of this, the app keeps the speech engine behind one interface, and the first prototype measures transcribe-cpp on the test laptop. If NeMo-Speech.cpp is clearly faster, we change.
