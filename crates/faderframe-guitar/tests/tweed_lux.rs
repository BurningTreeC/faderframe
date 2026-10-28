//! The Tweed Lux (Fender Deluxe 5E3) against its drawing and the kit chart
//! (`circuits::tweed_lux`): the operating points, the volumes' interaction,
//! the tone control, the jumper, the cathodyne and the 5Y3's sag.

use faderframe_guitar::circuits::{power, tweed_lux};
use faderframe_guitar::dsp::device::Pentode;
use faderframe_guitar::dsp::measure::{self, Tone};
use faderframe_guitar::dsp::netlist::{Circuit, PentodeSpec, RectifierSpec, GROUND};
use faderframe_guitar::dsp::time::Simulation;
use tweed_lux::{Channel, OTHER_VOLUME, TONE, VOLUME};

fn near(got: f64, chart: f64, within: f64) -> bool {
    (got / chart - 1.0).abs() <= within
}

fn settled(c: Circuit) -> Simulation {
    let mut s = Simulation::new(c, 48_000.0);
    s.find_operating_point();
    s
}

fn at(c: &Circuit, n: &str) -> usize {
    c.unknown_named(n).unwrap_or_else(|| panic!("no node {n}"))
}

/// The power stage at idle against the kit chart (SECONDARY): B+1 370 V,
/// B+2 318 V, the cathodes 20 V and the 6V6s' 38 mA, the inverter's plate
/// 200 V and cathode 45 V. B+3 is the chart's 247 V less what its own
/// currents (3.9 mA) drop across the drawing's 22 k from its own 318 V --
/// 233 V; the chart disagrees with itself by 14 V there, and the model sits
/// on its currents. The plates are at B+1: the model's ideal transformer
/// does not drop the primary's copper (the chart's 8 V).
#[test]
fn the_power_stage_sits_on_the_chart() {
    let spec = &power::PowerSpec::TWEED_6V6;
    let c = power::build(spec, 10_000.0).unwrap();
    let nodes = ["ht", "scr", "pi_c", "ok", "pi_p1", "pi_p2"];
    let idx: Vec<usize> = nodes.iter().map(|n| at(&c, n)).collect();
    let s = settled(c);
    let v: Vec<f64> = idx.iter().map(|&i| s.voltage_at(i)).collect();
    for (n, x) in nodes.iter().zip(&v) {
        println!("{n:<6} {x:8.2} V");
    }
    let (ht, scr, b3, k, pi_p, pi_k) = (v[0], v[1], v[2], v[3], v[4], v[5]);
    let valve = Pentode::new(0, 1, GROUND, 2, 1.0, PentodeSpec::T6V6GT);
    let (ia, is) = valve.currents(ht - k, -k, scr - k);
    println!(
        "a 6V6GT idles at {:.1} mA, {:.1} mA screen, {:.1} W",
        ia * 1e3,
        is * 1e3,
        ia * (ht - k)
    );
    assert!(near(ht, 370.0, 0.02), "B+1 {ht}");
    assert!(near(scr, 318.0, 0.02), "B+2 {scr}");
    assert!(near(b3, 247.0, 0.06), "B+3 {b3}");
    assert!(near(k, 20.0, 0.05), "cathodes {k}");
    assert!(near(ia * 1e3, 38.0, 0.06), "6V6 {ia}");
    assert!(near(pi_p, 200.0, 0.05), "inverter plate {pi_p}");
    assert!(near(pi_k, 45.0, 0.10), "inverter cathode {pi_k}");
    // The preamplifier hangs from the same B+2 it was told.
    assert!(near(scr, tweed_lux::B2_NODE, 0.005), "{scr}");
}

/// The preamplifier at idle: V1's shared cathode 1.9 V, V2A's plate 164 V
/// and cathode 1.2 V, V1's plates 135-138 V less B+3's shortfall (above).
#[test]
fn the_preamp_sits_on_the_chart() {
    let c = tweed_lux::build_channel(Channel::Bright, 10_000.0, 10e6).unwrap();
    let nodes = [
        ("b3", 247.0, 0.06),
        ("v1a_p", 138.0, 0.15),
        ("v1b_p", 135.0, 0.15),
        ("v1_k", 1.9, 0.05),
        ("v2a_p", 164.0, 0.05),
        ("v2a_k", 1.2, 0.10),
    ];
    let idx: Vec<usize> = nodes.iter().map(|(n, ..)| at(&c, n)).collect();
    let s = settled(c);
    for ((n, want, within), i) in nodes.iter().zip(idx) {
        let v = s.voltage_at(i);
        println!("{n:<6} {v:8.2} V (chart {want})");
        assert!(near(v, *want, *within), "{n}: {v} against {want}");
    }
}

