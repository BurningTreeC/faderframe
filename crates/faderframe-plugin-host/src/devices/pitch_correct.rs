//! Pitch Correct: a live, monophonic pitch corrector (written fresh, in
//! the manner of the classic ones).
//!
//! The input's pitch is found every 2 ms ([`detect`]: YIN with octave
//! guards on a decimated copy, refined at full rate). Each frame the
//! nearest allowed pitch is the target — the key track's key where the
//! voice is (or the chord track's chord, or a key set here, every
//! semitone, the notes ticked here, or the notes a MIDI track plays into
//! the device), as the track's tuning tunes those keys — with hysteresis,
//! so a note is left only once the voice is clearly nearer another.
//!
//! The correction works on the error against the target: its slow part
//! (an 80 ms average, started afresh where the voice starts) is the
//! intonation, the rest is the voice's own movement (vibrato, scoops).
//! The intonation is taken out; the movement is kept by the Vibrato
//! amount (100 %: as sung, 0 %: flattened onto the note, more: wider).
//! The shift glides to what that asks for at the Retune Speed (the time
//! to get 95 % of the way; 0 = at once, the hard-tuned sound), slower on
//! notes held longer with Humanize. Transpose then moves everything by
//! semitones. [`shift`] lays the voice down at the corrected pitch by
//! PSOLA, the formants kept (or moved by Formant, or along with the pitch
//! when Preserve Formants is off). Unvoiced sounds pass as they were.
//!
//! The latency is fixed by the Range (the lowest pitch followed: 2.5 of
//! its periods) and reported. Allocation-free once made.

mod detect;
pub mod shift;

use super::chord::{ROOTS, key_at};
use super::{fixed, on_off, param, pick, stepped};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_midi::MidiEvent;
use faderframe_midi::theory::Scale;
use faderframe_midi::tuning::TuningTable;
use std::sync::Arc;

pub use detect::{Detector, Found};
pub use shift::Shifter;

pub mod id {
    pub const TARGET: u32 = 0;
    pub const ROOT: u32 = 1;
    pub const SCALE: u32 = 2;
    pub const SPEED: u32 = 3;
    pub const HUMANIZE: u32 = 4;
    pub const VIBRATO: u32 = 5;
    pub const TRANSPOSE: u32 = 6;
    pub const PRESERVE_FORMANTS: u32 = 7;
    pub const FORMANT: u32 = 8;
    /// The lowest pitch followed (fixed: it sets the latency).
    pub const RANGE: u32 = 9;
    pub const MIX: u32 = 10;
    /// The Custom notes: C, C♯ … B.
    pub const fn note(pc: u32) -> u32 {
        11 + pc
    }
}

/// Published: the pitch found, the target, the pitch played (MIDI notes,
/// fractional; −1 = none), the correction (semitones), how clear the
/// pitch is (0–1), the pitch classes allowed now (a 12-bit mask) and the
/// level in and out (peaks, linear).
pub mod value {
    pub const INPUT: usize = 0;
    pub const TARGET: usize = 1;
    pub const OUTPUT: usize = 2;
    pub const CORRECTION: usize = 3;
    pub const CLARITY: usize = 4;
    pub const ALLOWED: usize = 5;
    pub const IN_PEAK: usize = 6;
    pub const OUT_PEAK: usize = 7;
}
pub const TAP_VALUES: usize = 8;

/// What the notes are corrected toward.
pub const TARGETS: [&str; 6] = [
    "Key Track",
    "Chord Track",
    "Scale",
    "Chromatic",
    "Custom",
    "MIDI",
];
pub mod target {
    pub const KEY_TRACK: usize = 0;
    pub const CHORD_TRACK: usize = 1;
    pub const SCALE: usize = 2;
    pub const CHROMATIC: usize = 3;
    pub const CUSTOM: usize = 4;
    pub const MIDI: usize = 5;
}

/// The ranges and the lowest pitch each follows (Hz).
pub const RANGES: [(&str, f64); 4] = [
    ("Soprano", 150.0),
    ("Alto / Tenor", 95.0),
    ("Low Male", 62.0),
    ("Instrument", 40.0),
];
/// The highest pitch followed (Hz).
pub const HIGHEST_HZ: f64 = 1_600.0;

