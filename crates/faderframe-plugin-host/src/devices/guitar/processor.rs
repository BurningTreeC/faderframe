//! The Guitar Station on the audio thread: input trim, the line's stages
//! one after another (each its own reservoir), then Mix and Output on the
//! main bus and the DI, at unity, on the second.

use super::bank::Bank;
use super::stages::{AmpWorker, PedalWorker, Run, StageRun, Workshop};
use super::{amp_settings, id, nth_pedal, pedal_count, quality, value};
use crate::devices::preamp::{BUFFER_LATENCY, buffer_delay};
use crate::tap::{AnalysisTap, MeterTap};
use crate::{
    ParamValues, PluginError, PluginProcessContext, PluginProcessor, ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_guitar::chain::Chain;
use faderframe_guitar::pedal::PedalStage;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

/// Channels the editor's meters show.
const METERED: usize = 2;

fn db_gain(db: f32) -> f32 {
    10f32.powf(db.clamp(-60.0, 24.0) / 20.0)
}

/// One step of a one-pole glide, landing exactly on the target at the end.
#[inline]
fn glide(v: &mut f32, target: f32, k: f32) {
    *v += k * (target - *v);
    if (target - *v).abs() < 1e-6 {
        *v = target;
    }
}

pub struct GuitarProcessor {
    params: ParamValues,
    tap: Option<Arc<AnalysisTap>>,
    pedals: Vec<StageRun<PedalWorker>>,
    amp: StageRun<AmpWorker>,
    workshop: Arc<Workshop>,
    builder: Option<JoinHandle<()>>,
    channels: usize,
    /// The guitar's own lanes (the DI's source), then the DI out.
    raw: Vec<Vec<f32>>,
    input_gain: f32,
    output_gain: f32,
    mix: f32,
    /// One-pole glide per sample, about 10 ms.
    glide: f32,
    meters: [[MeterTap; METERED]; 2],
    deadline: Option<Instant>,
    reported_underruns: u64,
}

impl GuitarProcessor {
    /// Builds the line for the parameters' pedals (their circuits, every
    /// amplifier) off the audio thread. `delay`: [`buffer_delay`] of the
    /// device's callbacks; `realtime`: run the stages on workers.
    pub fn new(
        params: ParamValues,
        tap: Option<Arc<AnalysisTap>>,
        config: &ProcessConfig,
        channels: usize,
        realtime: bool,
        device_block: usize,
    ) -> Result<Self, PluginError> {
        let run = if realtime { Run::Live } else { Run::Inline };
        Self::with_run(params, tap, config, channels, run, device_block)
    }

    pub(crate) fn with_run(
        params: ParamValues,
        tap: Option<Arc<AnalysisTap>>,
        config: &ProcessConfig,
        channels: usize,
        run: Run,
        device_block: usize,
    ) -> Result<Self, PluginError> {
        let channels = channels.max(1);
        let rate = config.sample_rate.max(1.0);
        let quality = quality(&params);
        let max_block = config.max_block_size.max(1) as usize;
        let delay = buffer_delay(device_block).max(BUFFER_LATENCY);
        let failed =
            |what: &str, e: String| PluginError::Failed(format!("Guitar Station {what}: {e}"));
        let pedal_count = pedal_count(&params);
        let workshop = Workshop::new(pedal_count, rate, quality, channels);
        // The amplifier's chains are the slow part: one thread per channel.
        let chains: Vec<Chain> = std::thread::scope(|s| {
            let jobs: Vec<_> = (0..channels)
                .map(|_| s.spawn(move || Chain::new(rate, quality)))
                .collect();
            jobs.into_iter()
                .map(|j| j.join().unwrap_or_else(|_| Err("panicked".into())))
                .collect::<Result<Vec<_>, String>>()
        })
        .map_err(|e| failed("amplifier", e))?;
        let lanes = 2 * channels;
        let mut pedals = Vec::with_capacity(pedal_count);
        for n in 0..pedal_count {
            let (_, settings) = nth_pedal(&params, n);
            let mut units: Vec<PedalStage> = (0..channels)
                .map(|_| PedalStage::new(rate, quality))
                .collect();
            let mut bundle = workshop
                .build(settings.stomp)
                .map_err(|e| failed("pedal", e))?;
            for (unit, circuit) in units.iter_mut().zip(bundle.circuits_mut()) {
                unit.swap(circuit);
                unit.apply(&settings);
                unit.reset();
            }
            let worker = PedalWorker::new(
                Bank::new(units),
                Arc::clone(&workshop),
                n,
                run != Run::Live,
                tap.clone(),
            );
            pedals.push(
                StageRun::new(worker, lanes, delay, max_block, rate, run)
                    .map_err(|e| failed("worker", e.to_string()))?,
            );
        }
        let mut chains = chains;
        let settings = amp_settings(&params);
        for chain in &mut chains {
            chain.apply(&settings);
            chain.reset();
        }
        let amp = StageRun::new(
            AmpWorker::new(Bank::new(chains)),
            lanes,
            delay,
            max_block,
            rate,
            run,
        )
        .map_err(|e| failed("worker", e.to_string()))?;
        let builder = workshop
            .start()
            .map_err(|e| failed("workshop", e.to_string()))?;
        if let Some(tap) = &tap {
            let stage = super::stage_latency(quality, device_block);
            tap.set_value(value::LATENCY, ((pedal_count as u32 + 1) * stage) as f32);
        }
        let at = |i: u32| params.get(i as usize);
        Ok(Self {
            input_gain: db_gain(at(id::INPUT)),
            output_gain: db_gain(at(id::OUTPUT)),
            mix: at(id::MIX).clamp(0.0, 1.0),
            params,
            tap,
            pedals,
            amp,
            workshop,
            builder,
            channels,
            raw: vec![vec![0.0; max_block]; channels],
            glide: 1.0 - (-1.0 / (0.01 * rate)).exp() as f32,
            meters: [[MeterTap::new(rate as f32); METERED]; 2],
            deadline: None,
            reported_underruns: 0,
        })
    }

    /// The pedals the line was built with.
    pub fn pedals(&self) -> usize {
        self.pedals.len()
    }

    #[cfg(test)]
    pub(super) fn force_stereo(&mut self) {
        use super::stages::Probe;
        for stage in &mut self.pedals {
            if let Some(w) = stage.probe() {
                w.force_stereo();
            }
        }
        if let Some(w) = self.amp.probe() {
            w.force_stereo();
        }
    }

    #[cfg(test)]
    pub(super) fn installed(&mut self) -> Vec<faderframe_guitar::pedal::Stomp> {
        use super::stages::Probe;
        self.pedals
            .iter_mut()
            .filter_map(|s| s.probe().map(|w| w.installed()))
            .collect()
    }
}

impl Drop for GuitarProcessor {
    fn drop(&mut self) {
        self.workshop.stop();
        if let Some(handle) = self.builder.take() {
            let _ = handle.join();
        }
    }
}

impl PluginProcessor for GuitarProcessor {
    fn set_callback_deadline(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
    }

    fn preferred_block_size(&self) -> usize {
        if self.amp.is_worker() {
            BUFFER_LATENCY
        } else {
            usize::MAX
        }
    }

    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        if super::super::pass_through(io).is_none() {
            return ProcessStatus::Continue;
        }
        let (main, extra) = io.audio_out.split_at_mut(1);
        let Some(out) = main.first_mut() else {
            return ProcessStatus::Continue;
        };
        let count = out.num_channels().min(self.channels);
        if count == 0 {
            return ProcessStatus::Continue;
        }
        if frames > self.raw.first().map_or(0, Vec::len) {
            return ProcessStatus::Error;
        }
        // Input trim (the DI's guitar is taken after it).
        let target = db_gain(self.params.get(id::INPUT as usize));
        let k = self.glide;
        {
            let mut gain = self.input_gain;
            for i in 0..frames {
                glide(&mut gain, target, k);
                for c in 0..count {
                    let x = out.channel_mut(c)[i] * gain;
                    out.channel_mut(c)[i] = x;
                    self.raw[c][i] = x;
                    if c < METERED {
                        self.meters[0][c].add(x);
                    }
                }
            }
            self.input_gain = gain;
        }
        // The line, in segments between parameter changes.
        let mut events = ctx.param_events.iter().peekable();
        let mut start = 0;
        let mut alive = true;
        while start < frames {
            while let Some(e) = events.peek() {
                if e.sample_offset as usize > start {
                    break;
                }
                self.params.apply_event(e.parameter, e.value);
                events.next();
            }
            let end = events.peek().map_or(frames, |e| {
                (e.sample_offset as usize).clamp(start + 1, frames)
            });
            let mut lanes: [&mut [f32]; 2 * super::bank::BATCH] =
                std::array::from_fn(|_| &mut [][..]);
            let n = count.min(super::bank::BATCH);
            for (slot, channel) in lanes.iter_mut().zip(out.channels_mut()).take(n) {
                *slot = &mut channel[start..end];
            }
            for (slot, raw) in lanes[n..].iter_mut().zip(self.raw.iter_mut()).take(n) {
                *slot = &mut raw[start..end];
            }
            let lanes = &mut lanes[..2 * n];
            for (k, stage) in self.pedals.iter_mut().enumerate() {
                alive &= stage.process(lanes, nth_pedal(&self.params, k), self.deadline);
            }
            alive &= self
                .amp
                .process(lanes, amp_settings(&self.params), self.deadline);
            start = end;
        }
        // Mix, Output, the DI bus.
        let mix_target = self.params.get(id::MIX as usize).clamp(0.0, 1.0);
        let out_target = db_gain(self.params.get(id::OUTPUT as usize));
        let (mut mix, mut gain) = (self.mix, self.output_gain);
        let mut di = extra.first_mut();
        for i in 0..frames {
            glide(&mut mix, mix_target, k);
            glide(&mut gain, out_target, k);
            for c in 0..count {
                let dry = self.raw[c][i];
                let wet = out.channel(c)[i];
                let y = (dry * (1.0 - mix) + wet * mix) * gain;
                out.channel_mut(c)[i] = y;
                if c < METERED {
                    self.meters[1][c].add(y);
                }
                // The DI at unity: it is the guitar (or the pedals, or the
                // preamp) for reamping, not the amplifier's level.
                if let Some(bus) = di.as_mut()
                    && c < bus.num_channels()
                {
                    bus.channel_mut(c)[i] = dry;
                }
            }
        }
        self.mix = mix;
        self.output_gain = gain;
        if let Some(bus) = di {
            // A mono line on a wider DI bus: the same on every side.
            for c in count..bus.num_channels() {
                for i in 0..frames {
                    let v = bus.channel(0)[i];
                    bus.channel_mut(c)[i] = v;
                }
            }
        }
        if let Some(tap) = &self.tap {
            for c in 0..count.min(METERED) {
                self.meters[0][c].publish(&tap.meter_in, c, frames);
                self.meters[1][c].publish(&tap.meter_out, c, frames);
            }
            let underruns =
                self.pedals.iter().map(StageRun::underruns).sum::<u64>() + self.amp.underruns();
            tap.set_value(value::UNDERRUNS, underruns as f32);
        }
        if alive {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Error
        }
    }

    fn reset(&mut self) {
        for stage in &mut self.pedals {
            stage.reset();
        }
        self.amp.reset();
        for m in self.meters.iter_mut().flatten() {
            m.reset();
        }
    }

    fn take_underruns(&mut self) -> u64 {
        let total = self.pedals.iter().map(StageRun::underruns).sum::<u64>() + self.amp.underruns();
        let new = total.saturating_sub(self.reported_underruns);
        self.reported_underruns = total;
        new
    }
}
