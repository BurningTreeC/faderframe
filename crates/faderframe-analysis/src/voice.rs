//! Singing into MIDI live: a causal pitch tracker that turns a monophonic
//! input (a voice, a whistle, a monophonic instrument) into notes as it
//! comes in.
//!
//! The input is low-passed and decimated to about 11 kHz; every 5 ms the
//! last 40 ms are run through McLeod's method ([`crate::pitch`]) and the
//! level measured. A frame is voiced when its pitch is clear and it is loud
//! enough; a note starts when the same key (the nearest one the scale
//! allows) holds for [`VoiceConfig::onset_frames`], changes when another
//! one holds for [`VoiceConfig::change_frames`] (with half a semitone of
//! hysteresis, so vibrato and scoops stay on the note), and ends after
//! [`VoiceConfig::release_frames`] unvoiced. While a note sounds, its pitch
//! against the key comes out as [`VoiceEvent::Bend`] (for glides). Each
//! event says when the sound it reports began (`at`, frames from the start
//! of the input it came with, earlier than the analysis by the window and
//! the holds), so a recording can put notes where they were sung.

use crate::pitch::{Detector, note_at};

/// The analysis rate the input is decimated to (about).
const ANALYSIS_RATE: f64 = 11_025.0;
const HOP_SECONDS: f64 = 0.005;
const WINDOW_SECONDS: f64 = 0.040;
/// How clear a voiced frame's pitch is.
const CLARITY: f64 = 0.78;
/// The voice's range (Hz).
const LOWEST_HZ: f64 = 60.0;
const HIGHEST_HZ: f64 = 1_600.0;
/// Half a semitone, and this much more, before another key is heard.
const HYSTERESIS: f64 = 0.3;

/// How the tracker hears.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceConfig {
    /// Quieter than this (dBFS) is silence.
    pub threshold_db: f32,
    /// The lowest and highest keys played.
    pub low: u8,
    pub high: u8,
    /// Analysis frames (5 ms) a key holds before its note starts.
    pub onset_frames: u32,
    /// Frames another key holds before the note changes.
    pub change_frames: u32,
    /// Unvoiced frames that end a note.
    pub release_frames: u32,
    /// Pitch classes notes are snapped to (bit 0 = C … bit 11 = B).
    pub scale: u16,
    /// A4 (Hz).
    pub reference: f64,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            threshold_db: -45.0,
            low: 28,
            high: 96,
            onset_frames: 3,
            change_frames: 5,
            release_frames: 8,
            scale: 0xFFF,
            reference: 440.0,
        }
    }
}

/// What the tracker heard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceEvent {
    NoteOn {
        key: u8,
        velocity: u8,
    },
    NoteOff {
        key: u8,
    },
    /// The sounding note's pitch against its key (semitones).
    Bend {
        semitones: f32,
    },
}

/// A second-order low-pass section (Butterworth, direct form II
/// transposed).
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn lowpass(cutoff: f64, rate: f64, q: f64) -> Self {
        let w = 2.0 * std::f64::consts::PI * cutoff / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos) / 2.0 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// The live tracker (see the module docs).
pub struct VoiceTracker {
    config: VoiceConfig,
    /// Input frames per analysis frame.
    decimate: usize,
    phase: usize,
    filters: [Biquad; 2],
    analysis_rate: f64,
    /// The last window of decimated input (a ring) and its contiguous copy.
    ring: Vec<f32>,
    write: usize,
    filled: usize,
    window: Vec<f32>,
    hop: usize,
    since_hop: usize,
    detector: Detector,
    sounding: Option<u8>,
    /// A key being heard (not the sounding one) and for how many frames,
    /// and the loudest level meanwhile.
    candidate: Option<(u8, u32, f32)>,
    unvoiced: u32,
    last_bend: f32,
}

