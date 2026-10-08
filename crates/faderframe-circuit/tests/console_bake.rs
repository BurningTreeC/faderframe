//! Bakes the channels' console models (`src/console_models.rs`) from the
//! console bus circuits (`circuits::console_bus`, run as `Preamp`s at their
//! calibration: −18 dBFS is +4 dBu at the line, the drive at 0 dB):
//!
//! * the small-signal response (−30 dBFS) from 10 Hz to 20 kHz, fitted with
//!   a second-order high-pass, a first-order low-pass, a bell for what is
//!   left between 50 Hz and 10 kHz and a high shelf for the top octave;
//! * the transfer curves: periods near 1 kHz at each of `console::LEVELS`
//!   (−24 to +12 dBFS), each aligned to its fundamental, binned into a
//!   table of the output against the input, both relative to the level
//!   (each bin the mean of what fell in it), unity at small levels.
//!
//! `tests/console_match.rs` then holds the runtime against the circuits.
//!
//! `FADERFRAME_BAKE_CONSOLE=1 cargo test -p faderframe-circuit --release
//! --test console_bake -- --ignored --nocapture`.

use faderframe_circuit::circuits::console_bus::FAMILIES;
use faderframe_circuit::console::{ConsoleModel, ConsoleStage, LEVELS, LEVEL_COUNT, TABLE_POINTS};
use faderframe_circuit::dsp::measure::{run, Tone};
use faderframe_circuit::preamp::{Preamp, MODELS};

const RATE: f64 = 48_000.0;
const WINDOW: usize = 9600;
/// How long a tone runs before the window: the British 73's bias servo
/// and coupling settle over more than a fifth of a second.
const SETTLE: usize = 3 * WINDOW;
/// Where the stage's emphasis bands turn (Hz).
const EMPHASIS: (f64, f64) = (150.0, 3000.0);

/// The family's bus circuit at its calibration, the drive at 0 dB.
fn bus(family: usize) -> Preamp {
    Preamp::new(MODELS + family, RATE, 0.5, 0.0).expect("the bus builds")
}

fn gain(family: usize, hz: f64) -> f64 {
    let mut p = bus(family);
    let tone = Tone::near(RATE, WINDOW, hz, 10f64.powf(-30.0 / 20.0));
    run(tone, SETTLE, |x| p.process(x)).gain_db()
}

/// A tone through the circuit: the tone and a window of what came out.
fn through(family: usize, hz: f64, amplitude: f64) -> (Tone, Vec<f64>) {
    let mut p = bus(family);
    let tone = Tone::near(RATE, WINDOW, hz, amplitude);
    for i in 0..SETTLE {
        p.process(tone.at(i));
    }
    let samples = (0..WINDOW)
        .map(|i| p.process(tone.at(SETTLE + i)))
        .collect();
    (tone, samples)
}

fn hp_db(f: f64, fc: f64, q: f64) -> f64 {
    if fc <= 0.0 {
        return 0.0;
    }
    let x = f / fc;
    20.0 * (x * x / ((1.0 - x * x).powi(2) + (x / q).powi(2)).sqrt()).log10()
}

fn lp_db(f: f64, fc: f64) -> f64 {
    if fc <= 0.0 {
        return 0.0;
    }
    -10.0 * (1.0 + (f / fc).powi(2)).log10()
}

/// A first-order high shelf's magnitude (dB) at `f`: `db` above `fc`.
fn shelf_db(f: f64, fc: f64, db: f64) -> f64 {
    let g = 10f64.powf(db / 20.0);
    let x = (f / fc).powi(2);
    10.0 * ((1.0 + g * g * x) / (1.0 + x)).log10()
}

const FREQS: [f64; 26] = [
    10.0, 15.0, 20.0, 25.0, 30.0, 40.0, 50.0, 63.0, 80.0, 100.0, 150.0, 200.0, 300.0, 500.0, 700.0,
    1000.0, 1500.0, 2000.0, 3000.0, 5000.0, 7000.0, 10000.0, 12500.0, 15000.0, 18000.0, 20000.0,
];

