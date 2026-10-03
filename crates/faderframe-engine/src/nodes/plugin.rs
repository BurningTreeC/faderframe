use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_plugin_host::{PluginProcessContext, PluginProcessor, ProcessStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Hosts a plugin processor (insert or instrument) inside the graph.
///
/// Bypassed or failed plugins pass audio through (instruments go silent).
/// The reported latency is the instance's latency at build time; a bypassed
/// plugin reports zero (the graph is rebuilt when bypass changes).
pub struct PluginNode {
    processor: Box<dyn PluginProcessor>,
    latency: u32,
    bypass: bool,
    failed: Arc<AtomicBool>,
}

impl PluginNode {
    pub fn new(
        processor: Box<dyn PluginProcessor>,
        latency: u32,
        bypass: bool,
        failed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            processor,
            latency: if bypass { 0 } else { latency },
            bypass,
            failed,
        }
    }
}

impl Processor<EngineContext> for PluginNode {
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
        let ctx = PluginProcessContext {
            transport: &cx.data.transport,
            param_events: &[],
        };
        if self.processor.process(&ctx, io) == ProcessStatus::Error {
            // Reported to the control side through the shared flag.
            self.failed.store(true, Ordering::Relaxed);
            for out in io.audio_out.iter_mut() {
                out.clear();
            }
        }
    }

    fn reset(&mut self) {
        self.processor.reset();
    }
}
