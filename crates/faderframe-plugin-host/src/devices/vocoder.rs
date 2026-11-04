//! Vocoder: the spectral envelope of one signal (the modulator — a voice)
//! laid on another (the carrier — a synth), band by band.
//!
//! The modulator (the input, or the sidechain) is split by a bank of
//! band passes (two state variable filters in a row each, fourth order)
//! between Low and High, spaced evenly in octaves, in mels or in hertz,
//! each as wide as Bandwidth times its spacing; each band's envelope is
//! followed (rectified, Attack and Release). The carrier goes through the
//! same bank — moved by Formant (semitones: the voice's formants higher or
//! lower) — and each band is turned to the modulator's envelope there:
//! divided by its own envelope with Enhance (the carrier's tilt flattened,
//! every band speaks alike), or by the carrier's overall envelope without
//! (its tilt kept). Width spreads the bands, alternately left and right.
//!
//! The carrier is the built-in synth, noise, the sidechain, or the input
//! (with the voice on the sidechain: a vocoder on a synth's track). The
//! synth plays two detuned oscillators per voice (saw to pulse by Wave,
//! some noise for breath) at the pitches Pitch chooses: a held chord on
//! Root (Chord), the notes the device is played (MIDI: on an instrument
//! track, or from a MIDI track routed there; up to eight), or the
//! modulator's own pitch, followed by the Voice-to-MIDI tracker
//! ([`faderframe_analysis::voice`]) — a robot that sings along in tune —
//! all moved by Transpose, gliding over Glide.
//!
//! Consonants have no pitch for a carrier to take: where the modulator's
//! highs outweigh the rest (the unvoiced detector — Sensitivity sets how
//! readily), its highs (over 3 kHz) pass straight to the output, as loud as
//! Unvoiced says. Mix blends with the input as it came; Output sets the
//! level. No latency.

use super::{on_off, param, pass_through, pick, stepped};
use crate::dsp::env::coeff;
use crate::dsp::filter::{OnePole, Svf};
use crate::dsp::smooth::Smoothed;
use crate::dsp::{Noise, db, flush, gain};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_analysis::voice::{VoiceConfig, VoiceEvent, VoiceTracker};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_midi::MidiEvent;
use std::sync::Arc;

pub mod id {
    /// [`super::CARRIERS`].
    pub const CARRIER: u32 = 0;
    /// [`super::MODULATORS`].
    pub const MODULATOR: u32 = 1;
    /// An index into [`super::BAND_COUNTS`].
    pub const BANDS: u32 = 2;
    /// [`super::SPACINGS`].
    pub const SPACING: u32 = 3;
    pub const LOW: u32 = 4;
    pub const HIGH: u32 = 5;
    /// The bands' width as a multiple of their spacing.
    pub const BANDWIDTH: u32 = 6;
    /// Semitones the carrier's bands move.
    pub const FORMANT: u32 = 7;
    pub const ATTACK: u32 = 8;
    pub const RELEASE: u32 = 9;
    /// How loud the unvoiced highs pass.
    pub const UNVOICED: u32 = 10;
    pub const SENSITIVITY: u32 = 11;
    pub const ENHANCE: u32 = 12;
    pub const WIDTH: u32 = 13;
    pub const MIX: u32 = 14;
    pub const OUTPUT: u32 = 15;
    /// [`super::PITCH_MODES`].
    pub const PITCH: u32 = 16;
    /// The chord's root (a MIDI key).
    pub const ROOT: u32 = 17;
    /// [`super::CHORDS`].
    pub const CHORD: u32 = 18;
    /// Saw (0) to pulse (1).
    pub const WAVE: u32 = 19;
    /// The two oscillators apart (cents).
    pub const DETUNE: u32 = 20;
    pub const GLIDE: u32 = 21;
    /// Semitones (MIDI and Follow).
    pub const TRANSPOSE: u32 = 22;
    /// Breath: noise in the synth.
    pub const NOISE: u32 = 23;
}

