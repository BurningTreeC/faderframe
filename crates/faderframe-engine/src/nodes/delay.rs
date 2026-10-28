//! A track's own delay ([`faderframe_project::Track::delay_ms`]): its
//! input delayed by a fraction of a sample or more, later or earlier.
//!
//! Whole samples come from a ring; the fraction from a windowed sinc (64
//! taps, Kaiser β 8: within 0.001 dB and 0.0001 samples of the ideal to
//! 20 kHz at 44.1 kHz, and exact on whole samples, where the filter is
//! left out). The filter needs [`HALF`] samples of lookahead, so a
//! fraction is at least that late. An *earlier* track reports latency to
//! the graph and delays itself by that less the advance: the graph's
//! compensation holds every other path back by the latency, so the track
//! is heard ahead of them by its delay.
//!
//! What a delay needs of the graph is its [`DelayShape`] (rate-free, so
//! the project alone tells when it changes): a delay of a millisecond or
//! more needs no latency (at 32 kHz and up), a smaller or negative one
//! reports [`HALF`] samples and the whole milliseconds ahead. Within a
//! shape the delay follows its parameter slot at once, crossfading
//! between the old and the new delay over [`FADE_SECONDS`]; another
//! shape is another node (a rebuild, which dips if the latency changed).

use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_realtime::ParamSlot;

/// Samples of the fractional filter either side of its centre.
pub const HALF: usize = 32;
const TAPS: usize = 2 * HALF;
/// The Kaiser window's β.
const BETA: f64 = 8.0;
/// How long a change of delay crossfades.
pub const FADE_SECONDS: f64 = 0.010;

/// What a delay needs of the graph, whatever the rate (see the module
/// docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DelayShape {
    /// A millisecond or more later: no latency.
    Later,
    /// Up to this many whole milliseconds earlier (0: less than a
    /// millisecond later): that and [`HALF`] samples of latency.
    Ahead(u32),
}

impl DelayShape {
    /// The shape of a delay of `ms` (`None`: no delay, no node).
    pub fn of(ms: f64) -> Option<Self> {
        if ms == 0.0 || ms.is_nan() {
            None
        } else if ms >= 1.0 {
            Some(Self::Later)
        } else {
            Some(Self::Ahead((-ms).ceil().max(0.0) as u32))
        }
    }

    /// A number naming it (node keys).
    pub fn code(self) -> u64 {
        match self {
            Self::Later => 1,
            Self::Ahead(k) => 2 + u64::from(k),
        }
    }

    /// The latency it reports at `rate` (frames).
    pub fn latency(self, rate: f64) -> u32 {
        let frames = |ms: f64| ms * rate / 1000.0;
        match self {
            Self::Later => (HALF as f64 - frames(1.0)).ceil().max(0.0) as u32,
            Self::Ahead(k) => HALF as u32 + frames(f64::from(k)).ceil() as u32,
        }
    }

    /// The real delays (latency included) it can make at `rate`.
    fn range(self, rate: f64) -> (f64, f64) {
        let frames = |ms: f64| ms * rate / 1000.0;
        let r = f64::from(self.latency(rate));
        match self {
            Self::Later => (
                r + frames(1.0),
                r + frames(faderframe_project::MAX_TRACK_DELAY_MS),
            ),
            Self::Ahead(k) => ((r - frames(f64::from(k))).max(HALF as f64), r + frames(1.0)),
        }
    }
}

