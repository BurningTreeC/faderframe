//! What the transport played, frame by frame of a graph's processing.
//!
//! A graph's frames are counted from its context's start ([`PlayHistory`]
//! keeps the counter); every chunk processed is recorded with the timeline
//! position, whether it was playing and the transport's jumps counter, as
//! segments of frames that continue each other. Three things read it:
//!
//! * nodes behind latency: the audio arriving at a node `L` frames late was
//!   played `L` frames ago — before a loop wrap, a locate or a stop, maybe —
//!   so its automation is read at [`PlayHistory::at`] that frame
//!   (`nodes::automation_at`);
//! * the metronome, which plays as late as the graph's output;
//! * the UI's playhead: the realtime side publishes the segments around
//!   what is being heard ([`HeardClock`]) and the control side maps a time
//!   to the timeline position heard then.
//!
//! Fixed size, no allocation: the oldest segments are forgotten (a frame
//! older than them reads as the oldest one's start, stopped).

use std::sync::atomic::{AtomicU64, Ordering, fence};

/// Segments kept: loop wraps, locates, starts and stops each begin one.
pub const SEGMENTS: usize = 64;

/// Frames of one run of the transport: the position advancing with the
/// frames while playing, standing still while stopped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    /// Its first frame (the graph's frame counter).
    pub frame: i64,
    /// The frame after its last.
    pub end: i64,
    /// Timeline position of `frame`.
    pub pos: i64,
    pub playing: bool,
    /// The transport's jumps counter (locates, stops; not loop wraps).
    pub jumps: u32,
}

impl Segment {
    /// Timeline position of `frame` (extrapolated outside the segment).
    #[inline]
    pub fn pos_at(&self, frame: i64) -> i64 {
        if self.playing {
            self.pos + (frame - self.frame)
        } else {
            self.pos
        }
    }

    /// Whether `next` carries on from it (one run).
    fn continued_by(&self, frame: i64, pos: i64, playing: bool, jumps: u32) -> bool {
        self.end == frame
            && self.playing == playing
            && self.jumps == jumps
            && self.pos_at(frame) == pos
    }
}

/// Where one frame was on the timeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Played {
    pub pos: i64,
    pub playing: bool,
    pub jumps: u32,
    /// The first frame of its run (a new run: something jumped or wrapped).
    pub start: i64,
}

/// Nanoseconds from the first call (made when an engine is created) to
/// `at`: the time base of the published heard positions.
pub fn ns_since_epoch(at: std::time::Instant) -> u64 {
    at.saturating_duration_since(epoch()).as_nanos() as u64
}

/// Nanoseconds since the time base's start (the first call starts it).
pub fn now_ns() -> u64 {
    ns_since_epoch(std::time::Instant::now())
}

fn epoch() -> std::time::Instant {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(std::time::Instant::now)
}

/// The recent past of one graph's processing (see the module docs).
#[derive(Clone, Debug)]
pub struct PlayHistory {
    segs: [Segment; SEGMENTS],
    /// Index of the newest segment.
    newest: usize,
    len: usize,
    /// First frame of the chunk being processed.
    chunk: i64,
}

impl Default for PlayHistory {
    fn default() -> Self {
        Self {
            segs: [Segment::default(); SEGMENTS],
            newest: 0,
            len: 0,
            chunk: 0,
        }
    }
}

impl PlayHistory {
    /// Record the chunk about to be processed: `n` frames from timeline
    /// position `pos`. Realtime-safe.
    pub fn record(&mut self, pos: i64, playing: bool, jumps: u32, n: usize) {
        let frame = self.end();
        self.chunk = frame;
        let end = frame + n as i64;
        if self.len > 0 && self.segs[self.newest].continued_by(frame, pos, playing, jumps) {
            self.segs[self.newest].end = end;
            return;
        }
        if self.len > 0 {
            self.newest = (self.newest + 1) % SEGMENTS;
        }
        self.len = (self.len + 1).min(SEGMENTS);
        self.segs[self.newest] = Segment {
            frame,
            end,
            pos,
            playing,
            jumps,
        };
    }

    /// First frame of the chunk being processed.
    #[inline]
    pub fn chunk_start(&self) -> i64 {
        self.chunk
    }

    /// The frame after the last one recorded.
    pub fn end(&self) -> i64 {
        if self.len == 0 {
            0
        } else {
            self.segs[self.newest].end
        }
    }

