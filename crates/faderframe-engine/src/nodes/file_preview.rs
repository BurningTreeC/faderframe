//! A file previewed from the media browser, heard beside the mix.
//!
//! It lives in the master's listening node ([`super::ListenOut`]), after
//! the compare source and the codec monitor, before headphones, the mono
//! check and bass management: it is heard the way the mix is, but never
//! coded, compared, metered, analysed or rendered (renders build their own
//! engine; the master strip meters before it).
//!
//! * **Audio as it is decoded.** A worker on the control side decodes the
//!   file (resampled to the stream's rate, stretched to the project's tempo
//!   or transposed to its key when asked) into a [`PreviewAudio`]: chunks
//!   of [`CHUNK_FRAMES`] published one by one, so a long file starts at
//!   once. The audio thread reads what is there wait-free; a preview
//!   playing on its own waits at the edge of what is decoded rather than
//!   skip it.
//! * **When it plays.** [`PreviewStart::Now`]: from its first frame at once,
//!   whatever the transport does. [`PreviewStart::At`]: from a timeline
//!   frame (the next bar) as the transport — read from the play history at
//!   the node's arrival, so in step with the mix as heard — reaches it,
//!   then in step with the timeline; a looped one stays in phase with the
//!   bars ([`PreviewSound::loop_frames`]) through loop wraps and locates,
//!   and a stop ends it.
//! * **No clicks.** Every start, stop and change fades over
//!   [`FADE_SECONDS`]; a new sound waits for the old one to fade out.
//! * **Realtime.** Sounds reach the audio thread through a mailbox and go
//!   back through another, never freed there; the volume and the stop are
//!   atomics ([`FilePreviewShared`]).

use crate::context::EngineContext;
use faderframe_audio_graph::{AudioBuffer, ProcessContext};
use faderframe_core::ChannelLayout;
use faderframe_realtime::{MailboxReceiver, MailboxSender, mailbox};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Frames in one published chunk of a preview's audio.
pub const CHUNK_FRAMES: usize = 16_384;
/// How long starts, stops and changes fade.
pub const FADE_SECONDS: f64 = 0.005;
/// Channels a preview keeps (a wider file's first two).
pub const MAX_PREVIEW_CHANNELS: usize = 2;

/// A preview's audio, published chunk by chunk by the worker decoding it
/// and read by the audio thread (whatever is published is never changed).
pub struct PreviewAudio {
    rate: u32,
    channels: usize,
    /// Planar chunks: channel `c` at `c · CHUNK_FRAMES`.
    chunks: Box<[OnceLock<Box<[f32]>>]>,
    /// Frames published (the chunks holding them are set).
    ready: AtomicU64,
    /// Everything is published (`ready` is the length).
    done: AtomicBool,
}

impl std::fmt::Debug for PreviewAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewAudio")
            .field("rate", &self.rate)
            .field("channels", &self.channels)
            .field("ready", &self.ready())
            .field("done", &self.is_done())
            .finish()
    }
}

impl PreviewAudio {
    /// Room for `max_frames` frames of `channels` (1 or 2) at `rate`, and
    /// the writer that fills it (control side).
    pub fn new(rate: u32, channels: usize, max_frames: usize) -> (Arc<Self>, PreviewWriter) {
        let channels = channels.clamp(1, MAX_PREVIEW_CHANNELS);
        let count = max_frames.div_ceil(CHUNK_FRAMES).max(1);
        let audio = Arc::new(Self {
            rate,
            channels,
            chunks: (0..count).map(|_| OnceLock::new()).collect(),
            ready: AtomicU64::new(0),
            done: AtomicBool::new(false),
        });
        let writer = PreviewWriter {
            audio: Arc::clone(&audio),
            partial: vec![0.0; CHUNK_FRAMES * channels],
            filled: 0,
            chunk: 0,
        };
        (audio, writer)
    }

