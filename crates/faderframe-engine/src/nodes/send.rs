use super::strip::{MatrixMix, surround_at};
use super::{AUTOMATION_STEP, automation_at, ramp_step};
use crate::context::EngineContext;
use crate::slots::{MixSlots, StripSlots};
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_core::pan::{self, PanLaw, PanMatrix};
use faderframe_core::{ChannelLayout, SendId, SurroundPan, TrackId, db_to_gain};
use faderframe_realtime::ParamSlot;

/// An aux/bus send: level-controlled copy of a tap point, converted to the
/// destination layout by the graph's channel rules — or, into or out of a
/// surround bed, through the panner's matrix: a mono or stereo track's
/// send taken before its panner follows the panner (and, a stereo source
/// balanced into the bed, its balance) into the bed; a bed into stereo or
/// mono folds by the project's downmix, into another bed by its speakers'
/// places (a bed folded into a stereo reverb). An automated level (and
/// panner) is evaluated every [`AUTOMATION_STEP`] frames and ramped in
/// between.
///
/// With a pan slot holding a position (not NaN), a mono or stereo tap into
/// a stereo destination is placed there: a mono tap by the project's pan
/// law, a stereo one balanced (as the strip places them), every
/// source/destination pair ramped. NaN leaves it as tapped (the graph's
/// channel rule: a mono tap at unity on both sides).
pub struct SendNode {
    track: TrackId,
    /// Its automation lanes' key (`None`: a cue mix's send, not automated).
    send: Option<SendId>,
    level: ParamSlot,
    /// Its pan (see above).
    pan: Option<ParamSlot>,
    /// Each pair's gain at the end of the last range, panned (NaN: none).
    pairs: PanMatrix,
    current: f32,
    /// Rendered ahead: shows nothing (a [`super::StripEcho`] does).
    quiet: bool,
    mix: Option<MatrixMix>,
    /// The track's strip slots, when the send follows its panner.
    follow: Option<StripSlots>,
    /// The project's downmix (a bed folded into the destination).
    shared: Option<MixSlots>,
    /// How late its input is (its automation is read that much earlier).
    input_latency: u32,
}

impl SendNode {
    pub fn new(track: TrackId, send: SendId, level: ParamSlot) -> Self {
        Self {
            track,
            send: Some(send),
            level,
            pan: None,
            pairs: [[f32::NAN; 2]; 2],
            current: f32::NAN,
            quiet: false,
            mix: None,
            follow: None,
            shared: None,
            input_latency: 0,
        }
    }

    /// A track's send to a cue mix: its level (0 while off) and pan.
    pub fn cue(track: TrackId, level: ParamSlot, pan: ParamSlot) -> Self {
        Self {
            send: None,
            pan: Some(pan),
            ..Self::new(track, SendId(0), level)
        }
    }

    /// Panned by `pan` (see the type).
    pub fn with_pan(self, pan: ParamSlot) -> Self {
        Self {
            pan: Some(pan),
            ..self
        }
    }

    /// Frames `off..off + m` of a mono or stereo `input` added into the
    /// stereo `out` through `place`, each pair reaching its gain there at
    /// the end of the range.
    fn render_panned(
        &mut self,
        input: &faderframe_audio_graph::AudioBuffer,
        out: &mut faderframe_audio_graph::AudioBuffer,
        off: usize,
        m: usize,
        place: PanMatrix,
    ) {
        let ins = input.num_channels().min(2);
        for (s, (row, pairs)) in place.iter().zip(&mut self.pairs).enumerate().take(ins) {
            let src = &input.channel(s)[off..off + m];
            for (d, (&to, pair)) in row.iter().zip(pairs.iter_mut()).enumerate() {
                let from = if pair.is_nan() { to } else { *pair };
                *pair = to;
                if from == 0.0 && to == 0.0 {
                    continue;
                }
                let step = ramp_step(from, to, m);
                let mut g = from;
                for (o, &x) in out.channel_mut(d)[off..off + m].iter_mut().zip(src) {
                    g += step;
                    *o += x * g;
                }
            }
        }
    }

    /// From a tap of layout `input` into a destination of `output`;
    /// `follow`: the strip slots of the track whose panner it follows into
    /// a bed; `shared`: the project's downmix.
    pub fn with_layouts(
        self,
        input: ChannelLayout,
        output: ChannelLayout,
        follow: Option<StripSlots>,
        shared: MixSlots,
    ) -> Self {
        let mix = MatrixMix::new(input, output);
        Self {
            follow: follow.filter(|_| mix.is_some()),
            shared: Some(shared),
            mix,
            ..self
        }
    }

