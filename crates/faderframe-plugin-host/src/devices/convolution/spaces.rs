//! The Convolution Reverb's built-in spaces: responses made here, never
//! recordings (no one else's rooms ship with FaderFrame), true stereo
//! (two loudspeakers heard by a spaced pair of microphones).
//!
//! A room is a shoebox. Its early part is an image-source model (Allen and
//! Berkley): every mirror image of each loudspeaker within the mixing time
//! is an impulse at its distance's delay, `β^k / r` loud for `k`
//! reflections, with `β` the walls' reflection (Eyring: the energy that
//! decays over the room's middle RT60, `β² = exp(−0.161 V / (S T))`); the
//! reflections are grouped by order and the higher orders low-passed
//! (walls take the highs first). The late part is noise in three bands
//! (Linkwitz–Riley splits at 300 Hz and 3 kHz), each decaying over its own
//! RT60, faded in across the mixing time at the level a diffuse field has
//! in a room of that volume (Sabine: its energy against the direct sound's
//! at a distance is `(r / r_c)²`, `r_c = 0.057 √(V/T)`). A plate is that
//! late part alone, dense from the start. Every path has noise of its own:
//! the outputs are decorrelated.

use crate::dsp::filter::{Crossover, OnePole};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// A built-in space.
pub struct Space {
    pub name: &'static str,
    /// Length, width and height (m); `None`: a plate.
    pub room: Option<[f64; 3]>,
    /// RT60 (s) below 300 Hz, in the middle, above 3 kHz.
    pub decay: [f64; 3],
    /// When the late part has taken over (ms).
    pub mixing_ms: f64,
    /// The microphones' spacing (m).
    pub spacing: f64,
    /// How bright the walls are (the cutoff of the first order's low pass,
    /// Hz; each further group halves it).
    pub walls_hz: f64,
}

pub const SPACES: [Space; 8] = [
    Space {
        name: "Vocal Booth",
        room: Some([2.2, 1.8, 2.3]),
        decay: [0.28, 0.2, 0.12],
        mixing_ms: 18.0,
        spacing: 0.25,
        walls_hz: 6_000.0,
    },
    Space {
        name: "Small Room",
        room: Some([4.6, 3.6, 2.7]),
        decay: [0.55, 0.45, 0.32],
        mixing_ms: 30.0,
        spacing: 0.4,
        walls_hz: 9_000.0,
    },
    Space {
        name: "Studio",
        room: Some([9.0, 6.5, 3.8]),
        decay: [0.85, 0.7, 0.5],
        mixing_ms: 45.0,
        spacing: 0.6,
        walls_hz: 8_000.0,
    },
    Space {
        name: "Chamber",
        room: Some([6.5, 4.2, 3.6]),
        decay: [1.7, 1.5, 1.1],
        mixing_ms: 40.0,
        spacing: 0.5,
        walls_hz: 12_000.0,
    },
    Space {
        name: "Hall",
        room: Some([26.0, 17.0, 12.0]),
        decay: [2.5, 2.1, 1.4],
        mixing_ms: 80.0,
        spacing: 0.8,
        walls_hz: 7_000.0,
    },
    Space {
        name: "Large Hall",
        room: Some([42.0, 26.0, 18.0]),
        decay: [3.6, 2.9, 1.9],
        mixing_ms: 100.0,
        spacing: 1.0,
        walls_hz: 6_000.0,
    },
    Space {
        name: "Cathedral",
        room: Some([75.0, 32.0, 28.0]),
        decay: [6.5, 5.2, 3.1],
        mixing_ms: 140.0,
        spacing: 1.2,
        walls_hz: 5_000.0,
    },
    Space {
        name: "Plate",
        room: None,
        decay: [2.0, 1.9, 1.6],
        mixing_ms: 0.0,
        spacing: 0.0,
        walls_hz: 16_000.0,
    },
];

/// The speed of sound (m/s).
const SOUND: f64 = 343.0;
/// The longest a built-in response is (seconds).
const LONGEST: f64 = 8.0;

/// A built-in space's response at a rate: four channels (left into left,
/// left into right, right into left, right into right).
pub struct Rendered {
    pub rate: f64,
    pub channels: Vec<Vec<f32>>,
}

type Cache = Mutex<Vec<((usize, u64), Weak<Rendered>)>>;

/// Space `i`'s response at `rate`, shared while anyone holds it.
pub fn render(i: usize, rate: f64) -> Arc<Rendered> {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (i, rate.to_bits());
    if let Ok(c) = cache.lock()
        && let Some(r) = c
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, w)| w.upgrade())
    {
        return r;
    }
    let r = Arc::new(make(&SPACES[i.min(SPACES.len() - 1)], rate));
    if let Ok(mut c) = cache.lock() {
        c.retain(|(_, w)| w.strong_count() > 0);
        c.push((key, Arc::downgrade(&r)));
    }
    r
}

