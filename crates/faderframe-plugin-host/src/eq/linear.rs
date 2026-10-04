//! The EQ's linear phase mode.
//!
//! The static bands become one zero-phase FIR per path, designed from the
//! bands' *analog* magnitude (exact to Nyquist, no cramping): sampled on
//! `N / 2 + 1` bins, inverse transformed, centred and windowed. Bands that
//! work on one side or on mid or side make the stereo path a 2 × 2 system
//! (left to left, right to left, …), still linear phase. The FIRs run as a
//! uniformly partitioned convolution in blocks of [`PARTITION`] samples, so
//! the latency is `N / 2 + PARTITION` whatever the host's block size.
//! Dynamic bands stay minimum phase sections after the FIR.
//!
//! Kernels are designed on a thread of their own, which reads the live
//! parameters (automation included) and hands each new set to the audio
//! thread through a mailbox; the audio thread crossfades to it over one
//! partition and hands the old set back to be freed. Nothing on the audio
//! thread allocates.

use super::design::{BandShape, analog_db};
use super::{BANDS, BandParams, Placement, global};
use crate::ParamValues;
use faderframe_realtime::{MailboxReceiver, MailboxSender, mailbox};
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Samples per convolution block.
pub const PARTITION: usize = 256;
/// FIR lengths of the quality settings.
pub const LENGTHS: [usize; 4] = [4096, 8192, 16384, 32768];

/// The latency of linear phase at a quality setting (samples).
pub fn latency(quality: usize) -> u32 {
    (LENGTHS[quality.min(LENGTHS.len() - 1)] / 2 + PARTITION) as u32
}

/// The quality setting the parameters ask for.
pub fn quality(params: &ParamValues) -> usize {
    (params.get(global::QUALITY).round().max(0.0) as usize).min(LENGTHS.len() - 1)
}

/// Whether the parameters ask for linear phase.
pub fn wanted(params: &ParamValues) -> bool {
    params.get(global::PHASE) >= 0.5
}

/// The partitioned spectra of the four paths: left to left, right to
/// left, left to right, right to right (`None`: silent path).
pub struct Kernels {
    paths: [Option<Vec<Complex<f64>>>; 4],
}

/// The zero-phase magnitudes of the static bands on each part of the
/// signal, at `bins` frequencies up to Nyquist.
fn magnitudes(bands: &[BandParams], scale: f64, rate: f64, n: usize) -> [Vec<f64>; 5] {
    let bins = n / 2 + 1;
    let mut m: [Vec<f64>; 5] = std::array::from_fn(|_| vec![1.0; bins]);
    for b in bands.iter().filter(|b| b.enabled && !b.dynamic()) {
        let shape: BandShape = b.shape(scale);
        let target = match b.placement {
            Placement::Stereo => 0,
            Placement::Left => 1,
            Placement::Right => 2,
            Placement::Mid => 3,
            Placement::Side => 4,
        };
        for (k, v) in m[target].iter_mut().enumerate().skip(1) {
            let f = k as f64 * rate / n as f64;
            *v *= 10f64.powf(analog_db(&shape, f) / 20.0);
        }
        // DC: just above it.
        let f0 = 0.25 * rate / n as f64;
        m[target][0] *= 10f64.powf(analog_db(&shape, f0) / 20.0);
    }
    m
}

