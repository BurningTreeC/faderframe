//! The codec monitor in the monitor path: the master heard as a listener
//! gets it — coded and decoded through Opus at a bitrate, at a streaming
//! platform's loudness — or the original beside it, or the difference of
//! the two (what the codec removed or added, alone).
//!
//! It lives in the master's listening node ([`super::ListenOut`]), after
//! the compare source (a reference or the past is heard through it too)
//! and before headphones, the mono check and bass management: it emulates
//! the delivered master, which listeners then hear on whatever they have.
//! Monitoring only — renders build their own engine; meters and analysers
//! show the mix (the master strip before it meters).
//!
//! * **Aligned.** The chain ([`faderframe_codec::CodecChain`]) delays the
//!   decoded signal and the original by exactly the same whole number of
//!   frames (the codec's frame and lookahead, the resamplers when the
//!   project is not at 48 kHz), so Codec, Original and Difference switch
//!   with a 20 ms crossfade and nothing moves in time.
//! * **The delay is listening's.** While the monitor is heard, what comes
//!   out is [`CodecShared::delay`] frames later than the graph made it (26.5
//!   ms at 48 kHz). The graph's latency does not change (no rebuild, no
//!   plugin delay compensation for it): like varispeed's resampler it is
//!   added to what the heard clock counts after the graph
//!   (`EngineController::listening_delay`), so the playhead, the meters and
//!   the analysers stay with what is heard. Switching the monitor on plays
//!   the last 26 ms again (crossfaded, once the chain is warm); switching
//!   it off skips them.
//! * **Realtime.** Chains are made on the control thread
//!   ([`CodecLink::poll`] for the format the running node publishes) and
//!   handed in through a mailbox; retired ones go back through another and
//!   are dropped there. Bitrate, VBR, the platform's gain and what is heard
//!   are atomics, applied without a new chain (libopus' `ctl`s allocate
//!   nothing). The measurements ([`faderframe_codec::CodecMeter`]: true
//!   peak after decoding at the platform's gain, frames over full scale,
//!   the energy of the difference against the original's) are published
//!   every block.

use crate::context::EngineContext;
use faderframe_audio_graph::{AudioBuffer, ProcessContext};
use faderframe_codec::{CHUNK, CodecChain, CodecError, CodecMeter, CodecSettings, CoderKind};
use faderframe_core::ChannelLayout;
use faderframe_realtime::{MailboxReceiver, MailboxSender, mailbox};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// How long a switch (on, off, between what is heard) crossfades.
pub const FADE_SECONDS: f64 = 0.02;

/// Frames after a chain's start left out of the measurements: the
/// resamplers' ringing at the abrupt start of what they were fed.
const SETTLE: usize = 4096;

/// What the codec monitor plays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodecHear {
    /// The decoded signal (at the platform's gain).
    #[default]
    Codec,
    /// The original, delayed alike (at the platform's gain): a lossless
    /// tier, or the reference for the codec.
    Original,
    /// The original less the decoded: what the codec removed or added.
    Difference,
}

impl CodecHear {
    pub const ALL: [CodecHear; 3] = [CodecHear::Codec, CodecHear::Original, CodecHear::Difference];

    pub fn id(self) -> &'static str {
        match self {
            CodecHear::Codec => "codec",
            CodecHear::Original => "original",
            CodecHear::Difference => "difference",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            CodecHear::Codec => "Codec",
            CodecHear::Original => "Original",
            CodecHear::Difference => "Difference",
        }
    }

    fn index(self) -> usize {
        match self {
            CodecHear::Codec => 0,
            CodecHear::Original => 1,
            CodecHear::Difference => 2,
        }
    }

    fn from_index(i: u8) -> Self {
        match i {
            1 => CodecHear::Original,
            2 => CodecHear::Difference,
            _ => CodecHear::Codec,
        }
    }
}

/// The monitor's controls and what the node reports (atomics shared by
/// the control side and the audio thread).
#[derive(Debug)]
pub struct CodecShared {
    on: AtomicBool,
    hear: AtomicU8,
    kbps: AtomicU32,
    vbr: AtomicBool,
    /// The platform's gain (f32 bits, linear).
    gain: AtomicU32,
    /// Bumped to start the measurements anew.
    reset: AtomicU64,
    // --- reported by the node -------------------------------------------
    /// The running node's channels and rate (0: no node has run).
    channels: AtomicU32,
    rate: AtomicU32,
    /// The generation of the chain the node holds (0: none) and of the
    /// last one it took out of the mailbox (installed or refused).
    installed: AtomicU64,
    taken: AtomicU64,
    /// Frames the node adds to what is heard now.
    delay: AtomicU32,
    /// How much of the monitor is heard (f32 bits; 0 the mix as it is).
    heard: AtomicU32,
    /// Measurements since the last reset: highest true peak (f32 bits,
    /// linear), frames over full scale, energies (f64 bits), frames.
    peak: AtomicU32,
    overs: AtomicU64,
    difference: AtomicU64,
    original: AtomicU64,
    frames: AtomicU64,
    failures: AtomicU64,
    /// The reset the measurements belong to.
    measured: AtomicU64,
}

