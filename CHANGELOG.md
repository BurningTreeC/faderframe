# Changelog

What changed in each FaderFrame release, newest first. Generated from the commit history by `scripts/changelog.py`; the downloads are on the [releases page](https://github.com/BurningTreeC/faderframe/releases).

## [0.47.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.47.0) — 2026-11-13

- **Takes are clips: a take keeps every edit, and each clip editor works on one take ([#67](https://github.com/BurningTreeC/faderframe/issues/67))**

  <details><summary>Details</summary>

  [#67](https://github.com/BurningTreeC/faderframe/issues/67): folding a clip into a take folder dropped its edits (pitch, warp,
  reversal, gain, fades, envelope, effects, spectral edits), and a take
  could not be edited. A take is now an audio clip placed in its folder
  (file version 3; older folders load as takes holding their region), so
  recording in Takes mode folds clips in with all their edits, each at
  the level it had, and a folder taken into another brings its gain and
  edge fades into its takes. What a comp plays is worked out in one place
  (TakeFolder::played: each take cut at the comp's boundaries, edits and
  all, as AudioClip::cut cuts), used by playback, the stretcher voices
  and Flatten Comp, so a comp and its flattened pieces sound the same.
  Separate on a reversed clip now keeps what each piece played.

  Takes have ids of their own that edits keep, and every clip editor
  works on a clip or on one take: a take lane's menu opens the pitch
  editor, the spectral editor and clip effects on that take, converts it
  to MIDI and holds its clip envelope. While a clip envelope is shown the
  take lanes draw and edit their takes' envelopes (comping waits until
  it is hidden) and the folder's main lane draws what the comp plays.
  Strip Silence leaves take folders out and says to flatten the comp
  first: a take cannot be cut into pieces inside its folder.

  </details>

- **Windows crash reports name where it crashed: the stack, the modules, symbols kept apart ([#62](https://github.com/BurningTreeC/faderframe/issues/62))**

  <details><summary>Details</summary>

  [#62](https://github.com/BurningTreeC/faderframe/issues/62): a Windows crash report had the exception's code and address and a
  backtrace of `<unknown>` frames (the program is stripped). It now comes
  from a crate of its own, faderframe-crash, written a part at a time so
  that a fault while walking a broken stack keeps what came before: the
  code, the thread's name and id, where it happened as module + offset
  with the nearest export, what an access violation touched, the
  registers, the stack walked from the exception's context
  (RtlVirtualUnwind, memory read only where VirtualQuery says it reads),
  and the loaded modules with their link time stamps and CodeView ids. A
  Windows panic's stack is walked the same way.

  Nothing is symbolised in the process: the program is linked with a
  build id, the Windows bundle keeps its debug info apart before
  stripping (FaderFrame-<version>-windows-x64-symbols.zip, a release
  asset), and scripts/symbolize_crash.py adds the functions and lines to
  a report, refusing symbols of another build. The graphics-driver check
  reads a Windows report's stack, never its module list (which names
  every driver loaded).

  </details>

[All changes since 0.46.0](https://github.com/BurningTreeC/faderframe/compare/v0.46.0...v0.47.0)

## [0.46.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.46.0) — 2026-11-11

- **Plugins get time to load before renders, automation lanes sized, EQ band controls under the graph ([#56](https://github.com/BurningTreeC/faderframe/issues/56), [#57](https://github.com/BurningTreeC/faderframe/issues/57), [#61](https://github.com/BurningTreeC/faderframe/issues/61))**

  <details><summary>Details</summary>

  [#56](https://github.com/BurningTreeC/faderframe/issues/56): a render hosts plugin instances of its own and starts at once,
  faster than real time; a plugin that prepares what its state names on
  threads of its own (a NAM amp model) played silence for its first
  seconds — a self-frozen track's cache silent at its start. No format
  lets a plugin say it is ready, so a render hosting any third-party
  plugin gives them real time first (Preferences → General → Renders,
  2 s by default, 0 = none; built-ins never wait): blocks of silence
  paced from when the newest instance was made, a fixed number of them
  so renders stay alike, the plugins polled between them (renders never
  polled before: a restart, a latency known once loaded, now rebuilds the
  graph). What the warm-up played is wiped from Mix Intelligence's taps
  and probes (they read it as lost frames and quieter devices). The
  self-freeze key holds the warm-up where third-party plugins play
  (KEY_VERSION 2: caches made without one are made again).

  [#57](https://github.com/BurningTreeC/faderframe/issues/57): automation lanes have a height each (32-400 px, saved with the
  layout): drag a lane's bottom edge in the header column (Alt: every
  lane), double-click it for the default, or the lane menu's Lane Height.
  A saved layout keeps the heights of the lanes there are. Height drags no
  longer end a Mix Intelligence audition or a scene crossfade.

  [#61](https://github.com/BurningTreeC/faderframe/issues/61): the EQ's band controls have a room of their own between the
  frequency scale and the bottom bar, so a deep cut or a dynamic range
  pulled down is never under them. The room is as tall as the tallest
  controls any band shows (a band's dynamics settings included) and held
  while dragging, so the graph never resizes under the pointer; the
  controls hang under the scale, ">>" staying where it was clicked; in a
  low editor the room gives way first, the graph keeping 160 px. With
  nothing selected it says how to begin.

  From Marcin Kozioł's pull requests, reworked.

  </details>

[All changes since 0.45.0](https://github.com/BurningTreeC/faderframe/compare/v0.45.0...v0.46.0)

## [0.45.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.45.0) — 2026-11-09

- **Plugin GUIs at the desktop's scale, a mono track per output the plugin did not pair ([#54](https://github.com/BurningTreeC/faderframe/issues/54), [#52](https://github.com/BurningTreeC/faderframe/issues/52))**

  <details><summary>Details</summary>

  [#54](https://github.com/BurningTreeC/faderframe/issues/54): on Linux plugin GUIs run on X11 (XWayland), and FaderFrame told
  them a scale of 1.0 whatever the desktop's: on a 4K monitor at 150 %
  they drew at 100 % beside a header GTK draws at 150 %. They are now told
  how large the desktop shows a pixel of their window (editor_scale): on
  Wayland the X11 size of the monitor the editor opens on over its Wayland
  logical size — 1 where the compositor scales X11 windows itself, the
  monitor's scale where X11 programs draw at full resolution (GNOME's
  native XWayland scaling, KDE's "applications scale themselves",
  Hyprland's force_zero_scaling); X11 sessions and unmatched monitors take
  GTK's window scale times the desktop's Xft DPI over 96. VST3 gets
  setContentScaleFactor on Linux too (it was Windows only); CLAP's
  set_scale and LV2's ui:scaleFactor carry the new value. Checked here:
  TONE3000 (LV2) asks for 1536x921 instead of 1024x614, PultEQFx (CLAP and
  VST3) 1740x534.

  [#52](https://github.com/BurningTreeC/faderframe/issues/52): DrumGizmo's outputs reach FaderFrame sample-exact (measured with
  the installed DrumGizmo 0.9.20 and a kit of known microphone offsets:
  in-process and sandboxed, live and offline, humanizer on and off). The
  flam came from the tracks: its sixteen ungrouped LV2 outputs were paired
  by the host into stereo tracks, two microphones hard left and right.
  "Create Output Tracks" now pairs channels only where the plugin itself
  groups them (PluginInstance::output_buses_declared: false for LV2 with
  more than two outputs and no port groups, forwarded by the sandbox);
  others get a mono track per channel, named after the port. "Create Mono
  Output Tracks" is offered only where it makes a difference.

  </details>

[All changes since 0.44.0](https://github.com/BurningTreeC/faderframe/compare/v0.44.0...v0.45.0)

## [0.44.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.44.0) — 2026-11-09

- **Keep on Top: plugin editor windows above every other window**

  <details><summary>Details</summary>

  A pin at the right of every editor window's header, and "Keep Window on
  Top" in the mixer's insert menu, keep a plugin's editor windows above
  all others; saved per plugin with the project (WorkspaceSet's
  plugin_windows_on_top, Action::SetPluginWindowOnTop, not an undo step).
  on_top::keep brings open windows in line on the UI tick, by each
  platform's own means:

  - Windows: topmost (HWND_TOPMOST), the plugin's child window with it.
  - macOS: the floating window level; a sandboxed plugin's editor, in the
    helper's own window, through EditorCall::SetAbove.
  - Linux/BSD: the EWMH request (_NET_WM_STATE_ABOVE) for X11 windows,
    honoured by X11 window managers and by GNOME's, KDE's and most other
    Wayland compositors for XWayland windows. Wayland has no such request,
    so a built-in editor or parameter window kept on top opens on XWayland.
    Hyprland reads neither and pins instead, over its socket (0.56's Lua
    dispatchers, else the older ones). A plugin's own floating window is
    found by title among FaderFrame's and its helpers' windows.
  - Where nothing can (Wayland without XWayland, not Hyprland), the pin is
    insensitive and says why.

  Editor windows on XWayland (plugin GUIs' frames, and now these) are
  transient for a hidden anchor window there: GTK then marks them dialogs
  itself, where it had written "normal" over the dialog type set by hand,
  so tiling window managers float them as intended.

  </details>

[All changes since 0.43.0](https://github.com/BurningTreeC/faderframe/compare/v0.43.0...v0.44.0)

## [0.43.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.43.0) — 2026-11-09

- **Offline renders told to plugins, a plugin's outputs a channel per track ([#50](https://github.com/BurningTreeC/faderframe/issues/50), [#52](https://github.com/BurningTreeC/faderframe/issues/52))**

  <details><summary>Details</summary>

  [#50](https://github.com/BurningTreeC/faderframe/issues/50): exports, stems, freezes, bounces, batch processing and self-freeze
  caches rendered with every plugin believing it played live. Plugins that
  work differently offline kept their live ways: Aeolus's reverb, whose
  convolver runs on a worker thread live and in the block offline, came out
  chopped in renders far faster than real time. ProcessConfig::offline is
  set when the engine as a whole renders offline (never in a live session,
  not even in the graphs rendered ahead) and each format passes it on:
  VST3 kOffline in the setup and in every block, CLAP's clap.render before
  activation, LV2's freewheel port (it read 1 in live sessions' graphs
  rendered ahead until now) and its worker's work done in the block that
  scheduled it, the AU's offline render property, the sandbox's Activate.
  Sandboxed plugins in an offline render had a block's real-time duration
  before the host gave up on a block and wrote silence; every block is now
  waited for (OFFLINE_BLOCK_TIMEOUT, 30 s).

  [#52](https://github.com/BurningTreeC/faderframe/issues/52): the timing between a plugin's outputs was exact (new engine tests:
  latency, block sizes, worker pool, render-ahead), but how a plugin groups
  its channels decided what a track took. DrumGizmo's sixteen ungrouped LV2
  ports came as eight pairs, two microphones hard left and right on each
  output track: the close kick in one ear, its overhead bleed a few
  milliseconds later in the other. A track can now take one channel of any
  plugin output in any format (InputRouting::Plugin's channel, a
  PickChannel node; written only when set, so projects read as before),
  "Create Mono Output Tracks" makes a mono track per channel (mixer insert
  menu, arranger track menu, the editor window's Outputs button; "<bus> as
  Mono Tracks" per bus), the input menus list each channel, and LV2 names
  each channel by its port. A project whose multi-output plugin is missing
  no longer fails to play.

  </details>

- **CI: GStreamer's bad plugins, a nextest profile for the runners**

  <details><summary>Details</summary>

  With the disks no longer full, CI's tests ran for the first time in weeks:
  3492 of 3497 passed. Four 360° video tests read JPEG and VP9 movies
  through parsebin, whose parsers (jpegparse, vp9parse) are in
  gstreamer1.0-plugins-bad, which CI never installed (the release jobs
  and the packages have them); CI and Ports' Windows job install it now.

  The AES67 followers loopback needs a quiet machine: on GitHub's runner
  132 of 144 636 frames of the circle came back short of full level
  (none lost or repeated; none on a workstation). .config/nextest.toml's
  ci profile leaves it out there and holds the retries; CI and Ports run
  `cargo nextest run --workspace --profile ci`.

  macOS (both) passed every test in 0.42.0's run.

  </details>

[All changes since 0.42.0](https://github.com/BurningTreeC/faderframe/compare/v0.42.0...v0.43.0)

## [0.42.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.42.0) — 2026-11-07

- **CI: room on the runners' disks, CoreMIDI's own destination, tests fixed**

  <details><summary>Details</summary>

  CI (Linux) and Ports (Windows) had been red for weeks: the runners' disks
  filled while linking the workspace's test binaries (ld's bus error, "No
  space left on device"). Builds there use line tables only
  (CARGO_PROFILE_DEV_DEBUG) and no incremental cache; the Linux job removes
  SDKs it never uses first. Tests run under nextest with two retries for
  real-time tests that miss a deadline on a shared runner.

  macOS, every run:
  - faderframe-midi-ump: FaderFrame's own MIDI 2.0 destination is made
    with the CoreMIDI C API (own_destination.rs): coremidi 0.9.2's
    virtual_destination_with_protocol turns the source-connection context
    of every event list into a `&mut ()`, and a destination has none — a
    null reference, which aborted debug builds at the first message.
  - show_gear's a_jump_cancels_first_frame_changes_not_yet_sent watched for
    400 ticks; where sleeps overshoot that outlasted the 2.75 s until the
    song plays into C again (its gear sent anew, rightly). It watches 1.6 s
    of wall-clock time now (reproduced here with 8 ms ticks).
  - Left out on macOS (cfg_attr ignore, the reasons in the code): the AES67
    loopbacks (the shared VMs keep no 1 ms media clock: xruns, each about
    80 ms of silence; what arrives is all in place) and the A/V calibration
    with a virtual camera (25-29 ms against 33.3 there; to be looked into on
    a real Mac).

  Windows (seen when its tests last ran):
  - LV2 file URLs: a Windows path became the URL's host
    ("file://C%3A%5C..."), so names relative to a bundle's files did not
    resolve; now "file:///C:/..." both ways (tested for both forms here).
    Its UI tests' lock no longer passes one failure on as poison.
  - Mix Intelligence's phase test measured a cancelled sum with log10 of a
    value rounding can put just below zero (Windows' maths library): NaN.
    Clamped as ratio_db already does; checked under Wine.

  </details>

- **Mono → Stereo inserts: a mono track keeps a plugin's stereo output**

  <details><summary>Details</summary>

  A hosted plugin's node runs at the plugin's own port widths and the graph
  folds its stereo output into a mono track ((L + R) / 2): an amp
  simulator's stereo preset (TONE3000's "Stereo Crunch Verb") played folded
  to mono on a guitar track, with no way to keep the pair. A slot set to
  Mono → Stereo (PluginSlot::mono_to_stereo, written only when on;
  Command::SetPluginMonoToStereo, Impact Graph) is one of the slots that
  widen a mono track (PluginSlot::widens_mono, beside the Guitar Station
  and the Hardware Insert's own switch), so the track is stereo from it on:
  chain_layout, layout_into, the graph, freezes and renders follow.

  Offered in the mixer's insert menu (also the Channel Overview's) where a
  mono signal reaches a device whose main output has two channels or more
  (Session::mono_to_stereo_offered), never for devices that widen by
  themselves, have their own switch, hold chains or sit in the input
  stage; refused on frozen tracks; kept by track presets. Not automatic:
  most effects report stereo ports. Dev action mono-to-stereo:<n>=<on|off>.

  Checked with TONE3000 on the demo's Pluck (centred, dummy device): off
  -24.7 / -24.7 dBFS, on -21.8 / -20.4. Tests: project (layouts, file,
  presets), engine tests/mono_to_stereo.rs (a hosted stereo effect: folded
  to exactly the average, or kept), session tests/mono_to_stereo.rs, mixer
  a_mono_strips_insert_menu_turns_it_stereo_after_the_insert.

  </details>

[All changes since 0.41.0](https://github.com/BurningTreeC/faderframe/compare/v0.41.0...v0.42.0)

## [0.41.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.41.0) — 2026-11-07

- **Helpers end without plugins' destructors; helpers leave long nodes; platform fonts**

  <details><summary>Details</summary>

  [#47](https://github.com/BurningTreeC/faderframe/issues/47): the plugin scan helper returned from main after describing a bundle,
  and exit() ran the plugin's static destructors: Native Instruments'
  Qt-based plugins (Replika) crash there on macOS, a crash report per bundle
  scanned, and the failed exit had the bundle recorded as failed. Every
  helper (scan, check, sandbox) and the program after a clean quit end with
  faderframe_ui::plugins::end_process (_exit / TerminateProcess) once what
  they printed is flushed. The scan answer follows SCAN_MARKER and counts
  even when the helper died after printing it; bundles that failed under
  another scanner version, or when the browser's Rescan is pressed
  (ScanCache::forget_failures), are scanned again.

  [#49](https://github.com/BurningTreeC/faderframe/issues/49): while one node ran long - a sandboxed plugin waiting for its helper -
  every helper of the cycle spun through it at the audio thread's priority.
  TONE3000's stereo engine hands its channels to JUCE realtime threads that
  fall back to normal priority where RLIMIT_RTPRIO is below 99; they found
  no core free of spinning FIFO threads: dropouts, a stuttering desktop. A
  helper without a job for LEAVE_AFTER (15 us) leaves the cycle
  (leave_running_job: it sleeps after 25 us) and a thread queueing jobs
  calls it back (WorkerPool::call_back); the caller works on. Measured
  (PipeWire, 64 frames, the demo stopped, 4 cores, "Stereo Crunch Verb"):
  pool workers 24 % -> 9 % of a core each, the process 114 % -> 62 %; the
  bench (128 tracks, 4 threads) unchanged. Test
  helpers_do_not_spin_through_a_long_node (360 ms of worker CPU in 4
  cycles before, under 32 ms now).

  [#48](https://github.com/BurningTreeC/faderframe/issues/48): none of the canvases' fonts ship with macOS, and Core Text takes
  "Sans" for Helvetica and "Monospace" for Courier - thin and soft at the
  canvases' sizes on a 1x display, while GTK's widgets use the system font.
  The painters (pango and parley) and the stylesheet name the platform's UI
  and monospace fonts before the generic ones on macOS and Windows; Linux
  unchanged.

  </details>

[All changes since 0.40.0](https://github.com/BurningTreeC/faderframe/compare/v0.40.0...v0.41.0)

## [0.40.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.40.0) — 2026-11-05

- **Remote: a new look, room to scroll the mixer, drawn icons**

  <details><summary>Details</summary>

  The mixer could only be scrolled by its thin scrollbar: every fader and
  pan took the touch (touch-action: none) and the faders filled the strips.
  Now a drag on a slider starts only once the finger has moved 4 px along
  it (and only then sends touch on), vertical faders let sideways swipes
  through (pan-x: a swipe anywhere on a strip scrolls the mixer) and
  horizontal sliders up/down ones (pan-y: cue-mix lists scroll). Under the
  strips an overview - each strip a cell in its colour with its level, the
  strips on screen framed - scrolls the mixer when dragged or tapped; the
  master strip is pinned beside the scroller and the strips snap.

  Icons are SVGs in index.html (#icons) cloned by icon(name), replacing
  Unicode symbols phones drew as emoji or odd glyphs: transport, tabs,
  markers, sections, launcher, setlist, lyrics, macros.

  The look: fader caps in the track's colour, segmented two-channel meters
  with a held peak, level readouts, a counter and round transport buttons,
  toggles for record/loop/click, marker and section steppers, cards for cue
  sends with on/off switches, launcher clips in their colours with sticky
  headers and playing/queued/recording states, the setlist's current song
  highlighted, a pairing card. Phones held sideways get a tab rail on the
  left and M/S/R in one row; narrow phones narrower strips; light mode
  kept. The teleprompter scrolls only itself (scrollIntoView scrolled its
  toolbar away).

  serve_the_demo_for_a_browser takes FADERFRAME_REMOTE_PLAY=1: the demo
  plays, looping, on the dummy device, so meters and playhead move.

  </details>

[All changes since 0.39.0](https://github.com/BurningTreeC/faderframe/compare/v0.39.0...v0.40.0)

## [0.39.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.39.0) — 2026-11-05

- **Remote: say when the firewall keeps phones out**

  <details><summary>Details</summary>

  A firewall that drops what it does not allow (ufw, on by default on
  Omarchy; firewalld) left a phone's page loading for ever while the
  remote answered on the computer itself: its own connections come in
  over loopback, which every firewall lets through, so no test from here
  can tell.

  faderframe_remote::firewall reads the firewall's own files instead (no
  root, nothing run; Linux): ufw by ufw.conf, its systemd unit, the
  default input policy and user.rules in order (protocol, ports, port
  lists and ranges, source/destination/interface against the LAN route
  from /proc/net/route); firewalld when it runs, by the interface's zone
  or the default one (target, ports, rich rules' ports, services' ports).
  What is not understood counts as allowing when it allows and as missing
  when it denies, so a note comes only when the firewall is in the way.
  FirewallNote carries a command that lets in the local network only, on
  the remote's port, over TCP (ufw: on the LAN interface).

  The session reads it when the remote starts answering the network (a
  warning notice once per turning on) and every 3 s while it runs (an
  Info notice when it lets devices in); WebRemoteStatus::firewall.
  Preferences -> Remote shows it under Status with the command and Copy.
  check_in / Session::set_firewall_root read a pretend root for tests.

  README: what's new in 0.37, 0.38 and 0.39, the phone and web remote
  under What works today, and a troubleshooting entry for phones that
  cannot open the remote.

  </details>

[All changes since 0.38.0](https://github.com/BurningTreeC/faderframe/compare/v0.38.0...v0.39.0)

## [0.38.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.38.0) — 2026-11-04

- **Pitch editor follows the playhead; colour chooser with room for its editor**

  <details><summary>Details</summary>

  The pitch editor painted the playhead but never scrolled after it, so
  Follow Playhead did nothing there ([#39](https://github.com/BurningTreeC/faderframe/issues/39)). It now pages like the piano
  roll: while playing inside the clip, a playhead that left the view is
  brought back near its left edge; not while dragging or while a
  scrollbar is held.

  GTK's colour dialog holds its content to 262 px high and cannot be
  resized, while its custom-colour editor needs about 350 px: on desktops
  that keep the dialog at its first size the colour plane was cut off
  ([#40](https://github.com/BurningTreeC/faderframe/issues/40)). Colours are now chosen in a window of our own around GTK's
  chooser, sized for the larger of its two pages from the start and
  resizable (color_chooser.rs). The theme editor's buttons open it on the
  editor, track and section colours on the palette as before.

  </details>

- **Replace the deprecated fetch_update with compare-and-swap loops**

  <details><summary>Details</summary>

  Rust 1.99 deprecates AtomicUsize::fetch_update (renamed try_update),
  which CI's -D warnings turns into an error; try_update does not exist
  on older toolchains. The held-signal budget in Mix Intelligence and the
  hand-over test's stall counter now use compare_exchange_weak loops.
  Checked: cargo +1.99.1 clippy --workspace --all-targets -D warnings and
  fmt are clean.

  </details>

[All changes since 0.37.0](https://github.com/BurningTreeC/faderframe/compare/v0.37.0...v0.38.0)

## [0.37.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.37.0) — 2026-11-04

- **The roadmap's open items, the gaps, and five new features**

  <details><summary>Details</summary>

  Top five:
  - Undo tree with Audio Bisect: branches kept, any step auditioned A/B,
    a listening search for the edit that made it worse.
  - Codec monitor and platform previews: Opus on the monitor path,
    platform loudness, decoded true peak, artefacts.
  - Mix scenes: console snapshots recalled, crossfaded, morphed and
    compared on the audio thread.
  - Delivery QC: null-tested stems, a shared-processing stem, QC reports
    (PDF/JSON) against loudness specs.
  - Self-freezing: untouched tracks play from a disk cache of their chain;
    When Needed by default (only while the machine struggles).

  Roadmap and gaps:
  - Stem separation (X-UMXL), restoration devices (de-noise, de-click,
    de-hum, de-clip, de-reverb), convolution reverb, multiband dynamics,
    vocoder, Wavetable/FM/Granular, Pitch Correct, aligning doubles.
  - Comping by lyric, polyphonic lead sheets, notes/markers/sections
    locked to time, articulation and drum maps with an editor, show-mode
    gear changes, microtonal tuning, MIDI 2.0 resolution in clips and on
    macOS, a score (notation) editor.
  - Ambisonics with head-tracked listening, A/V calibration by camera,
    panning that follows the picture, 360° video.
  - ARA hosting, several audio interfaces joined natively, AES67 network
    audio, Capture Audio, slicing, a device's own A/B and undo, clip
    envelopes, Strip Silence, batch processing, Hardware Insert Mono →
    Stereo.
  - Merging project versions, live collaboration, DAWproject/MusicXML and
    AAF interchange, a media browser, Lua scripts and macros, a phone/web
    remote, adaptive music for games (FMOD/Wwise exports), screen readers
    for the remaining views.
  - Render ahead: the bus tier keeps the old sequence until the deep tier
    has the new one ready (no gap at a locate); paced tests patient on
    retries. External MIDI out keeps its order per port. Windows zero-copy
    video (untested). Carbon Echo's disagreements with the manual traced
    to the circuit.

  </details>

[All changes since 0.36.0](https://github.com/BurningTreeC/faderframe/compare/v0.36.0...v0.37.0)

## [0.36.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.36.0) — 2026-10-29

- **Mix Intelligence: re-analysis after a change, Undo for applied proposals, held signals**

  <details><summary>Details</summary>

  - Applying a proposal analyses again by itself (same range and tracks).
    The tracks the change reached get a first look: a render of only what
    reaches them (capture::pruned: every other track's clips, devices, sends
    and automation taken out; taps bit-identical to the whole mix's) cut
    into segments rendered side by side with a 2 s run-up (render_look; one
    piece with third-party plugins), shown with their pairs while the master
    and buses show as last measured (MixResults::{provisional, pairs}). The
    whole render then measures the changed tracks again exactly; the first
    look is never cached. Demo (release): the applied track shown again
    after 0.47 s, everything after 1.95 s.
  - History steps have ids (History::{last_step, is_done, knows,
    inverse_of}). An applied experiment follows its step: "Checking",
    "Applied", or "Resolved" when the analysis after it no longer finds the
    finding (still listed, with its Undo); undo and redo of it analyse
    again. MixOp::Undo(step) undoes the step when it is the last, else
    applies its inverse as "Undo Mix Intelligence: …" (later edits kept;
    refused whole when a later step removed what it changed). The view has
    an Undo beside the experiment and at the top of the finding, badges in
    the list. Dev action mix-undo:x.
  - Signals held from renders (mix::held: 16 bits with a scale per 1024
    frames; 512 MB a job, 192 MB kept between analyses under the profiles'
    keys): the phase and timing pairs read their excerpts there instead of
    rendering the mix again; excerpts still missing are rendered pruned to
    the tracks they need. Demo 2.65 s -> 1.70 s, four times the tracks
    3.87 s -> 2.47 s. Health checks kept for the last 8 fingerprints (an
    undo finds the check of the project as it was).
  - Renders running side by side open streamed sources of their own
    (own_sources): two renders reading one StreamSource could free a page
    under the other's epoch (copied tracks sharing a recording).

  Tests: session tests/mix_apply.rs, view an_applied_experiment_shows_its_undo,
  a_pruned_render_taps_what_the_whole_mix_taps, held::tests, history
  step_tests; one_pass_speed prints the apply path.

  </details>

[All changes since 0.35.0](https://github.com/BurningTreeC/faderframe/compare/v0.35.0...v0.36.0)

## [0.35.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.35.0) — 2026-10-28

- **Mix Intelligence: one-pass analysis, phase, width, dynamics, same audio, health, references**

  <details><summary>Details</summary>

  - One-pass processed analysis: engine::capture (CapturePlan,
    OfflineRenderer::with_capture; NodeRole::Heard taps on every analysed
    track, bus and return where it reaches its destination, aligned by the
    graph; a probe after every device; nothing built without a plan) via
    session::render::render_captured, replacing one solo render per track.
    Profiles built beside the render on at most 8 threads (mix::measure);
    per-track fingerprints of the upstream closure (mix::fingerprint), so an
    edit re-measures what it reaches and stales only the findings about it.
    Demo (release) 20.1 s -> 2.6 s, the demo four times 54.4 s -> 3.6 s,
    unchanged 0.13 s. Buses and returns analysable; bus and master dynamics
    react to the whole mix. First-stage findings shown while the second runs.
  - Phase and timing (mix/extra.rs, phase, rules_phase): pairs from the
    selection, roles, same audio, kick/bass, masking and co-moving levels (at
    most 12; excerpts tapped in one render); Auto-Align's lag and polarity,
    per-band coherence and residual phase, the sum against the energies,
    onset pairing. PolarityInverted, TimeOffset (comb 0.05-15 ms; longer
    offsets are musical), FrequencyDependentPhase (manual all-pass
    suggestion), OnsetOffset, LooseTiming; Experiment::Alignment sets a
    track's delay and polarity.
  - Width (side against mid, mono change per band; WideLowEnd,
    MonoBalanceShift), dynamics (5 ms envelope, BS.1770 momentary and
    short-term; crest, PLR, LRA, transient-to-sustain; TransientFlattening
    as "possible", WideLoudnessRange, LevelJump).
  - Same audio (same): copies of one source are not masking. Complex band
    sums per frame of the fold-down and, stereo, of each channel; per-band
    coherence in 4 s windows (delays to 1 ms allowed); same when the
    fold-downs or left/left and right/right reach the threshold (copies
    panned apart through a wide source or a stereo chorus); undecided
    windows (fades, clip ends) follow the nearest judged one. Those frames
    leave the masking rule and are reported as SameAudio.
  - Health (DC offset, overs and flattened peaks between devices, invalid
    samples, silencing or idle devices with their cost, bypassed devices)
    and reference comparisons (loudness-matched loudness, spectrum,
    sections, stereo/mono; to A/B listening and EQ Match).
  - One listening gain: EngineController::set_listening_gain /
    Session::listening_gain (the monitor trim is gone); the real level
    change and the fairness gain shown apart.
  - ALGORITHM_VERSION 5; opt-level 3 for faderframe-analysis and
    faderframe-mix-intelligence in dev; dev actions mix-open, mix-pairs,
    mix-dynamics, duplicate-track.
  - README for 0.35; docs/ARCHITECTURE.md Mix Intelligence and roadmap (the
    health check done).

  Tests: the full workspace suite (2356 passed), clippy -D warnings and fmt
  clean. Checked in the app: the demo with a copied, differently panned
  Pad - no masking between the copies, a same-audio note instead; the
  analysis 3.7 s in a debug build.

  </details>

[All changes since 0.34.0](https://github.com/BurningTreeC/faderframe/compare/v0.34.0...v0.35.0)

## [0.34.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.34.0) — 2026-10-28

- **Mix Intelligence, A/B listening, time-locked tracks, Auto-Align, 2A Leveler, Tape Machine, new pedals and amps ([#22](https://github.com/BurningTreeC/faderframe/issues/22))**

  <details><summary>Details</summary>

  - Mix Intelligence (first milestone; deterministic, no AI, models or
    network): crate faderframe-mix-intelligence (third-octave band frames
    from offline renders, FileMeter levels, explicit documented rules),
    session::mix (processed = each track solo in place through the mix
    plus the master, or source; a worker job, an LRU profile cache,
    stale/superseded results rejected), view faderframe-view-mix. Findings:
    spectral masking between track pairs over time, level problems (true
    peak, overs, a track over full scale, section loudness jumps), mono
    fold-down losses; each with evidence in units, severity and confidence
    apart with their terms, method and limitations, and experiments (EQ
    cut, dynamic cut, level) auditioned unrecorded A/B and loudness-matched
    (EngineController::set_monitor_trim), applied as one undo step. Track
    roles (Track::role, Command::SetTrackRole) set by the user only.
  - A/B listening: a compare source in ListenOut (nodes::compare: 20 ms
    crossfades, follows the transport through the play history, never in
    renders or meters). Auditioning the undo history: History::project_at
    renders a step on a copy beside the mix (bit-exact with really
    undoing), loudness-matched Now/Then. Reference tracks
    (Project::references, Command::SetReferences, faderframe-view-reference):
    along the song or looping, EBU R128 matched, long-term spectra
    compared (faderframe_analysis::ltas), Send to EQ Match.
  - Time-locked tracks: Track::time_base, Command::SetTrackTimeBase;
    faderframe_project::time_lock plans before and keeps after every edit
    in Session::edit, so clips, MIDI clip starts, automation and frozen
    renders keep their samples through any tempo change (exact sample round
    trips, TempoMap::musical_at_sample), in the same undo step; ripples in
    time on locked tracks. ADR cues always locked to the picture; the ADR
    beeps' track made time-locked.
  - Track delay and Auto-Align: Track::delay_ms (nodes::TrackDelay, a ring
    and a 64-tap Kaiser sinc, earlier tracks through delay compensation,
    rebuild only on a shape change); faderframe_analysis::align
    (ML-weighted GCC, z-score confidence, hub) and session::align with a
    window; freeze renders leave delay and polarity out (frozen
    polarity-inverted tracks played un-inverted).
  - 2A Leveler (faderframe.2a-leveler): an optical leveller from the UREI
    and Teletronix drawings, three netlists cut at the pots' wipers, the T4
    cell (EL panel under the Alfrey-Taylor law, CdS with trap levels for
    the history-dependent release), Mix, external key, link, 11 presets.
  - Tape Machine (faderframe.tape-machine): Jiles-Atherton tape with AC
    bias, three formulations fitted to data sheets, NAB/IEC/AES heads and
    bump, wow and flutter at IEC 386 figures inside the oversampler's
    latency, 13 presets; its display text contrasting in light themes.
  - Guitar Station: Red Squeeze (MXR Dyna Comp; the solver's new CA3080 OTA
    device), Juicer (Orange Squeezer; its bias knob a measured
    Taper::Table law spreading the squeeze over the turn), Carbon Echo (MXR
    Carbon Copy; dsp::bbd::Clocked), Tweed Lux (Fender 5E3) and Seattle 100
    (Soldano SLO-100) with both channels; pedal rigs in a "Pedals" group.
  - Clip effects: Open Window shows an effect's own window; the session
    hosts the slot outside the graph and turns the window's commands and
    the GUI's changes into chain edits.
  - Graphics driver crashes ([#22](https://github.com/BurningTreeC/faderframe/issues/22)): on Intel chips driven by Mesa's crocus
    (GMA to Haswell) Automatic draws in software from the first start;
    crash reports list the graphics chips and the renderer.
  - README for 0.34; docs/ARCHITECTURE.md status and roadmap brought up to
    date.

  Tests: the full workspace suite (2297 passed), clippy -D warnings and fmt
  clean, third-party licences up to date. Checked in the app: the Guitar
  Station with the new pedals and amps, the 2A Leveler and Tape Machine
  editors (dark and light), time-locked tracks, the Reference and History
  A/B views, the Auto-Align window, Mix Intelligence on the demo, clip
  effect windows (PultEQFx, the EQ).

  </details>

[All changes since 0.33.0](https://github.com/BurningTreeC/faderframe/compare/v0.33.0...v0.34.0)

## [0.33.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.33.0) — 2026-10-28

- **Stuck notes on bypass, start window, own themes, take lanes, drum notes ([#27](https://github.com/BurningTreeC/faderframe/issues/27), [#28](https://github.com/BurningTreeC/faderframe/issues/28))**

  <details><summary>Details</summary>

  - Stuck notes ([#27](https://github.com/BurningTreeC/faderframe/issues/27)): bypassing a MIDI effect while it plays (by hand or
    by automation) switched who plays the notes, and neither side ended
    what the other had started. Each plugin keeps a NoteLedger beside its
    MissedReleases (keys held, given to the processor, sounding at the
    output, which side the output follows): a switch ends the other
    side's notes, and playing again picks up the keys still held. Adding,
    removing or moving a MIDI effect, or a MIDI track no longer playing
    the instrument, changes what feeds a node: note-taking nodes are keyed
    by what plays them (PluginCx::upstream_of), and a node fed by
    something else ends what the old feed gave it.
  - Zoom saved with the project ([#28](https://github.com/BurningTreeC/faderframe/issues/28)): the arranger's zoom and scroll go
    into the workspace on save (WorkspaceSet::arranger_place, noted while
    painting, Session::workspace_to_save in every save) and come back on
    open (Session::project_serial).
  - Start window ([#28](https://github.com/BurningTreeC/faderframe/issues/28)): recent projects, an empty project, the templates,
    Open…, the demo; File → Start Window…. Start-up's new default "Ask"
    shows it (existing preferences keep theirs); scripted runs skip it.
  - Own themes ([#28](https://github.com/BurningTreeC/faderframe/issues/28)): View → Theme → Edit Themes… (and Preferences):
    a built-in skin with any of 56 colours changed, grouped, each with a
    reset, applied as edited; files in <settings>/themes/*.json
    (faderframe_ui_canvas::custom, user_themes, theme_editor), listed
    after the built-in skins.
  - Take lanes ([#28](https://github.com/BurningTreeC/faderframe/issues/28)): heights per track (drag the edge, double-click for
    the default, the lane menu's sizes; WorkspaceSet::take_lane_heights),
    and a ▶ per lane that plays the take whole instead of the comp
    (Action::AuditionTake, EngineController::set_take_audition; starts
    playback at the folder when stopped, ends when the folder changes).
  - Drum notes ([#28](https://github.com/BurningTreeC/faderframe/issues/28)): the piano roll draws diamonds on the note start
    (toolbar "Drums": Automatic, Bars, Diamonds; automatic for the Drum
    Sampler, devices named "drum" and clips all on channel 10); they move
    by their start and have no length handles.
  - Dev actions takes-demo:, take-lane-height:, audition-take:,
    piano-notes:, start-window, edit-themes.

  Tests: engine note_offs (arpeggiator bypassed by hand and by
  automation, removed, its MIDI track taken away), an automated arp
  bypass in the_midi_effects_do_not_allocate, an auditioned take;
  session drum clips, take audition; arranger zoom saved, take lane
  heights and audition; piano roll diamonds; custom themes and theme
  files. Checked in the app: the start window, the theme editor, take
  lanes with an audition, drum diamonds.

  </details>

[All changes since 0.32.0](https://github.com/BurningTreeC/faderframe/compare/v0.32.0...v0.33.0)

## [0.32.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.32.0) — 2026-10-27

- **Count-in of its own, Stop where heard, stuck notes, macOS start-up ([#22](https://github.com/BurningTreeC/faderframe/issues/22), [#23](https://github.com/BurningTreeC/faderframe/issues/23), [#25](https://github.com/BurningTreeC/faderframe/issues/25), [#26](https://github.com/BurningTreeC/faderframe/issues/26), [#27](https://github.com/BurningTreeC/faderframe/issues/27))**

  <details><summary>Details</summary>

  - Count-in ([#23](https://github.com/BurningTreeC/faderframe/issues/23)): a setting of its own (Transport → Count-in, its
    length, Preferences → Recording), no longer the pre-roll's click-only
    mode, which did nothing without a pre-roll. Recording from stop stands
    while the click counts, then plays (from the pre-roll's start, if any).
    Click Only During Count-in (on by default): the take records without
    the click, through loop wraps, whatever the metronome's mode
    (MetronomeShared::count_in_only, the processor's CountedIn). The
    rolling click-only pre-roll (RecordingNow::silent_before) is gone;
    older preferences become a count-in as long, without the pre-roll.
  - Stop stays where the playhead is heard: the engine, the output latency
    further on, goes back there (Session::stop_here) unless Stop returns
    elsewhere.
  - Stuck notes ([#27](https://github.com/BurningTreeC/faderframe/issues/27)): opening an editor moves a track rendered ahead to
    the live graph, and the arpeggiator was made anew there, leaving its
    notes sounding. Only buffered built-ins are made anew for the audio
    thread (Kind::buffered); others move as they are, reset. Blocks a
    plugin misses (ProcessStatus::Missed: a shared processor's contention,
    a late sandbox helper) keep what their events end for its next block
    (MissedReleases). CLAP and VST3 end a key's sounding note id before a
    new note-on on it; CLAP takes velocity-0 note-ons as offs.
  - Automation menus ([#26](https://github.com/BurningTreeC/faderframe/issues/26)): the track's parameters, then a submenu per
    device and the plugin's own groups (PluginInstance::parameter_group:
    CLAP modules, VST3 units, EQ bands, drum pads, guitar pedals; through
    the sandbox too). Menus of 30 entries or more get a search field
    (faderframe_ui_canvas::menu). Dev actions menu-search:,
    automation-menu:.
  - macOS ([#25](https://github.com/BurningTreeC/faderframe/issues/25)): FaderFrame's own hicolor index listed only its icons'
    folder and hid GTK's, and the bundle's icon themes were Homebrew's
    links, broken without Homebrew: GTK's image-missing fallback recursed
    until the stack ran out. The index is GTK's own now, GTK's
    image-missing is added when missing, the bundle copies the themes as
    files and refuses links.
  - macOS on Apple silicon was taken for an Intel app (Rosetta, "not
    supported"): the bundle's executable was a launcher script. The program
    is the executable now and sets the bundle's environment itself
    (app_bundle::prepare_environment).
  - Graphics driver crashes ([#22](https://github.com/BurningTreeC/faderframe/issues/22), Mesa's libgallium under GTK 4.22's GL
    renderer): Preferences → General → Renderer (Automatic, OpenGL,
    Vulkan, Software). After a run that crashed inside the graphics
    driver, the next start draws in software and the crash dialog says
    so; in software the GPU painter's frames are read back.
  - Pitch editor: a click or drag on its ruler moves the playhead.
  - Saving names the project after its file (the window's title shows
    it), unless it has a name of its own; files saved as "Untitled" open
    with their file's name.

  Tests: engine count_in (the click keeps quiet after counting in),
  note_offs, missed blocks, blocks_an_instrument_misses_do_not_allocate;
  session count_in, stop_stays_where_the_playhead_is_heard,
  saving_names_the_project_after_its_file, menus by device; plugin-host,
  sandbox, CLAP and VST3 note-id and grouping tests; ui prefs migration,
  app bundle, graphics-crash detection, menu grouping and search; the
  pitch view's ruler. Checked in the app: the Preferences pages, the
  pitch ruler, the automation menu and its search.

  </details>

[All changes since 0.31.0](https://github.com/BurningTreeC/faderframe/compare/v0.31.0...v0.32.0)

## [0.31.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.31.0) — 2026-10-24

- **LV2: plugin UIs open (JUCE's embedded, external UIs floating)**

  <details><summary>Details</summary>

  No LV2 plugin here showed its UI: the scan found none. A UI's binary
  was read only from ui:binary, but UI 2.x (and JUCE) writes lv2:binary;
  kxstudio's external UIs (Yoshimi's) were not a class the scan knew; and
  both kinds require instance-access, which the host did not give.

  - scan: a UI's lv2:binary or ui:binary, external-ui#Widget (and the
    older ui#external) classes; cache version 2, so caches are read again.
  - ui: X11 UIs embed (ui::embeddable), else external UIs float
    (ui::floating: the widget's show, its run on the host timer, hide
    before cleanup, ui_closed from the user's close). UIs get
    instance-access, data-access and the external UI host feature.
    Because a UI may now hold the instance, re-instantiation (another rate
    or block size) cleans the UI up before the old instance goes and makes
    it again on the new one, where it was (ui::Place). A closed UI is
    cleaned up after its callback returned.
  - FLTK UIs on Wayland decorate through libdecor's GTK 3 plugin, which
    cannot work in a process with GTK 4: Yoshimi's editor hung its
    sandbox helper. plugins::prepare_plugin_toolkits, first in main, sets
    FLTK_BACKEND=x11 while DISPLAY is set (helpers inherit it).
  - Editor frames: a size request made before the frame's first configure
    was lost (GDK resizes no X11 window while that configure is pending),
    so an LV2 UI stayed at the 480x320 placeholder, cut off. It is asked
    again with a queued resize once the window has its size.
  - The sandbox helper passes Raise on to the editor.

  Tests: tests/ui.rs (UIs linked in through Lv2Instance::with_ui_entry).
  Checked in the app with TONE3000 (JUCE, X11, embedded) and Yoshimi
  (FLTK, external), sandboxed and in process: open, close, reopen, closed
  by the window manager, reopened; the CLAP build's frame unchanged.

  </details>

[All changes since 0.30.0](https://github.com/BurningTreeC/faderframe/compare/v0.30.0...v0.31.0)

## [0.30.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.30.0) — 2026-10-22

- **Canvases claim the presses they take: TAP no longer maximises ([#21](https://github.com/BurningTreeC/faderframe/issues/21))**

  <details><summary>Details</summary>

  The transport display is a canvas in the header bar, and canvases never
  claimed their presses, so the header bar's window handle saw them too:
  two quick taps on TAP were a title bar double-click (maximise/restore),
  a right-click on the display could open the window manager's menu and
  a drag on it could move the window. A press the view handled is now
  claimed, which stops it there (GTK 4's gtk_gesture_handle_event); the
  drag gesture is grouped with the click so the claim does not deny it.
  Presses the view leaves alone still reach the header bar.

  Checked against GTK 4.24.1's sources and with a smoke run; not clicked
  on X11 here (no X server or pointer injection on this machine).

  </details>

- **Takes: a click on a lane comps only the section under it ([#20](https://github.com/BurningTreeC/faderframe/issues/20))**

  <details><summary>Details</summary>

  A click (no drag) on a take lane used that take for the whole folder,
  so one stray click replaced a carefully made comp. Now it puts the take
  into the comp section under the pointer only, between the comp
  boundaries around it (TakeFolder::use_take_at), as the comp tools of
  other DAWs do; the rest of the comp stays. A click on the take already
  playing there, or where the take has no material, changes nothing and
  makes no undo step. The whole take is still "Use" in the lane's
  right-click menu.

  </details>

[All changes since 0.29.0](https://github.com/BurningTreeC/faderframe/compare/v0.29.0...v0.30.0)

## [0.29.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.29.0) — 2026-10-22

- **Plugin editors: the editor header on Windows and macOS too**

  <details><summary>Details</summary>

  Third-party GUIs now sit under the editor header (Bypass, Presets,
  Outputs, Parameters) on every platform. frame.rs is shared; the backends
  make the window the plugin embeds into, laid over the room under the
  header and kept there every UI tick:

  - X11 (unchanged): a GTK window on GDK's X11 display with an X11 child.
  - Windows: a WS_CHILD window inside GTK's (Win32Surface::handle, given
    WS_CLIPCHILDREN); GTK's DirectComposition target is made with
    topmost = FALSE, so its content stays behind child windows.
  - macOS: GDK turns every mouse event in its own windows into a GDK
    event, so a view inside one would never get any. The plugin gets a
    borderless RoomWindow (an NSWindow subclass that can become key, for
    text fields) attached as a child window above GTK's
    (gdk_macos_surface_get_native_window, GTK 4.8), placed in screen
    coordinates every tick and detached while the frame is hidden.

  On Windows and macOS the frame is an application window, transient for
  the main window, like the device editors. Sizes are backend pixels:
  physical on X11 and Windows (x Surface::scale), points on macOS.
  FADERFRAME_EDITOR_HEADER=0 keeps the plain windows everywhere.

  Checked on Linux (Presswerk); Windows and macOS cross-checked
  (clippy) only.

  </details>

[All changes since 0.28.0](https://github.com/BurningTreeC/faderframe/compare/v0.28.0...v0.29.0)

## [0.28.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.28.0) — 2026-10-22

- **Plugin editors: third-party GUIs under the editor header (Linux)**

  <details><summary>Details</summary>

  Native editors (CLAP, VST3, LV2) opened in a bare XWayland window
  without the header the built-ins' editors have. GTK on Wayland cannot
  hold another program's window, but it can open GDK's X11 display beside
  the Wayland one: the editor window is now a GTK window there with the
  usual header as its title bar (name over vendor · track, Bypass,
  Presets, Outputs for multi-output plugins, Parameters, close), and the
  plugin's GUI goes into an X11 window laid over the room under it.

  - frame.rs: gdk_x11_display_open / gdk_x11_surface_get_xid looked up in
    the running GTK (a GTK without X11 keeps the plain windows, as does
    FADERFRAME_EDITOR_HEADER=0); the embedding window has the screen's
    default depth and visual (GTK's frames are 32-bit ARGB), follows the
    room every UI tick, takes plugin resize requests (content size, or
    the default size of resizable editors) and never answers a plugin's
    own resize with another one (rounding to GTK pixels).
  - The frame is a dialog (floated), moved to its stored place once
    shown; on close the plugin's window is reparented to the root and
    retired before GTK destroys its window.
  - The header is shared by every editor window (editor_header), and its
    Bypass follows changes made elsewhere (mixer, undo, MIDI).
  - style::add_display styles the X11 display like the default one,
    including the desktop's decoration layout, font and icon theme.
  - Windows and macOS keep their plain windows for now.

  Checked with u-he Presswerk and FabFilter Pro-Q 4 (yabridge): header
  buttons and menus, clicks into the GUI, plugin- and window-driven
  resizes, theme switches, removal while open.

  </details>

[All changes since 0.27.0](https://github.com/BurningTreeC/faderframe/compare/v0.27.0...v0.28.0)

## [0.27.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.27.0) — 2026-10-21

- **Arranger: dragging clips no longer runs out of file descriptors ([#18](https://github.com/BurningTreeC/faderframe/issues/18))**

  <details><summary>Details</summary>

  GTK 4.24 exports the renderer's textures as dmabufs (Vulkan and GL
  alike), an open file descriptor each. The arranger keeps its waveform
  tiles as such textures, keyed by the clip's position, so every step of
  a drag made new ones that stayed for 600 frames: dragging a clip back
  and forth passed the usual limit of 1024 open descriptors in seconds
  and Vulkan failed.

  - KeptCache keeps at most 256 pictures; when crowded, those not shown
    in the last frame go first, and with more on screen than that it
    settles instead of making pictures every frame.
  - Clips being moved, trimmed, slipped or gain-dragged draw their
    waveform without kept tiles until released.
  - The drag dev action can move a held drag on (drag:<view>@=>x/y).

  Measured on the demo: one held drag through 120 positions left
  21 -> 204 dmabufs open before, 21 -> 27 after.

  </details>

[All changes since 0.26.0](https://github.com/BurningTreeC/faderframe/compare/v0.26.0...v0.27.0)

## [0.26.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.26.0) — 2026-10-21

- **Ripple editing: per track, on selected tracks, or across the project**

  <details><summary>Details</summary>

  Rippling (Shuffle, or the new Ripple switch in any edit mode, Ctrl+Alt+R)
  moves what follows an edit. Where it reaches is a choice: the edited
  tracks only, those and the selected tracks, or all tracks with the
  timeline (markers, sections, loop and punch, Play Start Marker, tempo
  and metre, key, chords and lyrics, each switchable). Tracks can be left
  out of ripples; frozen ones are skipped with a notice.

  Every ripple is a list of shifts at cuts: the edited tracks' clips move
  and are never cut, the tracks kept in sync are cut and split at the same
  place, and automation moves on all of them by the same shifts
  (AutomationCurve::ripple: a point on the cut is duplicated, a cut segment
  gets two points at its value, shapes are kept). Session::apply_ripple_offset
  offsets one track or everything.

  Trims, moves, nudges, Spot, deletes, cuts, clears, pastes, duplicates,
  Insert Silence and Trim to Selection ripple; explicit commands ripple
  whatever the mode (Shift+Delete, Ctrl+Shift+Delete in all tracks, Insert
  Time and Remove Gaps in all tracks). Drags rewind the gesture to its
  first ripple each motion (History::mark/rewind), so they stay lossless
  and one undo step. The arranger tints what moves during a ripple drag;
  the edit toolbar, Edit → Ripple, the track menu and the Inspector carry
  the settings.

  </details>

- **README: Troubleshooting [skip ci]**

  <details><summary>Details</summary>

  Crash reports and logs (where they are, what to send, no report, freezes,
  core dumps, a debug log), realtime rights on Linux, and options that turn
  one part off at a time to narrow a problem down.

  </details>

[All changes since 0.25.0](https://github.com/BurningTreeC/faderframe/compare/v0.25.0...v0.26.0)

## [0.25.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.25.0) — 2026-10-21

- **Preferences → Audio: no crash on changing the audio system or device**

  <details><summary>Details</summary>

  Changing the Audio System panicked with "RefCell already borrowed": the
  "Looking for devices…" placeholder counted as a choice of output device,
  and that handler kept the scan borrowed while the page stored it again.
  Behind it, choosing an output device refilled the page inside the
  drop-down's own selection signal, replacing its model and freeing the
  selection that was still emitting (a use-after-free, or an endless
  rebuild loop under GLib 2.90).

  The placeholder is set while the page is being built; a new output
  device fills the page from an idle callback, only when it differs from
  the one shown, and keeps the unapplied choices (devices, rate, buffer
  size, channels), showing the rates and sizes of the device chosen rather
  than the stored one. Display test
  the_output_device_and_the_audio_system_can_be_changed (ignored without
  a display) panics as reported on the old code.

  </details>

[All changes since 0.24.0](https://github.com/BurningTreeC/faderframe/compare/v0.24.0...v0.25.0)

## [0.24.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.24.0) — 2026-10-21

- **README for 0.23 and new screenshots**

  <details><summary>Details</summary>

  The README caught up with 0.20 to 0.23: a "What's new in 0.23" section,
  screenshots grouped by topic, the new arranger, transport, mixing,
  rendering, audio, plugin and stability features, corrected device specs
  (compressor and limiter styles, seven reverb types, EQ output stage,
  the Synth's second wave), the shortcut table, build requirements and
  packaging notes, 470 factory presets. Every screenshot was retaken from
  the demo in a release build; new ones show the Inspector and Channel
  Overview, folder buses and groups, a cue mix with talkback, a scratch
  pad, the snap guide, the count-in, the Transport menu, the render
  report, the Audio preferences, plugin favorites, the effects editors and
  Lumen.

  Dev aid: FADERFRAME_DUMMY_DEVICES=<name>=<ins>x<outs>[;...] gives the
  dummy backend pretend devices (screenshots without the real hardware).
  docs/ARCHITECTURE.md: 470 factory presets.

  </details>

- **Windows and macOS: pool helpers, crash reporter, portable tests**

  <details><summary>Details</summary>

  Product fixes:
  - Windows: pool::sched::current() called every thread an MMCSS audio
    thread, so a plain caller (tests, offline renders, the dummy device)
    was taken for a realtime one whose workers were not realtime yet and
    ran its cycles serially. A thread now counts as realtime only once it
    joined the task (a const thread-local set by the shared sched::join).
  - macOS: the crash reporter took a signal sent to the process for a
    fault (XNU sets fault codes on sent SIGSEGV/SIGBUS/SIGILL), put the old
    handler back and carried on. is_fault() now asks the exception state:
    arm64 by the syndrome's exception class (a system call = sent), x86_64
    by the trap number (CPU exceptions below 32).

  Tests made to hold on the CI runners:
  - macOS reports a write to a PROT_NONE page as SIGBUS;
  - sandboxed macOS editors float in helper windows: the pipelined-poll
    test opens the editor as FaderFrame does on each platform;
  - Windows opens a folder only with FILE_FLAG_BACKUP_SEMANTICS; stale
    temporaries are swept by the platform's rule (an hour on Linux, a day
    elsewhere), both ways tested;
  - real-time session tests tick with the elapsed time and wait for
    outcomes, not intermediate states; the video test's tolerances come
    from the measured callback jitter.

  </details>

- **Lumen: a theme drawn with light instead of hardware**

  <details><summary>Details</summary>

  A new dark skin. Ink and graphite planes set apart by brightness steps
  instead of bevels, borders, screws or wood; hairline geometry, generous
  spacing; one cyan→violet accent for playhead, selection, active states
  and value arcs, one warm signal colour for record, arm and overload.
  Knobs are halos (fine ring, glowing value arc, dot), faders slim lit
  tracks with a pill thumb, switches ghost pills lit from within, meters
  fine cool→hot gradient bars with hairline peaks and a bloom, clips
  translucent cards with a line of the track colour, the transport a glass
  readout in tabular figures, the GTK chrome pill buttons and rounded
  floating menus with accent checks.

  New look options, defaulting to the old drawing: ConsoleLook::{knob_kind,
  fader_kind, button_kind, panel_kind, glow}, MeterKind::Gradient,
  ArrangerTheme::clip_style, Theme::chrome, UiPalette::accent_2,
  Typography::numerals (FontFamily::Figures, FontWeight::Light). Every
  existing skin draws exactly as before (op-for-op primitive dumps of all
  eight, test only_lumen_takes_the_newer_looks); lumen_reads_well checks
  WCAG AA contrast. View → Theme, Preferences, theme:lumen.

  </details>

[All changes since 0.23.0](https://github.com/BurningTreeC/faderframe/compare/v0.23.0...v0.24.0)

## [0.23.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.23.0) — 2026-10-21

- **Count in before the record start; version 0.23.0**

  <details><summary>Details</summary>

  Recording from bar 1 with a pre-roll had no pre-roll: Session::roll_back
  clamped its start to the project's start, so the transport played and
  recorded at once. The metronome's first bar sounded like a count-in and
  the part played after it landed a bar late. Now the missing length is
  counted in before playback (TransportCommand::PlayAfterCountIn): the
  transport stands, live inputs and monitoring work, nothing plays or
  records, the click counts on the tempo and metre carried back before
  bar 1, and playback starts on the frame after the count-in. A record
  point closer to the start than the pre-roll counts in only what is
  missing; the launcher's slot count-in at the start works the same way.
  Stop and abort cancel a count-in; abort and restart counts in again.
  The counter shows the count-in bars (bar 0 before bar 1) with COUNT-IN,
  the ruler a badge. Tests: engine count_in.rs (sample-exact clicks, with
  plugin latency), ahead identity, counting_in_does_not_allocate, session
  count_in.rs on the dummy device.

  Version 0.23.0.

  </details>

- **Synth patch checksums only on request**

  <details><summary>Details</summary>

  CI's Ubuntu glibc rounds sin/exp/tan differently from the glibc the
  first-wave checksums were taken with, as Windows and macOS do: the
  bit-exact comparison holds only there. Every patch is held to its RMS
  within 0.1 % on every platform; the exact checksums run with
  FADERFRAME_SYNTH_CHECKSUMS=1.

  </details>

- **Show stops armed ahead; CI tests that hold on every platform**

  <details><summary>Details</summary>

  The show armed a song's stop only on the tick after it noticed the song
  had begun, and only while the heard position was still before the end.
  The engine stops only where playback crosses the armed frame, so on a
  busy machine (a device catching up runs ahead of the ticks) a stop armed
  too late never fired and the show played on past the song. Now the stop
  of the last song in a run of songs playing straight on is armed from the
  first tick, a stop missed altogether stops at once on the song's end,
  and a stop the engine made before the ticks followed is taken as that
  song's end. Test: late_ticks_still_stop_the_show_on_the_songs_end (the
  old code played on to 19.6 s past a 4 s show).

  Tests made to hold on Windows and macOS (Ports):
  - the synth's first-wave patches keep their bit-exact checksums on
    x86_64 Linux, where they were taken, and are held to their RMS within
    0.1 % everywhere (other maths libraries round in the last bit);
  - a crashing sandboxed plugin's death may show a block or two after its
    first block on a busy machine: blocks are played until it does, within
    the second it must take;
  - the capture timing test sends its keys right after a callback, so the
    playhead extrapolated for its reference is fresh;
  - the setlist test's timeout says where the transport and show were.

  </details>

- **Arranger, transport, routing and rendering overhaul; 0.22 crash fixes**

  <details><summary>Details</summary>

  Work on the open issues 3 to 16 and the professional-workflow list:

  Arranger: a clip clipboard (copy/cut/paste across tracks with track
  mapping, replace or Shuffle insert, sources kept by value), shortcuts
  that keep targeting the selected clips (an editing context), Select All,
  split keeping every piece selected, slip editing (Ctrl+Alt+drag), snap
  to objects with a guide line (clips, markers, sections, loop, selection,
  playhead; also automation, tempo, chords and piano-roll notes), the grid
  drawn at every zoom from the snap positions with a strength preference,
  folder grouping (arming, selection, ranges and edits across a folder's
  tracks), scratch pads (copy material in, edit and play it, bring it back
  by insert, overwrite or replace), and an audit fixing 33 editing and
  recording defects (range cut takes automation, frozen armed tracks no
  longer lose every take, count-in for MIDI takes, punch loop passes, MIDI
  notes held across wraps, Delete routing, launcher clips kept out).

  Transport: loop changes while playing amend the render-ahead tiers at a
  frame beyond what they rendered instead of re-priming (no repeated
  callback); Return to Start on Stop and a Play Start Marker; abort
  recording and abort-and-restart (Ctrl+Space, Ctrl+Shift+Space).

  Routing: sends and cue sends with their own pan place their source
  instead of re-balancing the panned signal; folders can be buses.

  Rendering: statistics measured from the written file after renders and
  album exports; every export asks before replacing a file and writes
  beside it, swapping it in only after success.

  Panels and plugins: an Inspector, a channel overview, check marks for
  every menu toggle, the collapsed bottom panel keeps its button, plugin
  favorites with categories; sandboxed editors no longer close one tick
  after opening (a stale pipelined poll), with a non-fatal X error handler.

  Audio devices: a new Audio preferences page (devices, rate, buffer,
  channels, mapping, latency), several devices through PipeWire/JACK, live
  buffer changes, recovery when a device goes and comes back, real device
  latency for recording placement.

  0.22 crash fixes: realtime threads past rtkit's time limit are demoted
  instead of the process being killed (SCHED_RESET_ON_FORK kept, the limit
  kept below PipeWire's), rtkit requests paced by one broker thread off the
  audio path, code prefaulting never touches memory itself, stack
  prefaulting bounded, GPU errors no longer panic, a listening-node index
  bug, GStreamer's crashing MXF demuxer avoided; render-ahead tiers record
  their play history; a crash reporter (Crash Reports folder).

  </details>

- **Cue mixes finished, Synth presets and editor, Compressor layout fix**

  <details><summary>Details</summary>

  Cue mixes: buses and returns are solo-safe and mute-safe in cue mixes
  through cue paths (a second sum through cue shadows of the bus's devices
  into a solo-safe strip); new cue mixes copy the main mix including
  buses by the console rule (a channel once, where it reaches the master);
  buses render ahead with cue mixes; cue send and cue master automation,
  MIDI learn and control-surface following in cue mode; undo of a track
  removal restores its cue sends; talkback with a TALK button, dimming and
  settings. The three cue tests that asserted the old behaviour are
  updated.

  Synth: 33 grouped presets, the three-deck editor for the second wave
  (LFO 2, mod envelope, matrix, sync/ring/FM, ladder and notch filters,
  chorus, delay, reverb, voice options), a format fix for LFO 2 Rate, and
  realtime allocation coverage of every new path.

  Limiter: the gain-reduction scale labels no longer run under the
  readouts.

  </details>

[All changes since 0.22.0](https://github.com/BurningTreeC/faderframe/compare/v0.22.0...v0.23.0)

## [0.22.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.22.0) — 2026-10-14

- **bump version number**

- **Fix Clippy extend-with-drain warning**

- **Fix synth Clippy warnings**

- **Fix partial move in cue mix test**

- **Apply remaining rustfmt changes**

- **Apply remaining rustfmt changes**

- **Handle cue automation MIDI commands**

- **Handle cue automation static values**

- **Fix CI formatting and resolve preset test conflict**

- **Fix CI formatting and resolve preset test conflict**

- **Fix CI formatting and resolve preset test conflict**

- **Fix CI formatting and resolve preset test conflict**

- **Fix CI formatting and resolve preset test conflict**

- **Fix CI formatting and resolve preset test conflict**

- **Fix CI formatting and resolve preset test conflict**

- **Fix CI formatting and resolve preset test conflict**

- **updates for all built in plugins**

- **Integrate completed DAW enhancements**

- **EQ: double-click a key of the piano to add a band on its note**

  <details><summary>Details</summary>

  A bell at the note's frequency, 0 dB, selected to be shaped (Alt:
  dynamic, Alt+Shift: spectral). The key is the one under the pointer as
  the keyboard is painted — below a black key, the white key either side
  of its middle — and the hover highlight now shows that same key.

  </details>

- **View → Lanes: the arranger's lanes as check items**

  <details><summary>Details</summary>

  Each lane under the ruler (Video, Markers, Arranger, Key, Chords, Lyrics,
  Signature, Tempo) can be shown or hidden from the View menu as well as
  from the lane headers' menu; the ticks follow the setting wherever it is
  changed (`GlobalLane::id`, actions `app.lane-<id>`, `sync_lane_actions`).

  </details>

- **Realtime safety, plugin check, metronome sounds, panning and PDC work**

  <details><summary>Details</summary>

  Engine and realtime ("Keeping the deadline" in docs/ARCHITECTURE.md):
  - Memory the audio path touches is mapped first: prefaulting allocator,
    memory lock where allowed, code and constants mapped up front
    (prefault_code), stacks mapped; no page faults on the audio thread.
  - Thread priorities in order below the audio thread, a RealtimeKit /
    portal fallback without RLIMIT_RTPRIO, and breaths under RLIMIT_RTTIME.
  - No unbounded waits on helpers (reservoirs, sandboxed plugins), panics
    in nodes caught, bursts spread: EQ band redesigns, convolver and
    spectral frames, auto gain, MIDI controller chase checkpoints.
  - Panic resets without a burst: delay lines and compensation delays
    forgotten in constant time, render-ahead tiers reset in place (no
    transport jump), compensation delays now reset at all (old audio and
    note-ons no longer come out after a panic).
  - The system backend reports one nominal block size.
  - Plugin delay compensation through every summing point and heard
    output, play history for automation and the click, latency changes
    dip instead of jumping, low-latency monitoring through buses.
  - Panning: pan laws, dual pan, VBAP, diffuse spread, bass management,
    custom speaker layouts.

  Plugin check (faderframe-plugin-check):
  - Plays a plugin in a helper process (faderframe --check-plugin) and
    counts per call on its audio thread: allocations (the program routes
    the C allocator through a counter; the host's own allocations are told
    apart), waits, page faults, time per block and resets. Verdicts,
    findings and advice in the plugin browser, cached per plugin version.

  Features:
  - Metronome: levels for the first beat, other beats and off-beats,
    built-in sounds or the user's audio files (Preferences -> Metronome).
  - Stop Returns to Start; translucent arranger clips.
  - Control surfaces by name, FaderPort (2018).
  - GSK/GPU painter caches, background loading of samples and projects.

  </details>

[All changes since 0.21.0](https://github.com/BurningTreeC/faderframe/compare/v0.21.0...v0.22.0)

## [0.21.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.21.0) — 2026-10-11

- **trying to fix linux build step for ffmpeg**

- **release build for linux tarball needs cmake**

- **update build dependencies for release workflow**

- **build gtk against glibc 2.41**

[All changes since 0.20.0](https://github.com/BurningTreeC/faderframe/compare/v0.20.0...v0.21.0)

## [0.20.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.20.0) — 2026-10-10

- **fix AVI**

[All changes since 0.19.0](https://github.com/BurningTreeC/faderframe/compare/v0.19.0...v0.20.0)

## [0.19.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.19.0) — 2026-10-10

- **update macOS build steps**

- **fix circuits and macOS release run**

- **Several Voice Ins at once, and a port for every input**

  <details><summary>Details</summary>

  Every voice port a track takes is now listened to at the same time, so
  several singers or players can each have a track and play together.
  Before, only the first track's input was heard.

  - "Voice · All Inputs", a new port, brings every input to one track:
    input n on MIDI channel n + 1, up to 15 inputs, the way an MPE lower
    zone has its members. One track can take a choir in several
    microphones, or a hexaphonic guitar pickup with each string's pitch
    bend on its own channel.
  - Engine: a vector of listeners with room for 64, made with that room
    on the control side (`EngineController::add_voice`, `remove_voice`;
    `set_voice` still means this one only, or none). Each listener has
    its input, its port and its MIDI channel (`VoiceRun::on_channel`).
    A listener on the same input and port is replaced; one removed ends
    its notes and retires as garbage.
  - Session: what the tracks' MIDI inputs name is matched against what is
    listened to. Listeners that stay keep their trackers, so a note goes
    on while another input starts or stops; another device rate or the
    polyphonic setting makes them all anew. Each feed reaches Capture
    MIDI and the activity lights; a removed one drains to its last
    note-off. `voice_listening()` lists the inputs heard.
  - The dev action `voice-input:<track>=all` takes the new port.

  Tests: two inputs on two tracks sound together and record apart, and
  one leaves while the other sings on; the all-inputs port records eight
  inputs on channels 2 to 9 (on `DummyBackend::with_input_spread`, whose
  input n sings 4n semitones above A3 from a second in); the engine runs
  two listeners on their own ports and channels; the allocation test adds
  and removes a second listener on the audio thread.

  </details>

[All changes since 0.18.0](https://github.com/BurningTreeC/faderframe/compare/v0.18.0...v0.19.0)

## [0.18.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.18.0) — 2026-10-09

- **Polyphonic Voice In: chords from one input, an opt-in setting**

  <details><summary>Details</summary>

  Preferences → MIDI → Voice to MIDI → Polyphonic (chords), off by default:
  a voice port then follows several notes from its input, a guitar, a
  piano or a choir in one microphone, instead of one melodic line.

  - Tracker (faderframe_analysis::polyvoice::PolyTracker, causal and
    allocation-free): a 96, 128 or 160 ms Hann window (Fast, Balanced,
    Stable) analysed every 4 ms; harmonic candidates share the spectrum's
    energy and up to six notes are followed at once, each with its own
    tuning (VoiceEvent::Bend now names its key). Longer windows tell low
    fundamentals near each other apart; octave doublings sounding together
    may merge into one note.
  - Glides: note expression bends each key on its own; channel pitch bend
    stays with monophonic mode (it would bend every note together).
  - Engine: VoiceRun::new_polyphonic; a listener replaced or stopped hands
    its last note-offs on through MidiInputState across `take` and
    zero-frame pumps, and the session drains a retired listener's feed to
    its final note-off, so Capture MIDI keeps them. A missing input, a new
    device rate or a new glide mode releases the listener too.
  - Older preferences read as monophonic.

  Tests: plucks, strummed chords at all three responses, arpeggios,
  re-plucks, noise, thresholds, key snapping, reset, block sizes and
  rates from 44.1 to 192 kHz; the engine plays and records a detuned triad
  with each key's expression and releases it through a zero-frame pump;
  polyphonic_voice_to_midi_does_not_allocate covers chords, settings and
  retirement. Still one input listens at a time.

  </details>

- **A meter rendering probe for the GSK renderers**

  <details><summary>Details</summary>

  `cargo run -p faderframe-ui --example meter_probe [vulkan|gl|cairo]
  [frames]` paints the real meter controls (plasma, bar and ladder, 16
  each, moving levels) through the snapshot painter thousands of times
  without audio or a window, and compares every 64th frame with the first
  one at the same levels, saving a differing frame as
  /tmp/faderframe-meter-corrupted.png: a check for rendering stripes
  outside the DAW. Needs a display connection.

  </details>

- **AU gain reduction: read the Dynamics Processor's documented meter**

  <details><summary>Details</summary>

  The Ports CI still failed after b699523. On the macOS runners Apple's
  Dynamics Processor lists only its controls (Compression Threshold,
  Headroom, Expansion Ratio and Threshold, Attack, Release, Master Gain);
  its Compression Amount meter has no ParameterInfo and is not in the
  parameter list, so probing ids 1000 to 1003 for meter-flagged info could
  not find it either.

  Now that one unit (aufx:dcmp:appl) is asked for its documented
  Compression Amount (global id 1000, dB) by value, again once the unit is
  initialised; other units' meters still come from their parameter lists,
  so a third-party control at id 1000 is never taken for a meter. A new
  processor clears the old reading, and a failed or non-finite read marks
  the meter unreported. The test also checks the reading falls back once
  compression stops, and that the delay and low-pass report nothing.

  </details>

- **AU gain reduction: find meters flagged writable or not listed**

  <details><summary>Details</summary>

  The Ports CI failed since the AU gain-reduction meters came in: on the
  macOS runners `the_dynamics_processor_reports_its_gain_reduction` found
  no meter on Apple's Dynamics Processor (`reduction()` was None). Only
  parameters that were not writable could be meters, and only those in
  the unit's parameter list.

  Now a parameter flagged as a meter counts even when the unit also flags
  it writable (it is not offered as a control then), and when the list
  holds no gain-reduction meter, Apple's documented meter ids (1000 on:
  the Dynamics Processor's Compression Amount) are asked for directly
  (a probed id that is not a meter is left alone). The test's failure
  message now lists the unit's controls.

  </details>

- **Polyphonic pitch editing: every note of a chord moved on its own**

  <details><summary>Details</summary>

  The Pitch editor's new Polyphonic option (on by default; off is the
  melodic editor as before) finds every note of a recording, chords and
  overlapping lines too, and edits each on its own: move it, correct it
  to the key, straighten it, make it louder or quieter, mute it, or forget
  a note found where none was played.

  - Detection: the transcription model finds the notes (overtones of
    louder notes are left out); each note is followed to its pitch every
    5 ms, to the cent, from its own harmonics' exact peaks.
  - The sound (new crate faderframe-polypitch): in an STFT, each note's
    harmonic peaks are found near the multiples of its pitch (shared
    peaks split by the notes' levels) and moved by its ratio with
    phase-locked peak shifting; noise and attacks no note owns stay put.
    Long windows where notes sound together (their partials apart), short
    ones for a note alone (vibrato followed). Only changed notes' spans
    are processed; everything else stays sample for sample.
  - The clip plays a render made on a worker once an edit settles. It
    enters the clip without an undo step of its own: the edit is the
    step, and undo and redo bring back the matching audio with the notes.
  - Clips with spectral edits or clip effects keep to the melodic editor
    for now.

  Tests: in a C–E–G chord, E moved a semitone — its partials 46–64 dB
  down where they were and arriving within a dB, C and G unchanged to
  0.01 dB; a note taken out; each note followed to a cent; vibrato
  straightened; untouched audio identical; a recorded chord found, moved,
  undone, redone, muted and its edit removed through the session. The
  melodic tests choose the melodic editor.

  </details>

- **Voice to MIDI in realtime: tracked on the audio thread, notes in ms**

  <details><summary>Details</summary>

  A sung note used to sound 35–40 ms after it began (plus a polling
  thread and a block). Now:

  - The tracker runs inside the engine's callback: its notes join that
    callback's live MIDI on the voice port at the frame they were
    decided, so the instrument plays them at once. No polling thread, no
    wait for the next block; recordings still place each note where it
    was sung (its `early`), and a copy goes to the control side for
    Capture MIDI and the activity lights.
  - Faster hearing: analysis every 1 ms instead of 5, over the shortest
    window that holds enough periods (12 ms first, longer only for low
    voices; while a note sounds, 2.6 of its periods), and a Response
    setting (Preferences → MIDI → Voice to MIDI): Fast (two frames at a
    lower clarity bar), Balanced, Stable.

  Voice to sound through the engine (before the device's buffers): A4 in
  6 ms (Fast) / 10 ms (Balanced), A3 in 11 / 15 ms — about two and a half
  periods of the note, the least its pitch can be heard in.

  Tests: onset latencies by key and response; voice to sound through the
  engine; tracking on the audio thread does not allocate; the sung phrase
  still placed within 15 ms at every response; the session test.

  </details>

- **Singing into MIDI live: voice ports**

  <details><summary>Details</summary>

  Every input of the audio device now has a MIDI input port, "Voice · In
  n". Choose it as an instrument or MIDI track's MIDI input and sing (or
  play a monophonic instrument) into that input: the track plays the
  notes as you sing, records them where they were sung, captures them
  with Capture MIDI, and MIDI tracks pass them to external synths.

  - The tracker (faderframe_analysis::voice) is causal: the input low-
    passed and decimated to ~11 kHz, McLeod's pitch over the last 40 ms
    every 5 ms, a level gate; a note starts once a key has held for 15 ms,
    changes after 25 ms of another, with half a semitone plus hysteresis
    so vibrato and scoops stay on the note, and ends after 40 ms of
    silence. Each event is dated to when its sound began.
  - The engine copies the chosen input channel into a ring with the MIDI
    clock time of each callback; a thread runs the tracker and sends what
    it hears into the MIDI input queue, stamped with when it was sung.
  - Live input stamped earlier than a block is still played at the
    block's start, but recorded where it was stamped
    (MidiInputBlock::early).
  - Preferences → MIDI → Voice to MIDI: sensitivity, steady notes or
    glides (as note expression, or pitch bend ±2), snapping to the
    project's key.
  - The pitch detector no longer allocates per call.

  Tests: a sung phrase (vibrato, legato) becomes its notes placed within
  25 ms of where they were sung; quiet input plays nothing; the scale
  snaps; a strong second harmonic keeps the fundamental; an early-stamped
  event plays now and records where it was; the input tap does not
  allocate on the audio thread; the dummy device's sung tone plays and
  records on an instrument track through the session.

  </details>

- **Screen readers and keyboard control for the canvas views**

  <details><summary>Details</summary>

  Every view was a single unnamed box to a screen reader. Views now
  describe their controls (`CanvasView::accessible`: role, name, value,
  state, place, and what Enter and the arrows do), and the canvas host
  turns them into GTK accessible objects under the view, so AT-SPI
  readers (Orca) walk them:

  - the mixer: a group per strip with the volume and pan sliders, mute,
    solo and record arm, and the inserts (Enter opens the editor);
  - the arranger: a group per track with its header toggles and volume,
    and its clips with where they start and end (Enter selects one and
    puts the playhead on it);
  - the transport display (position, tempo as a spin button, meter), the
    piano roll's notes (Enter selects, Up/Down transpose), the MIDI event
    list, the setlist and its stage screen, the undo history;
  - every stock device editor: sections as groups, knobs as sliders,
    switches as toggles, stepped values a step at a time.

  Keyboard: Tab into a view lands on its first control; Tab and Shift+Tab
  move along them with a focus ring, Enter presses, the arrows move
  values (Page keys ten steps), Escape or a click leaves. The arranger
  keeps Tab for tab-to-transient: Ctrl+Tab starts on its controls. When a
  list's own selection moves, the keyboard follows it.

  The focused control is the view's active descendant and, on GTK 4.18+,
  its focused state (looked up at run time: FaderFrame still runs on GTK
  4.14); changes are announced, as are transport changes and session
  notices; on/off transport buttons report their pressed state; every
  docked view is named by its tab.

  Tests: the access model; the mixer's, arranger's and a stock device's
  controls stepping and toggling; checked on the real AT-SPI bus with the
  running app (focus, value, pressed and announcement events).

  </details>

- **MIDI 2.0: devices, per-note expression in and out, clip files**

  <details><summary>Details</summary>

  Universal MIDI Packets (`faderframe_midi::ump`): every message type the
  DAW meets (utility, system, MIDI 1.0 and 2.0 channel voice, SysEx7, flex
  data, UMP stream), the specification's value scaling, MIDI 1.0 ↔ 2.0
  translation (bank select and RPN/NRPN folded in), and per-note pitch
  bend, absolute pitch and the registered per-note controllers mapped to
  the host's per-note expression.

  MIDI 2.0 devices on Linux: FaderFrame is a MIDI 2.0 client of the ALSA
  sequencer (new crate `faderframe-midi-ump`, the C API confined to one
  module). MIDI 2.0 hardware and programs appear as inputs and outputs
  ("… (MIDI 2.0)") and are no longer opened a second time as MIDI 1.0;
  programs may also send straight to FaderFrame. Per-note pitch bend and
  controllers (and a note-on's pitch attribute) are played live as note
  expression at full resolution, and recorded and captured into clips'
  note expression. MIDI tracks play MIDI 2.0 outputs in MIDI 2.0: 16-bit
  velocities, and the clips' note expression as per-note pitch bend and
  controllers (MIDI 1.0 outputs still leave it out).

  MIDI 2.0 Clip Files (`.midi2`): File → Export MIDI 2.0 Clip… writes the
  selected MIDI clip (16-bit velocities, 32-bit controllers, programs with
  their banks, note expression, SysEx, the project's tempo, meter and key,
  the clip's name); Import MIDI File… and drops read them back, MIDI 1.0
  messages in them too.

  Tests: packet and clip-file round trips; MIDI 2.0 input captured with its
  expression and played out to a MIDI 2.0 port (session); a clip through a
  .midi2 file and back; opt-in tests on the real sequencer (two MIDI 2.0
  clients; a stand-in device through the hub and the outputs). Checked by
  hand with `aseqsend -u 2` into the running app. The two opt-in Midi
  Through tests no longer race each other under nextest.

  </details>

- **GPU painter on by default; Home brings the arranger to the start**

  <details><summary>Details</summary>

  - Drawing analysers, meters and curves on the GPU is the default. The
    preference is now on until it is chosen in Preferences
    (`Preferences::gpu_painter_chosen`): older preference files stored it
    off for everyone, and they come on with the new default too. Setting
    it in Preferences is kept from then on.
  - After a jump of the playhead the arranger follows it: Return to Start
    (Home) scrolls it back to the start, whether Follow Playhead is on or
    not; other jumps (markers, locates) bring the playhead into view when
    it is out of it and Follow Playhead is on.

  </details>

- **Setlist and show mode**

  <details><summary>Details</summary>

  View → Setlist: a show's songs in order — one per section (Songs from
  Sections) or a range of the project (+ Song from Selection) — each with
  what follows it (stop, the next song at once or after a gap, play on)
  and the performer's notes; renamed, annotated, reordered and removed in
  the list, each edit one undo step.

  Show Mode (the Setlist's button, or View → Show Mode) makes it the
  stage screen, full screen: the song playing or stood on in large type,
  its time and what is left, the part now and next (from the sections),
  the lyric line, the notes, the list with what was played and what is
  next, and big Previous / Play-Stop / Next buttons — Space, the arrows,
  Enter and Esc on the keyboard, and MIDI learn for foot switches (Next,
  Previous, Play/Stop).

  Every song stops on its exact last frame: the engine cuts the callback
  there (`EngineController::stop_at`), then the show stands on the next
  song, and plays it after its gap when it says so.

  Tests: the exact stop in the engine; a three-song show played in real
  time on the dummy device (the stop, the gap, playing on into the next
  song, the stop at the end); setlist edits and undo; the list and the
  stage screen.

  </details>

- **Song structure from a jam**

  <details><summary>Details</summary>

  An audio clip's menu (or several selected clips of a multitrack jam) →
  Find the Song's Structure: the recording's parts are found and laid on
  the arranger's section lane — Intro, Verse 1, Chorus 1, … Bridge,
  Solo, Outro — coloured by which parts are alike, in one undo step; on
  the project's bars when it plays at the jam's tempo, else where the
  parts are heard. Pauses in a jam stay without a section.

  `faderframe_analysis::structure`: bars by the detected tempo (the
  downbeat where the harmony changes), each described by its chroma,
  timbre (the shape of 20 log bands) and loudness; boundaries where the
  four bars before and the four after differ most (their means, so a
  chord cycle within a part is not a change; loudness changes count,
  four-bar phrases preferred); parts compared along the bar similarity
  matrix's diagonals and the alike given a letter, named by repetition,
  loudness and place.

  Tests: a synthetic song (every boundary, verses and choruses alike,
  intro and outro named), silence as a gap, a two-file jam in a session.

  </details>

- **Meter styles: VU, PPM, K-System, peak + RMS; a VU meter bridge**

  <details><summary>Details</summary>

  Each strip's meter can show (right-click the meter, or the strip's
  Meter submenu; one strip or every strip):

  - Peak: sample peaks with a held peak (as before).
  - Peak + RMS: the RMS (300 ms) as the lit bar, the peak one LED above.
  - VU: a moving-coil VU's movement (99 % in 300 ms, 1.5 % overshoot) on
    the VU scale by voltage, 0 VU at −18 dBFS RMS or another reference
    (−20…−12), green below −3 VU, red past 0.
  - PPM (EBU): the quasi-peak programme meter, measured sample by sample
    (10 ms integration: a 10 ms burst reads −2 dB; 24 dB fall in 2.8 s),
    TEST at −18 dBFS, red from +9.
  - K-20, K-14, K-12: Katz's K-System, the RMS bar with the peak above it,
    yellow from 0, red from +4.

  Every style lights the skin's own meters (LEDs, bars, plasma columns).
  The mixer's VU Meter Bridge puts a moving-coil VU meter over every strip
  and the master (two for a wide stereo strip), with its peak LED.

  Under it: the engine's meter bank also sums energy (the session gets the
  true RMS of every UI frame, not the loudest block's) and runs the
  quasi-peak envelope only for channels a PPM shows; the session adds the
  RMS, the VU needle and the PPM to its meter state. Settings are saved
  with the layout.

  Tests: the EBU ballistics, the VU movement, the scales and colour zones,
  the meter menu, the bridge.

  </details>

[All changes since 0.17.0](https://github.com/BurningTreeC/faderframe/compare/v0.17.0...v0.18.0)

## [0.17.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.17.0) — 2026-10-08

- **Snap to Samples and Snap to Zero Crossings**

  <details><summary>Details</summary>

  Two toggles beside the grid in the edit toolbar:

  - Snap to Samples (on by default): positions the arranger makes freely —
    snap off, Alt-drags, Slip, the relative grid — land on whole samples,
    so a cut or trim placed at sample zoom is exactly where it shows. Grid
    positions stay musical (a grid line at most tempos is not a whole
    sample; rounding it would pull it off the grid).
  - Snap to Zero Crossings: the cuts of Separate, split at the playhead and
    Trim to Selection, and trims of audio clips, move to the nearest zero
    crossing of the clip's audio (within 5 ms, per clip), so they do not
    click.

  `Session::{on_sample, cut_at}`, `EditFlag::{SnapToSamples,
  SnapToZeroCrossings}`, dev action names `edit-flag:samples` and
  `edit-flag:zero-crossings`.

  Tests: zero-crossing cuts and trims, sample rounding, a Slip drag in the
  arranger landing on a sample.

  </details>

- **Spectral editing**

  <details><summary>Details</summary>

  An audio clip's menu → Spectral Editor shows the clip's spectrogram
  (its source over the clip's region, a logarithmic frequency axis, zoom
  with Ctrl+wheel). Regions drawn on it — a rectangle, a time range, a
  frequency band, a lasso, a brush stroke — become edits:

  - Attenuate: down to the sound around it in time, only where it stands
    out (coughs, clicks, squeaks; quieter parts stay).
  - Heal: replaced by the sound around it, each bin's phase carried on
    from before, so a tone continues through a dropout.
  - Remove, or Gain by a value.

  Edits have soft edges in time and frequency, may be on one channel,
  stay editable (select, move, change the operation or softness, delete)
  and show as outlines; "Original" shows the unedited audio.

  How it works:
  - `faderframe_project::spectral`: the edits with the clip, in its
    source's frames and hertz (`AudioClip::spectral`); the unedited source
    kept.
  - `faderframe-spectral` (new, pure): each edit an STFT pass over the
    span it reaches, frames as long as fit the edit (85 ms down to 256
    samples, so a click is not smeared), identity frames where the mask is
    empty, and everything the edits do not reach copied bit for bit;
    spectrograms for the editor.
  - `session::spectral`: changes show at once, render after a moment in a
    worker into a processed copy of the source the clip then plays (same
    frames: warps, pitch edits and trims carry over), one "Spectral Edit"
    undo step; with clip effects the edits go before them and the effects
    render again; the Pencil redraws the unedited source too. Pictures
    come from a worker, the newest request first.
  - `faderframe-view-spectral` (new): the editor.

  Tests: remove, gain, attenuate a burst, heal a dropout, brush, lasso,
  one channel, bit-identical identity, spectrogram levels; the session's
  render, undo, clear and clip effects; the view's tools, edits, moves,
  picture and zoom.

  </details>

- **MIDI keeps everything: program changes, poly pressure, release velocity**

  <details><summary>Details</summary>

  Nothing a MIDI file or a performance carries is dropped any more:

  - Program changes and polyphonic key pressure are controller lanes
    (`MidiController::{Program, PolyPressure { key }}`); a note's release
    (note-off) velocity is `MidiNote::release` (`None` sends 0, as before).
  - MIDI file import keeps them (it used to count program changes and drop
    them, and dropped poly pressure and note-off velocities); export writes
    them, a bank select before its program; a file round-trips.
  - Recording, Capture MIDI and playback carry them; the clip player
    chases programs on locate after the CCs, so the bank goes first. The
    piano roll offers a Program lane; poly pressure lanes appear once a
    clip has them.

  The event list shows and edits every property: notes (position,
  channel, key, velocity, length, end, release velocity, mute — M toggles
  at a click), control changes, program changes (1–128 with their General
  MIDI name, new `faderframe_midi::gm`), pitch bend, channel and poly
  pressure, the points of the notes' expression curves (in their units),
  and SysEx (its bytes typed in hex). "+ Add" makes any of them; a row's
  menu sets the release velocity of the selected notes; an Expression
  filter joins Notes, Controllers and SysEx.

  Tests: the MIDI file round trip, the engine's playback of programs, poly
  pressure and release velocities, the event list's edits of every kind,
  SysEx parsing, GM names.

  </details>

- **MIDI event list**

  <details><summary>Details</summary>

  A dock view (View → MIDI Event List; a MIDI clip's menu → Edit in Event
  List) listing the edited MIDI clip's events a row each: notes, control
  changes, pitch bend, channel pressure and SysEx, filtered by kind, with
  position, channel, key or controller number, velocity or value, length.

  - A double-click types a field: positions in the main counter's unit,
    keys by name ("F#3") or number, lengths as beats.ticks or a note value
    ("1/8"), pitch bend as an offset from the centre.
  - A vertical drag on a number changes it, previewed and committed once on
    release.
  - "+ Add" (at the selected event, else the playhead) and each row's menu:
    a note, a control change (common ones and all 128), pitch bend, channel
    pressure; the selected rows' channel; mute notes; move the playhead
    there; delete. Delete, Ctrl+A, arrows, Shift/Ctrl-click work too.
  - Note rows share the session's note selection with the piano roll; the
    row at the playhead is marked and kept in view while playing; a click
    in the first column locates to the event.
  - Edits are now `Action::EditMidiEvents` (several events, one value, one
    undo step), so a channel change covers the whole selection.

  Tests: the view's (paint, drag, typing, add menu, filter, row menu,
  delete, locate, key and length parsing) and the session's.

  </details>

- **MIDI channels from the menus; the event list's session operations**

  <details><summary>Details</summary>

  - Piano roll: the notes' menu → Channel ▸ 1–16 (checked when the
    selected notes share one).
  - Arranger: a MIDI clip's menu → MIDI Channel ▸ 1–16 puts every note
    and controller value of the clip (or the selected clips) on that
    channel, controller lanes that meet merged — one undo step
    (`Action::SetMidiChannel`, `Session::midi_channel_of`).
  - `session::midi_events` for the event list: a clip's events in time
    order (`midi_events`: notes, control changes, pitch bend, channel
    pressure, SysEx), `Action::{EditMidiEvent, RemoveMidiEvents,
    AddMidiEvent}` — a field each (position, channel, key or controller
    number, velocity or value, length); a controller value moved to
    another channel or number changes lanes, emptied lanes go.

  Test: `the_event_list_edits_a_clips_events`.

  </details>

- **Gain-reduction meters for every plugin format that reports one**

  <details><summary>Details</summary>

  The mixer's gain-reduction stripe now shows what any compressor on the
  track reports, not only the built-ins:

  - `PluginInstance::reduction()`: a `Reduction` cell (atomic f32 in dB,
    NaN while nothing is reported) the processor sets after each block on
    the audio thread; the session reads it for every device in a track's
    input stage and inserts.
  - CLAP: the draft gain-adjustment-metering extension
    (`clap.gain-adjustment-metering/0`, what REAPER reads), its `get`
    called after `process`.
  - VST3: a read-only parameter named as gain reduction, caught in the
    block's output parameter changes (the documented route for meters) and
    converted by a table measured from the controller: dB unit, else the
    dB its text shows, else a 0…1 linear gain.
  - LV2: gain-reduction output control ports by name/symbol (Calf's
    "compression", LSP's "rlm", "GR"…), by `units:db`/`units:coef` or the
    range.
  - AU: a read-only meter named so (Apple's Dynamics Processor's
    "Compression Amount"), read after each render.
  - Sandboxed plugins (the default for CLAP, VST3, AU): the helper writes
    the reduction into the shared memory each block, the proxy hands it on
    (shared-memory version 5).
  - Built-ins report through the same cell.

  Tests: CLAP (the test plugin implements the extension), VST3 (the test
  plugin's meter), LV2 (a gain-reduction port), the sandbox round trip,
  AU against Apple's Dynamics Processor (macOS CI), the shared helpers.

  </details>

- **Mixer: a gain-reduction stripe beside each strip's meter**

  <details><summary>Details</summary>

  A thin stripe left of the meter shows what the track's compression
  takes off now, from the top down on the meter's own scale (6 dB taken
  off reaches the meter's −6), up at once and falling back over a few
  frames, as a console's GR meter does. It shows on strips whose input
  stage or inserts hold a compressor (not bypassed), the reductions of
  several in series added; its tooltip names them. A click on it does
  nothing (it sits over the fader's slot).

  The built-ins report it: Compressor, 76 Compressor, Limiter, De-esser,
  Channel Strip (its compressor) — `devices::reduction_value`,
  `Session::{gain_reduction, gain_reduction_devices}`. Gates are not
  compression and are left out. Other formats follow.

  Test: `a_compressing_track_shows_its_gain_reduction`.

  </details>

- **British 73: its output block driven from the trim, into 600 Ω or bridging**

  <details><summary>Details</summary>

  The British 73 was dull at the top (−1.5 dB at 10 kHz, −4.1 dB at
  20 kHz) because its output block was fed from 470 kΩ: a shortcut that
  held the block at no gain (instead of the 18 dB the assembly guide
  states) and turned its input capacitance into a low-pass.

  - The block is driven from the card's output trim (about 2.5 kΩ, as on
    the drawing), so it does its 18 dB and is the stage that runs out
    first — the output stage and transformer colouring before the gain
    stages.
  - Into the 600 Ω line it was built for by default (+0.8 dB at 20 kHz);
    the preamp's menu → Output Load → 10 kΩ (Bridging) for a slightly
    more open top (+1.6 dB at 20 kHz, the estimated leakage) and about
    8 dB more swing, at the same level (`BRIDGING_DB`); the plate reads
    "· 10 k". Changing it restarts the processor (its circuit changes).
  - Recalibrated on the circuit as upstream did (`tests/
    british73_calibration.rs`, which gives back upstream's figures for the
    old feed): 3 % at full gain for −18 dBFS, unity at the middle.

  Tests: `the_british_73_drives_its_line_like_the_card`, the allocation
  check with the bridging line.

  </details>

- **Mixer: folders have strips of their own, strips drop onto folders**

  <details><summary>Details</summary>

  - A folder's strip: narrow, its colour, its triangle (open or closed),
    mute and solo (reaching what it holds), how many tracks it holds and
    its name (double-click renames). Its tracks carry a band of its colour
    along their foot (one per level), and a closed folder's strips fold
    away from the mixer as its rows do from the arranger (the same fold).
  - A strip dragged onto a folder's strip (over its middle) goes into the
    folder; between strips it lands exactly there, folders included (the
    arranger's rule: right of a folder before what it holds = first in
    it); a folder's strip moves the folder with what it holds.
  - Channel numbers count channels, not folders.

  Test: `folder_strips_fold_mute_and_take_dropped_strips`.

  </details>

- **Arranger test: no needless mut**

- **Arranger: drag track headers to reorder them, or onto a folder**

  <details><summary>Details</summary>

  A track's name or the free part of its header carries it: past a few
  pixels a line shows where it lands between the rows (or a folder's row
  lights up when it is over its middle), the rows scroll at the edges,
  and on release it goes there — `Action::PlaceTrack` between rows,
  `Action::MoveToFolder` onto a folder. A folder moves with what it holds.
  A click still only selects.

  `place_track_commands` for rows with folders: right under an open
  folder's header, before what it holds, a track goes first into it; a
  folder is never placed into itself or a folder inside it.

  Tests: `every_row_lands_where_it_is_dropped_folders_included` (every
  track and folder into every gap of the rows, each one undo step),
  `track_headers_drag_to_reorder_and_into_folders`.

  </details>

- **Mixer: dark ink on a light bus amplifier plate (the German 76's)**

- **Console summing: six families, one for each preamp**

  <details><summary>Details</summary>

  Tube 610, British 47 and German 76 join American, British 4K and
  British 73. The valve desks summed passively and made the loss up with a
  booster of their own kind at a fixed gain: the 610 module (Level at
  mid), the REDD.47 at 34 dB, the V76 on its 34 dB step with its 40 Hz
  high-pass and 15 kHz low-pass (`console_bus::control`). Calibrated like
  the others (+4 dBu at −18 dBFS), the small-signal gain now measured at a
  millivolt (a tenth of a volt drove a valve booster nonlinear).

  Their channel models are baked like the others', with what the valves
  needed:
  - the response model: a further first-order high-pass, a first- or
    second-order low-pass matched to the analog magnitude up to Nyquist
    (`matched_low_pass`, tested against the analog one; the one-pole
    matched too, where corners above 0.45 of the rate were switched off
    before), corner and Q nudged on the digital stage, the stage's DC
    blocker allowed for;
  - the emphasis bands' corners chosen per family from nine pairs against
    checks at 60 Hz, 200 Hz, 1 kHz and 5 kHz, a wider fit (±12 dB).

  Tolerances: as before for the solid-state families; the valve ones'
  distortion within a factor of 2.5 or 0.1 %, their response within
  0.4 dB at 20 kHz, no gain judged past 20 % THD (+28 dBu into a
  booster). Their bus circuits cost 16–34 % of a core per channel on a
  hot signal (the solid-state ones 4–8 %); the channel stage 0.35 %.

  Looks: taupe and Bakelite with mahogany, hammertone with oak, light
  enamel grey with beech.

  </details>

- **Console in the master's slot, each bus its own amplifier**

  <details><summary>Details</summary>

  The master's input slot chooses the console for the whole mix (off or a
  family, the channel drive, the look; its × turns the console off). A
  bus's or return's slot is its own bus amplifier, and its menu says so:

  - Follow the Console (British 73) — checked while it does;
  - Through Another Console's Bus Amplifier ▸ — hybrid summing, e.g. the
    drums through the British 73 while the mix runs through the American;
  - Drive ▸ quick values, Bypass;
  - In the Box on This Bus;
  - Colour Through a Microphone Preamp ▸.

  `Action::SetBusAmplifier { track, family }` (refused on the master:
  its amplifier is the console's). Switching the console moves only the
  buses that follow it; a bus through another console's amplifier or in
  the box stays as chosen. Empty slots read "+ CONSOLE" (master), "+ BUS
  AMP" (buses), "+ PREAMP" (channels).

  Tests: `each_bus_has_its_own_amplifier`,
  `the_master_chooses_the_console_and_a_bus_its_amplifier`.

  </details>

- **Console looks: meters that show the level, each family's own wood**

  <details><summary>Details</summary>

  The American and British 73 looks used the edgewise VU meter, which on
  a narrow strip is a backlit face filling the column with a thin needle
  across it: it read as a meter always full. They use an LED ladder and a
  gas-plasma column now (the British 4K keeps its bar).

  Each family has its wood (walnut, ash, teak): the British 73 shows its
  cheeks always, the others whenever the skin has wooden cheeks of its
  own (Vintage Console keeps them under every console look).

  </details>

- **Preamps: 0 % Gain is silence on every model, reached gradually**

  <details><summary>Details</summary>

  Only the Tube 610 (its Level pot) and nearly the German 76 went silent
  at 0 %; the others stopped at their circuit's lowest gain (−26 to −46
  dBFS for −18 dBFS in).

  The bottom fifth of the Gain's travel now closes the input like a level
  pot (square law: −12 dB at 10 %, −24 dB at 5 %), on top of the
  circuit's own range, so the level falls steadily to silence at 0 %
  with no step at once. The 610's pot closes to about −80 dBFS by itself;
  only its last 2 % finish the way.

  Test: `gain_moves_the_level_as_the_circuit_does` (silent at 0 %, falling
  by under 15 dB per 2 % of travel from 20 % down, never rising).

  </details>

- **76 Compressor: fourteen factory presets**

  <details><summary>Details</summary>

  What the FET limiting amplifier is reached for, each level-matched to
  the dry signal (within 0.1 dB on the test material):

  Vocal Upfront, Vocal Gentle, Vocal Grit, Bass Even, Bass Growl, Kick
  Snap, Snare Crack, All Buttons Room, All Buttons Parallel (the smash
  under the dry, inside the unit), Drum Bus Crunch (sidechain high-pass
  at 120 Hz), Acoustic Strum, Guitar Sustain (12:1), Peak Catcher (20:1,
  fastest attack) and Line Amp Colour (attack off: the line amplifier
  alone, driven).

  The 76 joins the presets' tests (10–15 each, distinct, in range,
  complete when loaded, audible and in bounds on the test material).

  </details>

- **View menu: every docked view is checked, not only the one in front**

  <details><summary>Details</summary>

  The checks followed `is_showing` (the active tab of a visible group), so
  of a dock's tabs only the front one was checked. They follow
  `is_placed` now: docked, behind another tab or in a window of its own.
  A click still closes a view on screen and opens one that is not; one in
  a tab behind another comes forward first (so F2–F12 never close what
  cannot be seen), its tab's × closes it at once.

  Test: `every_docked_view_is_checked` (the default workspaces' tabs
  behind others included).

  </details>

- **Console summing: the mix through a console's circuits**

  <details><summary>Details</summary>

  Audio → Console (or the master strip's menu): American, British 4K or
  British 73, or off (in the box) — one undo step.

  - Buses, returns and the master sum through the console's bus amplifier
    circuit: built-ins `builtin::CONSOLE_BUSES` in their input stage
    (preamp models `MODELS + family` through the preamps' buffered
    processor: reservoir, worker pool, deadlines, latency compensated). Its
    Drive (±12 dB, taken off after the circuit: colour, not level) and
    Output show on the faceplate. Switching families keeps them; a bus with
    a microphone preamp keeps it; buses added later get theirs.
  - Audio and instrument channels go through the console's line amplifier
    as the baked light model (`ConsoleStage`, 0.3 % of a core per channel),
    in front of the strip wherever the strip is built (pre-fader sends carry
    it), with a channel drive from a parameter slot (no rebuild, ramped).
  - `Project::console` with `Command::{SetConsole, SetConsoleDrive,
    SetConsoleLook}`; `builtin::is_input_stage` where input-stage devices
    are special (never inserts or container devices).
  - The mixer takes the family's look (`Theme::with_console_family`: the
    console section's panels, knobs, faders, meters and finish in
    FaderFrame's own design — graphite and brass, slate with colour-coded
    caps, pewter and walnut) unless the look is turned off.
  - The console bus models' Gain is now the drive in dB.

  Dev actions `console:<off|american|british-4k|british-73>`,
  `console-drive:<dB>`, `console-look`.

  Tests: `session/tests/console.rs` (one step, latency to the sample,
  level kept at the nominal level, colour when driven, settings kept across
  families, new buses, off), `the_console_does_not_allocate` (all
  families, inline and on workers, drive changes, reset), the mixer's
  `a_console_shows_its_bus_amplifiers_and_its_look`, the looks' contrast.

  </details>

- **Console channel stage: the bus circuits baked into a light model**

  <details><summary>Details</summary>

  Every channel gets what it would pick up going through the console's
  line amplifier without solving a circuit per channel: each console bus
  circuit (American, British 4K, British 73) measured and baked
  (`tests/console_bake.rs`) into `console::ConsoleStage` and held against
  the circuit by `tests/console_match.rs`.

  - The small-signal response as a high-pass, low-pass, bell and shelf.
  - Transfer curves at 25 levels from −24 to +12 dBFS (1.5 dB apart),
    each relative to its level: the British 73's AC-coupled class-A stage
    moves its operating point so it clips near the peak at any level, and
    relative to the level neighbouring curves have their knees in nearly
    the same place, so mixing two moves the knee instead of making two
    (absolute curves made the distortion dip and jump between levels).
    A peak follower with a 25 ms hold chooses them (no movement inside a
    low note's period).
  - Emphasis bands per level (a low and a high one-pole band, the gain
    into the curve undone exactly after it, then a correction): the
    British 73 clips later in the lows and compresses harder there when
    hot, which one fixed emphasis cannot do. Fitted where the circuit
    distorts audibly and the curve can follow it.

  Matched within tolerance at 60 Hz, 1 kHz and 5 kHz from the nominal
  level to +6 dBFS (distortion within 35 %, or within what the circuit
  has a quarter of a dB either side on the British 73's tenfold-per-dB
  onset; gain within 0.3 dB; response within 0.3 dB), the distortion
  rising with the level all the way. Not modelled: the British 73's slew
  distortion at 5 kHz at levels where its curve is straight (≈ −60 dB).
  0.29 % of a core per channel against 3.5–6.3 % for the circuits.

  </details>

- **Preamps: Gain is real gain on every model; console bus circuits**

  <details><summary>Details</summary>

  The preamps' output makeup followed the Gain, so on five of the six
  models Gain only drove the circuit harder at the same level (−20.5 dBFS
  from 0 to 100 %); only the Tube 610 (its Level pot) changed the level —
  which is why it went silent at 0 % and the others did not.

  - The calibration holds the level at the middle of the travel (−18 dBFS
    in, about −20 dBFS out); from there the level follows the circuit's
    own gain: American 312 34 dB of range, British 4K E 50 dB, German 76
    73 dB, British 47 its switch's three positions, Tube 610 closing to
    silence (`preamp::level_change_db`).
  - The British 73 is wired as its switch's 70 dB position and its control
    sweeps only the PRE 2 feedback leg (10 dB): the lower half of its
    travel is now the switch's input divider (up to 35 dB, even in dB,
    smoothed), so its Gain spans about 45 dB.
  - Projects saved before keep their settings and now play at the level
    their Gain gives.

  Also the console mix-bus circuits for the coming console summing
  (`circuits::console_bus`: American 2520 virtual earth into the 2503,
  British 4K two 5534 stages, British 73 line amplifier; preamp models
  `MODELS + family`, drive and output with the polarity and small-signal
  gain measured at construction; tests in `tests/console_bus.rs`).

  Test: `gain_moves_the_level_as_the_circuit_does`.

  </details>

- **Alt is the edit modes' and tools': no menu bar mnemonics, app shortcuts**

  <details><summary>Details</summary>

  Holding Alt armed the menu bar's mnemonics (_File, _Edit, T_ransport …),
  and Alt+1 … Alt+0 only worked while the arranger had the keyboard focus
  (its own key handler).

  - The menu bar has no mnemonics (F10 still opens it).
  - Alt+1–4 (Shuffle, Slip, Spot, Grid), Alt+5–0 (Zoom, Trim, Select,
    Grab, Scrub, Pencil) and Alt+S (Smart) are application shortcuts
    (`app.edit-mode::…`, `app.edit-tool::…`): they work wherever the
    keyboard is and are in the shortcut editor.

  Checked with wtype in the running app: Alt+1 switches Grid to Shuffle
  with nothing focused in the arranger.

  </details>

- **Folders test: no needless mut**

- **Mixer: a dragged strip lands exactly where it is dropped**

  <details><summary>Details</summary>

  The mixer shows tracks in folder order but the drop moved the track by
  an index in the flat track list: next to a track in a folder (or across
  folders) it landed elsewhere, or nowhere — it kept its folder and went
  back. And a release a little above or below the strips cancelled it.

  - `Action::PlaceTrack { track, after, before }` (the strips either side
    of the drop; `Session::place_track_commands`): the track keeps its
    folder when a neighbour shares it and goes right after or before that
    neighbour in the list, else it joins the folder of the strip it lands
    after (or before, at the start) — one undo step, so it shows exactly
    in the gap.
  - Only across counts: the height of the pointer does not cancel the
    drop, and beyond the strips it takes the first or last gap.

  Tests: `every_strip_lands_where_it_is_dropped` (every strip into every
  gap with folders nested two deep, each one undo step, undone cleanly),
  `a_strip_lands_by_where_it_is_across_even_off_the_mixer`.

  </details>

- **Dock tabs: a close button on each (the arranger stays)**

  <details><summary>Details</summary>

  Every dock tab has an × beside its detach button: the view leaves the
  layout (WorkspaceAction::CloseView); View shows it again, in its place in
  the tab order.

  </details>

- **Render-ahead session test: a starved anticipator is not a stale ring**

  <details><summary>Details</summary>

  The Windows runner's anticipator was late 10 times in a paced run; a
  stale or mis-keyed ring is late about 60. Up to 20 now.

  </details>

- **Lead sheets of whole tracks, chords heard from the audio**

  <details><summary>Details</summary>

  - A lead sheet of a track (`LeadSheetOf::Track`, the arranger's track
    menu "Lead Sheet of This Track"): all its clips, the whole song, audio
    clips' melody heard and MIDI clips' notes read together; the bars from
    its first clip to its last (empty bars as bar rests).
  - Chords without a chord track or MIDI: the other audio tracks over the
    span are heard by basic-pitch in the same thread (tracks named like
    drums left out) and their chords found half a bar at a time.
  - The clef by the part's middle note: bass clef for a bass line (or a
    baritone), treble 8vb for a tenor, treble above.
  - Dev action `lead-sheet:track:<name>`.

  Test: `a_track_of_clips_with_chords_heard_from_the_audio` (two clips on
  one track, C and F heard from a keys track's audio).

  </details>

- **Tempo through hit points: the steadiest tempo that lands every hit on a beat**

  <details><summary>Details</summary>

  For scoring to picture. Markers are the hits — all of them, the cuts
  found in the picture, or those in the edit range — and
  `faderframe_timeline::hits::solve` finds the step tempo map that puts
  each on a beat, an eighth or a bar line: between two hits a whole number
  of steps, chosen by dynamic programming over the counts whose tempo is in
  the range, each change of tempo costing its squared log (plus, if asked,
  the distance from a preferred tempo); the stretch before the first hit is
  free in the range; hits too close to the one before are left out and
  named.

  Applied as one undo step: the tempo map, the hit markers onto their
  beats (sounding where they did), the other markers and (by default) the
  audio clips kept where they sound, MIDI following the beats; the picture
  stays. A form (Tempo from Hit Points…: hits, land on, slowest/fastest/
  near, keep audio) previews the result as it is set; in the menus of a
  marker, the markers lane and the tempo lane; dev action
  `tempo-from-hits:…`.

  Tests: steady hits keep their tempo; uneven cuts land within the range
  under 15 % change, on bar lines too; a preferred tempo; too-close hits
  left out; the session's apply (markers on beats where they sounded,
  dialogue in place, the score on its bar, undo).

  </details>

- **Lead sheets from a recording: melody, chords and words as MusicXML and PDF**

  <details><summary>Details</summary>

  A clip's lead sheet, end to end (`faderframe-leadsheet`, pure):

  - The melody: an audio clip's heard by the pitch analysis (in a thread,
    kept for rewrites), a MIDI clip's read; the bars from the meter map;
    chords from the chord track, else from what the other tracks' MIDI
    plays; the key from the key track, else detected from the melody and
    the chords; words from the lyrics lane; the tempo.
  - Written out (`build`): one voice, quantised per beat to sixteenths or
    eighth triplets (Auto/16ths/Triplets), short gaps closed; notes cut at
    bar lines with ties and split into values a reader expects (nothing
    across a beat unless it starts on one, nor across the middle of a 4/4
    bar; compound metres by the dotted quarter); spelt in the key,
    accidentals once a bar; beams by the beat with sixteenth beams and
    hooks; triplet brackets; chord symbols on the nearest beat; each lyric
    line's words matched in order to its notes by time (shared out by
    syllables), melisma lines.
  - MusicXML 4.0 (validates against the schema) and an engraver: A4 pages
    of justified systems (up to four bars), room by value, accidental, word
    and chord, Bravura's glyphs (34, SIL OFL 1.1, baked with their SMuFL
    metadata by scripts/notation_assets.py) — drawn by a hand-written PDF
    1.4 writer (standard fonts, Core 14 widths) and by the new Lead Sheet
    view (`faderframe-view-leadsheet`: pages on paper, Ctrl+wheel zoom, the
    grid, Export PDF…/MusicXML…).
  - Session `MakeLeadSheet { clip, grid }`, `ExportLeadSheet(path)`; the
    arranger's clip menu (audio and MIDI clips); View → Lead Sheet (a check
    item, in the tab order); dev actions `lead-sheet:<track>[@grid]`,
    `export-lead-sheet:<path>`.

  Tests: values and beats, spelling, ties/rests/beams, triplets,
  accidentals and words, chords, syllables, MusicXML (well formed, bars add
  up), the engraver, the PDF's cross-reference table, the view's strip, and
  `a_recorded_melody_becomes_a_lead_sheet` (a sung phrase: its notes,
  values, key, chords and words, both exports).

  </details>

- **View menu: the dock's views tick on and off; tabs keep one order**

  <details><summary>Details</summary>

  - Every dock view in the View menu is a check item (`app.view-<name>`):
    ticked while it is on screen; unticking closes it
    (`WorkspaceAction::CloseView`, the arranger stays), ticking shows it
    or brings its tab forward (`ToggleView`). "Bottom Dock" is a check
    item too. The ticks follow layout rebuilds and tab switches
    (`dock::sync_view_actions`). F2/F3/F4/F8/F12 toggle the same way;
    the `show-*` actions still only show (scripts, Edit → Undo History).
  - Tabs keep one order whatever opened first: the arranger, the mixer,
    then as the View menu lists the views (`ViewKind::TAB_ORDER`,
    `WorkspaceLayout::order_tabs` on attaching, in the presets and for
    layouts saved before): a view closed and shown again goes back to
    its place, not to the end.

  Tests: `views_toggle_closed_and_open_again`, `tabs_keep_their_order`.

  </details>

- **Arranger: a click on a clip's header strip leaves the playhead alone**

  <details><summary>Details</summary>

  A click on a clip moved the playhead there (with Link Timeline on) also
  when it landed in the clip's header strip, where one picks the clip
  up. Now the header selects the clip and only a click in its body moves
  the playhead.

  Test: `a_click_on_a_clips_header_does_not_move_the_playhead`.

  </details>

- **Tests: a silent side at the meter's floor; the insert's tone by its peak**

  <details><summary>Details</summary>

  - The Guitar Station's mono-to-stereo test asked the right side to be
    30 dB under the left; with the left quiet at that moment (-48 dB) a
    silent right (the floor, -72) failed it. The floor counts now.
  - The hardware insert's session test read the 300 ms RMS, which the
    macOS runners' bursty dummy device reads low (-31…-38 dB at a -6 dB
    peak); its peak shows the tone is heard.

  </details>

- **Architecture: the 76 Compressor, hardware inserts, output tracks given back, the Guitar Station's buses**

- **76 Compressor: a FET limiting amplifier with a front panel of its own**

  <details><summary>Details</summary>

  A new built-in, "76 Compressor" (`faderframe.76-compressor`), written
  from how the classic 1176 behaves (its owner's manual), not from any
  other plugin's code.

  DSP (`plugin_host::devices::fet76`):
  - Feedback gain control solved without a sample's delay (Newton on the
    loop's equation), so the 20 µs attack stays stable at any ratio; the
    Input drives a fixed threshold, the Output is the make-up; soft knee.
  - Attack 800 µs (1) … 20 µs (7), release 1.1 s … 50 ms, 7 fastest; the
    release depends on the programme (a slow stage after long reduction).
    The attack's Off position disconnects the compression (the line
    amplifier and its transformer still colour the signal).
  - The ratio buttons go in in any combination, as pressing them together
    does on the hardware (`network`): their taps mix, so the ratio lands
    between theirs and higher buttons set the threshold higher; every extra
    button moves the bias (threshold down, attack lagging, release quicker,
    more FET distortion; with all four the ratio is between 12:1 and 20:1
    and louder input comes out quieter). None in: no compression.
  - Meter Off is the power switch: nothing comes through (with the Mix,
    the dry part is left).
  - Colour: the FET's even harmonics grow with the reduction and the signal
    across it, the class-A output stage and transformer round the peaks,
    the lows first; 2× oversampled, the Mix's dry path delayed to match.
    Stereo link, sidechain high-pass (60/120/240 Hz).

  Panel (`view-devices::fet76`): black anodised rack panel with knurled
  knobs, lit push-buttons (click: alone; Shift/Ctrl-click: pressed in with
  the others or let out), a backlit VU (gain reduction, or the output at
  +4/+8; with several buttons in its bias swings it the other way, all four
  pin it as the manual warns; standard ballistics: 99 % in 300 ms, 1.5 %
  overshoot), input and output peak meters either side with held figures,
  a GR readout; Mix, sidechain filter and link in the strip above. Meter Off
  darkens the lamps and the meter.

  Tests: the static curves and each button's ratio and threshold, button
  combinations, all-buttons over-compression and grit, Attack Off / no
  button / power off, release speeds, the fastest loop bounded, link, dry
  alignment, the needle's ballistics, the panel's buttons and knobs, no
  allocations on the audio thread.

  </details>

- **Plugin outputs: untick one to give it back to the main mix**

  <details><summary>Details</summary>

  - `Action::RemoveOutputTrack { plugin, bus }` removes the track taking
    that output, and the output folder when nothing else is left in it,
    as one undo step ("Remove Output Track"). The main's track going gives
    the plugin's track its routing back, as removing it by hand did.
  - The mixer's Outputs submenu and the editor window's Outputs menu: a
    ticked output removes its track (before, choosing it again did
    nothing); dev action `remove-output-track:<plugin id|insert=n>\n<bus>`.
  - The engine tells instances which buses tracks take
    (`PluginInstance::configure_taken_outputs`, `Project::taken_plugin_buses`):
    a Drum Sampler pad on an output with no track plays in the main again —
    also while a higher output keeps its track (that bus still has a
    buffer, which nobody heard).

  Test: `an_output_given_back_plays_in_the_main_again`.

  </details>

- **Shuttle test: only that snippets were heard**

  <details><summary>Details</summary>

  A tick sees a snippet only while it is still playing; a macOS runner's
  ticks came as far apart as a snippet is long (17 a second), so the share
  of ticks hearing one was a race. The frames running back are what show
  the shuttle moved.

  </details>

- **Guitar Station: Mic A and Mic B on outputs of their own**

  <details><summary>Details</summary>

  - The acoustic stage keeps each microphone's signal (calibrated, before
    the blend and the pan; an Off one silent: `AcousticStage::mics`), the
    chain hands them on in `Frame::mics` at the output's level, through
    the amplifier-switch crossfade (without microphones — direct or legacy
    cabinets — both are the output).
  - The amplifier's stage carries two more lanes a channel (the pedals'
    stages keep audio and DI), through the same reservoir, so the
    microphones stay in step with Main.
  - The device's outputs: Main, DI, Mic A, Mic B (Create Output Tracks
    makes a track for each, close and room apart); the microphones at the
    Output's level.

  Tests: `the_microphones_have_outputs_of_their_own` (all on A the main is
  A's output, all on B it is B's, the two differ), the session guitar test
  with four output tracks, both microphones' tracks sounding.

  </details>

- **Hardware inserts: outboard gear in the chain, the round trip pinged**

  <details><summary>Details</summary>

  - Built-in device "Hardware Insert" (`faderframe.hardware-insert`,
    `devices::hardware_insert`): send and return channels, send and return
    levels, mix, return polarity and the measured round trip.
  - The live graph builds it as a send (level → device output from the
    send channel) and a return (device input from the return channel)
    whose latency is the round trip counted from what reaches the send:
    `NodeSpec::after` orders a node after one it has no edge from and
    starts its latency there, so compensation holds the dry signal (the
    device's main input) and everything downstream back to match. The
    device mixes dry and return. Renders, which have no interface, pass it
    by (and say so); tracks with one never render ahead; changing its
    channels or round trip builds the graph again.
  - The ping (`EngineController::ping`/`ping_result`): an impulse onto the
    send's output after the graph's, the return's input watched, the
    frames from one to the other counted on the audio thread (a second at
    most). `Action::PingHardwareInsert` sets the device's Round Trip from
    it (one undo step) and says what it measured.
  - The device opens the channels it needs: the session asks for as many
    outputs and inputs as the master, hardware routing and hardware
    inserts want (`Session::wanted_channels`), and when the project needs
    more than are open the UI starts the device again (`UiRequest::ReopenAudio`,
    once per count, not while recording).
  - Its editor: the way out and back, the round trip and a Ping button,
    send / return / mix.
  - The dummy device can loop its outputs back to its inputs
    (`DummyBackend::with_loopback`, `FADERFRAME_DUMMY_LOOPBACK=<frames>`):
    a round trip of a buffer plus the delay.

  Tests: `a_return_counts_its_latency_from_its_send` (graph),
  `the_ping_measures_the_round_trip` and `the_return_lines_up_with_the_dry_signal`
  (engine, a cabled four-channel device), `the_hardware_insert_and_its_ping_do_not_allocate`,
  `the_round_trip_is_pinged_and_compensated` (session, the loopback dummy),
  `routing_reads_the_parameters`.

  </details>

- **MIDI learn for solo, record arm, macros and the Surround Panner**

  <details><summary>Details</summary>

  - Mapping targets `TrackSolo`, `TrackArm` (buttons: a key or a CC
    switch's rising edge toggles; letting go does nothing) and `Macro`
    (a macro modulator's value, absolute or relative; moves are one undo
    step inside the controller's gesture); `MappingTarget::track`.
  - Learning them: the mixer's Solo and Record buttons (right-click), the
    arranger's track menu (Solo, Record Arm beside Volume, Pan, Mute), a
    macro card's display or knob (right-click), and the Surround Panner
    window — a knob's parameter, or every parameter of the room from a
    right-click on it (the session already drove surround parameters).

  Test: `solo_arm_macros_and_the_surround_panner_are_mappable`.

  </details>

- **Tests that hold under cargo test and on busy runners**

  <details><summary>Details</summary>

  - The video crate's clip tests shared one folder per process and each
    removed it when done: under `cargo test` (tests side by side in one
    process) one test deleted another's files ("no such file", a cut
    test's failed state change on Windows). Each now has its own folder.
  - The shuttle test asked for more than ten ticks with a snippet heard;
    a slow macOS runner got through fewer ticks in its second. Now most
    ticks must have one, however many there are.
  - Render-ahead's lateness and the launcher's start: not checked against
    the clock when the runner's dummy device lost pace (as the capture
    test does); the launcher waits up to 3 s for playback to start.

  </details>

- **Architecture: video phases 2 and 3 done**

  <details><summary>Details</summary>

  The crate map gains faderframe-conform and faderframe-view-adr; what
  the picture work added and what is left (shared D3D12 frames on Windows,
  tries on real Windows/macOS machines and with a DeckLink card).

  </details>

- **Picture outputs: DeckLink SDI/HDMI, full screen on a chosen monitor**

  <details><summary>Details</summary>

  - `faderframe_video::output`: frames pushed at an output's own rate to
    a Blackmagic DeckLink card (`decklinkvideosink`, which loads the card's
    driver library; `mode_format` knows the modes' sizes and rates,
    `decklink_modes`/`decklink_devices` list them) or to a test sink; the
    sink paces the pushing.
  - `session::picture_out`: `set_picture_output` runs a thread that picks
    each frame for when the output will show it — the engine's position
    then (`EngineClock`, the engine's position from any thread) less the
    sound's latency — from the frame service (`FrameHandle`); the session
    publishes what plays where on every tick; a failing output stops and
    says why.
  - Preferences → Video: Full screen on (a monitor of its own: Video Full
    Screen goes there) and Picture output (DeckLink device and mode, kept
    and opened at start-up when the card is there).
  - The packages carry GStreamer's DeckLink plugin.

  Test: `the_picture_goes_to_an_output_of_its_own` (paced at the output's
  25 fps, the picture following the sound), `modes_say_their_format`,
  `devices_and_modes_ask_safely` (GStreamer started on demand — the
  preferences page aborted on it).

  </details>

- **ADR: cue list, beeps, streamers and punches, cue runs, rated takes**

  <details><summary>Details</summary>

  - `faderframe_project::adr`: the cue list (`AdrCue`: number, character,
    line, in and out, the track its takes go on, note, done) and how the
    talent is cued (`AdrSettings`: beeps, their pitch, streamer length,
    pre- and post-roll, cues on the picture); `Project::adr`,
    `Command::SetAdr` (numbered and sorted when set); `AdrCueId`. Takes
    have a rating (0–5 stars).
  - `session::adr` (`Action::Adr(AdrOp)`): cues from the transcript (the
    Lyrics lane's lines Whisper made) or the selection; the beeps' track
    ("ADR Beeps": three 1 kHz beeps a second apart before every cue, made
    again in one step — route it to the talent's headphones); a cue
    rehearsed or recorded (pre-roll, the cue as the punch range, its track
    armed, stopping after the post-roll by itself); `adr_overlay` — the
    streamer crossing the picture, the punch where the line starts, the
    line, the beeps to come, REC; `adr_takes` and RateTake. Conform moves
    the cues with their picture.
  - The ADR cue list view (`faderframe-view-adr`, View → ADR Cue List):
    number, character, timecode in and out, line, takes with the best
    one's stars, rehearse / record / done buttons; double-click edits,
    right-click for the takes' track, rating takes and removing.
  - The video window draws the cue over the picture.
  - Dev action `adr:<from-transcript|add=…|beeps|run=n|record=n|track=…>`.

  Tests: `session/tests/adr.rs` (cues and beeps, the overlay's timing and a
  rehearsal that stops by itself, rated takes), the conform test's cue.

  </details>

- **Conform to a new picture cut: EDLs, OpenTimelineIO, or by the picture**

  <details><summary>Details</summary>

  - New crate `faderframe-conform` (pure): CMX3600 EDLs (FCM drop frame,
    channels V/A/AA/B/An, dissolves, M2 speed, clip-name and source-file
    comments) and OpenTimelineIO timelines (clips, gaps, transitions,
    Clip.2 media references) read into a `CutList`; `changes` finds where
    each span of the old cut is in the new one by source and source time
    (material used twice follows the cut's order), what is new and what
    went; `shots::match_pictures` finds the same between two pictures
    frame by frame (16×9 luma signatures; a run goes on while frames match,
    a break is looked for right after the last run, then anywhere — judged
    on a second of frames so stills and black do not land just anywhere —
    through a mean-luma index).
  - `faderframe_project::conform::conformed`: the timeline in pieces
    placed anew in absolute time (tempo and meter stay), clips cut at the
    pieces' edges with a 5 ms fade where a cut lands inside, automation
    with its edge values, markers, sections, lyrics, loop and punch with
    their piece; whole clips keep their ids.
  - `session::conform`: `Action::Conform(ConformOp::Lists { old, new })`
    (record timecode against the project's; without one set, the hour the
    old cut starts in) and `VideoOp::ConformPicture { old, new }` (a job:
    both clips' frames compared, from proxies when made). What precedes
    the cut stays, what follows moves with its end, new shots get markers;
    one undo step ("Conform").
  - File → Conform to a New Cut (EDL, OpenTimelineIO)…; the video lane's
    clip menu → Conform Sound to This Picture → from <other clip>. Dev
    actions `conform-lists:<old>|<new>`, `video:conform=<old>@<new>`.
  - `faderframe_video::cuts::signatures`.

  Tests: the crate's EDL/OTIO/changes/shots tests, the project's
  `pieces_cut_move_and_drop`, `a_whole_clip_keeps_its_id`, and
  `session/tests/conform.rs` (two EDLs; two movies matched shot by shot).

  </details>

- **Video: gstreamer-video's v1_18 on every platform**

  <details><summary>Details</summary>

  PQ and HLG transfer functions are 1.18 API; only Linux had it (through
  v1_24), so the Windows and macOS builds of the colour module failed.

  </details>

- **Video colour management: HDR and wide gamut mapped for SDR screens**

  <details><summary>Details</summary>

  - `faderframe_video::colour`: what a picture's colours mean (`Colour`:
    PQ / HLG / SDR, BT.709 / BT.2020 / P3, the mastering peak), read from
    the stream's caps while indexing (colorimetry, mastering display,
    content light level) and kept with the frame index (re-indexed: `v2`).
  - Such pictures decode at 16 bits in their own transfer and primaries
    and are mapped here (`ToneMap`, tables throughout): to light (PQ in
    nits, HLG through the 1000-nit reference OOTF, SDR gamma 2.4), to
    BT.709 primaries, BT.2390's EETF on max(R, G, B) from the mastering
    peak (else 1000 nits) down to reference white (203 nits) as SDR
    white — below ~100 nits untouched, hues kept — and the SDR display's
    gamma.
  - Proxies of such pictures are made from the mapped frames; they never
    go zero-copy (VA-API's post-processor would clip them). The video
    window names the colours of a mapped picture ("HDR (PQ, BT.2020) →
    SDR").
  - faderframe-video is opt-level 3 in dev builds.

  Tests: `hdr_pictures_are_mapped_for_sdr_screens` (raw PQ in Matroska,
  its proxy too), `hevc_hdr10_is_recognised` (VA-API machines), the EETF's
  and PQ's unit tests.

  </details>

- **MXF, DV, ProRes and DNxHD: demuxers bundled, LGPL decoders for Linux**

  <details><summary>Details</summary>

  - The packages carry GStreamer's MXF (demux and mux) and DV plugins.
  - `packaging/linux/lgpl_libav.sh` builds a decoders-only FFmpeg
    (ProRes, DNxHD/DNxHR, MPEG-2, DV, FFV1, HuffYUV, Cineform, JPEG 2000,
    …) that configure confirms is LGPL-2.1-or-later, and GStreamer's libav
    plugin against it; tarball.sh takes it with `FADERFRAME_LIBAV`, and the
    release workflow builds it (FFmpeg 6.1 for Ubuntu 24.04's GStreamer
    1.24). A distribution's FFmpeg (GPL parts) is never bundled.
  - Exact frames where a demuxer's seek lands past the frame (some MXF
    index tables): decoded from further back up to it.
  - Tests: `an_mxf_clip_is_read` (uncompressed picture with sound),
    `prores_and_dnxhr_decode` (where FFmpeg's encoders are). GStreamer's
    own muxer writes long-GOP H.264 MXF its demuxer cannot seek in (it
    asserts), so that is not tested.

  </details>

- **Capture test: check the device kept pace while the key was held**

  <details><summary>Details</summary>

  A busy macOS runner's dummy device kept the overall pace but fell behind
  in a burst while the key was held, so the captured note came out 40 ms
  long. The timing assertions are skipped then too, as they already are
  when the whole run lost pace.

  </details>

- **Zero-copy video display on Linux; proxies keep their colours**

  <details><summary>Details</summary>

  - `faderframe_video::zero_copy`: playback decodes through VA-API's
    post-processor into an RGB dmabuf at the shown size (the colour
    conversion knows the picture's matrix), in a format the display takes
    (`GdkDisplay::dmabuf_formats`, handed over at start-up); the frame
    keeps its GStreamer buffer (`GpuFrame`: fourcc, modifier, planes). The
    sink announces video-meta support (vapostproc refuses dmabufs
    without). Stills, scrubbing, thumbnails and cut detection stay RGBA.
  - `Painter::external` + `External` (a picture in video memory); the GTK
    painter imports it as a `GdkDmabufTexture` (the release callback keeps
    the frame); an import the display refuses switches to RGBA for good.
  - The frame cache keeps at most 48 dmabuf frames (video memory); a file
    that does not decode into dmabufs plays through memory.
  - Preferences → Video → Display (on by default), `FADERFRAME_VIDEO_DMABUF=0`.
  - Proxies were washed out (video-range YUV written into JPEG as full
    range: GStreamer converts the range only through RGB): converted
    through RGBx to JPEG's full-range BT.601 now, and named `-2` so old
    proxies are made again.
  - The proxy note in Preferences had a run of spaces in it.
  - gstreamer-video's `v1_24` (DMA_DRM caps) and gstreamer-allocators on
    Linux.

  Tests: `frames_decode_into_dmabufs` (VA-API machines), `proxies_keep_the_colours`.

  </details>

- **J/K/L shuttle with reverse, frame steps**

  <details><summary>Details</summary>

  - `session::shuttle`: L plays, again 2×/4×/8×; J runs in reverse, again
    faster; K stops where it got to; K held with J/L steps one picture
    frame (the shown movie's own frames — variable rates step exactly —
    else the project timecode's). Any speed but forward 1× moves a
    position of its own, heard as 60 ms scrub snippets from where it is
    (never plain playback), the playhead and the picture following it
    (the picture seeks — from the proxy where there is one).
  - Keys: with picture in the project J/K/L shuttle in place of L's loop
    and K's metronome; Transport → Shuttle has them all (app actions
    `shuttle-reverse|stop|forward`, `previous-frame`, `next-frame`; dev
    action `shuttle-speed:<v>`). The transport display shows ◀◀/▶▶ and the
    speed.
  - The frame service shows the nearest frame on either side while the
    wanted one is decoded (running in reverse, the frames it has are
    later ones).

  Test: `the_shuttle_runs_back_and_frames_step`.

  </details>

- **Fix the typed timecode test's expectation**

  <details><summary>Details</summary>

  12 frames at 25 fps are 0.48 s, not half a second: the case now types
  whole seconds with the leading fields left out.

  </details>

- **Timecode counters: transport display, edit counters, spot**

  <details><summary>Details</summary>

  - `CounterUnit::Timecode` (the project's rate and start): the edit
    toolbar's counters step to it, Spot takes typed timecode ("10:12"
    fills from the right) — `parse_position` takes the project timecode;
    `format_position`, `format_timecode_length`.
  - The transport display has a big and a small counter in any unit
    (`EditorSettings::{main_counter, sub_counter}`, Action
    `SetTransportCounter`): right-click to choose, a click on the small
    one steps it; the small one is automatic by default — timecode when
    the project has video or a timecode, else minutes and seconds. Long
    readouts shrink to fit.
  - Dev actions `counter:<main|sub>=<unit|auto>`, `timecode:<rate>@<start>`.

  </details>

- **Video export: the clip's span, with a timecode track**

  <details><summary>Details</summary>

  - Export Movie writes the exported clip's span, not the whole file: the
    picture is copied from the keyframe at or before its in-point up to
    the keyframe at or after its out-point (`MuxOptions { from, to }`, a
    pad probe drops the GOPs outside, a pad offset starts the movie at
    0); the mix is rendered for exactly that span.
  - The movie carries the project's timecode where it starts
    (`MuxOptions::timecode` → a `VideoTimeCodeMeta` on the first frame;
    QuickTime/MPEG-4 write it as a `tmcd` track).
  - Timecode tracks are read back (`qt_timecode::start_timecode` walks the
    atoms: GStreamer's demuxer parses the track but hands nothing on), so
    imported camera and editor files bring their start timecode too.

  </details>

- **Video: several tracks with A/B, trimming, cut detection**

  <details><summary>Details</summary>

  - Video tracks: added, renamed (double-click the row's name), hidden and
    removed from the Video lane's menu; the lane has a row per track, the
    names in the header column; a clip dragged up or down moves onto that
    track (or Move to Track in its menu).
  - The video window compares: the top track alone, the first two shown
    tracks side by side (A | B), or A wiped over B with a divider to drag
    (Compare in its menu). The frame service plays and seeks several
    videos at once now (a target per video; stale ones dropped).
  - Clips trim by their edges in the lane (whole frames; Shift: free).
  - Cut detection (`faderframe_video::cuts`, the clip's menu → Detect
    Cuts): frames decoded in order at 64x36 (from the proxy when there is
    one), soft colour histograms compared, a cut where the change is far
    beyond the shot's own motion; the cuts inside the clip become markers
    in one undo step.
  - Dev action `video:<add-track|compare=…|to-track=…|trim=…|cuts=…>`.

  Tests: `cuts_are_found_where_shots_change`, `video_tracks_compare_and_cuts_become_markers`.

  </details>

- **Video: presentation feedback, HiDPI pictures, the late-frame meter, cache settings**

  <details><summary>Details</summary>

  - The frame lead: GTK's predicted presentation corrected by how much
    later than predicted earlier frames reached the screen (the frame
    clock's completed timings, a running average) -- presentation feedback
    where the compositor reports it. `CanvasView::frame_timing` also gives
    the device scale, so the video window decodes at device pixels (sharp
    on HiDPI screens).
  - The late-frame meter in the video window: frames shown while playing
    and how many were not the one wanted (reset when playback starts).
  - Preferences → Video: the proxies' height (360p-1080p MJPEG) or none,
    the cache folder (choose, back to the default) and its size, Clear
    Cache (`VideoSettings`, `set_video_settings`, `clear_video_cache`,
    `video_cache_size`); proxies of another height replace the ones in use.
  - Dev action `preferences-page:<page>`.

  Test `the_video_cache_is_kept_where_set_and_cleared`; the video tests
  take turns (the cache folder is the process's).

  </details>

- **Roadmap: what is open, and what could come next**

  <details><summary>Details</summary>

  docs/ARCHITECTURE.md's "Status and roadmap": video (phase 1), project
  templates and main-output tracks among what is implemented; after the
  survey's ten waves, in order: time-locked audio (scoring to picture),
  video phases 2 and 3, the known gaps (MIDI learn for solo/arm/macros,
  hardware inserts, the Guitar Station's microphone outputs, ARA, the
  Comp76Fx port, stem separation), ideas built on FaderFrame's own pieces
  (lead sheets from a recording, comping by lyric, tempo through hit
  points, self-calibrating A/V sync, head-tracked binaural, picture-
  following panning, circuit-modelled summing, auditioning the undo
  history), pro features no DAW does well (auto-align and a project health
  check, reference tracks, a masking map, spectral editing, mix scenes,
  song structure from a jam, adaptive game music, show mode) and further
  out (MIDI 2.0, screen-reader accessibility, merging versions, singing to
  MIDI). The suggested next steps close it.

  </details>

[All changes since 0.16.0](https://github.com/BurningTreeC/faderframe/compare/v0.16.0...v0.17.0)

## [0.16.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.16.0) — 2026-10-08

- **Create Output Tracks: the main output gets a track too**

  <details><summary>Details</summary>

  Every multi-output plugin (the Drum Sampler, the Guitar Station, CLAP,
  VST3, LV2 and AU instruments with extra outputs) now gets a track for
  each of its outputs in one step, the main one too ("Main" and "DI" for
  the Guitar Station), in one folder, routed where the plugin's track
  went; the plugin's own track then only carries the plugin (its output
  off, so the main is not heard twice). Making the rest later routes them
  like the Main track. Removing the Main track gives the plugin's track
  its routing back (`keep_main_heard`, hooked into `Session::edit`).

  The engine connects a track taking bus 0 right after the plugin, as it
  did the extra buses (`taken_plugin_outputs` counts the main now; neither
  side renders ahead). The menus (insert, track, the editor's Outputs)
  list and count the main too.

  Tests: the kit's main on a track of its own (a pad routed there plays
  on it, not on the kit's track), the routing given back when it is
  removed and taken again by undo; the Guitar Station's Main and DI
  tracks. README: the multi-output text and screenshot.

  </details>

- **GPU painter on Windows: shaders through DXC, never on a software adapter**

  <details><summary>Details</summary>

  The Windows CI crash (0xc0000005 in the first GPU render test, since
  before 0.12.0) was FXC: without dxcompiler.dll wgpu falls back to it,
  and vello's shaders as FXC compiles them crash Windows' software
  rasteriser (WARP). A probe on the runner (gpu-probe branch, deleted):
  inbox WARP with FXC crashed; with DXC -- dxcompiler.dll alone, no
  dxil.dll, picked up by wgpu beside the program -- all six render tests
  passed, as with WARP 1.0.21 from NuGet.

  - The Windows packages carry dxcompiler.dll (DXC 1.9.2609, LLVM and MIT
    licences, its licence texts beside it; fetched and checked by
    packaging/windows/fetch_dxc.sh), so the shaders are DXC's. dxil.dll,
    under Microsoft's own licence, is not needed and not shipped.
  - The application does not use the GPU painter on a software adapter
    (WARP, llvmpipe, lavapipe: the toolkit draws faster there);
    FADERFRAME_GPU_SOFTWARE=1 allows one. `GpuRenderer::new_any` for the
    tests, which run on CI's software adapters.
  - Ports gives the Windows tests DXC as the packages have it.

  </details>

[All changes since 0.15.0](https://github.com/BurningTreeC/faderframe/compare/v0.15.0...v0.16.0)

## [0.15.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.15.0) — 2026-10-08

- **Linux tarball: always its own GStreamer**

  <details><summary>Details</summary>

  0.14.0's tarball did not start on Arch: its launcher used the system's
  GStreamer when there was one, but the tarball's GLib comes from the
  build machine (Ubuntu 24.04), and Arch's GStreamer 1.28 needs a newer
  one ("undefined symbol: g_sort_array"). A system's GStreamer cannot be
  mixed with the bundled GTK libraries, so the launcher always puts the
  bundled GStreamer, its plugins, scanner and registry first. Checked with
  the released 0.14.0 tarball and this launcher on Arch: it starts,
  imports and plays an H.264 movie (VA through the system's libva).

  The session's video tests keep their indexes and proxies in their own
  folder (`video::set_cache_dir`), not the user's cache. README and the
  tarball's README say what the tarball does now.

  </details>

[All changes since 0.14.0](https://github.com/BurningTreeC/faderframe/compare/v0.14.0...v0.15.0)

## [0.14.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.14.0) — 2026-10-07

- **Video: JPEG through jpegdec everywhere; CI fixes for Windows and macOS**

  <details><summary>Details</summary>

  - On Apple Silicon `decodebin` took VideoToolbox's JPEG decoder for MJPEG,
    and its frames came out with the wrong colour (green and blue off,
    red right): proxies would have shown so. A JPEG stream now always goes
    to the software `jpegdec`, exact everywhere and fast enough for any
    proxy. (Ports: macOS Apple Silicon, `an_intra_clip_…`,
    `the_service_answers_…`.)
  - The clip tests' helper put the file's path into a pipeline
    description, where Windows' backslashes are escapes: the clip landed
    elsewhere ("Resource not found"). It is a property now.
  - `the_picture_follows_the_engine_while_playing` polled 30 and 47 times
    in 1.5 s on the CI Macs (50 asked); 15 are enough to see frames move,
    and the frame-rate check stays.
  - `looped_sysex_goes_out_once_per_pass`: a CI Mac's dummy device caught
    up 1.5 s of audio at once, bringing two passes 503 ms apart on the wall
    clock; a pass sent twice comes within milliseconds, so passes must be
    more than 250 ms apart (was 1 s).

  </details>

- **README: video, project templates, MIDI learn everywhere and the rest**

  <details><summary>Details</summary>

  - Video: what it does (import with sound and timecode, the Video lane,
    the video window and full screen, frame-exact scrubbing through the
    index and proxies, picture offset, the sync test, Export Movie with the
    picture copied), a screenshot of the demo with a movie, GStreamer in
    the requirements for every platform and in the packages (nothing to
    install with a package; the Linux tarball prefers the system's), video
    caches outside portable folders, the full-screen keys.
  - Project templates, and New Project / a new start from the default one
    (Ctrl+N, Ctrl+O, Ctrl+S in the shortcuts).
  - MIDI learn on every control (device editors, surround pans, bypasses,
    transport buttons, editor windows) and Remove MIDI Mapping.
  - Guitar Station: the noise gate, 61 factory presets in groups, mono
    turning stereo; Drum Sampler pads' start and end and audition; drops
    from any file manager; 277 factory presets in all.
  - The two "MIDI sync" bullets made one (the second is per-note expression
    and SysEx).

  </details>

- **Session tests: the Guitar Station's live checks wait for sound**

  <details><summary>Details</summary>

  `it_plays_live_and_a_pedal_added_while_playing_restarts_the_line` failed
  on CI at -50.7 dB against -50 after a fixed 1.5 s, as the mono test did:
  every live level check of the file now goes through `level_once_over`
  (up to 20 s more for a busy runner's debug build to bring the line up; a
  line that never sounds still fails).

  </details>

- **Video, part 6: GStreamer in the packages**

  <details><summary>Details</summary>

  - Linux tarball: the system's GStreamer is used when there is one (its
    plugins and codecs match it); otherwise the launcher puts the bundled
    one on the library path (lib/gstreamer) with the plugins video uses
    (lib/gstreamer-1.0: demuxers and muxers, JPEG, conversion and
    resampling, Opus, the H.264/H.265 parsers, VA decoding -- libva and its
    drivers stay the system's), the plugin scanner and a registry in the
    user's cache. Checked: the tarball made here imports and shows a 720p
    H.264 movie on its bundled GStreamer alone.
  - Windows: the same plugins (Direct3D 11/12 decoding instead of VA) and
    the scanner where GStreamer's relocatable build looks for them, beside
    its DLL.
  - macOS: the plugins (VideoToolbox through applemedia) in Resources,
    their libraries bundled and signed like the rest; the scanner beside
    the program; the launcher points GStreamer at them.
  - Release builds install gst-plugins-bad (parsers, hardware decoders);
    no libav/FFmpeg is bundled (the usual builds are GPL).
  - THIRD_PARTY_LICENSES lists GStreamer; the tarball's README says what
    video uses and where its caches go.

  </details>

- **Video, part 5: the video window and the Video lane**

  <details><summary>Details</summary>

  - `faderframe-view-video` (View → Video, dockable; ViewKind::Video):
    the picture for the sample heard when the frame being drawn reaches
    the screen, letterboxed on black, a timecode overlay (the project's
    timecode, the file and frame) and the jobs (reading, proxies,
    writing). Its menu: import, export, the picture offset (a frame, 5 ms
    or 1 ms either way), the flash-and-beep sync test, spot to timecode,
    remove, full screen (a double-click too). A movie dropped on it is
    imported.
  - Full screen: the view in a window of its own (detached first when it
    sits in the main window); Escape leaves, F11 toggles.
  - The arranger's Video lane (shown once there is video): each clip as a
    filmstrip, the top video track over the ones below; a click goes
    there, a drag moves the clip in one undo step snapped to whole frames
    of the project's timecode (Shift: free), a double-click shows the
    video window; tooltips name the file, codec, size, start timecode and
    proxy.
  - File → Import Video…, File → Export Movie (Picture Copied, New
    Sound)…, View → Video and Video Full Screen; dev actions
    `import-video-from:<path>`, `video-offset:<ms>`,
    `export-movie-to:<path>`, `show-video`, `video-sync-test`,
    `video-full-screen`.
  - The canvas: `Painter::pixels` (run-time RGBA, GSK memory textures
    cached by key), `CanvasView::frame_lead` (when the frame being drawn is
    expected on screen, from GTK's frame clock), `FileChoice::Save`.

  Checked in the GUI with a 720p H.264 movie: the burnt-in clock and the
  overlay agree located and playing (frame 262 at 10.48 s), the picture
  goes black after the clip's end while the transport plays on.

  </details>

- **Video, part 4: video in the session**

  <details><summary>Details</summary>

  `session::video`:
  - Import (`Action::Video(VideoOp::Import)`), a job: probe, frame index
    (kept in the cache), the first sound stream as project media on an
    audio track of its own at the picture's place; the clip goes on the
    first video track (made when there is none) at the file's timecode,
    else the start; the project takes the video's rate and start
    timecode when it has none, and says so when its rate differs. One
    undo step.
  - Proxies made in the background, one at a time, for long-GOP or
    larger-than-720p pictures; indexes and proxies live in the computer's
    cache folder (also in portable mode: proxies take gigabytes), named by
    the file's path, size and time.
  - The presenter, `Session::video_picture(lead, max)`: the frame for the
    sample heard when the frame reaches the screen -- the engine's position
    at that moment on its callback clock, less the output latency at the
    transport's speed, less the picture offset; still pictures follow the
    playhead. Frames shown and late are counted. `video_thumbnail` for
    filmstrips.
  - Edits as undoable steps: move, trim, remove, spot to timecode, the
    picture offset, show/hide a track; projects, versions and templates
    keep video paths relative in the file.
  - Export: the mix rendered for the whole of the file (silence before the
    project's start), muxed next to the picture as it is coded.
  - The flash-and-beep sync test (`faderframe_video::sync_test`), made and
    imported at the start.

  A stream nobody wants now drains through a leaky one-buffer queue: a
  paused fakesink holding the sound's first buffer stalled the demuxer
  before the first picture of a movie with sound.

  Tests `session/tests/video.rs`: import with sound and timecode, the
  flash at frame 50 exact and dark a frame later, one undo step, saved
  relative and reopened, a MOV written as long as the movie; frames
  follow the engine at 25 a second while playing, none late.

  </details>

- **Video, part 3: video tracks and the project's timecode**

  <details><summary>Details</summary>

  `faderframe_project::video`: video files with what they hold (size,
  pixel aspect, rate, codec, length, start timecode, sound streams), video
  tracks of clips placed in absolute time (project samples: tempo changes
  never move picture) with an in-point and length in the file's own time,
  the top shown track winning where tracks overlap, and the picture
  offset (ms) for what the display chain adds. `ProjectTimecode`: the
  project's rate and the label at its start, labels and positions both
  ways. Commands `SetVideo` and `SetTimecode` (no engine impact); version
  comparisons name them; templates keep video tracks without their clips
  and files.

  </details>

- **Video, part 2: the frame service**

  <details><summary>Details</summary>

  `faderframe_video::service::FrameService`: decoders on their own threads
  and a frame cache within a memory budget (thumbnails apart). Asking for
  the frame at a time answers at once with the best there is -- exact, a
  proxy-size stand-in while playing, or the nearest earlier frame decoded
  -- and tells the threads what to decode:
  - the player reads ahead in order while playing (12 frames), seeking only
    when the position jumps;
  - the seeker serves the newest still or scrub request only: on long-GOP
    originals without a proxy the keyframe first, then the exact frame;
    a still picture larger than the proxy is decoded from the original
    ("sharp when stopped");
  - the thumbnailer makes filmstrip pictures, newest first, keyframes where
    exact frames would be slow.
  A video that cannot be decoded reports why.

  Test `the_service_answers_at_once_and_catches_up`.

  </details>

- **Video, part 1: SMPTE timecode and the faderframe-video crate**

  <details><summary>Details</summary>

  `faderframe_core::timecode`: every picture rate (23.976 to 120, the
  1000/1001 rates as exact fractions), drop-frame numbering for 29.97 and
  59.94, labels parsed and shown (";" for drop-frame), a file's rate matched
  to its SMPTE rate. Ids for video sources, clips and tracks.

  `faderframe-video`, GStreamer used as a library only (it never owns
  time -- the audio engine will -- nor shows a frame):
  - probe: picture size, pixel aspect, rate and codec, sound streams;
  - index: every frame's start in the file's timeline and the keyframes,
    from the demuxer alone (fast), so variable rates, B-frames and edit
    lists are exact; the start timecode of a QuickTime timecode track;
  - decode: exact frames as RGBA at a requested size, the keyframe at once
    for scrubbing, frames in order for playback (frames are asked for by
    their start: decoders stamp a frame cut by a seek with the seek's
    time);
  - proxy: an all-intra MJPEG copy at display size, frame times kept;
  - audio: one sound stream as project media (float WAV and peaks at the
    project rate through our resampler), filled from the picture's time
    zero so it stays in step;
  - mux: the picture copied as coded with new sound tracks -- MOV (24-bit
    PCM), MKV (float PCM) or MP4 (Opus); a container that cannot carry the
    picture's codec is refused before anything is written, and a frame
    without a duration gets one (Matroska's last, else a frame short).
  Each job parses first and decodes only the stream it needs; a job that
  stops moving for 30 s fails as stalled.

  Tests on clips made in the test (each frame its own colour): probe,
  index, exact and in-order decoding, proxies, sound, MOV/MKV muxing and
  MJPEG refused by MP4; long-GOP H.264 with B-frames in every container,
  exact frame for frame (where VA can encode H.264).

  CI, Ports and Release install GStreamer (base and good plugins).

  </details>

- **Project templates: save a project's set-up, start new projects from it**

  <details><summary>Details</summary>

  File → Save as Template… keeps everything a project is set up with --
  tracks and routing, devices with their state, sends, groups, VCAs,
  folders, modulators and containers, MIDI mappings, automation, tempo,
  meter, key and chords, markers and sections, loop and punch, the album's
  delivery settings, the launcher's scenes and the window layout -- without
  its content: clips (arranger and launcher), media, freezes, aliases,
  lyrics and album songs that are files (`faderframe_project::template::
  without_content`). A template is a folder in the data folder's
  `templates` (portable mode too) with the samplers' samples copied in, so
  it stands on its own; saving under an existing name asks first and
  replaces it, written beside so a failed save keeps the old one.

  File → New from Template… lists them: a new, unsaved project from one
  (its samples copied into the new project's scratch media), the Default
  one New Project (Ctrl+N) and the "new project" start-up choice begin
  from, delete; also the set-up of any project file, the templates folder
  and the empty project.

  Dev actions `template:<name>`, `from-template:<name>`. Tests: the
  template keeps the set-up and leaves the content; listed, replaced and
  started from; a sampler's samples go with it.

  </details>

- **Session test: the Guitar Station's mono track is heard when it sounds**

  <details><summary>Details</summary>

  `a_mono_track_is_stereo_from_the_guitar_station_on` read the meter after
  a fixed 2 s and failed twice on CI with the track still silent (-72 dB);
  it never failed here, alone or with its file's tests on two cores. It
  now waits up to 20 s more for sound, so a slow runner passes and a line
  that never sounds still fails, naming the pass.

  </details>

[All changes since 0.13.0](https://github.com/BurningTreeC/faderframe/compare/v0.13.0...v0.14.0)

## [0.13.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.13.0) — 2026-10-07

- **Views repaint while a locate waits for the engine**

  <details><summary>Details</summary>

  Stopped, with quiet meters, nothing asks for frames, so a playhead that
  changed after the click's own repaint stayed drawn where it was. A
  pending locate now counts as animating until the engine has applied it
  (about 10 ms, measured with render-ahead on the dummy device).

  </details>

- **The playhead stays where a click put it while the engine catches up**

  <details><summary>Details</summary>

  Clicking the arranger moved the playhead, but on the next tick it often
  fell back to where it was, and only got there a moment later -- though
  Play started from the click. With render-ahead the engine applies a
  locate only once the new position is primed, and each tick the session
  took the engine's position, the old one. Now a position the session
  shows for a locate (`show_position`: locates, scrubs, return to start,
  recording's pre-roll, sync chases, editing's moves) stays shown until
  the engine has jumped (the transport snapshot carries its jump count
  again, for the display), is there, or two seconds have passed.

  Test `a_locate_shows_at_once_while_render_ahead_primes_it` (fails
  without the fix).

  </details>

- **Guitar Station: GainStageFx's noise gate**

  <details><summary>Details</summary>

  Upstream's noise reduction, unchanged (`faderframe_guitar::noise_gate`):
  an input expander after the Input trim, one detector on the hotter
  channel so the balance cannot wander, 2:1 below the threshold with a
  soft knee and at most 40 dB, 0.5 ms open, 25 ms hold, 80 ms close, a
  5 ms fade when switched, no latency. The DI is taken after it, as
  upstream does, so the Mix lets no noise back in; the IN meter shows the
  input before it.

  Two parameters after the places, so no id moved: Noise Gate (off) and
  Gate Threshold (-90..-30 dBFS, -60). The editor's output strip has its
  switch and threshold knob (the meters are shorter for them).

  Tests: upstream's two, `the_noise_gate_takes_hiss_and_leaves_the_guitar`.

  </details>

- **MIDI learn everywhere, and every mapping removable where it was learned**

  <details><summary>Details</summary>

  Every MIDI-learnable control now offers "MIDI Learn…" and, for each
  mapping it has, "Remove MIDI Mapping (<source>)" (`midi_learn_menu`),
  where it used to only learn or not at all:

  - device editors: the Guitar Station and the stock devices (kit) through
    `kit::learn_items`; the EQ's band knobs and output controls and the
    Program EQ's knobs and switches gain a parameter menu (default,
    automation, learn) on right-click -- a double-click still types (EQ)
    or resets (Program EQ);
  - the arranger's track header removes the volume, pan and mute mappings;
  - the mixer learns a plugin's bypass (insert menu) and, for a strip
    panned into a surround bed, each of the room's parameters;
  - the shell's own buttons: the transport (to start, stop, play, record,
    loop) and an editor window's Bypass, on right-click (`learn.rs`);
  - Preferences → MIDI: a remove button beside each learned transport
    control.

  Test `a_mapped_control_can_be_unmapped_from_its_menu`.

  </details>

- **Guitar Station: the amplifiers' scales are where the knobs turn**

  <details><summary>Details</summary>

  The 0–10 numbers spread over 300 degrees while the knob (the Program
  EQ's filmstrip) turns 250: at its stops the pointer showed about 1 and 9.
  The scales now use the knob's own angle (`program_eq::knob_angle`).

  </details>

- **GPU painter: a dmabuf is written again only once the toolkit stopped reading it**

  <details><summary>Details</summary>

  With the GPU painter on, the Guitar Station's meters showed stripes of
  two frames. A pooled dmabuf counted as free when GTK dropped its
  texture, but GTK's renderer could still be sampling it on the GPU, and
  Vulkan does not wait for such readers by itself (no implicit sync for
  exported memory): the next frame's copy overwrote a buffer on screen,
  tile by tile. Before writing a buffer again the exporter now takes its
  readers' fences from the kernel (DMA_BUF_IOCTL_EXPORT_SYNC_FILE) and
  waits for them (50 ms at most); where the kernel cannot (before Linux
  6.0) a released buffer is left alone for two more frames. The pool has
  four buffers.

  </details>

[All changes since 0.12.0](https://github.com/BurningTreeC/faderframe/compare/v0.12.0...v0.13.0)

## [0.12.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.12.0) — 2026-10-07

- **Files dropped from any file manager are taken, whatever the drag's action**

  <details><summary>Details</summary>

  Dropping a file from Nautilus did nothing, my earlier fix
  notwithstanding: Nautilus offers a drag to another program as a move
  only, and the file target took only copies, so GTK turned every drag
  away before FaderFrame saw it (`FADERFRAME_TRACE_DND=1` showed it). The
  file target now takes copies, links and moves, asking for a copy where
  the source offers one, then a link, then a move; the files are imported
  (copied into the project) either way, and the source keeps them
  (checked with Nautilus). Sources offering no file list but text naming
  files -- `file://` URIs or absolute paths, one a line, as some drag
  tools do -- reach the views through the text target, which otherwise
  takes only FaderFrame's own clip payloads.

  `FADERFRAME_TRACE_DND=1` (with `RUST_LOG=info`) logs what drags reach the
  canvases: formats, actions, whether they are FaderFrame's, the views'
  answers and the drops.

  </details>

- **Timing tests ride out busy runners: the rest of render-ahead, job ranks**

  <details><summary>Details</summary>

  CI failed `latency_reaching_a_bus_rendered_ahead_is_compensated` (two
  blocks late on the runner, nothing differing) and the Windows runner
  `measured_cost_decides_what_starts_first`. The remaining paced
  render-ahead tests go through `on_time` too (tried again when late, bit
  identity required whenever on time; the fader test checks lateness
  before timing anything). The scheduler test allows a few rank refreshes:
  a light node preempted for a Windows scheduler slice measures as heavier
  than the heavy one until its average decays.

  </details>

- **Drum Sampler: each pad's start and end, dragged on its waveform; pads play**

  <details><summary>Details</summary>

  A pad has an End beside its Start (`id::END`, 14): it plays between them
  -- reversed, from the end back to the start, as the Sampler does -- and
  the picked pad's waveform dims what does not play, with the two markers
  to drag (the nearer one moves; they keep apart). Clicking a pad plays it
  on its track, held while the button is (a gated pad stops when let go).
  Tests: `a_pad_plays_between_its_start_and_end`,
  `keep_length_switched_on_keeps_a_tuned_pads_length` (the instance asks for
  a processor with stretchers under a new activation).

  A reversed pad with a start used to skip the sample's tail; it now
  stops before the start, as the markers show.

  </details>

- **Keep Length switched on takes effect while playing**

  <details><summary>Details</summary>

  The Sampler's and the Drum Sampler's Keep Length need a processor with
  stretchers, and switching it asked for a restart -- whose new processor
  the engine then replaced with the old one: a node keeps its processor
  across a rebuild when its key is unchanged, and a built-in's key (its
  latency, bypass and activation) did not change with the mode. Built-ins
  now count such restarts as their activation, so the new processor stays.

  </details>

- **Files dropped from a file manager arrive again**

  <details><summary>Details</summary>

  Dragging a WAV from a file manager onto an audio track, or a sample onto
  a sampler's pad, did nothing. Every canvas has two drop targets: files,
  and text for the launcher's clip payloads between windows. A file
  manager offers its files as text as well, the text target took the drag,
  found no payload and answered no drop -- and the file target never saw
  it. The text target now accepts only drags started in FaderFrame.

  </details>

- **GPU painter: one wgpu instance per process, never dropped**

  <details><summary>Details</summary>

  The Windows runner's GPU tests still died (0xc0000005), and always
  right after the first renderer once they took turns. Dropping a
  process's last D3D12 renderer unloads the graphics DLLs, and on the
  software rasteriser (WARP) its threads can still be winding down then;
  side by side another renderer usually kept them loaded. Every renderer
  now shares the process's instance (as wgpu recommends; a second without
  D3D12 for the fallback), made once and never dropped.

  The GPU tests mark how far they got straight to stderr, past the
  harness's capture, so a run that dies still shows where.

  </details>

- **SysEx playback: a jump is the transport's, not a wobble of the clock**

  <details><summary>Details</summary>

  `looped_sysex_goes_out_once_per_pass` failed on macOS: one pass sent its
  message twice, 14 ms apart. The scheduler took any tick where the
  engine was more than 20 ms from where it predicted for a locate,
  cancelled what was scheduled and started again from the reported
  position -- and a device that sleeps late (the dummy on a busy runner,
  real ones at an xrun) renders one block where a callback's worth of time
  has passed, so the estimate fell back behind a message already sent,
  which then went out again.

  The transport now counts its jumps (stops, locates, scrub snippets; not
  loop wraps), and the engine's callback stamp carries the count with the
  position it put playback at (`EngineController::position_and_jumps_at`),
  so neither is seen without the other. The scheduler restarts only when
  the count changes; estimates that wobble just continue the windows where
  they ended.

  </details>

- **GPU painter: one wgpu instance chooses the adapter and keeps it**

  <details><summary>Details</summary>

  The Windows runner's GPU tests died with 0xc0000005 at random -- side by
  side, and again when they took turns (so that was not it). Since 0.12.0
  a renderer listed D3D12 adapters on an instance of its own, dropped
  right after: listing makes a device on each adapter, and on WARP the
  software rasteriser's threads were still winding down when the dropped
  instance unloaded its DLLs. In 0.11.0 one instance did everything and
  lived as long as the renderer; the D3D12 unit test, whose instance
  stays, never crashed. Now one instance chooses and requests the adapter
  (a second only when D3D12 failed, the first kept until then).

  </details>

- **Guitar Station: a reset does not depend on what its worker had played**

  <details><summary>Details</summary>

  CI failed `workers_match_inline_with_pedals_automation_and_resets`: after
  a reset the worker's amplifier settled from a different start than the
  inline one (1 run in 4 on two busy cores, 2 in 12 on one).

  A reset snaps the circuits' glides to the settings applied last. On a
  worker those depend on timing: the reset begins a new epoch, and the
  segments still queued from before it are dropped unplayed when the
  worker had not reached them, so the last settings applied were an
  earlier segment's. Now a stage's first segment after a reset takes its
  own settings and resets again before it plays (as a line does when it is
  built), so nothing from before the reset reaches what follows it. Inline
  runs the same code: worker and inline stay identical.

  `the_guitar_station_does_not_allocate_live` asked for sound in the last
  block after its reset; on an overloaded machine the live workers are
  late and that block is concealed (silence), before this change too. It
  now asks for sound anywhere after the reset; the allocation check is
  unchanged.

  </details>

- **LV2 host test: a bundle of each call's own**

  <details><summary>Details</summary>

  Both tests wrote their test bundle into one directory named by the
  process id; side by side (cargo test runs a binary's tests on threads),
  one removed it while the other scanned it and found no plugin -- the
  CI run on 0.12.0 failed so.

  </details>

- **EQ: the piano display's keys are the piano roll's**

  <details><summary>Details</summary>

  It drew every note as a flat column in the device's colours, white keys
  stopping at the black ones. Now it is the piano roll's keyboard on its
  side (one column a semitone on the logarithmic axis, low notes left,
  the keys' fronts at the bottom): white keys reaching halfway under the
  black keys beside them with the roll's shading and separators, rounded
  black keys 0.62 of the depth, the roll's piano colours, hover and names
  (the Cs, every key where there is room). Keys beyond the 88 stay dim,
  the band dots and the hovered note stay.

  </details>

- **Ports CI: paced tests ride out a busy runner, GPU tests take turns**

  <details><summary>Details</summary>

  The last Ports run failed three ways:

  - Windows: the GPU render tests' process died (0xc0000005) while two of
    them ran beside the new shared-texture test, each making and dropping
    a D3D12 device on the runner's software adapter. The application has
    one renderer, on the main thread; the tests now take turns too.
  - macOS Intel: `buses_rendered_ahead_sound_the_same` differed in 256
    samples, one block -- a late block (silence) of the anticipator, paced
    in real time beside every other test. The plain-against-ahead
    comparisons now try again when a run was late (three times at most)
    and still assert bit identity whenever none was.
  - macOS Intel: the synth with a preamp inserted while rendered ahead
    was not heard 800 ms later: the new chain primes its lookahead first.
    The test waits for it (up to 8 s) and reports the misses if not.

  </details>

- **A mono track is stereo from a Guitar Station on**

  <details><summary>Details</summary>

  The microphones' pans did nothing on a mono track (an audio track's
  default): built-ins take the track's layout, so the Guitar Station had
  one output channel and the pans folded into it. Now a device that makes
  a stereo sound of a mono source (`builtin::widens_mono`, the Guitar
  Station) turns the track stereo from its slot on: it takes the mono
  signal duplicated (one amplifier, its microphones placed in the field,
  as on a stereo bus, so it costs what it did), and the inserts after it,
  the strip's input (whose pan becomes a balance), the pre-fader tap and
  the render-ahead rings are stereo. `Track::chain_layout` says so,
  bypassed or not, so bypassing does not reroute the track; freezes,
  bounces and the track render follow it (a frozen guitar keeps both
  sides), samples (rendered without the devices) stay mono.

  The panel's IN and OUT meters light both bars for a mono line.

  Tests: `a_mono_track_is_stereo_from_the_guitar_station_on` (live, rendered
  ahead and not, rendered, frozen), `a_guitar_station_turns_a_mono_track_stereo_after_it`.

  </details>

- **Guitar Station: 49 more presets, the records' sounds under "Sounds"**

  <details><summary>Details</summary>

  GainStageFx's amplifier and album presets on the Guitar Station: each
  amplifier's own presets (36) in an "Amplifiers" submenu, and the fifteen
  records' guitar sounds, named for the record ("Metallica – Master of
  Puppets (1986)", oldest first), in "Sounds". Its Metal Zone as the
  circuit becomes a pedal behind the HM-2 (At the Gates), its wah and
  pedal two places of the line. Our ten rigs stay at the top; the two
  that GainStageFx's amplifier presets replace (Blackface Clean, Plexi
  Crunch) went.

  Every rig plays a plucked guitar at -20 dB RMS like the rigs before,
  measured (`preset_levels`, now all of them on every core, reporting the
  Output that would level a rig out of line); the tremolo and auto-wah
  rigs are levelled by their peaks.

  Factory presets can be grouped (`FactoryPreset::group`), carried as
  `PluginInstance::program_groups` through the engine and
  `Session::plugin_program_groups`; the editor window's Presets menu and
  the mixer's insert menu list each group as a submenu. The ten-to-fifteen
  rule counts the top of the menu.

  </details>

- **Solver cutoffs from the worker's start, not the reservoir's pace**

  <details><summary>Details</summary>

  The guitar stages and the preamps gave their circuit solvers a live
  cutoff of `Timing::due` plus 0.7 of the buffer. `due` is the reservoir's
  pace model, which drains 200 ppm and resyncs only once it lags real time
  by 5 ms -- so about nine seconds into a run the cutoff was already in
  the past when a segment began, every hard sample was abandoned
  unsettled, the circuit stayed off balance and the next sample was hard
  too: the level fell away and crackled until the input went quiet,
  though no frame was ever late. Traced live: a pedal stage abandoning
  all 48,000 samples a second with its buffer full.

  `devices::solve_deadline` is GainStageFx's rule: from when the worker
  starts the segment (or `due`, if later), 1.5 periods plus the buffer
  beyond one period, so a solve is only cut once its audio is certainly
  late. Test `the_solve_cutoff_does_not_follow_the_reservoirs_lagging_pace`.

  And a live trace for the Guitar Station (`FADERFRAME_TRACE_GUITAR=1`):
  once a second per stage the late frames, waits, the buffer's low water,
  the slowest segment and the solver cutoffs, plus restarts with their
  cause and how long building took.

  </details>

- **Buffered devices keep their latency through short cycle changes**

  <details><summary>Details</summary>

  A PipeWire graph whose quantum is not pinned changes cycle size whenever
  another client asks for a longer one (a browser, a notification through
  pipewire-pulse). The engine re-prepares, the session rebuilds, and the
  preamps and the Guitar Station -- whose buffers are a device callback --
  changed latency and restarted mid-song: a stretch of silence while they
  primed, then the audio back with a crack.

  Their buffers now follow a new device block only once it has held for
  three seconds (`BuiltinInstance::sized_block`); until then the processors
  stay as they are (larger callbacks are split into their 128-frame
  chunks). The PipeWire node, asked for no buffer size, now asks the graph
  for the server's own default cycle (`clock.quantum` from the settings
  metadata), so pulse clients cannot drag it up in the first place. A
  Guitar Station built anew starts with its circuits settled instead of
  spending its first segment on their operating points.

  Also: the editor's last cable runs along the board's floor, its pedals
  have screws and show a knob's value under the pointer; ignored
  diagnostics for the presets live and paced, and in the playing demo.

  </details>

- **Guitar Station: its editor, factory rigs and the DI at unity**

  <details><summary>Details</summary>

  The editor draws the rig as hardware, one section beneath the other: a
  pedalboard with the line's pedals as boxes on their footswitches (knobs as
  their boxes name them, a wah's treadle with its follower's live position,
  patch cables, a place to add one), dragged into any order and removed from
  their menus; the amplifier's head in its family's finish with its own
  knobs, switches, graphic, power stage and mains; the cabinet drawn to
  scale with the speakers behind the cloth, the two microphones placed on a
  close-up of the cone and from the side (distance, angle); the output
  strip with meters, the DI source, mix, output and the line's latency. It
  uses the Program EQ's renders and the kit's control behaviour, and every
  pedal edit is one undo step.

  Twelve factory rigs, each trimmed to play at about the guitar's own level
  (`preset_levels`). The DI leaves at unity (it is for reamping); Output
  sets the main bus only. Session tests: it plays live, a pedal added while
  playing restarts the line with its stage's latency, a DI track takes the
  second output, and renders are identical every time.

  </details>

- **Guitar Station: GainStageFx's pedals, amplifiers and cabinets as a device**

  <details><summary>Details</summary>

  A new crate, faderframe-guitar, takes GainStageFx's guitar half on
  FaderFrame's circuit solver: its pedals and two wahs, twenty modelled
  guitar and bass amplifiers with their power stages, the physical
  loudspeaker, cabinet and microphone path and the measured calibration.
  The amplifier stage (`chain`) is upstream's `Chain` without its pedal
  slot, microphone preamps, iron, pipelining and speculation; each pedal is
  a stage of its own (`pedal`) behind a true-bypass footswitch. Against
  upstream's untouched `voice.rs` every amplifier is bit-identical (legacy
  and physical, stereo microphones) and every pedal within 1e-8.

  The built-in `faderframe.guitar-station` runs a line of up to eight
  pedals and the amplifier: every stage on a reservoir worker of its own,
  one device callback behind the stage before, and each added pedal reports
  that buffer as latency for the graph to compensate. A stereo signal is
  two threads per stage, a duplicated mono signal one circuit that wakes
  the other from its history, as the preamps do. A place's pedal is built
  by a workshop thread and swapped in without allocating; offline the stage
  waits for it. A second output carries the DI (from the input, after the
  pedals, or the preamp's direct out), with Mix blending it into the main.

  Tests: worker and inline bit-identical with automation, footswitches and
  resets; latency per pedal; bypass; DI timing; the dual-mono wake; every
  amplifier and pedal; no allocation on the audio thread or any stage
  worker (counted on every thread but the workshop's, pedals changed while
  armed) and in the engine, inline and live.

  </details>

- **A new skin: Fjord**

  <details><summary>Details</summary>

  Dark slate with a hint of teal (between Adwaita's neutral greys and
  Nord's cool blues), petrol console panels and an olive-grey master
  section, a soft coral accent and old-gold displays, muted
  gruvbox-material meter colours, and a calm contrast (about 12:1 for
  text). README: the skin collage shows all eight, re-shot.

  </details>

- **Multi-output plugins: tracks for a plugin's extra outputs**

  <details><summary>Details</summary>

  A track can take a plugin's extra output bus as its input
  (InputRouting::Plugin). The graph gives the plugin's node outputs up to
  the highest bus taken and connects them after pass 1; both tracks stay
  on the audio thread, and the routing edge makes solo and cycle checks
  see the route.

  Create Output Tracks (insert menu, track menu, the editor's Outputs
  menu) makes an Aux track per extra output in one undo step: named after
  the plugin's buses (prefixed with its track when a name is only a
  number), laid out by the bus's channels, routed like the plugin's
  track, in a folder under it. The input choices list other tracks' extra
  outputs, the mixer's input well shows the bus, and freezing a track
  whose plugin feeds output tracks is refused.

  Formats: CLAP and LV2 copy every port into its graph output; VST3
  switches on the taken output buses; Audio Units render each taken
  output element with the main time stamp (tested on the macOS runners
  with AUSplitter). The sandbox packs channel slots in its shared memory
  (up to 32 output buffers, format version 4) and forwards the bus count
  and names. The Drum Sampler gets eight stereo extra outputs and a
  per-pad Output.

  </details>

- **LV2 hosting on Linux**

  <details><summary>Details</summary>

  New crate faderframe-plugin-lv2: the LV2 C ABI written by hand,
  metadata read from Turtle (oxttl) without loading plugin code, with a
  stat-checked scan cache (lv2-scan.json) and preset bundles merged into
  their plugins. Instances get URID map/unmap, options, bounded block
  length, the worker (rings + a thread per instance), state
  (loadDefaultState, mapPath/freePath, our FFL2 format with URIs), the
  log (a C shim formats printf), time:Position and MIDI/SysEx as atom
  sequences. run() is split at automation events (control ports are
  block-rate), the latency port is measured after activation, and a rate
  change instantiates again with the state carried over. X11 UIs embed
  with ui:parent, ui:resize/touch/portMap and the idle interface; they
  talk to the plugin only through ports.

  Wired into the plugin host and project formats, the sandbox wire,
  the app's registry (sandboxed), the plugin browser (LV2 badge and
  filter), the performance view, the bench (--plugin lv2:<uri>) and the
  installed-plugin tests. Tests: a plugin written in the test (CI,
  including no heap use on the audio thread) and the LV2 book's and
  DPF's examples (opt-in, FADERFRAME_TEST_LV2_PATH). The instrument
  test's insert count no longer assumes an empty Lead Synth. ARA leaves
  the roadmap.

  </details>

- **Presets: ask before replacing a name, delete your own**

  <details><summary>Details</summary>

  Saving a plugin preset under a name that exists (in any case) asks
  before replacing it; the prompt stays open to take another name.
  Track presets ask for a name (one track; prefilled with its name) with
  the same check, and several tracks at once offer Cancel / Keep Both /
  Replace. Delete Preset (plugin window, insert menu) and Delete Track
  Preset (track menu) remove the user's own presets after a
  confirmation; factory presets are never listed, and the session refuses
  any path outside the user's preset folders (compared canonically).

  Session: existing_plugin_preset / existing_track_preset,
  SaveTrackPreset { name, replace }, PromptSaveTrackPreset,
  PromptDeletePreset / DeletePreset; audio-files clean_stem.

  </details>

- **Hand GPU frames to GTK on Windows without a readback**

  <details><summary>Details</summary>

  The GPU painter's frames go over as shared D3D12 textures on Windows:
  pooled committed textures on a shared heap with simultaneous access and
  an NT handle each, rendered on the D3D12 adapter GTK's GL device is on
  (by LUID). With GTK's GL renderer, the Windows default, they are
  imported into a WGL context of GTK's display through
  EXT_memory_object_win32 and handed over as GdkGLTextures (GTK's own D3D12
  import into GL imports the resource handle as its fence and falls back
  to a CPU copy); with its Vulkan renderer they are GdkD3D12Textures. The
  first frame is compared with a readback; any mismatch or error and frames
  are read back again. When vello cannot be built on D3D12 the painter
  takes another backend, the shader errors caught in error scopes instead
  of panicking.

  </details>

- **Deliver IAMF masters (Opus, FLAC or LPCM, as MP4 or .iamf)**

  <details><summary>Details</summary>

  Render -> "IAMF" writes the master (mono, stereo or a bed) as an IA
  Sequence of AOM's Immersive Audio Model and Formats, in an MP4 track
  (sample entry iamf/iacb, edit list, roll group) or standalone. The new
  faderframe-iamf crate stays inside IAMF v1.0.0-errata's Simple profile:
  one channel-based Audio Element of one layer (formats without an IAMF
  layout ride in the smallest that holds them), substreams in the
  specification's order coded as LPCM, FLAC (flacenc) or Opus (libopus,
  bundled and built with CMake), and one Mix Presentation with the loudness
  measured on Stereo, as decoders render it, and on the layout itself.
  Multichannel BS.1770 loudness (integrated_weighted) comes with it.

  Checked against AOM's reference decoder libiamf (every codec, both
  containers, to 7.1.4 and stereo) and FFmpeg (stream groups, exact
  LPCM/FLAC, the stereo loudness written equals ebur128 of libiamf's
  render). The AOM patent licence ships at the root and in the packages.

  </details>

- **Listen through other heads, your own SOFA file, with headphone correction**

  <details><summary>Details</summary>

  Twenty heads are built in for listening on headphones: SADIE II's KU100
  (still the default) and KEMAR dummy heads and its eighteen listeners,
  level-matched so they compare by ear. Any SOFA file (AES69
  SimpleFreeFieldHRIR or GeneralFIR, read in pure Rust with hdf5-reader)
  can stand in for your own ears: each speaker takes the nearest
  measurement, prepared as the built-in heads are (SADIE's own KU100 file
  reproduces the baked head). A headphone correction evens out the
  headphones: EqualizerAPO/AutoEq parametric EQs (RBJ biquads), graphic
  EQs (minimum-phase FIRs) or an impulse response, applied after the room
  in the same block, so it adds no latency. The head, the SOFA file and the
  correction are remembered; Audio -> Listen and the master strip's Listen
  menu choose them. A render for headphones uses the head listened with,
  without the correction.

  The scripted menu dev action now prefers an exact label over the first
  entry containing it.

  </details>

- **Start renders where their range does**

  <details><summary>Details</summary>

  A master export rendered with the graph's output latency at its front: a
  lookahead limiter on the master delayed the file by its lookahead (198
  samples with the stock limiter) and cut as much off the end. Renders now
  render the latency more and drop it from the front, so exports, stems,
  album songs, bounces, clip effects and samples start on time; their own
  offsets for it are gone. A freeze alone keeps the latency in its file and
  the frozen player claims it, so everything after the track hears it
  exactly as before (freezing stays bit-exact).

  </details>

- **Listen on headphones (binaural) and check the master in mono**

  <details><summary>Details</summary>

  The master's way to the interface now passes a listening node: on
  headphones the master (a bed, stereo or mono) is rendered binaurally
  through the KU100 dummy head's measured responses from SADIE II
  (University of York, Apache-2.0; fifteen BS.2051 directions baked at
  44.1/48/96 kHz by scripts/binaural_hrirs.py), with a uniformly
  partitioned convolver (one block of latency), the LFE to both ears and
  Near/Mid/Far rooms. A MONO button in the master strip's record slot sums
  what is heard to both speakers without a rebuild. Both are listening
  only (Audio -> Listen, the master strip's Listen menu); renders stay as
  mixed, and "Headphones (binaural)" is a render option of its own.

  Tracks carry Dolby's binaural render mode (the master for its bed,
  object tracks for their objects), written into the dbmd chunk of ADM
  masters.

  </details>

- **ADM: Dolby's metadata chunk (dbmd) in Atmos masters**

  <details><summary>Details</summary>

  Dolby Atmos masters now carry the `dbmd` chunk Dolby's tools look for:
  the EBU Tech 3285 Supplement 6 container (version 1.0.0.6, segments with
  their checksums, the pad counted in the chunk) with the Dolby Digital
  Plus segment as Dolby's tools write it, the Atmos segment (FaderFrame
  and its version as the creating tool, warp mode not indicated, bed
  distribution and the constants Dolby's files share) and the supplemental
  segment (automatic trims for the nine speaker configurations, each
  track's binaural render mode — the LFE off). `binaural_modes` reads the
  modes back.

  Checked: Dolby's own parser (dbmd_atmos_parse, BSD) reads the chunk and
  names FaderFrame; MediaInfo now reports the file as a "Dolby Atmos
  Master" with no conformance findings (it calls a file an Atmos master
  only with segment 9).

  </details>

- **A console Channel Strip device**

  <details><summary>Details</summary>

  One insert for a console channel: input trim; high pass (16–600 Hz,
  12/18/24 dB/oct) and low pass (3–22 kHz), optionally only into the
  dynamics' key; a gate (hysteresis, hold, range) or 2:1 expander; a
  compressor on the stock compressor's gain computer (peak or RMS, knee,
  attack, release, make-up, parallel mix) keyed by the channel or the
  sidechain; a four-band EQ (LF/HF shelves or bells, two parametric mids,
  "Brown" as set or "Black" with proportional Q); the EQ before or after
  the dynamics; console drive (unity for small signals) and the output.
  Zero latency, dynamics linked across the channels.

  The editor (kit face) draws the EQ curve with the filters and the
  history of input, output and compression, with sections laid out like a
  channel and meters for input, gate, compressor and output.

  Tests: nine DSP tests (unity as it comes, the cuts, the shelf, the
  ratio, the gate, the order, the sidechain, drive), two channel strips in
  the_stock_devices_do_not_allocate.

  </details>

- **Tests: two timing-sensitive checks hold under a loaded machine**

  <details><summary>Details</summary>

  `touch_and_latch_write_automation_while_playing` took the fader's values
  from the wall clock and the points' times from the playhead; with the
  whole suite running, late callbacks bent the ramp and thinning kept more
  points than the bound (21 > 20, once in five full runs). The ramp now
  follows the playhead's progress (60 of 60 runs at load 34 on 16 threads).

  `rendering_ahead_does_not_allocate_on_the_audio_thread` failed once in
  four full runs earlier and could not be reproduced (36 parallel runs,
  eight more full runs). Its renderer thread may be starved past its
  lookahead when the suite runs beside it, which counts as a miss: the
  lookahead is 400 ms now, and every assertion message reports the
  allocations, misses and scope writes together, so a failure says which.

  </details>

- **Surround sends follow the panner into beds and fold out of them**

  <details><summary>Details</summary>

  Sends copied channels by the graph's generic rules: a mono track's
  pre-fader send into a 5.1 reverb reached every channel (LFE too) at full
  level, and a post-fader send from a track panned into a 5.1 master into
  a stereo reverb carried only L and R (a centre-panned track sent
  nothing). Sends into or out of a bed now mix through the panner's matrix
  (`MatrixMix`, factored out of the channel strip and shared): a mono or
  stereo track's pre-FX or pre-fader send into a bed follows its panner
  and the panner's automation; other sends between a bed and another
  layout place by the speakers (fold-down / open-up).

  Tests: sends_follow_the_panner_into_beds_and_fold_out_of_them; the
  surround allocation test now has a following send.

  </details>

- **GPU painter: frames go to GTK as dmabufs on Linux**

  <details><summary>Details</summary>

  The readback is gone where the GPU and the display allow it: the
  painter opens its Vulkan device through wgpu's hal with dma-buf export
  (VK_KHR_external_memory_fd, VK_EXT_external_memory_dma_buf), copies
  each frame on the GPU into a pooled buffer whose memory is a dmabuf
  (linear rows padded to 256 bytes, DRM AB24, straight alpha) and GTK
  takes it as a GdkDmabufTexture. A buffer is reused only after GTK's
  release callback gives it back (three at most; with all of them shown a
  frame is read back). Used when the display imports linear AB24; if GTK
  refuses a frame, frames are read back from then on;
  FADERFRAME_GPU_DMABUF=0 turns it off.

  On a Radeon iGPU (RADV) the mastering tools at 2411×711 device pixels:
  4.98 → 3.80 ms per paint, 52 → 60 fps. Windows and macOS keep the
  readback.

  `unsafe` is allowed in `faderframe-ui-gpu`'s dmabuf module only (the
  crate denies it otherwise). Test: frames_go_out_as_dmabufs (the dmabuf's
  rows equal the readback, the descriptor is a dma-buf, buffers are reused
  after release).

  </details>

- **Object-based masters: object tracks, ADM BWF out and in**

  <details><summary>Details</summary>

  Objects. A mono or stereo track panned into the master's bed can be an
  object (`Track::object`, `SetTrackObject`): it skips the master's inserts
  and fader and joins the master's output in a renderer node after the
  master strip, as a renderer adds objects to the bed, so what is heard is
  what is delivered; the renderer meters the master. Objects have no LFE
  send. The track menus and the Surround Panner toggle it; objects' pucks
  are square.

  Delivery (Render → ADM BWF): the master's format as far as one of the
  profile's beds (2.0 … 7.1.2) holds it, its other channels (a 7.1.4's top
  front and rear) as objects fixed at their speakers, every object track
  as one object (a stereo one as two). One render plays the bed and every
  object track on outputs of their own, the plugins' delay taken off the
  front, streamed into a BW64 file (`Bw64Writer`: chna before the audio,
  axml after it, RF64 with ds64 past 4 GiB) as 24-bit PCM at 48 or 96 kHz.
  Object metadata comes from the panner and its automation in blocks that
  start wherever the place moves.

  `faderframe-adm` writes the ADM document in two profiles: Dolby's Atmos
  master ADM profile (as MediaArea's checker states it; MediaInfo reports
  no findings) and ITU-R BS.2076 as the EBU ADM renderer reads it (it
  renders the objects where the panner put them). It reads both back, and
  other ADM files' BS.2051 beds and polar objects.

  Import (File → Import ADM BWF Master…): the file is split into a bed
  file and one file per object, brought in through the importer, and
  becomes a bed track and object tracks with their movement as automation
  in one undo step; the master takes a format that holds them.

  Also: `WavFile` and chunk reading for RF64/BW64; meters follow a 5.1
  master back to stereo (test); dev actions `object:`, `import-adm-from:`,
  `render-adm:`.

  </details>

- **Meters follow a track's format back to fewer channels**

  <details><summary>Details</summary>

  A track's meter range keeps its size when the track goes back to fewer
  channels (a master from 5.1 to stereo), and the meters reported every
  channel of the range: strips kept drawing six meters. The registry now
  records how many channels each strip meters at every graph build and
  `take_meter` reports those.

  </details>

- **Context menus: submenus are stack pages, so they open by a screen edge**

  <details><summary>Details</summary>

  A submenu replaced the rows of the open popover from inside the click
  handler: the popover changed size and had to move after it was shown,
  and by the right edge (the mixer's master strip, its Channel Format
  submenu) it closed instead. Every level is now a page of one stack, built
  up front: the stack is as large as the largest level, so the popover is
  placed once with room for all of them and moving between levels neither
  resizes it nor removes the clicked button.

  Dev aids: `right-click:<view id>@<x>/<y>` (negative from the right and
  bottom edges), `menu:<label>` clicks an entry of the open context menu,
  `menu-state:x` logs it, and `screenshot` also saves an open menu as
  `<stem>-menu.png`.

  </details>

[All changes since 0.11.0](https://github.com/BurningTreeC/faderframe/compare/v0.11.0...v0.12.0)

## [0.11.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.11.0) — 2026-10-07

- **Surround panner: automation, format menus, the mixer room and the panner view**

  <details><summary>Details</summary>

  Wave 10 is done: surround beds and panning.

  Automation: `SurroundParam` (x, y, z, spread, width, LFE) and
  `AutomationTarget::Surround`; the snapshot's `TrackAutomation::surround`
  lanes drive the strip's matrix every automation step (echoed when the
  strip renders ahead). A track panned into a bed lists the values that
  apply (height with top speakers, width for stereo, LFE with an LFE) in
  place of Pan; the session writes them from `SetTrackSurround` in
  Touch/Latch/Write and shows them as they play (`shown_surround`); MIDI
  learn sets one value.

  `Project::destination_layout` and `surround_panned`; `fold_into` shared
  by the engine's fold-down and the mixer's output label ("Out 1-2 folded").

  Channel Format submenus (mixer and arranger track menus) offer mono,
  stereo and every bed for audio tracks, buses, auxes and the master.

  Mixer: a strip feeding a bed shows the room from above in place of its
  pan knob — drag the puck (Shift/Ctrl fine), wheel for left/right,
  double-click or a click on the readout opens the panner; a bed's own
  strip shows its format; meters widen for every channel.

  `faderframe-view-surround` (View → Surround Panner): the selected
  track's room, its speakers lit by what they play, the puck (a stereo
  track's two channels), spread and height, knobs for height, spread,
  width and the LFE send; every move one gesture. Its `room` painter is
  shared with the mixer.

  Dev actions `track-format:`, `surround:` and `show-surround-panner:`.
  Tests: surround automation in the engine and the session, the mixer's
  room, the panner view, the room geometry.

  </details>

- **Surround beds: formats, the panner, folding, meters and delivery**

  <details><summary>Details</summary>

  `faderframe_core::surround`: channel-based formats (LCR, quad, 5.0/5.1,
  7.0/7.1, 5.1.2/5.1.4, 7.1.2/7.1.4) in the order WAVE files carry them,
  each speaker with its mask bit and place in a room box; the panner
  (`SurroundPan`: x, y, z, spread, stereo width, LFE send) crossfades power-
  preservingly between the floor and top layers, the rows of a layer and
  the speakers of a row, as console and Dolby bed panners do; `matrix`
  places mono, stereo and discrete sources by it, and folds a bed into
  another format by its speakers' places (into the same one it passes
  straight). Nothing allocates. `ChannelLayout::Surround`.

  `Track::surround` and `Command::SetTrackSurround`; six more strip slots.
  Into or out of a bed the channel strip mixes through the matrix (every
  pair ramped); node keys tell layouts of equal width apart. The engine
  carries 16 channels. A bed wider than the device folds down to the
  largest format it plays (`FoldDown`; rebuilt when its outputs change),
  so a 5.1 master on a stereo interface is heard whole. Meters cover every
  channel (meter ranges sized by format; `TrackMeter`/`MeterDisplay`
  channels). Render "as the master is" writes every channel as
  WAVE_FORMAT_EXTENSIBLE with the bed's channel mask (`write_wav_mask`,
  `WavWriter::create_with_mask`; `read_wav` reads masks), or folds down to
  stereo; the default for a surround master.

  Tests: power kept and points on their speakers for every format; stereo
  width; 7.1.4 into 5.1 and 5.1 into stereo; a 5.1 master's channels,
  LFE send, metering and fold-down in the engine; no allocations with the
  panner moving; a 5.1 render with its mask and folded to stereo.

  </details>

- **A DDP player that checks, plays and imports CD masters**

  <details><summary>Details</summary>

  `session::ddp` opens any DDP fileset on a worker: `ddp::inspect` (the
  strict `read` without refusing on the Red Book rules, which it reports),
  the image's byte order found by its sound (`ddp::image_big_endian`), an
  overview of its peaks, and the image as a float file at the engine rate
  that the engine plays instead of the project, like album playback (they
  exclude each other; the project playing takes the outputs back). The
  view (`faderframe-view-ddp`, File -> DDP Player) shows the checks
  (checksums, Red Book), the disc's text per CD-Text language, an overview
  with track marks and pregaps (click to seek), the track table (start,
  length, pregap, ISRC, flags, title, performer; click to play) and a CD
  player's display: track, index and time, counting down in a pregap.

  Import writes each track's samples unchanged into a 16-bit WAV in the
  media folder (a pregap with more than dither noise stays with the track
  before, a silent one becomes the pause) and adds the songs with titles,
  credits, ISRC, the UPC and every language in one step. The first save
  now also moves album songs' files made in the scratch media folder and
  re-points the songs (also after undo/redo).

  Tests: a master FaderFrame wrote, checked, played by track, imported
  bit for bit, saved; a byte-swapped image recognised. Dev action
  `ddp:open=<folder>|play|pause|stop|next|previous|import|close`.

  </details>

- **CD-Text in up to eight languages**

  <details><summary>Details</summary>

  `AlbumInfo::language` is the main text's language (block 0, English by
  default), `AlbumInfo::translations` the further ones: the album's title
  and credits and each song's, empty fields falling back to the main text.
  The Details form picks the language shown, adds and removes languages
  and sets the main one; `AlbumAction::Texts` saves release and song in
  one step (duplicate languages dropped).

  faderframe-disc writes a CD-Text block per language: sequence numbers
  from 0 in each block, the block number in the fourth header byte, size
  packs with the block's character code and every block's last sequence
  number and language. Latin-script languages are ISO 8859-1, Japanese is
  MS-JIS (double-byte, ASCII full-width, encoding_rs's Shift-JIS).
  `decode_blocks` reads them back and `ddp::read` keeps every language.
  Block 0 is byte-identical to before (reference filesets unchanged).
  Checked against libcdio: all three blocks of an English/German/Japanese
  disc decode with their languages (libcdio applies the last block's
  character code to every block, so its German fails after Japanese; ours
  follows the spec). Dev action `album:details-in=<hex code>`.

  </details>

- **An optional GPU painter for dense views (vello on wgpu)**

  <details><summary>Details</summary>

  GTK 4.22's renderer rasterises every fill and stroke node with cairo on
  the CPU when its path changes: the mastering tools at 2524x747 ran at 26
  fps, cairo_stroke 23 % of the profile, although building their nodes took
  under a millisecond.

  `faderframe-ui-gpu` (GTK-free) implements `Painter` on a vello scene:
  paths, gradients, clips as clip layers, outset shadows as blurred rounded
  rectangles clipped even-odd outside the shape, inset shadows as the
  colour less a blurred copy (DestOut), images cropped to the frame drawn
  (vello's atlas does not take a tall filmstrip), text with parley in the
  toolkit painter's font families (cached, ellipsised, hinted). A frame
  renders into a texture and is read back as straight-alpha RGBA (buffers
  recycled). `CanvasView::dense` marks the views worth it (the tools, the
  EQ, the Program EQ, the devices' faces); with Preferences -> General ->
  Drawing or `FADERFRAME_GPU_PAINTER=1` the canvas shows their frames as a
  GDK memory texture, and keeps GSK without an adapter or after an error.
  Feature `gpu-painter` (default on in faderframe-ui and the app).

  Measured: tools 26 -> 55 fps, tools and EQ editor together 17-19 -> 39
  fps; screenshots match GSK. The mixer stays with GSK (its rectangles are
  cheap there, a 22 MB readback per frame is not). `FADERFRAME_PAINT_STATS`
  logs paint time, frame rate and primitives per canvas. Tests render real
  frames (skipped without a GPU adapter).

  </details>

- **Graph scheduler: job affinity for cache locality**

  <details><summary>Details</summary>

  Each thread of a parallel cycle has a seat (the caller 0, pool worker i
  seat i + 1: `faderframe_realtime::worker_seat`, a thread-local), folded
  onto as many queues as the cycle has seats. A ready job goes to the queue
  of the seat that ran it last, so its processors' state and buffers tend
  to stay in that core's caches; a thread works its own queue first and
  steals when it has none. Entries carry the cycle (no clearing per cycle;
  cleared once on wrap). Output stays bit-identical (parallel tests).

  Unpaced bench, 8 threads, perf stat: default devices 128 tracks at 64
  frames 6-9 % fewer cycles (mean 59.9 -> 54.7-56.3 us), 32-frame buffers
  2-4 %, echo-heavy 3 %. Tried and dropped: rank-aware stealing and a
  shared queued-jobs counter (both cost more than they saved).

  </details>

- **Render buses ahead in a shallow second tier: faders answer within ~10 ms**

  <details><summary>Details</summary>

  Strips rendered ahead in the deep anticipator answered fader moves after
  its lookahead (200 ms). Buses rendered ahead and the strips and sends
  reaching them now run in a second, shallow anticipator: it reads the
  deep tier's rings of those tracks' chains and renders in 128-frame
  blocks a device callback and two blocks ahead (`shallow_lookahead`), at
  the audio thread's scheduling (published once by the link). It renders
  a block once the deep tier has (each anticipator publishes its
  progress); the link hands sequences to both tiers (shallow first) and
  switches when both are ready. The shallow tier's lookahead follows the
  device's callback size.

  Measured (`faders_reaching_buses_rendered_ahead_answer_within_milliseconds`):
  a closed fader is heard after 13.3 ms with 256-frame callbacks (5.3 ms
  live), instead of the lookahead. Output stays bit-identical to live
  rendering; bench (64 tracks, 4 buses, 64-frame buffers): callbacks mean
  36 -> 16 us, p99 53 -> 24 us. "Buses too" is now on by default.

  </details>

- **Preamps: channels in parallel, one device callback of buffer, a live deadline**

  <details><summary>Details</summary>

  Stereo preamps on the demo's drums dropped out live, and the British 73
  at 50 % was enough in a dev build. Three causes, measured:

  * A track's channels were solved one after the other on one worker:
    stereo British 73 took 1030 us per 128 frames (release; 2000-2500 in
    dev). `PreampBank` solves them side by side (the calling thread and a
    `WorkerPool` of helpers, which adopt its scheduling, take a channel
    each; duplicated mono and its exact wake-up are kept): 552 us. Used by
    the reservoir worker and the inline (render-ahead, offline) path.
  * The reservoir buffered 128 frames, so with 256-frame callbacks half of
    each callback's circuit work ran while the audio thread waited. It now
    buffers one device callback (at least 128, `buffer_delay`): the engine
    passes the callback size as `PrepareConfig::device_block` (0 offline,
    so renders do not depend on their block) and rebuilds when it changes;
    the reported latency follows, the same live and rendered ahead.
  * Driven into hard clipping (German 76 at full gain on drums), samples
    that do not settle take 64 passes plus the half-step rescue: blocks of
    up to 3.5x real time. Live, the solver now keeps to the audio's due
    time (`Preamp::set_realtime_deadline`, GainStageFx's mechanism); renders
    keep the full solve.

  The circuit crate is optimised in dev builds too. Session check (6 s on
  the dummy device, ignored test `a_full_gain_preamp_plays_without_dropouts`):
  British 73 and German 76 at full gain, rendered ahead and armed, dev and
  release: 0 xruns (were 51 and 405-534). Measurement tests:
  `preamp_cost_by_gain`, `drum_loop_cost_by_model`.

  </details>

- **Render buses ahead (opt-in), sum audio in key order**

  <details><summary>Details</summary>

  With "Buses too" (Preferences → Audio, off by default), buses, auxes and
  the master's devices render ahead when everything reaching them can:
  strips that feed only buses rendered ahead, key no sidechain or
  follower, without crosstalk. `build::ahead_sets` finds them (a fixpoint;
  while playing nothing joins, and a strip that has to come back brings
  its chain and its buses). Their strips and sends go into the ahead
  graph (quiet), and their post-fader audio comes back through a ring to a
  `StripEcho` on the audio thread for meters, scope and automated values.
  Buses are built after what reaches them, so their latency is known.

  The graph compiler now sums a port's audio inputs in the sources' key
  order (then builder order), so a different build order keeps addition
  order and output stays bit-identical to live (tests: buses rendered
  ahead, latency reaching a bus, no allocations, a live input keeps its
  buses live).

  Still to come: fader moves on strips rendered ahead are heard after the
  lookahead; a shallow second stage will take the strips.

  </details>

- **Launcher test: wait for back-to-arrangement instead of a fixed 50 ms**

  <details><summary>Details</summary>

  The macOS Intel runner had not processed the command after 50 ms.

  </details>

- **Windows: the installer stays unsigned; wave 7 closed**

  <details><summary>Details</summary>

  No code-signing certificate, so the signed installer leaves the plans;
  the README says how to get past SmartScreen (More info → Run anyway).

  </details>

- **macOS: ad-hoc signed for good; how to open it on macOS 15**

  <details><summary>Details</summary>

  No notarisation (it needs a paid Apple developer account), so it leaves
  the plans. The README's way past Gatekeeper is right-click → Open, which
  macOS 15 no longer allows: it now says to allow the app in Privacy &
  Security (Open Anyway) or to clear the quarantine flag.

  </details>

- **Roadmap: no Flathub submission (Flathub does not take LLM-generated apps)**

  <details><summary>Details</summary>

  The Flatpak stays a bundle on the releases page.

  </details>

[All changes since 0.10.0](https://github.com/BurningTreeC/faderframe/compare/v0.10.0...v0.11.0)

## [0.10.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.10.0) — 2026-10-07

- **Release 0.10.0: performance and control, finished**

  <details><summary>Details</summary>

  The clip launcher's launch modes, clip quantisation and legato, follow
  actions with a second action, chance and jumps, tempo-following audio,
  slot recording with a fixed length, count-in and overdubs, Record to
  Arrangement written as it plays with the mixer's moves, drags back to
  the arrangement and between windows, MIDI learn; grid controllers
  (Launchpad, APC mini, Push 2) and OSC launcher feedback; the rest of
  Mackie Control and HUI; MIDI time code out and varispeed; modulators on
  VST3 and AU parameters; full SFZ import; the macOS audio workgroup for
  sandboxed plugins. The README's launcher screenshot shows launch modes,
  follow actions and Record to Arrangement writing live.

  </details>

- **Record to Arrangement: its automation joins the recording's undo step**

  <details><summary>Details</summary>

  The automation written while recording to the arrangement was
  committed as its own step when the transport stopped, so the last
  stretch of the recording (written after it) became a second "Record
  Launches" step. While recording to the arrangement the automation now
  goes into the recording's step (`Session::record_step`).

  Tests that the slow macOS runners tripped: the record-launches test
  waits for the stop to arrive, the launcher MIDI-learn test does not
  wait for bars, and the capture test checks its timing only while the
  dummy device kept the wall clock's pace (a busy runner's falls behind
  and catches up without an xrun).

  </details>

- **Sandbox: the workgroup service name is Unix-only (Windows lint)**

- **Mackie Control and HUI: the rest of their buttons**

  <details><summary>Details</summary>

  Mackie: the channel-strip pages (track, plug-in, EQ, instrument: the
  selected track's own, a device's, its EQ's or its instrument's
  parameters on the pots and the LCD; bank buttons page, channel buttons
  or the page again move to the next device; pot press resets, Flip puts
  them on the faders), F1–F8 (workspaces; Shift: the markers), Global
  View and the track types (strip filters), Name/Value, SMPTE/Beats
  (timecode at the MIDI time code rate, with its LEDs), Shift/Option/
  Control/Alt (Shift + Undo redoes, + Rewind/Forward to the start/end,
  + Select adds; Option + Mute/Solo/Arm: every track), Read again = Off,
  Trim (Off: no trim mode), Group, Cancel, Enter, Nudge, Drop (punch),
  Replace (record mode), the global Solo (clears; lit with rude solo),
  Up/Down (the strip above/below), the arrows' zoom and nudge modes,
  Zoom, Scrub (the jog wheel scrubs, heard), the footswitches, and the
  LEDs of all of it.

  HUI: the keyboard zone (modifiers, edit mode and tool), windows (mixer,
  editor, launcher), record ready all, the cursor zone (arrows, zoom mode,
  scrub, shuttle), quick punch, pre-roll, punch in/out at the playhead,
  the monitor section's mute (the master), the numeric keypad (a bar to
  locate to, `.n.` a marker, enter alone a marker, ± a bar), input
  monitoring, create group, the edit zone (cut, copy, paste, delete,
  separate, capture), F1–F8, the parameter-edit zone (plug-in page,
  bypass), the timecode/beats/rude-solo LEDs and timecode on its display.
  It answers the pings now, so the preferences say when one is off line.

  </details>

- **Grid controllers play the launcher; OSC shows it; banks per surface**

  <details><summary>Details</summary>

  `faderframe_control::grid::Grid`: Launchpad Mini MK3 / X / Pro MK3
  (programmer mode), APC mini, APC mini mk2 and Push 2 (user mode). Pads
  are slots (eight launcher tracks by eight scenes from the surface's own
  banks), lit in the clips' colours — Launchpads by RGB, the mk2 by the
  nearest colour of the shared 128-colour palette, Push 2 by palette
  entries set to the clips' colours — green pulsing while playing,
  flashing while waiting to start or stop, red pulsing while recording,
  dim red on armed tracks' empty slots; scene buttons, stop buttons (the
  APCs' track row, Push's lower row), arrows (or Shift + track buttons)
  for the banks, stop all / back to the arrangement, the APCs' faders on
  the shown tracks' volumes; pad releases release (gate/repeat). Only
  changed lights are sent; leaving restores the controller's own mode.

  Surfaces see the launcher (`SurfaceState::launcher`: slot and scene
  states of their bank, `Protocol::{scenes, launcher_columns, goodbye}`),
  with releases, track stops and a scene bank as inputs. OSC sends it
  (`/launcher/clip/N/S` state, name and colour, scenes, playing tracks,
  the scene bank) and takes `/launcher/clip/N/S 0` releases, track stops
  and scene-bank moves. A surface can have a bank of its own (grids
  always; "Own bank" for the others) instead of continuing from the one
  before it.

  </details>

- **MIDI learn for launcher slots, scenes and stops**

  <details><summary>Details</summary>

  Mappings get launcher targets (`MappingTarget::{LauncherSlot,
  LauncherScene, LauncherStop}`): a pad or button learned for a slot
  launches it on its press and releases it when let go (gate and repeat
  clips stop: key-offs and falling CC edges count as releases now, and
  are ignored by every other kind of mapping and by learning); a scene's
  launch button, a track's stop and Stop All. The launcher's menus for
  slots, scenes, track stop buttons and Stop All have MIDI Learn and
  their mappings' removal.

  </details>

- **Launcher drags: back to the arrangement, own columns, across windows**

  <details><summary>Details</summary>

  A launcher clip dragged out of the launcher carries its payload; the
  arranger takes payloads of launcher clips (showing where they go) and
  copies them, as they play (following the tempo), to the track and
  snapped time under the pointer (`LauncherOp::ToArrangement`; clips of
  other tracks go to the tracks after it, one after another on each).
  Arrangement clips of several tracks dropped on a slot go to their own
  columns (each track's down the scenes) instead of all down one.

  A payload drag that leaves its window goes on as a native GTK drag
  (`gdk::Drag::begin` from the canvas's drag gesture; the view's own drag
  is undone), and every canvas takes `clips:` strings through a drop
  target that feeds its view's `hover_payload`/`drop_payload` — so clips
  drag between the main window and detached ones.

  </details>

- **Record to Arrangement writes as it plays, with automation**

  <details><summary>Details</summary>

  What the launcher plays goes into the arrangement while it plays: each
  loop once it has played, the rest where the clip stopped or another
  took over (from the launch the last tick saw waiting, so legato
  launches end the previous run where they took over, not at the shared
  loop start), copies in time with the clip's loop. However often it
  writes, a transport run is one "Record Launches" undo step
  (`History::apply_amending`: commands join the last step while nothing
  else was done, undone or redone since; writing waits while a gesture is
  open). Moves of the mixer and of device parameters are written as
  automation meanwhile, latched — into a lane made for them when the
  control had none; Off lanes written to read back.

  </details>

- **Launcher slot recording: fixed length, count-in, MIDI overdub**

  <details><summary>Details</summary>

  The launcher's recording settings (`Launcher::{record_bars, count_in}`,
  `Command::SetLaunchRecording`, in the quantise menu): a slot recording
  ends by itself after a fixed number of bars, and one started from stop
  counts in that many bars (the metronome clicking) instead of using the
  arrangement's pre-roll. Slot recordings ignore the punch range (it used
  to clip them).

  Recording on a slot holding a MIDI clip overdubs: the clip plays
  (launched with its own quantisation if it does not), and the notes
  played join its loop where they fell (from where it started playing),
  as one "Overdub" undo step. The slot shows a record dot and outline
  while overdubbing; its menu has Overdub / End Overdub.

  </details>

- **Launcher: launch modes, clip quantise and legato, two-way follow actions, tempo**

  <details><summary>Details</summary>

  Slots get launch settings (`ClipLaunch`, `Command::SetClipLaunch`,
  saved per slot and carried by moves, copies, scene duplicates and
  deletes): a launch mode (trigger; gate: plays while held; toggle: the
  next press stops it; repeat: starts again every launch quantum while
  held), their own launch quantisation, legato (takes over the playing
  clip's position) and Follow Tempo (audio in time at a BPM is stretched
  to its musical length at the tempo it plays at — through its warp map;
  set when audio is sent, placed or recorded into the launcher; record to
  arrangement writes what played).

  The engine takes legato and repeat with a launch and a `Release` (a
  held launch let go: stops at the next launch position, or plays one
  quantum when let go before it started). Follow actions get a second
  action with a chance in a hundred and a jump to a scene.

  The launcher view releases on mouse-up and shows non-trigger modes and
  legato as letters on the slot; its clip menu has submenus (menus can
  nest now: `MenuItem::submenu`, a level with a way back in the GTK
  popover) for the launch mode, quantisation, legato, tempo and the follow
  action (then, otherwise, chance, when). Dev actions
  `launcher:mode=<track>@<n>=<mode>[/<q>][/legato]` and
  `launcher:follow=…[:<other>@<chance>]` with `jump<n>` kinds.

  </details>

- **Modulators reach VST3 and Audio Unit parameters**

  <details><summary>Details</summary>

  Neither format has non-destructive modulation, so it is emulated
  (`plugin_host::emulated`): the processor follows each parameter's value
  as set (UI, automation, mapped MIDI, the plugin's own non-echo changes,
  the values after a loaded state through a bases ring), sends
  base + modulation as an ordinary change (VST3: a point at the block's
  end; AU: a parameter event at the start or with each automation point)
  and the base again when the modulation ends. Continuous automatable
  parameters qualify (not bypass or program switches).

  The host keeps showing the value as set (`parameter()` reads the base
  while modulated, even when a plugin echoes the modulated value to its
  controller; AU editor values outside a gesture are not taken as edits).
  The plugin's state holds modulated values, so the bases of modulated
  parameters are saved after it (`FFMB` trailer after the VST3 state; an
  `FFAU` wrapper around the AU property list only while modulating) and
  set again on load. Nothing allocates on the audio thread.

  Tests: modulation_rides_on_the_value_as_set (in-process VST3, no
  allocations), modulation_moves_the_unit_and_keeps_the_value_as_set
  (Apple's AULowpass, macOS CI); the opt-in installed-plugin test now
  modulates the first parameters (checked with PultEQFx, GainStageFx,
  u-he Satin and Presswerk).

  </details>

- **Capture MIDI: a dropout no longer splits a note into two passes**

  <details><summary>Details</summary>

  Events are positioned from the engine's last callback when they are
  fed; after a dropout (the device lost time) a later event could land
  before an earlier one, which counted as a loop pass and left the note
  hanging to the take's end (seen on the macOS arm64 runner). Only a step
  back of half the loop or more is a new pass now; smaller ones keep the
  pass in order. The test skips its timing checks when the device
  reported an xrun.

  </details>

- **Sampler: full SFZ import (SFZ 1 + common SFZ 2 opcodes)**

  <details><summary>Details</summary>

  A new `devices::sfz` reads the text (block comments, #define, #include
  relative to the instrument with a loop guard, sfizz's embedded <sample>
  data), the headers (<control> with default_path as a prefix reset per
  header, note/octave offsets, set_cc/set_hdcc/set_realcc; <curve>s;
  <global>/<master>/<group>/<region> inheritance) and generators
  (*sine, *saw, *square, *triangle, *noise, *silence). Sample paths
  resolve case-insensitively for libraries made on other systems.

  Zones now carry conditions (channel, key, velocity, CC incl. hdcc,
  bend, aftertouch, random, tempo, round robins, keyswitches sw_last /
  sw_down / sw_up / sw_previous / sw_default, triggers attack / release /
  first / legato / release_key, on_loccN), voice control (group, off_by,
  off_mode, off_time, polyphony, note_polyphony), playback (offset with
  random and CC, end, count, loop crossfade, direction, delay,
  oscillator=on), amplifier (volume, amplitude, ARIA header levels, pan,
  width, position, key and velocity tracking, velocity curves, random,
  rt_decay, key/velocity/CC crossfades), pitch (key centre, tracking,
  velocity, random, bend range and step), two filters (1-6 poles low,
  high, band, reject, peak, shelves) and a 3-band EQ, ampeg/fileg/pitcheg
  and amplfo/fillfo/pitchlfo, all modulatable by controller with curves
  (extended CCs 128-136 too, 512 controllers).

  The sampler evaluates a region's modulation every 16 samples with
  ramped gains; no allocation on the audio thread (an SFZ using all of
  it joins the_instruments_do_not_allocate). Real-world check against
  sfizz's test files (opt-in test real_world_files_read).

  </details>

- **macOS: sandboxed plugins' audio threads join the audio workgroup**

  <details><summary>Details</summary>

  The device's workgroup is kept for the whole process
  (faderframe_realtime::set_process_workgroup, with a generation). Helpers
  get it as a Mach port: FaderFrame checks in a bootstrap service named
  after its pid (given to helpers in FADERFRAME_WORKGROUP_SERVICE) whose
  thread answers each request with os_workgroup_copy_port's send right;
  a helper's fetch thread looks the service up and makes the workgroup
  (os_workgroup_create_with_port) when the host's audio thread writes a new
  generation into the shared block, and the helper's audio thread leaves
  the old one and joins it at the start of a block, reporting the
  generation it joined. Elsewhere this does nothing. Test (macOS runners):
  a_helpers_audio_thread_joins_the_audio_workgroup.

  </details>

- **Varispeed and MTC out: compensated delay, manual speed, own start time**

  <details><summary>Details</summary>

  The varispeed resampler's delay (five frames each way, measured by
  the_delay_each_way_is_delay_frames) is compensated: recordings made while
  it runs (audio and MIDI), the chase and everything using the output
  latency. A manual varispeed (Transport -> Varispeed..., +-10 %, shown as
  VARI on the transport display; dev action set-varispeed) plays the song
  faster or slower like a tape machine while FaderFrame is the master;
  following a master it gives way and comes back. MTC output starts from
  its own timecode (SyncSettings::mtc_out_offset, Preferences -> MIDI ->
  Sync), no longer the one MTC input expects.

  </details>

- **MIDI clock and MTC stay in order; the full frame is where playback starts**

  <details><summary>Details</summary>

  A callback that runs late stamps its MIDI clock and quarter frames late;
  the next one, on time, could then put its messages ahead of them (the
  macOS runners swapped two quarter frames). Their due times now never go
  back. The MTC full frame goes out as playback starts, from where it
  starts, instead of at the next tick (the macOS Intel runner had moved a
  frame on by then).

  </details>

- **Varispeed: follow an external clock without jumps**

  <details><summary>Details</summary>

  engine::varispeed renders as many engine frames per device callback as
  the device's need at a speed within 2 % (the transport advancing by
  them), resampling outputs to the device and inputs from it with an 8-tap
  Lanczos kernel (allocated on the control thread; no allocation on the
  audio thread, varispeed_does_not_allocate). process_device wraps the new
  render in it; position_at extrapolates at the speed. Following MIDI
  clock or MTC, the session now steers the speed with a PI loop on the
  smoothed distance to the master (within 1 %) instead of relocating when
  the clocks drift apart; only a distance four times the tolerance chases
  (Preferences -> MIDI -> Sync: Follow by varispeed, on by default; the
  status shows the speed). A master 0.5 % fast is followed at 1.005 with no
  relocks (the old chase relocated within six seconds).

  </details>

- **MIDI time code output**

  <details><summary>Details</summary>

  Outputs with MTC on get quarter frames from the audio thread while the
  transport plays (four a frame, sequences starting on even frames,
  sample-accurate like MIDI clock; engine::midi::MtcGen) and a full frame
  from the session when playback starts or the playhead jumps (playing or
  stopped; MidiOutputs::send_now, past the cancellable SysEx queue). The
  timecode runs from the project start's (SyncSettings::offset) at its own
  rate (SyncSettings::mtc_out_rate: 24, 25, 29.97 drop, 30). Timecode and
  MtcRate moved to faderframe_midi::timecode for the engine. Preferences:
  an MTC switch per output, the rate under Sync.

  </details>

- **README and architecture: slot recording, follow actions, dragging clips in, surface pages**

- **Control surfaces: send pages, flip, automation modes and save**

  <details><summary>Details</summary>

  The pots move pan or a send (Mackie Send cycles through the sends, HUI
  send a-e pick one; the ring shows a level bar then, the LCD the send's
  level, the assignment display PN/S1…); Flip swaps faders and pots; the
  automation buttons put the selected track's lanes in Off/Read/Touch/
  Latch/Write (a volume lane made when there is none), their LEDs show it;
  Save saves a project that has a file. OSC gets /flip, /page/pan,
  /page/send/K and /automation/<mode>.

  </details>

- **Drag arrangement clips into the clip launcher**

  <details><summary>Details</summary>

  Views can hand a drag to another view: CanvasView::drag_payload (what a
  drag carries once it leaves the view), hover_payload/drop_payload on the
  view under the pointer (found by the canvas host with a pick in the
  window), cancel_drag on the source. The arranger offers the clips it
  moves (and puts them back while they are carried outside); dropped on a
  launcher slot they are copied there and the slots below
  (LauncherOp::PlaceClips) and the move is cancelled
  (Action::CancelGesture, History::cancel: undone, no undo step).

  </details>

- **Capture test: allow a device period of lateness on busy runners**

  <details><summary>Details</summary>

  The note's position and the reference are both extrapolated from the last
  device callback; the macOS Intel runner was 17 ms late.

  </details>

- **Clip launcher: follow actions**

  <details><summary>Details</summary>

  A slot's follow action (Again, Next, Previous, First, Last, Any, Other or
  Stop, after N bars or the clip's length; Launcher::follow,
  Command::SetFollowAction) is queued on the audio thread as soon as its
  clip plays (targets among the track's clips in scene order, random ones
  by xorshift), sample-accurately; it is not shown as a waiting launch, and
  a new snapshot queues them again. Follow actions move, copy and go with
  their clips; set from a clip's menu, marked by a chevron. Every slot
  clip gets a lane, so an empty MIDI clip plays (silence) too. Dev action
  launcher:follow=<track>@<n>=<kind>[/<bars>].

  </details>

- **Clip launcher: record into slots**

  <details><summary>Details</summary>

  An empty slot of an armed track shows a record button: the recording
  starts on the next launch position (playback starts if stopped) for that
  track only, clicking again (or launching or stopping the track) ends it on
  the next one, and the take becomes the slot's clip (audio or MIDI, whole
  launch steps long) and plays on in time from where the recording began
  (LaunchCommand::Resume). Arrangement recording narrows to one track for
  this (start_recording_only); MIDI takes become clips through
  midi_take_clip, bar-rounded or over a span. Dev action
  launch-record:<track>@<scene n>.

  </details>

[All changes since 0.9.0](https://github.com/BurningTreeC/faderframe/compare/v0.9.0...v0.10.0)

## [0.9.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.9.0) — 2026-10-06

- **Release 0.9.0: performance and control**

  <details><summary>Details</summary>

  The clip launcher (scenes of clips launched on the next bar, beat or
  bars, Back to Arrangement, Record to Arrangement) and control surfaces
  (Mackie Control and extenders, HUI, OSC). The README shows the launcher
  in a new screenshot (dev action launcher:name=<n>=<name> names scenes).

  </details>

- **Control surfaces: MIDI ports, session and preferences**

  <details><summary>Details</summary>

  faderframe-midi-io::SurfacePorts: a surface's own input connection (split
  running status) and immediate output; the session keeps those ports from
  the hub and the track outputs (and out of the saved disabled lists), so
  a surface never plays tracks. session::control opens Mackie Control,
  extenders, HUI and OSC (non-blocking UDP, answering the last sender on
  the reply port) from SurfaceSettings, shows the mixer's strips from a
  shared bank start every tick and turns what is done on them into
  actions: faders and pots as gestures (touch to release, or 400 ms of
  rest), mute/solo/arm/select, the transport, banks, undo, markers, the
  click, OSC launching the clip launcher. Preferences -> MIDI -> Control
  Surfaces sets them up (saved in the preferences); dev action
  add-surface.

  </details>

- **Control surfaces: Mackie Control, HUI and OSC protocols**

  <details><summary>Details</summary>

  faderframe-control (no I/O): a SurfaceState of what a surface shows (a
  bank of strips with fader, pan, mute/solo/arm/select, name, level and
  meter; master, transport, bars and beats) and SurfaceInputs of what is
  done on it. Protocol::update sends only what changed; Mackie Control
  (notes, pitch-bend faders, LCD SysEx, rings, ten-digit display, meters
  refreshed every 100 ms), HUI (zone/port switches and LEDs, 14-bit
  faders over two CCs, scribble strips, the 2x40 display, timecode digits,
  meters, a ping each second) and an OSC 1.0 codec with FaderFrame's own
  address space. Held faders are not moved.

  </details>

- **Clip launcher: the grid view**

  <details><summary>Details</summary>

  faderframe-view-launcher (View → Clip Launcher): a column per track, a
  row per scene; a clip's ▶ launches it, a scene's ▶ its row, playing clips
  show a green ▶ and their loop's progress, waiting ones a yellow one; per
  track stop buttons, Stop All, Back to Arrangement, Record to Arrangement,
  the launch quantisation and + Scene in the toolbar; clips drag between
  slots (Ctrl copies), a double click makes a MIDI clip or opens one in the
  piano roll; slot and scene menus. The arranger's clip menu sends clips to
  the launcher and dims the tracks that play it. Theme colours
  launch_playing/launch_queued; dev actions send-to-launcher, launch,
  launch-scene, launcher.

  </details>

- **Clip launcher: session actions and recording to the arrangement**

  <details><summary>Details</summary>

  Action::Launcher(LauncherOp): launch slots and scenes (launching while
  stopped plays), stop a track or all, back to the arrangement; scenes
  added, renamed, duplicated, removed with their clips; empty MIDI clips,
  clips sent from the arrangement to their tracks' first free slots, moved
  or copied between slots. Removing a track takes its launcher clips (undo
  restores them). With Record to Arrangement on, what played is written
  into the arrangement when the transport stops (one undo step). Slot
  numbers stay below 2^63 (the engine's status flag bit).

  The capture test allows a device period between the measured and the
  captured note length (macOS runner flake).

  </details>

- **Clip launcher: model and engine**

  <details><summary>Details</summary>

  Scenes and slots in Project::launcher (launcher clips live in
  Project::clips but in no track's list; arrangement edits and repair leave
  them alone), SetLauncherSlot/SetScenes/SetLaunchQuantize commands. The
  snapshot gives every slot's clip a lane of its own from sample 0 (its
  streamed pages pinned); engine::launch keeps per-track launch state on
  the audio thread: launches wait for the quantised position (beat, bars),
  clip players split blocks where they land, launched clips loop, notes
  end where a clip gives way. Tracks with launcher clips play live and get
  stretcher voices for them.

  </details>

[All changes since 0.8.0](https://github.com/BurningTreeC/faderframe/compare/v0.8.0...v0.9.0)

## [0.8.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.8.0) — 2026-10-06

- **Release 0.8.0: vocals and audio intelligence**

  <details><summary>Details</summary>

  Pitch editing, audio to MIDI (melody, harmony with basic-pitch, drums),
  tempo and key from clips, clip effects rendered offline, and speech and
  lyrics transcription (Whisper) into a Lyrics lane; track presets keep
  containers and modulators, and containers and modulated tracks render
  ahead. The README shows the vocal tools and the clip tools in two new
  screenshots (dev action track-height for compact rows).

  </details>

- **Lyrics: export as LRC and SRT from the Lyrics lane's menu**

- **README: wave 5 (pitch editing, audio to MIDI, tempo and key, clip effects, speech and lyrics)**

- **Speech and lyrics: Whisper in plain Rust, a Lyrics lane**

  <details><summary>Details</summary>

  - faderframe-speech: OpenAI's Whisper without a runtime: safetensors
    weights, the feature extractor's log-mel spectrogram (the checkpoint's
    filters), the encoder and decoder (cached self-attention, cross-
    attention, exact GELU, matrix products on all cores), GPT-2 byte-level
    detokenising and the reference transcription loop (language detection,
    greedy decoding under the timestamp rules, segments, seeking). The JFK
    test clip comes out word for word with whisper-tiny in about a second
    (opt-in test).
  - session::speech: the whisper-base checkpoint (290 MB, Apache-2.0) is
    downloaded on request (Audio -> Download Speech Model) into the data
    folder; Transcribe (an audio clip's menu) puts the clip's words into
    the project's lyrics, replacing the lines inside the clip, in one step;
    lines are edited or deleted one at a time and written as LRC or SRT.
  - faderframe_project::lyrics: Project::lyrics, Command::SetLyrics; lines
    move, copy and cut with sections (arrange).
  - Arranger: a Lyrics lane (shown once there are lyrics): lines lit while
    they play, click to go there, double-click to edit, right-click to
    delete.

  </details>

- **Clip effects: a device chain on one clip, rendered offline**

  <details><summary>Details</summary>

  - faderframe_project::clip_fx: AudioClip::effects keeps a clip's chain
    and its original audio (source, region, warp, pitch edit, reversal)
    with the frame where it starts in the render, so the chain can change
    and render again (trims carry over) or go and give the audio back.
  - session::clip_fx: edits change a pending chain at once; once it rests
    (300 ms) it renders on a one-track copy of the project (the original
    audio, the chain as inserts, 2 s of tail) in a worker, and the clip
    plays the result, chain and all, in one undo step; a render overtaken
    by a newer edit is dropped. EngineController::describe_parameters lists
    a plugin's parameters from an instance made for the asking.
  - faderframe-view-clipfx: the Clip Effects editor (View menu, the
    arranger's clip menu): a card per device with its parameters as drag
    bars, bypass, move and remove; '+ Add Effect' offers the audio
    effects. Dev actions clip-fx, clip-fx-add, clip-fx-set.

  </details>

- **Audio to MIDI: melody, harmony (basic-pitch) and drums**

  <details><summary>Details</summary>

  - faderframe-transcribe: Spotify's basic-pitch (Apache-2.0) in plain
    Rust. scripts/basic_pitch_model.py converts its ONNX model (CQT,
    harmonic stacking, six small convolutions; 36k weights) to
    model/basic-pitch.ffnn, run by graph::Graph, an evaluator for the
    opset-15 operators the graph uses, checked against onnxruntime's
    outputs (tests/reference.json). Windows run on all cores and are
    unwrapped and turned into notes as basic-pitch's inference.py and
    note_creation.py do. 17 s of the demo's plucks take 0.4 s.
  - session::to_midi: Action::ConvertToMidi puts an audio clip's notes on
    a new instrument track below it as a MIDI clip in the same place, in
    one step: Melody by the pitch analysis, Harmony by basic-pitch, Drums
    by onsets sorted into kick, snare and hat on the Drum Sampler's pads.
    In the arranger's clip menu; dev action to-midi.
  - The model and the ported note creation are listed with their licence
    in THIRD_PARTY_LICENSES.md (vendored libraries and models).

  </details>

- **Pitch editor: a keyboard like the piano roll's**

  <details><summary>Details</summary>

  The pitch editor's keys are drawn as the piano roll's: its width and row
  height from the piano-roll theme, white keys reaching halfway under the
  black keys beside them with the same gradients, rounded black keys at
  the same share of the width, the names placed the same way; the keys of
  the hovered and selected notes light up, and the grid shows the key's
  scale along the clip as the piano roll's does.

  </details>

- **Tempo and key from clips**

  <details><summary>Details</summary>

  - faderframe_analysis::tempo: a recording's tempo and first beat from
    an onset envelope (spectral flux at ~11 kHz) and a comb over beat
    periods with a log-normal prior around 120 BPM, refined to 0.01 BPM
    and snapped to whole BPM within 0.05, with a confidence (noise ~0).
  - faderframe_analysis::chroma: a pitch-class profile from spectral
    peaks, for theory::detect_key's Krumhansl-Kessler correlation.
  - session::detect: Action::FromClip sets the project's tempo from an
    audio clip, warps the clip to the project's tempo, or sets the key
    from an audio or MIDI clip (audio analysed in a thread, one undo step,
    a notice saying what was found). In the arranger's clip menu; dev
    action from-clip:<tempo|warp|key>=<track>. The demo's drum loop and
    bassline read 112.00 BPM, its plucks A minor.

  </details>

- **Pitch editing, part 2: detection, edits and the pitch editor**

  <details><summary>Details</summary>

  - session::pitch: Detect Pitch analyses a clip's source in a thread
    (kept for the session) and gives the clips their notes in one step
    (curve values further than a fourth from a note dropped: tracker
    octave errors and the next note's onset). Action::EditPitch with a
    PitchOp: move (a drag sends the total inside one gesture, recomputed
    from where it began), set, correct to the key at the note (by an
    amount, with straightening), split, join, reset, keep formants,
    remove. Action::OpenPitchEditor finds the notes when there are none.
  - faderframe-view-pitch: the pitch editor (View → Pitch Editor, the
    arranger's clip menu 'Edit Pitch'): notes as blobs at the pitch they
    are heard at, the played and the sung curve, a keyboard and the bars;
    drag to move by semitones (Alt: freely), double-click to split, a
    rubber band selects, arrows move, J joins, Delete resets; the toolbar
    detects again, corrects (pitch amount and straightening as drag
    values), keeps or moves formants, resets and removes.
  - Dev actions import-audio-from, edit-pitch, pitch:<op>.
  - The capture test measures the hold on the engine clock: the dummy
    device's timing moved the wall-clock comparison under load.

  </details>

- **Pitch editing, part 1: melody analysis, the pitch edit, PSOLA playback**

  <details><summary>Details</summary>

  - faderframe_analysis::melody: a 5 ms pitch track of monophonic audio
    (McLeod's method on a copy decimated to ~11 kHz, octave errors folded,
    glitches dropped) and its notes (split where the pitch moves to a new
    level, a 150 ms average ignoring vibrato, or the level dips and comes
    back; a note's pitch is the median of its middle). Three minutes of
    audio take about a second.
  - faderframe_project::pitch: AudioClip::pitch, a PitchEdit of notes in
    source frames with their sung pitch and cents curve (so projects play
    without the analysis), each with shift, drift (straightening) and
    formant; corrections glide between notes; snap to a scale; split and
    join keep the sung curve.
  - Engine: pitch-edited clips play through TD-PSOLA voices
    (nodes::psola): grains two sung periods long, cut a sung period apart
    and laid down a target period apart, so the pitch is exact and the
    formants stay (or move with the pitch, or on their own by squeezing
    the grains); the marks follow the clip's time map, so warping comes
    along. Signalsmith's phase vocoder missed small shifts by up to 0.2
    semitones, too much for pitch correction. Voices are counted per track
    like stretcher voices and never allocate (realtime_alloc covers them).
  - faderframe-stretch: formant control (set_formant, set_formant_base),
    covered by the no-allocation test and a formant test; its tests run
    one at a time because the C++ allocation counter is process-wide.

  </details>

[All changes since 0.7.0](https://github.com/BurningTreeC/faderframe/compare/v0.7.0...v0.8.0)

## [0.7.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.7.0) — 2026-10-06

- **Release: a moved tag replaces the release's builds**

  <details><summary>Details</summary>

  gh release create fails when the tag's release exists; when a tag is
  moved, the release now gets the new builds (upload --clobber) and keeps
  its notes.

  </details>

- **CI: no automation written after Stop; sturdier timing tests**

  <details><summary>Details</summary>

  The Ports runs failed on loaded macOS and Windows runners:

  - A plugin editor's move right after Stop was still written as
    automation: the session takes the engine's word for playing, which
    stays true until the audio thread has run the stop. Sending Stop now
    ends writing at once (AutomationWriter::stop_sent, cleared when
    playback starts again); the test moves the plugin right after Stop and
    fails without the fix.
  - tracks_render_ahead_until_armed rendered the whole (grown) demo ahead
    in a debug build: it plays the Lead Synth alone now.
  - Capture MIDI tests compared with nominal sleep times and the last
    reported playhead; they compare with the time a key was really held
    and the position extrapolated as captured events are
    (VirtualMidiInput::clock).
  - The reservoir's render-ahead test allowed a tenth of the frames lost
    to a loaded machine; without its waits nearly all would be, so it
    allows half.

  </details>

- **Render containers and position-driven modulation ahead**

  <details><summary>Details</summary>

  Tracks with containers are rendered ahead like other tracks:
  build_ahead_chain builds them with add_container, which now returns the
  container's latency (its slowest chain's) for the reader's alignment.
  Modulated tracks are rendered ahead when their modulators depend on the
  song position, the track's own signal or its notes only and move plugin
  parameters only; the anticipator gets the ModulationSet through its own
  mailbox and runs the track's ModNode in the ahead graph. A follower of
  another track or a fader/pan route keeps the track live.

  </details>

- **Track presets keep containers and modulators**

  <details><summary>Details</summary>

  A track preset now holds its containers' chains (with their devices,
  mix and key ranges) and the track's modulators. Plugins get new ids when
  a preset is used, so a route to a plugin keeps the plugin's place among
  the track's devices (the order of Track::slots) and finds it there
  again, and a follower names its source track like a send does (falling
  back to the track's own input, with a note, where that track does not
  exist). Applying a preset onto a track replaces its modulators in the
  same undo step.

  </details>

- **Mixer: a dot where modulation has the pan and the fader**

  <details><summary>Details</summary>

  A modulated strip shows where its modulators have the pan (a dot on the
  knob's ring) and the fader (a dot on its slot) right now, like the
  device knobs do; the values themselves stay where they are
  (Session::strip_modulation).

  </details>

- **Containers: notes in key ranges and sidechains for their devices**

  <details><summary>Details</summary>

  On an instrument track each chain's instruments and note-taking effects
  now get the track's notes, through a ChainNotes node that passes the
  chain's key range (every note-off passes, so a range changed while notes
  sound never strands one): layers and key splits. MIDI tracks routed to
  the track reach the chains; a container in a chain takes that chain's
  notes. Devices in chains get sidechain inputs like inserts, keyed by
  another track's pre-fader signal.

  The container editor shows each chain's keys on instrument tracks (click
  to type "C2-B3", double-click for all), offers instruments in + Device
  there, and a device's menu sets its sidechain. A track's instrument may
  sit in a container (instrument_slot looks there too).

  Tests: key splits (a high chain never plays a low note), a MIDI track
  reaching an instrument in a container, a gate in a chain keyed by
  another track, the editor's key range, typed key ranges, no allocation
  with instruments in chains.

  </details>

- **Release 0.7.0: composition, organisation and modulation**

  <details><summary>Details</summary>

  The workspace version is 0.7.0, and the AppStream release notes list
  what came since 0.6.0: the key and chord track, MIDI effects and MIDI
  Tools; MIDI tracks with their own strips and Capture MIDI; folder
  tracks, clip aliases, project versions, the command palette with a
  shortcut editor and the undo history panel; modulators (per note too,
  with CLAP's non-destructive and polyphonic modulation); containers;
  samplers that keep a sample's length, with start and end points and
  made from audio selections; the album's vinyl premaster; and a demo
  that shows them.

  </details>

- **README: up to date, with new screenshots of the extended demo**

  <details><summary>Details</summary>

  The README describes what FaderFrame does now: modulators (per note
  too), CLAP's non-destructive and polyphonic modulation, containers, the
  sampler's start and end points and held keys, the Modulators tab, the
  arranger's wheel and scrollbar behaviour, the shortcuts added since
  (new tracks, Capture MIDI, Save Version, the command palette) and that
  every shortcut can be changed; the engine line no longer lists the first
  built-in plugins only.

  New screenshots, all of the extended demo: the Editing workspace (global
  lanes, a folder, the pad's modulators), the Mixing workspace, the piano
  roll under the key and chord tracks, the Mastering workspace, the album,
  six device editors and the seven skins.

  </details>

- **Modulators view: depth readouts beside their bars**

  <details><summary>Details</summary>

  A route's depth was written over its own bar, where the fill made it
  hard to read; it now stands before the bar (right-aligned, dimmed), and
  the bar is a little narrower.

  </details>

- **Demo: the newer capabilities in the demo session**

  <details><summary>Details</summary>

  The demo now shows much of what FaderFrame does: a key (A minor), the
  chord track (Am – F – C – G), Intro and Verse sections and a marker;
  parallel compression in a container on the drum bus (a dry chain and a
  squashed, driven one); the bass through a preamp, compressor and
  saturator; the plucks through the program EQ; a chorused pad that ducks
  under the drums (an envelope follower) and drifts across the stereo
  field (an LFO); the lead's filter following velocity (per note) and a
  Brightness macro; a MIDI track whose notes the Chord effect turns into
  the chord track's chords, played by an arpeggiating synth whose filter
  an LFO sweeps, the two in a folder; a reverb aux next to the echo; and
  a true-peak limiter on the master. The existing tracks keep their names
  and order.

  Tests that used the demo as a plain fixture now say what they need:
  start without the modulators, sections, key, chords or folder they
  create themselves; paint the mixer before clicking it (its rows follow
  the session, as in the app); look at a MIDI strip's own texts; count
  inserts and choices as the demo has them; and the render-ahead
  comparison leaves out free-running LFOs (playing with render-ahead
  starts once primed, a phase apart at the same song position).

  </details>

- **Chord: no allocation when following the chord track**

  <details><summary>Details</summary>

  The Chord MIDI effect in its Chord Track mode built each chord through
  Chord::voicing, which returns a Vec: an allocation and a free on the
  audio thread for every note. Chord::voicing_into fills a fixed array
  (voicing keeps its signature on top of it) and the effect uses that.
  The MIDI effects' allocation test now runs a Chord in Chord Track mode
  over a chord track with a slash chord; it counts 96 allocations without
  the fix.

  Found playing the extended demo.

  </details>

- **Containers: parallel chains of devices, mixed**

  <details><summary>Details</summary>

  Wave 4, last part. A container (a built-in insert that does nothing
  itself) splits its input into parallel chains, each a series of devices
  with its own level, balance, mute and solo, and mixes them at its
  output; a chain without devices is the dry signal. Containers nest.

  The chains live in Track::containers by the container's slot, so the slot
  moves, bypasses and is removed and restored like any other while its
  chains stay put. Track::slots()/slots_mut()/plugin() reach the devices
  inside, and parameter edits, bypass, state capture, sampler paths,
  automation, modulation targets and plugin hosting use them, so devices
  in chains are the track's like any other. SetContainer changes the
  chains (a graph change), SetChainMix a chain's mix (parameter slots;
  solo resolved to zero gains).

  The engine expands a container in place: the chains fan out, each ends
  in a ChainMix node (ramped gain and balance), and they meet at a sum
  where the graph's delay compensation aligns their latencies. The session
  adds, renames and removes chains and puts devices in and out of them
  (new containers start with a dry chain and an empty one). The editor
  shows a column a chain: name, mute, solo, level and balance bars, the
  devices (light bypasses, click opens the editor, right button removes)
  and + Device; + Chain adds one.

  Tests: mixing, mute, solo and level, a late chain aligned with the dry
  one, nesting, no allocation with nested containers and moving mixes,
  session actions with undo and a save round trip, the editor.

  </details>

- **Per-note modulation: CLAP's polyphonic parameter modulation**

  <details><summary>Details</summary>

  Wave 4, second part. Five per-note modulator sources (velocity, key, a
  note envelope, a note LFO that starts with the note, a note random) have
  a value for every sounding note and move the devices that get the
  track's notes. Plugin nodes follow their notes (NoteVoices: 32 voices,
  closed-form envelopes, released notes through their release) and hand
  each voice its own modulation (PluginProcessContext::note_mods) for
  parameters that take it, sent as per-voice CLAP_EVENT_PARAM_MODs right
  after a new note's note-on: addressed by key and channel for
  IS_MODULATABLE_PER_KEY, by the voice's note id for parameters that only
  take IS_MODULATABLE_PER_NOTE_ID. Other parameters of the device, and
  built-ins, get the newest note's value. The sandbox carries both.

  The session lists which targets get notes and which take per-voice
  modulation and refuses to map a per-note modulator onto a device that
  does not get notes; the Modulators view has the new kinds (an envelope
  with its four knobs, a note LFO with shape and sync) and offers them the
  instrument's parameters only.

  Tests: voices and their offsets, the newest note for other parameters, a
  velocity modulator lowering the built-in synth's volume by the note's
  velocity, no allocation while following notes, CLAP per-voice events
  after their note-on (test plugin), mapping rules, the view. Checked
  against u-he Diva: 62 per-voice parameters, its filter frequency moved on
  one voice changes the voice.

  </details>

- **Modulators: LFOs, followers, steps, random and macros on any parameter**

  <details><summary>Details</summary>

  Wave 4, first part. A track's modulators (at most 16) move its
  parameters without changing them: an LFO (five shapes, synced or free,
  start phase), an envelope follower (the track's input or another track),
  a step sequence with glide, a smoothed random source and a macro knob,
  each routed with a depth to the fader (in fader travel), the pan, and any
  device parameter that takes modulation: continuous built-in parameters
  and CLAP parameters flagged IS_MODULATABLE (sent as CLAP_EVENT_PARAM_MOD,
  also through the plugin sandbox).

  The engine publishes a ModulationSet like the timeline; a ModNode after
  each modulated track's input evaluates the modulators once a block (from
  the song position while playing, so renders repeat) into the track's bus;
  plugin nodes hand their parameters' offsets to the processors
  (PluginProcessContext::param_mods) and the strip moves fader and pan.
  Built-ins apply offsets through ParamValues (octaves for Hz parameters);
  values, state, automation and the editors' copy keep the values as set,
  and kit knobs draw a dot where modulation has the value now. Followers
  hear other tracks through graph inputs (none that would close a loop);
  modulated tracks are not rendered ahead.

  The session adds, changes and removes modulators (one undo step a
  gesture) and maps one by touching a parameter on the track's devices.
  The Modulators view shows the selected track's modulators as cards: a
  live display (steps drawn, macro dragged), knobs, targets with depth bars,
  Map. Tests: engine renders (fader travel, a macro on a device, a
  follower, loops between followers, repeatable renders), no allocation
  while modulating, session mapping and undo, the view's interactions, and
  PultEQFx through the opt-in CLAP test.

  </details>

- **Held scrollbars stop the view following the playhead**

  <details><summary>Details</summary>

  Dragging the arranger's bottom scrollbar while it followed the playing
  playhead was fought by the follow, which pulled the view back on every
  paint. Views now hear when one of their scrollbars is held
  (CanvasView::scroll_held): the host watches presses and releases on its
  bars in the capture phase (the bar's own drag is untouched; a broken grab
  counts as letting go), and the arranger and the piano roll do not follow
  while a bar is held. Let go, and following picks up again.

  </details>

- **Arranger: the wheel over the track headers never scrolls**

  <details><summary>Details</summary>

  A wheel meant for a header's fader or pan knob could land between
  controls and scroll the tracks under the pointer instead. Over the
  header panel the wheel now only turns the control under it; elsewhere it
  does nothing. Ctrl+wheel (zoom) and Alt+wheel (track heights) still work
  there, and the lanes scroll as before.

  </details>

- **Sampler: an end point**

  <details><summary>Details</summary>

  The sampler had a start point but always played to the sample's end
  (unless looping). The new End parameter (id 23, default the end) bounds
  the region with the start: the editor has an END knob and a second white
  marker to drag, and dims what does not play. Reverse now plays that
  region backwards, from the end marker down to the start (it counted the
  start from the sample's end before). The last 3 ms before the end fade
  out, so an end point inside the waveform does not click. The loop's end
  stays within the region. The editor opens 60 px wider for the knob.

  </details>

- **Sampler: light every held key on its keyboard**

  <details><summary>Details</summary>

  The sampler published only its newest voice's key, so its keyboard lit
  one key however many were held. It now publishes the held keys (attack,
  decay or sustain; a released note fading out goes dark) as six values of
  24 key bits each, exact in an f32, and the editor lights each of them.

  </details>

- **Undo history view**

  <details><summary>Details</summary>

  A dockable History panel (Edit or View → Undo History) lists every step
  of the project's history: the project as opened, each step done, and —
  dimmed — the steps that can be redone, the current one marked. A click
  on any row undoes or redoes to just after it in one action (the engine
  synced once, Action::HistoryTo); the panel keeps the current step in
  view as steps come.

  Session::history_steps reads the labels (History::undo_labels /
  redo_labels). The view is its own crate (faderframe-view-history) and a
  new view kind, so it docks, detaches and stays in the layout like the
  others.

  This completes wave 3 (Organisation).

  </details>

- **Command palette and keyboard shortcut editor**

  <details><summary>Details</summary>

  View → Command Palette… (Ctrl+Shift+P) lists every command of the menus
  by its place ("File › Save Version…") with its shortcut; typing finds
  them by words (whole words first, word starts before middles, letters in
  order as a last resort), Up/Down and Enter run one, Esc closes.

  View → Keyboard Shortcuts… lists the same commands with their shortcuts:
  click one and press the new keys (Esc cancels, Backspace leaves it
  without one); a shortcut taken from another command leaves that one;
  reset one command or all. Changes are kept in the preferences and
  applied over the defaults at start-up. The single-key shortcuts (Space,
  Home, L, K, Shift+R, T, A) stay fixed and are listed, as they leave text
  fields their keys.

  Both walk the menu model, so a new menu item is in them without more
  work. The defaults moved to palette::DEFAULT_SHORTCUTS.

  </details>

- **Project versions: save, compare, restore**

  <details><summary>Details</summary>

  File → Save Version… (Ctrl+Alt+S) keeps the project as it is now under a
  name, as a numbered project file in the project's Versions folder (media
  paths relative to the project folder, so they share its media and move
  with it). File → Versions… lists them with how long ago they were saved:

  - Compare names what the project has changed since a version: tracks
    added, removed, renamed or reordered, their volume, pan, mute, solo,
    polarity, output, colour, folder, plugins (added, removed, bypassed,
    settings, order), sends and automation; clips added, removed, moved or
    edited per track; tempo, time signatures, key and chord tracks,
    markers, sections, loop, groups and album
    (faderframe_project::compare);
  - Restore goes back to one, keeping the project as it is as a version
    first ("Before restoring n"), and leaves the session unsaved so the
    project file changes only when it is saved.

  Versions need a saved project; old-style instruments in older versions
  are converted as on opening.

  </details>

- **Samplers: change the pitch without changing the length**

  <details><summary>Details</summary>

  The Sampler (Pitch: Keep Length) and the Drum Sampler (per pad) can now
  move a sample's pitch while keeping its length: such a voice reads its
  sample at the sample's own speed and a Signalsmith stretcher (the one
  warping uses) transposes it; envelope, filter, loops with their
  crossfade, reverse, start and pitch bend work as before.

  There is no added latency: on its first block a voice primes its
  stretcher (the analysis fed ahead from the sample in memory, the output
  latency run and discarded), so it sounds when its note comes. Priming
  costs about a third of a millisecond, so a block primes at most one voice
  per 1.5 ms of its length (others start a block later), and after the
  sample's end the stretcher is fed silence for its latency so nothing is
  cut. Sixteen voices can keep their length at once (more steal the
  oldest). The stretchers are made with the processor only when the mode
  is on; switching it restarts the processor.

  Tests: lengths and pitches in both modes for both samplers, no start
  delay, no allocations in the instruments with Keep Length on, and the
  stretcher's transposition proven allocation-free.

  </details>

- **Clip aliases**

  <details><summary>Details</summary>

  Clips can share their content: Duplicate as Alias (clip menu) puts an
  alias right after each selected clip. An edit that changes one alias's
  content — notes, controllers, length, gain, fades, warp — is made to
  every alias in the same undo step (a front trim moves their starts as
  much); each keeps its own position, name, colour and mute. Make Unique
  ends it, splitting an alias makes the split clip its own first, and
  copies and pastes are never aliases. Aliases carry a mark (two
  overlapping frames) before their names.

  The links are a side table on the project (clip → link id), saved with
  it and cleaned of missing clips and single members on load; playback,
  rendering and the clips themselves are unchanged.

  </details>

- **Folder tracks**

  <details><summary>Details</summary>

  A new track kind holds tracks (and folders) to organise a project:

  - the arranger shows a folder's tracks indented right under it, with a
    stripe in each enclosing folder's colour, and a folder lane with a bar
    per track for its clips; a folder closes by its triangle (or a
    double-click), hiding its tracks; folder rows are compact by default;
  - its mute and solo reach everything inside (as a VCA's do), including
    MIDI tracks;
  - Track menu (and the "+"): New Folder with the Selected Tracks (inside
    the folder they share), Move into / out of a folder, Select Its
    Tracks, Sum into a New Bus (a bus in the folder taking the tracks that
    went to the master);
  - removing a folder keeps its tracks: they move up a level in the same
    undo step.

  Editors list tracks in folder order (each folder followed by what it
  holds), so a folder's tracks stay together whatever the project order;
  the mixer follows the same order (folders have no strip).

  MIDI tracks had no working mute: they have no channel strip, so their
  mute (and another track's solo) never reached the notes. A MIDI track
  now has a mute slot its clip player and live input check, releasing
  sounding notes when it is set.

  </details>

- **Capture MIDI: what was played live becomes clips afterwards**

  <details><summary>Details</summary>

  FaderFrame now keeps the last ten minutes of what the tracks that play
  live were played, recording or not, and Capture MIDI (the new button
  after Record, Ctrl+Shift+C, Transport → Capture MIDI) turns the latest
  playing into clips on those tracks as one undo step:

  - played while the song ran, the notes land where they were heard
    (the processing position at their MIDI-clock time, less the output
    latency and the record offset, as a recording places them; loop wraps
    start new passes, kept as the loop-record setting says);
  - played while stopped, the latest phrase (back to an 8 s pause) starts
    on the bar at or before the playhead with its timing kept.

  The kept messages go through the MIDI take machinery recording uses
  (notes, controllers, record mode). A capture takes only what is new
  since the last one, and what a recording took counts as taken. Nothing
  runs on the audio thread: the session already sees a copy of every
  incoming message. The button greys out when there is nothing to capture.

  This completes wave 2 (Composition).

  </details>

- **Arranger: size tracks by the edges of their headers**

  <details><summary>Details</summary>

  The resize zone was the bottom 4 px of a whole row: invisible, only on
  one side of the boundary, and with take or automation lanes open it was
  at the bottom of the last lane (or taken by the lanes) instead of at the
  track's own edge. Now the edge under each track's own part is a zone a
  few pixels either side of the boundary in the header column (dragging
  the top edge of the next header works too), it lights up while hovered
  or dragged, Alt-drag sizes every track and Alt-double-click resets them
  all. Hovering the "+" or an edge now repaints at once.

  </details>

- **Convert old-style instruments into inserts**

  <details><summary>Details</summary>

  Older projects (and track presets) kept an instrument track's
  instrument in a slot of its own, apart from the inserts. It is now an
  insert like any other wherever a project or a preset comes in:
  Session::open and both ways of using a track preset put it after the
  track's MIDI effects, where it played. When the inserts already hold an
  instrument, the old one was never heard (an instrument replaces what
  comes in), so it is dropped and a notice names it; the file keeps it
  until the project is saved. The demo's Lead Synth is an insert too, so
  nothing creates the old slot any more; the engine still plays it.

  Tests that built on the demo's old-style instrument now build that
  layout themselves.

  </details>

- **Piano roll: a keyboard like a real one**

  <details><summary>Details</summary>

  The keyboard drew every key as its own row and found the key under the
  pointer by the row alone: the white keys did not reach under the black
  ones, and pressing the white key beside a black key played the black
  key. White keys now meet halfway under their black neighbours (where
  those are shown; folded rows stay as they were), black keys are only as
  wide as they are drawn, and clicks, drags along the keys and the hover
  highlight follow the same shapes (PianoRollView::keyboard_key_at). Black
  keys carry their names on the key.

  </details>

- **Mixer: a strip for MIDI tracks**

  <details><summary>Details</summary>

  MIDI tracks now have a strip in the mixer, keeping the rows of the
  others so their sections line up: PLAYS (the instrument track it plays,
  in its colour, with its instrument; a click chooses another) where the
  others have their preamp, the MIDI input and live-play mode, the MIDI
  effects in the insert slots (an empty slot opens the browser on MIDI
  effects; drag to reorder, or onto an instrument track), the notes it
  plays now by name and lit on a key display where the fader would be,
  mute, solo and arm, and the external MIDI device in the output well.

  Moving and copying plugins refused every track without audio, so MIDI
  effects on a MIDI track could not be reordered at all. They now move
  within and between MIDI and instrument tracks (onto an instrument track,
  before its instrument); MIDI tracks take no audio effects and audio
  tracks no MIDI effects.

  Session::sounding_keys: the keys a MIDI or instrument track plays now
  (its clips' notes under the playhead while playing, held keys while it
  plays live).

  </details>

- **Engine: a MIDI track plays every instrument of its track**

  <details><summary>Details</summary>

  A MIDI track routed to an instrument track was connected to one node
  only: the track's first MIDI effect, else its old-style instrument slot,
  else the first insert taking notes. An instrument track with an
  old-style instrument followed by a synth insert (which replaces the
  audio coming in) therefore played the MIDI track's notes on the first
  synth, whose sound the second one discarded: silence, while the
  track's own notes, which reach both, played. The graph now records
  every node taking an instrument track's own MIDI as it comes (the first
  MIDI effect, or the instrument and the note-taking inserts before any
  MIDI effect) and connects routed MIDI tracks to all of them.

  </details>

- **Dev action midi-plays: route a MIDI track to an instrument track**

  <details><summary>Details</summary>

  midi-plays:<MIDI track>=<instrument track|none> chooses the instrument a
  MIDI track plays, as its track menu does, for scripted checks of live
  play and playback through MIDI tracks.

  </details>

- **MIDI tracks: choose the instrument track they play**

  <details><summary>Details</summary>

  The engine has always played a MIDI track through the instrument of the
  track its output names, but nothing in the UI set that output: MIDI
  tracks have no mixer strip, and their header menu only offered external
  MIDI ports. The track menu now lists "Plays: None" and every instrument
  track (with its instrument), a MIDI track added while an instrument
  track is selected plays that one, and the header shows "→ <track>" or
  "→ no instrument" (and no channel layout, which MIDI tracks lack).

  </details>

- **Arranger: no hidden fader on MIDI track headers**

  <details><summary>Details</summary>

  MIDI track headers paint no pan, fader or meter, but the hit test still
  found them: the pointer showed the resize cursor and a "Volume" tooltip
  over empty space, and the scroll wheel there changed the track's unused
  volume. The header's hit test now offers pan, fader and meter only on
  the tracks that show them.

  </details>

- **Samples from audio selections**

  <details><summary>Details</summary>

  Right-click an audio clip, or an audio track's header, to turn the edit
  selection (or the selected clips, or the clip under the pointer) into a
  sample. It is rendered as the clips play, with clip gain, fades, warp
  and reverse but without the track's plugins, fader or pan, into the
  media folder's Samples, and then:

  - played by a new Sampler track, whose root key and tuning come from the
    audio (McLeod pitch over the first seconds; no clear pitch keeps C4),
  - put on pad 1 of a new Drum Sampler track,
  - put on the next free pad of a Drum Sampler already in the project, or
  - saved as a 24-bit WAV file wherever the user chooses.

  The render and the work after it (latency trim, a 1 ms fade where a cut
  goes through sound, the pitch, decoding into the samplers' cache) run on
  a worker thread; the new track and its instrument are one undo step and
  the sampler's editor opens (session::sampling, Action::MakeSample,
  Action::PromptSaveSample, UiRequest::SaveSample).

  The Tuner's pitch detector moved from its editor to
  faderframe_analysis::pitch so the session can use it.

  Both WAV writers now pad an odd-sized data chunk as RIFF requires: a
  24-bit mono file with an odd number of frames was unreadable before.

  </details>

- **Mixer: wider or narrower strips**

  <details><summary>Details</summary>

  Drag a channel strip's right edge to resize it (64 to 240 px; Shift
  resizes every strip), double-click the edge for the default width, or
  pick Narrow / Normal / Wide / Extra Wide (and "Every Strip This Wide")
  from the strip's menu. Widths are per track with an all-strips default,
  kept in the workspace like arranger track heights
  (Action::SetStripWidth, WorkspaceSet::strip_widths), so they are saved
  with the layout and need no undo.

  The mixer lays strips out from per-strip offsets instead of a fixed
  pitch: visibility, hit tests, drop positions for reordering, scrolling
  and the "+" all follow them. Dev action strip-width:<track|all>=<px>.

  </details>

- **Arranger and mixer: a "+" to add a track**

  <details><summary>Details</summary>

  A "+ Add Track" button under the last track header in the arranger and
  a round "+" right of the last channel strip in the mixer open the same
  choices as a popover: mono or stereo audio, instrument (which opens the
  plugin browser for its instrument), MIDI, bus, aux, VCA, and a new track
  from each saved track preset. The choices come from the session
  (Session::add_track_choices, Action::AddInstrumentTrack), so both views
  offer the same; the mixer scrolls far enough to reach its button.

  </details>

- **Piano roll: MIDI Tools — transformations and generators**

  <details><summary>Details</summary>

  A MIDI Tools panel on the piano roll's right (toolbar: MIDI Tools) with
  ten transformations of the selection (or every note) and five
  generators (in the bars the selection spans, or the whole clip):

  - Transform: Strum, Chop (ratchets with a velocity ramp), Join, Connect
    (scale runs through the melody's gaps), Arpeggiate, Recombine (shuffle,
    rotate or reverse pitches; shuffle velocities or lengths), Conform to
    Chords (the chord track; the scale where there is none), Accent,
    Time Scale and Ornament (grace notes, flams, mordents).
  - Generate: Euclidean rhythms, Seed melodies (a walk in the key from a
    seed), Chords from the chord track (voice-led round a register),
    Basslines on its roots (five patterns) and Drum Patterns (five styles
    on the General MIDI keys, so the Drum Sampler plays them), adding to
    the clip or replacing what is there.

  The tools are pure and deterministic (faderframe_project::midi_tools);
  the session previews them (the grid shows the result outlined over what
  it replaces, the panel what applying will do) and applies exactly that
  as one undo step, the result selected. Settings are dragged, wheeled or
  clicked; Enter applies. Dev action midi-tool:<label>|apply|off.

  </details>

- **MIDI effects before the instrument: arpeggiator, chord, scale, note echo**

  <details><summary>Details</summary>

  Four built-in MIDI effects (notes in, notes out, no audio) that sit in an
  instrument track's insert chain before the instrument, or on a MIDI
  track to shape what it sends:

  - Arpeggiator: nine orders over one to four octaves, on the song's grid
    (rate, swing) or free from the first key when stopped, gate with
    legato, fixed or played velocity, repeats and hold.
  - Chord: each note a chord from up to five intervals, the key's triad or
    seventh, or the chord track's chord voiced round it; strum and softer
    added notes.
  - Scale: notes kept in the key (nearest, up, down or blocked), then moved
    by scale degrees and octaves; note ends follow the notes.
  - Note Echo: up to 16 fading repeats, synced or in ms, optionally
    climbing or falling by semitones, with or without the played note.

  The engine chains a track's MIDI effects in insert order and feeds each
  later instrument what the effects above it made (live and render-ahead
  graphs); MIDI routed in from MIDI tracks enters at the first effect, and
  bypassed effects pass their notes through. Effects follow the key and
  chord tracks through the timeline snapshot (PluginProcessContext gains
  the harmony). Nothing allocates on the audio thread.

  The session puts a MIDI effect before the first instrument and refuses
  it on audio tracks (and audio plugins on MIDI tracks); the browser has a
  MIDI Effects filter, the arranger's track menu "Add MIDI Effect…". Each
  effect has an editor (the pattern as a small piano roll, the chord on a
  keyboard, the key's degrees, the repeats in time) and 12-13 factory
  presets, played through by a test.

  </details>

- **Piano roll: follows the key and chord tracks**

  <details><summary>Details</summary>

  The piano roll's scale is now the key track's key at each position
  (Follow the Key Track, on by default; a scale chosen in the menu applies
  where the project has no key or when following is off). Rows are shaded
  outside the scale and lit on the chord track's chord tones, span by span;
  notes outside the scale where they sit get a corner mark; scale snap,
  Fold to Scale, Transpose in Scale and the scale chords use the key at the
  note. A new chord stamp, From the Chord Track, draws the chord track's
  chord voiced round the clicked key. Under the ruler a Key row and a
  Chords row show the project's keys and chords along the clip. Dev action
  set-chord:<from>-<to>=<chord>.

  </details>

- **Album: a vinyl premaster with sides, checks and a cutting sheet**

  <details><summary>Details</summary>

  The album can now go to vinyl alongside the CD: 12″ at 33⅓ or 45 rpm,
  10″ at 33⅓ and 7″ at 45, each with its recommended and maximum side
  length. Sides keep the album's order: split automatically (as few as fit
  the recommended length, an odd count taking the record's other side too,
  the longest side as short as can be) or by hand from a row's menu, which
  keeps the current sides as breaks.

  The analysis now measures what a groove makes of each song (bass side
  energy and the lowest left/right correlation below 150 Hz, the loudest
  10 ms of the sibilance band, the share above 8 kHz), and the checks list,
  worst first: sides over the format's lengths, out-of-phase or wide bass,
  esses that will distort at the premaster's level, hard limiting, and a
  side's brightest or loudest song at its inner groove. The export writes
  one continuous 24-bit file per side (the digital release's balance,
  brought to the vinyl peak with gain only), each song's file named for
  its place (A1, B2, …) and a cutting sheet with the side times, songs and
  findings.

  The Album view gets a Vinyl menu, numbers rows by side with a line where
  a side starts, badges songs with findings (the tooltip lists them) and
  shows the side times under the summary.

  </details>

- **Synth presets: balanced loudness**

  <details><summary>Details</summary>

  The mono leads and basses came out 12–15 dB under the pads. Every patch
  is now set to −18 LUFS momentary as it is meant to be played (a bass on
  C2, a lead on C4, a poly patch as a four-note chord), with at least 1 dB
  of headroom; a test measures it (K-weighted, 400 ms windows) and prints
  the volume each patch needs. The roadmap marks the key and chord track
  done.

  </details>

- **Tests: fix macOS preamp and reservoir timing failures**

[All changes since 0.6.0](https://github.com/BurningTreeC/faderframe/compare/v0.6.0...v0.7.0)

## [0.6.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.6.0) — 2026-10-05

- **preamp models and slots**

[All changes since 0.5.0](https://github.com/BurningTreeC/faderframe/compare/v0.5.0...v0.6.0)

## [0.5.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.5.0) — 2026-10-05

- **Release 0.5.0: mixer editing, crosstalk and transport fixes**

[All changes since 0.4.0](https://github.com/BurningTreeC/faderframe/compare/v0.4.0...v0.5.0)

## [0.4.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.4.0) — 2026-10-05

- **Release: bump version to 0.4.0 and format album test**

- **UI: reuse drawing paths to prevent periodic playback freezes**

- **Stock plugins: factory presets with undo and Aux return mixes**

- **Album: play the album as it will be delivered**

  <details><summary>Details</summary>

  Play renders the album like Export's first pass, levels and limits each
  song as delivery does and joins them with their pauses and crossfades
  into one float file, then the engine plays that file instead of the
  project (a preview source read wait-free on the audio thread, its pages
  kept resident by the disk loader, feeding the Tools meters as the
  master). The Album view gets a player strip — play/pause from the
  selected song, previous/next, a seek bar with the songs on it, back to
  the project — and the playing row shows its progress; "Play from Here"
  on a row. Playing again re-renders only when the album or the project
  changed; starting the project's transport hands the outputs back.

  </details>

- **Composition: the project's key and the chord track**

  <details><summary>Details</summary>

  Music theory shared by everything (faderframe_midi::theory): scales,
  keys (snap, scale steps, diatonic chords), 24 chord qualities, chord
  symbols parsed and named, close voicings, chord recognition from notes
  with inversions, Roman numerals and Krumhansl–Kessler key detection.

  The project keeps key changes and a chord track (Command::SetKeys and
  SetChords, normalised), carried along by section moves, copies and
  deletes. The arranger gains Key and Chords lanes: pick or type keys,
  drag out and type chords, move and resize them, choose from the key's
  diatonic chords; chords and the key can be detected from MIDI clips
  (Action::DetectChords, DetectKey).

  </details>

- **Album: a selected song is tinted, not filled**

  <details><summary>Details</summary>

  The selected row was filled with the skin's selection colour — sand on
  Vintage, cyan on Frost and Neon — under the light text of dark skins,
  which made it unreadable. It now gets a translucent tint and an outline
  in that colour, as selections do everywhere else.

  </details>

[All changes since 0.3.0](https://github.com/BurningTreeC/faderframe/compare/v0.3.0...v0.4.0)

## [0.3.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.3.0) — 2026-10-04

- **Windows: compare a mapped sample path in the platform's spelling**

  <details><summary>Details</summary>

  The sample-state test expected "/y/kick.wav"; Path::join spells it
  "/y\kick.wav" on Windows. The 0.3.0 notes also mention the crest factor
  and DR value.

  </details>

- **Tools: crest factor and DR value next to LRA and PLR**

  <details><summary>Details</summary>

  The loudness pane gains a Dynamics group: loudness range (LRA),
  peak-to-loudness ratio (PLR), the crest factor of the measurement and
  the DR value after the TT Dynamic Range Meter (3 s blocks, second highest
  block peak over the RMS of the loudest 20 %, coloured 1–7 / 8–13 / 14+).

  </details>

- **Release 0.3.0**

  <details><summary>Details</summary>

  Workspace version 0.3.0 (the packages take it from Cargo.toml) and the
  0.3.0 release notes in the AppStream metainfo: the stock devices.

  </details>

- **Samplers: samples belong to the project, load in the background**

  <details><summary>Details</summary>

  Files picked for (or dropped on) the sampler and the drum sampler are
  copied into <media>/Samples (SFZ instruments referenced where they lie)
  and decoded on a worker thread into a process-wide sample cache; the
  state change is then one "Load Samples" undo step and the instance loads
  from the cache at once. Concurrent loads merge into the current document.
  Sampler paths are absolute in memory, relative to the project in the
  file, and move with the media on the first save (also in states undo
  brings back). Editors show "Loading…", highlight drop targets, and
  waveform overviews adapt to the sample's length.

  Wave 1 of the roadmap (stock devices) is complete.

  </details>

- **Stock instruments: synth rework, sampler (SFZ) and drum sampler**

  <details><summary>Details</summary>

  - Synth: two PolyBLEP oscillators with octave/semitone/level and pulse
    width, up to seven unison copies spread in pitch and stereo, sub and
    noise, drive into a 12/24 dB low pass, band or high pass filter with
    key tracking, velocity and its own envelope (or the amp's, as before),
    an LFO to pitch/cutoff/level/PW, poly/mono/legato with glide, up to 32
    voices. The first nine parameters, MIDI/MPE handling and per-note
    expressions are unchanged. New editor with oscillator, filter and
    envelope displays over two decks.
  - Samples: a SampleDoc packed into the plugin state after the parameter
    block, loaded on the control thread (any importer format, SFZ parsed
    into zones with ranges, loops, envelopes, release triggers, choke
    groups, round robins and random layers), swapped into a TryCell the
    processor only try_locks, shown to editors via AnalysisTap assets.
  - Sampler: a sample across the keys (root, loop modes with crossfade,
    start, reverse, key tracking) or an SFZ; ADSR, filter, velocity; editor
    with the waveform, draggable markers and a zone keyboard.
  - Drum sampler: 16 pads with level, pan, tune, attack, decay, start,
    choke, reverse, one-shot/gate, low pass and velocity; editor with a
    lit pad grid and the picked pad's waveform and controls.
  - Instrument/sampler accents, dev actions set-instrument, show-instrument
    and device-samples; the_instruments_do_not_allocate.

  </details>

- **Stock effects: saturator, utility, delay, reverb, modulation, tuner**

  <details><summary>Details</summary>

  - Saturator: six curves of unit slope (soft, tape, tube, transistor,
    fold, clip) with bias, 1–8× oversampled with the dry path aligned,
    DC blocker, tilt, low and high cut, static auto gain; editor with the
    curve and the input's reach over the spectrum.
  - Utility replaces Gain (same id, parameter 0 unchanged): a ramped 2×2
    matrix for channels, polarity, width, balance, gain and mute, a DC
    filter and phase-coherent mono bass; vectorscope, correlation and
    balance.
  - Delay replaces Echo (same id, its parameters keep their meaning):
    stereo/ping-pong/mono, tempo sync, stereo offset, feedback to 110 %
    through a safety curve, per-pass low cut, damping, saturation and a
    digital/tape/analog style, wow and flutter, ducking, width, freeze;
    editor with the repeats on a timeline and one pass's spectrum.
  - Reverb: pre-delay, early reflections, allpass diffusion and a 16-line
    Hadamard FDN with modulated taps and an exact three-band decay per
    line; Room/Hall/Plate/Chamber/Ambience, freeze, ducking, wet cuts and
    width; RT60 verified within 12 %; editor with the decay picture and
    decay over frequency.
  - Modulation: chorus, ensemble, flanger, phaser (2–12 stages) and
    vibrato with a free or synced LFO in six shapes; editor with the LFO
    and the voices or the live comb/notch response.
  - Tuner: pass or mute; McLeod pitch detection in the editor (within a
    cent, 25 Hz–4.2 kHz), needle or strobe, adjustable A4.

  All are covered by the_stock_devices_do_not_allocate.

  </details>

- **Stock dynamics: compressor, limiter, gate and de-esser with editors**

  <details><summary>Details</summary>

  A DSP toolkit shared by the stock devices (plugin_host::dsp: matched
  filters, SVF, one-pole, DC block, the halfband oversampler, a Hermite
  delay, envelope followers with programme-dependent release, LFOs,
  smoothing, BS.1770 true peak) and a device editor kit in view-devices
  (faces with a display, a deck of titled sections whose controls are
  bound to parameter ids, meters, history and spectrum displays; right
  click for default, automation and MIDI learn; every drag one undo step).

  - Compressor rewritten: five styles (feedback slopes compensated so the
    ratio reads true), knee, range, auto release and makeup, lookahead,
    RMS/peak, link, colour, mix, filtered external sidechain and listen;
    editor with the transfer curve (drag the threshold) and a history.
  - Limiter: lookahead with a guaranteed ceiling (window minimum averaged
    over an attack window within the lookahead), three styles, auto
    release, link, true-peak mode, unity gain; history and readouts.
  - Gate: gate with hysteresis and hold, expander, ducker; keyed from the
    input or sidechain with filters, listen and lookahead.
  - De-esser: split (dynamic shelf or bell) or wide, high or band
    detection, relative or absolute, link, lookahead, listen; spectrum
    display with the detector band and the cut it makes.

  Device peaks reach editors through AnalysisTap::raise_value/take_value;
  DeviceTheme replaces EqTheme. The new devices are covered by
  the_stock_devices_do_not_allocate. Dev action device-set:<id>=<v>/…

  </details>

- **EQ: sidechaining and the Pro-Q 4 feature set**

  <details><summary>Details</summary>

  Sidechain throughout: every dynamic band chooses its trigger (the input
  or the sidechain), filtered to its region or by free 24 dB/oct low and
  high cuts, and the trigger can be auditioned. The sidechain source is
  picked in the editor; its spectrum shows in the analyser, and where it
  crowds the output's frequencies they glow red (collisions). Another EQ's
  output can be the external spectrum instead.

  DSP (plugin_host::eq):
  - design: every shape a cascade of analog sections; new flat tilt and
    all pass; cuts of any slope 0-96 dB/oct (fractions as first order
    step ladders) and brickwall; Butterworth shelving of any order;
    steeper notches and band passes; AnalogBand gives the analog
    magnitude and phase.
  - natural phase: the matched sections plus a 1024-tap correction FIR
    (analog over digital, 2x2 for side/mid bands), 128 samples latency.
  - linear phase up to 65536 taps; both FIRs share a partitioned
    convolver with a design thread (fir).
  - dynamics: soft knee, auto threshold from the trigger's long-term
    level, frequency-dependent attack and release, custom times.
  - spectral dynamics: an STFT stage acting per frequency (density,
    3 dB/oct tilt, auto threshold), linear phase, latency = frame.
  - character (subtle transformer, warm tube), output pan (L/R or M/S),
    phase invert, soft latency-compensated bypass, gain-Q interaction.
  - EQ Match fit (matching).
  - Parameter ids stay stable: new fields in a second id block, the first
    version's slope slot and global attack/release/sidechain retired.

  Editor (view-devices::eq, rewritten): Pro-Q-style floating band
  controls with the dynamic range ring and custom dynamics, values beside
  each band to drag, scroll or type ("1k", "A4", "2x"), multi-selection
  and rectangle selection, copy/paste between EQs, EQ Sketch, Spectrum
  Grab, EQ Match (sidechain, another EQ or recorded input as reference),
  the instance list, piano display, zoomable frequency scale, A/B,
  analyser resolution/speed/range/tilt/freeze. Editor-only settings are
  Action::SetDeviceView values the session keeps per instance; EQ colours
  come from Theme::eq.

  Also: the tap gets a sidechain ring; device-click targets the latest
  device editor; device-demo:eq-sc. Realtime allocation test covers every
  new stage.

  </details>

- **Stock devices: the FaderFrame EQ and the Program EQ (from PultEQFx)**

  <details><summary>Details</summary>

  Wave 1 of the roadmap, the first two devices.

  EQ (faderframe.eq): 24 bands — bell, low/high shelf, low/high cut (6 to
  96 dB/oct), notch, band pass, tilt — with per band placement (stereo,
  left, right, mid, side), dynamic range keyed by the band's own region or
  the sidechain, auto gain, gain scale and listening to one band.
  - eq::design keeps the analog shape to Nyquist: matched poles (Vicanek),
    numerators exact where the shape is defined and least-squares fitted to
    the analog magnitude above; first order sections and notches solved for
    their defining points. Tests compare every type with the analog curves.
  - eq::linear: linear phase from the bands' analog magnitudes (exact),
    2 x 2 FIRs for mid/side, partitioned convolution (latency N/2 + 256),
    kernels designed on a thread and crossfaded in; phase mode and quality
    restart the plugin (latency).
  - Editor (faderframe-view-devices::eq): response display with a pre/post
    analyser, band curves, live dynamic movement, draggable nodes, menus,
    a band panel with themed knobs, top bar for the global settings.

  Program EQ (faderframe.program-eq): PultEQFx's DSP by Simon Huber, used
  under the MIT licence — the passive network by nodal analysis, the tube
  stage, oversampling, 74-sample latency — with its response and latency
  tests, its "Low End Punch" preset as a program, and its hardware panel
  drawn with FaderFrame's painter using its renders.

  Infrastructure: AnalysisTap (live parameters, analyser rings, meters,
  published values) through PluginInstance::tap/Session::plugin_tap;
  Painter::image (filmstrips, tint), fill_path_paint, push_transform;
  device editor windows (plugin_window::open_device); built-ins' programs;
  recording a program now refreshes the slot's explicit values (they used
  to override the program on load). Both devices are covered by the
  realtime allocation test. Dev actions device-demo and device-click.

  </details>

[All changes since 0.2.0](https://github.com/BurningTreeC/faderframe/compare/v0.2.0...v0.3.0)

## [0.2.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.2.0) — 2026-10-04

- **Icons: FaderFrame's own icons in a hicolor theme folder**

  <details><summary>Details</summary>

  GTK before 4.16 does not recolour loose -symbolic files on the icon search
  path, so the metronome stayed #2e3436 (invisible on the dark toolbar) with
  the Linux tarball's bundled GTK 4.14. The icons now go into
  <cache>/icons/hicolor/scalable/apps (with an index.theme for systems
  without hicolor; an installed theme's index wins), and loose ones from
  earlier versions are removed. Checked with GTK 4.14.5 and 4.22.4.

  </details>

- **Linux tarball: find gdk-pixbuf-query-loaders on Debian and Ubuntu**

  <details><summary>Details</summary>

  Ubuntu installs it beside the loaders (/usr/lib/<triplet>/gdk-pixbuf-2.0),
  not on PATH, so the v0.2.0 release's tarball job failed. Try PATH, the
  pkg-config variable, then the loaders' parent folder.

  </details>

- **Release 0.2.0**

  <details><summary>Details</summary>

  Workspace version 0.2.0 (the packages take it from Cargo.toml) and the
  0.2.0 release notes in the AppStream metainfo.

  </details>

- **DDP reference fixtures: no line-ending conversion**

  <details><summary>Details</summary>

  The Windows runner checked the reference filesets out with CRLF, so the
  byte-for-byte comparison of CHECKSUM.MD5/.TXT failed although the writer's
  output was right. The fixtures are now binary to git.

  </details>

- **Mastering: DDP CD masters, ISRC/UPC and credits, crossfades, song inserts**

  <details><summary>Details</summary>

  Roadmap item 4.

  - faderframe-disc (new, no dependencies besides thiserror): the Red Book
    model and its rules (track 1 pregap, 4 s tracks, 99 tracks, 99:59:74),
    ISRC and UPC/EAN normalisation with the EAN check digit, CD-Text packs
    (CRC-16, size information), cue sheets, and a DDP 2.00 writer/reader:
    IMAGE.DAT streamed and hashed, DDPID, DDPMS, the PQ descriptor,
    CDTEXT.BIN, CHECKSUM.MD5/.TXT (MD5 and CRC-32 built in). The tests
    compare every descriptor file byte for byte with reference filesets of
    the ddp-reverse-eng project (MIT, credited in tests/reference), and the
    image by checksum.
  - The album: release info (title, credits, UPC/EAN), per song ISRC,
    credits, a crossfade from the previous song and its own inserts; codes
    are checked when entered.
  - Export: session::album_master assembles the album stream (track marks
    on CD frames, equal-power crossfades) into the album file, a cue sheet
    with CATALOG/ISRC/FLAGS/CD-Text, a CD master (resampled to 44.1 kHz,
    dithered to 16 bits, after the 2 s pregap; read back and verified) and,
    with crossfades, gapless song files cut at the marks. Tracks shorter
    than 4 s are refused before writing; masters over 79:57 are flagged.
  - Song inserts: Command::SetSongInserts; the plugin commands find them
    when they name the master, so editors, generic windows, presets and
    programs work; the engine hosts them and runs the monitored song's
    after the master strip (AlbumAction::Monitor). Analyse/Export run them
    offline (render::process_through: latency compensated, tail kept).
  - Album view: ISRC and Inserts columns (menu: add via the plugin browser,
    edit, bypass, reorder, remove, hear on the master), crossfades in the
    Pause column (×), Release… (a details form, also per song) and a CD
    master menu (DDP, CD-Text, digital copy permitted).
  - Dev actions album:ddp|details|crossfade=<s>@<n>|monitor=<n>|
    insert=<n>:<plugin id>; docs and README.

  </details>

- **Tests: tolerate the dummy device catching up on busy CI machines**

  <details><summary>Details</summary>

  The macOS Intel Ports runner failed two timing tests after 60b8106. The
  dummy backend catches up after oversleeping (up to 500 ms of audio at
  once), which brings looped SysEx passes closer on the wall clock and lets
  the audio thread outrun the anticipator by a block or two:

  - looped SysEx: a two-second loop, passes must be over a second apart
    (a duplicate in one pass still fails);
  - render ahead: at most 4 late blocks (a stale or mis-keyed ring misses
    every block, ~60 there).

  faderframe-stretch declares its `ff_cpp_count` cfg before the
  check-only early return, so cross-clippy with -D warnings passes.

  </details>

- **Render ahead: new rings get new readers; a track's start is not "late"**

  <details><summary>Details</summary>

  Changing the lookahead while stopped gave every rendered-ahead track a
  new ring, but its reader in the realtime graph kept its node key, so the
  graph adopted the old reader, which went on reading the old ring while
  the new anticipator filled the new one: those tracks fell silent. The
  ring's identity is now part of the reader's and writer's node keys. New
  test: change the lookahead while stopped, then play — identical to
  rendering on the audio thread (with the old keys 95 743 of 95 744 samples
  differed).

  A reader also counts missing audio as late only once it has played some:
  a track that comes back to rendering ahead when playback stops has no
  audio until the anticipator gets to it, which a slow CI machine counted
  as two late blocks (render_ahead session test, macOS Intel runner).

  </details>

- **Render ahead: anticipative processing of tracks nobody plays live**

  <details><summary>Details</summary>

  Tracks that play from the timeline alone — audio and instrument tracks
  with plugins, not frozen or armed, no input monitoring, live MIDI,
  external MIDI output, sidechain input or pre-FX send, not fed by another
  track and without an open plugin editor — have their sources, instrument
  and inserts rendered ahead of the playhead (200 ms by default;
  Preferences → Audio → Render ahead) on a thread of their own, in the
  engine's block size and in parallel on a pool of its own. The realtime
  graph keeps a reader with the chain's latency in front of each such
  track's strip, so faders, pan, mute, solo and sends stay immediate, and
  everything live stays on the audio thread.

  * The anticipator runs its own copy of the transport, so it renders
    exactly the positions the audio thread will ask for, loop wraps and
    scrub snippets included. Commands that move the playhead start a new
    sequence: the audio thread hands the commanded state over (wait-free)
    and switches once two blocks of it are rendered (at most 250 ms later)
    — play, stop and locate without a gap. Commands that change nothing
    (the loop range sent with every timeline update) start none.
  * Per-track rings tag audio with (sequence, position, playing); the
    reader skips stale audio, waits for missing audio (silence, counted as
    late) and never reads a sequence it has not switched to.
  * A track that becomes live (armed, selected for live play, editor
    opened) moves to the audio thread at once; one that stops being live
    stays there until playback stops, then the session rebuilds.
  * The timeline snapshot is an Arc shared by both threads; both return
    their old ones to the control thread.

  faderframe-bench --ahead: 64 tracks × 6 echoes at 64-frame buffers on 4
  threads (paced): audio-thread callbacks mean 187 → 34 µs, p99 635 →
  50 µs, max 1345 → 81 µs, deadline misses 1 → 0, no late blocks.
  Tests: output identical sample for sample with and without it through
  playback, a locate and loop wraps (engine/tests/ahead.rs, also under
  Wine as a Windows build), nothing allocated on the audio thread, and a
  session test (arming moves a track, a stop brings it back). Dev action
  log-ahead:x.

  </details>

- **macOS: DSP workers join the audio device's workgroup**

  <details><summary>Details</summary>

  CoreAudio's IO thread belongs to its device's os_workgroup; threads that
  do part of its work should join it so the scheduler treats them as one
  realtime workload with the IO thread's deadline (on Apple silicon:
  performance cores, no throttling mid-cycle). The cpal backend now finds
  the device's IOThreadOSWorkgroup (cpal's device id is the CoreAudio UID,
  translated to the device) and reports it with the stream
  (`AudioStream::io_workgroup`); the session hands it to the worker pool
  (`WorkerPool::set_workgroup`, withdrawn when audio stops), whose workers
  join before their next job and leave when it changes or they end — never
  blocking (a try-lock, tried again next time). `faderframe_realtime::
  Workgroup` wraps the retained os_workgroup_t (CoreAudio, AudioToolbox and
  os_workgroup calls; nothing to make on other systems).

  A macOS test creates a work interval of its own (CI machines may have no
  audio device), sees both workers join and leave it, and joins the default
  output device's workgroup where there is one.

  </details>

- **Plugins: VST3 program lists, 64-bit processing, SysEx to plugins**

  <details><summary>Details</summary>

  Program lists. A VST3 plugin's program-change parameter (kIsProgramChange,
  often hidden) names its programs through its unit's program list
  (IUnitInfo), else through the parameter's own value texts
  (`PluginInstance::programs`/`current_program`/`select_program`/
  `changes_pending`, also across the sandbox). The insert menu and the
  parameter window's Presets menu list them. Selecting one changes the
  plugin's whole state, so `session::programs` makes it one undo step: the
  state from before is captured, the program selected, and once the
  processor has taken it the new state is recorded as "Select Program"
  (the program itself is not stored, so loading a project never overwrites
  later tweaks). Lists of numbered slots only (u-he, Arturia: "Program 0",
  "ProgramChange 1"…) are MIDI program change targets and not shown; MIDI
  program changes now select VST3 programs (they were dropped).

  64-bit processing. `ProcessConfig::double_precision` (Preferences →
  Audio, applied at once by reactivating the plugins): VST3 plugins that
  offer kSample64 and CLAP plugins whose ports support 64 bits get double
  buffers, converted around the plugin from preallocated storage; the graph
  stays 32-bit. (None of the plugins installed here offers it; the test
  plugins of both formats do.)

  SysEx to plugins. MidiBuffer gets a preallocated byte area (16 KiB per
  block) and `MidiEvent::SysEx(SysexRef)` refers into it, tagged with the
  buffer's id: `push_sysex` copies bytes in, `push_from`/`merge_from` copy
  events with their bytes, a plain `push` of another buffer's SysEx is
  dropped, never misread. Clip SysEx is in the snapshot and played by the
  MIDI clip player; live SysEx from an input port goes from the control
  side through a byte ring to the tracks playing live from that port.
  CLAP gets CLAP_EVENT_MIDI_SYSEX, VST3 a kMidiSysEx data event, Audio Units
  MusicDeviceSysEx, sandboxed plugins the bytes in their shared block; MIDI
  outputs keep getting clip SysEx from the control side. The realtime
  allocation tests cover clip and live SysEx.

  </details>

- **Sandbox: platform-independent pipe test, block size checked first**

  <details><summary>Details</summary>

  macOS reports a pipe whose writer closed as readable (at its end), Linux
  as hung up; the unit test now checks the hang-up through `Waiter`, which
  reports it the same way on both. `Block::open` checks that the mapping
  holds a header before reading one.

  </details>

- **SysEx playback: no repeats at window seams or in loops**

  <details><summary>Details</summary>

  Two bugs behind the SysEx test failing on the macOS runners:

  * The engine published "callback start time" and "transport position
    then" as two separate relaxed atomics; a reader between the two stores
    got a pair one callback apart, so `position_at` jumped. It is now a
    sequence lock over atomics (`CallbackStamp`; wait-free for the audio
    thread), so the pair is consistent.
  * The SysEx scheduler mapped each 100 ms window of *time* to timeline
    positions with that tick's position estimate; estimates wobble between
    ticks, so windows could overlap (a message sent twice) or leave gaps.
    Each window now continues in the timeline where the last one ended.
    The loop handling also scheduled a whole loop body at once when a
    window started past the loop end, sending a looped message several
    times per pass; windows are now split into timeline segments
    generically (several wraps for short loops), wrapping only while the
    playhead is before the loop end.

  New test: a one-second loop around a message sends it once per pass
  (the old scheduler sent it three times in the third pass).

  </details>

- **Sandboxed plugins on Windows and macOS**

  <details><summary>Details</summary>

  The sandbox's operating-system part is now one interface (`sys`) with a
  Unix and a Windows implementation, and sandboxing is available — and on
  by default — on Linux, Windows and macOS:

  * Windows: control over a single-instance, local-only named pipe with
    overlapped I/O (every wait has a deadline and also watches the helper's
    process, so a death is noticed at once), auto-reset events for the
    per-block wake-ups, a pagefile-backed file mapping for the audio; the
    helper finds them by name in its environment, copies FaderFrame's DPI
    awareness, initialises OLE, suppresses crash dialogs and ends itself
    when FaderFrame's process does. Its main thread runs a message loop
    (`MsgWaitForMultipleObjectsEx`) woken by a thread reading the pipe.
    Editors embed into FaderFrame's windows across processes; because a
    child window sends its parent messages synchronously (creation,
    resizing, destruction), FaderFrame handles sent messages while it waits
    for an answer — without that, attaching an editor deadlocked until the
    timeout (tests/editor_windows.rs, which fails in 60 s without it).
  * macOS: the Unix transport, plus AppKit: views cannot cross processes,
    so the helper reports a floating editor and shows it in an NSWindow of
    its own (accessory app, no Dock icon), handles resizes and the close
    button itself and reports closing with the poll; opening the editor
    again raises it (new `PluginEditor::raise`). The helper's poll loop
    hands AppKit its events and run loop at least every 10 ms while an
    editor is open (50 ms otherwise). Audio Units are sandboxed too.
  * Unix: Linux helpers also get PR_SET_PDEATHSIG, and every Unix helper
    exits when it is reparented, even with a plugin stuck on its main
    thread.

  The sandbox tests now run on every platform. Under Wine the Windows
  build passes them all (bit-identical audio, a dying and a hanging
  plugin, the cross-process editor) at 7–8 µs per round trip; Linux is
  unchanged (live: Presswerk killed mid-playback and reloaded).

  </details>

- **Sandboxed plugins: each CLAP/VST3 instance in a process of its own (Linux)**

  <details><summary>Details</summary>

  A plugin that crashes or hangs now costs only itself. With sandboxing on
  (Preferences → General, the default on Linux) every CLAP and VST3
  instance runs in a helper process — FaderFrame itself started as
  `faderframe --plugin-sandbox`, hosting the plugin with the usual
  in-process factories — behind proxy implementations of PluginInstance and
  PluginProcessor (new crate faderframe-plugin-sandbox):

  * control over a Unix socket (framed JSON with a binary payload, size
    limits), with parameter values, editor requests and edits cached from a
    per-tick poll that sends only changes;
  * audio through a shared-memory block per activation, MIDI, note
    expressions, parameter events and transport as fixed-layout records,
    one wake-up byte through a pipe each way; the helper's audio thread
    copies the host audio thread's scheduling; everything the helper writes
    is validated (counts, event kinds, finite samples); about 9 µs per
    plugin and block;
  * a helper that dies is noticed at once (its pipe closes), one that hangs
    after 250 ms; the engine bypasses the plugin, the session reports it
    once, Reload Plugin (insert and instrument menus) builds it again from
    its slot, and sandboxed plugins' changed state is kept in their slots
    every 5 s so a reload restores recent settings;
  * editors run in the helper and embed into FaderFrame's X11 parent
    windows across processes.

  Verified with u-he Presswerk (CLAP and VST3, editors embedded, killed
  mid-playback and reloaded), Diva (notes and note expressions identical to
  in process) and PultEQFx (VST3). The crate's tests start the test binary
  itself as the helper: sandboxed built-ins match in-process ones bit for
  bit, a dying and a hanging plugin are handled.

  </details>

- **SysEx stop test: stop once the message is scheduled, not by the clock**

  <details><summary>Details</summary>

  The test stopped playback 50 ms before a SysEx was due and expected the
  already scheduled message to be cancelled; a busy macOS runner overshot
  the 50 ms. It now waits until the message has been handed to the output
  sender (100 ms ahead of its time) and stops then
  (`Session::sysex_scheduled`, from `MidiOutputs::sysex_scheduled`).

  </details>

- **SysEx test: wait for the message instead of a fixed 800 ms**

  <details><summary>Details</summary>

  The test expected a SysEx due half a second into playback to have gone
  out within 800 ms of wall-clock time; the Intel macOS runner lagged. It
  still checks nothing goes out before its beat, and now waits up to 3 s
  for it (and for the repeat after the locate).

  </details>

- **Analysis test: wait for the new source's silence instead of 0.6 s**

  <details><summary>Details</summary>

  Switching the meters to the silent Pad track, the test allowed 0.6 s for
  the 400 ms momentary window to fill with the new source; a slow macOS
  runner delivered fewer blocks than that. It now waits up to 4 s (the pad
  enters only at 8.6 s).

  </details>

- **Page table: sequentially consistent reclamation; VST3 test off the lock**

  <details><summary>Details</summary>

  The disk-streaming page table frees an evicted page once the audio
  thread has completed a cycle since the eviction. With acquire/release
  alone the loader's read of that cycle count may be stale, so a page the
  reader loaded in its current cycle could be freed one cycle early while
  it is read — the torn pages the concurrent test caught on Apple Silicon.
  The reader's slot load and cycle increment and the loader's unlinking
  swap and count read are now sequentially consistent (loads cost the same
  as acquire loads on x86 and ARMv8).

  The VST3 host test recorded events through a mutex whose first lock
  allocates on macOS, inside the allocation-counting window; it now locks
  it only after setting it up.

  </details>

- **realtime_alloc tests take turns**

  <details><summary>Details</summary>

  The allocation counter and the armed flag of the DSP workers are
  process-wide, so a test running in parallel could count another test's
  allocations (a worker pool shutting down while it was armed) — which CI
  hit after the demo melody gained note expressions and the timing shifted.
  The tests of the file now hold a shared lock.

  </details>

- **Native per-note expression for CLAP, VST3 and the built-in synth**

  <details><summary>Details</summary>

  Per-note expression grows from pitch, pressure and timbre to the seven
  dimensions CLAP and VST3 share (volume, pan, vibrato and expression added).
  On tracks without MPE the timeline snapshot now sends every curve to the
  instrument natively as MidiEvent::NoteExpression (fixed-point, ordered
  after its note-on, sampled every 1/128 quarter at a per-kind resolution):

  * CLAP: note-ons and note-offs carry note ids, expressions go out as
    CLAP_EVENT_NOTE_EXPRESSION addressed by key and channel (id -1) — u-he
    Diva honours only those (verified: tuning, volume and pan act on it).
  * VST3: note ids on note events; NoteExpressionValueEvents for the types
    the plugin lists through INoteExpressionController (normalised the VST3
    way), pressure as PolyPressureEvent. PluginInstance::note_expressions
    reports what an instrument accepts.
  * The built-in synth applies all seven per voice.
  * MIDI outputs drop them; MPE tracks keep playing pitch, pressure and
    timbre on member channels.

  The piano roll offers the new lanes (dB, pan and value read-outs, zero
  lines), and tells when the instrument does not take the lane's kind or a
  MIDI output needs MPE. Dev action expression-demo.

  </details>

- **Quantize and Humanize for audio transients and MIDI clips alike**

  <details><summary>Details</summary>

  Quantize (Q in the arranger) and Humanize work on the selected clips in
  one undo step: MIDI clips through their notes, audio clips by pinning
  every detected transient with a warp marker where it should play.
  Quantize has strength and swing (and note ends for MIDI), Humanize a
  timing range in milliseconds (converted at each clip's tempo for notes)
  and a velocity range. The settings are shared between the arranger (the
  edit toolbar's new Quantize menu, the clip context menu) and the piano
  roll (its quantize settings menu, which also humanizes the selection);
  the piano roll's Humanize no longer uses fixed amounts. Transient
  quantize now honours strength and swing too.

  </details>

- **Album: songs in release order, analysed and exported for delivery**

  <details><summary>Details</summary>

  The album (saved with the project, edited through Command::SetAlbum) lists
  songs — sections of the project, the whole project, other FaderFrame
  projects or finished mixes — with a pause, trim and fades each, and the
  delivery settings. Analyse renders or decodes every song on a worker and
  measures it (integrated loudness, range, true peak); the view previews
  how each song will be delivered. Export levels the songs with one gain
  for the album (keeping their balance) or per song, limits the true peak,
  dithers, writes "NN Title.wav" and optionally the whole album as one file
  with a CUE sheet (pauses as pregaps). A section renders without the clips
  that start after it, so the next song does not ring into its tail.

  The Album view sits in the bottom dock next to Tools (View → Album); it
  adds songs, sets levelling, target, ceiling, format, rate and dither
  (delivery presets), reorders by drag, renames, drags or types trims,
  pauses and fades. Views can now ask the host for file and folder
  choosers (HostRequest::ChooseFiles).

  </details>

- **Render window: the Cancel button closes it, or stops a running render**

  <details><summary>Details</summary>

  The button was disabled whenever no render was running, so it seemed to
  do nothing. It is now "Close" while idle and "Stop" during a render.

  </details>

- **Delivery on export: loudness normalisation, true-peak limiting, shaped dither**

  <details><summary>Details</summary>

  Renders can be brought to an integrated loudness target (BS.1770) with the
  true peak held under a ceiling by a lookahead limiter (min-hold plus box
  filter, smooth 1.5 ms attack, 60 ms release, extra passes for inter-sample
  overshoots; the gain is refined until the limited result meets the
  target), or only limited, or peak-normalised as before. Dither is a choice
  of off, TPDF or noise-shaped TPDF (Lipshitz E-weighted error feedback at
  44.1/48 kHz). Every written file is measured: the Render window lists
  loudness, range, true peak, gain and limiting per file and offers delivery
  presets (streaming, Apple Music, CD, EBU R128, ATSC A/85, loud master).

  </details>

- **Test that a plugin's own parameter change is not overwritten**

  <details><summary>Details</summary>

  A value the plugin changes itself (its editor's bypass button, say) must
  survive later syncs and graph rebuilds even when the slot holds an older
  explicit value, and saving takes the plugin's value over. Verified by hand
  with u-he Presswerk (CLAP): the editor's bypass passes the input through,
  with and without HQ (whose latency change restarts the plugin).

  </details>

- **Master strip at the window's side, per workspace (on in Mastering)**

  <details><summary>Details</summary>

  A workspace layout can show the master strip at the main window's right
  edge, full height between the toolbars and the status bar, whatever view
  is shown (View → Master Strip at the Side; WorkspaceAction::
  ToggleMasterPanel). The Mastering preset has it on. The panel is a
  master-only MixerView; the dock's mixer leaves its pinned master out
  while the panel is up.

  </details>

- **Arranger: track headers run down to the bottom, scrollbars over the lanes**

  <details><summary>Details</summary>

  The arranger's scrollbars now lie over the canvas (ViewHost::overlaid)
  instead of in rows beside it, inset to the scrolled area and hidden when
  there is nothing to scroll: the track-header column runs down to the
  status bar and the ruler and global lanes reach the right edge.

  </details>

- **Scrollbars span only the scrolled part of a view**

  <details><summary>Details</summary>

  ScrollInfo carries where the scrolled area begins and ends; the host sets
  the scrollbar's margins from it. The arranger's horizontal scrollbar now
  starts right of the track headers and its vertical one below the ruler and
  global lanes; the mixer's spans the strips (not the cheek or the pinned
  master), the piano roll's its grid, the performance table's its rows.

  </details>

- **Automation tab: every lane at a glance and a full-width lane editor**

  <details><summary>Details</summary>

  The bottom dock's Automation tab was a placeholder. It now hosts
  faderframe-view-automation: a list of every automatable track with its
  lanes (mode, point count, arranger visibility, "+" to add a lane,
  Selected Tracks filter, All Read / All Off) and a large editor for the
  selected lane over the whole song (value scale, bar ruler, loop, edit
  selection, playhead, zoom): add/drag/delete points, curve shapes, Write
  Value at the playhead or over the edit selection, Thin, Clear, Delete
  Lane. New session action SetAutomationModes (one undo step).

  </details>

- **Portable mode on every platform; portable Linux tarball**

  <details><summary>Details</summary>

  A "FaderFrame Data" folder next to the program (or FADERFRAME_DATA_DIR)
  makes FaderFrame keep its settings, plugin caches, presets and unsaved
  recordings in there, and load CLAP/VST3 plugins from its Plug-Ins folder
  (faderframe_core::paths). Preferences → General shows where data goes.

  Linux: packaging/linux/tarball.sh builds a tarball with GTK and its
  libraries bundled (not the C runtime, graphics drivers, display/audio
  server clients or fonts), a launcher that runs it in place, and install.sh
  for a per-user or system-wide installation (--uninstall). The Windows
  bundle builds the installer without and the portable zip with the data
  folder. The Release workflow builds the tarball on Ubuntu 24.04.

  VST3 also searches the standard folders on Windows and macOS (it only
  knew the Linux ones).

  </details>

- **Vintage Console: legible plasma bar-graph meters**

  <details><summary>Details</summary>

  The edgewise VU needles of the Vintage Console skin did not read at the
  size of mixer strips and track headers: every meter looked like a static
  cream face. The skin now uses a new MeterKind::Plasma — a glowing
  neon-orange column up to the level with fine cell lines, red above
  -2 dBFS, a dim unlit column and a peak-hold mark. The theme gallery is
  updated.

  </details>

- **Move, copy and delete song sections with their content**

  <details><summary>Details</summary>

  faderframe_project::arrange copies a time span with everything in it,
  cuts a span out and opens one: clips are split at the edges, automation
  keeps its values on both sides of a cut, tempo points and time-signature
  changes (whole bars), markers, sections and the loop/punch ranges move
  along. The session applies the result as one Command::SetArrangement.

  In the arranger lane, dragging a section moves it with its content (the
  song closes up and makes room; a line shows where it goes), Ctrl-drag
  inserts a copy, Shift-drag moves only the section. The section menu adds
  Move Earlier/Later, Duplicate and Delete with Content. Dev actions:
  move-section / copy-section.

  </details>

- **DSP workers at realtime priority on Windows and macOS**

  <details><summary>Details</summary>

  Workers now match the audio thread's urgency on the ports too: on macOS
  they copy the CoreAudio IO thread's Mach time-constraint policy, on Windows
  they join MMCSS's "Pro Audio" task (as cpal does for the WASAPI thread).
  With that, the pool uses SMT siblings there as it does under SCHED_FIFO on
  Linux.

  </details>

[All changes since 0.1.0](https://github.com/BurningTreeC/faderframe/compare/v0.1.0...v0.2.0)

## [0.1.0](https://github.com/BurningTreeC/faderframe/releases/tag/v0.1.0) — 2026-10-04

- **Fix the macOS app's executable; robust timing in the remaining port tests**

  <details><summary>Details</summary>

  macOS file systems ignore case, so the launcher `FaderFrame` overwrote the
  program `faderframe` in the app bundle: the program is now
  `faderframe-bin`, and bundle_dylibs.py refuses inputs that are not Mach-O.
  The Windows bundle strips the executable.

  Tests: the analysis test checks the spectrum's held peaks above 20 Hz (the
  loudest bin of the last frame depends on the drum loop's phase); the AU
  lowpass test checks that the automation arrives, not Apple's filter slope;
  the audition release may take longer where the device clock lags; SysEx
  cancellation stops by transport position and the MIDI output test judges
  by send times, not by sleeping.

  </details>

- **macOS bundle: own library bundler instead of dylibbundler**

  <details><summary>Details</summary>

  dylibbundler could not resolve the SVG image loader's @rpath reference to
  librsvg and kept prompting on stdin, hanging the release job. The new
  packaging/macos/bundle_dylibs.py copies every non-system library
  recursively, resolves @rpath/@loader_path/@executable_path like dyld (from
  each file's real location) and fails instead of prompting. macOS links get
  -headerpad_max_install_names so the rewritten names fit.

  </details>

- **Portable media paths, analysis source switch, test timing on the ports**

  <details><summary>Details</summary>

  Project files store media paths relative to the project with '/' on every
  system; paths saved on Windows with '\' still resolve elsewhere.

  Switching the analysed track no longer lets a block of the previous source
  into the freshly reset meters: the scope ring records which source wrote
  its newest frames and where that run began, and the analyser reads only
  the analysed source's run.

  The dummy device catches up after oversleeping (up to 500 ms) instead of
  dropping audio time, so its clock keeps pace with the wall clock on busy
  machines.

  Tests: the AU automation test uses a zero-mean tone continuous across
  blocks (the repeated block had a DC offset that passes a lowpass);
  automation and audition tests no longer depend on sleep precision; the
  recent-projects test expects absolute Windows paths; the real-time MIDI
  clock/MTC tests run on Linux only (the CI VMs oversleep by tens of ms).

  </details>

- **Skins, native plugin editors on Windows/macOS, ASIO, Audio Units, packaging**

  <details><summary>Details</summary>

  Themes: seven skins switched live (View → Theme, Preferences → General):
  Studio, Vintage Console (walnut cheeks, enamel panels, skirted knobs,
  edgewise VU meters), Daylight (light), Midnight, Frost, Neon and High
  Contrast. A theme's ConsoleLook covers knob style, flat or glossy
  controls, brushed panels, screws, wood cheeks and the meter kind. GTK's
  stylesheet is generated from the theme and the Adwaita variant follows
  it; LCD colours and editor overlays come from the theme.

  Plugin editors: the editor trait is platform-neutral (WindowApi,
  ParentWindow); CLAP and VST3 GUIs embed into X11 (XWayland), Win32 and
  Cocoa parent windows from three backends behind one Parents API.

  Audio: ASIO through cpal behind the opt-in `asio` feature (the ASIO SDK's
  licence then applies), checked on Windows MSVC in the Ports workflow.

  Audio Units (macOS): new faderframe-plugin-au crate on the AUv2 C API —
  registry listing, parameters and sample-accurate automation, MIDI, state
  and .aupreset files, latency/tail, Cocoa or generic editor views, editor
  automation; tests against Apple's built-in units.

  Packaging: app icon, desktop entry, AppStream metadata and MIME type,
  Flatpak manifest (GNOME 51), macOS app bundle and DMG, Windows bundle with
  Inno Setup installer and zip, and a Release workflow for v* tags. Windows
  release builds use the GUI subsystem.

  Ports CI fixes: no C++ allocation counter in Windows test links, a
  multicore timing test tolerant of slow VMs, a MIDI output test judged by
  send times.

  </details>

- **MIDI files, plugin presets and editor automation, mixing features, native PipeWire, Windows/macOS ports, mastering tools**

  <details><summary>Details</summary>

  - Standard MIDI File import and export (midly)
  - Automation written from plugin editors (CLAP, VST3); user presets
    (.ffpreset) and VST3 factory presets; inserts drag to reorder or copy,
    Alt-click removes
  - Sidechain inputs (built-in compressor with a key input), track groups,
    VCAs, relative edits of every selected track, freeze and bounce in place
  - Scrubbing plays audio; the Pencil redraws samples at sample-level zoom
    (non-destructive); Follow button
  - Native PipeWire backend (pw_filter), system backend through cpal
    (WASAPI, CoreAudio, ALSA); Windows and macOS builds (platform-gated
    JACK/PipeWire, posix-fd and VST3 module loading) and a Ports CI
    workflow for macOS (Apple Silicon, Intel) and Windows
  - Tools view for mastering: EBU R128 loudness and true peak, levels with
    K-System scales, phase, spectrum
  - Arranger global lanes: markers, song sections, time signatures, tempo;
    track colour chooser
  - ARCHITECTURE and README updated, README screenshots

  </details>

- **Start-up project choice and File → Open Recent**

  <details><summary>Details</summary>

  Preferences → General → On start-up: open the last project (the demo
  session on a first start; a new project with a warning when the file is
  gone), a new empty project, or the demo session. A project path, --empty
  or the new --demo flag override it for one start.

  File → Open Recent lists the last ten projects (recorded whenever the
  session's file changes: open, save as, start-up), asks about unsaved
  changes, drops projects that no longer exist, and can be cleared there or
  in Preferences.

  </details>

- **Record button, take redraw, plugin restarts, instruments, clip gain knobs, resizable mixer inserts**

  <details><summary>Details</summary>

  Recording: the record button records and plays when stopped, punches in
  while playing and out while recording; with nothing armed a warning pops
  up. Warnings and errors now pop up as a toast at the top of the window.
  Takes placed after stopping appeared only on the next user action: the UI
  tick now redraws whenever the session changed during it (also for finished
  imports and analyses).

  Plugins: a plugin restart (e.g. requested when its editor opens) left the
  track silent, because the rebuilt graph adopted the old, deactivated
  processor by node key. Plugin node keys now include the instance's
  activation count (PluginInstance::activation, CLAP and VST3).

  Instruments: new instrument tracks start without the built-in synth and
  open the plugin browser to choose one; an instrument picked for an empty
  instrument track's insert slot becomes its instrument; inserts that take
  notes get the track's MIDI (clips and live input).

  Clips: a gain knob in every audio clip's name strip (drag, wheel,
  double-click resets, Alt-click types), with a taller name strip.

  Mixer: the inserts section shows five slots by default; the rule below it
  is a grip that shows more or fewer slots (saved with the layout), and
  strips with more plugins than slots show "+N more".

  Opt-in tests render installed plugins through the engine and a live
  session (FADERFRAME_TEST_BUNDLES).

  </details>

- **Avoid AtomicUsize::fetch_update (deprecated in Rust 1.99)**

  <details><summary>Details</summary>

  The worker seat count is taken with a compare-exchange loop, which works
  on every supported toolchain.

  </details>

- **VST3 hosting, multicore engine, MIDI sync/MPE/SysEx, Pro-style editing and elastic audio**

  <details><summary>Details</summary>

  VST3 hosting (faderframe-plugin-vst3): module loading, crash-safe scan helper
  with a shared scan cache, combined or separate controllers, sample-accurate
  parameters and automation, notes and MIDI-mapped controllers, state, and
  embedded editors with run-loop servicing.

  Multicore engine: fused node chains scheduled lock-free on a realtime
  worker pool (futex wake-up, SCHED_FIFO inheritance, flush-to-zero), ranked
  by measured critical path; output stays bit-identical to serial processing.
  Processing threads are configurable in Preferences.

  MIDI: following MIDI clock and MTC (tempo fit, drop-frame, chase and
  re-lock), per-note MPE expression (model, playback on member channels,
  recording, piano roll lanes, MPE in the built-in synth) and SysEx (recorded,
  scheduled to devices with cancellation, .syx import/send).

  Editing, following Pro Tools: an edit toolbar under the transport (wraps
  into rows) with edit modes (Shuffle, Slip, Spot, Grid absolute/relative),
  tools (Smart, Zoom, Trim, Time-Stretch Trim, Selector, Grabber, Separation
  Grabber, Scrubber, Pencil), grids down to 1/256 with triplet/dotted, nudge,
  Tab to transients, Link Timeline, Insertion Follows Playback, selection
  counters and zoom. Clicking a clip selects it and moves the playhead to the
  snapped click. Edit-selection ranges with separate, trim, clear,
  copy/cut/paste, duplicate, repeat and insert silence. Shift/Ctrl-click
  selects several clips; moves, trims, fades, gain and mute apply to all of
  them. Clip gain, fades with five shapes and drawn curve bends.

  Transient detection (spectral flux, cached .fftr) and elastic audio: warp
  markers, transient and range warping, telescoping, quantizing to the grid,
  separating at transients, time-compression trims. Warped clips play
  pitch-preserving through faderframe-stretch (vendored Signalsmith Stretch,
  MIT, C shim; C++ allocations counted in tests) or as varispeed; stretcher
  voices are preallocated per track and primed sample-accurately.

  Transport: tap tempo, editable time signatures (new SetTimeSignature
  command), metronome button (K). Mixer pan and level readouts open a text
  field on click. Generated sources added by an edit are now rendered.

  </details>

- **Piano roll, MIDI output and clock, controller lanes**

  <details><summary>Details</summary>

  Piano roll (rewritten)
  - Tools (select, draw, erase, split, mute), rubber band, previewed drags
    committed as one action (Alt copies, Shift skips snap), resize from both
    edges, auto-scroll, copy/cut/paste/duplicate with session-allocated ids.
  - Grid incl. triplets, note length (grid, last, fixed), scales with
    highlighting, snap and folding, fixed and scale-aware chords, quantize
    (strength, swing, ends), humanize, legato, reverse, invert.
  - Velocity stems and ramps, controller lanes per controller and channel
    (freehand, lines, erase), ghost notes, auditioning, step input from a
    MIDI keyboard, clip-length handle, inspector, live keys, zoom, follow.
  - Pure note operations, scales and chords in faderframe_project::midi_ops;
    session note actions in faderframe_session::notes.

  MIDI
  - Controller lanes in clips: recorded, played, chased on locate, reset on
    stop, carried across splits; muted notes.
  - MIDI output to external devices (Track::midi_output, EventOutput graph
    role, scheduled sender thread) and MIDI clock with start/stop/continue
    and song position.
  - Mapping modes: soft takeover and relative encoders; mapped controls are
    consumed (not passed to instruments or recordings).
  - Auditioning port, step input, held-key display.
  - Built-in synth: sustain pedal, pitch bend, mod-wheel vibrato.
  - Preferences: MIDI outputs with clock; GTK measuring warning fixed.

  </details>

- **Performance meter, MIDI keyboards and controllers**

  <details><summary>Details</summary>

  Performance meter
  - Graph nodes are timed per device callback (summed over chunks, one
    clock read per node, single-writer publishing in finish_cycle) with
    per-track groups for exact track peaks; node owners (track, plugin,
    kind of work) come from build_graph. Node timing runs only while the
    meter is read; the total DSP load is always measured.
  - session::performance: loads as shares of the callback budget, smoothed
    averages, held peaks, 60 s history, per track and per plugin instance.
  - Performance view (F8, dockable/detachable, status-bar DSP readout):
    total with history and breakdown, sortable track/plugin table,
    editor and bypass from plugin rows.

  MIDI keyboards and controllers
  - faderframe-midi-io: ALSA sequencer inputs via midir with hotplug and
    stable port identity, plus virtual inputs.
  - Timestamped input queue to the audio thread (constant one-block
    latency) and a control-thread copy (learn, mappings, activity).
  - Engine: live MIDI input node per instrument/MIDI track (port/channel
    filter, note-offs when no longer live), MIDI recorder ring.
  - Session: live rule (Input, or Auto when armed/selected), MIDI takes
    into bar-aligned clips (takes/replace, loop passes), MIDI learn for
    every automatable parameter and transport functions, mapped controls as
    gestures (one undo step, automation writing).
  - Project: InputRouting::Midi, controller mappings with undoable
    commands, file format v2 migrating instrument tracks to MIDI input.
  - UI: MIDI In / Play Live menus, MIDI learn in mixer, arranger, lanes and
    the generic plugin editor, Preferences → MIDI, status-bar activity,
    live notes while recording.
  - CI installs libasound2-dev.

  </details>

- **CLAP hosting, plugin browser and plugin editors**

  <details><summary>Details</summary>

  - faderframe-plugin-clap: CLAP host on clack-host. Bundles are scanned in a
    helper process (`faderframe --scan-clap`) with a size/mtime cache;
    instances live on the main thread, processors are shared across graph
    generations through the wait-free TryCell. Parameters, automation events,
    notes, transport, latency, restarts and state are supported; extensions
    are queried after init() (fixes yabridge plugins).
  - Plugin editors: native GUIs embedded into X11 parent windows via
    XWayland, opened centred on FaderFrame's monitor (RandR geometry) or at
    their last position, saved per instance with the project; posix-fd and
    timer extensions serviced on the GTK main loop; floating fallback.
  - Generic parameter window for every plugin (built-ins included) with
    module sections, plugin value text, gesture undo and live follow.
  - Command::SetPluginParameter with baseline restore on undo; explicit
    values follow the plugin GUI before saving.
  - Plugin browser window: search, effects/instruments, format and vendor
    filters, details, one-click insertion; mixer and arranger entry points.
  - Resizable arranger header column, input source menus, consistent percent
    formatting, dev actions (insert-plugin, show-insert, quit-discard) and
    screenshots of all app windows and plugin editors.

  </details>

- **Import, disk streaming, recording, takes, automation and more**

  <details><summary>Details</summary>

  Media
  - Audio import (Symphonia: WAV, AIFF, CAF, FLAC, MP3, Ogg, AAC/M4A,
    ALAC) with rubato resampling to the project rate, float32 media files
    and waveform peak caches; File > Import Audio, drag & drop, --import.
  - Lock-free disk streaming: page tables with epoch-based reclamation,
    a disk loader thread, bounded memory; offline renders load
    synchronously. Media lives in <project>/Audio (scratch folder until
    the first save); missing files show as offline clips.

  Recording
  - Lock-free capture rings per recording, a writer thread, latency
    compensation, punch in/out, pre-roll, metronome, mono or stereo
    inputs, live waveforms while recording.
  - Take folders with comping (click a lane to use a take, swipe to comp
    a section, flatten); selectable record mode (takes / replace) and
    loop recording mode (takes / last pass / new track per pass).

  Automation
  - Lanes for every automatable parameter (volume, pan, mute, sends,
    plugin parameters and bypass), sample-accurate playback, plugin
    parameter events, latency-aligned soft bypass, Read/Touch/Latch/Write
    writing with thinning, lane editing in the arranger.

  Mixer and arranger
  - Any number of sends per strip (rows and banks), resizable tracks,
    track presets (save, recall, apply), mono/stereo input selection,
    eight device inputs by default.

  Tests cover streaming, recording, takes, automation and the realtime
  no-allocation rule for every new audio-thread path.

  </details>

- **CI: build the app before the GUI smoke test**

  <details><summary>Details</summary>

  `cargo test` does not produce the `faderframe` binary (the app crate has
  only unit tests), so the xvfb smoke step failed with "not found". Build it
  explicitly first. Also move actions/checkout to v5, which runs on Node 24
  instead of the deprecated Node 20.

  </details>

- **Initial FaderFrame vertical slice**

  <details><summary>Details</summary>

  Cargo workspace for a multitrack DAW in Rust: Linux first, Wayland-native
  GTK 4 shell, realtime graph engine.

  Engine and model
  - Project model (audio/instrument/MIDI tracks, buses, auxes, master),
    undoable edit commands with gesture coalescing, versioned .ffproj files
    with migrations, tempo map with ramps, time signatures, grid snapping
  - Directed processing graph with cycle detection, plugin delay
    compensation at every summing point, processor state adoption across
    rebuilds, and dependency data for a future multicore scheduler
  - Realtime processor fed through latest-value mailboxes, a bounded SPSC
    queue and atomics; never allocates on the audio thread (enforced by a
    counting-allocator test); sample-accurate loops at any rate/buffer size
  - Built-in synth, echo, gain and latency-probe plugins behind a
    format-independent hosting API; JACK (JACK2/pipewire-jack) and dummy
    backends; offline render to WAV (master or stems, dither, normalise)

  UI
  - Custom-rendered arranger, analogue-console mixer and piano roll
    (GTK-free views painted through GSK snapshot nodes)
  - Dock model with detachable windows and workspace presets; Preferences
    and Render/Export windows

  Tooling: 156 tests, headless engine benchmark, CI (fmt, clippy, tests,
  GUI smoke test, cargo-deny), MIT license with generated third-party
  license inventory, architecture documentation.

  </details>
