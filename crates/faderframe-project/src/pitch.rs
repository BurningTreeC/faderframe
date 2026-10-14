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
//!
//! Notes are made longer or shorter by moving their edges
//! ([`PitchEdit::move_edge`]): a note's `timing` is where its start and
//! end play now, its audio stretched in between. In a melodic edit the
//! neighbour on that side gives or takes the time (the next note when it
//! follows without a pause, else the pause before it), so everything else
//! stays where it was, and the clip plays through [`PitchEdit::time_map`];
//! in a polyphonic one each note is stretched on its own, in the render.
//! Removed notes ([`PitchEdit::removed`]) leave the editor and take their
//! sound with them.

use crate::WarpMarker;
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
/// A removed note of a melodic edit fades out and in over this long
/// (seconds), centred on its edges.
const FADE_SECONDS: f64 = 0.005;
/// The shortest a note is made (hops), and the shortest a pause (or the
/// audio before the first note or after the last) is squeezed to.
pub const MIN_NOTE_HOPS: i64 = 4;
pub const MIN_GAP_HOPS: i64 = 1;
/// Notes this close (hops: 100 ms at the detectors' 5 ms) follow each
/// other without a pause: the transition between them (unvoiced to the
/// tracker) moves along when either is made longer or shorter.
pub const JOIN_HOPS: i64 = 20;

fn yes() -> bool {
    true
}

/// A clip's pitch edit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PitchEdit {
    /// Source frames between curve values.
    pub hop: u32,
    /// Sorted by start; not overlapping, but in a polyphonic edit.
    pub notes: Vec<PitchNote>,
    /// Keep the formants where the pitch moves (a voice keeps its size;
    /// off, they move with the pitch).
    #[serde(default = "yes")]
    pub keep_formants: bool,
    /// Polyphonic (`None`: melodic, played live by PSOLA).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polyphonic: Option<Polyphonic>,
    /// Notes taken out: no longer shown, their sound gone (melodic: the
    /// audio silenced, fading over 5 ms; polyphonic: the note's partials,
    /// as muting takes them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<PitchNote>,
}

/// One edge of a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// A polyphonic edit: its notes may sound together and are moved in the
/// spectrum, each on its own (`faderframe_polypitch`). The clip plays a
/// render of `original` made from the notes as [`PitchEdit::sound_key`]
/// named them then (`rendered`; 0: nothing moved, the clip plays
/// `original` itself). Undo and redo carry the source and the key along
/// with the notes, so they always agree; a session renders again where
/// they do not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Polyphonic {
    pub original: faderframe_core::AudioSourceId,
    #[serde(default)]
    pub rendered: u64,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
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
    /// Its level change (dB; polyphonic edits).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gain_db: f32,
    /// Taken out (polyphonic edits).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
    /// Where its start and end play now that it was made longer or
    /// shorter, in the frames of the timing as sung (the frames that
    /// played there before); `None`: where it was sung.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<(i64, i64)>,
}

impl PitchNote {
    /// Has it been edited?
    pub fn edited(&self) -> bool {
        self.shift != 0.0
            || self.drift != 0.0
            || self.formant != 0.0
            || self.gain_db != 0.0
            || self.muted
            || self.timed()
    }

    /// Its start and end as it plays now (see `timing`).
    pub fn placed(&self) -> (i64, i64) {
        self.timing.unwrap_or((self.start, self.end))
    }

    /// Made longer or shorter?
    pub fn timed(&self) -> bool {
        self.timing.is_some_and(|t| t != (self.start, self.end))
    }

    /// Play it from `start` to `end` (placed frames).
    pub fn set_placed(&mut self, start: i64, end: i64) {
        self.timing = ((start, end) != (self.start, self.end)).then_some((start, end));
    }

    /// Where source frame `at` of it plays (placed frames; stretched
    /// evenly inside it).
    pub fn place_of(&self, at: f64) -> f64 {
        let (a, b) = self.placed();
        let len = (self.end - self.start).max(1) as f64;
        a as f64 + (at - self.start as f64) * (b - a) as f64 / len
    }

    /// The source frame of it playing at placed frame `at`.
    pub fn source_of(&self, at: f64) -> f64 {
        let (a, b) = self.placed();
        let len = (b - a).max(1) as f64;
        self.start as f64 + (at - a as f64) * (self.end - self.start) as f64 / len
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
        self.notes.iter().any(PitchNote::edited) || !self.removed.is_empty()
    }

    /// Played live by the engine (PSOLA): a melodic edit with something
    /// moved. A polyphonic one plays its render.
    pub fn plays_live(&self) -> bool {
        self.polyphonic.is_none() && self.edited()
    }

