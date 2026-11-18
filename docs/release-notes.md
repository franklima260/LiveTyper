LiveTyper brings local dictation to Windows x64, KDE Wayland, and Ubuntu 26.04 GNOME.

Choose a download for your desktop:
- Windows: use the Setup exe for Vulkan acceleration, or the CpuOnly Setup exe if you do not have a Vulkan loader.
- Ubuntu 26.04 GNOME: install the amd64 .deb with apt. It includes the Scratch Pad extension for GNOME Shell 50. Run livetyper-download-models, sign out and back in, then enable livetyper@franklima260.github.io.
- KDE Wayland: the Linux CpuOnly archive contains a binary built on Ubuntu 26.04. It requires compatible glibc 2.43+, GTK 4.22+, GTK4 Layer Shell and ALSA libraries, plus the keyboard/ydotool setup documented in the README. Older distributions should build from source.

The Linux downloads use CPU speech recognition. The Windows Vulkan build needs a compatible Vulkan driver. All builds need an x86-64 CPU with AVX2. Speech models are downloaded separately (about 1.4 GB) and checked against SHA-256 hashes.

This version includes GNOME portal hold-to-talk, automatic paste with remembered permission, the Scratch Pad, Escape cancellation, follow-up spacing, high-DPI rendering, and the Ctrl+Space release-order fix confirmed in issue #7.

Quit LiveTyper before updating. Keep your settings and models. After an Ubuntu update, sign out and back in to reload the extension. A per-user extension copy takes precedence over the packaged copy and must be updated too. See the README for full installation and update instructions.

Windows installers are unsigned, so Windows may display a publisher warning. SHA256SUMS provides the download checksums.
