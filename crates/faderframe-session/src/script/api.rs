//! The `ff` table: what scripts read and do (docs/SCRIPTING.md is its
//! reference). Its functions are made once with the sandbox; each reaches
//! the session through a scoped userdata the run puts in the registry
//! ([`SESSION_KEY`]), so they work only while a script runs. Positions
//! are quarter notes from the project's start (bar 1 = 0), levels dB,
//! times of timers seconds. Tracks are named by id, name or table, clips
//! and markers by id, a track's devices by their place (1 = the first
//! insert), name or table.

use super::{
    FieldKind, MAX_TIMERS, MIN_INTERVAL, Prompt, PromptField, RenderOutcome, ScriptRender, Timer,
};
use crate::editing::{ClipEdge, EditRange, NudgeTarget};
use crate::{Action, SelectMode, Session, TransportAction};
use faderframe_core::{ClipId, MarkerId, NoteId, PluginInstanceId, TrackId};
use faderframe_project::{
    Clip, ClipContent, Command, MidiClip, MidiNote, MonitorMode, MusicalRange, PluginFormat,
    PluginRef, SendTap, Track, TrackColor, TrackKind,
};
use faderframe_script::mlua::{
    self, AnyUserData, FromLuaMulti, Function, IntoLuaMulti, Lua, Table, Value,
};
use faderframe_script::{Sandbox, ScriptError};
use faderframe_timeline::{MusicalTime, TICKS_PER_QUARTER};
use std::time::{Duration, Instant};

/// The registry key of the session a run lends its functions.
pub(crate) const SESSION_KEY: &str = "faderframe.session";

type LResult<T> = mlua::Result<T>;

fn fail(e: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(e.to_string())
}

/// `f` with the session of the run under way.
fn with<R>(lua: &Lua, f: impl FnOnce(&mut Session) -> LResult<R>) -> LResult<R> {
    let outside =
        || fail("the FaderFrame API works only while a script runs (use ff.after for later)");
    let ud: AnyUserData = lua
        .named_registry_value(SESSION_KEY)
        .map_err(|_| outside())?;
    ud.borrow_mut_scoped::<Session, _>(f)
        .map_err(|_| outside())?
}

/// Put `name` into `t`: a function of the session.
fn add<A, R, F>(lua: &Lua, t: &Table, name: &str, f: F) -> LResult<()>
where
    A: FromLuaMulti,
    R: IntoLuaMulti,
    F: Fn(&Lua, &mut Session, A) -> LResult<R> + mlua::MaybeSend + 'static,
{
    t.set(
        name,
        lua.create_function(move |lua, args: A| with(lua, |s| f(lua, s, args)))?,
    )
}

/// Make the `ff` table, frozen, among the sandbox's globals.
pub(crate) fn install(sb: &Sandbox) -> Result<(), ScriptError> {
    let lua = sb.lua();
    let ff = lua.create_table()?;
    ff.set("version", env!("CARGO_PKG_VERSION"))?;
    reading(lua, &ff)?;
    tracks(lua, &ff)?;
    clips(lua, &ff)?;
    timeline(lua, &ff)?;
    selection(lua, &ff)?;
    devices(lua, &ff)?;
    transport(lua, &ff)?;
    rendering(lua, &ff)?;
    talking(lua, &ff)?;
    lua.load(
        r#"
        local ff = ...
        function ff.prompt(spec, default)
            ff._ask(spec, default)
            return coroutine.yield()
        end
        function ff.run(name)
            return ff._script(name)()
        end
        "#,
    )
    .set_name("=ff")
    .call::<()>(ff.clone())?;
    let frozen = sb.freeze(ff, "ff")?;
    sb.set_global("ff", frozen)?;
    Ok(())
}

// --- values ---------------------------------------------------------------

fn at(q: f64) -> MusicalTime {
    MusicalTime::from_quarters(if q.is_finite() { q.max(0.0) } else { 0.0 })
}

fn length(q: f64, what: &str) -> LResult<MusicalTime> {
    if !(q.is_finite() && q > 0.0) {
        return Err(fail(format!("{what} must be more than 0 quarters")));
    }
    Ok(MusicalTime::from_quarters(q))
}

fn quarters(t: MusicalTime) -> f64 {
    t.quarters()
}

fn kind_name(k: TrackKind) -> &'static str {
    match k {
        TrackKind::Audio => "audio",
        TrackKind::Instrument => "instrument",
        TrackKind::Midi => "midi",
        TrackKind::Bus => "bus",
        TrackKind::Aux => "aux",
        TrackKind::Master => "master",
        TrackKind::Vca => "vca",
        TrackKind::Folder => "folder",
    }
}

fn kind_of(name: &str) -> LResult<TrackKind> {
    Ok(match name.to_lowercase().as_str() {
        "audio" => TrackKind::Audio,
        "instrument" => TrackKind::Instrument,
        "midi" => TrackKind::Midi,
        "bus" => TrackKind::Bus,
        "aux" | "return" => TrackKind::Aux,
        "vca" => TrackKind::Vca,
        "folder" => TrackKind::Folder,
        other => {
            return Err(fail(format!(
                "no track kind '{other}' (audio, instrument, midi, bus, aux, vca, folder)"
            )));
        }
    })
}

fn monitor_name(m: MonitorMode) -> &'static str {
    match m {
        MonitorMode::Off => "off",
        MonitorMode::Input => "input",
        MonitorMode::Auto => "auto",
    }
}

fn format_name(f: PluginFormat) -> &'static str {
    match f {
        PluginFormat::Builtin => "builtin",
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
        PluginFormat::AudioUnit => "au",
        PluginFormat::Lv2 => "lv2",
    }
}

fn format_of(name: &str) -> LResult<PluginFormat> {
    Ok(match name.to_lowercase().as_str() {
        "builtin" => PluginFormat::Builtin,
        "clap" => PluginFormat::Clap,
        "vst3" => PluginFormat::Vst3,
        "au" => PluginFormat::AudioUnit,
        "lv2" => PluginFormat::Lv2,
        other => {
            return Err(fail(format!(
                "no plugin format '{other}' (builtin, clap, vst3, lv2, au)"
            )));
        }
    })
}

fn tap_name(t: SendTap) -> &'static str {
    match t {
        SendTap::PreFx => "pre-fx",
        SendTap::PreFader => "pre",
        SendTap::PostFader => "post",
    }
}

/// A value as the console shows it (tables a level or two deep).
pub(crate) fn show(v: &Value, depth: usize) -> String {
    match v {
        Value::String(s) => faderframe_script::literal::string(&s.to_string_lossy()),
        Value::Table(t) if depth < 2 => {
            let mut parts = Vec::new();
            let len = t.raw_len();
            for i in 1..=len.min(20) {
                if let Ok(v) = t.raw_get::<Value>(i) {
                    parts.push(show(&v, depth + 1));
                }
            }
            let mut named: Vec<(String, String)> = Vec::new();
            for (k, v) in t.clone().pairs::<Value, Value>().flatten() {
                match &k {
                    Value::Integer(i) if *i >= 1 && (*i as usize) <= len => continue,
                    _ => {}
                }
                let key = match &k {
                    Value::String(s) => {
                        let s = s.to_string_lossy();
                        if faderframe_script::literal::is_name(&s) {
                            s
                        } else {
                            format!("[{}]", faderframe_script::literal::string(&s))
                        }
                    }
                    other => format!("[{}]", show(other, depth + 1)),
                };
                named.push((key, show(&v, depth + 1)));
            }
            named.sort();
            let more = len > 20 || named.len() > 20;
            parts.extend(
                named
                    .into_iter()
                    .take(20)
                    .map(|(k, v)| format!("{k} = {v}")),
            );
            if more {
                parts.push("…".into());
            }
            if parts.is_empty() {
                "{}".into()
            } else {
                format!("{{ {} }}", parts.join(", "))
            }
        }
        Value::Table(_) => "{…}".into(),
        other => other
            .to_string()
            .unwrap_or_else(|_| other.type_name().into()),
    }
}

