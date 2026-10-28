//! 2A Leveler: an optical leveling amplifier in the manner of the classic
//! Teletronix/UREI LA-2A, written fresh from the factory schematic and the
//! published behaviour of its T4 cell (not from any other code).
//!
//! * **The circuit** (`faderframe_circuit::circuits::leveler`, solved by
//!   the netlist solver as the preamps are): the input transformer and the
//!   T4's attenuator (R6 68 k, R7, the cell as the shunt leg, the Gain and
//!   Peak Reduction pots' loads) at node `c`; the make-up amplifier — from
//!   R1's wiper (the Gain) into two 12AX7 stages and the 12BH7 totem pole
//!   with its 20 dB of feedback — and the output transformer into 600 ohm;
//!   and the sidechain — from R2's wiper (Peak Reduction) into both halves
//!   of a 12AX7, R37's HF emphasis, the 6AQ5 that drives the
//!   electroluminescent panel. The valves after the Gain run at twice the
//!   rate below 88.2 kHz (the latency of the halfband filters is reported);
//!   the input and the sidechain at the host's.
//! * **The cell** ([`cell`]): the panel's light from its voltage (the
//!   Alfrey–Taylor law over the charge it moves, which makes it brighter
//!   with frequency up to about a kilohertz, then less so), and the
//!   cadmium-sulphide photoresistor's conductance from the light: free
//!   electrons recombining through one level and through Rose's spread of
//!   centres, and three trap levels whose slow emptying is the T4's
//!   two-stage, history-dependent release.
//! * **Compress / Limit** is SW3: in Compress the sidechain hears the
//!   reduced signal (feedback, about 3:1); in Limit the junction above R7,
//!   which keeps hearing part of the input however deep the reduction
//!   (part feed-forward: the curve flattens).
//! * **The meter**: gain reduction (from the cell, as the T4's second cell
//!   drives the hardware's), or the output with 0 VU at +4 or +10 dBm
//!   (−18 dBFS being +4 dBm).
//! * Modern additions, none of which changes the classic sound when left
//!   alone: Mix, an external sidechain into the panel's driver, a stereo
//!   link (both cells lit by the brighter panel).
//!
//! Levels: −18 dBFS at the input is +4 dBm on its 600 ohm terminals (from a
//! 600 ohm source), and +4 dBm at the output (across 600 ohm) is −18 dBFS.

mod cell;
#[cfg(test)]
mod tests;

use super::{fixed, on_off, param, pass_through, pick, stepped};
use crate::dsp::delay::DelayLine;
use crate::dsp::env::MeanSquare;
use crate::dsp::smooth::Smoothed;
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginError, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_circuit::circuits::leveler as circuit;
use faderframe_circuit::dsp::netlist::Taper;
use faderframe_circuit::dsp::oversample::Oversampler;
use faderframe_circuit::dsp::time::Simulation;
use faderframe_core::ParameterId;
use std::sync::Arc;

pub use cell::{CellSpec, Panel, PanelSpec, Photocell, RELAXATIONS, TRAPS, Trap};

pub mod id {
    /// The Gain knob (0…100 as its dial is marked): R1, the make-up
    /// amplifier's gain control, in the circuit.
    pub const GAIN: u32 = 0;
    /// The Peak Reduction knob (0…100): R2, how much of the signal reaches
    /// the panel's driver.
    pub const PEAK_REDUCTION: u32 = 1;
    /// Compress (0) or Limit (1): SW3.
    pub const MODE: u32 = 2;
    /// The meter switch: Gain Reduction (0), +4 (1), +10 (2).
    pub const METER: u32 = 3;
    /// R37, the sidechain's HF emphasis trim (0 = its "flat" end … 1).
    pub const EMPHASIS: u32 = 4;
    /// Dry to processed (0…1).
    pub const MIX: u32 = 5;
    /// Both cells lit by the brighter panel (stereo).
    pub const LINK: u32 = 6;
    /// A sidechain routed in drives the panel's driver instead of the
    /// signal.
    pub const EXTERNAL: u32 = 7;
}

/// Published values.
pub mod value {
    /// The reduction now (dB, ≥ 0; the most of both sides).
    pub const GR: usize = 0;
    /// The output's and the input's mean square over the last block.
    pub const OUT_MS: usize = 1;
    pub const IN_MS: usize = 2;
    /// The panel's light now (1 = the reference drive's; the most of both).
    pub const LIGHT: usize = 3;
}
pub const TAP_VALUES: usize = 4;

