//! Drum sampler: sixteen pads on consecutive notes (from C1 by default),
//! each its own sample with level, pan, tuning, attack, decay, start,
//! choke group, direction, one-shot or gated play, a low pass and
//! velocity. Hitting a pad again plays it again (four layers a pad, the
//! oldest stolen); a pad in a choke group silences the others in it (the
//! open hi-hat stops when the closed one plays).

use super::keep_length::{KeepLength, Render};
use super::sampler::read;
use super::samples::{Sample, Shared};
use super::{on_off, param, pick, stepped};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ParameterId, db_to_gain};
use faderframe_midi::MidiEvent;
use std::f32::consts::PI;
use std::sync::Arc;

pub const PADS: usize = 16;
/// Parameters per pad.
const FIELDS: usize = 13;
const LAYERS: usize = 4;

pub mod id {
    pub const VOLUME: u32 = 0;
    pub const BASE_NOTE: u32 = 1;
    /// A pad's parameter: `pad(p) + field`.
    pub const fn pad(p: usize) -> u32 {
        100 + 16 * p as u32
    }
    pub const LEVEL: u32 = 0;
    pub const PAN: u32 = 1;
    pub const TUNE: u32 = 2;
    pub const DECAY: u32 = 3;
    pub const ATTACK: u32 = 4;
    pub const START: u32 = 5;
    pub const CHOKE: u32 = 6;
    pub const REVERSE: u32 = 7;
    pub const MODE: u32 = 8;
    pub const CUTOFF: u32 = 9;
    pub const RESONANCE: u32 = 10;
    pub const VELOCITY: u32 = 11;
    /// The pad's tune moves its pitch, not its length.
    pub const KEEP: u32 = 12;
}

/// Published: per pad how hard it sounds now (0–1), then voices sounding.
pub mod value {
    pub const fn pad(p: usize) -> usize {
        p
    }
    pub const VOICES: usize = 16;
    pub const OUT_PEAK: usize = 17;
}
pub const TAP_VALUES: usize = 18;