    /// With decorrelation filters for the diffuse spread of the panner it
    /// follows.
    pub fn with_diffuser(self, rate: f64) -> Self {
        Self {
            mix: self.mix.map(|m| m.with_diffuser(rate)),
            ..self
        }
    }

    /// Rendered ahead of the playhead: the level it plays is not shown.
    pub fn quiet(self) -> Self {
        Self {
            quiet: true,
            ..self
        }
    }
}

impl Processor<EngineContext> for SendNode {
    fn set_input_latency(&mut self, frames: u32) {
        self.input_latency = frames;
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let static_level = cx.data.params.get(self.level);
        let auto = cx.data.timeline.automation(self.track);
        let lane = self.send.and_then(|send| auto.and_then(|a| a.send(send)));
        let pan_lanes = auto
            .map(|a| &a.surround)
            .filter(|l| self.follow.is_some() && l.iter().any(Option::is_some));
        let balance_lane = auto
            .and_then(|a| a.pan.as_ref())
            .filter(|_| self.follow.is_some());
        let downmix = self.shared.map(|s| s.downmix(&cx.data.params));
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        let n = io.frames;
        // Panned (a mono or stereo tap into stereo, a position set).
        let position = self
            .pan
            .map(|s| cx.data.params.get(s))
            .filter(|p| !p.is_nan());
        if let Some(position) = position.filter(|_| {
            self.mix.is_none() && out.num_channels() == 2 && (1..=2).contains(&input.num_channels())
        }) {
            out.clear();
            let law = self.shared.map_or(PanLaw::default(), |s| {
                PanLaw::from_index(cx.data.params.get(s.pan_law))
            });
            let position = pan::position(position);
            let place = if input.num_channels() == 1 {
                pan::mono_pan(law, position)
            } else {
                pan::balance(position)
            };
            let step = if lane.is_some() {
                AUTOMATION_STEP
            } else {
                n.max(1)
            };
            let mut off = 0;
            while off < n {
                let m = step.min(n - off);
                let target = lane
                    .and_then(|l| l.value_at(automation_at(cx, self.input_latency, off + m)))
                    .map_or(static_level, |db| db_to_gain(db as f32));
                // Silent and staying so: nothing to add.
                if target == 0.0 && self.current == 0.0 {
                    self.pairs = [[0.0; 2]; 2];
                } else {
                    self.render_panned(input, out, off, m, pan::scaled(place, target));
                }
                self.current = target;
                off += m;
            }
            if lane.is_some() && !self.quiet {
                cx.data.readback.set(self.level, self.current);
            }
            return;
        }
        // Unpanned from here on: the pairs start again when it is panned.
        self.pairs = [[f32::NAN; 2]; 2];
        if self.mix.is_some() {
            out.clear();
        } else {
            out.copy_from(input);
        }
        let step = if lane.is_some() || pan_lanes.is_some() || balance_lane.is_some() {
            AUTOMATION_STEP
        } else {
            n.max(1)
        };
        let mut off = 0;
        while off < n {
            let m = step.min(n - off);
            let target = lane
                .and_then(|l| l.value_at(automation_at(cx, self.input_latency, off + m)))
                .map_or(static_level, |db| db_to_gain(db as f32));
            if let Some(mix) = self.mix.as_mut() {
                // Through the matrix (its pairs ramp the level too).
                let at = automation_at(cx, self.input_latency, off + m);
                let (pan, balance) = match &self.follow {
                    Some(slots) => (
                        surround_at(&slots.surround, &cx.data.params, pan_lanes, at),
                        balance_lane
                            .and_then(|l| l.value_at(at))
                            .map_or(cx.data.params.get(slots.pan), |v| v as f32),
                    ),
                    None => (SurroundPan::default(), 0.0),
                };
                mix.pan(&pan, balance, downmix.unwrap_or_default());
                mix.mix(input, out, off, m, target);
                self.current = target;
                off += m;
                continue;
            }
            let from = if self.current.is_nan() {
                target
            } else {
                self.current
            };
            let inc = ramp_step(from, target, m);
            for c in 0..out.num_channels() {
                let mut g = from;
                for s in &mut out.channel_mut(c)[off..off + m] {
                    g += inc;
                    *s *= g;
                }
            }
            self.current = target;
            off += m;
        }
        if lane.is_some() && !self.quiet {
            cx.data.readback.set(self.level, self.current);
        }
    }
}
