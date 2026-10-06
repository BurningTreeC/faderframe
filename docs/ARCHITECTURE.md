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
       ├─ faderframe-view-arranger / -mixer / -pianoroll / -performance / -tools / -automation / -devices   (GTK-free views)
       │    └─ faderframe-ui-canvas   Painter trait, events, CanvasView, theme, console controls
       ├─ faderframe-audio-pipewire   native PipeWire backend (pw_filter, Linux)
       ├─ faderframe-audio-jack       JACK backend (JACK2 / pipewire-jack, Linux)
       ├─ faderframe-audio-cpal       system backend through cpal: WASAPI, ASIO (opt-in), CoreAudio, ALSA
       ├─ faderframe-plugin-clap      CLAP host (clack-host): scan helper, instances, editors
       ├─ faderframe-plugin-vst3      VST3 host (vst3 bindings): modules, scan helper, instances, editors
       ├─ faderframe-plugin-au        Audio Unit host (AudioToolbox C API, macOS): registry scan, instances, Cocoa views
       ├─ faderframe-midi-io          MIDI devices (midir: ALSA sequencer, CoreMIDI, WinMM), virtual ports
       └─ faderframe-session          control-world hub (GTK-free)
            ├─ faderframe-analysis     loudness (EBU R128), true peak, levels, phase, FFT spectrum
            ├─ faderframe-disc         Red Book CD masters: DDP 2.00 filesets, CD-Text, cue sheets, ISRC/UPC
            ├─ faderframe-engine       project→graph compiler, RT processor, controller, offline render
            │    ├─ faderframe-audio-graph   generic DSP graph: ports, edges, PDC, compile, executor
            │    ├─ faderframe-plugin-host   plugin abstraction + built-in plugins
            │    ├─ faderframe-transport     RT transport state, TransportInfo
            │    ├─ faderframe-realtime      atomics, mailbox, meters, param table, metrics
            │    ├─ faderframe-stretch       time stretching (vendored Signalsmith Stretch, C shim)
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
on GTK**, and only the backend crates name an audio API (JACK, PipeWire,
cpal). The engine, session and views build and run headless (tests, CI,
offline rendering, the benchmark).

`faderframe-plugin-clap`, `faderframe-plugin-vst3` and
`faderframe-plugin-au` implement the `faderframe-plugin-host` traits and are
registered by the shell (`set_default_registry`), so the engine never names
a plugin format. ASIO is a host of the cpal backend behind its `asio`
feature, not a crate of its own. Planned: an optional wgpu painter for
dense views.

`packaging/` holds what turns a release build into packages: the
application icon (SVG; `icons.py` makes `.ico`/`.icns` from it), the
desktop entry, AppStream metadata and MIME type with a Linux install
script, the Flatpak manifest, the macOS app bundle and DMG script (GTK
bundled by `bundle_dylibs.py`, which copies every non-system library and
rewrites the install names, resolving `@rpath` like dyld; a launcher
pointing GTK at the bundle's data)
the Windows bundle script with its Inno Setup installer and portable zip,
and the portable Linux tarball (`tarball.sh`: the program, GTK and the
libraries it needs except the C runtime, graphics drivers, display and
audio server clients and fonts, schemas, icon themes and image loaders;
a launcher that runs it in place and an `install.sh` for a per-user or
system-wide installation). The *Release* workflow runs them for `v*` tags.

Portable mode (`faderframe_core::paths`): a `FaderFrame Data` folder next
to the executable, one level up (the `bin/` layouts) or beside
`FaderFrame.app` — or `FADERFRAME_DATA_DIR` — takes every per-user file:
`Settings` (preferences), `Cache` (plugin scans, generated icons; the Linux
launcher also points Mesa's shader cache there), `Data` (presets, track
presets, unsaved recordings) and `Plug-Ins/{CLAP,VST3}`, searched before
the standard plugin folders.

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
  `Midi`, `Bus`, `Aux`, `Vca`, `Master` (exactly one) share one structure:
  fader, pan, mute/solo/arm/monitor, inserts (each with an optional
  sidechain source), instrument slot, sends (pre-FX / pre-fader /
  post-fader), input and output routing, automation, the VCA and group it
  follows, a freeze (rendered audio) and an explicit `ChannelLayout` (mono,
  stereo, discrete N).
* `groups` — `TrackGroup`s (name, colour, active, `GroupLink`: volume,
  mute, solo, record arm, selection); members name theirs in `Track::group`.
* `clips: BTreeMap<ClipId, Clip>` — audio clips (source, offset, length in
  project-rate frames, gain, fades, stretch settings, reverse) and MIDI clips
  (length + notes relative to the clip). Clip starts are `MusicalTime`.
* `sources` — files or deterministic generators (the demo session).
* `timeline` (tempo map with constant and linearly ramped segments, meter
  map), markers, arrangement `sections` (named, coloured ranges: Intro,
  Verse, …), loop and punch ranges, MIDI mappings, `IdAllocator`.

Routing validity is checked when a command is applied: targets must exist
and be summing tracks (instrument tracks for MIDI tracks), and
`Project::would_cycle` rejects feedback loops before the graph compiler
(which detects cycles again as a second line of defence).

Solo is solo-in-place: a soloed track keeps everything it feeds and
everything that feeds it audible (`Project::solo_audible`), and the result is
written into per-strip mute slots. A soloed VCA solos the tracks it scales.

**VCAs** are tracks without audio: a member's gain is its own fader times
its VCA's (and that VCA's VCA, `Project::vca_chain`); a muted VCA mutes its
members. `Project::would_cycle` also follows sidechain edges
(`dependency_edges`), so a plugin can't be keyed by a track that depends on
it, and `SetTrackVca` refuses VCA loops.

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
* **Scheduling** is dependency-driven and multicore (below).

### Multicore processing

The graph runs on the audio thread plus a fixed pool of DSP worker threads
(`faderframe_realtime::WorkerPool`; default one thread per logical core,
Preferences → Audio → Processing threads, `--threads`).

* **Jobs.** At compile time nodes are fused into jobs: a node whose only
  dependent has no other input continues that dependent's job (a track's
  input → inserts → strip chain), and a source feeding a node that depends
  on sources only (clip players and live input into a track input) runs at
  the start of that job. 128 tracks with sends and buses become ~260 jobs
  instead of ~1200 nodes, so scheduling costs a few atomic operations per
  track, not per node. `GraphStats::jobs` / `job_width` report the result.
* **Execution.** Per cycle every job gets a counter of unfinished upstream
  jobs; ready jobs go into a preallocated lock-free queue (each job is
  pushed at most once per cycle). Threads pop jobs, run their nodes in
  order, decrement their dependents' counters and continue directly with
  one released dependent (cache-warm), queueing the others. The caller
  returns when every job is done. Graphs whose width is one job, or whose
  measured work is below `parallel_min_ns` (40 µs, with hysteresis), stay
  on the audio thread — waking workers would cost more than it saves.
* **Critical path first.** Every job's time is measured (two clock reads).
  Every 16 cycles the audio thread smooths the costs and recomputes each
  job's rank — its own cost plus the most expensive path to the end —
  without allocating; roots start in rank order and a thread continues with
  the released dependent of highest rank. A track with several heavy
  plugins therefore starts first instead of becoming the tail of the cycle.
  The verdict on threading carries over when an edit swaps the graph.
* **Soundness without trusting the scheduler.** Node buffers and node work
  (processor, input delay lines) live in `TaskCells`: per-cycle
  pending → running → done cells that hand out `&mut` to exactly one
  claimant and `&` only after done (acquire/release). A scheduling bug
  would skip work, never race; `faderframe-audio-graph` stays free of
  `unsafe`. Every node sums its inputs in a fixed order, so parallel output
  is bit-identical to serial (tested on random graphs and the demo
  project).
