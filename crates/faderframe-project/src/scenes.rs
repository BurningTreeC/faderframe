//! Mix scenes: snapshots of the console, stored in the project and
//! recalled, compared and crossfaded (the scene automation of a digital
//! desk, a DAW's mixer snapshots).
//!
//! A scene ([`MixScene`]) holds, per track by id ([`TrackMix`]), what the
//! console's controls were when it was stored: the fader, the pan (mono
//! pan or balance, the pan mode, dual pan, the surround panner), mute,
//! solo, polarity, the track delay, every send's level, pan and switch,
//! every cue send, each device's bypass — and, when the scene keeps
//! them, the devices' parameter values and states — and the console's
//! drive. Which parts a recall sets is the scene's [`SceneScope`]; tracks
//! in its `safe` set are never touched by it. Tracks added after the
//! scene was stored are not in it and stay as they are; tracks, sends,
//! cue mixes and devices gone since are passed over.
//!
//! Everything here is pure: [`SceneMix::capture`] reads the console (the
//! session hands in the devices' values, which only the running plugins
//! know), [`commands_to`] turns a scene into the existing commands that
//! set the project to it (only what differs, so a recall is one undo step
//! of ordinary edits), and [`blend`] mixes two scenes at a position — the
//! crossfade's way from the console as it was to the scene, and the morph
//! between two scenes. Continuous values glide (faders and send levels in
//! fader travel, as a motorised fader moves; pans, positions and the
//! drive linearly; devices' continuous parameters as the session says),
//! switched ones (mute, solo, polarity, delay, pan mode, send and cue
//! switches, bypass, devices' stepped parameters and states) change at
//! the fade's [`SwitchPoint`]. At the ends a blend is exactly one scene.

use crate::cue::CueSend;
use crate::{Command, PluginSlot, Project, SavedParameter, Track};
use faderframe_core::{
    CueId, DualPan, FaderLaw, MixSceneId, PanMode, ParameterId, PluginInstanceId, SendId,
    SurroundPan, SurroundParam, TrackId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Longest crossfade (ten minutes, or 256 beats).
pub const MAX_FADE_MS: f64 = 600_000.0;
pub const MAX_FADE_BEATS: f64 = 256.0;

/// Where in a crossfade the switched values (mute, bypass, pan mode …)
/// change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchPoint {
    Start,
    #[default]
    Middle,
    End,
}

impl SwitchPoint {
    pub const ALL: [SwitchPoint; 3] = [SwitchPoint::Start, SwitchPoint::Middle, SwitchPoint::End];

    pub fn label(self) -> &'static str {
        match self {
            SwitchPoint::Start => "Switch at the Start",
            SwitchPoint::Middle => "Switch Halfway",
            SwitchPoint::End => "Switch at the End",
        }
    }

    /// Has a fade at `t` (0…1) passed the point (the switched values are
    /// the destination's)?
    pub fn passed(self, t: f64) -> bool {
        match self {
            SwitchPoint::Start => t > 0.0,
            SwitchPoint::Middle => t >= 0.5,
            SwitchPoint::End => t >= 1.0,
        }
    }
}

/// How long a recall takes: at once, or a crossfade in time or in beats
/// (at the tempo where the playhead is).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum FadeLength {
    #[default]
    Instant,
    Millis {
        ms: f64,
    },
    Beats {
        beats: f64,
    },
}

impl FadeLength {
    /// In range (NaN, zero and less: instant).
    pub fn clamped(self) -> Self {
        match self {
            FadeLength::Millis { ms } if ms > 0.0 => FadeLength::Millis {
                ms: ms.min(MAX_FADE_MS),
            },
            FadeLength::Beats { beats } if beats > 0.0 => FadeLength::Beats {
                beats: beats.min(MAX_FADE_BEATS),
            },
            _ => FadeLength::Instant,
        }
    }

    pub fn is_instant(self) -> bool {
        self.clamped() == FadeLength::Instant
    }

    /// Seconds at `bpm` (quarter notes a minute).
    pub fn seconds(self, bpm: f64) -> f64 {
        match self.clamped() {
            FadeLength::Instant => 0.0,
            FadeLength::Millis { ms } => ms / 1000.0,
            FadeLength::Beats { beats } => beats * 60.0 / bpm.max(1.0),
        }
    }

