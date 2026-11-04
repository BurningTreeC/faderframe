//! Wavetables: up to [`MAX_FRAMES`] single cycles of [`CYCLE`] samples,
//! each kept as band-limited copies an octave apart (mipmaps: the copy for
//! a pitch holds only the harmonics below Nyquist there), built off the
//! audio thread from the cycles' spectra; the factory tables made by code
//! here (never taken from another product); and user tables read from a
//! WAV — its frame size from a `clm ` chunk, the document, or guessed.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;
use std::f64::consts::{PI, TAU};
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// Samples in a cycle of the full table.
pub const CYCLE: usize = 2048;
/// Most frames a table has.
pub const MAX_FRAMES: usize = 256;
/// Band-limited copies: harmonics up to 1023, 512, 256, … 1.
pub const LEVELS: usize = 11;
/// Most harmonics a cycle has (below its Nyquist).
pub const MAX_HARMONICS: usize = CYCLE / 2 - 1;
/// The highest a level's top harmonic is played at (cycles per sample):
/// a level fades out before its top harmonic reaches this.
const TOP: f32 = 0.47;

/// Harmonics level `l` keeps.
pub const fn harmonics(level: usize) -> usize {
    let h = (CYCLE / 2) >> level;
    if h > MAX_HARMONICS { MAX_HARMONICS } else { h }
}

/// Samples per cycle at level `l`: sixteen a period of its top harmonic,
/// at least 256 (linear interpolation's images stay 70 dB down).
pub const fn level_size(level: usize) -> usize {
    let n = 16 * ((CYCLE / 2) >> level);
    if n > CYCLE {
        CYCLE
    } else if n < 256 {
        256
    } else {
        n
    }
}

/// A wavetable: its frames at every level.
pub struct Table {
    pub name: String,
    pub frames: usize,
    /// Per level, every frame's cycle and a guard sample (the cycle's
    /// first again), frame after frame.
    levels: Vec<Vec<f32>>,
}

impl std::fmt::Debug for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Table")
            .field("name", &self.name)
            .field("frames", &self.frames)
            .finish()
    }
}

/// Which two levels a pitch reads and how much of the second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mip {
    pub a: usize,
    pub b: usize,
    pub mix: f32,
}

impl Mip {
    /// The levels for a phase increment of `inc` cycles per sample: the
    /// first's harmonics reach at most [`TOP`] while it is heard, the
    /// second (an octave fewer) takes over across the octave.
    pub fn for_increment(inc: f32) -> Self {
        let x = (inc.max(1e-9) * (CYCLE / 2) as f32 / TOP).log2().max(-1.0);
        let fl = x.floor();
        let a = ((fl + 1.0) as usize).min(LEVELS - 1);
        Mip {
            a,
            b: (a + 1).min(LEVELS - 1),
            mix: x - fl,
        }
    }
}

/// Two frames a position falls between.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frames {
    pub f0: usize,
    pub f1: usize,
    pub mix: f32,
}

impl Table {
    /// Where `position` (0…1) falls among the frames.
    #[inline]
    pub fn frames_at(&self, position: f32) -> Frames {
        let last = self.frames.saturating_sub(1);
        let x = position.clamp(0.0, 1.0) * last as f32;
        let f0 = (x as usize).min(last);
        Frames {
            f0,
            f1: (f0 + 1).min(last),
            mix: x - f0 as f32,
        }
    }

    #[inline]
    fn at_level(&self, level: usize, f: Frames, phase: f32) -> f32 {
        let n = level_size(level);
        let data = &self.levels[level];
        let x = phase * n as f32;
        let i = (x as usize).min(n - 1);
        let t = x - i as f32;
        let a = f.f0 * (n + 1) + i;
        let s0 = data[a] + (data[a + 1] - data[a]) * t;
        if f.mix <= 0.0 {
            return s0;
        }
        let b = f.f1 * (n + 1) + i;
        let s1 = data[b] + (data[b + 1] - data[b]) * t;
        s0 + (s1 - s0) * f.mix
    }

