use super::{MAX_CHANNELS, ramp_step};
use crate::context::EngineContext;
use crate::slots::StripSlots;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor, for_each_channel_route};
use faderframe_core::{PanLaw, pan::stereo_balance};
use faderframe_realtime::MeterRange;

/// Channel strip: polarity, mute, fader, pan/balance and post-fader metering.
///
/// * input 0 — the track signal after inserts (track layout)
/// * output 0 — post-fader, panned, in the destination layout
/// * output 1 — pre-fader (post-insert, post-mute) in the track layout,
///   used by pre-fader sends
///
/// All gain changes are ramped linearly across the block, so fader moves,
/// mutes and solos never click. Pan law: mono sources use `pan_law`
/// (default constant-power, -3 dB centre); stereo sources use a 0 dB
/// balance control (see `faderframe_core::pan`).
pub struct ChannelStrip {
    slots: StripSlots,
    meter: MeterRange,
    pan_law: PanLaw,
    post: [f32; MAX_CHANNELS],
    pre: f32,
}

impl ChannelStrip {
    pub fn new(slots: StripSlots, meter: MeterRange, pan_law: PanLaw) -> Self {
        Self {
            slots,
            meter,
            pan_law,
            post: [f32::NAN; MAX_CHANNELS],
            pre: f32::NAN,
        }
    }
}

impl Processor<EngineContext> for ChannelStrip {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let params = &cx.data.params;
        let fader = params.get(self.slots.volume);
        let pan = params.get(self.slots.pan);
        let mute = params.get(self.slots.mute) >= 0.5;
        let polarity = if params.get(self.slots.phase) >= 0.5 {
            -1.0
        } else {
            1.0
        };
        let audible = if mute { 0.0 } else { polarity };

        let Some(input) = io.audio_in.first() else {
            return;
        };
        let n = io.frames;
        let in_ch = input.num_channels();

        // Pre-fader output (track layout).
        if let Some(pre) = io.audio_out.get_mut(1) {
            let from = if self.pre.is_nan() { audible } else { self.pre };
            let step = ramp_step(from, audible, n);
            for c in 0..pre.num_channels() {
                let src = input.channel(c.min(in_ch.saturating_sub(1)));
                let mut g = from;
                for (o, &x) in pre.channel_mut(c).iter_mut().zip(src) {
                    g += step;
                    *o = x * g;
                }
            }
            self.pre = audible;
        }

        // Post-fader output.
        let Some(post) = io.audio_out.first_mut() else {
            return;
        };
        let out_ch = post.num_channels().min(MAX_CHANNELS);
        let mut target = [0.0f32; MAX_CHANNELS];
        match (in_ch, out_ch) {
            (1, 2) => {
                let (l, r) = self.pan_law.mono_gains(pan);
                target[0] = l;
                target[1] = r;
            }
            (2, 2) => {
                let (l, r) = stereo_balance(pan);
                target[0] = l;
                target[1] = r;
            }
            _ => target[..out_ch].fill(1.0),
        }
        for t in &mut target[..out_ch] {
            *t *= fader * audible;
        }
        post.clear();
        for_each_channel_route(in_ch, post.num_channels(), |s, d, w| {
            if d >= MAX_CHANNELS {
                return;
            }
            let from = if self.post[d].is_nan() {
                target[d]
            } else {
                self.post[d]
            };
            let step = ramp_step(from, target[d], n);
            let mut g = from;
            for (o, &x) in post.channel_mut(d).iter_mut().zip(input.channel(s)) {
                g += step;
                *o += x * g * w;
            }
        });
        self.post[..out_ch].copy_from_slice(&target[..out_ch]);

        for c in 0..out_ch {
            if let Some(idx) = self.meter.channel(c) {
                cx.data.meters.measure(idx, post.channel(c));
            }
        }
        // Mono strips light both meter channels.
        if out_ch == 1
            && let Some(idx) = self.meter.channel(1)
        {
            cx.data.meters.measure(idx, post.channel(0));
        }
    }
}
