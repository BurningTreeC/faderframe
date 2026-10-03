use crate::{ParamValues, PluginProcessContext, PluginProcessor, ProcessConfig, ProcessStatus};
use faderframe_audio_graph::NodeIo;

/// Feed-forward peak compressor. The key signal is the sidechain input
/// when one is connected (second audio input), else the main input.
pub struct CompressorProcessor {
    params: ParamValues,
    sample_rate: f32,
    /// Envelope of the key signal (linear peak).
    env: f32,
}

impl CompressorProcessor {
    pub fn new(params: ParamValues, config: &ProcessConfig) -> Self {
        Self {
            params,
            sample_rate: config.sample_rate as f32,
            env: 0.0,
        }
    }

    /// Smoothing coefficient for a time constant in milliseconds.
    fn coefficient(&self, ms: f32) -> f32 {
        (-1.0 / (ms.max(0.01) * 0.001 * self.sample_rate)).exp()
    }
}

impl PluginProcessor for CompressorProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let threshold = self.params.get(0);
        let ratio = self.params.get(1).max(1.0);
        let attack = self.coefficient(self.params.get(2));
        let release = self.coefficient(self.params.get(3));
        let makeup = self.params.get(4);
        let slope = 1.0 - 1.0 / ratio;
        let n = io.frames;
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        match io.audio_in.first() {
            Some(input) => out.copy_from(input),
            None => out.clear(),
        }
        let key = io.audio_in.get(1).or(io.audio_in.first());
        for i in 0..n {
            let level = key.map_or(0.0, |k| {
                (0..k.num_channels()).fold(0.0f32, |m, c| m.max(k.channel(c)[i].abs()))
            });
            let coeff = if level > self.env { attack } else { release };
            self.env = level + coeff * (self.env - level);
            let env_db = 20.0 * self.env.max(1e-9).log10();
            let reduction = (env_db - threshold).max(0.0) * slope;
            let gain = 10f32.powf((makeup - reduction) / 20.0);
            for c in 0..out.num_channels() {
                out.channel_mut(c)[i] *= gain;
            }
        }
        // Denormal-safe idle.
        if self.env < 1e-12 {
            self.env = 0.0;
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.env = 0.0;
    }
}