/// Published: the input and output peaks (linear), how unvoiced the
/// modulator is now (0–1), the synth's pitch (a MIDI key, fractional; 0
/// silent), its voices, and per band the modulator's level (dB).
pub mod value {
    pub const IN_PEAK: usize = 0;
    pub const OUT_PEAK: usize = 1;
    pub const UNVOICED: usize = 2;
    pub const PITCH: usize = 3;
    pub const VOICES: usize = 4;
    /// Band `k`'s level: `BAND + k`.
    pub const BAND: usize = 5;
}
pub const TAP_VALUES: usize = value::BAND + MAX_BANDS;

pub const CARRIERS: [&str; 4] = ["Synth", "Noise", "Sidechain", "Input"];
pub const MODULATORS: [&str; 2] = ["Input", "Sidechain"];
pub const BAND_COUNTS: [usize; 7] = [8, 12, 16, 20, 24, 32, 40];
pub const MAX_BANDS: usize = 40;
pub const SPACINGS: [&str; 3] = ["Octaves", "Mel", "Linear"];
pub const PITCH_MODES: [&str; 3] = ["Chord", "MIDI", "Follow"];
/// The chords (name, intervals in semitones).
pub const CHORDS: [(&str, &[i32]); 8] = [
    ("Unison", &[0]),
    ("Octaves", &[-12, 0, 12]),
    ("Fifth", &[0, 7, 12]),
    ("Major", &[0, 4, 7, 12]),
    ("Minor", &[0, 3, 7, 12]),
    ("Sus4", &[0, 5, 7, 12]),
    ("Major 7", &[0, 4, 7, 11]),
    ("Minor 7", &[0, 3, 7, 10]),
];
/// Synth voices.
pub const VOICES: usize = 8;
/// The vocoded signal's level against the voice's: a band-limited
/// carrier through the bank comes out about 6 dB under the voice whose
/// envelope it took; half of that is made up.
const WET_GAIN: f64 = 1.412_537_5;

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    let key = |id: u32, name: &str, min: f64, max: f64, default: f64| ParameterInfo {
        stepped: true,
        ..param(id, name, min, max, default, None)
    };
    vec![
        stepped(id::CARRIER, "Carrier", (CARRIERS.len() - 1) as f64, 0.0),
        stepped(
            id::MODULATOR,
            "Modulator",
            (MODULATORS.len() - 1) as f64,
            0.0,
        ),
        stepped(id::BANDS, "Bands", (BAND_COUNTS.len() - 1) as f64, 2.0),
        stepped(id::SPACING, "Spacing", (SPACINGS.len() - 1) as f64, 0.0),
        param(id::LOW, "Low", 50.0, 1_000.0, 100.0, Hertz),
        param(id::HIGH, "High", 2_000.0, 16_000.0, 8_000.0, Hertz),
        param(id::BANDWIDTH, "Bandwidth", 0.25, 2.0, 1.0, None),
        param(id::FORMANT, "Formant", -12.0, 12.0, 0.0, None),
        param(id::ATTACK, "Attack", 0.5, 100.0, 4.0, Milliseconds),
        param(id::RELEASE, "Release", 5.0, 1_000.0, 40.0, Milliseconds),
        param(id::UNVOICED, "Unvoiced", 0.0, 1.0, 0.3, Percent),
        param(id::SENSITIVITY, "Sensitivity", 0.0, 1.0, 0.5, Percent),
        stepped(id::ENHANCE, "Enhance", 1.0, 1.0),
        param(id::WIDTH, "Width", 0.0, 1.0, 0.5, Percent),
        param(id::MIX, "Mix", 0.0, 1.0, 1.0, Percent),
        param(id::OUTPUT, "Output", -24.0, 24.0, 0.0, Decibels),
        stepped(id::PITCH, "Pitch", (PITCH_MODES.len() - 1) as f64, 0.0),
        key(id::ROOT, "Root", 24.0, 96.0, 48.0),
        stepped(id::CHORD, "Chord", (CHORDS.len() - 1) as f64, 4.0),
        param(id::WAVE, "Wave", 0.0, 1.0, 0.25, Percent),
        param(id::DETUNE, "Detune", 0.0, 50.0, 12.0, None),
        param(id::GLIDE, "Glide", 0.0, 500.0, 30.0, Milliseconds),
        key(id::TRANSPOSE, "Transpose", -24.0, 24.0, 0.0),
        param(id::NOISE, "Breath", 0.0, 1.0, 0.05, Percent),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::CARRIER => pick(&CARRIERS, v),
        id::MODULATOR => pick(&MODULATORS, v),
        id::BANDS => format!("{}", band_count(v)),
        id::SPACING => pick(&SPACINGS, v),
        id::PITCH => pick(&PITCH_MODES, v),
        id::CHORD => CHORDS[(v.round().max(0.0) as usize).min(CHORDS.len() - 1)]
            .0
            .into(),
        id::ROOT => super::note_name(v.round() as i32),
        id::ENHANCE => on_off(v),
        id::BANDWIDTH => format!("{v:.2}×"),
        id::FORMANT | id::TRANSPOSE if v.abs() < 0.005 => "0 st".into(),
        id::FORMANT => format!("{v:+.1} st"),
        id::TRANSPOSE => format!("{v:+.0} st"),
        id::DETUNE => format!("{v:.0} ct"),
        id::GLIDE | id::UNVOICED | id::NOISE if v <= 0.0 => "Off".into(),
        _ => return None,
    })
}

