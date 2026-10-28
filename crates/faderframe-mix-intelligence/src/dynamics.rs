//! Dynamics over time: a fine envelope of each rendered signal (5 ms
//! blocks: the highest sample and the mean square), its BS.1770 loudness
//! every 100 ms (momentary and short-term), and the indicators read from
//! them. Streamed with the profile ([`crate::profile::ProfileBuilder`]):
//! about 4 bytes per 5 ms (a five-minute track: 0.25 MB).
//!
//! The indicators, each defined once here (all levels in dB):
//!
//! * **Crest factor** — the sample peak over the RMS of the whole range
//!   (dBFS − dBFS; a sine reads 3.0, a square 0).
//! * **PLR** (peak-to-loudness ratio) — the true peak (dBTP, 4×
//!   oversampled) minus the integrated loudness (LUFS, BS.1770-4) of the
//!   whole range.
//! * **LRA** (loudness range, EBU Tech 3342) — the spread between the 10th
//!   and 95th percentiles of the short-term loudness (3 s windows every
//!   100 ms), gated absolutely at −70 LUFS and relatively 20 LU under the
//!   energy mean of what passed the first gate.
//! * **Short-term distribution** — the 10th, 50th and 95th percentiles of
//!   the same gated short-term values; with the time they cover (100 ms
//!   each): how long the signal is "active" in the LRA's sense.
//! * **Events** — where the envelope (5 ms RMS) rises by
//!   [`crate::config::DynamicsRule::event_rise_db`] or more over its lowest
//!   value in the 50 ms before, with its peak above the floor, at least
//!   50 ms after the last one. A deterministic envelope rule (the onset
//!   detector proper, `faderframe_audio_files::onsets`, needs the waveform,
//!   which the streaming profile does not keep; it is used where timing
//!   precision matters, [`crate::phase`]).
//! * **Transient-to-sustain ratio** (TSR) — per event, the highest 5 ms
//!   peak within 30 ms of it over the RMS from 60 to 200 ms after it (cut
//!   short before the next event, at least 40 ms of it); the median over
//!   events. How far attacks stand over what follows them: compression
//!   with a fast attack, limiting, clipping and added sustain (reverb)
//!   lower it, a slow attack can raise it.
//! * **Attack rise** — per event, that peak over the RMS of the 50 ms
//!   before the event: how sharply it starts; the median.
//!
//! None of them is a quality: they describe the signal.

use crate::config::DynamicsRule;
use crate::profile::{FLOOR_DB, TrackProfile, from_cdb, to_cdb};
use faderframe_analysis::WeightedLoudness;

/// The envelope's block (seconds).
pub const ENVELOPE_SECONDS: f64 = 0.005;
/// The loudness series' step (seconds): BS.1770's 100 ms blocks.
pub const LOUDNESS_STEP: f64 = 0.1;
/// Short-term value `i` ends `(i + 30)` steps in (3 s windows).
pub const SHORT_TERM_LAG: usize = 30;
/// Momentary value `i` ends `(i + 4)` steps in (400 ms windows).
pub const MOMENTARY_LAG: usize = 4;
/// EBU Tech 3342's gates.
const ABSOLUTE_GATE: f64 = -70.0;
const RELATIVE_GATE: f64 = -20.0;

/// A loudness (LUFS) in hundredths (the floor for silence).
fn lufs_cdb(l: f64) -> i16 {
    if l.is_finite() {
        ((l * 100.0).round() as i32).clamp((FLOOR_DB * 100.0) as i32, 6_000) as i16
    } else {
        (FLOOR_DB * 100.0) as i16
    }
}

fn cdb_lufs(v: i16) -> f64 {
    let l = f64::from(v) / 100.0;
    if l <= FLOOR_DB { f64::NEG_INFINITY } else { l }
}

