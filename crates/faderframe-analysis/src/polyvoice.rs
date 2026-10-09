//! Chords into MIDI live: a causal tracker of several notes at once from
//! one input with harmonic tones (for example, a guitar), the
//! polyphonic sibling of [`crate::voice`].
//!
//! The input is low-passed and decimated to about 11 kHz. Every 4 ms the
//! newest 96, 128 or 160 ms (by [`Responsiveness`]) are Hann-windowed into a
//! spectrum and its peaks found (frequency and level interpolated). Then,
//! a note at a time, the strongest pitch is taken: each peak in range is a
//! candidate fundamental, scored by the harmonics it explains (weights
//! after Klapuri: low harmonics and high notes count more; levels
//! compressed so that partials far down still count), with the
//! harmonics' frequencies following the fundamental as they are found
//! (stiff strings run sharp). A candidate needs its fundamental and at
//! least two further harmonics where the bandwidth permits. The chosen
//! note takes from each harmonic as much as its smooth spectrum allows —
//! a harmonic louder than
//! its neighbours keeps the rest, as when another note (its octave, its
//! fifth) shares it — and the search goes on over what is left, up to six
//! notes, while a candidate scores a fair share of the first.
//!
//! Over frames, a key starts a note when it holds for a couple of frames,
//! ends when it has not been heard for a few, and starts again when its
//! level jumps (a string plucked again). Sounding notes are kept with less
//! evidence than new ones need. Bends report each note's pitch against its
//! key. The longer window resolves nearby low fundamentals; it costs tens
//! of milliseconds more latency than the monophonic tracker. Ambiguous
//! simultaneous octave doublings and very close pitches may merge. Pure sine waves do not have
//! the harmonic evidence this tracker needs. Events carry where they were
//! decided and where the sound began. Allocation-free after [`PolyTracker::new`].

use crate::pitch::note_at;
use crate::spectrum::fft;
use crate::voice::{
    Biquad, Responsiveness, VoiceConfig, VoiceEvent, nearest_in_scale, velocity_of,
};

/// The analysis rate the input is decimated to (about).
const ANALYSIS_RATE: f64 = 11_025.0;
/// The transform size (the longest window zero-padded to it).
const FFT: usize = 2048;
/// One analysis frame.
pub const HOP_SECONDS: f64 = 0.004;
/// Notes found in one frame, at most.
pub const MAX_NOTES: usize = 6;
/// Keys followed at once (sounding and about to).
const SLOTS: usize = 24;
/// Peaks of one spectrum, at most (the lowest kept).
const MAX_PEAKS: usize = 128;
/// Harmonics scored per candidate.
const MAX_HARMONICS: usize = 20;
/// Partials above this are not looked at (Hz).
const TOP_HZ: f64 = 5_000.0;
/// The range notes are looked for in (keys): A1 to D6.
const LOWEST_KEY: u8 = 33;
const HIGHEST_KEY: u8 = 86;
/// Harmonics above this are not asked whether their fundamental merged
/// into a lobe (Hz); a merged fundamental is within this many of the
/// window's bins of its neighbour's peak and at most this far under it
/// (dB).
const SHOULDER_TOP_HZ: f64 = 2_000.0;
const SHOULDER_LOBES: f64 = 1.6;
const SHOULDER_DB: f64 = 9.0;
/// ... and this much above the lone peak's lobe there (dB).
const SHOULDER_ABOVE_DB: f64 = 3.0;
/// Levels are compressed by this power (whitening).
const COMPRESS: f64 = 0.33;
/// Klapuri's harmonic weights: (f + ALPHA) / (m f + BETA).
const ALPHA: f64 = 40.0;
const BETA: f64 = 320.0;
/// Peaks below the loudest by more than this are not peaks (dB).
const PEAK_FLOOR_DB: f64 = 50.0;
/// A peak stands this far above its octave's noise floor (the level a
/// tenth of its bins are under; dB): noise seldom does.
const ABOVE_NOISE_DB: f64 = 22.0;
/// ... a sounding note's partial only this far (dB).
const ABOVE_NOISE_HELD_DB: f64 = 10.0;
/// A frame's own floor counts less this (dB).
const FRAME_FLOOR_MARGIN_DB: f64 = 6.0;
const NOISE_PERCENTILE: f64 = 0.1;
/// How fast the noise floor may rise (dB/s), and the bands at most.
const NOISE_RISE_DB_PER_S: f64 = 6.0;
/// The share of a lower frame's floor (in dB) the noise floor follows.
const NOISE_FALL: f64 = 0.1;
const BANDS: usize = 12;
/// A candidate this far down (an octave, a twelfth, two octaves) scoring
/// this share of the best is taken first (the best is likely its
/// harmonic).
const LOWER: [f64; 3] = [2.0, 3.0, 4.0];
const LOWER_SHARE: f64 = 0.5;
/// Peaks this near (in the window's bins) a stronger one may be its
/// sidelobes, when within this of a sidelobe's level (dB).
const SIDELOBE_BINS: f64 = 8.0;
const SIDELOBE_MARGIN_DB: f64 = 4.0;
/// A note this far under the frame's loudest (dB) is not new.
const NEW_LEVEL_DB: f32 = 20.0;
/// Notes of one frame are at least this far apart (semitones): nearer is
/// the leakage of a note's lobe.
const APART: f64 = 1.25;
/// A note on another's harmonic keeps at least this share of its
/// fundamental's peak and is at most this far under the loudest (dB).
const HARMONIC_OWN: f64 = 0.4;
const HARMONIC_LEVEL_DB: f32 = 6.0;
/// Energy this near a new key's level (dB) an octave or a twelfth below it
/// may be a note still resolving.
const LOWER_ENERGY_DB: f64 = 12.0;
/// A sounding note whose fundamental has no peak is still there while
/// its level is at most this far under where it was (dB).
const SOUNDING_DROP_DB: f64 = 12.0;
/// Semitones above a note where its harmonics (2nd–10th) fall.
const HARMONIC_INTERVALS: [i32; 9] = [12, 19, 24, 28, 31, 34, 36, 38, 40];
/// Frames more a new key on a sounding note's harmonic holds before it
/// starts.
const HARMONIC_HOLD: u32 = 12;
/// Less than this share of a peak left counts as nothing left.
const LEFT_SHARE: f64 = 0.4;
/// A candidate's fundamental may be this much under its strongest
/// harmonic (dB).
const FUNDAMENTAL_DB: f64 = 24.0;
/// A harmonic is within this of where it should be (semitones), and the
/// more so the higher it is (stiff strings).
const TOLERANCE: f64 = 0.35;
const TOLERANCE_PER_HARMONIC: f64 = 0.012;
/// A new note scores at least this share of the frame's strongest; a
/// sounding one this (it holds on with less).
const NEW_SHARE: f64 = 0.24;
const HELD_SHARE: f64 = 0.1;
/// Half a semitone, and this much more, before another key is heard.
const HYSTERESIS: f64 = 0.3;
/// A sounding key's fundamental this much louder (dB) than a window ago,
/// with an attack between, is played again.
const RETRIGGER_DB: f32 = 3.0;
/// Frames of a note's fundamental level kept (longer than a window).
const HISTORY: usize = 64;
/// An attack: the upper band's energy over the last 10 ms this much above
/// that over the last 50, at least this long after the last (s).
const ONSET_RATIO: f64 = 1.8;
const ONSET_FAST_S: f64 = 0.01;
const ONSET_SLOW_S: f64 = 0.05;
const ONSET_GAP_S: f64 = 0.03;
/// ... and armed again once the fast energy is back under this much of
/// the slow.
const ONSET_REARM: f64 = 1.3;
/// ... and the fast energy this share of the band's recent peak (which
/// falls over this long, s): small stirrings in a decay are no attacks.
const ONSET_OF_PEAK: f64 = 0.2;
const ONSET_PEAK_S: f64 = 0.3;
/// The window holds a fresh attack while more than this share of its
/// energy is in its newer half.
const ATTACK_SHARE: f64 = 0.85;
/// Bends at most this often (frames), unless the pitch jumps.
const BEND_FRAMES: u32 = 2;

