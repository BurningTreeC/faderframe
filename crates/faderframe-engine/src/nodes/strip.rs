use super::{AUTOMATION_STEP, MAX_CHANNELS, automation_at, ramp_step};
use crate::context::EngineContext;
use crate::slots::StripSlots;
use faderframe_audio_graph::{
    AudioBuffer, NodeIo, ProcessContext, Processor, for_each_channel_route,
};
use faderframe_automation::SampleLane;
use faderframe_core::surround::{self, MAX_SPEAKERS};
use faderframe_core::{
    ChannelLayout, FaderLaw, PanLaw, SendId, SurroundPan, TrackId, db_to_gain, gain_to_db,
    pan::stereo_balance,
};
use faderframe_realtime::{MeterRange, ParamSlot};

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
///
/// Modulators move the fader in travel (as a hand on the console's fader
/// would) and the pan, on top of their values (see [`crate::modulation`]).
///
/// Into a surround bed, or from one into another layout, the strip mixes
/// through a gain matrix instead: the surround panner's (see
/// `faderframe_core::surround`), every input-output pair ramped.
pub struct ChannelStrip {
    track: TrackId,
    slots: StripSlots,
    meter: MeterRange,
    pan_law: PanLaw,
    law: FaderLaw,
    post: [f32; MAX_CHANNELS],
    pre: f32,
    /// Mixing through the surround panner's matrix (a bed in or out).
    mix: Option<MatrixMix>,
    /// Rendered ahead: no meters, scope or shown values (a [`StripEcho`]
    /// publishes them when the audio is heard).
    quiet: bool,
    /// An object: no LFE send (objects have none, as in Atmos).
    object: bool,
    /// Its meters and scope are fed by another node (the master's, by the
    /// [`ObjectRenderer`] that adds the objects).
    metered_elsewhere: bool,
}

impl ChannelStrip {
    pub fn new(track: TrackId, slots: StripSlots, meter: MeterRange, pan_law: PanLaw) -> Self {
        Self {
            track,
            slots,
            meter,
            pan_law,
            law: FaderLaw::console(),
            post: [f32::NAN; MAX_CHANNELS],
            pre: f32::NAN,
            mix: None,
            quiet: false,
            object: false,
            metered_elsewhere: false,
        }
    }

    /// Panned as an object: everything but the LFE send.
    pub fn object(self) -> Self {
        Self {
            object: true,
            ..self
        }
    }

    /// Meters and scope fed by another node.
    pub fn metered_elsewhere(self) -> Self {
        Self {
            metered_elsewhere: true,
            ..self
        }
    }

    /// From `input` into `output` (the matrix when either is a surround
    /// bed).
    pub fn with_layouts(self, input: ChannelLayout, output: ChannelLayout) -> Self {
        Self {
            mix: MatrixMix::new(input, output),
            ..self
        }
    }

    /// Rendered ahead of the playhead (see [`StripEcho`]).
    pub fn quiet(self) -> Self {
        Self {
            quiet: true,
            ..self
        }
    }

