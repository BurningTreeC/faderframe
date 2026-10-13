use super::strip::{MatrixMix, surround_at};
use super::{AUTOMATION_STEP, automation_at, ramp_step};
use crate::context::EngineContext;
use crate::slots::{MixSlots, StripSlots};
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
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
pub struct SendNode {
    track: TrackId,
    send: SendId,
    level: ParamSlot,
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
            send,
            level,
            current: f32::NAN,
            quiet: false,
            mix: None,
            follow: None,
            shared: None,
            input_latency: 0,
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
        let lane = auto.and_then(|a| a.send(self.send));
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
        if self.mix.is_some() {
            out.clear();
        } else {
            out.copy_from(input);
        }
        let n = io.frames;
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
