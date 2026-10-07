use super::strip::{MatrixMix, surround_at};
use super::{AUTOMATION_STEP, automation_at, ramp_step};
use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_core::{ChannelLayout, SendId, TrackId, db_to_gain};
use faderframe_realtime::ParamSlot;

/// An aux/bus send: level-controlled copy of a tap point, converted to the
/// destination layout by the graph's channel rules — or, into or out of a
/// surround bed, through the panner's matrix: a mono or stereo track's
/// send taken before its panner follows the panner into the bed, others
/// are placed by their speakers (a bed folded into a stereo reverb). An
/// automated level (and panner) is evaluated every [`AUTOMATION_STEP`]
/// frames and ramped in between.
pub struct SendNode {
    track: TrackId,
    send: SendId,
    level: ParamSlot,
    current: f32,
    /// Rendered ahead: shows nothing (a [`super::StripEcho`] does).
    quiet: bool,
    mix: Option<MatrixMix>,
    /// The track's surround panner slots, when the send follows it.
    follow: Option<[ParamSlot; 6]>,
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
        }
    }

    /// From a tap of layout `input` into a destination of `output`;
    /// `follow`: the panner slots of the track it follows into a bed.
    pub fn with_layouts(
        self,
        input: ChannelLayout,
        output: ChannelLayout,
        follow: Option<[ParamSlot; 6]>,
    ) -> Self {
        let mix = MatrixMix::new(input, output);
        Self {
            follow: follow.filter(|_| mix.is_some()),
            mix,
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
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let static_level = cx.data.params.get(self.level);
        let auto = cx.data.timeline.automation(self.track);
        let lane = auto.and_then(|a| a.send(self.send));
        let pan_lanes = auto
            .map(|a| &a.surround)
            .filter(|l| self.follow.is_some() && l.iter().any(Option::is_some));
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        if self.mix.is_some() {
            out.clear();
        } else {
            out.copy_from(input);
        }
        let n = io.frames;
        let step = if lane.is_some() || pan_lanes.is_some() {
            AUTOMATION_STEP
        } else {
            n.max(1)
        };
        let mut off = 0;
        while off < n {
            let m = step.min(n - off);
            let target = lane
                .and_then(|l| l.value_at(automation_at(cx, off + m)))
                .map_or(static_level, |db| db_to_gain(db as f32));
            if let Some(mix) = self.mix.as_mut() {
                // Through the matrix (its pairs ramp the level too).
                if let Some(slots) = &self.follow {
                    let at = automation_at(cx, off + m);
                    mix.pan(&surround_at(slots, &cx.data.params, pan_lanes, at));
                }
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
