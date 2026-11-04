//! The macro recorder: the actions the user dispatches, written as the
//! Lua that does them again (a macro is a script).
//!
//! * Tracks are named by name (by id when the name is not one track's),
//!   tracks the macro made by the variable it made them in (`local t1 =
//!   ff.add_track{…}`); clips likewise, and a list of clips that was the
//!   selection as `ff.selected_clips()` — so a macro acts on what is
//!   selected when it is played, as the user's edit did.
//! * A drag records its last motion (edits of one thing in a gesture
//!   replace each other) and a cancelled drag nothing.
//! * What only changes the view (zoom, layout, meters, opened editors) is
//!   left out silently; what a script cannot do is left out by name
//!   ([`super::MacroRecording::skipped`]) and listed in the macro.

use super::MacroRecording;
use crate::{Action, SelectMode, Session, TransportAction};
use faderframe_core::{ClipId, PluginInstanceId, TrackId};
use faderframe_project::{Command, MonitorMode, SendTap, TrackKind};
use faderframe_script::literal;
use faderframe_timeline::{MusicalTime, TICKS_PER_QUARTER};
use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

struct Line {
    code: String,
    /// Lines with the same key in a row replace each other (a drag).
    key: Option<String>,
}

pub(crate) struct Recorder {
    lines: Vec<Line>,
    skipped: Vec<String>,
    started: Instant,
    tracks: HashMap<TrackId, String>,
    clips: HashMap<ClipId, String>,
    vars: usize,
    /// Where the open gesture's lines start (a cancel takes them away).
    gesture_from: Option<usize>,
}

/// What the project held before an action (to find what it made).
pub(crate) struct Before {
    tracks: BTreeSet<TrackId>,
    names: HashMap<TrackId, String>,
    clips: BTreeSet<ClipId>,
    selected_clips: BTreeSet<ClipId>,
    inserts: HashMap<TrackId, Vec<PluginInstanceId>>,
}

impl Before {
    pub(crate) fn of(s: &Session) -> Self {
        Self {
            tracks: s.project.tracks.iter().map(|t| t.id).collect(),
            names: s
                .project
                .tracks
                .iter()
                .map(|t| (t.id, t.name.clone()))
                .collect(),
            clips: s.project.clips.keys().copied().collect(),
            selected_clips: s.selected_arranger_clips().into_iter().collect(),
            inserts: s
                .project
                .tracks
                .iter()
                .map(|t| (t.id, t.inserts.iter().map(|p| p.id).collect()))
                .collect(),
        }
    }
}

/// Write `action` (done, as `before` was) into the macro being recorded.
pub(crate) fn note(s: &mut Session, before: &Before, action: &Action) {
    let Some(mut rec) = s.scripts.recorder.take() else {
        return;
    };
    rec.hear(s, before, action);
    s.scripts.recorder = Some(rec);
}

fn q(t: MusicalTime) -> String {
    literal::number(t.quarters())
}

fn ticks(by: i64) -> String {
    literal::number(by as f64 / TICKS_PER_QUARTER as f64)
}

fn kind(k: TrackKind) -> &'static str {
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

/// The variant's name ("InsertPlugin").
fn variant(action: &Action) -> String {
    let text = format!("{action:?}");
    text.chars().take_while(|c| c.is_alphanumeric()).collect()
}

