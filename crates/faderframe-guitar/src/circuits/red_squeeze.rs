//! The Red Squeeze: an OTA whose gain is the current its own output's
//! envelope leaves it, between an emitter follower and a phase splitter.
//!
//! Engineering reference (developer documentation, not panel text): the MXR
//! Dyna Comp, the CA3080 ("script") version, from ElectroSmash's "MXR Dyna
//! Comp Analysis" (schematic, parts list and bias calculations); the CA3080
//! from Intersil's data sheet FN475.5 (`OtaSpec::CA3080`); behaviour checked
//! against O. Kroening, K. Dempwolf and U. Zoelzer, "Analysis and Simulation of
//! an Analog Guitar Compressor", DAFx-11 (Paris, 2011), whose Figures 5 and 6
//! are measurements of a real Dyna Comp: its static curves at both ends of
//! Sensitivity, and its envelope across a 10/100 mV step.
//!
//! ```text
//! in -- C1 / R1 -- Q1 follower -- C2 -- pin 2 (-), and through the 2 k trimmer
//!    to pin 3 (+), which R7 // C4 and C3 tie to AC ground: the OTA amplifies
//!    the small difference the trimmer leaves, more of it above a kilohertz
//! U1 CA3080 -- its current into R8 // C5 (back to the 2.54 V bias) -- Q2,
//!    a phase splitter: R9 and R10 equal, so collector and emitter swing equal
//!    and opposite
//! Q2's emitter -- C9 / R14 -- OUTPUT -- out
//! Q2's collector and emitter -- C6 / C7 into Q3 and Q4 (D1 / D2 clamping the
//!    bases below ground): on each peak one of them pulls C8 down; R13
//!    charges it back up from the battery -- Q5 follows C8 -- SENSITIVITY and
//!    R17 -- pin 5, the amplifier bias current
//! ```
//!
//! So the loop is a feedback compressor with an infinite ratio: a louder
//! output discharges C8, which takes current from the OTA's bias pin, which
//! takes gain away, until the output is back where the rectifiers just
//! conduct. Q3 and Q4 discharge C8 with collector currents of milliamps on a
//! peak -- the attack is a few milliseconds, and before the loop catches up a
//! pluck's first cycles come through at the gain the quiet before it left,
//! which is the "squash" -- and R13's 150 k charges 10 uF back with a time
//! constant of 1.5 s, which is the sustain. Sensitivity is the most current
//! C8 can deliver into pin 5: how much gain there is to give a quiet note.
//!
//! **Against the measurements** (`tests`): at full Sensitivity a quiet note
//! gets 34.1 dB (the paper's unit about 32 dB; MXR rates the M102's maximum
//! compression at 36 dB) and everything from -40 dBV up comes out between
//! -12.4 and -11.7 dBV (the paper: about -11 dBV, from -50 dBV up); the
//! overshoot at a step dies with a time constant of 4 ms (MXR: attack 5 ms)
//! and the gain is back within a decibel of rest 0.90 s after the step down
//! (MXR: release 1 s; the paper's envelope about 1.1 s, and two thirds of
//! the way back in decibels at 0.35 s against 0.31 here). With no
//! Sensitivity the paper's unit had about 9 dB less gain than this (+3 dB
//! against +11): unexplained here. A weaker part would do it (the sheet
//! gives the CA3080's transconductance a 2:1 spread) or a pot wired to leave
//! more resistance at its end; the drawings settle neither.
//!
//! **The disagreements.** ElectroSmash draws R16, the bias divider's lower
//! leg, as 22 k (2.54 V); the DAFx paper's traced unit -- a later one, an
//! LM13700 in the CA3080's place -- has 27 k (2.93 V), as does every Ross.
//! The 22 k is built: it is the CA3080 drawing. Neither
//! drawing decouples the bias line or the battery (ElectroSmash suggests C10
//! and C11 as an improvement, the DAFx unit has 1 uF across its battery), so
//! neither is built: the OTA's output current returns through R8 into the
//! divider's 16 k, which is part of the circuit's gain. ElectroSmash's
//! calculation of the inputs' bias (3.1 V) takes the CA3080's input resistance
//! as a resistance to ground; it is a differential one, and the solve here
//! gives 4.5 V -- what builders measure on pins 2 and 3 (General Guitar
//! Gadgets' Ross: 4.4 V). Mad Bean's Ambulator, a Dyna Comp on 2N3904s whose
//! every pin voltage its build notes list, is the operating point this one
//! is held to (`the_operating_point_is_a_real_ones`).
//!
//! Not built: D3, the reverse-polarity diode across the battery, which a
//! battery fitted the right way round never forward biases.
//!
//! ## The Ross
//!
//! The Ross Compressor is this circuit with different values (`ROSS`, from
//! Fuzz Central's drawing of an original grey unit, cross-checked against the
//! voltages in General Guitar Gadgets' replica notes): a 27 k bias leg; 220 pF
//! across the input; Q1's base fed through two 470 k with 1 uF at their middle,
//! and a 10 k collector resistor; the trimmer fed through two 220 k with 1 uF
//! at their middle; a 100 k linear Level (its 2SC1849s are the 2N3904s here:
//! no figures for them were found). Built, it is the same compressor
//! with a little more of it, all within reach of the Dyna's own knobs
//! (`the_ross_is_the_dyna_within_its_own_knobs`): its decoupled base feed
//! takes away the Dyna's feedback from the unbypassed bias line into Q1's
//! base, which is 3 dB more gain at full Sensitivity (a touch more of the
//! knob), and its larger Level pot leaves a decibel more at the top of its
//! travel and moves the output's 50 nF corner from 53 Hz to 29 Hz. The ratio,
//! the attack, the squash and the release are identical. What the Ross's
//! decoupling is really for is the battery's noise, which a noiseless model
//! does not have. It is kept as `ROSS` for that measurement, and the second
//! compressor on the line is the Juicer, which works another way.

