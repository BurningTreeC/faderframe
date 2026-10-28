//! The Juicer: a JFET across the signal ahead of an op-amp, opened by a
//! germanium diode's reading of the op-amp's output.
//!
//! Engineering reference (developer documentation, not panel text): the Dan
//! Armstrong Orange Squeezer, from General Guitar Gadgets' drawing of the
//! original module (version 2012January12) and their replica's notes and
//! measured voltages (2007June12, 2013June11).
//!
//! ```text
//! in -- C1 / R2 82 k -- A -- C4 -- IC1 (1/2 JRC4558), x23 (R9 / R10, C5)
//!    -- C6 -- O -- VOLUME -- out
//! Q2 2N5457 from A to S, its gate at half A's swing (C2 / R3 against R6)
//!    and at what C7 holds; S a constant voltage: Q1's current through the
//!    BIAS trimmer, C3 holding it still
//! O -- R11 1k5 -- D1 1N100 -- C7 4.7 uF // R12 100 k -- R6 -- Q2's gate
//! ```
//!
//! A different machine from the Red Squeeze. There the gain element is an
//! amplifier whose gain is set by a current; here it is a resistor -- Q2's
//! channel, a variable shunt against R2's 82 k ahead of a fixed-gain stage --
//! and the detector is one germanium diode charging a capacitor, half-wave,
//! from the output. The channel is linearised the classic way, its gate fed
//! half the drain's own swing, so it attenuates rather than distorts; the
//! squeeze comes on gradually (Q2's square law near pinch-off) and goes away
//! with C7 over R12, half a second. That gentleness is the pedal.
//!
//! **The bias.** Q1 is a constant-current source (gate tied to the far end of
//! R5) whose current, through the BIAS trimmer, sets S; Q2's gate rests at
//! ground, so S is how far below pinch-off Q2 sits with nothing to squeeze.
//! The two JFETs being one type, the current tracks the part: the procedure
//! (GGG: turn down "until the sound dies, then slowly clockwise until you get
//! the best sounding compression; turning too far will lead to distortion")
//! puts Q2 at its pinch-off at rest. `BIAS_LAW` makes that the knob's noon.
//!
//! **Referenced to ground** with a battery of its own: the op-amp's swing is
//! measured from a 4.5 V point that only its model uses (`RAILS`), since the
//! circuit's own half-supply (R4 / R8, 4.92 V) carries the signal.
//!
//! The original had no controls outside the box: the BIAS trimmer and the
//! volume trimmer were inside, set once. GGG's replica brings both out, as
//! "Bias Trim" and "Volume"; so does the slot.
//!
//! **What it measures** (`tests`; no published measurement of the pedal was
//! found, so its times are held to the parts that set them): at the bias
//! procedure's point a quiet note gets 21 dB, and from -40 dBV up the output
//! rises 0.4 dB a decibel -- a ratio near 2.5:1, with no ceiling; the
//! overshoot at a step dies in 6 ms (R11 into C7: 7 ms), two thirds of the
//! release is done in 0.36 s and all but a decibel in 0.64 s (C7 over R12:
//! 0.47 s). A string left ringing falls by half as many decibels.

use crate::dsp::netlist::{Circuit, DiodeSpec, Fault, JfetSpec, Netlist, Taper};

/// R7, the 10 k BIAS trimmer, a rheostat from S to ground. The Drive knob
/// turns it: up is more resistance, a higher S, Q2 further from conducting
/// -- less squeeze, more of the op-amp, and at the top, its clipping.
pub const BIAS: usize = 0;
/// R13, the 10 k volume trimmer, from O to ground with the output on its
/// wiper.
pub const VOLUME: usize = 1;

/// Where VOLUME rests: unity through the pedal at a guitar's level with the
/// bias at noon (`calibrate_the_volume_rest`).
pub const VOLUME_REST: f64 = 0.904;

/// BIAS's law: the trimmer's resistance in circuit (as a fraction of its
/// 10 k) at points of the knob's travel, straight between them.
///
/// The trimmer is a plain track set once inside the box, and on it
/// everything the pedal does lies in a fifth of the turn: below 3.0 k Q2 is
/// held open and the sound dies ("until the sound dies"), from 3.0 k to
/// 4.7 k the squeeze goes from the deepest that still plays to none (a
/// quiet note from 18 dB under the procedure's level to 15 dB over it), and
/// beyond, the pedal is a limiter with a rising threshold until the op-amp
/// clips ("turning too far will lead to distortion"). A panel knob gets the
/// useful part, spread evenly (`the_bias_knob_spreads_the_squeeze_evenly`):
/// the deepest squeeze at the bottom; the squeeze easing by even decibels
/// to the bias procedure's point -- Q2 just pinched off with nothing to
/// squeeze, 3.77 k -- at noon (`the_bias_procedure_is_the_knobs_noon`) and
/// on to the full gain at 0.7; then the limiter into the clipping. The
/// points are where a quiet note (-30 dBV) measured those levels on the
/// model, every 4.5 dB below noon and every 7.5 dB above.
pub const BIAS_LAW: [(f64, f64); 9] = [
    (0.0, 0.3033),
    (0.125, 0.3343),
    (0.25, 0.3530),
    (0.375, 0.3659),
    (0.5, 0.3770),
    (0.6, 0.4062),
    (0.7, 0.4674),
    (0.85, 0.6999),
    (1.0, 1.0),
];

