use super::{AUTOMATION_STEP, automation_at, ramp_step};
use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_core::{SendId, TrackId, db_to_gain};
use faderframe_realtime::ParamSlot;

/// An aux/bus send: level-controlled copy of a tap point, converted to the
/// destination layout by the graph's channel rules. An automated level is
/// evaluated every [`AUTOMATION_STEP`] frames and ramped in between.
pub struct SendNode {
    track: TrackId,
    send: SendId,
    level: ParamSlot,
    current: f32,
    /// Rendered ahead: shows nothing (a [`super::StripEcho`] does).
    quiet: bool,
}

impl SendNode {
    pub fn new(track: TrackId, send: SendId, level: ParamSlot) -> Self {
        Self {
            track,
            send,
            level,
            current: f32::NAN,
            quiet: false,
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
        let lane = cx
            .data
            .timeline
            .automation(self.track)
            .and_then(|a| a.send(self.send));
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        out.copy_from(input);
        let n = io.frames;
        let step = if lane.is_some() {
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
