//! The 2A Leveler: an optical leveling amplifier in the manner of the
//! Teletronix/UREI LA-2A. Three circuits, solved side by side: the input
//! transformer and the T4's attenuator ([`input`]), the make-up amplifier
//! ([`amplifier`]) and the sidechain that drives the T4 cell's
//! electroluminescent panel ([`sidechain`]). They are cut where nothing
//! flows back: at the Gain and Peak Reduction pots' wipers, which feed
//! valve grids that draw no current (each sits about a volt below its
//! cathode), so a wiper's voltage is its share of the node above it and the
//! pot's load stays in [`input`]. The cell itself (light from the panel's
//! voltage, the photoresistor's conductance from the light) is not a
//! circuit and lives with the device; it closes the loop by setting the
//! photoresistor ([`CELL`]) every sample — the only part that changes every
//! sample, in the smallest circuit.
//!
//! Engineering reference (developer documentation, not panel text): UREI
//! drawing C-10953, issue 3 (6 Aug 1968), revision C (30 Apr 1979, "schematic
//! corrections, no circuit change"), "redrawn from S-1014-1M" — the factory
//! drawing of the late Teletronix and the UREI units, with its test-point
//! voltages; cross-checked against Teletronix S-1014-1F (Dec 1964), -1J (Mar
//! 1966) and -1M (Dec 1966), the 3-66 instruction manual and UA's 2000
//! reissue manual (LA2A-M01). Where the drawings differ the choice is noted.
//!
//! ## The audio path
//!
//! - **T1**, UTC A-10 (UREI rev B, Feb 1969; earlier units the HA-100X):
//!   600 ohm line to a 50 k split grid winding, 1 : 9.13; R5 68 k across the
//!   secondary.
//! - **The attenuator**: R6 68 k from the secondary to node `b`, R7 2.7 k
//!   (UREI: selected 1 k–2.7 k to match the cell; 2.7 k in every Teletronix
//!   drawing and UA's figure) to node `c`, and the T4's audio photoresistor
//!   from `c` to ground — the shunt leg ([`CELL`]). SW3 in Compress shorts
//!   R7 ([`R7_SLOT`]); in Limit it is in.
//! - **The sidechain's tap**: node `b` (SW3's pivot), which feeds R2, the
//!   Peak Reduction pot (100 k, here its load; the pot itself is in
//!   [`sidechain`]). In Compress `b` is `c`: the sidechain hears the reduced
//!   signal (feedback). In Limit it hears the junction above R7, where the
//!   signal cannot fall below R7's share of the secondary: part feed-forward.
//! - **R1, Gain**, 100 k (Allen-Bradley "modified logarithmic"; an audio
//!   taper here) from `c` to ground, its wiper on V1A's grid: the
//!   [`amplifier`] is driven at the grid with the wiper's voltage, through
//!   [`WIPER_OHMS`].
//! - **V1**, 12AX7A: two common-cathode stages, both cathodes unbypassed
//!   (R10 1.5 k, R14 2.7 k), plates on R9/R13 220 k from the 205 V node,
//!   C1 0.02 µF and R12 470 k between them.
//! - **V2**, 12BH7A as a "totem pole or double cathode follower" (the 3-66
//!   manual's words): the upper half's plate on R17 10 k, its cathode
//!   through R18 1 k to the output node, which is the lower half's plate;
//!   the lower half's grid driven from the upper plate through C3 0.1 µF
//!   (R19 470 k leak), its cathode on R20 1 k. R15 470 k returns the upper
//!   grid to the output node (bootstrapped).
//! - **Feedback**, "approximately 20 DB" (3-66 manual): R21 100 k from the
//!   output node (ahead of C5) to node `f`, R22 33 k with the C4 trimmer
//!   (50–380 pF) across it from `f` to ground, and R11 68 k from `f` into
//!   V1A's cathode. (At low resolution the UREI drawing seems to take R22 to
//!   T2's primary; at full resolution, and on every Teletronix drawing, it
//!   goes to ground — and only that gives the specified 40 dB.)
//! - **T2**, UTC A-24 (every version, UA's too): 15 k : 600 ohm, 5 : 1,
//!   parallel fed through C5 10 µF; the line at its secondary.
//!
//! ## The sidechain
//!
//! - **R2, Peak Reduction**, 100 k (same parts as R1), its wiper on V3's
//!   grids: the [`sidechain`] is driven there, as the amplifier is.
//! - **V3**, 12AX7A with both halves in parallel (plates, cathodes and —
//!   through R32 1 k — grids joined): R33 220 k plate load, R31 1 k cathode
//!   bypassed by C8 0.03 µF only (a lift above about 5 kHz).
//! - **R37** (1 M rheostat, "LIM. RESP. ADJ.", [`R37_SLOT`]) with C12
//!   0.001 µF across it, after C9 0.02 µF: at its "flat" end it passes
//!   everything; turned up it takes the lows down ahead of R3 and leaves the
//!   highs (C12): more reduction for high frequencies.
//! - **R3**, 1 M "stereo adjust", clockwise (its wiper at the top: V4's grid
//!   on node `g`), and C6 0.01 µF / R30 47 k, the stereo-parallel terminal's
//!   network, on the same node.
//! - **V4**, 6AQ5A beam power valve (the 6V6's miniature twin): R34 22 k
//!   2 W plate load and R36 1 k with C10 50 µF in the cathode (Teletronix
//!   S-1014-1J/-1M and UA's reissue; the UREI drawing's 10 k / 470 Ω draws
//!   more than its own supply drop allows — see [`SUPPLY_DRAW`]), the screen
//!   on R35 220 k decoupled by R38 22 k into C7-D 30 µF, with C13 510 pF.
//! - **The panel**: C11 0.01 µF from V4's plate to the T4's
//!   electroluminescent panel, a capacitor ([`PANEL_FARADS`]) with R8 2.2 M
//!   across it, inside the T4. Its voltage is the circuit's output.

