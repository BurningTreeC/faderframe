#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::rig::{Rig, SR, level, silence, thd, tone};

/// The T4's constants: the fitted ones, or (fitting by hand) those
/// `LEVELER_FIT` overrides, e.g. `b=60,vref=100,tau1=0.2,tau2=0.03,
/// rmin=3700,t3r=6`: the Alfrey–Taylor `b`, the reference volts, the
/// lifetimes, the resistance at light 1 and trap `N`'s density (`tNd`),
/// capture (`tNc`) and release (`tNr`).
fn t4() -> T4 {
    let mut t = T4::FITTED;
    let Ok(spec) = std::env::var("LEVELER_FIT") else {
        return t;
    };
    for pair in spec.split(',').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap();
        let v: f64 = v.trim().parse().unwrap();
        match k.trim() {
            "b" => t.panel.alfrey_taylor = v,
            "vref" => t.panel.reference_volts = v,
            "tau1" => t.cell.lifetime = v,
            "tau2" => t.cell.pairs = v,
            "rmin" => t.cell.per_density = 1.0 / v,
            k if k.len() == 3 && k.starts_with('t') => {
                let n = k[1..2].parse::<usize>().unwrap() - 1;
                let trap = &mut t.cell.traps[n];
                match &k[2..] {
                    "d" => trap.density = v,
                    "c" => trap.capture = v,
                    _ => trap.release = v,
                }
            }
            k => panic!("unknown {k}"),
        }
    }
    t.cell.generation = 1.0 / t.cell.lifetime + 1.0 / t.cell.pairs;
    t
}

/// The device on the rig, External off (the rig always feeds a key) unless
/// `set` turns it on.
fn rig(set: &[(u32, f64)]) -> Rig<LevelerProcessor> {
    let mut all = vec![(id::EXTERNAL, 0.0)];
    all.extend_from_slice(set);
    Rig::with(parameters(), TAP_VALUES, &all, |p, t, c| {
        LevelerProcessor::with_t4(p, t, c, t4()).unwrap()
    })
}

