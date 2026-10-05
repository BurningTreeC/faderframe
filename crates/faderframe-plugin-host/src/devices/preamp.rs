//! Dedicated microphone input processors, backed by the reusable circuit solver.
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginError, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_circuit::preamp::Preamp;

mod buffered;
pub use buffered::{BUFFER_LATENCY, BufferedPreampProcessor};

pub const GAIN: u32 = 0;
pub const MASTER: u32 = 1;
pub fn parameters() -> Vec<ParameterInfo> {
    vec![
        super::param(GAIN, "Gain", 0.0, 1.0, 0.5, ParameterUnit::Percent),
        super::param(MASTER, "Master", -60.0, 12.0, 0.0, ParameterUnit::Decibels),
    ]
}

pub struct PreampProcessor {
    params: ParamValues,
    channels: Vec<Preamp>,
    // Like GainStageFx, run one circuit while stereo input is exactly mono.
    // Once histories diverge, preserve both tails until a transport reset.
    stereo_seen: bool,
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
        let channels = (0..channels)
            .map(|_| {
                Preamp::new(model, config.sample_rate, gain, master)
                    .map_err(|e| PluginError::Failed(format!("Microphone preamp: {e}")))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            params,
            channels,
            stereo_seen: false,
        })
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
        let duplicated_mono =
            if !self.stereo_seen && self.channels.len() == 2 && out.num_channels() == 2 {
                if out.channel(0) == out.channel(1) {
                    true
                } else {
                    let (left, right) = self.channels.split_at_mut(1);
                    right[0].copy_runtime_state_from(&left[0]);
                    self.stereo_seen = true;
                    false
                }
            } else {
                false
            };
        let active_channels = if duplicated_mono {
            1
        } else {
            out.num_channels()
        };
        let mut events = ctx.param_events.iter().peekable();
        for i in 0..frames {
            let mut changed = i == 0;
            while let Some(e) = events.peek() {
                if e.sample_offset as usize > i {
                    break;
                }
                self.params.apply_event(e.parameter, e.value);
                changed = true;
                events.next();
            }
            if changed {
                // Read shared atomics once so a concurrent GUI edit cannot
                // give the sleeping channel different controls/history.
                let gain = f64::from(self.params.get(GAIN as usize));
                let master = f64::from(self.params.get(MASTER as usize));
                for p in &mut self.channels {
                    p.set_controls(gain, master);
                }
            }
            for (c, p) in self.channels.iter_mut().enumerate().take(active_channels) {
                let samples = out.channel_mut(c);
                samples[i] = p.process(f64::from(samples[i])) as f32;
            }
            if duplicated_mono {
                out.channel_mut(1)[i] = out.channel(0)[i];
            }
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {
        self.channels.iter_mut().for_each(Preamp::reset);
        self.stereo_seen = false;
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
                independent.p.stereo_seen = true;
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
                    assert_eq!(shared.p.stereo_seen, block >= 8);
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
                p.stereo_seen = !shared;
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
}
