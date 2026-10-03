//! Generic, context-independent processors.

use crate::{NodeIo, PrepareConfig, ProcessContext, Processor};

/// Copies (with channel conversion) input `i` to output `i`; extra outputs
/// are silenced. Also used as a stand-in for missing/unavailable plugins.
#[derive(Debug, Default, Clone, Copy)]
pub struct Passthrough;

impl<C> Processor<C> for Passthrough {
    fn process(&mut self, _cx: &ProcessContext<'_, C>, io: &mut NodeIo<'_>) {
        for (i, out) in io.audio_out.iter_mut().enumerate() {
            match io.audio_in.get(i) {
                Some(input) => out.copy_from(input),
                None => out.clear(),
            }
        }
        if let (Some(src), Some(dst)) = (io.events_in.first(), io.events_out.first_mut()) {
            dst.merge_from(src, 0);
        }
    }
}

/// Fixed linear gain from input 0 to output 0.
#[derive(Debug, Clone, Copy)]
pub struct Gain {
    pub gain: f32,
}

impl<C> Processor<C> for Gain {
    fn process(&mut self, _cx: &ProcessContext<'_, C>, io: &mut NodeIo<'_>) {
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        out.copy_from(input);
        for c in 0..out.num_channels() {
            for s in out.channel_mut(c) {
                *s *= self.gain;
            }
        }
    }
}

/// Writes a constant value to every output channel (tests, calibration).
#[derive(Debug, Clone, Copy)]
pub struct Constant {
    pub value: f32,
}

impl<C> Processor<C> for Constant {
    fn process(&mut self, _cx: &ProcessContext<'_, C>, io: &mut NodeIo<'_>) {
        for out in io.audio_out.iter_mut() {
            for c in 0..out.num_channels() {
                out.channel_mut(c).fill(self.value);
            }
        }
    }
}

/// Silence on all outputs; a placeholder sink/source.
#[derive(Debug, Default, Clone, Copy)]
pub struct Silence;

impl<C> Processor<C> for Silence {
    fn process(&mut self, _cx: &ProcessContext<'_, C>, io: &mut NodeIo<'_>) {
        for out in io.audio_out.iter_mut() {
            out.clear();
        }
    }
}

/// Delays input 0 by a fixed amount *and reports it as latency* — behaves
/// like a look-ahead plugin. Used to exercise delay compensation.
#[derive(Debug)]
pub struct LatencyProbe {
    samples: usize,
    ring: Vec<Vec<f32>>,
    pos: usize,
}

impl LatencyProbe {
    pub fn new(samples: u32) -> Self {
        Self {
            samples: samples as usize,
            ring: Vec::new(),
            pos: 0,
        }
    }
}

impl<C> Processor<C> for LatencyProbe {
    fn prepare(&mut self, _config: &PrepareConfig) {
        // Allocated lazily per channel count on first prepare; 8 channels
        // covers every layout used in tests.
        let len = self.samples.max(1);
        self.ring = (0..8).map(|_| vec![0.0; len]).collect();
        self.pos = 0;
    }

    fn latency(&self) -> u32 {
        self.samples as u32
    }

    fn process(&mut self, _cx: &ProcessContext<'_, C>, io: &mut NodeIo<'_>) {
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        if self.samples == 0 {
            out.copy_from(input);
            return;
        }
        let len = self.samples;
        let channels = out
            .num_channels()
            .min(input.num_channels())
            .min(self.ring.len());
        let mut pos = self.pos;
        for c in 0..channels {
            pos = self.pos;
            let ring = &mut self.ring[c];
            let inp = input.channel(c);
            let o = out.channel_mut(c);
            for (o, &i) in o.iter_mut().zip(inp) {
                *o = ring[pos];
                ring[pos] = i;
                pos += 1;
                if pos == len {
                    pos = 0;
                }
            }
        }
        for c in channels..out.num_channels() {
            out.channel_mut(c).fill(0.0);
        }
        self.pos = pos;
    }

    fn reset(&mut self) {
        for r in &mut self.ring {
            r.fill(0.0);
        }
    }
}