pub const MODES: [&str; 2] = ["One-shot", "Gate"];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    let mut v = vec![
        param(id::VOLUME, "Volume", -48.0, 12.0, -3.0, Decibels),
        ParameterInfo {
            stepped: true,
            ..param(id::BASE_NOTE, "First Note", 0.0, 112.0, 36.0, None)
        },
    ];
    for p in 0..PADS {
        let b = id::pad(p);
        let n = |s: &str| format!("Pad {} {s}", p + 1);
        v.extend([
            param(b + id::LEVEL, &n("Level"), -48.0, 12.0, 0.0, Decibels),
            param(b + id::PAN, &n("Pan"), -1.0, 1.0, 0.0, None),
            param(b + id::TUNE, &n("Tune"), -24.0, 24.0, 0.0, None),
            param(
                b + id::DECAY,
                &n("Decay"),
                5.0,
                10_000.0,
                10_000.0,
                Milliseconds,
            ),
            param(b + id::ATTACK, &n("Attack"), 0.0, 200.0, 0.0, Milliseconds),
            param(b + id::START, &n("Start"), 0.0, 0.95, 0.0, Percent),
            stepped(b + id::CHOKE, &n("Choke Group"), 8.0, 0.0),
            stepped(b + id::REVERSE, &n("Reverse"), 1.0, 0.0),
            stepped(b + id::MODE, &n("Mode"), 1.0, 0.0),
            param(
                b + id::CUTOFF,
                &n("Cutoff"),
                20.0,
                20_000.0,
                20_000.0,
                Hertz,
            ),
            param(b + id::RESONANCE, &n("Resonance"), 0.0, 1.0, 0.1, Percent),
            param(b + id::VELOCITY, &n("Velocity"), 0.0, 1.0, 0.8, Percent),
            stepped(b + id::KEEP, &n("Keep Length"), 1.0, 0.0),
        ]);
    }
    v
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    if pid.0 == id::BASE_NOTE {
        return Some(super::note_name(v.round() as i32));
    }
    if pid.0 < id::pad(0) {
        return None;
    }
    Some(match (pid.0 - id::pad(0)) % 16 {
        id::MODE => pick(&MODES, v),
        id::REVERSE | id::KEEP => on_off(v),
        id::CHOKE if v < 0.5 => "None".into(),
        id::CHOKE => format!("{:.0}", v.round()),
        id::TUNE => format!("{v:+.2} st").replace("+0.00 st", "0 st"),
        id::DECAY if v >= 9_990.0 => "Full".into(),
        id::CUTOFF if v >= 19_500.0 => "Open".into(),
        id::PAN if v.abs() < 0.005 => "C".into(),
        id::PAN if v < 0.0 => format!("L {:.0}", -v * 100.0),
        id::PAN => format!("R {:.0}", v * 100.0),
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    on: bool,
    pad: usize,
    sample: usize,
    pos: f64,
    step: f64,
    reverse: bool,
    env: f32,
    attack_step: f32,
    rising: bool,
    decay: f32,
    /// Releasing (gated pads let go, chokes): a fast fade.
    fading: bool,
    gain: [f32; 2],
    svf: [(f32, f32); 2],
    coefs: (f32, f32, f32),
    filtering: bool,
    age: u64,
    /// Keep Length: the stretcher it plays through and its pitch factor;
    /// its sample has all been fed.
    slot: Option<u8>,
    pitch: f32,
    fed_out: bool,
}

impl Voice {
    const IDLE: Voice = Voice {
        on: false,
        pad: 0,
        sample: 0,
        pos: 0.0,
        step: 1.0,
        reverse: false,
        env: 0.0,
        attack_step: 1.0,
        rising: false,
        decay: 1.0,
        fading: false,
        gain: [1.0; 2],
        svf: [(0.0, 0.0); 2],
        coefs: (0.0, 0.0, 0.0),
        filtering: false,
        age: 0,
        slot: None,
        pitch: 1.0,
        fed_out: false,
    };
}

pub struct DrumsProcessor {
    params: ParamValues,
    tap: Option<Arc<AnalysisTap>>,
    watching: Watching,
    shared: Shared,
    sr: f64,
    voices: [Voice; PADS * LAYERS],
    counter: u64,
    generation: u64,
    fade: f32,
    hits: [f32; PADS],
    mix: [Vec<f32>; 2],
    meters: [MeterTap; 2],
    /// Stretchers for pads that keep their length (made when one does).
    keep: Option<KeepLength>,
}

/// Does any pad keep its length (its processor needs stretchers)?
pub fn keeps_length(params: &ParamValues) -> bool {
    (0..PADS).any(|p| params.get(2 + p * FIELDS + id::KEEP as usize) >= 0.5)
}

/// A voice's sample at its position, and on by its step: the frame and
/// whether that was the sample's end.
fn next_frame(v: &mut Voice, s: &Sample) -> ([f32; 2], bool) {
    let y = [read(s, 0, v.pos), read(s, usize::from(s.stereo), v.pos)];
    let ended = if v.reverse {
        v.pos -= v.step;
        v.pos < 0.0
    } else {
        v.pos += v.step;
        v.pos >= s.frames as f64
    };
    (y, ended)
}

/// A Keep Length voice's sample at its own speed into `a`/`b`: the frames
/// it had (fewer once it ended).
fn feed_voice(v: &mut Voice, s: &Sample, a: &mut [f32], b: &mut [f32]) -> usize {
    if v.fed_out {
        return 0;
    }
    for k in 0..a.len().min(b.len()) {
        let (y, ended) = next_frame(v, s);
        a[k] = y[0];
        b[k] = y[1];
        if ended {
            v.fed_out = true;
            return k + 1;
        }
    }
    a.len().min(b.len())
}

impl DrumsProcessor {
    pub fn new(
        params: ParamValues,
        tap: Option<Arc<AnalysisTap>>,
        config: &ProcessConfig,
        shared: Shared,
    ) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        Self {
            keep: keeps_length(&params)
                .then(|| KeepLength::new(sr, block))
                .flatten(),
            params,
            tap,
            watching: Watching::new(sr as f32),
            shared,
            sr,
            voices: [Voice::IDLE; PADS * LAYERS],
            counter: 0,
            generation: 0,
            fade: (-6.9 / (0.004 * sr) as f32).exp(),
            hits: [0.0; PADS],
            mix: [vec![0.0; block], vec![0.0; block]],
            meters: [MeterTap::new(sr as f32); 2],
        }
    }

    /// A pad's parameter (they follow the two globals, twelve a pad).
    fn pad(&self, p: usize, field: u32) -> f64 {
        f64::from(self.params.get(2 + p * FIELDS + field as usize))
    }

    fn hit(&mut self, set: &super::samples::SampleSet, pad: usize, velocity: u8) {
        let Some(Some(sample)) = set.slots.get(pad).copied() else {
            return;
        };
        let Some(s) = set.samples.get(sample) else {
            return;
        };
        let sr = self.sr;
        let choke = self.pad(pad, id::CHOKE).round() as i64;
        if choke > 0 {
            for p in 0..PADS {
                if p != pad && self.pad(p, id::CHOKE).round() as i64 == choke {
                    for v in self.voices.iter_mut().filter(|v| v.on && v.pad == p) {
                        v.fading = true;
                    }
                }
            }
        }
        // A free layer of this pad, or its oldest.
        let layers = pad * LAYERS..(pad + 1) * LAYERS;
        let idx = layers
            .clone()
            .find(|&i| !self.voices[i].on)
            .unwrap_or_else(|| {
                layers
                    .min_by_key(|&i| self.voices[i].age)
                    .unwrap_or(pad * LAYERS)
            });
        let reverse = self.pad(pad, id::REVERSE) >= 0.5;
        let frames = s.frames as f64;
        let start = self.pad(pad, id::START).clamp(0.0, 0.95) * frames;
        let vel = f64::from(velocity) / 127.0;
        let vs = self.pad(pad, id::VELOCITY);
        let level = db_to_gain(
            (self.pad(pad, id::LEVEL) + f64::from(self.params.get(id::VOLUME as usize))) as f32,
        ) * (1.0 - vs + vs * vel * vel) as f32;
        let pan = self.pad(pad, id::PAN).clamp(-1.0, 1.0) as f32;
        let attack = self.pad(pad, id::ATTACK);
        let decay = self.pad(pad, id::DECAY);
        let cutoff = self.pad(pad, id::CUTOFF) as f32;
        let k = 2.0 - 1.9 * self.pad(pad, id::RESONANCE).clamp(0.0, 1.0) as f32;
        let g = (PI * cutoff.min(sr as f32 * 0.45) / sr as f32).tan();
        let a1 = 1.0 / (1.0 + g * (g + k));
        let tune = 2f64.powf(self.pad(pad, id::TUNE) / 12.0);
        // Keep Length: read at the sample's own speed, pitched by a
        // stretcher.
        let keep = self.pad(pad, id::KEEP) >= 0.5;
        let (step, pitch, slot) = match self.keep.as_mut().filter(|_| keep) {
            Some(pool) => {
                let ages: [u64; PADS * LAYERS] = std::array::from_fn(|i| self.voices[i].age);
                let (slot, evicted) = pool.claim(idx, |i| ages[i]);
                if let Some(e) = evicted {
                    self.voices[e].on = false;
                    self.voices[e].slot = None;
                }
                (s.rate / sr, tune as f32, Some(slot))
            }
            None => (tune * s.rate / sr, 1.0, None),
        };
        self.counter += 1;
        self.voices[idx] = Voice {
            on: true,
            pad,
            sample,
            pos: if reverse { frames - 1.0 - start } else { start },
            step,
            reverse,
            env: if attack < 0.05 { 1.0 } else { 0.0 },
            attack_step: if attack < 0.05 {
                1.0
            } else {
                1.0 / (attack * 0.001 * sr) as f32
            },
            rising: attack >= 0.05,
            decay: if decay >= 9_990.0 {
                1.0
            } else {
                (-6.9 / (decay * 0.001 * sr) as f32).exp()
            },
            fading: false,
            gain: [level * (1.0 - pan).min(1.0), level * (1.0 + pan).min(1.0)],
            svf: [(0.0, 0.0); 2],
            coefs: (a1, g * a1, g * g * a1),
            filtering: cutoff < 19_500.0,
            age: self.counter,
            slot,
            pitch,
            fed_out: false,
        };
    }

    fn pad_of(&self, key: u8) -> Option<usize> {
        let base = self.params.get(id::BASE_NOTE as usize).round() as i32;
        let p = i32::from(key) - base;
        (0..PADS as i32).contains(&p).then_some(p as usize)
    }

    // Each frame writes both output sides at its index.
    #[allow(clippy::needless_range_loop)]
    fn render(
        &mut self,
        set: Option<&super::samples::SampleSet>,
        out: &mut [&mut [f32]; 2],
        start: usize,
        end: usize,
    ) {
        let Some(set) = set else { return };
        let fade = self.fade;
        let Self {
            voices, keep, hits, ..
        } = self;
        for v in voices.iter_mut().filter(|v| v.on) {
            let Some(s) = set.samples.get(v.sample) else {
                v.on = false;
                continue;
            };
            // Keep Length: the segment from the voice's stretcher.
            let mut kept: Option<(&[f32], &[f32])> = None;
            if let (Some(slot), Some(pool)) = (v.slot, keep.as_mut()) {
                let pitch = v.pitch;
                let mut feed = |a: &mut [f32], b: &mut [f32]| feed_voice(v, s, a, b);
                match pool.render(slot, pitch, end - start, &mut feed) {
                    Render::Wait => continue,
                    Render::Done => {
                        v.on = false;
                        continue;
                    }
                    Render::Out(l, r) => kept = Some((l, r)),
                }
            }
            let mut loudest = 0.0f32;
            for i in start..end {
                let y = match kept {
                    Some((l, r)) => [l[i - start], r[i - start]],
                    None => {
                        let (y, ended) = next_frame(v, s);
                        if ended {
                            v.on = false;
                        }
                        y
                    }
                };
                let amp = v.env;
                // Attack, then the decay (a fast fade when let go or
                // choked).
                if v.fading {
                    v.env *= fade;
                } else if v.rising {
                    v.env += v.attack_step;
                    if v.env >= 1.0 {
                        v.env = 1.0;
                        v.rising = false;
                    }
                } else {
                    v.env *= v.decay;
                }
                if v.env < 1e-4 && !v.rising {
                    v.on = false;
                }
                for ch in 0..2 {
                    let mut x = y[ch];
                    if v.filtering {
                        let (a1, a2, a3) = v.coefs;
                        let (ic1, ic2) = &mut v.svf[ch];
                        let v3 = x - *ic2;
                        let v1 = a1 * *ic1 + a2 * v3;
                        let v2 = *ic2 + a2 * *ic1 + a3 * v3;
                        *ic1 = 2.0 * v1 - *ic1;
                        *ic2 = 2.0 * v2 - *ic2;
                        x = v2;
                    }
                    let o = x * amp * v.gain[ch];
                    loudest = loudest.max(o.abs());
                    out[ch][i] += o;
                }
                if !v.on {
                    break;
                }
            }
            hits[v.pad] = hits[v.pad].max(loudest);
        }
    }
}

