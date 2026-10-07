//! What the master's way to the device does for listening — never part of
//! a render: headphones (the mix rendered binaurally), a bed folded to the
//! device's outputs, and the mono check (everything summed to both
//! speakers, to hear what a mono playback keeps).

use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_core::ChannelLayout;
use faderframe_core::surround::{self, MAX_SPEAKERS};
use faderframe_realtime::ParamSlot;

/// The master to the device: binaural, folded or as it is, then mono when
/// its switch is on.
pub struct ListenOut {
    binaural: Option<Box<faderframe_binaural::Renderer>>,
    /// Folding the bed into fewer channels (`None`: as it is).
    fold: Option<Box<[[f32; MAX_SPEAKERS]; MAX_SPEAKERS]>>,
    /// The output to stereo, for the mono check (`None`: it is stereo or
    /// mono already).
    to_stereo: Option<Box<[[f32; MAX_SPEAKERS]; MAX_SPEAKERS]>>,
    mono: ParamSlot,
    scratch: Vec<Vec<f32>>,
}

impl ListenOut {
    /// From `input` to `output`; `listen.binaural`: render `input` for
    /// headphones (the output is then stereo) at `rate`; `block`: the
    /// largest call.
    pub fn new(
        input: ChannelLayout,
        output: ChannelLayout,
        listen: &crate::build::Listen,
        rate: u32,
        block: usize,
        mono: ParamSlot,
    ) -> Self {
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
        let matrix = |from, to| {
            let mut m = Box::new([[0.0f32; MAX_SPEAKERS]; MAX_SPEAKERS]);
            surround::matrix(from, to, &faderframe_core::SurroundPan::default(), &mut m)
                .then_some(m)
        };
        let fold = if renderer.is_some() || input == output {
            None
        } else {
            matrix(input, output)
        };
        let to_stereo = (output.channel_count() > 2)
            .then(|| matrix(output, ChannelLayout::Stereo))
            .flatten();
        Self {
            binaural: renderer,
            fold,
            to_stereo,
            mono,
            scratch: vec![vec![0.0; block.max(1)]; 2],
        }
    }
}

impl Processor<EngineContext> for ListenOut {
    fn latency(&self) -> u32 {
        self.binaural.as_ref().map_or(0, |r| r.latency() as u32)
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let n = io.frames;
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        out.clear();
        let outs = out.num_channels();
        if let Some(r) = self.binaural.as_mut() {
            let ins: [&[f32]; MAX_SPEAKERS] = std::array::from_fn(|c| {
                if c < input.num_channels() {
                    &input.channel(c)[..n]
                } else {
                    &[][..]
                }
            });
            let count = input.num_channels().min(MAX_SPEAKERS);
            let (l, rest) = self.scratch.split_at_mut(1);
            r.process(&ins[..count], &mut l[0][..n], &mut rest[0][..n], n);
            for c in 0..outs.min(2) {
                out.channel_mut(c)[..n].copy_from_slice(&self.scratch[c][..n]);
            }
        } else if let Some(m) = self.fold.as_ref() {
            for (s, row) in m.iter().enumerate().take(input.num_channels()) {
                for (d, &g) in row.iter().enumerate().take(outs) {
                    if g != 0.0 {
                        for (o, &x) in out.channel_mut(d)[..n]
                            .iter_mut()
                            .zip(&input.channel(s)[..n])
                        {
                            *o += x * g;
                        }
                    }
                }
            }
        } else {
            for c in 0..outs.min(input.num_channels()) {
                out.channel_mut(c)[..n].copy_from_slice(&input.channel(c)[..n]);
            }
        }
        // The mono check: the stereo picture summed, on both speakers.
        if cx.data.params.get(self.mono) >= 0.5 && outs >= 2 {
            let (l, r) = self.scratch.split_at_mut(1);
            let (l, r) = (&mut l[0][..n], &mut r[0][..n]);
            match self.to_stereo.as_ref() {
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
            for i in 0..n {
                let m = 0.5 * (l[i] + r[i]);
                l[i] = m;
            }
            for c in 0..outs {
                if c < 2 {
                    out.channel_mut(c)[..n].copy_from_slice(l);
                } else {
                    out.channel_mut(c)[..n].fill(0.0);
                }
            }
        }
    }
}
