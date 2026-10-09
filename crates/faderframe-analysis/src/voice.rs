//! Singing into MIDI live: a causal pitch tracker that turns a monophonic
//! input (a voice, a whistle, a monophonic instrument) into notes as it
//! comes in, fast enough to play an instrument with.
//!
//! The input is low-passed and decimated to about 11 kHz; every 2 ms the
//! newest audio is run through McLeod's method ([`crate::pitch`]) over the
//! shortest window that holds enough periods to be sure — 12 ms first
//! (most voices), 22 or 40 ms only for low ones, and while a note sounds,
//! two and a half of its periods — and the level measured. A frame is
//! voiced when its pitch is clear and it is loud enough; a note starts when
//! the same key (the nearest the scale allows) holds for
//! [`VoiceConfig::onset_frames`] (one frame when it is very clear and the
//! tracker is set to [`Responsiveness::Fast`]), changes when another holds
//! for [`VoiceConfig::change_frames`] (with half a semitone of hysteresis,
//! so vibrato and scoops stay on the note), and ends after
//! [`VoiceConfig::release_frames`] unvoiced. While a note sounds, its pitch
//! against the key comes out as [`VoiceEvent::Bend`] (for glides).
//!
//! Each event comes with two places: where in the input it was decided
//! (`now`: play it there) and where the sound it reports began (`at`,
//! earlier by half the window and the holds: record it there).
//! Allocation-free after [`VoiceTracker::new`], so it runs on the audio
//! thread.

use crate::pitch::{Detector, note_at};

/// The analysis rate the input is decimated to (about).
const ANALYSIS_RATE: f64 = 11_025.0;
/// One analysis frame.
pub const HOP_SECONDS: f64 = 0.001;
/// The windows tried, shortest first (seconds).
const WINDOWS: [f64; 3] = [0.012, 0.022, 0.040];
/// Periods a window must hold for its pitch to be taken.
const PERIODS: f64 = 2.3;
/// How clear a voiced frame's pitch is (fast: a note's first periods,
/// still swelling, are heard sooner at the cost of more doubt).
const CLARITY: f64 = 0.78;
const FAST_CLARITY: f64 = 0.6;
/// Bends at most this often (frames), unless the pitch jumps.
const BEND_FRAMES: u32 = 4;
/// The voice's range (Hz).
const LOWEST_HZ: f64 = 60.0;
const HIGHEST_HZ: f64 = 1_600.0;
/// Half a semitone, and this much more, before another key is heard.
const HYSTERESIS: f64 = 0.3;

/// How quickly notes follow the voice (and how much they risk a wrong
/// note for it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Responsiveness {
    /// A clear frame starts a note (a few ms; for playing).
    Fast,
    /// A few frames hold first.
    #[default]
    Balanced,
    /// Notes hold longer before they start or change (for recording
    /// clean takes).
    Stable,
}

impl Responsiveness {
    pub const ALL: [Responsiveness; 3] = [
        Responsiveness::Fast,
        Responsiveness::Balanced,
        Responsiveness::Stable,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Responsiveness::Fast => "Fast",
            Responsiveness::Balanced => "Balanced",
            Responsiveness::Stable => "Stable",
        }
    }

    /// (onset, change, release) in analysis frames (1 ms), and the clarity
    /// a voiced frame needs.
    pub fn frames(self) -> (u32, u32, u32, f64) {
        match self {
            Responsiveness::Fast => (2, 6, 20, FAST_CLARITY),
            Responsiveness::Balanced => (4, 12, 30, CLARITY),
            Responsiveness::Stable => (14, 22, 40, CLARITY),
        }
    }
}

/// How the tracker hears.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceConfig {
    /// The responsiveness the holds came from (the polyphonic tracker's
    /// window follows it).
    pub responsiveness: Responsiveness,
    /// Quieter than this (dBFS) is silence.
    pub threshold_db: f32,
    /// The lowest and highest keys played.
    pub low: u8,
    pub high: u8,
    /// Analysis frames (1 ms) a key holds before its note starts.
    pub onset_frames: u32,
    /// Frames another key holds before the note changes.
    pub change_frames: u32,
    /// Unvoiced frames that end a note.
    pub release_frames: u32,
    /// How clear (0–1) a frame's pitch must be to count as voiced.
    pub clarity: f64,
    /// Pitch classes notes are snapped to (bit 0 = C … bit 11 = B).
    pub scale: u16,
    /// A4 (Hz).
    pub reference: f64,
}