/// A deterministic Gaussian-ish noise (four uniforms summed).
struct Noise(u32);

impl Noise {
    fn uniform(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        f64::from(x) / f64::from(u32::MAX) * 2.0 - 1.0
    }

    fn next(&mut self) -> f64 {
        (self.uniform() + self.uniform() + self.uniform() + self.uniform()) * 0.866
    }
}

fn make(space: &Space, rate: f64) -> Rendered {
    let longest = space.decay.iter().copied().fold(0.0, f64::max);
    let len = ((longest * 1.15 + space.mixing_ms * 1e-3) * rate).min(LONGEST * rate) as usize;
    let mixing = space.mixing_ms * 1e-3;
    let mut channels = Vec::with_capacity(4);
    for path in 0..4 {
        let (speaker, mic) = (path / 2, path % 2);
        let mut h = vec![0.0f32; len];
        let level = match space.room {
            Some(room) => {
                early(&mut h, space, room, speaker, mic, rate);
                // Sabine's diffuse level (direct sound at 1 m is 1).
                let v = room[0] * room[1] * room[2];
                (4_248.0 / (rate * v)).sqrt()
            }
            None => 0.02 * (48_000.0 / rate).sqrt(),
        };
        late(
            &mut h,
            space,
            level,
            mixing,
            rate,
            0x9e37_79b9 ^ (path as u32 * 7_919 + 17),
        );
        channels.push(h);
    }
    Rendered { rate, channels }
}

/// The image sources' impulses (up to the mixing time).
fn early(h: &mut [f32], space: &Space, room: [f64; 3], speaker: usize, mic: usize, rate: f64) {
    let [lx, ly, lz] = room;
    let volume = lx * ly * lz;
    let surface = 2.0 * (lx * ly + lx * lz + ly * lz);
    let beta = (-0.0805 * volume / (surface * space.decay[1])).exp();
    let z = (0.4 * lz).clamp(1.2, 3.0).min(lz * 0.8);
    let sep = (0.18 * lx).min(3.0);
    let source = [
        0.5 * lx + if speaker == 0 { -sep } else { sep },
        0.3 * ly,
        z,
    ];
    let half = space.spacing / 2.0;
    let listener = [0.5 * lx + if mic == 0 { -half } else { half }, 0.65 * ly, z];
    let end = space.mixing_ms * 1e-3;
    let reach = end * SOUND;
    // Groups by reflections: direct, 1–2, 3–5, 6 and more.
    let mut groups = vec![vec![0.0f32; h.len().min((end * rate) as usize + 8)]; 4];
    let n_of = |l: f64| (reach / (2.0 * l)).ceil() as i64 + 1;
    let (nx, ny, nz) = (n_of(lx), n_of(ly), n_of(lz));
    for px in 0..2 {
        for py in 0..2 {
            for pz in 0..2 {
                for ix in -nx..=nx {
                    for iy in -ny..=ny {
                        for iz in -nz..=nz {
                            let axis = |n: i64, p: i64, s: f64, l: f64| {
                                let x = 2.0 * n as f64 * l + if p == 0 { s } else { -s };
                                let k = if p == 0 {
                                    2 * n.abs()
                                } else {
                                    (2 * n - 1).abs()
                                };
                                (x, k)
                            };
                            let (x, kx) = axis(ix, px, source[0], lx);
                            let (y, ky) = axis(iy, py, source[1], ly);
                            let (zz, kz) = axis(iz, pz, source[2], lz);
                            let d = ((x - listener[0]).powi(2)
                                + (y - listener[1]).powi(2)
                                + (zz - listener[2]).powi(2))
                            .sqrt()
                            .max(0.3);
                            if d > reach {
                                continue;
                            }
                            let k = kx + ky + kz;
                            let gain = beta.powi(k as i32) / d;
                            let at = d / SOUND * rate;
                            let g = &mut groups[match k {
                                0 => 0,
                                1..=2 => 1,
                                3..=5 => 2,
                                _ => 3,
                            }];
                            let i = at.floor() as usize;
                            let f = at - i as f64;
                            if i + 1 < g.len() {
                                g[i] += (gain * (1.0 - f)) as f32;
                                g[i + 1] += (gain * f) as f32;
                            }
                        }
                    }
                }
            }
        }
    }
    // Each group darker than the one before; fading out at the end of
    // the mixing time.
    let fade_from = 0.75 * end * rate;
    for (gi, g) in groups.iter().enumerate() {
        let mut lp = (gi > 0).then(|| {
            let cut = (space.walls_hz / f64::from(1u32 << (gi - 1))).min(0.45 * rate);
            [OnePole::new(cut, rate), OnePole::new(cut, rate)]
        });
        for (n, v) in g.iter().enumerate() {
            let mut x = f64::from(*v);
            if let Some(p) = &mut lp {
                let a = p[0].low(x);
                x = p[1].low(a);
            }
            let t = n as f64;
            let w = if t <= fade_from {
                1.0
            } else {
                (1.0 - (t - fade_from) / (end * rate - fade_from).max(1.0)).clamp(0.0, 1.0)
            };
            if n < h.len() {
                h[n] += (x * w) as f32;
            }
        }
    }
}

