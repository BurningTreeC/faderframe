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

    /// Every audio source this content needs: the ones it plays
    /// ([`Self::sources`]) and the ones kept to go back to — the audio
    /// under clip effects, the unedited audio of spectral edits and of a
    /// polyphonic pitch edit.
    pub fn referenced_sources(&self) -> Vec<AudioSourceId> {
        fn pitch(p: Option<&crate::pitch::PitchEdit>, out: &mut Vec<AudioSourceId>) {
            if let Some(poly) = p.and_then(|p| p.polyphonic.as_ref()) {
                out.push(poly.original);
            }
        }
        let mut out = Vec::new();
        match self {
            ClipContent::Audio(a) => {
                out.push(a.source);
                pitch(a.pitch.as_ref(), &mut out);
                if let Some(e) = &a.effects {
                    out.push(e.original.source);
                    pitch(e.original.pitch.as_ref(), &mut out);
                }
                if let Some(s) = &a.spectral {
                    out.push(s.original);
                }
            }
            ClipContent::Takes(f) => out.extend(f.sources()),
            ClipContent::Midi(_) => {}
        }
        out.sort();
        out.dedup();
        out
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
    /// Volume and pan envelopes that move with the clip
    /// ([`crate::clip_envelope`]; `None`: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<Box<crate::clip_envelope::ClipEnvelope>>,
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

/// Time-stretch / pitch settings of an audio clip (its time map is
/// [`AudioClip::warp`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum StretchSettings {
    #[default]
    Off,
    /// Played transposed (through the stretcher; its time is kept): a
    /// sample brought into the project's key.
    Pitch { semitones: f32 },
}

impl StretchSettings {
    /// The largest transposition either way (semitones).
    pub const MAX_SEMITONES: f32 = 24.0;

    /// Transposed by `semitones` (0: off; kept within ±
    /// [`Self::MAX_SEMITONES`]).
    pub fn pitch(semitones: f32) -> Self {
        let st = if semitones.is_finite() {
            semitones.clamp(-Self::MAX_SEMITONES, Self::MAX_SEMITONES)
        } else {
            0.0
        };
        if st == 0.0 {
            Self::Off
        } else {
            Self::Pitch { semitones: st }
        }
    }

    /// The transposition (semitones; 0 when off).
    pub fn semitones(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Pitch { semitones } if semitones.is_finite() => {
                semitones.clamp(-Self::MAX_SEMITONES, Self::MAX_SEMITONES)
            }
            Self::Pitch { .. } => 0.0,
        }
    }

    /// The pitch factor the stretcher plays at (1: unchanged).
    pub fn pitch_factor(self) -> f32 {
        2f32.powf(self.semitones() / 12.0)
    }
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

    /// The bits of its MIDI 1.0 value (14 for pitch bend, else 7).
    pub fn bits(self) -> u32 {
        match self {
            MidiController::PitchBend => 14,
            _ => 7,
        }
    }

    /// Whether MIDI 2.0 gives it 32 bits (every controller but programs).
    pub fn has_fine(self) -> bool {
        !matches!(self, MidiController::Program)
    }

    /// The event of a point, with its MIDI 2.0 resolution.
    pub fn event_fine(
        self,
        channel: u8,
        point: &ControllerPoint,
    ) -> (faderframe_midi::MidiEvent, faderframe_midi::Fine) {
        let fine = match point.fine_of(self) {
            Some(v) => faderframe_midi::Fine::value(v),
            None => faderframe_midi::Fine::NONE,
        };
        (self.event(channel, point.value), fine)
    }

    /// Controller, channel, value and 32-bit value (when it came with one
    /// that says more than the MIDI 1.0 value) of an event with its MIDI
    /// 2.0 resolution.
    pub fn of_event_fine(
        ev: faderframe_midi::MidiEvent,
        fine: faderframe_midi::Fine,
    ) -> Option<(MidiController, u8, u16, Option<u32>)> {
        let (c, ch, v) = Self::of_event(ev)?;
        let up = faderframe_midi::ump::scale_up(u32::from(v), c.bits(), 32);
        let fine = if c.has_fine() {
            ev.fine_value(fine).filter(|&f| f != up)
        } else {
            None
        };
        Some((c, ch, v, fine))
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
    /// The MIDI 1.0 value (7 bits, 14 for pitch bend).
    pub value: u16,
    /// MIDI 2.0's 32-bit value (recorded from a MIDI 2.0 device, drawn at
    /// full resolution). It counts while its top bits are `value`: an edit
    /// of `value` alone leaves it behind ([`Self::value32`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fine: Option<u32>,
}

