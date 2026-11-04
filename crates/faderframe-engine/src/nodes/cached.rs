//! The two nodes a self-frozen track plays through (see [`crate::cache`]).

use crate::cache::HandoverShared;
use crate::context::EngineContext;
use crate::snapshot::{AudioRegion, Source};
use faderframe_audio_graph::{AudioBuffer, NodeIo, ProcessContext, Processor};
use faderframe_core::TrackId;
use std::sync::Arc;

/// Seconds a cached track fades out when the transport stops (the chain
/// would ring on; the cache plays on into what follows for this long).
pub const STOP_FADE_SECONDS: f64 = 0.01;

/// Plays a cached track's cache file in place of its chain.
///
/// It reports the latency the chain has live and reads the file the way
/// the chain's output follows its input: what it makes at a frame is what
/// the chain made of the input played `latency` frames earlier — read from
/// the play history, so loop wraps, locates and stops land as they do
/// behind a latent chain. When playback stops it plays on for
/// [`STOP_FADE_SECONDS`], fading out, where the chain would ring on.
/// Streamed pages that are not resident play as silence (and are
/// counted by the stream); a block played with every page it needed tells
/// the handover the cache is ready.
pub struct CachedPlayer {
    track: TrackId,
    latency: u32,
    file_latency: u32,
    /// Where the file would carry on (its timeline position) after the
    /// last frame played, while the stop fade lasts.
    carry: Option<i64>,
    fade_left: usize,
    fade_frames: usize,
    handover: Option<Arc<HandoverShared>>,
}

impl CachedPlayer {
    pub fn new(
        track: TrackId,
        latency: u32,
        file_latency: u32,
        sample_rate: f64,
        handover: Option<Arc<HandoverShared>>,
    ) -> Self {
        Self {
            track,
            latency,
            file_latency,
            carry: None,
            fade_left: 0,
            fade_frames: ((STOP_FADE_SECONDS * sample_rate).round() as usize).max(1),
            handover,
        }
    }
}

/// Copy the file's frames at timeline positions `[at, at + n)` (clipped
/// to the region) into `out` from `offset`; `false` when a page was
/// missing.
fn read(region: &AudioRegion, out: &mut AudioBuffer, offset: usize, at: i64, n: usize) -> bool {
    let a = at.max(region.start);
    let b = (at + n as i64).min(region.end);
    if b <= a {
        return true;
    }
    let src_ch = region.source.channels();
    let mut whole = true;
    for c in 0..out.num_channels() {
        // A file of another width: mono to every channel, else channel by
        // channel (missing ones silent).
        let s = if src_ch == 1 { 0 } else { c };
        if s >= src_ch {
            continue;
        }
        let dst = out.channel_mut(c);
        let first = offset + (a - at) as usize;
        match &region.source {
            Source::Memory(d) => {
                let src = d.channel(s);
                let from = (region.source_start + (a - region.start)) as usize;
                let len = (b - a) as usize;
                if let (Some(src), Some(dst)) =
                    (src.get(from..from + len), dst.get_mut(first..first + len))
                {
                    dst.copy_from_slice(src);
                }
            }
            Source::Stream(stream) => stream.read_segments(
                s,
                region.source_start + (a - region.start),
                (b - a) as usize,
                |off, k, seg| match seg {
                    Some(seg) => {
                        let at = first + off;
                        if let Some(d) = dst.get_mut(at..at + k) {
                            d.copy_from_slice(&seg[..k]);
                        }
                    }
                    None => whole = false,
                },
            ),
        }
    }
    whole
}

