//! A compare source in the monitor path: another stream that can take the
//! place of the mix as it is heard — the mix as it was some edits ago (the
//! undo history auditioned), a reference track — switched in an instant.
//!
//! It lives in the master's listening node ([`super::ListenOut`]), before
//! headphones, the mono check and bass management, so both sides are heard
//! the same way. It is monitoring only: renders build their own engine, and
//! nothing here reaches a file, a meter or the analysers (the master strip
//! before it meters the mix whichever side is heard).
//!
//! * **In step.** The stream's position comes from the play history at the
//!   node's arrival ([`crate::history::PlayHistory::pieces`] at its input
//!   latency): exactly the timeline position of the mix it sits beside, so
//!   locates, loop wraps and stops land on the same frame on both sides.
//!   [`CompareSync::Follow`] reads the file at that position less an
//!   anchor; [`CompareSync::Loop`] plays its own region round and round
//!   while the transport plays, whatever the timeline does.
//! * **Switching.** [`CompareShared::hear`] moves between the mix and the
//!   source with an equal-power crossfade over [`FADE_SECONDS`]; the
//!   source is read all the while it is heard at all, so the side coming in
//!   is already where the other one is. A new source (or new settings)
//!   crossfades from the old one; taking the source away fades back to the
//!   mix first. Nothing is rebuilt.
//! * **The listening gain.** The one gain of the monitor path, per side:
//!   [`CompareShared::set_gain`] plays the compare source louder or quieter
//!   (a reference or the past brought to the mix's loudness),
//!   [`CompareShared::set_mix_gain`] the mix (a candidate Mix Intelligence
//!   auditions brought to the original's loudness). Both are fairness gains
//!   for a comparison: heard only, never in a render, a file or a meter;
//!   each ramps over a block, nothing is rebuilt.
//! * **Realtime.** Streams reach the audio thread through a mailbox and go
//!   back through another, never freed there; the file is read wait-free
//!   from its page table ([`StreamSource`]: one reader — this node — whose
//!   pages the session's disk loader keeps resident round
//!   [`CompareShared::position`]; a page not there is silence).

use crate::context::EngineContext;
use faderframe_audio_files::StreamSource;
use faderframe_audio_graph::{AudioBuffer, ProcessContext};
use faderframe_core::ChannelLayout;
use faderframe_realtime::{MailboxReceiver, MailboxSender, mailbox};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// How long a switch between the mix and the compare source takes.
pub const FADE_SECONDS: f64 = 0.02;

/// No restart asked for.
const NO_SEEK: i64 = i64::MIN;

/// How the compare source follows the transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareSync {
    /// File frame = timeline position − `anchor` (an arrangement matched
    /// to the project, or a render of a stretch of it).
    Follow { anchor: i64 },
    /// The region over and over while the transport plays, on its own.
    Loop,
}

/// What of the file plays and how it follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompareSettings {
    pub sync: CompareSync,
    /// File frames `[from, to)` play (outside them: silence).
    pub from: i64,
    pub to: i64,
}

impl CompareSettings {
    /// The whole of a file of `frames`, following the timeline from
    /// `anchor`.
    pub fn follow(anchor: i64, frames: i64) -> Self {
        Self {
            sync: CompareSync::Follow { anchor },
            from: 0,
            to: frames.max(0),
        }
    }

    /// The file frame the timeline position `pos` plays (`Follow`), or
    /// `None` (`Loop`: its own).
    pub fn file_frame(&self, pos: i64) -> Option<i64> {
        match self.sync {
            CompareSync::Follow { anchor } => Some(pos - anchor),
            CompareSync::Loop => None,
        }
    }
}

/// A source the control side asks to be heard beside the mix.
#[derive(Clone, Debug)]
pub struct CompareSource {
    pub source: Arc<StreamSource>,
    pub settings: CompareSettings,
}

/// The audio thread's side of one source (empty: none).
pub struct CompareStream {
    source: Option<Arc<StreamSource>>,
    settings: CompareSettings,
    generation: u64,
    /// `Loop`: the next frame of the region.
    pos: i64,
}

