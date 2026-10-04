//! Mastering for delivery: what a finished render goes through before it
//! is written ([`Finish`]: loudness normalisation and true-peak limiting)
//! and the delivery presets (target, ceiling, format, rate and dither for
//! the usual destinations). Album exports use the same.

use faderframe_analysis::delivery::{
    LOOKAHEAD_MS, RELEASE_MS, apply_gain, limit_true_peak, measure, normalize_loudness,
};
pub use faderframe_analysis::delivery::{LoudnessReport, PeakHandling};
use faderframe_audio_files::{Dither, WavFormat};

/// Level processing of a finished mix.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Finish {
    /// Integrated loudness target (LUFS).
    pub loudness: Option<f32>,
    /// True-peak ceiling (dBTP).
    pub ceiling: Option<f32>,
    /// Over the ceiling after the loudness gain: limit, or use less gain.
    pub peaks: PeakHandling,
}

/// What [`Finish::apply`] did, and how the result measures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Finished {
    /// Gain applied (dB).
    pub gain_db: f64,
    /// Largest limiter gain reduction (dB).
    pub limited_db: f64,
    pub report: LoudnessReport,
}

impl Finish {
    pub fn is_none(&self) -> bool {
        self.loudness.is_none() && self.ceiling.is_none()
    }

    pub fn apply(&self, audio: &mut [Vec<f32>], sample_rate: u32) -> Finished {
        match (self.loudness, self.ceiling) {
            (Some(target), Some(ceiling)) => {
                let n = normalize_loudness(
                    audio,
                    sample_rate,
                    target as f64,
                    ceiling as f64,
                    self.peaks,
                );
                Finished {
                    gain_db: n.gain_db,
                    limited_db: n.limited_db,
                    report: n.after,
                }
            }
            (Some(target), None) => {
                let before = measure(audio, sample_rate);
                let gain_db = if before.integrated.is_finite() {
                    target as f64 - before.integrated
                } else {
                    0.0
                };
                apply_gain(audio, gain_db);
                Finished {
                    gain_db,
                    limited_db: 0.0,
                    report: measure(audio, sample_rate),
                }
            }
            (None, Some(ceiling)) => {
                let limited_db =
                    limit_true_peak(audio, sample_rate, ceiling as f64, LOOKAHEAD_MS, RELEASE_MS);
                Finished {
                    gain_db: 0.0,
                    limited_db,
                    report: measure(audio, sample_rate),
                }
            }
            (None, None) => Finished {
                gain_db: 0.0,
                limited_db: 0.0,
                report: measure(audio, sample_rate),
            },
        }
    }
}

/// Settings for a common destination.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeliveryPreset {
    pub name: &'static str,
    pub finish: Finish,
    pub format: WavFormat,
    /// `None`: the project's rate.
    pub sample_rate: Option<u32>,
    pub dither: Dither,
}

const fn preset(
    name: &'static str,
    loudness: Option<f32>,
    ceiling: f32,
    format: WavFormat,
    sample_rate: Option<u32>,
    dither: Dither,
) -> DeliveryPreset {
    DeliveryPreset {
        name,
        finish: Finish {
            loudness,
            ceiling: Some(ceiling),
            peaks: PeakHandling::Limit,
        },
        format,
        sample_rate,
        dither,
    }
}

/// Delivery presets (the loudness targets match the Tools view's).
pub const DELIVERY_PRESETS: [DeliveryPreset; 6] = [
    preset(
        "Streaming · Spotify, YouTube, Tidal (−14 LUFS, −1 dBTP)",
        Some(-14.0),
        -1.0,
        WavFormat::Pcm24,
        None,
        Dither::Tpdf,
    ),
    preset(
        "Apple Music (−16 LUFS, −1 dBTP)",
        Some(-16.0),
        -1.0,
        WavFormat::Pcm24,
        None,
        Dither::Tpdf,
    ),
    preset(
        "CD · 16-bit 44.1 kHz, noise-shaped dither (−0.3 dBTP)",
        None,
        -0.3,
        WavFormat::Pcm16,
        Some(44_100),
        Dither::Shaped,
    ),
    preset(
        "EBU R128 broadcast (−23 LUFS, −1 dBTP, 48 kHz)",
        Some(-23.0),
        -1.0,
        WavFormat::Pcm24,
        Some(48_000),
        Dither::Tpdf,
    ),
    preset(
        "ATSC A/85 broadcast (−24 LUFS, −2 dBTP, 48 kHz)",
        Some(-24.0),
        -2.0,
        WavFormat::Pcm24,
        Some(48_000),
        Dither::Tpdf,
    ),
    preset(
        "Loud club master (−9 LUFS, −0.5 dBTP)",
        Some(-9.0),
        -0.5,
        WavFormat::Pcm24,
        None,
        Dither::Tpdf,
    ),
];

/// "−14.2 LUFS · LRA 5.1 LU · −1.0 dBTP" (and what finishing did).
pub fn describe(f: &Finished) -> String {
    let r = &f.report;
    let mut s = if r.integrated.is_finite() {
        format!(
            "{:.1} LUFS · LRA {:.1} LU · {:.1} dBTP",
            r.integrated, r.range, r.true_peak
        )
    } else {
        "silent".to_string()
    };
    if f.gain_db.abs() >= 0.05 {
        s += &format!(" · gain {:+.1} dB", f.gain_db);
    }
    if f.limited_db >= 0.05 {
        s += &format!(" · limited {:.1} dB", f.limited_db);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amp: f32) -> Vec<Vec<f32>> {
        let x: Vec<f32> = (0..96_000)
            .map(|i| amp * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0).sin())
            .collect();
        vec![x.clone(), x]
    }

    #[test]
    fn finishing_reaches_the_targets() {
        let streaming = DELIVERY_PRESETS[0].finish;
        let mut a = tone(0.1);
        let f = streaming.apply(&mut a, 48_000);
        assert!((f.report.integrated - -14.0).abs() < 0.1, "{f:?}");
        assert!(f.report.true_peak <= -0.98);
        assert!(describe(&f).starts_with("-14.0 LUFS"), "{}", describe(&f));
        // Ceiling only (CD): a hot tone is limited, nothing else changes.
        let cd = DELIVERY_PRESETS[2].finish;
        let mut b = tone(1.0);
        let g = cd.apply(&mut b, 48_000);
        assert_eq!(g.gain_db, 0.0);
        assert!(g.limited_db > 0.2 && g.report.true_peak <= -0.28, "{g:?}");
        // Nothing to do: measured only.
        let mut c = tone(0.5);
        let before = c.clone();
        let h = Finish::default().apply(&mut c, 48_000);
        assert_eq!(c, before);
        assert!(h.report.integrated.is_finite());
    }
}