fn gain_db(mut sim: Simulation, hz: f64) -> f64 {
    sim.find_operating_point();
    let t = Tone::near(96_000.0, 16_384, hz, 1e-3);
    measure::run(t, 24_000, |x| sim.process(x)).gain_db()
}

/// Small-signal gain from the jacks to the hand-off, at the controls given.
fn preamp(channel: Channel, set: &[(usize, f64)], hz: f64) -> f64 {
    let c = tweed_lux::build_channel(channel, 10_000.0, 10e6).unwrap();
    let mut s = Simulation::new(c, 96_000.0);
    for &(k, v) in set {
        s.set_control(k, v);
    }
    gain_db(s, hz)
}

/// The other channel's volume loads the one played (Rob Robinette: "The
/// unused channel load can run from 100k at max volume to 1M at min volume"):
/// with the played volume at noon, turning the unused one from zero to ten
/// takes several decibels off, and more off the top than the middle.
#[test]
fn the_unused_volume_loads_the_one_played() {
    for ch in [Channel::Bright, Channel::Normal] {
        let g = |other: f64, hz: f64| {
            preamp(ch, &[(VOLUME, 0.5), (OTHER_VOLUME, other), (TONE, 0.5)], hz)
        };
        let (down, up) = (g(0.0, 1_000.0), g(1.0, 1_000.0));
        println!("{ch:?}: unused volume 0 {down:+.1} dB, 10 {up:+.1} dB at 1 kHz");
        assert!(down - up > 4.0, "{ch:?}: {down} {up}");
        // At ten the played channel's own volume is in series with a few tens
        // of kilohms, a divider: the knob moves the level far more.
        let span_loaded = preamp(ch, &[(VOLUME, 1.0), (OTHER_VOLUME, 1.0)], 1_000.0)
            - preamp(ch, &[(VOLUME, 0.5), (OTHER_VOLUME, 1.0)], 1_000.0);
        let span_free = preamp(ch, &[(VOLUME, 1.0), (OTHER_VOLUME, 0.0)], 1_000.0)
            - preamp(ch, &[(VOLUME, 0.5), (OTHER_VOLUME, 0.0)], 1_000.0);
        println!("{ch:?}: volume noon to ten {span_free:+.1} dB free, {span_loaded:+.1} dB loaded");
        assert!(span_loaded > span_free + 3.0, "{span_loaded} {span_free}");
    }
}

/// The tone control: down, the .005 on V2A's grid takes the top away; up,
/// the bright channel gets its 500 pF round the volume and the normal
/// channel only loses the cut.
#[test]
fn the_tone_control_cuts_and_the_bright_channel_gets_the_cap() {
    let tilt = |ch: Channel, tone: f64| {
        let set = [(VOLUME, 0.4), (OTHER_VOLUME, 0.3), (TONE, tone)];
        preamp(ch, &set, 4_000.0) - preamp(ch, &set, 400.0)
    };
    let (b0, b1, n0, n1) = (
        tilt(Channel::Bright, 0.0),
        tilt(Channel::Bright, 1.0),
        tilt(Channel::Normal, 0.0),
        tilt(Channel::Normal, 1.0),
    );
    println!("4 kHz against 400 Hz: bright {b0:+.1} / {b1:+.1} dB, normal {n0:+.1} / {n1:+.1} dB");
    assert!(b1 - b0 > 20.0, "{b0} {b1}");
    assert!(n1 - n0 > 5.0, "{n0} {n1}");
    // Only the bright channel's top is lifted above its middle.
    assert!(b1 > 3.0 && b1 > n1 + 6.0, "{b1} {n1}");
}

