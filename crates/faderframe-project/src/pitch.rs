//! Pitch editing of audio clips: the notes a monophonic recording holds,
//! each moved, straightened or given other formants on its own.
//!
//! A clip's [`PitchEdit`] lists its notes in source frames (at the
//! project rate, like `AudioClip::source_offset`), with the pitch they
//! were sung at and the curve around it (cents every `hop` frames), so a
//! project plays and renders without the analysis that found them. What a
//! note plays is its sung curve moved by `shift`, with `drift` of its
//! wandering around its pitch straightened; [`PitchEdit::correction_at`]
//! gives the transposition (and formant move) at a source frame, gliding
//! between neighbouring notes so no edit jumps.

use serde::{Deserialize, Serialize};

/// A curve value where no pitch was heard.
pub const UNVOICED: i16 = i16::MIN;
/// Furthest a note moves (semitones, either way).
pub const MAX_SHIFT: f32 = 24.0;
/// Furthest its formants move.
pub const MAX_FORMANT: f32 = 12.0;
/// Corrections glide over this long between notes (seconds).
const GLIDE_SECONDS: f64 = 0.03;
/// Gaps up to this long glide from one note's correction to the next's;
/// longer ones go back to none.
const BRIDGE_SECONDS: f64 = 0.2;

fn yes() -> bool {
    true
}

/// A clip's pitch edit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PitchEdit {
    /// Source frames between curve values.
    pub hop: u32,
    /// Sorted by start, not overlapping.
    pub notes: Vec<PitchNote>,
    /// Keep the formants where the pitch moves (a voice keeps its size;
    /// off, they move with the pitch).
    #[serde(default = "yes")]
    pub keep_formants: bool,
}

/// One note of a [`PitchEdit`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PitchNote {
    /// Source frames it spans.
    pub start: i64,
    pub end: i64,
    /// The pitch it was sung at (MIDI note, fractional).
    pub pitch: f32,
    /// Semitones it is moved by.
    #[serde(default)]
    pub shift: f32,
    /// How much of its wandering around `pitch` is straightened (0…1).
    #[serde(default)]
    pub drift: f32,
    /// Semitones its formants move.
    #[serde(default)]
    pub formant: f32,
    /// The sung pitch every `hop` frames from `start`, in cents from
    /// `pitch` ([`UNVOICED`] where none was heard).
    #[serde(default)]
    pub curve: Vec<i16>,
}

impl PitchNote {
    /// Has it been edited?
    pub fn edited(&self) -> bool {
        self.shift != 0.0 || self.drift != 0.0 || self.formant != 0.0
    }

    /// The pitch sung at source frame `at` (inside the note), `None`
    /// where unvoiced.
    pub fn sung_at(&self, at: i64, hop: u32) -> Option<f32> {
        let x = (at - self.start) as f64 / f64::from(hop.max(1));
        if x < 0.0 || self.curve.is_empty() {
            return None;
        }
        let i = (x.floor() as usize).min(self.curve.len() - 1);
        let j = (i + 1).min(self.curve.len() - 1);
        let (a, b) = (self.curve[i], self.curve[j]);
        let cents = match (a == UNVOICED, b == UNVOICED) {
            (true, true) => return None,
            (false, true) => f32::from(a),
            (true, false) => f32::from(b),
            (false, false) => {
                let f = (x - i as f64) as f32;
                f32::from(a) + (f32::from(b) - f32::from(a)) * f
            }
        };
        Some(self.pitch + cents / 100.0)
    }

    /// Semitones the pitch at `at` is moved by: the shift, less the
    /// straightened part of its wandering.
    pub fn correction_at(&self, at: i64, hop: u32) -> f32 {
        match self.sung_at(at, hop) {
            Some(sung) => self.shift - self.drift * (sung - self.pitch),
            None => self.shift,
        }
    }

    /// What it plays at `at` (MIDI), `None` where unvoiced.
    pub fn played_at(&self, at: i64, hop: u32) -> Option<f32> {
        self.sung_at(at, hop)
            .map(|s| s + self.correction_at(at, hop))
    }

    /// The pitch it is heard at now (its centre, moved).
    pub fn heard(&self) -> f32 {
        self.pitch + self.shift
    }
}

impl PitchEdit {
    pub fn edited(&self) -> bool {
        self.notes.iter().any(PitchNote::edited)
    }

