//! Convolution without latency, its work spread evenly over the samples.
//!
//! A response is cut the way Gardner proposed (*Efficient Convolution
//! without Input-Output Delay*, 1995): its first [`HEAD`] taps are
//! convolved directly, sample by sample; the rest goes to stages of
//! partitions growing eightfold (32, 256, 2048, 16 384 samples), each a
//! uniformly partitioned overlap-save convolution that starts two of its
//! partitions into the response. That start is what keeps it on time
//! without a burst: a stage collects a partition of input, and the output
//! that input makes is not due before another partition has passed — so
//! the stage's work for it (the input's transform, the products with every
//! partition of the response, the inverse transforms) is done a little at
//! a time over the next partition's samples, in units, as they arrive.
//! Every stage's first tap is where the stage before it ends:
//! `64 + 14·32 = 512 = 2·256`, and so on; the last stage takes any number
//! of partitions.
//!
//! An [`Engine`] convolves any set of paths (an input into an output) — a
//! reverb's two or four (left to left, left to right, …), a linear-phase
//! crossover's one per band and channel — at a capacity fixed when it is
//! made (the frequency-domain delay lines hold that much input history).
//! New [`Kernels`] for the same paths (a response shortened, faded,
//! stretched) are taken in without losing that history: each stage
//! computes its next block with the old and the new kernels and crossfades
//! between them over the block after, the head over [`CHUNK`] samples;
//! once every stage has gone over, the old set is handed back
//! ([`Engine::take_retired`]) to be freed off the audio thread. Kernels are
//! made where allocating is allowed ([`Kernels::new`]); nothing the engine
//! does once made allocates.
//!
//! Reset forgets rather than clears (the panic rules): the history counts
//! as silence from the reset on, the outputs as not computed yet.

use faderframe_realtime::{MailboxSender, mailbox};
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Taps convolved directly.
pub const HEAD: usize = 64;
/// The engine runs in pieces of at most this many frames (the smallest
/// partition): [`Engine::room`].
pub const CHUNK: usize = 32;

/// The partitioned stages: partition length, the response's first tap it
/// takes, and the most partitions it holds (the last: any number).
const STAGES: [(usize, usize, usize); 4] = [
    (32, 64, 14),
    (256, 512, 14),
    (2_048, 4_096, 14),
    (16_384, 32_768, usize::MAX),
];

type C32 = Complex<f32>;

const ZERO: C32 = Complex { re: 0.0, im: 0.0 };

/// Which input feeds which output, path by path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Routing {
    pub inputs: usize,
    pub outputs: usize,
    /// (input, output) per path, in the order the kernels' responses come.
    pub paths: Vec<(usize, usize)>,
}

impl Routing {
    pub fn new(inputs: usize, outputs: usize, paths: Vec<(usize, usize)>) -> Self {
        Self {
            inputs,
            outputs,
            paths,
        }
    }

    /// Left to left and right to right (a mono or a stereo response).
    pub fn parallel() -> Self {
        Self::new(2, 2, vec![(0, 0), (1, 1)])
    }

    /// Each input to both outputs (a true-stereo response: left to left,
    /// left to right, right to left, right to right).
    pub fn true_stereo() -> Self {
        Self::new(2, 2, vec![(0, 0), (0, 1), (1, 0), (1, 1)])
    }
}

/// How many partitions stage `s` needs for a response of `taps`.
fn parts_for(s: usize, taps: usize) -> usize {
    let (p, d, most) = STAGES[s];
    if taps <= d {
        0
    } else {
        (taps - d).div_ceil(p).min(most)
    }
}

/// A response made ready for an [`Engine`]: per path, the head (reversed
/// for a dot product with the input history) and each stage's partitions
/// transformed (and scaled for the inverse transform).
pub struct Kernels {
    /// Its owner's name for the engine it was made for.
    tag: u64,
    taps: usize,
    head: Vec<[f32; HEAD]>,
    /// Per stage: its partitions and, per path, their spectra one after
    /// another (`parts × (P + 1)`).
    stages: Vec<(usize, Vec<Vec<C32>>)>,
}