/// Note names for the Custom toggles.
pub const NOTE_LABELS: [&str; 12] = [
    "C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B",
];

/// The longest Retune Speed (ms) and how much slower Humanize makes held
/// notes at most (ms), reached after `HELD_SECONDS`.
pub const SLOWEST_MS: f64 = 400.0;
const HUMANIZE_MS: f64 = 400.0;
const HELD_SECONDS: (f64, f64) = (0.15, 0.7);
/// The intonation is the error averaged over this (seconds).
const CENTRE_SECONDS: f64 = 0.08;
/// The pitch target choice follows over this (seconds).
const FOLLOW_SECONDS: f64 = 0.012;
/// A note is left once the voice is this much nearer another (semitones),
/// and this much more with the vibrato kept (its swing must not take the
/// note to the next one).
const HYSTERESIS: f64 = 0.2;
const VIBRATO_HYSTERESIS: f64 = 0.25;
/// Corrections wane over this where nothing is sung (seconds).
const RELEASE_SECONDS: f64 = 0.01;
/// Unvoiced frames before the voice counts as gone (a frame of doubt
/// inside a note keeps it).
const HANGOVER: u32 = 2;
/// Frames of analysis kept (enough for the longest latency).
const HISTORY: usize = 256;

fn ranged(pid: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(pid, name, min, max, default, ParameterUnit::None)
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    let mut out = vec![
        stepped(id::TARGET, "Target", (TARGETS.len() - 1) as f64, 0.0),
        stepped(id::ROOT, "Key", 11.0, 0.0),
        stepped(id::SCALE, "Scale", (Scale::ALL.len() - 1) as f64, 0.0),
        param(
            id::SPEED,
            "Retune Speed",
            0.0,
            SLOWEST_MS,
            40.0,
            Milliseconds,
        ),
        param(id::HUMANIZE, "Humanize", 0.0, 1.0, 0.0, Percent),
        param(id::VIBRATO, "Natural Vibrato", 0.0, 2.0, 1.0, Percent),
        ranged(id::TRANSPOSE, "Transpose", -12.0, 12.0, 0.0),
        stepped(id::PRESERVE_FORMANTS, "Preserve Formants", 1.0, 1.0),
        param(id::FORMANT, "Formant", -12.0, 12.0, 0.0, None),
        fixed(stepped(id::RANGE, "Range", (RANGES.len() - 1) as f64, 1.0)),
        param(id::MIX, "Mix", 0.0, 1.0, 1.0, Percent),
    ];
    for pc in 0..12u32 {
        // C major by default.
        let on = [0, 2, 4, 5, 7, 9, 11].contains(&pc);
        out.push(stepped(
            id::note(pc),
            &format!("Note {}", NOTE_LABELS[pc as usize]),
            1.0,
            if on { 1.0 } else { 0.0 },
        ));
    }
    out
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    let signed = |v: f64, unit: &str| {
        if v.abs() < 0.005 {
            "0".to_string()
        } else {
            format!("{v:+.1}{unit}").replace('-', "−")
        }
    };
    Some(match pid.0 {
        id::TARGET => pick(&TARGETS, v),
        id::ROOT => pick(&ROOTS, v),
        id::SCALE => Scale::ALL[(v.round().max(0.0) as usize).min(Scale::ALL.len() - 1)]
            .name()
            .into(),
        id::SPEED if v < 0.5 => "Instant".into(),
        id::SPEED => format!("{v:.0} ms"),
        id::HUMANIZE | id::VIBRATO | id::MIX => format!("{:.0} %", v * 100.0),
        id::TRANSPOSE if v.round() == 0.0 => "0".into(),
        id::TRANSPOSE => format!("{:+.0} st", v.round()).replace('-', "−"),
        id::PRESERVE_FORMANTS => on_off(v),
        id::FORMANT => signed(v, " st"),
        id::RANGE => RANGES[(v.round().max(0.0) as usize).min(RANGES.len() - 1)]
            .0
            .into(),
        p if (id::note(0)..id::note(12)).contains(&p) => on_off(v),
        _ => return None,
    })
}