// --- references -----------------------------------------------------------

/// The track `v` names: an id, a name (exactly, else ignoring case; it
/// must be one track's) or a table with `id`.
fn track_ref(s: &Session, v: &Value) -> LResult<TrackId> {
    let p = &s.project;
    match v {
        Value::Integer(i) => {
            let id = TrackId(*i as u64);
            p.track(id)
                .map(|t| t.id)
                .ok_or_else(|| fail(format!("no track {i}")))
        }
        Value::Number(n) if n.fract() == 0.0 => track_ref(s, &Value::Integer(*n as i64)),
        Value::String(name) => {
            let name = name.to_string_lossy();
            let exact: Vec<TrackId> = p
                .tracks
                .iter()
                .filter(|t| t.name == name)
                .map(|t| t.id)
                .collect();
            let found = if exact.is_empty() {
                p.tracks
                    .iter()
                    .filter(|t| t.name.eq_ignore_ascii_case(&name))
                    .map(|t| t.id)
                    .collect()
            } else {
                exact
            };
            match found.as_slice() {
                [one] => Ok(*one),
                [] => Err(fail(format!("no track named '{name}'"))),
                _ => Err(fail(format!(
                    "{} tracks are named '{name}': name it by its id",
                    found.len()
                ))),
            }
        }
        Value::Table(t) => track_ref(s, &t.get::<Value>("id")?),
        other => Err(fail(format!(
            "a track (id, name or table) expected, got {}",
            other.type_name()
        ))),
    }
}

fn clip_ref(s: &Session, v: &Value) -> LResult<ClipId> {
    match v {
        Value::Integer(i) => {
            let id = ClipId(*i as u64);
            s.project
                .clip(id)
                .map(|c| c.id)
                .ok_or_else(|| fail(format!("no clip {i}")))
        }
        Value::Number(n) if n.fract() == 0.0 => clip_ref(s, &Value::Integer(*n as i64)),
        Value::Table(t) => clip_ref(s, &t.get::<Value>("id")?),
        other => Err(fail(format!(
            "a clip (id or table) expected, got {}",
            other.type_name()
        ))),
    }
}

/// One clip or a list of them.
fn clip_list(s: &Session, v: &Value) -> LResult<Vec<ClipId>> {
    match v {
        Value::Table(t) if t.contains_key("id")? => Ok(vec![clip_ref(s, v)?]),
        Value::Table(t) => t
            .sequence_values::<Value>()
            .map(|v| clip_ref(s, &v?))
            .collect(),
        other => Ok(vec![clip_ref(s, other)?]),
    }
}

/// The device `v` names on `track`: its place among the inserts (from
/// 1), its name as the mixer shows it, or a table with `id`.
fn plugin_ref(s: &Session, track: TrackId, v: &Value) -> LResult<PluginInstanceId> {
    let t = s
        .project
        .track(track)
        .ok_or_else(|| fail("no such track"))?;
    match v {
        Value::Integer(i) => t
            .inserts
            .get((*i as usize).wrapping_sub(1))
            .map(|p| p.id)
            .ok_or_else(|| fail(format!("{} has no insert {i}", t.name))),
        Value::Number(n) if n.fract() == 0.0 => plugin_ref(s, track, &Value::Integer(*n as i64)),
        Value::String(name) => {
            let name = name.to_string_lossy();
            t.slots()
                .into_iter()
                .find(|p| t.name_of(p).eq_ignore_ascii_case(&name))
                .map(|p| p.id)
                .ok_or_else(|| fail(format!("{} has no device '{name}'", t.name)))
        }
        Value::Table(tab) => {
            let id = PluginInstanceId(tab.get::<i64>("id")? as u64);
            t.plugin(id)
                .map(|p| p.id)
                .ok_or_else(|| fail(format!("{} has no device {}", t.name, id.0)))
        }
        other => Err(fail(format!(
            "a device (place, name or table) expected, got {}",
            other.type_name()
        ))),
    }
}

// --- acting -----------------------------------------------------------------

/// Each of a script's edits starts afresh from the project as it is:
/// drag bases and the ripple mark of the run's gesture are forgotten
/// (the changes stay in its one step).
fn fresh(s: &mut Session) {
    s.history.unmark();
    s.gesture_base.clear();
    s.follow_base.clear();
    s.grouped_targets = None;
    s.ripple_gesture_ended();
}

fn act(s: &mut Session, action: Action) -> LResult<()> {
    fresh(s);
    s.dispatch(action).map_err(fail)
}

fn command(s: &mut Session, cmd: Command) -> LResult<()> {
    fresh(s);
    s.edit(cmd).map_err(fail)
}

fn batch(s: &mut Session, label: &str, commands: Vec<Command>) -> LResult<()> {
    match commands.len() {
        0 => Ok(()),
        1 => command(
            s,
            commands
                .into_iter()
                .next()
                .ok_or_else(|| fail("no command"))?,
        ),
        _ => command(
            s,
            Command::Batch {
                label: label.into(),
                commands,
            },
        ),
    }
}

// --- reading ----------------------------------------------------------------

fn track_table(lua: &Lua, s: &Session, t: &Track) -> LResult<Table> {
    let out = lua.create_table()?;
    out.set("id", t.id.0)?;
    out.set("name", t.name.as_str())?;
    out.set("kind", kind_name(t.kind))?;
    out.set("color", t.color.to_hex())?;
    out.set("volume", t.volume_db)?;
    out.set("pan", t.pan)?;
    out.set("mute", t.mute)?;
    out.set("solo", t.solo)?;
    out.set("arm", t.record_arm)?;
    out.set("phase", t.phase_invert)?;
    out.set(
        "delay",
        if t.delay_ms.is_finite() {
            t.delay_ms
        } else {
            0.0
        },
    )?;
    out.set("monitor", monitor_name(t.monitor))?;
    out.set("layout", t.layout.short_name().to_lowercase())?;
    out.set("folder", t.folder.map(|f| f.0))?;
    out.set("selected", s.selection.tracks.contains(&t.id))?;
    out.set("clips", t.clips.len())?;
    let inserts = lua.create_table()?;
    for (i, slot) in t.inserts.iter().enumerate() {
        let d = lua.create_table()?;
        d.set("id", slot.id.0)?;
        d.set("place", i + 1)?;
        d.set("name", t.name_of(slot))?;
        d.set("plugin", slot.plugin.id.as_str())?;
        d.set("format", format_name(slot.plugin.format))?;
        d.set("bypass", slot.bypass)?;
        inserts.raw_push(d)?;
    }
    out.set("inserts", inserts)?;
    let sends = lua.create_table()?;
    for send in &t.sends {
        let d = lua.create_table()?;
        d.set("id", send.id.0)?;
        d.set("to", send.target.0)?;
        d.set("level", send.level_db)?;
        d.set("tap", tap_name(send.tap))?;
        d.set("on", send.enabled)?;
        sends.raw_push(d)?;
    }
    out.set("sends", sends)?;
    Ok(out)
}

fn clip_table(lua: &Lua, s: &Session, c: &Clip) -> LResult<Table> {
    let out = lua.create_table()?;
    out.set("id", c.id.0)?;
    out.set("track", c.track.0)?;
    out.set("name", c.name.as_str())?;
    let end = s.clip_end(c);
    out.set("start", quarters(c.start))?;
    out.set("finish", quarters(end))?;
    out.set("length", quarters(end) - quarters(c.start))?;
    out.set("muted", c.muted)?;
    out.set("selected", s.selection.clips.contains(&c.id))?;
    match &c.content {
        ClipContent::Audio(a) => {
            out.set("kind", "audio")?;
            out.set("gain", a.gain_db)?;
        }
        ClipContent::Midi(m) => {
            out.set("kind", "midi")?;
            out.set("notes", m.notes.len())?;
        }
        ClipContent::Takes(_) => out.set("kind", "takes")?,
    }
    Ok(out)
}