impl Processor<EngineContext> for CachedPlayer {
    fn latency(&self) -> u32 {
        self.latency
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let Some(out) = io.audio_out.first_mut() else {
            return;
        };
        out.clear();
        let region = cx.data.timeline.cached(self.track);
        let mut whole = region.is_some();
        let file_latency = i64::from(self.file_latency);
        let (latency, n) = (self.latency, io.frames);
        let mut carry = self.carry;
        let mut fade_left = self.fade_left;
        let fade_frames = self.fade_frames;
        cx.data.history.pieces(latency, 0, n, |off, k, played| {
            let (mut off, mut k) = (off, k);
            if played.count_in > 0 {
                // Counting in: nothing played yet for these frames.
                let wait = (played.count_in as usize).min(k);
                off += wait;
                k -= wait;
                if k == 0 {
                    return;
                }
            } else if !played.playing {
                // Stopped: the file plays on, fading out.
                if let (Some(r), Some(at)) = (region, carry) {
                    let m = fade_left.min(k);
                    if m > 0 {
                        whole &= read(r, out, off, at, m);
                        for c in 0..out.num_channels() {
                            let dst = &mut out.channel_mut(c)[off..off + m];
                            for (i, s) in dst.iter_mut().enumerate() {
                                let g = (fade_left - i) as f32 / (fade_frames + 1) as f32;
                                *s *= g;
                            }
                        }
                        fade_left -= m;
                        carry = Some(at + m as i64);
                    }
                }
                return;
            }
            let at = played.pos + file_latency;
            if let Some(r) = region {
                whole &= read(r, out, off, at, k);
            }
            carry = Some(at + k as i64);
            fade_left = fade_frames;
        });
        self.carry = carry;
        self.fade_left = fade_left;
        if whole && let Some(h) = &self.handover {
            h.set_ready();
        }
    }

    fn reset(&mut self) {
        self.carry = None;
        self.fade_left = 0;
    }
}

/// Crossfades a track from its chain (input 0) to its cache (input 1) or
/// back, as its [`HandoverShared`] says: exactly one input at either end
/// of the fade, `live + g · (cached − live)` between (so exactly either
/// wherever the two agree). The fade to the cache starts once the cache
/// player is ready (and the timeline carries the file); the fade back once
/// the chain has played its wait.
pub struct Handover {
    track: TrackId,
    shared: Arc<HandoverShared>,
    /// Gain change per frame.
    step: f32,
}

impl Handover {
    pub fn new(track: TrackId, shared: Arc<HandoverShared>, sample_rate: f64) -> Self {
        let frames = (crate::cache::FADE_SECONDS * sample_rate).max(1.0);
        Self {
            track,
            shared,
            step: (1.0 / frames) as f32,
        }
    }
}

impl Processor<EngineContext> for Handover {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let n = io.frames;
        let from = self.shared.gain();
        let target = self.shared.target();
        // Whether the gain moves in this block, and from which frame.
        let start = if target > from {
            let ok = self.shared.ready() && cx.data.timeline.cached(self.track).is_some();
            ok.then_some(0)
        } else if target < from {
            let wait = self.shared.wait() as usize;
            self.shared.waited(n as u32);
            (wait < n).then_some(wait)
        } else {
            None
        };
        let gain_at = |i: usize| -> f32 {
            match start {
                Some(s) if i >= s => {
                    let moved = self.step * (i - s + 1) as f32;
                    if target > from {
                        (from + moved).min(target)
                    } else {
                        (from - moved).max(target)
                    }
                }
                _ => from,
            }
        };
        let Some(out) = io.audio_out.first_mut() else {
            return;
        };
        let live = io.audio_in.first();
        let cached = io.audio_in.get(1);
        for c in 0..out.num_channels() {
            let l = live.map(|b| b.channel(c.min(b.num_channels().saturating_sub(1))));
            let k = cached.map(|b| b.channel(c.min(b.num_channels().saturating_sub(1))));
            for (i, o) in out.channel_mut(c).iter_mut().enumerate() {
                let x = l.map_or(0.0, |l| l[i]);
                let y = k.map_or(0.0, |k| k[i]);
                let g = gain_at(i);
                *o = if g <= 0.0 {
                    x
                } else if g >= 1.0 {
                    y
                } else {
                    x + g * (y - x)
                };
            }
        }
        if n > 0 {
            self.shared.set_gain(gain_at(n - 1));
        }
    }
}