impl Kernels {
    /// The responses of a routing's paths, in its order (made off the
    /// audio thread).
    pub fn new(impulses: &[&[f32]], planner: &mut RealFftPlanner<f32>) -> Self {
        let taps = impulses.iter().map(|h| h.len()).max().unwrap_or(0);
        let head = impulses
            .iter()
            .map(|h| {
                let mut r = [0.0; HEAD];
                for (k, v) in h.iter().take(HEAD).enumerate() {
                    r[HEAD - 1 - k] = *v;
                }
                r
            })
            .collect();
        let stages = (0..STAGES.len())
            .map(|s| {
                let (p, d, _) = STAGES[s];
                let parts = parts_for(s, taps);
                if parts == 0 {
                    return (0, Vec::new());
                }
                let forward = planner.plan_fft_forward(2 * p);
                let mut time = vec![0.0f32; 2 * p];
                let mut spec = forward.make_output_vec();
                let norm = 1.0 / (2 * p) as f32;
                let spectra = impulses
                    .iter()
                    .map(|h| {
                        let mut out = Vec::with_capacity(parts * (p + 1));
                        for i in 0..parts {
                            time.fill(0.0);
                            let a = (d + i * p).min(h.len());
                            let b = (d + (i + 1) * p).min(h.len());
                            time[..b - a].copy_from_slice(&h[a..b]);
                            if forward.process(&mut time, &mut spec).is_err() {
                                spec.fill(ZERO);
                            }
                            out.extend(spec.iter().map(|c| c * norm));
                        }
                        out
                    })
                    .collect();
                (parts, spectra)
            })
            .collect();
        Self {
            tag: 0,
            taps,
            head,
            stages,
        }
    }

    /// The same, named for the engine `tag` (see [`Engine::tagged`]).
    pub fn tagged(mut self, tag: u64) -> Self {
        self.tag = tag;
        self
    }

    pub fn tag(&self) -> u64 {
        self.tag
    }

    /// Nothing: every path silent.
    pub fn silent(paths: usize) -> Self {
        Self {
            tag: 0,
            taps: 0,
            head: vec![[0.0; HEAD]; paths],
            stages: vec![(0, Vec::new()); STAGES.len()],
        }
    }

    /// The longest path's length.
    pub fn taps(&self) -> usize {
        self.taps
    }

    fn paths(&self) -> usize {
        self.head.len()
    }

    fn parts(&self, stage: usize) -> usize {
        self.stages[stage].0
    }
}

/// A block's work in a stage.
#[derive(Clone, Copy, Debug, Default)]
struct Job {
    active: bool,
    /// The engine's frame when it started.
    start: u64,
    done: usize,
    total: usize,
    /// One kernel set, or the old and the new while switching.
    dual: bool,
    /// A single set's kernels are the incoming ones.
    new: bool,
    /// The block before and the block just completed (in `blocks`).
    prev: usize,
    cur: usize,
    /// The first block since the reset (nothing before it).
    first: bool,
}

/// One partitioned stage.
struct Stage {
    /// Its place in [`STAGES`].
    index: usize,
    p: usize,
    /// Partitions of history it holds.
    cap: usize,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    /// Three blocks of input per input: the one filling and the two the
    /// running job reads.
    blocks: [Vec<Vec<f32>>; 3],
    filling: usize,
    /// Per input, the spectra of the last `cap` windows.
    fdl: Vec<Vec<C32>>,
    newest: usize,
    /// Spectra written since the reset (older ones are silence).
    fresh: usize,
    /// A block was taken since the reset.
    started: bool,
    job: Job,
    /// Per kernel set (old, new) and output: the products summed, the
    /// block computed, the block playing.
    acc: [Vec<Vec<C32>>; 2],
    next: [Vec<Vec<f32>>; 2],
    current: [Vec<Vec<f32>>; 2],
    /// `current` holds a computed block (not since the reset).
    playing: bool,
    /// The block playing crossfades from the old kernels' to the new.
    fading: bool,
    /// Switching kernels: 0 on the old, 1 computing both, 2 crossfading,
    /// 3 on the new.
    phase: u8,
    time: Vec<f32>,
    spec: Vec<C32>,
    fscratch: Vec<C32>,
    iscratch: Vec<C32>,
}

impl Stage {
    fn new(index: usize, cap: usize, routing: &Routing, planner: &mut RealFftPlanner<f32>) -> Self {
        let p = STAGES[index].0;
        let forward = planner.plan_fft_forward(2 * p);
        let inverse = planner.plan_fft_inverse(2 * p);
        let per_input = || vec![vec![0.0f32; p]; routing.inputs];
        let per_output = || vec![vec![0.0f32; p]; routing.outputs];
        let spectra = || vec![vec![ZERO; p + 1]; routing.outputs];
        Self {
            index,
            p,
            cap,
            blocks: [per_input(), per_input(), per_input()],
            filling: 0,
            fdl: vec![vec![ZERO; cap * (p + 1)]; routing.inputs],
            newest: 0,
            fresh: 0,
            started: false,
            job: Job::default(),
            acc: [spectra(), spectra()],
            next: [per_output(), per_output()],
            current: [per_output(), per_output()],
            playing: false,
            fading: false,
            phase: 0,
            time: vec![0.0; 2 * p],
            spec: forward.make_output_vec(),
            fscratch: forward.make_scratch_vec(),
            iscratch: inverse.make_scratch_vec(),
            forward,
            inverse,
        }
    }

