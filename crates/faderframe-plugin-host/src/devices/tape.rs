//! Tape Machine: recording to and playing back from analog tape on a
//! studio deck, in the manner of a Studer A80/A800 or an Ampex ATR-102 —
//! modelled from the physics of magnetic recording, not from any other
//! code.
//!
//! The signal goes the machine's way:
//!
//! 1. The input amplifier (its trim, its headroom +20 dB over the
//!    operating level) and the record equaliser: the standard's flux curve
//!    (NAB, IEC or AES by speed) and the boosts that make up the tape's
//!    thickness and recording-zone losses — so slow speeds drive the highs
//!    hard ([`heads`]).
//! 2. The tape, at 2, 4 or 8 times the rate: the Jiles–Atherton model of its
//!    particles, the AC bias recording all it reaches anhysteretically and
//!    leaving the rest on the hysteresis loop, solved with an exponential
//!    integrator that stays exact and stable however stiff ([`media`]).
//!    Three formulations fitted to their data sheets (a 456-like standard
//!    tape, a GP9-like high-output one and an older 632-like one) at five
//!    operating levels, 185 to 520 nWb/m.
//! 3. The reproduce side: the losses the record equaliser made up for,
//!    the bias's own HF erasure when it is not aligned, the reproduce gap
//!    and spacing, azimuth, the reproduce equaliser and the head bump of a
//!    ¼″ or ½″ head.
//! 4. The transport: wow, flutter and scrape flutter from the lines of a
//!    capstan machine's spectrum at a good machine's weighted figures
//!    ([`transport`]), carried out inside the oversampler's own delay —
//!    the device's latency is the oversampling's alone.
//! 5. Hiss (off unless asked for: a device on every track would add every
//!    track's hiss; when on it is at the formulation's measured level for
//!    the speed, equalisation, track width and operating level),
//!    crosstalk between the tracks, the output trim — the device is
//!    level-neutral at its operating level, and Auto Output takes the Input
//!    trim back off so driving the tape harder only saturates it — and a
//!    Mix.

mod heads;
mod media;
mod os;
#[cfg(test)]
mod tests;
mod transport;

pub use heads::{FORMATS, SPEEDS, STANDARDS, bump_frequency, standard_name};
pub use media::{Bias, FORMULATIONS, Formulation, Magnet, Media};
pub use transport::SPEC as FLUTTER_SPEC;

use super::{fixed, on_off, param, pass_through, pick, stepped};
use crate::dsp::delay::DelayLine;
use crate::dsp::smooth::Smoothed;
use crate::dsp::{Noise, flush, gain};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::{Arc, OnceLock};

/// Stable parameter ids (`id == position` in [`parameters`]).
pub mod id {
    pub const INPUT: u32 = 0;
    pub const OUTPUT: u32 = 1;
    /// The output follows the input trim back (level-neutral driving).
    pub const AUTO_OUTPUT: u32 = 2;
    pub const MIX: u32 = 3;
    /// 3.75, 7.5, 15, 30 ips.
    pub const SPEED: u32 = 4;
    /// Standard, High Output, Vintage.
    pub const TAPE: u32 = 5;
    /// The operating level: 185, 250, 320, 370, 520 nWb/m.
    pub const LEVEL: u32 = 6;
    /// NAB, IEC (at 30 ips both are AES).
    pub const EQ: u32 = 7;
    /// The bias current re the aligned one (dB).
    pub const BIAS: u32 = 8;
    /// ¼″ or ½″ two-track.
    pub const FORMAT: u32 = 9;
    /// Wow and flutter, times the machine's specification.
    pub const FLUTTER: u32 = 10;
    pub const HISS: u32 = 11;
    /// The reproduce head's azimuth error (arc minutes).
    pub const AZIMUTH: u32 = 12;
    /// 2×, 4×, 8× (changes the latency).
    pub const OVERSAMPLING: u32 = 13;
}

