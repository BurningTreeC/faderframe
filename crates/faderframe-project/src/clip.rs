use crate::{TakeFolder, TrackColor};
use faderframe_core::{AudioSourceId, ClipId, NoteId, TrackId};
use faderframe_timeline::{MusicalTime, Timeline};
use serde::{Deserialize, Serialize};

/// A region on a track lane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub track: TrackId,
    pub name: String,
    /// Overrides the track colour when set.
    #[serde(default)]
    pub color: Option<TrackColor>,
    /// Timeline position of the clip start.
    pub start: MusicalTime,
    #[serde(default)]
    pub muted: bool,
    pub content: ClipContent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ClipContent {
    Audio(AudioClip),
    Midi(MidiClip),
    /// Several recorded takes and the comp that plays (audio tracks).
    Takes(TakeFolder),
}

impl ClipContent {
    /// Audio sources this content references.
    pub fn sources(&self) -> Vec<AudioSourceId> {
        match self {
            ClipContent::Audio(a) => vec![a.source],
            ClipContent::Takes(f) => f.sources().collect(),
            ClipContent::Midi(_) => Vec::new(),
        }
    }

    /// Audio material (plain audio or a take folder)?
    pub fn is_audio(&self) -> bool {
        !matches!(self, ClipContent::Midi(_))
    }
}

/// Audio region referencing part of an audio source.
///
/// Offsets and lengths are frames at the **project** sample rate. Sources at
/// other rates are converted on import; if the engine runs at a different
/// rate than the project, the engine scales positions (and sources are
/// rendered/resampled at the engine rate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioClip {
    pub source: AudioSourceId,
    /// First source frame played.
    pub source_offset: i64,
    /// Played length in frames.
    pub length: i64,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub fades: ClipFades,
    #[serde(default)]
    pub stretch: StretchSettings,
    #[serde(default)]
    pub reversed: bool,
    /// Elastic audio: the clip's time map (`None`: plays 1:1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<crate::Warp>,
    /// Pitch editing: its notes, moved or straightened (`None`: as
    /// recorded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<crate::pitch::PitchEdit>,
    /// Effects rendered onto the clip (the original audio kept).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Box<crate::clip_fx::ClipEffects>>,
    /// Spectral edits (the unedited source kept): the source played is
    /// their processed copy (before the effects, when there are any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spectral: Option<Box<crate::spectral::SpectralEdits>>,
}

impl AudioClip {
    /// The source frame played at clip-relative output frame `out`.
    pub fn source_at(&self, out: f64) -> f64 {
        match &self.warp {
            Some(w) => w.source_of(self.source_offset, self.length, out),
            None => self.source_offset as f64 + out,
        }
    }

    /// Source frames the clip covers.
    pub fn source_span(&self) -> i64 {
        self.warp.as_ref().map_or(self.length, |w| w.source_length)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FadeShape {
    #[default]
    Linear,
    /// sin/cos — constant power, the usual crossfade shape.
    EqualPower,
    SCurve,
    /// Slow start (cubic) — "slow" in the fade menus.
    Exponential,
    /// Fast start, slow end (inverse cubic) — "fast".
    Logarithmic,
}

impl FadeShape {
    pub const ALL: [FadeShape; 5] = [
        FadeShape::Linear,
        FadeShape::EqualPower,
        FadeShape::SCurve,
        FadeShape::Logarithmic,
        FadeShape::Exponential,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FadeShape::Linear => "Linear",
            FadeShape::EqualPower => "Equal Power",
            FadeShape::SCurve => "S-Curve",
            FadeShape::Logarithmic => "Fast",
            FadeShape::Exponential => "Slow",
        }
    }

    /// Fade-in gain at fraction `t` (0..=1); fade-outs use `gain(1 - t)`.
    pub fn gain(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            FadeShape::Linear => t,
            FadeShape::EqualPower => (t * std::f32::consts::FRAC_PI_2).sin(),
            FadeShape::SCurve => t * t * (3.0 - 2.0 * t),
            FadeShape::Exponential => t * t * t,
            FadeShape::Logarithmic => {
                let u = 1.0 - t;
                1.0 - u * u * u
            }
        }
    }

    /// Fade-in gain with a drawn bend (−1…1: 0 keeps the shape, positive
    /// bulges up — faster —, negative sags — slower).
    #[inline]
    pub fn gain_bent(self, t: f32, bend: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let t = if bend > 0.0 {
            1.0 - (1.0 - t).powf(1.0 + 3.0 * bend)
        } else if bend < 0.0 {
            t.powf(1.0 - 3.0 * bend)
        } else {
            t
        };
        self.gain(t)
    }
}

/// A drawn fade bend in percent (−100…100) as a factor.
pub fn bend_factor(percent: i16) -> f32 {
    percent.clamp(-100, 100) as f32 / 100.0
}

