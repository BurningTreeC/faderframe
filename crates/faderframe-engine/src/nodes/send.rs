use super::ramp_step;
use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_realtime::ParamSlot;

/// An aux/bus send: level-controlled copy of a tap point, converted to the
/// destination layout by the graph's channel rules.
pub struct SendNode {
    level: ParamSlot,
    current: f32,
}

impl SendNode {
    pub fn new(level: ParamSlot) -> Self {
        Self {
            level,
            current: f32::NAN,
        }
    }
}

impl Processor<EngineContext> for SendNode {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let target = cx.data.params.get(self.level);
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        out.copy_from(input);
        let from = if self.current.is_nan() {
            target
        } else {
            self.current
        };
        let step = ramp_step(from, target, io.frames);
        for c in 0..out.num_channels() {
            let mut g = from;
            for s in out.channel_mut(c) {
                g += step;
                *s *= g;
            }
        }
        self.current = target;
    }
}