pub const MODES: [&str; 2] = ["Compress", "Limit"];
pub const METERS: [&str; 3] = ["Gain Reduction", "+4", "+10"];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::GAIN, "Gain", 0.0, 100.0, DEFAULT_GAIN, None),
        param(
            id::PEAK_REDUCTION,
            "Peak Reduction",
            0.0,
            100.0,
            DEFAULT_PEAK,
            None,
        ),
        stepped(id::MODE, "Compress / Limit", 1.0, 0.0),
        fixed(stepped(id::METER, "Meter", 2.0, 0.0)),
        param(id::EMPHASIS, "HF Emphasis (R37)", 0.0, 1.0, 0.0, Percent),
        param(id::MIX, "Mix", 0.0, 1.0, 1.0, Percent),
        stepped(id::LINK, "Stereo Link", 1.0, 1.0),
        stepped(id::EXTERNAL, "External Sidechain", 1.0, 1.0),
    ]
}

/// The defaults, as the 3-66 manual sets the unit up ("4 to 8 DB of
/// compression continuously", the Gain near the middle): a −18 dBFS signal
/// levelled by about 5 dB and its level kept.
pub const DEFAULT_GAIN: f64 = 50.0;
pub const DEFAULT_PEAK: f64 = 60.0;

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::GAIN | id::PEAK_REDUCTION => format!("{v:.0}"),
        id::MODE => pick(&MODES, v),
        id::METER => pick(&METERS, v),
        id::LINK | id::EXTERNAL => on_off(v),
        _ => return None,
    })
}

/// The audio circuits' oversampling at `rate`: twice below 88.2 kHz.
pub fn oversampling(rate: f64) -> usize {
    if rate < 88_000.0 { 2 } else { 1 }
}

pub fn latency(_params: &ParamValues, rate: f64) -> u32 {
    Oversampler::latency_of(oversampling(rate))
}

// --- levels ----------------------------------------------------------------------

/// Full scale (peak volts): +4 dBm (1.228 V rms) is −18 dBFS.
pub const FULL_SCALE_VOLTS: f64 = 1.227_8 * std::f64::consts::SQRT_2 * 7.943_282_347;
/// The fixed pad ahead of the input, as the 3-66 manual asks: "sufficient
/// fixed pad should be placed ahead of the LA-2A to allow normal output at
/// approximately 50% setting of the gain control" — with its recommended
/// "4 to 8 DB of compression continuously" a −18 dBFS signal then comes
/// out at about its own level with the Gain near the middle (a matched
/// 600 ohm pad: attenuation only).
pub const INPUT_PAD_DB: f64 = 15.0;
/// The source's EMF (peak volts) per unit of full scale at the input: twice
/// the terminals' voltage (a 600 ohm source into a 600 ohm input), less the
/// pad.
pub fn input_volts() -> f64 {
    2.0 * FULL_SCALE_VOLTS * 10f64.powf(-INPUT_PAD_DB / 20.0)
}
/// The source feeding the input (ohms) and the line the output drives: a
/// 600 ohm system, as the unit was built for and its figures are measured.
pub const SOURCE_OHMS: f64 = 600.0;
pub const LOAD_OHMS: f64 = 600.0;

/// A knob (0…100) on an audio-taper pot: the share of the track below its
/// wiper, which is the share of the node above it the wiper passes on (the
/// grid it feeds draws nothing).
pub fn wiper(knob: f64) -> f64 {
    Taper::Audio.fraction((knob / 100.0).clamp(0.0, 1.0))
}

/// R37 (ohms) at the trim's `t` (0…1 of a linear track).
pub fn r37_ohms(t: f64) -> f64 {
    circuit::R37_FLAT + t.clamp(0.0, 1.0) * (circuit::R37_OHMS - circuit::R37_FLAT)
}

/// The resistance the cell sees at node `c` (the rest of the attenuator
/// and the transformer's secondary behind it), in Limit or Compress:
/// `[(R_source ∥ R5) + R6] ∥ R2 + R7`, all of it ∥ R1.
pub fn cell_source_ohms(limit: bool) -> f64 {
    let par = |a: f64, b: f64| a * b / (a + b);
    // The 600 ohm line and T1's primary copper seen through T1, and its
    // secondary copper.
    let r = circuit::T1_RATIO;
    let secondary = (SOURCE_OHMS + 59.0) / (r * r) + 2_400.0;
    let b = par(par(secondary, circuit::R5) + circuit::R6, circuit::POT_OHMS);
    let r7 = if limit {
        circuit::R7_OHMS
    } else {
        circuit::SWITCH_OHMS
    };
    par(b + r7, circuit::POT_OHMS)
}

