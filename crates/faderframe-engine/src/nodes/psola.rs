//! Pitch-synchronous overlap-add (TD-PSOLA) voices: how pitch-edited clips
//! play. Monophonic audio is cut into grains two periods long around
//! analysis marks one sung period apart (the period from the pitch edit's
//! curve: its sung pitch), and the grains are laid down again one *target*
//! period apart: the pitch is exactly the sung one times the correction,
//! and the formants (each grain's shape) stay where they were. Squeezing
//! or stretching the grains moves the formants on their own. The marks are
//! chosen through the clip's time map, so warping comes along too.
//!
//! Unvoiced audio (no sung pitch) is laid down in 5 ms grains as it was.
//! With nothing corrected the grains rebuild the source exactly (Hann
//! windows a period apart sum to one), so notes left alone sound as
//! recorded. Everything is preallocated; reading the source never blocks
//! (a missing page reads as silence, as for other clips).

use super::clip_player::read_source;
use crate::snapshot::{PitchCurve, WarpedRegion};

/// Lowest sung pitch followed (Hz); lower ones are clamped.
const LOWEST_HZ: f64 = 40.0;
/// Unvoiced audio is cut into grains this far apart (seconds).
const UNVOICED_SECONDS: f64 = 0.005;
/// Formants move at most an octave either way (grains at most twice or
/// half as long).
const FORMANT_RANGE: (f64, f64) = (0.5, 2.0);
/// Overlapped grains waiting to be played (frames, per channel).
const RING: usize = 1 << 15;

pub(crate) struct PsolaVoice {
    channels: usize,
    /// Engine sample rate.
    sr: f64,
    /// Clip key and the next timeline frame this voice will produce.
    pub(crate) bound: Option<(u64, i64)>,
    pub(crate) last_cycle: u64,
    /// Grains overlapped, by clip-relative output frame (mod `RING`).
    ring: Vec<Vec<f32>>,
    /// The next clip-relative output frame to play.
    emitted: i64,
    /// The next grain's place (clip-relative output frame).
    next: f64,
    /// The analysis mark in use (source frame).
    mark: f64,
    /// Source frames read for one grain.
    grain: Vec<Vec<f32>>,
    /// The block played.
    pub(crate) output: Vec<Vec<f32>>,
}

/// One grain: where it goes and what it is made of.
struct Grain {
    /// Output frames from one grain to the next.
    spacing: f64,
    /// Half its length in output frames.
    half: f64,
    /// Source frames per output frame inside it.
    step: f64,
}

impl PsolaVoice {
    /// Buffers for `channels` channels at `sample_rate`, blocks up to
    /// `max_block` (allocates; control thread). Sources up to twice the
    /// engine rate are followed down to [`LOWEST_HZ`].
    pub(crate) fn new(channels: usize, sample_rate: f64, max_block: usize) -> Self {
        let longest = (2.0 * sample_rate / LOWEST_HZ).ceil() as usize;
        // A grain reads up to a (source) period either side, twice that
        // with the formants moved down an octave.
        let grain = 2 * 2 * longest + 8;
        Self {
            channels,
            sr: sample_rate.max(1.0),
            bound: None,
            last_cycle: 0,
            ring: vec![vec![0.0; RING]; channels],
            emitted: 0,
            next: 0.0,
            mark: 0.0,
            grain: vec![vec![0.0; grain]; channels],
            output: vec![vec![0.0; max_block.max(1)]; channels],
        }
    }

    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// Source frames per engine frame without warping.
    fn ratio(w: &WarpedRegion) -> f64 {
        f64::from(w.transpose).max(1e-6)
    }

    /// The sung period at source frame `s` (source frames), and whether a
    /// pitch was sung there.
    fn period(&self, w: &WarpedRegion, curve: &PitchCurve, s: f64) -> (f64, bool) {
        let source_rate = Self::ratio(w) * self.sr;
        let hz = f64::from(curve.at(s).2);
        if hz > 0.0 {
            ((source_rate / hz).min(source_rate / LOWEST_HZ), true)
        } else {
            (source_rate * UNVOICED_SECONDS, false)
        }
    }

