//! The Carbon Echo: a compander round four bucket brigades in series, the
//! filters either side of them, a regeneration loop through all of it, and a
//! clock that a slow triangle sweeps.
//!
//! Engineering reference (developer documentation, not panel text): the MXR
//! M169 Carbon Copy Analog Delay, from kanengomibako's trace of a REV D3 board
//! (PCB P/N 93503005906, 03-24-2009; ASSY P/N 93503005907), published with a
//! repair write-up and a measured response in "MXR M169 CARBON COPY ANALOG
//! DELAY 修理・解析" (kanengomibako.github.io/pages/00324.html, August 2023).
//! The figures the box is sold on are Dunlop's own M169A manual's: 20 to
//! 600 ms, modulation 0.2 to 2.2 Hz, 2:1 noise reduction, mix full clockwise
//! "half wet/half dry", 1 MΩ in and 1 kΩ out, delay distortion under 1 % at
//! 1 kHz and -5 dBV in, a noise floor of -96 dBV A-weighted at full mix. The
//! parts' data sheets: Panasonic's MN3208 (the BL3208A is Beileng's second
//! source of it), Philips' NE570 (2003; the SA571 is its industrial grade,
//! the same die), the RCA/TI CD4047B and onsemi's MC33178.
//!
//! ```text
//! in -- C33 / R44 -- Q8 follower (R51 1M to VB) -- C38 -- U8A pre-emphasis
//!    (R46 // C35+R47 in, R45 out: x1 below 410 Hz, x5.7 above 2.34 kHz)
//!    -- the dry signal, R48 to the mixer, and
//!    C30 -- R31 -- U7A: SA571 compressor (its gain cell in the loop, R33 /
//!    R34 / C29 for DC), regeneration in through R32
//!    -- R36 / C26, R28 / R27 / C25 / C20 round Q7 -- C21 -- BBD IN
//! BBD: U1-U4, four BL3208A, 4 x 2048 stages, one clock
//! BBD OUT -- R69 / C47, R68 / R66 / C45 / C48 round Q14, R53 / R49 / C37 /
//!    C36 round Q9 -- TR8 -- C41 -- U7B: SA571 expander (R23 / R26 / C23 in
//!    its loop) -- C16 -- REGEN (VR2) back to U7A, MIX (VR1) to the mixer
//! U8B: dry through R48, wet through R38, de-emphasis in its loop -- R41 /
//!    C7 -- out
//! clock: U5A Schmitt and U5B integrator (SPEED TR1, MOD SW1B), WIDTH TR2
//!    into Q2, R6 / C3 to node A; a CD4047 astable (R13, DELAY VR3 with R21,
//!    TR3, C14) with D5 / C13 / C15 / R14 / R15 between node A and its
//!    timing node; Q1 / Q3 drive the BBDs' clock lines
//! ```
//!
//! ## What the drawing says that the box's legend does not
//!
//! The pedal is usually described as two MN3205s with an MN3102 clock. The
//! traced D3 board has **four BL3208As** (2048 stages each) and a **CD4047**
//! astable with MMDT3946 pairs as the clock driver. The stage count is the
//! same -- 8192 either way -- so the delay arithmetic is too: 600 ms is a
//! 6.8 kHz clock, 20 ms one of 205 kHz, outside the MN3208 sheet's 10 to
//! 100 kHz at both ends. The kanengomibako write-up also notes that the
//! emphasis networks and both filters carry exactly the Boss DM-2's values,
//! which the parts here bear out. Earlier boards may differ; this is the one
//! traced.
//!
//! ## What is solved and what is not
//!
//! **Two netlists.** The audio path (this module's `build`) and the clock
//! board (`build_clock`): the LFO, the width trimmer, Q2 and R6 / C3 into
//! node A. Nothing joins them but the supply, which is ideal here, so they are
//! solved apart -- the clock board is cheap on its own and would cost the
//! audio path's Newton boundary its two Schmitt op-amps.
//!
//! **The bucket brigades are not in the netlist**, as in the Blue Chorus: the
//! audio netlist's `BBD_IN` is sent and its `BBD_OUT` returns as an auxiliary
//! input standing at the chips' output level. Between is
//! `dsp::bbd::Clocked`, a model of a brigade at its own clock: sampled at
//! every clock period, held at the output, aliasing and imaging as a slow
//! clock does, its charge-transfer loss, its chips' transfer curve, noise and
//! clock leak. See [`LINE`] for the figures and where each comes from.
//!
//! **The SA571's gain cells are conductances the outboard sets.** The
//! compander's op-amps, its internal resistors (R1 10 k, R2 20 k, R3 20 k, R4
//! 30 k, V_REF 1.8 V) and every external part are solved. Its gain cell --
//! a current-in, current-out multiplier, `I_out = I_in I_G / I_1` with `I_1`
//! 140 µA -- is the one thing a nodal solver has no part for, so it is
//! stamped as a resistor of `R2 I_1 / I_G` from a buffered copy of its input
//! to the op-amp's summing node (`Adjust::RealtimeResistor`): the same
//! current for the same input, with R2's own 20 k load on the pin. `I_G` is
//! the full-wave rectifier's average, `2 |I_rect|` through the internal 10 k
//! and the external C_RECT, run per sample from the rectifier pin's voltage
//! (`Detector`). Attack and decay are both `10 k x C_RECT` as the sheet
//! states: 2.2 ms here (C5, C4).
//!
//! **The clock is a law, read off a transient model of the drawing.** A
//! CD4047 astable runs at hundreds of kilohertz and cannot be stepped at the
//! audio rate. Its period is the data sheet's `4.40 R C` with the driver's
//! output resistance and propagation delay -- but D5 with C13 and C15 sits
//! between its timing node and node A, and that network is how the triangle
//! moves the clock: it pumps charge through D5 at every edge, some hundred
//! microamps, and how much, and how much capacitance it hangs on the timing
//! node while it does, depends on node A's voltage. `transient` (in the tests)
//! steps that network at a few nanoseconds -- the 4047 as two ideal
//! inverters with its sheet's 220 ohm and 125 ns, D5 as a 1N4148, node A held
//! by C3 -- and `CLOCK_RATIO`, `PUMP_*` are its results, re-measured by
//! `the_clock_law_is_the_transient_models`. The averaged pump goes back into
//! the clock board's netlist at node A as a Thevenin source (`PUMP_SOURCE`,
//! `PUMP_RESISTOR`), so node A sits where the pump pushes it -- a volt above
//! Q2's emitter at the shortest delay, half a volt at the longest.
//!
//! **The clock board is stepped at about 12 kHz** (`CLOCK_BOARD_RATE`), a
//! whole fraction of the stage's rate, node A interpolated between its
//! samples: a triangle of a few hertz needs no more, and at 48 kHz it cost
//! as much again as the rest of the outboard.
//!
//! **Held on.** The footswitch and the three MMBFJ177 mutes (Q12 at the input,
//! Q13 in the wet path, Q10 at the output) are in their effect-on states and
//! not built; nor is the LED, nor the supply's reverse-voltage protection.
//!
//! ## The controls
//!
//! DELAY on the slot's drive knob, MIX on its level, REGEN on its first tone
//! knob and the MOD switch on its second, a switch on a knob as the Orange
//! Phase's R28 is: below half way off, above it on. The four internal trimmers
//! sit where the factory leaves them (`TR*` constants).
//!
//! ## What it does (`tests::measurements`, at 48 kHz)
//!
//! - **DELAY** 18.7 ms to 600 ms: 89, 161, 232, 298, 358, 414, 466, 516, 562
//!   ms at its tenths (the manual: 20 to 600). The bottom is the trace's
//!   parts and the 4047's sheet, nothing set to it; the top is TR3's.
//! - **MIX**: none of the echo to as much of it as of the guitar; half way is
//!   10 dB under full, the linear track's 25 k behind R38's 43 k.
//! - **REGEN**: each repeat 9 to 11 dB under the last at half, 4 to 6 dB at
//!   0.8; at full the loop holds a 200 ms echo at its level and runs away at
//!   90 ms, where the filters' peak lifts its gain above one, and still lets
//!   600 ms die away at 2 dB a repeat, the slow clock's loss taking more each
//!   pass. Running away it is held by the expander's op-amp against its
//!   rails, the filters rounding each pass.
//! - **MOD**: the triangle at TR1's 12 o'clock is 1.27 Hz (0.24 to 3.65 Hz
//!   across TR1; the manual says 0.2 to 2.2 -- see below); at TR2's 12
//!   o'clock it sweeps the clock ±0.5 to ±0.7 %, ±10 cents of pitch at a
//!   160 ms delay and ±26 at 600 ms. Off, the clock holds still.
//! - **The wet response** to white noise against the write-up's measured
//!   M169, read off its plot against 100 Hz: at no DELAY +4.1, +5.5, +4.0,
//!   -8.4 dB at 1, 1.5, 2, 3 kHz (measured +3.3, +5.0, +2.3, -10.3); at full
//!   -0.1, -2.4, -8.7, -26.8 (measured -1.3, -3.4, -9.2, -24.2). A steady
//!   tone sees the filters' peak twice -- the expander squares the level the
//!   filters hand it -- so a held note in the upper middle echoes louder than
//!   a bass note: the box's forward, "vocal" repeats.
//! - **Distortion** of the wet path, harmonics 2 to 5: about 0.5 % for a
//!   guitar's 220 Hz at its level, 1.5 % at 1 kHz; at the manual's own test
//!   point (1 kHz, -5 dBV) 4.5 % at 12 o'clock, where the M169A says "under
//!   1 %". The four BBDs add under half a per cent of it; the rest is the
//!   Sallen-Key followers' (`the_filters_followers_are_where_a_hot_echo_frays`).
//! - **At rest** under -115 dBV at full MIX: the expander shuts the BBDs'
//!   hiss and clock away.
//!
//! ## Open questions
//!
//! - The manual's top modulation speed, 2.2 Hz, against TR1's 3.65 Hz here:
//!   with the traced R19, C11 / C12 and R8 / R16 the triangle's slope and its
//!   thresholds put the top there. A misread part, a smaller trimmer stop, or
//!   a manual quoting a narrower range than the trimmer has would each do it.
//! - Its distortion at the manual's test point, above.
//! - Whether REGEN's full turn runs away at long delays on a real unit: here
//!   it depends on TR8's setting twice over (the compander squares the wet
//!   gain), and a dB on TR8 is two on the loop.