use crate::dsp::netlist::{Adjust, Circuit, CoreSpec, Fault, Netlist, PentodeSpec, TriodeSpec};

/// The audio photoresistor's slot in [`input`] (ohms, set every sample).
pub const CELL: usize = 0;
/// R7 between the sidechain's tap and the cell, in [`input`] (SW3 shorts it
/// in Compress).
pub const R7_SLOT: usize = 1;
/// R37, the sidechain's high-frequency emphasis (in [`sidechain`]; a trim:
/// the circuit is rebuilt when it moves).
pub const R37_SLOT: usize = 0;

/// R1 and R2: 100 k each.
pub const POT_OHMS: f64 = 100_000.0;
/// What a valve grid sees behind a pot's wiper: ESTIMATED at 10 k, between
/// the track's nothing at its ends and the 25 k (plus the node's own) at the
/// middle. Only grid current, which these grids never draw at the levels in
/// use, would hear the difference.
pub const WIPER_OHMS: f64 = 10_000.0;
/// R7, Limit; and SW3's closed contact (Compress), a contact and its wiring.
pub const R7_OHMS: f64 = 2_700.0;
pub const SWITCH_OHMS: f64 = 1.0;
/// R37 at its "flat" end (a rheostat's end resistance) and at its other.
pub const R37_FLAT: f64 = 50.0;
pub const R37_OHMS: f64 = 1_000_000.0;
/// The photoresistor in the dark (the device sets it from the cell model).
pub const CELL_DARK: f64 = 10_000_000.0;

/// R5, R6: 68 k each in every drawing.
pub const R5: f64 = 68_000.0;
pub const R6: f64 = 68_000.0;

