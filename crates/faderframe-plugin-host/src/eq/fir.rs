//! The EQ's FIRs: a stereo 2 × 2 system (left to left, right to left, …)
//! run as a uniformly partitioned convolution, its kernels designed on a
//! thread of their own.
//!
//! The design thread reads the live parameters (automation included) and
//! hands each new set of kernels to the audio thread through a mailbox; the
//! audio thread crossfades to it over one partition and hands the old set
//! back to be freed. Nothing on the audio thread allocates.

use super::{linear, natural};
use crate::ParamValues;
use faderframe_realtime::{MailboxReceiver, MailboxSender, mailbox};
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// What the FIR does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The static bands, linear phase, `n` taps.
    Linear(usize),
    /// The natural phase correction after the minimum phase sections.
    Natural,
}

impl Kind {
    fn length(self) -> usize {
        match self {
            Kind::Linear(n) => n,
            Kind::Natural => natural::LENGTH,
        }
    }

    fn partition(self) -> usize {
        match self {
            Kind::Linear(_) => linear::PARTITION,
            Kind::Natural => natural::PARTITION,
        }
    }

    /// The delay through the FIR (its main tap and one partition).
    fn latency(self) -> usize {
        match self {
            Kind::Linear(n) => n / 2 + linear::PARTITION,
            Kind::Natural => natural::PRE + natural::PARTITION,
        }
    }

    /// A snapshot of what the kernels depend on.
    fn key(self, params: &ParamValues) -> Vec<u32> {
        match self {
            Kind::Linear(_) => linear::key(params),
            Kind::Natural => natural::key(params),
        }
    }

    /// The kernels as impulse responses of [`Kind::length`] taps per path:
    /// left to left, right to left, left to right, right to right (`None`:
    /// silent).
    fn impulses(
        self,
        params: &ParamValues,
        rate: f64,
        planner: &mut RealFftPlanner<f64>,
    ) -> [Option<Vec<f64>>; 4] {
        match self {
            Kind::Linear(n) => linear::impulses(params, rate, n, planner),
            Kind::Natural => natural::impulses(params, rate, planner),
        }
    }
}

/// The partitioned spectra of the four paths.
pub(crate) struct Kernels {
    paths: [Option<Vec<Complex<f64>>>; 4],
}

impl Kernels {
    fn design(
        kind: Kind,
        params: &ParamValues,
        rate: f64,
        planner: &mut RealFftPlanner<f64>,
    ) -> Self {
        let p = kind.partition();
        let impulses = kind.impulses(params, rate, planner);
        Self {
            paths: impulses.map(|h| h.map(|h| partitioned(&h, p, planner))),
        }
    }
}