#[cfg(test)]
thread_local! {
    pub(crate) static DEBUG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A Hann window's spectrum `d` bins (of the window's length) from its
/// centre, against the centre.
fn hann_lobe(d: f64) -> f64 {
    if d < 1e-6 {
        return 1.0;
    }
    if (d - 1.0).abs() < 1e-6 {
        return 0.5;
    }
    let x = std::f64::consts::PI * d;
    (x.sin() / x / (1.0 - d * d)).abs()
}

/// The window of a responsiveness (seconds).
fn window_seconds(r: Responsiveness) -> f64 {
    match r {
        Responsiveness::Fast => 0.096,
        Responsiveness::Balanced => 0.128,
        Responsiveness::Stable => 0.160,
    }
}

/// Frames a key holds before its note starts, and frames unheard that end
/// it.
fn holds(r: Responsiveness) -> (u32, u32) {
    match r {
        Responsiveness::Fast => (2, 6),
        Responsiveness::Balanced => (2, 8),
        Responsiveness::Stable => (4, 12),
    }
}

/// A spectral peak: frequency (Hz), level (linear amplitude) and what of
/// it no note has taken yet.
#[derive(Clone, Copy, Debug, Default)]
struct Peak {
    freq: f64,
    amp: f64,
    left: f64,
}

/// A candidate note (see [`PolyTracker::candidate`]).
#[derive(Clone, Copy, Debug)]
struct Candidate {
    f0: f64,
    score: f64,
    /// The fundamental's peak (None: a shoulder of a neighbour's lobe).
    fundamental: Option<usize>,
    /// The fundamental's level (what is left of its peak, or the shoulder).
    level: f64,
    /// The harmonics' peaks (from the second).
    harm: [Option<usize>; MAX_HARMONICS],
}

/// A note found in one frame.
#[derive(Clone, Copy, Debug, Default)]
struct Found {
    /// Fractional key.
    pitch: f64,
    level_db: f32,
    /// Its fundamental's level (dB).
    fundamental_db: f32,
}

/// A key followed over frames.
#[derive(Clone, Copy, Debug)]
struct Slot {
    key: u8,
    /// Sounding (else about to: a candidate).
    on: bool,
    /// Frames heard in a row (a candidate's).
    count: u32,
    /// Where a candidate was first heard (absolute analysis input frame).
    first: i64,
    /// The loudest level while a candidate (velocity).
    loudest: f32,
    /// Frames not heard since last heard.
    missing: u32,
    /// Its fundamental's level the last frames it was heard (a ring;
    /// `heard` frames so far).
    history: [f32; HISTORY],
    heard: usize,
    /// The attack it began with (absolute input frame).
    started: i64,
    /// Frames since its note-on.
    age: u32,
    last_bend: f32,
    since_bend: u32,
    /// Heard this frame.
    seen: bool,
    pitch: f64,
    level: f32,
}

impl Slot {
    /// Is a fundamental of `level` (linear) still this sounding note's (not
    /// far under where it was)?
    fn still(&self, level: f64) -> bool {
        self.heard == 0
            || 20.0 * level.max(1e-12).log10()
                >= f64::from(self.history[(self.heard - 1) % HISTORY]) - SOUNDING_DROP_DB
    }