use crate::dsp::bbd::{Clocked, ClockedSpec};
use crate::dsp::netlist::{steps, Adjust, BipolarSpec, Circuit, DiodeSpec, Fault, Netlist, Taper};
use crate::dsp::time::Simulation;

// --- controls ---------------------------------------------------------------------

/// REGEN, VR2, 100 k B: the expander's output back into the compressor.
pub const REGEN: usize = 0;
/// MIX, VR1, 100 k B: the wet signal into the mixer.
pub const MIX: usize = 1;
/// TR8, 10 k: the expander's input level (on the audio board).
const EXPANDER_TRIM: usize = 2;
/// MOD, SW1B: R12 across the integrator stops the triangle (on the clock
/// board).
pub const MOD: usize = 3;
/// TR1 SPEED, 500 k, and TR2 WIDTH, 100 k (on the clock board).
const SPEED_TRIM: usize = 4;
const WIDTH_TRIM: usize = 5;
/// DELAY, VR3, 100 k B: in the CD4047's timing network, which is in neither
/// netlist (`timing_resistance`). The outboard takes it.
pub const DELAY: usize = 6;

/// MIX rests at the middle of its travel: the slot's level knob is the pot.
const MIX_REST: f64 = 0.5;

/// TR8, the expander's input trimmer, where it makes the compander unity:
/// the echo at its first repeat as loud as the dry signal at full MIX, a
/// guitar at its nominal level -- the manual's "fully clockwise results in
/// half wet / half dry". The fraction of the track from the wiper to ground,
/// which the expander's rectifier and gain cell (6.7 k inside) load.
/// CALIBRATED (`an_echo_comes_back_after_the_delay_as_loud_as_the_dry`); the
/// write-up's "TR3 and TR8 at about 2 to 3 o'clock" is 0.67-0.83 of the
/// rotation, and this is 2:15.
const TR8_REST: f64 = 0.755;
/// TR1 and TR2 near 12 o'clock, as the write-up finds every trimmer but TR3
/// and TR8. ESTIMATED from that.
const TR1_REST: f64 = 0.5;
const TR2_REST: f64 = 0.5;
/// TR3, a 500 k rheostat across VR3 and R21: the ohms in circuit where it
/// makes full DELAY 600 ms, the manual's longest. CALIBRATED
/// (`the_delay_runs_from_twenty_to_six_hundred_milliseconds`); that is a
/// third of its track, which is the write-up's "2 to 3 o'clock" if, as its
/// lug 1 is the open one, the track turns away from the wiper's used end.
const TR3_OHMS: f64 = 163_000.0;