    /// The grain laid down at output frame `t`, and the analysis mark it
    /// is cut around (advanced to the one nearest `t`'s source frame).
    fn grain_at(&mut self, w: &WarpedRegion, curve: &PitchCurve, t: f64) -> Grain {
        let ratio = Self::ratio(w);
        let s = w.source_at(t);
        let (p, voiced) = self.period(w, curve, s);
        // The mark nearest `s`: marks are a (sung) period apart, the
        // nearest up to half one ahead (more: the source went back).
        if self.mark > s + p {
            self.mark = s;
        }
        loop {
            let (p, _) = self.period(w, curve, self.mark);
            if self.mark + p / 2.0 >= s {
                break;
            }
            self.mark += p;
        }
        let (st, formant, _) = curve.at(s);
        let r = if voiced {
            2f64.powf(f64::from(st) / 12.0)
        } else {
            1.0
        };
        let mut phi = 2f64.powf(f64::from(formant) / 12.0);
        if !curve.keep_formants {
            phi *= r;
        }
        let phi = phi.clamp(FORMANT_RANGE.0, FORMANT_RANGE.1);
        let period = p / ratio;
        Grain {
            spacing: period / r,
            half: period / phi,
            step: phi * ratio,
        }
    }

    /// Lay down grain `g` at output frame `t`, cut around source frame
    /// `mark`.
    fn add(&mut self, w: &WarpedRegion, t: f64, mark: f64, g: &Grain) {
        let reach = g.half * g.step;
        let from = (mark - reach).floor() as i64 - 2;
        let len = ((2.0 * reach).ceil() as usize + 5).min(self.grain[0].len());
        read_source(w, from, len, &mut self.grain);
        // Hann windows `half` long either side, `spacing` apart, sum to
        // half / spacing: weighted back to one.
        let weight = (g.spacing / g.half) as f32;
        let first = (t - g.half).ceil() as i64;
        let last = (t + g.half).floor() as i64;
        let lo = first.max(self.emitted);
        let hi = last.min(self.emitted + RING as i64 - 1);
        for k in lo..=hi {
            let d = k as f64 - t;
            let win = (0.5 + 0.5 * (std::f64::consts::PI * d / g.half).cos()) as f32 * weight;
            let at = mark + d * g.step - from as f64;
            let slot = (k as usize) & (RING - 1);
            for (ring, src) in self.ring.iter_mut().zip(&self.grain) {
                ring[slot] += win * cubic(&src[..len], at);
            }
        }
    }

    /// Start producing output frame `rel` of `w` (after a jump or a new
    /// clip): grains from far enough back that frame `rel` is whole.
    fn prime(&mut self, w: &WarpedRegion, rel: i64) {
        for r in &mut self.ring {
            r.fill(0.0);
        }
        self.emitted = rel;
        let back = 2.0 * 2.0 * self.sr / LOWEST_HZ;
        self.next = rel as f64 - back;
        self.mark = w.source_at(self.next);
    }

    /// Play `n` frames of `w` from clip-relative output frame `rel` into
    /// `output` (realtime-safe).
    pub(crate) fn process(&mut self, w: &WarpedRegion, curve: &PitchCurve, rel: i64, n: usize) {
        let n = n.min(self.output[0].len());
        if self.bound != Some((w.key, w.region.start + rel)) {
            self.prime(w, rel);
        }
        let end = rel + n as i64;
        loop {
            let t = self.next;
            let g = self.grain_at(w, curve, t);
            if t - g.half >= end as f64 {
                break;
            }
            let mark = self.mark;
            self.add(w, t, mark, &g);
            self.next += g.spacing.max(1.0);
        }
        for (ring, out) in self.ring.iter_mut().zip(self.output.iter_mut()) {
            for (k, o) in out[..n].iter_mut().enumerate() {
                let slot = ((rel + k as i64) as usize) & (RING - 1);
                *o = ring[slot];
                ring[slot] = 0.0;
            }
        }
        self.emitted = end;
        self.bound = Some((w.key, w.region.start + end));
    }
}

/// Catmull-Rom interpolation of `b` at `x` (zero outside).
#[inline]
fn cubic(b: &[f32], x: f64) -> f32 {
    let i = x.floor() as i64;
    let t = (x - i as f64) as f32;
    let at = |k: i64| {
        let j = i + k;
        if j >= 0 && (j as usize) < b.len() {
            b[j as usize]
        } else {
            0.0
        }
    };
    let (p0, p1, p2, p3) = (at(-1), at(0), at(1), at(2));
    p1 + 0.5
        * t
        * (p2 - p0 + t * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + t * (3.0 * (p1 - p2) + p3 - p0)))
}