/// The bands a Bands setting gives.
pub fn band_count(v: f64) -> usize {
    BAND_COUNTS[(v.round().max(0.0) as usize).min(BAND_COUNTS.len() - 1)]
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

fn mel(f: f64) -> f64 {
    2_595.0 * (1.0 + f / 700.0).log10()
}

fn from_mel(m: f64) -> f64 {
    700.0 * (10f64.powf(m / 2_595.0) - 1.0)
}

/// Band `k` of a bank of `n` between `low` and `high`: its centre (Hz)
/// and its spacing (Hz, its distance to its neighbours).
pub fn band_at(k: usize, n: usize, low: f64, high: f64, spacing: usize) -> (f64, f64) {
    let n = n.clamp(1, MAX_BANDS);
    let (low, high) = (low.max(20.0), high.max(low * 1.5));
    let at = |t: f64| match spacing {
        1 => from_mel(mel(low) + (mel(high) - mel(low)) * t),
        2 => low + (high - low) * t,
        _ => low * (high / low).powf(t),
    };
    let step = if n > 1 { 1.0 / (n - 1) as f64 } else { 1.0 };
    let t = k as f64 * step;
    let width = (at(t + 0.5 * step) - at(t - 0.5 * step)).max(1.0);
    (at(t), width)
}

/// The whole bank ([`band_at`] for each band).
pub fn bands(n: usize, low: f64, high: f64, spacing: usize) -> Vec<(f64, f64)> {
    let n = n.clamp(1, MAX_BANDS);
    (0..n).map(|k| band_at(k, n, low, high, spacing)).collect()
}

/// One band: the modulator's filters and follower, the carrier's filters
/// (per channel) and follower.
#[derive(Clone, Copy, Default)]
struct Band {
    analysis: [Svf; 2],
    synthesis: [[Svf; 2]; 2],
    /// Peak gain of a band pass is 1/k: times k, unity.
    k_analysis: f64,
    k_synthesis: f64,
    env: f64,
    carrier_env: f64,
    /// Left and right gains (Width).
    pan: [f64; 2],
}

/// A synth voice: two oscillators, gliding.
#[derive(Clone, Copy, Default)]
struct Voice {
    /// The key it plays (MIDI), or none.
    key: Option<u8>,
    on: bool,
    /// Its pitch now and where it glides (MIDI keys, fractional).
    pitch: f64,
    target: f64,
    phase: [f64; 2],
    level: f64,
    age: u64,
}

/// A PolyBLEP correction at a discontinuity `t` (phase) apart, `dt` the
/// phase step.
#[inline]
fn blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

/// A band-limited saw and pulse at `phase`, blended by `wave`.
#[inline]
fn oscillator(phase: f64, dt: f64, wave: f64) -> f64 {
    let saw = 2.0 * phase - 1.0 - blep(phase, dt);
    let mut pulse = if phase < 0.5 { 1.0 } else { -1.0 };
    pulse += blep(phase, dt);
    pulse -= blep((phase + 0.5).fract(), dt);
    saw + (pulse - saw) * wave
}

const STEP: usize = 32;

pub struct VocoderProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    bands: [Band; MAX_BANDS],
    n: usize,
    /// What the bank was tuned for (bands, spacing, low, high, width,
    /// formant).
    tuned: Option<(usize, usize, u64, u64, u64, u64)>,
    voices: [Voice; VOICES],
    clock: u64,
    /// The pitch Follow heard last (a key, fractional).
    followed: Option<f64>,
    tracker: VoiceTracker,
    noise: [Noise; 2],
    /// The unvoiced detector: the modulator's highs and all of it (mean
    /// squares), and the gate (smoothed).
    hp: OnePole,
    pass_hp: [[OnePole; 2]; 2],
    highs: f64,
    total: f64,
    unvoiced: f64,
    mix: Smoothed,
    output: Smoothed,
    meters: [[MeterTap; 2]; 2],
    dry: [Vec<f32>; 2],
    modulator: Vec<f32>,
}