/// An impulse response cut into partitions of `p` and transformed.
fn partitioned(h: &[f64], p: usize, planner: &mut RealFftPlanner<f64>) -> Vec<Complex<f64>> {
    let forward = planner.plan_fft_forward(2 * p);
    let k = h.len().div_ceil(p);
    let mut out = Vec::with_capacity(k * (p + 1));
    let mut block = vec![0.0; 2 * p];
    let mut spec = forward.make_output_vec();
    for i in 0..k {
        block.fill(0.0);
        let part = &h[i * p..((i + 1) * p).min(h.len())];
        block[..part.len()].copy_from_slice(part);
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

/// The audio thread's half: the partitioned convolution.
pub(crate) struct Convolver {
    kind: Kind,
    p: usize,
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

impl Convolver {
    /// Set up for the parameters at `rate` (control thread): the first
    /// kernels are designed here, later ones on the design thread.
    pub(crate) fn new(kind: Kind, params: &ParamValues, rate: f64) -> Self {
        let p = kind.partition();
        let k = kind.length().div_ceil(p);
        let mut planner = RealFftPlanner::<f64>::new();
        let forward = planner.plan_fft_forward(2 * p);
        let inverse = planner.plan_fft_inverse(2 * p);
        let kernels = Box::new(Kernels::design(kind, params, rate, &mut planner));
        let (tx, rx) = mailbox::<Kernels>();
        let (retired_tx, mut retired_rx) = rtrb::RingBuffer::<Box<Kernels>>::new(8);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = Arc::clone(&stop);
            let params = params.clone();
            let mut last = kind.key(&params);
            std::thread::Builder::new()
                .name("faderframe-eq-design".into())
                .spawn(move || {
                    let mut planner = RealFftPlanner::<f64>::new();
                    let tx: MailboxSender<Kernels> = tx;
                    while !stop.load(Ordering::Relaxed) {
                        while let Ok(old) = retired_rx.pop() {
                            drop(old);
                        }
                        let now = kind.key(&params);
                        if now != last {
                            last = now;
                            let kernels = Kernels::design(kind, &params, rate, &mut planner);
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
            kind,
            p,
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
            window: [vec![0.0; 2 * p], vec![0.0; 2 * p]],
            fdl: [
                vec![Complex::new(0.0, 0.0); k * (p + 1)],
                vec![Complex::new(0.0, 0.0); k * (p + 1)],
            ],
            head: 0,
            fill: 0,
            input: [vec![0.0; p], vec![0.0; p]],
            output: [vec![0.0; p], vec![0.0; p]],
            acc: vec![Complex::new(0.0, 0.0); p + 1],
            acc_old: vec![Complex::new(0.0, 0.0); p + 1],
            time: vec![0.0; 2 * p],
            time_old: vec![0.0; 2 * p],
            spec,
            _designer: Designer { stop, handle },
        }
    }

    pub(crate) fn latency(&self) -> u32 {
        self.kind.latency() as u32
    }

    /// Filter one frame (in place).
    #[inline]
    pub(crate) fn process(&mut self, l: &mut f64, r: &mut f64) {
        let i = self.fill;
        self.input[0][i] = *l;
        self.input[1][i] = *r;
        *l = self.output[0][i];
        *r = self.output[1][i];
        self.fill += 1;
        if self.fill == self.p {
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
        let p = self.p;
        let bins = p + 1;
        let k = self.k;
        self.head = (self.head + k - 1) % k;
        for c in 0..2 {
            let w = &mut self.window[c];
            w.copy_within(p.., 0);
            w[p..].copy_from_slice(&self.input[c]);
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
        let norm = 1.0 / (2 * p) as f64;
        for out in 0..2 {
            Self::accumulate(
                &self.kernels,
                &self.fdl,
                self.head,
                k,
                p,
                out,
                &mut self.acc,
            );
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
                Self::accumulate(old, &self.fdl, self.head, k, p, out, &mut self.acc_old);
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
                let y = self.time[p + i] * norm;
                *o = if fading {
                    let t = (i as f64 + 0.5) / p as f64;
                    self.time_old[p + i] * norm * (1.0 - t) + y * t
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
        p: usize,
        out: usize,
        acc: &mut [Complex<f64>],
    ) {
        let bins = p + 1;
        acc.fill(Complex::new(0.0, 0.0));
        for (input, line) in fdl.iter().enumerate() {
            let Some(h) = &kernels.paths[out * 2 + input] else {
                continue;
            };
            let parts = (h.len() / bins).min(k);
            for part in 0..parts {
                let x = &line[((head + part) % k) * bins..][..bins];
                let hk = &h[part * bins..][..bins];
                for ((a, xv), hv) in acc.iter_mut().zip(x).zip(hk) {
                    *a += xv * hv;
                }
            }
        }
        // The inverse transform needs real DC and Nyquist bins.
        acc[0].im = 0.0;
        acc[bins - 1].im = 0.0;
    }

    pub(crate) fn reset(&mut self) {
        for c in 0..2 {
            self.window[c].fill(0.0);
            self.fdl[c].fill(Complex::new(0.0, 0.0));
            self.input[c].fill(0.0);
            self.output[c].fill(0.0);
        }
        self.fill = 0;
    }
}
