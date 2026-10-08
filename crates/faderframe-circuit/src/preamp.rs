//! Calibrated microphone channels. Gain moves the actual circuit control
//! (and the level with it, as on the hardware: −18 dBFS in comes out near
//! −20 dBFS at the middle of its travel); Master trims the digital output.
//! Each audio channel owns independent state.
use crate::circuits::{american312, british_47, console_bus, console_e, german_76, neve, tube610};
use crate::dsp::{netlist::Fault, oversample::Oversampler, time::Simulation};

pub const OVERSAMPLING: usize = 2;
pub const MODELS: usize = 6;
/// Console mix buses after the microphone models: model `MODELS + f` is
/// family `f` of [`console_bus::FAMILIES`].
pub const CONSOLE_BUSES: usize = console_bus::FAMILIES.len();

/// A console bus's line at full scale (peak volts): −18 dBFS is +4 dBu, so
/// 0 dBFS is +22 dBu.
pub const BUS_FULL_SCALE: f64 = 1.227_8 * std::f64::consts::SQRT_2 * 7.943_282;

/// A console bus's drive range (dB either way; its Gain control is the
/// drive in dB).
pub const BUS_DRIVE_DB: f64 = 12.0;
struct Calibration {
    drive_volts: f64,
    make_up_db: [f64; 33],
}
include!("calibration.rs");

fn makeup(table: &[f64; 33], gain: f64, steps: Option<usize>) -> f64 {
    let position = |i: usize| (i as f64 / 32.0).powi(3);
    if let Some(n) = steps.filter(|n| *n > 1) {
        let step = |x: f64| ((x * n as f64) as usize).min(n - 1);
        if let Some(i) = (0..33)
            .filter(|i| step(position(*i)) == step(gain))
            .min_by(|a, b| {
                (position(*a) - gain)
                    .abs()
                    .total_cmp(&(position(*b) - gain).abs())
            })
        {
            return table[i];
        }
    }
    let x = gain.cbrt() * 32.0;
    let i = (x as usize).min(31);
    table[i] + (table[i + 1] - table[i]) * (x - i as f64)
}

/// Where on its travel the Gain leaves the level as it came in.
pub const REFERENCE: f64 = 0.5;

/// The British 73's output block is driven from the card's output trim (VR3
/// at mid travel in series with VR2 near its top, over R67): about 2.5 k
/// (ESTIMATED from those settings). It does the 18 dB of gain the guide
/// states, so it is the stage that runs out first.
pub const LINE_SOURCE: f64 = 2_500.0;
/// The line the British 73 drives: 600 ohms, what the block was built for
/// (terminated, the default), or a bridging 10 k input (its estimated
/// leakage then rings against C26 a little: +1.6 dB at 20 kHz, and 8 dB
/// more swing before it clips).
pub const TERMINATED: f64 = 600.0;
pub const BRIDGING: f64 = 10_000.0;
/// How much louder the British 73 is into a bridging input (its
/// calibration is the terminated one's): taken off so the level stays.
pub const BRIDGING_DB: f64 = 0.93;

/// Model `model`'s circuit (catalogue order).
fn netlist(model: usize) -> Result<crate::dsp::netlist::Circuit, Fault> {
    match model {
        0 => neve::build(150.0, 10_000.0),
        1 => american312::build(150.0, 10_000.0),
        2 => console_e::build(150.0, 10_000.0),
        3 => tube610::build(50.0, 600.0),
        4 => british_47::build(200.0, 200.0),
        _ => german_76::build(200.0, 300.0),
    }
}

/// The British 73's gain switch pads the input at its lower positions (a
/// resistive divider of up to 35 dB, see `circuits::neve`), which the
/// circuit, wired as the 70 dB position, does not have: the lower half of
/// the Gain's travel is that divider, even in dB, so the Gain spans the
/// switch's range rather than only the feedback leg's 10 dB.
const BRITISH73_DIVIDER_DB: f64 = 35.0;