impl VocoderProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        let mut p = Self {
            watching: Watching::new(sr as f32),
            sr,
            bands: [Band::default(); MAX_BANDS],
            n: 0,
            tuned: None,
            voices: [Voice::default(); VOICES],
            clock: 0,
            followed: None,
            tracker: VoiceTracker::new(sr, VoiceConfig::default()),
            noise: [Noise::new(0x1234_5678), Noise::new(0x8765_4321)],
            hp: OnePole::new(3_000.0, sr),
            pass_hp: [
                [OnePole::new(3_000.0, sr), OnePole::new(3_000.0, sr)],
                [OnePole::new(3_000.0, sr), OnePole::new(3_000.0, sr)],
            ],
            highs: 0.0,
            total: 0.0,
            unvoiced: 0.0,
            mix: Smoothed::new(1.0, 20.0, sr),
            output: Smoothed::new(1.0, 20.0, sr),
            meters: [[mt; 2]; 2],
            dry: [vec![0.0; block], vec![0.0; block]],
            modulator: vec![0.0; block],
            params,
            tap,
        };
        p.tune();
        p.mix.set(p.get(id::MIX).clamp(0.0, 1.0));
        p.output.set(gain(p.get(id::OUTPUT)));
        p.mix.snap();
        p.output.snap();
        p
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }

    /// Tune the bank to the settings (when they changed).
    fn tune(&mut self) {
        let n = band_count(self.get(id::BANDS));
        let spacing = self.get(id::SPACING).round().max(0.0) as usize;
        let (low, high) = (self.get(id::LOW), self.get(id::HIGH));
        let width = self.get(id::BANDWIDTH).clamp(0.25, 2.0);
        let formant = self.get(id::FORMANT).clamp(-12.0, 12.0);
        let key = (
            n,
            spacing,
            low.to_bits(),
            high.to_bits(),
            width.to_bits(),
            formant.to_bits(),
        );
        if self.tuned == Some(key) {
            return;
        }
        self.tuned = Some(key);
        self.n = n;
        let shift = 2f64.powf(formant / 12.0);
        let top = 0.45 * self.sr;
        for k in 0..n {
            // Tuned on the audio thread (Formant is automatable): nothing
            // allocated.
            let (f, w) = band_at(k, n, low, high, spacing);
            let b = &mut self.bands[k];
            let f = f.min(top);
            let q = (f / (w * width)).clamp(0.5, 60.0);
            for s in &mut b.analysis {
                s.set(f, q, self.sr);
            }
            let fs = (f * shift).clamp(20.0, top);
            for s in b.synthesis.iter_mut().flatten() {
                s.set(fs, q, self.sr);
            }
            b.k_analysis = 1.0 / q;
            b.k_synthesis = 1.0 / q;
        }
    }

    /// Band `k`'s pan: odd bands one way, even the other.
    fn pans(&mut self) {
        let width = self.get(id::WIDTH).clamp(0.0, 1.0);
        for (k, b) in self.bands.iter_mut().enumerate() {
            let side = if k % 2 == 0 { -width } else { width };
            b.pan = [(1.0 - side).min(1.0), (1.0 + side).min(1.0)];
        }
    }

    /// The voices the synth should play now (Chord and Follow; MIDI keeps
    /// the notes it was given).
    fn steer_voices(&mut self, mode: usize) {
        let transpose = self.get(id::TRANSPOSE).round();
        match mode {
            0 => {
                let root = self.get(id::ROOT).round();
                let chord =
                    CHORDS[(self.get(id::CHORD).round().max(0.0) as usize).min(CHORDS.len() - 1)].1;
                for (i, v) in self.voices.iter_mut().enumerate() {
                    match chord.get(i) {
                        Some(iv) => {
                            let pitch = root + f64::from(*iv);
                            if !v.on || v.key.is_some() {
                                v.pitch = pitch;
                            }
                            v.key = None;
                            v.on = true;
                            v.target = pitch;
                        }
                        None => v.on = false,
                    }
                }
            }
            2 => {
                let root = self.get(id::ROOT).round();
                let pitch = self.followed.unwrap_or(root) + transpose;
                for (i, v) in self.voices.iter_mut().enumerate() {
                    if i == 0 {
                        if !v.on {
                            v.pitch = pitch;
                        }
                        v.key = None;
                        v.on = true;
                        v.target = pitch;
                    } else {
                        v.on = false;
                    }
                }
            }
            _ => {
                for v in &mut self.voices {
                    if v.key.is_none() {
                        v.on = false;
                    } else if let Some(k) = v.key {
                        v.target = f64::from(k) + transpose;
                    }
                }
            }
        }
    }

    fn note_on(&mut self, key: u8) {
        self.clock += 1;
        let transpose = self.get(id::TRANSPOSE).round();
        let pitch = f64::from(key) + transpose;
        // A free voice, else the oldest.
        let i = self
            .voices
            .iter()
            .position(|v| !v.on && v.level < 1e-4)
            .or_else(|| self.voices.iter().position(|v| !v.on))
            .unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map_or(0, |(i, _)| i)
            });
        // Legato from the newest sounding voice (Glide).
        let from = self
            .voices
            .iter()
            .filter(|v| v.on)
            .max_by_key(|v| v.age)
            .map_or(pitch, |v| v.pitch);
        let v = &mut self.voices[i];
        v.key = Some(key);
        v.on = true;
        v.pitch = from;
        v.target = pitch;
        v.age = self.clock;
    }

    fn note_off(&mut self, key: u8) {
        for v in &mut self.voices {
            if v.key == Some(key) {
                v.on = false;
            }
        }
    }

    fn handle(&mut self, e: &MidiEvent) {
        match *e {
            MidiEvent::NoteOn { key, velocity, .. } if velocity > 0 => self.note_on(key),
            MidiEvent::NoteOn { key, .. } | MidiEvent::NoteOff { key, .. } => self.note_off(key),
            MidiEvent::ControlChange {
                controller: 120 | 123,
                ..
            } => {
                for v in &mut self.voices {
                    v.on = false;
                }
            }
            _ => {}
        }
    }
}

