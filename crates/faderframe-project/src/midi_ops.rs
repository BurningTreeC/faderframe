//! Note editing operations (quantize, humanize, legato, transpose, …),
//! scales and chords — pure functions on note lists, used by the piano roll
//! through session actions so every operation is one undoable command.
//!
//! Times are clip-relative; operations that need the grid get the clip's
//! timeline position and the meter, so grids stay bar-aligned in any time
//! signature.

use crate::MidiNote;
use faderframe_timeline::{GridDivision, MusicalTime, TimeSignatureMap};
use serde::{Deserialize, Serialize};

// --- scales -------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleKind {
    #[default]
    Chromatic,
    Major,
    NaturalMinor,
    HarmonicMinor,
    MelodicMinor,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Locrian,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
    WholeTone,
}

impl ScaleKind {
    pub const ALL: [ScaleKind; 14] = [
        ScaleKind::Chromatic,
        ScaleKind::Major,
        ScaleKind::NaturalMinor,
        ScaleKind::HarmonicMinor,
        ScaleKind::MelodicMinor,
        ScaleKind::Dorian,
        ScaleKind::Phrygian,
        ScaleKind::Lydian,
        ScaleKind::Mixolydian,
        ScaleKind::Locrian,
        ScaleKind::MajorPentatonic,
        ScaleKind::MinorPentatonic,
        ScaleKind::Blues,
        ScaleKind::WholeTone,
    ];

    /// Semitones above the root.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            ScaleKind::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            ScaleKind::Major => &[0, 2, 4, 5, 7, 9, 11],
            ScaleKind::NaturalMinor => &[0, 2, 3, 5, 7, 8, 10],
            ScaleKind::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            ScaleKind::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
            ScaleKind::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            ScaleKind::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            ScaleKind::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            ScaleKind::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            ScaleKind::Locrian => &[0, 1, 3, 5, 6, 8, 10],
            ScaleKind::MajorPentatonic => &[0, 2, 4, 7, 9],
            ScaleKind::MinorPentatonic => &[0, 3, 5, 7, 10],
            ScaleKind::Blues => &[0, 3, 5, 6, 7, 10],
            ScaleKind::WholeTone => &[0, 2, 4, 6, 8, 10],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScaleKind::Chromatic => "Chromatic",
            ScaleKind::Major => "Major",
            ScaleKind::NaturalMinor => "Minor",
            ScaleKind::HarmonicMinor => "Harmonic Minor",
            ScaleKind::MelodicMinor => "Melodic Minor",
            ScaleKind::Dorian => "Dorian",
            ScaleKind::Phrygian => "Phrygian",
            ScaleKind::Lydian => "Lydian",
            ScaleKind::Mixolydian => "Mixolydian",
            ScaleKind::Locrian => "Locrian",
            ScaleKind::MajorPentatonic => "Major Pentatonic",
            ScaleKind::MinorPentatonic => "Minor Pentatonic",
            ScaleKind::Blues => "Blues",
            ScaleKind::WholeTone => "Whole Tone",
        }
    }
}

pub const PITCH_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// A key: root pitch class and scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scale {
    /// Pitch class 0 (C) – 11 (B).
    pub root: u8,
    pub kind: ScaleKind,
}

impl Scale {
    pub fn new(root: u8, kind: ScaleKind) -> Self {
        Self {
            root: root % 12,
            kind,
        }
    }

    pub fn is_chromatic(&self) -> bool {
        self.kind == ScaleKind::Chromatic
    }

    pub fn label(&self) -> String {
        if self.is_chromatic() {
            "Chromatic".into()
        } else {
            format!("{} {}", PITCH_NAMES[self.root as usize], self.kind.label())
        }
    }

    pub fn contains(&self, key: u8) -> bool {
        let pc = (key + 12 - self.root % 12) % 12;
        self.kind.intervals().contains(&pc)
    }