fn note_table(lua: &Lua, s: &Session, n: &MidiNote) -> LResult<Table> {
    let out = lua.create_table()?;
    out.set("id", n.id.0)?;
    out.set("key", n.key)?;
    out.set("start", quarters(n.start))?;
    out.set("length", quarters(n.length))?;
    out.set("velocity", n.velocity)?;
    out.set("channel", u32::from(n.channel) + 1)?;
    out.set("muted", n.muted)?;
    out.set("selected", s.selection.notes.contains(&n.id))?;
    Ok(out)
}

fn midi_of(s: &Session, clip: ClipId) -> LResult<&MidiClip> {
    s.project
        .clip(clip)
        .and_then(|c| c.as_midi())
        .ok_or_else(|| fail(format!("clip {} is not a MIDI clip", clip.0)))
}

fn reading(lua: &Lua, ff: &Table) -> LResult<()> {
    add(lua, ff, "script", |lua, s, ()| {
        let t = lua.create_table()?;
        t.set("name", s.scripts.running.clone())?;
        Ok(t)
    })?;
    add(lua, ff, "project", |lua, s, ()| {
        let p = &s.project;
        let t = lua.create_table()?;
        t.set("name", p.name.as_str())?;
        t.set(
            "path",
            s.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        )?;
        t.set("rate", p.sample_rate)?;
        t.set("tempo", p.timeline.tempo.bpm_at(MusicalTime::ZERO))?;
        let sig = p.timeline.meter.signature_at(MusicalTime::ZERO);
        t.set(
            "signature",
            format!("{}/{}", sig.numerator, sig.denominator),
        )?;
        t.set("length", quarters(p.content_end()))?;
        t.set("tracks", p.tracks.len())?;
        t.set("saved", !s.is_dirty())?;
        Ok(t)
    })?;
    add(lua, ff, "tracks", |lua, s, ()| {
        let out = lua.create_table()?;
        for t in s.project.folder_order() {
            out.raw_push(track_table(lua, s, t)?)?;
        }
        Ok(out)
    })?;
    add(lua, ff, "track", |lua, s, v: Value| {
        match track_ref(s, &v) {
            Ok(id) => match s.project.track(id) {
                Some(t) => Ok(Value::Table(track_table(lua, s, t)?)),
                None => Ok(Value::Nil),
            },
            Err(_) => Ok(Value::Nil),
        }
    })?;
    add(lua, ff, "clips", |lua, s, v: Option<Value>| {
        let out = lua.create_table()?;
        let launcher = s.project.launcher_clips();
        let mut list: Vec<&Clip> = match v {
            Some(v) if !v.is_nil() => s.project.clips_of(track_ref(s, &v)?),
            _ => s
                .project
                .clips
                .values()
                .filter(|c| !launcher.contains(&c.id))
                .collect(),
        };
        list.sort_by_key(|c| (c.start, c.id));
        for c in list {
            out.raw_push(clip_table(lua, s, c)?)?;
        }
        Ok(out)
    })?;
    add(lua, ff, "clip", |lua, s, v: Value| {
        match clip_ref(s, &v).ok().and_then(|id| s.project.clip(id)) {
            Some(c) => Ok(Value::Table(clip_table(lua, s, c)?)),
            None => Ok(Value::Nil),
        }
    })?;
    add(lua, ff, "notes", |lua, s, v: Value| {
        let clip = clip_ref(s, &v)?;
        let m = midi_of(s, clip)?;
        let mut notes: Vec<&MidiNote> = m.notes.iter().collect();
        notes.sort_by_key(|n| (n.start, n.key, n.id));
        let out = lua.create_table()?;
        for n in notes {
            out.raw_push(note_table(lua, s, n)?)?;
        }
        Ok(out)
    })?;
    add(lua, ff, "markers", |lua, s, ()| {
        let out = lua.create_table()?;
        let mut markers: Vec<_> = s.project.markers.iter().collect();
        markers.sort_by_key(|m| m.position);
        for m in markers {
            let t = lua.create_table()?;
            t.set("id", m.id.0)?;
            t.set("name", m.name.as_str())?;
            t.set("at", quarters(m.position))?;
            out.raw_push(t)?;
        }
        Ok(out)
    })?;
    add(lua, ff, "sections", |lua, s, ()| {
        let out = lua.create_table()?;
        let mut list: Vec<_> = s.project.sections.iter().collect();
        list.sort_by_key(|x| x.start);
        for x in list {
            let t = lua.create_table()?;
            t.set("id", x.id.0)?;
            t.set("name", x.name.as_str())?;
            t.set("start", quarters(x.start))?;
            t.set("finish", quarters(x.end))?;
            out.raw_push(t)?;
        }
        Ok(out)
    })?;
    add(lua, ff, "scripts", |lua, s, ()| {
        let out = lua.create_table()?;
        for e in s.script_library() {
            let t = lua.create_table()?;
            t.set("key", e.key.as_str())?;
            t.set("name", e.name.as_str())?;
            t.set("number", e.number)?;
            out.raw_push(t)?;
        }
        Ok(out)
    })?;
    // `ff.run(name)` calls the script `_script` compiles (in the run).
    add(lua, ff, "_script", |lua, s, v: Value| {
        let wanted = match &v {
            Value::Integer(n) => format!("macro/{n}"),
            Value::String(t) => t.to_string_lossy(),
            other => {
                return Err(fail(format!(
                    "a script's name, key or number expected, got {}",
                    other.type_name()
                )));
            }
        };
        s.rescan_scripts(false);
        let number = match &v {
            Value::Integer(n) => u16::try_from(*n).ok(),
            _ => None,
        };
        let entry = s
            .script_library()
            .iter()
            .find(|e| e.key == wanted || (number.is_some() && e.number == number))
            .or_else(|| {
                s.script_library()
                    .iter()
                    .find(|e| e.name.eq_ignore_ascii_case(&wanted))
            })
            .cloned()
            .ok_or_else(|| fail(format!("no script '{wanted}'")))?;
        let source = std::fs::read_to_string(&entry.path).map_err(fail)?;
        let file = entry
            .path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let env = faderframe_script::environment_of(lua)?;
        lua.load(source.as_str())
            .set_name(format!("@{file}"))
            .set_mode(mlua::chunk::ChunkMode::Text)
            .set_environment(env)
            .into_function()
    })?;
    Ok(())
}

// --- tracks -----------------------------------------------------------------