/// The reduction (dB) a cell of `ohms` makes at node `c`, relative to the
/// cell in the dark: what the T4's second cell shows on the meter.
pub fn reduction_db(ohms: f64, limit: bool) -> f64 {
    let rs = cell_source_ohms(limit);
    let loss = |r: f64| 20.0 * ((rs + r) / r).log10();
    (loss(ohms.max(1.0)) - loss(circuit::CELL_DARK)).max(0.0)
}

// --- the T4 ----------------------------------------------------------------------

/// The panel (see the module notes in [`cell`] and the sources in
/// [`PANEL`]'s fields).
pub const PANEL: PanelSpec = PanelSpec {
    // The Alfrey–Taylor law B = B0·exp(−b/√V) (Zalm 1956; Diemer, Klasens
    // and Zalm, Philips Tech. Rev. 1957; Destriau & Ivey, Proc. IRE 1955).
    // NASA TN D-5979 (Conway 1970) fits b = 56.8 to 61.0 √V (V peak) on thin
    // (42–51 µm) laboratory ZnS:Cu cells; a T4's panel is steeper — the
    // Howard–Ivey–Lehmann data (Destriau & Ivey's Fig. 3) slope 7.5 in
    // log–log at 35–50 V, and the panel voltages measured in a working
    // LA-2A at equal reduction (GroupDIY, "zmix": 70 V rms at 20 Hz against
    // 46 V at 1 kHz) imply b of 100 or more. FITTED at 75, between them: it
    // gives Compress its "approximately 3:1" (UREI's 11-66 addendum).
    alfrey_taylor: 75.0,
    top_volts: 400.0,
    // The brightness against frequency: about in proportion below 1 kHz
    // ("almost a linear function of frequency to 1 kHz", NASA; Philips 1957:
    // "roughly in direct proportion", 10 to 1,000 c/s), sub-linear above —
    // a T4-type cell under 50 V rms (Stroe 2018, Figs 6.8/6.9, the LA-2A
    // replacement panel) gives the light's slope about 0.9 to 1 kHz and 0.5
    // from 1 to 20 kHz; these two corners and shares give 0.95 and 0.55.
    relaxations: [(1_500.0, 0.8), (30_000.0, 0.2)],
    // ZnS:Cu powder EL: light peaks within ~20 µs of the field's edge and
    // falls to half in 100–200 µs (Chimczak et al. 2012, Fig. 3).
    afterglow: 150e-6,
    reference_volts: 100.0,
    reference_hz: 1_000.0,
};

/// The photoresistor. FITTED, in the loop of the whole device, to the
/// LA-2A's published timing (the tests in `tests.rs` hold the result to
/// it): "approximately 0.06 seconds for 50% release, 0.5 to 5 seconds for
/// complete release depending upon amount of previous reduction" (the 3-66
/// specifications, and UA's); the cell "releases quickly (40-80
/// milliseconds) … the remainder … over as much as several seconds" and its
/// release "is slower if the unit has either been in compression for a
/// while, or the amount of compression is large" (UA's manual); "Five or
/// six db of limiting will permit full release in about 2 seconds, while 20
/// or 30 db … 5 to 10 seconds" (Lawrence, J. SMPTE, Aug 1964); attack
/// "10 milliseconds" (UA) — measured on six units, 33–81 ms to settle
/// (Moore, draft AES paper).
pub const CELL: CellSpec = CellSpec {
    // At light 1 the free electrons hold steady at density 1 (without
    // traps): what they recombine at.
    generation: 1.0 / LIFETIME + 1.0 / PAIRS,
    // The two recombinations cross at density (PAIRS / LIFETIME)³ = 0.027:
    // below it the conductance follows the light, above it — wherever the
    // T4 reduces by more than a decibel or so — its 0.75 power, CdS's 0.7–0.9
    // (Bube 1955: "values of n in the range between 0.7 and 0.9 are very
    // common"; Rose 1951: "around 0.8 extending over three orders of
    // magnitude of light intensity"). Together they set the fast stage:
    // half the release in 45–65 ms.
    lifetime: LIFETIME,
    pairs: PAIRS,
    // Shallow, medium and deep traps (Bube 1955: "an initial rapid portion
    // corresponding to the direct recombination of free electrons, and a
    // subsequent, much slower portion corresponding to … electrons which
    // have been thermally freed from traps"; Rose 1955: the rise and the
    // decay are both "extended by the ratio of shallow-trapped to free
    // carriers"). The deep level fills only over seconds of bright light and
    // empties over seconds: the memory.
    traps: [
        Trap {
            density: 0.3,
            capture: 6.0,
            release: 0.3,
        },
        Trap {
            density: 0.6,
            capture: 1.5,
            release: 1.2,
        },
        Trap {
            density: 1.5,
            capture: 0.6,
            release: 6.0,
        },
    ],
    // Dark: "many megohms in darkness" (Lawrence's patent, US 3,258,707);
    // PerkinElmer's cells 0.5–20 M five seconds after 2 fc. Lit: 1.2 k at
    // light 1 — about 29 dB of reduction — and a few hundred ohms at the
    // panel's brightest ("only a few hundred ohms when excited", the
    // patent), which the 6AQ5's swing reaches near the specification's
    // "up to 40 dB".
    dark: 1.0 / circuit::CELL_DARK,
    per_density: 1.0 / 1_200.0,
};
const LIFETIME: f64 = 0.1;
const PAIRS: f64 = 0.03;