* **The pool.** `WorkerPool::run` is a scoped broadcast: the job pointer is
  published atomically, workers register in an `active` count before
  reading it, and the caller waits for zero before returning. Idle workers
  spin ~100 µs (consecutive chunks of a callback catch them awake), then
  sleep on a futex; one non-blocking `FUTEX_WAKE` wakes as many as the
  graph can use. Workers run at the audio thread's urgency — on Linux its
  scheduling policy and priority (`SCHED_FIFO` under JACK/PipeWire), on
  macOS its Mach time-constraint policy and the device's audio workgroup
  (`Workgroup`: the CoreAudio device's `IOThreadOSWorkgroup`, found by
  cpal's device UID and handed to `WorkerPool::set_workgroup`; each worker
  joins before its next job, so the scheduler sees one realtime workload
  with the IO thread's deadline — on Apple silicon performance cores), on
  Windows MMCSS's "Pro Audio" task (elsewhere they park/unpark) — and flush denormals, as does
  the audio thread for every callback (`ScopedFlushDenormals`). Without
  realtime scheduling a preempted worker would stall the cycle, so the pool
  then stays within the physical cores.
* **Plugins** run on whichever thread processes their node. Each instance
  is in one node, behind a `TryCell`, so calls never overlap; CLAP's
  thread-check reports every DSP thread as an audio thread.
* **Measured** (`faderframe-bench --paced`, Ryzen 7 5700G, 8 cores / 16
  threads, `SCHED_FIFO`): 128 tracks × 4 echo inserts at 128 frames went
  from 1542 µs (58 % load, 85 late callbacks) on one thread to 184 µs
  (7 %) on 16; 64 instances of a real VST3/CLAP EQ (332 % of the deadline
  on one thread) run at 27.5 % with a 774 µs p99.9 and no late callbacks,
  28 % at 64-frame buffers.
* **Metering.** The performance view reports graph CPU time summed over
  threads, the graph's wall-clock share (so "engine" stays right), the
  thread count and how many threads were busy at once; free capacity is
  counted over all threads.

### Render ahead (anticipative processing)

Tracks nobody plays live are rendered ahead of the playhead on a thread of
their own (`faderframe_engine::ahead`; Preferences → Audio → Render ahead,
default 200 ms), so a heavy plugin chain no longer has to finish within one
device buffer and spikes are absorbed by the lookahead.

* **What.** An audio or instrument track with plugins that is not frozen
  or armed, has no input monitoring, live MIDI, external MIDI output,
  sidechain input or pre-FX send, is not fed by another track and has no
  plugin editor open (`build::ahead_eligible`, the session's open
  editors): its sources, instrument and inserts go into a second graph
  ending in an `AheadWriter`; the realtime graph keeps an `AheadReader`
  with the chain's latency (delay compensation unchanged) in front of its
  strip, so faders, pan, mute, solo and sends stay immediate. Buses,
  masters and everything live stay on the audio thread.
* **Prediction.** The anticipator runs its own copy of the transport
  (`TransportState` is plain data), so it renders exactly the positions
  the audio thread will ask for, loop wraps and scrub snippets included.
  A command that changes them starts a new *sequence*: the audio thread
  hands the commanded state over (`AheadLink::command`, wait-free) and
  carries on until the rings hold two blocks of it (`ready`), at most
  250 ms, then switches — play, stop and locate take effect a few
  milliseconds later, without a gap. Commands that change nothing (the
  loop range sent again with every timeline update) start none;
  recording on/off is applied at once.
* **Rings** (`AheadRing`, one per track, kept across rebuilds) carry
  interleaved frames and segments tagged (sequence, position, playing).
  The reader skips older sequences and passed positions, waits (silence,
  a counted miss) for audio not there yet and never reads a sequence it
  has not switched to. Both ends sit in `TryCell`s shared by successive
  graphs.
* **The anticipator** owns the ahead graph (adopting processor state
  across rebuilds like the audio thread) and its own context (the
  timeline snapshot is an `Arc` shared with the audio thread; both hand
  their old ones back to the control thread), renders in the engine's
  block size while the rings hold less than the lookahead, in parallel on
  a pool of its own (normal priority), and sleeps a millisecond otherwise.
* **Changes.** What is heard of an anticipated track was rendered up to
  the lookahead earlier: edits to its clips, automation and plugins
  arrive that much later — so opening a plugin's editor moves its track to
  the audio thread. A track that becomes live (armed, selected for live
  play, editor opened) moves at once (its plugins jump back by up to the
  lookahead); one that stops being live stays on the audio thread until
  playback stops (moving it while playing would leave a gap), then the
  session rebuilds (`ahead_wants_rebuild`). Stateful effects have
  processed the lookahead's worth of audio a locate discards: their tails
  differ briefly from a render without anticipation.
* **Numbers.** 64 tracks × 6 echoes at 64-frame buffers (4 threads,
  paced): audio-thread callbacks mean 187 → 34 µs, p99 635 → 50 µs, max
  1345 → 81 µs, deadline misses 1 → 0, no late blocks
  (`faderframe-bench --ahead 200`). Tests: `engine/tests/ahead.rs` (output
  identical sample for sample through playback, a locate and loop wraps,
  no late blocks), `realtime_alloc.rs` (the audio thread's side does not
  allocate), `session/tests/render_ahead.rs`.

### Engine graph per track

```text
sources ──► TrackInput ──► insert₁ … insertₙ ──► ChannelStrip ──out0 (post)──► destination TrackInput / device out
(clips,      (sum point,         │  ▲             │      └─out1 (pre)
 monitor,     pre-FX tap)        │  └ sidechain   └─► SendNode (pre-FX / pre-fader / post-fader) ──► aux / bus
 instrument)                     └──► (post-insert tap) ──► sidechain input of a plugin on another track
MIDI clip player ──events──► instrument plugin       MIDI tracks ──events──► target instrument
```

The channel strip implements polarity, mute, the fader (dB via a table-driven
console `FaderLaw`, unity at 75 % travel, "-inf" stored as −144 dB) and
pan: constant-power −3 dB for mono sources, 0 dB balance for stereo sources
(`faderframe_core::pan`). All gain changes are ramped per block. A strip
also applies its VCAs: the static gain of unautomated VCAs comes in a
parameter slot, automated VCA faders and mutes as lanes in the timeline
snapshot (so VCA automation is sample-accurate on every member). The strip
of the analysed track (the Tools view) copies its post-fader output into a
`ScopeRing`.

* **Analogue crosstalk.** `Project::crosstalk` (Preferences → Project) defaults
  off and is saved/undoable. Adjacent audio/instrument strips in mixer order
  leak in both directions; bus, aux and VCA strips break adjacency, while
  hidden MIDI tracks and the pinned master are excluded. Each leak taps the
  donor after inserts, before its fader, and sums into the recipient before
  its strip. Donor mute/solo/VCA mute (including automation) gates the leak;
  the recipient's strip controls the combined signal. Nodes never read
  another leak, avoiding recursive crossfeed. The graph's normal layout
  conversion and latency compensation apply. Existing sends or sidechains
  can make coupling cyclic: both directions of that pair are then omitted
  and an engine warning names it. Reordering rebuilds these connections.
  The generic model is resistive plus capacitive coupling,
  `H(s) = 1e-5 + 10^(-65/20) s/(s + 2π·10000)`, bilinear discretised,
  approximately −85 dB at 1 kHz, increasing toward −65 dB at high frequencies.
  This is a chosen generic response, not a measured console emulation.
  For scale, the [Xone:96 specification](https://www.allen-heath.com/content/uploads/2023/06/AP11645_2_XONE_96_USER_GUIDE.pdf)
  specifies inter-channel crosstalk below −85 dB at 1 kHz. Processing allocates
  nothing; independent state per direction/channel resets on locate. Offline
  export, parallel playback and render-ahead use the same realtime graph
  coupling. `faderframe-bench --crosstalk` measures its processing cost.
* **Instrument inserts.** Newly chosen instruments occupy visible insert
  slots and receive the track's MIDI there. The separate instrument field
  remains readable for old projects; replacing it through the chooser moves
  the new instrument into the insert chain as one undoable edit.
* **MIDI effects.** A plugin with notes in, notes out and no audio
  (`PluginCategory::MidiEffect`, or any plugin shaped like that) is a MIDI
  effect: `build::note_effects` chains a track's MIDI effects in insert
  order (the first fed by the clip player and live input, each by the one
  before) and gives every later note-taking insert the output of the
  effects above it; a legacy instrument slot (before every insert) gets
  what all of them made. MIDI tracks routed to an instrument track enter
  at its first MIDI effect; a MIDI track's own MIDI effects shape what it
  sends to an instrument track or a MIDI output. The render-ahead graph
  builds the same chain. Bypassed (or bypass-automated) MIDI effects pass
  their notes through (`nodes::plugin::pass_events`). The session places a
  MIDI effect before the track's first instrument, refuses one on an
  audio track and anything else on a MIDI track; the plugin browser has a
  MIDI Effects filter (chosen for MIDI tracks and slots before an
  instrument), the arranger's track menu "Add MIDI Effect…". Built-in
  MIDI effects follow the key and chord track: the timeline snapshot
  carries them in samples (`plugin_host::Harmony`,
  `PluginProcessContext::harmony`, `NO_HARMONY` outside a project).
* **Sidechains.** A `PluginSlot::sidechain` names a source track; a plugin
  with a second audio input gets the source's signal *after its inserts and
  before its fader, mute and solo* (a muted "ghost kick" still keys) as
  graph input 1 — PDC aligns it like any edge. CLAP ports and VST3 buses
  map graph inputs by index; VST3 activates its second input bus only when
  keyed (`ProcessConfig::sidechain`). The built-in compressor detects on
  the sidechain when connected, else on its input.
* **Frozen tracks** (`Track::freeze`) skip their MIDI, instrument and insert
  nodes: a clip player plays the rendered file into the strip, and the
  plugin host unloads their plugins (state stays in the slots).

## 6. Transport and timing

`TransportState` lives on the audio thread. The engine splits every device
callback into internal blocks of at most `max_block_size` frames **and** at
loop boundaries, so loops are sample-accurate for any buffer size (verified
for 44.1/48/88.2/96/176.4/192 kHz × 32…4096 and odd 441-frame buffers in
`tests/formats.rs`). Each block gets a `TransportInfo` (position, tempo,
meter, bar, loop, play/record state) for processors and plugins, plus a
`discontinuity` flag on stop/locate/wrap so note generators release notes.

**Scrubbing** (`TransportCommand::Scrub`) plays a short snippet (~70 ms)
from a position, faded in and out (1/16 of its length at each end), and
then returns there. Further scrub commands extend the snippet while the
pointer follows on, or fade it out and jump; while playing normally a scrub
only locates. The published snapshot reports scrubbing apart from playing,
so the control side treats it as stopped (no automation writing, follow
scrolling or MIDI clock).

## 7. MIDI and instruments

Events always carry a frame offset inside the block (`TimedMidiEvent`).
`MidiBuffer` is fixed-capacity and sorted (note-offs before note-ons at the
same offset); overflow drops and counts. The MIDI clip player emits events at
exact offsets; the built-in synth renders between event offsets (and
handles RPN 0 bend range, the MPE zone message, pressure and CC 74).

### Live MIDI input (keyboards and controllers)

* **Devices.** `faderframe-midi-io::MidiHub` connects every enabled input of
  the ALSA sequencer (via `midir`; PipeWire's MIDI bridges appear there too;
  CoreMIDI and WinMM on macOS and Windows)
  and adds virtual inputs (the built-in "FaderFrame Keyboard" used by
  tests, scripting and future on-screen keys). Ports are identified by name
  without the sequencer's client:port numbers, so a device keeps its routing
  and enable state across replugging; the session rescans every two seconds.
  Hardware opens only when the shell calls `Session::start_midi`
  (`FADERFRAME_NO_MIDI=1` skips it) — tests never touch real devices.
* **Two paths.** Every channel message is stamped on arrival (`MidiClock`)
  and pushed into one bounded queue for the audio thread and, as a copy,
  into a channel for the control thread (`MidiControlFeed`): learn,
  mappings and activity work without an audio stream. A new engine gets a
  fresh queue (`MidiInputSender::renew`) without reconnecting devices.
* **Scheduling.** Once per device callback the processor drains the queue
  and places each event by arrival time — one block duration ago → frame 0,
  just now → the end — so live input has a constant latency of one block
  and keeps its timing instead of jittering by up to a block. Chunks see
  their share in `EngineContext::midi_input`.
* **Tracks.** Instrument and MIDI tracks have `InputRouting::Midi { port,
  channel }` (any/one port, omni/one channel; new tracks default to all).
  A `MidiInputNode` per such track filters and passes events to the
  instrument while the track is *live*: monitoring `Input`, or `Auto` when
  armed — or selected while no track is armed. The session computes the
  live set; the node reads a per-track parameter slot and sends note-offs
  for held keys when the track stops being live (no hanging notes).
* **Recording.** Armed instrument/MIDI tracks record with the audio ones
  (or alone): a `MidiRecorder` on the audio thread copies their filtered
  events inside the record window into a ring with timeline positions and
  pass numbers; the session pairs them into notes (shown live in the
  arranger), moves them back by one block plus the output latency (plus the
  user offset) and places a bar-aligned MIDI clip per track — on top in
  Takes mode, carving earlier MIDI in Replace mode; loop recording merges
  passes or keeps the last one (`LoopRecordMode::LastPass`).
* **Controller mappings (MIDI learn).** `Project::midi_mappings` map a
  source (port or any, channel, CC / pitch bend / aftertouch / note) to any
  automatable parameter of a track or to a transport function. Learning
  takes the next suitable control (notes only for switches and transport).
  Mapped values go through the same `Command`s as the mouse inside a
  gesture that closes after 400 ms of rest — one undo step per twist, and
  Touch/Latch automation writing records controller moves. Mapping
  commands are undoable and saved with the project. Modes
  (`MappingMode`): absolute, soft takeover (the parameter follows only once
  the control reaches it, and again after the mouse moved it) and three
  relative-encoder encodings. Mapped controls are *consumed*: an atomic
  bitmap per port and channel (`ConsumedControls`) keeps them from
  instruments and recordings.
* **Controllers in clips.** `MidiClip::controllers` holds step lanes per
  controller and channel (CC, pitch bend, channel pressure). Recording puts
  controller moves there; playback sends them with the notes (at equal
  times: note-offs, controllers, note-ons). The clip player *chases*:
  starting or jumping mid-clip first sends each controller's current value,
  and on stop or jump moved controllers return to rest (pedals up, bend
  centred). Splitting a clip carries the values across the cut.
* **Per-note expression and MPE.** Expression is stored with the notes,
  not the channels: `MidiClip::expressions` holds a `NoteExpression` per
  note (pitch in semitones, pressure, timbre, vibrato and expression 0–1,
  volume in dB, pan −1…1; points relative to the note's start, linear
  between them), so notes can be moved, transposed, copied, split and
  deleted with their expression (the session's note operations carry it;
  clip splits move it with the notes). On a plain track the snapshot sends
  the curves to the instrument natively: `MidiEvent::NoteExpression`
  (fixed-point plain value, sorted after its note-on, sampled every 1/128
  quarter where it moves by the kind's resolution). The CLAP processor
  gives every note-on a note id (`faderframe_midi::NoteIds`) and sends
  `CLAP_EVENT_NOTE_EXPRESSION` addressed by key and channel (id −1: u-he
  Diva matches only those); the VST3 processor sends
  `NoteExpressionValueEvent`s for the types the plugin lists through
  `INoteExpressionController` (normalised: volume 0.25 = 0 dB, tuning ±120
  around 0.5) and pressure as `PolyPressureEvent`, both addressed by note
  id; the built-in synth applies them per voice; MIDI outputs drop them.
  `PluginInstance::note_expressions` reports what an instrument accepts
  (the piano roll says when it does not take a lane's kind). A track with
  `Track::mpe` (`MpeConfig`: member channels, bend range ±48) plays MPE
  instead (pitch, pressure and timbre only):
  the timeline snapshot gives each note its own member channel (the least
  recently used free one, else the one freeing first), sends the initial
  values right before the note-on and samples the curves every 1/128
  quarter into pitch bend, channel pressure and CC 74 where the MIDI value
  changes; the clip player announces the zone (RPN 6 on the master channel,
  RPN 0 on every member) whenever playback starts. Recording on an MPE
  track turns each note's member-channel pitch bend, pressure and CC 74
  (from just before its note-on to its note-off) into its expression,
  thinned to the points where the curve bends; master-channel controllers
  stay lanes.
* **Following an external clock.** System messages (clock, start,
  continue, stop, song position, MTC quarter frames, SysEx) bypass the
  realtime queue: they reach the control side through the feed's system
  channel, timestamped. `session::sync` follows MIDI clock (24 per quarter;
  Start makes the next clock beat 0; song position moves while stopped) or
  MTC (eight quarter frames make a timecode two frames long; 24, 25,
  29.97 drop-frame and 30 fps; full-frame SysEx moves; a pause of 150 ms
  stops). To start, `EngineController::chase` locates so that the engine is
  at the master's position plus the output latency *as of the message's
  timestamp* — the engine adds the time until it applies the command — and
  plays, through the same path as the play button (recording, automation
  writing). Each callback publishes its start time and position, so every
  tick measures the distance to the master; three ticks beyond the
  tolerance (15 ms) re-lock. The master's tempo is a least-squares fit over
  up to eight seconds of clock timestamps (a gap, burst or tempo jump
  restarts it); with "follow tempo" the project tempo is set to it — one
  undoable edit — when the master starts or stops, never while playing
  (running audio clips would jump). Settings (source, port, MTC offset)
  are per machine (preferences).
* **SysEx.** `MidiClip::sysex` holds messages at musical times; SysEx
  arriving on a recording track's input is placed where it was heard and
  becomes part of the take. For tracks with an external MIDI output the
  session schedules the messages 100 ms ahead to the output sender with
  exact due times from the engine's time-to-position mapping; each window
  continues in the timeline where the last one ended (position estimates
  wobble between ticks) and is split into timeline segments at loop wraps;
  a locate or stop (the engine is not where it was predicted) cancels what
  was scheduled (a generation counter in `MidiOutputs`). `.syx` files can
  be imported into clips and sent to any output from the preferences.
  **To plugins** SysEx travels the realtime path without allocating: a
  `MidiBuffer` keeps a preallocated byte area (16 KiB per block) and
  `MidiEvent::SysEx(SysexRef)` refers into it, tagged with the buffer's id
  — `push_sysex` copies bytes in, `push_from`/`merge_from` copy events with
  their bytes to another buffer, and a plain `push` of another buffer's
  SysEx is dropped, never misread. Clip SysEx is in the snapshot
  (`MidiRegion::sysex`, played by the MIDI clip player); live SysEx from an
  input port goes from the control side through a byte ring
  (`EngineController::send_live_sysex`, whole frames) to the tracks playing
  live from that port. CLAP gets `CLAP_EVENT_MIDI_SYSEX`, VST3 a
  `kMidiSysEx` data event, Audio Units `MusicDeviceSysEx`, sandboxed
  plugins the bytes in their block (`sysex_in`); MIDI outputs drop it there
  (the control side sends clip SysEx to devices).
* **Auditioning and step input.** The editor plays notes on a track's
  instrument through a reserved port (`AUDITION_PORT`) that the track's
  `MidiInputNode` takes whatever its live state, never recorded. Step input
  turns MIDI-keyboard notes (via the control feed) into notes at a cursor
  of the open clip — overlapping keys make a chord, the release advances.
* **MIDI output.** `Track::midi_output` sends a MIDI track (clips and live
  input, optionally on another channel) to an external device. Output
  nodes have the graph role `EventOutput`; after each chunk the processor
  reads them and queues messages stamped with the time this callback's
  audio is heard (buffer + output latency). `faderframe-midi-io::MidiOutputs`
  runs a sender thread that orders them by due time and sends each when
  due (ALSA via midir; virtual capture outputs for tests). MIDI clock (24
  ppqn, Start/Continue with Song Position, Stop, re-sync on loop wraps) is
  generated on the audio thread for the outputs selected in preferences.

* **Standard MIDI Files** (`session::midifile`, parsed and written with
  `midly`). Import makes an instrument track per MIDI track (format 0 files
  are split per channel, channel 10 as drums) with notes, controllers
  (CC, pitch bend, channel pressure) and SysEx; into an empty project it
  also takes tempo and meter changes — all one undo step. Export writes
  format 1 at 960 PPQ: a conductor track (tempo, meter) and one track per
  instrument/MIDI track, or only the selected MIDI clips.

## 8. Plugins

`faderframe_plugin_host` mirrors how CLAP/VST3 split a plugin:

* `PluginInstance` — control thread: descriptor, parameters, state,
  latency/tail, activation (`create_processor`).
* `PluginProcessor` — audio thread: `process(ctx, io) -> ProcessStatus`
  (no allocating `Result` on RT), `reset()`.

Neither trait assumes the plugin is in-process; a sandboxed plugin is a proxy
pair speaking IPC with shared-memory audio (see *Sandboxed plugins* below).
`PluginHost` (engine) owns one instance per slot. Built-ins: latency probe (used to test PDC
end to end), the EQ, the Program EQ and the stock devices — compressor,
limiter, gate, de-esser, saturator, utility (id `faderframe.gain`),
delay (id `faderframe.echo`), reverb, modulation, tuner, the MIDI effects
arpeggiator, chord, scale and note echo, and the instruments synth,
sampler and drum sampler (see *Built-in devices*).
Failed plugins
are bypassed and flagged; missing formats pass audio through with a
warning.

### Built-in devices

The EQ and the Program EQ are built-ins with editors of their own
(`faderframe-view-devices`, opened by `plugin_window::open_device` in a
window with the usual Bypass/Presets header). A built-in instance can hand
out an `AnalysisTap` (`PluginInstance::tap`, `Session::plugin_tap`): its
live `ParamValues` (so the editor follows automation), lock-free stereo
rings of the audio going in, coming out and arriving at the sidechain
(filled only while an editor calls `watch()` each frame), PultEQFx's level
meters (peak taken per frame, held peak, 300 ms RMS and a 200 ms figure),
published values (per band: dynamic gain, trigger level, threshold, and a
spectral band's gain at 64 frequencies) and what the editor wants to hear
alone (a band's region, or a band's trigger). Edits are ordinary
`SetPluginParameter` commands in gestures, so they undo and automate like
any other; an editor's own settings (analyser, display, the external
spectrum's source) are `Action::SetDeviceView` values the session keeps per
instance, outside the project and the undo history (the EQ's band
clipboard lives there too, so menus and other EQs reach it). Real-time
safety of both devices is covered by `engine/tests/realtime_alloc.rs`
(dynamic, sidechain-keyed, freely triggered, spectral, mid/side, steep,
fractional and brickwall bands, linear and natural phase, character,
bypass, automation, listening).

* **EQ** (`plugin_host::eq`), after FabFilter Pro-Q 4's feature set: 24
  bands, each unused/on/bypassed with a stereo placement (stereo, left,
  right, mid, side). Shapes: bell, low/high shelf, low/high cut, notch,
  band pass, tilt shelf, flat tilt and all pass. Slopes: cuts take any
  slope from 0 to 96 dB/oct (whole Butterworth orders plus, for a fraction,
  a ladder of first order steps an octave apart) and brickwall (order 32);
  shelves and tilts are Butterworth shelving filters of any order
  (Holters & Zölzer); notches and band passes cascade sections that keep
  the −3 dB edges; all passes go from first order up. Parameter ids are
  stable: the first version's band ids keep their meaning, new fields use
  a second id block (`BAND_BASE2`), retired ids are never reused.
  `eq::design` builds every shape as a cascade of analog sections and
  turns each into a biquad that keeps its shape to Nyquist: poles by
  impulse invariance (Vicanek, *Matched Second Order Digital Filters*),
  numerators exact where the shape is defined (DC, centre or corner) and
  least-squares fitted to the analog magnitude up to Nyquist; first order
  sections likewise (fitted from DC when their corner is past Nyquist);
  notches solve their damping for the analog −3 dB edge; below ~0.001
  rad/sample sections are bilinear. The same cascade gives the analog
  magnitude and phase (`AnalogBand`) the FIR modes are designed from.
  **Processing modes** (not automatable: they change the latency, and the
  instance asks for a graph rebuild): *zero latency* (the matched
  sections); *natural phase* (`eq::natural`): the sections followed by a
  1024-tap correction FIR — per frequency the analog response over the
  digital one (a 2 × 2 system when bands work on one side, regularised
  where the digital one has nothing), 64 samples ahead of its main tap —
  so magnitude and phase are the analog filter's at a latency of 128
  samples; *linear phase* (`eq::linear`): the static bands' analog
  magnitudes as zero-phase FIRs of 4096–65536 taps. Both FIRs are uniformly
  partitioned convolutions (`eq::fir`) whose kernels a design thread
  builds when the parameters move and the audio thread crossfades to.
  **Dynamics** (`eq::dynamics`): a band moves by its range as its trigger
  rises over the threshold, through a 6 dB soft knee; the trigger is the
  input or the sidechain (per band), filtered to the band's region (a band
  pass round a bell, a shelf's side) or by free 24 dB/oct low and high
  cuts, and the trigger can be heard on its own. Auto mode sets attack and
  release from the band's frequency and puts the threshold a little over
  the trigger's own long-term level; custom mode scales the times (50 % =
  automatic) and takes the band's threshold. The detectors run where the
  sections run, fed by the input and the sidechain delayed to match.
  **Spectral dynamics** (`eq::spectral`): a short-time Fourier stage
  (square-root Hann, 75 % overlap, 1024–4096 frames, which is also its
  latency) where each spectral band acts per frequency: the trigger's
  power, optionally tilted by 3 dB/oct, smoothed across frequency by the
  density and over time by attack and release, against the threshold or
  (auto) the region's octave-smoothed level; the gain is weighted by the
  band's normalised shape and added to its static gain (spectral bands are
  linear phase). **Output**: character (`eq::character`: clean; a
  transformer's low-end saturation; a tube's asymmetric curve driven below
  the top octave, its harmonics DC-blocked), gain, pan (left/right or
  mid/side), phase invert, auto gain (pink-noise loudness), a gain scale,
  gain-Q interaction and a 10 ms bypass ramp against the input delayed by
  the latency. Frequency, gain, Q and a cut's fractional slope glide
  (15 ms); a band whose structure changes fades out and back in; a band
  not heard yet starts where it is set. **EQ Match** (`eq::matching`):
  the reference-minus-input spectrum smoothed to a third of an octave and
  centred, bands placed where most is left (bells at the extremes, shelves
  at the ends) and refined by pattern search.
  The editor (`view-devices::eq`): a display with the analyser (pre, post
  and an external spectrum — the sidechain or any other EQ's output — with
  resolution, speed, range, tilt and freeze; collisions glow red), every
  band's curve (a dynamic band's reach shaded, spectral movement per
  frequency) and the overall response per part of the signal (stereo,
  left, right, mid, side); floating band controls under the band in focus
  (the gain knob carries the dynamic range ring; ">>" opens threshold,
  sidechain, attack, release, trigger and spectral settings); values next
  to a band to drag, scroll or type ("1k", "A4", "C#2+13", "2x"); multi-
  selection, rectangle selection, copy and paste; EQ Sketch (a drawn curve
  turned into bands, `eq::sketch`); Spectrum Grab (rest on the spectrum,
  drag a peak into a bell); the piano display; a zoomable frequency scale;
  A/B; the sidechain source picked in the editor; the instance list (every
  EQ in the project with its spectrum and collisions; reference, open,
  match) and EQ Match (reference: the sidechain, another EQ, or the input
  recorded earlier).
* **Program EQ** (`plugin_host::program_eq`): PultEQFx by Simon Huber,
  used under the MIT licence — the passive LC/RC network of the classic
  tube program equaliser solved by nodal analysis (trapezoidal companion
  models, Cholesky factorised at control rate), the push-pull tube make-up
  stage and its transformers, halfband oversampling (1–8×, switchable
  while running) and a fixed 74-sample latency (the dry signal is held
  back as long with the power off). Its "Low End Punch" preset is a
  program of the built-in (`PluginInstance::programs`), selected as one
  undo step. The panel (`view-devices::program_eq`) draws PultEQFx's
  faceplate, lettering, light and meters with FaderFrame's painter and
  uses its renders (knob filmstrip, switch knob, lamp, screws:
  `crates/faderframe-view-devices/assets/program-eq`) through
  `Painter::image` — filmstrip frames, never a rotated picture, each tinted
  by its distance from the panel's lamp.

**MIDI effects.** `devices::{arpeggiator, chord, scale, note_echo}` on
`devices::midi_fx` (a fixed-room `Schedule` of events due later on the
effect's own sample clock, `Sounding` notes counted per channel and key
so overlapping copies of a key never leave it stuck, pass-through of
everything else; a reset sends every sounding note off on the next
block). The *Arpeggiator* plays the held keys in nine orders (up, down,
up-down, down-up, converge, diverge, as played, random, chord) over one to
four octaves, on the song's grid while playing (the rate a division,
swing delaying every second step) or from the first key when stopped; a
new chord's first note plays at once unless the grid's next step is
close; gate (past 100 %: legato), fixed or played velocity, repeats,
hold. `order_index`/`sequence` are shared with its editor. The *Chord*
makes each note a chord: up to five intervals, the key's triad or
seventh (the key track's key where the note is, or its own), or the
chord track's chord voiced round the note; strummed up or down, added
notes softer; notes of a strum not started when the key is let go never
start. The *Scale* keeps notes in a key (the key track's or its own):
nearest (ties down), up, down or blocked, then by scale degrees and
octaves; note-offs, poly pressure and note expressions follow each note
to where it went. The *Note Echo* repeats each note (a division of the
tempo or milliseconds apart, up to 16, each the feedback's share as loud
until too soft to play, optionally moving by semitones), each repeat as
long as the played note, with or without the note itself. Their editors
(`view-devices::{arpeggiator, chord, scale, note_echo}`, accent
`Accent::Midi`, `keys::keyboard`) show the pattern as a small piano roll,
the chord on a keyboard with its name, the key's notes with their
degrees, and the repeats in time. Engine tests (`midi_effects.rs`) render
a sine synth and check the pitches it plays; `the_midi_effects_do_not_
allocate` plays all four chained live over a key change.

**Stock devices.** The other built-ins with editors live in
`plugin_host::devices` (one module each: parameter ids — stable, never
reused —, `parameters`, `format`, `latency`, published values and the
processor; tests with the shared `devices::rig`) on a common DSP toolkit,
`plugin_host::dsp`: the EQ's matched designs as `dsp::filter::Filter`, TPT
state-variable and one-pole filters, DC blocker, PultEQFx's halfband
oversampler, a Hermite-interpolated delay line, envelope followers
(peak, mean square, a programme-dependent `DualRelease`), tempo-synced
LFOs, parameter smoothing and a BS.1770 4× true-peak meter. Their editors
are `view-devices::kit` faces: a `Face` lays out a display and a deck of
titled sections whose controls are bound to parameter ids
(knob/small knob/toggle/choice/segments; drag, Shift fine, wheel,
double-click to type a value, Ctrl-click default, right-click for
default/automation/MIDI learn; every drag one undo step), meters for in,
out and reduction, and `kit::History`/`kit::Spectrum` for the scrolling
level history and the analyser. Peaks a face shows travel through
`AnalysisTap::raise_value`/`take_value` (held until the editor takes
them), so a 40 fps history misses no transient. Their real-time safety is
covered by `the_stock_devices_do_not_allocate`.

* **Compressor** (`devices::compressor`): five styles — Clean (feed
  forward, RMS), Punch (peak), Opto (feedback, programme-dependent
  release), Vintage (feedback, fast, colour), Bus (glue) —; the feedback
  styles' slope is compensated so the ratio knob reads true; threshold,
  ratio up to ∞, soft knee, attack, release with auto release, range,
  auto makeup, lookahead (latency), RMS or peak detection, stereo link,
  colour (saturation), mix, an external sidechain (on by default once one
  is routed) with high/low cuts and listen. Editor: the transfer curve
  with the level moving on it (drag to set the threshold) and a history.
* **Limiter** (`devices::limiter`): a lookahead limiter that guarantees
  its ceiling — per channel the minimum needed gain over the lookahead
  window, averaged over an attack window no longer than the lookahead
  (Transparent: all of it, Punchy: a half, Aggressive: a quarter), so the
  gain is down before the peak arrives; release (auto: slower while
  limiting goes on); stereo link; a true-peak mode measuring the 4×
  oversampled output (6 samples more latency); unity gain listening.
  Editor: history of in, out and reduction with the ceiling, held readouts
  of the reduction and the output's (true) peak.
* **Gate** (`devices::gate`): gate (with hysteresis and hold, opening and
  closing in dB at the attack/release rates), downward expander (ratio,
  range) and ducker; key from the input or the sidechain through high and
  low cuts, listen, lookahead. Editor: the curve with both thresholds
  (drag), the history and an open strip.
* **De-esser** (`devices::deesser`): the detector hears above the
  frequency (24 dB/oct) or round it; absolute or relative detection (the
  threshold follows the take's smoothed level: it reads as set for a take
  peaking at −18 dB); split mode turns down only the band (a dynamic high
  shelf or bell, the EQ's designs), wide mode everything; range, attack,
  release, link, lookahead, listen. Editor: the spectrum with the
  detector's band and the cut it makes now (drag the frequency, wheel the
  width) and a history.
* **Saturator** (`devices::saturator`): low cut → drive → one of six
  curves of unit slope at zero (soft `tanh`, tape arctangent with a head
  bump and high-frequency loss, asymmetric tube, transistor's hard knee,
  sine fold, hard clip) with bias, run at 1–8× through the halfband
  oversampler (latency, the dry signal held back as long) → DC blocker →
  tilt round 1 kHz → high cut; auto gain is static (what the curve does to
  a −12 dBFS sine), so the curve's dynamics stay. Editor: the curve as set
  with the input's reach and the spectrum in and out.
* **Utility** (`devices::utility`, the built-in Gain's id and parameter
  0): one ramped 2 × 2 matrix for channel choice (stereo, left, right,
  swap, mid, side), polarity, width, balance, gain and mute — at rest the
  identity times the gain, so old projects sound the same — plus a DC
  filter and mono bass (Linkwitz–Riley split of the side; the mid through
  the same crossover's all pass, so above it both stay in phase;
  crossfaded on and off). Editor: vectorscope, correlation and balance.
* **Delay** (`devices::delay`, the built-in Echo's id; its five
  parameters keep their meaning): stereo, ping-pong or mono; free or
  synced (note divisions), stereo offset, feedback to 110 % into a loop
  that is linear to full scale and then rounds off; per pass the Echo's
  damping, a low cut, saturation and a style (digital; tape: arctangent,
  head bump, high-frequency loss, wow; analog: a bucket brigade's 4.5 kHz
  band limit); wow and flutter; time changes glide; ducking, width,
  freeze. Editor: the repeats on a timeline (drag for the time) and one
  pass's spectrum.
* **Reverb** (`devices::reverb`): pre-delay; early reflections (a type's
  12-tap pattern scaled by the size); four allpass diffusers a side into a
  16-line FDN with Hadamard mixing (orthogonal), line lengths spread
  geometrically over the type's range and gliding with the size, slowly
  modulated taps, and per line a three-band decay (one-pole splits at
  250 Hz and the damping frequency, each band's gain exactly what its
  decay time asks for the line's length: bass × the bass multiplier,
  highs a third); orthogonal output taps for decorrelated sides. Types
  Room, Hall, Plate, Chamber, Ambience; early/late balance, width, wet
  cuts, freeze (lossless, modulation off), ducking. Tests measure RT60
  (Schroeder integration) within 12 %. Editor: the decay picture (pre-
  delay, reflections, bass/middle/high tails; drag for the decay) and the
  decay time across the spectrum.
* **Modulation** (`devices::modulation`): chorus (1–4 voices a side),
  ensemble (three voices, slow and fast LFOs), flanger (feedback ±95 %),
  phaser (2–12 first-order allpass stages swept exponentially round the
  centre, feedback) and vibrato; free or synced LFO in six shapes with a
  stereo spread; wet high cut and width. Editor: the LFO with each side's
  position, and the voices on the delay or the comb or notches right now
  between the sweep's extremes.
* **Tuner** (`devices::tuner`): passes or mutes; the pitch is found in
  the editor (`view-devices::tuner`: McLeod's normalised square
  difference over 4096 frames by FFT, parabolic peak, clarity ≥ 0.8,
  median of five), within a cent from 25 Hz to 4.2 kHz; needle or strobe,
  reference A4 400–480 Hz.
* **Synth** (`devices::synth`; the first nine parameters, the MIDI
  handling — pedal, bend with RPN 0, MPE zones, pressure, CC 74, mod-wheel
  vibrato — and per-note expressions are the first Synth's): two PolyBLEP
  oscillators (saw, square with pulse width, triangle, sine; octave,
  semitone, level), up to seven unison copies (detuned, spread across the
  width), sub and noise; drive into a state-variable filter (12/24 dB low
  pass, band, high) with key tracking, velocity and its own envelope or
  the amplifier's; an LFO (free or synced, six shapes) to pitch, cutoff,
  level and pulse width; poly (up to 32 voices), mono or legato with
  glide. Editor: two decks of sections under the oscillator cycles, the
  filter's response and reach, and both envelopes.
* **Sampler** and **Drum Sampler** (`devices::{sampler, drums}`): what
  they play is a `samples::SampleDoc` (a file per slot) packed into the
  plugin state after the parameter block (`samples::pack`: `FFSD`, length,
  parameters, JSON), so it saves, undoes (`Command::SetPluginState`) and
  travels with presets. Loading (`samples::load`: the importer's decoder,
  any rate — playback steps by the ratio; SFZ files parsed into zones:
  key/velocity ranges, roots, tune, volume, pan, loops, `ampeg_*`, release
  triggers, `group`/`off_by`, round robins, random layers) happens on the
  control thread; the set is swapped into a `TryCell` the processor only
  ever `try_lock`s (voices of an older generation stop; the old set is
  dropped on the control thread) and handed to the editor through
  `AnalysisTap::set_assets`. Sampler: one sample across the keys (root,
  loop off/forward/while held with a crossfade, start, reverse, key
  tracking) or an SFZ; ADSR, filter with envelope, velocity, up to 64
  voices. Drum sampler: 16 pads from a first note, each with level, pan,
  tune, attack, decay, start, choke group, reverse, one-shot or gate, low
  pass and velocity, four layers a pad. Editors: waveform with draggable
  start/loop markers and a keyboard with the zones; the pad grid (lit on
  hits, double-click to load, several files fill the pads) with the
  picked pad's waveform and controls; files can be dropped on the
  sampler or a pad. Samples are the project's (`session::samples`): picked
  or dropped files are copied into `<media>/Samples` (an SFZ instrument is
  referenced where it lies, with its samples) and decoded on a worker
  thread into a process-wide cache (`samples::load_cached`, shared while
  anyone holds a sample — offline renders reuse the live ones), then one
  "Load Samples" undo step sets the state and the instance loads from the
  cache at once; concurrent loads merge into the document as it is when
  each lands. In memory the paths are absolute; the project file stores
  them relative to itself (`samples::map_states` in `save_as`/`open`), and
  the first save moves the scratch folder's samples with the media
  (`media_moves`, also applied to states brought back by undo).

* **Automation from plugin editors.** Formats report the user's moves in a
  plugin's own GUI as `EditorEdit`s (begin, value, end): CLAP from the
  processor's output events (gesture begin/end and parameter values, through
  a wait-free queue), VST3 from `beginEdit`/`performEdit`/`endEdit`. The
  session records them like control moves: Touch ends with the gesture end
  (or after 750 ms of rest for plugins that send none), Latch and Write when
  playback stops.
* **Presets.** User presets are `.ffpreset` files (JSON: plugin reference,
  state, explicit parameter values) under
  `$XDG_DATA_HOME/faderframe/presets/<format>/<id>/`; VST3 factory presets
  (`.vstpreset` from the standard folders, matched by class id) are listed
  too and loaded into the plugin's state. Loading is
  `Command::SetPluginState` — one undo step; the engine follows a changed
  slot state.
  Built-in factory presets live in `plugin_host::presets`: 216 across
  the EQ, Program EQ, compressor, limiter, gate, de-esser, saturator,
  utility, delay, reverb, modulation, the four MIDI effects and Synth
  (MIDI effect presets are played through by a test: notes, none stuck). Samplers, the tuner and
  the latency probe have none. Each preset expands its overrides over
  the device's defaults, replacing every parameter when selected.
  `BuiltinInstance::programs`/`select_program` expose them through the
  existing program API; the editor labels that section "Factory Presets".
  The original Program EQ "Low End Punch" remains index 0. On Aux tracks,
  `session::programs` overrides a built-in delay/reverb's mix to fully wet
  before the new state is captured, so loading and undo remain one step;
  inserts on other track kinds keep the preset's mix. User preset files
  always retain the saved mix. Synth presets are balanced: played as meant
  (a mono bass on C2, a mono lead on C4, a poly patch as a four-note
  chord), each peaks at −18 LUFS momentary with at least 1 dB of headroom
  (`the_synth_presets_are_balanced` prints the volume each needs). Preset
  tests validate every parameter and render every program; session tests cover undo, rebuilds and project
  save/reopen with subsequent user tweaks.
* **Programs** (`PluginInstance::programs`/`select_program`; VST3 program
  lists) are listed with the presets. Selecting one changes the plugin's
  whole state, so `session::programs` makes it one undo step like a
  preset: the state from before is captured, the program selected, and
  once the processor has taken it (`changes_pending`, at the latest after
  a second) the new state is recorded as "Select Program". The program
  itself is not stored (reapplying it on load would overwrite later
  tweaks). Lists of numbered slots only ("Program 3", "ProgramChange 12" —
  u-he, Arturia) are MIDI program change targets and not shown.
* **Precision.** `ProcessConfig::double_precision` (Preferences → Audio,
  applied at once: the plugins are reactivated) gives plugins that offer it
  64-bit buffers — VST3 `kSample64`, CLAP ports with `SUPPORTS_64BITS` —
  converted around the plugin from preallocated double buffers; the
  graph stays 32-bit.
* **Inserts** move and copy between slots and tracks
  (`Action::MovePlugin`/`CopyPlugin`: a copy gets a new id and the
  original's captured state).

A plugin node's key includes its latency, bypass and the instance's
activation count (`PluginInstance::activation`): a restart (requested by
the plugin, e.g. when its editor opens, or a latency change) deactivates
the old processor, so the rebuilt graph must keep the fresh one rather than
adopt the old, dead one. Instrument tracks feed their MIDI (clips and live
input) to the instrument slot and to every insert whose plugin takes notes;
an instrument track without an instrument routes MIDI tracks to the first
such insert. Instrument tracks start without an instrument.

Opt-in tests run installed plugins end to end:
`FADERFRAME_TEST_BUNDLES=<bundle>:<bundle> cargo test -p faderframe-bench
--test installed_plugins -- --ignored` (offline: effects keep mono/stereo
tracks audible, instruments play a clip) and the same with
`-p faderframe-session` (live session: inserting while playing with the
editor opening, instruments playing live MIDI as instrument and insert).

### Sandboxed plugins

`faderframe-plugin-sandbox` runs each CLAP/VST3 (and, on macOS, Audio
Unit) instance in a helper process of its own: the application's own binary started as
`faderframe --plugin-sandbox`, which hosts the plugin with the in-process
factories (`faderframe_ui::plugins::in_process_registry`).
`SandboxedFactory` wraps a format's factory and, while sandboxing is on
(`set_enabled`, from `Preferences::sandbox_plugins`), instantiates through a
helper; the engine only sees ordinary `PluginInstance`/`PluginProcessor`s.
The operating system's part is `sys` (one interface; `unix.rs`:
socketpair, pipes at fixed descriptor numbers in the helper, POSIX shared
memory, `poll`, `PR_SET_PDEATHSIG` on Linux; `windows.rs`: a
single-instance local named pipe with overlapped I/O, auto-reset events, a
pagefile-backed file mapping, all named in the helper's environment).

* **Control** goes over the socket or pipe (`wire`: JSON plus a binary
  payload for states and preset files, framed with a magic number and size
  limits).
  The host answers what the UI reads every frame (parameter values, editor
  requests and edits, latency) from a cache that the per-tick `Poll`
  refreshes with the changes only.
* **Audio**: per activation the host creates a named shared-memory block
  (`shm::Block`: a header with sync words, the block's shapes, transport,
  parameter events, MIDI and note expressions both ways as fixed-layout
  `Wire*` records — never Rust enums —, then the audio) and unlinks the
  name once the helper has mapped it. Per block the audio thread writes
  the request, bumps `seq`, wakes the helper (`Signal`: a byte into its
  pipe, or `SetEvent`) and waits (`Waiter`) for `done == seq`; the helper's
  audio thread (copying the host audio thread's scheduling — SCHED_FIFO,
  Mach time constraints or MMCSS —, flush-to-zero) runs the real processor
  on reused buffers and answers the same way. Whatever the helper wrote is
  decoded defensively (counts clamped, unknown events dropped, non-finite
  samples zeroed). A round trip costs 7–9 µs.
* **Failure**: a dead helper is noticed at once (its pipe closes; on
  Windows the wait includes its process handle); one that misses
  `BLOCK_TIMEOUT` (250 ms) is given up and killed by the next poll.
  The processor then returns `ProcessStatus::Error`, so the engine bypasses
  the plugin; the session reports it once (`session::sandbox`) and
  `Action::ReloadPlugin` drops the instance and builds it again from its
  slot. Sandboxed plugins that report unsaved state have it taken into
  their slots every 5 s, so a reload restores recent settings.
* **Editors** run in the helper. Its main thread services the plugin
  GUI's descriptors and timers along with the control stream: Unix `poll`s
  them (macOS also hands AppKit its events and run loop every 10 ms while
  an editor is open, 50 ms otherwise); Windows reads the pipe on a thread
  that wakes a `MsgWaitForMultipleObjectsEx` message loop. On X11 and
  Windows editors embed into FaderFrame's parent window across processes
  (window ids and handles are global). A Windows child window sends its
  parent messages synchronously (on creation, resizing, destruction), so
  FaderFrame handles sent messages while it waits for the helper's answer
  (`wait_two_pumping`; posted messages stay queued) — otherwise both sides
  block. On macOS views cannot cross processes: the helper reports
  "floats", shows the editor in an `NSWindow` of its own (`mac.rs`; an
  accessory app, no Dock icon), handles resizes and the close button
  itself and reports closing with the poll; opening it again raises it
  (`PluginEditor::raise`). A helper exits when FaderFrame goes away
  (Linux: death signal; macOS: a watchdog on the parent pid; Windows: a
  thread waiting on FaderFrame's process), even with a plugin stuck on
  its main thread.
* **Tests**: `faderframe-plugin-sandbox/tests/sandbox.rs` starts its own
  test binary as the helper and compares sandboxed built-ins with
  in-process ones bit for bit, lets one helper die mid-block and another
  hang; `round_trip_cost` (ignored) measures the overhead.
  `tests/editor_windows.rs` (Windows) embeds a helper's child window into
  one of the test's and fails if that deadlocks. Both run on the Ports
  runners, and locally as Windows binaries under Wine:
  `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine PKG_CONFIG_ALLOW_CROSS=1
  cargo test -p faderframe-plugin-sandbox --target x86_64-pc-windows-gnu`.

### CLAP

* **Scanning** runs each bundle in a throw-away helper process
  (`faderframe --scan-clap <bundle>`, JSON on stdout), so a crashing plugin
  cannot take the DAW down. Results are cached by bundle size and mtime
  (`$XDG_CACHE_HOME/faderframe/clap-scan.json`); the shell rescans in the
  background at start-up and the plugin browser refreshes when the global
  catalog generation changes. The scan cache, helper protocol and
  `ScannedPlugin` are format-independent (`faderframe_plugin_host::scan`;
  directory bundles are stamped by the files inside).
* **Extensions** are queried once `init()` has returned (CLAP forbids
  asking earlier, and bridges such as yabridge only know theirs then), plus
  from within `init()` for plugins that call the host while initialising.
* **Threads.** A `ClapInstance` lives on the main (UI) thread and is not
  `Send`. Its live audio processor sits in a `TryCell` shared by every graph
  generation that references it: the audio thread `try_lock`s it (busy =
  one silent block, never a wait), the control thread may block briefly to
  reconfigure. `start_processing` happens on the audio thread; while
  processing — or stopping processing on behalf of the audio thread — a
  thread-local scope makes the thread-check extension report "audio thread"
  (offline renders run both roles on one thread).
* **Parameters** reach a running plugin as `ParamValueEvent`s from a
  wait-free queue (UI) and from automation (`ParameterEvents`, sample
  offsets within the block); while inactive they are flushed through
  `params.flush`. A slot's explicit values (`PluginSlot::parameters`, edited
  by `Command::SetPluginParameter`, one undo step per gesture) are applied
  after the saved state; the engine remembers the plugin's own value before
  the first explicit one so undo can restore it. Before saving, explicit
  values are refreshed from the plugin, so changes made in its own GUI win.
* **State** is the plugin's own blob, base64 in the slot, captured before
  save/render/engine rebuild and before edits that remove plugins.
* **Editors.** CLAP GUIs embed into a host window of the platform's own
  window system (`gui` API `x11`, `win32` or `cocoa`; see *Editor windows*
  below). On Windows the editor's scale is set from the display's DPI. On
  Linux FaderFrame is a Wayland client, so the parent is a top-level window
  on a separate X11 connection (`x11rb`, XWayland): created at the editor's size, with
  fixed-size hints for non-resizable editors (window managers float them),
  `WM_DELETE_WINDOW` closing the editor, configure events resizing
  resizable ones and `request_resize` resizing the parent. Plugins that
  cannot embed but can float open their own window. Editors open centred
  on the monitor showing FaderFrame — in X11 coordinates from RandR,
  matched by connector name, because XWayland's layout can differ from the
  Wayland one (e.g. physical pixels with zero scaling) — or where they were
  last; positions are kept per plugin instance in the workspace and saved
  with the project. Plugin GUIs run on the GTK main loop: descriptors
  registered through `posix-fd` become glib fd sources and `timer`
  registrations glib timeouts, reconciled every UI tick.
* **Generic editor.** Every plugin (built-ins — the EQ and Program EQ
  also have editors of their own —, plugins without a GUI, no X
  server) has a GTK parameter window with a
  presets menu: filter, module sections from CLAP's
  "Module/Name" paths, the plugin's own value text (`value_to_text`),
  sliders and switches, double-click to reset, live follow of automation
  and GUI changes, bypass, and a button to the plugin's own GUI. On Wayland
  the compositor places GTK windows; apps cannot.

### VST3

Bindings come from the `vst3` crate (generated from the MIT-licensed VST 3
SDK headers); FaderFrame implements the host side itself.

* **Modules.** A bundle's binary (`Contents/<arch>-linux/*.so`,
  `Contents/<arch>-win/*.vst3`, `Contents/MacOS/*`) is opened once per
  process — Linux: `RTLD_NOW | RTLD_LOCAL` and `ModuleEntry` with the dlopen
  handle; Windows: the optional `InitDll`; macOS: `bundleEntry` with a
  `CFBundleRef` — and never unloaded — plugins keep static state and threads that do not
  survive `dlclose`, and every engine shares the factory. Scanning uses
  `faderframe --scan-vst3 <bundle>` like CLAP (`vst3-scan.json`): classes
  of category "Audio Module Class" are described via `IPluginFactory2`,
  buses read from an initialised component, sub-categories ("Fx|EQ") mapped
  onto CLAP's feature tags. The id is the class id as 32 hex digits.
* **Set-up** follows the SDK's host workflow: initialise the component with
  a per-instance host object, use it as edit controller if it is one, or
  create the controller from `getControllerClassId`, initialise it and join
  both through `IConnectionPoint`; set the component handler and give the
  controller the component's state. The host object (`com::HostApp`) is
  `IHostApplication` (creating `IMessage`/`IAttributeList` for plugins that
  talk between their halves), `IComponentHandler(2)`, the editor's
  `IPlugFrame` and Linux `IRunLoop`. Its callbacks only record (edits,
  restart flags, resize requests, fd/timer registrations); the control
  thread acts on them when it polls — callbacks may come from any thread.
* **Programs** come from the program-change parameter (`kIsProgramChange`,
  often hidden): the names of its unit's program list (`IUnitInfo`), else
  the parameter's own value texts; selecting one sets that parameter in
  the controller and the processor.
* **Parameters** are normalised in VST3. FaderFrame shows continuous ones as
  0–1 and stepped ones as integers 0…steps, converting at the boundary; the
  plugin formats values (`getParamStringByValue`). Hidden parameters and the
  non-automatable proxies some frameworks register as MIDI controller
  targets (JUCE, u-he: 16 × 130) are not listed. Unit names prefix
  parameter names ("Unit/Name") like CLAP modules.
* **Processing.** Activation activates the main audio buses and the first
  event input, confirms the plugin's own arrangements, sets up 32-bit (or,
  when asked and offered, 64-bit) processing and hands an `Active` (processor, all bus buffers, the host's
  `IParameterChanges`/`IEventList` objects and the process context, all
  preallocated) to a `TryCell` like CLAP. Per block: UI and editor changes
  (offset 0, from a wait-free queue; they wait when the 512 queues of a
  block are taken), then automation (sample offsets), then MIDI: notes and
  poly pressure become events, CC/pitch bend/channel pressure become
  parameter changes through the plugin's `IMidiMapping` (VST3 has no CC
  events; the table is built at activation), a program change selects the
  program-change parameter's program (the controller follows), SysEx is a
  `kMidiSysEx` data event. Values the processor reports
  in `outputParameterChanges` go back to the controller; note output goes to
  the graph's event output. `reset()` is a `setProcessing` off/on cycle.
  The processor is `Send` and runs on whichever thread processes its node.
* **Editor edits** (`performEdit`) are forwarded to the processor and mark
  the project dirty; the engine's explicit-value sync never overwrites them.
* **State** is `FFV3` + length-prefixed component state + controller state;
  foreign blobs are taken as component state. Loading also feeds the
  component state to the controller (`setComponentState`).
* **Editors** use the same parent windows as CLAP (`X11EmbedWindowID`,
  `HWND` or `NSView`; Windows editors get the display scale through
  `IPlugViewContentScaleSupport`); `resizeView` is answered with `onSize`,
  and Linux run-loop file descriptors and timers become glib sources. VST3
  has no floating editors.

### Audio Units

macOS only (the crate is empty elsewhere). Hand-written declarations of the
AUv2 C API (`ffi.rs`, layouts checked by a test) — AUv3 extensions are
reached through it too.

* **Listing** reads the component registry (`AudioComponentFindNext` for
  effects, music effects and instruments) without instantiating anything,
  so it runs in-process and needs no scan cache. Ids are
  `type:subtype:manufacturer` four-character codes (`aufx:dely:appl`).
* **Instances** set non-interleaved `f32` stream formats (stereo, else
  mono), the maximum slice size, an input render callback that reads the
  block's input from a preallocated feed, and host callbacks (beat and
  tempo, musical time, transport state) that read a per-block transport
  copy; then `AudioUnitInitialize`. Parameters come from
  `kAudioUnitProperty_ParameterList`/`ParameterInfo` (meters skipped,
  value strings when the unit has them). State is the `ClassInfo`
  property list (binary plist bytes); `.aupreset` files are the same
  property list, listed from `~/Library/Audio/Presets/<vendor>/<name>`.
  Latency and parameter-list changes arrive through property listeners and
  become restarts/parameter refreshes on the next poll.
