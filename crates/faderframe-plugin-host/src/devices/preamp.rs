//! Dedicated microphone input processors, backed by the reusable circuit solver.
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginError, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_circuit::preamp::Preamp;
use faderframe_realtime::{PoolConfig, PoolJob, TryCell, WorkerPool};
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};

mod buffered;
pub use buffered::{BUFFER_LATENCY, BufferedPreampProcessor, buffer_delay};

pub const GAIN: u32 = 0;
pub const MASTER: u32 = 1;
/// The British 73's line: 0 = 600 ohms (terminated), 1 = a bridging 10 k
/// input (see `faderframe_circuit::preamp::BRIDGING`).
pub const LOAD: u32 = 2;
pub fn parameters() -> Vec<ParameterInfo> {
    vec![
        super::param(GAIN, "Gain", 0.0, 1.0, 0.5, ParameterUnit::Percent),
        super::param(MASTER, "Master", -60.0, 12.0, 0.0, ParameterUnit::Decibels),
    ]
}

/// Model `model`'s parameters: the British 73 also has its Output Load.
pub fn parameters_for(model: usize) -> Vec<ParameterInfo> {
    let mut p = parameters();
    if model == 0 {
        p.push(super::stepped(LOAD, "Output Load", 1.0, 0.0));
    }
    p
}

/// Whether model `model` with `params` drives a bridging input.
pub fn bridging(model: usize, params: &ParamValues) -> bool {
    model == 0 && params.get(LOAD as usize) >= 0.5
}

/// A console bus amplifier's: the drive into it (dB; taken off after it,
/// so it changes the colour, not the level) and its output.
pub fn bus_parameters() -> Vec<ParameterInfo> {
    let drive = faderframe_circuit::preamp::BUS_DRIVE_DB;
    vec![
        super::param(GAIN, "Drive", -drive, drive, 0.0, ParameterUnit::Decibels),
        super::param(MASTER, "Output", -24.0, 12.0, 0.0, ParameterUnit::Decibels),
    ]
}

/// Channels solved in one pass of the helpers (more follow in further
/// passes).
const BATCH: usize = 64;

/// The circuits of a track's channels. Each channel's circuit is
/// independent, so they are solved side by side: the calling thread and
/// helper threads (which adopt its scheduling, see [`WorkerPool`]) take a
/// channel each, and the result is what one thread would compute. Like
/// GainStageFx, one circuit runs while stereo input is exactly mono; once
/// the channels differ its state is copied into the dormant one, and both
/// keep their tails until a reset.
pub(crate) struct PreampBank {
    circuits: Vec<TryCell<Preamp>>,
    stereo_seen: bool,
    helpers: Option<WorkerPool>,
}