impl VoiceConfig {
    /// The holds of a responsiveness.
    pub fn with(mut self, r: Responsiveness) -> Self {
        self.responsiveness = r;
        (
            self.onset_frames,
            self.change_frames,
            self.release_frames,
            self.clarity,
        ) = r.frames();
        self
    }
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            responsiveness: Responsiveness::Balanced,
            threshold_db: -45.0,
            low: 28,
            high: 96,
            onset_frames: 0,
            change_frames: 0,
            release_frames: 0,
            clarity: CLARITY,
            scale: 0xFFF,
            reference: 440.0,
        }
        .with(Responsiveness::Balanced)
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
    /// A sounding note's pitch against its key (semitones).
    Bend {
        key: u8,
        semitones: f32,
    },
}

/// A second-order low-pass section (Butterworth, direct form II
/// transposed).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub(crate) fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    pub(crate) fn lowpass(cutoff: f64, rate: f64, q: f64) -> Self {
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
    pub(crate) fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// A pitch heard in one frame: the fractional key and the window it came
/// from (analysis samples).
#[derive(Clone, Copy, Debug)]
struct Heard {
    pitch: f64,
    window: usize,
}

/// The live tracker (see the module docs).
pub struct VoiceTracker {
    config: VoiceConfig,
    /// Input frames per analysis frame.
    decimate: usize,
    phase: usize,
    filters: [Biquad; 2],
    analysis_rate: f64,
    /// The newest audio (decimated, a ring as long as the longest window)
    /// and room for a window's contiguous copy.
    ring: Vec<f32>,
    write: usize,
    filled: usize,
    window: Vec<f32>,
    windows: [usize; 3],
    hop: usize,
    since_hop: usize,
    detector: Detector,
    sounding: Option<u8>,
    /// A key being heard (not the sounding one): the key, frames, the
    /// loudest level meanwhile and the window of its first frame.
    candidate: Option<(u8, u32, f32, usize)>,
    unvoiced: u32,
    last_bend: f32,
    /// Frames since the last bend.
    since_bend: u32,
}

impl VoiceTracker {
    pub fn new(rate: f64, config: VoiceConfig) -> Self {
        let decimate = ((rate / ANALYSIS_RATE).floor() as usize).max(1);
        let analysis_rate = rate / decimate as f64;
        let windows = WINDOWS.map(|s| (s * analysis_rate).round() as usize);
        let longest = windows[2];
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
            ring: vec![0.0; longest],
            write: 0,
            filled: 0,
            window: vec![0.0; longest],
            windows,
            hop: ((HOP_SECONDS * analysis_rate).round() as usize).max(1),
            since_hop: 0,
            detector: Detector::new(),
            sounding: None,
            candidate: None,
            unvoiced: 0,
            last_bend: 0.0,
            since_bend: 0,
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

    /// Input frames per analysis frame step.
    fn hop_frames(&self) -> usize {
        self.hop * self.decimate
    }

    /// End the sounding note (the input stopped).
    pub fn reset(&mut self) -> Option<VoiceEvent> {
        self.candidate = None;
        self.unvoiced = 0;
        self.filled = 0;
        self.since_hop = 0;
        self.phase = 0;
        for filter in &mut self.filters {
            filter.reset();
        }
        self.sounding.take().map(|key| VoiceEvent::NoteOff { key })
    }

    /// Feed `x`; `emit(now, at, event)` for what it heard: `now` the frame
    /// of `x` where it was decided, `at` where the sound began (frames from
    /// the start of `x`; negative: before it).
    pub fn process(&mut self, x: &[f32], mut emit: impl FnMut(isize, isize, VoiceEvent)) {
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
            if self.since_hop >= self.hop && self.filled >= self.windows[0] {
                self.since_hop = 0;
                self.analyse(i as isize, &mut emit);
            }
        }
    }

    /// The newest `n` analysis samples into the window buffer.
    fn take_window(&mut self, n: usize) -> &[f32] {
        let len = self.ring.len();
        for k in 0..n {
            self.window[k] = self.ring[(self.write + len - n + k) % len];
        }
        &self.window[..n]
    }

    /// The pitch of the newest `n` samples, when clear, in range and of a
    /// period the window holds enough of.
    fn pitch_over(&mut self, n: usize) -> Option<Heard> {
        let rate = self.analysis_rate;
        let c = self.config;
        let w: &[f32] = {
            let len = self.ring.len();
            for k in 0..n {
                self.window[k] = self.ring[(self.write + len - n + k) % len];
            }
            &self.window[..n]
        };
        let p = self.detector.detect(w, rate)?;
        let periods = n as f64 * p.freq / rate;
        let pitch = note_at(p.freq, c.reference);
        (p.clarity >= c.clarity
            && (LOWEST_HZ..=HIGHEST_HZ).contains(&p.freq)
            && periods >= PERIODS
            && (f64::from(c.low) - 0.5..=f64::from(c.high) + 0.5).contains(&pitch))
        .then_some(Heard { pitch, window: n })
    }