use crate::circuits::ts808::D1N4148;
use crate::dsp::netlist::{BipolarSpec, Circuit, Fault, Netlist, OtaSpec, Taper};

/// RV1 SENSITIVITY, 500 k, a rheostat between Q5 and R17. The Drive knob
/// turns it.
pub const SENSITIVITY: usize = 0;
/// RV2 OUTPUT, 50 k, the volume after C9 and R14.
pub const OUTPUT: usize = 1;
/// The 2 k trimmer across the OTA's inputs, which balances their bias. Not on
/// the panel: set to its middle, as ElectroSmash says it practically always
/// is.
pub const TRIM: usize = 2;

/// Where OUTPUT rests: unity through the pedal at a guitar's level with
/// Sensitivity at noon (`calibrate_the_output_rest`).
pub const OUTPUT_REST: f64 = 0.624;
pub const TRIM_REST: f64 = 0.5;

pub const OUTPUT_NODE: &str = "out";
/// Q2's emitter: the compressed signal before the output network.
pub const SPLITTER: &str = "q2e";
/// C8: the envelope, which is what pin 5's current follows.
pub const ENVELOPE: &str = "env";

/// Q1-Q5, 2N3904. From Fairchild's (now ON Semiconductor's) published SPICE
/// model: IS 6.734 fA, BR 0.7371, VAF 74.03 V. The forward beta is neither
/// the model's BF of 416 nor what the model gives at the tenths of a
/// milliamp these stages run (96 at 0.1 mA, 137 at 1 mA, its base-emitter
/// recombination current dominating): it is measured. Mad Bean's Ambulator,
/// a Dyna Comp built on 2N3904s, has Q1's base at 1.79 V and its emitter at
/// 1.27 V on a 9.26 V rail -- 0.82 uA through R2 from the 2.61 V bias line
/// for 127 uA through R3, a beta of 155. 150. DERIVED.
const Q2N3904: BipolarSpec = BipolarSpec {
    saturation: 6.734e-15,
    forward_beta: 150.0,
    reverse_beta: 0.7371,
    early: 74.03,
};