/// The nodes the outboard reads and drives.
pub const OUTPUT: &str = "out";
pub const BBD_IN: &str = "bbd_in";
/// The SA571's rectifier inputs (pins 2 and 3, 14 and 15 joined) and its
/// reference.
const COMPRESSOR_SENSE: &str = "c_det";
const EXPANDER_SENSE: &str = "e_det";
const VREF_NODE: &str = "vref";
/// Node A on the clock board: R6 / C3, D5's network's other end.
pub const NODE_A: &str = "node_a";

/// The gain cells' realtime resistors on the audio board, and the BBD's
/// return.
const COMPRESSOR_CELL: usize = 0;
const EXPANDER_CELL: usize = 1;
const BBD_RETURN: usize = 0;
/// The pump's source and resistor on the clock board.
const PUMP_SOURCE: usize = 0;
const PUMP_RESISTOR: usize = 0;

// --- the supply and the parts ---------------------------------------------------

/// The battery, through R60 / R61 (10 ohm each) to VCC and VDD.
const BATTERY: f64 = 9.0;
const RAIL_FEED: f64 = 10.0;

/// The SA571's internal parts, from the NE570 sheet's block diagram: R1 into
/// the rectifier, R2 into the gain cell, R3 brought out to pin 6 / 11, R4 to
/// ground from the summing node, V_REF on every summing node, and the cell's
/// reference current I_1.
const SA571_R1: f64 = 10_000.0;
const SA571_R2: f64 = 20_000.0;
const SA571_R3: f64 = 20_000.0;
const SA571_R4: f64 = 30_000.0;
const SA571_VREF: f64 = 1.8;
const SA571_I1: f64 = 140e-6;
/// The rectifier's averaging resistor (R5 inside, 10 k), with C5 and C4,
/// 0.22 µF each: the sheet's `t = 10 k x C_RECT`.
const RECT_TAU: f64 = 10_000.0 * 0.22e-6;
/// The rectifier's own bias current, "typically 50 nA" (the sheet's
/// rectifier notes): the floor under I_G at no signal.
const RECT_BIAS: f64 = 50e-9;
/// Its input saturates near 300 µA (onsemi AND8159: "3 V / R1"); the
/// sheet's advice is to keep under 250.
const RECT_MAX: f64 = 300e-6;
/// The rectifier's two halves read the current through output transistors
/// whose alpha is short by 2.5 % (PNP, positive swings) and 0.5 % (NPN): the
/// sheet's own accounting of its tracking error.
const RECT_POSITIVE: f64 = 0.975;
const RECT_NEGATIVE: f64 = 0.995;
/// The gain cell at no signal: what I_G's floor makes of R2 I_1 / I_G.
const CELL_REST: f64 = SA571_R2 * SA571_I1 / (2.0 * RECT_BIAS);
/// The SA571's op-amps on 9 V: about 1.2 V from ground to 1.4 V from VCC.
/// The sheet gives only ±20 mA and +13 dBm into 300 ohm on 15 V. ESTIMATED.
const SA571_MIDDLE: f64 = 4.4;
const SA571_SWING: f64 = 3.2;
/// U8, an MC33178: to within about 1.1 V of either rail at these loads
/// (onsemi's sheet, 1.1 V typical at 600 ohm on ±15 V). ESTIMATED for 9 V.
const MC33178_SWING: f64 = 3.4;

/// The BBD's bias: TR5's wiper, set where the MN3208 sheet's V_Bias-V_DD curve
/// puts the best point at V_DD = 9 V (about half of it), behind its 12 k or so
/// of trimmer and C39. The other three chips' bias trimmers are inside
/// `Clocked`, which works about the bias.
const BBD_BIAS: f64 = 4.6;
const BBD_BIAS_SOURCE: f64 = 12_400.0;
/// The last chip's output: its level (the MN3208 sheet's V_O-V_I curve, its
/// output stage a source follower about a volt under its input's bias,
/// scaled to 9 V: ESTIMATED), and what TR12 and the two follower outputs look
/// like from R69 -- 10 k's two halves in parallel and the followers' own.
/// ESTIMATED.
const BBD_OUT_LEVEL: f64 = 4.0;
const BBD_OUT_SOURCE: f64 = 3_500.0;

/// Q2, Q7, Q8, Q9, Q14: MMBT5089L. Onsemi's sheet gives hFE 400-1200 at
/// 100 µA; the middle. ESTIMATED, as the catalogue's other high-gain parts.
const MMBT5089: BipolarSpec = BipolarSpec {
    saturation: 1.0e-14,
    forward_beta: 700.0,
    reverse_beta: 4.0,
    early: 100.0,
};

/// U5A, a TL062C half as the Schmitt: 3.5 V/µs and 6 V/mV (TI's sheet,
/// typical), its output about 1.5 V from each rail. Its bandwidth is cut for
/// the solver's sake, as the Blue Chorus's is (see
/// `blue_chorus::COMPARATOR_GBW_HZ`, whose 5 kHz at 44.1 kHz is the ratio
/// kept here): at the clock board's rate a flip then takes some 9 ms instead
/// of microseconds, which slows the triangle by 0.4 % at the bottom of SPEED
/// to 4 % at the top against the same board stepped at 48 kHz
/// (`tests::measurements`). U5B, the integrator, never leaves its
/// linear region (a triangle of ±0.4 V on the bias line) and is an ideal
/// op-amp: the TL062's 1 MHz is nothing to a triangle of a hertz.
const TL062_SLEW: f64 = 3.5e6;
const TL062_OPEN_LOOP: f64 = 6.0e3;
const TL062_HEADROOM: f64 = 1.5;
const COMPARATOR_GBW_PER_RATE: f64 = 5.0e3 / 44_100.0;
const INTEGRATOR: f64 = 1e-9;
/// The clock board runs at about this rate, a whole fraction of the stage's:
/// a triangle of at most 4 Hz, smoothed by R6 / C3 at 10 Hz, needs no more,
/// and the board costs a quarter of what it did at 48 kHz. Node A is
/// interpolated between its samples.
const CLOCK_BOARD_RATE: f64 = 12_000.0;