/// The lowest pitch followed (Hz).
pub fn lowest_hz(params: &ParamValues) -> f64 {
    let r = (params.get(id::RANGE as usize).round().max(0.0) as usize).min(RANGES.len() - 1);
    RANGES[r].1
}

/// Frames the voice comes out later: 2.5 of the longest period's (and a
/// few for the interpolation).
pub fn latency(params: &ParamValues, rate: f64) -> u32 {
    latency_for(lowest_hz(params), rate) as u32
}

fn latency_for(lowest: f64, rate: f64) -> usize {
    (2.5 * rate / lowest).ceil() as usize + 4
}

/// MIDI note (fractional, A4 = 440 at 69) of a period at a rate.
fn note_of(period: f64, rate: f64) -> f64 {
    69.0 + 12.0 * (rate / period / 440.0).log2()
}

/// The pitch classes a scale of `root` allows (a 12-bit mask).
fn scale_mask(key: faderframe_midi::theory::Key) -> u16 {
    (0..12)
        .filter(|pc| key.contains(*pc))
        .fold(0u16, |m, pc| m | 1 << pc)
}

/// The control side of the correction: target, intonation, glide.
#[derive(Clone, Copy, Debug)]
struct Correction {
    /// The target note now (its key and pitch).
    target: Option<(u8, f64)>,
    /// The followed pitch (for the target's choice).
    followed: f64,
    /// The error's slow part (intonation).
    centre: f64,
    /// The shift applied now (semitones).
    shift: f64,
    /// Seconds since the target note began.
    held: f64,
    voiced: bool,
    /// Unvoiced frames in a row.
    gap: u32,
    /// The last pitch found.
    pitch: f64,
}

impl Default for Correction {
    fn default() -> Self {
        Self {
            target: None,
            followed: 0.0,
            centre: 0.0,
            shift: 0.0,
            held: 0.0,
            voiced: false,
            gap: 0,
            pitch: -1.0,
        }
    }
}

/// One analysis frame kept for the marks laid after it.
#[derive(Clone, Copy, Debug, Default)]
struct Frame {
    at: f64,
    period: f64,
    shift: f64,
}

/// Settings read per block.
#[derive(Clone, Copy)]
struct Settings {
    target: usize,
    speed: f64,
    humanize: f64,
    vibrato: f64,
    transpose: f64,
    preserve: bool,
    formant: f64,
    mix: f32,
}

pub struct PitchCorrectProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    rate: f64,
    detector: Detector,
    shifter: Shifter,
    correction: Correction,
    /// Analysis frames (ring) and how many.
    frames: Box<[Frame; HISTORY]>,
    count: usize,
    /// The newest frame that a mark may use (index into the ring).
    use_from: usize,
    /// Keys held on the MIDI input (any channel).
    held: [u8; 128],
    tuning: TuningTable,
    /// The allowed pitch classes last used (for the editor).
    allowed: u16,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
    out: [Vec<f32>; 2],
    /// Seconds per analysis frame.
    hop: f64,
}