    /// The table at `phase` (0…1), between frames `f` and levels `m`.
    #[inline]
    pub fn read(&self, m: Mip, f: Frames, phase: f32) -> f32 {
        let a = self.at_level(m.a, f, phase);
        if m.mix <= 0.0 || m.b == m.a {
            return a;
        }
        a + (self.at_level(m.b, f, phase) - a) * m.mix
    }

    /// Frame `frame`'s full cycle (for drawing).
    pub fn cycle(&self, frame: usize) -> &[f32] {
        let f = frame.min(self.frames.saturating_sub(1));
        &self.levels[0][f * (CYCLE + 1)..f * (CYCLE + 1) + CYCLE]
    }

    /// A table from its frames' spectra: per frame per harmonic `h`
    /// (index `h − 1`) the cosine and sine amplitudes. The loudest frame
    /// peaks at 1.
    pub fn from_spectra(name: &str, spectra: &[Vec<(f64, f64)>]) -> Self {
        let frames = spectra.len().clamp(1, MAX_FRAMES);
        let mut planner = RealFftPlanner::<f64>::new();
        // The full cycles first, for the gain.
        let peak = spectra
            .iter()
            .take(frames)
            .map(|s| {
                cycle_of(s, CYCLE, MAX_HARMONICS, &mut planner)
                    .iter()
                    .fold(0.0f64, |m, v| m.max(v.abs()))
            })
            .fold(0.0f64, f64::max);
        let gain = if peak > 1e-9 { 1.0 / peak } else { 1.0 };
        let levels = (0..LEVELS)
            .map(|l| {
                let n = level_size(l);
                let mut data = Vec::with_capacity(frames * (n + 1));
                for k in 0..frames {
                    let s = spectra.get(k).map_or(&[][..], |s| &s[..]);
                    let cycle = cycle_of(s, n, harmonics(l), &mut planner);
                    data.extend(cycle.iter().map(|v| (v * gain) as f32));
                    data.push((cycle[0] * gain) as f32);
                }
                data
            })
            .collect();
        Self {
            name: name.into(),
            frames,
            levels,
        }
    }

    /// A table from cycles of any length (each a frame): their spectra,
    /// up to what a cycle of that length holds.
    pub fn from_cycles(name: &str, cycles: &[Vec<f32>]) -> Self {
        let mut planner = RealFftPlanner::<f64>::new();
        let spectra: Vec<Vec<(f64, f64)>> = cycles
            .iter()
            .take(MAX_FRAMES)
            .map(|c| spectrum_of(c, &mut planner))
            .collect();
        Self::from_spectra(name, &spectra)
    }

    /// A table from a function of the phase (0…1) per frame, sampled
    /// eight times finer than a cycle so its harmonics come out clean.
    pub fn from_shape(name: &str, frames: usize, f: impl Fn(usize, f64) -> f64) -> Self {
        let n = CYCLE * 8;
        let cycles: Vec<Vec<f32>> = (0..frames)
            .map(|k| (0..n).map(|i| f(k, i as f64 / n as f64) as f32).collect())
            .collect();
        Self::from_cycles(name, &cycles)
    }
}

/// The cosine/sine amplitudes of a cycle's harmonics (as many as fit,
/// at most [`MAX_HARMONICS`]).
fn spectrum_of(cycle: &[f32], planner: &mut RealFftPlanner<f64>) -> Vec<(f64, f64)> {
    let n = cycle.len();
    if n < 4 {
        return Vec::new();
    }
    let fft = planner.plan_fft_forward(n);
    let mut input: Vec<f64> = cycle.iter().map(|v| f64::from(*v)).collect();
    let mut out = fft.make_output_vec();
    if fft.process(&mut input, &mut out).is_err() {
        return Vec::new();
    }
    let top = (n / 2).saturating_sub(1).min(MAX_HARMONICS);
    let scale = 2.0 / n as f64;
    out.iter()
        .skip(1)
        .take(top)
        .map(|c| (c.re * scale, -c.im * scale))
        .collect()
}