/// A signal's envelope and loudness over time (see the module docs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DynamicsTrace {
    /// Samples per envelope block.
    pub block: usize,
    pub rate: u32,
    /// Per block: the highest sample magnitude of any channel (as a power,
    /// in hundredths of a dB) and the mean square (the channels' mean).
    peak: Vec<i16>,
    power: Vec<i16>,
    /// Short-term loudness every 100 ms (hundredths of a LUFS), the first
    /// ending at 3 s.
    short_term: Vec<i16>,
    /// Momentary loudness every 100 ms, the first ending at 0.4 s.
    momentary: Vec<i16>,
}

impl DynamicsTrace {
    pub fn blocks(&self) -> usize {
        self.power.len()
    }

    /// Seconds per envelope block.
    pub fn block_seconds(&self) -> f64 {
        self.block as f64 / f64::from(self.rate.max(1))
    }

    /// Envelope blocks in `seconds`.
    pub fn blocks_in(&self, seconds: f64) -> usize {
        (seconds / self.block_seconds().max(1e-9)).round().max(1.0) as usize
    }

    /// Block `i`'s highest sample (dBFS; [`FLOOR_DB`] for silence).
    pub fn peak_db(&self, i: usize) -> f64 {
        self.peak.get(i).map_or(FLOOR_DB, |v| from_cdb(*v))
    }

    /// Block `i`'s RMS (dBFS; [`FLOOR_DB`] for silence).
    pub fn power_db(&self, i: usize) -> f64 {
        self.power.get(i).map_or(FLOOR_DB, |v| from_cdb(*v))
    }

    /// The mean square of blocks `a..b` (0 when empty or silent).
    pub fn mean_power(&self, a: usize, b: usize) -> f64 {
        let b = b.min(self.blocks());
        if b <= a {
            return 0.0;
        }
        self.power[a..b]
            .iter()
            .map(|v| crate::profile::db_power(from_cdb(*v)))
            .sum::<f64>()
            / (b - a) as f64
    }

    /// Short-term loudness every 100 ms (LUFS, `-inf` for silence); value
    /// `i` ends at `(i + 30) × 100 ms`.
    pub fn short_term(&self) -> Vec<f64> {
        self.short_term.iter().map(|v| cdb_lufs(*v)).collect()
    }

    /// Momentary loudness every 100 ms; value `i` ends at `(i + 4) × 100
    /// ms`.
    pub fn momentary(&self) -> Vec<f64> {
        self.momentary.iter().map(|v| cdb_lufs(*v)).collect()
    }

    /// Memory it takes (bytes).
    pub fn bytes(&self) -> usize {
        2 * (self.peak.len() + self.power.len() + self.short_term.len() + self.momentary.len())
    }
}

/// Builds a [`DynamicsTrace`] from audio fed piece by piece.
#[derive(Clone, Debug)]
pub(crate) struct TraceBuilder {
    fill: usize,
    peak: f32,
    sum: f64,
    channels: usize,
    out: DynamicsTrace,
}

impl TraceBuilder {
    pub(crate) fn new(rate: u32, channels: usize) -> Self {
        Self {
            fill: 0,
            peak: 0.0,
            sum: 0.0,
            channels: channels.max(1),
            out: DynamicsTrace {
                block: ((f64::from(rate) * ENVELOPE_SECONDS).round() as usize).max(1),
                rate,
                ..DynamicsTrace::default()
            },
        }
    }

    fn end_block(&mut self, n: usize) {
        let ms = self.sum / n.max(1) as f64;
        let pk = f64::from(self.peak);
        self.out.peak.push(to_cdb(pk * pk));
        self.out.power.push(to_cdb(ms));
        self.fill = 0;
        self.peak = 0.0;
        self.sum = 0.0;
    }