* **Processing** (`AuProcessor`, in a `TryCell` like the other formats):
  automation as scheduled immediate parameter events (or set at the block
  start for units that do not schedule), MIDI through
  `MusicDeviceMIDIEvent` with sample offsets, `AudioUnitRender` into
  preallocated buffers. The instance takes the cell before uninitialising
  the unit; a busy or empty cell renders silence.
* **Editors** are the unit's Cocoa view (`kAudioUnitProperty_CocoaUI`: a
  view factory class in a bundle) or CoreAudioKit's `AUGenericView`, added
  to the host window's content view. Moves in the editor reach automation
  through an `AUEventListener` on the main run loop; FaderFrame's own
  parameter changes notify the editor (`AUParameterListenerNotify`, with
  its own listener excluded).
* Tests (`tests/apple_units.rs`, run on the macOS CI runners) host Apple's
  AUDelay, AULowpass and DLSMusicDevice: listing, an echoed impulse,
  sample-accurate cutoff automation, a state round trip and notes into the
  synth.

### Editor windows

`faderframe-ui/src/plugin_window/` keeps the shared logic (opening,
placement, positions saved per instance, resize requests, closing, fd and
timer sources, the generic window) apart from three backends with the same
`Parents` API: `x11.rs` (XWayland top-level windows, RandR monitor
geometry, the screenshot capture), `win32.rs` (plain top-level Win32
windows with `WS_CLIPCHILDREN`; GTK's main loop dispatches their messages,
the window procedure records close requests and resizes for the next UI
tick; positions in physical pixels from the monitors' work areas) and
`cocoa.rs` (an `NSWindow` per editor, released only on destroy; closes and
content-size changes are noticed by polling on the UI tick; positions are
converted between GDK's top-left and Cocoa's bottom-left origin). The
plugin-host trait is platform-neutral: `can_embed(WindowApi)`,
`open_embedded(api, scale)`, `attach(ParentWindow)`.

## 9. Audio backends

`faderframe_audio::AudioBackend` opens an `AudioStream` with a boxed
`AudioCallback`. Device buffers are exposed through the `DeviceBuffers` trait
(per-channel slices, no per-callback arrays), so backends need no
allocation in their process callbacks. Status (xruns, rate, buffer size,
shutdown) is shared through lock-free `StreamMonitor` atomics.

* `PipeWireBackend` (Linux) — one DSP node (`pw_filter`) with a mono float
  port per channel; inputs and outputs are processed in the same cycle on
  PipeWire's realtime data thread, so recording stays sample-aligned. A
  control thread runs a PipeWire main loop and owns every PipeWire object
  (the stream handle only holds that thread and a channel). The node asks
  for the requested rate and quantum (`node.rate`, `node.latency`), is
  scheduled even when unlinked (`node.always-process`), and the callback
  re-prepares the engine when the graph's rate or quantum changes.
  Auto-connect links the ports to the highest-priority sink and source,
  found through the registry. Raw `pw_filter` calls go through
  `pipewire-sys`; the rest uses pipewire-rs.
* `JackBackend` (Linux) — JACK2 or pipewire-jack, libjack loaded at
  runtime. The server owns rate and buffer size; FaderFrame follows (the
  session rebuilds the engine on a rate change) and can request a buffer
  size live.
* `CpalBackend` — the system API through cpal: WASAPI (Windows), CoreAudio
  (macOS), ALSA (Linux). Playback drives the engine; capture arrives
  through a lock-free ring (a few milliseconds of extra input latency);
  `f32`, `i32` and `i16` devices. The streams live on a control thread, so
  the stream handle is `Send` everywhere. With the `asio` feature
  (Windows) the same backend drives ASIO drivers (`Api::Asio`), taking
  capture from the driver itself. It is opt-in because it compiles
  Steinberg's ASIO SDK, whose licence (proprietary or GPLv3) then applies
  to the binary; the *Ports* workflow checks it with MSVC.
* `DummyBackend` — timer-driven, silent; used when no audio device exists
  and in CI.

*Automatic* tries PipeWire, JACK, ALSA, then the dummy device on Linux, and
ASIO (where built in), the system API, then the dummy device elsewhere.

## 10. Session

`faderframe_session::Session` is the single mutation point for the GUI:
`dispatch(Action)` covers edits, gestures, undo/redo, transport, workspace
changes, selection, editor focus, track creation and editor settings.
`tick(dt)` (once per UI frame) polls transport position and meters (with
ballistics, peak hold and clip latching on the UI side) and reacts to stream
status (rate change, server shutdown). `render::start` runs offline bounces
(master or stems, range, rate, channels, tail, normalise, dither) on a
worker thread through the same engine path.

Delivery (`session::delivery`, on `faderframe_analysis::delivery`): a
render's `Finish` normalises the whole file to an integrated loudness
target and keeps the true peak under a ceiling — a lookahead limiter
(min-hold of the needed gain over 1.5 ms, then a box filter of the same
length, so the gain is down in time and ramps smoothly; 60 ms release;
repeated where the reconstructed signal still overshoots), the gain refined
until the limited result meets the target — and every written file is
measured (`Rendered::finished`). Dither (`faderframe_audio_files::Dither`)
is TPDF or TPDF with Lipshitz's 5-tap E-weighted error feedback (44.1 and
48 kHz). `DELIVERY_PRESETS` bundle target, ceiling, format, rate and
dither.

