#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::rig::{BLOCK, Rig, SR, bin_db, peak_db, silence, tone};

fn rig(set: &[(u32, f64)]) -> Rig<TapeProcessor> {
    Rig::with(parameters(), TAP_VALUES, set, TapeProcessor::new)
}

/// The settings with the transport still (for clean measurements).
fn still(set: &[(u32, f64)]) -> Vec<(u32, f64)> {
    let mut v = vec![(id::FLUTTER, 0.0)];
    v.extend_from_slice(set);
    v
}

fn amp(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// A sine on the left track only (no crosstalk adding in phase).
fn left(f: f64, a: f64) -> impl Fn(usize) -> (f32, f32) {
    move |n| {
        (
            (a * (std::f64::consts::TAU * f * n as f64 / SR).sin()) as f32,
            0.0,
        )
    }
}

/// The level (dB re the operating level's sine) and the harmonics (each's
/// amplitude over the fundamental) of the last whole periods of `x` at `f`
/// (`f` must divide the rate).
fn analyse(x: &[f32], f: f64) -> (f64, [f64; 8]) {
    let period = (SR / f).round() as usize;
    let periods = (x.len() / 2) / period;
    let tail = &x[x.len() - periods * period..];
    let n = tail.len() as f64;
    let bin = |h: f64| {
        let w = std::f64::consts::TAU * f * h / SR;
        let (mut re, mut im) = (0.0, 0.0);
        for (k, v) in tail.iter().enumerate() {
            re += f64::from(*v) * (w * k as f64).cos();
            im += f64::from(*v) * (w * k as f64).sin();
        }
        2.0 * re.hypot(im) / n
    };
    let fund = bin(1.0);
    let mut h = [0.0; 8];
    for (i, h) in h.iter_mut().enumerate().skip(2) {
        *h = bin(i as f64) / fund.max(1e-30);
    }
    (20.0 * (fund / gain(REFERENCE_DBFS)).log10(), h)
}

/// A 1 kHz tone `over` dB over the operating level: the output level (dB
/// re the operating level) and its third harmonic (fraction).
fn third(set: &[(u32, f64)], over: f64) -> (f64, f64) {
    let mut r = rig(&still(set));
    let (l, _) = r.run(0.3, left(1000.0, amp(REFERENCE_DBFS + over)), silence);
    let (db, h) = analyse(&l, 1000.0);
    (db, h[3])
}

/// The output level (dB re the operating level) at which the third
/// harmonic reaches 3 %.
fn mol(set: &[(u32, f64)]) -> f64 {
    let (mut lo, mut hi) = (-6.0, 30.0);
    for _ in 0..14 {
        let mid = 0.5 * (lo + hi);
        if third(set, mid).1 < 0.03 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    third(set, 0.5 * (lo + hi)).0
}

#[test]
fn it_is_level_neutral_at_its_operating_level() {
    for tape in 0..3 {
        for level in 0..5 {
            for speed in [0, 2, 3] {
                let set = [
                    (id::TAPE, tape as f64),
                    (id::LEVEL, level as f64),
                    (id::SPEED, speed as f64),
                ];
                let (db, _) = third(&set, 0.0);
                assert!(
                    db.abs() < 0.05,
                    "tape {tape} level {level} speed {speed}: {db:+.3} dB at 0 VU"
                );
            }
        }
    }
    // Auto Output takes the input trim back off: pushed 9 dB the level
    // stays (less what the tape compresses); without it, it rises.
    let (auto, _) = third(&[(id::INPUT, 9.0)], 0.0);
    let (plain, _) = third(&[(id::INPUT, 9.0), (id::AUTO_OUTPUT, 0.0)], 0.0);
    assert!((-0.6..=0.05).contains(&auto), "auto: {auto:+.2}");
    assert!((plain - 9.0 - auto).abs() < 0.05, "plain {plain:+.2}");
}

/// The standard tape's distortion against its data sheet: 0.1 % at its
/// 260 nWb/m reference, 3 % at 1077 nWb/m (+12.7 dB over 250 nWb/m); the
/// high-output tape takes 3 dB more; the vintage tape 0.75 % at 185.
#[test]
fn the_distortion_follows_the_data_sheets() {
    // Standard, aligned at 250 nWb/m.
    let (_, hd) = third(&[(id::LEVEL, 1.0)], 0.0);
    assert!(
        (0.0007..0.0015).contains(&hd),
        "standard at 250: {:.3} %",
        hd * 100.0
    );
    let standard = mol(&[(id::LEVEL, 1.0)]);
    let want = 20.0 * (1077.0f64 / 250.0).log10();
    assert!(
        (standard - want).abs() < 0.6,
        "standard: 3 % at {standard:+.2} dB re 250 nWb/m, the sheet {want:+.2}"
    );
    // High output, at the same level: 3 dB more headroom.
    let high = mol(&[(id::LEVEL, 1.0), (id::TAPE, 1.0)]);
    assert!(
        (2.0..4.5).contains(&(high - standard)),
        "high output {high:+.2} vs {standard:+.2}"
    );
    // Vintage at 185 nWb/m.
    let (_, hd) = third(&[(id::LEVEL, 0.0), (id::TAPE, 2.0)], 0.0);
    assert!(
        (0.006..0.0095).contains(&hd),
        "vintage at 185: {:.2} %",
        hd * 100.0
    );
    // The distortion grows faster than the square law near the top (the
    // sheared curve's knee): 0.1 % → 3 % within 14 dB.
    let (_, at_ref) = third(&[(id::LEVEL, 1.0)], 0.0);
    let (_, top) = third(&[(id::LEVEL, 1.0)], 13.5);
    assert!(at_ref < 0.0015 && top > 0.025, "{at_ref} {top}");
    // Odd harmonics: the tape is symmetric.
    let mut r = rig(&still(&[(id::LEVEL, 1.0)]));
    let (l, _) = r.run(0.3, left(1000.0, amp(REFERENCE_DBFS + 10.0)), silence);
    let (_, h) = analyse(&l, 1000.0);
    assert!(h[2] < 0.05 * h[3], "even {:.4} odd {:.4}", h[2], h[3]);
}

/// The small-signal response at each speed, against the machine's
/// specifications (Studer A80: 30 Hz–18 kHz at 15 ips, 30 Hz–15 kHz at 7.5;
/// A810: 40 Hz–22 kHz ±2 dB at 30 ips) and against what the editor draws,
/// with the head bump where the speed puts it.
#[test]
fn the_response_at_each_speed_has_its_bump_and_its_top() {
    let measure = |speed: usize, f: f64| {
        let mut r = rig(&still(&[(id::SPEED, speed as f64)]));
        let (l, _) = r.run(0.6, left(f, amp(REFERENCE_DBFS - 20.0)), silence);
        bin_db(&l, f) - (REFERENCE_DBFS - 20.0)
    };
    let within = |speed: usize, lo: f64, hi: f64, tol: f64, top: f64| {
        for f in [
            30.0, 40.0, 60.0, 100.0, 300.0, 1000.0, 3000.0, 8000.0, 12_000.0, 15_000.0, 18_000.0,
            20_000.0,
        ] {
            if f < lo || f > hi {
                continue;
            }
            let db = measure(speed, f);
            let tol = if f >= 12_000.0 { top } else { tol };
            assert!(
                db.abs() <= tol,
                "{} ips at {f} Hz: {db:+.2} dB",
                SPEEDS[speed]
            );
            let at = Response {
                speed,
                iec: false,
                tape: 0,
                format: 0,
                bias: 0.0,
                factor: 2,
            };
            let drawn = response_db(&at, f);
            assert!(
                (db - drawn).abs() < 0.15,
                "{} Hz: {db:+.2} measured, {drawn:+.2} drawn",
                f
            );
            // The oversampling leaves the band alone.
            let deeper = response_db(&Response { factor: 8, ..at }, f);
            assert!(
                (deeper - drawn).abs() < 0.25,
                "{f} Hz: 8× {deeper:+.2}, 2× {drawn:+.2}"
            );
        }
    };
    within(2, 30.0, 18_000.0, 2.0, 2.0);
    within(1, 30.0, 15_000.0, 2.0, 3.5);
    println!(
        "top: 7.5 ips 15 kHz {:+.2} dB, 15 ips 18 kHz {:+.2}, 20 kHz {:+.2}, 30 ips 20 kHz {:+.2}",
        measure(1, 15_000.0),
        measure(2, 18_000.0),
        measure(2, 20_000.0),
        measure(3, 20_000.0)
    );
    within(3, 40.0, 20_000.0, 2.0, 2.0);
    // The bump: up 1–3 dB, an octave higher at twice the speed.
    let bump = |speed: usize| {
        let fb = bump_frequency(speed, 0);
        (measure(speed, fb), fb)
    };
    let (b15, f15) = bump(2);
    let (b30, f30) = bump(3);
    assert!(
        (1.0..3.0).contains(&b15) && (1.0..3.0).contains(&b30),
        "{b15} {b30}"
    );
    assert!(
        (f30 / f15 - 2.0).abs() < 1e-9 && (45.0..65.0).contains(&f15),
        "{f15}"
    );
    // Slow speeds lose the top: 3.75 ips is down by 12 kHz.
    assert!(measure(0, 12_000.0) < -3.5);
    assert!(measure(0, 3000.0).abs() < 2.0);
}

/// Slow tape saturates its treble first: the record equaliser drives the
/// highs harder to make up the tape's losses, the more so the slower it
/// runs — and IEC's 35 µs at 15 ips asks more than NAB's 50 µs.
#[test]
fn slow_tape_saturates_the_treble_first() {
    // How much 10 kHz, pushed 8 dB over the operating level, comes out
    // under what the response gives it at a low level.
    let squash = |speed: usize, iec: bool| {
        let set = still(&[
            (id::SPEED, speed as f64),
            (id::EQ, f64::from(u8::from(iec))),
        ]);
        let mut quiet = rig(&set);
        let (q, _) = quiet.run(0.3, left(10_000.0, amp(REFERENCE_DBFS - 20.0)), silence);
        let mut loud = rig(&set);
        let (l, _) = loud.run(0.3, left(10_000.0, amp(REFERENCE_DBFS + 8.0)), silence);
        (bin_db(&q, 10_000.0) + 28.0) - bin_db(&l, 10_000.0)
    };
    let (s7, s15, s30) = (squash(1, false), squash(2, false), squash(3, false));
    println!("10 kHz at +8: 7.5 ips {s7:.2} dB, 15 ips {s15:.2}, 30 ips {s30:.2}");
    assert!(
        s7 > s15 + 1.0 && s7 > 3.0,
        "7.5 ips {s7:.2}, 15 ips {s15:.2}"
    );
    assert!(s30 < 3.0 && s15 < 4.0, "{s15} {s30}");
    let iec = squash(2, true);
    assert!(iec > s15, "IEC {iec:.2} vs NAB {s15:.2}");
}

/// The bias as on the machine: distortion least near the alignment and
/// rising fast under it; the 10 kHz output peaking under the alignment
/// (alignment practice over-biases past the 10 kHz peak by 2–3 dB at 15
/// ips) and falling over it; the long wavelengths barely moved.
#[test]
fn the_bias_trades_distortion_against_treble() {
    let hd = |bias: f64| third(&[(id::BIAS, bias), (id::LEVEL, 1.0)], 0.0).1;
    let (under, aligned, over) = (hd(-4.0), hd(0.0), hd(3.0));
    println!(
        "HD3: under {:.3} %, aligned {:.3} %, over {:.3} %",
        under * 100.0,
        aligned * 100.0,
        over * 100.0
    );
    assert!(under > 3.0 * aligned, "{under} vs {aligned}");
    assert!(over < 1.3 * aligned, "{over} vs {aligned}");
    // At 10 dB under the operating level (where alignment is done).
    let out = |bias: f64, f: f64| {
        let mut r = rig(&still(&[(id::BIAS, bias)]));
        let (l, _) = r.run(0.3, left(f, amp(REFERENCE_DBFS - 10.0)), silence);
        bin_db(&l, f)
    };
    let mut best = (f64::NEG_INFINITY, 0.0);
    for i in 0..=16 {
        let b = -6.0 + 0.5 * f64::from(i);
        let o = out(b, 10_000.0);
        if o > best.0 {
            best = (o, b);
        }
    }
    println!("10 kHz peaks at {:+.1} dB of bias", best.1);
    assert!((-4.5..=-1.0).contains(&best.1), "10 kHz peak at {}", best.1);
    let treble = out(3.0, 10_000.0) - out(0.0, 10_000.0);
    let lows = out(3.0, 1000.0) - out(0.0, 1000.0);
    assert!(
        treble < lows - 1.5,
        "over-bias: 10 kHz {treble:+.2}, 1 kHz {lows:+.2}"
    );
    assert!(lows.abs() < 1.5, "{lows}");
}

/// Wow and flutter: the transport's weighted peak (IEC 386 weighting, the
/// 2σ value) is a good machine's at each speed, times the control, and a
/// 3150 Hz tone through the whole machine carries it.
#[test]
fn wow_and_flutter_meet_the_machines_figures() {
    // The weighting as a filter: two poles high-pass at 1.5 Hz, one pole
    // low-pass at 10 Hz, 0 dB at 4 Hz (as `transport::weighting`).
    let weighted_peak = |dev: &[f64], rate: f64| {
        let w0 = std::f64::consts::TAU * 1.5 / rate;
        let (mut hp1, mut hp2, mut lp) = (0.0f64, 0.0f64, 0.0f64);
        let (mut x1, mut y1, mut x2, mut y2) = (0.0, 0.0, 0.0, 0.0);
        let r = 1.0 / (1.0 + w0);
        let k = 1.0 - (-std::f64::consts::TAU * 10.0 / rate).exp();
        let norm = transport::weighting(4.0) / {
            // The filter's own gain at 4 Hz, measured.
            let mut g = (0.0f64, 0.0f64, 0.0f64, 0.0, 0.0, 0.0, 0.0);
            let mut most = 0.0f64;
            for n in 0..(rate * 4.0) as usize {
                let x = (std::f64::consts::TAU * 4.0 * n as f64 / rate).sin();
                let a = r * (g.0 + x - g.3);
                g.3 = x;
                g.0 = a;
                let b = r * (g.1 + a - g.4);
                g.4 = a;
                g.1 = b;
                g.2 += k * (b - g.2);
                if n as f64 > rate * 2.0 {
                    most = most.max(g.2.abs());
                }
            }
            most
        };
        let mut w: Vec<f64> = dev
            .iter()
            .map(|x| {
                let a = r * (hp1 + x - x1);
                x1 = *x;
                hp1 = a;
                let b = r * (hp2 + a - x2);
                x2 = a;
                hp2 = b;
                let _ = (&mut y1, &mut y2);
                lp += k * (b - lp);
                (lp * norm).abs()
            })
            .collect();
        let skip = w.len() / 5;
        let mut w = w.split_off(skip);
        w.sort_by(|a, b| a.partial_cmp(b).unwrap());
        w[(w.len() as f64 * 0.95) as usize]
    };
    let rate = 2000.0;
    // (At 3.75 ips the transport's room holds a good machine's figure;
    // more is held to about that.)
    for (speed, spec) in FLUTTER_SPEC.iter().enumerate() {
        for depth in [1.0, 2.0] {
            if speed == 0 && depth > 1.0 {
                continue;
            }
            let mut t = transport::Transport::new(SR, 14.0);
            t.set(speed, depth);
            let step = (SR / rate) as usize;
            let mut dev = Vec::new();
            for n in 0..(SR * 30.0) as usize {
                t.tick();
                if n % step == 0 {
                    dev.push(t.speed);
                }
            }
            let peak = weighted_peak(&dev, rate);
            let want = spec * depth;
            println!(
                "{} ips ×{depth}: weighted peak {:.4} %, spec {:.3} %",
                SPEEDS[speed],
                peak * 100.0,
                want * 100.0
            );
            assert!(
                (0.7..1.35).contains(&(peak / want)),
                "{} ips ×{depth}: {:.4} % for {:.3} %",
                SPEEDS[speed],
                peak * 100.0,
                want * 100.0
            );
        }
    }
    // Off is off.
    let mut t = transport::Transport::new(SR, 14.0);
    t.set(2, 0.0);
    for _ in 0..10_000 {
        assert_eq!(t.tick(), 0.0);
    }
    // Through the machine: a 3150 Hz tone's period, measured between its
    // upward zero crossings, wanders by the weighted figure.
    let mut r = rig(&[(id::SPEED, 2.0)]);
    let (l, _) = r.run(12.0, left(3150.0, amp(REFERENCE_DBFS)), silence);
    let mut crossings = Vec::new();
    for i in 1..l.len() {
        let (a, b) = (f64::from(l[i - 1]), f64::from(l[i]));
        if a < 0.0 && b >= 0.0 {
            crossings.push(i as f64 - 1.0 + a / (a - b));
        }
    }
    let periods: Vec<f64> = crossings.windows(2).map(|w| w[1] - w[0]).collect();
    let mean = periods.iter().sum::<f64>() / periods.len() as f64;
    let dev: Vec<f64> = periods.iter().map(|p| mean / p - 1.0).collect();
    // One reading a period: resample to 2 kHz by holding.
    let per = SR / mean;
    let mut held = Vec::new();
    let mut t = 0.0;
    for d in &dev {
        t += 1.0 / per;
        while (held.len() as f64) < t * rate {
            held.push(*d);
        }
    }
    let peak = weighted_peak(&held, rate);
    println!("3150 Hz through the machine: {:.4} %", peak * 100.0);
    assert!((0.6..1.5).contains(&(peak / FLUTTER_SPEC[2])), "{peak}");
}

/// Scrape flutter puts modulation noise beside a treble tone, far under it.
#[test]
fn scrape_flutter_is_heard_as_modulation_noise() {
    let sidebands = |depth: f64| {
        let mut r = rig(&[(id::FLUTTER, depth)]);
        let (l, _) = r.run(1.0, left(10_000.0, amp(REFERENCE_DBFS)), silence);
        let tone = bin_db(&l, 10_000.0);
        let side = [6_500.0, 7_000.0, 13_000.0, 13_500.0]
            .iter()
            .map(|f| 10f64.powf(bin_db(&l, *f) / 10.0))
            .sum::<f64>();
        10.0 * side.log10() - tone
    };
    let (with, without) = (sidebands(1.0), sidebands(0.0));
    println!("scrape sidebands {with:.1} dB re the tone, still {without:.1}");
    assert!(with > without + 10.0, "{with} vs {without}");
    assert!((-85.0..-40.0).contains(&with), "{with}");
}

/// The hiss at its measured level: the standard tape's A-weighted noise is
/// 76.6 dB under its 3 % level (1077 nWb/m) at 15 ips on a ¼″ track; half
/// the speed adds 3 dB, the ½″ track takes 4.3 dB off, and it is off
/// unless asked for.
#[test]
fn the_hiss_is_the_tapes() {
    let a_weighted = |set: &[(u32, f64)]| {
        let mut r = rig(&still(set));
        let (l, _) = r.run(2.0, silence, silence);
        let tail = &l[l.len() / 2..];
        // A-weighting by an FFT-free sum: bins of a periodogram over
        // 4096-sample windows.
        let n = 4096;
        let mut power = vec![0.0f64; n / 2];
        let windows = tail.len() / n;
        for w in 0..windows {
            let x = &tail[w * n..(w + 1) * n];
            for (k, p) in power.iter_mut().enumerate().skip(1) {
                let wk = std::f64::consts::TAU * k as f64 / n as f64;
                let (mut re, mut im) = (0.0, 0.0);
                for (i, v) in x.iter().enumerate() {
                    let win = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
                    re += f64::from(*v) * win * (wk * i as f64).cos();
                    im += f64::from(*v) * win * (wk * i as f64).sin();
                }
                *p += re * re + im * im;
            }
        }
        // Hann's power gain 3/8; one-sided.
        let mut sum = 0.0;
        for (k, p) in power.iter().enumerate().skip(1) {
            let f = k as f64 * SR / n as f64;
            if !(20.0..=20_000.0).contains(&f) {
                continue;
            }
            let a = heads::a_weighting(f);
            sum += p / windows as f64 * a * a * 2.0 / (n as f64 * n as f64 * 0.375);
        }
        // dB re the operating level's sine (its RMS).
        10.0 * sum.log10() - 20.0 * (gain(REFERENCE_DBFS) / 2f64.sqrt()).log10()
    };
    let level = [(id::HISS, 1.0), (id::LEVEL, 1.0)];
    let standard = a_weighted(&level);
    // 0.159 nWb/m A-weighted under a 250 nWb/m sine.
    let want = 20.0 * (1077.0 / 6_760.8 / 250.0f64).log10();
    println!("hiss {standard:.1} dB re 250 nWb/m, the sheet {want:.1}");
    assert!((standard - want).abs() < 1.5, "{standard} vs {want}");
    let slow = a_weighted(&[(id::HISS, 1.0), (id::LEVEL, 1.0), (id::SPEED, 1.0)]);
    let wide = a_weighted(&[(id::HISS, 1.0), (id::LEVEL, 1.0), (id::FORMAT, 1.0)]);
    let hot = a_weighted(&[(id::HISS, 1.0), (id::LEVEL, 4.0)]);
    println!("7.5 ips {slow:.1}, ½″ {wide:.1}, at 520 nWb/m {hot:.1}");
    assert!(slow > standard + 1.0, "{slow} vs {standard}");
    assert!((standard - wide - 4.3).abs() < 1.0, "{wide}");
    assert!((standard - hot - 20.0 * (520.0f64 / 250.0).log10()).abs() < 0.5);
    // Off: silence stays silence.
    let mut r = rig(&[]);
    let (l, rr) = r.run(0.5, silence, silence);
    assert!(l.iter().chain(&rr).all(|v| *v == 0.0));
}

#[test]
fn crosstalk_and_azimuth_are_the_heads() {
    // A tone on the left only: −45 dB on the right on ¼″, −53 on ½″.
    for (format, want) in [(0.0, -45.0), (1.0, -53.0)] {
        let mut r = rig(&still(&[(id::FORMAT, format)]));
        let (l, rr) = r.run(0.5, left(1000.0, amp(REFERENCE_DBFS)), silence);
        let leak = bin_db(&rr, 1000.0) - bin_db(&l, 1000.0);
        assert!((leak - want).abs() < 1.0, "{format}: {leak:.1} dB");
    }
    // Azimuth: 10′ skews the tracks by pitch·tan θ / v (33 µs at 15 ips on
    // ¼″): 10 kHz on both tracks comes out that far apart in phase.
    let mut r = rig(&still(&[(id::AZIMUTH, 10.0)]));
    let (l, rr) = r.run(0.5, tone(10_000.0, amp(REFERENCE_DBFS - 10.0)), silence);
    let phase = |x: &[f32]| {
        let tail = &x[x.len() / 2..];
        let w = std::f64::consts::TAU * 10_000.0 / SR;
        let off = x.len() / 2;
        let (mut re, mut im) = (0.0, 0.0);
        for (k, v) in tail.iter().enumerate() {
            re += f64::from(*v) * (w * (k + off) as f64).cos();
            im += f64::from(*v) * (w * (k + off) as f64).sin();
        }
        im.atan2(re)
    };
    let mut d = (phase(&l) - phase(&rr)).to_degrees();
    while d > 180.0 {
        d -= 360.0;
    }
    while d < -180.0 {
        d += 360.0;
    }
    let tau = FORMATS[0].pitch * (10.0f64 / 60.0).to_radians().tan() / 0.381;
    let want = 360.0 * 10_000.0 * tau;
    assert!((d.abs() - want).abs() < 15.0, "{d:.1}° for {want:.1}°");
}

#[test]
fn the_latency_is_the_oversamplings_and_mix_zero_is_the_dry_signal_in_time() {
    for (setting, want) in [(0.0, 64), (1.0, 72), (2.0, 74)] {
        let r = rig(&[(id::OVERSAMPLING, setting)]);
        assert_eq!(latency(&r.params, SR), want);
        assert_eq!(os::latency(factor(setting)), want);
    }
    let mut r = rig(&[(id::MIX, 0.0), (id::INPUT, 12.0), (id::OUTPUT, -6.0)]);
    let input = tone(440.0, 0.5);
    let (l, _) = r.run(0.2, &input, silence);
    let d = latency(&r.params, SR) as usize;
    for (i, y) in l.iter().enumerate().skip(d + 1) {
        let want = f64::from(input(i - d).0) * amp(-6.0);
        assert!((f64::from(*y) - want).abs() < 1e-5, "{i}: {y} vs {want}");
    }
    // The wet signal arrives at the latency too: a short burst's
    // cross-correlation with the dry peaks within a sample of it.
    for setting in [0.0, 1.0, 2.0] {
        let mut r = rig(&still(&[(id::OVERSAMPLING, setting), (id::SPEED, 3.0)]));
        let burst = |n: usize| {
            let t = n as f64 - 2000.0;
            let v = 0.05 * (-t * t / 800.0).exp() * (std::f64::consts::TAU * 2000.0 * t / SR).sin();
            (v as f32, 0.0)
        };
        let (l, _) = r.run(0.2, burst, silence);
        let x: Vec<f64> = (0..l.len()).map(|n| f64::from(burst(n).0)).collect();
        let lag = (0..200)
            .max_by(|a, b| {
                let c = |lag: usize| -> f64 {
                    (lag..l.len()).map(|i| x[i - lag] * f64::from(l[i])).sum()
                };
                c(*a).partial_cmp(&c(*b)).unwrap()
            })
            .unwrap();
        let want = latency(&r.params, SR) as i64;
        assert!(
            (lag as i64 - want).abs() <= 1,
            "{setting}: {lag} for {want}"
        );
    }
}

/// Every extreme: the hottest input, the bias at both ends, every speed,
/// tape, format and oversampling, loud squares, noise, a low tone, DC —
/// finite and bounded.
#[test]
fn it_stays_finite_and_bounded_at_every_extreme() {
    let square = |n: usize| {
        let v = if (n / 37).is_multiple_of(2) {
            1.0
        } else {
            -1.0
        };
        (v, -v)
    };
    let noise = |n: usize| {
        let mut x = (n as u32).wrapping_mul(2_654_435_761) ^ 0x5bd1_e995;
        x ^= x >> 15;
        let v = (x as f32 / u32::MAX as f32) * 2.0 - 1.0;
        (v, -v * 0.7)
    };
    let low = tone(31.0, 1.0);
    let dc = |_: usize| (0.9f32, -0.9f32);
    for speed in [0.0, 3.0] {
        for tape in [0.0, 2.0] {
            for bias in [-6.0, 6.0] {
                for os in [0.0, 2.0] {
                    let set = [
                        (id::INPUT, 24.0),
                        (id::AUTO_OUTPUT, 0.0),
                        (id::SPEED, speed),
                        (id::TAPE, tape),
                        (id::BIAS, bias),
                        (id::OVERSAMPLING, os),
                        (id::LEVEL, 4.0),
                        (id::FLUTTER, 5.0),
                        (id::AZIMUTH, -20.0),
                        (id::HISS, 1.0),
                        (id::FORMAT, 1.0),
                    ];
                    let mut r = rig(&set);
                    for (name, signal) in [
                        ("square", &square as &dyn Fn(usize) -> (f32, f32)),
                        ("noise", &noise),
                        ("low", &low),
                        ("dc", &dc),
                    ] {
                        let (l, rr) = r.run(0.15, signal, silence);
                        let ok = l.iter().chain(&rr).all(|v| v.is_finite());
                        let p = peak_db(&l).max(peak_db(&rr));
                        assert!(ok && p < 30.0, "{set:?} {name}: peak {p:.1}");
                    }
                    r.p.reset();
                    let (l, _) = r.run(0.05, silence, silence);
                    assert!(l.iter().all(|v| v.is_finite()));
                }
            }
        }
    }
    // Parameters moving every block.
    let mut r = rig(&[]);
    for i in 0..60 {
        r.set(id::BIAS, if i % 2 == 0 { -6.0 } else { 6.0 });
        r.set(id::SPEED, f64::from(i % 4));
        r.set(id::TAPE, f64::from(i % 3));
        r.set(id::INPUT, if i % 3 == 0 { 24.0 } else { -24.0 });
        let (l, _) = r.run(BLOCK as f64 / SR, square, silence);
        assert!(l.iter().all(|v| v.is_finite()));
    }
}

#[test]
fn it_shows_its_values() {
    let f = |id: u32, v: f64| format(ParameterId(id), v).unwrap();
    assert_eq!(f(id::SPEED, 2.0), "15 ips");
    assert_eq!(f(id::LEVEL, 1.0), "250 nWb/m (+3)");
    assert_eq!(f(id::BIAS, 0.0), "0.0 dB (aligned)");
    assert_eq!(f(id::BIAS, 2.0), "+2.0 dB over");
    assert_eq!(f(id::BIAS, -1.5), "−1.5 dB under");
    assert_eq!(f(id::OVERSAMPLING, 1.0), "4×");
    assert_eq!(standard_name(2, true), "IEC 35 µs");
    assert_eq!(standard_name(2, false), "NAB 50 µs");
    assert_eq!(standard_name(3, false), "AES 17.5 µs");
    assert_eq!(standard_name(0, true), "IEC 90 µs + 3180");
    // Ids are positions.
    for (i, p) in parameters().iter().enumerate() {
        assert_eq!(p.id.0 as usize, i);
    }
}

/// The record equaliser: 0 dB at 1 kHz at every speed and standard, and
/// the slower the tape, the harder it drives 10 kHz (the losses it makes
/// up grow as the wavelength shrinks) — while record and reproduce
/// together stay at 0 dB at 1 kHz.
#[test]
fn the_record_equaliser_drives_slow_tape_hardest() {
    let fs = 192_000.0;
    let t = FORMULATIONS[0];
    let drive = |speed: usize, iec: bool| {
        let d = heads::Design::new(&heads::Setup {
            speed,
            iec,
            coating: t.coating,
            zone: 1.0,
            format: 0,
            azimuth: 0.0,
            fs,
        });
        assert!((d.record_response(1000.0, fs) - 1.0).abs() < 1e-9);
        assert!((d.response(1000.0, fs) - 1.0).abs() < 1e-9);
        20.0 * d.record_response(10_000.0, fs).log10()
    };
    let (d3, d7, d15, d30) = (
        drive(0, false),
        drive(1, false),
        drive(2, false),
        drive(3, false),
    );
    println!("10 kHz record drive: {d3:+.1} {d7:+.1} {d15:+.1} {d30:+.1} dB");
    assert!(d3 > d15 + 6.0 && d7 > d15 + 6.0, "{d3} {d7} {d15}");
    assert!(d15 > 0.0 && d30 > 0.0, "{d15} {d30}");
    // IEC's 35 µs at 15 ips leaves more treble on the tape than NAB's 50.
    assert!(drive(2, true) > d15 + 1.0);
}

/// The aliases of the tape's harmonics at each oversampling: a 7 kHz tone
/// 6 dB over the operating level at 7.5 ips (the record equaliser drives
/// it far into the tape), the power at the in-band alias frequencies of
/// its harmonics 3–31 against the fundamental (run by hand with
/// `--ignored --nocapture`).
#[test]
#[ignore]
fn tape_aliases() {
    let f = 7000.0;
    for setting in [0.0, 1.0, 2.0] {
        let mut r = rig(&still(&[(id::OVERSAMPLING, setting), (id::SPEED, 1.0)]));
        let (l, _) = r.run(1.0, left(f, amp(REFERENCE_DBFS + 6.0)), silence);
        let rate = SR * factor(setting) as f64;
        let fund = bin_db(&l, f);
        let mut power = 0.0;
        for n in (3..=31).step_by(2) {
            let h = f * n as f64;
            // Folded at the internal rate, then what lands in the band.
            let folded = (h % rate).min(rate - h % rate);
            if h > SR / 2.0 && folded < 20_000.0 && folded > 20.0 {
                power += 10f64.powf((bin_db(&l, folded) - fund) / 10.0);
            }
        }
        println!(
            "{}: aliases {:.1} dB under the fundamental",
            OVERSAMPLING_NAMES[setting as usize],
            -10.0 * power.max(1e-30).log10()
        );
    }
}

/// What a second of stereo costs at each oversampling (run by hand:
/// `cargo test -p faderframe-plugin-host --release tape_cost -- --ignored
/// --nocapture`).
#[test]
#[ignore]
fn tape_cost() {
    for setting in [0.0, 1.0, 2.0] {
        for under in [false, true] {
            let mut r = rig(&[
                (id::OVERSAMPLING, setting),
                (id::HISS, 1.0),
                (id::BIAS, if under { -3.0 } else { 0.0 }),
            ]);
            let music = |n: usize| {
                let t = n as f64 / SR;
                let v = 0.3 * (std::f64::consts::TAU * 110.0 * t).sin()
                    + 0.1 * (std::f64::consts::TAU * 2_500.0 * t).sin();
                (v as f32, (v * 0.8) as f32)
            };
            r.run(0.5, music, silence);
            let start = std::time::Instant::now();
            r.run(5.0, music, silence);
            let s = start.elapsed().as_secs_f64() / 5.0;
            println!(
                "{}{}: {:.1} ms per second of stereo ({:.2} % of a core, {:.0} ns a sample)",
                OVERSAMPLING_NAMES[setting as usize],
                if under { " under-biased" } else { "" },
                s * 1000.0,
                s * 100.0,
                s * 1e9 / SR
            );
        }
    }
}
