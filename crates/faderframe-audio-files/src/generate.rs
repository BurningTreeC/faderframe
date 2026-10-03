//! Deterministic procedural audio used for demo projects and tests.
//!
//! A project can reference a [`GeneratorSpec`] instead of a file; the
//! session renders it at the engine's sample rate. Same spec + same sample
//! rate ⇒ bit-identical audio (a fixed-seed PRNG is used for noise).

use crate::AudioData;
use serde::{Deserialize, Serialize};
use std::f64::consts::TAU;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeneratorSpec {
    Silence {
        seconds: f64,
        channels: u16,
    },
    Sine {
        frequency: f64,
        seconds: f64,
        amplitude: f32,
    },
    /// Stereo kick/snare/hat groove, 16th-note grid.
    DrumLoop {
        bpm: f64,
        bars: u32,
        seed: u64,
    },
    /// Mono filtered-saw bass playing eighth notes on one root per bar.
    Bassline {
        bpm: f64,
        bars: u32,
        roots: Vec<u8>,
    },
    /// Mono plucked-string (Karplus–Strong) sixteenth-note arpeggio.
    PluckArpeggio {
        bpm: f64,
        bars: u32,
        chords: Vec<Vec<u8>>,
        seed: u64,
    },
    /// Stereo detuned-saw pad sustaining one chord per bar.
    Pad {
        bpm: f64,
        bars: u32,
        chords: Vec<Vec<u8>>,
    },
}

impl GeneratorSpec {
    pub fn channels(&self) -> usize {
        match self {
            GeneratorSpec::Silence { channels, .. } => (*channels).max(1) as usize,
            GeneratorSpec::Sine { .. }
            | GeneratorSpec::Bassline { .. }
            | GeneratorSpec::PluckArpeggio { .. } => 1,
            GeneratorSpec::DrumLoop { .. } | GeneratorSpec::Pad { .. } => 2,
        }
    }

    pub fn seconds(&self) -> f64 {
        let bars_len = |bpm: f64, bars: u32| bars as f64 * 4.0 * 60.0 / bpm.max(1.0);
        match self {
            GeneratorSpec::Silence { seconds, .. } | GeneratorSpec::Sine { seconds, .. } => {
                *seconds
            }
            GeneratorSpec::DrumLoop { bpm, bars, .. }
            | GeneratorSpec::Bassline { bpm, bars, .. }
            | GeneratorSpec::PluckArpeggio { bpm, bars, .. }
            | GeneratorSpec::Pad { bpm, bars, .. } => bars_len(*bpm, *bars),
        }
    }

    pub fn frames(&self, sample_rate: u32) -> usize {
        (self.seconds().max(0.0) * sample_rate as f64).round() as usize
    }

    pub fn label(&self) -> &'static str {
        match self {
            GeneratorSpec::Silence { .. } => "Silence",
            GeneratorSpec::Sine { .. } => "Sine",
            GeneratorSpec::DrumLoop { .. } => "Drum loop",
            GeneratorSpec::Bassline { .. } => "Bassline",
            GeneratorSpec::PluckArpeggio { .. } => "Pluck arpeggio",
            GeneratorSpec::Pad { .. } => "Pad",
        }
    }
}

/// Render a generator at `sample_rate`.
pub fn generate(spec: &GeneratorSpec, sample_rate: u32) -> AudioData {
    let sr = sample_rate.max(1) as f64;
    let frames = spec.frames(sample_rate);
    let mut out = vec![vec![0.0f32; frames]; spec.channels()];
    match spec {
        GeneratorSpec::Silence { .. } => {}
        GeneratorSpec::Sine {
            frequency,
            amplitude,
            ..
        } => {
            for (i, s) in out[0].iter_mut().enumerate() {
                *s = (TAU * frequency * i as f64 / sr).sin() as f32 * amplitude;
            }
        }
        GeneratorSpec::DrumLoop { bpm, bars, seed } => drums(&mut out, sr, *bpm, *bars, *seed),
        GeneratorSpec::Bassline { bpm, bars, roots } => bass(&mut out[0], sr, *bpm, *bars, roots),
        GeneratorSpec::PluckArpeggio {
            bpm,
            bars,
            chords,
            seed,
        } => pluck(&mut out[0], sr, *bpm, *bars, chords, *seed),
        GeneratorSpec::Pad { bpm, bars, chords } => pad(&mut out, sr, *bpm, *bars, chords),
    }
    normalise(&mut out, 0.7);
    AudioData::from_channels(sample_rate, out)
}

fn normalise(channels: &mut [Vec<f32>], target: f32) {
    let peak = channels
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 1e-9 {
        let g = target / peak;
        for c in channels {
            for s in c {
                *s *= g;
            }
        }
    }
}

