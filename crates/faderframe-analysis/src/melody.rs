//! Melody analysis for pitch editing: the pitch a monophonic recording
//! (a voice, a solo instrument) sings every 5 ms, and the notes it holds.
//!
//! [`track`] decimates a copy to about 12 kHz (a linear-phase FIR, so the
//! track stays aligned) and runs McLeod's method ([`crate::pitch`]) over
//! 40 ms windows. Frames with too little clarity or level are unvoiced;
//! single frames an octave off their neighbours are folded back, lone
//! voiced frames dropped. [`notes`] splits the voiced runs where the pitch
//! moves to a new level and stays (a 150 ms average hides vibrato) or the
//! level (over 10 ms) dips and comes back (a repeated note); a note's pitch is the
//! median of its middle, without the glides into and out of it.

use crate::pitch::{Detector, note_at};

/// Seconds between pitch frames.
pub const HOP_SECONDS: f64 = 0.005;
/// The analysis window.
const WINDOW_SECONDS: f64 = 0.040;
/// The level's window (shorter: dips between notes show).
const LEVEL_SECONDS: f64 = 0.010;
/// Rate the pitch is found at (roughly: an integer decimation).
const ANALYSIS_RATE: f64 = 11_025.0;
/// Least clarity of a voiced frame.
const CLARITY: f32 = 0.7;
/// Quietest voiced frame, absolute and below the loudest.
const FLOOR_DB: f32 = -55.0;
const RANGE_DB: f32 = 45.0;

/// One pitch frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PitchFrame {
    /// MIDI note (fractional; 69 = A 440 Hz), NaN where no pitch is heard.
    pub note: f32,
    pub clarity: f32,
    /// RMS level over 10 ms, dBFS.
    pub level_db: f32,
}

impl PitchFrame {
    pub fn voiced(&self) -> bool {
        !self.note.is_nan()
    }
}

/// The pitch over a recording: frame `i` is centred on sample `i × hop`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PitchTrack {
    pub hop: usize,
    pub frames: Vec<PitchFrame>,
}

/// A held note: frames `start..end` of a [`PitchTrack`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteSpan {
    pub start: usize,
    pub end: usize,
    /// The note it is heard as (MIDI, fractional).
    pub pitch: f32,
}

/// The pitch track of mono `x` at `rate`.
pub fn track(x: &[f32], rate: f64) -> PitchTrack {
    let hop = ((rate * HOP_SECONDS).round() as usize).max(1);
    let d = ((rate / ANALYSIS_RATE).floor() as usize).max(1);
    let low = decimate(x, d);
    let low_rate = rate / d as f64;
    let window = ((WINDOW_SECONDS * low_rate).round() as usize).clamp(64, crate::pitch::FRAMES);
    let count = x.len().div_ceil(hop);
    let mut detector = Detector::new();
    let mut buf = vec![0.0f32; window];
    let mut frames = Vec::with_capacity(count);
    for i in 0..count {
        // The window centred on the frame (in the decimated copy).
        let centre = (i * hop) as f64 / d as f64;
        let from = centre.round() as i64 - (window / 2) as i64;
        for (k, b) in buf.iter_mut().enumerate() {
            let at = from + k as i64;
            *b = if at >= 0 && (at as usize) < low.len() {
                low[at as usize]
            } else {
                0.0
            };
        }
        let lw = ((LEVEL_SECONDS * low_rate).round() as usize).clamp(8, window);
        let part = &buf[(window - lw) / 2..(window + lw) / 2];
        let rms = (part.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / lw as f64).sqrt();
        let level_db = (20.0 * rms.max(1e-9).log10()) as f32;
        let (note, clarity) = match detector.detect(&buf, low_rate) {
            Some(p) if p.freq >= 40.0 && p.freq <= 2_000.0 => {
                (note_at(p.freq, 440.0) as f32, p.clarity as f32)
            }
            _ => (f32::NAN, 0.0),
        };
        frames.push(PitchFrame {
            note,
            clarity,
            level_db,
        });
    }
    let loudest = frames
        .iter()
        .map(|f| f.level_db)
        .fold(f32::NEG_INFINITY, f32::max);
    let floor = FLOOR_DB.max(loudest - RANGE_DB);
    for f in &mut frames {
        if f.clarity < CLARITY || f.level_db < floor {
            f.note = f32::NAN;
        }
    }
    fold_octaves(&mut frames);
    drop_glitches(&mut frames);
    PitchTrack { hop, frames }
}