impl Default for CodecShared {
    fn default() -> Self {
        Self {
            on: AtomicBool::new(false),
            hear: AtomicU8::new(0),
            kbps: AtomicU32::new(128),
            vbr: AtomicBool::new(true),
            gain: AtomicU32::new(1.0f32.to_bits()),
            reset: AtomicU64::new(0),
            channels: AtomicU32::new(0),
            rate: AtomicU32::new(0),
            installed: AtomicU64::new(0),
            taken: AtomicU64::new(0),
            delay: AtomicU32::new(0),
            heard: AtomicU32::new(0),
            peak: AtomicU32::new(0),
            overs: AtomicU64::new(0),
            difference: AtomicU64::new(0),
            original: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            measured: AtomicU64::new(0),
        }
    }
}

/// What the node measured since the measurements were last reset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CodecReading {
    /// Highest true peak after decoding at the platform's gain (linear).
    pub peak: f32,
    /// Frames over full scale (0 dBTP).
    pub overs: u64,
    /// Energies of the difference and of the original.
    pub difference: f64,
    pub original: f64,
    pub frames: u64,
    /// Frames the codec could not code.
    pub failures: u64,
}

impl CodecReading {
    /// The highest true peak (dBTP).
    pub fn peak_dbtp(&self) -> f64 {
        faderframe_codec::meter::db(f64::from(self.peak))
    }

    /// What the codec removed or added against the original (dB).
    pub fn difference_db(&self) -> Option<f64> {
        faderframe_codec::meter::relative_db(self.difference, self.original)
    }
}

impl CodecShared {
    /// Is the monitor asked to be heard?
    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn set_hear(&self, hear: CodecHear) {
        self.hear.store(hear.index() as u8, Ordering::Relaxed);
    }

    pub fn hear(&self) -> CodecHear {
        CodecHear::from_index(self.hear.load(Ordering::Relaxed))
    }

    /// The platform's gain (linear); nonsense is unity, kept within ±24 dB.
    pub fn set_gain(&self, gain: f32) {
        let g = if gain.is_finite() {
            gain.clamp(0.063, 15.85)
        } else {
            1.0
        };
        self.gain.store(g.to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    /// Bitrate (kb/s a stereo pair) and VBR, from the chain's next frame.
    pub fn set_codec(&self, kbps: u32, vbr: bool) {
        self.kbps
            .store(faderframe_codec::clamp_kbps(kbps), Ordering::Relaxed);
        self.vbr.store(vbr, Ordering::Relaxed);
    }

    /// Start the measurements anew.
    pub fn reset_measurements(&self) {
        self.reset.fetch_add(1, Ordering::Relaxed);
    }

    /// Frames the monitor adds to what is heard now (0 while the mix is
    /// heard as it is).
    pub fn delay(&self) -> u32 {
        self.delay.load(Ordering::Relaxed)
    }

    /// How much of the monitor is heard (0: the mix as it is, 1: the
    /// monitor; between: crossfading or warming up).
    pub fn heard(&self) -> f32 {
        f32::from_bits(self.heard.load(Ordering::Relaxed))
    }

    /// The node holds a chain.
    pub fn installed(&self) -> bool {
        self.installed.load(Ordering::Relaxed) != 0
    }

    /// The measurements (empty until the node measured after the last
    /// reset).
    pub fn reading(&self) -> CodecReading {
        if self.measured.load(Ordering::Relaxed) != self.reset.load(Ordering::Relaxed) {
            return CodecReading::default();
        }
        CodecReading {
            peak: f32::from_bits(self.peak.load(Ordering::Relaxed)),
            overs: self.overs.load(Ordering::Relaxed),
            difference: f64::from_bits(self.difference.load(Ordering::Relaxed)),
            original: f64::from_bits(self.original.load(Ordering::Relaxed)),
            frames: self.frames.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
        }
    }
}

/// What the control side wants of the monitor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CodecWanted {
    pub settings: CodecSettings,
    pub hear: CodecHear,
    /// The platform's gain (dB; 0 for none).
    pub gain_db: f32,
}

/// A chain on its way to the node (and back).
pub struct Monitor {
    chain: CodecChain,
    meter: CodecMeter,
    generation: u64,
}

struct Wanted {
    on: Option<CodecWanted>,
    /// The master's layout and rate as the newest listening node was built
    /// for.
    layout: Option<(ChannelLayout, u32)>,
    /// The last chain made: its generation and coder.
    made: Option<(u64, CoderKind)>,
    next_generation: u64,
    error: Option<String>,
}

/// The way a chain reaches the running listening node and comes back (one
/// per engine, inside its [`super::ListenLink`]).
pub struct CodecLink {
    wanted: Mutex<Wanted>,
    to_audio: (MailboxSender<Monitor>, MailboxReceiver<Monitor>),
    back: (MailboxSender<Monitor>, MailboxReceiver<Monitor>),
    shared: Arc<CodecShared>,
}

impl std::fmt::Debug for CodecLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CodecLink")
    }
}

