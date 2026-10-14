//! Singing into MIDI live: a causal pitch tracker that turns a monophonic
//! input (a voice, a whistle, a monophonic instrument) into notes as it
//! comes in, fast enough to play an instrument with.
//!
//! The input is high-passed under the voice (rumble), low-passed and
//! decimated to about 11 kHz; every 1 ms the newest audio is run through
//! McLeod's method ([`crate::pitch`]) over the shortest window that holds
//! enough periods to be sure — 12 ms first (most voices), 22 or 40 ms only
//! for low ones, and while a note sounds, two and a half of its periods —
//! and the level measured. A pitch an octave (or a twelfth) under the one
//! found that is clearer still is the pitch (a strong second harmonic, a
//! vowel's first formant on it); before a note starts, the next longer
//! window must not clearly hear another pitch.
//!
//! A frame is voiced when its pitch is clear and it is loud enough: over
//! [`VoiceConfig::threshold_db`] and over the input's own noise floor,
//! which the tracker follows as it plays (so the gate sits over the room
//! whatever the input's gain; see [`level`]); mains hum starts no note. A
//! note starts when the same key (the nearest the scale allows) holds for
//! [`VoiceConfig::onset_frames`] (at Stable, once a scoop has settled) and
//! is placed where the voice began. It changes when another key holds for
//! [`VoiceConfig::change_frames`] once the pitch has settled (the passing
//! keys of a glide are no notes), never while the voice fades (a fall as
//! it ends), and ends after [`VoiceConfig::release_frames`] unvoiced. A
//! young note follows the key the voice settles on (a scoop that ended
//! between keys); an established one holds through the swing it has shown
//! (vibrato, up to almost a semitone each way). While a note sounds, its
//! pitch against the key comes out as [`VoiceEvent::Bend`] (for glides).
//! Velocities follow the performer's loud notes, or the dynamics a
//! calibration measured ([`calibrate`]).
//!
//! Each event comes with two places: where in the input it was decided
//! (`now`: play it there) and where the sound it reports began (`at`,
//! earlier by half the window and the holds: record it there).
//! Allocation-free after [`VoiceTracker::new`], so it runs on the audio
//! thread.

use crate::pitch::{Detector, Pitch, note_at};
use std::sync::Arc;

mod calibration;
pub mod level;

pub use calibration::{Advice, Calibration, CalibrationError, calibrate};
pub(crate) use level::LevelGate;
pub use level::{Dynamics, Levels, METER_BINS, Meter};

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
/// A pitch an octave or a twelfth under the one found is taken when it is
/// this much clearer.
const LOWER_CLEARER: f64 = 0.04;
/// Before a note starts, a longer window hearing a pitch this clear more
/// than this far (semitones) from the frame's makes the frame unsure.
const CONTRADICTS: f64 = 0.6;
const AGREES: f64 = 0.6;
/// Bends at most this often (frames), unless the pitch jumps.
const BEND_FRAMES: u32 = 4;
/// The voice's range (Hz; whistles reach past 2 kHz).
const LOWEST_HZ: f64 = 60.0;
const HIGHEST_HZ: f64 = 2_300.0;
/// The input is high-passed here (Hz).
const HIGH_PASS_HZ: f64 = 50.0;
/// Half a semitone, and this much more, before another key is heard.
const HYSTERESIS: f64 = 0.3;
/// A note's first frames hold it with only this much: a note begun on the
/// way up a scoop moves to the key the voice settles on.
const YOUNG_FRAMES: u32 = 100;
const YOUNG_HYSTERESIS: f64 = 0.1;
/// An established note holds while the voice stays within the swing it
/// showed over these frames (vibrato), and this much more (semitones),
/// up to the widest.
const SWING_FRAMES: usize = 250;
const SWING_MARGIN: f64 = 0.15;
const WIDEST: f64 = 0.95;
/// Frames of pitch kept.
const HISTORY: usize = 256;
/// A change waits for the pitch to settle — within this spread
/// (semitones) over the last frames — unless the new key holds this many
/// times its change frames.
const STEADY_FRAMES: usize = 8;
const STEADY_SPREAD: f64 = 0.4;
const STEADY_HOLD: u32 = 3;
/// ... nor while the voice fades by this much (dB) over those frames (a
/// fall as a note ends).
const FADING_DB: f32 = 3.0;
/// A note does not start while the pitch slides at least this fast
/// (semitones/s) toward another key it would reach within this long (s).
const SCOOP_SLOPE: f64 = 8.0;
const SCOOP_AHEAD: f64 = 0.03;
/// A note starting within this many frames of where the voice began is
/// placed there; voicing ends after this many unvoiced frames.
const ONSET_REACH: u32 = 150;
const VOICING_GAP: u32 = 3;

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

    /// Frames (1 ms) a note waits at most for a scoop to settle. (Only
    /// Stable waits: a note's first frames drift as its window fills, and
    /// the others start before a slope can be told from that; a young
    /// note follows the key the voice settles on instead.)
    pub fn settle_frames(self) -> u32 {
        match self {
            Responsiveness::Fast | Responsiveness::Balanced => 0,
            Responsiveness::Stable => 90,
        }
    }
}