/// Fade lengths in frames (project rate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipFades {
    pub fade_in: i64,
    pub fade_out: i64,
    #[serde(default)]
    pub fade_in_shape: FadeShape,
    #[serde(default)]
    pub fade_out_shape: FadeShape,
    /// Drawn bend of the fade-in curve, percent (−100…100).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_in_bend: i16,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_out_bend: i16,
}

fn is_zero(v: &i16) -> bool {
    *v == 0
}

/// Time-stretch / pitch settings (only `Off` is implemented so far; the
/// variant exists so the clip model already accommodates stretching).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum StretchSettings {
    #[default]
    Off,
}

/// Key names for labels ("C4" = MIDI 60).
const KEY_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// A MIDI controller whose values a clip can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum MidiController {
    /// Control change 0–127.
    Cc { number: u8 },
    /// 14-bit pitch bend (8192 = centre).
    PitchBend,
    /// Channel pressure (aftertouch).
    ChannelPressure,
    /// Program change 0–127 (a bank comes from CC 0 and 32 before it).
    Program,
    /// Polyphonic key pressure on one key.
    PolyPressure { key: u8 },
}

impl MidiController {
    pub const MOD_WHEEL: MidiController = MidiController::Cc { number: 1 };
    pub const SUSTAIN: MidiController = MidiController::Cc { number: 64 };

    /// Largest value (127, or 16383 for pitch bend).
    pub fn max(self) -> u16 {
        match self {
            MidiController::PitchBend => 16383,
            _ => 127,
        }
    }

    /// The resting value (pitch bend centre, else 0).
    pub fn rest(self) -> u16 {
        match self {
            MidiController::PitchBend => 8192,
            _ => 0,
        }
    }

    /// Switch-like controllers (sustain, sostenuto, soft pedal, …).
    pub fn is_switch(self) -> bool {
        matches!(self, MidiController::Cc { number: 64..=69 })
    }

    pub fn label(self) -> String {
        match self {
            MidiController::Cc { number } => match number {
                1 => "Mod Wheel".into(),
                2 => "Breath".into(),
                4 => "Foot".into(),
                7 => "Volume".into(),
                10 => "Pan".into(),
                11 => "Expression".into(),
                64 => "Sustain".into(),
                66 => "Sostenuto".into(),
                67 => "Soft Pedal".into(),
                71 => "Resonance".into(),
                74 => "Brightness".into(),
                n => format!("CC {n}"),
            },
            MidiController::PitchBend => "Pitch Bend".into(),
            MidiController::ChannelPressure => "Aftertouch".into(),
            MidiController::Program => "Program".into(),
            MidiController::PolyPressure { key } => format!(
                "Poly Pressure {}{}",
                KEY_NAMES[usize::from(key % 12)],
                i32::from(key) / 12 - 1
            ),
        }
    }

    pub fn event(self, channel: u8, value: u16) -> faderframe_midi::MidiEvent {
        use faderframe_midi::MidiEvent;
        match self {
            MidiController::Cc { number } => MidiEvent::ControlChange {
                channel,
                controller: number,
                value: value.min(127) as u8,
            },
            MidiController::PitchBend => MidiEvent::PitchBend {
                channel,
                value: value.min(16383),
            },
            MidiController::ChannelPressure => MidiEvent::ChannelPressure {
                channel,
                pressure: value.min(127) as u8,
            },
            MidiController::Program => MidiEvent::ProgramChange {
                channel,
                program: value.min(127) as u8,
            },
            MidiController::PolyPressure { key } => MidiEvent::PolyPressure {
                channel,
                key: key.min(127),
                pressure: value.min(127) as u8,
            },
        }
    }

    /// Controller, channel and value of an event.
    pub fn of_event(ev: faderframe_midi::MidiEvent) -> Option<(MidiController, u8, u16)> {
        use faderframe_midi::MidiEvent;
        match ev {
            MidiEvent::ControlChange {
                channel,
                controller,
                value,
            } => Some((
                MidiController::Cc { number: controller },
                channel,
                u16::from(value),
            )),
            MidiEvent::PitchBend { channel, value } => {
                Some((MidiController::PitchBend, channel, value))
            }
            MidiEvent::ChannelPressure { channel, pressure } => Some((
                MidiController::ChannelPressure,
                channel,
                u16::from(pressure),
            )),
            MidiEvent::ProgramChange { channel, program } => {
                Some((MidiController::Program, channel, u16::from(program)))
            }
            MidiEvent::PolyPressure {
                channel,
                key,
                pressure,
            } => Some((
                MidiController::PolyPressure { key },
                channel,
                u16::from(pressure),
            )),
            _ => None,
        }
    }
}

