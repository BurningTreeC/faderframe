use crate::{PluginProcessContext, PluginProcessor, ProcessStatus};
use faderframe_audio_graph::NodeIo;

/// Pure delay of `latency` samples that reports the same latency, behaving
/// like a look-ahead plugin. Used to verify delay compensation end to end.
pub struct LatencyProcessor {
    latency: usize,
    /// One ring per output channel (up to 8).
    ring: Vec<Vec<f32>>,
    pos: usize,
}

impl LatencyProcessor {
    pub fn new(latency: u32) -> Self {
        let latency = latency as usize;
        Self {
            latency,
            ring: (0..8).map(|_| vec![0.0; latency]).collect(),
            pos: 0,
        }
    }
}

impl PluginProcessor for LatencyProcessor {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return ProcessStatus::Continue;
        };
        if self.latency == 0 {
            out.copy_from(input);
            return ProcessStatus::Continue;
        }
        let len = self.latency;
        let channels = out.num_channels().min(self.ring.len());
        let mut end = self.pos;
        for c in 0..channels {
            let src = input.channel(if c < input.num_channels() { c } else { 0 });
            let ring = &mut self.ring[c];
            let dst = out.channel_mut(c);
            let mut pos = self.pos;
            for (o, &x) in dst.iter_mut().zip(src) {
                *o = ring[pos];
                ring[pos] = x;
                pos += 1;
                if pos == len {
                    pos = 0;
                }
            }
            end = pos;
        }
        for c in channels..out.num_channels() {
            out.channel_mut(c).fill(0.0);
        }
        self.pos = end;
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        for r in &mut self.ring {
            r.fill(0.0);
        }
    }
}
