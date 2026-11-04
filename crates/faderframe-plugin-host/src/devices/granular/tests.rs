#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::poly::rig::{Play, on, peak_hz, rms};
use crate::devices::samples::{self, SampleDoc, SampleHost};
use faderframe_core::ParameterId;

fn play_with(set: &[(u32, f64)], shared: Shared) -> Play<GranularProcessor> {
    let params = ParamValues::new(parameters());
    for (pid, v) in set {
        params.set_by_id(ParameterId(*pid), *v).unwrap();
    }
    let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
    Play::new(GranularProcessor::new(
        params,
        Some(tap),
        &crate::devices::rig::config(),
        shared,
    ))
}

fn play(set: &[(u32, f64)]) -> Play<GranularProcessor> {
    play_with(set, samples::empty())
}

/// A sample of 220 Hz for its first half and 880 Hz for its second.
/// (In a folder of its own per test: tests run at once.)
fn two_tones(name: &str) -> (SampleHost, std::path::PathBuf) {
    let dir = samples::fixtures::dir(&format!("granular-{name}"));
    let path = dir.join("two.wav");
    let n = 48_000;
    let x: Vec<f32> = (0..2 * n)
        .map(|i| {
            let f = if i < n { 220.0 } else { 880.0 };
            (0.5 * (std::f64::consts::TAU * f * i as f64 / 48_000.0).sin()) as f32
        })
        .collect();
    faderframe_audio_files::write_wav(
        &path,
        &[x],
        48_000,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )
    .unwrap();
    let mut doc = SampleDoc::default();
    doc.set(0, Some(path.to_string_lossy().into_owned()));
    let mut host = SampleHost::default();
    host.set_doc(doc, None);
    (host, dir)
}

/// Grains that read one place, at the sample's own pitch.
fn still(extra: &[(u32, f64)]) -> Vec<(u32, f64)> {
    let mut v = vec![
        (id::FREEZE, 1.0),
        (id::SPRAY, 0.0),
        (id::PITCH_RANDOM, 0.0),
        (id::FOLLOW_KEYS, 0.0),
        (id::ATTACK, 1.0),
        (id::SUSTAIN, 1.0),
        (id::CHORUS_MIX, 0.0),
    ];
    v.extend_from_slice(extra);
    v
}

#[test]
fn ids_are_positions() {
    for (i, p) in parameters().iter().enumerate() {
        assert_eq!(p.id.0 as usize, i, "{}", p.name);
    }
    assert_eq!(TARGETS.len(), target::SCAN + 1);
    assert_eq!(
        format(ParameterId(id::WINDOW), 3.0).as_deref(),
        Some("Gauss")
    );
    assert_eq!(format(ParameterId(id::ROOT), 60.0).as_deref(), Some("C4"));
    assert!(per_voice(ParameterId(id::POSITION)));
    assert!(!per_voice(ParameterId(id::WINDOW)));
    for w in windows() {
        assert!(w[0] < 0.2 && w.iter().all(|v| (0.0..=1.0001).contains(v)));
    }
}

/// Grains start as often as the density says: evenly without jitter,
/// about as often with it.
#[test]
fn grains_start_at_their_density() {
    for (density, jitter) in [(10.0, 0.0), (40.0, 0.0), (120.0, 0.0), (40.0, 1.0)] {
        let mut p = play(&still(&[
            (id::DENSITY, density),
            (id::JITTER, jitter),
            (id::SIZE, 20.0),
        ]));
        p.send(on(60));
        p.run(0.5);
        let before = p.p.grains_started();
        p.run(4.0);
        let count = (p.p.grains_started() - before) as f64;
        // Evenly: within a grain of the count; jittered: within 10 %.
        let reach = if jitter > 0.0 {
            0.1 * density * 4.0
        } else {
            1.0
        };
        assert!(
            (count - density * 4.0).abs() <= reach,
            "density {density}, jitter {jitter}: {count} in 4 s"
        );
    }
    // The level stays about the same as grains pile up.
    let level = |density: f64| {
        let mut p = play(&still(&[(id::DENSITY, density), (id::SIZE, 100.0)]));
        p.send(on(60));
        let (l, _) = p.run(1.0);
        rms(&l[24_000..])
    };
    let (sparse, dense) = (level(10.0), level(160.0));
    let db = 20.0 * (dense / sparse).log10();
    assert!(db.abs() < 6.0, "denser by {db:.1} dB");
}