fn tracks(lua: &Lua, ff: &Table) -> LResult<()> {
    add(lua, ff, "add_track", |lua, s, spec: Value| {
        let (kind, name, layout, index) = match &spec {
            Value::String(k) => (kind_of(&k.to_string_lossy())?, None, None, None),
            Value::Table(t) => (
                kind_of(
                    &t.get::<Option<String>>("kind")?
                        .unwrap_or_else(|| "audio".into()),
                )?,
                t.get::<Option<String>>("name")?,
                t.get::<Option<String>>("layout")?,
                t.get::<Option<usize>>("index")?,
            ),
            Value::Nil => (TrackKind::Audio, None, None, None),
            other => {
                return Err(fail(format!(
                    "a track kind or a table expected, got {}",
                    other.type_name()
                )));
            }
        };
        let layout = match layout.as_deref().map(str::to_lowercase).as_deref() {
            None => None,
            Some("mono") => Some(faderframe_core::ChannelLayout::Mono),
            Some("stereo") => Some(faderframe_core::ChannelLayout::Stereo),
            Some(other) => return Err(fail(format!("no layout '{other}' (mono, stereo)"))),
        };
        fresh(s);
        let id = s.add_track_with_layout(kind, layout).map_err(fail)?;
        if let Some(name) = name {
            command(s, Command::RenameTrack { track: id, name })?;
        }
        if let Some(i) = index {
            let i = i.clamp(1, s.project.tracks.len()) - 1;
            command(
                s,
                Command::MoveTrack {
                    track: id,
                    index: i,
                },
            )?;
        }
        let t = s.project.track(id).ok_or_else(|| fail("the track went"))?;
        track_table(lua, s, t)
    })?;
    add(lua, ff, "remove_track", |_, s, v: Value| {
        let track = track_ref(s, &v)?;
        if s.project
            .track(track)
            .is_some_and(|t| t.kind == TrackKind::Master)
        {
            return Err(fail("the master stays"));
        }
        command(s, Command::RemoveTrack { track })
    })?;
    add(lua, ff, "move_track", |_, s, (v, index): (Value, usize)| {
        let track = track_ref(s, &v)?;
        let i = index.clamp(1, s.project.tracks.len()) - 1;
        command(s, Command::MoveTrack { track, index: i })
    })?;
    add(lua, ff, "set_track", |_, s, (v, fields): (Value, Table)| {
        let track = track_ref(s, &v)?;
        let mut cmds = Vec::new();
        for (k, value) in fields.pairs::<String, Value>().flatten() {
            let num = || -> LResult<f64> {
                match &value {
                    Value::Integer(i) => Ok(*i as f64),
                    Value::Number(n) => Ok(*n),
                    other => Err(fail(format!(
                        "{k}: a number expected, got {}",
                        other.type_name()
                    ))),
                }
            };
            let flag = || -> LResult<bool> {
                value
                    .as_boolean()
                    .ok_or_else(|| fail(format!("{k}: true or false expected")))
            };
            let text = || -> LResult<String> {
                value
                    .as_string()
                    .map(|t| t.to_string_lossy())
                    .ok_or_else(|| fail(format!("{k}: a string expected")))
            };
            cmds.push(match k.as_str() {
                "name" => Command::RenameTrack { track, name: text()? },
                "volume" => Command::SetTrackVolume { track, db: num()? as f32 },
                "pan" => Command::SetTrackPan {
                    track,
                    pan: (num()? as f32).clamp(-1.0, 1.0),
                },
                "mute" => Command::SetTrackMute { track, on: flag()? },
                "solo" => Command::SetTrackSolo { track, on: flag()? },
                "arm" => Command::SetTrackRecordArm { track, on: flag()? },
                "phase" => Command::SetTrackPhaseInvert { track, on: flag()? },
                "delay" => Command::SetTrackDelay { track, ms: num()? },
                "color" => Command::SetTrackColor {
                    track,
                    color: TrackColor::from_hex(&text()?)
                        .ok_or_else(|| fail("color: \"#rrggbb\" expected"))?,
                },
                "monitor" => Command::SetTrackMonitor {
                    track,
                    mode: match text()?.as_str() {
                        "off" => MonitorMode::Off,
                        "input" => MonitorMode::Input,
                        "auto" => MonitorMode::Auto,
                        other => return Err(fail(format!("monitor: no '{other}' (off, input, auto)"))),
                    },
                },
                other => {
                    return Err(fail(format!(
                        "set_track: no field '{other}' (name, volume, pan, mute, solo, arm, phase, delay, color, monitor)"
                    )));
                }
            });
        }
        batch(s, "Script: Track", cmds)
    })?;
    Ok(())
}

// --- clips and notes --------------------------------------------------------

fn note_from(t: &Table, base: Option<&MidiNote>) -> LResult<MidiNote> {
    let mut n = base.copied().unwrap_or(MidiNote {
        id: NoteId(0),
        start: MusicalTime::ZERO,
        length: MusicalTime::from_quarters(1.0),
        key: 60,
        velocity: 100,
        channel: 0,
        muted: false,
        release: None,
        time_locked: false,
        articulation: None,
        fine: None,
    });
    if let Some(k) = t.get::<Option<i64>>("key")? {
        n.key = u8::try_from(k)
            .ok()
            .filter(|k| *k < 128)
            .ok_or_else(|| fail("key: 0…127"))?;
    }
    if let Some(v) = t.get::<Option<f64>>("start")? {
        n.start = at(v);
    }
    if let Some(v) = t.get::<Option<f64>>("length")? {
        n.length = length(v, "length")?;
    }
    if let Some(v) = t.get::<Option<i64>>("velocity")? {
        n.velocity = u8::try_from(v)
            .ok()
            .filter(|v| (1..128).contains(v))
            .ok_or_else(|| fail("velocity: 1…127"))?;
    }
    if let Some(c) = t.get::<Option<i64>>("channel")? {
        n.channel = u8::try_from(c - 1)
            .ok()
            .filter(|c| *c < 16)
            .ok_or_else(|| fail("channel: 1…16"))?;
    }
    if let Some(m) = t.get::<Option<bool>>("muted")? {
        n.muted = m;
    }
    Ok(n)
}

