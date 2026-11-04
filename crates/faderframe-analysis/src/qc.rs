//! What a delivery check measures beyond the levels ([`crate::levels`]),
//! fed piece by piece as a file is read back: silence at the head and the
//! tail and gaps of digital silence inside, where clipped runs are, and
//! for a stereo pair its phase (correlation over the file and in 400 ms
//! windows) and how it sums to mono (the BS.1770 loudness of the mono
//! fold-down against the stereo's) — [`QcMeter`].
//!
//! Stems are checked against the mix they were made from with
//! [`NullMeter`] (the residual of their sum against the mix: peak, RMS,
//! where it starts and where it is worst, in the range and in the tail)
//! and [`best_lag`] (an offset that would null them better).
//!
//! "Silence" is every channel within the format's dither floor
//! ([`silence_floor`]): a dithered file never reads zero, and noise-shaped
//! dither reaches about 10 LSB.

use crate::loudness::WeightedLoudness;

/// The shortest run of digital silence inside a file that counts as a
/// gap (seconds).
pub const GAP_SECONDS: f64 = 1.0;
/// Clipped runs and gaps listed (more are counted).
pub const LISTED: usize = 32;
/// The window the phase is read in (seconds).
pub const PHASE_WINDOW_SECONDS: f64 = 0.4;
/// Windows whose pair is quieter than this (RMS, dBFS) say nothing about
/// the phase and are left out.
pub const PHASE_GATE_DB: f64 = -50.0;
/// The silence floor of fixed-point files, in LSB: TPDF dither peaks at
/// 2, noise-shaped dither at about 10.
pub const DITHER_LSB: f64 = 16.0;
/// The silence floor of floating-point files (−120 dBFS).
pub const FLOAT_FLOOR: f32 = 1e-6;

fn db(v: f64) -> f64 {
    if v > 0.0 {
        20.0 * v.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// The level at or under which a sample counts as silent: the dither
/// floor of a `bits`-bit fixed-point file, or [`FLOAT_FLOOR`].
pub fn silence_floor(bits: u16, float: bool) -> f32 {
    if float || bits < 2 {
        FLOAT_FLOOR
    } else {
        (DITHER_LSB / 2f64.powi(i32::from(bits) - 1)) as f32
    }
}

/// A run of frames (`channel`: the channel it is on, when it is one's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub frames: u64,
    pub channel: Option<usize>,
}

/// The phase of a stereo pair (the first two channels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StereoLevels {
    /// Correlation over the whole file (+1 mono, 0 unrelated, −1 one side
    /// inverted; 0 for silence).
    pub correlation: f64,
    /// The lowest of the windows loud enough to tell, and where it starts
    /// (`None`: no window was).
    pub min_correlation: Option<(f64, u64)>,
    /// The share of those windows with the sides more opposed than alike
    /// (correlation below 0).
    pub negative_share: f64,
    /// Windows loud enough to tell.
    pub windows: u64,
    /// Integrated loudness of the mono fold-down ((L + R) / 2 on both
    /// sides: what a mono player gives at the stereo's level; LUFS).
    pub mono_integrated: f64,
}

/// What [`QcMeter`] measured.
#[derive(Clone, Debug, PartialEq)]
pub struct QcLevels {
    pub frames: u64,
    /// The silence floor used (linear).
    pub floor: f32,
    /// Silence before the first and after the last sample above the
    /// floor (frames; the whole file when it is silent).
    pub head_silence: u64,
    pub tail_silence: u64,
    /// Nothing rises above the floor.
    pub silent: bool,
    /// Digital silence of [`GAP_SECONDS`] or more between the first and
    /// the last sound (the first [`LISTED`]) and how many there are.
    pub gaps: Vec<Span>,
    pub gap_count: u64,
    /// Runs of [`crate::levels::OVER_RUN`] or more samples at full scale
    /// (the first [`LISTED`]), how many, and the longest.
    pub clip_runs: Vec<Span>,
    pub clip_run_count: u64,
    pub longest_clip_run: Option<Span>,
    /// A stereo file's phase (`None`: not two channels).
    pub stereo: Option<StereoLevels>,
}