/// UTC A-10: 600 ohm to 50 k, `sqrt(600 / 50,000)` (Sowter's replacement,
/// part 1449, states the same 1 : 9.13).
pub const T1_RATIO: f64 = 0.109_544_51;
/// UTC's 1963 catalogue: primary resistance 59 ohm.
const T1_PRIMARY_R: f64 = 59.0;
/// ESTIMATED: the secondary's copper (Sowter's replacement: 2,360 ohm).
const T1_SECONDARY_R: f64 = 2_400.0;
/// ESTIMATED winding and wiring capacitance at the secondary.
const T1_SHUNT: f64 = 60e-12;
/// ESTIMATED magnetising branch on the 600 ohm primary: UTC rates the A-10
/// ±2 dB 20 Hz–20 kHz and +15 dBm. 5 H puts its low corner near 9 Hz in a
/// 600 ohm system (Sowter's replacement: 6.7 H); the knee sits at the flux a
/// +15 dBm, 30 Hz sine reaches (6.2 V peak / 2π·30 = 0.033 Wb-turns), a
/// nickel core's soft corner.
pub const T1_CORE: CoreSpec = CoreSpec {
    henry: 5.0,
    knee: 0.035,
    sharpness: 3.0,
};

/// UTC A-24: 15 k : 600 ohm, `sqrt(15,000 / 600)` = 5 (Sowter's 1290: "Ratio
/// 5:1. 15k/600").
pub const T2_RATIO: f64 = 5.0;
/// UTC's 1963 catalogue: primary resistance 1,430 ohm.
const T2_PRIMARY_R: f64 = 1_430.0;
/// ESTIMATED: the 600 ohm secondary's copper (the primary's over the
/// impedance ratio, about 57 ohm).
const T2_SECONDARY_R: f64 = 55.0;
/// ESTIMATED magnetising branch, on the secondary (no direct current flows
/// in a parallel-fed winding): 60 H on the primary (Sowter's replacement,
/// 97 H; UTC rates the A-24 ±2 dB 20 Hz–40 kHz) is 2.4 H here; the knee at
/// the flux a +15 dBm (UTC's rating) 30 Hz sine reaches on the secondary,
/// 6.2 V peak / 2π·30 = 0.033 Wb-turns.
pub const T2_CORE: CoreSpec = CoreSpec {
    henry: 2.4,
    knee: 0.036,
    sharpness: 4.0,
};

/// C4, the feedback trimmer (50–380 pF): ESTIMATED, set where the response
/// is flattest to 15 kHz (the spec: ±0.1 dB, 30 Hz–15 kHz).
pub const C4: f64 = 100e-12;

/// The EL panel's capacitance: ESTIMATED from measured T4-size panels —
/// about 2.2 nF at 60 Hz falling to 1.6–1.9 nF at 6–12 kHz (Stroe 2018,
/// Univ. Huddersfield thesis, Fig. 6.22; J. Knif on GroupDIY: 1.6–1.9 nF).
pub const PANEL_FARADS: f64 = 2.0e-9;

/// The 12AX7 with both halves in parallel (V3): twice the current at every
/// voltage, which in Koren's law is half `kg1`; nothing else changes.
pub const ECC83_PAIR: TriodeSpec = TriodeSpec {
    mu: 100.0,
    ex: 1.4,
    kg1: 530.0,
    kp: 600.0,
    kvb: 300.0,
};

/// The 6AQ5A: RCA rates it as the 6V6GT (250 V plate and screen, −12.5 V:
/// 45 mA, 4.5 mA screen, 4,100 µmho), so the 6V6GT's fit stands for it.
pub const T6AQ5A: PentodeSpec = PentodeSpec::T6V6GT;