/// SW1B's other position: R12 out of circuit, as an open contact of 100 MΩ
/// (a leak of four minutes on the integrator). No more: a pot's track is held
/// a ten-thousandth from its ends, so R12 is that fraction of it exactly, and
/// a larger "open" made R12 ten times itself and left the triangle running.
const SWITCH_OPEN: f64 = 1.0e8;
const R12: f64 = 10_000.0;
/// Where the integrator's capacitors start: the triangle just past the
/// Schmitt's lower threshold, so the LFO starts at once and from the same
/// phase every time (`Netlist::charged`).
const TRIANGLE_START: f64 = -0.45;

// --- the bucket brigades ---------------------------------------------------------

/// The four BL3208As as one line: MN3208 figures at V_DD = 9 V.
///
/// - **8192 stages**, four chips of 2048 (traced).
/// - **Insertion gain** +1.2 dB a chip: the MN3208 sheet's G_i-V_DD curve at
///   9 V. Its typical 0 dB is at 5 V. READ OFF THE CURVE.
/// - **Charge-transfer loss** of 1.0 over the line (0.25 a chip). The sheet's
///   G_i-f_i curves fall 4 dB at about a quarter of the clock (10 kHz clock,
///   2.6 kHz), of which the hold costs 1 dB: 0.32 a chip, 1.28 for four. The
///   write-up's measured response of a whole M169 at full DELAY (read off its
///   plot: -1.3, -9.2, -24.2 dB at 1, 2, 3 kHz against 100 Hz) fits 0.8 to
///   1.1 for the line. Between them. ESTIMATED
///   (`the_wet_response_is_the_measured_units`).
/// - **Transfer curve**: second- and third-order terms in the proportion of
///   the sheet's THD-V_I curve (mostly second at low level, third taking over
///   toward V_i(max)), and buckets full at 2.2 V, where V_i(max) at 9 V
///   (+3.5 dBm, 2.5 % THD) puts the knee. Their size is a third of the
///   sheet's (0.2 % a chip at -10 dBm, 9 V): the four chips add under half a
///   per cent at the M169A's own test point (1 kHz, -5 dBV in), whose whole
///   wet path is rated "under 1 %". The BL3208A is not Panasonic's part.
///   ESTIMATED. (The traced filters' followers take more than that on their
///   own there: `the_filters_followers_are_where_a_hot_echo_frays`.)
/// - **Noise**: white plus a part that grows with the holding time, from the
///   sheet's V_no-f_cp curve, A-weighted at a chip's output: -63 dBm at a
///   10 kHz clock, -72 at 100 kHz. Four chips' powers summed, that is 1.1 mV
///   and 0.39 mV, which a held sample spreads over the clock's band: 0.54 mV
///   white and 0.96 mV of leakage at 10 kHz, per sample. READ OFF THE CURVE.
///   (The M169A's floor, -96 dBV A-weighted at full MIX, is not these: the
///   expander takes them down past -130 dBV with nothing playing. It is the
///   NE570 sheet's own expander output noise, 20 µV typical, and like every
///   circuit here the board's own noise is not modelled.)
/// - **Clock leak** of 3 mV either way, what TR12's balance leaves of a few
///   hundred millivolts of pedestal. ESTIMATED.
pub const LINE: ClockedSpec = ClockedSpec {
    stages: 4 * 2048,
    chips: 4,
    gain: 1.148,
    second: 0.004,
    third: 0.006,
    saturation: 2.2,
    inefficiency: 1.0,
    noise: 0.54e-3,
    leakage_noise: 0.96e-3,
    noise_reference_hz: 10_000.0,
    clock_leak: 0.003,
};

// --- the clock -----------------------------------------------------------------

/// The CD4047's timing parts and the D5 network's (traced), with the 4047's
/// own figures at 9 V from the RCA/TI CD4047B sheet: its R and C pins'
/// drivers about 220 ohm (2.6 mA sink at 0.5 V and 10 V, typical), each flip
/// 125 ns late (astable-to-OSC propagation 100 ns typical at 10 V, plus half
/// the 50 ns transition), its threshold half the supply. D5 is a PMBD914:
/// the 1N4148 model's figures. C3 holds node A, and R15 returns to the LFO's
/// bias line (R10 / R17: 4.18 V). READ OFF THE SHEETS, the delay ESTIMATED
/// within them. (The network's own parts are the transient model's, in the
/// tests; at run time they are in the tables.)
const C14: f64 = 470e-12;
const R13: f64 = 390.0;
const R21: f64 = 1_500.0;
const VR3: f64 = 100_000.0;
const DRIVER: f64 = 220.0;
const PROPAGATION: f64 = 125e-9;
#[cfg(test)]
const C13: f64 = 1e-9;
#[cfg(test)]
const C15: f64 = 1e-9;
#[cfg(test)]
const R14: f64 = 7_500.0;
#[cfg(test)]
const R15: f64 = 27_000.0;
#[cfg(test)]
const LFO_BIAS: f64 = BATTERY * 13_000.0 / (15_000.0 + 13_000.0);

/// The resistance between the 4047's R pin (2) and its RC common (3) at a
/// DELAY position: R13, then VR3 with R21 across the part of its track
/// between the wiper and lug 3, all of it with TR3 across. Turning DELAY up
/// puts more of the track in circuit.
pub fn timing_resistance(delay: f64) -> f64 {
    let p = delay.clamp(0.0, 1.0);
    let lower = VR3 * p;
    let upper = VR3 * (1.0 - p);
    let upper = if upper <= 0.0 {
        0.0
    } else {
        upper * R21 / (upper + R21)
    };
    let track = upper + lower;
    R13 + track * TR3_OHMS / (track + TR3_OHMS)
}

/// The clock with no D5 network: the sheet's `4.40 R C` with the driver in
/// series and four flips' delay a period.
pub fn plain_clock_hz(timing: f64) -> f64 {
    1.0 / (4.4 * (timing + DRIVER) * C14 + 4.0 * PROPAGATION)
}

/// The BBDs' clock for a timing resistance and node A's voltage: the plain
/// clock times what the transient model found the network does to it.
pub fn clock_hz(timing: f64, node_a: f64) -> f64 {
    plain_clock_hz(timing) * table(&CLOCK_RATIO, timing, node_a)
}

