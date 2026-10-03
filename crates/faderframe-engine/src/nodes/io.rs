use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_project::MonitorMode;

/// Device-input node: its outputs are filled by the engine before the graph
/// runs (`NodeRole::DeviceInput`), so processing is a no-op.
pub struct DeviceInputTap;

impl Processor<EngineContext> for DeviceInputTap {
    fn process(&mut self, _cx: &ProcessContext<'_, EngineContext>, _io: &mut NodeIo<'_>) {}
}

/// Device-output node: the engine reads its (summed, latency-aligned) input
/// after the graph runs (`NodeRole::DeviceOutput`).
pub struct DeviceOutputSink;

impl Processor<EngineContext> for DeviceOutputSink {
    fn process(&mut self, _cx: &ProcessContext<'_, EngineContext>, _io: &mut NodeIo<'_>) {}
}

/// Passes live input to the track according to its monitoring mode.
///
/// `Auto` is tape-style: monitor while armed, except during playback that is
/// not recording (so existing takes can be auditioned).
pub struct MonitorGate {
    mode: MonitorMode,
    armed: bool,
}

impl MonitorGate {
    pub fn new(mode: MonitorMode, armed: bool) -> Self {
        Self { mode, armed }
    }
}

impl Processor<EngineContext> for MonitorGate {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let t = &cx.data.transport;
        let open = match self.mode {
            MonitorMode::Off => false,
            MonitorMode::Input => true,
            MonitorMode::Auto => self.armed && (!t.playing || t.recording),
        };
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        if open {
            out.copy_from(input);
        } else {
            out.clear();
        }
    }
}