fn grid(a: f64, b: f64, n: usize) -> impl Iterator<Item = f64> {
    (0..=n).map(move |i| a * (b / a).powf(i as f64 / n as f64))
}

fn bake(family: usize) -> String {
    // The response.
    let reference = gain(family, 1000.0);
    let resp: Vec<(f64, f64)> = FREQS
        .iter()
        .map(|&f| (f, gain(family, f) - reference))
        .collect();
    let err = |fh: f64, q: f64, fl: f64, sh: (f64, f64)| -> f64 {
        resp.iter()
            .filter(|(f, _)| *f <= 300.0 || *f >= 2000.0)
            .map(|(f, db)| {
                (db - hp_db(*f, fh, q) - lp_db(*f, fl) - shelf_db(*f, sh.0, sh.1)).powi(2)
            })
            .sum()
    };
    let none = (10_000.0, 0.0);
    let mut best = (0.0, 0.707, 0.0, err(0.0, 0.707, 0.0, none));
    for q in [0.5, 0.707, 1.0] {
        for fh in std::iter::once(0.0).chain(grid(0.5, 80.0, 120)) {
            let e = err(fh, q, 0.0, none);
            if e < best.3 {
                best = (fh, q, 0.0, e);
            }
        }
    }
    for fl in grid(4000.0, 400_000.0, 160) {
        let e = err(best.0, best.1, fl, none);
        if e < best.3 {
            best = (best.0, best.1, fl, e);
        }
    }
    // A rise in the top octave (the British 73's iron and C26): a shelf.
    let mut shelf = (none, best.3);
    for fc in grid(4000.0, 16_000.0, 50) {
        for tenth in -30..=30 {
            let sh = (fc, f64::from(tenth) * 0.1);
            let e = err(best.0, best.1, best.2, sh);
            if e < shelf.1 {
                shelf = (sh, e);
            }
        }
    }
    let (hp_hz, hp_q, lp_hz, _) = best;
    let shelf = shelf.0;
    let residual = resp
        .iter()
        .filter(|(f, _)| (50.0..=10_000.0).contains(f))
        .map(|(f, db)| {
            (
                *f,
                db - hp_db(*f, hp_hz, hp_q) - lp_db(*f, lp_hz) - shelf_db(*f, shelf.0, shelf.1),
            )
        })
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap_or((1000.0, 0.0));
    let bell = if residual.1.abs() > 0.2 {
        (residual.0, residual.1, 0.9)
    } else {
        (1000.0, 0.0, 1.0)
    };
    // The transfer curves. A tone at each level (995 Hz, not a whole
    // number of samples to the period, so the phases spread), aligned to
    // its fundamental and its own mean taken off; what the circuit adds to
    // the input binned over the input relative to the level (smooth across
    // a bin, where the output itself is not), the curve relative to the
    // level. An AC-coupled stage that clips unevenly moves its operating
    // point with the level, and the curve a signal meets is the one at its
    // own.
    let g0 = 10f64.powf(reference / 20.0);
    let last = (TABLE_POINTS - 1) as f64;
    let centre = |k: usize| k as f64 / last * 2.0 - 1.0;
    let tables: Vec<Vec<f64>> = LEVELS
        .iter()
        .map(|&a| {
            let (tone, y) = through(family, 995.0, a);
            let w = std::f64::consts::TAU * tone.bin / WINDOW as f64;
            let at = |i: usize| w * (SETTLE + i) as f64;
            let (s, c) = y.iter().enumerate().fold((0.0, 0.0), |(s, c), (i, v)| {
                (s + v * at(i).sin(), c + v * at(i).cos())
            });
            let theta = c.atan2(s);
            let mean = y.iter().sum::<f64>() / y.len() as f64;
            let mut sum = vec![0.0; TABLE_POINTS];
            let mut count = vec![0usize; TABLE_POINTS];
            for (i, v) in y.iter().enumerate() {
                let x = a * (at(i) + theta).sin();
                let k = ((x / a + 1.0) / 2.0 * last).round().clamp(0.0, last) as usize;
                sum[k] += (v - mean) / g0 - x;
                count[k] += 1;
            }
            let known: Vec<Option<f64>> = (0..TABLE_POINTS)
                .map(|k| (count[k] > 0).then(|| sum[k] / count[k] as f64 / a))
                .collect();
            (0..TABLE_POINTS)
                .map(|k| {
                    let d = known[k].or_else(|| {
                        (0..TABLE_POINTS)
                            .filter(|j| known[*j].is_some())
                            .min_by_key(|j| j.abs_diff(k))
                            .and_then(|j| known[j])
                    });
                    centre(k) + d.unwrap_or(0.0)
                })
                .collect()
        })
        .collect();
    let table = &tables[LEVEL_COUNT - 1];
    let leaked: &'static [[f32; TABLE_POINTS]; LEVEL_COUNT] =
        Box::leak(Box::new(std::array::from_fn(|l| {
            std::array::from_fn(|k| tables[l][k] as f32)
        })));
    const UNITY: [[f32; 4]; LEVEL_COUNT] = [[1.0; 4]; LEVEL_COUNT];
    let model = ConsoleModel {
        name: FAMILIES[family],
        hp_hz,
        hp_q,
        lp_hz,
        bell,
        shelf,
        emphasis: EMPHASIS,
        bands: &UNITY,
        tables: leaked,
    };
    // The bands, level by level: the gain into the curve so the stage's
    // distortion at 60 Hz (the low band) and 5 kHz (the high band) is the
    // circuit's (bisection: more in, more distortion; within ±6 dB), then
    // the correction after it so its level is. Only where the circuit
    // distorts audibly and the curve can follow it (no limit reached):
    // other levels take the nearest fitted level's way in, uncorrected —
    // a circuit's distortion at levels where its curve is straight (the
    // British 73's slew at 5 kHz) is not the curve's to make.
    let stage = |bands: &[[f64; 4]; LEVEL_COUNT], hz: f64, a: f64| {
        let mut st = ConsoleStage::with_model(model, 0.0, RATE);
        st.set_bands(bands);
        let tone = Tone::near(RATE, WINDOW, hz, a);
        let m = run(tone, SETTLE, |x| st.process(x));
        (m.gain_db(), m.thd_percent())
    };
    let mut bands = [[1.0f64; 4]; LEVEL_COUNT];
    for (band, hz) in [(0usize, 60.0), (1, 5000.0)] {
        let mut fitted = [false; LEVEL_COUNT];
        for i in 0..LEVEL_COUNT {
            let a = LEVELS[i];
            let mut p = bus(family);
            let tone = Tone::near(RATE, WINDOW, hz, a);
            let c = run(tone, SETTLE, |x| p.process(x));
            let (cg, ct) = (c.gain_db(), c.thd_percent());
            if ct < 0.1 {
                continue;
            }
            let set = |bands: &mut [[f64; 4]; LEVEL_COUNT], db: f64| {
                bands[i][band] = 10f64.powf(db / 20.0);
                bands[i][band + 2] = 1.0;
            };
            let (mut lo, mut hi) = (-6.0, 6.0);
            for _ in 0..12 {
                let mid = 0.5 * (lo + hi);
                set(&mut bands, mid);
                if stage(&bands, hz, a).1 < ct {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let db = 0.5 * (lo + hi);
            if db.abs() > 5.9 {
                set(&mut bands, 0.0);
                continue;
            }
            set(&mut bands, db);
            for _ in 0..4 {
                let sg = stage(&bands, hz, a).0;
                bands[i][band + 2] *= 10f64.powf((cg - sg) / 20.0);
            }
            fitted[i] = true;
        }
        // Unfitted levels take the nearest fitted level's way in.
        for i in 0..LEVEL_COUNT {
            if !fitted[i] {
                let near = (0..LEVEL_COUNT)
                    .filter(|j| fitted[*j])
                    .min_by_key(|j| j.abs_diff(i));
                bands[i][band] = near.map_or(1.0, |j| bands[j][band]);
                bands[i][band + 2] = 1.0;
            }
        }
    }
    eprintln!(
        "{}: hp {hp_hz:.2} Hz q {hp_q}, lp {lp_hz:.0} Hz, bell {:.0} Hz {:+.2} dB, shelf {:.0} Hz {:+.1} dB, table ends {:.3} / {:.3}",
        FAMILIES[family],
        bell.0,
        bell.1,
        shelf.0,
        shelf.1,
        table[0],
        table[TABLE_POINTS - 1]
    );
    for (l, b) in LEVELS.iter().zip(&bands) {
        let db = |g: f64| 20.0 * g.log10();
        eprintln!(
            "  {:+6.1} dBFS: low {:+5.2} / {:+5.2} dB, high {:+5.2} / {:+5.2} dB",
            20.0 * l.log10(),
            db(b[0]),
            db(b[2]),
            db(b[1]),
            db(b[3])
        );
    }
    let f = |v: f64| format!("{v:.6}");
    let name = FAMILIES[family].to_uppercase().replace(' ', "_");
    let blocks: Vec<String> = tables
        .iter()
        .map(|t| {
            let rows: Vec<String> = t
                .chunks(8)
                .map(|c| {
                    format!(
                        "        {},",
                        c.iter()
                            .map(|v| format!("{v:.6}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect();
            format!("    [\n{}\n    ],", rows.join("\n"))
        })
        .collect();
    let band_rows: Vec<String> = bands
        .iter()
        .map(|b| {
            format!(
                "    [{}],",
                b.iter()
                    .map(|v| format!("{v:.6}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    format!(
        "const {name}: [[f32; TABLE_POINTS]; LEVEL_COUNT] = [\n{}\n];\n\nconst {name}_BANDS: [[f32; 4]; LEVEL_COUNT] = [\n{}\n];\n\n@@ConsoleModel {{ name: \"{}\", hp_hz: {}, hp_q: {}, lp_hz: {}, bell: ({}, {}, {}), shelf: ({}, {}), emphasis: ({}, {}), bands: &{name}_BANDS, tables: &{name} }},\n",
        blocks.join("\n"),
        band_rows.join("\n"),
        FAMILIES[family],
        f(hp_hz),
        f(hp_q),
        f(lp_hz),
        f(bell.0),
        f(bell.1),
        f(bell.2),
        f(shelf.0),
        f(shelf.1),
        f(EMPHASIS.0),
        f(EMPHASIS.1),
    )
}

#[test]
#[ignore = "runs the circuits; writes src/console_models.rs when FADERFRAME_BAKE_CONSOLE is set"]
fn bake_console_models() {
    let parts: Vec<String> = std::thread::scope(|s| {
        let jobs: Vec<_> = (0..FAMILIES.len())
            .map(|f| s.spawn(move || bake(f)))
            .collect();
        jobs.into_iter()
            .map(|j| j.join().expect("a family baked"))
            .collect()
    });
    let mut out = String::from(
        "// Generated by `FADERFRAME_BAKE_CONSOLE=1 cargo test -p faderframe-circuit\n// --release --test console_bake -- --ignored`: each family measured on its\n// console bus circuit at its calibration. Do not edit.\n\n",
    );
    let mut models = String::from(
        "/// The channels' console models, in the families' order.\npub const MODELS: [ConsoleModel; 3] = [\n",
    );
    for p in &parts {
        let (table, model) = p.split_once("@@").unwrap_or((p.as_str(), ""));
        out.push_str(table);
        models.push_str("    ");
        models.push_str(model);
    }
    models.push_str("];\n");
    out.push_str(&models);
    if std::env::var_os("FADERFRAME_BAKE_CONSOLE").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/console_models.rs");
        std::fs::write(&path, &out).expect("written");
        eprintln!("wrote {}", path.display());
    } else {
        eprintln!("{out}");
    }
}
