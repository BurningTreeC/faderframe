# Building and packaging

How to build FaderFrame from source and how the release packages are made.

Requirements: Rust 1.92 or newer (`rustup`), GTK 4.14+ and GStreamer
(1.24+ on Linux, where video uses its dmabuf formats; 1.18+ on Windows and
macOS — with its base and good plugins; the bad ones add the H.264/H.265
parsers and hardware decoders video uses) with `pkg-config`.

**Linux** additionally needs the development files of JACK (`jack.pc`;
libjack itself is loaded at run time, so FaderFrame starts without it),
ALSA and PipeWire, and libclang (the PipeWire bindings are generated at
build time):

| Distribution | Packages |
|---|---|
| Arch / Manjaro | `gtk4 pkgconf pipewire-jack` (or `jack2`) `alsa-lib pipewire clang cmake gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad` |
| Debian / Ubuntu (24.04+) | `libgtk-4-dev pkg-config libjack-jackd2-dev libasound2-dev libpipewire-0.3-dev libclang-dev cmake libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-good gstreamer1.0-plugins-bad` |
| Fedora | `gtk4-devel pkgconf-pkg-config pipewire-jack-audio-connection-kit-devel alsa-lib-devel pipewire-devel clang-devel cmake gstreamer1-devel gstreamer1-plugins-base-devel gstreamer1-plugins-good gstreamer1-plugins-bad-free` |

**macOS** (Apple Silicon or Intel): `brew install gtk4 pkgconf cmake gstreamer`.

**Windows**: in an [MSYS2](https://www.msys2.org) UCRT64 shell,
`pacman -S mingw-w64-ucrt-x86_64-gtk4 mingw-w64-ucrt-x86_64-pkgconf
mingw-w64-ucrt-x86_64-gcc mingw-w64-ucrt-x86_64-rust mingw-w64-ucrt-x86_64-cmake
mingw-w64-ucrt-x86_64-gstreamer mingw-w64-ucrt-x86_64-gst-plugins-base
mingw-w64-ucrt-x86_64-gst-plugins-good mingw-w64-ucrt-x86_64-gst-plugins-bad`.

Plugin GUIs embed natively everywhere: X11 windows on Linux (through
XWayland), Win32 windows on Windows, Cocoa views on macOS. CI builds and
tests all three platforms.

**ASIO** (Windows) is opt-in: `cargo build --release --features
faderframe-app/asio` compiles Steinberg's ASIO SDK (downloaded by `asio-sys`,
or taken from `CPAL_ASIO_DIR`; bindgen needs LLVM — CI checks the backend
with the MSVC toolchain). The SDK's licence — Steinberg's proprietary one
or GPLv3 — then applies to that binary, which is why the default builds
leave it out.

```bash
cargo build --release
cargo run --release -p faderframe-app -- --demo   # starts with the demo session
```

The binary is `target/release/faderframe`:

```text
faderframe [--backend auto|pipewire|jack|system|asio|dummy] [--sample-rate HZ]
           [--buffer-size FRAMES] [--threads N] [--empty | --demo]
           [--import FILE]... [PROJECT.ffproj]
```

Without `--empty`, `--demo` or a project, FaderFrame opens what
Preferences → General → On start-up says (the last project by default).
`system` is ALSA, WASAPI or CoreAudio. JACK clients connect to the PipeWire
graph directly when `pipewire-jack` is installed; the native PipeWire
backend needs no JACK at all.

## Packages

`packaging/` builds installable packages; the *Release* workflow builds all
of them for a `v*` tag and attaches them to the GitHub release.

| Platform | Package | How |
|---|---|---|
| Linux | portable tarball (GTK and GStreamer bundled; runs in place, `install.sh` installs it) | `cargo build --release && packaging/linux/tarball.sh` |
| Linux | Flatpak (GNOME 51 runtime) | `flatpak-builder --user --install build-dir packaging/flatpak/io.github.BurningTreeC.FaderFrame.yml` |
| Linux | system install from source (binary, desktop entry, AppStream, MIME type, icon) | `cargo build --release && sudo packaging/linux/install.sh /usr/local` |
| macOS | `FaderFrame.app` in a DMG (GTK and GStreamer bundled, ad-hoc signed) | `brew install gtk4 adwaita-icon-theme librsvg pkgconf gstreamer && packaging/macos/bundle.sh` |
| Windows | installer (Inno Setup) and portable zip (GTK and GStreamer bundled) | in MSYS2 UCRT64: `packaging/windows/bundle.sh` |

The release tarball is built on Debian 13 and bundles GTK 4.22. Every ELF in
the package is checked to require no newer than glibc 2.41, so it runs on
Debian 13 and newer distributions. The Flatpak carries its complete runtime
for older distributions. Source builds use the requirements listed above.

Video needs nothing installed with a package: the Linux tarball, the
macOS and the Windows packages carry GStreamer with the plugins video
uses (a system's GStreamer cannot be mixed with the bundled GTK
libraries); the Flatpak takes it from the GNOME runtime. The Linux tarball
also carries an LGPL build of FFmpeg's post-production decoders (ProRes,
DNxHD/DNxHR, MPEG-2, DV, FFV1, JPEG 2000 and others) and its MXF demuxer,
through GStreamer's libav plugin; no package carries FFmpeg's H.264 or
H.265 decoders, so those go through the hardware (VA-API on Linux, through
your graphics driver).

The macOS app is signed ad hoc, not notarised, so Gatekeeper stops it the
first time. On macOS 15 and later, try to open it once, then allow it in
System Settings → Privacy & Security → Open Anyway (earlier versions:
right-click → Open). Or clear the download's quarantine flag:
`xattr -dr com.apple.quarantine /Applications/FaderFrame.app`.
The Windows installer and zip are not signed either: if SmartScreen
stops the installer, choose More info → Run anyway.

**Portable mode.** With a folder named `FaderFrame Data` next to the
program, FaderFrame keeps everything it would put into your profile in
there — settings, plugin caches, presets, recordings of unsaved projects —
and also loads CLAP and VST3 plugins from its `Plug-Ins/CLAP` and
`Plug-Ins/VST3`. The Linux tarball and the Windows zip come with the
folder, so they run from a USB stick as they are; on macOS put the folder
next to `FaderFrame.app` (after moving the app out of the download
folder, which macOS runs from a read-only copy). Preferences → General
shows where the data goes. Video proxies and frame indexes are the
exception: they take gigabytes per hour of picture, so they go to the
computer's cache folder even then.
