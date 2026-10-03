# FaderFrame

A multitrack recording, editing and mixing environment written in Rust —
Linux first, Wayland native, GTK 4 native.

FaderFrame aims to become a professional DAW: graph-based routing with buses,
aux sends and sidechains, hard-realtime audio processing with automatic plugin
delay compensation, MIDI and virtual instruments, automation, CLAP/VST3
hosting, an analogue-console-style mixer and dockable, detachable editors.

> **Status: early development.** The architecture and a first vertical slice
> work; many features are still to come. See
> [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#status-and-roadmap).

## What works today

* GTK 4 application, native on Wayland, HiDPI/fractional scaling via GTK.
* Arranger (virtualised): tracks, audio clips with waveforms, MIDI clips,
  move/split/delete, snapping, zoom, loop range, playhead, console-style track
  headers with mute/solo/arm/monitor, volume, pan and meters.
* Analogue-console mixer: inserts, sends (pre-FX / pre / post), pan, M/S/R,
  faders with a console fader law, segmented peak meters, routing menus,
  any number of sends per channel (rows grow, ◂ ▸ pages through banks),
  scribble strips, pinned master section.
* Piano roll: draw, move, resize, delete notes, velocity lane, transpose.
* Docking: mixer / piano roll / automation tabs in a bottom dock, detach any
  view into its own window and dock it back, workspaces (Recording, Editing,
  Mixing, MIDI, Mastering), layouts saved with the project.
* Engine: routing graph with cycle detection and plugin delay compensation,
  buses, auxes, sends, solo-in-place, sample-accurate loops, built-in synth,
  echo, gain and latency-probe plugins, realtime-safe (verified by an
  allocation-counting test).
* Audio: JACK (JACK2 or PipeWire-JACK) and a silent dummy device. All common
  sample rates (44.1 – 192 kHz) and buffer sizes (16 – 8192 frames).
* Audio import: WAV, AIFF, CAF, FLAC, MP3, Ogg Vorbis, AAC/M4A, ALAC — via
  File → Import Audio (Ctrl+I) or drag & drop onto the arranger. Files are
  converted to the project rate once and streamed from disk during playback
  (bounded memory, lock-free read-ahead); media is kept in the project's
  `Audio/` folder; missing files show as offline clips.
* Recording: arm tracks, choose a mono input or a stereo pair per track,
  record with punch in/out, pre-roll and metronome; takes are latency
  compensated. Loop recording keeps every pass as a take.
* Takes and comping: take folders with take lanes — click a lane to use a
  take, drag across it to comp a section, flatten when done. Record mode
  (keep as takes / replace) and loop-record mode (takes / last pass / new
  track per pass) are selectable in Transport and Preferences → Recording.
* Track presets: save a track's channel settings (format, input, plugins
  with state, fader, pan, sends, output, colour) and recall them as a new
  track or onto another track. Resizable tracks (drag a header's bottom
  edge, Alt+wheel for all, View → Track Height).
* Automation for every automatable parameter — volume, pan, mute, sends,
  plugin parameters and bypass: lanes under each track (A button / A key),
  point editing and freehand drawing with curve shapes, Read / Touch /
  Latch / Write modes; faders follow the automation while playing.
* Plugins: built-in synth/echo/gain and CLAP effects and instruments, found
  by a crash-safe background scan and picked in a plugin browser (click an
  empty insert slot or Track → Plugin Browser…). Click a filled insert slot
  for the plugin's own GUI (Ctrl-click bypasses, right-click for the
  parameter window and more); editors open centred or where they were last,
  and their positions are saved with the project. Plugin state, parameters
  and automation are saved too.
* Render / export to WAV (16/24-bit with TPDF dither, 32-bit float): master
  or stems, project/loop/bar range, any sample rate, mono or stereo, tail,
  normalisation.
* Preferences (audio system, sample rate, buffer size, live DSP statistics,
  editing defaults), undo/redo, versioned project files (`.ffproj`).

## Building

Requirements:

* Rust 1.92 or newer (`rustup`)
* GTK 4.14+ development files and `pkg-config`
* JACK development files (`jack.pc`) — needed at build time; libjack is loaded
  at run time, so FaderFrame still starts without it.

| Distribution | Packages |
|---|---|
| Arch / Manjaro | `gtk4 pkgconf pipewire-jack` (or `jack2`) |
| Debian / Ubuntu (24.04+) | `libgtk-4-dev pkg-config libjack-jackd2-dev` |
| Fedora | `gtk4-devel pkgconf-pkg-config pipewire-jack-audio-connection-kit-devel` |

```bash
cargo build --release
cargo run --release -p faderframe-app          # starts with the demo session
```

The binary is `target/release/faderframe`:

```text
faderframe [--backend auto|jack|dummy] [--sample-rate HZ] [--buffer-size FRAMES]
           [--empty] [--import FILE]... [PROJECT.ffproj]
```

With PipeWire, JACK clients connect to the PipeWire graph directly (no JACK
server needed when `pipewire-jack` is installed).

### Shortcuts

| Key | Action |
|---|---|
| Space | Play / pause |
| Home | Return to start |
| L | Toggle loop |
| Shift+R | Record |
| Ctrl+P | Punch in/out |
| T | Show / hide the take lanes of the selected take folder |
| A | Show / hide automation of the selected tracks |
| Alt+wheel | Track height (all tracks) |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo |
| F2 / F3 / F4 | Toggle bottom dock / show mixer / show piano roll |
| Ctrl+1 … Ctrl+5 | Workspaces |
| Ctrl+I | Import audio files |
| Ctrl+Shift+R | Render / export |
| Ctrl+, | Preferences |
| Ctrl+wheel / Shift+wheel | Zoom / scroll horizontally |
| Shift or Ctrl while dragging | Fine adjustment |
| Alt while dragging | Disable snapping |

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace                 # or: cargo nextest run --workspace
cargo run -p faderframe-bench --release -- --tracks 128 --block 64 --rate 96000
python3 scripts/third_party_licenses.py --check
cargo deny check licenses
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design, crate
responsibilities and invariants.

## License

FaderFrame is released under the [MIT license](LICENSE). Third-party
components and their licenses are listed in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