#[test]
fn the_position_picks_the_part_of_the_sample_heard() {
    let (host, dir) = two_tones("position");
    for (position, want) in [(0.25, 220.0), (0.75, 880.0)] {
        let mut p = play_with(
            &still(&[(id::POSITION, position), (id::PAN_SPREAD, 0.0)]),
            Arc::clone(&host.shared),
        );
        p.send(on(60));
        let (l, _) = p.run(1.0);
        let f = peak_hz(&l[16_000..]);
        assert!((f - want).abs() < want * 0.03, "at {position}: {f}");
    }
    // Pitched up an octave, and following the keys from the root.
    let mut p = play_with(
        &still(&[(id::POSITION, 0.25), (id::PITCH, 12.0)]),
        Arc::clone(&host.shared),
    );
    p.send(on(60));
    let (l, _) = p.run(1.0);
    assert!((peak_hz(&l[16_000..]) - 440.0).abs() < 12.0);
    let mut p = play_with(
        &still(&[
            (id::POSITION, 0.25),
            (id::FOLLOW_KEYS, 1.0),
            (id::ROOT, 60.0),
        ]),
        Arc::clone(&host.shared),
    );
    p.send(on(67));
    let (l, _) = p.run(1.0);
    let fifth = 220.0 * 2f64.powf(7.0 / 12.0);
    assert!((peak_hz(&l[16_000..]) - fifth).abs() < 8.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unfrozen_the_position_scans_through_the_sample() {
    let (host, dir) = two_tones("scan");
    let mut p = play_with(
        &still(&[
            (id::FREEZE, 0.0),
            (id::SCAN, 1.0),
            (id::POSITION, 0.0),
            (id::PAN_SPREAD, 0.0),
        ]),
        Arc::clone(&host.shared),
    );
    p.send(on(60));
    let (first, _) = p.run(0.8);
    let (second, _) = p.run(1.0);
    assert!((peak_hz(&first[8_000..]) - 220.0).abs() < 10.0);
    assert!(
        (peak_hz(&second[24_000..]) - 880.0).abs() < 20.0,
        "moved on: {}",
        peak_hz(&second[24_000..])
    );
    // Frozen it stays.
    let mut p = play_with(
        &still(&[(id::SCAN, 1.0), (id::POSITION, 0.0), (id::PAN_SPREAD, 0.0)]),
        Arc::clone(&host.shared),
    );
    p.send(on(60));
    p.run(1.2);
    let (later, _) = p.run(0.5);
    assert!((peak_hz(&later) - 220.0).abs() < 10.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn with_no_sample_it_granulates_its_own_pad() {
    let mut p = play(&[]);
    p.send(on(60));
    let (l, r) = p.run(1.0);
    assert!(rms(&l) > 1e-3 && rms(&r) > 1e-3);
    let tap = p.p.tap.as_ref().unwrap();
    assert!(tap.value(value::GRAINS) >= 1.0);
    assert!((0.0..=1.0).contains(&tap.value(value::GRAIN)));
}

/// The cost of a voice (by hand: `cargo test -p faderframe-plugin-host
/// --release granular_cost -- --ignored --nocapture`).
#[test]
#[ignore]
fn granular_cost() {
    for (name, density, size) in [
        ("25 grains/s × 90 ms", 25.0, 90.0),
        ("100 grains/s × 200 ms", 100.0, 200.0),
        ("200 grains/s × 240 ms (full)", 200.0, 240.0),
    ] {
        let mut p = play(&[
            (id::DENSITY, density),
            (id::SIZE, size),
            (id::POLYPHONY, 32.0),
            (id::SUSTAIN, 1.0),
        ]);
        for k in 0..16u8 {
            p.send(on(40 + 2 * k));
        }
        p.run(0.5);
        let seconds = 4.0;
        let t = std::time::Instant::now();
        p.run(seconds);
        let used = t.elapsed().as_secs_f64() / seconds;
        println!(
            "{name:30} {:6.3} % of a core per voice",
            100.0 * used / 16.0
        );
    }
}
