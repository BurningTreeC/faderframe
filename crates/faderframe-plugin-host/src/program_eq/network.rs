//! The passive EQP-1A equaliser network.
//!
//! From PultEQFx by Simon Huber, used in FaderFrame under the MIT licence.
//!
//! The Pultec is a passive LC/RC network followed by a make-up amplifier that
//! undoes its ~22 dB insertion loss. Everything the unit is famous for comes
//! out of the topology: the boost and attenuate controls of each band tap the
//! same divider chain at different points, so using both at once is not two
//! filters added together but one network rearranged. That is where the "low
//! end trick" - a bump at the selected frequency with a dip an octave or two
//! above it - comes from, and why the manual told you not to do it.
//!
//! ```text
//!   IN --[BOOST HI 10k]-- X --[CUT HI 1k]-- Y --[BOOST LO 10k || Cb]-- GND
//!          |                  |                 |
//!          W                  Wc                Y
//!          |                  |
//!          [C1-L1-Rq tank]    [75R + Chc]
//!          |                  |
//!          +------> Q        GND
//!
//!   X --[CUT LO 100k || Cs]-- P --[1k]-- Q --[10k]-- Y      out = V(Q)
//! ```
//!
//! Component values and pot tapers are those of the hardware: the low boost and
//! low attenuate pots are 10k and 100k with an audio taper, the high boost and
//! high attenuate pots 10k and 1k linear, and the bandwidth pot 2.5k linear,
//! exactly as listed in the Pultec service documentation.

use super::nodal::{Capacitor, GND, Inductor, Network, bilinear, prewarped};

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

/// Junction of the high boost and high attenuate pots.
const X: usize = 0;
/// High boost pot wiper, where the resonant tank is tapped off.
const W: usize = 1;
/// Inside the tank, between its capacitor and its inductor.
const N1: usize = 2;
/// Inside the tank, between its inductor and the bandwidth pot.
const N2: usize = 3;
/// Junction of the low attenuate network and the output divider.
const P: usize = 4;
/// The output node.
const Q: usize = 5;
/// Junction of the high attenuate pot and the low boost network.
const Y: usize = 6;
/// High attenuate pot wiper.
const WC: usize = 7;
/// Between the high attenuate build-out resistor and its capacitor.
const N3: usize = 8;

const NODES: usize = 9;

// ---------------------------------------------------------------------------
// Component values
// ---------------------------------------------------------------------------

/// High boost pot, 10k linear.
const R_BOOST_HI: f64 = 10e3;
/// Bandwidth pot, 2.5k linear.
const R_BANDWIDTH: f64 = 2.5e3;
/// Winding resistance of the tank inductor plus wiring.
const R_TANK: f64 = 120.0;
/// High attenuate pot, 1k linear.
const R_CUT_HI: f64 = 1e3;
/// Build-out resistor ahead of the high attenuate capacitor.
const R_BUILD_OUT: f64 = 75.0;
/// Low attenuate pot, 100k audio.
const R_CUT_LO: f64 = 100e3;
/// Low boost pot, 10k audio.
const R_BOOST_LO: f64 = 10e3;
/// The output divider.
const R_TAP: f64 = 1e3;
const R_SHUNT: f64 = 10e3;

/// The high boost pot only sweeps part of its track, which is what puts the
/// maximum boost at the specified +18 dB rather than the +22 dB it would reach
/// with the wiper hard against the input end.
const BOOST_HI_SPAN: f64 = 0.9079;

/// Insertion loss of the passive network, which the amplifier makes up.
const MAKEUP: f64 = 13.0950;

/// Shapes an audio taper pot: 15 % of the track at half rotation.
const AUDIO_TAPER: f64 = 3.4679;

/// Selectable low frequencies, in Hz.
pub const LOW_FREQS: [f32; 4] = [20.0, 30.0, 60.0, 100.0];
/// Selectable high boost frequencies, in Hz.
pub const HIGH_BOOST_FREQS: [f32; 7] = [3e3, 4e3, 5e3, 8e3, 10e3, 12e3, 16e3];
/// Selectable high attenuation frequencies, in Hz.
pub const HIGH_ATTEN_FREQS: [f32; 3] = [5e3, 10e3, 20e3];