/// A cycle of `n` samples from harmonic amplitudes (no DC), up to
/// `harmonics`.
fn cycle_of(
    spectrum: &[(f64, f64)],
    n: usize,
    harmonics: usize,
    planner: &mut RealFftPlanner<f64>,
) -> Vec<f64> {
    let fft = planner.plan_fft_inverse(n);
    let mut spec = fft.make_input_vec();
    let top = harmonics.min(n / 2 - 1).min(spectrum.len());
    for (h, (a, b)) in spectrum.iter().take(top).enumerate() {
        spec[h + 1] = Complex::new(a / 2.0, -b / 2.0);
    }
    let mut out = fft.make_output_vec();
    if fft.process(&mut spec, &mut out).is_err() {
        out.iter_mut().for_each(|v| *v = 0.0);
    }
    out
}

/// The factory tables by name, in their stable order (an oscillator's
/// Table parameter is an index into them; [`FACTORY`]`.len()` is "User").
pub const FACTORY: [&str; 13] = [
    "Basic Shapes",
    "Pulse Width",
    "Saw Sync",
    "Harmonic Build",
    "Odd Build",
    "Formants",
    "FM 1:1",
    "FM 2:7",
    "Wavefold",
    "Bit Steps",
    "Organ",
    "Resonant Saw",
    "Spectra",
];

/// Frames of a factory table.
const FACTORY_FRAMES: usize = 32;

/// The factory tables (made once, on first use, never on the audio
/// thread).
pub fn factory() -> &'static [Arc<Table>] {
    static TABLES: OnceLock<Vec<Arc<Table>>> = OnceLock::new();
    TABLES.get_or_init(|| {
        (0..FACTORY.len())
            .map(|i| Arc::new(make_factory(i)))
            .collect()
    })
}

/// The spectrum of a harmonic function (cosine and sine amplitudes per
/// harmonic, up to [`MAX_HARMONICS`]).
fn additive(f: impl Fn(usize) -> (f64, f64)) -> Vec<(f64, f64)> {
    (1..=MAX_HARMONICS).map(f).collect()
}

/// Sine amplitudes `a(h)`.
fn sines(a: impl Fn(usize) -> f64) -> Vec<(f64, f64)> {
    additive(|h| (0.0, a(h)))
}

fn saw(h: usize) -> f64 {
    let sign = if h % 2 == 1 { 1.0 } else { -1.0 };
    sign * 2.0 / (PI * h as f64)
}

fn square(h: usize) -> f64 {
    if h % 2 == 1 {
        4.0 / (PI * h as f64)
    } else {
        0.0
    }
}

fn triangle(h: usize) -> f64 {
    if h % 2 == 1 {
        let sign = if (h / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
        sign * 8.0 / (PI * PI * (h * h) as f64)
    } else {
        0.0
    }
}

/// `t` of the way from spectrum `a` to `b`.
fn blend(a: &[(f64, f64)], b: &[(f64, f64)], t: f64) -> Vec<(f64, f64)> {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x.0 + (y.0 - x.0) * t, x.1 + (y.1 - x.1) * t))
        .collect()
}

/// Frames morphing through `keys` evenly.
fn morph(keys: &[Vec<(f64, f64)>], frames: usize) -> Vec<Vec<(f64, f64)>> {
    (0..frames)
        .map(|k| {
            let x = k as f64 / (frames - 1) as f64 * (keys.len() - 1) as f64;
            let i = (x as usize).min(keys.len() - 2);
            blend(&keys[i], &keys[i + 1], x - i as f64)
        })
        .collect()
}