impl PluginProcessor for DrumsProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let frames = io.frames.min(self.mix[0].len());
        let watched = self
            .tap
            .as_ref()
            .is_some_and(|t| self.watching.check(t, frames));
        let shared = Arc::clone(&self.shared);
        let guard = shared.try_lock();
        let set = guard.as_ref().and_then(|g| {
            if g.generation != self.generation {
                self.generation = g.generation;
                for v in &mut self.voices {
                    v.on = false;
                }
            }
            g.set.as_deref()
        });
        self.hits = [0.0; PADS];
        // Stretchers of voices that stopped go back; the block's priming.
        if let Some(pool) = &mut self.keep {
            let voices = &self.voices;
            pool.begin_block(frames, self.sr, |o, slot| {
                voices[o].on && voices[o].slot == Some(slot)
            });
        }
        let mut mix = std::mem::take(&mut self.mix);
        {
            let [l, r] = &mut mix;
            l[..frames].fill(0.0);
            r[..frames].fill(0.0);
            let mut sides: [&mut [f32]; 2] = [&mut l[..frames], &mut r[..frames]];
            let mut pos = 0usize;
            if let Some(events) = io.events_in.first() {
                for e in events.iter() {
                    let at = (e.sample_offset as usize).min(frames);
                    if at > pos {
                        self.render(set, &mut sides, pos, at);
                        pos = at;
                    }
                    match e.event {
                        MidiEvent::NoteOn { key, velocity, .. } => {
                            if let (Some(p), Some(set)) = (self.pad_of(key), set) {
                                self.hit(set, p, velocity);
                            }
                        }
                        MidiEvent::NoteOff { key, .. } => {
                            if let Some(p) = self.pad_of(key)
                                && self.pad(p, id::MODE) >= 0.5
                            {
                                for v in self.voices.iter_mut().filter(|v| v.on && v.pad == p) {
                                    v.fading = true;
                                }
                            }
                        }
                        MidiEvent::ControlChange { controller, .. }
                            if controller == MidiEvent::CC_ALL_SOUND_OFF
                                || controller == MidiEvent::CC_ALL_NOTES_OFF =>
                        {
                            for v in &mut self.voices {
                                v.fading = v.on;
                            }
                        }
                        _ => {}
                    }
                }
            }
            if frames > pos {
                self.render(set, &mut sides, pos, frames);
            }
        }
        drop(guard);
        let Some(out) = io.audio_out.first_mut() else {
            self.mix = mix;
            return ProcessStatus::Continue;
        };
        out.clear();
        let channels = out.num_channels();
        if channels >= 2 {
            out.channel_mut(0)[..frames].copy_from_slice(&mix[0][..frames]);
            out.channel_mut(1)[..frames].copy_from_slice(&mix[1][..frames]);
        } else if channels == 1 {
            for (o, (a, b)) in out
                .channel_mut(0)
                .iter_mut()
                .zip(mix[0].iter().zip(&mix[1]))
                .take(frames)
            {
                *o = 0.5 * (a + b);
            }
        }
        self.mix = mix;
        if let Some(tap) = &self.tap {
            for (p, h) in self.hits.iter().enumerate() {
                tap.raise_value(value::pad(p), *h);
            }
            tap.set_value(
                value::VOICES,
                self.voices.iter().filter(|v| v.on).count() as f32,
            );
            let mut peak = 0.0f32;
            for c in 0..2.min(channels) {
                for s in out.channel(c).iter().take(frames) {
                    self.meters[c].add(*s);
                    peak = peak.max(s.abs());
                }
                self.meters[c].publish(&tap.meter_out, c, frames);
            }
            tap.raise_value(value::OUT_PEAK, peak);
            if watched && channels > 0 {
                let r = out.channel(1.min(channels - 1));
                tap.output.push(&out.channel(0)[..frames], &r[..frames]);
            }
        }
        if self.voices.iter().any(|v| v.on) {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Sleep
        }
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; PADS * LAYERS];
        if let Some(pool) = &mut self.keep {
            pool.release_all();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::bin_db;
    use crate::devices::sampler::tests::{Play, on};
    use crate::devices::samples::{SampleDoc, SampleHost, fixtures};

    #[test]
    fn pads_play_their_samples_and_chokes_cut() {
        let d = fixtures::dir("drums");
        let mut doc = SampleDoc::default();
        doc.set(
            0,
            Some(fixtures::tone(&d.join("kick.wav"), 60.0, 48_000, 0.5)),
        );
        doc.set(
            2,
            Some(fixtures::tone(&d.join("closed.wav"), 3_000.0, 48_000, 0.5)),
        );
        doc.set(
            3,
            Some(fixtures::tone(&d.join("open.wav"), 5_000.0, 48_000, 2.0)),
        );
        let params = ParamValues::new(parameters());
        for p in [2, 3] {
            params
                .set_by_id(ParameterId(id::pad(p) + id::CHOKE), 1.0)
                .unwrap();
        }
        // Pad 0 an octave up.
        params
            .set_by_id(ParameterId(id::pad(0) + id::TUNE), 12.0)
            .unwrap();
        let mut host = SampleHost::default();
        host.set_doc(doc, None);
        let mut s = Play::new(DrumsProcessor::new(
            params,
            None,
            &crate::devices::rig::config(),
            Arc::clone(&host.shared),
        ));
        s.send(on(36, 127));
        let (l, _, _) = s.run(0.2);
        assert!(bin_db(&l, 120.0) > -15.0, "{}", bin_db(&l, 120.0));
        // The open hat rings until the closed one cuts it.
        s.send(on(39, 127));
        let (l, _, _) = s.run(0.3);
        assert!(bin_db(&l, 5_000.0) > -15.0);
        s.send(on(38, 127));
        s.run(0.05);
        let (l, _, _) = s.run(0.4);
        assert!(
            bin_db(&l, 5_000.0) < -80.0,
            "choked: {}",
            bin_db(&l, 5_000.0)
        );
        // An empty pad is silent.
        s.run(1.0);
        s.send(on(37, 127));
        let (l, _, status) = s.run(0.2);
        assert!(l.iter().all(|v| *v == 0.0));
        assert_eq!(status, ProcessStatus::Sleep);
    }

    #[test]
    fn a_pad_can_keep_its_length_when_tuned() {
        let d = fixtures::dir("drums-keep");
        let mut doc = SampleDoc::default();
        doc.set(
            0,
            Some(fixtures::tone(&d.join("tom.wav"), 220.0, 48_000, 0.4)),
        );
        let play = |keep: f64| {
            let params = ParamValues::new(parameters());
            let pad = |f: u32, v: f64| params.set_by_id(ParameterId(id::pad(0) + f), v).unwrap();
            pad(id::TUNE, 12.0);
            pad(id::KEEP, keep);
            let mut host = SampleHost::default();
            host.set_doc(doc.clone(), None);
            let mut s = Play::new(DrumsProcessor::new(
                params,
                None,
                &crate::devices::rig::config(),
                Arc::clone(&host.shared),
            ));
            s.send(on(36, 127));
            let (l, _, _) = s.run(1.0);
            let sr = crate::devices::rig::SR;
            let sounding = l.iter().rposition(|v| v.abs() > 0.01).unwrap_or(0) as f64 / sr;
            let head = &l[..(0.15 * sr) as usize];
            (sounding, bin_db(head, 440.0))
        };
        // Repitched: half as long; kept: as long, both an octave up.
        let (short, hi) = play(0.0);
        assert!((short - 0.2).abs() < 0.03, "{short}");
        assert!(hi > -14.0, "{hi}");
        let (long, hi) = play(1.0);
        assert!((long - 0.4).abs() < 0.05, "{long}");
        assert!(hi > -14.0, "{hi}");
    }

    #[test]
    fn the_state_carries_the_samples_through_the_registry() {
        use crate::PluginFactory;
        let d = fixtures::dir("drum-state");
        let mut doc = SampleDoc::default();
        doc.set(
            5,
            Some(fixtures::tone(&d.join("x.wav"), 440.0, 48_000, 0.2)),
        );
        let mut a = crate::builtin::BuiltinFactory
            .instantiate(faderframe_core::builtin::DRUMS)
            .unwrap();
        let params = ParamValues::new(parameters());
        a.load_state(&crate::devices::samples::pack(&params.save(), &doc))
            .unwrap();
        let state = a.save_state().unwrap();
        let (_, back) = crate::devices::samples::unpack(&state).unwrap();
        assert_eq!(back, doc);
        let contents = a
            .tap()
            .unwrap()
            .assets::<crate::devices::samples::Contents>()
            .unwrap();
        assert!(contents.set.slot(5).is_some());
        assert!(contents.set.errors.is_empty());
    }
}
