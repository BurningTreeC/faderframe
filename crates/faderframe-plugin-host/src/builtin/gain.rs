use crate::{ParamValues, PluginProcessContext, PluginProcessor, ProcessStatus};
use faderframe_audio_graph::NodeIo;
use faderframe_core::db_to_gain;

pub struct GainProcessor {
    params: ParamValues,
    current: f32,
}

impl GainProcessor {
    pub fn new(params: ParamValues) -> Self {
        let current = db_to_gain(params.get(0));
        Self { params, current }
    }
}

impl PluginProcessor for GainProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let target = db_to_gain(self.params.get(0));
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return ProcessStatus::Continue;
        };
        out.copy_from(input);
        let n = io.frames.max(1) as f32;
        for c in 0..out.num_channels() {
            let mut g = self.current;
            let step = (target - self.current) / n;
            for s in out.channel_mut(c) {
                g += step;
                *s *= g;
            }
        }
        self.current = target;
        ProcessStatus::Continue
    }

    fn reset(&mut self) {}
}
