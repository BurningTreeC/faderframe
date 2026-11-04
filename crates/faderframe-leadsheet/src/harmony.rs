//! Chord symbols from the notes a recording plays (or MIDI): each beat's
//! pitch-class profile — every note weighted by how long it sounds in the
//! beat and how loud it is, the lowest notes also into a bass profile —
//! scored against chord templates, and a Viterbi path through the beats
//! that changes chord on a bar line cheaply, on its middle at a price and
//! on other beats only when the evidence is strong, so the harmonic rhythm
//! keeps to the bar or the half bar and nothing flickers.
//!
//! A template scores the share of the profile on its tones (the root and
//! third count fully, the fifth a little less) less the share off them and
//! a price for each tone missing; sevenths, suspensions, diminished and
//! augmented chords pay for being less common; chords of the key (its
//! scale's triads and sevenths, a minor key's major dominant too) are
//! preferred and chords with a root outside it less likely. Beats with
//! nothing sounding keep the chord before them. A chord whose bass stays
//! on another of its tones is written over that bass ("C/E").

use crate::build::Bar;
use crate::score::{DIVISIONS, bar_length};
use faderframe_midi::theory::{Chord, Key, Quality};

/// A note as harmonic evidence: when it sounds (quarters, the bars'
/// clock), its key and its weight (its level, 0…1 or so).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sounded {
    pub start: f64,
    pub end: f64,
    pub key: u8,
    pub weight: f32,
}

/// The qualities a recording's chords are named with.
const QUALITIES: [Quality; 10] = [
    Quality::Major,
    Quality::Minor,
    Quality::Dominant7,
    Quality::Major7,
    Quality::Minor7,
    Quality::Diminished,
    Quality::HalfDiminished7,
    Quality::Sus4,
    Quality::Sus2,
    Quality::Augmented,
];

/// What a change of chord costs: on a bar line, in the middle of a bar,
/// on another beat (in beats' worth of a clear chord's score).
const CHANGE_BAR: f64 = 0.12;
const CHANGE_HALF: f64 = 0.4;
const CHANGE_BEAT: f64 = 1.1;
/// A beat with less than this share of a usual beat's weight is silent.
const SILENT: f64 = 0.08;

/// A template's score for a profile `p` and bass profile `b` (each summing
/// to 1, or all zero), `key` the key in force.
fn score(root: u8, q: Quality, p: &[f64; 12], b: &[f64; 12], key: Option<Key>) -> f64 {
    let tones: Vec<(u8, u8)> = q
        .intervals()
        .iter()
        .map(|i| (*i % 12, (root + *i % 12) % 12))
        .collect();
    let on: f64 = tones.iter().map(|(_, pc)| p[usize::from(*pc)]).sum();
    let mut s = 0.0;
    for (interval, pc) in &tones {
        let have = p[usize::from(*pc)];
        let weight = match interval {
            0 => 1.0,
            3 | 4 => 1.0,
            7 => 0.8,
            _ => 0.9,
        };
        s += weight * have;
        // A tone missing: the root and the third (or what stands for it)
        // matter most, a seventh must be heard to be named.
        if have < 0.03 {
            s -= match interval {
                0 => 0.2,
                2..=5 => 0.15,
                7 => 0.03,
                _ => 0.25,
            };
        }
    }
    s -= 0.75 * (1.0 - on);
    s -= match q {
        Quality::Major | Quality::Minor => 0.0,
        Quality::Dominant7 | Quality::Minor7 | Quality::Major7 => 0.05,
        Quality::Sus4 | Quality::Sus2 => 0.1,
        Quality::HalfDiminished7 => 0.1,
        Quality::Diminished => 0.12,
        _ => 0.16,
    };
    // The bass on the root.
    s += 0.12 * b[usize::from(root)];
    if let Some(k) = key {
        let scale = k.scale.intervals();
        let in_key = |pc: u8| scale.contains(&((pc + 12 - k.root) % 12));
        let minor_dominant = k.scale.is_minor()
            && root == (k.root + 7) % 12
            && matches!(q, Quality::Major | Quality::Dominant7);
        if tones.iter().all(|(_, pc)| in_key(*pc)) || minor_dominant {
            s += 0.06;
        } else if !in_key(root) {
            s -= 0.06;
        }
    }
    s
}

