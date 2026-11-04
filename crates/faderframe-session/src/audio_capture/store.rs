//! The rolling store of kept input: per device channel, the newest
//! samples in chunks of [`CHUNK`] frames (the oldest chunk goes when a
//! channel holds its share), a peak per [`MARK`] frames with a histogram
//! of them (the noise floor the phrase finder measures sound against),
//! and the transport's spans over all of it.
//!
//! Frames are counted on the engine's frame counter (continuous whatever
//! channels are kept; a new engine's counter is moved on to continue
//! where the last one ended). Full chunks are shared (`Arc`): a capture
//! takes them without copying and writes its files from them while the
//! store goes on.

use std::collections::VecDeque;
use std::sync::Arc;

/// Frames per chunk of a channel (64 KiB of samples).
pub const CHUNK: usize = 16_384;
/// Bytes of one chunk.
pub const CHUNK_BYTES: usize = CHUNK * std::mem::size_of::<f32>();
/// Frames per level mark (the phrase finder's resolution).
pub const MARK: u64 = 256;
/// Marks per group: a group's loudest mark lets scans pass over quiet
/// minutes quickly.
pub const GROUP: u64 = 64;
/// Level marks counted in 1 dB steps from −120 dBFS to 0 dBFS.
const BINS: usize = 121;

/// Frames the transport treated alike: one pass of one run, recorded or
/// not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub frames: u64,
    /// Timeline position (engine samples) of `start` while playing.
    pub position: Option<i64>,
    /// Transport runs: a start or a stop begins one (playing and stopped
    /// runs alternate).
    pub run: u32,
    /// Contiguous stretches of a playing run (a loop wrap or a locate
    /// begins one).
    pub pass: u32,
    /// The recorder took these frames.
    pub recording: bool,
}

impl Span {
    pub fn end(&self) -> u64 {
        self.start + self.frames
    }

    /// The timeline position of `frame` (extrapolated past the span).
    pub fn position_at(&self, frame: u64) -> Option<i64> {
        self.position
            .map(|p| p + (frame as i64 - self.start as i64))
    }
}

/// Part of a channel's frames: shared from a full chunk, copied from the
/// one being filled, or silence (before the channel was kept).
#[derive(Clone, Debug)]
pub enum Piece {
    Shared(Arc<Vec<f32>>, std::ops::Range<usize>),
    Owned(Vec<f32>),
    Silence(usize),
}