    /// What the notes do to the sound, as a key: equal keys, equal renders
    /// (0: nothing changes).
    pub fn sound_key(&self) -> u64 {
        if !self.edited() {
            return 0;
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |v: u64| {
            for b in v.to_le_bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        eat(u64::from(self.hop));
        // (Timing and removed notes only where there are any: the keys of
        // edits made before them stay as they were.)
        let notes = self.notes.iter().map(|n| (n, false));
        for (n, gone) in notes.chain(self.removed.iter().map(|n| (n, true))) {
            eat(n.start as u64);
            eat(n.end as u64);
            eat(u64::from(n.pitch.to_bits()));
            eat(u64::from(n.shift.to_bits()));
            eat(u64::from(n.drift.to_bits()));
            eat(u64::from(n.gain_db.to_bits()));
            eat(u64::from(n.muted));
            for c in &n.curve {
                eat(*c as u64);
            }
            if n.timed() {
                let (a, b) = n.placed();
                eat(a as u64);
                eat(b as u64);
            }
            if gone {
                eat(0x5e_1ec7);
            }
        }
        h.max(1)
    }

    /// The level at source frame `at` of a melodic edit at `rate` (0…1):
    /// silent inside removed notes, fading around their edges. (A
    /// polyphonic edit takes them out in its render.)
    pub fn level_at(&self, at: i64, rate: f64) -> f32 {
        let fade = (FADE_SECONDS * rate).max(1.0);
        let at = at as f64;
        self.removed.iter().fold(1.0f32, |g, n| {
            // How far inside the note widened by half a fade either side.
            let into = (at - n.start as f64).min(n.end as f64 - at) + fade / 2.0;
            g.min((1.0 - into / fade).clamp(0.0, 1.0) as f32)
        })
    }

    /// A melodic edit's time map over the source frames `lo..hi` (the
    /// clip's): (placed frame, source frame) at the clip's ends and every
    /// note's edges, strictly increasing in both — empty when no note was
    /// made longer or shorter. Between anchors the audio is stretched
    /// evenly.
    pub fn time_map(&self, lo: i64, hi: i64) -> Vec<(i64, i64)> {
        if self.polyphonic.is_some() || !self.notes.iter().any(PitchNote::timed) || hi <= lo {
            return Vec::new();
        }
        let mut out = vec![(lo, lo)];
        for n in &self.notes {
            let (a, b) = n.placed();
            for (v, s) in [(a, n.start), (b, n.end)] {
                let last = out[out.len() - 1];
                if v > last.0 && s > last.1 && v < hi && s < hi {
                    out.push((v, s));
                }
            }
        }
        out.push((hi, hi));
        out
    }

    /// Do notes `i` and `i + 1` follow each other without a pause (the
    /// gap between them, if any, a transition of [`JOIN_HOPS`] at most)?
    fn joined(&self, i: usize) -> bool {
        let hop = i64::from(self.hop.max(1));
        match (self.notes.get(i), self.notes.get(i + 1)) {
            (Some(a), Some(b)) => b.start - a.end <= JOIN_HOPS * hop,
            _ => false,
        }
    }

    /// Move the `edge` of note `i` to placed frame `to`, inside the clip's
    /// source frames `lo..hi`: the note is stretched between its edges.
    /// Melodic: the neighbour on that side gives or takes the time — the
    /// next (previous) note when one follows without a pause (the
    /// transition between them moving along), else the pause, or the audio
    /// up to the clip's edge — so nothing else moves; notes keep
    /// [`MIN_NOTE_HOPS`], pauses [`MIN_GAP_HOPS`]. Polyphonic: only the
    /// note itself changes.
    pub fn move_edge(&mut self, i: usize, edge: Edge, to: i64, lo: i64, hi: i64) {
        let hop = i64::from(self.hop.max(1));
        let (min_note, min_gap) = (MIN_NOTE_HOPS * hop, MIN_GAP_HOPS * hop);
        let melodic = self.polyphonic.is_none();
        let Some(n) = self.notes.get(i) else {
            return;
        };
        let (a, b) = n.placed();
        let gap = if melodic { min_gap } else { 0 };
        match edge {
            Edge::End => {
                let mut top = hi - gap;
                // The next note following on: (it, the transition's length).
                let mut follows = None;
                if let Some(next) = self.notes.get(i + 1).filter(|_| melodic) {
                    let (na, nb) = next.placed();
                    if self.joined(i) {
                        follows = Some((i + 1, na - b));
                        top = nb - min_note - (na - b);
                    } else {
                        top = top.min(na - min_gap);
                    }
                }
                let to = to.clamp((a + min_note).min(b), top.max(b));
                self.notes[i].set_placed(a, to);
                if let Some((k, lead)) = follows {
                    let nb = self.notes[k].placed().1;
                    self.notes[k].set_placed(to + lead, nb);
                }
            }
            Edge::Start => {
                let mut bottom = lo + gap;
                let mut follows = None;
                let prev = i.checked_sub(1).and_then(|k| self.notes.get(k));
                if let Some(prev) = prev.filter(|_| melodic) {
                    let (pa, pb) = prev.placed();
                    if self.joined(i - 1) {
                        follows = Some((i - 1, a - pb));
                        bottom = pa + min_note + (a - pb);
                    } else {
                        bottom = bottom.max(pb + min_gap);
                    }
                }
                let to = to.clamp(bottom.min(a), (b - min_note).max(a));
                self.notes[i].set_placed(to, b);
                if let Some((k, lead)) = follows {
                    let pa = self.notes[k].placed().0;
                    self.notes[k].set_placed(pa, to - lead);
                }
            }
        }
    }

    /// Note `i` back to its timing as sung; in a melodic edit a neighbour
    /// following on without a pause gives back the edge next to it.
    pub fn reset_timing(&mut self, i: usize) {
        let Some(n) = self.notes.get_mut(i) else {
            return;
        };
        n.timing = None;
        if self.polyphonic.is_some() {
            return;
        }
        if i > 0 && self.joined(i - 1) {
            let prev = &mut self.notes[i - 1];
            let pa = prev.placed().0;
            prev.set_placed(pa, prev.end.max(pa + 1));
        }
        if self.joined(i) {
            let next = &mut self.notes[i + 1];
            let nb = next.placed().1;
            next.set_placed(next.start.min(nb - 1), nb);
        }
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

/// A clip's anchors (`points`: output frame → source frame, as
/// [`crate::Warp::points`] gives them) played through a pitch edit's
/// `map` ([`PitchEdit::time_map`]: placed frame → source frame): the
/// clip's time map with its notes' timing, as (output, source) frames.
pub fn through(points: &[WarpMarker], map: &[(i64, i64)]) -> Vec<(f64, f64)> {
    if map.len() < 2 || points.len() < 2 {
        return points
            .iter()
            .map(|p| (p.at as f64, p.source as f64))
            .collect();
    }
    // Placed → source (the map), and placed → output (the clip's anchors
    // read the other way).
    let source_of = |v: f64| {
        let i = map
            .partition_point(|m| (m.0 as f64) <= v)
            .saturating_sub(1)
            .min(map.len() - 2);
        let (a, b) = (map[i], map[i + 1]);
        a.1 as f64 + (v - a.0 as f64) * (b.1 - a.1) as f64 / (b.0 - a.0).max(1) as f64
    };
    let output_of = |v: f64| {
        let i = points
            .partition_point(|p| (p.source as f64) <= v)
            .saturating_sub(1)
            .min(points.len() - 2);
        let (a, b) = (points[i], points[i + 1]);
        a.at as f64
            + (v - a.source as f64) * (b.at - a.at) as f64 / (b.source - a.source).max(1) as f64
    };
    let (first, last) = (points[0].source, points[points.len() - 1].source);
    let mut out: Vec<(f64, f64)> = points
        .iter()
        .map(|p| (p.at as f64, source_of(p.source as f64)))
        .chain(
            map.iter()
                .filter(|m| m.0 > first && m.0 < last)
                .map(|m| (output_of(m.0 as f64), m.1 as f64)),
        )
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut kept: Vec<(f64, f64)> = Vec::with_capacity(out.len());
    for p in out {
        if kept.last().is_none_or(|l| p.0 > l.0 && p.1 > l.1) {
            kept.push(p);
        }
    }
    kept
}

/// Carry the corrections of `old` (an edit of the other kind: melodic for
/// polyphonic or the other way round) over to the notes of `new` found in
/// the same place. A new note takes them from the old note it overlaps
/// most (by at least half its length) at the same pitch (within half a
/// semitone): the move and the straightening; the formants into a
/// melodic edit, level and mute into a polyphonic one (a muted note is
/// removed from a melodic edit). One lying in a removed note is removed.
/// Lengths are not carried: the notes play as sung.
pub fn carry(old: &PitchEdit, new: &mut PitchEdit) {
    let melodic = new.polyphonic.is_none();
    new.keep_formants = old.keep_formants;
    let olds: Vec<(&PitchNote, bool)> = old
        .notes
        .iter()
        .map(|n| (n, false))
        .chain(old.removed.iter().map(|n| (n, true)))
        .collect();
    let mut kept = Vec::with_capacity(new.notes.len());
    for mut n in std::mem::take(&mut new.notes) {
        let len = (n.end - n.start).max(1);
        let best = olds
            .iter()
            .filter(|(o, _)| (o.pitch - n.pitch).abs() <= 0.5)
            .map(|(o, gone)| (*o, *gone, o.end.min(n.end) - o.start.max(n.start)))
            .filter(|(_, _, overlap)| 2 * overlap >= len)
            .max_by_key(|(_, _, overlap)| *overlap);
        let Some((o, gone, _)) = best else {
            kept.push(n);
            continue;
        };
        if gone || (melodic && o.muted) {
            new.removed.push(n);
            continue;
        }
        n.shift = o.shift;
        n.drift = o.drift;
        if melodic {
            n.formant = o.formant;
        } else {
            n.gain_db = o.gain_db;
            n.muted = o.muted;
        }
        kept.push(n);
    }
    new.notes = kept;
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
    // Where the cut plays (a stretched note's halves stay stretched).
    let (from, to) = note.placed();
    let cut = (note.place_of(at as f64).round() as i64).clamp(from + 1, (to - 1).max(from + 1));
    a.end = at;
    a.curve.truncate(k);
    a.set_placed(from, cut);
    b.start = at;
    b.curve = note.curve[k..].to_vec();
    b.set_placed(cut, to);
    for n in [&mut a, &mut b] {
        if let Some(m) = median_cents(&n.curve) {
            n.pitch += m / 100.0;
            rebase(&mut n.curve, m / 100.0);
        }
    }
    Some((a, b))
}

/// Join neighbouring notes into one (the gap unvoiced; the longest one's
/// edits apply to all of it; it plays from where the first starts to where
/// the last ends).
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
        gain_db: longest.gain_db,
        muted: longest.muted,
        timing: None,
    };
    out.set_placed(first.placed().0, notes.last()?.placed().1);
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
            gain_db: 0.0,
            muted: false,
            timing: None,
        }
    }

