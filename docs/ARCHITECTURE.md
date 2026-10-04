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
       ├─ faderframe-view-arranger / -mixer / -pianoroll / -performance / -tools   (GTK-free views)
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
  macOS its Mach time-constraint policy, on Windows MMCSS's "Pro Audio"
  task (elsewhere they park/unpark) — and flush denormals, as does
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
* **MPE (per-note expression).** Expression is stored with the notes, not
  the channels: `MidiClip::expressions` holds a `NoteExpression` per note
  (pitch in semitones, pressure and timbre 0–1; points relative to the
  note's start, linear between them), so notes can be moved, transposed,
  copied, split and deleted with their expression (the session's note
  operations carry it; clip splits move it with the notes). A track with
  `Track::mpe` (`MpeConfig`: member channels, bend range ±48) plays MPE:
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
* **SysEx.** Variable-length data stays out of the realtime path.
  `MidiClip::sysex` holds messages at musical times; SysEx arriving on a
  recording track's input is placed where it was heard and becomes part of
  the take. For tracks with an external MIDI output the session schedules
  the messages 100 ms ahead to the output sender with exact due times from
  the engine's time-to-position mapping, following loop wraps; a locate or
  stop (the engine is not where it was predicted) cancels what was
  scheduled (a generation counter in `MidiOutputs`). `.syx` files can be
  imported into clips and sent to any output from the preferences.
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
pair speaking IPC with shared-memory audio. `PluginHost` (engine) owns one
instance per slot. Built-ins: synth, echo, compressor (with a sidechain
input), gain, latency probe (used to test PDC end to end). Failed plugins
are bypassed and flagged; missing formats pass audio through with a
warning.

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
* **Generic editor.** Every plugin (built-ins, plugins without a GUI, no X
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
* **Parameters** are normalised in VST3. FaderFrame shows continuous ones as
  0–1 and stepped ones as integers 0…steps, converting at the boundary; the
  plugin formats values (`getParamStringByValue`). Hidden parameters and the
  non-automatable proxies some frameworks register as MIDI controller
  targets (JUCE, u-he: 16 × 130) are not listed. Unit names prefix
  parameter names ("Unit/Name") like CLAP modules.
* **Processing.** Activation activates the main audio buses and the first
  event input, confirms the plugin's own arrangements, sets up 32-bit
  processing and hands an `Active` (processor, all bus buffers, the host's
  `IParameterChanges`/`IEventList` objects and the process context, all
  preallocated) to a `TryCell` like CLAP. Per block: UI and editor changes
  (offset 0, from a wait-free queue; they wait when the 512 queues of a
  block are taken), then automation (sample offsets), then MIDI: notes and
  poly pressure become events, CC/pitch bend/channel pressure become
  parameter changes through the plugin's `IMidiMapping` (VST3 has no CC
  events; the table is built at activation). Values the processor reports
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

### Track presets

`faderframe_project::preset::TrackPreset` captures a track's channel
settings (kind, mono/stereo format, input, monitoring, instrument and
inserts with parameters and opaque state, fader, pan, polarity, sends,
output, colour — not clips or automation) as versioned JSON (`.fftrack`).
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

HiDPI and fractional scaling are handled entirely by GTK; views never assume
96 DPI. On Wayland the app is a native Wayland client (the status bar shows
the GDK backend).

### Piano roll

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
fold, quantize, lane, ghosts, audition) live in `EditorSettings`; every
edit is a session `Action` (`NoteOperation`, `AddNotes`, `AddChord`,
`DuplicateNotes`, `SplitNotes`, `PasteNotes`, `SetControllerPoints`, …) —
one undo step each, ids allocated by the session. Pure note operations
(quantize with strength/swing/ends, humanize, legato, reverse, invert,
velocity ramps, scales and chords) are in `faderframe_project::midi_ops`.

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
mixer, Tools, piano roll, automation and performance views; Mastering opens
on Tools). Views added after a layout was saved register on first use. `faderframe_ui::dock::realize`
turns the active layout into `gtk::Paned` / `gtk::Notebook` /
`gtk::ApplicationWindow`s. Every view has exactly one persistent host widget
that is only re-parented, so detaching a view never copies state. Divider
positions, tab switches and window sizes are written back into the model and
saved with the project.

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
  sinc). The measurement accumulates while playing; it restarts when
  playback starts (optional) or on demand.
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
    `faderframe-stretch` (the C shim of the vendored stretcher) and
    `faderframe-ui` (GObject subclassing macros).
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
| DSP threads | futex wake-up, the audio thread's `SCHED_FIFO` | `park`/`unpark`, MMCSS "Pro Audio" | `park`/`unpark`, the IO thread's Mach time constraint |

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
sample-accurate transport, loops and scrubbing; built-in synth, echo,
compressor, gain and latency probe; offline render and export (stems,
normalise, dither); freeze and bounce in place. Audio: native PipeWire,
JACK, the system API (WASAPI, CoreAudio, ALSA), ASIO (opt-in) and a dummy
device.
Recording with punch, pre-roll, metronome, take folders and comping.
Automation of every automatable parameter, written from controls, MIDI
controllers and plugin editors. Media import (Symphonia, rubato) and
lock-free disk streaming.

MIDI: devices with hotplug, live play with constant latency, recording,
MIDI learn, MIDI output and clock, clock/MTC sync, MPE, SysEx, Standard
MIDI File import and export, and a full piano roll.

Plugins: CLAP and VST3 hosting with crash-safe scanning and Audio Units on
macOS, a plugin browser, embedded editors on all three platforms, generic
parameter windows, presets (user, VST3 factory, `.aupreset`), inserts that
move and copy between tracks, sidechain inputs.

Mixing: an analogue-console mixer, sends in banks, track groups with
linked controls, VCAs, relative edits of every selected track, track
presets, and the Tools view for mastering (EBU R128 loudness and true peak,
levels with K-System scales, phase, spectrum).

Editing: Pro Tools-style edit modes and tools, edit-selection ranges, clip
gain, shaped fades, transient detection, warp markers and pitch-preserving
playback, sample-level waveform redraw, global lanes for markers, song
sections (moved, copied and deleted with their content), time signatures
and the tempo map, and track colours from a colour chooser.

Shell: docking and detaching, workspaces (Recording, Editing, Mixing,
MIDI, Mastering), seven skins switched live, performance meter,
preferences, recent projects and start-up choice. Windows and macOS builds
and packages for all three platforms (see §14).

**Next**, roughly in order:

1. **Mastering**: offline loudness analysis per song and loudness
   normalisation on export, dithering and true-peak limiting for delivery
   formats, an album/sequence view.
2. **Ports**: the CoreAudio IO workgroup for DSP workers, signed and
   notarised packages, a Flathub submission (vendored crates).
3. **Plugins**: note expressions (CLAP, VST3), VST3 program lists and
   64-bit processing, sandboxed plugins (out-of-process with shared-memory
   audio), SysEx to plugins.
4. **MIDI**: MTC output, varispeed chase without a shared word clock.
5. **Performance**: anticipative processing of tracks that are not
   monitored live, job affinity for cache locality, an optional wgpu
   painter for dense views.