impl VoiceTracker {
    pub fn new(rate: f64, config: VoiceConfig) -> Self {
        let decimate = ((rate / ANALYSIS_RATE).floor() as usize).max(1);
        let analysis_rate = rate / decimate as f64;
        let window_len = (WINDOW_SECONDS * analysis_rate).round() as usize;
        // Two sections: a fourth-order Butterworth well below the new
        // Nyquist.
        let cutoff = (analysis_rate * 0.42).min(rate * 0.45);
        Self {
            config,
            decimate,
            phase: 0,
            filters: [
                Biquad::lowpass(cutoff, rate, 0.541_196_1),
                Biquad::lowpass(cutoff, rate, 1.306_563),
            ],
            analysis_rate,
            ring: vec![0.0; window_len],
            write: 0,
            filled: 0,
            window: vec![0.0; window_len],
            hop: ((HOP_SECONDS * analysis_rate).round() as usize).max(1),
            since_hop: 0,
            detector: Detector::new(),
            sounding: None,
            candidate: None,
            unvoiced: 0,
            last_bend: 0.0,
        }
    }

    pub fn config(&self) -> VoiceConfig {
        self.config
    }

    pub fn set_config(&mut self, config: VoiceConfig) {
        self.config = config;
    }

    /// The note sounding now.
    pub fn sounding(&self) -> Option<u8> {
        self.sounding
    }

    /// How long before an analysis frame the sound it hears began: half
    /// the window (input frames).
    fn heard_delay(&self) -> usize {
        self.window.len() / 2 * self.decimate
    }

    /// Input frames per analysis frame step.
    fn hop_frames(&self) -> usize {
        self.hop * self.decimate
    }

    /// End the sounding note (the input stopped).
    pub fn reset(&mut self) -> Option<VoiceEvent> {
        self.candidate = None;
        self.unvoiced = 0;
        self.sounding.take().map(|key| VoiceEvent::NoteOff { key })
    }

    /// Feed `x`; `emit(at, event)` for what it heard, `at` in frames from
    /// the start of `x` where the sound began (negative: before it).
    pub fn process(&mut self, x: &[f32], mut emit: impl FnMut(isize, VoiceEvent)) {
        for (i, &v) in x.iter().enumerate() {
            let mut s = f64::from(v);
            for f in &mut self.filters {
                s = f.run(s);
            }
            self.phase += 1;
            if self.phase < self.decimate {
                continue;
            }
            self.phase = 0;
            self.ring[self.write] = s as f32;
            self.write = (self.write + 1) % self.ring.len();
            self.filled = (self.filled + 1).min(self.ring.len());
            self.since_hop += 1;
            if self.since_hop >= self.hop && self.filled == self.ring.len() {
                self.since_hop = 0;
                self.analyse(i as isize, &mut emit);
            }
        }
    }