impl PreampBank {
    /// Builds and settles the circuits (and starts the helpers) off the
    /// audio thread.
    pub(crate) fn new(
        model: usize,
        config: &ProcessConfig,
        channels: usize,
        gain: f64,
        master: f64,
        bridging: bool,
    ) -> Result<Self, PluginError> {
        let circuits = (0..channels)
            .map(|_| {
                Preamp::with_line(model, config.sample_rate, gain, master, bridging)
                    .map(TryCell::new)
                    .map_err(|e| PluginError::Failed(format!("Microphone preamp: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let helpers = (channels > 1).then(|| {
            let cores = faderframe_realtime::physical_cores()
                .saturating_sub(1)
                .max(1);
            WorkerPool::new(PoolConfig::new((channels - 1).min(cores).min(BATCH - 1)))
        });
        Ok(Self {
            circuits,
            stereo_seen: false,
            helpers,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.circuits.len()
    }

    /// How many of `channels` this block solves: one while a stereo pair is
    /// still exactly mono (the second is then a copy), all of them once it
    /// is not — waking the second from the first's exact history.
    pub(crate) fn active(&mut self, channels: &[&mut [f32]]) -> usize {
        let count = self.circuits.len().min(channels.len());
        if !self.stereo_seen && count == 2 {
            if channels[0] == channels[1] {
                return 1;
            }
            let (left, right) = self.circuits.split_at_mut(1);
            right[0]
                .get_mut()
                .copy_runtime_state_from(left[0].get_mut());
            self.stereo_seen = true;
        }
        count
    }

    /// Frames `range` of `channels` in place, frame `i` with the controls
    /// `gain[i]` and `master[i]`; `active` from [`Self::active`].
    pub(crate) fn process(
        &mut self,
        channels: &mut [&mut [f32]],
        active: usize,
        range: Range<usize>,
        gain: &[f32],
        master: &[f32],
    ) {
        let count = self.circuits.len().min(channels.len());
        let active = active.min(count);
        // Dormant circuits follow the controls, so that waking one copies
        // state between identically configured circuits.
        for circuit in &mut self.circuits[active..count] {
            let circuit = circuit.get_mut();
            for i in range.clone() {
                circuit.set_controls(f64::from(gain[i]), f64::from(master[i]));
            }
        }
        let (gain, master) = (&gain[range.clone()], &master[range.clone()]);
        match &self.helpers {
            Some(helpers) if active > 1 => {
                for start in (0..active).step_by(BATCH) {
                    let n = (active - start).min(BATCH);
                    let mut samples: [TryCell<Option<&mut [f32]>>; BATCH] =
                        std::array::from_fn(|_| TryCell::new(None));
                    for (cell, channel) in samples.iter_mut().zip(&mut channels[start..start + n]) {
                        *cell.get_mut() = Some(&mut channel[range.clone()]);
                    }
                    let job = Solve {
                        circuits: &self.circuits[start..start + n],
                        samples: &samples[..n],
                        next: AtomicUsize::new(0),
                        gain,
                        master,
                    };
                    helpers.run(&job, n - 1);
                }
            }
            _ => {
                for (circuit, channel) in self
                    .circuits
                    .iter_mut()
                    .zip(channels.iter_mut())
                    .take(active)
                {
                    solve(circuit.get_mut(), &mut channel[range.clone()], gain, master);
                }
            }
        }
        if active == 1 && count == 2 {
            let (left, right) = channels.split_at_mut(1);
            right[0][range.clone()].copy_from_slice(&left[0][range]);
        }
    }

    pub(crate) fn reset(&mut self) {
        self.circuits.iter_mut().for_each(|c| c.get_mut().reset());
        self.stereo_seen = false;
    }

    /// When the audio being solved is due (live): see
    /// [`Preamp::set_realtime_deadline`].
    pub(crate) fn set_deadline(&mut self, deadline: Option<std::time::Instant>) {
        for c in &mut self.circuits {
            c.get_mut().set_realtime_deadline(deadline);
        }
    }
}

/// One pass of the helpers: each thread takes the next channel.
struct Solve<'a, 'b> {
    circuits: &'a [TryCell<Preamp>],
    samples: &'a [TryCell<Option<&'b mut [f32]>>],
    next: AtomicUsize,
    gain: &'a [f32],
    master: &'a [f32],
}

impl PoolJob for Solve<'_, '_> {
    fn work(&self) {
        loop {
            let c = self.next.fetch_add(1, Ordering::Relaxed);
            let (Some(circuit), Some(samples)) = (self.circuits.get(c), self.samples.get(c)) else {
                return;
            };
            // Each channel is taken by one thread only: these never fail.
            if let (Some(mut circuit), Some(mut samples)) = (circuit.try_lock(), samples.try_lock())
                && let Some(samples) = samples.as_deref_mut()
            {
                solve(&mut circuit, samples, self.gain, self.master);
            }
        }
    }
}

fn solve(circuit: &mut Preamp, samples: &mut [f32], gain: &[f32], master: &[f32]) {
    for ((x, g), m) in samples.iter_mut().zip(gain).zip(master) {
        circuit.set_controls(f64::from(*g), f64::from(*m));
        *x = circuit.process(f64::from(*x)) as f32;
    }
}

pub struct PreampProcessor {
    params: ParamValues,
    bank: PreampBank,
    /// Each frame's controls (the parameters as their events set them).
    gain: Vec<f32>,
    master: Vec<f32>,
}

impl PreampProcessor {
    pub fn new(
        model: usize,
        params: ParamValues,
        config: &ProcessConfig,
        channels: usize,
    ) -> Result<Self, PluginError> {
        // Host layouts are negotiated by the graph; built-ins receive the
        // track's actual channel count rather than their stereo descriptor.
        let gain = f64::from(params.get(GAIN as usize));
        let master = f64::from(params.get(MASTER as usize));
        let bridging = bridging(model, &params);
        let block = config.max_block_size.max(1) as usize;
        Ok(Self {
            params,
            bank: PreampBank::new(model, config, channels, gain, master, bridging)?,
            gain: vec![0.0; block],
            master: vec![0.0; block],
        })
    }
}

/// Each frame's gain and master from `params` and the call's events.
fn control_lanes(
    params: &mut ParamValues,
    events: &[faderframe_automation::ParameterEvent],
    gain: &mut [f32],
    master: &mut [f32],
) {
    let mut events = events.iter().peekable();
    // Read the shared atomics at events only, so a concurrent GUI edit
    // cannot give channels different controls within a block.
    let mut g = params.get(GAIN as usize);
    let mut m = params.get(MASTER as usize);
    for (i, (gain, master)) in gain.iter_mut().zip(master.iter_mut()).enumerate() {
        while let Some(e) = events.peek() {
            if e.sample_offset as usize > i {
                break;
            }
            params.apply_event(e.parameter, e.value);
            g = params.get(GAIN as usize);
            m = params.get(MASTER as usize);
            events.next();
        }
        *gain = g;
        *master = m;
    }
}

impl PluginProcessor for PreampProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        let Some(_) = super::pass_through(io) else {
            return ProcessStatus::Continue;
        };
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        if frames > self.gain.len() {
            return ProcessStatus::Error;
        }
        control_lanes(
            &mut self.params,
            ctx.param_events,
            &mut self.gain[..frames],
            &mut self.master[..frames],
        );
        let mut slices: [&mut [f32]; BATCH] = std::array::from_fn(|_| &mut [][..]);
        let mut count = 0;
        for (slice, channel) in slices.iter_mut().zip(out.channels_mut()) {
            *slice = channel;
            count += 1;
        }
        let channels = &mut slices[..count];
        let active = self.bank.active(channels);
        self.bank
            .process(channels, active, 0..frames, &self.gain, &self.master);
        ProcessStatus::Continue
    }
    fn reset(&mut self) {
        self.bank.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::rig::{BLOCK, Rig, SR, silence};

    #[test]
    fn duplicated_mono_wakes_to_stereo_without_changing_audio_or_losing_tails() {
        for model in 0..faderframe_circuit::preamp::MODELS {
            let rig = || {
                Rig::with(parameters(), 0, &[], |params, _, config| {
                    PreampProcessor::new(model, params, config, 2).unwrap()
                })
            };
            let mut shared = rig();
            let mut independent = rig();
            for _ in 0..2 {
                shared.p.reset();
                independent.p.reset();
                // Reference always computes both channels, including mono.
                independent.p.bank.stereo_seen = true;
                for block in 0..16 {
                    if block == 1 || block == 5 || block == 8 {
                        let gain = if block == 1 { 0.8 } else { 0.35 };
                        for r in [&shared, &independent] {
                            r.set(GAIN, gain);
                            r.set(MASTER, -9.0);
                        }
                    }
                    let input = |n: usize| {
                        let x = if (9..12).contains(&block) {
                            0.0
                        } else {
                            0.125 * (n as f32 * 0.13).sin()
                        };
                        // First divergence near the end of a block, followed
                        // by equal input: the independent tails must survive.
                        let right = if block == 8 && n % BLOCK == BLOCK - 1 {
                            x + 0.25
                        } else {
                            x
                        };
                        (x, right)
                    };
                    assert_eq!(
                        shared.run(BLOCK as f64 / SR, input, silence),
                        independent.run(BLOCK as f64 / SR, input, silence),
                        "model {model}, block {block}"
                    );
                    assert_eq!(shared.p.bank.stereo_seen, block >= 8);
                }
            }
        }
    }

    /// Same binary, input, controls and buffers; only redundant stereo solves
    /// differ. Run in release mode with --ignored --nocapture, without other
    /// tests/builds competing for CPU time. Setup is outside the timed region.
    #[test]
    #[ignore]
    fn duplicated_mono_throughput() {
        use faderframe_audio_graph::AudioBuffer;
        use faderframe_core::ChannelLayout;
        use faderframe_realtime::ScopedFlushDenormals;
        use faderframe_transport::TransportInfo;
        use std::time::Instant;

        let _denormals = ScopedFlushDenormals::new();
        const FRAMES: usize = 128;
        let transport = TransportInfo::default();
        let ctx = PluginProcessContext {
            transport: &transport,
            param_events: &[],
            harmony: &crate::NO_HARMONY,
            param_mods: &[],
            note_mods: &[],
        };
        let mut input = AudioBuffer::new(ChannelLayout::Stereo, FRAMES);
        let mut output = AudioBuffer::new(ChannelLayout::Stereo, FRAMES);
        input.set_len(FRAMES);
        output.set_len(FRAMES);
        for channel in 0..2 {
            for (i, x) in input.channel_mut(channel).iter_mut().enumerate() {
                *x = (i as f32 * std::f32::consts::TAU / FRAMES as f32).sin() * 0.125;
            }
        }
        let ins = [input];
        let mut outs = [output];
        for model in 0..faderframe_circuit::preamp::MODELS {
            for shared in [false, true] {
                let mut p = PreampProcessor::new(
                    model,
                    ParamValues::new(parameters()),
                    &crate::devices::rig::config(),
                    2,
                )
                .unwrap();
                p.bank.stereo_seen = !shared;
                let mut times = Vec::with_capacity(1024);
                for block in 0..1056 {
                    let mut io = NodeIo {
                        frames: FRAMES,
                        audio_in: &ins,
                        audio_out: &mut outs,
                        events_in: &[],
                        events_out: &mut [],
                    };
                    let start = Instant::now();
                    p.process(&ctx, &mut io);
                    let elapsed = start.elapsed().as_secs_f64() * 1e6;
                    std::hint::black_box(io.audio_out[0].channel(0));
                    if block >= 32 {
                        times.push(elapsed);
                    }
                }
                let mean = times.iter().sum::<f64>() / times.len() as f64;
                times.sort_by(f64::total_cmp);
                eprintln!(
                    "model {model}, shared={shared}: mean {mean:.1} us, p99 {:.1} us (128 frames / 48 kHz / 2x / gain 0.5)",
                    times[1013]
                );
            }
        }
    }
    /// The demo's drum loop through each model at full gain: time per
    /// 128-frame block of one channel (budget 2667 us), solver passes.
    /// Run in release with --ignored --nocapture.
    #[test]
    #[ignore]
    fn drum_loop_cost_by_model() {
        use faderframe_audio_files::{GeneratorSpec, generate};
        use std::time::Instant;
        let drums = generate(
            &GeneratorSpec::DrumLoop {
                bpm: 112.0,
                bars: 4,
                seed: 11,
            },
            48_000,
        );
        let input: Vec<f32> = drums.channel(0).iter().take(48_000 * 4).copied().collect();
        let peak = input.iter().fold(0f32, |m, x| m.max(x.abs()));
        eprintln!("drum loop peak {:.1} dBFS", 20.0 * peak.log10());
        for model in 0..faderframe_circuit::preamp::MODELS {
            for gain in [0.5, 1.0] {
                let mut p = Preamp::new(model, 48_000.0, gain, 0.0).unwrap();
                let before = p.solver_statistics();
                let mut times: Vec<f64> = Vec::new();
                for block in input.chunks(128) {
                    let start = Instant::now();
                    for &x in block {
                        std::hint::black_box(p.process(f64::from(x)));
                    }
                    times.push(start.elapsed().as_secs_f64() * 1e6);
                }
                let after = p.solver_statistics();
                let mean = times.iter().sum::<f64>() / times.len() as f64;
                times.sort_by(f64::total_cmp);
                eprintln!(
                    "model {model} gain {gain}: mean {mean:>7.1} us, p99 {:>7.1}, max {:>7.1} us / 128 frames; {:.2} passes/solve, {} unsettled, {} rebuilds",
                    times[times.len() * 99 / 100],
                    times[times.len() - 1],
                    (after.1 - before.1) as f64 / (after.0 - before.0).max(1) as f64,
                    after.2 - before.2,
                    after.3 - before.3,
                );
            }
        }
    }
}
