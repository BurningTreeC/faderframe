//! The interactive score: music for a game, as middleware (Wwise, FMOD)
//! plays it. The game asks for a *state* (Explore, Combat, Boss …) and the
//! music moves there on a sync point, as a transition rule says; *layers*
//! (groups of tracks) fade in and out with *parameters* (intensity);
//! *stingers* are short phrases played over the music on a sync point.
//!
//! A state is a song section (it loops between its entry and exit cues,
//! the bars before the entry a pickup played on the way in) or a launcher
//! scene (its clips loop as the launcher plays them). Rules match a source
//! and a destination state (either may be "any"); the most specific one
//! that matches wins, the first of equals, and without one the music moves
//! on the next bar ([`TransitionRule::fallback`]). A rule's sync point is
//! read in the playing state — beats, bars and bar grids from the tempo
//! and metre there, project markers, the state's own named cues, its exit
//! cue — and a transition segment (another state) may play once in between.
//!
//! Everything here is plain data and pure functions: [`sync_point`] finds
//! where a transition lands, [`InteractiveScore::layer_gains`] what the
//! parameters make of the layers. The session plays it
//! (`faderframe_session::adaptive`) and exports it for game engines.

use crate::Project;
use crate::launcher::SlotKey;
use crate::scenes::FadeLength;
use faderframe_core::{
    MusicLayerId, MusicStateId, SceneId, ScoreParamId, SectionId, StingerId, TrackId, TransitionId,
};
use faderframe_timeline::{MusicalTime, Timeline};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The quietest a layer gets (dB): silent.
pub const FLOOR_DB: f32 = -96.0;

/// How far ahead a sync point is looked for (seconds of playback).
pub const REACH_SECONDS: f64 = 3600.0;

/// What a state plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum StateSource {
    /// A song section of the arrangement.
    Section { section: SectionId },
    /// A row of the clip launcher.
    Scene { scene: SceneId },
}

/// A named sync point in a state, from its start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateCue {
    pub name: String,
    pub at: MusicalTime,
}

/// A state of the score (see the module docs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MusicState {
    pub id: MusicStateId,
    pub name: String,
    pub source: StateSource,
    /// Where its loop begins, from its start (the entry cue): what comes
    /// before is a pickup, played on the way in.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub entry: MusicalTime,
    /// Where its loop ends, from its start (the exit cue; `None`: its end).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<MusicalTime>,
    /// It loops (else it plays once and goes on to `then`, or stops).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub looping: bool,
    /// Where a state that plays once goes when it ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<MusicStateId>,
    /// Named sync points, by position.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cues: Vec<StateCue>,
}

fn yes() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

fn instant(f: &FadeLength) -> bool {
    f.is_instant()
}

fn is_zero(t: &MusicalTime) -> bool {
    *t == MusicalTime::ZERO
}

impl MusicState {
    pub fn new(id: MusicStateId, name: impl Into<String>, source: StateSource) -> Self {
        Self {
            id,
            name: name.into(),
            source,
            entry: MusicalTime::ZERO,
            exit: None,
            looping: true,
            then: None,
            cues: Vec::new(),
        }
    }
}

/// Tracks heard together, faded by parameters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MusicLayer {
    pub id: MusicLayerId,
    pub name: String,
    #[serde(default)]
    pub tracks: Vec<TrackId>,
}

/// Where a transition leaves the playing state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SyncPoint {
    /// At once.
    Immediate,
    /// The next beat (the metre's beat there).
    Beat,
    /// The next bar line.
    #[default]
    Bar,
    /// The next line of a grid of this many bars, counted from the
    /// state's start.
    Grid { bars: u16 },
    /// The next project marker.
    Marker,
    /// The next of the state's named cues (`name` empty: any of them).
    Cue {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        name: String,
    },
    /// The state's exit cue: the end of its loop ("end of section").
    Exit,
}

impl SyncPoint {
    /// The choices menus offer (named cues by name besides).
    pub const COMMON: [SyncPoint; 7] = [
        SyncPoint::Immediate,
        SyncPoint::Beat,
        SyncPoint::Bar,
        SyncPoint::Grid { bars: 2 },
        SyncPoint::Grid { bars: 4 },
        SyncPoint::Marker,
        SyncPoint::Exit,
    ];

