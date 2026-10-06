//! Note Echo: every note repeated, softer each time.
//!
//! The repeats come a division of the song's tempo apart (or a time in
//! milliseconds), up to sixteen of them, each the feedback's share of the
//! one before as loud (until too soft to play), and each optionally moved
//! by some semitones (rising or falling echoes). The played note itself
//! can be left out. Each repeat lasts as long as the played note did.

use super::midi_fx::{self, Schedule, Sounding};
use super::{on_off, param, stepped};
use crate::dsp::lfo::{DIVISIONS, division_name};
use crate::tap::AnalysisTap;
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_midi::{MidiBuffer, MidiEvent};
use std::sync::Arc;

pub mod id {
    pub const SYNC: u32 = 0;
    pub const DIVISION: u32 = 1;
    pub const TIME: u32 = 2;
    pub const REPEATS: u32 = 3;
    pub const FEEDBACK: u32 = 4;
    pub const PITCH: u32 = 5;
    pub const DRY: u32 = 6;
}

/// Published: the repeats sounding now, and the delay (ms).
pub mod value {
    pub const ECHOES: usize = 0;
    pub const DELAY_MS: usize = 1;
}
pub const TAP_VALUES: usize = 2;

/// 1/8.
const DEFAULT_DIVISION: f64 = 8.0;

fn ranged(pid: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(pid, name, min, max, default, ParameterUnit::None)
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        stepped(id::SYNC, "Sync", 1.0, 1.0),
        stepped(
            id::DIVISION,
            "Division",
            (DIVISIONS.len() - 1) as f64,
            DEFAULT_DIVISION,
        ),
        param(id::TIME, "Time", 10.0, 2_000.0, 250.0, Milliseconds),
        ranged(id::REPEATS, "Repeats", 1.0, 16.0, 3.0),
        param(id::FEEDBACK, "Feedback", 0.1, 1.0, 0.7, Percent),
        ranged(id::PITCH, "Pitch", -24.0, 24.0, 0.0),
        stepped(id::DRY, "Played Note", 1.0, 1.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::SYNC | id::DRY => on_off(v),
        id::DIVISION => division_name(v.round().max(0.0) as usize).into(),
        id::REPEATS => format!("{:.0}", v.round()),
        id::PITCH if v.round() == 0.0 => "0 st".into(),
        id::PITCH => format!("{:+.0} st", v.round()).replace('-', "−"),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

/// The repeats of one played note, as they were set when it started.
#[derive(Clone, Copy, Default)]
struct Echoes {
    delay: u64,
    /// Repeat k plays when bit k of the mask is set (loud enough, in range).
    mask: u32,
    pitch: i8,
}

pub struct NoteEchoProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    sr: f64,
    echoes: Box<[Echoes; 2048]>,
    due: Schedule,
    sounding: Sounding,
    clock: u64,
    panic: bool,
}

impl NoteEchoProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        Self {
            params,
            tap,
            sr: config.sample_rate.max(1.0),
            echoes: Box::new([Echoes::default(); 2048]),
            due: Schedule::new(4096),
            sounding: Sounding::default(),
            clock: 0,
            panic: false,
        }
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(pid as usize))
    }

    /// The delay between repeats now (samples).
    fn delay(&self, ctx: &PluginProcessContext<'_>) -> u64 {
        let d = if self.get(id::SYNC) >= 0.5 {
            let i = (self.get(id::DIVISION).round().max(0.0) as usize).min(DIVISIONS.len() - 1);
            DIVISIONS[i].1 * midi_fx::samples_per_quarter(ctx.transport, self.sr)
        } else {
            self.get(id::TIME) * 0.001 * self.sr
        };
        (d.round() as u64).max(1)
    }

    fn advance(&mut self, out: &mut MidiBuffer, from: usize, to: usize) {
        while let Some(p) = self.due.pop_due(self.clock + to as u64) {
            let offset = p.at.saturating_sub(self.clock).max(from as u64) as u32;
            midi_fx::play(&mut self.sounding, out, offset, p.event);
        }
    }
}