/// The D5 network's average current into node A at a timing resistance, as a
/// source and its resistance (the current falls as node A rises, in a
/// straight line to within a microamp across the table).
pub fn pump(timing: f64) -> (f64, f64) {
    let at = |row: &[f64; TIMING_POINTS]| interpolate_timing(row, timing);
    let low = at(&PUMP_LOW);
    let high = at(&PUMP_HIGH);
    let slope = ((low - high) / (PUMP_HIGH_V - PUMP_LOW_V)).max(1e-9);
    (PUMP_LOW_V + low / slope, 1.0 / slope)
}

/// Where the tables are taken: timing resistances geometric from 1.6 k to
/// 110 k, node A from 3.0 V to 6.0 V.
const TIMING_POINTS: usize = 12;
const NODE_POINTS: usize = 7;
const TIMING_LOW: f64 = 1_600.0;
const TIMING_HIGH: f64 = 110_000.0;
const NODE_LOW: f64 = 3.0;
const NODE_HIGH: f64 = 6.0;
const PUMP_LOW_V: f64 = 3.5;
const PUMP_HIGH_V: f64 = 5.0;

#[cfg(test)]
fn timing_at(i: usize) -> f64 {
    TIMING_LOW * (TIMING_HIGH / TIMING_LOW).powf(i as f64 / (TIMING_POINTS - 1) as f64)
}

#[cfg(test)]
fn node_at(j: usize) -> f64 {
    NODE_LOW + (NODE_HIGH - NODE_LOW) * j as f64 / (NODE_POINTS - 1) as f64
}

fn timing_position(timing: f64) -> (usize, f64) {
    let x = (timing / TIMING_LOW).max(1e-9).ln() / (TIMING_HIGH / TIMING_LOW).ln()
        * (TIMING_POINTS - 1) as f64;
    let x = x.clamp(0.0, (TIMING_POINTS - 1) as f64);
    let i = (x as usize).min(TIMING_POINTS - 2);
    (i, x - i as f64)
}

fn interpolate_timing(row: &[f64; TIMING_POINTS], timing: f64) -> f64 {
    let (i, w) = timing_position(timing);
    row[i] + (row[i + 1] - row[i]) * w
}

fn table(t: &[[f64; NODE_POINTS]; TIMING_POINTS], timing: f64, node_a: f64) -> f64 {
    let (i, wi) = timing_position(timing);
    let y = ((node_a - NODE_LOW) / (NODE_HIGH - NODE_LOW) * (NODE_POINTS - 1) as f64)
        .clamp(0.0, (NODE_POINTS - 1) as f64);
    let j = (y as usize).min(NODE_POINTS - 2);
    let wj = y - j as f64;
    let row = |k: usize| t[k][j] + (t[k][j + 1] - t[k][j]) * wj;
    row(i) + (row(i + 1) - row(i)) * wi
}

/// The clock over `plain_clock_hz`, by timing resistance (rows) and node A
/// (columns). Measured by `print_the_clock_tables`.
///
/// Down the rows, the network's whole effect: below about 25 k (the shorter
/// half of DELAY) the pump's edges hurry the timing node and the clock runs
/// up to 18 % fast of the formula; above, C13 and C15 hang on it through D5
/// and it runs up to 22 % slow. Across a row, the modulation: node A higher,
/// less of each cycle with D5 conducting, a faster clock -- 1 % a volt at the
/// shortest delay, 5 or 6 in the middle of the travel, flattening at the top
/// of the table where D5 hardly conducts at all.
#[rustfmt::skip]
const CLOCK_RATIO: [[f64; NODE_POINTS]; TIMING_POINTS] = [
    [1.03364, 1.03890, 1.04400, 1.04931, 1.05440, 1.05868, 1.06042],
    [1.03365, 1.04044, 1.04726, 1.05461, 1.06162, 1.06907, 1.07656],
    [1.04048, 1.04948, 1.05886, 1.06847, 1.07819, 1.08845, 1.09897],
    [1.04766, 1.05992, 1.07248, 1.08545, 1.09869, 1.11256, 1.12691],
    [1.05213, 1.06840, 1.08512, 1.10187, 1.12005, 1.13877, 1.15799],
    [1.04398, 1.06440, 1.08683, 1.10941, 1.13269, 1.15710, 1.18309],
    [1.00549, 1.03053, 1.05731, 1.08492, 1.11439, 1.14490, 1.17784],
    [0.94943, 0.97436, 1.00130, 1.02972, 1.05991, 1.09267, 1.12725],
    [0.90720, 0.92846, 0.95112, 0.97496, 1.00053, 1.02695, 1.05394],
    [0.87296, 0.89203, 0.91154, 0.93093, 0.94764, 0.95613, 0.95434],
    [0.82681, 0.84443, 0.86114, 0.86978, 0.86621, 0.86156, 0.85711],
    [0.78334, 0.79649, 0.81074, 0.81101, 0.80746, 0.80495, 0.80249],
];
/// The pump's average current into node A, amps, with node A at
/// `PUMP_LOW_V` and `PUMP_HIGH_V`. Measured by `print_the_clock_tables`.
#[rustfmt::skip]
const PUMP_LOW: [f64; TIMING_POINTS] = [169.323e-6, 140.717e-6, 125.206e-6, 117.897e-6, 114.053e-6, 111.333e-6, 108.942e-6, 105.111e-6, 97.452e-6, 85.022e-6, 68.249e-6, 50.691e-6];
#[rustfmt::skip]
const PUMP_HIGH: [f64; TIMING_POINTS] = [128.559e-6, 102.019e-6, 87.612e-6, 80.975e-6, 77.686e-6, 75.403e-6, 73.508e-6, 70.982e-6, 65.555e-6, 55.956e-6, 43.152e-6, 31.184e-6];

// --- the audio board ------------------------------------------------------------

pub fn build(source: f64, load: f64) -> Result<Circuit, Fault> {
    tap(source, load, OUTPUT)
}