impl CompareStream {
    fn empty(generation: u64) -> Self {
        Self {
            source: None,
            settings: CompareSettings::follow(0, 0),
            generation,
            pos: 0,
        }
    }

    fn of(wanted: Option<&CompareSource>, generation: u64) -> Self {
        match wanted {
            Some(w) => Self {
                source: Some(Arc::clone(&w.source)),
                settings: w.settings,
                generation,
                pos: w.settings.from,
            },
            None => Self::empty(generation),
        }
    }

    fn same_file(&self, other: &Self) -> bool {
        match (&self.source, &other.source) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// The compare source's controls and what it reports (atomics: the
/// control side, the audio thread and the disk loader share them).
#[derive(Debug)]
pub struct CompareShared {
    hear: AtomicBool,
    /// The source's gain (f32 bits, linear).
    gain: AtomicU32,
    /// The mix's gain (f32 bits, linear).
    mix_gain: AtomicU32,
    seek: AtomicI64,
    /// Published by the audio thread: the file frame read last.
    position: AtomicI64,
    /// How much of the source is heard (f32 bits, 0 = the mix, 1 = it).
    heard: AtomicU32,
    /// The generation of the stream the node has.
    installed: AtomicU64,
    /// The wanted settings, for the disk loader (it reads ahead of the
    /// audio thread): `Loop`, the anchor, the region.
    looped: AtomicBool,
    anchor: AtomicI64,
    from: AtomicI64,
    to: AtomicI64,
}

impl Default for CompareShared {
    fn default() -> Self {
        Self {
            hear: AtomicBool::new(false),
            gain: AtomicU32::new(1.0f32.to_bits()),
            mix_gain: AtomicU32::new(1.0f32.to_bits()),
            seek: AtomicI64::new(NO_SEEK),
            position: AtomicI64::new(0),
            heard: AtomicU32::new(0),
            installed: AtomicU64::new(0),
            looped: AtomicBool::new(false),
            anchor: AtomicI64::new(0),
            from: AtomicI64::new(0),
            to: AtomicI64::new(0),
        }
    }
}

impl CompareShared {
    /// Hear the compare source (or the mix).
    pub fn hear(&self, on: bool) {
        self.hear.store(on, Ordering::Relaxed);
    }

    /// Is the compare source asked to be heard?
    pub fn hearing(&self) -> bool {
        self.hear.load(Ordering::Relaxed)
    }

    /// The source's gain (linear; level matching).
    pub fn set_gain(&self, gain: f32) {
        let g = if gain.is_finite() { gain.max(0.0) } else { 1.0 };
        self.gain.store(g.to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    /// The mix's gain on the monitor path (linear; a candidate heard at
    /// the original's loudness). Nonsense is taken as unity.
    pub fn set_mix_gain(&self, gain: f32) {
        let g = if gain.is_finite() {
            gain.clamp(0.0, 16.0)
        } else {
            1.0
        };
        self.mix_gain.store(g.to_bits(), Ordering::Relaxed);
    }

    pub fn mix_gain(&self) -> f32 {
        f32::from_bits(self.mix_gain.load(Ordering::Relaxed))
    }

    /// `Loop`: play the region from this file frame on.
    pub fn seek(&self, frame: i64) {
        self.seek.store(frame, Ordering::Relaxed);
    }

    /// The file frame the audio thread read last.
    pub fn position(&self) -> i64 {
        self.position.load(Ordering::Relaxed)
    }

    /// How much of the compare source is heard now (0: the mix only, 1:
    /// the source only; between: crossfading).
    pub fn heard(&self) -> f32 {
        f32::from_bits(self.heard.load(Ordering::Relaxed))
    }

    /// The generation of the source the listening node plays (see
    /// [`CompareLink::set`]).
    pub fn installed(&self) -> u64 {
        self.installed.load(Ordering::Relaxed)
    }

    /// The file frames to keep resident for a transport at `position`
    /// (disk loader): where the audio thread reads, where the transport
    /// is (`Follow`), the region's start (`Loop`: where it wraps to).
    pub fn wanted_frames(&self, position: i64) -> [Option<i64>; 3] {
        let looped = self.looped.load(Ordering::Relaxed);
        [
            Some(self.position()),
            self.file_frame_at(position),
            looped.then(|| self.from.load(Ordering::Relaxed)),
        ]
    }

    /// The file frame a `Follow` source plays at the timeline position
    /// `pos` (`None`: it loops on its own).
    pub fn file_frame_at(&self, pos: i64) -> Option<i64> {
        (!self.looped.load(Ordering::Relaxed)).then(|| pos - self.anchor.load(Ordering::Relaxed))
    }

    fn note_settings(&self, s: Option<&CompareSettings>) {
        let Some(s) = s else {
            return;
        };
        let (looped, anchor) = match s.sync {
            CompareSync::Follow { anchor } => (false, anchor),
            CompareSync::Loop => (true, 0),
        };
        self.looped.store(looped, Ordering::Relaxed);
        self.anchor.store(anchor, Ordering::Relaxed);
        self.from.store(s.from, Ordering::Relaxed);
        self.to.store(s.to, Ordering::Relaxed);
    }
}

/// The way a compare source reaches the running listening node and comes
/// back (one per engine, inside its [`super::ListenLink`]).
pub struct CompareLink {
    /// What is wanted (control thread only: a listening node made later
    /// starts with it).
    wanted: Mutex<(Option<CompareSource>, u64)>,
    to_audio: (MailboxSender<CompareStream>, MailboxReceiver<CompareStream>),
    back: (MailboxSender<CompareStream>, MailboxReceiver<CompareStream>),
    shared: Arc<CompareShared>,
}

impl CompareLink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            wanted: Mutex::new((None, 0)),
            to_audio: mailbox(),
            back: mailbox(),
            shared: Arc::new(CompareShared::default()),
        })
    }

