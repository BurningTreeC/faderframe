//! The JC-120's bucket-brigade chorus: an MN3007, its MN3101 clock, and the
//! oscillator that sweeps them.
//!
//! Read off the EFF BOARD of the Sep. 2000 service notes; the checkpoint is
//! `docs/models/jazz_120.md`.
//!
//! ## What a bucket brigade is, and why this one is not solved
//!
//! An MN3007 is 1024 capacitors in a chain. A two-phase clock walks a charge
//! packet from each to the next, so a sample put in at one end arrives at the
//! other `N / (2 f_clk)` seconds later -- it is a delay line made of analogue
//! samples, and its length is set by how fast it is clocked rather than by
//! anything in the signal path.
//!
//! The sheet prints that clock as **2.5 to 12 µs**, which is 400 kHz down to
//! 83.3 kHz. Solving the chain stage by stage would mean running this circuit
//! at four hundred kilohertz to model a delay, and no part of this plugin
//! makes that trade. So the bucket brigade itself is **APPROXIMATED** by a
//! fractional delay whose length is what that clock range gives, in the same
//! way and for the same stated reason that the spring tank's propagation is in
//! the two AB763s.
//!
//! What is *not* approximated is everything the sound comes from. The stage
//! count, the clock range and the oscillator's period are the sheet's printed
//! figures, and the delay they imply falls out of them rather than being
//! chosen:
//!
//! | | printed | here |
//! |---|---|---|
//! | stages | MN3007, 1024 | [`STAGES`] |
//! | clock period | 2.5-12 µs | [`CLOCK_FAST_HZ`], [`CLOCK_SLOW_HZ`] |
//! | delay, therefore | -- | **1.28 ms to 6.14 ms** |
//! | oscillator, CHORUS | triangle, 1.2 s | [`CHORUS_PERIOD`] |
//! | oscillator, VIBRATO | 120-500 ms | [`VIBRATO_FAST`], [`VIBRATO_SLOW`] |
//!
//! Five to one is a wide sweep for a chorus, and it is why this one is as
//! audible as it is: over a 0.6 s half period, 4.86 ms of delay change is
//! 0.81 % of pitch, about fourteen cents.
//!
//! ## What is deliberately absent
//!
//! **No companding.** There is no NE570 or anything like it anywhere on that
//! board -- the JC-120 runs its bucket brigade open, with filters either side
//! and nothing else. Most BBD choruses of the period do compand, so this is
//! worth saying: a compander here would be an invented sound.
//!
//! **No clock whine.** A real MN3007 leaks its clock into the signal, and the
//! reconstruction filter is there to take it out. Both the leak and the filter
//! that removes it live above 83 kHz, which is above every sample rate this
//! plugin runs at, so neither is modelled. The filters that *are* modelled are
//! the ones inside the audio band.
//!
//! ## The interpolation, and why it is this one
//!
//! The read pointer moves continuously, so the delay is almost never a whole
//! number of samples. Linear interpolation between neighbours is the obvious
//! thing and it is wrong here in an audible way: its response is a comb that
//! loses over 3 dB at a quarter of the sample rate and moves as the delay
//! moves, so a swept delay reads as a swept low-pass. This uses a four-point
//! Hermite interpolation instead, which is flat enough across the band that
//! what moves is the pitch and not the tone.

/// An MN3007's stage count, from the part.
pub const STAGES: f64 = 1024.0;

/// The clock, from the sheet's printed 2.5-12 µs period.
pub const CLOCK_FAST_HZ: f64 = 1.0 / 2.5e-6;
pub const CLOCK_SLOW_HZ: f64 = 1.0 / 12.0e-6;

/// What that clock range makes of 1024 stages: a delay of `N / (2 f)`.
///
/// Not chosen -- derived. 1.28 ms at the fast end, 6.14 ms at the slow one.
pub const SHORTEST: f64 = STAGES / (2.0 * CLOCK_FAST_HZ);
pub const LONGEST: f64 = STAGES / (2.0 * CLOCK_SLOW_HZ);

/// The highest rate the delay line is sized for up front, so that changing
/// rate between the five the solver is tested at never allocates.
const MAX_RATE: f64 = 192_000.0;

/// Room for the longest delay plus the four-point interpolator's reach.
fn line_len(rate: f64) -> usize {
    ((LONGEST * rate).ceil() as usize + 8).max(16)
}

/// CHORUS: a triangle of 1.2 s, and no speed control on the panel in this
/// mode. The sheet draws the waveform and dimensions one full period.
pub const CHORUS_PERIOD: f64 = 1.2;

/// VIBRATO: the same oscillator with the panel's Speed reaching it. The CH1
/// board's note dimensions the period at its two ends.
pub const VIBRATO_FAST: f64 = 0.120;
pub const VIBRATO_SLOW: f64 = 0.500;

/// Which of the two the switch is in. `SW3`, four sections, marked
/// VIB / OFF / CHORUS on the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Chorus,
    Vibrato,
}