    /// One analysis frame ending at input frame `now` (of the current
    /// slice).
    fn analyse(&mut self, now: isize, emit: &mut impl FnMut(isize, VoiceEvent)) {
        let n = self.ring.len();
        for k in 0..n {
            self.window[k] = self.ring[(self.write + k) % n];
        }
        // The level over the newest 10 ms.
        let recent = (self.hop * 2).min(n);
        let power = self.window[n - recent..]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            / recent as f64;
        let level_db = (10.0 * power.max(1e-12).log10()) as f32;
        let c = self.config;
        let heard = (level_db >= c.threshold_db)
            .then(|| self.detector.detect(&self.window, self.analysis_rate))
            .flatten()
            .filter(|p| p.clarity >= CLARITY && (LOWEST_HZ..=HIGHEST_HZ).contains(&p.freq))
            .map(|p| note_at(p.freq, c.reference))
            .filter(|m| (f64::from(c.low) - 0.5..=f64::from(c.high) + 0.5).contains(m));
        let delay = self.heard_delay() as isize;
        let Some(pitch) = heard else {
            self.candidate = None;
            self.unvoiced += 1;
            if self.unvoiced >= c.release_frames
                && let Some(key) = self.sounding.take()
            {
                let back = delay + (self.unvoiced as isize - 1) * self.hop_frames() as isize;
                emit(now - back, VoiceEvent::NoteOff { key });
            }
            return;
        };
        self.unvoiced = 0;
        // The key heard: the sounding one while within its hysteresis,
        // else the nearest the scale allows.
        let key = match self.sounding {
            Some(s) if (pitch - f64::from(s)).abs() < 0.5 + HYSTERESIS => s,
            _ => nearest_in_scale(pitch, c.scale, c.low, c.high),
        };
        if self.sounding == Some(key) {
            self.candidate = None;
            let semitones = (pitch - f64::from(key)) as f32;
            if (semitones - self.last_bend).abs() >= 0.02 {
                self.last_bend = semitones;
                emit(now - delay, VoiceEvent::Bend { semitones });
            }
            return;
        }
        let (count, loudest) = match self.candidate {
            Some((k, n, l)) if k == key => (n + 1, l.max(level_db)),
            _ => (1, level_db),
        };
        self.candidate = Some((key, count, loudest));
        let needed = if self.sounding.is_some() {
            c.change_frames
        } else {
            c.onset_frames
        };
        if count < needed {
            return;
        }
        let back = delay + (count as isize - 1) * self.hop_frames() as isize;
        if let Some(old) = self.sounding.take() {
            emit(now - back, VoiceEvent::NoteOff { key: old });
        }
        let velocity = velocity_of(loudest, c.threshold_db);
        emit(now - back, VoiceEvent::NoteOn { key, velocity });
        self.sounding = Some(key);
        self.candidate = None;
        self.last_bend = 0.0;
        let semitones = (pitch - f64::from(key)) as f32;
        if semitones.abs() >= 0.02 {
            self.last_bend = semitones;
            emit(now - back, VoiceEvent::Bend { semitones });
        }
    }
}

/// The nearest key to `pitch` whose pitch class the scale has, in range.
fn nearest_in_scale(pitch: f64, scale: u16, low: u8, high: u8) -> u8 {
    let scale = if scale & 0xFFF == 0 { 0xFFF } else { scale };
    let base = pitch.round() as i32;
    let mut best = base;
    let mut dist = f64::MAX;
    for k in base - 6..=base + 6 {
        if scale & (1 << k.rem_euclid(12)) == 0 {
            continue;
        }
        let d = (f64::from(k) - pitch).abs();
        if d < dist {
            dist = d;
            best = k;
        }
    }
    best.clamp(i32::from(low), i32::from(high)) as u8
}

