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
       ├─ faderframe-ui-gpu       optional GPU painter for dense views (vello on wgpu, parley text)
       ├─ faderframe-view-ddp     DDP player: a CD master checked, played and imported
       ├─ faderframe-view-surround surround panner: a track in its bed, from above
       ├─ faderframe-view-adr     ADR cue list: lines rehearsed and recorded with beeps and streamers
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
            ├─ faderframe-qc           delivery QC: spec checks, stems' null tests, the QC report (JSON, PDF), SHA-256
            ├─ faderframe-interchange  DAWproject files (written and read), MusicXML scores (read, .mxl too), zip
            ├─ faderframe-conform      new picture cuts: CMX3600 EDLs, OpenTimelineIO, cut-to-cut changes, shot matching
            ├─ faderframe-aaf          AAF interchange: compound files, the object model, compositions written and read
            ├─ faderframe-adm          object-based masters: ADM (BS.2076) axml/chna, Dolby Atmos master profile
            ├─ faderframe-iamf         IAMF masters: OBUs, MP4 (iamf/iacb), LPCM/FLAC/Opus substreams
            ├─ faderframe-collab       live collaboration's network: invite codes, Noise-encrypted TCP, ordered entries, media by content
            ├─ faderframe-engine       project→graph compiler, RT processor, controller, offline render
            │    ├─ faderframe-binaural     headphone listening: beds through measured HRIRs (SADIE II KU100), rooms
            │    ├─ faderframe-audio-graph   generic DSP graph: ports, edges, PDC, compile, executor
            │    ├─ faderframe-plugin-host   plugin abstraction + built-in plugins
            │    │    └─ faderframe-ara       ARA 2 hosting: the C API, host interfaces, document model
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
feature, not a crate of its own. The GPU painter (`faderframe-ui-gpu`,
feature `gpu-painter`) is optional; see *Canvas views*.

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
  The delay lines compensating an adopted node's inputs go along where
  the same source (by key and port) arrives through one just as long, so
  a rebuild while playing leaves no gap on a delayed path
  (`compensation_delays_carry_over_into_a_rebuilt_graph`).

The realtime path never allocates, frees, locks, logs or does I/O. This is
enforced by `crates/faderframe-engine/tests/realtime_alloc.rs`, which runs the
demo project (synth voices, echo, loop wrap, stop/start, graph swap) under a
counting global allocator and asserts zero allocations and deallocations.

### Keeping the deadline

Allocation-free is not enough: a callback is late when its thread waits
for memory, for the CPU or for another thread. What FaderFrame does about
each:

* **Memory the audio thread touches is mapped before it does.** Buffers
  are made on control threads but first written on the audio thread — a
  new graph's buffers and delay lines, a delay line as its write head
  moves on, a recording's capture rings; every first touch of a page was a
  fault there (553 in 400 callbacks of the demo, 300 more after a graph
  swap; with transparent huge pages one fault zeroes 2 MB, ~100 µs). The
  programs install `faderframe_realtime::memory::Prefaulting` as global
  allocator: every allocation is touched page by page where it is made.
  RT threads map their stacks at start (`prefault_stack`: DSP workers,
  the PipeWire data loop's and cpal's first callback) — at most the stack
  the thread has left less 64 KiB (`stack_left`): callbacks run on threads
  others made (CoreAudio's, an ASIO driver's), and mapping past a stack's
  end is a crash. Code and constants
  fault in too, the first time a path runs (found as a fault in one run of
  five: the recorder's ring write, run first when recording starts, on a
  code page nothing had mapped yet): `prefault_code` maps the program's,
  the C/C++ runtimes', the audio libraries' (PipeWire and its SPA plugins,
  JACK, ALSA) and the plugin binaries' code and data — at start-up, after
  each graph build (plugins just loaded; only new mappings) and in the
  sandbox helper — locked with the rest. Only the kernel touches them
  (`MADV_POPULATE_READ`/`WRITE`; before Linux 5.14 a `MADV_WILLNEED`
  hint): the ranges come from a reading of `/proc/self/maps` that other
  threads may have made stale, and the touch-every-page fallback once
  taken on any `EINVAL` read ranges gone `PROT_NONE` (`SIGSEGV`;
  `a_range_gone_inaccessible_is_not_touched`). `tests/realtime_faults.rs`
  counts the audio thread's page faults (`RUSAGE_THREAD`): playing, after a
  graph swap, recording — 0 (40 runs of 40).
* **Nothing it needs is swapped out.** With an unlimited memory-lock limit
  (the `realtime`/`audio` group), the process locks every page it touches
  (`lock_process_memory`: `mlockall(MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT)`
  — untouched stacks and mappings stay unmapped): decoded samples played
  once an hour, streamed pages, code. With a finite limit nothing is locked
  (an allocation past the limit would fail) and the log says why.
* **The audio path runs at realtime priority, in order**
  (`faderframe_realtime::priority`): the audio callback on top (PipeWire's
  and JACK's own; cpal's thread is made `SCHED_FIFO` 80 /
  MMCSS "Pro Audio" critical on its first callback — cpal itself does not);
  the DSP workers and sandboxed plugins' audio threads take its scheduling;
  helpers follow it a step below, re-applied when it changes (the audio
  thread publishes it every 4096 callbacks): circuit reservoirs and the
  shallow render-ahead tier P − 1, the deep tier and its workers and MIDI
  in/out P − 5, disk streaming and the recording writer P − 10 (their I/O
  takes the realtime I/O class with it). PipeWire's data loop runs
  `SCHED_FIFO | SCHED_RESET_ON_FORK`: the flag is masked when testing for
  realtime (it once made the pool and helpers take the audio thread for a
  normal one), and helpers set it too (threads they start are normal);
  pools re-read their caller's scheduling when the audio thread's
  changes. Measured with PipeWire (data loop 83): DSP workers 83, shallow
  tier 82, deep tier and its workers 78, MIDI out 78, disk 73, UI normal;
  memory locked; no page faults on any audio-path thread. The UI and other work stay normal:
  at realtime they would compete with the audio. Threads that cannot get
  their priority are counted and the session says so once
  (`priority_failures`); the Audio preferences' performance line says how
  many DSP workers run without it. A realtime caller's cycles are for the
  workers that run realtime: the others sit out until they do (`pool::
  helpers_for`) — 0.22.0 ran such a caller serially as soon as one worker
  had been refused. Never mixed: everyone in a cycle spins while it waits
  for a job someone holds, and a normal worker preempted while it holds
  one can wait behind the spinning realtime threads as long as the
  scheduler leaves it there. End to end (FaderFrame under PipeWire,
  realtime only through a stand-in rtkit, a machine busy with builds), 7
  realtime + 8 normal workers all used had the data loop and the realtime
  workers spin into the realtime time limit: 8 `SIGXCPU` demotions in the
  first second (before playing), 16 in 25 s, nothing realtime left;
  realtime workers only: none. (A synthetic
  cycle — `cycles_with_workers_refused_realtime`, release, load average
  70–110: 48 items of ~25 µs every 2.67 ms, serial p50 0.4–1.2 ms, 7
  workers 0.07–0.2 ms; 64 items of ~250 µs with 16 busy threads in the
  process, serial late in every cycle, 7 normal workers in 13–14 %, 3
  realtime + 4 normal in none, 3 realtime alone p50 1.9–2.4 ms — did not
  show the inversion.) Where the process may not take realtime itself (no
  `RLIMIT_RTPRIO`), a thread asks RealtimeKit (`faderframe_realtime::rtkit`:
  `rtkit-daemon` on the system bus, else the desktop portal's Realtime
  interface — the way out of a Flatpak), through a small D-Bus client of
  its own (no library): `SCHED_RR` at the priority wanted, capped by
  rtkit's maximum (20 by default). The thread does not ask itself — it
  posts its thread id and priority (`rtkit::request`: atomics and a futex
  wake, no allocation; the audio thread may) and carries on; the process's
  broker thread (`ff-rtkit`, `rtkit::start`, started at start-up, by every
  `WorkerPool` and in the sandbox helper) asks *for* it
  (`MakeThreadRealtime` takes any thread of the caller's process, as
  PipeWire's own client asks from a thread of its own), over one
  connection it keeps. Asking inline cost the round trips where they hurt:
  a sandboxed plugin's audio thread on its first block (late, silent),
  cpal's audio thread in its first callback, workers on their first job.
  The broker keeps within rtkit's limits, which count every program of the
  user (PipeWire's threads too): 25 + 1 requests in 20 s, 25 realtime
  threads, 15 processes, answered `AccessDenied` ("Device or resource
  busy") beyond — a refusal for now, which 0.22.0 took for an absent
  service, failing every later request of the process. It sends at most 16
  requests in any 20 s (counted from their answers), highest priority
  first (requests posted together are gathered for 20 ms), keeps at most
  16 threads realtime through rtkit (a higher priority takes the place of
  the lowest grant, which goes back to normal and waits for room), pauses
  21 s after a refusal (doubling while they go on, up to 336 s) and asks
  again — never gives up —, forgets threads that went and notes grants
  lost (the watchdog, the `SIGXCPU` demotion: not asked again until the
  thread asks); a thread still waiting after 30 s counts as a priority
  failure. Fewer ask: workers past the physical cores (SMT siblings) and
  the disk and recording writers (seconds of slack) do not — they run
  realtime only where the process may set it itself (the SMT siblings sit
  out a realtime caller's cycles under rtkit). Measured against a
  stand-in rtkit with its default limits, time shrunk 33× (rtkit
  `a_sessions_requests_against_rtkits_limits`; a 16-thread machine,
  PipeWire, 4 realtime threads held elsewhere, Restart Audio after 10 s):
  0.22.0 asked 22 times, the 22nd refused (rtkit's thread limit), and so
  nothing more — reservoirs, the deep tier and its workers normal, and
  after the restart no thread realtime at all, the pools serial; the
  broker asked 16 + 12 times, none refused: the 7 DSP workers within the
  cores, the shallow tier and its 4 workers, the reservoirs, MIDI —
  everything on the callback's path — realtime at once, the restarted
  ones one window (20 s) later, the deep tier waiting for room. rtkit
  wants `RLIMIT_RTTIME`: set to its maximum (hard) and half of it (soft);
  a thread realtime that long without sleeping gets `SIGXCPU`, whose
  handler (`faderframe_realtime::rttime`) returns it to normal scheduling
  and counts it, before the hard limit's `SIGKILL` — which kills the whole
  process, with no log and no core dump. Two things made that kill the
  usual outcome in 0.22.0 (Debian and other systems where rtkit grants
  realtime): the handler's `sched_setscheduler(SCHED_OTHER)` failed with
  `EPERM` on every thread rtkit, PipeWire or FaderFrame make realtime —
  they carry `SCHED_RESET_ON_FORK`, which an unprivileged thread may not
  clear — so the thread kept running and was killed at the hard limit; and
  PipeWire's client sets the limit again with every context it makes
  (`module-rt` under rtkit: soft = hard = 200 ms, so no `SIGXCPU` comes
  before the `SIGKILL`). Now the handler demotes with the flag kept, and
  `priority::note_rttime_limit` keeps the margin whoever set the limit
  (soft half the hard one, the handler installed): at start-up of every
  FaderFrame process (sandbox helpers inherit the limit), after each audio
  stream opens and once a second from the session's tick, which also logs
  each thread demoted and says so once (`rttime_demotions`; Restart Audio
  gives the audio thread its priority back). End to end
  (`rt_e2e`-style run: a private PipeWire daemon, the limit as rtkit's
  users set it given with `prlimit --rttime=200000:200000`, the audio
  thread overloaded — render-ahead off, one thread, 32 frames, 70 devices):
  0.22.0 was `SIGKILL`ed without a word ten seconds in; now the data loop
  is demoted, logged, and the session plays on. Helpers going back to normal
  (`follow_audio` with no realtime audio thread, pool workers following a
  normal caller) keep the flag for the same reason. Thread scheduling is
  read from the kernel (`sched_getscheduler`), not glibc's
  `pthread_getschedparam`, which answers from a cache that rtkit's changes
  from outside and the handler's never reach. `tests/rttime_guard.rs`,
  against the kernel in child processes: a reset-on-fork thread running
  past the limit as PipeWire sets it, and as FaderFrame sets it, is
  demoted and the process lives (both were killed before); helpers and
  pool workers return to normal. So that no thread gets there, helpers
  breathe under any such limit: `priority::breathe` (in `follow_audio`,
  which every helper loop calls, and per page in the disk loader and per
  block in the recording writer, whose catch-up never sleeps) blocks 50 µs
  once a quarter of the soft limit has passed since the thread last did —
  the kernel counts realtime time only since a thread last slept — and
  pool workers take the same breath while idle when cycles come too close
  for them to sleep (`tests/rttime.rs`: against the kernel, a spinning
  `SCHED_FIFO` thread gets `SIGXCPU`, a breathing one not). Work that is
  not ours can still run that long on a realtime thread (a plugin's first
  `process` loading a model): it is demoted now, not killed. A thread
  already where rtkit would put it is not asked for. rtkit caps priorities at its
  `MaxRealtimePriority` (20 unless its daemon is started with another
  `--max-realtime-priority`): the order below the audio thread is kept
  (19/15/10 under 20); higher needs the `realtime` group.
* **CPUs stay awake** while the stream runs (`CpuWakeLatency`: 0 µs held in
  `/dev/cpu_dma_latency` where the system allows; workers sleep between
  callbacks, and a deep C-state takes hundreds of µs to leave).
* **No unbounded waits on other threads.** A late circuit reservoir worker
  is waited for only when its frames cannot exist yet (input given in this
  very device callback); frames it owes from earlier callbacks are
  concealed at once (`frames_owed_from_an_earlier_callback_are_concealed_not_waited_for`).
  A sandboxed plugin's helper is waited for until the callback's deadline
  (else a block's duration), then its block is silent and counted
  (`take_underruns`); it is dead only after `BLOCK_TIMEOUT`. Offline
  renders wait for every block (dead after `OFFLINE_BLOCK_TIMEOUT`, 30 s):
  nothing waits on a device, and a plugin told it renders offline may
  take longer than real time. A panicking
  node counts as done (the pool and the executor catch it), so no thread
  waits for it for ever; the caller of a parallel cycle always keeps a seat
  (workers still spinning from the last cycle cannot leave it only
  waiting).
* **Bursts are spread.** Voice to MIDI listeners are analysed side by side
  on the DSP workers (`voice::Listening`), not one after another; warped
  clips prime their stretchers within a quarter of the callback (more
  wait a block, in the live graph only); a loop is at least
  `MIN_LOOP_SECONDS` long (every wrap is another pass through the graph);
  SFZ instruments look up their zones by key (`SampleSet::index`). The EQ:
  its convolver (linear and natural phase) multiplies a partition's
  products a share per block (`fir::Convolver::prepare`), a new kernel set
  prepared over one block before the crossfade; the spectral stage spreads
  a frame's work (transforms, a band's curves in eight parts, a band's
  dynamics, the gains, the inverse transforms) evenly over the hop after
  it, one hop more latency (`spectral::latency` = frame + hop); band
  designs share a budget of `DESIGN_BUDGET` sections per control step (the
  bands take turns: a preset recall or Gain Scale moving 24 steep bands
  takes a few steps, not one), a band without gain (cuts, notch, band
  pass, all pass) is not redesigned when only its gain moves, and auto
  gain's loudness estimate advances within what is left (`Loudness`: a few
  bands' designs, then eight probe points per step). Slowest of 4096
  64-frame blocks at 48 kHz (`eq_block_peaks`, release; 1333 µs available;
  ranges over runs), before → after: 24 brickwall cuts with Gain Scale
  moving 804 → 107–110 µs, with their frequencies moving 809 → 155–193 µs,
  24 steep shelves with Gain Scale moving 629 → 158–163 µs, 4 spectral
  bands at the highest resolution 339 → 85–113 µs, 8 bells linear phase at
  the highest resolution 195–347 → 80–167 µs; auto gain keeps up with
  bands that never stop moving (`LOUDNESS_SHARE`: an estimate under way
  finishes first, `auto_gain_follows_bands_that_keep_moving`). MIDI regions carry the controllers' values every
  `CHASE_EVERY` (512) events (`snapshot::ChasePoints`, made with the
  snapshot): a jump into a long take chases from the point before, not from
  the region's start.
* **A panic resets without a burst.** `reset_processors` fades the output
  out (the graph-swap dip), resets the graph's nodes in processing order a
  quarter of a callback at a time while silent (`CompiledGraph::reset_nodes`;
  upstream nodes first, so nothing reset is fed old tails), then fades in.
  A node's reset includes the delay lines compensating its inputs (audio
  and MIDI waiting there came out after the panic before — a note-on still
  on its way sounded after the notes were released). Large state resets
  in constant time by being forgotten rather than cleared: delay lines
  (`dsp::DelayLine`: the Delay's 5 s lines, reverb, modulation; the graph's
  `AudioDelay`; the EQ's dry delay) count what was written since and read
  older samples as silence, the EQ's convolver skips spectra from before
  the reset, its spectral bands start their envelopes again when they next
  run. Every built-in's reset after playing, at 192 kHz
  (`tests/reset_cost.rs`, ignored): at most 4 µs (the EQ at its heaviest:
  137–245 µs before), the Guitar Station's stages reset on their workers
  (live; 0.6 ms inline, in renders only). Render-ahead tiers reset in place
  (`AheadLink::reset`: each anticipator on its own thread, at the frame of
  its sequence it has reached, which it publishes): the transport goes on
  — no new sequence, no jump — and the output stays silent until the
  audio thread plays past those frames (what was rendered before still
  holds the tails; at most the lookahead) (`tests/panic.rs`: the echoes
  gone, a later click on its sample, the position moving one callback per
  callback).
* **The system backend reports one block size.** cpal callbacks vary in
  size on some systems (and larger ones run in chunks); the nominal size
  is the largest callback, larger at once, smaller only after 3 s of
  smaller callbacks (`Nominal`) — no longer a re-prepare and a graph
  rebuild (preamps, buses rendered ahead) per size, nor a step in the
  output latency.
* **No I/O in plugins' log calls**: CLAP and LV2 messages logged while
  processing are copied into a slot and written by the main thread's poll.
* **Pages for a locate are loaded first** (`EngineShared::recent_locate`),
  breadth-first across sources so all tracks come back together, and one
  failing file no longer stops the others. Render-ahead threads hold epoch
  reader slots (`Epoch::reader`) so a streamed page is never freed while
  they read it.
* PipeWire's xrun count counts xruns (`spa_io_clock.xrun` is a duration in
  samples; `XRUN_RECOVER` marks this node late).

* **Plugins from other makers are measured, not trusted.** Their code is
  theirs; the plugin check (`faderframe-plugin-check`) plays one for a few
  seconds in a `faderframe --check-plugin` helper process (paced like a
  device; noise in, chords for instruments; then five resets) and counts
  per call on its audio thread: allocations — the program defines
  `malloc` and its kin itself (`faderframe-app`'s `alloc_probe`, Linux
  with glibc: plain forwarding to `__libc_malloc` …, a thread-local check
  per call; C and C++ plugins allocate there), and the host's own Rust
  allocations, counted by the prefaulting allocator, are told apart (a
  FaderFrame bug, reported as such) —, waits (voluntary context switches),
  page faults and CPU time per block (`faderframe_realtime::usage`). The
  report's findings and verdict (realtime-safe / mostly / not / not
  checked) and advice show in the plugin browser (a badge per plugin, the
  detail panel, "With findings"; "Check" runs those not checked in their
  version, one after another, cached in `plugin-checks.json`). First
  results here: PultEQFx and the built-ins clean; Satin allocates in every
  block (16 per block), Pro-Q 4 waits in every block, Diva allocates now
  and then and when reset — with the host's allocations at zero in each.

Limits that remain: hosted plugins' (CLAP, VST3, LV2, AU) resets are
theirs — a panic resets them one per share of a callback, but one that
clears a lot at once still does it in one go (the check measures it);
through rtkit, the priority cap is the system's setting; allocations,
waits and page faults are counted on Linux only.

## 3. Project model

`faderframe_project::Project` is the persistent model and contains only
persistent data:

* `tracks: Vec<Track>` in display order — kinds `Audio`, `Instrument`,
  `Midi`, `Bus`, `Aux`, `Vca`, `Master` (exactly one) share one structure:
  fader, pan, mute/solo/arm/monitor, inserts (each with an optional
  sidechain source), instrument slot, sends (pre-FX / pre-fader /
  post-fader), input and output routing, automation, the VCA and group it
  follows, a freeze (rendered audio) and an explicit `ChannelLayout` (mono,
  stereo, a surround bed, discrete N), and where it sits in a bed it feeds
  (`Track::surround`).
* `groups` — `TrackGroup`s (name, colour, active, `GroupLink`: volume,
  mute, solo, record arm, selection); members name theirs in `Track::group`.
* `clips: BTreeMap<ClipId, Clip>` — audio clips (source, offset, length in
  project-rate frames, gain, fades, stretch settings, reverse) and MIDI clips
  (length + notes relative to the clip). Clip starts are `MusicalTime`.
* `sources` — files or deterministic generators (the demo session).
* `timeline` (tempo map with constant and linearly ramped segments, meter
  map), markers, the Play Start Marker (`play_start`, §6), arrangement
  `sections` (named, coloured ranges: Intro,
  Verse, …), loop and punch ranges, MIDI mappings, `IdAllocator`.
* `scratch_pads` — other arrangements of the same tracks
  (`faderframe_project::scratch`, see "Scratch pads" in §10). The
  arrangement fields above (`clips` but the launcher's, each track's
  `clips` and `automation`, `timeline`, markers, sections, keys, chords,
  lyrics, loop, Play Start Marker) always hold the arrangement *shown*;
  the others are put aside by value as `Stash`es.

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

The history is a tree (`faderframe_project::history`). Every step is a
node holding the commands that join it to its parent; the root
(`History::ROOT`) is the state the history starts from. A node's commands
point one way at a time: while it is *done* (on the way from the root to
the current node) they take it back, otherwise they do it again, and
replaying them turns them into the opposite. An edit made after undoing
becomes a new child of the current node — a branch beside the steps
undone, which stay. Undo goes to the parent; redo goes to the child last
made or visited (`next`), so undo/redo follow the current branch as they
did when the history was a list. `History::go_to` reaches any node: undo
to the common ancestor, then redo down the other branch (setting `next`
on the way); `project_at_node` makes the same state on a copy.

* **Ids.** A step's id is given when it is committed (1, 2, 3 … in the
  order made, kept through undo, redo and moves). `last_step` is the
  current node, `is_done` asks whether a node is on the current path,
  `knows` whether the tree still holds it, `inverse_of` (done steps) and
  `again_of` (steps not done: undone, or on another branch) give a step's
  commands as a batch to apply as a new step.
* **Gestures.** The open gesture is recorded apart from the tree and
  becomes one node when it ends; while it has changes, redo is off (the
  steps it would redo stay; a cancel brings redo back). `mark`/`rewind`
  (ripple drags) and coalescing work inside it as before; moving anywhere
  closes it first, as undo does. `apply_amending` (Record to Arrangement)
  amends the current node only while it is a leaf.
* **A failed replay** (a command that cannot be applied any more) takes
  back what it applied before failing: the project and the node stay as
  they were and the error is returned.
* **Memory.** At most `limit` steps (500) are kept, branches included. Over
  it, the oldest branch tip off the current line goes first; a history
  that is one line forgets its oldest step (the root becomes the state
  after it, `forgot_older`).
* **Branches** are numbered as they are made (the first line is branch 1;
  a node is its parent's branch when it was the first child). `tree` lays
  the tree out as rows: depth first, each branch right after the step it
  starts from, one level in, before the line it left goes on.

The session moves through it with `Action::HistoryToNode(id)` (the engine
synced once), `Action::HistoryBranch(n)` (the newest step of branch n)
and `Action::HistoryTo(n)` (a place on the current line); the History view
(`faderframe-view-history`) shows the rows from `Session::history_tree`.
Tests: `history::tree_tests` (branching, moves, copies, gestures, mark and
rewind, amending, ids across branches, the limit, a failed undo),
`session/tests/undo_tree.rs`.

### File format

`faderframe_project::file`: versioned JSON (`format`, `version`, `project`,
`workspace`). Loading parses into an untyped value, runs the migration chain
(`MIGRATIONS[v]` upgrades `v → v+1`), deserialises, then `Project::repair()`
fixes dangling references. Newer versions are rejected. Saving writes a
temporary file and renames it. A file always holds the song in place: with
a scratch pad shown, `to_string` writes `Project::song()` and names the pad
in `ScratchPads::open`, which loading shows again (older versions open the
song and leave the pads out; files without pads have no such field).

### Project versions and merging them

Versions are numbered project files in the project's `Versions` folder
(`session::versions`; media paths relative to the project folder, so the
folder moves with them). Compare names what changed since one
(`faderframe_project::compare::differences`); restoring one keeps the
project as it is as a version first ("Before restoring n"), and moves the
id allocator past the present's, so the line set aside can be merged back
without its ids meaning two things.

**Where a saved state comes from.** Every save stamps the file with a
`Lineage` (`faderframe_project::lineage`, in the file beside the project
with the media's content hashes: `file::FileExtras`, ignored by older
readers): a random id, the id allocator's next value, a generation, and the
states it descends from, newest first (at most 256). The project file, each
version and a hidden copy of each saved state (`Versions/.bases/<id>.ffproj`,
the newest eight, hard links where the file system has them — a save writes
a new file, so the link keeps the old content) are stamped. The session
keeps the stamp of the state it descends from; the next save is its child.

**The merge** (`faderframe_project::merge`) is three-way: a base both sides
descend from, ours (the project as it is) and theirs (a version, or a copy
someone else worked on). It works on the projects' JSON form, so every part
of the model — side tables like mix scenes, articulation and drum maps,
ARA archives, source words, tunings — merges without being named:

* objects merge field by field; maps by id and lists of things with ids
  (tracks, plug-in slots, sends, automation lanes, notes, markers,
  sections, scenes…) thing by thing, their order merged too (one side's
  reordering taken with the other's additions in place; both reordering
  is a conflict);
* lists of things placed in time (automation points, tempo and meter
  changes, keys, chords, lyrics, SysEx) merge by the stretches each side
  changed: stretches of one side are taken from it; where both changed an
  overlapping stretch, that is a conflict over the list's owner;
* taken whole: another enum variant, a thing replaced by another (a new
  id), a range (start and end move together), a plug-in's settings once
  either side loaded a new state (the blob and its values; otherwise
  values merge by parameter), take folders (comps name takes by place) and
  clip edits whose parts depend on each other (warp, pitch, spectral edits,
  clip effects);
* a change only one side made is taken, the same change on both sides
  once. A conflict — both changed the same thing differently, or one
  removed what the other changed — is resolved for the thing it is in (a
  track, a clip, a note, a plug-in; a project field otherwise): ours by
  default, theirs, or for clips, notes, markers and sections both (a copy
  of theirs beside ours with ids of its own). Conflicts are keyed by the
  thing's path of ids, stable between merges of the same versions, so a
  merge again with `MergeOptions::resolutions` makes the project for other
  choices.

**Ids.** Every kind is allocated from one counter, so two lines hand out
the same numbers for different things. Theirs is read with its own new
ids — from the point the lines branched (`Lineage::branch_point`: the
newest state both reach on their own lines; without stamps, the base's
allocator) — moved past ours: every id type reads and writes through
`faderframe_core::id::translate` (a thread-local flag, otherwise plain
numbers), so ids move wherever they sit, references and side tables
included. The merged state's stamp records the move (`IdShift`: the state
merged, the merge, the range and offset), and states taken in through a
merge are ancestors marked `merged`: a later merge with the same line, or
one the other side made with ours, reads the ids an earlier merge moved
as the things they already are (`Lineage::known_ranges`, forward and
back). Plug-in parameters that hold ids (an EQ Match reference) are values
and do not move.

**Checks.** The merged JSON is built back into types track by track and
clip by clip; one that will not read is taken whole from a side (listed as
a conflict that does not combine), then a top-level field that keeps the
project from reading. Each track's clip list is made again from the clips
(ours' order kept), the allocator set past both sides, and
`Project::repair` makes the cross references consistent (a clip kept on a
track the other side removed goes, and the notes say so).

**The session** (`session::version_merge`): `Action::MergeVersion {
theirs, base }` (base `None`: the newest common ancestor whose content is
here — a base copy, a version or the project file) and
`Action::MergeProjectFile` (someone else's copy: its media resolved
against its own folder, then each found here by content hash — the
project's media and its media folders, by the hashes stamped in their
file or computed — else used where it is, else reported missing; the copy
is kept as a base, its ids as merged, for their next file). The result is
applied as one undo step, `Command::ReplaceProject` ("Merge Version n
‘…’"), and left unsaved; `Action::ResolveMergeConflict` merges again with
the choice and replaces the step's result in place
(`History::apply_amending`) while it is still the last step;
`Action::AuditionMergeConflict` renders the merge with the conflict's other
choice through the A/B compare stage (`Comparing::MergeChoice`) against the
merge as applied. `Session::merge_status` gives the views the conflicts
(in words, `Conflict::summary`), the changes, the notes and the media
report. Content hashes (`version_merge::content_hash`) are the length and
SHA-256 of the file, of 64 blocks of 64 KiB spread over it past 4 MiB (at
most 4 MiB read a file, remembered for the session); a file found by its
hash is compared byte for byte when theirs is here. Where theirs is not, a
copy changed only between the blocks (a sample redraw's copy) can be taken
for the original.

UI: the Versions window's Merge buttons and "Merge from" choice, File →
Merge Project File…, and the Merge window (`faderframe-ui/src/version_merge.rs`:
a row per conflict with its choices and Hear Other, what changed, media).
Tests: `merge::tests` (disjoint edits, same-field conflicts, delete vs
edit, moved vs edited clips, notes, automation stretches, both sides
adding things, plug-in settings, order, tempo and markers, ranges known
from an earlier merge), `lineage::tests`, `id::translate::tests`,
`session/tests/version_merge.rs` (a version set aside merged back,
conflicts resolved in the step and auditioned, a copy from another folder
with media found by content and its next file, stamps and bases).

### Live collaboration

Several people work in one project at once, over a local network or the
internet (`faderframe-collab` is the network, `session::collab` the rest;
File → Collaborate…). One session **hosts**: it holds the authoritative
order of edits. Others **join** with its address and invite code and get
its project; from then on every edit anyone makes reaches everyone in the
host's order.

**The command stream.** Every change of a project is a `Command` applied
by `Command::apply` — edits, undo and redo, history moves, auditions
applied and taken back, a merge replacing the project whole. While a
project is shared, its **journal** (`faderframe_project::journal`, a field
of `Project` that files and clones never carry) writes each one down with
its inverse; commands inside a batch are written as the batch, once, and a
`ReplaceProject` keeps the journal where it was. Each tick the session
takes what was written as one *step* (neighbouring setters of one thing —
a fader drag — coalesced to the first inverse and the last value):

* the host gives it the next place in its log and sends it as an `Entry`;
* a peer keeps it as *pending* — already applied, so the person sees it at
  once — and proposes it. The host applies it where it falls in its order
  (all or none) and sends it back like any entry, or refuses it when it no
  longer applies (the clip was removed meanwhile).

**Rebasing.** A peer's project is the host's order so far plus its pending
steps. An entry from someone else takes the pending steps back (newest
first, by their inverses), applies the entry, and does them again on top
with fresh inverses; one that no longer applies is dropped and the person
told — the host refuses it too, since it sees the same project. Its own
entry coming back confirms the oldest pending step (commands alike:
nothing to do; else the entry as the host applied it takes its place). So
every session applies the same commands in the same order to the same
project. When edits pause, the host sends the SHA-256 of its project's
canonical form (media by content key, maps in key order, the id counter
left out: `collab::project_hash`); a peer with nothing pending whose own
differs asks for the whole project (`Resync`), its pending steps redone on
top.

**Last writer wins** on values: commands carry values, not differences,
so of two people setting the same thing the one ordered later wins, and
whoever set it in the half minute before is told who changed it since
(by coalescing key, or for setters without one by setter and target).
Structural clashes do not merge: the later edit refuses. (The three-way
merge of *Project versions and merging them* is for lines that went apart;
a live session never does.)

**Each undoes their own.** Remote entries never enter the local undo
history: undo takes back the person's own steps, applying their inverses
as a new step for everyone; what others changed since stays unless they
changed the same thing (then the undo sets it back to what it was before
the person's own edit — last writer wins again).

**Ids.** Every kind of id comes from one counter, so two sessions would
hand out the same numbers. Each session draws from ranges set aside for it
alone (`IdAllocator`'s `IdLease`, shared by the allocator's clones, never
in files nor compared): the host sets a range aside as a `Grant` entry of
its log (2²⁰ ids), so every copy's counter moves alike, gives a joining
peer its first range in the Welcome, and a session asks for more below
2¹⁸ left. After every entry each session sets its counter past every range
granted (`SetArrangement` and `ReplaceProject` set the allocator wholesale);
a project replaced whole (a merge, another project the host opened) may
hold ids in old ranges, so everyone's leases are dropped and granted anew.

**Media by content** (`collab::form`). A path means nothing on another
machine: on the way out every string that is an absolute path of a regular
file becomes `ff-media:<len>:<sha-256>/<name>` — found by walking the
serialised step, so any command that carries a path is covered — and so do
the files named inside samplers' states (`FFSD` blobs). Keys are SHA-256 of
whole files, hashed on a thread of their own (a step naming a file not
hashed yet waits, holding up the steps after it). On the way in a token
becomes the file with that key here, or the place in the media folder it
is fetched to (a name of its own when another file has it). Fetches run in
the background between everything else on the connection (edits and
presence first; 256 KiB chunks), resume from a `.part` kept from a lost
connection, and only a file whose SHA-256 is the sender's takes its name;
sources being fetched are not reported offline and open when their file
arrives (samplers naming it are reloaded). Peers fetch from the host; the
host fetches what a peer brought in (an import, a recording) from that
peer, answering others' requests once it has it.

**The wire** (`faderframe-collab`). TCP, `Noise_NNpsk0_25519_ChaChaPoly_SHA256`
with the invite code as the pre-shared key (`snow`, no `std` feature —
that would pull in ring): fresh ephemeral keys per connection, nobody
without the code gets past the first message, nobody between can read or
pose. Invite codes are 15 random bytes in Crockford base 32
(`K7QF-M2ZP-…`, read forgivingly). Messages are MessagePack
(`rmp-serde`, fields by name, exact floats — commands serialise with
`serde` like the project); a frame carries at most 65 518 bytes of a
message, larger ones are cut and joined. A connection's writer takes, in order:
messages that must arrive in order, the newest presence of each person
(older ones dropped), the host's mix (a few packets, the oldest dropped on
a slow link), a media chunk. Pings every 4 s; 20 s of silence ends a
connection. No relay server: over the internet the host forwards its port
(TCP 47400 unless chosen) on its router, or everyone joins one VPN.

**Coming back.** A peer that loses its connection keeps working (its steps
wait) and tries again every 2–10 s, saying who it was (`Resume`: its id, a
secret token from its Welcome, the last entry it applied). The host sends
the entries it missed (it keeps 64 MB of log) or, when the log no longer
reaches back, the project, and which proposal it answered last; proposals
answered but never seen applied were refused meanwhile, the rest are sent
again in order. A peer that leaves says goodbye; the host ending the
session ends it for all, each keeping their copy (unsaved).

**Presence and chat.** Each session sends its playhead, selection (tracks,
clips, edit range) and the clip open in an editor whenever they change (at
most ten times a second); the arranger draws the others' playheads as
named lines, their selected tracks edged and clips outlined, each in their
colour (`view-arranger/src/presence.rs`). Chat lines go through the host
to everyone, in its order.

**Plug-ins.** Slots and their state cross with the project whatever is
installed; a session lacking a plug-in keeps the slot and its blob
untouched (so it goes back to the others as it came) and is told once.
Plug-in window edits reach the project as steps where the device's own
history watches the window (*A/B compare and a device's own undo*).

**Listen Along** plays the host's mix on a peer — for missing media or
plug-ins. While a peer listens, the host's engine writes the master's
post-fader audio into a second scope ring (`EngineContext::share`,
`EngineController::{share, set_share_source}`); the host codes it each tick
as stereo Opus (`faderframe_codec::stream`, 128 kbit/s, 20 ms frames,
resampled with the codec monitor's filters) and sends numbered packets.
The peer decodes at its engine's rate (a missing number concealed by
Opus) into a wait-free ring read by `nodes::along` in the listening node,
first of all (before the compare source and the codec monitor): heard in
place of the own mix once 150 ms wait, faded over 30 ms; a ring that runs
dry is silence until primed again; one holding more than twice that (the
host's clock running faster) drops the surplus. Monitoring only, like the
other stages there.

Tests: the crate's units (codes, the handshake with a wrong code, frames of
any size, media resume and damaged parts, messages), `tests/link.rs` (a
host and clients on loopback: hello and welcome, a 1 MB step, presence,
goodbye, a wrong code refused, a file fetched and resumed), project
`journal::tests`, core `a_lease_hands_out_its_ranges_and_then_the_counter`,
session lib `collab::form::tests` (the demo project crossing exactly,
tokens, samplers, hashes alike across folders) and `collab::tests`,
`session/tests/collab.rs` (sessions in one process on loopback: joining,
concurrent edits ending alike with new ids from both sides and the
override told, per-user undo and redo, an edit of something removed
refused, media both ways and to a third session through the host, a lost
connection coming back with work done on both sides meanwhile, the host
ending the session, a wrong code, Listen Along packets flowing on a running
device), engine `tests/listen_along.rs`, `listening_along_does_not_allocate`,
codec `stream::tests`, arranger `presence_tests`.

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
  covered because they are all just nodes (details below).
* **Scheduling** is dependency-driven and multicore (below).

### Plugin delay compensation and what is heard

**The latency contract.** `Processor::latency()` is the delay a node adds,
in whole frames at the graph's (host) rate, read when compiling; zero
unless declared. Plugins report through `PluginInstance::latency_samples()`
in host frames — built-ins compute it from their parameters (a device that
oversamples or runs a reservoir reports its total in host frames; the
EQ's linear phase is `N / 2 + PARTITION`, its spectral stage one frame
size and the hop its work is spread over, natural phase `PRE +
PARTITION`: resolution 1 linear phase with a spectral band is 4352 + 2048
+ 512 = 6912 frames, whatever the rate, so 144 ms at 48 kHz and 72 ms at
96 kHz), CLAP/VST3/LV2/AU from the plugin. A
change (phase mode, resolution, a CLAP `latency.changed`, VST3
`kLatencyChanged`, an LV2 latency port) makes `poll()` ask for a restart:
the graph is rebuilt off the audio thread and swapped in whole; latency is
part of a plugin node's key, so the old processor is not adopted into the
new place. Bypassing a plugin from the slot rebuilds with the node at zero
latency (a plain pass-through); *automated* bypass is soft — the plugin
keeps running and the dry signal is delayed by its latency, so nothing
moves. Nothing counts a latency twice: a node reports its own, the graph
adds upstream. `engine/tests/pdc.rs` measures the EQ: an impulse alone
comes out 6912 frames late (the reported figure), and beside a dry track
both meet on that sample.

**Arrival.** `compile()` walks the nodes in topological order: a node's
*arrival* is the latest output latency among its non-early inputs (and the
node it follows without an edge, a hardware insert's send); each input is
delayed by arrival − its latency (`AudioDelay`/`EventDelay`, ring buffers
of delay + max block, made with the graph; any block size, delays not
multiples of it). Serial plugins add (128 + 6400 + 256 = 6784); paths that
split and meet again (sends to a return with latency, containers' chains,
nested buses) meet on the same sample — the compensation goes on the
shorter edge at the summing point only, so nothing is delayed twice.

**Outputs heard together.** After the walk, every *heard* output — the
device outputs and MIDI outputs, not a hardware insert's send (its return
times itself from it) — is held back to the latest of them: a direct out
on 3/4 waits for the master on 1/2. `GraphStats::output_latency` is that
figure: how far behind the timeline processed every output is.

**Feedback.** The graph is a DAG: routings that would close a loop are
refused by the project layer and the compiler (and crosstalk that would
form one is skipped with a warning), so there is no cyclic case to
compensate.

**Automation behind latency.** The graph tells each processor its arrival
(`Processor::set_input_latency`, at compile time and again when a processor
is adopted into a new graph). A node whose input arrives `L` frames late
processes audio that was played `L` frames ago — so strips, sends,
crosstalk and plugin nodes read their automation there
(`nodes::automation_at(cx, latency, offset)`): a fader step after a
6400-frame plugin comes 6400 frames later than its curve's point, exactly
with its audio. "Played `L` frames ago" goes through the **play history**
(`engine::history::PlayHistory`, in every graph's `EngineContext`): each
chunk processed is recorded (graph frame counter, timeline position,
playing, the transport's jumps), as runs that continue each other — a
fixed ring of 64 runs, no allocation. Reading back across a loop wrap, a
locate or a stop gives the position from before it
(`PlayHistory::pieces` splits a block where a run starts; plugin
parameter events follow each run). The render-ahead graphs keep their own
history: each anticipator records every chunk it renders, as the audio
thread does. (Until 0.22.x they recorded none: everything behind latency
on a track rendered ahead — a plugin after a latent one in the deep tier,
the strip of a latent track in the shallow tier — read its automation and
modulation at position 0 for ever, a mute that never came, a fader shut
at its first value; `automation_behind_latency_renders_ahead_as_it_plays`
compares both tiers with the audio thread sample for sample.)

**Modulation behind latency.** Modulators are evaluated where the track's
input is (`ModNode`) and stamped with the graph frame of the audio they
belong to: the `ModBus` keeps the last `MOD_HISTORY` (1024) blocks, each
stamped `chunk_start − input latency` of the modulation node. Strips and
plugin nodes read the block of their own audio
(`TrackModulation::{strip_at, plugin_mods_at}` at `chunk_start − their
arrival`): an LFO on a device after a 6400-frame plugin moves with the
audio it shapes, not 6400 frames early (`engine/tests/modulation.rs`
`modulation_behind_latency_follows_its_audio`).

**A latency change while playing** (a phase mode switched, a plugin
reporting another figure) swaps in a graph whose paths line up again —
but the audio would jump by the difference. The engine compares the two
graphs node by node (`CompiledGraph::timing_changed_from`: a merge over
their sorted keys, no allocation) and, when anything that both have sits
at another latency, dips instead of clicking: the old graph fades out over
`DIP_SECONDS` (4 ms), a few silent blocks while the delay lines fill, the
new one fades in (`engine.rs` `Dip`, `apply_dip` after the render;
`a_latency_change_while_playing_dips_instead_of_clicking`). A change no
path notices (a new track) installs at once.

**The metronome** plays as late as the outputs: its beats are those of
the history `output_latency` back (a locate restarts it as heard). It
clicks the first beat of each bar, the other beats and — when given a
level — the off-beats (the eighths between beats), each at its own level
under one volume (`MetronomeShared` atomics). The sounds (`ClickSounds`: a
built-in `ClickSound` synthesised, or the user's audio files, an optional
second one for the first beat) are made on the control side at the
stream's rate (`session::metronome`, made again when the rate changes) and
handed over whole through a mailbox; the audio thread plays them in a few
overlapping voices (Preferences → Metronome; right-click the metronome
button).

**Timing model: processed versus heard.** Four positions, kept apart:

* the *timeline* (musical) position — what clips, automation, markers and
  edits are anchored to; nothing is moved for latency;
* the *processing* position — `TransportState::position`, where the
  sources are being read (`Session::processing_position`);
* the graph's *output latency* (above) and the device's (`buffer +
  driver output latency`, varispeed's resampler):
  `EngineController::output_latency() = device_latency() +
  graph_latency()` — processing to hearing;
* the *heard* position — the timeline position of the audio leaving the
  speakers now.

After every callback the audio thread publishes the history around what
will be heard until the next one (`history::HeardClock`: a seqlock over
atomics — the reader retries, the audio thread never waits) with the
callback's start time. `EngineController::heard_at(now)` maps a time to the
heard position: frames since that callback at the stream rate (× speed),
less the device latency and the graph's, looked up in the runs — exact
across wraps and locates, never past what was processed, `None` after a
second without callbacks. While a locate or stop has not reached the ears
it reports where the transport went (`Heard::jumped`).

`Session::transport()` / `playhead()` present that: computed when asked
(views painting get their own frame's position, not the 16 ms tick's),
steadied — callbacks are stamped when they start, which jitters, so the
playhead moves at the playback rate and eases onto the measured position
(within ~0.2 s), and takes a new run (wrap, locate, start, stop) or a
far-off measurement as it is. A locate shows its target at once and waits
there until its audio arrives; play from stopped waits at the start while
the latency fills. Until the engine has applied the locate the session
holds the target (`Session::show_position`), released by a jump *to* it
(`TransportSnapshot::jumped_to`): a Stop sent just before is a jump too,
to where playback stopped, and released it early, showing that position
in between (a chase lands near its target, not on it: any jump releases
it, `show_position_until_jump`); pictures show the held target as well. Everything drawn from the transport — arranger and
piano-roll playheads, follow scrolling, the transport display, surface
timecode, automation values shown (`display_value` reads the curve at the
playhead) and automation written while touching (at the playhead the
player heard) — follows the heard position. Things timed against the
engine use the processing position (launcher quantisation, stop-at,
render-ahead). External sync uses `output_latency()`: MIDI clock and MTC
are sent when their position is heard; captured MIDI, recorded SysEx and
MTC chase all count the graph's latency too. Video pictures (the video
window, picture outputs) read the play history itself for the moment the
frame reaches the screen (`heard_ahead`: as `heard_at`, but going on past
the frames processed when the screen is further ahead than the sound's
latency): after Play they stay on the frame standing until the latency
has filled, as the sound does — the processing position less the latency
went back before it, to the frame before (or none at a clip's start) —
and a locate's target shows at once.

**Meters** read a strip as it is processed; its audio is heard after the
latency still ahead of it (buses, the master's devices, compensation —
`EngineController::meter_delay(track)` = output latency − the latency
where it is metered) plus the device's. The session holds each reading
back by that (`Session::tick_meters`: per track a queue of stamped readings,
ballistics run as they are shown), so a dry track's meter beside a
6400-frame one lights when it is heard, the latent one's at once; peaks and
overs are kept, just later. The Tools analysers read the master's scope as
processed and **read as heard**: `poll_analysis` reads the scope up to
`written − (meter_delay(source) + device latency)` (`ScopeRing::read_between`),
so a spectrum or loudness reading of a track behind a 6400-frame plugin
moves with what is heard (`the_analyser_reads_its_track_when_it_is_heard`).

**Recording.** A player hears playback the output latency after it is
processed and plays along: takes move back by the device's input and
output latency, the user's offset **and the graph's output latency**
(`session/tests/pdc.rs`: a take of the playback through a patch cable
lands on the same sample with 6400 frames of plugin latency as without).
The input latency is the take's own channels' (`Session::input_latency_of`):
on a stream that joins several devices, one following the master's clock
comes a few milliseconds later (its ring and resampler, see "Several audio
interfaces"), and only its takes move back by that more
(`ActiveRecording::later`).

**Offline renders** run the same graph through `OfflineRenderer` (one
engine, the same compensation and history) and drop `output_latency`
frames from the front, rendering that much further so the tail is whole
(`render_one`; freezes keep the latency and are claimed by the frozen
player). Renders never use low-latency monitoring (their engines are
their own).

**Low-latency monitoring** (Audio → Low-Latency Monitoring,
`Session::set_low_latency_monitoring`, remembered): tracks *heard live* —
a hardware input monitored (always, or armed with auto monitoring) or
played from a MIDI keyboard (`build::live_monitored`) — bypass their
effects with latency (instruments never) and their outputs and sends are
**early** edges (`GraphBuilder::connect_audio_early`: summed as they
arrive, not delayed to the other paths, not holding them back). The
player hears themselves at once instead of after the slowest plugin
elsewhere; everything else stays aligned, takes are placed as always. A
track's own playback while it is heard live is early too. **The whole way
out** is cleared: the buses, returns and master a monitored track reaches
(`build::monitored_path`) bypass their effects with more latency than the
limit (Preferences → Audio, `Session::set_low_latency_limit`, default
`DEFAULT_LOW_LATENCY_LIMIT_MS` 3 ms — short lookaheads and console stages
stay), and the compiler's `clear_the_way` pass carries early audio through
every summing point after it (a bus summing an early input and late ones
lets the early one through at once and delays the others to each other),
so nothing on the path waits (`early_audio_passes_every_summing_point_after_it`,
`low_latency_monitoring_clears_the_whole_way_out`). Arming or a live MIDI
change rebuilds when the set changes; the Performance view lists the
tracks and the effects bypassed ("bypassed while monitoring").

**Cost.** Recording a callback in the history, 64 read-backs and the
heard clock: ~136 ns (`history::cost::cost_per_callback`, release). The
128-track benchmark (64 frames, one thread) with 6400 frames of latency
on every other track: 280 → 316 µs per callback, the delay lines' memory
traffic; pan automation behind it ~8 µs more than without latency
(`faderframe-bench --latency 6400 [--pan-automation]`).

**Diagnostics.** The Performance view (F8) shows per plugin its latency,
per track the latency where it is metered or the compensation it waits
for, and the graph's compensation and processing-to-hearing totals in
frames and milliseconds.

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
  returns when every job is done. A helper that finds no ready job for
  `LEAVE_AFTER` (15 µs) leaves the cycle (its seat freed,
  `faderframe_realtime::leave_running_job`: it sleeps after 25 µs instead
  of ~100) and a thread that queues jobs calls it back
  (`WorkerPool::call_back`, the generation word the idle workers watch);
  the caller keeps working the cycle. While one node runs long — a
  sandboxed plugin waiting for its helper, a plugin waiting for threads of
  its own — the helpers would otherwise spin through it at the audio
  thread's priority: TONE3000's stereo engine (#49) hands its channels to
  JUCE realtime threads that fall back to normal priority where
  `RLIMIT_RTPRIO` is below 99, and those found no core free of spinning
  FIFO threads — dropouts and a stuttering desktop. Measured (PipeWire, 64
  frames, the demo stopped, 4 cores): pool workers 24 % → 9 % of a core
  each with TONE3000's "Stereo Crunch Verb"; `helpers_do_not_spin_through_a_long_node`
  (a 30 ms blocking node: 360 ms of worker CPU in 4 cycles before, under
  32 ms now). Graphs whose width is one job, or whose
  measured work is below `parallel_min_ns` (40 µs, with hysteresis), stay
  on the audio thread — waking workers would cost more than it saves.
* **Critical path first.** Every job's time is measured (two clock reads).
  Every 16 cycles the audio thread smooths the costs and recomputes each
  job's rank — its own cost plus the most expensive path to the end —
  without allocating; roots start in rank order and a thread continues with
  the released dependent of highest rank. A track with several heavy
  plugins therefore starts first instead of becoming the tail of the cycle.
  The verdict on threading carries over when an edit swaps the graph.
* **Affinity.** Each thread has a seat (the caller 0, pool worker `i`
  seat `i + 1`, `faderframe_realtime::worker_seat`, folded onto as many
  queues as the cycle has seats) and a ready job goes to the queue of the
  seat that ran it last, so its processors' state and buffers tend to stay
  in that core's caches; a thread takes its own queue's jobs first and
  steals from the others' when it has none (a seat whose thread did not
  join is emptied that way). Queue entries carry the cycle, so resetting a
  cycle clears nothing. Unpaced, 8 threads: 128 tracks of default devices
  at 64 frames 6–9 % fewer cycles, 32-frame buffers 2–4 %, echo-heavy
  graphs 3 % (`faderframe-bench … --seconds 30` under `perf stat`). A
  rank-aware variant (stealing a far more expensive head) cost more than it
  saved; so did a shared count of queued jobs.
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
  strip, so faders, pan, mute, solo and sends stay immediate. Everything
  live stays on the audio thread.
* **Buses** (Preferences → Audio → Buses too, on by default;
  `build::ahead_sets`): a bus, aux or the master's devices render ahead
  when everything reaching it is a strip that can — a track (or bus)
  whose output and sends all go to buses rendered ahead, that keys no
  sidechain or follower, without crosstalk (a fixpoint; while playing
  nothing joins, and a strip that has to come back brings its chain and
  its buses). They run in a second, *shallow* anticipator: it reads the
  deep one's rings of those tracks' chains and runs their strips and sends
  (quiet: their meters, scope and automated values come back through a
  ring to a `StripEcho` on the audio thread), the buses' sums and devices,
  in 128-frame blocks only a device callback and two blocks ahead
  (`ahead::shallow_lookahead`), at the audio thread's scheduling (the link
  publishes it once). A block is rendered there once the deep tier has
  (its `progress`); the link hands every sequence to both tiers, shallow
  first, and switches when both are ready. Fader moves on those strips
  are heard after about 10 ms (13 ms with 256-frame callbacks; 5 ms
  live), bus devices still leave the audio thread. Buses are built after
  what reaches them, so their latency is known; the graph compiler sums a
  port's audio in the sources' key order, so the different build order
  keeps the output bit-identical.
* **Prediction.** The anticipator runs its own copy of the transport
  (`TransportState` is plain data), so it renders exactly the positions
  the audio thread will ask for, loop wraps and scrub snippets included.
  A command that changes them starts a new *sequence*: the audio thread
  hands the commanded state over (`AheadLink::command`, wait-free) and
  carries on until the rings hold two blocks of it (`ready`), at most
  250 ms, then switches — play, stop and locate take effect a few
  milliseconds later, without a gap. Commands that change nothing start
  none; recording on/off is applied at once.
* **Loop changes** start no sequence either (a loop dragged while
  playing used to start one per pointer move, and each switch put the
  playhead back by the frames played while the anticipators primed — a
  callback of audio repeated per move; measured with 256-frame callbacks:
  40 of 40 changes repeated one, `resizing_the_loop_while_playing_…` in
  `engine/tests/ahead.rs`). What is rendered stays valid up to where the
  anticipators are, so the audio thread hands the new range over as an
  *amend* of the current sequence at a frame beyond all of them (their
  progress plus two of their blocks, `AheadLink::amend_frame`); every
  tier and the audio thread itself take it on exactly there — their
  blocks and the callback's chunks end at that frame
  (`AheadLink::amend_due`) — so all follow one path: nothing repeats,
  drops out or is rendered twice, and stateful effects run on undisturbed.
  The new range is in force about the lookahead after the change: a wrap
  already rendered still happens once (pushing the end out just before the
  playhead reaches it), an end pulled in further ahead wraps there, one
  pulled in behind the playhead lets it play on. Stopped or scrubbing, a
  loop change moves nothing that plays and applies at once. An
  anticipator that has already passed the frame (a preempted audio thread
  between reading progress and handing over) keeps what it rendered when
  neither range wraps in between (`amend_late_is_equal`), else reports it
  and the audio thread starts a sequence after all; a full channel (32
  changes in flight) does the same. A new link (rebuilt anticipators)
  takes over what the old one was waiting for (`AheadLink::settle`).
  Tests `resizing_the_loop_while_playing_neither_repeats_nor_drops_out`,
  `loop_changes_near_the_playhead_land_beyond_the_lookahead`,
  `rapid_loop_changes_while_playing_stay_seamless` (deep and shallow
  tier), `a_loop_changed_while_stopped_holds_from_the_first_pass`, the
  link's unit tests, `rendering_ahead_does_not_allocate_on_the_audio_thread`
  (loop changes mid-callback).
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
  (`faderframe-bench --ahead 200`); 64 tracks into 4 buses, two echoes
  each, 64-frame buffers: with the buses rendered ahead too, callbacks
  mean 36 → 16 µs, p99 53 → 24 µs (`--ahead-buses`). Tests: `engine/tests/ahead.rs` (output
  identical sample for sample through playback, a locate and loop wraps,
  with buses rendered ahead and latency reaching them, no late blocks,
  faders reaching buses answering within milliseconds), `realtime_alloc.rs` (the audio thread's side does not
  allocate), `session/tests/render_ahead.rs`.
* **Paced tests on a busy machine.** Those tests play the device
  themselves, at its pace, with the anticipators beside them — but in a
  test the audio thread they follow is an ordinary thread, so they are
  ordinary threads too, and a machine loaded with builds keeps them from
  the shallow tier's few milliseconds (or, rarely, the deep tier's
  lookahead) once in a few seconds: a block of silence, a counted miss,
  where nothing is wrong. Each paced comparison is therefore tried again
  when late (`on_time`), and the tries after a late one — and
  `rendering_ahead_does_not_allocate_on_the_audio_thread`'s, which used to
  fail on such a miss — play a device that waits, before a callback,
  until every tier has rendered what it plays
  (`EngineController::ahead_ready_for`: the link publishes the sequence it
  plays, the frames played and the sequence pending (`AheadWatch`); a
  pending one is waited for until ready everywhere, since the tiers have
  left the old one; the tests' `common::Patience`, 30 s a run at most).
  Every try still counts allocations and asserts the audio bit for bit,
  and a patient one still counts misses: audio that never arrives — a
  sequence rendered the wrong way, an empty ring — is a miss there after
  the budget. What the patience gives up is only the proof that this
  machine kept the deadline, which no test under a load can give. The
  audio thread itself never waits.
* **A new sequence with buses rendered ahead.** The shallow tier holds
  only a device callback and two small blocks of what plays, while the
  deep tier may take a while to start a new sequence (a millisecond's
  sleep, then two of its blocks of the heaviest chains) — and the audio
  thread plays the old sequence until every tier has the new one ready.
  So the shallow tier keeps a new sequence waiting (`next`, with the loop
  changes handed over for it) and renders the old one on from the deep
  tier's rings, which still hold the lookahead's worth of it, until the
  deep tier has the new one *ready* (`Options::upstream_ready`: its first
  blocks rendered — not merely started, or the shallow tier would leave
  the old sequence while the audio thread still waited for the deep
  one); then it starts it, two of its small blocks from what the deep
  tier has rendered. The first try left both tiers at once, and the rest
  of a slow start played silence: `tests/ahead_handover.rs` keeps the
  deep tier from a locate's first blocks for 100 ms with a stalling
  plugin and finds no block missing and no callback silent (it found 14
  misses and 7 silent callbacks before).

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
pan (see *Panning* below: mono pan by the project's pan law, balance or
dual pan for stereo sources). All gain changes are ramped per block. A strip
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

### Mono → Stereo inserts

A hosted plugin's node takes the plugin's own main port widths
(`PluginCx::node`); the graph converts at its edges
(`for_each_channel_route`: mono → N duplicated, N → mono averaged). So a
stereo plugin on a mono track gets the track twice and its pair is folded
back into the track, `(L + R) / 2` — an amp simulator's stereo preset
(TONE3000's "Stereo Crunch Verb") plays folded to mono. A slot set to
**Mono → Stereo** (`PluginSlot::mono_to_stereo`, `Command::SetPluginMonoToStereo`,
Impact Graph; written in the file only when on) keeps the pair: it is one
of the slots that widen a mono track (`PluginSlot::widens_mono`, beside the
Guitar Station and the Hardware Insert's own switch), so the track is
stereo from it on — `Track::chain_layout`, `layout_into`, the graph (live,
render-ahead, captures), freezes and renders agree. Offered
(`Session::mono_to_stereo_offered`) in the mixer's insert menu where a mono
signal reaches a device whose main output has two channels or more, never
for devices that widen by themselves, have their own switch, hold chains
or sit in the input stage; refused on frozen tracks (their audio is mono).
Not automatic: most effects report stereo ports, and every compressor
would widen its track. Plugins are not asked for a mono input bus (their
stereo input gets the track twice). Dev action
`mono-to-stereo:<insert n>=<on|off>` (the selected or first audio track).
Tests: project `a_slot_set_to_mono_to_stereo_turns_a_mono_track_stereo_after_it`,
engine `tests/mono_to_stereo.rs` (a hosted stereo effect: folded to the
average, or kept), session `tests/mono_to_stereo.rs`, mixer
`a_mono_strips_insert_menu_turns_it_stereo_after_the_insert`.

### Panning

Positions are −1…1 (hard left … hard right, 0 centre) everywhere: the
project, lanes, parameter slots and `faderframe_core::pan`, whose functions
clamp and read NaN as centre (`pan::position`) so no value can reach the
audio as NaN. Gains are a 2 × 2 `PanMatrix`, `m[source][destination]`. The
strip chooses by what it has to place — its input is `Track::chain_layout()`
(after the inserts, so a device that widens a mono track makes it stereo),
its output `Project::destination_layout`:

* **Mono pan** (a mono source into stereo): `PanLaw::mono_gains`, by the
  project's pan law (`Project::pan_law`, Preferences → Project; the
  default is −3 dB). Every law gives exactly `(1, 0)` hard left and
  `(0, 1)` hard right and is mirror-symmetric:
  * −3 dB, constant power (default): `θ = π (p + 1) / 4`, `L = cos θ`,
    `R = sin θ` (computed as `sin(π/4 (1 − p))`, `sin(π/4 (1 + p))` so the
    ends are exact); `L² + R² = 1`; centre 0.7071 (−3.0103 dB).
  * −4.5 dB, compromise: `L = √(cos θ · (1 − p)/2)`, `R` likewise — the
    geometric mean of the −3 dB and −6 dB laws; centre 0.5946
    (−4.515 dB); power dips 1.5 dB at centre.
  * −6 dB, linear: `L = (1 − p)/2`, `R = (1 + p)/2`; `L + R = 1`, so the
    output's mono sum never changes; centre 0.5.
  * 0 dB: `L = min(1, 1 − p)`, `R = min(1, 1 + p)`; centre at unity, a
    centred source 3 dB louder (power) than one at a side.
* **Balance** (a stereo source into stereo, `PanMode::Balance`, the
  default): `L·min(1, 1 − p)`, `R·min(1, 1 + p)` — centre unity on both
  sides, the opposite side attenuated linearly, a diagonal matrix (no
  crossfeed, nothing summed, left stays left). The pan law never applies.
* **Dual pan** (a stereo source into stereo, `PanMode::Dual`,
  `Track::dual_pan`): each input channel is a mono source placed by the pan
  law from its own position — `out_L = in_L·law(l).L + in_R·law(r).L`,
  `out_R = in_L·law(l).R + in_R·law(r).R`. The defaults (`DualPan::ORIGINAL`,
  −1/+1) are the identity, as is balance at centre, so switching an
  unpanned track changes nothing. Moving the positions together narrows
  the image, moving both one way moves it, `(+1, −1)` swaps the sides.
  Where the positions meet the channels add: at one position the source is
  mono there, `(in_L + in_R)` by the law (−3 dB law, centre: `0.707 (L + R)`
  per side — correlated material rises 3 dB, uncorrelated keeps its power;
  antiphase material cancels as it would in any mono fold). With the −3 dB
  law each channel's power is spread, never changed.
* Into a **mono** destination there is nothing to pan: the graph's channel
  rule folds stereo to `(L + R)/2` and pan, balance and dual pan do not
  apply (`Project::pans_stereo` is false; the UI offers no mode). Other
  layouts (`Discrete`, beds) go through the graph's rules or the surround
  panner's matrix (below), never through these laws.

Stereo **width** is not a strip control: the Utility insert has an M/S
width (and balance, channel modes, mono bass), and dual pan narrows by
position. The mode is per track (`Track::pan_mode`, `Command::SetTrackPanMode`);
both settings are kept whatever the mode, so switching back restores the
earlier sound, and lanes of the other mode stay in the project (they do not
drive anything until the mode returns).

**In the engine** the law, the mode and both dual positions are parameter
slots (`SlotRegistry::pan_law`, `StripSlots::{pan_mode, pan_left,
pan_right}`), so every pan edit — the law and the mode too — is
`Impact::Params`: no rebuild, applied at the next block. Each
source/destination pair ramps from its gain at the end of the last range
to the new one. When only positions move (same mode and law) they glide:
the strip interpolates the *positions* across the range, a matrix every
16th of the pan range moved (`GLIDE_STEPS_PER_UNIT`, at most one a frame),
the gains ramping linearly between — a hard-left-to-right jump keeps the
−3 dB law's power within 0.003 dB on the way instead of dipping 3 dB as a
crossfade of the gains would. A change of mode or law crossfades the
matrices over the block. Pan automation (`TrackPan`, `DualPanLeft`,
`DualPanRight`) is read every `AUTOMATION_STEP` (32) frames like the fader,
from absolute timeline positions, so with blocks that are multiples of 32
any block size renders identically (`automation_plays_the_same_whatever_the_block_size`).
Pan modulation moves the mono pan or balance, and a dual-panned image as a
whole (both positions, each clamped). No pan stage adds latency, buffering
or allocation (`panning_does_not_allocate`).

**Signal path.** Pan sits in the strip after the inserts and the fader:
the post-fader output (out 0) is panned into the destination layout and
feeds the destination, post-fader sends and the meters; the pre-fader
output (out 1) and pre-FX taps are unpanned, in the track's layout, so
pre-fader and pre-FX sends ignore both the fader and the pan (a mono
track's pre-fader send reaches both sides of a stereo return at unity, the
graph's channel rule) while post-fader sends carry the panned, faded
signal. Buses and the master pan their own sum once with their own control
(a stereo bus or master: balance or dual pan) — nothing is panned twice.
Pre-fader output 1 is after the mute and the solos (a muted or solo-muted
track's pre-fader sends are silent); the cue mixes' taps, outputs 2 and 3,
are not (see *Cue mixes*). A send with a pan of its own
(`AuxSend::pan`) places its source by it (see *Send pan*). Into a surround
bed the surround panner takes over (below). A strip's
meters show its post-fader, post-pan output in the destination layout.

**Returns and buses** pan as every strip does — verified on the output
samples, not the control: a stereo aux return by its balance (or dual
pan), a mono one by the law, whatever feeds it (mono or stereo tracks,
pre-FX, pre-fader and post-fader sends), its pan lane on time, into a
bed by its own spatial model (a mono return by the panner, a stereo one
balanced into the bed when set to); buses, folder buses and nested buses
each once (`aux_returns_pan_whatever_feeds_them`,
`an_aux_returns_pan_lane_moves_it`, `aux_returns_pan_into_a_surround_bed`,
`tracks_and_buses_sum_with_each_pan_applied_once`, `folder_buses.rs`), and
in a running session with buses rendered ahead (the shallow tier's strips
read the same slots) the knob's edit is heard there
(`returns_buses_and_folder_buses_pan_in_a_running_session`).

**Compatibility.** `pan_mode`, `dual_pan` and `pan_law` are new optional
fields (written only when not the default), so files from before load as
they always played — mono −3 dB constant power, stereo linear balance — and
the file version did not change. Older FaderFrame builds ignore the fields
(a dual-panned track opens there as balanced). Track presets carry the mode
and positions the same way. Tests: `faderframe-core` `pan::tests`
(reference values, invariants, NaN/out-of-range), `engine/tests/panning.rs`
(output samples per law and mode, glides, mode switches, automation
timing, buses, sends, the master, mono destinations, phase),
`project/tests/project.rs` (undo, files, older files), `session/tests/panning.rs`,
the mixer's and the arranger's view tests.

**Controls.** A mono strip has one Pan knob; a stereo strip's knob is a
Balance (tooltip, accessible name and lane say so). Right-click it (or the
strip's or track header's menu → Pan Mode) for Balance / Dual Pan; in dual
pan the mixer shows two knobs side by side — left input, right input —
each with its readout (click to type, double-click: back to its side,
wheel, Shift/Ctrl fine, MIDI learn per side). The arranger's header keeps
one knob: its pointer at the image's middle, a dot per channel; dragging it
moves both channels, a double-click restores them. The "Pan" MIDI-learn
target addresses the mono pan / balance (a dual-panned track's sides are
learnable on their own knobs). A control surface's pan pot on a
dual-panned track moves the image as a whole — stopping at the side, never
narrowing it — or, with Shift held, the left side, with Option the right;
its ring and display show the image's middle (or that side; `L60R40`,
`<L60`, `R40>`), OSC's absolute pan centres the image there
(`session::control` `set_dual_pan_from_surface`, test
`a_pan_pot_moves_a_dual_panned_image`).

### Surround beds

Channel-based beds before any object-based format: `faderframe_core::surround`
defines the formats (LCR, quad, 5.0/5.1, 7.0/7.1, 5.1.2/5.1.4, 7.1.2/7.1.4;
`ChannelLayout::Surround`) with their channels in WAVE channel-mask order —
files and device outputs need no reordering (hardware outputs map a bed's
channels in that order from the output's first channel). Each `Speaker`
has an identity (label, mask; the LFE flagged, with no place or direction)
and two descriptions of where it stands: a place in a room box (`x`
left/right, `y` back/front, `z` ear level/ceiling) for the room panner and
a direction (`azimuth` in degrees, 0 ahead, positive to the left as in
ITU-R BS.2051/ADM; `elevation` above ear level) for VBAP and binaural
listening — one table both use. Fronts stand at ±30°, 5.x surrounds at
±110°, 7.x sides at ±90° and backs at ±135°, quad's rears at ±135°, tops at
±45°/±135° and 30° up (x.1.4), top middles at ±90° and 45° up (x.1.2).
`Speaker::role` (front, centre, surround, height, LFE) and `side` come from
the direction, never from a channel index. Any audio track, bus, aux or the
master can be a bed (Channel Format in the track menus,
`Session::format_choices`).

* **Custom speaker layouts** (Audio → Speaker Layouts…,
  `faderframe-ui/src/speaker_layouts.rs`; dev action
  `speaker-layout:<name>=<label>@<az>[/<el>];…`, `LFE` alone the LFE):
  a `LayoutDef` lists up to `MAX_SPEAKERS` (16) channels in order — the
  order of the device's outputs and of files — each a direction
  (azimuth −180…180°, elevation −90…90°) or the LFE (at most one).
  `LayoutDef::validate` refuses an empty or built-in name, repeated or
  empty labels, a second LFE, no main speaker and anything VBAP cannot pan
  over (`Vbap::new`: two speakers within 1°, a 3D set that leaves the
  listener outside). `LayoutDef::intern` makes it a format,
  `SurroundFormat::Custom(&'static CustomLayout)`: interned once per
  definition (the leaked layout lives as long as the program; formats
  compare and hash by it), so `ChannelLayout` stays `Copy` and the audio
  thread holds a plain reference — no lock, no allocation. Each speaker's
  room place is derived from its direction (on the box's walls; above ear
  level the top layer), its WAVE mask is 0 (files carry no speaker
  positions for it). The project keeps its library
  (`Project::speaker_layouts`, `Command::SetSpeakerLayouts`, no audio
  impact); a track's file entry carries the layout it uses in full, so a
  project or a preset reads back on its own. `Session::save_speaker_layout`
  (`Action::SaveSpeakerLayout`) moves the tracks on a layout it replaces
  to the new version in the same undo step; `remove_speaker_layout` refuses
  a layout in use. Into or out of a custom layout a bed's speakers are
  placed by VBAP at their directions (between built-in formats by their
  room places, as before); into stereo or mono by the downmix's roles
  (from the directions); IAMF refuses a custom master whose speakers do
  not all have a place in the IAMF layout (by label), ADM writes speakers
  outside a standard bed as fixed objects at their places.

* **Panning.** A mono or stereo track feeding a bed
  (`Project::surround_panned`) is placed by its `SurroundPan`
  (`Command::SetTrackSurround`) through one of two panners
  (`SurroundPan::panner`; each keeps its own values when the other is
  chosen):
  * **Room** (the default; what older projects use): x, y, z —
    power-preserving crossfades between the floor and the top layer, the
    rows of a layer and the speakers of a row, as console and Dolby bed
    panners work. Objects are always placed by it (their ADM metadata is
    the room position; `Project::effective_surround`).
  * **VBAP** (`faderframe_core::vbap`): azimuth and elevation. A source
    plays on the two (horizontal formats) or three (formats with tops)
    speakers around its direction, `p = Σ gᵢ sᵢ` solved with the
    precomputed inverse of each region's matrix and normalised to
    constant power (`Σ g² = 1`); exactly one speaker at its own direction,
    continuous across region edges, mirror-symmetric in symmetric formats,
    never the LFE. 2D: neighbours by azimuth; a gap of 180° or more (LCR
    or stereo behind the listener) cannot be represented with positive
    gains and is crossfaded by angle between its end speakers (`covers`
    says so). 3D: the convex hull of the speakers plus a virtual one below
    (nothing in FaderFrame's formats is below ear level; its share is left
    out), faces of four or more coplanar speakers (7.1.4's tops) split
    around a virtual centre speaker whose share goes to each at 1/√k, so
    overhead in 7.1.4 is the four tops alike; elevation 0…90°, down to
    −90° in a (custom) layout with speakers below ear level — which has
    no virtual speaker below and is panned over the whole sphere
    (`SurroundFormat::elevation_range`, `SurroundParam::range_in`: the
    knob and the lane offer what the format has; `Vbap::min_elevation`);
    ignored in 2D formats (the panner offers no elevation there). `Vbap::new`
    validates a speaker set (no speakers, duplicates within 1°, NaN, a
    hull that leaves the listener outside) and runs off the audio thread:
    each `MatrixMix` into a bed makes its destination's geometry when the
    node is built; `gains` then allocates nothing and never yields NaN.
  * **Spread** (both panners) blends a point's gains towards every main
    speaker alike, power for power (`g' = √((1 − s)g² + s/n)`): the same
    signal on more speakers — a wider, coherent image (off-centre
    listeners hear comb filtering, a fold to fewer speakers adds it in
    phase). **Diffuse** (`SurroundPan::diffuse`, the panner's Diffuse
    button) instead leaves the point `√(1 − s)` of its gains and adds a
    diffuse field: the source's channels summed, `√(s/n)` on each main
    speaker through a decorrelation filter of its own
    (`surround::diffuse_gain`, `nodes::diffuse::Diffuser`: four Schroeder
    all-pass sections, feedback 0.5, delays 1.7–9.3 ms offset per
    speaker) — the same power as the coherent spread, flat in level, no
    delay as a whole; copies uncorrelated with each other (they add in
    power when folded), transients smeared over the filters' tails. The
    filters are made with the graph, only for tracks whose spread is
    diffuse (`build::diffuse_tracks`; a parameter edit that changes that
    set rebuilds; strips' and following sends' node keys carry it);
    objects are always coherent.
  * **Stereo sources** (`SurroundPan::stereo`): **Image** (default) — the
    pair placed as a whole, its channels `width` either side (room:
    `x ∓ width`; VBAP: `azimuth ± 30° × width`, so width 1 at azimuth 0 is
    the format's L and R exactly; turned to the back the image is rotated,
    its left on the listener's right-back); **Balance** — not placed: left
    on the format's L, right on its R, at the strip's balance (`Track::pan`
    and its lane; unity at centre, no spread, no crossfeed; the strip shows
    its balance knob); **Dual** (VBAP) — each channel at its own azimuth
    (`left`, `right`, shared elevation); where they meet they add.
  * **LFE**: a separate send (`lfe_db`, shared by a source's channels),
    never reached by a position and never part of the panners'
    normalisation. Nothing filters it: what is sent reaches the LFE
    channel full range (bass management acts only on the way to the
    speakers, see *Listening*; binaural listening low-passes it at 120 Hz
    for the ears only).
  A bed into its own format passes straight; into another bed each of its
  speakers is placed where it stands (7.1.4 into 5.1, 5.1 opened up into
  7.1.4), LFE to LFE; into stereo or mono it folds by the project's
  downmix (below). `surround::matrix` computes all of these without
  allocating.
* **Downmix** (`Project::downmix`, `Command::SetDownmix`, Preferences →
  Project; parameter slots, so a change is heard at once, ramped): with
  coefficients, `Lo = fronts on the left + centre·C + surround·(left
  surrounds) + height·(left tops) + lfe·LFE`, `Ro` alike, mono
  `(Lo + Ro) / 2` (as FaderFrame folds any stereo to mono). New projects
  use −3 dB (1/√2) for the centre, surrounds and tops and leave the LFE out
  — the centre and surround coefficients of the common ITU-R BS.775 Lo/Ro
  downmix (FaderFrame claims no conformance beyond them; BS.775 has no
  tops). Projects saved before the setting fold **by place** (each
  speaker where the room panner puts it: 5.1 → `L + 0.707·C + Ls`, the
  surrounds at unity) as they always did — except into mono, where the
  old fold summed every speaker at unity (up to +14 dB) and now takes
  `(Lo + Ro) / 2`. Nothing normalises: a full-scale 5.1 whose channels all
  carry one signal peaks at 1 + √2 (+7.7 dB) in the −3 dB fold. The
  downmix applies wherever a bed becomes stereo or mono: strips and sends
  into narrower layouts, `FoldDown` and `ListenOut` before the device, the
  mono check, stereo renders of a bed master.
* **The strip.** Into or out of a bed the `ChannelStrip` mixes through that
  matrix instead of its pan law (each source/destination pair ramped; the
  matrix is remade when the panner's slots, the balance or the downmix
  move — `SURROUND_VALUES` slots per strip, `SurroundPan::values`). The
  values are automatable (`AutomationTarget::Surround(SurroundParam)`: x,
  y, z, spread, width, LFE, azimuth, elevation, the dual pair's azimuths;
  azimuths wrap round ±180°; the snapshot's `TrackAutomation::surround`,
  read every `AUTOMATION_STEP` like pan, echoed by `StripEcho` when
  rendered ahead, so movement through speaker regions is sample-for-sample
  the same at any block size that is a multiple of 32); a panned track lists
  those its panner and mode use (`SurroundParam::applies`) in place of Pan,
  the session writes them from `SetTrackSurround` in Touch/Latch/Write,
  MIDI learn sets one value. Node keys tell layouts of equal width apart.
* **Sends** into or out of a bed mix through the same matrix
  (`nodes::strip::MatrixMix`, shared with the strip): a mono or stereo
  track's pre-FX or pre-fader send into a bed follows the track's panner
  and its automation (as console sends follow the main pan); any other
  send between layouts where one is a bed places by the speakers (a 5.1
  post-fader send into a stereo reverb folds the bed). Elsewhere the
  graph's channel rules still apply.
* **Monitoring.** A bed wider than the device's outputs is folded down to
  the largest format it can play (`FoldDown` before the device output,
  `surround::fold_into`; the graph is rebuilt when the device's output count
  changes), so a 7.1.4 master on a stereo interface is heard whole. The
  engine carries up to 16 channels; meters cover each of a bed's channels
  (`TrackMeter::channels`, `MeterDisplay::shown`).
* **Delivery.** Rendering a surround master "as the master is" writes every
  channel as `WAVE_FORMAT_EXTENSIBLE` with the bed's channel mask
  (`write_wav_mask`, `WavWriter::create_with_mask`; `read_wav` reads masks
  back); stereo folds it down.
* **Views.** The mixer strip of a track panned into a bed shows the room
  from above in place of the pan knob (room: drag the puck, wheel for
  left/right; VBAP: the circle of directions, a drag across or the wheel
  turns it, a dual pair together; double-click or a click on the readout
  opens the panner; balanced into the bed: its balance knob), a bed's own
  strip shows its format, meters widen for every channel and a folded
  hardware output says so. The Surround Panner (`faderframe-view-surround`,
  `ViewKind::Surround`, View → Surround Panner) shows the selected track's
  room with its speakers lit by what they play; a toolbar picks the panner
  (Room/VBAP; the first switch to VBAP starts from the room position's
  direction) and a stereo source's way in (Image/Balance/Dual); VBAP draws
  the circle (speakers at their azimuths, tops nearer the middle by the
  cosine of their elevation) and drags round the listener (dual: either
  channel); the knobs show what applies (azimuth, the dual pair, elevation
  in formats with tops, height, spread, width, LFE) and a click on a
  knob's value types it (`L30`, `R110`, `-45`, `off`); every move is one
  gesture. Dev actions `surround-panner:<track>=<room|vbap>`,
  `surround-stereo:<track>=<image|balance|dual>`,
  `downmix:<standard|by-place>`; `surround:` takes the values in
  `SurroundParam::ALL` order (x/y/z/spread/width/lfe/azimuth/elevation/…).
* **Tests.** `faderframe-core` `vbap::tests` (speaker-aligned positions,
  reference values, symmetry, continuity, 5.1 vs 7.1, gaps, 3D and
  elevation, invalid layouts and values) and `surround::placing` (downmix
  coefficients, mono, headroom, stereo modes, LFE, persistence),
  `engine/tests/spatial.rs` (output samples in 5.1/7.1/7.1.4, LFE, stereo
  modes, azimuth and elevation automation, live downmix, the headphone
  switch), `engine/tests/surround.rs`, `spatial_panning_does_not_allocate`,
  `session/tests/spatial.rs`, the surround view's and the mixer's tests,
  `binaural` `each_speaker_reaches_the_ears_through_its_own_direction`.
  `faderframe-bench --bed 5.1|7.1|7.1.4 --panner room|vbap [--pan
  mono|balance|dual] [--pan-automation] [--headphones]` measures them.
  Custom layouts: `surround::custom` (identity, files, validation,
  places, folds, 16 channels), `a_custom_layout_is_a_bed_like_the_others`,
  `speaker_layouts_are_kept_edited_and_offered`; bass management:
  `nodes::bass::tests` (crossover flatness, routing, LFE band limit,
  crossfade), `bass_management_moves_the_mains_lows_to_the_sub`,
  `bass_management_is_listening_only`.

### Object-based masters

Objects come after beds and build on them. A mono or stereo track panned
into the master's bed can be an **object** (`Track::object`,
`Command::SetTrackObject`; `Project::is_object` = marked, panned into the
master's bed and feeding the master directly):

* **Monitoring is the delivery.** An object skips the master's inserts and
  fader: its strip (panned as before, without an LFE send — objects have
  none) joins the master's output in `nodes::ObjectRenderer`, after the
  master strip, as a renderer adds objects to the bed. The renderer meters
  what is heard as the master's (the master strip is then metered
  elsewhere). Projects without objects build exactly as before.
* **Delivery** (`session::adm`; Render → "ADM BWF · Dolby Atmos master" or
  "· ITU-R BS.2076"): `adm::plan` takes the master's format as far as one of
  the profile's eight beds (2.0 … 7.1.2) holds it — channels no bed has (a
  7.1.4's top front and rear, a quad's rear pair) become objects fixed at
  their speakers — plus every object track (a stereo one as two objects
  width apart). One render plays the master (the bed) and every object
  track (post fader, unpanned) on outputs of their own; the plugins' delay
  is taken off the front; the audio streams into a BW64 file
  (`faderframe_audio_files::bw64`: `chna` before the audio, `axml` after
  it, RF64 with `ds64` past 4 GiB) as 24-bit PCM at 48 or 96 kHz. Object
  metadata comes from the panner and its automation: blocks start wherever
  the place has moved by more than `adm::MOVE`, checked every `adm::STEP`
  frames, each taking the place at the middle of its step.
* **`faderframe-adm`** (pure; `roxmltree` for reading) writes the ADM
  document and the `chna` chunk in two profiles: Dolby's Atmos master ADM
  profile (v1.1, as MediaArea's conformance checker states it — the
  programme `Atmos_Master`, `ACO_1001…` contents with `dialogue` 2, the
  bed object `AO_1001`, objects `AO_100b`…`AO_1080`, custom IDs from
  `1001`, Dolby's bed channels `RoomCentricLeft`/`RC_L`… at their
  Cartesian places in one timeless block, object blocks with `rtime`,
  `duration`, Cartesian positions, equal width/depth/height, a gain and
  `jumpPosition` 1 over 0.005208 s, stream formats naming channel and
  pack, at most 118 objects and 128 channels) and ITU-R BS.2076 as the EBU
  ADM renderer reads it (stream formats naming only their channel, BS.2051
  speaker labels, the LFE marked by a 120 Hz low-pass). Dolby masters also
  carry Dolby's metadata chunk (`faderframe_adm::dbmd`; EBU Tech 3285 s6
  container, Atmos segments 9 and 10 as Dolby's BSD parser and MediaInfo
  read them: FaderFrame as the creating tool, warp mode not indicated,
  automatic trims for the nine speaker configurations, each track's
  binaural render mode — the LFE off). Checked from outside: MediaInfo
  (pymediainfo) reports no conformance findings and names the file a
  "Dolby Atmos Master", Dolby's `dbmd_atmos_parse` reads the chunk, the
  EBU renderer (`ear-render`) places the ITU flavour's objects as the
  panner did.
* **Import** (File → Import ADM BWF Master…, `Action::ImportAdm`): the
  reader takes Dolby's beds and other ADM files' BS.2051 labels and the
  common definitions of 5.1, Cartesian objects exactly and polar ones mapped
  into the room (azimuth through the speakers' angles, elevation up to 30°
  as height). A job splits the file into a bed file in our channel order
  and one file per object and brings them in through the importer
  (resampled to the project's rate); then, in one undo step, the master
  takes a format that holds them (with heights when an object rises), the
  bed becomes a track of its layout and each object a mono object track
  with its first place, its gain as the fader, and lanes for X, Y, Z and
  spread that follow its blocks (jumps over their interpolation).
* **Views.** The track menus and the Surround Panner have an Object toggle
  for tracks that can be one; objects' pucks are square; the panner shows
  no LFE knob for them and their automation has no LFE lane.
* **Headphone render modes.** The master (for its bed) and object tracks
  carry Dolby's binaural render mode (`Track::binaural`: Off, Near, Mid,
  Far or not set; `Command::SetTrackBinaural`; the track menu's "Headphone
  Render (Dolby)" while the master is a bed). It is delivery metadata only:
  `dbmd` writes it per bed channel (the LFE always Off) and per object.

### IAMF masters

Render → "IAMF · Opus / FLAC / LPCM" (`RenderChannels::Iamf(Codec)`,
`session::iamf`): the master — mono, stereo or a bed — as an IA Sequence
of the Alliance for Open Media's Immersive Audio Model and Formats, in an
MP4 file (`.mp4`, one track with sample entry `iamf`/`iacb`, brands
`iamf`/`iso6`, edit list for the trims, `roll` group for Opus, `stco`) or
standalone (`.iamf`, Temporal Delimiters included). `faderframe-iamf`
writes inside IAMF v1.0.0-errata's Simple profile so every decoder reads
it: one channel-based Audio Element of one layer (Mono, Stereo, 5.1,
5.1.2, 5.1.4, 7.1, 7.1.2, 7.1.4 — LCR and 5.0 go into 5.1, quad and 7.0
into 7.1, the missing speakers silent, x.1.2's top middles as the top
front pair; `iamf::plan`), its channels as mono and coupled substreams in
§ 3.6.2.3's order, coded as LPCM (`ipcm`, 1024-sample frames), FLAC
(`fLaC`, `flacenc`, independent stereo, 1024 — libiamf crashes on longer
coupled FLAC frames) or Opus (libopus via the `opus` crate, 48 kHz, 960,
pre-skip trimmed at the start, roll −4), the last frame padded and
trimmed; one Mix Presentation (one sub-mix, mix gains at their 0 dB
defaults, a bed binaural on headphones) with the loudness measured on
Stereo — IAMF's down-mix as the reference decoder renders it
(`stereo_downmix`: L + 0.707 C + 0.707 surrounds + top fronts + 0.707 top
backs) — and on its own layout (`faderframe_analysis::integrated_weighted`,
BS.1770 weights), integrated, sample and true peak. A loudness target of
the render's finish is reached on that stereo down-mix. Checked from
outside: AOM's libiamf (`iamfdec`) decodes every codec in both containers
to 7.1.4 and stereo, FFmpeg's IAMF demuxer reads the stream groups and
decodes every substream (LPCM/FLAC exact), and the stereo loudness written
equals FFmpeg's `ebur128` of libiamf's stereo render. The patents
necessary for IAMF are licensed royalty free by AOM's members
(`AOM-PATENT-LICENSE.txt`, shipped with every package). MPEG-H is out: no
open encoder, a paid specification and per-unit patent royalties.

### Listening: headphones and the mono check

What the master does on its way to the interface is listening, never part
of a render (renders build their own engine, which listens on speakers;
`exports_never_carry_what_the_listener_hears` holds stereo and bed
exports bit-identical with headphones and the mono check on):
the master's hardware output always passes `nodes::ListenOut` (other
tracks with hardware outputs keep the plain fold-down). Its node key leaves
listening out, so a rebuild keeps the running node; a change of listening
(`EngineController::set_binaural`/`set_head`/`set_headphone_correction`
then `relisten`) makes the new `nodes::Listening` (a renderer for the
master's layout, or none) on the control thread and hands it over through
`nodes::ListenLink` (two mailboxes); the node crossfades to it over 30 ms
(constant power, `SWITCH_SECONDS`) and hands the old one back to be
dropped on the control thread — switching speakers ⇄ headphones never
rebuilds or clicks. Headphones play on the device's first two outputs
(a 5.1 device's others are silent). A head that cannot be read is refused
by the session and listening stays as it was; a renderer that cannot be
made leaves the speakers. Binaural renders set their listening before
their engine's first graph is installed, so they are built with it (and
its latency taken off the front). The Performance view's graph latency is
the node's as last built.

* **Headphones** (Audio → Listen, or Listen in the master strip's menu;
  `Session::set_headphones`, `Preferences::headphones`): the master —
  a bed, stereo or mono — rendered binaurally by `faderframe-binaural`.
  Each speaker's feed is convolved with the impulse responses measured at
  the ears of a `Head` from its direction: fifteen BS.2051 directions
  (`binaural::direction` finds each speaker's by its azimuth and
  elevation in the speaker table — the same VBAP uses; quad's rears sit at
  ±135°, 5.x surrounds at ±110°; a custom layout's speaker between them
  is heard through a blend of the measured responses around it,
  `binaural::weights`: VBAP over the baked directions, the spectra added —
  an approximation of a measurement from there; the baked heads have no
  direction below ear level, so a speaker below it is heard at ear level
  on headphones) baked by `scripts/binaural_hrirs.py` at
  44.1/48/96 kHz (other rates resampled from the nearest) for twenty SADIE
  II subjects (University of York, Apache-2.0: the KU100 — the default —
  and KEMAR dummy heads and the listeners H3–H20, `BUILTIN_HEADS`,
  `data/sadie-*.ffhr`), all at unity at 1 kHz from the front so they
  compare by ear. **Your own ears**: `Head::from_sofa` reads a SOFA file
  (AES69 SimpleFreeFieldHRIR/GeneralFIR, e.g. from Mesh2HRTF or a lab;
  `hdf5-reader`, pure Rust) — the nearest measurement for each direction,
  `Data.Delay` in front, prepared as the bake script does (SADIE's own
  KU100 file reproduces the baked head to 1e-5); a set whose measurements
  lie far from the speakers gets a notice. Heads are chosen by id
  (`Session::set_head`: `ku100`, `kemar`, `sadie-h3`…, `sofa:<path>`;
  `Preferences::headphone_head`). **Headphone correction**
  (`Session::set_headphone_correction`, `binaural::Correction`,
  `Preferences::headphone_correction`): EqualizerAPO/AutoEq text (Preamp,
  PK/LSC/HSC/LS/HS/LP/HP/NO/BP/AP filters as RBJ biquads, `BW Oct`;
  `GraphicEQ:` curves as minimum-phase FIRs from the real cepstrum) or an
  impulse response (one channel or one per ear, resampled), applied per
  ear after the room in the same block — no added latency.
  The convolution is uniformly partitioned (blocks of 64 frames, 128 above
  50 kHz; one forward FFT per speaker, one inverse per ear), so it adds
  one block of latency to what is heard (1.3 ms), reported by the node;
  it is monitoring only, so nothing in the mix is delayed for it. The LFE reaches both ears
  low-passed at 120 Hz and +10 dB. Rooms: Near is the dry measurement, Mid
  and Far add an eight-line feedback delay network fed in mono (about 8
  and 3 dB under the direct sound). Render → "Headphones (binaural · room)"
  writes the same render to a stereo file through the head listened with
  (`RenderSettings::head`), without the headphone correction (it belongs
  to the listener's headphones, not to the mix).
* **The mono check** (the MONO button in the master strip's record slot,
  Audio → Listen → Mono Check, `Session::set_mono_check`): a parameter slot
  (`SlotRegistry::monitor_mono`), no rebuild; what is heard is folded to
  stereo and summed to (L + R) / 2 on both speakers, after the binaural
  render when both are on. Per session, not saved.
* **Bass management** (Audio → Listen → Bass Management, settings in
  Preferences → Audio, `Preferences::bass_management`;
  `Session::set_bass_management`, `Action::SetBassManagement`;
  `faderframe_core::surround::BassManagement`, `nodes::bass`): a property
  of the room, so per machine, never in the project, a render or the
  meters. With it on, on the speakers only (not on headphones), after the
  mono check: every main speaker is high-passed at the crossover (40–200
  Hz, default 80) and the bass taken from them, summed, goes to the
  layout's LFE output at "Bass to sub" (default −10 dB: for a subwoofer
  calibrated for the LFE's +10 dB in-band gain, so the redirected bass
  plays at the mains' level); the LFE channel itself is only low-passed
  (default 120 Hz). Both crossovers are Linkwitz–Riley 24 dB/octave (two
  Butterworth sections in f64): a main's high and its low add to its own
  level at every frequency at 0 dB redirect, each −6 dB at the crossover.
  Layouts without an LFE channel are left alone
  (`BassManagement::applies_to`, `Session::bass_management_applies`).
  Parameter slots (`SlotRegistry::bass_management`): changes are heard at
  once, switching on or off crossfades over 20 ms, nothing allocates.
  `recreate_engine` carries it (and headphones, head, correction, the
  mono check) over to a new engine. Dev action
  `bass-management:<on|off>[@<Hz>]`.

### Ambisonics and head-tracked listening

Besides speaker beds, a track, bus, aux or the master can carry an
ambisonic sound field: `ChannelLayout::Ambisonic(AmbiOrder)`, first to
third order (4, 9 or 16 channels — the engine's 16 at most), AmbiX
throughout (ACN channel order, SN3D normalisation, the AmbiX frame: `x`
front, `y` left, `z` up; directions as everywhere else, azimuth positive
to the left). The project file writes the order (`{"ambisonic": 3}`);
nothing else changed in the format. `faderframe_core::ambisonics` holds
the mathematics, pure and allocation-free once made:

* **Harmonics** (`harmonics`, `encode`): the real spherical harmonics up
  to third order in closed form from the direction's unit vector. Held by
  tests to their definition in angles, to SN3D (each order's channels
  have unit power in every direction) and to orthogonality over the
  sphere.
* **Rotation** (`Rotator`, `Orientation`): an orientation is a unit
  quaternion; yaw, pitch and roll compose as `Rz(yaw)·Ry(−pitch)·Rx(roll)`
  (yaw left, pitch up, roll towards the right shoulder). A field's
  rotation matrix is block-diagonal per order and is found by sampling:
  the harmonics of 24 fixed probe directions, turned, times the
  pseudo-inverse of the unturned ones (worked out once in f64 when the
  `Rotator` is made). It is exact for any turn and consistent with
  `harmonics` by construction — no sign conventions to get wrong — and
  making one costs 24 evaluations and about 2000 multiplications, no
  allocation. Tests: a turned plane wave is the plane wave from the
  turned direction (every order, arbitrary turns), turns compose, power
  is kept, nothing leaks between orders.
* **Decoding** (`Decoder::allrad`): AllRAD (Zotter and Frank, JAES 2012)
  — the field sampled, max-rE weighted (`max_re`: `Pₙ(rE)` with `rE` the
  largest root of `P_{N+1}`), at a 1200-point Fibonacci grid of virtual
  speakers, each panned onto the real ones by their VBAP geometry,
  normalised so a plane wave plays at unit power on average over the
  sphere. Below ear level a layout without speakers there plays at ear
  level (VBAP clamps the elevation), so nothing coming from below is
  lost. A 2D layout takes each virtual speaker by its azimuth. Into mono
  a field is its W (the omnidirectional pressure); into discrete channels
  one to one; between fields the shared orders pass and the rest is
  silent. `Decoder::sampling` is the geometry-free fallback `matrix` uses
  where no decoder was made. Near-field compensation is not done
  (speakers have no distance here).
* **Spread and distance**: a source's spread tapers the higher orders by
  `(1 − s)ⁿ` (at full spread only W is left: every direction alike),
  scaled to keep the power a max-rE decode plays (`spread_gains`). The
  distance (`SurroundPan::distance`, `SurroundParam::Distance`, 0…20, 1
  the reference) lowers the level as `1/d` beyond the reference and,
  inside it, opens the source up towards W (spread `1 − d` at unity) —
  at the listener a source is everywhere (`distance_effect`).
* **FuMa** (`FUMA_TO_AMBIX`): first-order B-format `W X Y Z` (W at
  −3 dB) to AmbiX.

**Panning and routing.** A mono or stereo track feeding a field
(`Project::ambisonic_panned`; `Project::spatially_panned` answers for
beds and fields alike) is encoded by the surround panner's values — the
direction, never the room's place: azimuth, elevation (over the whole
sphere, `SurroundParam::range_into`), spread, a stereo image's width
(`azimuth ± 30° × width`), balance (left at 30°, right at −30°, by the
strip's balance) or dual pan (each channel at its own azimuth), and the
distance; there is no LFE send. `SurroundParam::applies_into(source,
dest, pan)` says what applies, so the automation lanes, MIDI learn and
the panner's knobs are those of the surround panner. `surround::matrix`
computes every case through the strip's and the sends' `MatrixMix`
(made off the audio thread with the destination's AllRAD decoder,
`Mixing::decoder`): a bed into a field encodes each speaker at its
direction (the LFE is left out of the field), a field into a bed or
stereo decodes, a field into a field passes its shared orders. A
pre-FX or pre-fader send of a mono or stereo track into a field follows
the track's panner as into a bed. Node keys tell a field from a bed or
discrete channels of the same width. Hosted plugins on a field see its
channels one to one (an IEM or SPARTA plugin on a third-order track gets
16 channels in ACN order).

**Monitoring and delivery.** A field that fits the device's outputs
passes to them as it is (for an outboard decoder); one that does not —
and the master on any device it does not fit — is decoded to the largest
built-in format the outputs hold (`fold_into`), stereo on a stereo
interface. Rendering an ambisonic master "as the master is" writes its
channels as AmbiX (plain WAV, no speaker mask: they are not speakers);
Stereo decodes it (AllRAD onto L/R at ±30°: a source behind crossfades
between them by angle), Mono writes W, Headphones renders it binaurally.
UHJ stereo is not written. IAMF refuses an ambisonic master (its
scene-based audio elements are not written yet); ADM writes no HOA.
A/B loudness matching weighs a field by its W.

**Import.** File → Import Ambisonic Audio (AmbiX)… brings each file onto
a new track of its order as it is; File → Import First-Order B-Format
(FuMa)… converts on the import thread (`session::ambix::fuma_to_ambix`,
in place in the media folder, waveforms made again). A file whose
channels are no ambisonic order is refused with a notice
(`Action::ImportAmbisonic`, `Session::import_ambisonic`).

**The Ambisonic Rotator** (`builtin::AMBI_ROTATOR`,
`plugin_host::devices::ambi_rotate`): a field turned by yaw, pitch and
roll (degrees, automatable and modulatable; the generic editor), the
matrix from the `Rotator` remade when an angle moves and every entry
ramped across the block. The order comes from the track's channels;
other counts pass. No latency.

**Binaural rendering of fields and turning heads.** The binaural renderer
(`faderframe_binaural::Renderer`) works through its fifteen measured
directions: every input channel has a weight per direction — a speaker
at a measured direction is that direction alone, one between them a VBAP
blend over the measured ones, an ambisonic field its AllRAD decode onto
them — each direction's feed (its channels by their weights) is
convolved with its pair. A speaker bed sounds exactly as before (the
same arithmetic in another order; `each_speaker_reaches_the_ears_through_its_own_direction`
holds). Directions no channel feeds are skipped once their history is
silent, so a stereo master costs two convolutions as it did.

`Renderer::turn(head, smoothing)` turns the listener's head: the scene is
turned the other way relative to the head — each speaker's direction is
turned by the head's inverse and panned over the measured directions
(below ear level heard at ear level, as for custom speakers), a field is
rotated (`Rotator`) before its fixed decode — so only the weights
change, never the measured filters. The head glides to its target by
slerp with the smoothing time constant (25 ms by default), the weights
are made at most once a block and ramped linearly across it: no click,
no allocation. The room's feed (a bed's mains summed, a field's W) does
not turn. Objects are in the master's bed by then, so beds, objects and
fields all turn. Speakers never turn, renders never hear the head (their
engine has its own `ListenLink`).

**The head in the engine.** `ListenLink::head()` (`nodes::HeadShared`:
the quaternion's four components and the smoothing as atomics; a read
between two writes mixes two nearby orientations, which the renderer
normalises and glides over); `ListenOut` hands it to the renderer every
block. `EngineController::{set_head_orientation, head_orientation,
set_head_smoothing}`: no rebuild, no new renderer. `recreate_engine`
carries the head over (`Session::reapply_head`).

**Head tracking** (`session::headtrack`, `faderframe_control::headtrack`):
`HeadTrackingSettings` (on, UDP port — 8001 by default —, smoothing, the
axes a tracker counts the other way, angles in radians; in the
preferences, never the project) and the per-session head: set by hand
(`HeadOp::Turn`, `Straight`), or by a tracker's OSC messages relative to
where it pointed when recentred (`HeadOp::Recentre`; `wanted` =
reference⁻¹ · tracked). Messages read: `/SceneRotator/ypr f f f` and
`/ypr f f f` (yaw, pitch, roll — IEM's SceneRotator and SPARTA's
rotator/binaural decoders), `/SceneRotator/{yaw,pitch,roll}`,
`/{yaw,pitch,roll}` (one angle, the others kept),
`/SceneRotator/quaternions f f f f` and `/quaternion f f f f` (`w x y
z`, in the ambisonic frame), `/SceneRotator/{qw,qx,qy,qz}`; bundles are
opened. Inverting an axis negates its angle, or for quaternions the
component of that axis (z for yaw, y for pitch, x for roll), as
SceneRotator's flips do. Any app that can send one of these addresses —
phone apps that send their orientation over OSC usually let the address
be chosen — works as a tracker. The tracker listens on a socket of its
own; when an OSC control surface listens on the same port, the surface's
socket hands the packets over (`HeadState::feed_packet`; packets made
only of head-tracker messages do not make the tracker the surface's
peer), and a surface set up later takes the port from the tracker's
socket (`release_head_port_for`). `tick_head` reads the socket and tells
the engine only when the head moved. UI: Audio → Listen → Head Tracking
(the toggle `app.toggle-head-tracking`, a row in `toggles`), Recentre
Head, Straighten Head, Head Tracking Settings… (`head_tracking.rs`: port,
smoothing, inverts, radians, a live status line).

**UI.** The Channel Format menus offer AmbiX1–3 (`Session::format_choices`).
The Surround Panner and the mixer strip draw a track panned into a field
on the circle of directions with no speakers (`room::Room::dest`,
`by_direction_into`, `stereo_mode_into`, `format_place_into`): no
Room/VBAP or Diffuse buttons, dual pan always on offer, the Distance
knob (0…1 on its first half, then logarithmic to 20); inside the
reference distance the puck sits nearer the middle. A field's own strip
reads its order and FIELD; the inspector says "into AmbiX n".

**Tests.** `faderframe-core` `ambisonics::tests` (harmonics, SN3D,
orthogonality, orientations, slerp, rotation identities, spread power,
AllRAD placement and power round the horizon, stereo/mono decodes, the
fallback, FuMa) and `surround::ambisonic` (the matrix: encoding by
direction, stereo modes, distance, a 7.1.4 bed through a third-order
field and back, fields between orders, mono, discrete, what applies,
files); `faderframe-binaural` `turning_the_head_turns_the_scene`,
`a_field_is_heard_where_it_comes_from_and_turns_with_the_head` (turning
the head by an angle equals turning the field the other way, sample for
sample), `the_head_glides_to_where_it_is_turned`; `faderframe-control`
`headtrack::tests`; the rotator's `devices::ambi_rotate::tests`;
`engine/tests/ambisonics.rs` (encoding through the engine, beds encoded,
a field decoded into 7.1.4, a field master on a stereo device, azimuth
and distance lanes, a pre-fader send following the panner, binaural
listening turning with the head for a bed and a field while speakers do
not); `ambisonics_and_head_tracking_do_not_allocate` (encoding with
automated directions and distance, the rotator automated, decoding,
binaural with the head turned every block, a field master on
headphones); `session/tests/ambisonics.rs` (the format menu and files,
panning lanes and undo, a field bus into a bed, the head by hand, a
tracker on its own port with recentring, quaternions and an inverted
axis, a tracker sharing an OSC surface's port, AmbiX and FuMa imports, an
ambisonic master rendered as AmbiX and decoded to stereo); the surround
view's `a_track_into_a_field_is_turned_round_the_listener`.

### Folders and folder buses

A folder (`TrackKind::Folder`, `Track::folder` = the folder a track is
in) holds tracks: the editors show them under it (`Project::folder_order`),
it opens and closes (`WorkspaceSet::closed_folders`), its mute and solo
reach what it holds (`folder_chain` in `slots::write_params` and
`solo_audible`), and it has no audio. A **folder bus** is a folder that is
a bus: `TrackKind::Bus` with `Track::folder_bus` (written only when set;
older files and builds see plain folders and buses as before). Being a
`Bus`, it is one everywhere a bus is — its strip, devices and the
console's bus amplifier, sends, lanes, meters, cue path
(`build::has_cue_path`), render-ahead (`build::ahead_sets`), stems, the
mixer's full strip — and `Track::is_folder()` makes it a folder wherever
folders are (`folder_chain`, `folder_order`, `SetTrackFolder`'s target,
the session's folder actions, the editors' fold triangles).

**Routing rule.** `OutputRouting::Master` is *the default output*:
`Project::output_target` resolves it, for a track with audio (audio,
instrument, bus, aux, folder bus — not MIDI tracks, VCAs or plain
folders), to the nearest folder bus the track is in
(`Project::folder_bus_of`, plain folders between passed by), else to the
master. So what a folder bus holds plays through it once, nested folder
buses chain, moving a track in or out reroutes it, and an explicit
routing stays where it is: `Track { track }` to another bus, a hardware
output, none — and the master *by name* (`Track { track: master }`, the
output menu's "Master" inside a folder bus) passes the folder bus by.
Everything downstream reads `output_target` (graph, PDC, solo, cue
paths, render-ahead, `path_to_master`), so no other code knows folder
buses. A loop through one is refused: `SetTrackFolder` and `SetFolderBus`
check `Project::routing_loops` after the change, `SetTrackOutput`'s
default resolves before `would_cycle`. Track renders (freeze, bounce,
merge, clip effects, samples: `render::track_render_project`) route the
track to the master by name, past its folder bus.

**Commands and actions.** `Command::SetFolderBus { track, on, output }`
(`Impact::Graph`; kind, flag and output swapped back by its inverse; fader,
pan, devices and sends kept while it is plain);
`Session::set_folder_bus` (`Action::SetFolderBus`; `Action::SumFolder`,
which used to add a separate "<folder> Bus" and reroute the tracks to it,
now makes the folder itself the bus) also gives it its destination's
channel format (a bed in a surround project: its tracks pan into the
format they panned into before) and, with a console set, the console's
bus amplifier — one undo step. `SetTrackFolder` stays `Impact::Params`
(folder mutes): the controller rebuilds when the folder buses' tracks
change (`build::ParamShape::folder_routes`). The cue mixes keep hearing a
new folder bus's tracks by their own sends ("Copy Main" moves them onto
the bus by the console rule).

**Editors.** The mixer shows a folder bus as a full bus strip tagged
FOLDER BUS (F-BUS when narrow) with a fold triangle in its header
(`Hit::Fold`); strips dropped on its middle go into it; what it holds
names it as its output ("→ <folder>") and offers it first in the output
menu. The arranger draws its header as a folder's with a BUS badge
("Folder Bus · n tracks"), and its menu's "Folder Is a Bus" (checked)
switches it. Dev action `folder-bus:<folder>=<on|off>` (and `pan:<track>=<v>`).
Tests: `engine/tests/folder_buses.rs` (once, no doubling; pan, mute,
solo, device, send, lane; nesting; explicit routings; PDC; moved while
playing; loops; a bed folder bus), `a_folder_bus_renders_ahead_as_it_plays`,
`a_folder_bus_in_a_cue_mix_is_solo_safe_and_mute_safe`,
`folder_buses_and_sends_placing_their_source_do_not_allocate`,
`project::tests::folder_buses_take_their_tracks_default_outputs`,
`session/tests/folders.rs` (on and off, undo, Sum Folder, save and open,
the console, loops, stems and track renders), the mixer's
`a_folder_bus_is_a_full_strip_that_folds`, the arranger's
`a_folder_becomes_a_bus_from_its_menu_and_says_so`. Grouping edits across a
folder's tracks is the folder's Group switch (next section).

### Folder groups (edit and record groups)

A folder's **Group** switch (`Track::folder_group`, folders and folder
buses; written only when set, older files open with it off;
`Command::SetFolderGroup` — `Impact::None`, labels "Group Folder" /
"Ungroup Folder"; `Action::SetFolderGroup`, `Session::set_folder_group`)
makes the tracks with clips it holds one for recording and editing (Studio
One's folder group; the dry and wet guitar recorded on two tracks of issue
#5). It is an implicit edit group beside the mix groups of
`session::groups`; it links no faders, mutes or solos (a folder's mute
and solo already reach what it holds) and never changes the track
selection, so the multi-selection followers of `group_edits` do not start
moving members' faders together. All of it is in `session::folder_groups`.

* **Members** — `Project::edit_group(track)`: the *outermost* grouped
  folder among the folders a track is in (a folder: itself too);
  `edit_group_members` = its audio, instrument and MIDI tracks at every
  level (folders, buses, auxes and VCAs inside are skipped);
  `grouped_with(track)` = the other members. A grouped folder inside a
  grouped folder is part of the outer group; its own switch counts again
  once the outer one is off. Every track is in one group at most, so an
  expansion happens once and never travels from group to group.
* **Record arm** — `Session::folder_group_arms` in `Session::edit` (after
  `group_edits`): every `SetTrackRecordArm` in the command, batches too
  (Shift+R's Toggle Arm Selected, Arm All, a surface, MIDI learn), adds the
  same arm for the other members not already set — one command, one undo
  step.
* **Corresponding clips** — `Session::grouped_clips`: clips on two members
  share their place when they overlap by at least half of the shorter one
  (`shares_place`: the same take, a piece of a longer clip — not a
  neighbour touching it in a crossfade); a clip's corresponding clips are
  the closure of that relation across the members (the pieces of a clip
  cut up on one track only all go with the whole clip on the other). So
  the set is the same whichever of its clips is clicked, expanding twice
  changes nothing, and moves keep the members aligned. Inside a gesture
  the clips are measured where the gesture found them (`gesture_base`), and
  the first event's expansion is kept for the whole drag
  (`Session::grouped_targets`); launcher clips are never expanded.
* **Selection** — `Action::SelectClips` goes through
  `select_clips_grouped` (Replace/Add take the corresponding clips, Toggle
  switches them with the clicked clip; the anchor stays the clicked clip);
  switching a group on extends the current selection. A **range** on a
  member covers every member: `Session::edit_tracks` (now public) adds the
  grouped tracks, and every range operation (separate, clear, trim to
  selection, insert silence, copy/cut/duplicate/repeat of a range, Tab
  stops) and the arranger's range painting use it.
* **Edits** — one place for edits naming their clips:
  `Session::grouped_action` at the top of `dispatch` extends
  `MoveClips`, `TrimClips` (and stretch), `ClipGain`, `SetClipsGain`,
  `SetFade`, `SlipClips`, `SetClipsMuted`, and turns a grouped `SpotClip`
  into a `MoveClips` of all of them by as far; `ShuffleClip` shuffles the
  others on their own tracks by as far (`shuffle_grouped`, one history
  step) when the clip stays on its track. Edits of the selection read
  `grouped_selection()` (delete, nudge, split at the playhead) or
  `selected_arranger_clips()` (copy, cut, duplicate, repeat), so a
  selection made before the switch was on still works on the whole place.
  A split at the playhead also cuts every member's clip the playhead
  crosses on a member a selected clip is cut on (`grouped_cuts`); the
  pieces sharing a selected piece's place are selected. Every one stays
  one undo step. Paste follows the clipboard's lanes (a copy of a grouped
  place holds a lane per member, so it lands on the members again); a
  single lane is never multiplied. Operations that do not apply to a clip
  skip it as before (fades and gain of MIDI clips); per-clip analyses
  (quantize, warp, transients, pitch) are not expanded. A frozen member
  refuses the group's clip edits as it refuses its own (`frozen_target`
  sees the whole batch), so the members never drift apart.

**Editors.** The arranger's folder header has a **G** button where a
track's record button is (`HeaderPart::Group`, lit in the folder's
colour; the folder bus badge moved right of it), its menu "Group Its
Tracks" (checked), the tooltip names the outer group when the folder is
part of one; the mixer's folder strip has a G under its solo
(`FolderLayout::group`) and a folder bus's full strip has it in the
record slot (`Hit::FolderGroup`); the Inspector's Track section shows a
"Grouped" toggle for folders and folder buses (`Field::FolderGroup`;
"with ‘<outer>’" when inside a grouped folder). All three are toggles in
the accessibility trees. Dev action `folder-group:<folder>=<on|off>`.
Tests: `project::tests::grouped_folders_edit_their_tracks_together`,
`session/tests/folder_groups.rs` (arm and disarm, batches; selection and
toggling; a range on all members; split, drag move, trim, gain, fades,
slip, delete, cut/paste, duplicate as one undo step each; different
boundaries and crossfades; nesting; incompatible tracks; a frozen member;
save and reopen; Spot and Shuffle), `folder_groups::tests` (the place
rule), the arranger's
`a_folder_header_groups_its_tracks`, the mixer's
`a_folder_strip_has_a_group_switch`, the Inspector's
`a_folder_shows_its_group_switch`.

### Cue mixes (the performers' headphones)

A cue mix is a headphone mix on its own device outputs, apart from the
main mix — a console's foldback sends, Cubase's studio sends and Control
Room, Pro Tools' cue sends. The model (`faderframe_project::cue`):

* `Project::cue_mixes: Vec<CueMix>` (at most `MAX_CUE_MIXES`, 8): name,
  colour (what the mixer marks it with), `layout` (stereo or mono),
  `first_channel` (its outputs), master `level_db` and `mute`, `tap`
  (`SendTap::PreFader`, the default, or `PostFader`; never before the
  inserts), `click` and `click_db`, `talkback` (hears the talkback
  microphone, default on). Cue mixes are not tracks — they never show in
  the arranger, routing menus, stems, solo or anything that walks
  `Project::tracks`.
* `Track::cue_sends: Vec<CueSend>` — level, pan, on — one per cue mix on
  every track that `feeds_cues` (audio, instrument, bus, aux; not the
  master, MIDI tracks, VCAs, folders). Kept on the track, so they come and
  go with it (removal and undo, duplicates). New fields with serde
  defaults, written only when not empty: older files load with none, the
  file version did not change; `Project::repair` drops sends to cue mixes
  that are gone.
* Commands: `AddCueMix` (with every track's send; inverse `RemoveCueMix`,
  which takes the sends along), `SetCueMix` (name, colour, outputs, tap:
  `Impact::Graph`), `SetCueLevel`, `SetCueMute`, `SetCueClick`,
  `SetCueTalkback`, `SetCueSend` (a whole send or none),
  `SetCueSendLevel`/`Pan`/`On` (made when missing; their inverse is
  `SetCueSend`) — all `Impact::Params`; level and pan drags coalesce
  (`CoalesceKey::{CueLevel, CueClick, CueSendLevel, CueSendPan}`).
* A track added or restored joins every cue mix there is
  (`Project::join_cue_mixes` in `AddTrack`'s and `RestoreTrack`'s
  `apply`): its sends as it had them, plus one as the main mix has it to
  every cue mix made since — undoing a removal never brings back a
  silent track, whatever cue mixes came and went meanwhile.

**Solo-safe and mute-safe, buses included.** A cue send taps its
track's strip at the cue taps (`ChannelStrip` outputs 2 and 3, only while
the project has cue mixes): the signal after the inserts with its
polarity, before the fader (2, the chain layout, unpanned) or the
*post-fader image* (3: output 0 — fader, VCAs, modulation, panning, the
destination layout — without the mute; into a stereo cue mix the
never-muted *unpanned* twin instead, so the cue send's pan places the
source: see *Send pan*), never muted: the track's mute
and the solos of the main mix (`solo_mute`, folder and VCA mutes) do not
reach a performer's headphones, unlike an ordinary pre-fader send (strip
output 1 is post-mute). The strip's node key says whether it has the cue
taps (and whether it is quiet), so a cue mix added or the last one removed
while playing makes every strip anew: carried over by `adopt_state_from`,
a strip made without them left them unwritten (the cue mix without the
track) and wrote its unpanned output (a send placing its source) on what
is now cue tap 2 — the return silent in the main mix too
(`cue_mixes_added_and_removed_while_playing_sound_as_built_with_them`).
A pre-FX send keeps its track on the audio thread (`ahead_eligible`), so
a bus's cue path takes it from the track's input there, also when it is
added while the track renders ahead
(`a_pre_fx_send_added_to_a_track_rendered_ahead_builds_and_plays`). Every bus and return (`build::has_cue_path`) has
a **cue path** beside its main path: a second sum of its sources *as the
cue hears them* — the post-fader images of the tracks feeding it, the cue
paths of sub-buses, and every send into it taken from the never-muted
taps by a second, quiet `SendNode` on the same slots — through **cue
shadows** of its devices (a second instance of each slot, hosted under
`PluginInstanceId::cue_shadow()`, bit 62; `PluginHost::sync_parameters`
pushes the slot's state and values to both, `retain_project` keeps a
shadow while its bus has a cue path, its dirt and failures count as the
slot's; `PluginCx::shadow` makes the nodes and salts every node key with
`key_track`; outboard gear is passed by — it plays once, in the main
mix; containers and sidechains as on the main path) into a strip of its
own (`add_strip` with `StripKind::Cue`: `ChannelStrip::solo_safe()` —
mute, solo and VCA mutes never reach it, the fader, VCAs and polarity do
— quiet, outputs 0 post-fader image / 1 pre / 2 cue pre). The bus's cue
sends tap that strip (pre: 2, post: 0), and a parent bus's cue sum takes
its output 0. So the main mix is untouched (its sum, devices and strip
are the same nodes as before), and the cue mix keeps sounding while the
engineer solos, mutes and rides bus faders: the drum bus and the reverb
return in the headphones carry what their sources bring them, through the
bus compressor and the reverb, whatever the main mix does. The price is
a second instance of every bus device while cue mixes exist (live
engines only).

**The console rule.** A channel is in a cue mix once, where it reaches
the master. A copy of the main mix (`Project::main_mix_send`, the
session's `main_mix_send` from the *shown* values — automated and live
faders, pans and mutes via `main_mix_send_with`) switches a channel on
when its output goes straight into the master and it is not muted, at
the level it reaches the master with (fader and VCAs; post-fader: 0 dB),
its pan (a dual-panned image's middle): a bus or a return into the
master is on; a track that reaches the master through a bus is off (its
level set, there for more of it in the headphones) — it is sent by the
bus, never doubled, as a console's foldback would. New Cue Mix and Copy
Main Mix use the shown values (`Session::shown_*`), not the static
faders.

**Engine** (`build::build_graph` pass 3, live engines only —
`PluginHost::realtime()`; a render's engine builds none, so exports,
stems, bounces, freezes, clip effects and the album never contain them):
per cue mix a sum (`Role::CueInput`), its own `ChannelStrip` (keyed by
`CueMix::channel()`, the cue id as a `TrackId` — ids share one allocator —
with `SlotRegistry::strip` slots for level and mute, its talkback dim as
`with_trim`, and meters like a track's), a `Role::CueOut` sum after it
(the talkback joins there) and a `DeviceOutputSink` on its outputs; the
buses' cue paths (`cue_bus_order`: feeders first, `build_cue_chain`,
`cue_tap`/`cue_prefx_tap` say where a channel's never-muted signal is);
per channel and cue mix a `SendNode::cue` (slots `SlotRegistry::cue_send
(track, cue)`: gain = level, pan, on), built whether the track has a
send or not (none: silent), so a send's level, pan and switch are
parameters. Cue outputs never pass the master's `ListenOut` (headphone
rendering, the mono check, bass management). They are heard outputs:
`compile()` holds them back to the graph's output latency with the master
(plugin delay compensation as for any output), and with low-latency
monitoring a track heard live sends to its cue mixes (and into its bus's
cue sum) over early edges (`connect_audio_early`), which `clear_the_way`
carries through the sums — performers hear themselves at once, the
playback in their headphones still aligned. The session sizes the device
for the cue outputs and the talkback input (`wanted_channels`,
`check_channels` → `UiRequest::ReopenAudio`).

**Render-ahead.** Buses render ahead with cue mixes as without
(`ahead_sets` is unchanged by them). A strip rendered ahead carries its
cue taps in the shallow tier; a bus whose chain is in the shallow tier
has its cue path there too (its sources are strips rendered ahead there,
so nothing crosses the other way); the cue sends made there sum per cue
mix into a `Role::CueAhead` node and come back to the audio thread
through a ring (`AheadPlan::cue_rings`, `cross`: an `AheadWriter` there,
an `AheadReader` here reporting the slowest source's latency, as a bus
strip's echo does) into the cue mix's sum; a sub-bus's cue path in the
shallow tier feeding a bus on the audio thread crosses the same way. The
cue output is bit-identical to the non-ahead path
(`buses_render_ahead_with_a_cue_mix_and_the_cue_sounds_the_same`).

**Automation and control.** `AutomationTarget::CueSend { cue, what:
CueSendParam::{Level, Pan, On} }` on the channel and `CueLevel { cue }`
on the master (listed by `automatable_parameters`, so the arranger's
lanes and the Automation view show them; `static_value`,
`shown_cue_send`, `shown_cue_level_db`; written in Touch/Latch/Write like
sends — `write_target` from `SetCueSendLevel`/`Pan`/`On`/`SetCueLevel`;
On is a switch: steps, Touch ends with the click). The snapshot carries
them as `TrackAutomation::cue_sends` (`CueSendLanes`) and the cue mix's
level as the volume lane of its channel; `SendNode::cue` and the cue
strip read them per `AUTOMATION_STEP`. MIDI learn: `MappingTarget::
Parameter` with those targets (the mixer's fader/pan/mute menus learn
the cue send in cue mode — `cue::cue_target` —, the cue menus offer
"MIDI Learn Level/Pan/On" and the cue master "MIDI Learn Level"), and
`MappingTarget::Talkback` (momentary: a note or high CC talks while
down). Control surfaces follow the mixer's cue mode (`Session::
mixer_cue`): `surface_volume_db`/`surface_pan`/`surface_mute` show the
shown cue mix's sends (the master fader its level), and a fader, pan pot
or mute button sets them (`surface_volume_command`,
`surface_cue_pan_command`, `surface_mute_command`) — Mackie, HUI and the
mapped profiles alike, through `show_on_surfaces`/`surface_input`.

**Talkback** (Control Room style): `session::cues::TalkbackSettings`
(`Preferences::talkback`: the device input, level, dim in dB) and the
button (`Action::Talkback(TalkOp::{Press, Release, Latch})`,
`Session::{talking, talkback_latched, set_talkback_settings}`); which cue
mixes hear it is the project's (`CueMix::talkback`). The engine
(`slots::Talkback`, `EngineController::set_talkback`: another input
rebuilds, the rest are parameters `SlotRegistry::cue_talk` → `talk` gain
and `dim`, written by `write_talkback`): a `DeviceInputTap` on the
microphone's input, per cue mix a plain `SendNode` at level × on × hears
over an early edge into the cue mix's `CueOut` sum (heard at once, never
delayed by PDC, never dimmed), while the cue mix's strip is dimmed by the
`dim` trim — all ramped, nothing allocates, never in the main mix or a
render. The mixer's cue bar has TALK (held: talking; Shift-click or
double-click: latched; right-click: MIDI learn, which cue mixes hear it);
the Cue Mixes window has the microphone's input, level and dim and a
Talkback check per cue mix.

**Send pan.** `SendNode` pans when given a pan slot holding a position: a
mono tap by the project's pan law, a stereo tap balanced, each
source/destination pair ramped (NaN: as tapped — the graph's channel
rule). Cue sends always pan; an ordinary send pans when
`AuxSend::pan` is `Some` (`Command::SetSendPan`, `Impact::Params`, slot
`SlotRegistry::send_pan`; carried by track presets) — older sends are
`None` and play as before. A *post-fader* send with a pan of its own into
a stereo destination places the track's **source** (`build::places_itself`):
it taps the strip's post-fader *unpanned* output (fader, VCAs, mute and
polarity as output 0 has them, the source in the track's layout:
`ChannelStrip::with_unpanned`, made only for tracks with such a send), not
the main mix's panned image — balancing that image again was the defect
behind "panning a send does nothing": a mono track panned hard right sent
hard left reached the return as silence, and sent to the centre still
only on the right. "As tapped" (`None`) still follows the main mix's image.
The strip's extra output comes after outputs 0, 1 and the cue taps (2, 3);
with cue mixes a never-muted twin (`with_cue_unpanned`) feeds the cue
paths' copies of such sends, and a bus's cue strip has one of its own
(`build::unpanned_ports`). Whether a send places its source is graph
shape: `SetSendPan` stays `Impact::Params`, and the controller rebuilds
when `None` ↔ `Some` changes the set (`build::ParamShape`, with the
diffuse spreads and the folder buses' tracks). **Cue sends after the
fader** place their source the same way (`build::cue_places_itself`: a
post-fader stereo cue mix, a mono or stereo channel): they tap the
never-muted unpanned twin (a track's strip; a bus's or return's cue
strip, whose one unpanned output serves sends and cue mixes), so the cue
send's pan places the source as it does before the fader — the main
mix's pan is the main mix's. Measured (`a_post_fader_cue_send_places_its_
source_by_its_own_pan`, DC 0.5, fader −6 dB): a mono track panned hard
right, its cue send hard left — before L 0, R 0 (the image balanced away),
now L 0.2506, R 0; a stereo track balanced hard right, its cue send
centred — before L 0, R 0.25, now 0.25/0.25; a bus balanced hard right,
its cue send hard left — before silence, now L 0.125, R 0. A mono cue
mix or a surround channel takes the image as before. Which strips get
the twin depends only on the cue mixes' tap and layout and the track's
layout (all `Impact::Graph` edits); renders build no cue mixes, so no
twin. Tests: `a_post_fader_send_with_its_own_pan_places_the_source`,
`a_send_given_its_own_pan_while_playing_places_the_source`,
`a_send_placing_its_source_reaches_a_returns_cue_path`,
`a_post_fader_cue_send_places_its_source_by_its_own_pan`,
`buses_render_ahead_with_a_cue_mix_and_the_cue_sounds_the_same` (pre- and
post-fader), `folder_buses_and_sends_placing_their_source_do_not_allocate`,
`cue_mixes_do_not_allocate` (a stereo post-fader cue mix too).

**The click** plays in every cue mix whose click is on, at its
`click_db` (relative to the metronome's volume) under its master level
and mute (`MetronomeShared::set_cues`, packed `CueClick` words written in
`EngineController::update_params`, read whole by the audio thread), as
late as the outputs like the main click; a count-in reaches them the same
way. `MetronomeShared::set_main` (`MetronomeSettings::main_outputs`,
Preferences → Metronome and the Cue Mixes window) takes it off the main
outputs, for a click in the headphones only. Tests:
`engine/tests/cue_mixes.rs` (levels, pans, fader/mute/solo independence,
post-fader, click and count-in, renders, PDC, low latency, send pan, a
bus and a return solo-safe and mute-safe through their devices, a bus in
a post-fader cue, buses rendered ahead with a cue mix bit-identical,
talkback, cue lanes), `cue_mixes_do_not_allocate` (a bus with an echo in
the cue mixes, talkback on and off), `project` `cue::tests` (the console
rule, restored tracks), `session/tests/cue_mixes.rs` (a copy from shown
values, a removed track back into every cue mix, Touch writing, MIDI
learn of a cue send and the talkback, a Mackie Control in cue mode,
talkback settings), the mixer's `in_a_cue_mix_the_strips_set_its_sends`,
`the_cue_bar_chooses_what_the_faders_set` and
`the_talk_button_talks_while_held_and_latches_with_shift`.

**Session and UI.** `session::cues` (`Action::Cue(CueOp::{New, Remove,
Rename, SetOutput, SetTap, CopyMainMix, Show, SetTalkback})`): New makes
a copy of the main mix on the next free outputs (`Project::
free_cue_outputs` past the master, hardware outputs, hardware insert
sends and other cue mixes — 3-4, 5-6 …) and shows it in the mixer; SetTap
moves every send by its track's fader the other way so the cue mix sounds
the same; tracks a command adds join every cue mix
(`cue_sends_for_new_tracks`, hooked into `Session::edit` like the
console's buses, and the project's `join_cue_mixes`). The mixer's cue
mode is presentation state (`Session::mixer_cue`, not saved): a bar over
the strips (while there are cue mixes) chooses Main Mix or a cue mix,
adds one, copies the main mix, switches pre/post, TALK (right-click a
cue mix: outputs, click, mute, talkback, MIDI learn of its level,
remove); in a cue mix every strip's fader, pan and mute set its send
there (`MixerView::{fader_command, pan_command, mute_command}`, shown
from `shown_cue_send`), framed and tagged in the cue mix's colour, the
fader caps too; solo is dark (the main mix's); strips without cue sends
are dimmed; the master section is the cue mix's own (level, mute, the
click's switch on the MONO place and level on the pan knob, its meters
and outputs). Audio → Cue Mixes: New Cue Mix, Cue Mixes… (a window with
every cue mix's name, outputs, taps, level, mute, click and click level,
talkback, Copy Main Mix, remove, the talkback microphone's input, level
and dim, and the main outputs' click), Copy Main Mix into the Shown Cue
Mix, Mixer Shows. Dev actions `cue-mix:<new|remove=<n>|rename=<n>=<name>|
output=<n>=<first output>[/mono]|pre=<n>|post=<n>|click=<n>=<on|off>[@<dB>]|
level=<n>=<dB>|mute=<n>=<on|off>|copy-main[=<n>]|main-click=<on|off>>`,
`cue-mode:<n|main>`, `cue-send:<track>@<n>=<dB|on|off>[/<pan>]` (cue
mixes counted from 1; pans as typed in the mixer, `L60`, `C`, `R40`),
`cue-mixes` (the window), `talkback:<on|off|latch|input=<n|none>|
level=<dB>|dim=<dB>>` (inputs counted from 1).

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

**Changing the loop while playing.** The arranger's loop drags (an edge,
or a new range in the loop band) are one gesture and one undo step with a
`Command::SetLoop` per pointer move. Its impact is `Impact::Transport`
(below `Params`): no timeline snapshot, no graph, no parameter pass — the
session only tells the disk loader the new loop start (its pages stay
resident) and `EngineController::send_loop` sends `SetLoopRange` /
`SetLoopEnabled` when the range *in samples* changed (`loop_in_samples`:
at least `MIN_LOOP_SECONDS`, recomputed with every timeline update, so a
tempo change moves it). Before, every move rebuilt the timeline snapshot
(and reset the launcher's pending follow actions). On the audio thread
the command is a plain field change at a block boundary — no allocation,
no lock, no discontinuity flag (notes keep sounding). Rendering ahead,
it is an amend at a frame beyond the anticipators (§5, Render ahead).
Behaviour: the loop wraps when the playhead reaches the end *in force*
while it is before it; a playhead inside the new range plays on to its
end, one before it plays into it, one past its end plays on (no jump
back); an empty or reversed range is no loop (the engine's
`LoopRange::new`), and the arranger never makes one: a dragged edge stops
a tick from the other, a range dragged across its anchor is normalised.
Stopped, a change applies at once. Tests: `engine/tests/ahead.rs` (above),
session `dragging_the_loop_while_playing_rebuilds_nothing` (real time,
dummy device: no snapshot, no graph, one undo step, the playhead never
steps back), arranger `both_loop_edges_resize_without_toggling_…`.
Verified with the dummy device and the offline renderer only; a real
interface with a small buffer is still to be listened to.

**Where Stop leaves the playhead, and where Play starts.** Three ways,
persisted in the preferences (`return_to_play_start`, `stop_to_start`):
it stays where playback stopped (the default; the edit toolbar's
Insertion Follows Playback) — where it was *heard*: the engine has
processed the output latency (the graph's and the device's) further and
stops there, so a user's Stop (`Session::stop_here`: the button, Space)
locates back to the heard position (`after_stop(Some(heard))`; stops the
engine makes on a frame — the post-roll, a show — end where they are
heard anyway; test `stop_stays_where_the_playhead_is_heard`), it returns to where playback started
(*Return to Start on Stop*: `EditorSettings::insertion_follows_playback`
off), or to the start of the project (`stop_to_start`, which wins). The
play start (`Session::play_started_at`) is where the playhead was when
Play was pressed from stop — before any pre-roll, so the next take
pre-rolls the same way (it used to be the pre-roll's start, which moved
every take a pre-roll earlier) — or the Play Start Marker; loop passes do
not move it; a selection made with Link Timeline puts the playhead at its
start, which is where playback starts. Set from the Stop button's
right-click menu (`recording::stop_menu`: the two returns, one at a
time, then its MIDI learn) and the Transport menu
(`app.return-to-play-start`, `app.stop-to-start`); the menu ticks follow
the session wherever the change came from (`recording::follow`). The
playhead's jump is shown at once (`show_position`) and the arranger
scrolls it into view (jumps counter).

**The Play Start Marker** (`Project::play_start: Option<PlayStart { at,
on }>`, saved with the project, `Command::SetPlayStart` — undoable,
coalesced, `Impact::None`) is a fixed anchor the user sets, apart from
the dynamic play start: while `on`, Play from stop (the button, Space,
MIDI learn, control surfaces: `Session::locate_play_start` before `play`)
starts there whatever the playhead and the edit selection
(`Session::play_start_position`), and so does Record from stop (its
record start; a punch range still decides the record window, the
pre-roll goes back from it); while playing, locates work as ever.
Starts that put the playhead somewhere themselves play from there: a
setlist song, an ADR cue's pre-roll (`Session::play_here` around its
dispatch), the shuttle at normal speed, the launcher, a restarted take. Return to Start on Stop then returns to it (that is
where playback started). A loop does not move it: playback starts at the
marker and wraps at the loop's end once it gets there. The ruler draws it
as a flag in the theme's playing green (hollow while off); its flag drags
in the loop band (`Hit::PlayStart`, `Drag::PlayStart`: one gesture, on or
off kept, snapped like the loop's edges — a loop edge under it still drags
from the edge's side of its line; a click on it changes nothing); the
ruler's right-click menu sets it where clicked (snapped), switches it and
removes it; the Transport menu sets it at the playhead
(`app.set-play-start`, `app.play-from-marker`, `app.remove-play-start`);
dev action `play-start:<quarters|here|toggle|off>`. Section rearrangement
(`faderframe_project::arrange`, `Arrangement::play_start`) moves it with
the material under it: later material moving up or back takes it along, a
moved section it is in takes it (a copied one does not: there is one), a
cut it is in leaves it where the cut was. Tests `session/tests/stop.rs`
(the marker, loop passes, the pre-roll), arranger
`the_ruler_sets_switches_and_removes_the_play_start_marker`,
`the_play_start_marker_drags_in_the_ruler_and_snaps`, arrange
`the_play_start_marker_moves_with_the_material_under_it`.

**Requested play and stop are held** (`Session::requested_playing`,
`requested_play`): Play from stop (and the user's Stop) set the session's
transport state at once and keep it until the engine's snapshot agrees
(or two seconds pass, like a held locate). A second transport press before
the engine applied the first — Record and Play together on a tape-style
surface, Play then Stop — sees the transport as asked: Play does not
restart the take from the pre-roll's start (it used to record the pre-roll
too), Stop stops instead of locating to zero. Show mode: Space and the play
button are the show's play and stop (`ShowOp::PlayStop`: a stop cues the
song's start again); the setlist view's own Space never reached it.

**Counting in before playback** (as Logic, Cubase and Studio One do). A
pre-roll cannot play before the project's start: recording from bar 1 with
a one-bar pre-roll used to clamp it away entirely — playback and the take
started at once, the user took the click's first bar for a count-in and
played a bar late. Now `Session::pre_roll` (`roll_back` is its start)
measures the pre-roll on the timeline carried back before the start (its
first tempo and metre: negative positions, bar 0 before bar 1, the
pre-roll's bar line too) and returns where playback starts (never before
0) and the frames it could not reach; `play` sends
`TransportCommand::PlayAfterCountIn(frames)` instead of Play. The
transport (`TransportState::count_in`, `counting_in()`, `started()`)
stands where it is — not playing: the graph runs as stopped, so live
inputs and monitoring keep working, nothing of the arrangement plays,
nothing is recorded, plugins see a stopped transport, MIDI clock and MTC
start with playback — while the frames count down
(`frames_until_wrap` ends a block on the count-in's last frame), then
plays from the next frame, inside the callback (not a jump). Play while
counting changes nothing (Record and Play pressed together), Stop and
TogglePlay cancel it (a jump), Scrub only locates. Rendering ahead it is a
new sequence like any start: the anticipators count the same frames in
and render the playback after it ahead during the count-in
(`ahead_plan`/`amend` treat counting as started). The play history keeps
the count-in in its runs (`Segment::count_in`, `Played::count_in`,
published by `HeardClock` — five words a segment — and read back as
`Heard::counting_in`); the click (`Click::render` takes negative
positions) plays the count-in's runs at `position − count_in` whatever the
metronome's mode, as late as the graph's outputs like every click, and a
count-in run turning into the playback it counted in keeps its clicks
sounding — so the last count-in beat and bar 1's downbeat are a beat
apart as heard, behind plugin latency too. A pre-roll partly before the
start counts in only what is missing: from beat 3 of bar 1 a one-bar
pre-roll (not on a bar) counts two beats in and plays the first two
beats; a launcher slot's count-in (`Launcher::count_in`) recorded from
the start counts in the same way.

**Count-in** (#23; until 0.31 it was a pre-roll with the click alone,
which did nothing without a pre-roll). `RecordSettings::{count_in,
count_in_length}`: recording from stop counts in its own length (bars,
beats or seconds, measured back on the timeline by `rolled_back`, the
place in the bar kept) before everything else — `play` adds those frames
to the `PlayAfterCountIn` (on top of what a pre-roll could not reach
before the project's start), so the transport stands at the pre-roll's
start (or the record start) while the click counts, then the pre-roll
plays the song. `count_in_click_only` (on by default): the metronome
keeps quiet after it — `MetronomeShared::count_in_only` (set while
counting in is on, `Session::apply_count_in_click`, again for every new
engine), and the processor's `CountedIn` (Playing from a count-in,
Stopped after it so the take's last clicks still on their way stay
quiet, back to No when playback starts without one) silences the clicks
of that playback, through loop wraps, whatever the metronome's mode.
Older preferences with the pre-roll's Count-in become a count-in as long,
without the pre-roll (`Preferences::record_settings`). Menus: Transport →
Count-in (on/off, lengths, Click Only During Count-in); Preferences →
Recording. Tests `the_click_can_keep_quiet_after_counting_in` (engine),
`a_count_in_counts_its_bars_then_records_from_the_playhead`,
`a_count_in_comes_before_the_pre_roll` (session),
`older_count_ins_become_count_ins_of_their_own` (ui prefs).
The session sees a count-in as playing (started): the tick sets
`TransportSnapshot::playing` from `started()`, so Stop, Abort (and Abort
and Restart, which counts in again), the play button and the requested
play hold work on it; `Session::transport()` carries the count-in as heard
(`counting_in`) with the playhead held at the start, and
`Session::count_in_position()` is where it is (the start less the frames
left). The transport display's main counter shows it before bar 1 (a
one-bar count-in in 4/4: 0.1.000 … 0.4.xxx, then 1.1.000; two bars from
−1.1; other units negative), the sub counter reads COUNT-IN; the
arranger's ruler shows a "Count-in 0.3" badge by the playhead; screen
readers hear "Counting in". Return to Start on Stop goes back to where
Play was pressed, before any count-in or pre-roll. Tests
`engine/tests/count_in.rs` (the clicks a beat apart into bar 1's, the
song from the frame after the count-in, behind latency; the heard clock;
Stop/Play while counting), `a_count_in_rendered_ahead_starts_on_the_same_frame`
(`engine/tests/ahead.rs`), `counting_in_does_not_allocate` and the
count-in in `rendering_ahead_does_not_allocate_on_the_audio_thread`,
transport `a_count_in_stands_then_plays_on_the_frame_after_it`, history
`a_count_in_counts_down_standing_then_plays`, session
`tests/count_in.rs` (from the start, partial, a count-in of its own,
before a pre-roll, Stop, Abort, Toggle, Abort and Restart, a launcher
slot).

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

### Articulation maps and drum maps

**Articulation (expression) maps**, as in Cubase
(`faderframe_project::articulation`): a map lists an instrument's
articulations — name, the symbol notes show, a group (up to four), and
whether it is an *attribute* (set on a note: that note only) or a
*direction* (set in the clip's articulation lane: from there on until the
next of its group) — and what switching to each sends: keyswitch notes, a
program change, controller values, a channel the notes go out on, and
changes to the notes (transpose, velocity and length in percent). One
articulation of each group is in effect at a time; before any direction a
group plays its first articulation (the default). What a state sends is
each group's articulation's output together, unless the map has a
`Combination` for exactly those articulations (Cubase's sound slots naming
several). Projects keep their maps (`Project::articulation_maps`,
`Command::SetArticulationMap`, refused for a map a track names); a track
names one (`Track::articulation_map`, `Command::SetTrackArticulationMap`;
instrument and MIDI tracks; both `Impact::Timeline`). A note carries its
attribute (`MidiNote::articulation` — keep `articulation: None` in new
`MidiNote` literals; copies, splits and moves carry it), a clip its
directions (`MidiClip::articulations`, sorted; splits and time-scaling
carry them).

*Playback.* The timeline snapshot works out every note's state — the
directions in effect where it starts along the track's clips in time order
(`Directions`: a direction carries from clip to clip; a launcher clip's are
its own), its attribute over its group's — applies the state's note
changes, and notes a switch wherever a note needs another state than the
note before it (`engine::articulation::SwitchBuilder` →
`Lane::articulation`: `Switch { at, state }` and per state the events,
built on the control side). The MIDI clip player sends a switch's
controllers, program and keyswitch note-ons one sample before its note
(`SWITCH_LEAD`) and the keyswitches' note-offs on the note's sample —
note-offs sort first there, so the note-on follows. Starting or jumping
mid-clip *chases*: the state playback from the start would have left the
instrument in (the last switch whose note-ons went out before the
position) goes first, after the controllers, its note-offs a sample later
(kept for the next block when a block is one frame long); a launched clip
gets its first note's state each time round. Everything the audio thread
reads is built ahead; it indexes and copies. MPE tracks leave maps out (the
member channels are the snapshot's).

*Session* (`session::maps`, `Action::Maps(MapOp)`): new maps from
templates (`ArticulationMap::TEMPLATES`: Strings, Brass) or empty, edits
replacing a map whole, removal taking it from the tracks in the same step,
use by a track, import of FaderFrame's own JSON maps and of Cubase
`.expressionmap` files (`maps::cubase`, read with `roxmltree`: slot
visuals → articulations, a sound slot's output messages and note changer
→ its articulation's output, slots naming several → combinations, a slot
naming none → an articulation of its own, the first of group 1 when it is
the first slot), user files in the data folder's `articulation-maps`
(`maps::user_dir`, `set_user_dir` for tests). `SetNoteArticulation` gives
notes an attribute — a direction chosen for notes goes into the lane where
the first of them starts — and `SetDirection`/`RemoveDirection` edit the
lane (a direction replaces the one of its group at its time). Track menus
(arranger, mixer) have an Articulation Map submenu
(`Session::articulation_map_choices`); notes' menus an Articulation
submenu (`articulation_choices`).

**Drum maps** (`faderframe_project::drum_map`): a track's drum sounds by the
key they are written on — name, place in the list (the map's order), a grid
for drawing on its row, and where it plays: an output note, a channel, an
external MIDI output of its own. `Project::drum_maps`,
`Command::{SetDrumMap, SetTrackDrumMap}` (`Impact::Graph`), `Track::drum_map`.
Notes stay written on the map's pitches; the snapshot plays each on its
sound's note and channel (before an articulation's changes), the track's
`MidiInputNode` maps live input and auditioning the same way
(`midi::NoteMap`, its fingerprint in the node key), and a MIDI track with an
external output gets one more output node per port the map's sounds name:
each takes only its sounds' notes by the note and channel they play on
(`midi::KeyFilter`), the track's own output everything else. Maps come
from the General MIDI key map (`DrumMap::gm`; names in
`faderframe_midi::gm::DRUMS`), from the track's Drum Sampler (`pads`: its
first note and its samples' names), or from user files (`drum-maps`). A
track with a drum map is a drum track: the piano roll's drum mode lists the
sounds by name in the map's order instead of a keyboard (only those played
with Fold to Used Keys; keys the map lacks below), and drawing on a sound
with a grid of its own keeps to it (start and length).

*Views.* The piano roll's lane menu has Articulations
(`PianoRollSettings::articulations`): a row per articulation, names in the
header column; directions as bars to the next of their group (the group's
default faint before any), attributes under their notes; a click in a
direction's row sets it on the grid (or removes the one there), in an
attribute's row gives it to the notes sounding there (or takes it back);
right-click: the articulations for the selected notes. Notes show their
articulation's symbol. The MIDI event list shows directions as rows of
their own (before the notes at their time; moved by position, changed
among their group from the row menu, deleted) and articulations by name in
the last column; notes' row menus have the Articulation submenu.

*The map editor* (`faderframe-view-maps`, `ViewKind::Maps` "Map Editor",
`ViewId::maps()`, a dock view: View → Map Editor, `app.show-maps`) edits a
map's contents row by row. An articulation map is a table of its
articulations — name, symbol, group, type (a click switches attribute ⇄
direction) and what each sends (keyswitches typed as "C0, D0@90", a program,
controller values "1=64, 68=127", the channel, transpose, velocity and length
in percent) — and under a line of its own its combinations, whose
articulations are chosen per group from a menu (a combination keeps at least
two). A drum map is a table of its sounds in the map's order: pitch (typed
or dragged; a sound already on that key swaps with it, `DrumMap::set_pitch`),
name, the note it plays, channel, port (the MIDI outputs), grid, and Learn.
A double-click types a cell, a vertical drag changes a number (shown at
once, committed on release, like the event list), a click opens a choice's
menu; rows move by their handle (previewed, committed on release) or
Alt+Up/Down, are added after the selected row (+, the toolbar, a row's menu;
new articulations take a fresh id within the map, new sounds the first free
key, `DrumMap::free_pitch`), duplicated and deleted. Every change is one
undo step and the whole map replaced (`MapOp::EditArticulationMap`,
`EditDrumMap`); an edit that removes articulations also takes them off the
notes and lanes of the clips of the tracks using the map in the same step
(`Session::articulations_removed`), so an id the map gives again never
reaches an old note. The toolbar chooses the map (the project's maps, New ▸
templates/General MIDI/the Drum Sampler's pads/empty, `new_map_choices`,
Delete), renames it, imports a file (FaderFrame's JSON or a Cubase
`.expressionmap`, for the track edited for), exports JSON and saves a user
map. What it shows is the session's (`session::maps::editor`:
`MapRef`/`MapKind`, `Session::{map_editor, map_editor_track, map_users,
has_map, map_name}`): the map it was opened on — `MapOp::OpenEditor` from the
track menus' "Edit Map…" and the piano roll's articulation lane menu,
`MapOp::EditTrackMap` (making an empty articulation map or the General MIDI
drum map first), any map made or imported — until the track selection
changes and a selected track has a map (it then follows the selection like
the Inspector); a map gone from the project falls back to the track's. Learn
(`MapOp::LearnDrumPitch { map, pitch }`, `CancelLearn`, Escape) waits for the
next note-on from a MIDI input (`Session::map_learn_event` in `tick_midi`;
the audition port never counts) and puts the sound on that key in one step
("Learn Drum Sound Pitch"). For the keyboard and screen readers the toolbar
is buttons, each row a table row read as a whole with its menu, and each
editable cell a control: names and lists typed with Enter, numbers spin
buttons the arrows step (an optional one steps down to unset), choices
menus (group and channel step too), the type and Learn switched with Enter;
the map shown changing is said. Dev action `edit-map:<articulation|drum>=<track>`.
Tests: view `faderframe-view-maps` (`tests.rs`: typed cells one step each,
drags committed on release, menus, handle and Alt+arrow moves, rows added
after the selection, combinations, drum sounds, Learn, toolbar requests,
every theme, the keyboard through `access::press_key`; `text::tests`),
session `tests/maps_editor.rs`, project
`a_sound_moved_to_a_taken_pitch_swaps_with_the_sound_there`, piano roll
`the_articulation_lane_sets_directions_and_attributes`.

Dev actions `articulation-map:<track>=<name|template|file|none>`,
`drum-map:<track>=<gm|pads|name|file|none>`, `piano-lane:articulations`.
Tests: project `articulation::tests`, `drum_map::tests`; session
`maps::cubase::tests`, `tests/maps.rs` (attributes and the lane, files and
removal in one step, pads, keyswitches before their notes and chased in a
running session); engine `articulation::tests`, `tests/maps.rs`
(keyswitches a sample ahead, chasing, drum notes/channels/ports, live
drums), `articulation_and_drum_maps_do_not_allocate`; piano roll
`a_drum_map_lists_its_sounds_by_name_and_draws_on_their_grids`,
`the_articulation_lane_sets_directions_and_attributes`; event list
`articulations_show_and_change_in_the_list`; arranger
`the_track_menu_chooses_articulation_and_drum_maps`.

### Microtonal tuning

A project has a tuning (`Project::tuning`; `None` = twelve-tone equal
temperament) and a reference pitch (`Project::reference_pitch`, A4 in Hz,
200–1000); a track may play a tuning of its own (`Track::tuning`, `None` =
the project's). `Command::SetTuning { track, tuning }` and
`SetReferencePitch` are `Impact::Timeline` (the reference pitch coalesces);
`SetTrackRetune` is `Impact::Graph`. Nothing is migrated: the fields are
left out of files while they are at their defaults.

* **Tunings** (`faderframe_midi::tuning`, pure; re-exported by
  `faderframe_project::tuning`). `ScalaScale` reads Scala `.scl` files
  (comment lines, a description line that may be empty, the number of
  notes, pitches in cents when they have a period, else ratios or whole
  numbers; text after a pitch ignored; errors name the line); `KeyboardMap`
  reads `.kbm` files (pattern size, 0 = linear; first and last key
  retuned; middle key = degree 0; reference key and frequency; formal
  octave degree; entries or `x` for unmapped keys, missing entries
  unmapped). `Tuning { name, scale: ScaleSource::{Equal, Scala}, mapping }`
  keeps the degrees themselves, never a path (projects stay portable);
  `Tuning::check` refuses a mapping whose reference key plays nothing.
  Without a mapping, degree 0 sits on middle C at its equal-tempered
  frequency at the reference pitch, the next keys play the next degrees
  (Scala's convention). A mapping's reference frequency is taken as given
  at A4 = 440 Hz and moves with the reference pitch, so 432 Hz lowers every
  tuning alike. Keys outside a mapping's range stay equal-tempered.
* **The key table** (`TuningTable`): per key a fractional MIDI note number
  in equal temperament at 440 Hz (69.5 = a quarter tone above A4), NaN for
  an unmapped key. One representation serves every destination: built-ins
  play its pitch, MTS encodes it directly, MPE and note expressions carry
  `pitch − key` (the *offset*), MIDI 2.0 per-note pitch too. `Copy`, 1 KiB,
  allocation-free; `fingerprint()` tells tables apart.
* **The engine** (`faderframe_engine::tuning`). The timeline snapshot holds
  a `Retuning` per retuned track (`Tunings::build`: the table, its
  fingerprint, the table as an MTS bulk dump and as real-time single note
  changes, the track's MPE zone — all made on the control thread) and a
  shared standard one (`TimelineSnapshot::{tuning, standard_tuning}`).
  Every node that takes notes has a `Retuner` whose `Method` was chosen
  when the graph was built (`method_for`; part of the node key):
  `Builtin` — the processor gets the table (`PluginProcessor::set_tuning`,
  default no-op; `SharedProcessor` keeps a table it could not hand over
  while another graph held the processor; `Modulated` forwards) whenever
  its fingerprint changes; `Mts` — a bulk dump at offset 0 before the first
  notes, the 128 keys as two real-time single note changes when the tuning
  changes; what the instance was told is the instance's (`TunedPlugin` in
  `ActivatedPlugin`, beside `MissedReleases`), so a rebuilt graph does not
  tell it again and a plugin switched to another method (or retuning Off)
  is told equal temperament once; `Expression` — a tuning note expression
  (the offset) after each note-on, the offset added to the note's own
  tuning expressions; `Mpe` — each member channel's bends carry the offset
  of its key: a bend before a note-on looks ahead for that note-on (bends
  sort before note-ons at one offset), a note-on with no bend gets one, a
  later bend uses the channel's key; the master channel is left alone.
  Unmapped keys' note-ons are dropped (MTS cannot silence a key: they go
  out at their own pitch there). The rewriting happens in the plugin
  node's merged event buffer (`PluginNode::with_retune`) with a scratch
  buffer made with the node: copies and arithmetic only
  (`retuning_does_not_allocate`).
* **Which method.** `Track::retune` (`Retune::{Auto, Mts, Expression, Mpe,
  Off}`; Automatic by default). Built-ins always read the table (they have
  no tuning of their own). Hosted plugins in Automatic: MPE on tracks with
  `Track::mpe`; else note expressions for plugins that take tuning per
  note — CLAP (its note dialect carries `CLAP_NOTE_EXPRESSION_TUNING`; it
  lists nothing) and VST3 instruments listing the tuning type — else MTS
  (VST3 without it, LV2, Audio Units). Off leaves hosted plugins in their
  own tuning. MIDI effects pass keys on; the instrument after them is
  retuned. A MIDI track playing an instrument track is retuned by the
  instrument track's tuning at the instrument.
* **Built-in instruments.** The synth starts each voice at the table's
  pitch (glide, key tracking, sub key tracking and the Key modulation
  source follow the tuned pitch; note expression tuning and bends add to
  it); the sampler chooses zones by key as before and plays them at the
  key's tuned pitch relative to their root (key tracking scales the
  distance; samples at their root sound at the reference pitch's root).
  Unmapped keys start nothing. The Drum Sampler is left alone: its pads
  are triggers, not pitches (a tuning would detune a kit by where its pads
  sit). The Guitar Station plays a guitar, not notes, and the Tuner keeps
  its own reference parameter.
* **External MIDI outputs.** `MidiOutputSink::with_retune` (`output_method`):
  MPE bends on MPE tracks, else tuning expressions after each note-on — a
  MIDI 2.0 port sends them as per-note pitch bends (`per_note_message`,
  ±48 semitones), MIDI 1.0 ports drop them. MIDI 1.0 devices get MTS from
  the session (`session::tuning::tick_tuning`, in `tick_midi`): a bulk dump
  to program 0 and the keyboard as real-time changes, straight out
  (`MidiOutputs::send_now`), when the table a MIDI track plays changes or
  its port connects (`MidiOutputs::is_ump` keeps MIDI 2.0 ports out).
* **Session and UI.** `session::tuning`: `Action::{LoadTuning,
  LoadKeyboardMapping, PromptTuning}` (Scala files read whatever their
  encoding; a loaded tuning is named after its files, "ptolemy (white)";
  a broken file is a notice naming the line), `UiRequest::ChooseTuning` →
  `faderframe-ui/src/tuning.rs` file choosers; menus from
  `Session::{tuning_choices, more_tuning_choices, retune_choices,
  reference_choices}` (Audio → Tuning for the project — `app.tuning`,
  `app.tuning-ref`, Reference Pitch → Other… —, a Tuning submenu with
  Retune Plugins in the arranger's and mixer's track menus for instrument
  and MIDI tracks). Freezing keeps a track's own tuning in its audio
  (track tuning and retune edits on a frozen track are refused); a
  self-freeze cache key holds the fingerprint of each closure track's
  table and its retune setting.
* **The piano roll.** `Session::piano_tuning(clip)` (scales other than
  twelve-tone equal temperament, at any reference pitch): the keyboard
  labels rows with the degree and its cents ("3 · 386¢"; the tonic's row
  always, the others with room), shades keys the mapping leaves out and
  marks degree 0 with a period line; notes carry the same label; "Scale
  keys only" folds to the keys the tuning plays (and those holding notes).
  The key and chord track stay twelve-tone: keys and chords name keys of
  the MIDI keyboard, not pitches, so a tuned clip's rows are not lit by
  them, and MIDI effects that follow them (arpeggiator, chord, scale)
  produce keys that the tuning then retunes.
* **MTS-ESP** (ODDSound's tuning sharing between a master and client
  plugins) is not used. Its client and master glue (`libMTSClient.cpp`,
  `libMTSMaster.cpp`) is 0BSD, but a master works only through ODDSound's
  `libMTS` dynamic library, a closed binary the user installs; one master
  per machine holds one global table (per-track tunings fit only its
  multi-channel tables), and its clients would take FaderFrame's per-note
  retuning on top (no API tells a host which plugins are clients). A
  dlopened master in a crate allowed `unsafe` (the UI crate) remains
  possible: register when `MTS_CanRegisterMaster`, send the project table
  with `MTS_SetNoteTunings`, filter unmapped keys, and set Off for the
  hosted plugins it reaches.
* **Tests.** `faderframe_midi::tuning::tests` (Scala and `.kbm` files
  written for the tests: ratios, cents, labels, comments, errors with
  lines; mappings with unmapped keys and a just-intonation reference; MTS
  frequency data against the specification's examples, bulk dumps with
  checksums, real-time changes); project `tuning::tests` (commands,
  coalescing, files); plugin-host
  `notes_play_the_tuning_and_unmapped_keys_stay_silent`,
  `a_sample_plays_at_the_pitch_the_tuning_gives_its_key`; engine
  `tuning::tests` and `tests/tuning.rs` (expressions, MTS once/changes/
  rebuild/back to 12-TET, MPE bends, the synth at 432 Hz, MIDI outputs),
  `retuning_does_not_allocate`; session `tests/tuning.rs` (files, undo,
  menus, piano-roll labels, MTS to a MIDI 1.0 port, per-note pitch on a
  MIDI 2.0 port); piano roll
  `a_tuned_track_labels_rows_with_degrees_and_folds_to_its_keys`.

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

**Offline renders are told** (issue #50). `ProcessConfig::offline` is set
by `build_graph` when the engine's `PluginHost` is not realtime — an
`OfflineRenderer` (exports, stems, freezes, bounces, clip effects, batch
processing, self-freeze caches) — and never in a live session, not even in
the graphs rendered ahead (those are "not realtime" only for FaderFrame's
own buffered built-ins, `configure_realtime`; they still play live). Each
format passes it on at activation: VST3 `kOffline` in `ProcessSetup` and
in every block's `ProcessData`, CLAP's `clap.render` `set(OFFLINE)` before
`activate` (a plugin that declines keeps its live behaviour), LV2's
`lv2:freeWheeling` port (and the worker: freewheeling, the audio thread
waits after `run` for the work `run` scheduled, so responses come as if
done synchronously — `worker::Responses::deliver`), the AU's
`kAudioUnitProperty_OfflineRender`, the sandbox's `Activate` request.
Plugins that keep background work going live (Aeolus's convolver on a
worker thread) finish it in the block offline; otherwise a render far
faster than real time outruns it. Tests: engine `tests/offline_mode.rs`,
`an_offline_render_sets_the_render_mode` (CLAP),
`an_offline_render_processes_in_offline_mode` (VST3),
`an_offline_render_freewheels_and_does_its_work_in_the_block` (LV2),
`an_offline_render_is_told_and_waits_for_every_block` (sandbox).

**Renders give plugins time to get ready** (PR #56's report: a NAM amp
plugin self-frozen silent for its first 2.3 s). A render hosts plugin
instances of its own and starts at once, faster than real time; a plugin
that prepares what its state names on threads of its own after
`load_state` (an amp model, samples, an impulse response) plays silence
or a stand-in until it is ready — and no format has a way to tell the
host when that is. So an `OfflineRenderer` hosting any third-party
instance (CLAP, VST3, LV2, AU, sandboxed or not;
`PluginHost::third_party_since` = when the newest was made) gives them
`offline::plugin_warm_up_ms` of real time before its first frame, as a
device would: blocks of silence with the transport stopped, paced from
the newest instance's making (the set-up's time counts), plugins polled
between blocks — their main-thread callbacks served, and a restart (a
latency known only once loaded) rebuilds the graph so the render
compensates it. The block count follows from the time, rate and block
alone, so renders of one project stay alike (a free-running plugin's
state moves as far each time); the capture plan's taps and probes are
reset afterwards (`Capture::reset`: Mix Intelligence would otherwise read
the warm-up as lost frames and quieter devices). Process-wide
(`set_plugin_warm_up`, 0 = none until a program sets it, at most
`MAX_PLUGIN_WARM_UP_MS` 30 s); the shell sets it at start-up from
`Preferences::plugin_warm_up_ms` (default `DEFAULT_PLUGIN_WARM_UP_MS`
2 s; Preferences → General → Renders), `Session::set_plugin_warm_up`
changes it (self-frozen chains with third-party plugins are made again:
their `ChainView` holds the warm-up). Built-ins load synchronously in
renders and never wait. Tests: engine `tests/offline_warm_up.rs`,
`capture::tests::a_reset_forgets_what_was_captured_and_missed`, session
`self_freeze::key::tests::the_warm_up_counts_where_a_third_party_plug_in_plays`.

**Favorites** (issue #15) are the user's, not a project's:
`faderframe_session::plugin_favorites` (pure; `PluginFavorites` with
`Favorite`s keyed by `FavoriteKey` = format + the format's id — never the
name —, remembered name/vendor, and `Category`s with stable `CategoryId`s,
a plugin in any number of them) changed only by
`Action::PluginFavorites(FavoriteOp)` (`Set`, `Assign`, `NewCategory`,
`RenameCategory`, `RemoveCategory`: no command, no undo step, no dirty
project, plugin states untouched; refused names are notices). The shell
loads them at start-up and saves them whenever `Session::favorites_revision`
moves (`faderframe-ui/src/favorites.rs`: `plugin-favorites.json` in the
configuration folder — the portable `Settings` folder in portable mode —,
written through a temporary file; an unreadable file is kept as
`.json.unreadable`). Each format of a plugin is a favorite of its own;
favorites whose plugin is not in the catalogue stay and show as missing
(`missing`, with `other_formats` naming the same plugin installed in
another format), rescans change nothing but the remembered names of
renamed plugins (`note_names`, after each catalogue change). The plugin
browser has a star per row and in the detail panel, category check boxes
and "New Category…", a FAVORITES sidebar section (all favorites, each
category — right-click renames or removes it), search over category names;
missing favorites are listed only there, dimmed, and cannot be inserted.
An empty mixer insert slot's menu has a Favorites submenu
(`PluginFavorites::menu`: by name, then a submenu per category; missing
ones disabled), and so has a container chain's "+" menu
(`ContainerView::favorites_menu`, first, with the chain's own filter: an
instrument only on an instrument track, no MIDI effects; a choice is
`Action::InsertIntoChain`). Tests: the module's,
`session/tests/plugin_favorites.rs`, the browser's `plugin_browser::tests`,
mixer `an_empty_insert_slot_offers_the_favorites_by_category`, devices
`a_chains_add_menu_offers_the_favorites_by_category`. Dev actions
`favorite:<plugin id>[=<category>]`, `plugin-browser-scope:<all|favorites|
<category or vendor>>`.

**Processors survive rebuilds.** Graph rebuilds (an edit that changes
routing, a track selected and so made live, a latency change elsewhere)
ask every slot for a processor again. External formats hand out their
active one (shared through a `TryCell`); built-ins do the same: a
`BuiltinInstance` keeps the processor it made with what it was made for
(`MadeFor`: process config, channels, live or not, buffer size, latency,
shape, activation, its samples and tap) and gives graphs `SharedProcessor`s
of it while that holds — the Guitar Station's circuits and workers, the
EQ's filters, the reverb's lines are not built anew on the UI thread at
each rebuild. A graph that adopts the running processor (same node) keeps
its state; one that gets it handed out again (its node changed: bypass,
latency) resets it when it first runs, as a new processor would be
(`rebuilds_share_the_processor_made_and_start_it_clean`). Only the
buffered built-ins (`Kind::buffered`: preamps, console buses, the Guitar
Station — reservoir workers and deadlines) are made anew for the live
graph rather than the render-ahead one (`realtime`, the sized block);
every other processor moves between them as it is, reset on the way, so
a MIDI effect ends the notes it had started (#27: an arpeggiator made
anew when its track moved to the live graph — opening an editor does that
— left them sounding in the instrument for ever).

**Blocks a plugin misses.** A shared processor busy in another graph (the
`try_lock` lost while two graphs overlap during a move), or a sandbox
helper still working on an earlier block, answers
`ProcessStatus::Missed`: silence, its events never taken. The plugin's
`MissedReleases` (`nodes::plugin`; one per instance, atomic bitsets
shared by its nodes in every graph) keeps what those events would have
ended — note-offs, pedals let go, all sound/notes off — and the plugin's
next block, in any graph, starts with them (missed note-ons stay
missed). CLAP and VST3 processors end a key's sounding note id before a
new note-on on the same key (plugins matching offs by id kept the first
voice), CLAP takes a velocity-0 note-on as an off. Tests
`engine/tests/note_offs.rs` (an arpeggiator moving between the graphs
with an editor open while playing; missed note-offs and pedals reaching
the instrument), `blocks_a_late_helper_never_gets_say_they_missed_it`
(sandbox), `a_note_started_again_ends_the_one_sounding_by_its_id` (CLAP,
VST3), `blocks_an_instrument_misses_do_not_allocate`.

**Switching what plays the notes.** Each plugin instance also has a
`NoteLedger` (`nodes::plugin`, atomic, shared by its nodes in every graph):
the keys held at its event input (with velocities), the keys its processor
was given, the keys its event output has sounding, whether the processor
got the last block, where the last block's output came from, and what fed
it. A MIDI effect bypassed (by hand: a new node; automated: the processor
keeps running, its output replaced) ends the notes it had sounding and
sounds the keys held at its input; played again, its processor first gets
note-offs for what it was given and note-ons for the keys held now, and
the notes that passed it end (#27: bypassing the arpeggiator while playing
left notes stuck). Instruments get the same catch-up after a bypass. A node
that takes notes is keyed by what feeds them (`PluginCx::upstream_of`: the
MIDI effects before it, the MIDI tracks playing its track and their
effects; bypass aside): removing, adding or moving one, or a MIDI track
going, makes a new node, which ends what the plugin was given on its first
block. Notes generated at offset 0 sort after these offs. Tests
`engine/tests/note_offs.rs` (bypassed by hand and by automation, removed,
a MIDI track taken away), the arpeggiator bypass lane in
`the_midi_effects_do_not_allocate`.

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
  linear phase). **Output**: character (`eq::character`: clean; Subtle, a
  transformer's low-end saturation; Warm, a tube's asymmetric curve driven
  below the top octave, its harmonics DC-blocked; Console, a line
  amplifier's biased soft limit over the whole band, mostly third harmonic
  with a touch of second; Tape, a +6 dB record emphasis over 3 kHz into a
  soft saturation and its exact inverse, so the treble saturates first)
  with a drive (id 15, the stage hit harder or softer at the same level,
  glided) and optional 2×/4× oversampling of the stage (id 16, the
  Saturator's halfband `dsp::oversample`; not automatable — its latency
  joins the EQ's, `latency()` sums spectral + FIR + oversampling and the
  dry path is delayed by all three), gain, pan (left/right or mid/side),
  phase invert, auto gain (pink-noise loudness), a gain scale, gain-Q
  interaction, a dry/wet mix (id 14, parallel EQ: the whole processed
  signal against the input delayed by the latency, glided) and a 10 ms
  bypass ramp against the same delayed input. The three later globals
  default to neutral values (mix 100 %, drive 100 %, oversampling off), so
  older states sound as they did. Frequency, gain, Q and a cut's fractional
  slope glide (15 ms); a band whose structure changes fades out and back
  in; a band not heard yet starts where it is set. **EQ Match**
  (`eq::matching`):
  the reference-minus-input spectrum smoothed to a third of an octave and
  centred, bands placed where most is left (bells at the extremes, shelves
  at the ends) and refined by pattern search.
  The editor (`view-devices::eq`): a display with the analyser (pre, post
  and an external spectrum — the sidechain or any other EQ's output — with
  resolution, speed, range, tilt, freeze and a peak hold line over the
  output; collisions glow red), every
  band's curve (a dynamic band's reach shaded, spectral movement per
  frequency) and the overall response per part of the signal (stereo,
  left, right, mid, side); band controls under the band in focus, in a
  room of their own between the frequency scale and the bottom bar (PR
  #61's proposal: a band cut deep or a dynamic range pulled down is never
  under them; `Layout::panel`, as tall as the tallest controls any band
  shows — with a band's dynamics settings open, every band's room grows,
  so the graph keeps its size whichever band is selected — and held while
  a drag goes on; the controls hang under the scale, a band's dynamics
  settings opening below their first row so ">>" stays under the pointer;
  in an editor too low for it the room gives way first, the graph keeping
  `MIN_GRAPH_H` 160 px, and the controls rise from the bottom bar over its
  foot; with nothing selected the room says how to begin) (the gain knob
  carries the dynamic range ring; ">>" opens threshold,
  sidechain, attack, release, trigger and spectral settings); values next
  to a band to drag, scroll or type ("1k", "A4", "C#2+13", "2x"); multi-
  selection, rectangle selection, copy and paste; EQ Sketch (a drawn curve
  turned into bands, `eq::sketch`); Spectrum Grab (rest on the spectrum,
  drag a peak into a bell); the piano display; a zoomable frequency scale;
  A/B; the sidechain source picked in the editor; the instance list (every
  EQ in the project with its spectrum and collisions; reference, open,
  match) and EQ Match (reference: the sidechain, another EQ, or the input
  recorded earlier). The background menu flattens every band's gain (and
  range) or inverts them in one step, and has Solo While Dragging (a band
  dragged is heard alone until it is let go — an editor setting,
  `key::SOLO_DRAG`); the output panel holds gain, pan, mix and character
  drive knobs beside the switches and the gain scale; the character menu
  lists the five characters, the oversampling and drive steps, and the
  button shows the oversampling ("Tape ×2"). Factory presets
  (`presets::equalisers`, 42) are all grouped — Vocals, Drums, Bass,
  Guitars, Keys & Synths, Mix & Master, Surgical, Creative
  (`presets::eq_group`) — the first fifteen keeping their names, settings
  and order from the first version.
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
hold; a velocity pattern (`pattern_weight`: accent on the first of every
few, alternate, ramps, random, by the accent's amount —
`shaped_velocity`) and chance (steps left out at random, the pattern
going on). `order_index`/`sequence`/`pattern_weight` are shared with its
editor. The *Chord* makes each note a chord: up to five intervals, the
key's triad or seventh (the key track's key where the note is, or its
own), or the chord track's chord voiced round the note; then `voiced`
(inversions carry the lowest note up an octave or the highest down,
octaves move the whole chord); strummed up or down, added notes softer;
notes of a strum not started when the key is let go never start. The *Scale* keeps notes in a key (the key track's or its own):
nearest (ties down), up, down or blocked, then by scale degrees and
octaves; note-offs, poly pressure and note expressions follow each note
to where it went. The *Note Echo* repeats each note (a division of the
tempo or milliseconds apart, up to 16, each the feedback's share as loud
until too soft to play, optionally moving by semitones or, In Key, by
scale degrees of the key track's key where the note was played —
`echo_key`), each repeat the Length's share of the played note (its end
is scheduled when the note ends), with or without the note itself. Their editors
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

* **Compressor** (`devices::compressor`): seven styles — Clean (feed
  forward, RMS), Punch (peak), Opto (feedback, programme-dependent
  release), Vintage (feedback, fast, colour), Bus (glue), Mastering (long
  RMS window, slow, wide knee), Pumping (peak, quick, a hold of its own
  before the release) —; the feedback styles' slope is compensated so the
  ratio knob reads true; threshold, ratio up to ∞, soft knee, attack,
  hold (`id::HOLD`: the reduction waits before releasing), release with
  auto release, range, upward compression (`id::UPWARD`: the curve
  mirrored below the threshold, `lift`, capped in dB; lifted at the
  release rate, let down at the attack rate), auto makeup, lookahead
  (latency), RMS or peak detection, stereo link in left/right or mid/side
  (`id::CHANNELS`: detected and compressed as M and S), colour
  (saturation, oversampled 2×/4× on request — `id::OVERSAMPLE`, a fixed
  parameter: the oversampler's round trip is latency and the dry path of
  the mix is delayed to match), mix, delta listening (`id::DELTA`: wet −
  dry), an external sidechain (on by default once one is routed) with
  high/low cuts and listen. Ids are positions; 19–23 came with the second
  revision and are neutral at their defaults (projects saved before sound
  the same). Published values: reduction (≥ 0), its peak, in/out peaks,
  the detector level, `value::GAIN` = the net gain (negative down,
  positive lifted). Editor: the transfer curve (`curve`: knee, range,
  lift) with the level moving on it (drag to set the threshold), the
  reduction readout and a history. Presets: the 14 at the top of the menu
  plus groups Vocals, Drums, Bass, Bus, Master, Creative (levelled on the
  preset tests' material: `the_grouped_dynamics_presets_are_levelled…`).
* **76 Compressor** (`devices::fet76`, id `faderframe.76-compressor`): a
  FET limiting amplifier in the manner of the 1176, written from how the
  hardware behaves (its owner's manual), not from another plugin. Feedback
  gain control solved without a sample's delay (Newton on the loop's
  equation: stable at the 20 µs attack and any ratio), the Input driving a
  fixed threshold, soft knee, attack 800 µs (1) … 20 µs (7) with an Off
  position that disconnects the compression (the amplifier still colours),
  release 1.1 s … 50 ms with a programme-dependent slow stage. The ratio
  buttons go in in any combination, as pressing them together does
  (`network`): their taps mix (the ratio lands between theirs; higher
  buttons set the threshold higher) and every extra button moves the bias —
  threshold down, attack lagging, release quicker, more FET distortion;
  with all four the ratio is between 12:1 and 20:1 and louder input comes
  out quieter; none in: no compression. Meter Off is the power switch.
  Colour: the FET's even harmonics (with the reduction and the signal
  across it), the class-A stage and transformer, 2× oversampled; stereo
  link, sidechain high-pass, mix. Its editor is a panel of its own
  (`view-devices::fet76`): knurled knobs, lit push-buttons (Shift-click
  presses several), a backlit VU with standard ballistics (gain reduction,
  or the output at +4/+8; its bias swings with several buttons in, all four
  pin it), input and output peak meters either side.
* **2A Leveler** (`devices::leveler`, id `faderframe.2a-leveler`): an
  optical leveling amplifier in the LA-2A's manner, written from UREI
  C-10953 / Teletronix S-1014 and the published T4 behaviour. Three
  netlists cut at the Gain and Peak Reduction wipers (`circuits::leveler`;
  the grids draw no current, so the wiper voltage is exact and a knob costs
  no solving): the input (T1 A-10 with its iron, the R6/R7/cell attenuator,
  the cell a resistor set every sample), the make-up amplifier at 2× (two
  12AX7, the 12BH7 totem pole, 20 dB of feedback, the A-24) and the
  sidechain at the host rate (V3, R37 ∥ C12, the 6AQ5 — every drawing has
  a 6AQ5, not a 6BA6 — into the 2 nF panel). The T4 (`leveler::cell`): the
  EL panel's light is the charge the field moves under the Alfrey–Taylor
  law with two relaxations and a phosphor afterglow; the CdS cell's
  carriers recombine linearly and through Rose's n^(4/3) spread (γ ≈ 0.75),
  with three trap levels giving the history-dependent release (a 0.3 s
  burst: half released in 44 ms, under 0.5 dB in 0.52 s; an 8 s burst:
  63 ms and 4.7 s). SW3: Compress is the feedback (about 3:1), Limit
  takes the sidechain above R7 (flattening). The 3-66 manual's 15 dB pad
  ahead of the input, −18 dBFS = +4 dBm; gain 41.3 dB (spec 40 ± 1), THD
  0.19 % at +10 dBm. Modern: Mix, an external key, a stereo link (one
  side runs while the pair is exactly mono, the other wakes bit-exactly).
  Its editor (`view-devices::leveler`) is a panel in the theme's colours
  with the VU (`fet76::Needle`), the big knobs, the bat-handle switch and
  the meter selector. About 18 % of a core a side, the valve amplifier at
  2× the largest share.
* **Tape Machine** (`devices::tape`, id `faderframe.tape-machine`):
  recording to and playing back from tape, modelled physically and fitted
  to published tape and machine figures. The record side is Jiles–Atherton
  magnetisation (`tape/media.rs`: a tanh anhysteretic for oriented
  particles with a negative mean field acting on the anhysteretic state,
  solved by an exponential integrator — stable where RK4 diverges). AC
  bias is the anhysteretic process itself, so at the aligned bias the
  anhysteretic curve is what is heard; under-biased, the particles the
  bias does not switch record on the loop (crossover-like distortion),
  over-biased the drive falls and the recording zone grows. Formulations
  456-type, 632-type and a GP9-like high output (456: 0.10 % third
  harmonic at 250 nWb/m, 3 % at about 1067, against the sheet's 1077).
  Heads and EQ (`tape/heads.rs`): NAB/IEC/AES per speed, thickness, zone,
  gap and spacing losses, the head bump. Transport (`tape/transport.rs`):
  capstan rotation lines, random and scrape flutter at the IEC 386 2σ
  figures of a studio machine (0.053/0.042/0.033 % weighted at 7½/15/30
  ips), held inside the oversampler's latency (`tape/os.rs`), so the device
  reports only the oversampling's (64/72/74 samples at 2/4/8×; 2× by
  default). Level-neutral at the operating level, Auto Output takes the
  input trim back; hiss off by default (on every track it would add up),
  at the sheet's level when on. Editor `view-devices::tape::TapeFace`: two
  VUs, the response and the M–H curve. About 1.6 % of a core at 2×.
* **Hardware Insert** (`devices::hardware_insert`): outboard gear in the
  chain — the graph sends the input to an interface output and takes the
  return from an input (`NodeSpec::after` orders the return after the send
  and counts the send's latency in its arrival), the round trip measured by
  a ping (`EngineController::ping`: an impulse out, the return scanned, on
  the device level after the render) and kept as the insert's Round Trip,
  which the graph compensates; the session opens the device with the
  channels the project wants (`wanted_channels`, `UiRequest::ReopenAudio`).
  It is as wide as the track where it sits (a stereo track: two outputs and
  two inputs from the channels chosen). Channels = Mono → Stereo (`id::
  CHANNELS` 7, not automatable) is for stereo gear fed from a mono source:
  a mono track goes out on two outputs alike (the graph duplicates it into
  the send) and comes back as a pair, so the track is stereo from the
  insert on — `PluginSlot::{widens_mono, output_layout}` decide it from the
  slot's saved parameters (beside the devices that always widen,
  `builtin::widens_mono`), so `Track::chain_layout`, `Track::layout_into`,
  the graph and `required_channels` agree; the setting is part of the
  insert's `shape`, so changing it rebuilds the graph. Mono gear on a mono
  track keeps one output and one input: sending it to both would tie up an
  output and, cabled into a stereo unit, feed two paths that differ by the
  unit's tolerances. Test `mono_to_stereo_sends_both_outputs_and_returns_a_pair`
  (the gear halves the right channel; the master hears the pair as a pair),
  session `mono_to_stereo_takes_a_pair_each_way`.
* **Limiter** (`devices::limiter`): a lookahead limiter that guarantees
  its ceiling — per channel the minimum needed gain over the lookahead
  window, averaged over an attack window no longer than the lookahead
  (`limiter::style`: Transparent all of it, Punchy a half, Aggressive a
  quarter, Allround 0.7, Bus 0.6 with a slower and always programme
  dependent release, Safe all of it with a 0.3 dB margin under the ceiling
  so true peaks stay under it through the interpolator's error, Modern
  0.4 quick and programme dependent), so the gain is down before the peak
  arrives; release (auto: slower while limiting goes on); stereo link of
  the detection and `id::RELEASE_LINK` of the reduction (each side pulled
  towards the deeper one); a true-peak mode measuring the 4× oversampled
  input (6 samples more latency) and publishing the output's true peak;
  `id::OUTPUT` trims after the clamp (the output ceiling is the ceiling
  plus the trim); unity gain listening. The output's momentary and
  short-term loudness (`dsp::loudness::LoudnessMeter`: BS.1770
  K-weighting, 100 ms buckets, allocation free) are published
  (`value::{LUFS_MOMENTARY, LUFS_SHORT}`). Ids 9–10 are the second
  revision's, neutral by default. Editor: history of in, out and
  reduction with the ceiling, readouts of the reduction, the output's
  (true) peak now and its highest since the readout was clicked, the
  short-term and momentary loudness. Presets: 13 at the top of the menu
  plus the same six groups as the compressor's.
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
* **Channel Strip** (`devices::channel_strip`, id
  `faderframe.channel-strip`): input trim, high pass (12/18/24 dB/oct)
  and low pass, optionally only in the dynamics' key; a gate (threshold,
  range, attack, hold, release, hysteresis) or 2:1 expander; a compressor
  (the stock compressor's gain computer, peak or RMS, make-up, mix for
  parallel compression); four bands (LF/HF shelf or bell, two
  parametric mids; "Black" narrows bands as their gain grows), before or
  after the dynamics; console drive; output. The dynamics are keyed by
  the channel or the sidechain, and Sidechain Listen plays the key.
  `bands()` is shared with the editor's curve. Zero latency.
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
  swap, mid, side), polarity and trim per side (before the width), width,
  balance, gain and mute — at rest the identity times the gain, so old
  projects sound the same — plus a DC filter and mono bass
  (`dsp::filter::Crossover`, a Linkwitz–Riley split of the side; the mid
  through the same crossover's all pass, so above it both stay in phase;
  crossfaded on and off). Editor: vectorscope, correlation and balance.
* **Delay** (`devices::delay`, the built-in Echo's id; its five
  parameters keep their meaning): stereo, ping-pong or mono; free or
  synced (note divisions, dotted and triplet), stereo offset, feedback to
  110 % into a loop that is linear to full scale and then rounds off; per
  pass the Echo's damping, a low cut, a tilt shelf round 1 kHz
  (`TILT`), saturation and a style (digital; tape: arctangent, head
  bump, high-frequency loss, wow; analog: a bucket brigade's 4.5 kHz
  band limit); wow and flutter and a chorus on the read heads
  (`MODULATION`/`MOD_RATE`, up to ±4 ms); diffusion (four
  `dsp::delay::Allpass`es a side on the read, their length taken off
  the delay so the repeat lands on time); time changes glide; ducking,
  width, freeze. `LINK` (on by default) makes the right side follow the
  left; off, it has its own time (`TIME_R`) and note value
  (`DIVISION_R`) — ids 16–22 are all neutral at their defaults, so
  projects saved before them sound the same. Editor: the repeats on a
  timeline (drag for the time; smeared by the diffusion, both sides'
  notes when unlinked) and one pass's spectrum. Presets grouped
  Slapback, Rhythmic, Ambient, Tape, Creative (the first fourteen are
  the first version's).
* **Reverb** (`devices::reverb`): pre-delay (ms, or synced to a note
  value: `PRE_SYNC`/`PRE_DIVISION`, up to `MAX_PRE_SECONDS`); early
  reflections (a type's 12-tap pattern scaled by the size); four allpass
  diffusers a side into a 16-line FDN with Hadamard mixing (orthogonal),
  line lengths spread geometrically over the type's range and gliding
  with the size, slowly modulated taps, and per line a three-band decay
  (one-pole splits at the bass crossover (`BASS_FREQ`, 250 Hz) and the
  damping frequency, each band's gain exactly what its decay time asks
  for the line's length: bass × the bass multiplier, highs × `HIGH_DECAY`
  (a third by default)); orthogonal output taps for decorrelated sides.
  Types Room, Hall, Plate, Chamber, Ambience, Spring (two spring lines
  looping through twelve second-order allpasses tuned 1.8–3.8 kHz — the
  dispersion of a spring's chirp — and a 4.2 kHz low pass, feedback from
  the decay; they are the early part and feed the network), Cathedral
  (the longest lines). Shimmer (`SHIMMER`, any type): the late output
  through a two-head crossfaded pitch shifter (50 ms window,
  `SHIMMER_PITCHES` −12 … +24 semitones), band limited and softly
  limited, fed back in with the input. Early/late balance, width, wet
  cuts and shelves (`LOW_SHELF` 250 Hz, `HIGH_SHELF` 4 kHz), freeze
  (lossless, modulation off), ducking. Ids 16–23 are neutral at their
  defaults. Tests measure RT60 (Schroeder integration) within 12 % for
  every type, the highs' multiple, the synced pre-delay, the shelves and
  crossover, the shimmer's octave and its bound, the spring's dispersion.
  Editor: the decay picture (pre-delay, reflections or the springs'
  bounces, bass/middle/high tails, the shimmer's climb; drag for the
  decay) and the decay time across the spectrum with the shelves'
  curve. Presets grouped Rooms, Halls, Plates, Ambiences, Springs,
  Creative. The Delay's and the Reverb's presets are levelled
  (`delay_and_reverb_presets_are_levelled`: fully wet, a click's energy
  within −36 … +6 dB, nothing clips).
* **Modulation** (`devices::modulation`): chorus (1–4 voices a side),
  ensemble (three voices, slow and fast LFOs), flanger (feedback ±95 %),
  phaser (2–12 first-order allpass stages swept exponentially round the
  centre, feedback), vibrato, tremolo (`tremolo_gain`: down by the depth
  at the trough, the gain slewed over a millisecond so a square does not
  click; the spread makes an auto-pan) and a rotary speaker (all wet: a
  Linkwitz–Riley split at 800 Hz into a horn and a drum, each a rotor
  heard as Doppler on a swept delay line and as its level shading as it
  turns away from the microphones — `rotor_level`; the drum at 0.84 of
  the horn's rate on the ensemble's second LFO; rate changes ramp with
  the rotors' inertia, a second for the horn, two and a half for the
  drum); free or synced LFO in six shapes with a stereo spread; wet high
  cut and width. Editor: the LFO with each side's position, and the
  voices on the delay, the comb or notches right now between the sweep's
  extremes, the tremolo's level over a cycle, or the two rotors turning.
* **Tuner** (`devices::tuner`): passes or mutes; the pitch is found in
  the editor (`view-devices::tuner`: McLeod's normalised square
  difference over 4096 frames by FFT, parabolic peak, clarity ≥ 0.8,
  median of five), within a cent from 25 Hz to 4.2 kHz; needle or strobe,
  reference A4 400–480 Hz, Transpose names the notes as a transposing
  instrument's player reads them (B♭: +2).
* **Synth** (`devices::synth`; the first nine parameters, the MIDI
  handling — pedal, bend with RPN 0, MPE zones, pressure, CC 74, mod-wheel
  vibrato — and per-note expressions are the first Synth's): two PolyBLEP
  oscillators (saw, square with pulse width, triangle, sine; octave,
  semitone, level) — oscillator 2 hard-synced to 1 (`SYNC`, 78: it
  restarts when 1 wraps, the step band-limited over the sample held back
  and the next, `held2`/`pending2`), ring-modulated with it (`RING`, 79),
  bending 1's phase (`FM`, 80, `FM_CYCLES` at a squared amount) and the
  cutoff at audio rate (`FILTER_FM`, 85: `g` scaled per sample); phases
  free-running or reset per note (`PHASE_RESET`, 81) —, up to seven unison
  copies (detuned, spread across the width), a sub (square, sine or
  triangle, `SUB_WAVE` 83; its level ±6 dB an octave across the keys,
  `SUB_KEY` 82) and white or pink noise (`NOISE_COLOUR` 84, Kellet's
  filter); drive into the filter (`FILTER_TYPE` 0–5: state-variable 12/24
  dB low pass, band, high, a four-pole ladder — `ladder`: TPT one-poles,
  the feedback solved linearly, input and feedback through `fast_tanh`,
  the passband's loss mostly made up —, and a notch) with key tracking,
  velocity and its own envelope or the amplifier's; LFO 1 (global, free
  or synced, six shapes) to pitch, cutoff, level and pulse width; LFO 2
  per voice (50–55: rate/sync/division/shape, retriggered per note,
  faded in) and a mod envelope (56–59) that reach their targets only
  through the six-slot matrix (`id::mod_source/mod_target/mod_amount(k)`
  = 60 + 3k…: a `SOURCES` entry — LFO 1/2, mod/amp/filter envelope,
  velocity, key (bipolar round C4), aftertouch, wheel, a random value per
  note — a `TARGETS` entry and a bipolar amount); poly (up to 32 voices,
  stolen oldest or quietest first, `STEAL` 101), mono or legato with glide
  (always, or only onto a key played over a held one, `GLIDE_MODE` 102,
  `keys_down`), analogue drift (`DRIFT` 103: a fixed offset per voice and
  a slow `Shape::Drift` wander, `DRIFT_CENTS`); the wheels: Pitch Bend
  Range (id 43, 1–24 st; a range a controller sets with RPN 0, or an MPE
  zone's ±48, wins on its channel) and three mod-wheel slots
  (`id::wheel_target(k)`/`wheel_amount(k)` = 44 + 2k / 45 + 2k) — a
  destination from `TARGETS` (vibrato, pitch, cutoff, resonance, filter
  envelope, drive, LFO amount, LFO rate, pulse width, oscillator levels,
  sub, noise, detune, spread, volume, pan, oscillator 2 pitch, FM, ring,
  filter FM, LFO 2 rate; the wheel's and the matrix's one list, appended
  to only) and a bipolar amount. Each voice applies its channel's wheel
  to a copy of the block's settings (`Settings::wheeled` → `Modded`) and
  the matrix on top of that (`Modded::apply`) at the segment's start and
  every control tick (16 samples); `Segment` holds what the samples read
  between ticks. LFO 1's rate follows the wheel moved last. After the
  voices, `Fx` (made with the processor, lines sized by the rate): a
  chorus (one tap a side in antiphase) or ensemble (three a side, slow and
  fast sweeps) (86–89), a stereo delay (90–96: ms or a division, feedback
  through a one-pole tone, ping-pong) and a small reverb (97–100: two
  input diffusers, eight modulated lines mixed by a Hadamard matrix,
  per-line decay gains, damping; orthogonal output rows) — each run only
  while mixed in; `tails` count what is still to come, so the processor
  stays awake for echoes and tails and resets the lines when it sleeps.
  Everything from id 50 on defaults to off, and
  `earlier_patches_sound_as_they_did` holds five first-wave renders to
  their checksums. Slot 1 of the wheel defaults to vibrato at 50 % (the
  first Synth's ½ semitone). Editor (1600 × 840): three decks of sections
  under the oscillator cycles (synced oscillator 2 drawn restarting), the
  filter's response (the ladder's and the notch's too) and reach, and the
  envelopes (the mod envelope faint when a slot uses it).
* **Sampler** and **Drum Sampler** (`devices::{sampler, drums}`): what
  they play is a `samples::SampleDoc` (a file per slot) packed into the
  plugin state after the parameter block (`samples::pack`: `FFSD`, length,
  parameters, JSON), so it saves, undoes (`Command::SetPluginState`) and
  travels with presets. Loading (`samples::load`: the importer's decoder,
  any rate — playback steps by the ratio; SFZ files read by
  `devices::sfz`: `#define`, `#include`, block comments, `<control>`
  (`default_path`, note/octave offsets, `set_ccN`), `<curve>`s, the
  generators `*sine`/`*saw`/`*square`/`*triangle`/`*noise`/`*silence`,
  and the SFZ 1 opcode set with the common SFZ 2 additions inherited
  through `<global>`/`<master>`/`<group>`/`<region>` into `sfz::Zone`s —
  conditions (channel, key, velocity, controller, bend, aftertouch,
  random, tempo, round robins, keyswitches `sw_last`/`sw_down`/`sw_up`/
  `sw_previous`/`sw_default`, triggers attack/release/first/legato/
  `release_key`, `on_loccN`), voice control (`group`/`off_by`/`off_mode`,
  `polyphony`, `note_polyphony`), playback (offset with random and
  controller, end, count, loops with crossfade, direction, delay),
  amplifier (volume, amplitude, pan, width, position, key/velocity
  tracking, velocity curves, random, `rt_decay`, key/velocity/controller
  crossfades), pitch (key centre, tracking, velocity, random, bend range
  and step), two filters (1/2/4/6-pole low/high/band pass and reject,
  peak, shelves) and a three-band EQ, `ampeg`/`fileg`/`pitcheg` (delay,
  start, attack, hold, decay, sustain, release, depth, `vel2*`) and
  `amplfo`/`fillfo`/`pitchlfo`; anything of these by controller as
  `_ccN`/`_onccN` with `_curveccN`, the extended controllers 128–136
  (bend, aftertouch, velocity, key, random) included. Voices evaluate
  the modulation every 16 samples; `<effect>` buses are not played)
  happens on the control thread; the set is swapped into a `TryCell` the processor only
  ever `try_lock`s (voices of an older generation stop; the old set is
  dropped on the control thread) and handed to the editor through
  `AnalysisTap::set_assets`. Sampler: one sample across the keys (root,
  loop off/forward/while held with a crossfade, start, reverse, key
  tracking), the sample cut into slices played a key each (slice mode,
  see *Slicing* in section 10) or an SFZ; ADSR, filter with envelope,
  velocity, up to 64 voices. Drum sampler: 16 pads from a first note, each with level, pan,
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
  each lands. A state naming samples not decoded yet (opening a project,
  undo to samples nothing holds any more) loads them **in the background**
  in the live session (`PluginInstance::configure_background_loading`,
  asked by the engine's `PluginHost::set_background_loading`, which the
  session turns on): `SampleHost::set_doc` starts a thread, `flush` (from
  `poll`) swaps the set in when it is there; the device plays what it had
  meanwhile. Offline renders and tests load before `load_state` returns.
  `Session::{samples_loading, wait_for_samples}`. In memory the paths are absolute; the project file stores
  them relative to itself (`samples::map_states` in `save_as`/`open`), and
  the first save moves the scratch folder's samples with the media
  (`media_moves`, also applied to states brought back by undo).
* **Wavetable**, **FM** and **Granular** (`devices::{wavetable, fm,
  granular}`, ids `faderframe.wavetable`, `faderframe.fm`,
  `faderframe.granular`): three instruments rather than one with three
  engines — each engine's parameters (the FM's six operators and free
  matrix alone are 167) would otherwise crowd every automation and learn
  menu, preset and generic editor with values that do nothing for the
  engine in use, and each would carry the others' memory (tables, a
  sample). What they share lives in `devices::poly` so they play alike
  and like the Synth: the first 82 parameters of each (`poly::id`, `id ==
  position`; then the instrument's own from `poly::COMMON`) are volume,
  the amplifier envelope, the Synth's six filters or none (`VoiceFilter`
  wraps its SVF and ladder), the filter and mod envelopes, LFO 1 (one for
  all voices, free or synced) and LFO 2 (per voice, retriggered, faded
  in), voicing (poly up to 32 with oldest/quietest stealing, mono, legato,
  glide always or legato only, bend range), the three mod-wheel slots, an
  eight-slot matrix and the Synth's chorus, delay and reverb (`synth::Fx`,
  made with the processor). The matrix's sources are the Synth's plus
  Timbre (CC 74 and the brightness expression) and Pitch Bend
  (`poly::SOURCES`); targets are the shared ten (`COMMON_TARGETS`, the
  Synth's scales) and then each instrument's own. `Chassis` holds the
  voices (allocated with the processor), the MIDI channels — pedal, bend
  with RPN 0 and MPE zones, pressure, CC 74, per-note expressions
  (`Expr`) — held keys, glide and the tuning (`set_tuning`: every key
  sounds the track's tuning; unmapped keys play nothing); a voice is a
  `Head` (envelopes, key, pitch, expressions, LFO 2, filter state) and its
  engine's parts. A block's MIDI events and per-note modulations are taken
  in time order (`Steps`). **Per-note modulation**: the three answer
  `modulatable_per_note` for every continuous per-voice parameter
  (`per_voice`: not LFO 1, the voicing, the effects or a choice); a
  `NoteParamMod` sets a share on the voices of its key (`NoteMods`, eight
  parameters a voice), and such a voice reads its settings for itself
  through `ParamValues::shifted` (the share in the parameter's own scale)
  whenever they change — the others share the block's. Published values:
  the Synth's nine, then each instrument's (positions, operator envelopes,
  grains).
  *Wavetable*: two oscillators reading tables of up to 256 frames
  (`table::Table`): each frame kept as eleven band-limited copies an
  octave apart (harmonics 1023, 512 … 1), each sixteen samples a period
  of its top harmonic (at least 256, so linear interpolation's images
  stay 70 dB down), built from the frames' spectra off the audio thread;
  `Mip::for_increment` picks two copies for a pitch and crossfades across
  the octave so a copy's top harmonic stays under 0.47 of the rate while
  it is heard and a glide never steps; frames are interpolated by the
  position. Thirteen factory tables are made by code (`table::FACTORY`:
  basic shapes, pulse width, a synced saw, harmonic and odd builds,
  formants, two FM ratios, a wavefolder, bit steps, drawbars, a resonant
  saw, random spectra; 32 frames each, made once per process). User
  tables are the oscillator's sample slot (`SampleHost::with_prepare(
  wavetable::prepare)`): the loaded audio cut into frames of the size the
  document gives (`SampleDoc::cycles`, `Action::SetWavetableCycle`, one
  undo step), else the file's `clm ` chunk, else guessed (a short file is
  one cycle; else 2048, 1024, 512, 256 or 4096 as the length divides);
  `SampleSet::tables`. Warps: Sync, Bend + / Bend − (a half of the cycle
  squeezed), Mirror (forwards then back, blended in), FM (the other
  oscillator in its phase); the copies are picked for the warp's speed-up.
  Up to eight unison copies per oscillator (detune in cents across them,
  spread across the field, start phase and its randomness), a sub (sine,
  square or triangle an octave or two down) and white or pink noise.
  *FM*: six sine operators (`fm::sine_table`, 4096 points) wired by one of
  the 32 classic algorithms (`fm::algorithms`, written from their
  published diagrams: modulators always above what they modulate, so
  computing 6 down to 1 has every input ready; the feedback loop is a
  sample late and takes the mean of the last two outputs, a quarter cycle
  at full) or a free matrix (any into any, the diagonal each one's
  feedback, the others' last outputs for upward routes, an output level
  each). Operators: ratio (½, 1–31 × 1 + fine) or a fixed frequency,
  detune, level 0–99 at ¾ dB a step (a full-level modulator moves the
  phase a cycle), velocity sensitivity, LFO amplitude sensitivity, an
  envelope run every control tick (attack linear to the peak, then decay,
  slope and release in dB at the rate the times between their levels set;
  equal levels pass at once), keyboard rate scaling and level scaling
  either side of a break key (±24 dB an octave at full). A voice ends when
  its carriers have released (the shared amplifier envelope defaults to
  out of the way). Pitch envelope, transpose, LFO 1 to pitch and
  amplitude, key sync, carriers spread across the field. **DX7 voices**
  (`fm::dx7`, from the published SysEx format: 32-voice packed bulk dumps
  and single unpacked voices, with or without their headers; checksums not
  required) become complete parameter sets: rates by the original's
  timing (a level falls at a rate doubling every 6¼ steps, an attack about
  eight times faster), levels kept, keyboard depths of 99 = ±24 dB an
  octave, the pitch envelope in two segments, LFO speed/shape/depths. The
  session keeps a file's bank per instance (`session::instruments`:
  `Action::{ImportDx7, LoadDx7Voice}`, `Session::dx7_bank`; a voice loads
  as one undo step, its whole state replaced after the old one is
  captured).
  *Granular*: per voice up to 48 grains started at a density (evenly or
  jittered), from a position (sprayed either side, scanning on through the
  sample at a speed unless frozen), each with a size, a window (Hann,
  triangle, Tukey, Gauss, decay, swell — tables made once), the key's
  pitch from a root (or the sample's), a transpose and a random spread, a
  place in the field, a chance of playing backwards and a random level;
  grains are scaled by the square root of their expected overlap, so the
  density does not change the level. The sample is slot 0 (the samplers'
  loading, read through the `TryCell`; a block that misses the lock is
  silent, events still taken); with none it granulates a pad of its own
  (`granular::builtin_source`). Live input is not granulated.
  Editors (`view-devices::{wavetable, fm, granular}` on `view-devices::
  poly`): two pages switched by tabs in the display — the instrument's own
  sound, and its modulation (LFOs, mod envelope, wheels, matrix, effects)
  — both ending with the filter, its envelope, the amplifier envelope,
  voicing and the output. Wavetable: each oscillator's table as a stack of
  frames, the one the newest voice reads bold; Load… copies a file into
  the slot and switches the oscillator to it (`Action::Several`), right
  click chooses the frame size or clears it, files drop on an oscillator.
  FM: the algorithm drawn (click an operator to put it on the deck) or the
  matrix as a grid (drag a cell), the operator's envelope with operator
  tabs, the DX7 bank (Import DX7…, click a voice). Granular: the sample
  with the position, the spray, the grains sounding, a drag sets the
  position; the window's shape. Presets (`presets::instruments`, twelve
  each) are levelled like the Synth's (`the_new_instruments_presets_are_
  balanced`: −18 LUFS momentary, a decibel of headroom). Tests: the
  devices' (pitch, aliasing up to C8 below −70 dB with the copies'
  crossfade continuous, warps, user tables by `clm `, per-note modulation,
  tuning; every algorithm's routing — each carrier heard, each modulator
  moving exactly the carriers drawn — feedback and the matrix, scaling,
  voices ending, DX7 dumps; grain density within a grain, level by
  density, position, scan and freeze), `poly::tests`, the presets',
  `the_instruments_do_not_allocate` (a user table with FM/sync warps, the
  FM's full matrix and a feedback algorithm, the Granular on a sample),
  `session/tests/instruments.rs`, the faces' (`instruments_tests`). Cost
  per voice (ignored `wavetable_cost`, `fm_cost`, `granular_cost`,
  release, 16 voices at 48 kHz): the Wavetable 0.3 % of a core with one
  copy, 1.3 % with two oscillators of eight copies through a filter; the
  FM 0.5–0.7 %, 1.3 % with every matrix route; the Granular 0.5 % at 25
  grains a second of 90 ms, 2.2 % at its 48 grains (grain positions are
  kept inside the sample as they move, not wrapped per read: that was
  five times the cost).

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
  slot state. A name is one file whatever its case
  (`Session::existing_plugin_preset`): the shell asks before saving over
  it, and the new spelling replaces the old file.
  `Action::DeletePreset` removes only the user's own files (a
  `.ffpreset` under the presets folder, or a track preset in the
  library, compared as canonical paths); the menus list only those. The
  shell confirms first (`PromptDeletePreset`).
  Built-in factory presets live in `plugin_host::presets`: 470 across
  the EQ, Program EQ, compressor, 76, limiter, gate, de-esser, channel
  strip, saturator, utility, delay, reverb, modulation (its tremolos in a
  "Tremolo" submenu), the four MIDI effects and Synth (its first fifteen
  on top, the second wave's 33 in Bass/Leads/Pads/Keys/Plucks/FX
  submenus; MIDI effect
  presets are played through by a test: notes, none stuck; every audio
  preset is rendered over a drum-and-voice fixture and must stay within
  12 dB of it). Samplers, the tuner and the latency probe have none. Each preset expands its overrides over
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

### Restoration devices

Five built-ins repair recordings: De-noise, De-click, De-hum, De-clip and
De-reverb (`builtin::{DENOISE, DECLICK, DEHUM, DECLIP, DEREVERB}`, devices
in `plugin_host::devices::{denoise, declick, dehum, declip, dereverb}`,
faces in `faderframe-view-devices`). Their DSP is the pure crate
`faderframe-restore`, which the spectral editor's "Learn Noise and Remove"
uses too, so a profile heard in one place sounds the same in the other.
Everything in it is allocation free once made (the devices run it on the
audio thread; `engine/tests/realtime_alloc.rs` covers every device in its
heavier modes).

* **Framing.** `restore::stft::Streaming` is a streaming STFT —
  square-root Hann on both sides at 75 % overlap, so unity gains give the
  input back exactly, a frame of latency, all of a frame's work on its last
  sample. Bin powers are kept per sample (divided by the window's energy):
  white noise of variance σ² reads σ² in every bin at any frame size.
  `restore::blocks::Blocks` frames the time-domain repairs: a window is
  `margin` samples already processed, `hop` new ones and `margin` ahead,
  the repairs written into a work copy that travels with the input as it
  came (so "listen to what is taken" is a subtraction, and a repair that
  reaches into the samples ahead is not made twice). Latency `hop + margin`.
* **De-noise** (`restore::denoise`): a Wiener gain on the
  decision-directed a priori SNR (Ephraim and Malah) against the noise
  (times Threshold), floored by Reduction (tilted across the spectrum by
  Tilt), smoothed across up to half an octave (Spectral Smoothing — bins
  with a gain near one keep it), opened by onsets (Transients), closing at
  a release time (Smoothing sets it and the decision-directed weight).
  The noise is a learned profile or `NoiseTracker` (minima-controlled
  recursive averaging, held under 2.5 times the 1.5 s minimum of the
  smoothed power; a sound held unchanged longer than that looks like noise
  to it — that is what learning is for). Profiles (`restore::profile`)
  are 64 bands a sixth of an octave wide, 20 Hz–24 kHz, in dB per sample,
  so any frame size and rate reads them. Modes: Low Latency (~11 ms
  frames), Balanced (~43 ms), Quality (~85 ms and three hops of lookahead:
  the most open gain of the frame and the three after it, so attacks are
  not dulled); the mode is `fixed` (it is the latency). Learn: while on, the
  processor averages the input's power (channels together), plays through
  what it has learned and publishes it (`value::NOISE`, 64 values); the
  face's Learn button pressed again writes it into the profile parameters
  (`id::profile(b)` = 14 + b, `PROFILE_SET`, not automatable) with Learn
  off and Source Learned, as one gesture ("Learn Noise Profile";
  `denoise::learned_profile`). Factory presets keep the profile, the
  source and Learn (`presets::kept`, `factory_preset_values` leaves them
  out); `denoise::hidden` names the profile for lists of settings.
* **De-click** (`restore::declick` on `restore::ar`): per window an AR
  model (order ≈ rate/1500) is fitted (autocorrelation method,
  Levinson–Durbin, Hann window, a tiny ridge), its prediction error passed
  through the model again backwards — the matched filter of an impulse in
  an AR process, which puts a click back where it is — and samples over
  `K` times the statistic's robust spread (median, floored for perfectly
  predictable tones) are clicks, `K` from 13.5 to 3.5 by Sensitivity; only
  what reaches half the local peak counts (the matched filter rings for
  the model's order). Crackle is taken at a lower threshold in runs no
  longer than 0.3 ms. Runs are widened by 0.1 ms, joined across half a
  click width, and continued while the sound after them stays away from
  the model's extrapolation from before the click (a click's decay is
  quieter than its edge and smooth enough to pass for music; the
  allowance grows with the k-step prediction error, held within twice the
  one-step one). Gaps are filled by least-squares AR interpolation: the
  errors the gap takes part in, minimised — a symmetric Toeplitz system
  (the model's autocorrelation) solved by Levinson's recursion in O(m²).
  Two passes: the second fits the model to the repaired window and refills
  everything with it. Linked, marks are shared by the channels. Windows of
  ~21 ms with context either side: about 32 ms of latency at 48 kHz.
* **De-hum** (`restore::dehum`): RBJ notches of a constant width in Hz at
  the fundamental and its harmonics (up to 32, below 0.45 fs), Depth
  mixing the notched signal with the input. The mains is followed by
  complex demodulation of the first four harmonics (three one-pole
  stages at 6 Hz; the phase step between readings 32 samples apart is the
  offset), weighed by how tonal each is and by its smoothed power (an
  instantaneous weight correlates with the low pass's image ripple and
  biases the mean), smoothed over half a second, held within ±3 %. Auto
  compares the 50 and 60 Hz families with 3 dB of hysteresis. No latency.
* **De-clip** (`restore::declip`): runs of two or more samples at or
  beyond the threshold (the clip level less Tolerance) on one side, up to
  Max Length; the level is the loudest the input has been lately (falling
  1 dB/s) or set. A run is filled by the same LS AR interpolation,
  constrained by an active set to stay at or beyond what was recorded on
  its side (pinned samples become known, the stretches between solved
  again, Gauss–Seidel); Best fits the model a second time on the rebuilt
  window and rebuilds from the clipped samples. Output trims the level
  (rebuilt peaks go over the old ceiling). About 32 ms at 48 kHz.
* **De-reverb** (`restore::dereverb`): per bin the smoothed power is
  split into its reverberant part, followed recursively (the last frame's
  decayed by a hop, plus what the direct part excites at the room's share
  of a held sound, which grows with Amount), so a free decay is followed
  exactly and a held note settles at that share; the late part is the
  reverberation `early` ms before decayed by that much (RT60 = Tail,
  shorter towards the highs by Damping), taken out by power subtraction
  from the smoothed power (over-subtraction and the floor — up to 30 dB —
  by Amount; no decision-directed bias against held notes). Auto Tail:
  `DecayEstimator` finds free decays in seven octave bands (the level
  falling steadily, the first 5 dB skipped), fits them, and takes the
  median of each band's recent decays and of the bands. ~21 ms frames.

Reset follows the panic rules: the STFT ring and the trackers are
forgotten (a `fresh` count, `live` flags that restart them with the next
frame), small accumulators cleared. Latencies are reported from the
parameters and the rate (`latency(params, rate)`): only De-noise's mode
changes it. Tests: the crate's own (Levinson against elimination, gaps in
tones filled, clicks found and filled, clipped sines rebuilt, the tracker
finding noise under a melody, drifting hum followed to 0.005 Hz and
notched, reverb tails estimated and taken down, the STFT's identity and
scaling); the devices on `devices::rig` (SNR raised for hissy melodies
adaptively and with a learned profile, clicks of 4–28 samples repaired with
the music kept and nothing touched in clean music, buzz notched by 30 dB
with the tone within 0.3 dB, clipped peaks rebuilt, tails between bursts
taken down by over 8 dB with the bursts kept, every mode delaying by its
reported latency, listen modes); presets (ten or eleven each, in
`presets::restoration`, the usual checks); faces (`faderframe-view-devices`
tests: controls in their sections, every theme, De-noise's Learn and
Forget as single undo steps); `session/tests/restoration.rs` (Learn Noise
and Remove in one step, the devices' latencies through a rebuild).

### Convolution reverb, multiband dynamics and the vocoder

Three stock effects on the `plugin_host::dsp` toolkit, with kit faces in
`faderframe-view-devices` (`convolution`, `multiband`, `vocoder`):
Convolution Reverb (`builtin::CONVOLUTION_REVERB`, `devices::convolution`),
Multiband Dynamics (`builtin::MULTIBAND_DYNAMICS`, `devices::multiband`) and
Vocoder (`builtin::VOCODER`, `devices::vocoder`).

* **Convolution without latency** (`dsp::convolve`). A response is cut the
  way Gardner proposed: its first 64 taps convolved directly (a dot product
  with the input history per sample), the rest in stages of partitions
  32, 256, 2048 and 16 384 long, each stage a uniformly partitioned
  overlap-save convolution starting two of its partitions into the
  response (`64 + 14·32 = 512 = 2·256`, …; the last stage takes any number).
  Starting two partitions in is what lets a stage spread its work: the
  output a block of input makes is not due before another block has
  passed, so the block's transform, its products with every partition and
  the inverse transforms run as units over the next block's samples (at
  most one large transform in a callback; no burst at a boundary). An
  `Engine` convolves any `Routing` of paths (an input into an output) up
  to a capacity fixed when made (its frequency-domain delay lines hold
  that much history); it runs in pieces of at most 32 frames
  (`room`/`input_mut`/`run`/`output`, or `convolve::run` over buffers).
  New `Kernels` for the same paths go in without losing the history: each
  stage computes its next block with the old and the new kernels and
  crossfades over the block after (the head over 32 samples), then the
  old set comes back from `take_retired` to be freed elsewhere. Kernels
  are made off the audio thread (`Kernels::new`); a `Designer` thread
  (15 ms) makes them for a processor and sends them through a mailbox,
  freeing what the audio thread hands back through a ring. Reset forgets
  (history counts as silence, outputs as not computed) and clears
  nothing large.
* **Convolution Reverb.** The response is a file kept like a sampler's
  sample (its state's `SampleDoc` slot 0: copied into `<media>/Samples`,
  decoded in the background in a live session, relative in the project
  file; `session::samples::is_sampler` names it, and a file loaded sets
  Space to the file in the same step, "Load Impulse Response",
  `samples::plays_the_file`) or a built-in space (`convolution::spaces`:
  made here, never recorded — rooms are a shoebox image-source model for
  the early part, walls' reflection from Eyring for the middle RT60,
  higher orders low-passed, and a three-band noise tail at Sabine's
  diffuse level faded in over the mixing time; a plate is the tail alone;
  true stereo, the paths' tails decorrelated). Files may be mono, stereo
  or true stereo (four channels: LL, LR, RL, RR; `Sample::extra` keeps the
  third and fourth). Start, Length, Fade In/Out, Reverse, Stretch
  (resampled: the room scaled) and Normalise (the outputs' energy made
  one) shape the response on the designer (`convolution::shape`, one
  windowed-sinc resampling to the engine's rate); they are not
  automatable. A new shape of the same source is new kernels; a new
  source or one longer than the engine holds (capacity = the source at
  twice its length) is a new engine, crossfaded over 4096 samples
  (`ENGINE_FADE`). Kernels name the engine they were made for (`tagged`,
  the processor publishes the engine it plays), so none reach an engine
  about to be replaced. After the convolution: Pre-Delay (a gliding delay
  line before it), Low/High Cut (12 dB/oct), Width, Output and Mix. No
  latency; the editor reads the response through the tap's assets
  (`ImpulseView`: envelope, energy to come, RT60 by Schroeder's T20,
  what is wrong). In a live session the first response is made in the
  background (the wet signal comes in when ready); offline it is made with
  the processor, so renders repeat exactly. Undoing the first file loaded
  (a slot whose state goes back to none) puts the device back as it was
  made: the engine keeps every built-in's state from when it was made
  (`plugins::Hosted::initial`) and loads it then (samplers alike).
* **Multiband Dynamics.** One to six bands. Minimum phase: Linkwitz–Riley
  crossovers (24 dB/oct) in a tree, each band through the all passes of
  the crossovers above it (`x − 2√2·band` of a state variable filter at the
  crossover) so the bands sum to one all pass at any count; no latency.
  Linear phase: per band a symmetric FIR (about 85 ms, `fir_taps`) whose
  magnitude is the Linkwitz–Riley band's (`band_magnitude`: they add up to
  one, and the same Hann window on each keeps their sum an impulse), run
  as twelve paths on the convolution engine, new crossovers designed on a
  thread and crossfaded in; latency half the FIR (`fixed` parameter). Per
  band: the key's band (the input's, or a sidechain split alike and, in
  linear phase, delayed by the FIR's half), peak or RMS, linked; the gain
  computer (`multiband::curve`) — Compress takes `1 − 1/ratio` of what is
  over, Expand `ratio − 1` times what is under, Upward Compress lifts
  `1 − 1/ratio` of what is under, Upward Expand `ratio − 1` times what is
  over, soft knee, Range caps every change; Attack moves away from 0 dB,
  Release back. Lookahead delays the bands; solo, mute and bypass per
  band; Mix blends with the unprocessed bands (in phase with the wet).
  Parameter ids: globals 0–12, band `b`'s fields at `13 + 11 b + field`
  (`id::band`, `id::field_of`; `parameter_group` names the band).
  Published: per band its gain change and level, the deepest cut
  (`devices::reduction_value`: the mixer's gain-reduction meter).
* **Vocoder.** The modulator (the input or the sidechain, mono) through a
  bank of 8–40 fourth-order band passes (two state variable filters each)
  between Low and High, spaced in octaves, mels or hertz, as wide as
  Bandwidth times their spacing; each band's envelope followed. The
  carrier (the synth, noise, the sidechain or the input) through the same
  bank moved by Formant, each band turned to the modulator's envelope:
  divided by its own envelope with Enhance (the carrier's tilt flattened;
  a −80 dB floor so a carrier fading out is not lifted back), by the
  carrier's overall envelope without. The synth: two PolyBLEP oscillators
  per voice (saw to pulse, detuned), breath noise with the voices, pitches
  from a held chord on Root, the notes played (the vocoder takes notes:
  on an instrument track, or from a MIDI track playing it; eight voices,
  gliding), or the modulator's own pitch through the Voice-to-MIDI tracker
  (`faderframe_analysis::voice::VoiceTracker`, notes and bends). The
  unvoiced detector compares the modulator's highs (over 3 kHz) with all
  of it; consonants' highs pass straight through, as loud as Unvoiced. No
  latency.

Presets (`presets::{convolution, multiband, vocoder}`, thirteen or twelve
each; the usual checks render them all). Cost (release, one core, 48 kHz,
ignored `device_costs`): the Convolution Reverb about 1 % (a small room)
to 1.2 % (the cathedral's 7.6 s, true stereo), Multiband Dynamics 3 % (six
bands) or 4 % (linear phase), the Vocoder 1.6 % (16 bands) to 3.2 % (40). Real-time safety:
`the_convolution_multiband_and_vocoder_do_not_allocate` in
`engine/tests/realtime_alloc.rs` (a true-stereo file and a built-in space
with pre-delay automated, both multiband phases keyed and automated, the
vocoder's chord, follow and sidechain carriers, then new responses,
engines and crossovers taken in while armed). Tests: `dsp::convolve`
(exact against direct convolution at lengths across every stage, true
stereo, kernels crossfaded in with the history kept and a steady input
gliding, reset, refusals), `convolution::{shape, spaces, tests}`
(resampling, trimming/fades/stretch/normalising, every space's RT60 and
decorrelation, no latency, a four-channel file's paths, pre-delay, cuts,
width, shaping while playing, a new engine, background first build,
reset), `multiband::tests` (bands sum flat at every count, linear phase
returns the input exactly a half FIR later, a loud band compressed by its
curve and the others left, the static curves, upward compression, solo/
mute/bypass, an external key, lookahead latency), `vocoder::tests`
(the modulator's spectrum on the carrier, silence, formant, sidechain as
carrier and as voice, follow, MIDI voices, consonants), views
`the_convolution_multiband_and_vocoder_faces_hold_their_controls`,
`the_multiband_face_picks_bands_and_drags_crossovers`,
`the_convolution_face_loads_and_uses_a_file`, session
`tests/convolution.rs` (copied in, one step that also picks the file,
undo, saved relative, opened elsewhere and played).

### Pitch Correct

Pitch Correct (`builtin::PITCH_CORRECT`, `plugin_host::devices::pitch_correct`,
face `view-devices::pitch_correct`) is a live, monophonic pitch corrector,
written fresh. It takes notes (`note_inputs` 1) and is an ordinary effect
otherwise.

* **Detection** (`pitch_correct::detect`): YIN on the input low-passed and
  decimated to about 8 kHz, every 2 ms, over a window as long as the
  lowest period of the Range; the first dip of the cumulative mean
  normalised difference under 0.15 is the period (no octave too low), a
  parabola places it, and it is refined on the full-rate signal over the
  same span (to a cent). While a note sounds, a jump by an octave or a
  twelfth is taken only when the lag near the old period repeats clearly
  worse (a strong second harmonic or a formant on it holds no sway).
  Quieter than −50 dBFS or an aperiodicity over 0.35 is unvoiced; two
  unvoiced frames in a row end the note.
* **Targets**: the nearest allowed key to the pitch followed over 12 ms —
  the key track's key where the voice is (or the key set here when there
  is none), the chord track's chord (else the key), the key set here,
  every semitone, the notes ticked (Custom), or the keys a MIDI track
  playing the track holds (MIDI; none held: the voice is left alone).
  Keys are tuned by the track's tuning (`set_tuning`: the processor's
  `TuningTable`), so a 19-EDO project pulls to its own steps and the
  reference pitch counts. A note is left only once the voice is 0.2
  semitones nearer another (0.45 with the natural vibrato kept).
* **Correction**: on the error against the target, its 80 ms average
  (started afresh where a note starts) is the intonation and is taken
  out; the rest — vibrato, scoops — is kept by Natural Vibrato (100 %:
  as sung; 0: flattened onto the note). The shift glides there at the
  Retune Speed (95 % of the way in that time; 0 = at once, the hard-tuned
  sound), more slowly on notes held longer with Humanize (up to 400 ms
  more after 0.7 s). Transpose moves the result by semitones.
* **Shifting** (`pitch_correct::shift`): TD-PSOLA at a fixed latency.
  Marks are laid on the input a sung period apart (5 ms unvoiced), each
  with the ratio and formant factor wanted where the analysis frame
  nearest it was centred; output grains one target period apart are cut
  around the mark nearest their place less the latency, two periods long
  under a Hann window, so the formants stay (Formant moves them; with
  Preserve Formants off they move with the pitch). Formants down read
  less than two periods into the same span, formants up squeeze two
  periods into less, so a grain never reaches more than a period either
  side and the latency is 2.5 of the Range's lowest period (Soprano
  150 Hz ≈ 17 ms, Alto/Tenor 95 Hz ≈ 26 ms, Low Male 62 Hz ≈ 40 ms,
  Instrument 40 Hz ≈ 63 ms at 48 kHz; Range is `fixed`, a change rebuilds).
  The grain grid is locked to the marks plus the latency (and drawn back
  to them, at most 0.1 % of a spacing per grain, after a shift moved it),
  so a voice left alone comes out exactly the latency later — the Mix
  with the dry voice never combs. Stereo inputs are shifted alike from
  the mono sum's marks. Published: the pitch sung, the target, the pitch
  played, the correction, the clarity and the pitch classes allowed
  (`value::*`), which the face draws as a four-second history over the
  semitones.

A MIDI track may play an audio track too (the project's routing check
allows it), devices that take notes on audio tracks get the notes of the
MIDI tracks playing them (`build::devices_take_notes`: instrument and
audio tracks; such a track is fed, so it is not rendered ahead), and a
MIDI track's "Plays" menu lists audio tracks holding such a device
(`Session::midi_instrument_choices`): a melody in a MIDI track is Pitch
Correct's target. Channels past the second (a wider track) pass as they
came. Fifteen factory presets (`presets::voice`).
Tests: `devices::pitch_correct::tests` (a detuned tone pulled within the
retune time, hard tune with the vowel kept, vibrato kept or flattened,
key/chord/custom/MIDI targets, the tuning, transpose with and without
formants, latency exact and identity when in tune), `detect::tests`,
`engine/tests/pitch_correct.rs` (a MIDI track's note reaching it on an
audio track), `the_stock_devices_do_not_allocate`,
`session/tests/pitch_correct.rs`, view-devices
`pitch_correct_shows_the_pitch_and_holds_its_controls`.

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
  `BLOCK_TIMEOUT` (250 ms; `OFFLINE_BLOCK_TIMEOUT` 30 s in an offline
  render, where every block is waited for) is given up and killed by the
  next poll.
  The processor then returns `ProcessStatus::Error`, so the engine bypasses
  the plugin; the session reports it once (`session::sandbox`) and
  `Action::ReloadPlugin` drops the instance and builds it again from its
  slot. Sandboxed plugins that report unsaved state have it taken into
  their slots every 5 s, so a reload restores recent settings.
* **The UI never waits for a helper** on its regular round: the tick's
  Poll is sent and its answer read at a later tick when it is there
  (`sys::readable`: `poll` with no wait / `PeekNamedPipe`), one request on
  its way at a time; the 5-second state capture rides the same way
  (`PluginInstance::{request_state, take_state}`: a SaveState sent in the
  Poll's place, the answer stored by the session when it comes,
  `session::sandbox`). Any other request first takes in an answer still on
  its way (`collect_answer`), so replies never cross; a helper that
  leaves a poll unanswered for `REQUEST_TIMEOUT` (60 s for a state) is
  given up. `PluginInstance::settle` waits for answers on their way
  (tests). `the_host_does_not_wait_for_a_busy_helper`: a state taking
  400 ms to write arrives while polls return within milliseconds.
* **Stale answers.** A pipelined answer describes the helper as it was
  when it read the request; what FaderFrame changed after sending it the
  answer cannot know. `RemoteInstance::changes` counts the host's changes
  (`Changes`: editor opened/attached/floated/closed, state or program
  loaded, activated/deactivated, parameter set — `set_at` says which and
  when), each Poll/SaveState carries the counts it was sent with, and an
  answer from before a change does not overwrite what that change set:
  `editor_open` and editor requests, `program`/`pending`, latency and tail
  (no restart for a latency of the previous activation), values set since,
  a saved state from before a load (`take_state` asks again). Events —
  edits made in the plugin's editor, new parameter lists, restart requests,
  values changed only in the helper — are always taken. This was issue
  #12 (0.22): the tick after an editor opened applied a poll answered
  just before — "editor not open" —, the UI took that for a closed editor
  and destroyed the X11 window the plugin's GUI sat in; Xlib plugins
  (JUCE: TONE3000, the reporter's #TCompressor) then died of BadWindow /
  BadDrawable in their next request, others lost their GUI. Tests:
  `tests/pipelined_polls.rs` (each fails without the counts). An editor's
  life from a script: `insert-plugin:<id>` (opens it), `close-insert:<n>`,
  `show-insert:<n>`, `remove-insert:<n>`, `reload-plugins:x`.
* **X errors are not fatal** (`xlib::tolerate_x_errors`): Xlib's default
  error handler prints "X Error of failed request" and calls `exit(1)`,
  and plugin toolkits leave installing one to the host (JUCE does only in
  standalone apps). Helpers install a handler that logs (the first 20,
  then every hundredth) and returns, before the plugin loads; FaderFrame
  installs it for plugins hosted in process too (`plugins::install`), but
  keeps a handler someone else installed first (GDK on an X11 display —
  its handler ignores errors on displays it did not open). libX11 is
  `dlopen`ed by its soname (no link-time dependency) and never closed.
  `tests/x_errors.rs` makes a BadWindow on `X_GetProperty` in a helper and
  in the test process (needs an X server; XWayland counts). On FaderFrame's
  side, a closed editor's X11 window is unmapped at once and destroyed
  `RETIRE_AFTER` (2 s) later (`plugin_window::x11::Parents::destroy`/`reap`):
  requests the plugin made on its own connection as its GUI closed reach
  the server before the window they live in is gone.
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
  (`faderframe --scan-clap <bundle>`, JSON on stdout after
  `scan::SCAN_MARKER`), so a crashing plugin cannot take the DAW down. Every
  helper (scan, check, sandbox) — and the program after a clean quit —
  ends with `faderframe_ui::plugins::end_process` (`_exit`,
  `TerminateProcess`): modules stay loaded and never see their exit entry,
  and some plugins crash in their static destructors (Native Instruments'
  Qt plugins on macOS, #47: a crash report per bundle scanned, and the
  bundle recorded as failed). An answer printed before the helper died
  still counts; bundles failed under another scanner version, or when the
  browser's Rescan is pressed (`ScanCache::forget_failures`), are scanned
  again. Results are cached by bundle size and mtime
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
  every platform the parent sits under a GTK window's header like the
  built-ins' editors (Bypass, Presets, Outputs, Parameters — see *Editor
  windows*). On Linux FaderFrame is a Wayland client, so the parent is an
  X11 window (XWayland) of a separate connection (`x11rb`): inside a GTK
  window on GDK's X11 display, or, where that display cannot be opened, a
  plain top-level window created at the editor's size, with fixed-size hints for non-resizable editors (window
  managers float them), `WM_DELETE_WINDOW` closing the editor, configure
  events resizing resizable ones and `request_resize` resizing the parent. Plugins that
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

### ARA (Audio Random Access)

ARA 2 plug-ins — Melodyne, SpectraLayers, VocAlign, RePitch — read the
host's audio whenever they like and edit it as a whole; they come as VST3
or CLAP plug-ins with an ARA extension. FaderFrame hosts them through the
ARA C API (ARA SDK 2.3.0, Apache-2.0), translated by hand in
`faderframe-ara` (`ffi.rs`: the header packs its structs to one byte on
x86 and naturally on ARM, so structs whose fields the packing moves are
`repr(C, packed)` there, pointer tables plain `repr(C)`; everything a
plug-in hands over is copied with `read_unaligned`; layouts are checked
against offsets worked out from the header).

* **The crate.** `Factory` (a plug-in's static factory: name, archive IDs,
  stretching, analysable content; initialised once per process at the
  newest generation both sides know, at least 2.0 Final — region
  sequences and partial persistency are needed), `Document` (the host's
  five interfaces and the plug-in's document controller), `Model` (what a
  document should hold, by key), `Renderer`/`Assignment` (a bound
  instance's playback regions and previewed sequences), `Extension` (what
  binding hands back), `Samples` (where audio readers read). The host's
  interfaces: audio readers (one thread each, several at once, 32 or 64
  bit, silence outside the source; never the plug-in's render thread, by
  ARA's rules, so they may do file I/O), archive readers and writers
  (positional writes, zero-filled gaps, reads checked against the size),
  the musical context's content (tempo sync points, bar signatures, keys,
  chords — what the plug-in asks for from its calls), model updates
  (recorded during `notifyModelUpdates`: analysis progress, sources,
  modifications and regions changed, document data) and playback requests
  (from any non-render thread). Host references are pointers to boxes the
  document owns until after the plug-in's controller is destroyed.
* **One edit cycle per change.** `Document::sync(model, assignments,
  archive)` compares the model with a mirror of what the plug-in holds and,
  when anything differs, runs one cycle in ARA's order: document name,
  musical context, sequences, sources (access enabled after creation,
  disabled around a change of length, rate or channels), modifications,
  regions, the archive (restored once every object exists), renderers
  (removals first — no region is destroyed while a renderer holds it —,
  then additions), then regions and sequences no longer wanted are
  destroyed, and modifications and sources deactivated for the undo
  history: they come back with their edits when the model has them again
  (an undone delete). The store leaves deactivated objects out. A region
  never changes its modification under the same key (the key names it).
* **The model from the project** (`faderframe_engine::ara`): one document
  per factory (`AraHost` in the `PluginHost`, after its instances so they
  go first); a region sequence per track with an ARA insert (its name,
  colour, place); an audio source per media file (persistent ID
  `src:<source id>`; files are read through a `WavFile` of their own —
  never the live `StreamSource`s —, generated material from memory); an
  audio modification per track and file (`mod:<track>:<source>`: splits,
  trims and moves keep the plug-in's edits; copies on one track share
  them); a playback region per clip piece (`r:<clip>.<piece>@<mod>`: a
  plain clip is one piece, a take folder's comp one per piece), in song
  seconds and source seconds (a warped clip asks for stretching when the
  factory supports it). Muted clips and frozen tracks have none. The
  musical context is the tempo map (sync points at every change and every
  quarter of a ramp, one at quarter 0), the bar signatures, the key changes
  and the chord track (a gap after a chord is ARA's "no chord").
* **Binding.** `Project::ara` (`faderframe_project::ara`) lists the inserts
  bound to ARA (a side table: `PluginSlot` literals stay as they are;
  `Command::SetAraSlot`, Graph) and keeps each factory's archive (base64,
  with the plug-in's name, vendor and version to say what is missing). An
  ARA insert is instantiated in process (`PluginFactory::instantiate_in_
  process`: the sandbox would put it in a helper, but the plug-in reads
  FaderFrame's audio and model through pointers, so ARA plug-ins never
  run sandboxed) and bound in `PluginHost::ensure` before its state loads,
  it activates or its editor opens (VST3: the component's
  `IPlugInEntryPoint2::bindToDocumentControllerWithRoles`, else ARA 1's
  `bindToDocumentController`; CLAP: the `org.ara-audio.ara.
  pluginextension/2` extension, or its draft ID). It takes all three roles:
  it renders its track's regions (its input is ignored), previews edits
  (its track's sequence) and its editor shows the document. The first
  instance of a factory makes the document, whose first cycle creates
  every object of every track using the plug-in and restores the project's
  archive. A plug-in that is no ARA plug-in after all plays as an ordinary
  insert (remembered, not tried again while its slot stays).
* **Keeping in step.** `EngineController::sync` runs `PluginHost::sync_ara`
  for Timeline and Graph impacts: instances whose binding no longer
  matches their slot go (made again by the build); a document whose
  project archive it never restored or stored belongs to another project
  (one opened, a version restored) and goes with its instances; renderers
  whose regions change (a clip added or removed on their track) are
  deactivated first — ARA lets a playback renderer change its regions only
  while inactive — and the sync escalates to a graph rebuild, which
  activates them again (a short gap on that track). Moves and trims change
  only region properties and keep playing. A document nothing uses any
  more is parked (its archive kept in memory with the archives it knew):
  made again for the same project, it starts from there, so undoing and
  redoing "Open with ARA" keeps the edits.
* **Playback.** The snapshot leaves out the audio clips of tracks an ARA
  insert renders (`TimelineSnapshot::drop_audio_of`; not when it is
  bypassed: then the clips play as they are). The plug-in renders at the
  transport position it is given; a plug-in node marked
  `rendering_the_timeline` behind latency gets the transport set back by
  its input latency, so what it renders lines up with the other tracks
  through PDC (`an_ara_insert_behind_latency_lines_up`: the track nulls
  against an inverted copy of the clip on another track). Render-ahead
  and renders work as for any plugin: renders make their own engine, whose
  document restores the archive the session captured just before.
* **Archives and saving.** `Session::capture_plugin_states` (before saving,
  rendering, engine rebuilds and edits that remove plugins or unbind one)
  stores every document's archive in the project (bookkeeping, no undo
  step). Edits made in the plug-in are reported through model updates
  (polled with the plugins every tick) and keep the project dirty
  (`Session::is_dirty` asks `EngineController::ara_unsaved`) until the
  next capture.
* **The session** (`session::ara`): `Action::OpenWithAra { clip, plugin }`
  puts the plug-in first on the clip's audio track and binds it (one undo
  step "Open with <plug-in>": `InsertPlugin` + `SetAraSlot`), or with
  `plugin: None` uses the track's own; either way the editor opens
  (`UiRequest::PluginEditor`) with the clip selected
  (`notifySelection`). `Action::AraNotesToMidi` reads the notes the plug-in
  found in the clip's regions (song time) into a MIDI clip on a new
  instrument track (the FaderFrame Synth) under it. The plug-in's play,
  stop, locate and cycle requests drive the transport (`tick_ara`).
  `Session::{ara_plugins, ara_menu, ara_rendered_tracks, ara_analysis}`.
  Self-freezing keeps ARA tracks live (`Live::Ara`: a cache would not
  follow the plug-in's edits). Not mapped: clip gain, fades and reversing
  (an ARA plug-in reads the files; with one renderer per track there is
  nowhere to apply them), ARA audio file chunks, processing algorithm
  choice, plug-in content (tempo, key) read back other than notes.
* **Discovery.** VST3 modules pair an ARA plugin's processor class with
  an "ARA Main Factory Class" class of the same name; the scan also asks
  each component for `IPlugInEntryPoint`. CLAP plugins advertise
  `ara:supported`/`ara:required`; the scan also reads the entry's
  `org.ara-audio.ara.factory/2`. Both end up as the `ara:supported`
  feature in the scan cache; `PluginFactory::ara_plugins` lists them.
* **UI.** The arranger's audio clip menu has the session's `ara_menu`
  ("Open with <plug-in> (ARA)" per installed ARA plug-in, a submenu when
  there are several; on an ARA track "Edit in <plug-in> (ARA)" and "Notes
  Found by <plug-in> to MIDI"). A mixer insert that is an ARA plug-in (or
  bound) has "Render the Track's Clips (ARA)" in its menu, checked while
  bound (`Command::SetAraSlot` either way: the instance is made anew). The
  editor is the ordinary plugin window.
  Dev actions `ara-open:<track>=<plugin id>` (its first clip; no id: the
  track's own), `ara-notes:<track>`.
* **Tests.** `faderframe-ara`'s mock plug-in (feature `mock`, a plug-in
  side written in Rust: factory, document controller, renderers, editor
  view; it analyses sources through the host's readers, finds notes, keeps
  a gain per modification as its "edit", archives it, renders its regions,
  and lists every breach of ARA's rules it sees) drives the crate's tests
  (model held and rendered, a moved region while playing, a delete and its
  undo keeping the edits, archives into a new document and refused from
  another plug-in, notes in song time, reports and requests, a reshaped
  source, the selection, a document emptied before its controller goes, a
  region handed between renderers, readers and archives at the C level).
  `plugin-clap/tests/ara_clap.rs` puts it behind a `clack-plugin` CLAP
  plugin carrying the ARA extension: found as ARA, bound before activation
  (a second binding refused), rendering at the song position CLAP's
  transport gives it.
  `faderframe-plugin-host`'s `ara_mock` (feature `ara-mock`) hosts it as a
  CLAP-format plugin for `engine/tests/ara.rs` (renders like the clip
  player would, moves and removals, edits through the archive into a
  render, bypass, behind latency, notes) and `session/tests/ara.rs` (open
  and undo/redo, saved and reopened, notes to MIDI, the transport);
  `an_ara_renderer_does_not_allocate` in `realtime_alloc.rs`; arranger
  `ara_tests.rs`, mixer `an_inserts_menu_shows_its_ara_binding`. Renders
  wait (at most two minutes) while a plug-in still analyses a source
  (`Document::analysing`, `OfflineRenderer` after its first sync). Against a
  real plug-in (the ARA SDK's ARATestPlugIn built with the VST3 or CLAP
  SDK, or an installed one; none of the user's installed plug-ins is an
  ARA plug-in, and building ARATestPlugIn was not possible here):
  `FADERFRAME_TEST_ARA=<bundle>[:<bundle>…] cargo test -p
  faderframe-session --test ara_installed -- --ignored --nocapture` —
  listed as ARA, bound, the clip audible through it, archived on saving,
  bound again on opening, and no ARA asserts (`faderframe_ara::
  assert_count`: ARA's assert function counts programming errors either
  side reports).

### Multi-output plugins

A plugin's output buses are listed main first
(`PluginDescriptor::audio_outputs`), and `PluginInstance::output_bus_names`
gives their names. The engine caches buses and names per instance
(`PluginHost::outputs`, `OutputBus`).

* **Taking a bus, or one channel of it.** A track takes an extra bus with
  `InputRouting::Plugin { plugin, bus, channel: None }`; bus 0, the main
  one, is the plugin's own track. With `channel: Some(c)` a (mono) track
  takes channel *c* of the bus alone — the user decides how a plugin's
  channels become tracks, whatever its format grouped them into: a drum
  instrument that sends its microphones on stereo buses (DrumGizmo's
  sixteen ungrouped LV2 ports become eight pairs) would otherwise play two
  microphones hard left and right on one track, the close kick in one
  ear and its overhead bleed a few milliseconds later in the other, which
  sounds like flams (#52). `channel` is written only when set (older
  files read as whole buses).
  * `build_graph` puts a `PickChannel` node (`faderframe_audio_graph::nodes`,
    `Role::PickChannel`, no latency) between the bus and such a track's
    input — not the graph's own mix of a wide output into a mono input,
    which averages the channels. A channel the bus does not have is
    silence; a one-channel bus is connected as it is.
  * `PluginInstance::output_channel_names` names a bus's channels where the
    format does (LV2: each port; the sandbox forwards them in
    `Instantiated`); otherwise the session names them after the bus
    ("Out 3 L"/"Out 3 R", a number for wider ones).
  * `Project::taken_plugin_outputs` gives the highest bus taken per
    plugin. `plugin_output_source` adds a routing edge from the plugin's
    track to the taking track, so solo keeps both audible and cycle
    checks see the route.
  * `build_graph` gives such a plugin's node outputs up to that bus,
    with each bus's channels, and connects bus *n* to the taking track's
    input after pass 1. Delay compensation is the graph's.
  * Neither track renders ahead: `plays_from_timeline` refuses both.
* **Formats** fill graph output *b* from bus *b*, after
  `configure_outputs(n)` before activation:
  * CLAP copies its ports.
  * VST3 switches on output buses below *n* (`set_bus_states`) and
    reactivates when *n* changes.
  * Audio Units set a float format on each taken output element and
    render element *k* with the main render's time stamp
    (`tests/apple_units.rs` uses AUSplitter).
  * LV2 groups its ports into buses (`Lv2Plugin::output_buses`): by
    port group, else the first two ports as the main bus and the rest in
    pairs (named "Out 3/4" when the ports are numbered). More than two
    ports without groups are the host's pairs, not the plugin's
    (`output_buses_declared` false, `PluginInstance::output_buses_declared`,
    forwarded by the sandbox): tracks for them take one channel each.
  * The sandbox packs channels (`shm`: `IN_SLOTS`/`OUT_SLOTS` channel
    slots, up to `MAX_OUT_BUFFERS` = 32 output buffers; format version 4)
    and forwards `configure_outputs` with `Activate` and the names with
    `Instantiated`.
* **The Drum Sampler** has eight stereo extra outputs. Each pad's
  `id::OUTPUT` sends it to one, or to the main output while no track
  takes that output — the engine tells instances which buses are taken
  (`configure_taken_outputs`, `Project::taken_plugin_buses`), so a bus
  below a taken one that has no track of its own goes to the main too.
* **The Guitar Station** has three: DI (the guitar before the rig, at
  unity), Mic A and Mic B (each microphone on the cabinet, calibrated,
  before the blend and the pan, at the Output's level — carried as two
  more lanes of the amplifier's reservoir stage, so they stay in step with
  the main).
* **Session** (`session::outputs`):
  * `plugin_output_buses` gives each bus (`PluginOutput`) with the track
    taking all of it (`track`), the tracks taking a channel each
    (`channel_tracks`) and its channels' names; `taken`, `free_channels`,
    `covered`; `missing_output_tracks(plugin, mono)`,
    `plugin_has_wide_outputs`, `plugin_output_name(input)` (what a track
    takes, as the menus and strips name it).
  * `Action::CreateOutputTracks { plugin, buses, mono }` is one undo
    step. It makes Aux tracks named after the buses (prefixed with the
    plugin's track when a name is only a number), with the layout from
    the bus's channels — or, `mono` or a bus the plugin did not group
    itself (`PluginOutput::declared`, `by_channel`: DrumGizmo's sixteen
    ungrouped LV2 microphones, #52), a mono track per channel no track
    takes yet ("Create Mono Output Tracks" in the mixer's insert menu,
    the arranger's track menu and the editor window's Outputs button;
    "<bus> as Mono Tracks" per bus) —, routed like the plugin's track, in
    a new folder under it or in the folder earlier ones are in. The
    plugin's track goes silent once its main has a track or a track per
    channel.
  * `Action::RemoveOutputTrack { plugin, bus }` gives an output back:
    the tracks taking it go (their folder too when that empties), one
    undo step; for the main, the plugin's track gets its routing back
    once no track takes any of it (`keep_main_heard`, also when tracks
    are removed by hand).
  * The input choices list other tracks' extra outputs and, for wider
    ones, each of their channels (not ones that would loop).
  * Tests: engine `tests/plugin_outputs.rs` (outputs meet on the same
    sample whatever the latency, block, worker pool or render-ahead;
    `a_track_taking_one_channel_gets_that_channel_alone`), session
    `mono_output_tracks_take_a_channel_each`,
    `a_track_can_take_one_channel_by_hand`, the pick in
    `the_instruments_do_not_allocate`. A missing plugin leaves its output
    tracks silent and the rest of the graph playing (it used to fail the
    build).
  * Freezing a track whose plugin feeds output tracks is refused. Track
    presets drop a plugin-output input.
* **UI**: the insert menu (Create Output Tracks, an Outputs submenu —
  a ticked output removes its track),
  the arranger's track menu, the editor header's Outputs menu
  (`app.create-output-tracks`, dev form `insert=<n>`), and the mixer's
  input well (the bus name; Aux/Bus strips taking a bus get the input
  row).

### LV2 (Linux)

`faderframe-plugin-lv2`, hand-written C ABI (`sys.rs`, from the ISC
headers). The plugin is driven directly, without lilv.

* **Metadata.** It is Turtle (`ttl.rs`: `oxttl` into one graph per bundle,
  relative IRIs resolved per file).
  * `scan` reads a bundle's `manifest.ttl` and the files it points to:
    plugins, UIs, presets. A UI's binary is its `lv2:binary` (UI 2.x;
    JUCE writes it) or the older `ui:binary`; UI classes are the `ui:`
    ones and kxstudio's external UI (`external-ui#Widget`, or the older
    `ui#external`).
  * Nothing is loaded, so scanning needs no helper process. A cache of
    descriptions (`scan::Cache`, `lv2-scan.json`) is refreshed by `stat`
    unless a bundle's Turtle files changed.
  * Paths: `LV2_PATH`, else a portable `Plug-Ins/LV2`, `~/.lv2` and the
    system directories.
  * Preset bundles (a user's presets of another bundle's plugin) are
    merged into their plugin.
  * Ports become buses: the main input, the sidechain (`lv2:isSideChain`
    or a `pg:sideChainOf` group), output buses by port group (the main one
    first), else the first two ports and then pairs.
* **Instances** (`instance.rs`) start at 48 kHz/8192 frames, because the
  host learns the rate only at activation.
  * Another rate or larger blocks instantiates the plugin again, its state
    carried over.
  * Host features: URID map/unmap (one table per process), options (block
    sizes, sequence size, sample rate), bounded block length, the worker,
    `loadDefaultState` (the plugin's `state:state` restored after
    instantiation), and the log (formatted by a C shim, `log.c`: stable
    Rust has no variadic definitions).
  * Parameters are the input control ports, except the latency, enabled,
    free-wheeling and `notOnGUI`/`notAutomatic` ones. Their id is the port
    index.
  * Latency: the latency port is read from one block of silence after
    activation (then deactivate/activate). A later change asks for a
    restart.
* **State** (`state.rs`) is our own format, `FFL2`: port values by symbol
  plus the state extension's properties, with URIs instead of URIDs.
  * Paths are kept absolute (`mapPath` is the identity).
  * `restore` runs holding the core's cell and the worker's gate (it is
    an instantiation function).
  * Presets (`pset:Preset`) are the plugin's programs: port values plus a
    `state:state` node, typed from the literals' datatypes.
* **Processing** (`core.rs`, `Core` in a `TryCell` like the other
  formats). Controls are block-rate, so `run` is split at automation
  events: audio ports are connected at offsets, and atom inputs are
  rebuilt per piece.
  * MIDI and SysEx travel as atom sequences (`atom.rs`: preallocated,
    8-byte aligned).
  * `time:Position` goes to ports that support it whenever the transport
    changes or jumps.
  * Worker requests and responses use length-prefixed byte rings
    (`worker.rs`, a thread per instance); `end_run` is called after each
    `run`.
  * `reset` sends All Sound/Notes Off, because activation is not realtime
    safe.
  * Output control ports (meters, latency) are published as atomics.
  * No allocation on the audio thread (`tests/host.rs`, which drives a
    plugin written in the test itself).
* **Editors** (`ui.rs`): X11 UIs embedded with `ui:parent`
  (`ui::embeddable`), else external UIs in windows of their own
  (`ui::floating`, `can_float`/`open_floating`: the widget's `show`, its
  `run` on the host timer, `hide` before cleanup; the user closing it
  calls the host feature's `ui_closed`; `plugin_human_id` = the editor
  title).
  * Supported UI features: `ui:resize` (requests), `ui:touch` (gestures),
    `ui:portMap`, the idle interface on a 30 ms host timer, options
    (rate, scale), `instance-access` (the plugin's handle — JUCE's and
    Yoshimi's UIs need it), `data-access` (the descriptor's
    `extension_data`) and the external UI host (kxstudio's and the older
    URI).
  * The size comes from `ui:resize`, else from the UI window's X11
    geometry; the frame is made at 480 × 320 and follows the UI's first
    request (`Frame::request_size` before its first configure).
  * Control writes become `EditorEdit`s and atom messages travel through
    rings both ways. A UI may hold the instance, so re-instantiation
    (another rate or block size) cleans the UI up before the old instance
    goes and makes it again on the new one, where it was (`ui::Place`:
    the same parent window, or floating with its title).
  * A closed UI (idle answered non-zero, or `ui_closed`) is cleaned up
    after its callback returned (`Lv2Instance::idle_ui`).
  * FLTK UIs (Yoshimi) on Wayland would decorate through libdecor's GTK 3
    plugin, which cannot work in a process with GTK 4 (its calls bind to
    GTK 4's; the process hangs): `faderframe_ui::plugins::prepare_plugin_toolkits`,
    first in `main`, sets `FLTK_BACKEND=x11` while `DISPLAY` is set
    (helpers inherit it).
  * Tests: `tests/ui.rs` (UIs linked into the test through
    `Lv2Instance::with_ui_entry`: both binaries scanned, instance and
    data access, made again after a rate change, an external UI's
    show/run/close).
* The sandbox carries LV2 like the other formats (`Format::Lv2`). Tests:
  `tests/host.rs` (CI) and the opt-in `tests/bundles.rs`
  (`FADERFRAME_TEST_LV2_PATH`: the LV2 book's examples and DPF's). The
  installed-plugin tests and `faderframe-bench --plugin lv2:<uri>` take
  LV2 too.

### Editor windows

`faderframe-ui/src/plugin_window/` keeps the shared logic (opening,
placement, positions saved per instance, resize requests, closing, fd and
timer sources, the generic window, the editor header shared by every
editor window: name over vendor · track, Bypass following changes made
elsewhere, Presets, Outputs, the device's own undo and its A/B compare
(below), Parameters) apart from three backends with the
same `Parents` API: `x11.rs` (XWayland windows, RandR monitor geometry, the
screenshot capture), `win32.rs` (plain top-level Win32
windows with `WS_CLIPCHILDREN`; GTK's main loop dispatches their messages,
the window procedure records close requests and resizes for the next UI
tick; positions in physical pixels from the monitors' work areas) and
`cocoa.rs` (an `NSWindow` per editor, released only on destroy; closes and
content-size changes are noticed by polling on the UI tick; positions are
converted between GDK's top-left and Cocoa's bottom-left origin). The
plugin-host trait is platform-neutral: `can_embed(WindowApi)`,
`open_embedded(api, scale)`, `attach(ParentWindow)`.

Native editors get the header too (`frame.rs`): the editor window is a
GTK window with the header as its title bar and a `DrawingArea` room
under it; the plugin embeds into a native window of the backend's laid
over the room, which the UI tick keeps there (`Frame::follow`: the room's
place from `compute_point` + the surface transform, in backend pixels —
physical on X11 and Windows (× `Surface::scale`), points on macOS — what
plugins size their editors in). Backends add `frame_display`,
`frame_handle`, `frame_scale`, `create_child`, `place_child`,
`hide_child`, `release_child`, `release_top`, `move_to`, `set_dialog`,
`size`:

* X11: GTK on Wayland cannot hold another program's window, but GTK can
  open GDK's X11 display beside the Wayland one (`gdk_x11_display_open`
  and `gdk_x11_surface_get_xid`, looked up in the running GTK, so a GTK
  without X11 still runs). The frame is not one of the application's
  windows there (its `app` actions are inserted) and is styled through
  `style::add_display`, which also copies the desktop's decoration
  layout, font and icon theme. The embedding window is an X11 child of
  the frame's with the screen's default depth, visual and colormap (GTK's
  windows are often 32-bit ARGB, which plugins drawing with the default
  visual cannot use). The frame is transient for a hidden anchor window
  of that display (`frame::anchor`), so GTK marks it a dialog
  (`WM_TRANSIENT_FOR`, `_NET_WM_WINDOW_TYPE_DIALOG`) and window managers,
  tiling ones too, float it — a type set by hand GTK writes over with
  "normal" when it maps a window without a transient parent. On close the embedding window is reparented to the root and
  retired like the plain windows (`release_child`, with a round trip so
  the server has done that before GTK destroys its window).
* Windows: a `WS_CHILD` window of GTK's (from `Win32Surface::handle`,
  which gets `WS_CLIPCHILDREN`); GTK's DirectComposition target is made
  with `topmost = FALSE`, so its content stays behind child windows.
* macOS: GDK turns every mouse event in its own windows into a GDK
  event (GTK reports them all handled), so a view inside one would never
  get any; the embedding window is a borderless `RoomWindow` (an
  `NSWindow` subclass that can become key, for the plugin's text fields)
  attached as a child window above GTK's (`gdk_macos_surface_get_native_window`,
  GTK 4.8, retained in `Parents::tops`), placed in screen coordinates
  every tick (macOS moves child windows with their parent, but a resize
  from the top or left moves the room too), detached while the frame is
  hidden. Events of windows not GDK's go to Cocoa.

**The scale plugin GUIs are told** (`editor_scale`, passed to
`open_embedded`: CLAP `gui.set_scale`, VST3 `setContentScaleFactor` on
Windows and Linux, LV2 `ui:scaleFactor`; the sandbox forwards it) is how
large the desktop shows a pixel of the window they draw in (#54). Linux on
Wayland: the X11 size of the monitor the editor opens on (where it was
last, else the main window's; RandR by connector name) over its Wayland
logical size (`x11_ratio`) — 1 where the compositor scales X11 windows
itself, the monitor's scale where X11 programs draw at full resolution
(GNOME's native XWayland scaling, one scale for every monitor of the X11
screen; KDE's "applications scale themselves"; Hyprland's
`force_zero_scaling`). Else the backend's `scale()`: X11 sessions (and
monitors that cannot be matched) GTK's integer window scale × the
desktop's font DPI (`Xft/DPI` from XSETTINGS through GTK, else `Xft.dpi`
from `RESOURCE_MANAGER`) / 96 (`desktop_scale`), Windows the DPI, macOS 1
(points). An editor moved to a monitor of another scale is not told
again (where X11 has one scale per monitor: Hyprland's zero scaling).

On Windows and macOS the frame is an application window, transient for
the main window, like the device editors. Plugin resize requests set the
room's content size (fixed-size editors) or the window's default size
(resizable); a request made before the frame's first configure (an editor
asking while it starts) is asked again once the window has its size,
with a queued resize (`Frame::pending_size`: GDK resizes no X11 window
while its first configure is pending); a resizable editor takes the room's new size unless it is
within one GTK pixel of the size it has or asked for (never answer a
plugin's own resize with another). Frames are moved to their stored place
once shown (GTK cannot place windows; the backends can). The plugin
closes its GUI before its window and the frame go.
`FADERFRAME_EDITOR_HEADER=0` keeps the plain windows.

**Keep on Top.** A plugin's editor windows stay above every other window
while `Session::plugin_window_on_top` says so (`WorkspaceSet::plugin_windows_on_top`,
saved with the project like the positions; `Action::SetPluginWindowOnTop`,
not an undo step), set by the pin at the right of the editor header
(`on_top::button`, following changes made elsewhere) or the mixer insert
menu's "Keep Window on Top". `on_top::keep` (every sixth UI tick) brings
the open windows in line, asking each window once per change (and again
while one is not on screen yet, `PATIENCE`):

* Backends' `set_above(win, on)` / `set_surface_above(surface, on)` /
  `mapped`: Windows `SetWindowPos(HWND_TOPMOST | HWND_NOTOPMOST)` (the
  plugin's child window comes along); macOS the floating window level on
  GTK's `NSWindow` and the frame's child window; X11 the EWMH request to
  the root window (`_NET_WM_STATE` add/remove `_NET_WM_STATE_ABOVE`, source
  1), which X11 window managers and GNOME's, KDE's and most other Wayland
  compositors honour for XWayland windows (test
  `the_window_manager_is_asked_to_keep_a_window_above`, ignored: needs an
  X server).
* Wayland has no such request (a "keep above" protocol is only proposed,
  wayland-protocols MR 369): on the Native route a built-in editor or
  parameter window kept on top opens on GDK's X11 display instead
  (`on_top::display_for` in `editor_window`; an open one is reopened
  there, `reopen_on_its_display`), transient for the anchor like the
  frames.
* Hyprland reads no `_NET_WM_STATE_ABOVE` (its XWM lists no such atom)
  and pins instead (pinned floating windows are drawn above all, on every
  workspace): `crate::hyprland` over `$XDG_RUNTIME_DIR/hypr/<sig>/.socket.sock`
  finds FaderFrame's windows by pid and title in `j/clients` and floats
  and pins them — Hyprland 0.56's Lua dispatchers
  (`hl.dsp.window.pin({ action = "on", window = "address:…" })`) first,
  else the older `setfloating`/`pin` (`commands`, the syntax remembered).
  The route is chosen once (`on_top::route`; `FADERFRAME_ON_TOP=x11|hyprland`
  forces one).
* A plugin's own floating window: its host side keeps it where it owns
  it (`PluginEditor::set_above`; the sandbox's `EditorCall::SetAbove`, the
  macOS helper's window at the floating level); else on Linux it is found
  by title among the windows of FaderFrame and its helpers (Hyprland's
  list, or the window manager's `_NET_CLIENT_LIST` with `_NET_WM_PID`,
  `Parents::windows_titled`).
* Where nothing can (Wayland without XWayland, not Hyprland) the pin is
  insensitive and says why. Sway honours neither request; its floating
  windows (the editors are dialogs) stay above tiled ones.

Dev action `window-on-top:<on|off>` (the editor opened last,
`latest_editor`) or `<plugin id>=<on|off>`.

### A/B compare and a device's own undo

Every editor window's header — the built-in devices' canvases, the EQ,
the generic window, hosted plugins' framed editors — carries the device's
own Undo and Redo, A and B, Copy (A → B or B → A) and Match
(`faderframe-ui/src/device_states.rs`, following `Session::device_status`
every 200 ms; `Action::Device(DeviceOp)`, `session::device_states`). A
clip effect's own window has none (its settings are its card's).

**A/B.** The two sides are complete settings of the slot — its state and
the explicit parameter values applied over it (`DeviceSettings`). The
side heard is the slot's own; the other side and which side is heard are
a side table in the project (`Project::device_compare`, by slot,
`faderframe_project::device_compare`, so `PluginSlot` literals stay as
they are; written only when used; entries of slots that are gone are
kept while the session runs — undoing a removal brings the B back — and
dropped by `Project::repair` on opening). A switch is one undoable step:
the slot gets the other side's settings (`Command::SetPluginState`) and
the compare keeps those it had (`Command::SetDeviceCompare`, Impact
None), so undo, redo, saving and opening keep slot and compare together
— chosen over switching unrecorded, which would leave the inverses of
earlier parameter edits pointing at the other side's values; the cost is
a step per switch ("Compare B (EQ 1)"). The first switch makes B a copy
of A (nothing changes but the compare). A hosted plugin's live state is
taken into its slot first (`capture_plugin_state`: its own window may
have changed it unseen). Built-ins keep everything in the slot; a side
whose state is `None` (how the device was made) switches back exactly
because the engine loads a built-in's initial state when its slot's
state goes back to none (`plugins::Hosted::initial`), then the explicit
values.

**Match** (listening only): while the transport plays, the RMS level at
the device's track output is followed for the side heard
(`LEVEL_SECONDS` 3 s averaging, passages under −60 dBFS left out,
`SETTLE_SECONDS` 0.4 s after a switch ignored); once both sides are
measured for a second, side B is heard brought to A's level through the
monitor path's mix-side listening gain (`set_listening_mix_gain`, ±24 dB;
a Mix Intelligence audition keeps that gain for itself while it runs).
Meters and renders never see it.

**A device's own undo.** While an editor window shows a device
(`DeviceOp::Watch`, counted per window) the session keeps its history:
a snapshot of its settings and compare each time they change outside a
gesture — after every sync of the project (`follow_device_histories`)
and when a gesture ends — labelled by the step that made it, and, for a
hosted plugin, after moves in the plugin's own window end
(`EditorEdit::End`, or `EDITOR_IDLE` 400 ms after the last move: its live
state taken into the slot, "Plugin Window Edit"). A change equal to the
snapshot before or after the present is a move back or on (the project's
own undo and redo of device steps), anything else a new step (up to
`MAX_DEVICE_STEPS` 64). The device's Undo and Redo apply the snapshot
before or after as one step of the project's history ("Undo in EQ 1:
Change Plugin Parameter"), whatever was done elsewhere since; undo in the
project takes that back like any step. Window moves not taken in yet are
taken in first.

Tests: `session/tests/device_states.rs` (a built-in's explicit values,
a sampler's states, a CLAP plugin built with clack whose own window
changed it — each side back exactly, undo of a switch; Copy; the
device's undo beside the project's, a drag as one step, a switch undone
with its compare; a plugin whose window reports its moves; level
matching on and off), project `device_compare::tests`.

## 9. Audio backends

`faderframe_audio::AudioBackend` opens an `AudioStream` with a boxed
`AudioCallback`. Device buffers are exposed through the `DeviceBuffers` trait
(per-channel slices, no per-callback arrays), so backends need no
allocation in their process callbacks. Status (xruns, rate, buffer size,
shutdown, the latencies the backend learns while running) is shared through
lock-free `StreamMonitor` atomics.

What a backend lets the user choose is `AudioBackend::capabilities()`
(`Capabilities`: who owns the rate — `RateControl::{Server, Requested,
Device}` —, whether a buffer size or rate applies to the running stream, how
many output/input devices one stream takes, whether the backend keeps
devices of different clocks in step (`clocked_by_server`), whether it
reconnects devices that come and go by itself (`follows_devices`), free
channel counts, a forced format). `enumerate_devices()` lists devices
(`DeviceInfo`: id, name, channel counts *and names*, rates, buffer-size
range, a clock id, default flags); `StreamConfig::{output_devices,
input_devices}` name the devices to open (empty: the defaults), and a
running stream answers `routing()` (`StreamRouting`: what each of its
channels is connected to, devices missing now) and may take
`request_buffer_size`/`request_sample_rate` and a `maintain()` call about
once a second (control side only). `faderframe_audio::resolve` checks a
`Request` (the Preferences) against a backend's devices before anything
opens: a missing device falls back to the default (unless the backend
follows devices itself), a rate the device does not list falls back to its
own, a buffer size outside its range is moved inside it — each with a
sentence for a notice.

* `PipeWireBackend` (Linux) — one DSP node (`pw_filter`) with a mono float
  port per channel; inputs and outputs are processed in the same cycle on
  PipeWire's realtime data thread, so recording stays sample-aligned. A
  control thread runs a PipeWire main loop and owns every PipeWire object
  (the stream handle only holds that thread and a `pw::channel` to it). The
  node asks for the requested rate and quantum (`node.rate`,
  `node.latency`; with `force_format` it holds the graph at them with
  `node.force-rate`/`node.force-quantum`, node properties that end with
  the node — nothing in PipeWire's settings is written), is scheduled even
  when unlinked (`node.always-process`), and the callback re-prepares the
  engine when the graph's rate, quantum or the devices' latency changes. A
  running stream takes a buffer size or rate request through the channel
  (`pw_filter_update_properties`) without reopening. Devices are the
  graph's `Audio/Sink`, `Audio/Source` and `Audio/Duplex` nodes by
  `node.name` (`graph.rs`: the registry's nodes, ports and links, the
  `default` metadata's `default.audio.sink/source` and the `settings`
  metadata's rate/quantum limits). Auto-connect links the node to the
  devices asked for — several per direction, their channels one after
  another (`graph::plan`, pure and unit-tested) — or to the server's
  default sink and source, following the default when it changes. The
  links are brought in line with the graph every 100 ms after a change:
  a device that goes away keeps its channels' places (its last channel
  count), the default stands in when none of a direction's devices is
  there, and the links come back when it returns — an unplugged interface
  needs no restart. Only links this node made are removed
  (`registry.destroy_global`). The latency is what the graph reports on
  the node's own ports: they are bound as port proxies subscribed to their
  `SPA_PARAM_Latency` (an output port's playback side, an input port's
  capture side, as pipewire-jack maps JACK's ranges); on this machine both
  are one quantum, the same as the JACK backend reads through
  pipewire-jack. Raw `pw_filter` calls go through `pipewire-sys`; the rest
  uses pipewire-rs.
* `JackBackend` (Linux) — JACK2 or pipewire-jack, libjack loaded at
  runtime. The server owns rate and buffer size; FaderFrame follows (the
  session rebuilds the engine on a rate change) and can request a buffer
  size live. Devices are the clients owning physical ports (`plan.rs`:
  JACK2's `system`; under pipewire-jack one client per interface), the
  stream connects to the ones asked for (channels one after another) or
  the first one, and `maintain()` connects them again when a device
  returns and undoes connections of its own that are no longer wanted.
  Latencies are the ports' `jack_port_get_latency_range` (playback for
  outputs, capture for inputs), read in `maintain()`.
* `CpalBackend` — the system API through cpal: WASAPI (Windows), CoreAudio
  (macOS), ALSA (Linux). Playback drives the engine; capture arrives
  through a lock-free ring (a few milliseconds of extra input latency);
  `f32`, `i32` and `i16` devices. The output device is the one asked for
  (by cpal's device id, or name) or the default; the config opened is the
  fewest channels that give what the session wants, else the most there
  are (`pick_config`), so a multichannel interface whose default is stereo
  opens its other outputs for cue mixes and hardware inserts. Capture comes
  from the device asked for, else from the output device itself where it
  records (one clock), else the system's default input. Latencies come
  from cpal's callback timestamps (playback − callback beyond the buffer;
  callback − capture plus the ring's fill for inputs). More devices — a
  second output, an input other than the output device — join the stream
  as followers of the output device's clock (see "Several audio
  interfaces"). The streams live on a control thread, so the stream
  handle is `Send` everywhere; it also watches the followers and opens
  one again when its device is back. With the
  `asio` feature (Windows) the same backend drives ASIO drivers
  (`Api::Asio`, `Capabilities::input_devices == 0`: capture from the
  driver itself). It is opt-in because it compiles Steinberg's ASIO SDK,
  whose licence (proprietary or GPLv3) then applies to the binary; the
  *Ports* workflow checks it with MSVC. cpal exposes no ASIO control panel,
  so Preferences offers none.
* `DummyBackend` — timer-driven, silent; used when no audio device exists
  and in CI. `DummyBackend::with_devices(DummyDevices)` stands in for a
  machine's interfaces in tests (`mock_device`: rates, buffer range,
  channels; `unplug`/`plug` — a stream on an unplugged device shuts down
  like a pulled USB interface). Pretend devices join like the system
  API's: the first output device is the clock master, the others follow
  on threads of their own with clocks of their own
  (`DummyDevices::set_clock(id, PretendClock { ppm, jitter, block })`);
  an unplugged follower only falls silent and is attached again when it
  is plugged back. `DummyBackend::simulated(speed)` runs master and
  followers on a simulated clock (the followers' callbacks between the
  master's, on its thread, jitter from a seeded generator): minutes in
  seconds, the same result on a loaded machine. `with_input_pulses(s)`
  feeds 2 ms raised-cosine pulses every `s` seconds of each device's own
  clock — captured at the same moments by master and followers on the
  simulated clock, which is how per-channel latency is checked.
* `Aes67Backend` (`faderframe-audio-aes67`) — AES67 network streams on a
  PTP clock, as the audio system or beside another one's device (see
  "AES67 network audio" below). Never chosen by *Automatic*.

*Automatic* tries PipeWire, JACK, ALSA, then the dummy device on Linux, and
ASIO (where built in), the system API, then the dummy device elsewhere.

### The audio device in the session

`session::audio_device`: `AudioPreferences` (rate, buffer size, threads,
the devices chosen per backend id — `devices: BTreeMap<String,
DeviceChoice>` —, channel counts, `force_format`) are opened by
`Session::start_audio_with(BackendFactory, &prefs)` (the shell's
`BackendChoice::factory()`; `start_audio(backends, prefs)` stays for tests
that need no recovery). `open_audio` resolves each backend's choice
against its device list, tries in turn as resolved, at the device's own
format (a device refusing what it listed), and on the defaults (a device
gone between the list and the open), and reports what changed as notices
(`AudioDeviceStatus::notes`). Channels: as chosen, else as many as the
devices have (8 in / 2 out where unknown), never fewer than
`Session::required_channels()` (the master's bed, tracks on interface
outputs, hardware inserts, cue mixes, talkback; `check_channels` compares
against these, so choosing fewer channels does not cause reopen loops).

`Session::plan_audio(&prefs)` says what `apply_audio` will do: `Unchanged`,
`Live` (a buffer size on JACK/PipeWire/the dummy device, a rate request on
PipeWire, a rate on JACK = nothing to do; playback goes on) or `Restarted`
(anything else; the transport stops, the playhead stays); refused while
recording; settings that fail to open bring the previous ones back (the
error says so). Recovery: `tick_audio_device` notices a stream that shut down
(`AudioHealth::Reconnecting`), probes the backend on a thread of its own
every `PROBE_EVERY` (750 ms; a system API's enumeration can be slow) and
reopens the same devices as soon as they are back, or the defaults after
`FALLBACK_AFTER` (2 s; `AudioHealth::Fallback`, with a notice). On the
default it keeps probing and moves back to the chosen device when it
returns — at once when stopped, after the stop when playing ("… is back:
audio moves to it when playback stops"), never while recording.
`stop_audio` ends the watching. `stream_routing()`,
`input_channel_name(n)`/`output_channel_name(n)` give the device channel
names (for Preferences; available to routing menus). Tests
`session/tests/audio_device.rs` (pretend devices: channels and names, live
buffer size vs reopening rate, unsupported rate/buffer, unplug and return,
fallback and back after stop, missing at start-up, refused while
recording, a failed reopen restores the previous settings, stop ends the
watching), `faderframe-audio` `setup::tests` and
`dummy::tests`, `faderframe-audio-pipewire` `graph::tests` and the live
test (devices of the running server), `faderframe-audio-jack` `plan::tests`
and its live test, `faderframe-audio-cpal`
`the_config_opened_has_the_channels_wanted` and its live test (latency
printed).

Preferences → Audio (`faderframe-ui/src/audio_prefs.rs`) lists the
selected audio system's devices on a thread (the running backend first for
Automatic) and shows only what its `Capabilities` allow: output device and
a second one ("Also play on") where the backend takes several, input
device(s) (none for ASIO), sample rate (the device's rates for system APIs;
the server's rate as text for JACK), buffer size (within the device's range
for system APIs), channel counts, PipeWire's "hold the graph" option; a
note on what the audio system decides (PipeWire's allowed rates and
quantum limits from its `settings` metadata) and on devices of different
clocks. Apply says beforehand whether it applies live or reopens. *Running*
shows the stream, what was asked for against what runs, the latencies (in,
out, heard, recording compensation), a line per device of a joined stream
(the clock master; each follower's rate, drift, latency and gaps, or that
it is not connected — `joined_lines`), where each channel goes (device
port names) and a device that is missing or being reconnected. Settings persist
in `Preferences::{backend, sample_rate, buffer_size, threads,
audio_devices, audio_input_channels, audio_output_channels,
audio_force_format}`; a chosen device not connected now stays chosen and is
shown as "(not connected)". `preferences-page:audio@0.5` scrolls the page
(scripted screenshots).

### Several audio interfaces

* **PipeWire** (implemented): one FaderFrame node can be linked to any
  number of devices; PipeWire runs one graph with one driver, and the
  devices that follow it with a clock of their own (another USB interface)
  are resampled adaptively by their ALSA nodes, so they stay in step
  without drift. Preferences offers two devices per direction (the backend
  takes four); their channels follow one another on FaderFrame's ports.
  Cost: the followers' resampling (a little latency and CPU), and the
  latency PipeWire reports is the largest over the node's ports, so
  recording compensation uses the slower device's. Channel places come
  from the devices present when the stream opens; a device missing then
  gets its place once it returns and the session reopens the stream (when
  stopped).
* **JACK**: connections to any physical ports JACK sees. JACK2 drives one
  device (others need `alsa_in`/`alsa_out` or `zita-a2j` bridges, which
  resample — FaderFrame connects to their ports like any other);
  pipewire-jack exposes every PipeWire device, kept in step by PipeWire as
  above.
* **System APIs (WASAPI, CoreAudio, ALSA) and the dummy device**: any
  number of devices, joined natively (below). Preferences offers two per
  direction (the backend takes four). Input defaults to the output device
  itself where it records. macOS users can still make an aggregate device
  in Audio MIDI Setup (one CoreAudio device to us). ASIO is one driver =
  one device.

**Joined streams** (`faderframe_audio::follow`). One device is the clock
master: the first output device; its playback callback drives the engine,
and its own capture (same clock) comes through the plain capture ring as
before. Every other device — further outputs, inputs on other devices — is
a *follower*: one direction of one device (a device used both ways is two
followers, two loops measuring one clock), a stream of its own on its own
thread (cpal's callback thread, or the dummy's), at the master's rate where
it runs at it, else at its own. `follow::join` wraps the engine's callback
in a `Joined` callback the master drives; the stream's channels are the
master's and the followers' in the order the devices were asked for
(`Place::{Master, Follower}`), and the engine sees one device.

* **The ring and the stamp.** A follower's callback pushes its frames into
  an `rtrb` ring (recording; or takes them, playing) and publishes how many
  it has moved by when (`link::Stamp`: a sequence lock over atomics, the
  process's monotonic clock `follow::now_ns`). The master's callback
  estimates the fill *at its own moment* — the follower's count
  extrapolated from its stamp at its nominal rate, less what the master
  has taken, plus what the resampler holds past its read position — so
  the follower's block size and phase do not saw the estimate up and
  down; only the callbacks' timing jitter is left in it.
* **The drift loop** (`follow::drift::DriftLoop`): the fill error against
  the target, smoothed over 50 ms, through a critically damped
  proportional-integral loop whose integral part settles on the drift
  itself (so the fill settles on the target with no error left); the
  correction scales the resampler's step. Bandwidth 0.128 Hz for the first
  4 s (locking), then narrowing to 0.016 Hz over 10 s (PipeWire's ALSA
  followers use the same two): once settled, a callback whose timing is
  10 frames off moves the read position by a hundredth of a frame. The
  error is also averaged over 2 s (jitter out): that average is how far
  the fill is off its target (the latency reported counts it) and whether
  the loop has settled (`locked`: the fill within a few frames). The
  drift found carries over when the follower comes back. The correction
  is capped at 0.5 %.
* **The resampler** (`follow::resample::Resampler`): a polyphase
  Kaiser-windowed sinc (β 10, 40 taps a side at a ratio of one, more going
  down in rate, the cut-off at 92 % and the passband to 84 % of the lower
  Nyquist frequency), tabled at 512 fractional offsets and interpolated
  between the two nearest, each row normalised to unity gain; read at a
  fractional position advancing by the step. Worst error against an ideal
  sine read at the same positions, the step wobbling by 0.05 %: −114 dB at
  1 kHz (1:1, +150 ppm, 44.1 → 48, 96 → 48, 48 → 44.1 kHz), −97 dB at
  18 kHz. Allocated when made; `push`/`read` allocate nothing.
* **The target** is what both sides' blocks need with the resampler's
  look-ahead and a safety margin: recording, the follower's largest block,
  the master's block in follower frames, the resampler's 40 frames, 2 ms
  and a quarter block more; playing, the same without the look-ahead.
  The follower's largest block is measured as it runs (the spec gives a
  first guess).
* **Gaps, never stops.** A ring that runs dry (or over), and a follower
  whose stamp is older than 150 ms (or four of its blocks), silence only
  that follower's channels; the link waits for it, drops what is beyond
  the target, starts again with a 256-frame fade-in and keeps 2 ms more
  fill from then on (up to 50 ms). Gaps are counted per follower
  (`DeviceStatus::gaps`). The master is never stopped or reopened for a
  follower.
* **Hot-plug.** The stream's control side (`follow::Followers`) attaches a
  new link for a follower whose device is back (`Followers::attach`: new
  ring, resampler and loop seeded with the drift found; its device end for
  the new stream); the link travels to the master's callback through a
  small queue and the old one comes back through another, dropped on the
  control side (`collect`). The cpal backend's control thread notices a
  follower's stream failing (its error callback), drops it, looks for the
  device every second and opens it again; the dummy device's watch thread
  does the same for pretend devices plugged back. The session says once
  that a follower went away and that it is back (`tick_audio_device`).
* **Latency per device and channel.** A follower's input frame read in a
  master callback was captured `fill + device latency` follower frames
  before it (the fill counted from the resampler's read position, so it
  includes the look-ahead), while the master's own input is a buffer old:
  its channels' latency beyond the buffer is `(fill + device) ·
  master rate / follower rate − buffer`. Outputs alike, plus the
  resampler's delay. `AudioStream::devices()` → `DeviceStatus` per device
  (master first: channels, rate, latencies, drift, settled, gaps);
  `faderframe_audio::channel_input_latency` picks a channel's; the session
  compensates each take by its own channels' (`Session::input_latency_of`;
  also the A/V calibration's input). Outputs are not aligned across
  devices: a follower's outputs sound its output latency after the
  master's (Preferences shows it); the heard position and recording
  compensation follow the master's output.
* **Realtime.** `Joined::process`, `FollowerInput::capture_with` and
  `FollowerOutput::play_with` take no locks and allocate nothing (rings,
  resamplers and scratch made with the link); follower threads follow
  the audio thread a step below (`priority::follow_audio(Helper::Near)`),
  cpal's map their stacks on the first callback. The sequence lock's
  reader retries a few times and keeps its last stamp if a write keeps
  getting in the way.

Tests: `follow::tests` (the resampler's noise floor at five ratios and
18 kHz; frames in = frames out × step; a follower 120 ppm fast with
±0.5 ms jitter for ten simulated minutes — no slip in the three-term
recurrence of its sine, no gap, the drift found within 1 ppm; 44.1 kHz
and 96 kHz followers; the reported latency against a pulse's centroid
within a frame; a playing follower 50 ppm slow; a follower unplugged
for two seconds and attached again — the master's callbacks never stop,
its channels silent meanwhile, the drift carried over; devices
described with their channels), `tests/follow_alloc.rs` (master and
follower callbacks, links taken and handed back, a ring running dry
again and again: no allocation), engine
`joining_followers_to_the_engine_does_not_allocate`, dummy
`a_pretend_follower_records_and_comes_back_after_unplugging`,
`a_simulated_stream_runs_ahead_of_the_wall_clock`, session
`tests/multi_device.rs` (three simulated minutes of a 180 ppm fast
follower recorded through the session: no slip, no gap, its drift found
within 2 ppm; pulses captured at the same moments by the master's own
input and a 44.1 kHz follower 90 ppm slow land within 1.5 samples of each
other in their takes — rounding included; a follower unplugged while
playing: the master plays on, the notice, back without a reopen), ui
`the_joined_devices_are_told_one_a_line`. Preferences → Audio says which
device drives and shows a line per device under *Running* (rate,
drift, latency, gaps, not connected). Not done: aligning the outputs of
several devices to each other (a follower's outputs sound its latency
later), a follower missing when the stream opens (resolved to the
default as before; the session reopens when it is back and playback is
stopped), and devices joined under JACK (its own bridges do it).

### AES67 network audio

`faderframe-audio-aes67`: PCM streams over RTP on a PTPv2 clock — what
Dante devices exchange in their AES67 mode, and RAVENNA, Livewire+ and
most broadcast equipment natively. It is an audio system of its own
(`Aes67Backend`, "AES67 network audio" on the Audio page: the engine's
callback runs on a timer locked to the PTP media clock) or joins the device
of any other audio system (`Alongside`: the streams are followers of
`faderframe_audio::follow`, resampled onto that device's clock).
Everything is plain sockets (`socket2` for address reuse, the multicast
interface and DSCP; no `unsafe`), so it is the same on Linux, Windows and
macOS.

* **The clock** (`ptp`). Messages of IEEE 1588-2008 (`message`: header,
  Announce, Sync, Follow_Up, Delay_Req, Delay_Resp; others recognised and
  ignored). `port::Port` is one ordinary clock's port as a state machine
  without I/O — messages and timer ticks go in with local timestamps,
  messages to send come out — so the same code runs on sockets and in
  simulated networks. Best master (one port's 9.3): foreign masters kept
  while they announce (qualified after two Announces within four
  intervals, dropped after `announceReceiptTimeout`), the best by the data
  set comparison followed; a clock that may lead (`PtpSettings::lead`,
  priorities 250 so that any real grandmaster wins) leads when nobody
  better has announced for the timeout, and steps down when one does.
  Following: two-step Syncs matched with their Follow_Ups in either order
  (they arrive on different sockets), end-to-end delay requests at the
  master's `logMinDelayReqInterval` (randomised within twice it), the path
  delay the median of the last eight. Leading: Announce and two-step
  Sync/Follow_Up at the profile's intervals, a Delay_Resp for every
  Delay_Req. Profiles: the default (Sync every second) and AES67's media
  profile (every 125 ms; Announce every 2 s, timeout 3), on any domain.
  The time kept is a software clock over the process's monotonic clock
  (`clock::VirtualClock`: an anchor and a rate; nothing touches the system
  clock and nothing needs privileges but ports 319/320), published to
  other threads in a `clock::ClockMap` — two copies behind sequence locks,
  the writer filling the one readers are not directed to, so a writer
  interrupted half way never holds up the audio thread. `servo::Servo` is
  ptp4l's PI servo adapted to software timestamps: the first second of
  offsets is fitted with a line (frequency and phase; two samples are too
  rough), the clock steps once onto the master's time, then the loop
  steers the rate alone (wide for 16 s, then narrow; gains per second, so
  the Sync interval does not change its dynamics); lone offsets far
  outside the recent noise are late wake-ups and left out, unless they
  keep coming; an offset past 2 ms starts over (a new grandmaster). A
  clock that starts leading keeps the frequency it found (the integral,
  not the momentary correction), so followers see no jump. `steps`
  counts deliberate jumps; readers that count samples start again. The
  media clock is the PTP time in samples (`mediaclk:direct=0`;
  `media_frame`, `frame_ns`, RTP timestamps the low 32 bits,
  `unwrap_timestamp`). `service::PtpService` runs the port: a thread
  receiving event messages (timestamps taken the moment `recv` returns;
  it follows the audio thread a step below), one for general messages,
  one for the timers. Where the ports cannot be opened (an unprivileged
  program on Linux; another PTP daemon) the clock runs free on the system
  time and the status says how to allow it (`setcap
  cap_net_bind_service`, `net.ipv4.ip_unprivileged_port_start`). With
  `ClockSource::System` the same servo follows the system clock instead
  (TAI = UTC + 37 s), for machines where linuxptp's ptp4l and phc2sys keep
  it on PTP with the network card's hardware timestamps — more precise
  than any software timestamp.
* **Streams** (`rtp`, `sdp`, `sap`, `stream`, `ring`). L16/L24 at 44.1,
  48 or 96 kHz, packet times 125 µs, 250 µs, 333 µs, 1 ms (default) and
  4 ms, at most 1440 payload bytes a packet. `sdp::StreamDescription`
  reads what Dante's AES67 mode, RAVENNA and others write (session- or
  media-level `c=`, `ts-refclk` with grandmaster and domain or
  `traceable`, `mediaclk` offsets, `source-filter`, `clock-domain`,
  `framecount`, the first audio stream of several) and writes them back;
  `key()` (origin host and session id) follows one stream across
  versions. `sap` reads and writes announcements and keeps a `Directory`
  (a stream forgotten when deleted or silent for ten of its intervals);
  `discovery::Discovery` listens on 239.255.255.255:9875 and announces
  FaderFrame's streams every 30 s, a deletion when one ends. The audio
  side and the network side meet in a `ring::MediaRing`: frames indexed
  by media frame, each slot tagged with the frame it holds, so a receiver
  writes a packet where its timestamp says and the audio side reads the
  frames whose time has come — a lost packet, or a frame from a lap ago,
  reads as silence and is counted; neither side waits for the other.
  `stream::Receiver` (a thread per stream: joined on the interface — on
  Linux bound to its group with `IP_MULTICAST_ALL` off, elsewhere
  checking the sender — packets placed within a window around now; lost,
  late and outside packets and the latest/worst arrival counted) and
  `stream::Sender` (a thread per stream sending a packet once its last
  frame's time has passed, at the media clock's pace; frames the audio
  side did not write go out as silence and are counted). Both follow the
  audio thread a step below (`Helper::Near`).
* **As the audio system** (`backend`). Callback k runs when the media
  clock reaches `N` (advancing exactly one buffer `B` a callback): its
  inputs are the frames stamped `[N − L − B, N − L)` (`L` the link
  offset), so every frame is played `L` after its timestamp as AES67
  receivers do, whatever the jitter within `L`; its outputs are stamped
  `[N + B, N + 2B)` — the block's deadline is `N + B` — so senders pace
  packets evenly and never send a frame before it was computed.
  Receivers elsewhere play them their own link offset later; the stream
  reports `L` both ways (`StreamInfo::{input_latency, output_latency}`;
  the network's receivers are taken to use the same). A loop of our own
  stream comes back `2B + L` later, frame for frame. The timer thread is
  the audio thread (made realtime, it sleeps until the next callback's
  media time; `Clocked::block` reads the rings, runs the engine and fills
  the send rings — no allocation, no locks); a clock step starts the count
  again (an xrun), as does falling far behind. The stream's channels are
  the received streams in order, then as many silent ones as asked for;
  outputs likewise. `AudioStream::devices` lists the PTP clock first (the
  master) and every stream (present = packets arriving; gaps = lost and
  late packets), so the session's notices say when a stream goes away
  and comes back; `routing` names channels "stream · n"; `maintain`
  announces again under a new grandmaster and follows a received stream
  whose announcement moved it (another group or port).
* **Beside another device** (`alongside`). `Alongside::wrap_all` wraps
  every backend of a choice when `Aes67Settings::alongside` is on. The
  inner stream opens first with a `Forward` callback (silence until the
  joined callback arrives through a queue: `follow::join` needs the
  device's real rate and buffer); the streams' channels follow the
  device's. One pacer thread on the media clock hands each received
  stream's frames to its follower input when their playout time comes,
  stamped with the local time that frame boundary was reached — the drift
  loop measures the PTP clock against the device's without the pacer's
  wake-up jitter — and takes each sent stream's frames from its follower
  output 2 ms ahead of the media clock into its send ring. Device
  latencies: the link offset (receiving), the lead plus the receivers'
  link offset (sending); `follow` adds its own ring and resampler, so
  `devices()` reports each stream's latency and drift like any follower's.
  When the device's rate changes while running, `maintain` re-attaches
  every follower at the new rate (`Followers::set_master`, added for it;
  `follow::tests::a_master_whose_rate_changes_takes_its_followers_along`).
* **Settings** (`settings::Aes67Settings`, `Preferences::aes67`):
  interface (`None`: the route to 239.69.0.0/16, found by connecting a UDP
  socket — nothing is sent), PTP domain/profile/lead/clock source, rate,
  link offset, packet time and format of what is sent, send streams
  (name, channels, a group or 239.69.x.y chosen from the interface's
  address — `net::default_group`), receive streams (by key, with their
  description kept so they open without hearing the announcement again;
  a newer announced description wins), alongside, announce, and the
  standard ports/groups (`NetSettings`, moved only by tests). The shell
  hands them over with `faderframe_audio_aes67::configure` (in
  `AppState::start_audio`); the PTP service and discovery are shared by
  everything in the process with equal settings and run while something
  holds them (`services`, `discovery_for`: a stream, the Preferences page);
  a clock its last stream let go of runs on for `LINGER_SECONDS` (30), so
  a reopened stream finds it still locked.

Preferences → Network Audio (`faderframe-ui/src/aes67_prefs.rs`):
interface, rate, link offset, packet time and format; the clock (source,
domain, profile, lead) and its state (grandmaster, offset, path delay,
drift, why the ports are closed); the streams to send (name, channels,
group); the streams announced on the network to receive (ticked = the
engine's inputs, in order) and one added from a pasted description; "beside
the audio system" and announcing. Apply saves and reopens the audio stream
when the streams are in use. `--backend aes67` on the command line.

Not done: RAVENNA's own discovery (mDNS/DNS-SD and RTSP for the
description) — RAVENNA devices that announce with SAP (most do, for AES67)
are found; unicast PTP and peer-to-peer delay; kernel or hardware
timestamps in FaderFrame's own clock (the system-clock source covers
hardware timestamps through linuxptp); RTCP; aligning several senders'
outputs (each receiver plays its own link offset); AM824 (AES3
transparent) payloads; SMPTE ST 2110-30 conformance was not checked. NDI
is out: its SDK comes only under the vendor's proprietary licence
agreement, which an MIT program cannot take in (as MPEG-H, above). Dante
itself is proprietary (Audinate's); Dante devices take part through their
AES67 mode.

Tests: `ptp::message` (round trips, a captured Sync, refused packets),
`ptp::clock` (continuity, the shared clock, media frames and RTP
timestamps), `ptp::servo` (the delay median; a clock 80 ppm fast and 3 ms
off with 20 µs of noise and late wake-ups: one step, the drift within
0.5 ppm, within 30 µs), `ptp::port::tests` on simulated networks (150 µs
paths with 40 µs of jitter, software timestamps late 10–60 µs and 2 % of
them 0.9 ms: a follower 60 ppm fast locks within 60 µs of the master and
finds its drift within 1 ppm; in the default profile one 35 ppm slow; the best clock leads, a
real grandmaster takes over, the fallback leads again when it goes, each
follower stepping once; a lone follower listens; other domains are not
heard), `ptp::service` (two clocks on the loopback interface lock; the
system clock followed), `rtp`, `sdp` (Dante, RAVENNA and a plain unicast
description, written and read back, refusals), `sap` (packets, the
directory), `discovery` (one service hears what another announces, a new
version, a withdrawal), `ring`, `net` (multicast on loopback, two
listeners), `settings`; `tests/loopback.rs` (on the loopback interface
with TTL 0, nothing leaves the machine: a sent stream comes back as a
received one frame for frame `2B + L` later with AES67 as the audio
system; joined beside a realtime test device the stream comes back
through two followers at full level with no frame lost or repeated),
`tests/rt_alloc.rs` (the audio thread's block: rings read and written,
frames missing, a buffer size change — no allocation), engine
`aes67_streams_to_the_engine_do_not_allocate`, ui
`scripts_change_the_settings`, `stream_lines_say_what_and_where`.
Checked by hand against hardware: nothing yet (no AES67 device here) —
`aes67:status` logs the clock and the streams heard; a Dante device in
AES67 mode or `ptp4l -i <if> -m` (as master, `-S` software stamps) on
another machine with an AES67 receiver (Merging's ALSA RAVENNA driver,
`aes67-linux-daemon`) is the way to check.

## 10. Session

`faderframe_session::Session` is the single mutation point for the GUI:
`dispatch(Action)` covers edits, gestures, undo/redo, transport, workspace
changes, selection, editor focus, track creation and editor settings.
`tick(dt)` (once per UI frame) polls transport position and meters (with
ballistics, peak hold and clip latching on the UI side) and reacts to stream
status (rate change, server shutdown). `render::start` runs offline bounces
(master, stems or one track — `RenderSource::Track`, soloed through the
mix as its stem would be; range, rate, channels, tail, normalise, dither)
on a worker thread through the same engine path.

**Render destinations** (`render::files`). Every file a render writes
goes to a temporary name beside its destination (`.<name>.ffrender-<pid>-<n>.<ext>`,
claimed with `create_new` before rendering, so an unwritable folder fails
at once) and moves into place only when the whole render — every stem —
has been written and measured: `Staged::commit`, a rename on the same
file system, so an existing file is replaced in one step or not at all.
`RenderSettings::overwrite` says what may be replaced: `Never` (the
default), `Confirmed(paths)` (what the user agreed to) or `Always` (files
the session names itself: freezes, bounces, merges, clip effects, samples,
the movie export's scratch WAV — no question for those). It is checked
again right before the move: a destination it does not allow that is
taken then (it appeared while rendering) ends the job with
`RenderError::Exists(Staged)` — the render kept aside for the UI to ask
and `commit` or `discard`. Moving without replacing is a hard link (fails
atomically when the name is taken, POSIX and NTFS; without hard links,
FAT and some shares, a check and a rename); replacing is `fs::rename`
(POSIX rename; on Windows a replacing rename, which fails while another
program holds the old file open — the old file stays). Cancelled and
failed renders drop their `StagedFile`s, which remove the temporaries;
ADM and IAMF masters go the same way. `render::destinations` lists the
files beforehand (stems of tracks with equal names get " (2)", …), so the
Render window asks one question (Overwrite / Cancel) for all that exist
when Render is pressed (also after a save dialog asked: the file may have
changed since), and asks once more for a file that appeared meanwhile.
A folder of files (a DDP fileset, a vinyl premaster) is staged as a
temporary folder (`StagedFile::new_dir`) and replaces its destination as a
whole: no rename replaces a folder holding files, so the old one is
renamed aside under a temporary name, the new one renamed in and the old
one removed (renamed back when the new one cannot move). Staging into a
folder first sweeps temporaries killed processes left there
(`files::sweep_stale`): never this process's, never one touched within
`STALE_AFTER` (1 h — a sandboxed copy in another PID namespace may still
wait on it), of a PID that no longer runs where the system says so
(Linux `/proc`), elsewhere once `STALE_UNKNOWN_AFTER` (a day) old.

**Exports** (`session::exports`) put every other user-chosen destination
on the same staging: the album (song files, album file and cue sheet, the
DDP fileset, the vinyl premaster folder — `Session::album_destinations`),
the movie export's muxed file, Make Sample → File, lead sheets, lyrics
(`.lrc` and `.srt` together), MIDI and MIDI 2.0 files and track preset
exports. `Session::export_to(what, dests, start)` asks first about the
destinations there now — one `UiRequest::AskReplace(ReplaceQuestion)`
listing all of them, answered by `Action::AnswerReplace` (the shell shows
the Render window's Overwrite / Cancel dialog, `render_report::ask_replace`;
folders are named "replaced as a whole") — and starts the job with what
may be replaced; `commit_export` moves a finished job's `Staged` files and,
for a destination that appeared meanwhile, asks again (`appeared`) while
they wait beside it (Cancel discards them); `write_export` does both for
small files written at once. A save dialog that already asked (GTK's file
choosers always ask about a file that is there) is not asked again: the
shell passes its path with `Session::agree_to_replace` around the one
dispatch (`AppState::dispatch_chosen`, the canvas host's Save chooser,
`dialogs::chooser_agreed`; not when it added an extension). Answers to
questions that are gone are ignored; quitting drops waiting `Staged`
files, which remove their temporaries. Library saves (track and plugin
presets under names the session chose, asked about by name) and the
session's own files (freezes, bounces, scratch renders) are not asked
about. Tests: `session/tests/exports.rs` (MIDI asked, cancelled, replaced,
agreed; lyrics in one question; a track preset; an album asked first with
its DDP folder replaced whole and a killed export's temporary swept; a
file appearing while the album is written; a sample file), the movie in
`session/tests/video.rs`, `render::files::tests` (folders, the sweep).
Dev action `answer-question:<overwrite|cancel>` answers the open question
as its button would.

**Render statistics** (`render::stats`, on `faderframe_analysis::levels`).
With `RenderSettings::report` each written file is read back from disk on
the render's worker (between writing and committing; `RenderProgress::measuring`)
and measured in 64k-frame pieces: header facts (container, encoding, rate,
channels, the channel mask's layout, frames, size), per channel sample
peak, true peak (4×), RMS, samples at full scale (the format's largest
code), samples past 0 dBFS (float), overs (runs of 3), and BS.1770 loudness
over all channels with each speaker's weight (`WeightedLoudness`, the
streaming form of `integrated_weighted` with the meter's gating, windows
and range: integrated, LRA, maxima). An ADM master gets peaks but no
loudness (only a renderer places its objects); an IAMF file is not
decoded here, so its figures are the encoder's (`from_file` false) and it
has no levels — each says so in `FileStats::notes`. `Rendered::stats`
holds them (`RenderSummary` adds a render's files up), `Rendered::finished`
only what level processing did. The Render window shows them in a report
(`faderframe-ui/src/render_report.rs`: a section per file, warnings for
clipping, overs and true peaks over 0 dBTP, Show in Folder via
`gtk::FileLauncher::open_containing_folder`, falling back to opening the
folder); cancelled and failed renders say so instead (no report). The
album export measures its written files the same way on its worker
(`album::measured`: each song file and the album file — render time = its
pass-1 render plus its pass-2 levelling and writing — and each vinyl side,
read back from the staged files before they move; `AlbumExport::report`)
and the session asks the shell to show them (`UiRequest::RenderReport`,
the same report window; the DDP image is verified by reading the fileset
back, not measured). Tests: `session/tests/render_files.rs`,
`render::files::tests`, `faderframe-analysis` `levels::tests`,
`the_streaming_weighted_measurement_reads_as_the_meter`, the album's in
`session/tests/exports.rs` and `album.rs` (vinyl sides). Dev actions
`render-track:<track>` (the window for one track), `render-now:[<path>|x]`
(press Render), `render-stop:x` (press Stop). File → Render Selected
Track… (`app.render-selected-track`).

Delivery (`session::delivery`, on `faderframe_analysis::delivery`): a
render's `Finish` normalises the whole file to an integrated loudness
target and keeps the true peak under a ceiling — a lookahead limiter
(min-hold of the needed gain over 1.5 ms, then a box filter of the same
length, so the gain is down in time and ramps smoothly; 60 ms release;
repeated where the reconstructed signal still overshoots), the gain refined
until the limited result meets the target (`Rendered::finished`: the
gain, the limiting and the audio's measurement before writing) — and every
written file is read back and measured (`Rendered::stats`, above). Dither (`faderframe_audio_files::Dither`)
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

**CD-Text languages.** The release's text may come in up to eight
languages: `AlbumInfo::language` is the main one (CD-Text block 0,
English by default) and `AlbumInfo::translations` the others — per
language the album's title and credits and the songs' (`SongText` by song
id), a field left empty being the main text's (`album::fall_back`). The
Details form (File → Album → Release…/a song's Details) chooses the
language shown, adds and removes languages and sets the main one;
`AlbumAction::Texts` saves the release and a song in one step and drops
duplicate languages. `album_master::disc` turns them into
`faderframe_disc::TextBlock`s; `cdtext::packs` writes a block per
language (sequence numbers from 0 in each, the block number in bits 4–6
of the fourth header byte, three size packs per block with its character
code and the last sequence numbers and languages of all blocks — after
libburn's description of MMC-3 Annex J and Sony's format). Latin-script
languages are ISO 8859-1; Japanese is MS-JIS, double-byte (bit 7, two
NULs end a string), every character two bytes (ASCII full-width) so the
stream stays on character boundaries. `cdtext::decode_blocks` reads every
block (`ddp::read` fills `Disc::text_language`/`more_text`). Checked
against libcdio's parser (`cdtext_data_init`): all blocks, languages and
Japanese text decode; libcdio takes the character code of the last
block's size information for every block, so it misreads a Latin-1 block
when a Japanese one follows. Cue sheets carry the main language only.
Validation: at most eight languages, none twice, 256 packs per block.

**The DDP player** (`session::ddp`, view `faderframe-view-ddp`,
`ViewKind::Ddp`, File → DDP Player) opens any DDP fileset on a worker:
`ddp::inspect` (sizes, PQ against the image, CD-Text CRCs and every
block, `CHECKSUM.MD5`) with the Red Book rules reported instead of
refused, the image's byte order found by its sound
(`ddp::image_big_endian`: the order with the smaller sum of sample steps),
an overview of its peaks, and the image converted to a float file at the
engine's rate that the engine plays instead of the project
(`engine::preview`, as album playback; the two never play at once, and the
project starting takes the outputs back). `DdpDisc::locate` turns a
sector into track, index and the time from index 01 — negative in a
pregap, as a CD player counts. Import (`DdpAction::Import`) writes each
track's samples unchanged into a 16-bit WAV in the media folder (a pregap
with more than dither noise — 8 LSB — stays with the track before; a
silent one becomes the song's pause) and adds the songs with their titles,
credits and ISRC, the UPC and every CD-Text language, as one step; the
first save moves those files into the project with the album following
(`remap_album_files`). Dev action `ddp:<open=<folder>|play|pause|stop|
next|previous|import|close>`. Tests: `session/tests/ddp_player.rs` (a
master FaderFrame wrote: checked, played by track, imported bit for bit,
saved; a byte-swapped image recognised).
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

### Delivery QC: null-tested stems and the QC report

A delivery is checked as it was written: every file of a render or an
album export is read back once (`render::stats`), and the same reading
that measures the levels (`faderframe_analysis::levels::FileMeter`, now
also where the sample and true peaks are, each channel's DC offset and
the reconstructed overs — runs of the 4× interpolated signal past 0 dBFS)
feeds `faderframe_analysis::qc::QcMeter`: silence at the head and the tail
and gaps of digital silence of a second or more inside (silence is every
channel within the dither floor, `qc::silence_floor` — 16 LSB of a
fixed-point file, as noise-shaped dither peaks at about 10; −120 dBFS in
floating point), runs of three samples at full scale with where they are,
and for the first two channels the correlation over the file and in
400 ms windows louder than −50 dBFS, the share of those leaning out of
phase, and the BS.1770 loudness of the mono fold-down ((L + R) / 2 on both
sides) against the stereo's. `FileStats::qc` keeps it, so a report for
any spec needs no second reading.

**Checks** (`faderframe-qc`, pure): `spec::SPECS` are the presets — EBU
R128 (and s1 for short form), ATSC A/85, Netflix, streaming (Spotify,
YouTube, Tidal at −14 LUFS), Apple Music, Amazon Music, CD Red Book,
ARIB TR-B32, OP-59, and general checks only — each rule saying whether
missing it fails or warns (broadcast loudness fails; a streaming target
warns: the service turns the file). `check::checks(facts, levels, spec,
role)` gives every check a verdict (pass, warn, fail, or info: measured,
nothing asked) with the measured value and the requirement. A *programme*
(a master render, an album's songs and album file, a null test's mix) is
judged on the spec's loudness and ceiling; a *stem* or a *part* (a vinyl
side, the nulling stem) lists them as information. Every file also gets
clipping (runs fail), inter-sample overs (warn), DC offset (warns above
−50 dBFS), silence (a silent programme fails), gaps (warn) and phase and
mono compatibility (correlation under 0 or a fold-down 9 LU down fails,
a 400 ms window under −0.5 or 4.5 LU down warns). Netflix's loudness is
dialogue-gated; FaderFrame measures programme loudness (BS.1770-4 gated)
without dialogue gating, so that check warns but never fails and the
report says so.

**The null test** (`render::null`, `StemOptions::null_test`). The stems
that make up the mix are those of tracks no other track with audio feeds
(`stems_summed`): a bus's, a return's or a folder bus's stem is soloed in
place with its sources and holds them again, so it is left out of the sum
and named with the reason. The render job renders the mix first — the same
range and settings, every solo off — and writes it beside the stems as
they are written (a staged file never moved into place), then reads the
stems and the mix back in pieces: `qc::NullMeter` measures the residual of
their sum (in double precision) against the mix — peak, RMS, how far under
the mix, where it first rises above what rounding allows and where it is
worst, in the range and in the tail. The outcome (`null::outcome`) is
against what the files' format leaves of a perfect null
(`Precision::bound`: half an LSB per file, an LSB more with TPDF dither,
about ten with noise shaping, never under the −100 dBFS of floating-point
arithmetic through a chain): *exact*, *to the dither* (both pass), *near*
(under −60 dBFS: inaudible within the mix, a warning) or *fails*. Causes
come from the project before the render — the Render window shows them as
soon as Stems is chosen — and from the residual after it
(`render::null::causes`, `NullTest::new`): a non-linear built-in several
stems pass through (`null::nonlinearity`: compressors, limiters, gates,
de-essers, the channel strip, saturation, tape, the 2A and 76, preamps
and console buses, the Program EQ's valve stage; linear ones — EQ, delay,
reverb, modulation, utility — sum like their parts), a plug-in there
(FaderFrame cannot see into it), a device keyed by a bus or return (a key
is taken before its track's fader, mute and solo, so a track playing its
own clips keys a stem as it keys the mix, while a bus's sources are
silenced by the solo), per-stem normalisation or loudness finishing, a
track fed by others that plays clips of its own, an offset in time
(`qc::best_lag` over a 16 384-frame excerpt round the worst place,
±2048 frames, taken when it removes three quarters of the residual), a
difference only in the tail, and each file's dither as a note. Nothing
else to blame is said as such (something renders differently each time).
The job hands the test over in `RenderProgress::null_test`
(`RenderJob::null_test`). A stems render made without the test can be
tested later (`Action::NullTestStems`): the delivery keeps the project
copy the stems were rendered from and the stem files by track, and
`null::test_written` renders the mix from that copy and compares it with
the files as they are.

**Stems that null** (`StemOptions::shared_stem`): only the stems that make
up the mix are written; the mix's level processing (normalisation, the
finish's loudness gain) is worked out on the mix and its gain applied to
every stem alike (`finish_linked`), and what still differs — the shared
processing's doing and the finish's limiting — is written as one more
stem, `"<project> - Shared Processing.wav"`: the mix as written less the
stems' sum, in the format's codes without dither
(`WavWriter::write_codes`), so the files sum to the mix to the last bit
where the codes fit (`stems_made_to_null_sum_to_a_master_render_of_the_same_settings`
holds them against a master render of the same settings within half an
LSB at 24 bits). Detectors keyed by the full mix — the master's limiter
acting on each stem as it acts on the sum — would leave the shared stem
smaller but need the whole mix beside every stem in one graph; the
difference stem is exact whatever the processing (saturation too) and
keeps the stems free of each other.

**The report** (`faderframe_qc::report::QcReport`, schema
`faderframe.qc/1`): the project (name, file, program version, file
format), the date (UTC), the spec with its source and notes, the render's
settings, the verdict and counts, per file its facts, levels, checks,
notes and SHA-256 (`faderframe_qc::sha256`, FIPS 180-4 — the render
receipt, as `sha256sum` prints it), the delivery's other files as receipts
(an album's cue sheet, its DDP fileset's files, the vinyl cutting sheet),
the null test with the mix checked as the programme, and how everything
was measured. Levels that do not exist are `null`, never a number. The
PDF (`faderframe_qc::pdf`) lays it out on A4 pages for the lead sheet's
writer (`faderframe_leadsheet::pdf::write` — generic enough: filled
outlines, lines and text in the standard fonts; the lead sheet's output is
untouched): an overview, the null test, a section per file with its check
table, receipts and the method; characters WinAnsi lacks are spelt out
(`pdf::safe`). In the session (`session::qc`) the last delivery is kept
(`Session::delivered`, noted by the shell as a render's report opens; an
album export notes its own), `Session::qc_report(spec)` is the report as
it reads now (no receipts) and `Action::SaveQcReport { path, spec }`
hashes the files on a worker (reading any the delivery did not measure),
then writes the PDF and the JSON beside each other through the exports'
staging (asked before replacing; a save dialog's answer is kept for when
it is written). An album with a spec (`AlbumSettings::qc_spec`) writes its
report into the export folder with every export.

UI: the Render window's Stems row ("Null-test them against the mix",
"Make them null") with the causes it foresees; the render report window's
Spec choice, verdict, null test section, Null Test (untested stems) and
Save QC Report…; the album view's QC button (the export's spec, the last
export's report). Dev actions `qc-report:<path>[@<spec id>]`,
`null-test:<x|nulling|off>`. Tests: `faderframe-qc` (SHA-256 vectors,
dates, every spec's checks, the null outcomes and causes, the JSON values,
the PDF's structure), `faderframe_analysis::{levels, qc}` (positions,
reconstructed overs, silence and gaps, clipped runs, phase and fold-down,
residuals, an offset found), `session/tests/qc.rs` (stems that null in
float and to 16-bit dither; a bus left out of the sum; a master limiter
named before and after; a key from a track of its own nulls, from a bus
not; stems made to null against a master render; a
later null test; the report's JSON — receipts, levels, verdicts — and a PDF
`pdftotext` reads), the album view's QC menu. `FADERFRAME_QC_OUT=<dir>`
(the crate) and `FADERFRAME_QC_SESSION_OUT=<dir>` (the session test) keep
sample reports.

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
their key: a change is hit on its bar or its name (click: roots and
scales, drag: moves it by bars, `harmony::move_key`; double-click: type
one); a click on any other bar adds a change there, a drag across bars
draws a passage in another key (`harmony::set_key_span`: changes inside
give way, the key in effect after it comes back at its end). The Chords
lane shows the chord track (drag across for a chord and type it,
double-click for one a bar, drag to move, edges to resize, right-click for
the key's diatonic chords with their numerals, Delete Chord, Delete All
Chords; colours by root round the circle of fifths).

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

Renders start where their range does: `render_one` renders the graph's
output latency (plugins, listening) more and drops it from the front, so
an export with a lookahead limiter on the master lines up with the
timeline, and bounces, clip effects and samples need no offsets. A freeze
alone keeps the latency in its file (`RenderSettings::keep_latency`,
`Freeze::latency`) and the frozen player claims it: everything after the
track (time-varying devices on buses, automation) sees the audio exactly
as before, so freezing is bit-exact (`renders_take_the_latency_off_the_front`,
`freezing_keeps_the_sound_and_blocks_edits_until_unfrozen`).

### Self-freezing

Tracks nobody touches play from a disk cache of their chain's output, so
a heavy project plays on a small machine. It is a freeze made
automatically and invisibly: nothing in the project changes, any edit
sends the track back to its chain instead of being refused, and the
plugins stay loaded.

* **What is cached.** A track's chain — clips, MIDI effects, instrument,
  inserts — up to its strip; fader, pan, sends, mute and polarity stay
  live. `self_freeze::job::trimmed` renders the track and what reaches its
  chain alone (the MIDI tracks playing it as muted as they are, the
  tracks keying its sidechains or followed by its modulators muted: they
  still key) at unity into a plain master (mono for a mono chain), at the
  engine's rate and callback size, the graph's latency kept at the front,
  streamed into a float WAV. The render goes on until the chain has been
  silent for half a second after its last clip; a chain that never falls
  silent (a generator, hiss) is never cached — a file could not stand for
  it outside its span.
* **Playing it** (`faderframe_engine::cache`). `EngineController::
  set_cached` gives a track a `CachedChain`; the graph builds a
  `nodes::CachedPlayer` in place of its sources and chain (in the
  render-ahead graph too, where the chain would be). It reports the
  latency the chain has *live* (`track_latency`, buffered devices
  included), so delay compensation and everything downstream are as
  before, and reads the file the way a chain's output follows its input:
  at output frame `t` what the chain made of the input played at
  `history(t − latency)` — the play history — so loop wraps, locates and
  stops behind latency land exactly as they do behind the chain. The
  timeline snapshot carries the files' regions, so the disk loader keeps
  their pages resident (and a cached track's clips too: it may go back any
  moment).
* **Switching never jumps.** A track going to its cache, or back, passes
  through a handover graph holding both, the chain and the player into a
  `nodes::Handover` that crossfades in 20 ms (`FADE_SECONDS`): exactly one
  side at either end, `live + g · (cached − live)` between, so exactly
  either wherever both agree — a cache of a deterministic chain taken over
  while playing on is sample-exact. The fade to the cache waits for a
  block with every page resident; `settle_cache` then lets the next graph
  drop the side no longer heard. A chain coming back resets its plugins
  before its first block (`NoteLedger::mark_stale` → `PluginNode::reset`:
  they held whatever they had when it left) and fades in once its latency
  is filled; coming back *gently* (`release_cache`: nothing changed — an
  editor opened, the track armed) it first plays through its devices'
  tails (up to `MAX_WARM_SECONDS`) while the cache, still right, is
  heard, so a reset delay line's first repeat never sounds.
* **Exactly what the chain makes** (`self_freeze::key`). A cache is
  played only while the project still makes what it was made from: every
  `Session::sync` compares each cached track's `ChainView` with the
  project in place (clips, media, devices and their state blobs, the
  automation and modulators driving them, the MIDI tracks playing it with
  their effective mute, sidechain and follower sources recursively, tempo,
  key and chords, project and engine rate, the plugins' warm-up where a
  third-party plugin is among the chains) and a difference sends the
  track back in that very sync, before the edit is heard. The view's key
  (a hash) names the cache file, so undo, or reopening the project, finds
  the old file again without rendering. The comparisons destructure
  `Track` and `Clip` field by field: a new field must be placed on one
  side before this compiles.
* **Live touches** (`Live`): an editor open, automation being written (or
  a lane in Write), record arm, input monitoring, live MIDI, an audition,
  a take auditioned, a live key or followed track; never cached: frozen
  tracks, surround chains, a track delay (it plays before the devices),
  pre-FX sends, MIDI outputs, launcher clips, plugin outputs, outboard
  gear, tracks fed by others' audio, modulated faders or pans, the user's
  Keep Live (`Track::no_self_freeze`, `Command::SetTrackSelfFreeze`) and
  plugins kept live (`SelfFreezeSettings::never`, the insert menu). A
  third-party plugin's state is part of the key; a sandboxed one whose
  state is on its way is not rendered yet; one that reads the room, rolls
  dice or follows the clock is rendered as one performance of it, as a
  freeze would be. A track whose devices' saved state changes when
  nothing was edited (its cache falls stale on its own `CHURN` times)
  stays live for the session.
* **When Needed** (`SelfFreezeMode`, the default; Always and Off in
  Preferences → Audio). A machine that copes hears every chain live:
  caches are made only while it struggles — the audio load (the
  performance figures' smoothed total, measured whether or not anyone
  looks) at `NEED_LOAD` 60 % or more for `NEED_FOR` 3 s, or a dropout
  (xrun or late callback) while playing — and for `NEED_HOLD` 30 s after
  (one track renders at a time, costliest first; then the load is read
  again with what was cached, so it caches about as much as the machine
  needs). Caches made stay while they are right when the load falls:
  releasing them would raise it again, and tracks would flip between
  their caches and their chains. Not needed, a track reads `Live::
  NotNeeded`; `self_freeze_now` counts as needed (the dev action,
  tests); `Session::simulate_self_freeze_load` stands in for the load in
  tests, where the dummy device's load is whatever the machine gives it.
* **When.** The tick (`tick_self_freeze`) looks every 250 ms once the
  undo history has been quiet for `SETTLE` (2 s): caches in the folder for
  a track's key are played at once; otherwise one track is rendered at a
  time on a thread below the UI's priority (`priority::lower_this_thread`)
  — tracks playing round the playhead first while playing, then the
  costliest. The render measures its devices' cost (node timing) and
  drops chains cheaper than `min_cost_percent`, judged early on a probe.
  No fade to a cache starts within `STOP_QUIET` of a stop (tails still
  ring); without a device nothing would move a fade, so they end at once.
* **Storage** (`self_freeze::store`). `<project>/Cache` (a never-saved
  project's scratch folder until the first save moves it; Save As moves
  it along), `<key>.wav` and `index.json` (start, latencies, cost, last
  use). Over `max_megabytes` the least recently used go first, never one
  being played; files no track would play go with the unused media when a
  project closes.
* **Known differences from the chain.** Stopping fades a cached track out
  in `STOP_FADE_SECONDS` (10 ms) where the chain would ring on; after a
  locate or loop wrap a cached track plays as if played through from the
  start (the chain's tail of what came before is not there), as a freeze
  does; a linear-phase device's pre-ringing before the first frame after
  starting playback is not there; parameter ramps (`AUTOMATION_STEP`) fall
  on the live engine's block boundaries, so automated chains match to
  within those ramps' timing rather than bit for bit.

Status: the Performance view's summary line (tracks cached, CPU saved,
the render going on, the folder's size) and its tracks' notes, the star
before a cached track's name in the arranger and "INSERTS · CACHED" on
its strip, the Self-Freeze submenu of both track menus; Preferences →
Audio → Self-freeze (on, size, minimum chain cost). Dev actions
`self-freeze:<on|off|when-needed|always|now|status|clean|keep-live=<track>|auto=<track>|never=<plugin id>|allow=<plugin id>>`;
`faderframe-bench --self-freeze` measures the CPU saved. Tests:
`engine/tests/self_freeze.rs` (a switch while playing and a cache from
the start bit-identical, the way back, loops/locates/stops behind
latency, a gentle return), `self_frozen_tracks_do_not_allocate`,
`session/tests/self_freeze.rs` (a heavy track cached after its render, an
edit back at once and undo finding the cache, the session's cache played
by an engine of its own bit-identical, live touches, cheap chains and the
folder, a fade while a device plays), the views'.

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
group when the group links selection. A folder's Group switch is the
edit and record group of what it holds (clips, ranges and arming, not
faders): see "Folder groups" under folders.

### Duplicating tracks, merging clips, device names, unused media

*Duplicate Track* (`Action::DuplicateTracks { tracks, content }`, no
tracks = the selected ones; `session::duplicate`): each copy goes right
after its original (a folder after its contents), named on ("Bass" →
"Bass 2", "Audio 1" → "Audio 2"), with everything a track holds under
fresh ids — devices (nested ones, the containers' map; their current
state captured first), sends, automation lanes (plugin and send targets
remapped), modulators (routes remapped) — not armed, soloed or frozen; a
plugin output it took goes back to its owner. *With content* copies its
clips (and a folder's tracks into the new folder). One undo step.

*Merge Clips* (`Action::MergeClips`, `session::merge`): per track, two or
more clips become one from the first start to the last end. MIDI joins
at once (notes, controllers and SysEx placed from the new start, fresh
note ids, expression kept by note, what a clip cut off stays cut off).
Audio is rendered as it plays — gain, fades, warps, takes, gaps as
silence, muted clips silent, without the track's devices — on a one-track
copy in a worker (`render::start`), into a new file; `poll_merges` puts
it in place as one "Merge Clips" step unless a clip went meanwhile
(`wait_for_merges` in tests).

*Device names*: `faderframe_project::device_name(chain, slot)` /
`Track::name_of(slot)` / `Session::device_name(id)`: a plugin's name, and
"(2)", "(3)" … after the first of that name in a chain (a track's
`slots()` order, a song's inserts), used by every view that names a
device (mixer inserts and menus, editor window titles, automation and
modulation targets, the Performance view, graph node names, plugin
outputs, sandbox notices). Numbers follow the chain's order.

*Unused media* (`session::cleanup`, `Session::unused_media`): the audio
files directly in the saved project's `Audio` folder that neither the
saved file nor any saved version uses — a listed source counts when
`Project::used_sources` (clips, takes, clip-effect, spectral and
polyphonic originals, freezes) has it, and a file named anywhere else in
a saved file is kept; `Samples` and other subfolders are left alone,
nothing is reported when a saved file cannot be read. The shell asks once
the project is really replaced or the window closes (`dialogs::Closing`,
`ask_unused`; File → Open captures it before its chooser and acts after
it), moving them to the trash; Preferences → General: Ask / Move to the
trash / Keep. While a project is open its undo history may still need
them, so nothing is removed then.

### Sample-level redraw

The Pencil at sample-level zoom redraws audio samples (click repair). The
redraw never touches the original: the source is copied in chunks with the
drawn samples into a new float WAV in the media folder, and only that clip
switches to it (`AddSource` + `SetClipContent`, one undo step). Other clips
on the source keep the original. The copy is written on a thread
(`session::redraw`: strokes queue, each copying the source the clip has
when it starts, so quick strokes add up; `poll_redraws` applies one when
its file is done, dropping it if the clip changed meanwhile); the arranger
keeps drawing released strokes until then (`Session::pending_redraws`),
and saving waits for them (the media folder may move).

### Slicing (REX-style)

Audio is cut where it hits and played back from a sampler, the slices in
the original rhythm (`session::slicing`, `Action::Slice { source, by,
target, midi }`). The source is a span of an audio track — the edit
selection on it, else the selected clips, else the clip under the
pointer, rendered as the clips play it (gain, fades, warp; no devices,
fader or pan: `Session::render_span`, shared with *samples made of a
selection*) — or the sample a Sampler or a Drum Sampler pad plays. The
cuts (`SliceBy`) go at the transients (detected on the material itself
with `onsets::detect`, at the editor's transient sensitivity; nothing
closer than `MIN_SLICE_SECONDS` 20 ms to the start or another cut, the
stronger kept), on the grid (every step from the span's start, worked out
in the session through the tempo map, so the cuts fall where the grid
lines are), into n equal parts, or where clips on the track start inside
the span (cut by hand with Separate). The first slice always starts at
the material's start, so the slices end to end are the material.

The targets (`SliceTarget`): a new instrument track with a Drum Sampler
(a pad and a float WAV in `<media>/Samples` per slice, the cuts faded over
1 ms only where they go through sound), the free pads of a kit (slicing a
pad's own sample: that pad keeps the first slice), a new instrument track
with a Sampler in slice mode (the material as its one sample, the cuts in
its document), or the Sampler's own sample in place. Pads take one slice
each (16, or the free pads): slicing at transients keeps the strongest
that fit and says how many were left out; the grid, the clips and equal
parts that make more slices than fit are refused before anything is
rendered. A Sampler takes up to `MAX_SAMPLER_SLICES` (64). With `midi`
the slicing writes a MIDI clip on the instrument's track at the
material's place (a sample: the playhead, or the edit selection's start):
one note per slice where its slice began, as long as it, velocity 127 (a
pad plays its slice at its recorded level), on the kit's key for that pad
or the Sampler's key for that slice; MusicalTime's resolution places the
notes on their samples. The clips the span covers whole are muted (the
MIDI clip plays them now). The render, detection and the files run on a
worker thread; the tick (`poll_slicings`) places the result as one undo
step ("Slice to Drum Sampler", "Slice to Pads", "Slice to Sampler",
"Slice Sample"); pads taken meanwhile are never overwritten.

The Sampler's **slice mode** (`sampler::id::SLICES` 24, `SLICE_KEY` 25,
appended so older states load): its `SampleDoc` carries `slices` (cut
frames of the sample at its own rate; `#[serde(default)]`, written only
when set) and the loaded `SampleSet` the regions (`slice_regions`); a key
from the first slice key plays its slice as recorded — no key tracking,
no loop, the device's transpose, tune, envelope, filter and reverse on
top; keys past the slices play nothing (`slice_of_key`). Cuts by hand go
through `Action::SetSampleSlices` ("Slice Points"; a drag of a cut is one
gesture), and `Action::SlicesToMidi` writes the slices one after another
at the playhead. Faces: the Sampler's waveform numbers its slices and the
keyboard shows the slice keys; its right-click menu slices, adds a cut
there or removes the one there, turns slice mode on and sends the slices
to a MIDI clip or a new kit (`Session::sample_slice_choices`); a Drum
Sampler pad's menu slices its sample onto the free pads. The arranger's
clip and track menus have "Slice to Pads or Keys"
(`Session::slice_choices`).

Tests: `session/tests/slicing.rs` (a drum bar cut at its hits, the
slices end to end are the bar, the notes where the slices were, and the
kit played back against the drums before slicing: correlation 0.9999 at
the kit's −3 dB; the grid onto a Sampler; a kit's free pads; a pad and a
Sampler's sample sliced), `slicing::tests`, plugin-host
`slices_play_a_key_each_as_recorded` and
`slices_cut_the_first_sample_from_its_start`, the sliced sampler in
`the_instruments_do_not_allocate`, view-devices
`the_samplers_slice_their_samples_from_their_faces`, arranger
`slice_tests`.

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

### The media browser

The Browser (`faderframe-view-browser`, `ViewKind::Browser`, the left dock
area beside the Inspector; View → Browser, Ctrl+Shift+B) finds sounds and
MIDI files, lets them be heard in the song's tempo and key, and takes them
where they are used. Its state is the session's (`session::browser`,
`Action::Browser(BrowserOp)`); nothing in it edits the project except
placing files.

* **Places and listings.** Project Media (the project's media folder),
  Samples (`<media>/Samples`), Recent (files heard or placed lately), Home,
  and folders the user adds (a folder dropped on the view is added). A
  worker thread (`browser::scan::Scanner`, below the UI's priority, started
  the first time the browser is used) lists the folder looked in — and,
  while searching, the folders inside it (Subfolders; at most
  `MAX_LISTED` 20 000 files, symlinked folders never followed, hidden files
  left out). A search matches every word against the path and the tags;
  the filters (type — audio, MIDI, loops, one-shots —, length, key: the
  project's (the same notes or a fifth apart) or one, tempo: the project's
  within 6 % or doubled/halved, or a range, tag, least rating) and the sort
  (name, kind, length, tempo, key, rating) apply to what is known.
* **The index.** What a file is comes from its container
  (`faderframe_audio_files::decode::probe`: format, bits, channels, rate,
  length; MIDI files from `midly`: format, channels, length at their own
  tempo) and then from its audio, decoded once on the worker
  (`scan::analyse`: tempo by `faderframe_analysis::tempo` kept at
  confidence ≥ 0.1 — a sound of 1.5 to 8 s, shorter than the detector
  needs, is heard round for it and keeps the tempo only when its length is
  then an even number of beats with an onset at least every other beat
  (`scan::tempo_of`: a held pad heard round has a "beat" where it wraps) —,
  key by `chroma` + `detect_key` kept when the pitch
  classes are tonal enough — 1 − entropy/ln 12 ≥ 0.08, so drums have
  none —, a 128-point waveform thumbnail, a `SoundPrint` from the first
  30 s; MIDI files: their tempo, the key of their notes, a note-density
  thumbnail). Files of the listing the search passes are read and analysed
  first (at most `MAX_QUEUED` 1000 by themselves; "Read and Analyse
  Everything Here" takes a whole tree). The index (`browser::index`,
  `<data>/browser/index.json`, `INDEX_VERSION`) keeps an entry for as long
  as the file's size and modification time are those it was made from — a
  file changed on disk is read again — and drops the longest unseen past
  `MAX_ENTRIES`; it is written a few seconds after a change. Tags, ratings,
  recent files, the user's folders and the preview settings are
  `user.json` beside it: the index can be thrown away without losing them.
  A loop is audio whose length is an even number of beats at its tempo
  (within 3 %).
* **Similar sounds.** `faderframe_analysis::sound_print` (pure,
  deterministic): mean power in 30 third-octave bands (25 Hz–20 kHz) over
  the frames within 40 dB of the loudest (so a quieter copy prints the
  same), the spectral centroid in octaves, onsets a second (the level in
  23 ms windows rising 9 dB from its low since the last one fell 6 dB — a
  chord's beating is not an onset), the frame levels' spread and the
  length; `distance` weighs them so that 1 is a clearly different sound.
  Find Similar lists the 100 nearest analysed sounds of the whole index
  (missing files left out), the file asked about analysed first.
* **Previews** play beside the mix through the monitor path
  (`engine::nodes::file_preview`, in `ListenOut` after the compare source
  and the codec monitor, before listening: heard on headphones and in the
  mono check, never coded, compared, metered or rendered). A worker
  (`browser::preview`) decodes the file, resamples it to the stream's rate
  and — matched to the project at the playhead (`Session::browser_fit`) —
  stretches a loop to the project's tempo (its tempo doubled or halved to
  the nearest; speed within ½–2) and transposes it into the project's key
  (by the keys' relative major, −5…+6 semitones) with Signalsmith Stretch
  offline (`OfflineStretch`: the stretcher primed and its latencies taken
  out, so output frame u is input u · speed; Rhythmic for material with
  more than two onsets a second), publishing the audio chunk by chunk
  (`PreviewAudio`: `OnceLock` chunks of 16384 frames and a `ready` count,
  read wait-free; at most `MAX_PREVIEW_SECONDS` 600). A preview starts at
  once, waiting at the edge of what is decoded rather than skipping it; a
  synced loop started while the transport plays waits for the next bar
  (`PreviewStart::At`, the timeline frame read from the play history at
  the node's arrival, so in step with the mix as heard) and loops in phase
  with the bars (`loop_frames` = whole bars at the project's tempo) through
  loop wraps and locates; a stop ends it. Starts from the first frame are
  unfaded (a drum's attack is kept); joining late, stopping and replacing
  fade over 5 ms. The volume is an atomic gain (−40…+6 dB). MIDI files play
  the selected instrument (or MIDI) track, else the first instrument track,
  through the audition port (`engine::midi_preview`: events in frames —
  the project's tempo map when synced, the file's own tempos otherwise —,
  injected into the callback's live MIDI at their offsets, keys transposed
  except on channel 10, loops wrapping with the notes cut there ended,
  everything sounding released when it stops or is replaced). Auto hears a
  file when it is selected.
* **Placing.** A drag out of the list carries `browser:` and a path a line
  (`browser::payload`): the arranger places the files where they drop
  (`BrowserOp::Place`), a launcher slot takes them from there down
  (`BrowserOp::ToLauncher`); any other view that takes files (a sampler's
  slots, the Reference view) gets them as files, through the canvas host
  (`CanvasWidget::take_payload`; the payload also travels as text between
  windows). Files are imported as any file is (`ImportJob`), then placed
  in one step ("Place from Browser", "Browser to Launcher") with the
  preview's match: on the arranger a loop is warped to the project's tempo
  (`Warp::uniform`, its length the source's / speed), in a launcher slot it
  follows the tempo itself (`ClipLaunch::tempo`), and the key is a
  transposition of the clip (`StretchSettings::Pitch { semitones }`: the
  clip plays through a stretcher voice, its time kept — the engine's
  warped-region path, `WarpedRegion::transpose` × the pitch factor; not
  with Varispeed). MIDI files go onto the arranger as MIDI imports do.

Tests: `faderframe-audio-files` `a_probe_reads_the_container_without_decoding`;
analysis `sound_print::tests`; engine `tests/file_preview.rs` (beside the
mix at once and unfaded, volume ramp, played out, stopped with a 5 ms fade;
from the next bar in phase through a locate and a loop wrap, a stop ends
it; waits for decoding; a MIDI file plays the auditioned instrument and
its notes end; a one-shot ends with its length),
`the_media_browser_previews_do_not_allocate`,
`warp::a_transposed_clip_keeps_its_time_and_moves_its_pitch`; project
`a_transposition_keeps_in_the_file_and_off_is_written_as_before`; session
lib `browser::{tests, index::tests, scan::tests, smf::tests,
preview::tests}`, `session/tests/browser.rs` (analysis and the index kept
until a file changes, filters/search/tags/ratings/Find Similar, previews
stretched and transposed, placing on the arranger and in a launcher slot);
views `faderframe-view-browser` tests, arranger
`files_from_the_browser_are_placed_where_they_are_dropped`, launcher
`sounds_dropped_from_the_browser_land_in_the_slot`.

### Batch processing

File → Batch Processing… takes a list of files through a chain of steps
and writes each anew into an output folder; nothing of it is a project
edit, so it never reaches the undo history and works without a project's
media. `faderframe_session::batch` holds the model and the work, the GTK
window is `faderframe-ui/src/batch.rs`.

* **The chain.** A `BatchChain` is a list of `BatchStep`s and a
  `BatchOutput`. Steps: `Plugin` (a `PluginRef` with an optional state and
  explicit values, as an insert slot stores them, plus seconds of tail),
  `Normalize` (sample or true peak to a ceiling), `Loudness` (BS.1770
  integrated loudness to a target with a true-peak ceiling:
  `faderframe_analysis::delivery::normalize_loudness`, limiting the peaks
  or, asked, using less gain), `Convert` (sample rate with the importer's
  resampler, channels to mono — the average — or stereo — mono doubled, a
  wider file's first two), `TrimSilence` (what lies before the first and
  after the last sound `faderframe_analysis::silence::regions` finds, with
  pads and a shortest sound) and `Fade` (lengths and `FadeShape`s). Steps
  run in order on the file's audio held in memory as 32-bit float at its
  own rate. The output is WAV (`Bw64Writer`: plain RIFF, RF64 past
  4 GiB) or FLAC (`faderframe_audio_files::flac::FlacWriter`: flacenc,
  16/24 bits, up to eight channels, streamed frame by frame with the
  stream information — length, MD5 — written over the first one at the
  end), 16/24-bit or float with `Dither`, and a name pattern
  (`batch::TOKENS`: `{name}`, `{ext}`, `{folder}`, `{n}`/`{n:3}`,
  `{preset}`, `{date}`, `{time}` — the shell's local time —, `{rate}`,
  `{bits}`, `{channels}`, `{format}`). `BatchChain::check` refuses what
  cannot work before anything starts (an unknown name part, float FLAC, a
  rate out of range).
* **Plugins offline.** A plugin step plays the audio through the engine's
  `OfflineRenderer` exactly as an insert would: a one-track project with
  the file's audio as an in-memory source (never opened from disk, never
  the live engine's) and the plugin on its master, mono through a mono
  master, stereo through a stereo one, at unity. Its latency comes off the
  front; its tail is kept up to the step's seconds, cut where it falls
  silent. A plugin that cannot be loaded or stops working fails that file
  only; more than two channels are refused (a Convert step before it
  helps), and so are devices that work only on a track — the Hardware
  Insert, containers, input-stage devices (`batch::batchable`). Each
  renderer runs on its batch worker alone (no DSP pool: the batch runs
  files side by side). Built-ins take their factory presets
  (`PluginStep::use_factory_preset`); the window also edits any plugin's
  values by number (`Session::clip_fx_parameters`) and takes over the
  state and values of one of the project's inserts of the same plugin, so
  a third-party plugin set up with its own editor on a track can join a
  chain.
* **Running.** `batch::start(BatchRequest)` checks, makes the folder and
  starts up to `MAX_WORKERS` threads (`default_workers`: half the cores,
  at most four) at a lowered priority (`priority::lower_this_thread`;
  never the UI's or the audio's, never realtime). Workers take the files in
  turn: decode (Symphonia, any format the importer reads), name, the steps,
  a measurement (`delivery::measure` and the true peak of every channel),
  and the file written under a temporary name beside its destination and
  moved into place (`render::files::StagedFile`, `Staged::commit`). A
  memory budget (`batch::MEMORY_BUDGET`, 3 GiB counting a file three times
  its decoded size) holds workers back while the files in hand would pass
  it; one larger than the budget runs alone. `BatchRun::status` gives
  every file's `FileState` (waiting, working with the step and a fraction,
  done with its `FileResult` — rate, channels, length, loudness, peaks and
  notes such as the gain applied or a clip in an integer format —, skipped,
  failed with the reason, cancelled); `cancel` stops at the next look
  (decode packets, 16 384-frame pieces), and a dropped run is cancelled.
  A file that fails or panics fails alone.
* **Names and inputs.** A file is named before its steps run (the rate and
  channel count it will have are known from the chain), so a skipped file
  costs nothing. Names are claimed under a lock: two files of a batch never
  take one name (the later one is numbered, " 2", " 3" …), and the batch's
  inputs (canonical paths) count as taken, so an input is never a
  destination whatever `IfExists` says (`Number` the new file — the
  default —, `Replace` an existing one, `Skip` the input). A destination
  that appears while a file is processed is left alone and that file
  fails.
* **Presets.** `BatchPreset`s are JSON files in the data folder's
  `batch-presets` (`batch::presets_dir`, through `media::data_dir`, so
  portable mode keeps them in its folder): `{"format":
  "faderframe.batch-preset", "version": 1, "name", "chain"}`, written aside
  and renamed into place. Factory presets ("Normalise to −1 dBTP",
  "Podcast −16 LUFS", "Streaming −14 LUFS", "Broadcast −23 LUFS (EBU R
  128)", "CD 44.1/16", "Trim Silence and Fade", "FLAC Archive (24-bit)")
  cannot be replaced or deleted; names match whatever their case. The
  `_in(dir)` variants take another folder (tests).
* **The window.** Files (chooser, a folder searched through, drag and
  drop), the step cards (add, remove, reorder, each step's settings, a
  plugin chosen from the installed and built-in effects with search), the
  output, presets (load, save, delete) and Run/Cancel with every file's
  progress and result; what it holds is kept while the program runs.
  Closing it while a batch runs asks first. Dev action
  `batch:<preset>=<file>[|<file>…]` runs a preset into a "Batch" folder
  beside the first file and logs each file's result.

Tests: `session/tests/batch.rs` (generated files: normalising to a sample
and a true peak, loudness within 0.5 LU under its ceiling, rate, depth and
channels in WAV and FLAC, trimming with pads, fade shapes, the utility's
gain on mono and stereo, refusals and failures that leave the others
alone, names, numbering, inputs never written, replace and skip, eight
files side by side, a cancel leaving no temporaries, presets, a factory
preset), the session lib's `batch::{tests, naming::tests, steps::tests,
presets::tests}`, and audio-files `flac::tests` (bit-exact read-back, the
header's length and MD5, refusals).

### Scripts and macros

Scripts are Lua 5.4 (the reference interpreter, vendored by `mlua`'s
`lua-src`, MIT) over the session; docs/SCRIPTING.md is the API's reference.
Three layers: the pure crate `faderframe-script` (the sandboxed state:
libraries, limits, errors, `--!` headers, Lua literals; no session), the
session's `script` module (the `ff` table bound to the session, runs,
prompts, timers, the console, the library of scripts and macros, the macro
recorder) and the shell's `faderframe-ui/src/scripts.rs` (the Scripts menu,
console window, prompt dialog, Macros window, development aids).

* **One state, a run at a time.** The session keeps one Lua state (made on
  first use) with the `ff` table frozen in it. Its functions are made once;
  each reaches the session through a scoped userdata the run puts in the
  registry (`Lua::scope` + `create_any_userdata_ref_mut`), so they work only
  while a run is under way and never outlive the borrow. A script runs as
  a coroutine on the UI thread; scripts never run on the audio thread and
  never start one another (`ff.run` calls another script's chunk inside
  the run instead). Each run gets its own global environment over the
  shared libraries.
* **One undo step.** A run is a gesture named after the script; every API
  call is an existing session action or command (`Session::edit`,
  `Session::dispatch`), so groups, ripple, time-locked material, aliases
  and the engine's sync behave as for the editors. Edits a script makes are
  internal: they reach what they name, not other selected tracks or group
  members. Drag-style actions (`MoveClips`, `TrimClips`, `ClipGain`) are
  worked out from the gesture's first sight of a clip; inside a script's one
  gesture each call is a drag of its own, so before every call the run
  forgets the drag bases and the ripple mark (`History::unmark`) while the
  changes stay in the step. A failing run cancels its gesture: the project
  is as it was.
* **Prompts wait without blocking.** `ff.prompt` stores the question and
  yields the coroutine; the run commits what it did so far and the session
  sends `UiRequest::ScriptPrompt`. The answer (`ScriptOp::Answer`) resumes
  the coroutine in a gesture marked with `History::amend_on_end(token)`:
  when it ends, its changes join the step before the question while that
  is still the last one (nothing done, undone or redone since); otherwise
  they make a step of their own with the same name. A failure after a
  prompt also takes back the part before it when that is still the last
  step. A modal nested main loop was avoided: the session lives in a
  `RefCell` the dialog's callbacks need.
* **The sandbox.** No `dofile`/`loadfile`/`string.dump`, `load` in text
  mode into the caller's environment, `os` cut to the clock and dates, an
  `io` whose `open`/`lines` reach only the allowed folders (the saved
  project's folder and the user's scripts folder; links resolved before the
  check; a script run from a file also its own folder), `require` only for
  Lua text from the scripts folders, `print`/`io.write` to the console,
  `collectgarbage` only counting and collecting. Every run has a time limit
  (10 s by default; a global count hook every 1000 instructions): past it
  the hook yields the coroutine out of wherever it is (a `pcall` cannot
  catch a yield), and inside a C function that cannot yield it fails after
  a 0.5 s grace; the sandbox's `pcall`, `xpcall` and `coroutine.resume`/
  `wrap` re-raise once a run is over its time, so no Lua code catches the
  stop. `Lua::set_memory_limit` bounds the state (256 MB). Errors carry the
  chunk and line (from the message, else the traceback's first Lua frame).
* **Timers.** `ff.after`/`ff.every` keep functions in the session; the
  session's tick calls those due (at most 8 a tick, never while a gesture
  is open or a run is under way), each as a run of its own named after the
  script that set it. Renders started by scripts are polled there too.
* **The library.** `script::library` reads `<settings>/scripts` (the shell
  sets it with `set_user_dir`), `<project>/Scripts` and
  `<settings>/scripts/macros` (top-level `.lua` files; every 2 s from the
  tick); entries have stable keys (`user/<file>`, `project/<file>`,
  `macro/<number>`) that the menu's `app.script::<key>` items and the
  shortcuts the user gives them keep.
* **Macros.** While recording, `Session::dispatch` hands the outermost
  user action to `dispatch_heard`, which notes what the project held,
  dispatches it and writes it as Lua (`script::record`): tracks by name or
  by the variable a track the macro made was made in, clip lists that were
  the selection as `ff.selected_clips()`, drags as their last motion
  (lines with one key in an open gesture replace each other; a cancelled
  gesture drops its lines), view-only actions left out silently, the rest
  listed as not recorded. Stopping writes "Macro n" with a `--! macro: n`
  header; n is what `MappingTarget::RunMacro`, `SurfaceCommand::RunMacro`
  (Mackie/HUI Control + F1…F8, OSC `/macro/run i`, `/macro/N`) and
  `ScriptOp::RunMacro` run. Actions of a script being run are never heard.

Tests: the crate's (`nothing_runs_programs_or_reads_the_machine`,
`files_open_only_inside_the_allowed_folders`,
`a_run_over_its_time_is_stopped_whatever_catches_it`,
`errors_say_where_they_happened`, …), the session's `tests/scripts.rs`
(one undo step, a failure taken back with its line, sandbox refusals,
prompts joining the step, timers, the console, a recorded macro replayed
on a fresh project and from a MIDI button, the library and `require`),
the history's `amend_tests`, the OSC surface's `macros_are_run_by_number`
and the UI's `the_scripts_menu_lists_its_commands_for_the_palette`.

### AAF interchange

Post-production hands sound between Pro Tools, Media Composer, Premiere
and Resolve as AAF; FaderFrame writes and reads the part of it those
programs use for audio. The pure crate `faderframe-aaf` holds the
format in four layers, written from the AMWA AAF Object Specification
and Edit Protocol (not from the AAF SDK, whose licence we do not take):

* **The container** (`cfb`): Microsoft's Compound File Binary. The
  writer streams: large streams go to disk sector by sector as they are
  filled (an export holding gigabytes of embedded audio holds one buffer
  of it), streams under 4096 bytes into the mini stream, then the
  directory (each storage's children as a balanced tree whose deepest
  level is red — a valid red-black tree), the FAT and, past 109 FAT
  sectors, the DIFAT, the header last. Version 4 (4096-byte sectors, as
  AAF's "4K" files and pyaaf2 write) by default, version 3 on request;
  the reader takes both, chains checked against loops.
* **The stored form** (`store`): an object is a storage whose class id
  is its class; its properties a `properties` stream (local id, stored
  form, size, then the values); strongly referenced objects child
  storages named like the AAF SDK names them (`Header-2`,
  `Mobs-1901{3}`), vectors and sets of them with an `index` stream (sets
  keyed by a property: mob ids, definition ids); weak references a key
  and a tag into the root's `referenced properties`, the property paths
  from the root to the sets they point into; data streams child streams.
  `Object`/`Value` hold one object whole, `StoreWriter`/`StoreReader`
  move trees in and out, `dump` outlines one for golden tests.
* **The model** (`model`, `meta`): every class with its properties
  (local ids, types, optional or not) and every type, generated into
  `model/baseline.rs` by `scripts/aaf_model.py` from pyaaf2's tables (MIT;
  the AAF baseline plus the Avid extensions Avid applications write,
  dynamic local ids as pyaaf2 gives them — regenerate, never edit).
  Files carry the whole meta dictionary, like the AAF SDK's and pyaaf2's
  do, so a reader needs nothing of its own to know any property;
  `model::{cls, pid, ty}` name what the crate uses and a test holds them
  to the tables. Reading, classes a file defines itself are known by the
  parents its meta dictionary gives them and extension properties by the
  local ids it gives them.
* **The composition** (`timeline`, `write`, `read`): what AAF says
  about sound without its machinery — mono tracks of clips (position,
  length, a medium's channel, offset into it, clip gain, a level
  envelope, fades of five curves), the timecode at position 0, markers,
  media (where each channel's sound is). `write` lays it out as the Edit
  Protocol does: a top-level CompositionMob (a timecode slot "TC1", a
  sound slot per track, an event slot of DescriptiveMarkers with Avid's
  comment colour), a MasterMob per medium with a slot per channel, each
  pointing at a mono file SourceMob — embedded (PCMDescriptor, 16 or
  24-bit samples in an EssenceData stream, converted as they are
  written) or linked (WAVEDescriptor with the file's header as Summary
  and a NetworkLocator URL). Fades are Transitions (Mono Audio Dissolve)
  against Filler — a filler wholly under a fade-out and a fade-in when
  clips abut —, crossfades Transitions between the clips (only where the
  overlap is exactly the one's fade-out and the other's fade-in; other
  overlapping clips go to a second slot of the track's name, "Dia (2)",
  which sums the same), the curve drawn into the dissolve's Level as
  nine points. Clip gain is a Mono Audio Gain OperationGroup around the
  clip (ConstantValue Amplitude, or a VaryingValue when there is an
  envelope). `read` takes the top-level composition (the one marked so,
  else one no other composition uses, the one with the most sound slots)
  and flattens its sound slots: sequences, fillers, transitions → fades
  and crossfades (the curve the nearest of the five to the Level drawn),
  gain operations → gain and envelopes (nested ones multiplied), other
  operations → their first input and a note, selectors, essence groups,
  nested scopes and nested compositions followed (cut to the window the
  clip shows), the AAF 1.0 SourceClip fade properties; each source clip
  followed through master mobs to the file source mob that holds the
  sound, edit rates converted on the way (a 25 fps sequence over 48 kHz
  master clips). Embedded essence is told as PCM (raw, its format from
  the descriptor) or as a whole WAVE or AIFF file (sniffed); `wave` puts
  a header round raw PCM so a decoder opens it. OMF is neither read nor
  written: OMF 2.0 files are Bento containers with an object model of
  their own, sharing no layer here; `read::is_omf` recognises one and
  the import says so.

The session (`session::aaf`) maps projects onto that and back.
`Action::ExportAaf { path, media: AafMedia::{Embed, Link} }`: every
track holding audio becomes one AAF track per channel ("Gtr.L",
"Gtr.R", "Bed.1"…); each clip plays its source's channel there as the
clip player routes it (a mono source on every channel), with its fades
(shapes kept; drawn bends not), gain and envelope — volume and, on
stereo tracks, the pan's balance folded into one level envelope per
side, points added where a straight line in amplitude would stray more
than a tenth of a dB from the curve (`channel_envelope`). Take folders
go as their comp's pieces. What AAF cannot say — warped, pitch-edited or
reversed clips, generated sources, sources at another rate, a stereo
source on a mono track — is rendered as it plays on a plain one-track
project (its gain, fades and envelope taken off and written natively,
so they stay editable). Markers and sections go as markers, the
project's timecode (25 fps from zero when unset) as the timecode slot;
MIDI tracks and muted clips are left out with a notice. A worker
renders, writes linked media as mono 24-bit WAVE files into "<name>
Media" beside the AAF, and writes the AAF into a staged temporary;
it is placed through the exports' question flow (`export_to`,
`commit_export`). `Action::ImportAaf(path)` reads the top-level
composition at the project rate in a worker, copies embedded essence
out or finds linked files (at their URL, by name in the AAF's folder
two levels down, in the folder the user names four levels down,
`locate`), decodes each through the media import into the project's
Audio folder and splits multichannel files into the mono channels the
tracks play. When linked files are missing, `UiRequest::RelinkAaf`
asks (Look in a Folder… / Import Without Them); `Action::RelinkAaf(Some
(folder))` looks once more, `None` keeps them as offline sources at
their linked paths. Then one "Import AAF" step: a mono track per AAF
track, clips on exact samples (`Timeline::at_sample`), fades with their
shapes, gain, envelopes as volume points straight in dB
(`volume_points`), markers; placed by timecode — a project with a
timecode puts the AAF's start where it counts that timecode, one
without takes the AAF's.

Tests: the crate's (compound files of both versions and past the DIFAT,
names and trees; stored objects round trip; model constants held to the
tables; lanes, transitions and fades laid out; a composition written and
read back at its rate and another; linked media; cancelling; the
golden outline `tests/golden/sample.txt` (`FADERFRAME_AAF_BLESS=1`);
OMF told apart), `session/tests/aaf.rs` (a project out and back,
embedded and linked, the audio bit for bit at 24 bits, a reversed clip
rendered, missing media relinked from a folder or left offline) and the
session's `aaf::tests` (envelopes per side, dB vs amplitude). Outside
checks: `FADERFRAME_AAF_OUT=<dir> cargo test -p faderframe-aaf --test
sample` writes sample AAFs that pyaaf2 (MIT) and olefile read
(`uv run --no-project --with pyaaf2==1.7.1 --with olefile`); files from
other writers go through `FADERFRAME_TEST_AAF=<file>[:…] cargo test -p
faderframe-aaf --test foreign -- --ignored --nocapture` (an Avid-style
file made with pyaaf2 — a 25 fps sequence over 48 kHz master clips with
pyaaf2's own PCM essence, a fade, a crossfade, gain, a nested sequence
— reads exactly). Not done: stereo AAF tracks paired back into stereo
tracks on import (they come in as mono pairs), track volume/pan
automation and plug-ins (AAF carries clip-level sound only here),
picture tracks, OMF.

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
for input + output latency as reported by the driver, the graph's output
latency (plugin delay compensation: what the player heard was processed
that much earlier) plus a user offset — the file is read `latency` frames
later than its capture position, so what was played lines up with what
was heard.

**Take folders.** `ClipContent::Takes(TakeFolder)` holds several `Take`s
and a *comp*: sorted `CompSegment`s, each playing one take (or nothing) until
the next one starts — the comp can never overlap or leave holes. Comp edits
(`set_comp`, `use_take`, `remove_take`) are ordinary `SetClipContent`
commands, so a swipe coalesces into one undo step; folders move, delete and
split like any clip.

A take is an audio clip of its own placed in the folder (`Take { name,
start, audio: AudioClip, rating }`, file format version 3; `MIGRATIONS`
turns version 2's folder-aligned source offsets into it): it carries every
edit a clip can — pitch, warp, transposition, reversal, clip effects,
spectral edits, envelope, fades, gain. What a comp plays is its takes' clips
cut at the comp's boundaries, `TakeFolder::played` (and `played_take` for a
take auditioned whole): centred equal-power crossfades (`crossfade`, 10 ms
by default) where pieces touch, short declicks elsewhere, the folder's fades
at its edges, a take's own fade where the take itself begins or ends (the
longer of it and the comp's plays), the folder's gain in every piece. Cutting
is `AudioClip::cut`/`split_frames`, the same cut Separate makes
(`Clip::split_at`): a warp's map is cut where the output is, an envelope is
sliced, a reversed clip's earlier output is its later source. The engine
plays each piece exactly as it plays a clip (`snapshot::place_audio`, its
stretcher key from `piece_key`; `build::stretch_voices` counts the pieces,
and one voice of each kind a folder's takes need for an audition), and
*Flatten Comp* places the same pieces as clips, so a flattened comp plays
what the comp played sample for sample. Readers that need a source position
go through the take: `Take::source_at` / `folder_of`, or the cut clip.

Every take has an id of its own in its folder (`TakeId`, from the
folder's `next_take`; `add_take` gives it, `normalize` — run by the clip
commands and on loading — gives one to a take without one or with one
another take has). It is kept through comp edits, splits, undo and
saving and never given to another take of the folder, so an editor
working on one take names it by `TakeRef { clip, take }` (`Project::{
take, take_in, take_ref}`) and changes it with `Project::set_take_audio`
(the folder's content with that take's clip changed: one undo step, as a
comp edit is) while takes before it come and go. The comp still names
takes by index (they are only looked up at once); the engine keys a
piece's stretcher by the take's id.

Recording in *Takes* mode folds the clips in the way with all their edits,
each at the level it had (a take's gain under the folder's own), and a
folder taken into another brings its gain and edge fades in its takes
(`TakeFolder::takes_as_played`), so nothing is lost and a pitch edit's or
clip effect's original recording stays in use (#67). A take is edited
like a clip, on its own: every clip editor takes an `AudioRef` (a clip or
one take; see *Pitch editing*), and a take lane's menu (Show Takes) opens
the pitch editor ("Edit Pitch of “Take 2”"), the spectral editor and clip
effects on it, converts it to MIDI and holds its "Clip Envelope" submenu.
While a clip envelope is shown the take lanes draw and edit their takes'
envelopes instead of comping, and the folder's main lane draws what the
comp plays (each piece its take's envelope there; edited in the lanes).
Strip Silence cannot cut a take into pieces inside its folder: it leaves
folders out and says to Flatten Comp first.

**Record modes** (`RecordSettings`, Transport menu / Preferences → Recording):

| Setting | Options |
|---|---|
| Record mode (over existing clips) | *Takes* — overlapped clips and folders become takes of one folder, the new take is comped in over its range, playback elsewhere is unchanged (a take keeps every edit of the clip it was — see *Take folders*) · *Replace* — overlapped clips are cut back (tape style) |
| Loop recording | *Passes as takes* · *Keep last pass* · *New track per pass* (earlier passes on new, muted tracks) |
| Metronome | Off · While recording · Always (`click.rs`: decaying sine per beat, accented downbeats, follows tempo and meter, live output only) |
| Pre-roll | `Roll` (amount + `RollUnit` bars/beats/seconds; menu presets, any amount in Preferences) before the record start when starting from stop; from that bar's line or exactly (`preroll_on_bar`, `Session::pre_roll`); what lies before the project's start is counted in before playback (§6, *Counting in before playback*) |
| Count-in | `count_in` + `count_in_length` (a `Roll`): the click alone, the transport standing, before the pre-roll (§6, *Count-in*); `count_in_click_only`: no click during the take after it |
| Post-roll | `Roll` after a punch-out, then playback stops (`EngineController::stop_at`, `Session::roll_on`; the tick ends the take when it ran out, a user stop disarms it) |
| Punch | `Project::punch_range` + `punch_enabled` (`Command::SetPunch`); capture only inside it |

**Input quantize (the transport's Q).** `RecordSettings::{input_quantize,
input_grid}` (None: the edit grid; `Session::input_quantize_settings` =
the editor's quantize strength, swing and ends on that grid): recorded
MIDI lands on the grid as `midi_take_clip` places it — takes, Capture
MIDI, launcher slot recordings and overdubs (only the new notes);
playing live is not delayed. `TransportAction::ToggleInputQuantize`;
the Q in the header bar (right-click: its grid, MIDI learn as
`TransportControl::InputQuantize`); a controller's Quantize button
(`EditButton::Quantize`) toggles it — Shift+Quantize quantizes the
selected clips — and its light follows `SurfaceState::input_quantize`
(Komplete Kontrol, KeyLab Essential mk3). Saved in the preferences.
Tests `input_quantize_puts_played_notes_on_the_grid`,
`a_controllers_quantize_button_toggles_input_quantize`,
`the_quantize_light_follows_input_quantize`.

**Aborting a take** (`TransportAction::AbortRecording`, Ctrl+Space as in
Ardour; `AbortAndRestartRecording`, Ctrl+Shift+Space; Transport menu
`app.abort-recording` / `app.abort-and-restart`, rebindable in the
shortcut editor, MIDI learn as `TransportControl::{AbortRecording,
AbortAndRestart}`). Plain Space stays play/stop: `transport_keys` leaves
Space with Ctrl (or Alt, Super) to the accelerators, canvases never take
Space, and no other default shortcut uses these. `Session::abort_recording`
takes the recording out of record mode (engine first, whatever fails —
a device gone — the take is thrown away all the same), drops the MIDI
take unmarked (what was played stays for Capture MIDI), stops playback and
goes back to where the recording began (`ActiveRecording::from`; with a
punch range to where playback started), and parks the recording in
`discarding`: once its writer has let go, `discard_recording` deletes the
files it planned and made (`ActiveRecording::paths`, their `.ffpk`) and
nothing else — never a file a source of the project names. Nothing reaches
the project or the undo history. *Restart* records again from there as
soon as the engine reports stopped (`tick_restart_recording`; a stop is
deferred while rendering ahead primes it, and a take started earlier
would catch what still plays), through the usual path: the same pre-roll,
count-in and punch range; a user transport action in between, or no stop
within two seconds, cancels it. A launcher slot's recording is thrown
away while playback goes on (restart: the slot records again from its
next launch position). Not recording, Abort is Stop and Abort and Restart
does nothing.
A project replaced (New, Open) while recording: the shell first ends the
take and places it so the unsaved-changes question covers it
(`confirm_discard`, `recording_in_progress`); the session itself never
lets a recording outlive its project (`end_recordings_for_close` in
`replace_project` and `Drop`: its takes reach neither project, their
files stay; they used to be placed into the next project, onto whatever
track had the same id). Tests `session/tests/abort_recording.rs` (the
project and the media folder as before the pass, earlier takes kept,
restart with its pre-roll, punch, MIDI, closing while recording).

**Recording over clips.** The audio thread knows what is recorded:
`EngineContext::recording` (`record::RecordingNow`: the record window and
the recorded tracks, set on `BeginRecord`, cleared on `EndRecord`).
Inside the window a recorded track's clip player is silent and its Auto
monitoring passes the input (`MonitorGate`, sample-accurate at the
window's edges); before and after it the track plays its clips — so what
is heard while recording over a clip is the new take, not the old one
under it. (A count-in stands before playback instead of playing the
pre-roll silently: §6, *Count-in*.) Tests
`engine/tests/recording.rs`
(`a_recorded_track_plays_its_input_not_its_clip_inside_the_window`), session
`pre_and_post_roll_lengths_and_the_post_roll_stop`,
`the_count_in_survives_the_audio_device_reopening`.

**Takes that would go wrong** (`session/tests/recording_audit.rs`):
a frozen armed track is left out with a notice (it takes no clips; before,
it refused the whole "Record" step and every track's take was lost); with
several armed tracks a new track per pass puts each track's passes right
under it; a pass reads the `latency` frames past its own capture only when
the next pass followed at once (a wrap inside the record window) — after a
punch-out or a locate those frames are other audio, so the pass ends with
its own; Keep Last Pass keeps the last *complete* pass (within 1 %) when
Stop cut the final one short; each take counts its own dropped frames
(`record_counters` restart with `begin_recording`); Flatten Comp keeps the
engine's 64-frame declicks (`COMP_DECLICK`) at comp edges inside the folder
that touch no other piece; using a take with no material in a split half
changes nothing (it silenced the half). MIDI takes
(`MidiTake::{window, loop_end}`): a note held across a loop wrap ends at the
wrap (not at another pass's furthest event), one still held at a punch-out
ends there; Replace clears only inside the record window and the take's
bar-rounded clip is cut to it. Still open: Takes mode folds an overlapped
clip into a take as its plain source region — a `Take` has no warp,
reversal, pitch edit, clip effects or fades, so those are not kept in the
folder (undo brings them back); MIDI loop recording has no "new track per
pass" and stacks passes in one clip.

**Arranger.** Rows have per-track heights (stored in `WorkspaceSet`:
default + per-track overrides, saved with the layout; drag a header's
bottom edge, Alt+wheel for all, View → Track Height). Open take folders
(`Session::takes_open`, editor state like the selection) add a lane per
take below the track; clicking a lane puts that take into the comp section
under the pointer (between the comp boundaries around it — never the whole
comp), dragging comps the swiped range (snapped), right-click offers
Use (the whole take)/Delete Take and Flatten. Take lanes have a height per
track (`WorkspaceSet::take_lane_heights`, `TAKE_LANE_HEIGHT` 30 px,
20–240; drag their bottom edge — each lane grows by its share —,
double-click it for the default, or the lane menu's Take Lane Height).
Automation lanes have a height each (PR #57's request: a taller lane
edits a curve more finely): `WorkspaceSet::automation_lane_heights` by
lane, over `automation_lane_height` for every lane (both saved with the
layout; `AUTOMATION_LANE_HEIGHT` 56 px, 32–400 — the lowest keeps the
lane header's controls clear of its edge); drag a lane's bottom edge in
the header column (`Hit::AutomationEdge`, `Drag::AutomationLane`; the
track's own edge above its lanes still sizes the track; Alt: every
lane), double-click it for the default (Alt: every lane's), or the lane
menu's Lane Height (sizes, Every Lane at This Height) —
`Action::SetAutomationLaneHeight { lane: Option, height }`, not
undoable, kept by scenes, auditions and the macro recorder like the
other heights. A saved layout keeps only the heights of lanes there are
(`WorkspaceSet::keep_automation_lane_heights` in `workspace_to_save`);
the session keeps the others while an undo may bring their lanes back.
Each lane's ▶ auditions its take (`Action::AuditionTake`,
`session::take_audition`): the folder plays that take whole instead of the
comp (`EngineController::set_take_audition` → `TimelineSnapshot::build_with`
→ `take_pieces`), playback starting at the folder when stopped; nothing in
the project changes, and a change to the folder's comp or takes (an edit,
undo), clicking again or another project ends it.
While recording, armed tracks show a growing red region; the punch range is
drawn in the ruler.

### Capture Audio (retrospective recording)

"I played the best take while not recording": as Capture MIDI keeps
what live tracks were played, Capture Audio keeps the input of the armed
tracks (`AudioCaptureSettings::tracks`: armed only, armed and those
monitoring their input — the default —, or every audio track with a
hardware input), playing or stopped, and turns the newest phrase into
takes afterwards.

**Kept where recording would capture.** `EngineController::capture_inputs`
gives the audio thread `input_capture::CaptureRt`: for every chunk
`render` processes, right beside the recorder, the device input channels
the kept tracks use (after varispeed's resampler, as the recorder's) go
into a sample ring and a `CaptureBlock` header into another — the
engine's frame counter (the play history's `chunk_start`, continuous
whatever channels are kept), the timeline position while the transport
plays (not counting in, not scrubbing), whether the recorder took frames
of the chunk. Copy only: a full ring never blocks, the header still goes
in marked `lost`; with no room for the header either, the keeper sees
the counter jump. A changed set of channels is a new capture (the old
one retires through the garbage queue); the session asks for one when
the tracks kept change, the setting is switched, or the engine let go
without being asked (a new engine). Tests `engine/tests/input_capture.rs`,
`capturing_inputs_for_capture_audio_does_not_allocate`,
`keeping_inputs_for_capture_audio_takes_no_page_faults`.

**Memory, bounded.** `session::audio_capture::keeper` (a thread a step
below the audio thread, as the recording writer: `follow_audio(Feeder)`,
`breathe` while catching up; the rings hold `TRANSFER_SECONDS` 2 s) drains
them into `audio_capture::store::Store`: per device channel, samples in
chunks of `CHUNK` 16 384 frames (full chunks shared as `Arc`, the newest
filled in place), a peak per `MARK` 256 frames with a 1 dB histogram of
them and the loudest mark per `GROUP` of 64 (so scans pass over quiet
minutes), and the transport's `Span`s (run = a start or stop, pass = a
loop wrap or a locate, recorded or not). A channel keeps
`chunks_per_channel` = what `seconds` asks (default 300, 10–600), within
`max_megabytes` (default 1024) less one spare chunk shared by all the
channels, at least two: memory never passes the limit
(`the_memory_limit_holds_and_the_oldest_goes`). Lost frames are kept as
silence in place; a new engine's counter is moved on to continue. Memory,
not disk: nothing is written while nobody captures, and 48 kHz float is
11.5 MB per channel and minute — the defaults hold five minutes of a
stereo track in 115 MB and some 89 channel-minutes in all, each track
keeping less as more are kept. Switched off, everything kept goes.

**The phrase** (`audio_capture::phrase::find`). Sound is a mark over the
channel's threshold: 15 dB over its noise floor (the histogram's 10th
percentile), between −60 and −30 dBFS. Input frame `f` was played while
the player heard what was processed at `f − latency`, the latency
recording compensates (`Session::audio_take_latency`: input + output
latency, graph latency, varispeed both ways, the user's offset — one
helper for recording and capturing; the latency in effect is noted as it
changes, and a run uses its own). The newest sound after what was taken
decides: heard in a playing run, that run is taken, back to the transport
start — its passes as they were heard, from `LEAD_IN_SECONDS` (0.2 s)
before its first sound to `TAIL_AFTER_SECONDS` (1 s) after its last,
passes without sound left out; heard in a stopped run, the phrase back to
a pause of `PHRASE_GAP_SECONDS` (Capture MIDI's 8 s). Sound in a stopped
run that carries on from the run before (a chord still ringing when Stop
was pressed) is that run's tail when it ends within `TAIL_SECONDS` (10 s):
the run before is taken; sound after a pause of `TAIL_QUIET_SECONDS` is a
phrase of its own. A run the recorder took is not offered (what an
aborted recording played is: it was never placed).

**Placed as recording places takes.** Capture Audio (`Action::CaptureAudio`)
finds the phrase, takes each sounding track's channels from the store
(shared chunks, a copy of the partial one) and writes a float WAV per track
("<track> Capture n", peaks too) on a thread; `poll_audio_captures` then
places them in one "Capture Audio" step: playing, each piece at the
timeline position it was heard at, the file read `latency` frames later
than processed — the same frames as a recording of that run
(`a_run_played_while_playing_is_what_recording_it_would_have_been`, through
the dummy device's loopback with the metronome on: identical samples, the
clicks on the beats); stopped, the first sound (to the sample) on the bar
at or before the playhead, the lead-in before it. Takes go through
`record::place_passes` (shared with recording): the record mode (a take
folder over existing clips, or Replace) and the loop recording mode (passes
as takes, the last pass, new tracks). Frozen tracks are left out with a
notice; input before the phrase's end counts as taken. A replaced project
leaves what was kept and captures being written behind.

**Using it.** Transport → Capture Audio (`app.capture-audio`, Ctrl+Alt+C,
rebindable), Transport → Keep Input for Capture Audio (the toggle,
`app.toggle-audio-capture`), Preferences → Recording (on/off, minutes per
track, memory limit, which tracks, what is kept now);
`Preferences::audio_capture`. `Session::{audio_capture_settings,
set_audio_capture, audio_capture_status, can_capture_audio,
capturing_audio, wait_for_audio_captures}`. Dev actions `capture-audio`,
`audio-capture:<on|off|seconds=<n>|limit=<MB>|tracks=<armed|monitored|all>|status>`.
Tests: the store's and the finder's (`session/src/audio_capture/`),
`session/tests/capture_audio.rs` (stopped phrase on the bar, the run as
recorded, loop passes in place, the memory limit, the tracks kept, not
while recording).

Inputs on a device that follows another's clock (see *Several audio
interfaces*) reach the store later by their ring and resampler: a capture
reads such a track's channels that much later (`channel_input_latency`
less the stream's), as recording moves those takes back
(`ActiveRecording::later`).

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
a mapping to lane height (fader law for gains, log for Hz/ms). Each one
also carries where menus list it (#26: `AutomationParam::{menu, short}`):
the track's own parameters at the top, then a submenu per device (named
as `Track::name_of` names it, nested container devices too) and inside it
the plugin's own groups (`PluginInstance::parameter_group`: CLAP's module
path, VST3's unit path, the EQ's bands, the Drum Sampler's pads, the
Guitar Station's pedals; sandboxed plugins send theirs in `Param::group`),
the entry's label without its group's prefix. The arranger's lane menu,
the Automation view's "+" and the modulators' "+ Target"
(`ModTargetChoice` likewise) build them with
`faderframe_ui_canvas::menu::grouped` (levels past `LONG_LEVEL` entries
split into ranges); the GTK host puts a search field over any context
menu of `SEARCH_FROM` entries or more (`menu::search`: every word, any
case, own-label matches first; results flat with their submenus as
context on a stack page of their own; Enter takes the first, Escape
closes). Dev actions `automation-menu:<track>`, `menu-search:<text>`.

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
built-in parameters (stepped ones are left out), CLAP parameters
flagged `IS_MODULATABLE`, and continuous automatable VST3 and AU
parameters (not bypass or program switches). VST3 and AU have no
modulation of their own, so it is emulated
(`faderframe_plugin_host::emulated`): the processor follows each
parameter's value as set — the *base*: the UI, automation, mapped MIDI,
the plugin's own changes that are not a modulated parameter's echo,
values the controller or unit has after a loaded state (sent through a
bases ring) — and sends `base + modulation` as an ordinary change (VST3:
a point at the block's end, so ramping plugins glide; AU: a parameter
event at the block's start, or with each automation point), the base
again once the modulation ends. The host keeps showing and recording the
base (`parameter()` reads it while modulated; an AU editor's values
outside a gesture are taken as the modulation's echo, not edits), and as
a plugin's own state holds the modulated values, the bases of modulated
parameters are saved after it (`FFMB` trailer: after the VST3 state's
controller part, or around the AU property list in an `FFAU` wrapper) and
set again on load. Edits are `Command::SetModulators`
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

What the editor works on is an `AudioRef`: an audio clip, or one take of a
take folder (`AudioRef::Take(TakeRef)`; a take is an audio clip of its
own) — as do spectral editing, clip effects, clip envelopes and Audio to
MIDI. `Project::{audio, audio_start, audio_name, audio_refs}` read it and
`Project::set_audio` writes it back — a clip's content, or its folder's
with that take changed, one command a clip however many of its takes
change (several takes detected together are one step).
`Action::{OpenPitchEditor, DetectPitch, EditPitch}` and
`Session::{pitch_target, pitch_rendering, pitch_syllables}` take it, drags
recompute from `Session::gesture_audio` (the folder as the gesture found
it), and the polyphonic renderer keeps its detections, renders and settle
times by it, looking over every clip and take for renders due. The editor
names a take "<folder> · <take>" and keeps to it while takes before it
come and go (it holds the take's id).

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
reset, keep formants, length, remove notes, remove the edit). The editor
is `faderframe-view-pitch` (`ViewKind::Pitch`, `Session::pitch_clip`,
`Action::OpenPitchEditor` from the arranger's clip menu: notes as blobs at
the heard pitch where they play now, the played and the sung curve, a drag
moves by semitones (Alt: freely), an edge dragged (handle and resize
cursor on hover; Ctrl+←/→ 10 ms, with Shift 1 ms) makes notes longer or
shorter, a double click splits, ↑↓ move, J joins, Delete removes, R
resets).

Note lengths (`PitchOp::Length { notes, edge, by }`, inside a gesture from
where it began): a note's `PitchNote::timing` is where its start and end
play now, in the frames of the timing as sung (`placed`, `place_of`,
`source_of`); `PitchEdit::move_edge` moves one edge. In a melodic edit the
neighbour on that side gives or takes the time, so nothing else moves —
the next (previous) note when it follows on within `JOIN_HOPS` (100 ms:
the unvoiced transition between them moves along and that note is
squeezed or stretched, its far edge staying), else the pause (squeezed to
`MIN_GAP_HOPS` at most), or the audio up to the clip's edge; notes keep
`MIN_NOTE_HOPS`. `PitchEdit::time_map(lo, hi)` turns the notes' edges into
anchors (placed → source) that `pitch::through` composes with the clip's
own warp anchors in `warped_region`, so PSOLA plays the stretched notes
through the time map (marks repeated or skipped) and renders, freezes and
clip effects follow. Reset gives a note its sung timing back
(`reset_timing`, the joined neighbour's edge too); split and join keep the
timing. Removing notes (`PitchOp::RemoveNotes`) moves them to
`PitchEdit::removed`: no longer shown, their audio silenced where it plays
(`PitchEdit::level_at`: 0 inside, 5 ms fades centred on their edges, as
`PitchCurve::levels` every hop applied to the PSOLA voice's output by
source position). Undo brings them back. Both fields are serde defaults
(no schema bump).

Polyphonic and Melodic (the editor's `EditFlag::PolyphonicPitch`):
switching it detects the clip open in the editor again the new way when it
was detected (or is being detected) the other way
(`Session::pitch_mode_changed`; a detection of the other kind still
running is dropped). The notes found replace the old ones in one "Detect
Pitch" step, but `pitch::carry` brings the corrections over: each new note
takes the move and straightening (formants into a melodic edit, level and
mute into a polyphonic one; a muted note becomes a removed one in a
melodic edit) of the old note it overlaps most — by at least half its
length — within half a semitone; one lying in a removed note is removed;
lengths are not carried. Going melodic, the clip plays its recording again
(the polyphonic render left behind, undo brings both back). The view shows
"finding the notes (polyphonic|melodic)…" while it runs. Tests:
`session/tests/pitch.rs` (`notes_are_made_longer_and_removed`: rendered
pitch where the longer note now plays, silence where the removed one
did), `session/tests/polypitch.rs`
(`switching_between_polyphonic_and_melodic_finds_the_notes_again`), engine
`tests/pitch.rs`, `warped_playback_does_not_allocate` (a longer note and
a removed one), the view's.

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

### Audio to MIDI

`session::to_midi` (`Action::ConvertToMidi { audio, how }`, the arranger's
clip menu "Convert Melody/Harmony/Drums to MIDI", a take lane's "Convert
“Take n” to MIDI") listens to an audio clip's or take's part of its
source in a thread and puts the notes on a new instrument track below it
(the built-in synth, or the Drum Sampler for drums) as a MIDI clip in the
same place (a take's from where it starts in its folder; named
"<folder> · <take> Melody"), in one "Convert to MIDI" step.
**Melody** uses the pitch-editing analysis (`melody::notes`, velocity
from the level). **Harmony** runs Spotify's basic-pitch
(`faderframe-transcribe`, Apache-2.0, `model/NOTICE`): its ONNX graph
(CQT, harmonic stacking, six small convolutions; 36k weights) converted
by `scripts/basic_pitch_model.py` into `model/basic-pitch.ffnn` and run
by `graph::Graph`, an evaluator for the opset-15 operators it uses
(tested against onnxruntime's outputs, `tests/reference.json`); 2 s
windows of 22 050 Hz audio overlapping by 30 frames run on all cores,
then are unwrapped and turned into notes exactly as basic-pitch's
`inference.py` and `note_creation.py` do (onset peaks, inferred onsets,
the energy walk, the melodia trick). The crate builds with opt-level 3
in dev builds. **Drums** takes the onsets (strength ≥ 0.08; a hit at the
very start added) and sorts each hit by its first 60 ms's energy under
150 Hz (kick, C1) or over 5 kHz (closed hat, F#1), else snare (D1),
velocity from its peak.

### Clip effects

An audio clip can carry a chain of devices rendered offline into the
audio it plays (`faderframe_project::clip_fx`: `AudioClip::effects`, a
`ClipEffects` with the chain, the `OriginalAudio` it processes — source,
region, warp, pitch edit, reversal — and `rendered_offset`, the frame in
the render where the original region starts, so trims carry over).
`session::clip_fx` (`Action::OpenClipEffects`, `Action::ClipEffects {
audio, op: ClipFxOp }`, on an `AudioRef`: a clip, or one take of a folder,
whose chain is its own; `Session::clip_fx_target`): an edit changes a
pending chain at once (what the editor shows); after 300 ms of rest the chain renders in a worker on a
one-track copy of the project (the original audio, the chain as inserts,
`render::track_render_project`, 2 s of tail), and the result becomes a
source the clip plays, chain and all, in one "Clip Effects" step; a
render overtaken by a newer edit is dropped. Removing every effect
restores the original (`OriginalAudio::restore`). Devices' parameters
come from an instance made for the asking (`EngineController::
describe_parameters`, cached per plugin). Editor `faderframe-view-clipfx`
(`ViewKind::ClipFx`, a card per device with parameter bars).

A card's "Open Window" (`Action::OpenClipEffectEditor { audio, index,
generic }`) shows the device in the window an insert gets — its own GUI,
a built-in's editor, or the generic one. The engine hosts an instance of
the slot outside the graph, as it does an album song's inserts
(`EngineController::host_plugin`, kept through rebuilds by
`keep_plugins`, pushed to by `sync_plugin_slot`, dropped by
`release_plugin` once the slot leaves its chain). The session answers for
the slot as a plugin's owner (`Session::clip_fx_slot`, the clip or take
and the place in its chain: `plugin_owner` gives the clip's (folder's)
track, `device_name` the chain's name), so the editor
windows work unchanged; the track commands they send about it
(`SetPluginParameter`, `SetPluginBypass`, `SetPluginState`, batches of
them) become chain edits in `Session::edit` (`clip_fx_commands`;
`ClipFxOp::{ForgetParameter, SetState}`). What the plugin's own GUI
changes (its editor edits, a dirty state) is taken into the chain once it
has rested 300 ms — its state, and every value it moved as an explicit
one, since explicit values are applied after the state — and rendered at
once. A bar dragged in the view keeps the device's state.

### Clip envelopes

An audio clip can carry volume and pan envelopes that belong to it and
move with it (`faderframe_project::clip_envelope`: `AudioClip::envelope`,
an `Option<Box<ClipEnvelope>>` — `None` in every literal and in files
that have none —, `ClipEnvelope { volume, pan }` of `EnvelopePoint { at,
value, shape }`). Points are in clip-relative output frames at the
project rate, so the envelope stays with the clip's audio: a start trim
shifts it by what came off (`warping::trim_audio_start`; points left
outside the clip are kept, so a trim back finds them again), a split
gives each piece its part with a point at the cut holding the value
there (`ClipEnvelope::slice`, in `Clip::split_at`, so every edit built on
splits — Separate, Strip Silence, slices, ripple cuts, range copies —
keeps it), a time-stretch trim scales it (`scale`), a slip leaves it
where it is (like the fades and the gain). Segments have track
automation's shapes (`faderframe_automation::CurveShape`, `interpolate`);
volume is in dB, silent at or below `FLOOR_DB` (−96), at most `MAX_DB`
(+24); pan is −1…1 and a balance — the far side fades out, the near one
stays at unity — on stereo outputs only (mono and surround tracks leave
pan out). `Command::SetClipEnvelope { clip, envelope }` replaces it
(sorted and clamped on apply; refused on MIDI clips and take folders;
label "Clip Envelope", `CoalesceKey::ClipEnvelope`, Impact Timeline);
aliases mirror it and frozen tracks refuse it like any content edit. A
take's envelope is its clip's (`Take::audio.envelope`): it changes with
its folder's content (`Project::{set_take_audio, set_audio}`), comp
pieces get their part of it (`AudioClip::cut`), and the session's
`clip_envelope`, `Action::SetClipEnvelope { audio, lane, points }` and
`Action::ClearClipEnvelope { audio, lane }` take an `AudioRef`.

The engine applies it as the clip plays, sample by sample: the snapshot
places the points at engine frames (`snapshot::RegionEnvelope` on the
`AudioRegion`), and the clip player works out a region's gains once per
chunk of 128 samples on the stack (`clip_player::Gains`: clip gain ×
fades × volume, per side × the pan's balance) instead of per channel and
sample — the same numbers as before for clips without an envelope, and
the same whatever the block size. Every path uses them: plain, streamed,
resampled and reversed regions, varispeed warps and the stretcher and
PSOLA voices. It comes after clip effects and spectral edits (they are
rendered into the source the clip plays), so a ride never waits for a
render. ARA-rendered tracks leave it out like clip gain and fades (the
plug-in reads the files; with one renderer per track there is nowhere to
apply them). Self-freezing's chain key holds the clip content whole, so
an envelope edit makes a cache stale. Tests: `engine/tests/
clip_envelope.rs` (a ride sample by sample, the block size, balance,
moving with the clip), `clip_envelopes_do_not_allocate`, `session/tests/
clip_envelope.rs` (one undo step per drag, trims/splits/stretch, copies,
save and open, MIDI refused), project `clip_envelope::tests`.

**Arranger.** `EditorSettings::clip_envelope` is the lane shown
(`Action::ShowClipEnvelope(Option<EnvelopeLane>)`: Edit → Clip
Envelopes, the clip menu's "Clip Envelope" submenu). While shown, audio
clips draw it over their waveform in the theme's automation colour
(volume on a fader law with 0 dB three quarters up, `EnvelopeLane::
{to_unit, from_unit}`; pan with the centre in the middle) and their body
is `ClipZone::Envelope` for the Smart, Selector and Grabber tools (edges
still trim, the name strip still moves): a click adds a point on the
grid (Alt: free) and drags it, a drag moves a point between its
neighbours (Shift keeps its time, Ctrl fine), a double-click removes it,
a right-click sets its segment's shape or removes it — each one
`Action::SetClipEnvelope { audio, lane, points }` inside a gesture, one
undo step (`view-arranger/src/clip_envelope.rs`; `EnvelopePlace` = the
clip or take, its offset in the clip and the area drawn). In an open take
folder each take's lane does the same for its take (comping by click and
swipe, and the words of comping by lyric, wait until the envelopes are
hidden), and the main lane draws the comp's envelope as played. The clip menu also
clears a lane or all of the selected clips' envelopes
(`Action::ClearClipEnvelope`). Clip-effect parameters are not enveloped
(they are rendered into the audio; an envelope there would mean a render
per point). Dev action `clip-envelope:<track>[@<take n>]=[pan:]<t>/<value>[|…]`
(seconds from the first audio clip's start, or take n's of the first take
folder). Tests arranger `clip_envelope_tests.rs`,
`a_take_lane_edits_its_takes_envelope_while_one_is_shown`, session
`one_take_of_a_folder_has_an_envelope_of_its_own`.

### Strip Silence

`faderframe_analysis::silence` finds the sound between silences: a
signal is measured once into `Levels` (every channel's peak per `HOP` =
64 frames) and `regions(levels, rate, &SilenceSettings { threshold_db,
min_length, pre_pad, post_pad, min_gap })` then gives the `Span`s with
sound at once — blocks at or above the threshold are sound, gaps shorter
than the minimum gap are bridged, spans shorter than the minimum length
go, and what is left is padded (spans the padding joins become one).
`session::strip_silence` (`Action::StripSilence(StripSilenceOp::{Open {
clips }, Settings, Apply(StripMode), Close})`) measures each source of
the dialog's clips once on a helper thread (kept for the session,
`poll_strip_silence` in the tick) and reads a clip's levels through what
it plays (`clip_levels`: the warp map, backwards when reversed), so a
turn of a setting previews at once: `Session::strip_silence_preview()`
(regions per clip in clip frames) is what the arranger shades and
outlines (`paint_strip_preview`). Take folders are left out with a
notice (or, alone, refused) saying to Flatten Comp first: a take is one
clip in its folder and cannot be cut into pieces there. Opened with an edit range (no clips named),
it takes the selected tracks' audio clips in the range, and only silence
inside the range goes (`within_range`: what lies outside is sound). Apply is one undo step: `Strip` takes
the silence out (each region a clip of its own, sliced with
`Clip::slice`, so gain, fades at the outer edges and envelopes carry
over; a clip with no sound goes), `Separate` splits at every region edge
and keeps everything, `NewClips` ("Strip Silence to New Clips", for
dialogue lines and drum hits) puts the regions as numbered clips
("<clip> 01", …) on a new audio track under each clip's track and mutes
the clips themselves. The pieces become the selection and the dialog
looks at them next. The dialog (`faderframe-ui/src/strip_silence.rs`,
Edit → Strip Silence…, the audio clip menu) has the threshold, minimum
length, pads and minimum gap, a summary line and the three buttons. Dev
actions `strip-silence:<track>@<dB>/<ms>` (the track's audio clips,
threshold and minimum length), `strip-silence:apply[=strip|separate|
new-clips]`, `strip-silence:close`. Tests `analysis::silence::tests`,
`session/tests/strip_silence.rs` (synthetic hits: preview, each mode
and its undo, an envelope carried into the pieces), session lib
`strip_silence::tests` (warped and reversed clips), arranger
`the_strip_silence_preview_shades_the_silences`.

### Speech and lyrics

`faderframe-speech` is OpenAI's Whisper in plain Rust (no runtime, no
ONNX): safetensors loading (F32/F16), the feature extractor's log-mel
spectrogram (400-point frames every 160 samples at 16 kHz, the
checkpoint's mel filters, log10 floored 8 below the loudest, (x+4)/4),
the encoder (two convolutions as matrix products, pre-norm blocks, exact
GELU) and the decoder (cached self-attention, cross-attention over the
encoder, logits by the token embedding), matrix products on all cores.
`Whisper::transcribe` follows the reference loop: the spectrogram of the
whole recording plus 30 s of silence, 30 s windows, the language
detected once (or given), greedy decoding with the timestamp rules (pairs,
never back, the first within a second, a timestamp when they are likelier
together than any text token), segments from consecutive timestamps, and
the next window from the last complete segment. Tokens become text
through GPT-2's byte-level vocabulary. The JFK test clip comes out word
for word with whisper-tiny in about a second (opt-in test
`faderframe-speech/tests/jfk.rs`). **Word timings**
(`Options::word_timestamps`, `Segment::words`, `speech::timing`) follow
the reference's `find_alignment`: the window's text read again by the
decoder (prompt, no-timestamps token, text, end) while the checkpoint's
alignment heads (`generation_config.json`'s `alignment_heads`; else every
head of the upper half of the decoder) give their cross-attention scores
(`Model::cross_scores`, the decoder's `forward` hook); each head a softmax
over the window's audio, standardised along the tokens, median-filtered
(7 frames) along time; their mean, negated, is warped through the text
(`timing::dtw`), and where the path enters a token row is where the token
starts (20 ms frames). Tokens join into words at spaces (every character
for zh/ja/th/lo/my/yue), punctuation joins the neighbouring word as in
`merge_punctuations`, a word's probability is the mean of its tokens' from
the greedy decode. The reference's truncation of long words is left out
(sung words are long); the session moves the edges to the voice instead.
whisper-base times "Americans" over 1.8 s as the reference's test wants
(`the_words_are_timed`).

The checkpoint is not shipped: `session::speech` downloads `whisper-base`
(290 MB, Apache-2.0 on Hugging Face) on request (Audio → Download Speech
Model; the system's `curl`, `.part` files) into the data folder's
`models`. `Action::Transcribe(clip)` hears an audio clip's part of its
source at 16 kHz in a thread and puts its lines into the project's lyrics
(`faderframe_project::lyrics`, `Project::lyrics`, `Command::SetLyrics`),
replacing those starting inside the clip, in one step. Whisper's times are
coarse, so each line then starts at the vocal's attack nearest to it
(`speech::word_onsets` = `faderframe_audio_files::onsets` above a strength,
`align_to_onsets` within `ONSET_REACH` 0.35 s, in order; ends stay before
the next line). The arranger's
Lyrics lane (shown once there are lyrics) draws the lines (lit while
playing), a click goes there, a double-click edits, right-click deletes;
`arrange` moves, copies and cuts them with sections; `Session::
lyrics_text` writes LRC or SRT. A transcription also keeps the words
heard in the clip's part of its source (`Project::source_words`, below),
so the pitch editor shows their syllables.

### Comping by lyric

Whisper's word timings on every take of a take folder; the comp chosen
word by word.

**Words by source.** `faderframe_project::lyric_comp::SourceWords` holds
what was heard in a source: `SungWord`s (source frames at `rate`, the
project's; text; `line` = first word of a stretch the model heard as one;
`confidence`) and the source spans heard so far (`heard`; `missing`, `add`
replaces the words starting inside a span heard again). They live in
`Project::source_words` by source (serde default, skipped when empty: no
format change), edited whole by `Command::SetSourceWords` (`Impact::None`,
"Hear Words"/"Forget Words"); templates drop them. By source because a
loop recording's passes share one file: each part is heard once,
whichever takes use it.

**Lining up the takes** (`lyric_comp::align`, pure). `take_words` maps
each take's source words into folder frames (those whose middle lies in
the take's material, cut to it). The take with the most words makes the
first columns; each other take is aligned to the columns by
Needleman–Wunsch on the normalised text (equal +2, different −1, left out
−1); its exact matches are anchors, and between two anchors — where the
text differs: misheard, sung differently, left out — the words are paired
by a dynamic time warping of their onsets measured from the anchor before
(so a take sung late all along still pairs), a pair only where both are
each other's cheapest cell on the path and within `PAIR_REACH` 0.5 s; the
rest become columns of their own, in order. A column's text is the
majority's (one take's spelling), `line` the majority's. `LyricAlignment`
answers `span(c, take)`, `chosen(c, folder)` (the take whose word plays
whole, else the one playing at its middle), `shown_span`, `line(c)`,
`lines()`.

**Comping by words.** `plan_comp(alignment, folder, columns, take)` gives
the stretch's edges: the start goes after both takes' words before it
(the comped take's and the one heard there now) and before both takes'
first word of the stretch, at most `EDGE_REACH` 0.5 s from the word, the
crossfade's half inside the gap where it fits (words that run together:
right at the word's edge); the end mirrored; a comp boundary already in a
gap is used again (no sliver of a third take between two words); no edge
where the take already plays on that side. The session
(`Session::place_edge`) puts each edge at the quietest point of its gap in
both takes (`analysis::sung::quiet_point`: summed 5 ms energy, nearest the
word among the candidates within 1 dB of the least), then on the comped
take's zero crossing within `ZERO_CROSSING_REACH`; `apply_comp` is one
`TakeFolder::set_comp`, so the folder's invariants hold, and the engine
plays its crossfades (centred, `crossfade`) at the new boundaries. Every
comp is one `SetClipContent` step ("Comp “light” from Take 2").

**Hearing** (`session::lyric_comp`, `Action::LyricComp(LyricCompOp::Hear
| HearAgain | Comp | CompLine | UseBest | Forget)`). A request runs in a
thread, one at a time (Whisper uses every core): per source the folder
uses, its mono mix, the parts not heard yet at 16 kHz through the
`Transcriber` (`WhisperTranscriber` on the downloaded checkpoint, with
word timings; `Session::set_transcriber` gives tests a stand-in), the
words' edges moved to where the voice is (`sung::refine`: from the first
to the last 10 ms frame within 30 dB of the word's loudest and 8 dB over
the floor around it; the start looked for from 150 ms before to the end of
the span heard — the model often has a word start in the pause before it,
and sound before a `PAUSE_SECONDS` 0.15 s pause inside the span is the
last word's tail —, the end at most 150 ms past, never into a
neighbour; three takes of the JFK clip, one 120 ms late, come out with
their words 120 ms apart), and every word
measured (`sung::measure` with the part's pitch track). The words go in as
one "Hear Words" step; the measurements stay in the session
(`LyricCompState::sounds` by source and word span) — a project opened
again is measured again from the tick for open folders whose sources have
words (`poll_lyric_comp`, once per source). A project replaced while a job
runs drops its result (`project_serial`). Hear Again forgets the folder's
spans first. `Session::lyric_comp(clip)` → `LyricComp { alignment,
judged, best }`, kept in a memo keyed by the takes, their words, the
measurements' revision and the tempo map.

**Suggestions** (`sung::judge`, deterministic, never applied unasked).
Per word, each take that sings it: tuning (share of the voiced middle 70 %
within `IN_TUNE_CENTS` 25 of a semitone) and steadiness (RMS cents round
its median) when any take is voiced; the start's distance from the
sixteenth-note grid; level against the other takes' median (quieter than
3 dB, louder than 4 dB costs); level over the floor around it; clipped
samples (−3, −4 past 20); a plosive pop (the loudest 10 ms below 120 Hz
at the start, 6 dB over the other takes' and within 10 dB of the word).
The best score wins (ties: the first take); every take keeps its
`Reason`s, strongest first, as text ("in tune (94 %)", "late (45 ms)",
"plosive pop (9 dB more low end)").

**Arranger** (`view-arranger/src/lyric_lane.rs`). Open take folders with
words show each take's words in a strip at the bottom of its lane (the
chosen take's word lit, the suggested one outlined): a click uses that
take for the word (`Hit::TakeWord`), Shift+click for the words from the
last one clicked (`word_anchor`), a double-click for the line; right-click
offers the word, the line, the reasons. Under the lanes the lyrics comp
row (`LYRIC_ROW_H` 22 px, `Hit::LyricWord`/`LyricRow`) lists the line word
by word in the chosen take's shade with its number, ★ where another take
is suggested; a click uses the suggestion, right-click lists every take
with its reasons (the chosen one checked) and the line's suggestions;
while hearing it shows the progress. The folder's clip menu has "Comp by
Lyrics" (it opens the lanes), the lane menus Use All Suggested Takes /
Hear the Words Again / Forget the Words. The row sits between the take
lanes and the automation lanes (`lyric_row_h` in `update_rows` and
`lane_geoms`).

**Pitch editor.** `Session::pitch_syllables(clip)` pairs the clip's pitch
notes (source frames) with its source's words: a note belongs to the word
its start is in (from 50 ms before it); the word's syllables
(`leadsheet::build::syllables_in`, the language by `Language::of`) are
dealt over its notes — extra notes hold the last syllable (a line under
them), too few notes take the rest on the last. The view draws each under
its note ("Beau-", "ti-", "ful"); its toolbar's Hear Words asks for the
clip's words.

Dev actions `comp-lyrics:<track>`, `comp-word:<track>=<word>[-<word>]@<take
n>` (words from 0, takes from 1), `comp-line:<track>=<word>@<take n>`,
`comp-best:<track>[=<a>-<b>]`, `log-lyric-comp:<track>`. Tests:
`project/src/lyric_comp/tests.rs` (spans, mapping, the alignment with a
misheard and a missing word, a late take, phrases, the gaps, reuse of a
boundary, invariants after word comps, the file), `analysis::sung` tests
(edges, measurements, the judge, quiet point and crossing),
`speech::timing` tests (median filter, DTW, token starts, words),
`session/tests/lyric_comp.rs` (three synthetic takes heard by a stand-in
transcriber: words, edges, suggestions with reasons, comps with
boundaries in the gaps on zero crossings, the line, undo, hearing again,
no model, saved and measured again, the pitch editor's syllables; opt-in
`whisper_hears_the_takes` with `FADERFRAME_TEST_WHISPER` /
`FADERFRAME_TEST_SPEECH`), arranger `lyric_tests.rs`, pitch view
`the_syllable_sung_on_each_note_shows_under_it`.

### Stem separation

`faderframe-separate` splits a stereo recording into vocals, drums, bass
and the rest with Sony's CrossNet-Open-Unmix (X-UMX; Sawata, Uhlich,
Takahashi and Mitsufuji, ICASSP 2021), in plain Rust, as asteroid's X-UMX
recipe does it (`egs/musdb18/X-UMX/eval.py`). The mixture's STFT
(`stft::forward` = `torch.stft`: 4096-point periodic Hann frames every
1024 samples, centred, reflected at the ends) gives magnitudes; per
source the bins up to `max_bin` (16 kHz) are shifted, scaled and encoded
(linear, batch norm, tanh) to 512 features, the sources' encodings are
averaged (the first bridge), each source's three-layer bidirectional LSTM
runs over the average, each source's encoding and LSTM output are
averaged again (the second bridge) and decoded into a ReLU mask for every
bin and channel (`xumx`). The estimates (mask × magnitude) go through
norbert's multichannel Wiener filter (`wiener`: the mixture's phase first,
then EM iterations — per bin a spatial covariance per source from the
frames, `v·R·C⁻¹·x` per frame — in double precision, the spectra scaled
to a tenth of the loudest bin; one iteration by default) and the inverse
STFT (`stft::Inverse` = `scipy.signal.istft`). Matrix products run four
rows per weight pass on all threads, the eight LSTM recurrences (four
sources, two directions) side by side; the filter keeps nothing between
iterations but the covariances (a frame's estimate depends only on its
own mixture, its magnitudes and the earlier covariances, so they are
recomputed), and frames are separated and inverse-transformed in runs
into buffers of their own. Recordings longer than 34 s go in 30 s
segments with 2 s of context either side, crossfaded (sin²/cos²) over
1 s around each boundary; up to that a recording is separated exactly as
the reference does it. With at least one EM iteration the stems add up
to the mixture but for what the filter's regularisation keeps back where
every estimate is silent (below −60 dB).

The weights are read from the published PyTorch checkpoint itself
(`torch`): its zip directory (ZIP64 too, stored entries only, each
entry's data after its own local header, which `torch.save` pads for
alignment) and a pickle machine that knows the opcodes `torch.save`
writes and builds plain values — tensors as descriptions of their
storages, every other class a named object holding its arguments;
nothing a pickle names is called, so a checkpoint is data only. Batch
norms fold into affine maps, the output scale and mean into the last one.
`scripts/separation_reference.py` (asteroid's model definition cut to
inference, norbert, scipy; `torch.load(weights_only=True)`) writes a tiny
X-UMX with random weights (`tests/data/tiny-xumx.pth`) and what it gives
for a synthetic test signal the Rust tests make as well
(`tests/reference-tiny.json`: sampled estimates, frame sums, sampled
stems and block RMS after 0, 1 and 2 iterations), and with the published
checkpoint `tests/reference-xumxl.json` for the opt-in test: X-UMXL's
estimates agree to 4·10⁻⁵ (relative) and its stems to 4·10⁻⁶ of the
peak; its 3 s separate in 0.3–0.7 s on 16 threads here.

The model (`models::X_UMXL`: Zenodo record 7128659, 142 MB, CC BY 4.0,
trained on 1505 of Sony's own songs, no MUSDB18) is not shipped:
`session::separate` downloads it on request into the data folder's
`models/x-umxl` (the system's `curl`, a `.part` file checked against
Zenodo's MD5 before it takes its name; the status bar and Preferences →
Stems show its progress by the part file's size) and checks it again
before loading. Asking to separate before it is there downloads it
first and starts the separation when it arrives. The model catalogue
carries each model's licence, attribution, training data and any
caveat, listed in About and in `THIRD_PARTY_LICENSES.md` (models
downloaded on request).

`Action::SeparateStems { target: StemTarget::{Clip, Track}, model, stems
}` renders what it separates as it plays (the clips with gain, fades,
warps and takes on a track of their own without devices, as Merge Clips
does; a clip asked for by name even when muted, a track's unmuted audio
clips over their span), then in the worker takes it to the model's rate,
separates it (all but one core), brings each stem back to the project's
rate (exactly as long) and writes it as a float WAV into the media folder.
"Other" takes what the rest leave of the mix — what the model cannot hear
above its band at higher project rates included — so the stems add up to
what was separated exactly; `Stem::Instrumental` is the mix less the
vocals. A mono track gets mono stems. When it is done the stems arrive in
one undo step ("Separate Stems", "Extract Vocals", "Remove Vocals"):
several on new tracks in a folder "<name> Stems" right after the track
they came from (inside that track's folder), one on a track of its own,
each track sent where that track goes, each stem a clip where the audio
was — and the separated clips muted. A separation whose clips were all
deleted meanwhile drops its files. `StemSeparator` is what the session
separates with: the model, or a stand-in (`Session::set_stem_separator`;
the session tests use one). Separations show in the status bar (stage
and progress, a stop button: `Action::CancelSeparations`). Settings
(`SeparationSettings`: model, Wiener iterations 0–3) live in Preferences
→ Stems with the model's licence and its download.

### Lead sheets

A clip's melody with its chords and words, written out as notation — or
several tracks' lines as a score, a staff each, one below the other, or a
part as played (`faderframe-leadsheet`, pure; `session::leadsheet`; view
`faderframe-view-leadsheet`, `ViewKind::LeadSheet`):

* **Hearing.** `LeadSheetOf::{Clip, Track, Clips, Tracks}`: a staff a
  track (`Clips`: the chosen clips grouped by track, staves in the
  arranger's folder order; `Tracks`: in the order given; a track's unmuted
  clips). A staff's clips make one line on one timeline — gaps are rests,
  where clips overlap the later one (on top) is written, MIDI notes past a
  clip's end are left out. Audio clips are heard polyphonically (see
  below; in a thread, kept by clip with the melody marked, so writing them
  again with another grid or style does not listen again), take folders
  as their comp plays (piece by piece, `TakeFolder::pieces`), MIDI clips'
  notes are read (afresh each time) and their melody picked out the same
  way. Bars come from the meter map over the clips, the chords from the
  chord track — or, with none there, named from the notes under the
  staves' melodies and what the other tracks play (their MIDI, or with no
  MIDI their audio, heard the same way in the same thread; tracks named
  like drums left out), the melodies counting a little
  (`MELODY_WEIGHT`); a single line with nothing under or around it gets
  none, a score with nothing else goes by what its staves play together —,
  the key from the key track (else detected from all the staves and the
  chords, which keep a tune from leaning on its fifth's key), words from
  the lyrics lane, under the first staff whose track is named like a voice
  (else the top one). Each staff's clef follows its middle note (treble;
  treble 8vb for a tenor or a guitar; bass for a bass line).
* **Writing** (`build`, `Input::staves` → `LeadSheet::parts`, the same
  bars in each, chord symbols on the top part): one voice a staff (a staff
  written as played, `Staff::chords`, keeps notes starting together as one
  chord); each beat's grid chosen by cost from its onsets — the coarsest
  of quarter, eighth, eighth triplet and sixteenth (`grids`: compound
  beats dotted quarter, eighth, sixteenth; `Grid::{Auto, Straight,
  Triplets}` limit the choice) that keeps every onset within 50 ms at the
  tempo (`tolerance`, 0.8–1.6 divisions), a finer one only when its
  squared distances plus its reading cost come out lower (`grid_for`: a
  dotted rhythm stays sixteenths, a swing becomes triplets); no note
  shorter than its beat's grid, ends on the grid, short gaps closed
  (legato), so rests are the grid's and come merged; notes cut at bar
  lines with ties and
  split into the values a reader expects (nothing crosses a beat unless it
  starts on one, nor the middle of a 4/4 bar unless it starts the bar;
  compound metres count dotted quarters); spelt in the key with
  accidentals once a bar; beams by the beat (sixteenths' second beams,
  hooks); triplet brackets; chord symbols on the nearest beat, a repeat
  not written again; each lyric line's words matched in order to the
  onsets in the line (by time, words shared out by their syllables), a
  word over several notes drawing its line.
* **MusicXML 4.0** (`musicxml`, `score-partwise`, valid against the
  schema): a `<part>` a staff (a score's in a bracketed `part-group`,
  names and abbreviations, `group-barline` as engraved), harmony with
  kinds and degrees, ties, beams, tuplets, lyrics with extenders, the
  tempo, a final bar line.
* **Engraving** (`engrave`): a display list (filled outlines, lines,
  text; points, y down) of A4 pages — systems of up to four bars (bars
  close up to 80 % to share a line), justified; each bar's spacing is
  planned once for all its staves (`BarPlan`: every staff's onsets and
  the chords are points, each note wants its room from its start to its
  end), so notes sounding together stand one above the other; each note's
  room grows with its value, accidental, dot, word and the chord over it.
  A score's system draws each staff on its own, measures what it reaches
  and stacks them clear of each other (room for chords over the top
  staff, words under a staff that has them), bracketed, part names in
  full before the first system and short (`Part::abbreviation`) after,
  bar lines through the system unless words are sung under a staff above
  the last (`LeadSheet::bar_lines_joined`); a system that does not fit
  starts the next page; `Layout::for_staves` makes staves smaller as a
  system has more. Glyphs are 34 of Bravura's (SIL OFL 1.1), baked with
  their SMuFL metadata (stem anchors, engraving defaults) by
  `scripts/notation_assets.py` (the bracket is drawn, not a glyph); text
  measured with the Adobe Core 14 widths. **PDF** (`pdf`): a hand-written
  PDF 1.4 with the paths and the standard fonts (WinAnsi, nothing
  embedded). The view draws the same display list on paper.
* Actions `MakeLeadSheet { of, grid }`, `ExportLeadSheet(path)` (by
  the extension); the arranger's clip menu (this clip; "Lead Sheet of the
  n Selected Clips" on one track, "Score of the Selected Clips" across
  tracks) and track menu ("Lead Sheet of This Track"; "Score of the n
  Selected Tracks"); dev actions `lead-sheet:<track>|track:<track>|
  tracks:<a>|<b>…|selected[@straight|triplets]`,
  `export-lead-sheet:<path>`.

**Polyphonic first.** Every audio clip is heard as the Pitch editor's
polyphonic detection hears it — one function for both,
`session::polypitch::hear_notes` (basic-pitch, overtones of louder notes
dropped, notes under 60 ms dropped, each note followed every 5 ms by
`faderframe_polypitch::follow`, which gives its pitch exactly as
`pitch_tracks` and how strongly its harmonics are heard; notes never
followed left out; the editor's notes are bit for bit what they were).
`session::leadsheet::hear_part` turns them into
`faderframe_leadsheet::heard::Heard` notes and `heard` cleans them:
notes followed in under a quarter of their frames go; each is written at
its followed pitch (a sung note a little off, or named a semitone off,
lands on the note sung); a note struck again while it still sounded (its
level falling well under its usual level and jumping back) is split; a
louder note's overtones and subharmonics heard as notes (octave, twelfth
… above, an octave or two below, sounding mostly with it — or with its
pieces struck again — at under 65 % of its activation) go; fragments of one note (the same key, a gap under
35 ms — or 100 ms where a piece is short —, no new attack: the second's
first 60 ms under 1.6 times the quietest of the first's last 100 ms) are
joined; ghosts (20 dB under the loudest note round them, or 3 dB and
shorter than 250 ms and weakly activated) go.

**The melody** (`heard::melody`) is a dynamic-programming skyline: time
is cut wherever a note starts or ends, and in each piece the melody is a
note sounding or a rest. A note's salience is its level against the
loudest note sounding (over 24 dB), its height among them, a voice's
marks — vibrato (the pitch's wobble about its 250 ms mean, level-gated,
counted when its autocorrelation peaks at 4–8 Hz; a vibrato borrowed from
a note it shares partials with left out, `voices`) and a glide into the
note (its first 60 ms clearly heard, all on one side) — and a little its
length. The line enters a note only at its onset, pays for leaps (past a
whole tone, more past an octave) and for leaving a sounding note into
silence; a rest costs less than a weak note where notes sound together
and a full note's worth where one sounds alone (a solo line keeps every
note); a second pass also pays for straying from the register the first
pass sang in. `heard::octave_suspects` finds melody notes an octave off
both close neighbours; `hear_part` weighs that note's octave below, its
own and its octave above with `follow` (a note named an octave high hears
its octave below more strongly than itself — its fundamental's odd
harmonics sit there; one named low hears its octave above more), and
`fix_octaves` moves it. `heard::voiced` marks the melody's notes; every
note is kept (`Voiced { start, end, key, level, melody }`).

**Chords** (`faderframe_leadsheet::harmony::chords`): each beat's
pitch-class profile (every note weighted by its overlap and level, the
lowest notes also into a bass profile) scored against templates (major,
minor, the three sevenths, diminished, half-diminished, sus4, sus2,
augmented: the share on the chord's tones, root and third full, the fifth
less, minus the share off them and a price per missing tone, rarer
qualities paying more, the key's diatonic chords — a minor key's major
dominant too — preferred, roots outside the key less likely), then a
Viterbi path that changes chord on a bar line cheaply, in the middle of a
bar at a price and on other beats only when the evidence is strong, so
the harmonic rhythm keeps to the bar or the half bar and passing notes do
not flicker; silent beats keep the chord before; a chord whose bass stays
on another of its tones (70 % of the bass profile) is written over it.

**As played** (`Notation::AsPlayed`, the strip's "As Played"): every
note, notes starting together on one stem; a part named like a keyboard,
or one that plays chords and reaches well below and above middle C
(`build::wants_grand_staff`), goes on a grand staff — `build::hands`
splits it (E4 up right, below G3 left, between to the hand nearer to
where it has been playing, the hand that took the last such note keeping
the next — hysteresis —, no hand past a tenth in one chord) into two
`Staff`s, the upper with `brace` (treble), the lower nameless (bass).
`Part::brace` / `LeadSheet::{instruments, lower_staff}`: the engraver
draws the brace (`brace`, two Bézier halves) left of the system line,
names the instrument once between its staves and runs the bar lines
through both (unless words are sung under the upper); a score's bracket
moves out past it. MusicXML writes the pair as one part of two staves
(`<staves>2</staves>`, numbered clefs, the right hand, a `<backup>`, the
left hand on staff 2 in voice 5). Changing the style rewrites the sheet
from what was heard (`Kept` no longer depends on it).

**Measured** (`session/tests/leadsheet_hearing.rs`: synthetic recordings
whose every note is known — an inharmonic, decaying piano-like tone with
block chords a half bar and a tune on top; a voice-like line, a glottal
saw through "ah" formants, scooped, with vibrato, up to 12 cents off, over
an organ-like pad; a plucked arpeggio left ringing — each heard, written,
scored against what was played; old = the monophonic pipeline this
replaced):

| | piano | voice | guitar |
|---|---|---|---|
| note F1 heard, all notes (old: monophonic / raw basic-pitch) | 0.97 (0.11 / 0.72) | 0.95 (0.00 / 0.71) | 0.96 (0.04 / 0.44) |
| melody F1 heard (old) | 1.00 (0.14) | 1.00 (0.00) | – |
| melody written right / written notes right (old) | 1.00 / 1.00 (0.21 / 0.10) | 1.00 / 1.00 (0.00 / 0.00) | – |
| beats with the right chord, root and major/minor (old) | 0.94 (0.00) | 1.00 (0.00) | 1.00 (0.00) |
| values shorter than the music (old) | 0 (71) | 0 (29) | 0 (11) |
| as played: notes written where played | 0.97 (grand staff) | 0.95 | 0.96 |

The test asserts thresholds the old pipeline fails (heard F1 ≥ 0.8 and
above the raw transcription's, chords ≥ 0.85, melody ≥ 0.9 / 0.85, no
value shorter than the music, at most two ties a bar, a quarter of an
accidental a bar; as played ≥ 0.75, the piano on two staves).
Unit tests: `heard::tests` (a solo line, the top voice over block chords,
a sung line over a pad's higher top, fragments/re-attacks/partials/ghosts,
pitch and voice marks, octave slips), `harmony::tests` (a bar each, half
bars and passing notes, an arpeggio and silence, slash chords),
`build::tests::{each_beat_is_written_on_the_coarsest_grid_that_fits,
a_piano_part_is_split_between_the_hands}`,
`musicxml::tests::a_grand_staff_is_one_part_of_two_staves`,
`engrave::tests::a_grand_staff_is_braced_and_barred_through`,
`faderframe-polypitch` `followed_notes_carry_their_levels_and_octaves`,
session `a_piano_part_is_a_lead_sheet_or_a_grand_staff_as_played`, the
view's `the_strip_switches_to_tab_and_offers_tunings` (As Played) and
`the_strip_fits_beside_its_title`. Not yet: several voices on one staff
(a held note under a moving line is cut at the next onset), pedal marks,
rolled chords, ornaments.

**Words a syllable a note.** `build::lay_words` sings each line's words
on the notes starting in it: cut into syllables (`syllables_in`: between
vowel groups, English or German rules by `Language::of` the lyrics —
German keeps a final e and sends the last consonant on, "sch"/"ch"/"ck"
together) when the line has notes enough, else a word a note; matched in
order to the nearest onsets by a least-distance alignment of the
syllables spread over the line. `score::Lyric::syllabic`
(single/begin/middle/end) gives MusicXML's `syllabic`; the engraver's
`draw_words` puts hyphens between a word's syllables (after the last on a
system when the word goes on) and melisma lines only after a word's last
syllable.

**Guitar tab.** `faderframe_leadsheet::tab`: `TUNINGS` (guitar standard,
drop, open and 7-string tunings, 4/5-string bass, ukulele; string 1 =
the top line first), `finger(chords, tuning, capo)` = a Viterbi search
over each chord's candidate shapes (a string a note; a shape costs its
stretch beyond four frets, its height on the neck, strings skipped inside
it and crossed voicings, less for open strings; moving costs the hand's
travel; out-of-reach notes move by octaves, more notes than strings keep
the outer ones), `text` = plain-text tab (a row a string, bars, words
under). A `build::Staff` with a `StaffTab` keeps its chords (notes
starting together on the grid are one event: `Event::chord`, fingered
into `Event::frets`) and becomes a notes part and a tab part
(`Part::tab: Some(TabStaff { strings, capo, rhythm: false })`, no words)
or a tab part alone (`rhythm: true`, its words and chord symbols on it);
guitars write on the treble 8vb clef, basses on the bass 8vb clef
(`Clef::Bass8vb`). The engraver draws chords on one stem (seconds beside
it, accidentals in columns, a tie a note, stems from the far note), a
tab staff's lines `TAB_GAP` apart broken around fret numbers (a tied
note's bracketed), the SMuFL tab clefs and, alone, the rhythm as stems,
beams, flags, dots and threes under it; staves have their own heights
(`staff_height`). MusicXML: `<chord/>` notes, a TAB clef with
`staff-details` (`staff-tuning` from the bottom line, `capo`) and
`technical` string/fret. The session's `SheetStyle` (`Notation::{Standard,
WithTab, Tab, AsPlayed}`, tuning, capo; `Action::SetSheetStyle` rewrites
the sheet made from what was heard, `Action::MakeTab` = notes with tab)
applies tab to every staff, or in a score to those named like fretted
instruments (`fretted`; a bass part on a bass tuning, `Tuning::for_part`);
those staves are written as played (every note heard, chords kept), the
others as the style says. Arranger clip/track menus: Guitar Tab…; the
Lead Sheet view's strip: Melody / As Played / Notes + Tab / Tab, the
tuning and capo menu, Export Tab… (`.txt`). Dev actions
`guitar-tab:<track>`,
`sheet-style:<notes|as-played|notes-tab|tab>[@<tuning n>][/<capo>]`. Tests:
`tab::tests` (open C and G shapes, a run in one position, capo, octaves,
basses), `build::tab_tests` (syllables in both languages, chords kept,
engraving, MusicXML, text), session
`a_guitar_staff_becomes_tab_with_the_words_on_the_voice`,
`recorded_chords_are_heard_as_chords_for_tab`, the view's
`the_strip_switches_to_tab_and_offers_tunings`. The tab clefs and the bass
8vb clef were added to `scripts/notation_assets.py` (Bravura 1.482; the
regenerated assets otherwise byte-identical).

### The score editor

MIDI clips edited as notation (`faderframe_leadsheet::notate`,
`faderframe_project::notation`, `session::score`, view
`faderframe-view-score`, `ViewKind::Score`). The score and the piano roll
are one model: the score writes the clips' notes and every edit in it is
an edit of those notes, so neither keeps a copy of the other.

* **What it shows** (`ScoreOf`): the clip open in the piano roll (else the
  first selected MIDI clip), a clip, or tracks' MIDI clips (not the
  launcher's), a staff each in folder order; a keyboard's part (named so,
  or `build::wants_grand_staff`) on a grand staff split by `hand_of` (the
  lead sheet's `hands`, note by note). Bars from the meter over the clips
  (in note input one more after the last, to go on writing); unmuted notes
  inside their clip; the key from the key track, chords from the chord
  track.
* **Writing** (`notate`, beside the lead sheet's `build` and on its
  pieces): each beat's grid by cost as the lead sheet's, but an end written
  on a grid counts as an onset (an entered rest after a dotted eighth stays
  a sixteenth's rest) and a played end, off every grid, does not; a
  staccato note is read at its written value (twice what it plays). Notes
  starting and ending together are a chord; a chord sounding while another
  starts — by more than legato playing's overlap (half the note or half a
  beat, whichever is less) — goes to the second voice; where the voices
  sound against each other the higher line is the first (the whole
  overlapping run turned over when the second sits higher), a note given
  a voice keeps it, a third line is cut where its voice's next note
  starts. In each voice notes starting together are merged, overlaps cut,
  a played note's short gap closed when the clip reads legato
  (`Display::legato`; a rest written on the grid always stays). Then bars,
  values, ties across bar lines, beams and triplets as the lead sheet's —
  three quarter triplets fill a pair of beats under one bracket rather
  than tying eighth triplets across the beat — and accidentals by key over
  both voices in the order they sound. The second voice is only written in
  bars where it has notes. Every event carries the notes it was written
  from (`Event::sources`, a note a pitch), and `Notated::written` says
  where each note went (part, voice, start and end on the grid).
* **Engraving.** The lead sheet's engraver with a second voice (`Measure::
  under`; stems up and down, rests raised and lowered, ties bowing by
  voice, the bar plan counting both), articulations (Bravura's glyphs
  opposite the stem, at the voice's side with two voices) and dynamics
  under the staff, a strip layout (`Layout::continuous`: one system of
  every bar at its own width on one page as large as it, no heading), and
  a map of what went where (`engrave_mapped` → `Map`: each system's page,
  staves and bars with the x of every time drawn, each event's notehead
  centres with their ink and their accidentals'): `Map::event_at` finds
  the note or rest under a point, `place_at` the staff, bar, time and line,
  `x_of` the x of a time. The PDF and MusicXML writers are the lead
  sheet's (a second voice after a `<backup>`, articulations, dynamics as
  directions).
* **What MIDI does not say** lives beside the clips
  (`Project::notation`: `ClipNotation { grid: ScoreGrid, legato, notes:
  NoteNotation { voice, step, lyric, marks, dynamic } }` by clip and note,
  `Command::SetClipNotation`, Impact None): the voice a note is written in,
  the letter it is spelt on, the syllable sung on it (a trailing `-`: the
  word goes on, `_`: held), its articulations and dynamic; the clip's
  display quantise. Entries of notes that are gone stay while the session
  runs (an undone delete brings the note back as written) and are dropped
  on opening (`repair_notation`).
* **The score in the session** (`Session::score()` → `Arc<ScoreDoc>`: the
  sheet, pages, map, bar starts, where each note went and its clip) is
  made again only when what it reads changes — its clips' notes and
  notation, the meter, keys, chords, the layout — hashed each time it is
  asked (cheap; a fader move keeps the same document). Paint and events
  ask freely.
* **Editing** (`Action::Score(ScoreOp)`, each edit one step of the clips'
  `SetClipContent` and `SetClipNotation`). Entry puts notes of the chosen
  value (`ScoreSettings`: value, dots, triplet, voice) at the cursor
  (`ScoreCursor { part, at }`) in the chosen voice, replacing what that
  voice had there (notes starting inside go, one sounding into it is cut),
  played as loud as the dynamic in effect (else the note before); letters
  (`ScoreKey::Letter`) take the key's accidental and the octave whose place
  on the staff is nearest the note before (a fourth up beats a fifth down,
  as MuseScore does; an empty staff from its middle line), Shift adds to
  the chord entered last above its top note, `0` writes a rest, `+` ties
  (lengthens the note), a click on the staff in note input enters the
  line or space there (`EnterAt`, the time snapped to the value). A note
  past its clip lengthens the clip to the end of the bar. Outside note
  input, values and voices go to the selection (`SetDuration` replaces what
  the voice had inside the new length). Transposition spells by direction
  (a semitone up sharp, down flat, the key's own notes as the key has
  them; octaves keep the spelling; steps go by the key), accidentals keep
  the letter, respelling cycles a note's enharmonics (`notate::spellings`).
  Dynamics set the velocity (`Dynamic::velocity`) of the marked note and
  the notes after it on its track until the next dynamic; staccato plays
  half the written value (`STACCATO_LENGTH`), accents (+20) and marcato
  (+30) play louder, tenuto plays the whole value, and a track with an
  articulation map gets the attribute named like the mark too. Words are
  typed a line at a time from a note on (`Lyrics`: a syllable a note of
  its line, hyphens join syllables, `_` holds the word before); chord
  symbols go to the chord track until the next chord (`ChordSymbol`).
  Quantize to the Score moves notes to where the score writes them (a
  note locked to time stays); display quantise is per clip and leaves the
  MIDI alone. Step input from a MIDI keyboard is the piano roll's
  (`StepInput`), at the score's cursor with the score's value.
* **The view** keeps only its scroll, zoom and pointer: pages (fitted to
  the width) or a strip scrolled sideways, paper and ink from the theme's
  keyboard colours, selected noteheads drawn again in the accent from the
  map's ink, the cursor and step input's cursor, a ghost note under the
  pointer in note input; the palette (note input, values, dot, triplet,
  rest, tie, accidentals, voices, dynamics, articulations, words, chord
  symbol, display, pages/strip, step input, PDF/MusicXML export) wraps
  with the width; MuseScore's keys where they exist (A–G, 3–7, `.`, T, 0,
  `+`, arrows with Ctrl for octaves and Alt for steps, J, Alt+1/2, N,
  Ctrl+L words, Ctrl+K chord, Ctrl+A/C/X/V); a note's menu holds the rest.
  Arranger menus: "Edit as Notation" (a MIDI clip), "Edit This Track as
  Notation" (and the selected tracks); View → Score (Notation Editor).

Not done yet: a voice-level rest or note entry inside an existing chord
of another length (a chord's notes share one length), 32nd notes and
other tuplets than triplets, cross-staff notes and a grand staff's notes
kept on the staff they were entered on (the hands are split by pitch),
percussion staves (drum maps' notes are written as pitches), slurs,
hairpins, repeats and text, engraving only the systems an edit touched
(the document is rebuilt as a whole when its notes change), and screen
reader nodes for the score.

Tests: `notate::tests` (quarters from their notes, a held note under a
moving line is a second voice either way up, legato overlaps stay one
voice, a tie across the bar line selects one note, accidentals by key and
bar, a letter given, staccato read at its value, written rests stay and
played gaps close, triplets and quarter triplets, syllables, a voice
given, chords, marks and dynamics on the first piece, MusicXML's
backup), `engrave::tests::{the_map_finds_what_was_drawn,
a_strip_is_one_system_as_wide_as_its_bars, two_voices_share_a_staff}`,
project `notation::tests`, session `score::tests` and `tests/score.rs`
(typed lines, values/dots/rests/ties, chords above, letters by key and
arrows spelling by direction with respelling, entry replacing its voice,
a second voice, value keys on the selection, dynamics until the next,
articulations played and written, words, chord symbols, quantising to the
score, the piano roll's edits and the score's on the same notes, pointed
entry, tracks and a grand staff, the document kept until its notes
change, step input's value, PDF/MusicXML export and the strip), view
`faderframe-view-score` tests (a click selects and places the cursor,
selected heads drawn in the accent, note input clicks, typed keys, the
palette's values and words and Ctrl+K, a note's menu, a piano roll edit
redrawn, the strip scrolling, the empty view). The articulation and
dynamic glyphs were added to `scripts/notation_assets.py` (the
regenerated assets otherwise byte-identical).

### DAWproject files and MusicXML scores

Projects travel to and from other programs as DAWproject files (Bitwig's
open format, version 1.0, MIT); scores come in as MusicXML. The formats
live in the pure crate `faderframe-interchange`: `zip` (stored and
deflated entries, ZIP64, CRC-checked reads, a stub in front tolerated;
large entries streamed stored with their CRC patched into the local
header, which Java's stream readers need), `xml` (an element tree written
indented and escaped), `dawproject` (a model mirroring `Project.xsd`,
written in the schema's element order — an extension's elements after its
base's, the lower-case `markers` where timelines nest, no empty
`Markers` — and read leniently: unknown elements and unreadable
attributes are noted once each, never fatal; `file` = the container),
`presets` (`.clap-preset` = `clap`, a big-endian id length, the id, the
state; `.vstpreset` with its `Comp`/`Cont` chunks; VST3 class ids as
`FUID::toString` writes them, the COM byte order swapped on Windows) and
`musicxml` (the score reader). The schemas are vendored under
`data/dawproject` for the tests only.

Export (`session::dawproject`, File → Export DAWproject…): plug-in states
are taken on the control thread, the documents built there
(`dawproject::export`), and a job writes the container beside its
destination — media copied in as they are, generated sources rendered at
the project's rate, states in their own files — before it moves into
place as every export does. Tracks nest as folders do (a folder bus is a
`tracks` track with a submix channel, a plain folder one without); each
channel carries volume (linear, up to the fader's +12 dB), pan
(normalised), mute, solo, its destination, sends (pre/post; a pre-effects
send goes as pre-fader) and its devices in playing order: CLAP and VST3
plug-ins with their states as the plug-ins saved them, built-ins as
`BuiltinDevice`s (vendor FaderFrame) with every kept parameter and their
own state, LV2 as generic devices (`lv2:` ids); a device's parameters
with values or automation are listed with their ids. The arrangement is
in beats: MIDI clips hold notes (velocities at 16 bits when they have
them, release velocities, per-note pitch/pressure/timbre/volume/pan) and
controller lanes as expression points at full resolution; audio clips
hold their file in seconds — a FaderFrame clip keeps its samples whatever
the tempo does, so its content time is seconds and a warp is a `Warps`
in seconds — with fades and clip gain (a `gain` expression beside the
audio); a take folder goes as its comp, a clip in the folder's clip per
piece. Automation becomes points on the parameters (fader lines, linear
in dB, and curved segments get points enough for a straight line between
them to stay within reach); markers stay markers and sections become
markers saying where they end (their comment), which FaderFrame reads
back as sections. Tempo points (ramps as linear) and time signatures
follow. What has no place — keys, chords, lyrics, the launcher, cue
mixes, modulators, containers, SysEx, articulations, muted notes,
reversed clips, pitch edits — is counted in the report.

Import (File → Import DAWproject…, a new project the way New makes one;
unsaved changes are asked about first): a job reads the documents,
decodes every audio file the timelines name (from the container, or
beside it when external) into a new scratch media folder at the session's
rate and reads the plug-in states; `dawproject::import` then builds the
project. Every timeline is followed wherever it nests through a context
that takes its local times to the arrangement's beats (`Clock`: beats add
in beats, seconds in seconds through the tempo map), with the window a
clip lets through: clips in clips (Bitwig's audio clips), lanes, warps
(anchors on the arrangement in samples against frames of the file, cut
at the window — a FaderFrame warp, or none when the speed is one), loops
played out pass by pass, linked clips (the timeline they share becomes a
clip link). Notes of one top-level clip make one MIDI clip; each audio
piece makes an audio clip (kept on its beat when it starts at its clip's
edge), with the clip gain and the fades of the clip whose edge it shares.
A notes track without an instrument whose channel plays another notes
track's instrument becomes a MIDI track; an input-stage built-in goes back
to the preamp slot. Automation finds its parameter by id wherever its
points are and is taken to FaderFrame's units (gain to dB — the line in
gain followed, then thinned —, normalised pan to −1…1, normalised
plug-in values by their range, `Enabled` to bypass). Generic devices of
other programs (their EQ, compressor, gate, limiter), VST2 plug-ins,
launcher scenes and video are listed as not carried.

MusicXML (`faderframe_interchange::musicxml`, `session::musicxml`, File →
Import MusicXML Score…, also by dropping a `.musicxml`/`.mxl`): partwise
and timewise documents, compressed `.mxl` through its container, UTF-16
text. Each part's measures are read into events at their places in the
bar (`<backup>`/`<forward>` for voices and staves, chords, durations as
written — tuplets are in them —, transposing instruments at sounding
pitch, unpitched notes on their instrument's MIDI key), then the bars are
laid out in the order they are played (repeats with `times`, numbered
endings; verse n sung on pass n), a short first bar placed late in a
whole bar so bar lines stay, other odd bars given a time signature that
counts them. Velocities come from dynamics (marks on MuseScore's scale,
`<sound dynamics>`, a note's own `dynamics`; wedges ramp to the next mark
that goes their way, `sf`-like marks accent one note, `fp` and kin then
drop), articulations shorten and accent (`ARTICULATIONS`); ties join;
grace and cue notes are left out and da capo, dal segno and codas noted.
A part becomes an instrument track with one clip (its MIDI program at the
start, percussion on channel 10); a grand staff is one part, so one
track; a tablature part repeating the part above it is left out. The
score's tempo and metre are taken when it is imported at the start of a
project without clips; its keys go to the key lane, its chord symbols
(kind and degrees to `Quality`, bass notes kept) to the chord track and
the words of the first part with words to the lyrics, joined into words
and broken into lines where the score ends them (`<end-line/>`, which the
lead-sheet writer now writes) or at rests of a beat and before a line
would pass two bars — all one undo step "Import MusicXML". A lead sheet
FaderFrame wrote reads back note for note with its chords, key, metre,
tempo and lines.

Tests: the crate's (`zip`, `xml`, `presets`, `dawproject::tests` — the
model reads back as written, Bitwig's README file reads, the written
documents pass `xmllint` against the vendored schemas —,
`musicxml::tests` — a song with a pickup, ties over the bar, a triplet,
voices, words and chords; dynamics and articulations; repeats and
endings with verses; a grand staff; timewise; `.mxl` and UTF-16; odd
bars; a lead sheet read back), `session/tests/dawproject.rs` (the demo
out and back; a project with a warped clip, CLAP and VST3 states,
automation and MIDI 2.0 values out and back; another program's file;
cancelling) and `session/tests/musicxml.rs`. Dev actions
`export-dawproject:<path>`, `import-dawproject:<path>`,
`import-musicxml:<path>`, `dawproject-report:x`.

### Tempo through hit points

For scoring to picture (`faderframe_timeline::hits`, pure; `session::hits`;
the GTK form `dialogs::tempo_from_hits`): markers are the hits — all of
them, the cuts found in the picture ("Cut n" markers from cut detection)
or those in the edit range. Between two hits the music plays a whole
number of steps (beats, eighths or bars, by the project's metre), so each
stretch has the tempo `60 · steps / seconds`; the solver picks the counts
by dynamic programming over the counts whose tempo is inside the range,
each step costing the squared change of tempo in log (so a change counts
as a share) plus, when asked, the distance from a preferred tempo. The
stretch before the first hit may take any tempo in the range; hits closer
to the one before than a step at the fastest tempo are left out and
named. Applied as one undo step ("Tempo from Hit Points"): the new tempo
map (a step at each hit), the hit markers moved onto their beats (each
sounding where it did), the other markers and — by default — the audio
clips kept where they sound, MIDI following the beats; the picture is
placed in samples and stays. The form previews the result (hits, tempo
range, the largest change) as it is set. Menus: a marker's, the markers
lane's and the tempo lane's; dev action
`tempo-from-hits:<markers|cuts|range>[@beats|eighths|bars][/lo-hi][/near=bpm][/move]`
(`dialog` opens the form).

### MIDI time code out and varispeed

MTC output: outputs with MTC on (`MidiShared::mtc_ports`, rate and the
project start's timecode packed in `mtc_format`) get quarter frames from
the audio thread while playing (`engine::midi::MtcGen`: four a frame,
sequences of eight starting on even frames, timed like MIDI clock), and a
full frame from the session when playback starts or the playhead jumps
(`Session::tick_mtc_out`, sent at once with `MidiOutputs::send_now`).
`faderframe_midi::timecode` holds `MtcRate`/`Timecode` for both sides.

Varispeed (`engine::varispeed`): with it on (`EngineController::
set_varispeed`, built on the control thread for the stream's channels and
buffer size) each device callback renders as many engine frames as its
frames need at the speed (`EngineShared::speed`, ±2 %), the transport
advancing by those, the outputs resampled to the device and the inputs
from it (FIFOs read with an 8-tap Lanczos kernel; four frames of latency
each way); `process_device` wraps `render` in it. `position_at`
extrapolates at the speed. `session::sync` follows a master with it by
default: a PI loop on the smoothed distance per tick sets the speed within
±1 %, a distance beyond four times the tolerance for three ticks still
chases. Tests: `engine/tests/varispeed.rs`, `varispeed_does_not_allocate`,
`follows_a_drifting_clock_by_varispeed_without_jumps`,
`session/tests/mtc_out.rs`.

### Clip launcher

Scenes (`faderframe_project::launcher::Scene`, rows) and slots
(`SlotKey { track, scene }` → `ClipId`) live in `Project::launcher`;
launcher clips are ordinary `Project::clips` entries (start 0) listed in
no `Track::clips`, so the arrangement, its edits (`arrange`, carving,
`repair`) and `clips_of` never see them; `RemoveTrack` takes a track's
launcher clips and restores them on undo. Commands: `SetLauncherSlot`
(Timeline), `SetScenes`, `SetLaunchQuantize`.

The snapshot gives every slot's clip a `LaunchLane` of its own from
sample 0 (looping over `length`), its streamed pages pinned in the
`StreamPlan` (a launch can come any moment). `engine::launch` keeps the
launch state on the audio thread (in `EngineContext`, preallocated for
`MAX_TRACKS`): `Message::Launch` commands (launch a slot, stop a track or
all, back to the arrangement) wait for the next quantised position
(`next_boundary`: beat, bars from the meter), take effect at once while
stopped, and `transport()` follows play/stop/jumps (stopping stops the
launched clips; the tracks stay silent until launched again or back to
the arrangement). Clip players ask `launch::pieces` what to play over a
block — the arrangement, a launched clip or silence, split where a launch
lands — so switches are sample-accurate in any block size; the MIDI
player ends sounding notes where the played source changes or the clip
wraps. The state is published through `LaunchStatus` atomics (slot
numbers below 2^63: the top bit is the status flag). Tracks with launcher
clips play live (not rendered ahead; `sync` escalates to a graph rebuild
when that set changes) and get one more stretcher voice per algorithm
their launcher clips use. Tests: `engine/tests/launcher.rs`,
`the_clip_launcher_does_not_allocate`.

`session::launcher` (`Action::Launcher(LauncherOp)`) launches slots and
scenes (a scene stops the tracks without a clip in it; launching while
stopped plays), edits scenes and slots (send arrangement clips to their
tracks' first free slots, new MIDI clips, move/copy), and with Record to
Arrangement on follows the published state on every tick: each run of a
clip becomes copies over its span (the last cut where it ended, the
track's material there carved away) in one "Record Launches" step when
the transport stops. The grid is `faderframe-view-launcher`
(`ViewKind::Launcher`); the arranger dims tracks that play the launcher.

Recording into a slot (`LauncherOp::Record`, an armed track's empty
slot) runs the arrangement recorder for that track only
(`start_recording_only`) from the next launch position, with a
`SlotRecording` on the `ActiveRecording`; asked again (or the track
launched or stopped) it ends on the next one, is stopped once the
transport is past that plus the capture latency, and `finish_slot_recording`
turns the take into the slot's clip (audio from the take file, MIDI via
`midi_take_clip` over the span) that plays on in time
(`LaunchCommand::Resume` with the recording's start). Follow actions
(`Launcher::follow`, `FollowKind`, `Command::SetFollowAction`) become
`snapshot::Follow`s on the launch lanes (targets among the track's clips
in scene order); `LaunchState::follow` queues them on the audio thread
as soon as a clip plays (`followed` ones are not shown as waiting, a new
snapshot queues them again). Clips dragged from the arranger reach the
grid through the canvas host's payloads (`CanvasView::drag_payload`/
`hover_payload`/`drop_payload`/`cancel_drag`; the move is cancelled with
`Action::CancelGesture` = `History::cancel`) and land by
`LauncherOp::PlaceClips` — clips of several tracks into their own
columns; launcher clips dragged to the arranger go back as copies
(`LauncherOp::ToArrangement`, as they play). A payload drag that leaves
its window goes on as a native GTK drag (`gdk::Drag::begin`, the source
view's drag undone) that every canvas's string drop target hands to its
view, so clips drag between windows.

Slots have launch settings (`ClipLaunch`, `Command::SetClipLaunch`,
moved and copied with their clips): the launch mode — trigger, gate
(plays while held), toggle (the next press stops it), repeat (starts
again every launch quantum while held) — presses are `LauncherOp::Launch`
and releases `LauncherOp::Release` (mouse-up, key-off, a falling CC
edge); their own quantisation; legato (`LaunchCommand::Launch { legato }`
keeps the playing clip's loop start, so it takes over its position) and
Follow Tempo (`ClipLaunch::tempo`: audio in time at that BPM is played
as `launcher::as_played` makes it — its warp map, or a uniform one,
scaled to its musical length under the tempo map — by the snapshot,
the loop length and Record to Arrangement; set when audio is sent,
placed or recorded into a slot). The engine's `TrackLaunch` keeps
`legato`, `repeat` (slot and samples: requeued like a follow action) and
`stop_after` (released before the launch landed: one quantum).
`LaunchCommand::Release` stops at the next launch position. Follow
actions have a second action (`FollowAction::{other, chance}`: drawn per
cycle on the audio thread) and `FollowKind::Jump(scene)`
(`Launcher::follow_targets`). Slot recording has a fixed length and a
count-in from stop (`Launcher::{record_bars, count_in}`,
`Command::SetLaunchRecording`; the recording ignores the punch range),
and recording on a MIDI clip that plays overdubs (`SlotRecording::
overdub`: the notes join its loop where they fell, one "Overdub" step).
Record to Arrangement writes as it plays — each loop once played, the
rest where a clip stopped or another took over (the switch time from
the launch the previous tick saw waiting) — in one undo step per
transport run (`History::apply_amending`), and writes the mixer's and
devices' moves as latched automation (lanes made for controls without
one). Slots, scenes and stops take MIDI learn
(`MappingTarget::{LauncherSlot, LauncherScene, LauncherStop}`).

### Scratch pads

Scratch pads (issue #13, after Studio One's) are other arrangements of the
project's own tracks to try an idea in without touching the song.

* Model (`faderframe_project::scratch`): `Project::scratch_pads` =
  `ScratchPads { pads, shown, song, open }`; a `ScratchPad { id, name,
  stash }`; a `Stash` holds an arrangement by value — clips (in each
  track's order, `StashedTrack`), each track's automation, its own
  timeline (tempo and metre), markers, sections, key changes, chords,
  lyrics, loop and Play Start Marker. Everything else is shared: the
  tracks themselves (mixer, devices, routing, sends, modulators, freezes),
  sources, video, album, setlist, the launcher; nothing is duplicated but
  the clips a pad holds (their media are the song's).
* The project's arrangement fields hold the arrangement **shown**;
  `Project::show_arrangement(pad)` swaps (puts the shown one aside, takes
  the other out of its stash; an exact inverse). So `Project::clips` never
  holds a clip put aside, and every editor, the engine snapshot, the
  clipboard, recording and every `Command` work on what is shown without
  knowing about pads. A new pad starts empty in the song's tempo and metre
  at its start, without automation lanes; material copied in brings its
  lanes (`TimeSlice::adopt_lanes`: the same lane id in every arrangement,
  or the lane of the same target there).
* Commands: `ShowArrangement { pad }` (Timeline impact; an undo step of
  its own, so every edit is undone in the arrangement it was made in —
  switching waits for gestures and recordings to end) and `SetScratchPad`
  (add, change, remove; the shown pad's value has no stash, the shown pad
  is never removed; Impact::None). `RemoveTrack` takes the track's parts
  out of every stash and `RestoreTrack` puts them back
  (`RemovedTrack::stashed`), so stashes never name a missing track.
  `RemoveSource` refuses sources stashed clips play,
  `Project::used_sources` (unused media, cleanup) counts them,
  `repair` keeps stashes consistent with the tracks and reserves their ids,
  `compare::differences` compares the songs and names pads added, renamed,
  changed or removed; templates drop pads.
* Session (`session::scratch`, `Action::ScratchPad(ScratchOp)`): `New
  { name, from }` (empty or holding a copy, shown), `CopyTo { pad, what }`
  (after what the pad holds, from the next bar line; the shown arrangement
  stays), `Show`, `Rename` / `PromptRename` (`UiRequest::RenameScratchPad`),
  `Duplicate` (new ids for its clips, markers and sections), `Delete` (the
  song is shown if the pad was), `ToSong { pad, what, place }`. Material
  (`PadMaterial`): a section with everything in it (tempo, metre, markers,
  the section, key, chords, lyrics, automation — `arrange::copy_span`),
  the edit selection (its range on the selected tracks, else the selected
  clips — whole clips without automation, as copies of clips are), given
  clips, or all an arrangement holds (`Project::arrangement_span`, out to
  bar lines). Clips keep their place in the bar; tracks keep their clips
  (pads share the tracks, so nothing needs mapping). Each op is computed on
  a copy of the project and applied as one batch of `ShowArrangement` /
  `SetArrangement` / `SetScratchPad`, so it is one undo step.
* Back into the song (`SongPlacement`, the song shown after, at the
  material, which is selected): `Insert { at }` (time opens:
  `arrange::insert_span`), `Overwrite { at }` (`arrange::overlay_span`:
  laid over the song on its tracks, what lies under it goes, time does not
  move) or `Replace { section, keep_length }` (`arrange::replace_span`).
  A replacement by material of another length is ambiguous, so the caller
  says which: the section takes the material's length (what follows moves
  by the difference) or keeps its own (the material is cut or padded to
  it); the menus offer both by name with the lengths in bars
  (`Session::pad_material_length`). The replaced section is put back over
  the new span when the material brings none.
* Playback plays what is shown (the snapshot's clips, automation, tempo
  and loop are the shown arrangement's). A freeze is the song's: in a pad
  a frozen track is silent (its devices are unloaded); freezing renders
  the song, bouncing what is shown. Renders and exports (`render_copy`:
  master, stems, album, video, ADM, IAMF) always take the song. Each
  arrangement keeps its playhead (`Session::pad_positions`; a pad starts
  at bar 1; `song_position()` is the song's while a pad is shown) and the
  arranger its scroll and zoom per arrangement (`ArrangerView::places`).
* UI: the arranger's corner has the switcher (the arrangement's name, an
  accent while a pad is shown, with the ruler tinted and an accent line
  under it; click or right-click: the menu — Song and the pads to show,
  New Scratch Pad (from the selection, from a section), the shown pad's
  Rename / Duplicate / Delete / Bring to Song, from the song each pad's
  entries; double-click a pad's name to rename it there; a tab list for
  screen readers). Section and clip menus have Copy to Scratch Pad ▸ (new
  or another pad) and, in a pad, Bring to Song ▸ (Insert / Overwrite at
  the song's playhead bar, Replace Section ▸). Edit → Scratch Pads and the
  command palette run `app.scratch-pad::<op>`, also the dev action
  `scratch-pad:<new|new-from-selection|copy-section=<n>[@<pad>]|copy-selection@<pad>|show=<pad>|song|next|to-song[@<q>]|overwrite[@<q>]|replace=<n>[/keep]|rename[=<pad>=<name>]|duplicate[=<pad>]|delete[=<pad>]>`
  (sections 0-based, pads 1-based).
* Tests: `faderframe-project/tests/scratch_pads.rs` (swap exactness and
  undo, commands, track removal, file form and older files, used media,
  repair, span operations), `faderframe-session/tests/scratch_pads.rs`
  (copy a section into a pad, edit there, the engine plays the pad's clips
  only, insert/overwrite/replace both ways, the selection in and out,
  clipboard across arrangements, every step undone and redone, save and
  reopen, unused media kept, renders of the song, freezes), the arranger's
  `scratch_tests.rs` (switcher, menus, tint, places, accessibility).

### Control surfaces

`faderframe-control` holds the protocols without I/O: the session builds
a `SurfaceState` (a bank of strips — fader travel by the console law,
level text, pan, mute/solo/arm/select, name, peak — the master fader,
transport flags, bars/beats/sixteenths/ticks, the bank's first track)
every tick and a `Protocol` sends only what changed; what arrives becomes
`SurfaceInput`s. `mackie` (notes for buttons/LEDs, pitch-bend faders, the
2×56 LCD by SysEx, pot rings, the ten-digit display, channel-pressure
meters refreshed every 100 ms), `hui` (zone/port switches and LEDs,
14-bit faders over two CCs, scribble strips, the 2×40 display, timecode
digits, meters, a ping every second) and `osc` (an OSC 1.0 codec and
FaderFrame's address space, documented in the module; toggles compare
with the state shown so TouchOSC's toggle and push buttons both work).
Faders a user holds are never moved. The pots' page (`Page::Pan` or a
send), Flip (faders and pots swapped) and the selected track's automation
mode are part of the state (Mackie: Pan/Send/Flip/Read/Write/Touch/Latch
buttons and LEDs, the assignment display; HUI: the assignment and
auto-mode zones; OSC: `/page`, `/flip`, `/automation`).

`session::control` opens the surfaces from `SurfaceSettings` (saved as
`Preferences::control_surfaces`): MIDI ones through
`faderframe_midi_io::SurfacePorts` — their own input connection and an
immediate output, the ports taken from the hub and the track outputs
(`MidiState::surface_inputs/outputs`, kept out of the preferences'
disabled lists) so a surface never plays tracks — and OSC on a
non-blocking `UdpSocket`, answering the last sender on the reply port.
Surfaces sit side by side from the shared bank start over
`Session::surface_tracks` (mixer strips in editor order). Faders and pots
are gestures (touch to release, or 400 ms of rest), buttons toggle, select
or run the transport, banks move by a surface's width; OSC also launches
the clip launcher. Automation buttons put the selected track's lanes in
that mode (a volume lane is made when it has none); Save saves a project
that has a file. Tests: `faderframe-control`'s, `session/tests/control.rs`
(a virtual Mackie on the demo, OSC over UDP on localhost).

The rest of the surfaces' buttons (`Button`, `session::control_extra`):
the channel-strip pages (`Page::{Track, Plugin, Eq, Instrument}`: the
selected track's own parameters, a device's — the plug-in page again or
the channel buttons: the next device —, its EQ's or its instrument's on
the pots and the display; the bank buttons page through them; pot press
resets one, Flip puts them on the faders), F1–F8 (workspaces; Shift: the
markers in time), the track types (filters of `surface_tracks`: MIDI and
instrument tracks, armed ones, audio, aux, buses, VCAs, the selected;
Global View: all), Name/Value (the lower line: levels or the pots'
values), SMPTE/Beats (`SurfaceState::timecode` at the MIDI time code
rate), Shift/Option/Control/Alt (Shift + Undo redoes, + Rewind/Forward
goes to the start/end, + Select adds to the selection; Option + Mute/
Solo/Arm sets every track), Read again = Off (Trim too: there is no trim
mode), Group, Cancel (clears the selection), Enter, Nudge/Zoom/Scrub/
Shuttle (the arrows nudge or zoom, the jog wheel scrubs a 64th or
shuttles a beat a tick, heard while stopped), the arrows (Up/Down select
the strip above/below), Drop (punch) and Replace (record mode), punch
in/out and pre-roll, the global Solo (clears them; lit with the rude
solo LED while one is on), arm all, the monitor section's mute (the
master), input monitoring, bypass of the shown device, windows (the
mixer, the editor, the launcher), the edit mode and tool, cut/copy/
paste/delete/separate/capture, footswitches, and HUI's numeric keypad
(a bar to locate to, `.n.` a marker, enter alone a marker; ± a bar). The
mode lights come from `SurfaceState::modes`. HUI answers its pings, so
`Session::control_surface_online` (and the preferences) say when it is
off line. A surface may have a bank of its own (`SurfaceSettings::
own_bank`; grids always).

Grid controllers (`faderframe_control::grid::Grid`, `SurfaceKind::
{LaunchpadMiniMk3, LaunchpadX, LaunchpadProMk3, ApcMini, ApcMiniMk2,
Push2}`) play the launcher: `Protocol::launcher_columns` makes their
strips the launcher's tracks, `Protocol::scenes` gives them a
`LauncherView` (slot and scene states from their scene bank) to light —
the clips' colours by Launchpad RGB SysEx, by the nearest entry of the
shared 128-colour palette (APC mini mk2), by Push 2 palette entries set
to the clips' colours; green pulsing playing, flashing waiting, red
pulsing recording — and pads launch and release slots; scene, stop,
arrow and mode buttons and the APCs' faders as their module docs say;
`Protocol::goodbye` puts them back in their own mode. OSC shows the
launcher too (`/launcher/clip/N/S` and friends, see `osc`).

**Controllers known by name** (`faderframe_control::controllers::
Controller`, `SurfaceKind::Controller`): keyboards and surfaces with a DAW
mode of their own, each with what to do on the device
(`Controller::setup`, the kind's tooltip in Preferences) and only what
sources document (each module names its own: vendors' manuals and
programmer's guides, else open-source integrations that agree —
Bitwig's official extensions, Ableton's scripts, Ardour, DrivenByMoss;
GPL and decompiled code read for facts only). Two families:

* **Mackie profiles** (`mackie::MackieProfile`): hello/goodbye messages
  (Arturia's Mackie DAW mode and DAW map, the X-Touch Mini's MC mode by
  CC 127), notes on channel 1 only (a keyboard's pads share the DAW
  port), another display (`Display::Arturia`: the 2 × 16 LCD with the
  selected track, its level, the position) or none, no time display or
  meters, the X-Touch's scribble strips in the tracks' colours, note
  remaps both ways (iCON Platform M+: Read/Write swapped, Mixer =
  Flip; X-Touch One: SCRUB sends F8), one strip following the selection.
  KeyLab Essential (mkI), KeyLab mkII, BeatStep Pro (its MCU knob
  layer), X-Touch, X-Touch Extender, X-Touch One, X-Touch Mini, Platform
  M+.
* **Tables** (`mapped::Profile` run by `mapped::Mapped`): the controls as
  (message, `Does`) — buttons, strip buttons (or the strip in the value:
  NI), faders (7-bit CC or pitch bend), knobs absolute or relative
  (`Encoding::{SignedBit, TwosComplement, Offset64}`), wheels, relative
  volume and pan, the selected track's pan, fader touch, launcher pads,
  fixed inputs — plus hello/goodbye, a keep-alive (FaderPort's `A0 00
  00` each second), a handshake to be answered before any state goes
  out (Komplete Kontrol), presses without releases, and a feedback
  function writing lights and displays through `Feedback` (sent when
  changed; a held motor fader is not moved and goes where it should when
  let go). KeyLab Essential mk3 and MiniLab 3 (Arturia's DAW program:
  connection handshake, RGB lights, screen, knobs re-seeded to the values
  shown), Launchkey MK3 / 88 MK3 / Mini MK3 / MK4 / Mini MK4 (DAW mode,
  session pads on the launcher in the shared palette — static, flashing
  on channel 2, pulsing on 3 —, the LCD or OLED), SL MkIII (InControl:
  relative knobs, the screens' knob layout with the tracks' names, the
  LEDs, pads), nanoKONTROL2 (Korg's native mode, host-lit buttons),
  Komplete Kontrol S MK3 (NI's Kontrol protocol on its DAW port: names,
  states, knob rings by SysEx/CC), Oxygen Pro and MKV (M-Audio's DAW
  mode), MPK mini Plus (Akai's transport CCs), FaderPort 8/16 (native
  mode: motor faders, RGB select buttons, scribble strips, value bars,
  meters).
* **Stateful protocols** where a table cannot hold the surface: the
  single-fader FaderPort (2018) (`controllers::presonus::FaderPortOne`,
  native "Studio One" mode per the manual's §2.4 and §8.2) keeps its
  session navigator's mode — Link, Master, Pan, Channel, Scroll (Shift:
  zoom), Section, Marker — lit on the mode buttons, and gives the encoder,
  its push and Prev/Next that mode's job: select the track above/below,
  the master's level (push: 0 dB), the selected track's pan (push:
  centre), the playhead (push: the whole project), zoom, nudge the
  selected clips and go through the sections, go through the markers
  (push: drop one). Shift + Prev/Next undo and redo, Shift + Pan puts the
  pan on the motor fader, Shift + Master/Click/Section/Marker are F1–F4
  (workspaces 1–4: Shift stays inside the protocol, the session's Shift
  would make them marker jumps),
  Shift + Arm/Solo/Mute arm all, unsolo and unmute all, Bypass bypasses
  the track's devices (again: all back), Shift + Touch latches, Shift +
  Read turns automation off. What a surface asks beyond a button is a
  `SurfaceInput::Command(SurfaceCommand)` (the session's
  `surface_command`; `SurfaceState::unity` is the fader travel of 0 dB).

Absolute knobs arrive as `SurfaceInput::Knob { strip, value }` (the pan,
a send, a channel-strip parameter or — flipped — the volume, set where
the knob is), relative ones as `Pot` ticks. **Shared ports**: keyboards
whose DAW controls arrive on the keys' port (KeyLab Essential mk3,
MiniLab 3, MPK mini Plus; `Protocol::claims`) keep it in the MIDI hub —
`MidiHub::set_claims` filters what the profile takes (its controls, the
vendor's SysEx) out of what reaches the instruments, so the keys, pads
and wheels still play. **Finding them** (`controllers::detect`,
`Session::detect_control_surfaces`, Preferences → MIDI → Find
Controllers, dev action `add-surface:find`): by the device's (ALSA
client's) name and the order of its ports — port names are cut to 31
characters and change between kernels, and ports whose names come out
alike are told apart by their order (`faderframe_midi_io::
port_identities`: "… #2"). Not covered (no documented protocol, or
nothing to map): the Komplete Kontrol A/M32/S MK2 (NI's helper only),
Nektar's DAW modes, Alesis, Roland A-series, MPC Studio; Oxygen and
Keystation Mackie modes, the nanoKONTROL2's DAW modes and other plain
Mackie/HUI devices work as "Mackie Control"/"HUI". Tests: each module's,
`session/tests/control.rs` (`a_keylab_essential_mk3_runs_the_transport_
and_the_strips`, `an_x_touch_one_follows_the_selection`).

### Phone and web remote

A phone or tablet runs FaderFrame from its browser on the local network:
the transport (play/stop/record, loop, click, start/end, a bar back or
on, undo/redo, markers added and located, the next and previous marker
or section), the mixer (faders, pans, mute/solo/arm, sends with their
switches, peak meters), cue mixes, the setlist and show mode, the lyrics
as a teleprompter, the clip launcher, mix scenes and macros. Nothing to
install: FaderFrame serves the app itself.

**The crate** `faderframe-remote` holds everything that is not the
session: a small server of its own (`server`: a thread accepting, a
thread per connection reading HTTP/1.1 (`http`) and then the WebSocket
(`ws`, RFC 6455: frames, fragments, control frames, masks, close codes),
a writer thread per WebSocket fed through a bounded queue), pairing
(`pairing`), the protocol (`protocol`), state diffs (`diff`), QR codes
(`qr`, ISO 18004 byte mode, versions 1–40, every level and mask; checked
against ZBar by the ignored `codes_read_back`), SHA-1/SHA-256 (`hash`),
secrets from the system's random source (`secret`, `getrandom`) and the
web app (`web`: plain HTML, CSS and JavaScript under `web/`, built in
with `include_str!`, no build step). No TLS: browsers trust no
self-signed certificate on a LAN address without the user installing
it, and a certificate in every phone is more than a remote is worth. The
stance instead is the local network's, enforced by the server:

* **Off by default.** `WebRemoteSettings::on` is false until the user
  turns it on (Preferences → Remote, Transport → Web Remote On).
* **Local peers only.** A connection from outside loopback, the private
  IPv4 ranges, link-local and IPv6 unique-local addresses is dropped
  before a byte is read (`net::is_local_peer`), unless the user asks for
  devices outside (`any_network`, for a VPN of their own). "This
  computer only" binds 127.0.0.1.
* **No public names.** A request whose `Host` is not an address,
  `localhost`, a single-label name or a local domain (`.local`, `.lan`,
  `.home.arpa`, `.internal`) is refused (`net::host_allowed`): a page on
  the internet cannot rebind its name to this computer and drive the
  remote. A WebSocket handshake carrying an `Origin` must come from the
  page's own origin (programs send none, and still have to pair).
* **Pairing.** A WebSocket says nothing but `pair` and `hello` until it
  has paired. A code (six digits, uniform) is drawn when the server
  starts and after every pairing — one code, one device — and names the
  role the device gets. A device pairs by typing it or by scanning the
  QR code of `http://<address>:<port>/#pair=<code>` (the app takes the
  code from the address and removes it); it gets a token (32 random
  bytes) to keep, FaderFrame keeps only its SHA-256
  (`PairedDevice::token_hash`, compared in constant time). A wrong code
  costs its connection a second, five end the connection, and ten in all
  withdraw the code until the user asks for a new one (Preferences says
  someone may be guessing). An unpaired connection is dropped after two
  minutes, a silent one after 45 s (pings every 15 s), at most 32
  connections at once, messages up to 16 KiB, request headers up to
  8 KiB.
* **Roles** (`Role`): Full (the engineer), Performer (one cue mix, by its
  name, case aside — templates and other projects find theirs) and
  Viewer (the lyrics, setlist and position). `State::for_role` leaves
  out what a role does not see — a performer gets only their cue mix
  (the tracks in it by name and colour, no main-mix values, no master),
  a viewer no mixer — and `RemoteInput::allowed` refuses in the session
  what a role may not do, whatever the app shows. Roles change while a
  device is connected (it is sent `welcome` again and the whole state);
  a forgotten device is told `bye` and disconnected.
* **The computer's firewall** (`firewall`). A firewall that drops what
  it does not allow (ufw — on by default on Omarchy —, firewalld) leaves
  a phone's page loading for ever, while the remote answers on this
  computer: loopback passes every firewall, so no test from here can
  tell. `firewall::check(port, lan)` reads the firewall's own files
  instead (no root, nothing run; Linux — Windows and macOS ask the user
  when a program first listens): ufw by `ufw.conf` `ENABLED`, its
  systemd unit enabled and not masked, `DEFAULT_INPUT_POLICY` and
  `user.rules` in order (first deciding rule; protocol, ports and port
  lists/ranges, `-s`/`-d`/`-i` against the LAN route from
  `/proc/net/route`; what is not understood counts as allowing when it
  allows and as missing when it denies — a note only when sure); firewalld
  when it runs (`/proc/*/comm`) by the zone an administrator bound the
  interface to, else `DefaultZone`, open when its target is ACCEPT or a
  port, rich rule's port or service's port holds the remote's (runtime
  rules and NetworkManager's zones are not in files: "most likely").
  `FirewallNote { firewall, port, certain, zone, lan, command }`: the
  command lets in the local network only, TCP, the remote's port (ufw:
  `allow in on <interface> from <network> to any port <port> proto tcp`;
  firewalld: a permanent rich rule for the network). The session reads it
  when the remote starts answering the network (one warning notice per
  turning on) and every `FIREWALL_EVERY` 3 s while it runs (an Info
  notice when it lets devices in), `WebRemoteStatus::firewall`;
  Preferences → Remote shows it under Status with the command and Copy.
  `check_in(root, …)` / `Session::set_firewall_root` read a pretend root
  (tests).

The app's files are served with a content security policy of their own
files only (`web::CSP`), `nosniff`, no caching, no referrer; text from
the project is always set as text (`textContent`; `web::tests` holds the
script to it).

**The protocol.** JSON text messages tagged by `"t"`. The app sends
`pair`/`hello`, `view` (its screen: meters go only to the mixer and cue
screens) and `RemoteInput`s; the server answers `paired`, `welcome`
(name, role), `refused` (why), `state` (whole), `patch` (`diff::diff`:
objects key by key, arrays element by element when their length holds,
else whole; `{"p": path, "v": value}` or `{"p": path, "d": true}`;
`app.js`'s `applyPatch` mirrors `diff::apply`), `meters` (peaks by strip
and cue mix id, to half a decibel, floored at −90), `notice` and `bye`.
Faders, sends and cue levels are fader travel (the console law; the
state carries `unity`), pans −1…1; a drag is `touch` on, the moves
(throttled to one every 30 ms), `touch` off; double tap resets. Tracks
are an object by id plus `order`, so a fader moving is one small patch.
Positions are seconds through the tempo map (`transport.seconds`, the
lyrics, markers, sections); the app moves the playhead on between
updates while playing and lights the line being sung 150 ms early.

**The session** (`session::web_remote`): `WebRemoteSettings` (on, port —
`DEFAULT_PORT` 8642 —, `RemoteListen::{Network, ThisComputer}`,
`any_network`, the paired devices) ↔ `Preferences::web_remote`;
`Session::{set_web_remote, web_remote_settings, web_remote_status,
web_remote_address, web_remote_state}`, `Action::WebRemote(RemoteOp::
{Settings, NewCode(role), Forget, SetRole, Rename})` (not undoable; in
`keeps_audition`/`keeps_scene_work`). `tick_web_remote` (last in the
tick) takes the server's `Event`s on the session's thread — the path
control surfaces' inputs take: `Paired` adds the device to the settings
(the shell saves them), `Joined`/`Left` keep the clients, `Input`s become
the same actions a surface's do (`Action::Edit` of `SetTrackVolume`,
`SetTrackPan` — dual-panned images through `set_dual_pan_from_surface`
—, `SetTrackMute`/`Solo`/`RecordArm`, `SetSendLevel`/`Enabled`,
`SetCueLevel`/`Mute`, `SetCueSendLevel`/`Pan`/`On`; transport actions;
`ShowOp`s (a song tapped outside show mode enters it); `LauncherOp`s;
`SceneOp`s; `ScriptOp::RunMacro`; `locate_step` for the marker and
section steps), so groups, selected tracks and automation writing follow
them as from the mixer. Fader, send, pan and cue moves open a gesture
"Web Remote" (`BeginGesture`) held while any client holds a control and
ended on the last release, at 400 ms of rest without a touch, or when
the client goes — one undo step, as a surface's. Every 100 ms (at once
when a client joins) `remote_state` builds the `State` (shown values:
`shown_volume_db`, `shown_pan`, `shown_mute`, `shown_send_db`,
`shown_cue_send`, `shown_cue_level_db`; the launcher through
`surface_launcher`, as the grids light it) and each client gets the diff
against what it was last sent as its role sees it; a client whose queue
is full gets the whole state again when it has room. Meters every 80 ms
to screens that show them (a performer: their cue mix's).

**The app** (`web/app.js`): the pairing screen (code, device name — the
user agent's kind by default), then the role's screens as tabs —
Transport (counter, buttons, markers, sections, mix scenes, macros),
Mixer (strips with meters, a Sends switch), Cue Mix (a performer's own;
the engineer picks one), Launcher (scenes by tracks, slots in their
clips' colours, press and release for gate clips, stop buttons, Stop
All, Back to Arrangement), Setlist (show mode's buttons, the songs — a
tap stands on one, ▶ plays it —, the countdown, the song's notes) and
Lyrics (large lines centred as they are sung, A−/A+ remembered). The
screen stays awake where the browser allows it. It reconnects by itself
with growing waits and keeps its token in local storage.

*Touch.* A drag on a slider starts only once the finger has moved 4 px
along it, and only then sends `touch` on: a swipe across a control
belongs to what it sits in. Vertical faders are `touch-action: pan-x`
(a sideways swipe anywhere on a strip scrolls the mixer), horizontal
sliders `pan-y` (an up/down swipe on a cue send scrolls the list). Under
the strips an overview — every strip a cell in its colour with its
level, the strips on screen framed — scrolls the mixer when dragged or
tapped; the master strip is pinned beside the scroller; the strips
snap. Faders are relative drags (double tap: unity), with a cap in the
track's colour and a two-channel meter (−60…+6 dB, lit colours fixed
under a moving cover, peak held 1.5 s).

*Look.* Icons are SVGs in `index.html` (`#icons`, 24 × 24, stroked in
`currentColor`; classes `solid`/`fill`/`thick`), cloned by `icon(name)`
— no namespace strings or markup in the script, and nothing for the CSP
to refuse. Tabs carry icons over labels; on phones held sideways
(landscape, ≤ 540 px high) they become a rail on the left and the
strips' M/S/R share a row. Colours are tokens on `:root`, a light set
under `prefers-color-scheme: light`. Check the app in a phone-sized
headless browser against the demo with
`FADERFRAME_REMOTE_PLAY=1` (the demo plays, looping, on the dummy
device) on the ignored `serve_the_demo_for_a_browser`; `node --check`
the script.

Tests: the crate's units (`hash`, `ws` incl. RFC 6455's examples,
`http`, `net`, `pairing`, `diff`, `protocol`'s role filter, `qr` against
the standard's worked examples, `web`), `faderframe-remote/tests/server.rs`
(on localhost through `client::Client`: files and refusals — bad host,
foreign origin, POST, garbage —, pairing once and coming back with the
token, inputs only once paired, forgetting, oversized and unmasked
frames, stopping frees the port), `session/tests/web_remote.rs` (the
demo: state then patches, a fader drag one undo step, pans at rest, loop,
markers and undo from the phone, meters to the mixer screen, an unpaired
device changes nothing, a performer moves only their own cue mix and is
made the engineer while connected, a viewer reads the setlist and lyrics
while the engineer's phone runs the show, turning it off closes the
port), ui `web_remote::tests`.

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
step. Saving asks for a name (`PromptSaveTrackPreset`; several tracks
keep their names). `SaveTrackPreset { name, replace }` overwrites the
preset of that name, ignoring case (`existing_track_preset`), only with
`replace`; otherwise the new file is numbered (keep both).

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
* **Skins.** `Theme::all()` lists nine (`themes.rs` derives each from a
  compact `Spec` of base colours): Studio, Vintage Console, Daylight
  (light), Midnight, Frost, Fjord, Neon, High Contrast and Lumen. Besides
  colours a theme has a `ConsoleLook`: skirted knobs (with or without a
  value ring), flat or glossy controls, brushed-metal panel grain, screws,
  walnut cheeks framing the mixer, and the meter kind — LED ladder,
  continuous bar, plasma bar graph (a glowing column with cell lines), an
  edgewise VU meter (0 VU = −18 dBFS, voltage-proportional scale) or
  `MeterKind::Gradient` (fine rounded bars in one cool-to-hot gradient, a
  hairline hold, a bloom at the level). The shell switches skins live
  (`AppState::set_theme`: every canvas view gets `CanvasView::set_theme`,
  GTK's stylesheet is regenerated from the theme as `@define-color`s, and
  the Adwaita light/dark variant follows the skin, overriding a dark
  desktop theme for light skins); the choice is a preference. Overlays
  drawn over editor backgrounds derive from the theme's text and selection
  colours so they work on light skins too.
* **Themes of the user's own** (View → Theme → Edit Themes…, Preferences →
  General): `faderframe_ui_canvas::custom` names every colour a user may set
  (`COLORS`, by section; each key sets all the theme's fields showing that
  colour) and registers `CustomTheme`s (a built-in base skin + changed
  colours) so `Theme::all`/`by_id` find them (ids `custom:<file>`;
  `Theme::builtin` lists the skins). The shell keeps them as JSON files in
  `<settings>/themes` (`user_themes`: `{"name", "base", "colors": {key:
  "#rrggbb"}}`, loaded before the first window) and edits them in
  `theme_editor` (a colour button and a reset per key, seen at once).
* **Lumen** (`themes::lumen`) is drawn with light instead of hardware, on
  look options that default to the older skins' drawing (so those draw
  exactly as before — `only_lumen_takes_the_newer_looks`; their painted
  primitives were compared op for op before and after):
  `ConsoleLook::{knob_kind: KnobKind::Halo` (a fine ring, the value as a
  glowing arc, a flat disc with an indicator dot), `fader_kind:
  FaderKind::Pill` (a slim track lit up to the level in the cap colour, a
  floating pill thumb with a glowing index), `button_kind:
  ButtonKind::Pill` (ghost pills lit from within, `controls::pill_label`),
  `panel_kind: PanelKind::Plane` (flat rounded planes stepped by
  luminance with a hairline edge, `controls::plane`; hairline section
  rules, flat wells, scribble strips as chips with a glowing line of the
  track colour; no screws), `glow}` (the strength of every glow the newer
  kinds draw — arcs, pills, blooms, the strips' colour lines, the
  arranger's playhead halo and gradient line, the transport's light
  pool); `ArrangerTheme::clip_style: ClipStyle::Card` (translucent cards
  tinted with the clip colour, a line of it along the rounded top, names
  in `Theme::card_ink`, lighter-at-the-peaks waveforms — the waveform
  tiles' key names the style and the text colour — and sections and the
  key lane to match); `Theme::chrome: Chrome::Planes` (the transport as a
  glass readout, `style::PLANES_CSS`: flat header and tabs on hairlines,
  pill header buttons, rounded floating menus and popovers, the accent
  for check marks, switches, selections and focus outlines — GTK's
  Default theme paints with `background-image`, so the rules set images);
  `UiPalette::accent_2` (the accent's gradient end: cyan to violet on the
  master's colour line and the playhead); `Typography::numerals` with
  `FontFamily::Figures` (the UI face with tabular figures, `tnum` in Pango
  and parley) for readouts, rulers and counters; `FontWeight::Light`.
  `lumen_reads_well` checks WCAG AA (`Color::contrast`) for its text,
  legends, wells, lit pills and card names in every hue.

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

The UI's tick (`AppState::tick`) runs with the main window's frames — a
tick callback on its frame clock, so in each frame's update phase, right
before layout and paint (a frame shows that moment's playhead and meters,
one tick a frame at the display's rate, no beat against a free timer) —
and on a 16 ms timer only while no frames come (the window hidden: the
compositor sends none). It redraws every view when the session's revision
changed during the tick — finished recordings, imports and analyses
arrive there, not through a user action; what only changes the picture
(keys held on a MIDI input, launcher clips playing) bumps
`Session::display_revision` instead, which repaints the views without the
shell's full `after_change`. Warnings and errors also pop up as a toast at
the top of the main window. The start-up project opens after the
window's first frame (`open_after_first_frame`, "Opening …" meanwhile);
scripted start-up actions run after it. By default a start window shows
over a new project (`start.rs`, `StartupProject::Ask`; File → Start
Window…): recent projects, an empty project, the templates, a file, the
demo; scripted runs (`FADERFRAME_STARTUP_ACTIONS`) open the last project
instead. The arranger's zoom and scroll are saved with the project
(`WorkspaceSet::arranger_place`: noted on the session as the arranger
paints the song, `Session::note_arranger_place`, written by every save;
`Session::project_serial` tells the arranger another project opened).

**What a frame costs** (the demo playing, arranger and mixer, 2524×1371,
dev build; `FADERFRAME_PAINT_STATS=1` and the UI thread's CPU time from
`/proc`): 54 % of a core at first — 28 % of the profile cairo filling
paths again where the playhead and meters damaged them (GTK's renderer
re-rasterises a cached path cut by a new clip region), 15 % pango laying
labels out again. Three caches took it to 26 %, the views' paint from
1.7 + 1.7 to 0.3 + 0.5 ms a frame, the rest is GTK's renderer:

* **Text nodes** (`painter::TextCache`): the finished text node of a
  label (by text, style, width, colour) is appended again under a
  translation — pango and GTK's glyph measuring run once per label.
  Text nodes do not depend on the display's scale (GSK rasterises glyphs
  at the frame's), which matters on macOS: GDK there never notifies a
  window's scale change when it moves to another display. The canvases'
  font lists (`painter::families`, `faderframe-ui-gpu` `text::families`,
  the stylesheet's `MONOSPACE`) name the platform's UI and monospace fonts
  before the generic ones on macOS and Windows — Core Text takes "Sans"
  for Helvetica and "Monospace" for Courier, thin and soft at the canvases'
  sizes on a 1× display (#48), while GTK's widgets use the system font.
  GTK draws glyph masks on macOS without Core Graphics' font smoothing (an
  alpha-only bitmap), so 1× text stays lighter than native text whatever
  options pango is given.
* **Kept pictures** (`faderframe_ui_canvas::kept(p, key, rect, draw)`,
  `Painter::{show_kept, begin_keep, end_keep}`; the GSK painter's
  `KeptCache`): a layer that rarely changes under one that does is drawn
  once into a texture (rendered by the widget's renderer at device pixels,
  sized by its size alone so equal pictures anywhere are one) and shown
  again while its key holds; at most six are made a frame (a zoom gesture
  draws directly until it stops), unseen ones go after ten seconds or past
  192 MB. Painters without a store (the GPU painter, recordings) draw.
  Used for clip waveforms in 256-pixel tiles from the clip's start (keyed
  by source, its peaks, offset, gain, colour, height, zoom, tempo map;
  warped and live-recording waveforms draw directly) and for the ladder
  meters' unlit and lit ladders (the lit one clipped to the level — about
  a thousand rectangles a frame fewer in the demo's mixer).
* `PathCache` (below).

**The GPU painter.** GTK 4.22's renderer rasterises every fill and stroke
node with cairo on the CPU when its path changes, over the path's bounds:
analysers and curves redrawn each frame cost the main thread most of its
time (the mastering tools at 2524×747 ran at 26 fps, `cairo_stroke` 23 %
of the profile). Views whose geometry is dense and changes every frame
say so (`CanvasView::dense`: the tools, the EQ, the Program EQ and the
stock devices' faces) and, with Preferences → General → Drawing (or
`FADERFRAME_GPU_PAINTER=1`), are painted by `faderframe-ui-gpu`:
`ScenePainter` turns `Painter` calls into a vello scene (compute-shader
rasterisation on wgpu: Vulkan, Metal, DX12) — clips as clip layers, outset
shadows as blurred rounded rectangles clipped even-odd to outside the
shape, inset shadows as the colour less a blurred copy (`Compose::DestOut`),
images cropped to the frame drawn (vello's atlas does not take a tall
filmstrip), text laid out with parley in the same font families (cached,
ellipsised by binary search, hinted) — rendered into a texture and read
back (`GpuRenderer::render`, straight-alpha RGBA, buffers recycled) into a
`gdk::MemoryTexture`. Measured: the tools 26 → 55 fps, tools and EQ editor
together 17–19 → 39 fps. Rectangle-heavy views stay with GSK (the mixer:
its 3800 rectangles cost GSK 2 ms, while a 1.5×-scaled readback of 22 MB a
frame costs more). On Linux the readback goes too: the painter opens its
Vulkan device with dma-buf export (`dmabuf::open_device`, through wgpu's
hal: `VK_KHR_external_memory_fd`, `VK_EXT_external_memory_dma_buf`),
copies each frame on the GPU into a pooled buffer whose memory is a
dmabuf — linear rows padded to 256 bytes, DRM `AB24`, straight alpha —
and GTK takes it as a `GdkDmabufTexture` (`GpuRenderer::render_to`,
`Output::Dmabuf`); a buffer is reused only once GTK's release callback
gave it back (`DMABUF_POOL`, eight; with all of them shown a frame is read back).
It is used when the display imports linear `AB24` (`dmabuf_formats`),
never on Broadway (`takes_gpu_dmabufs`: its formats are what GTK can
`mmap`, and the painter's device-local buffers cannot be — the dense
views were empty there, "Failed to mmap dmabuf");
if GTK refuses a frame, frames are read back from then on
(`FADERFRAME_GPU_DMABUF=0` turns it off). Measured on a Radeon iGPU
(RADV): the mastering tools at 2411×711 device pixels 4.98 → 3.80 ms per
paint, 52 → 60 fps. Without fences the UI thread waited for each frame
(2.9 ms of a 4.0 ms paint at 3786×716): a view that animates now renders
**ahead** (`GpuRenderer::render_ahead(view, …)`): its frame stays on the
GPU and the one submitted the frame before — finished by then — goes to
GTK (`Output::Pending` the first time: the view's last texture is shown
again); a paint that is not animation (an event, the frame after the
animation stops, which the canvas asks for while `gpu::behind`) settles
the view and waits as before. The tools: 4.0 → 1.24 ms per paint. The pool
holds eight buffers. On Windows the frames go over as shared D3D12
textures (`d3d12::Exporter`: pooled committed textures on a shared heap
with simultaneous access, so they decay to the common state after the
copy, and an NT handle each; `Output::Shared`): the painter renders on the
D3D12 adapter with the LUID of GTK's GL device (`GpuRenderer::new_on`),
falling back to wgpu's choice without D3D12 when vello cannot be built
there (shader compilation errors are caught in error scopes, not
panics). With GTK's GL renderer — the default on Windows — the texture is
imported into a WGL context of GTK's display through
`EXT_memory_object_win32` (once per pooled texture, dedicated, `GL_RGBA8`;
`faderframe-ui/src/gpu_win32.rs`) and handed over as a `GdkGLTexture`:
GTK's own D3D12 import into GL imports the resource handle as the fence
too and falls back to a copy through the CPU. With GTK's Vulkan renderer
it is a `GdkD3D12Texture` (GTK ≥ 4.20 imports it itself). The first frame
is read back both ways and compared; any difference or error and frames
are read back from then on (`FADERFRAME_GPU_SHARE=0` turns it off). Not
measured on hardware here: Wine's vkd3d stubs `CreateSharedHandle` and
cannot compile vello's shaders, so the exporter's test runs on the Windows
CI runners (WARP). macOS keeps the readback. Without a GPU adapter, or
after any error, views keep the GSK painter — errors no error scope
catches included: wgpu's default for those (validation, out of memory)
and for a device lost (a GPU reset, a driver update, resume from sleep)
is a panic, which inside GTK's paint ends the program; the renderer keeps
the first instead (`on_uncaptured_error`, `set_device_lost_callback`,
`GpuRenderer::fault`) and its next frame fails with it
(`a_device_error_fails_the_next_frame_instead_of_panicking`). `FADERFRAME_PAINT_STATS=1` logs each canvas's paint time, frame
rate and primitives per frame. Tests: `faderframe-ui-gpu/tests/render.rs`
(skipped without an adapter).

Each canvas keeps a bounded `painter::PathCache` alongside its text cache.
Unchanged geometry must reuse the same `GskPath`: GSK caches rasterized
fills and strokes by path identity. Rebuilding native paths on every paint
accumulates duplicate GPU atlas entries and causes periodic main-loop
stalls when GTK collects them. Paths unused for two frames expire; entry
and geometry-size limits also bound the cache during animation or zooming.

HiDPI and fractional scaling are handled entirely by GTK; views never assume
96 DPI. On Wayland the app is a native Wayland client (the status bar shows
the GDK backend).

### Crash reports

A crash leaves something to send (`faderframe-ui/src/crash.rs`, installed
first in `main`; files in `<data>/Crash Reports`, the portable folder's
`Data` too):

* `crash-<start>-<pid>.log`: a **panic** on any thread (the hook, after
  Rust's own message: thread, message, place, backtrace, the end of the
  log — in GTK callbacks and `extern "C"` callbacks a panic ends the
  program, on workers it may not); a **fatal signal** (Linux, macOS:
  `SIGSEGV`, `SIGBUS`, `SIGILL`, `SIGFPE`, `SIGABRT`; written by an
  async-signal-safe handler from what `install` prepared: version, system,
  GTK and glibc versions, the signal, the faulting address, the thread's
  name and id, and — where the stack it runs on has 64 KiB, the main
  thread's 256 KiB signal stack included — the native backtrace as
  module(+offset) for `addr2line`. Every other thread Rust starts has a
  signal stack of a few KiB: a fault there that is not a stack overflow
  (its address not within `OVERFLOW_REACH` of the interrupted stack
  pointer; Linux x86_64/aarch64) is sent back once to run on the thread's
  own stack — the handler without `SA_ONSTACK`, the instruction faults
  again — so a worker's crash has its backtrace too (a video job's on
  Debian 13 said only "no backtrace"; `a_crash_on_another_thread_has_its_backtrace`,
  `a_stack_overflow_is_reported_and_ends_as_rusts`); then the previous action, Rust's stack
  overflow report or the default, so core dumps and the system's crash
  collection go on); an **unhandled exception** (Windows,
  `SetUnhandledExceptionFilter`, then Windows Error Reporting; the details
  come from `faderframe-crash`, written a part at a time so that a fault
  while walking a broken stack leaves the parts before it: the code, the
  thread's name and id, where it happened as module + offset with the
  nearest export of the module (GTK, GLib, plugins and the system export
  names; the program none), what an access violation touched ("reading
  0x10": a null pointer), the registers, the stack walked from the
  faulting instruction's context with `RtlLookupFunctionEntry` +
  `RtlVirtualUnwind` — a frame no unwind table covers (a leaf, or a call
  through a bad pointer) pops its return address — within the thread's
  stack limits, memory read only where `VirtualQuery` says it reads, runs
  of one address written once, at most the top 96 and bottom 32 frames of
  4096; then the loaded modules from a Toolhelp snapshot with their link
  time stamps and CodeView ids, and the end of the log). A Windows panic's
  stack is the same walk from `RtlCaptureContext` (Rust's backtrace says
  only `<unknown>` in the stripped program). Nothing is symbolised in the
  process: the program is linked with a build id (`.cargo/config.toml`:
  `-Wl,--build-id`, a CodeView record stripping keeps), `bundle.sh` keeps
  its debug info apart before stripping (`objcopy --only-keep-debug` →
  `FaderFrame-<version>-windows-x64-symbols.zip`, a release asset), and
  `scripts/symbolize_crash.py <report> <symbols>` adds the functions and
  lines (inlined ones too) under each of the program's frames with
  addr2line, refusing symbols whose build id differs from the report's.
  The tests (`faderframe-crash/tests/windows.rs`) crash a child in a
  `qsort` callback and read its report; they run under Wine too
  (`CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine cargo test -p
  faderframe-crash --target x86_64-pc-windows-gnu`). The graphics-driver
  check reads a Windows report's stack (never its module list, which
  names every driver loaded). Sandbox and check helpers write theirs
  (`crash-<start>-plugin-sandbox-<pid>.log`): a plugin crashing there
  leaves its report.
* `session-<start>-<pid>.log`: the run's log (`tracing` writes it beside
  stderr; Windows' GUI build has no console).
* `running-<pid>`: while the program runs. One whose process is gone at
  the next start is a run that did not close; the window offers its report
  (Show Report, Open Folder), or says there is none — the system killed it
  (out of memory, the realtime time limit's `SIGKILL`) or the machine
  stopped — and points to its log. The newest 20 reports and 8 logs are
  kept. Tests in `crash.rs` crash child processes (a write to a
  `PROT_NONE` page, a panic on a thread) and read what they left.

### Piano roll

**Zoom.** Like the arranger: the toolbar's −, +, Sel (the selected notes'
span and keys) and Fit (the whole clip, the keys it uses) (`toolbar::Zoom`,
`PianoRollView::zoom`), Ctrl+wheel for time, Alt+wheel (or Ctrl+Shift) for
the row height, `+`/`-`.

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

* Selecting: the selection keeps an anchor (`Selection::{anchor_track,
  anchor_clip, anchor_note}`: the last single item clicked). Shift-click
  selects from it to the click — clips on the rows between over the time
  from the earlier start to the later end, track headers and mixer strips
  in their shown order, piano-roll notes on the keys between over their
  span; Ctrl-click toggles one. Ctrl+drag (Smart, Select, Grab) draws a
  rectangle selecting the clips it touches (Ctrl+Shift adds; `Marquee`,
  outside `Drag`), Ctrl+Alt+drag in a body slips its contents (below); a
  click below the last track moves the playhead like a click on a lane.
  Selecting clips — Shift/Ctrl-click, a rectangle, a drag that moved
  clips — drops the edit range (`clips_not_range`), as a plain click and
  Select All did: Delete, Copy, Cut and Duplicate take a range with a
  length first, and a range left from before used to win over the clips
  just selected (clearing a time span on their tracks). Edits that take
  "the selected clips" take the arrangement's (`selected_arranger_clips`:
  never launcher clips, which the launcher puts in the same selection) —
  split, nudge, drags, the clip menu, quantize, merge, Delete.

* `EditorSettings`: edit mode (Shuffle / Slip / Spot / Grid, grid absolute
  or relative — `snap` is true exactly in Grid mode), tool (Smart, Trim,
  Time-Stretch Trim, Selector, Grabber, Separation Grabber, Scrubber,
  Pencil, Zoom), grid value (`GridDivision` down to 1/256, triplet and
  dotted; `GridDivision::menu` is the shared menu), Snap to Objects (see
  "Snap to Objects"), nudge value, Tab to
  Transients, Link Timeline and Edit Selection, Insertion Follows Playback,
  Follow Playhead (scrolling while playing), transient display and
  sensitivity, warp view, counter units, the shown global lanes, and zoom
  requests (a sequence number the arranger applies once). The Scrubber
  plays snippets of audio while dragging (see §6); the Pencil draws MIDI
  clips on instrument tracks and redraws samples at sample-level zoom.
* `Selection::range` is the edit selection (a time range on the selected
  tracks). Range operations — separate, trim, clear, copy/cut/paste, repeat,
  insert silence, nudge, Tab — and clip edits are `Batch` commands, one
  undo step each; rippling (Shuffle, or ripple on: see "Ripple editing")
  closes and opens gaps. Time added or taken away on tracks (Insert
  Silence, ripple clears and pastes) moves their automation too
  (`clipboard::LaneWork::ripple`: one `SetAutomationLane` per changed
  lane, `AutomationCurve::ripple` holding the value at the edit point over
  inserted time).
  Clear, Cut and Delete of a range in the other modes take the range's
  automation out, as Copy takes it (`LaneWork::clear_time`,
  `curve_cleared`: the points in it go, anchors at both edges keep the
  values outside, the line runs straight between; a lane with no point in
  the range is left as it is); the undo step is "Cut" for a cut. A range
  edge less than a sample inside an audio clip (grid positions are not
  whole samples) counts as that clip's edge — Clear, Cut and Trim to
  Selection used to split there, find nothing to split and refuse the
  whole step.
* The clipboard (`session::clipboard`, `Session::clipboard()`) holds
  values, never references: `Clipboard { kind: Range | Clips, length,
  origin, lanes, sources, rate }`. Copy (Ctrl+C, `Action::Copy`) takes the
  edit range on the selected tracks when it has a length (the clip parts
  in it and the tracks' automation of it), else the selected clips whole
  (never launcher clips) — audio with take folders, warp, pitch, clip
  effects, spectral edits, fades and gain; MIDI with notes, expression,
  controllers and SysEx. Every source those clips need goes along by
  value (`ClipContent::referenced_sources`): a paste registers a source
  again after its track was removed or its import undone (no file is
  copied; clips whose file is gone are left out with a notice). Lanes are
  the copied tracks in folder order, each with its `gap` in arranger rows
  (`Session::arranger_rows`) to the lane above. Paste (`Action::Paste`)
  goes to the insertion point (`Session::insertion_point`: the playhead;
  the edit cursor without Link Timeline), snapped to the grid in Grid
  mode (relative grid: the copied material's offset from the grid kept),
  on whole samples otherwise. The top lane lands on a selected track that
  is its own track, else on the first selected track, else on its own
  track; the others follow at their distance, each going on down to the
  next track that takes its clips (audio → audio tracks, MIDI →
  instrument/MIDI tracks) and left out with a notice when none does. Slip,
  Grid and Spot replace what lies under the pasted clips (a range: under
  the range, as before); Shuffle inserts and pushes. Pasted clips get
  fresh ids (MIDI notes too, expression following) and are never aliases;
  they become the selection and the insertion point moves to their end,
  so repeated Ctrl+V lays copies end to end (Ableton Live / REAPER).
  Delete in the arranger (the key, Edit → Delete Selection routed to it)
  is `Action::DeleteInArranger`: the range when it has a length, else the
  selected clips — never the piano roll's selected notes, which
  `Action::DeleteSelection` (control surfaces' Delete) still takes before
  clips; the clip menu's Delete removes its clips even with a range.
  Duplicate (Ctrl+D) and Repeat (Alt+R, `Action::Repeat(n)`) place copies
  of the range or of the selected clips right after them, end to end, and
  select the last copy; replacement clears are computed for all copies at
  once (`clear_spans_commands`), so repeats over a long clip work. Cut
  removes the copied clips like Delete (or clears the range). Range-only
  `CopyRange`/`CutRange`/`RepeatRange` stay; `PasteRange` pastes the
  clipboard. Object copies carry no automation (as moves never did).
* Select All (`Action::SelectAllClips`, Ctrl+A in the arranger) selects
  every clip on the rows the arranger shows (not in closed folders, never
  launcher clips) and their tracks, and drops the edit range; the piano
  roll, event list and pitch editor select their own notes and rows;
  text fields keep Ctrl+A.
* Splitting (S, Split at Playhead; Separate with a range) keeps every
  piece of every selected clip selected, across tracks, so repeated
  splits of aligned drums need no reselecting; clips the cut does not
  cross (starting or ending there, `Session::cuts_inside`) stay as they
  are, a zero crossing that would leave the clip falls back to the
  playhead.
* Slip editing (`session::slip`, `Action::SlipClips { clips, by }`):
  Ctrl+Alt+drag in a clip's body (Smart, Selector, Grabber; whole grid
  steps in Grid mode, Shift free; a click toggles like Ctrl-click) moves
  the audio or MIDI inside the clips — all selected when the pressed one
  is — while place, length, fades and gain stay. Audio offsets stay
  inside the source; pitch and spectral edits are in source frames and
  stay with the audio; a clip with clip effects slips within its render
  but never into the devices' latency before it; a warped clip's map
  moves with the audio (`slip_audio`: output frame u plays what u − d
  played; markers keep their source frames, those leaving the clip go);
  reversed clips move their offset the other way; MIDI moves and what
  leaves the start is cut there (as a start trim). Take folders do not
  slip (as they do not trim). Drags are absolute within one gesture from
  `gesture_clip`, so they are lossless and one undo step.
* The editing context (`faderframe-ui/src/edit_keys.rs`): selection is
  the session's, keyboard focus GTK's; the context is the editor view
  last pressed or focused (`is_editor`: arranger, piano roll, automation,
  album, pitch, launcher, event list, spectral, setlist). Toolbars, tool
  selectors, transport controls and non-editor views take the focus but
  not the context and select nothing. Edit → Cut/Copy/Paste/Duplicate/
  Select All (`app.cut` … `app.select-all`, Ctrl+X/C/V/D/A in
  `palette::DEFAULT_SHORTCUTS`; application accelerators run before the
  focused widget) and Undo/Redo route by focus (`command_routes`): a text
  field gets its own `clipboard.*`, `selection.select-all`, `text.undo`;
  a focused editor gets the key (only it); another focused view gets it,
  then the context; in plugin editors and dialogs nothing behind them.
  Plain editing keys the focused widget leaves unused (Delete, S, B, Q,
  nudges, …) go to the context from the main window and detached view
  windows (`install_window`, also the single-key transport shortcuts
  there now) — never while typing or with another editor focused; Tab,
  arrows, Enter, Space, Escape and the page keys stay with focus
  navigation (Escape also ends MIDI learn and leaves a video window's full
  screen: the context used to swallow it, clearing notes or the range).
  Without a shown context the arranger is it, if shown. Edit → Delete
  Selection (`app.delete`) routes like Cut and Copy (the editors' own
  Delete key: notes in the piano roll, rows in the event list, the range
  or clips in the arranger; `selection.delete` in a text field). The
  launcher and the automation view handle no keys, so they do not take
  the context (keys pressed after clicking them reached nothing).
  Shortcut collisions resolved: Ctrl+, is the Preferences', so the
  nudge-trims of the end are Ctrl+Alt+, / Ctrl+Alt+. (Ctrl+. too); F10 is
  GTK's menu bar key in the main window (Pencil is Alt+0 everywhere, F10
  in detached windows); F11 is the Inspector's, but in a video window gone
  full screen it toggles full screen as it did (`video::toggle_active_full_screen`);
  Ctrl+P (Punch) is in `DEFAULT_SHORTCUTS` (it was set apart, so the
  shortcut editor showed no default and Reset cleared it); the shortcut
  editor's window is not an application window, so the shortcuts being
  set no longer run while it waits for keys (Ctrl+Q quit). Ctrl+Tab stays
  the keyboard's way into a view's controls, so Tab to Transients has no
  extend key.
* Multi-clip edits (`MoveClips`, `TrimClips`, `ClipGain`, `SetFade`,
  `SetClipsMuted`) act on all selected clips when the pressed clip is
  selected. Drags send absolute values inside a gesture and the session
  recomputes from `gesture_clip` — the clip as the gesture first saw it —
  so dragging back is lossless (trimmed notes and audio return). A
  vertical move counts the rows the arranger shows (`arranger_rows`:
  folder order, closed folders' tracks left out; it counted the project's
  flat list, landing clips on hidden tracks). Shuffle trims of several
  clips on a track add up (`editing::Trimmed`, `trims_commands`: each clip
  moves by what the trims before it added — they used to overlap), drags,
  nudge-trims and time-stretch alike. Nudges pass over take folders for
  trims (they refused every clip) and move a group back together, stopping
  at zero. A start trim of a clip with clip effects stops at the render's
  start (`rendered_offset`), never in the devices' latency before it. The
  Separation Grabber separates on its first move (not on a mere click),
  takes the range out of every selected track and moves those pieces
  together, all one undo step ("Separate and Move").
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
mixer, Tools, album, piano roll, automation and performance views; Mixing
adds the mix scenes beside the mixer, Mixing and Mastering the Reference
view beside the meters, a tab behind them; Mastering opens on Tools).
Views added after a layout was saved register on first use; saved layouts
are never rewritten to follow the presets (a view a preset later places
joins its place in the tab order once shown; Reset Workspace gives the
preset). `faderframe_ui::dock::realize`
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
A view goes to its home area when shown, else to its kind's
`ViewKind::default_area` (the Inspector's `left`, every other view's
`bottom`); the `left` area beside `main` is made on first use
(`WorkspaceLayout::ensure_left_area`, `LEFT_AREA_RATIO`), so layouts saved
before it existed need no migration, and while empty it takes no room.
The bottom dock hidden (F2, its tab strip's ▾ button) leaves a bar with its
views' names and, at the right end where the ▾ was, the ▴ that shows it
again (`dock::collapsed_bar`, issue #16).

### Inspector and Channel Overview

Crate `faderframe-view-inspector` (GTK-free) holds two views of the
selected track — `chain::inspected_track`: the anchor (the track clicked
last) while it is selected, else the first selected in `folder_order`
(the master last); removed tracks are skipped; several selected: it shows
one and says "1 of n selected" (edits reach every selected track through
`group_edits`, as from the mixer).

* `InspectorView` (`ViewKind::Inspector`, area `left`; the header bar's
  "i", View → Inspector, F11, `app.view-inspector`; dev action
  `show-inspector`) — Studio One's Inspector: the name (click: rename),
  colour, kind, format and number; foldable sections **Routing** (input,
  output / the instrument a MIDI track plays and its MIDI device,
  monitoring as segments, format, pan mode, surround panner, object),
  **Track** (record arm, MPE, group/VCA, folder, a folder's contents and
  open state and its **Folder Bus** switch — `Field::FolderBus` =
  `Action::SetFolderBus`, for a plain folder and a folder bus alike; a
  folder bus is a bus, so its bus controls are the rows and strip of one:
  what feeds it — `chain::input_label` counts tracks by
  `Project::output_target`, so the tracks it holds count —, output,
  format, pan mode, group/VCA, automation, its devices and a full strip —,
  automation modes of every lane, freeze; the master: Listen
  and Console), **Devices** (the chain, one line each) and **Channel**: the
  console strip of the track hosted in the view — `MixerView::single_strip`
  (`show_track`; any kind, folder strips and the master included; no
  inserts, no cheeks, at most the mixer's widest strip) — so fader, meter,
  pan modes, mute/solo/arm, sends, input stage, MIDI learn and their menus
  are the mixer's own. Events reach the strip in its coordinates; its
  requests (menus, text entry) are moved back; a press there keeps the
  drag; the wheel moves its controls, elsewhere scrolls the Inspector.
* `ChannelOverviewView` (`ViewKind::ChannelOverview`, the bottom dock like
  Clip Effects; View → Channel Overview, `show-channel-overview`) — #14:
  the signal path in one row: Input → a card per device → "+" → Channel
  (level, pan, M S R) → Sends → Output.

`chain::devices` walks a track in `Track::slots` order with depths (input
stage, legacy instrument slot, inserts, after a container what its chains
hold) and reads role, failure (`plugin_failed`), latency
(`EngineController::plugin_latency`), gain reduction, sidechain, program.
Every control is an existing action: power = `SetPluginBypass`, click =
`OpenPluginEditor` (Ctrl: bypass, Alt: remove, as on the mixer's slots),
× = `RemovePlugin` / `SetPreamp { model: None }` (the master's input
stage: `SetConsole { family: None }`) / `RemoveFromChain`, drag =
`MovePlugin` (top-level inserts only; `chain::drop_index` counts the other
inserts before the gap; the session keeps MIDI effects before the
instrument), "+" = `OpenPluginBrowser` (right-click: the mixer's quick
list), right-click on a card = the mixer's insert menu
(`MixerView::insert_menu`, `stage_menu`; the strip menus `input_menu`,
`output_menu`, `plays_menu`, `midi_out_menu`, `send_menu`, `track_menu`
are public for this). Rows take their values and menus from the session's
choice lists (`input_choices`, `midi_*_choices`, `format_choices`,
`pan_mode_choices`, `group_menu`, `folder_choices`, `listen_choices`,
`console_choices`, `object_choice`) and `shown_*`: no second copy of a
track's settings. Screen readers get every line (a menu's choices as
toggles under it), the devices with their power and remove buttons, and
the strip's own nodes under Channel.

### Menu checks (toggles)

`faderframe_ui::toggles` (issue #6): every on/off command is a boolean
stateful GIO action, every one-of-several a string state with the choice
as the item's target, so GTK's menus draw the check or radio dot (and
expose check-box / radio menu items to screen readers). One table —
`TOGGLES` (name, state from the session, press) and `CHOICES` (edit mode,
edit tool) — and `toggles::follow`, run from `AppState::update_chrome`
(after every change and ~10 times a second), keep every state, also of
actions defined elsewhere (stop-to-start, pre-roll on a bar, count-in,
the recording radios, master panel), in step whatever changed it: menus,
toolbar buttons, shortcuts, surfaces, MIDI learn. The Preferences'
Snap/Follow check boxes are bound to the same actions; the command palette
shows a ✓ for commands that are on (`palette::checked`). Edit → Edit
Options lists the edit flags; Transport has Metronome On and Input
Quantize. A new toggle is a row in `TOGGLES` plus a menu item. Dev actions
`show-menu:<Menu>[/<Submenu>]` (opens it, logs each entry's state; the
screenshot saves `<stem>-menubar.png`), `command-palette-search:<text>`.

## 12. Metering and metrics

Meters: the RT side accumulates per-channel peak and mean square with atomic
max; the UI consumes with atomic swap — no peak is lost between frames. A
channel strip's meter reads its post-fader, post-pan output in the
destination layout (a mono strip into mono lights both bars alike); buses
and the master meter what they sum, after their own fader and pan.
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
8. Every summing point is latency-aligned (but for early edges of
   low-latency monitoring), every heard output too; plugin latency is part
   of a node's identity (a change rebuilds the graph); automation is read
   at the position a node's audio was played; what is shown as playing is
   what is heard.
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
    stretcher), `faderframe-ui` (GObject subclassing macros, dmabuf
    textures, WGL/D3D12 texture import — the GPU painter's and video
    frames', through GStreamer's d3d12 library —, the crash reporter's signal
    handlers and exception filter in `crash.rs`) and `faderframe-ui-gpu`'s `dmabuf`
    and `d3d12` modules (Vulkan external memory through wgpu's hal and
    ash; shared D3D12 textures through wgpu's hal and the windows crate;
    the rest of the crate denies it).
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
| Video frames to GTK | VA-API dmabufs | D3D12 textures (untested) | through memory |

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

The packages are not signed with paid certificates: the macOS app is
signed ad hoc and not notarised (Gatekeeper asks once), the Windows
installer and zip are unsigned (SmartScreen asks once) — see [BUILDING.md](BUILDING.md#packages).
CoreAudio's IO workgroup is joined (see *Multicore*), by sandboxed
plugins' audio threads too.

## Status and roadmap

**Implemented.** The engine: routing graph with PDC, state adoption and
sidechains; multicore scheduling with measured critical-path ranks;
sample-accurate transport, loops and scrubbing; a 24 band EQ in the spirit
of Pro-Q 4 (zero latency, natural and linear phase; dynamic and spectral
bands triggered by their region, free cuts or the sidechain; EQ Match,
Sketch, Spectrum Grab, an analyser with collisions, the instance list), the Program
EQ (PultEQFx's circuit-modelled passive tube EQ with its panel), the stock
devices with editors of their own (compressor, true-peak limiter,
gate/expander/ducker, de-esser, a console channel strip, the Guitar Station (GainStageFx's pedals,
amplifiers, power stages and miked cabinets), saturator, utility, delay, algorithmic
reverb, chorus/flanger/phaser, tuner; a virtual analogue synth, a sampler
with SFZ import and a drum sampler), the 76 Compressor (a FET limiting
amplifier), the 2A Leveler (an optical leveller, its T4 cell and valve
make-up amplifier modelled), the Tape Machine (Jiles–Atherton tape, heads,
transport), six circuit-modelled microphone preamplifiers, circuit-modelled
console summing (six families), a hardware insert with its round trip
measured, and gain-reduction meters from every plugin format; offline
render and export (stems, normalise, dither); freeze and bounce in place. Audio: native PipeWire,
JACK, the system API (WASAPI, CoreAudio, ALSA), ASIO (opt-in) and a dummy
device. Render-ahead (anticipative processing) of every track nobody plays
live.
Recording with punch, pre- and post-roll, count-in, metronome, take
folders and comping (take lanes of any height, each take auditioned on
its own), aborting or restarting a take, cue mixes for the performers'
headphones, and singing or playing into MIDI (voice ports, monophonic and
polyphonic).
Automation of every automatable parameter, written from controls, MIDI
controllers and plugin editors. Media import (Symphonia, rubato) and
lock-free disk streaming.

MIDI: devices with hotplug, live play with constant latency, recording,
always-on capture of what was played (Capture MIDI), MIDI learn, MIDI output and clock, clock/MTC sync, MPE and native note
expressions for CLAP and VST3 instruments, SysEx to devices and plugins,
Standard MIDI File import and export, a full piano roll (drum notes as
diamonds) and a MIDI event list, input quantize, and MIDI 2.0 on Linux
(UMP devices through ALSA, `.midi2` clip files).

Plugins: CLAP and VST3 hosting with crash-safe scanning, LV2 on Linux and
Audio Units on macOS, sandboxed instances (a process each) on all three
platforms, VST3 program lists, optional 64-bit processing, a plugin
browser, embedded editors on all three platforms, generic
parameter windows, presets (user, VST3 factory, LV2, `.aupreset`), inserts that
move and copy between tracks, sidechain inputs, multi-output instruments
with tracks for their extra outputs.

Mixing: an analogue-console mixer, sends in banks, track groups with
linked controls, VCAs, folder buses and folder groups, relative edits of
every selected track, track presets, pan laws and dual panning, surround
panning (room or VBAP, diffuse spread, custom speaker layouts, bass
management), meter styles (peak and RMS, VU, PPM, K-System) with a VU
meter bridge, track delays to a fraction of a sample with microphone
Auto-Align, A/B listening (the mix against a step of the undo history or a
loudness-matched reference track), Mix Intelligence (masking between
tracks, levels and mono compatibility, measured and explained, with
auditionable experiments), and the Tools view for mastering (EBU R128 loudness and true peak,
loudness range, PLR, crest factor and DR value, levels with K-System
scales, phase, spectrum). Delivery: loudness
normalisation and true-peak limiting on export, noise-shaped dither,
delivery presets, and the album (songs analysed and exported with album or
per-song levelling, pauses or crossfades, a song's own inserts heard on
the master, ISRC/UPC and credits, a cue sheet and a verified DDP 2.00 CD
master with CD-Text), surround beds and objects with ADM BWF (Dolby Atmos
master profile) and IAMF masters, and binaural monitoring (measured heads
or a SOFA file, headphone correction) with a mono check.

Editing: Pro Tools-style edit modes and tools, ripple editing (per
track, on chosen tracks or across the whole project with the timeline,
automation moving in sync), edit-selection ranges, clip gain, shaped fades, transient detection, warp markers and pitch-preserving
playback, sample-level waveform redraw, global lanes for markers, song
sections (moved, copied and deleted with their content), time signatures
and the tempo map, and track colours from a colour chooser. Snapping to
samples, zero crossings and objects; a clipboard for ranges and clips;
slip editing; scratch pads beside the song; spectral editing; polyphonic
pitch editing; song structure found in a jam; tempo through hit points;
lead sheets and guitar tab (MusicXML, PDF); clip effects, each device in
its own window too; time-locked tracks (material kept in time through
tempo changes, ADR cues locked to the picture).

Shell: docking and detaching, workspaces (Recording, Editing, Mixing,
MIDI, Mastering), nine skins switched live and themes of the user's own,
performance meter, preferences, recent projects, a start window and
start-up choice, project templates (a project's set-up without its
content; a default one for New Project), project versions, the Inspector
and Channel Overview, a command palette with a shortcut editor, the undo
history, crash reports (with software drawing after a graphics driver's
crash, and from the start on the chips whose driver crashes), plugin
checks, screen readers, and a setlist with a show mode.
Windows and macOS builds and packages for all three platforms (see §14).

Added in the 2026-10/11 wave: an undo tree with Audio Bisect (*Undo/redo*,
*A/B listening*), the codec monitor and platform previews, mix scenes,
null-tested stems and the QC report, self-freezing, stem separation
(X-UMXL), restoration devices (de-noise, de-click, de-hum, de-clip,
de-reverb), the convolution reverb, multiband dynamics and the vocoder,
Wavetable, FM and Granular instruments, Pitch Correct and aligning doubles
to a guide, slicing to pads or keys, a device's own A/B and undo, clip
envelopes, Strip Silence, batch processing, Capture Audio (retrospective
recording), several audio interfaces joined natively, microtonal tuning,
ambisonics with head-tracked listening, articulation and drum maps, gear
changes per song in show mode, ARA hosting, MIDI 2.0's resolution in clips
and on macOS, merging project versions, comping by lyric, A/V calibration
by a camera and panning that follows the picture, polyphonic lead sheets
(a staff heard as the pitch editor hears it, piano parts on a grand staff),
notes, markers and sections locked to time, more views for screen readers,
the Hardware Insert's Mono → Stereo, DAWproject export and import, and
MusicXML scores imported as tracks, and Lua scripts and macros (a
sandboxed `ff` API, recorded macros, MIDI and surface triggers), and AAF
export and import for Pro Tools, Media Composer and Resolve (OMF is not
read: see *AAF interchange*), and a media browser (indexed sounds, loops
and MIDI files with tempo, key and Find Similar, previews in tempo and
key, placed by dragging), and AES67 network audio (a PTP-clocked backend
or streams beside another device; Dante in its AES67 mode; NDI is out —
its SDK's licence), and a phone and web remote (paired by a code or a QR
code, local network only: transport, mixer, cue mixes for performers,
launcher, setlist, lyrics), and a score editor (MIDI clips engraved and
edited as notation, two voices a staff, the piano roll's notes), and 360°
video (a viewport turning with the binaural scene, panning by clicking the
picture, spherical and ambisonic metadata kept on export), and live
collaboration (one project edited by several people over an encrypted link,
undo per person, media fetched by content hash, Listen Along), and adaptive
music for games (an interactive score of states, layers, transition rules
and stingers, previewed live and exported as looped stems with FMOD and
Wwise imports), and a map editor for articulation and drum maps.

Picture (video, phase 1): `faderframe-video` on GStreamer as a library
(probe, a demux-only frame index, exact-frame decoding, MJPEG proxies,
sound from video, muxing with the picture copied, a frame service with
player, seeker and thumbnailer threads); frames are asked for by their
index start and the decoder seeks 2 ms into them (`SEEK_INTO`):
containers round frame times (Matroska to milliseconds, so at 29.97 fps
a frame's predecessor seems to end after it starts, and a proxy's times
differ from its original's), and a seek to the start itself handed out a
sliver of the frame before first — stills one frame early, and every
frame of a run played from there one frame late. A frame shows (and a
timecode label reads) from a millisecond before its start
(`faderframe_core::timecode::FRAME_HAIR_NS`, `FrameIndex::frame_at`,
`FrameRate::frame_at_sample`), so a frame's start lands in it however it
was reckoned, and a label's position is its frame's first sample
(`FrameRate::first_sample`; the nearest one lay before up to half the
frames at 29.97 and 59.94, the counter and the picture a frame behind a
typed or spotted timecode). Seeks and the index read past a demuxer's
own end (`streams::open_seek`, `end_after_late_start`: an MP4 without an
edit list, whose B-frame delay starts its picture after 0, ended for
qtdemux at its media's duration, two frames short); video tracks in
absolute time with the project's SMPTE timecode (every rate,
drop-frame); the presenter
showing the frame for the sample heard when it reaches the screen (the
audio engine is the clock); the video window (docked, detached, full
screen, timecode overlay), the arranger's Video lane, picture offset,
the flash-and-beep sync test and Export Movie. Create Output Tracks gives
every output of a multi-output plugin, the main too, a track of its own.

Picture, phases 2 and 3: zero-copy display on Linux (VA-API decodes and
scales into an RGB dmabuf the display takes, `faderframe_video::zero_copy`,
`Painter::external`; RGBA otherwise), decoding at device pixels, a
late-frame meter with presentation feedback; J/K/L shuttle in reverse and
up to 8× heard as scrub snippets (`session::shuttle`), frame steps;
several video tracks compared side by side or wiped; trims in the lane;
export of a clip's span cut at keyframes with a timecode track
(`qt_timecode` reads them back); timecode counters on the transport
display and in Spot; proxy settings and the cache; cut detection to
markers. Colour management: the stream's colours kept with the frame
index, HDR (PQ, HLG) and wide gamut mapped for an SDR screen
(`faderframe_video::colour`: BT.2390's EETF to reference white, BT.709
primaries), proxies mapped the same way (and proxies keep full-range JPEG
colours now). Every proxy is decoded by a `Decoder` (as shown: mapped,
JPEG at its own full range whatever a QuickTime colour atom implies —
qtdemux took its missing range for video range) and written at its
frames' stream times with the source's frame rate; the frame service
reads a proxy relative to its first frame (`service::in_file`), since
Matroska readers start a stream at its first block (a picture starting
after 0 read back early in its proxy). MXF and DV demuxed in every package; ProRes, DNxHD and the
other intermediates decoded through an LGPL FFmpeg the Linux tarball
builds (`packaging/linux/lgpl_libav.sh`). GStreamer's MXF demuxer before
1.28.4 crashes (a NULL read in `find_edit_entry`, on seeks and at a file's
end, for a track whose index delta it has not matched yet; Debian 13 has
1.26.2, which the tarball bundles): where it is such a one and gst-libav
has `avdemux_mxf` (the tarball's FFmpeg includes the MXF demuxer),
`streams::avoid_crashing_demuxers` ranks it below FFmpeg's at start-up
(`faderframe_video::mxf_by_ffmpeg`). FFmpeg's has no uncompressed picture
(a GStreamer-made UYVY MXF is refused with "no picture" — it crashed
before); FFmpeg-made XDCAM/IMX/DNxHR and OP-Atom files import. Conform to a new cut from EDLs
or OpenTimelineIO, or by matching the old and new picture shot by shot
(`faderframe-conform`, `faderframe_project::conform`, `session::conform`;
one undo step, new shots marked). ADR (`faderframe_project::adr`,
`session::adr`, the cue list view): cues from the transcript, beeps on a
track of their own, rehearsing and recording a cue with pre-roll, punch
and streamer over the picture, takes rated. Picture outputs
(`faderframe_video::output`, `session::picture_out`): a Blackmagic
DeckLink card's SDI or HDMI paced by the card, and full screen on a
monitor of its own. Not done: shared D3D12 frames on Windows (nothing
here can run D3D12 sharing; the decoder's D3D12 frames to GTK are written
but untried, see *Video frames on the GPU*), video tried in the Windows and macOS packages
on real machines, and a DeckLink card tried (none here).

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
   `Track::folder`, `Project::folder_order`, `session::folders`; folder
   buses: see *Folders and folder buses*), ~~clip
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
   ~~audio-to-MIDI (basic-pitch, Apache-2.0)~~ (done: *Audio to MIDI*), ~~tempo and key
   detection~~ (done: *Tempo and key from clips*), ~~per-clip effects rendered offline~~ (done: *Clip effects*), ~~a speech and lyrics transcription track (Whisper,
   MIT)~~ (done: *Speech and lyrics*), ~~stem separation~~ (done: X-UMXL, CC BY
   4.0, see *Stem separation*).
6. ~~**Performance and control**: a clip launcher with scenes recorded
   into the arrangement, control surfaces (Mackie Control/HUI, OSC)~~ —
   done (see *Clip launcher* and *Control surfaces*).
7. ~~**Ports**: sandboxed plugins' audio threads in the device's
   workgroup~~ — done (`faderframe_plugin_sandbox::workgroup`: a
   bootstrap service hands helpers the workgroup's Mach port). Packages
   stay without paid signatures (the macOS app ad-hoc signed, not
   notarised; the Windows installer unsigned), and the Flatpak is a
   bundle on the releases page, not on Flathub (Flathub does not take
   LLM-generated apps).
8. ~~**MIDI**: MTC output, varispeed chase without a shared word clock~~ —
   done (see *MIDI time code out and varispeed*).
9. **Performance**: ~~render-ahead for buses whose inputs are all rendered
   ahead~~ (done: the shallow tier, see *Render ahead*), ~~job affinity for
   cache locality~~ (done: see *Affinity*), ~~an optional wgpu painter for
   dense views~~ (done: see *The GPU painter*, with dmabuf hand-over on
   Linux and shared D3D12 textures on Windows).
10. ~~**Mastering**: multiple CD-Text languages, a DDP player/import,
    surround beds and panning before any object-based format~~ — done (see
    *CD-Text languages*, *The DDP player* and *Surround beds*). ~~Object-based
    masters (ADM BWF, Dolby Atmos master profile), export and import~~ —
    done (see *Object-based masters*, with Dolby's `dbmd` chunk).
    ~~Binaural monitoring~~ — done (see *Listening*). ~~Consumer object
    formats~~ — IAMF done (see *IAMF masters*; MPEG-H is out — no open
    encoder, a paid spec and per-unit patent royalties).

**Then**, after the survey's waves, roughly in order (open work first,
then ideas: what FaderFrame's pieces make possible, and pro features no
DAW does well yet):

11. ~~**Time-locked audio**~~ — done (see *Time-locked tracks*): a
    per-track time base, Musical or Time-locked; ADR cues always locked to
    the picture; ~~a per-note lock inside MIDI clips; markers placed in
    time~~ (done: locked notes, markers and sections, see *Time-locked
    tracks*).
12. ~~**Video, phase 2 — display and playback**~~ — done but for shared
    D3D12 frames on Windows and real-machine tries (see *Picture, phases
    2 and 3* above): zero-copy display (dmabuf
    on Wayland, shared D3D12 textures on Windows) and decoding at device
    pixels on HiDPI screens; a late-frame meter (the session counts
    already) and presentation feedback; shuttle and reverse play from the
    proxies; several video tracks with A/B (side by side or wiped; tracks
    are added from the Video lane, `VideoOp::AddTrack`); trimming clips in
    the lane
    and exporting a trimmed span (cut at keyframes, or an edit list); a
    timecode track in exported movies and timecode in the transport
    display; proxy settings (size, where they live, clearing the cache);
    cut detection to markers. Try video in the Windows and macOS packages
    on real machines.
13. ~~**Video, phase 3 — post-production**~~ — done (see above; the
    DeckLink output untried without a card): ADR (cues from Whisper's
    transcript, streamers and punches over the picture, beeps, takes
    rated in their folders); conform from EDL/OTIO, helped by cut
    detection; SDI output through Blackmagic DeckLink (read its SDK's
    licence first); MXF through an LGPL FFmpeg; colour management and HDR.
14. **Gaps**: ~~MIDI learn for solo, record arm, modulator macros and the
    Surround Panner window~~, ~~hardware inserts~~, ~~the Guitar
    Station's microphones as outputs of their own~~, ~~a 76-style FET
    compressor~~ — done; ~~ARA hosting (Melodyne, SpectraLayers; the ARA
    SDK is Apache-2.0)~~ (done: *ARA*, untried with a real ARA plug-in);
    ~~stem separation~~ (done: *Stem separation*);
    ~~`rendering_ahead_does_not_allocate_on_the_audio_thread`'s rare timing
    failure under load~~ (done: the paced tests wait for the render-ahead
    tiers on a retry, see *Render ahead* → Paced tests on a busy machine).
15. **Ideas from what FaderFrame has**:
    - ~~*Lead sheets from a recording*~~ — done (see Lead sheets): the
      melody heard, chords from the chord track or the other tracks' MIDI,
      words from the lyrics, key and tempo, as MusicXML or a PDF; a
      track's clips as one line, several tracks as a score.
    - ~~*Comping by lyric*~~ — done (see Comping by lyric): Whisper's
      word timings on every take; choose the best take word by word, the
      syllable shown under each note in the pitch editor.
    - ~~*Tempo through hit points*~~ — done (see Tempo through hit
      points): cuts and markers as hits, the steadiest step tempo map that
      lands each on a beat or a bar line.
    - ~~*A/V calibration by itself*~~ — done (see A/V calibration and
      panning that follows the picture; untried with a real webcam): the
      flash-and-beep test seen by a webcam and heard by a microphone gives
      the picture offset.
    - ~~*Head-tracked binaural monitoring*~~ — done (see Ambisonics and
      head-tracked listening): a phone's motion sensors over OSC turn the
      binaural scene.
    - ~~*Panning that follows the picture*~~ — done (see A/V calibration
      and panning that follows the picture): an object tracked on screen
      becomes the automation of an object or surround pan.
    - ~~*Circuit-modelled console summing*~~ — done (see Console
      summing): the circuit solver behind the preamps and the Guitar
      Station as transformer and summing colour per channel and bus.
    - ~~*Auditioning the undo history*~~ — done (see *A/B listening*): every edit is a command, so the
      mix as it was some steps ago can play beside the mix as it is (A/B)
      without undoing anything.
16. **Pro features no DAW does well yet**:
    - ~~*Auto-align*~~ — done (see *Track delay and Auto-Align*): delay
      and polarity between overheads, close and room microphones by
      cross-correlation, set in one undo step; ~~a project health check
      (phase, DC offset, clipping inside chains, unused plugins costing
      DSP)~~ — done in Mix Intelligence (Health and Phase findings).
    - ~~*Reference tracks*~~ — done (see *A/B listening*): a loudness-matched A/B player in the monitor
      path, its spectral balance compared and handed to EQ Match.
    - ~~*A masking map*~~ — done (see *Mix Intelligence*): every pair of
      tracks compared in time and frequency, with auditionable cuts; with
      level, mono, phase, width, dynamics, reference and health
      diagnostics.
    - ~~*Spectral editing*~~ — done (see Spectral editing): a
      spectrogram to paint noises away in.
    - ~~*Mix scenes*~~ — done (see Mix scenes): console snapshots
      recalled, compared, crossfaded and morphed.
    - ~~*Song structure from a jam*~~ — done (see Song structure from a
      jam): self-similarity of the chroma finds verses and choruses as
      sections and markers.
    - ~~*Adaptive music for games*~~ — done (see Adaptive music for games
      (the interactive score); the FMOD and Wwise imports are written to
      their documented APIs and untried in either): sections and launcher
      scenes with transition rules, exported as stems and metadata for
      Wwise or FMOD.
    - ~~*Show mode*~~ — done (see Setlist and show mode, and Cue mixes
      for the in-ear outputs), ~~program changes to external gear per
      song~~ too (see Setlist and show mode → Gear changes).
17. **Further out**: ~~MIDI 2.0 on macOS (CoreMIDI's `MIDIEventList`)~~
    (done, tested on the macOS runners only) and Windows (Windows MIDI
    Services: still open — a preview SDK under a go-live licence, not in
    the `windows` crate; see *MIDI 2.0*), ~~clips that keep MIDI 2.0's
    resolution~~ (done: 16-bit velocities, 32-bit controllers, see *MIDI
    2.0*); ~~more views for screen readers (Album, Launcher, Automation,
    Tools and Video, the edit toolbar)~~ (done: *Screen readers and the
    keyboard*); ~~merging project versions (three-way) for
    collaborators~~ (done: *Project versions and merging them*).

Suggested next: Windows MIDI Services once its SDK is released (17);
trying the untested platform paths (D3D12 video and shared frames on Windows, ARA with
a real plug-in, AES67 and DeckLink hardware).


### Guitar Station

`faderframe-guitar` is GainStageFx's guitar half on `faderframe-circuit`'s
solver (`UPSTREAM.md`): the pedal, wah, amplifier and power-stage netlists,
the loudspeaker/cabinet/microphone models and the measured calibration,
unchanged; upstream's `Chain` split into the amplifier stage (`chain`) and a
stage per pedal (`pedal`). Against upstream's untouched chain the amplifiers
are bit-identical and the pedals within 1e-8.

The built-in `faderframe.guitar-station` (`plugin_host::devices::guitar`)
runs the line as a pipeline: each added pedal and the amplifier is a
reservoir worker of its own, one device callback behind the stage before
it, so a pedal's cost never adds to another's thread, and each stage
reports its buffer (and at 2x its oversampler) as latency; adding or
removing a pedal therefore restarts the device, a footswitch or a model
change does not. Per stage each channel is a thread of its own (`WorkerPool`
helpers: a stereo signal two, a 5.1 one six, within the physical cores and
the user's processing threads, `faderframe_realtime::channel_threads`;
channels beyond them share threads) and a duplicated mono one circuit that
wakes the other from its history, as the preamps do. A place's circuit is built by the workshop
thread and swapped in on the stage's worker without allocating (offline the
stage waits for it). Guitar lanes travel beside the audio through every
stage for the DI, which leaves on the second output bus at unity. Offline
and ahead the stages run inline with the same delays (bit-identical to the
workers). The editor (`view-devices::guitar`) draws the rig as hardware
with the Program EQ's renders.

Added here since (`UPSTREAM.md`, "Added here"), each from published
drawings, data sheets and measurements:

* **Compressors** (family Dynamics): the Red Squeeze (the MXR Dyna Comp,
  CA3080 version, `circuits::red_squeeze`, held to a built unit's voltages
  and the DAFx-11 measurements: +34 dB on a quiet note, ceiling about
  −12 dBV, release 0.31 s to two thirds) and the Juicer (the Orange
  Squeezer, `circuits::juicer`: a JFET shunt linearised by half its drain
  swing ahead of a ×23 op-amp, a germanium detector). The solver gained
  an operational transconductance amplifier for them (`device::Ota`,
  `Netlist::ota`, `OtaSpec::CA3080`: Iout = I_ABC·tanh(ΔV/2V_T), the bias
  pin a junction above V−, input bias currents, a current output
  saturating inside its headroom; `faderframe-circuit/tests/ota.rs`). The
  Ross was modelled too and measured as the same compressor within the
  Dyna Comp's own knobs (`ROSS`, kept for that test). The Juicer's bias,
  a trimmer whose whole squeeze lies in a fifth of its turn, is drawn over
  what the circuit measured (`juicer::BIAS_LAW`, a `Taper::Table`): the
  squeeze in even decibels to 0.7, the bias procedure's point at noon,
  then the limiter into clipping.
* **The Carbon Echo** (family Delay; the MXR M169 from a traced REV D3
  board: four BL3208A, 8192 stages, a CD4047 clock, the DM-2's emphasis
  and filters): two netlists, the audio board and a clock board stepped
  at about 12 kHz; the SA571 compander's gain cells are resistors set by
  per-sample rectifier models; the clock is a law tabulated from a
  transient model of the 4047 and its modulation network, kept in the
  tests. Its line is a clocked bucket brigade (`dsp::bbd::Clocked`):
  sampled each clock period, held a period, the steps band-limited into
  the netlist's rate, so a slow clock aliases and images as the chip does
  and turning the delay bends the pitch; sized by stages, never by time.
  18.7–600 ms; the wet response within about 2 dB of a measured unit's.
  About 37 % of a core at 1x (`is_expensive`). It disagrees with the
  M169A manual twice, both times as the traced circuit does (the module's
  "Where the model and the manual disagree", measured stage by stage in
  its tests): 4.7 % THD at the manual's distortion test point (1 kHz,
  −5 dBV) against "under 1 %" — the compressor's rectifier ripple (C5's
  2.2 ms), Q7's tiny base-emitter error returned some 36 times larger by
  the anti-aliasing Sallen-Key's 248:1 capacitors, and Q9 all but cutting
  off on TR8's 6 k AC load, the same at every step rate; and a top
  modulation speed of 3.65 Hz against 2.2 — the traced LFO's own
  arithmetic, whose bottom is the manual's 0.2 Hz, while no single traced
  part moves both ends to the manual's (about 100 k at TR1's top would).
* **Amplifiers**: the Tweed Lux (Fender 5E3: both channels, the volumes
  interacting through the shared grid, the cathodyne inverter
  (`power::CathodyneFront`), cathode-biased 6V6s, the 5Y3's sag) and the
  Seattle 100 (Soldano SLO-100: all eight triodes, the LDR channel
  switching, Crunch and Bright, the depth network `power::DepthNetwork`
  on the SWEEP knob), each two `Gain`s (one per channel; the other channel
  is built and loads the played one), their power stages matched, their
  calibration and power-trim rows measured by
  `tests/calibrate_new_amps.rs`.

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
calibration curves. British 73 includes its separate line driver, its output
block driven from the card's output trim (about 2.5 k) as on the drawing, so
it does its 18 dB of gain and is the stage that runs out first, into the 600
ohm line it was built for — or, by its Output Load parameter, a bridging 10 k
input (a little brighter, +1.6 dB at 20 kHz from the estimated leakage, and
8 dB more swing; the level is matched, `BRIDGING_DB`, and changing it
restarts the processor). Its calibration is measured on the circuit by
`tests/british73_calibration.rs` (upstream's fed the block from 470 k, which
held it at no gain and took 4 dB off at 20 kHz). Gain is the
circuit's gain, as on the hardware: the calibration holds the level at the
middle of its travel (−18 dBFS in, about −20 dBFS out) and the level follows
the circuit from there (`preamp::level_change_db`) — the Tube 610's Level pot
closes to silence, the British 47's switch has its three positions, and the
British 73's lower half of the travel is its gain switch's input divider (up
to 35 dB; the circuit is wired as the 70 dB position, its feedback leg
covering only 10 dB). The bottom fifth of the travel closes the input
gradually (square law: −12 dB at 10 %, −24 dB at 5 %), so 0 % is silence on
every model (the 610's own pot closes to about −80 dBFS; only its last 2 %
finish the way). Master trims after. Each channel
has independent circuit state and uses fixed 2x oversampling with reported
latency. A track's channels are solved side by side (`PreampBank`: the
calling thread and helper threads of a `WorkerPool`, which adopt its
scheduling, take a channel each — a 5.1 track six threads, a 7.1.4 one
twelve — within the physical cores and the user's processing threads,
`faderframe_realtime::{channel_helpers, channel_threads}`, which the
session sets with its own pool; beyond them channels share threads. The
output is what one thread computes).
Live preamps run on a dedicated worker through GainStageFx's generic
reservoir in `faderframe-realtime`, buffering one device callback (at least
128 host samples, `preamp::buffer_delay`; the engine passes the callback
size as `PrepareConfig::device_block` and rebuilds when it changes),
reported alongside the FIR latency for graph compensation: with less, part
of each callback's circuit work would be done while the audio thread
waits. Live, the solver keeps to when the audio is due
(`Preamp::set_realtime_deadline`): past it, a sample that will not settle
stops after a few passes instead of the half-step rescue, so a block of hard
transients (the German 76 at full gain on a drum loop near full scale takes
up to 3.2× real time for a block) cannot drop out. Monitoring stays available: the live signal
necessarily incurs that delay (2.67 ms at 48 kHz and up to 128-frame
callbacks). Offline and already anticipated chains use inline DSP plus the
same delay and the full solve, keeping renders deterministic. Parameter values travel
with each input sample in fixed storage; resets discard the previous epoch.
Worker underruns join the existing xrun counter. Live graphs honor processors'
preferred 128-frame quantum so
large device callbacks enqueue tracks in smaller chunks. Those chunks share
one bounded device deadline, retaining time for downstream work. Gain moves
the circuit control and the level with it (the calibration is fixed at 50 %,
see above). Master is a
smoothed -60 to +12 dB output trim. Calibration is
applied before decimation, synchronously with circuit gain changes. Constructor
work runs off the audio thread; processing, automation, and reset use reserved
storage. Gain switches retain the circuit's discrete positions. Extra hardware
switches stay at their netlist resting values.

What a channel costs. A sample's solve stops after one Newton pass where the
second would only confirm it (`Simulation::set_early_acceptance`, on for the
preamps and buses: the quadratic constant measured on recent two-pass solves
predicts the correction it skips at a hundredth of the tolerance; −117 dB
relative to the signal at worst, test tones' levels and THD within 1e-5 dB
and 0.02 % of themselves). Compiled LU kernels cover the pivot plans the
preamps and buses meet through their Gain at 44.1, 48 and 96 kHz
(`examples/kernels.rs` takes the census and regenerates `kernels.rs`);
before, the Tube 610 at full Level, the British 47 low, the German 76 driven
hard, most valve buses and much at 44.1 kHz fell back to the masked replay,
about three times slower. The Schur recovery and the per-sample RHS
responses skip their exact zeros (bit-identical). Per channel at 48 kHz, CPU
per 128-frame block (2667 µs), mean over the four-tone test at −30, −18 and
−6 dBFS and a drum loop at Gain 25–100 %: British 73 512 → 348 µs, Tube 610
437 → 301, British 47 310 → 222, German 76 616 → 460 (at full Gain on the
tones 580–700 → 400–530), the valve buses 360–730 → 210–430; a drum loop
near full scale at full Gain keeps the German 76 at about 920 µs, where its
plates and grids cross their corners from sample to sample.

As in GainStageFx, exact duplicated stereo input needs only one circuit solve.
On the first differing block, the dormant channel receives the active channel's
solver and FIR history without allocation. Both channels then remain active,
including during silence, until reset. Tests compare this path bit for bit
against always processing two independent channels through gain changes,
stereo transitions, tails, and resets.

### Console summing

`Project::console` (`faderframe_project::console`: family, channel drive,
whether the mixer takes the console's look; `Command::{SetConsole,
SetConsoleDrive, SetConsoleLook}`) runs the mix through a console's
circuits, one family for each preamp: American (a 2520-style virtual earth
into the 2503 output transformer), British 4K (two 5534 stages), British 73
(the 73's class-A line amplifier into its transformer), and the valve desks,
which summed passively and made the loss up with a booster of their own kind
at a fixed gain — Tube 610 (the 610 module, Level at mid), British 47 (the
REDD.47 at 34 dB) and German 76 (the V76 on its 34 dB step, with its 40 Hz
high-pass and 15 kHz low-pass) — all in `circuits::console_bus`
(`control(family)` = the booster's gain position), calibrated so −18 dBFS is
+4 dBu at the line (the small-signal gain measured at a millivolt). The bus
circuits cost 2–6 % of a core per channel (solid state) and 11–21 % (valves,
the German 76 the most) on a hot signal.

* **Buses, returns and the master** sum through the circuit itself: a
  built-in device in their input stage (`builtin::CONSOLE_BUSES`, preamp
  model `MODELS + family` through the preamps' `BufferedPreampProcessor` —
  reservoir buffering, worker pool, deadlines — its latency compensated).
  Its Gain is the drive in dB (±12), taken off after the circuit so the
  colour changes, not the level; Output trims. `Session::set_console`
  places, replaces (keeping drive and output) or removes them in the same
  undo step as the setting; a bus whose input stage holds a microphone
  preamp keeps it; buses added while a console is set get theirs
  (`console_buses_for_new_tracks`, a hook in `Session::edit`). Each bus's
  amplifier is its own (`Action::SetBusAmplifier`): following the console,
  through another console's (hybrid summing) or in the box; a console
  switch moves only the followers, the master's is always the console's.
  In the mixer the master's input slot chooses the console, a bus's slot
  its amplifier (follow, another console's, drive, bypass, in the box, a
  microphone preamp's colour instead).
* **Audio and instrument channels** go through the console's line
  amplifier as a light model (`faderframe_circuit::console::ConsoleStage`,
  0.3 % of a core per channel against 3.5–6.3 % for the circuits), in
  front of the strip (`nodes::ConsoleChannel` in `add_strip`, so whichever
  graph builds a strip builds its stage; pre-fader sends carry it). The
  channel drive is a parameter slot (`SlotRegistry::console_drive`, no
  rebuild), ramped across a block.

The model is baked from the bus circuits (`tests/console_bake.rs`, run with
`FADERFRAME_BAKE_CONSOLE=1 … --ignored`): the small-signal response as a
high-pass (second order and a further first), low-pass (first order or an
LC's second, matched to the analog magnitude up to Nyquist, its corner and
Q nudged on the digital stage at 48 kHz), bell and shelf, the stage's 5 Hz
DC blocker allowed for; transfer curves at 25 levels from −24
to +12 dBFS, each relative to its level (an AC-coupled class-A stage moves
its operating point so it clips near the peak at any level; relative to the
level, neighbouring curves keep their knees together, so mixing them moves
the knee instead of making two), chosen by a peak follower with a 25 ms
hold; and per-level emphasis bands (a low and a high one-pole band, the
gain into the curve undone exactly after it, then a correction) where the
circuit's distortion and compression depend on the frequency differently
at different levels, their corners each family's own (chosen from nine
pairs against checks at 60 Hz, 200 Hz, 1 kHz and 5 kHz). `tests/console_match.rs` holds it to the circuit at
60 Hz, 1 kHz and 5 kHz from the nominal level to +6 dBFS (distortion within
35 % or within what the circuit has a quarter of a dB either side, gain and
response within 0.3 dB, distortion rising with the level; the gain not
judged past 20 % THD). The valve families' feedback makes their distortion
depend on the frequency more than the model follows: within a factor of
2.5 or 0.1 %, and their response within 0.4 dB at 20 kHz. Not modelled: the
British 73's slew distortion at 5 kHz where its curve is still straight
(about −60 dB).

The mixer takes the family's look (`Theme::with_console_family`: panels,
knobs, faders, meters and finish of the console section, in FaderFrame's
own design — graphite and brass, slate with colour-coded caps, pewter and
teak, taupe and Bakelite with mahogany, hammertone with oak, light enamel
grey with beech; a skin with wooden cheeks keeps them in the family's wood)
unless the look is turned off; Audio → Console and the master
strip's menu switch it all. Tests: `session/tests/console.rs` (one step,
latency to the sample, level kept, colour when driven, families keep the
bus settings), `the_console_does_not_allocate`, the mixer's
`a_console_shows_its_bus_amplifiers_and_its_look`.

### Gain-reduction meters

Every strip with a compressor shows its gain reduction in a stripe left of
the meter (on the meter's scale, from the top down; several in series
added). Any `PluginInstance` may report it through `reduction()` — a
`plugin_host::Reduction` cell (an atomic f32 in dB; NaN = not reported) its
processor sets after each block on the audio thread:

* built-ins: the compressors' published value (`devices::reduction_value`:
  Compressor, 76, Limiter, De-esser, Channel Strip), copied by the
  `Modulated` wrapper;
* CLAP: the draft `clap.gain-adjustment-metering/0` extension
  (`host::PluginGainAdjustment`, its `get` called after `process`);
* VST3: a read-only parameter named as gain reduction
  (`plugin_host::names_gain_reduction`), caught in the block's output
  parameter changes and converted by a `ReductionTable` measured from the
  controller (dB unit, else the dB its text shows, else a 0…1 linear gain);
* LV2: output control ports named so (`scan::Lv2Plugin::reduction_ports`,
  `ReductionScale::{Db, Gain}` from `units:db`/`units:coef`/the range);
* AU: a read-only or meter-flagged parameter named so, read with
  `AudioUnitGetParameter` after render. Apple's Dynamics Processor also
  gets a direct probe of its documented `CompressionAmount` parameter
  (global id 1000, dB), retried after initialization: discovery does not
  require this built-in meter's parameter metadata. The probe is specific
  to `aufx:dcmp:appl`; third-party ids come from their parameter lists.
  Activation clears old readings, and failed or non-finite reads mark
  the meter unreported;
* sandboxed plugins: the helper writes the reduction into the shared
  memory's `Header::reduction` each block and the proxy hands it on (shm
  version 5).

`Session::{gain_reduction, gain_reduction_devices}` sum the devices of a
track's input stage and inserts that report (bypassed ones not).

### MIDI event list

`faderframe-view-events` (`ViewKind::Events`, View → MIDI Event List, a MIDI
clip's menu → Edit in Event List = `Action::OpenEventList`) lists the edited
MIDI clip (else the first selected one) a row per event, every property a
column: notes (position, channel, key, velocity, length, end, release
velocity, mute), control changes, program changes (shown 1–128 with their
General MIDI name, `faderframe_midi::gm`), pitch bend, channel pressure,
polyphonic key pressure, the points of the notes' expression curves, and
SysEx (its bytes). Filters by kind. A double-click types a field (positions
in the main counter's unit, keys by name or number, lengths as beats.ticks
or a note value like `1/8`, expression values in their unit, SysEx in hex);
a vertical drag on a number previews and commits once on release; M mutes;
"+ Add" and the rows' menus add any kind at a row or the playhead, set the
channel or release velocity of the selected rows, mute notes; Delete
removes. Note rows share the session's note selection with the piano roll.
The operations are `session::midi_events` (`midi_events`, `Action::{
EditMidiEvents, RemoveMidiEvents, AddMidiEvent}`): a controller value moved
to another channel, number or key changes lanes and emptied lanes go. A
clip's channel as a whole: `Action::SetMidiChannel` (the arranger's MIDI
Channel submenu: notes and controller lanes); the piano roll's Channel
submenu sets the selected notes'.

Nothing MIDI carries is dropped: program changes and polyphonic key
pressure are controller lanes (`MidiController::{Program, PolyPressure {
key }}`), a note's release velocity is `MidiNote::release` (`None` sends
0). MIDI file import and export, recording (`MidiTake`), Capture MIDI and
playback carry them; the clip player chases programs on locate after the
CCs, so a bank select goes first.

### Spectral editing

`faderframe_project::spectral` keeps a clip's edits in its source's own
frames and hertz (`AudioClip::spectral: Option<Box<SpectralEdits>>` — keep
it `None` in new `AudioClip` literals; `SpectralEdits::original` is the
unedited source): shapes `Rect` (a time range has `low` ≤ 1 Hz and `high`
at Nyquist, a band spans the clip), `Lasso`, `Brush`; ops `Gain`, `Remove`,
`Attenuate`, `Heal`, `Denoise { reduction_db }`; a feather in ms and
semitones; one channel or all.

`faderframe-spectral` (pure; realfft) does the DSP: `apply` runs each edit
as its own STFT pass over the span it reaches, frames as long as fit the
edit (`frame_for`: up to `frame_size` ≈ 85 ms, down to 256 samples, so a
click is not smeared), Hann, a quarter-frame hop, an identity frame (`w²x`)
wherever the mask is empty, and copies everything else bit for bit. Masks
(`mask::Mask`) weigh bins in (frames, octaves) with raised-cosine
feathers. Attenuate caps each bin at the level around the edit in time
(mean magnitudes of 8 frames before and after, interpolated in dB); Heal
replaces it by that level, each bin's phase carried on from the frames
before (measured phase advance), so tones continue through a dropout.
`Spectrogram::compute` makes the editor's picture (log rows from 20 Hz,
loudest bin per row, mean power over channels, bytes from −120 dB).
Denoise ("Learn Noise and Remove", `faderframe_spectral::denoise`) is the
De-noise device's DSP offline: the noise is learned per channel and bin
from the shape's stretch of the source as recorded (frames whose centres
fall in it; one frame round it when it is shorter), and taken out of the
whole source in the shape's band (with the frequency feather) at the
Balanced frame size. A source with a Denoise is processed in chunks of
20 s (`apply_in_chunks`), each read with room either side — 4 s of
run-in for the gains and every other edit's reach and surroundings — and
every edit run over it in order; only the chunk is written, so a long clip
never sits in memory whole and the joins do not show.

`session::spectral`: changes (`Action::EditSpectral { audio, change:
SpectralChange::{Add, Set, Remove, Clear} }`, on an `AudioRef` — a clip,
or one take of a folder; `Session::spectral_target` is the one shown) are
pending at once
(`spectral_edits`), render after `SETTLE` in a worker from the unedited
source into a float WAV beside the media, then `AddSource` + `SetClipContent`
as one "Spectral Edit" step (overtaken renders dropped); with clip effects
the processed copy becomes the effects' original and they render again
(`rerender_clip_fx`); no edits left = the original back. The Pencil redraws
the unedited source too. Pictures: `Session::spectrogram(PictureKey)` asks
a worker (newest request wins) and returns the closest picture of that
source meanwhile. View `faderframe-view-spectral` (`ViewKind::Spectral`,
`Action::OpenSpectralEditor`, the audio clip menu); it repaints until the
picture it asked for has come (`waiting`), since a result can land between
two frames.

### Snapping to samples and zero crossings

Grid positions stay musical (a grid line at 112 BPM is not a whole sample,
and rounding it would pull it off the grid), so the session never rounds
moves or trims. The arranger rounds the positions it makes freely — snap
off, Alt, Slip, the relative grid — with `Session::on_sample` when Snap to
Samples is on (`EditFlag::SnapToSamples`, the default). Snap to Zero
Crossings (`EditFlag::SnapToZeroCrossings`) moves the cuts of Separate,
split at the playhead and Trim to Selection, and audio trims, to the
nearest zero crossing of the clip's audio (channels summed, within 5 ms;
`Session::cut_at`), per clip.

### Snap to Objects

`EditFlag::SnapToObjects` (`EditorSettings::snap_objects`, on by default,
remembered in `Preferences::snap_objects`; Edit → Snap to Objects, the edit
toolbar's *Objects*, Preferences → Editing) makes drags snap to **objects**:
clip starts and ends on the same and every other shown track (closed
folders hide theirs; launcher clips are in no track's list), markers,
section bounds, the loop and edit-selection bounds, the Play Start Marker
and the playhead while stopped. `session::snapping` finds them
(`Session::snap_to_object`, `snap_edges_to_object` → `SnapHit { at,
object: SnapObject, track }`; `snap_edges_to_objects_and` takes an
editor's own objects too); the arranger applies the rules (`clip_edit.rs`:
`SN0.97 (grand staff)X` = `snapping::SN0.97 (grand staff)X`, `object_shift`, `snap_point`,
`moved_edges`). Besides clips, ranges, markers, sections and the loop,
these drags snap the same way: the Play Start Marker's flag, automation
points (a point added and dragged, not a freehand stroke), tempo points
moved sideways, chords (a new chord's end, a moved chord by either edge, a
resized edge). Key changes stay on bar lines (a key starts a bar), so they
take no objects. The piano roll snaps a moved or resized note's edge (a
moved group by its leading or trailing edge or the grabbed note's) to the
other notes' starts and ends (`SnapObject::{NoteStart, NoteEnd}`) and the
arranger's objects, before the grid; Shift moves freely there
(`PianoRollView::note_object_shift`, its own guide line):

* **Alt bypasses all snapping**, objects and grid alike.
* **An object within `SN0.97 (grand staff)X` (8 px) beats the grid**; the threshold is
  pixels turned into time at the current zoom (`snap_reach`), so it feels
  the same at every zoom. The nearest object wins; at equal distance the
  first found (tracks in order, then markers, sections, loop, selection,
  playhead).
* **Otherwise the edit mode decides as before**: Grid snaps to the grid
  (absolute, or relative steps), Slip and the others move freely (whole
  samples with Snap to Samples). So in Slip only objects attract.
* **What is edited is never a target** (`SnapSkip`): the moved or trimmed
  clips, the dragged marker or section, the selection while it is drawn,
  the loop while it is dragged — nor what a ripple drag moves along with
  them (see "Ripple editing").
* **Which edge snaps**: a moved group by its leading or trailing edge or the
  grabbed clip's start or end — whichever is nearest an object, all moving
  by the same shift; a trim by the trimmed edge (audio ends then land on
  the whole sample nearest the object); a range by its moving edge (and its
  anchor at the press); a section body by either bound; markers, section
  bounds and loop edges by themselves.
* **Shuffle moves are not snapped** (clips butt where they land) and Spot
  asks for the position.
* The guide shows on the lanes for ruler drags too (the loop, the Play
  Start Marker).

While a drag is snapped, `ArrangerView::snap_guide` holds the `SnapHit`: a
line across the lanes in `ArrangerTheme::snap_guide` with the object's
name (`SnapObject::label`) on a chip; it goes with the release. Tests:
`session/tests/snapping.rs` (`objects_within_reach_…`,
`a_group_snaps_by_…`, `clips_in_a_closed_folder_…`), arranger
`snap_tests.rs` (`the_play_start_marker_drags_in_the_ruler_and_snaps`,
`automation_points_tempo_points_and_chords_snap_to_objects`), piano roll
`notes_snap_to_other_notes_and_objects`.

### Ripple editing

When an edit changes how much time something takes, a ripple moves what
follows it — on the tracks the edit is on, on the tracks chosen with them,
or across the whole project (`faderframe_session::ripple`, the pure parts
in `faderframe_project::ripple` and `faderframe_automation::ripple`).

* **When**: Shuffle mode always ripples (its moves reorder clips, as in Pro
  Tools); `RippleSettings::on` makes Grid, Slip and Spot ripple too —
  snapping as their mode does — with moves pushing what follows them
  (`Session::ripples`). `RippleOp::Toggle` (Ctrl+Alt+R, the edit toolbar's
  *Ripple*, Edit → Ripple) switches it; off leaves Shuffle for Slip.
* **Where** (`RippleScope`, the toolbar's *Track ▾ / Selected ▾ / All ▾*,
  `app.ripple-scope`): *Edited Tracks* — track-selective, only the tracks
  the edit is on; *Selected Tracks* — those and every selected track (and
  what folder groups add), together; *All Tracks* — global, every track and
  the timeline's parts the settings choose (`RippleSettings::{markers,
  tempo, harmony}`: markers, sections, loop and punch ranges and the Play
  Start Marker; tempo and metre; key, chords and lyrics). Video stays: the
  picture is what sound is edited against. A track left out
  (`Track::ripple_excluded`, `Command::SetTrackRippleExcluded`, the track
  menu's *Leave Out of Ripples*, the Inspector's *Ripple* line, its header
  says "no ripple") is never moved by edits elsewhere; its own edits still
  ripple. Frozen tracks are left out with a notice (their audio is fixed;
  once a gesture). Folders hold nothing and are skipped.
* **The model**: a ripple is a list of `RippleShift { cut, delta }` — at or
  after `cut` everything moves by `delta`; a negative one takes
  `[cut + delta, cut)` out — in the coordinates before any of them,
  applied from the latest cut back (`RippleShift::ordered`). The tracks the
  edit is on are *moved* (`ClipRipple::Moved`: clips at or after a cut move,
  into a span taken out onto its start, none is cut — a crossfaded
  neighbour is not trimmed; the edit places its own clips); the tracks kept
  in sync are *cut* (`ClipRipple::Cut`: their time opens or closes at the
  same place, a clip across a cut is split, what lies in a span taken out
  goes). Positions are worked out with the old tempo map; a changed
  timeline is the batch's last command. With several edited clips each
  moves what follows it on its own track and the tracks kept in sync follow
  the first of them.
* **Automation** is its own entity on every track that ripples, moved by
  the same shifts at the same cuts (`LaneWork::ripple` →
  `AutomationCurve::ripple`): opening time duplicates a point on the cut
  (one copy stays, one moves) or gives a cut segment two points at its
  value there; the gap holds that value (of a jump on the cut the first
  point's: the jump moves with what follows). Taking time out keeps the
  value reached at the span's start and jumps there to the value the curve
  had at its end. Linear, step and exponential pieces keep their shape
  exactly; a cut smooth segment is redrawn as straight pieces within
  `SMOOTH_TOLERANCE` (0.1 % of its rise). Binary search and `split_off`:
  O(log n + k) for k points after the cut. Shuffle reorders carry the
  moved clip's stretch of automation along (`range_automation`,
  `LaneWork::write_range`); clip moves without rippling carry none.
* **The timeline's parts** (`ripple::global_commands`): markers move (into
  a span taken out: onto its start); sections, the loop and punch map as
  ranges (one across an opened cut grows, one inside a span taken out
  goes); key changes move (the last one inside a span taken out carries on
  from its start); chords and lyrics as ranges (lines starting in a span
  taken out go); tempo: the tempo at the cut holds over an opened gap, a
  ramp across a cut keeps its slope on both sides (`rippled_tempo`; when
  taking out, a tick short of the seam: tempo points cannot share a place);
  signatures move by whole bars from a bar line only.
* **What ripples**: trims and time-stretch trims of the end keep the clip's
  start and move what follows by the change of length (a start trim at the
  start: what was trimmed off its head); moves push what follows the moved
  clips on their tracks (clips they pass over stay; `push_commands`);
  nudges and Spot push; Delete and Cut of clips close the time they took
  (Edited: on each clip's track; Selected/All: the time of all of them);
  Clear and Cut of a range close it; paste, duplicate, repeat and Insert
  Silence open room; Trim to Selection closes up both ends (take folders
  are not trimmed). Explicit commands ripple whatever the mode
  (`Action::Ripple(RippleOp::…)`, Edit → Ripple, `app.ripple-edit::<op>`):
  `DeleteClips(scope)`, `DeleteRange(scope)` (Shift+Delete in the
  arranger: the range, else the clips, as far as the settings reach;
  Ctrl+Shift+Delete in all tracks), `InsertTime(scope)`,
  `RemoveGaps(scope)` (Edited: between the selected clips on their tracks;
  All: the time no track that ripples has a clip in, between the selected
  clips' ends or over the song) and `Offset { cut, delta, track }` —
  `Session::apply_ripple_offset(cut_time, delta_t, track_id)`: one track
  (cut) or, `None`, every track with the timeline.
* **Drags stay lossless**: a ripple splits clips and cuts curves on the
  way, which the next motion could not take back from where the last one
  left things. `History::mark` marks the open gesture at its first ripple
  motion and `History::rewind` undoes what it did since (a mark makes
  coalescing count only keys recorded after it, so a setter done before is
  recorded again); every motion of `move_clips`, `trim_clips`, `trim_clip`
  and `stretch_clip` rewinds (`Session::rewind_ripple`) and is worked out
  afresh from the material as it was; `ripple_batch` tells the engine about
  the rewind with the edit (or on its own when that changes less). Dragging
  back gives the project back exactly; the drag is one undo step. What the
  drag moves along (`RippleGesture::moved`: clips, and with All Tracks the
  markers, sections, loop and Play Start Marker after the edit) is no snap
  target while it moves.
* **Seeing it**: while a ripple drag runs, the arranger tints the rows that
  move from the edit on and marks the cut (All Tracks: a line through the
  whole view; `ArrangerView::ripple_drag`); its corner reads "RIPPLE
  TRACK/SELECTED/ALL" while editing ripples; the edit toolbar shows the
  switch and the scope; Edit → Ripple has the switch, the scope (radio),
  the three "follow" checks, the commands and *Leave Selected Tracks Out of
  Ripples* (`toggles.rs` keeps the checks). The scope and the follow
  settings are remembered (`Preferences::ripple`); the switch is not —
  editing starts without rippling, as it starts in Grid mode.

Tests: `faderframe-automation` `ripple::tests` (every shape both ways,
points on the cut, jumps, the curve's ends, before zero, open then close),
`faderframe-project` `ripple::tests` and `history::rewind_tests`,
`session/tests/ripple.rs` (one track or everything, automation in sync,
the scopes and the tracks cut, ripple deletes per track and everywhere,
the timeline as chosen, left-out and frozen tracks, a lossless drag that is
one undo step, pushes, duplicates, gaps, Trim to Selection, Insert Time,
Shuffle carrying automation), arranger `ripple_tests.rs` and
`edit_bar::tests::ripple_button_and_menu`, the Inspector's
`a_track_can_be_left_out_of_ripples`, `toggles::tests::
ripple_items_follow_the_session`.

### The drawn grid: every zoom, one strength

`faderframe_timeline::for_each_visible_line` (`visible_lines`,
`GridDensity { min_px: 8, fine_px: 18 }`) decides what the arranger and the
piano roll draw from the **edit grid** (`EditorSettings::grid`, so triplet
and dotted values show as they snap), the meter map and the zoom: bars
(every 2ⁿth when closer than `min_px`, `bar_stride`), beats where they are
`min_px` apart, the grid's lines — exactly `snap_nearest`'s positions —
where they are, else every 2ⁿth of them while finer than a beat, and
`GridLineKind::Fine` lines that halve the finest cells drawn (the grid's,
or the beats' when the grid is coarser) down to about `fine_px` once those
cells are twice as wide. Nothing snaps to fine lines; they keep the grid
visible to single samples (the old arranger drew a fixed 1/16 at most, so
from about 2 000 px a quarter no line was in view, and its grid ignored the
chosen value). Each position comes once with its strongest kind (bar >
beat > subdivision > fine). Zoomed in the ruler labels beats, grid lines
and fine lines too (`ruler_labels`: by level, at least 76 px apart;
"bar.beat.tick" in 960ths, decimals below a tick). Tests
`drawn_grid_lines_are_the_snap_positions`,
`the_grid_shows_at_every_zoom_without_flooding` (timeline),
`the_grid_shows_at_every_zoom_and_sits_where_clips_snap` (arranger).

How strongly lines are drawn is `Theme::grid: GridLook` — a preference laid
over every skin (`Preferences::grid_strength` 0…1, default 0.5; slider
Preferences → Editing → Grid lines; `AppState::set_grid_strength` hands the
theme to every canvas with `CanvasWidget::restyle`, a repaint without a new
layout; dev action `grid-strength:<0…1>`). The skins give the colours at
the default (`ArrangerTheme::{bar,beat,sub,fine}_line` from `GRID_ALPHAS`
0.19/0.11/0.075/0.055 — bars, beats and subdivisions 1.9×, 2.4× and 3.4×
the old values — the piano roll's from `PIANO_GRID_ALPHAS`, scaled by each
skin's line strength);
`GridLook::gain` scales opacity ⅓× … 3×. Lines are whole device pixels on
device-pixel edges (`GridLook::line_rect`): hairlines for subdivisions and
fine lines, a logical pixel for bars and beats (two device pixels at 2×),
bars one device pixel wider from strength 0.8.

### Meter styles and the VU meter bridge

The engine's `MeterBank` keeps per channel the sample peak, the loudest
block's mean square, the **energy and frames** since the last read (a
compare-and-swap add, so the session gets the true RMS of every UI frame)
and, for channels a PPM shows (`MeterBank::set_ppm`, switched per track by
the session's tick through `EngineController::set_meter_ppm`), a
sample-accurate **EBU quasi-peak** envelope (`PPM_ATTACK_S` 3.3 ms: a 10 ms
burst reads −2 dB; 24 dB fall in 2.8 s). The session's `MeterChannel` turns
that into the peak with its hold, an RMS integrated on the power (300 ms),
a **VU needle** (second order, 99 % in 300 ms, 1.5 % overshoot, run in
linear volts so the 0 VU reference only scales it: `vu_db(reference)`) and
the PPM level.

What a strip's meter shows is presentation (`WorkspaceSet::{meter_mode,
meter_modes, vu_reference, meter_bridge}`, `MeterMode::{Peak, PeakRms, Vu,
Ppm, K20, K14, K12}`; `Action::{SetMeterMode, SetVuReference,
SetMeterBridge}`), chosen from the meter's right-click menu or the strip's
Meter submenu, for one strip or every one. Every style is drawn in the
skin's own look (LED ladder, bar, plasma; edgewise needles only in a skin
whose meters are edgewise) with its standard's colours and scale
(`controls::MeterZones::{Digital, Ppm, K(zero), Vu(reference)}` —
`position` places a level, a VU on the voltage scale); with an RMS the lit
bar is the RMS and the peak one LED (or line) above it. The mixer's meter
bridge (`BRIDGE_H` over the strips; `paint`/`event`/`tooltip` shift the
strips below it) shows a moving-coil VU (`controls::vu_arc`) per audio
strip and the master, two for a wide stereo strip.

### Song structure from a jam

`faderframe_analysis::structure::analyse(mono, rate, quarters_per_bar)`
cuts a recording into bars by its tempo (`tempo::detect`; the downbeat is
the beat phase whose bar lines fall where the harmony changes most; two
seconds a unit without a beat), describes each bar by chroma, the shape of
20 log band energies and loudness, and finds part boundaries where the
mean features of the four bars before and the four after differ most
(means, so a chord cycle inside a verse is not change; z-scored, loudness
changes added, four-bar phrases preferred, no part under three bars at
either end; silence is a gap). Parts are compared along the bar
self-similarity matrix's diagonals; alike ones share a letter, named by
repetition, loudness and place (the loudest repeated letter Chorus, the
next Verse, one only opening and closing Intro/Outro, a lone middle part
Bridge or Solo). `session::structure` (`Action::SongStructure { clips }`,
the audio clip menu, dev action `song-structure:<track>[|…]`) mixes the
clips to mono in project time, analyses in a thread and lays the parts on
the section lane in one "Song Structure" step (replacing the sections they
overlap, coloured by letter, snapped to the project's bars when it plays at
the jam's tempo).

### Setlist and show mode

`Project::setlist` (`faderframe_project::setlist`: `SetSong { name, start,
end, then: AfterSong::{Stop, Next { gap }, Continue}, notes }`,
`Command::SetSetlist`) holds a show: one project, each song a range of it.
`session::setlist`: `Action::Setlist(SetlistOp)` edits it (songs from the
sections, from the edit selection, move, rename, then, notes; one undo step
each); `Action::Show(ShowOp::{Enter, Leave, Cue, Go, Next, Previous,
PlayStop})` runs it (not saved). While a song plays the session asks the
engine to stop on its exact last frame (`EngineController::stop_at`: the
callback is cut there and the transport stopped, once); stopped there, the
show stands on the next song and, for `Next`, plays it after the gap
(`tick_show`); `Continue` plays on (a song elsewhere is jumped to). MIDI
learn targets `MappingTarget::Show { control: ShowControl::{Next,
Previous, PlayStop} }` (foot switches). View `faderframe-view-setlist`
(`ViewKind::Setlist`): the list, and in show mode the stage screen (full
screen via `UiRequest::FullScreen`): song, time and what is left, the part
now and next, the lyric line, notes, the list, big buttons; Space, arrows,
Enter, Esc.

**Gear changes.** A song can change external gear (`SetSong::gear`:
`SongGear { changes, timing }`, a `GearChange` = an output port, a wait in
milliseconds before it, and a `GearMessage::{Program { channel, program,
bank }, Control, SysEx { name, data }}` — a program after its bank select,
a controller snapshot as several controls, patch dumps). They go out when
the show stands on the song (`GearTiming::Cue`, the default: cued, before
it plays, so a synth has loaded its patch by the time the song is played),
or on its first frame (`GearTiming::Start`); a song played into straight on
from the one before has no moment to stand on it and gets them on its first
frame too. `session::show_gear` sends them through `MidiOutputs` on
schedules of their own (`Schedule::{ShowCue, ShowStart}`, each cancelled
alone; clip SysEx is `Schedule::Clips`), each message after the one before
it has gone down a MIDI cable (`WIRE_NS_PER_BYTE`, 320 µs a byte) and after
its change's wait (`SongGear::schedule`). Standing on another song cancels
what has not gone out; standing on the same song again (stopped back on its
start) sends nothing new until the setlist is edited or the show entered
again (`GearPlayback::cued`); first-frame changes are scheduled ahead like clip
SysEx (`tick_show_gear` from the MIDI tick: due when the first frame is
heard — the engine's position at the callback plus the output latency — or
at once when a start from a stop has just passed it), and a locate or stop
cancels them. Edits are `SetlistOp::{Gear, GearText, GearTiming,
GearProgram, GearSysexFile, GearSnapshot}` (one undo step each; text in
`SongGear::to_text`'s form — `pc 5 bank 1 ch 1`, `cc 7=100 ch 2`, `sysex F0
… F7`, `wait 200`, `on "<output>"`; outputs not named go to
`Session::default_gear_output`; a snapshot takes the controllers a track's
clips set by the song's start), `ShowOp::SendGear` sends them now. The
setlist view's Gear column shows them (▸ for first-frame timing), a
double-click types them, a click (or the row menu's External Gear) lists
them with the timing, Send Now, Add Patch Dump (`UiRequest::ImportGearSysex`
→ a file dialog), Add Controller Snapshot of a track, Clear. Dev action
`show-gear:<song n>=<program>[/<bank>]`. Tests `setlist::tests` (project),
`session/tests/show_gear.rs` (cue sends spaced as a cable carries them,
first-frame and straight-on timing, a jump cancelling, edits), the
setlist view's `the_gear_column_types_gear_changes_and_its_menu_does_the_rest`.

### Adaptive music for games (the interactive score)

`Project::interactive` (`faderframe_project::interactive`,
`Command::SetInteractiveScore`, Impact None; the session wraps it in a
batch named after the edit) holds music for a game as middleware plays it.
*States* are song sections (they loop between an entry cue — the bars
before it a pickup played on the way in — and an exit cue, or play once
and go on to `then`) or launcher scenes (their clips loop as the launcher
plays them; the scene's length is its longest clip as played). *Layers*
are groups of tracks; *parameters* (intensity …) move them through
`LayerCurve`s (points of value → dB, the amplitude interpolated between
them, `FLOOR_DB` silent). *Rules* match a source and a destination (either
may be any state): the most specific wins, the first of equals, and
without one the music moves on the next bar (`rule_between`). A rule says
where the source is left (`SyncPoint`: at once, the next beat, bar or grid
of bars counted from the state's start, project marker, one of the state's
named cues, its exit), the fades out and in (`FadeLength`, milliseconds or
beats), a transition segment (a section state played once on the way) and
where the destination begins (its start, its entry cue, the same place in
its loop). *Stingers* are launcher clips played once over the music on a
sync point, with a pickup before it. Sync points are pure: `sync_point`
walks the stretches playback goes through from a position — a looping
state's loop again and again, a scene repeating from its launch — and
takes the first point at least a wait away, beats and bars from the tempo
and metre in force there. Ids are allocated by the session; references to
gone states are dropped by `tidy`, states whose section or scene is gone
play nothing and are left out of exports.

**The preview** (`faderframe_session::adaptive`, `Action::Adaptive(AdaptiveOp)`,
not saved) plays one state at a time on the live engine. A section state
is the transport looping its loop — the session *holds* the loop
(`EngineController::hold_loop`: the project's loop is not sent while it
holds); a scene state is the launcher playing its row. Asked for another
state, the session finds the rule and its sync point in the state playing,
at least `cue_margin` ahead (the render-ahead lookahead, two of the tiers'
blocks, a callback, 60 ms) plus the fade-out, and plans the move there:

* **Planned jumps** (`faderframe_transport::{Cue, Cues}`,
  `TransportCommand::SetCues`, `EngineController::set_cues`): when
  playback reaches `at` it goes on from `to` with the loop `then_loop`
  says, on that frame — `frames_until_wrap` ends a block there, a cue on a
  loop's end wins over the wrap, `to == at` only changes the loop. Up to
  `MAX_CUES` (2: into a transition segment and on to the destination);
  only the first is armed; a locate, stop or scrub forgets them; they are
  not jumps (`jumps` counts locates) but count in `cues_taken`
  (`TransportSnapshot::{cues_taken, cues_waiting}`). Rendering ahead, a
  plan is a loop change: `AheadLink` hands the loop and the cues over
  together as a `LoopPlan` amend at a frame beyond every anticipator, so
  the tiers and the audio thread take the same jump on the same frame
  (`amend_late_is_equal` also refuses a late amend whose cue falls in
  what was rendered past its frame). A plan whose first jump lies within
  what was rendered is not taken (the session plans again).
* **The launcher on a sample**: `Quantize::At(sample)` (at once if past)
  and `LaunchCommand::Arrangement { quantize }` (every launched track back
  to the arrangement there; the slot number `launch::ARRANGEMENT` stands
  for it, `SlotKey::hash` stays below it). A scene comes in by launching
  its clips (and stopping the score's other scene tracks) at the sync
  point; leaving a scene for a section returns to the arrangement there,
  beside the jump. A launch waiting for a sample is shifted with a jump
  like any launch (`LaunchState::transport`), so both land together.
* **Fades** (`engine::transition`: `TransitionFade { at, out_frames,
  in_frames, cue }`, `EngineController::transition_fade`, `Fading` on the
  audio thread): the frame of the transition is predicted while playback
  runs straight to `at` and fixed once the cue was taken (or the position
  passed); the outputs heard the graph's latency later are faded down to
  it and up from it. One playhead: the fade-out ends where the destination
  begins (middleware overlaps them; the exports carry the times). A jump
  that does not come cancels its fade.

The tick (`tick_adaptive`) follows the cues taken (the state playing, a
segment on the way), plans again when the jumps went missing (a locate in
between), sends a one-shot state on to its `then` at its exit (or stops
there with `stop_at`), and holds the loop and the layers' gains again
after a new engine. Asked for the state playing while a move waits, the
move is called off (scene clips relaunched in place with legato).
Parameters move the layers' tracks through gains beside their faders
(`EngineController::set_track_trims` → `SlotRegistry::set_trims`, folded
into the strips' VCA gain by `write_params`; never in the project).
Stingers launch on their sync point less the pickup and are released a
clip's length later (`LaunchCommand::Release` at a sample).

**Export** (`adaptive::export`, `AdaptiveOp::Export(dir)`, a thread like a
render, the project as `render_copy` makes it): a stem per state and layer
(a layer = its tracks soloed in place, so the buses and returns they feed
come along; the tracks in no layer are "Base"; without layers one "Mix"),
rendered through `render_one` (the graph's latency off the front: frame k
of a file is the state's frame k) from the state's start to its end plus
`ScoreExport::tail_seconds`, with the loop (entry to exit) in a `smpl`
chunk and Entry, Exit, the named cues and the project markers as `cue `
markers labelled in `LIST`/`adtl` (`faderframe_audio_files::markers::
WavMarkers`, written after the audio by `write_wav_chunks`). A scene state
renders a project where its clips (as played) are placed from the song's
start, repeated to its length and the last cut there, the arrangement's
own clips left out (`scene_project`); a stinger is its clip once on its
own track. Then the metadata (`adaptive::formats`):

* `<name>.score.json` (schema `faderframe.interactive-score/1`): states
  (kind, source, lengths, entry/exit/loop frames, tempo, metre, tempo
  changes, every beat with its bar, cues, markers, stems), layers and
  their tracks, parameters with their curves, the rules as written, the
  transition every pair of states takes, stingers, the other files.
* `<name>.fmod.js`, for FMOD Studio 2.02 and later through its documented
  Scripting API only (`workspace.addEvent`, `Event.addGameParameter`,
  `addGroupTrack`, `addMarkerTrack`, `GroupTrack.addSound`,
  `MarkerTrack.addNamedMarker/addRegion/addTransitionRegion/
  addTransitionMarker`, `Triggerable.addParameterCondition`,
  `AutomatableObject.addAutomator`, `Automator.addAutomationCurve`,
  `AutomationCurve.addAutomationPoint`, `project.importAudioFile`,
  `studio.menu.addMenuItem`): in the project's Scripts folder it adds
  Scripts › FaderFrame › Import Interactive Score, which makes one event —
  a labelled parameter with a label per state, the score's parameters, a
  group track per layer with the states' stems one after another, a
  destination marker at each state's start and entry, a loop region over
  each loop, a transition region (beat, bar, grid, immediate) or
  transition markers (markers, cues, the exit) per pair with a condition
  on the state parameter (segments: to the segment, and from its end on),
  the layers' volume automated by the parameters (sampled amplitude
  curves, −80 dB floor), an event per stinger. The reference documents no
  property for a transition region's quantisation, so it keeps FMOD's own
  (the manifest names the sync point).
* `<name>.wwise.tsv`: Wwise's tab-delimited Audio File Importer list
  (`Audio File`, `Object Path` with `<Music Switch Container>`/
  `<Music Playlist Container>`/`<Music Segment>`/`<Music Track>` type
  prefixes, `Object Type`, `Notes`).
* `<name>.waapi.json` + `<name>.waapi.py` (Audiokinetic's `waapi-client`,
  not part of FaderFrame): `ak.wwise.core.object.set` calls for Wwise
  2022.1 and later, written against the 2024.1 object reference — a State
  Group with a State per state, Game Parameters, Triggers; the Music
  Switch Container (`@Arguments` the state group, `@Entries` per state)
  with a Music Playlist Container per state (`@LoopCount` 0 or 1) and its
  Music Segment (`@Tempo`, `@TimeSignature…`, `@Cues` entry/exit/custom,
  the stems imported as Music Tracks); the layers' Volume RTPCs
  (`@RTPC` with an in-place `Curve`); the transition list
  (`@TransitionRoot`: `MusicTransition`s with source/destination context,
  `@ExitSourceAt` 0/1/2/3/4/5/7, `@DestinationJumpPositionPreset`,
  `@PlayDestinationPreEntry`; least specific first since Wwise reads the
  list from the bottom up) and `@Stingers`. Fades and transition segments
  are written into each transition's notes: their objects are not among
  WAAPI's documented properties.

The view `faderframe-view-adaptive` (`ViewKind::InteractiveMusic`,
`app.show-interactive-music`, View → Interactive Music) is the control
panel: Preview/Stop, + State/Rule/Layer/Parameter/Stinger, Export… (a
folder chooser), the state playing and the one waiting with its seconds
and sync point; the states as buttons (a click goes there as the game
would; right-click: loops/once, then, entry cue, start, remove; F2
renames), the rules (a click opens everything a rule sets), layers,
parameter sliders (the preview's values; right-click: each layer's curve)
and stingers (a click plays one). Dev action `adaptive:` (see the CLAUDE.md
notes). Tests: project `interactive::tests` (rule precedence, beats and
bars under tempo and metre changes, loops coming round, grids, markers,
cues, repeating scenes, layer curves, the command and files), transport
`planned_jumps_land_on_their_frames_and_change_the_loop`, engine
`launch` and `transition` units, `tests/interactive.rs` (jumps on their
frames in any block size, the same with render-ahead, fades as heard, a
launch and a return on samples, trims), `interactive_score_transitions_do_not_allocate`,
`ahead` amend units, audio-files `markers::tests`, session
`tests/adaptive.rs` (edits, the preview moving on a bar in real time,
the export's files: loop points on their frames, manifest, `node --check`
of the FMOD script when Node is there, the WAAPI calls, the runner
compiled by Python), the view's tests.

### MIDI 2.0

Packets: `faderframe_midi::ump` (pure: `Ump` = 1–4 words by message type,
`packets` splits a stream, `Message::{parse, to_ump}` for utility, system,
MIDI 1.0 and 2.0 channel voice (`Voice2`), SysEx7, flex data (tempo,
meter, key, text) and UMP stream messages; `scale_up`/`scale_down` = the
specification's min-centre-max scaling; `Midi1ToMidi2` = the default
translation with bank select and RPN/NRPN folded in, `Voice2::to_midi1`
back; `Voice2::to_midi1_fine`/`Voice2::of_event` = one message both ways
with its full resolution beside the MIDI 1.0 form; `per_note_expression`/
`per_note_message` map per-note pitch bend (±48 semitones), absolute pitch
7.25 and the registered per-note controllers (volume, pan/balance,
modulation, expression, brightness) to the host's `NoteExpressionKind`s
and back).

Devices (Linux): `faderframe-midi-ump` is a MIDI 2.0 client of the ALSA
sequencer (`snd_seq_set_client_midi_version`, one in and one out port,
`ports()` lists the other UMP clients — kernel UMP clients for MIDI 2.0
hardware, programs — with `chosen_ports` taking an endpoint port for its
groups; a reader thread polls and hands packets on; `UmpWriter` writes
straight to a port; one handle behind a mutex). `faderframe-midi-io` lists
them as inputs and outputs ("… (MIDI 2.0)", keys `ump:<client>:<port>`),
leaves those devices out of midir's MIDI 1.0 lists (`UmpClient::claimed`
→ `Claimed`: by sequencer client on Linux, by name on macOS), files
packets sent straight to FaderFrame under "Sent to FaderFrame (MIDI 2.0)",
and gives MIDI 2.0 outputs `Sink::Ump` (events at the resolution they
carry, expressions as per-note controllers). `FADERFRAME_NO_UMP=1` keeps
it closed.

Devices (macOS 11 and later): the same `UmpClient` API on CoreMIDI
(`core_midi`, safe code on the `coremidi` crate that midir already brings,
which holds the FFI): an input port created with
`MIDIInputPortCreateWithProtocol` (MIDI 2.0) connected to the MIDI 2.0
sources, an output port sending event lists (`MIDISendEventList`) to MIDI
2.0 destinations, and a destination of FaderFrame's own ("FaderFrame MIDI
2.0 In", `MIDIDestinationCreateWithProtocol`) that programs send to.
MIDI 2.0 endpoints are the ones whose `kMIDIPropertyProtocolID` is 2;
offline and private ones are left out, and a driver's endpoint counts as
hardware. CoreMIDI endpoints have no sequencer addresses, so `addresses`
gives each one a stable `(client, port)` pair by its unique id (port 0 a
source, 1 a destination; FaderFrame's own destination is `(0, 0)`, which
midi-io files as sent straight to FaderFrame). CoreMIDI calls the input
on its own high-priority thread, so there is no reader thread. Nothing is
published as a source of FaderFrame's own, as on Linux. Tested on the
macOS runners (`tests/coremidi.rs`: two clients in one process, a
destination listed and written to bit for bit).

Windows: Windows MIDI Services is not wired (the existing midir/WinMM
path stays). What blocks it, as of November 2026: the
`Windows.Devices.Midi2` WinRT API is still a preview (Preview 11 locked
its metadata in October 2026 but it has not shipped in Windows); it is not
in the Windows SDK metadata the `windows` crate projects, so bindings
would have to be generated with `windows-bindgen` from the preview
`Windows.Devices.Midi2.winmd` of the NuGet package, whose redistribution
(dll, winmd, pri) is under a "go-live" agreement with obligations (keep up
with every preview, tell customers, remove the binaries within 90 days of
the in-box release) rather than a permissive licence; the API needs
Windows 11 25H2 or later, and nothing here can run it (Wine has no MIDI
service; the CI's Windows images do not have it). Once the in-box API
ships it reaches the `windows` crate with the SDK metadata, and the
backend is a `UmpClient` like the others: `MidiSession`, endpoint
connections by device id, `MidiApi::IsSystemProvided` and activation
failures as the feature test, midir/WinMM as the fallback.

Resolution beside the event: inside, events stay MIDI 1.0 shaped
(`MidiEvent`) and carry what MIDI 2.0 adds beside them, a
`faderframe_midi::Fine` (the value at full width — a note's 16-bit
velocity, 32 bits for controllers, pressure and pitch bend — and a note's
attribute) on `TimedMidiEvent::fine`, `MidiInputEvent::fine`,
`MidiOutputEvent::fine`, the recorder's `RecordedMidi::fine` and the
snapshot's region events `(time, event, fine)`. A fine value counts only
while it agrees with the event — its top bits are the event's own value
(`MidiEvent::{fine_value, full_value, full_unit, velocity16}`) — so
anything that changes an event at seven bits (a velocity curve, a MIDI
effect, an articulation map) leaves the old one behind by itself and the
event plays at what it says; moving an event in time, to another key or
channel keeps it (`TimedMidiEvent::at`, buffers' `merge_from`, the PDC
`EventDelay`, chasing). Without a fine value `full_unit` is the MIDI 1.0
value over its own range, exactly as plugins were always given it.
`MidiInputSender::send_ump` puts the resolution of notes, controllers,
pressure and pitch bend beside their MIDI 1.0 forms (per-note controllers
and a note-on's pitch 7.9 attribute still arrive as per-note expressions).
Who uses it: CLAP note-on/off velocities as floats at 16 bits; VST3
velocities, poly pressure and the CC/aftertouch/pitch-bend parameters at
full resolution (and plugins' own notes back at 16 bits); the sandbox's
shared memory (`WireEvent::{fine, has_fine, attribute, attribute_data}`,
shm v6); the built-in synth's velocity and the per-note Velocity
modulation source; MIDI learn values (32-bit controllers move mapped
parameters smoothly); the MIDI effects pass the played note's resolution
on where the note goes through (scale, the chord's own key, the echo's
dry note); MIDI 2.0 outputs (`ump_ports::event_words` via
`Voice2::of_event`). MIDI 1.0 outputs, LV2, AU and SFZ instruments get the
MIDI 1.0 bytes (the top bits). The snapshot also gives MPE's member
channel bends, pressure and timbre their 32 bits. Scripted runs play
packets through the keyboard input with the dev action `ump:<hex words>`
(`ump:40903c00 12340000` = a note-on at velocity 0x1234).

Clips at MIDI 2.0's resolution: `MidiNote::fine: Option<NoteFine>`
(16-bit velocity and release velocity, the note-on's attribute; keep
`fine: None` in new `MidiNote` literals) and `ControllerPoint::fine:
Option<u32>` (32 bits; programs have none) sit beside the 7-bit (14-bit
for pitch bend) values that everything else keeps editing. The same rule
holds: a fine value counts while its top bits are the stored value
(`MidiNote::{velocity16, release16, attribute, on_fine, off_fine}`,
`ControllerPoint::{value32, fine_of, unit, steps}`), so old files load
with every value derived from the 7-bit one (`scale_up`) and no schema
change, and nothing new is written for MIDI 1.0 values (a fine value equal
to the scaled-up one is dropped: `set_velocity16`, `normalize_fine`,
`ControllerPoint::at32`, `ControllerLane::normalize`, which also compares
repeats at 32 bits). Recording and Capture MIDI keep what MIDI 2.0 devices
play (`RecNote::{on, off}` + `keep_fine`, `RecController`'s 32-bit value).
Editing at full resolution: the piano roll's velocity lane drags and draws
lines at 16 bits (`NoteOp::{SetVelocity16, OffsetVelocity16,
RampVelocity16}`), controller lines interpolate at 32 bits between their
ends (freehand drawing stays at the lane's pixels, MIDI 1.0 steps); the
older velocity operations keep the resolution where notes have it
(`MidiNote::set_velocity_keeping`: the 7-bit result exactly as before,
the 16 bits as near the scaled value as that allows; shuffles carry the
16 bits along); the event list shows MIDI 1.0 steps with MIDI 2.0's
fraction cut to two decimals (`EventRow::{fine, release_fine}`, "100.56";
`steps_text`), and a typed or dragged fraction sets the full resolution
(`EventValue::Fine`, `velocity_of_steps`, `ControllerPoint::of_steps`)
while whole numbers set MIDI 1.0 values. `.midi2` clip files keep it all
(16-bit velocities and attributes, 32-bit controllers, both ways);
Standard MIDI File export writes the top bits.

Inside, recording and Capture MIDI keep per-note expression as before
(`MidiTake::expressions` → `native_expressions`, by channel and key,
thinned like MPE's).

Clip files: `faderframe_midi::clipfile` (`SMF2CLIP`, big-endian words, a
Delta Clockstamp before every packet, ticks per quarter in the header,
Start/End of Clip; gaps over 20 bits as several clockstamps with no-ops) and
`session::midi2file` (`export_midi2_clip` writes one clip with its 16-bit
velocities and attributes, 32-bit controllers, programs with banks,
expression, SysEx, the project's tempo/meter/key and the clip's name;
`import_midi2_clip` reads MIDI 1.0 and 2.0 messages into a clip on a new
instrument track, at their resolution, poly pressure during its note as
the note's pressure). Import MIDI takes `.midi2`; File → Export MIDI 2.0
Clip….

Tests: `ump`/`clipfile` units, `faderframe_midi` `a_fine_value_counts_while_it_agrees`,
`midi2_packets_arrive_at_full_resolution`, project
`a_note_keeps_its_16_bit_velocity_while_it_agrees`,
`old_notes_and_points_load_and_new_ones_keep_their_resolution`,
`controller_points_hold_32_bits_in_midi_1_steps`, session
`tests/midi2_resolution.rs` (a take from a MIDI 2.0 port, a captured
phrase, clips played to MIDI 2.0 and MIDI 1.0 devices, `.midi2` vs SMF,
edits), `tests/{midi2,midi2file}.rs`, CLAP `notes_carry_ids_and_note_expressions_reach_their_keys`
and VST3 `hosts_a_plugin_with_separate_controller` (velocities as floats),
the sandbox's wire round trip, `event_delay_shifts_events`,
`midi2_resolution_does_not_allocate`, macOS `tests/coremidi.rs`, opt-in
ALSA `tests/sequencer.rs`.

### Screen readers and the keyboard

Every canvas view can describe its controls (`CanvasView::accessible` →
`faderframe_ui_canvas::access::AccessNode`: role, name, value with its text,
checked/selected, bounds, and the actions Enter and the arrows send; ids
from `access_id` stay the same while the same things are shown) and names
itself (`accessible_name`; docked views take their tab's title).
`faderframe-ui::access` turns the tree into GObjects implementing
`gtk::Accessible` (`AccessObject`, an `ATContext` each) under the canvas,
whose `first_accessible_child` lists them before its child widgets; the
canvas asks for the tree four times a second and on every keyboard action,
rebuilding objects only when the shape (ids, roles, child counts) changes,
else telling GTK the names, values and states that changed. The tree is
taken out of its `RefCell` while GTK is told (GTK calls back), and GTK's
callbacks read the separate `access_roots`.

Keyboard: Tab into a view (GTK's focus chain reaches the canvas's `focus`
vfunc) moves onto its first control; Tab and Shift+Tab move along them, off
the end to the next widget; Enter activates, the arrows step (Page keys:
ten steps, each from where the last left it), Escape or a click leaves; a
focus ring marks the control. Views that take Tab themselves (`uses_tab`:
the arranger's tab to transient) start on their controls with Ctrl+Tab.
After the view moves its own list selection with a key, the focus follows
the selected item. The focused control is the canvas's active descendant
and, on GTK 4.18 and later, its focused platform state
(`gtk_accessible_update_platform_state`, looked up in the running process
so FaderFrame still runs on GTK 4.14); after an action the control's new
state is announced. The chrome: on/off transport buttons report `Pressed`,
transport changes ("Playing", "Stopped", "Recording") and session notices
(errors at high priority) are announced. `HostRequest::Announce` lets views
say something.

What only the view can do for a control — open the menu, text entry or
chooser its press opens, change its own presentation (a filter, the song
selected, a divider) — the keyboard asks of the view: a node marks it
(`on_press`: Enter; `with_menu`: Shift+F10 and the Menu key, and Enter
when nothing else answers; `view_steps`: the arrows) and the host calls
`CanvasView::access_press(id, AccessPress::{Activate, Menu, Increment,
Decrement})`, whose requests and actions go out as for an event (after a
menu opens the control is not said again: the menu speaks for itself).
`access::key_on` is the one place that decides what a key does on a node,
for the host and for the views' tests (`access::press_key` drives a view
as the host would). A node can also be `disabled` (told as unavailable,
the keys pass it by), a `tab_stop` that does nothing (a meter the keyboard
visits to hear it), and `fixed` along an axis (a toolbar, a header row or
column): the host never scrolls the view to bring it in. Views with no
scrollbars still answer `scroll_info` where they scroll (the album's
songs, the automation lanes) so the keyboard's focus is scrolled into
view.

What changes by itself — a clip launched, an analysis finished, a level
past its limit — a view says through `CanvasView::access_news`, asked with
the tree four times a second while the view is shown: it compares with
what it last told (the first call only looks) and returns sentences the
host announces. After a key the news and the control's new state are said
together, a sentence the control says anyway left out (a tool switched
from the toolbar is not said twice).

Views: the mixer (strips: volume and pan sliders, mute/solo/arm toggles,
inserts that open their editors), the arranger (tracks: header toggles,
volume, clips with their spans — Enter selects and locates), the transport
display (position, tempo spin button, meter), the piano roll (notes —
Enter selects, arrows transpose), the event list, setlist (and its stage
screen), undo history, every kit device editor (sections as groups, knobs
as sliders by a hundredth of their travel, switches as toggles, stepped
values a step at a time), and:

- the edit toolbar: a toolbar of modes, tools and options as toggle
  buttons pressed while on, menu buttons (grid mode and value, ripple
  scope, trim and grabber kinds, nudge, quantize) opening their menus, the
  transient sensitivity a spin button, the counters changing their units;
  a mode, tool, ripple or grid changed by a shortcut is said;
- the Album: the toolbar (menu buttons, "+ Files…" its chooser, the album
  file a toggle), the player (previous/play/next/back, unavailable while
  nothing plays, the album's position a slider by five seconds), the songs
  as a list — a song read as its number, title, length and loudness (the
  rest described); Enter selects it (the arrows and Delete are the list's),
  its menu the row's; its title, fades and ISRC typed with Enter, its pause
  or crossfade and trim spin buttons by a tenth, its inserts' menu — and
  the export folder; an analysis, export or preparation starting and
  finishing (with the album's loudness), a song failing, the song playing
  are said;
- the Clip Launcher: toolbar (quantise menu, Back to Arrangement, Record to
  Arrangement as a toggle, + Scene), the tracks' headers (Enter selects),
  a group per scene — launch, name (Enter renames) and a slot per track
  read as its track, clip and state; Enter does what a click on its ▶ does
  (a held gate or repeat clip: its release; an empty MIDI slot: a new
  clip), every slot, scene and stop button has its menu (the slot's adds
  Move To / Copy To, the drag's way for the keyboard) — and the stop
  buttons; launches, stops, the way back to the arrangement and slot
  recordings are said;
- Automation: the toolbar (Selected tracks only a toggle, the lane tools
  unavailable without a lane), the list's rows as shown (a track: its menu
  adds a lane; a lane: Enter edits it, its menu the mode's, with its mode
  and arranger visibility as controls), the editor's "Add point at the
  playhead" (its menu: the segment's shape, Write Value) and each point a
  slider by a hundredth of the lane — Enter locates to it, its menu
  shapes, deletes or moves it by a sixteenth (Move Earlier/Later, in the
  pointer's menu too);
- Tools: the toolbar's menus and toggles, then every reading a meter the
  keyboard visits (loudness and dynamics, levels per channel, correlation,
  the spectrum summed up as where it is strongest, the codec pane with its
  buttons). Announce Limits (a toolbar toggle of the view) says a reading
  crossing its limit (`access::Limits`): the true peak over −1 dBTP, the
  integrated loudness reaching the target (±1 LU) or leaving it, the
  correlation below −0.1 and back over +0.1, the codec's decoded peaks
  over full scale;
- Video: the picture as one control read as what it shows (timecode, file,
  frame; the calibration's step) and described (comparison, the ADR cue,
  jobs), its menu the window's; a wiped comparison's divider a slider of
  the view's; jobs finishing and a calibration's result are said.

Tests drive the trees through `press_key` (each view's `keyboard` tests,
`edit_bar::access_tests`, `access::tests::keys_reach_actions_presses_and_menus`).
Check with the system's AT-SPI (the scratchpad's `a11y_dump.py` /
`a11y_events.py` on `/usr/bin/python3` with `gi` Atspi) and the dev action
`access-key:<view>=<tab|backtab|enter|up|down|left|right|page-up|page-down|menu|escape>[/…]`
(`edit-bar` and `transport` name the edit toolbar and the transport
display; announcements are logged at debug level as "access: said …").

### Singing into MIDI (voice ports)

Every input channel of the audio device has a MIDI input port "Voice · In
n", and "Voice · All Inputs" takes them all (`session::voice`, virtual
inputs of the hub, left out of the Preferences' input list). A track whose
MIDI input names one makes the session hand the engine a listener (`engine::voice::VoiceRun`: a
`faderframe_analysis::voice::VoiceTracker` for that channel at the device
rate, built on the control thread, `Message::Voice`, retired as garbage;
settings by `Message::VoiceConfig`). In `render`, right after the queued
live MIDI is taken, the tracker hears the callback's input and its notes
are injected into that callback's live MIDI (`MidiInputState::inject`, in
order) on the voice port at the frame each was decided — the instrument
plays them in the callback they were heard, with nothing but the input and
output buffers and the detection itself between voice and sound (6 ms for
A4, 11 ms for A3 at Fast; `engine/tests/voice.rs` measures it). Each
injected event's `early` is how much earlier its sound began, so the
recorder (`MidiInputBlock::iter_early`) places it where it was sung, as it
places hardware MIDI; a copy of each goes to the session through a ring
(stamped with when it was sung) for Capture MIDI and the activity lights
(`VoiceState::take_feed`, joined with the devices' messages in
`tick_midi`).

The tracker: high-passed at 50 Hz (rumble), low-passed and decimated to
~11 kHz; every 1 ms McLeod pitch over the shortest window holding 2.3
periods — 12 ms first, 22 or 40 ms for low voices, and while a note sounds
2.6 of its periods — up to 2.3 kHz (whistles); a pitch an octave or a
twelfth lower that is clearer by 0.04 is taken instead (a vowel's first
formant on the second harmonic sounds an octave up otherwise,
`Detector::peak_near`), and before a note starts the next longer window
must not clearly hear another pitch. `Responsiveness` sets the holds and
the clarity a voiced frame needs (Fast 2 frames at 0.6, Balanced 4 at
0.78, Stable 14), change after 6/12/22 frames, release after 20/30/40
unvoiced. A change waits until the pitch has settled (spread under 0.4
semitone over 8 frames and not sliding into another key within 30 ms — the
passing keys of a legato glide are no notes — unless the key holds three
times its change frames) and never happens while the voice fades (3 dB in
8 ms: a fall at a note's end). A young note (its first 100 ms) holds only
0.6 semitone around its key, so a note begun on the way up a scoop moves
to the key the voice settles on; an established one holds within the swing
it showed over the last 250 ms plus 0.15, at least 0.8 and at most 0.95
semitone each way (vibrato on a note sung a little off). Stable waits for
a scoop to settle before a note starts (up to 90 ms; the others start at
once — a note's first frames drift while its window fills). A note is
placed where the voice began, a change where the new key was first heard;
bends at most every 4 frames unless the pitch jumps; each event with where
it was decided (`now`) and where its sound began (`at`). Allocation-free
after `new` (fixed rings of the last 256 frames' pitch and level).

Levels (`voice::level`): a frame is voiced over `VoiceConfig::threshold_db`
(default −62 dBFS; saved preferences with the old default −45 read as the
new one) and over the input's own noise floor plus 6 dB (`LevelGate`: the
6 ms level followed down at once and up at 10 dB/s while nothing plays,
0.5 dB/s while a note sounds; it starts where the gate is the threshold),
so any input gain works and the room's noise stays out. Mains hum starts
no note: a pitch within 18/30 cents of 50/60 Hz or 12 cents of their 2nd–4th
harmonics only carries on the note it belongs to (no equal-tempered note
is that near). Velocities follow the performer: 127 at the loudest note
start lately (a reference at least 12 dB over the gate, falling 1 dB/s),
1 at 36 dB under it — or a calibration's `Dynamics` (velocity 40 at the
soft level, 127 at the loud). Each tracker counts every frame's level, and
the voiced ones, into a `Meter` (1 dB bins, atomics: the audio thread only
adds; `VoiceRun::meter`), which the session reads as `Levels`.

Calibration (`voice::calibrate`, `Session::start_voice_calibration`,
Preferences → MIDI → Voice to MIDI → Calibrate…, dev action
`voice-calibrate:<n|cancel|forget|state>`): the input's listener (one is
made for it when no track takes it, on that input's own port) counts the
room alone for 3 s, then the performance for 8 s; the room's 95th
percentile + 3 dB (and any pitched noise in it + 4) is the lowest the gate
goes, the soft notes' 10th percentile − 6 the highest; the threshold sits
35 % of the way up (the soft notes win where they overlap, with advice).
The soft and loud levels become `VoiceSettings::dynamics`; advice for hot,
quiet or noisy inputs (`calibration::Advice`); `forget_voice_calibration`
goes back to the defaults. Glides as note expression (tuning), pitch bend
(±2) or not at all; notes can snap to the project's key (`VoiceSettings`:
sensitivity, glide, response, in key, dynamics — Preferences → MIDI →
Voice to MIDI).
Polyphonic mode is opt-in (`VoiceSettings::polyphonic`): `PolyTracker`
uses a 96/128/160 ms Hann window at Fast/Balanced/Stable, analysed every
4 ms. Harmonic candidates share spectral energy; up to six notes are
followed independently, with per-key tuning in `VoiceEvent::Bend`. The
longer windows resolve nearby low fundamentals. Ambiguous simultaneous
octave doublings can merge, and pure sine tones lack the required
harmonics. Note expression glides each key; channel pitch bend is only
used in monophonic mode, since it would bend every note together. Its
levels: new notes start only while the input's 6 ms level is over the
threshold and the floor it follows; each note's level (its partials) must
be over that floor and at most 22 dB under the loudest notes lately (the
gate follows the performance, so a quiet guitar is heard like a loud one);
new notes start only within 24 dB of the input's loudest lately (falling
6 dB/s: decays start none); mains hum starts no note; a note played again
starts again only when its fundamental keeps up for 3 more frames (what
rings on as the next chord is struck swells for a frame in its leakage);
the meter counts the loudest note's level for voiced frames.
Changing mono/poly constructs a new listener on the control thread.
Releases from the old listener are deferred in `MidiInputState` until the
next nonempty callback, so `take` cannot erase them. The session drains
retired listeners' feeds through their final note-offs, preserving Capture
MIDI. Missing inputs, changed device rates, and changed glide modes also
release the listener; resets discard old analysis audio.

Every input a track takes is listened to at once. The engine keeps up to
`voice::MAX_VOICES` (64) listeners in a vector made with that room
(`Message::VoiceAdd` replaces one on the same input and port,
`VoiceRemove { input, port }` releases and retires one, `Voice` clears
them all and is what `set_voice` sends; `VoiceConfig` reaches every one).
Each listener has its input, its port and its MIDI channel
(`VoiceRun::on_channel`). The session (`tick_voice`, `voice_wanted`)
reconciles what the tracks' MIDI inputs name with what it listens to: a
listener per "Voice · In n" a track takes (on channel 1), and for the port
"Voice · All Inputs" one per input up to 15, input n on MIDI channel
n + 1 — an MPE lower zone's members, so one track takes a choir or a
hexaphonic pickup with each singer's or string's glides on its own
channel. Listeners that stay keep their trackers (a note goes on while
another input starts or stops); another device rate or mode makes them
all anew. Each listener's feed is drained into `tick_midi`; a removed
one's feed drains to its last note-off. Tests: `voice::tests` (a sung
phrase placed within 15 ms at every response, onset latencies by key,
threshold, scale, octave), engine
`an_early_stamp_plays_now_and_records_where_it_was`, `tests/voice.rs`
(voice to sound; `two_inputs_are_listened_to_at_once`: two ports and
channels, one leaves), `voice_to_midi_does_not_allocate` (a second
listener added and removed), `session/tests/voice.rs` (the dummy
device's tone played and recorded; `two_inputs_sing_into_two_tracks_at_once`
and `the_all_inputs_port_puts_each_input_on_its_own_channel` on
`DummyBackend::with_input_spread`, whose input n sings 4n semitones above
A3 from a second in).
Polyphonic tests cover plucks, strummed chords at all three responses,
arpeggios, retriggers, noise, thresholds, scale snapping, reset, block-size
invariance and device rates from 44.1 to 192 kHz. The engine test plays and
records a detuned triad, checks each key's expression, and verifies release
through a zero-frame control pump; the counting allocator covers chords,
settings changes and listener retirement. Manual timing:
`cargo test --release -p faderframe-analysis polyvoice::tests::callback_cost -- --ignored --exact --nocapture`.
Measurement: `faderframe-analysis/tests/voice_corpus` makes sung phrases
(Rosenberg glottal pulses through vowel formants, scoops, vibrato, notes
off pitch, falls, breaths, consonants, loudness moving; a man's and a
woman's voice), whistling, a plucked lead line, strummed and picked guitar
chords and piano (stiff detuned strings, hammer) — each at −40 to −6 dBFS
peak in a quiet room, a noisy one (pink noise 32 dB under) and with 60 Hz
hum — and scores every note (right, late, octave, wrong, missed, merged
doublings, notes in silence, stray, played again, latency, end, velocity):
`cargo test --release -p faderframe-analysis --test voice_corpus -- --ignored --nocapture`
(`VOICE_CORPUS=mono|poly`, `VOICE_RESPONSE=all`, `VOICE_CALIBRATED=1`;
`voice_corpus_dump` with `VOICE_DUMP=<piece>@<dB>@<room>` lists one case
note by note, `voice_corpus_cost` times the trackers). At Balanced the F1
of right notes went from 47 % to 79 % (monophonic) and 52 % to 68 %
(polyphonic), the same at every level; `voice_corpus_holds_its_accuracy`
holds it on part of the corpus.

### Polyphonic pitch editing

The Pitch editor's Polyphonic option (on by default; `EditFlag::PolyphonicPitch`,
`EditorSettings::pitch_polyphonic`; off: the melodic editor, a single line
played live by PSOLA with its formants) finds every note, chords too, and
moves each on its own.

Detection (`session::polypitch::hear_notes`, which lead sheets hear their
audio with too): the clip's original audio through the
transcription model (`to_midi::harmony`, every note), overtone notes of
louder ones dropped (`drop_overtones`: on the 2nd–12th harmonic, sounding
mostly with it, under 60 % of its velocity), each note followed to its pitch
every 5 ms (`faderframe_polypitch::follow` — `pitch_tracks` is its pitch: a
weighted harmonic sum within ±70 cents, then the exact peaks of its own
unshared harmonics; it also gives the sum as the note's level) — a
`PitchEdit` whose notes may overlap, marked `Polyphonic { original,
rendered }`, in one "Detect Pitch" step. Notes also carry `gain_db` and
`muted`; `PitchOp::{Gain, Mute, Forget}`.

The sound (`faderframe_polypitch::render`, pure): an STFT; in each frame
every sounding note's harmonic peaks are found near the multiples of its
pitch, a peak two notes share is split by their levels, each peak's bins
(halfway to the next peak, at most four) move by its note's ratio — whole
bins, with the phase turned on by the frequency change so the partial lands
on its new frequency exactly (Laroche–Dolson peak shifting) — and take its
gain; bins no note owns (noise, attacks) stay. The window is 85 ms where
notes sound together (their partials apart) and about 4.5 periods (at least
20 ms) for a note alone (vibrato followed); only the spans of changed notes
are processed, crossfaded into the original, the rest bit for bit.

Lengths and removal: each note is stretched on its own — the rest of the
sound (the other notes, noise) stays where it is, and a longer note rings
on over whatever follows, as a held key would. `Moved::placed` is where it
sounds now: where it was played it is taken out (its peaks' share at gain
0, as muting does), and over `placed` each frame takes its partials from
the frame of its own sound that falls there (stretched evenly; shared peaks
split as there), turned by `f·(ratio·hop − dt)` per frame, `dt` the step
between the frames taken — a phase vocoder on that note's peaks alone,
which reduces to the shift above where nothing is stretched. Removed notes
(`PitchEdit::removed`) go to the renderer at gain 0. Test:
`one_note_of_a_chord_is_made_longer_and_another_shorter` (a held E within
0.01 dB of its level in the chord after the chord ends, C and G 70 dB
down there; a shortened G gone where the chord still plays).

Playback: the engine plays a polyphonic clip's source like any other
(`PitchEdit::plays_live` keeps PSOLA to melodic edits). The session renders
once an edit has settled (300 ms, no gesture open) on a worker and puts the
new source and the notes' `sound_key` into the clip without an undo step
(`apply_unrecorded`): the edit that caused it is the undo step, and since a
clip's content carries notes, source and key together, undo and redo bring
back matching audio; wherever key and render disagree the session renders
again (`poll_polyphonic`). Polyphonic editing refuses clips with spectral
edits or clip effects (melodic works on them). Tests: `faderframe-polypitch`
(a chord's note moved — its partials 46–64 dB down where they were and
arriving within a dB, the others ±0.01 dB; a note removed; each note followed
to a cent; vibrato straightened; untouched audio bit for bit) and
`session/tests/polypitch.rs` (a recorded chord found, moved, undone, redone,
muted, removed).

### Time-locked tracks

A track's `TimeBase` (`Track::time_base`, `Command::SetTrackTimeBase`,
Impact None) places its material with the music (Musical, the default)
or in time (Time-locked), as Cubase's linear tracks and Pro Tools' sample
time base do. Positions stay `MusicalTime`: `Session::edit` runs
`faderframe_project::time_lock::plan` before every edit and applies
`TimeLock::keep`'s moves after it in the same undo step. The rule:
material keeps its sample unless the edit places it; a position an edit
gives is read against the tempo map in effect where it gives it (a
ripple's clip commands: the old tempo; a rearrangement: its own
timeline). Every conversion goes through whole samples at the project
rate (`TempoMap::musical_at_sample`), so tempo-lane drags never drift and
undo is exact. Covered: audio clips and take folders, MIDI clips' starts
(their notes follow the tempo from the start: Logic's region lock),
automation points, a frozen render. Material placed on a locked track
lands on whole samples (the grid snaps onto the sample under its line).
Ripples move locked tracks by the time they take out or open
(`RippleShift::map_in_time`, `time_lock::ripple_curve_in_time`); section
moves carry locked material with its section; tempo from hit points
always keeps locked tracks. ADR cues are always locked, whatever any
track's time base — they mark the picture's dialogue (`keep_adr`, one
`SetAdr` in the same step) — and the ADR beeps' track is made
time-locked. No per-clip lock: the track is the unit. The arranger marks
locked tracks and clips with a padlock (`controls::lock_glyph`); Time
Base in the track menus and the Inspector.

Notes, markers and sections can be placed in time on their own, on any
track, through the same planner. A locked note (`MidiNote::time_locked`,
Logic's SMPTE lock of an event) is a hit inside a musical cue: a tempo
change keeps its start and end on their samples — its expression points
on theirs — while the clip and its other notes follow the beats. The
rule is the tracks' rule applied inside the clip: a note the edit leaves
alone keeps its sample; one the edit places (moved, resized, made, newly
locked) is read against the tempo map in effect where it was given and
lands on a whole sample; a clip the edit moves carries its locked notes
at the same time from its start (in samples), so a cue moved across a
tempo change keeps its hit as far into it. A locked note the clip no
longer spans after a tempo change — on a musical track a slower tempo
before the clip moves its start past the hit — makes the clip grow to
hold it (its start back to the note, everything else keeping its place
in the music; its end on to the note's end), so the hit is never
silently cut off; one already outside stays outside. Keeping is one
`SetClipContent` per clip in the edit's undo step. Quantize and Humanize
leave locked notes' timing alone; explicit moves place them. Aliases
share content, but a tempo change keeps each alias's locked notes on its
own samples, so their content then differs by those notes until the next
edit mirrors one. Locked markers and sections (`Marker::time_base`,
`Section::time_base`, both bounds of a section) keep their samples; a
ripple of every track moves them by the time it takes out or opens
(`RippleShift::map_in_time`, `RippleShift::map_range_all_in_time`), a
section move carries them with their span in its own timeline, and
tempo from hit points leaves locked markers that are not hits exactly
where they are. The cuts found in the picture and the shots a conform
adds are locked markers. All three fields are written only when set, so
older files read as musical. The piano roll draws a padlock at a locked
note's start (beside a diamond), locks from the note menu (Lock to Time,
checked when every selected note is) and says the note's place in time
in its inspector and tooltip; the event list has a padlock column; the
marker and arranger lanes draw the padlock before a locked marker's or
section's name and lock from their menus; the Inspector's Time Base
line counts a track's notes in time and its menu frees them. Not locked:
controller points, SysEx and the clip launcher's clips (they play from
their launch, not the timeline).

### Track delay and Auto-Align

`Track::delay_ms` is the track delay of Pro Tools and Cubase:
non-destructive, to a fraction of a sample, within ±`MAX_TRACK_DELAY_MS`
(250 ms), `Command::SetTrackDelay` a parameter edit (coalesced), not the
master's. With a delay the track's input node is a `nodes::TrackDelay`
(live and ahead graphs): whole samples from a ring, the fraction through
a 64-tap Kaiser-windowed sinc (within 0.001 dB and 0.0001 samples of
ideal to 20 kHz at 44.1 kHz), the taps summed in a fixed order (render
ahead bit-exact). It needs 32 samples of look-ahead: a delay of a
millisecond or more reports no latency, a smaller or negative one
reports the look-ahead plus the whole milliseconds of advance and delays
itself by that minus the advance, so the graph's compensation hears the
track early by exactly its delay. Only a change of `DelayShape` (none,
later, each whole millisecond ahead) rebuilds the graph; within a shape
the node follows its slot with a 10 ms crossfade. Not automated (a moving
delay is a pitch effect). A frozen track plays its file through its delay
and polarity, so freeze renders leave both out (bounces keep them).

Auto-Align (`session::align`, `Action::AutoAlign`, window
`faderframe-ui/src/auto_align.rs`, in the track menus and the Inspector's
Delay menu) renders the selected tracks' input stages (the clips as
played, no devices) over the edit range, else where their clips overlap,
else everything (the busiest 30 s), and `faderframe_analysis::align`
compares every pair: cross-spectra averaged over the busiest frames where
both play, maximum-likelihood weighted (Hannan–Thomson: the coherence's
weight, debiased) over 30 Hz–18 kHz; the strongest peak within ±30 ms
gives lag and polarity, its robust z-score the confidence (below a floor:
no common signal, left alone), the next peak's height the clarity
(bleed-only pairs); the lag refined between samples, twice. Arrival times
run through the track sharing the most (the overheads, mostly); the
default reference is the earliest. Apply is one undo step: each included
track's delay and polarity. On synthetic kits (delays, filters, rooms,
level, flips, noise, bleed) within 0.004 samples.

### Aligning doubles to a guide

Align Timing (`session::timing_align`, `Action::AlignTiming { guide, dubs,
tightness }`, the arranger's clip menu "Align Timing to", dev action
`align-timing`) warps dub clips — a doubled vocal, backing vocals, an ADR
line — onto the audio of a guide track. A thread reads the guide as it
plays over each dub (its clips and take folders' comps through their
warps, reversed clips backwards) and the dub as it plays now, both mixed
to mono and low-passed to 16 kHz, and `faderframe_analysis::timing_align`
finds where the dub sang each moment of the guide:

* **Features** every 5 ms: cepstra of third-octave band levels (bands at
  least 450 Hz wide, so the envelope counts and not where another voice's
  harmonics fall; each coefficient's mean over the active frames taken
  off), the onset strength (the bands' rises) and the level against the
  loudest. Sibilants (bright, noisy, sounding) are marked in the dub.
* **DTW** with the symmetric slope constraint (steps of one or two frames:
  never more than twice as fast or slow), a small cost for leaving the
  diagonal (held vowels give no reason to wander), open at both ends:
  first on 40 ms frames within ±0.5 s of the lag the onset envelopes
  agree on, then on 5 ms frames within ±120 ms of that path.
* **The map**: the dub frame per guide frame, its offset smoothed over a
  Gaussian of 300 ms (Tightness 0: phrases) to 10 ms (1: syllables) where
  either sounds (silences take the offsets around them), the speed held
  within 1.25 (loose) to 2 (tight) either way. With Protect Consonants
  each sibilant up to 150 ms plays at its own speed from where its onset
  lands, the map rejoining within the speed allowed after it.

The map becomes the dub's warp: its edges where the map puts them (the
source offset and length moved, clamped to the source), markers where it
bends (Ramer–Douglas–Peucker within 0.5 ms), composed with the warp the
dub had. With "Align Pitch Too" (`EditorSettings::timing_align.pitch`)
the dub gets a melodic pitch edit (its own, or one found now) whose
notes move toward the guide's median pitch where they now play, folded
into an octave and only within two semitones (a harmony keeps its
notes). All dubs of a request change in one step "Align Timing"; a dub
edited meanwhile is left alone, with a notice. Synthetic doubles (late,
each syllable off by its own amount, longer or shorter, another voice, a
harmony a fifth up) are aligned within a few milliseconds of every
syllable (`timing_align::tests`, `session/tests/timing_align.rs`).

### A/B listening: the undo history and reference tracks

A compare source can be heard instead of the mix: `nodes::compare` inside
`ListenOut`, before listening, so headphones, the mono check and bass
management apply to both sides. The control side hands a `CompareSource`
(a `StreamSource` of its own, read only by this node; the disk loader
keeps its pages resident round where it reads) to the `CompareLink`
inside `ListenLink` through a mailbox (and back through another); the
link keeps the wanted source, so a listening node built later starts with
it. Switching is a 20 ms equal-power crossfade, nothing rebuilt. The
position comes from the play history at the node's input latency, so
locates, loop wraps and plugin latency land on the same frame as the mix
(`Follow { anchor }`), or a region loops on its own while the transport
plays (`Loop`). Monitoring only — renders build their own engine — and
never on a meter or analyser (the master strip meters before it).

Auditioning the undo history (`session::compare`,
`Action::AuditionNode(id)` for any step of the tree, `AuditionHistory(done)`
for a place on the current line, the History view's A/B):
`History::project_at_node` makes the project as going to that step would,
on a copy; over the loop, else the edit selection, else 16
bars from two before the playhead (with 2 s of run-up for tails), "Then"
and "Now" are rendered into a per-session temporary folder, measured (EBU
R128 with the master's channel weights), and Now/Then flips instantly,
loudness-matched on request (the gain on the other side only). The
render is bit-for-bit a render after really undoing.

Audio Bisect (`session::bisect`, `Action::Bisect(BisectOp)`, the History
view's bar, Edit → Audio Bisect) searches the undo tree for the edit that
made the mix worse, as `git bisect` searches commits. The user marks a
state that sounded good and one that sounds bad (by default the project
now); the path between them (`History::path_between`: up to the common
ancestor, then down, so the bad state may be on another branch) is a list
of moves, each a step done or undone. A round plays the state in the
middle of what is in question (the candidate) against the last state
judged good, blind: which is A and which B is drawn at random (splitmix64,
`Session::set_bisect_seed` for scripts and tests) and nothing shown tells
until the round is answered; both are loudness-matched by default to the
quieter one (EBU R128 over the stretch heard). "The candidate is better"
makes it good, "the other is better" makes it bad; "Can't tell" asks again
against the first bad state, and "Can't tell" there skips the candidate
(the next is the nearest to the middle not skipped). With no candidate
left the step is found (or, with skipped ones in the way, the few steps it
is among): its label and when it was made, and three ways on — hear its
before and after (named, not blind), take just it back (`inverse_of` as a
new step, or plain undo when it is the last; a step the path undid is
brought back with `again_of`), or go to the state before or after it.

Each state is rendered once per search as an audition is (the stretch
fixed when the search starts; a state rendered for one round serves the
next, and the other end is rendered beside the first comparison in case
of "Can't tell"). A round's two states are joined into one file on a
worker thread — A, then B, each at its fairness gain — and play through
the compare source with `Follow` sync: A ⇄ B is a crossfade between two
regions of one file, in step with the transport, and the disk loader keeps
the side not heard resident too (`DiskLoader::set_compare`'s `also`
offsets). Ctrl+Alt+A flips A ⇄ B while bisecting. The search never touches
the project or the history: cancelling (or another project, or another
comparison taking the compare path) ends it with everything as it was, its
files deleted (renders still running delete theirs when they end), and a
reference compared before chosen again. Tests: `session/tests/bisect.rs`
(a planted bad edit found from a scripted listener who measures the two
regions the compare path plays; "Can't tell" twice skipping; the blind
assignment hidden but kept through flips and re-joins; a good state on
another branch; a step undone on the way brought back; cancel), the view's
`audio_bisect_is_driven_from_its_bar`.

Reference tracks (`faderframe_project::reference`,
`Command::SetReferences`, `session::references`, the Reference view
`faderframe-view-reference`): files imported into the media folder (on no
track, never exported); the chosen one plays along the timeline from a
point or loops its region; a job renders the mix over the same stretch,
matches the loudness (EBU R128) and compares long-term spectra
(`faderframe_analysis::ltas`); Send to EQ Match gives the master's (or a
track's) first EQ the reference's spectrum as EQ Match's reference (a
"reference track" source in the EQ editor). Ctrl+Alt+A flips.

### Codec monitor and platform previews

The master can be heard as listeners get it: through a lossy codec, at a
streaming platform's loudness (Audio → Listen → Codec Monitor, the Tools
view's Codec pane; `session::codec`, `Action::Codec(CodecOp)`). Three
things can be heard, switched with a 20 ms crossfade: the decoded signal,
the original (delayed alike: a lossless tier) and their difference — what
the codec removed or added, alone.

* **Where it sits.** `nodes::codec` inside `ListenOut`, after the compare
  source (a reference or the past is heard through it too) and before
  headphones, the mono check and bass management: it emulates the
  delivered master, which listeners then play on whatever they have.
  Monitoring only — renders build their own engine
  (`renders_never_hear_the_codec_monitor`), the master strip meters before
  it, the analysers read the mix.
* **The chain** (`faderframe-codec`, `CodecChain`): Opus via the `opus`
  crate (the bundled libopus 1.6.1 of the IAMF masters, refused in
  `faderframe_check_only` builds; an identity coder with Opus' framing
  stands in there and in alignment tests), 48 kHz, 20 ms frames,
  complexity 10, 16–510 kb/s a stereo pair (a mono substream half),
  unconstrained VBR or CBR. A bed is coded as mirrored pairs and mono
  substreams (`substreams`), whose frames start at staggered points so a
  7.1.4 bed's encoders work in different callbacks. Other rates go to 48
  kHz and back through two rational polyphase resamplers (Kaiser sincs at
  `V = L·fs = M·48 kHz`, 90 dB, passband to 92 % of the lower Nyquist
  frequency); `RatePlan::delay_for` pads the codec's delay by up to `L − 1`
  samples at 48 kHz so the whole way, `R + (C + pad)·M` samples of `V`, is
  a whole number of project samples. The original is then the input
  delayed by that integer, so at 48 kHz the identity coder's output is the
  input delayed bit for bit, and at 44.1/88.2/96 kHz the difference of the
  round trip is under −80 dB. Cost (release): stereo 1 % of a core at 48
  kHz, 2.6 % at 44.1 kHz, a frame's work about 0.3 ms
  (`chain::tests::codec_cost`).
* **Realtime.** Chains are made on the control thread
  (`CodecLink::poll`, for the layout the newest listening node was built
  for and the format the running node publishes) and handed in through a
  mailbox; the node refuses one made for another format, warms a new one
  up for its delay before crossfading to it (the mix plays meanwhile),
  fades out and hands it back when turned off. Bitrate, VBR, what is heard
  and the platform's gain are atomics (`CodecShared`): libopus' `ctl`s
  change the encoder at its next frame; nothing is made anew. libopus is
  built with variable-length stack arrays, so coding allocates nothing
  (`the_codec_monitor_does_not_allocate`).
* **Delay and the clocks.** While the monitor is heard, what reaches the
  ears is `CodecShared::delay` frames later than the graph made it (1272
  frames, 26.5 ms, at 48 kHz). The graph's latency does not change: no
  rebuild, no plugin delay compensation, MIDI and other outputs as they
  were. Like varispeed's resampler it is counted after the graph:
  `EngineController::listening_delay`/`heard_latency` feed the heard clock
  (`heard_at`, `heard_ahead`, `EngineClock` — the playhead and pictures
  stay with the sound), the meters' and the analysers' hold-back. Turning
  it on plays the last 26 ms again (crossfaded), turning it off skips
  them; switching what is heard moves nothing in time. Recording does not
  make up for it (performers may listen on cue mixes, which never pass
  it): a recording started with the monitor on gets a notice.
* **Measured as it plays** (`CodecMeter`, published every block): the
  true peak of the decoded signal at the platform's gain (BS.1770 4×,
  `faderframe_analysis::TruePeak`), the frames over 0 dBTP, and the energy
  of the difference against the original's (shown since the last reset
  and over the last 3 s). The first 4096 frames of a chain (the
  resamplers' ringing at its abrupt start) are left out; starting playback
  resets them when the Tools view resets on play.
* **Platforms** (`faderframe_codec::PLATFORMS`, published defaults of
  October 2026): Spotify −14 LUFS, quiet masters turned up until their
  peaks reach −1 dBTP, Ogg Vorbis 320 kb/s; Apple Music −16 LUFS, the same
  rule, AAC 256; YouTube −14, never turned up, Opus 160; Tidal −14, never
  up, AAC 320; Amazon Music −14, never up, about 256; Deezer −15, never
  up, MP3 320 CBR. Vorbis, AAC and MP3 have no encoder under a permissive
  licence: Opus at the same bitrate stands in, and the menus, the pane and
  the report say "emulated". The platform's gain (`Platform::gain`) comes
  from the song's integrated loudness and true peak, measured by a render
  of the master over the export's range (loop off, 2 s of tail; two DSP
  helpers) once the project has stood still for 1.5 s while a platform is
  chosen, kept for eight states by the mix fingerprint (undo back to a
  measured state takes its measurement at once; while a change is
  measured, the gain of the state before it stays). Until a project's
  first render is done the Tools view's running measurement of the master
  stands in ("from what played"), else 0 dB. The gain goes to the node in tenths of a decibel; a
  measured gain arriving resets the measurements.
* **Preview on platforms** (the Render window's button, with its range,
  rate, channels and finish; Codec Monitor → Preview on Platforms… with
  the project as it is): the master rendered to a temporary float file,
  read back in pieces, measured (BS.1770 with the layout's weights), then
  coded and decoded per platform on threads of their own
  (`faderframe_codec::preview`), each measured at the platform's gain and
  at unity (normalisation off). The report gives the gain (and whether
  turning up stopped at the peaks), the true peak decoded both ways, the
  frames over full scale, the artefacts' level, a clipping risk (over 0,
  within 1 dB, safe) and advice: a ceiling low enough for −1 dBTP after
  decoding, a master turned down by more than 2 dB, one that a platform
  leaves quiet, loud artefacts, the stand-in codec.

Tests: the crate's (`at_48_khz_the_identity_coder_is_the_input_delayed_exactly`,
`at_other_rates_the_decoded_and_the_original_line_up`,
`opus_is_heard_in_step_with_the_original`,
`decoding_a_hot_master_finds_its_overs_and_says_to_lower_the_ceiling`),
engine `tests/codec_monitor.rs` (frame-exact switching, no clicks, the
44.1 kHz null, Opus measured, overs at +6 dB), `realtime_alloc`'s
`the_codec_monitor_does_not_allocate` (48 and 44.1 kHz), session
`tests/codec_monitor.rs` (switching, the gain from the song's render, the
fingerprint cache, renders untouched, the preview), the Tools view's
`the_codec_pane_shows_while_the_monitor_is_on`.

### Mix scenes

A mix scene (`faderframe_project::scenes`, `Project::mix_scenes`) is a
snapshot of the console: per track, by id (`TrackMix`), the fader, the
pan (mono pan or balance, the pan mode, dual pan, the surround panner),
mute, solo, polarity, the track delay, every send's level, pan and
switch, every cue send, each device's bypass — and, when the scene keeps
them, every device's automatable parameter values and its state blob —
and the console's drive (`SceneMix`). Each scene has a recall scope
(`SceneScope`, a switch per `ScopePart`), tracks it never touches
(`safe`) and optionally a recall of its own (`SceneFade`: a
`FadeLength` — instant, milliseconds or beats at the tempo where the
playhead is — and a `SwitchPoint`); the project has a default recall and
the scenes A and B. Tracks added after a scene was stored are not in it
and stay as they are; what has gone since (tracks, sends, cue mixes,
devices) is passed over. Commands: `SetMixScene { scene, value, index }`
stores, replaces (update, rename, scope, safe tracks), moves and deletes
one scene (its inverse puts it back where it was; A or B naming a deleted
scene come back with it), `SetMixSceneOptions` the default recall and
A/B; both `Impact::None`. Show mode's songs may name a scene
(`SetSong::scene`), recalled when the show stands on the song (also when
it plays straight on into it).

**A recall is ordinary edits.** `scenes::commands_to(project, target,
scope, safe, now)` turns a scene into the mixer commands that set the
project to it — only what differs, nothing a command would refuse (a delay
on the master, a send switched on into a loop, a cue send from a track
that feeds none, the drive without a console); `now` gives where a device
parameter stands when the project has no explicit value (only the plugin
knows). So a recall is one undo step, every view sees the faders where
they are, and the engine syncs as it does for a hand on the console. A
scene with devices' states loads a state that differs (with the values
stored beside it) and sets the parameters that differ. Frozen tracks'
devices are left alone.

**Crossfades, morphs and compares move unrecorded.** `scenes::blend(a, b,
t, switch, glide)` is the console `t` of the way from one scene to
another: faders and send levels in fader travel (as a motorised fader
moves), pans, dual pans, surround positions and the drive linearly,
devices' continuous parameters linearly or geometrically (frequencies),
and everything switched — mute, solo, polarity, delay, pan mode, the
surround panner's modes, send and cue switches, bypass, stepped
parameters, states — at the switch point (start: as soon as it moves,
middle, end); at 1 it is exactly the destination. The session
(`session::scenes`) applies each tick's blend with its inverses kept —
the first per parameter, as a gesture coalesces — and the last command
per parameter; when the work ends it takes them back and applies those
last commands as one batch, so the step holds exactly where the console
ended and undoing it returns to where the work began. A crossfade runs on
the session's clock (`tick_scenes`) and commits at its end; a morph
(`SceneOp::Morph`, the Scenes view's slider, a controller) commits on
`EndMorph` or when a controller has rested `MORPH_REST`; a compare
(`CompareStart`, `CompareHear`, `CompareEnd { keep }`) hears scene B, or
A — another scene or the mix as it was — each side recalled over the mix
as it was, and ends back at the mix (nothing recorded: unsaved mix
changes are kept, the mix itself is the scratch) or keeping the side
heard. Any action that edits the project first settles the work
(`scenes_before` in `dispatch`, `scenes_before_edit` in `edit`, except
edits of automation lanes and of nothing heard): a crossfade stops where
it is ("(Stopped)", one step), a morph ends where it stands, a compare
keeps the side heard. Saving and rendering settle it too; a new project
forgets it.

**Zipper-free: the sound glides on the audio thread.** The strips and
sends ramp linearly across each block to the slot value they read, so
per-tick slot writes alone give a staircase at the tick rate (ramps of a
block, flats between). While a stream runs, a crossfade's continuous
values therefore glide in the engine: `EngineController::glide_mix`
writes the target project's parameters into a scratch `ParamTable` with
the same `SlotRegistry::write_params`, keeps the continuous slots that
differ (`SlotRegistry::glidable`: faders, pans, dual pans, surround
positions, VCA gains, send levels and pans, cue sends, container chains,
the drive — never NaN-valued slots like an unpanned send's) and sends an
`engine::glide::Glide` (`Message::Glide`). The audio thread takes each
slot's start where it is, and before every chunk writes every slot's
value for the chunk's end — gains in fader travel — so the per-block
ramps join into one straight line; at the end it writes the exact targets
and hands the glide back (`Garbage::Glide`, `glide_done`). Until the
session lets go (`release_glide`: at once for a stop, else once the glide
is done — the project holds the same values by then), `write_params`
skips the gliding slots, so the ticks' syncs never fight it. A new glide
takes over the slots a running one holds from where they are. Morph steps
and compare switches glide over 30 ms. Switched values change through the
project at the switch point as ordinary syncs (mutes ramp over a block, a
bypass rebuilds the graph). Without a stream, or after the engine was
made again, the ticks alone move the values (each tick's slot writes, as
a fader drag's). Devices' continuous parameters move per tick, as knob
moves do (plugins smooth their own). Realtime-safe: the glide is made on
the control thread (its fader-law table too), taken and finished without
allocation (`mix_scene_glides_do_not_allocate`).

**Automation.** A recall moves the mix as the controls would: a parameter
a lane drives in Read keeps following its lane (the recall sets the
static value under it, heard where the lane no longer drives); while
playing, lanes in Touch, Latch or Write record the recall as they record a
control (`capture_automation` on every command applied, a crossfade tick
by tick, so the written curve glides as heard); Touch lets go when the
recall ends, Latch and Write write on until playback stops.

**Control.** MIDI learn: `MappingTarget::MixScene { control:
SceneControl::{Recall { scene }, Next, Previous, Morph} }` (buttons on
presses; the morph a value, relative encoders too). Control surfaces:
`SurfaceCommand::{SceneNext, ScenePrevious, RecallScene(n),
SceneMorph { permille }}`, from OSC `/scene/next`, `/scene/previous`,
`/scene/recall i`, `/scene/morph f`. Next and Previous go on from the
scene recalled last.

**UI.** The Scenes view (`faderframe-view-scenes`,
`ViewKind::MixScenes`, `app.show-mix-scenes`, View → Mix Scenes, a tab
beside the mixer in the Mixing workspace): store (with or without the
plugins), update, recall (Stop while fading), previous/next, the default
crossfade and switch point (a menu of lengths; a double- or right-click
types one: "350 ms", "2 s", "4 beats"); the list (double-click recalls,
F2 renames, a menu for the rest: recall at once, rename, update with or
without plugins, the scene's own crossfade, A/B, compare with the mix,
move, MIDI learn, delete; a crossfade's progress runs along its row); the
chosen scene's scope and safe tracks as chips and its own recall; A and
B, the compare halves with Keep and Back, and the morph slider. The
mixer's strip menu has a Mix Scenes submenu (`Session::mix_scene_menu`:
store, recall each, the track safe from each, the view). Dev action
`mix-scene:<store=name|store-plugins=name|recall=n|crossfade=n@ms|
morph=a/b@pos|compare=a/b|compare=flip|keep|back|next|previous|stop|
delete=n>` (n: the place in the list from 1, or the name; `compare=mix/b`).

Tests: project `scenes::tests` (commands and inverses, old files load,
recall within scope and safe tracks, nothing impossible asked, blends
glide and switch), engine `glide::tests` and `tests/glide.rs` (one
unbroken line in fader travel, exact targets, slots held until released,
a stop stays), `mix_scene_glides_do_not_allocate`, session
`tests/mix_scenes.rs` (a recall in one step, scope and safe tracks, a
crossfade reaching its targets after its time and moving steadily with
and without a stream, an edit stopping it, a morph and a compare in one
step or none, next/previous, devices' values, a song's scene), the view's
tests, the mixer's `the_strip_menu_stores_and_recalls_mix_scenes`, OSC
`mix_scenes_are_recalled_and_morphed`.

### Mix Intelligence

Mix Intelligence analyses the mix deterministically: no models, no
network; the same audio and settings give the same findings (`ALGORITHM_VERSION`
and FNV fingerprints keep ids and cache keys stable). Three layers:

* `faderframe-mix-intelligence` (pure, on `faderframe-analysis`) measures
  rendered audio: Hann frames of about 0.17 s at half overlap summed into
  third octaves (IEC 61260 centres, 25 Hz–20 kHz) as mean-square power
  (one-sided, Parseval-scaled: a full-scale sine reads −3.01 dB), per frame
  and band the left/right levels and their correlation, plus `FileMeter`
  levels (sample and 4× true peak, RMS, overs) and BS.1770 loudness by
  section. Explicit rules (`config.rs` thresholds, each documented as a
  heuristic; `RuleId::{method, limitations}`) turn these into findings
  with evidence in units, severity and confidence kept apart (each shown
  with the terms it came from), caveats and conservative experiments.
  *Spectral Masking Radar*: a target band counts while it is prominent in
  its frame; another track covers it by how close its level comes (fully
  at the target's level or above), weighted by how alike their stereo
  placements are; covered bands group into ranges of up to two octaves and
  into events (gaps up to 1 s merged, 1 s at least), persistent,
  recurring or isolated; severity = role weight × √persistence × depth ×
  importance, confidence = signal × roles × amount × level × resolution.
  Track roles (`Track::role`, `Command::SetTrackRole`, set only by the
  user; names are never taken for roles) weigh the pair (drum-kit pairs
  are not compared, kick/bass and foreground targets weigh most).
  *Levels*: master true peak over −1 dBTP, overs, a track over full scale
  alone, non-finite samples, adjacent sections 8 LU apart. *Stereo*:
  fold-down loss against the sum without cancellation (`(√L+√R)²/4`: a
  panned source loses nothing, uncorrelated channels 3 dB), an inverted
  channel.
* `faderframe_session::mix` analyses on a thread of its own. *Processed*
  analyses render the whole mix **once** (`faderframe_engine::capture`):
  a tap on every track, bus and return analysed hears it where it reaches
  its destination — taps are `NodeRole::Heard` sinks, so the compiler
  aligns them with the device outputs and the render takes the graph's
  latency off the front of all of them alike (no second delay
  compensation); bus and master dynamics react to the whole mix. A probe
  after every device compares its output with its input (aligned by the
  graph), and the engine's node timing gives each device's cost; the
  master is the device output. Profiles are built beside the render on at
  most 8 threads (`mix::measure`, bounded chunks). *Source* analyses render
  each track's clips before any device, side by side. Fingerprints are per
  track (`mix::fingerprint`: the upstream closure — what is routed, sent,
  keyed or modulated into it — plus the global settings; nothing downstream
  changes a tap), so an edit re-measures what it reaches and makes stale
  only the findings about those tracks (`Session::mix_finding_stale`); a
  superseded or replaced job's results are dropped. The signals a render
  measures are held (`mix::held`: 16 bits with a scale per 1024 frames, up
  to 512 MB in a job, 192 MB kept between analyses under the profiles'
  keys) so the pairs read their excerpts there, never rendering the mix a
  second time. After a change the tracks it reached get a *first look*:
  a render of only what reaches them (`capture::pruned` — every other
  track's clips, devices, sends and automation taken out; taps bit-identical
  to the whole mix's, test `a_pruned_render_taps_what_the_whole_mix_taps`)
  cut into segments rendered side by side, each with a 2 s run-up
  (`render_look`; one piece when a third-party plugin is involved), shown
  with their pairs while the master and buses show as last measured
  (`MixResults::{provisional, pairs}`); the whole render then measures the
  changed tracks again exactly (the first look is never cached). Health
  checks are kept for the last 8 project fingerprints. Measured on the demo
  (release): 20.1 s before the one pass, 1.7 s now; the demo with every
  track four times: 54.4 s before, 2.5 s now; analysed again unchanged:
  0.13 s; an experiment applied: its track shown again after 0.47 s,
  everything after 1.95 s (the one heavy chain — a preamp, a saturator
  and a compressor in series, 0.9 s on one thread — bounds the whole
  render). The first stage's findings show while the second runs
  (`MixResults::complete`). Experiments are auditioned
  by applying them unrecorded with their inverses kept (a built-in EQ
  appended to the masker — keyed from the target for a dynamic cut — or a
  fader): A/B by the band's bypass or the fader's two values, loudness
  matched by an offline BS.1770 measurement and the mix side of the one
  listening gain (`EngineController::set_listening_gain`,
  `Session::listening_gain`; the real level change and the fairness gain
  shown apart). Anything that edits, undoes, saves or renders takes the candidate
  out first; Apply is one undo step ("Mix Intelligence: …") and starts the
  analysis again by itself (same range and tracks). History steps have
  ids (`History::{last_step, is_done, knows, inverse_of}`): an applied
  experiment's "Applied" — "Resolved" when the analysis after it no longer
  finds the finding, which stays listed — follows its step through undo
  and redo (both analyse again); its Undo (`MixOp::Undo(step)`) undoes the
  step when it is the last, else applies its inverse as a step of its own
  ("Undo Mix Intelligence: …", later edits kept; refused whole when a later
  step removed what it changed).
* `faderframe-view-mix` (`ViewKind::MixIntelligence`, `app.show-mix`): the
  ranked findings (filtered, sorted), each one's measurements (clickable
  events: the tracks selected and the range shown), a band chart,
  interpretation, method, limitations and experiments; levels; roles.

**Phase, timing, width, dynamics, same audio** (a second stage,
`session::mix::extra`, for what needs more than one profile per track):

* *Pairs* (`phase::candidates`): the selection, roles that share a source
  (kit tracks against the loudest "Drums" track, same-role groups against
  their loudest), same-audio pairs, kick and bass, masking pairs, pairs
  whose levels rise and fall together — at most 12, never a track against
  a bus it reaches; their excerpts (up to 12 s, the busiest stretch both
  play in) tapped in one render. `phase::measure_pair`: Auto-Align's lag
  and polarity, per band coherence, phase and the residual phase, the sum
  against the energies (now, corrected, flipped, in mono, onset-aligned),
  onset pairing. Rules `PolarityInverted` (a clear peak), `TimeOffset`
  (comb filtering from 0.05 to 15 ms; longer offsets are musical — no
  delay suggested), `FrequencyDependentPhase` (cancellation the correction
  leaves; a manual all-pass suggestion), `OnsetOffset`/`LooseTiming`;
  `Experiment::Alignment` sets the track's delay and polarity (the bass
  moves first; leads and the kit's reference keep their place).
* *Width* (`width`): side against mid and the mono change per band and
  region; `WideLowEnd`, `MonoBalanceShift` (naming the side-signal
  contributors); low correlation is never a fault in itself.
* *Dynamics* (`dynamics`): profiles carry a 5 ms envelope and BS.1770
  momentary/short-term loudness every 100 ms; crest, PLR, LRA, short-term
  percentiles, transient-to-sustain, attack rise; `TransientFlattening`
  ("possible": the processed track against its source), `WideLoudnessRange`
  and `LevelJump` judged against the mix.
* *Same audio* (`same`): copies of one source — panned, at another gain,
  through an EQ, a little later, a stereo recording split in two, one
  source processed two ways — are not masking. Profiles keep complex band
  sums per frame of the mono fold-down and, stereo, of each channel;
  per-band coherence in 4 s windows (delays up to 1 ms allowed); a window
  is the same audio when the fold-downs reach the threshold (≥ 0.85
  weighted and plain over at least 2 bands) or left against left and right
  against right do (copies panned apart through a wide source, a stereo
  chorus: their fold-downs differ, their channels do not); windows with
  too little to judge (a fade, a clip's ends) take the verdict of the
  nearest judged one. Those frames are left out of the masking rule
  (`conflicts_except`) and reported as `SameAudio` (the copies' level, their
  mono sum: +6 dB for equal copies in phase). Two performances of a part
  are different audio and still judged.

**Health** (`health`): DC offset per track, bus or master (the Utility's
DC filter); overs and flattened peaks between devices (gain staging around
the device); the first device producing invalid samples; a device that
silences its input; a device that changes nothing, with its measured cost
(bypass it); bypassed devices. **Reference** (`reference`): against a
reference track (`Project::references`), loudness-matched — loudness,
spectral differences, section balance, stereo and mono — handed to A/B
listening and EQ Match; a comparison target, never a standard.

`ALGORITHM_VERSION` 5. Limits: same audio misses copies more than about
1 ms apart or through nonlinear or time-varying processing; pair excerpts
read from held signals carry the whole render's state, rendered ones (past
the memory share) start 1 s early (longer tails and a compressor's earlier
state are missing, so a pair can measure a little differently then); a
surround tap is measured on its first two channels; device
cost is timed offline (a guide); the master depends on everything, so any
audio change renders the mix once more after the first look (a change of
only the master renders the whole mix at once); a first look differs from
the whole render where a device remembers longer than its 2 s run-up (it
is replaced, never kept); held signals cover a job's range only while they
fit 512 MB (beyond, the pairs' excerpts are rendered, pruned to the tracks
they need).

### Video frames on the GPU (zero-copy display)

Playback frames can reach GTK without coming back through memory
(`faderframe_video::zero_copy`, Preferences → Video → "Show frames
straight from the decoder"). The decoder's output is a frame in video
memory (`Frame::gpu`), which the view draws with `Painter::external` and
the shell's painter turns into a GTK texture; anything that fails says
so once (`zero_copy::failed`) and frames come as RGBA from then on, as
they always do for stills, thumbnails, proxies being made and HDR
pictures (those are mapped on the CPU).

* **Linux**: VA-API decodes and its post-processor scales into an RGB
  dmabuf in a format the display imports (`set_display_formats` from
  `GdkDisplay::dmabuf_formats`), handed over as a `GdkDmabufTexture`.
* **Windows** (UNTESTED: nothing that built it could run Direct3D 12 or
  its sharing; written against GStreamer 1.26's d3d12 library and GTK
  4.20's `GdkD3D12Texture` as documented, checked by the Windows
  cross-build only). `Decoder::open_d3d12`: whatever decodes the picture,
  GStreamer's `d3d12convert` scales it and converts it to RGBA in a
  texture (caps `video/x-raw(memory:D3D12Memory), format=RGBA`); its own
  pool makes them on a shared heap (`D3D12_HEAP_FLAG_SHARED`, GStreamer's
  default for a filter's pool), which every import below needs. The frame
  keeps its `gst::Buffer` (`zero_copy::GpuFrame`, `memory()` = its
  `GstD3D12Memory`, `read_back()` = its pixels through memory). The shell
  (`faderframe-ui/src/video_d3d12.rs`) calls GStreamer's d3d12 library,
  looked up at run time where the running GStreamer has it
  (`libgstd3d12-1.0-0.dll` or `gstd3d12-1.0-0.dll`; missing — GStreamer
  before 1.26 — and the preference has no effect: `available()`,
  `set_d3d12_display`): `gst_d3d12_memory_sync` waits on the CPU for the
  converter's work (the frames are decoded ahead, so this is a check, and
  GTK is handed no fence), `gst_d3d12_memory_get_resource_handle` gives the
  `ID3D12Resource`, whose layout must be RGBA8, the frame's size, one mip.
  Under GTK's GL renderer (its default on Windows) the texture is imported
  into a GL context of GTK's display through `EXT_memory_object_win32`,
  as the GPU painter's frames are (`gpu_win32::GlImport`, generalised to a
  `Shared` descriptor): the memory's NT handle
  (`gst_d3d12_memory_get_nt_handle`, made once and owned by it), the
  allocation's size from the texture's device, only when GL draws on the
  adapter GStreamer decodes on; each pooled texture is imported once, known
  by an id of ours kept on GStreamer's memory
  (`gst_d3d12_memory_set_token_data` under a token of
  `gst_d3d12_create_user_token`: never reused, so a texture made later is
  never taken for one imported before), 64 kept. Under Vulkan it is a
  `GdkD3D12Texture` GTK imports itself (`gpu_win32::d3d12_texture_keeping`).
  Either way the texture's release keeps a clone of the frame, so its
  texture stays out of the converter's pool while GTK shows it, and the
  first frame is read back both ways (GStreamer's copy, what GL or GTK
  sees) and compared, as the painter's first frame is. Under a renderer
  that imports nothing (Cairo) the first frame fails and frames come as
  RGBA. `FADERFRAME_VIDEO_D3D12=0` turns it off.

### A/V calibration and panning that follows the picture

**A/V calibration by itself** (`session::av_calibration`,
`faderframe_video::{camera, calibration}`, `faderframe_engine::input_tap`):
a camera watches the video window flash and a microphone hears the beeps;
the picture offset (`Video::offset_ms`) is set from how much later the
picture came than the sound — the screen's own delay (compositor, the
monitor's processing) and the speakers' included, as they should be.

* *The test.* The flash-and-beep test is played by the session, not from
  files in the project: from the playhead (past the loop when the loop
  would wrap), a white flash of 240 ms (six frames at 25 fps: frames
  fully inside it give its brightness) every second from a second in, a
  1 kHz beep of 40 ms with each. The video window paints the flashes
  (`Session::av_calibration_screen`) by `Session::picture_position` — the
  one function the picture is timed by too (the heard position when the
  frame reaches the screen, less the offset). The beeps play through the
  A/B compare source in the mix's place (`compare::hear_instead`,
  `CompareSync::Follow` from the first flash: in step with what is heard
  by construction). Nothing in the project changes until the end; the
  offset is one undo step, "Calibrate Picture Offset". Calibration is
  refused while A/B listening compares something.
* *The two clocks.* A capture pipeline runs on a monotonic
  `GstSystemClock` of its own (`use_clock`); a live source stamps buffers
  with their capture time as running time (v4l2src takes the driver's
  monotonic stamp — UVC: start of exposure — else the arrival less a
  frame), so base time + running time is the exposure start on that
  clock. That clock and `Instant` (the heard clock's time base,
  `history::ns_since_epoch`) are related by reading both back to back
  (`camera::ClockMap`: the closest of sixteen pairs); both are the
  system's monotonic clock on every platform, so the relation does not
  drift. Frames without a time are dropped and counted. The microphone is
  a device input tapped on the audio thread before anything touches it
  (`EngineController::tap_input`: rtrb rings of samples and `TapBlock`
  headers stamped with the callback's start on that time base, whatever
  the transport does; a full ring is counted as lost and keeps its place;
  the ends go back as garbage); a block's input came in over the
  callback's own frames before the callback, and the device's reported
  input latency before that (devices report their latencies without
  their buffer — the engine adds the buffer to the output's alike).
  Sample times come from a straight line through the stamps (late
  wake-ups left out).
* *Flashes, sub-frame* (`calibration::flashes`). Frames are measured as a
  16 × 12 grid of mean brightness; cells that flash are the screen. A
  frame integrates light over its exposure, so the frame a flash starts
  in is partly bright: its brightness over the flash's own (frames fully
  inside it) is the share of its exposure that saw the flash, which
  started that share before the exposure ended (the stamp is taken as the
  first row's exposure start, the exposure as the frame period). Each row
  band is timed on its own and the onsets fitted with a line down the
  picture (one slope for all flashes: a rolling shutter's readout against
  the display's scan-out), read at the screen's middle row.
* *Beeps, sub-millisecond* (`calibration::beeps`): the input demodulated
  at 1 kHz and summed over the beep's length (a matched filter for a tone
  burst) rises in a straight line from the beep's first sample; the line
  through the first 4 ms of the rise (the direct sound, before
  reflections steepen it) meets the noise floor at the start.
* *The estimate* (`calibration::estimate`): every flash paired with the
  beep nearest it (450 ms at most); the median of the differences, then
  the mean of those within three robust deviations (at least 3 ms); the
  spread is their standard deviation. At least half the flashes (and 3)
  must pair. The new offset is the old less the lateness, rounded to
  0.1 ms (refused past ±1 s).

What stays in the result are the sensors' own: the camera's stamp against
its exposure, a rolling shutter's readout down to the screen's middle row
(it cannot be told from the display's scan-out from one picture), the
microphone's latency as the device reports it. A virtual camera
(`camera::Scene`, `VirtualCamera`; `CameraChoice::Virtual { display_ms }`)
films the screen as the video window would show it, `display_ms` late,
integrating its light over each row band's exposure and pushing the frames
through the same pipeline stamped on its clock: the tests and a self-test
without a camera. Cameras are listed by GStreamer's device monitor
("Video/Source": v4l2, PipeWire, AVFoundation, Media Foundation); a
camera is opened on a thread (it can take seconds), preferring 25 fps or
more.

Tests: `faderframe-video` `calibration::tests` (global and rolling
shutters, the short sync-test flash, beeps with an echo and jitter, the
estimate's outliers), `tests/camera.rs` (a virtual camera through a real
pipeline: flashes come out when they happened, a rolling shutter as the
slope), engine `tests/input_tap.rs`, `tapping_an_input_does_not_allocate`,
session `tests/av_calibration.rs` (the dummy device's patch cable and a
virtual camera 30 ms late: the offset within 2.5 ms, one undo step, a
second run leaves less than the spread; a silent input fails and leaves
the offset).

**Panning that follows the picture** (`session::object_tracking`,
`faderframe_video::track`): an object boxed on a frame of a video clip
is followed through the clip and becomes automation of what places a
track.

* *The tracker* (`track::Tracker`, deterministic): normalised
  cross-correlation of a template on a pyramid level where the box is
  about 40 pixels across, looked for around the last place plus the
  speed at three sizes (±4 %), every other position first, then around
  the best; the peak refined by parabolas (place and size). A good match
  (≥ 0.8) blends a tenth of what it found into the template, pulled back
  a fiftieth towards the key's picture (no drift onto the background); a
  poor one (< 0.45) is lost: the box coasts on at a slowing speed, marked
  so a key can put it right there. Frames are decoded in order at
  480 × 270 at most on a video job, from the proxy when there is one
  (`track::follow_in_file`; the proxy's times count from its first frame,
  as the frame service reads them).
* *Keys.* The first box starts it (`FollowOp::Key`); another is a
  correction: the stretch before keeps what was followed up to it, bent
  onto the new box (`track::bend`: the error spread over the stretch,
  none at its own key), and the object is followed afresh from it to the
  next key or the clip's end. A key taken back (`RemoveKey`) lets the
  stretch before follow on through. Keys and path are the session's,
  forgotten with another project.
* *Applied* (`FollowOp::Apply`, at once or as soon as it is followed
  through): every frame within the clip, smoothed over five frames, a
  point at the frame's start; the picture's horizontal place −1…1 drives
  the track's pan (balance), both sides of a dual pan (spread kept), its
  room X in a surround bed or as an object (`AutomationTarget::Surround`),
  or its VBAP azimuth (±30° across the picture) — `Session::follow_target`
  decides; with depth a room-panned track moves front to back by the box's
  size (`depth` per doubling against the first key's). Points thinned
  (`faderframe_automation::thin`) replace the stretch of the lanes the
  path covers (`AutomationCurve::replace_range`), new lanes made where
  there are none, all one batch "Follow Object".

The video window: a Shift+drag on the picture, or a drag with "Follow an
Object" on, draws a key; the box is drawn over the picture (the accent for
a key, the selection colour followed, the console meters' red lost), and the menu
takes a key back, applies to a track ("Pan a Track with It") or forgets
it. Tests: `track::tests` (sub-pixel moves within half a pixel —
0.08 px mean measured —, an object coming nearer, hidden and found
again, the bend), session `tests/object_tracking.rs` (a textured square
across a clip: pan within 0.02 of where it is, one undo step, a
correction and taking it back; a surround bed's X and Y with depth),
the video view's tests (the box drag, the menus, the calibration screen).

### 360° video

A spherical (360°) picture is looked into, not shown flat: the video
window is a viewport into the sphere, the listener's head on headphones
turns with it, and a click on the picture pans a track to the direction it
shows there. Picture and sound share one frame — the ambisonic frame of
`faderframe_core::ambisonics` (`x` front, `y` left, `z` up; azimuth
positive to the left, elevation up) — so a track panned to a direction
(an ambisonic field's encoder, a bed's VBAP panner) sounds where the
picture shows that direction.

* *Layouts* (`faderframe_core::spherical`, pure). `Spherical {
  projection, stereo, pose }`: equirectangular (the front in the middle,
  the right to the right of the frame, up at the top; `Crop` = Spherical
  Video V2's `equi` bounds, shares of the whole projection cut from each
  edge) or the V2 cubemap layout 0 (a 3×2 grid: right, left, up over
  down, front, back; the four round the viewer upright, the up face's
  top forwards, the down face's to the back; `padding` a share of a
  face's width); stereo frames show the left eye; the pose turns the
  projection in the scene (yaw left, pitch up, roll to the right, as an
  `Orientation`). `Mapping` turns scene directions into a frame's pixels
  and back; `Viewport` is a gnomonic view with a horizontal field of view
  (20–150°) looking where an `Orientation` turns it; `dragged` turns a
  view so what was under the pointer stays under it. `VideoSource::
  spherical` holds a video's layout (from its file, or as the user marked
  it — one undoable `SetVideo`).
* *Metadata in files* (`faderframe_video::spherical`). GStreamer reads
  and writes none of it, so the boxes are handled here. Read when a file
  is probed (`MediaInfo::spherical`, `AudioStream::ambisonic`): Spherical
  Video V2 (`st3d`, `sv3d` → `svhd`, `proj` → `prhd`, `equi`/`cbmp` in
  the picture's sample entry; a mesh projection is not shown as 360°),
  V1 (the `uuid` box of XML in the track, its cropped area) and spatial
  audio (`SA3D` in a sound's sample entry: an AmbiX field when
  periphonic, ACN, SN3D, its channels in order without head-locked
  stereo — `SpatialAudio::field`); Matroska's `Projection` and
  `StereoMode` and V1's `spherical-video` tag. Written into MP4 and
  QuickTime movies (`write`): `moov` is read and rewritten with V2's boxes
  before the entry's `clap`/`pasp`, V1's XML at the track's end (V1 has
  no cubemap and no right-left stereo) and `SA3D` at the end of the first
  sound's entry — what was there replaced, not doubled; QuickTime's sound
  description versions 1 and 2 are told by the major brand. The media is
  copied as it is; the chunk offsets (`stco`, `co64`) of everything stored
  after `moov` move by the bytes it grew (a 32-bit offset that would
  overflow refuses; fragmented movies are refused). An Opus sound marked
  as a field also gets channel mapping family 2 (RFC 8486) in its `dOps`
  — GStreamer's encoder writes the discrete family 255 for unplaced
  channels, whose table family 2 reads as ACN components.
* *Viewing* (`faderframe_video::reproject`, `faderframe-view-video`
  `sphere.rs`). A frame is looked into on the CPU: where each view pixel
  looks is worked out exactly every 8 pixels (`GRID`) and interpolated
  between — cells near a pole, across the seam, between cube faces or
  past a crop are worked out pixel by pixel — and the frame sampled
  bilinearly (two channels at a time in a `u32`; `floorf` avoided, it is
  a call without SSE 4.1); views over 200 000 pixels are drawn in bands on
  up to four scoped threads. The view is drawn at the window's device
  pixels, 1.3 MP at most (`MAX_PIXELS`, scaled up beyond), and kept until
  the frame, the look, the field of view or the size changes. Frames of a
  360° picture are decoded as wide as keeps the view sharp
  (`Spherical::width_for`, at most the file's own size and
  `sphere::MAX_DECODE_WIDTH` 5760) and always into memory
  (`service::Media::memory`: no dmabuf frames, which the CPU cannot look
  into). The GPU painter is not used: GSK has no custom shaders and vello
  no arbitrary resampling. Measured (`reprojection_cost`, ignored; a
  4K frame, dev profile on this crate's opt-level 3, a machine loaded by
  other builds): 12 ms a 1280×720 view on four threads.
* *The viewport and the head* (`session::sphere`). `SphereState` (the
  session's, never saved): field of view, linked, its own look, the pan
  track. Linked (the default) the viewport *is* the listener's head
  (`Session::head_orientation`): `SphereOp::Look` points the head
  (`Session::point_head` — with a tracker it moves the tracker's
  reference so the head points there), and a tracker's OSC turns the
  picture; the head moving asks the views to draw again
  (`shown_changed` in `apply_head`). The binaural renderer turns the
  scene the other way, so sound and picture turn together; speakers never
  turn. Unlinked, the picture looks on its own; linking again gives the
  head the picture's way. View ops (`Look`, `Fov`, `Ahead`, `Link`,
  `PanTrack`) are no edits: Mix Intelligence auditions and mix-scene work
  go on through them (`SphereOp::edits`).
* *Panning by the picture.* `Session::sphere_pannable`: a track panned
  into an ambisonic field or a bed whose azimuth applies (VBAP; not the
  room panner, not a field or bed into a field). `SphereOp::PanTo` sets
  azimuth (and elevation where it applies, within a bed's range) through
  `Session::edit` — the panner's own path, so lanes in Touch, Latch or
  Write record it while playing; the view sends a gesture (one undo step a
  drag). `SphereOp::WriteAutomation` makes the direction lanes (one step)
  and sets them to Touch, or back to Read. The tracks panned by direction
  are marked on the picture where they sound (the chosen one in the
  accent colour, "● writing" while its lanes are written).
* *Import and export.* A movie's first sound marked as a plain AmbiX field
  imports as a track of that order. Export Movie keeps the picture's 360°
  layout and, when the master is an ambisonic field, writes its first
  order (W, Y, Z, X — the first four AmbiX channels) marked with `SA3D`, as
  YouTube takes it: QuickTime with 24-bit PCM or MP4 with Opus. Matroska
  carries neither mark (a notice says so) and keeps every channel. Sounds
  of more than two channels now go into QuickTime as they are, unplaced
  (qtmux takes no more than two placed channels: they were folded down to
  two before), as do fields and anything past eight channels (more than
  GStreamer has default places for) — through `appsrc` from the WAV's
  data, since nothing converts placed channels to unplaced ones.

The video window: dragging a 360° picture looks round, the wheel narrows
or widens it; with a track chosen in "Pan by Clicking", a click or drag
pans it (Alt+drag still looks); the menu's "360° Picture" marks the
layout (flat, equirectangular, cubemap, stereo modes), then "Look
Straight Ahead", "Field of View", "Turn the Sound with the Picture",
"Pan by Clicking" and "Write <track>'s Pan Automation (Touch)". Following
an object is offered on flat pictures only. For the keyboard the picture
has three sliders more (which way, how far up, how wide; the arrows turn
by ten degrees).

Tests: core `spherical::tests` (known directions on known pixels, crops,
the cube's faces and their turns, the pose, stereo, viewports and drags),
`faderframe-video` `spherical::tests` (written and read back into
QuickTime versions 0–2 and MP4, offsets moved and media untouched,
written twice replaced, an index at the end, Matroska, V1's XML, `SA3D`)
and `reproject::tests` (the view looks where turned, the seam, the
interpolation against exact directions, cubemaps, stereo, bands),
`tests/spherical.rs` (GStreamer-made movies muxed with the marks: probed
back, decoded, the sound out channel for channel, looked into; with
`ffprobe` installed FFmpeg reads "Spherical Mapping" and "ambisonic 1";
MP4/Opus with family 2; six channels kept in QuickTime and Matroska),
session `tests/video_360.rs` (a 360° camera's movie imports as a
spherical picture and an AmbiX track; the view is the head, turned by
hand and by OSC; clicks pan and are written as automation while playing;
the movie written out keeps the marks and the master's first order), the
video view's `sphere::tests` (drags, the wheel, a click's pan as one
gesture, the menu, a frame drawn and kept, the overlay, the keyboard).

Not done: a mesh projection (V2's `mshp`) and YouTube's equi-angular
cubemap; writing the marks into Matroska (its `Tracks` would grow and the
clusters' and cues' positions move); following an object on a 360°
picture (its boxes are flat); head-locked stereo beside the field.