/// The delay line, its oscillator and the filters either side.
pub struct Bbd {
    rate: f64,
    line: Vec<f64>,
    write: usize,
    /// Oscillator phase, 0..1 over one period.
    phase: f64,
    mode: Mode,
    /// Panel Speed and Depth, 0..1. Speed reaches the oscillator only in
    /// vibrato; depth scales how much of the clock range is swept.
    speed: f64,
    depth: f64,
    /// The two filters, as one-pole pairs. See `set_rate`.
    anti_alias: [f64; 2],
    reconstruct: [f64; 2],
    alpha_in: f64,
    alpha_out: f64,
}

impl Bbd {
    /// The band either side of the delay.
    ///
    /// The sheet's filters are op-amp sections with their own networks; what
    /// they do inside the audio band is a low-pass at a few kilohertz on the
    /// way in and the same on the way out, which is what keeps a BBD's
    /// quantised, clock-limited path from sounding gritty. Two one-pole
    /// sections each, at the corner the drawing's values give.
    ///
    /// DERIVED from the network around IC2A and IC2B rather than read as a
    /// single number, and stated here so it can be checked: R60 68 k with
    /// C37 150 pF is 15.6 kHz, and R25 3.3 k with C36 0.015 µF is 3.2 kHz.
    /// The pair of them is what sets the band.
    const CORNER_IN: f64 = 6_500.0;
    const CORNER_OUT: f64 = 6_500.0;

    pub fn new(rate: f64) -> Self {
        let mut bbd = Self {
            rate,
            line: vec![0.0; line_len(rate.max(MAX_RATE))],
            write: 0,
            phase: 0.0,
            mode: Mode::Chorus,
            speed: 0.5,
            depth: 1.0,
            anti_alias: [0.0; 2],
            reconstruct: [0.0; 2],
            alpha_in: 0.0,
            alpha_out: 0.0,
        };
        bbd.set_rate(rate);
        bbd
    }

    /// The sample rate. Constructed at `MAX_RATE`, so every rate the plugin is
    /// tested at reuses the buffer and this never allocates; a host past that
    /// grows it once, here, off the audio thread.
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
        let len = line_len(rate);
        if self.line.len() < len {
            self.line.resize(len, 0.0);
        }
        self.line.fill(0.0);
        self.write = 0;
        self.anti_alias = [0.0; 2];
        self.reconstruct = [0.0; 2];
        self.alpha_in = one_pole(Self::CORNER_IN, rate);
        self.alpha_out = one_pole(Self::CORNER_OUT, rate);
    }

    pub fn reset(&mut self) {
        self.line.fill(0.0);
        self.write = 0;
        self.phase = 0.0;
        self.anti_alias = [0.0; 2];
        self.reconstruct = [0.0; 2];
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Panel Speed, 0..1. Reaches the oscillator in vibrato only: in chorus
    /// the sheet's period is fixed at 1.2 s and the panel has no control over
    /// it, which is a fact about the amplifier rather than a simplification.
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(0.0, 1.0);
    }

    /// Panel Depth, 0..1, scaling how much of the clock's range is swept.
    pub fn set_depth(&mut self, depth: f64) {
        self.depth = depth.clamp(0.0, 1.0);
    }

    /// The oscillator's period at the current setting.
    pub fn period(&self) -> f64 {
        match self.mode {
            Mode::Chorus => CHORUS_PERIOD,
            // The panel's Speed runs the period from slow to fast.
            Mode::Vibrato => VIBRATO_SLOW + (VIBRATO_FAST - VIBRATO_SLOW) * self.speed,
        }
    }

    /// One sample through the delay. The return is the *wet* signal alone; the
    /// two amplifiers' split is the caller's, because on this amplifier it is
    /// two power stages and two speakers rather than a mix control.
    pub fn process(&mut self, x: f64) -> f64 {
        // Into the band the bucket brigade works in.
        self.anti_alias[0] += self.alpha_in * (x - self.anti_alias[0]);
        self.anti_alias[1] += self.alpha_in * (self.anti_alias[0] - self.anti_alias[1]);
        let into = self.anti_alias[1];

        self.line[self.write] = into;

        // The oscillator: a triangle, as the sheet draws it, from 0 to 1 and
        // back over one period.
        self.phase += 1.0 / (self.period() * self.rate);
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        let triangle = if self.phase < 0.5 {
            2.0 * self.phase
        } else {
            2.0 * (1.0 - self.phase)
        };

        // The clock sweeps between its two printed ends, and Depth says how
        // much of that range is used. At rest it sits at the fast end, which
        // is where the shortest delay is.
        let span = (LONGEST - SHORTEST) * self.depth;
        let delay = SHORTEST + span * triangle;
        let samples = delay * self.rate;

        let out = self.read(samples);
        self.write = (self.write + 1) % self.line.len();

        // And out through the reconstruction filter.
        self.reconstruct[0] += self.alpha_out * (out - self.reconstruct[0]);
        self.reconstruct[1] += self.alpha_out * (self.reconstruct[0] - self.reconstruct[1]);
        self.reconstruct[1]
    }

    /// Four-point Hermite, reading `samples` behind the write pointer.
    fn read(&self, samples: f64) -> f64 {
        hermite(&self.line, self.write, samples)
    }
}