#[derive(Clone, Debug, Default)]
struct Run {
    start: u64,
    len: u64,
}

#[derive(Clone, Debug)]
struct Pair {
    window: u64,
    gate: f64,
    // The window being filled.
    fill: u64,
    ll: f64,
    rr: f64,
    lr: f64,
    window_start: u64,
    // The whole file.
    all_ll: f64,
    all_rr: f64,
    all_lr: f64,
    windows: u64,
    negative: u64,
    min: Option<(f64, u64)>,
    mono: WeightedLoudness,
    mid: Vec<f32>,
}

impl Pair {
    fn end_window(&mut self) {
        let n = self.fill.max(1) as f64;
        let energy = (self.ll + self.rr) / (2.0 * n);
        if energy > self.gate && self.ll > 0.0 && self.rr > 0.0 {
            let r = self.lr / (self.ll * self.rr).sqrt();
            self.windows += 1;
            if r < 0.0 {
                self.negative += 1;
            }
            if self.min.is_none_or(|(m, _)| r < m) {
                self.min = Some((r, self.window_start));
            }
        }
        self.all_ll += self.ll;
        self.all_rr += self.rr;
        self.all_lr += self.lr;
        self.window_start += self.fill;
        self.fill = 0;
        self.ll = 0.0;
        self.rr = 0.0;
        self.lr = 0.0;
    }
}

/// Measures what [`QcLevels`] holds as a file is read (see the module).
#[derive(Clone, Debug)]
pub struct QcMeter {
    floor: f32,
    full_scale: f32,
    min_gap: u64,
    frames: u64,
    first_sound: Option<u64>,
    last_sound: Option<u64>,
    gaps: Vec<Span>,
    gap_count: u64,
    clip: Vec<Run>,
    clip_runs: Vec<Span>,
    clip_run_count: u64,
    longest: Option<Span>,
    pair: Option<Box<Pair>>,
}

impl QcMeter {
    /// `channels` channels at `sample_rate`; `full_scale` the format's
    /// largest magnitude ([`crate::levels::full_scale`]), `floor` its
    /// silence ([`silence_floor`]).
    pub fn new(sample_rate: u32, channels: usize, full_scale: f32, floor: f32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        Self {
            floor,
            full_scale,
            min_gap: (GAP_SECONDS * rate).round() as u64,
            frames: 0,
            first_sound: None,
            last_sound: None,
            gaps: Vec::new(),
            gap_count: 0,
            clip: vec![Run::default(); channels],
            clip_runs: Vec::new(),
            clip_run_count: 0,
            longest: None,
            pair: (channels == 2).then(|| {
                Box::new(Pair {
                    window: (PHASE_WINDOW_SECONDS * rate).round().max(1.0) as u64,
                    gate: 10f64.powf(PHASE_GATE_DB / 10.0),
                    fill: 0,
                    ll: 0.0,
                    rr: 0.0,
                    lr: 0.0,
                    window_start: 0,
                    all_ll: 0.0,
                    all_rr: 0.0,
                    all_lr: 0.0,
                    windows: 0,
                    negative: 0,
                    min: None,
                    // The fold-down on both sides: one channel weighing 2.
                    mono: WeightedLoudness::new(sample_rate, &[2.0]),
                    mid: Vec::new(),
                })
            }),
        }
    }

    fn end_clip_run(&mut self, channel: usize) {
        let run = std::mem::take(&mut self.clip[channel]);
        if run.len < crate::levels::OVER_RUN as u64 {
            return;
        }
        let span = Span {
            start: run.start,
            frames: run.len,
            channel: Some(channel),
        };
        self.clip_run_count += 1;
        if self.clip_runs.len() < LISTED {
            self.clip_runs.push(span);
        }
        if self.longest.is_none_or(|l| span.frames > l.frames) {
            self.longest = Some(span);
        }
    }