impl Piece {
    pub fn len(&self) -> usize {
        match self {
            Piece::Shared(_, r) => r.len(),
            Piece::Owned(v) => v.len(),
            Piece::Silence(n) => *n,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One device channel's kept input.
#[derive(Debug)]
pub struct Channel {
    pub channel: u16,
    /// Frame of the first sample kept.
    start: u64,
    full: VecDeque<Arc<Vec<f32>>>,
    partial: Vec<f32>,
    /// Peaks of closed marks; `marks[i]` is mark `mark0 + i` (mark `m`
    /// covers frames `m·MARK..(m+1)·MARK`).
    marks: VecDeque<f32>,
    mark0: u64,
    /// The loudest closed mark of each group of [`GROUP`] marks from
    /// group `group0` (the oldest may count marks evicted since).
    groups: VecDeque<f32>,
    group0: u64,
    /// The mark being filled: its index and peak so far.
    open: (u64, f32),
    /// Closed marks per level bin.
    hist: [u32; BINS],
}

fn bin(peak: f32) -> usize {
    let db = 20.0 * peak.max(1e-6).log10();
    (db.round() as i32 + 120).clamp(0, BINS as i32 - 1) as usize
}

/// Where sound starts above a channel's noise floor (dB over the 10th
/// percentile of its marks), and the bounds of that threshold.
const ABOVE_NOISE_DB: f32 = 15.0;
const QUIETEST_SOUND_DB: f32 = -60.0;
const LOUDEST_THRESHOLD_DB: f32 = -30.0;

impl Channel {
    fn new(channel: u16, start: u64) -> Self {
        Self {
            channel,
            start,
            full: VecDeque::new(),
            partial: Vec::with_capacity(CHUNK),
            marks: VecDeque::new(),
            mark0: start / MARK,
            groups: VecDeque::new(),
            group0: start / MARK / GROUP,
            open: (start / MARK, 0.0),
            hist: [0; BINS],
        }
    }

    pub fn start(&self) -> u64 {
        self.start
    }

    pub fn end(&self) -> u64 {
        self.start + (self.full.len() * CHUNK + self.partial.len()) as u64
    }

    /// Chunks allocated (the one being filled too).
    fn chunks(&self) -> usize {
        self.full.len() + 1
    }

    fn append(&mut self, samples: &[f32], spare: &mut Vec<Vec<f32>>) {
        let mut rest = samples;
        let mut frame = self.end();
        while !rest.is_empty() {
            // Up to the end of the chunk and of the mark.
            let room = CHUNK - self.partial.len();
            let to_mark = ((self.open.0 + 1) * MARK - frame) as usize;
            let n = rest.len().min(room).min(to_mark);
            let (now, later) = rest.split_at(n);
            self.partial.extend_from_slice(now);
            let peak = now.iter().fold(0.0f32, |p, s| p.max(s.abs()));
            self.open.1 = self.open.1.max(peak);
            frame += n as u64;
            if frame == (self.open.0 + 1) * MARK {
                let (m, peak) = self.open;
                self.marks.push_back(peak);
                self.hist[bin(peak)] += 1;
                let g = m / GROUP;
                while self.group0 + (self.groups.len() as u64) <= g {
                    self.groups.push_back(0.0);
                }
                if let Some(v) = self.groups.get_mut((g - self.group0) as usize) {
                    *v = v.max(peak);
                }
                self.open = (m + 1, 0.0);
            }
            if self.partial.len() == CHUNK {
                let next = spare.pop().unwrap_or_else(|| Vec::with_capacity(CHUNK));
                let done = std::mem::replace(&mut self.partial, next);
                self.full.push_back(Arc::new(done));
            }
            rest = later;
        }
    }

    /// Keep at most `chunks` chunks: the oldest go (into `spare` when no
    /// capture holds them).
    fn evict(&mut self, chunks: usize, spare: &mut Vec<Vec<f32>>) {
        while self.chunks() > chunks.max(1) {
            let Some(old) = self.full.pop_front() else {
                break;
            };
            self.start += CHUNK as u64;
            if spare.is_empty()
                && let Ok(mut v) = Arc::try_unwrap(old)
            {
                v.clear();
                spare.push(v);
            }
        }
        let first = self.start / MARK;
        while self.mark0 < first {
            let Some(peak) = self.marks.pop_front() else {
                break;
            };
            self.hist[bin(peak)] -= 1;
            self.mark0 += 1;
        }
        while self.group0 < self.mark0 / GROUP && self.groups.pop_front().is_some() {
            self.group0 += 1;
        }
    }

    /// The loudest mark of group `g` (`None`: nothing kept there).
    pub fn group(&self, g: u64) -> Option<f32> {
        let stored = g
            .checked_sub(self.group0)
            .and_then(|i| self.groups.get(i as usize))
            .copied();
        let open = (self.open.0 / GROUP == g).then_some(self.open.1);
        match (stored, open) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// The peak of mark `m` (the one being filled too).
    pub fn mark(&self, m: u64) -> Option<f32> {
        if m < self.mark0 {
            return None;
        }
        let i = (m - self.mark0) as usize;
        match self.marks.get(i) {
            Some(p) => Some(*p),
            None => (m == self.open.0).then_some(self.open.1),
        }
    }

    /// The level sound starts at on this channel (linear): 15 dB over its
    /// noise floor (the 10th percentile of its marks), between −60 and
    /// −30 dBFS.
    pub fn threshold(&self) -> f32 {
        let total: u32 = self.hist.iter().sum();
        let mut db = QUIETEST_SOUND_DB;
        if total > 0 {
            let tenth = total.div_ceil(10);
            let mut seen = 0;
            for (b, n) in self.hist.iter().enumerate() {
                seen += n;
                if seen >= tenth {
                    db = (b as f32 - 120.0 + ABOVE_NOISE_DB)
                        .clamp(QUIETEST_SOUND_DB, LOUDEST_THRESHOLD_DB);
                    break;
                }
            }
        }
        10f32.powf(db / 20.0)
    }

    /// The sample at `frame`, if kept.
    pub fn sample(&self, frame: u64) -> Option<f32> {
        if frame < self.start || frame >= self.end() {
            return None;
        }
        let i = (frame - self.start) as usize;
        let (c, k) = (i / CHUNK, i % CHUNK);
        match self.full.get(c) {
            Some(chunk) => chunk.get(k).copied(),
            None => self.partial.get(k).copied(),
        }
    }

    /// Frames `from..to` (silence for what is not kept before; `to` at
    /// most [`Self::end`]).
    pub fn extract(&self, from: u64, to: u64) -> Vec<Piece> {
        let mut out = Vec::new();
        let to = to.min(self.end());
        if from >= to {
            return out;
        }
        if from < self.start {
            out.push(Piece::Silence((self.start.min(to) - from) as usize));
        }
        let mut at = from.max(self.start);
        while at < to {
            let i = (at - self.start) as usize;
            let (c, k) = (i / CHUNK, i % CHUNK);
            let n = (CHUNK - k).min((to - at) as usize);
            match self.full.get(c) {
                Some(chunk) => out.push(Piece::Shared(Arc::clone(chunk), k..k + n)),
                None => {
                    let n = n.min(self.partial.len().saturating_sub(k));
                    if n == 0 {
                        break;
                    }
                    out.push(Piece::Owned(self.partial[k..k + n].to_vec()));
                }
            }
            at += n as u64;
        }
        out
    }
}

/// Everything kept (see the module docs).
#[derive(Debug)]
pub struct Store {
    /// The engine's rate the frames are at.
    pub rate: u32,
    channels: Vec<Channel>,
    spans: VecDeque<Span>,
    /// The frame after the newest kept.
    newest: u64,
    /// Added to the engine's frame counter (a new engine counts from 0).
    offset: u64,
    /// The next span begins a run (after a new engine).
    break_run: bool,
    /// Frames lost (the reader fell behind; kept as silence).
    lost: u64,
    /// How long a channel keeps and the memory all channels may use.
    seconds: f64,
    max_bytes: usize,
    /// One evicted chunk's buffer for the next.
    spare: Vec<Vec<f32>>,
    /// Nothing was kept since the store was made or last held no channel:
    /// the next block starts it afresh (the time in between is not lost).
    resync: bool,
    /// What the recorder took from this frame on was thrown away (an
    /// aborted take): not counted as recorded.
    forgiven_from: Option<u64>,
}

impl Store {
    pub fn new(rate: u32, seconds: f64, max_bytes: usize) -> Self {
        Self {
            rate: rate.max(1),
            channels: Vec::new(),
            spans: VecDeque::new(),
            newest: 0,
            offset: 0,
            break_run: false,
            lost: 0,
            seconds,
            max_bytes,
            spare: Vec::new(),
            resync: true,
            forgiven_from: None,
        }
    }

    /// Chunks each channel may hold: as long as asked, within the memory
    /// limit (counting the spare), at least two.
    pub fn chunks_per_channel(&self) -> usize {
        let by_time = (self.seconds * f64::from(self.rate) / CHUNK as f64).ceil() as usize + 1;
        let n = self.channels.len().max(1);
        let by_memory = (self.max_bytes / CHUNK_BYTES).saturating_sub(1) / n;
        by_time.min(by_memory).max(2)
    }

    pub fn set_limits(&mut self, seconds: f64, max_bytes: usize) {
        self.seconds = seconds;
        self.max_bytes = max_bytes;
        self.evict();
    }

    /// Keep these device channels from now on: those kept stay, new ones
    /// start at the newest frame, others go.
    pub fn set_channels(&mut self, channels: &[u16]) {
        self.channels.retain(|c| channels.contains(&c.channel));
        if self.channels.is_empty() {
            self.resync = true;
        }
        for &ch in channels {
            if !self.channels.iter().any(|c| c.channel == ch) {
                self.channels.push(Channel::new(ch, self.newest));
            }
        }
        self.channels.sort_by_key(|c| c.channel);
        self.evict();
    }

    pub fn channel(&self, ch: u16) -> Option<&Channel> {
        self.channels.iter().find(|c| c.channel == ch)
    }

    pub fn channels(&self) -> impl Iterator<Item = &Channel> {
        self.channels.iter()
    }

    pub fn spans(&self) -> &VecDeque<Span> {
        &self.spans
    }

    /// The frame after the newest kept.
    pub fn newest(&self) -> u64 {
        self.newest
    }

    /// The first frame every kept channel has (the newest if none).
    pub fn oldest(&self) -> u64 {
        self.channels
            .iter()
            .map(Channel::start)
            .min()
            .unwrap_or(self.newest)
    }

    pub fn lost(&self) -> u64 {
        self.lost
    }

    /// Bytes of samples held (chunks and the spare).
    pub fn bytes(&self) -> usize {
        let chunks: usize = self.channels.iter().map(Channel::chunks).sum();
        (chunks + self.spare.len()) * CHUNK_BYTES
    }

    /// Seconds the longest kept channel holds.
    pub fn seconds_kept(&self) -> f64 {
        (self.newest - self.oldest()) as f64 / f64::from(self.rate)
    }

    /// One block from the engine: `frames` at engine frame `frame`, the
    /// samples of `channels` one after another (`None`: lost, kept as
    /// silence).
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        frame: u64,
        frames: usize,
        position: Option<i64>,
        recording: bool,
        channels: &[u16],
        samples: Option<&[f32]>,
        zeros: &mut Vec<f32>,
    ) {
        if frames == 0 {
            return;
        }
        let mut frame = frame + self.offset;
        if frame < self.newest {
            // A new engine counts from 0: continue where the last ended.
            self.offset += self.newest - frame;
            frame = self.newest;
            self.break_run = true;
        }
        if std::mem::take(&mut self.resync) {
            // Nothing kept up to here: start afresh.
            self.newest = frame;
            self.spans.clear();
            for c in &mut self.channels {
                *c = Channel::new(c.channel, frame);
            }
        }
        if frame > self.newest {
            // Blocks the reader never got: silence in their place, as the
            // transport was.
            let gap = (frame - self.newest) as usize;
            self.lost += gap as u64;
            let last = self.spans.back().copied();
            self.fill_silence(gap, zeros);
            match last {
                Some(s) => self.note(s.position_at(s.end()), s.recording, gap),
                None => self.note(None, false, gap),
            }
        }
        match samples {
            Some(data) => {
                for c in &mut self.channels {
                    match channels.iter().position(|x| *x == c.channel) {
                        Some(i) => match data.get(i * frames..(i + 1) * frames) {
                            Some(s) => c.append(s, &mut self.spare),
                            None => {
                                zeros.clear();
                                zeros.resize(frames, 0.0);
                                c.append(zeros, &mut self.spare);
                            }
                        },
                        None => {
                            zeros.clear();
                            zeros.resize(frames, 0.0);
                            c.append(zeros, &mut self.spare);
                        }
                    }
                }
            }
            None => {
                self.lost += frames as u64;
                self.fill_silence(frames, zeros);
            }
        }
        self.note(position, recording, frames);
        self.evict();
    }

    fn fill_silence(&mut self, frames: usize, zeros: &mut Vec<f32>) {
        zeros.clear();
        zeros.resize(frames.min(CHUNK), 0.0);
        let mut left = frames;
        while left > 0 {
            let n = left.min(zeros.len());
            for c in &mut self.channels {
                c.append(&zeros[..n], &mut self.spare);
            }
            left -= n;
        }
    }

    /// A recording begins (from about the newest frame): what it takes
    /// counts as recorded. Returns that frame.
    pub fn recording_starts(&mut self) -> u64 {
        self.forgiven_from = None;
        self.newest
    }

    /// The recording begun at `from` was thrown away: what it took is
    /// offered again, blocks still on their way too.
    pub fn forgive_recording(&mut self, from: u64) {
        self.forgiven_from = Some(from);
        for s in self.spans.iter_mut().filter(|s| s.end() > from) {
            s.recording = false;
        }
    }

    /// The transport over the `frames` just appended.
    fn note(&mut self, position: Option<i64>, recording: bool, frames: usize) {
        let start = self.newest;
        let recording = recording && self.forgiven_from.is_none_or(|f| start < f);
        self.newest += frames as u64;
        let (run, pass) = match self.spans.back() {
            None => (0, 0),
            Some(last) if std::mem::take(&mut self.break_run) => (last.run + 1, 0),
            Some(last) => match (last.position, position) {
                (Some(_), Some(p)) if last.position_at(start) == Some(p) => (last.run, last.pass),
                (Some(_), Some(_)) => (last.run, last.pass + 1),
                (None, None) => (last.run, last.pass),
                _ => (last.run + 1, 0),
            },
        };
        if let Some(last) = self.spans.back_mut()
            && last.run == run
            && last.pass == pass
            && last.recording == recording
            && last.end() == start
        {
            last.frames += frames as u64;
            return;
        }
        self.spans.push_back(Span {
            start,
            frames: frames as u64,
            position,
            run,
            pass,
            recording,
        });
    }

    fn evict(&mut self) {
        let chunks = self.chunks_per_channel();
        for c in &mut self.channels {
            c.evict(chunks, &mut self.spare);
        }
        let oldest = self.oldest();
        while self.spans.len() > 1 && self.spans.front().is_some_and(|s| s.end() <= oldest) {
            self.spans.pop_front();
        }
    }

    /// The span holding `frame`.
    pub fn span_at(&self, frame: u64) -> Option<&Span> {
        let i = self.spans.partition_point(|s| s.end() <= frame);
        self.spans.get(i).filter(|s| s.start <= frame)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn push(store: &mut Store, frame: u64, data: &[f32], position: Option<i64>) {
        let channels: Vec<u16> = store.channels().map(|c| c.channel).collect();
        let n = data.len();
        let all: Vec<f32> = channels.iter().flat_map(|_| data.iter().copied()).collect();
        store.push(
            frame,
            n,
            position,
            false,
            &channels,
            Some(&all),
            &mut Vec::new(),
        );
    }

    fn read(store: &Store, ch: u16, from: u64, to: u64) -> Vec<f32> {
        let mut out = Vec::new();
        for p in store.channel(ch).unwrap().extract(from, to) {
            match p {
                Piece::Shared(c, r) => out.extend_from_slice(&c[r]),
                Piece::Owned(v) => out.extend(v),
                Piece::Silence(n) => out.resize(out.len() + n, 0.0),
            }
        }
        out
    }

    #[test]
    fn samples_come_back_as_they_went_in() {
        let mut s = Store::new(48_000, 60.0, 1 << 30);
        s.set_channels(&[3, 1]);
        let data: Vec<f32> = (0..50_000).map(|i| i as f32).collect();
        for (k, block) in data.chunks(333).enumerate() {
            push(&mut s, 1_000 + k as u64 * 333, block, None);
        }
        assert_eq!(s.oldest(), 1_000);
        assert_eq!(s.newest(), 51_000);
        assert_eq!(read(&s, 1, 1_000, 51_000), data);
        assert_eq!(read(&s, 3, 20_000, 20_010), data[19_000..19_010]);
        assert_eq!(s.channel(1).unwrap().sample(1_005), Some(5.0));
        // Before the channel was kept: silence.
        assert_eq!(
            read(&s, 1, 990, 1_002),
            [0.0; 10]
                .iter()
                .chain(&[0.0, 1.0])
                .copied()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_memory_limit_holds_and_the_oldest_goes() {
        // Two channels, 1 MiB: seven chunks a channel and the spare.
        let limit = 1 << 20;
        let mut s = Store::new(48_000, 600.0, limit);
        s.set_channels(&[0, 1]);
        assert_eq!(s.chunks_per_channel(), 7);
        let block: Vec<f32> = vec![0.25; 512];
        // Ten minutes in blocks of 512.
        for k in 0..(48_000 * 600 / 512) as u64 {
            push(&mut s, k * 512, &block, Some(k as i64 * 512));
            assert!(s.bytes() <= limit, "{} bytes", s.bytes());
        }
        let kept = s.newest() - s.oldest();
        assert!(
            kept > 6 * CHUNK as u64 && kept <= 7 * CHUNK as u64,
            "{kept}"
        );
        // Only the spans of what is kept are.
        assert!(s.spans().front().unwrap().end() > s.oldest());
        // A shorter length keeps less.
        s.set_limits(1.0, limit);
        assert_eq!(s.chunks_per_channel(), 4);
        assert!(s.newest() - s.oldest() <= 4 * CHUNK as u64);
    }

    #[test]
    fn runs_and_passes_follow_the_transport() {
        let mut s = Store::new(48_000, 60.0, 1 << 30);
        s.set_channels(&[0]);
        let b = vec![0.0f32; 100];
        push(&mut s, 0, &b, None);
        push(&mut s, 100, &b, None);
        // Play from 1000, wrap back to 1000 after 200.
        push(&mut s, 200, &b, Some(1_000));
        push(&mut s, 300, &b, Some(1_100));
        push(&mut s, 400, &b, Some(1_000));
        // Stop; play again from where it stood.
        push(&mut s, 500, &b, None);
        push(&mut s, 600, &b, Some(1_100));
        let spans: Vec<(u64, u64, Option<i64>, u32, u32)> = s
            .spans()
            .iter()
            .map(|s| (s.start, s.frames, s.position, s.run, s.pass))
            .collect();
        assert_eq!(
            spans,
            [
                (0, 200, None, 0, 0),
                (200, 200, Some(1_000), 1, 0),
                (400, 100, Some(1_000), 1, 1),
                (500, 100, None, 2, 0),
                (600, 100, Some(1_100), 3, 0),
            ]
        );
        assert_eq!(s.span_at(450).unwrap().pass, 1);
        assert_eq!(s.span_at(450).unwrap().position_at(450), Some(1_050));
    }

    #[test]
    fn lost_blocks_and_a_new_engine_keep_later_audio_in_place() {
        let mut s = Store::new(48_000, 60.0, 1 << 30);
        s.set_channels(&[0]);
        let b = vec![0.5f32; 100];
        push(&mut s, 0, &b, Some(0));
        // A block lost in the ring: silence, the pass goes on.
        s.push(100, 100, Some(100), false, &[0], None, &mut Vec::new());
        // One the reader never saw (its header was lost too).
        push(&mut s, 300, &b, Some(300));
        assert_eq!(s.lost(), 200);
        assert_eq!(s.spans().len(), 1, "{:?}", s.spans());
        assert_eq!(
            read(&s, 0, 95, 105),
            [0.5; 5]
                .iter()
                .chain(&[0.0; 5])
                .copied()
                .collect::<Vec<_>>()
        );
        assert_eq!(read(&s, 0, 300, 302), [0.5, 0.5]);
        // A new engine counts from 0 again: it continues, as a new run.
        push(&mut s, 0, &b, Some(0));
        assert_eq!(s.newest(), 500);
        assert_eq!(s.spans().back().unwrap().run, 1);
        assert_eq!(read(&s, 0, 398, 402), [0.5; 4]);
    }

    #[test]
    fn a_recording_thrown_away_is_not_counted_as_recorded() {
        let mut s = Store::new(48_000, 60.0, 1 << 30);
        s.set_channels(&[0]);
        let b = vec![0.5f32; 100];
        let take = |s: &mut Store, frame: u64| {
            s.push(
                frame,
                100,
                Some(frame as i64),
                true,
                &[0],
                Some(&b),
                &mut Vec::new(),
            );
        };
        let from = s.recording_starts();
        take(&mut s, 0);
        take(&mut s, 100);
        assert!(s.spans().iter().all(|s| s.recording));
        s.forgive_recording(from);
        assert!(s.spans().iter().all(|s| !s.recording));
        // A block of it still on its way.
        take(&mut s, 200);
        assert!(s.spans().iter().all(|s| !s.recording));
        // The next recording counts.
        s.recording_starts();
        take(&mut s, 300);
        assert!(s.spans().back().unwrap().recording);
    }

    #[test]
    fn the_threshold_sits_over_the_noise_floor() {
        let mut s = Store::new(48_000, 60.0, 1 << 30);
        s.set_channels(&[0]);
        // Digital silence: the quietest threshold.
        push(&mut s, 0, &vec![0.0; 25_600], None);
        let quiet = s.channel(0).unwrap().threshold();
        assert!((20.0 * quiet.log10() + 60.0).abs() < 0.01, "{quiet}");
        // Hum at −50 dBFS most of the time: 15 dB over it.
        let hum = 10f32.powf(-50.0 / 20.0);
        push(&mut s, 25_600, &vec![hum; 256_000], None);
        let t = 20.0 * s.channel(0).unwrap().threshold().log10();
        assert!((t + 35.0).abs() < 0.6, "{t}");
        // Loud all the time: never above −30 dBFS.
        push(&mut s, 281_600, &vec![0.5; 2_560_000], None);
        let t = 20.0 * s.channel(0).unwrap().threshold().log10();
        assert!((t + 30.0).abs() < 0.01, "{t}");
    }
}