/// Design the kernels for the current parameters.
pub fn design(
    params: &ParamValues,
    rate: f64,
    n: usize,
    planner: &mut RealFftPlanner<f64>,
) -> Kernels {
    let bands: Vec<BandParams> = (0..BANDS).map(|b| BandParams::read(params, b)).collect();
    let scale = f64::from(params.get(global::GAIN_SCALE));
    let [st, l, r, mid, side] = magnitudes(&bands, scale, rate, n);
    let bins = n / 2 + 1;
    let ms = mid.iter().zip(&side).any(|(a, b)| (a - b).abs() > 1e-9);
    let path = |f: &dyn Fn(usize) -> f64| -> Vec<f64> { (0..bins).map(f).collect() };
    let a = |k: usize| 0.5 * (mid[k] + side[k]);
    let b = |k: usize| 0.5 * (mid[k] - side[k]);
    let lc = |k: usize| st[k] * l[k];
    let rc = |k: usize| st[k] * r[k];
    let ll = path(&|k| a(k) * lc(k));
    let rr = path(&|k| a(k) * rc(k));
    let (rl, lr) = if ms {
        (Some(path(&|k| b(k) * rc(k))), Some(path(&|k| b(k) * lc(k))))
    } else {
        (None, None)
    };
    let mut kernel = |mag: Option<Vec<f64>>| mag.map(|m| partition(&m, n, planner));
    Kernels {
        paths: [kernel(Some(ll)), kernel(rl), kernel(lr), kernel(Some(rr))],
    }
}

/// A zero-phase magnitude turned into a centred, windowed FIR of length
/// `n`, cut into partitions and transformed.
fn partition(magnitude: &[f64], n: usize, planner: &mut RealFftPlanner<f64>) -> Vec<Complex<f64>> {
    let inverse = planner.plan_fft_inverse(n);
    let mut spectrum: Vec<Complex<f64>> = magnitude.iter().map(|m| Complex::new(*m, 0.0)).collect();
    let mut h = vec![0.0; n];
    // A zero-phase spectrum is real; its DC and Nyquist bins must be too.
    if inverse.process(&mut spectrum, &mut h).is_err() {
        return vec![Complex::new(0.0, 0.0); (n / PARTITION) * (PARTITION + 1)];
    }
    // Centre the impulse (time N/2) and window it (Blackman).
    let mut centred = vec![0.0; n];
    for (i, c) in centred.iter_mut().enumerate() {
        let src = (i + n / 2) % n;
        let x = i as f64 / (n - 1) as f64;
        let w = 0.42 - 0.5 * (std::f64::consts::TAU * x).cos()
            + 0.08 * (2.0 * std::f64::consts::TAU * x).cos();
        *c = h[src] / n as f64 * w;
    }
    let forward = planner.plan_fft_forward(2 * PARTITION);
    let k = n / PARTITION;
    let mut out = Vec::with_capacity(k * (PARTITION + 1));
    let mut block = vec![0.0; 2 * PARTITION];
    let mut spec = forward.make_output_vec();
    for p in 0..k {
        block.fill(0.0);
        block[..PARTITION].copy_from_slice(&centred[p * PARTITION..(p + 1) * PARTITION]);
        if forward.process(&mut block, &mut spec).is_err() {
            spec.fill(Complex::new(0.0, 0.0));
        }
        out.extend_from_slice(&spec);
    }
    out
}

/// The design thread: watches the parameters, designs, sends.
struct Designer {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Designer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.thread().unpark();
            let _ = h.join();
        }
    }
}

/// A snapshot of what the kernels depend on.
fn key(params: &ParamValues) -> Vec<u32> {
    let mut k: Vec<u32> = (0..BANDS)
        .flat_map(|b| {
            let p = BandParams::read(params, b);
            [
                u32::from(p.enabled),
                p.kind.index() as u32,
                (p.freq as f32).to_bits(),
                (p.gain as f32).to_bits(),
                (p.q as f32).to_bits(),
                p.slope,
                p.placement.index() as u32,
                u32::from(p.dynamic()),
            ]
        })
        .collect();
    k.push(params.get(global::GAIN_SCALE).to_bits());
    k
}