/// Actions that change only what is shown: never written, never missed.
fn view_only(action: &Action) -> bool {
    matches!(
        action,
        Action::Workspace(_)
            | Action::Zoom(_)
            | Action::SetTrackHeight { .. }
            | Action::SetTakeLaneHeight { .. }
            | Action::SetHeaderWidth(_)
            | Action::SetStripWidth { .. }
            | Action::SetMixerInsertSlots(_)
            | Action::SetPluginWindowPosition { .. }
            | Action::SetMeterMode { .. }
            | Action::SetVuReference(_)
            | Action::SetMeterBridge(_)
            | Action::SetDeviceView { .. }
            | Action::OpenPluginEditor { .. }
            | Action::OpenPluginBrowser { .. }
            | Action::OpenClipEditor(_)
            | Action::OpenEventList(_)
            | Action::ResetClipIndicators
            | Action::Audition { .. }
            | Action::AuditionOff
            | Action::ShowGlobalLane(..)
            | Action::ToggleFollowPlayhead
            | Action::ResetPerformance
            | Action::SetAnalysisSource(_)
            | Action::ResetAnalysis
            | Action::MidiLearn(_)
            | Action::CancelMidiLearn
            | Action::Script(_)
            | Action::Maps(
                crate::maps::MapOp::OpenEditor { .. }
                    | crate::maps::MapOp::LearnDrumPitch { .. }
                    | crate::maps::MapOp::CancelLearn
            )
    )
}