    /// The next piece (every channel, equal lengths, finite samples).
    pub(crate) fn push(&mut self, piece: &[&[f32]]) {
        let n = piece.iter().map(|c| c.len()).min().unwrap_or(0);
        let k = self.channels as f64;
        for i in 0..n {
            let mut pk = 0.0f32;
            let mut ms = 0.0f64;
            for c in piece {
                let v = c[i];
                pk = pk.max(v.abs());
                ms += f64::from(v) * f64::from(v);
            }
            self.peak = self.peak.max(pk);
            self.sum += ms / k;
            self.fill += 1;
            if self.fill == self.out.block {
                self.end_block(self.out.block);
            }
        }
    }

    /// The trace, with the loudness series of `meter` (the profile's).
    pub(crate) fn finish(mut self, meter: Option<&WeightedLoudness>) -> DynamicsTrace {
        if self.fill * 2 >= self.out.block {
            let n = self.fill;
            self.end_block(n);
        }
        if let Some(m) = meter {
            self.out.short_term = m.short_terms().iter().map(|l| lufs_cdb(*l)).collect();
            self.out.momentary = m.momentary().iter().map(|l| lufs_cdb(*l)).collect();
        }
        self.out
    }
}

/// `p`-th quantile (0 … 1) of sorted values, nearest rank as EBU Tech
/// 3342's reference does (`round((n − 1) · p)`).
pub fn quantile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let i = ((sorted.len() - 1) as f64 * p.clamp(0.0, 1.0)).round() as usize;
    sorted.get(i).copied()
}

/// The median of `v` (its order changed).
pub fn median(v: &mut [f64]) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        None
    } else if n % 2 == 1 {
        Some(v[n / 2])
    } else {
        Some(0.5 * (v[n / 2 - 1] + v[n / 2]))
    }
}

/// The short-term values that pass EBU Tech 3342's gates, sorted.
pub fn gated_short_terms(t: &DynamicsTrace) -> Vec<f64> {
    let above: Vec<f64> = t
        .short_term()
        .into_iter()
        .filter(|l| *l > ABSOLUTE_GATE)
        .collect();
    if above.is_empty() {
        return Vec::new();
    }
    let mean = above
        .iter()
        .map(|l| 10f64.powf((l + 0.691) / 10.0))
        .sum::<f64>()
        / above.len() as f64;
    let gate = -0.691 + 10.0 * mean.log10() + RELATIVE_GATE;
    let mut kept: Vec<f64> = above.into_iter().filter(|l| *l > gate).collect();
    kept.sort_by(f64::total_cmp);
    kept
}

/// Events (envelope blocks; see the module docs).
pub fn events(t: &DynamicsTrace, rule: &DynamicsRule) -> Vec<usize> {
    let back = t.blocks_in(0.05);
    let gap = t.blocks_in(rule.event_gap_seconds);
    let rise = |i: usize| -> f64 {
        let lo = (i.saturating_sub(back)..i)
            .map(|j| t.power_db(j))
            .fold(f64::INFINITY, f64::min);
        t.power_db(i) - lo
    };
    let mut out: Vec<usize> = Vec::new();
    for i in back + 1..t.blocks() {
        if t.peak_db(i) < rule.event_floor_db {
            continue;
        }
        if rise(i) < rule.event_rise_db || rise(i - 1) >= rule.event_rise_db {
            continue;
        }
        if out.last().is_some_and(|l| i - l < gap) {
            continue;
        }
        out.push(i);
    }
    out
}

/// One event's shape on a trace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EventShape {
    pub block: usize,
    /// Transient-to-sustain ratio (dB).
    pub tsr_db: f64,
    /// Attack rise (dB).
    pub attack_db: f64,
    /// The attack's peak (dBFS).
    pub peak_db: f64,
}