pub fn tap(source: f64, load: f64, at: &str) -> Result<Circuit, Fault> {
    let mut net = Netlist::new("Carbon Echo");

    // The supply, VB (R58 / R65, C9) and the SA571's own reference and
    // output window.
    net.supply("vcc", RAIL_FEED, BATTERY) // R60
        .resistor("vcc", "vb", 10_000.0) // R58
        .resistor("vb", "gnd", 10_000.0) // R65
        .capacitor("vb", "gnd", 47e-6) // C9
        // U7's bandgap. R1 and R2 return to it here, but in the chip they
        // end at virtual grounds that its rectifier's and gain cell's own
        // amplifiers hold at V_REF: their currents never load the reference.
        // So it is stiff. Given even 10 ohm it couples each detector's
        // current into the compressor's non-inverting input, which at no
        // signal has a gain of thousands, and the board hums to itself.
        .supply(VREF_NODE, 0.01, SA571_VREF)
        .supply("u7_mid", 1.0, SA571_MIDDLE);

    // --- input follower and pre-emphasis --------------------------------------
    net.input("in", source)
        .capacitor("in", "n33", 47e-9) // C33
        .resistor("n33", "q8b", 1_000.0) // R44
        .resistor("q8b", "vb", 1_000_000.0) // R51
        .bipolar("vcc", "q8b", "q8e", MMBT5089) // Q8
        .resistor("q8e", "gnd", 10_000.0) // R42
        .capacitor("q8e", "pe", 1e-6) // C38
        .resistor("pe", "m8a", 47_000.0) // R46
        .capacitor("pe", "x35", 6.8e-9) // C35
        .resistor("x35", "m8a", 10_000.0) // R47
        .resistor("dry", "m8a", 47_000.0) // R45 (C34 not fitted)
        .opamp_biased("dry", "vb", "m8a", "vb", MC33178_SWING); // U8A

    // --- U7A, the compressor ------------------------------------------------------
    // In through C30 and R31 to pin 6, R3 inside to the summing node (pin 5);
    // the regeneration through C17 and R32 straight to pin 5. The gain cell
    // from the output (C24, pins 2 and 3) back to pin 5; R33 / R34 with C29
    // the DC loop alone.
    net.capacitor("dry", "c30", 1e-6) // C30
        .resistor("c30", "p6", 39_000.0) // R31
        .resistor("p6", "s1", SA571_R3) // U7A R3
        .resistor("s1", "gnd", SA571_R4) // U7A R4
        .resistor("s1", "dc1", 18_000.0) // R34
        .capacitor("dc1", "gnd", 10e-6) // C29
        .resistor("dc1", "c_out", 18_000.0) // R33
        .capacitor("c_out", COMPRESSOR_SENSE, 10e-6) // C24
        .resistor(COMPRESSOR_SENSE, VREF_NODE, SA571_R1) // U7A R1
        .resistor(COMPRESSOR_SENSE, VREF_NODE, SA571_R2) // U7A R2
        .linear_opamp("c_cell", COMPRESSOR_SENSE, "c_cell");
    let cell = net.adjustable("c_cell", "s1", Adjust::RealtimeResistor, CELL_REST);
    debug_assert_eq!(cell, COMPRESSOR_CELL);
    net.opamp_biased("c_out", VREF_NODE, "s1", "u7_mid", SA571_SWING) // U7A
        .capacitor("rw", "r17", 1e-6) // C17
        .resistor("r17", "s1", 62_000.0); // R32
                                          // C28, 100 pF on pin 8 (THD trim): at V_REF inside, nothing in the band.

    // --- anti-aliasing, into the bucket brigade ------------------------------------
    net.resistor("c_out", "f1", 10_000.0) // R36
        .capacitor("f1", "gnd", 6.8e-9) // C26
        .resistor("f1", "f2", 10_000.0) // R28
        .capacitor("f2", "q7e", 82e-9) // C25
        .resistor("f2", "q7b", 10_000.0) // R27
        .capacitor("q7b", "gnd", 330e-12) // C20
        .bipolar("vcc", "q7b", "q7e", MMBT5089) // Q7
        .resistor("q7e", "gnd", 10_000.0) // R35
        .capacitor("q7e", BBD_IN, 1e-6) // C21
        .resistor(BBD_IN, "tr5", 100_000.0) // R24
        .supply("tr5", BBD_BIAS_SOURCE, BBD_BIAS) // TR5
        .capacitor("tr5", "gnd", 1e-6); // C39

    // --- out of the bucket brigade: reconstruction ---------------------------------
    let ret = net.aux_input_at("tp10", BBD_OUT_SOURCE, BBD_OUT_LEVEL);
    debug_assert_eq!(ret, BBD_RETURN);
    net.resistor("tp10", "r1", 10_000.0) // R69
        .capacitor("r1", "gnd", 2.2e-9) // C47
        .resistor("r1", "r2", 10_000.0) // R68 (C49 / Q13, the mute, off)
        .capacitor("r2", "q14e", 33e-9) // C45
        .resistor("r2", "q14b", 10_000.0) // R66
        .capacitor("q14b", "gnd", 1e-9) // C48
        .bipolar("vcc", "q14b", "q14e", MMBT5089) // Q14
        .resistor("q14e", "gnd", 10_000.0) // R67
        .resistor("q14e", "r3", 10_000.0) // R53
        .capacitor("r3", "q9e", 39e-9) // C37
        .resistor("r3", "q9b", 10_000.0) // R49
        .capacitor("q9b", "gnd", 330e-12) // C36
        .bipolar("vcc", "q9b", "q9e", MMBT5089) // Q9
        .rest(EXPANDER_TRIM, TR8_REST)
        .pot("q9e", "tr8", "gnd", 10_000.0, Taper::Linear, EXPANDER_TRIM) // TR8
        .capacitor("tr8", EXPANDER_SENSE, 10e-6); // C41

    // --- U7B, the expander ------------------------------------------------------
    // The gain cell from pins 14 and 15 into the summing node (pin 12, not
    // brought out); R3 inside to pin 11, and R23 from there to the output with
    // R26 / C23 to ground at the joint: a T of 58 k at audio, 40 k at DC.
    net.resistor(EXPANDER_SENSE, VREF_NODE, SA571_R1) // U7B R1
        .resistor(EXPANDER_SENSE, VREF_NODE, SA571_R2) // U7B R2
        .linear_opamp("e_cell", EXPANDER_SENSE, "e_cell");
    let cell = net.adjustable("e_cell", "s2", Adjust::RealtimeResistor, CELL_REST);
    debug_assert_eq!(cell, EXPANDER_CELL);
    net.resistor("s2", "gnd", SA571_R4) // U7B R4
        .resistor("s2", "p11", SA571_R3) // U7B R3
        .resistor("p11", "e_out", 20_000.0) // R23
        .resistor("p11", "c23", 22_000.0) // R26
        .capacitor("c23", "gnd", 10e-6) // C23
        .opamp_biased("e_out", VREF_NODE, "s2", "u7_mid", SA571_SWING); // U7B
                                                                        // C18, 100 pF on pin 9: as C28.

    // --- REGEN and MIX ------------------------------------------------------------
    net.capacitor("e_out", "eo", 1e-6) // C16
        .pot("eo", "rw", "gnd", 100_000.0, Taper::Linear, REGEN) // VR2
        .rest(MIX, MIX_REST)
        .pot("eo", "mw", "gnd", 100_000.0, Taper::Linear, MIX) // VR1
        .capacitor("mw", "c19", 1e-6) // C19
        .resistor("c19", "m8b", 43_000.0); // R38

    // --- U8B, the mixer and de-emphasis ---------------------------------------------
    net.resistor("dry", "m8b", 43_000.0) // R48
        .resistor("o8b", "m8b", 47_000.0) // R40
        .capacitor("o8b", "x31", 6.8e-9) // C31
        .resistor("x31", "m8b", 10_000.0) // R39 (C32 not fitted)
        .opamp_biased("o8b", "vb", "m8b", "vb", MC33178_SWING) // U8B
        .resistor("o8b", "o41", 1_000.0) // R41
        .capacitor("o41", OUTPUT, 1e-6) // C7
        .resistor(OUTPUT, "gnd", 47_000.0) // R52
        .resistor(OUTPUT, "gnd", load);

    net.build(at)
}