/// Modified Bessel function of the first kind, order 0.
fn bessel_i0(x: f64) -> f64 {
    let y = x * x / 4.0;
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..64 {
        term *= y / (k * k) as f64;
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

/// One delay: whole samples and, for a fraction, the filter.
#[derive(Clone, Copy)]
struct Tap {
    /// Frames (the latency included).
    delay: f64,
    whole: usize,
    /// `None`: a whole number of samples.
    filter: Option<[f32; TAPS]>,
}

impl Tap {
    fn new(delay: f64) -> Self {
        let mut whole = delay.floor();
        let mut frac = delay - whole;
        if frac > 1.0 - 1e-9 {
            whole += 1.0;
            frac = 0.0;
        }
        if frac < 1e-9 {
            return Self {
                delay,
                whole: whole.max(0.0) as usize,
                filter: None,
            };
        }
        // Tap j weighs the sample `whole + HALF − j` frames back: offset
        // x = (HALF − j) − frac from the delayed instant.
        let norm = bessel_i0(BETA);
        let mut c = [0.0f64; TAPS];
        for (j, v) in c.iter_mut().enumerate() {
            let x = (HALF as f64 - j as f64) - frac;
            let sinc = if x.abs() < 1e-12 {
                1.0
            } else {
                (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
            };
            let r = x / HALF as f64;
            let w = bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / norm;
            *v = sinc * w;
        }
        // Unity at DC.
        let sum: f64 = c.iter().sum();
        let mut filter = [0.0f32; TAPS];
        for (f, v) in filter.iter_mut().zip(c) {
            *f = (v / sum) as f32;
        }
        Self {
            delay,
            whole: whole as usize,
            filter: Some(filter),
        }
    }
}

/// The delay node, in place of a track's input pass-through.
pub struct TrackDelay {
    slot: ParamSlot,
    shape: DelayShape,
    rate: f64,
    latency: u32,
    range: (f64, f64),
    /// Per channel, a power-of-two ring.
    ring: Vec<Vec<f32>>,
    mask: usize,
    /// Frames written so far (absolute index of the next one).
    written: i64,
    /// Frames before this one read as silence (after a reset).
    fresh_from: i64,
    tap: Tap,
    /// Crossfading to this one.
    next: Option<Tap>,
    faded: usize,
    fade: usize,
    /// The slot's value the taps were made for.
    seen: f32,
}

impl TrackDelay {
    /// A delay of `ms` (whose shape is `shape`) on `channels` channels at
    /// `rate`, for blocks of up to `max_block` frames; later values come
    /// from `slot` (milliseconds).
    pub fn new(
        slot: ParamSlot,
        shape: DelayShape,
        ms: f64,
        rate: f64,
        max_block: usize,
        channels: usize,
    ) -> Self {
        let latency = shape.latency(rate);
        let range = shape.range(rate);
        let size = (range.1.ceil() as usize + TAPS + max_block.max(1) + 2).next_power_of_two();
        let mut node = Self {
            slot,
            shape,
            rate,
            latency,
            range,
            ring: vec![vec![0.0; size]; channels.max(1)],
            mask: size - 1,
            written: 0,
            fresh_from: 0,
            tap: Tap::new(range.0),
            next: None,
            faded: 0,
            fade: ((rate * FADE_SECONDS) as usize).max(1),
            seen: ms as f32,
        };
        node.tap = Tap::new(node.delay_for(ms));
        node
    }

    /// The real delay (frames) for `ms`, within what this shape makes.
    fn delay_for(&self, ms: f64) -> f64 {
        (f64::from(self.latency) + ms * self.rate / 1000.0).clamp(self.range.0, self.range.1)
    }

    /// The real delay now (frames, the latency included; tests).
    pub fn delay(&self) -> f64 {
        self.tap.delay
    }

    /// Take a new value from the slot (a value of another shape is one
    /// meant for the node replacing this one: ignored).
    fn follow(&mut self, ms: f32) {
        if ms == self.seen || self.next.is_some() {
            return;
        }
        if DelayShape::of(f64::from(ms)) != Some(self.shape) {
            return;
        }
        self.seen = ms;
        let delay = self.delay_for(f64::from(ms));
        if (delay - self.tap.delay).abs() > 1e-9 {
            self.next = Some(Tap::new(delay));
            self.faded = 0;
        }
    }

    /// The output of `tap` for the input frame at absolute index `at`.
    #[inline]
    fn read(&self, ring: &[f32], tap: &Tap, at: i64) -> f32 {
        let size = ring.len();
        match &tap.filter {
            None => {
                let k = at - tap.whole as i64;
                if k < self.fresh_from {
                    0.0
                } else {
                    ring[k as usize & self.mask]
                }
            }
            Some(c) => {
                // Always summed the same way (the render-ahead graph and
                // the audio thread's meet the ring's end on other samples
                // and must agree to the bit).
                let base = at - tap.whole as i64 - HALF as i64;
                let b = base as usize & self.mask;
                if base >= self.fresh_from && b + TAPS <= size {
                    return dot(&ring[b..b + TAPS], c);
                }
                let mut window = [0.0f32; TAPS];
                for (j, w) in window.iter_mut().enumerate() {
                    let k = base + j as i64;
                    if k >= self.fresh_from {
                        *w = ring[k as usize & self.mask];
                    }
                }
                dot(&window, c)
            }
        }
    }
}

#[inline]
fn dot(a: &[f32], b: &[f32; TAPS]) -> f32 {
    // Eight lanes (vectorised), always added up in this order.
    let mut acc = [0.0f32; 8];
    for (x, y) in a.as_chunks::<8>().0.iter().zip(b.as_chunks::<8>().0) {
        for ((s, p), q) in acc.iter_mut().zip(x).zip(y) {
            *s += p * q;
        }
    }
    acc.iter().sum()
}

impl Processor<EngineContext> for TrackDelay {
    fn latency(&self) -> u32 {
        self.latency
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let (Some(input), Some(out)) = (io.audio_in.first(), io.audio_out.first_mut()) else {
            return;
        };
        self.follow(cx.data.params.get(self.slot));
        let frames = io.frames;
        let start = self.written;
        let channels = out.num_channels().min(self.ring.len());
        for c in 0..channels {
            let mut ring = std::mem::take(&mut self.ring[c]);
            if c < input.num_channels() {
                for (i, v) in input.channel(c)[..frames].iter().enumerate() {
                    ring[(start + i as i64) as usize & self.mask] = *v;
                }
            } else {
                for i in 0..frames {
                    ring[(start + i as i64) as usize & self.mask] = 0.0;
                }
            }
            let dst = &mut out.channel_mut(c)[..frames];
            match self.next {
                None => {
                    for (i, o) in dst.iter_mut().enumerate() {
                        *o = self.read(&ring, &self.tap, start + i as i64);
                    }
                }
                Some(next) => {
                    let step = 1.0 / self.fade as f32;
                    for (i, o) in dst.iter_mut().enumerate() {
                        let at = start + i as i64;
                        let g = ((self.faded + i) as f32 * step).min(1.0);
                        let a = self.read(&ring, &self.tap, at);
                        let b = self.read(&ring, &next, at);
                        *o = a + (b - a) * g;
                    }
                }
            }
            self.ring[c] = ring;
        }
        for c in channels..out.num_channels() {
            out.channel_mut(c)[..frames].fill(0.0);
        }
        self.written += frames as i64;
        if let Some(next) = self.next {
            self.faded += frames;
            if self.faded >= self.fade {
                self.tap = next;
                self.next = None;
            }
        }
    }

    fn reset(&mut self) {
        // What it holds is forgotten, not cleared (a reset is cheap).
        self.fresh_from = self.written;
        if let Some(next) = self.next.take() {
            self.tap = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The magnitude (dB) and phase delay (frames) of `tap` at `hz`.
    fn response(tap: &Tap, hz: f64, rate: f64) -> (f64, f64) {
        let w = 2.0 * std::f64::consts::PI * hz / rate;
        let (mut re, mut im) = (0.0, 0.0);
        match &tap.filter {
            None => return (0.0, tap.whole as f64),
            Some(c) => {
                for (j, v) in c.iter().enumerate() {
                    // Tap j: the sample `whole + HALF − j` back.
                    let d = (tap.whole + HALF) as f64 - j as f64;
                    re += f64::from(*v) * (-w * d).cos();
                    im += f64::from(*v) * (-w * d).sin();
                }
            }
        }
        let mag = 20.0 * (re * re + im * im).sqrt().log10();
        // The phase wraps every period: the delay nearest the tap's.
        let period = rate / hz;
        let d = -im.atan2(re) / w;
        (mag, d + ((tap.delay - d) / period).round() * period)
    }

    #[test]
    fn fractions_are_flat_to_twenty_kilohertz() {
        for rate in [44_100.0, 48_000.0, 96_000.0] {
            for frac in [0.1, 0.25, 0.5, 0.75, 0.9] {
                let tap = Tap::new(40.0 + frac);
                for hz in [20.0, 100.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0, 20_000.0] {
                    let (mag, delay) = response(&tap, hz, rate);
                    assert!(mag.abs() < 0.01, "{rate} {frac} {hz}: {mag} dB");
                    assert!(
                        (delay - (40.0 + frac)).abs() < 0.001,
                        "{rate} {frac} {hz}: {delay}"
                    );
                }
            }
        }
    }

    #[test]
    fn shapes_need_latency_only_ahead() {
        assert_eq!(DelayShape::of(0.0), None);
        assert_eq!(DelayShape::of(1.0), Some(DelayShape::Later));
        assert_eq!(DelayShape::of(0.4), Some(DelayShape::Ahead(0)));
        assert_eq!(DelayShape::of(-0.4), Some(DelayShape::Ahead(1)));
        assert_eq!(DelayShape::of(-3.0), Some(DelayShape::Ahead(3)));
        assert_eq!(DelayShape::of(-3.01), Some(DelayShape::Ahead(4)));
        assert_eq!(DelayShape::Later.latency(48_000.0), 0);
        assert_eq!(DelayShape::Later.latency(22_050.0), 10);
        assert_eq!(DelayShape::Ahead(0).latency(48_000.0), 32);
        assert_eq!(DelayShape::Ahead(2).latency(48_000.0), 32 + 96);
        // Every delay of a shape is made at least the filter's half late.
        for ms in [1.0, 0.01, 0.99, -0.01, -0.5, -1.0, -2.7, -29.99] {
            let shape = DelayShape::of(ms).expect("a delay");
            for rate in [22_050.0, 44_100.0, 48_000.0, 192_000.0] {
                let (lo, hi) = shape.range(rate);
                let real = f64::from(shape.latency(rate)) + ms * rate / 1000.0;
                assert!(lo >= HALF as f64, "{ms} {rate}");
                assert!(
                    real >= lo - 1e-9 && real <= hi + 1e-9,
                    "{ms} {rate}: {real}"
                );
            }
        }
    }
}