    pub fn label(&self) -> String {
        match self {
            SyncPoint::Immediate => "Immediate".into(),
            SyncPoint::Beat => "Next Beat".into(),
            SyncPoint::Bar => "Next Bar".into(),
            SyncPoint::Grid { bars } => format!("Next {} Bars", (*bars).max(1)),
            SyncPoint::Marker => "Next Marker".into(),
            SyncPoint::Cue { name } if name.is_empty() => "Next Cue".into(),
            SyncPoint::Cue { name } => format!("Cue ‘{name}’"),
            SyncPoint::Exit => "End of Section (Exit Cue)".into(),
        }
    }

    /// A short form for lists: "bar", "4 bars", "exit" …
    pub fn short(&self) -> String {
        match self {
            SyncPoint::Immediate => "now".into(),
            SyncPoint::Beat => "beat".into(),
            SyncPoint::Bar => "bar".into(),
            SyncPoint::Grid { bars } => format!("{} bars", (*bars).max(1)),
            SyncPoint::Marker => "marker".into(),
            SyncPoint::Cue { name } if name.is_empty() => "cue".into(),
            SyncPoint::Cue { name } => format!("cue {name}"),
            SyncPoint::Exit => "exit".into(),
        }
    }

    /// Read back what [`Self::short`] writes (and a few spellings more:
    /// `now`, `beat`, `bar`, `4bars`, `marker`, `cue`, `cue=Name`, `exit`,
    /// `end`).
    pub fn parse(text: &str) -> Option<SyncPoint> {
        let t = text.trim();
        let lower = t.to_lowercase();
        Some(match lower.as_str() {
            "now" | "immediate" => SyncPoint::Immediate,
            "beat" => SyncPoint::Beat,
            "bar" => SyncPoint::Bar,
            "marker" => SyncPoint::Marker,
            "cue" => SyncPoint::Cue {
                name: String::new(),
            },
            "exit" | "end" => SyncPoint::Exit,
            _ => {
                if let Some(name) = t.strip_prefix("cue=").or_else(|| t.strip_prefix("cue ")) {
                    return Some(SyncPoint::Cue {
                        name: name.trim().to_string(),
                    });
                }
                let n = lower.trim_end_matches("bars").trim_end_matches("bar");
                let bars = n.trim().parse::<u16>().ok()?;
                SyncPoint::Grid { bars: bars.max(1) }
            }
        })
    }
}

/// Where the destination starts playing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    /// From its start: its pickup, then its loop.
    #[default]
    Start,
    /// From its entry cue (the pickup left out).
    EntryCue,
    /// As far into its loop as the source was into its own.
    SameTime,
}

impl Entry {
    pub const ALL: [Entry; 3] = [Entry::Start, Entry::EntryCue, Entry::SameTime];

    pub fn label(self) -> &'static str {
        match self {
            Entry::Start => "From Its Start (Pickup)",
            Entry::EntryCue => "From Its Entry Cue",
            Entry::SameTime => "At the Same Place in the Loop",
        }
    }
}

/// One end of a rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RuleEnd {
    #[default]
    Any,
    State {
        state: MusicStateId,
    },
}

impl RuleEnd {
    pub fn matches(self, state: Option<MusicStateId>) -> bool {
        match self {
            RuleEnd::Any => true,
            RuleEnd::State { state: s } => state == Some(s),
        }
    }

    pub fn state(self) -> Option<MusicStateId> {
        match self {
            RuleEnd::Any => None,
            RuleEnd::State { state } => Some(state),
        }
    }
}

/// How the music moves from one state to another.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransitionRule {
    pub id: TransitionId,
    #[serde(default)]
    pub from: RuleEnd,
    #[serde(default)]
    pub to: RuleEnd,
    #[serde(default)]
    pub sync: SyncPoint,
    /// The source fades out before the sync point; the destination fades
    /// in after it.
    #[serde(default, skip_serializing_if = "instant")]
    pub fade_out: FadeLength,
    #[serde(default, skip_serializing_if = "instant")]
    pub fade_in: FadeLength,
    /// A state played once in between.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment: Option<MusicStateId>,
    #[serde(default)]
    pub entry: Entry,
}

impl TransitionRule {
    pub fn new(id: TransitionId, from: RuleEnd, to: RuleEnd) -> Self {
        Self {
            id,
            from,
            to,
            sync: SyncPoint::Bar,
            fade_out: FadeLength::Instant,
            fade_in: FadeLength::Instant,
            segment: None,
            entry: Entry::Start,
        }
    }