impl CodecLink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            wanted: Mutex::new(Wanted {
                on: None,
                layout: None,
                made: None,
                next_generation: 1,
                error: None,
            }),
            to_audio: mailbox(),
            back: mailbox(),
            shared: Arc::new(CodecShared::default()),
        })
    }

    /// The controls and what the node reports.
    pub fn shared(&self) -> &Arc<CodecShared> {
        &self.shared
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Wanted> {
        self.wanted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Control thread: monitor through `wanted` (`None`: the mix as it
    /// is). Settings reach the running node at once; a chain is made when
    /// the node has none ([`Self::poll`] does, at once here).
    pub fn set(&self, wanted: Option<CodecWanted>) -> Result<(), CodecError> {
        {
            let mut w = self.lock();
            let s = &self.shared;
            match wanted {
                Some(c) => {
                    s.set_codec(c.settings.kbps, c.settings.vbr);
                    s.set_hear(c.hear);
                    s.set_gain(faderframe_core::db_to_gain(c.gain_db));
                    s.on.store(true, Ordering::Relaxed);
                    if w.on.is_none_or(|o| o.settings != c.settings) {
                        // New settings start new measurements.
                        s.reset_measurements();
                    }
                }
                None => s.on.store(false, Ordering::Relaxed),
            }
            w.on = wanted;
        }
        self.poll()
    }

    /// What is wanted now.
    pub fn wanted(&self) -> Option<CodecWanted> {
        self.lock().on
    }

    /// The last chain that could not be made.
    pub fn error(&self) -> Option<String> {
        self.lock().error.clone()
    }

    /// Control thread (listening node built for the master): the layout
    /// and rate chains are made for.
    pub(crate) fn note_layout(&self, layout: ChannelLayout, rate: u32) {
        self.lock().layout = Some((layout, rate));
    }

    /// Control thread, every tick: drop what came back; make and hand over
    /// a chain when the running node needs one (none yet, or another
    /// coder wanted).
    pub fn poll(&self) -> Result<(), CodecError> {
        self.collect();
        let mut w = self.lock();
        let Some(on) = w.on else {
            return Ok(());
        };
        let s = &self.shared;
        let channels = s.channels.load(Ordering::Relaxed) as usize;
        let rate = s.rate.load(Ordering::Relaxed);
        let Some((layout, built_rate)) = w.layout else {
            return Ok(());
        };
        // No node running yet: the chain would be made for nothing.
        if channels == 0 || channels != layout.channel_count() || rate != built_rate {
            return Ok(());
        }
        let taken = s.taken.load(Ordering::Relaxed);
        let installed = s.installed.load(Ordering::Relaxed);
        if w.made.is_some_and(|(generation, _)| generation > taken) {
            // On its way.
            return Ok(());
        }
        // Another coder wanted than the one the node holds.
        let other_kind = w
            .made
            .is_some_and(|(generation, kind)| generation == installed && kind != on.settings.kind);
        if installed != 0 && !other_kind {
            return Ok(());
        }
        let groups = faderframe_codec::substreams(layout);
        let chain = match CodecChain::new(channels, rate, &groups, on.settings) {
            Ok(c) => c,
            Err(e) => {
                w.error = Some(e.to_string());
                return Err(e);
            }
        };
        w.error = None;
        let generation = w.next_generation;
        w.next_generation += 1;
        w.made = Some((generation, on.settings.kind));
        let monitor = Monitor {
            chain,
            meter: CodecMeter::new(channels),
            generation,
        };
        // One the node had not taken yet comes back: dropped here.
        drop(self.to_audio.0.send(Box::new(monitor)));
        Ok(())
    }

    /// Control thread: drop what the node retired.
    pub fn collect(&self) {
        while self.back.1.take().is_some() {}
    }
}

/// The codec monitor inside the listening node.
pub(crate) struct Codec {
    link: Arc<CodecLink>,
    shared: Arc<CodecShared>,
    channels: usize,
    rate: u32,
    now: Option<Box<Monitor>>,
    /// Coming in while another is heard (swapped once it faded out).
    next: Option<Box<Monitor>>,
    retired: Option<Box<Monitor>>,
    /// Frames fed into the chain held (its output is the signal once it
    /// reaches the chain's delay; counted up to [`SETTLE`] beyond).
    warm: usize,
    /// 0: the mix as it is, 1: the monitor.
    x: f32,
    /// The weights of codec, original and difference.
    weights: [f32; 3],
    gain: f32,
    fade_len: usize,
    reset_seen: u64,
    /// The monitor's output (a block).
    out: AudioBuffer,
}

impl Codec {
    /// For a master of `layout` at `rate`, blocks up to `block` (control
    /// thread).
    pub(crate) fn new(
        link: &Arc<CodecLink>,
        layout: ChannelLayout,
        rate: u32,
        block: usize,
    ) -> Self {
        link.note_layout(layout, rate);
        let block = block.max(1);
        let mut out = AudioBuffer::new(layout, block);
        out.set_len(block);
        let shared = Arc::clone(&link.shared);
        Self {
            link: Arc::clone(link),
            gain: shared.gain(),
            reset_seen: shared.reset.load(Ordering::Relaxed),
            weights: weights_for(shared.hear()),
            shared,
            channels: layout.channel_count(),
            rate,
            now: None,
            next: None,
            retired: None,
            warm: 0,
            x: 0.0,
            fade_len: ((f64::from(rate) * FADE_SECONDS) as usize).max(1),
            out,
        }
    }

    fn give_back(&mut self) {
        if let Some(old) = self.retired.take() {
            self.retired = self.link.back.0.send(old);
        }
    }

    fn retire(&mut self, old: Box<Monitor>) {
        debug_assert!(self.retired.is_none());
        self.retired = Some(old);
        self.give_back();
    }

    fn take_incoming(&mut self) {
        if self.next.is_some() || self.retired.is_some() {
            return;
        }
        let Some(new) = self.link.to_audio.1.take() else {
            return;
        };
        self.shared.taken.store(new.generation, Ordering::Relaxed);
        if new.chain.channels() != self.channels || new.chain.rate() != self.rate {
            // Made for another format (a rebuild crossed it).
            self.retire(new);
            return;
        }
        if self.now.is_none() {
            self.now = Some(new);
            self.warm = 0;
        } else {
            self.next = Some(new);
        }
    }

    /// Audio thread: what is heard for `input` (the master, or what is
    /// compared with it): `input` itself while the monitor is off.
    pub(crate) fn process<'b>(
        &'b mut self,
        _cx: &ProcessContext<'_, EngineContext>,
        input: &'b AudioBuffer,
        n: usize,
    ) -> &'b AudioBuffer {
        self.give_back();
        self.take_incoming();
        let shared = Arc::clone(&self.shared);
        shared
            .channels
            .store(self.channels as u32, Ordering::Relaxed);
        shared.rate.store(self.rate, Ordering::Relaxed);
        let on = shared.on();
        if self.now.is_none()
            && let Some(next) = self.next.take()
        {
            self.now = Some(next);
            self.warm = 0;
        }
        // Off (or another chain waiting) and faded out: the chain goes back.
        if self.x <= 0.0 && self.retired.is_none() && (!on || self.next.is_some()) {
            if let Some(old) = self.now.take() {
                self.retire(old);
            }
            self.now = self.next.take();
            self.warm = 0;
        }
        shared.installed.store(
            self.now.as_ref().map_or(0, |m| m.generation),
            Ordering::Relaxed,
        );
        let n = n.min(self.out.capacity());
        let channels = self.channels;
        let Some(monitor) = self
            .now
            .as_mut()
            .filter(|_| input.num_channels() >= channels)
        else {
            shared.delay.store(0, Ordering::Relaxed);
            shared.heard.store(0.0f32.to_bits(), Ordering::Relaxed);
            return input;
        };
        let chain = &mut monitor.chain;
        let delay = chain.delay();
        chain.set_settings(CodecSettings {
            kind: chain.settings().kind,
            kbps: shared.kbps.load(Ordering::Relaxed),
            vbr: shared.vbr.load(Ordering::Relaxed),
        });
        let reset = shared.reset.load(Ordering::Relaxed);
        if reset != self.reset_seen {
            monitor.meter.reset();
            self.reset_seen = reset;
        }
        let target_weights = weights_for(shared.hear());
        let goal_gain = shared.gain();
        let fade = 1.0 / self.fade_len as f32;
        let gain_step = (goal_gain - self.gain) / n.max(1) as f32;
        let mut heard_any = self.x > 0.0;
        let mut done = 0;
        while done < n {
            let k = (n - done).min(CHUNK);
            chain.process(k, |c| &input.channel(c)[done..done + k]);
            // Frames of this piece that are the chain's start, not signal;
            // it is heard only from a piece that is signal throughout, and
            // measured once its start has settled.
            let ready = self.warm >= delay;
            let skip = (delay + SETTLE).saturating_sub(self.warm).min(k);
            self.warm = (self.warm + k).min(delay + SETTLE);
            let target = if on && ready && self.next.is_none() {
                1.0
            } else {
                0.0
            };
            heard_any |= target > 0.0;
            let (x0, w0, g0) = (self.x, self.weights, self.gain);
            for c in 0..self.out.num_channels() {
                let out = &mut self.out.channel_mut(c)[done..done + k];
                if c >= channels {
                    out.fill(0.0);
                    continue;
                }
                let (mut x, mut w, mut g) = (x0, w0, g0);
                let (wet, dry) = (chain.wet(c), chain.dry(c));
                let mix = &input.channel(c)[done..done + k];
                for i in 0..k {
                    x = step_to(x, target, fade);
                    for (wv, tv) in w.iter_mut().zip(target_weights) {
                        *wv = step_to(*wv, tv, fade);
                    }
                    g += gain_step;
                    let (a, b) = (wet[i], dry[i]);
                    let monitored = g * (w[0] * a + w[1] * b + w[2] * (b - a));
                    let (wm, wn) = equal_power(x);
                    out[i] = mix[i] * wm + monitored * wn;
                }
            }
            let fk = fade * k as f32;
            self.x = step_to(x0, target, fk);
            for (wv, tv) in self.weights.iter_mut().zip(target_weights) {
                *wv = step_to(*wv, tv, fk);
            }
            let g1 = g0 + gain_step * k as f32;
            if skip < k {
                monitor.meter.add(
                    k - skip,
                    |c| &chain.wet(c)[skip..],
                    |c| &chain.dry(c)[skip..],
                    g0 + gain_step * skip as f32,
                    g1,
                );
            }
            self.gain = g1;
            done += k;
        }
        self.gain = goal_gain;
        // Report.
        let meter = &monitor.meter;
        shared
            .peak
            .store((meter.peak as f32).to_bits(), Ordering::Relaxed);
        shared.overs.store(meter.overs, Ordering::Relaxed);
        shared
            .difference
            .store(meter.difference.to_bits(), Ordering::Relaxed);
        shared
            .original
            .store(meter.original.to_bits(), Ordering::Relaxed);
        shared.frames.store(meter.frames, Ordering::Relaxed);
        shared.failures.store(chain.failures(), Ordering::Relaxed);
        shared.measured.store(self.reset_seen, Ordering::Relaxed);
        shared.delay.store(
            if self.x >= 0.5 { delay as u32 } else { 0 },
            Ordering::Relaxed,
        );
        shared.heard.store(self.x.to_bits(), Ordering::Relaxed);
        if !heard_any {
            // Nothing of the monitor heard (warming up): the mix exactly
            // as it came.
            return input;
        }
        &self.out
    }
}

/// The weights of codec, original and difference for `hear`.
fn weights_for(hear: CodecHear) -> [f32; 3] {
    let mut w = [0.0; 3];
    w[hear.index()] = 1.0;
    w
}

#[inline]
fn step_to(v: f32, target: f32, step: f32) -> f32 {
    if v < target {
        (v + step).min(target)
    } else {
        (v - step).max(target)
    }
}

/// The mix's and the monitor's weights at `x` (equal power: they are a
/// delay apart; exact at either end).
#[inline]
fn equal_power(x: f32) -> (f32, f32) {
    if x <= 0.0 {
        (1.0, 0.0)
    } else if x >= 1.0 {
        (0.0, 1.0)
    } else {
        let t = x * std::f32::consts::FRAC_PI_2;
        (t.cos(), t.sin())
    }
}