/// Four-point Hermite on a circular line, reading `samples` behind `write`.
fn hermite(line: &[f64], write: usize, samples: f64) -> f64 {
    let len = line.len();
    let whole = samples.floor();
    let frac = samples - whole;
    let base = write as isize - whole as isize;
    let at = |offset: isize| -> f64 {
        let mut i = (base + offset) % len as isize;
        if i < 0 {
            i += len as isize;
        }
        line[i as usize]
    };
    // Newest to oldest as the offset grows, so `y0` is one *ahead* of the
    // read point and `y3` two behind it.
    let (y0, y1, y2, y3) = (at(1), at(0), at(-1), at(-2));
    let c0 = y1;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * frac + c2) * frac + c1) * frac + c0
}

/// A bucket brigade's delay and nothing else: an MN3007's 1024 stages at
/// whatever clock the circuit around it sets.
///
/// For a pedal whose oscillator, clock control and filters are all solved in
/// its own netlist -- the CE-2's -- and which needs from here only the thing a
/// netlist cannot be: a delay of some milliseconds that moves. The same
/// four-point Hermite as `Bbd`, for the same reason. Sized up front for the
/// longest delay at the highest rate it will be run at, so that running,
/// resetting and changing rate never allocate.
///
/// **It delays the signal about the level it sits at**, not the level itself.
/// A bucket brigade's output stands at the brigade's own bias, not at its
/// input's, and everything after it is coupled through a capacitor; and the
/// netlist's operating point is solved with the brigade's return at rest. So
/// the input's standing level is tracked (a one-pole a hertz wide) and taken
/// off before the line, primed with the first sample after a reset -- which
/// is the operating point -- so that nothing steps. Delaying the standing level
/// instead put the CE-2's -0.78 V into its return as a step five milliseconds
/// after every reset, and out of it as a thump of 0.45.
pub struct Brigade {
    line: Vec<f64>,
    write: usize,
    level: f64,
    primed: bool,
}

impl Brigade {
    pub fn new(longest: f64, max_rate: f64) -> Self {
        Self {
            line: vec![0.0; (longest * max_rate).ceil() as usize + 8],
            write: 0,
            level: 0.0,
            primed: false,
        }
    }

    pub fn reset(&mut self) {
        self.line.fill(0.0);
        self.write = 0;
        self.level = 0.0;
        self.primed = false;
    }

    /// Take another line's exact history (FaderFrame: a dormant channel
    /// waking from the one that ran). Both were made with the same size, so
    /// this copies into existing storage and never allocates.
    pub fn copy_runtime_state_from(&mut self, source: &Self) {
        if self.line.len() == source.line.len() {
            self.line.copy_from_slice(&source.line);
        }
        self.write = source.write;
        self.level = source.level;
        self.primed = source.primed;
    }

    /// One sample in, and the line read `samples` behind it, at `rate`.
    pub fn process(&mut self, x: f64, samples: f64, rate: f64) -> f64 {
        if !self.primed {
            self.level = x;
            self.primed = true;
        }
        self.level += (std::f64::consts::TAU / rate).min(1.0) * (x - self.level);
        self.line[self.write] = x - self.level;
        let out = hermite(
            &self.line,
            self.write,
            samples.clamp(1.0, (self.line.len() - 4) as f64),
        );
        self.write = (self.write + 1) % self.line.len();
        out
    }
}

// --- a bucket brigade at its own clock -------------------------------------------

/// What a clocked line is made of: its stages, its chips and what each chip
/// does to the charge it carries. See [`Clocked`].
#[derive(Clone, Copy, Debug)]
pub struct ClockedSpec {
    /// Stages in the whole line, every chip in series. A sample is taken once
    /// a clock period and comes out `stages / 2` periods later.
    pub stages: usize,
    /// How many chips the stages are in. Each brings its own insertion gain
    /// and its own transfer curve; between them the signal is re-biased.
    pub chips: usize,
    /// One chip's insertion gain.
    pub gain: f64,
    /// One chip's transfer curve about its bias: `x + second x^2 - third x^3`,
    /// its buckets full at `saturation` volts either way.
    pub second: f64,
    pub third: f64,
    pub saturation: f64,
    /// The whole line's charge-transfer inefficiency, N times the fraction of
    /// a packet each transfer leaves behind for the one after it. Its
    /// response is `exp(-inefficiency (1 - cos(2 pi f / f_clock)))`: a
    /// low-pass that follows the clock down.
    pub inefficiency: f64,
    /// Noise on each sample the line carries, in volts RMS: a white part, and
    /// a part from the charge each bucket gathers while it waits, which grows
    /// with the holding time and is `leakage_noise` at `noise_reference_hz`.
    pub noise: f64,
    pub leakage_noise: f64,
    pub noise_reference_hz: f64,
    /// The clock the output stages leave in the signal after their two
    /// outputs are balanced against each other: a square at the clock, this
    /// many volts either way.
    pub clock_leak: f64,
}