fn clips(lua: &Lua, ff: &Table) -> LResult<()> {
    add(
        lua,
        ff,
        "add_midi_clip",
        |_, s, (v, start, len, name): (Value, f64, f64, Option<String>)| {
            let track = track_ref(s, &v)?;
            let t = s
                .project
                .track(track)
                .ok_or_else(|| fail("no such track"))?;
            if !matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
                return Err(fail(format!("{} takes no MIDI clips", t.name)));
            }
            let id: ClipId = s.project.ids.allocate();
            command(
                s,
                Command::AddClip {
                    clip: Box::new(Clip {
                        id,
                        track,
                        name: name.unwrap_or_else(|| "MIDI".into()),
                        color: None,
                        start: at(start),
                        muted: false,
                        content: ClipContent::Midi(MidiClip {
                            length: length(len, "length")?,
                            ..MidiClip::default()
                        }),
                    }),
                },
            )?;
            Ok(id.0)
        },
    )?;
    add(lua, ff, "add_note", |_, s, (v, note): (Value, Table)| {
        let clip = clip_ref(s, &v)?;
        midi_of(s, clip)?;
        let mut n = note_from(&note, None)?;
        n.id = s.project.ids.allocate();
        command(s, Command::AddNote { clip, note: n })?;
        Ok(n.id.0)
    })?;
    add(lua, ff, "add_notes", |lua, s, (v, list): (Value, Table)| {
        let clip = clip_ref(s, &v)?;
        midi_of(s, clip)?;
        let mut cmds = Vec::new();
        let ids = lua.create_table()?;
        for t in list.sequence_values::<Table>() {
            let mut n = note_from(&t?, None)?;
            n.id = s.project.ids.allocate();
            ids.raw_push(n.id.0)?;
            cmds.push(Command::AddNote { clip, note: n });
        }
        batch(s, "Script: Add Notes", cmds)?;
        Ok(ids)
    })?;
    add(
        lua,
        ff,
        "set_note",
        |_, s, (v, id, fields): (Value, u64, Table)| {
            let clip = clip_ref(s, &v)?;
            let old = midi_of(s, clip)?
                .notes
                .iter()
                .find(|n| n.id == NoteId(id))
                .copied()
                .ok_or_else(|| fail(format!("clip {} has no note {id}", clip.0)))?;
            let n = note_from(&fields, Some(&old))?;
            command(s, Command::UpdateNote { clip, note: n })
        },
    )?;
    add(lua, ff, "remove_note", |_, s, (v, id): (Value, u64)| {
        let clip = clip_ref(s, &v)?;
        midi_of(s, clip)?;
        command(
            s,
            Command::RemoveNote {
                clip,
                note: NoteId(id),
            },
        )
    })?;
    add(lua, ff, "move_clip", |_, s, (v, place): (Value, Table)| {
        let clip = clip_ref(s, &v)?;
        let c = s.project.clip(clip).ok_or_else(|| fail("no such clip"))?;
        let (mut track, mut start) = (c.track, c.start);
        if let Some(q) = place.get::<Option<f64>>("start")? {
            start = at(q);
        }
        let t: Value = place.get("track")?;
        if !t.is_nil() {
            track = track_ref(s, &t)?;
        }
        command(s, Command::MoveClip { clip, track, start })
    })?;
    add(lua, ff, "set_clip", |_, s, (v, fields): (Value, Table)| {
        let clips = clip_list(s, &v)?;
        let name: Option<String> = fields.get("name")?;
        let muted: Option<bool> = fields.get("muted")?;
        let gain: Option<f64> = fields.get("gain")?;
        for (k, _) in fields.pairs::<String, Value>().flatten() {
            if !matches!(k.as_str(), "name" | "muted" | "gain") {
                return Err(fail(format!(
                    "set_clip: no field '{k}' (name, muted, gain)"
                )));
            }
        }
        let mut cmds = Vec::new();
        for clip in &clips {
            if let Some(n) = &name {
                cmds.push(Command::RenameClip {
                    clip: *clip,
                    name: n.clone(),
                });
            }
            if let Some(m) = muted {
                cmds.push(Command::SetClipMuted {
                    clip: *clip,
                    muted: m,
                });
            }
        }
        batch(s, "Script: Clips", cmds)?;
        if let Some(db) = gain {
            act(
                s,
                Action::SetClipsGain {
                    clips,
                    db: db as f32,
                },
            )?;
        }
        Ok(())
    })?;
    add(lua, ff, "remove_clip", |_, s, v: Value| {
        let clips = clip_list(s, &v)?;
        batch(
            s,
            "Script: Remove Clips",
            clips
                .into_iter()
                .map(|clip| Command::RemoveClip { clip })
                .collect(),
        )
    })?;
    add(lua, ff, "split_clip", |_, s, (v, q): (Value, f64)| {
        let clip = clip_ref(s, &v)?;
        let c = s.project.clip(clip).ok_or_else(|| fail("no such clip"))?;
        let at = at(q);
        if at <= c.start || at >= s.clip_end(c) {
            return Err(fail("split_clip: the place must be inside the clip"));
        }
        let new_clip: ClipId = s.project.ids.allocate();
        command(s, Command::SplitClip { clip, at, new_clip })?;
        Ok(new_clip.0)
    })?;
    add(
        lua,
        ff,
        "move_clips",
        |_, s, (v, by, lanes): (Value, f64, Option<i32>)| {
            let clips = clip_list(s, &v)?;
            let by = (by * TICKS_PER_QUARTER as f64).round() as i64;
            act(
                s,
                Action::MoveClips {
                    clips,
                    by,
                    tracks: lanes.unwrap_or(0),
                },
            )
        },
    )?;
    add(
        lua,
        ff,
        "trim_clips",
        |_, s, (v, edge, by, stretch): (Value, String, f64, Option<bool>)| {
            let clips = clip_list(s, &v)?;
            let edge = edge_of(&edge)?;
            let by = (by * TICKS_PER_QUARTER as f64).round() as i64;
            act(
                s,
                Action::TrimClips {
                    clips,
                    edge,
                    by,
                    stretch: stretch.unwrap_or(false),
                },
            )
        },
    )?;
    add(lua, ff, "clip_gain", |_, s, (v, delta): (Value, f64)| {
        let clips = clip_list(s, &v)?;
        act(
            s,
            Action::ClipGain {
                clips,
                delta_db: delta as f32,
            },
        )
    })?;
    Ok(())
}

fn edge_of(e: &str) -> LResult<ClipEdge> {
    match e {
        "start" => Ok(ClipEdge::Start),
        "end" | "finish" => Ok(ClipEdge::End),
        other => Err(fail(format!("no edge '{other}' (start, end)"))),
    }
}

// --- timeline ----------------------------------------------------------------

fn timeline(lua: &Lua, ff: &Table) -> LResult<()> {
    add(
        lua,
        ff,
        "add_marker",
        |_, s, (q, name): (f64, Option<String>)| {
            fresh(s);
            let id = s.add_marker(at(q)).map_err(fail)?;
            if let Some(name) = name
                && let Some(m) = s.project.markers.iter().find(|m| m.id == id).cloned()
            {
                command(
                    s,
                    Command::UpdateMarker {
                        marker: faderframe_project::Marker { name, ..m },
                    },
                )?;
            }
            Ok(id.0)
        },
    )?;
    add(lua, ff, "set_marker", |_, s, (id, fields): (u64, Table)| {
        let mut m = s
            .project
            .markers
            .iter()
            .find(|m| m.id == MarkerId(id))
            .cloned()
            .ok_or_else(|| fail(format!("no marker {id}")))?;
        if let Some(n) = fields.get::<Option<String>>("name")? {
            m.name = n;
        }
        if let Some(q) = fields.get::<Option<f64>>("at")? {
            m.position = at(q);
        }
        command(s, Command::UpdateMarker { marker: m })
    })?;
    add(lua, ff, "remove_marker", |_, s, id: u64| {
        if !s.project.markers.iter().any(|m| m.id == MarkerId(id)) {
            return Err(fail(format!("no marker {id}")));
        }
        command(
            s,
            Command::RemoveMarker {
                marker: MarkerId(id),
            },
        )
    })?;
    add(
        lua,
        ff,
        "add_section",
        |_, s, (a, b, name): (f64, f64, Option<String>)| {
            fresh(s);
            let id = s.add_section(at(a), at(b)).map_err(fail)?;
            if let Some(name) = name
                && let Some(x) = s.project.sections.iter().find(|x| x.id == id).cloned()
            {
                command(
                    s,
                    Command::UpdateSection {
                        section: faderframe_project::Section { name, ..x },
                    },
                )?;
            }
            Ok(id.0)
        },
    )?;
    add(lua, ff, "set_tempo", |_, s, bpm: f64| {
        if !(bpm.is_finite() && (10.0..=999.0).contains(&bpm)) {
            return Err(fail("set_tempo: 10…999 BPM"));
        }
        command(s, Command::SetTempo { bpm })
    })?;
    add(
        lua,
        ff,
        "set_loop",
        |_, s, (a, b): (Option<f64>, Option<f64>)| {
            let range = match (a, b) {
                (Some(a), Some(b)) => Some(
                    MusicalRange::new(at(a), at(b))
                        .ok_or_else(|| fail("set_loop: the end must come after the start"))?,
                ),
                _ => None,
            };
            command(
                s,
                Command::SetLoop {
                    range,
                    enabled: range.is_some(),
                },
            )
        },
    )?;
    add(lua, ff, "tempo_at", |_, s, q: f64| {
        Ok(s.project.timeline.tempo.bpm_at(at(q)))
    })?;
    add(lua, ff, "signature_at", |_, s, q: f64| {
        let sig = s.project.timeline.meter.signature_at(at(q));
        Ok((sig.numerator, sig.denominator))
    })?;
    add(lua, ff, "bar", |_, s, n: i32| {
        Ok(quarters(s.project.timeline.meter.bar_start(n.max(1) - 1)))
    })?;
    add(lua, ff, "bar_at", |_, s, q: f64| {
        Ok(s.project.timeline.meter.bar_at(at(q)) + 1)
    })?;
    add(lua, ff, "seconds", |_, s, q: f64| {
        Ok(s.project.timeline.tempo.musical_to_seconds(at(q)))
    })?;
    add(lua, ff, "quarters", |_, s, secs: f64| {
        Ok(s.project.timeline.tempo.seconds_to_quarters(secs.max(0.0)))
    })?;
    add(lua, ff, "bbt", |_, s, q: f64| {
        Ok(s.project.timeline.format_bbt(at(q)))
    })?;
    Ok(())
}

