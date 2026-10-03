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
  headers with mute/solo/arm/monitor, volume, pan and meters. Clicking a
  clip selects it and moves the playhead to the (snapped) click; Shift- or
  Ctrl-click selects more clips, and moves, trims, fades and gain changes
  apply to every selected clip.
* Pro-style editing, with an edit toolbar under the transport (Edit button
  or Ctrl+E; it wraps into as many rows as the window needs): edit modes
  Shuffle, Slip, Spot and Grid (absolute or relative); the Smart tool
  (upper half selects a range, lower half grabs, edges trim, top corners
  fade) plus Zoom, Trim and Time-Stretch Trim, Selector, Grabber and
  Separation Grabber, Scrubber and Pencil; grids from a bar down to 1/256
  with triplet and dotted values; nudge values; Tab to clip boundaries or
  transients; Link Timeline and Edit Selection; Insertion Follows Playback;
  selection Start/End/Length counters in bars, time or samples; zoom
  buttons. Range edits: separate, trim to selection, clear (Shuffle closes
  the gap), copy/cut/paste, duplicate, repeat, insert silence.
* Clip gain (drag the dB readout in the clip's name strip, Ctrl+Shift+↑/↓,
  Alt-click to type) and fades with five shapes (Linear, Equal Power,
  S-Curve, Fast, Slow) and a curve you bend by dragging its handle;
  right-click a fade to pick its shape.
* Transient detection (spectral flux, adjustable sensitivity, cached) and
  elastic audio: Warp view shows transients and warp markers — double-click
  adds a marker, dragging a transient moves just that hit (its neighbours
  stay pinned), dragging inside a selection warps only that range,
  Ctrl-drag telescopes; Quantize Transients to Grid, Separate at Transients
  and time-compression trims. Warped clips play pitch-preserving
  (Polyphonic or Rhythmic, via Signalsmith Stretch) or as Varispeed.
* Transport: tap tempo (the TAP pad in the display), editable time
  signature (click it; right-click for common meters and meter changes),
  metronome button (K).
* Analogue-console mixer: inserts, sends (pre-FX / pre / post), pan, M/S/R,
  faders with a console fader law, segmented peak meters, routing menus,
  any number of sends per channel (rows grow, ◂ ▸ pages through banks),
  scribble strips, pinned master section; click a pan or level readout to
  type a value.
* Piano roll: select/draw/erase/split/mute tools, rubber-band selection,
  moving and Alt-copying with snap (Shift: free), resizing from either edge,
  chords (fixed or scale-aware), scales with highlighting, snap and folding,
  quantize (strength, swing, ends), humanize, legato, reverse, invert,
  velocity stems and line ramps, controller lanes (mod wheel, pitch bend,
  sustain, any CC: freehand, lines, erase), ghost notes of the track's other
  clips, auditioning, step input from a MIDI keyboard, copy/paste/duplicate,
  clip-length handle, inspector with numeric entry, live keys on the
  keyboard.
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
* Multicore engine: tracks, buses and plugins are processed in parallel on
  all cores (critical path first, realtime priority, flush-to-zero), with
  output bit-identical to single-threaded processing; Preferences → Audio
  → Processing threads.
* Plugins: built-in synth/echo/gain and CLAP and VST3 effects and
  instruments, found by a crash-safe background scan and picked in a plugin browser (click an
  empty insert slot or Track → Plugin Browser…). Click a filled insert slot
  for the plugin's own GUI (Ctrl-click bypasses, right-click for the
  parameter window and more); editors open centred or where they were last,
  and their positions are saved with the project. Plugin state, parameters
  and automation are saved too.
* MIDI sync: follow an external MIDI clock (tempo too) or MIDI time code
  (Preferences → MIDI → Sync). MPE: per-note pitch, pressure and timbre —
  recorded from MPE controllers, drawn in the piano roll's expression
  lanes, played on member channels (track menu → MPE). SysEx is recorded,
  sent to external devices with the clip, imported from and sent as `.syx`
  files.
* MIDI keyboards and controllers: every MIDI input (ALSA sequencer, incl.
  PipeWire; hotplug) — instrument tracks play what you play while armed or
  selected, with constant low latency; choose the input and channel per
  track (track menu → MIDI In). Record MIDI into clips (takes or replace,
  loop recording). MIDI learn: right-click a fader, pan, mute, send,
  automation lane or plugin parameter → MIDI Learn and move a knob; pads
  can toggle switches or run transport functions; soft takeover and
  endless encoders (relative modes); mapped controls don't reach the
  instrument. Mod wheel, pitch bend, sustain and aftertouch are recorded
  into clips and chased on playback. MIDI tracks play external instruments
  (track menu → MIDI Out) in time with the audio, and MIDI clock can sync
  external gear. Inputs, outputs, clock and mappings in Preferences → MIDI.
* Performance meter (F8, or click the DSP readout in the status bar): total
  DSP load with a 60 s history and a breakdown into plugins, mixing and
  engine work; load per track and per plugin instance (average and peak,
  sortable, latency and bypass shown); xruns, late callbacks and late disk
  reads.
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
| F8 | Performance meter |
| Ctrl+1 … Ctrl+5 | Workspaces |
| Ctrl+I | Import audio files |
| Ctrl+Shift+R | Render / export |
| Ctrl+, | Preferences |
| Ctrl+wheel / Shift+wheel | Zoom / scroll horizontally |
| Shift or Ctrl while dragging | Fine adjustment |
| Alt while dragging | Disable snapping |
| Ctrl+E | Show / hide the edit toolbar |
| K (or keypad 7) | Metronome on / off |
| Alt+1 … Alt+4 | Edit mode: Shuffle, Slip, Spot, Grid |
| Alt+S, F5–F7, F9, F10 (Alt+5 … Alt+0) | Smart, Zoom, Trim, Selector, Scrubber, Pencil (Grabber: Alt+8) |
| B | Separate clips at the selection (or the selected clips at the playhead) |
| Ctrl+C / Ctrl+X / Ctrl+V / Ctrl+D | Copy / cut / paste / duplicate the selection range |
| Alt+R | Repeat the selection range… |
| Ctrl+Alt+T | Trim clips to the selection |
| , / . | Nudge back / forward (Alt: trim start, Ctrl: trim end) |
| Tab / Shift+Tab | Next / previous clip boundary or transient (Ctrl: extend the selection) |
| Ctrl+Shift+↑ / ↓ | Clip gain ±0.5 dB |
| Ctrl+] / Ctrl+[ | Zoom in / out |

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