/// Knob positions, each `0.0..=1.0`. The pot tapers are applied inside the
/// circuit, so these are rotations rather than resistances.
#[derive(Clone, Copy, PartialEq)]
pub struct Controls {
    pub low_boost: f64,
    pub low_atten: f64,
    pub high_boost: f64,
    pub high_atten: f64,
    /// `0.0` = sharp, `1.0` = broad.
    pub bandwidth: f64,
    pub low_freq: f64,
    pub high_boost_freq: f64,
    pub high_atten_freq: f64,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            low_boost: 0.0,
            low_atten: 0.0,
            high_boost: 0.0,
            high_atten: 0.0,
            bandwidth: 0.5,
            low_freq: 100.0,
            high_boost_freq: 10e3,
            high_atten_freq: 10e3,
        }
    }
}

/// Control positions that cannot occur, used to force a full rebuild.
const SENTINEL: Controls = Controls {
    low_boost: -1.0,
    low_atten: -1.0,
    high_boost: -1.0,
    high_atten: -1.0,
    bandwidth: -1.0,
    low_freq: -1.0,
    high_boost_freq: -1.0,
    high_atten_freq: -1.0,
};

pub struct PassiveNetwork {
    net: Network<NODES>,
    /// Tank capacitor and inductor, which the high boost selector swaps.
    tank_c: Capacitor,
    tank_l: Inductor,
    /// High attenuate shelf capacitor.
    hi_cut_c: Capacitor,
    /// The two low frequency capacitors, across the attenuate and boost pots.
    lo_cut_c: Capacitor,
    lo_boost_c: Capacitor,
    /// Conductance the input drives the high boost pot through.
    g_input: f64,
    sample_rate: f64,
    controls: Controls,
}

impl PassiveNetwork {
    pub fn new(sample_rate: f64) -> Self {
        let mut eq = Self {
            net: Network::default(),
            tank_c: Capacitor::new(W, N1),
            tank_l: Inductor::new(N1, N2),
            hi_cut_c: Capacitor::new(N3, GND),
            lo_cut_c: Capacitor::new(X, P),
            lo_boost_c: Capacitor::new(Y, GND),
            g_input: 0.0,
            sample_rate,
            // A sentinel no real setting can match, so the first
            // `set_controls` builds every part of the network.
            controls: SENTINEL,
        };
        eq.set_controls(Controls::default());
        eq
    }

    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        let controls = self.controls;
        self.controls = SENTINEL;
        self.set_controls(controls);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.net.reset();
        self.tank_c.reset();
        self.tank_l.reset();
        self.hi_cut_c.reset();
        self.lo_cut_c.reset();
        self.lo_boost_c.reset();
    }

    /// Rebuild and refactorise the network for a new set of knob positions.
    /// Called at control rate, not per sample.
    pub fn set_controls(&mut self, c: Controls) {
        if c == self.controls {
            return;
        }

        let fs = self.sample_rate;

        // Reactances first, since their companion conductances go into the
        // matrix alongside the resistors.
        if c.high_boost_freq != self.controls.high_boost_freq {
            let (cap, ind) = tank_components(c.high_boost_freq);
            let warp = prewarped(c.high_boost_freq, fs);
            self.tank_c.set(cap, warp);
            self.tank_l.set(ind, warp);
        }
        if c.high_atten_freq != self.controls.high_atten_freq {
            self.hi_cut_c.set(
                hi_cut_cap(c.high_atten_freq),
                prewarped(c.high_atten_freq, fs),
            );
        }
        if c.low_freq != self.controls.low_freq {
            let (small, large) = low_freq_caps(c.low_freq);
            // Far enough below Nyquist that plain bilinear is exact enough.
            let warp = bilinear(fs);
            self.lo_cut_c.set(small, warp);
            self.lo_boost_c.set(large, warp);
        }

        // Pot rotations to wiper positions and resistances.
        let hi_boost = c.high_boost.clamp(0.0, 1.0) * BOOST_HI_SPAN;
        let hi_atten = c.high_atten.clamp(0.0, 1.0);
        let bandwidth = c.bandwidth.clamp(0.0, 1.0);
        let lo_boost = audio_taper(c.low_boost) * R_BOOST_LO;
        let lo_atten = audio_taper(c.low_atten) * R_CUT_LO;

        let net = &mut self.net;
        net.clear();

        // High boost: a divider in the signal path with the resonant tank
        // bridging from its wiper to the output. Turning it up walks the wiper
        // towards the input, which feeds more signal straight through the tank.
        let r_input = ((1.0 - hi_boost) * R_BOOST_HI).max(1e-3);
        self.g_input = 1.0 / r_input;
        net.conductance(W, GND, self.g_input); // the input end is a driven node
        net.resistor(W, X, hi_boost * R_BOOST_HI);
        net.conductance(W, N1, self.tank_c.conductance());
        net.conductance(N1, N2, self.tank_l.conductance());
        net.resistor(N2, Q, bandwidth * R_BANDWIDTH + R_TANK);

        // High attenuate: a divider whose wiper shunts the highs to ground
        // through the selected capacitor.
        net.resistor(X, WC, (1.0 - hi_atten) * R_CUT_HI);
        net.resistor(WC, Y, hi_atten * R_CUT_HI);
        net.resistor(WC, N3, R_BUILD_OUT);
        net.conductance(N3, GND, self.hi_cut_c.conductance());

        // Low attenuate: series resistance into the output divider, bypassed
        // at higher frequencies by the small capacitor.
        net.resistor(X, P, lo_atten);
        net.conductance(X, P, self.lo_cut_c.conductance());
        net.resistor(P, Q, R_TAP);
        net.resistor(Q, Y, R_SHUNT);

        // Low boost: resistance in the network's path to ground, bypassed at
        // higher frequencies by the large capacitor.
        net.resistor(Y, GND, lo_boost);
        net.conductance(Y, GND, self.lo_boost_c.conductance());

        net.factorize();
        self.controls = c;
    }

    #[inline]
    pub fn process(&mut self, sample: f64) -> f64 {
        let net = &mut self.net;
        net.begin();
        net.drive(W, self.g_input, sample);
        self.tank_c.stamp(net);
        self.tank_l.stamp(net);
        self.hi_cut_c.stamp(net);
        self.lo_cut_c.stamp(net);
        self.lo_boost_c.stamp(net);
        net.solve();

        self.tank_c.update(net);
        self.tank_l.update(net);
        self.hi_cut_c.update(net);
        self.lo_cut_c.update(net);
        self.lo_boost_c.update(net);

        net.voltage(Q) * MAKEUP
    }
}