The album (`faderframe_project::album`, saved in `Project::album`, edited
as a whole through `Command::SetAlbum`) lists songs — a section, this
project, another project file or an audio file — with pause or crossfade
(from the previous song, equal power), trim, fades, ISRC, credits and its
own inserts, the release information (`AlbumInfo`: title, credits,
UPC/EAN) and the delivery settings (album or per-song levelling, target,
ceiling, limit or less gain, format, rate, dither, folder, album file, CD
master with or without CD-Text, digital copy permitted). Codes are checked
and normalised when they are entered (`faderframe_disc::normalize_isrc`,
`normalize_upc` with the EAN check digit).
`session::album` runs Analyse and Export on a worker: each song is
rendered at the album rate (`render::render_span`; a section without the
clips that start after it, so the next song does not ring into its tail;
other projects load with their media resolved against their folder) or
decoded (`decode_at_rate`), made stereo, trimmed, run through its inserts
(`render::process_through`: a one-track project with the chain on its
master and the song as an in-memory source; latency taken off, the tail
kept until it falls silent), faded and measured.
Export writes the songs to temporary float files while one `Measurement`
measures the whole album, then levels (album gain = target − album
loudness; per song through `normalize_loudness`), limits, dithers and
writes `NN Title.wav`. `session::album_master::Assembler` joins the
finished songs into the album stream — after their pause, rounded up so
every track mark lies on a CD frame (1/75 s), or overlapping the previous
song — and feeds the album file, the CD master and, when songs crossfade,
`Gapless`, which cuts the song files from the stream at the marks. The
cue sheet (`faderframe_disc::cue`) carries CATALOG, titles and credits,
ISRC, FLAGS and the indexes (pauses as pregaps). The CD master
(`CdMaster`: resampled to 44.1 kHz with `StreamResampler`, quantised to 16
bits with the album's dither, after track 1's 2 s pregap) is a DDP 2.00
fileset written by `faderframe_disc::ddp::DdpWriter` — `IMAGE.DAT`, the
`DDPID`, `DDPMS` and PQ descriptor packets, `CDTEXT.BIN` (packs with
CRC-16 and size information) and `CHECKSUM.MD5`/`CHECKSUM.TXT` — and read
back with `ddp::read` (sizes, PQ against the image, CD-Text CRCs, the Red
Book rules, every checksum) before the export reports success. The layout
follows the open description of the ddp-reverse-eng project (MIT); the
crate's tests compare every descriptor file byte for byte with reference
filesets recorded there (`crates/faderframe-disc/tests/reference`).
`Session::album_delivered` previews the gains from the analyses (album
loudness approximated from the songs' loudness and length).

**Album playback** plays the album as it will be delivered.
`AlbumTask::Prepare` runs export's first pass (every song rendered and
measured) and levels and limits each song as delivery does
(`album::level`), joining them through the `Assembler` (pauses,
crossfades) into one float file at the engine's rate in a per-session
temporary folder (`AlbumPreview`: the file, each song's start, and the
album and project revision it was made from — playing again re-renders
only when either changed). The engine plays the file instead of the
project (`faderframe_engine::preview`: `EngineController::set_preview`,
play/pause/locate through `PreviewShared`'s atomics; the audio thread
reads it wait-free and feeds the Tools meters as if it were the master,
whose strip then stays off the scope; the disk loader keeps its pages
resident round the playback position). The Album view's player strip
plays and pauses (from the selected song), skips, seeks on its bar (songs
ticked on it) and goes back to the project; the playing song's row shows
its progress; playing the project hands the outputs back. Tested in
`engine/tests/preview.rs` (sample-exact, pause, locate, end) and
`album_playback_does_not_allocate`.