    /// What one frame hears: while a note sounds, over two and a half of
    /// its periods; else the shortest window that is sure.
    fn hear(&mut self) -> Option<Heard> {
        let filled = self.filled;
        if let Some(k) = self.sounding {
            let f = self.config.reference * 2f64.powf((f64::from(k) - 69.0) / 12.0);
            let n = ((2.6 * self.analysis_rate / f).ceil() as usize)
                .clamp(self.windows[0], self.windows[2])
                .min(filled);
            if let Some(h) = self.pitch_over(n) {
                return Some(h);
            }
        }
        for i in 0..self.windows.len() {
            let n = self.windows[i];
            if n > filled {
                break;
            }
            if let Some(h) = self.pitch_over(n) {
                return Some(h);
            }
        }
        None
    }

    /// One analysis frame ending at input frame `now` (of the current
    /// slice).
    fn analyse(&mut self, now: isize, emit: &mut impl FnMut(isize, isize, VoiceEvent)) {
        // The level over the newest 6 ms.
        let recent = (self.hop * 6).min(self.filled);
        let power = self
            .take_window(recent)
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            / recent.max(1) as f64;
        let level_db = (10.0 * power.max(1e-12).log10()) as f32;
        let c = self.config;
        let heard = if level_db >= c.threshold_db {
            self.hear()
        } else {
            None
        };
        let hop = self.hop_frames() as isize;
        let half = |window: usize| (window / 2 * self.decimate) as isize;
        let Some(h) = heard else {
            self.candidate = None;
            self.unvoiced += 1;
            if self.unvoiced >= c.release_frames
                && let Some(key) = self.sounding.take()
            {
                let back = (self.unvoiced as isize - 1) * hop;
                emit(now, now - back, VoiceEvent::NoteOff { key });
            }
            return;
        };
        self.unvoiced = 0;
        // The key heard: the sounding one while within its hysteresis,
        // else the nearest the scale allows.
        let key = match self.sounding {
            Some(s) if (h.pitch - f64::from(s)).abs() < 0.5 + HYSTERESIS => s,
            _ => nearest_in_scale(h.pitch, c.scale, c.low, c.high),
        };
        if self.sounding == Some(key) {
            self.candidate = None;
            self.since_bend += 1;
            let semitones = (h.pitch - f64::from(key)) as f32;
            let moved = (semitones - self.last_bend).abs();
            if moved >= 0.1 || (moved >= 0.02 && self.since_bend >= BEND_FRAMES) {
                self.last_bend = semitones;
                self.since_bend = 0;
                emit(
                    now,
                    now - half(h.window),
                    VoiceEvent::Bend { key, semitones },
                );
            }
            return;
        }
        let (count, loudest, first_window) = match self.candidate {
            Some((k, n, l, w)) if k == key => (n + 1, l.max(level_db), w),
            _ => (1, level_db, h.window),
        };
        self.candidate = Some((key, count, loudest, first_window));
        let needed = if self.sounding.is_some() {
            c.change_frames
        } else {
            c.onset_frames
        };
        if count < needed {
            return;
        }
        let at = now - half(first_window) - (count as isize - 1) * hop;
        if let Some(old) = self.sounding.take() {
            emit(now, at, VoiceEvent::NoteOff { key: old });
        }
        let velocity = velocity_of(loudest, c.threshold_db);
        emit(now, at, VoiceEvent::NoteOn { key, velocity });
        self.sounding = Some(key);
        self.candidate = None;
        self.last_bend = 0.0;
        let semitones = (h.pitch - f64::from(key)) as f32;
        if semitones.abs() >= 0.02 {
            self.last_bend = semitones;
            emit(now, at, VoiceEvent::Bend { key, semitones });
        }
    }
}

