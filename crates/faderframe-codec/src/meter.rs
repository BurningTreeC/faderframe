//! What the codec does to the master, measured as it plays (and offline):
//! the true peak of the decoded signal after the platform's gain (BS.1770
//! 4× oversampling, [`faderframe_analysis::TruePeak`]) with the frames that
//! go over full scale counted, and the energy of what the codec removed or
//! added (the original less the decoded) against the original's.
//!
//! Allocation free once made: it runs on the audio thread.

use crate::chain::CHUNK;
use faderframe_analysis::TruePeak;

/// The measurements of one stretch of decoded audio.
#[derive(Clone, Debug)]
pub struct CodecMeter {
    tp: Vec<TruePeak>,
    /// Per frame of a chunk: the highest true peak of any channel.
    frame_peak: Box<[f32]>,
    /// The highest true peak (linear).
    pub peak: f64,
    /// Frames whose true peak went over full scale (0 dBTP).
    pub overs: u64,
    /// Energy of the difference (original − decoded) and of the original,
    /// both at the gain heard, over every channel.
    pub difference: f64,
    pub original: f64,
    /// Frames measured.
    pub frames: u64,
}

impl CodecMeter {
    pub fn new(channels: usize) -> Self {
        Self {
            tp: vec![TruePeak::new(); channels.max(1)],
            frame_peak: vec![0.0; CHUNK].into_boxed_slice(),
            peak: 0.0,
            overs: 0,
            difference: 0.0,
            original: 0.0,
            frames: 0,
        }
    }

    /// Everything measured so far forgotten.
    pub fn reset(&mut self) {
        for t in &mut self.tp {
            *t = TruePeak::new();
        }
        self.peak = 0.0;
        self.overs = 0;
        self.difference = 0.0;
        self.original = 0.0;
        self.frames = 0;
    }

    /// `n` frames (at most [`CHUNK`]) of the decoded (`wet`) and the
    /// original (`dry`) channels, heard at a gain moving from `g0` to `g1`
    /// over them (linear).
    pub fn add<'a>(
        &mut self,
        n: usize,
        wet: impl Fn(usize) -> &'a [f32],
        dry: impl Fn(usize) -> &'a [f32],
        g0: f32,
        g1: f32,
    ) {
        let n = n.min(CHUNK);
        if n == 0 {
            return;
        }
        let step = (g1 - g0) / n as f32;
        self.frame_peak[..n].fill(0.0);
        let (mut diff, mut orig) = (0.0f64, 0.0f64);
        for (c, tp) in self.tp.iter_mut().enumerate() {
            let (w, d) = (&wet(c)[..n], &dry(c)[..n]);
            let mut g = g0;
            for i in 0..n {
                g += step;
                let heard = w[i] * g;
                let p = tp.run(heard) as f32;
                if p > self.frame_peak[i] {
                    self.frame_peak[i] = p;
                }
                let o = d[i] * g;
                let e = o - heard;
                diff += f64::from(e * e);
                orig += f64::from(o * o);
            }
        }
        for &p in &self.frame_peak[..n] {
            if p > 1.0 {
                self.overs += 1;
            }
            self.peak = self.peak.max(f64::from(p));
        }
        self.difference += diff;
        self.original += orig;
        self.frames += n as u64;
    }

    /// The highest true peak (dBTP; −∞ for silence).
    pub fn peak_dbtp(&self) -> f64 {
        db(self.peak)
    }

    /// What the codec removed or added against the original (dB; −∞ when
    /// nothing differs, `None` before anything was heard).
    pub fn difference_db(&self) -> Option<f64> {
        relative_db(self.difference, self.original)
    }
}

/// Below this an amplitude is silence (−200 dB): a decoder's output for
/// silence is not always exactly zero, and −670 dBTP reads as a fault.
pub const SILENT: f64 = 1e-10;

/// Amplitude in dB (−∞ for silence, below [`SILENT`]).
pub fn db(linear: f64) -> f64 {
    if linear >= SILENT {
        20.0 * linear.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// `part` against `whole` as energies, in dB (`None` when `whole` is
/// silence).
pub fn relative_db(part: f64, whole: f64) -> Option<f64> {
    (whole > 1e-20).then(|| {
        if part > 0.0 {
            10.0 * (part / whole).log10()
        } else {
            f64::NEG_INFINITY
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inter_sample_peak_above_full_scale_is_counted() {
        // A tone at fs/4 sampled 45° off its peaks: every sample at 0.707
        // of the amplitude, the peaks between them.
        let n = 1024;
        let amp = 1.2f32;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                amp * (std::f32::consts::FRAC_PI_2 * i as f32 + std::f32::consts::FRAC_PI_4).sin()
            })
            .collect();
        assert!(x.iter().all(|v| v.abs() < 0.86));
        let mut m = CodecMeter::new(1);
        m.add(n, |_| &x, |_| &x, 1.0, 1.0);
        assert!(m.peak_dbtp() > 1.0, "true peak {:.2} dBTP", m.peak_dbtp());
        assert!(m.overs > n as u64 / 3, "{} overs", m.overs);
        assert_eq!(m.difference_db(), Some(f64::NEG_INFINITY));
        // Turned down by the platform: no overs.
        let mut m = CodecMeter::new(1);
        m.add(n, |_| &x, |_| &x, 0.5, 0.5);
        assert_eq!(m.overs, 0);
        assert!(m.peak_dbtp() < -3.0);
        m.reset();
        assert_eq!(m.frames, 0);
        assert_eq!(m.difference_db(), None);
    }

    #[test]
    fn a_decoded_silence_reads_as_silence() {
        assert_eq!(db(0.0), f64::NEG_INFINITY);
        assert_eq!(db(2.3e-34), f64::NEG_INFINITY);
        assert!((db(SILENT) + 200.0).abs() < 1e-9);
        assert!((db(1.0)).abs() < 1e-12);
    }

    #[test]
    fn the_difference_is_measured_against_the_original() {
        let n = 1000;
        let dry: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let wet: Vec<f32> = dry.iter().map(|v| v * 0.9).collect();
        let mut m = CodecMeter::new(1);
        m.add(n, |_| &wet, |_| &dry, 1.0, 1.0);
        let d = m.difference_db().unwrap();
        assert!((d - (-20.0)).abs() < 0.01, "{d}");
    }
}