// --- the processor ----------------------------------------------------------------

/// One side: its three circuits, the oversampler around the audio ones and
/// its T4.
struct Side {
    input: Simulation,
    amp: Simulation,
    sidechain: Simulation,
    over: Oversampler,
    panel: Panel,
    cell: Photocell,
    /// Nodes `b` and `c` in the input circuit.
    b: usize,
    c: usize,
    /// R37 as last set (ohms).
    r37: f64,
}

/// The T4's constants (the processor's are [`PANEL`] and [`CELL`]).
#[derive(Clone, Copy, Debug)]
pub struct T4 {
    pub panel: PanelSpec,
    pub cell: CellSpec,
}

impl T4 {
    pub const FITTED: T4 = T4 {
        panel: PANEL,
        cell: CELL,
    };
}

/// A side's circuit settings when it is made: R37 (ohms) and SW3.
#[derive(Clone, Copy)]
struct Controls {
    r37: f64,
    limit: bool,
}

impl Side {
    fn new(rate: f64, k: Controls, t4: T4) -> Result<Self, PluginError> {
        let fail = |e: faderframe_circuit::dsp::netlist::Fault| {
            PluginError::Failed(format!("2A Leveler: {e}"))
        };
        let factor = oversampling(rate);
        let inner = rate * factor as f64;
        let input = circuit::input(SOURCE_OHMS).map_err(fail)?;
        let (b, c) = (
            input.unknown_named("b").unwrap_or(0),
            input.unknown_named("c").unwrap_or(0),
        );
        let mut s = Self {
            input: Simulation::new(input, rate),
            amp: Simulation::new(circuit::amplifier(LOAD_OHMS).map_err(fail)?, inner),
            sidechain: Simulation::new(circuit::sidechain().map_err(fail)?, rate),
            over: Oversampler::new(factor),
            panel: Panel::new(t4.panel, rate),
            cell: Photocell::new(t4.cell, rate),
            b,
            c,
            r37: f64::NAN,
        };
        for sim in [&mut s.input, &mut s.amp, &mut s.sidechain] {
            // As the preamps: one Newton pass where a second would only
            // confirm it (see `Simulation::set_early_acceptance`).
            sim.set_early_acceptance(true);
        }
        s.set_r37(k.r37);
        s.set_limit(k.limit);
        s.input.set_realtime_value(circuit::CELL, s.cell.ohms());
        for sim in [&mut s.input, &mut s.amp, &mut s.sidechain] {
            sim.reset();
        }
        Ok(s)
    }

    /// R37, a trim: the sidechain is rebuilt (once) when it has moved by
    /// more than a few ohms.
    fn set_r37(&mut self, r37: f64) {
        if (r37 - self.r37).abs() > 5.0 || self.r37.is_nan() {
            self.sidechain.set_value(circuit::R37_SLOT, r37);
            self.r37 = r37;
        }
    }

    /// SW3 (a switch: the input circuit is rebuilt once).
    fn set_limit(&mut self, limit: bool) {
        self.input.set_value(
            circuit::R7_SLOT,
            if limit {
                circuit::R7_OHMS
            } else {
                circuit::SWITCH_OHMS
            },
        );
    }