// --- the clock board --------------------------------------------------------------

/// A TL062 half as the solver's transconductor into an integrator behind a
/// follower, its swing two diodes on the integrator node (as the Blue
/// Chorus's TL022s, on a ground-referred supply).
fn tl062(net: &mut Netlist, prefix: &str, plus: &str, minus: &str, out: &str, gbw: f64) {
    let stage = format!("{prefix}_int");
    let high = format!("{prefix}_high");
    let low = format!("{prefix}_low");
    let gm = std::f64::consts::TAU * gbw * INTEGRATOR;
    let drop = 0.55;
    net.transconductor(plus, minus, &stage, "gnd", gm, TL062_SLEW * INTEGRATOR)
        .capacitor(&stage, "gnd", INTEGRATOR)
        .resistor(&stage, "gnd", TL062_OPEN_LOOP / gm)
        .supply(&high, 1.0, BATTERY - TL062_HEADROOM - drop)
        .supply(&low, 1.0, TL062_HEADROOM + drop)
        .diode(&stage, &high, DiodeSpec::SILICON)
        .diode(&low, &stage, DiodeSpec::SILICON)
        .opamp(out, &stage, out, 1_000.0);
}

/// The LFO, the width trimmer and Q2, into node A: what moves the clock.
/// `rate` is the rate it will be stepped at (the Schmitt's bandwidth follows
/// it).
pub fn build_clock(rate: f64) -> Result<Circuit, Fault> {
    let mut net = Netlist::new("Carbon Echo clock");
    // No signal comes in; the input is there because a circuit has one, and
    // reaches node A as a teraohm on the bias line.
    net.input("lb", 1e12)
        .supply("vcc", RAIL_FEED, BATTERY) // R60
        // The LFO's bias line: R10 / R17, C1. R9 and R7 are links.
        .resistor("vcc", "lb", 15_000.0) // R10
        .resistor("lb", "gnd", 13_000.0) // R17
        .capacitor("lb", "gnd", 47e-6) // C1
        // U5A: the triangle in through R8 and its own square back through
        // R16, both into its non-inverting input; the inverting on the bias.
        .resistor("sq", "sp", 470_000.0) // R16
        .resistor("tri", "sp", 62_000.0); // R8
    tl062(
        &mut net,
        "u5a",
        "sp",
        "lb",
        "sq",
        COMPARATOR_GBW_PER_RATE * rate,
    );
    // SPEED across the square and R11, its wiper through R19 into the
    // integrator: C12 and C11, 4.7 µF back to back, 2.35 µF.
    net.rest(SPEED_TRIM, TR1_REST)
        .pot("sq", "tr1", "r11", 500_000.0, Taper::Linear, SPEED_TRIM) // TR1
        .resistor("r11", "lb", 39_000.0) // R11
        .resistor("tr1", "im", 220_000.0) // R19
        .capacitor("tri", "im", 2.35e-6) // C12 + C11
        .charged("tri", "im", TRIANGLE_START)
        // MOD off puts R12 across the integrator (SW1B): a gain of a twentieth,
        // and the triangle stops where it is.
        .rest(MOD, 1.0)
        .pot(
            "im",
            "im",
            "tri",
            SWITCH_OPEN,
            steps([R12 / SWITCH_OPEN, 1.0]),
            MOD,
        ) // SW1B, R12
        .linear_opamp("tri", "lb", "im"); // U5B
                                          // WIDTH across the triangle and the bias line, into Q2's base; Q2's
                                          // emitter through R6 to node A, C3 holding it against the bias line.
    net.rest(WIDTH_TRIM, TR2_REST)
        .pot("tri", "tr2", "lb", 100_000.0, Taper::Linear, WIDTH_TRIM) // TR2
        .bipolar("vcc", "tr2", "q2e", MMBT5089) // Q2
        .resistor("q2e", "gnd", 10_000.0) // R5
        .resistor("q2e", NODE_A, 7_500.0) // R6
        .capacitor(NODE_A, "lb", 2.2e-6); // C3
                                          // D5's network as its average: the pump's source behind its resistance,
                                          // both set from DELAY by the outboard (`Echo::set_delay`).
    let (_, ohms) = pump(timing_resistance(0.5));
    let source = net.aux_input("pump", 1.0);
    debug_assert_eq!(source, PUMP_SOURCE);
    let resistor = net.adjustable("pump", NODE_A, Adjust::Resistor, ohms);
    debug_assert_eq!(resistor, PUMP_RESISTOR);
    net.build(NODE_A)
}

// --- the outboard -----------------------------------------------------------------

/// One half of the SA571: its full-wave rectifier's average.
#[derive(Clone, Copy, Debug, Default)]
struct Detector {
    average: f64,
}

impl Detector {
    fn step(&mut self, current: f64, k: f64) {
        let rectified = if current >= 0.0 {
            current * RECT_POSITIVE
        } else {
            -current * RECT_NEGATIVE
        };
        self.average += k * (rectified.min(RECT_MAX) - self.average);
    }

    /// The gain cell as a resistance: R2 I_1 / I_G, I_G twice the average.
    fn cell(&self) -> f64 {
        SA571_R2 * SA571_I1 / (2.0 * (self.average + RECT_BIAS))
    }
}