/// The shape of each of `events` (blocks; found on this or another trace
/// of the same timing) on `t` (`None` where `t` is silent there or no
/// sustain window fits before the next event).
pub fn shapes(t: &DynamicsTrace, events: &[usize], rule: &DynamicsRule) -> Vec<Option<EventShape>> {
    let attack = t.blocks_in(0.03);
    let (s0, s1) = (t.blocks_in(0.06), t.blocks_in(0.2));
    let min_sustain = t.blocks_in(0.04);
    let back = t.blocks_in(0.05);
    events
        .iter()
        .enumerate()
        .map(|(k, &i)| {
            if i >= t.blocks() {
                return None;
            }
            let next = events.get(k + 1).copied().unwrap_or(usize::MAX);
            let peak = (i..(i + attack).min(t.blocks()))
                .map(|j| t.peak_db(j))
                .fold(FLOOR_DB, f64::max);
            if peak < rule.event_floor_db {
                return None;
            }
            let (a, b) = (i + s0, (i + s1).min(next.saturating_sub(1)).min(t.blocks()));
            if b < a + min_sustain {
                return None;
            }
            let sustain = crate::profile::power_db(t.mean_power(a, b));
            let pre = crate::profile::power_db(t.mean_power(i.saturating_sub(back), i));
            Some(EventShape {
                block: i,
                tsr_db: (peak - sustain).clamp(-20.0, 80.0),
                attack_db: (peak - pre).clamp(-20.0, 80.0),
                peak_db: peak,
            })
        })
        .collect()
}

/// A signal's dynamics indicators (see the module docs; `None`: not
/// measurable — silence, too short, too few events).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackDynamics {
    pub crest_db: Option<f64>,
    pub plr_db: Option<f64>,
    pub lra_lu: Option<f64>,
    /// 10th, 50th and 95th percentiles of the gated short-term loudness.
    pub short_term: Option<[f64; 3]>,
    /// Time the gated short-term values cover (seconds).
    pub active_seconds: f64,
    pub events: usize,
    pub tsr_db: Option<f64>,
    pub attack_db: Option<f64>,
}

/// The indicators of a profile.
pub fn measure(p: &TrackProfile, rule: &DynamicsRule) -> TrackDynamics {
    let l = &p.levels;
    let finite = |v: f64| v.is_finite().then_some(v);
    let crest_db = (finite(l.sample_peak).zip(finite(l.rms))).map(|(a, b)| a - b);
    let integrated = l.loudness.and_then(|x| finite(x.integrated));
    let plr_db = finite(l.true_peak).zip(integrated).map(|(a, b)| a - b);
    let gated = gated_short_terms(&p.dynamics);
    let short_term = match (
        quantile(&gated, 0.1),
        quantile(&gated, 0.5),
        quantile(&gated, 0.95),
    ) {
        (Some(a), Some(b), Some(c)) => Some([a, b, c]),
        _ => None,
    };
    let lra_lu = short_term
        .filter(|_| gated.len() >= 2)
        .map(|[lo, _, hi]| hi - lo);
    let ev = events(&p.dynamics, rule);
    let shaped: Vec<EventShape> = shapes(&p.dynamics, &ev, rule)
        .into_iter()
        .flatten()
        .collect();
    let enough = shaped.len() >= rule.min_events;
    let mut tsr: Vec<f64> = shaped.iter().map(|s| s.tsr_db).collect();
    let mut attack: Vec<f64> = shaped.iter().map(|s| s.attack_db).collect();
    TrackDynamics {
        crest_db,
        plr_db,
        lra_lu,
        short_term,
        active_seconds: gated.len() as f64 * LOUDNESS_STEP,
        events: ev.len(),
        tsr_db: if enough { median(&mut tsr) } else { None },
        attack_db: if enough { median(&mut attack) } else { None },
    }
}

/// The same events measured on a signal before (`source`) and after
/// processing (`processed`, the same timing): the median change of the
/// transient-to-sustain ratio and of the attack rise (processed − source),
/// over the events both carry, and how many that is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flattening {
    pub events: usize,
    pub source_tsr_db: f64,
    pub processed_tsr_db: f64,
    pub tsr_change_db: f64,
    pub attack_change_db: f64,
}