/// The power supply as each circuit sees it. The rectifiers charge C7A
/// (40 µF, 350 V on the drawing; ESTIMATED 100 ohm of winding and diode
/// behind it), R29 4.7 k 2 W drops to C7B (40 µF, 275 V) and R16 68 k to
/// C7C (30 µF, 205 V) for V1. The two circuits share R29, so each carries
/// the other's (nearly constant, class A) current as a resistor on the
/// 275 V node: the sidechain's V3, V4 and screen and the meter's neon
/// about 8.3 mA, the amplifier's V1 and V2 about 6.3 mA. With R34 22 k
/// the total, about 15 mA, drops the 75 V the drawing shows across R29.
pub const SUPPLY_VOLTS: f64 = 350.0;
const SUPPLY_OHMS: f64 = 100.0;
const R29: f64 = 4_700.0;
/// The other circuit's draw on the 275 V node, as a resistor (275 V / mA).
pub const SUPPLY_DRAW: [f64; 2] = [275.0 / 8.3e-3, 275.0 / 6.3e-3];

fn supply(net: &mut Netlist, other: f64) {
    net.supply("b350", SUPPLY_OHMS, SUPPLY_VOLTS)
        .capacitor("b350", "gnd", 40e-6) // C7A
        .resistor("b350", "b275", R29) // R29
        .capacitor("b275", "gnd", 40e-6) // C7B
        .resistor("b275", "gnd", other); // the other circuit's draw
}

/// The input from a source of `source` ohms (the line feeding it: 600 in
/// the system the unit was built for) through T1 and the attenuator to node
/// `c`, its output (and node `b`, the sidechain's tap). R2 and R1 are here
/// as their loads (100 k each); V1A's grid draws no current at the wiper
/// (it sits about a volt below its cathode), so [`amplifier`] takes `c` as
/// it is solved here. The cell ([`CELL`]) changes every sample; R7
/// ([`R7_SLOT`]) only when SW3 is thrown.
pub fn input(source: f64) -> Result<Circuit, Fault> {
    let mut net = Netlist::new("2A Leveler input");

    // --- T1, the input transformer -------------------------------------------------
    net.input("in", source)
        .resistor("in", "t1p", T1_PRIMARY_R)
        .core("t1p", "gnd", T1_CORE)
        .transformer("t1p", "gnd", "t1s", "gnd", T1_RATIO)
        .resistor("t1s", "sec", T1_SECONDARY_R)
        .capacitor("sec", "gnd", T1_SHUNT)
        .resistor("sec", "gnd", R5); // R5

    // --- the attenuator: R6, R7 (SW3), the cell, R2's and R1's loads ----------------
    net.resistor("sec", "b", R6) // R6
        .resistor("b", "gnd", POT_OHMS) // R2, Peak Reduction
        .resistor("c", "gnd", POT_OHMS); // R1, Gain
    let cell = net.adjustable("c", "gnd", Adjust::RealtimeResistor, CELL_DARK);
    let r7 = net.adjustable("b", "c", Adjust::Resistor, SWITCH_OHMS);
    debug_assert_eq!((cell, r7), (CELL, R7_SLOT));
    net.build("c")
}

