//! A test rig for the devices: a processor fed blocks of a signal (and a
//! sidechain), its output collected.

use crate::tap::AnalysisTap;
use crate::{ParamValues, ParameterInfo, PluginProcessContext, PluginProcessor, ProcessConfig};
use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_core::{ChannelLayout, ParameterId};
use faderframe_transport::TransportInfo;
use std::sync::Arc;

pub const SR: f64 = 48_000.0;
pub const BLOCK: usize = 256;

pub fn config() -> ProcessConfig {
    ProcessConfig {
        sample_rate: SR,
        max_block_size: BLOCK as u32,
        sidechain: true,
        double_precision: false,
    }
}

pub struct Rig<P> {
    pub params: ParamValues,
    pub tap: Arc<AnalysisTap>,
    pub p: P,
    ins: Vec<AudioBuffer>,
    outs: Vec<AudioBuffer>,
    pub transport: TransportInfo,
    /// Frames run so far.
    pub at: usize,
}

impl<P: PluginProcessor> Rig<P> {
    /// With parameters set before the processor is made.
    pub fn with(
        infos: Vec<ParameterInfo>,
        values: usize,
        set: &[(u32, f64)],
        make: impl FnOnce(ParamValues, Arc<AnalysisTap>, &ProcessConfig) -> P,
    ) -> Self {
        let params = ParamValues::new(infos);
        for (id, v) in set {
            params.set_by_id(ParameterId(*id), *v).unwrap();
        }
        let tap = Arc::new(AnalysisTap::new(params.clone(), values));
        let p = make(params.clone(), Arc::clone(&tap), &config());
        let mut ins = vec![
            AudioBuffer::new(ChannelLayout::Stereo, BLOCK),
            AudioBuffer::new(ChannelLayout::Stereo, BLOCK),
        ];
        let mut outs = vec![AudioBuffer::new(ChannelLayout::Stereo, BLOCK)];
        for b in ins.iter_mut().chain(outs.iter_mut()) {
            b.set_len(BLOCK);
        }
        Self {
            params,
            tap,
            p,
            ins,
            outs,
            transport: TransportInfo::default(),
            at: 0,
        }
    }

    pub fn set(&self, id: u32, v: f64) {
        self.params.set_by_id(ParameterId(id), v).unwrap();
    }

    /// Run `seconds` of `signal(frame)` with `key(frame)` on the sidechain.
    pub fn run(
        &mut self,
        seconds: f64,
        signal: impl Fn(usize) -> (f32, f32),
        key: impl Fn(usize) -> (f32, f32),
    ) -> (Vec<f32>, Vec<f32>) {
        let blocks = (seconds * SR / BLOCK as f64).ceil() as usize;
        let (mut l, mut r) = (Vec::new(), Vec::new());
        for _ in 0..blocks {
            for i in 0..BLOCK {
                let n = self.at + i;
                let (a, b) = signal(n);
                self.ins[0].channel_mut(0)[i] = a;
                self.ins[0].channel_mut(1)[i] = b;
                let (ka, kb) = key(n);
                self.ins[1].channel_mut(0)[i] = ka;
                self.ins[1].channel_mut(1)[i] = kb;
            }
            let ctx = PluginProcessContext {
                transport: &self.transport,
                param_events: &[],
                harmony: &crate::NO_HARMONY,
                param_mods: &[],
            };
            let mut io = NodeIo {
                frames: BLOCK,
                audio_in: &self.ins,
                audio_out: &mut self.outs,
                events_in: &[],
                events_out: &mut [],
            };
            self.p.process(&ctx, &mut io);
            l.extend_from_slice(self.outs[0].channel(0));
            r.extend_from_slice(self.outs[0].channel(1));
            self.at += BLOCK;
            self.transport.sample_position += BLOCK as i64;
        }
        (l, r)
    }
}

pub fn tone(f: f64, amp: f64) -> impl Fn(usize) -> (f32, f32) {
    move |n| {
        let v = (amp * (std::f64::consts::TAU * f * n as f64 / SR).sin()) as f32;
        (v, v)
    }
}

pub fn silence(_: usize) -> (f32, f32) {
    (0.0, 0.0)
}

/// A sine's level in dB from its RMS (whole periods of `f` at the end).
pub fn level(x: &[f32], f: f64) -> f64 {
    let period = SR / f;
    let periods = ((x.len() as f64 / 2.0) / period).floor().max(1.0);
    let n = ((periods * period).round() as usize).min(x.len());
    let tail = &x[x.len() - n..];
    let ms: f64 = tail.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / n as f64;
    10.0 * (2.0 * ms).max(1e-30).log10()
}

/// The level (dB, of a sine's amplitude) at `f`: a Hann-windowed DFT of
/// the second half.
pub fn bin_db(x: &[f32], f: f64) -> f64 {
    let tail = &x[x.len() / 2..];
    let n = tail.len() as f64;
    let w = std::f64::consts::TAU * f / SR;
    let (mut re, mut im, mut sum) = (0.0, 0.0, 0.0);
    for (k, v) in tail.iter().enumerate() {
        let win = 0.5 - 0.5 * (std::f64::consts::TAU * k as f64 / n).cos();
        re += f64::from(*v) * win * (w * k as f64).cos();
        im += f64::from(*v) * win * (w * k as f64).sin();
        sum += win;
    }
    20.0 * (2.0 * re.hypot(im) / sum).max(1e-15).log10()
}

pub fn peak_db(x: &[f32]) -> f64 {
    let p = x.iter().fold(0.0f64, |m, v| m.max(f64::from(v.abs())));
    20.0 * p.max(1e-15).log10()
}

/// Total harmonic distortion of a sine at `f` (percent, harmonics 2–7).
pub fn thd(x: &[f32], f: f64) -> f64 {
    let bin = |h: f64| {
        let w = std::f64::consts::TAU * f * h / SR;
        let (mut re, mut im) = (0.0, 0.0);
        for (k, v) in x.iter().enumerate() {
            re += f64::from(*v) * (w * k as f64).cos();
            im += f64::from(*v) * (w * k as f64).sin();
        }
        re.hypot(im)
    };
    let fundamental = bin(1.0);
    let harmonics: f64 = (2..8).map(|h| bin(h as f64).powi(2)).sum::<f64>().sqrt();
    100.0 * harmonics / fundamental.max(1e-30)
}