    fn melodic(notes: Vec<PitchNote>) -> PitchEdit {
        PitchEdit {
            hop: HOP,
            notes,
            keep_formants: true,
            polyphonic: None,
            removed: Vec::new(),
        }
    }

    #[test]
    fn a_longer_note_takes_its_time_from_its_neighbour() {
        // Two notes sung on without a pause (25 ms of transition between
        // them), a pause, a third; the clip 0..140 000.
        let mut e = melodic(vec![
            note(0, 48_000, 57.0, 0),
            note(49_200, 46_800, 60.0, 0),
            note(110_000, 24_000, 64.0, 0),
        ]);
        let (lo, hi) = (0, 140_000);
        assert!(e.time_map(lo, hi).is_empty(), "nothing made longer");
        // The first longer: the second starts later (the transition moving
        // along), ends where it did.
        e.move_edge(0, Edge::End, 60_000, lo, hi);
        assert_eq!(e.notes[0].placed(), (0, 60_000));
        assert_eq!(e.notes[1].placed(), (61_200, 96_000));
        assert!(e.edited());
        assert_eq!(
            e.time_map(lo, hi),
            [
                (0, 0),
                (60_000, 48_000),
                (61_200, 49_200),
                (96_000, 96_000),
                (110_000, 110_000),
                (134_000, 134_000),
                (140_000, 140_000)
            ]
        );
        // Not through the second: it keeps its shortest.
        let min = MIN_NOTE_HOPS * i64::from(HOP);
        e.move_edge(0, Edge::End, 200_000, lo, hi);
        assert_eq!(e.notes[0].placed().1, 96_000 - min - 1_200);
        // The second into the pause, up to the shortest pause.
        e.move_edge(1, Edge::End, 105_000, lo, hi);
        assert_eq!(e.notes[1].placed().1, 105_000);
        assert_eq!(e.notes[2].placed(), (110_000, 134_000), "after a pause");
        e.move_edge(2, Edge::Start, 90_000, lo, hi);
        assert_eq!(e.notes[2].placed().0, 105_000 + i64::from(HOP));
        // The last one up to just before the clip's end.
        e.move_edge(2, Edge::End, 200_000, lo, hi);
        assert_eq!(e.notes[2].placed().1, hi - i64::from(HOP));
        // Back as sung: the neighbour gives back the edge next to it.
        e.move_edge(0, Edge::End, 40_000, lo, hi);
        assert_eq!(e.notes[1].placed().0, 41_200);
        e.reset_timing(0);
        assert_eq!(e.notes[0].timing, None);
        assert_eq!(e.notes[1].placed(), (49_200, 105_000));
        // Polyphonic: each note on its own.
        e.polyphonic = Some(Polyphonic {
            original: faderframe_core::AudioSourceId(1),
            rendered: 0,
        });
        e.notes[1].timing = None;
        e.move_edge(0, Edge::End, 70_000, lo, hi);
        assert_eq!(e.notes[0].placed(), (0, 70_000));
        assert_eq!(e.notes[1].placed(), (49_200, 96_000));
        assert!(e.time_map(lo, hi).is_empty(), "rendered, not mapped");
    }

