use super::{AUTOMATION_STEP, MAX_CHANNELS, automation_at, ramp_step};
use crate::context::EngineContext;
use crate::slots::{MixSlots, StripSlots};
use faderframe_audio_graph::{
    AudioBuffer, NodeIo, ProcessContext, Processor, for_each_channel_route,
};
use faderframe_automation::SampleLane;
use faderframe_core::pan::{self, PanMatrix};
use faderframe_core::surround::{
    self, Downmix, MAX_SPEAKERS, Mixing, SURROUND_VALUES, StereoSource, SurroundPanner,
};
use faderframe_core::{
    ChannelLayout, DualPan, FaderLaw, PanLaw, SendId, SurroundPan, TrackId, db_to_gain, gain_to_db,
};
use faderframe_realtime::{MeterRange, ParamSlot};

/// Channel strip: polarity, mute, fader, pan/balance and post-fader metering.
///
/// * input 0 — the track signal after inserts (track layout)
/// * output 0 — post-fader, panned, in the destination layout
/// * output 1 — pre-fader (post-insert, post-mute) in the track layout,
///   unpanned, used by pre-fader sends
/// * outputs 2 and 3 (only while the project has cue mixes) — the cue
///   mixes' taps, with the polarity and never muted (neither the track's
///   mute nor a solo elsewhere reaches a performer's headphones): 2
///   before the fader, in the track layout, unpanned; 3 the post-fader
///   image — output 0 (fader, VCAs, modulation and panning as the main
///   mix has them, in the destination layout) without the mute
/// * a bus's cue path has a strip of its own (`solo_safe`): the mute,
///   the solos and VCA mutes never reach any of its outputs
///
/// All gain changes are ramped linearly, so fader moves, mutes and solos
/// never click. Automated volume, pan and mute are evaluated every
/// [`AUTOMATION_STEP`] frames and ramped in between (breakpoints land within
/// that step; steps in the curve become short declicked ramps).
///
/// Into a stereo destination a mono or stereo input goes through a 2 × 2
/// matrix ([`Placement`], see `faderframe_core::pan`): a mono source by the
/// project's pan law (a parameter slot, so changing it needs no rebuild), a
/// stereo source by balance or, in dual-pan mode, each channel by the law
/// from its own position. Every source/destination pair ramps on its own.
/// A position that jumps (a knob, a typed value, a step in a lane) is
/// interpolated as a position across the range, a matrix every
/// [`GLIDE_STEPS_PER_UNIT`]th of the pan range it moves, so it follows the
/// law on its way instead of crossfading the gains. Into a
/// mono destination, or between other layouts, the graph's channel rules
/// apply and pan has nothing to do.
///
/// Modulators move the fader in travel (as a hand on the console's fader
/// would) and the pan, on top of their values (see [`crate::modulation`]).
///
/// Into a surround bed, or from one into another layout, the strip mixes
/// through a gain matrix instead: the surround panner's (room or VBAP; a
/// stereo source as an image, balanced on L/R by the strip's balance, or
/// dual), or a bed's fold by the project's downmix (see
/// `faderframe_core::surround`), every input-output pair ramped. Objects
/// are always placed by the room panner, as an image (their ADM metadata
/// is the room position).
pub struct ChannelStrip {
    track: TrackId,
    slots: StripSlots,
    meter: MeterRange,
    /// The project's pan law and downmix.
    shared: MixSlots,
    law: FaderLaw,
    layouts: (ChannelLayout, ChannelLayout),
    pre: f32,
    /// The cue mixes' pre-fader tap's gain at the end of the last range
    /// (NaN: none yet).
    cue_pre: f32,
    /// The post-fader image (output 0): its placement and ramps.
    main: Stage,
    /// The cue mixes' post-fader image (output 3: the same, never muted),
    /// with ramps of its own.
    cue: Option<Stage>,
    /// Rendered ahead: no meters, scope or shown values (a [`StripEcho`]
    /// publishes them when the audio is heard).
    quiet: bool,
    /// A bus's cue path: the track's mute, the solos of the main mix and
    /// VCA mutes never reach it (the fader, VCAs and polarity do).
    solo_safe: bool,
    /// A linear gain on top of the fader (a cue mix dimmed for talkback).
    trim: Option<ParamSlot>,
    /// An object: no LFE send (objects have none, as in Atmos).
    object: bool,
    /// Its meters and scope are fed by another node (the master's, by the
    /// [`ObjectRenderer`] that adds the objects).
    metered_elsewhere: bool,
    /// How late its input is (its automation is read that much earlier).
    input_latency: u32,
}

