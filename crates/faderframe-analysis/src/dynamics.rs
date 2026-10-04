//! Dynamics of a measurement: the crest factor and the DR value.
//!
//! * Crest factor — the sample peak over the RMS of everything measured
//!   (dB; a sine reads 3.0, a square 0).
//! * DR value — the Pleasurize Music Foundation's / TT Dynamic Range
//!   Meter's figure: the audio in 3 s blocks; per channel each block's
//!   peak and RMS (× √2, so a full-scale sine reads 0 dBFS); the channel's
//!   DR is its second highest block peak over the RMS of its loudest 20 %
//!   of blocks; the value is the channels' mean, shown rounded ("DR8").
//!
//! The loudness range (LRA) and the peak-to-loudness ratio (PLR) come with
//! the loudness ([`crate::LoudnessMeter`]).

/// What a [`DynamicsMeter`] reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dynamics {
    /// dB; `None` until something above silence was measured.
    pub crest: Option<f64>,
    /// dB (unrounded); `None` until a block was measured.
    pub dr: Option<f64>,
}

impl Dynamics {
    /// The DR value as the meters show it ("DR8").
    pub fn dr_value(&self) -> Option<i32> {
        self.dr.map(|d| d.round().max(0.0) as i32)
    }
}

/// Seconds a DR block lasts.
const BLOCK_SECONDS: f64 = 3.0;
/// The share of loudest blocks the DR's RMS is taken over.
const LOUDEST: f64 = 0.2;
/// The shortest last block that still counts (seconds).
const MIN_PARTIAL: f64 = 0.5;

#[derive(Clone, Debug)]
pub struct DynamicsMeter {
    rate: f64,
    block: usize,
    /// Per channel: the current block's sum of squares and peak.
    sum: [f64; 2],
    peak: [f64; 2],
    fill: usize,
    /// Per channel, finished blocks: (RMS × √2, peak).
    blocks: [Vec<(f64, f64)>; 2],
    /// Everything measured: sum of squares (both channels), frames, peak.
    total_sum: f64,
    total_frames: u64,
    total_peak: f64,
}

impl DynamicsMeter {
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(8_000));
        Self {
            rate,
            block: (BLOCK_SECONDS * rate).round() as usize,
            sum: [0.0; 2],
            peak: [0.0; 2],
            fill: 0,
            blocks: [Vec::new(), Vec::new()],
            total_sum: 0.0,
            total_frames: 0,
            total_peak: 0.0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.rate as u32);
    }

    /// Feed audio (only what is measured: while playing).
    pub fn process(&mut self, left: &[f32], right: &[f32]) {
        for (&l, &r) in left.iter().zip(right) {
            let x = [f64::from(l), f64::from(r)];
            for (c, v) in x.iter().enumerate() {
                self.sum[c] += v * v;
                self.peak[c] = self.peak[c].max(v.abs());
            }
            self.total_sum += x[0] * x[0] + x[1] * x[1];
            self.total_peak = self.total_peak.max(x[0].abs()).max(x[1].abs());
            self.total_frames += 1;
            self.fill += 1;
            if self.fill == self.block {
                self.end_block();
            }
        }
    }

    fn end_block(&mut self) {
        if self.fill == 0 {
            return;
        }
        for c in 0..2 {
            let rms = (2.0 * self.sum[c] / self.fill as f64).sqrt();
            self.blocks[c].push((rms, self.peak[c]));
            self.sum[c] = 0.0;
            self.peak[c] = 0.0;
        }
        self.fill = 0;
    }

    pub fn read(&self) -> Dynamics {
        let crest = (self.total_frames > 0 && self.total_sum > 0.0).then(|| {
            let rms = (self.total_sum / (2 * self.total_frames) as f64).sqrt();
            20.0 * (self.total_peak / rms).log10()
        });
        // The blocks so far and the current one if long enough.
        let partial = self.fill as f64 >= MIN_PARTIAL * self.rate;
        let mut channels = Vec::with_capacity(2);
        for c in 0..2 {
            let mut blocks = self.blocks[c].clone();
            if partial {
                blocks.push(((2.0 * self.sum[c] / self.fill as f64).sqrt(), self.peak[c]));
            }
            if blocks.is_empty() {
                continue;
            }
            let mut peaks: Vec<f64> = blocks.iter().map(|b| b.1).collect();
            peaks.sort_by(|a, b| b.total_cmp(a));
            let second = peaks.get(1).copied().unwrap_or(peaks[0]);
            let mut rms: Vec<f64> = blocks.iter().map(|b| b.0).collect();
            rms.sort_by(|a, b| b.total_cmp(a));
            let n = ((rms.len() as f64 * LOUDEST).round() as usize).clamp(1, rms.len());
            let top = (rms[..n].iter().map(|r| r * r).sum::<f64>() / n as f64).sqrt();
            if top > 0.0 && second > 0.0 {
                channels.push(20.0 * (second / top).log10());
            }
        }
        let dr =
            (!channels.is_empty()).then(|| channels.iter().sum::<f64>() / channels.len() as f64);
        Dynamics { crest, dr }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn feed(m: &mut DynamicsMeter, seconds: f64, f: impl Fn(f64) -> f64) {
        let n = (seconds * 48_000.0) as usize;
        let x: Vec<f32> = (0..n).map(|i| f(i as f64 / 48_000.0) as f32).collect();
        m.process(&x, &x);
    }

    #[test]
    fn a_sine_has_its_crest_and_no_dynamic_range() {
        let mut m = DynamicsMeter::new(48_000);
        assert_eq!(
            m.read(),
            Dynamics {
                crest: None,
                dr: None
            }
        );
        feed(&mut m, 9.0, |t| {
            0.5 * (std::f64::consts::TAU * 1_000.0 * t).sin()
        });
        let d = m.read();
        assert!((d.crest.unwrap() - 3.01).abs() < 0.02, "{:?}", d.crest);
        assert!(d.dr.unwrap().abs() < 0.05, "{:?}", d.dr);
        assert_eq!(d.dr_value(), Some(0));
    }

    #[test]
    fn dr_compares_the_second_peak_with_the_loud_blocks() {
        // Ten blocks of a sine at −20 dBFS, two of them with a full scale
        // click: the second highest peak is 0 dBFS, the loudest 20 % of
        // blocks sit at −20: DR 20.
        let mut m = DynamicsMeter::new(48_000);
        for b in 0..10 {
            feed(&mut m, 3.0, |t| {
                let click = (b == 2 || b == 7) && t < 1.0 / 48_000.0;
                if click {
                    1.0
                } else {
                    0.1 * (std::f64::consts::TAU * 440.0 * t).sin()
                }
            });
        }
        let d = m.read();
        assert!((d.dr.unwrap() - 20.0).abs() < 0.1, "{:?}", d.dr);
        assert_eq!(d.dr_value(), Some(20));
        // One click only: the second peak is the sine's own.
        let mut m = DynamicsMeter::new(48_000);
        for b in 0..10 {
            feed(&mut m, 3.0, |t| {
                if b == 2 && t < 1.0 / 48_000.0 {
                    1.0
                } else {
                    0.1 * (std::f64::consts::TAU * 440.0 * t).sin()
                }
            });
        }
        assert!(m.read().dr.unwrap().abs() < 0.1);
        m.reset();
        assert_eq!(m.read().dr, None);
    }
}
