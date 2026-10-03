# FaderFrame architecture

FaderFrame is designed from the first line as a professional multitrack DAW:
graph-based routing, hard-realtime audio processing, latency compensation,
plugin hosting, multicore scheduling and dockable custom-rendered editors.
Not everything is implemented yet (see [Status](#status-and-roadmap)), but no
part of the design assumes "one track", "stereo only", "track → master",
"zero-latency plugins" or "single core".

This document explains how the pieces fit together and lists the invariants
that keep it that way.

## 1. Crate map and dependency direction

```text
faderframe-app            binary: CLI parsing, logging, starts the GTK app
  └─ faderframe-ui        GTK 4 shell: windows, menus, dialogs, docking, canvas host
       ├─ faderframe-view-arranger / -view-mixer / -view-pianoroll   (GTK-free views)
       │    └─ faderframe-ui-canvas   Painter trait, events, CanvasView, theme, console controls
       ├─ faderframe-audio-jack       JACK backend (JACK2 / pipewire-jack)
       └─ faderframe-session          control-world hub (GTK-free)
            ├─ faderframe-engine       project→graph compiler, RT processor, controller, offline render
            │    ├─ faderframe-audio-graph   generic DSP graph: ports, edges, PDC, compile, executor
            │    ├─ faderframe-plugin-host   plugin abstraction + built-in plugins
            │    ├─ faderframe-transport     RT transport state, TransportInfo
            │    ├─ faderframe-realtime      atomics, mailbox, meters, param table, metrics
            │    └─ faderframe-audio         backend abstraction, dummy backend
            ├─ faderframe-project      persistent model, commands, undo, file format
            │    ├─ faderframe-workspace     docking/workspace layout model (GTK-free)
            │    ├─ faderframe-automation    automation curves
            │    └─ faderframe-audio-files   audio data, peak cache, generators, WAV I/O
            ├─ faderframe-timeline     musical time, tempo map, meter, grid
            ├─ faderframe-midi         MIDI events, sample-accurate buffers
            └─ faderframe-core         ids, channel layouts, gain/fader law, pan laws
faderframe-bench          headless engine benchmark (callback percentiles)
```

Dependencies only point downwards. **No crate below `faderframe-ui` depends
on GTK**, and no crate except `faderframe-audio-jack` depends on JACK. The
engine, session and views build and run headless (tests, CI, offline
rendering, the benchmark).

Planned crates (not created yet, to avoid empty boilerplate): CLAP and VST3
format hosts (`faderframe-plugin-clap`, `faderframe-plugin-vst3`), a
recording/disk-streaming crate, PipeWire-native/ALSA/WASAPI/ASIO/CoreAudio
backends, and an optional wgpu painter for dense views.

## 2. Control world vs realtime world

```text
 CONTROL WORLD (GTK main thread)                 REALTIME WORLD (audio thread)
 ───────────────────────────────                 ─────────────────────────────
 Session: Project + History + Workspace
   │ edits (Command) ──► Impact
   │
   ├─ Graph  ─► build_graph ─► compile ─► Mailbox ───────►  CompiledGraph (swap + state adoption)
   ├─ Timeline ─► TimelineSnapshot ────► Mailbox ───────►  clip / MIDI players read it
   ├─ Params ─► ParamTable (AtomicF32 slots) ───────────►  strips / sends read per block
   └─ TransportCommand ─► rtrb SPSC queue ──────────────►  TransportState
                                                          │
 meters, transport position, metrics  ◄── atomics ───────┤  MeterBank, TransportShared, CallbackMetrics
 drop retired graphs / snapshots      ◄── rtrb garbage ──┘  (old objects are never freed on RT)
```

* Every editor change is a [`Command`](../crates/faderframe-project/src/edit.rs).
  `Command::impact()` says how much engine state must be refreshed:
  `Params` (atomics only), `Timeline` (new snapshot), or `Graph` (rebuild).
* **Graphs and timeline snapshots** travel through latest-value mailboxes
  (`faderframe_realtime::mailbox`, an `AtomicPtr` swap). A newer object
  replaces an unconsumed older one, which is dropped on the control thread,
  so bursts of edits can never overflow a queue with stale structures.
* **Transport commands** are small, ordered and go through a bounded `rtrb`
  SPSC queue. When no stream is running, the session drains the idle
  processor itself (zero-frame calls), so queues never fill up.
* **Retired objects** (the previous graph/snapshot) are pushed into a
  garbage queue and dropped by the controller; if that queue were ever full
  the object is leaked and counted (`leaked_objects()`), never freed on RT.
* **State adoption**: when a new graph is installed, processors whose
  `NodeKey` matches a node of the old graph are swapped in, so filter
  states, synth voices, echo tails and smoothing survive routing edits.

The realtime path never allocates, frees, locks, logs or does I/O. This is
enforced by `crates/faderframe-engine/tests/realtime_alloc.rs`, which runs the
demo project (synth voices, echo, loop wrap, stop/start, graph swap) under a
counting global allocator and asserts zero allocations and deallocations.

## 3. Project model

`faderframe_project::Project` is the persistent model and contains only
persistent data:

* `tracks: Vec<Track>` in display order — kinds `Audio`, `Instrument`,
  `Midi`, `Bus`, `Aux`, `Master` (exactly one) share one structure: fader,
  pan, mute/solo/arm/monitor, inserts, instrument slot, sends
  (pre-FX / pre-fader / post-fader), input and output routing, automation,
  and an explicit `ChannelLayout` (mono, stereo, discrete N).
* `clips: BTreeMap<ClipId, Clip>` — audio clips (source, offset, length in
  project-rate frames, gain, fades, stretch settings, reverse) and MIDI clips
  (length + notes relative to the clip). Clip starts are `MusicalTime`.
* `sources` — files or deterministic generators (the demo session).
* `timeline` (tempo map with constant and linearly ramped segments, meter
  map), markers, loop range, `IdAllocator`.

Routing validity is checked when a command is applied: targets must exist
and be summing tracks (instrument tracks for MIDI tracks), and
`Project::would_cycle` rejects feedback loops before the graph compiler
(which detects cycles again as a second line of defence).

Solo is solo-in-place: a soloed track keeps everything it feeds and
everything that feeds it audible (`Project::solo_audible`), and the result is
written into per-strip mute slots.

### Undo/redo

`History` stores the inverse of every applied command. `BeginGesture` /
`EndGesture` group edits (a fader drag, drawing a note) into one undo step,
and repeated setters on the same target inside a gesture are coalesced.
ID allocations are deliberately not rolled back, so IDs stay unique.

### File format

`faderframe_project::file`: versioned JSON (`format`, `version`, `project`,
`workspace`). Loading parses into an untyped value, runs the migration chain
(`MIGRATIONS[v]` upgrades `v → v+1`), deserialises, then `Project::repair()`
fixes dangling references. Newer versions are rejected. Saving writes a
temporary file and renames it.

## 4. Time

`MusicalTime` is an `i64` tick count at 960 000 ticks per quarter note —
exact for straight, triplet and quintuplet grids and finer than one sample at
192 kHz down to ~20 BPM. `TempoMap` converts musical time ↔ seconds ↔
samples (closed-form for linear ramps). `TimeSignatureMap` handles bar/beat
math with meter changes at bar boundaries; grids restart at every bar.

## 5. The processing graph

`faderframe_audio_graph` is generic over an engine context type `C` and knows
nothing about projects.

* Nodes declare audio ports (each with a `ChannelLayout`) and event ports.
  Edges connect audio→audio or events→events; multiple edges into one input
  are summed (audio) or merged in time order (events). Channel conversion at
  edges: mono→N duplicates, N→mono averages, otherwise index-wise.
* `compile()` performs cycle detection (labels of the offending nodes are
  reported), a deterministic topological order, latency propagation, buffer
  allocation and dependency analysis.
* **Plugin delay compensation**: each node's output latency is the maximum
  arrival latency over all its inputs (main, sidechain, events) plus its own
  latency; every edge arriving early gets a delay line of exactly the
  difference. Tracks, inserts, buses, sends, returns and the master are all
  covered because they are all just nodes.
* **Scheduling**: the executor is currently serial. Nodes are stored in
  topological order and each node only reads upstream output buffers and
  writes its own, so the serial executor needs no `unsafe`, and the compiled
  graph already carries per-node dependency counts, dependents lists and
  levels (`GraphStats::levels`, `max_width`) for the planned dependency-aware
  multicore scheduler (fixed worker threads, preallocated jobs, atomic
  counters — no Rayon on the audio thread).

### Engine graph per track

```text
sources ──► TrackInput ──► insert₁ … insertₙ ──► ChannelStrip ──out0 (post)──► destination TrackInput / device out
(clips,      (sum point,                          │      └─out1 (pre)
 monitor,     pre-FX tap)                         └─► SendNode (pre-FX / pre-fader / post-fader) ──► aux / bus
 instrument)
MIDI clip player ──events──► instrument plugin       MIDI tracks ──events──► target instrument
```

The channel strip implements polarity, mute, the fader (dB via a table-driven
console `FaderLaw`, unity at 75 % travel, "-inf" stored as −144 dB) and
pan: constant-power −3 dB for mono sources, 0 dB balance for stereo sources
(`faderframe_core::pan`). All gain changes are ramped per block.

## 6. Transport and timing

`TransportState` lives on the audio thread. The engine splits every device
callback into internal blocks of at most `max_block_size` frames **and** at
loop boundaries, so loops are sample-accurate for any buffer size (verified
for 44.1/48/88.2/96/176.4/192 kHz × 32…4096 and odd 441-frame buffers in
`tests/formats.rs`). Each block gets a `TransportInfo` (position, tempo,
meter, bar, loop, play/record state) for processors and plugins, plus a
`discontinuity` flag on stop/locate/wrap so note generators release notes.

## 7. MIDI and instruments

Events always carry a frame offset inside the block (`TimedMidiEvent`).
`MidiBuffer` is fixed-capacity and sorted (note-offs before note-ons at the
same offset); overflow drops and counts. The MIDI clip player emits events at
exact offsets; the built-in synth renders between event offsets. MPE / note
expressions are planned as additional event types.

## 8. Plugins

`faderframe_plugin_host` mirrors how CLAP/VST3 split a plugin:

* `PluginInstance` — control thread: descriptor, parameters, state,
  latency/tail, activation (`create_processor`).
* `PluginProcessor` — audio thread: `process(ctx, io) -> ProcessStatus`
  (no allocating `Result` on RT), `reset()`.

Neither trait assumes the plugin is in-process; a sandboxed plugin is a proxy
pair speaking IPC with shared-memory audio. `PluginHost` (engine) owns one
instance per slot. Built-ins: synth, echo, gain, latency probe (used to test
PDC end to end). CLAP via `clack-host` is the next format, then VST3, then AU.
Failed plugins are bypassed and flagged; missing formats pass audio through
with a warning.

## 9. Audio backends

`faderframe_audio::AudioBackend` opens an `AudioStream` with a boxed
`AudioCallback`. Device buffers are exposed through the `DeviceBuffers` trait
(per-channel slices, no per-callback arrays), so backends need no
allocation in their process callbacks. Status (xruns, rate, buffer size,
shutdown) is shared through lock-free `StreamMonitor` atomics.

* `JackBackend` — JACK2 or pipewire-jack, libjack loaded at runtime. The
  server owns rate and buffer size; FaderFrame follows (the session rebuilds
  the engine on a rate change) and can request a buffer size live.
* `DummyBackend` — timer-driven, silent; used when no audio server exists
  and in CI.
* Planned: PipeWire native, ALSA, WASAPI/ASIO (Windows), CoreAudio (macOS).

## 10. Session

`faderframe_session::Session` is the single mutation point for the GUI:
`dispatch(Action)` covers edits, gestures, undo/redo, transport, workspace
changes, selection, editor focus, track creation and editor settings.
`tick(dt)` (once per UI frame) polls transport position and meters (with
ballistics, peak hold and clip latching on the UI side) and reacts to stream
status (rate change, server shutdown). `render::start` runs offline bounces
(master or stems, range, rate, channels, tail, normalise, dither) on a
worker thread through the same engine path.

## 11. UI architecture

GTK owns windows, menus, dialogs, text entry, clipboard, accessibility and
the event loop. DAW work surfaces are **custom-rendered views**:

* `CanvasView<Session, Action>` (`faderframe-ui-canvas`) — paint through the
  `Painter` trait in logical pixels, receive `ViewEvent`s, emit `Action`s and
  `HostRequest`s (native popover menus, inline text entry). Views own only
  presentation state (scroll, zoom, hover, drag).
* `CanvasWidget` (`faderframe-ui`) — a `gtk::Widget` subclass that paints via
  `SnapshotPainter` (GSK render nodes: colours, gradients, shadows,
  fill/stroke paths, cached Pango layouts) and maps GTK gestures to events.
* No widget per clip, note, knob or fader. The arranger and mixer are
  virtualised (only visible rows/strips/clips are visited); waveforms are one
  filled path per clip channel from a multi-resolution `PeakCache`.
* The mixer is drawn as an analogue console from `controls` (panel, knob,
  fader, segmented meter, LED buttons, scribble strips) using a `Theme`
  (skins replace the theme, not the views). Track headers reuse the same
  controls.

HiDPI and fractional scaling are handled entirely by GTK; views never assume
96 DPI. On Wayland the app is a native Wayland client (the status bar shows
the GDK backend).

### Docking

`faderframe_workspace` models layouts independently of GTK: per window a tree
of `Split`s and `TabGroup`s; named dock areas (`main`, `bottom`) persist when
empty so detached views have a home; floating windows; geometry; presets
(Recording, Editing, Mixing, MIDI, Mastering). `faderframe_ui::dock::realize`
turns the active layout into `gtk::Paned` / `gtk::Notebook` /
`gtk::ApplicationWindow`s. Every view has exactly one persistent host widget
that is only re-parented, so detaching a view never copies state. Divider
positions, tab switches and window sizes are written back into the model and
saved with the project.

## 12. Metering and metrics

Meters: the RT side accumulates per-channel peak and mean square with atomic
max; the UI consumes with atomic swap — no peak is lost between frames.
Metrics: `CallbackMetrics` records every callback's duration against its
deadline in a log-scaled histogram (p50/p95/p99/max, deadline misses, xruns)
and per-node accumulated time (`NodeTimings`). `faderframe-bench` measures
worst-case callback time for N tracks/buses/inserts/sends at any rate and
block size.

## 13. Invariants

1. No GTK (or other GUI) dependency below `faderframe-ui`.
2. No backend-specific types (JACK, PipeWire, ALSA, Win32, CoreAudio) outside
   their backend crates; the generic engine sees `DeviceBuffers` only.
3. The realtime path performs no allocation, deallocation, locking, I/O,
   logging, sleeping or thread creation. Retired objects go back to the
   control thread.
4. Realtime-visible state changes only through the mailboxes, the bounded
   SPSC queue and atomics. The audio thread never reads the `Project`.
5. All project mutations go through `Command`s (undoable, impact-tagged);
   views never mutate project data directly.
6. Views own presentation state only; project state is never duplicated per
   view or per window.
7. The graph is a DAG; feedback is rejected both by the project layer and the
   graph compiler.
8. Every summing point is latency-aligned; plugin latency is part of a
   node's identity (a change rebuilds the graph).
9. Channel layouts are explicit everywhere; nothing assumes stereo.
10. IDs are persisted newtypes, allocated by the project's `IdAllocator`.
11. `unsafe` is forbidden in every crate except `faderframe-realtime`
    (documented mailbox), `faderframe-audio-jack` (JACK trait requirement) and
    `faderframe-ui` (GObject subclassing macros).

## 14. Cross-platform strategy

Platform specifics are isolated in backends and the GTK shell. Windows:
GTK 4 Win32 backend, WASAPI and ASIO backends implementing `AudioBackend`.
macOS: GTK 4 macOS backend, CoreAudio, AU as an additional plugin format. The
core crates already build for every target the toolchain supports.

## Status and roadmap

Implemented in this first vertical slice: workspace and crate structure,
project model with undo and versioned files, tempo/meter maps, graph engine
with PDC and state adoption, transport with sample-accurate loops, built-in
synth/echo/gain/latency plugins, JACK and dummy backends, offline render with
WAV export (stems, normalise, dither), GTK shell with docking/detaching and
workspaces, arranger, analogue mixer, piano roll, preferences and render
windows, benchmark, and tests.

Next, in order:

1. Audio file import (Symphonia), disk streaming with read-ahead, resampling.
2. Recording: lock-free capture buffers → writer thread, punch, takes, input
   latency compensation.
3. CLAP hosting (`clack-host`), plugin scanning in a helper process, plugin
   GUIs, parameter automation, state.
4. MIDI input/output (`midir`), live auditioning, MIDI learn.
5. Automation lanes in the arranger, sample-accurate parameter events.
6. Dependency-aware multicore scheduler.
7. VST3, PipeWire-native backend, Windows and macOS ports.