/// Each beat of the bars: (start, end) in quarters and where it sits (0:
/// a bar's first beat, 1: its middle, 2: another).
fn beats(bars: &[Bar]) -> Vec<(f64, f64, u8)> {
    let mut out = Vec::new();
    for b in bars {
        let len = f64::from(bar_length(b.time)) / f64::from(DIVISIONS);
        let (num, den) = b.time;
        // Compound metres count dotted quarters; x/8 others eighths… but a
        // chord's beat is never shorter than a quarter.
        let beat = if den == 8 && num % 3 == 0 && num > 3 {
            1.5
        } else {
            (4.0 / f64::from(den.max(1))).max(1.0).min(len)
        };
        let n = ((len / beat).round() as usize).max(1);
        for i in 0..n {
            let s = b.start + beat * i as f64;
            let at = if i == 0 {
                0
            } else if n.is_multiple_of(2) && i == n / 2 {
                1
            } else {
                2
            };
            out.push((s, (s + beat).min(b.start + len), at));
        }
    }
    out
}

/// The chords `notes` make over `bars` (see the module docs).
pub fn chords(notes: &[Sounded], bars: &[Bar], key: Option<Key>) -> Vec<crate::build::ChordAt> {
    let beats = beats(bars);
    if beats.is_empty() || notes.is_empty() {
        return Vec::new();
    }
    // Each beat's profile and bass profile (unnormalised).
    let mut profiles: Vec<([f64; 12], [f64; 12], f64)> = Vec::with_capacity(beats.len());
    for &(a, b, _) in &beats {
        let mut p = [0.0f64; 12];
        let mut sounding: Vec<(u8, f64)> = Vec::new();
        for n in notes {
            let overlap = n.end.min(b) - n.start.max(a);
            if overlap <= 0.0 {
                continue;
            }
            let w = overlap * f64::from(n.weight.max(0.0));
            p[usize::from(n.key % 12)] += w;
            sounding.push((n.key, w));
        }
        let mut bass = [0.0f64; 12];
        if let Some(low) = sounding.iter().map(|(k, _)| *k).min() {
            // The lowest notes (within a fifth of the lowest).
            for (k, w) in &sounding {
                if *k < low + 7 {
                    bass[usize::from(k % 12)] += w;
                }
            }
        }
        let total: f64 = p.iter().sum();
        profiles.push((p, bass, total));
    }
    let mut weights: Vec<f64> = profiles
        .iter()
        .map(|(_, _, t)| *t)
        .filter(|t| *t > 0.0)
        .collect();
    weights.sort_by(f64::total_cmp);
    let usual = weights.get(weights.len() / 2).copied().unwrap_or(0.0);
    if usual <= 0.0 {
        return Vec::new();
    }
    let candidates: Vec<Chord> = (0..12u8)
        .flat_map(|r| QUALITIES.iter().map(move |q| Chord::new(r, *q)))
        .collect();
    // Each beat's scores, weighted by how much sounds in it (silent beats
    // score nothing: the chord before goes on).
    let emit: Vec<Vec<f64>> = profiles
        .iter()
        .map(|(p, b, total)| {
            let weight = (total / usual).min(1.5);
            if weight < SILENT {
                return vec![0.0; candidates.len()];
            }
            let norm = |v: &[f64; 12]| {
                let s: f64 = v.iter().sum();
                let mut out = [0.0; 12];
                if s > 0.0 {
                    for (o, x) in out.iter_mut().zip(v) {
                        *o = x / s;
                    }
                }
                out
            };
            let (p, b) = (norm(p), norm(b));
            candidates
                .iter()
                .map(|c| weight * score(c.root, c.quality, &p, &b, key))
                .collect()
        })
        .collect();
    // Viterbi (maximising), changes priced by where they fall.
    let n = candidates.len();
    let mut best: Vec<f64> = emit[0].clone();
    let mut from: Vec<Vec<usize>> = vec![(0..n).collect()];
    for k in 1..beats.len() {
        let price = match beats[k].2 {
            0 => CHANGE_BAR,
            1 => CHANGE_HALF,
            _ => CHANGE_BEAT,
        };
        let (top, top_at) = best
            .iter()
            .enumerate()
            .fold((f64::NEG_INFINITY, 0), |acc, (i, v)| {
                if *v > acc.0 { (*v, i) } else { acc }
            });
        let mut next = vec![0.0; n];
        let mut back = vec![0usize; n];
        for c in 0..n {
            let (v, f) = if best[c] >= top - price {
                (best[c], c)
            } else {
                (top - price, top_at)
            };
            next[c] = v + emit[k][c];
            back[c] = f;
        }
        best = next;
        from.push(back);
    }
    let mut path = vec![0usize; beats.len()];
    let mut c = best
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(i, _)| i);
    for k in (0..beats.len()).rev() {
        path[k] = c;
        c = from[k][c];
    }
    // Runs of one chord; silent beats before anything sounded write
    // nothing.
    let first = profiles
        .iter()
        .position(|(_, _, t)| t / usual >= SILENT)
        .unwrap_or(0);
    let mut out: Vec<crate::build::ChordAt> = Vec::new();
    let mut k = first;
    while k < beats.len() {
        let c = path[k];
        let mut j = k;
        while j < beats.len() && path[j] == c {
            j += 1;
        }
        // The bass over the run: another chord tone held there.
        let mut bass = [0.0f64; 12];
        for (_, b, _) in &profiles[k..j] {
            for (o, x) in bass.iter_mut().zip(b) {
                *o += x;
            }
        }
        let total: f64 = bass.iter().sum();
        let mut chord = candidates[c];
        if total > 0.0 {
            let (pc, w) = bass
                .iter()
                .enumerate()
                .fold(
                    (0usize, 0.0f64),
                    |acc, (i, w)| {
                        if *w > acc.1 { (i, *w) } else { acc }
                    },
                );
            let pc = pc as u8;
            if pc != chord.root && w / total >= 0.7 && chord.contains(i32::from(pc)) {
                chord = chord.with_bass(Some(pc));
            }
        }
        out.push(crate::build::ChordAt {
            start: beats[k].0,
            chord,
        });
        k = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_midi::theory::Scale;

    fn bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| Bar {
                start: 4.0 * i as f64,
                time: (4, 4),
            })
            .collect()
    }

    fn block(at: f64, len: f64, keys: &[u8]) -> Vec<Sounded> {
        keys.iter()
            .map(|k| Sounded {
                start: at,
                end: at + len,
                key: *k,
                weight: 0.6,
            })
            .collect()
    }

    fn names(c: &[crate::build::ChordAt]) -> Vec<(f64, String)> {
        c.iter().map(|c| (c.start, c.chord.name(false))).collect()
    }

    #[test]
    fn block_chords_are_named_a_bar_each() {
        let mut notes = Vec::new();
        for (i, keys) in [
            &[48u8, 52, 55][..],
            &[45, 48, 52],
            &[41, 45, 48],
            &[43, 47, 50, 53],
        ]
        .iter()
        .enumerate()
        {
            notes.extend(block(4.0 * i as f64, 4.0, keys));
        }
        let c = chords(&notes, &bars(4), Some(Key::new(0, Scale::Major)));
        assert_eq!(
            names(&c),
            [
                (0.0, "C".into()),
                (4.0, "Am".into()),
                (8.0, "F".into()),
                (12.0, "G7".into())
            ]
        );
    }

    #[test]
    fn passing_notes_do_not_flicker_and_halves_change() {
        // C with a melody passing through D and F; then F for half a bar,
        // G for the other half.
        let mut notes = block(0.0, 4.0, &[48, 52, 55]);
        for (i, k) in [72u8, 74, 76, 77].iter().enumerate() {
            notes.extend(block(i as f64, 1.0, &[*k]));
        }
        notes.extend(block(4.0, 2.0, &[41, 45, 48]));
        notes.extend(block(6.0, 2.0, &[43, 47, 50]));
        let c = chords(&notes, &bars(2), None);
        assert_eq!(
            names(&c),
            [(0.0, "C".into()), (4.0, "F".into()), (6.0, "G".into())]
        );
    }

    #[test]
    fn an_arpeggio_is_one_chord_and_silence_keeps_it() {
        let mut notes = Vec::new();
        for (j, k) in [45u8, 52, 57, 60, 64, 60, 57, 52].iter().enumerate() {
            notes.extend(block(0.5 * j as f64, 0.75, &[*k]));
        }
        let c = chords(&notes, &bars(2), None);
        assert_eq!(names(&c), [(0.0, "Am".into())]);
    }

    #[test]
    fn a_bass_on_the_third_is_written_under_the_chord() {
        let mut notes = block(0.0, 4.0, &[40]);
        notes.extend(block(0.0, 4.0, &[60, 64, 67]));
        let c = chords(&notes, &bars(1), Some(Key::new(0, Scale::Major)));
        assert_eq!(names(&c), [(0.0, "C/E".into())]);
    }
}