/// The input divider (dB, ≤ 0) of model `model` at `gain`.
fn divider_db(model: usize, gain: f64) -> f64 {
    if model == 0 && gain < REFERENCE {
        -BRITISH73_DIVIDER_DB * (1.0 - gain / REFERENCE)
    } else {
        0.0
    }
}

/// The bottom of the Gain's travel closes the input, gradually (square
/// law, like a level pot: −12 dB at half of it, −24 at a quarter), so 0 %
/// is silence on every model. The Tube 610's Level pot closes by itself
/// (to about −80 dBFS): only its last 2 % finish the way.
fn closing(model: usize) -> f64 {
    if model == 3 {
        0.02
    } else {
        0.2
    }
}

/// What reaches the circuit (linear): the divider and the closing.
fn pad(model: usize, gain: f64) -> f64 {
    let end = closing(model);
    let closing = if gain < end {
        (gain.max(0.0) / end).powi(2)
    } else {
        1.0
    };
    10f64.powf(divider_db(model, gain) / 20.0) * closing
}

/// How much louder (dB) model `model` is with its Gain at `gain` than at
/// [`REFERENCE`]: the circuit's own gain change (from its calibration), its
/// input divider and the closing at the bottom (−200 dB for silence).
pub fn level_change_db(model: usize, gain: f64) -> f64 {
    let model = model.min(MODELS - 1);
    let table = &CALIBRATION[model].make_up_db;
    let steps = netlist(model).ok().and_then(|c| c.control_steps(0));
    let gain = if gain.is_finite() {
        gain.clamp(0.0, 1.0)
    } else {
        REFERENCE
    };
    makeup(table, REFERENCE, steps) - makeup(table, gain, steps)
        + (20.0 * pad(model, gain).log10()).max(-200.0)
}

pub struct Preamp {
    circuit: Simulation,
    line: Option<Simulation>,
    over: Oversampler,
    model: usize,
    input_scale: f64,
    /// The input divider (linear), smoothed toward its target.
    pad: f64,
    pad_target: f64,
    makeup_scale: f64,
    output_scale: f64,
    output_target: f64,
    smoothing: f64,
    gain: f64,
    master: f64,
    /// A console bus: the circuit's small-signal gain at 1 kHz (its sign
    /// the polarity), measured once.
    bus: Option<f64>,
    /// The British 73 into a bridging input (else terminated).
    bridging: bool,
}

impl Preamp {
    /// Console bus `family` (see [`console_bus`]): Gain is the drive
    /// (dB, ±[`BUS_DRIVE_DB`]), Master the output trim. Levels are set so that
    /// −18 dBFS leaves the bus's line at +4 dBu, the drive putting more in
    /// and taking it off after.
    fn console_bus(family: usize, rate: f64, drive: f64, master_db: f64) -> Result<Self, Fault> {
        let family = family.min(CONSOLE_BUSES - 1);
        let net = console_bus::build(family, 10_000.0)?;
        let inner = rate.max(1.0) * OVERSAMPLING as f64;
        let mut circuit = Simulation::new(net, inner);
        if let Some(c) = console_bus::control(family) {
            circuit.set_control(0, c);
        }
        circuit.reset();
        // The gain at 1 kHz at a small level (a valve booster's 34 dB and
        // more turn a tenth of a volt in into its nonlinear range): a
        // millivolt, less if that comes out over 0.3 V.
        let mut amplitude = 1e-3;
        let (mut gain, mut f) = (1.0, crate::dsp::complex::C::ZERO);
        for _ in 0..2 {
            circuit.reset();
            let tone = crate::dsp::measure::Tone::near(inner, 1920, 1000.0, amplitude);
            let m = crate::dsp::measure::run(tone, 9600, |x| circuit.process(x));
            f = m.fundamental();
            gain = f.magnitude() / tone.amplitude;
            if gain * amplitude <= 0.3 {
                break;
            }
            amplitude = 0.1 / gain.max(1e-9);
        }
        // Its polarity: the window starts on a whole number of periods, so
        // the input's own bin is (0, −A); in phase the output's is too.
        let sign = if f.im <= 0.0 { 1.0 } else { -1.0 };
        circuit.reset();
        let mut p = Self {
            circuit,
            line: None,
            over: Oversampler::new(OVERSAMPLING),
            model: MODELS + family,
            input_scale: 1.0,
            pad: 1.0,
            pad_target: 1.0,
            makeup_scale: 1.0,
            output_scale: 0.0,
            output_target: 0.0,
            smoothing: 1.0 - (-1.0 / (rate.max(1.0) * 0.01)).exp(),
            gain: -1.0,
            master: f64::NAN,
            bus: Some(sign * gain.max(1e-6)),
            bridging: false,
        };
        p.set_controls(drive, master_db);
        p.output_scale = p.output_target;
        Ok(p)
    }

