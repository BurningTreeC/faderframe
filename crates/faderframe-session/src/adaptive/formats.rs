//! The interactive score's metadata, beside its stems:
//!
//! * `<name>.score.json` — the manifest (schema
//!   `faderframe.interactive-score/1`): states with their files, lengths,
//!   entry/exit/loop frames, cues, markers, tempo, metre and every beat;
//!   layers and their tracks; parameters and their layer curves; the
//!   rules as written and the transition each pair of states takes;
//!   stingers.
//! * `<name>.fmod.js` — a script for FMOD Studio 2.02 and later (its
//!   Scripting API: `studio.project.workspace.addEvent`,
//!   `Event.addGameParameter/addGroupTrack/addMarkerTrack`,
//!   `GroupTrack.addSound`, `MarkerTrack.addNamedMarker/addRegion/
//!   addTransitionRegion/addTransitionMarker`,
//!   `Triggerable.addParameterCondition`, `AutomatableObject.addAutomator`,
//!   `Automator.addAutomationCurve`, `AutomationCurve.addAutomationPoint`,
//!   `project.importAudioFile`, `studio.menu.addMenuItem`). Put into the
//!   FMOD project's `Scripts` folder it adds "Scripts › FaderFrame ›
//!   Import Interactive Score: <name>", which makes one event: a labelled
//!   game parameter "<name> State" (a label per state), the score's
//!   parameters, a group track per layer with the states' stems one after
//!   another on the timeline, a destination marker at each state's start
//!   and entry, a loop region over each loop, transition regions (beat,
//!   bar, grid, immediate: their quantisation is left at FMOD's own and
//!   named in the manifest — the scripting reference documents no
//!   property for it) or transition markers (markers, cues, the exit)
//!   with a condition on the state parameter, volume automation of the
//!   layers by the parameters, and an event per stinger.
//! * `<name>.wwise.tsv` — Wwise's tab-delimited list for the Audio File
//!   Importer (Import Tab Delimited), any version with it: every stem as a
//!   Music Track of a Music Segment in a Music Playlist Container per
//!   state under a Music Switch Container.
//! * `<name>.waapi.json` (+ `<name>.waapi.py`, which runs it with
//!   Audiokinetic's `waapi-client`) — WAAPI calls for Wwise 2022.1 and
//!   later (`ak.wwise.core.object.set`): a State Group with a State per
//!   state, Game Parameters, Triggers for the stingers; the Music Switch
//!   Container (switched by the state group) with its playlists
//!   (looping or once), segments (tempo, metre, entry/exit/custom cues;
//!   stems imported as music tracks), the layers' Volume RTPC curves, the
//!   transition list (`MusicTransition`: source/destination, Exit Source
//!   At, Destination Sync To, pre-entry; most specific last — Wwise reads
//!   the list from the bottom up) and the stingers (`MusicStinger`). Fades
//!   and transition segments are written into each transition's notes
//!   (their objects are not in WAAPI's documented properties).

use super::export::ExportData;
use faderframe_core::MusicStateId;
use faderframe_project::interactive::{
    Entry, FLOOR_DB, InteractiveScore, RuleEnd, SyncPoint, TransitionRule,
};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The manifest's schema.
pub const SCHEMA: &str = "faderframe.interactive-score/1";

/// Seconds between states on FMOD's timeline.
const FMOD_GAP: f64 = 2.0;

/// The quietest FMOD's volume goes (dB).
const FMOD_FLOOR_DB: f32 = -80.0;

/// Write every metadata file; their paths (the manifest first).
pub fn write_all(score: &InteractiveScore, data: &ExportData) -> Result<Vec<PathBuf>, String> {
    let base = super::export::file_stem(&data.name);
    let files = [
        (format!("{base}.score.json"), pretty(&manifest(score, data))),
        (format!("{base}.fmod.js"), fmod_script(score, data)),
        (format!("{base}.wwise.tsv"), wwise_tsv(data)),
        (format!("{base}.waapi.json"), pretty(&waapi(score, data))),
        (format!("{base}.waapi.py"), waapi_runner(&base)),
    ];
    let mut out = Vec::new();
    for (name, text) in files {
        let path = data.folder.join(name);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push(path);
    }
    Ok(out)
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default() + "\n"
}

fn name_of(score: &InteractiveScore, id: MusicStateId) -> String {
    score
        .state(id)
        .map_or_else(|| "?".into(), |s| s.name.clone())
}

