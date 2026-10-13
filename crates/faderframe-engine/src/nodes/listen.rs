//! What the master's way to the device does for listening — never part of
//! a render: headphones (the mix rendered binaurally), a bed folded to the
//! device's outputs (by the project's downmix), and the mono check
//! (everything summed to both speakers, to hear what a mono playback
//! keeps: a bed by the downmix's stereo fold, then `(L + R) / 2`), then —
//! on the speakers only — bass management (see [`super::bass`]).
//!
//! Switching between speakers and headphones (or to another head, room or
//! correction) needs no rebuild: the controller makes the new
//! [`Listening`] on its thread and hands it over through a [`ListenLink`];
//! the node crossfades to it over [`SWITCH_SECONDS`] (constant power) and
//! hands the old one back to be dropped there. The node keeps its place
//! (and key) across graph rebuilds, so a rebuild never cuts what is heard.

use super::bass::BassManager;
use super::strip::MatrixMix;
use crate::context::EngineContext;
use crate::slots::MixSlots;
use faderframe_audio_graph::{AudioBuffer, NodeIo, ProcessContext, Processor};
use faderframe_core::ChannelLayout;
use faderframe_core::surround::{self, BassManagement, Downmix, MAX_SPEAKERS, Mixing};
use faderframe_realtime::{MailboxReceiver, MailboxSender, ParamSlot, mailbox};
use std::sync::Arc;

/// Gains from each channel to each (a fold).
type Matrix = [[f32; MAX_SPEAKERS]; MAX_SPEAKERS];

/// How long a switch of listening crossfades.
pub const SWITCH_SECONDS: f64 = 0.03;

/// How the master is heard: on the speakers (`None`) or through a
/// binaural renderer.
pub struct Listening {
    renderer: Option<Box<faderframe_binaural::Renderer>>,
}

impl Listening {
    /// The master (`input`) heard as `listen` says at `rate` (a renderer
    /// that cannot be made — a damaged head — leaves it on the speakers).
    pub fn new(input: ChannelLayout, rate: u32, listen: &crate::build::Listen) -> Self {
        let renderer = listen.binaural.and_then(|room| {
            faderframe_binaural::Renderer::new(
                input,
                rate,
                room,
                &listen.head,
                listen.correction.as_deref(),
            )
            .map_err(|e| tracing::warn!("binaural: {e}"))
            .ok()
            .map(Box::new)
        });
        Self { renderer }
    }

    pub fn is_binaural(&self) -> bool {
        self.renderer.is_some()
    }

    fn latency(&self) -> u32 {
        self.renderer.as_ref().map_or(0, |r| r.latency() as u32)
    }
}

/// The way new [`Listening`] reaches the running node and old comes back
/// (one per engine; the node and the controller share it).
pub struct ListenLink {
    to_audio: (MailboxSender<Listening>, MailboxReceiver<Listening>),
    back: (MailboxSender<Listening>, MailboxReceiver<Listening>),
}

impl ListenLink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            to_audio: mailbox(),
            back: mailbox(),
        })
    }

    /// Control thread: hand `listening` over. Drop what comes back (one
    /// the node had not taken yet) here.
    #[must_use = "a replaced listening must be dropped on the control thread"]
    pub fn send(&self, listening: Box<Listening>) -> Option<Box<Listening>> {
        self.to_audio.0.send(listening)
    }

    /// Control thread: what the node retired (drop it here).
    pub fn collect(&self) -> Option<Box<Listening>> {
        self.back.1.take()
    }
}

impl std::fmt::Debug for ListenLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ListenLink")
    }
}

/// The master to the device: binaural, folded or as it is, then mono when
/// its switch is on.
pub struct ListenOut {
    /// What is heard.
    now: Box<Listening>,
    /// What is coming in, crossfading (`faded` frames of `fade_len` done).
    next: Option<Box<Listening>>,
    faded: usize,
    fade_len: usize,
    /// Not a frame played yet: a switch applies at once.
    fresh: bool,
    link: Arc<ListenLink>,
    /// Retired, waiting for the way back to be free (never dropped here).
    retired: Option<Box<Listening>>,
    stages: Stages,
    mono: ParamSlot,
    /// Bass management's settings ([`BassManagement::values`]).
    bass_slots: [ParamSlot; 4],
    shared: MixSlots,
    /// The incoming way's output while crossfading.
    alt: AudioBuffer,
}

