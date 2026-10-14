//! What a tracker heard against what was sung or played: right notes (and
//! how soon), late ones, octave (harmonic) errors, wrong ones, missed
//! ones, octave doublings merged, notes heard in silence or breaths,
//! stray notes within notes, and notes played again (vibrato, beating).

use crate::signals::{Note, RATE};
use faderframe_analysis::voice::VoiceEvent;

/// A note the tracker played: key, when it was decided (played live),
/// where it was placed (recorded), its end (s) and velocity.
#[derive(Clone, Copy, Debug)]
pub struct Heard {
    pub key: u8,
    pub decided: f64,
    pub at: f64,
    pub end: f64,
    pub velocity: u8,
}

/// The notes of a tracker's events (absolute frames: decided, placed).
pub fn heard(events: &[(i64, i64, VoiceEvent)], frames: usize) -> Vec<Heard> {
    let mut open: Vec<(u8, i64, i64, u8)> = Vec::new();
    let mut out = Vec::new();
    let close = |o: (u8, i64, i64, u8), end: i64, out: &mut Vec<Heard>| {
        out.push(Heard {
            key: o.0,
            decided: o.1 as f64 / RATE,
            at: o.2 as f64 / RATE,
            end: end.max(o.2) as f64 / RATE,
            velocity: o.3,
        });
    };
    for &(now, at, e) in events {
        match e {
            VoiceEvent::NoteOn { key, velocity } => {
                if let Some(i) = open.iter().position(|o| o.0 == key) {
                    let o = open.remove(i);
                    close(o, at, &mut out);
                }
                open.push((key, now, at, velocity));
            }
            VoiceEvent::NoteOff { key } => {
                if let Some(i) = open.iter().position(|o| o.0 == key) {
                    let o = open.remove(i);
                    close(o, at, &mut out);
                }
            }
            VoiceEvent::Bend { .. } => {}
        }
    }
    for o in open {
        close(o, frames as i64, &mut out);
    }
    out.sort_by(|a, b| a.at.total_cmp(&b.at));
    out
}

/// Counts over notes.
#[derive(Clone, Debug, Default)]
pub struct Tally {
    /// Notes sung or played.
    pub notes: u32,
    /// Heard at the right key, placed within the tolerance.
    pub correct: u32,
    /// The right key, placed later (or earlier) than that.
    pub late: u32,
    /// Only an octave (or twelfth) away.
    pub octave: u32,
    /// Only another key.
    pub wrong: u32,
    /// Nothing heard.
    pub missed: u32,
    /// Missed, but an octave doubling sounding with it was heard.
    pub merged: u32,
    /// Notes played.
    pub played: u32,
    /// Played where nothing sounded (silence, breaths, consonants, hum).
    pub in_silence: u32,
    /// Stray notes within notes (another key: a scoop, a fall, a
    /// harmonic).
    pub stray: u32,
    /// The right key played again within one note (vibrato, beating).
    pub again: u32,
    /// How long after each right note began it was decided (ms).
    pub latency: Vec<f64>,
    /// The velocities of the right notes.
    pub velocity: Vec<u8>,
    /// How far from where each right note ended it was ended (ms).
    pub ends: Vec<f64>,
}

impl Tally {
    pub fn add(&mut self, o: &Tally) {
        self.notes += o.notes;
        self.correct += o.correct;
        self.late += o.late;
        self.octave += o.octave;
        self.wrong += o.wrong;
        self.missed += o.missed;
        self.merged += o.merged;
        self.played += o.played;
        self.in_silence += o.in_silence;
        self.stray += o.stray;
        self.again += o.again;
        self.latency.extend_from_slice(&o.latency);
        self.velocity.extend_from_slice(&o.velocity);
        self.ends.extend_from_slice(&o.ends);
    }

    pub fn precision(&self) -> f64 {
        f64::from(self.correct) / f64::from(self.played.max(1))
    }

    pub fn recall(&self) -> f64 {
        f64::from(self.correct) / f64::from(self.notes.max(1))
    }

    /// The F-measure of right notes.
    pub fn f1(&self) -> f64 {
        2.0 * f64::from(self.correct) / f64::from((self.notes + self.played).max(1))
    }

    fn percentile(v: &[f64], p: f64) -> f64 {
        if v.is_empty() {
            return f64::NAN;
        }
        let mut s = v.to_vec();
        s.sort_by(f64::total_cmp);
        s[((s.len() - 1) as f64 * p).round() as usize]
    }

