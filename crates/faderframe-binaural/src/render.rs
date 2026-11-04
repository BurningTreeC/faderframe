//! The renderer: a layout's channels to two ears.

use crate::BinauralError;
use crate::correction::{Compiled, Correction};
use crate::head::Head;
use faderframe_core::ChannelLayout;
use faderframe_core::ambisonics::{self, AmbiOrder, Orientation, Rotator};
use faderframe_core::surround::{MAX_SPEAKERS, Speaker};
use faderframe_core::vbap::Vbap;
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// How the headphone render places the mix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Room {
    /// The dry measurement: close, anechoic.
    Near,
    /// A little room around it.
    #[default]
    Mid,
    /// More room, further away.
    Far,
}

impl Room {
    pub const ALL: [Room; 3] = [Room::Near, Room::Mid, Room::Far];

    pub fn name(self) -> &'static str {
        match self {
            Room::Near => "Near",
            Room::Mid => "Mid",
            Room::Far => "Far",
        }
    }

    /// Its stable id (preferences, scripts).
    pub fn id(self) -> &'static str {
        match self {
            Room::Near => "near",
            Room::Mid => "mid",
            Room::Far => "far",
        }
    }

    pub fn from_id(id: &str) -> Option<Room> {
        Room::ALL.into_iter().find(|r| r.id() == id)
    }

    /// The room's level against the direct sound and its decay (s).
    fn settings(self) -> Option<(f32, f32, f32)> {
        match self {
            Room::Near => None,
            // (wet gain, RT60, pre-delay s)
            Room::Mid => Some((0.5, 0.32, 0.008)),
            Room::Far => Some((0.85, 0.5, 0.016)),
        }
    }
}

/// A small room: an eight-line feedback delay network (Hadamard mixing,
/// damped), fed with the mix in mono, two decorrelated outputs.
struct Fdn {
    lines: [Vec<f32>; 8],
    pos: [usize; 8],
    len: [usize; 8],
    gain: [f32; 8],
    damp: [f32; 8],
    lp: f32,
    pre: Vec<f32>,
    pre_pos: usize,
    wet: f32,
}

const FDN_MS: [f32; 8] = [11.3, 13.7, 15.1, 17.9, 19.3, 23.1, 25.7, 29.3];