/// See [`Flattening`] (`None`: fewer than `rule.min_events` events both
/// carry, or the traces' timing differs).
pub fn flattening(
    source: &DynamicsTrace,
    processed: &DynamicsTrace,
    rule: &DynamicsRule,
) -> Option<Flattening> {
    if source.block != processed.block || source.rate != processed.rate {
        return None;
    }
    let ev = events(source, rule);
    let a = shapes(source, &ev, rule);
    let b = shapes(processed, &ev, rule);
    let pairs: Vec<(EventShape, EventShape)> = a
        .into_iter()
        .zip(b)
        .filter_map(|(x, y)| Some((x?, y?)))
        .collect();
    if pairs.len() < rule.min_events {
        return None;
    }
    let mut d_tsr: Vec<f64> = pairs.iter().map(|(x, y)| y.tsr_db - x.tsr_db).collect();
    let mut d_att: Vec<f64> = pairs
        .iter()
        .map(|(x, y)| y.attack_db - x.attack_db)
        .collect();
    let mut s: Vec<f64> = pairs.iter().map(|(x, _)| x.tsr_db).collect();
    let mut p: Vec<f64> = pairs.iter().map(|(_, y)| y.tsr_db).collect();
    Some(Flattening {
        events: pairs.len(),
        source_tsr_db: median(&mut s)?,
        processed_tsr_db: median(&mut p)?,
        tsr_change_db: median(&mut d_tsr)?,
        attack_change_db: median(&mut d_att)?,
    })
}

/// A step in level: where a signal's momentary loudness changes by `change`
/// LU between the 2 s before and the 2 s after `at` (seconds from the
/// start), both sides playing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelStep {
    pub at: f64,
    pub before: f64,
    pub after: f64,
}

impl LevelStep {
    pub fn change(&self) -> f64 {
        self.after - self.before
    }
}

/// The medians of momentary loudness before and after step boundary `k`
/// (100 ms steps; `None` when a side has too few values).
pub fn step_at(momentary: &[f64], k: usize, span: usize) -> Option<(f64, f64)> {
    // Windows wholly before the boundary end at or before it.
    let before_end = k.checked_sub(MOMENTARY_LAG)? + 1;
    let before_start = before_end.checked_sub(span)?;
    let after_end = (k + span).min(momentary.len());
    if after_end < k + span {
        return None;
    }
    let mut b: Vec<f64> = momentary.get(before_start..before_end)?.to_vec();
    let mut a: Vec<f64> = momentary.get(k..after_end)?.to_vec();
    Some((median(&mut b)?, median(&mut a)?))
}