    /// The controls and what the node reports.
    pub fn shared(&self) -> &Arc<CompareShared> {
        &self.shared
    }

    /// Control thread: play `source` beside the mix (`None`: no compare
    /// source; the mix is heard). Returns its generation (see
    /// [`CompareShared::installed`]). Whatever comes back is dropped here.
    pub fn set(&self, source: Option<CompareSource>) -> u64 {
        let generation = {
            let mut w = self
                .wanted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            w.1 += 1;
            w.0 = source;
            self.shared.note_settings(w.0.as_ref().map(|s| &s.settings));
            if w.0.is_none() {
                self.shared.hear(false);
            }
            let stream = CompareStream::of(w.0.as_ref(), w.1);
            drop(self.to_audio.0.send(Box::new(stream)));
            w.1
        };
        self.collect();
        generation
    }

    /// The source wanted now.
    pub fn wanted(&self) -> Option<CompareSource> {
        self.wanted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .clone()
    }

    /// Control thread: drop what the node retired.
    pub fn collect(&self) {
        while self.back.1.take().is_some() {}
    }

    /// Control thread: the stream a new listening node starts with.
    fn current(&self) -> Box<CompareStream> {
        let w = self
            .wanted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Box::new(CompareStream::of(w.0.as_ref(), w.1))
    }
}

impl std::fmt::Debug for CompareLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompareLink")
    }
}

/// The compare source inside the listening node.
pub(crate) struct Compare {
    link: Arc<CompareLink>,
    now: Box<CompareStream>,
    /// Coming in (crossfading from `now` over `fade_len`, `swapped` done).
    next: Option<Box<CompareStream>>,
    swapped: usize,
    /// Retired, waiting for the way back to be free.
    retired: Option<Box<CompareStream>>,
    /// 0: the mix is heard, 1: the source.
    x: f32,
    gain: f32,
    /// The mix's gain where the last block ended.
    mix_gain: f32,
    fade_len: usize,
    /// No frame processed yet: the side asked for is heard at once.
    fresh: bool,
    /// How late the audio arriving here is (the play history is read
    /// there).
    pub(crate) input_latency: u32,
    /// What is heard (the mix, the source or both).
    out: AudioBuffer,
    /// The source as read, and the one coming in.
    a: AudioBuffer,
    b: AudioBuffer,
    /// A second channel when a stereo file plays into a mono mix.
    tmp: Box<[f32]>,
}