/// The audio thread's half: the partitioned convolution.
pub struct LinearPhase {
    n: usize,
    k: usize,
    forward: Arc<dyn RealToComplex<f64>>,
    inverse: Arc<dyn ComplexToReal<f64>>,
    kernels: Box<Kernels>,
    old: Option<Box<Kernels>>,
    incoming: MailboxReceiver<Kernels>,
    retired: rtrb::Producer<Box<Kernels>>,
    held: Option<Box<Kernels>>,
    /// Per input channel: the last two partitions of input, and the
    /// frequency domain delay line of their spectra.
    window: [Vec<f64>; 2],
    fdl: [Vec<Complex<f64>>; 2],
    head: usize,
    /// Samples collected for the next block, and the last block's output.
    fill: usize,
    input: [Vec<f64>; 2],
    output: [Vec<f64>; 2],
    acc: Vec<Complex<f64>>,
    acc_old: Vec<Complex<f64>>,
    time: Vec<f64>,
    time_old: Vec<f64>,
    spec: Vec<Complex<f64>>,
    scratch_f: Vec<Complex<f64>>,
    scratch_i: Vec<Complex<f64>>,
    _designer: Designer,
}

impl LinearPhase {
    /// Set up for the parameters' quality at `rate` (control thread): the
    /// first kernels are designed here, later ones on the design thread.
    pub fn new(params: &ParamValues, rate: f64) -> Self {
        let n = LENGTHS[quality(params)];
        let k = n / PARTITION;
        let mut planner = RealFftPlanner::<f64>::new();
        let forward = planner.plan_fft_forward(2 * PARTITION);
        let inverse = planner.plan_fft_inverse(2 * PARTITION);
        let kernels = Box::new(design(params, rate, n, &mut planner));
        let (tx, rx) = mailbox::<Kernels>();
        let (retired_tx, mut retired_rx) = rtrb::RingBuffer::<Box<Kernels>>::new(8);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = Arc::clone(&stop);
            let params = params.clone();
            let mut last = key(&params);
            std::thread::Builder::new()
                .name("faderframe-eq-design".into())
                .spawn(move || {
                    let mut planner = RealFftPlanner::<f64>::new();
                    let tx: MailboxSender<Kernels> = tx;
                    while !stop.load(Ordering::Relaxed) {
                        while let Ok(old) = retired_rx.pop() {
                            drop(old);
                        }
                        let now = key(&params);
                        if now != last {
                            last = now;
                            let kernels = design(&params, rate, n, &mut planner);
                            // An unconsumed older set comes back: freed here.
                            drop(tx.send(Box::new(kernels)));
                        }
                        std::thread::park_timeout(Duration::from_millis(15));
                    }
                })
                .ok()
        };
        let spec = forward.make_output_vec();
        Self {
            n,
            k,
            scratch_f: forward.make_scratch_vec(),
            scratch_i: inverse.make_scratch_vec(),
            forward,
            inverse,
            kernels,
            old: None,
            incoming: rx,
            retired: retired_tx,
            held: None,
            window: [vec![0.0; 2 * PARTITION], vec![0.0; 2 * PARTITION]],
            fdl: [
                vec![Complex::new(0.0, 0.0); k * (PARTITION + 1)],
                vec![Complex::new(0.0, 0.0); k * (PARTITION + 1)],
            ],
            head: 0,
            fill: 0,
            input: [vec![0.0; PARTITION], vec![0.0; PARTITION]],
            output: [vec![0.0; PARTITION], vec![0.0; PARTITION]],
            acc: vec![Complex::new(0.0, 0.0); PARTITION + 1],
            acc_old: vec![Complex::new(0.0, 0.0); PARTITION + 1],
            time: vec![0.0; 2 * PARTITION],
            time_old: vec![0.0; 2 * PARTITION],
            spec,
            _designer: Designer { stop, handle },
        }
    }

    pub fn latency(&self) -> u32 {
        (self.n / 2 + PARTITION) as u32
    }

    /// Filter one frame (in place).
    #[inline]
    pub fn process(&mut self, l: &mut f64, r: &mut f64) {
        let i = self.fill;
        self.input[0][i] = *l;
        self.input[1][i] = *r;
        *l = self.output[0][i];
        *r = self.output[1][i];
        self.fill += 1;
        if self.fill == PARTITION {
            self.fill = 0;
            self.block();
        }
    }

    /// One partition in, one out.
    fn block(&mut self) {
        // Hand the previous set back and take a new one when it arrives.
        if let Some(h) = self.held.take()
            && let Err(rtrb::PushError::Full(h)) = self.retired.push(h)
        {
            self.held = Some(h);
        }
        if self.old.is_none()
            && self.held.is_none()
            && let Some(new) = self.incoming.take()
        {
            self.old = Some(std::mem::replace(&mut self.kernels, new));
        }
        let bins = PARTITION + 1;
        let k = self.k;
        self.head = (self.head + k - 1) % k;
        for c in 0..2 {
            let w = &mut self.window[c];
            w.copy_within(PARTITION.., 0);
            w[PARTITION..].copy_from_slice(&self.input[c]);
            self.time[..].copy_from_slice(w);
            if self
                .forward
                .process_with_scratch(&mut self.time, &mut self.spec, &mut self.scratch_f)
                .is_err()
            {
                self.spec.fill(Complex::new(0.0, 0.0));
            }
            let at = self.head * bins;
            self.fdl[c][at..at + bins].copy_from_slice(&self.spec);
        }
        let norm = 1.0 / (2 * PARTITION) as f64;
        for out in 0..2 {
            Self::accumulate(&self.kernels, &self.fdl, self.head, k, out, &mut self.acc);
            self.time.fill(0.0);
            if self
                .inverse
                .process_with_scratch(&mut self.acc, &mut self.time, &mut self.scratch_i)
                .is_err()
            {
                self.time.fill(0.0);
            }
            let fading = self.old.is_some();
            if let Some(old) = &self.old {
                Self::accumulate(old, &self.fdl, self.head, k, out, &mut self.acc_old);
                if self
                    .inverse
                    .process_with_scratch(
                        &mut self.acc_old,
                        &mut self.time_old,
                        &mut self.scratch_i,
                    )
                    .is_err()
                {
                    self.time_old.fill(0.0);
                }
            }
            for (i, o) in self.output[out].iter_mut().enumerate() {
                let y = self.time[PARTITION + i] * norm;
                *o = if fading {
                    let t = (i as f64 + 0.5) / PARTITION as f64;
                    self.time_old[PARTITION + i] * norm * (1.0 - t) + y * t
                } else {
                    y
                };
            }
        }
        if let Some(old) = self.old.take() {
            self.held = Some(old);
        }
    }

    /// `Σ` over partitions and inputs of input spectra times kernel spectra
    /// into `acc`, for output channel `out`.
    fn accumulate(
        kernels: &Kernels,
        fdl: &[Vec<Complex<f64>>; 2],
        head: usize,
        k: usize,
        out: usize,
        acc: &mut [Complex<f64>],
    ) {
        let bins = PARTITION + 1;
        acc.fill(Complex::new(0.0, 0.0));
        for (input, line) in fdl.iter().enumerate() {
            let Some(h) = &kernels.paths[out * 2 + input] else {
                continue;
            };
            for p in 0..k {
                let x = &line[((head + p) % k) * bins..][..bins];
                let hk = &h[p * bins..][..bins];
                for ((a, xv), hv) in acc.iter_mut().zip(x).zip(hk) {
                    *a += xv * hv;
                }
            }
        }
        // The inverse transform needs real DC and Nyquist bins.
        acc[0].im = 0.0;
        acc[bins - 1].im = 0.0;
    }

    pub fn reset(&mut self) {
        for c in 0..2 {
            self.window[c].fill(0.0);
            self.fdl[c].fill(Complex::new(0.0, 0.0));
            self.input[c].fill(0.0);
            self.output[c].fill(0.0);
        }
        self.fill = 0;
    }
}