impl ControllerPoint {
    /// A point at MIDI 1.0's resolution.
    pub fn new(time: MusicalTime, value: u16) -> Self {
        Self {
            time,
            value,
            fine: None,
        }
    }

    /// A point of `controller` at its 32-bit value (the MIDI 1.0 value its
    /// top bits; programs have none).
    pub fn at32(time: MusicalTime, controller: MidiController, value: u32) -> Self {
        if !controller.has_fine() {
            return Self::new(
                time,
                (value >> (32 - controller.bits())).min(u32::from(controller.max())) as u16,
            );
        }
        let coarse = (value >> (32 - controller.bits())) as u16;
        let up = faderframe_midi::ump::scale_up(u32::from(coarse), controller.bits(), 32);
        Self {
            time,
            value: coarse,
            fine: (value != up).then_some(value),
        }
    }

    /// The 32-bit value when it agrees with `value`.
    pub fn fine_of(&self, controller: MidiController) -> Option<u32> {
        let v = self.fine?;
        (controller.has_fine() && v >> (32 - controller.bits()) == u32::from(self.value))
            .then_some(v)
    }

    /// The value at MIDI 2.0's 32 bits: the fine one while it agrees,
    /// else the MIDI 1.0 value scaled up the specification's way.
    pub fn value32(&self, controller: MidiController) -> u32 {
        self.fine_of(controller).unwrap_or_else(|| {
            faderframe_midi::ump::scale_up(u32::from(self.value), controller.bits(), 32)
        })
    }

    /// The value in MIDI 1.0 steps (7 bits; pitch bend 14) with MIDI 2.0's
    /// fraction when it has one (64.5, 8192.25).
    pub fn steps(&self, controller: MidiController) -> f64 {
        match self.fine_of(controller) {
            Some(v) => f64::from(v) / f64::from(1u32 << (32 - controller.bits())),
            None => f64::from(self.value),
        }
    }

    /// A point at `steps` (see [`Self::steps`]): whole steps are a MIDI 1.0
    /// value, a fraction gives it MIDI 2.0's 32 bits.
    pub fn of_steps(time: MusicalTime, controller: MidiController, steps: f64) -> Self {
        let max = f64::from(controller.max());
        let steps = steps.clamp(0.0, max + 0.999_999);
        if !controller.has_fine() || steps.fract() == 0.0 {
            return Self::new(time, steps.floor().min(max) as u16);
        }
        let v = (steps * f64::from(1u32 << (32 - controller.bits())))
            .round()
            .min(f64::from(u32::MAX)) as u32;
        Self::at32(time, controller, v)
    }

    /// The value for display: the MIDI 1.0 value, with the fraction MIDI
    /// 2.0's resolution adds ("64.53").
    pub fn steps_text(&self, controller: MidiController) -> String {
        match self.fine_of(controller) {
            Some(_) => steps_text(self.steps(controller)),
            None => self.value.to_string(),
        }
    }