/// Velocity from the loudest level of a note's start: the threshold soft,
/// 40 dB above it full.
fn velocity_of(level_db: f32, threshold_db: f32) -> u8 {
    let t = ((level_db - threshold_db) / 40.0).clamp(0.0, 1.0);
    (30.0 + t * 97.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    /// A sung phrase: (start s, end s, key, vibrato cents); a glottal-ish
    /// saw with a falling spectrum, attack and release ramps.
    fn phrase(notes: &[(f64, f64, f64, f64)], total: f64, gain: f32) -> Vec<f32> {
        let n = (total * RATE) as usize;
        let mut out = vec![0.0f32; n];
        for &(a, b, key, vib) in notes {
            let mut phase = 0.0f64;
            let (i0, i1) = ((a * RATE) as usize, (b * RATE) as usize);
            for (j, o) in out.iter_mut().enumerate().take(i1.min(n)).skip(i0) {
                let t = (j - i0) as f64 / RATE;
                let cents = vib * (2.0 * std::f64::consts::PI * 5.5 * t).sin();
                let f = 440.0 * 2f64.powf((key - 69.0 + cents / 100.0) / 12.0);
                phase = (phase + f / RATE).fract();
                let mut v = 0.0;
                for h in 1..12 {
                    v += (2.0 * std::f64::consts::PI * phase * h as f64).sin() / (h * h) as f64;
                }
                let env = (t / 0.02).min(1.0) * ((b - a - t) / 0.02).clamp(0.0, 1.0);
                *o += (v * env) as f32 * gain;
            }
        }
        out
    }

    /// Run in blocks; events with their absolute frame.
    fn track(x: &[f32], config: VoiceConfig) -> Vec<(i64, VoiceEvent)> {
        let mut t = VoiceTracker::new(RATE, config);
        let mut out = Vec::new();
        for (b, chunk) in x.chunks(256).enumerate() {
            let base = (b * 256) as i64;
            t.process(chunk, |at, e| out.push((base + at as i64, e)));
        }
        if let Some(e) = t.reset() {
            out.push((x.len() as i64, e));
        }
        out
    }

    fn notes(events: &[(i64, VoiceEvent)]) -> Vec<(u8, f64, f64)> {
        let mut out = Vec::new();
        let mut open: Option<(u8, i64)> = None;
        for &(at, e) in events {
            match e {
                VoiceEvent::NoteOn { key, .. } => open = Some((key, at)),
                VoiceEvent::NoteOff { key } => {
                    if let Some((k, s)) = open.take() {
                        assert_eq!(k, key);
                        out.push((k, s as f64 / RATE, at as f64 / RATE));
                    }
                }
                VoiceEvent::Bend { .. } => {}
            }
        }
        out
    }

    #[test]
    fn a_sung_phrase_becomes_its_notes_where_they_were_sung() {
        let x = phrase(
            &[
                (0.10, 0.50, 57.0, 25.0),
                (0.50, 0.90, 60.0, 25.0),
                (1.20, 1.60, 64.0, 30.0),
            ],
            2.0,
            0.3,
        );
        let ev = track(&x, VoiceConfig::default());
        let got = notes(&ev);
        assert_eq!(
            got.iter().map(|n| n.0).collect::<Vec<_>>(),
            [57, 60, 64],
            "{got:?}"
        );
        // Placed where sung (within 25 ms), legato into the second.
        for ((_, s, e), (a, b)) in got.iter().zip([(0.10, 0.50), (0.50, 0.90), (1.20, 1.60)]) {
            assert!((s - a).abs() < 0.025, "start {s} for {a}");
            assert!((e - b).abs() < 0.04, "end {e} for {b}");
        }
        // Vibrato stays on the note, as bends of about a quarter tone.
        let bends: Vec<f32> = ev
            .iter()
            .filter_map(|(_, e)| match e {
                VoiceEvent::Bend { semitones } => Some(*semitones),
                _ => None,
            })
            .collect();
        assert!(bends.iter().all(|b| b.abs() < 0.5), "{bends:?}");
        assert!(bends.iter().any(|b| b.abs() > 0.1), "vibrato heard");
    }

    #[test]
    fn quiet_input_plays_nothing_and_the_scale_snaps() {
        let x = phrase(&[(0.1, 0.5, 61.0, 0.0)], 0.8, 0.001);
        assert!(track(&x, VoiceConfig::default()).is_empty());
        // A sharp C♯ sung, C major allowed: D, the nearer.
        let x = phrase(&[(0.1, 0.5, 61.2, 0.0)], 0.8, 0.3);
        let c_major = 0b1010_1011_0101;
        let got = notes(&track(
            &x,
            VoiceConfig {
                scale: c_major,
                ..VoiceConfig::default()
            },
        ));
        assert_eq!(got.iter().map(|n| n.0).collect::<Vec<_>>(), [62]);
    }

    #[test]
    fn a_strong_second_harmonic_keeps_the_fundamental() {
        // Octave-ambiguous: the second harmonic louder than the first.
        let n = (0.6 * RATE) as usize;
        let x: Vec<f32> = (0..n)
            .map(|j| {
                let t = j as f64 / RATE;
                let f = 196.0;
                let w = 2.0 * std::f64::consts::PI * f * t;
                (0.15 * w.sin() + 0.3 * (2.0 * w).sin() + 0.1 * (3.0 * w).sin()) as f32
            })
            .collect();
        let got = notes(&track(&x, VoiceConfig::default()));
        assert_eq!(got.iter().map(|n| n.0).collect::<Vec<_>>(), [55], "{got:?}");
    }
}