/// The diffuse tail: three bands of noise, each decaying over its RT60,
/// faded in over the mixing time.
fn late(h: &mut [f32], space: &Space, level: f64, mixing: f64, rate: f64, seed: u32) {
    let mut noise = Noise(seed);
    let mut low = Crossover::new(300.0, rate);
    let mut high = Crossover::new(3_000.0f64.min(0.4 * rate), rate);
    // Per sample decay of each band's amplitude (−60 dB over its RT60).
    let k = space
        .decay
        .map(|t| (-6.907_755 / (t.max(0.05) * rate)).exp());
    let mut env = [level; 3];
    let start = 0.5 * mixing * rate;
    let full = mixing * rate;
    let onset = if space.room.is_none() {
        0.0005 * rate
    } else {
        0.0
    };
    for (n, out) in h.iter_mut().enumerate() {
        let x = noise.next();
        let (l, rest) = low.split(x);
        let (m, hi) = high.split(rest);
        let y = l * env[0] + m * env[1] + hi * env[2];
        for (e, kk) in env.iter_mut().zip(&k) {
            *e *= kk;
        }
        let t = n as f64;
        let w = if space.room.is_none() {
            (t / onset.max(1.0)).min(1.0)
        } else if t < start {
            0.0
        } else if t < full {
            let u = (t - start) / (full - start).max(1.0);
            0.5 - 0.5 * (std::f64::consts::PI * u).cos()
        } else {
            1.0
        };
        *out += (y * w) as f32;
    }
}

/// The RT60 a response's decay shows (Schroeder's backward integration,
/// the slope from −5 to −25 dB doubled: T20), if it falls that far.
pub fn rt60(h: &[f32], rate: f64) -> Option<f64> {
    let mut acc = 0.0f64;
    let mut curve = vec![0.0f64; h.len()];
    for (i, v) in h.iter().enumerate().rev() {
        acc += f64::from(*v).powi(2);
        curve[i] = acc;
    }
    let total = curve.first().copied().filter(|t| *t > 0.0)?;
    let db = |i: usize| 10.0 * (curve[i] / total).max(1e-30).log10();
    let a = (0..h.len()).find(|&i| db(i) <= -5.0)?;
    let b = (a..h.len()).find(|&i| db(i) <= -25.0)?;
    Some(3.0 * (b - a) as f64 / rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_space_decays_over_its_own_time() {
        for (i, s) in SPACES.iter().enumerate() {
            let r = render(i, 48_000.0);
            assert_eq!(r.channels.len(), 4);
            for c in &r.channels {
                assert!(c.iter().all(|v| v.is_finite()));
                let rt = rt60(c, 48_000.0).unwrap();
                // The early part shortens the start of the decay a little;
                // the middle band rules it.
                assert!(
                    rt > 0.6 * s.decay[1] && rt < 1.35 * s.decay[0],
                    "{}: RT60 {rt:.2} s for {:.2}",
                    s.name,
                    s.decay[1]
                );
            }
            // The tails are decorrelated (the early parts of mirrored paths
            // are alike, as in a symmetrical room).
            let from = (2.0 * s.mixing_ms * 48.0) as usize;
            let (a, b) = (&r.channels[0][from..], &r.channels[3][from..]);
            let dot: f64 = a.iter().zip(b).map(|(x, y)| f64::from(*x * *y)).sum();
            let ea: f64 = a.iter().map(|x| f64::from(*x).powi(2)).sum();
            let eb: f64 = b.iter().map(|x| f64::from(*x).powi(2)).sum();
            assert!(dot.abs() / (ea * eb).sqrt() < 0.6, "{}", s.name);
        }
        // Shared while held.
        let a = render(1, 44_100.0);
        let b = render(1, 44_100.0);
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn a_room_starts_with_its_direct_sound_and_a_plate_at_once() {
        let r = render(1, 48_000.0);
        // Left speaker to left mic: the direct sound arrives first.
        let first = r.channels[0].iter().position(|v| v.abs() > 1e-4).unwrap();
        let d = first as f64 / 48_000.0 * SOUND;
        assert!(d > 1.0 && d < 3.5, "{d:.2} m");
        let plate = render(SPACES.len() - 1, 48_000.0);
        assert!(plate.channels[0][..48].iter().any(|v| v.abs() > 0.0));
    }
}