    /// Audio already decoded (tests, short sounds).
    pub fn from_planes(rate: u32, planes: &[Vec<f32>]) -> Arc<Self> {
        let frames = planes.iter().map(Vec::len).min().unwrap_or(0);
        let (audio, mut w) = Self::new(rate, planes.len(), frames);
        let views: Vec<&[f32]> = planes.iter().map(|p| &p[..frames]).collect();
        w.push(&views, frames);
        w.finish();
        audio
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Frames published so far.
    pub fn ready(&self) -> i64 {
        self.ready.load(Ordering::Acquire) as i64
    }

    /// Everything is published.
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// The length once done.
    pub fn frames(&self) -> Option<i64> {
        self.is_done().then(|| self.ready())
    }

    /// Frames `[from, from + out.len())` of channel `c` into `out` (zeros
    /// beyond what is published). Wait-free.
    pub fn read(&self, c: usize, from: i64, out: &mut [f32]) {
        let ready = self.ready();
        let c = c.min(self.channels - 1);
        let mut done = 0;
        while done < out.len() {
            let f = from + done as i64;
            let k =
                (CHUNK_FRAMES - (f.rem_euclid(CHUNK_FRAMES as i64) as usize)).min(out.len() - done);
            let dst = &mut out[done..done + k];
            match (f >= 0 && f < ready)
                .then(|| self.chunks.get(f as usize / CHUNK_FRAMES)?.get())
                .flatten()
            {
                Some(chunk) => {
                    let at = c * CHUNK_FRAMES + f as usize % CHUNK_FRAMES;
                    // Never past what is published.
                    let n = k.min((ready - f) as usize);
                    dst[..n].copy_from_slice(&chunk[at..at + n]);
                    dst[n..].fill(0.0);
                }
                None => dst.fill(0.0),
            }
            done += k;
        }
    }

    /// The loudest sample published, by a stride (control side; a quick
    /// look for level matching).
    pub fn peak(&self) -> f32 {
        let mut peak = 0.0f32;
        for chunk in self.chunks.iter().filter_map(OnceLock::get) {
            for v in chunk.iter().step_by(7) {
                peak = peak.max(v.abs());
            }
        }
        peak
    }
}

/// Fills a [`PreviewAudio`] (the worker decoding a file).
pub struct PreviewWriter {
    audio: Arc<PreviewAudio>,
    partial: Vec<f32>,
    filled: usize,
    chunk: usize,
}

impl PreviewWriter {
    /// The audio this writer fills.
    pub fn audio(&self) -> &Arc<PreviewAudio> {
        &self.audio
    }

    /// Frames written (published or not).
    pub fn written(&self) -> usize {
        self.chunk * CHUNK_FRAMES + self.filled
    }

    /// Is there room for more?
    pub fn full(&self) -> bool {
        self.chunk >= self.audio.chunks.len()
    }

    /// Append `n` frames (one slice per channel; a mono source feeds every
    /// channel, extra channels are left out). What does not fit is dropped.
    pub fn push(&mut self, planes: &[&[f32]], n: usize) {
        if planes.is_empty() {
            return;
        }
        let channels = self.audio.channels;
        let mut done = 0;
        while done < n && !self.full() {
            let k = (CHUNK_FRAMES - self.filled).min(n - done);
            for c in 0..channels {
                let src = planes[c.min(planes.len() - 1)];
                let to = c * CHUNK_FRAMES + self.filled;
                let have = src.len().saturating_sub(done).min(k);
                self.partial[to..to + have].copy_from_slice(&src[done..done + have]);
                self.partial[to + have..to + k].fill(0.0);
            }
            self.filled += k;
            done += k;
            if self.filled == CHUNK_FRAMES {
                self.publish(CHUNK_FRAMES);
            }
        }
    }

    fn publish(&mut self, frames: usize) {
        let channels = self.audio.channels;
        let mut whole = vec![0.0f32; CHUNK_FRAMES * channels].into_boxed_slice();
        for c in 0..channels {
            let at = c * CHUNK_FRAMES;
            whole[at..at + frames].copy_from_slice(&self.partial[at..at + frames]);
        }
        if let Some(slot) = self.audio.chunks.get(self.chunk) {
            let _ = slot.set(whole);
        }
        self.audio.ready.store(
            (self.chunk * CHUNK_FRAMES + frames) as u64,
            Ordering::Release,
        );
        if frames == CHUNK_FRAMES {
            self.chunk += 1;
            self.filled = 0;
        }
    }

    /// Everything is written: the last part published, the length known.
    pub fn finish(mut self) {
        if self.filled > 0 && !self.full() {
            let frames = self.filled;
            self.publish(frames);
        }
        self.audio.done.store(true, Ordering::Release);
    }
}

/// When a preview starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewStart {
    /// At once, on its own (the transport does not matter).
    Now,
    /// When the transport reaches this timeline frame, then in step with
    /// it (a stop ends it).
    At(i64),
}

/// What the listening node plays beside the mix.
#[derive(Clone, Debug)]
pub struct PreviewSound {
    pub audio: Arc<PreviewAudio>,
    pub start: PreviewStart,
    /// Round and round (until stopped).
    pub looped: bool,
    /// A loop's length in frames (bars at the project's tempo, so it stays
    /// in phase with them); 0: the audio's own length.
    pub loop_frames: i64,
}