    /// "Instant", "350 ms", "2.5 s", "4 beats".
    pub fn label(self) -> String {
        match self.clamped() {
            FadeLength::Instant => "Instant".into(),
            FadeLength::Millis { ms } if ms < 1000.0 => format!("{ms:.0} ms"),
            FadeLength::Millis { ms } => {
                let s = ms / 1000.0;
                if (s - s.round()).abs() < 1e-9 {
                    format!("{s:.0} s")
                } else {
                    format!("{s:.1} s")
                }
            }
            FadeLength::Beats { beats } => {
                let n = if (beats - beats.round()).abs() < 1e-9 {
                    format!("{beats:.0}")
                } else {
                    format!("{beats:.2}")
                };
                if (beats - 1.0).abs() < 1e-9 {
                    "1 beat".into()
                } else {
                    format!("{n} beats")
                }
            }
        }
    }

    /// From what a user types: "0" or "instant", "350 ms", "2 s", "2.5"
    /// (seconds), "4 beats", "4 b".
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim().to_lowercase();
        if t.is_empty() || t == "instant" || t == "off" || t == "none" {
            return Some(FadeLength::Instant);
        }
        let split = t
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
            .unwrap_or(t.len());
        let (num, unit) = t.split_at(split);
        let v: f64 = num.replace(',', ".").parse().ok()?;
        if !v.is_finite() || v < 0.0 {
            return None;
        }
        let length = match unit.trim() {
            "ms" => FadeLength::Millis { ms: v },
            "" | "s" | "sec" | "secs" | "second" | "seconds" => {
                FadeLength::Millis { ms: v * 1000.0 }
            }
            "b" | "beat" | "beats" => FadeLength::Beats { beats: v },
            _ => return None,
        };
        Some(length.clamped())
    }
}

/// How a recall goes: its length and where switched values change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneFade {
    #[serde(default)]
    pub length: FadeLength,
    #[serde(default)]
    pub switch: SwitchPoint,
}

impl SceneFade {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// "2 s, switch halfway" (instant: "Instant").
    pub fn label(&self) -> String {
        if self.length.is_instant() {
            return self.length.label();
        }
        let switch = match self.switch {
            SwitchPoint::Start => "switch at the start",
            SwitchPoint::Middle => "switch halfway",
            SwitchPoint::End => "switch at the end",
        };
        format!("{}, {switch}", self.length.label())
    }
}

/// A part of the console a scene's recall can set or leave.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopePart {
    Faders,
    /// Pan and balance, the pan mode, dual pan, the surround panner.
    Pans,
    Mutes,
    Solos,
    Polarity,
    Delays,
    /// Sends' levels, pans and switches.
    Sends,
    CueSends,
    Bypass,
    /// Devices' parameters and states (scenes that keep them).
    Plugins,
    /// The console's drive.
    Console,
}

impl ScopePart {
    pub const ALL: [ScopePart; 11] = [
        ScopePart::Faders,
        ScopePart::Pans,
        ScopePart::Mutes,
        ScopePart::Solos,
        ScopePart::Polarity,
        ScopePart::Delays,
        ScopePart::Sends,
        ScopePart::CueSends,
        ScopePart::Bypass,
        ScopePart::Plugins,
        ScopePart::Console,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ScopePart::Faders => "Faders",
            ScopePart::Pans => "Pans",
            ScopePart::Mutes => "Mutes",
            ScopePart::Solos => "Solos",
            ScopePart::Polarity => "Polarity",
            ScopePart::Delays => "Track Delays",
            ScopePart::Sends => "Sends",
            ScopePart::CueSends => "Cue Sends",
            ScopePart::Bypass => "Bypass",
            ScopePart::Plugins => "Plugins",
            ScopePart::Console => "Console Drive",
        }
    }

    /// The short name a chip shows.
    pub fn short(self) -> &'static str {
        match self {
            ScopePart::Faders => "Fdr",
            ScopePart::Pans => "Pan",
            ScopePart::Mutes => "Mute",
            ScopePart::Solos => "Solo",
            ScopePart::Polarity => "Ø",
            ScopePart::Delays => "Dly",
            ScopePart::Sends => "Snd",
            ScopePart::CueSends => "Cue",
            ScopePart::Bypass => "Byp",
            ScopePart::Plugins => "Plg",
            ScopePart::Console => "Drv",
        }
    }
}

