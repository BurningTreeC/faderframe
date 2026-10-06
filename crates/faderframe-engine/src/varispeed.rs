//! Varispeed: the engine plays a little faster or slower than the device
//! (following an external clock without a shared word clock, see
//! `session::sync`).
//!
//! Each callback renders as many engine frames as the device's frames need
//! at the speed (`m ≈ n · speed`), the transport advancing by those; the
//! outputs are resampled from the engine's frames to the device's and the
//! inputs the other way. Both go through FIFOs read at fractional positions
//! with an 8-tap Lanczos kernel (a table of 256 phases), which adds four
//! frames of latency each way. Everything is allocated on the control
//! thread for the stream's channels and buffer size; the audio thread only
//! moves samples.

use faderframe_audio::{DeviceBuffers, OwnedBuffers};

const TAPS: usize = 8;
/// Taps before the read position (the rest are after it).
const BEFORE: usize = 3;
const PHASES: usize = 256;

/// The varispeed state (see the module docs).
pub struct Varispeed {
    kernel: Box<[[f32; TAPS]]>,
    /// Device input frames waiting to be read at engine speed.
    inputs: Fifo,
    /// Engine output frames waiting to be read at device speed.
    outputs: Fifo,
    /// The engine's side of one callback.
    engine: OwnedBuffers,
}

struct Fifo {
    data: Vec<Vec<f32>>,
    len: usize,
    /// Read position (frames into `data`).
    pos: f64,
}

impl Fifo {
    fn new(channels: usize, capacity: usize) -> Self {
        let mut f = Self {
            data: vec![vec![0.0; capacity]; channels],
            len: 0,
            pos: 0.0,
        };
        f.reset();
        f
    }

    /// Empty, but for the taps before the first read (silence).
    fn reset(&mut self) {
        for c in &mut self.data {
            c.fill(0.0);
        }
        self.len = TAPS;
        self.pos = BEFORE as f64;
    }

    fn capacity(&self) -> usize {
        self.data.first().map_or(0, Vec::len)
    }

    /// Frames that can be read from `pos` with all their taps.
    fn readable(&self, step: f64) -> usize {
        let last = self.len as f64 - (TAPS - BEFORE) as f64;
        if last < self.pos {
            return 0;
        }
        ((last - self.pos) / step).floor() as usize + 1
    }

    fn push<'a>(&mut self, frames: usize, mut channel: impl FnMut(usize) -> &'a [f32]) {
        let room = self.capacity().saturating_sub(self.len);
        let n = frames.min(room);
        for (c, d) in self.data.iter_mut().enumerate() {
            let src = channel(c);
            let k = n.min(src.len());
            d[self.len..self.len + k].copy_from_slice(&src[..k]);
            d[self.len + k..self.len + n].fill(0.0);
        }
        self.len += n;
    }

    /// Read `out.len()` frames per channel advancing by `step`; then drop
    /// what no later read needs.
    fn read(
        &mut self,
        kernel: &[[f32; TAPS]],
        step: f64,
        frames: usize,
        mut out: impl FnMut(usize, usize, f32),
    ) {
        for (c, d) in self.data.iter().enumerate() {
            let mut p = self.pos;
            for j in 0..frames {
                let i = p.floor();
                let phase = ((p - i) * PHASES as f64).round() as usize;
                let (i, phase) = if phase == PHASES {
                    (i as usize + 1, 0)
                } else {
                    (i as usize, phase)
                };
                let taps = &kernel[phase];
                let start = i.saturating_sub(BEFORE);
                let mut acc = 0.0f32;
                if start + TAPS <= self.len {
                    for (k, w) in taps.iter().enumerate() {
                        acc += d[start + k] * w;
                    }
                }
                out(c, j, acc);
                p += step;
            }
        }
        self.pos += frames as f64 * step;
        // Keep the taps before the next read.
        let keep_from = (self.pos.floor() as usize)
            .saturating_sub(BEFORE)
            .min(self.len);
        if keep_from > 0 {
            for d in &mut self.data {
                d.copy_within(keep_from..self.len, 0);
            }
            self.len -= keep_from;
            self.pos -= keep_from as f64;
        }
    }
}

/// The Lanczos (a = 4) taps for each of `PHASES` fractional positions,
/// each set summing to one.
fn kernel() -> Box<[[f32; TAPS]]> {
    let sinc = |x: f64| {
        if x.abs() < 1e-12 {
            1.0
        } else {
            let px = std::f64::consts::PI * x;
            px.sin() / px
        }
    };
    (0..=PHASES)
        .map(|p| {
            let f = p as f64 / PHASES as f64;
            let mut w = [0.0f64; TAPS];
            for (k, w) in w.iter_mut().enumerate() {
                let x = k as f64 - BEFORE as f64 - f;
                *w = if x.abs() < 4.0 {
                    sinc(x) * sinc(x / 4.0)
                } else {
                    0.0
                };
            }
            let sum: f64 = w.iter().sum();
            let mut out = [0.0f32; TAPS];
            for (o, w) in out.iter_mut().zip(w) {
                *o = (w / sum) as f32;
            }
            out
        })
        .collect()
}

