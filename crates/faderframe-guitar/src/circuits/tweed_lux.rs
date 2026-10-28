//! The Tweed Lux preamplifier: a mid-50s American 15 W combo with two
//! channels, two volumes, one tone control and no master -- the amplifier a
//! great many records were cut on by turning it all the way up.
//!
//! Engineering reference (developer documentation, not panel text): the
//! Fender Deluxe, model 5E3 (narrow panel, 1955-1960), from Fender's own
//! drawings "Fender 'Deluxe' Schematic, Model 5E3" and "Fender 'Deluxe'
//! Layout, Model 5E3", both F-EE (as hosted by Schematic Heaven,
//! `fenderamps/deluxe_5e3_schem.pdf`, two pages). Every value below is read off
//! that schematic and checked against the layout, which agree part for part;
//! the layout's lug wiring settles which end of each pot is which (below).
//! The drawings carry no voltages (the layout's "NOTICE" has none
//! printed), so the operating points are checked against a kit maker's chart,
//! SECONDARY: Weber's "5E3 Typical Voltages", rev 01/21/10, as reproduced on
//! robrobinette.com ("How the 5E3 Deluxe Works"), whose currents -- V1 1.1 mA
//! a side, V2A 0.83 mA, V2B 0.84 mA, the 6V6s 38 mA -- agree with it. Where
//! the kit differs from Fender's drawing the drawing is built: the kit's B+1
//! to B+2 dropper is 4.7 k, Fender's 5000, and its tone cap .0047, Fender's
//! .005. The power stage, from the inverter on, is
//! `power::PowerSpec::TWEED_6V6`, with the cathodyne in
//! `power::CathodyneFront::TWEED_5E3`.
//!
//! ```text
//! BRIGHT  jacks -- 68 k / 68 k / 1 M -- V1B 12AY7 -- .1 -- [VOLUME 1 M] --+
//! NORMAL  jacks -- 68 k / 68 k / 1 M -- V1A 12AY7 -- .1 -- [VOLUME 1 M] --+-- V2A
//!                  V1's halves share 820 ohm // 25 uF; plates 100 k        |   12AX7
//!          BRIGHT's coupling cap -- 500 pF -- [TONE 1 M] wiper ------------+
//!                                                  `-- .005 -- ground
//! V2A: 100 k plate, 1500 // 25 uF cathode -- .02 -- the cathodyne (power)
//! ```
//!
//! **The volumes are wired backwards, and that is the amplifier.** Each
//! channel's .1 uF lands on its volume pot's *wiper*; lug 1 is grounded and
//! lug 3 is V2A's grid, the two lug 3s tied together (the layout's wiring,
//! which the schematic draws as the same). Turned up, a channel's wiper walks
//! towards V2A; turned down, towards ground, shorting its own signal and
//! putting the whole track in series with the grid. So the other channel's
//! volume is in circuit whatever is plugged in: at ten its wiper puts V2A's
//! grid straight onto that channel's coupling capacitor and V1 plate, a load
//! of a few tens of kilohms; at zero it is a megohm to ground. Turning the
//! unused volume up darkens and tames the one being played; that is not a
//! fault to be modelled away but the reason the 5E3 has two volume knobs worth
//! turning. Both channels are built, and the panel reaches the other volume.
//!
//! **One tone control, between the channels.** The 500 pF mica runs from the
//! bright channel's coupling capacitor (its volume's wiper) to lug 3 of the
//! 1 M tone pot; lug 1 goes to ground through .005 uF; the wiper is V2A's
//! grid. Up, the grid is on the 500 pF -- a bright cap round the bright
//! volume -- and the .005 is a megohm away; down, the .005 sits on the grid.
//! So the bright channel gets both halves of the control and the normal
//! channel only the cut, and the bright volume's setting moves where the
//! tone control's top is for the normal channel.
//!
//! **The jacks.** Fender's pair, as on the AB763s: each jack's 68 k to the
//! grid, the 1 M on jack 1, jack 1's shorting contact to ground, jack 2's
//! contact to jack 1's tip. Built as their contacts (`INPUT_*`), so the
//! panel's three positions are the three ways a tweed is plugged into:
//! jack 2 (half the signal through 136 k), jack 1, and **jumpered** -- the
//! guitar in the played channel's jack 1 and a short cable from its jack 2 to
//! the other channel's jack 1, so both triodes of V1 get the guitar and their
//! two volumes blend at V2A's grid. Both halves of V1 invert, so the two
//! arrive in phase and add: the classic way to play one.
//!
//! **Inter-electrode capacitances.** V2A's grid sits behind hundreds of
//! kilohms of volume and tone network, and its grid-to-plate capacitance,
//! multiplied by the stage's gain, is a pole in the top octave that the triode
//! model (memoryless) would otherwise leave out. The types' tabulated
//! capacitances are stamped across each triode, as `circuits::deluxe` does.
//!
//! **The pots' laws.** The drawings print "1 MEG" three times and no law.
//! All three are built audio (ESTIMATED): an audio track is what a volume
//! needs to sweep evenly by ear, and nothing on the drawings says otherwise.
//! A volume with its signal on the wiper has its law between the wiper and
//! lug 1, ground, as built.
//!
//! **What differs from the AB763 Deluxe** (`circuits::deluxe`), the same
//! name ten years on: a 12AY7, not a 7025, at the front; volumes ahead of
//! the stack, and no stack -- one cut control; V2A straight into a cathodyne
//! rather than a reverb, a mixer and a long-tailed pair; cathode bias and no
//! loop where the AB763 has fixed bias and 820 ohm of feedback; a 5Y3 where
//! it has a GZ34.
//!
//! **What is not built.** The heaters, the ground switch and its .05 uF
//! "death cap", the pilot lamp, the extra speaker jack. The 5Y3 and the
//! supply are in the power stage (`power::T5Y3GT`).