    /// The segments, newest first.
    pub fn newest_first(&self) -> impl Iterator<Item = &Segment> {
        (0..self.len).map(move |k| &self.segs[(self.newest + SEGMENTS - k) % SEGMENTS])
    }

    /// The newest segment.
    pub fn newest(&self) -> Option<&Segment> {
        (self.len > 0).then(|| &self.segs[self.newest])
    }

    /// The segment holding `frame`: past the newest, the newest
    /// (extrapolated); before the oldest, the oldest's start, stopped.
    fn find(&self, frame: i64) -> Option<Segment> {
        let mut oldest = None;
        for s in self.newest_first() {
            if frame >= s.frame {
                return Some(*s);
            }
            oldest = Some(*s);
        }
        oldest.map(|s| Segment {
            playing: false,
            ..s
        })
    }

    /// Where `frame` was on the timeline (nothing recorded: position 0,
    /// stopped).
    pub fn at(&self, frame: i64) -> Played {
        match self.find(frame) {
            Some(s) => Played {
                pos: s.pos_at(frame),
                playing: s.playing,
                jumps: s.jumps,
                start: s.frame,
            },
            None => Played::default(),
        }
    }

    /// Where frame `offset` of the current chunk was, `latency` frames ago.
    #[inline]
    pub fn delayed(&self, latency: u32, offset: usize) -> Played {
        self.at(self.chunk + offset as i64 - i64::from(latency))
    }

    /// The runs `n` frames from `offset` of the current chunk were played
    /// in, `latency` frames ago: `f(offset, frames, segment-relative
    /// played)`, in order. At most `SEGMENTS + 1` calls.
    pub fn pieces(
        &self,
        latency: u32,
        offset: usize,
        n: usize,
        mut f: impl FnMut(usize, usize, Played),
    ) {
        let from = self.chunk + offset as i64 - i64::from(latency);
        let to = from + n as i64;
        let mut at = from;
        while at < to {
            let s = self.find(at).unwrap_or_default();
            // Where this run stops: the next segment's start (if any lies
            // between), else the window's end.
            let next = self
                .newest_first()
                .map(|x| x.frame)
                .filter(|&f| f > at && f < to)
                .min()
                .unwrap_or(to);
            let played = Played {
                pos: s.pos_at(at),
                playing: s.playing,
                jumps: s.jumps,
                start: s.frame,
            };
            f((at - from) as usize + offset, (next - at) as usize, played);
            at = next;
        }
    }
}

/// Segments published to the control side.
pub const PUBLISHED: usize = 8;
const WORDS: usize = 6 + 4 * PUBLISHED;

/// The play history around what is being heard, published after every
/// callback for the control side's playhead (a seqlock over atomics: the
/// audio thread never waits, a reader retries if a callback wrote while it
/// read).
#[derive(Debug)]
pub struct HeardClock {
    seq: AtomicU64,
    /// `[callback ns, frame at the callback, frames from processing to
    /// hearing, segment count, newest jumps, position the newest jump went
    /// to]` and the segments (`frame, end, pos, playing << 63 | jumps`).
    words: [AtomicU64; WORDS],
}