impl PluginProcessor for NoteEchoProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let frames = io.frames;
        let Some(out) = io.events_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        out.clear();
        if self.panic {
            self.sounding.all_off(out, 0);
            self.due.clear();
            self.panic = false;
        }
        let dry = self.get(id::DRY) >= 0.5;
        let mut cursor = 0;
        if let Some(input) = io.events_in.first() {
            for e in input.iter() {
                let at = (e.sample_offset as usize).min(frames);
                self.advance(out, cursor, at);
                cursor = at;
                let now = self.clock + at as u64;
                match midi_fx::note(&e.event) {
                    Some((true, channel, key, velocity)) => {
                        if dry {
                            self.sounding.on(out, at as u32, channel, key, velocity);
                        }
                        let delay = self.delay(ctx);
                        let repeats = self.get(id::REPEATS).round().clamp(1.0, 16.0) as u32;
                        let feedback = self.get(id::FEEDBACK).clamp(0.0, 1.0);
                        let pitch = self.get(id::PITCH).round().clamp(-24.0, 24.0) as i8;
                        let src = midi_fx::source(channel, key);
                        let mut mask = 0u32;
                        for k in 1..=repeats {
                            let v = f64::from(velocity) * feedback.powi(k as i32);
                            if v < 1.0 {
                                break;
                            }
                            let key = i32::from(key) + k as i32 * i32::from(pitch);
                            if !(0..=127).contains(&key) {
                                continue;
                            }
                            let on = MidiEvent::NoteOn {
                                channel,
                                key: key as u8,
                                velocity: v.round() as u8,
                            };
                            if self.due.push(now + u64::from(k) * delay, on, src) {
                                mask |= 1 << k;
                            }
                        }
                        self.echoes[usize::from(src)] = Echoes { delay, mask, pitch };
                    }
                    Some((false, channel, key, _)) => {
                        if dry {
                            self.sounding.off(out, at as u32, channel, key);
                        }
                        let src = midi_fx::source(channel, key);
                        let echoes = std::mem::take(&mut self.echoes[usize::from(src)]);
                        for k in 1..32u32 {
                            if echoes.mask & (1 << k) == 0 {
                                continue;
                            }
                            let key = i32::from(key) + k as i32 * i32::from(echoes.pitch);
                            let off = MidiEvent::NoteOff {
                                channel,
                                key: key as u8,
                                velocity: 0,
                            };
                            if !self.due.push(now + u64::from(k) * echoes.delay, off, src) {
                                // No room: end it with the note (it may
                                // not have started; then nothing happens).
                                self.sounding.off(out, at as u32, channel, key as u8);
                            }
                        }
                    }
                    None => midi_fx::pass(out, input, e),
                }
            }
        }
        self.advance(out, cursor, frames);
        self.tap.set_value(value::ECHOES, self.due.len() as f32);
        self.tap.set_value(
            value::DELAY_MS,
            (self.delay(ctx) as f64 / self.sr * 1000.0) as f32,
        );
        self.clock += frames as u64;
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.panic = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NO_HARMONY;
    use crate::devices::midi_fx::rig::{self, balanced, off, on, ons};

    fn echo(set: &[(u32, f64)]) -> NoteEchoProcessor {
        let params = rig::params(parameters(), set);
        let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
        NoteEchoProcessor::new(params, tap, &rig::config())
    }

    #[test]
    fn repeats_come_an_eighth_apart_and_fade() {
        // An eighth at 120 BPM: 12 000 samples; feedback 50 %.
        let mut e = echo(&[(id::FEEDBACK, 0.5)]);
        let got = rig::run(
            &mut e,
            &[on(0, 60, 100), off(3000, 60)],
            60_000,
            120.0,
            true,
            &NO_HARMONY,
        );
        assert_eq!(
            ons(&got),
            [
                (0, 60, 100),
                (12_000, 60, 50),
                (24_000, 60, 25),
                (36_000, 60, 13)
            ]
        );
        // Each as long as the played note.
        let ends: Vec<u64> = got
            .iter()
            .filter(|(_, e)| matches!(e, MidiEvent::NoteOff { .. }))
            .map(|(t, _)| *t)
            .collect();
        assert_eq!(ends, [3000, 15_000, 27_000, 39_000]);
        assert!(balanced(&got));
    }

    #[test]
    fn rising_echoes_in_milliseconds_without_the_played_note() {
        let set = [
            (id::SYNC, 0.0),
            (id::TIME, 100.0),
            (id::PITCH, 12.0),
            (id::REPEATS, 4.0),
            (id::FEEDBACK, 1.0),
            (id::DRY, 0.0),
        ];
        let mut e = echo(&set);
        let got = rig::run(
            &mut e,
            &[on(0, 100, 90), off(1000, 100)],
            30_000,
            120.0,
            false,
            &NO_HARMONY,
        );
        // 100 ms = 4800 samples; 124 and up are past the top.
        assert_eq!(ons(&got), [(4800, 112, 90), (9600, 124, 90)]);
        assert!(balanced(&got));
    }
}