/// The nearest key to `pitch` whose pitch class the scale has, in range.
pub(crate) fn nearest_in_scale(pitch: f64, scale: u16, low: u8, high: u8) -> u8 {
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
pub(crate) fn velocity_of(level_db: f32, threshold_db: f32) -> u8 {
    let t = ((level_db - threshold_db) / 40.0).clamp(0.0, 1.0);
    (30.0 + t * 97.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    /// A sung phrase: (start s, end s, key, vibrato cents); a glottal-ish
    /// saw with a falling spectrum, a 5 ms attack and release.
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
                let env = (t / 0.005).min(1.0) * ((b - a - t) / 0.005).clamp(0.0, 1.0);
                *o += (v * env) as f32 * gain;
            }
        }
        out
    }

    /// Run in blocks; events with the absolute frames they were decided
    /// at and placed at.
    fn track(x: &[f32], config: VoiceConfig) -> Vec<(i64, i64, VoiceEvent)> {
        let mut t = VoiceTracker::new(RATE, config);
        let mut out = Vec::new();
        for (b, chunk) in x.chunks(256).enumerate() {
            let base = (b * 256) as i64;
            t.process(chunk, |now, at, e| {
                out.push((base + now as i64, base + at as i64, e));
            });
        }
        if let Some(e) = t.reset() {
            out.push((x.len() as i64, x.len() as i64, e));
        }
        out
    }

    /// The notes: (key, start, end) by where they were placed (s).
    fn notes(events: &[(i64, i64, VoiceEvent)]) -> Vec<(u8, f64, f64)> {
        let mut out = Vec::new();
        let mut open: Option<(u8, i64)> = None;
        for &(_, at, e) in events {
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

    /// How long after a note's start its note-on was decided (ms).
    fn onset_ms(key: f64, r: Responsiveness) -> f64 {
        let x = phrase(&[(0.1, 0.5, key, 0.0)], 0.7, 0.3);
        let ev = track(&x, VoiceConfig::default().with(r));
        let on = ev
            .iter()
            .find(|e| matches!(e.2, VoiceEvent::NoteOn { .. }))
            .unwrap_or_else(|| panic!("no note at {key}"));
        assert!(matches!(on.2, VoiceEvent::NoteOn { key: k, .. } if f64::from(k) == key));
        (on.0 as f64 / RATE - 0.1) * 1000.0
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
        for r in Responsiveness::ALL {
            let ev = track(&x, VoiceConfig::default().with(r));
            let got = notes(&ev);
            assert_eq!(
                got.iter().map(|n| n.0).collect::<Vec<_>>(),
                [57, 60, 64],
                "{r:?}: {got:?}"
            );
            // Placed where sung (within 15 ms), legato into the second.
            for ((_, s, e), (a, b)) in got.iter().zip([(0.10, 0.50), (0.50, 0.90), (1.20, 1.60)]) {
                assert!((s - a).abs() < 0.015, "{r:?}: start {s} for {a}");
                assert!((e - b).abs() < 0.03, "{r:?}: end {e} for {b}");
            }
            // Vibrato stays on the note, as bends of about a quarter tone.
            let bends: Vec<f32> = ev
                .iter()
                .filter_map(|(_, _, e)| match e {
                    VoiceEvent::Bend { semitones, .. } => Some(*semitones),
                    _ => None,
                })
                .collect();
            // (Up to the hysteresis just before a legato change, where the
            // pitch already moves to the next note.)
            assert!(bends.iter().all(|b| b.abs() <= 0.81), "{r:?}: {bends:?}");
            let mut sizes: Vec<f32> = bends.iter().map(|b| b.abs()).collect();
            sizes.sort_by(f32::total_cmp);
            let typical = sizes[sizes.len() * 9 / 10];
            assert!(typical < 0.35, "{r:?}: 90 % of bends under {typical}");
            assert!(bends.iter().any(|b| b.abs() > 0.1), "vibrato heard");
        }
    }

    /// The point: notes start a few milliseconds after they are sung.
    #[test]
    fn notes_start_within_milliseconds() {
        let mut table = Vec::new();
        for key in [45.0, 57.0, 69.0, 76.0] {
            for r in Responsiveness::ALL {
                table.push((key, r, onset_ms(key, r)));
            }
        }
        for (key, r, ms) in &table {
            eprintln!("key {key} {r:?}: {ms:.1} ms");
        }
        let at = |key: f64, r: Responsiveness| {
            table
                .iter()
                .find(|(k, q, _)| *k == key && *q == r)
                .map_or(f64::MAX, |t| t.2)
        };
        // About two and a half periods (the least a pitch can be heard
        // in) and a frame or two: A4 in 8 ms, A3 in 14, A2 in 26.
        for (key, fast, balanced) in [(76.0, 6.5, 9.0), (69.0, 8.0, 11.0), (57.0, 14.0, 17.0)] {
            assert!(at(key, Responsiveness::Fast) <= fast, "{key}");
            assert!(at(key, Responsiveness::Balanced) <= balanced, "{key}");
        }
        assert!(at(45.0, Responsiveness::Fast) <= 26.0);
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
                let w = 2.0 * std::f64::consts::PI * 196.0 * t;
                (0.15 * w.sin() + 0.3 * (2.0 * w).sin() + 0.1 * (3.0 * w).sin()) as f32
            })
            .collect();
        for r in Responsiveness::ALL {
            let got = notes(&track(&x, VoiceConfig::default().with(r)));
            assert_eq!(
                got.iter().map(|n| n.0).collect::<Vec<_>>(),
                [55],
                "{r:?}: {got:?}"
            );
        }
    }
}