// --- selection and the Edit menu ---------------------------------------------

fn ids(lua: &Lua, list: impl IntoIterator<Item = u64>) -> LResult<Table> {
    let t = lua.create_table()?;
    for id in list {
        t.raw_push(id)?;
    }
    Ok(t)
}

fn selection(lua: &Lua, ff: &Table) -> LResult<()> {
    add(lua, ff, "selection", |lua, s, ()| {
        let out = lua.create_table()?;
        out.set("tracks", ids(lua, s.selection.tracks.iter().map(|t| t.0))?)?;
        out.set("clips", ids(lua, s.selection.clips.iter().map(|c| c.0))?)?;
        out.set("notes", ids(lua, s.selection.notes.iter().map(|n| n.0))?)?;
        if let Some(r) = s.selection.range {
            let range = lua.create_table()?;
            range.set("start", quarters(r.start))?;
            range.set("finish", quarters(r.end))?;
            out.set("range", range)?;
        }
        Ok(out)
    })?;
    add(lua, ff, "selected_tracks", |lua, s, ()| {
        ids(lua, s.selection.tracks.iter().map(|t| t.0))
    })?;
    add(lua, ff, "selected_clips", |lua, s, ()| {
        ids(lua, s.selected_arranger_clips().into_iter().map(|c| c.0))
    })?;
    add(lua, ff, "select", |_, s, spec: Table| {
        let mode = match spec.get::<Option<String>>("mode")?.as_deref() {
            None | Some("replace") => SelectMode::Replace,
            Some("add") => SelectMode::Add,
            Some("toggle") => SelectMode::Toggle,
            Some(other) => {
                return Err(fail(format!(
                    "select: no mode '{other}' (replace, add, toggle)"
                )));
            }
        };
        let tracks: Value = spec.get("tracks")?;
        if let Value::Table(t) = &tracks {
            let list = t
                .sequence_values::<Value>()
                .map(|v| track_ref(s, &v?))
                .collect::<LResult<Vec<_>>>()?;
            act(s, Action::SelectTracks { tracks: list, mode })?;
        }
        let clips: Value = spec.get("clips")?;
        if let Value::Table(_) = &clips {
            let list = clip_list(s, &clips)?;
            act(s, Action::SelectClips { clips: list, mode })?;
        }
        let notes: Value = spec.get("notes")?;
        if let Value::Table(t) = &notes {
            let list = t
                .sequence_values::<u64>()
                .map(|v| v.map(NoteId))
                .collect::<LResult<Vec<_>>>()?;
            act(s, Action::SelectNotes { notes: list, mode })?;
        }
        match spec.get::<Value>("range")? {
            Value::Table(r) => {
                let a: f64 = r.get(1).or_else(|_| r.get("start"))?;
                let b: f64 = r.get(2).or_else(|_| r.get("finish"))?;
                act(s, Action::SetEditRange(Some(EditRange::new(at(a), at(b)))))?;
            }
            Value::Boolean(false) => act(s, Action::SetEditRange(None))?,
            _ => {}
        }
        Ok(())
    })?;
    add(lua, ff, "clear_selection", |_, s, ()| {
        act(s, Action::ClearSelection)
    })?;
    add(lua, ff, "edit", |_, s, (name, arg): (String, Value)| {
        let count = || -> LResult<u32> {
            match &arg {
                Value::Nil => Ok(1),
                Value::Integer(n) if *n >= 1 => Ok(*n as u32),
                _ => Err(fail(format!("{name}: a count of at least 1 expected"))),
            }
        };
        let selected = |s: &Session| -> LResult<Vec<ClipId>> {
            match &arg {
                Value::Nil => Ok(s.selected_arranger_clips()),
                v => clip_list(s, v),
            }
        };
        let action = match name.as_str() {
            "separate" => Action::Separate,
            "trim-to-selection" => Action::TrimToSelection,
            "clear-range" => Action::ClearRange,
            "copy-range" => Action::CopyRange,
            "cut-range" => Action::CutRange,
            "paste-range" => Action::PasteRange,
            "repeat-range" => Action::RepeatRange(count()?),
            "insert-silence" => Action::InsertSilence,
            "copy" => Action::Copy,
            "cut" => Action::Cut,
            "paste" => Action::Paste,
            "duplicate" => Action::Repeat(1),
            "repeat" => Action::Repeat(count()?),
            "select-all" => Action::SelectAllClips,
            "split" => Action::SplitSelectedAtPlayhead,
            "delete" => Action::DeleteSelection,
            "delete-clips" => Action::DeleteInArranger,
            "remove-tracks" => Action::RemoveSelectedTracks,
            "arm" => Action::ToggleArmSelected,
            "group" => Action::GroupSelectedTracks,
            "quantize" => Action::QuantizeClips(selected(s)?),
            "humanize" => Action::HumanizeClips(selected(s)?),
            "merge" => Action::MergeClips(selected(s)?),
            "duplicate-tracks" => Action::DuplicateTracks {
                tracks: Vec::new(),
                content: arg.as_boolean().unwrap_or(true),
            },
            other => {
                return Err(fail(format!(
                    "edit: no command '{other}' (see docs/SCRIPTING.md for the list)"
                )));
            }
        };
        act(s, action)
    })?;
    add(
        lua,
        ff,
        "nudge",
        |_, s, (forward, target): (bool, Option<String>)| {
            let target = match target.as_deref() {
                None | Some("move") => NudgeTarget::Move,
                Some("trim-start") => NudgeTarget::TrimStart,
                Some("trim-end") => NudgeTarget::TrimEnd,
                Some(other) => {
                    return Err(fail(format!(
                        "nudge: no '{other}' (move, trim-start, trim-end)"
                    )));
                }
            };
            act(s, Action::Nudge { forward, target })
        },
    )?;
    Ok(())
}

// --- devices and sends -------------------------------------------------------