**Vinyl.** `AlbumSettings::vinyl` (`VinylSettings`: format — 12″ 33⅓,
12″ 45, 10″ 33⅓, 7″ 45, each with a recommended and a maximum side length
—, automatic or hand-made sides, the premaster's peak, optional limiting,
per-song files) adds a vinyl premaster to the export. `session::vinyl`
plans the sides in album order (`plan_sides`: as few as fit the
recommended length — an odd count takes the record's other side too —
with the longest side as short as can be, by a small DP; by hand, a side
starts at each `Song::side_break`; pauses count within a side, a crossfade
into a side's first song is dropped). The analysis measures what a groove
makes of each song (`faderframe_analysis::vinyl`: side energy and the
lowest left/right correlation below 150 Hz, the loudest 10 ms of the
4.5–10 kHz band, the share above 8 kHz), and `vinyl::check` turns that
into findings, worst first: sides over the recommended or maximum length,
out-of-phase or wide bass, esses that will distort at the premaster's
level, hard limiting (PLR under 8 LU), and a side's brightest or loudest
song sitting at its inner groove. The premaster keeps the digital
release's balance between the songs and brings the highest peak to the
vinyl peak with gain only (`premaster_gains`); `deliver_vinyl` writes
`<name> (Vinyl)/Side A.wav …` (24-bit, continuous, pauses and crossfades
inside each side), `A1 <title>.wav …` and `Cutting Sheet.txt` (format,
side times against the limits, per song start, length, ISRC, peak and
loudness, and the findings). The Album view's Vinyl menu sets it up; rows
are numbered A1, A2, B1 … with a line where a side starts, songs with
findings carry a badge (the findings in the tooltip), the footer lists the
side times, and a row's menu starts a new side there (sides split
automatically are kept as hand-made breaks from then on, one undo step).
Dev actions `album:vinyl[=<format index>]`, `album:side-break=<n>`.

A song's inserts are ordinary `PluginSlot`s (`Command::SetSongInserts`,
`Session::song_insert`/`plugin_owner`); the plugin commands
(`SetPluginBypass`, `SetPluginParameter`, `SetPluginState`) find them when
they name a track that does not own them (the UI names the master), so
editors, generic parameter windows, presets and programs work unchanged.
The engine hosts every song's inserts (`PluginHost::retain_project` and
`sync_parameters` include them) and runs the monitored song's
(`EngineController::set_album_monitor`, `AlbumAction::Monitor`) after the
master strip, post fader as the album renders it; offline renders never
monitor. The Album view is `faderframe-view-album` (the plugin browser
adds to a song through `PluginTarget::Song`, the details form is
`UiRequest::AlbumDetails`); file and folder choosers are a
`HostRequest::ChooseFiles` the GTK host answers with `gtk::FileDialog`.

### Harmony: the key and the chord track

`faderframe_midi::theory` is the music theory everything shares: pitch
classes and their spelling, 14 scales, keys (`Key`: contains, snap to the
nearest note of the key, step by scale degrees, the diatonic triad or
seventh on a degree), 24 chord qualities, chords (`Chord`: root, quality,
another bass; named, parsed from what people type — "Am7", "F#m7b5",
"Bb/D", "CΔ7" —, voiced in close position, recognised from notes with
inversions over their bass, Roman numerals in a key) and key detection
(Krumhansl–Kessler profiles against a duration-weighted pitch-class
histogram). The project (`faderframe_project::harmony`) keeps key changes
(`Project::keys`) and the chord track (`Project::chords`) as sorted lists
edited whole (`Command::SetKeys`/`SetChords`, normalised on apply: no
repeats, no overlaps; impact Timeline); section moves, copies and deletes
carry them (`arrange`: the key in effect at a moved section's start goes
with it and the key from before resumes after it; chords across an
insertion point split round it). `session::lanes` detects chords (a beat
at most, from the pitches sounding at least a quarter of each window;
equal neighbours join) and the key from the selected MIDI clips
(`Action::DetectChords`/`DetectKey`, one undo step each). In the arranger
(`view-arranger/src/harmony.rs`) the Key lane shows spans named after
their key (click: roots and scales, double-click: type one) and the
Chords lane the chord track (drag across for a chord and type it,
double-click for one a bar, drag to move, edges to resize, right-click for
the key's diatonic chords with their numerals; colours by root round the
circle of fifths).

The piano roll follows them (`PianoRollSettings::follow_key`, on by
default; the chosen scale applies where the project has no key or when it
is off — choosing a scale in the menu turns it off). `Session::
piano_scale_at`/`piano_scales` give the scale at a time and the spans over
a range (`midi_ops::Scale::of_key` maps a key's scale); rows are shaded
outside the scale and lit on chord tones per key and chord span, notes
outside the scale at their time get a corner mark, scale snap, Fold to
Scale (the union of the clip's keys), Transpose in Scale and the scale
chords use the key where the note is, and the `ChordKind::ChordTrack`
stamp draws the chord track's chord voiced round the clicked key (a scale
triad where there is none; `Session::piano_chord_keys`, used by the draw
preview and `add_chord` alike). Under the ruler a Key row and a Chords row
(each only when the project has some) show the key spans and the chords.
Dev action `set-chord:<from>-<to>=<chord>` (quarters).

### Freezing and bouncing

Both render a track after its inserts and before its fader
(`render::track_render_project`: a copy where the track is soloed at unity
and centre into a plain master — mono tracks through a mono master, so no
pan law applies — with a tail for reverbs) on a worker thread; the tick
finishes the job. *Freeze* sets `Track::freeze` (the file, its start and
length) in one undo step: the engine plays the file instead of clips,
instrument and inserts, the plugins are unloaded, and edits of the frozen
track's clips and plugins are refused until it is unfrozen. *Bounce to New
Track* puts the file on a new audio track below and mutes the original.

### Groups and multi-track edits

`Session::edit` expands an edit of one track to the tracks that follow it
(`group_edits`): members of its active group for the linked controls, and —
for the user's own edits (`Action::Edit`), not mapped controllers or
automation — the other selected tracks when the edited track is one of
several selected. Levels, pan and sends to the same destination move
relatively (inside a gesture from where each track started, so balances
survive pulling everything to −∞ and back); mute, solo, record arm,
polarity, monitoring, colour, output and VCA assignment are set alike
(invalid targets, such as feedback loops, are skipped). The edit and its
followers are one `Batch`, one undo step; selecting a member selects its
group when the group links selection.

### Sample-level redraw

The Pencil at sample-level zoom redraws audio samples (click repair). The
redraw never touches the original: the source is copied in chunks with the
drawn samples into a new float WAV in the media folder, and only that clip
switches to it (`AddSource` + `SetClipContent`, one undo step). Other clips
on the source keep the original.

### Media: import and disk streaming

Imported files are decoded once with Symphonia (`faderframe-audio-files::
decode`: WAV, AIFF, CAF, FLAC, MP3, Ogg Vorbis, MP4/AAC, ALAC, ADPCM),
converted to the project sample rate with rubato's FFT resampler (exact
output length, start-up delay removed) and written as 32-bit float WAV
(`WavWriter`) together with a waveform peak cache (`.ffpk`, built
incrementally by `PeakBuilder`). Converting once makes every later read a
single positional `pread`, independent of the original codec, and lets the
original move or disappear.

