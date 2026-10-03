use crate::context::EngineContext;
use crate::snapshot::{AudioRegion, Source};
use faderframe_audio_graph::{
    AudioBuffer, NodeIo, ProcessContext, Processor, for_each_channel_route,
};
use faderframe_core::TrackId;

/// Plays the audio regions of one track's lane from the timeline snapshot.
///
/// In-memory sources are read directly. Streamed sources are read from
/// their resident pages; a page the disk loader has not provided yet plays
/// as silence and is counted as a miss, the realtime thread never waits for
/// the disk. Silent while the transport is stopped.
pub struct AudioClipPlayer {
    track: TrackId,
}

impl AudioClipPlayer {
    pub fn new(track: TrackId) -> Self {
        Self { track }
    }
}

/// Mix `region` into `out` for timeline samples `[a, b)` of the block
/// starting at `pos`.
#[inline]
fn play_region(region: &AudioRegion, out: &mut AudioBuffer, pos: i64, a: i64, b: i64) {
    let out_ch = out.num_channels();
    let src_ch = region.source.channels();
    let len_src = region.source.frames();
    match &region.source {
        Source::Memory(data) => {
            for_each_channel_route(src_ch, out_ch, |s, d, w| {
                let src = data.channel(s);
                let dst = out.channel_mut(d);
                for t in a..b {
                    let rel = t - region.start;
                    let frame = if region.reversed {
                        region.source_start + (region.end - region.start) - 1 - rel
                    } else {
                        region.source_start + rel
                    };
                    if frame < 0 || frame >= len_src {
                        continue;
                    }
                    dst[(t - pos) as usize] += src[frame as usize] * region.gain_at(t) * w;
                }
            });
        }
        Source::Stream(stream) if region.step == 1.0 && !region.reversed => {
            for_each_channel_route(src_ch, out_ch, |s, d, w| {
                let dst = out.channel_mut(d);
                stream.read_segments(
                    s,
                    region.source_start + (a - region.start),
                    (b - a) as usize,
                    |off, n, seg| {
                        if let Some(seg) = seg {
                            for (k, v) in seg[..n].iter().enumerate() {
                                let t = a + (off + k) as i64;
                                dst[(t - pos) as usize] += v * region.gain_at(t) * w;
                            }
                        }
                    },
                );
            });
        }
        Source::Stream(stream) => {
            // File rate differs from the engine rate (or reversed): linear
            // interpolation between resident samples.
            let span = ((region.end - region.start) as f64 * region.step).ceil() as i64;
            for_each_channel_route(src_ch, out_ch, |s, d, w| {
                let dst = out.channel_mut(d);
                for t in a..b {
                    let x = (t - region.start) as f64 * region.step;
                    let x = if region.reversed {
                        (span - 1) as f64 - x
                    } else {
                        x
                    };
                    let i0 = x.floor() as i64;
                    let frac = (x - i0 as f64) as f32;
                    let f0 = region.source_start + i0;
                    let (Some(s0), s1) = (stream.sample(s, f0), stream.sample(s, f0 + 1)) else {
                        continue;
                    };
                    let v = s0 + (s1.unwrap_or(s0) - s0) * frac;
                    dst[(t - pos) as usize] += v * region.gain_at(t) * w;
                }
            });
        }
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
        for region in lane.audio[..upto].iter().filter(|r| r.end > pos) {
            let a = pos.max(region.start);
            let b = end.min(region.end);
            play_region(region, out, pos, a, b);
        }
    }
}