    /// What applies without a rule: the next bar, no fades, from the
    /// destination's start.
    pub fn fallback() -> Self {
        Self::new(TransitionId(0), RuleEnd::Any, RuleEnd::Any)
    }

    /// How specific it is (both ends named first).
    fn rank(&self) -> u8 {
        match (self.from, self.to) {
            (RuleEnd::State { .. }, RuleEnd::State { .. }) => 3,
            (RuleEnd::State { .. }, RuleEnd::Any) => 2,
            (RuleEnd::Any, RuleEnd::State { .. }) => 1,
            (RuleEnd::Any, RuleEnd::Any) => 0,
        }
    }
}

/// A short phrase played over the music on a sync point: a launcher clip
/// that plays once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stinger {
    pub id: StingerId,
    pub name: String,
    pub slot: SlotKey,
    #[serde(default)]
    pub sync: SyncPoint,
    /// How much of it plays before the sync point (a pickup).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pickup: MusicalTime,
}

/// A point of a layer's curve: at parameter `value` the layer is at `db`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub value: f32,
    pub db: f32,
}

/// What a parameter does to one layer: its level over the parameter's
/// range (amplitude interpolated between points, held past the ends).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerCurve {
    pub layer: MusicLayerId,
    pub points: Vec<CurvePoint>,
}

impl LayerCurve {
    /// Silent below `from`, full above `to`, faded in between.
    pub fn fade_in(layer: MusicLayerId, from: f32, to: f32) -> Self {
        let (a, b) = if to < from { (to, from) } else { (from, to) };
        Self {
            layer,
            points: vec![
                CurvePoint {
                    value: a,
                    db: FLOOR_DB,
                },
                CurvePoint { value: b, db: 0.0 },
            ],
        }
    }

    /// The layer's level (dB) at `value`.
    pub fn db_at(&self, value: f32) -> f32 {
        let pts = &self.points;
        let Some(first) = pts.first() else {
            return 0.0;
        };
        if pts.len() == 1 || value <= first.value {
            return first.db;
        }
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if value <= b.value {
                let span = (b.value - a.value).max(f32::EPSILON);
                let t = ((value - a.value) / span).clamp(0.0, 1.0);
                let ga = gain_of(a.db);
                let gb = gain_of(b.db);
                return db_of(ga + (gb - ga) * t);
            }
        }
        pts.last().map_or(0.0, |p| p.db)
    }

    /// The curve as `n` points or more a segment (middleware draws lines
    /// between points in dB; these follow the amplitude curve closely).
    pub fn sampled(&self, per_segment: usize) -> Vec<CurvePoint> {
        let n = per_segment.max(1);
        let mut out = Vec::new();
        for w in self.points.windows(2) {
            for k in 0..n {
                let value = w[0].value + (w[1].value - w[0].value) * k as f32 / n as f32;
                out.push(CurvePoint {
                    value,
                    db: self.db_at(value),
                });
            }
        }
        if let Some(last) = self.points.last() {
            out.push(*last);
        }
        out
    }

    fn tidy(&mut self) {
        for p in &mut self.points {
            p.db = if p.db.is_finite() {
                p.db.clamp(FLOOR_DB, 12.0)
            } else {
                FLOOR_DB
            };
            if !p.value.is_finite() {
                p.value = 0.0;
            }
        }
        self.points.sort_by(|a, b| a.value.total_cmp(&b.value));
    }
}

fn gain_of(db: f32) -> f32 {
    if db <= FLOOR_DB {
        0.0
    } else {
        faderframe_core::db_to_gain(db)
    }
}

fn db_of(gain: f32) -> f32 {
    if gain <= faderframe_core::db_to_gain(FLOOR_DB) {
        FLOOR_DB
    } else {
        faderframe_core::gain_to_db(gain).max(FLOOR_DB)
    }
}

/// A value the game sets (intensity, danger …) and what it does to the
/// layers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoreParameter {
    pub id: ScoreParamId,
    pub name: String,
    #[serde(default)]
    pub min: f32,
    #[serde(default = "one")]
    pub max: f32,
    #[serde(default)]
    pub default: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curves: Vec<LayerCurve>,
}

fn one() -> f32 {
    1.0
}