/// The battery behind its leads: a fresh carbon-zinc 9 V cell is some ohms.
/// ESTIMATED; nothing here depends on it closely.
const BATTERY_OHMS: f64 = 10.0;

/// The parts the Dyna Comp and the Ross differ in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Values {
    /// R16, the bias divider's lower leg (R15, 56 k, above it).
    pub bias_lower: f64,
    /// A capacitor from the input jack to ground ahead of C1.
    pub input_shunt: Option<f64>,
    /// Q1's base bias from the bias line: the resistance, and the capacitor
    /// to ground at its middle where it is split in two.
    pub base_feed: (f64, Option<f64>),
    /// Q1's collector resistor, where it has one.
    pub q1_collector: Option<f64>,
    /// The trimmer's feed from the battery, likewise.
    pub trim_feed: (f64, Option<f64>),
    /// The output volume: its value and track.
    pub output_pot: (f64, Taper),
}

/// The MXR Dyna Comp, CA3080 version (ElectroSmash).
pub const DYNA: Values = Values {
    bias_lower: 22_000.0, // R16
    input_shunt: None,
    base_feed: (1_000_000.0, None), // R2
    q1_collector: None,
    trim_feed: (470_000.0, None), // R5
    // RV2: 50 k; the drawing gives no track. An audio one, as a volume's
    // usually is. ESTIMATED.
    output_pot: (50_000.0, Taper::Audio),
};

/// The Ross Compressor (Fuzz Central's drawing of an original).
pub const ROSS: Values = Values {
    bias_lower: 27_000.0,
    input_shunt: Some(220e-12),
    base_feed: (940_000.0, Some(1e-6)),
    q1_collector: Some(10_000.0),
    trim_feed: (440_000.0, Some(1e-6)),
    // "Level 100KB": linear.
    output_pot: (100_000.0, Taper::Linear),
};

pub fn build(source: f64, load: f64) -> Result<Circuit, Fault> {
    assemble(&DYNA, source, load, OUTPUT_NODE)
}

pub fn build_with(values: &Values, source: f64, load: f64) -> Result<Circuit, Fault> {
    assemble(values, source, load, OUTPUT_NODE)
}

/// The same pedal brought out at a chosen node.
pub fn tap(values: &Values, source: f64, load: f64, at: &str) -> Result<Circuit, Fault> {
    assemble(values, source, load, at)
}

/// A resistance from `a` to `b`, split in two with `cap` to ground at its
/// middle where there is one.
fn feed(net: &mut Netlist, a: &str, b: &str, (ohms, cap): (f64, Option<f64>), middle: &str) {
    match cap {
        Some(farads) => {
            net.resistor(a, middle, ohms / 2.0)
                .capacitor(middle, "gnd", farads)
                .resistor(middle, b, ohms / 2.0);
        }
        None => {
            net.resistor(a, b, ohms);
        }
    }
}