    /// Feed planar audio (`audio[c]`, equal lengths).
    pub fn add(&mut self, audio: &[&[f32]]) {
        let frames = audio.iter().map(|c| c.len()).min().unwrap_or(0);
        let start = self.frames;
        for i in 0..frames {
            let at = start + i as u64;
            let mut sound = false;
            for (c, ch) in audio.iter().enumerate() {
                let a = ch[i].abs();
                sound |= a > self.floor;
                if a >= self.full_scale {
                    let run = &mut self.clip[c];
                    if run.len == 0 {
                        run.start = at;
                    }
                    run.len += 1;
                } else if self.clip[c].len > 0 {
                    self.end_clip_run(c);
                }
            }
            if sound {
                // A quiet stretch since the last sound: a gap when it is
                // long enough (silence at the head is not one).
                if let Some(last) = self.last_sound {
                    let quiet = at - last - 1;
                    if quiet >= self.min_gap {
                        self.gap_count += 1;
                        if self.gaps.len() < LISTED {
                            self.gaps.push(Span {
                                start: last + 1,
                                frames: quiet,
                                channel: None,
                            });
                        }
                    }
                }
                self.first_sound.get_or_insert(at);
                self.last_sound = Some(at);
            }
        }
        if let Some(p) = &mut self.pair {
            let (l, r) = (&audio[0][..frames], &audio[1][..frames]);
            for (&a, &b) in l.iter().zip(r) {
                let (a, b) = (f64::from(a), f64::from(b));
                p.ll += a * a;
                p.rr += b * b;
                p.lr += a * b;
                p.fill += 1;
                if p.fill == p.window {
                    p.end_window();
                }
            }
            p.mid.clear();
            p.mid.extend(l.iter().zip(r).map(|(a, b)| (a + b) * 0.5));
            let mid = std::mem::take(&mut p.mid);
            p.mono.add(&[&mid]);
            p.mid = mid;
        }
        self.frames += frames as u64;
    }

    /// What was measured (the runs still open end with the file).
    pub fn finish(mut self) -> QcLevels {
        for c in 0..self.clip.len() {
            self.end_clip_run(c);
        }
        let stereo = self.pair.take().map(|mut p| {
            if p.fill > 0 {
                p.end_window();
            }
            let correlation = if p.all_ll > 0.0 && p.all_rr > 0.0 {
                p.all_lr / (p.all_ll * p.all_rr).sqrt()
            } else {
                0.0
            };
            StereoLevels {
                correlation,
                min_correlation: p.min,
                negative_share: if p.windows > 0 {
                    p.negative as f64 / p.windows as f64
                } else {
                    0.0
                },
                windows: p.windows,
                mono_integrated: p.mono.integrated(),
            }
        });
        let (head, tail) = match (self.first_sound, self.last_sound) {
            (Some(a), Some(b)) => (a, self.frames - b - 1),
            _ => (self.frames, self.frames),
        };
        QcLevels {
            frames: self.frames,
            floor: self.floor,
            head_silence: head,
            tail_silence: tail,
            silent: self.first_sound.is_none(),
            gaps: self.gaps,
            gap_count: self.gap_count,
            clip_runs: self.clip_runs,
            clip_run_count: self.clip_run_count,
            longest_clip_run: self.longest,
            stereo,
        }
    }
}

/// The residual of stems summed against the mix they were made from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NullLevels {
    pub frames: u64,
    /// Highest residual sample (dBFS) and where (frame, channel).
    pub peak: f64,
    pub peak_at: u64,
    pub peak_channel: usize,
    /// RMS of the residual over every channel (dBFS).
    pub rms: f64,
    /// RMS of the mix (dBFS): the residual against it is how deep the
    /// stems cancel the mix.
    pub mix_rms: f64,
    /// The first frame the residual is above the threshold the meter was
    /// given (`None`: never).
    pub first_at: Option<u64>,
    /// The highest residual before and after the end of the range (the
    /// tail) (dBFS).
    pub range_peak: f64,
    pub tail_peak: f64,
}