use crate::dsp::netlist::{Adjust, Circuit, Fault, Netlist, Taper, TriodeSpec};

/// The played channel's VOLUME, 1 M. The Drive knob turns it.
pub const VOLUME: usize = 0;
/// TONE, 1 M, with its .005 and the bright channel's 500 pF.
pub const TONE: usize = 1;
/// The other channel's VOLUME, which the panel's fourth knob reaches.
pub const OTHER_VOLUME: usize = 2;

/// Which channel the guitar is plugged into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// V1B, the one with the 500 pF and the whole tone control.
    Bright,
    /// V1A: the tone control is only a cut here.
    Normal,
}

/// The input jacks' contacts, as adjustable resistors (`CONTACT_CLOSED`,
/// `CONTACT_OPEN`): the source to the played channel's jack 1 and jack 2,
/// jack 1's shorting contact, jack 2's contact to jack 1, the jumper cable
/// from jack 2 to the other channel's jack 1, and that jack's own shorting
/// contact.
pub const INPUT_SLOTS: [usize; 6] = [0, 1, 2, 3, 4, 5];
pub const CONTACT_CLOSED: f64 = 1.0;
pub const CONTACT_OPEN: f64 = 1.0e9;
const C: f64 = CONTACT_CLOSED;
const O: f64 = CONTACT_OPEN;
/// The three ways in, in the panel's order: jack 2, jack 1 (built, and where
/// the circuit is calibrated), and jack 1 jumpered to the other channel.
pub const INPUT: [[f64; 6]; 3] = [
    [O, C, C, O, O, C], // jack 2: jack 1 empty and shorted, the 68 k pair a divider
    [C, O, O, C, O, C], // jack 1: jack 2 empty, the pair in parallel
    [C, O, O, O, C, O], // jumpered: both channels' jack 1s carry the guitar
];

/// The supply node the preamplifier hangs from: B+2, the screens', which the
/// 22 k dropper takes down to B+3. Re-measured from the power stage's
/// operating point by `tests/tweed_lux.rs` (the kit chart's 318 V).
pub const B2_NODE: f64 = 318.0;
/// The inverter's idle current on B+3, as a resistance: the chart's 0.84 mA
/// (47 V across its 56 k) at 247 V. DERIVED.
pub const INVERTER_IDLE: f64 = 320_000.0;

const V12AY7: TriodeSpec = TriodeSpec::T12AY7;
const V12AX7: TriodeSpec = TriodeSpec::ECC83;

// The ECC83 figures `circuits::deluxe` stamps (1.6 pF grid to plate, 1.6 pF
// in, 0.33 pF out), and the 12AY7's as the type's tables give them (1.3, 1.3,
// 0.6 pF). SECONDARY: the tabulated figures, not a particular sheet.
const ECC83_CAG: f64 = 1.6e-12;
const ECC83_CGK: f64 = 1.6e-12;
const ECC83_CAK: f64 = 0.33e-12;
const AY7_CAG: f64 = 1.3e-12;
const AY7_CGK: f64 = 1.3e-12;
const AY7_CAK: f64 = 0.6e-12;

/// One channel's front end: its two jacks, its half of V1 and its volume.
struct Side {
    /// Jack 1's tip, jack 2's tip, the grid, the plate, and the volume's
    /// wiper (where the .1 uF lands).
    t1: &'static str,
    t2: &'static str,
    grid: &'static str,
    plate: &'static str,
    wiper: &'static str,
}

const BRIGHT: Side = Side {
    t1: "b_t1",
    t2: "b_t2",
    grid: "v1b_g",
    plate: "v1b_p",
    wiper: "b_w",
};

const NORMAL: Side = Side {
    t1: "n_t1",
    t2: "n_t2",
    grid: "v1a_g",
    plate: "v1a_p",
    wiper: "n_w",
};

/// The grid V2A takes from both volumes and the tone pot.
pub const V2A_GRID: &str = "v2a_g";
/// V2A's plate, where the inverter's .02 is driven from.
pub const V2A_PLATE: &str = "v2a_p";

/// The preamplifier from the played channel's jacks to V2A's plate, handed
/// over through a capacitor large enough not to be one (the .02 is the power
/// stage's, on the grid it charges) into `load`, which stands for the
/// cathodyne's grid: its 1 M leak bootstrapped by the cathode to some
/// megohms.
pub fn build_channel(channel: Channel, source: f64, load: f64) -> Result<Circuit, Fault> {
    tap_channel(channel, source, load, "out")
}