impl ScoreParameter {
    pub fn new(id: ScoreParamId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            curves: Vec::new(),
        }
    }

    pub fn clamp(&self, v: f32) -> f32 {
        if v.is_finite() {
            v.clamp(self.min, self.max)
        } else {
            self.default
        }
    }

    pub fn curve(&self, layer: MusicLayerId) -> Option<&LayerCurve> {
        self.curves.iter().find(|c| c.layer == layer)
    }
}

/// How the score is exported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoreExport {
    /// Seconds rendered after each state for reverb and echo tails.
    #[serde(default = "default_tail")]
    pub tail_seconds: f32,
    #[serde(default = "default_format")]
    pub format: faderframe_audio_files::WavFormat,
    /// The files' rate (`None`: the project's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
}

fn default_tail() -> f32 {
    4.0
}

fn default_format() -> faderframe_audio_files::WavFormat {
    faderframe_audio_files::WavFormat::Pcm24
}

impl Default for ScoreExport {
    fn default() -> Self {
        Self {
            tail_seconds: default_tail(),
            format: default_format(),
            sample_rate: None,
        }
    }
}

impl ScoreExport {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The score (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InteractiveScore {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<MusicState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<MusicLayer>,
    /// In order of precedence among equally specific rules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<TransitionRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stingers: Vec<Stinger>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<ScoreParameter>,
    /// The state the music starts in (`None`: the first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<MusicStateId>,
    #[serde(default, skip_serializing_if = "ScoreExport::is_default")]
    pub export: ScoreExport,
}

impl InteractiveScore {
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
            && self.layers.is_empty()
            && self.rules.is_empty()
            && self.stingers.is_empty()
            && self.parameters.is_empty()
    }

    pub fn state(&self, id: MusicStateId) -> Option<&MusicState> {
        self.states.iter().find(|s| s.id == id)
    }

    pub fn state_mut(&mut self, id: MusicStateId) -> Option<&mut MusicState> {
        self.states.iter_mut().find(|s| s.id == id)
    }

    pub fn layer(&self, id: MusicLayerId) -> Option<&MusicLayer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn rule(&self, id: TransitionId) -> Option<&TransitionRule> {
        self.rules.iter().find(|r| r.id == id)
    }

    pub fn stinger(&self, id: StingerId) -> Option<&Stinger> {
        self.stingers.iter().find(|s| s.id == id)
    }

    pub fn parameter(&self, id: ScoreParamId) -> Option<&ScoreParameter> {
        self.parameters.iter().find(|p| p.id == id)
    }

    /// The state a state's name or place (from 1) names.
    pub fn state_named(&self, text: &str) -> Option<&MusicState> {
        let t = text.trim();
        if let Ok(n) = t.parse::<usize>() {
            return n.checked_sub(1).and_then(|i| self.states.get(i));
        }
        self.states.iter().find(|s| s.name.eq_ignore_ascii_case(t))
    }

    /// The state the music starts in.
    pub fn first_state(&self) -> Option<MusicStateId> {
        self.start
            .filter(|s| self.state(*s).is_some())
            .or_else(|| self.states.first().map(|s| s.id))
    }

    /// The rule for moving from `from` (`None`: nothing plays) to `to`:
    /// the most specific that matches, the first of equals (see the module
    /// docs); the fallback without one.
    pub fn rule_between(&self, from: Option<MusicStateId>, to: MusicStateId) -> TransitionRule {
        let mut best: Option<&TransitionRule> = None;
        for r in &self.rules {
            if !r.from.matches(from) || !r.to.matches(Some(to)) {
                continue;
            }
            if best.is_none_or(|b| r.rank() > b.rank()) {
                best = Some(r);
            }
        }
        best.cloned().unwrap_or_else(TransitionRule::fallback)
    }

    /// Each layer's level (dB) for these parameter values (missing ones at
    /// their defaults): the sum of what every parameter's curve says.
    pub fn layer_gains(&self, values: &BTreeMap<ScoreParamId, f32>) -> BTreeMap<MusicLayerId, f32> {
        let mut out: BTreeMap<MusicLayerId, f32> =
            self.layers.iter().map(|l| (l.id, 0.0)).collect();
        for p in &self.parameters {
            let v = p.clamp(values.get(&p.id).copied().unwrap_or(p.default));
            for c in &p.curves {
                if let Some(db) = out.get_mut(&c.layer) {
                    *db = (*db + c.db_at(v)).max(FLOOR_DB);
                }
            }
        }
        out
    }

    /// Each track's gain (linear) from the layers it is in (tracks in no
    /// layer are left out: unity).
    pub fn track_gains(&self, values: &BTreeMap<ScoreParamId, f32>) -> BTreeMap<TrackId, f32> {
        let layers = self.layer_gains(values);
        let mut out: BTreeMap<TrackId, f32> = BTreeMap::new();
        for l in &self.layers {
            let g = gain_of(layers.get(&l.id).copied().unwrap_or(0.0));
            for t in &l.tracks {
                *out.entry(*t).or_insert(1.0) *= g;
            }
        }
        out
    }

    /// Values kept sane (curves sorted, ranges ordered, cues in their
    /// states, references to gone states dropped).
    pub fn tidy(&mut self) {
        let known: Vec<MusicStateId> = self.states.iter().map(|s| s.id).collect();
        for s in &mut self.states {
            s.entry = s.entry.max(MusicalTime::ZERO);
            if let Some(e) = s.exit
                && e <= s.entry
            {
                s.exit = None;
            }
            if s.then.is_some_and(|t| !known.contains(&t)) {
                s.then = None;
            }
            s.cues.retain(|c| c.at >= MusicalTime::ZERO);
            s.cues.sort_by_key(|c| c.at);
        }
        for r in &mut self.rules {
            if r.segment.is_some_and(|t| !known.contains(&t)) {
                r.segment = None;
            }
            r.fade_in = r.fade_in.clamped();
            r.fade_out = r.fade_out.clamped();
        }
        for p in &mut self.parameters {
            if !(p.min.is_finite() && p.max.is_finite()) || p.max <= p.min {
                p.min = 0.0;
                p.max = 1.0;
            }
            p.default = p.clamp(p.default);
            for c in &mut p.curves {
                c.tidy();
            }
        }
        if self.start.is_some_and(|s| !known.contains(&s)) {
            self.start = None;
        }
        self.export.tail_seconds = if self.export.tail_seconds.is_finite() {
            self.export.tail_seconds.clamp(0.0, 60.0)
        } else {
            default_tail()
        };
    }
}

/// Where a state sits in musical time: from its start to its end, its loop
/// between (a launcher scene's from the song's start: it plays from there
/// in renders).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateSpan {
    pub start: MusicalTime,
    pub end: MusicalTime,
    pub loop_start: MusicalTime,
    pub loop_end: MusicalTime,
}