/// The threshold of a new configuration (dBFS): low, the tracker's own
/// gate keeps over the room's noise.
pub const DEFAULT_THRESHOLD_DB: f32 = -62.0;

/// How the tracker hears.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceConfig {
    /// The responsiveness the holds came from (the polyphonic tracker's
    /// window follows it).
    pub responsiveness: Responsiveness,
    /// Quieter than this (dBFS) is silence; the tracker's own gate (over
    /// the noise floor it follows) may be higher.
    pub threshold_db: f32,
    /// The levels velocities span (a calibration's); `None` follows the
    /// performer's loud notes.
    pub dynamics: Option<Dynamics>,
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
            threshold_db: DEFAULT_THRESHOLD_DB,
            dynamics: None,
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

    pub(crate) fn highpass(cutoff: f64, rate: f64, q: f64) -> Self {
        let w = 2.0 * std::f64::consts::PI * cutoff / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos) / 2.0 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) / 2.0 / a0,
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

/// A key being heard that is not the sounding one.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    key: u8,
    /// Frames it held.
    count: u32,
    /// The loudest level meanwhile (velocity).
    loudest: f32,
    /// The window of its first frame.
    window: usize,
}

/// The live tracker (see the module docs).
pub struct VoiceTracker {
    config: VoiceConfig,
    /// Input frames per analysis frame.
    decimate: usize,
    phase: usize,
    filters: [Biquad; 3],
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
    /// Frames since the sounding note began.
    age: u32,
    candidate: Option<Candidate>,
    unvoiced: u32,
    /// Voiced frames since the voice began (unvoiced gaps shorter than
    /// `VOICING_GAP` included) and the window of its first.
    voicing: u32,
    voicing_window: usize,
    last_bend: f32,
    /// Frames since the last bend.
    since_bend: u32,
    /// Each frame's pitch (NaN: unvoiced), a ring by frame.
    pitches: [f32; HISTORY],
    /// ... and its level (dBFS).
    levels: [f32; HISTORY],
    frame: usize,
    gate: LevelGate,
    meter: Option<Arc<Meter>>,
}

impl VoiceTracker {
    pub fn new(rate: f64, config: VoiceConfig) -> Self {
        let decimate = ((rate / ANALYSIS_RATE).floor() as usize).max(1);
        let analysis_rate = rate / decimate as f64;
        let windows = WINDOWS.map(|s| (s * analysis_rate).round() as usize);
        let longest = windows[2];
        // Two sections: a fourth-order Butterworth well below the new
        // Nyquist; and a high-pass under the voice's range (rumble, the
        // room's low noise: pitches the voice has none of).
        let cutoff = (analysis_rate * 0.42).min(rate * 0.45);
        let hop = ((HOP_SECONDS * analysis_rate).round() as usize).max(1);
        Self {
            config,
            decimate,
            phase: 0,
            filters: [
                Biquad::lowpass(cutoff, rate, 0.541_196_1),
                Biquad::lowpass(cutoff, rate, 1.306_563),
                Biquad::highpass(HIGH_PASS_HZ, rate, std::f64::consts::FRAC_1_SQRT_2),
            ],
            analysis_rate,
            ring: vec![0.0; longest],
            write: 0,
            filled: 0,
            window: vec![0.0; longest],
            windows,
            hop,
            since_hop: 0,
            detector: Detector::new(),
            sounding: None,
            age: 0,
            candidate: None,
            unvoiced: 0,
            voicing: 0,
            voicing_window: 0,
            last_bend: 0.0,
            since_bend: 0,
            pitches: [f32::NAN; HISTORY],
            frame: 0,
            gate: LevelGate::new(hop as f64 / analysis_rate),
            levels: [f32::MIN; HISTORY],
            meter: None,
        }
    }