/// The audio thread's side of one sound (empty: none).
struct PreviewStream {
    sound: Option<PreviewSound>,
    generation: u64,
    /// `Now`: the next frame.
    pos: i64,
    /// `At`: the start was reached.
    started: bool,
    /// Played out (or stopped): fading away.
    ended: bool,
    /// It began this block from its first frame: heard at once, unfaded
    /// (a drum's attack kept); joined later, it fades in.
    opened: bool,
}

impl PreviewStream {
    fn of(sound: Option<PreviewSound>, generation: u64) -> Self {
        Self {
            sound,
            generation,
            pos: 0,
            started: false,
            ended: false,
            opened: false,
        }
    }
}

/// The preview's controls and what it reports (atomics shared by the
/// control side and the audio thread).
#[derive(Debug)]
pub struct FilePreviewShared {
    /// Linear gain (f32 bits).
    gain: AtomicU32,
    /// Stop the sound playing (fades out).
    stop: AtomicU64,
    /// The generation of the sound the node has.
    installed: AtomicU64,
    /// The generation whose sound has ended (played out or stopped).
    ended: AtomicU64,
    /// The audio frame read last (−1: none yet).
    position: AtomicI64,
}

impl Default for FilePreviewShared {
    fn default() -> Self {
        Self {
            gain: AtomicU32::new(1.0f32.to_bits()),
            stop: AtomicU64::new(0),
            installed: AtomicU64::new(0),
            ended: AtomicU64::new(0),
            position: AtomicI64::new(-1),
        }
    }
}

impl FilePreviewShared {
    /// The preview's volume (linear, 0…4; nonsense is unity).
    pub fn set_gain(&self, gain: f32) {
        let g = if gain.is_finite() {
            gain.clamp(0.0, 4.0)
        } else {
            1.0
        };
        self.gain.store(g.to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    /// The generation of the sound the node plays (see
    /// [`FilePreviewLink::set`]).
    pub fn installed(&self) -> u64 {
        self.installed.load(Ordering::Relaxed)
    }

    /// Has the sound of `generation` ended (played out, stopped, or
    /// replaced)?
    pub fn has_ended(&self, generation: u64) -> bool {
        self.ended.load(Ordering::Relaxed) >= generation
    }

    /// The audio frame the node read last (`None`: none yet, or waiting
    /// for its start).
    pub fn position(&self) -> Option<i64> {
        let p = self.position.load(Ordering::Relaxed);
        (p >= 0).then_some(p)
    }
}

/// The way a preview reaches the running listening node and comes back
/// (one per engine, inside its [`super::ListenLink`]).
pub struct FilePreviewLink {
    /// The generation last handed over (control thread only).
    generation: Mutex<u64>,
    to_audio: (MailboxSender<PreviewStream>, MailboxReceiver<PreviewStream>),
    back: (MailboxSender<PreviewStream>, MailboxReceiver<PreviewStream>),
    shared: Arc<FilePreviewShared>,
}

impl FilePreviewLink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            generation: Mutex::new(0),
            to_audio: mailbox(),
            back: mailbox(),
            shared: Arc::new(FilePreviewShared::default()),
        })
    }

    pub fn shared(&self) -> &Arc<FilePreviewShared> {
        &self.shared
    }

    /// Control thread: play `sound` (the one playing fades out first), or
    /// stop (`None`). Returns its generation (see
    /// [`FilePreviewShared::installed`]); what comes back is dropped here.
    pub fn set(&self, sound: Option<PreviewSound>) -> u64 {
        let generation = {
            let mut g = self
                .generation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *g += 1;
            *g
        };
        if sound.is_none() {
            // Whatever plays ends (also before the empty stream arrives).
            self.shared.stop.store(generation, Ordering::Relaxed);
        }
        drop(
            self.to_audio
                .0
                .send(Box::new(PreviewStream::of(sound, generation))),
        );
        self.collect();
        generation
    }

    /// Control thread: drop what the node retired.
    pub fn collect(&self) {
        while self.back.1.take().is_some() {}
    }
}

impl std::fmt::Debug for FilePreviewLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FilePreviewLink")
    }
}

