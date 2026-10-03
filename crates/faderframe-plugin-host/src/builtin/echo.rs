use crate::{ParamValues, PluginProcessContext, PluginProcessor, ProcessConfig, ProcessStatus};
use faderframe_audio_graph::NodeIo;

const MAX_SECONDS: f64 = 2.0;

/// Stereo feedback delay with damping and optional ping-pong.
pub struct EchoProcessor {
    params: ParamValues,
    sample_rate: f32,
    buf: [Vec<f32>; 2],
    write: usize,
    /// Smoothed delay time in samples (avoids zipper noise when changed).
    delay: f32,
    damp_state: [f32; 2],
}

impl EchoProcessor {
    pub fn new(params: ParamValues, config: &ProcessConfig) -> Self {
        let len = (MAX_SECONDS * config.sample_rate) as usize + config.max_block_size as usize + 4;
        let delay = params.get(0) * 0.001 * config.sample_rate as f32;
        Self {
            params,
            sample_rate: config.sample_rate as f32,
            buf: [vec![0.0; len], vec![0.0; len]],
            write: 0,
            delay,
            damp_state: [0.0; 2],
        }
    }

    #[inline]
    fn read(buf: &[f32], write: usize, delay: f32) -> f32 {
        let len = buf.len();
        let d = delay.clamp(1.0, (len - 2) as f32);
        let di = d.floor() as usize;
        let frac = d - di as f32;
        let a = buf[(write + len - di) % len];
        let b = buf[(write + len - di - 1) % len];
        a + (b - a) * frac
    }
}

impl PluginProcessor for EchoProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let target = self.params.get(0) * 0.001 * self.sample_rate;
        let feedback = self.params.get(1).clamp(0.0, 0.95);
        let damping = self.params.get(2).clamp(0.0, 1.0);
        let mix = self.params.get(3).clamp(0.0, 1.0);
        let ping_pong = self.params.get(4) >= 0.5;
        let lp = 1.0 - damping * 0.85; // one-pole coefficient in the feedback path

        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return ProcessStatus::Continue;
        };
        let in_ch = input.num_channels();
        let out_ch = out.num_channels();
        let len = self.buf[0].len();
        for i in 0..io.frames {
            self.delay += (target - self.delay) * 0.0015;
            let xl = if in_ch > 0 { input.channel(0)[i] } else { 0.0 };
            let xr = if in_ch > 1 { input.channel(1)[i] } else { xl };
            let dl = Self::read(&self.buf[0], self.write, self.delay);
            let dr = Self::read(&self.buf[1], self.write, self.delay);
            self.damp_state[0] += lp * (dl - self.damp_state[0]);
            self.damp_state[1] += lp * (dr - self.damp_state[1]);
            let (fl, fr) = (self.damp_state[0] * feedback, self.damp_state[1] * feedback);
            if ping_pong {
                // Mono input enters on the left; repeats alternate sides.
                self.buf[0][self.write] = (xl + xr) * 0.5 + fr;
                self.buf[1][self.write] = fl;
            } else {
                self.buf[0][self.write] = xl + fl;
                self.buf[1][self.write] = xr + fr;
            }
            self.write = (self.write + 1) % len;
            let yl = xl * (1.0 - mix) + dl * mix;
            let yr = xr * (1.0 - mix) + dr * mix;
            if out_ch == 1 {
                out.channel_mut(0)[i] = 0.5 * (yl + yr);
            } else if out_ch >= 2 {
                out.channel_mut(0)[i] = yl;
                out.channel_mut(1)[i] = yr;
            }
        }
        for c in 2..out_ch {
            out.channel_mut(c).fill(0.0);
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        for b in &mut self.buf {
            b.fill(0.0);
        }
        self.damp_state = [0.0; 2];
    }
}
