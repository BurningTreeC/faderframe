//! Fixed-delay circuit worker. Offline/ahead processing uses the same delay
//! inline, so renders are deterministic and the graph's compensation is stable.
use super::*;
use faderframe_realtime::reservoir::{self, Reservoir, Segments, Timing};

/// The least the worker buffers (and its chunk size).
pub const BUFFER_LATENCY: usize = 128;

/// How much a preamp buffers with device callbacks of `device_block`
/// frames: at least one callback, so the worker always has a whole
/// callback's time for a chunk (with less, part of each callback's circuit
/// work would be done while the audio thread waits for it).
pub fn buffer_delay(device_block: usize) -> usize {
    device_block.clamp(BUFFER_LATENCY, 8192)
}

struct CircuitWorker {
    bank: PreampBank,
    rate: f64,
}

impl Segments for CircuitWorker {
    type Controls = ();
    type Reset = ();

    fn process(
        &mut self,
        channels: &mut [&mut [f32]],
        _: &(),
        timing: &Timing,
        publish: &mut dyn FnMut(&[&mut [f32]], usize),
    ) {
        // The audio, then gain/drive, master and calibrated bus Gain.
        let count = self.bank.len();
        if channels.len() < count + 3 {
            return;
        }
        let frames = channels.first().map_or(0, |c| c.len());
        // What the host waits for comes first.
        let first = if (1..frames).contains(&timing.first) {
            timing.first
        } else {
            frames
        };
        // Live: a block of samples that will not settle does not run on
        // past the point where its audio is certainly late.
        self.bank
            .set_deadline(crate::devices::solve_deadline(timing, frames, self.rate));
        let active = {
            let (audio, lanes) = channels.split_at_mut(count);
            let active = self.bank.active(audio);
            self.bank
                .process(audio, active, 0..first, lanes[0], lanes[1], lanes[2]);
            active
        };
        if first < frames {
            publish(channels, first);
            let (audio, lanes) = channels.split_at_mut(count);
            self.bank
                .process(audio, active, first..frames, lanes[0], lanes[1], lanes[2]);
        }
    }

    fn reset(&mut self, _: &()) {
        self.bank.reset();
    }
}

enum Processing {
    Inline {
        dsp: Box<PreampProcessor>,
        delay: Vec<Vec<f32>>,
        cursor: usize,
    },
    Worker(Box<Reservoir<CircuitWorker>>),
}

pub struct BufferedPreampProcessor {
    params: ParamValues,
    processing: Processing,
    reported_underruns: u64,
    deadline: Option<std::time::Instant>,
    // Three additional reservoir lanes carry sample-accurate controls alongside
    // audio. This preserves the host callback size (and structural wait budget)
    // without large event packets or allocations on either realtime thread.
    gain_lane: Vec<f32>,
    master_lane: Vec<f32>,
    bus_gain_lane: Vec<f32>,
}

impl BufferedPreampProcessor {
    /// `delay`: [`buffer_delay`] of the device's callbacks (what the
    /// instance reports as its latency, live or not).
    pub fn new(
        model: usize,
        params: ParamValues,
        config: &ProcessConfig,
        channels: usize,
        realtime: bool,
        delay: usize,
    ) -> Result<Self, PluginError> {
        let delay = delay.max(BUFFER_LATENCY);
        let dsp = PreampProcessor::new(model, params.clone(), config, channels)?;
        let processing = if realtime && (1..=reservoir::MAX_CHANNELS - 3).contains(&channels) {
            Processing::Worker(Box::new(
                Reservoir::try_new(
                    reservoir::Config {
                        delay,
                        channels: channels + 3,
                        max_block: config.max_block_size as usize,
                        sample_rate: config.sample_rate,
                        offline: false,
                    },
                    Box::new(CircuitWorker {
                        bank: dsp.bank,
                        rate: config.sample_rate,
                    }),
                )
                .map_err(|e| PluginError::Failed(format!("Microphone preamp worker: {e}")))?,
            ))
        } else {
            Processing::Inline {
                dsp: Box::new(dsp),
                delay: vec![vec![0.0; delay]; channels],
                cursor: 0,
            }
        };
        Ok(Self {
            params,
            processing,
            reported_underruns: 0,
            deadline: None,
            gain_lane: vec![0.0; config.max_block_size as usize],
            master_lane: vec![0.0; config.max_block_size as usize],
            bus_gain_lane: vec![0.0; config.max_block_size as usize],
        })
    }
}

