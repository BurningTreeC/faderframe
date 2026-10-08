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

/// A hardware insert's send: its input at the device's Send level, on to
/// the device output node after it.
pub struct HardwareSendGain {
    params: faderframe_plugin_host::ParamValues,
    current: f32,
}

impl HardwareSendGain {
    pub fn new(params: faderframe_plugin_host::ParamValues) -> Self {
        let current = faderframe_plugin_host::devices::hardware_insert::routing(&params).2;
        Self { params, current }
    }
}

impl Processor<EngineContext> for HardwareSendGain {
    fn process(&mut self, _cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        out.copy_from(input);
        let target = faderframe_plugin_host::devices::hardware_insert::routing(&self.params).2;
        let step = (target - self.current) / io.frames.max(1) as f32;
        for c in 0..out.num_channels() {
            let mut g = self.current;
            for s in out.channel_mut(c).iter_mut() {
                g += step;
                *s *= g;
            }
        }
        self.current = target;
    }
}

/// A hardware insert's return: a device input (filled by the engine) whose
/// latency is the round trip, counted from its send (`NodeSpec::after`).
pub struct HardwareReturn {
    pub latency: u32,
}

impl Processor<EngineContext> for HardwareReturn {
    fn process(&mut self, _cx: &ProcessContext<'_, EngineContext>, _io: &mut NodeIo<'_>) {}

    fn latency(&self) -> u32 {
        self.latency
    }
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

/// A surround bed folded down to fewer channels for the device (a 5.1
/// master on a stereo interface): each speaker where it stands in the
/// smaller format (`faderframe_core::surround::matrix`), LFE dropped unless
/// the smaller one has one.
pub struct FoldDown {
    matrix:
        [[f32; faderframe_core::surround::MAX_SPEAKERS]; faderframe_core::surround::MAX_SPEAKERS],
}

impl FoldDown {
    pub fn new(from: faderframe_core::ChannelLayout, to: faderframe_core::ChannelLayout) -> Self {
        let mut matrix = [[0.0; faderframe_core::surround::MAX_SPEAKERS];
            faderframe_core::surround::MAX_SPEAKERS];
        faderframe_core::surround::matrix(
            from,
            to,
            &faderframe_core::SurroundPan::default(),
            &mut matrix,
        );
        Self { matrix }
    }
}

impl Processor<EngineContext> for FoldDown {
    fn process(&mut self, _cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        out.clear();
        let n = io.frames;
        for (s, row) in self.matrix.iter().enumerate().take(input.num_channels()) {
            for (d, &g) in row.iter().enumerate().take(out.num_channels()) {
                if g == 0.0 {
                    continue;
                }
                for (o, &x) in out.channel_mut(d)[..n]
                    .iter_mut()
                    .zip(&input.channel(s)[..n])
                {
                    *o += x * g;
                }
            }
        }
    }
}