pub const OUTPUT: &str = "out";
/// C7: the detector's reading, which Q2's gate rests at.
pub const DETECTOR: &str = "det";
/// The source of Q2: the bias point.
pub const BIAS_POINT: &str = "s";

/// Q1 and Q2, 2N5457: the catalogue's figures for the part
/// (`JfetSpec::J2N5457`, Idss 3 mA, pinch-off -1.5 V, inside the data
/// sheet's 1-5 mA and -0.5 to -6 V). GGG's measured voltages (Q1 source
/// 1.6 V above its gate, Q2's source at 2.6 V) say their pair pinched off
/// nearer -3 V: the spread the trimmer exists for.
const J2N5457: JfetSpec = JfetSpec::J2N5457;

/// D1, the 1N100, a point-contact germanium diode: the catalogue's germanium
/// figures, 0.12 V at 10 uA and 0.26 V at 1 mA. GGG: "it must be germanium".
const D1N100: DiodeSpec = DiodeSpec::GERMANIUM;

/// The JRC4558's swing either way of the middle of a 9 V battery: within
/// about a volt and a half of each rail, as for the TS808's.
const SWING: f64 = 3.0;
const BATTERY: f64 = 9.0;
/// The battery behind its leads. ESTIMATED.
const BATTERY_OHMS: f64 = 10.0;
/// The point the op-amp model's swing is measured from.
const RAILS: &str = "rails";

pub fn build(source: f64, load: f64) -> Result<Circuit, Fault> {
    tap(source, load, OUTPUT)
}

pub fn tap(source: f64, load: f64, at: &str) -> Result<Circuit, Fault> {
    let mut net = Netlist::new("Juicer");
    net.rest(VOLUME, VOLUME_REST);

    net.supply("vcc", BATTERY_OHMS, BATTERY)
        .supply(RAILS, 1.0, BATTERY / 2.0);

    // --- the shunt ------------------------------------------------------------------
    net.input("in", source)
        .capacitor("in", "n1", 0.047e-6) // C1
        .resistor("n1", "a", 82_000.0) // R2
        .jfet("a", "g", BIAS_POINT, J2N5457) // Q2
        .capacitor("a", "c2", 2200e-12) // C2
        .resistor("c2", "g", 470_000.0) // R3
        .resistor("g", DETECTOR, 470_000.0); // R6

    // --- the bias -----------------------------------------------------------------------
    // Q1's gate on S, its source through R5: a current source into S. BIAS
    // with its wiper on the `a` end leaves `R f` in circuit.
    net.jfet("vcc", BIAS_POINT, "q1s", J2N5457) // Q1
        .resistor("q1s", BIAS_POINT, 2_400.0) // R5
        .capacitor(BIAS_POINT, "gnd", 4.7e-6) // C3
        .pot(
            BIAS_POINT,
            BIAS_POINT,
            "gnd",
            10_000.0,
            Taper::Table(&BIAS_LAW),
            BIAS,
        ); // R7

    // --- the amplifier -------------------------------------------------------------------
    net.capacitor("a", "p", 0.047e-6) // C4
        .resistor("vcc", "p", 390_000.0) // R4
        .resistor("p", "gnd", 470_000.0) // R8
        .opamp_biased("o1", "p", "m", RAILS, SWING) // IC1
        .resistor("o1", "m", 220_000.0) // R9
        .resistor("m", "c5", 10_000.0) // R10
        .capacitor("c5", "gnd", 4.7e-6); // C5

    // --- the output and the detector -----------------------------------------------
    net.capacitor("o1", "o", 4.7e-6) // C6
        .pot("o", OUTPUT, "gnd", 10_000.0, Taper::Audio, VOLUME) // R13
        .resistor(OUTPUT, "gnd", load)
        .resistor("o", "d1", 1_500.0) // R11
        .diode("d1", DETECTOR, D1N100) // D1
        .capacitor(DETECTOR, "gnd", 4.7e-6) // C7
        .resistor(DETECTOR, "gnd", 100_000.0); // R12

    net.build(at)
}

#[cfg(test)]
mod tests;
