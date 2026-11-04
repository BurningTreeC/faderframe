//! What the Convolution Reverb does to a response before it plays it: the
//! part from Start, Length of what follows; reversed; faded in and out
//! (raised cosines); stretched (resampled: the room scaled, its pitch and
//! colour with it) and brought to the engine's rate in one resampling;
//! normalised (the paths' energy into each output, averaged, made one: a
//! dense tail as loud as the dry signal for noise). Done off the audio
//! thread (the processor's designer).

/// The longest response played (seconds, after stretching).
pub const MAX_SECONDS: f64 = 20.0;
/// The most a response is stretched.
pub const MAX_STRETCH: f64 = 2.0;
pub const MIN_STRETCH: f64 = 0.5;

/// The settings that shape a response.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shaping {
    pub start_ms: f64,
    /// A share of what follows the start (0–1).
    pub length: f64,
    pub fade_in_ms: f64,
    /// A share of the length, at its end (0–1).
    pub fade_out: f64,
    pub reverse: bool,
    /// Time scale (1: as recorded).
    pub stretch: f64,
    pub normalise: bool,
}

impl Default for Shaping {
    fn default() -> Self {
        Self {
            start_ms: 0.0,
            length: 1.0,
            fade_in_ms: 0.0,
            fade_out: 0.0,
            reverse: false,
            stretch: 1.0,
            normalise: true,
        }
    }
}

/// The response's channels as the engine plays them: one (mono), two
/// (stereo: left into left, right into right) or four (true stereo: left
/// into left, left into right, right into left, right into right).
pub fn shape(rate_in: f64, channels: &[&[f32]], s: &Shaping, rate: f64) -> Vec<Vec<f32>> {
    let len = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    let start = ((s.start_ms.max(0.0) * 1e-3 * rate_in) as usize).min(len.saturating_sub(1));
    let rest = len - start;
    let take = ((rest as f64 * s.length.clamp(0.0, 1.0)).round() as usize).clamp(1.min(rest), rest);
    let stretch = s.stretch.clamp(MIN_STRETCH, MAX_STRETCH);
    let ratio = rate / rate_in.max(1.0) * stretch;
    let most = (MAX_SECONDS * rate) as usize;
    let fade_in = (s.fade_in_ms.max(0.0) * 1e-3 * rate_in) as usize;
    let fade_out = (take as f64 * s.fade_out.clamp(0.0, 1.0)) as usize;
    let mut out: Vec<Vec<f32>> = channels
        .iter()
        .map(|c| {
            let mut x: Vec<f32> = c[start..start + take].to_vec();
            if s.reverse {
                x.reverse();
            }
            fade(&mut x, fade_in, fade_out);
            let mut y = resample(&x, ratio);
            y.truncate(most);
            y
        })
        .collect();
    if s.normalise {
        normalise(&mut out);
    }
    out
}

/// Raised-cosine fades over the first `fade_in` and the last `fade_out`
/// samples.
fn fade(x: &mut [f32], fade_in: usize, fade_out: usize) {
    let n = x.len();
    let fade_in = fade_in.min(n);
    for (i, v) in x.iter_mut().take(fade_in).enumerate() {
        let t = (i as f64 + 0.5) / fade_in as f64;
        *v *= (0.5 - 0.5 * (std::f64::consts::PI * t).cos()) as f32;
    }
    let fade_out = fade_out.min(n);
    for i in 0..fade_out {
        let t = (i as f64 + 0.5) / fade_out as f64;
        x[n - 1 - i] *= (0.5 - 0.5 * (std::f64::consts::PI * t).cos()) as f32;
    }
}

/// Each output's energy (the paths into it), averaged, made one.
fn normalise(channels: &mut [Vec<f32>]) {
    let energy = |c: &[f32]| c.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
    let e: Vec<f64> = channels.iter().map(|c| energy(c)).collect();
    // Per output: mono and stereo responses one path each, true stereo two.
    let per_output = match e.len() {
        4 => 0.5 * (e[0] + e[2] + e[1] + e[3]),
        n => e.iter().sum::<f64>() / n.max(1) as f64,
    };
    if per_output <= 1e-20 {
        return;
    }
    let g = (1.0 / per_output).sqrt() as f32;
    for c in channels {
        for v in c.iter_mut() {
            *v *= g;
        }
    }
}

/// Zero crossings each side of the resampling kernel.
const HALF: usize = 24;
/// Table points per zero crossing.
const STEPS: usize = 256;

/// The windowed sinc, tabulated from 0 to `HALF` (Kaiser, β = 8).
fn table() -> &'static [f32] {
    static TABLE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let n = HALF * STEPS + 2;
        let beta = 8.0;
        let i0b = bessel_i0(beta);
        (0..n)
            .map(|i| {
                let u = i as f64 / STEPS as f64;
                if u >= HALF as f64 {
                    return 0.0;
                }
                let sinc = if u < 1e-12 {
                    1.0
                } else {
                    (std::f64::consts::PI * u).sin() / (std::f64::consts::PI * u)
                };
                let r = u / HALF as f64;
                let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
                (sinc * w) as f32
            })
            .collect()
    })
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    let q = x * x / 4.0;
    for k in 1..60 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// `x` resampled by `ratio` (output samples per input sample), band