impl Compare {
    /// For a mix of `layout` at `rate`, blocks up to `block` (control
    /// thread).
    pub(crate) fn new(
        link: &Arc<CompareLink>,
        layout: ChannelLayout,
        rate: u32,
        block: usize,
    ) -> Self {
        let block = block.max(1);
        let buffer = || {
            let mut b = AudioBuffer::new(layout, block);
            b.set_len(block);
            b
        };
        Self {
            link: Arc::clone(link),
            now: link.current(),
            next: None,
            swapped: 0,
            retired: None,
            x: 0.0,
            gain: link.shared.gain(),
            mix_gain: link.shared.mix_gain(),
            fade_len: ((f64::from(rate) * FADE_SECONDS) as usize).max(1),
            fresh: true,
            input_latency: 0,
            out: buffer(),
            a: buffer(),
            b: buffer(),
            tmp: vec![0.0; block].into_boxed_slice(),
        }
    }

    fn give_back(&mut self) {
        if let Some(old) = self.retired.take() {
            self.retired = self.link.back.0.send(old);
        }
    }

    fn retire(&mut self, old: Box<CompareStream>) {
        // At most one waits: `take_incoming` takes nothing new meanwhile.
        debug_assert!(self.retired.is_none());
        self.retired = Some(old);
        self.give_back();
    }

    /// A stream coming in: at once when nothing of the source is heard (or
    /// it is the same one), else crossfaded.
    fn take_incoming(&mut self) {
        if self.next.is_some() || self.retired.is_some() {
            return;
        }
        let Some(mut new) = self.link.to_audio.1.take() else {
            return;
        };
        if new.same_file(&self.now)
            && new.settings.sync == CompareSync::Loop
            && self.now.settings.sync == CompareSync::Loop
        {
            // The same file looping: on from where it was.
            new.pos = self.now.pos;
        }
        let same = new.same_file(&self.now) && new.settings == self.now.settings;
        if self.x <= 0.0 || self.now.source.is_none() || same || self.fresh {
            let old = std::mem::replace(&mut self.now, new);
            self.retire(old);
        } else {
            self.next = Some(new);
            self.swapped = 0;
        }
    }

