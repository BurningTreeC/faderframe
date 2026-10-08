//! How long each preamp circuit takes for a second of audio at 48 kHz
//! (a full-scale 100 Hz sine; release build).
use faderframe_circuit::preamp::{Preamp, CONSOLE_BUSES, MODELS};
use std::time::Instant;

fn main() {
    for model in 0..MODELS + CONSOLE_BUSES {
        let mut p = Preamp::new(model, 48_000.0, if model == 3 { 0.5 } else { 0.0 }, 0.0).unwrap();
        let t = Instant::now();
        let mut acc = 0.0;
        for i in 0..48_000 {
            acc += p.process(0.5 * (std::f64::consts::TAU * 100.0 * i as f64 / 48_000.0).sin());
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "model {model}: {ms:.1} ms per second of audio ({:.2} % of a core) {acc:.1}",
            ms / 10.0
        );
    }
    for f in 0..CONSOLE_BUSES {
        let mut s = faderframe_circuit::console::ConsoleStage::new(f, 6.0, 48_000.0);
        let t = Instant::now();
        let mut acc = 0.0;
        for i in 0..480_000 {
            acc += s.process(0.5 * (std::f64::consts::TAU * 100.0 * i as f64 / 48_000.0).sin());
        }
        let ms = t.elapsed().as_secs_f64() * 100.0;
        println!(
            "channel stage {f}: {ms:.2} ms per second of audio ({:.3} % of a core) {acc:.1}",
            ms / 10.0
        );
    }
}