/// Steps of at least `min_lu` (see [`LevelStep`]) on `t`, both sides above
/// `gate` (LUFS). The 2 s medians see one step from many boundaries (a
/// plateau about 2 s wide): each run of boundaries changing the same way
/// is one step, placed at the middle of the boundaries within 1 LU of
/// its largest change.
pub fn level_steps(t: &DynamicsTrace, gate: f64, min_lu: f64) -> Vec<LevelStep> {
    let m = t.momentary();
    let span = (2.0 / LOUDNESS_STEP).round() as usize;
    let at = |k: usize| -> Option<(f64, f64)> {
        let (before, after) = step_at(&m, k, span)?;
        (before > gate && after > gate && (after - before).abs() >= min_lu)
            .then_some((before, after))
    };
    let mut out = Vec::new();
    let mut k = 0;
    while k < m.len() {
        let Some((b0, a0)) = at(k) else {
            k += 1;
            continue;
        };
        let rising = a0 > b0;
        let mut run = vec![(k, b0, a0)];
        k += 1;
        while let Some((b, a)) = at(k).filter(|(b, a)| (a > b) == rising) {
            run.push((k, b, a));
            k += 1;
        }
        let top = run
            .iter()
            .map(|(_, b, a)| (a - b).abs())
            .fold(0.0, f64::max);
        let near: Vec<&(usize, f64, f64)> = run
            .iter()
            .filter(|(_, b, a)| (a - b).abs() >= top - 1.0)
            .collect();
        let mid = near.iter().map(|x| x.0 as f64).sum::<f64>() / near.len().max(1) as f64;
        if let Some(&&(_, before, after)) = near.iter().min_by(|x, y| {
            (x.0 as f64 - mid)
                .abs()
                .total_cmp(&(y.0 as f64 - mid).abs())
        }) {
            out.push(LevelStep {
                at: mid * LOUDNESS_STEP,
                before,
                after,
            });
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::{SR, profile, tone};
    use std::f64::consts::TAU;

    /// Decaying 60 Hz hits every `every` seconds over `len` seconds.
    pub(crate) fn hits(amp: f64, every: f64, len: f64) -> Vec<f32> {
        let rate = f64::from(SR);
        (0..(len * rate) as usize)
            .map(|i| {
                let t = i as f64 / rate;
                let k = t % every;
                (amp * (-k * 12.0).exp() * (TAU * 60.0 * k).sin()) as f32
            })
            .collect()
    }

    /// A crude fast compressor: gain from a peak follower (attack 0.1 ms,
    /// release 30 ms), 8:1 over `threshold`, the makeup setting the peak
    /// to half the input's.
    pub(crate) fn squash(x: &[f32], threshold: f64) -> Vec<f32> {
        let rate = f64::from(SR);
        let (att, rel) = ((-1.0 / (0.0001 * rate)).exp(), (-1.0 / (0.03 * rate)).exp());
        let mut env = 0.0f64;
        let out: Vec<f64> = x
            .iter()
            .map(|v| {
                let a = f64::from(*v).abs();
                env = if a > env {
                    att * env + (1.0 - att) * a
                } else {
                    rel * env + (1.0 - rel) * a
                };
                let over = (env / threshold).max(1.0);
                let gain = over.powf(1.0 / 8.0 - 1.0);
                f64::from(*v) * gain
            })
            .collect();
        let peak_in = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let peak_out = out.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let makeup = f64::from(peak_in) / peak_out.max(1e-9) * 0.5;
        out.into_iter().map(|v| (v * makeup) as f32).collect()
    }

    #[test]
    fn the_trace_keeps_the_envelope_and_the_loudness() {
        let s = tone(1000.0, 0.5, 0.0, 6.0, 6.0);
        let p = profile(&[&s]);
        let t = &p.dynamics;
        assert_eq!(t.block, 240, "5 ms at 48 kHz");
        assert_eq!(t.blocks(), 1200);
        // A steady sine: peak −6 dBFS, RMS −9 dBFS in every full block.
        let i = 600;
        assert!((t.peak_db(i) - -6.02).abs() < 0.05, "{}", t.peak_db(i));
        assert!((t.power_db(i) - -9.03).abs() < 0.05, "{}", t.power_db(i));
        // Loudness every 100 ms: 30 short-term windows fit in 6 s once
        // three seconds have passed.
        assert_eq!(t.short_term().len(), 31);
        assert_eq!(t.momentary().len(), 57);
        // Gated short-terms of a steady sine: no spread.
        let d = measure(&p, &DynamicsRule::default());
        let lra = d.lra_lu.unwrap_or(-1.0);
        assert!(lra.abs() < 0.2, "{lra}");
        assert!(
            (lra - p.levels.loudness.map_or(-9.0, |l| l.range)).abs() < 0.05,
            "the meter's LRA"
        );
        assert!((d.crest_db.unwrap_or(0.0) - 3.01).abs() < 0.05);
        assert_eq!(d.events, 0, "a steady tone has no events");
        assert_eq!(d.tsr_db, None);
        // Silence: nothing measurable, nothing invented.
        let z = vec![0.0f32; SR as usize * 4];
        let d = measure(&profile(&[&z]), &DynamicsRule::default());
        assert_eq!(d.crest_db, None);
        assert_eq!(d.lra_lu, None);
        assert_eq!(d.events, 0);
        assert_eq!(d.active_seconds, 0.0);
    }

    #[test]
    fn hits_have_events_and_their_transient_to_sustain() {
        let rule = DynamicsRule::default();
        let x = hits(0.8, 0.5, 10.0);
        let p = profile(&[&x]);
        let ev = events(&p.dynamics, &rule);
        assert_eq!(ev.len(), 19, "every hit after the first: {ev:?}");
        for (k, e) in ev.iter().enumerate() {
            let at = *e as f64 * 0.005;
            let want = 0.5 * (k + 1) as f64;
            assert!((at - want).abs() < 0.011, "{at} {want}");
        }
        let d = measure(&p, &rule);
        // Peak 0.8 at the start; 60–200 ms later the decay is 2–11 dB
        // further down and a sine's RMS 3 dB under its peak.
        let tsr = d.tsr_db.unwrap_or(0.0);
        assert!((10.0..16.0).contains(&tsr), "{tsr}");
        assert!(d.attack_db.unwrap_or(0.0) > 30.0, "{:?}", d.attack_db);
        // Squashed: the attacks no longer stand out as far.
        let c = squash(&x, 0.05);
        let pc = profile(&[&c]);
        let dc = measure(&pc, &rule);
        assert!(
            dc.tsr_db.unwrap_or(99.0) < tsr - 4.0,
            "{:?} vs {tsr}",
            dc.tsr_db
        );
        let f = flattening(&p.dynamics, &pc.dynamics, &rule).unwrap_or(Flattening {
            events: 0,
            source_tsr_db: 0.0,
            processed_tsr_db: 0.0,
            tsr_change_db: 0.0,
            attack_change_db: 0.0,
        });
        assert!(f.events >= 18 && f.tsr_change_db < -4.0, "{f:?}");
        // Only quieter: nothing changes in the shape.
        let quiet: Vec<f32> = x.iter().map(|v| v * 0.25).collect();
        let pq = profile(&[&quiet]);
        let f = flattening(&p.dynamics, &pq.dynamics, &rule).unwrap_or(Flattening {
            events: 0,
            source_tsr_db: 0.0,
            processed_tsr_db: 0.0,
            tsr_change_db: 9.0,
            attack_change_db: 9.0,
        });
        assert!(f.tsr_change_db.abs() < 0.1, "{f:?}");
    }

    #[test]
    fn steps_and_ranges_of_loudness() {
        // 12 s at −30 dBFS, then 12 s at −10.
        let a = tone(500.0, 0.0447, 0.0, 12.0, 24.0);
        let b = tone(500.0, 0.447, 12.0, 24.0, 24.0);
        let x: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let p = profile(&[&x]);
        let d = measure(&p, &DynamicsRule::default());
        let lra = d.lra_lu.unwrap_or(0.0);
        assert!((18.0..21.0).contains(&lra), "{lra}");
        let [lo, mid, hi] = d.short_term.unwrap_or([0.0; 3]);
        assert!(lo < mid && mid <= hi && hi - lo > 18.0);
        let steps = level_steps(&p.dynamics, -70.0, 10.0);
        assert_eq!(steps.len(), 1, "{steps:?}");
        assert!((steps[0].at - 12.0).abs() < 0.3, "{:?}", steps[0]);
        assert!((steps[0].change() - 20.0).abs() < 0.5, "{:?}", steps[0]);
        // A steady tone: none.
        let s = tone(500.0, 0.3, 0.0, 24.0, 24.0);
        assert!(level_steps(&profile(&[&s]).dynamics, -70.0, 10.0).is_empty());
        assert_eq!(quantile(&[], 0.5), None);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), Some(2.5));
    }
}
