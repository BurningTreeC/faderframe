//! Sung words: where a word really sounds, what each take's word sounds
//! like, which take sings it best, and the quiet points between words a
//! comp boundary goes in. Everything is plain measurement (no learning):
//! the same audio always gives the same answer, and every answer says
//! what it was judged by.
//!
//! * [`refine`] moves a transcriber's coarse word edges to where the
//!   voice is: from the first to the last 10 ms frame within 30 dB of the
//!   word's loudest (and 8 dB over the floor around it) — the start looked
//!   for from [`EDGE_REACH`] before where it was heard to the end of the
//!   span heard (a word heard as starting in the pause before it starts
//!   where the voice does; sound before a [`PAUSE_SECONDS`] pause in the
//!   span is the last word's tail), the end at most [`EDGE_REACH`] past
//!   it, never into a neighbour.
//! * [`measure`] takes a word's level, the floor around it, clipped
//!   samples, a low-frequency burst at its start (a plosive pop) and, from
//!   the pitch track ([`crate::melody`]), how much of its middle is
//!   voiced, how much of that is within [`IN_TUNE_CENTS`] of a semitone
//!   and how much it wanders around its own pitch.
//! * [`judge`] scores the takes of one word against each other (level,
//!   pops and noise relative to the other takes; tuning, steadiness,
//!   timing and clipping absolutely) and gives each its [`Reason`]s.
//! * [`quiet_point`] and [`zero_crossing`] place a boundary in a gap.

use crate::melody::PitchTrack;
use std::fmt;
use std::ops::Range;

/// The frames levels are taken over (seconds).
pub const FRAME_SECONDS: f64 = 0.010;
/// How far (seconds) [`refine`] moves an edge.
pub const EDGE_REACH: f64 = 0.15;
/// A quiet stretch this long (seconds) inside a word's span heard is a
/// pause before the word, not a part of it.
pub const PAUSE_SECONDS: f64 = 0.15;
/// A voiced frame this close to a semitone (cents) is in tune.
pub const IN_TUNE_CENTS: f32 = 25.0;
/// Full scale: a sample at least this loud is clipped.
pub const CLIP_LEVEL: f32 = 0.999;

fn db(power: f64) -> f32 {
    (10.0 * power.max(1e-12).log10()) as f32
}

/// Levels (dBFS) of consecutive frames of `frame` samples.
fn frame_levels(x: &[f32], frame: usize) -> Vec<f32> {
    x.chunks(frame.max(1))
        .map(|c| db(c.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / c.len() as f64))
        .collect()
}

/// The `q` quantile (0…1) of `v`.
fn quantile(v: &[f32], q: f64) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    Some(s[((s.len() - 1) as f64 * q).round() as usize])
}

/// Word edges (`(start, end)` samples of `mono` at `rate`, in order)
/// moved to where the voice is (see the module). Words too quiet to tell
/// from the floor stay as they are.
pub fn refine(mono: &[f32], rate: f64, words: &mut [(usize, usize)]) {
    let frame = ((rate * FRAME_SECONDS).round() as usize).max(1);
    let levels = frame_levels(mono, frame);
    if levels.is_empty() {
        return;
    }
    let reach = ((EDGE_REACH * rate) as usize / frame).max(1);
    let pause = ((PAUSE_SECONDS * rate) as usize / frame).max(1);
    let second = (rate as usize / frame).max(1);
    let last = levels.len();
    let mut floor_edge = 0usize;
    for i in 0..words.len() {
        let (s, e) = words[i];
        let next = words.get(i + 1).map_or(last, |w| (w.0 / frame).min(last));
        let (fs, fe) = (
            (s / frame).min(last - 1),
            (e.div_ceil(frame)).clamp(s / frame + 1, last),
        );
        let peak = levels[fs..fe]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let around = &levels[fs.saturating_sub(second)..(fe + second).min(last)];
        let floor = quantile(around, 0.1).unwrap_or(-120.0);
        if peak < floor + 10.0 {
            floor_edge = e / frame;
            continue;
        }
        let threshold = (peak - 30.0).max(floor + 8.0);
        let lo = fs.saturating_sub(reach).max(floor_edge);
        let hi = fe;
        let Some(mut start) = (lo..hi.max(lo)).find(|&f| levels[f] >= threshold) else {
            floor_edge = e / frame;
            continue;
        };
        // A pause inside the span heard: what came before it was the last
        // word's tail (or a breath); the word starts after it.
        let mut quiet = 0;
        for (f, &level) in levels.iter().enumerate().take(hi).skip(start) {
            if level < threshold {
                quiet += 1;
            } else {
                if quiet >= pause {
                    start = f;
                    break;
                }
                quiet = 0;
            }
        }
        let end_hi = (fe + reach).min(next.max(start + 1)).min(last);
        let end = (start..end_hi)
            .rev()
            .find(|&f| levels[f] >= threshold)
            .map_or(fe, |f| f + 1);
        words[i] = (
            (start * frame).min(mono.len()),
            (end * frame).min(mono.len()).max(start * frame + 1),
        );
        floor_edge = end;
    }
}