    /// Frames `off..off + m` through the surround matrix, every pair's gain
    /// reaching `gain` × its matrix entry at the end of the range (the
    /// pre-fader output as [`Self::render`] makes it).
    fn render_matrix(
        &mut self,
        input: &AudioBuffer,
        outs: &mut [AudioBuffer],
        off: usize,
        m: usize,
        gain: f32,
        audible: f32,
    ) {
        if let Some(pre) = outs.get_mut(1) {
            let in_ch = input.num_channels();
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
        if let (Some(post), Some(mix)) = (outs.first_mut(), self.mix.as_mut()) {
            mix.mix(input, post, off, m, gain);
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
        let surround_lanes = auto.map(|a| &a.surround).filter(|_| self.mix.is_some());
        let automated = vol_lane.is_some()
            || pan_lane.is_some()
            || mute_lane.is_some()
            || !vca_volume.is_empty()
            || !vca_mute.is_empty()
            || surround_lanes.is_some_and(|l| l.iter().any(Option::is_some));

        let Some(input) = io.audio_in.first() else {
            return;
        };
        let n = io.frames;
        if let Some(post) = io.audio_out.first_mut() {
            post.clear();
        }
        let step = if automated { AUTOMATION_STEP } else { n.max(1) };
        let (travel, pan_mod) = cx
            .data
            .modulation
            .track(self.track)
            .map_or((0.0, 0.0), |m| m.strip());
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
            let (fader_m, pan_m) = if travel == 0.0 && pan_mod == 0.0 {
                (fader, pan)
            } else {
                let at = self.law.db_to_position(gain_to_db(fader)) + travel;
                (
                    db_to_gain(self.law.position_to_db(at.clamp(0.0, 1.0))),
                    (pan + pan_mod).clamp(-1.0, 1.0),
                )
            };
            if let Some(mix) = self.mix.as_mut() {
                let mut pan = surround_at(&self.slots.surround, params, surround_lanes, at);
                if self.object {
                    pan.lfe_db = surround::LFE_OFF_DB;
                }
                mix.pan(&pan);
                self.render_matrix(
                    input,
                    io.audio_out,
                    off,
                    m,
                    fader_m * vca * audible,
                    audible,
                );
            } else {
                self.render(input, io.audio_out, off, m, fader_m * vca, pan_m, audible);
            }
            values = (fader, pan, mute);
            off += m;
        }
        if self.quiet {
            return;
        }
        if automated {
            // What the faders show while automation plays.
            let rb = &cx.data.readback;
            rb.set(self.slots.volume, values.0);
            rb.set(self.slots.pan, values.1);
            rb.set(self.slots.mute, if values.2 { 1.0 } else { 0.0 });
            if let Some(pan) = self.mix.as_ref().and_then(MatrixMix::made_for) {
                set_surround(rb, &self.slots.surround, &pan);
            }
        }

        if self.metered_elsewhere {
            return;
        }
        if let Some(post) = io.audio_out.first() {
            publish(cx, self.track, self.meter, post, n);
        }
    }
}

/// The master's output with the objects added, as a renderer adds them to
/// the bed after the master strip (objects skip its inserts and fader):
/// what is heard, metered as the master's.
pub struct ObjectRenderer {
    master: TrackId,
    meter: MeterRange,
}

impl ObjectRenderer {
    pub fn new(master: TrackId, meter: MeterRange) -> Self {
        Self { master, meter }
    }
}

impl Processor<EngineContext> for ObjectRenderer {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let n = io.frames;
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        for c in 0..input.num_channels().min(out.num_channels()) {
            out.channel_mut(c)[..n].copy_from_slice(&input.channel(c)[..n]);
        }
        if let Some(out) = io.audio_out.first() {
            publish(cx, self.master, self.meter, out, n);
        }
    }
}

/// One layout mixed into another through the surround panner's matrix
/// (`faderframe_core::surround::matrix`): every pair of channels ramped to
/// its new gain over a range. Strips and sends into or out of a bed.
pub(crate) struct MatrixMix {
    layouts: (ChannelLayout, ChannelLayout),
    matrix: [[f32; MAX_SPEAKERS]; MAX_SPEAKERS],
    /// The panner `matrix` was made for.
    made_for: Option<SurroundPan>,
    /// Each pair's gain at the end of the last range.
    pairs: [[f32; MAX_SPEAKERS]; MAX_SPEAKERS],
}

impl MatrixMix {
    /// `None` when neither side is a bed (the graph's channel rules do).
    pub(crate) fn new(input: ChannelLayout, output: ChannelLayout) -> Option<Self> {
        let mut matrix = [[0.0; MAX_SPEAKERS]; MAX_SPEAKERS];
        let pan = SurroundPan::default();
        surround::matrix(input, output, &pan, &mut matrix).then_some(Self {
            layouts: (input, output),
            matrix,
            made_for: Some(pan),
            pairs: [[f32::NAN; MAX_SPEAKERS]; MAX_SPEAKERS],
        })
    }

    /// Place by `pan` (the matrix is remade when it changed).
    pub(crate) fn pan(&mut self, pan: &SurroundPan) {
        if self.made_for != Some(*pan) {
            surround::matrix(self.layouts.0, self.layouts.1, pan, &mut self.matrix);
            self.made_for = Some(*pan);
        }
    }

    pub(crate) fn made_for(&self) -> Option<SurroundPan> {
        self.made_for
    }

    /// Add frames `off..off + m` of `input` into `out`, each pair reaching
    /// its matrix entry × `gain` at the end of the range.
    pub(crate) fn mix(
        &mut self,
        input: &AudioBuffer,
        out: &mut AudioBuffer,
        off: usize,
        m: usize,
        gain: f32,
    ) {
        let ins = input.num_channels().min(MAX_SPEAKERS);
        let outs = out.num_channels().min(MAX_SPEAKERS);
        for s in 0..ins {
            for d in 0..outs {
                let target = self.matrix[s][d] * gain;
                let last = self.pairs[s][d];
                let from = if last.is_nan() { target } else { last };
                self.pairs[s][d] = target;
                if from == 0.0 && target == 0.0 {
                    continue;
                }
                let step = ramp_step(from, target, m);
                let mut g = from;
                for (o, &x) in out.channel_mut(d)[off..off + m]
                    .iter_mut()
                    .zip(&input.channel(s)[off..off + m])
                {
                    g += step;
                    *o += x * g;
                }
            }
        }
    }
}