    fn reset(&mut self) {
        self.job = Job::default();
        self.playing = false;
        self.fading = false;
        self.fresh = 0;
        self.started = false;
        self.filling = 0;
    }
}

/// The paths into each output (indices into the routing's paths).
fn by_output(routing: &Routing) -> Vec<Vec<usize>> {
    (0..routing.outputs)
        .map(|o| {
            routing
                .paths
                .iter()
                .enumerate()
                .filter(|(_, (_, out))| *out == o)
                .map(|(i, _)| i)
                .collect()
        })
        .collect()
}

/// The convolution (see the module docs).
pub struct Engine {
    /// Its owner's name for it.
    tag: u64,
    routing: Routing,
    by_output: Vec<Vec<usize>>,
    capacity: usize,
    /// Frames run since the start (or the reset).
    at: u64,
    /// Per input: the last [`HEAD`] samples, then the piece being run.
    hist: Vec<Vec<f32>>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    /// The head's output with the incoming kernels (while crossfading).
    head_new: Vec<f32>,
    kernels: Box<Kernels>,
    incoming: Option<Box<Kernels>>,
    /// The head's crossfade: samples done of [`CHUNK`] (`None`: not begun).
    head_fade: Option<usize>,
    head_switched: bool,
    stages: Vec<Stage>,
    retired: Option<Box<Kernels>>,
}

impl Engine {
    /// For `routing`, holding responses up to `capacity` taps, starting
    /// with `kernels` (made off the audio thread).
    pub fn new(
        routing: Routing,
        capacity: usize,
        kernels: Box<Kernels>,
        planner: &mut RealFftPlanner<f32>,
    ) -> Self {
        let capacity = capacity.max(kernels.taps());
        let stages = (0..STAGES.len())
            .filter_map(|s| {
                let cap = parts_for(s, capacity);
                (cap > 0).then(|| Stage::new(s, cap, &routing, planner))
            })
            .collect();
        Self {
            tag: 0,
            by_output: by_output(&routing),
            capacity,
            at: 0,
            hist: vec![vec![0.0; HEAD + CHUNK]; routing.inputs],
            input: vec![vec![0.0; CHUNK]; routing.inputs],
            output: vec![vec![0.0; CHUNK]; routing.outputs],
            head_new: vec![0.0; CHUNK],
            kernels,
            incoming: None,
            head_fade: None,
            head_switched: false,
            stages,
            retired: None,
            routing,
        }
    }

    /// An engine for `routing` that holds nothing and plays silence.
    pub fn silent(routing: Routing, planner: &mut RealFftPlanner<f32>) -> Self {
        let paths = routing.paths.len();
        Self::new(routing, 0, Box::new(Kernels::silent(paths)), planner)
    }

    /// The same, named `tag` by its owner (which engine is playing).
    pub fn tagged(mut self, tag: u64) -> Self {
        self.tag = tag;
        self
    }

    pub fn tag(&self) -> u64 {
        self.tag
    }

    pub fn routing(&self) -> &Routing {
        &self.routing
    }

    /// The longest response it holds (taps).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The response playing (or being switched to): its length.
    pub fn taps(&self) -> usize {
        self.incoming
            .as_ref()
            .map_or(self.kernels.taps(), |k| k.taps())
    }

    /// Whether `kernels` fit (its paths and length).
    pub fn fits(&self, kernels: &Kernels) -> bool {
        kernels.paths() == self.routing.paths.len() && kernels.taps() <= self.capacity
    }

    /// Whether new kernels are being taken in (another waits until done).
    pub fn switching(&self) -> bool {
        self.incoming.is_some()
    }

    /// Take `kernels` in (crossfaded, the history kept). Refused (handed
    /// back) while switching or when they do not fit.
    pub fn offer(&mut self, kernels: Box<Kernels>) -> Result<(), Box<Kernels>> {
        if self.switching() || !self.fits(&kernels) {
            return Err(kernels);
        }
        self.incoming = Some(kernels);
        self.head_fade = None;
        self.head_switched = false;
        for s in &mut self.stages {
            s.phase = 0;
        }
        Ok(())
    }