    /// The note playing at source frame `at`.
    pub fn note_at(&self, at: i64) -> Option<usize> {
        let i = self.notes.partition_point(|n| n.start <= at);
        (i > 0 && at < self.notes[i - 1].end).then(|| i - 1)
    }

    /// (semitones, formant semitones) at source frame `at`, for a project
    /// at `rate` (see the module docs).
    pub fn correction_at(&self, at: i64, rate: f64) -> (f32, f32) {
        let glide = (GLIDE_SECONDS * rate) as i64;
        let bridge = (BRIDGE_SECONDS * rate) as i64;
        let hop = self.hop;
        let edge_in = |n: &PitchNote| (n.correction_at(n.start, hop), n.formant);
        let edge_out = |n: &PitchNote| (n.correction_at(n.end - 1, hop), n.formant);
        let lerp = |a: (f32, f32), b: (f32, f32), t: f64| {
            let t = smooth(t) as f32;
            (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
        };
        // The next note starting after `at` and the last one before.
        let i = self.notes.partition_point(|n| n.start <= at);
        let next = self.notes.get(i);
        let Some(cur) = i.checked_sub(1).map(|k| &self.notes[k]) else {
            // Before the first note: into it.
            return match next {
                Some(n) if n.start - at <= glide => lerp(
                    (0.0, 0.0),
                    edge_in(n),
                    1.0 - (n.start - at) as f64 / glide as f64,
                ),
                _ => (0.0, 0.0),
            };
        };
        if at >= cur.end {
            // In a gap.
            return match next {
                Some(n) if n.start - cur.end <= bridge => lerp(
                    edge_out(cur),
                    edge_in(n),
                    (at - cur.end) as f64 / (n.start - cur.end).max(1) as f64,
                ),
                Some(n) if n.start - at <= glide => lerp(
                    (0.0, 0.0),
                    edge_in(n),
                    1.0 - (n.start - at) as f64 / glide as f64,
                ),
                _ if at - cur.end < glide => lerp(
                    edge_out(cur),
                    (0.0, 0.0),
                    (at - cur.end) as f64 / glide as f64,
                ),
                _ => (0.0, 0.0),
            };
        }
        let here = (cur.correction_at(at, hop), cur.formant);
        // Next to a touching neighbour: glide across the boundary.
        let half = glide / 2;
        if let Some(n) = next
            && n.start - cur.end <= 1
            && n.start - at <= half
        {
            let t = 0.5 - (n.start - at) as f64 / (2 * half.max(1)) as f64;
            return lerp(here, edge_in(n), t);
        }
        if i >= 2 {
            let p = &self.notes[i - 2];
            if cur.start - p.end <= 1 && at - cur.start < half {
                let t = 0.5 + (at - cur.start) as f64 / (2 * half.max(1)) as f64;
                return lerp(edge_out(p), here, t);
            }
        }
        here
    }

    /// The correction every `step` source frames from `from` (`count`
    /// values): what the engine plays.
    pub fn corrections(&self, from: i64, step: f64, count: usize, rate: f64) -> Vec<(f32, f32)> {
        (0..count)
            .map(|k| self.correction_at(from + (k as f64 * step).round() as i64, rate))
            .collect()
    }
}

/// Smoothstep: 0 → 0, 1 → 1, flat at both ends.
fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The note nearest `pitch` among those of a scale (`root` 0–11 = C–B,
/// `steps` bit k = root + k semitones), or the nearest semitone without
/// one.
pub fn snap(pitch: f32, scale: Option<(u8, u16)>) -> f32 {
    let Some((root, steps)) = scale.filter(|(_, s)| *s & 0xfff != 0) else {
        return pitch.round();
    };
    let base = pitch.round() as i32;
    (-6..=6)
        .map(|d| base + d)
        .filter(|n| steps >> (n - i32::from(root)).rem_euclid(12) & 1 == 1)
        .map(|n| n as f32)
        .min_by(|a, b| (a - pitch).abs().total_cmp(&(b - pitch).abs()))
        .unwrap_or(pitch.round())
}

/// The median of a note's voiced curve (cents).
fn median_cents(curve: &[i16]) -> Option<f32> {
    let mut v: Vec<i16> = curve.iter().copied().filter(|c| *c != UNVOICED).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(f32::from(v[v.len() / 2]))
}

/// `curve` measured from a pitch `by` semitones higher.
fn rebase(curve: &mut [i16], by: f32) {
    let d = (by * 100.0).round() as i32;
    for c in curve.iter_mut().filter(|c| **c != UNVOICED) {
        *c = (i32::from(*c) - d).clamp(i32::from(i16::MIN) + 1, i32::from(i16::MAX)) as i16;
    }
}

/// Split `note` at source frame `at` into two notes, each with its own
/// pitch (the edits stay with both).
pub fn split(note: &PitchNote, at: i64, hop: u32) -> Option<(PitchNote, PitchNote)> {
    if at <= note.start || at >= note.end {
        return None;
    }
    let k =
        (((at - note.start) as f64 / f64::from(hop.max(1))).round() as usize).min(note.curve.len());
    let mut a = note.clone();
    let mut b = note.clone();
    a.end = at;
    a.curve.truncate(k);
    b.start = at;
    b.curve = note.curve[k..].to_vec();
    for n in [&mut a, &mut b] {
        if let Some(m) = median_cents(&n.curve) {
            n.pitch += m / 100.0;
            rebase(&mut n.curve, m / 100.0);
        }
    }
    Some((a, b))
}

/// Join neighbouring notes into one (the gap unvoiced; the longest one's
/// edits apply to all of it).
pub fn join(notes: &[PitchNote], hop: u32) -> Option<PitchNote> {
    let first = notes.first()?;
    let longest = notes.iter().max_by_key(|n| n.end - n.start)?;
    let mut out = PitchNote {
        start: first.start,
        end: notes.last()?.end,
        pitch: first.pitch,
        shift: longest.shift,
        drift: longest.drift,
        formant: longest.formant,
        curve: Vec::new(),
    };
    let hop = i64::from(hop.max(1));
    for n in notes {
        // Absolute cents (from MIDI 0) to sit under one pitch.
        let at = ((n.start - out.start) / hop) as usize;
        out.curve.resize(at, UNVOICED);
        let d = ((n.pitch - out.pitch) * 100.0).round() as i32;
        out.curve.extend(n.curve.iter().map(|c| {
            if *c == UNVOICED {
                UNVOICED
            } else {
                (i32::from(*c) + d).clamp(i32::from(i16::MIN) + 1, i32::from(i16::MAX)) as i16
            }
        }));
    }
    if let Some(m) = median_cents(&out.curve) {
        out.pitch += m / 100.0;
        rebase(&mut out.curve, m / 100.0);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;
    const HOP: u32 = 240;

    /// A note of `frames` frames at `pitch` wandering ±`wobble` cents.
    fn note(start: i64, frames: i64, pitch: f32, wobble: i16) -> PitchNote {
        let n = (frames / i64::from(HOP)) as usize;
        PitchNote {
            start,
            end: start + frames,
            pitch,
            shift: 0.0,
            drift: 0.0,
            formant: 0.0,
            curve: (0..n)
                .map(|k| if k % 2 == 0 { wobble } else { -wobble })
                .collect(),
        }
    }

    #[test]
    fn a_note_moves_and_straightens() {
        let mut n = note(0, 48_000, 60.0, 30);
        assert_eq!(n.sung_at(0, HOP), Some(60.3));
        n.shift = 2.0;
        assert_eq!(n.correction_at(0, HOP), 2.0);
        assert_eq!(n.played_at(0, HOP), Some(62.3));
        n.drift = 1.0;
        assert!((n.played_at(0, HOP).unwrap() - 62.0).abs() < 1e-5);
        assert!((n.played_at(240, HOP).unwrap() - 62.0).abs() < 1e-5);
        n.drift = 0.5;
        assert!((n.played_at(0, HOP).unwrap() - 62.15).abs() < 1e-5);
        // Unvoiced: moved, not straightened.
        n.curve[10] = UNVOICED;
        n.curve[11] = UNVOICED;
        assert_eq!(n.correction_at(10 * 240, HOP), 2.0);
        assert_eq!(n.sung_at(10 * 240, HOP), None);
    }

    #[test]
    fn corrections_glide_between_notes() {
        let mut e = PitchEdit {
            hop: HOP,
            notes: vec![
                note(48_000, 24_000, 60.0, 0),
                // Touching the first.
                note(72_000, 24_000, 62.0, 0),
                // After a short gap, then a long one.
                note(100_000, 24_000, 64.0, 0),
                note(200_000, 24_000, 65.0, 0),
            ],
            keep_formants: true,
        };
        e.notes[0].shift = 1.0;
        e.notes[1].shift = -1.0;
        e.notes[2].shift = 3.0;
        e.notes[2].formant = 2.0;
        e.notes[3].shift = 1.0;
        let c = |at: i64| e.correction_at(at, RATE);
        // Outside any note and far from them: nothing.
        assert_eq!(c(0), (0.0, 0.0));
        assert_eq!(c(150_000), (0.0, 0.0));
        // Into the first note over the glide, then its own.
        assert!(c(47_000).0 > 0.0 && c(47_000).0 < 1.0);
        assert_eq!(c(60_000), (1.0, 0.0));
        // Across the touching boundary: halfway at it.
        assert!((c(72_000).0 - 0.0).abs() < 0.05, "{:?}", c(72_000));
        assert!(c(71_500).0 > 0.0 && c(72_500).0 < 0.0);
        assert_eq!(c(80_000), (-1.0, 0.0));
        // Across the short gap from −1 to +3 (formants 0 to 2).
        let mid = c(98_000);
        assert!(
            mid.0 > -1.0 && mid.0 < 3.0 && mid.1 > 0.0 && mid.1 < 2.0,
            "{mid:?}"
        );
        assert_eq!(c(110_000), (3.0, 2.0));
        // Out of the third into the long gap, back to nothing.
        assert!(c(124_500).0 > 0.0 && c(124_500).0 < 3.0);
        // Continuous everywhere: no step bigger than a glide allows.
        let mut last = c(0);
        for at in (0..230_000).step_by(48) {
            let now = c(at);
            assert!((now.0 - last.0).abs() < 0.2, "{at}: {last:?} → {now:?}");
            last = now;
        }
    }

    #[test]
    fn notes_snap_to_the_scale() {
        assert_eq!(snap(60.4, None), 60.0);
        assert_eq!(snap(60.6, None), 61.0);
        // A minor (root 9): A B C D E F G.
        let a_minor = Some((9, 0b0101_1010_1101));
        assert_eq!(snap(60.8, a_minor), 60.0, "a flat C# → C");
        assert_eq!(snap(61.2, a_minor), 62.0, "a sharp C# → D");
        assert_eq!(snap(65.8, a_minor), 65.0, "a flat F# → F");
        assert_eq!(snap(66.4, a_minor), 67.0, "a sharp F# → G");
        assert_eq!(snap(68.6, a_minor), 69.0, "A");
    }

    #[test]
    fn notes_split_and_join_keeping_their_curve() {
        // Sung low then high: one note around 61 spanning both.
        let mut n = note(0, 48_000, 61.0, 0);
        for (k, c) in n.curve.iter_mut().enumerate() {
            *c = if k < 100 { -100 } else { 100 };
        }
        n.shift = 0.5;
        let (a, b) = split(&n, 24_000, HOP).unwrap();
        assert_eq!(
            (a.start, a.end, b.start, b.end),
            (0, 24_000, 24_000, 48_000)
        );
        assert!((a.pitch - 60.0).abs() < 1e-4 && (b.pitch - 62.0).abs() < 1e-4);
        assert!(a.curve.iter().all(|c| *c == 0) && b.curve.iter().all(|c| *c == 0));
        assert_eq!((a.shift, b.shift), (0.5, 0.5));
        // The sung pitch is unchanged by splitting.
        assert_eq!(a.sung_at(1_000, HOP), n.sung_at(1_000, HOP));
        assert_eq!(b.sung_at(30_000, HOP), n.sung_at(30_000, HOP));
        let j = join(&[a, b], HOP).unwrap();
        assert_eq!((j.start, j.end), (0, 48_000));
        assert_eq!(j.curve.len(), n.curve.len());
        for at in [1_000, 30_000] {
            assert!((j.sung_at(at, HOP).unwrap() - n.sung_at(at, HOP).unwrap()).abs() < 0.011);
        }
        assert!(split(&n, 0, HOP).is_none() && split(&n, 48_000, HOP).is_none());
    }
}
