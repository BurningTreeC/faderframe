use super::{MAX_CHANNELS, ramp_step};
use crate::context::EngineContext;
use crate::slots::ChainSlots;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_core::pan::stereo_balance;

/// The end of a container's chain: its level and balance (ramped, so
/// moves, mutes and solos never click), on the way to the container's
/// sum.
pub struct ChainMix {
    slots: ChainSlots,
    /// Gains applied at the end of the last block, per channel.
    last: [f32; MAX_CHANNELS],
}

impl ChainMix {
    pub fn new(slots: ChainSlots) -> Self {
        Self {
            slots,
            last: [f32::NAN; MAX_CHANNELS],
        }
    }
}

impl Processor<EngineContext> for ChainMix {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let params = &cx.data.params;
        let gain = params.get(self.slots.gain);
        let pan = params.get(self.slots.pan);
        let n = io.frames;
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        let channels = out.num_channels().min(MAX_CHANNELS);
        let (l, r) = if channels == 2 {
            stereo_balance(pan)
        } else {
            (1.0, 1.0)
        };
        for c in 0..channels {
            let target = gain * if c == 0 { l } else { r };
            let from = if self.last[c].is_nan() {
                target
            } else {
                self.last[c]
            };
            let step = ramp_step(from, target, n);
            let src = input.channel(c.min(input.num_channels().saturating_sub(1)));
            let mut g = from;
            for (o, x) in out.channel_mut(c).iter_mut().zip(src).take(n) {
                g += step;
                *o = x * g;
            }
            self.last[c] = target;
        }
    }

    fn reset(&mut self) {
        self.last = [f32::NAN; MAX_CHANNELS];
    }
}