impl Default for HeardClock {
    fn default() -> Self {
        Self {
            seq: AtomicU64::new(0),
            words: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

/// One published view of the history (see [`HeardClock::read`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct HeardSnapshot {
    /// When the last callback started (ns since the engine's epoch).
    pub callback_ns: u64,
    /// The graph's frame counter at that callback's start.
    pub frame: i64,
    /// Frames from processing to hearing (the graph's output latency and
    /// the device's, in engine frames).
    pub latency: i64,
    /// The transport's newest jumps counter and where that jump went.
    pub jumps: u32,
    pub jumped_to: i64,
    pub segments: [Segment; PUBLISHED],
    pub count: usize,
}

impl HeardSnapshot {
    /// What is heard `frames_since` engine frames after the callback: the
    /// timeline position and whether it plays — `None` before anything was
    /// published. Never past the frames processed.
    pub fn heard(&self, frames_since: f64) -> Option<Played> {
        if self.count == 0 {
            return None;
        }
        let segs = &self.segments[..self.count];
        let last_end = segs[self.count - 1].end;
        let h = (self.frame as f64 + frames_since - self.latency as f64).floor() as i64;
        let h = h.min(last_end - 1);
        let s = segs
            .iter()
            .rev()
            .find(|s| h >= s.frame)
            .copied()
            .unwrap_or(Segment {
                playing: false,
                ..segs[0]
            });
        Some(Played {
            pos: s.pos_at(h),
            playing: s.playing,
            jumps: s.jumps,
            start: s.frame,
        })
    }
}

impl HeardClock {
    /// Publish (audio thread, after a callback): the segments that may be
    /// heard until the next callback — those reaching past `callback frame
    /// − latency − margin`, oldest first, at most [`PUBLISHED`] — and the
    /// transport's jumps counter and position now (a stop or locate at the
    /// end of the callback is in no segment yet).
    pub fn publish(
        &self,
        history: &PlayHistory,
        callback_ns: u64,
        frame: i64,
        latency: i64,
        transport: (u32, i64),
    ) {
        let from = frame - latency - (history.end() - frame).max(1) * 2;
        // The oldest segment still reaching `from`, then the newer ones.
        let mut first = 0;
        for (k, s) in history.newest_first().enumerate() {
            first = k;
            if s.frame <= from {
                break;
            }
        }
        let count = (first + 1).min(PUBLISHED);
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        let (jumps, jumped_to) = match jump_of(history) {
            (j, to) if j == transport.0 => (j, to),
            _ => transport,
        };
        let w = &self.words;
        w[0].store(callback_ns, Ordering::Relaxed);
        w[1].store(frame as u64, Ordering::Relaxed);
        w[2].store(latency as u64, Ordering::Relaxed);
        w[3].store(count as u64, Ordering::Relaxed);
        w[4].store(u64::from(jumps), Ordering::Relaxed);
        w[5].store(jumped_to as u64, Ordering::Relaxed);
        // Oldest of the chosen first.
        for (i, seg) in history.newest_first().take(count).enumerate() {
            let k = 6 + 4 * (count - 1 - i);
            w[k].store(seg.frame as u64, Ordering::Relaxed);
            w[k + 1].store(seg.end as u64, Ordering::Relaxed);
            w[k + 2].store(seg.pos as u64, Ordering::Relaxed);
            w[k + 3].store(
                (u64::from(seg.playing) << 63) | u64::from(seg.jumps),
                Ordering::Relaxed,
            );
        }
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// The last published view (control side; `None` while a callback is
    /// writing it after a few tries, or before the first).
    pub fn read(&self) -> Option<HeardSnapshot> {
        for _ in 0..8 {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let w: [u64; WORDS] = std::array::from_fn(|i| self.words[i].load(Ordering::Relaxed));
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) != s1 {
                continue;
            }
            if s1 == 0 {
                return None;
            }
            let count = (w[3] as usize).min(PUBLISHED);
            let mut snap = HeardSnapshot {
                callback_ns: w[0],
                frame: w[1] as i64,
                latency: w[2] as i64,
                count,
                jumps: w[4] as u32,
                jumped_to: w[5] as i64,
                segments: [Segment::default(); PUBLISHED],
            };
            for i in 0..count {
                let k = 6 + 4 * i;
                snap.segments[i] = Segment {
                    frame: w[k] as i64,
                    end: w[k + 1] as i64,
                    pos: w[k + 2] as i64,
                    playing: w[k + 3] >> 63 == 1,
                    jumps: w[k + 3] as u32,
                };
            }
            return Some(snap);
        }
        None
    }
}

/// The newest jumps counter and the position its jump went to (the start
/// of the oldest segment carrying it among those kept).
fn jump_of(history: &PlayHistory) -> (u32, i64) {
    let Some(newest) = history.newest() else {
        return (0, 0);
    };
    let mut to = newest.pos;
    for s in history.newest_first() {
        if s.jumps != newest.jumps {
            break;
        }
        to = s.pos;
    }
    (newest.jumps, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(chunks: &[(i64, bool, u32, usize)]) -> PlayHistory {
        let mut h = PlayHistory::default();
        for &(pos, playing, jumps, n) in chunks {
            h.record(pos, playing, jumps, n);
        }
        h
    }

    #[test]
    fn continuing_chunks_are_one_segment() {
        let h = h(&[(0, true, 0, 64), (64, true, 0, 64), (128, true, 0, 10)]);
        assert_eq!(h.newest_first().count(), 1);
        assert_eq!(h.end(), 138);
        assert_eq!(h.chunk_start(), 128);
        assert_eq!(h.at(100).pos, 100);
    }

    #[test]
    fn a_loop_wrap_is_read_back_across() {
        // Looping 1000..1500: the chunk split at the wrap.
        let h = h(&[
            (1300, true, 0, 200),
            (1000, true, 0, 64),
            (1064, true, 0, 64),
        ]);
        assert_eq!(h.newest_first().count(), 2);
        // The current chunk starts at frame 264 (position 1064); 100 frames
        // back is before the wrap.
        assert_eq!(h.delayed(100, 0).pos, 1464);
        assert_eq!(h.delayed(0, 5).pos, 1069);
        let mut got = Vec::new();
        h.pieces(100, 0, 64, |o, n, p| got.push((o, n, p.pos)));
        assert_eq!(got, vec![(0, 36, 1464), (36, 28, 1000)]);
    }

    #[test]
    fn stopped_frames_stand_still_and_the_oldest_reads_back_stopped() {
        let h = h(&[(500, false, 0, 64), (500, true, 0, 64), (564, true, 0, 64)]);
        assert_eq!(h.at(10).pos, 500);
        assert!(!h.at(10).playing);
        assert_eq!(h.at(70).pos, 506);
        assert!(h.at(70).playing);
        assert_eq!(h.at(-1000).pos, 500, "before anything: the start, stopped");
        assert!(!h.at(-1000).playing);
    }

    #[test]
    fn the_ring_forgets_the_oldest() {
        let mut h = PlayHistory::default();
        for k in 0..(SEGMENTS as i64 + 10) {
            // A locate every chunk.
            h.record(k * 1000, true, k as u32, 10);
        }
        assert_eq!(h.newest_first().count(), SEGMENTS);
        assert_eq!(h.at(h.end() - 5).pos, (SEGMENTS as i64 + 9) * 1000 + 5);
    }

    #[test]
    fn the_published_view_reads_what_is_heard() {
        let clock = HeardClock::default();
        assert!(clock.read().is_none());
        // Stopped at 0, then playing from 0; the jump of a locate to 9000.
        let mut h = PlayHistory::default();
        h.record(0, false, 0, 256);
        h.record(0, true, 0, 256);
        h.record(256, true, 0, 256);
        let frame = h.chunk_start();
        h.record(9000, true, 1, 256);
        clock.publish(&h, 1_000, frame, 300, (1, 9256));
        let s = clock.read().unwrap();
        assert_eq!((s.jumps, s.jumped_to), (1, 9000));
        // At the callback: 512 − 300 = frame 212 → stopped at 0.
        assert_eq!(s.heard(0.0).unwrap().pos, 0);
        assert!(!s.heard(0.0).unwrap().playing);
        // 100 frames on: frame 312 → position 56.
        assert_eq!(s.heard(100.0).unwrap().pos, 56);
        // 400 on: frame 612, still before the locate (at frame 768).
        assert_eq!(s.heard(400.0).unwrap().pos, 356);
        // 656 on: frame 868 → 100 into the locate to 9000.
        let p = s.heard(656.0).unwrap();
        assert_eq!((p.pos, p.jumps), (9100, 1));
        // Never past what was processed.
        assert_eq!(s.heard(1e9).unwrap().pos, 9255);
    }
}

#[cfg(test)]
mod cost {
    use super::*;

    /// What a callback adds: recording its chunk, publishing the heard
    /// clock, and a node behind latency reading back 64 positions (run
    /// with `--release --ignored --nocapture`).
    #[test]
    #[ignore = "a measurement"]
    fn cost_per_callback() {
        let mut h = PlayHistory::default();
        let clock = HeardClock::default();
        let n = 1_000_000;
        let t0 = std::time::Instant::now();
        let mut pos = 0i64;
        let mut sum = 0i64;
        for k in 0..n {
            let frame = h.end();
            // A loop wrap every 100 callbacks.
            if k % 100 == 0 {
                pos = 0;
            }
            h.record(pos, true, 0, 64);
            for o in 0..64 {
                sum = sum.wrapping_add(h.delayed(6400, o).pos);
            }
            clock.publish(&h, k as u64, frame, 6400, (0, 0));
            pos += 64;
        }
        let per = t0.elapsed().as_nanos() as f64 / f64::from(n);
        println!("{per:.1} ns per callback (record + 64 read-backs + publish), {sum}");
    }
}