impl Recorder {
    pub(crate) fn new() -> Self {
        Self {
            lines: Vec::new(),
            skipped: Vec::new(),
            started: Instant::now(),
            tracks: HashMap::new(),
            clips: HashMap::new(),
            vars: 0,
            gesture_from: None,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub(crate) fn status(&self) -> MacroRecording {
        MacroRecording {
            steps: self.lines.len(),
            skipped: self.skipped.clone(),
            seconds: self.started.elapsed().as_secs_f64(),
        }
    }

    /// The macro's description and its Lua.
    pub(crate) fn script(&self) -> (String, String) {
        let n = self.lines.len();
        let description = format!("Recorded macro, {n} step{}", if n == 1 { "" } else { "s" });
        let mut body = String::new();
        if !self.skipped.is_empty() {
            body.push_str("-- Not recorded (no script equivalent): ");
            body.push_str(&self.skipped.join(", "));
            body.push_str("\n\n");
        }
        for l in &self.lines {
            body.push_str(&l.code);
            body.push('\n');
        }
        (description, body)
    }

    fn push(&mut self, code: String, key: Option<String>) {
        let in_gesture = self
            .gesture_from
            .is_some_and(|from| self.lines.len() > from);
        if let (Some(k), Some(last)) = (&key, self.lines.last_mut())
            && last.key.as_ref() == Some(k)
            && in_gesture
        {
            last.code = code;
            return;
        }
        self.lines.push(Line { code, key });
    }

    fn skip(&mut self, what: String) {
        if !self.skipped.contains(&what) {
            self.skipped.push(what);
        }
    }

    fn var(&mut self, prefix: &str) -> String {
        self.vars += 1;
        format!("{prefix}{}", self.vars)
    }

    /// How the macro names `track`.
    fn track(&self, s: &Session, before: &Before, track: TrackId) -> String {
        if let Some(v) = self.tracks.get(&track) {
            return v.clone();
        }
        let name = s
            .project
            .track(track)
            .map(|t| t.name.clone())
            .or_else(|| before.names.get(&track).cloned());
        match name {
            Some(n) if s.project.tracks.iter().filter(|t| t.name == n).count() <= 1 => {
                literal::string(&n)
            }
            _ => track.0.to_string(),
        }
    }

    fn clip(&self, clip: ClipId) -> String {
        self.clips
            .get(&clip)
            .cloned()
            .unwrap_or_else(|| clip.0.to_string())
    }

    /// A list of clips: the selection when it was the selection.
    fn clips(&self, before: &Before, clips: &[ClipId]) -> String {
        let set: BTreeSet<ClipId> = clips.iter().copied().collect();
        if !set.is_empty() && set == before.selected_clips {
            return "ff.selected_clips()".into();
        }
        literal::list(&clips.iter().map(|c| self.clip(*c)).collect::<Vec<_>>())
    }

    /// A device's place on its track (1 = the first insert).
    fn place(
        s: &Session,
        before: &Before,
        track: TrackId,
        plugin: PluginInstanceId,
    ) -> Option<usize> {
        s.project
            .track(track)
            .and_then(|t| t.inserts.iter().position(|p| p.id == plugin))
            .or_else(|| {
                before
                    .inserts
                    .get(&track)?
                    .iter()
                    .position(|p| *p == plugin)
            })
            .map(|i| i + 1)
    }

    fn hear(&mut self, s: &Session, before: &Before, action: &Action) {
        if view_only(action) {
            return;
        }
        use Action as A;
        match action {
            A::BeginGesture(_) => {
                if self.gesture_from.is_none() {
                    self.gesture_from = Some(self.lines.len());
                }
            }
            A::EndGesture => self.gesture_from = None,
            A::CancelGesture => {
                if let Some(from) = self.gesture_from.take() {
                    self.lines.truncate(from);
                }
            }
            A::Several(all) => {
                for a in all {
                    self.hear(s, before, a);
                }
            }
            A::Transport(t) => self.transport(t),
            A::SelectTracks { tracks, mode } => {
                let list: Vec<String> = tracks.iter().map(|t| self.track(s, before, *t)).collect();
                let code = format!(
                    "ff.select{{ tracks = {}{} }}",
                    literal::list(&list),
                    mode_text(*mode)
                );
                self.push(code, None);
            }
            A::SelectClips { clips, mode } => {
                let list: Vec<String> = clips.iter().map(|c| self.clip(*c)).collect();
                let code = format!(
                    "ff.select{{ clips = {}{} }}",
                    literal::list(&list),
                    mode_text(*mode)
                );
                self.push(code, None);
            }
            A::ClearSelection => self.push("ff.clear_selection()".into(), None),
            A::SetEditRange(Some(r)) => self.push(
                format!("ff.select{{ range = {{ {}, {} }} }}", q(r.start), q(r.end)),
                Some("range".into()),
            ),
            A::SetEditRange(None) => self.push("ff.select{ range = false }".into(), None),
            A::Separate => self.edit("separate"),
            A::TrimToSelection => self.edit("trim-to-selection"),
            A::ClearRange => self.edit("clear-range"),
            A::CopyRange => self.edit("copy-range"),
            A::CutRange => self.edit("cut-range"),
            A::PasteRange => self.edit("paste-range"),
            A::RepeatRange(n) => self.push(format!("ff.edit(\"repeat-range\", {n})"), None),
            A::InsertSilence => self.edit("insert-silence"),
            A::Copy => self.edit("copy"),
            A::Cut => self.edit("cut"),
            A::Paste => self.edit("paste"),
            A::Repeat(n) => self.push(format!("ff.edit(\"repeat\", {n})"), None),
            A::SelectAllClips => self.edit("select-all"),
            A::SplitSelectedAtPlayhead => self.edit("split"),
            A::DeleteSelection => self.edit("delete"),
            A::DeleteInArranger => self.edit("delete-clips"),
            A::RemoveSelectedTracks => self.edit("remove-tracks"),
            A::ToggleArmSelected => self.edit("arm"),
            A::GroupSelectedTracks => self.edit("group"),
            A::QuantizeClips(c) | A::HumanizeClips(c) | A::MergeClips(c) => {
                let name = match action {
                    A::QuantizeClips(_) => "quantize",
                    A::HumanizeClips(_) => "humanize",
                    _ => "merge",
                };
                let list = self.clips(before, c);
                if c.is_empty() || list == "ff.selected_clips()" {
                    self.edit(name);
                } else {
                    self.push(format!("ff.edit({}, {list})", literal::string(name)), None);
                }
            }
            A::DuplicateTracks { tracks, content } if tracks.is_empty() => {
                self.push(format!("ff.edit(\"duplicate-tracks\", {content})"), None);
            }
            A::Nudge { forward, target } => {
                let t = match target {
                    crate::editing::NudgeTarget::Move => "move",
                    crate::editing::NudgeTarget::TrimStart => "trim-start",
                    crate::editing::NudgeTarget::TrimEnd => "trim-end",
                };
                self.push(format!("ff.nudge({forward}, \"{t}\")"), None);
            }
            A::AddTrack(k) => self.made_tracks(s, before, *k, None),
            A::AddTrackWithLayout(k, layout) => match layout {
                faderframe_core::ChannelLayout::Mono => {
                    self.made_tracks(s, before, *k, Some("mono"))
                }
                faderframe_core::ChannelLayout::Stereo => {
                    self.made_tracks(s, before, *k, Some("stereo"));
                }
                _ => self.skip("Add Track (surround or ambisonic format)".into()),
            },
            A::AddMarker(at) => self.push(format!("ff.add_marker({})", q(*at)), None),
            A::AddSection { start, end } => {
                self.push(format!("ff.add_section({}, {})", q(*start), q(*end)), None);
            }
            A::CreateMidiClip {
                track,
                start,
                length,
            } => {
                let t = self.track(s, before, *track);
                let made: Vec<ClipId> = s
                    .project
                    .clips
                    .keys()
                    .filter(|c| !before.clips.contains(c))
                    .copied()
                    .collect();
                let call = format!("ff.add_midi_clip({t}, {}, {})", q(*start), q(*length));
                match made.as_slice() {
                    [one] => {
                        let v = self.var("c");
                        self.clips.insert(*one, v.clone());
                        self.push(format!("local {v} = {call}"), None);
                    }
                    _ => self.push(call, None),
                }
            }
            A::AddNote {
                clip,
                start,
                length,
                key,
                velocity,
            } => {
                let c = self.clip(*clip);
                self.push(
                    format!(
                        "ff.add_note({c}, {{ key = {key}, start = {}, length = {}, velocity = {velocity} }})",
                        q(*start),
                        q(*length)
                    ),
                    None,
                );
            }
            A::MoveClips { clips, by, tracks } => {
                let list = self.clips(before, clips);
                self.push(
                    format!("ff.move_clips({list}, {}, {tracks})", ticks(*by)),
                    Some("move-clips".into()),
                );
            }
            A::TrimClips {
                clips,
                edge,
                by,
                stretch,
            } => {
                let list = self.clips(before, clips);
                let edge = match edge {
                    crate::editing::ClipEdge::Start => "start",
                    crate::editing::ClipEdge::End => "end",
                };
                self.push(
                    format!(
                        "ff.trim_clips({list}, \"{edge}\", {}, {stretch})",
                        ticks(*by)
                    ),
                    Some(format!("trim-clips-{edge}")),
                );
            }
            A::ClipGain { clips, delta_db } => {
                let list = self.clips(before, clips);
                self.push(
                    format!(
                        "ff.clip_gain({list}, {})",
                        literal::number(f64::from(*delta_db))
                    ),
                    Some("clip-gain".into()),
                );
            }
            A::SetClipsGain { clips, db } => {
                let list = self.clips(before, clips);
                self.push(
                    format!(
                        "ff.set_clip({list}, {{ gain = {} }})",
                        literal::number(f64::from(*db))
                    ),
                    Some("clips-gain".into()),
                );
            }
            A::SetClipsMuted { clips, muted } => {
                let list = self.clips(before, clips);
                self.push(format!("ff.set_clip({list}, {{ muted = {muted} }})"), None);
            }
            A::InsertPlugin {
                track,
                index,
                plugin,
            } => {
                let t = self.track(s, before, *track);
                let spec = literal::table(&[
                    ("format", literal::string(format_name(plugin.format))),
                    ("id", literal::string(&plugin.id)),
                ]);
                self.push(
                    format!("ff.insert_plugin({t}, {spec}, {})", index + 1),
                    None,
                );
            }
            A::AddSend {
                track,
                target,
                level_db,
                tap,
            } => {
                let t = self.track(s, before, *track);
                let d = self.track(s, before, *target);
                let tap = match tap {
                    SendTap::PreFx => "pre-fx",
                    SendTap::PreFader => "pre",
                    SendTap::PostFader => "post",
                };
                self.push(
                    format!(
                        "ff.add_send({t}, {d}, {{ level = {}, tap = \"{tap}\" }})",
                        literal::number(f64::from(*level_db))
                    ),
                    None,
                );
            }
            A::Edit(cmd) => self.command(s, before, cmd),
            A::Undo | A::Redo | A::HistoryTo(_) | A::HistoryToNode(_) | A::HistoryBranch(_) => {
                self.skip("Undo/Redo".into());
            }
            other => self.skip(variant(other)),
        }
    }

    fn edit(&mut self, name: &str) {
        self.push(format!("ff.edit({})", literal::string(name)), None);
    }

    fn transport(&mut self, t: &TransportAction) {
        use TransportAction as T;
        let code = match t {
            T::Play => "ff.play()".into(),
            T::Stop => "ff.stop()".into(),
            T::TogglePlay => "ff.toggle_play()".into(),
            T::Locate(at) => format!("ff.locate({})", q(*at)),
            T::ReturnToStart => "ff.return_to_start()".into(),
            T::ToggleLoop => "ff.toggle_loop()".into(),
            T::ToggleRecord => "ff.record()".into(),
            T::NudgeBars(n) => format!("ff.nudge_bars({n})"),
            T::SetLoop(Some(r)) => format!("ff.set_loop({}, {})", q(r.start), q(r.end)),
            T::SetLoop(None) => "ff.set_loop(nil)".into(),
            other => {
                let text = format!("{other:?}");
                self.skip(text.chars().take_while(|c| c.is_alphanumeric()).collect());
                return;
            }
        };
        let key = matches!(t, T::Locate(_)).then(|| "locate".to_string());
        self.push(code, key);
    }

    fn made_tracks(&mut self, s: &Session, before: &Before, k: TrackKind, layout: Option<&str>) {
        let made: Vec<TrackId> = s
            .project
            .tracks
            .iter()
            .filter(|t| !before.tracks.contains(&t.id))
            .map(|t| t.id)
            .collect();
        let mut fields = vec![("kind", literal::string(kind(k)))];
        if let Some(l) = layout {
            fields.push(("layout", literal::string(l)));
        }
        let call = format!("ff.add_track{}", literal::table(&fields));
        match made.as_slice() {
            [one] => {
                let v = self.var("t");
                self.tracks.insert(*one, v.clone());
                self.push(format!("local {v} = {call}"), None);
            }
            _ => self.push(call, None),
        }
    }

    fn command(&mut self, s: &Session, before: &Before, cmd: &Command) {
        use Command as C;
        let set = |me: &Self, track: &TrackId, field: &str, value: String| {
            (
                format!(
                    "ff.set_track({}, {{ {field} = {value} }})",
                    me.track(s, before, *track)
                ),
                Some(format!("track-{}-{field}", track.0)),
            )
        };
        let (code, key) = match cmd {
            C::SetTrackVolume { track, db } => {
                set(self, track, "volume", literal::number(f64::from(*db)))
            }
            C::SetTrackPan { track, pan } => {
                set(self, track, "pan", literal::number(f64::from(*pan)))
            }
            C::SetTrackMute { track, on } => set(self, track, "mute", on.to_string()),
            C::SetTrackSolo { track, on } => set(self, track, "solo", on.to_string()),
            C::SetTrackRecordArm { track, on } => set(self, track, "arm", on.to_string()),
            C::SetTrackPhaseInvert { track, on } => set(self, track, "phase", on.to_string()),
            C::SetTrackDelay { track, ms } => set(self, track, "delay", literal::number(*ms)),
            C::RenameTrack { track, name } => set(self, track, "name", literal::string(name)),
            C::SetTrackColor { track, color } => {
                set(self, track, "color", literal::string(&color.to_hex()))
            }
            C::SetTrackMonitor { track, mode } => set(
                self,
                track,
                "monitor",
                literal::string(match mode {
                    MonitorMode::Off => "off",
                    MonitorMode::Input => "input",
                    MonitorMode::Auto => "auto",
                }),
            ),
            C::MoveTrack { track, index } => (
                format!(
                    "ff.move_track({}, {})",
                    self.track(s, before, *track),
                    index + 1
                ),
                None,
            ),
            C::RemoveTrack { track } => (
                format!("ff.remove_track({})", self.track(s, before, *track)),
                None,
            ),
            C::SetTempo { bpm } => (
                format!("ff.set_tempo({})", literal::number(*bpm)),
                Some("tempo".into()),
            ),
            C::SetLoop { range: Some(r), .. } => (
                format!("ff.set_loop({}, {})", q(r.start), q(r.end)),
                Some("loop".into()),
            ),
            C::SetLoop { range: None, .. } => ("ff.set_loop(nil)".into(), Some("loop".into())),
            C::RenameClip { clip, name } => (
                format!(
                    "ff.set_clip({}, {{ name = {} }})",
                    self.clip(*clip),
                    literal::string(name)
                ),
                None,
            ),
            C::SetClipMuted { clip, muted } => (
                format!("ff.set_clip({}, {{ muted = {muted} }})", self.clip(*clip)),
                None,
            ),
            C::RemoveClip { clip } => (format!("ff.remove_clip({})", self.clip(*clip)), None),
            C::MoveClip { clip, track, start } => (
                format!(
                    "ff.move_clip({}, {{ start = {}, track = {} }})",
                    self.clip(*clip),
                    q(*start),
                    self.track(s, before, *track)
                ),
                Some(format!("move-clip-{}", clip.0)),
            ),
            C::RemoveMarker { marker } => (format!("ff.remove_marker({})", marker.0), None),
            C::UpdateMarker { marker } => (
                format!(
                    "ff.set_marker({}, {{ name = {}, at = {} }})",
                    marker.id.0,
                    literal::string(&marker.name),
                    q(marker.position)
                ),
                Some(format!("marker-{}", marker.id.0)),
            ),
            C::SetPluginBypass {
                track,
                plugin,
                bypass,
            } => {
                let Some(place) = Self::place(s, before, *track, *plugin) else {
                    return self.skip(cmd.label());
                };
                (
                    format!(
                        "ff.set_plugin({}, {place}, {{ bypass = {bypass} }})",
                        self.track(s, before, *track)
                    ),
                    None,
                )
            }
            C::SetPluginParameter {
                track,
                plugin,
                parameter,
                value: Some(v),
            } => {
                let Some(place) = Self::place(s, before, *track, *plugin) else {
                    return self.skip(cmd.label());
                };
                (
                    format!(
                        "ff.set_plugin({}, {place}, {{ params = {{ [{}] = {} }} }})",
                        self.track(s, before, *track),
                        parameter.0,
                        literal::number(*v)
                    ),
                    Some(format!("param-{}-{}", plugin.0, parameter.0)),
                )
            }
            C::RemovePlugin { track, plugin } => {
                let Some(place) = Self::place(s, before, *track, *plugin) else {
                    return self.skip(cmd.label());
                };
                (
                    format!(
                        "ff.remove_plugin({}, {place})",
                        self.track(s, before, *track)
                    ),
                    None,
                )
            }
            other => return self.skip(other.label()),
        };
        self.push(code, key);
    }
}

fn mode_text(mode: SelectMode) -> &'static str {
    match mode {
        SelectMode::Replace => "",
        SelectMode::Add => ", mode = \"add\"",
        SelectMode::Toggle => ", mode = \"toggle\"",
    }
}

fn format_name(f: faderframe_project::PluginFormat) -> &'static str {
    use faderframe_project::PluginFormat as F;
    match f {
        F::Builtin => "builtin",
        F::Clap => "clap",
        F::Vst3 => "vst3",
        F::AudioUnit => "au",
        F::Lv2 => "lv2",
    }
}