/// Everything about the Carbon Echo that is not its audio netlist: the clock
/// board, the CD4047's law, the four BBDs and the compander's two rectifiers.
/// Built with the stage's circuit off the audio thread; nothing here
/// allocates afterwards.
pub struct Echo {
    clock: Simulation,
    line: Clocked,
    rate: f64,
    send: usize,
    vref: usize,
    compressor_sense: usize,
    expander_sense: usize,
    node_a: usize,
    compressor: Detector,
    expander: Detector,
    detector_k: f64,
    /// The DELAY knob's timing resistance, and the pump's source for it.
    timing: f64,
    pump_volts: f64,
    /// The clock board steps once every `every` samples; node A between its
    /// last two steps, and where in that span this sample is.
    every: usize,
    count: usize,
    was: f64,
    then: f64,
    node: f64,
    fresh: bool,
    returned: f64,
}

impl Echo {
    /// For the audio board `circuit` running at `rate`.
    pub fn new(circuit: &Circuit, rate: f64) -> Result<Self, Fault> {
        let missing = |what: &str| Fault::Malformed(format!("the Carbon Echo has no '{what}'"));
        let find = |name: &str| circuit.unknown_named(name).ok_or_else(|| missing(name));
        let every = (rate / CLOCK_BOARD_RATE).round().max(1.0) as usize;
        let board_rate = rate / every as f64;
        let clock_board = build_clock(board_rate)?;
        let node_a = clock_board
            .unknown_named(NODE_A)
            .ok_or_else(|| missing(NODE_A))?;
        let mut echo = Self {
            clock: Simulation::new(clock_board, board_rate),
            line: Clocked::new(LINE, 0x4d31_3639),
            rate,
            send: find(BBD_IN)?,
            vref: find(VREF_NODE)?,
            compressor_sense: find(COMPRESSOR_SENSE)?,
            expander_sense: find(EXPANDER_SENSE)?,
            node_a,
            compressor: Detector::default(),
            expander: Detector::default(),
            detector_k: 1.0 - (-1.0 / (rate * RECT_TAU)).exp(),
            timing: 0.0,
            pump_volts: 0.0,
            every,
            count: 0,
            was: 0.0,
            then: 0.0,
            node: 0.0,
            fresh: true,
            returned: 0.0,
        };
        echo.set_delay(0.5);
        Ok(echo)
    }

    /// The DELAY knob, 0..1.
    pub fn set_delay(&mut self, position: f64) {
        let timing = timing_resistance(position);
        if timing != self.timing {
            self.timing = timing;
            let (volts, ohms) = pump(timing);
            self.pump_volts = volts;
            self.clock.set_value(PUMP_RESISTOR, ohms);
        }
    }

    /// A knob of the slot's: DELAY is taken here, MOD and the clock board's
    /// trimmers go to the clock board, the rest are the audio board's.
    pub fn set_control(&mut self, which: usize, position: f64) {
        match which {
            DELAY => self.set_delay(position),
            MOD | SPEED_TRIM | WIDTH_TRIM => self.clock.set_control(which, position),
            _ => {}
        }
    }

    /// Before the audio board's sample: the gain cells and the BBD's output.
    #[inline]
    pub fn before(&self, sim: &mut Simulation) {
        sim.set_realtime_value(COMPRESSOR_CELL, self.compressor.cell());
        sim.set_realtime_value(EXPANDER_CELL, self.expander.cell());
        sim.set_aux_input(BBD_RETURN, self.returned);
    }

    /// After it: the rectifiers, the clock board's sample when one is due,
    /// and the BBDs.
    #[inline]
    pub fn after(&mut self, sim: &Simulation) {
        let vref = sim.voltage_at(self.vref);
        let k = self.detector_k;
        self.compressor
            .step((sim.voltage_at(self.compressor_sense) - vref) / SA571_R1, k);
        self.expander
            .step((sim.voltage_at(self.expander_sense) - vref) / SA571_R1, k);
        if self.count == 0 {
            self.clock.set_aux_input(PUMP_SOURCE, self.pump_volts);
            self.clock.process(0.0);
            let now = self.clock.voltage_at(self.node_a);
            self.was = if self.fresh { now } else { self.then };
            self.then = now;
            self.fresh = false;
        }
        self.count += 1;
        self.node = self.was + (self.then - self.was) * self.count as f64 / self.every as f64;
        if self.count == self.every {
            self.count = 0;
        }
        let hz = clock_hz(self.timing, self.node);
        let send = sim.voltage_at(self.send) - BBD_BIAS;
        self.returned = self.line.process(send, hz, self.rate);
    }

    /// The BBDs' clock now.
    pub fn clock_hz(&self) -> f64 {
        clock_hz(self.timing, self.node)
    }

    /// The delay the BBDs give now, in seconds.
    pub fn delay_seconds(&self) -> f64 {
        self.line.delay_at(self.clock_hz())
    }

    /// Node A, where the triangle reaches the clock.
    pub fn node_a(&self) -> f64 {
        self.node
    }

    /// Back to rest with the audio board (call after its `reset_deferred`):
    /// empty buckets, rectifiers at nothing, the clock board to find its
    /// operating point again on its next sample.
    pub fn reset(&mut self, sim: &mut Simulation) {
        self.line.reset();
        self.compressor = Detector::default();
        self.expander = Detector::default();
        self.returned = 0.0;
        self.clock.reset_deferred();
        self.count = 0;
        self.fresh = true;
        self.before(sim);
    }

    pub fn set_realtime_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.clock.set_realtime_deadline(deadline);
    }

    pub fn deadline_aborts(&self) -> u64 {
        self.clock.deadline_aborts()
    }

    /// Another channel's exact state. Copies into existing storage only.
    pub fn copy_runtime_state_from(&mut self, source: &Self) {
        self.clock.copy_runtime_state_from(&source.clock);
        self.line.copy_runtime_state_from(&source.line);
        self.compressor = source.compressor;
        self.expander = source.expander;
        if self.timing != source.timing {
            self.timing = source.timing;
            self.pump_volts = source.pump_volts;
            self.clock.set_value(PUMP_RESISTOR, pump(self.timing).1);
        }
        self.count = source.count;
        self.was = source.was;
        self.then = source.then;
        self.node = source.node;
        self.fresh = source.fresh;
        self.returned = source.returned;
    }
}

#[cfg(test)]
mod tests;