    /// One host sample of the source's EMF through the audio circuits, R1's
    /// wiper at `gain` of node `c`: the line (volts) and node `b` (the
    /// sidechain's tap). The input circuit (T1's iron, nearly linear at the
    /// levels it sees, and the attenuator) runs at the host's rate; the
    /// valves and T2 after the wiper at the oversampled one.
    #[inline]
    fn audio(&mut self, emf: f64, gain: f64) -> (f64, f64) {
        self.input
            .set_realtime_value(circuit::CELL, self.cell.ohms());
        self.input.process(emf);
        let wiper = gain * self.input.voltage_at(self.c);
        let amp = &mut self.amp;
        let line = self.over.process(wiper, &mut |u| amp.process(u));
        (line, self.input.voltage_at(self.b))
    }

    /// The sidechain for `drive` (volts at R2's wiper): the panel's light.
    #[inline]
    fn drive(&mut self, drive: f64) -> f64 {
        let volts = self.sidechain.process(drive);
        self.panel.step(volts)
    }

    fn copy_from(&mut self, other: &Self) {
        self.input.copy_runtime_state_from(&other.input);
        self.amp.copy_runtime_state_from(&other.amp);
        self.sidechain.copy_runtime_state_from(&other.sidechain);
        self.over.copy_runtime_state_from(&other.over);
        self.panel.clone_from(&other.panel);
        self.cell = other.cell;
        self.r37 = other.r37;
    }

    fn reset(&mut self) {
        self.input.reset_deferred();
        self.amp.reset_deferred();
        self.sidechain.reset_deferred();
        self.over.reset();
        self.panel.reset();
        self.cell.reset();
    }
}

pub struct LevelerProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sides: [Side; 2],
    /// While a stereo pair is exactly mono only the first side is solved;
    /// once they differ the second wakes from its exact state.
    stereo_seen: bool,
    limit: bool,
    /// The Gain and Peak Reduction knobs (0…100), gliding.
    gain: Smoothed,
    peak: Smoothed,
    mix: Smoothed,
    dry: [DelayLine; 2],
    delay: usize,
    ms_in: MeanSquare,
    ms_out: MeanSquare,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
    key: [Vec<f32>; 2],
}

impl LevelerProcessor {
    pub fn new(
        params: ParamValues,
        tap: Arc<AnalysisTap>,
        config: &ProcessConfig,
    ) -> Result<Self, PluginError> {
        Self::with_t4(params, tap, config, T4::FITTED)
    }

    /// With the T4's constants `t4` (the tests try others).
    pub fn with_t4(
        params: ParamValues,
        tap: Arc<AnalysisTap>,
        config: &ProcessConfig,
        t4: T4,
    ) -> Result<Self, PluginError> {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let get = |id: u32| f64::from(params.get(id as usize));
        let limit = get(id::MODE) >= 0.5;
        let (gain, peak) = (get(id::GAIN), get(id::PEAK_REDUCTION));
        let k = Controls {
            r37: r37_ohms(get(id::EMPHASIS)),
            limit,
        };
        let sides = [Side::new(sr, k, t4)?, Side::new(sr, k, t4)?];
        let delay = latency(&params, sr) as usize;
        let mt = MeterTap::new(sr as f32);
        let smooth = |v: f64| {
            let mut s = Smoothed::new(v, 20.0, sr);
            s.snap();
            s
        };
        Ok(Self {
            watching: Watching::new(sr as f32),
            sides,
            stereo_seen: false,
            limit,
            gain: smooth(gain),
            peak: smooth(peak),
            mix: smooth(get(id::MIX).clamp(0.0, 1.0)),
            dry: [DelayLine::new(delay + 1), DelayLine::new(delay + 1)],
            delay,
            ms_in: MeanSquare::new(50.0, sr),
            ms_out: MeanSquare::new(50.0, sr),
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            key: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        })
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(pid as usize))
    }
}