* **Where media lives.** `<project dir>/Audio/` for saved projects. A
  never-saved project imports into a scratch folder under
  `$XDG_DATA_HOME/faderframe/unsaved/<stamp>-<pid>/`; the first *Save As*
  moves (or, across file systems, copies) that media next to the project.
  Scratch folders are deleted with their session; the shell sweeps folders
  of dead processes older than a week at start-up.
* **Paths.** In memory, `SourceSpec::File` paths are absolute (so undo
  history stays valid across save-as); `.ffproj` files store them relative
  to the project file when they are inside its directory.
* **Undo.** `Command::AddSource`/`RemoveSource` register sources; an import
  is one `Batch` (sources, new tracks, clips). Removing a source that clips
  still use is rejected; undo never deletes media files.
* **Offline media.** A file source that cannot be opened is reported once,
  its clips play silence and are drawn hatched "OFFLINE"; the project still
  opens.

Playback streams from those files (`faderframe-audio-files::stream`). A
`StreamSource` is the open `WavFile` plus a `PageTable` (in
`faderframe-realtime`) of 16384-frame pages: an array of `AtomicPtr`s the
audio thread reads wait-free. The **disk loader** thread
(`faderframe-session::media::DiskLoader`) keeps the pages from just behind
the playhead to 3 s ahead resident (plus the loop start while looping),
using the `StreamPlan` the engine controller derives from every timeline
snapshot, and evicts everything else. Evicted pages are retired through
**epoch-based reclamation**: the engine advances an `Epoch` after every
callback (including idle pumps), and a page retired at epoch *e* is freed
only once epoch *e*+1 has completed, i.e. once no callback that might still
hold it is running. All engines of a session share one epoch, so pages
survive engine replacement safely. A page that is not resident when needed
plays as silence and counts as a late read (shown in Preferences → Audio);
the audio thread never waits for the disk. Memory is bounded by the
resident window, not file length.

The clip player reads in-memory sources directly and streamed ones by page
segments; when a file's rate differs from the engine's (engine restarted
at another rate), it interpolates linearly between resident samples. The
offline renderer is its own loader: before each block it synchronously
loads the pages that block needs, so bounces never drop out. Renders open
their own `StreamSource`s (page tables are never shared with the live
engine).

### Recording, takes and comping

**Capture.** When record mode starts, the session asks the engine
(`EngineController::begin_recording`) for a pair of lock-free rings sized
for `RecordSettings::buffer_seconds` of the armed inputs — headers
(`RecordBlock`: timeline position, frames, pass) and samples — and hands
the producer ends to the audio thread inside a `Recorder`. While the
transport plays *and* records, `process_device` copies the device input
channels of every target (armed audio track with a hardware input; the
track's mono/stereo format decides one or two channels) into the rings,
clipped sample-accurately to the record window (punch range, or "from the
record start"). Every discontinuity — loop wrap, locate, re-entering the
window — starts a new *pass*. A full ring never blocks: the block is dropped
and counted, and the writer fills the hole with silence so later audio
stays in place. Ending recording retires the `Recorder` through the
garbage queue, so the rings are freed on the control thread.

**Writer.** `faderframe-session::record::RecordWriter` drains the rings on
its own thread into one float WAV per armed track (created on the first
captured block, in the media folder), builds the waveform peaks on the fly,
and records one `Segment` per pass. Peaks and segments live in a shared
`LiveTake` (`PeakBuilder` keeps a base and a coarse level and answers
`min_max` including the newest partial peak), so the arranger draws the
take's waveform while it is being recorded — every frame, at a cost bounded
by the coarse level however long the take gets. When the rings are abandoned the files
are finalised and the session turns them into clips in **one undo step**
(`Batch "Record"`): an `AddSource`, then the takes. Takes are compensated
for input + output latency as reported by the driver plus a user offset —
the file is read `latency` frames later than its capture position, so
what was played lines up with what was heard.

**Take folders.** `ClipContent::Takes(TakeFolder)` holds several `Take`s
(each a source, its alignment and the folder range it has material for) and
a *comp*: sorted `CompSegment`s, each playing one take (or nothing) until
the next one starts — the comp can never overlap or leave holes. Comp edits
(`set_comp`, `use_take`, `remove_take`) are ordinary `SetClipContent`
commands, so a swipe coalesces into one undo step; folders move, delete and
split like any clip. The engine expands the comp into regions with centred
equal-power crossfades (`crossfade`, 10 ms by default) at touching segment
boundaries and short declicks elsewhere. *Flatten Comp* replaces a folder by
plain clips with the same fades.

**Record modes** (`RecordSettings`, Transport menu / Preferences → Recording):

| Setting | Options |
|---|---|
| Record mode (over existing clips) | *Takes* — overlapped clips and folders become takes of one folder, the new take is comped in over its range, playback elsewhere is unchanged · *Replace* — overlapped clips are cut back (tape style) |
| Loop recording | *Passes as takes* · *Keep last pass* · *New track per pass* (earlier passes on new, muted tracks) |
| Metronome | Off · While recording · Always (`click.rs`: decaying sine per beat, accented downbeats, follows tempo and meter, live output only) |
| Pre-roll | 0/1/2/4 bars before the record start when starting from stop |
| Punch | `Project::punch_range` + `punch_enabled` (`Command::SetPunch`); capture only inside it |

**Arranger.** Rows have per-track heights (stored in `WorkspaceSet`:
default + per-track overrides, saved with the layout; drag a header's
bottom edge, Alt+wheel for all, View → Track Height). Open take folders
(`Session::takes_open`, editor state like the selection) add a lane per
take below the track; clicking a lane uses that take, dragging comps the
swiped range (snapped), right-click offers Use/Delete Take and Flatten.
While recording, armed tracks show a growing red region; the punch range is
drawn in the ruler.

### Automation

**Model** (`faderframe-automation`): each track has an `AutomationSet` of
lanes; a lane has a target (volume, pan, mute, a send level, a plugin
parameter, a plugin's bypass), a mode (Off, Read, Touch, Latch, Write) and a
curve of points in musical time with plain values (dB, -1..1, 0/1, the
plugin's own units) and a shape per segment (step, linear, smooth,
exponential). `Command::{Add,Remove,Set}AutomationLane` edit lanes; a lane
replacement coalesces while dragging, so a point drag is one undo step.
`Session::automatable_parameters` lists everything automatable — strip
parameters, sends, and every `automatable` parameter plus bypass of the
instrument and inserts, from the plugin's `ParameterInfo` — with ranges and
a mapping to lane height (fader law for gains, log for Hz/ms).

**Playback.** The timeline snapshot carries each track's driving lanes
converted to engine samples (`SampleLane`: binary search + interpolation,
no allocation). Channel strips and sends evaluate volume, pan, mute and send
levels every `AUTOMATION_STEP` (32) frames and ramp in between; mute
automation is kept apart from solo-implied muting (separate parameter slot).
Plugin nodes turn their lanes into sample-accurate `ParameterEvent`s — one
at every breakpoint inside the block and one per step on ramps — into a
preallocated buffer; automated bypass is a soft bypass that keeps the
plugin running and crossfades to the input delayed by the plugin's
latency, so summing points stay aligned. While stopped, the value at the
playhead applies.

**Writing.** While playing, moving a control whose lane is in Touch or
Latch mode (or any control of a Write lane, from the start of playback)
records points at the playhead. Written lanes are *suspended* in the engine
(`EngineController::set_suspended_lanes`), so the control is heard. Touch
ends with the gesture, Latch and Write when playback stops; the points are
thinned (Ramer–Douglas–Peucker) and replace the curve over the written
range in one undo step. Controls display `Session::display_value`: the
automated value at the playhead while a lane drives the parameter.

**Arranger.** The header's **A** button (or the A key, Edit → Show/Hide
Automation) shows a track's lanes below its main lane and take lanes (shown
lanes are layout state, saved with the project). Lane header: parameter
picker, mode, close. In the lane: click adds a point, drag moves it
(snapped; Alt for free), double-click deletes, Ctrl+drag draws freehand,
right-click sets segment shapes, clears the lane or changes the mode.

**Automation view** (`faderframe-view-automation`, the bottom dock's
Automation tab): a list of every automatable track with its lanes (mode,
point count, arranger visibility; "+" adds a lane; "Selected Tracks"
filters; All Read / All Off is one `SetAutomationModes` undo step) and a
full-width editor for the selected lane over the whole song (value scale
from the parameter's `AutomationParam`, bar ruler, loop, edit selection,
playhead; Ctrl+wheel zooms). Point edits use `SetAutomationLane` inside
gestures like the arranger's lanes; Write Value puts the control's value
at the playhead or over the edit selection (with jumps at its edges),
Thin drops points within half a percent of the lane's height.

### Modulators

A track's modulators (`Track::modulators`, `faderframe_project::modulation`;
at most 16) move its parameters without changing them: an LFO (five
shapes, synced to the beat or free in Hz, start phase), an envelope
follower (the track's input before its devices, or another track before
its fader; attack, release, gain), a step sequence (up to 32 steps with
glide), a random source (a new value a step, smoothed) and a macro knob.
Each routes to any number of targets with a depth (−1..1 of the target's
range): the track's fader (in fader travel, as a hand would move it), its
pan, and every device parameter that takes modulation — continuous
built-in parameters (stepped ones are left out) and CLAP parameters
flagged `IS_MODULATABLE`. VST3 and AU parameters are not modulated (they
have no non-destructive modulation). Edits are `Command::SetModulators`
(one undo step a gesture); the session's actions add, change and remove
modulators and *map* one: while mapping, the next parameter touched on the
track's devices becomes a target (a FaderFrame control's move is taken for
it, a plugin editor's stays).

The engine (`faderframe_engine::modulation`): the control side turns the
project into a `ModulationSet` (modulators and their routes in the
targets' units, per track a `ModBus` of atomics kept across sets) sent
through a latest-value mailbox and swapped into `EngineContext::modulation`
like the timeline, published on every Params impact when it changed.
A `ModNode` between a track's input and its devices evaluates the
modulators once a block into the bus — while playing from the song
position (synced rates by the beat, free rates by the second; random
values hashed from the step index), so every playback and render of a
passage sounds alike; stopped, they run on. Followers listen on extra
inputs connected from their source tracks' post-insert taps (a connection
that would close a loop is left out). Plugin nodes turn the routes to
their plugin into `PluginProcessContext::param_mods` (`ParamMod`: a share
of the range for built-ins, which apply it in their own scale — octaves
for Hz parameters — through `ParamValues`' modulation offsets; the plain
amount for CLAP, sent as `CLAP_EVENT_PARAM_MOD` and reset to 0 when a
route goes); the strip adds the fader and pan offsets. Values, state and
automation never see modulation: `ParamValues::get` includes it for
processors only, the editors' copy (the tap's, `as_set`) reads values as
set and `live` for the dot that kit knobs draw where modulation has a
value. The sandbox carries `param_mods` and the modulation flags. A track
gaining or losing modulators or a follower's source rebuilds the graph.
A modulated track is rendered ahead when its modulators depend on the
song position, its own signal or its notes only and move plugin
parameters only (`build::modulation_renders_ahead`: no follower of
another track, no fader or pan routes); the anticipator gets the
`ModulationSet` through its own mailbox and runs the track's `ModNode`
in the ahead graph. A macro moved while playing is then heard a
lookahead later, like a knob on such a track. The Modulators view
(`faderframe-view-modulators`) shows the selected track's modulators as
cards.

