//! The corpus's sounds, made here (deterministic, seeded): sung phrases (a
//! glottal pulse through vowel formants, with scoops, vibrato, notes a
//! little off, falls, breaths and consonants, the loudness moving),
//! whistling, a plucked lead line, strummed and picked guitar chords and
//! piano chords; each with its notes (the truth), then put at an input
//! level with the room's noise or mains hum.

use std::f64::consts::PI;

pub const RATE: f64 = 48_000.0;
/// Room before the first note (s): the input as it is before anyone sings.
pub const LEAD_IN: f64 = 0.8;

pub fn hz(key: f64) -> f64 {
    440.0 * 2f64.powf((key - 69.0) / 12.0)
}

/// A note that was sung or played: key and span (s).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    pub key: u8,
    pub on: f64,
    pub off: f64,
}

/// A sound and its notes, its peak at full scale.
pub struct Piece {
    pub name: &'static str,
    pub audio: Vec<f32>,
    pub truth: Vec<Note>,
}

/// xorshift64*: the same numbers on every machine.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0 … 1.
    pub fn uniform(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.uniform()
    }

    /// −1 … 1.
    pub fn white(&mut self) -> f64 {
        2.0 * self.uniform() - 1.0
    }

    /// About normal (the sum of four uniforms), unit variance.
    pub fn gauss(&mut self) -> f64 {
        (self.uniform() + self.uniform() + self.uniform() + self.uniform() - 2.0) * 3f64.sqrt()
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.uniform() < p
    }
}

/// A two-pole resonator with unity gain at DC (Klatt's).
#[derive(Clone, Copy, Debug, Default)]
struct Resonator {
    a: f64,
    b: f64,
    c: f64,
    y1: f64,
    y2: f64,
}

impl Resonator {
    fn tuned(f: f64, bw: f64) -> Self {
        let mut r = Self::default();
        r.set(f, bw);
        r
    }