/// Published values.
pub mod value {
    /// The tape level's mean square per side over the last block (linear,
    /// 1 = the operating level's sine: 0 VU).
    pub const VU_L: usize = 0;
    pub const VU_R: usize = 1;
    /// The most the tape was magnetised in the last block (share of its
    /// saturation).
    pub const SATURATION: usize = 2;
    /// The tape speed's deviation now (fraction).
    pub const SPEED: usize = 3;
}
pub const TAP_VALUES: usize = 4;

/// Operating levels (nWb/m) and their names.
pub const LEVELS: [f64; 5] = [185.0, 250.0, 320.0, 370.0, 520.0];
pub const LEVEL_NAMES: [&str; 5] = [
    "185 nWb/m (+0)",
    "250 nWb/m (+3)",
    "320 nWb/m (+5)",
    "370 nWb/m (+6)",
    "520 nWb/m (+9)",
];
pub const SPEED_NAMES: [&str; 4] = ["3¾ ips", "7½ ips", "15 ips", "30 ips"];
pub const TAPE_NAMES: [&str; 3] = ["Standard", "High Output", "Vintage"];
pub const FORMAT_NAMES: [&str; 2] = ["¼″ two-track", "½″ two-track"];
pub const OVERSAMPLING_NAMES: [&str; 3] = ["2×", "4×", "8×"];

/// The digital level of the operating level: a sine of −18 dBFS peak reads
/// 0 VU (EBU R68).
pub const REFERENCE_DBFS: f64 = -18.0;

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::INPUT, "Input", -24.0, 24.0, 0.0, Decibels),
        param(id::OUTPUT, "Output", -24.0, 24.0, 0.0, Decibels),
        stepped(id::AUTO_OUTPUT, "Auto Output", 1.0, 1.0),
        param(id::MIX, "Mix", 0.0, 1.0, 1.0, Percent),
        stepped(id::SPEED, "Speed", 3.0, 2.0),
        stepped(id::TAPE, "Tape", 2.0, 0.0),
        stepped(id::LEVEL, "Operating Level", 4.0, 1.0),
        stepped(id::EQ, "Equalisation", 1.0, 0.0),
        param(id::BIAS, "Bias", -6.0, 6.0, 0.0, Decibels),
        stepped(id::FORMAT, "Format", 1.0, 0.0),
        param(id::FLUTTER, "Wow & Flutter", 0.0, 5.0, 1.0, None),
        stepped(id::HISS, "Hiss", 1.0, 0.0),
        param(id::AZIMUTH, "Azimuth", -20.0, 20.0, 0.0, None),
        fixed(stepped(id::OVERSAMPLING, "Oversampling", 2.0, 0.0)),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::SPEED => pick(&SPEED_NAMES, v),
        id::TAPE => pick(&TAPE_NAMES, v),
        id::LEVEL => pick(&LEVEL_NAMES, v),
        id::EQ => pick(&STANDARDS, v),
        id::FORMAT => pick(&FORMAT_NAMES, v),
        id::OVERSAMPLING => pick(&OVERSAMPLING_NAMES, v),
        id::AUTO_OUTPUT | id::HISS => on_off(v),
        id::BIAS if v.abs() < 0.05 => "0.0 dB (aligned)".into(),
        id::BIAS if v > 0.0 => format!("+{v:.1} dB over"),
        id::BIAS => format!("{v:.1} dB under").replace('-', "−"),
        id::FLUTTER if v < 0.005 => "Off".into(),
        id::FLUTTER => format!("×{v:.2}"),
        id::AZIMUTH if v.abs() < 0.05 => "0′".into(),
        id::AZIMUTH => format!("{v:+.1}′").replace('-', "−"),
        _ => return None,
    })
}

/// The oversampling factor a setting asks for.
pub fn factor(setting: f64) -> usize {
    [2, 4, 8][setting.round().clamp(0.0, 2.0) as usize]
}

pub fn latency(params: &ParamValues, _rate: f64) -> u32 {
    os::latency(factor(f64::from(params.get(id::OVERSAMPLING as usize))))
}

