//! The Tweed Lux and the Seattle 100 as the chain plays them: every switch
//! position and the controls at their ends, into a resistor and into a
//! loudspeaker, at both qualities -- finite, settled, and sounding -- and
//! what each costs against the catalogue.

use faderframe_guitar::acoustics::cabinet::CabinetProfile;
use faderframe_guitar::chain::{self, Chain, Settings, AMPS};
use faderframe_guitar::voice::{AcousticSettings, CabinetChoice, Gain, Throw};

const RATE: f64 = 48_000.0;
const NEW: [Gain; 4] = [
    Gain::TweedLux,
    Gain::TweedLuxNormal,
    Gain::Seattle100,
    Gain::Seattle100Normal,
];

/// A plucked chord at about the level a guitar is tracked at, hit hard.
fn pluck(n: usize, gain: f64) -> f64 {
    let t = n as f64 / RATE;
    let since = (t * 2.0).fract() / 2.0;
    let env = (-since * 5.0).exp();
    gain * 0.25
        * env
        * ((std::f64::consts::TAU * 82.4 * t).sin()
            + 0.6 * (std::f64::consts::TAU * 123.5 * t).sin()
            + 0.4 * (std::f64::consts::TAU * 164.8 * t).sin())
}

fn play(chain: &mut Chain, s: &Settings, seconds: f64, hit: f64) -> (f64, f64) {
    chain.apply(s);
    chain.reset();
    chain.apply(s);
    chain.find_operating_point();
    let n = (seconds * RATE) as usize;
    let (mut sum, mut peak) = (0.0, 0.0f64);
    for k in 0..n {
        let x = pluck(k, hit);
        let f = chain.process(x, x, false);
        assert!(f.left.is_finite() && f.right.is_finite(), "{:?}", s.amp);
        if k >= n / 2 {
            sum += f.left * f.left;
            peak = peak.max(f.left.abs());
        }
    }
    (10.0 * (sum / (n / 2) as f64).max(1e-30).log10(), peak)
}

fn cabinet() -> AcousticSettings {
    AcousticSettings {
        cabinet: CabinetChoice::Model(&CabinetProfile::AMERICAN_OPEN_112),
        ..AcousticSettings::default()
    }
}

/// Every position of every switch, the knobs at their ends, a loudspeaker
/// and a resistor, both qualities: finite, every solve settled, and audible.
#[test]
fn every_setting_plays_and_settles() {
    for quality in [1, 2] {
        let mut chain = Chain::new(RATE, quality).unwrap();
        for amp in NEW {
            for throw in [Throw::Left, Throw::Centre, Throw::Right] {
                for (knobs, acoustic) in [
                    (0.0, AcousticSettings::default()),
                    (1.0, AcousticSettings::default()),
                    (1.0, cabinet()),
                    (0.5, cabinet()),
                ] {
                    // At twice the rate only the hardest case: every knob up
                    // into a loudspeaker.
                    if quality == 2 && (knobs < 1.0 || acoustic.cabinet == CabinetChoice::Legacy) {
                        continue;
                    }
                    let s = Settings {
                        amp,
                        acoustic,
                        drive: knobs,
                        // A master at zero is silence, which is not what is tested.
                        master: knobs.max(0.25),
                        presence: knobs,
                        bass: knobs,
                        mid: knobs,
                        treble: knobs,
                        sweep: knobs,
                        bright: knobs > 0.5,
                        low_switch: throw,
                        mains: if knobs > 0.5 { 0.8 } else { 1.0 },
                        ..Settings::default()
                    };
                    let before = chain.statistics();
                    let (db, peak) = play(&mut chain, &s, 0.4, 2.0);
                    let after = chain.statistics();
                    let unsettled = after.2 - before.2;
                    println!(
                        "{:<20} {quality}x {throw:?} knobs {knobs} {}: {db:6.1} dB, peak {peak:.2}, unsettled {unsettled}",
                        chain::amp_name(amp),
                        if acoustic.cabinet == CabinetChoice::Legacy { "resistor" } else { "speaker" },
                    );
                    assert_eq!(unsettled, 0, "{amp:?} {s:?}");
                    assert!(db > -80.0 && db < 20.0, "{amp:?} {db}");
                }
            }
        }
    }
}

/// The tweed jumpered is louder and dirtier than either channel alone: both
/// halves of V1 carry the guitar and add at V2A's grid.
#[test]
fn jumpering_the_tweed_adds_the_channels() {
    let mut chain = Chain::new(RATE, 1).unwrap();
    let at = |chain: &mut Chain, throw| {
        let s = Settings {
            amp: Gain::TweedLux,
            drive: 0.3,
            sweep: 0.3,
            low_switch: throw,
            ..Settings::default()
        };
        play(chain, &s, 0.5, 0.2).0
    };
    let (low, high, jumpered) = (
        at(&mut chain, Throw::Left),
        at(&mut chain, Throw::Centre),
        at(&mut chain, Throw::Right),
    );
    println!("jack 2 {low:.1} dB, jack 1 {high:.1} dB, jumpered {jumpered:.1} dB");
    assert!(high > low + 3.0, "{low} {high}");
    assert!(jumpered > high + 1.0, "{high} {jumpered}");
}

/// What a channel's amplifier stage costs on this thread's own clock (not the
/// wall's, so a loaded machine does not inflate it), against the catalogue:
/// three seconds of a pluck at Drive 0.7 through the default cabinet path.
/// Run with --ignored --nocapture.
#[test]
#[ignore]
fn cost_against_the_catalogue() {
    fn cpu_ns() -> u64 {
        std::fs::read_to_string("/proc/thread-self/schedstat")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
            .unwrap_or(0)
    }
    let n = (RATE * 3.0) as usize;
    for quality in [1, 2] {
        let mut c = Chain::new(RATE, quality).unwrap();
        for amp in AMPS {
            let s = Settings {
                amp,
                drive: 0.7,
                ..Settings::default()
            };
            c.apply(&s);
            c.reset();
            c.apply(&s);
            c.find_operating_point();
            let t = cpu_ns();
            for k in 0..n {
                let x = pluck(k, 1.0);
                std::hint::black_box(c.process(x, x, false));
            }
            let used = (cpu_ns() - t) as f64 * 1e-9;
            let (solves, passes, unsettled, _) = c.statistics();
            println!(
                "{quality}x {:<24} {:5.1} % of a core, {:.2} passes/solve, {unsettled} unsettled",
                chain::amp_name(amp),
                100.0 * used / 3.0,
                passes as f64 / solves.max(1) as f64
            );
        }
    }
}