    const EMPTY: Slot = Slot {
        key: 0,
        on: false,
        count: 0,
        first: 0,
        loudest: f32::MIN,
        missing: 0,
        history: [f32::MIN; HISTORY],
        heard: 0,
        started: i64::MIN,
        age: 0,
        last_bend: 0.0,
        since_bend: 0,
        seen: false,
        pitch: 0.0,
        level: f32::MIN,
    };
}

/// The live polyphonic tracker (see the module docs).
pub struct PolyTracker {
    config: VoiceConfig,
    decimate: usize,
    phase: usize,
    filters: [Biquad; 2],
    analysis_rate: f64,
    /// The newest audio (decimated), a ring as long as the longest window.
    ring: Vec<f32>,
    write: usize,
    filled: usize,
    /// The window in use (samples) and its Hann weights.
    window: usize,
    hann: Vec<f64>,
    /// Sinusoid amplitude per magnitude unit (2 / Σ window).
    scale: f64,
    hop: usize,
    since_hop: usize,
    re: Vec<f64>,
    im: Vec<f64>,
    mags: Vec<f64>,
    /// Room to sort an octave's magnitudes (the noise floor).
    scratch: Vec<f64>,
    /// Each bin's noise floor, and each band's.
    floors: Vec<f64>,
    band_floors: [f64; BANDS],
    loudest: f64,
    peaks: Vec<Peak>,
    found: [Found; MAX_NOTES],
    found_count: usize,
    slots: [Slot; SLOTS],
    /// Input frames taken so far.
    clock: i64,
    /// Energy followers (fast, slow), their coefficients, the last attack
    /// (absolute input frame) and whether the window holds mostly it.
    fast: f64,
    slow: f64,
    fast_k: f64,
    slow_k: f64,
    last_onset: i64,
    armed: bool,
    previous: f64,
    /// The upper band's recent peak (falling slowly) and its fall per
    /// sample.
    peak: f64,
    peak_k: f64,
    attack: bool,
}

impl PolyTracker {
    pub fn new(rate: f64, config: VoiceConfig) -> Self {
        let rate = if rate.is_finite() {
            rate.clamp(8_000.0, 384_000.0)
        } else {
            48_000.0
        };
        let decimate = ((rate / ANALYSIS_RATE).floor() as usize).max(1);
        let analysis_rate = rate / decimate as f64;
        let longest = (window_seconds(Responsiveness::Stable) * analysis_rate).ceil() as usize;
        let cutoff = (analysis_rate * 0.42).min(rate * 0.45);
        let mut t = Self {
            config,
            decimate,
            phase: 0,
            filters: [
                Biquad::lowpass(cutoff, rate, 0.541_196_1),
                Biquad::lowpass(cutoff, rate, 1.306_563),
            ],
            analysis_rate,
            ring: vec![0.0; longest.min(FFT)],
            write: 0,
            filled: 0,
            window: 0,
            hann: vec![0.0; longest.min(FFT)],
            scale: 1.0,
            hop: ((HOP_SECONDS * analysis_rate).round() as usize).max(1),
            since_hop: 0,
            re: vec![0.0; FFT],
            im: vec![0.0; FFT],
            mags: vec![0.0; FFT / 2 + 1],
            scratch: vec![0.0; FFT / 2 + 1],
            floors: vec![0.0; FFT / 2 + 1],
            band_floors: [f64::MAX / 1e10; BANDS],
            loudest: 1.0,
            peaks: Vec::with_capacity(MAX_PEAKS),
            found: [Found::default(); MAX_NOTES],
            found_count: 0,
            slots: [Slot::EMPTY; SLOTS],
            clock: 0,
            fast: 0.0,
            slow: 0.0,
            fast_k: 1.0 - (-1.0 / (ONSET_FAST_S * analysis_rate)).exp(),
            slow_k: 1.0 - (-1.0 / (ONSET_SLOW_S * analysis_rate)).exp(),
            last_onset: i64::MIN / 2,
            armed: true,
            previous: 0.0,
            peak: 0.0,
            peak_k: (-1.0 / (ONSET_PEAK_S * analysis_rate)).exp(),
            attack: false,
        };
        t.set_config(config);
        t
    }

    pub fn config(&self) -> VoiceConfig {
        self.config
    }

