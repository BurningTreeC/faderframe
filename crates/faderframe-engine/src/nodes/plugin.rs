use super::{AUTOMATION_STEP, automation_at, ramp_step};
use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_automation::ParameterEvent;
use faderframe_core::{ParameterId, PluginInstanceId, TrackId};
use faderframe_plugin_host::{PluginProcessContext, PluginProcessor, ProcessStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Most parameter events handed to a plugin per block.
const EVENT_CAPACITY: usize = 1024;
/// Most automated parameters tracked per plugin.
const MAX_AUTOMATED: usize = 128;

/// Hosts a plugin processor (insert or instrument) inside the graph.
///
/// Bypassed or failed plugins pass audio through (instruments go silent).
/// The reported latency is the instance's latency at build time; a bypassed
/// plugin reports zero (the graph is rebuilt when bypass changes).
///
/// Automated parameters reach the plugin as sample-accurate
/// [`ParameterEvent`]s: one wherever the curve has a breakpoint and one
/// every [`AUTOMATION_STEP`] frames while the value changes. Automated
/// bypass is a *soft* bypass: the plugin keeps running (so its latency and
/// state stay consistent), and the output crossfades to the input delayed
/// by the plugin's latency, keeping every summing point aligned.
pub struct PluginNode {
    track: TrackId,
    plugin: PluginInstanceId,
    processor: Box<dyn PluginProcessor>,
    latency: u32,
    bypass: bool,
    failed: Arc<AtomicBool>,
    events: Vec<ParameterEvent>,
    /// Last value sent per automated parameter (to skip repeats).
    sent: Vec<(ParameterId, f32)>,
    /// Soft-bypass mix: 0 = plugin output, 1 = (delayed) input.
    dry: f32,
    /// Latency-matching delay of the dry signal, per channel.
    delay: Vec<Box<[f32]>>,
    delay_pos: usize,
}

impl PluginNode {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        track: TrackId,
        plugin: PluginInstanceId,
        processor: Box<dyn PluginProcessor>,
        latency: u32,
        bypass: bool,
        failed: Arc<AtomicBool>,
        channels: usize,
    ) -> Self {
        let latency = if bypass { 0 } else { latency };
        Self {
            track,
            plugin,
            processor,
            latency,
            bypass,
            failed,
            events: Vec::with_capacity(EVENT_CAPACITY),
            sent: Vec::with_capacity(MAX_AUTOMATED),
            dry: 0.0,
            delay: (0..channels)
                .map(|_| vec![0.0; latency as usize].into_boxed_slice())
                .collect(),
            delay_pos: 0,
        }
    }

    fn last_sent(&mut self, id: ParameterId) -> Option<&mut f32> {
        self.sent.iter_mut().find(|(p, _)| *p == id).map(|(_, v)| v)
    }

    /// Fill `self.events` from the automation of this plugin.
    fn collect_events(&mut self, cx: &ProcessContext<'_, EngineContext>, frames: usize) {
        self.events.clear();
        let Some(auto) = cx.data.timeline.automation(self.track) else {
            return;
        };
        let start = cx.data.transport.sample_position;
        let playing = cx.data.transport.playing;
        for (id, lane) in auto.plugin_params(self.plugin) {
            let emit = |node: &mut Self, offset: usize, at: i64| {
                let Some(v) = lane.value_at(at) else { return };
                let v = v as f32;
                match node.last_sent(id) {
                    Some(last) if *last == v => return,
                    Some(last) => *last = v,
                    None => {
                        if node.sent.len() < MAX_AUTOMATED {
                            node.sent.push((id, v));
                        }
                    }
                }
                if node.events.len() < EVENT_CAPACITY {
                    node.events.push(ParameterEvent {
                        parameter: id,
                        value: v,
                        sample_offset: offset as u32,
                    });
                }
            };
            emit(self, 0, automation_at(cx, 0));
            if !playing {
                continue;
            }
            // Breakpoints inside the block, exactly.
            let mut s = start;
            while let Some(at) = lane.next_point_in(s, start + frames as i64) {
                emit(self, (at - start) as usize, at);
                s = at;
            }
            // Ramps between them.
            let mut off = AUTOMATION_STEP;
            while off < frames {
                emit(self, off, start + off as i64);
                off += AUTOMATION_STEP;
            }
        }
        // In place (no allocation): stable order by offset.
        self.events.sort_by_key(|e| e.sample_offset);
    }

    fn soft_bypass(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let target = cx
            .data
            .timeline
            .automation(self.track)
            .and_then(|a| a.bypass(self.plugin))
            .and_then(|l| l.value_at(automation_at(cx, io.frames)))
            .map_or(0.0, |v| if v >= 0.5 { 1.0 } else { 0.0 });
        let n = io.frames;
        let latency = self.latency as usize;
        let start_pos = self.delay_pos;
        let from = self.dry;
        let step = ramp_step(from, target, n);
        let Some(out) = io.audio_out.first_mut() else {
            return;
        };
        for c in 0..out.num_channels() {
            let input = io
                .audio_in
                .first()
                .map(|i| i.channel(c.min(i.num_channels().saturating_sub(1))));
            let mut delay = self.delay.get_mut(c);
            let mut g = from;
            let mut pos = start_pos;
            for (i, o) in out.channel_mut(c).iter_mut().enumerate() {
                g += step;
                let x = input.map_or(0.0, |inp| inp[i]);
                let dry = match delay.as_deref_mut() {
                    Some(d) if latency > 0 => {
                        let y = d[pos];
                        d[pos] = x;
                        pos = (pos + 1) % latency;
                        y
                    }
                    _ => x,
                };
                *o = *o * (1.0 - g) + dry * g;
            }
        }
        if latency > 0 {
            self.delay_pos = (start_pos + n) % latency;
        }
        self.dry = target;
    }
}

impl Processor<EngineContext> for PluginNode {
    fn preferred_block_size(&self) -> usize {
        if self.bypass {
            usize::MAX
        } else {
            self.processor.preferred_block_size()
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        if self.bypass || self.failed.load(Ordering::Relaxed) {
            for (i, out) in io.audio_out.iter_mut().enumerate() {
                match io.audio_in.get(i) {
                    Some(input) => out.copy_from(input),
                    None => out.clear(),
                }
            }
            return;
        }
        self.collect_events(cx, io.frames);
        let events = std::mem::take(&mut self.events);
        let ctx = PluginProcessContext {
            transport: &cx.data.transport,
            param_events: &events,
        };
        self.processor
            .set_callback_deadline(cx.data.callback_deadline);
        let status = self.processor.process(&ctx, io);
        let underruns = self.processor.take_underruns();
        if underruns != 0 {
            cx.data
                .worker_underruns
                .fetch_add(underruns, Ordering::Relaxed);
        }
        self.events = events;
        if status == ProcessStatus::Error {
            // Reported to the control side through the shared flag.
            self.failed.store(true, Ordering::Relaxed);
            for out in io.audio_out.iter_mut() {
                out.clear();
            }
            return;
        }
        let automated_bypass = cx
            .data
            .timeline
            .automation(self.track)
            .is_some_and(|a| a.bypass(self.plugin).is_some());
        if automated_bypass || self.dry > 0.0 {
            self.soft_bypass(cx, io);
        }
    }

    fn reset(&mut self) {
        self.processor.reset();
        self.sent.clear();
        for d in &mut self.delay {
            d.fill(0.0);
        }
    }
}