Per-note modulators (velocity, key, a note envelope, a note LFO that
starts with the note, a note random) have a value for every sounding note.
They move the devices that get the track's notes (the session's targets
say which: `ModTargetChoice::{takes_notes, per_note}`; mapping one onto
another device is refused). Their state lives in each plugin node
(`modulation::NoteVoices`, 32 voices, closed-form envelopes, released
notes followed through their release): parameters that take modulation
per voice (CLAP `IS_MODULATABLE_PER_KEY`, addressed by key and channel, or
`IS_MODULATABLE_PER_NOTE_ID`, addressed by the voice's note id) get one
`NoteParamMod` per voice and block in `PluginProcessContext::note_mods`
(a new note's right after its note-on, at its offset), sent as per-voice
`CLAP_EVENT_PARAM_MOD`s; the device's other parameters (and built-ins) get
the newest note's value with the block's `param_mods`. The sandbox carries
both. Checked against u-he Diva (62 per-voice parameters; its filter
frequency moved on one voice) in the opt-in CLAP test.

### Containers

A container (`faderframe.container`, an ordinary insert slot of a built-in
that does nothing itself) splits its input into parallel chains — each a
series of devices with its own level, balance, mute and solo — and mixes
them at its output; a chain without devices is the dry signal. The chains
live in `Track::containers` by the container's slot
(`faderframe_project::container`; containers in containers up to
`MAX_DEPTH` there too), so the slot moves, bypasses (dry) and is removed
and restored by undo like any other while its chains stay put.
`Track::slots()`/`slots_mut()`/`plugin()`/`plugin_mut()` reach the devices
inside, so parameters, bypass, state capture, sampler paths, automation,
modulation and plugin hosting treat them like the track's own. Edits:
`Command::SetContainer` (the chains, a graph change) and
`Command::SetChainMix` (a chain's level, balance, mute and solo: parameter
slots, `SlotRegistry::chain`, with solo resolved to a zero gain); session
actions add, rename and remove chains and put devices into and out of them
(not MIDI effects; inserted containers start with a dry chain and an empty
one). The engine expands a container in place (`build::add_container`):
the chains fan out from the previous node, each ends in a `ChainMix` node
(ramped gain and balance) and all meet at a sum, where the graph's delay
compensation aligns their latencies. On instrument tracks each chain's
note-taking devices get the track's notes through a `ChainNotes` node
that passes the chain's key range (`Chain::{key_low, key_high}`, every
note-off passes; `Action::SetChainKeys`) — layers and key splits; MIDI
tracks routed to the track reach it, nested containers take their outer
chain's notes. Devices in chains have sidechain inputs like inserts
(`SetPluginSidechain` reaches them; the build's `ChainLinks` carries the
notes and the sidechains to connect). Containers are rendered ahead
like any device (`build_ahead_chain` builds them with `add_container`;
the chain's latency is its slowest chain's), unless a device in them has
a sidechain. Editor:
`view-devices` `container::ContainerView` (a column a chain); dev action
`chain-insert:<n>=<builtin id>`.

### Pitch editing

An audio clip's notes are found by `faderframe_analysis::melody`: a pitch
track every 5 ms (McLeod's method over 40 ms windows of a copy decimated
to about 11 kHz; octave errors folded to the neighbourhood's median,
lone frames dropped) and its notes (voiced runs split where a 150 ms
average of the pitch moves 0.7 semitones and stays, or where the 10 ms
level dips 8 dB and comes back; a note's pitch is the median of its
middle 70 %). `session::pitch` runs it per source in a thread (kept for
the session) and gives the clips a `faderframe_project::pitch::PitchEdit`
in one "Detect Pitch" step: the notes in source frames at the project
rate, each with its sung pitch, a cents curve every `hop` frames (values
further than a fourth from the note are dropped), `shift`, `drift` (how
much of the wandering around the pitch is straightened) and `formant`,
plus `keep_formants`. The project holds everything playback needs; the
analysis is not saved.

Playback: an edited clip becomes a `WarpedRegion` (an identity map when
unwarped) with a `PitchCurve` (corrections every hop, gliding over 30 ms
between notes and across gaps up to 200 ms, and the sung pitch for the
grain spacing) played by `nodes::psola::PsolaVoice` — TD-PSOLA: grains
two sung periods long (Hann) cut around analysis marks one sung period
apart, laid down one target period apart, so the pitch is exact and the
formants stay; `formant` (or, with `keep_formants` off, the shift too)
squeezes the grains. Unvoiced audio goes in 5 ms grains unchanged, and
an uncorrected stretch rebuilds the source sample for sample. The marks
are found through the time map, so warping comes along. Voices are
counted per track like stretcher voices (`StretchVoices::psola`) and
primed from 100 ms back on a jump. (Signalsmith's phase vocoder missed
small transpositions by up to 0.2 semitones: no good for correction; its
formant control is wrapped all the same.)

Edits are `Action::EditPitch { clip, op }` with a `PitchOp` (move —
inside a gesture the total from where it began, recomputed from
`Session::gesture_clip` —, set, correct to the key at the note's
position (`pitch::snap`, by an amount, with straightening), split, join,
reset, keep formants, remove). The editor is `faderframe-view-pitch`
(`ViewKind::Pitch`, `Session::pitch_clip`, `Action::OpenPitchEditor`
from the arranger's clip menu: notes as blobs at the heard pitch, the
played and the sung curve, a drag moves by semitones (Alt: freely), a
double click splits, ↑↓ move, J joins, Delete resets).

### Tempo and key from clips

`faderframe_analysis::tempo::detect` finds a recording's tempo and first
beat: an onset envelope (spectral flux of 512-point frames every 64
samples at about 11 kHz, less its local mean), then the beat period
whose comb (the envelope a period apart at the best phase; half and
double periods counting a quarter) stands out most under a log-normal
prior around 120 BPM, refined to 0.01 BPM over the whole recording and
snapped to whole BPM within 0.05; confidence is how far the comb peak
stands over the envelope's mean (noise ≈ 0, a clear beat 1).
`faderframe_analysis::chroma::profile` sums the pitch classes of spectral
peaks (C2–C7, a quarter-tone tolerance, each frame normalised) and
`faderframe_midi::theory::detect_key` correlates the profile with the
Krumhansl–Kessler keys. `session::detect` (`Action::FromClip { clip,
what }`, the arranger's clip menu) analyses an audio clip's part of its
source in a thread and then sets the project's tempo, warps the clip to
the project's tempo (`length = span × clip BPM / project BPM`), or sets
the key (at the start when the project has none, else from the clip's
start); MIDI clips give their key from their notes at once. Tempos under
confidence 0.1 are refused with a notice.

### Track presets

`faderframe_project::preset::TrackPreset` captures a track's channel
settings (kind, mono/stereo format, input, monitoring, instrument and
inserts with parameters and opaque state, fader, pan, polarity, sends,
output, colour, containers with their chains and modulators — not clips
or automation) as versioned JSON (`.fftrack`); a modulator's route to a
plugin keeps the plugin's place among the track's devices (the order of
`Track::slots`) and a follower its source track's name, so both find
their plugin and track again under new ids.
Other tracks are referenced by name and resolved on use; unknown targets
are reported and dropped. The session keeps a library folder
(`$XDG_DATA_HOME/faderframe/track-presets`): save a track into it, add a
new track from a preset, or apply one onto an existing track as one undo
step.

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
  fader, meter, LED buttons, scribble strips) using a `Theme` (skins
  replace the theme, not the views). Track headers reuse the same
  controls.
* **Skins.** `Theme::all()` lists seven (`themes.rs` derives each from a
  compact `Spec` of base colours): Studio, Vintage Console, Daylight
  (light), Midnight, Frost, Neon and High Contrast. Besides colours a theme
  has a `ConsoleLook`: skirted knobs (with or without a value ring), flat
  or glossy controls, brushed-metal panel grain, screws, walnut cheeks
  framing the mixer, and the meter kind — LED ladder, continuous bar,
  plasma bar graph (a glowing column with cell lines) or an edgewise VU
  meter (0 VU = −18 dBFS, voltage-proportional scale). The
  shell switches skins live (`AppState::set_theme`: every canvas view gets
  `CanvasView::set_theme`, GTK's stylesheet is regenerated from the theme
  as `@define-color`s, and the Adwaita light/dark variant follows the
  skin, overriding a dark desktop theme for light skins); the choice is a
  preference. Overlays drawn over editor backgrounds derive from the
  theme's text and selection colours so they work on light skins too.

The arranger has **global lanes** under the ruler (shown or hidden from
their titles): markers (double-click adds, drag moves, double-click
renames, click locates), the arranger lane of sections (drag to create;
resize, rename, recolour, loop or select a section's range; drag a section
to move it with its content, Ctrl-drag to copy, Shift-drag to move only
the section; menu: duplicate, earlier, later, delete with content), time
signature changes (pick from a menu or type, per bar) and the tempo map
(points dragged up/down for the tempo and sideways for the position, typed
values, step or ramp to the next point). Marker and section edits are
commands; tempo edits replace the timeline (`SetTimeline`), merged into one
undo step per drag. Section rearrangements (`faderframe_project::arrange`)
are built from three operations on a time span — copy it with
everything in it, cut it out (later material moves up), open it (later
material moves back) — over clips (split at the edges; audio ends within a
millisecond of a bar line count as on it), automation (values kept on
both sides of a cut), tempo points (repeats dropped), time-signature
changes (whole bars), markers, sections and the loop and punch ranges
(which grow when material goes in at their start). The session computes
the result on a copy of the project and applies it as one
`Command::SetArrangement`, whose inverse is the previous arrangement. The horizontal scroll position is an `f64`, so the
arranger zooms down to single samples anywhere in a long project; there it
draws the samples themselves and the Pencil redraws them. Track colours
come from the palette or a colour chooser (click a track's colour stripe in
the arranger or the colour bar in the mixer); the mixer shows each strip's
group and VCA in a tag row once a project has any.

The UI's frame tick (`AppState::tick`, every 16 ms) redraws every view
when the session's revision changed during the tick — finished recordings,
imports and analyses arrive there, not through a user action. Warnings and
errors also pop up as a toast at the top of the main window.

Each canvas keeps a bounded `painter::PathCache` alongside its text cache.
Unchanged geometry must reuse the same `GskPath`: GSK caches rasterized
fills and strokes by path identity. Rebuilding native paths on every paint
accumulates duplicate GPU atlas entries and causes periodic main-loop
stalls when GTK collects them. Paths unused for two frames expire; entry
and geometry-size limits also bound the cache during animation or zooming.

HiDPI and fractional scaling are handled entirely by GTK; views never assume
96 DPI. On Wayland the app is a native Wayland client (the status bar shows
the GDK backend).

### Piano roll

**MIDI Tools.** `faderframe_project::midi_tools` holds pure, deterministic
note tools over a `ToolContext` (the clip's start, the meter, the scale
and key at a time, the chord track): transformations of a selection —
Strum, Chop, Join, Connect (scale runs through gaps in the top line),
Arpeggiate, Recombine (shuffle/rotate/reverse pitches, shuffle velocities
or lengths, by seed), Conform to Chords, Accent, Time Scale, Ornament —
returning the notes that replace it (kept ones with their ids, new ones
id 0), and generators filling a range — Euclidean rhythm, Seed melody (a
walk in the key), Chords (the chord track voice-led round a register),
Bassline (the chord track's roots, five patterns), Drum Pattern (General
MIDI keys, five styles). Their settings are `ToolSettings` in
`PianoRollSettings::tools`; `PianoRollSettings::tool` opens the panel.
`Session::preview_midi_tool` computes the result (transformations: the
selection or every note; generators: the bars the selection spans or the
whole clip, optionally replacing the notes there) and `apply_midi_tool`
(`Action::ApplyMidiTool`) applies exactly that as one undo step, the
result selected. The panel (`view-pianoroll/src/tools.rs`) sits right of
the grid: Transform/Generate tabs, the tools, each one's settings (drag
or wheel to change, click to cycle a choice or draw a new seed), a
summary and Apply (Enter); the grid shows the result outlined over the
notes it replaces. Dev action `midi-tool:<label>|apply|off`.

`faderframe-view-pianoroll` edits the clip in `Session::editor_clip`:
toolbar (tools, grid and snap, note length, default velocity, scale, fold,
chord, quantize and its settings, ghost notes, audition, step input,
inspector with numeric velocity entry), ruler (bars, loop, clip-end handle,
step cursor, playhead; click/drag locates), keyboard (names, scale and root
marks, live keys of the input when the track is live, click/drag to
listen), note grid and the lane below (velocity stems and line ramps, or a
controller lane per controller and channel: freehand, Shift = line, Alt =
erase, or per-note pitch / pressure / timbre expression of the selected
notes — or those sounding under the pointer — as one undo step; pitch
glides are also drawn over the notes; SysEx shows as diamonds in the ruler
with their bytes on hover). Tools: select (rubber band, double-click adds), draw (drag sets the
length; chords stamp the chosen chord, scale-aware), erase and mute
(sweep), split. Dragging notes previews and commits once on release
(`NoteOp::Move` / `Resize`, Alt copies via `DuplicateNotes`, Shift skips
snap, auto-scroll at the edges). Keys: Delete, Ctrl+A/C/X/V/D/L, Q, M,
arrows (Shift: octave or fine; Ctrl+arrows: length; in-scale transposing
with scale snap), 1–5 tools, +/−/F zoom, Esc. Ctrl+wheel zooms time,
Ctrl+Shift+wheel the rows; folding shows only scale or used keys. The
musical settings (`PianoRollSettings`: scale, chord, length, velocity,
fold, lane, ghosts, audition) live in `EditorSettings`, next to the
`quantize` and `humanize` settings the arranger shares; every
edit is a session `Action` (`NoteOperation`, `AddNotes`, `AddChord`,
`DuplicateNotes`, `SplitNotes`, `PasteNotes`, `SetControllerPoints`, …) —
one undo step each, ids allocated by the session. Pure note operations
(quantize with strength/swing/ends, humanize, legato, reverse, invert,
velocity ramps, scales and chords) are in `faderframe_project::midi_ops`.
`session::groove` applies Quantize and Humanize to whole clips in one undo
step (`QuantizeClips`, `HumanizeClips`): MIDI clips through `NoteOp`s,
audio clips by pinning each detected transient with a warp marker where
it should play (`quantize_target`: the grid point with swing, by the
strength; humanize: random offsets up to the timing in samples).

### Editing and elastic audio

The editing model follows Pro Tools and lives in the session
(`faderframe_session::editing`, `warping`, `transients`); the arranger
(`clip_edit.rs`) and the edit toolbar (`edit_bar.rs`, a canvas view hosted
full width under the header by `faderframe-ui`, wrapped into rows by
`faderframe_ui_canvas::Flow`) only turn gestures into actions.

* `EditorSettings`: edit mode (Shuffle / Slip / Spot / Grid, grid absolute
  or relative — `snap` is true exactly in Grid mode), tool (Smart, Trim,
  Time-Stretch Trim, Selector, Grabber, Separation Grabber, Scrubber,
  Pencil, Zoom), grid value (`GridDivision` down to 1/256, triplet and
  dotted; `GridDivision::menu` is the shared menu), nudge value, Tab to
  Transients, Link Timeline and Edit Selection, Insertion Follows Playback,
  Follow Playhead (scrolling while playing), transient display and
  sensitivity, warp view, counter units, the shown global lanes, and zoom
  requests (a sequence number the arranger applies once). The Scrubber
  plays snippets of audio while dragging (see §6); the Pencil draws MIDI
  clips on instrument tracks and redraws samples at sample-level zoom.
* `Selection::range` is the edit selection (a time range on the selected
  tracks). Range operations — separate, trim, clear, copy/cut/paste, repeat,
  insert silence, nudge, Tab — and clip edits are `Batch` commands, one
  undo step each; Shuffle mode closes and opens gaps.
* Multi-clip edits (`MoveClips`, `TrimClips`, `ClipGain`, `SetFade`,
  `SetClipsMuted`) act on all selected clips when the pressed clip is
  selected. Drags send absolute values inside a gesture and the session
  recomputes from `gesture_clip` — the clip as the gesture first saw it —
  so dragging back is lossless (trimmed notes and audio return).
* Fades: length, `FadeShape` (Linear, Equal Power, S-Curve, Fast, Slow)
  and a drawn bend (percent; `FadeShape::gain_bent`), the same function in
  the engine and the views.
* Transients: `faderframe_audio_files::onsets` (spectral flux of
  log-magnitude spectra, adaptive median threshold, attack refinement, a
  strength per onset) runs once per source on a helper thread, cached next
  to the media as `.fftr`; the sensitivity filters by strength.
* Warp: `AudioClip::warp` is a piecewise-linear output→source map
  (`faderframe_project::Warp`: implicit start/end anchors, markers in
  between, algorithm). Splitting, trimming and time-compression keep the
  map; a marker drag pins the neighbouring transients (`WarpDrag`), a
  range, or telescopes. The engine builds a `WarpedRegion` (anchors in
  engine frames → source frames) per warped clip: Varispeed resamples
  along the map; Polyphonic/Rhythmic run a `faderframe_stretch::Stretcher`
  voice. Voices are allocated with the clip-player node (their count is in
  the node key; a timeline edit that changes a track's needs escalates to a
  graph rebuild), bound to a clip while playback continues and primed
  after every jump: seek with the pre-roll ending at `source_at(t) + Li`,
  run and discard `Lo` output frames — then output frame `u` is exactly
  `source_at(u)` (feeding up to `source_at(u + Lo) + Li`). Outside the
  clip's source range the stretcher hears silence. The disk plan gets one
  linear piece per warp segment (widened for look-ahead and pre-roll).

### Docking

`faderframe_workspace` models layouts independently of GTK: per window a tree
of `Split`s and `TabGroup`s; named dock areas (`main`, `bottom`) persist when
empty so detached views have a home; floating windows; geometry; presets
(Recording, Editing, Mixing, MIDI, Mastering — the bottom dock holds the
mixer, Tools, album, piano roll, automation and performance views; Mastering opens
on Tools). Views added after a layout was saved register on first use. `faderframe_ui::dock::realize`
turns the active layout into `gtk::Paned` / `gtk::Notebook` /
`gtk::ApplicationWindow`s. Every view has exactly one persistent host widget
that is only re-parented, so detaching a view never copies state. Divider
positions, tab switches and window sizes are written back into the model and
saved with the project. A layout's `master_panel` flag (on in the Mastering
preset; View → Master Strip at the Side, `WorkspaceAction::ToggleMasterPanel`)
shows a master-only `MixerView` at the main window's right edge, between
the toolbars and the status bar; the dock's mixer then leaves its pinned
master out. The arranger's host lays its scrollbars over the canvas
(`ViewHost::overlaid`) so its header column and ruler reach the edges.