    /// Audio thread: what is heard for `input` (the mix arriving here):
    /// `input` itself while only the mix is heard, else the mix and the
    /// source crossfaded.
    pub(crate) fn process<'b>(
        &'b mut self,
        cx: &ProcessContext<'_, EngineContext>,
        input: &'b AudioBuffer,
        n: usize,
    ) -> &'b AudioBuffer {
        self.give_back();
        self.take_incoming();
        let shared = Arc::clone(&self.link.shared);
        let seek = shared.seek.swap(NO_SEEK, Ordering::Relaxed);
        if seek != NO_SEEK {
            self.now.pos = seek;
        }
        // Taking the source away (or a stream without a file coming in):
        // back to the mix first.
        let leaving = self.next.as_ref().is_some_and(|s| s.source.is_none());
        let target = if shared.hearing() && self.now.source.is_some() && !leaving {
            1.0
        } else {
            0.0
        };
        if self.fresh {
            self.x = target;
            self.gain = shared.gain();
            self.mix_gain = shared.mix_gain();
            self.fresh = false;
        }
        let mix_goal = shared.mix_gain();
        shared
            .installed
            .store(self.now.generation, Ordering::Relaxed);
        let n = n.min(self.out.capacity());
        if self.x <= 0.0 && target <= 0.0 {
            // The mix alone: nothing to read (a stream waiting to go is
            // swapped now).
            if let Some(next) = self.next.take() {
                let old = std::mem::replace(&mut self.now, next);
                self.retire(old);
            }
            // A looping source keeps its own time unheard: flipping to it
            // lands where it would be.
            if let Some(p) = skip(&mut self.now, &cx.data.history, self.input_latency, n) {
                shared.position.store(p, Ordering::Relaxed);
            }
            shared.heard.store(0.0f32.to_bits(), Ordering::Relaxed);
            if mix_goal == 1.0 && self.mix_gain == 1.0 {
                return input;
            }
            // The mix at its listening gain, ramped from where it was.
            let step = (mix_goal - self.mix_gain) / n.max(1) as f32;
            let channels = self.out.num_channels().min(input.num_channels());
            for c in 0..channels {
                let mut g = self.mix_gain;
                for (o, &m) in self.out.channel_mut(c)[..n]
                    .iter_mut()
                    .zip(&input.channel(c)[..n])
                {
                    g += step;
                    *o = m * g;
                }
            }
            for c in channels..self.out.num_channels() {
                self.out.channel_mut(c)[..n].fill(0.0);
            }
            self.mix_gain = mix_goal;
            return &self.out;
        }
        let history = &cx.data.history;
        let latency = self.input_latency;
        let pos = read(
            &mut self.now,
            &mut self.a,
            &mut self.tmp,
            history,
            latency,
            n,
        );
        let mut published = pos;
        if let Some(next) = self.next.as_mut()
            && next.source.is_some()
        {
            // One source to another (constant power: different signals).
            let p = read(next, &mut self.b, &mut self.tmp, history, latency, n);
            published = p;
            let len = self.fade_len as f32;
            for c in 0..self.a.num_channels() {
                let to = &self.b.channel(c)[..n];
                for (i, (o, &v)) in self.a.channel_mut(c)[..n].iter_mut().zip(to).enumerate() {
                    let t = ((self.swapped + i + 1) as f32 / len).min(1.0)
                        * std::f32::consts::FRAC_PI_2;
                    *o = *o * t.cos() + v * t.sin();
                }
            }
            self.swapped += n;
            if self.swapped >= self.fade_len
                && let Some(next) = self.next.take()
            {
                let old = std::mem::replace(&mut self.now, next);
                self.retire(old);
            }
        }
        if let Some(p) = published {
            shared.position.store(p, Ordering::Relaxed);
        }
        // Mix and source by `x`, the source at its gain (both moving
        // sample by sample: no zipper, no click).
        let step = 1.0 / self.fade_len as f32;
        let goal = shared.gain();
        let g_step = (goal - self.gain) / n.max(1) as f32;
        let m_step = (mix_goal - self.mix_gain) / n.max(1) as f32;
        let channels = self.out.num_channels().min(input.num_channels());
        let (x0, g0, m0) = (self.x, self.gain, self.mix_gain);
        for c in 0..channels {
            let (mut x, mut g, mut mg) = (x0, g0, m0);
            let mix = &input.channel(c)[..n];
            let src = &self.a.channel(c)[..n];
            for ((o, &m), &s) in self.out.channel_mut(c)[..n].iter_mut().zip(mix).zip(src) {
                x = if x < target {
                    (x + step).min(target)
                } else {
                    (x - step).max(target)
                };
                g += g_step;
                mg += m_step;
                let (wm, ws) = weights(x);
                *o = m * mg * wm + s * g * ws;
            }
        }
        for c in channels..self.out.num_channels() {
            self.out.channel_mut(c)[..n].fill(0.0);
        }
        self.x = if x0 < target {
            (x0 + step * n as f32).min(target)
        } else {
            (x0 - step * n as f32).max(target)
        };
        self.gain = goal;
        self.mix_gain = mix_goal;
        if self.x <= 0.0
            && leaving
            && let Some(next) = self.next.take()
        {
            let old = std::mem::replace(&mut self.now, next);
            self.retire(old);
        }
        shared.heard.store(self.x.to_bits(), Ordering::Relaxed);
        &self.out
    }
}

/// The mix's and the source's weights at `x` (equal power; exact at
/// either end).
#[inline]
fn weights(x: f32) -> (f32, f32) {
    if x <= 0.0 {
        (1.0, 0.0)
    } else if x >= 1.0 {
        (0.0, 1.0)
    } else {
        let t = x * std::f32::consts::FRAC_PI_2;
        (t.cos(), t.sin())
    }
}