    /// Constructs and settles off the audio thread. Models follow the catalogue
    /// order: British 73, American 312, British 4K E, Tube 610, British 47, German 76.
    pub fn new(model: usize, rate: f64, gain: f64, master_db: f64) -> Result<Self, Fault> {
        Self::with_line(model, rate, gain, master_db, false)
    }

    /// [`Self::new`], the British 73 into a bridging input when `bridging`
    /// (see [`BRIDGING`]).
    pub fn with_line(
        model: usize,
        rate: f64,
        gain: f64,
        master_db: f64,
        bridging: bool,
    ) -> Result<Self, Fault> {
        if model >= MODELS {
            return Self::console_bus(model - MODELS, rate, gain, master_db);
        }
        let model = model.min(MODELS - 1);
        let net = netlist(model)?;
        let rate = rate.max(1.0);
        let mut circuit = Simulation::new(net, rate * OVERSAMPLING as f64);
        circuit.set_control(0, gain.clamp(0.0, 1.0));
        circuit.reset();
        let line = if model == 0 {
            Some(Simulation::new(
                neve::output(LINE_SOURCE, if bridging { BRIDGING } else { TERMINATED })?,
                rate * OVERSAMPLING as f64,
            ))
        } else {
            None
        };
        let mut p = Self {
            circuit,
            line,
            over: Oversampler::new(OVERSAMPLING),
            model,
            input_scale: CALIBRATION[model].drive_volts / 10f64.powf(-18.0 / 20.0),
            pad: 1.0,
            pad_target: 1.0,
            makeup_scale: 1.0,
            output_scale: 0.0,
            output_target: 0.0,
            smoothing: 1.0 - (-1.0 / (rate * 0.01)).exp(),
            gain: -1.0,
            master: f64::NAN,
            bus: None,
            bridging: bridging && model == 0,
        };
        p.set_controls(gain, master_db);
        p.output_scale = p.output_target;
        p.pad = p.pad_target;
        Ok(p)
    }

    pub fn latency() -> u32 {
        Oversampler::latency_of(OVERSAMPLING)
    }

