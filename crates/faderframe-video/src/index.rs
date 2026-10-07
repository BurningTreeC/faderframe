//! Every frame's time and the keyframes, read from the demuxer alone (no
//! decoding: fast), in the file's own timeline (stream time: edit lists
//! and start offsets applied). Variable frame rates and B-frames come out
//! exact because nothing is computed from a nominal rate.

use crate::streams::{Kind, Pipeline};
use crate::{Result, VideoError};
use faderframe_core::timecode::{FrameRate, Timecode};
use gst::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameIndex {
    /// Each frame's start (ns, stream time), in presentation order.
    pub times: Vec<i64>,
    /// The frames that are keyframes (indices into `times`, ascending).
    pub keys: Vec<u32>,
    /// Where the last frame ends (ns).
    pub end: i64,
    /// The file's start timecode and its rate, when it carries one (a
    /// QuickTime timecode track).
    pub timecode: Option<(Timecode, FrameRate)>,
}

impl FrameIndex {
    pub fn len(&self) -> usize {
        self.times.len()
    }

    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// The frame showing at `t` ns (`None` before the first and from the
    /// end on).
    pub fn frame_at(&self, t: i64) -> Option<usize> {
        if t >= self.end {
            return None;
        }
        // A hair forward: a frame's own start lands in it.
        let n = self.times.partition_point(|&s| s <= t + 1);
        n.checked_sub(1)
    }

    /// Where frame `n` starts and how long it shows (ns).
    pub fn span(&self, n: usize) -> Option<(i64, i64)> {
        let start = *self.times.get(n)?;
        let end = self.times.get(n + 1).copied().unwrap_or(self.end);
        Some((start, (end - start).max(1)))
    }

    /// The keyframe at or before frame `n`.
    pub fn key_before(&self, n: usize) -> usize {
        let i = self.keys.partition_point(|&k| k as usize <= n);
        i.checked_sub(1).map_or(0, |i| self.keys[i] as usize)
    }

    /// Frames between keyframes at most (1: every frame is one).
    pub fn longest_gop(&self) -> usize {
        let mut longest = 1;
        for w in self.keys.windows(2) {
            longest = longest.max((w[1] - w[0]) as usize);
        }
        if let Some(&last) = self.keys.last() {
            longest = longest.max(self.times.len() - last as usize);
        }
        longest
    }

    /// The typical frame length (median, ns).
    pub fn frame_length(&self) -> i64 {
        let mut d: Vec<i64> = self.times.windows(2).map(|w| w[1] - w[0]).collect();
        if d.is_empty() {
            return 40_000_000;
        }
        d.sort_unstable();
        d[d.len() / 2].max(1)
    }

    /// Whether frames are evenly spaced (a constant frame rate, within
    /// 1 %).
    pub fn constant_rate(&self) -> bool {
        let typical = self.frame_length() as f64;
        self.times
            .windows(2)
            .all(|w| ((w[1] - w[0]) as f64 - typical).abs() <= typical * 0.01)
    }
}

fn timecode_of(meta: &gst_video::VideoTimeCodeMeta) -> Option<(Timecode, FrameRate)> {
    let tc = meta.tc();
    let fps = tc.fps();
    let drop = tc
        .flags()
        .contains(gst_video::VideoTimeCodeFlags::DROP_FRAME);
    let rate = FrameRate::from_fraction(fps.numer().max(0) as u32, fps.denom().max(1) as u32)?
        .with_drop(drop);
    Some((
        Timecode {
            hours: tc.hours() as u8,
            minutes: tc.minutes() as u8,
            seconds: tc.seconds() as u8,
            frames: tc.frames() as u8,
        },
        rate,
    ))
}

/// Index the picture of `path` (`progress`: the share read so far).
pub fn index(path: &Path, cancel: &AtomicBool, progress: impl FnMut(f64)) -> Result<FrameIndex> {
    let p = Pipeline::new(path)?;
    struct Seen {
        frames: Vec<(i64, bool)>,
        end: i64,
        timecode: Option<(Timecode, FrameRate)>,
        picture: bool,
    }
    let seen = Arc::new(Mutex::new(Seen {
        frames: Vec::new(),
        end: 0,
        timecode: None,
        picture: false,
    }));
    let sink = gst_app::AppSink::builder()
        .sync(false)
        .max_buffers(64)
        .drop(false)
        .build();
    let s2 = Arc::clone(&seen);
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let (Some(buffer), Some(segment)) = (sample.buffer(), sample.segment()) else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                let Some(segment) = segment.downcast_ref::<gst::ClockTime>() else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                let Some(pts) = buffer.pts() else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                // Outside the segment (cut by an edit list): never shown.
                let Some(t) = segment.to_stream_time(pts) else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                let key = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
                let Ok(mut s) = s2.lock() else {
                    return Err(gst::FlowError::Error);
                };
                let t = crate::ns(t);
                let end = t + buffer.duration().map_or(0, crate::ns);
                s.end = s.end.max(end);
                if s.timecode.is_none()
                    && let Some(meta) = buffer.meta::<gst_video::VideoTimeCodeMeta>()
                {
                    s.timecode = timecode_of(&meta);
                }
                s.frames.push((t, key));
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    let sink_el: gst::Element = sink.upcast();
    p.pipeline.add(&sink_el)?;
    let pad = sink_el
        .static_pad("sink")
        .ok_or_else(|| VideoError::Gst("appsink without a sink".into()))?;
    let s3 = Arc::clone(&seen);
    p.parse(move |_, kind, n, _| {
        if kind == Kind::Picture && n == 0 {
            if let Ok(mut s) = s3.lock() {
                s.picture = true;
            }
            Some(pad.clone())
        } else {
            None
        }
    })?;
    p.run(cancel, progress)?;
    drop(p);
    let mut s = seen
        .lock()
        .map_err(|_| VideoError::Gst("index lock poisoned".into()))?;
    if !s.picture || s.frames.is_empty() {
        return Err(VideoError::NoPicture(path.to_path_buf()));
    }
    // Presentation order (B-frames arrive in decoding order).
    s.frames.sort_by_key(|f| f.0);
    s.frames.dedup_by_key(|f| f.0);
    let times: Vec<i64> = s.frames.iter().map(|f| f.0).collect();
    let keys = s
        .frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.1)
        .map(|(i, _)| i as u32)
        .collect();
    let mut index = FrameIndex {
        times,
        keys,
        end: s.end,
        timecode: s.timecode,
    };
    // A last frame without a duration lasts as long as the others.
    if let Some(&last) = index.times.last()
        && index.end <= last
    {
        index.end = last + index.frame_length();
    }
    Ok(index)
}