/// The preview inside the listening node.
pub(crate) struct FilePreview {
    link: Arc<FilePreviewLink>,
    now: Box<PreviewStream>,
    /// Coming in once `now` has faded out.
    next: Option<Box<PreviewStream>>,
    /// Retired, waiting for the way back to be free.
    retired: Option<Box<PreviewStream>>,
    /// The fade's place (0 silent … 1 full) and its step a frame.
    env: f32,
    step: f32,
    gain: f32,
    /// How late the audio arriving here is (the play history is read
    /// there).
    pub(crate) input_latency: u32,
    out: AudioBuffer,
    /// The preview's channels as read.
    read: [Box<[f32]>; MAX_PREVIEW_CHANNELS],
}

impl FilePreview {
    /// For a mix of `layout` at `rate`, blocks up to `block` (control
    /// thread).
    pub(crate) fn new(
        link: &Arc<FilePreviewLink>,
        layout: ChannelLayout,
        rate: u32,
        block: usize,
    ) -> Self {
        let block = block.max(1);
        let mut out = AudioBuffer::new(layout, block);
        out.set_len(block);
        Self {
            link: Arc::clone(link),
            now: Box::new(PreviewStream::of(None, 0)),
            next: None,
            retired: None,
            env: 0.0,
            step: (1.0 / (f64::from(rate.max(1)) * FADE_SECONDS)) as f32,
            gain: link.shared.gain(),
            input_latency: 0,
            out,
            read: std::array::from_fn(|_| vec![0.0; block].into_boxed_slice()),
        }
    }

    fn give_back(&mut self) {
        if let Some(old) = self.retired.take() {
            self.retired = self.link.back.0.send(old);
        }
    }

    /// `now` is done with: back it goes, `next` (or nothing) plays.
    fn swap_in(&mut self) {
        if self.retired.is_some() {
            return;
        }
        let shared = &self.link.shared;
        let next = self
            .next
            .take()
            .unwrap_or_else(|| Box::new(PreviewStream::of(None, self.now.generation)));
        let old = std::mem::replace(&mut self.now, next);
        if old.sound.is_some() {
            shared.ended.fetch_max(old.generation, Ordering::Relaxed);
        }
        if self.now.sound.is_none() {
            shared
                .ended
                .fetch_max(self.now.generation, Ordering::Relaxed);
        }
        shared
            .installed
            .store(self.now.generation, Ordering::Relaxed);
        self.retired = Some(old);
        self.give_back();
    }

