# Features

Everything FaderFrame does today, by area. The [README](../README.md) has the overview, [SCREENSHOTS.md](SCREENSHOTS.md) the pictures and [ARCHITECTURE.md](ARCHITECTURE.md#status-and-roadmap) what comes next.

## Contents

- [Interface and workflow](#interface-and-workflow)
- [Arranging and editing](#arranging-and-editing)
- [Recording](#recording)
- [MIDI and instruments](#midi-and-instruments)
- [Mixing](#mixing)
- [Audio editing: pitch, spectrum, words](#audio-editing-pitch-spectrum-words)
- [Devices and plugins](#devices-and-plugins)
- [Surround and immersive](#surround-and-immersive)
- [Mastering and delivery](#mastering-and-delivery)
- [Picture and performance](#picture-and-performance)
- [Sync, control surfaces and remote](#sync-control-surfaces-and-remote)
- [Interchange and collaboration](#interchange-and-collaboration)
- [Engine and audio](#engine-and-audio)

## Interface and workflow

* GTK 4 application, native on Wayland, HiDPI/fractional scaling via GTK.
  Optionally the analysers, the EQ and the devices' faces are drawn on the
  GPU (vello on wgpu; Preferences → General → Drawing): the mastering tools
  went from 26 to 55 frames a second, and the frames go to GTK without a
  copy through the CPU — as dmabufs on Linux (60 frames a second), as
  shared D3D12 textures on Windows.
* Track colours: click a track's colour stripe (arranger) or colour bar
  (mixer) for a colour chooser, or pick from the palette in the track menu.
* Undo history (Edit or View → Undo History; a dockable panel): every
  step of the project's history in order, the current one marked and
  those that can be redone dimmed; a click goes back (or forward) to just
  after any step.
* Command palette (Ctrl+Shift+P, View → Command Palette…): find any
  command of the menus by typing and run it with Enter. Keyboard
  Shortcuts… (View) changes any command's shortcut — click it and press
  the keys (Backspace: none); a shortcut taken from another command
  leaves it; Reset per command or for all. Kept in the preferences. Every
  switch in the menus shows a check mark (and every choice a dot) that
  follows it whatever changed it — a menu, a button, a shortcut, MIDI learn
  or a control surface — and the palette marks the commands that are on.
* Adding tracks: the "+" under the last track in the arranger and right of
  the last strip in the mixer (each kind of track, or one from a saved
  track preset; an instrument track opens the plugin browser). Track →
  Duplicate Selected Tracks copies tracks with everything they hold —
  devices, sends, automation, modulators — named on ("Bass 2"), or with
  their clips too. Devices of the same name in a chain are numbered
  ("Compressor (2)") wherever they are named.
* Docking: mixer / channel overview / Tools / album / piano roll /
  automation / performance / undo history / modulators / video tabs in a
  bottom dock (collapsed, it keeps its tab bar), the Inspector on the
  left, detach any view into its own window and dock it back, workspaces
  (Recording, Editing, Mixing, MIDI, Mastering), layouts saved with the
  project. View → Master Strip at the Side keeps the master fader at the
  window's right edge, full height, whatever view is shown (per workspace;
  on in Mastering).
* Screen readers and the keyboard: the mixer, arranger, piano roll, event
  list, setlist, history, transport display and the device editors list
  their controls to screen readers (Orca and other AT-SPI readers on
  Linux); Tab moves along a view's controls with a focus ring, Enter
  presses, the arrows move faders, knobs and values, and transport changes
  and messages are announced.
* Preferences (General, Audio, Editing, Recording, Metronome, MIDI, Video,
  Engine, Project: the start-up project, the audio system and devices,
  editing defaults and the grid's strength, recording options, the
  metronome's sounds, MIDI devices, sync and controllers, live DSP
  statistics …), undo/redo, versioned project files (`.ffproj`). A new
  start shows the start window — recent projects, an empty project, the
  templates, Open…, the demo — or opens the last project, a new one (from
  the default template, if one is set) or the demo session (Preferences →
  General; `--empty` / `--demo` / a project path override it once; File →
  Start Window… any time); File → Open Recent lists
  the last ten projects. Closing a saved project offers to move the audio
  files no saved version uses any more to the trash (Preferences →
  General: ask, move or keep).
* Crash reports: if FaderFrame ever crashes — a panic, a fatal signal or
  an unhandled exception, also in a plugin's sandbox process — it writes
  a report with a backtrace into the Crash Reports folder of its data
  folder, beside the logs of the last runs, and the next start offers to show
  the report or open the folder (or says the system ended the run when
  there is none).
* Undo tree and Audio Bisect: the history is a tree — an edit made after
  undoing starts a branch and the old one stays. Edit → Audio Bisect
  searches it for the edit that made the mix worse, as `git bisect`
  searches commits: mark a state that sounded good and one that sounds
  bad, and blind A/B rounds narrow it down to one edit.
* Media browser (View → Browser, Ctrl+Shift+B): sounds, loops and MIDI
  files indexed with their tempo and key, Find Similar, previews in the
  song's tempo and key, placed by dragging.
* Scripts and macros: Lua 5.4 scripts over a sandboxed `ff` API (see
  [SCRIPTING.md](SCRIPTING.md)), recorded macros, run from the Scripts
  menu or by MIDI and control-surface triggers.
* Batch processing (File → Batch Processing…): a list of files through a
  chain of steps, plugins among them, each written anew into an output
  folder, outside any project.

## Arranging and editing

* Arranger (virtualised): tracks, audio clips with waveforms, MIDI clips,
  move/split/delete, snapping, zoom down to single samples with the grid
  drawn at every zoom where clips snap (how strongly: Preferences → Editing
  → Grid lines), loop range (dragged or resized while playing without a
  glitch), playhead, console-style track headers with mute/solo/arm/monitor,
  volume, pan and meters (the wheel over the headers turns their faders and
  knobs and never scrolls; a held scrollbar stops the view following the
  playhead). Clicking a clip selects it and moves the playhead to the
  (snapped) click; Shift-click selects from the last clicked clip,
  Ctrl-click toggles one, Ctrl-drag draws a rectangle, Ctrl+A selects every
  clip shown, and moves, trims, fades and gain changes apply to every
  selected clip. Splitting (S, or B with a range) keeps every piece
  selected, so aligned drums split again without reselecting.
* Snap to Objects (on by default; Edit menu, the edit toolbar's Objects,
  Preferences → Editing): a drag within 8 pixels of a clip edge on any
  shown track, a marker, a section, the loop, the edit selection, the Play
  Start Marker or the stopped playhead lands on it, before the grid, with a
  guide line naming what it caught; automation points, tempo points,
  chords and piano-roll notes snap the same way. Alt bypasses all
  snapping; in Slip mode only objects attract.
* Clipboard: Ctrl+C/X/V/D copy, cut, paste and duplicate the edit range
  (with its automation) or else the selected clips — with everything they
  hold, by value, so a paste works after the source track is gone. Paste
  goes to the playhead (the edit cursor without Link Timeline) on the
  selected track, the other copied tracks following on the tracks below
  that take their kind of clip; Shuffle inserts and pushes, the other
  modes replace what is under the paste; the insertion point moves to the
  end, so repeated pastes lay copies end to end. Shortcuts follow the
  editing context — the editor last worked in, never a text field or a
  toolbar.
* Global lanes under the ruler (click a lane's title, or View → Lanes, to
  show or hide lanes): **markers** (double-click to add, drag to move,
  double-click to rename, click to jump there), the **arranger** lane for
  song sections (drag to create Intro, Verse, Chorus …; resize, rename,
  recolour, loop or select a section), the **key** (drag to set a key over
  some bars, drag a key change), the **chords** (drawn, typed or found in
  the music; Delete All Chords), **time signature** changes, the **tempo**
  map (drag points up and down or sideways, type values, steps or ramps),
  and, when there is something to show, the **lyrics** and the **video**.
  The ruler carries the loop, the Play Start Marker and, while counting
  in, a badge.
* Song arrangement with sections: drag a section to move it *with its
  content* — clips (split at its edges), automation, tempo and time
  signature changes, markers — and the song closes up and makes room
  around it; Ctrl-drag inserts a copy, Shift-drag moves only the section.
  The section menu duplicates a section, moves it earlier or later, or
  deletes it with its content. Each is one undo step.
* Scratch pads (the switcher in the arranger's corner, Edit → Scratch
  Pads, or a section's or clip's menu → Copy to Scratch Pad): other
  arrangements of the project's own tracks to try an idea in without
  touching the song — a section with everything in it, the edit selection
  or chosen clips copied in, edited and played there with its own tempo,
  markers, sections, key, chords, lyrics, automation and loop (the mixer,
  devices and routing are the song's). Bring to Song inserts the pad at the
  song's playhead, overwrites what is there or replaces a section (taking
  the pad's length or keeping the section's). Each step is one undo step;
  renders and exports always take the song.
* Pro-style editing, with an edit toolbar under the transport (Edit button
  or Ctrl+E; it wraps into as many rows as the window needs): edit modes
  Shuffle, Slip, Spot and Grid (absolute or relative); the Smart tool
  (upper half selects a range, lower half grabs, edges trim, top corners
  fade) plus Zoom, Trim and Time-Stretch Trim, Selector, Grabber and
  Separation Grabber, Scrubber and Pencil; grids from a bar down to 1/256
  with triplet and dotted values; nudge values; Tab to clip boundaries or
  transients; Link Timeline and Edit Selection; Insertion Follows Playback;
  selection Start/End/Length counters in bars, time or samples; zoom
  buttons; a Follow button for scrolling along while playing. Range edits:
  separate, trim to selection, clear (Shuffle closes the gap),
  copy/cut/paste, duplicate, repeat, insert silence, merge clips into one
  (audio rendered as it plays). Ctrl+Alt+drag in a clip slips its audio or
  MIDI inside it while its place, length, fades and gain stay (every
  selected clip, one undo step). The Scrubber plays the audio under the
  pointer while you drag; zoomed in to single samples, the Pencil redraws
  the waveform (click repair, on a copy — undo brings the original back).
* Clip gain: a gain knob in every audio clip's name strip (drag or turn
  the wheel; double-click resets, Alt-click types a value; Ctrl+Shift+↑/↓) and fades with five shapes (Linear, Equal Power,
  S-Curve, Fast, Slow) and a curve you bend by dragging its handle;
  right-click a fade to pick its shape.
* Transient detection (spectral flux, adjustable sensitivity, cached) and
  elastic audio: Warp view shows transients and warp markers — double-click
  adds a marker, dragging a transient moves just that hit (its neighbours
  stay pinned), dragging inside a selection warps only that range,
  Ctrl-drag telescopes; Separate at Transients and time-compression trims.
  Quantize (Q) and Humanize work on the selected clips, audio by its
  transients and MIDI by its notes alike: quantize strength and swing,
  note ends, humanize timing (ms) and velocity — set in the edit
  toolbar's Quantize menu or the piano roll's quantize settings. Warped clips play pitch-preserving
  (Polyphonic or Rhythmic, via Signalsmith Stretch) or as Varispeed.
* Project versions: File → Save Version… (Ctrl+Alt+S) keeps the project
  as it is now under a name in its Versions folder; File → Versions…
  lists them, compares one with the project now (tracks, clips, plugins,
  mixer settings, tempo, key, sections …) and restores one — the project
  as it is is kept as a version first, so nothing is lost.
* Project templates: File → Save as Template… keeps a project's set-up —
  tracks and routing, devices with their settings, sends, groups, VCAs,
  folders, modulators and containers, MIDI mappings, automation, tempo,
  meter, key and chords, markers and sections, loop and punch, the album's
  delivery settings, the launcher's scenes, video tracks and the window
  layout — without its clips and media (the samplers' samples go with it,
  so a template stands on its own). File → New from Template… starts an
  unsaved project from one (or from any project file's set-up); the one
  marked Default is where New Project (Ctrl+N) and a new start begin.
* Clip aliases: right-click a clip → Duplicate as Alias; aliases (marked
  by two overlapping frames before the name) share their content — notes,
  controllers, length, gain, fades, warp — so an edit of one is an edit of
  all (one undo step), while each keeps its own position, name, colour and
  mute. Make Unique ends it; splitting an alias makes the split one its
  own. Copies and pastes are never aliases.
* Folder tracks: Track menu (or the "+") → New Folder with the Selected
  Tracks; folders hold tracks and other folders, show them indented under
  them, close by their triangle (or a double-click), give an overview of
  their clips, and their mute and solo reach everything inside. Move tracks
  into and out of folders from the track menu, or drag a track's header
  (arranger) or strip (mixer) between others to reorder it and onto a
  folder to put it in; removing a folder keeps its tracks. A folder can be
  a bus (its menu → Folder Is a Bus, or the Inspector's Folder Bus switch):
  the tracks in it that play to the master play through it once instead —
  its strip, devices, sends and automation are a bus's, nested folder
  buses chain, moving a track in or out reroutes it, and a track routed
  elsewhere by name stays there. In the mixer a folder has a narrow strip
  of its own (its triangle, mute, solo, what it holds) and a folder bus a
  full strip marked FOLDER BUS, its tracks a band of its colour along their
  foot, and a closed folder's strips fold away there too. A folder's G
  button (its header, its strip, the Inspector) groups what it holds:
  arming one of its tracks arms the others, selecting a clip or a range
  selects the material at the same place on the others, and edits of it —
  split, move, trim, fades, gain, slip, cut/copy/paste, delete, duplicate —
  reach them, one undo step each (a dry and a wet guitar recorded on two
  tracks stay together).
* Tempo and key from clips: set the project's tempo or key from a loop or
  a recording, or warp a clip to the project's tempo.
* Snap to Samples (on by default) and Snap to Zero Crossings (the edit
  toolbar, beside the grid): free positions land on whole samples, and
  cuts and trims of audio clips move to the nearest zero crossing so they
  do not click.
* Song structure from a jam (an audio clip's menu, or several clips of a
  multitrack jam): the recording's parts found — Intro, Verse, Chorus,
  Bridge, Solo, Outro — and laid on the arranger's section lane, on the
  bars when the project plays at the jam's tempo.
* Tempo from hit points (a marker's menu, the markers or tempo lane's
  menu): for scoring to picture, the markers (or the cuts found in the
  picture) become hits the music lands on — a tempo map with a whole
  number of beats, eighths or bars between them, as steady as it can be
  within the tempo range asked for, previewed before it is applied.
* Clip envelopes (Edit → Clip Envelopes, or the clip menu's Clip
  Envelope): volume and pan drawn on an audio clip over its waveform.
  They belong to the clip and move with it: a split gives each piece its
  part, trims and time-stretch trims keep them on the audio. Volume from
  −96 to +24 dB; pan as a balance on stereo tracks.
* Strip Silence (Edit → Strip Silence…, or the audio clip menu): the sound
  between silences kept as clips, by threshold, minimum length, padding
  before and after, and a minimum gap.
* Slicing (REX-style): a span of audio, or the sample a Sampler or Drum
  Sampler pad plays, cut at its transients and played back from a sampler
  in the original rhythm, the slices on pads or keys.
* Aligning doubles (the clip menu's Align Timing to): a doubled vocal,
  backing vocals or an ADR line warped onto a guide track's timing, as
  tight as you choose.
* Merging project versions: two versions of a project merged three-way
  against the one they came from, for collaborators.

## Recording

* Transport: tap tempo (the TAP pad in the display), editable time
  signature (click it; right-click for common meters and meter changes),
  metronome button (K; Preferences → Metronome: levels for the first beat,
  the other beats and the off-beats, built-in sounds or your own samples,
  the click only in the cue mixes if wanted). Where Stop leaves the
  playhead: where playback stopped (the default), back where Play was
  pressed (Return to Start on Stop) or at the project's start (Return to
  Project Start on Stop) — in the Transport menu and on the Stop button's
  right-click menu. A Play Start Marker (a flag in the ruler; set it from
  the ruler's right-click menu or the Transport menu) makes Play and Record
  from stop start there whatever the playhead.
* Audio import: WAV, AIFF, CAF, FLAC, MP3, Ogg Vorbis, AAC/M4A, ALAC — via
  File → Import Audio (Ctrl+I) or drag & drop from any file manager onto
  the arranger (samples onto the samplers' slots and pads). Files are
  converted to the project rate once and streamed from disk during playback
  (bounded memory, lock-free read-ahead); media is kept in the project's
  `Audio/` folder; missing files show as offline clips.
* Recording: arm tracks, choose a mono input or a stereo pair per track,
  record with punch in/out (Ctrl+P), pre-roll (bars, beats or seconds,
  from a bar line if wanted, or a count-in with the click alone), post-roll
  and metronome; takes are placed by the device's real input and output
  latency and the graph's. Where the pre-roll would reach back before the
  project's start, the missing bars are counted in: the transport stands
  (inputs and monitoring live, nothing played or recorded), the click
  counts on the song's tempo and metre, the counter shows bar 0 (or −1)
  with COUNT-IN, and playback starts on bar 1. While recording over a
  clip the recorded track plays its input, not the old clip. Loop
  recording keeps every pass as a take. The record button (Shift+R)
  records and plays when stopped, punches in while playing and out while
  recording; with nothing armed a notice pops up. Ctrl+Space aborts the
  take (its files deleted, nothing placed), Ctrl+Shift+Space aborts it
  and starts again from where it began. Input quantize (the Q in the
  header bar; right-click for its grid) puts recorded MIDI on the grid as
  it lands, without delaying what you hear. Warnings and errors pop up at
  the top of the window.
* Takes and comping: take folders with take lanes — click a lane to use a
  take, drag across it to comp a section, flatten when done; drag a
  folder's lanes taller (or pick a size from a lane's menu), and its ▶
  plays that take whole instead of the comp until you stop it. Record mode
  (keep as takes / replace) and loop-record mode (takes / last pass / new
  track per pass) are selectable in Transport and Preferences → Recording.
* Track presets: save a track's channel settings (format, input, plugins
  with state, fader, pan, sends, output, colour) under a name and recall
  them as a new track or onto another track. A name that exists already
  is replaced only after you confirm (several tracks at once: replace or
  keep both); the track menu's Delete Track Preset removes one. Resizable tracks (drag a header's bottom
  edge, Alt+wheel for all, View → Track Height).
* Capture MIDI (the ⟲ button after Record, Ctrl+Shift+C, Transport →
  Capture MIDI): FaderFrame keeps the last ten minutes of what the tracks
  that play live were played, recording or not. Capture turns the latest
  playing into clips on those tracks: played while the song ran, the notes
  land where you played them (loop passes as the loop-record setting
  says); played while stopped, the phrase (back to an 8 s pause) starts on
  the bar at the playhead with its timing kept. One undo step.
* Capture Audio (Transport → Capture Audio, Ctrl+Alt+C): the input of the
  armed tracks (and, by default, those monitoring their input) is kept
  while playing or stopped, and the newest phrase becomes takes
  afterwards — the best take, played while not recording.
* Comping by lyric: Whisper's word timings on every take; choose the best
  take word by word, the syllable shown under each note in the pitch
  editor.

## MIDI and instruments

* Samples from audio: right-click an audio clip (or an audio track's
  header) to turn the edit selection, or the selected clips, into a sample:
  rendered as the clips play (clip gain, fades, warp; without the track's
  plugins), it goes to a new Sampler track (the root key and tuning found
  in the audio), a new Drum Sampler track, the next free pad of a Drum
  Sampler in the project, or a 24-bit WAV file.
* Piano roll: select/draw/erase/split/mute tools, rubber-band selection,
  moving and Alt-copying with snap (Shift: free), resizing from either edge,
  chords (fixed or scale-aware), scales with highlighting, snap and folding,
  quantize (strength, swing, ends), humanize, legato, reverse, invert,
  velocity stems and line ramps, controller lanes (mod wheel, pitch bend,
  sustain, any CC: freehand, lines, erase), ghost notes of the track's other
  clips, auditioning, step input from a MIDI keyboard, copy/paste/duplicate,
  clip-length handle, inspector with numeric entry, live keys on the
  keyboard, zoom buttons (−, +, Sel, Fit) and Alt+wheel for taller or
  shorter rows; moved and resized notes snap to the other notes' edges;
  drum clips (a drum sampler's, or all on channel 10) show their notes as
  diamonds (toolbar → Drums).
  MIDI Tools (toolbar): transform the selection — strum, chop,
  join, connect with scale runs, arpeggiate, recombine, conform to the
  chord track, accent, time scale, ornaments — or generate Euclidean
  rhythms, seeded melodies in the key, voice-led chords and basslines from
  the chord track, and drum patterns; previewed in the grid, applied in
  one undo step. The scale follows the key track; the chord track shows
  above the notes and can be stamped.
* Audio to MIDI (an audio clip's menu): a melody, the notes of any
  instrument, chords too (Spotify's basic-pitch model, built in), or a
  drum pattern (kick, snare and hat on the Drum Sampler's pads) on a new
  instrument track.
* Singing into MIDI live: choose "Voice · In n" as an instrument or MIDI
  track's MIDI input and sing (or play a monophonic instrument) into that
  audio input — the track plays the notes as you sing, a few milliseconds
  after each begins (tracked on the audio thread), with glides as note
  expression or pitch bend if wanted, snapped to the project's key if
  wanted, and records them where they were sung. Preferences → MIDI →
  Voice to MIDI → Polyphonic enables chords from one input, with independent
  glides through note expression. Chord detection takes longer; simultaneous
  octave doublings can merge. Monophonic mode remains the default. Every
  input a track takes is heard at once, so several singers or players can
  each have a track; "Voice · All Inputs" brings every input to one track,
  each on its own MIDI channel (a choir, or a hexaphonic guitar pickup with
  every string's bends apart).
* MIDI 2.0 (Linux): MIDI 2.0 devices and programs as inputs and outputs
  through the ALSA sequencer; per-note pitch bend and controllers are
  played, recorded and captured as note expression, and clips' note
  expression goes out to MIDI 2.0 gear as per-note controllers; MIDI 2.0
  Clip Files (`.midi2`) are imported and exported (File menu).
* MIDI event list (View → MIDI Event List, or a MIDI clip's menu): every
  event of a clip a row — notes with release velocity, control and program
  changes (with their General MIDI names), pitch bend, channel and poly
  pressure, the notes' expression points, SysEx — each field typed or
  dragged. MIDI files keep program changes, poly pressure and release
  velocities in and out.
* Lead sheets (View → Lead Sheet; a clip's or track's menu): a melody —
  sung or played, audio or MIDI — written out as notation with its chords
  (from the chord track, else from what the other tracks play) and the
  lyrics lane's words a syllable a note, or several tracks as a score with
  a staff each; for guitar and bass also as tablature (notes and tab, or
  tab alone, in common tunings with a capo), fingered for the hand.
  Exported as MusicXML 4.0, PDF or plain-text tab.
* Stock instruments: a virtual analogue synth (two oscillators with sync,
  ring modulation and FM, up to seven unison voices, sub, white or pink
  noise; low pass 12/24, band, high pass, ladder and notch filters with
  audio-rate FM; two LFOs, filter, amplifier and mod envelopes and a
  six-slot modulation matrix; up to 32 voices, mono or legato with glide,
  analogue drift; pitch-bend range and three mod-wheel destinations; MPE
  and per-note expression; chorus/ensemble, delay and reverb), a sampler
  that plays a sample across the keys (between start and end markers
  dragged on its waveform, forwards or reversed, looped or not, every key
  held lit on its keyboard) or an SFZ instrument (SFZ 1 and the common
  SFZ 2 opcodes: keyswitches, release and legato triggers, crossfades,
  two filters and an EQ, three envelopes and LFOs, controller
  modulation with curves, `#define`/`#include`), and a
  sixteen-pad drum sampler with choke groups, each pad's start and end
  dragged on its waveform, a click on a pad to hear it, and eight extra
  stereo outputs (each pad's Output; Create Output Tracks gives them and
  the main tracks).
  Both samplers can keep a
  sample's length when they change its pitch (Sampler: Keep Length;
  Drum Sampler: per pad), through the same stretcher as warping, without
  added latency.
* MIDI effects before the instrument: Arpeggiator (nine orders, synced,
  swing, gate, octaves, hold), Chord (intervals, the key's chords or the
  chord track's), Scale (keeps notes in the key, transposes by degrees)
  and Note Echo (fading, optionally rising repeats); the scale-minded ones
  follow the key and chord track. On instrument tracks before the
  instrument, and on MIDI tracks (track menu → Add MIDI Effect…).
* MIDI files: File → Import MIDI File… (a track per MIDI track or channel,
  with controllers and SysEx; tempo and meter too when the project is
  empty) and File → Export MIDI File… (all MIDI tracks, or the selected
  clips).
* Per-note expression and SysEx: pitch, pressure,
  timbre, volume, pan, vibrato and expression per note — drawn in the piano
  roll's expression lanes, recorded from MPE controllers, played natively
  to plugin instruments (CLAP note expressions, VST3 note expression values
  and poly pressure, the built-in synth) or as MPE on member channels
  (track menu → MPE: pitch, pressure, timbre). SysEx is recorded,
  sent to external devices and to the track's plugins with the clip (and
  from a live keyboard to the instrument playing it), imported from and
  sent as `.syx` files.
* MIDI keyboards and controllers: every MIDI input (ALSA sequencer incl.
  PipeWire, CoreMIDI, WinMM; hotplug) — instrument tracks play what you play while armed or
  selected, with constant low latency; choose the input and channel per
  track (track menu → MIDI In). Record MIDI into clips (takes or replace,
  loop recording). MIDI learn: right-click a fader, pan, mute, send,
  automation lane, plugin parameter or bypass — in FaderFrame's own device
  editors too (the Guitar Station, the EQs, every stock device), a surround
  pan's parameters, the transport buttons and an editor window's Bypass —
  → MIDI Learn… and move a knob; Remove MIDI Mapping (named after its
  controller) takes it away where it was learned, and Preferences → MIDI
  removes learned transport controls. Pads can toggle switches or run
  transport functions; soft takeover and endless encoders (relative
  modes); mapped controls don't reach the instrument. Mod wheel, pitch bend, sustain and aftertouch are recorded
  into clips and chased on playback. MIDI tracks play an instrument track
  (track menu → Plays: …; a MIDI track added while an instrument track is
  selected plays that one) and external instruments (track menu → MIDI
  Out) in time with the audio, and MIDI clock can sync
  external gear. Inputs, outputs, clock and mappings in Preferences → MIDI.
* Wavetable, FM and Granular: three more instruments that play like the
  Synth (its envelopes, filters, LFOs, voicing, an eight-slot matrix and
  effects), each with an engine of its own (FM: six operators and a free
  matrix).
* Microtonal tuning (Audio → Tuning): a tuning and a reference pitch (A4,
  200–1000 Hz) for the project; a track may play a tuning of its own.
* Articulation and drum maps (View → Map Editor): articulation maps as in
  Cubase — an instrument's articulations, set on notes or in the clip's
  articulation lane, sending keyswitches, program changes, controllers or
  a channel — and drum maps.
* Score editor (View → Score): MIDI clips engraved and edited as
  notation, two voices a staff; the score and the piano roll edit the
  same notes.

## Mixing

* Panning: the project's pan law for mono tracks (−3, −4.5, −6 or 0 dB;
  Preferences → Project), balance or dual pan (each side placed on its
  own) for stereo tracks, all automatable; a jump in position glides
  instead of clicking.
* Analogue-console mixer: inserts, sends (pre-FX / pre / post; a send given
  a pan of its own in its menu places the track's source in the return,
  rather than re-balancing the panned signal), pan, M/S/R, faders with a
  console fader law, segmented peak meters with a gain-reduction stripe
  beside them (built-in compressors and CLAP, VST3, LV2 and Audio Unit
  plugins that report it), routing menus, any number of sends per channel
  (rows grow, ◂ ▸ pages through banks), scribble strips, pinned master
  section; click a pan or level readout to type a value. The input section
  holds a modelled microphone preamp (circuit-solved: British 73, American
  312, British 4K E, Tube 610, British 47 or German 76, their gain the
  circuit's real gain). Console summing (Audio → Console, or the master's input slot): American,
  British 4K, British 73, Tube 610, British 47 or German 76 — the buses and
  the master sum through the console's bus amplifier circuit (latency
  compensated; each bus may follow the console, run through another
  console's or stay in the box), every channel through a light
  model of its line amplifier matched to that circuit, a channel drive, and
  the mixer in the console's look (or your theme). The inserts section shows five slots by default; drag the
  grip on the rule below it for more or fewer (saved with the layout).
  MIDI tracks get a strip of their own: the instrument track they play
  (PLAYS, where the others have their preamp), their MIDI input and live
  mode, their MIDI effects in the inserts (add, open, drag to reorder or
  onto an instrument track), the notes they play now by name and on a key
  display, mute/solo/arm, and the external MIDI device in the output well.
  Drag a strip's right edge to make it wider or narrower (Shift: every
  strip; double-click: back to normal), or pick Narrow / Normal / Wide /
  Extra Wide in its menu (saved with the layout).
  Drag inserts to reorder them or onto another track to copy them with
  their settings (Ctrl copies within a track, Shift moves to another
  track); Alt-click removes one.
* Inspector (F11, the "i" in the header bar, View → Inspector; a panel on
  the left): the selected track's name, colour and kind, and foldable
  sections — routing (input, output, monitoring, format, pan mode,
  surround panner, object), track (record arm, MPE, group, VCA, folder,
  a folder's bus and group switches, automation modes, freeze), its devices
  in chain order with their latency and gain reduction, and its console
  strip from the mixer. View → Channel Overview shows the same track's
  signal path in one row in the bottom dock: input → a card per device
  (switch it, open it, remove it, drag it into another order, "+" for
  another) → channel → sends → output.
* Cue mixes (Audio → Cue Mixes; up to eight): headphone mixes on outputs
  of their own, apart from the main mix. A new cue mix copies the main mix
  as the console would (each channel once, where it reaches the master);
  the bar over the mixer's strips switches the faders, pans and mutes to
  set the cue sends instead (pre- or post-fader, their pan placing the
  source). Cue mixes are solo-safe and mute-safe, buses and returns
  included — they hear the drum bus's compressor and the reverb return
  whatever the main mix solos — and every track added later joins them.
  Each has its level, mute, outputs, a click of its own (or only the cue
  mixes click) and talkback: the TALK button (hold, or Shift-click to
  latch) sends a microphone input to the cue mixes that hear it and dims
  them. Cue sends and levels are automatable, take MIDI learn and follow
  control surfaces; renders and exports never contain cue mixes.
* Groups, VCAs and multi-track edits: with several tracks selected, moving
  a fader, pan or send moves all of them relatively (balances survive even
  pulling everything to −∞ and back), and mute, solo, arm, polarity,
  monitoring, colour, output and VCA are set on all of them. Track groups
  link volume, mute, solo, record arm and selection (each switchable, the
  group can be deactivated); VCA faders scale, mute and solo the tracks
  assigned to them, nested VCAs included, with automation. The mixer shows
  each strip's group and VCA.
* Sidechains: any plugin with a sidechain input can be keyed by another
  track (insert menu → Sidechain from …; the signal after its inserts, so a
  muted "ghost" track still keys); the built-in compressor has one.
* Freeze and bounce: Freeze Track renders a track after its inserts and
  plays the file instead (its plugins are unloaded until you unfreeze);
  Bounce to New Track puts the rendered audio on a new track.
* Modulators (View → Modulators, a tab in the bottom dock, for the
  selected track): LFOs (five shapes, synced to the beat or free, start
  phase), envelope followers (the track's own input or another track),
  step sequences (up to 32 steps, drawn with the mouse, with glide),
  smoothed random and macro knobs — and per note: velocity, key, a note
  envelope, a note LFO and a note random, a value for every sounding
  note. Each moves any number of targets with a depth: the track's fader
  (in fader travel) and pan and every device parameter that takes
  modulation; Map, then move a control, routes it. Modulation never
  changes a value — automation, presets and saved state keep what you set,
  and the device knobs show a dot where modulation has the value now.
  Synced modulators follow the song position, so every playback and every
  render of a passage sounds alike.
* CLAP modulation: CLAP parameters take FaderFrame's modulation without
  their values changing (CLAP's parameter modulation events), and
  per-note modulators reach single voices of CLAP instruments that offer
  polyphonic modulation (by key or note id — with u-he Diva, for example,
  each note's filter moves on its own), also through the plugin sandbox.
* Containers (insert a Container): parallel chains of devices, each with
  its own level, balance, mute and solo, mixed at the container's output —
  parallel compression, dry/wet blends, split effects; a chain without
  devices is the dry signal, containers nest, and the chains' latencies
  are aligned automatically. Its editor shows a column a chain: add
  devices and chains, drag levels, open the devices' editors.
* Time-locked tracks (a track's menu or the Inspector → Time Base:
  Musical or Time-locked): on a time-locked track clips, MIDI clips'
  starts, automation and a frozen render keep their samples whatever the
  tempo does, in the same undo step as the tempo change; a padlock marks
  them. ADR cues are always locked to the picture, and the ADR beeps'
  track is time-locked.
* Track delay and Auto-Align: a track's own delay (Inspector → Delay,
  milliseconds or samples), later or earlier, to a fraction of a sample,
  compensated like plugin latency. Auto-Align Microphones… (the track
  menus) measures the delay and polarity of microphones on one source by
  cross-correlation and sets them in one undo step; tracks without a
  common signal are left alone.
* A/B listening: the History view's A/B renders the project as it was at
  that step (on a copy — nothing is undone) and flips Now/Then in step
  with the transport, loudness-matched on request. Reference tracks (View →
  Reference Tracks): songs to compare with, playing along the song or
  looping their region, matched in loudness, their spectrum against the
  mix's, and Send to EQ Match. Ctrl+Alt+A flips; meters always show the
  mix.
* Mix Intelligence (View → Mix Intelligence): analyses the project, the
  edit range or the selected tracks — processed (as mixed, from one
  render that captures every track, bus and return where it reaches its
  destination) or the source — without AI, models or network, and lists,
  ranked: potential masking between tracks (copies of the same audio are
  recognised and not counted), phase and timing problems between related
  tracks, stereo width and mono collapse per band, dynamics, level
  problems, a health check of the devices (DC offset, overs between
  devices, devices doing nothing at a CPU cost, bypassed ones) and
  comparisons with a reference track. Each finding comes with its
  measurements, severity and confidence and how they were reached, the
  rule's limitations, and experiments — EQ cuts, levels, a track's delay
  and polarity, gain staging, bypassing — to audition (A/B,
  loudness-matched) and apply as one undo step. Clicking a finding selects
  its tracks and shows its time. Track roles help it weigh pairs; after an
  edit only what it affects is measured again.
* Meters: each strip's meter shows sample peaks, peaks with the RMS, a
  VU (0 VU at −18 dBFS or another reference), the EBU PPM or a K-System
  meter (K-20, K-14, K-12) — chosen from its right-click menu, per strip
  or for every one — and the mixer's meter bridge puts a moving-coil VU
  meter over every strip.
* Automation for every automatable parameter — volume, pan, mute, sends,
  plugin parameters and bypass: lanes under each track (A button / A key),
  point editing and freehand drawing with curve shapes, Read / Touch /
  Latch / Write modes; faders follow the automation while playing. The
  Automation tab in the bottom dock lists every lane of the project by
  track (modes, point counts, shown in the arranger or not; "+" adds a
  lane; all lanes to Read or Off at once) and edits the selected lane at
  full width over the whole song — add, drag and delete points, curve
  shapes, Write Value at the playhead or over the edit selection, Thin,
  Clear, Delete.
* Mix scenes (View → Mix Scenes): console snapshots recalled, compared,
  crossfaded and morphed.
* Self-freezing (Audio → Self-freeze): tracks nobody touches play from a
  disk cache of their chain's output, so a heavy project plays on a small
  machine. Nothing in the project changes, any edit sends the track back
  to its chain, and the plugins stay loaded.

## Audio editing: pitch, spectrum, words

* Pitch editing (an audio clip's menu → Edit Pitch, or View → Pitch
  Editor): the notes of a voice or solo instrument are found and shown as
  blobs at the pitch they are heard at, the pitch curve drawn through
  them. Drag notes to move them by semitones (Alt: freely), straighten
  their drift and vibrato, correct them to the key of the song, move their
  formants, split and join them, change their length or take them out.
  Notes play back through pitch-synchronous
  grains: the pitch is exact and a voice keeps its character, warping
  included.
* Clip effects (an audio clip's menu → Clip Effects…): a chain of devices
  on one clip, rendered into the audio it plays as you change it; the
  original stays with the clip, so the effects can change again or go.
  Open Window shows an effect's own window (its GUI, a built-in's editor,
  or the parameter list), and what you change there is rendered too.
* Polyphonic pitch editing (the Pitch editor's Polyphonic option, on by
  default): every note of a recording is found, chords too, and each is
  moved, corrected to the key, straightened, made louder or quieter or
  taken out on its own, the rest of the sound left as it is.
* Spectral editing (an audio clip's menu → Spectral Editor): the clip's
  spectrogram, regions drawn on it (rectangle, time range, frequency band,
  lasso, brush) with soft edges, and turned into edits: Attenuate (down to
  the sound around it, only where it stands out — coughs, clicks,
  squeaks), Heal (replaced by the sound around it, tones carried through a
  dropout), Remove or Gain. Edits stay editable (select, move, change,
  delete); they render into a copy of the clip's audio, bit-identical
  wherever no edit reaches, and the original is kept.
* Speech and lyrics (an audio clip's menu → Transcribe Words): a clip's
  words, sung or spoken, as lines on the arranger's Lyrics lane (OpenAI's
  Whisper running in FaderFrame; the model, 290 MB, is downloaded once
  from Audio → Download Speech Model). Lines are edited in place; the
  lane's menu writes them next to the project as LRC and SRT.
* Stem separation: a stereo recording split into vocals, drums, bass and
  the rest (X-UMXL, running in FaderFrame).

## Devices and plugins

* Stock dynamics with editors of their own: a compressor (seven styles —
  Clean, Punch, Opto, Vintage, Bus, Mastering, Pumping — soft knee, hold,
  lookahead, auto release and makeup, range, upward compression, left/right
  or mid/side, colour with optional oversampling, mix, delta listening, a
  filtered sidechain), a true-peak lookahead limiter that never passes its
  ceiling (seven styles — Transparent, Punchy, Aggressive, Allround, Bus,
  Safe, Modern — release link,
  unity-gain listening, the output's momentary and short-term loudness),
  a gate/expander/ducker with hysteresis, hold and lookahead, and a de-esser
  (split band or wide, absolute or relative detection).
* 76 Compressor: a FET limiting amplifier written from how the 1176
  behaves — its feedback loop solved per sample, attack from 800 µs to
  20 µs or Off, the ratio buttons in any combination (all four in: the
  "all buttons" sound, where louder input comes out quieter), the FET's
  colour, a backlit VU with real ballistics and a panel of its own.
* 2A Leveler: an optical leveling amplifier in the LA-2A's manner, from
  the factory drawings — the input transformer, the T4 optical cell (an
  electroluminescent panel lighting photoresistors whose release depends on
  how long and how hard they were lit), the valve make-up amplifier and
  output transformer — with Gain, Peak Reduction, Compress/Limit, a VU
  (gain reduction, +4, +10), and Mix, an external key and a stereo link.
* Tape Machine: a studio tape deck modelled physically — Jiles–Atherton
  magnetisation with AC bias (under- and over-biased too), three tape
  formulations at five operating levels (185 to 520 nWb/m), NAB/IEC/AES EQ
  at 3¾, 7½, 15 and 30 ips, head losses and the head bump, wow and flutter
  fitted to a studio machine's figures, azimuth and crosstalk, optional hiss
  at the tape's level; level-neutral at the operating level, two VUs, the
  response and the tape's M–H curve drawn.
* Hardware Insert: outboard gear in a chain — the signal goes out of an
  interface output and comes back on an input; a ping measures the round
  trip, which delay compensation then takes into account.
* A console channel strip in one insert: input trim, high and low pass
  (optionally only into the dynamics' key), gate/expander, compressor
  (peak or RMS, soft knee, make-up, parallel mix, sidechain key), a
  four-band EQ with "Brown" or "Black" (proportional Q) bands, the
  dynamics before or after the EQ, console drive and output, with its EQ
  curve and gain-reduction history.
* Stock effects: an oversampled saturator (six curves), a utility (width,
  balance, mono bass, polarity, channel modes, vectorscope), a delay
  (stereo, ping-pong or mono; free or tempo-synced with dotted and triplet
  values, left and right times linked or apart; digital, tape and analog
  styles with wow, flutter and saturation; diffusion, ducking, freeze), an
  algorithmic reverb (Room, Hall, Plate, Chamber, Ambience, Spring and
  Cathedral; a 16-line network with three-band decay, pre-delay in ms or
  synced, shimmer, freeze, ducking), chorus/ensemble/flanger/phaser/vibrato/
  tremolo and a rotary speaker, and a tuner (needle or strobe, transposing
  instruments).
* EQ in the spirit of FabFilter Pro-Q 4: 24 bands (bell, shelves, cuts of
  any slope from 0 to 96 dB/oct and brickwall, notch, band pass, tilt, flat
  tilt, all pass) whose curves keep their analog shape up to Nyquist; zero
  latency, natural phase (analog magnitude and phase at 128 samples) and
  linear phase; per band stereo, left, right, mid or side; dynamic bands
  (auto or custom threshold, attack and release, soft knee) triggered by
  their region, free low/high cuts or the sidechain, and spectral dynamics
  that act per frequency; an output stage with character (Clean, Subtle
  transformer, Warm tube, Console, Tape) driven harder or softer and
  oversampled 2× or 4× on request, a dry/wet mix for parallel EQ, gain,
  pan, phase invert, auto gain and a gain scale; an analyser with pre, post and an
  external spectrum (the sidechain or another EQ) and collision detection;
  EQ Match, EQ Sketch, Spectrum Grab, a piano display, A/B, the instance
  list, typed values ("1k", "A4").
* Program EQ: PultEQFx's circuit-modelled passive tube program equaliser
  (the low end trick falls out of the circuit), with its hardware panel,
  input and output meters, drive and oversampling.
* Guitar Station: GainStageFx's guitar rig as one device. A pedal line of
  up to eight pedals and wahs (drives, distortions, fuzzes, two
  compressors, a bass preamp, a phaser, a chorus, a bucket-brigade delay,
  two wahs with an envelope follower), each on a true-bypass footswitch,
  dragged into any order; twenty-four circuit-modelled guitar and bass
  amplifiers (the Tweed Lux and Seattle 100 newest, each with both
  channels) with their own switches, reverbs, tremolos and power stages (or
  another amplifier's), the power stage working into a
  physical loudspeaker in a cabinet, and two microphones placed on the cone
  (position, distance, angle, pan). A DI on a second output (the input,
  after the pedals or the preamp's direct out), factory rigs, and a panel
  drawn like the hardware: the pedalboard, the amplifier's head, the cabinet
  with its microphones. Every pedal is a stage of its own on its own thread
  (a stereo signal on two), which reports its buffer as latency.
  GainStageFx's noise gate sits after the input (switch and threshold in
  the output strip). 71 factory presets: the rigs, rigs round the newer
  pedals (Pedals), every amplifier on its own (Amplifiers) and guitar
  sounds researched from well-known records, named for the record
  (Sounds). A mono
  track turns stereo from the Guitar Station on, so the microphones' pans
  are heard, and the IN and OUT meters always show left and right.
* Plugins: built-in synth, EQ, Program EQ and the stock devices, CLAP and VST3 effects and
  instruments on every platform, LV2 on Linux (X11 editors, presets,
  state, the worker and transport; `LV2_PATH` or `~/.lv2` and the system
  folders) and Audio Units on macOS, found by a
  crash-safe background scan (LV2: its metadata, no code loaded; Audio
  Units: the system's registry) and
  picked in a plugin browser (click an
  empty insert slot or Track → Plugin Browser…; search by name, vendor or
  feature, filter by format, vendor or kind). Favorites: star a plugin and
  sort favorites into categories of your own (a plugin can be in several);
  they head the browser's sidebar and the empty insert slot's menu, by
  category, and stay favorites — shown as missing — if a plugin is
  uninstalled. New instrument tracks
  start empty and open the browser to choose their instrument (an
  instrument picked for an empty instrument track's insert slot becomes
  its instrument); plugins that take notes in insert slots get the
  track's MIDI too. Multi-output plugins (a drum plugin's kit pieces on
  outputs of their own, the Guitar Station's amplifier and DI): Create
  Output Tracks in the insert menu, the track menu or the editor's
  Outputs menu makes an Aux track for every output in one step, the main
  one too, in a folder under the plugin's track and routed where that
  went; the plugin's own track then only carries the plugin (its output
  off, so nothing plays twice), and removing the Main track gives it its
  routing back (Outputs → one output; any track can also take an extra
  output as its input). The Drum Sampler sends pads to its eight extra
  outputs (the pad's Output). Click a filled insert slot
  for the plugin's own GUI (Ctrl-click bypasses, right-click for the
  parameter window, presets, sidechain and more); editors open centred or
  where they were last, and their positions are saved with the project.
  Plugin state, parameters and automation are saved too. Moving a knob in a
  plugin's own GUI writes automation like FaderFrame's controls do. Presets:
  save and load your own for any plugin (insert menu or the parameter
  window's Presets menu); saving under an existing name asks before it
  replaces that preset, and Delete Preset removes your own (factory
  presets stay). The built-in devices come with 470 factory presets — the
  EQ 47, the compressor 43, the limiter 35, the reverb 33, the delay 31,
  the Synth 48, the Guitar Station 61, the MIDI effects 56 and the others
  the rest — grouped where there are many (Vocals, Drums, Bass, Bus,
  Master, Creative; Rooms, Halls, Plates, Springs; Leads, Pads, Plucks …)
  and selected from the editor's Presets menu in one undo step. Delay and
  Reverb presets load fully wet on Aux returns. VST3
  factory presets and LV2 presets (the plugin's and preset bundles') are listed too, as are
  VST3 program lists (selecting a program is one undo step; MIDI program
  changes select VST3 programs). Plugins that offer it can process in
  64-bit floating point (Preferences → Audio).
  Sandboxing (Preferences → General, on by default): each CLAP and VST3
  plugin (and Audio Unit on macOS) runs in a process of its own, so a
  plugin that crashes or hangs costs only itself — its track plays on dry,
  FaderFrame says so, and Reload Plugin (insert or instrument menu) starts
  it again with its recent settings. On Linux and Windows editors still
  open in FaderFrame's windows; on macOS, where a window cannot show
  another program's views, they open in a window of their own. Audio
  passes through shared memory (7–9 µs per plugin and block).
  Realtime check (the plugin browser's Check: every plugin not checked
  yet, or one at a time): a plugin is played in a helper process while
  what it does on its audio thread is counted — allocations, waits, page
  faults, time per block, resets — and the browser shows a verdict, the
  findings and advice per plugin version ("With findings" and "Not
  checked" list them).
* A device's own A/B and undo: every editor window's header has the
  device's Undo and Redo, A and B, Copy (A → B or B → A) and Match.
* Restoration devices: De-noise, De-click, De-hum, De-clip and De-reverb;
  the spectral editor's Learn Noise and Remove uses the same processing.
* Convolution Reverb (without latency), Multiband Dynamics and a
  Vocoder.
* Pitch Correct: a live, monophonic pitch corrector, to a key or to the
  notes played into it.
* ARA hosting (Melodyne, SpectraLayers and the like), not yet tried with
  a real ARA plug-in.

## Surround and immersive

* Surround: any track, bus or the master can be a bed — LCR, quad, 5.0,
  5.1, 7.0, 7.1, 5.1.2, 5.1.4, 7.1.2 or 7.1.4, or a speaker layout of your
  own (Audio → Speaker Layouts…, up to 16 speakers, below ear level too)
  (Channel Format in the track menu). Tracks feeding a bed are placed in a
  room seen from above, in the mixer strip and in the Surround Panner (View
  → Surround Panner): left/right, front/back, height, spread (coherent or
  diffuse), stereo width and an LFE send, all automatable — by room
  position or by direction between the speakers (VBAP), a stereo track as
  an image, by balance or with each side placed; beds fold into smaller
  formats with a standard downmix (and down to the interface's outputs, so
  a 7.1.4 mix plays on headphones), bass management for the speakers
  (Preferences → Audio), meters show every channel, and renders keep every
  channel with the speakers in the WAV header — or fold down to stereo. Objects: any track panned into the master's bed can be an object,
  heard as a renderer plays it (it skips the master's inserts and fader);
  the mix is delivered as an ADM BWF master — the bed (up to 7.1.2) and up
  to 118 objects with their movement — in Dolby's Atmos master ADM profile
  or as ITU-R BS.2076 for the EBU renderer, and ADM BWF masters import back
  into tracks with their movement as automation. Listening: on headphones
  the master is rendered binaurally (measured KU100 responses, SADIE II;
  Near, Mid or Far), and a MONO button on the master strip checks the mix
  in mono — both for listening only (Audio → Listen); a binaural render is
  a render option. IAMF (AOM's Immersive Audio Model and Formats) masters
  of any master — mono, stereo or a bed — as MP4 or `.iamf`, with Opus
  (to stream), FLAC or LPCM and the loudness IAMF carries; checked against
  AOM's reference decoder and FFmpeg. Heads: two dummy heads and eighteen measured listeners
  to choose from by ear, or your own as a SOFA file; headphone correction
  from EqualizerAPO/AutoEq presets or an impulse response.
* Ambisonics: tracks, buses and the master can carry an ambisonic sound
  field, first to third order (AmbiX; File → Import Ambisonic Audio
  (AmbiX)…), with head-tracked binaural listening (a phone's motion
  sensors over OSC turn the scene).

## Mastering and delivery

* Mastering meters (Tools, F12; the Mastering workspace opens on them): EBU
  R128 loudness — integrated, short-term and momentary, loudness range,
  true peak and PLR against a delivery target (−14, −16, −23, −24 or −9
  LUFS) with a short-term history; crest factor and DR value; peak/RMS levels in dBFS or K-12/14/20;
  goniometer and correlation; an FFT spectrum with peak hold — for the
  master or any track.
* Album (bottom dock, next to Tools; plays the album as it will be
  delivered — levelled, limited, with its pauses and crossfades): songs in release order — the
  project's sections, the whole project, other FaderFrame projects or
  finished mixes — with a pause or an equal-power crossfade, trim, fades,
  ISRC, credits and the song's own plugin inserts (heard on the master
  while editing) per song, and the release's title, credits and UPC/EAN;
  offline analysis (integrated loudness, range, true peak) and a preview of
  how each song will be delivered; export with one gain for the album (the
  songs keep their balance) or per-song levelling, true-peak limiting,
  dither, one file per song (cut gaplessly when songs crossfade), the whole
  album with a cue sheet and a CD master for replication: a DDP 2.00
  fileset (44.1 kHz/16-bit, PQ codes, CD-Text in up to eight languages —
  Japanese too — checksums) verified after writing; a DDP player that
  checks any fileset as a plant would (checksums, PQ, Red Book rules,
  every CD-Text language), plays it like a CD player (track, index, the
  pregap counting down) and imports its tracks back as album songs,
  bit for bit; and a vinyl premaster (12″ 33⅓ or 45, 10″, 7″): sides split
  automatically or by hand with their times against the format's limits,
  checks for out-of-phase bass, esses, hard limiting and the inner
  grooves, one continuous 24-bit file per side (gain only, no limiting),
  each song's file and a cutting sheet.
* Render / export to WAV (16/24-bit with TPDF or noise-shaped dither,
  32-bit float): master, stems or one track (File → Render Selected
  Track…), project/loop/bar range, any sample rate, mono, stereo or a
  surround master's every channel, tail, peak normalisation. Delivery:
  loudness normalisation to a target (BS.1770 integrated loudness) with a
  true-peak lookahead limiter, presets for streaming, Apple Music, CD, EBU
  R128 and ATSC A/85. Renders start on time (the graph's latency taken off
  the front). After every render and album export a report measured from
  each file as written — read back from disk at its own rate, channels and
  format — gives its format, length, render time, sample and true peak,
  RMS, integrated loudness, loudness range, the loudest short-term
  loudness, the peaks per channel and whether anything clipped. Every
  export asks before replacing a file, writes beside it and swaps the new
  file in only once it is complete, so a failed or stopped render leaves
  the old one as it was.
* Codec monitor and platform previews (Audio → Listen → Codec Monitor,
  the Tools view's Codec pane): the master as listeners get it, through a
  lossy codec at a streaming platform's loudness — the decoded signal,
  the original, or what the codec changed alone.
* Delivery QC: null-tested stems and a QC report, every file of a render
  read back once — silence at the head and the tail, gaps, peaks, DC
  offset and overs.

## Picture and performance

* Setlist and show mode (View → Setlist): a show's songs in order — one
  per section, or ranges of the project — with what follows each (stop,
  the next song after a gap, play on) and the performer's notes; Show Mode
  is a full-screen stage screen (the song, its time left, the part now and
  next, the lyric line, the notes, the list) where every song stops on its
  last frame (the stop is armed ahead, so a busy machine still stops on
  time), controlled by Space and the arrows or MIDI foot switches.
* Video (File → Import Video…, or drop a movie on the video window): the
  picture on a Video lane above the tracks, a filmstrip of each clip, in
  absolute time (tempo changes never move it) and placed at its own
  timecode; its sound on an audio track of its own in step with it; the
  project takes the movie's frame rate and start timecode (every SMPTE
  rate, 23.976 to 120, drop-frame 29.97 and 59.94) and warns when another
  movie's rate differs. The video window (View → Video; dockable,
  detachable, full screen with a double-click — Escape leaves) shows the
  frame for the sample you hear when that frame reaches the screen: the
  audio engine is the clock, the picture follows locates, loops and
  varispeed, and output latency is taken into account; a timecode overlay
  shows the project's timecode and the frame. Scrubbing and locating are
  exact to the frame: every file is indexed on import (variable frame
  rates and B-frames included) and gets an all-intra proxy in the
  background where it needs one, so once it is made a 4K long-GOP original
  scrubs like an intra-frame file (until then a keyframe shows at once and
  the exact frame follows); stopped, the picture is decoded sharp from the
  original. Drag a clip along the lane (snapped to whole frames), spot it
  to its timecode, move the picture against the sound by frames or
  milliseconds (the window's menu), and check the whole chain with the
  built-in flash-and-beep sync test. Export Movie (File menu) writes the
  movie with the mix next to the picture copied as it is coded — no
  re-encoding: QuickTime or Matroska with PCM, MPEG-4 with Opus — the
  whole movie or a clip's span cut at keyframes, with a timecode track.
  Decoding and muxing are GStreamer's (hardware decoders where the system
  has them: VA-API, Direct3D, VideoToolbox; on Linux decoded frames go to
  the screen without a copy); proxies and indexes go to the computer's
  cache folder (`faderframe/video`), deletable any time. HDR (PQ, HLG) and
  wide-gamut pictures are mapped for an SDR screen; MXF and DV files are
  read (and ProRes, DNxHD and the other intermediates in the Linux
  tarball). For post-production: J/K/L shuttle (reverse and up to 8×,
  heard as scrub snippets) and frame steps, several video tracks compared
  side by side or with a wipe, trims in the lane, timecode counters on the
  transport display, cut detection (the cuts as markers), conform to a new
  cut from EDLs or OpenTimelineIO — or by matching the old and new picture
  shot by shot — and an ADR cue list (View → ADR Cue List: cues from the
  transcript, beeps on a track of their own, a cue rehearsed and recorded
  with pre-roll, punch and a streamer over the picture, takes rated). The
  picture can also play out through a Blackmagic DeckLink card (SDI or
  HDMI) or full screen on a monitor of its own.
* Clip launcher (View → Clip Launcher): scenes of clips per track,
  launched while the song plays on the next bar, beat or bars, looping
  until the next one; whole scenes at once, per-track stops, "Back to
  Arrangement", and "Record to Arrangement" writes what was launched into
  the arrangement. Clips are dragged in from the arrangement (or sent from
  a clip's menu), recorded into an empty slot of an armed track (audio or
  MIDI, playing on in time when the recording ends), or are new MIDI
  clips; they drag between slots (and back into the arrangement, across
  windows too), and follow actions (next, previous, first, last, any,
  other, again, stop, jump to a scene — after the clip or some bars, two
  of them with a chance each) move a track on by itself. Clips launch as
  triggers, gates, toggles or repeats, with their own quantisation,
  legato, and audio that follows the tempo; slots record with a fixed
  length and a count-in, MIDI clips take overdubs, and Record to
  Arrangement writes what plays (and the mixer's moves) as it plays.
  Pads, keys and buttons learn slots, scenes and stops.
* A/V calibration by itself: a camera watches the video window flash and
  a microphone hears the beeps, and the picture offset is set from them
  (not yet tried with a real webcam); panning that follows an object
  tracked on screen.
* 360° video: the video window is a viewport into the sphere, the
  listener's head on headphones turns with it, a click on the picture
  pans a track to the direction it shows; spherical and ambisonic
  metadata kept on export.
* Adaptive music for games (View → Interactive Music): an interactive
  score of states, layers, transition rules and stingers, previewed live
  and exported as looped stems with imports for FMOD and Wwise.
* Gear changes per song in show mode: program changes to external gear
  as each song comes up.

## Sync, control surfaces and remote

* MIDI sync (Preferences → MIDI → Sync): follows MIDI clock (its tempo
  too) or MIDI time code (by varispeed, so the clocks drifting apart never
  makes it jump), and sends MIDI clock and MIDI time code (24, 25, 29.97
  drop or 30 fps from the project's start time).
* Control surfaces (Preferences → MIDI → Control Surfaces): Mackie Control
  and its extenders, HUI, and OSC (e.g. TouchOSC) — motorised faders,
  pan and send pots, Flip, mute/solo/arm/select, automation modes, names,
  levels and meters on the displays, the transport and the song position
  (bars and beats or timecode), banks of eight (shared or a surface's own),
  channel-strip pages for the selected track's parameters, devices, EQ
  and instrument, track-type filters, F-keys for workspaces, modifiers,
  zoom/nudge/scrub/shuttle, punch, edit keys and HUI's numeric keypad;
  over OSC also the clip launcher, shown and played. Grid controllers
  play the clip launcher in the clips' colours: Launchpad Mini MK3, X and
  Pro MK3, APC mini and mk2, Push 2. Keyboards and surfaces with a DAW
  mode are known by name (Preferences → MIDI → Find Controllers): Arturia
  KeyLab, Novation Launchkey, Native Instruments Komplete Kontrol, Akai,
  Korg and M-Audio controllers and the PreSonus FaderPort (its navigator
  modes and Shift functions) — only what their makers or open integrations
  document. In a cue mix, faders, pans and mutes follow the mixer and set
  the cue sends.
* Phone and web remote (Transport → Phone and Web Remote…, Preferences →
  Remote; off until you turn it on): the transport, the mixer, cue mixes,
  the setlist and show mode, the lyrics as a teleprompter, the clip
  launcher, mix scenes and macros in a phone's or tablet's browser on the
  same network — nothing to install. A device pairs once with a six-digit
  code (typed, or the QR code scanned) and gets a role: full control, a
  performer's own cue mix, or reading the lyrics and setlist. Only devices
  on the local network are answered; there is no encryption, so keep it
  to networks you trust. When the computer's firewall blocks the port,
  Preferences → Remote says so and gives the command that opens it to
  your network.

## Interchange and collaboration

* DAWproject (File → Export DAWproject… and Import DAWproject…):
  projects to and from other programs in Bitwig's open format; MusicXML
  scores imported as tracks.
* AAF export and import for Pro Tools, Media Composer, Premiere and
  Resolve (OMF is not read).
* Live collaboration (File → Collaborate…): one project edited by several
  people at once over an encrypted link, on a local network or the
  internet; undo per person, media fetched by content hash, Listen
  Along.

## Engine and audio

* Engine: routing graph with cycle detection and plugin delay compensation
  through every summing point and output (a latency change while playing
  dips instead of jumping; low-latency monitoring bypasses latent effects
  on the way out of a monitored track), buses, auxes, sends, sidechains,
  solo-in-place, sample-accurate loops, realtime-safe (verified by
  allocation-counting tests over every device, modulator and container).
  Keeping the deadline beyond allocation: memory the audio path touches is
  mapped up front (and locked where allowed), so no page faults on the
  audio thread; helper threads run at priorities just under it (through
  RealtimeKit or the desktop portal where the system grants no realtime
  rights, a thread past its time limit demoted instead of the program
  killed); no helper is waited for without a deadline; Panic (Transport
  menu) resets every processor without a burst.
* Audio: native PipeWire (one node with a port per channel, linked to the
  devices you choose or following the default ones), JACK (JACK2 or
  PipeWire-JACK), ALSA, WASAPI and ASIO (Windows; ASIO in builds with the
  `asio` feature, see below), CoreAudio (macOS) and a silent dummy device;
  *Automatic* picks PipeWire, then JACK, then ALSA on Linux, and ASIO
  before WASAPI where it is built in. All common sample rates (44.1 – 192
  kHz) and buffer sizes (16 – 8192 frames). Preferences → Audio (also
  Audio → Audio Settings…) lists the audio system's devices and offers only
  what it allows: the output and input device and, through PipeWire or
  JACK, a second device to play on or record from (kept in step by
  PipeWire), the rate, the buffer size, the channel counts; Apply says
  beforehand whether it applies live (a buffer size on JACK or PipeWire)
  or reopens the device. Below it, what runs: the stream, the latencies,
  where each channel goes. A device that goes away is reopened as soon as
  it is back — or the default after two seconds, moving back to the chosen
  one when it returns (never while recording).
* Multicore engine: tracks, buses and plugins are processed in parallel on
  all cores (critical path first, realtime priority, flush-to-zero), with
  output bit-identical to single-threaded processing; Preferences → Audio
  → Processing threads.
* Render ahead (anticipative processing, on by default): tracks nobody
  plays live are rendered 200 ms ahead of the playhead on threads of their
  own, so heavy plugin chains need not finish within one tiny buffer —
  armed and live tracks, tracks with a plugin editor open, faders and
  sends stay immediate. With 64 tracks of six effects at 64-frame buffers
  the audio thread's worst callback went from 1.3 ms to 81 µs. Buses,
  auxes and the master's devices render ahead too when everything reaching
  them can, in a second stage only a buffer and a few milliseconds ahead,
  so the faders feeding them still answer at once (about 10 ms).
* Performance meter (F8, or click the DSP readout in the status bar): total
  DSP load with a 60 s history and a breakdown into plugins, mixing and
  engine work; load per track and per plugin instance (average and peak,
  sortable, latency and bypass shown); xruns, late callbacks and late disk
  reads.
* Several audio interfaces joined: on PipeWire one FaderFrame node
  linked to several devices, those with a clock of their own resampled
  adaptively so they stay in step.
* AES67 network audio: PTP-clocked RTP streams, as an audio system of its
  own or beside another device (Dante in its AES67 mode, RAVENNA).
