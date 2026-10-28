//! The Seattle 100 preamplifier: a late-80s American 100 W two-channel head,
//! a cascade of cold and hot triodes that set the pattern for every
//! high-gain amplifier after it.
//!
//! Engineering reference (developer documentation, not panel text): the
//! Soldano SLO-100 (Super Lead Overdrive), from the drawings "Soldano Super
//! Lead Overdrive 100W", B size, rev A, 15 March 1996, sheets 1 (preamplifier
//! and supply) and 2 (tone stack, masters, power amplifier, footswitch), in
//! the set circulated as `Soldano_slo100.pdf` (el34world.com's Soldano
//! index), which carries the drawing twice -- once plain, once with reference
//! designators -- and agrees with itself value for value. Soldano has not
//! issued a service schematic; this CAD set is the reference every repair
//! and clone works from, and it is what is cited here. Pot laws, the output
//! transformer's impedance and the depth control are SECONDARY, from Rob
//! Robinette's annotated SLO-100 drawing (v1.6h, robrobinette.com, "How the
//! Soldano SLO-100 Works"), which is the same circuit redrawn and agrees with
//! every value below. The power stage is `power::PowerSpec::SEATTLE_6L6`.
//!
//! ```text
//! J1 -- R2 68k -- V1b (R4 220k, R3 1k8 // C1 1u) -- C2 .02 -- X
//! X -- R5 470k // C3 .002 -- P1 OD VOLUME 500k (C100 .001 top to wiper)
//!   -- R8 470k -- V2a (R11 100k, R9 1k8 // C4 1u) -- C6 .02 -- R12 470k
//!   -- V2b grid (R13 1M; VR1) -- V2b, the cold clipper (R19 100k // C9 .001,
//!   R14 39k unbypassed) -- C10 .02 -- R20 2M2 -- VR2 -- M
//! X -- S1 (R6 470k in, or shorted) -- R10 470k (C5 470 pF on S2) -- Z
//!   (R7 39k on S1) -- P2 NORMAL VOLUME 500k -- V1a (R15 100k, R16 2k2)
//!   -- C7 .02 -- R17 2M2 // C8 120 pF -- M (R18 330k)
//! M -- R21 220k -- V3b (R22 220k, R23 1k8 // C11 1u) -- V3a follower
//!   -- R25 100k -- R24 2k2 ("RL") -- C12 1u -- FX SEND / RETURN (R26 1M)
//!   -- V4b (R28 220k, R27 2k2 // C13) -- V4a follower (R29 100k)
//!   -- C14 470p / R30 47k / C15 .02 / C16 .02: TREBLE P3 250k, BASS P4 1M,
//!   MID P5 25k -- P6 OD MV 1M / P7 NORM MV 1M -- VR3 / VR4 -- C17 .02 (power)
//! ```
//!
//! **Two channels, one preamplifier.** V1b feeds both. The overdrive channel
//! is V2a into V2b, the **cold clipper**: 39 k of unbypassed cathode resistor
//! idles it near cut-off, so it clips the bottom of the wave by cutting off
//! and the top by grid current through R12's 470 k, and it has hardly any
//! gain to give -- it is a shaper, not an amplifier. The normal channel is
//! V1a on its own. They meet at M and share V3b, the stage that does most of
//! the distorting on the overdrive channel, and everything after it. The
//! channels are switched by four Vactec VTL5C1 photo-resistors (VR1-VR4):
//! overdrive mode lights VR2 (V2b into M) and VR3 (the overdrive master);
//! normal mode lights VR1 (V2b's grid to ground) and VR4 (the normal master).
//! The normal channel is never switched out: in overdrive mode it still
//! reaches M through R17's 2.2 M, out of phase -- a known part of this
//! amplifier's voice, and built.
//!
//! **Crunch and Bright.** S1 (DPDT, "Crunch/(Clean)") sets the divider in
//! front of the normal volume: Clean puts R6 470 k in series and R7 39 k to
//! ground behind R10, which hands V1a about 4 % of the signal; Crunch shorts
//! R6 and lifts R7, about half. S2 ("Bright/(Norm)") puts C5 470 pF across
//! R10. Both are built as their contacts (`CRUNCH_SLOTS`, `BRIGHT_SLOT`).
//!
//! **The effects loop is in the signal path** and is built: V3a's follower
//! into R25 and the send divider, the return jack's contact normalling send
//! to return, V4b recovering, V4a following into the stack. The drawing's own
//! note -- "Place 1K parallel to RL to reduce FX send lvl to 0dBm. Add 25uF to
//! Rk of Return amp marked *" -- is the production change (Rob Robinette:
//! "Bypass Added By Production Change"), and is built: RL 2.2 k // 1 k, C13
//! 25 uF.
//!
//! **What is not built.** The footswitch's LED supply (the photo-resistors
//! are their lit and dark resistances), the line out, the bias supply (a
//! voltage in the power stage).
//!
//! **Inter-electrode capacitances.** R8, R12 and R21 are hundreds of
//! kilohms in front of high-gain triodes, and their grid-to-plate capacitance,
//! multiplied by the stage's gain, is a low-pass in the top octaves -- the
//! drawing's own way of taming the cascade, by R8 and R12 as much as by any
//! capacitor. The ECC83 figures `circuits::deluxe` stamps are stamped across
//! the five gain stages.