/// What a take's word sounds like.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WordSound {
    /// RMS over the word (dBFS).
    pub level_db: f32,
    /// The floor around it: the quietest tenth of the frames outside the
    /// word (dBFS; −120 when there are none).
    pub floor_db: f32,
    /// Samples at full scale.
    pub clipped: u32,
    /// The loudest 10 ms below 120 Hz at its start, against its level
    /// (dB).
    pub pop_db: f32,
    /// Share of its middle that is voiced (0…1).
    pub voiced: f32,
    /// Share of the voiced middle within [`IN_TUNE_CENTS`] of a semitone.
    pub in_tune: f32,
    /// RMS of the voiced middle around its median pitch (cents).
    pub wobble_cents: f32,
}

/// Measure the word at `word` (samples of `mono` at `rate`); the floor
/// is read in `around` outside it; `track` is `mono`'s pitch track.
pub fn measure(
    mono: &[f32],
    rate: f64,
    word: Range<usize>,
    around: Range<usize>,
    track: Option<&PitchTrack>,
) -> WordSound {
    let n = mono.len();
    let w = word.start.min(n)..word.end.min(n).max(word.start.min(n));
    let x = &mono[w.clone()];
    let mut s = WordSound {
        level_db: if x.is_empty() {
            -120.0
        } else {
            db(x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64)
        },
        floor_db: -120.0,
        clipped: x.iter().filter(|v| v.abs() >= CLIP_LEVEL).count() as u32,
        ..WordSound::default()
    };
    let frame = ((rate * FRAME_SECONDS).round() as usize).max(1);
    let a = around.start.min(n)..around.end.min(n);
    let mut outside = frame_levels(&mono[a.start..w.start.max(a.start)], frame);
    outside.extend(frame_levels(
        &mono[w.end.min(a.end)..a.end.max(w.end)],
        frame,
    ));
    if let Some(f) = quantile(&outside, 0.1) {
        s.floor_db = f;
    }
    // A pop: low frequencies bursting at the start (a 120 Hz low-pass,
    // two poles).
    let from = w.start.saturating_sub((0.02 * rate) as usize);
    let to = (w.start + (0.08 * rate) as usize).min(n);
    if to > from {
        let (b, aa) = lowpass(120.0, rate);
        let mut z = [0.0f64; 2];
        let mut low = Vec::with_capacity(to - from);
        for v in &mono[from..to] {
            let v = f64::from(*v);
            let y = b[0] * v + z[0];
            z[0] = b[1] * v - aa[0] * y + z[1];
            z[1] = b[2] * v - aa[1] * y;
            low.push(y as f32);
        }
        let peak = frame_levels(&low, frame)
            .into_iter()
            .fold(f32::NEG_INFINITY, f32::max);
        s.pop_db = peak - s.level_db;
    }
    if let Some(t) = track.filter(|t| t.hop > 0) {
        // The middle 70 %: scoops into the word and falls out of it are
        // singing, not tuning.
        let len = w.len();
        let (m0, m1) = (w.start + len * 15 / 100, w.end - len * 15 / 100);
        let frames: Vec<f32> = (m0 / t.hop..m1.div_ceil(t.hop).min(t.frames.len()))
            .map(|i| t.frames[i].note)
            .collect();
        let voiced: Vec<f32> = frames.iter().copied().filter(|v| !v.is_nan()).collect();
        if !frames.is_empty() {
            s.voiced = voiced.len() as f32 / frames.len() as f32;
        }
        if !voiced.is_empty() {
            let tuned = voiced
                .iter()
                .filter(|v| ((*v - v.round()) * 100.0).abs() <= IN_TUNE_CENTS)
                .count();
            s.in_tune = tuned as f32 / voiced.len() as f32;
            let median = quantile(&voiced, 0.5).unwrap_or(0.0);
            s.wobble_cents = (voiced
                .iter()
                .map(|v| ((v - median) * 100.0).powi(2))
                .sum::<f32>()
                / voiced.len() as f32)
                .sqrt();
        }
    }
    s
}

