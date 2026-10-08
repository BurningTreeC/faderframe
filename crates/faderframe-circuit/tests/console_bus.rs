//! Console mix buses on their circuits: unity and the right polarity at
//! small levels, clean at the nominal level, headroom where each family's
//! amplifiers run out (the American's 2520 into its 2503, the British 4K's
//! 5534s), and the American's iron saturating first in the lows. The valve
//! boosters (Tube 610, British 47, German 76) are held to valve figures:
//! under half a percent at the nominal level, within 2 dB to 20 kHz (the
//! German 76 with its 40 Hz high-pass and 15 kHz low-pass).

/// The valve consoles' buses.
fn valve(family: usize) -> bool {
    family >= 3
}

use faderframe_circuit::dsp::measure::{run, Tone};
use faderframe_circuit::preamp::{Preamp, CONSOLE_BUSES, MODELS};

const RATE: f64 = 48_000.0;

fn bus(family: usize, drive: f64) -> Preamp {
    Preamp::new(MODELS + family, RATE, drive, 0.0).expect("the bus builds")
}

fn measure(family: usize, hz: f64, dbfs: f64) -> (f64, f64) {
    let mut p = bus(family, 0.0);
    let tone = Tone::near(RATE, 9600, hz, 10f64.powf(dbfs / 20.0));
    let m = run(tone, 9600, |x| p.process(x));
    (m.gain_db(), m.thd_percent())
}

#[test]
fn buses_pass_at_unity_and_in_phase() {
    for f in 0..CONSOLE_BUSES {
        let mut p = bus(f, 0.0);
        let tone = Tone::near(RATE, 9600, 1000.0, 0.01);
        let m = run(tone, 9600, |x| p.process(x));
        let g = m.fundamental();
        assert!(m.gain_db().abs() < 0.2, "family {f}: {:.2} dB", m.gain_db());
        // In phase with the input (a sine's bin is (0, −A)): the oversampler's
        // latency turns it, but less than a quarter turn at 1 kHz.
        assert!(g.im < 0.0, "family {f}: {g:?}");
    }
}

#[test]
fn nominal_is_clean_and_full_scale_is_near_the_rails() {
    for f in 0..CONSOLE_BUSES {
        let (_, nominal) = measure(f, 1000.0, -18.0);
        let limit = if valve(f) { 0.5 } else { 0.1 };
        assert!(nominal < limit, "family {f}: {nominal:.3} % at +4 dBu");
        let (_, hot) = measure(f, 1000.0, 6.0);
        assert!(hot > nominal, "family {f}: more at +28 dBu ({hot:.2} %)");
        eprintln!("family {f}: {nominal:.4} % at -18 dBFS, {hot:.2} % at +6 dBFS");
    }
    // The British 4K's 5534s swing ±16 V, about +23 dBu: clean at full scale
    // (+22 dBu), clipping by +6 dBFS.
    let (_, at_fs) = measure(1, 1000.0, 0.0);
    let (_, over) = measure(1, 1000.0, 6.0);
    assert!(
        at_fs < 0.5 && over > 3.0,
        "British 4K: {at_fs:.2} % / {over:.2} %"
    );
}

#[test]
fn the_american_iron_runs_out_in_the_lows_first() {
    // Hot: the 2503's core saturates at 30 Hz well before the 2520 clips.
    let (_, low) = measure(0, 30.0, 0.0);
    let (_, mid) = measure(0, 1000.0, 0.0);
    assert!(low > 2.0 * mid, "30 Hz {low:.2} %, 1 kHz {mid:.2} %");
    // No iron on the British 4K: the lows are no worse.
    let (_, low) = measure(1, 30.0, 0.0);
    let (_, mid) = measure(1, 1000.0, 0.0);
    assert!(low < 2.0 * mid + 0.02, "30 Hz {low:.2} %, 1 kHz {mid:.2} %");
}

#[test]
fn the_response_is_flat_through_the_band() {
    for f in 0..CONSOLE_BUSES {
        let (reference, _) = measure(f, 1000.0, -30.0);
        for hz in [20.0, 100.0, 10_000.0, 20_000.0] {
            let (g, _) = measure(f, hz, -30.0);
            // The V76's 40 Hz high-pass, by design (the broadcast desks'
            // low end): down at 20 Hz, flat from 100 Hz.
            if f == 5 && hz == 20.0 {
                let d = g - reference;
                assert!((-14.0..-6.0).contains(&d), "German 76: {d:+.2} dB at 20 Hz");
                continue;
            }
            // And its 15 kHz low-pass (flat to 15 kHz, 20 dB down at 40).
            if f == 5 && hz == 20_000.0 {
                let d = g - reference;
                assert!((-4.0..-1.0).contains(&d), "German 76: {d:+.2} dB at 20 kHz");
                continue;
            }
            let limit = if valve(f) { 2.0 } else { 1.0 };
            assert!(
                (g - reference).abs() < limit,
                "family {f}: {:+.2} dB at {hz} Hz",
                g - reference
            );
        }
    }
}