    pub fn latency_ms(&self, p: f64) -> f64 {
        Self::percentile(&self.latency, p)
    }

    pub fn velocity_median(&self) -> f64 {
        let v: Vec<f64> = self.velocity.iter().map(|v| f64::from(*v)).collect();
        Self::percentile(&v, 0.5)
    }

    pub const HEADER: &str = "                                   notes   ok late  oct wrong miss mrgd | played silence stray again |  prec  rec   F1 | lat50 lat90 end50 vel";

    pub fn row(&self, label: &str) -> String {
        format!(
            "{label:<34} {:>5} {:>4} {:>4} {:>4} {:>5} {:>4} {:>4} | {:>6} {:>7} {:>5} {:>5} | {:>4.0}% {:>3.0}% {:>3.0}% | {:>5.0} {:>5.0} {:>5.0} {:>3.0}",
            self.notes,
            self.correct,
            self.late,
            self.octave,
            self.wrong,
            self.missed,
            self.merged,
            self.played,
            self.in_silence,
            self.stray,
            self.again,
            100.0 * self.precision(),
            100.0 * self.recall(),
            100.0 * self.f1(),
            self.latency_ms(0.5),
            self.latency_ms(0.9),
            Self::percentile(&self.ends, 0.5),
            self.velocity_median(),
        )
    }
}

/// Octave and twelfth relations (a harmonic or subharmonic heard).
fn harmonic(a: u8, b: u8) -> bool {
    matches!(a.abs_diff(b), 12 | 19 | 24)
}

/// Score `heard` against `truth`: a played note belongs to the note it
/// overlaps (the same key first, then a harmonic relation, then the most
/// overlap); `tolerance` (s) is how far from a note's start it may be
/// placed to count as right.
pub fn score(truth: &[Note], heard: &[Heard], tolerance: f64) -> Tally {
    let mut t = Tally {
        notes: truth.len() as u32,
        played: heard.len() as u32,
        ..Tally::default()
    };
    let mut owner: Vec<Option<usize>> = vec![None; heard.len()];
    for (j, h) in heard.iter().enumerate() {
        let mut best: Option<(usize, f64)> = None;
        for (i, n) in truth.iter().enumerate() {
            let overlap = h.end.min(n.off) - h.at.max(n.on - tolerance);
            if overlap <= 0.0 && !(h.at >= n.on - tolerance && h.at < n.off) {
                continue;
            }
            let rank = if h.key == n.key {
                // The nearest start among notes of its key.
                3e3 - (h.at - n.on).abs()
            } else if harmonic(h.key, n.key) {
                2e3 + overlap
            } else {
                1e3 + overlap
            };
            if best.is_none_or(|b| rank > b.1) {
                best = Some((i, rank));
            }
        }
        owner[j] = best.map(|b| b.0);
    }
    let mut right = vec![false; truth.len()];
    for (i, n) in truth.iter().enumerate() {
        let mine: Vec<&Heard> = heard
            .iter()
            .zip(&owner)
            .filter(|(_, o)| **o == Some(i))
            .map(|(h, _)| h)
            .collect();
        let same: Vec<&&Heard> = mine.iter().filter(|h| h.key == n.key).collect();
        let others = mine.len() - same.len();
        if let Some(first) = same.first() {
            if (first.at - n.on).abs() <= tolerance {
                t.correct += 1;
                right[i] = true;
                t.latency.push((first.decided - n.on) * 1000.0);
                t.velocity.push(first.velocity);
                t.ends.push((first.end - n.off).abs() * 1000.0);
            } else {
                t.late += 1;
            }
            t.again += same.len() as u32 - 1;
            t.stray += others as u32;
        } else if mine.iter().any(|h| harmonic(h.key, n.key)) {
            t.octave += 1;
            t.stray += others as u32 - 1;
        } else if others > 0 {
            t.wrong += 1;
            t.stray += others as u32 - 1;
        } else {
            t.missed += 1;
        }
    }
    // Missed doublings: a note an octave away struck with it was heard.
    for (i, n) in truth.iter().enumerate() {
        let missed = !right[i]
            && !heard
                .iter()
                .zip(&owner)
                .any(|(h, o)| *o == Some(i) && h.key == n.key);
        if missed
            && truth.iter().enumerate().any(|(k, m)| {
                right[k] && matches!(m.key.abs_diff(n.key), 12 | 24) && (m.on - n.on).abs() < 0.1
            })
        {
            t.merged += 1;
        }
    }
    t.in_silence = owner.iter().filter(|o| o.is_none()).count() as u32;
    t
}