/// The make-up amplifier: from R1's wiper (node `c` of the [`input`] as it
/// is solved there, times the Gain's share of the track) through V1, V2 and
/// T2 into a `load` (600 ohm, as the specifications are measured). Its
/// output is the line.
pub fn amplifier(load: f64) -> Result<Circuit, Fault> {
    let mut net = Netlist::new("2A Leveler amplifier");
    supply(&mut net, SUPPLY_DRAW[0]);
    net.resistor("b275", "b205", 68_000.0) // R16
        .capacitor("b205", "gnd", 30e-6); // C7C

    // --- R1's wiper on V1A's grid -------------------------------------------------------
    net.input("g1", WIPER_OHMS);

    // --- V1, 12AX7A -------------------------------------------------------------------
    net.resistor("b205", "p1", 220_000.0) // R9
        .triode("p1", "g1", "k1", TriodeSpec::ECC83)
        .resistor("k1", "gnd", 1_500.0) // R10
        .resistor("f", "k1", 68_000.0) // R11, the feedback into the cathode
        .capacitor("p1", "g2", 0.02e-6) // C1
        .resistor("g2", "gnd", 470_000.0) // R12
        .resistor("b205", "p2", 220_000.0) // R13
        .triode("p2", "g2", "k2", TriodeSpec::ECC83)
        .resistor("k2", "gnd", 2_700.0) // R14
        .capacitor("p2", "g3", 0.1e-6); // C2

    // --- V2, 12BH7A: the totem pole --------------------------------------------------
    net.resistor("b275", "p3", 10_000.0) // R17
        .triode("p3", "g3", "k3", TriodeSpec::T12BH7)
        .resistor("g3", "out", 470_000.0) // R15
        .resistor("k3", "out", 1_000.0) // R18
        .capacitor("p3", "g4", 0.1e-6) // C3
        .resistor("g4", "gnd", 470_000.0) // R19
        .triode("out", "g4", "k4", TriodeSpec::T12BH7)
        .resistor("k4", "gnd", 1_000.0); // R20

    // --- the feedback and T2 -----------------------------------------------------------
    net.capacitor("out", "t2p", 10e-6) // C5
        .resistor("out", "f", 100_000.0) // R21
        .resistor("f", "gnd", 33_000.0) // R22
        .capacitor("f", "gnd", C4) // C4
        .resistor("t2p", "t2w", T2_PRIMARY_R)
        .transformer("t2w", "gnd", "t2s", "gnd", T2_RATIO)
        .core("t2s", "gnd", T2_CORE)
        .resistor("t2s", "line", T2_SECONDARY_R)
        .resistor("line", "gnd", load);

    net.build("line")
}

/// The sidechain, from R2's wiper (node `b` of the [`input`] as it is
/// solved there, times the Peak Reduction's share of the track) to the
/// panel. Its output is the panel's voltage.
pub fn sidechain() -> Result<Circuit, Fault> {
    let mut net = Netlist::new("2A Leveler sidechain");
    supply(&mut net, SUPPLY_DRAW[1]);

    // --- R2's wiper on V3's grids --------------------------------------------------------
    net.input("g5", WIPER_OHMS);

    // --- V3, 12AX7A, both halves ------------------------------------------------------
    net.resistor("b275", "p5", 220_000.0) // R33
        .triode("p5", "g5", "k5", ECC83_PAIR)
        .resistor("k5", "gnd", 1_000.0) // R31
        .capacitor("k5", "gnd", 0.03e-6) // C8 (C8-1 .01 + C8-2 .02)
        .capacitor("p5", "e", 0.02e-6); // C9

    // --- R37 and C12, R3, the stereo-parallel network ---------------------------------
    let r37 = net.adjustable("e", "g6", Adjust::Resistor, R37_FLAT);
    debug_assert_eq!(r37, R37_SLOT);
    net.capacitor("e", "g6", 0.001e-6) // C12
        .resistor("g6", "gnd", 1_000_000.0) // R3, clockwise
        .capacitor("g6", "link", 0.01e-6) // C6
        .resistor("link", "gnd", 47_000.0); // R30

    // --- V4, 6AQ5A, and the panel ------------------------------------------------------
    net.pentode("p6", "g6", "k6", "s6", 1.0, T6AQ5A)
        .resistor("k6", "gnd", 1_000.0) // R36
        .capacitor("k6", "gnd", 50e-6) // C10
        .resistor("b275", "s6", 220_000.0) // R35
        .resistor("s6", "c7d", 22_000.0) // R38
        .capacitor("c7d", "gnd", 30e-6) // C7-D
        .capacitor("s6", "gnd", 510e-12) // C13
        .resistor("b275", "p6", 22_000.0) // R34
        .capacitor("p6", "panel", 0.01e-6) // C11
        .capacitor("panel", "gnd", PANEL_FARADS) // the EL panel
        .resistor("panel", "gnd", 2_200_000.0); // R8

    net.build("panel")
}