impl Fdn {
    fn new(rate: f32, wet: f32, rt60: f32, predelay: f32) -> Self {
        let len = FDN_MS.map(|ms| ((ms * 0.001 * rate) as usize).max(1));
        let gain = len.map(|l| 10f32.powf(-3.0 * l as f32 / (rt60 * rate)));
        Self {
            lines: len.map(|l| vec![0.0; l]),
            pos: [0; 8],
            len,
            gain,
            damp: [0.0; 8],
            // A one-pole at about 6 kHz in the loop.
            lp: 1.0 - (-std::f32::consts::TAU * 6_000.0 / rate).exp(),
            pre: vec![0.0; ((predelay * rate) as usize).max(1)],
            pre_pos: 0,
            wet,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> (f32, f32) {
        let x = {
            let d = self.pre[self.pre_pos];
            self.pre[self.pre_pos] = x;
            self.pre_pos = (self.pre_pos + 1) % self.pre.len();
            d
        };
        let mut out = [0.0f32; 8];
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.lines[i][self.pos[i]];
        }
        // Hadamard 8 × 8, scaled to stay lossless.
        let mut h = out;
        let mut step = 1;
        while step < 8 {
            for i in (0..8).step_by(step * 2) {
                for j in i..i + step {
                    let (a, b) = (h[j], h[j + step]);
                    h[j] = a + b;
                    h[j + step] = a - b;
                }
            }
            step *= 2;
        }
        let norm = 1.0 / 8f32.sqrt();
        for (i, &hi) in h.iter().enumerate() {
            let fb = hi * norm * self.gain[i];
            self.damp[i] += (fb - self.damp[i]) * self.lp;
            self.lines[i][self.pos[i]] = self.damp[i] + x * 0.35;
            self.pos[i] = (self.pos[i] + 1) % self.len[i];
        }
        let l = out[0] - out[2] + out[4] - out[6] + out[1] * 0.5;
        let r = out[1] - out[3] + out[5] - out[7] + out[2] * 0.5;
        (l * self.wet, r * self.wet)
    }

    fn flush(&mut self) {
        for d in &mut self.damp {
            if d.abs() < 1e-20 {
                *d = 0.0;
            }
        }
    }
}

/// The baked directions the renderer convolves.
const DIRS: usize = crate::DIRECTIONS.len();

/// What the renderer's input channels are.
enum Input {
    /// Speakers (a bed, stereo, mono): each main channel a direction in
    /// the ambisonic frame; the LFE (and discrete channels) none.
    Speakers {
        dirs: Box<[Option<[f32; 3]>; MAX_SPEAKERS]>,
    },
    /// An ambisonic field: decoded onto the baked directions (AllRAD,
    /// direction × channel), after the head's turn.
    Field {
        order: AmbiOrder,
        decode: Box<[[f32; ambisonics::MAX_CHANNELS]; DIRS]>,
        rotator: Box<Rotator>,
    },
}

/// Renders the channels of a layout to two ears.
///
/// Every input channel reaches the ears through the baked directions:
/// its share of each direction's feed (a weight per direction and
/// channel — a speaker at a baked direction is that direction alone, one
/// between them a VBAP blend, an ambisonic field its AllRAD decode),
/// each direction's feed convolved with its measured pair. Turning the
/// listener's head ([`Renderer::turn`]) turns the scene the other way:
/// the speakers' directions relative to the head move over the baked
/// directions, a field is rotated before its decode — only the weights
/// change (glided over each block, the head's orientation smoothed), the
/// measured filters never.
pub struct Renderer {
    block: usize,
    parts: usize,
    rate: f32,
    input: Input,
    /// The baked directions as speakers, for VBAP when the head turns.
    baked: Box<Vbap>,
    /// Per direction: its filters per ear and partition (`None`: the head
    /// has no measurement there — never the case for a complete set).
    filters: Vec<Option<[Vec<Vec<Complex32>>; 2]>>,
    /// Per direction: the last `parts` input spectra (a ring).
    fdl: Vec<Vec<Vec<Complex32>>>,
    /// Per direction: blocks its feed has been silent (its ring is all
    /// zeros once this reaches `parts`: nothing to multiply).
    quiet: Vec<usize>,
    ring: usize,
    /// Each direction's weight of each channel: now, wanted for the next
    /// block, and with the head straight.
    weights: Box<[[f32; MAX_SPEAKERS]; DIRS]>,
    wanted: Box<[[f32; MAX_SPEAKERS]; DIRS]>,
    straight: Box<[[f32; MAX_SPEAKERS]; DIRS]>,
    /// The head: where it is (smoothed) and where it is going, the
    /// smoothing time (s), and the orientation the weights were made for.
    head: Orientation,
    target: Orientation,
    smoothing: f32,
    made_for: Option<Orientation>,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    frame: Vec<f32>,
    spec: Vec<Complex32>,
    acc: [Vec<Complex32>; 2],
    time: Vec<f32>,
    scratch_f: Vec<Complex32>,
    scratch_i: Vec<Complex32>,
    /// Per direction: the previous block of its feed and this one.
    feed_prev: Vec<Vec<f32>>,
    feed: Vec<Vec<f32>>,
    /// Per channel: the block being filled.
    fill_in: Vec<Vec<f32>>,
    fill: usize,
    out: [Vec<f32>; 2],
    lfe_gain: f32,
    lfe: [[f32; 4]; 1],
    lfe_coef: [f32; 5],
    lfe_buf: Vec<f32>,
    /// Which channels are the LFE.
    lfe_channels: [bool; MAX_SPEAKERS],
    room: Option<Fdn>,
    mono_buf: Vec<f32>,
    /// The headphone correction: a gain, biquads and a FIR per ear.
    eq_gain: f32,
    eq: Vec<[f32; 5]>,
    eq_state: [Vec<[f32; 4]>; 2],
    eq_fir: Option<[Fir; 2]>,
}

/// A uniformly partitioned convolution of one channel, block by block
/// (the renderer's transforms).
struct Fir {
    parts: Vec<Vec<Complex32>>,
    fdl: Vec<Vec<Complex32>>,
    ring: usize,
    prev: Vec<f32>,
}

impl Fir {
    fn new(taps: &[f32], block: usize, fft: &Arc<dyn RealToComplex<f32>>) -> Self {
        let n = 2 * block;
        let count = taps.len().div_ceil(block).max(1);
        let parts = (0..count)
            .map(|p| {
                let mut t = vec![0.0f32; n];
                for (i, v) in taps.iter().skip(p * block).take(block).enumerate() {
                    t[i] = *v;
                }
                let mut s = fft.make_output_vec();
                let _ = fft.process(&mut t, &mut s);
                s
            })
            .collect();
        Self {
            parts,
            fdl: vec![vec![Complex32::default(); block + 1]; count],
            ring: 0,
            prev: vec![0.0; block],
        }
    }
}

/// The baked directions as speakers (for VBAP over them).
pub(crate) fn baked_speakers() -> Vec<Speaker> {
    crate::DIRECTIONS
        .iter()
        .map(|&(azimuth, elevation)| Speaker {
            label: "",
            mask: 0,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            azimuth,
            elevation,
            lfe: false,
        })
        .collect()
}

impl Renderer {
    /// For `layout` at `rate` through `head`'s ears with `room`, evened
    /// out for the headphones by `correction`.
    pub fn new(
        layout: ChannelLayout,
        rate: u32,
        room: Room,
        head: &Head,
        correction: Option<&Correction>,
    ) -> Result<Self, BinauralError> {
        let set = head.at(rate)?;
        if set.pairs.len() < DIRS {
            return Err(BinauralError::Data);
        }
        let block = if rate > 50_000 { 128 } else { 64 };
        let parts = set.taps.div_ceil(block).max(1);
        let n = 2 * block;
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);
        let channels = layout.channel_count().min(MAX_SPEAKERS);
        let spectrum = |taps: &[f32], p: usize| -> Vec<Complex32> {
            let mut t = vec![0.0f32; n];
            for (i, v) in taps.iter().skip(p * block).take(block).enumerate() {
                t[i] = *v;
            }
            let mut s = fft.make_output_vec();
            let _ = fft.process(&mut t, &mut s);
            s
        };
        // Each direction's measured pair, partitioned.
        let filters = (0..DIRS)
            .map(|d| {
                let pair = set.pairs.get(d)?;
                let ear = |taps: &[f32]| -> Vec<Vec<Complex32>> {
                    (0..parts).map(|p| spectrum(taps, p)).collect()
                };
                Some([ear(&pair.left), ear(&pair.right)])
            })
            .collect();
        let baked_list = baked_speakers();
        let baked = Box::new(Vbap::new(&baked_list).map_err(|_| BinauralError::Data)?);
        // Each channel's weights with the head straight: its direction's
        // own pair, or the blend of those it is panned over (`weights`);
        // a field's decode.
        let mut straight = Box::new([[0.0f32; MAX_SPEAKERS]; DIRS]);
        let mut lfe_channels = [false; MAX_SPEAKERS];
        let input = match layout {
            ChannelLayout::Ambisonic(order) => {
                let decoder = ambisonics::Decoder::allrad(order, &baked_list, &baked);
                let mut decode = Box::new([[0.0f32; ambisonics::MAX_CHANNELS]; DIRS]);
                for (row, g) in decode.iter_mut().zip(decoder.gains()) {
                    *row = *g;
                }
                for (d, row) in straight.iter_mut().enumerate() {
                    row[..order.channels()].copy_from_slice(&decode[d][..order.channels()]);
                }
                Input::Field {
                    order,
                    decode,
                    rotator: Box::new(Rotator::new()),
                }
            }
            _ => {
                let mut dirs = Box::new([None; MAX_SPEAKERS]);
                let speakers = faderframe_core::surround::speakers_of(layout);
                for c in 0..channels {
                    let w = crate::weights(layout, c);
                    // No direction (the LFE, discrete channels): low-passed
                    // to both ears.
                    lfe_channels[c] = w.is_empty();
                    for (d, g) in w {
                        straight[d][c] = g;
                    }
                    if let Some(s) = speakers.and_then(|s| s.get(c)).filter(|_| !lfe_channels[c]) {
                        dirs[c] = Some(ambisonics::direction(s.azimuth, s.elevation));
                    }
                }
                Input::Speakers { dirs }
            }
        };
        let lfe_coef = lowpass(120.0, rate as f32);
        let eq = correction.map(|c| c.compile(rate)).unwrap_or(Compiled {
            gain: 1.0,
            biquads: Vec::new(),
            fir: None,
        });
        let eq_fir = eq
            .fir
            .as_ref()
            .map(|[l, r]| [Fir::new(l, block, &fft), Fir::new(r, block, &fft)]);
        let eq_state = [
            vec![[0.0f32; 4]; eq.biquads.len()],
            vec![[0.0f32; 4]; eq.biquads.len()],
        ];
        Ok(Self {
            block,
            parts,
            rate: rate as f32,
            input,
            baked,
            filters,
            fdl: vec![vec![vec![Complex32::default(); block + 1]; parts]; DIRS],
            quiet: vec![parts; DIRS],
            ring: 0,
            weights: straight.clone(),
            wanted: straight.clone(),
            straight,
            head: Orientation::IDENTITY,
            target: Orientation::IDENTITY,
            smoothing: 0.0,
            made_for: Some(Orientation::IDENTITY),
            frame: vec![0.0; n],
            spec: vec![Complex32::default(); block + 1],
            acc: [
                vec![Complex32::default(); block + 1],
                vec![Complex32::default(); block + 1],
            ],
            time: vec![0.0; n],
            scratch_f: fft.make_scratch_vec(),
            scratch_i: ifft.make_scratch_vec(),
            fft,
            ifft,
            feed_prev: vec![vec![0.0; block]; DIRS],
            feed: vec![vec![0.0; block]; DIRS],
            fill_in: vec![vec![0.0; block]; channels],
            fill: 0,
            out: [vec![0.0; block], vec![0.0; block]],
            lfe_gain: 10f32.powf(10.0 / 20.0),
            lfe: [[0.0; 4]],
            lfe_coef,
            lfe_buf: vec![0.0; block],
            lfe_channels,
            room: room
                .settings()
                .map(|(wet, rt, pre)| Fdn::new(rate as f32, wet, rt, pre)),
            mono_buf: vec![0.0; block],
            eq_gain: eq.gain,
            eq: eq.biquads,
            eq_state,
            eq_fir,
        })
    }