    fn set(&mut self, f: f64, bw: f64) {
        let t = 1.0 / RATE;
        self.c = -(-2.0 * PI * bw * t).exp();
        self.b = 2.0 * (-PI * bw * t).exp() * (2.0 * PI * f * t).cos();
        self.a = 1.0 - self.b - self.c;
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.a * x + self.b * self.y1 + self.c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Noise between two corners (a band-pass of two resonators).
struct Band(Resonator, Resonator);

impl Band {
    fn new(centre: f64, width: f64) -> Self {
        Self(
            Resonator::tuned(centre, width),
            Resonator::tuned(centre, width),
        )
    }

    /// Band-passed `x` (the resonators' DC gain taken off by a difference).
    fn run(&mut self, x: f64, last: &mut f64) -> f64 {
        let d = x - *last;
        *last = x;
        self.1.run(self.0.run(d))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vowel {
    Ah,
    Eh,
    Ee,
    Oh,
    Oo,
}

impl Vowel {
    /// Formants (Hz) and bandwidths of a man's vowel (Peterson and
    /// Barney's averages).
    fn formants(self) -> [(f64, f64); 4] {
        match self {
            Vowel::Ah => [
                (730.0, 90.0),
                (1090.0, 110.0),
                (2440.0, 160.0),
                (3400.0, 250.0),
            ],
            Vowel::Eh => [
                (530.0, 70.0),
                (1840.0, 100.0),
                (2480.0, 160.0),
                (3400.0, 250.0),
            ],
            Vowel::Ee => [
                (270.0, 60.0),
                (2290.0, 100.0),
                (3010.0, 170.0),
                (3600.0, 250.0),
            ],
            Vowel::Oh => [
                (570.0, 80.0),
                (840.0, 90.0),
                (2410.0, 160.0),
                (3400.0, 250.0),
            ],
            Vowel::Oo => [
                (300.0, 60.0),
                (870.0, 90.0),
                (2240.0, 150.0),
                (3400.0, 250.0),
            ],
        }
    }
}

/// One sung note of a phrase: key, length, the gap after it (0: legato
/// into the next), the vowel and whether a breath is taken in the gap.
#[derive(Clone, Copy, Debug)]
pub struct Step {
    pub key: u8,
    pub dur: f64,
    pub gap: f64,
    pub vowel: Vowel,
    pub breath: bool,
}

pub const fn step(key: u8, dur: f64, gap: f64, vowel: Vowel) -> Step {
    Step {
        key,
        dur,
        gap,
        vowel,
        breath: false,
    }
}

pub const fn breath(key: u8, dur: f64, gap: f64, vowel: Vowel) -> Step {
    Step {
        key,
        dur,
        gap,
        vowel,
        breath: true,
    }
}

/// How one note is sung (drawn per note).
#[derive(Clone, Copy, Debug)]
struct Sung {
    key: f64,
    on: f64,
    off: f64,
    legato_in: bool,
    legato_out: bool,
    /// Cents off the key.
    detune: f64,
    /// From this far under (cents) over this long (s).
    scoop: f64,
    scoop_s: f64,
    vib_cents: f64,
    vib_hz: f64,
    vib_delay: f64,
    vib_phase: f64,
    /// Falls off this far (cents) over the last `fall_s`.
    fall: f64,
    fall_s: f64,
    loud: f64,
    attack: f64,
    release: f64,
    formants: [(f64, f64); 4],
}

impl Sung {
    /// The pitch (fractional key) `t` s into the piece, alone.
    fn pitch(&self, t: f64) -> f64 {
        let x = t - self.on;
        let mut cents = self.detune;
        if !self.legato_in && self.scoop != 0.0 {
            let k = (x / self.scoop_s).clamp(0.0, 1.0);
            // Approaches the note exponentially-ish.
            cents -= self.scoop * (1.0 - k).powi(2);
        }
        if x > self.vib_delay {
            let fade = ((x - self.vib_delay) / 0.25).min(1.0);
            cents += fade
                * self.vib_cents
                * (2.0 * PI * self.vib_hz * (x - self.vib_delay) + self.vib_phase).sin();
        }
        let left = self.off - t;
        if !self.legato_out && self.fall != 0.0 && left < self.fall_s {
            let k = 1.0 - left / self.fall_s;
            cents -= self.fall * k * k;
        }
        self.key + cents / 100.0
    }

    /// The loudness `t` s into the piece, alone.
    fn amp(&self, t: f64) -> f64 {
        let x = t - self.on;
        let mut a = self.loud * (1.0 + 0.12 * (2.0 * PI * 0.8 * x).sin());
        if !self.legato_in {
            let k = (x / self.attack).clamp(0.0, 1.0);
            a *= k * k * (3.0 - 2.0 * k);
        }
        if !self.legato_out {
            let k = ((self.off - t) / self.release).clamp(0.0, 1.0);
            a *= k * k * (3.0 - 2.0 * k);
        }
        a
    }
}

/// A sung phrase: a man's (`female` false) or a woman's voice; `seed`
/// draws how each note is sung.
pub fn sung(name: &'static str, steps: &[Step], female: bool, seed: u64) -> Piece {
    let mut rng = Rng::new(seed);
    let mut notes: Vec<Sung> = Vec::new();
    let mut truth = Vec::new();
    let mut t = LEAD_IN;
    let scale = if female { 1.17 } else { 1.0 };
    for (i, s) in steps.iter().enumerate() {
        let legato_in = i > 0 && steps[i - 1].gap == 0.0;
        let legato_out = s.gap == 0.0 && i + 1 < steps.len();
        let mut formants = s.vowel.formants();
        for (k, f) in formants.iter_mut().enumerate() {
            if k < 3 {
                f.0 *= scale;
            }
        }
        let long = s.dur > 0.35;
        notes.push(Sung {
            key: f64::from(s.key),
            on: t,
            off: t + s.dur,
            legato_in,
            legato_out,
            detune: rng.range(-28.0, 28.0),
            scoop: if rng.chance(0.55) {
                rng.range(60.0, 160.0)
            } else {
                0.0
            },
            scoop_s: rng.range(0.05, 0.11),
            vib_cents: if long { rng.range(25.0, 65.0) } else { 0.0 },
            vib_hz: rng.range(5.0, 6.6),
            vib_delay: rng.range(0.12, 0.3),
            vib_phase: rng.range(0.0, 2.0 * PI),
            fall: if !legato_out && rng.chance(0.3) {
                rng.range(80.0, 220.0)
            } else {
                0.0
            },
            fall_s: rng.range(0.06, 0.1),
            loud: rng.range(0.35, 1.0),
            attack: rng.range(0.02, 0.07),
            release: rng.range(0.04, 0.09),
            formants,
        });
        truth.push(Note {
            key: s.key,
            on: t,
            off: t + s.dur,
        });
        t += s.dur + s.gap;
    }
    let total = t + 0.8;
    let n = (total * RATE) as usize;
    let mut out = vec![0.0f32; n];
    // The glottal pulse (Rosenberg's flow derivative), jitter and shimmer
    // per period, aspiration in the open phase.
    let (open, close) = (0.42, 0.16);
    let mut phase = 0.0f64;
    let mut jitter = 1.0;
    let mut shimmer = 1.0;
    let mut tract = [Resonator::default(); 4];
    let mut shape = notes[0].formants;
    let mut current = 0usize;
    let mut breath_band = Band::new(1_600.0, 1_800.0);
    let mut breath_last = 0.0;
    let mut hiss_band = Band::new(5_000.0, 3_000.0);
    let mut hiss_last = 0.0;
    // Consonants before some detached notes: (start, end, kind 0 "t" 1 "s").
    let mut consonants = Vec::new();
    let mut breaths = Vec::new();
    for (i, s) in notes.iter().enumerate() {
        if !s.legato_in && rng.chance(0.35) {
            if rng.chance(0.5) {
                consonants.push((s.on - 0.014, s.on, 0));
            } else {
                consonants.push((s.on - 0.11, s.on - 0.01, 1));
            }
        }
        let gap = steps[i].gap;
        if steps[i].breath && gap >= 0.3 {
            let len = (gap - 0.12).min(0.45);
            let mid = s.off + gap / 2.0;
            breaths.push((mid - len / 2.0, mid + len / 2.0, rng.range(0.1, 0.2)));
        }
    }
    for (j, o) in out.iter_mut().enumerate() {
        let t = j as f64 / RATE;
        while current + 1 < notes.len() && t >= notes[current + 1].on - 0.04 {
            current += 1;
        }
        let s = &notes[current];
        // Pitch and loudness: legato boundaries glide over 70 ms.
        let (mut pitch, mut amp) = (s.pitch(t), s.amp(t));
        if t < s.on || t >= s.off {
            amp = 0.0;
        }
        let glide = 0.07;
        if s.legato_in && (t - s.on).abs() < glide / 2.0 {
            let p = &notes[current - 1];
            let k = 0.5 - 0.5 * (PI * (t - s.on + glide / 2.0) / glide).cos();
            pitch = p.pitch(t) * (1.0 - k) + s.pitch(t) * k;
            amp = (p.amp(t) * (1.0 - k) + s.amp(t) * k) * (1.0 - 0.15 * (PI * k).sin());
        } else if s.legato_out && current + 1 < notes.len() && s.off - t < glide / 2.0 {
            let q = &notes[current + 1];
            let k = 0.5 - 0.5 * (PI * (t - q.on + glide / 2.0) / glide).cos();
            pitch = s.pitch(t) * (1.0 - k) + q.pitch(t) * k;
            amp = (s.amp(t) * (1.0 - k) + q.amp(t) * k) * (1.0 - 0.15 * (PI * k).sin());
        }
        // The vowel glides to the note's.
        if j % 16 == 0 {
            for (k, r) in tract.iter_mut().enumerate() {
                shape[k].0 += 0.015 * (s.formants[k].0 - shape[k].0);
                shape[k].1 += 0.015 * (s.formants[k].1 - shape[k].1);
                r.set(shape[k].0, shape[k].1);
            }
        }
        let f = hz(pitch) * jitter;
        phase += f / RATE;
        if phase >= 1.0 {
            phase -= 1.0;
            jitter = 1.0 + 0.004 * rng.gauss();
            shimmer = 1.0 + 0.03 * rng.gauss();
        }
        let source = if phase < open {
            PI / (2.0 * open) * (PI * phase / open).sin()
        } else if phase < open + close {
            -PI / (2.0 * close) * (PI * (phase - open) / (2.0 * close)).sin()
        } else {
            0.0
        };
        let aspiration = rng.white() * if phase < open + close { 0.25 } else { 0.08 };
        let mut v = (source * shimmer + aspiration) * amp;
        for r in &mut tract {
            v = r.run(v);
        }
        // Breaths and consonants: noise.
        let w = rng.white();
        let mut noise = 0.0;
        for &(a, b, level) in &breaths {
            if (a..b).contains(&t) {
                let k = ((t - a) / 0.08).min((b - t) / 0.08).min(1.0);
                noise += breath_band.run(w, &mut breath_last) * level * 6.0 * k;
            }
        }
        for &(a, b, kind) in &consonants {
            if (a..b).contains(&t) {
                noise += if kind == 0 {
                    w * 0.35
                } else {
                    hiss_band.run(w, &mut hiss_last) * 2.0
                };
            }
        }
        *o = (v + noise) as f32;
    }
    normalise(&mut out);
    Piece {
        name,
        audio: out,
        truth,
    }
}

/// Whistling: a nearly pure tone sliding between notes, air noise around
/// it.
pub fn whistle(name: &'static str, steps: &[(u8, f64, f64)], seed: u64) -> Piece {
    let mut rng = Rng::new(seed);
    let mut truth = Vec::new();
    let mut t = LEAD_IN;
    // (key with its detune, on, off, legato in)
    let mut plan = Vec::new();
    for (i, &(key, dur, gap)) in steps.iter().enumerate() {
        let legato_in = i > 0 && steps[i - 1].2 == 0.0;
        plan.push((
            f64::from(key) + rng.range(-0.2, 0.2),
            t,
            t + dur,
            legato_in,
            rng.range(0.4, 1.0),
            rng.range(0.0, 2.0 * PI),
        ));
        truth.push(Note {
            key,
            on: t,
            off: t + dur,
        });
        t += dur + gap;
    }
    let n = ((t + 0.6) * RATE) as usize;
    let mut out = vec![0.0f32; n];
    let mut phase = 0.0f64;
    let mut current = 0usize;
    let mut air = Band::new(1_500.0, 2_000.0);
    let mut air_last = 0.0;
    for (j, o) in out.iter_mut().enumerate() {
        let t = j as f64 / RATE;
        while current + 1 < plan.len() && t >= plan[current + 1].1 - 0.03 {
            current += 1;
        }
        let (key, on, off, legato_in, loud, vp) = plan[current];
        let mut pitch = key + 0.18 * (2.0 * PI * 5.2 * (t - on) + vp).sin();
        let glide = 0.05;
        if legato_in && t - on < glide / 2.0 {
            let prev = plan[current - 1].0;
            let k = 0.5 - 0.5 * (PI * (t - on + glide / 2.0) / glide).cos();
            pitch = prev * (1.0 - k) + pitch * k;
        } else if !legato_in {
            // A whistle starts a little flat.
            let k = ((t - on) / 0.04).clamp(0.0, 1.0);
            pitch -= 0.4 * (1.0 - k);
        }
        let legato_out = current + 1 < plan.len() && plan[current + 1].3;
        let mut amp = loud;
        if !legato_in {
            amp *= ((t - on) / 0.03).clamp(0.0, 1.0);
        }
        if !legato_out {
            amp *= ((off - t) / 0.04).clamp(0.0, 1.0);
        }
        if t < on || t >= off + if legato_out { 0.03 } else { 0.0 } {
            amp = 0.0;
        }
        phase = (phase + hz(pitch) / RATE).fract();
        let v = (2.0 * PI * phase).sin() + 0.04 * (4.0 * PI * phase).sin();
        let breath = air.run(rng.white(), &mut air_last) * 0.5;
        *o = ((v + breath) * amp) as f32;
    }
    normalise(&mut out);
    Piece {
        name,
        audio: out,
        truth,
    }
}

/// Partials of a string decaying each at its own rate: (frequency, level,
/// decay per second, phase) summed from `at` and damped at `until`.
fn partials(x: &mut [f32], parts: &[(f64, f64, f64, f64)], at: f64, until: f64, damp: f64) {
    let i0 = (at * RATE) as usize;
    let i1 = (((until + damp) * RATE) as usize).min(x.len());
    let mut state: Vec<[f64; 5]> = parts
        .iter()
        .map(|&(f, level, _, ph)| {
            let w = 2.0 * PI * f / RATE;
            [ph.cos(), ph.sin(), w.cos(), w.sin(), level]
        })
        .collect();
    let falls: Vec<f64> = parts.iter().map(|p| (-p.2 / RATE).exp()).collect();
    for (j, o) in x.iter_mut().enumerate().take(i1).skip(i0) {
        let t = (j - i0) as f64 / RATE;
        let mut v = 0.0;
        for (p, fall) in state.iter_mut().zip(&falls) {
            let (c, s) = (p[0] * p[2] - p[1] * p[3], p[0] * p[3] + p[1] * p[2]);
            p[0] = c;
            p[1] = s;
            v += s * p[4];
            p[4] *= fall;
        }
        let env = ((until + damp - at - t) / damp).clamp(0.0, 1.0);
        *o += (v * env) as f32;
    }
}

/// A guitar string plucked at `at` (with its pick click), damped at `until`.
fn string(x: &mut [f32], key: u8, at: f64, until: f64, gain: f64, rng: &mut Rng) {
    let f0 = hz(f64::from(key));
    let position = rng.range(0.11, 0.24);
    let parts: Vec<(f64, f64, f64, f64)> = (1..=40)
        .map(f64::from)
        .map(|h| {
            let f = h * f0 * (1.0 + 6e-5 * h * h).sqrt();
            let level = (PI * h * position).sin().abs() / h.powf(1.1) * gain;
            (
                f,
                level,
                0.9 + 0.22 * h + 0.0015 * f,
                rng.range(0.0, 2.0 * PI),
            )
        })
        .filter(|p| p.0 < 9_000.0)
        .collect();
    partials(x, &parts, at, until, 0.03);
    let i0 = (at * RATE) as usize;
    for j in 0..(0.006 * RATE) as usize {
        if let Some(o) = x.get_mut(i0 + j) {
            *o += (rng.white() * (-(j as f64) / (0.0015 * RATE)).exp() * 0.12 * gain) as f32;
        }
    }
}

/// A piano key struck at `at` with `velocity` (0–1), released at
/// `until`: two or three strings a little apart (beating), stiff
/// (inharmonic) partials, a fast then slow decay, the hammer's thump.
fn piano_key(x: &mut [f32], key: u8, at: f64, until: f64, velocity: f64, rng: &mut Rng) {
    let f0 = hz(f64::from(key));
    let strings = if key < 40 {
        1
    } else if key < 52 {
        2
    } else {
        3
    };
    let b = 1.5e-4 * 10f64.powf((f64::from(key) - 60.0) / 40.0);
    let tilt = 1.3 - 0.6 * velocity;
    let mut parts = Vec::new();
    for s in 0..strings {
        let detune = if s == 0 { 0.0 } else { rng.range(-1.2, 1.2) };
        let ratio = 2f64.powf(detune / 1200.0);
        for m in 1..=40 {
            let h = f64::from(m);
            let f = h * f0 * ratio * (1.0 + b * h * h).sqrt();
            if f > 9_000.0 {
                break;
            }
            let strike = (PI * h / 7.3).sin().abs().max(0.08);
            let level = strike / h.powf(tilt) * velocity / f64::from(strings);
            // Prompt sound, then the aftersound (a quieter second partial).
            let fast = 1.2 + 0.25 * h + f0 / 400.0;
            let phase = rng.range(0.0, 2.0 * PI);
            parts.push((f, level * 0.7, fast, phase));
            parts.push((f * 1.000_2, level * 0.3, 0.35 + 0.05 * h, phase + 0.3));
        }
    }
    partials(x, &parts, at, until, 0.08);
    // The hammer: a low thump.
    let mut low = Resonator::tuned(180.0, 300.0);
    let i0 = (at * RATE) as usize;
    for j in 0..(0.012 * RATE) as usize {
        if let Some(o) = x.get_mut(i0 + j) {
            let k = (-(j as f64) / (0.003 * RATE)).exp();
            *o += (low.run(rng.white()) * k * 0.4 * velocity) as f32;
        }
    }
}

/// A plucked monophonic lead line: (key, length, gap).
pub fn lead_line(name: &'static str, steps: &[(u8, f64, f64)], seed: u64) -> Piece {
    let mut rng = Rng::new(seed);
    let mut t = LEAD_IN;
    let mut truth = Vec::new();
    let total: f64 = steps.iter().map(|s| s.1 + s.2).sum::<f64>() + LEAD_IN + 0.8;
    let mut out = vec![0.0f32; (total * RATE) as usize];
    for &(key, dur, gap) in steps {
        string(&mut out, key, t, t + dur, rng.range(0.5, 1.0), &mut rng);
        truth.push(Note {
            key,
            on: t,
            off: t + dur,
        });
        t += dur + gap;
    }
    normalise(&mut out);
    Piece {
        name,
        audio: out,
        truth,
    }
}

/// Strummed chords: (keys low to high, length); each strummed down (the
/// strings `spread` s apart), the last damped as the next is struck.
pub fn strums(name: &'static str, chords: &[(&[u8], f64)], spread: f64, seed: u64) -> Piece {
    let mut rng = Rng::new(seed);
    let mut t = LEAD_IN;
    let mut truth = Vec::new();
    let total: f64 = chords.iter().map(|c| c.1).sum::<f64>() + LEAD_IN + 0.8;
    let mut out = vec![0.0f32; (total * RATE) as usize];
    for &(keys, dur) in chords {
        let loud = rng.range(0.5, 1.0);
        for (i, &key) in keys.iter().enumerate() {
            let on = t + spread * i as f64 * rng.range(0.7, 1.3);
            string(
                &mut out,
                key,
                on,
                t + dur - 0.02,
                loud * rng.range(0.6, 1.0),
                &mut rng,
            );
            if !truth
                .iter()
                .any(|n: &Note| n.key == key && (n.on - t).abs() < 0.2)
            {
                truth.push(Note {
                    key,
                    on,
                    off: t + dur - 0.02,
                });
            }
        }
        t += dur;
    }
    normalise(&mut out);
    Piece {
        name,
        audio: out,
        truth,
    }
}

/// Picked arpeggios: per bar, the keys in turn every `step` s, each ringing
/// to the end of the bar (or until it is picked again).
pub fn arpeggio(name: &'static str, bars: &[&[u8]], step: f64, seed: u64) -> Piece {
    let mut rng = Rng::new(seed);
    let mut t = LEAD_IN;
    let mut truth: Vec<Note> = Vec::new();
    let total: f64 = bars.iter().map(|b| b.len() as f64 * step).sum::<f64>() + LEAD_IN + 0.8;
    let mut out = vec![0.0f32; (total * RATE) as usize];
    for keys in bars {
        let end = t + keys.len() as f64 * step;
        for (i, &key) in keys.iter().enumerate() {
            let on = t + i as f64 * step;
            let until = keys[i + 1..]
                .iter()
                .position(|k| *k == key)
                .map_or(end, |n| on + (n + 1) as f64 * step);
            string(
                &mut out,
                key,
                on,
                until - 0.01,
                rng.range(0.5, 1.0),
                &mut rng,
            );
            truth.push(Note {
                key,
                on,
                off: until - 0.01,
            });
        }
        t = end;
    }
    normalise(&mut out);
    Piece {
        name,
        audio: out,
        truth,
    }
}

/// Piano: (keys, start, length, velocity) per struck chord or note.
pub fn piano(name: &'static str, events: &[(&[u8], f64, f64, f64)], seed: u64) -> Piece {
    let mut rng = Rng::new(seed);
    let mut truth = Vec::new();
    let total = events.iter().map(|e| e.1 + e.2).fold(0.0, f64::max) + LEAD_IN + 0.8;
    let mut out = vec![0.0f32; (total * RATE) as usize];
    for &(keys, at, dur, velocity) in events {
        for &key in keys {
            // A hand's keys a few milliseconds apart.
            let on = LEAD_IN + at + rng.range(0.0, 0.012);
            piano_key(
                &mut out,
                key,
                on,
                LEAD_IN + at + dur,
                velocity * rng.range(0.8, 1.0),
                &mut rng,
            );
            truth.push(Note {
                key,
                on,
                off: LEAD_IN + at + dur,
            });
        }
    }
    normalise(&mut out);
    Piece {
        name,
        audio: out,
        truth,
    }
}

fn normalise(x: &mut [f32]) {
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak > 0.0 {
        for v in x {
            *v /= peak;
        }
    }
}

/// What else the input hears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Room {
    /// A quiet room: noise 55 dB under the peak.
    Quiet,
    /// Fans and traffic: noise 32 dB under the peak.
    Noisy,
    /// A ground loop: 60 Hz mains hum and its harmonics 34 dB under the
    /// peak, over a quiet room.
    Hum,
}

impl Room {
    pub const ALL: [Room; 3] = [Room::Quiet, Room::Noisy, Room::Hum];

    pub fn label(self) -> &'static str {
        match self {
            Room::Quiet => "quiet room",
            Room::Noisy => "noisy room",
            Room::Hum => "mains hum",
        }
    }
}

/// `seconds` of the room alone at a peak level of `level_db` (what the
/// input hears before anyone sings).
pub fn room(seconds: f64, level_db: f64, room: Room, seed: u64) -> Vec<f32> {
    let mut x = vec![0.0f32; (seconds * RATE) as usize];
    add_room(&mut x, level_db, room, seed);
    x
}

/// `piece` at a peak of `level_db` dBFS in `room`.
pub fn at_level(piece: &Piece, level_db: f64, room: Room, seed: u64) -> Vec<f32> {
    let gain = 10f64.powf(level_db / 20.0) as f32;
    let mut x: Vec<f32> = piece.audio.iter().map(|v| v * gain).collect();
    add_room(&mut x, level_db, room, seed);
    x
}

fn add_room(x: &mut [f32], level_db: f64, room: Room, seed: u64) {
    let mut rng = Rng::new(seed ^ 0x5EED);
    let noise_db = level_db
        - match room {
            Room::Quiet | Room::Hum => 55.0,
            Room::Noisy => 32.0,
        };
    // Pink noise (Paul Kellet's filter: RMS about 0.11 per unit white
    // noise of RMS 0.58 → scaled to the level).
    let pink_rms = 10f64.powf(noise_db / 20.0);
    let hum_rms = 10f64.powf((level_db - 34.0) / 20.0);
    // The converter's own floor.
    let floor = 10f64.powf(-100.0 / 20.0) * 3f64.sqrt();
    let mut b = [0.0f64; 7];
    let mut hum_phase = rng.range(0.0, 1.0);
    // Measured RMS of the pinking filter on uniform noise.
    const PINK_UNIT: f64 = 0.33;
    for v in x.iter_mut() {
        let w = rng.white();
        b[0] = 0.99886 * b[0] + w * 0.0555179;
        b[1] = 0.99332 * b[1] + w * 0.0750759;
        b[2] = 0.96900 * b[2] + w * 0.1538520;
        b[3] = 0.86650 * b[3] + w * 0.3104856;
        b[4] = 0.55000 * b[4] + w * 0.5329522;
        b[5] = -0.7616 * b[5] - w * 0.0168980;
        let pink = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
        b[6] = w * 0.115926;
        let mut s = pink / PINK_UNIT * pink_rms + rng.white() * floor;
        if room == Room::Hum {
            hum_phase = (hum_phase + 60.0 / RATE).fract();
            let p = 2.0 * PI * hum_phase;
            // A buzz: odd and even harmonics (RMS of the sum about 0.9).
            let h = p.sin()
                + 0.7 * (2.0 * p).sin()
                + 0.5 * (3.0 * p).sin()
                + 0.3 * (4.0 * p).sin()
                + 0.25 * (5.0 * p).sin()
                + 0.12 * (7.0 * p).sin();
            s += h / 0.98 * hum_rms;
        }
        *v += s as f32;
    }
}