/// A two-pole Butterworth low-pass (RBJ): `(b, a)` with `a0` = 1.
fn lowpass(hz: f64, rate: f64) -> ([f64; 3], [f64; 2]) {
    let w = std::f64::consts::TAU * hz / rate;
    let alpha = w.sin() / std::f64::consts::SQRT_2;
    let c = w.cos();
    let a0 = 1.0 + alpha;
    (
        [(1.0 - c) / 2.0 / a0, (1.0 - c) / a0, (1.0 - c) / 2.0 / a0],
        [-2.0 * c / a0, (1.0 - alpha) / a0],
    )
}

/// Why a take's word was judged as it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reason {
    /// Share of the voiced middle in tune.
    InTune(f32),
    OutOfTune(f32),
    /// RMS cents around its pitch.
    Steady(f32),
    Wobbly(f32),
    /// Milliseconds from the grid (negative: early).
    OnTime(f64),
    OffTime(f64),
    /// dB from the other takes' median level.
    Quieter(f32),
    Louder(f32),
    /// Its level over the floor around it (dB).
    Clean(f32),
    Noisy(f32),
    /// Samples at full scale.
    Clipped(u32),
    /// dB of the burst over the other takes'.
    Pop(f32),
}

impl Reason {
    /// Does it speak for the take?
    pub fn good(&self) -> bool {
        matches!(
            self,
            Reason::InTune(_) | Reason::Steady(_) | Reason::OnTime(_) | Reason::Clean(_)
        )
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Reason::InTune(s) => write!(f, "in tune ({:.0} %)", s * 100.0),
            Reason::OutOfTune(s) => write!(f, "off pitch ({:.0} % in tune)", s * 100.0),
            Reason::Steady(c) => write!(f, "steady pitch (±{c:.0} cents)"),
            Reason::Wobbly(c) => write!(f, "wavering pitch (±{c:.0} cents)"),
            Reason::OnTime(ms) => write!(f, "on the grid ({ms:+.0} ms)"),
            Reason::OffTime(ms) if ms < 0.0 => write!(f, "early ({:.0} ms)", -ms),
            Reason::OffTime(ms) => write!(f, "late ({ms:.0} ms)"),
            Reason::Quieter(d) => write!(f, "{:.1} dB quieter than the others", -d),
            Reason::Louder(d) => write!(f, "{d:.1} dB louder than the others"),
            Reason::Clean(d) => write!(f, "clean ({d:.0} dB over the noise)"),
            Reason::Noisy(d) => write!(f, "noisy (only {d:.0} dB over the noise)"),
            Reason::Clipped(n) => write!(f, "clipped ({n} samples)"),
            Reason::Pop(d) => write!(f, "plosive pop ({d:.0} dB more low end)"),
        }
    }
}

/// One take's word, judged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Judged {
    pub score: f32,
    /// Strongest first.
    pub reasons: Vec<Reason>,
}

