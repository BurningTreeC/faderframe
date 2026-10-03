use super::{AUTOMATION_STEP, MAX_CHANNELS, automation_at, ramp_step};
use crate::context::EngineContext;
use crate::slots::StripSlots;
use faderframe_audio_graph::{
    AudioBuffer, NodeIo, ProcessContext, Processor, for_each_channel_route,
};
use faderframe_automation::SampleLane;
use faderframe_core::{PanLaw, TrackId, db_to_gain, pan::stereo_balance};
use faderframe_realtime::MeterRange;

/// Channel strip: polarity, mute, fader, pan/balance and post-fader metering.
///
/// * input 0 — the track signal after inserts (track layout)
/// * output 0 — post-fader, panned, in the destination layout
/// * output 1 — pre-fader (post-insert, post-mute) in the track layout,
///   used by pre-fader sends
///
/// All gain changes are ramped linearly, so fader moves, mutes and solos
/// never click. Automated volume, pan and mute are evaluated every
/// [`AUTOMATION_STEP`] frames and ramped in between (breakpoints land within
/// that step; steps in the curve become short declicked ramps). Pan law:
/// mono sources use `pan_law` (default constant-power, -3 dB centre); stereo
/// sources use a 0 dB balance control (see `faderframe_core::pan`).
pub struct ChannelStrip {
    track: TrackId,
    slots: StripSlots,
    meter: MeterRange,
    pan_law: PanLaw,
    post: [f32; MAX_CHANNELS],
    pre: f32,
}

impl ChannelStrip {
    pub fn new(track: TrackId, slots: StripSlots, meter: MeterRange, pan_law: PanLaw) -> Self {
        Self {
            track,
            slots,
            meter,
            pan_law,
            post: [f32::NAN; MAX_CHANNELS],
            pre: f32::NAN,
        }
    }

    /// Gains for frames `off..off + m` reaching `fader`/`pan`/`audible` at
    /// the end of the range.
    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        input: &AudioBuffer,
        outs: &mut [AudioBuffer],
        off: usize,
        m: usize,
        fader: f32,
        pan: f32,
        audible: f32,
    ) {
        let in_ch = input.num_channels();
        // Pre-fader output (track layout).
        if let Some(pre) = outs.get_mut(1) {
            let from = if self.pre.is_nan() { audible } else { self.pre };
            let step = ramp_step(from, audible, m);
            for c in 0..pre.num_channels() {
                let src = &input.channel(c.min(in_ch.saturating_sub(1)))[off..off + m];
                let mut g = from;
                for (o, &x) in pre.channel_mut(c)[off..off + m].iter_mut().zip(src) {
                    g += step;
                    *o = x * g;
                }
            }
            self.pre = audible;
        }
        let Some(post) = outs.first_mut() else {
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
        let current = self.post;
        for_each_channel_route(in_ch, post.num_channels(), |s, d, w| {
            if d >= MAX_CHANNELS {
                return;
            }
            let from = if current[d].is_nan() {
                target[d]
            } else {
                current[d]
            };
            let step = ramp_step(from, target[d], m);
            let mut g = from;
            for (o, &x) in post.channel_mut(d)[off..off + m]
                .iter_mut()
                .zip(&input.channel(s)[off..off + m])
            {
                g += step;
                *o += x * g * w;
            }
        });
        self.post[..out_ch].copy_from_slice(&target[..out_ch]);
    }
}

impl Processor<EngineContext> for ChannelStrip {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let params = &cx.data.params;
        let static_fader = params.get(self.slots.volume);
        let static_pan = params.get(self.slots.pan);
        let static_mute = params.get(self.slots.mute) >= 0.5;
        let solo_muted = params.get(self.slots.solo_mute) >= 0.5;
        let polarity = if params.get(self.slots.phase) >= 0.5 {
            -1.0
        } else {
            1.0
        };
        let vca_gain = params.get(self.slots.vca);
        let auto = cx.data.timeline.automation(self.track);
        let (vol_lane, pan_lane, mute_lane) = auto.map_or((None, None, None), |a| {
            (a.volume.as_ref(), a.pan.as_ref(), a.mute.as_ref())
        });
        let (vca_volume, vca_mute): (&[SampleLane], &[SampleLane]) =
            auto.map_or((&[], &[]), |a| (&a.vca_volume, &a.vca_mute));
        let automated = vol_lane.is_some()
            || pan_lane.is_some()
            || mute_lane.is_some()
            || !vca_volume.is_empty()
            || !vca_mute.is_empty();

        let Some(input) = io.audio_in.first() else {
            return;
        };
        let n = io.frames;
        if let Some(post) = io.audio_out.first_mut() {
            post.clear();
        }
        let step = if automated { AUTOMATION_STEP } else { n.max(1) };
        let mut values = (static_fader, static_pan, static_mute);
        let mut off = 0;
        while off < n {
            let m = step.min(n - off);
            let at = automation_at(cx, off + m);
            let fader = vol_lane
                .and_then(|l| l.value_at(at))
                .map_or(static_fader, |db| db_to_gain(db as f32));
            let pan = pan_lane
                .and_then(|l| l.value_at(at))
                .map_or(static_pan, |v| v.clamp(-1.0, 1.0) as f32);
            let mute = mute_lane
                .and_then(|l| l.value_at(at))
                .map_or(static_mute, |v| v >= 0.5);
            let vca = vca_volume.iter().fold(vca_gain, |g, l| {
                l.value_at(at).map_or(g, |db| g * db_to_gain(db as f32))
            });
            let vca_muted = vca_mute
                .iter()
                .any(|l| l.value_at(at).is_some_and(|v| v >= 0.5));
            let audible = if mute || solo_muted || vca_muted {
                0.0
            } else {
                polarity
            };
            self.render(input, io.audio_out, off, m, fader * vca, pan, audible);
            values = (fader, pan, mute);
            off += m;
        }
        if automated {
            // What the faders show while automation plays.
            let rb = &cx.data.readback;
            rb.set(self.slots.volume, values.0);
            rb.set(self.slots.pan, values.1);
            rb.set(self.slots.mute, if values.2 { 1.0 } else { 0.0 });
        }

        let Some(post) = io.audio_out.first() else {
            return;
        };
        // The analysed track feeds the scope (mono: both sides alike).
        if cx.data.scope.source() == Some(self.track.raw()) && post.num_channels() > 0 {
            let l = &post.channel(0)[..n];
            let r = &post.channel(post.num_channels().min(2) - 1)[..n];
            cx.data.scope.push(l, r);
        }
        let out_ch = post.num_channels().min(MAX_CHANNELS);
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