/// The weighted peak wow and flutter (fraction) at a speed and setting.
pub fn flutter_figure(speed: usize, depth: f64) -> f64 {
    transport::SPEC[speed.min(3)] * depth.max(0.0)
}

/// The record drive (field per unit of the input, share of `M_s`) and the
/// output gain that make each formulation level-neutral at each operating
/// level: the drive whose recording has the level's flux as its
/// fundamental, the gain that brings that back to the reference.
struct Calibration {
    drive: [[f64; 5]; 3],
    output: [[f64; 5]; 3],
}

fn calibration() -> &'static Calibration {
    static CAL: OnceLock<Calibration> = OnceLock::new();
    CAL.get_or_init(|| {
        let reference = gain(REFERENCE_DBFS);
        let mut cal = Calibration {
            drive: [[1.0; 5]; 3],
            output: [[1.0; 5]; 3],
        };
        for (t, f) in FORMULATIONS.iter().enumerate() {
            let media = Media::new(f, 1.0, 1.0);
            for (l, level) in LEVELS.iter().enumerate() {
                let m = f.m_of_flux(*level);
                cal.drive[t][l] = media::amplitude_for(&media, m) / reference;
                cal.output[t][l] = reference / m;
            }
        }
        cal
    })
}

/// The settings a response is drawn for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Response {
    pub speed: usize,
    pub iec: bool,
    pub tape: usize,
    pub format: usize,
    /// dB re the aligned bias.
    pub bias: f64,
    /// The oversampling factor.
    pub factor: usize,
}

/// The machine's small-signal response (record and reproduce, the bump)
/// at `f` Hz, at 48 kHz — what the editor draws (the bias's level change
/// at low levels included).
pub fn response_db(r: &Response, f: f64) -> f64 {
    let host = 48_000.0;
    let fs = host * r.factor.max(1) as f64;
    let (speed, iec, tape, format, bias_db) = (r.speed, r.iec, r.tape, r.format, r.bias);
    let t = FORMULATIONS[tape.min(2)];
    let b = Bias::at(gain(bias_db), &t);
    let d = heads::Design::new(&heads::Setup {
        speed,
        iec,
        coating: t.coating,
        zone: b.zone,
        format,
        azimuth: 0.0,
        fs,
    });
    let bump = heads::Bump::new(speed, format, host);
    let small = b.switched + (1.0 - b.switched) * t.c;
    20.0 * (d.response(f, fs) * bump.magnitude(f, host) * b.drive * small)
        .max(1e-12)
        .log10()
}

/// The tape's magnetisation against the recording field over one period
/// of a sine reaching the formulation's 3 % level (times `over_db`), at a
/// bias (dB re aligned): `(field as a share of the sine's peak,
/// magnetisation as a share of saturation)` — the loop the editor draws,
/// closed when aligned, open under-biased.
pub fn hysteresis_loop(tape: usize, bias_db: f64, over_db: f64, steps: usize) -> Vec<(f64, f64)> {
    let f = FORMULATIONS[tape.min(2)];
    let bias = Bias::at(gain(bias_db), &f);
    let media = Media::biased(&f, &bias);
    let aligned = Media::new(&f, 1.0, 1.0);
    let peak = media::amplitude_for(&aligned, f.m_of_flux(f.mol)) * bias.drive * gain(over_db);
    let mut magnet = Magnet::default();
    let steps = steps.max(8);
    let mut out = Vec::with_capacity(steps + 1);
    for i in 0..=3 * steps {
        let s = (std::f64::consts::TAU * i as f64 / steps as f64).sin();
        let m = magnet.step(peak * s, &media);
        if i >= 2 * steps {
            out.push((s, m));
        }
    }
    out
}

/// What the knobs ask for, read once a block.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Controls {
    speed: usize,
    iec: bool,
    tape: usize,
    level: usize,
    format: usize,
    bias_db: f64,
    azimuth: f64,
    flutter: f64,
    hiss: bool,
}

