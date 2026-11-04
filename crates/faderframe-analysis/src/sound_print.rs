//! A sound's fingerprint, for finding sounds like it (the media browser's
//! "Find Similar"): deterministic and cheap to compare, no learning.
//!
//! * **Spectral balance**: the mean power in third-octave bands from 25 Hz
//!   to 20 kHz over the frames that are not silent, in dB with the loudest
//!   band at 0 and a floor at [`FLOOR_DB`] (so a quiet copy of a sound is
//!   the same sound).
//! * **Brightness**: the spectral centroid, in octaves (log2 Hz).
//! * **Rhythm**: onsets a second (the level rising sharply from a quieter
//!   point after the last one has fallen away).
//! * **Envelope**: how much the frame levels spread (dB): a pad holds its
//!   level, a drum loop jumps.
//! * **Length**: one-shots and loops stay apart.
//!
//! [`distance`] weighs the five so that a difference of 1 is about "a
//! clearly different sound"; 0 is the same.

use crate::fft;

/// Third-octave bands, 25 Hz … 20 kHz (centres `25 · 2^(k/3)`).
pub const BANDS: usize = 30;
/// The lowest a band reads (dB below the loudest).
pub const FLOOR_DB: f32 = -60.0;
const LOWEST_CENTRE: f64 = 25.0;
const FRAME: usize = 4096;
const HOP: usize = 2048;
/// Frames quieter than this (dBFS, RMS) do not count, nor those this far
/// below the loudest.
const SILENT_DB: f64 = -70.0;
const BELOW_LOUDEST_DB: f64 = 40.0;
/// How far the level rises for an onset (dB).
const ONSET_RISE_DB: f64 = 9.0;
/// Values in [`SoundPrint::to_values`].
pub const VALUES: usize = BANDS + 4;

/// What a sound is like.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundPrint {
    /// Mean level per third-octave band (dB, the loudest 0, ≥ [`FLOOR_DB`]).
    pub bands: [f32; BANDS],
    /// Spectral centroid (log2 Hz).
    pub centroid: f32,
    /// Onsets a second.
    pub onsets: f32,
    /// The spread of the frame levels (dB, standard deviation).
    pub spread: f32,
    /// Length in seconds.
    pub seconds: f32,
}

impl SoundPrint {
    /// The centre frequency of band `k`.
    pub fn band_hz(k: usize) -> f64 {
        LOWEST_CENTRE * 2f64.powf(k as f64 / 3.0)
    }

    /// Flat values (to keep in an index): the bands, then centroid,
    /// onsets, spread, seconds.
    pub fn to_values(&self) -> Vec<f32> {
        let mut v = self.bands.to_vec();
        v.extend([self.centroid, self.onsets, self.spread, self.seconds]);
        v
    }

    /// From [`Self::to_values`] (`None`: not a print of this version).
    pub fn from_values(v: &[f32]) -> Option<Self> {
        if v.len() != VALUES || v.iter().any(|x| !x.is_finite()) {
            return None;
        }
        let mut bands = [0.0; BANDS];
        bands.copy_from_slice(&v[..BANDS]);
        Some(Self {
            bands,
            centroid: v[BANDS],
            onsets: v[BANDS + 1],
            spread: v[BANDS + 2],
            seconds: v[BANDS + 3],
        })
    }
}