    /// Frames of delay the render adds (one block).
    pub fn latency(&self) -> usize {
        self.block
    }

    /// Turn the listener's head towards `head` (from the next block on),
    /// gliding there with a time constant of `smoothing` seconds (0: in
    /// one block). Allocation-free.
    pub fn turn(&mut self, head: Orientation, smoothing: f32) {
        self.target = head;
        self.smoothing = if smoothing.is_finite() {
            smoothing.clamp(0.0, 2.0)
        } else {
            0.0
        };
    }

    /// Where the listener's head is now (smoothed).
    pub fn head(&self) -> Orientation {
        self.head
    }

    /// Render `frames` frames of `input` (one slice per channel, at least
    /// `frames` long) into `left` and `right`.
    pub fn process(
        &mut self,
        input: &[&[f32]],
        left: &mut [f32],
        right: &mut [f32],
        frames: usize,
    ) {
        for i in 0..frames {
            for (c, buf) in self.fill_in.iter_mut().enumerate() {
                buf[self.fill] = input.get(c).and_then(|s| s.get(i)).copied().unwrap_or(0.0);
            }
            left[i] = self.out[0][self.fill];
            right[i] = self.out[1][self.fill];
            self.fill += 1;
            if self.fill == self.block {
                self.fill = 0;
                self.run_block();
            }
        }
    }