impl StateSpan {
    pub fn length(&self) -> MusicalTime {
        self.end - self.start
    }
}

impl Project {
    /// The interactive score's state `state`'s span (`None`: its section
    /// or scene is gone, or the scene holds no clip).
    pub fn state_span(&self, state: &MusicState) -> Option<StateSpan> {
        let (start, end) = match state.source {
            StateSource::Section { section } => {
                let s = self.sections.iter().find(|s| s.id == section)?;
                (s.start, s.end)
            }
            StateSource::Scene { scene } => (MusicalTime::ZERO, self.scene_length(scene)?),
        };
        if end <= start {
            return None;
        }
        let loop_start = (start + state.entry).min(end);
        let loop_end = state
            .exit
            .map_or(end, |e| (start + e).min(end))
            .max(loop_start);
        let (loop_start, loop_end) = if loop_end > loop_start {
            (loop_start, loop_end)
        } else {
            (start, end)
        };
        Some(StateSpan {
            start,
            end,
            loop_start,
            loop_end,
        })
    }

    /// How long a launcher scene plays before it repeats: its longest clip
    /// as played (from the song's start; `None`: no clip in it).
    pub fn scene_length(&self, scene: SceneId) -> Option<MusicalTime> {
        self.launcher
            .slots
            .keys()
            .filter(|k| k.scene == scene)
            .filter_map(|k| self.launcher_clip_as_played(*k))
            .map(|c| {
                let at_zero = crate::Clip {
                    start: MusicalTime::ZERO,
                    ..c
                };
                at_zero.end(&self.timeline, self.sample_rate)
            })
            .max()
            .filter(|l| *l > MusicalTime::ZERO)
    }

    /// The tracks a launcher scene plays clips on.
    pub fn scene_tracks(&self, scene: SceneId) -> Vec<TrackId> {
        self.launcher
            .slots
            .keys()
            .filter(|k| k.scene == scene)
            .map(|k| k.track)
            .collect()
    }