impl PluginProcessor for LevelerProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let side_in = io.audio_in.get(1).filter(|k| k.num_channels() > 0);
        let n = io.frames.min(self.scratch[0].len());
        let external = self.get(id::EXTERNAL) >= 0.5 && side_in.is_some();
        if let Some(k) = side_in.filter(|_| external) {
            for c in 0..2 {
                let src = k.channel(c.min(k.num_channels() - 1));
                self.key[c][..n].copy_from_slice(&src[..n]);
            }
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
        let watched = self.watching.check(&self.tap, n);
        let sides = channels.min(2);
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        // The controls.
        let limit = self.get(id::MODE) >= 0.5;
        if limit != self.limit {
            self.limit = limit;
            for s in &mut self.sides {
                s.set_limit(limit);
            }
        }
        self.gain.set(self.get(id::GAIN).clamp(0.0, 100.0));
        self.peak
            .set(self.get(id::PEAK_REDUCTION).clamp(0.0, 100.0));
        self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
        // R37 is a trim: set once a block (the sidechain is rebuilt when it
        // has moved).
        let r37 = r37_ohms(self.get(id::EMPHASIS));
        for s in &mut self.sides {
            s.set_r37(r37);
        }
        let link = self.get(id::LINK) >= 0.5 && sides == 2;
        // A stereo pair that is still exactly mono (and keyed the same) is
        // solved once; the first block that differs wakes the second side
        // from the first's exact state.
        let mut active = sides;
        if sides == 2 && !self.stereo_seen {
            let same = self.scratch[0][..n] == self.scratch[1][..n]
                && (!external || self.key[0][..n] == self.key[1][..n]);
            if same {
                active = 1;
            } else {
                let (a, b) = self.sides.split_at_mut(1);
                b[0].copy_from(&a[0]);
                self.stereo_seen = true;
            }
        }
        let rs = cell_source_ohms(limit);
        let rs_dark = (rs + circuit::CELL_DARK) / circuit::CELL_DARK;
        let (into, key_volts) = (input_volts(), key_volts());
        let mut most_gr = 0.0f64;
        let mut most_light = 0.0f64;
        for i in 0..n {
            let (g, p, mix) = (
                wiper(self.gain.tick()),
                wiper(self.peak.tick()),
                self.mix.tick(),
            );
            let mut light = [0.0f64; 2];
            let mut wet = [0.0f64; 2];
            for (c, s) in self.sides.iter_mut().enumerate().take(active) {
                let x = f64::from(self.scratch[c][i]);
                let (line, tap) = s.audio(x * into, g);
                wet[c] = line / FULL_SCALE_VOLTS;
                // The external key in the place of node `b`: at the level the
                // input would put there with the cell dark.
                let drive = if external {
                    f64::from(self.key[c][i]) * key_volts
                } else {
                    tap
                };
                light[c] = s.drive(p * drive);
            }
            if active == 1 {
                light[1] = light[0];
                wet[1] = wet[0];
            }
            let shared = light[0].max(light[1]);
            for (c, s) in self.sides.iter_mut().enumerate().take(active) {
                let l = if link { shared } else { light[c] };
                s.cell.step(l);
                let ohms = s.cell.ohms();
                // The meter's reading, as `reduction_db` (inline).
                let gr = 20.0 * ((rs + ohms) / ohms).log10() - 20.0 * rs_dark.log10();
                most_gr = most_gr.max(gr);
            }
            most_light = most_light.max(shared);
            for (c, wet) in wet.iter().enumerate().take(sides) {
                let x = f64::from(self.scratch[c][i]);
                self.dry[c].push(x);
                let dry = self.dry[c].tap(self.delay);
                let y = dry * (1.0 - mix) + wet * mix;
                out.channel_mut(c)[i] = y as f32;
                self.ms_in.process(x);
                self.ms_out.process(y);
            }
        }
        self.ms_in.flush();
        self.ms_out.flush();
        self.tap.set_value(value::GR, most_gr.max(0.0) as f32);
        self.tap.set_value(value::OUT_MS, self.ms_out.value as f32);
        self.tap.set_value(value::IN_MS, self.ms_in.value as f32);
        self.tap.set_value(value::LIGHT, most_light as f32);
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
        for s in &mut self.sides {
            s.reset();
        }
        self.stereo_seen = false;
        self.ms_in.reset();
        self.ms_out.reset();
    }
}

/// An external key's volts at node `b` per unit of full scale: what the
/// input puts there with the cell in the dark (the attenuator's share of
/// T1's secondary in a 600 ohm system), so a key as loud as the signal
/// drives the panel as hard.
fn key_volts() -> f64 {
    input_volts() * KEY_SHARE
}
/// Node `b` over the source's EMF with the cell dark, at 1 kHz (measured
/// on the circuit, `tests::the_external_key_is_scaled_as_the_input`).
const KEY_SHARE: f64 = 1.67;