/// One post-fader image of a strip's input: into stereo through a
/// [`Placement`] (with glides), into or out of a bed through the
/// panner's matrix, otherwise by the graph's channel rules — and the
/// ramp state of each.
struct Stage {
    post: [f32; MAX_CHANNELS],
    /// Into stereo: the placement and the gain (fader × VCA × mute) at the
    /// end of the last range, and every pair's gain there (NaN: none yet).
    placed: Option<Placement>,
    gain: f32,
    pairs: PanMatrix,
    /// Mixing through the surround panner's matrix (a bed in or out).
    mix: Option<MatrixMix>,
}

impl Stage {
    fn new(input: ChannelLayout, output: ChannelLayout) -> Self {
        Self {
            post: [f32::NAN; MAX_CHANNELS],
            placed: None,
            gain: f32::NAN,
            pairs: [[f32::NAN; 2]; 2],
            mix: MatrixMix::new(input, output),
        }
    }

    fn with_diffuser(self, rate: f64) -> Self {
        Self {
            mix: self.mix.map(|m| m.with_diffuser(rate)),
            ..self
        }
    }

    /// Frames `off..off + m` of `input` into `post` through the surround
    /// matrix, every pair's gain reaching `gain` × its matrix entry at the
    /// end of the range.
    fn render_matrix(
        &mut self,
        input: &AudioBuffer,
        post: &mut AudioBuffer,
        off: usize,
        m: usize,
        gain: f32,
    ) {
        if let Some(mix) = self.mix.as_mut() {
            mix.mix(input, post, off, m, gain);
        }
    }