    /// A stinger's length as its clip plays (`None`: its slot is empty).
    pub fn stinger_length(&self, stinger: &Stinger) -> Option<MusicalTime> {
        let c = self.launcher_clip_as_played(stinger.slot)?;
        let at_zero = crate::Clip {
            start: MusicalTime::ZERO,
            ..c
        };
        Some(at_zero.end(&self.timeline, self.sample_rate)).filter(|l| *l > MusicalTime::ZERO)
    }

    /// The name of what a state plays ("Section ‘Verse’", "Scene 3") for
    /// lists.
    pub fn state_source_name(&self, source: StateSource) -> String {
        match source {
            StateSource::Section { section } => {
                self.sections.iter().find(|s| s.id == section).map_or_else(
                    || "a section that is gone".into(),
                    |s| format!("Section ‘{}’", s.name),
                )
            }
            StateSource::Scene { scene } => match self.launcher.scene_index(scene) {
                Some(i) => {
                    let name = &self.launcher.scenes[i].name;
                    if name.is_empty() {
                        format!("Scene {}", i + 1)
                    } else {
                        format!("Scene ‘{name}’")
                    }
                }
                None => "a scene that is gone".into(),
            },
        }
    }
}

/// How playback goes on in a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementKind {
    /// The transport loops the state's loop (a section).
    Loops,
    /// It plays once and ends at its exit.
    Once,
    /// The timeline runs on and the state repeats every loop's length from
    /// its start (a launcher scene: its clips loop by themselves).
    Repeats,
}

/// Where a playing state is, in timeline samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Its start (offset 0).
    pub start: i64,
    /// Its loop (`loop_end > loop_start`); one that plays once ends at
    /// `loop_end` (its exit).
    pub loop_start: i64,
    pub loop_end: i64,
    pub kind: PlacementKind,
}

impl Placement {
    /// A section state's place (`span` from [`Project::state_span`]).
    pub fn of_span(span: &StateSpan, looping: bool, timeline: &Timeline, rate: f64) -> Self {
        let loop_start = timeline.to_samples(span.loop_start, rate);
        Self {
            start: timeline.to_samples(span.start, rate),
            loop_start,
            loop_end: timeline.to_samples(span.loop_end, rate).max(loop_start + 1),
            kind: if looping {
                PlacementKind::Loops
            } else {
                PlacementKind::Once
            },
        }
    }

    /// A launcher scene that started at `start` and repeats every
    /// `length` samples.
    pub fn repeating(start: i64, length: i64) -> Self {
        Self {
            start,
            loop_start: start,
            loop_end: start + length.max(1),
            kind: PlacementKind::Repeats,
        }
    }

    fn loop_len(&self) -> i64 {
        (self.loop_end - self.loop_start).max(1)
    }

    /// The stretches of the timeline playback goes through from `pos`, in
    /// order (a looping state's loop again and again), up to `reach`
    /// samples of playback: `(from, to)`, the first from `pos`.
    fn passes(&self, pos: i64, reach: i64) -> Vec<(i64, i64)> {
        let mut out = Vec::new();
        match self.kind {
            PlacementKind::Loops => {
                let first_end = self.loop_end.max(pos);
                out.push((pos, first_end));
                let mut played = first_end - pos;
                while played < reach && out.len() < 4096 {
                    out.push((self.loop_start, self.loop_end));
                    played += self.loop_len();
                }
            }
            PlacementKind::Once => out.push((pos, self.loop_end.max(pos))),
            PlacementKind::Repeats => out.push((pos, pos + reach)),
        }
        out
    }

    /// Where a position in the state is, from its loop's start (a
    /// repeating state's counts its repeats away).
    pub fn into_loop(&self, at: i64) -> i64 {
        (at - self.loop_start).rem_euclid(self.loop_len())
    }
}

/// What a sync point is read from besides the timeline: the project's
/// markers and the state's named cues (both in timeline samples; a cue as
/// its offset from the state's start).
#[derive(Clone, Debug, Default)]
pub struct SyncContext<'a> {
    pub markers: &'a [i64],
    pub cues: &'a [(String, i64)],
}