impl PluginProcessor for BufferedPreampProcessor {
    fn set_callback_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.deadline = deadline;
    }
    fn preferred_block_size(&self) -> usize {
        if matches!(self.processing, Processing::Worker(_)) {
            BUFFER_LATENCY
        } else {
            usize::MAX
        }
    }

    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        match &mut self.processing {
            Processing::Inline { dsp, delay, cursor } => {
                let status = dsp.process(ctx, io);
                if let Some(out) = io.audio_out.first_mut() {
                    let len = delay.first().map_or(1, Vec::len);
                    for (samples, ring) in out.channels_mut().zip(delay.iter_mut()) {
                        for (i, sample) in samples.iter_mut().enumerate() {
                            std::mem::swap(sample, &mut ring[(*cursor + i) % len]);
                        }
                    }
                    *cursor = (*cursor + io.frames) % len;
                }
                status
            }
            Processing::Worker(worker) => {
                if !worker.worker_alive() {
                    return ProcessStatus::Error;
                }
                let Some(_) = super::super::pass_through(io) else {
                    return ProcessStatus::Continue;
                };
                let Some(out) = io.audio_out.first_mut() else {
                    return ProcessStatus::Continue;
                };
                let count = out.num_channels();
                if io.frames > self.gain_lane.len() || count + 3 != worker.config().channels {
                    return ProcessStatus::Error;
                }
                control_lanes(
                    &mut self.params,
                    ctx.param_events,
                    &mut self.gain_lane[..io.frames],
                    &mut self.master_lane[..io.frames],
                    &mut self.bus_gain_lane[..io.frames],
                );
                let mut slices: [&mut [f32]; reservoir::MAX_CHANNELS] =
                    std::array::from_fn(|_| &mut [][..]);
                for (slice, channel) in slices.iter_mut().zip(out.channels_mut()) {
                    *slice = channel;
                }
                slices[count] = &mut self.gain_lane[..io.frames];
                slices[count + 1] = &mut self.master_lane[..io.frames];
                slices[count + 2] = &mut self.bus_gain_lane[..io.frames];
                worker.process_with_deadline(&mut slices[..count + 3], (), self.deadline);
                ProcessStatus::Continue
            }
        }
    }

    fn reset(&mut self) {
        match &mut self.processing {
            Processing::Inline { dsp, delay, cursor } => {
                dsp.reset();
                delay.iter_mut().for_each(|c| c.fill(0.0));
                *cursor = 0;
            }
            Processing::Worker(worker) => worker.reset(()),
        }
    }

    fn take_underruns(&mut self) -> u64 {
        let Processing::Worker(worker) = &self.processing else {
            return 0;
        };
        let total = worker
            .stats()
            .underruns()
            .saturating_add(worker.stats().input_overflows());
        let new = total.saturating_sub(self.reported_underruns);
        self.reported_underruns = total;
        new
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_audio_graph::AudioBuffer;
    use faderframe_automation::ParameterEvent;
    use faderframe_core::{ChannelLayout, ParameterId};
    use faderframe_transport::TransportInfo;

    #[test]
    fn worker_matches_inline_with_exact_delay_automation_channels_and_resets() {
        // The engine and worker both flush denormals in real use.
        let _denormals = faderframe_realtime::ScopedFlushDenormals::new();
        let config = ProcessConfig {
            sample_rate: 48_000.0,
            max_block_size: 511,
            sidechain: false,
            double_precision: false,
        };
        for model in
            0..faderframe_circuit::preamp::MODELS + faderframe_circuit::preamp::CONSOLE_BUSES
        {
            for count in [1, 2, 4] {
                let parameters = || {
                    if model < faderframe_circuit::preamp::MODELS {
                        parameters_for(model)
                    } else {
                        bus_parameters()
                    }
                };
                let params = ParamValues::new(parameters());
                let dsp = PreampProcessor::new(model, params.clone(), &config, count).unwrap();
                let mut worker = BufferedPreampProcessor {
                    params,
                    processing: Processing::Worker(Box::new(
                        Reservoir::try_new(
                            reservoir::Config {
                                delay: BUFFER_LATENCY,
                                channels: count + 3,
                                max_block: config.max_block_size as usize,
                                sample_rate: config.sample_rate,
                                // Deterministic parity: scheduling cannot drop audio.
                                offline: true,
                            },
                            Box::new(CircuitWorker {
                                bank: dsp.bank,
                                rate: config.sample_rate,
                            }),
                        )
                        .unwrap(),
                    )),
                    reported_underruns: 0,
                    deadline: None,
                    gain_lane: vec![0.0; config.max_block_size as usize],
                    master_lane: vec![0.0; config.max_block_size as usize],
                    bus_gain_lane: vec![0.0; config.max_block_size as usize],
                };
                let mut inline = BufferedPreampProcessor::new(
                    model,
                    ParamValues::new(parameters()),
                    &config,
                    count,
                    false,
                    BUFFER_LATENCY,
                )
                .unwrap();
                let layout = ChannelLayout::from_channel_count(count);
                let mut input = [AudioBuffer::new(layout, 511)];
                let mut a = [AudioBuffer::new(layout, 511)];
                let mut b = [AudioBuffer::new(layout, 511)];
                for _ in 0..2 {
                    worker.reset();
                    inline.reset();
                    let mut at = 0;
                    for (block, len) in [31, 64, 128, 511, 97, 128, 256, 511]
                        .into_iter()
                        .enumerate()
                    {
                        for buf in [&mut input[0], &mut a[0], &mut b[0]] {
                            buf.set_len(len);
                        }
                        for (c, samples) in input[0].channels_mut().enumerate() {
                            for (i, x) in samples.iter_mut().enumerate() {
                                *x = if block >= 5 {
                                    0.0
                                } else {
                                    ((at + i) as f32 * 0.13
                                        + if block >= 3 { c as f32 * 0.37 } else { 0.0 })
                                    .sin()
                                        * 0.125
                                };
                            }
                        }
                        // Change both controls inside chunks and at their boundaries.
                        let events: Vec<_> = [0, 17, 128, 255, 400]
                            .into_iter()
                            .filter(|&i| i < len)
                            .flat_map(|i| {
                                [
                                    ParameterEvent {
                                        sample_offset: i as u32,
                                        parameter: ParameterId(GAIN),
                                        value: if (at + i) % 2 == 0 { 0.3 } else { 0.7 },
                                    },
                                    ParameterEvent {
                                        sample_offset: i as u32,
                                        parameter: ParameterId(MASTER),
                                        value: -6.0,
                                    },
                                    ParameterEvent {
                                        sample_offset: i as u32,
                                        parameter: ParameterId(BUS_GAIN),
                                        value: if (at + i) % 2 == 0 { 0.0 } else { 0.8 },
                                    },
                                ]
                            })
                            .collect();
                        let transport = TransportInfo::default();
                        let ctx = PluginProcessContext {
                            transport: &transport,
                            param_events: &events,
                            harmony: &crate::NO_HARMONY,
                            param_mods: &[],
                            note_mods: &[],
                        };
                        for (p, out) in [(&mut worker, &mut a), (&mut inline, &mut b)] {
                            assert_eq!(
                                p.process(
                                    &ctx,
                                    &mut NodeIo {
                                        frames: len,
                                        audio_in: &input,
                                        audio_out: out,
                                        events_in: &[],
                                        events_out: &mut [],
                                    }
                                ),
                                ProcessStatus::Continue
                            );
                        }
                        for c in 0..count {
                            assert_eq!(
                                a[0].channel(c),
                                b[0].channel(c),
                                "model {model}, channel {c}, block {block}"
                            );
                            if at + len <= BUFFER_LATENCY {
                                assert!(
                                    a[0].channel(c).iter().all(|&x| x == 0.0),
                                    "128 samples of priming"
                                );
                            }
                        }
                        assert_eq!(worker.take_underruns(), 0);
                        at += len;
                    }
                }
            }
        }
    }
}