    /// Frames `off..off + m` of `input` into `post`, the gains reaching
    /// `gain` through `place` at the end of the range.
    fn render(
        &mut self,
        input: &AudioBuffer,
        post: &mut AudioBuffer,
        off: usize,
        m: usize,
        gain: f32,
        place: Placement,
    ) {
        let in_ch = input.num_channels();
        if post.num_channels() == 2 && (1..=2).contains(&in_ch) {
            self.render_stereo(input, post, off, m, gain, place);
            return;
        }
        // Other layouts: the graph's channel rules, nothing to pan.
        let out_ch = post.num_channels().min(MAX_CHANNELS);
        let mut target = [0.0f32; MAX_CHANNELS];
        target[..out_ch].fill(gain);
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

    /// Frames `off..off + m` of a mono or stereo input added into a stereo
    /// output through `place`'s matrix, each pair reaching its gain ×
    /// `gain` at the end of the range. When only positions moved (same
    /// mode and law) they glide: matrices from positions interpolated
    /// across the range (one per [`GLIDE_STEPS_PER_UNIT`]th of the range
    /// moved, at most one a frame), the gains ramped linearly between.
    fn render_stereo(
        &mut self,
        input: &AudioBuffer,
        post: &mut AudioBuffer,
        off: usize,
        m: usize,
        gain: f32,
        place: Placement,
    ) {
        let from_place = self.placed.unwrap_or(place);
        let from_gain = if self.gain.is_nan() { gain } else { self.gain };
        let steps = if from_place != place && from_place.glides_to(&place) {
            let wanted = (from_place.distance(&place) * GLIDE_STEPS_PER_UNIT).ceil();
            (wanted as usize).clamp(1, m.max(1))
        } else {
            1
        };
        let ins = input.num_channels().min(2);
        let mut start = off;
        for i in 1..=steps {
            let end = off + m * i / steps;
            let (p, g) = if i == steps {
                (place, gain)
            } else {
                let t = i as f32 / steps as f32;
                (
                    from_place.toward(place, t),
                    from_gain + (gain - from_gain) * t,
                )
            };
            let target = pan::scaled(p.matrix(), g);
            let len = end - start;
            for (s, (row, pairs)) in target.iter().zip(&mut self.pairs).enumerate().take(ins) {
                let src = &input.channel(s)[start..end];
                for (d, (&to, pair)) in row.iter().zip(pairs.iter_mut()).enumerate() {
                    let from = if pair.is_nan() { to } else { *pair };
                    *pair = to;
                    if from == 0.0 && to == 0.0 {
                        continue;
                    }
                    let step = ramp_step(from, to, len);
                    let mut g = from;
                    for (o, &x) in post.channel_mut(d)[start..end].iter_mut().zip(src) {
                        g += step;
                        *o += x * g;
                    }
                }
            }
            start = end;
        }
        self.placed = Some(place);
        self.gain = gain;
    }
}

impl ChannelStrip {
    /// `shared`: the slots of the project's pan law and downmix.
    pub fn new(track: TrackId, slots: StripSlots, meter: MeterRange, shared: MixSlots) -> Self {
        Self {
            track,
            slots,
            meter,
            shared,
            law: FaderLaw::console(),
            layouts: (ChannelLayout::Stereo, ChannelLayout::Stereo),
            pre: f32::NAN,
            cue_pre: f32::NAN,
            main: Stage::new(ChannelLayout::Stereo, ChannelLayout::Stereo),
            cue: None,
            quiet: false,
            solo_safe: false,
            trim: None,
            object: false,
            metered_elsewhere: false,
            input_latency: 0,
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
            layouts: (input, output),
            main: Stage::new(input, output),
            cue: self.cue.map(|_| Stage::new(input, output)),
            ..self
        }
    }

    /// With the cue mixes' post-fader image (output 3): the post-fader
    /// output as [`Self::with_layouts`] makes it, never muted (call after
    /// `with_layouts`).
    pub fn with_cue_image(self) -> Self {
        let (input, output) = self.layouts;
        Self {
            cue: Some(Stage::new(input, output)),
            ..self
        }
    }