/// The surround panner at `at`: each value from its lane where one drives
/// it, else from its slot.
pub(crate) fn surround_at(
    slots: &[ParamSlot; 6],
    params: &faderframe_realtime::ParamTable,
    lanes: Option<&[Option<SampleLane>; 6]>,
    at: i64,
) -> SurroundPan {
    let mut pan = SurroundPan::default();
    for p in faderframe_core::SurroundParam::ALL {
        let i = p.index();
        let v = lanes
            .and_then(|l| l[i].as_ref())
            .and_then(|l| l.value_at(at))
            .map_or(params.get(slots[i]), |v| v as f32);
        pan = p.set(pan, v);
    }
    pan
}

fn set_surround(rb: &faderframe_realtime::ParamTable, slots: &[ParamSlot; 6], pan: &SurroundPan) {
    for p in faderframe_core::SurroundParam::ALL {
        rb.set(slots[p.index()], p.get(pan));
    }
}

/// A strip's post-fader output to its meters and (when it is the analysed
/// track) the scope.
fn publish(
    cx: &ProcessContext<'_, EngineContext>,
    track: TrackId,
    meter: MeterRange,
    post: &AudioBuffer,
    n: usize,
) {
    // The analysed track feeds the scope (mono: both sides alike).
    if !cx.data.preview_active
        && cx.data.scope.source() == Some(track.raw())
        && post.num_channels() > 0
    {
        let l = &post.channel(0)[..n];
        let r = &post.channel(post.num_channels().min(2) - 1)[..n];
        cx.data.scope.push(track.raw(), l, r);
    }
    let out_ch = post.num_channels().min(MAX_CHANNELS);
    for c in 0..out_ch {
        if let Some(idx) = meter.channel(c) {
            cx.data.meters.measure(idx, post.channel(c));
        }
    }
    // Mono strips light both meter channels.
    if out_ch == 1
        && let Some(idx) = meter.channel(1)
    {
        cx.data.meters.measure(idx, post.channel(0));
    }
}

/// A channel strip rendered ahead ([`crate::ahead`]) as it is heard: its
/// post-fader audio, back from the ring, goes to the meters and the scope,
/// and its automated fader, pan, mute and send levels are shown — what the
/// strip and its sends do on the audio thread (rendered ahead they would
/// be early, so they are quiet there).
pub struct StripEcho {
    track: TrackId,
    slots: StripSlots,
    meter: MeterRange,
    sends: Vec<(SendId, ParamSlot)>,
}

impl StripEcho {
    pub fn new(
        track: TrackId,
        slots: StripSlots,
        meter: MeterRange,
        sends: Vec<(SendId, ParamSlot)>,
    ) -> Self {
        Self {
            track,
            slots,
            meter,
            sends,
        }
    }
}

impl Processor<EngineContext> for StripEcho {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let n = io.frames;
        if let Some(auto) = cx.data.timeline.automation(self.track) {
            // The values at the block's end, as the strip shows them.
            let at = automation_at(cx, n);
            let rb = &cx.data.readback;
            let params = &cx.data.params;
            if auto.volume.is_some()
                || auto.pan.is_some()
                || auto.mute.is_some()
                || !auto.vca_volume.is_empty()
                || !auto.vca_mute.is_empty()
            {
                let fader = auto
                    .volume
                    .as_ref()
                    .and_then(|l| l.value_at(at))
                    .map_or(params.get(self.slots.volume), |db| db_to_gain(db as f32));
                let pan = auto
                    .pan
                    .as_ref()
                    .and_then(|l| l.value_at(at))
                    .map_or(params.get(self.slots.pan), |v| v.clamp(-1.0, 1.0) as f32);
                let mute = auto
                    .mute
                    .as_ref()
                    .and_then(|l| l.value_at(at))
                    .map_or(params.get(self.slots.mute) >= 0.5, |v| v >= 0.5);
                rb.set(self.slots.volume, fader);
                rb.set(self.slots.pan, pan);
                rb.set(self.slots.mute, if mute { 1.0 } else { 0.0 });
            }
            if auto.surround.iter().any(Option::is_some) {
                let pan = surround_at(&self.slots.surround, params, Some(&auto.surround), at);
                set_surround(rb, &self.slots.surround, &pan);
            }
            for (send, level) in &self.sends {
                if let Some(l) = auto.send(*send) {
                    let v = l
                        .value_at(at)
                        .map_or(params.get(*level), |db| db_to_gain(db as f32));
                    rb.set(*level, v);
                }
            }
        }
        if let Some(post) = io.audio_in.first() {
            publish(cx, self.track, self.meter, post, n);
        }
    }
}