/// A small deterministic random number in 0…1.
fn hash(n: u64) -> f64 {
    let mut z = n
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(0x6a09_e667);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// Factory table `i`.
fn make_factory(i: usize) -> Table {
    let name = FACTORY[i.min(FACTORY.len() - 1)];
    let n = FACTORY_FRAMES;
    let t = |k: usize| k as f64 / (n - 1) as f64;
    match i {
        // Sine, triangle, saw, square.
        0 => Table::from_spectra(
            name,
            &morph(
                &[
                    sines(|h| if h == 1 { 1.0 } else { 0.0 }),
                    sines(triangle),
                    sines(saw),
                    sines(square),
                ],
                n,
            ),
        ),
        // A pulse narrowing from square to a sliver.
        1 => Table::from_spectra(
            name,
            &(0..n)
                .map(|k| {
                    let w = 0.5 - 0.47 * t(k);
                    additive(|h| {
                        let x = PI * h as f64 * w;
                        let c = 2.0 / (PI * h as f64) * x.sin();
                        (c * x.cos(), c * x.sin())
                    })
                })
                .collect::<Vec<_>>(),
        ),
        // A saw hard-synced to a cycle, its own pitch rising to eight
        // times.
        2 => Table::from_shape(name, n, |k, p| {
            let r = 1.0 + 7.0 * t(k);
            2.0 * (p * r).fract() - 1.0
        }),
        // Harmonics coming in one after another (a saw's levels).
        3 => Table::from_spectra(
            name,
            &(0..n)
                .map(|k| {
                    let top = 64f64.powf(t(k));
                    sines(|h| {
                        let w = (top - (h as f64 - 1.0)).clamp(0.0, 1.0);
                        saw(h) * w
                    })
                })
                .collect::<Vec<_>>(),
        ),
        // Odd harmonics coming in: a sine growing into a square.
        4 => Table::from_spectra(
            name,
            &(0..n)
                .map(|k| {
                    let top = 63f64.powf(t(k));
                    sines(|h| {
                        let w = (top - (h as f64 - 1.0)).clamp(0.0, 1.0);
                        square(h) * w
                    })
                })
                .collect::<Vec<_>>(),
        ),
        // A saw at 130 Hz through the vowels a, e, i, o, u.
        5 => {
            const VOWELS: [[f64; 3]; 5] = [
                [730.0, 1_090.0, 2_440.0],
                [530.0, 1_840.0, 2_480.0],
                [270.0, 2_290.0, 3_010.0],
                [570.0, 840.0, 2_410.0],
                [300.0, 870.0, 2_240.0],
            ];
            let frames = (0..n)
                .map(|k| {
                    let x = t(k) * 4.0;
                    let v = (x as usize).min(3);
                    let f: Vec<f64> = (0..3)
                        .map(|j| VOWELS[v][j] + (VOWELS[v + 1][j] - VOWELS[v][j]) * (x - v as f64))
                        .collect();
                    sines(|h| {
                        let hz = 130.0 * h as f64;
                        let env: f64 = f
                            .iter()
                            .zip([1.0, 0.6, 0.3])
                            .map(|(fc, g)| {
                                let bw = 60.0 + 0.06 * fc;
                                g / (1.0 + ((hz - fc) / bw).powi(2))
                            })
                            .sum::<f64>()
                            + 0.01;
                        saw(h) * env
                    })
                })
                .collect::<Vec<_>>();
            Table::from_spectra(name, &frames)
        }
        // Frequency modulation, its index rising to 8.
        6 => Table::from_shape(name, n, |k, p| {
            (TAU * p + 8.0 * t(k) * (TAU * p).sin()).sin()
        }),
        7 => Table::from_shape(name, n, |k, p| {
            (TAU * 2.0 * p + 6.0 * t(k) * (TAU * 7.0 * p).sin()).sin()
        }),
        // A sine folded back as its gain rises to 8.
        8 => Table::from_shape(name, n, |k, p| {
            let x = (TAU * p).sin() * (1.0 + 7.0 * t(k));
            // Triangle folding into −1…1.
            let y = (x + 1.0).rem_euclid(4.0);
            if y < 2.0 { y - 1.0 } else { 3.0 - y }
        }),
        // A sine on ever fewer steps (64 down to 2).
        9 => Table::from_shape(name, n, |k, p| {
            let steps = 64f64.powf(1.0 - t(k)).max(2.0) / 2.0;
            ((TAU * p).sin() * steps).round() / steps
        }),
        // Drawbar registrations (8', 4', 2⅔', 2', 1⅗', 1⅓', 1').
        10 => {
            const BARS: [[f64; 7]; 5] = [
                [8.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                [8.0, 8.0, 8.0, 0.0, 0.0, 0.0, 0.0],
                [8.0, 6.0, 8.0, 4.0, 0.0, 0.0, 0.0],
                [8.0, 8.0, 8.0, 8.0, 0.0, 0.0, 8.0],
                [8.0, 8.0, 8.0, 8.0, 8.0, 8.0, 8.0],
            ];
            const HARMONIC: [usize; 7] = [1, 2, 3, 4, 5, 6, 8];
            let keys: Vec<Vec<(f64, f64)>> = BARS
                .iter()
                .map(|bars| {
                    sines(|h| {
                        HARMONIC
                            .iter()
                            .position(|x| *x == h)
                            // Each drawbar step 3 dB.
                            .map_or(0.0, |i| {
                                if bars[i] > 0.0 {
                                    10f64.powf(-3.0 * (8.0 - bars[i]) / 20.0)
                                } else {
                                    0.0
                                }
                            })
                    })
                })
                .collect();
            Table::from_spectra(name, &morph(&keys, n))
        }
        // A saw through a resonant low pass sweeping up from its second
        // harmonic to its 128th.
        11 => Table::from_spectra(
            name,
            &(0..n)
                .map(|k| {
                    let fc = 2.0 * 64f64.powf(t(k));
                    sines(|h| {
                        let x = h as f64 / fc;
                        let q = 4.0;
                        let m = 1.0 / ((1.0 - x * x).powi(2) + (x / q).powi(2)).sqrt();
                        saw(h) * m.min(q)
                    })
                })
                .collect::<Vec<_>>(),
        ),
        // Random spectra (each falling off), one into the next.
        _ => {
            let keys: Vec<Vec<(f64, f64)>> = (0..8u64)
                .map(|s| {
                    additive(|h| {
                        let a = hash(s * 4099 + h as u64) / (h as f64).powf(0.8);
                        let ph = TAU * hash(s * 7919 + h as u64 + 17);
                        (a * ph.cos(), a * ph.sin())
                    })
                })
                .collect();
            Table::from_spectra(name, &morph(&keys, n))
        }
    }
}

/// The frame size a WAV's `clm ` chunk names ("<!>2048 …").
pub fn clm_frame_size(path: &Path) -> Option<usize> {
    let body = faderframe_audio_files::wavstream::read_chunk(path, b"clm ")
        .ok()
        .flatten()?;
    let text = String::from_utf8_lossy(&body);
    let rest = text.split("<!>").nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|n| (4..=65_536).contains(n))
}

/// The frame size of a file of `len` samples without one given: the
/// whole file when short (a single cycle), else the first of the usual
/// sizes it is made of.
pub fn guess_frame_size(len: usize) -> usize {
    if len <= 4_096 {
        return len.max(4);
    }
    [2048, 1024, 512, 256, 4096]
        .into_iter()
        .find(|n| len.is_multiple_of(*n))
        .unwrap_or(CYCLE)
}

/// A user table from decoded audio (mono: the channels' mean) cut into
/// frames of `size` samples (at most [`MAX_FRAMES`], spread over the
/// file when it holds more).
pub fn from_audio(name: &str, left: &[f32], right: &[f32], size: usize) -> Option<Table> {
    let size = size.max(4);
    let len = left.len().min(right.len());
    let count = len / size;
    if count == 0 {
        return None;
    }
    let picks: Vec<usize> = if count <= MAX_FRAMES {
        (0..count).collect()
    } else {
        (0..MAX_FRAMES)
            .map(|k| k * (count - 1) / (MAX_FRAMES - 1))
            .collect()
    };
    let cycles: Vec<Vec<f32>> = picks
        .into_iter()
        .map(|f| {
            (0..size)
                .map(|i| 0.5 * (left[f * size + i] + right[f * size + i]))
                .collect()
        })
        .collect();
    Some(Table::from_cycles(name, &cycles))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn levels_keep_fewer_harmonics_an_octave_apart() {
        assert_eq!(harmonics(0), MAX_HARMONICS);
        assert_eq!(harmonics(1), 512);
        assert_eq!(harmonics(LEVELS - 1), 1);
        for l in 0..LEVELS {
            assert!(level_size(l) > 2 * harmonics(l) || l == 0, "{l}");
            assert!(level_size(l).is_power_of_two());
        }
        // Low notes read the full table, high ones the thin copies, and a
        // level's top harmonic stays below Nyquist while it is heard.
        let m = Mip::for_increment(5.0 / 48_000.0);
        assert_eq!((m.a, m.mix), (0, 0.0));
        for f in [55.0f32, 440.0, 1_000.0, 3_000.0, 9_000.0] {
            let inc = f / 48_000.0;
            let m = Mip::for_increment(inc);
            assert!(harmonics(m.a) as f32 * inc <= 0.5, "{f}: {m:?}");
            if m.mix > 0.0 {
                assert!(harmonics(m.b) as f32 * inc <= 0.5);
            }
        }
    }

    /// A glide never steps from one copy to the next: each level's
    /// share moves smoothly with the pitch.
    #[test]
    fn the_copies_crossfade_as_the_pitch_moves() {
        let shares = |inc: f32| {
            let m = Mip::for_increment(inc);
            let mut w = [0.0f32; LEVELS];
            w[m.a] += 1.0 - m.mix;
            w[m.b] += m.mix;
            w
        };
        let mut inc = 1e-4f32;
        let mut last = shares(inc);
        while inc < 0.4 {
            inc *= 1.001;
            let now = shares(inc);
            let step = now
                .iter()
                .zip(&last)
                .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(step < 0.01, "a jump of {step} at {inc}");
            last = now;
        }
    }

    #[test]
    fn a_table_plays_its_cycles_band_limited() {
        let t = Table::from_spectra("Saw", &[sines(saw)]);
        assert_eq!(t.frames, 1);
        let c = t.cycle(0);
        assert_eq!(c.len(), CYCLE);
        let peak = c.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 1.0).abs() < 1e-4, "normalised: {peak}");
        // The thinnest copy is the fundamental alone.
        let f = t.frames_at(0.0);
        let m = Mip {
            a: LEVELS - 1,
            b: LEVELS - 1,
            mix: 0.0,
        };
        let a = t.read(m, f, 0.25);
        let b = t.read(m, f, 0.75);
        assert!(a > 0.1 && (a + b).abs() < 1e-3, "a sine: {a} {b}");
    }

    #[test]
    fn frames_interpolate_and_user_audio_cuts_into_frames() {
        let mut audio = Vec::new();
        for f in 0..4 {
            audio.extend(
                (0..256).map(|i| ((TAU * i as f64 / 256.0 * (f + 1) as f64).sin() * 0.5) as f32),
            );
        }
        let t = from_audio("x", &audio, &audio, 256).unwrap();
        assert_eq!(t.frames, 4);
        assert_eq!(t.frames_at(1.0).f0, 3);
        let mid = t.frames_at(0.5);
        assert_eq!((mid.f0, mid.f1), (1, 2));
        assert!((mid.mix - 0.5).abs() < 1e-6);
        assert_eq!(guess_frame_size(600), 600);
        assert_eq!(guess_frame_size(2048 * 64), 2048);
        assert_eq!(guess_frame_size(256 * 33), 256);
        assert!(from_audio("x", &audio[..10], &audio[..10], 256).is_none());
    }

    #[test]
    fn the_factory_tables_are_made_and_normalised() {
        let all = factory();
        assert_eq!(all.len(), FACTORY.len());
        for t in all {
            assert_eq!(t.frames, FACTORY_FRAMES, "{}", t.name);
            let peak = (0..t.frames)
                .flat_map(|f| t.cycle(f).iter())
                .fold(0.0f32, |m, v| m.max(v.abs()));
            assert!((peak - 1.0).abs() < 1e-3, "{}: {peak}", t.name);
            assert!(
                (0..t.frames).all(|f| t.cycle(f).iter().all(|v| v.is_finite())),
                "{}",
                t.name
            );
        }
    }
}
