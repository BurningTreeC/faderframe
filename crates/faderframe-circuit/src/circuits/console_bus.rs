//! Console mix buses: where a console's channels are summed and the sum
//! goes out to a line.
//!
//! Engineering reference (developer documentation, not panel text): the
//! virtual-earth mixing amplifier — every channel's output through its own
//! resistor into an amplifier's inverting input held at ground by
//! feedback, so the currents add and the output is minus the feedback
//! resistor times their sum — as the textbooks and the consoles draw it;
//! the parts it is built from are the ones the preamp netlists already
//! take from their drawings and data sheets (API's 2520 and 2503 in
//! `american312`, the 5534s on SSL's ±18 V cards in `console_e`, the
//! 1073's output block in `neve`).
//!
//! The channels' resistors are drawn as one: the summed signal through one
//! summing resistor carries exactly the current the channels' resistors
//! together would. What the number of channels changes on a real bus — the
//! amplifier's noise gain, and with it its noise and its loop gain — does
//! not show here: the op-amps are ideal with a rail, as in the preamps,
//! and noise is not simulated. What does show is what the bus does at the
//! level it is driven: the rails, the iron.
//!
//! ## What is measured and what is estimated
//!
//! Measured (from the preamps' references): the 2520's swing, the 2503's
//! windings and core, the 5534s' rails, the 1073 output block's parts.
//!
//! Estimated: the summing and feedback resistors (10 k, the usual value;
//! the API bus's 5 k feedback makes the op-amp's half and the 2503's 1:2
//! give unity, as a program amplifier into that transformer would be
//! strapped), the stability capacitors across the feedback, the loads.

use crate::circuits::{american312, console_e, neve};
use crate::dsp::netlist::{Circuit, Fault, Netlist};

/// The families, in catalogue order (persistent).
pub const FAMILIES: [&str; 3] = ["American", "British 4K", "British 73"];

/// The summing resistor: each channel's, and the one drawn for them all.
const SUMMING: f64 = 10_000.0;

/// The bus of `family` into `load` (ohms).
pub fn build(family: usize, load: f64) -> Result<Circuit, Fault> {
    match family {
        0 => american(load),
        1 => british_4k(load),
        _ => british_73(load),
    }
}

/// A 2520 summing into a 2503 strapped 1:2: the op-amp works at half the
/// line's level and the transformer makes it up, so the bus clips where
/// the 2520 does, a little over +26 dBu at the line, and the iron runs out
/// in the lows near there.
fn american(load: f64) -> Result<Circuit, Fault> {
    let mut net = Netlist::new("American mix bus");
    net.input("in", 50.0)
        .resistor("in", "minus", SUMMING)
        .opamp("out", "gnd", "minus", american312::RAIL)
        .resistor("out", "minus", SUMMING / 2.0)
        .capacitor("out", "minus", 100e-12);
    net.resistor("out", "t1p", american312::OUTPUT_PRIMARY_R)
        .core("t1p", "gnd", american312::OUTPUT_CORE)
        .transformer("t1p", "gnd", "t1s", "gnd", american312::OUTPUT_RATIO)
        .resistor("t1s", "line", american312::OUTPUT_SECONDARY_R)
        .resistor("line", "gnd", load);
    net.build("line")
}

/// A 5534 summing, a second 5534 turning it back the right way up (the hot
/// leg of the balanced output): transformerless, clean until the ±16 V the
/// cards' rails let them swing, about +23 dBu.
fn british_4k(load: f64) -> Result<Circuit, Fault> {
    let mut net = Netlist::new("British 4K mix bus");
    net.input("in", 50.0)
        .resistor("in", "m1", SUMMING)
        .opamp("o1", "gnd", "m1", console_e::RAIL)
        .resistor("o1", "m1", SUMMING)
        .capacitor("o1", "m1", 33e-12)
        .resistor("o1", "m2", SUMMING)
        .opamp("line", "gnd", "m2", console_e::RAIL)
        .resistor("line", "m2", SUMMING)
        .capacitor("line", "m2", 33e-12)
        .resistor("line", "gnd", load);
    net.build("line")
}

/// The summed channels into the 1073's output block (its class-A line
/// driver and the transformer it works into), from the impedance of the
/// summing network, into the 600 ohm line it was built to drive: lighter
/// loads let the block's estimated leakage ring against C26 (+1.6 dB at
/// 20 kHz into 10 k, +0.8 dB into 600 ohms).
fn british_73(_load: f64) -> Result<Circuit, Fault> {
    neve::output(SUMMING / 4.0, 600.0)
}