fn assemble(v: &Values, source: f64, load: f64, at: &str) -> Result<Circuit, Fault> {
    let mut net = Netlist::new("Red Squeeze");
    net.rest(OUTPUT, OUTPUT_REST).rest(TRIM, TRIM_REST);

    // --- the battery and the bias line -------------------------------------------
    net.supply("v9", BATTERY_OHMS, 9.0)
        .resistor("v9", "vb", 56_000.0) // R15
        .resistor("vb", "gnd", v.bias_lower); // R16

    // --- the input follower --------------------------------------------------------
    net.input("in", source);
    if let Some(farads) = v.input_shunt {
        net.capacitor("in", "gnd", farads);
    }
    net.capacitor("in", "n1", 0.01e-6) // C1
        .resistor("n1", "q1b", 10_000.0); // R1
    feed(&mut net, "vb", "q1b", v.base_feed, "q1f"); // R2
    let collector = match v.q1_collector {
        Some(ohms) => {
            net.resistor("v9", "q1c", ohms);
            "q1c"
        }
        None => "v9",
    };
    net.bipolar(collector, "q1b", "q1e", Q2N3904) // Q1
        .resistor("q1e", "gnd", 10_000.0) // R3
        .capacitor("q1e", "inv", 1e-6) // C2
        .resistor("inv", "gnd", 1_000_000.0); // R4

    // --- the OTA ---------------------------------------------------------------------
    // The trimmer's track is the whole of the path from pin 2 to pin 3; its
    // wiper only shares R5's current between them.
    net.pot("inv", "trw", "noninv", 2_000.0, Taper::Linear, TRIM);
    feed(&mut net, "v9", "trw", v.trim_feed, "trf"); // R5
    net.resistor("noninv", "gnd", 1_000_000.0) // R6
        .resistor("noninv", "c3", 15_000.0) // R7
        .capacitor("noninv", "c3", 0.01e-6) // C4
        .capacitor("c3", "gnd", 1e-6) // C3
        .ota("noninv", "inv", "ota", "abc", "v9", "gnd", OtaSpec::CA3080) // U1
        .resistor("ota", "vb", 150_000.0) // R8
        .capacitor("ota", "vb", 1e-9); // C5

    // --- the phase splitter ------------------------------------------------------------
    net.bipolar("q2c", "ota", SPLITTER, Q2N3904) // Q2
        .resistor("v9", "q2c", 10_000.0) // R9
        .resistor(SPLITTER, "gnd", 10_000.0); // R10

    // --- the envelope --------------------------------------------------------------------
    net.capacitor("q2c", "q3b", 0.01e-6) // C6
        .resistor("q3b", "gnd", 1_000_000.0) // R11
        .diode("gnd", "q3b", D1N4148) // D1, 1N914
        .bipolar(ENVELOPE, "q3b", "gnd", Q2N3904) // Q3
        .capacitor(SPLITTER, "q4b", 0.01e-6) // C7
        .resistor("q4b", "gnd", 1_000_000.0) // R12
        .diode("gnd", "q4b", D1N4148) // D2, 1N914
        .bipolar(ENVELOPE, "q4b", "gnd", Q2N3904) // Q4
        .resistor("v9", ENVELOPE, 150_000.0) // R13
        .capacitor(ENVELOPE, "gnd", 10e-6) // C8
        .bipolar("v9", ENVELOPE, "q5e", Q2N3904); // Q5
                                                  // SENSITIVITY: with the wiper on the `b` end what is in circuit is
                                                  // `R (1 - f)`, which a reverse-log ("C") track makes `R audio(1 - p)`:
                                                  // all of it fully down, a tenth at noon. The track is ESTIMATED: the
                                                  // drawings give 500 k and no law; builders fit linear (TonePad) or
                                                  // reverse-log (Fuzz Central, for its feel), and only the second spreads
                                                  // the bias current's 26 dB evenly round the knob.
    net.pot(
        "q5e",
        "rv1",
        "rv1",
        500_000.0,
        Taper::AntiAudio,
        SENSITIVITY,
    ) // RV1
    .resistor("rv1", "abc", 27_000.0); // R17

    // --- the output ------------------------------------------------------------------------
    let (pot, taper) = v.output_pot;
    net.capacitor(SPLITTER, "c9", 0.05e-6) // C9
        .resistor("c9", "lvl", 10_000.0) // R14
        .pot("lvl", OUTPUT_NODE, "gnd", pot, taper, OUTPUT) // RV2
        .resistor(OUTPUT_NODE, "gnd", load);

    net.build(at)
}

#[cfg(test)]
mod tests;
