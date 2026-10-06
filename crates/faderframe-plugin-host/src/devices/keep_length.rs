//! Pitch without changing length, for the samplers: a pool of stretchers
//! (Signalsmith Stretch, 60 ms analysis) that voices playing in "Keep
//! Length" borrow. Such a voice reads its sample at the sample's own speed
//! and the stretcher moves its pitch; envelope, filter and level come
//! after, as for any voice.
//!
//! No added latency: on its first block a voice primes its stretcher — the
//! analysis fed ahead (the sample is in memory) and the output latency run
//! and discarded — so it sounds when its note comes. Priming costs about a
//! third of a millisecond, so a block primes at most one voice per
//! [`PRIME_SECONDS`] of its length; the others start a block later. When
//! the sample ends, the stretcher is fed silence for its latency so the
//! last of it is heard.
//!
//! The pool is made on the control thread (with the processor); nothing
//! here allocates, locks or does I/O on the audio thread (the stretcher's
//! own calls are proven allocation-free by `faderframe-stretch`'s tests).

use faderframe_stretch::{Preset, Stretcher};

/// Voices that can keep their length at once (more steal the oldest).
pub const VOICES: usize = 16;
/// Priming work a block may do per voice, in seconds of the block.
pub const PRIME_SECONDS: f64 = 0.0015;

struct Slot {
    stretcher: Stretcher,
    input: [Vec<f32>; 2],
    output: [Vec<f32>; 2],
    /// The voice using it.
    owner: Option<usize>,
    primed: bool,
    /// Frames of silence still to feed after the sample ended.
    tail: Option<usize>,
    done: bool,
}

/// What a voice gets from its stretcher for a block.
pub enum Render<'a> {
    /// Waiting for its priming (the block's budget is spent).
    Wait,
    /// Finished: the sample and the stretcher's tail are out.
    Done,
    /// The block's frames (both sides).
    Out(&'a [f32], &'a [f32]),
}

pub struct KeepLength {
    slots: Vec<Slot>,
    budget: usize,
}

impl KeepLength {
    /// The pool for blocks of up to `max_block` frames (allocates; control
    /// thread). `None` when the stretcher is not available.
    pub fn new(sample_rate: f64, max_block: usize) -> Option<Self> {
        let mut slots = Vec::with_capacity(VOICES);
        for _ in 0..VOICES {
            let stretcher = Stretcher::new(2, sample_rate, Preset::Rhythmic)?;
            let out = max_block.max(256);
            let input = stretcher.seek_length().max(out);
            slots.push(Slot {
                stretcher,
                input: [vec![0.0; input], vec![0.0; input]],
                output: [vec![0.0; out], vec![0.0; out]],
                owner: None,
                primed: false,
                tail: None,
                done: false,
            });
        }
        Some(Self { slots, budget: 0 })
    }

    /// At the start of a block of `frames`: the primes it may do, and the
    /// stretchers of voices that stopped (`alive(voice, slot)` says whether
    /// a voice still plays on that stretcher) go back to the pool.
    pub fn begin_block(
        &mut self,
        frames: usize,
        sample_rate: f64,
        alive: impl Fn(usize, u8) -> bool,
    ) {
        self.budget = ((frames as f64 / (sample_rate * PRIME_SECONDS)) as usize).max(1);
        for (i, s) in self.slots.iter_mut().enumerate() {
            if s.owner.is_some_and(|o| !alive(o, i as u8)) {
                s.owner = None;
            }
        }
    }

    /// A stretcher for `voice` (a new note): a free one, else the one whose
    /// voice is the oldest by `age` — that voice must stop (returned).
    pub fn claim(&mut self, voice: usize, age: impl Fn(usize) -> u64) -> (u8, Option<usize>) {
        let i = self
            .slots
            .iter()
            .position(|s| s.owner.is_none_or(|o| o == voice))
            .unwrap_or_else(|| {
                (0..self.slots.len())
                    .min_by_key(|&i| self.slots[i].owner.map_or(0, &age))
                    .unwrap_or(0)
            });
        let s = &mut self.slots[i];
        let evicted = s.owner.filter(|&o| o != voice);
        s.owner = Some(voice);
        s.primed = false;
        s.tail = None;
        s.done = false;
        (i as u8, evicted)
    }

    /// Every stretcher back to the pool.
    pub fn release_all(&mut self) {
        for s in &mut self.slots {
            s.owner = None;
        }
    }

    /// `n` frames (at most the block size) of the voice on `slot`, pitched
    /// by `transpose`. `feed` writes up to the buffers' length of the
    /// voice's sample at its own speed and returns how many frames it had
    /// (fewer: the sample ended).
    pub fn render(
        &mut self,
        slot: u8,
        transpose: f32,
        n: usize,
        feed: &mut dyn FnMut(&mut [f32], &mut [f32]) -> usize,
    ) -> Render<'_> {
        let Some(s) = self.slots.get_mut(slot as usize) else {
            return Render::Done;
        };
        if s.done {
            return Render::Done;
        }
        let n = n.min(s.output[0].len());
        let latency = s.stretcher.input_latency() + s.stretcher.output_latency();
        if !s.primed {
            if self.budget == 0 {
                return Render::Wait;
            }
            self.budget -= 1;
            let (li, lo) = (s.stretcher.input_latency(), s.stretcher.output_latency());
            let pre = s.stretcher.seek_length().min(s.input[0].len());
            let lead = li.min(pre);
            s.stretcher.reset();
            s.stretcher.set_transpose(transpose);
            // Before the start, silence; the first `li` frames of the
            // sample end the pre-roll.
            let [a, b] = &mut s.input;
            a[..pre - lead].fill(0.0);
            b[..pre - lead].fill(0.0);
            pull(
                &mut s.tail,
                latency,
                feed,
                &mut a[pre - lead..pre],
                &mut b[pre - lead..pre],
            );
            s.stretcher.seek(&[&a[..pre], &b[..pre]], 1.0);
            // Run the output latency and discard it.
            let chunk = s.output[0].len();
            let mut done = 0;
            while done < lo {
                let k = chunk.min(lo - done);
                let [a, b] = &mut s.input;
                pull(&mut s.tail, latency, feed, &mut a[..k], &mut b[..k]);
                let [oa, ob] = &mut s.output;
                s.stretcher
                    .process(&[&a[..k], &b[..k]], k, &mut [&mut oa[..k], &mut ob[..k]], k);
                done += k;
            }
            s.primed = true;
        }
        s.stretcher.set_transpose(transpose);
        let [a, b] = &mut s.input;
        pull(&mut s.tail, latency, feed, &mut a[..n], &mut b[..n]);
        let [oa, ob] = &mut s.output;
        s.stretcher
            .process(&[&a[..n], &b[..n]], n, &mut [&mut oa[..n], &mut ob[..n]], n);
        if s.tail == Some(0) {
            s.done = true;
        }
        Render::Out(&s.output[0][..n], &s.output[1][..n])
    }
}

/// Fill `a`/`b` from the sample, silence after its end (counting down the
/// stretcher's tail).
fn pull(
    tail: &mut Option<usize>,
    latency: usize,
    feed: &mut dyn FnMut(&mut [f32], &mut [f32]) -> usize,
    a: &mut [f32],
    b: &mut [f32],
) {
    let n = a.len().min(b.len());
    let got = if tail.is_some() {
        0
    } else {
        feed(&mut a[..n], &mut b[..n]).min(n)
    };
    a[got..n].fill(0.0);
    b[got..n].fill(0.0);
    if got < n {
        let t = tail.get_or_insert(latency);
        *t = t.saturating_sub(n - got);
    }
}