    /// With decorrelation filters for a diffuse spread into its bed.
    pub fn with_diffuser(self, rate: f64) -> Self {
        Self {
            main: self.main.with_diffuser(rate),
            cue: self.cue.map(|c| c.with_diffuser(rate)),
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

    /// A bus's cue path: mutes and solos never reach it.
    pub fn solo_safe(self) -> Self {
        Self {
            solo_safe: true,
            ..self
        }
    }

    /// A linear gain on top of the fader, from a parameter slot (a cue
    /// mix dimmed while the engineer talks).
    pub fn with_trim(self, trim: ParamSlot) -> Self {
        Self {
            trim: Some(trim),
            ..self
        }
    }

    /// The cue mixes' pre-fader tap (output 2, when the node has it) for
    /// frames `off..off + m`: the input with its polarity, never muted.
    fn render_cue_pre(
        &mut self,
        input: &AudioBuffer,
        outs: &mut [AudioBuffer],
        off: usize,
        m: usize,
        polarity: f32,
    ) {
        let Some(out) = outs.get_mut(2) else {
            return;
        };
        let in_ch = input.num_channels();
        let from = if self.cue_pre.is_nan() {
            polarity
        } else {
            self.cue_pre
        };
        self.cue_pre = polarity;
        let step = ramp_step(from, polarity, m);
        for c in 0..out.num_channels() {
            let src = &input.channel(c.min(in_ch.saturating_sub(1)))[off..off + m];
            let mut g = from;
            for (o, &x) in out.channel_mut(c)[off..off + m].iter_mut().zip(src) {
                g += step;
                *o = x * g;
            }
        }
    }

    /// The pre-fader output (1, track layout) for frames `off..off + m`,
    /// reaching `audible` at the end of the range.
    fn render_pre(
        &mut self,
        input: &AudioBuffer,
        outs: &mut [AudioBuffer],
        off: usize,
        m: usize,
        audible: f32,
    ) {
        let Some(pre) = outs.get_mut(1) else {
            return;
        };
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
}

/// Matrices per unit of pan position a glide moves (hard left to hard
/// right is 2 units: 32 matrices). Between two of them the gains ramp
/// linearly; a 16th of the range apart, the −3 dB law's power sags at most
/// 0.003 dB on the way.
const GLIDE_STEPS_PER_UNIT: f32 = 16.0;

/// How a strip places a mono or stereo input into a stereo destination
/// (positions already made safe: in range, never NaN).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    /// A mono source by the pan law.
    Mono { law: PanLaw, pan: f32 },
    /// A stereo source's balance.
    Balance { pan: f32 },
    /// A stereo source dual-panned by the pan law.
    Dual { law: PanLaw, pan: DualPan },
}

impl Placement {
    fn matrix(self) -> PanMatrix {
        match self {
            Placement::Mono { law, pan } => pan::mono_pan(law, pan),
            Placement::Balance { pan } => pan::balance(pan),
            Placement::Dual { law, pan } => pan::dual_pan(law, pan),
        }
    }

    /// Can the positions glide from here to `to` (same mode and law)?
    fn glides_to(&self, to: &Self) -> bool {
        match (self, to) {
            (Placement::Mono { law: a, .. }, Placement::Mono { law: b, .. })
            | (Placement::Dual { law: a, .. }, Placement::Dual { law: b, .. }) => a == b,
            (Placement::Balance { .. }, Placement::Balance { .. }) => true,
            _ => false,
        }
    }

    /// How far the positions move to `to` (the larger side of a dual pan).
    fn distance(&self, to: &Self) -> f32 {
        match (self, to) {
            (Placement::Mono { pan: a, .. }, Placement::Mono { pan: b, .. })
            | (Placement::Balance { pan: a }, Placement::Balance { pan: b }) => (a - b).abs(),
            (Placement::Dual { pan: a, .. }, Placement::Dual { pan: b, .. }) => {
                (a.left - b.left).abs().max((a.right - b.right).abs())
            }
            _ => 0.0,
        }
    }

    /// The positions a fraction `t` of the way to `to` (`to` itself when
    /// they cannot glide).
    fn toward(self, to: Self, t: f32) -> Self {
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        match (self, to) {
            (Placement::Mono { pan: a, .. }, Placement::Mono { law, pan: b }) => Placement::Mono {
                law,
                pan: lerp(a, b),
            },
            (Placement::Balance { pan: a }, Placement::Balance { pan: b }) => {
                Placement::Balance { pan: lerp(a, b) }
            }
            (Placement::Dual { pan: a, .. }, Placement::Dual { law, pan: b }) => Placement::Dual {
                law,
                pan: DualPan {
                    left: lerp(a.left, b.left),
                    right: lerp(a.right, b.right),
                },
            },
            _ => to,
        }
    }
}

impl Processor<EngineContext> for ChannelStrip {
    fn set_input_latency(&mut self, frames: u32) {
        self.input_latency = frames;
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let params = &cx.data.params;
        let static_fader = params.get(self.slots.volume);
        let static_pan = params.get(self.slots.pan);
        let static_dual = DualPan {
            left: params.get(self.slots.pan_left),
            right: params.get(self.slots.pan_right),
        };
        let pan_law = PanLaw::from_index(params.get(self.shared.pan_law));
        let downmix = self.shared.downmix(params);
        let static_mute = params.get(self.slots.mute) >= 0.5 && !self.solo_safe;
        let solo_muted = params.get(self.slots.solo_mute) >= 0.5 && !self.solo_safe;
        let polarity = if params.get(self.slots.phase) >= 0.5 {
            -1.0
        } else {
            1.0
        };
        let vca_gain = params.get(self.slots.vca);
        let trim = self.trim.map_or(1.0, |s| params.get(s).max(0.0));
        let auto = cx.data.timeline.automation(self.track);
        let (vol_lane, pan_lane, mute_lane) = auto.map_or((None, None, None), |a| {
            (
                a.volume.as_ref(),
                a.pan.as_ref(),
                a.mute.as_ref().filter(|_| !self.solo_safe),
            )
        });
        let Some(input) = io.audio_in.first() else {
            return;
        };
        // Dual pan: a stereo source in that mode (its lanes drive it then).
        let dual = input.num_channels() == 2 && params.get(self.slots.pan_mode) >= 0.5;
        let (left_lane, right_lane) = auto.filter(|_| dual).map_or((None, None), |a| {
            (a.pan_left.as_ref(), a.pan_right.as_ref())
        });
        let (vca_volume, vca_mute): (&[SampleLane], &[SampleLane]) =
            auto.map_or((&[], &[]), |a| (&a.vca_volume, &a.vca_mute));
        let vca_mute: &[SampleLane] = if self.solo_safe { &[] } else { vca_mute };
        let surround_lanes = auto
            .map(|a| &a.surround)
            .filter(|_| self.main.mix.is_some());
        let automated = vol_lane.is_some()
            || pan_lane.is_some()
            || left_lane.is_some()
            || right_lane.is_some()
            || mute_lane.is_some()
            || !vca_volume.is_empty()
            || !vca_mute.is_empty()
            || surround_lanes.is_some_and(|l| l.iter().any(Option::is_some));

        let n = io.frames;
        if let Some(post) = io.audio_out.first_mut() {
            post.clear();
        }
        if let Some(image) = io.audio_out.get_mut(3) {
            image.clear();
        }
        let step = if automated { AUTOMATION_STEP } else { n.max(1) };
        // The modulation its audio passed the modulators with.
        let came_in = cx.data.history.chunk_start() - i64::from(self.input_latency);
        let (travel, pan_mod) = cx
            .data
            .modulation
            .track(self.track)
            .map_or((0.0, 0.0), |m| m.strip_at(m.bus.slot_at(came_in)));
        let mut values = (static_fader, static_pan, static_mute, static_dual);
        let mut off = 0;
        while off < n {
            let m = step.min(n - off);
            let at = automation_at(cx, self.input_latency, off + m);
            let fader = vol_lane
                .and_then(|l| l.value_at(at))
                .map_or(static_fader, |db| db_to_gain(db as f32));
            let pan = pan_lane
                .and_then(|l| l.value_at(at))
                .map_or(static_pan, |v| v.clamp(-1.0, 1.0) as f32);
            let mute = mute_lane
                .and_then(|l| l.value_at(at))
                .map_or(static_mute, |v| v >= 0.5);
            let side = |lane: Option<&SampleLane>, v: f32| {
                lane.and_then(|l| l.value_at(at)).map_or(v, |v| v as f32)
            };
            let dual_pan = DualPan {
                left: side(left_lane, static_dual.left),
                right: side(right_lane, static_dual.right),
            }
            .clamped();
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
            let fader_m = fader_m * trim;
            // Pan modulation moves a dual-panned image as a whole.
            let place = if input.num_channels() != 2 {
                Placement::Mono {
                    law: pan_law,
                    pan: pan::position(pan_m),
                }
            } else if dual {
                Placement::Dual {
                    law: pan_law,
                    pan: DualPan {
                        left: dual_pan.left + pan_mod,
                        right: dual_pan.right + pan_mod,
                    }
                    .clamped(),
                }
            } else {
                Placement::Balance {
                    pan: pan::position(pan_m),
                }
            };
            self.render_cue_pre(input, io.audio_out, off, m, polarity);
            self.render_pre(input, io.audio_out, off, m, audible);
            if self.main.mix.is_some() {
                let mut pan = surround_at(&self.slots.surround, params, surround_lanes, at);
                if self.object {
                    pan.lfe_db = surround::LFE_OFF_DB;
                    pan.panner = SurroundPanner::Room;
                    pan.stereo = StereoSource::Image;
                    pan.diffuse = false;
                }
                let (post, rest) = io.audio_out.split_at_mut(1);
                if let (Some(post), Some(mix)) = (post.first_mut(), self.main.mix.as_mut()) {
                    mix.pan(&pan, pan_m, downmix);
                    self.main
                        .render_matrix(input, post, off, m, fader_m * vca * audible);
                }
                if let (Some(image), Some(cue)) = (rest.get_mut(2), self.cue.as_mut()) {
                    if let Some(mix) = cue.mix.as_mut() {
                        mix.pan(&pan, pan_m, downmix);
                    }
                    cue.render_matrix(input, image, off, m, fader_m * vca * polarity);
                }
            } else {
                let (post, rest) = io.audio_out.split_at_mut(1);
                if let Some(post) = post.first_mut() {
                    self.main
                        .render(input, post, off, m, fader_m * vca * audible, place);
                }
                if let (Some(image), Some(cue)) = (rest.get_mut(2), self.cue.as_mut()) {
                    cue.render(input, image, off, m, fader_m * vca * polarity, place);
                }
            }
            values = (fader, pan, mute, dual_pan);
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
            rb.set(self.slots.pan_left, values.3.left);
            rb.set(self.slots.pan_right, values.3.right);
            if let Some(pan) = self.main.mix.as_ref().and_then(MatrixMix::made_for) {
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
    /// The destination bed's VBAP geometry, made with the node (off the
    /// audio thread).
    vbap: Option<Box<faderframe_core::vbap::Vbap>>,
    /// What `matrix` was made for: the panner, the balance, the downmix.
    made_for: Option<(SurroundPan, f32, Downmix)>,
    /// Each pair's gain at the end of the last range.
    pairs: [[f32; MAX_SPEAKERS]; MAX_SPEAKERS],
    /// A diffuse spread's decorrelation filters (made for tracks whose
    /// spread is diffuse), the field's gain wanted and as last applied.
    diffuser: Option<Box<super::diffuse::Diffuser>>,
    diffuse: f32,
    diffuse_last: f32,
}

impl MatrixMix {
    /// `None` when neither side is a bed (the graph's channel rules do).
    pub(crate) fn new(input: ChannelLayout, output: ChannelLayout) -> Option<Self> {
        let mut matrix = [[0.0; MAX_SPEAKERS]; MAX_SPEAKERS];
        let vbap = match output {
            ChannelLayout::Surround(f) => Some(Box::new(faderframe_core::vbap::Vbap::of(f))),
            _ => None,
        };
        let pan = SurroundPan::default();
        let downmix = Downmix::default();
        let mixing = Mixing {
            vbap: vbap.as_deref(),
            downmix,
            balance: 0.0,
            diffuse: false,
        };
        let applies = surround::matrix(input, output, &pan, &mixing, &mut matrix);
        applies.then_some(Self {
            layouts: (input, output),
            matrix,
            vbap,
            made_for: Some((pan, 0.0, downmix)),
            pairs: [[f32::NAN; MAX_SPEAKERS]; MAX_SPEAKERS],
            diffuser: None,
            diffuse: 0.0,
            diffuse_last: f32::NAN,
        })
    }

    /// With decorrelation filters for a diffuse spread (at `rate`).
    pub(crate) fn with_diffuser(mut self, rate: f64) -> Self {
        self.diffuser = super::diffuse::Diffuser::new(self.layouts.1, rate).map(Box::new);
        // Remade for it at the first `pan`.
        self.made_for = None;
        self
    }

    /// Place by `pan` (`balance`: the strip's, for a stereo source
    /// balanced into the bed; `downmix`: how a bed folds), the matrix
    /// remade when any of them changed.
    pub(crate) fn pan(&mut self, pan: &SurroundPan, balance: f32, downmix: Downmix) {
        // The balance only counts where it is used.
        let balance = if pan.stereo == StereoSource::Balance {
            balance
        } else {
            0.0
        };
        let key = (*pan, balance, downmix);
        if self.made_for != Some(key) {
            let mixing = Mixing {
                vbap: self.vbap.as_deref(),
                downmix,
                balance,
                diffuse: self.diffuser.is_some(),
            };
            surround::matrix(
                self.layouts.0,
                self.layouts.1,
                pan,
                &mixing,
                &mut self.matrix,
            );
            // Without filters (made before it was asked for) a diffuse
            // spread stays coherent: the matrix has it all.
            self.diffuse = if self.diffuser.is_some() {
                surround::diffuse_gain(self.layouts.0, self.layouts.1, pan)
            } else {
                0.0
            };
            self.made_for = Some(key);
        }
    }

    pub(crate) fn made_for(&self) -> Option<SurroundPan> {
        self.made_for.map(|(p, _, _)| p)
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
        // The diffuse field.
        if let Some(diffuser) = self.diffuser.as_mut() {
            let target = self.diffuse * gain;
            let from = if self.diffuse_last.is_nan() {
                target
            } else {
                self.diffuse_last
            };
            self.diffuse_last = target;
            if from != 0.0 || target != 0.0 {
                diffuser.mix(input, out, off, m, from, target);
            }
        }
    }
}

/// The surround panner at `at`: its values from the slots, each
/// [`faderframe_core::SurroundParam`] from its lane where one drives it.
pub(crate) fn surround_at(
    slots: &[ParamSlot; SURROUND_VALUES],
    params: &faderframe_realtime::ParamTable,
    lanes: Option<&[Option<SampleLane>; faderframe_core::SurroundParam::ALL.len()]>,
    at: i64,
) -> SurroundPan {
    let values = slots.map(|s| params.get(s));
    let pan = SurroundPan::from_values(&values);
    let Some(lanes) = lanes else {
        return pan;
    };
    faderframe_core::SurroundParam::ALL
        .iter()
        .fold(pan, |pan, p| {
            match lanes[p.index()].as_ref().and_then(|l| l.value_at(at)) {
                Some(v) => p.set(pan, v as f32),
                None => pan,
            }
        })
}

fn set_surround(
    rb: &faderframe_realtime::ParamTable,
    slots: &[ParamSlot; SURROUND_VALUES],
    pan: &SurroundPan,
) {
    for (slot, v) in slots.iter().zip(pan.values()) {
        rb.set(*slot, v);
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
    input_latency: u32,
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
            input_latency: 0,
        }
    }
}

impl Processor<EngineContext> for StripEcho {
    fn set_input_latency(&mut self, frames: u32) {
        self.input_latency = frames;
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let n = io.frames;
        if let Some(auto) = cx.data.timeline.automation(self.track) {
            // The values at the block's end, as the strip shows them.
            let at = automation_at(cx, self.input_latency, n);
            let rb = &cx.data.readback;
            let params = &cx.data.params;
            if auto.volume.is_some()
                || auto.pan.is_some()
                || auto.pan_left.is_some()
                || auto.pan_right.is_some()
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
                let side = |lane: &Option<SampleLane>, slot| {
                    lane.as_ref()
                        .and_then(|l| l.value_at(at))
                        .map_or(params.get(slot), |v| v.clamp(-1.0, 1.0) as f32)
                };
                rb.set(self.slots.volume, fader);
                rb.set(self.slots.pan, pan);
                rb.set(self.slots.mute, if mute { 1.0 } else { 0.0 });
                rb.set(
                    self.slots.pan_left,
                    side(&auto.pan_left, self.slots.pan_left),
                );
                rb.set(
                    self.slots.pan_right,
                    side(&auto.pan_right, self.slots.pan_right),
                );
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