    /// New settings (allocation-free: the window is recomputed in place).
    pub fn set_config(&mut self, config: VoiceConfig) {
        self.config = config;
        let n = ((window_seconds(config.responsiveness) * self.analysis_rate).round() as usize)
            .min(self.ring.len());
        if n != self.window {
            self.window = n;
            let mut sum = 0.0;
            for (i, w) in self.hann.iter_mut().enumerate().take(n) {
                *w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * (i as f64 + 0.5) / n as f64).cos();
                sum += *w;
            }
            self.scale = 2.0 / sum.max(1e-9);
        }
    }

    /// The notes sounding now.
    pub fn sounding(&self) -> impl Iterator<Item = u8> + '_ {
        self.slots.iter().filter(|s| s.on).map(|s| s.key)
    }

    /// End every sounding note (the input stopped).
    pub fn reset(&mut self, mut emit: impl FnMut(VoiceEvent)) {
        for s in &mut self.slots {
            if s.on {
                emit(VoiceEvent::NoteOff { key: s.key });
            }
            *s = Slot::EMPTY;
        }
        self.filled = 0;
        self.since_hop = 0;
        self.phase = 0;
        self.fast = 0.0;
        self.slow = 0.0;
        self.peak = 0.0;
        self.previous = 0.0;
        self.last_onset = i64::MIN / 2;
        self.armed = true;
        self.band_floors.fill(f64::MAX / 1e10);
        for filter in &mut self.filters {
            filter.reset();
        }
    }

    /// Feed `x`; `emit(now, at, event)` as [`crate::voice::VoiceTracker::process`].
    pub fn process(&mut self, x: &[f32], mut emit: impl FnMut(isize, isize, VoiceEvent)) {
        for (i, &v) in x.iter().enumerate() {
            self.clock += 1;
            let mut s = f64::from(v);
            for f in &mut self.filters {
                s = f.run(s);
            }
            self.phase += 1;
            if self.phase < self.decimate {
                continue;
            }
            self.phase = 0;
            // Attacks in the upper band (the first difference: picks and new
            // partials show over ringing notes); armed again once it has
            // settled.
            let e = (s - self.previous).powi(2);
            self.previous = s;
            self.fast += self.fast_k * (e - self.fast);
            self.slow += self.slow_k * (e - self.slow);
            self.peak = self.fast.max(self.peak * self.peak_k);
            let gap = (ONSET_GAP_S * self.analysis_rate) as i64 * self.decimate as i64;
            if self.fast < ONSET_REARM * self.slow {
                self.armed = true;
            }
            if self.armed
                && self.fast > ONSET_RATIO * self.slow.max(ONSET_OF_PEAK * self.peak)
                && self.fast > 1e-3 * 10f64.powf(f64::from(self.config.threshold_db) / 10.0)
                && self.clock - self.last_onset > gap
            {
                self.last_onset = self.clock;
                self.armed = false;
            }
            self.ring[self.write] = s as f32;
            self.write = (self.write + 1) % self.ring.len();
            self.filled = (self.filled + 1).min(self.ring.len());
            self.since_hop += 1;
            if self.since_hop >= self.hop && self.filled >= self.window {
                self.since_hop = 0;
                self.spectrum();
                self.find_notes();
                self.follow(i as isize, &mut emit);
            }
        }
    }

    /// The window's spectrum and its peaks: local maxima not too far under
    /// the loudest and well above the noise of their octave.
    fn spectrum(&mut self) {
        let n = self.window;
        let len = self.ring.len();
        for k in 0..n {
            self.re[k] = f64::from(self.ring[(self.write + len - n + k) % len]) * self.hann[k];
        }
        // A fresh attack: nearly all of the window's energy in its newer
        // half.
        let (mut all, mut newest) = (0.0, 0.0);
        for k in 0..n {
            let v = f64::from(self.ring[(self.write + len - n + k) % len]).powi(2);
            all += v;
            if k >= n / 2 {
                newest += v;
            }
        }
        // (Or a sound just stopped: nearly all of it in the older half.)
        self.attack = newest > ATTACK_SHARE * all || newest < (1.0 - ATTACK_SHARE) * all;
        self.re[n..].fill(0.0);
        self.im.fill(0.0);
        fft(&mut self.re, &mut self.im);
        let bin = self.analysis_rate / FFT as f64;
        let top = ((TOP_HZ.min(self.analysis_rate * 0.45)) / bin) as usize;
        let mut loudest = 0f64;
        for k in 0..=top {
            let m = (self.re[k].powi(2) + self.im[k].powi(2)).sqrt() * self.scale;
            self.mags[k] = m;
            loudest = loudest.max(m);
        }
        self.loudest = loudest;
        // The noise floor by octaves (at least 24 bins wide): each frame's
        // low percentile, followed down at once and up slowly (an attack
        // smears the window's spectrum for a moment; noise stays).
        let rise = 10f64.powf(NOISE_RISE_DB_PER_S * HOP_SECONDS / 20.0);
        let (mut lo, mut b) = (2, 0);
        while lo < top && b < BANDS {
            let hi = (lo * 2).max(lo + 24).min(top + 1);
            let band = &mut self.scratch[..hi - lo];
            band.copy_from_slice(&self.mags[lo..hi]);
            let nth = ((band.len() as f64 * NOISE_PERCENTILE) as usize).min(band.len() - 1);
            let (_, now, _) = band.select_nth_unstable_by(nth, f64::total_cmp);
            let was = self.band_floors[b];
            self.band_floors[b] = if was >= f64::MAX / 1e11 {
                *now
            } else if *now < was {
                // Down smoothly (a frame's percentile scatters).
                was * (*now / was).powf(NOISE_FALL)
            } else {
                now.min(was * rise)
            };
            // A frame's own (an attack's noise passing the window's middle)
            // counts too, less a margin.
            let frame = *now * 10f64.powf(-FRAME_FLOOR_MARGIN_DB / 20.0);
            self.floors[lo..hi].fill(self.band_floors[b].max(frame));
            lo = hi;
            b += 1;
        }
        let above_noise = 10f64.powf(ABOVE_NOISE_DB / 20.0);
        let above_noise_held = 10f64.powf(ABOVE_NOISE_HELD_DB / 20.0);
        // Sounding notes' fundamentals (their partials need less).
        let mut held = [0f64; SLOTS];
        let mut held_count = 0;
        for s in self.slots.iter().filter(|s| s.on) {
            held[held_count] = self.config.reference * 2f64.powf((s.pitch - 69.0) / 12.0);
            held_count += 1;
        }
        let held = &held[..held_count];
        self.peaks.clear();
        let floor = (loudest * 10f64.powf(-PEAK_FLOOR_DB / 20.0))
            .max(10f64.powf(f64::from(self.config.threshold_db - 20.0) / 20.0));
        for k in 2..top {
            let b = self.mags[k];
            if b < floor || b <= self.mags[k - 1] || b < self.mags[k + 1] {
                continue;
            }
            if b < above_noise * self.floors[k] {
                let f = k as f64 * bin;
                let on_held = held.iter().any(|g| {
                    let m = (f / g).round();
                    (1.0..=MAX_HARMONICS as f64).contains(&m) && (f / (m * g) - 1.0).abs() < 0.02
                });
                if !on_held || b < above_noise_held * self.floors[k] {
                    continue;
                }
            }
            let (a, c) = (
                self.mags[k - 1].max(1e-12).ln(),
                self.mags[k + 1].max(1e-12).ln(),
            );
            let lb = b.ln();
            let d = a - 2.0 * lb + c;
            let p = if d < 0.0 {
                (0.5 * (a - c) / d).clamp(-0.5, 0.5)
            } else {
                0.0
            };
            if self.peaks.len() == MAX_PEAKS {
                break;
            }
            let amp = (lb - 0.25 * (a - c) * p).exp();
            self.peaks.push(Peak {
                freq: (k as f64 + p) * bin,
                amp,
                left: amp,
            });
        }
        // The window's sidelobes beside strong partials are no partials:
        // Hann's fall from 31 dB under the lobe 2.5 bins off by 18 dB an
        // octave further.
        let lobe = self.analysis_rate / self.window as f64;
        let peaks = &mut self.peaks;
        let mut keep = [true; MAX_PEAKS];
        for i in 0..peaks.len() {
            for j in 0..peaks.len() {
                let d = (peaks[i].freq - peaks[j].freq).abs() / lobe;
                if i == j || !(1.5..=SIDELOBE_BINS).contains(&d) {
                    continue;
                }
                let under = 20.0 * (peaks[j].amp / peaks[i].amp).log10();
                let sidelobe = 31.5 + 18.0 * (d / 2.5).max(1.0).log2() - SIDELOBE_MARGIN_DB;
                if under >= sidelobe {
                    keep[i] = false;
                    break;
                }
            }
        }
        let mut n = 0;
        peaks.retain(|_| {
            n += 1;
            keep[n - 1]
        });
    }

    /// What is left of a peak, as it counts (compressed).
    fn weight(&self, left: f64) -> f64 {
        (left / self.loudest).powf(COMPRESS)
    }

    /// The spectrum's level at `f` (between bins, linearly).
    fn level_at(&self, f: f64) -> f64 {
        let x = f / (self.analysis_rate / FFT as f64);
        let k = (x.floor() as usize).min(self.mags.len() - 2);
        let t = (x - k as f64).clamp(0.0, 1.0);
        self.mags[k] * (1.0 - t) + self.mags[k + 1] * t
    }

    /// The level of a fundamental at `f` merged into a neighbour's lobe:
    /// within the lobe, nearly as loud as its peak and louder than the
    /// lobe of the peak alone would be there (else 0).
    fn shoulder(&self, f: f64) -> f64 {
        let bin = self.analysis_rate / self.window as f64;
        let level = self.level_at(f);
        let i = self.peaks.partition_point(|p| p.freq < f);
        let merged = [i.wrapping_sub(1), i].iter().any(|j| {
            self.peaks.get(*j).is_some_and(|p| {
                let d = (p.freq - f).abs() / bin;
                d <= SHOULDER_LOBES && level >= 10f64.powf(-SHOULDER_DB / 20.0) * p.amp
            })
        });
        if merged { self.own_level(f) } else { 0.0 }
    }

    /// The level of a fundamental at `f` without a peak of its own: a
    /// sounding note's wherever it is (its lobe interferes with a
    /// neighbour's now and then), else a shoulder's.
    fn fundamental_at(&self, f: f64) -> f64 {
        let pitch = note_at(f, self.config.reference);
        let level = self.own_level(f);
        if self
            .slots
            .iter()
            .any(|s| s.on && (s.pitch - pitch).abs() < 0.5 && s.still(level))
        {
            level
        } else {
            self.shoulder(f)
        }
    }

    /// The level at `f` when something of its own is there: above what the
    /// lobes of the peaks around would put there together (else 0).
    fn own_level(&self, f: f64) -> f64 {
        let bin = self.analysis_rate / self.window as f64;
        let level = self.level_at(f);
        let spill = self
            .peaks
            .iter()
            .filter(|p| (p.freq - f).abs() <= 3.0 * bin)
            .map(|p| p.amp * hann_lobe((p.freq - f).abs() / bin))
            .sum::<f64>();
        if level >= 10f64.powf(SHOULDER_ABOVE_DB / 20.0) * spill {
            level
        } else {
            0.0
        }
    }

    /// Is there a peak at harmonic frequency `f` (taken or not)?
    fn present(&self, f: f64) -> bool {
        self.nearest(f, 2f64.powf((TOLERANCE + 0.1) / 12.0))
            .is_some()
    }

    /// The peak nearest `f` within `ratio` (above or below).
    fn nearest(&self, f: f64, ratio: f64) -> Option<usize> {
        let i = self.peaks.partition_point(|p| p.freq < f);
        let mut best: Option<(usize, f64)> = None;
        for j in [i.wrapping_sub(1), i] {
            if let Some(p) = self.peaks.get(j) {
                let r = if p.freq > f { p.freq / f } else { f / p.freq };
                if r <= ratio && best.is_none_or(|b| r < b.1) {
                    best = Some((j, r));
                }
            }
        }
        best.map(|b| b.0)
    }

    /// A note at about `f0` whose fundamental is peak `fundamental`, or
    /// (None) a shoulder of `shoulder` level where lobes merged: its
    /// harmonics' peaks, refined fundamental and score over what is left;
    /// None when it is no note (out of range, no fundamental to speak of,
    /// too few harmonics, or one found already).
    fn candidate(
        &self,
        f0: f64,
        fundamental: Option<usize>,
        shoulder: f64,
        claimed: &[f64],
    ) -> Option<Candidate> {
        let c = self.config;
        let key = note_at(f0, c.reference);
        let lo = f64::from(c.low.max(LOWEST_KEY)) - 0.5;
        let hi = f64::from(c.high.min(HIGHEST_KEY)) + 0.5;
        let apart = 2f64.powf(APART / 12.0);
        if !(lo..=hi).contains(&key) || claimed.iter().any(|g| (f0 / g).max(g / f0) < apart) {
            return None;
        }
        let base = match fundamental {
            Some(p) => self.peaks[p].left,
            None => shoulder,
        };
        if base <= 0.0 {
            return None;
        }
        let top = TOP_HZ.min(self.analysis_rate * 0.45);
        let mut out = Candidate {
            f0,
            score: (f0 + ALPHA) / (f0 + BETA) * self.weight(base),
            fundamental,
            level: base,
            harm: [None; MAX_HARMONICS],
        };
        let (mut sum, mut weights) = match fundamental {
            Some(p) => {
                let w = self.weight(base);
                (self.peaks[p].freq * w, w)
            }
            None => (0.0, 0.0),
        };
        let mut f0 = f0;
        let mut strongest = base;
        let (mut matched, mut room) = (0, 0);
        for m in 1..MAX_HARMONICS {
            let h = (m + 1) as f64;
            let want = h * f0;
            if want > top {
                break;
            }
            if m < 6 {
                room += 1;
            }
            let ratio = 2f64.powf((TOLERANCE + TOLERANCE_PER_HARMONIC * h) / 12.0);
            let Some(p) = self.nearest(want, ratio) else {
                continue;
            };
            let peak = self.peaks[p];
            // There (even when other notes took it all: shared).
            if m < 6 {
                matched += 1;
            }
            if peak.left <= 0.0 {
                continue;
            }
            out.harm[m] = Some(p);
            strongest = strongest.max(peak.left);
            let w = self.weight(peak.left);
            out.score += (f0 + ALPHA) / (h * f0 + BETA) * w;
            if m < 8 {
                // The fundamental follows its harmonics (weighted by how
                // exactly each tells it).
                let w = w * h;
                sum += peak.freq / h * w;
                weights += w;
                f0 = sum / weights;
            }
        }
        out.f0 = f0;
        let fundamental_floor = 10f64.powf(-FUNDAMENTAL_DB / 20.0);
        // An odd harmonic (3rd, 5th, 7th) where there is room for one: a
        // note an octave under another has only the other's.
        let odd_room = 3.0 * f0 <= top;
        let odd = [2, 4, 6]
            .iter()
            .any(|m| out.harm[*m].is_some() || self.present(f0 * (*m + 1) as f64));
        (base >= fundamental_floor * strongest && matched >= room.min(2) && (odd || !odd_room))
            .then_some(out)
    }

    /// A candidate's level as it stands (power of what it would take).
    fn level_of(&self, c: &Candidate) -> f64 {
        let mut power = c.level * c.level / 2.0;
        for p in c.harm.iter().flatten() {
            power += self.peaks[*p].left.powi(2) / 2.0;
        }
        power
    }

    /// The notes of the newest frame, strongest first, into `found`.
    fn find_notes(&mut self) {
        self.found_count = 0;
        if self.peaks.is_empty() {
            return;
        }
        #[cfg(test)]
        if DEBUG.with(|d| d.get()) {
            let peaks: Vec<String> = self
                .peaks
                .iter()
                .map(|p| format!("{:.0}:{:.0}", p.freq, 20.0 * (p.amp / self.loudest).log10()))
                .collect();
            eprintln!("peaks {}", peaks.join(" "));
        }
        let c = self.config;
        let mut first = 0.0;
        let mut loudest_db = 0f32;
        let mut claimed = [0f64; MAX_NOTES];
        let fundamental_floor = 10f64.powf(-FUNDAMENTAL_DB / 20.0);
        let near = 2f64.powf(TOLERANCE / 12.0);
        let mut refused = [0f64; MAX_NOTES];
        let mut refused_count = 0;
        while self.found_count < MAX_NOTES {
            // Found notes and refused shoulders are not candidates again.
            let mut taken_all = [0f64; 2 * MAX_NOTES];
            taken_all[..self.found_count].copy_from_slice(&claimed[..self.found_count]);
            taken_all[self.found_count..self.found_count + refused_count]
                .copy_from_slice(&refused[..refused_count]);
            let taken = &taken_all[..self.found_count + refused_count];
            let mut best: Option<Candidate> = None;
            let keep = |cand: Option<Candidate>, best: &mut Option<Candidate>| {
                if let Some(cand) = cand
                    && best.is_none_or(|b| cand.score > b.score)
                {
                    *best = Some(cand);
                }
            };
            for i in 0..self.peaks.len() {
                let p = self.peaks[i];
                if p.left <= 0.0 {
                    continue;
                }
                keep(self.candidate(p.freq, Some(i), 0.0, taken), &mut best);
                // Or the harmonic of a fundamental that merged into a
                // neighbour's lobe (no peak of its own, but its level).
                if p.freq > SHOULDER_TOP_HZ {
                    continue;
                }
                for div in 2..=4 {
                    let f = p.freq / f64::from(div);
                    if self.nearest(f, near).is_some() {
                        continue;
                    }
                    let level = self.fundamental_at(f);
                    if level >= fundamental_floor * p.left {
                        keep(self.candidate(f, None, level, taken), &mut best);
                    }
                }
            }
            // Sounding notes where they were, peak or not.
            for s in self.slots.iter().filter(|s| s.on) {
                let f = c.reference * 2f64.powf((s.pitch - 69.0) / 12.0);
                let level = self.own_level(f);
                if self.nearest(f, near).is_none() && s.still(level) {
                    keep(self.candidate(f, None, level, taken), &mut best);
                }
            }
            let Some(mut best) = best else {
                break;
            };
            // A note an octave or so down that explains nearly as much is
            // the note (this one its harmonic).
            for div in LOWER {
                let f = best.f0 / div;
                // (A peak, or a sounding note: a shoulder between two notes
                // explains their harmonics as well as any.)
                let pitch = note_at(f, c.reference);
                let sounding = self
                    .slots
                    .iter()
                    .any(|s| s.on && (s.pitch - pitch).abs() < 0.5);
                let cand = match self.nearest(f, near) {
                    Some(j) => self.candidate(self.peaks[j].freq, Some(j), 0.0, taken),
                    None if sounding => self.candidate(f, None, self.fundamental_at(f), taken),
                    None => None,
                };
                // ... with harmonics of its own besides the upper note's
                // (else it is a stray peak under it).
                if let Some(cand) = cand
                    && cand.score >= LOWER_SHARE * best.score
                    && cand
                        .harm
                        .iter()
                        .enumerate()
                        .skip(1)
                        .take(7)
                        .filter(|(m, p)| p.is_some() && (m + 1) % div as usize != 0)
                        .count()
                        >= 2
                {
                    best = cand;
                }
            }
            let pitch = note_at(best.f0, c.reference);
            #[cfg(test)]
            if DEBUG.with(|d| d.get()) {
                let hs: Vec<String> = best
                    .harm
                    .iter()
                    .enumerate()
                    .filter_map(|(m, p)| {
                        p.map(|p| {
                            format!(
                                "{}:{:.0}Hz {:.0}dB",
                                m + 1,
                                self.peaks[p].freq,
                                20.0 * (self.peaks[p].left / self.loudest).max(1e-9).log10(),
                            )
                        })
                    })
                    .collect();
                eprintln!(
                    "  pick {pitch:.2} ({}) score {:.3} first {first:.3}: {}",
                    if best.fundamental.is_some() {
                        "peak"
                    } else {
                        "shoulder"
                    },
                    best.score,
                    hs.join(" ")
                );
            }
            let level_db = (10.0 * self.level_of(&best).max(1e-12).log10()) as f32;
            // On a harmonic of a note found already: a note of its own only
            // when most of its fundamental is still its own and it is
            // loud (else it is that note's harmonic, shared with others).
            let on_harmonic = claimed[..self.found_count].iter().any(|g| {
                let r = best.f0 / g;
                let k = r.round();
                k >= 2.0 && (r / k).max(k / r) <= near
            });
            if on_harmonic {
                let own = best
                    .fundamental
                    .is_some_and(|p| self.peaks[p].left >= HARMONIC_OWN * self.peaks[p].amp);
                if !own || level_db < loudest_db - HARMONIC_LEVEL_DB {
                    match best.fundamental {
                        // Its partials are the other note's: nothing of
                        // them is left for anyone.
                        Some(p) => {
                            self.peaks[p].left = 0.0;
                            for q in best.harm.iter().flatten() {
                                self.peaks[*q].left = 0.0;
                            }
                        }
                        None if refused_count < refused.len() => {
                            refused[refused_count] = best.f0;
                            refused_count += 1;
                        }
                        None => break,
                    }
                    continue;
                }
            }
            if self.found_count == 0 {
                first = best.score;
                loudest_db = level_db;
            } else {
                let held = self
                    .slots
                    .iter()
                    .any(|s| s.on && (pitch - f64::from(s.key)).abs() < 0.5 + HYSTERESIS);
                let share = if held { HELD_SHARE } else { NEW_SHARE };
                if best.score < share * first || (!held && level_db < loudest_db - NEW_LEVEL_DB) {
                    break;
                }
            }
            // Estimate each partial from the quieter neighbour, since the
            // louder neighbour may itself be shared. A gently falling
            // envelope from the fundamental also consumes isolated upper
            // partials; otherwise they can become phantom high notes.
            // A partial above that envelope keeps its excess for another note.
            // (The neighbours as they were: what another note took of
            // them says nothing of this note's spectrum.)
            let mut a = [0.0; MAX_HARMONICS];
            a[0] = best.level;
            for (m, p) in best.harm.iter().enumerate().skip(1) {
                if let Some(p) = p {
                    a[m] = self.peaks[*p].amp;
                }
            }
            let mut power = best.level * best.level / 2.0;
            if let Some(p) = best.fundamental {
                self.peaks[p].left = 0.0;
            }
            for (m, p) in best.harm.iter().enumerate().skip(1) {
                let Some(p) = *p else { continue };
                let prev = a[m - 1];
                let next = a.get(m + 1).copied().unwrap_or(0.0);
                let envelope = prev.min(next).max(best.level / ((m + 1) as f64).powf(0.7));
                let left = self.peaks[p].left;
                let take = if envelope <= 0.0 {
                    left
                } else {
                    left.min(envelope)
                };
                power += take * take / 2.0;
                let peak = &mut self.peaks[p];
                peak.left -= take;
                if peak.left < LEFT_SHARE * peak.amp {
                    peak.left = 0.0;
                }
            }
            claimed[self.found_count] = best.f0;
            self.found[self.found_count] = Found {
                pitch,
                level_db: (10.0 * power.max(1e-12).log10()) as f32,
                fundamental_db: (20.0 * best.level.max(1e-9).log10()) as f32,
            };
            self.found_count += 1;
        }
    }

    /// Follow keys over frames: notes start, change, are played again and
    /// end.
    fn follow(&mut self, now: isize, emit: &mut impl FnMut(isize, isize, VoiceEvent)) {
        let c = self.config;
        let (onset, release) = holds(c.responsiveness);
        let hop = (self.hop * self.decimate) as i64;
        // The window and half of it, in input frames: a sound is heard once
        // it fills about half the window.
        let span = (self.window * self.decimate) as i64;
        let half = span / 2;
        let full = (self.window / self.hop) as u32;
        let abs_now = self.clock - 1;
        let rel = |abs: i64| now - (abs_now - abs) as isize;
        // The attack this window holds, if any.
        let attack = (abs_now - self.last_onset <= span).then_some(self.last_onset);
        for s in &mut self.slots {
            s.seen = false;
        }
        for i in 0..self.found_count {
            let f = self.found[i];
            if f.level_db < c.threshold_db {
                continue;
            }
            let key = self
                .slots
                .iter()
                .find(|s| s.on && (f.pitch - f64::from(s.key)).abs() < 0.5 + HYSTERESIS)
                .map(|s| s.key)
                .unwrap_or_else(|| {
                    nearest_in_scale(
                        f.pitch,
                        c.scale,
                        c.low.max(LOWEST_KEY),
                        c.high.min(HIGHEST_KEY),
                    )
                });
            let slot = match self
                .slots
                .iter()
                .position(|s| s.key == key && (s.on || s.count > 0 || s.seen))
            {
                Some(j) => j,
                None => match self
                    .slots
                    .iter()
                    .position(|s| !s.on && s.count == 0 && !s.seen)
                {
                    Some(j) => {
                        self.slots[j] = Slot {
                            key,
                            first: abs_now,
                            ..Slot::EMPTY
                        };
                        j
                    }
                    None => continue,
                },
            };
            let s = &mut self.slots[slot];
            if s.seen {
                continue;
            }
            s.seen = true;
            if !s.on && s.count == 0 {
                s.first = abs_now;
            }
            s.pitch = f.pitch;
            s.level = f.level_db;
            s.history[s.heard % HISTORY] = f.fundamental_db;
            s.heard += 1;
        }
        // Keys on a harmonic of a sounding note or of one about to start
        // (heard before their own fundamental resolves) hold longer.
        // So do keys with strong energy an octave or a twelfth below that
        // no sounding note explains (a lower note still resolving).
        let mut harmonic = [false; SLOTS];
        for (i, h) in harmonic.iter_mut().enumerate() {
            let slot = self.slots[i];
            if slot.on || !(slot.seen || slot.count > 0) {
                continue;
            }
            let key = i32::from(slot.key);
            let f = c.reference * 2f64.powf((slot.pitch - 69.0) / 12.0);
            let here = self.level_at(f);
            *h = self.slots.iter().any(|o| {
                let d = key - i32::from(o.key);
                ((o.on || o.count > 0 || o.seen) && HARMONIC_INTERVALS.contains(&d))
                    || (o.on && d.abs() == 1)
            }) || [2.0, 3.0].iter().any(|div| {
                let below = f / div;
                let pitch = note_at(below, c.reference);
                below >= 40.0
                    && self.level_at(below) >= 10f64.powf(-LOWER_ENERGY_DB / 20.0) * here
                    && !self
                        .slots
                        .iter()
                        .any(|o| o.on && (o.pitch - pitch).abs() < 0.6)
            });
        }
        for (i, s) in self.slots.iter_mut().enumerate() {
            if s.on {
                s.age += 1;
                if !s.seen {
                    s.missing += 1;
                    if s.missing >= release {
                        let at = (abs_now - i64::from(s.missing - 1) * hop - half).max(s.started);
                        emit(now, rel(at), VoiceEvent::NoteOff { key: s.key });
                        *s = Slot::EMPTY;
                    }
                    continue;
                }
                s.missing = 0;
                // Played again: an attack since it began, and its own
                // fundamental louder than a window ago (a note added on its
                // harmonics leaves the fundamental be).
                let back = full as usize;
                if s.age > full
                    && s.heard > back + 3
                    && let Some(a) = attack
                    && a > s.started
                {
                    // Means of three frames (beating moves single ones).
                    let mean = |from: usize| {
                        (0..3).map(|k| s.history[(from - k) % HISTORY]).sum::<f32>() / 3.0
                    };
                    let newest = mean(s.heard - 1);
                    let before = mean(s.heard - 1 - back);
                    if newest >= before + RETRIGGER_DB {
                        emit(now, rel(a), VoiceEvent::NoteOff { key: s.key });
                        let velocity = velocity_of(s.level, c.threshold_db);
                        emit(
                            now,
                            rel(a),
                            VoiceEvent::NoteOn {
                                key: s.key,
                                velocity,
                            },
                        );
                        s.age = 0;
                        s.started = a;
                        s.last_bend = 0.0;
                    }
                }
                s.since_bend += 1;
                let semitones = (s.pitch - f64::from(s.key)) as f32;
                let moved = (semitones - s.last_bend).abs();
                if moved >= 0.1 || (moved >= 0.02 && s.since_bend >= BEND_FRAMES) {
                    s.last_bend = semitones;
                    s.since_bend = 0;
                    emit(
                        now,
                        rel(abs_now - half),
                        VoiceEvent::Bend {
                            key: s.key,
                            semitones,
                        },
                    );
                }
            } else if s.count > 0 || s.seen {
                if !s.seen {
                    *s = Slot::EMPTY;
                    continue;
                }
                s.count += 1;
                s.loudest = s.loudest.max(s.level);
                // Not while the window holds mostly a fresh attack (its
                // tail alone: partials smeared together).
                if s.count < onset + if harmonic[i] { HARMONIC_HOLD } else { 0 } || self.attack {
                    continue;
                }
                // At the attack it came with, else where half a window
                // first heard it.
                let at = attack
                    .filter(|a| *a >= s.first - span)
                    .unwrap_or(s.first - half);
                let velocity = velocity_of(s.loudest, c.threshold_db);
                emit(
                    now,
                    rel(at),
                    VoiceEvent::NoteOn {
                        key: s.key,
                        velocity,
                    },
                );
                s.on = true;
                s.count = 0;
                s.age = 0;
                s.started = attack.unwrap_or(at).max(at);
                let semitones = (s.pitch - f64::from(s.key)) as f32;
                s.last_bend = 0.0;
                if semitones.abs() >= 0.02 {
                    s.last_bend = semitones;
                    emit(
                        now,
                        rel(at),
                        VoiceEvent::Bend {
                            key: s.key,
                            semitones,
                        },
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