/// The print of mono `x` at `rate` (`None`: too short or silent).
pub fn analyse(x: &[f32], rate: f64) -> Option<SoundPrint> {
    if rate.is_nan() || rate <= 0.0 || x.len() < 64 {
        return None;
    }
    // Short sounds: one frame, zero-padded.
    let frames = if x.len() <= FRAME {
        1
    } else {
        (x.len() - FRAME) / HOP + 2
    };
    let window: Vec<f64> = (0..FRAME)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / FRAME as f64).cos())
        .collect();
    let bin_hz = rate / FRAME as f64;
    // Each bin's band (or none) and log2 frequency.
    let band_of: Vec<Option<usize>> = (0..FRAME / 2)
        .map(|b| {
            let f = b as f64 * bin_hz;
            if f <= 0.0 {
                return None;
            }
            let k = (3.0 * (f / LOWEST_CENTRE).log2()).round();
            (k >= 0.0 && (k as usize) < BANDS).then_some(k as usize)
        })
        .collect();
    let mut bands = [0.0f64; BANDS];
    let (mut weighted, mut total) = (0.0f64, 0.0f64);
    let mut levels = Vec::with_capacity(frames);
    let (mut re, mut im) = (vec![0.0f64; FRAME], vec![0.0f64; FRAME]);
    // The window's power, so a frame's RMS reads as the signal's.
    let window_power = window.iter().map(|w| w * w).sum::<f64>() / FRAME as f64;
    // Each frame's level; the quiet ones (silence, a tail far below the
    // sound) do not count, so a quieter copy prints the same.
    let frame_db: Vec<f64> = (0..frames)
        .map(|f| {
            let at = f * HOP;
            let part = &x[at.min(x.len())..(at + FRAME).min(x.len())];
            let energy = part
                .iter()
                .map(|&v| f64::from(v) * f64::from(v))
                .sum::<f64>();
            10.0 * (energy / part.len().max(1) as f64).max(1e-30).log10()
        })
        .collect();
    let loudest_frame = frame_db.iter().copied().fold(f64::MIN, f64::max);
    let quiet = SILENT_DB.max(loudest_frame - BELOW_LOUDEST_DB);
    for (f, &rms_db) in frame_db.iter().enumerate() {
        if rms_db < quiet {
            continue;
        }
        let at = f * HOP;
        for i in 0..FRAME {
            re[i] = x.get(at + i).map_or(0.0, |&s| f64::from(s)) * window[i];
            im[i] = 0.0;
        }
        levels.push(rms_db);
        fft(&mut re, &mut im);
        for (b, band) in band_of.iter().enumerate() {
            let p = (re[b] * re[b] + im[b] * im[b]) / (FRAME as f64 * window_power);
            if let Some(k) = band {
                bands[*k] += p;
            }
            if b > 0 {
                let hz = b as f64 * bin_hz;
                if hz >= 20.0 {
                    weighted += p * hz.log2();
                    total += p;
                }
            }
        }
    }
    if levels.is_empty() || total <= 0.0 {
        return None;
    }
    let loudest = bands.iter().copied().fold(0.0f64, f64::max).max(1e-30);
    let mut out = [FLOOR_DB; BANDS];
    for (o, p) in out.iter_mut().zip(bands) {
        *o = ((10.0 * (p / loudest).max(1e-30).log10()) as f32).max(FLOOR_DB);
    }
    let mean = levels.iter().sum::<f64>() / levels.len() as f64;
    let spread =
        (levels.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / levels.len() as f64).sqrt();
    let seconds = x.len() as f64 / rate;
    Some(SoundPrint {
        bands: out,
        centroid: (weighted / total) as f32,
        onsets: (onset_count(x, rate) as f64 / seconds.max(0.25)) as f32,
        spread: spread as f32,
        seconds: seconds as f32,
    })
}

/// How many onsets `x` has: rises of at least [`ONSET_RISE_DB`] in the
/// level (23 ms windows every 5 ms) from the quietest point since the last one fell
/// away by 6 dB, within 45 dB of the loudest.
pub fn onset_count(x: &[f32], rate: f64) -> usize {
    let hop = ((rate * 0.005) as usize).max(1);
    // 23 ms windows: a chord's beating is not an onset.
    let window = ((rate * 0.023) as usize).max(hop);
    let mut prefix = Vec::with_capacity(x.len() + 1);
    prefix.push(0.0f64);
    for &v in x {
        let last = prefix.last().copied().unwrap_or(0.0);
        prefix.push(last + f64::from(v) * f64::from(v));
    }
    let levels: Vec<f64> = (0..x.len().div_ceil(hop))
        .map(|i| {
            let a = i * hop;
            let b = (a + window).min(x.len());
            let e = (prefix[b] - prefix[a]) / (b - a).max(1) as f64;
            10.0 * e.max(1e-30).log10()
        })
        .collect();
    let top = levels.iter().copied().fold(f64::MIN, f64::max);
    let floor = top - 45.0;
    let (mut count, mut armed) = (0usize, true);
    // The sound rises from silence before its first window.
    let (mut low, mut peak) = (-300.0f64, f64::MIN);
    for &l in &levels {
        if armed {
            low = low.min(l);
            if l > floor && l - low >= ONSET_RISE_DB {
                count += 1;
                armed = false;
                peak = l;
            }
        } else {
            peak = peak.max(l);
            if l <= peak - 6.0 {
                armed = true;
                low = l;
            }
        }
    }
    count.max(1)
}

