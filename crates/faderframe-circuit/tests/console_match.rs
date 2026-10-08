//! The channels' console stage against the bus circuit it was baked from:
//! the distortion at levels from the nominal to +6 dBFS (at 60 Hz, 1 kHz
//! and 5 kHz), the compression when hot and the small-signal response,
//! each within its tolerance — so the light model on every channel sounds
//! like the circuit on the buses. The distortion is within 35 % of the
//! circuit's, or (where it starts steeply: the British 73's class A rises
//! tenfold in a dB) within what the circuit has a quarter of a dB either
//! side; and the stage's distortion grows with the level all the way, as
//! the circuit's does.

use faderframe_circuit::circuits::console_bus::FAMILIES;
use faderframe_circuit::console::ConsoleStage;
use faderframe_circuit::dsp::measure::{run, Tone};
use faderframe_circuit::preamp::{Preamp, MODELS};

const RATE: f64 = 48_000.0;

fn both(f: usize, hz: f64, dbfs: f64) -> ((f64, f64), (f64, f64)) {
    let tone = Tone::near(RATE, 9600, hz, 10f64.powf(dbfs / 20.0));
    let mut c = Preamp::new(MODELS + f, RATE, 0.0, 0.0).expect("the bus");
    // Both settle 0.6 s: the British 73's bias servo and coupling take more
    // than a fifth of a second to come to rest.
    let mc = run(tone, 28_800, |x| c.process(x));
    let mut s = ConsoleStage::new(f, 0.0, RATE);
    let ms = run(tone, 28_800, |x| s.process(x));
    (
        (mc.gain_db(), mc.thd_percent()),
        (ms.gain_db(), ms.thd_percent()),
    )
}

#[test]
fn the_channel_stage_matches_its_circuit() {
    let mut report = String::new();
    let mut bad = Vec::new();
    for (f, name) in FAMILIES.iter().enumerate() {
        for (hz, dbfs) in [
            (1000.0, -18.0),
            (1000.0, -3.0),
            (60.0, -3.0),
            (5000.0, -3.0),
            (1000.0, -6.0),
            (1000.0, 0.0),
            (1000.0, 6.0),
            (60.0, 0.0),
            (60.0, 6.0),
            (5000.0, 0.0),
            (5000.0, 6.0),
            (200.0, 3.0),
        ] {
            let ((cg, ct), (sg, st)) = both(f, hz, dbfs);
            let ((_, below), _) = both(f, hz, dbfs - 0.25);
            let ((_, above), _) = both(f, hz, dbfs + 0.25);
            let near = (below.min(above) * 0.9..=below.max(above) * 1.1).contains(&st);
            report.push_str(&format!(
                "{name} {hz} Hz {dbfs:+} dBFS: circuit {ct:.3} % {cg:+.2} dB, stage {st:.3} % {sg:+.2} dB\n"
            ));
            if (st - ct).abs() > (0.35 * ct).max(0.03) && !near {
                bad.push(format!(
                    "{name} THD at {hz} Hz {dbfs:+} dBFS: {st:.3} % vs {ct:.3} %"
                ));
            }
            if (sg - cg).abs() > 0.3 {
                bad.push(format!(
                    "{name} gain at {hz} Hz {dbfs:+} dBFS: {sg:+.2} vs {cg:+.2} dB"
                ));
            }
        }
        let ((reference, _), (sref, _)) = both(f, 1000.0, -30.0);
        for hz in [20.0, 100.0, 10_000.0, 20_000.0] {
            let ((cg, _), (sg, _)) = both(f, hz, -30.0);
            if ((sg - sref) - (cg - reference)).abs() > 0.3 {
                bad.push(format!(
                    "{name} response at {hz} Hz: {:+.2} vs {:+.2} dB",
                    sg - sref,
                    cg - reference
                ));
            }
        }
    }
    // Distortion rising with the level, from the nominal to +8 dBFS, as
    // the circuits' does (mixing curves made two knees and dips before).
    for (f, name) in FAMILIES.iter().enumerate() {
        for hz in [60.0, 1000.0, 5000.0] {
            let mut last = 0.0;
            for half in -12..=16 {
                let dbfs = f64::from(half) * 0.5;
                let tone = Tone::near(RATE, 9600, hz, 10f64.powf(dbfs / 20.0));
                let mut s = ConsoleStage::new(f, 0.0, RATE);
                let st = run(tone, 28_800, |x| s.process(x)).thd_percent();
                if st < last * 0.9 && last > 0.05 {
                    bad.push(format!(
                        "{name} at {hz} Hz: distortion falls from {last:.3} % to {st:.3} % at {dbfs:+} dBFS"
                    ));
                }
                last = st;
            }
        }
    }
    eprintln!("{report}");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