fn devices(lua: &Lua, ff: &Table) -> LResult<()> {
    add(lua, ff, "plugins", |lua, s, ()| {
        let out = lua.create_table()?;
        for p in s.available_plugins() {
            let t = lua.create_table()?;
            t.set("format", format_name(p.plugin.format))?;
            t.set("id", p.plugin.id.as_str())?;
            t.set("name", p.plugin.name.as_str())?;
            t.set("vendor", p.vendor.as_str())?;
            t.set("instrument", p.instrument)?;
            out.raw_push(t)?;
        }
        Ok(out)
    })?;
    add(
        lua,
        ff,
        "insert_plugin",
        |_, s, (v, spec, place): (Value, Value, Option<usize>)| {
            let track = track_ref(s, &v)?;
            let (format, id) = match &spec {
                Value::String(id) => (PluginFormat::Builtin, id.to_string_lossy()),
                Value::Table(t) => (
                    format_of(
                        &t.get::<Option<String>>("format")?
                            .unwrap_or_else(|| "builtin".into()),
                    )?,
                    t.get::<String>("id")?,
                ),
                other => {
                    return Err(fail(format!(
                        "a plugin id or {{ format, id }} expected, got {}",
                        other.type_name()
                    )));
                }
            };
            let found = s
                .available_plugins()
                .into_iter()
                .find(|p| p.plugin.format == format && p.plugin.id == id)
                .ok_or_else(|| fail(format!("no {} plugin '{id}' here", format_name(format))))?;
            let before: Vec<PluginInstanceId> = s
                .project
                .track(track)
                .map(|t| t.inserts.iter().map(|p| p.id).collect())
                .unwrap_or_default();
            let count = before.len();
            let index = place.map_or(count, |p| p.clamp(1, count + 1) - 1);
            act(
                s,
                Action::InsertPlugin {
                    track,
                    index,
                    plugin: PluginRef {
                        format,
                        id,
                        name: found.plugin.name,
                    },
                },
            )?;
            s.project
                .track(track)
                .and_then(|t| t.inserts.iter().find(|p| !before.contains(&p.id)))
                .map(|p| p.id.0)
                .ok_or_else(|| fail("the plugin was not inserted"))
        },
    )?;
    add(lua, ff, "remove_plugin", |_, s, (v, p): (Value, Value)| {
        let track = track_ref(s, &v)?;
        let plugin = plugin_ref(s, track, &p)?;
        command(s, Command::RemovePlugin { track, plugin })
    })?;
    add(
        lua,
        ff,
        "set_plugin",
        |_, s, (v, p, fields): (Value, Value, Table)| {
            let track = track_ref(s, &v)?;
            let plugin = plugin_ref(s, track, &p)?;
            let mut cmds = Vec::new();
            if let Some(b) = fields.get::<Option<bool>>("bypass")? {
                cmds.push(Command::SetPluginBypass {
                    track,
                    plugin,
                    bypass: b,
                });
            }
            if let Some(params) = fields.get::<Option<Table>>("params")? {
                for pair in params.pairs::<u32, f64>() {
                    let (id, value) = pair?;
                    cmds.push(Command::SetPluginParameter {
                        track,
                        plugin,
                        parameter: faderframe_core::ParameterId(id),
                        value: Some(value),
                    });
                }
            }
            batch(s, "Script: Device", cmds)
        },
    )?;
    add(lua, ff, "parameters", |lua, s, (v, p): (Value, Value)| {
        let track = track_ref(s, &v)?;
        let plugin = plugin_ref(s, track, &p)?;
        let out = lua.create_table()?;
        for view in s.plugin_parameter_views(plugin) {
            let t = lua.create_table()?;
            t.set("id", view.info.id.0)?;
            t.set("name", view.info.name.as_str())?;
            t.set("value", view.value)?;
            t.set("min", view.info.min)?;
            t.set("max", view.info.max)?;
            t.set("default", view.info.default)?;
            out.raw_push(t)?;
        }
        Ok(out)
    })?;
    add(
        lua,
        ff,
        "add_send",
        |_, s, (v, d, opts): (Value, Value, Option<Table>)| {
            let track = track_ref(s, &v)?;
            let target = track_ref(s, &d)?;
            let (mut level_db, mut tap) = (0.0_f32, SendTap::PostFader);
            if let Some(o) = opts {
                if let Some(l) = o.get::<Option<f64>>("level")? {
                    level_db = l as f32;
                }
                if let Some(t) = o.get::<Option<String>>("tap")? {
                    tap = match t.as_str() {
                        "post" => SendTap::PostFader,
                        "pre" => SendTap::PreFader,
                        "pre-fx" => SendTap::PreFx,
                        other => {
                            return Err(fail(format!("tap: no '{other}' (post, pre, pre-fx)")));
                        }
                    };
                }
            }
            act(
                s,
                Action::AddSend {
                    track,
                    target,
                    level_db,
                    tap,
                },
            )
        },
    )?;
    Ok(())
}

// --- transport -----------------------------------------------------------------

fn transport(lua: &Lua, ff: &Table) -> LResult<()> {
    for (name, t) in [
        ("play", TransportAction::Play),
        ("stop", TransportAction::Stop),
        ("toggle_play", TransportAction::TogglePlay),
        ("return_to_start", TransportAction::ReturnToStart),
        ("toggle_loop", TransportAction::ToggleLoop),
        ("record", TransportAction::ToggleRecord),
    ] {
        add(lua, ff, name, move |_, s, ()| {
            act(s, Action::Transport(t.clone()))
        })?;
    }
    add(lua, ff, "locate", |_, s, q: f64| {
        act(s, Action::Transport(TransportAction::Locate(at(q))))
    })?;
    add(lua, ff, "nudge_bars", |_, s, n: i32| {
        act(s, Action::Transport(TransportAction::NudgeBars(n)))
    })?;
    add(lua, ff, "transport", |lua, s, ()| {
        let t = s.transport();
        let out = lua.create_table()?;
        out.set("playing", t.playing)?;
        out.set("recording", t.recording)?;
        out.set("looping", t.looping)?;
        let here = s.playhead();
        out.set("position", quarters(here))?;
        out.set("seconds", s.project.timeline.tempo.musical_to_seconds(here))?;
        out.set("bar", s.project.timeline.meter.bar_at(here) + 1)?;
        if let Some(r) = s.project.loop_range {
            let l = lua.create_table()?;
            l.set("start", quarters(r.start))?;
            l.set("finish", quarters(r.end))?;
            l.set("on", s.project.loop_enabled)?;
            out.set("loop", l)?;
        }
        Ok(out)
    })?;
    Ok(())
}

// --- rendering ------------------------------------------------------------------

fn rendering(lua: &Lua, ff: &Table) -> LResult<()> {
    add(lua, ff, "render", |_, s, spec: Table| {
        use crate::render::{Overwrite, RenderChannels, RenderRange, RenderSettings, RenderSource};
        let file: String = spec
            .get::<Option<String>>("file")?
            .ok_or_else(|| fail("render: say where: { file = \"mix.wav\" }"))?;
        // Renders write only where scripts may write files.
        let folders = s.script_folders().files;
        let base = folders
            .first()
            .cloned()
            .ok_or_else(|| fail("render: no folder to write into"))?;
        let path = std::path::Path::new(&file);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        };
        let parent = path
            .parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .ok_or_else(|| fail(format!("render: no folder for {file}")))?;
        let roots: Vec<_> = folders
            .iter()
            .filter_map(|f| std::fs::canonicalize(f).ok())
            .collect();
        if !roots.iter().any(|r| parent.starts_with(r)) {
            return Err(fail(format!(
                "render: {file} is outside the folders scripts may use (the project's folder and the scripts folders)"
            )));
        }
        let path = parent.join(
            path.file_name()
                .ok_or_else(|| fail("render: not a file name"))?,
        );
        let mut settings = RenderSettings::defaults_for(&s.project, path);
        match spec.get::<Value>("range")? {
            Value::Nil => {}
            Value::String(r) if r.to_string_lossy() == "project" => {
                settings.range = RenderRange::Project
            }
            Value::String(r) if r.to_string_lossy() == "loop" => settings.range = RenderRange::Loop,
            Value::Table(r) => {
                let a: f64 = r.get(1)?;
                let b: f64 = r.get(2)?;
                settings.range = RenderRange::Span {
                    start: at(a),
                    end: at(b),
                };
            }
            _ => {
                return Err(fail(
                    "render: range is \"project\", \"loop\" or { start, finish }",
                ));
            }
        }
        match spec.get::<Value>("source")? {
            Value::Nil => {}
            Value::String(x) if x.to_string_lossy() == "master" => {
                settings.source = RenderSource::Master
            }
            Value::String(x) if x.to_string_lossy() == "stems" => {
                settings.source = RenderSource::Stems
            }
            other => settings.source = RenderSource::Track(track_ref(s, &other)?),
        }
        if let Some(f) = spec.get::<Option<String>>("format")? {
            settings.format = match f.as_str() {
                "pcm16" | "16" => faderframe_audio_files::WavFormat::Pcm16,
                "pcm24" | "24" => faderframe_audio_files::WavFormat::Pcm24,
                "float" | "32" => faderframe_audio_files::WavFormat::Float32,
                other => {
                    return Err(fail(format!(
                        "render: no format '{other}' (pcm16, pcm24, float)"
                    )));
                }
            };
        }
        if let Some(r) = spec.get::<Option<u32>>("rate")? {
            settings.sample_rate = r.clamp(8000, 384_000);
        }
        if let Some(c) = spec.get::<Option<String>>("channels")? {
            settings.channels = match c.as_str() {
                "stereo" => RenderChannels::Stereo,
                "mono" => RenderChannels::Mono,
                "master" => RenderChannels::Master,
                other => {
                    return Err(fail(format!(
                        "render: no channels '{other}' (stereo, mono, master)"
                    )));
                }
            };
        }
        if let Some(t) = spec.get::<Option<f64>>("tail")? {
            settings.tail_seconds = t.clamp(0.0, 60.0) as f32;
        }
        settings.normalize_db = spec.get::<Option<f64>>("normalize")?.map(|n| n as f32);
        if spec.get::<Option<bool>>("overwrite")?.unwrap_or(false) {
            settings.overwrite = Overwrite::Always;
        }
        let label = s.scripts.running.clone().unwrap_or_default();
        let job = s.render(settings).map_err(fail)?;
        let id = s.scripts.fresh_id();
        s.scripts.renders.push(ScriptRender {
            id,
            label,
            job: Some(job),
            outcome: RenderOutcome {
                done: false,
                progress: 0.0,
                files: Vec::new(),
                error: None,
            },
        });
        Ok(id)
    })?;
    add(lua, ff, "render_status", |lua, s, id: u64| {
        s.poll_script_renders();
        let r = s
            .scripts
            .renders
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| fail(format!("no render {id}")))?;
        let out = lua.create_table()?;
        out.set("done", r.outcome.done)?;
        out.set("progress", r.outcome.progress)?;
        out.set("error", r.outcome.error.clone())?;
        let files = lua.create_table()?;
        for f in &r.outcome.files {
            files.raw_push(f.to_string_lossy().into_owned())?;
        }
        out.set("files", files)?;
        Ok(out)
    })?;
    add(lua, ff, "cancel_render", |_, s, id: u64| {
        if let Some(j) = s
            .scripts
            .renders
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.job.as_ref())
        {
            j.cancel();
        }
        Ok(())
    })?;
    Ok(())
}