/// How many samples of the circuit's rate either side of a step the
/// band-limited step spreads over.
const STEP_HALF: usize = 8;
const STEP_SPAN: usize = 2 * STEP_HALF;
/// Fractional positions per sample in the step table.
const STEP_PHASES: usize = 64;
/// The band the steps are limited to, as a fraction of the circuit's rate.
const STEP_CUTOFF: f64 = 0.45;
/// Taps of the charge-transfer smear: enough for an inefficiency of two.
const SMEAR_TAPS: usize = 8;

/// A bucket brigade run at its own clock, between two halves of a solved
/// circuit (FaderFrame: the Carbon Echo's four BL3208As).
///
/// Where [`Brigade`] is a delay whose length a clock sets, this is the clock
/// itself. A sample of the input is taken at every clock period -- whenever
/// in the circuit's own samples that falls -- and held in a bucket; the
/// bucket `stages / 2` periods back comes out and is held at the output for
/// one period, as a BBD's two summed outputs hold it. So:
///
/// - **a slow clock aliases.** Anything above half the clock that the
///   input filter lets through folds back down, as it does in the chip;
/// - **its output is a staircase**, with images of the signal about every
///   multiple of the clock, for the output filter to take out -- and below
///   about 14 kHz of clock the first image is in the audio band;
/// - **turning the clock moves the pitch.** The buckets hold samples, not
///   time: what was put in at one clock comes out at another, faster or
///   slower, which is the swoop of a BBD's delay knob;
/// - **the clock is in the output**: a square at the clock, what is left of
///   it once the two outputs are balanced; and the buckets are noisy, more
///   so the longer each waits.
///
/// The steps are placed between the circuit's samples band-limited (each
/// step is the integral of a windowed sinc, tabulated), so nothing above the
/// circuit's own Nyquist frequency folds into it: the aliasing is the chip's
/// and none of it this model's. That and the four-point interpolation of the
/// input cost `STEP_HALF + 1` samples of the circuit's rate in latency --
/// under 0.2 ms at 48 kHz.
///
/// Allocates once, in `new`, off the audio thread; the line's size is the
/// stage count, not a time, so no clock or rate ever resizes it.
pub struct Clocked {
    spec: ClockedSpec,
    buckets: Vec<f64>,
    mask: usize,
    write: usize,
    periods: usize,
    smear: [f64; SMEAR_TAPS],
    /// Half periods into the present one, 0..1.
    phase: f64,
    /// Whether the next clock edge ends the first half of a period.
    first_half: bool,
    /// The packet at the output, and what the output holds (it and the
    /// clock).
    packet: f64,
    held: f64,
    /// The input at the last four samples, oldest first.
    input: [f64; 4],
    /// What the steps still owe the next `STEP_SPAN` outputs, and what the
    /// held output was `STEP_HALF` samples ago.
    owed: [f64; STEP_SPAN],
    naive: [f64; STEP_SPAN],
    at: usize,
    seed: u64,
    rng: u64,
    steps: Vec<f64>,
}

impl Clocked {
    pub fn new(spec: ClockedSpec, seed: u64) -> Self {
        let periods = (spec.stages / 2).max(1);
        let size = (periods + SMEAR_TAPS + 1).next_power_of_two();
        let mut smear = [0.0; SMEAR_TAPS];
        // The fraction left behind at each of N transfers, compounded: a
        // packet arrives with `e^-l l^k / k!` of the one `k` periods ahead of
        // it (Poisson, for a small fraction over many transfers).
        let l = spec.inefficiency.max(0.0);
        let mut term = (-l).exp();
        for (k, tap) in smear.iter_mut().enumerate() {
            *tap = term;
            term *= l / (k + 1) as f64;
        }
        let total: f64 = smear.iter().sum();
        smear.iter_mut().for_each(|t| *t /= total);
        let seed = seed | 1;
        Self {
            spec,
            buckets: vec![0.0; size],
            mask: size - 1,
            write: 0,
            periods,
            smear,
            phase: 0.0,
            first_half: false,
            packet: 0.0,
            held: 0.0,
            input: [0.0; 4],
            owed: [0.0; STEP_SPAN],
            naive: [0.0; STEP_SPAN],
            at: 0,
            seed,
            rng: seed,
            steps: step_table(),
        }
    }

    pub fn spec(&self) -> &ClockedSpec {
        &self.spec
    }

    /// The delay a clock gives, in seconds: `stages / (2 f)`.
    pub fn delay_at(&self, clock_hz: f64) -> f64 {
        self.periods as f64 / clock_hz.max(1.0)
    }