/// What a way of listening passes after its render.
struct Stages {
    /// Folding the bed into fewer channels (`None`: as it is).
    fold: Option<Box<MatrixMix>>,
    /// The output to stereo, for the mono check (`None`: it is stereo or
    /// mono already), and the downmix it was made with.
    to_stereo: Option<(ChannelLayout, Box<Matrix>, Downmix)>,
    /// Bass management of the device's layout (`None`: it has no LFE).
    bass: Option<BassManager>,
    scratch: Vec<Vec<f32>>,
}

/// What every way of listening is told for a block.
#[derive(Clone, Copy)]
struct Block {
    downmix: Downmix,
    mono: bool,
    bass: BassManagement,
}

impl ListenOut {
    /// From `input` to `output` (the device's channels; headphones play
    /// on its first two), heard as `listen` says at `rate`; `block`: the
    /// largest call.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input: ChannelLayout,
        output: ChannelLayout,
        listen: &crate::build::Listen,
        rate: u32,
        block: usize,
        mono: ParamSlot,
        bass_slots: [ParamSlot; 4],
        shared: MixSlots,
    ) -> Self {
        let fold = if input == output {
            None
        } else {
            MatrixMix::new(input, output).map(Box::new)
        };
        let to_stereo = (output.channel_count() > 2).then(|| {
            (
                output,
                Box::new([[0.0f32; MAX_SPEAKERS]; MAX_SPEAKERS]),
                // Made on first use.
                Downmix {
                    centre: f32::NAN,
                    ..Downmix::STANDARD
                },
            )
        });
        let block = block.max(1);
        let mut alt = AudioBuffer::new(output, block);
        alt.set_len(block);
        Self {
            now: Box::new(Listening::new(input, rate, listen)),
            next: None,
            faded: 0,
            fade_len: ((f64::from(rate) * SWITCH_SECONDS) as usize).max(1),
            fresh: true,
            link: Arc::clone(&listen.link),
            retired: None,
            stages: Stages {
                fold,
                to_stereo,
                bass: BassManager::new(output, rate, block),
                scratch: vec![vec![0.0; block]; 2],
            },
            mono,
            bass_slots,
            shared,
            alt,
        }
    }

    /// `input` heard by `listening` into `out` (cleared first): rendered
    /// or folded, then the mono check, then (on the speakers) bass
    /// management.
    fn hear(
        listening: &mut Listening,
        stages: &mut Stages,
        block: Block,
        input: &AudioBuffer,
        out: &mut AudioBuffer,
        n: usize,
    ) {
        out.clear();
        let outs = out.num_channels();
        let scratch = &mut stages.scratch;
        if let Some(r) = listening.renderer.as_mut() {
            let ins: [&[f32]; MAX_SPEAKERS] = std::array::from_fn(|c| {
                if c < input.num_channels() {
                    &input.channel(c)[..n]
                } else {
                    &[][..]
                }
            });
            let count = input.num_channels().min(MAX_SPEAKERS);
            let (l, rest) = scratch.split_at_mut(1);
            r.process(&ins[..count], &mut l[0][..n], &mut rest[0][..n], n);
            if outs == 1 {
                // A mono device: both ears summed.
                let (l, r) = (&scratch[0][..n], &scratch[1][..n]);
                for ((o, a), b) in out.channel_mut(0)[..n].iter_mut().zip(l).zip(r) {
                    *o = 0.5 * (a + b);
                }
            } else {
                for (c, ear) in scratch.iter().enumerate().take(2) {
                    out.channel_mut(c)[..n].copy_from_slice(&ear[..n]);
                }
            }
        } else if let Some(m) = stages.fold.as_mut() {
            m.pan(&faderframe_core::SurroundPan::default(), 0.0, block.downmix);
            m.mix(input, out, 0, n, 1.0);
        } else {
            for c in 0..outs.min(input.num_channels()) {
                out.channel_mut(c)[..n].copy_from_slice(&input.channel(c)[..n]);
            }
        }
        if block.mono && outs >= 2 {
            Self::mono_check(stages, block.downmix, out, n);
        }
        if listening.renderer.is_none()
            && let Some(b) = stages.bass.as_mut()
        {
            b.process(block.bass, out, n);
        }
    }

    /// What is heard folded to stereo and summed to `(L + R) / 2` on both
    /// speakers.
    fn mono_check(stages: &mut Stages, downmix: Downmix, out: &mut AudioBuffer, n: usize) {
        let outs = out.num_channels();
        let (l, r) = stages.scratch.split_at_mut(1);
        let (l, r) = (&mut l[0][..n], &mut r[0][..n]);
        if let Some((from, m, made)) = stages.to_stereo.as_mut()
            && *made != downmix
        {
            let mixing = Mixing {
                downmix,
                ..Mixing::default()
            };
            let pan = faderframe_core::SurroundPan::default();
            surround::matrix(*from, ChannelLayout::Stereo, &pan, &mixing, m);
            *made = downmix;
        }
        match stages.to_stereo.as_ref().map(|(_, m, _)| m) {
            Some(m) => {
                l.fill(0.0);
                r.fill(0.0);
                for (s, row) in m.iter().enumerate().take(outs) {
                    let x = &out.channel(s)[..n];
                    for (side, buf) in [(0, &mut *l), (1, &mut *r)] {
                        let g = row[side];
                        if g != 0.0 {
                            for (o, &v) in buf.iter_mut().zip(x) {
                                *o += v * g;
                            }
                        }
                    }
                }
            }
            None => {
                l.copy_from_slice(&out.channel(0)[..n]);
                r.copy_from_slice(&out.channel(1)[..n]);
            }
        }
        for (a, b) in l.iter_mut().zip(r.iter()) {
            *a = 0.5 * (*a + b);
        }
        for c in 0..outs {
            if c < 2 {
                out.channel_mut(c)[..n].copy_from_slice(l);
            } else {
                out.channel_mut(c)[..n].fill(0.0);
            }
        }
    }

    /// Hand back what was retired, as soon as the way back is free.
    fn give_back(&mut self) {
        if let Some(old) = self.retired.take() {
            // Whatever the control side has not collected yet comes out
            // of the slot: kept for the next try, never dropped here.
            self.retired = self.link.back.0.send(old);
        }
    }
}

