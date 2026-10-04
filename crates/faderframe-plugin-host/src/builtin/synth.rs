use crate::{ParamValues, PluginProcessContext, PluginProcessor, ProcessConfig, ProcessStatus};
use faderframe_audio_graph::NodeIo;
use faderframe_core::db_to_gain;
use faderframe_midi::{MidiEvent, NoteExpressionKind};
use std::f32::consts::PI;

const VOICES: usize = 16;
/// Filter coefficients are recomputed every this many samples per voice.
const CONTROL_INTERVAL: u32 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    stage: Stage,
    key: u8,
    channel: u8,
    velocity: f32,
    env: f32,
    phase: [f32; 2],
    /// TPT state-variable filter state per oscillator/side.
    ic1: [f32; 2],
    ic2: [f32; 2],
    g: f32,
    k: f32,
    control_counter: u32,
    age: u64,
    /// Released while the sustain pedal was down: keeps sounding until the
    /// pedal goes up.
    pedal_held: bool,
    /// Per-note expression: semitones, linear gain (volume × expression),
    /// pan −1..1, vibrato 0..1, brightness 0..1 (0.5 as played), pressure
    /// 0..1.
    tuning: f32,
    volume: f32,
    expression: f32,
    pan: f32,
    vibrato: f32,
    brightness: f32,
    pressure: f32,
}

impl Voice {
    const IDLE: Voice = Voice {
        stage: Stage::Idle,
        key: 0,
        channel: 0,
        velocity: 0.0,
        env: 0.0,
        phase: [0.0; 2],
        ic1: [0.0; 2],
        ic2: [0.0; 2],
        g: 0.1,
        k: 1.4,
        control_counter: 0,
        age: 0,
        pedal_held: false,
        tuning: 0.0,
        volume: 1.0,
        expression: 1.0,
        pan: 0.0,
        vibrato: 0.0,
        brightness: 0.5,
        pressure: 0.0,
    };

    /// Apply a note expression (plain units, see `NoteExpressionKind`).
    fn express(&mut self, kind: NoteExpressionKind, v: f32) {
        match kind {
            NoteExpressionKind::Volume => self.volume = 10f32.powf(v.min(12.0) / 20.0),
            NoteExpressionKind::Pan => self.pan = v.clamp(-1.0, 1.0),
            NoteExpressionKind::Tuning => self.tuning = v.clamp(-120.0, 120.0),
            NoteExpressionKind::Vibrato => self.vibrato = v.clamp(0.0, 1.0),
            NoteExpressionKind::Expression => self.expression = v.clamp(0.0, 1.0),
            NoteExpressionKind::Brightness => self.brightness = v.clamp(0.0, 1.0),
            NoteExpressionKind::Pressure => self.pressure = v.clamp(0.0, 1.0),
        }
    }
}

/// Default pitch-bend range in semitones (RPN 0 changes it per channel).
const BEND_RANGE: f32 = 2.0;
/// MPE member channels' default bend range.
const MPE_BEND_RANGE: f32 = 48.0;
/// Full mod wheel: vibrato depth in semitones, at this rate.
const VIBRATO_DEPTH: f32 = 0.5;
const VIBRATO_HZ: f32 = 5.5;

/// Per-channel controller state.
#[derive(Clone, Copy, Debug)]
struct ChannelState {
    sustain: bool,
    /// Semitones.
    bend: f32,
    bend_range: f32,
    /// 0..=1.
    modulation: f32,
    /// Channel pressure 0..=1 (louder).
    pressure: f32,
    /// CC 74, 0..=1 (brighter above the middle).
    timbre: f32,
    /// Selected RPN (MSB, LSB); 127/127 = none.
    rpn: (u8, u8),
}

impl ChannelState {
    const REST: ChannelState = ChannelState {
        sustain: false,
        bend: 0.0,
        bend_range: BEND_RANGE,
        modulation: 0.0,
        pressure: 0.0,
        timbre: 0.5,
        rpn: (127, 127),
    };
}

