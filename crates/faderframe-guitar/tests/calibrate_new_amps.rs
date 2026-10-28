//! The calibration (`src/calibration.rs`) and power-trim (`src/power_trim.rs`)
//! rows of the amplifiers added after the extraction, measured by upstream's
//! procedure (GainStageFx `examples/calibrate.rs` and `examples/powertrim.rs`,
//! whose reasoning those files cite): a guitar's 0.122 V in, the make-up the
//! negative of the broadband level of a low chord through the preamplifier and
//! its own power stage at 96 kHz, at each knot of the Drive control, anchored
//! to the Twin Reverb's knot 29 at -51.44624 dB; the power trim the level
//! change of each power override against the voice's own path, through the
//! chain at the calibration drive.
//!
//! `cargo test -p faderframe-guitar --release --test calibrate_new_amps --
//! --ignored --nocapture` prints the rows; the plain tests hold them.

use faderframe_guitar::chain::{Chain, Settings};
use faderframe_guitar::dsp::time::Simulation;
use faderframe_guitar::voice::{
    self, knot_position, Amplifier, Diode, Gain, PowerAmp, CALIBRATION, NOMINAL_DBFS, POINTS,
    POWER_TRIM_DB, VOICES,
};

const RATE: f64 = 96_000.0;
const GUITAR_VOLTS: f64 = 0.122;
const ANCHOR_VOICE: Gain = Gain::Twin;
const ANCHOR_KNOT: usize = 29;
const ANCHOR_DB: f64 = -51.44624;

/// The added amplifiers, in `Gain::ALL` order.
const NEW: [Gain; 4] = [
    Gain::TweedLux,
    Gain::TweedLuxNormal,
    Gain::Seattle100,
    Gain::Seattle100Normal,
];

struct Voice {
    gain: Simulation,
    power: Option<Simulation>,
}

impl Voice {
    fn new(gain: Gain, drive: f64) -> Self {
        let mut v = Self {
            gain: Simulation::new(
                voice::build_voice(gain, Diode::Silicon, Amplifier::Valve).unwrap(),
                RATE,
            ),
            power: voice::build_power(gain).map(|b| Simulation::new(b.unwrap(), RATE)),
        };
        v.gain.set_control(gain.drive_control(), drive);
        v
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.gain.process(x);
        match self.power {
            Some(ref mut p) => p.process(y),
            None => y,
        }
    }
}

/// Upstream's `broadband_db`: a quarter second of a low chord to settle, a
/// quarter second measured, output against input.
fn broadband_db(sim: &mut Voice, volts: f64) -> f64 {
    let n = (RATE / 4.0) as usize;
    let partials = [(82.4, 0.5), (123.5, 0.3), (246.9, 0.2)];
    let chord = |k: usize| -> f64 {
        let t = k as f64 / RATE;
        partials
            .iter()
            .map(|(hz, a)| a * (std::f64::consts::TAU * hz * t).sin())
            .sum()
    };
    for k in 0..n {
        sim.process(volts * chord(k));
    }
    let (mut out, mut inp) = (0.0, 0.0);
    for k in n..2 * n {
        let x = volts * chord(k);
        let y = sim.process(x);
        out += y * y;
        inp += x * x;
    }
    10.0 * (out / inp.max(1e-30)).max(1e-30).log10()
}

fn raw_make_up(gain: Gain, knot: usize) -> f64 {
    let mut v = Voice::new(gain, knot_position(knot));
    -broadband_db(&mut v, GUITAR_VOLTS)
}

fn anchor() -> f64 {
    ANCHOR_DB - raw_make_up(ANCHOR_VOICE, ANCHOR_KNOT)
}

fn row(gain: Gain) -> usize {
    (0..VOICES).find(|&i| voice::voice_at(i).0 == gain).unwrap()
}