    /// Gain (0…1; a console bus: its drive in dB) and Master (dB).
    pub fn set_controls(&mut self, gain: f64, master_db: f64) {
        let (low, high, rest) = if self.bus.is_some() {
            (-BUS_DRIVE_DB, BUS_DRIVE_DB, 0.0)
        } else {
            (0.0, 1.0, REFERENCE)
        };
        let gain = if gain.is_finite() {
            gain.clamp(low, high)
        } else {
            rest
        };
        let master = if master_db.is_finite() {
            master_db.clamp(-60.0, 12.0)
        } else {
            0.0
        };
        if gain == self.gain && master == self.master {
            return;
        }
        self.gain = gain;
        self.master = master;
        if let Some(g) = self.bus {
            // The drive into the bus and off it after: the colour changes,
            // not the level.
            let drive = 10f64.powf(gain / 20.0);
            self.input_scale = BUS_FULL_SCALE * drive / g.abs();
            self.makeup_scale = g.signum() / (BUS_FULL_SCALE * drive);
            self.output_target = 10f64.powf(master / 20.0);
            return;
        }
        self.circuit.set_control(0, gain);
        let cal = &CALIBRATION[self.model];
        // Gain is the circuit's own, as on the hardware: the level follows
        // the circuit's gain, from the least to the most it has (the 610's
        // Level pot closing to silence), calibrated at the middle of its
        // travel. (Following the gain with the makeup instead made it a
        // drive control, and on the 610 boosted the C15 feedthrough when
        // the pot closed.)
        let db = makeup(&cal.make_up_db, REFERENCE, self.circuit.control_steps(0))
            - if self.bridging { BRIDGING_DB } else { 0.0 };
        self.pad_target = pad(self.model, gain);
        // Circuit gain and its calibration must change together. Smoothing
        // their product held the old (sometimes +70 dB) correction while the
        // new circuit gain was already active. Only the independent Master
        // trim is smoothed; calibration occurs before the decimator so its
        // filter history always contains correctly scaled samples.
        self.makeup_scale = 10f64.powf(db / 20.0) / self.input_scale;
        self.output_target = 10f64.powf(master / 20.0);
    }

    pub fn process(&mut self, input: f64) -> f64 {
        let input = if input.is_finite() { input } else { 0.0 };
        let circuit = &mut self.circuit;
        let line = &mut self.line;
        let makeup = self.makeup_scale;
        self.pad += self.smoothing * (self.pad_target - self.pad);
        let out = self
            .over
            .process(input * self.input_scale * self.pad, &mut |x| {
                let y = circuit.process(x);
                let y = match line {
                    Some(sim) => sim.process(y),
                    None => y,
                };
                y * makeup
            });
        self.output_scale += self.smoothing * (self.output_target - self.output_scale);
        let out = out * self.output_scale;
        if out.is_finite() {
            out
        } else {
            0.0
        }
    }

    pub fn reset(&mut self) {
        self.circuit.reset_deferred();
        if let Some(sim) = &mut self.line {
            sim.reset_deferred();
        }
        self.over.reset();
        self.output_scale = self.output_target;
        self.pad = self.pad_target;
    }

    /// When the current audio is due (live use): past it, samples that do
    /// not settle stop after a few passes instead of the full recovery
    /// (half-step rescue), so a block of hard transients cannot take
    /// several times its duration. `None` (rendering): always the full
    /// solve.
    pub fn set_realtime_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.circuit.set_realtime_deadline(deadline);
        if let Some(sim) = &mut self.line {
            sim.set_realtime_deadline(deadline);
        }
    }

    /// Samples whose recovery the realtime deadline cut short.
    pub fn deadline_aborts(&self) -> u64 {
        self.circuit.deadline_aborts() + self.line.as_ref().map_or(0, Simulation::deadline_aborts)
    }

    /// Solver work so far, both circuits together: (solves, Newton passes,
    /// samples left unsettled, rebuilds).
    pub fn solver_statistics(&self) -> (u64, u64, u64, u64) {
        let (a, b, c, d) = self.circuit.statistics();
        let (e, f, g, h) = self
            .line
            .as_ref()
            .map_or((0, 0, 0, 0), Simulation::statistics);
        (a + e, b + f, c + g, d + h)
    }

    /// Wake an identically configured channel from this stream's exact history.
    /// Both channels must have received the same controls. Copies into existing
    /// storage, including solver caches and resampler history; never allocates.
    pub fn copy_runtime_state_from(&mut self, source: &Self) {
        assert_eq!(self.model, source.model);
        debug_assert_eq!(self.gain, source.gain);
        debug_assert_eq!(self.master, source.master);
        self.circuit.copy_runtime_state_from(&source.circuit);
        if let (Some(dst), Some(src)) = (&mut self.line, &source.line) {
            dst.copy_runtime_state_from(src);
        }
        self.over.copy_runtime_state_from(&source.over);
        self.output_scale = source.output_scale;
        self.pad = source.pad;
    }
}