/// A looping stream's region moved on by the frames played, unread
/// (`Follow` streams need nothing: their place is the timeline's).
fn skip(
    stream: &mut CompareStream,
    history: &crate::history::PlayHistory,
    latency: u32,
    n: usize,
) -> Option<i64> {
    let s = stream.settings;
    if stream.source.is_none() || s.sync != CompareSync::Loop || s.to <= s.from {
        return None;
    }
    let mut played = 0i64;
    history.pieces(latency, 0, n, |_, len, p| {
        if p.playing {
            played += len as i64;
        }
    });
    if played == 0 {
        return None;
    }
    let pos = if stream.pos < s.from || stream.pos >= s.to {
        s.from
    } else {
        stream.pos
    };
    stream.pos = s.from + (pos - s.from + played).rem_euclid(s.to - s.from);
    Some(stream.pos)
}

/// Read `n` frames of `stream` into `buf` where the audio arriving
/// `latency` frames late was played (silence while stopped, outside the
/// region or the file). Returns the file frame reached.
fn read(
    stream: &mut CompareStream,
    buf: &mut AudioBuffer,
    tmp: &mut [f32],
    history: &crate::history::PlayHistory,
    latency: u32,
    n: usize,
) -> Option<i64> {
    for c in 0..buf.num_channels() {
        buf.channel_mut(c)[..n].fill(0.0);
    }
    let source = stream.source.as_deref()?;
    let s = stream.settings;
    let mut reached = None;
    let pos = &mut stream.pos;
    history.pieces(latency, 0, n, |o, len, played| {
        if !played.playing {
            return;
        }
        match s.sync {
            CompareSync::Follow { anchor } => {
                let f = played.pos - anchor;
                // The region only.
                let a = f.max(s.from);
                let b = (f + len as i64).min(s.to);
                if b > a {
                    let at = o + (a - f) as usize;
                    copy(source, buf, tmp, a, at, (b - a) as usize);
                }
                reached = Some(f + len as i64);
            }
            CompareSync::Loop => {
                let span = s.to - s.from;
                if span <= 0 {
                    return;
                }
                if *pos < s.from || *pos >= s.to {
                    *pos = s.from;
                }
                let mut done = 0;
                while done < len {
                    let k = ((s.to - *pos) as usize).min(len - done);
                    copy(source, buf, tmp, *pos, o + done, k);
                    *pos += k as i64;
                    if *pos >= s.to {
                        *pos = s.from;
                    }
                    done += k;
                }
                reached = Some(*pos);
            }
        }
    });
    reached
}

/// File frames `[frame, frame + len)` into `buf` from `at`, the file's
/// channels mapped onto the mix's: the same count one to one, a mono file
/// on the first two, a stereo file into a mono mix as `(L + R) / 2`, else
/// as many as both have.
fn copy(
    source: &StreamSource,
    buf: &mut AudioBuffer,
    tmp: &mut [f32],
    frame: i64,
    at: usize,
    len: usize,
) {
    let fc = source.channels().max(1);
    let nc = buf.num_channels();
    let fill = |ch: usize, dst: &mut [f32]| {
        source.read_segments(ch, frame, len, |off, l, seg| {
            if let Some(seg) = seg {
                dst[off..off + l].copy_from_slice(seg);
            }
        });
    };
    if nc == 1 && fc >= 2 {
        let tmp = &mut tmp[..len];
        tmp.fill(0.0);
        fill(1, tmp);
        let dst = &mut buf.channel_mut(0)[at..at + len];
        fill(0, dst);
        for (o, r) in dst.iter_mut().zip(tmp.iter()) {
            *o = 0.5 * (*o + r);
        }
        return;
    }
    for c in 0..nc {
        let ch = if fc == nc {
            Some(c)
        } else if fc == 1 {
            (c < 2).then_some(0)
        } else {
            (c < fc).then_some(c)
        };
        if let Some(ch) = ch {
            fill(ch, &mut buf.channel_mut(c)[at..at + len]);
        }
    }
}