impl PitchCorrectProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let rate = config.sample_rate.max(8_000.0);
        let block = config.max_block_size.max(1) as usize;
        let lowest = lowest_hz(&params);
        let detector = Detector::new(rate, lowest, HIGHEST_HZ);
        let hop = detector.hop_frames() as f64 / rate;
        let shifter = Shifter::new(2, rate, rate / lowest, latency_for(lowest, rate), block);
        let sr = rate as f32;
        Self {
            params,
            tap,
            watching: Watching::new(sr),
            rate,
            detector,
            shifter,
            correction: Correction::default(),
            frames: Box::new([Frame::default(); HISTORY]),
            count: 0,
            use_from: 0,
            held: [0; 128],
            tuning: TuningTable::STANDARD,
            allowed: 0,
            meters: [[MeterTap::new(sr); 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            out: [vec![0.0; block], vec![0.0; block]],
            hop,
        }
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(pid as usize))
    }

    fn settings(&self) -> Settings {
        Settings {
            target: (self.get(id::TARGET).round().max(0.0) as usize).min(TARGETS.len() - 1),
            speed: self.get(id::SPEED).clamp(0.0, SLOWEST_MS) / 1000.0,
            humanize: self.get(id::HUMANIZE).clamp(0.0, 1.0),
            vibrato: self.get(id::VIBRATO).clamp(0.0, 2.0),
            transpose: self.get(id::TRANSPOSE).round().clamp(-12.0, 12.0),
            preserve: self.get(id::PRESERVE_FORMANTS) >= 0.5,
            formant: self.get(id::FORMANT).clamp(-12.0, 12.0),
            mix: self.get(id::MIX).clamp(0.0, 1.0) as f32,
        }
    }

    /// The pitch classes allowed at timeline sample `at` (`None`: every
    /// held MIDI key, by key not class).
    fn mask_at(&self, ctx: &PluginProcessContext<'_>, s: &Settings, at: i64) -> Option<u16> {
        let own = || {
            key_at(
                ctx.harmony,
                at,
                false,
                self.get(id::ROOT),
                self.get(id::SCALE),
            )
        };
        Some(match s.target {
            target::KEY_TRACK => scale_mask(key_at(
                ctx.harmony,
                at,
                true,
                self.get(id::ROOT),
                self.get(id::SCALE),
            )),
            target::CHORD_TRACK => match ctx.harmony.chord_at(at) {
                Some(chord) => chord.pitch_classes().fold(0u16, |m, pc| m | 1 << (pc % 12)),
                None => scale_mask(key_at(
                    ctx.harmony,
                    at,
                    true,
                    self.get(id::ROOT),
                    self.get(id::SCALE),
                )),
            },
            target::SCALE => scale_mask(own()),
            target::CHROMATIC => 0x0FFF,
            target::CUSTOM => (0..12u32)
                .filter(|pc| self.get(id::note(*pc)) >= 0.5)
                .fold(0u16, |m, pc| m | 1 << pc),
            _ => return None,
        })
    }

    /// The allowed key nearest `pitch` (its key and tuned pitch).
    fn nearest(&self, pitch: f64, mask: Option<u16>) -> Option<(u8, f64)> {
        let mut best: Option<(u8, f64)> = None;
        for key in 0..128u8 {
            let ok = match mask {
                Some(m) => m & (1 << (key % 12)) != 0,
                None => self.held[usize::from(key)] > 0,
            };
            if !ok {
                continue;
            }
            let Some(p) = self.tuning.pitch(key) else {
                continue;
            };
            if best.is_none_or(|b| (p - pitch).abs() < (b.1 - pitch).abs()) {
                best = Some((key, p));
            }
        }
        best
    }

    /// Is `key` still allowed?
    fn allows(&self, key: u8, mask: Option<u16>) -> bool {
        self.tuning.is_mapped(key)
            && match mask {
                Some(m) => m & (1 << (key % 12)) != 0,
                None => self.held[usize::from(key)] > 0,
            }
    }

    /// A new analysis frame: the target and the shift there.
    fn analysed(&mut self, found: Found, s: &Settings, mask: Option<u16>) {
        let hop = self.hop;
        let follow = 1.0 - (-hop / FOLLOW_SECONDS).exp();
        let centre_k = 1.0 - (-hop / CENTRE_SECONDS).exp();
        let mut c = self.correction;
        let voiced = if found.period > 0.0 {
            c.gap = 0;
            true
        } else {
            c.gap += 1;
            c.voiced && c.gap < HANGOVER
        };
        let pitch = if found.period > 0.0 {
            Some(note_of(found.period, self.rate))
        } else if voiced {
            // Inside the hangover: the pitch before.
            (c.pitch > 0.0).then_some(c.pitch)
        } else {
            None
        };
        let onset = voiced && !c.voiced;
        c.voiced = voiced;
        let mut wanted = 0.0;
        let mut release = true;
        match pitch {
            Some(p) => {
                c.pitch = p;
                c.followed = if onset {
                    p
                } else {
                    c.followed + follow * (p - c.followed)
                };
                let followed = c.followed;
                let current = c.target;
                let hysteresis = HYSTERESIS + VIBRATO_HYSTERESIS * s.vibrato.min(1.0);
                let best = self.nearest(followed, mask);
                let keep = current.filter(|&(key, at)| {
                    self.allows(key, mask)
                        && best.is_none_or(|b| {
                            (followed - at).abs() < (followed - b.1).abs() + hysteresis
                        })
                });
                let next = keep.or(best);
                c.target = next;
                if let Some((key, n)) = next {
                    let error = p - n;
                    if onset || current.is_none() {
                        c.centre = error;
                        c.held = 0.0;
                    } else {
                        if current.map(|t| t.0) != Some(key) {
                            c.held = 0.0;
                        }
                        c.centre += centre_k * (error - c.centre);
                        c.held += hop;
                    }
                    wanted = -c.centre + (s.vibrato - 1.0) * (error - c.centre);
                    release = false;
                }
            }
            None => c.target = None,
        }
        let time = if release {
            RELEASE_SECONDS
        } else {
            let (a, b) = HELD_SECONDS;
            let x = ((c.held - a) / (b - a)).clamp(0.0, 1.0);
            let held = x * x * (3.0 - 2.0 * x);
            (s.speed + s.humanize * held * HUMANIZE_MS / 1000.0) / 3.0
        };
        let k = if time <= 0.0 {
            1.0
        } else {
            1.0 - (-hop / time).exp()
        };
        c.shift += k * (wanted - c.shift);
        self.correction = c;
        let period = match pitch {
            _ if found.period > 0.0 => found.period,
            Some(p) if voiced => self.rate / (440.0 * ((p - 69.0) / 12.0).exp2()),
            _ => 0.0,
        };
        self.frames[self.count % HISTORY] = Frame {
            at: found.at,
            period,
            shift: c.shift,
        };
        self.count += 1;
        // What the editor shows.
        let shown = |v: Option<f64>| v.map_or(-1.0, |v| v as f32);
        self.tap.set_value(value::INPUT, shown(pitch));
        self.tap
            .set_value(value::TARGET, shown(c.target.map(|t| t.1)));
        self.tap.set_value(
            value::OUTPUT,
            shown(pitch.map(|p| p + c.shift + s.transpose)),
        );
        self.tap.set_value(value::CORRECTION, c.shift as f32);
        self.tap.set_value(value::CLARITY, found.clarity as f32);
    }

    /// The analysis frame nearest input frame `t`.
    fn frame_near(&mut self, t: f64) -> Option<Frame> {
        if self.count == 0 {
            return None;
        }
        let oldest = self.count.saturating_sub(HISTORY);
        let mut i = self.use_from.clamp(oldest, self.count - 1);
        while i + 1 < self.count && self.frames[(i + 1) % HISTORY].at <= t {
            i += 1;
        }
        if i + 1 < self.count {
            let (a, b) = (self.frames[i % HISTORY], self.frames[(i + 1) % HISTORY]);
            if (b.at - t).abs() < (t - a.at).abs() {
                i += 1;
            }
        }
        self.use_from = i.saturating_sub(1).max(oldest);
        Some(self.frames[i % HISTORY])
    }

    /// Lay the marks the input taken so far allows.
    fn lay_marks(&mut self, s: &Settings) {
        while self.shifter.next_mark() < self.shifter.written() as f64 {
            let t = self.shifter.next_mark();
            let f = self.frame_near(t);
            let (period, shift) = match f {
                Some(f) if f.period > 0.0 => (Some(f.period), f.shift),
                _ => (None, 0.0),
            };
            let ratio = ((shift + s.transpose) / 12.0).exp2();
            let mut formant = (s.formant / 12.0).exp2();
            if !s.preserve {
                formant *= ratio;
            }
            self.shifter.lay_mark(period, ratio, formant);
        }
    }

    fn note_event(&mut self, e: &MidiEvent) {
        match *e {
            MidiEvent::NoteOn { key, velocity, .. } if velocity > 0 => {
                let h = &mut self.held[usize::from(key & 0x7F)];
                *h = h.saturating_add(1);
            }
            MidiEvent::NoteOn { key, .. } | MidiEvent::NoteOff { key, .. } => {
                let h = &mut self.held[usize::from(key & 0x7F)];
                *h = h.saturating_sub(1);
            }
            MidiEvent::ControlChange {
                controller: 120 | 123,
                ..
            } => self.held = [0; 128],
            _ => {}
        }
    }
}