use crate::dsp::netlist::{Adjust, Circuit, Fault, Netlist, Taper, TriodeSpec};

/// P1, OD VOLUME ("OD Gain"), 500 k linear (SECONDARY law).
pub const OD_VOLUME: usize = 0;
/// P2, NORMAL VOLUME ("Normal Gain"), 500 k linear (SECONDARY law).
pub const NORMAL_VOLUME: usize = 1;
/// P3 TREBLE 250 k linear, P4 BASS 1 M audio, P5 MID 25 k linear.
pub const TREBLE: usize = 2;
pub const BASS: usize = 3;
pub const MIDDLE: usize = 4;
/// P6 OD MV and P7 NORM MV, 1 M audio each.
pub const OD_MASTER: usize = 5;
pub const NORMAL_MASTER: usize = 6;

/// Which channel the footswitch has selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// VR2 and VR3 lit.
    Overdrive,
    /// VR1 and VR4 lit.
    Normal,
}

/// S1, Crunch/(Clean), as its two contacts that matter: one across R6, one
/// from the divider's foot to R7. Built in Crunch.
pub const CRUNCH_SLOTS: [usize; 2] = [0, 1];
pub const SWITCH_CLOSED: f64 = 1.0;
pub const SWITCH_OPEN: f64 = 1.0e9;
const C: f64 = SWITCH_CLOSED;
const O: f64 = SWITCH_OPEN;
/// Clean, Crunch.
pub const CRUNCH: [[f64; 2]; 2] = [[O, C], [C, O]];
/// S2, Bright/(Norm): C5, 470 pF across R10, in or out. Built in.
pub const BRIGHT_SLOT: usize = 2;
pub const BRIGHT_FARADS: f64 = 470e-12;
pub const BRIGHT_OFF_FARADS: f64 = 1.0e-18;

/// A lit VTL5C1: Vactec's sheet gives 600 ohm at 10 mA and 200 ohm at 40 mA;
/// the footswitch supply's LED current is not printed. ESTIMATED between them.
/// Every photo-resistor here is in series with, or across, hundreds of
/// kilohms, so the choice moves nothing audible.
pub const LDR_LIT: f64 = 400.0;
/// A dark one: the sheet's minimum dark resistance, 50 Mohm at ten seconds.
pub const LDR_DARK: f64 = 50.0e6;

/// The screens' node the preamplifier hangs from (sheet 1: "495V Screens"),
/// through R60 10 k 2 W to B2.
pub const SCREEN_NODE: f64 = 495.0;
/// The inverter's idle current on B2, as a resistance: its tail from the
/// leak junction (about 32 V) through R32 10 k into R44 4.7 k and R35's 39 k
/// to the 4 ohm tap, about 2.3 mA at 378 V. DERIVED; re-measured against the
/// power stage by `tests/seattle_100.rs`.
pub const INVERTER_IDLE: f64 = 134_800.0;

const ECC83: TriodeSpec = TriodeSpec::ECC83;
const CAG: f64 = 1.6e-12;
const CGK: f64 = 1.6e-12;
const CAK: f64 = 0.33e-12;

/// The node the masters hand to the power stage's C17.
pub const TO_INVERTER: &str = "q";

/// A gain stage: the triode with its inter-electrode capacitances.
fn stage(net: &mut Netlist, plate: &str, grid: &str, cathode: &str) {
    net.capacitor(plate, grid, CAG)
        .capacitor(grid, cathode, CGK)
        .capacitor(plate, cathode, CAK)
        .triode(plate, grid, cathode, ECC83);
}

/// The Marshall-shaped stack the SLO has (the JCM800's but for R30's 47 k):
/// from `drive` to the treble wiper `out`, which is loaded by `load`. Built
/// alone for its frequency response; the preamplifier builds the same parts.
pub fn tone_stack(source: f64, load: f64) -> Result<Circuit, Fault> {
    let mut net = Netlist::new("Seattle 100 tone stack");
    net.input("drive", source);
    stack(&mut net, "drive", "out");
    net.resistor("out", "gnd", load);
    net.build("out")
}