    /// Is `key` the root?
    pub fn is_root(&self, key: u8) -> bool {
        key % 12 == self.root % 12
    }

    /// The nearest key in the scale (ties go down).
    pub fn nearest(&self, key: u8) -> u8 {
        for d in 0..12i32 {
            for k in [key as i32 - d, key as i32 + d] {
                if (0..=127).contains(&k) && self.contains(k as u8) {
                    return k as u8;
                }
            }
        }
        key
    }

    /// `key` moved by `steps` scale degrees (keys outside the scale move to
    /// the nearest degree first).
    pub fn step(&self, key: u8, steps: i32) -> u8 {
        let mut k = self.nearest(key) as i32;
        let dir = steps.signum();
        for _ in 0..steps.abs() {
            loop {
                k += dir;
                if !(0..=127).contains(&k) {
                    return (k - dir).clamp(0, 127) as u8;
                }
                if self.contains(k as u8) {
                    break;
                }
            }
        }
        k as u8
    }

    /// Keys of the scale (all 128 for chromatic), ascending.
    pub fn keys(&self) -> Vec<u8> {
        (0..=127u8).filter(|k| self.contains(*k)).collect()
    }

    /// The scale of a key from the project's key track.
    pub fn of_key(key: faderframe_midi::theory::Key) -> Self {
        use faderframe_midi::theory::Scale as S;
        let kind = match key.scale {
            S::Major => ScaleKind::Major,
            S::Minor => ScaleKind::NaturalMinor,
            S::HarmonicMinor => ScaleKind::HarmonicMinor,
            S::MelodicMinor => ScaleKind::MelodicMinor,
            S::Dorian => ScaleKind::Dorian,
            S::Phrygian => ScaleKind::Phrygian,
            S::Lydian => ScaleKind::Lydian,
            S::Mixolydian => ScaleKind::Mixolydian,
            S::Locrian => ScaleKind::Locrian,
            S::MajorPentatonic => ScaleKind::MajorPentatonic,
            S::MinorPentatonic => ScaleKind::MinorPentatonic,
            S::Blues => ScaleKind::Blues,
            S::WholeTone => ScaleKind::WholeTone,
            S::Chromatic => ScaleKind::Chromatic,
        };
        Self::new(key.root, kind)
    }
}

// --- chords -----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChordKind {
    /// One note (no chord).
    #[default]
    Single,
    Major,
    Minor,
    Diminished,
    Augmented,
    Sus2,
    Sus4,
    Major7,
    Minor7,
    Dominant7,
    Power,
    Octave,
    /// Three notes of the scale, stacked in thirds from the clicked key.
    ScaleTriad,
    /// Four notes of the scale, stacked in thirds.
    ScaleSeventh,
    /// The chord track's chord where the note goes (a scale triad where
    /// there is none).
    ChordTrack,
}