    /// The head's next step towards its target, and the weights for it.
    fn steer(&mut self) {
        if self.head != self.target {
            let t = if self.smoothing <= 0.0 {
                1.0
            } else {
                1.0 - (-(self.block as f32) / (self.smoothing * self.rate)).exp()
            };
            self.head = self.head.slerp(self.target, t);
            if self.head.angle_to(&self.target) < 0.01 {
                self.head = self.target;
            }
        }
        if self.made_for == Some(self.head) {
            return;
        }
        self.made_for = Some(self.head);
        if self.head.is_identity() {
            *self.wanted = *self.straight;
            return;
        }
        // The scene relative to the head: turned the other way.
        let back = self.head.inverse();
        match &self.input {
            Input::Speakers { dirs } => {
                let mut g = [0.0f32; MAX_SPEAKERS];
                for row in self.wanted.iter_mut() {
                    row.fill(0.0);
                }
                for (c, dir) in dirs.iter().enumerate().take(self.fill_in.len()) {
                    let Some(dir) = dir else { continue };
                    let (az, el) = ambisonics::angles(back.rotate(*dir));
                    self.baked.gains(az, el, &mut g[..DIRS]);
                    for (row, v) in self.wanted.iter_mut().zip(&g) {
                        row[c] = *v;
                    }
                }
            }
            Input::Field {
                order,
                decode,
                rotator,
            } => {
                let mut m = [[0.0f32; ambisonics::MAX_CHANNELS]; ambisonics::MAX_CHANNELS];
                rotator.matrix(*order, &back, &mut m);
                let n = order.channels();
                for (row, dec) in self.wanted.iter_mut().zip(decode.iter()) {
                    for (j, w) in row.iter_mut().enumerate().take(n) {
                        *w = (0..n).map(|k| dec[k] * m[k][j]).sum();
                    }
                }
            }
        }
    }

    fn run_block(&mut self) {
        let b = self.block;
        let n = 2 * b;
        let parts = self.parts;
        self.steer();
        self.ring = (self.ring + parts - 1) % parts;
        for a in &mut self.acc {
            a.iter_mut().for_each(|v| *v = Complex32::default());
        }
        self.lfe_buf.iter_mut().for_each(|v| *v = 0.0);
        self.mono_buf.iter_mut().for_each(|v| *v = 0.0);
        // The LFE (low-passed, to both ears) and the room's mono feed:
        // a bed's main channels summed, a field's W.
        for c in 0..self.fill_in.len() {
            if self.lfe_channels[c] {
                for (i, &x) in self.fill_in[c].iter().enumerate() {
                    self.lfe_buf[i] += biquad(&mut self.lfe[0], &self.lfe_coef, x);
                }
                continue;
            }
            let into_room = match self.input {
                Input::Speakers { .. } => self.straight.iter().any(|row| row[c] != 0.0),
                Input::Field { .. } => c == 0,
            };
            if into_room {
                for (m, &x) in self.mono_buf.iter_mut().zip(&self.fill_in[c]) {
                    *m += x;
                }
            }
        }
        // Each direction's feed: its channels by their weights, glided
        // from the last block's to this one's.
        let step = 1.0 / b as f32;
        for d in 0..DIRS {
            let feed = &mut self.feed[d];
            feed.fill(0.0);
            let mut sounding = false;
            for (c, x) in self.fill_in.iter().enumerate() {
                let (from, to) = (self.weights[d][c], self.wanted[d][c]);
                if from == 0.0 && to == 0.0 {
                    continue;
                }
                sounding = true;
                if from == to {
                    for (f, &v) in feed.iter_mut().zip(x) {
                        *f += v * to;
                    }
                } else {
                    let delta = (to - from) * step;
                    for (i, (f, &v)) in feed.iter_mut().zip(x).enumerate() {
                        *f += v * (from + delta * (i + 1) as f32);
                    }
                }
            }
            let Some(filter) = &self.filters[d] else {
                continue;
            };
            if !sounding {
                // Silent: its ring fills with zeros; once all of it is,
                // there is nothing to add.
                self.fdl[d][self.ring]
                    .iter_mut()
                    .for_each(|v| *v = Complex32::default());
                self.feed_prev[d].fill(0.0);
                self.quiet[d] = (self.quiet[d] + 1).min(parts);
                if self.quiet[d] >= parts {
                    continue;
                }
            } else {
                self.quiet[d] = 0;
                // Overlap-save: the previous block then this one.
                self.frame[..b].copy_from_slice(&self.feed_prev[d]);
                self.frame[b..n].copy_from_slice(&self.feed[d]);
                let _ = self.fft.process_with_scratch(
                    &mut self.frame,
                    &mut self.spec,
                    &mut self.scratch_f,
                );
                self.fdl[d][self.ring].copy_from_slice(&self.spec);
                std::mem::swap(&mut self.feed_prev[d], &mut self.feed[d]);
            }
            for (ear, acc) in self.acc.iter_mut().enumerate() {
                for (p, h) in filter[ear].iter().enumerate() {
                    let x = &self.fdl[d][(self.ring + p) % parts];
                    for ((a, &xv), &hv) in acc.iter_mut().zip(x).zip(h) {
                        *a += xv * hv;
                    }
                }
            }
        }
        *self.weights = *self.wanted;
        let scale = 1.0 / n as f32;
        for ear in 0..2 {
            // The inverse transform needs real DC and Nyquist bins.
            self.acc[ear][0].im = 0.0;
            self.acc[ear][b].im = 0.0;
            let _ = self.ifft.process_with_scratch(
                &mut self.acc[ear],
                &mut self.time,
                &mut self.scratch_i,
            );
            for i in 0..b {
                self.out[ear][i] = self.time[b + i] * scale + self.lfe_buf[i] * self.lfe_gain;
            }
        }
        if let Some(room) = self.room.as_mut() {
            for i in 0..b {
                let (l, r) = room.process(self.mono_buf[i]);
                self.out[0][i] += l;
                self.out[1][i] += r;
            }
            room.flush();
        }
        // The headphone correction, on this block (no more latency).
        if let Some(firs) = self.eq_fir.as_mut() {
            for (ear, fir) in firs.iter_mut().enumerate() {
                let parts = fir.parts.len();
                fir.ring = (fir.ring + parts - 1) % parts;
                self.frame[..b].copy_from_slice(&fir.prev);
                self.frame[b..n].copy_from_slice(&self.out[ear]);
                fir.prev.copy_from_slice(&self.out[ear]);
                let _ = self.fft.process_with_scratch(
                    &mut self.frame,
                    &mut self.spec,
                    &mut self.scratch_f,
                );
                fir.fdl[fir.ring].copy_from_slice(&self.spec);
                let acc = &mut self.acc[0];
                acc.iter_mut().for_each(|v| *v = Complex32::default());
                for (p, h) in fir.parts.iter().enumerate() {
                    let x = &fir.fdl[(fir.ring + p) % parts];
                    for ((a, &xv), &hv) in acc.iter_mut().zip(x).zip(h) {
                        *a += xv * hv;
                    }
                }
                acc[0].im = 0.0;
                acc[b].im = 0.0;
                let _ = self
                    .ifft
                    .process_with_scratch(acc, &mut self.time, &mut self.scratch_i);
                for i in 0..b {
                    self.out[ear][i] = self.time[b + i] * scale;
                }
            }
        }
        if !self.eq.is_empty() || self.eq_gain != 1.0 {
            for ear in 0..2 {
                for v in self.out[ear].iter_mut() {
                    let mut x = *v * self.eq_gain;
                    for (k, z) in self.eq.iter().zip(self.eq_state[ear].iter_mut()) {
                        x = biquad(z, k, x);
                    }
                    *v = x;
                }
            }
        }
    }
}