fn stack(net: &mut Netlist, drive: &str, out: &str) {
    // `pot(a, wiper, b)` puts `R f(p)` between wiper and `b`. Bass is a
    // variable resistor, turned up putting resistance in: the reverse law.
    net.capacitor(drive, "t_top", 470e-12) // C14
        .resistor(drive, "slope", 47_000.0) // R30
        .pot("t_top", out, "t_bot", 250_000.0, Taper::Linear, TREBLE) // P3
        .capacitor("slope", "t_bot", 0.02e-6) // C15
        .pot(
            "t_bot",
            "b_bot",
            "b_bot",
            1_000_000.0,
            Taper::ReverseAudio,
            BASS,
        ) // P4
        .pot("b_bot", "m_w", "gnd", 25_000.0, Taper::Linear, MIDDLE) // P5
        .capacitor("slope", "m_w", 0.02e-6); // C16
}

/// The preamplifier from J1 to the masters' photo-resistors, handed to the
/// power stage's C17 at `TO_INVERTER` and loaded by `load`, the inverter's
/// R31 bootstrapped a little by its tail.
pub fn build_channel(channel: Channel, source: f64, load: f64) -> Result<Circuit, Fault> {
    tap_channel(channel, source, load, TO_INVERTER)
}

/// The same brought out at a chosen node.
pub fn tap_channel(channel: Channel, source: f64, load: f64, at: &str) -> Result<Circuit, Fault> {
    let mut net = Netlist::new(match channel {
        Channel::Overdrive => "Seattle 100 preamp, overdrive channel",
        Channel::Normal => "Seattle 100 preamp, normal channel",
    });
    let (od, normal) = match channel {
        Channel::Overdrive => (LDR_LIT, LDR_DARK),
        Channel::Normal => (LDR_DARK, LDR_LIT),
    };

    // --- supplies: the screens -- R60 10k -- B2 (C36 40u) ----------------------
    // B2 -- R61 15k -- B4 (C37 10u); B2 -- R62 15k -- B3 (C38 10u).
    net.supply("b2", 10_000.0, SCREEN_NODE)
        .capacitor("b2", "gnd", 40e-6)
        .resistor("b2", "gnd", INVERTER_IDLE)
        .resistor("b2", "b4", 15_000.0)
        .capacitor("b4", "gnd", 10e-6)
        .resistor("b2", "b3", 15_000.0)
        .capacitor("b3", "gnd", 10e-6);

    // --- J1 and V1b ---------------------------------------------------------------
    net.input("in", source)
        .resistor("in", "gnd", 1_000_000.0) // R1
        .resistor("in", "v1b_g", 68_000.0) // R2
        .resistor("b4", "v1b_p", 220_000.0) // R4
        .resistor("v1b_k", "gnd", 1_800.0) // R3
        .capacitor("v1b_k", "gnd", 1e-6) // C1
        .capacitor("v1b_p", "x", 0.02e-6); // C2
    stage(&mut net, "v1b_p", "v1b_g", "v1b_k");

    // --- the overdrive channel: V2a and the cold clipper -------------------------
    net.resistor("x", "p1_top", 470_000.0) // R5
        .capacitor("x", "p1_top", 0.002e-6) // C3
        .pot("p1_top", "p1_w", "gnd", 500_000.0, Taper::Linear, OD_VOLUME) // P1
        .capacitor("p1_top", "p1_w", 0.001e-6) // C100
        .resistor("p1_w", "v2a_g", 470_000.0) // R8
        .resistor("b3", "v2a_p", 100_000.0) // R11
        .resistor("v2a_k", "gnd", 1_800.0) // R9
        .capacitor("v2a_k", "gnd", 1e-6) // C4
        .capacitor("v2a_p", "n12", 0.02e-6) // C6
        .resistor("n12", "v2b_g", 470_000.0) // R12
        .resistor("v2b_g", "gnd", 1_000_000.0) // R13
        .resistor("v2b_g", "gnd", normal) // VR1, lit in normal mode
        .resistor("b3", "v2b_p", 100_000.0) // R19
        .capacitor("b3", "v2b_p", 0.001e-6) // C9
        .resistor("v2b_k", "gnd", 39_000.0) // R14, unbypassed
        .capacitor("v2b_p", "n20", 0.02e-6) // C10
        .resistor("n20", "gnd", 2_200_000.0) // R20
        .resistor("n20", "m", od); // VR2, lit in overdrive mode
    stage(&mut net, "v2a_p", "v2a_g", "v2a_k");
    stage(&mut net, "v2b_p", "v2b_g", "v2b_k");

    // --- the normal channel: S1, S2, P2 and V1a -----------------------------------
    // S1's left pole: X to L through R6, or straight; its right pole: Z to R7.
    let crunch = CRUNCH[1];
    net.resistor("x", "l", 470_000.0); // R6
    let short = net.adjustable("x", "l", Adjust::Resistor, crunch[0]);
    net.resistor("l", "z", 470_000.0); // R10
    let foot = net.adjustable("z", "r7", Adjust::Resistor, crunch[1]);
    let bright = net.adjustable("l", "z", Adjust::Capacitor, BRIGHT_FARADS); // C5 on S2
    debug_assert_eq!([short, foot], CRUNCH_SLOTS);
    debug_assert_eq!(bright, BRIGHT_SLOT);
    net.resistor("r7", "gnd", 39_000.0) // R7
        .pot("z", "v1a_g", "gnd", 500_000.0, Taper::Linear, NORMAL_VOLUME) // P2
        .resistor("b4", "v1a_p", 100_000.0) // R15
        .resistor("v1a_k", "gnd", 2_200.0) // R16, unbypassed
        .capacitor("v1a_p", "n17", 0.02e-6) // C7
        .resistor("n17", "m", 2_200_000.0) // R17
        .capacitor("n17", "m", 120e-12) // C8
        .resistor("m", "gnd", 330_000.0); // R18
    stage(&mut net, "v1a_p", "v1a_g", "v1a_k");

    // --- V3b, and V3a following it into the send ----------------------------------
    net.resistor("m", "v3b_g", 220_000.0) // R21
        .resistor("b2", "v3b_p", 220_000.0) // R22
        .resistor("v3b_k", "gnd", 1_800.0) // R23
        .capacitor("v3b_k", "gnd", 1e-6) // C11
        .triode("b2", "v3b_p", "v3a_k", ECC83) // V3a, direct coupled
        .resistor("v3a_k", "fx", 100_000.0) // R25
        // R24 "RL" 2.2 k with the note's 1 k across it.
        .resistor("fx", "gnd", 2_200.0 * 1_000.0 / 3_200.0)
        .capacitor("fx", "send", 1e-6) // C12
        // The return jack's contact, normalling the send to the return.
        .resistor("send", "ret", SWITCH_CLOSED)
        .resistor("ret", "gnd", 1_000_000.0); // R26
    stage(&mut net, "v3b_p", "v3b_g", "v3b_k");

    // --- V4b recovering, V4a following into the stack -----------------------------
    net.resistor("b2", "v4b_p", 220_000.0) // R28
        .resistor("v4b_k", "gnd", 2_200.0) // R27
        .capacitor("v4b_k", "gnd", 25e-6) // C13, the note's 25 uF
        .triode("v4b_p", "ret", "v4b_k", ECC83) // V4b
        .triode("b2", "v4b_p", "cf", ECC83) // V4a
        .resistor("cf", "gnd", 100_000.0); // R29

    // --- the stack, the masters, the photo-resistors ------------------------------
    stack(&mut net, "cf", "t_out");
    net.pot("t_out", "od_w", "gnd", 1_000_000.0, Taper::Audio, OD_MASTER) // P6
        .pot(
            "t_out",
            "n_w",
            "gnd",
            1_000_000.0,
            Taper::Audio,
            NORMAL_MASTER,
        ) // P7
        .resistor("od_w", TO_INVERTER, od) // VR3
        .resistor("n_w", TO_INVERTER, normal) // VR4
        .resistor(TO_INVERTER, "gnd", load)
        // Where nobody's knob is: the volume of the channel not played (its
        // own is the Drive knob), and both masters where the calibration was
        // taken -- the played one's is the Master knob's middle.
        .rest(OD_VOLUME, 0.5)
        .rest(NORMAL_VOLUME, 0.5)
        .rest(OD_MASTER, MASTER_REST)
        .rest(NORMAL_MASTER, MASTER_REST);

    net.build(at)
}

/// Where the masters rest: the Master knob's middle. 3 on the dial (6 % of
/// the track on its audio law), where a hundred-watt SLO is played in a room:
/// with a guitar's chord at the calibration level the overdrive channel
/// makes about 17 W, the normal channel about 9 W -- the power stage working,
/// not yet the sound (`tests/seattle_100.rs`, `print_the_power_drive`).
pub const MASTER_REST: f64 = 0.3;