/// One controller value; it holds until the next point (MIDI is stepwise).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerPoint {
    /// Relative to the clip start.
    pub time: MusicalTime,
    pub value: u16,
}

/// The values of one controller on one channel, sorted by time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerLane {
    pub controller: MidiController,
    #[serde(default)]
    pub channel: u8,
    #[serde(default)]
    pub points: Vec<ControllerPoint>,
}

impl ControllerLane {
    pub fn new(controller: MidiController, channel: u8) -> Self {
        Self {
            controller,
            channel,
            points: Vec::new(),
        }
    }

    /// The value in effect at `t` (the last point at or before it).
    pub fn value_at(&self, t: MusicalTime) -> Option<u16> {
        let i = self.points.partition_point(|p| p.time <= t);
        i.checked_sub(1).map(|i| self.points[i].value)
    }

    /// Replace the points in `a..b` with `points` (kept sorted, values
    /// clamped).
    pub fn replace_range(&mut self, a: MusicalTime, b: MusicalTime, points: &[ControllerPoint]) {
        let max = self.controller.max();
        self.points.retain(|p| p.time < a || p.time >= b);
        self.points.extend(points.iter().map(|p| ControllerPoint {
            time: p.time.max(MusicalTime::ZERO),
            value: p.value.min(max),
        }));
        self.normalize();
    }

    /// Sort, drop duplicates at the same time (the last wins) and repeated
    /// values.
    pub fn normalize(&mut self) {
        self.points.sort_by_key(|p| p.time);
        let mut out: Vec<ControllerPoint> = Vec::with_capacity(self.points.len());
        for p in self.points.drain(..) {
            match out.last_mut() {
                Some(last) if last.time == p.time => *last = p,
                Some(last) if last.value == p.value => {}
                _ => out.push(p),
            }
        }
        self.points = out;
    }
}

/// MIDI region; notes and controller values are relative to the clip start.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MidiClip {
    pub length: MusicalTime,
    #[serde(default)]
    pub notes: Vec<MidiNote>,
    /// Controller values (mod wheel, pitch bend, sustain, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controllers: Vec<ControllerLane>,
    /// Per-note expression (MPE), by note id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expressions: Vec<crate::NoteExpression>,
    /// System exclusive messages (sent to the track's external MIDI
    /// device), sorted by time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sysex: Vec<SysexEvent>,
}

impl MidiClip {
    /// The clip played `factor` times as long (time-stretched MIDI: note
    /// positions and lengths, controllers, expression and SysEx scale).
    pub fn scaled(&self, factor: f64) -> MidiClip {
        let f = factor.max(1e-3);
        let t = |m: MusicalTime| MusicalTime((m.ticks() as f64 * f).round() as i64);
        let mut out = self.clone();
        out.length = t(self.length);
        for n in &mut out.notes {
            n.start = t(n.start);
            n.length = t(n.length).max(MusicalTime(1));
        }
        for l in &mut out.controllers {
            for p in &mut l.points {
                p.time = t(p.time);
            }
        }
        for e in &mut out.expressions {
            for p in e
                .pitch
                .iter_mut()
                .chain(&mut e.pressure)
                .chain(&mut e.timbre)
            {
                p.time = t(p.time);
            }
        }
        for e in &mut out.sysex {
            e.time = t(e.time);
        }
        out
    }
}

/// A SysEx message in a clip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SysexEvent {
    /// From the clip start.
    pub time: MusicalTime,
    /// The whole message, `F0 … F7`.
    pub data: Vec<u8>,
}

impl SysexEvent {
    /// Split a byte stream (e.g. a `.syx` file) into complete messages.
    pub fn split_messages(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut cur: Option<Vec<u8>> = None;
        for &b in bytes {
            match b {
                0xF0 => cur = Some(vec![b]),
                0xF7 => {
                    if let Some(mut m) = cur.take() {
                        m.push(b);
                        out.push(m);
                    }
                }
                b if b < 0x80 => {
                    if let Some(m) = &mut cur {
                        m.push(b);
                    }
                }
                // Other status bytes abort a message.
                _ => cur = None,
            }
        }
        out
    }

    /// "F0 41 10 … F7 (11 bytes)", shortened for display.
    pub fn describe(&self) -> String {
        let hex: Vec<String> = self
            .data
            .iter()
            .take(8)
            .map(|b| format!("{b:02X}"))
            .collect();
        let more = if self.data.len() > 8 { " …" } else { "" };
        format!("{}{more} ({} bytes)", hex.join(" "), self.data.len())
    }
}

impl MidiClip {
    pub fn expression(&self, note: NoteId) -> Option<&crate::NoteExpression> {
        self.expressions.iter().find(|e| e.note == note)
    }