/// A second-order Butterworth low pass: (b0, b1, b2, a1, a2).
fn lowpass(freq: f32, rate: f32) -> [f32; 5] {
    let w = std::f32::consts::TAU * freq / rate;
    let (s, c) = w.sin_cos();
    let q = std::f32::consts::FRAC_1_SQRT_2;
    let alpha = s / (2.0 * q);
    let a0 = 1.0 + alpha;
    let b1 = (1.0 - c) / a0;
    [b1 / 2.0, b1, b1 / 2.0, -2.0 * c / a0, (1.0 - alpha) / a0]
}

#[inline]
fn biquad(z: &mut [f32; 4], k: &[f32; 5], x: f32) -> f32 {
    let y = k[0] * x + k[1] * z[0] + k[2] * z[1] - k[3] * z[2] - k[4] * z[3];
    z[1] = z[0];
    z[0] = x;
    z[3] = z[2];
    z[2] = if y.abs() < 1e-20 { 0.0 } else { y };
    y
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::direction;
    use faderframe_core::SurroundFormat;

    fn ku100(layout: ChannelLayout, room: Room) -> Renderer {
        Renderer::new(layout, 48_000, room, &Head::default(), None).unwrap()
    }

    fn level(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// The first sample over a tenth of the peak.
    fn onset(x: &[f32]) -> usize {
        let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        x.iter().position(|v| v.abs() > peak * 0.1).unwrap_or(0)
    }

    #[test]
    fn every_speaker_has_a_direction_but_the_lfe() {
        for f in SurroundFormat::ALL {
            let layout = ChannelLayout::Surround(f);
            for (c, s) in f.speakers().iter().enumerate() {
                assert_eq!(
                    direction(layout, c).is_none(),
                    s.lfe,
                    "{} {}",
                    f.name(),
                    s.label
                );
            }
        }
        assert_eq!(direction(ChannelLayout::Stereo, 0), Some(1));
        assert_eq!(direction(ChannelLayout::Mono, 0), Some(0));
    }

    /// An impulse on a stereo master's left speaker: the left ear first
    /// and louder; on the centre both alike.
    #[test]
    fn a_left_speaker_reaches_the_left_ear_first_and_louder() {
        let mut r = ku100(ChannelLayout::Stereo, Room::Near);
        let n = 2048;
        let mut x = vec![0.0f32; n];
        x[100] = 1.0;
        let silent = vec![0.0f32; n];
        let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
        r.process(&[&x, &silent], &mut l, &mut rr, n);
        assert!(
            onset(&l) + 5 < onset(&rr),
            "ITD: {} {}",
            onset(&l),
            onset(&rr)
        );
        assert!(
            level(&l) > 1.5 * level(&rr),
            "ILD: {} {}",
            level(&l),
            level(&rr)
        );
        // Delayed by one block plus the trimmed onset margin.
        assert!(onset(&l) >= 100 + r.latency());
        let mut c = ku100(ChannelLayout::Mono, Room::Near);
        let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
        c.process(&[&x], &mut l, &mut rr, n);
        let (a, b) = (level(&l), level(&rr));
        assert!((a - b).abs() < 0.2 * a.max(b), "centre: {a} {b}");
    }

    /// Block by block or all at once: the same output.
    #[test]
    fn the_split_of_the_calls_does_not_matter() {
        let layout = ChannelLayout::Surround(SurroundFormat::S714);
        let n = 3000;
        let input: Vec<Vec<f32>> = (0..12)
            .map(|c| {
                (0..n)
                    .map(|i| ((i * (c + 3)) % 97) as f32 / 97.0 - 0.5)
                    .collect()
            })
            .collect();
        let refs: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
        let mut a = ku100(layout, Room::Mid);
        let (mut al, mut ar) = (vec![0.0; n], vec![0.0; n]);
        a.process(&refs, &mut al, &mut ar, n);
        let mut b = ku100(layout, Room::Mid);
        let (mut bl, mut br) = (vec![0.0; n], vec![0.0; n]);
        let mut at = 0;
        for step in [1usize, 63, 64, 65, 200, 1000].iter().cycle() {
            if at >= n {
                break;
            }
            let m = (*step).min(n - at);
            let part: Vec<&[f32]> = input.iter().map(|c| &c[at..at + m]).collect();
            b.process(&part, &mut bl[at..at + m], &mut br[at..at + m], m);
            at += m;
        }
        for i in 0..n {
            assert!(
                (al[i] - bl[i]).abs() < 1e-5 && (ar[i] - br[i]).abs() < 1e-5,
                "{i}"
            );
        }
    }

    /// The room adds a tail after the direct sound (Mid), Near has none.
    #[test]
    fn mid_and_far_add_a_room() {
        let tail = |room: Room| {
            let mut r = ku100(ChannelLayout::Mono, room);
            let n = 24_000;
            let mut x = vec![0.0f32; n];
            x[0] = 1.0;
            let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
            r.process(&[&x], &mut l, &mut rr, n);
            level(&l[4_800..])
        };
        let energy = |room: Room| {
            let mut r = ku100(ChannelLayout::Mono, room);
            let n = 48_000;
            let mut x = vec![0.0f32; n];
            x[0] = 1.0;
            let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
            r.process(&[&x], &mut l, &mut rr, n);
            let e = |s: &[f32]| s.iter().map(|v| v * v).sum::<f32>();
            // Direct: the first 5 ms after the block; the room: after 20 ms.
            let d = e(&l[..64 + 240]) + e(&rr[..64 + 240]);
            let w = e(&l[960..]) + e(&rr[960..]);
            10.0 * (w / d).log10()
        };
        // About 8 dB under the direct sound in Mid, 3 dB in Far.
        let (mid_db, far_db) = (energy(Room::Mid), energy(Room::Far));
        assert!((-11.0..-5.0).contains(&mid_db), "Mid {mid_db:.1} dB");
        assert!((-6.0..0.0).contains(&far_db), "Far {far_db:.1} dB");
        assert!(tail(Room::Near) < 1e-6);
        let (mid, far) = (tail(Room::Mid), tail(Room::Far));
        assert!(mid > 0.0 && far > mid, "{mid} {far}");
    }

    /// Every head places a left speaker on the left.
    #[test]
    fn every_head_hears_the_left_speaker_on_the_left() {
        for b in crate::BUILTIN_HEADS {
            let head = Head::builtin(b.id).unwrap();
            let mut r =
                Renderer::new(ChannelLayout::Stereo, 48_000, Room::Near, &head, None).unwrap();
            let n = 8192;
            let x: Vec<f32> = (0..n).map(|i| (i as f32 * 0.13).sin() * 0.25).collect();
            let silent = vec![0.0f32; n];
            let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
            r.process(&[&x, &silent], &mut l, &mut rr, n);
            assert!(
                level(&l) > 1.3 * level(&rr),
                "{}: {} {}",
                b.id,
                level(&l),
                level(&rr)
            );
        }
    }

    /// The correction's gain and filters reach both ears, its FIR too, and
    /// it adds no latency.
    #[test]
    fn the_headphone_correction_applies_without_latency() {
        let n = 4096;
        let mut x = vec![0.0f32; n];
        x[100] = 1.0;
        let run = |c: Option<&Correction>| {
            let mut r = Renderer::new(ChannelLayout::Mono, 48_000, Room::Near, &Head::default(), c)
                .unwrap();
            let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
            r.process(&[&x], &mut l, &mut rr, n);
            (l, rr, r.latency())
        };
        let (plain, _, lat) = run(None);
        let gain = Correction::parse("gain", "Preamp: -6.0206 dB").unwrap();
        let (half, half_r, lat2) = run(Some(&gain));
        assert_eq!(lat, lat2);
        for i in 0..n {
            assert!((half[i] - 0.5 * plain[i]).abs() < 1e-6);
        }
        assert!(level(&half_r) > 0.0);
        // An impulse response that delays by 10 samples and halves.
        let mut ir = vec![0.0f32; 300];
        ir[10] = 0.5;
        let delay = Correction::from_ir("ir", 48_000, &[ir]).unwrap();
        let (moved, _, _) = run(Some(&delay));
        for i in 10..n {
            assert!((moved[i] - 0.5 * plain[i - 10]).abs() < 1e-5, "{i}");
        }
    }

    /// An impulse on any one speaker of 5.1 or 7.1 reaches the ears as that
    /// speaker's own measured pair (by its direction in the speaker table),
    /// one block late — the channel order is the format's, the LFE has no
    /// direction.
    #[test]
    fn each_speaker_reaches_the_ears_through_its_own_direction() {
        let head = Head::default();
        let set = head.at(48_000).unwrap();
        for f in [
            SurroundFormat::S51,
            SurroundFormat::S71,
            SurroundFormat::S714,
        ] {
            let layout = ChannelLayout::Surround(f);
            for (c, s) in f.speakers().iter().enumerate() {
                if s.lfe {
                    continue;
                }
                let d = direction(layout, c).unwrap();
                assert_eq!(
                    crate::DIRECTIONS[d],
                    (s.azimuth, s.elevation),
                    "{} {}",
                    f.name(),
                    s.label
                );
                let mut r = Renderer::new(layout, 48_000, Room::Near, &head, None).unwrap();
                let n = 1024 + set.taps;
                let silent = vec![0.0f32; n];
                let mut x = vec![0.0f32; n];
                x[100] = 1.0;
                let input: Vec<&[f32]> = (0..f.channels())
                    .map(|i| if i == c { &x[..] } else { &silent[..] })
                    .collect();
                let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
                r.process(&input, &mut l, &mut rr, n);
                let at = 100 + r.latency();
                let pair = &set.pairs[d];
                for k in 0..set.taps.min(n - at) {
                    assert!(
                        (l[at + k] - pair.left[k]).abs() < 1e-4
                            && (rr[at + k] - pair.right[k]).abs() < 1e-4,
                        "{} {} at {k}",
                        f.name(),
                        s.label
                    );
                }
                // Left speakers are louder on the left ear, right ones on
                // the right.
                match s.side() {
                    Some(faderframe_core::surround::Side::Left) => {
                        assert!(level(&l) > level(&rr), "{}", s.label)
                    }
                    Some(faderframe_core::surround::Side::Right) => {
                        assert!(level(&rr) > level(&l), "{}", s.label)
                    }
                    None => {}
                }
            }
        }
    }

    /// A custom layout's speaker between two measured directions is heard
    /// through their blend; at a measured direction exactly as that one.
    #[test]
    fn a_custom_speaker_is_heard_between_the_measured_ones() {
        use faderframe_core::surround::{LayoutDef, SpeakerDef};
        let spk = |label: &str, azimuth: f32| SpeakerDef {
            label: label.into(),
            azimuth,
            elevation: 0.0,
            lfe: false,
        };
        let def = LayoutDef {
            name: "Hexagon".into(),
            speakers: vec![
                spk("F", 0.0),
                spk("FL", 60.0),
                spk("BL", 120.0),
                spk("B", 180.0),
                spk("BR", -120.0),
                spk("FR", -60.0),
            ],
        };
        let layout = ChannelLayout::Surround(def.intern().unwrap());
        // Ahead is the measured front; 60° left lies between 30° and 90°.
        assert_eq!(crate::weights(layout, 0), vec![(0, 1.0)]);
        let w = crate::weights(layout, 1);
        let dirs: Vec<usize> = w.iter().map(|(d, _)| *d).collect();
        assert_eq!(dirs, [1, 3], "{w:?}");
        assert!((w.iter().map(|(_, g)| g * g).sum::<f32>() - 1.0).abs() < 1e-5);
        // Rendered: the left speaker louder on the left ear.
        let mut r = ku100(layout, Room::Near);
        let n = 8192;
        let x: Vec<f32> = (0..n).map(|i| (i as f32 * 0.07).sin() * 0.25).collect();
        let silent = vec![0.0f32; n];
        let input: Vec<&[f32]> = (0..6)
            .map(|c| if c == 1 { &x[..] } else { &silent[..] })
            .collect();
        let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
        r.process(&input, &mut l, &mut rr, n);
        assert!(level(&l) > 1.3 * level(&rr), "{} {}", level(&l), level(&rr));
    }

    /// Turned 30° to the left, the left speaker of a stereo master is
    /// straight ahead: heard through the front's measured pair exactly,
    /// alike in both ears; turned back, as before.
    #[test]
    fn turning_the_head_turns_the_scene() {
        use faderframe_core::ambisonics::Orientation;
        let head = Head::default();
        let set = head.at(48_000).unwrap();
        let n = 2048 + set.taps;
        let mut x = vec![0.0f32; n];
        x[1000] = 1.0;
        let silent = vec![0.0f32; n];
        let mut r = ku100(ChannelLayout::Stereo, Room::Near);
        r.turn(Orientation::from_ypr(30.0, 0.0, 0.0), 0.0);
        let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
        r.process(&[&x, &silent], &mut l, &mut rr, n);
        assert!(r.head().angle_to(&Orientation::from_ypr(30.0, 0.0, 0.0)) < 1e-3);
        let at = 1000 + r.latency();
        let front = &set.pairs[0];
        for k in 0..set.taps.min(n - at) {
            assert!(
                (l[at + k] - front.left[k]).abs() < 1e-3
                    && (rr[at + k] - front.right[k]).abs() < 1e-3,
                "at {k}"
            );
        }
        // The right speaker is now 60° to the right: louder on the right.
        let mut r = ku100(ChannelLayout::Stereo, Room::Near);
        r.turn(Orientation::from_ypr(30.0, 0.0, 0.0), 0.0);
        let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
        r.process(&[&silent, &x], &mut l, &mut rr, n);
        assert!(level(&rr) > 1.5 * level(&l), "{} {}", level(&l), level(&rr));
    }

    /// An ambisonic field is heard where its sources are: a plane wave
    /// from the left louder on the left ear, from ahead alike; and
    /// turning the head by an angle is the field turned the other way —
    /// the same output, sample for sample.
    #[test]
    fn a_field_is_heard_where_it_comes_from_and_turns_with_the_head() {
        use faderframe_core::AmbiOrder;
        use faderframe_core::ambisonics::{Orientation, encode};
        let n = 8192;
        let tone: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.25).collect();
        let field = |order: AmbiOrder, az: f32| -> Vec<Vec<f32>> {
            let mut g = [0.0f32; 16];
            encode(order, az, 0.0, &mut g);
            (0..order.channels())
                .map(|k| tone.iter().map(|v| v * g[k]).collect())
                .collect()
        };
        let render = |order: AmbiOrder, az: f32, head: Orientation| {
            let layout = ChannelLayout::Ambisonic(order);
            let mut r = ku100(layout, Room::Near);
            r.turn(head, 0.0);
            let input = field(order, az);
            let refs: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
            let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
            r.process(&refs, &mut l, &mut rr, n);
            (l, rr)
        };
        for order in AmbiOrder::ALL {
            let (l, r) = render(order, 80.0, Orientation::IDENTITY);
            assert!(
                level(&l) > 1.5 * level(&r),
                "{order:?}: {} {}",
                level(&l),
                level(&r)
            );
            let (l, r) = render(order, 0.0, Orientation::IDENTITY);
            let (a, b) = (level(&l), level(&r));
            assert!((a - b).abs() < 0.2 * a.max(b), "{order:?} ahead: {a} {b}");
        }
        // Head turned 50° right (yaw −50) = the source 50° further left.
        let (l1, r1) = render(
            AmbiOrder::Third,
            10.0,
            Orientation::from_ypr(-50.0, 0.0, 0.0),
        );
        let (l2, r2) = render(AmbiOrder::Third, 60.0, Orientation::IDENTITY);
        // (Skip the first block and its filters' tail: the turn glides in
        // over it.)
        let taps = Head::default().at(48_000).unwrap().taps;
        for i in 2 * 64 + taps..n {
            assert!(
                (l1[i] - l2[i]).abs() < 2e-4 && (r1[i] - r2[i]).abs() < 2e-4,
                "{i}: {} {} / {} {}",
                l1[i],
                l2[i],
                r1[i],
                r2[i]
            );
        }
    }

    /// The head glides: smoothed, it gets to where it is turned over the
    /// time constant, and the output never jumps on the way.
    #[test]
    fn the_head_glides_to_where_it_is_turned() {
        use faderframe_core::ambisonics::Orientation;
        let layout = ChannelLayout::Surround(SurroundFormat::S51);
        let mut r = ku100(layout, Room::Near);
        let n = 48_000;
        // About 3 kHz: the head shadows it well.
        let tone: Vec<f32> = (0..n).map(|i| (i as f32 * 0.4).sin() * 0.5).collect();
        let silent = vec![0.0f32; n];
        let input: Vec<&[f32]> = (0..6)
            .map(|c| if c == 2 { &tone[..] } else { &silent[..] })
            .collect();
        let target = Orientation::from_ypr(90.0, 0.0, 0.0);
        let (mut l, mut rr) = (vec![0.0; n], vec![0.0; n]);
        // A tenth of a second straight, then turned with 50 ms smoothing.
        let first = 4800;
        let part: Vec<&[f32]> = input.iter().map(|c| &c[..first]).collect();
        r.process(&part, &mut l[..first], &mut rr[..first], first);
        r.turn(target, 0.05);
        let after_10ms: Vec<&[f32]> = input.iter().map(|c| &c[first..first + 480]).collect();
        r.process(
            &after_10ms,
            &mut l[first..first + 480],
            &mut rr[first..first + 480],
            480,
        );
        let moved = r.head().angle_to(&Orientation::IDENTITY);
        assert!(moved > 5.0 && moved < 60.0, "after 10 ms: {moved}°");
        let rest: Vec<&[f32]> = input.iter().map(|c| &c[first + 480..]).collect();
        let m = n - first - 480;
        r.process(&rest, &mut l[first + 480..], &mut rr[first + 480..], m);
        assert!(r.head().angle_to(&target) < 0.01);
        // The centre speaker ends on the right (the head turned left).
        let tail = n - 9600;
        assert!(level(&rr[tail..]) > 1.5 * level(&l[tail..]));
        // No step bigger than the tone's own slope allows (a jump would be
        // a click).
        let steepest = |x: &[f32]| {
            x.windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };
        let steady = steepest(&l[1000..first]).max(steepest(&rr[1000..first]));
        let turning = steepest(&l[first..first + 9600]).max(steepest(&rr[first..first + 9600]));
        assert!(turning < 2.0 * steady.max(1e-3), "{turning} vs {steady}");
    }

    /// A head that is not there is refused (monitoring then stays on the
    /// speakers), and a SOFA file that is not one too.
    #[test]
    fn a_missing_head_is_refused() {
        assert!(Head::builtin("no-such-head").is_err());
        assert!(Head::from_sofa(std::path::Path::new("/no/such/file.sofa")).is_err());
    }
}