impl PluginProcessor for PitchCorrectProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let frames = io.frames;
        let watched = self.watching.check(&self.tap, frames);
        let s = self.settings();
        let n = frames.min(self.scratch[0].len());
        let channels = io.audio_out.first().map_or(0, |o| o.num_channels());
        // The input (mono or stereo), kept for the meters and the editor.
        match io.audio_in.first() {
            Some(input) if input.num_channels() > 0 => {
                let ic = input.num_channels();
                for c in 0..2 {
                    self.scratch[c][..n].copy_from_slice(&input.channel(c.min(ic - 1))[..n]);
                }
            }
            _ => {
                for c in &mut self.scratch {
                    c[..n].fill(0.0);
                }
            }
        }
        let events: &[faderframe_midi::TimedMidiEvent] =
            io.events_in.first().map_or(&[], |e| e.iter().as_slice());
        let mut next_event = 0usize;
        let block_start = ctx.transport.sample_position;
        let mut mask = self.mask_at(ctx, &s, block_start);
        for i in 0..n {
            while let Some(e) = events.get(next_event) {
                if e.sample_offset as usize > i {
                    break;
                }
                self.note_event(&e.event);
                next_event += 1;
            }
            let (l, r) = (self.scratch[0][i], self.scratch[1][i]);
            self.shifter.push(l, r);
            if let Some(found) = self.detector.push(0.5 * (l + r)) {
                if s.target != target::MIDI {
                    mask = self.mask_at(ctx, &s, block_start + i as i64);
                }
                self.analysed(found, &s, mask);
            }
        }
        for e in &events[next_event.min(events.len())..] {
            self.note_event(&e.event);
        }
        self.allowed = match mask {
            Some(m) => m,
            None => (0..128usize)
                .filter(|k| self.held[*k] > 0)
                .fold(0u16, |m, k| m | 1 << (k % 12)),
        };
        self.tap.set_value(value::ALLOWED, f32::from(self.allowed));
        self.lay_marks(&s);
        let from = self.shifter.written() - n;
        {
            let [a, b] = &mut self.out;
            let mut outs: [&mut [f32]; 2] = [&mut a[..n], &mut b[..n]];
            self.shifter.render(from, &mut outs, s.mix);
        }
        // (Channels past the second, on a wider track, pass as they came.)
        super::pass_through(io);
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        for c in 0..channels.min(2) {
            out.channel_mut(c)[..n].copy_from_slice(&self.out[c][..n]);
        }
        let peak = |x: &[f32]| x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        self.tap.raise_value(
            value::IN_PEAK,
            peak(&self.scratch[0][..n]).max(peak(&self.scratch[1][..n])),
        );
        self.tap.raise_value(
            value::OUT_PEAK,
            peak(&self.out[0][..n]).max(peak(&self.out[1][..n])),
        );
        for c in 0..2 {
            for (x, y) in self.scratch[c][..n].iter().zip(&self.out[c][..n]) {
                self.meters[0][c].add(*x);
                self.meters[1][c].add(*y);
            }
            self.meters[0][c].publish(&self.tap.meter_in, c, n);
            self.meters[1][c].publish(&self.tap.meter_out, c, n);
        }
        if watched {
            self.tap
                .input
                .push(&self.scratch[0][..n], &self.scratch[1][..n]);
            self.tap.output.push(&self.out[0][..n], &self.out[1][..n]);
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.detector.reset();
        self.shifter.reset();
        self.correction = Correction::default();
        self.count = 0;
        self.use_from = 0;
        self.held = [0; 128];
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }

    fn set_tuning(&mut self, tuning: &TuningTable) {
        self.tuning = *tuning;
    }
}

#[cfg(test)]
mod tests;