fn end_name(score: &InteractiveScore, end: RuleEnd) -> Value {
    match end.state() {
        Some(id) => json!(name_of(score, id)),
        None => json!("*"),
    }
}

fn sync_json(sync: &SyncPoint) -> Value {
    let mut v = serde_json::to_value(sync).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("label".into(), json!(sync.label()));
    }
    v
}

fn entry_name(e: Entry) -> &'static str {
    match e {
        Entry::Start => "start",
        Entry::EntryCue => "entry_cue",
        Entry::SameTime => "same_time",
    }
}

/// The JSON manifest (see the module docs).
pub fn manifest(score: &InteractiveScore, data: &ExportData) -> Value {
    let rate = f64::from(data.sample_rate.max(1));
    let states: Vec<Value> = data
        .states
        .iter()
        .map(|s| {
            json!({
                "id": s.id.raw(),
                "name": s.name,
                "kind": s.kind,
                "source": s.source,
                "looping": s.looping,
                "then": s.then,
                "length_frames": s.length_frames,
                "file_frames": s.file_frames,
                "entry_frame": s.entry_frame,
                "exit_frame": s.exit_frame,
                "loop": if s.looping { json!({"start": s.entry_frame, "end": s.exit_frame}) } else { Value::Null },
                "tempo": {"bpm": s.bpm, "signature": [s.signature.0, s.signature.1]},
                "tempo_changes": s.tempo_changes.iter().map(|(f, b)| json!({"frame": f, "bpm": b})).collect::<Vec<_>>(),
                "cues": s.cues.iter().map(|(n, f)| json!({"name": n, "frame": f})).collect::<Vec<_>>(),
                "markers": s.markers.iter().map(|(n, f)| json!({"name": n, "frame": f})).collect::<Vec<_>>(),
                "beats": s.beats.iter().map(|(f, bar, beat)| json!({"frame": f, "bar": bar, "beat": beat})).collect::<Vec<_>>(),
                "stems": s.stems.iter().map(|(l, f)| json!({"layer": l, "file": f})).collect::<Vec<_>>(),
            })
        })
        .collect();
    let layers: Vec<Value> = data
        .layers
        .iter()
        .map(|l| {
            json!({
                "name": l.name,
                "base": l.layer.is_none(),
                "tracks": l.tracks.iter().map(|t| data.track_name(*t)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let parameters: Vec<Value> = score
        .parameters
        .iter()
        .map(|p| {
            json!({
                "id": p.id.raw(),
                "name": p.name,
                "min": p.min,
                "max": p.max,
                "default": p.default,
                "curves": p.curves.iter().filter_map(|c| {
                    let layer = score.layer(c.layer)?;
                    Some(json!({
                        "layer": layer.name,
                        "interpolation": "amplitude",
                        "points": c.points.iter().map(|pt| json!({"value": pt.value, "db": pt.db})).collect::<Vec<_>>(),
                    }))
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    let fade = |f: faderframe_project::scenes::FadeLength, bpm: f64| json!({"label": f.label(), "seconds": f.seconds(bpm)});
    let rules: Vec<Value> = score
        .rules
        .iter()
        .map(|r| {
            let bpm = r
                .from
                .state()
                .and_then(|s| data.state(s))
                .map_or(120.0, |s| s.bpm);
            json!({
                "id": r.id.raw(),
                "from": end_name(score, r.from),
                "to": end_name(score, r.to),
                "sync": sync_json(&r.sync),
                "fade_out": fade(r.fade_out, bpm),
                "fade_in": fade(r.fade_in, bpm),
                "segment": r.segment.map(|g| name_of(score, g)),
                "entry": entry_name(r.entry),
            })
        })
        .collect();
    // Each pair's transition as the rules resolve it.
    let mut transitions = Vec::new();
    for from in &data.states {
        for to in &data.states {
            if from.id == to.id {
                continue;
            }
            let r = score.rule_between(Some(from.id), to.id);
            transitions.push(json!({
                "from": from.name,
                "to": to.name,
                "rule": if r.id.raw() == 0 { Value::Null } else { json!(r.id.raw()) },
                "sync": sync_json(&r.sync),
                "fade_out_seconds": r.fade_out.seconds(from.bpm),
                "fade_in_seconds": r.fade_in.seconds(from.bpm),
                "segment": r.segment.map(|g| name_of(score, g)),
                "entry": entry_name(r.entry),
            }));
        }
    }
    let stingers: Vec<Value> = data
        .stingers
        .iter()
        .map(|s| {
            json!({
                "id": s.id.raw(),
                "name": s.name,
                "file": s.file,
                "length_frames": s.length_frames,
                "file_frames": s.file_frames,
                "pickup_frames": s.pickup_frames,
                "sync": sync_json(&s.sync),
            })
        })
        .collect();
    let base = super::export::file_stem(&data.name);
    json!({
        "schema": SCHEMA,
        "generator": format!("FaderFrame {}", env!("CARGO_PKG_VERSION")),
        "name": data.name,
        "sample_rate": data.sample_rate,
        "bits": data.bits,
        "channels": data.channels,
        "seconds_per_frame": 1.0 / rate,
        "start_state": score.first_state().map(|s| name_of(score, s)),
        "states": states,
        "layers": layers,
        "parameters": parameters,
        "rules": rules,
        "transitions": transitions,
        "stingers": stingers,
        "files": {
            "fmod": format!("{base}.fmod.js"),
            "wwise_importer": format!("{base}.wwise.tsv"),
            "waapi": format!("{base}.waapi.json"),
        },
    })
}

/// Where each state starts on FMOD's timeline (seconds).
fn fmod_places(data: &ExportData) -> Vec<f64> {
    let rate = f64::from(data.sample_rate.max(1));
    let mut at = 0.0;
    data.states
        .iter()
        .map(|s| {
            let here = at;
            at += s.file_frames as f64 / rate + FMOD_GAP;
            here
        })
        .collect()
}

/// The FMOD Studio script (see the module docs): fixed code over the score
/// as a JavaScript object.
pub fn fmod_script(score: &InteractiveScore, data: &ExportData) -> String {
    let rate = f64::from(data.sample_rate.max(1));
    let sec = |f: u64| f as f64 / rate;
    let places = fmod_places(data);
    let place_of = |id: MusicStateId| {
        data.states
            .iter()
            .position(|s| s.id == id)
            .map(|i| (&data.states[i], places[i]))
    };
    let mut markers = Vec::new();
    let mut states = Vec::new();
    for (s, place) in data.states.iter().zip(&places) {
        markers.push(json!({"name": s.name, "at": place}));
        markers
            .push(json!({"name": format!("{} Entry", s.name), "at": place + sec(s.entry_frame)}));
        states.push(json!({
            "name": s.name,
            "place": place,
            "fileSeconds": sec(s.file_frames),
            "loop": if s.looping { json!([sec(s.entry_frame), sec(s.exit_frame)]) } else { Value::Null },
            "stems": s.stems.iter().map(|(l, f)| json!({"layer": l, "file": f})).collect::<Vec<_>>(),
        }));
    }
    // Where a transition can leave a state, and how.
    let leave = |s: &super::export::ExportedState, place: f64, sync: &SyncPoint| -> Vec<Value> {
        let start = if s.looping { s.entry_frame } else { 0 };
        let exit = place + sec(s.exit_frame) - 0.001;
        let at_frames = |frames: Vec<u64>| -> Vec<Value> {
            frames
                .into_iter()
                .filter(|f| *f >= start && *f < s.exit_frame)
                .map(|f| json!({"kind": "marker", "at": place + sec(f)}))
                .collect()
        };
        let found = match sync {
            SyncPoint::Marker => at_frames(s.markers.iter().map(|(_, f)| *f).collect()),
            SyncPoint::Cue { name } => at_frames(
                s.cues
                    .iter()
                    .filter(|(n, _)| name.is_empty() || n.eq_ignore_ascii_case(name))
                    .map(|(_, f)| *f)
                    .collect(),
            ),
            SyncPoint::Exit => Vec::new(),
            _ => vec![json!({
                "kind": "region",
                "at": place + sec(start),
                "length": sec(s.exit_frame.saturating_sub(start)),
            })],
        };
        if found.is_empty() {
            vec![json!({"kind": "marker", "at": exit})]
        } else {
            found
        }
    };
    let mut transitions = Vec::new();
    let mut segment_ends: Vec<(MusicStateId, MusicStateId, String)> = Vec::new();
    for (s, place) in data.states.iter().zip(&places) {
        for d in &data.states {
            if d.id == s.id {
                continue;
            }
            let r = score.rule_between(Some(s.id), d.id);
            let dest = match r.entry {
                Entry::Start => d.name.clone(),
                Entry::EntryCue | Entry::SameTime => format!("{} Entry", d.name),
            };
            let target = match r.segment.and_then(place_of) {
                Some((g, _)) => {
                    segment_ends.push((g.id, d.id, dest.clone()));
                    g.name.clone()
                }
                None => dest,
            };
            for mut t in leave(s, *place, &r.sync) {
                if let Some(o) = t.as_object_mut() {
                    o.insert("destination".into(), json!(target));
                    o.insert("condition".into(), json!(d.name));
                    o.insert("sync".into(), json!(r.sync.label()));
                }
                transitions.push(t);
            }
        }
        // A state that plays once goes on by itself.
        if !s.looping
            && let Some(then) = score.state(s.id).and_then(|x| x.then)
            && let Some((t, _)) = place_of(then)
        {
            transitions.push(json!({
                "kind": "marker",
                "at": place + sec(s.exit_frame) - 0.001,
                "destination": t.name,
                "condition": Value::Null,
                "sync": "end",
            }));
        }
    }
    // Transition segments go on to the destination at their end.
    segment_ends.sort_by_key(|a| (a.0, a.1));
    segment_ends.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    for (g, d, dest) in segment_ends {
        if let (Some((gs, gplace)), Some((ds, _))) = (place_of(g), place_of(d)) {
            transitions.push(json!({
                "kind": "marker",
                "at": gplace + sec(gs.exit_frame) - 0.001,
                "destination": dest,
                "condition": ds.name,
                "sync": "end of the transition segment",
            }));
        }
    }
    let layers: Vec<&str> = data.layers.iter().map(|l| l.name.as_str()).collect();
    let parameters: Vec<Value> = score
        .parameters
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "min": p.min,
                "max": p.max,
                "curves": p.curves.iter().filter_map(|c| {
                    let layer = score.layer(c.layer)?;
                    // Only layers that have a group track.
                    if !layers.contains(&layer.name.as_str()) {
                        return None;
                    }
                    Some(json!({
                        "layer": layer.name,
                        "points": c.sampled(8).iter().map(|pt| json!([pt.value, if pt.db <= FLOOR_DB { FMOD_FLOOR_DB } else { pt.db.max(FMOD_FLOOR_DB) }])).collect::<Vec<_>>(),
                    }))
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    let stingers: Vec<Value> = data
        .stingers
        .iter()
        .map(|s| json!({"name": s.name, "file": s.file, "seconds": sec(s.file_frames)}))
        .collect();
    let folder = data.folder.to_string_lossy().replace('\\', "/");
    let score_js = json!({
        "event": data.name,
        "folder": if folder.ends_with('/') { folder } else { format!("{folder}/") },
        "stateParameter": format!("{} State", data.name),
        "states": states,
        "layers": layers,
        "markers": markers,
        "transitions": transitions,
        "parameters": parameters,
        "stingers": stingers,
    });
    let data_text = serde_json::to_string_pretty(&score_js).unwrap_or_else(|_| "{}".into());
    format!(
        r#"// FaderFrame interactive score "{name}" for FMOD Studio 2.02 or later
// (FMOD Studio's JavaScript Scripting API).
//
// Put this file into the FMOD Studio project's "Scripts" folder (or use
// Scripts > Reload) and choose Scripts > FaderFrame > Import Interactive
// Score: {name}. It makes one event: a labelled game parameter
// "{name} State" (a label per state), the score's parameters, a group
// track per layer holding the states' stems one after another, a
// destination marker at each state's start and entry, a loop region over
// each loop, transition regions and markers with a condition on the state
// parameter, the layers' volume automated by the parameters, and an event
// per stinger. The audio files are imported from SCORE.folder: change it
// if the export moved. Transition regions keep FMOD's own quantisation:
// set it as each region's name in the manifest says (the sync point).

(function () {{
    var SCORE = {data_text};

    // A game parameter as the scripting API asks for it in conditions and
    // automation curves (an event's parameter, or its preset's).
    function gameParameterOf(p) {{
        if (p && p.preset && p.preset.parameter) {{
            return p.preset.parameter;
        }}
        if (p && p.parameter) {{
            return p.parameter;
        }}
        return p;
    }}

    function importScore() {{
        var event = studio.project.workspace.addEvent(SCORE.event, false);
        var labels = SCORE.states.map(function (s) {{
            return s.name;
        }});
        var stateParameter = event.addGameParameter({{
            name: SCORE.stateParameter,
            type: studio.project.parameterType.UserEnumeration,
            min: 0,
            max: Math.max(labels.length - 1, 0),
            enumerationLabels: labels
        }});
        var parameters = {{}};
        SCORE.parameters.forEach(function (p) {{
            parameters[p.name] = event.addGameParameter({{
                name: p.name,
                type: studio.project.parameterType.User,
                min: p.min,
                max: p.max
            }});
        }});
        var tracks = {{}};
        SCORE.layers.forEach(function (name) {{
            tracks[name] = event.addGroupTrack(name);
        }});
        var markerTrack = event.addMarkerTrack();
        var markers = {{}};
        SCORE.markers.forEach(function (m) {{
            markers[m.name] = markerTrack.addNamedMarker(m.name, m.at);
        }});
        SCORE.states.forEach(function (s) {{
            s.stems.forEach(function (stem) {{
                var file = studio.project.importAudioFile(SCORE.folder + stem.file);
                if (!file) {{
                    throw new Error("FaderFrame: could not import " + SCORE.folder + stem.file);
                }}
                var sound = tracks[stem.layer].addSound(event.timeline, "SingleSound", s.place, s.fileSeconds);
                sound.audioFile = file;
            }});
            if (s.loop !== null) {{
                markerTrack.addRegion(s.place + s.loop[0], s.loop[1] - s.loop[0], s.name + " Loop",
                    studio.project.regionLoopMode.Looping);
            }}
        }});
        SCORE.transitions.forEach(function (t) {{
            var destination = markers[t.destination];
            var logic = t.kind === "region"
                ? markerTrack.addTransitionRegion(t.at, t.length, destination)
                : markerTrack.addTransitionMarker(t.at, destination);
            if (t.condition !== null) {{
                logic.addParameterCondition(gameParameterOf(stateParameter), t.condition);
            }}
        }});
        SCORE.parameters.forEach(function (p) {{
            p.curves.forEach(function (c) {{
                var automator = tracks[c.layer].mixerGroup.addAutomator("volume");
                var curve = automator.addAutomationCurve(gameParameterOf(parameters[p.name]));
                c.points.forEach(function (point) {{
                    curve.addAutomationPoint(point[0], point[1]);
                }});
            }});
        }});
        SCORE.stingers.forEach(function (s) {{
            var stinger = studio.project.workspace.addEvent(SCORE.event + " Stinger " + s.name, false);
            var file = studio.project.importAudioFile(SCORE.folder + s.file);
            var sound = stinger.addGroupTrack("Stinger").addSound(stinger.timeline, "SingleSound", 0, s.seconds);
            sound.audioFile = file;
        }});
        alert("FaderFrame: the event \"" + SCORE.event + "\" is made: " + SCORE.states.length +
            " states, " + SCORE.transitions.length + " transitions, " + SCORE.stingers.length + " stingers.");
    }}

    studio.menu.addMenuItem({{
        name: "FaderFrame\\Import Interactive Score: " + SCORE.event,
        execute: importScore
    }});
}})();
"#,
        name = data.name.replace(['\n', '\r'], " "),
        data_text = data_text,
    )
}

/// A name Wwise takes (no path separators).
fn wwise_name(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | '\t' | '\n' | '\r') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let n = n.trim().to_string();
    if n.is_empty() { "Untitled".into() } else { n }
}

const WWISE_ROOT: &str = "\\Interactive Music Hierarchy\\Default Work Unit";

/// Wwise's tab-delimited importer list (see the module docs).
pub fn wwise_tsv(data: &ExportData) -> String {
    let mut out = String::from("Audio File\tObject Path\tObject Type\tNotes\n");
    let score = wwise_name(&data.name);
    for s in &data.states {
        let state = wwise_name(&s.name);
        let notes = format!(
            "{} · loop {}–{} frames",
            s.source, s.entry_frame, s.exit_frame
        );
        for (layer, file) in &s.stems {
            let path = data.folder.join(file);
            out.push_str(&format!(
                "{}\t{WWISE_ROOT}\\<Music Switch Container>{score}\\<Music Playlist Container>{state}\\<Music Segment>{state}\\<Music Track>{}\t\t{notes}\n",
                path.display(),
                wwise_name(layer),
            ));
        }
    }
    for st in &data.stingers {
        let path = data.folder.join(&st.file);
        out.push_str(&format!(
            "{}\t{WWISE_ROOT}\\<Music Switch Container>{score}\\<Music Segment>Stinger {}\\<Music Track>Stinger\t\tstinger\n",
            path.display(),
            wwise_name(&st.name),
        ));
    }
    out
}

/// Wwise's "Exit Source At" for a sync point (and the custom cue's name).
fn exit_source_at(sync: &SyncPoint) -> (i64, Option<String>) {
    match sync {
        SyncPoint::Immediate => (0, None),
        SyncPoint::Grid { .. } => (1, None),
        SyncPoint::Bar => (2, None),
        SyncPoint::Beat => (3, None),
        // Project markers are custom cues of the segments.
        SyncPoint::Marker => (4, None),
        SyncPoint::Cue { name } if name.is_empty() => (4, None),
        SyncPoint::Cue { name } => (5, Some(name.clone())),
        SyncPoint::Exit => (7, None),
    }
}

/// Wwise's "Play Segment At" for a stinger's sync point.
fn play_segment_at(sync: &SyncPoint) -> (i64, Option<String>) {
    match sync {
        SyncPoint::Immediate => (0, None),
        SyncPoint::Grid { .. } => (1, None),
        SyncPoint::Bar => (2, None),
        SyncPoint::Beat => (3, None),
        SyncPoint::Marker => (4, None),
        SyncPoint::Cue { name } if name.is_empty() => (4, None),
        SyncPoint::Cue { name } => (5, Some(name.clone())),
        SyncPoint::Exit => (7, None),
    }
}

/// The WAAPI calls (see the module docs).
pub fn waapi(score: &InteractiveScore, data: &ExportData) -> Value {
    let rate = f64::from(data.sample_rate.max(1));
    let ms = |f: u64| f as f64 * 1000.0 / rate;
    let name = wwise_name(&data.name);
    let group = format!("{name} State");
    let group_path = format!("\\States\\Default Work Unit\\{group}");
    let container = format!("{WWISE_ROOT}\\{name}");
    let playlist = |s: &str| format!("{container}\\{}", wwise_name(s));
    let segment = |s: &str| format!("{container}\\{}\\{}", wwise_name(s), wwise_name(s));
    // 1. States, game parameters, triggers.
    let states: Vec<Value> = data
        .states
        .iter()
        .map(|s| json!({"type": "State", "name": wwise_name(&s.name)}))
        .collect();
    let parameters: Vec<Value> = score
        .parameters
        .iter()
        .map(|p| {
            json!({
                "type": "GameParameter",
                "name": wwise_name(&p.name),
                "@Min": p.min,
                "@Max": p.max,
                "@InitialValue": p.default,
            })
        })
        .collect();
    let triggers: Vec<Value> = data
        .stingers
        .iter()
        .map(|s| json!({"type": "Trigger", "name": wwise_name(&s.name)}))
        .collect();
    let mut first = vec![json!({
        "object": "\\States\\Default Work Unit",
        "children": [{"type": "StateGroup", "name": group, "children": states}],
    })];
    if !parameters.is_empty() {
        first.push(
            json!({"object": "\\Game Parameters\\Default Work Unit", "children": parameters}),
        );
    }
    if !triggers.is_empty() {
        first.push(json!({"object": "\\Triggers\\Default Work Unit", "children": triggers}));
    }
    // 2. The music hierarchy.
    let mut children = Vec::new();
    for s in &data.states {
        let mut cues = vec![
            json!({"type": "MusicCue", "name": "Entry", "@CueType": 0, "@TimeMs": ms(s.entry_frame)}),
            json!({"type": "MusicCue", "name": "Exit", "@CueType": 1, "@TimeMs": ms(s.exit_frame)}),
        ];
        for (n, f) in s.cues.iter().chain(&s.markers) {
            cues.push(json!({"type": "MusicCue", "name": wwise_name(n), "@CueType": 2, "@TimeMs": ms(*f)}));
        }
        let tracks: Vec<Value> = s
            .stems
            .iter()
            .map(|(layer, file)| {
                json!({
                    "type": "MusicTrack",
                    "name": wwise_name(layer),
                    "import": {"files": [{"audioFile": data.folder.join(file).to_string_lossy()}]},
                })
            })
            .collect();
        children.push(json!({
            "type": "MusicPlaylistContainer",
            "name": wwise_name(&s.name),
            "notes": format!("{} · from FaderFrame", s.source),
            "@PlaylistRoot": {
                "type": "MusicPlaylistItem",
                "name": "",
                "@LoopCount": if s.looping { 0 } else { 1 },
                "children": [{
                    "type": "MusicPlaylistItem",
                    "name": "",
                    "@Segment": segment(&s.name),
                    "@PlaylistItemType": 1,
                }],
            },
            "children": [{
                "type": "MusicSegment",
                "name": wwise_name(&s.name),
                "@Tempo": s.bpm,
                "@TimeSignatureUpper": s.signature.0,
                "@TimeSignatureLower": s.signature.1,
                "@Cues": cues,
                "children": tracks,
            }],
        }));
    }
    for st in &data.stingers {
        children.push(json!({
            "type": "MusicSegment",
            "name": format!("Stinger {}", wwise_name(&st.name)),
            "@Cues": [
                {"type": "MusicCue", "name": "Entry", "@CueType": 0, "@TimeMs": ms(st.pickup_frames)},
                {"type": "MusicCue", "name": "Exit", "@CueType": 1, "@TimeMs": ms(st.length_frames)},
            ],
            "children": [{
                "type": "MusicTrack",
                "name": "Stinger",
                "import": {"files": [{"audioFile": data.folder.join(&st.file).to_string_lossy()}]},
            }],
        }));
    }
    let entries: Vec<Value> = data
        .states
        .iter()
        .map(|s| {
            json!({
                "type": "MultiSwitchEntry",
                "name": "",
                "@EntryPath": [format!("{group_path}\\{}", wwise_name(&s.name))],
                "@AudioNode": playlist(&s.name),
            })
        })
        .collect();
    let tempo = data.states.first().map_or(120.0, |s| s.bpm);
    let hierarchy = json!({
        "object": WWISE_ROOT,
        "children": [{
            "type": "MusicSwitchContainer",
            "name": name,
            "notes": format!("FaderFrame interactive score, {} states", data.states.len()),
            "@Tempo": tempo,
            "@Arguments": [group_path],
            "@Entries": entries,
            "children": children,
        }],
    });
    // 3. The layers' volume curves.
    let mut rtpcs = Vec::new();
    for p in &score.parameters {
        for c in &p.curves {
            let Some(layer) = score.layer(c.layer) else {
                continue;
            };
            let points: Vec<Value> = c
                .sampled(8)
                .iter()
                .map(|pt| json!({"x": pt.value, "y": pt.db.max(FLOOR_DB), "shape": "Linear"}))
                .collect();
            for s in &data.states {
                if !s.stems.iter().any(|(l, _)| *l == layer.name) {
                    continue;
                }
                rtpcs.push(json!({
                    "object": format!("{}\\{}", segment(&s.name), wwise_name(&layer.name)),
                    "@RTPC": [{
                        "type": "RTPC",
                        "name": "",
                        "@Curve": {"type": "Curve", "points": points},
                        "@PropertyName": "Volume",
                        "@ControlInput": format!("\\Game Parameters\\Default Work Unit\\{}", wwise_name(&p.name)),
                    }],
                }));
            }
        }
    }
    // 4. Transitions (least specific first: Wwise reads from the bottom)
    // and stingers.
    let mut rules: Vec<(usize, &TransitionRule)> = score.rules.iter().enumerate().collect();
    let rank = |r: &TransitionRule| match (r.from, r.to) {
        (RuleEnd::State { .. }, RuleEnd::State { .. }) => 3,
        (RuleEnd::State { .. }, RuleEnd::Any) => 2,
        (RuleEnd::Any, RuleEnd::State { .. }) => 1,
        (RuleEnd::Any, RuleEnd::Any) => 0,
    };
    // Equals: the first of ours wins, so it goes lowest.
    rules.sort_by(|(ia, a), (ib, b)| rank(a).cmp(&rank(b)).then(ib.cmp(ia)));
    let mut transitions = vec![transition_json(
        &TransitionRule::fallback(),
        score,
        &playlist,
        None,
    )];
    for (_, r) in rules {
        let segment_name = r.segment.map(|g| name_of(score, g));
        transitions.push(transition_json(r, score, &playlist, segment_name));
    }
    let stingers: Vec<Value> = data
        .stingers
        .iter()
        .map(|s| {
            let (at, cue) = play_segment_at(&s.sync);
            let mut v = json!({
                "type": "MusicStinger",
                "name": "",
                "@Trigger": format!("\\Triggers\\Default Work Unit\\{}", wwise_name(&s.name)),
                "@Segment": format!("{container}\\Stinger {}", wwise_name(&s.name)),
                "@PlaySegmentAt": at,
            });
            if let (Some(o), Some(cue)) = (v.as_object_mut(), cue) {
                o.insert("@CustomCueMatchName".into(), json!(cue));
            }
            v
        })
        .collect();
    let mut last = json!({
        "object": container,
        "@TransitionRoot": {
            "type": "MusicTransition",
            "name": "",
            "children": transitions,
        },
    });
    if !stingers.is_empty()
        && let Some(o) = last.as_object_mut()
    {
        o.insert("@Stingers".into(), json!(stingers));
    }
    let mut calls = vec![
        json!({
            "what": "state group, game parameters, triggers",
            "uri": "ak.wwise.core.object.set",
            "args": {"objects": first, "onNameConflict": "merge", "listMode": "replaceAll"},
        }),
        json!({
            "what": "the music switch container, playlists, segments and stems",
            "uri": "ak.wwise.core.object.set",
            "args": {"objects": [hierarchy], "onNameConflict": "merge", "listMode": "replaceAll"},
        }),
    ];
    if !rtpcs.is_empty() {
        calls.push(json!({
            "what": "the layers' volume curves",
            "uri": "ak.wwise.core.object.set",
            "args": {"objects": rtpcs, "onNameConflict": "merge", "listMode": "replaceAll"},
        }));
    }
    calls.push(json!({
        "what": "transitions and stingers",
        "uri": "ak.wwise.core.object.set",
        "args": {"objects": [last], "onNameConflict": "merge", "listMode": "replaceAll"},
    }));
    json!({
        "schema": "faderframe.waapi-calls/1",
        "generator": format!("FaderFrame {}", env!("CARGO_PKG_VERSION")),
        "wwise": "2022.1 or later (ak.wwise.core.object.set)",
        "score": data.name,
        "calls": calls,
    })
}

fn transition_json(
    r: &TransitionRule,
    score: &InteractiveScore,
    playlist: &dyn Fn(&str) -> String,
    segment: Option<String>,
) -> Value {
    let mut v = json!({"type": "MusicTransition", "name": ""});
    let Some(o) = v.as_object_mut() else {
        return v;
    };
    match r.from.state() {
        Some(s) => {
            o.insert("@SourceContextType".into(), json!(2));
            o.insert(
                "@SourceContextObject".into(),
                json!(playlist(&name_of(score, s))),
            );
        }
        None => {
            o.insert("@SourceContextType".into(), json!(0));
        }
    }
    match r.to.state() {
        Some(s) => {
            o.insert("@DestinationContextType".into(), json!(2));
            o.insert(
                "@DestinationContextObject".into(),
                json!(playlist(&name_of(score, s))),
            );
        }
        None => {
            o.insert("@DestinationContextType".into(), json!(0));
        }
    }
    let (exit, cue) = exit_source_at(&r.sync);
    o.insert("@ExitSourceAt".into(), json!(exit));
    if let Some(cue) = cue {
        o.insert("@ExitSourceCustomCueMatchName".into(), json!(cue));
    }
    o.insert(
        "@DestinationJumpPositionPreset".into(),
        json!(if r.entry == Entry::SameTime { 1 } else { 0 }),
    );
    o.insert(
        "@PlayDestinationPreEntry".into(),
        json!(r.entry == Entry::Start),
    );
    o.insert("@PlaySourcePostExit".into(), json!(true));
    let mut notes = vec![format!("sync: {}", r.sync.label())];
    if !r.fade_out.is_instant() {
        notes.push(format!("fade out {}", r.fade_out.label()));
    }
    if !r.fade_in.is_instant() {
        notes.push(format!("fade in {}", r.fade_in.label()));
    }
    if let Some(g) = segment {
        notes.push(format!("transition segment ‘{g}’"));
    }
    o.insert("notes".into(), json!(notes.join(" · ")));
    v
}

/// A Python script that runs the WAAPI calls (Audiokinetic's
/// `waapi-client`, not part of FaderFrame).
pub fn waapi_runner(base: &str) -> String {
    format!(
        r#"#!/usr/bin/env python3
"""Run {base}.waapi.json against a running Wwise 2022.1 or later.

Wwise must have the Authoring API on (User Preferences > Enable Wwise
Authoring API). Needs Audiokinetic's client: pip install waapi-client
(not part of FaderFrame). Each call is an ak.wwise.core.object.set; the
script reports each and goes on after one that fails.
"""

import json
import pathlib
import sys

from waapi import WaapiClient


def main():
    here = pathlib.Path(__file__).resolve()
    calls = json.loads(here.with_suffix(".json").read_text(encoding="utf-8"))["calls"]
    client = WaapiClient()
    failed = 0
    try:
        for i, call in enumerate(calls, 1):
            result = client.call(call["uri"], call["args"])
            if result is None:
                failed += 1
                print(f"{{i}}/{{len(calls)}} {{call['what']}}: failed (see Wwise's log)")
            else:
                print(f"{{i}}/{{len(calls)}} {{call['what']}}: done")
    finally:
        client.disconnect()
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
"#
    )
}