    /// Kernels it has let go of, to be freed off the audio thread.
    pub fn take_retired(&mut self) -> Option<Box<Kernels>> {
        self.retired.take()
    }

    /// Frames it can run now (to the next [`CHUNK`] boundary).
    pub fn room(&self) -> usize {
        CHUNK - (self.at % CHUNK as u64) as usize
    }

    /// Where the next piece's input goes (`CHUNK` long; the first
    /// [`Self::room`] frames are used).
    pub fn input_mut(&mut self, input: usize) -> &mut [f32] {
        &mut self.input[input]
    }

    /// The last piece's output.
    pub fn output(&self, output: usize) -> &[f32] {
        &self.output[output]
    }

    /// Forget everything heard (silence from here, no tail).
    pub fn reset(&mut self) {
        self.at = 0;
        for h in &mut self.hist {
            h.fill(0.0);
        }
        for s in &mut self.stages {
            s.reset();
        }
        // A switch under way completes at once: nothing to crossfade.
        if self.incoming.is_some() {
            self.head_fade = None;
            self.head_switched = true;
            for s in &mut self.stages {
                s.phase = 3;
            }
        }
    }

    /// Run `len` frames (at most [`Self::room`]) of the input written into
    /// [`Self::input_mut`]; the output is in [`Self::output`].
    pub fn run(&mut self, len: usize) {
        let len = len.min(self.room());
        if len == 0 {
            return;
        }
        let at = self.at;
        // Blocks completed: finish what was due, start the next.
        for si in 0..self.stages.len() {
            if at.is_multiple_of(self.stages[si].p as u64) {
                self.boundary(si);
            }
        }
        for o in &mut self.output {
            o[..len].fill(0.0);
        }
        self.head(len);
        // The stages: input in, the block playing out.
        for s in &mut self.stages {
            let r = (at % s.p as u64) as usize;
            for (i, input) in self.input.iter().enumerate() {
                s.blocks[s.filling][i][r..r + len].copy_from_slice(&input[..len]);
            }
            if !s.playing {
                continue;
            }
            for (o, out) in self.output.iter_mut().enumerate() {
                if self.by_output[o].is_empty() {
                    continue;
                }
                let cur = &s.current[0][o][r..r + len];
                if s.fading {
                    let new = &s.current[1][o][r..r + len];
                    let step = 1.0 / s.p as f32;
                    for (j, ((y, a), b)) in out[..len].iter_mut().zip(cur).zip(new).enumerate() {
                        let t = ((r + j) as f32 + 0.5) * step;
                        *y += a + (b - a) * t;
                    }
                } else {
                    for (y, a) in out[..len].iter_mut().zip(cur) {
                        *y += a;
                    }
                }
            }
        }
        self.at += len as u64;
        // This piece's share of each stage's work.
        for si in 0..self.stages.len() {
            let s = &self.stages[si];
            if !s.job.active {
                continue;
            }
            let elapsed = (self.at - s.job.start) as usize;
            let due = (s.job.total * elapsed).div_ceil(s.p);
            self.work(si, due);
        }
        self.finish_switch();
    }

    /// The direct-form head (and its crossfade to new kernels).
    fn head(&mut self, len: usize) {
        for (h, input) in self.hist.iter_mut().zip(&self.input) {
            h[HEAD..HEAD + len].copy_from_slice(&input[..len]);
        }
        let fading = match (&self.incoming, self.head_switched) {
            (Some(_), false) => {
                let done = *self.head_fade.get_or_insert(0);
                Some(done)
            }
            _ => None,
        };
        let current: &Kernels = if self.head_switched {
            self.incoming.as_deref().unwrap_or(&self.kernels)
        } else {
            &self.kernels
        };
        for (o, out) in self.output.iter_mut().enumerate() {
            for &pi in &self.by_output[o] {
                let (i, _) = self.routing.paths[pi];
                let h = &current.head[pi];
                let x = &self.hist[i];
                for (j, y) in out[..len].iter_mut().enumerate() {
                    *y += dot(h, &x[j + 1..j + 1 + HEAD]);
                }
            }
        }
        if let (Some(done), Some(new)) = (fading, self.incoming.as_deref()) {
            // The new head's output, crossfaded in over CHUNK samples.
            for (o, out) in self.output.iter_mut().enumerate() {
                let paths = &self.by_output[o];
                if paths.is_empty() {
                    continue;
                }
                let n = &mut self.head_new[..len];
                n.fill(0.0);
                for &pi in paths {
                    let (i, _) = self.routing.paths[pi];
                    let x = &self.hist[i];
                    let old = &self.kernels.head[pi];
                    let h = &new.head[pi];
                    for (j, y) in n.iter_mut().enumerate() {
                        let w = &x[j + 1..j + 1 + HEAD];
                        *y += dot(h, w) - dot(old, w);
                    }
                }
                for (j, (y, d)) in out[..len].iter_mut().zip(n.iter()).enumerate() {
                    let t = ((done + j) as f32 + 0.5) / CHUNK as f32;
                    *y += d * t.min(1.0);
                }
            }
            let done = done + len;
            if done >= CHUNK {
                self.head_fade = None;
                self.head_switched = true;
            } else {
                self.head_fade = Some(done);
            }
        }
        for h in &mut self.hist {
            h.copy_within(len..len + HEAD, 0);
        }
    }