/// xorshift64* — tiny deterministic PRNG for noise.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

fn midi_hz(note: u8) -> f64 {
    440.0 * 2f64.powf((note as f64 - 69.0) / 12.0)
}

fn add(buf: &mut [f32], start: usize, samples: impl Iterator<Item = f32>) {
    for (s, v) in buf.iter_mut().skip(start).zip(samples) {
        *s += v;
    }
}

fn drums(out: &mut [Vec<f32>], sr: f64, bpm: f64, bars: u32, seed: u64) {
    let step = 60.0 / bpm.max(1.0) / 4.0; // sixteenth
    let mut rng = Rng::new(seed);
    for bar in 0..bars {
        for st in 0..16u32 {
            let t0 = ((bar * 16 + st) as f64 * step * sr) as usize;
            let fill = bar % 4 == 3;
            let kick = matches!(st, 0 | 8) || (st == 10 && bar % 2 == 1) || (fill && st == 14);
            let snare = matches!(st, 4 | 12) || (fill && st == 15);
            let open_hat = st == 14 && !fill;
            let hat = st % 2 == 0 && !open_hat;
            if kick {
                let len = (0.45 * sr) as usize;
                let mut phase = 0.0f64;
                let samples = (0..len).map(|i| {
                    let t = i as f64 / sr;
                    let f = 48.0 + 130.0 * (-t * 32.0).exp();
                    phase += TAU * f / sr;
                    (phase.sin() * (-t * 6.5).exp()) as f32 * 0.95
                });
                let v: Vec<f32> = samples.collect();
                add(&mut out[0], t0, v.iter().copied());
                add(&mut out[1], t0, v.iter().copied());
            }
            if snare {
                let len = (0.25 * sr) as usize;
                let mut hp = 0.0f32;
                let mut prev = 0.0f32;
                let v: Vec<f32> = (0..len)
                    .map(|i| {
                        let t = i as f64 / sr;
                        let n = rng.next_f32();
                        hp = 0.92 * (hp + n - prev);
                        prev = n;
                        let tone = (TAU * 190.0 * t).sin() * (-t * 28.0).exp();
                        hp * (-t * 16.0).exp() as f32 * 0.55 + tone as f32 * 0.35
                    })
                    .collect();
                add(&mut out[0], t0, v.iter().map(|s| s * 0.95));
                add(&mut out[1], t0, v.iter().copied());
            }
            if hat || open_hat {
                let decay = if open_hat { 9.0 } else { 55.0 };
                let accent = if st % 4 == 2 { 1.0 } else { 0.6 };
                let len = ((if open_hat { 0.35 } else { 0.08 }) * sr) as usize;
                let mut prev = 0.0f32;
                let v: Vec<f32> = (0..len)
                    .map(|i| {
                        let t = i as f64 / sr;
                        let n = rng.next_f32();
                        let hp = n - prev;
                        prev = n;
                        hp * (-t * decay).exp() as f32 * 0.18 * accent
                    })
                    .collect();
                // Hats sit slightly right of centre.
                add(&mut out[0], t0, v.iter().map(|s| s * 0.7));
                add(&mut out[1], t0, v.iter().copied());
            }
        }
    }
}

fn bass(out: &mut [f32], sr: f64, bpm: f64, bars: u32, roots: &[u8]) {
    if roots.is_empty() {
        return;
    }
    let eighth = 60.0 / bpm.max(1.0) / 2.0;
    let mut lp = 0.0f64;
    for bar in 0..bars {
        let root = roots[bar as usize % roots.len()];
        for e in 0..8u32 {
            let note = if e == 6 { root + 12 } else { root };
            let f = midi_hz(note);
            let t0 = ((bar * 8 + e) as f64 * eighth * sr) as usize;
            let len = (eighth * 0.85 * sr) as usize;
            let mut phase = 0.0f64;
            for i in 0..len {
                let Some(s) = out.get_mut(t0 + i) else { break };
                let t = i as f64 / sr;
                phase = (phase + f / sr).fract();
                let saw = 2.0 * phase - 1.0;
                let cutoff = 250.0 + 1400.0 * (-t * 9.0).exp();
                let a = 1.0 - (-TAU * cutoff / sr).exp();
                lp += a * (saw - lp);
                let env = (t * 400.0).min(1.0) * (1.0 - (i as f64 / len as f64)).powf(0.3);
                *s += (lp * env * 0.8) as f32;
            }
        }
    }
}