    /// A note's expression, created when missing.
    pub fn expression_mut(&mut self, note: NoteId) -> &mut crate::NoteExpression {
        let i = match self.expressions.iter().position(|e| e.note == note) {
            Some(i) => i,
            None => {
                self.expressions.push(crate::NoteExpression::new(note));
                self.expressions.len() - 1
            }
        };
        &mut self.expressions[i]
    }

    /// Drop expressions of notes that are gone and empty ones.
    pub fn prune_expressions(&mut self) {
        let notes = &self.notes;
        self.expressions
            .retain(|e| !e.is_empty() && notes.iter().any(|n| n.id == e.note));
        self.expressions.sort_by_key(|e| e.note);
    }

    pub fn lane(&self, controller: MidiController, channel: u8) -> Option<&ControllerLane> {
        self.controllers
            .iter()
            .find(|l| l.controller == controller && l.channel == channel)
    }

    /// The lane, created when missing.
    pub fn lane_mut(&mut self, controller: MidiController, channel: u8) -> &mut ControllerLane {
        let i = match self
            .controllers
            .iter()
            .position(|l| l.controller == controller && l.channel == channel)
        {
            Some(i) => i,
            None => {
                self.controllers
                    .push(ControllerLane::new(controller, channel));
                self.controllers.sort_by_key(|l| (l.controller, l.channel));
                self.controllers
                    .iter()
                    .position(|l| l.controller == controller && l.channel == channel)
                    .unwrap_or(0)
            }
        };
        &mut self.controllers[i]
    }

    pub fn note(&self, id: NoteId) -> Option<&MidiNote> {
        self.notes.iter().find(|n| n.id == id)
    }

    pub fn note_mut(&mut self, id: NoteId) -> Option<&mut MidiNote> {
        self.notes.iter_mut().find(|n| n.id == id)
    }

    /// Keep notes sorted by start then key (stable editing order).
    pub fn sort_notes(&mut self) {
        self.notes.sort_by_key(|n| (n.start, n.key, n.id));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiNote {
    pub id: NoteId,
    /// Relative to the clip start.
    pub start: MusicalTime,
    pub length: MusicalTime,
    pub key: u8,
    pub velocity: u8,
    #[serde(default)]
    pub channel: u8,
    /// Kept but silent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
    /// Note-off (release) velocity; `None` sends 0, as a note-on of
    /// velocity 0 ends a note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<u8>,
}

impl MidiNote {
    pub fn end(&self) -> MusicalTime {
        self.start + self.length
    }
}

impl Clip {
    /// Musical end position. Audio clips have a fixed length in samples, so
    /// their musical length depends on the tempo map.
    pub fn end(&self, timeline: &Timeline, project_rate: u32) -> MusicalTime {
        match &self.content {
            ClipContent::Audio(a) => {
                timeline.end_of_sample_span(self.start, a.length, project_rate as f64)
            }
            ClipContent::Takes(f) => {
                timeline.end_of_sample_span(self.start, f.length, project_rate as f64)
            }
            ClipContent::Midi(m) => self.start + m.length,
        }
    }

    /// Plays audio (a plain audio clip or a take folder).
    pub fn is_audio(&self) -> bool {
        self.content.is_audio()
    }

    pub fn as_midi(&self) -> Option<&MidiClip> {
        match &self.content {
            ClipContent::Midi(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_midi_mut(&mut self) -> Option<&mut MidiClip> {
        match &mut self.content {
            ClipContent::Midi(m) => Some(m),
            _ => None,
        }
    }

    /// A plain audio clip (not a take folder).
    pub fn as_audio(&self) -> Option<&AudioClip> {
        match &self.content {
            ClipContent::Audio(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_takes(&self) -> Option<&TakeFolder> {
        match &self.content {
            ClipContent::Takes(f) => Some(f),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_shapes_hit_endpoints() {
        for s in [
            FadeShape::Linear,
            FadeShape::EqualPower,
            FadeShape::SCurve,
            FadeShape::Exponential,
        ] {
            assert!(s.gain(0.0).abs() < 1e-6);
            assert!((s.gain(1.0) - 1.0).abs() < 1e-6);
        }
        assert!((FadeShape::EqualPower.gain(0.5) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }

    #[test]
    fn audio_clip_end_depends_on_tempo() {
        let mut timeline = Timeline::default(); // 120 BPM
        let clip = Clip {
            id: ClipId(1),
            track: TrackId(1),
            name: "a".into(),
            color: None,
            start: MusicalTime::from_quarters_i(4),
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: AudioSourceId(1),
                source_offset: 0,
                length: 48_000, // one second
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
            }),
        };
        assert_eq!(clip.end(&timeline, 48_000), MusicalTime::from_quarters_i(6));
        timeline.tempo.set_initial_bpm(60.0);
        assert_eq!(clip.end(&timeline, 48_000), MusicalTime::from_quarters_i(5));
    }
}