    /// Count what it hears into `meter` (made on the control thread; the
    /// audio thread only adds to it).
    pub fn with_meter(mut self, meter: Arc<Meter>) -> Self {
        self.meter = Some(meter);
        self
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

    /// End the sounding note (the input stopped). The noise floor it
    /// followed stays (the room is the same).
    pub fn reset(&mut self) -> Option<VoiceEvent> {
        self.candidate = None;
        self.unvoiced = 0;
        self.voicing = 0;
        self.filled = 0;
        self.since_hop = 0;
        self.phase = 0;
        self.pitches.fill(f32::NAN);
        self.levels.fill(f32::MIN);
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
    /// period the window holds enough of; an octave or a twelfth lower
    /// when that is clearer still.
    fn pitch_over(&mut self, n: usize) -> Option<Heard> {
        let c = self.config;
        let p = self.detect_over(n)?;
        let periods = n as f64 * p.freq / self.analysis_rate;
        let pitch = note_at(p.freq, c.reference);
        (p.clarity >= c.clarity
            && (LOWEST_HZ..=HIGHEST_HZ).contains(&p.freq)
            && periods >= PERIODS
            && (f64::from(c.low) - 0.5..=f64::from(c.high) + 0.5).contains(&pitch))
        .then_some(Heard { pitch, window: n })
    }

    /// The pitch of the newest `n` samples as heard, an octave or a twelfth
    /// lower when that is clearer still.
    fn detect_over(&mut self, n: usize) -> Option<Pitch> {
        let rate = self.analysis_rate;
        self.take_window(n);
        let mut p = self.detector.detect_above(&self.window[..n], rate, 1e-14)?;
        let lag = rate / p.freq;
        for times in [2.0, 3.0] {
            if let Some((l, clarity)) = self.detector.peak_near(lag * times)
                && clarity >= p.clarity + LOWER_CLEARER
                && n as f64 / l >= PERIODS
            {
                p = Pitch {
                    freq: rate / l,
                    clarity,
                };
            }
        }
        Some(p)
    }

    /// What one frame hears: while a note sounds, over two and a half of
    /// its periods; else the shortest window that is sure — and, before a
    /// note starts, that the next longer window does not clearly hear
    /// another pitch (a weak voice's first periods can sound a twelfth or
    /// an octave off in one window).
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
                if self.sounding.is_none()
                    && let Some(&longer) = self.windows.get(i + 1)
                    && longer <= filled
                    && let Some(p) = self.detect_over(longer)
                    && p.clarity >= CONTRADICTS
                    && longer as f64 * p.freq / self.analysis_rate >= PERIODS
                    && (note_at(p.freq, self.config.reference) - h.pitch).abs() > AGREES
                {
                    return None;
                }
                return Some(h);
            }
        }
        None
    }

    /// The pitch `back` frames ago (0: this frame; NaN: unvoiced).
    fn pitch_back(&self, back: usize) -> f64 {
        f64::from(self.pitches[(self.frame + HISTORY - back % HISTORY) % HISTORY])
    }

    /// How far the voiced pitch of the frames `from..to` ago spread
    /// (semitones; 0 with fewer than two).
    fn spread(&self, from: usize, to: usize) -> f64 {
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for b in from..to {
            let p = self.pitch_back(b);
            if p.is_finite() {
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        if hi >= lo { hi - lo } else { 0.0 }
    }

    /// The pitch's slope over the last `n` frames (semitones per frame; a
    /// least-squares line over the voiced ones).
    fn slope(&self, n: usize) -> f64 {
        let (mut sx, mut sy, mut sxx, mut sxy, mut k) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for b in 0..n {
            let p = self.pitch_back(b);
            if p.is_finite() {
                let x = -(b as f64);
                sx += x;
                sy += p;
                sxx += x * x;
                sxy += x * p;
                k += 1.0;
            }
        }
        let d = k * sxx - sx * sx;
        if k < 3.0 || d.abs() < 1e-9 {
            0.0
        } else {
            (k * sxy - sx * sy) / d
        }
    }

    /// Is the voice fading (its level falling over the last frames)?
    fn fading(&self) -> bool {
        let back = (self.frame + HISTORY - STEADY_FRAMES) % HISTORY;
        self.levels[self.frame] < self.levels[back] - FADING_DB
    }

    /// Is the pitch (now `pitch`, nearest `key`) sliding fast enough to
    /// reach another key soon (a scoop, a glide, a fall)?
    fn sliding(&self, pitch: f64, key: u8) -> bool {
        let c = self.config;
        let slope = self.slope(STEADY_FRAMES) / HOP_SECONDS;
        slope.abs() >= SCOOP_SLOPE
            && nearest_in_scale(pitch + slope * SCOOP_AHEAD, c.scale, c.low, c.high) != key
    }

    /// Does `pitch` belong to the sounding key `key`? A young note's within
    /// a little more than half a semitone; an established one's within the
    /// swing it showed before the frames a candidate holds (vibrato), at
    /// least the hysteresis and at most the widest.
    fn holds(&self, key: u8, pitch: f64) -> bool {
        let d = pitch - f64::from(key);
        if self.age < YOUNG_FRAMES {
            return d.abs() < 0.5 + YOUNG_HYSTERESIS;
        }
        let skip = self.candidate.map_or(1, |c| c.count as usize + 1);
        let (mut up, mut down) = (0f64, 0f64);
        for b in skip..SWING_FRAMES.min(self.age as usize) {
            let p = self.pitch_back(b) - f64::from(key);
            if p.is_finite() {
                up = up.max(p);
                down = down.max(-p);
            }
        }
        let least = 0.5 + HYSTERESIS;
        let up = (up + SWING_MARGIN).clamp(least, WIDEST);
        let down = (down + SWING_MARGIN).clamp(least, WIDEST);
        -down < d && d < up
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
        let level_db = (10.0 * power.max(1e-14).log10()) as f32;
        let c = self.config;
        self.gate.hear(
            level_db,
            self.sounding.is_some() || self.candidate.is_some(),
            c.threshold_db,
        );
        let gate = self.gate.gate_db(c.threshold_db);
        let heard = if level_db >= gate { self.hear() } else { None };
        // Mains hum starts no note (no equal-tempered note is on it): a
        // pitch on it only carries on the note it belongs to.
        let heard = heard.filter(|h| {
            !level::on_mains(c.reference * 2f64.powf((h.pitch - 69.0) / 12.0))
                || self.sounding.is_some_and(|s| self.holds(s, h.pitch))
        });
        self.frame = (self.frame + 1) % HISTORY;
        self.pitches[self.frame] = heard.map_or(f32::NAN, |h| h.pitch as f32);
        self.levels[self.frame] = level_db;
        if let Some(m) = &self.meter {
            m.count(level_db, gate, heard.is_some());
        }
        if self.sounding.is_some() {
            self.age = self.age.saturating_add(1);
        }
        let hop = self.hop_frames() as isize;
        let half = |window: usize| (window / 2 * self.decimate) as isize;
        let Some(h) = heard else {
            self.candidate = None;
            self.unvoiced += 1;
            if self.unvoiced >= VOICING_GAP {
                self.voicing = 0;
            }
            if self.unvoiced >= c.release_frames
                && let Some(key) = self.sounding.take()
            {
                let back = (self.unvoiced as isize - 1) * hop;
                emit(now, now - back, VoiceEvent::NoteOff { key });
            }
            return;
        };
        self.voicing = if self.voicing == 0 {
            self.voicing_window = h.window;
            1
        } else {
            // (Short gaps count with it.)
            self.voicing.saturating_add(1 + self.unvoiced)
        };
        self.unvoiced = 0;
        // The key heard: the sounding one while the voice holds to it, else
        // the nearest the scale allows.
        let key = match self.sounding {
            Some(s) if self.holds(s, h.pitch) => s,
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
        let cand = match self.candidate {
            Some(k) if k.key == key => Candidate {
                count: k.count + 1,
                loudest: k.loudest.max(level_db),
                ..k
            },
            _ => Candidate {
                key,
                count: 1,
                loudest: level_db,
                window: h.window,
            },
        };
        self.candidate = Some(cand);
        let count = cand.count;
        let starting = self.sounding.is_none();
        if starting {
            if count < c.onset_frames {
                return;
            }
            // A scoop: wait while the pitch slides toward another key.
            if self.voicing < c.responsiveness.settle_frames() && self.sliding(h.pitch, key) {
                return;
            }
        } else {
            if count < c.change_frames {
                return;
            }
            // Passing keys (a glide, a fall) are no notes: the pitch
            // settles first. A voice fading (falling as it ends) changes
            // nothing.
            let span = (count as usize).min(STEADY_FRAMES);
            if self.fading()
                || count < STEADY_HOLD * c.change_frames
                    && (self.spread(0, span) > STEADY_SPREAD || self.sliding(h.pitch, key))
            {
                return;
            }
        }
        // A new note is placed where the voice began; a change where the
        // new key was first heard.
        let at = if starting && self.voicing <= ONSET_REACH {
            now - half(self.voicing_window) - (self.voicing as isize - 1) * hop
        } else {
            now - half(cand.window) - (count as isize - 1) * hop
        };
        if let Some(old) = self.sounding.take() {
            emit(now, at, VoiceEvent::NoteOff { key: old });
        }
        let velocity = self.gate.velocity(cand.loudest, c.threshold_db, c.dynamics);
        emit(now, at, VoiceEvent::NoteOn { key, velocity });
        self.sounding = Some(key);
        self.age = 0;
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
    fn input_under_the_threshold_plays_nothing_and_the_scale_snaps() {
        let x = phrase(&[(0.1, 0.5, 61.0, 0.0)], 0.8, 0.001);
        assert!(track(&x, VoiceConfig::default()).is_empty());
        let sensitive = VoiceConfig {
            threshold_db: -80.0,
            ..VoiceConfig::default()
        };
        assert_eq!(keys_of(&notes(&track(&x, sensitive))), [61], "heard");
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

    /// A voice following `pitch(t)` (a key, NaN: silent): a saw with a
    /// falling spectrum, 10 ms in and out.
    fn sing(total: f64, gain: f32, pitch: impl Fn(f64) -> f64) -> Vec<f32> {
        let mut phase = 0.0f64;
        let mut env = 0.0f64;
        (0..(total * RATE) as usize)
            .map(|j| {
                let p = pitch(j as f64 / RATE);
                env = (env + if p.is_nan() { -1.0 } else { 1.0 } / (0.01 * RATE)).clamp(0.0, 1.0);
                if !p.is_nan() {
                    phase = (phase + 440.0 * 2f64.powf((p - 69.0) / 12.0) / RATE).fract();
                }
                let v: f64 = (1..12)
                    .map(|h| (std::f64::consts::TAU * phase * h as f64).sin() / (h * h) as f64)
                    .sum();
                (v * env) as f32 * gain
            })
            .collect()
    }

    fn keys_of(got: &[(u8, f64, f64)]) -> Vec<u8> {
        got.iter().map(|n| n.0).collect()
    }

    /// Any gain: the same phrase 30 dB quieter plays the same notes at
    /// the same velocities (the gate keeps over the input's own noise,
    /// velocities follow the performer's loud notes).
    #[test]
    fn a_quiet_input_plays_like_a_loud_one() {
        let sung = [
            (0.30, 0.70, 57.0, 25.0),
            (0.70, 1.10, 60.0, 25.0),
            (1.40, 1.80, 64.0, 30.0),
        ];
        let mut heard = Vec::new();
        for gain in [0.3f32, 0.01] {
            let mut x = phrase(&sung, 2.2, gain);
            // Noise 50 dB under the voice.
            let mut seed = 7u32;
            for v in &mut x {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *v += ((seed >> 8) as f32 / (1 << 24) as f32 - 0.5) * gain * 0.006;
            }
            let ev = track(&x, VoiceConfig::default());
            assert_eq!(keys_of(&notes(&ev)), [57, 60, 64], "gain {gain}");
            let velocities: Vec<u8> = ev
                .iter()
                .filter_map(|e| match e.2 {
                    VoiceEvent::NoteOn { velocity, .. } => Some(velocity),
                    _ => None,
                })
                .collect();
            heard.push(velocities);
        }
        for (loud, quiet) in heard[0].iter().zip(&heard[1]) {
            assert!(loud.abs_diff(*quiet) <= 6, "{heard:?}");
        }
    }

    /// The keys a glide passes are no notes, nor a fall at a note's end;
    /// vibrato on a note a little flat plays it once; a scoop into a note
    /// ends on it.
    #[test]
    fn glides_falls_vibrato_and_scoops_play_only_their_notes() {
        let pitch = |t: f64| -> f64 {
            let vib = |from: f64| 0.6 * (std::f64::consts::TAU * 5.5 * (t - from)).sin();
            match t {
                // A scoop from a semitone and a half under.
                t if (0.2..0.7).contains(&t) => {
                    57.0 - 1.5 * (1.0 - ((t - 0.2) / 0.08).min(1.0)).powi(2)
                }
                // Legato up a major third over 80 ms.
                t if (0.7..0.78).contains(&t) => {
                    57.0 + 4.0 * (0.5 - 0.5 * (std::f64::consts::PI * (t - 0.7) / 0.08).cos())
                }
                // Wide vibrato, 30 cents flat.
                t if (0.78..1.6).contains(&t) => 60.7 + vib(0.78),
                // ... and a fall of a whole tone at its end.
                t if (1.6..1.7).contains(&t) => 60.7 - 2.0 * ((t - 1.6) / 0.1).powi(2),
                _ => f64::NAN,
            }
        };
        let x = sing(2.0, 0.3, pitch);
        for r in [Responsiveness::Balanced, Responsiveness::Stable] {
            let got = notes(&track(&x, VoiceConfig::default().with(r)));
            let keys = keys_of(&got);
            // (Balanced may start the scoop a key under and move up at
            // once: the note sung is there, early.)
            assert!(
                keys == [57, 61] || (r == Responsiveness::Balanced && keys == [56, 57, 61]),
                "{r:?}: {got:?}"
            );
            let held = got.iter().find(|n| n.0 == 61).map(|n| (n.1, n.2));
            assert!(
                held.is_some_and(|(s, e)| (s - 0.74).abs() < 0.04 && e > 1.6),
                "{r:?}: {got:?}"
            );
        }
    }

    /// Mains hum (60 Hz and its harmonics), loud, starts no note; a voice
    /// over it is heard.
    #[test]
    fn mains_hum_starts_no_note() {
        let hum = |j: usize| {
            let w = std::f64::consts::TAU * 60.0 * j as f64 / RATE;
            (0.05 * (w.sin() + 0.7 * (2.0 * w).sin() + 0.5 * (3.0 * w).sin())) as f32
        };
        let x: Vec<f32> = (0..(2.0 * RATE) as usize).map(hum).collect();
        for r in Responsiveness::ALL {
            assert!(
                track(&x, VoiceConfig::default().with(r)).is_empty(),
                "{r:?}"
            );
        }
        let sung = phrase(&[(0.8, 1.4, 62.0, 20.0)], 2.0, 0.3);
        let x: Vec<f32> = sung.iter().enumerate().map(|(j, v)| v + hum(j)).collect();
        assert_eq!(keys_of(&notes(&track(&x, VoiceConfig::default()))), [62]);
    }

    /// Whistles reach past two kilohertz.
    #[test]
    fn a_high_whistle_is_heard() {
        let n = (0.6 * RATE) as usize;
        let f = 440.0 * 2f64.powf((95.0 - 69.0) / 12.0);
        let x: Vec<f32> = (0..n)
            .map(|j| {
                let t = j as f64 / RATE;
                (0.2 * (std::f64::consts::TAU * f * t).sin() * (t / 0.02).min(1.0)) as f32
            })
            .collect();
        assert_eq!(keys_of(&notes(&track(&x, VoiceConfig::default()))), [95]);
    }

    /// The meter counts every frame's level, and the voiced ones, for the
    /// control side.
    #[test]
    fn the_meter_counts_what_the_tracker_hears() {
        let meter = Arc::new(Meter::new());
        let mut t = VoiceTracker::new(RATE, VoiceConfig::default()).with_meter(meter.clone());
        let x = phrase(&[(0.5, 1.0, 57.0, 0.0)], 1.5, 0.3);
        t.process(&x, |_, _, _| {});
        let l = meter.take();
        assert!(
            (1450..=1500).contains(&l.frames(false)),
            "{}",
            l.frames(false)
        );
        let voiced = l.frames(true);
        assert!((480..=505).contains(&voiced), "{voiced}");
        // The sung frames are near the phrase's level, the rest silence.
        let sung = l.percentile(true, 0.5).unwrap_or(f32::MIN);
        assert!((-24.0..-10.0).contains(&sung), "{sung}");
        assert_eq!(l.percentile(false, 0.3), Some(-127.0));
        assert!(meter.level_db() < -100.0, "silent at the end");
    }
}