impl NullLevels {
    /// How far the residual's RMS is below the mix's (dB; infinite when
    /// the sum nulls exactly).
    pub fn depth(&self) -> f64 {
        self.mix_rms - self.rms
    }
}

/// Compares the sum of stems with the mix piece by piece (see the module).
#[derive(Clone, Debug)]
pub struct NullMeter {
    threshold: f64,
    range_end: u64,
    frames: u64,
    peak: f64,
    peak_at: u64,
    peak_channel: usize,
    sum_square: f64,
    mix_square: f64,
    samples: u64,
    first_at: Option<u64>,
    range_peak: f64,
    tail_peak: f64,
}

impl NullMeter {
    /// `threshold`: the residual (linear) above which a sample does not
    /// null; frames from `range_end` on are the tail.
    pub fn new(threshold: f64, range_end: u64) -> Self {
        Self {
            threshold,
            range_end,
            frames: 0,
            peak: 0.0,
            peak_at: 0,
            peak_channel: 0,
            sum_square: 0.0,
            mix_square: 0.0,
            samples: 0,
            first_at: None,
            range_peak: 0.0,
            tail_peak: 0.0,
        }
    }

    /// Feed the stems' sum and the mix (planar, the same channels and
    /// lengths; summed in double precision by the caller).
    pub fn add(&mut self, sum: &[&[f64]], mix: &[&[f32]]) {
        let frames = sum
            .iter()
            .map(|c| c.len())
            .chain(mix.iter().map(|c| c.len()))
            .min()
            .unwrap_or(0);
        for (c, (s, m)) in sum.iter().zip(mix).enumerate() {
            for i in 0..frames {
                let at = self.frames + i as u64;
                let mv = f64::from(m[i]);
                let r = (s[i] - mv).abs();
                self.sum_square += r * r;
                self.mix_square += mv * mv;
                if r > self.peak {
                    self.peak = r;
                    self.peak_at = at;
                    self.peak_channel = c;
                }
                if r > self.threshold && self.first_at.is_none_or(|f| at < f) {
                    self.first_at = Some(at);
                }
                if at < self.range_end {
                    self.range_peak = self.range_peak.max(r);
                } else {
                    self.tail_peak = self.tail_peak.max(r);
                }
            }
            self.samples += frames as u64;
        }
        self.frames += frames as u64;
    }

    pub fn finish(&self) -> NullLevels {
        let n = self.samples.max(1) as f64;
        NullLevels {
            frames: self.frames,
            peak: db(self.peak),
            peak_at: self.peak_at,
            peak_channel: self.peak_channel,
            rms: db((self.sum_square / n).sqrt()),
            mix_rms: db((self.mix_square / n).sqrt()),
            first_at: self.first_at,
            range_peak: db(self.range_peak),
            tail_peak: db(self.tail_peak),
        }
    }
}