## 12. Metering and metrics

Meters: the RT side accumulates per-channel peak and mean square with atomic
max; the UI consumes with atomic swap — no peak is lost between frames.
Metrics: `CallbackMetrics` records every callback's duration against its
deadline in a log-scaled histogram (p50/p95/p99/max, deadline misses, xruns),
the sum of deadlines (average load over any window) and the worst callback
since last taken. `faderframe-bench` measures worst-case callback time for N
tracks/buses/inserts/sends at any rate and block size (`--measure-nodes`
includes the per-node timing cost).

### Tools (mastering meters)

The strip of the analysed track (the master unless another is chosen)
copies its post-fader output into a `ScopeRing` (`faderframe-realtime`): an
overwriting ring of `f32` bits in atomics — one writer, never waits; a
reader that falls more than its capacity behind is told how many frames it
lost. Every UI tick the session feeds what arrived to an `Analyzer`
(`faderframe-analysis`, control side, tested against EBU Tech 3341/3342
cases):

* **Loudness** after ITU-R BS.1770-4 / EBU R128: K-weighting designed for
  any rate, momentary (400 ms), short-term (3 s), gated integrated loudness
  (absolute −70 LUFS, relative −10 LU), loudness range (EBU Tech 3342: 10th
  to 95th percentile of gated short-term values), maximum momentary and
  short-term values and true peak (4× oversampling, polyphase windowed
  sinc), and the peak-to-loudness ratio (PLR: true peak over integrated
  loudness). The measurement accumulates while playing; it restarts when
  playback starts (optional) or on demand.
* **Dynamics** of the same measurement (`DynamicsMeter`): the crest factor
  (sample peak over RMS) and the DR value after the Pleasurize Music
  Foundation's TT Dynamic Range Meter (3 s blocks; per channel the second
  highest block peak over the RMS × √2 of the loudest 20 % of blocks, the
  channels averaged, shown rounded as "DR8" and coloured 1–7 / 8–13 / 14+).
* **Level**: sample peak with hold and 300 ms RMS per channel, in dBFS or on
  a K-System scale (K-12/14/20).
* **Phase**: correlation (−1 … +1, ~100 ms) and goniometer points.
* **Spectrum**: Hann-windowed 8192-point FFT of the mid signal (own radix-2
  FFT), 75 % overlap, fast attack/slow release, decaying peak hold, read out
  on a log axis.

The `Tools` view (F12; the Mastering workspace shows it in the bottom dock)
draws them with a source picker, a loudness target (−14, −16, −23, −24, −9
LUFS) and a short-term history.

### Performance meter

Per-node cost is measured inside the graph executor: one clock read per
node (a node's end is the next one's start; the time includes gathering its
inputs), summed per node and per *group* over all chunks of a device
callback in audio-thread-owned scratch arrays. At the end of the callback
`CompiledGraph::finish_cycle(budget)` publishes totals and peak shares of
the budget with plain atomic stores (the audio thread is the only writer).
Groups are tracks, so a track's peak is the real worst callback, not a sum
of its nodes' peaks. `build_graph` records what every node does for whom
(`NodeOwner`: track, plugin instance, `NodeWork`), which the controller
keeps next to the timings (`GraphProfile`).

Node timing costs about a clock read per node and callback (~15 % of the
callback in the 128-track/64-frame benchmark of trivial nodes, far less with
real plugins), so it runs on demand: `Session::performance()` keeps it on
while read, and it switches off two seconds after the meter is hidden. The
total DSP load (`Session::dsp_load`, status bar) is always measured.

`session::performance` polls four times a second: loads are shares of the
callback budget (100 % = the callback took as long as the audio it made),
averages lightly smoothed, peaks held and decaying, 60 s of history, per
track (total and own playback/mixing work) and per plugin instance (with
latency, bypass and failure). The `Performance` view (F8, dockable or
detached, also from the status bar's DSP readout) shows the total with
history and a plugins/mixing/engine/free breakdown, and a sortable table of
tracks with their plugins or of all plugin instances; double-click a plugin
for its editor.

## 13. Invariants

1. No GTK (or other GUI) dependency below `faderframe-ui`.
2. No backend-specific types (JACK, PipeWire, ALSA, Win32, CoreAudio) outside
   their backend crates; the generic engine sees `DeviceBuffers` only.
3. The realtime path performs no allocation, deallocation, locking, I/O,
   logging, sleeping or thread creation. Retired objects go back to the
   control thread; evicted disk pages are freed by the loader only after the
   reading epoch has passed.
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
    (documented mailbox, `TryCell`, task cells, worker pool, denormals),
    `faderframe-audio-jack` (JACK trait requirement),
    `faderframe-audio-pipewire` (the raw `pw_filter` API),
    `faderframe-plugin-clap` (loading plugin libraries, window handles for
    editors), `faderframe-plugin-vst3` (COM bindings, module loading),
    `faderframe-plugin-sandbox` (pipes, `poll`, shared memory, descriptors
    for helper processes, Win32 pipes/events/mappings, AppKit windows), `faderframe-stretch` (the C shim of the vendored
    stretcher) and `faderframe-ui` (GObject subclassing macros).
12. Vendored C/C++ code is listed in `THIRD_PARTY_LICENSES.md` and its
    realtime entry points are proven allocation-free by counting C++
    allocations in tests (`faderframe-stretch/tests/stretch.rs`; the Rust
    counting allocator cannot see `operator new`).

## 14. Cross-platform strategy

Linux is the primary platform; Windows and macOS build from the same code.
Platform specifics are isolated in backends and the GTK shell:

| | Linux | Windows | macOS |
|---|---|---|---|
| Audio | PipeWire, JACK, ALSA (cpal) | WASAPI (cpal) | CoreAudio (cpal) |
| MIDI | ALSA sequencer (midir) | WinMM (midir) | CoreMIDI (midir) |
| Audio (opt-in) | | ASIO (cpal, `asio` feature) | |
| CLAP / VST3 | ✓, editors embedded via XWayland | ✓, editors embedded (Win32) | ✓, editors embedded (Cocoa) |
| Audio Units | | | ✓ (AUv2 API, AUv3 through it) |
| Packages | Flatpak, install script | Inno Setup installer, zip | app bundle in a DMG |
| DSP threads | futex wake-up, the audio thread's `SCHED_FIFO` | `park`/`unpark`, MMCSS "Pro Audio" | `park`/`unpark`, the IO thread's Mach time constraint and audio workgroup |

* JACK and PipeWire are Linux-only dependencies; their crates are empty
  elsewhere. CLAP's posix-fd extension (plugin GUI event loops) exists on
  Unix only. VST3 loads modules per platform (see §8). The test-only C++
  allocation counter uses `_aligned_malloc` on Windows.
* Every crate, test and benchmark is cross-checked for
  `x86_64-pc-windows-gnu` and `aarch64-apple-darwin` (`cargo check
  --workspace --all-targets`; `FADERFRAME_CHECK_ONLY=1` skips the
  vendored C++ build where no target C++ toolchain exists).
* CI: the main workflow builds, lints and tests on Linux; the *Ports*
  workflow builds and tests on macOS (Apple Silicon `macos-15`, Intel
  `macos-15-intel`, GTK from Homebrew) and Windows (MSYS2 UCRT64 with its
  GTK 4 and Rust packages).

Still open for the ports: joining CoreAudio's IO workgroup
(`os_workgroup`, which keeps workers on performance cores on Apple
Silicon), Developer ID signing and notarisation of the macOS app, and a
signed Windows installer.

## Status and roadmap

**Implemented.** The engine: routing graph with PDC, state adoption and
sidechains; multicore scheduling with measured critical-path ranks;
sample-accurate transport, loops and scrubbing; a 24 band EQ in the spirit
of Pro-Q 4 (zero latency, natural and linear phase; dynamic and spectral
bands triggered by their region, free cuts or the sidechain; EQ Match,
Sketch, Spectrum Grab, an analyser with collisions, the instance list), the Program
EQ (PultEQFx's circuit-modelled passive tube EQ with its panel), the stock
devices with editors of their own (compressor, true-peak limiter,
gate/expander/ducker, de-esser, saturator, utility, delay, algorithmic
reverb, chorus/flanger/phaser, tuner; a virtual analogue synth, a sampler
with SFZ import and a drum sampler); offline render and export (stems,
normalise, dither); freeze and bounce in place. Audio: native PipeWire,
JACK, the system API (WASAPI, CoreAudio, ALSA), ASIO (opt-in) and a dummy
device. Render-ahead (anticipative processing) of every track nobody plays
live.
Recording with punch, pre-roll, metronome, take folders and comping.
Automation of every automatable parameter, written from controls, MIDI
controllers and plugin editors. Media import (Symphonia, rubato) and
lock-free disk streaming.

MIDI: devices with hotplug, live play with constant latency, recording,
always-on capture of what was played (Capture MIDI), MIDI learn, MIDI output and clock, clock/MTC sync, MPE and native note
expressions for CLAP and VST3 instruments, SysEx to devices and plugins,
Standard MIDI File import and export, and a full piano roll.

Plugins: CLAP and VST3 hosting with crash-safe scanning and Audio Units on
macOS, sandboxed instances (a process each) on all three platforms, VST3
program lists, optional 64-bit processing, a plugin browser, embedded
editors on all three platforms, generic
parameter windows, presets (user, VST3 factory, `.aupreset`), inserts that
move and copy between tracks, sidechain inputs.

Mixing: an analogue-console mixer, sends in banks, track groups with
linked controls, VCAs, relative edits of every selected track, track
presets, and the Tools view for mastering (EBU R128 loudness and true peak,
loudness range, PLR, crest factor and DR value, levels with K-System
scales, phase, spectrum). Delivery: loudness
normalisation and true-peak limiting on export, noise-shaped dither,
delivery presets, and the album (songs analysed and exported with album or
per-song levelling, pauses or crossfades, a song's own inserts heard on
the master, ISRC/UPC and credits, a cue sheet and a verified DDP 2.00 CD
master with CD-Text).

Editing: Pro Tools-style edit modes and tools, edit-selection ranges, clip
gain, shaped fades, transient detection, warp markers and pitch-preserving
playback, sample-level waveform redraw, global lanes for markers, song
sections (moved, copied and deleted with their content), time signatures
and the tempo map, and track colours from a colour chooser.

Shell: docking and detaching, workspaces (Recording, Editing, Mixing,
MIDI, Mastering), seven skins switched live, performance meter,
preferences, recent projects and start-up choice. Windows and macOS builds
and packages for all three platforms (see §14).

**Next**, roughly in order (waves from a survey of what Live, Bitwig,
Logic, Cubase, Studio One, Reaper, Pro Tools and Ardour shipped in
2024–2026):

1. ~~**Stock devices** (wave 1)~~ — done (see *Built-in devices*).
2. ~~**Composition** (wave 2): project key/scale and a chord track, a
   scale-aware piano roll, MIDI effects before the instrument (arpeggiator,
   chord, scale, note echo), MIDI transformations and generators,
   always-on retrospective MIDI capture~~ — done (see *Harmony*, *MIDI
   effects*, *Piano roll* and `session::capture`).
3. **Organisation**: ~~folder tracks~~ (done: `TrackKind::Folder`,
   `Track::folder`, `Project::folder_order`, `session::folders`), ~~clip
   aliases~~ (done: `Project::clip_links`, `session::aliases`), ~~project
   versions (snapshots to compare and restore)~~ (done: `session::versions`,
   `faderframe_project::compare`), ~~a command palette with a shortcut
   editor~~ (done: `faderframe-ui/src/palette.rs`), ~~an undo history
   view~~ (done: `faderframe-view-history`) — wave 3 done.
4. ~~**Modulation** (wave 4): modulators (LFO, envelope follower, steps,
   random, macros) on any parameter, CLAP's non-destructive and
   polyphonic (per-note) modulation, FX containers with parallel chains~~
   — done (see *Modulators* and *Containers*).
5. **Vocals and audio intelligence**: ~~native pitch editing~~ (done:
   TD-PSOLA rather than the Stretch engine, see *Pitch editing*),
   audio-to-MIDI (basic-pitch, Apache-2.0), ~~tempo and key
   detection~~ (done: *Tempo and key from clips*), per-clip effects rendered offline, ARA 2 hosting, a speech
   and lyrics transcription track (Whisper, MIT). Stem separation waits
   for permissively licensed model weights.
6. **Performance and control**: a clip launcher with scenes recorded into
   the arrangement, control surfaces (Mackie Control/HUI, OSC).
7. **Ports**: signed and notarised packages, a Flathub submission
   (vendored crates), sandboxed plugins' audio threads in the device's
   workgroup (macOS: needs the workgroup's Mach port in the helper).
8. **MIDI**: MTC output, varispeed chase without a shared word clock.
9. **Performance**: render-ahead for buses whose inputs are all rendered
   ahead, job affinity for cache locality, an optional wgpu painter for
   dense views.
10. **Mastering**: multiple CD-Text languages, a DDP player/import;
    surround beds and panning before any object-based format.


### Modelled microphone preamplifiers

`faderframe-circuit` contains the reusable GainStageFx circuit engine (MNA
netlists, nonlinear component models, transient and AC solvers, and FIR
oversampling). It has no external dependencies or GUI/plugin framework.
Its upstream revision and local extraction changes are recorded in `UPSTREAM.md`.

`Track::preamp` is a dedicated optional built-in plugin slot, with the same
parameter, state, automation, undo, and host lifecycle as other plugins.
It is omitted from ordinary plugin choices and drawn in the mixer's input
section. Audio, bus, aux, and master inputs feed it before inserts; instrument
tracks feed it after the last instrument, including projects with both legacy
instrument slots and visible instrument inserts. This keeps later generators
from replacing audio that has already passed through the preamp.
Pre-FX sends include the preamp output. MIDI-only tracks and VCAs have no audio
input stage. The live and anticipative
graphs use the same ordering. A frozen track's audio already includes its
preamp, so the saved slot is unloaded until unfreezing.
Frozen players report the latency baked into their audio, preserving alignment
with other tracks and sends; bounced clips skip that leading delay instead.

The six models retain upstream impedances, resting controls, and measured
calibration curves. British 73 includes its separate line driver. Each channel
has independent circuit state and uses fixed 2x oversampling with reported
latency. Live preamps run on a dedicated worker through GainStageFx's generic
reservoir in `faderframe-realtime`, adding exactly 128 host samples, reported
alongside the FIR latency for graph compensation. Monitoring stays available:
the live signal necessarily incurs that delay (2.67 ms at 48 kHz). Offline
and already anticipated chains use inline DSP plus the same delay, keeping
renders deterministic without adding nested workers. Parameter values travel
with each input sample in fixed storage; resets discard the previous epoch.
Worker underruns join the existing xrun counter. Live graphs honor processors'
preferred 128-frame quantum so
large device callbacks enqueue tracks in smaller chunks. Those chunks share
one bounded device deadline, retaining time for downstream work. Gain moves
the circuit control with upstream automatic level compensation, except on
the Tube 610: its physical Level pot uses fixed calibration at 50%, allowing
it to attenuate without boosting capacitive leakage near zero. Master is a
smoothed -60 to +12 dB output trim. Calibration is
applied before decimation, synchronously with circuit gain changes. Constructor
work runs off the audio thread; processing, automation, and reset use reserved
storage. Gain switches retain the circuit's discrete positions. Extra hardware
switches stay at their netlist resting values.

As in GainStageFx, exact duplicated stereo input needs only one circuit solve.
On the first differing block, the dormant channel receives the active channel's
solver and FIR history without allocation. Both channels then remain active,
including during silence, until reset. Tests compare this path bit for bit
against always processing two independent channels through gain changes,
stereo transitions, tails, and resets.
