//! The British 73's calibration, measured on its circuit as the others'
//! were (upstream): the drive (volts at the microphone input for −18 dBFS)
//! that makes 3 % distortion at full gain, and per Gain position (33, on
//! the cube-root taper) the make-up that brings −18 dBFS back to unity —
//! with the output block driven from the card's trim into the 600 ohm line
//! it was built for. Also how much louder a bridging input is. Run by hand
//! after changing the circuit:
//! `cargo test -p faderframe-circuit --release --test british73_calibration
//! -- --ignored --nocapture`, then paste the entry into
//! `src/calibration.rs` and `BRIDGING_DB` into `src/preamp.rs`.

use faderframe_circuit::circuits::neve;
use faderframe_circuit::dsp::measure::{run, Measured, Tone};
use faderframe_circuit::dsp::time::Simulation;
use faderframe_circuit::preamp::{BRIDGING, LINE_SOURCE, OVERSAMPLING, TERMINATED};

/// The circuits' own rate (48 kHz, oversampled).
const RATE: f64 = 48_000.0 * OVERSAMPLING as f64;

/// The card and its line at `control`, a 1 kHz tone of `volts` in.
fn chain(control: f64, volts: f64, load: f64) -> Measured {
    let mut card = Simulation::new(neve::build(150.0, 10_000.0).expect("the card"), RATE);
    card.set_control(0, control);
    card.reset();
    let mut line = Simulation::new(neve::output(LINE_SOURCE, load).expect("the line"), RATE);
    line.reset();
    let tone = Tone::near(RATE, 19_200, 1000.0, volts);
    run(tone, 57_600, |x| line.process(card.process(x)))
}

#[test]
#[ignore = "measures the circuit; prints the calibration"]
fn british73_calibration() {
    // The drive: 3 % at full gain (bisection in log).
    let (mut lo, mut hi) = (1e-5f64.ln(), 1.0f64.ln());
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if chain(1.0, mid.exp(), TERMINATED).thd_percent() < 3.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let drive = (0.5 * (lo + hi)).exp();
    let full = chain(1.0, drive, TERMINATED);
    let table: Vec<String> = (0..33)
        .map(|i| {
            let control = (i as f64 / 32.0).powi(3);
            format!("{:.2}", -chain(control, drive, TERMINATED).gain_db())
        })
        .collect();
    let bridging = chain(0.5, drive, BRIDGING).gain_db() - chain(0.5, drive, TERMINATED).gain_db();
    println!(
        "// Neve 73P with transistors, its output block into 600 ohms: {drive:.4} V in, {:.1} % distortion, {:.1} % third.",
        full.thd_percent(),
        full.harmonic_percent(3)
    );
    println!(
        "    Calibration {{\n        drive_volts: {drive:.6},\n        make_up_db: [{}],\n    }},",
        table.join(", ")
    );
    println!("BRIDGING_DB = {bridging:.2}");
}