/// The takes of one word judged against each other (`sounds[t]`: take
/// `t`'s word, `None` where it does not sing it; `timing_ms[t]`: its
/// start from the nearest grid line): per take its judgement, and the
/// best take (ties: the first).
pub fn judge(
    sounds: &[Option<WordSound>],
    timing_ms: &[Option<f64>],
) -> (Vec<Option<Judged>>, Option<usize>) {
    let present: Vec<&WordSound> = sounds.iter().flatten().collect();
    let median = |f: &dyn Fn(&WordSound) -> f32| {
        quantile(&present.iter().map(|s| f(s)).collect::<Vec<_>>(), 0.5)
    };
    let level = median(&|s| s.level_db).unwrap_or(0.0);
    let pop = median(&|s| s.pop_db).unwrap_or(0.0);
    let pitched = present.iter().any(|s| s.voiced >= 0.3);
    let judged: Vec<Option<Judged>> = sounds
        .iter()
        .enumerate()
        .map(|(t, s)| {
            let s = s.as_ref()?;
            // (points, weight for ordering, reason)
            let mut parts: Vec<(f32, Reason)> = Vec::new();
            if pitched && s.voiced >= 0.3 {
                let tune = 3.0 * s.in_tune;
                parts.push((
                    tune - 1.5,
                    if s.in_tune >= 0.6 {
                        Reason::InTune(s.in_tune)
                    } else {
                        Reason::OutOfTune(s.in_tune)
                    },
                ));
                let steady = 1.5 * ((60.0 - s.wobble_cents) / 40.0).clamp(0.0, 1.0);
                parts.push((
                    steady - 0.75,
                    if s.wobble_cents <= 35.0 {
                        Reason::Steady(s.wobble_cents)
                    } else {
                        Reason::Wobbly(s.wobble_cents)
                    },
                ));
            }
            if let Some(ms) = timing_ms.get(t).copied().flatten() {
                let on = 1.5 * (1.0 - ms.abs() / 80.0).clamp(0.0, 1.0) as f32;
                parts.push((
                    on - 0.75,
                    if ms.abs() <= 30.0 {
                        Reason::OnTime(ms)
                    } else {
                        Reason::OffTime(ms)
                    },
                ));
            }
            let dl = s.level_db - level;
            if dl < -3.0 {
                parts.push((-(-(dl + 3.0) * 0.15).min(1.5), Reason::Quieter(dl)));
            } else if dl > 4.0 {
                parts.push(((dl - 4.0) * -0.1, Reason::Louder(dl)));
            }
            let snr = s.level_db - s.floor_db;
            let clean = ((snr - 20.0) / 20.0).clamp(0.0, 1.0);
            parts.push((
                clean - 0.5,
                if snr >= 30.0 {
                    Reason::Clean(snr)
                } else {
                    Reason::Noisy(snr)
                },
            ));
            if s.clipped > 0 {
                parts.push((
                    if s.clipped > 20 { -4.0 } else { -3.0 },
                    Reason::Clipped(s.clipped),
                ));
            }
            let dp = s.pop_db - pop;
            if dp >= 6.0 && s.pop_db >= -10.0 {
                parts.push((-1.5 * ((dp - 6.0) / 6.0 + 0.5).min(1.0), Reason::Pop(dp)));
            }
            let score = parts.iter().map(|p| p.0).sum();
            parts.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()));
            Some(Judged {
                score,
                reasons: parts.into_iter().map(|p| p.1).collect(),
            })
        })
        .collect();
    let best = judged
        .iter()
        .enumerate()
        .filter_map(|(t, j)| j.as_ref().map(|j| (t, j.score)))
        .fold(None::<(usize, f32)>, |b, (t, s)| match b {
            Some((_, bs)) if bs >= s => b,
            _ => Some((t, s)),
        })
        .map(|(t, _)| t);
    (judged, best)
}

/// The quietest point (an index into the window) for a boundary between
/// `signals` (equal-length windows of the takes either side; the frames
/// `pad` from each end are context only): the least summed energy over
/// `span` samples around it, of the candidates within 1 dB of that the
/// nearest to `prefer`.
pub fn quiet_point(signals: &[&[f32]], pad: usize, span: usize, prefer: usize) -> usize {
    let len = signals.iter().map(|s| s.len()).min().unwrap_or(0);
    if len == 0 {
        return prefer;
    }
    let (lo, hi) = (
        pad.min(len - 1),
        len.saturating_sub(pad).max(pad.min(len - 1) + 1),
    );
    // Running sums of the summed power.
    let mut acc = vec![0.0f64; len + 1];
    for i in 0..len {
        let p: f64 = signals.iter().map(|s| f64::from(s[i]).powi(2)).sum();
        acc[i + 1] = acc[i] + p;
    }
    let half = (span / 2).max(1);
    let energy = |i: usize| {
        let (a, b) = (i.saturating_sub(half), (i + half).min(len));
        (acc[b] - acc[a]) / (b - a).max(1) as f64
    };
    // Candidates every 0.5 ms-ish (a sixteenth of the span).
    let step = (span / 16).max(1);
    let candidates: Vec<(usize, f64)> = (lo..hi).step_by(step).map(|i| (i, energy(i))).collect();
    let least = candidates.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
    let near = least.max(1e-12) * 10f64.powf(0.1);
    candidates
        .into_iter()
        .filter(|c| c.1 <= near)
        .min_by_key(|c| c.0.abs_diff(prefer))
        .map_or(prefer.clamp(lo, hi - 1), |c| c.0)
}