/// The make-up rows, printed as `calibration.rs` has them.
#[test]
#[ignore]
fn print_the_calibration_rows() {
    let anchor = anchor();
    eprintln!("anchor {anchor:+.5} dB");
    // The procedure reproduces an untouched voice's committed row.
    let check = row(Gain::Brit45);
    for knot in [4, 16, 32] {
        let got = raw_make_up(Gain::Brit45, knot) + anchor;
        let table = CALIBRATION[check].make_up_db[knot];
        eprintln!("JTM45 knot {knot}: {got:+.2} against the table's {table:+.2}");
    }
    let rows: Vec<(Gain, [f64; POINTS])> = std::thread::scope(|s| {
        let handles: Vec<_> = NEW
            .iter()
            .map(|&gain| {
                s.spawn(move || {
                    let mut m = [0.0; POINTS];
                    for (i, slot) in m.iter_mut().enumerate() {
                        *slot = raw_make_up(gain, i) + anchor;
                    }
                    (gain, m)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (gain, m) in rows {
        let mut check = Voice::new(gain, 1.0);
        let tone = faderframe_guitar::dsp::measure::Tone::near(RATE, 16_384, 220.0, GUITAR_VOLTS);
        let got = faderframe_guitar::dsp::measure::run(tone, (RATE / 10.0) as usize, |x| {
            check.process(x)
        });
        println!(
            "    // {} with a valve: {GUITAR_VOLTS:.4} V in, {:.1} % distortion, {:.1} % third.",
            gain.name(),
            got.thd_percent(),
            got.harmonic_percent(3)
        );
        println!("    Calibration {{");
        println!("        drive_volts: {GUITAR_VOLTS:.6},");
        let values: Vec<String> = m.iter().map(|v| format!("{v:.2}")).collect();
        println!("        make_up_db: [{}],", values.join(", "));
        println!("    }},");
    }
}

/// The new rows still describe the circuits: re-measured at four knots.
#[test]
fn the_new_calibration_rows_hold() {
    let anchor = anchor();
    for gain in NEW {
        let table = &CALIBRATION[row(gain)];
        assert_eq!(table.drive_volts, GUITAR_VOLTS);
        for knot in [8, 20, 26, 32] {
            let got = raw_make_up(gain, knot) + anchor;
            let want = table.make_up_db[knot];
            assert!(
                (got - want).abs() < 0.05,
                "{} knot {knot}: {got:+.3} against {want:+.2}",
                gain.name()
            );
        }
    }
}

/// Upstream's power-trim `level`: the chain at the calibration drive, its
/// output's RMS over the second half of a second of the low chord.
fn level(chain: &mut Chain, gain: Gain, power_amp: PowerAmp) -> f64 {
    let rate = 48_000.0;
    let s = Settings {
        amp: gain,
        power_amp,
        drive: 0.5,
        ..Settings::default()
    };
    chain.apply(&s);
    chain.reset();
    chain.apply(&s);
    chain.find_operating_point();
    let amplitude = 10f64.powf(NOMINAL_DBFS / 20.0);
    let n = rate as usize;
    let mut sum = 0.0;
    for i in 0..n {
        let t = i as f64 / rate;
        let x = amplitude
            * ((std::f64::consts::TAU * 82.4 * t).sin() * 0.5
                + (std::f64::consts::TAU * 123.5 * t).sin() * 0.3
                + (std::f64::consts::TAU * 246.9 * t).sin() * 0.2);
        let y = chain.process(x, x, false).left;
        if i >= n / 2 {
            sum += y * y;
        }
    }
    (sum / (n / 2) as f64).sqrt()
}

fn overrides() -> Vec<PowerAmp> {
    PowerAmp::ALL
        .into_iter()
        .filter(|p| *p != PowerAmp::Matched)
        .collect()
}

fn gain_row(gain: Gain) -> usize {
    Gain::ALL.iter().position(|g| *g == gain).unwrap()
}

/// The power-trim rows, printed as `power_trim.rs` has them: the table's
/// entry plus what is left over, so a row can be re-measured in place.
#[test]
#[ignore]
fn print_the_power_trim_rows() {
    let rows: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = NEW
            .iter()
            .map(|&gain| {
                s.spawn(move || {
                    let mut chain = Chain::new(48_000.0, 1).unwrap();
                    let reference = level(&mut chain, gain, PowerAmp::Matched);
                    let cells: Vec<String> = overrides()
                        .iter()
                        .enumerate()
                        .map(|(column, p)| {
                            let old = POWER_TRIM_DB[gain_row(gain)][column];
                            let residual = 20.0 * (level(&mut chain, gain, *p) / reference).log10();
                            format!("{:.2}", old + residual)
                        })
                        .collect();
                    format!("    [{}], // {}", cells.join(", "), gain.name())
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for r in rows {
        println!("{r}");
    }
}

/// Every override holds the new voices' level (`Chain::power_trim`): checked
/// on three columns a voice, the stage that is least like its own included.
#[test]
fn the_power_trim_holds_the_new_voices_level() {
    let mut chain = Chain::new(48_000.0, 1).unwrap();
    for gain in NEW {
        let reference = level(&mut chain, gain, PowerAmp::Matched);
        for p in [
            PowerAmp::Bypass,
            PowerAmp::BritEL34,
            PowerAmp::AmericanSS800,
        ] {
            let db = 20.0 * (level(&mut chain, gain, p) / reference).log10();
            assert!(db.abs() < 1.0, "{} into {p:?}: {db:+.2} dB", gain.name());
        }
    }
}