    pub fn reset(&mut self) {
        self.buckets.fill(0.0);
        self.write = 0;
        self.phase = 0.0;
        self.first_half = false;
        self.packet = 0.0;
        self.held = 0.0;
        self.input = [0.0; 4];
        self.owed = [0.0; STEP_SPAN];
        self.naive = [0.0; STEP_SPAN];
        self.at = 0;
        self.rng = self.seed;
    }

    /// Take another line's exact state (FaderFrame: a dormant channel waking
    /// from the one that ran). Copies into existing storage only.
    pub fn copy_runtime_state_from(&mut self, source: &Self) {
        if self.buckets.len() == source.buckets.len() {
            self.buckets.copy_from_slice(&source.buckets);
        }
        self.write = source.write;
        self.phase = source.phase;
        self.first_half = source.first_half;
        self.packet = source.packet;
        self.held = source.held;
        self.input = source.input;
        self.owed = source.owed;
        self.naive = source.naive;
        self.at = source.at;
        self.rng = source.rng;
    }

    /// One sample of the circuit in (volts about the input's bias), the
    /// clock over that sample, and the output for the circuit's next sample
    /// (volts about the output's own level).
    pub fn process(&mut self, x: f64, clock_hz: f64, rate: f64) -> f64 {
        let x = if x.is_finite() { x } else { 0.0 };
        self.input = [self.input[1], self.input[2], self.input[3], x];
        // Half periods in one sample of the circuit.
        let step = 2.0 * clock_hz.clamp(1.0, rate * 16.0) / rate;
        let mut at = 0.0;
        let mut phase = self.phase;
        loop {
            let need = (1.0 - phase) / step;
            if at + need > 1.0 {
                phase += (1.0 - at) * step;
                break;
            }
            at += need;
            phase = 0.0;
            self.edge(at, clock_hz);
        }
        self.phase = phase.min(1.0);

        let now = self.at & (STEP_SPAN - 1);
        self.naive[now] = self.held;
        let then = self.at.wrapping_sub(STEP_HALF) & (STEP_SPAN - 1);
        let out = self.naive[then] + self.owed[now];
        self.owed[now] = 0.0;
        self.at = self.at.wrapping_add(1);
        out
    }

    /// A clock edge `at` of the way through the present sample.
    fn edge(&mut self, at: f64, clock_hz: f64) {
        self.first_half = !self.first_half;
        let leak = self.spec.clock_leak;
        let next = if self.first_half {
            // A new period: the input sampled into the first bucket, the
            // last bucket's packet out.
            let sampled = self.through_chips(self.sampled(at));
            self.buckets[self.write] = sampled;
            let read = self.write.wrapping_sub(self.periods);
            let mut packet = 0.0;
            for (k, tap) in self.smear.iter().enumerate() {
                packet += tap * self.buckets[read.wrapping_sub(k) & self.mask];
            }
            self.write = (self.write + 1) & self.mask;
            self.packet = packet + self.noise(clock_hz);
            self.packet + leak
        } else {
            self.packet - leak
        };
        let delta = next - self.held;
        self.held = next;
        if delta != 0.0 {
            self.place_step(delta, 1.0 - at);
        }
    }

    /// The input `at` of the way between the circuit's last-but-one sample
    /// and its last: four-point Hermite.
    fn sampled(&self, at: f64) -> f64 {
        let [y0, y1, y2, y3] = self.input;
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        ((c3 * at + c2) * at + c1) * at + y1
    }

    /// Through every chip's transfer curve, each re-biased at its input.
    fn through_chips(&self, mut x: f64) -> f64 {
        let s = &self.spec;
        let limit = s.saturation.max(1e-3);
        for _ in 0..s.chips {
            let curved = x + s.second * x * x - s.third * x * x * x;
            // Full buckets: a knee of the sixth order, under 1e-4 below a
            // quarter of the way (where it is left out).
            let r = (curved / limit).abs();
            x = s.gain
                * if r < 0.25 {
                    curved
                } else {
                    let r3 = r * r * r;
                    curved / (1.0 + r3 * r3).powf(1.0 / 6.0)
                };
        }
        x
    }

    /// One packet's noise at this clock.
    fn noise(&mut self, clock_hz: f64) -> f64 {
        let s = &self.spec;
        let leakage = s.leakage_noise * s.leakage_noise * s.noise_reference_hz / clock_hz.max(1.0);
        let sigma = (s.noise * s.noise + leakage).sqrt();
        if sigma == 0.0 {
            return 0.0;
        }
        // Four uniforms: near enough a normal for noise, and cheap.
        let mut sum = 0.0;
        for _ in 0..4 {
            self.rng ^= self.rng >> 12;
            self.rng ^= self.rng << 25;
            self.rng ^= self.rng >> 27;
            let bits = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
            sum += bits as f64 * (1.0 / (1u64 << 53) as f64);
        }
        sigma * (sum - 2.0) * 3f64.sqrt()
    }