impl ChordKind {
    pub const ALL: [ChordKind; 15] = [
        ChordKind::Single,
        ChordKind::ChordTrack,
        ChordKind::ScaleTriad,
        ChordKind::ScaleSeventh,
        ChordKind::Major,
        ChordKind::Minor,
        ChordKind::Diminished,
        ChordKind::Augmented,
        ChordKind::Sus2,
        ChordKind::Sus4,
        ChordKind::Major7,
        ChordKind::Minor7,
        ChordKind::Dominant7,
        ChordKind::Power,
        ChordKind::Octave,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ChordKind::Single => "Single Note",
            ChordKind::Major => "Major",
            ChordKind::Minor => "Minor",
            ChordKind::Diminished => "Diminished",
            ChordKind::Augmented => "Augmented",
            ChordKind::Sus2 => "Sus2",
            ChordKind::Sus4 => "Sus4",
            ChordKind::Major7 => "Major 7",
            ChordKind::Minor7 => "Minor 7",
            ChordKind::Dominant7 => "Dominant 7",
            ChordKind::Power => "Power (5)",
            ChordKind::Octave => "Octave",
            ChordKind::ScaleTriad => "Scale Triad",
            ChordKind::ScaleSeventh => "Scale Seventh",
            ChordKind::ChordTrack => "From the Chord Track",
        }
    }

    /// The chord's keys with `root` as the lowest note.
    pub fn keys(self, root: u8, scale: &Scale) -> Vec<u8> {
        let fixed: &[i32] = match self {
            ChordKind::Single => &[0],
            ChordKind::Major => &[0, 4, 7],
            ChordKind::Minor => &[0, 3, 7],
            ChordKind::Diminished => &[0, 3, 6],
            ChordKind::Augmented => &[0, 4, 8],
            ChordKind::Sus2 => &[0, 2, 7],
            ChordKind::Sus4 => &[0, 5, 7],
            ChordKind::Major7 => &[0, 4, 7, 11],
            ChordKind::Minor7 => &[0, 3, 7, 10],
            ChordKind::Dominant7 => &[0, 4, 7, 10],
            ChordKind::Power => &[0, 7],
            ChordKind::Octave => &[0, 12],
            ChordKind::ScaleTriad | ChordKind::ScaleSeventh | ChordKind::ChordTrack => {
                let n = if self == ChordKind::ScaleSeventh {
                    4
                } else {
                    3
                };
                let diatonic = if scale.is_chromatic() {
                    Scale::new(root % 12, ScaleKind::Major)
                } else {
                    *scale
                };
                let base = diatonic.nearest(root);
                return (0..n)
                    .map(|i| diatonic.step(base, 2 * i))
                    .filter(|k| *k <= 127)
                    .collect();
            }
        };
        fixed
            .iter()
            .map(|i| root as i32 + i)
            .filter(|k| (0..=127).contains(k))
            .map(|k| k as u8)
            .collect()
    }
}

// --- operations ------------------------------------------------------------------

/// How quantize moves notes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuantizeSettings {
    pub grid: GridDivision,
    /// 0..=1: how far towards the grid (1 = all the way).
    pub strength: f32,
    /// 0..=1: delay of every second grid point (0.5 = triplet feel).
    pub swing: f32,
    /// Move note starts.
    pub starts: bool,
    /// Quantize note ends (lengths).
    pub ends: bool,
}

impl Default for QuantizeSettings {
    fn default() -> Self {
        Self {
            grid: GridDivision::Note(16),
            strength: 1.0,
            swing: 0.0,
            starts: true,
            ends: false,
        }
    }
}

/// The quantize target of an absolute time (nearest grid point, swing
/// applied to odd grid points).
fn quantize_point(t: MusicalTime, q: &QuantizeSettings, meter: &TimeSignatureMap) -> MusicalTime {
    let bar = meter.bar_at(t);
    let bar_start = meter.bar_start(bar);
    let step = q.grid.step(meter.signature_of_bar(bar)).ticks().max(1);
    let rel = (t - bar_start).ticks();
    let k = (rel as f64 / step as f64).round() as i64;
    let mut target = k * step;
    if k % 2 == 1 && q.swing > 0.0 {
        target += (step as f64 * q.swing.clamp(0.0, 1.0) as f64 * 0.5) as i64;
    }
    bar_start + MusicalTime(target)
}

/// Where quantize moves a time: towards its grid point (swing applied) by
/// the strength.
pub fn quantize_target(
    t: MusicalTime,
    q: &QuantizeSettings,
    meter: &TimeSignatureMap,
) -> MusicalTime {
    towards(t, quantize_point(t, q, meter), q.strength)
}

fn towards(from: MusicalTime, to: MusicalTime, strength: f32) -> MusicalTime {
    let s = strength.clamp(0.0, 1.0) as f64;
    MusicalTime(from.ticks() + ((to.ticks() - from.ticks()) as f64 * s).round() as i64)
}