/// The offset of `sum` against `mix` (each one channel of the same
/// excerpt) within ±`max_lag` frames that leaves the least residual, and
/// that residual's energy against the residual's at no offset (below 1
/// when shifting helps). `Some(lag)` positive: the sum is late by `lag`
/// frames. `None` when there is nothing to compare.
pub fn best_lag(sum: &[f64], mix: &[f32], max_lag: usize) -> Option<(isize, f64)> {
    let n = sum.len().min(mix.len());
    if n <= 2 * max_lag + 1 {
        return None;
    }
    // The mix's middle against the sum shifted by `lag`.
    let middle = &mix[max_lag..n - max_lag];
    let shifted = |lag: isize| &sum[(max_lag as isize + lag) as usize..];
    let residual = |lag: isize| {
        middle
            .iter()
            .zip(shifted(lag))
            .map(|(m, s)| {
                let r = s - f64::from(*m);
                r * r
            })
            .sum::<f64>()
    };
    let at_zero = residual(0);
    if at_zero <= 0.0 {
        return None;
    }
    // The cross-correlation's peak first (cheap), then the residual there.
    let mut best = (0isize, f64::NEG_INFINITY);
    for lag in -(max_lag as isize)..=max_lag as isize {
        let c: f64 = middle
            .iter()
            .zip(shifted(lag))
            .map(|(m, s)| s * f64::from(*m))
            .sum();
        if c > best.1 {
            best = (lag, c);
        }
    }
    Some((best.0, residual(best.0) / at_zero))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sine(hz: f32, amp: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| amp * (i as f32 * hz * std::f32::consts::TAU / RATE as f32).sin())
            .collect()
    }

    fn measure(chs: &[Vec<f32>], floor: f32) -> QcLevels {
        let mut m = QcMeter::new(RATE, chs.len(), 1.0, floor);
        let n = chs[0].len();
        let mut at = 0;
        // In uneven pieces, as files are read.
        for piece in [1000, 7777, 65_536].iter().cycle() {
            if at >= n {
                break;
            }
            let end = (at + piece).min(n);
            let parts: Vec<&[f32]> = chs.iter().map(|c| &c[at..end]).collect();
            m.add(&parts);
            at = end;
        }
        m.finish()
    }

    #[test]
    fn silence_at_the_ends_and_gaps_inside_are_found() {
        // 0.5 s silence, 1 s tone, 1.5 s dither-level noise, 1 s tone,
        // 2 s silence; 16-bit floor.
        let floor = silence_floor(16, false);
        let lsb = 1.0 / 32_768.0;
        let mut x = vec![0.0f32; RATE as usize / 2];
        x.extend(sine(440.0, 0.5, RATE as usize));
        x.extend((0..RATE as usize * 3 / 2).map(|i| if i % 3 == 0 { 9.0 * lsb } else { -lsb }));
        x.extend(sine(440.0, 0.5, RATE as usize));
        x.extend(vec![0.0; 2 * RATE as usize]);
        let q = measure(&[x.clone(), x.clone()], floor);
        assert!(!q.silent);
        // The sine's first sample is 0: the head ends one sample in.
        assert_eq!(q.head_silence, RATE as u64 / 2 + 1);
        assert!((q.tail_silence as i64 - 2 * RATE as i64).abs() <= 2);
        assert_eq!(q.gap_count, 1, "{:?}", q.gaps);
        let g = q.gaps[0];
        assert!((g.start as i64 - RATE as i64 * 3 / 2).abs() <= 2, "{g:?}");
        assert!((g.frames as i64 - RATE as i64 * 3 / 2).abs() <= 4, "{g:?}");
        // A short pause is no gap.
        let mut y = sine(440.0, 0.5, RATE as usize);
        y.extend(vec![0.0; RATE as usize / 2]);
        y.extend(sine(440.0, 0.5, RATE as usize));
        assert_eq!(measure(&[y], floor).gap_count, 0);
        // Silence throughout.
        let s = measure(&[vec![0.0; 1000]], floor);
        assert!(s.silent && s.head_silence == 1000 && s.tail_silence == 1000);
    }

    #[test]
    fn clipped_runs_are_placed() {
        let mut x = sine(100.0, 1.5, RATE as usize / 10)
            .into_iter()
            .map(|v| v.clamp(-1.0, 1.0))
            .collect::<Vec<_>>();
        x.extend(vec![0.2; 100]);
        let q = measure(&[x], 1e-6);
        // Ten cycles: twenty rails.
        assert_eq!(q.clip_run_count, 20);
        let first = q.clip_runs[0];
        assert_eq!(first.channel, Some(0));
        // The first rail starts where sin(…)·1.5 first reaches 1.
        let expect = (RATE as f64 / 100.0 * (1.0f64 / 1.5).asin() / std::f64::consts::TAU).ceil();
        assert!(
            (first.start as f64 - expect).abs() <= 1.0,
            "{first:?} vs {expect}"
        );
        assert!(q.longest_clip_run.unwrap().frames >= first.frames);
    }

    #[test]
    fn the_phase_and_the_mono_fold_down_are_read() {
        let n = RATE as usize * 4;
        let a = sine(220.0, 0.5, n);
        // Mono on both sides: correlation 1, nothing lost in mono.
        let q = measure(&[a.clone(), a.clone()], 1e-6);
        let st = q.stereo.unwrap();
        assert!((st.correlation - 1.0).abs() < 1e-9);
        assert_eq!(st.negative_share, 0.0);
        // One side inverted: −1, and the fold-down is silent.
        let inv: Vec<f32> = a.iter().map(|v| -v).collect();
        let st = measure(&[a.clone(), inv], 1e-6).stereo.unwrap();
        assert!((st.correlation + 1.0).abs() < 1e-9);
        assert_eq!(st.negative_share, 1.0);
        assert_eq!(st.mono_integrated, f64::NEG_INFINITY);
        let (min, _) = st.min_correlation.unwrap();
        assert!((min + 1.0).abs() < 1e-6);
        // Unrelated sides: about 0, the fold-down 3 dB under the stereo.
        let b = sine(331.0, 0.5, n);
        let st = measure(&[a.clone(), b.clone()], 1e-6).stereo.unwrap();
        assert!(st.correlation.abs() < 0.01, "{}", st.correlation);
        let mut stereo = WeightedLoudness::new(RATE, &[1.0, 1.0]);
        stereo.add(&[&a, &b]);
        let loss = st.mono_integrated - stereo.integrated();
        assert!((loss + 3.0).abs() < 0.3, "{loss}");
        // A mono file has none.
        assert!(measure(&[a], 1e-6).stereo.is_none());
    }

    #[test]
    fn a_residual_is_measured_where_it_is() {
        let n = RATE as usize;
        let mix = sine(440.0, 0.5, n);
        let mut sum: Vec<f64> = mix.iter().map(|&v| f64::from(v)).collect();
        let mut m = NullMeter::new(1e-6, n as u64 - 1000);
        m.add(&[&sum], &[&mix]);
        let exact = m.finish();
        assert_eq!(exact.peak, f64::NEG_INFINITY);
        assert_eq!(exact.first_at, None);
        assert!(exact.depth().is_infinite());
        // A click at 0.75 s, a smaller one in the tail.
        sum[36_000] += 0.01;
        sum[n - 10] += 0.001;
        let mut m = NullMeter::new(1e-6, n as u64 - 1000);
        for k in (0..n).step_by(4096) {
            let e = (k + 4096).min(n);
            m.add(&[&sum[k..e]], &[&mix[k..e]]);
        }
        let r = m.finish();
        assert_eq!((r.peak_at, r.first_at), (36_000, Some(36_000)));
        assert!((r.peak + 40.0).abs() < 0.01);
        assert!((r.range_peak + 40.0).abs() < 0.01 && (r.tail_peak + 60.0).abs() < 0.01);
        assert!(r.depth() > 40.0);
    }

    #[test]
    fn a_late_sum_is_found_late() {
        let mix: Vec<f32> = (0..20_000)
            .map(|i| ((i as f32 * 0.37).sin() * (i as f32 * 0.011).cos()) * 0.3)
            .collect();
        // The stems arrive 17 frames late.
        let sum: Vec<f64> = (0..20_000)
            .map(|i| if i >= 17 { f64::from(mix[i - 17]) } else { 0.0 })
            .collect();
        let (lag, left) = best_lag(&sum, &mix, 64).unwrap();
        assert_eq!(lag, 17);
        assert!(left < 1e-6, "{left}");
        assert!(best_lag(&sum[..100], &mix[..100], 64).is_none());
    }
}