/// The zero crossing of `x` nearest `at` inside `lo..hi` (the sample of
/// the pair nearer zero); `at` when there is none.
pub fn zero_crossing(x: &[f32], at: usize, lo: usize, hi: usize) -> usize {
    let hi = hi.min(x.len());
    (lo.max(1)..hi)
        .filter(|&i| (x[i - 1] <= 0.0) != (x[i] <= 0.0) || x[i] == 0.0)
        .map(|i| {
            if x[i - 1].abs() < x[i].abs() {
                i - 1
            } else {
                i
            }
        })
        .filter(|&i| i >= lo && i < hi)
        .min_by_key(|&i| i.abs_diff(at))
        .unwrap_or(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const RATE: f64 = 48_000.0;

    /// A sung note over `a..b` seconds at `hz`, `level` (linear), into `x`.
    fn note(x: &mut [f32], a: f64, b: f64, hz: f64, level: f64) {
        let (i0, i1) = ((a * RATE) as usize, ((b * RATE) as usize).min(x.len()));
        for (k, v) in x[i0..i1].iter_mut().enumerate() {
            let t = k as f64 / RATE;
            let fade = (t / 0.01).min(1.0).min((i1 - i0 - k) as f64 / RATE / 0.01);
            *v += (level * fade * (TAU * hz * t).sin()) as f32;
        }
    }

    fn noise(x: &mut [f32], level: f64, seed: u32) {
        let mut s = seed;
        for v in x.iter_mut() {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *v += (level * (f64::from(s >> 8) / f64::from(1u32 << 24) * 2.0 - 1.0)) as f32;
        }
    }

    #[test]
    fn coarse_edges_move_to_the_voice() {
        let mut x = vec![0.0f32; (2.0 * RATE) as usize];
        note(&mut x, 0.30, 0.70, 220.0, 0.5);
        note(&mut x, 1.10, 1.60, 247.0, 0.5);
        noise(&mut x, 0.0005, 3);
        let s = |v: f64| (v * RATE) as usize;
        // As a transcriber hears them: starts early, each to the next.
        let mut words = [(s(0.22), s(1.04)), (s(1.04), s(1.9))];
        refine(&x, RATE, &mut words);
        let secs = |v: usize| v as f64 / RATE;
        assert!((secs(words[0].0) - 0.30).abs() <= 0.011, "{words:?}");
        assert!((secs(words[0].1) - 0.70).abs() <= 0.021, "{words:?}");
        assert!((secs(words[1].0) - 1.10).abs() <= 0.011, "{words:?}");
        assert!((secs(words[1].1) - 1.60).abs() <= 0.021, "{words:?}");
        // Heard as starting in the pause before it: it starts where the
        // voice does.
        let mut late = [(s(0.22), s(0.71)), (s(0.71), s(1.65))];
        refine(&x, RATE, &mut late);
        assert!((secs(late[1].0) - 1.10).abs() <= 0.011, "{late:?}");
        // The last word's tail running into the span: past the pause.
        let mut tail = [(s(0.22), s(0.65)), (s(0.65), s(1.65))];
        refine(&x, RATE, &mut tail);
        assert!((secs(tail[0].1) - 0.65).abs() <= 0.011, "{tail:?}");
        assert!((secs(tail[1].0) - 1.10).abs() <= 0.011, "{tail:?}");
    }

    #[test]
    fn a_word_is_measured() {
        let mut x = vec![0.0f32; (2.0 * RATE) as usize];
        note(&mut x, 0.5, 1.0, 220.0, 0.3);
        noise(&mut x, 0.001, 7);
        let w = (0.5 * RATE) as usize..RATE as usize;
        let track = crate::melody::track(&x, RATE);
        let s = measure(&x, RATE, w.clone(), 0..x.len(), Some(&track));
        // A sine of 0.3: −13.5 dBFS.
        assert!((s.level_db + 13.5).abs() < 0.5, "{s:?}");
        assert!(s.floor_db < -60.0, "{s:?}");
        assert_eq!(s.clipped, 0);
        assert!(s.voiced > 0.9 && s.in_tune > 0.9, "{s:?}");
        assert!(s.wobble_cents < 5.0, "{s:?}");
        // A quarter tone flat, clipped, with a thump at the start.
        let mut y = vec![0.0f32; x.len()];
        note(&mut y, 0.5, 1.0, 220.0 * 2f64.powf(-0.5 / 12.0), 1.4);
        note(&mut y, 0.49, 0.53, 50.0, 0.9);
        for v in &mut y {
            *v = v.clamp(-1.0, 1.0);
        }
        let track = crate::melody::track(&y, RATE);
        let t = measure(&y, RATE, w, 0..y.len(), Some(&track));
        assert!(t.clipped > 100, "{t:?}");
        assert!(t.in_tune < 0.2, "{t:?}");
        assert!(t.pop_db > s.pop_db + 6.0, "{t:?} vs {s:?}");
    }

    fn sound(in_tune: f32, wobble: f32, level: f32) -> WordSound {
        WordSound {
            level_db: level,
            floor_db: -80.0,
            clipped: 0,
            pop_db: -25.0,
            voiced: 1.0,
            in_tune,
            wobble_cents: wobble,
        }
    }

    #[test]
    fn the_best_take_wins_by_its_reasons() {
        let sounds = [
            Some(sound(0.5, 40.0, -18.0)),
            Some(sound(0.95, 10.0, -18.5)),
            None,
            Some(WordSound {
                clipped: 30,
                ..sound(0.97, 8.0, -12.0)
            }),
        ];
        let timing = [Some(10.0), Some(-12.0), None, Some(60.0)];
        let (j, best) = judge(&sounds, &timing);
        assert_eq!(best, Some(1));
        assert!(j[2].is_none());
        let reasons = &j[1].as_ref().unwrap().reasons;
        assert!(reasons.iter().any(|r| matches!(r, Reason::InTune(_))));
        assert!(reasons.iter().any(|r| matches!(r, Reason::OnTime(_))));
        let clipped = &j[3].as_ref().unwrap().reasons;
        assert!(matches!(clipped[0], Reason::Clipped(30)), "{clipped:?}");
        assert!(j[0].as_ref().unwrap().reasons.iter().any(|r| !r.good()));
        // Deterministic, and ties go to the first take.
        let (_, again) = judge(&sounds, &timing);
        assert_eq!(again, best);
        let (_, tie) = judge(&[Some(sound(0.9, 10.0, -18.0)); 2], &[None, None]);
        assert_eq!(tie, Some(0));
        assert_eq!(Reason::OffTime(-42.0).to_string(), "early (42 ms)");
    }

    #[test]
    fn boundaries_find_the_quiet_and_the_crossing() {
        let mut a = vec![0.0f32; 4800];
        let mut b = vec![0.0f32; 4800];
        note(&mut a, 0.0, 0.04, 300.0, 0.5);
        note(&mut b, 0.06, 0.1, 200.0, 0.5);
        // Quiet between 40 and 60 ms in both; prefer near 55 ms.
        let q = quiet_point(&[&a, &b], 0, 240, 2640);
        assert!((2016..=2880).contains(&q), "{q}");
        let mut c = vec![0.0f32; 1000];
        note(&mut c, 0.0, 0.02, 1000.0, 0.5);
        let z = zero_crossing(&c, 30, 0, 1000);
        assert!(c[z].abs() < 0.07, "{z} {}", c[z]);
        assert!(z.abs_diff(24) <= 1, "{z}");
    }
}