/// The same brought out at a chosen node.
pub fn tap_channel(channel: Channel, source: f64, load: f64, at: &str) -> Result<Circuit, Fault> {
    let (played, other) = match channel {
        Channel::Bright => (BRIGHT, NORMAL),
        Channel::Normal => (NORMAL, BRIGHT),
    };
    let mut net = Netlist::new(match channel {
        Channel::Bright => "Tweed Lux preamp, bright channel",
        Channel::Normal => "Tweed Lux preamp, normal channel",
    });
    let [jack1, jack2, short1, link2, jumper, other_short] = INPUT[1];

    // --- supply: B+2 -- 22 k -- B+3, 16 uF -----------------------------------
    net.supply("b3", 22_000.0, B2_NODE)
        .capacitor("b3", "gnd", 16e-6)
        .resistor("b3", "gnd", INVERTER_IDLE);

    // --- the jacks -------------------------------------------------------------
    net.input("in", source);
    let slots = [
        net.adjustable("in", played.t1, Adjust::Resistor, jack1),
        net.adjustable("in", played.t2, Adjust::Resistor, jack2),
        net.adjustable(played.t1, "gnd", Adjust::Resistor, short1),
        net.adjustable(played.t2, played.t1, Adjust::Resistor, link2),
        net.adjustable(played.t2, other.t1, Adjust::Resistor, jumper),
        net.adjustable(other.t1, "gnd", Adjust::Resistor, other_short),
    ];
    debug_assert_eq!(slots, INPUT_SLOTS);
    // The played channel's pair, each jack's 68 k to the grid and the 1 M on
    // jack 1; the other channel's the same, its jack 2 always empty, so its
    // contact joins its tip to jack 1's and the pair is in parallel.
    net.resistor(played.t1, "gnd", 1_000_000.0)
        .resistor(played.t1, played.grid, 68_000.0)
        .resistor(played.t2, played.grid, 68_000.0)
        .resistor(other.t1, "gnd", 1_000_000.0)
        .resistor(other.t1, other.grid, 34_000.0); // 68 k || 68 k

    // --- V1, a 12AY7: both halves on one 820 ohm // 25 uF ----------------------
    net.resistor("v1_k", "gnd", 820.0)
        .capacitor("v1_k", "gnd", 25e-6);
    for side in [&BRIGHT, &NORMAL] {
        net.resistor("b3", side.plate, 100_000.0)
            .capacitor(side.plate, side.grid, AY7_CAG)
            .capacitor(side.grid, "v1_k", AY7_CGK)
            .capacitor(side.plate, "v1_k", AY7_CAK)
            .triode(side.plate, side.grid, "v1_k", V12AY7)
            .capacitor(side.plate, side.wiper, 0.1e-6); // .1-400
    }

    // --- the volumes, wipers in and lug 3s tied --------------------------------
    // `pot(a, wiper, b)` puts `R f(p)` between wiper and `b`: lug 1 is `b`,
    // ground, so turned up the wiper is at V2A's grid.
    let (played_volume, other_volume) = (VOLUME, OTHER_VOLUME);
    net.pot(
        V2A_GRID,
        played.wiper,
        "gnd",
        1_000_000.0,
        Taper::Audio,
        played_volume,
    )
    .pot(
        V2A_GRID,
        other.wiper,
        "gnd",
        1_000_000.0,
        Taper::Audio,
        other_volume,
    )
    // Where a player leaves a channel nobody is plugged into: half way, its
    // track a megohm-ish load, and where the panel's knob starts.
    .rest(OTHER_VOLUME, 0.5);

    // --- the tone control ---------------------------------------------------------
    // The 500 pF mica from the bright channel's wiper to lug 3, lug 1 to the
    // .005 and ground, the wiper on V2A's grid.
    net.capacitor(BRIGHT.wiper, "tone_top", 500e-12)
        .pot(
            "tone_top",
            V2A_GRID,
            "tone_bot",
            1_000_000.0,
            Taper::Audio,
            TONE,
        )
        .capacitor("tone_bot", "gnd", 0.005e-6);

    // --- V2A, a 12AX7 ---------------------------------------------------------------
    net.resistor("v2a_k", "gnd", 1_500.0)
        .capacitor("v2a_k", "gnd", 25e-6)
        .resistor("b3", V2A_PLATE, 100_000.0)
        .capacitor(V2A_PLATE, V2A_GRID, ECC83_CAG)
        .capacitor(V2A_GRID, "v2a_k", ECC83_CGK)
        .capacitor(V2A_PLATE, "v2a_k", ECC83_CAK)
        .triode(V2A_PLATE, V2A_GRID, "v2a_k", V12AX7);

    // --- the hand-off ----------------------------------------------------------------
    // Not a part of the amplifier: a direct-current block so the module hands
    // over an alternating signal, as every circuit here does. 10 uF into the
    // load is a corner far below anything audible.
    net.capacitor(V2A_PLATE, "out", 10e-6)
        .resistor("out", "gnd", load);

    net.build(at)
}