/// One channel of the machine.
struct Channel {
    os: os::TapeOversampler,
    design: heads::Design,
    magnet: Magnet,
    noise: Noise,
    bump: heads::Bump,
    dc: crate::dsp::filter::DcBlock,
    /// The other track's leak into this one.
    crosstalk: heads::FirstOrder,
    /// The most the tape was magnetised this block.
    peak_m: f64,
}

pub struct TapeProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    hi_rate: f64,
    channels: [Channel; 2],
    transport: transport::Transport,
    dry: [DelayLine; 2],
    latency: usize,
    input: Smoothed,
    output: Smoothed,
    /// The wet path's gains: the calibration back to the reference level,
    /// and the input trim taken back (Auto Output).
    calib: Smoothed,
    comp: Smoothed,
    mix: Smoothed,
    /// The record drive (field per input unit, the bias's share in).
    drive: Smoothed,
    media: Media,
    controls: Option<Controls>,
    setup: Option<heads::Setup>,
    /// Hiss per oversampled sample (share of `M_s`), 0 when off.
    hiss: f64,
    hiss_unit: f64,
    crosstalk: f64,
    /// The input amplifier's clip (input units).
    headroom: f64,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl TapeProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let f = factor(f64::from(params.get(id::OVERSAMPLING as usize)));
        let os_ = os::TapeOversampler::new(f);
        let latency = os_.latency() as usize;
        let hi_rate = sr * os_.factor() as f64;
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        // The tables and the calibration are made here, not on the audio
        // thread.
        media::tables();
        calibration();
        let channel = |seed: u32| Channel {
            os: os::TapeOversampler::new(f),
            design: heads::Design::default(),
            magnet: Magnet::default(),
            noise: Noise::new(seed),
            bump: heads::Bump::new(2, 0, sr),
            dc: crate::dsp::filter::DcBlock::new(sr),
            crosstalk: heads::FirstOrder::IDENTITY,
            peak_m: 0.0,
        };
        let mut p = Self {
            watching: Watching::new(sr as f32),
            sr,
            hi_rate,
            channels: [channel(0x1234_5678), channel(0x0bad_cafe)],
            transport: transport::Transport::new(sr, os::reach(f) - 1.0),
            dry: [DelayLine::new(latency + 1), DelayLine::new(latency + 1)],
            latency,
            input: Smoothed::new(1.0, 20.0, sr),
            output: Smoothed::new(1.0, 20.0, sr),
            calib: Smoothed::new(1.0, 20.0, sr),
            comp: Smoothed::new(1.0, 20.0, sr),
            mix: Smoothed::new(1.0, 20.0, sr),
            drive: Smoothed::new(1.0, 20.0, sr),
            media: Media::new(&FORMULATIONS[0], 1.0, 1.0),
            controls: None,
            setup: None,
            hiss: 0.0,
            hiss_unit: 0.0,
            crosstalk: 0.0,
            headroom: gain(REFERENCE_DBFS + 20.0),
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        };
        p.read_controls();
        p.input.snap();
        p.output.snap();
        p.calib.snap();
        p.comp.snap();
        p.mix.snap();
        p.drive.snap();
        p
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(pid as usize))
    }

    fn pick(&self, pid: u32, n: usize) -> usize {
        (self.get(pid).round().max(0.0) as usize).min(n - 1)
    }

    /// Read the knobs: redesign what changed.
    fn read_controls(&mut self) {
        let c = Controls {
            speed: self.pick(id::SPEED, 4),
            iec: self.get(id::EQ) >= 0.5,
            tape: self.pick(id::TAPE, 3),
            level: self.pick(id::LEVEL, 5),
            format: self.pick(id::FORMAT, 2),
            bias_db: self.get(id::BIAS).clamp(-12.0, 12.0),
            azimuth: self.get(id::AZIMUTH).clamp(-30.0, 30.0),
            flutter: self.get(id::FLUTTER).clamp(0.0, 10.0),
            hiss: self.get(id::HISS) >= 0.5,
        };
        let tape = FORMULATIONS[c.tape];
        let bias = Bias::at(gain(c.bias_db), &tape);
        let cal = calibration();
        let g_in = gain(self.get(id::INPUT));
        self.input.set(g_in);
        self.output.set(gain(self.get(id::OUTPUT)));
        self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
        let auto = self.get(id::AUTO_OUTPUT) >= 0.5;
        self.calib.set(cal.output[c.tape][c.level]);
        self.comp.set(if auto { 1.0 / g_in } else { 1.0 });
        self.drive.set(cal.drive[c.tape][c.level] * bias.drive);
        if self.controls == Some(c) {
            return;
        }
        self.media = Media::biased(&tape, &bias);
        self.transport.set(c.speed, c.flutter);
        let setup = heads::Setup {
            speed: c.speed,
            iec: c.iec,
            coating: tape.coating,
            zone: bias.zone,
            format: c.format,
            azimuth: c.azimuth,
            fs: self.hi_rate,
        };
        if self.setup != Some(setup) {
            let design = heads::Design::new(&setup);
            let fmt = FORMATS[c.format];
            for ch in &mut self.channels {
                ch.design.retune(&design);
                ch.bump.set(c.speed, c.format, self.sr);
                // The leak: −45 dB (¼″) or −53 dB (½″) at 1 kHz, 12 dB more
                // under 150 Hz (long wavelengths fringe).
                let shelf = heads::FirstOrder::low_shelf(600.0, 150.0, self.sr);
                ch.crosstalk.retune(shelf);
                self.crosstalk = gain(fmt.crosstalk) / shelf.magnitude(1000.0, self.sr);
            }
            self.hiss_unit = hiss_unit(&design, self.hi_rate);
            self.setup = Some(setup);
        }
        // The tape's noise (A-weighted, nWb/m) for the speed (3 dB per
        // halving: the same wavelengths in half the band) and the track
        // width (noise adds up over the width as power, the signal as
        // flux), as a share of the coating's saturation flux.
        let ips = SPEEDS[c.speed];
        let fmt = FORMATS[c.format];
        let flux = tape.noise * (15.0 / ips).sqrt() * (0.075 * 0.0254 / fmt.track).sqrt();
        self.hiss = if c.hiss {
            flux / tape.saturation_flux() * self.hiss_unit
        } else {
            0.0
        };
        self.controls = Some(c);
    }
}