    #[test]
    fn the_notes_timing_plays_through_the_clips_warp() {
        // A clip twice as long as its 1000 source frames, and a time map
        // pulling source 500 to 250.
        let w = crate::Warp::uniform(1_000);
        let points = w.points(0, 2_000);
        let map = [(0, 0), (250, 500), (1_000, 1_000)];
        let out = through(&points, &map);
        assert_eq!(out, [(0.0, 0.0), (500.0, 500.0), (2_000.0, 1_000.0)]);
        // Nothing mapped: the clip's own anchors.
        assert_eq!(through(&points, &[]), [(0.0, 0.0), (2_000.0, 1_000.0)]);
    }

    #[test]
    fn a_removed_note_is_silent_and_fades() {
        let mut e = melodic(vec![note(0, 48_000, 57.0, 0)]);
        e.removed.push(note(48_000, 48_000, 60.0, 0));
        assert!(e.edited());
        let fade = (FADE_SECONDS * RATE) as i64;
        assert_eq!(e.level_at(20_000, RATE), 1.0);
        assert_eq!(e.level_at(48_000 - fade, RATE), 1.0);
        assert_eq!(e.level_at(48_000, RATE), 0.5);
        assert_eq!(e.level_at(48_000 + fade, RATE), 0.0);
        assert_eq!(e.level_at(70_000, RATE), 0.0);
        assert_eq!(e.level_at(96_000, RATE), 0.5);
        assert_eq!(e.level_at(96_000 + fade, RATE), 1.0);
        // It changes the sound.
        let mut plain = e.clone();
        plain.removed.clear();
        assert_ne!(e.sound_key(), plain.sound_key());
    }