/// Which parts of a scene a recall sets (all by default).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneScope {
    pub faders: bool,
    pub pans: bool,
    pub mutes: bool,
    pub solos: bool,
    pub polarity: bool,
    pub delays: bool,
    pub sends: bool,
    pub cue_sends: bool,
    pub bypass: bool,
    pub plugins: bool,
    pub console: bool,
}

impl Default for SceneScope {
    fn default() -> Self {
        Self {
            faders: true,
            pans: true,
            mutes: true,
            solos: true,
            polarity: true,
            delays: true,
            sends: true,
            cue_sends: true,
            bypass: true,
            plugins: true,
            console: true,
        }
    }
}

impl SceneScope {
    fn field(&mut self, part: ScopePart) -> &mut bool {
        match part {
            ScopePart::Faders => &mut self.faders,
            ScopePart::Pans => &mut self.pans,
            ScopePart::Mutes => &mut self.mutes,
            ScopePart::Solos => &mut self.solos,
            ScopePart::Polarity => &mut self.polarity,
            ScopePart::Delays => &mut self.delays,
            ScopePart::Sends => &mut self.sends,
            ScopePart::CueSends => &mut self.cue_sends,
            ScopePart::Bypass => &mut self.bypass,
            ScopePart::Plugins => &mut self.plugins,
            ScopePart::Console => &mut self.console,
        }
    }

    pub fn has(&self, part: ScopePart) -> bool {
        let mut s = *self;
        *s.field(part)
    }

    pub fn with(mut self, part: ScopePart, on: bool) -> Self {
        *self.field(part) = on;
        self
    }

    /// The parts both have (a morph sets what both scenes recall).
    pub fn and(self, other: SceneScope) -> Self {
        ScopePart::ALL
            .iter()
            .fold(self, |s, p| s.with(*p, s.has(*p) && other.has(*p)))
    }

    /// Nothing at all.
    pub fn none() -> Self {
        ScopePart::ALL
            .iter()
            .fold(Self::default(), |s, p| s.with(*p, false))
    }
}

/// A send's settings in a scene (the send by its id).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SendMix {
    pub send: SendId,
    pub level_db: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<f32>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// A device's parameter values and state, as a scene keeps them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceValues {
    /// Plain values of its automatable parameters.
    #[serde(default)]
    pub parameters: Vec<SavedParameter>,
    /// Its opaque state (formats that have one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// A device's settings in a scene (its slot by id).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceMix {
    pub plugin: PluginInstanceId,
    #[serde(default)]
    pub bypass: bool,
    /// Parameters and state, when the scene keeps the devices'.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<DeviceValues>,
}

/// One track's console settings in a scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackMix {
    pub volume_db: f32,
    pub pan: f32,
    #[serde(default, skip_serializing_if = "PanMode::is_default")]
    pub pan_mode: PanMode,
    #[serde(default, skip_serializing_if = "DualPan::is_default")]
    pub dual_pan: DualPan,
    #[serde(default, skip_serializing_if = "SurroundPan::is_default")]
    pub surround: SurroundPan,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub phase_invert: bool,
    #[serde(default)]
    pub delay_ms: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<SendMix>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cue_sends: Vec<CueSend>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<DeviceMix>,
}