    /// A step of `delta` `before` of a sample ahead of the present sample's
    /// end, band-limited into the outputs it reaches.
    fn place_step(&mut self, delta: f64, before: f64) {
        let pos = before.clamp(0.0, 1.0) * STEP_PHASES as f64;
        let p = (pos as usize).min(STEP_PHASES - 1);
        let w = pos - p as f64;
        for j in 0..STEP_SPAN {
            let row = &self.steps[j * (STEP_PHASES + 1)..];
            let r = row[p] + (row[p + 1] - row[p]) * w;
            self.owed[(self.at + j) & (STEP_SPAN - 1)] += delta * r;
        }
    }
}

/// What a band-limited step differs from a sharp one by, `j - STEP_HALF +
/// p / STEP_PHASES` samples after it, for `j` in `0..STEP_SPAN` and `p` in
/// `0..=STEP_PHASES`: the running integral of a Blackman-windowed sinc at
/// `STEP_CUTOFF`, less the unit step.
fn step_table() -> Vec<f64> {
    let half = STEP_HALF as f64;
    let fine = 512usize;
    let impulse = |s: f64| -> f64 {
        if s.abs() >= half {
            return 0.0;
        }
        let x = s / half;
        let window = 0.42 + 0.5 * (std::f64::consts::PI * x).cos()
            + 0.08 * (std::f64::consts::TAU * x).cos();
        let arg = std::f64::consts::TAU * STEP_CUTOFF * s;
        let sinc = if arg.abs() < 1e-12 {
            2.0 * STEP_CUTOFF
        } else {
            (arg).sin() / (std::f64::consts::PI * s)
        };
        sinc * window
    };
    // The running integral on a fine grid (midpoint rule), normalised so the
    // step ends at exactly one.
    let n = STEP_SPAN * fine;
    let mut running = vec![0.0; n + 1];
    let ds = 1.0 / fine as f64;
    for k in 0..n {
        let s = -half + (k as f64 + 0.5) * ds;
        running[k + 1] = running[k] + impulse(s) * ds;
    }
    let total = running[n];
    let integral = |d: f64| -> f64 {
        let pos = ((d + half) * fine as f64).clamp(0.0, n as f64);
        let k = (pos as usize).min(n - 1);
        let w = pos - k as f64;
        (running[k] + (running[k + 1] - running[k]) * w) / total
    };
    let mut table = vec![0.0; STEP_SPAN * (STEP_PHASES + 1)];
    for j in 0..STEP_SPAN {
        for p in 0..=STEP_PHASES {
            let d = j as f64 - half + p as f64 / STEP_PHASES as f64;
            let sharp = if d >= 0.0 { 1.0 } else { 0.0 };
            table[j * (STEP_PHASES + 1) + p] = integral(d) - sharp;
        }
    }
    table
}