    /// A stage's block boundary: the job that was due is finished and its
    /// block plays; a job for the block just completed starts.
    fn boundary(&mut self, si: usize) {
        let total = self.stages[si].job.total;
        if self.stages[si].job.active {
            self.work(si, total);
        }
        let switching = self.incoming.is_some();
        let index = self.stages[si].index;
        let routing_inputs = self.routing.inputs;
        let outputs = self.routing.outputs;
        let parts_old = self.kernels.parts(index);
        let parts_new = self.incoming.as_ref().map_or(0, |k| k.parts(index));
        let s = &mut self.stages[si];
        if s.job.active {
            s.job.active = false;
            std::mem::swap(&mut s.current[0], &mut s.next[0]);
            if s.job.dual {
                std::mem::swap(&mut s.current[1], &mut s.next[1]);
            }
            s.fading = s.job.dual;
            s.playing = true;
        } else {
            s.playing = false;
            s.fading = false;
        }
        // Switching kernels: compute both, crossfade, then the new only.
        let (dual, new) = if switching {
            match s.phase {
                0 if parts_old == 0 && parts_new == 0 => {
                    s.phase = 3;
                    (false, true)
                }
                0 => {
                    s.phase = 1;
                    (true, false)
                }
                1 => {
                    s.phase = 2;
                    (false, true)
                }
                _ => {
                    s.phase = 3;
                    (false, true)
                }
            }
        } else {
            (false, false)
        };
        if s.phase == 2 && !s.fading {
            // The dual block never played (no job ran): nothing to fade.
            s.phase = 3;
        }
        if self.at < s.p as u64 {
            // No block completed yet.
            if dual {
                s.phase = 0;
            }
            return;
        }
        let parts = |use_new: bool| if use_new { parts_new } else { parts_old };
        let mac = if dual {
            outputs * (parts_old + parts_new)
        } else {
            outputs * parts(new)
        };
        let sets = if dual { 2 } else { 1 };
        s.job = Job {
            active: true,
            start: self.at,
            done: 0,
            total: routing_inputs + mac + sets * outputs,
            dual,
            new,
            prev: (s.filling + 2) % 3,
            cur: s.filling,
            first: !s.started,
        };
        s.started = true;
        s.filling = (s.filling + 1) % 3;
        s.newest = (s.newest + 1) % s.cap;
        s.fresh = (s.fresh + 1).min(s.cap);
        for set in &mut s.acc[..sets] {
            for a in set.iter_mut() {
                a.fill(ZERO);
            }
        }
    }

    /// Run a stage's job until `due` of its units are done.
    fn work(&mut self, si: usize, due: usize) {
        let s = &mut self.stages[si];
        let due = due.min(s.job.total);
        let inputs = self.routing.inputs;
        let outputs = self.routing.outputs;
        let index = s.index;
        let old = &*self.kernels;
        let incoming = self.incoming.as_deref();
        while s.job.done < due {
            let mut u = s.job.done;
            s.job.done += 1;
            if u < inputs {
                forward(s, u);
                continue;
            }
            u -= inputs;
            let sets = if s.job.dual { 2 } else { 1 };
            let kernels_of = |set: usize, job: &Job| -> Option<&Kernels> {
                if set == 1 || job.new {
                    incoming
                } else {
                    Some(old)
                }
            };
            let mut handled = false;
            for set in 0..sets {
                let Some(k) = kernels_of(set, &s.job) else {
                    continue;
                };
                let parts = k.parts(index);
                let m = outputs * parts;
                if u < m {
                    mac(
                        s,
                        set,
                        u / parts,
                        u % parts,
                        k,
                        &self.routing,
                        &self.by_output,
                    );
                    handled = true;
                    break;
                }
                u -= m;
            }
            if handled {
                continue;
            }
            let (set, o) = (u / outputs, u % outputs);
            if set < sets {
                inverse(s, set, o, !self.by_output[o].is_empty());
            }
        }
    }