/// How unlike two sounds are (0: the same; about 1: clearly different).
pub fn distance(a: &SoundPrint, b: &SoundPrint) -> f32 {
    let bands = (a
        .bands
        .iter()
        .zip(&b.bands)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f32>()
        / BANDS as f32)
        .sqrt()
        / 12.0;
    let centroid = (a.centroid - b.centroid).abs() / 1.5;
    let onsets = ((a.onsets + 0.5) / (b.onsets + 0.5)).ln().abs() / 2.0;
    let spread = (a.spread - b.spread).abs() / 12.0;
    let length = ((a.seconds + 0.25) / (b.seconds + 0.25)).ln().abs() / 3.0;
    bands + centroid + onsets + spread + length
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const RATE: f64 = 48_000.0;

    /// A pitched drum: a sine falling from `hz` with a fast decay, every
    /// `every` seconds over `seconds`.
    fn drum(hz: f64, every: f64, seconds: f64) -> Vec<f32> {
        let n = (RATE * seconds) as usize;
        (0..n)
            .map(|i| {
                let t = (i as f64 / RATE) % every;
                let f = hz * (1.0 + 2.0 * (-t * 40.0).exp());
                ((std::f64::consts::TAU * f * t).sin() * (-t * 12.0).exp() * 0.8) as f32
            })
            .collect()
    }

    /// Noise bursts (a hi-hat), hashed so the test is deterministic.
    fn hat(every: f64, seconds: f64) -> Vec<f32> {
        let n = (RATE * seconds) as usize;
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut last = 0.0f64;
        (0..n)
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let white = (state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
                // A first difference: bright.
                let v = white - last;
                last = white;
                let t = (i as f64 / RATE) % every;
                (v * (-t * 60.0).exp() * 0.4) as f32
            })
            .collect()
    }

    /// A held chord.
    fn pad(seconds: f64) -> Vec<f32> {
        let n = (RATE * seconds) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / RATE;
                [220.0, 277.18, 329.63]
                    .iter()
                    .map(|f| (std::f64::consts::TAU * f * t).sin() * 0.2)
                    .sum::<f64>() as f32
            })
            .collect()
    }

    #[test]
    fn like_sounds_are_nearer_than_unlike_ones() {
        let kick = analyse(&drum(55.0, 0.5, 4.0), RATE).unwrap();
        let other_kick = analyse(&drum(62.0, 0.5, 4.0), RATE).unwrap();
        let hats = analyse(&hat(0.25, 4.0), RATE).unwrap();
        let chord = analyse(&pad(4.0), RATE).unwrap();
        let near = distance(&kick, &other_kick);
        assert!(near < 0.5, "two kicks: {near}");
        for (what, other) in [("hats", &hats), ("a pad", &chord)] {
            let far = distance(&kick, other);
            assert!(
                far > 2.0 * near && far > 0.6,
                "kick vs {what}: {far} (kicks {near})"
            );
        }
        // Brightness and rhythm read as they should.
        assert!(hats.centroid > kick.centroid + 3.0);
        assert!(hats.onsets > 3.0 && hats.onsets < 5.0, "{}", hats.onsets);
        assert!(chord.onsets < 1.0, "{}", chord.onsets);
        assert!(chord.spread < kick.spread);
        // The same sound, quieter: the same print, nearly.
        let quiet: Vec<f32> = drum(55.0, 0.5, 4.0).iter().map(|v| v * 0.1).collect();
        let soft = analyse(&quiet, RATE).unwrap();
        assert!(distance(&kick, &soft) < 0.05, "{}", distance(&kick, &soft));
        assert_eq!(distance(&kick, &kick), 0.0);
    }

    #[test]
    fn silence_and_scraps_have_no_print_and_prints_keep_as_values() {
        assert!(analyse(&vec![0.0; 48_000], RATE).is_none());
        assert!(analyse(&[0.5; 10], RATE).is_none());
        // A sound shorter than a frame still has one.
        let click = analyse(&drum(80.0, 1.0, 0.05), RATE).unwrap();
        assert!((click.seconds - 0.05).abs() < 1e-3);
        let back = SoundPrint::from_values(&click.to_values()).unwrap();
        assert_eq!(back, click);
        assert!(SoundPrint::from_values(&[0.0; 3]).is_none());
        assert_eq!(click.bands.iter().copied().fold(f32::MIN, f32::max), 0.0);
    }
}