    /// Audio thread: the mix (`input`) with the preview added, or `input`
    /// itself while nothing previews.
    pub(crate) fn process<'b>(
        &'b mut self,
        cx: &ProcessContext<'_, EngineContext>,
        input: &'b AudioBuffer,
        n: usize,
    ) -> &'b AudioBuffer {
        self.give_back();
        if self.next.is_none()
            && self.retired.is_none()
            && let Some(new) = self.link.to_audio.1.take()
        {
            self.next = Some(new);
        }
        let shared = Arc::clone(&self.link.shared);
        // A stop asked for this sound (or a later one): it fades away.
        if shared.stop.load(Ordering::Relaxed) >= self.now.generation && self.now.sound.is_some() {
            self.now.ended = true;
        }
        // Nothing heard: a new one starts at once.
        if self.env <= 0.0 && (self.next.is_some() || self.now.ended) {
            self.swap_in();
        }
        let n = n.min(self.out.capacity());
        // (Two `Arc`s cloned: no allocation, and `now` keeps them alive.)
        let Some(sound) = self.now.sound.clone() else {
            shared.position.store(-1, Ordering::Relaxed);
            return input;
        };
        let goal = shared.gain();
        let target = if self.now.ended || self.next.is_some() {
            0.0
        } else {
            1.0
        };
        let audio = &*sound.audio;
        let rate_ok = f64::from(audio.rate) == cx.sample_rate;
        // Read the block (silence where nothing plays).
        let channels = audio.channels;
        for r in self.read.iter_mut().take(channels) {
            r[..n].fill(0.0);
        }
        if rate_ok {
            self.fill(cx, &sound, n);
        } else {
            self.now.ended = true;
        }
        if std::mem::take(&mut self.now.opened) && target > 0.0 {
            self.env = 1.0;
        }
        // Into the mix: the preview's channels on the first two, a mono
        // preview on both, either into a mono mix as their mean.
        let outs = self.out.num_channels().min(input.num_channels());
        for c in 0..self.out.num_channels() {
            if c < input.num_channels() {
                self.out.channel_mut(c)[..n].copy_from_slice(&input.channel(c)[..n]);
            } else {
                self.out.channel_mut(c)[..n].fill(0.0);
            }
        }
        let (env0, g0) = (self.env, self.gain);
        let g_step = (goal - g0) / n.max(1) as f32;
        let mut env_end = env0;
        for c in 0..outs.min(2) {
            let (mut env, mut g) = (env0, g0);
            let dst = &mut self.out.channel_mut(c)[..n];
            for (i, o) in dst.iter_mut().enumerate() {
                env = if env < target {
                    (env + self.step).min(target)
                } else {
                    (env - self.step).max(target)
                };
                g += g_step;
                let v = if outs == 1 && channels == 2 {
                    0.5 * (self.read[0][i] + self.read[1][i])
                } else {
                    self.read[c.min(channels - 1)][i]
                };
                *o += v * g * env;
            }
            env_end = env;
        }
        if outs == 0 {
            env_end = if target > env0 {
                (env0 + self.step * n as f32).min(target)
            } else {
                (env0 - self.step * n as f32).max(target)
            };
        }
        self.env = env_end;
        self.gain = goal;
        // The block's own reference goes before `now` may be handed back
        // (the control side drops what comes back: never the last
        // reference here).
        drop(sound);
        if self.env <= 0.0 && (self.now.ended || self.next.is_some()) {
            self.swap_in();
        }
        &self.out
    }

    /// The block's frames of `sound` into `read` (silence before its start
    /// and after its end, which it notes).
    fn fill(&mut self, cx: &ProcessContext<'_, EngineContext>, sound: &PreviewSound, n: usize) {
        let audio = &*sound.audio;
        let total = audio.frames();
        let span = if sound.loop_frames > 0 {
            Some(sound.loop_frames)
        } else {
            total
        };
        let channels = audio.channels;
        let shared = &self.link.shared;
        match sound.start {
            PreviewStart::Now => {
                if !self.now.started {
                    self.now.started = true;
                    self.now.opened = self.now.pos == 0;
                }
                let mut done = 0;
                while done < n {
                    let mut pos = self.now.pos;
                    if sound.looped
                        && let Some(len) = span.filter(|l| *l > 0)
                        && pos >= len
                    {
                        pos = 0;
                    }
                    if !sound.looped && total.is_some_and(|t| pos >= t) {
                        self.now.ended = true;
                        break;
                    }
                    // What can be read now: up to the loop's end and what is
                    // published (waiting at its edge, not skipping it).
                    let mut k = (n - done) as i64;
                    if let Some(len) = span.filter(|_| sound.looped) {
                        k = k.min(len - pos);
                    }
                    if !audio.is_done() {
                        k = k.min(audio.ready() - pos);
                    }
                    if k <= 0 {
                        break;
                    }
                    let k = k as usize;
                    for (c, r) in self.read.iter_mut().enumerate().take(channels) {
                        audio.read(c, pos, &mut r[done..done + k]);
                    }
                    self.now.pos = pos + k as i64;
                    done += k;
                }
                shared.position.store(self.now.pos, Ordering::Relaxed);
            }
            PreviewStart::At(at) => {
                let latency = self.input_latency;
                let mut reached = None;
                let now = &mut self.now;
                let read = &mut self.read;
                cx.data.history.pieces(latency, 0, n, |o, len, played| {
                    if !played.playing || played.count_in > 0 {
                        if now.started {
                            // The transport stopped: so does the preview.
                            now.ended = true;
                        }
                        return;
                    }
                    let f = played.pos - at;
                    // Starting here: silent up to its first frame.
                    let mut i = 0usize;
                    if !now.started {
                        if f + len as i64 <= 0 {
                            return;
                        }
                        now.started = true;
                        now.opened = f <= 0;
                        i = (-f).max(0) as usize;
                    }
                    while i < len {
                        let g = f + i as i64;
                        let (idx, k) = if sound.looped {
                            match span.filter(|l| *l > 0) {
                                Some(l) => {
                                    let idx = g.rem_euclid(l);
                                    (Some(idx), ((l - idx) as usize).min(len - i))
                                }
                                None => (Some(g.max(0)), len - i),
                            }
                        } else if g < 0 {
                            (None, ((-g) as usize).min(len - i))
                        } else if total.is_some_and(|t| g >= t) {
                            now.ended = true;
                            (None, len - i)
                        } else {
                            (Some(g), len - i)
                        };
                        if let Some(idx) = idx {
                            for (c, r) in read.iter_mut().enumerate().take(channels) {
                                audio.read(c, idx, &mut r[o + i..o + i + k]);
                            }
                            reached = Some(idx + k as i64);
                        }
                        i += k;
                    }
                });
                shared
                    .position
                    .store(reached.unwrap_or(-1), Ordering::Relaxed);
            }
        }
    }
}
