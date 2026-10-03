use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor, for_each_channel_route};
use faderframe_core::TrackId;

/// Plays the audio regions of one track's lane from the timeline snapshot.
///
/// Reads only immutable shared data (`Arc<AudioData>`), so it never touches
/// the disk or allocates. Silent while the transport is stopped.
pub struct AudioClipPlayer {
    track: TrackId,
}

impl AudioClipPlayer {
    pub fn new(track: TrackId) -> Self {
        Self { track }
    }
}

impl Processor<EngineContext> for AudioClipPlayer {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let Some(out) = io.audio_out.first_mut() else {
            return;
        };
        out.clear();
        let t = &cx.data.transport;
        if !t.playing {
            return;
        }
        let Some(lane) = cx.data.timeline.lane(self.track) else {
            return;
        };
        let pos = t.sample_position;
        let end = pos + io.frames as i64;
        // Regions are sorted by start; everything starting at/after `end` is
        // irrelevant for this block.
        let upto = lane.audio.partition_point(|r| r.start < end);
        let out_ch = out.num_channels();
        for region in lane.audio[..upto].iter().filter(|r| r.end > pos) {
            let a = pos.max(region.start);
            let b = end.min(region.end);
            let frames = region.data.frames() as i64;
            let src_ch = region.data.num_channels();
            for_each_channel_route(src_ch, out_ch, |s, d, w| {
                let src = region.data.channel(s);
                let dst = out.channel_mut(d);
                for t in a..b {
                    let rel = t - region.start;
                    let frame = if region.reversed {
                        region.source_offset + (region.end - region.start) - 1 - rel
                    } else {
                        region.source_offset + rel
                    };
                    if frame < 0 || frame >= frames {
                        continue;
                    }
                    dst[(t - pos) as usize] += src[frame as usize] * region.gain_at(t) * w;
                }
            });
        }
    }
}