// --- prompts, notices, timers -------------------------------------------------------

fn field_from(v: &Value) -> LResult<PromptField> {
    match v {
        Value::String(label) => Ok(PromptField {
            label: label.to_string_lossy(),
            kind: FieldKind::Text,
            default: String::new(),
        }),
        Value::Table(t) => {
            let label: String = t.get::<Option<String>>("label")?.unwrap_or_default();
            let default: Value = t.get("default")?;
            let choices: Option<Vec<String>> = t.get("choices")?;
            let kind = match (t.get::<Option<String>>("kind")?.as_deref(), choices) {
                (_, Some(c)) if !c.is_empty() => FieldKind::Choice(c),
                (Some("number"), _) => FieldKind::Number,
                (Some("check"), _) => FieldKind::Check,
                (None | Some("text"), _) => match default {
                    Value::Integer(_) | Value::Number(_) => FieldKind::Number,
                    Value::Boolean(_) => FieldKind::Check,
                    _ => FieldKind::Text,
                },
                (Some(other), _) => {
                    return Err(fail(format!(
                        "prompt: no field kind '{other}' (text, number, check)"
                    )));
                }
            };
            let default = match (&kind, &default) {
                (_, Value::Nil) => match &kind {
                    FieldKind::Check => "false".into(),
                    FieldKind::Choice(c) => c[0].clone(),
                    _ => String::new(),
                },
                (_, v) => v.to_string()?,
            };
            Ok(PromptField {
                label,
                kind,
                default,
            })
        }
        other => Err(fail(format!(
            "prompt: a field is a label or a table, got {}",
            other.type_name()
        ))),
    }
}

/// The answers to `fields` as the script gets them: by place and by
/// label (numbers as numbers, checks as booleans).
pub(crate) fn answers(lua: &Lua, fields: &[PromptField], values: &[String]) -> LResult<Table> {
    let out = lua.create_table()?;
    for (i, f) in fields.iter().enumerate() {
        let raw = values.get(i).cloned().unwrap_or_default();
        let v = match f.kind {
            FieldKind::Number => match raw.trim().parse::<f64>() {
                Ok(n) if n.fract() == 0.0 && n.abs() < 1e15 => Value::Integer(n as i64),
                Ok(n) => Value::Number(n),
                Err(_) => Value::Nil,
            },
            FieldKind::Check => Value::Boolean(raw == "true"),
            _ => Value::String(lua.create_string(&raw)?),
        };
        out.raw_set(i + 1, v.clone())?;
        if !f.label.is_empty() {
            out.raw_set(f.label.as_str(), v)?;
        }
    }
    Ok(out)
}

fn timer(
    s: &mut Session,
    label_from: Option<String>,
    secs: f64,
    f: Function,
    every: bool,
) -> LResult<u64> {
    if s.scripts.timers.len() >= MAX_TIMERS {
        return Err(fail(format!("too many timers (at most {MAX_TIMERS})")));
    }
    if !secs.is_finite() || secs < 0.0 {
        return Err(fail("a timer's time is seconds, 0 or more"));
    }
    let d = Duration::from_secs_f64(secs).max(if every { MIN_INTERVAL } else { Duration::ZERO });
    let id = s.scripts.fresh_id();
    s.scripts.timers.push(Timer {
        id,
        label: label_from.unwrap_or_else(|| "Script".into()),
        due: Instant::now() + d,
        every: every.then_some(d),
        function: f,
    });
    Ok(id)
}

fn talking(lua: &Lua, ff: &Table) -> LResult<()> {
    add(
        lua,
        ff,
        "_ask",
        |_, s, (spec, default): (Value, Option<String>)| {
            let prompt = match &spec {
                Value::String(question) => Prompt {
                    id: 0,
                    script: String::new(),
                    title: question.to_string_lossy(),
                    text: None,
                    fields: vec![PromptField {
                        label: String::new(),
                        kind: FieldKind::Text,
                        default: default.unwrap_or_default(),
                    }],
                },
                Value::Table(t) => {
                    let mut fields = Vec::new();
                    if let Some(list) = t.get::<Option<Table>>("fields")? {
                        for v in list.sequence_values::<Value>() {
                            fields.push(field_from(&v?)?);
                        }
                    }
                    Prompt {
                        id: 0,
                        script: String::new(),
                        title: t
                            .get::<Option<String>>("title")?
                            .unwrap_or_else(|| "Script".into()),
                        text: t.get("text")?,
                        fields,
                    }
                }
                other => {
                    return Err(fail(format!(
                        "prompt: a question or a table expected, got {}",
                        other.type_name()
                    )));
                }
            };
            s.scripts.pending_prompt = Some(prompt);
            Ok(())
        },
    )?;
    add(lua, ff, "message", |_, s, text: String| {
        s.notify(crate::NoticeLevel::Info, text);
        Ok(())
    })?;
    add(lua, ff, "warn", |_, s, text: String| {
        s.notify(crate::NoticeLevel::Warning, text);
        Ok(())
    })?;
    add(lua, ff, "after", |_, s, (secs, f): (f64, Function)| {
        let label = s.scripts.running.clone();
        timer(s, label, secs, f, false)
    })?;
    add(lua, ff, "every", |_, s, (secs, f): (f64, Function)| {
        let label = s.scripts.running.clone();
        timer(s, label, secs, f, true)
    })?;
    add(lua, ff, "cancel", |_, s, id: u64| {
        s.scripts.timers.retain(|t| t.id != id);
        Ok(())
    })?;
    Ok(())
}