/// An audio taper pot reaches 15 % of its track at half rotation.
fn audio_taper(rotation: f64) -> f64 {
    let x = rotation.clamp(0.0, 1.0);
    ((AUDIO_TAPER * x).exp() - 1.0) / (AUDIO_TAPER.exp() - 1.0)
}

/// Characteristic impedance of the high boost tank at each selectable
/// frequency, taken from the inductor and capacitor pairs the unit switches
/// between. It sets how much the bandwidth control can damp the resonance.
fn tank_impedance(freq: f64) -> f64 {
    const TABLE: [(f64, f64); 7] = [
        (3e3, 3400.0),
        (4e3, 2600.0),
        (5e3, 3000.0),
        (8e3, 2550.0),
        (10e3, 2270.0),
        (12e3, 1840.0),
        (16e3, 2400.0),
    ];
    TABLE
        .iter()
        .min_by(|a, b| {
            (a.0 - freq)
                .abs()
                .partial_cmp(&(b.0 - freq).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|&(_, z)| z)
        .unwrap_or(2500.0)
}

/// Capacitance and inductance of the tank for a given centre frequency.
fn tank_components(freq: f64) -> (f64, f64) {
    let z0 = tank_impedance(freq);
    let w = std::f64::consts::TAU * freq;
    (1.0 / (w * z0), z0 / w)
}

/// The low frequency selector swaps a pair of capacitors. The small one sets
/// where the attenuate shelf turns over, the large one the boost shelf, and
/// the gap between the two is what opens up the low end trick.
fn low_freq_caps(freq: f64) -> (f64, f64) {
    if freq <= 20.0 {
        (100e-9, 2.2e-6)
    } else if freq <= 30.0 {
        (47e-9, 1.0e-6)
    } else if freq <= 60.0 {
        (22e-9, 470e-9)
    } else {
        (15e-9, 330e-9)
    }
}

fn hi_cut_cap(freq: f64) -> f64 {
    if freq <= 5e3 {
        270e-9
    } else if freq <= 10e3 {
        135e-9
    } else {
        68e-9
    }
}
