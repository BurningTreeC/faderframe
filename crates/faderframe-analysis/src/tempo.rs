//! Tempo detection: the tempo a recording (a loop, a song) is played at
//! and where its beats fall.
//!
//! A copy decimated to about 11 kHz gives an onset envelope: the spectral
//! flux (log magnitudes rising, summed over 512-point frames every 64
//! samples), less its local mean. The tempo is the beat period whose comb
//! — the envelope summed a period apart, at the best phase, with half and
//! double periods counting a little — stands out most, weighted towards
//! moderate tempos (a log-normal prior around 120 BPM, so a loop is not
//! heard at half or double speed); then refined finely over the whole
//! recording. Tempos within 0.05 BPM of a whole number are taken as it.

use crate::fft;

/// Lowest and highest tempos considered (BPM).
const SLOWEST: f64 = 60.0;
const FASTEST: f64 = 200.0;
const ANALYSIS_RATE: f64 = 11_025.0;
const FRAME: usize = 512;
const HOP: usize = 64;

/// A recording's tempo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tempo {
    /// Beats (quarter notes) a minute.
    pub bpm: f64,
    /// Seconds from the start to the first beat.
    pub first_beat: f64,
    /// How clearly the beat stands out (0…1; under 0.1 hardly at all).
    pub confidence: f64,
}

/// The onset envelope of mono `x` at `rate`, and its frames a second.
pub fn onset_envelope(x: &[f32], rate: f64) -> (Vec<f32>, f64) {
    let d = ((rate / ANALYSIS_RATE).floor() as usize).max(1);
    let low = crate::melody::decimate(x, d);
    let low_rate = rate / d as f64;
    let frames = low.len().saturating_sub(FRAME) / HOP + 1;
    if low.len() < FRAME {
        return (Vec::new(), low_rate / HOP as f64);
    }
    let window: Vec<f64> = (0..FRAME)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / FRAME as f64).cos())
        .collect();
    let bins = FRAME / 2;
    let mut prev = vec![0.0f64; bins];
    let mut re = vec![0.0f64; FRAME];
    let mut im = vec![0.0f64; FRAME];
    let mut env = Vec::with_capacity(frames);
    for f in 0..frames {
        let at = f * HOP;
        for i in 0..FRAME {
            re[i] = f64::from(low[at + i]) * window[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        let mut flux = 0.0;
        for b in 1..bins {
            let m = (1.0 + 100.0 * (re[b] * re[b] + im[b] * im[b]).sqrt()).ln();
            flux += (m - prev[b]).max(0.0);
            prev[b] = m;
        }
        env.push(flux as f32);
    }
    if let Some(first) = env.first_mut() {
        // The first frame rises from nothing: not an onset.
        *first = 0.0;
    }
    // Less the local mean (half a second), never below zero.
    let reach = ((low_rate / HOP as f64) * 0.25) as usize;
    let mut prefix = vec![0.0f64; env.len() + 1];
    for (i, v) in env.iter().enumerate() {
        prefix[i + 1] = prefix[i] + f64::from(*v);
    }
    let out = (0..env.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(reach), (i + reach + 1).min(env.len()));
            let mean = (prefix[b] - prefix[a]) / (b - a) as f64;
            (f64::from(env[i]) - mean).max(0.0) as f32
        })
        .collect();
    (out, low_rate / HOP as f64)
}

/// The envelope at fractional frame `x` (zero outside).
fn at(env: &[f32], x: f64) -> f64 {
    let i = x.floor();
    if i < 0.0 {
        return 0.0;
    }
    let i = i as usize;
    let (Some(a), b) = (env.get(i), env.get(i + 1).copied().unwrap_or(0.0)) else {
        return 0.0;
    };
    let f = x - i as f64;
    f64::from(*a) * (1.0 - f) + f64::from(b) * f
}

/// The comb at period `p` (frames): the best phase's mean envelope a
/// period apart, and that phase.
fn comb(env: &[f32], p: f64) -> (f64, f64) {
    let mut best = (0.0, 0.0);
    let steps = (p * 2.0).ceil() as usize;
    for s in 0..steps {
        let phase = s as f64 * 0.5;
        let (mut sum, mut k) = (0.0, 0usize);
        let mut x = phase;
        while x < env.len() as f64 {
            sum += at(env, x);
            x += p;
            k += 1;
        }
        let v = sum / k.max(1) as f64;
        if v > best.0 {
            best = (v, phase);
        }
    }
    best
}