/// Where a transition on `sync` leaves a state playing at `place` from
/// timeline sample `pos`, at least `min_wait` samples of playback away
/// (the timeline sample the jump happens on; `None`: no such point within
/// reach — no marker or cue in the state, a state that plays once ends
/// first). Beats, bars and grids follow `timeline`'s tempo and metre at
/// `rate`; a looping state's points come round again after its loop's end.
pub fn sync_point(
    sync: &SyncPoint,
    place: &Placement,
    timeline: &Timeline,
    rate: f64,
    cx: &SyncContext<'_>,
    pos: i64,
    min_wait: i64,
) -> Option<i64> {
    let reach = (REACH_SECONDS * rate) as i64;
    let min_wait = min_wait.max(0);
    let passes = place.passes(pos, reach.max(min_wait + 1));
    let mut waited = 0i64;
    for (i, &(a, b)) in passes.iter().enumerate() {
        // The first stretch takes points from where playback is; the
        // others begin where the last ended (the loop's end is its start).
        let lo = if i == 0 { a } else { a + 1 };
        let first_ok = a + (min_wait - waited).max(0);
        let from = lo.max(first_ok);
        if from <= b {
            let found = match sync {
                SyncPoint::Immediate => Some(from),
                SyncPoint::Beat => next_grid(timeline, rate, from, b, Grid::Beat),
                SyncPoint::Bar => next_grid(timeline, rate, from, b, Grid::Bars(1, 0)),
                SyncPoint::Grid { bars } => {
                    let origin = timeline
                        .meter
                        .bar_at(timeline.to_musical(place.start, rate));
                    next_grid(
                        timeline,
                        rate,
                        from,
                        b,
                        Grid::Bars(i32::from((*bars).max(1)), origin),
                    )
                }
                SyncPoint::Marker => cx
                    .markers
                    .iter()
                    .copied()
                    .filter(|m| *m >= from && *m <= b)
                    .min(),
                SyncPoint::Cue { name } => {
                    let offsets = cx
                        .cues
                        .iter()
                        .filter(|(n, _)| name.is_empty() || n.eq_ignore_ascii_case(name))
                        .map(|(_, o)| *o);
                    offsets.filter_map(|o| place.next_offset(o, from, b)).min()
                }
                SyncPoint::Exit => match place.kind {
                    PlacementKind::Loops | PlacementKind::Once => {
                        (b == place.loop_end && b >= from).then_some(b)
                    }
                    PlacementKind::Repeats => {
                        place.next_offset(place.loop_end - place.start, from, b)
                    }
                },
            };
            if found.is_some() {
                return found;
            }
        }
        waited += b - a;
    }
    None
}

impl Placement {
    /// The first timeline sample in `[from, to]` at offset `offset` from
    /// the state's start (a repeating state's every repeat).
    fn next_offset(&self, offset: i64, from: i64, to: i64) -> Option<i64> {
        match self.kind {
            PlacementKind::Repeats => {
                let len = self.loop_len();
                let first = self.start + offset.rem_euclid(len);
                let k =
                    (from - first).div_euclid(len) + i64::from((from - first).rem_euclid(len) != 0);
                let at = first + k.max(0) * len;
                let at = if at < from { at + len } else { at };
                (at <= to).then_some(at)
            }
            _ => {
                let at = self.start + offset;
                (at >= from && at <= to).then_some(at)
            }
        }
    }
}

enum Grid {
    Beat,
    /// Every `n` bars from bar `origin`.
    Bars(i32, i32),
}

/// The first grid line at or after sample `from` and not after `to`.
fn next_grid(timeline: &Timeline, rate: f64, from: i64, to: i64, grid: Grid) -> Option<i64> {
    // Sample → musical rounds: start a little before and check on samples.
    let here = timeline.to_musical(from, rate);
    let mut bar = timeline.meter.bar_at(here) - 1;
    let end = timeline.meter.bar_at(timeline.to_musical(to, rate)) + 1;
    while bar <= end {
        let start = timeline.meter.bar_start(bar);
        match grid {
            Grid::Beat => {
                let sig = timeline.meter.signature_of_bar(bar);
                for k in 0..i64::from(sig.numerator.max(1)) {
                    let t = start + sig.beat_length() * k;
                    let s = timeline.to_samples(t, rate);
                    if s >= from {
                        return (s <= to).then_some(s);
                    }
                }
            }
            Grid::Bars(n, origin) => {
                if (bar - origin).rem_euclid(n.max(1)) == 0 {
                    let s = timeline.to_samples(start, rate);
                    if s >= from {
                        return (s <= to).then_some(s);
                    }
                }
            }
        }
        bar += 1;
    }
    None
}

#[cfg(test)]
mod tests;