impl Varispeed {
    /// For a stream of `inputs`/`outputs` channels and callbacks of up to
    /// `max_frames` (allocates; control thread). Speeds stay within
    /// ±[`MAX_DEVIATION`] of 1.
    pub fn new(inputs: usize, outputs: usize, max_frames: usize) -> Self {
        let engine_frames = (max_frames as f64 * (1.0 + MAX_DEVIATION)).ceil() as usize + TAPS + 2;
        let capacity = engine_frames + max_frames + 4 * TAPS;
        Self {
            kernel: kernel(),
            inputs: Fifo::new(inputs, capacity),
            outputs: Fifo::new(outputs, capacity),
            engine: OwnedBuffers::new(inputs, outputs, engine_frames),
        }
    }

    /// One device callback at `speed`: `render` processes the engine's
    /// frames (inputs filled, outputs to fill).
    pub fn process(
        &mut self,
        io: &mut dyn DeviceBuffers,
        speed: f64,
        render: impl FnOnce(&mut OwnedBuffers),
    ) {
        let speed = speed.clamp(1.0 - MAX_DEVIATION, 1.0 + MAX_DEVIATION);
        let n = io.frames();
        if n == 0 {
            return;
        }
        // Engine frames the device's need: enough rendered frames for every
        // read (with its taps after it).
        let have = self.outputs.readable(speed);
        let need_frames = if have >= n {
            0
        } else {
            let last = self.outputs.pos + (n - 1) as f64 * speed;
            (last.floor() as usize + (TAPS - BEFORE) + 1).saturating_sub(self.outputs.len)
        };
        let m = need_frames.min(self.engine.capacity());
        self.engine.set_frames(m);
        // Inputs: the device's frames in, the engine's out (at 1/speed).
        let ins = io.input_channels().min(self.inputs.data.len());
        self.inputs
            .push(n, |c| if c < ins { io.input(c) } else { &[] });
        let readable = self.inputs.readable(1.0 / speed).min(m);
        {
            let engine = &mut self.engine;
            self.inputs
                .read(&self.kernel, 1.0 / speed, readable, |c, j, v| {
                    engine.input_mut(c)[j] = v;
                });
            for c in 0..engine.input_channels() {
                engine.input_mut(c)[readable..m].fill(0.0);
            }
        }
        if m > 0 {
            render(&mut self.engine);
        }
        // Outputs: the engine's frames in, the device's out.
        let outs = io.output_channels().min(self.outputs.data.len());
        {
            let engine = &self.engine;
            self.outputs.push(m, |c| engine.output_ref(c));
        }
        self.outputs.read(&self.kernel, speed, n, |c, j, v| {
            if c < outs {
                io.output(c)[j] = v;
            }
        });
    }

    /// Forget what waits (a new start).
    pub fn reset(&mut self) {
        self.inputs.reset();
        self.outputs.reset();
    }
}

/// How far from 1 the speed may go.
pub const MAX_DEVIATION: f64 = 0.02;

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine through varispeed at `speed`: the device hears its frequency
    /// times the speed, and the engine renders `speed` frames per device
    /// frame.
    #[test]
    fn a_sine_plays_faster_and_the_engine_keeps_up() {
        let rate = 48_000.0;
        let f = 1_000.0;
        let speed = 1.01;
        let mut v = Varispeed::new(0, 1, 256);
        let mut io = OwnedBuffers::new(0, 1, 256);
        let mut phase = 0u64;
        let mut heard = Vec::new();
        for _ in 0..400 {
            io.set_frames(256);
            v.process(&mut io, speed, |e| {
                let m = e.frames();
                for (j, o) in e.output(0).iter_mut().enumerate().take(m) {
                    *o =
                        ((phase + j as u64) as f64 * f * std::f64::consts::TAU / rate).sin() as f32;
                }
                phase += m as u64;
            });
            heard.extend_from_slice(io.output_ref(0));
        }
        // Rendered: the device's frames at the speed.
        let device = 400.0 * 256.0;
        assert!(
            (phase as f64 / device - speed).abs() < 1e-3,
            "{}",
            phase as f64 / device
        );
        // Heard: the sine at 1010 Hz (zero crossings over the last second).
        let tail = &heard[heard.len() - 48_000..];
        let ups = tail
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        assert!((ups as f64 - f * speed).abs() <= 1.0, "{ups} crossings");
        // Clean: its peak stays one (no clicks between callbacks).
        let peak = tail.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 1.0).abs() < 0.01, "{peak}");
        let jumps = tail
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        let max_step = (std::f64::consts::TAU * f * speed / rate) as f32;
        assert!(jumps <= max_step * 1.05, "{jumps} > {max_step}");
    }
}