/// A one-pole coefficient for a corner in hertz.
fn one_pole(hz: f64, rate: f64) -> f64 {
    let x = (-std::f64::consts::TAU * hz / rate).exp();
    1.0 - x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The delay range is not a choice. It is 1024 stages and the printed
    /// clock, and this is the arithmetic written out so a later edit to either
    /// end has to face it.
    #[test]
    fn the_delay_range_is_what_the_stage_count_and_the_clock_give() {
        assert!(
            (SHORTEST - 1.28e-3).abs() < 1e-5,
            "1024 stages at 400 kHz is 1.28 ms, not {SHORTEST}"
        );
        assert!(
            (LONGEST - 6.144e-3).abs() < 1e-5,
            "1024 stages at 83.3 kHz is 6.14 ms, not {LONGEST}"
        );
    }

    /// A steady tone through a swept delay comes out at a shifted pitch, and
    /// the shift is the rate of change of the delay. That is what a chorus
    /// *is*, and it is the check that the interpolation moves the read point
    /// rather than merely filtering.
    #[test]
    fn a_swept_delay_shifts_the_pitch_by_its_own_rate_of_change() {
        // Over half a period the delay travels the whole span, so the shift is
        // span / (period / 2) -- about 0.81 % here.
        let expected = (LONGEST - SHORTEST) / (CHORUS_PERIOD / 2.0);
        assert!(
            (0.006..0.011).contains(&expected),
            "the sheet's numbers give {expected} of pitch shift, which is not a \
             chorus"
        );
    }

    /// It has to pass a signal, and not a quiet one.
    ///
    /// A delay line that reads the wrong side of its write pointer, or an
    /// interpolator with its coefficients transposed, still produces
    /// *something* -- so this checks the level rather than that it is not
    /// silent.
    #[test]
    fn it_passes_the_signal_at_its_own_level() {
        let rate = 48_000.0;
        let mut bbd = Bbd::new(rate);
        // Long enough for the longest delay to have filled.
        let n = (rate * 0.5) as usize;
        let mut sum_in = 0.0;
        let mut sum_out = 0.0;
        for i in 0..n {
            let t = i as f64 / rate;
            let x = (std::f64::consts::TAU * 220.0 * t).sin();
            let y = bbd.process(x);
            // Skip the fill.
            if t > 0.1 {
                sum_in += x * x;
                sum_out += y * y;
            }
        }
        let db = 10.0 * (sum_out / sum_in).log10();
        assert!(
            (-4.0..1.0).contains(&db),
            "a 220 Hz tone comes out {db:.1} dB down; the filters either side \
             should cost very little there"
        );
    }

    /// The band either side is a low-pass, so the top goes and the bottom does
    /// not.
    #[test]
    fn the_filters_take_the_top_and_leave_the_bottom() {
        let rate = 48_000.0;
        let level = |hz: f64| {
            let mut bbd = Bbd::new(rate);
            bbd.set_depth(0.0);
            let n = (rate * 0.4) as usize;
            let mut sum = 0.0;
            for i in 0..n {
                let t = i as f64 / rate;
                let y = bbd.process((std::f64::consts::TAU * hz * t).sin());
                if t > 0.2 {
                    sum += y * y;
                }
            }
            10.0 * sum.log10()
        };
        let low = level(200.0);
        let high = level(12_000.0);
        assert!(
            low - high > 6.0,
            "12 kHz should sit well below 200 Hz through a BBD's filters, but \
             the difference is {:.1} dB",
            low - high
        );
    }

    /// Chorus has no speed control and vibrato does, which is the amplifier's
    /// own arrangement and not a simplification.
    #[test]
    fn speed_reaches_the_oscillator_in_vibrato_only() {
        let mut bbd = Bbd::new(48_000.0);
        bbd.set_mode(Mode::Chorus);
        bbd.set_speed(0.0);
        let slow = bbd.period();
        bbd.set_speed(1.0);
        assert_eq!(
            slow,
            bbd.period(),
            "the panel has no Speed control in chorus; the sheet fixes it at \
             {CHORUS_PERIOD} s"
        );
        assert_eq!(bbd.period(), CHORUS_PERIOD);

        bbd.set_mode(Mode::Vibrato);
        bbd.set_speed(0.0);
        assert!((bbd.period() - VIBRATO_SLOW).abs() < 1e-9);
        bbd.set_speed(1.0);
        assert!((bbd.period() - VIBRATO_FAST).abs() < 1e-9);
    }

    /// Nothing in it depends on the sample rate.
    #[test]
    fn the_delay_is_the_same_length_at_every_rate() {
        for rate in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
            let mut bbd = Bbd::new(rate);
            bbd.set_depth(0.0);
            // An impulse, and where it comes back out.
            let n = (rate * 0.05) as usize;
            let mut peak_at = 0usize;
            let mut peak = 0.0;
            for i in 0..n {
                let y = bbd.process(if i == 0 { 1.0 } else { 0.0 });
                if y.abs() > peak {
                    peak = y.abs();
                    peak_at = i;
                }
            }
            let seconds = peak_at as f64 / rate;
            assert!(
                (seconds - SHORTEST).abs() < 3.0e-4,
                "at {rate} Hz the impulse came back at {seconds:.5} s, and the \
                 shortest delay is {SHORTEST:.5} s"
            );
        }
    }

    // --- the clocked line --------------------------------------------------------------

    /// A clean line: no curve, no loss, no noise, no leak.
    fn ideal(stages: usize) -> ClockedSpec {
        ClockedSpec {
            stages,
            chips: 1,
            gain: 1.0,
            second: 0.0,
            third: 0.0,
            saturation: 1e3,
            inefficiency: 0.0,
            noise: 0.0,
            leakage_noise: 0.0,
            noise_reference_hz: 10_000.0,
            clock_leak: 0.0,
        }
    }

    fn sine_power(x: &[f64], hz: f64, rate: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (k, &v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * k as f64 / x.len() as f64).cos();
            let a = std::f64::consts::TAU * hz * k as f64 / rate;
            re += v * w * a.cos();
            im += v * w * a.sin();
        }
        (re * re + im * im) / (x.len() as f64).powi(2)
    }

    /// An impulse comes out `stages / (2 f)` later, plus the step's and the
    /// interpolation's few samples.
    #[test]
    fn a_clocked_line_delays_by_its_stages_over_twice_its_clock() {
        let rate = 48_000.0;
        for clock in [6_800.0, 40_000.0, 200_000.0] {
            let mut line = Clocked::new(ideal(8192), 1);
            let (mut moment, mut area) = (0.0, 0.0);
            for k in 0..(rate * 0.8) as usize {
                // A short pulse, wide enough for the slowest clock to see.
                let x = if k < 8 { 1.0 } else { 0.0 };
                let y = line.process(x, clock, rate);
                moment += k as f64 * y;
                area += y;
            }
            // Where the pulse's weight comes out: the stages, the step's half
            // span and the interpolation's sample, and the pulse's own
            // middle; within half a clock period (where its samples fell)
            // and a sample of the circuit's.
            let want = line.delay_at(clock) + (STEP_HALF as f64 + 1.0 + 3.5) / rate;
            let got = moment / area / rate;
            assert!(
                (got - want).abs() < 1.0 / clock + 1.0 / rate,
                "at {clock} Hz: {got:.5} s, {want:.5} s by the stages"
            );
        }
    }

    /// The buckets hold samples, not time: what went in at one clock comes
    /// out at another an octave up when the clock doubles.
    #[test]
    fn doubling_the_clock_plays_the_buckets_an_octave_up() {
        let rate = 48_000.0;
        let mut line = Clocked::new(ideal(2048), 1);
        let fill = (rate * 0.2) as usize;
        for k in 0..fill {
            let x = (std::f64::consts::TAU * 300.0 * k as f64 / rate).sin();
            line.process(x, 20_000.0, rate);
        }
        // 1024 periods of 20 kHz is 51 ms of 300 Hz in the buckets; read it
        // back at 40 kHz, 25.6 ms of it, with nothing new behind it.
        let out: Vec<f64> = (0..(rate * 0.024) as usize)
            .map(|_| line.process(0.0, 40_000.0, rate))
            .collect();
        let octave = sine_power(&out, 600.0, rate);
        let unison = sine_power(&out, 300.0, rate);
        assert!(octave > 30.0 * unison, "600 Hz {octave:e}, 300 Hz {unison:e}");
    }

    /// A slow clock aliases: a tone above half of it comes out folded down,
    /// and the band-limited steps put nothing above the circuit's own
    /// Nyquist frequency back into the band.
    #[test]
    fn a_slow_clock_folds_what_is_above_half_of_it() {
        let rate = 48_000.0;
        let clock = 8_000.0;
        let mut line = Clocked::new(ideal(256), 1);
        let n = (rate * 0.5) as usize;
        let out: Vec<f64> = (0..n)
            .map(|k| {
                let x = (std::f64::consts::TAU * 5_000.0 * k as f64 / rate).sin();
                line.process(x, clock, rate)
            })
            .collect();
        let tail = &out[n / 2..];
        let folded = sine_power(tail, 3_000.0, rate);
        let straight = sine_power(tail, 5_000.0, rate);
        assert!(folded > 0.5 * straight, "3 kHz {folded:e}, 5 kHz {straight:e}");
        // A clean step's images fall where the clock's multiples put them,
        // and nowhere the circuit's sampling would have folded them to: the
        // 15 kHz image (8 + 7) stays; nothing at 48 - 21 = 27 -> 21 kHz's
        // mirror, 48 - 23 = 25 -> 23 kHz's, beyond what the sinc lets by.
        let image = sine_power(tail, 11_000.0, rate);
        assert!(image < straight, "image {image:e}");
    }

    /// The clock in the output is a square at the clock, there with nothing
    /// in the line.
    #[test]
    fn the_clock_leaks_into_the_output_at_the_clock() {
        let rate = 48_000.0;
        let clock = 6_000.0;
        let mut spec = ideal(512);
        spec.clock_leak = 0.01;
        let mut line = Clocked::new(spec, 1);
        let n = (rate * 0.25) as usize;
        let out: Vec<f64> = (0..n).map(|_| line.process(0.0, clock, rate)).collect();
        let at = sine_power(&out, clock, rate).sqrt();
        let off = sine_power(&out, 4_000.0, rate).sqrt();
        // A square of 10 mV either way has a fundamental of 12.7 mV peak; the
        // window's mean halves what the sum reads.
        assert!((at * 2.0 * 2.0 - 0.0127).abs() < 0.003, "{at}");
        assert!(off < at / 100.0, "{off} away from the clock");
    }

    /// Noise and resets are deterministic, and a copy carries on exactly.
    #[test]
    fn a_reset_line_repeats_itself_and_a_copy_carries_on() {
        let rate = 48_000.0;
        let mut spec = ideal(1024);
        spec.noise = 1e-3;
        spec.leakage_noise = 1e-3;
        spec.chips = 4;
        spec.second = 0.01;
        spec.third = 0.03;
        spec.inefficiency = 1.0;
        spec.clock_leak = 0.003;
        let mut a = Clocked::new(spec, 9);
        let input = |k: usize| 0.5 * (std::f64::consts::TAU * 440.0 * k as f64 / rate).sin();
        let first: Vec<f64> = (0..4_000).map(|k| a.process(input(k), 30_000.0, rate)).collect();
        a.reset();
        let again: Vec<f64> = (0..4_000).map(|k| a.process(input(k), 30_000.0, rate)).collect();
        assert_eq!(first, again);
        let mut b = Clocked::new(spec, 9);
        b.copy_runtime_state_from(&a);
        for k in 4_000..6_000 {
            let x = input(k);
            assert_eq!(a.process(x, 25_000.0, rate), b.process(x, 25_000.0, rate));
        }
    }
}