    /// The value as 0…1 of `controller`'s range, at full resolution.
    pub fn unit(&self, controller: MidiController) -> f64 {
        match self.fine_of(controller) {
            Some(v) => f64::from(v) / f64::from(u32::MAX),
            None => f64::from(self.value) / f64::from(controller.max()),
        }
    }
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
        self.point_at(t).map(|p| p.value)
    }

    /// The point in effect at `t`.
    pub fn point_at(&self, t: MusicalTime) -> Option<&ControllerPoint> {
        let i = self.points.partition_point(|p| p.time <= t);
        i.checked_sub(1).map(|i| &self.points[i])
    }

    /// The 32-bit value in effect at `t` (see [`ControllerPoint::value32`]).
    pub fn value32_at(&self, t: MusicalTime) -> Option<u32> {
        self.point_at(t).map(|p| p.value32(self.controller))
    }

    /// Replace the points in `a..b` with `points` (kept sorted, values
    /// clamped).
    pub fn replace_range(&mut self, a: MusicalTime, b: MusicalTime, points: &[ControllerPoint]) {
        let max = self.controller.max();
        self.points.retain(|p| p.time < a || p.time >= b);
        self.points.extend(points.iter().map(|p| ControllerPoint {
            time: p.time.max(MusicalTime::ZERO),
            value: p.value.min(max),
            fine: p.fine,
        }));
        self.normalize();
    }

    /// Sort, drop duplicates at the same time (the last wins) and repeated
    /// values (at full resolution); a 32-bit value that no longer agrees
    /// or says no more than the MIDI 1.0 value goes.
    pub fn normalize(&mut self) {
        let c = self.controller;
        self.points.sort_by_key(|p| p.time);
        let mut out: Vec<ControllerPoint> = Vec::with_capacity(self.points.len());
        for mut p in self.points.drain(..) {
            p.fine = p
                .fine_of(c)
                .filter(|&v| v != faderframe_midi::ump::scale_up(u32::from(p.value), c.bits(), 32));
            match out.last_mut() {
                Some(last) if last.time == p.time => *last = p,
                Some(last) if last.value32(c) == p.value32(c) => {}
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
    /// Articulation directions (the clip's articulation lane; see
    /// [`crate::articulation`]), sorted by time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub articulations: Vec<crate::articulation::ArticulationEvent>,
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
        for e in &mut out.articulations {
            e.time = t(e.time);
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

/// What a note keeps of MIDI 2.0: its velocities at 16 bits and its
/// attribute. Each value counts while it agrees with the note's MIDI 1.0
/// value (its top seven bits are it), so an edit at seven bits leaves it
/// behind (see [`MidiNote::velocity16`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NoteFine {
    /// The note-on's velocity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<u16>,
    /// The note-off's velocity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<u16>,
    /// The note-on's attribute type (1 manufacturer, 2 profile specific;
    /// 0 none — pitch 7.9 is the note's tuning expression instead).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub attribute: u8,
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub attribute_data: u16,
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

impl NoteFine {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A 16-bit velocity in MIDI 1.0 steps: its top seven bits are the whole
/// steps, the other nine the fraction (51456 = 100.5).
pub fn velocity_steps(v: u16) -> f64 {
    f64::from(v) / 512.0
}

/// MIDI 1.0 steps (with a fraction) as a 16-bit velocity.
pub fn velocity_of_steps(steps: f64) -> u16 {
    (steps * 512.0).round().clamp(0.0, 65535.0) as u16
}

/// A value in MIDI 1.0 steps with MIDI 2.0's fraction for display: two
/// decimals, cut (not rounded: the whole steps stay the MIDI 1.0 value).
pub fn steps_text(steps: f64) -> String {
    let cut = (steps * 100.0).floor() / 100.0;
    format!("{cut:.2}")
}

/// A 16-bit velocity as a note's 7-bit one (a note-on's at least 1).
fn seven(v: u16, note_on: bool) -> u8 {
    let s = (v >> 9) as u8;
    if note_on { s.max(1) } else { s }
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
    /// Placed in time rather than with the music: a tempo change keeps
    /// the note on its samples (start and end) while the clip's other
    /// notes follow the beats — a hit point inside a musical cue (see
    /// [`crate::time_lock`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub time_locked: bool,
    /// The articulation the note carries: an attribute of its track's
    /// articulation map (see [`crate::articulation`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub articulation: Option<crate::articulation::ArticulationId>,
    /// MIDI 2.0's resolution: 16-bit velocities and the note's attribute
    /// (recorded from MIDI 2.0 devices, edited at full resolution; `None`:
    /// the 7-bit values scaled up). See [`Self::velocity16`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fine: Option<NoteFine>,
}

impl MidiNote {
    pub fn end(&self) -> MusicalTime {
        self.start + self.length
    }

    /// The velocity at MIDI 2.0's 16 bits: the fine one while its top
    /// seven bits are `velocity`, else `velocity` scaled up the
    /// specification's way.
    pub fn velocity16(&self) -> u16 {
        self.fine
            .and_then(|f| f.velocity)
            .filter(|&v| seven(v, true) == self.velocity.max(1))
            .unwrap_or_else(|| {
                faderframe_midi::ump::scale_up(u32::from(self.velocity), 7, 16) as u16
            })
    }

    /// Set the velocity at 16 bits (`velocity` becomes its top seven bits,
    /// at least 1).
    pub fn set_velocity16(&mut self, v: u16) {
        self.velocity = seven(v, true);
        let up = faderframe_midi::ump::scale_up(u32::from(self.velocity), 7, 16) as u16;
        self.edit_fine(|f| f.velocity = (v != up).then_some(v));
    }

    /// The release velocity at 16 bits (none: a note-on of velocity 0
    /// ends the note).
    pub fn release16(&self) -> Option<u16> {
        let r = self.release?;
        Some(
            self.fine
                .and_then(|f| f.release)
                .filter(|&v| seven(v, false) == r)
                .unwrap_or_else(|| faderframe_midi::ump::scale_up(u32::from(r), 7, 16) as u16),
        )
    }

    pub fn set_release16(&mut self, v: Option<u16>) {
        self.release = v.map(|v| seven(v, false));
        let fine = match (v, self.release) {
            (Some(v), Some(r))
                if v != faderframe_midi::ump::scale_up(u32::from(r), 7, 16) as u16 =>
            {
                Some(v)
            }
            _ => None,
        };
        self.edit_fine(|f| f.release = fine);
    }

    /// The velocity as 0…1 at full resolution.
    pub fn velocity_unit(&self) -> f64 {
        f64::from(self.velocity16()) / 65535.0
    }

    /// The velocity says more than MIDI 1.0's seven bits.
    pub fn has_fine_velocity(&self) -> bool {
        self.velocity16() != faderframe_midi::ump::scale_up(u32::from(self.velocity), 7, 16) as u16
    }

    /// The velocity in MIDI 1.0 steps for display: "100", or with MIDI 2.0's
    /// resolution its fraction too ("100.56", see [`steps_text`]).
    pub fn velocity_text(&self) -> String {
        if self.has_fine_velocity() {
            steps_text(velocity_steps(self.velocity16()))
        } else {
            self.velocity.to_string()
        }
    }

    /// The release velocity likewise (`None`: none).
    pub fn release_text(&self) -> Option<String> {
        let r = self.release?;
        let v = self.release16()?;
        Some(
            if v == faderframe_midi::ump::scale_up(u32::from(r), 7, 16) as u16 {
                r.to_string()
            } else {
                steps_text(velocity_steps(v))
            },
        )
    }

    /// Set the 7-bit velocity as MIDI 1.0 edits do, keeping MIDI 2.0's
    /// resolution where the note has it: its 16 bits become `wanted` as near
    /// as the new 7-bit value allows.
    pub fn set_velocity_keeping(&mut self, velocity: u8, wanted: f64) {
        let velocity = velocity.max(1);
        if self.has_fine_velocity() {
            let lo = if velocity <= 1 {
                0.0
            } else {
                f64::from(u16::from(velocity) << 9)
            };
            let hi = f64::from((u16::from(velocity) << 9) | 511);
            self.set_velocity16(wanted.round().clamp(lo, hi) as u16);
        } else {
            self.set_velocity16(faderframe_midi::ump::scale_up(u32::from(velocity), 7, 16) as u16);
        }
    }

    /// The note-on's attribute (MIDI 2.0; kind 0 = none).
    pub fn attribute(&self) -> faderframe_midi::ump::Attribute {
        self.fine
            .map(|f| faderframe_midi::ump::Attribute {
                kind: f.attribute,
                data: f.attribute_data,
            })
            .unwrap_or_default()
    }

    pub fn set_attribute(&mut self, a: faderframe_midi::ump::Attribute) {
        self.edit_fine(|f| {
            f.attribute = a.kind;
            f.attribute_data = if a.kind == 0 { 0 } else { a.data };
        });
    }

    /// What its note-on carries beyond MIDI 1.0 (for playing).
    pub fn on_fine(&self) -> faderframe_midi::Fine {
        faderframe_midi::Fine::value(u32::from(self.velocity16())).with_attribute(self.attribute())
    }

    /// What its note-off carries beyond MIDI 1.0.
    pub fn off_fine(&self) -> faderframe_midi::Fine {
        match self.release16() {
            Some(v) => faderframe_midi::Fine::value(u32::from(v)),
            None => faderframe_midi::Fine::NONE,
        }
    }

    /// Drop fine values that no longer agree or say no more than the
    /// MIDI 1.0 ones.
    pub fn normalize_fine(&mut self) {
        let (v, r, a) = (self.velocity16(), self.release16(), self.attribute());
        self.fine = None;
        self.set_velocity16(v);
        self.set_release16(r);
        self.set_attribute(a);
    }

    fn edit_fine(&mut self, f: impl FnOnce(&mut NoteFine)) {
        let mut fine = self.fine.unwrap_or_default();
        f(&mut fine);
        self.fine = (!fine.is_empty()).then_some(fine);
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
    fn a_transposition_keeps_in_the_file_and_off_is_written_as_before() {
        #![allow(clippy::unwrap_used)]
        let up = StretchSettings::pitch(-3.0);
        let text = serde_json::to_string(&up).unwrap();
        assert_eq!(text, r#"{"mode":"pitch","semitones":-3.0}"#);
        let back: StretchSettings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.semitones(), -3.0);
        assert!((back.pitch_factor() - 2f32.powf(-0.25)).abs() < 1e-6);
        // Older files' "off" reads as before.
        assert_eq!(
            serde_json::to_string(&StretchSettings::Off).unwrap(),
            r#"{"mode":"off"}"#
        );
        assert_eq!(StretchSettings::Off.pitch_factor(), 1.0);
    }

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
                envelope: None,
            }),
        };
        assert_eq!(clip.end(&timeline, 48_000), MusicalTime::from_quarters_i(6));
        timeline.tempo.set_initial_bpm(60.0);
        assert_eq!(clip.end(&timeline, 48_000), MusicalTime::from_quarters_i(5));
    }

    fn note(velocity: u8) -> MidiNote {
        MidiNote {
            id: NoteId(1),
            start: MusicalTime::ZERO,
            length: MusicalTime::QUARTER,
            key: 60,
            velocity,
            channel: 0,
            muted: false,
            release: None,
            time_locked: false,
            articulation: None,
            fine: None,
        }
    }

    #[test]
    fn a_note_keeps_its_16_bit_velocity_while_it_agrees() {
        let up = |v: u8| faderframe_midi::ump::scale_up(u32::from(v), 7, 16) as u16;
        let mut n = note(100);
        assert_eq!(n.velocity16(), up(100), "scaled up the MIDI 2.0 way");
        assert_eq!(up(127), 0xFFFF);
        assert!(!n.has_fine_velocity());
        assert_eq!(n.velocity_text(), "100");
        n.set_velocity16(0x1234);
        assert_eq!((n.velocity, n.velocity16()), (9, 0x1234));
        assert_eq!(n.velocity_text(), "9.10");
        assert_eq!(n.on_fine().value, Some(0x1234));
        // Changed at seven bits (an older edit, a MIDI 1.0 transform): the
        // 16 bits no longer count.
        n.velocity = 80;
        assert_eq!(n.velocity16(), up(80));
        n.normalize_fine();
        assert_eq!(n.fine, None);
        // The scaled-up value is no fine value.
        n.set_velocity16(up(100));
        assert_eq!((n.velocity, n.fine), (100, None));
        // Release velocities and attributes likewise.
        n.set_release16(Some(0x7F00));
        assert_eq!((n.release, n.release16()), (Some(63), Some(0x7F00)));
        assert_eq!(n.release_text().as_deref(), Some("63.50"));
        n.set_attribute(faderframe_midi::ump::Attribute { kind: 1, data: 5 });
        assert_eq!(n.on_fine().attribute.data, 5);
        // MIDI 2.0's note-on below 1/128: a 7-bit velocity of 1.
        n.set_velocity16(40);
        assert_eq!((n.velocity, n.velocity16()), (1, 40));
        // Edits at seven bits keep the resolution where there is one.
        n.set_velocity16(51456 + 100);
        n.set_velocity_keeping(110, 51556.0 * 1.1);
        assert_eq!(n.velocity, 110);
        assert_eq!(n.velocity16() >> 9, 110);
        assert!(n.has_fine_velocity());
        let mut plain = note(100);
        plain.set_velocity_keeping(110, 60_000.0);
        assert_eq!((plain.velocity, plain.fine), (110, None));
    }

    #[test]
    fn old_notes_and_points_load_and_new_ones_keep_their_resolution() {
        // A note and a point as files before MIDI 2.0 resolution hold them.
        let old: MidiNote = serde_json::from_str(
            r#"{"id":7,"start":0,"length":960,"key":64,"velocity":90,"channel":0}"#,
        )
        .unwrap();
        assert_eq!(
            (old.fine, old.velocity16()),
            (None, faderframe_midi::ump::scale_up(90, 7, 16) as u16)
        );
        let old_point: ControllerPoint = serde_json::from_str(r#"{"time":0,"value":64}"#).unwrap();
        assert_eq!(old_point.value32(MidiController::MOD_WHEEL), 0x8000_0000);
        // Nothing new is written for MIDI 1.0 values.
        let json = serde_json::to_string(&old).unwrap();
        assert!(!json.contains("fine"), "{json}");
        // MIDI 2.0 values round trip.
        let mut n = old;
        n.set_velocity16(0xB3C5);
        n.set_release16(Some(0x0101));
        n.set_attribute(faderframe_midi::ump::Attribute {
            kind: 2,
            data: 0xBEEF,
        });
        let back: MidiNote = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
        assert_eq!(back.velocity16(), 0xB3C5);
        let p = ControllerPoint::at32(MusicalTime::QUARTER, MidiController::PitchBend, 0x9234_5678);
        assert_eq!(u32::from(p.value), 0x9234_5678u32 >> 18);
        let back: ControllerPoint =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.value32(MidiController::PitchBend), 0x9234_5678);
    }

    #[test]
    fn controller_points_hold_32_bits_in_midi_1_steps() {
        let c = MidiController::MOD_WHEEL;
        let p = ControllerPoint::of_steps(MusicalTime::ZERO, c, 64.5);
        assert_eq!(p.value, 64);
        assert_eq!(p.value32(c), 0x8100_0000);
        assert_eq!(p.steps_text(c), "64.50");
        let whole = ControllerPoint::of_steps(MusicalTime::ZERO, c, 64.0);
        assert_eq!((whole.value, whole.fine), (64, None));
        // Programs have no 32-bit value.
        let pc = ControllerPoint::at32(MusicalTime::ZERO, MidiController::Program, 0x0500_0000);
        assert_eq!((pc.value, pc.fine), (2, None));
        // A lane drops repeats at full resolution only, and stale 32 bits.
        let mut lane = ControllerLane::new(c, 0);
        lane.points = vec![
            ControllerPoint::of_steps(MusicalTime(0), c, 10.25),
            ControllerPoint::of_steps(MusicalTime(10), c, 10.75),
            ControllerPoint::of_steps(MusicalTime(20), c, 10.75),
            ControllerPoint {
                time: MusicalTime(30),
                value: 90,
                fine: Some(0x8000_0001),
            },
        ];
        lane.normalize();
        assert_eq!(lane.points.len(), 3);
        assert_eq!(lane.points[2].fine, None, "it no longer agreed");
        assert_eq!(
            lane.value32_at(MusicalTime(15)),
            Some(lane.points[1].value32(c))
        );
        let (ev, fine) = c.event_fine(3, &lane.points[0]);
        assert_eq!(ev.full_value(fine), Some(lane.points[0].value32(c)));
    }
}
