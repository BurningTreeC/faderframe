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
///
/// The work is spread over each partition's samples: the products of the
/// older partitions (1 … k − 1) for the next block use input spectra known
/// already, so they are summed a few at a time as samples arrive
/// (`prepare`); the block boundary adds partition 0 (the newest input) and
/// transforms back. A new set of kernels is prepared the same way for one
/// block before it is crossfaded in. The boundary keeps only the
/// transforms: no burst of every partition's products on one sample.
pub(crate) struct Convolver {
    kind: Kind,
    p: usize,
    k: usize,
    forward: Arc<dyn RealToComplex<f64>>,
    inverse: Arc<dyn ComplexToReal<f64>>,
    kernels: Box<Kernels>,
    /// A new set, prepared during this block, crossfaded in at its end.
    pending: Option<Box<Kernels>>,
    incoming: MailboxReceiver<Kernels>,
    retired: rtrb::Producer<Box<Kernels>>,
    held: Option<Box<Kernels>>,
    /// Per input channel: the last two partitions of input, and the
    /// frequency domain delay line of their spectra.
    window: [Vec<f64>; 2],
    fdl: [Vec<Complex<f64>>; 2],
    head: usize,
    /// Partitions written into `fdl` since the last reset (up to `k`):
    /// older ones are silence, skipped — a reset clears nothing there (it
    /// is megabytes at the highest resolutions).
    fresh: usize,
    /// Samples collected for the next block, and the last block's output.
    fill: usize,
    input: [Vec<f64>; 2],
    output: [Vec<f64>; 2],
    /// The older partitions' products for the next block, per output
    /// (current kernels, then the pending set's).
    pre: [Vec<Complex<f64>>; 2],
    pre_new: [Vec<Complex<f64>>; 2],
    /// Product units summed so far in this block, and in all.
    units_done: usize,
    units: usize,
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
        let zeros = || vec![Complex::new(0.0, 0.0); p + 1];
        Self {
            kind,
            p,
            k,
            scratch_f: forward.make_scratch_vec(),
            scratch_i: inverse.make_scratch_vec(),
            forward,
            inverse,
            kernels,
            pending: None,
            incoming: rx,
            retired: retired_tx,
            held: None,
            window: [vec![0.0; 2 * p], vec![0.0; 2 * p]],
            fdl: [
                vec![Complex::new(0.0, 0.0); k * (p + 1)],
                vec![Complex::new(0.0, 0.0); k * (p + 1)],
            ],
            head: 0,
            fresh: 0,
            fill: 0,
            input: [vec![0.0; p], vec![0.0; p]],
            output: [vec![0.0; p], vec![0.0; p]],
            pre: [zeros(), zeros()],
            pre_new: [zeros(), zeros()],
            units_done: 0,
            units: Self::units_for(k, false),
            acc: zeros(),
            acc_old: zeros(),
            time: vec![0.0; 2 * p],
            time_old: vec![0.0; 2 * p],
            spec,
            _designer: Designer { stop, handle },
        }
    }

    pub(crate) fn latency(&self) -> u32 {
        self.kind.latency() as u32
    }

    /// Product units in a block: per kernel set being summed, per output
    /// and input, the older partitions.
    fn units_for(k: usize, pending: bool) -> usize {
        (1 + usize::from(pending)) * 4 * k.saturating_sub(1)
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
        // This block's share of the older partitions' products so far.
        let due = self.units * self.fill / self.p;
        self.prepare(due);
        if self.fill == self.p {
            self.fill = 0;
            self.block();
        }
    }

    /// Sum product units until `due` of this block's are done.
    fn prepare(&mut self, due: usize) {
        let (p, k) = (self.p, self.k);
        let bins = p + 1;
        let per_set = 4 * k.saturating_sub(1);
        while self.units_done < due.min(self.units) {
            let u = self.units_done;
            self.units_done += 1;
            let (set, rest) = (u / per_set.max(1), u % per_set.max(1));
            let (path, part) = (rest / (k - 1), 1 + rest % (k - 1));
            let (out, input) = (path / 2, path % 2);
            let (kernels, pre) = if set == 0 {
                (&*self.kernels, &mut self.pre[out])
            } else {
                match &self.pending {
                    Some(n) => (&**n, &mut self.pre_new[out]),
                    None => continue,
                }
            };
            let Some(h) = &kernels.paths[out * 2 + input] else {
                continue;
            };
            if part >= h.len() / bins {
                continue;
            }
            // Next block's partition `part`: the spectrum `part - 1`
            // partitions older than the newest one now (silence from
            // before a reset).
            if part > self.fresh {
                continue;
            }
            let x = &self.fdl[input][((self.head + part - 1) % k) * bins..][..bins];
            let hk = &h[part * bins..][..bins];
            for ((a, xv), hv) in pre.iter_mut().zip(x).zip(hk) {
                *a += xv * hv;
            }
        }
    }

    /// One partition in, one out.
    fn block(&mut self) {
        // Anything not summed yet (a block cut short by a reset).
        self.prepare(self.units);
        // Hand the previous set back.
        if let Some(h) = self.held.take()
            && let Err(rtrb::PushError::Full(h)) = self.retired.push(h)
        {
            self.held = Some(h);
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
        self.fresh = (self.fresh + 1).min(k);
        let norm = 1.0 / (2 * p) as f64;
        // The prepared set (if any) fades in over this block.
        let fading = self.pending.is_some();
        for out in 0..2 {
            Self::newest(
                &self.kernels,
                &self.fdl,
                self.head,
                p,
                out,
                &self.pre[out],
                &mut self.acc,
            );
            if self
                .inverse
                .process_with_scratch(&mut self.acc, &mut self.time, &mut self.scratch_i)
                .is_err()
            {
                self.time.fill(0.0);
            }
            if let Some(new) = &self.pending {
                Self::newest(
                    new,
                    &self.fdl,
                    self.head,
                    p,
                    out,
                    &self.pre_new[out],
                    &mut self.acc_old,
                );
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
                let current = self.time[p + i] * norm;
                *o = if fading {
                    let t = (i as f64 + 0.5) / p as f64;
                    current * (1.0 - t) + self.time_old[p + i] * norm * t
                } else {
                    current
                };
            }
        }
        if let Some(new) = self.pending.take() {
            self.held = Some(std::mem::replace(&mut self.kernels, new));
        }
        // A new set arriving: prepared during the next block.
        if self.held.is_none() {
            self.pending = self.incoming.take();
        }
        for pre in self.pre.iter_mut().chain(self.pre_new.iter_mut()) {
            pre.fill(Complex::new(0.0, 0.0));
        }
        self.units_done = 0;
        self.units = Self::units_for(k, self.pending.is_some());
    }

    /// `pre` plus the newest partition's products for output `out`, into
    /// `acc` (ready for the inverse transform).
    fn newest(
        kernels: &Kernels,
        fdl: &[Vec<Complex<f64>>; 2],
        head: usize,
        p: usize,
        out: usize,
        pre: &[Complex<f64>],
        acc: &mut [Complex<f64>],
    ) {
        let bins = p + 1;
        acc.copy_from_slice(pre);
        for (input, line) in fdl.iter().enumerate() {
            let Some(h) = &kernels.paths[out * 2 + input] else {
                continue;
            };
            let x = &line[head * bins..][..bins];
            for ((a, xv), hv) in acc.iter_mut().zip(x).zip(&h[..bins]) {
                *a += xv * hv;
            }
        }
        // The inverse transform needs real DC and Nyquist bins.
        acc[0].im = 0.0;
        acc[bins - 1].im = 0.0;
    }

    pub(crate) fn reset(&mut self) {
        for c in 0..2 {
            self.window[c].fill(0.0);
            self.input[c].fill(0.0);
            self.output[c].fill(0.0);
        }
        // The spectra are forgotten, not cleared.
        self.fresh = 0;
        for pre in self.pre.iter_mut().chain(self.pre_new.iter_mut()) {
            pre.fill(Complex::new(0.0, 0.0));
        }
        self.units_done = 0;
        self.units = Self::units_for(self.k, self.pending.is_some());
        self.fill = 0;
    }
}