impl TrackMix {
    /// `track`'s settings as they are; `values` gives a device's
    /// parameters and state when the scene keeps them (`None`: not kept).
    pub fn of(
        track: &Track,
        values: &mut dyn FnMut(&Track, &PluginSlot) -> Option<DeviceValues>,
    ) -> Self {
        Self {
            volume_db: track.volume_db,
            pan: track.pan,
            pan_mode: track.pan_mode,
            dual_pan: track.dual_pan,
            surround: track.surround,
            mute: track.mute,
            solo: track.solo,
            phase_invert: track.phase_invert,
            delay_ms: track.delay_ms,
            sends: track
                .sends
                .iter()
                .map(|s| SendMix {
                    send: s.id,
                    level_db: s.level_db,
                    pan: s.pan,
                    enabled: s.enabled,
                })
                .collect(),
            cue_sends: track.cue_sends.clone(),
            devices: track
                .slots()
                .into_iter()
                .map(|s| DeviceMix {
                    plugin: s.id,
                    bypass: s.bypass,
                    values: values(track, s),
                })
                .collect(),
        }
    }

    fn send(&self, id: SendId) -> Option<&SendMix> {
        self.sends.iter().find(|s| s.send == id)
    }

    fn cue_send(&self, id: CueId) -> Option<&CueSend> {
        self.cue_sends.iter().find(|s| s.cue == id)
    }

    fn device(&self, id: PluginInstanceId) -> Option<&DeviceMix> {
        self.devices.iter().find(|d| d.plugin == id)
    }
}

/// What a scene holds: the tracks' console settings and the console's
/// drive.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneMix {
    #[serde(default)]
    pub tracks: BTreeMap<TrackId, TrackMix>,
    /// The console's drive (dB), when the project ran through one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_drive: Option<f64>,
}

impl SceneMix {
    /// The console as it is (every track; see [`TrackMix::of`] for
    /// `values`).
    pub fn capture(
        project: &Project,
        values: &mut dyn FnMut(&Track, &PluginSlot) -> Option<DeviceValues>,
    ) -> Self {
        Self {
            tracks: project
                .tracks
                .iter()
                .map(|t| (t.id, TrackMix::of(t, values)))
                .collect(),
            console_drive: project.console.as_ref().map(|c| c.drive_db),
        }
    }

    /// Does it keep any device's parameters?
    pub fn keeps_plugins(&self) -> bool {
        self.tracks
            .values()
            .any(|t| t.devices.iter().any(|d| d.values.is_some()))
    }
}

/// A stored console snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MixScene {
    pub id: MixSceneId,
    pub name: String,
    #[serde(default)]
    pub mix: SceneMix,
    #[serde(default)]
    pub scope: SceneScope,
    /// Tracks its recalls never touch.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub safe: BTreeSet<TrackId>,
    /// Its own recall (`None`: the project's, [`MixScenes::fade`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade: Option<SceneFade>,
}

/// The project's scenes, the recall they share and the two compared and
/// morphed between.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MixScenes {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenes: Vec<MixScene>,
    /// How recalls go unless a scene has its own.
    #[serde(default, skip_serializing_if = "SceneFade::is_default")]
    pub fade: SceneFade,
    /// The scenes A and B (compare, morph).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<MixSceneId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<MixSceneId>,
}

impl MixScenes {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn get(&self, id: MixSceneId) -> Option<&MixScene> {
        self.scenes.iter().find(|s| s.id == id)
    }

    pub fn index_of(&self, id: MixSceneId) -> Option<usize> {
        self.scenes.iter().position(|s| s.id == id)
    }

    /// The recall `scene` makes (its own, else the project's).
    pub fn fade_of(&self, scene: &MixScene) -> SceneFade {
        scene.fade.unwrap_or(self.fade)
    }

    /// A/B that name no scene any more are cleared.
    pub fn tidy(&mut self) {
        let ids: BTreeSet<MixSceneId> = self.scenes.iter().map(|s| s.id).collect();
        self.a = self.a.filter(|a| ids.contains(a));
        self.b = self.b.filter(|b| ids.contains(b));
        self.fade.length = self.fade.length.clamped();
        for s in &mut self.scenes {
            if let Some(f) = &mut s.fade {
                f.length = f.length.clamped();
            }
        }
    }
}

/// How a device's parameter moves in a blend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamGlide {
    /// Stepped (or unknown): changes at the switch point.
    Switch,
    Linear,
    /// Geometrically (frequencies, times): positive values only.
    Log,
}

/// `b` once the switch point is passed, else `a`.
fn pick<T>(switched: bool, a: T, b: T) -> T {
    if switched { b } else { a }
}