impl Processor<EngineContext> for ListenOut {
    fn latency(&self) -> u32 {
        self.now.latency()
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let n = io.frames;
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        self.give_back();
        // A new way of listening (one at a time, and only with the way
        // back free).
        if self.next.is_none()
            && self.retired.is_none()
            && let Some(new) = self.link.to_audio.1.take()
        {
            if self.fresh {
                // Nothing heard yet: no crossfade.
                self.retired = Some(std::mem::replace(&mut self.now, new));
                self.give_back();
            } else {
                self.next = Some(new);
                self.faded = 0;
            }
        }
        self.fresh = false;
        let params = &cx.data.params;
        let block = Block {
            downmix: self.shared.downmix(params),
            mono: params.get(self.mono) >= 0.5,
            bass: BassManagement::from_values(&self.bass_slots.map(|s| params.get(s))),
        };
        Self::hear(&mut self.now, &mut self.stages, block, input, out, n);
        if let Some(next) = self.next.as_mut() {
            Self::hear(next, &mut self.stages, block, input, &mut self.alt, n);
            // Constant power: the two ways are different signals.
            let len = self.fade_len as f32;
            for c in 0..out.num_channels() {
                let to = &self.alt.channel(c)[..n];
                for (i, (o, &b)) in out.channel_mut(c)[..n].iter_mut().zip(to).enumerate() {
                    let t =
                        ((self.faded + i + 1) as f32 / len).min(1.0) * std::f32::consts::FRAC_PI_2;
                    *o = *o * t.cos() + b * t.sin();
                }
            }
            self.faded += n;
            if self.faded >= self.fade_len
                && let Some(next) = self.next.take()
            {
                self.retired = Some(std::mem::replace(&mut self.now, next));
                self.give_back();
            }
        }
    }
}