/// The tempo of mono `x` at `rate` (`None`: too short or no beat).
pub fn detect(x: &[f32], rate: f64) -> Option<Tempo> {
    let (env, fps) = onset_envelope(x, rate);
    // At least four beats of the slowest tempo.
    if env.len() < (fps * 4.0 * 60.0 / SLOWEST) as usize {
        return None;
    }
    let mean = env.iter().map(|v| f64::from(*v)).sum::<f64>() / env.len() as f64;
    if mean <= 1e-9 {
        return None;
    }
    let period = |bpm: f64| fps * 60.0 / bpm;
    // Coarse: every 0.5 BPM, the comb with its half and double, weighted.
    let score = |bpm: f64| {
        let p = period(bpm);
        let main = comb(&env, p).0;
        let half = comb(&env, p / 2.0).0;
        let double = if p * 2.0 < env.len() as f64 / 2.0 {
            comb(&env, p * 2.0).0
        } else {
            main
        };
        let prior = (-0.5 * ((bpm / 120.0).log2() / 0.9).powi(2)).exp();
        (main + 0.25 * half + 0.25 * double) * prior
    };
    let mut best = (f64::MIN, 120.0);
    let mut bpm = SLOWEST;
    let mut scores = Vec::new();
    while bpm <= FASTEST {
        let s = score(bpm);
        scores.push(s);
        if s > best.0 {
            best = (s, bpm);
        }
        bpm += 0.5;
    }
    // Fine: ±0.5 BPM every 0.01, on the plain comb (exact period).
    let coarse = best.1;
    let mut fine = (f64::MIN, coarse, 0.0);
    let mut b = coarse - 0.5;
    while b <= coarse + 0.5 {
        let (v, phase) = comb(&env, period(b));
        if v > fine.0 {
            fine = (v, b, phase);
        }
        b += 0.01;
    }
    let mut bpm = fine.1;
    if (bpm - bpm.round()).abs() < 0.05 {
        bpm = bpm.round();
    }
    let p = period(bpm);
    let (peak, phase) = comb(&env, p);
    // How far the beat stands out over the envelope's own level (noise
    // reaches about 3 by chance, a clear beat 15 or more).
    let confidence = ((peak / mean - 3.0) / 12.0).clamp(0.0, 1.0);
    // The first beat: the phase, less the envelope's frame offset (a Hann
    // window's flux rises most with an onset three quarters into it).
    let first_beat = ((phase + 0.75 * FRAME as f64 / HOP as f64) / fps).rem_euclid(p / fps);
    Some(Tempo {
        bpm,
        first_beat,
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    /// Clicks (short noise bursts) on the beats of `bpm` from `offset`
    /// seconds, every other one softer, with off-beat hats.
    fn beat(bpm: f64, seconds: f64, offset: f64) -> Vec<f32> {
        let mut x = vec![0.0f32; (SR * seconds) as usize];
        let mut seed = 11u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
        };
        let step = 60.0 / bpm / 2.0;
        let mut k = 0;
        loop {
            let t = offset + k as f64 * step;
            let at = (t * SR) as usize;
            if at >= x.len() {
                break;
            }
            let (level, len) = match k % 4 {
                0 => (1.0, 2_400),
                2 => (0.7, 2_400),
                _ => (0.25, 600),
            };
            for i in 0..len {
                if let Some(s) = x.get_mut(at + i) {
                    *s += noise() * level * (-(i as f32) / (len as f32 / 5.0)).exp();
                }
            }
            k += 1;
        }
        x
    }

    #[test]
    fn a_beat_is_found_at_its_tempo_and_phase() {
        for (bpm, offset) in [
            (120.0, 0.0),
            (97.5, 0.21),
            (140.0, 0.1),
            (84.0, 0.0),
            (172.0, 0.05),
        ] {
            let t = detect(&beat(bpm, 16.0, offset), SR).unwrap();
            // Fast tempos may be heard in half time (drum and bass: 172
            // or 86).
            let heard = if bpm > 160.0 && (t.bpm - bpm / 2.0).abs() < 0.03 {
                bpm / 2.0
            } else {
                bpm
            };
            assert!((t.bpm - heard).abs() < 0.03, "{bpm}: {t:?}");
            let beat_len = 60.0 / bpm;
            let off = (t.first_beat - offset).rem_euclid(beat_len);
            let off = off.min(beat_len - off);
            // On the beat (or, hats being softer, at worst on a strong one).
            assert!(off < 0.02, "{bpm}: first beat {} vs {offset}", t.first_beat);
            assert!(t.confidence > 0.5, "{bpm}: {t:?}");
        }
    }

    #[test]
    fn noise_and_silence_have_no_clear_beat() {
        let mut seed = 3u32;
        let noise: Vec<f32> = (0..(SR * 8.0) as usize)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1 << 24) as f32 - 0.5) * 0.3
            })
            .collect();
        if let Some(t) = detect(&noise, SR) {
            assert!(t.confidence < 0.1, "{t:?}");
        }
        assert_eq!(detect(&vec![0.0; 96_000], SR), None);
        assert_eq!(detect(&[0.1; 1_000], SR), None);
    }
}