impl PluginProcessor for VocoderProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        let watched = self.watching.check(&self.tap, frames);
        let side = io.audio_in.get(1).filter(|k| k.num_channels() > 0);
        let midi = io.events_in.first();
        let Some(channels) = pass_through(io) else {
            return ProcessStatus::Continue;
        };
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        if channels == 0 {
            return ProcessStatus::Continue;
        }
        let n_frames = frames.min(self.dry[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.dry[c][..n_frames].copy_from_slice(&src[..n_frames]);
        }
        let side_at = |c: usize, i: usize| -> f64 {
            side.map_or(0.0, |s| {
                f64::from(s.channel(c.min(s.num_channels() - 1))[i])
            })
        };
        let modulator_on_side = self.get(id::MODULATOR) >= 0.5;
        for i in 0..n_frames {
            self.modulator[i] = if modulator_on_side {
                (0.5 * (side_at(0, i) + side_at(1, i))) as f32
            } else {
                0.5 * (self.dry[0][i] + self.dry[1][i])
            };
        }
        let mode = self.get(id::PITCH).round().max(0.0) as usize;
        // Follow: the modulator's pitch, heard as notes and bends.
        if mode == 2 {
            let followed = &mut self.followed;
            let mut key_now: Option<f64> = *followed;
            self.tracker
                .process(&self.modulator[..n_frames], |_, _, e| match e {
                    VoiceEvent::NoteOn { key, .. } => key_now = Some(f64::from(key)),
                    VoiceEvent::Bend { key, semitones } => {
                        key_now = Some(f64::from(key) + f64::from(semitones));
                    }
                    VoiceEvent::NoteOff { .. } => {}
                });
            *followed = key_now;
        }
        let carrier = self.get(id::CARRIER).round().max(0.0) as usize;
        let mut events = ctx.param_events.iter().peekable();
        let mut notes = midi.map(|m| m.iter().peekable());
        let (mut in_peak, mut out_peak) = (0.0f32, 0.0f32);
        let mut at = 0;
        while at < n_frames {
            let end = (at + STEP).min(n_frames);
            while let Some(e) = events.peek() {
                if (e.sample_offset as usize) < end {
                    self.params.apply_event(e.parameter, e.value);
                    events.next();
                } else {
                    break;
                }
            }
            if let Some(notes) = &mut notes {
                while let Some(e) = notes.peek() {
                    if (e.sample_offset as usize) < end {
                        if mode == 1 {
                            self.handle(&e.event);
                        }
                        notes.next();
                    } else {
                        break;
                    }
                }
            }
            self.tune();
            self.pans();
            self.steer_voices(mode);
            self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
            self.output.set(gain(self.get(id::OUTPUT)));
            let sr = self.sr;
            let attack = coeff(self.get(id::ATTACK), sr);
            let release = coeff(self.get(id::RELEASE), sr);
            let enhance = self.get(id::ENHANCE) >= 0.5;
            let wave = self.get(id::WAVE).clamp(0.0, 1.0);
            let breath = self.get(id::NOISE).clamp(0.0, 1.0);
            let detune = self.get(id::DETUNE) / 100.0;
            let glide = {
                let ms = self.get(id::GLIDE);
                if ms <= 0.0 { 1.0 } else { coeff(ms, sr) }
            };
            let pass = self.get(id::UNVOICED).clamp(0.0, 1.0);
            // The highs' share of the energy above which a frame is
            // unvoiced: 0.5 at Sensitivity 0 down to 0.1 at 1.
            let threshold = 0.5 - 0.4 * self.get(id::SENSITIVITY).clamp(0.0, 1.0);
            let level_k = coeff(5.0, sr);
            let gate_k = coeff(3.0, sr);
            let (voice_attack, voice_release) = (coeff(3.0, sr), coeff(60.0, sr));
            let sounding = self.voices.iter().filter(|v| v.on).count().max(1);
            let voice_gain = 1.0 / (sounding as f64).sqrt();
            let n = self.n;
            for i in at..end {
                let x = [f64::from(self.dry[0][i]), f64::from(self.dry[1][i])];
                in_peak = in_peak.max(x[0].abs().max(x[1].abs()) as f32);
                let m = f64::from(self.modulator[i]);
                // The carrier.
                let car = match carrier {
                    1 => [self.noise[0].tick(), self.noise[1].tick()],
                    2 => [side_at(0, i), side_at(1, i)],
                    3 => x,
                    _ => {
                        let mut s = [0.0f64; 2];
                        let mut sounding = 0.0f64;
                        for v in &mut self.voices {
                            v.level += ((if v.on { 1.0 } else { 0.0 }) - v.level)
                                * if v.on { voice_attack } else { voice_release };
                            if v.level < 1e-3 && !v.on {
                                v.level = 0.0;
                                continue;
                            }
                            sounding += v.level;
                            v.pitch += (v.target - v.pitch) * glide;
                            let f = 440.0 * 2f64.powf((v.pitch - 69.0) / 12.0);
                            for (o, ph) in v.phase.iter_mut().enumerate() {
                                let cents = if o == 0 { -0.5 } else { 0.5 } * detune;
                                let dt = (f * 2f64.powf(cents / 12.0) / sr).min(0.45);
                                let y = oscillator(*ph, dt, wave) * v.level * voice_gain;
                                *ph += dt;
                                if *ph >= 1.0 {
                                    *ph -= 1.0;
                                }
                                s[o] += y;
                                s[1 - o] += 0.6 * y;
                            }
                        }
                        // Breath sounds with the voices.
                        let b = [self.noise[0].tick(), self.noise[1].tick()];
                        let air = breath * 0.5 * sounding.min(1.0);
                        [0.6 * s[0] + air * b[0], 0.6 * s[1] + air * b[1]]
                    }
                };
                // The unvoiced detector on the modulator.
                let hi = self.hp.high(m);
                self.highs += (hi * hi - self.highs) * level_k;
                self.total += (m * m - self.total) * level_k;
                let share = self.highs / self.total.max(1e-12);
                let want = if self.total > 1e-9 && share > threshold {
                    1.0
                } else {
                    0.0
                };
                self.unvoiced += (want - self.unvoiced) * gate_k;
                // The bank.
                let mut wet = [0.0f64; 2];
                let mut carrier_power = 0.0;
                for b in &mut self.bands[..n] {
                    let a = b.analysis[0].process(m).band * b.k_analysis;
                    let a = b.analysis[1].process(a).band * b.k_analysis;
                    let r = a.abs();
                    b.env += (r - b.env) * if r > b.env { attack } else { release };
                    let mut y = [0.0; 2];
                    for c in 0..2 {
                        let s = b.synthesis[c][0].process(car[c]).band * b.k_synthesis;
                        y[c] = b.synthesis[c][1].process(s).band * b.k_synthesis;
                    }
                    let cr = 0.5 * (y[0].abs() + y[1].abs());
                    b.carrier_env +=
                        (cr - b.carrier_env) * if cr > b.carrier_env { attack } else { release };
                    carrier_power += b.carrier_env * b.carrier_env;
                    // A carrier fading out is not lifted back (the floor:
                    // −80 dB).
                    let g = b.env / b.carrier_env.max(1e-4);
                    if enhance {
                        wet[0] += y[0] * g * b.pan[0];
                        wet[1] += y[1] * g * b.pan[1];
                    } else {
                        wet[0] += y[0] * b.env * b.pan[0];
                        wet[1] += y[1] * b.env * b.pan[1];
                    }
                }
                if !enhance {
                    // The carrier's overall envelope: its tilt kept.
                    let overall = (carrier_power / n.max(1) as f64).sqrt().max(1e-4);
                    wet[0] /= overall;
                    wet[1] /= overall;
                }
                // The unvoiced highs, straight from the modulator.
                let hiss = if modulator_on_side {
                    [side_at(0, i), side_at(1, i)]
                } else {
                    x
                };
                for c in 0..2 {
                    let h = self.pass_hp[c][0].high(hiss[c]);
                    let h = self.pass_hp[c][1].high(h);
                    wet[c] += h * pass * self.unvoiced;
                }
                let mix = self.mix.tick();
                let out_gain = self.output.tick();
                let y = [
                    (x[0] * (1.0 - mix) + wet[0] * WET_GAIN * mix) * out_gain,
                    (x[1] * (1.0 - mix) + wet[1] * WET_GAIN * mix) * out_gain,
                ];
                out_peak = out_peak.max(y[0].abs().max(y[1].abs()) as f32);
                if channels >= 2 {
                    out.channel_mut(0)[i] = y[0] as f32;
                    out.channel_mut(1)[i] = y[1] as f32;
                } else {
                    out.channel_mut(0)[i] = (0.5 * (y[0] + y[1])) as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        if let Some(notes) = notes {
            for e in notes {
                if mode == 1 {
                    self.handle(&e.event);
                }
            }
        }
        for b in &mut self.bands {
            for s in b
                .analysis
                .iter_mut()
                .chain(b.synthesis.iter_mut().flatten())
            {
                s.flush();
            }
            b.env = flush(b.env);
            b.carrier_env = flush(b.carrier_env);
        }
        self.hp.flush();
        for p in self.pass_hp.iter_mut().flatten() {
            p.flush();
        }
        self.highs = flush(self.highs);
        self.total = flush(self.total);
        self.unvoiced = flush(self.unvoiced);
        for c in 2..channels {
            out.channel_mut(c).fill(0.0);
        }
        // Published values.
        self.tap.raise_value(value::IN_PEAK, in_peak);
        self.tap.raise_value(value::OUT_PEAK, out_peak);
        self.tap.set_value(value::UNVOICED, self.unvoiced as f32);
        let lead = self.voices.iter().filter(|v| v.on).max_by_key(|v| v.age);
        let synth = carrier == 0;
        self.tap.set_value(
            value::PITCH,
            lead.filter(|_| synth).map_or(0.0, |v| v.pitch as f32),
        );
        let voices = if synth {
            self.voices.iter().filter(|v| v.on).count()
        } else {
            0
        };
        self.tap.set_value(value::VOICES, voices as f32);
        for k in 0..MAX_BANDS {
            let level = if k < self.n {
                db(self.bands[k].env) as f32
            } else {
                -150.0
            };
            self.tap.set_value(value::BAND + k, level);
        }
        for c in 0..2 {
            let o = out.channel(c.min(channels - 1));
            for (x, y) in self.dry[c][..n_frames].iter().zip(&o[..n_frames]) {
                self.meters[0][c].add(*x);
                self.meters[1][c].add(*y);
            }
            self.meters[0][c].publish(&self.tap.meter_in, c, n_frames);
            self.meters[1][c].publish(&self.tap.meter_out, c, n_frames);
        }
        if watched {
            self.tap
                .input
                .push(&self.dry[0][..n_frames], &self.dry[1][..n_frames]);
            let r = out.channel(1.min(channels - 1));
            self.tap
                .output
                .push(&out.channel(0)[..n_frames], &r[..n_frames]);
            if let Some(k) = side {
                let r = k.channel(1.min(k.num_channels() - 1));
                self.tap
                    .sidechain
                    .push(&k.channel(0)[..n_frames], &r[..n_frames]);
            }
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        for b in &mut self.bands {
            for s in b
                .analysis
                .iter_mut()
                .chain(b.synthesis.iter_mut().flatten())
            {
                s.reset();
            }
            b.env = 0.0;
            b.carrier_env = 0.0;
        }
        for v in &mut self.voices {
            if v.key.is_some() {
                v.on = false;
            }
            v.level = 0.0;
        }
        self.hp.reset();
        for p in self.pass_hp.iter_mut().flatten() {
            p.reset();
        }
        self.highs = 0.0;
        self.total = 0.0;
        self.unvoiced = 0.0;
        self.followed = None;
        let _ = self.tracker.reset();
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests;
