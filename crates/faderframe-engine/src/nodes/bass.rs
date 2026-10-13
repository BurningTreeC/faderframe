//! Bass management of the speaker monitoring
//! (`faderframe_core::surround::BassManagement`): every main speaker
//! high-passed at the crossover, the bass taken from them added to the
//! LFE channel (the subwoofer's feed) at the redirect level, the LFE
//! channel band-limited. Linkwitz–Riley 24 dB/octave (two Butterworth
//! sections each), so a main's high and its low add up flat. Switching it
//! on or off crossfades; nothing allocates once made.

use faderframe_audio_graph::AudioBuffer;
use faderframe_core::ChannelLayout;
use faderframe_core::surround::{BassManagement, speakers_of};

/// How long switching bass management on or off crossfades.
const FADE_SECONDS: f32 = 0.02;

/// One second-order section (transposed direct form II, in f64: the
/// crossovers sit low against the sample rate).
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    /// A Butterworth (Q = 1/√2) low- or high-pass at `hz`.
    fn set(&mut self, high: bool, hz: f32, rate: f32) {
        let w = std::f64::consts::TAU * f64::from(hz) / f64::from(rate);
        let (s, c) = w.sin_cos();
        let alpha = s / std::f64::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        let (b0, b1) = if high {
            ((1.0 + c) / 2.0, -(1.0 + c))
        } else {
            ((1.0 - c) / 2.0, 1.0 - c)
        };
        self.b = [b0 / a0, b1 / a0, b0 / a0];
        self.a = [-2.0 * c / a0, (1.0 - alpha) / a0];
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }

    fn reset(&mut self) {
        self.z = [0.0; 2];
    }
}

/// A Linkwitz–Riley 24 dB/octave filter: two Butterworth sections.
#[derive(Clone, Copy, Debug, Default)]
struct Lr4([Biquad; 2]);

impl Lr4 {
    fn set(&mut self, high: bool, hz: f32, rate: f32) {
        for s in &mut self.0 {
            s.set(high, hz, rate);
        }
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.0[0].run(x);
        self.0[1].run(y)
    }

    fn reset(&mut self) {
        for s in &mut self.0 {
            s.reset();
        }
    }
}

/// Bass management of one speaker layout.
pub(crate) struct BassManager {
    rate: f32,
    /// The LFE channel (the subwoofer's feed).
    lfe: usize,
    /// Which channels are main speakers.
    mains: Vec<bool>,
    /// Each main's high-pass.
    high: Vec<Lr4>,
    /// The summed mains' low-pass.
    low: Lr4,
    /// The LFE channel's band limit.
    lfe_low: Lr4,
    /// The settings the filters were made for.
    made_for: Option<(f32, f32)>,
    /// 0 off … 1 on (crossfading between).
    on: f32,
    sum: Vec<f32>,
    dry: Vec<Vec<f32>>,
}

impl BassManager {
    /// For `layout` (`None` when it has no LFE channel, or no main) at
    /// `rate`; `block`: the largest call.
    pub(crate) fn new(layout: ChannelLayout, rate: u32, block: usize) -> Option<Self> {
        if !BassManagement::applies_to(layout) {
            return None;
        }
        let speakers = speakers_of(layout)?;
        let lfe = speakers.iter().position(|s| s.lfe)?;
        let n = speakers.len();
        let block = block.max(1);
        Some(Self {
            rate: rate as f32,
            lfe,
            mains: speakers.iter().map(|s| !s.lfe).collect(),
            high: vec![Lr4::default(); n],
            low: Lr4::default(),
            lfe_low: Lr4::default(),
            made_for: None,
            on: 0.0,
            sum: vec![0.0; block],
            dry: vec![vec![0.0; block]; n],
        })
    }

    fn reset(&mut self) {
        for f in &mut self.high {
            f.reset();
        }
        self.low.reset();
        self.lfe_low.reset();
    }