/// Every `d`-th sample of `x` after a windowed-sinc low-pass at 0.45 of
/// the new Nyquist rate (centred taps: no delay).
pub(crate) fn decimate(x: &[f32], d: usize) -> Vec<f32> {
    if d == 1 {
        return x.to_vec();
    }
    let half = 8 * d;
    let cutoff = 0.45 / d as f64;
    let taps: Vec<f64> = (0..=2 * half)
        .map(|k| {
            let t = k as f64 - half as f64;
            let sinc = if t == 0.0 {
                2.0 * cutoff
            } else {
                (std::f64::consts::TAU * cutoff * t).sin() / (std::f64::consts::PI * t)
            };
            let w = 0.5 + 0.5 * (std::f64::consts::PI * t / (half as f64 + 1.0)).cos();
            sinc * w
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    (0..x.len().div_ceil(d))
        .map(|j| {
            let c = (j * d) as i64;
            let mut acc = 0.0;
            for (k, h) in taps.iter().enumerate() {
                let at = c + k as i64 - half as i64;
                if at >= 0 && (at as usize) < x.len() {
                    acc += h * f64::from(x[at as usize]);
                }
            }
            (acc / sum) as f32
        })
        .collect()
}

/// The median of the voiced notes among `frames[a..b]`.
fn median_of(frames: &[PitchFrame], a: usize, b: usize) -> Option<f32> {
    let mut v: Vec<f32> = frames[a..b]
        .iter()
        .filter(|f| f.voiced())
        .map(|f| f.note)
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

/// Frames an octave (or two) away from their neighbourhood's median are
/// folded back to it: McLeod's method picks a harmonic now and then.
fn fold_octaves(frames: &mut [PitchFrame]) {
    let reach = 10;
    let notes: Vec<f32> = frames.iter().map(|f| f.note).collect();
    for i in 0..frames.len() {
        if notes[i].is_nan() {
            continue;
        }
        let (a, b) = (i.saturating_sub(reach), (i + reach + 1).min(frames.len()));
        let Some(m) = median_of(frames, a, b) else {
            continue;
        };
        let octaves = ((notes[i] - m) / 12.0).round();
        if octaves != 0.0 && (notes[i] - m - 12.0 * octaves).abs() < 1.5 {
            frames[i].note = notes[i] - 12.0 * octaves;
        }
    }
}

/// Voiced runs shorter than three frames are unvoiced; single frames far
/// from both neighbours take their mean.
fn drop_glitches(frames: &mut [PitchFrame]) {
    let mut i = 0;
    while i < frames.len() {
        if !frames[i].voiced() {
            i += 1;
            continue;
        }
        let start = i;
        while i < frames.len() && frames[i].voiced() {
            i += 1;
        }
        if i - start < 3 {
            for f in &mut frames[start..i] {
                f.note = f32::NAN;
            }
        }
    }
    for i in 1..frames.len().saturating_sub(1) {
        let (a, b, c) = (frames[i - 1].note, frames[i].note, frames[i + 1].note);
        if !a.is_nan()
            && !c.is_nan()
            && (b - a).abs() > 1.0
            && (b - c).abs() > 1.0
            && (a - c).abs() < 1.0
        {
            frames[i].note = 0.5 * (a + c);
        }
    }
}

/// The notes of a track (see the module docs).
pub fn notes(track: &PitchTrack) -> Vec<NoteSpan> {
    let frames = &track.frames;
    let per_second = 1.0 / HOP_SECONDS;
    // Unvoiced gaps this short are bridged (a consonant inside a phrase).
    let bridge = (0.025 * per_second) as usize;
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < frames.len() {
        if !frames[i].voiced() {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        while i < frames.len() {
            if frames[i].voiced() {
                end = i + 1;
                i += 1;
            } else if i - end < bridge {
                i += 1;
            } else {
                break;
            }
        }
        runs.push((start, end));
    }
    let mut out = Vec::new();
    for (a, b) in runs {
        let mut cuts = vec![a];
        cuts.extend(pitch_changes(frames, a, b));
        cuts.extend(level_dips(frames, a, b));
        cuts.push(b);
        cuts.sort_unstable();
        cuts.dedup();
        let spans: Vec<NoteSpan> = cuts
            .windows(2)
            .filter_map(|w| span(frames, w[0], w[1]))
            .collect();
        out.extend(merge_short(spans, (0.05 * per_second) as usize, frames));
    }
    out
}

/// The note heard over frames `a..b`: the median of the middle 70 %.
fn span(frames: &[PitchFrame], a: usize, b: usize) -> Option<NoteSpan> {
    if b <= a {
        return None;
    }
    let trim = (b - a) * 15 / 100;
    let pitch = median_of(frames, a + trim, b - trim).or_else(|| median_of(frames, a, b))?;
    Some(NoteSpan {
        start: a,
        end: b,
        pitch,
    })
}

/// Frames where a run's pitch moves to a new level and stays there.
fn pitch_changes(frames: &[PitchFrame], a: usize, b: usize) -> Vec<usize> {
    let reach = (0.075 / HOP_SECONDS) as usize;
    // The voiced notes, unvoiced frames carrying their last value.
    let mut raw = Vec::with_capacity(b - a);
    let mut last = median_of(frames, a, b).unwrap_or(0.0);
    for f in &frames[a..b] {
        if f.voiced() {
            last = f.note;
        }
        raw.push(last);
    }
    // A 150 ms average: vibrato is gone, a step is a ramp.
    let n = raw.len();
    let mut prefix = vec![0.0f64; n + 1];
    for (k, v) in raw.iter().enumerate() {
        prefix[k + 1] = prefix[k] + f64::from(*v);
    }
    let smooth: Vec<f32> = (0..n)
        .map(|k| {
            let (l, r) = (k.saturating_sub(reach), (k + reach + 1).min(n));
            ((prefix[r] - prefix[l]) / (r - l) as f64) as f32
        })
        .collect();
    let hold = (0.03 / HOP_SECONDS) as usize;
    let mut cuts = Vec::new();
    // Where the average has settled after the last cut (the run's start,
    // or a window past a cut).
    let mut settled = 0;
    let mut last_cut = 0;
    while settled + hold < n {
        let reference = smooth[settled..settled + hold].iter().sum::<f32>() / hold as f32;
        let Some(k) = (settled + hold..n).find(|k| (smooth[*k] - reference).abs() > 0.7) else {
            break;
        };
        // A step: cut where the raw pitch moves fastest near here (the
        // average leaves its level within a window of the step).
        let lo = k.saturating_sub(reach).max(last_cut + 1);
        let hi = (k + reach).min(n - 2);
        let at = (lo..=hi.max(lo))
            .max_by(|x, y| {
                let s = |j: usize| (raw[(j + 1).min(n - 1)] - raw[j.saturating_sub(1)]).abs();
                s(*x).total_cmp(&s(*y))
            })
            .unwrap_or(k);
        cuts.push(a + at);
        last_cut = at;
        settled = (at + reach + 1).max(k + 1);
    }
    cuts
}

/// Frames where the level falls 8 dB below its surroundings and comes
/// back (a note sung again).
fn level_dips(frames: &[PitchFrame], a: usize, b: usize) -> Vec<usize> {
    let reach = (0.06 / HOP_SECONDS) as usize;
    let mut cuts = Vec::new();
    let mut k = a + reach;
    while k + reach < b {
        let here = frames[k].level_db;
        let before = frames[k - reach..k]
            .iter()
            .map(|f| f.level_db)
            .fold(f32::MIN, f32::max);
        let after = frames[k + 1..=k + reach]
            .iter()
            .map(|f| f.level_db)
            .fold(f32::MIN, f32::max);
        let lowest = frames[k - reach..=k + reach]
            .iter()
            .map(|f| f.level_db)
            .fold(f32::MAX, f32::min);
        if here <= lowest && before - here >= 8.0 && after - here >= 8.0 {
            cuts.push(k);
            k += reach;
        } else {
            k += 1;
        }
    }
    cuts
}

/// Notes shorter than `min` frames join the neighbour nearer in pitch.
fn merge_short(mut spans: Vec<NoteSpan>, min: usize, frames: &[PitchFrame]) -> Vec<NoteSpan> {
    loop {
        let Some(i) = spans.iter().position(|s| s.end - s.start < min) else {
            return spans;
        };
        if spans.len() == 1 {
            return spans;
        }
        let near = |j: usize| (spans[j].pitch - spans[i].pitch).abs();
        let j = match (i.checked_sub(1), (i + 1 < spans.len()).then_some(i + 1)) {
            (Some(l), Some(r)) => {
                if near(l) <= near(r) {
                    l
                } else {
                    r
                }
            }
            (Some(l), None) => l,
            (None, Some(r)) => r,
            (None, None) => return spans,
        };
        let (a, b) = (spans[i.min(j)].start, spans[i.max(j)].end);
        let keep = span(frames, a, b).unwrap_or(spans[j]);
        spans[i.min(j)] = keep;
        spans.remove(i.max(j));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    /// A voice-like tone: harmonics falling off, following `pitch` (MIDI
    /// note at a time in seconds) and `level` (0…1).
    fn sing(seconds: f64, pitch: impl Fn(f64) -> f64, level: impl Fn(f64) -> f64) -> Vec<f32> {
        let mut phase = 0.0f64;
        (0..(SR * seconds) as usize)
            .map(|i| {
                let t = i as f64 / SR;
                let f = 440.0 * 2f64.powf((pitch(t) - 69.0) / 12.0);
                phase = (phase + f / SR).fract();
                let mut v = 0.0;
                for h in 1..=8 {
                    v += (std::f64::consts::TAU * phase * h as f64).sin() / (h * h) as f64;
                }
                (0.3 * v * level(t)) as f32
            })
            .collect()
    }

    fn at(track: &PitchTrack, seconds: f64) -> f32 {
        track.frames[(seconds * SR) as usize / track.hop].note
    }

    #[test]
    fn a_held_note_with_vibrato_is_one_note_at_its_centre() {
        // A3 with ±0.5 semitone of vibrato at 5.5 Hz.
        let x = sing(
            1.5,
            |t| 57.0 + 0.5 * (std::f64::consts::TAU * 5.5 * t).sin(),
            |_| 1.0,
        );
        let tr = track(&x, SR);
        assert_eq!(tr.hop, 240);
        // The curve follows the vibrato.
        let (lo, hi) = tr.frames[20..280]
            .iter()
            .filter(|f| f.voiced())
            .fold((f32::MAX, f32::MIN), |(l, h), f| {
                (l.min(f.note), h.max(f.note))
            });
        assert!(
            (lo - 56.5).abs() < 0.15 && (hi - 57.5).abs() < 0.15,
            "{lo}..{hi}"
        );
        let n = notes(&tr);
        assert_eq!(n.len(), 1, "{n:?}");
        assert!((n[0].pitch - 57.0).abs() < 0.1, "{n:?}");
    }

    #[test]
    fn a_melody_is_split_into_its_notes() {
        // C4 D4 E4 C4, 0.3 s each, gliding 30 ms between, voiced
        // throughout; then G3 after a rest.
        let line = [60.0, 62.0, 64.0, 60.0];
        let x = sing(
            2.0,
            |t| {
                if t >= 1.5 {
                    return 55.0;
                }
                let k = ((t / 0.3) as usize).min(3);
                let into = t - k as f64 * 0.3;
                if k > 0 && into < 0.03 {
                    line[k - 1] + (line[k] - line[k - 1]) * into / 0.03
                } else {
                    line[k]
                }
            },
            |t| if (1.2..1.5).contains(&t) { 0.0 } else { 1.0 },
        );
        let tr = track(&x, SR);
        assert!((at(&tr, 0.15) - 60.0).abs() < 0.05);
        assert!(
            !tr.frames[(1.35 * SR) as usize / tr.hop].voiced(),
            "the rest"
        );
        let n = notes(&tr);
        let pitches: Vec<i32> = n.iter().map(|s| s.pitch.round() as i32).collect();
        assert_eq!(pitches, [60, 62, 64, 60, 55], "{n:?}");
        // Boundaries within 30 ms of the steps.
        let seconds = |f: usize| (f * tr.hop) as f64 / SR;
        for (k, step) in [0.3, 0.6, 0.9].iter().enumerate() {
            let cut = seconds(n[k + 1].start);
            assert!((cut - step).abs() < 0.03, "{k}: {cut}");
        }
        for s in &n {
            assert!((s.pitch - s.pitch.round()).abs() < 0.08, "{s:?}");
        }
    }

    #[test]
    fn a_note_sung_again_is_two_notes() {
        // E4 twice, the level dipping between.
        let x = sing(
            0.8,
            |_| 64.0,
            |t| {
                let d = (t - 0.4).abs();
                if d < 0.03 {
                    d / 0.03 * 0.95 + 0.02
                } else {
                    1.0
                }
            },
        );
        let n = notes(&track(&x, SR));
        assert_eq!(n.len(), 2, "{n:?}");
        assert!(n.iter().all(|s| (s.pitch - 64.0).abs() < 0.05));
    }

    #[test]
    fn a_weak_fundamental_is_still_the_note() {
        // Harmonics 2–5 only: heard (and found) at the fundamental.
        let f = 196.0;
        let x: Vec<f32> = (0..24_000)
            .map(|i| {
                let t = i as f64 / SR;
                (2..=5)
                    .map(|h| (std::f64::consts::TAU * f * h as f64 * t).sin() * 0.1)
                    .sum::<f64>() as f32
            })
            .collect();
        let n = notes(&track(&x, SR));
        assert_eq!(n.len(), 1, "{n:?}");
        assert!((n[0].pitch - 55.0).abs() < 0.05, "{n:?}");
    }

    #[test]
    fn noise_and_silence_have_no_notes() {
        let mut seed = 7u32;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1 << 24) as f32 - 0.5) * 0.5
            })
            .collect();
        assert!(notes(&track(&noise, SR)).is_empty());
        let silence = vec![0.0; 48_000];
        let tr = track(&silence, SR);
        assert!(tr.frames.iter().all(|f| !f.voiced()));
        assert!(notes(&tr).is_empty());
    }

    #[test]
    fn other_rates_give_the_same_notes() {
        for rate in [44_100.0, 96_000.0] {
            let x: Vec<f32> = (0..(rate * 0.6) as usize)
                .map(|i| ((std::f64::consts::TAU * 330.0 * i as f64 / rate).sin() * 0.3) as f32)
                .collect();
            let tr = track(&x, rate);
            assert_eq!(tr.hop, (rate * HOP_SECONDS).round() as usize);
            let n = notes(&tr);
            assert_eq!(n.len(), 1, "{rate}: {n:?}");
            assert!((n[0].pitch - note_at(330.0, 440.0) as f32).abs() < 0.05);
        }
    }
}