/// Block-rate snapshot of the parameters, in per-sample units.
#[derive(Clone, Copy)]
struct Settings {
    gain: f32,
    cutoff: f32,
    k: f32,
    env_octaves: f32,
    attack_step: f32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
    detune: f32,
}

/// Polyphonic two-oscillator subtractive synth (16 voices) with sustain
/// pedal, pitch bend (±2 semitones; RPN 0 sets the range per channel),
/// MPE (member channels at ±48, pressure louder, CC 74 brighter),
/// per-note expressions (tuning, volume, pan, vibrato, expression,
/// brightness, pressure) and mod-wheel vibrato.
pub struct SynthProcessor {
    params: ParamValues,
    sample_rate: f32,
    voices: [Voice; VOICES],
    counter: u64,
    channels: [ChannelState; 16],
    /// Vibrato LFO phase (0..1).
    lfo: f32,
}

impl SynthProcessor {
    pub fn new(params: ParamValues, config: &ProcessConfig) -> Self {
        Self {
            params,
            sample_rate: config.sample_rate as f32,
            voices: [Voice::IDLE; VOICES],
            counter: 0,
            channels: [ChannelState::REST; 16],
            lfo: 0.0,
        }
    }

    fn settings(&self) -> Settings {
        let sr = self.sample_rate;
        let ms = |v: f32| (v * 0.001 * sr).max(1.0);
        // Exponential segments reach ~-60 dB after the given time.
        let coef = |t: f32| (-6.9 / ms(t)).exp();
        Settings {
            gain: db_to_gain(self.params.get(0)),
            cutoff: self.params.get(1),
            k: 2.0 - 1.9 * self.params.get(2).clamp(0.0, 1.0),
            env_octaves: self.params.get(3) * 6.0,
            attack_step: 1.0 / ms(self.params.get(4)),
            decay_coef: coef(self.params.get(5)),
            sustain: self.params.get(6).clamp(0.0, 1.0),
            release_coef: coef(self.params.get(7)),
            detune: self.params.get(8) / 1200.0,
        }
    }

    fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        self.counter += 1;
        let idx = self
            .voices
            .iter()
            .position(|v| v.stage == Stage::Idle)
            .or_else(|| {
                // Steal the oldest releasing voice, else the oldest voice.
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.stage == Stage::Release)
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
            })
            .unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map_or(0, |(i, _)| i)
            });
        let v = &mut self.voices[idx];
        let retrigger = v.stage != Stage::Idle;
        *v = Voice {
            stage: Stage::Attack,
            key,
            channel,
            velocity: velocity as f32 / 127.0,
            age: self.counter,
            env: if retrigger { v.env } else { 0.0 },
            ..Voice::IDLE
        };
    }

    fn note_off(&mut self, channel: u8, key: u8) {
        let pedal = self.channels[(channel & 15) as usize].sustain;
        for v in &mut self.voices {
            if v.key == key
                && v.channel == channel
                && v.stage != Stage::Idle
                && v.stage != Stage::Release
            {
                if pedal {
                    v.pedal_held = true;
                } else {
                    v.stage = Stage::Release;
                }
            }
        }
    }

    fn set_sustain(&mut self, channel: u8, on: bool) {
        let c = (channel & 15) as usize;
        self.channels[c].sustain = on;
        if !on {
            for v in &mut self.voices {
                if v.channel == channel && v.pedal_held {
                    v.pedal_held = false;
                    if v.stage != Stage::Idle {
                        v.stage = Stage::Release;
                    }
                }
            }
        }
    }

    fn all_off(&mut self) {
        for v in &mut self.voices {
            if v.stage != Stage::Idle {
                v.stage = Stage::Release;
            }
        }
    }

    fn handle(&mut self, event: MidiEvent) {
        match event {
            MidiEvent::NoteOn {
                channel,
                key,
                velocity,
            } => self.note_on(channel, key, velocity),
            MidiEvent::NoteOff { channel, key, .. } => self.note_off(channel, key),
            MidiEvent::ControlChange { controller, .. }
                if controller == MidiEvent::CC_ALL_NOTES_OFF
                    || controller == MidiEvent::CC_ALL_SOUND_OFF =>
            {
                for ch in 0..16 {
                    self.set_sustain(ch, false);
                }
                self.all_off()
            }
            MidiEvent::ControlChange {
                channel,
                controller: 64,
                value,
            } => self.set_sustain(channel, value >= 64),
            MidiEvent::ControlChange {
                channel,
                controller: 1,
                value,
            } => self.channels[(channel & 15) as usize].modulation = value as f32 / 127.0,
            MidiEvent::PitchBend { channel, value } => {
                let c = &mut self.channels[(channel & 15) as usize];
                c.bend = (value as f32 - 8192.0) / 8192.0 * c.bend_range;
            }
            MidiEvent::ChannelPressure { channel, pressure } => {
                self.channels[(channel & 15) as usize].pressure = pressure as f32 / 127.0;
            }
            MidiEvent::ControlChange {
                channel,
                controller: 74,
                value,
            } => self.channels[(channel & 15) as usize].timbre = value as f32 / 127.0,
            MidiEvent::ControlChange {
                channel,
                controller: 101,
                value,
            } => self.channels[(channel & 15) as usize].rpn.0 = value,
            MidiEvent::ControlChange {
                channel,
                controller: 100,
                value,
            } => self.channels[(channel & 15) as usize].rpn.1 = value,
            MidiEvent::ControlChange {
                channel,
                controller: 6,
                value,
            } => self.data_entry(channel & 15, value),
            MidiEvent::NoteExpression {
                channel,
                key,
                kind,
                value,
            } => {
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.stage != Stage::Idle && v.channel == channel && v.key == key)
                {
                    v.express(kind, value.get() as f32);
                }
            }
            _ => {}
        }
    }

    /// RPN data entry: 0 = pitch bend range, 6 = MPE configuration (on the
    /// master channel: member channels get the MPE default range).
    fn data_entry(&mut self, channel: u8, value: u8) {
        match self.channels[channel as usize].rpn {
            (0, 0) => self.channels[channel as usize].bend_range = value.max(1) as f32,
            (0, 6) if channel == 0 => {
                for ch in 1..=(value.min(15) as usize) {
                    self.channels[ch].bend_range = MPE_BEND_RANGE;
                }
            }
            _ => {}
        }
    }

    /// Render `[start, end)` additively into `out_l`/`out_r`.
    fn render(
        &mut self,
        s: &Settings,
        out_l: &mut [f32],
        mut out_r: Option<&mut [f32]>,
        start: usize,
        end: usize,
    ) {
        let sr = self.sample_rate;
        let nyquist_guard = sr * 0.45;
        // Vibrato at the segment's start (segments are at most a block).
        let lfo = (self.lfo * 2.0 * PI).sin();
        self.lfo = (self.lfo + (end - start) as f32 * VIBRATO_HZ / sr).fract();
        let channels = self.channels;
        for v in self.voices.iter_mut().filter(|v| v.stage != Stage::Idle) {
            let ch = channels[(v.channel & 15) as usize];
            let semis = v.key as f32 - 69.0
                + ch.bend
                + v.tuning
                + lfo * (ch.modulation + v.vibrato) * VIBRATO_DEPTH;
            // Balance: the far side fades as the note pans.
            let (pan_l, pan_r) = ((1.0 - v.pan).min(1.0), (1.0 + v.pan).min(1.0));
            let base = 440.0 * 2f32.powf(semis / 12.0);
            let freqs = [base * (1.0 - s.detune), base * (1.0 + s.detune)];
            let dts = [freqs[0] / sr, freqs[1] / sr];
            for i in start..end {
                // Envelope.
                match v.stage {
                    Stage::Attack => {
                        v.env += s.attack_step;
                        if v.env >= 1.0 {
                            v.env = 1.0;
                            v.stage = Stage::Decay;
                        }
                    }
                    Stage::Decay => {
                        v.env = s.sustain + (v.env - s.sustain) * s.decay_coef;
                        if (v.env - s.sustain).abs() < 1e-4 {
                            v.stage = Stage::Sustain;
                        }
                    }
                    Stage::Sustain => v.env = s.sustain,
                    Stage::Release => {
                        v.env *= s.release_coef;
                        if v.env < 1e-4 {
                            v.stage = Stage::Idle;
                            v.env = 0.0;
                        }
                    }
                    Stage::Idle => {}
                }
                if v.stage == Stage::Idle {
                    break;
                }
                // Filter coefficients at control rate (cutoff follows env).
                if v.control_counter == 0 {
                    let fc = (s.cutoff
                        * 2f32.powf(
                            s.env_octaves * v.env * v.velocity
                                + (ch.timbre - 0.5 + v.brightness - 0.5) * 4.0,
                        ))
                    .clamp(20.0, nyquist_guard);
                    v.g = (PI * fc / sr).tan();
                    v.k = s.k;
                }
                v.control_counter = (v.control_counter + 1) % CONTROL_INTERVAL;
                let a1 = 1.0 / (1.0 + v.g * (v.g + v.k));
                let a2 = v.g * a1;
                let a3 = v.g * a2;
                let amp = v.env
                    * v.velocity
                    * s.gain
                    * 0.35
                    * (1.0 + 0.5 * (ch.pressure + v.pressure))
                    * v.volume
                    * v.expression;
                let mut side = [0.0f32; 2];
                for o in 0..2 {
                    // PolyBLEP sawtooth.
                    let t = v.phase[o];
                    let dt = dts[o];
                    let mut saw = 2.0 * t - 1.0;
                    if t < dt {
                        let x = t / dt;
                        saw -= x + x - x * x - 1.0;
                    } else if t > 1.0 - dt {
                        let x = (t - 1.0) / dt;
                        saw -= x * x + x + x + 1.0;
                    }
                    v.phase[o] += dt;
                    if v.phase[o] >= 1.0 {
                        v.phase[o] -= 1.0;
                    }
                    // TPT SVF low-pass.
                    let v3 = saw - v.ic2[o];
                    let v1 = a1 * v.ic1[o] + a2 * v3;
                    let v2 = v.ic2[o] + a2 * v.ic1[o] + a3 * v3;
                    v.ic1[o] = 2.0 * v1 - v.ic1[o];
                    v.ic2[o] = 2.0 * v2 - v.ic2[o];
                    side[o] = v2 * amp;
                }
                match out_r.as_deref_mut() {
                    // Oscillators spread slightly left/right for width.
                    Some(r) => {
                        out_l[i] += (side[0] * 0.8 + side[1] * 0.2) * pan_l;
                        r[i] += (side[1] * 0.8 + side[0] * 0.2) * pan_r;
                    }
                    None => out_l[i] += 0.5 * (side[0] + side[1]),
                }
            }
        }
    }
}

impl PluginProcessor for SynthProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let settings = self.settings();
        let frames = io.frames;
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        out.clear();
        let events = io.events_in.first();
        let stereo = out.num_channels() >= 2;
        let mut pos = 0usize;
        if let Some(events) = events {
            for e in events.iter() {
                let at = (e.sample_offset as usize).min(frames);
                if at > pos {
                    if stereo {
                        let (l, r) = out.channel_pair_mut(0, 1);
                        self.render(&settings, l, Some(r), pos, at);
                    } else {
                        self.render(&settings, out.channel_mut(0), None, pos, at);
                    }
                    pos = at;
                }
                self.handle(e.event);
            }
        }
        if frames > pos {
            if stereo {
                let (l, r) = out.channel_pair_mut(0, 1);
                self.render(&settings, l, Some(r), pos, frames);
            } else {
                self.render(&settings, out.channel_mut(0), None, pos, frames);
            }
        }
        if self.voices.iter().all(|v| v.stage == Stage::Idle) {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; VOICES];
        self.channels = [ChannelState::REST; 16];
    }
}