fn pluck(out: &mut [f32], sr: f64, bpm: f64, bars: u32, chords: &[Vec<u8>], seed: u64) {
    if chords.is_empty() {
        return;
    }
    let sixteenth = 60.0 / bpm.max(1.0) / 4.0;
    let mut rng = Rng::new(seed);
    for bar in 0..bars {
        let chord = &chords[bar as usize % chords.len()];
        if chord.is_empty() {
            continue;
        }
        for st in 0..16usize {
            // Up-down arpeggio.
            let n = chord.len();
            let idx = if (st / n).is_multiple_of(2) {
                st % n
            } else {
                n - 1 - st % n
            };
            let note = chord[idx] + 12;
            let period = (sr / midi_hz(note)).max(2.0) as usize;
            let mut line: Vec<f32> = (0..period).map(|_| rng.next_f32() * 0.5).collect();
            let t0 = ((bar as usize * 16 + st) as f64 * sixteenth * sr) as usize;
            let len = (0.9 * sr) as usize;
            let vel = if st % 4 == 0 { 1.0 } else { 0.7 };
            let mut pos = 0usize;
            for i in 0..len {
                let Some(s) = out.get_mut(t0 + i) else { break };
                let next = (pos + 1) % period;
                let v = line[pos];
                line[pos] = 0.4985 * (v + line[next]);
                pos = next;
                *s += v * vel;
            }
        }
    }
}

fn pad(out: &mut [Vec<f32>], sr: f64, bpm: f64, bars: u32, chords: &[Vec<u8>]) {
    if chords.is_empty() {
        return;
    }
    let bar_len = 4.0 * 60.0 / bpm.max(1.0);
    let attack = 0.35;
    let release = 0.5;
    for bar in 0..bars {
        let chord = &chords[bar as usize % chords.len()];
        let t0 = (bar as f64 * bar_len * sr) as usize;
        let len = ((bar_len + release) * sr) as usize;
        for (ch, buf) in out.iter_mut().enumerate() {
            let detune = if ch == 0 { -0.006 } else { 0.006 };
            for &note in chord {
                let f = midi_hz(note);
                let mut p1 = (note as f64 * 0.13 + ch as f64 * 0.37).fract();
                let mut p2 = (note as f64 * 0.71).fract();
                let mut lp = 0.0f64;
                for i in 0..len {
                    let Some(s) = buf.get_mut(t0 + i) else { break };
                    let t = i as f64 / sr;
                    p1 = (p1 + f * (1.0 + detune) / sr).fract();
                    p2 = (p2 + f * (1.0 - detune * 0.5) / sr).fract();
                    let saw = (2.0 * p1 - 1.0) + (2.0 * p2 - 1.0);
                    let a = 1.0 - (-TAU * 1200.0 / sr).exp();
                    lp += a * (saw - lp);
                    let env = if t < attack {
                        t / attack
                    } else if t > bar_len {
                        (1.0 - (t - bar_len) / release).max(0.0)
                    } else {
                        1.0
                    };
                    *s += (lp * env * 0.15) as f32;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs() -> Vec<GeneratorSpec> {
        vec![
            GeneratorSpec::Sine {
                frequency: 440.0,
                seconds: 0.5,
                amplitude: 0.5,
            },
            GeneratorSpec::DrumLoop {
                bpm: 120.0,
                bars: 1,
                seed: 1,
            },
            GeneratorSpec::Bassline {
                bpm: 120.0,
                bars: 1,
                roots: vec![45],
            },
            GeneratorSpec::PluckArpeggio {
                bpm: 120.0,
                bars: 1,
                chords: vec![vec![57, 60, 64]],
                seed: 2,
            },
            GeneratorSpec::Pad {
                bpm: 120.0,
                bars: 1,
                chords: vec![vec![57, 60, 64]],
            },
        ]
    }

    #[test]
    fn generators_produce_expected_shape_at_all_standard_rates() {
        for sr in [44_100, 48_000, 96_000, 192_000] {
            for spec in specs() {
                let d = generate(&spec, sr);
                assert_eq!(d.num_channels(), spec.channels(), "{spec:?}");
                assert_eq!(d.frames(), spec.frames(sr), "{spec:?}");
                assert_eq!(d.sample_rate(), sr);
                let peak = d.peak();
                assert!(peak > 0.1 && peak <= 0.7001, "{spec:?} peak {peak}");
                assert!(d.channel(0).iter().all(|s| s.is_finite()));
            }
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let spec = GeneratorSpec::DrumLoop {
            bpm: 100.0,
            bars: 1,
            seed: 7,
        };
        assert_eq!(generate(&spec, 48_000), generate(&spec, 48_000));
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"kind\":\"drum_loop\""));
        assert_eq!(serde_json::from_str::<GeneratorSpec>(&json).unwrap(), spec);
    }
}