/// limited to the lower rate's Nyquist.
pub fn resample(x: &[f32], ratio: f64) -> Vec<f32> {
    if (ratio - 1.0).abs() < 1e-9 || x.is_empty() {
        return x.to_vec();
    }
    let t = table();
    let cutoff = ratio.min(1.0);
    let reach = HALF as f64 / cutoff;
    let n = ((x.len() as f64) * ratio).round().max(1.0) as usize;
    let mut y = vec![0.0f32; n];
    for (j, out) in y.iter_mut().enumerate() {
        let centre = j as f64 / ratio;
        let lo = (centre - reach).ceil().max(0.0) as usize;
        let hi = ((centre + reach).floor() as usize).min(x.len() - 1);
        let mut acc = 0.0f64;
        for (k, v) in x.iter().enumerate().take(hi + 1).skip(lo) {
            let u = (centre - k as f64).abs() * cutoff * STEPS as f64;
            let i = u as usize;
            if i + 1 >= t.len() {
                continue;
            }
            let f = (u - i as f64) as f32;
            let w = t[i] + (t[i + 1] - t[i]) * f;
            acc += f64::from(*v) * f64::from(w);
        }
        *out = (acc * cutoff) as f32;
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f64::consts::TAU * f * i as f64 / rate).sin() as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn resampling_keeps_a_tone_and_its_pitch() {
        let x = sine(1_000.0, 44_100.0, 44_100);
        let y = resample(&x, 48_000.0 / 44_100.0);
        assert_eq!(y.len(), 48_000);
        let want = sine(1_000.0, 48_000.0, 48_000);
        for n in 1_000..47_000 {
            assert!(
                (y[n] - want[n]).abs() < 2e-3,
                "at {n}: {} vs {}",
                y[n],
                want[n]
            );
        }
        // Down: a tone above the new Nyquist is taken out.
        let x = sine(20_000.0, 48_000.0, 48_000);
        let y = resample(&x, 0.5);
        assert!(rms(&y[500..23_500]) < 0.01, "{}", rms(&y[500..23_500]));
        // Twice as slow: the same tone an octave down.
        let x = sine(1_000.0, 48_000.0, 24_000);
        let y = resample(&x, 2.0);
        let want = sine(500.0, 48_000.0, 48_000);
        for n in 1_000..47_000 {
            assert!((y[n] - want[n]).abs() < 2e-3);
        }
    }

    #[test]
    fn a_response_is_trimmed_reversed_faded_stretched_and_normalised() {
        // A ramp 1 s long at 24 kHz.
        let x: Vec<f32> = (0..24_000).map(|i| 1.0 - i as f32 / 24_000.0).collect();
        let s = Shaping {
            start_ms: 250.0,
            length: 0.5,
            normalise: false,
            ..Shaping::default()
        };
        let y = shape(24_000.0, &[&x], &s, 24_000.0);
        // From 0.25 s, half of the 0.75 s left.
        assert_eq!(y[0].len(), 9_000);
        assert!((y[0][0] - 0.75).abs() < 1e-6);
        let r = shape(24_000.0, &[&x], &Shaping { reverse: true, ..s }, 24_000.0);
        assert!((r[0][0] - y[0][8_999]).abs() < 1e-6);
        // Faded: silent at both ends, untouched between.
        let f = shape(
            24_000.0,
            &[&x],
            &Shaping {
                fade_in_ms: 10.0,
                fade_out: 0.1,
                ..s
            },
            24_000.0,
        );
        assert!(f[0][0].abs() < 1e-3 && f[0][8_999].abs() < 1e-3);
        assert_eq!(f[0][4_000], y[0][4_000]);
        // Stretched twice at twice the rate: four times the samples.
        let st = shape(24_000.0, &[&x], &Shaping { stretch: 2.0, ..s }, 48_000.0);
        assert_eq!(st[0].len(), 36_000);
        // Normalised: a true-stereo response's outputs have unit energy on
        // average.
        let paths: Vec<Vec<f32>> = (0..4).map(|k| vec![0.1 * (k + 1) as f32; 100]).collect();
        let refs: Vec<&[f32]> = paths.iter().map(|p| p.as_slice()).collect();
        let n = shape(48_000.0, &refs, &Shaping::default(), 48_000.0);
        let e: Vec<f64> = n
            .iter()
            .map(|c| c.iter().map(|v| f64::from(*v).powi(2)).sum())
            .collect();
        assert!(((e[0] + e[1] + e[2] + e[3]) / 2.0 - 1.0).abs() < 1e-4);
        // Longer than the most played: cut.
        let long = vec![0.01f32; 1_000];
        let c = shape(
            100.0,
            &[&long],
            &Shaping {
                stretch: 2.0,
                ..Shaping::default()
            },
            100.0,
        );
        assert_eq!(c[0].len(), (MAX_SECONDS * 100.0) as usize);
    }
}