/// White noise of unit RMS per oversampled sample through the reproduce
/// side reads this A-weighted RMS; its inverse scales the hiss.
fn hiss_unit(design: &heads::Design, fs: f64) -> f64 {
    // ∫ |H(f) A(f)|² df over 20 Hz–20 kHz on a log grid, for white noise
    // whose power spreads evenly to fs/2.
    let n = 400;
    let (lo, hi) = (20.0f64.ln(), 20_000.0f64.ln());
    let mut sum = 0.0;
    for i in 0..n {
        let f0 = (lo + (hi - lo) * i as f64 / n as f64).exp();
        let f1 = (lo + (hi - lo) * (i + 1) as f64 / n as f64).exp();
        let f = (f0 * f1).sqrt();
        let g = design.reproduce_response(f, fs) * heads::a_weighting(f);
        sum += g * g * (f1 - f0);
    }
    let rms = (sum * 2.0 / fs).sqrt();
    1.0 / rms.max(1e-12)
}

impl PluginProcessor for TapeProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let Some(channels) = pass_through(io) else {
            return ProcessStatus::Continue;
        };
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        if channels == 0 {
            return ProcessStatus::Continue;
        }
        let n = io.frames.min(self.scratch[0].len());
        let watched = self.watching.check(&self.tap, n);
        let sides = channels.min(2);
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        self.read_controls();
        let Some(c) = self.controls else {
            return ProcessStatus::Continue;
        };
        // The azimuth error delays one track against the other by the
        // track pitch times its tangent over the speed.
        let v = heads::metres_per_second(SPEEDS[c.speed]);
        let skew = FORMATS[c.format].pitch * (c.azimuth / 60.0).to_radians().tan() / v * self.sr;
        let media = self.media;
        let (hiss, headroom, xt) = (self.hiss, self.headroom, self.crosstalk);
        let mut vu = [0.0f64; 2];
        for ch in &mut self.channels {
            ch.peak_m = 0.0;
        }
        for i in 0..n {
            let g_in = self.input.tick();
            let g_out = self.output.tick();
            let calib = self.calib.tick();
            let comp = self.comp.tick();
            let mix = self.mix.tick();
            let drive = self.drive.tick();
            let displace = self.transport.tick();
            let mut wet = [0.0f64; 2];
            for (s, w) in wet.iter_mut().enumerate().take(sides) {
                let x = f64::from(self.scratch[s][i]);
                self.dry[s].push(x);
                let shift = if s == 0 { 0.5 * skew } else { -0.5 * skew };
                let Channel {
                    os,
                    design,
                    magnet,
                    noise,
                    peak_m,
                    ..
                } = &mut self.channels[s];
                let mut tape = |u: f64| -> f64 {
                    // The input amplifier and the record equaliser.
                    let r = soft_clip(design.record(u * g_in), headroom);
                    // The tape.
                    let m = magnet.step(r * drive, &media);
                    *peak_m = peak_m.max(m.abs());
                    // Its noise, and the way back.
                    let h = if hiss > 0.0 {
                        hiss * (noise.tick() + noise.tick()) * 1.224_744_871
                    } else {
                        0.0
                    };
                    design.reproduce(m + h)
                };
                let y = os.process(x, displace + shift, &mut tape);
                let ch = &mut self.channels[s];
                *w = ch.dc.process(ch.bump.process(y)) * calib;
            }
            if sides == 2 {
                let [a, b] = wet;
                let la = self.channels[1].crosstalk.process(a);
                let lb = self.channels[0].crosstalk.process(b);
                wet = [a + xt * lb, b + xt * la];
            }
            for s in 0..sides {
                vu[s] += wet[s] * wet[s];
                let dry = self.dry[s].tap(self.latency);
                let y = (dry * (1.0 - mix) + wet[s] * comp * mix) * g_out;
                out.channel_mut(s)[i] = y as f32;
            }
        }
        for ch in &mut self.channels {
            ch.design.flush();
            ch.bump.flush();
            ch.crosstalk.flush();
        }
        let reference = gain(REFERENCE_DBFS);
        let unit = reference * reference / 2.0;
        let nf = n.max(1) as f64;
        self.tap
            .set_value(value::VU_L, flush(vu[0] / nf / unit) as f32);
        self.tap.set_value(
            value::VU_R,
            flush(vu[if sides == 2 { 1 } else { 0 }] / nf / unit) as f32,
        );
        let sat = self.channels[0].peak_m.max(self.channels[1].peak_m);
        self.tap.set_value(value::SATURATION, sat as f32);
        self.tap
            .set_value(value::SPEED, self.transport.speed as f32);
        for c in 0..2 {
            let o = out.channel(c.min(channels - 1));
            for (x, y) in self.scratch[c][..n].iter().zip(&o[..n]) {
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
            let r = out.channel(1.min(channels - 1));
            self.tap.output.push(&out.channel(0)[..n], &r[..n]);
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        for ch in &mut self.channels {
            ch.os.reset();
            ch.design.reset();
            ch.magnet.reset();
            ch.bump.reset();
            ch.dc.reset();
            ch.crosstalk.reset();
        }
        self.transport.reset();
        for d in &mut self.dry {
            d.reset();
        }
        for m in self.meters.iter_mut().flatten() {
            m.reset();
        }
    }
}

/// The input amplifier's headroom: clean to 70 % of `limit`, then rounding
/// off into it.
#[inline]
fn soft_clip(x: f64, limit: f64) -> f64 {
    let knee = 0.7 * limit;
    let a = x.abs();
    if a <= knee {
        x
    } else {
        let room = limit - knee;
        x.signum() * (knee + room * ((a - knee) / room).tanh())
    }
}