/// The cathodyne at idle is balanced: the plate and the cathode swing
/// equally and oppositely, each a little under the grid.
#[test]
fn the_cathodyne_is_balanced() {
    let spec = &power::PowerSpec::TWEED_6V6;
    let gain = |node: &str| {
        let c = power::tap(spec, 10_000.0, node).unwrap();
        let mut s = Simulation::new(c, 96_000.0);
        s.find_operating_point();
        let t = Tone::near(96_000.0, 16_384, 1_000.0, 0.1);
        measure::run(t, 24_000, |x| s.process(x)).gain_db()
    };
    let (plate, cathode) = (gain("pi_p1"), gain("pi_p2"));
    println!("plate {plate:+.2} dB, cathode {cathode:+.2} dB");
    assert!((plate - cathode).abs() < 0.5, "{plate} {cathode}");
    assert!(plate < 0.0 && plate > -2.0, "{plate}");
}

/// Played hard, the stage leans on its supply and its bias: the 5Y3 takes
/// B+1 down further than a GZ34 in its place would (the GZ34 at the same
/// idle voltage), the screens' B+2 behind its 5 k falls further still, and
/// the shared cathode climbs, biasing the pair back.
#[test]
fn the_5y3_sags_and_the_bias_follows() {
    let sag = |rectifier: RectifierSpec, open: f64| {
        let spec = power::PowerSpec {
            rectifier: Some(rectifier),
            plate_supply: open,
            ..power::PowerSpec::TWEED_6V6
        };
        let c = power::build(&spec, 10_000.0).unwrap();
        let (ht, scr, k) = (at(&c, "ht"), at(&c, "scr"), at(&c, "ok"));
        let mut s = Simulation::new(c, 48_000.0);
        s.find_operating_point();
        let idle = [s.voltage_at(ht), s.voltage_at(scr), s.voltage_at(k)];
        let mut worst = idle;
        for n in 0..24_000 {
            let t = n as f64 / 48_000.0;
            s.process(30.0 * (std::f64::consts::TAU * 110.0 * t).sin());
            worst[0] = worst[0].min(s.voltage_at(ht));
            worst[1] = worst[1].min(s.voltage_at(scr));
            worst[2] = worst[2].max(s.voltage_at(k));
        }
        (idle, worst)
    };
    let (idle, five) = sag(power::T5Y3GT, power::TWEED_RAW_OPEN);
    // The open voltage that puts a GZ34 at the same idle: its drop at the
    // stage's idle current is about a third of the 5Y3's.
    let (gz_idle, gz) = sag(RectifierSpec::GZ34, power::TWEED_RAW_OPEN - 31.0);
    println!(
        "5Y3: B+1 {:.1} -> {:.1} V, B+2 {:.1} -> {:.1} V, cathodes {:.1} -> {:.1} V",
        idle[0], five[0], idle[1], five[1], idle[2], five[2]
    );
    println!("GZ34: B+1 {:.1} -> {:.1} V", gz_idle[0], gz[0]);
    assert!((gz_idle[0] - idle[0]).abs() < 3.0, "{gz_idle:?} {idle:?}");
    let (five_sag, gz_sag) = (idle[0] - five[0], gz_idle[0] - gz[0]);
    assert!(five_sag > 1.15 * gz_sag, "{five_sag} {gz_sag}");
    assert!(idle[1] - five[1] > 1.3 * five_sag, "{idle:?} {five:?}");
    assert!(five[2] - idle[2] > 3.0, "{idle:?} {five:?}");
}

/// Printed: the small-signal gain to the hand-off across the controls.
#[test]
#[ignore]
fn print_the_preamp() {
    for ch in [Channel::Bright, Channel::Normal] {
        for (vol, other, tone) in [
            (1.0, 0.0, 0.5),
            (1.0, 0.5, 0.5),
            (1.0, 1.0, 0.5),
            (0.5, 0.5, 0.5),
            (0.5, 0.5, 0.0),
            (0.5, 0.5, 1.0),
        ] {
            let set = [(VOLUME, vol), (OTHER_VOLUME, other), (TONE, tone)];
            let g = |hz| preamp(ch, &set, hz);
            println!(
                "{ch:?} vol {vol} other {other} tone {tone}: 100 Hz {:+.1} dB, 1 kHz {:+.1}, 5 kHz {:+.1}",
                g(100.0),
                g(1_000.0),
                g(5_000.0)
            );
        }
    }
}
