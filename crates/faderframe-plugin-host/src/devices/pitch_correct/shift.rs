//! The live pitch shifter: pitch-synchronous overlap-add (TD-PSOLA) at a
//! fixed latency, the engine's clip PSOLA (`nodes::psola`) turned
//! around for a stream.
//!
//! Analysis marks are laid on the input one sung period apart (5 ms where
//! nothing is sung), each with the correction wanted there. Output grains
//! are laid one *target* period apart; each is cut from the input around
//! the mark nearest its place less the latency, two (input) periods long
//! under a Hann window, so the pitch is the target and the formants —
//! each grain's shape — stay where they were. Moving the formants squeezes
//! or stretches the grains (by `phi`): up, a grain reads its two periods
//! into a shorter span; down, it reads less than two periods into the
//! same span, so the grain never reaches further than two periods either
//! way and the latency is `2.5 ×` the longest period (the nearest mark
//! may be half a period after the place). With nothing to correct the
//! grains rebuild the input exactly, delayed by the latency. Every buffer
//! is made with the shifter: allocation-free while it runs.

/// The most a grain's formants move (a factor either way).
pub const FORMANT_RANGE: (f64, f64) = (0.5, 2.0);
/// Unvoiced input is cut into grains this far apart (seconds).
pub const UNVOICED_SECONDS: f64 = 0.005;
/// A grain laid off its mark's place plus the latency (a shift moved the
/// grid) is drawn back by this share of the slip per grain, at most this
/// share of its spacing (a pitch change of under two cents): with nothing
/// to correct the voice comes out exactly the latency later, so a dry mix
/// beside it never combs.
const RELOCK: f64 = 0.05;
const MAX_RELOCK: f64 = 0.001;

/// An analysis mark.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mark {
    /// Input frame.
    pub at: f64,
    /// The period there (frames); a voiced mark's is sung, an unvoiced
    /// one's [`UNVOICED_SECONDS`].
    pub period: f64,
    pub voiced: bool,
    /// The pitch ratio its grains are laid down at.
    pub ratio: f64,
    /// Their formant factor.
    pub formant: f64,
}

/// The shifter for up to two channels.
pub struct Shifter {
    channels: usize,
    latency: usize,
    unvoiced: f64,
    /// Input history per channel (ring).
    input: [Vec<f32>; 2],
    /// Overlapped grains per channel (ring, by output frame).
    output: [Vec<f32>; 2],
    /// Frames taken in (and given out: the same clock).
    written: usize,
    /// Marks (ring) and how many were laid; the oldest still kept.
    marks: Vec<Mark>,
    laid: usize,
    /// The next mark's place.
    next_mark: f64,
    /// The next grain's place (output frame) and the mark last used.
    next_grain: f64,
    cursor: usize,
}

impl Shifter {
    /// For `channels` (1 or 2) at `rate`, periods up to `longest` frames,
    /// blocks up to `block` (allocates; control thread).
    pub fn new(channels: usize, rate: f64, longest: f64, latency: usize, block: usize) -> Self {
        let room = (latency + 4 * longest.ceil() as usize + block + 64).next_power_of_two();
        let shortest = (rate / 2_000.0).max(4.0);
        let marks = (((latency + block) as f64 / shortest) as usize + 64).next_power_of_two();
        Self {
            channels: channels.clamp(1, 2),
            latency,
            unvoiced: (rate * UNVOICED_SECONDS).max(8.0),
            input: [vec![0.0; room], vec![0.0; room]],
            output: [vec![0.0; room], vec![0.0; room]],
            written: 0,
            marks: vec![Mark::default(); marks],
            laid: 0,
            next_mark: 0.0,
            next_grain: latency as f64,
            cursor: 0,
        }
    }

    pub fn reset(&mut self) {
        for c in 0..2 {
            self.input[c].fill(0.0);
            self.output[c].fill(0.0);
        }
        self.written = 0;
        self.laid = 0;
        self.next_mark = 0.0;
        self.next_grain = self.latency as f64;
        self.cursor = 0;
    }

    pub fn latency(&self) -> usize {
        self.latency
    }

    /// Frames taken in so far.
    pub fn written(&self) -> usize {
        self.written
    }

    /// The input frame where the next mark goes.
    pub fn next_mark(&self) -> f64 {
        self.next_mark
    }