/// Quantize notes of a clip that starts at `clip_start`.
pub fn quantize(
    notes: &mut [MidiNote],
    clip_start: MusicalTime,
    q: &QuantizeSettings,
    meter: &TimeSignatureMap,
) {
    for n in notes {
        let start = clip_start + n.start;
        let end = start + n.length;
        let new_start = if q.starts {
            towards(start, quantize_point(start, q, meter), q.strength)
        } else {
            start
        };
        let new_end = if q.ends {
            towards(end, quantize_point(end, q, meter), q.strength)
        } else {
            new_start + n.length
        };
        let min =
            MusicalTime(q.grid.step(meter.signature_at(start)).ticks() / 4).max(MusicalTime(1));
        n.start = (new_start - clip_start).max(MusicalTime::ZERO);
        n.length = (new_end - new_start).max(min);
    }
}

/// Small deterministic random numbers (xorshift) for humanize.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Uniform in −1..1.
    pub fn signed(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// Random offsets up to `timing` and `velocity` (deterministic per seed).
pub fn humanize(notes: &mut [MidiNote], timing: MusicalTime, velocity: u8, seed: u64) {
    let mut rng = Rng::new(seed);
    for n in notes {
        let dt = (rng.signed() * timing.ticks() as f64).round() as i64;
        n.start = (n.start + MusicalTime(dt)).max(MusicalTime::ZERO);
        let dv = (rng.signed() * velocity as f64).round() as i32;
        n.velocity = (n.velocity as i32 + dv).clamp(1, 127) as u8;
    }
}

/// Every note lasts until the next note starts (chords share the next
/// start); the last ones keep their length.
pub fn legato(notes: &mut [MidiNote]) {
    let mut starts: Vec<MusicalTime> = notes.iter().map(|n| n.start).collect();
    starts.sort();
    starts.dedup();
    for n in notes {
        let i = starts.partition_point(|s| *s <= n.start);
        if let Some(next) = starts.get(i) {
            n.length = *next - n.start;
        }
    }
}

pub fn transpose(notes: &mut [MidiNote], semitones: i32) {
    for n in notes {
        n.key = (n.key as i32 + semitones).clamp(0, 127) as u8;
    }
}

/// Transpose by scale degrees.
pub fn transpose_in_scale(notes: &mut [MidiNote], steps: i32, scale: &Scale) {
    for n in notes {
        n.key = scale.step(n.key, steps);
    }
}

pub fn set_length(notes: &mut [MidiNote], length: MusicalTime) {
    for n in notes {
        n.length = length.max(MusicalTime(1));
    }
}

/// Scale lengths by `factor` (0.5 = half as long).
pub fn scale_length(notes: &mut [MidiNote], factor: f64) {
    for n in notes {
        n.length = MusicalTime(((n.length.ticks() as f64) * factor).round().max(1.0) as i64);
    }
}

/// Mirror the notes in time inside the span they occupy.
pub fn reverse(notes: &mut [MidiNote]) {
    let (Some(a), Some(b)) = (
        notes.iter().map(|n| n.start).min(),
        notes.iter().map(|n| n.end()).max(),
    ) else {
        return;
    };
    for n in notes {
        n.start = a + (b - n.end());
    }
}

/// Mirror pitches around the middle of the range they occupy.
pub fn invert(notes: &mut [MidiNote]) {
    let (Some(lo), Some(hi)) = (
        notes.iter().map(|n| n.key).min(),
        notes.iter().map(|n| n.key).max(),
    ) else {
        return;
    };
    for n in notes {
        n.key = hi - (n.key - lo);
    }
}

/// `v * factor + offset`, clamped to 1..=127.
pub fn scale_velocity(notes: &mut [MidiNote], factor: f32, offset: i32) {
    for n in notes {
        n.velocity = ((n.velocity as f32 * factor).round() as i32 + offset).clamp(1, 127) as u8;
    }
}

pub fn set_velocity(notes: &mut [MidiNote], velocity: u8) {
    for n in notes {
        n.velocity = velocity.clamp(1, 127);
    }
}

/// Velocities in a straight line from `from` (earliest note) to `to`
/// (latest).
pub fn ramp_velocity(notes: &mut [MidiNote], from: u8, to: u8) {
    let (Some(a), Some(b)) = (
        notes.iter().map(|n| n.start).min(),
        notes.iter().map(|n| n.start).max(),
    ) else {
        return;
    };
    let span = (b - a).ticks().max(1) as f32;
    for n in notes {
        let t = (n.start - a).ticks() as f32 / span;
        n.velocity = (from as f32 + (to as f32 - from as f32) * t)
            .round()
            .clamp(1.0, 127.0) as u8;
    }
}

/// Move keys outside the scale to the nearest key in it.
pub fn fold_to_scale(notes: &mut [MidiNote], scale: &Scale) {
    for n in notes {
        n.key = scale.nearest(n.key);
    }
}

/// Overlapping notes of the same key and channel: earlier ones end where the
/// next one starts.
pub fn remove_overlaps(notes: &mut [MidiNote]) {
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by_key(|&i| (notes[i].channel, notes[i].key, notes[i].start));
    for w in order.windows(2) {
        let (a, b) = (w[0], w[1]);
        if notes[a].key == notes[b].key
            && notes[a].channel == notes[b].channel
            && notes[a].end() > notes[b].start
        {
            notes[a].length = (notes[b].start - notes[a].start).max(MusicalTime(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::NoteId;

    fn note(id: u64, start: f64, len: f64, key: u8) -> MidiNote {
        MidiNote {
            id: NoteId(id),
            start: MusicalTime::from_quarters(start),
            length: MusicalTime::from_quarters(len),
            key,
            velocity: 100,
            channel: 0,
            muted: false,
        }
    }

    #[test]
    fn scales_contain_snap_and_step() {
        let c_major = Scale::new(0, ScaleKind::Major);
        assert!(c_major.contains(60) && c_major.contains(64) && !c_major.contains(61));
        assert_eq!(c_major.nearest(61), 60);
        assert_eq!(c_major.step(60, 2), 64);
        assert_eq!(c_major.step(64, -2), 60);
        assert_eq!(c_major.step(71, 1), 72);
        let a_minor = Scale::new(9, ScaleKind::NaturalMinor);
        assert!(a_minor.contains(57) && a_minor.contains(60) && !a_minor.contains(61));
        assert_eq!(a_minor.label(), "A Minor");
        assert_eq!(Scale::default().keys().len(), 128);
    }

    #[test]
    fn chords_stack_on_the_clicked_key() {
        let c = Scale::new(0, ScaleKind::Major);
        assert_eq!(ChordKind::Major.keys(60, &c), vec![60, 64, 67]);
        assert_eq!(ChordKind::Minor7.keys(57, &c), vec![57, 60, 64, 67]);
        // Diatonic: D in C major is a minor triad, B is diminished.
        assert_eq!(ChordKind::ScaleTriad.keys(62, &c), vec![62, 65, 69]);
        assert_eq!(ChordKind::ScaleTriad.keys(71, &c), vec![71, 74, 77]);
        assert_eq!(ChordKind::ScaleSeventh.keys(60, &c), vec![60, 64, 67, 71]);
        assert_eq!(
            ChordKind::Octave.keys(120, &c),
            vec![120],
            "clamped to MIDI range"
        );
    }

    #[test]
    fn quantize_strength_swing_and_ends() {
        let meter = TimeSignatureMap::new(faderframe_timeline::TimeSignature::FOUR_FOUR);
        let sixteenth = 0.25;
        let mut n = vec![note(1, 0.1, 0.4, 60), note(2, 0.27, 0.2, 62)];
        quantize(
            &mut n,
            MusicalTime::ZERO,
            &QuantizeSettings::default(),
            &meter,
        );
        assert_eq!(n[0].start, MusicalTime::ZERO);
        assert_eq!(n[1].start, MusicalTime::from_quarters(sixteenth));
        assert_eq!(n[0].length, MusicalTime::from_quarters(0.4), "length kept");

        let mut half = vec![note(1, 0.1, 0.4, 60)];
        let q = QuantizeSettings {
            strength: 0.5,
            ..Default::default()
        };
        quantize(&mut half, MusicalTime::ZERO, &q, &meter);
        assert_eq!(half[0].start, MusicalTime::from_quarters(0.05));

        // Swing 50 %: odd sixteenths land an eighth of a sixteenth late …
        let mut sw = vec![note(1, 0.25, 0.1, 60), note(2, 0.5, 0.1, 60)];
        let q = QuantizeSettings {
            swing: 0.5,
            ..Default::default()
        };
        quantize(&mut sw, MusicalTime::ZERO, &q, &meter);
        assert_eq!(sw[0].start, MusicalTime::from_quarters(0.25 + 0.0625));
        assert_eq!(
            sw[1].start,
            MusicalTime::from_quarters(0.5),
            "even ones stay"
        );

        // Ends: lengths snap too; the grid is bar-aligned for clips that
        // start mid-bar.
        let mut e = vec![note(1, 0.2, 0.3, 60), note(2, 0.0, 0.1, 62)];
        let q = QuantizeSettings {
            ends: true,
            ..Default::default()
        };
        let at = MusicalTime::from_quarters(1.1);
        quantize(&mut e, at, &q, &meter);
        assert_eq!(at + e[0].start, MusicalTime::from_quarters(1.25));
        assert_eq!(at + e[0].end(), MusicalTime::from_quarters(1.5));
        assert_eq!(e[1].start, MusicalTime::ZERO, "never before the clip start");
    }

    #[test]
    fn legato_reverse_invert_and_overlaps() {
        let mut n = vec![
            note(1, 0.0, 0.2, 60),
            note(2, 0.0, 0.2, 64),
            note(3, 1.0, 0.5, 67),
        ];
        legato(&mut n);
        assert_eq!(n[0].length, MusicalTime::QUARTER);
        assert_eq!(n[1].length, MusicalTime::QUARTER);
        assert_eq!(n[2].length, MusicalTime::from_quarters(0.5), "last keeps");

        let mut r = vec![note(1, 0.0, 1.0, 60), note(2, 2.0, 1.0, 62)];
        reverse(&mut r);
        assert_eq!(r[0].start, MusicalTime::from_quarters(2.0));
        assert_eq!(r[1].start, MusicalTime::ZERO);

        let mut i = vec![note(1, 0.0, 1.0, 60), note(2, 0.0, 1.0, 67)];
        invert(&mut i);
        assert_eq!((i[0].key, i[1].key), (67, 60));

        let mut o = vec![note(1, 0.0, 2.0, 60), note(2, 1.0, 1.0, 60)];
        remove_overlaps(&mut o);
        assert_eq!(o[0].length, MusicalTime::QUARTER);
    }

    #[test]
    fn velocity_tools() {
        let mut n = vec![
            note(1, 0.0, 1.0, 60),
            note(2, 1.0, 1.0, 60),
            note(3, 2.0, 1.0, 60),
        ];
        ramp_velocity(&mut n, 20, 120);
        assert_eq!(
            n.iter().map(|n| n.velocity).collect::<Vec<_>>(),
            vec![20, 70, 120]
        );
        scale_velocity(&mut n, 2.0, 0);
        assert_eq!(n[2].velocity, 127, "clamped");
        humanize(&mut n, MusicalTime::from_quarters(0.05), 10, 7);
        assert!(n.iter().all(|n| n.velocity >= 1));
        let mut again = vec![note(1, 0.0, 1.0, 60)];
        let mut same = again.clone();
        humanize(&mut again, MusicalTime::from_quarters(0.1), 10, 3);
        humanize(&mut same, MusicalTime::from_quarters(0.1), 10, 3);
        assert_eq!(again, same, "deterministic");
    }
}