    /// Manage frames `..n` of `out` (the layout it was made for) by
    /// `settings`.
    pub(crate) fn process(&mut self, settings: BassManagement, out: &mut AudioBuffer, n: usize) {
        let target = if settings.enabled { 1.0 } else { 0.0 };
        if target == 0.0 && self.on == 0.0 {
            return;
        }
        let key = (settings.crossover, settings.lfe_lowpass);
        if self.made_for != Some(key) {
            for f in &mut self.high {
                f.set(true, settings.crossover, self.rate);
            }
            self.low.set(false, settings.crossover, self.rate);
            self.lfe_low.set(false, settings.lfe_lowpass, self.rate);
            self.made_for = Some(key);
        }
        let channels = out.num_channels().min(self.mains.len());
        let n = n.min(self.sum.len());
        let fading = self.on != target;
        if fading {
            for (c, dry) in self.dry.iter_mut().enumerate().take(channels) {
                dry[..n].copy_from_slice(&out.channel(c)[..n]);
            }
        }
        // The mains' bass, summed before they are high-passed.
        self.sum[..n].fill(0.0);
        for c in (0..channels).filter(|&c| self.mains[c]) {
            for (s, &x) in self.sum[..n].iter_mut().zip(&out.channel(c)[..n]) {
                *s += x;
            }
            let f = &mut self.high[c];
            for x in &mut out.channel_mut(c)[..n] {
                *x = f.run(f64::from(*x)) as f32;
            }
        }
        if self.lfe < channels {
            let redirect = faderframe_core::db_to_gain(settings.redirect_db);
            let (low, lfe_low) = (&mut self.low, &mut self.lfe_low);
            for (x, &s) in out.channel_mut(self.lfe)[..n]
                .iter_mut()
                .zip(&self.sum[..n])
            {
                let bass = low.run(f64::from(s)) as f32;
                *x = lfe_low.run(f64::from(*x)) as f32 + redirect * bass;
            }
        }
        if fading {
            // From the unmanaged signal to the managed one (or back).
            let step = 1.0 / (FADE_SECONDS * self.rate).max(1.0);
            let from = self.on;
            for (c, dry) in self.dry.iter().enumerate().take(channels) {
                let mut w = from;
                for (x, &d) in out.channel_mut(c)[..n].iter_mut().zip(&dry[..n]) {
                    w = if target > w {
                        (w + step).min(target)
                    } else {
                        (w - step).max(target)
                    };
                    *x = d + (*x - d) * w;
                }
            }
            let moved = step * n as f32;
            self.on = if target > from {
                (from + moved).min(target)
            } else {
                (from - moved).max(target)
            };
            if self.on == 0.0 {
                // Off: the next time starts from rest.
                self.reset();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::SurroundFormat;

    const RATE: u32 = 48_000;

    fn s51() -> ChannelLayout {
        ChannelLayout::Surround(SurroundFormat::S51)
    }

    /// `seconds` of a sine at `hz` on channel `ch` through bass management
    /// (in blocks of 256), every channel out.
    fn run(settings: BassManagement, ch: usize, hz: f32, frames: usize) -> Vec<Vec<f32>> {
        let mut m = BassManager::new(s51(), RATE, 256).unwrap();
        let mut out: Vec<Vec<f32>> = vec![Vec::new(); 6];
        let mut buf = AudioBuffer::new(s51(), 256);
        let mut t = 0usize;
        while t < frames {
            let n = 256.min(frames - t);
            buf.set_len(n);
            buf.clear();
            for i in 0..n {
                let x = (std::f32::consts::TAU * hz * (t + i) as f32 / RATE as f32).sin() * 0.5;
                buf.channel_mut(ch)[i] = x;
            }
            m.process(settings, &mut buf, n);
            for (c, o) in out.iter_mut().enumerate() {
                o.extend_from_slice(&buf.channel(c)[..n]);
            }
            t += n;
        }
        out
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn on() -> BassManagement {
        BassManagement {
            enabled: true,
            ..BassManagement::default()
        }
    }

    const L: usize = 0;
    const C: usize = 2;
    const LFE: usize = 3;

    #[test]
    fn highs_stay_on_the_mains_lows_go_to_the_sub() {
        let tail = 24_000..48_000;
        let sine = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        // 1 kHz on L: L as it was, nothing on the sub.
        let out = run(on(), L, 1000.0, 48_000);
        assert!((rms(&out[L][tail.clone()]) - sine).abs() < 0.002);
        assert!(rms(&out[LFE][tail.clone()]) < 1e-3);
        // 30 Hz on C: off the centre, on the sub at −10 dB.
        let out = run(on(), C, 30.0, 48_000);
        assert!(
            rms(&out[C][tail.clone()]) < 0.05 * sine,
            "{}",
            rms(&out[C][tail.clone()])
        );
        let sub = rms(&out[LFE][tail.clone()]);
        let want = sine * faderframe_core::db_to_gain(-10.0);
        assert!((sub - want).abs() < 0.05 * want, "{sub} vs {want}");
        // Nothing moves to other mains.
        assert!(rms(&out[L][tail.clone()]) == 0.0);
    }

    #[test]
    fn a_main_and_its_redirected_bass_add_up_flat() {
        // At 0 dB redirect the main's high and its low on the sub, summed,
        // keep the main's level at every frequency (the crossover is an
        // all-pass).
        let flat = BassManagement {
            redirect_db: 0.0,
            ..on()
        };
        for hz in [25.0f32, 50.0, 80.0, 120.0, 400.0] {
            let out = run(flat, L, hz, 48_000);
            let both: Vec<f32> = out[L][24_000..]
                .iter()
                .zip(&out[LFE][24_000..])
                .map(|(a, b)| a + b)
                .collect();
            let db = 20.0 * (rms(&both) / (0.5 * std::f32::consts::FRAC_1_SQRT_2)).log10();
            assert!(db.abs() < 0.1, "{hz} Hz: {db} dB");
        }
        // At the crossover each half is 6 dB down.
        let out = run(flat, L, 80.0, 48_000);
        let half = rms(&out[L][24_000..]) / (0.5 * std::f32::consts::FRAC_1_SQRT_2);
        assert!((20.0 * half.log10() + 6.02).abs() < 0.2, "{half}");
    }

    #[test]
    fn the_lfe_is_only_band_limited() {
        let tail = 24_000..48_000;
        let sine = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        let out = run(on(), LFE, 40.0, 48_000);
        assert!((rms(&out[LFE][tail.clone()]) - sine).abs() < 0.02 * sine);
        let out = run(on(), LFE, 1000.0, 48_000);
        assert!(rms(&out[LFE][tail]) < 1e-3 * sine);
    }

    #[test]
    fn off_it_passes_and_switching_crossfades() {
        let off = BassManagement::default();
        let out = run(off, C, 30.0, 4800);
        assert!((rms(&out[C][2400..]) - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);
        assert!(out[LFE].iter().all(|v| *v == 0.0));
        // On mid-signal: no jump beyond the signal's own steps.
        let mut m = BassManager::new(s51(), RATE, 64).unwrap();
        let mut buf = AudioBuffer::new(s51(), 64);
        let mut c_out = Vec::new();
        for block in 0..200 {
            buf.set_len(64);
            buf.clear();
            for i in 0..64 {
                let t = (block * 64 + i) as f32;
                buf.channel_mut(C)[i] =
                    (std::f32::consts::TAU * 60.0 * t / RATE as f32).sin() * 0.5;
            }
            let s = if block >= 100 { on() } else { off };
            m.process(s, &mut buf, 64);
            c_out.extend_from_slice(&buf.channel(C)[..64]);
        }
        let jump = c_out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        let natural = 0.5 * std::f32::consts::TAU * 60.0 / RATE as f32;
        assert!(jump < 1.5 * natural, "{jump} vs {natural}");
    }

    #[test]
    fn only_layouts_with_an_lfe_are_managed() {
        assert!(BassManager::new(ChannelLayout::Stereo, RATE, 64).is_none());
        assert!(BassManager::new(ChannelLayout::Surround(SurroundFormat::S50), RATE, 64).is_none());
        assert!(
            BassManager::new(ChannelLayout::Surround(SurroundFormat::S714), RATE, 64).is_some()
        );
        let b = BassManagement {
            crossover: f32::NAN,
            redirect_db: 99.0,
            lfe_lowpass: 1.0,
            enabled: true,
        }
        .clamped();
        assert_eq!(
            (b.crossover, b.redirect_db, b.lfe_lowpass),
            (80.0, 10.0, 40.0)
        );
        assert_eq!(BassManagement::from_values(&b.values()), b);
    }
}
