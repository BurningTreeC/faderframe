# Scripting FaderFrame

FaderFrame runs scripts written in [Lua 5.4](https://www.lua.org/manual/5.4/)
(the reference interpreter, MIT licensed, compiled into the program). A
script reads the project and changes it through the same edits the editors
make; everything it changes in one run is **one undo step named after the
script**. Recorded macros are scripts too.

This page is the reference of the `ff` table every script sees. The design
is in docs/ARCHITECTURE.md, "Scripts and macros".

## Where scripts live

| Folder | What | In the Scripts menu |
|---|---|---|
| `<settings>/scripts` (Linux `~/.config/faderframe/scripts`; the portable folder's `Settings/scripts`) | your scripts | Scripts › Scripts |
| `<project>/Scripts` | the project's own scripts (travel with it) | Scripts › Project Scripts |
| `<settings>/scripts/macros` | recorded macros | Scripts › Macros |

Only the `.lua` files at a folder's top are scripts; folders inside hold
modules for `require` (`require("lib.names")` reads `lib/names.lua` from the
project's `Scripts` folder, then your scripts folder). The menu follows the
folders (they are read again every two seconds; Scripts › Reload Scripts
reads them now). Every menu item is in the command palette (Ctrl+Shift+P)
and can get a shortcut in the shortcut editor.

A script may start with lines that say what it is:

```lua
--! name: Tidy Up the Drums
--! description: Colours, names and a folder for a multitrack kit.
--! macro: 12
```

`name` is what the menu shows (else the file's name), `description` its
tooltip, `macro` its number: what a MIDI button or a control surface runs
(see Macros).

## Running

* Scripts › (a script) or the command palette; Scripts › Run Script File…
  for any `.lua` file (its folder is open to it while it runs).
* The **Script Console** (Scripts › Script Console…): type a line of Lua
  and press Enter. An expression's value is printed (`ff.track("Bass")`
  shows the track's table); a statement runs. Variables you set stay for
  the next lines. ↑/↓ bring back earlier lines. Everything scripts `print`
  shows here, and every error with its script and line.
* Scripts › Stop All Scripts and Timers.

A run that fails leaves the project **as it was** (its edits are taken
back) and says where: `Tidy Up.lua:12: no track named 'Drumz'`.

## The sandbox

Scripts cannot run programs or reach the machine:

* Libraries: `string` (without `string.dump`), `table`, `math`, `utf8`,
  `coroutine`, `os.clock`, `os.date`, `os.difftime`, `os.time`; `load`
  compiles text only (never binary chunks); no `dofile`, `loadfile`,
  `debug`, `package`, `os.execute`, `os.exit`, `os.getenv`, `os.remove`,
  `os.rename`, `io.popen`.
* Files: `io.open(path, mode)` and `io.lines(path)` work only inside the
  project's folder (once it is saved) and your scripts folder (a relative
  path is in the project's folder, else the scripts folder; links that lead
  out are refused). `io.write` writes to the console, `io.read` reads
  nothing.
* Time: a run computes at most **10 seconds** (a timer's call, a console
  line, the rest of a script after its prompt was answered: each is a run);
  then it is stopped, whatever `pcall` it is inside. Waiting for a prompt's
  answer never counts.
* Memory: the scripts together hold at most 256 MB.
* Scripts run on the program's interface thread, never the audio thread:
  a long script holds the interface (not the sound) until it ends.
* Each run has its own globals; the `ff` table is read-only.

## Values

* **Positions and lengths** are quarter notes from the project's start
  (bar 1 beat 1 = 0): `ff.bar(5)` is where bar 5 starts; `ff.seconds(q)`
  and `ff.quarters(s)` convert with the tempo map.
* **Levels** are dB, **pan** −1 (left) … 1 (right), **colours** `"#rrggbb"`.
* **Tracks** are named by id (a number), name (a string: exactly, else
  ignoring case; it must be one track's), or a table with `id` (what
  `ff.track` returns). **Clips**, **notes** and **markers** by id (or a
  table with `id`). Where a function takes clips, a list of them works too
  (`ff.selected_clips()` is the selection).
* A track's **devices** by their place among its inserts (1 = the first),
  their name as the mixer shows it (`"EQ 2"`), or a table with `id`.

## Reading

| Function | Returns |
|---|---|
| `ff.version` | the program's version (a string) |
| `ff.script()` | `{ name }` of the script running |
| `ff.project()` | `{ name, path, rate, tempo, signature, length, tracks, saved }` |
| `ff.tracks()` | every track in the editors' order (folders before their contents) |
| `ff.track(t)` | one track or nil: `{ id, name, kind, color, volume, pan, mute, solo, arm, phase, delay, monitor, layout, folder, selected, clips, inserts = { { id, place, name, plugin, format, bypass } … }, sends = { { id, to, level, tap, on } … } }` |
| `ff.clips([t])` | a track's clips by start (all arranger clips without one): `{ id, track, name, start, finish, length, muted, selected, kind = "audio"/"midi"/"takes", gain (audio), notes (MIDI, how many) }` |
| `ff.clip(c)` | one clip or nil |
| `ff.notes(c)` | a MIDI clip's notes by start: `{ id, key, start (in the clip), length, velocity, channel (1–16), muted, selected }` |
| `ff.markers()` | `{ id, name, at }` by place |
| `ff.sections()` | `{ id, name, start, finish }` by place |
| `ff.selection()` | `{ tracks = {ids}, clips = {ids}, notes = {ids}, range = { start, finish } or nil }` |
| `ff.selected_tracks()`, `ff.selected_clips()` | ids (clips: in the arranger, with their folder group) |
| `ff.transport()` | `{ playing, recording, looping, position, seconds, bar, loop = { start, finish, on } or nil }` (the position as heard) |
| `ff.tempo_at(q)` | BPM there |
| `ff.signature_at(q)` | numerator, denominator |
| `ff.bar(n)` / `ff.bar_at(q)` | where bar n starts / the bar a position is in (from 1) |
| `ff.seconds(q)` / `ff.quarters(s)` | through the tempo map |
| `ff.bbt(q)` | `"5.2.480"` |
| `ff.plugins()` | the plugins here: `{ format, id, name, vendor, instrument }` |
| `ff.parameters(t, d)` | a device's parameters: `{ id, name, value, min, max, default }` |
| `ff.scripts()` | the library: `{ key, name, number }` |

## Editing

Every edit goes through the session (one step for the whole run). Edits a
script makes reach exactly what they name: other selected tracks and group
members do not follow (they do when you move a fader).

| Function | Does |
|---|---|
| `ff.add_track(spec)` | a track; `spec` is a kind (`"audio"`, `"instrument"`, `"midi"`, `"bus"`, `"aux"`, `"vca"`, `"folder"`) or `{ kind, name, layout = "mono"/"stereo", index }`; returns its table |
| `ff.remove_track(t)` | (not the master) |
| `ff.move_track(t, index)` | to place `index` (from 1) |
| `ff.set_track(t, fields)` | any of `name`, `volume`, `pan`, `mute`, `solo`, `arm`, `phase`, `delay` (ms), `color`, `monitor` (`"off"`, `"input"`, `"auto"`) |
| `ff.add_midi_clip(t, start, length[, name])` | an empty MIDI clip on an instrument or MIDI track; returns its id |
| `ff.add_note(c, note)` | `note = { key, start, length, velocity, channel, muted }` (start in the clip); returns its id |
| `ff.add_notes(c, notes)` | several at once; returns their ids |
| `ff.set_note(c, id, fields)` / `ff.remove_note(c, id)` | |
| `ff.move_clip(c, { start, track })` | |
| `ff.set_clip(clips, fields)` | `name`, `muted`, `gain` (dB) |
| `ff.remove_clip(clips)` | |
| `ff.split_clip(c, at)` | returns the new clip's id (the part after `at`) |
| `ff.move_clips(clips, by[, lanes])` | by quarters (and lanes down), as a drag: snapping off, rippling as set |
| `ff.trim_clips(clips, "start"/"end", by[, stretch])` | an edge by quarters (`stretch`: time-stretch instead) |
| `ff.clip_gain(clips, delta)` | add dB |
| `ff.add_marker(at[, name])` / `ff.set_marker(id, { name, at })` / `ff.remove_marker(id)` | |
| `ff.add_section(start, finish[, name])` | returns its id |
| `ff.set_tempo(bpm)` | the project's first tempo |
| `ff.set_loop(start, finish)` / `ff.set_loop(nil)` | |
| `ff.select{ tracks, clips, notes, range, mode }` | `range = { start, finish }` or `false`; `mode = "replace"` (default), `"add"`, `"toggle"` |
| `ff.clear_selection()` | |
| `ff.edit(command[, arg])` | the Edit menu on the selection: `separate`, `trim-to-selection`, `clear-range`, `copy-range`, `cut-range`, `paste-range`, `repeat-range` (n), `insert-silence`, `copy`, `cut`, `paste`, `duplicate`, `repeat` (n), `select-all`, `split` (at the playhead), `delete`, `delete-clips`, `remove-tracks`, `arm`, `group`, `quantize`, `humanize`, `merge` (these three: the selected clips, or the clips given), `duplicate-tracks` (with clips unless `false`) |
| `ff.nudge(forward[, "move"/"trim-start"/"trim-end"])` | by the nudge value |
| `ff.insert_plugin(t, plugin[, place])` | `plugin` = a built-in's id (`"faderframe.eq"`) or `{ format = "clap"/"vst3"/"lv2"/"au"/"builtin", id }`; returns the device's id |
| `ff.remove_plugin(t, d)` | |
| `ff.set_plugin(t, d, { bypass, params = { [id] = value } })` | parameter values in their own units |
| `ff.add_send(t, to, { level, tap = "post"/"pre"/"pre-fx" })` | |

## Transport

`ff.play()`, `ff.stop()`, `ff.toggle_play()`, `ff.locate(q)`,
`ff.return_to_start()`, `ff.toggle_loop()`, `ff.record()` (the record
button), `ff.nudge_bars(n)`.

## Rendering

`ff.render(spec)` starts a render in the background and returns its id:

```lua
local job = ff.render{
    file = "Mixes/song.wav",   -- in the project's folder (or the scripts folder)
    range = "project",         -- or "loop", or { start, finish }
    source = "master",         -- or "stems" (file = a folder), or a track
    format = "pcm24",          -- "pcm16", "pcm24", "float"
    rate = 48000, channels = "stereo", tail = 2, normalize = -1,
    overwrite = false,         -- an existing file is never replaced unless true
}
ff.every(0.5, function()
    local st = ff.render_status(job)   -- { done, progress, error, files }
    if st.done then print(st.error or table.concat(st.files, "\n")) return false end
end)
```

Renders write only where scripts may write files. `ff.cancel_render(id)`
stops one.

## Asking and telling

`ff.prompt(spec)` asks and **waits** for the answer (the window shows the
script's name). The script goes on when it comes; what it does after joins
the same undo step, unless you edited something meanwhile. It returns nil
when cancelled.

```lua
local a = ff.prompt{
    title = "Rename the selected tracks",
    text = "A number is added to each name.",
    fields = {
        { label = "Name", default = "Gtr" },
        { label = "First number", default = 1 },          -- a number field
        { label = "Colour", choices = { "#d96c4f", "#4d9ac4" } },
        { label = "Mute them", kind = "check" },
    },
}
if not a then return end
for i, id in ipairs(ff.selected_tracks()) do
    ff.set_track(id, { name = a.Name .. " " .. (a["First number"] + i - 1),
                       color = a.Colour, mute = a["Mute them"] })
end
```

Answers are there by place (`a[1]`) and by label; numbers are numbers,
checks booleans. `ff.prompt("Question?", "default")` asks for one text.
Call it from the script itself (not from a coroutine the script made).

`ff.message(text)` and `ff.warn(text)` show a notice in the status bar.

## Timers

`ff.after(seconds, fn)` calls `fn` once, later; `ff.every(seconds, fn)`
again and again (at least 50 ms apart) until `fn` returns `false`. Both
return an id for `ff.cancel(id)`. Calls happen on the interface thread when
no other script runs and no drag is under way; each call is a run of its
own (an undo step of its own when it edits, named after the script that
set it). At most 64 timers.

## Calling other scripts

`ff.run(name)` runs a script of the library inside the run under way (by
its name, key — `"user/tidy.lua"` — or number): its edits join this run's
step.

## Macros

Scripts › Record Macro starts recording; everything you then do that a
script can do is written as Lua; Scripts › Record Macro again keeps it as
"Macro n" in the macros folder (Scripts › Macros). Playing it is running it:
one undo step.

* A track you make while recording is named by the variable the macro makes
  it in (`local t1 = ff.add_track{ kind = "audio" }`); other tracks by
  name. Clips you acted on that were the selection are written as
  `ff.selected_clips()`, so the macro acts on what is selected when it is
  played.
* A drag records where it ended; a cancelled drag nothing.
* What only changes the view (zoom, layout, meters, opened windows) is left
  out; what a script cannot do is left out and listed at the macro's top
  ("Not recorded: …").
* The macro is a plain Lua file: open the scripts folder and edit it.

Scripts › Macros… lists macros and numbered scripts: Run, Rename…, Delete,
**MIDI Learn** (press a controller's key or button: it runs the macro), and
Shortcuts…. Control surfaces run macros too: Control + F1…F8 on Mackie/HUI
surfaces (macros 1–8), OSC `/macro/run i` or `/macro/N`.

## Examples

Colour every track whose name has "Vox" in it, and group them:

```lua
local voices = {}
for _, t in ipairs(ff.tracks()) do
    if t.name:find("Vox") then
        ff.set_track(t, { color = "#c46aa7" })
        voices[#voices + 1] = t.id
    end
end
ff.select{ tracks = voices }
ff.edit("group")
```

A count-in click as MIDI: a clip with four quarter notes before bar 1 of a
new instrument track:

```lua
local t = ff.add_track{ kind = "instrument", name = "Count" }
local c = ff.add_midi_clip(t, 0, 4, "Count-in")
local notes = {}
for i = 0, 3 do notes[#notes + 1] = { key = i == 0 and 76 or 77, start = i, length = 0.25 } end
ff.add_notes(c, notes)
```

## Development aids

`FADERFRAME_STARTUP_ACTIONS` takes `run-script:<path>`,
`macro:<record|stop|discard|play=<name or number>>` and
`script-line:<lua>` (a console line without commas).