fn amp(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// A 1 kHz signal whose level (dBFS, a sine's peak) follows `level(t)`.
fn shaped(level: impl Fn(f64) -> f64) -> impl Fn(usize) -> (f32, f32) {
    move |n| {
        let t = n as f64 / SR;
        let v = (amp(level(t)) * (std::f64::consts::TAU * 1000.0 * t).sin()) as f32;
        (v, v)
    }
}

/// The level (dB, a sine's peak) of the 1 kHz component in each
/// millisecond (one period: the circuit's slower transients, which a step
/// in level sets off, fall out of it).
fn envelope(x: &[f32]) -> Vec<f64> {
    x.as_chunks::<48>()
        .0
        .iter()
        .map(|c| {
            let (mut re, mut im) = (0.0, 0.0);
            for (k, v) in c.iter().enumerate() {
                let w = std::f64::consts::TAU * k as f64 / 48.0;
                re += f64::from(*v) * w.cos();
                im += f64::from(*v) * w.sin();
            }
            20.0 * (2.0 * re.hypot(im) / 48.0).max(1e-15).log10()
        })
        .collect()
}

/// The unit's gain (dB) for a quiet 1 kHz tone, no reduction.
fn quiet_gain(set: &[(u32, f64)]) -> f64 {
    let mut r = rig(set);
    let (l, _) = r.run(0.6, tone(1000.0, amp(-60.0)), silence);
    level(&l[l.len() / 2..], 1000.0) + 60.0
}

/// Settled at `input` dBFS: (output dBFS, the meter's reduction, the
/// panel's light).
fn settled(set: &[(u32, f64)], input: f64, seconds: f64) -> (f64, f64, f64) {
    let mut r = rig(set);
    let (l, _) = r.run(seconds, tone(1000.0, amp(input)), silence);
    (
        level(&l[l.len() * 3 / 4..], 1000.0),
        f64::from(r.tap.value(value::GR)),
        f64::from(r.tap.value(value::LIGHT)),
    )
}

/// The reduction (dB) each millisecond: the output's level against the
/// input's, less the unit's quiet gain.
fn reductions(set: &[(u32, f64)], seconds: f64, level: impl Fn(f64) -> f64 + Copy) -> Vec<f64> {
    let gain = quiet_gain(set);
    let mut r = rig(set);
    let (l, _) = r.run(seconds, shaped(level), silence);
    envelope(&l)
        .iter()
        .enumerate()
        .map(|(k, out)| level((k as f64 + 0.5) / 1000.0) + gain - out)
        .collect()
}

/// After the reduction `gr` (per ms) from `from` (ms): when it first falls
/// below `share` of its value at `from` (ms after `from`).
fn release_to(gr: &[f64], from: usize, share: f64) -> f64 {
    let start = gr[from];
    gr[from..]
        .iter()
        .position(|g| *g <= share * start)
        .map_or(f64::INFINITY, |k| k as f64)
}

/// The figures the model is fitted to (run by hand:
/// `cargo test -p faderframe-plugin-host --release leveler::tests::fit -- --ignored --nocapture`).
#[test]
#[ignore]
fn fit() {
    for (name, mode, peak) in [
        ("Compress", 0.0, 50.0),
        ("Limit", 1.0, 50.0),
        ("Compress", 0.0, 85.0),
        ("Limit", 1.0, 85.0),
    ] {
        let set = [
            (id::MODE, mode),
            (id::PEAK_REDUCTION, peak),
            (id::GAIN, 50.0),
        ];
        let g = quiet_gain(&set);
        print!("{name} at {peak}: quiet gain {g:+.1} dB; in → GR:");
        let mut last = None;
        for input in [-40.0, -30.0, -24.0, -18.0, -12.0, -6.0, 0.0] {
            let (out, gr, light) = settled(&set, input, 2.0);
            let slope = last.map_or(0.0, |(i, o): (f64, f64)| (input - i) / (out - o).max(1e-3));
            print!(
                " {input:.0}→{:.1} [{gr:.1} dB, light {light:.3}] ({slope:.1}:1)",
                input + g - out
            );
            last = Some((input, out));
        }
        println!();
    }
    // Attack: −18 dBFS for half a second, then −6 dBFS (Moore's burst).
    let set = [(id::PEAK_REDUCTION, 50.0)];
    let before = reductions(&set, 0.5, |_| -18.0)[490];
    let gr: Vec<f64> = reductions(&set, 1.0, |t| if t < 0.5 { -18.0 } else { -6.0 })
        .iter()
        .map(|g| g - before)
        .collect();
    let end = gr[950];
    // From 3 ms after the step (the envelope's window and the latency).
    let reach = |share: f64| {
        3 + gr[503..]
            .iter()
            .position(|g| *g >= share * end)
            .unwrap_or(999)
    };
    println!(
        "attack to {end:.1} dB: 50 % in {} ms, 63 % in {} ms, 90 % in {} ms",
        reach(0.5),
        reach(0.63),
        reach(0.9)
    );
    // Release after a short and a long burst.
    for (hold, label) in [(0.3, "0.3 s"), (2.0, "2 s"), (8.0, "8 s")] {
        for loud in [-12.0, 0.0] {
            let gr = reductions(&set, hold + 8.0, |t| if t < hold { loud } else { -50.0 });
            // A few milliseconds on: the envelope's window and the
            // oversampler's latency past the drop.
            let from = (hold * 1000.0) as usize + 3;
            println!(
                "release after {label} at {loud} dBFS ({:.1} dB): 50 % {:.0} ms, 90 % {:.0}, 97 % {:.0}, under 0.5 dB {:.0} ms",
                gr[from],
                release_to(&gr, from, 0.5),
                release_to(&gr, from, 0.1),
                release_to(&gr, from, 0.03),
                gr[from..]
                    .iter()
                    .position(|g| *g < 0.5)
                    .map_or(f64::INFINITY, |k| k as f64),
            );
        }
    }
}

#[test]
#[ignore]
fn trace() {
    let k = Controls {
        r37: r37_ohms(0.0),
        limit: false,
    };
    let mut s = Side::new(SR, k, t4()).unwrap();
    let (mut tap_most, mut panel_most, mut light_most) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..24_000 {
        let x = amp(-6.0) * (std::f64::consts::TAU * 1000.0 * i as f64 / SR).sin();
        let (_, tap) = s.audio(x * input_volts(), wiper(50.0));
        let volts = s.sidechain.process(wiper(50.0) * tap);
        let light = s.panel.step(volts);
        s.cell.step(light);
        if i > 12_000 {
            tap_most = tap_most.max(tap.abs());
            panel_most = panel_most.max(volts.abs());
            light_most = light_most.max(light);
        }
    }
    println!(
        "node b {tap_most:.4} V, panel {panel_most:.2} V, light {light_most:.5}, cell {:.0} ohm, n {:.5}",
        s.cell.ohms(),
        s.cell.density()
    );
}

/// The output (dB) at `input` (dBFS), settled, for each of `inputs`.
fn curve(set: &[(u32, f64)], inputs: &[f64]) -> Vec<f64> {
    inputs.iter().map(|i| settled(set, *i, 1.5).0).collect()
}

/// The static curves: a wire below the knee; above it Compress about 3:1
/// to 4:1 (UREI's 11-66 addendum: "a slope of approximately 3:1"; Mix,
/// 11/2000: "4:1"), Limit steeper and steeper as the reduction deepens
/// (R7's share of the input reaches the sidechain however low the cell
/// pulls node `c`) until the output barely rises (the 3-66 manual: "the
/// curve becomes horizontal").
#[test]
fn the_static_curves_compress_and_limit() {
    let inputs = [-36.0, -30.0, -12.0, -6.0, 0.0];
    let compress = curve(&[(id::PEAK_REDUCTION, 50.0)], &inputs);
    let limit = curve(&[(id::PEAK_REDUCTION, 50.0), (id::MODE, 1.0)], &inputs);
    for out in [&compress, &limit] {
        // Below the knee: 6 dB in, 6 dB out.
        assert!((out[1] - out[0] - 6.0).abs() < 0.2, "{out:?}");
    }
    let ratio = |out: &[f64], k: usize| 6.0 / (out[k + 1] - out[k]);
    let c = ratio(&compress, 3);
    assert!((2.5..4.2).contains(&c), "Compress {c:.2}:1");
    let (l_mid, l_deep) = (ratio(&limit, 2), ratio(&limit, 3));
    assert!(l_mid > ratio(&compress, 2), "Limit {l_mid:.2}:1");
    assert!(
        l_deep > 2.0 * c,
        "Limit {l_deep:.2}:1 against Compress {c:.2}:1"
    );
    assert!(
        l_deep > l_mid,
        "deeper, steeper: {l_mid:.2}:1, then {l_deep:.2}:1"
    );
}

/// Inserted as it comes, a −18 dBFS signal is levelled by "4 to 8 DB" (the
/// 3-66 manual's advice) and comes out at about its own level.
#[test]
fn the_defaults_level_a_normal_signal_and_keep_its_level() {
    let (out, gr, _) = settled(&[], -18.0, 1.5);
    assert!((3.0..=8.0).contains(&gr), "{gr:.2} dB");
    assert!((out + 18.0).abs() < 2.0, "{out:.2} dBFS out");
}

/// Peak Reduction is the threshold: more of it, more reduction at the same
/// level; none, none at all.
#[test]
fn peak_reduction_sets_how_much() {
    let gr = |peak: f64| settled(&[(id::PEAK_REDUCTION, peak)], -12.0, 1.0).1;
    let (none, some, more) = (gr(0.0), gr(50.0), gr(80.0));
    assert!(none < 0.05, "{none}");
    assert!(some > 2.0 && more > some + 4.0, "{some:.2} {more:.2}");
}

/// The attack, on a 12 dB step (−18 to −6 dBFS, Moore's burst): about
/// 10 ms to most of the way (UA: "10 milliseconds"), settled within the
/// 33–81 ms Moore measured on six units.
#[test]
fn the_attack_takes_milliseconds() {
    let set = [(id::PEAK_REDUCTION, 50.0)];
    let before = reductions(&set, 0.5, |_| -18.0)[490];
    let gr: Vec<f64> = reductions(&set, 1.0, |t| if t < 0.5 { -18.0 } else { -6.0 })
        .iter()
        .map(|g| g - before)
        .collect();
    let end = gr[950];
    assert!(end > 5.0, "{end:.2} dB more");
    let reach = |share: f64| 3 + gr[503..].iter().position(|g| *g >= share * end).unwrap();
    let (most, nearly) = (reach(0.63), reach(0.9));
    assert!((2..=15).contains(&most), "63 % in {most} ms");
    assert!(nearly <= 40, "90 % in {nearly} ms");
    let settled = 503
        + gr[503..]
            .iter()
            .position(|g| (end - g).abs() < 0.5)
            .unwrap()
        - 500;
    assert!(settled <= 120, "within 0.5 dB in {settled} ms");
}

/// The release, after `hold` seconds at `loud` dBFS: (the reduction as it
/// let go, ms to half of it, ms to under 0.5 dB).
fn release(hold: f64, loud: f64) -> (f64, f64, f64) {
    let set = [(id::PEAK_REDUCTION, 50.0)];
    let gr = reductions(&set, hold + 7.0, |t| if t < hold { loud } else { -50.0 });
    let from = (hold * 1000.0) as usize + 3;
    let gone = gr[from..]
        .iter()
        .position(|g| *g < 0.5)
        .map_or(f64::INFINITY, |k| k as f64);
    (gr[from], release_to(&gr, from, 0.5), gone)
}

/// "Approximately 0.06 seconds for 50% release, 0.5 to 5 seconds for
/// complete release depending upon amount of previous reduction": the fast
/// stage alike after any burst; the slow one short after a brief, moderate
/// one and several times longer after seconds of hard levelling.
#[test]
fn the_release_remembers_what_came_before() {
    let (gr_short, half_short, gone_short) = release(0.3, -12.0);
    let (gr_long, half_long, gone_long) = release(6.0, 0.0);
    assert!(
        gr_short > 3.0 && gr_long > gr_short + 5.0,
        "{gr_short} {gr_long}"
    );
    for half in [half_short, half_long] {
        assert!((40.0..=90.0).contains(&half), "50 % in {half} ms");
    }
    assert!(
        (300.0..=1500.0).contains(&gone_short),
        "short: {gone_short} ms"
    );
    assert!(
        (1500.0..=6000.0).contains(&gone_long),
        "long: {gone_long} ms"
    );
    assert!(
        gone_long > 3.0 * gone_short,
        "{gone_short} ms, {gone_long} ms"
    );
    // Held as long but not as hard: in between.
    let (_, _, gone_moderate) = release(6.0, -12.0);
    assert!(
        gone_moderate > gone_short && gone_moderate < gone_long,
        "{gone_moderate} ms"
    );
}

/// Levels: the unit's 40 dB (spec ±1) less the input pad at full Gain, and
/// about the pad's worth of make-up at half (unity with 4–8 dB of
/// levelling, as the 3-66 manual sets it up).
#[test]
fn the_gain_spans_the_units_forty_db() {
    let full = quiet_gain(&[(id::PEAK_REDUCTION, 0.0), (id::GAIN, 100.0)]);
    let half = quiet_gain(&[(id::PEAK_REDUCTION, 0.0), (id::GAIN, 50.0)]);
    let none = quiet_gain(&[(id::PEAK_REDUCTION, 0.0), (id::GAIN, 0.0)]);
    assert!((full + INPUT_PAD_DB - 40.0).abs() < 1.5, "{full:.2} dB");
    assert!((half - full + 20.0).abs() < 1.0, "half: {half:.2} dB");
    assert!(none < full - 60.0, "closed: {none:.2} dB");
}

/// THD of the make-up amplifier at the specification's levels, no
/// reduction: "less than 0.35% total harmonics at +10 DBM, and less than
/// 0.75% … at +16 DBM" (+10 dBm out is −12 dBFS here); the valves still
/// there (UA: 0.25 % typical).
#[test]
fn the_distortion_meets_the_specification() {
    let set = [(id::PEAK_REDUCTION, 0.0), (id::GAIN, 100.0)];
    let gain = quiet_gain(&set);
    for (out, most, least) in [(-12.0, 0.35, 0.05), (-6.0, 0.75, 0.1)] {
        let mut r = rig(&set);
        let (l, _) = r.run(0.5, tone(1000.0, amp(out - gain)), silence);
        let tail = &l[l.len() - 48 * 200..];
        let got = thd(tail, 1000.0);
        assert!((level(tail, 1000.0) - out).abs() < 0.5);
        assert!(got < most && got > least, "{out} dBFS out: THD {got:.3} %");
    }
}

/// The latency is the oversampler's, and the processed path is in time
/// with it: at Mix 0 the dry signal, exactly, that many samples late; at
/// Mix 1 a click comes out (through its transformers) at the same place.
#[test]
fn the_latency_is_reported_and_the_dry_path_matches() {
    let d = latency(&ParamValues::new(parameters()), SR) as usize;
    assert!(d > 0 && d < 64, "{d}");
    let mut r = rig(&[(id::MIX, 0.0), (id::PEAK_REDUCTION, 0.0)]);
    let input = tone(440.0, 0.3);
    let (l, _) = r.run(0.1, &input, silence);
    for (i, y) in l.iter().enumerate().skip(d) {
        assert!(
            (f64::from(*y) - f64::from(input(i - d).0)).abs() < 1e-6,
            "{i}"
        );
    }
    let mut r = rig(&[(id::PEAK_REDUCTION, 0.0), (id::GAIN, 30.0)]);
    let click = |n: usize| {
        if n == 1000 {
            (0.5f32, 0.5f32)
        } else {
            (0.0, 0.0)
        }
    };
    let (l, _) = r.run(0.1, click, silence);
    let peak = (0..l.len())
        .max_by(|a, b| l[*a].abs().total_cmp(&l[*b].abs()))
        .unwrap();
    assert!((peak as i64 - 1000 - d as i64).abs() <= 2, "peak at {peak}");
}

/// Linked, a loud left side reduces the quiet right one too.
#[test]
fn linked_sides_share_the_light() {
    let loud_left = |n: usize| {
        let v = (std::f64::consts::TAU * 1000.0 * n as f64 / SR).sin();
        ((amp(-6.0) * v) as f32, (amp(-40.0) * v) as f32)
    };
    let right = |link: f64| {
        let mut r = rig(&[(id::PEAK_REDUCTION, 60.0), (id::LINK, link)]);
        let (_, rr) = r.run(1.0, loud_left, silence);
        level(&rr[rr.len() / 2..], 1000.0)
    };
    let (free, linked) = (right(0.0), right(1.0));
    assert!(
        linked < free - 4.0,
        "unlinked {free:.2}, linked {linked:.2}"
    );
}

/// An external key drives the panel instead of the signal: a quiet signal
/// is reduced by a loud key, and a loud signal not at all by a silent one.
#[test]
fn an_external_key_drives_the_panel() {
    let mut r = rig(&[(id::EXTERNAL, 1.0), (id::PEAK_REDUCTION, 60.0)]);
    r.run(1.0, tone(1000.0, amp(-30.0)), tone(200.0, amp(-6.0)));
    let keyed = f64::from(r.tap.value(value::GR));
    let mut r = rig(&[(id::EXTERNAL, 1.0), (id::PEAK_REDUCTION, 60.0)]);
    r.run(1.0, tone(1000.0, amp(-6.0)), silence);
    let unkeyed = f64::from(r.tap.value(value::GR));
    assert!(
        keyed > 3.0 && unkeyed < 0.05,
        "keyed {keyed:.2}, unkeyed {unkeyed:.2}"
    );
}

/// R37 turned up levels the highs harder than the lows (the 3-66 manual:
/// "the leveling will be greater on the high frequencies"); at its "flat"
/// end the two are within a few dB of each other.
#[test]
fn r37_levels_the_highs_harder() {
    let gr = |hz: f64, emphasis: f64| {
        let set = [(id::PEAK_REDUCTION, 55.0), (id::EMPHASIS, emphasis)];
        let mut r = rig(&set);
        r.run(1.0, tone(hz, amp(-10.0)), silence);
        f64::from(r.tap.value(value::GR))
    };
    let flat = gr(8_000.0, 0.0) - gr(300.0, 0.0);
    let lifted = gr(8_000.0, 1.0) - gr(300.0, 1.0);
    assert!(flat.abs() < 4.0, "flat: {flat:+.2} dB more on the highs");
    assert!(
        lifted > flat + 3.0,
        "R37 up: {lifted:+.2} dB, flat {flat:+.2} dB"
    );
}

/// While a stereo pair is still mono one side is solved; when it parts the
/// second wakes from the first's exact state: unlinked, the left side's
/// output is the same to the bit whatever the right did before.
#[test]
fn a_mono_pair_wakes_its_second_side_exactly() {
    let set = [(id::PEAK_REDUCTION, 60.0), (id::LINK, 0.0)];
    let mono_then_stereo = |n: usize| {
        let t = n as f64 / SR;
        let l = (amp(-8.0) * (std::f64::consts::TAU * 330.0 * t).sin()) as f32;
        let r = if n < 12_000 {
            l
        } else {
            (amp(-14.0) * (std::f64::consts::TAU * 500.0 * t).sin()) as f32
        };
        (l, r)
    };
    let stereo = |n: usize| {
        let (l, _) = mono_then_stereo(n);
        let r = (amp(-20.0) * (std::f64::consts::TAU * 700.0 * n as f64 / SR).sin()) as f32;
        (l, r)
    };
    let mut a = rig(&set);
    let (la, ra) = a.run(0.5, mono_then_stereo, silence);
    let mut b = rig(&set);
    let (lb, _) = b.run(0.5, stereo, silence);
    assert_eq!(la, lb, "the left side never depended on the right");
    // And the right side came out of its sleep without a step.
    let jump = ra
        .windows(2)
        .skip(11_900)
        .take(200)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(jump < 0.1, "{jump}");
}

/// A reset forgets the cell's memory: the next note starts unreduced.
#[test]
fn a_reset_forgets_the_light() {
    let set = [(id::PEAK_REDUCTION, 70.0)];
    let mut r = rig(&set);
    r.run(2.0, tone(1000.0, amp(-6.0)), silence);
    assert!(r.tap.value(value::GR) > 5.0);
    r.p.reset();
    r.run(0.05, tone(1000.0, amp(-60.0)), silence);
    assert!(r.tap.value(value::GR) < 0.05);
}

/// What the device costs: a stereo second of a levelled drum-like signal
/// at 48 kHz, in milliseconds of one core (run by hand in release:
/// `cargo test -p faderframe-plugin-host --release leveler::tests::cost -- --ignored --nocapture`).
#[test]
#[ignore]
fn cost() {
    for (label, stereo) in [("mono pair", false), ("stereo", true)] {
        let mut r = rig(&[(id::PEAK_REDUCTION, 60.0), (id::LINK, 1.0)]);
        let signal = move |n: usize| {
            let t = n as f64 / SR;
            let env = (-(t % 0.25) * 12.0).exp();
            let l = 0.4 * env * (std::f64::consts::TAU * 110.0 * t).sin()
                + 0.1 * (std::f64::consts::TAU * 2200.0 * t).sin();
            let r = if stereo {
                0.4 * env * (std::f64::consts::TAU * 165.0 * t).sin()
            } else {
                l
            };
            (l as f32, r as f32)
        };
        r.run(0.5, signal, silence);
        let start = std::time::Instant::now();
        let cpu = faderframe_realtime::usage::thread_cpu_ns().unwrap_or(0);
        let seconds = 4.0;
        r.run(seconds, signal, silence);
        let wall = start.elapsed().as_secs_f64() * 1000.0 / seconds;
        let cpu =
            (faderframe_realtime::usage::thread_cpu_ns().unwrap_or(0) - cpu) as f64 / 1e6 / seconds;
        println!(
            "{label}: {cpu:.1} ms of CPU per second of audio ({:.1} % of a core; wall {wall:.1} ms)",
            cpu / 10.0
        );
    }
}

#[test]
fn the_meter_reads_the_cells_reduction() {
    assert_eq!(reduction_db(circuit::CELL_DARK, false), 0.0);
    // The cell at the attenuator's own resistance: half the voltage.
    let rs = cell_source_ohms(false);
    assert!((reduction_db(rs, false) - 6.02).abs() < 0.05);
    assert!(cell_source_ohms(true) > cell_source_ohms(false));
}

/// The external key's scale: a key as loud as the input puts the same
/// voltage on node `b` as the input does with the cell dark.
#[test]
fn the_external_key_is_scaled_as_the_input() {
    let ic = circuit::input(SOURCE_OHMS).unwrap();
    let b = ic.unknown_named("b").unwrap();
    let rate = SR;
    let mut sim = Simulation::new(ic, rate);
    sim.set_value(circuit::R7_SLOT, circuit::SWITCH_OHMS);
    sim.set_realtime_value(circuit::CELL, circuit::CELL_DARK);
    sim.reset();
    let mut most = 0.0f64;
    for i in 0..(rate as usize / 2) {
        sim.process((std::f64::consts::TAU * 1000.0 * i as f64 / rate).sin());
        if i > rate as usize / 4 {
            most = most.max(sim.voltage_at(b).abs());
        }
    }
    assert!(
        (most / KEY_SHARE - 1.0).abs() < 0.02,
        "node b / EMF = {most:.4}"
    );
}