    #[test]
    fn corrections_carry_over_to_the_notes_found_the_other_way() {
        let mut old = melodic(vec![
            note(0, 48_000, 60.0, 0),
            note(48_000, 48_000, 62.0, 0),
            note(96_000, 48_000, 64.0, 0),
        ]);
        old.notes[0].shift = 2.0;
        old.notes[0].drift = 0.5;
        old.notes[0].formant = 1.0;
        old.notes[1].shift = -1.0;
        old.removed.push(old.notes.remove(2));
        let mut new = PitchEdit {
            polyphonic: Some(Polyphonic {
                original: faderframe_core::AudioSourceId(1),
                rendered: 0,
            }),
            ..melodic(vec![
                // Found a little later and sharp: the same note.
                note(2_400, 45_000, 60.2, 0),
                // Another pitch at the same place: left alone.
                note(0, 48_000, 67.0, 0),
                // Mostly not where the old one was.
                note(80_000, 40_000, 62.0, 0),
                note(97_000, 40_000, 64.0, 0),
            ])
        };
        carry(&old, &mut new);
        let shifts: Vec<f32> = new.notes.iter().map(|n| n.shift).collect();
        assert_eq!(shifts, [2.0, 0.0, 0.0]);
        assert_eq!(new.notes[0].drift, 0.5);
        assert_eq!(new.notes[0].formant, 0.0, "formants stay melodic");
        assert_eq!(new.removed.len(), 1);
        assert_eq!(new.removed[0].pitch, 64.0);
    }

    #[test]
    fn a_stretched_note_splits_into_stretched_halves() {
        let mut n = note(0, 48_000, 57.0, 0);
        n.set_placed(0, 96_000);
        let (a, b) = split(&n, 24_000, HOP).unwrap();
        assert_eq!((a.placed(), b.placed()), ((0, 48_000), (48_000, 96_000)));
        assert_eq!(b.source_of(72_000.0), 36_000.0);
        assert_eq!(b.place_of(36_000.0), 72_000.0);
        let j = join(&[a, b], HOP).unwrap();
        assert_eq!(j.placed(), (0, 96_000));
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
            polyphonic: None,
            removed: Vec::new(),
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