    /// Once the head and every stage are on the new kernels, the old set
    /// is let go.
    fn finish_switch(&mut self) {
        if self.incoming.is_none()
            || !self.head_switched
            || self.retired.is_some()
            || self.stages.iter().any(|s| s.phase != 3)
        {
            return;
        }
        if let Some(new) = self.incoming.take() {
            self.retired = Some(std::mem::replace(&mut self.kernels, new));
        }
        self.head_switched = false;
        self.head_fade = None;
        for s in &mut self.stages {
            s.phase = 0;
            // A job running on the incoming set runs on the current one now
            // (they are the same).
            s.job.new = false;
        }
    }
}

#[inline]
fn dot(a: &[f32; HEAD], b: &[f32]) -> f32 {
    // Eight running sums: the compiler keeps them in vector lanes.
    let mut acc = [0.0f32; 8];
    for (ca, cb) in a.as_chunks::<8>().0.iter().zip(b.as_chunks::<8>().0) {
        for k in 0..8 {
            acc[k] += ca[k] * cb[k];
        }
    }
    acc.iter().sum()
}

/// The input's newest window transformed into the history.
fn forward(s: &mut Stage, input: usize) {
    let p = s.p;
    let (prev, cur) = (s.job.prev, s.job.cur);
    if s.job.first {
        // The first block since the reset: nothing before it.
        s.time[..p].fill(0.0);
    } else {
        s.time[..p].copy_from_slice(&s.blocks[prev][input]);
    }
    s.time[p..].copy_from_slice(&s.blocks[cur][input]);
    if s.forward
        .process_with_scratch(&mut s.time, &mut s.spec, &mut s.fscratch)
        .is_err()
    {
        s.spec.fill(ZERO);
    }
    let at = s.newest * (p + 1);
    s.fdl[input][at..at + p + 1].copy_from_slice(&s.spec);
}

/// One output's products with one partition (every path into it).
fn mac(
    s: &mut Stage,
    set: usize,
    output: usize,
    part: usize,
    k: &Kernels,
    routing: &Routing,
    by_output: &[Vec<usize>],
) {
    if part >= s.fresh {
        // Older than the reset: silence.
        return;
    }
    let bins = s.p + 1;
    let slot = (s.newest + s.cap - part) % s.cap;
    let acc = &mut s.acc[set][output];
    for &pi in &by_output[output] {
        let (input, _) = routing.paths[pi];
        let x = &s.fdl[input][slot * bins..(slot + 1) * bins];
        let h = &k.stages[s.index].1[pi][part * bins..(part + 1) * bins];
        for ((a, xv), hv) in acc.iter_mut().zip(x).zip(h) {
            *a += xv * hv;
        }
    }
}

/// One output's block back in time.
fn inverse(s: &mut Stage, set: usize, output: usize, fed: bool) {
    let p = s.p;
    let next = &mut s.next[set][output];
    if !fed {
        next.fill(0.0);
        return;
    }
    let acc = &mut s.acc[set][output];
    // The inverse transform wants real DC and Nyquist bins.
    acc[0].im = 0.0;
    acc[p].im = 0.0;
    if s.inverse
        .process_with_scratch(acc, &mut s.time, &mut s.iscratch)
        .is_err()
    {
        next.fill(0.0);
        return;
    }
    next.copy_from_slice(&s.time[p..]);
}

/// Run an engine over whole buffers: `inputs` in, `outputs` overwritten
/// (as many as the routing has; all the same length).
pub fn run(engine: &mut Engine, inputs: &[&[f32]], outputs: &mut [&mut [f32]]) {
    let n = outputs.first().map_or(0, |o| o.len());
    let mut at = 0;
    while at < n {
        let len = engine.room().min(n - at);
        for (i, x) in inputs.iter().enumerate().take(engine.routing.inputs) {
            engine.input[i][..len].copy_from_slice(&x[at..at + len]);
        }
        engine.run(len);
        for (o, y) in outputs.iter_mut().enumerate().take(engine.routing.outputs) {
            y[at..at + len].copy_from_slice(&engine.output[o][..len]);
        }
        at += len;
    }
}

/// A thread designing kernels (or whatever its `poll` makes) for a
/// processor: every 15 ms it calls `poll`, which frees what the audio
/// thread handed back and sends what is new (through a mailbox: an older
/// value not taken yet comes back and is freed there, never on the audio
/// thread). Stopped and joined when dropped.
pub struct Designer {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Designer {
    /// Start one named `name`.
    pub fn spawn(name: &str, mut poll: impl FnMut() + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        poll();
                        std::thread::park_timeout(Duration::from_millis(15));
                    }
                })
                .ok()
        };
        Self { stop, handle }
    }
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

