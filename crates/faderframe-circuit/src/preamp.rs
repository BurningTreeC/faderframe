//! Calibrated microphone channels. Gain moves the actual circuit control;
//! Master trims the digital output. Each audio channel owns independent state.
use crate::circuits::{american312, british_47, console_e, german_76, neve, tube610};
use crate::dsp::{netlist::Fault, oversample::Oversampler, time::Simulation};

pub const OVERSAMPLING: usize = 2;
pub const MODELS: usize = 6;
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

pub struct Preamp {
    circuit: Simulation,
    line: Option<Simulation>,
    over: Oversampler,
    model: usize,
    input_scale: f64,
    makeup_scale: f64,
    output_scale: f64,
    output_target: f64,
    smoothing: f64,
    gain: f64,
    master: f64,
}

impl Preamp {
    /// Constructs and settles off the audio thread. Models follow the catalogue
    /// order: British 73, American 312, British 4K E, Tube 610, British 47, German 76.
    pub fn new(model: usize, rate: f64, gain: f64, master_db: f64) -> Result<Self, Fault> {
        let model = model.min(MODELS - 1);
        let net = match model {
            0 => neve::build(150.0, 10_000.0),
            1 => american312::build(150.0, 10_000.0),
            2 => console_e::build(150.0, 10_000.0),
            3 => tube610::build(50.0, 600.0),
            4 => british_47::build(200.0, 200.0),
            _ => german_76::build(200.0, 300.0),
        }?;
        let rate = rate.max(1.0);
        let mut circuit = Simulation::new(net, rate * OVERSAMPLING as f64);
        circuit.set_control(0, gain.clamp(0.0, 1.0));
        circuit.reset();
        let line = if model == 0 {
            Some(Simulation::new(
                neve::output(470_000.0, 10_000.0)?,
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
            makeup_scale: 1.0,
            output_scale: 0.0,
            output_target: 0.0,
            smoothing: 1.0 - (-1.0 / (rate * 0.01)).exp(),
            gain: -1.0,
            master: f64::NAN,
        };
        p.set_controls(gain, master_db);
        p.output_scale = p.output_target;
        Ok(p)
    }

    pub fn latency() -> u32 {
        Oversampler::latency_of(OVERSAMPLING)
    }

    pub fn set_controls(&mut self, gain: f64, master_db: f64) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            0.5
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
        self.circuit.set_control(0, gain);
        let cal = &CALIBRATION[self.model];
        // The 610's control is its physical interstage Level pot. Cancelling
        // its attenuation with automatic makeup boosts the C15 feedthrough
        // when the pot is closed, making zero brighter and louder. Keep the
        // default setting's calibration and let this pot determine level.
        let calibration_gain = if self.model == 3 { 0.5 } else { gain };
        let db = makeup(
            &cal.make_up_db,
            calibration_gain,
            self.circuit.control_steps(0),
        );
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
        let out = self.over.process(input * self.input_scale, &mut |x| {
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
    }
}
