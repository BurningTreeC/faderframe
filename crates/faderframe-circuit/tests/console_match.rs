//! The channels' console stage against the bus circuit it was baked from:
//! the distortion at levels from the nominal to +6 dBFS (at 60 Hz, 1 kHz
//! and 5 kHz), the compression when hot and the small-signal response,
//! each within its tolerance — so the light model on every channel sounds
//! like the circuit on the buses. The distortion is within 35 % of the
//! circuit's, or (where it starts steeply: the British 73's class A rises
//! tenfold in a dB) within what the circuit has a quarter of a dB either
//! side; and the stage's distortion grows with the level all the way, as
//! the circuit's does. The valve consoles (Tube 610, British 47, German
//! 76), whose feedback makes the distortion depend on the frequency more
//! than the model follows, within a factor of 2.5 or 0.1 %, and their
//! response within 0.4 dB at 20 kHz.

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
            // The valve consoles' feedback makes their distortion depend
            // on the frequency more than two bands and a curve follow:
            // within a factor of 2.5 or a tenth of a percent there.
            let close = if f >= 3 {
                st < 2.5 * ct.max(0.04) && ct < 2.5 * st.max(0.04) || (st - ct).abs() < 0.1
            } else {
                (st - ct).abs() <= (0.35 * ct).max(0.03)
            };
            if !close && !near {
                bad.push(format!(
                    "{name} THD at {hz} Hz {dbfs:+} dBFS: {st:.3} % vs {ct:.3} %"
                ));
            }
            // Past a fifth of the signal in harmonics (+28 dBu into a
            // valve booster) the level is no measure of anything.
            if ct < 20.0 && (sg - cg).abs() > 0.3 {
                bad.push(format!(
                    "{name} gain at {hz} Hz {dbfs:+} dBFS: {sg:+.2} vs {cg:+.2} dB"
                ));
            }
        }
        let ((reference, _), (sref, _)) = both(f, 1000.0, -30.0);
        for hz in [20.0, 100.0, 10_000.0, 20_000.0] {
            let ((cg, _), (sg, _)) = both(f, hz, -30.0);
            // A valve console's top (the V76's 15 kHz LC network) is
            // steeper at 20 kHz than one second-order section follows.
            let limit = if f >= 3 && hz >= 20_000.0 { 0.4 } else { 0.3 };
            if ((sg - sref) - (cg - reference)).abs() > limit {
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