fn lerp(a: f32, b: f32, t: f64) -> f32 {
    (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as f32
}

/// A level between `a` and `b` (dB) as a fader moves: in fader travel.
pub fn blend_db(law: &FaderLaw, a: f32, b: f32, t: f64) -> f32 {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let pos = lerp(law.db_to_position(a), law.db_to_position(b), t);
    law.position_to_db(pos)
}

fn blend_param(glide: ParamGlide, a: f64, b: f64, t: f64, switched: bool) -> f64 {
    match glide {
        ParamGlide::Linear => a + (b - a) * t,
        ParamGlide::Log if a > 0.0 && b > 0.0 => (a.ln() + (b.ln() - a.ln()) * t).exp(),
        ParamGlide::Log => a + (b - a) * t,
        ParamGlide::Switch => {
            if switched {
                b
            } else {
                a
            }
        }
    }
}

/// Items of two lists matched by key: both in a blend, or the side the
/// switch point stands on.
fn blend_lists<T: Clone, K: PartialEq>(
    a: &[T],
    b: &[T],
    key: impl Fn(&T) -> K,
    switched: bool,
    both: impl Fn(&T, &T) -> T,
) -> Vec<T> {
    let mut out = Vec::new();
    for x in a {
        match b.iter().find(|y| key(y) == key(x)) {
            Some(y) => out.push(both(x, y)),
            None if !switched => out.push(x.clone()),
            None => {}
        }
    }
    if switched {
        out.extend(
            b.iter()
                .filter(|y| !a.iter().any(|x| key(x) == key(y)))
                .cloned(),
        );
    }
    out
}

fn blend_device(
    a: &DeviceMix,
    b: &DeviceMix,
    t: f64,
    switched: bool,
    glide: &dyn Fn(PluginInstanceId, ParameterId) -> ParamGlide,
) -> DeviceMix {
    let values = match (&a.values, &b.values) {
        (Some(x), Some(y)) => Some(DeviceValues {
            parameters: blend_lists(
                &x.parameters,
                &y.parameters,
                |p| p.id,
                switched,
                |p, q| SavedParameter {
                    id: p.id,
                    value: blend_param(glide(a.plugin, p.id), p.value, q.value, t, switched),
                },
            ),
            state: if switched {
                y.state.clone()
            } else {
                x.state.clone()
            },
        }),
        (x, y) => {
            if switched {
                y.clone()
            } else {
                x.clone()
            }
        }
    };
    DeviceMix {
        plugin: a.plugin,
        bypass: if switched { b.bypass } else { a.bypass },
        values,
    }
}

fn blend_track(
    law: &FaderLaw,
    a: &TrackMix,
    b: &TrackMix,
    t: f64,
    switched: bool,
    glide: &dyn Fn(PluginInstanceId, ParameterId) -> ParamGlide,
) -> TrackMix {
    let mut surround = if switched { b.surround } else { a.surround };
    for p in SurroundParam::ALL {
        surround = p.set(surround, lerp(p.get(&a.surround), p.get(&b.surround), t));
    }
    TrackMix {
        volume_db: blend_db(law, a.volume_db, b.volume_db, t),
        pan: lerp(a.pan, b.pan, t),
        pan_mode: pick(switched, a.pan_mode, b.pan_mode),
        dual_pan: DualPan {
            left: lerp(a.dual_pan.left, b.dual_pan.left, t),
            right: lerp(a.dual_pan.right, b.dual_pan.right, t),
        },
        surround,
        mute: pick(switched, a.mute, b.mute),
        solo: pick(switched, a.solo, b.solo),
        phase_invert: pick(switched, a.phase_invert, b.phase_invert),
        delay_ms: pick(switched, a.delay_ms, b.delay_ms),
        sends: blend_lists(
            &a.sends,
            &b.sends,
            |s| s.send,
            switched,
            |x, y| SendMix {
                send: x.send,
                level_db: blend_db(law, x.level_db, y.level_db, t),
                pan: match (x.pan, y.pan) {
                    (Some(p), Some(q)) => Some(lerp(p, q, t)),
                    (p, q) => pick(switched, p, q),
                },
                enabled: pick(switched, x.enabled, y.enabled),
            },
        ),
        cue_sends: blend_lists(
            &a.cue_sends,
            &b.cue_sends,
            |s| s.cue,
            switched,
            |x, y| CueSend {
                cue: x.cue,
                level_db: blend_db(law, x.level_db, y.level_db, t),
                pan: lerp(x.pan, y.pan, t),
                on: pick(switched, x.on, y.on),
            },
        ),
        devices: blend_lists(
            &a.devices,
            &b.devices,
            |d| d.plugin,
            switched,
            |x, y| blend_device(x, y, t, switched, glide),
        ),
    }
}

/// The console at `t` (0…1) of the way from `a` to `b`: continuous values
/// glide, switched ones are `b`'s once `switch` is passed (see the module
/// docs); `glide` says how each device parameter moves. At 0 it is `a`,
/// at 1 exactly `b` (as far as both hold the same tracks: a track only
/// one of them holds is in the blend on its side of the switch point).
pub fn blend(
    a: &SceneMix,
    b: &SceneMix,
    t: f64,
    switch: SwitchPoint,
    glide: &dyn Fn(PluginInstanceId, ParameterId) -> ParamGlide,
) -> SceneMix {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    if t >= 1.0 {
        return b.clone();
    }
    let switched = switch.passed(t);
    let law = FaderLaw::console();
    let mut tracks = BTreeMap::new();
    for (id, x) in &a.tracks {
        match b.tracks.get(id) {
            Some(y) => {
                tracks.insert(*id, blend_track(&law, x, y, t, switched, glide));
            }
            None if !switched => {
                tracks.insert(*id, x.clone());
            }
            None => {}
        }
    }
    if switched {
        for (id, y) in &b.tracks {
            tracks.entry(*id).or_insert_with(|| y.clone());
        }
    }
    let console_drive = match (a.console_drive, b.console_drive) {
        (Some(x), Some(y)) => Some(x + (y - x) * t),
        (x, y) => {
            if switched {
                y
            } else {
                x
            }
        }
    };
    SceneMix {
        tracks,
        console_drive,
    }
}

fn differs(a: f32, b: f32) -> bool {
    (a - b).abs() > 1e-5 || a.is_nan() != b.is_nan()
}

/// A device parameter's value as the project states it (its explicit
/// value; `None`: the plugin's own).
pub fn explicit_value(slot: &PluginSlot, parameter: ParameterId) -> Option<f64> {
    slot.parameters
        .iter()
        .find(|p| p.id == parameter)
        .map(|p| p.value)
}

/// The commands that set `project`'s console to `target` as far as
/// `scope` reaches, leaving the `safe` tracks alone — only what differs,
/// and nothing a command would refuse (a delay on a track that has none,
/// a send switched on into a loop, a cue send from a track that feeds no
/// cue mix, the drive without a console). `now` says where a device
/// parameter is (only the running plugin knows those without an explicit
/// value; `None`: unknown, set). Applied as one batch they are one undo
/// step.
pub fn commands_to(
    project: &Project,
    target: &SceneMix,
    scope: &SceneScope,
    safe: &BTreeSet<TrackId>,
    now: &dyn Fn(&PluginSlot, ParameterId) -> Option<f64>,
) -> Vec<Command> {
    let mut out = Vec::new();
    for (&id, m) in &target.tracks {
        if safe.contains(&id) {
            continue;
        }
        let Some(t) = project.track(id) else {
            continue;
        };
        track_commands(project, t, m, scope, now, &mut out);
    }
    if scope.console
        && let (Some(c), Some(drive)) = (&project.console, target.console_drive)
        && drive.is_finite()
        && (c.drive_db - drive).abs() > 1e-6
    {
        out.push(Command::SetConsoleDrive { drive_db: drive });
    }
    out
}

fn track_commands(
    project: &Project,
    t: &Track,
    m: &TrackMix,
    scope: &SceneScope,
    now: &dyn Fn(&PluginSlot, ParameterId) -> Option<f64>,
    out: &mut Vec<Command>,
) {
    let track = t.id;
    if scope.faders && differs(t.volume_db, m.volume_db) {
        out.push(Command::SetTrackVolume {
            track,
            db: m.volume_db,
        });
    }
    if scope.pans {
        if differs(t.pan, m.pan) {
            out.push(Command::SetTrackPan { track, pan: m.pan });
        }
        if t.pan_mode != m.pan_mode {
            out.push(Command::SetTrackPanMode {
                track,
                mode: m.pan_mode,
            });
        }
        if differs(t.dual_pan.left, m.dual_pan.left) || differs(t.dual_pan.right, m.dual_pan.right)
        {
            out.push(Command::SetTrackDualPan {
                track,
                pan: m.dual_pan,
            });
        }
        if t.surround != m.surround {
            out.push(Command::SetTrackSurround {
                track,
                pan: m.surround,
            });
        }
    }
    if scope.mutes && t.mute != m.mute {
        out.push(Command::SetTrackMute { track, on: m.mute });
    }
    if scope.solos && t.solo != m.solo {
        out.push(Command::SetTrackSolo { track, on: m.solo });
    }
    if scope.polarity && t.phase_invert != m.phase_invert {
        out.push(Command::SetTrackPhaseInvert {
            track,
            on: m.phase_invert,
        });
    }
    if scope.delays
        && (t.delay_ms - m.delay_ms).abs() > 1e-9
        && (t.takes_delay() || m.delay_ms == 0.0)
    {
        out.push(Command::SetTrackDelay {
            track,
            ms: m.delay_ms,
        });
    }
    if scope.sends {
        for s in &t.sends {
            let Some(x) = m.send(s.id) else { continue };
            if differs(s.level_db, x.level_db) {
                out.push(Command::SetSendLevel {
                    track,
                    send: s.id,
                    db: x.level_db,
                });
            }
            let pan_differs = match (s.pan, x.pan) {
                (Some(p), Some(q)) => differs(p, q),
                (p, q) => p.is_some() != q.is_some(),
            };
            if pan_differs {
                out.push(Command::SetSendPan {
                    track,
                    send: s.id,
                    pan: x.pan,
                });
            }
            if s.enabled != x.enabled && (!x.enabled || !project.would_cycle(track, s.target)) {
                out.push(Command::SetSendEnabled {
                    track,
                    send: s.id,
                    enabled: x.enabled,
                });
            }
        }
    }
    if scope.cue_sends && crate::cue::feeds_cues(t.kind) {
        for c in &project.cue_mixes {
            let Some(x) = m.cue_send(c.id) else { continue };
            let now = t.cue_send(c.id);
            let same = now.is_some_and(|s| {
                !differs(s.level_db, x.level_db) && !differs(s.pan, x.pan) && s.on == x.on
            });
            if !same {
                out.push(Command::SetCueSend {
                    track,
                    cue: c.id,
                    send: Some(*x),
                });
            }
        }
    }
    if scope.bypass || scope.plugins {
        for slot in t.slots() {
            let Some(d) = m.device(slot.id) else { continue };
            if scope.bypass && slot.bypass != d.bypass {
                out.push(Command::SetPluginBypass {
                    track,
                    plugin: slot.id,
                    bypass: d.bypass,
                });
            }
            if !scope.plugins {
                continue;
            }
            let Some(v) = &d.values else { continue };
            if v.state.is_some() && v.state != slot.state {
                // A new state: the values it was stored with go with it
                // (explicit values follow a state on load).
                let mut parameters = v.parameters.clone();
                for p in &slot.parameters {
                    if !parameters.iter().any(|q| q.id == p.id) {
                        parameters.push(*p);
                    }
                }
                out.push(Command::SetPluginState {
                    track,
                    plugin: slot.id,
                    state: v.state.clone(),
                    parameters,
                });
                continue;
            }
            for p in &v.parameters {
                let is = explicit_value(slot, p.id).or_else(|| now(slot, p.id));
                if is.is_none_or(|q| (q - p.value).abs() > 1e-9) {
                    out.push(Command::SetPluginParameter {
                        track,
                        plugin: slot.id,
                        parameter: p.id,
                        value: Some(p.value),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