    /// Take a frame in.
    #[inline]
    pub fn push(&mut self, left: f32, right: f32) {
        let m = self.input[0].len() - 1;
        let i = self.written & m;
        self.input[0][i] = left;
        self.input[1][i] = right;
        self.written += 1;
    }

    /// Lay the next mark (where [`Self::next_mark`] says) with the period
    /// sung there (`None`: unvoiced) and how its grains are laid down.
    pub fn lay_mark(&mut self, period: Option<f64>, ratio: f64, formant: f64) {
        let (period, voiced) = match period {
            Some(p) if p > 1.0 => (p, true),
            _ => (self.unvoiced, false),
        };
        let n = self.marks.len();
        self.marks[self.laid % n] = Mark {
            at: self.next_mark,
            period,
            voiced,
            ratio: if voiced { ratio.clamp(0.25, 4.0) } else { 1.0 },
            formant: if voiced {
                formant.clamp(FORMANT_RANGE.0, FORMANT_RANGE.1)
            } else {
                1.0
            },
        };
        self.laid += 1;
        self.next_mark += period;
    }

    /// The mark nearest input frame `t` (from the cursor on: places only
    /// move forward).
    fn mark_near(&mut self, t: f64) -> Option<Mark> {
        if self.laid == 0 {
            return None;
        }
        let n = self.marks.len();
        let oldest = self.laid.saturating_sub(n);
        let mut i = self.cursor.clamp(oldest, self.laid - 1);
        while i + 1 < self.laid && self.marks[(i + 1) % n].at <= t {
            i += 1;
        }
        // The one after may be nearer.
        if i + 1 < self.laid {
            let (a, b) = (self.marks[i % n], self.marks[(i + 1) % n]);
            if (b.at - t).abs() < (t - a.at).abs() {
                i += 1;
            }
        }
        self.cursor = i.saturating_sub(1).max(oldest);
        Some(self.marks[i % n])
    }

    /// Lay grains until frame `until` (exclusive) is whole, then play the
    /// frames from `from` into `out` (per channel), mixing in the input
    /// (delayed alike) by `1 − wet`.
    pub fn render(&mut self, from: usize, out: &mut [&mut [f32]], wet: f32) {
        let frames = out.first().map_or(0, |o| o.len());
        let until = from + frames;
        let mo = self.output[0].len() - 1;
        let mi = self.input[0].len() - 1;
        loop {
            let t = self.next_grain;
            let Some(mark) = self.mark_near(t - self.latency as f64) else {
                break;
            };
            let phi = mark.formant;
            let half = mark.period * if phi < 1.0 { 1.0 } else { 1.0 / phi };
            if t - half >= until as f64 {
                break;
            }
            let spacing = (mark.period / mark.ratio).max(1.0);
            // Hann windows `half` long either side, `spacing` apart, sum
            // to half / spacing: weighted back to one.
            let weight = (spacing / half).min(2.0) as f32;
            let first = (t - half).ceil().max(from as f64) as usize;
            let last = (t + half).floor();
            if last >= first as f64 {
                let last = last as usize;
                for k in first..=last {
                    let d = k as f64 - t;
                    let win = (0.5 + 0.5 * (std::f64::consts::PI * d / half).cos()) as f32 * weight;
                    let at = mark.at + d * phi;
                    if at < 0.0 {
                        continue;
                    }
                    let i = at.floor();
                    let f = (at - i) as f32;
                    let i = i as usize;
                    if i + 1 >= self.written {
                        continue;
                    }
                    let slot = k & mo;
                    for c in 0..self.channels {
                        let a = self.input[c][i & mi];
                        let b = self.input[c][(i + 1) & mi];
                        self.output[c][slot] += win * (a + (b - a) * f);
                    }
                }
            }
            let slip = t - (mark.at + self.latency as f64);
            let most = MAX_RELOCK * spacing;
            self.next_grain += spacing - (RELOCK * slip).clamp(-most, most);
        }
        let dry = 1.0 - wet;
        for (k, i) in (from..until).enumerate() {
            let slot = i & mo;
            let back = i.checked_sub(self.latency);
            for (c, o) in out.iter_mut().enumerate().take(self.channels) {
                let input = back.map_or(0.0, |b| self.input[c][b & mi]);
                o[k] = wet * self.output[c][slot] + dry * input;
                self.output[c][slot] = 0.0;
            }
        }
        if self.channels == 1 && out.len() > 1 {
            let (a, b) = out.split_at_mut(1);
            b[0][..frames].copy_from_slice(&a[0][..frames]);
        }
    }
}