/// The designer's ends of [`channels`]: updates out, leftovers back.
pub type DesignerEnds<U, R> = (MailboxSender<U>, rtrb::Consumer<R>);
/// The audio thread's ends of [`channels`].
pub type AudioEnds<U, R> = (faderframe_realtime::MailboxReceiver<U>, rtrb::Producer<R>);

/// A mailbox and a ring for a [`Designer`]'s updates and their return.
pub fn channels<U: Send, R: Send>() -> (DesignerEnds<U, R>, AudioEnds<U, R>) {
    let (tx, rx) = mailbox::<U>();
    let (back_tx, back_rx) = rtrb::RingBuffer::<R>::new(8);
    ((tx, back_rx), (rx, back_tx))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direct convolution of `x` with `h` at frame `n`.
    fn direct_at(x: &[f32], h: &[f32], n: usize) -> f32 {
        let mut y = 0.0f64;
        for (k, hk) in h.iter().enumerate().take(n + 1) {
            y += f64::from(*hk) * f64::from(x[n - k]);
        }
        y as f32
    }

    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = crate::dsp::Noise::new(seed);
        (0..n).map(|_| s.tick() as f32).collect()
    }

    fn engine(routing: Routing, impulses: &[&[f32]], capacity: usize) -> Engine {
        let mut planner = RealFftPlanner::new();
        let k = Box::new(Kernels::new(impulses, &mut planner));
        Engine::new(routing, capacity, k, &mut planner)
    }

    /// Run in uneven pieces (the engine chunks them itself).
    fn run_pieces(e: &mut Engine, inputs: &[&[f32]], n: usize) -> Vec<Vec<f32>> {
        let outs = e.routing().outputs;
        let mut out = vec![vec![0.0f32; n]; outs];
        let mut at = 0;
        let sizes = [7usize, 256, 1, 33, 64, 500, 128];
        let mut k = 0;
        while at < n {
            let len = sizes[k % sizes.len()].min(n - at);
            k += 1;
            let ins: Vec<&[f32]> = inputs.iter().map(|x| &x[at..at + len]).collect();
            let mut o: Vec<&mut [f32]> = out.iter_mut().map(|y| &mut y[at..at + len]).collect();
            run(e, &ins, &mut o);
            at += len;
        }
        out
    }

    /// The level a response gives white noise (its RMS gain).
    fn scale(h: &[f32]) -> f32 {
        h.iter().map(|v| v * v).sum::<f32>().sqrt().max(1.0)
    }

    #[test]
    fn it_convolves_exactly_without_latency_at_every_length() {
        for taps in [1usize, 50, 64, 65, 600, 5_000, 40_000] {
            let h = noise(taps, 7 + taps as u32);
            let x = noise(60_000, 3);
            let mut e = engine(Routing::new(1, 1, vec![(0, 0)]), &[&h], taps);
            let y = run_pieces(&mut e, &[&x], x.len());
            // The start, where the last stage begins, and the end.
            let windows = [0..2_000, 32_500..34_000, 58_000..60_000];
            for n in windows.into_iter().flatten() {
                let want = direct_at(&x, &h, n);
                assert!(
                    (y[0][n] - want).abs() < 1e-4 * scale(&h),
                    "{taps} taps, at {n}: {} vs {want}",
                    y[0][n]
                );
            }
            // An impulse comes out as the response, at once.
            let mut e = engine(Routing::new(1, 1, vec![(0, 0)]), &[&h], taps);
            let mut imp = vec![0.0f32; taps + 100];
            imp[0] = 1.0;
            let y = run_pieces(&mut e, &[&imp], imp.len());
            for (n, (a, b)) in y[0].iter().zip(&h).enumerate() {
                assert!((a - b).abs() < 1e-4, "{taps} taps: tap {n}: {a} vs {b}");
            }
            assert!(y[0][taps..].iter().all(|v| v.abs() < 1e-4));
        }
    }

    #[test]
    fn true_stereo_paths_cross_over() {
        let h: Vec<Vec<f32>> = (0..4).map(|i| noise(3_000, 11 + i)).collect();
        let refs: Vec<&[f32]> = h.iter().map(|v| v.as_slice()).collect();
        let mut e = engine(Routing::true_stereo(), &refs, 3_000);
        let (l, r) = (noise(9_000, 1), noise(9_000, 2));
        let y = run_pieces(&mut e, &[&l, &r], 9_000);
        let tol = 2e-4 * scale(&h[0]);
        for n in (0..9_000).step_by(7) {
            let left = direct_at(&l, &h[0], n) + direct_at(&r, &h[2], n);
            let right = direct_at(&l, &h[1], n) + direct_at(&r, &h[3], n);
            assert!((y[0][n] - left).abs() < tol, "left at {n}");
            assert!((y[1][n] - right).abs() < tol, "right at {n}");
        }
    }

    /// New kernels go in smoothly (every stage crossfades over a block of
    /// its own), keep the history (no gap), and then play exactly.
    #[test]
    fn new_kernels_crossfade_in_and_keep_the_history() {
        // A flat response summing to one: a steady input gives a steady
        // output, so the switch to half of it must glide, not jump.
        let len = 40_000;
        let a = vec![1.0 / len as f32; len];
        let b: Vec<f32> = a.iter().map(|v| v * 0.5).collect();
        let mut planner = RealFftPlanner::new();
        let mut e = engine(Routing::new(1, 1, vec![(0, 0)]), &[&a], len);
        let x = vec![1.0f32; 200_000];
        let first = 100_000;
        let mut out = vec![0.0f32; x.len()];
        run(&mut e, &[&x[..first]], &mut [&mut out[..first]]);
        assert!((out[first - 1] - 1.0).abs() < 1e-3);
        let k = Box::new(Kernels::new(&[&b], &mut planner));
        assert!(e.offer(k).is_ok());
        assert!(e.switching());
        assert!(
            e.offer(Box::new(Kernels::new(&[&b], &mut planner)))
                .is_err(),
            "one switch at a time"
        );
        run(&mut e, &[&x[first..]], &mut [&mut out[first..]]);
        assert!(
            !e.switching(),
            "switched within three of the largest blocks"
        );
        assert!(e.take_retired().is_some(), "the old set handed back");
        for n in first..x.len() - 1 {
            let step = out[n + 1] - out[n];
            assert!(step <= 1e-4, "it rose at {n}");
            assert!(step > -0.02, "a jump of {step} at {n}");
        }
        assert!(
            (out[x.len() - 1] - 0.5).abs() < 1e-3,
            "{}",
            out[x.len() - 1]
        );
        // With noise: exactly the new response's output after the switch.
        let x = noise(150_000, 9);
        let mut e = engine(Routing::new(1, 1, vec![(0, 0)]), &[&a], len);
        let mut out = vec![0.0f32; x.len()];
        run(&mut e, &[&x[..first]], &mut [&mut out[..first]]);
        assert!(e.offer(Box::new(Kernels::new(&[&b], &mut planner))).is_ok());
        run(&mut e, &[&x[first..]], &mut [&mut out[first..]]);
        for n in (first + 3 * 16_384..x.len()).step_by(97) {
            let want = direct_at(&x, &b, n);
            assert!((out[n] - want).abs() < 1e-4, "at {n}");
        }
    }

    #[test]
    fn a_reset_forgets_the_tail() {
        let h = noise(50_000, 4);
        let mut e = engine(Routing::new(1, 1, vec![(0, 0)]), &[&h], 50_000);
        let x = noise(60_000, 8);
        let _ = run_pieces(&mut e, &[&x], x.len());
        e.reset();
        let silence = vec![0.0f32; 80_000];
        let y = run_pieces(&mut e, &[&silence], silence.len());
        assert!(y[0].iter().all(|v| *v == 0.0), "nothing from before");
        // And it convolves exactly again from the reset (counting what
        // went in since it as everything).
        let mut joined = silence.clone();
        joined.extend_from_slice(&x[..40_000]);
        let y = run_pieces(&mut e, &[&x[..40_000]], 40_000);
        for n in (0..40_000).step_by(13) {
            let want = direct_at(&joined, &h, silence.len() + n);
            assert!((y[0][n] - want).abs() < 1e-4 * scale(&h), "at {n}");
        }
    }

    #[test]
    fn kernels_that_do_not_fit_are_refused() {
        let mut planner = RealFftPlanner::new();
        let mut e = engine(Routing::parallel(), &[&[1.0], &[1.0]], 1_000);
        let long = vec![0.1f32; 5_000];
        let k = Box::new(Kernels::new(&[&long, &long], &mut planner));
        assert!(e.offer(k).is_err());
        let k = Box::new(Kernels::new(&[&[0.5]], &mut planner));
        assert!(e.offer(k).is_err(), "one path for two");
        let silent = Engine::silent(Routing::parallel(), &mut planner);
        assert_eq!(silent.capacity(), 0);
    }
}
