#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::poly::rig::{Play, SR, inharmonic_db, off, on, peak_hz, rms};
use crate::devices::samples::{self, SampleDoc};
use crate::{NoteParamMod, PluginProcessor};
use faderframe_core::ParameterId;
use faderframe_midi::MidiEvent;

fn play(set: &[(u32, f64)]) -> Play<WavetableProcessor> {
    play_with(set, samples::empty())
}

fn play_with(set: &[(u32, f64)], shared: Shared) -> Play<WavetableProcessor> {
    let params = ParamValues::new(parameters());
    for (id, v) in set {
        params.set_by_id(ParameterId(*id), *v).unwrap();
    }
    let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
    Play::new(WavetableProcessor::new(
        params,
        Some(tap),
        &crate::devices::rig::config(),
        shared,
    ))
}

fn mono(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r).map(|(a, b)| a + b).collect()
}

/// A plain, steady patch: no filter, full sustain, one copy.
fn steady(extra: &[(u32, f64)]) -> Vec<(u32, f64)> {
    let mut v = vec![
        (id::SUSTAIN, 1.0),
        (id::ATTACK, 1.0),
        (id::FILTER_TYPE, 0.0),
        (id::osc(0, id::RANDOM), 0.0),
    ];
    v.extend_from_slice(extra);
    v
}

#[test]
fn ids_are_positions() {
    for (i, p) in parameters().iter().enumerate() {
        assert_eq!(p.id.0 as usize, i, "{}", p.name);
    }
    assert_eq!(TARGETS.len(), target::NOISE + 1);
    assert_eq!(
        &TARGETS[..poly::COMMON_TARGETS.len()],
        &poly::COMMON_TARGETS
    );
    assert_eq!(
        format(ParameterId(id::osc(1, id::TABLE)), USER as f64).as_deref(),
        Some("User")
    );
    assert_eq!(
        format(ParameterId(id::osc(0, id::WARP)), 4.0).as_deref(),
        Some("Mirror")
    );
    assert!(per_voice(ParameterId(id::osc(0, id::POSITION))));
    assert!(!per_voice(ParameterId(id::osc(0, id::TABLE))));
}

#[test]
fn a_note_plays_its_pitch_and_the_sine_frame_is_pure() {
    // Basic Shapes' first frame is a sine.
    let mut p = play(&steady(&[(id::osc(0, id::POSITION), 0.0)]));
    p.send(on(69));
    let (l, r) = p.run(0.5);
    let x = mono(&l, &r);
    assert!((peak_hz(&x) - 440.0).abs() < 4.0, "{}", peak_hz(&x));
    assert!(inharmonic_db(&x, 440.0) < -60.0);
    let (mags, bin) = crate::devices::poly::rig::spectrum(&x);
    let at = |f: f64| mags[(f / bin).round() as usize];
    assert!(at(880.0) < at(440.0) * 1e-3, "no second harmonic");
    p.send(off(69));
    let (l, _) = p.run(1.0);
    assert!(rms(&l[l.len() - 4_800..]) < 1e-4, "released");
}

/// High notes read the band-limited copies: what is not a harmonic stays
/// far down, from a square (all odd harmonics to Nyquist) up to C8.
#[test]
fn high_notes_do_not_alias() {
    for key in [84u8, 96, 100, 103, 108] {
        let f0 = 440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0);
        let mut p = play(&steady(&[(id::osc(0, id::POSITION), 1.0)]));
        p.send(on(key));
        let (l, r) = p.run(0.6);
        let x = mono(&l, &r);
        let alias = inharmonic_db(&x, f0);
        assert!(alias < -60.0, "key {key}: aliases at {alias:.1} dB");
    }
}

#[test]
fn warps_and_frames_change_the_sound() {
    let brightness = |set: &[(u32, f64)]| {
        let mut p = play(&steady(set));
        p.send(on(57));
        let (l, r) = p.run(0.5);
        let x = mono(&l, &r);
        let (mags, bin) = crate::devices::poly::rig::spectrum(&x);
        let total: f64 = mags.iter().map(|m| m * m).sum();
        let high: f64 = mags
            .iter()
            .enumerate()
            .filter(|(k, _)| *k as f64 * bin > 2_000.0)
            .map(|(_, m)| m * m)
            .sum();
        high / total
    };
    let sine = brightness(&[(id::osc(0, id::POSITION), 0.0)]);
    let saw = brightness(&[(id::osc(0, id::POSITION), 0.65)]);
    assert!(saw > sine * 100.0, "a later frame is brighter");
    for (warp, amount) in [(warp::SYNC, 0.6), (warp::BEND_UP, 0.8), (warp::MIRROR, 1.0)] {
        let w = brightness(&[
            (id::osc(0, id::POSITION), 0.0),
            (id::osc(0, id::WARP), warp as f64),
            (id::osc(0, id::WARP_AMOUNT), amount),
        ]);
        assert!(w > sine * 10.0, "{}: {w} vs {sine}", WARPS[warp]);
    }
    // FM from oscillator 2 (silent itself) colours oscillator 1.
    let fm = brightness(&[
        (id::osc(0, id::POSITION), 0.0),
        (id::osc(0, id::WARP), warp::FM as f64),
        (id::osc(0, id::WARP_AMOUNT), 0.8),
        (id::osc(1, id::POSITION), 0.0),
        (id::osc(1, id::SEMI), 7.0),
        (id::osc(1, id::OCTAVE), 2.0),
    ]);
    assert!(fm > sine * 10.0, "FM: {fm} vs {sine}");
}

#[test]
fn the_matrix_moves_the_position_and_unison_spreads() {
    let mut p = play(&steady(&[
        (id::osc(0, id::POSITION), 0.2),
        (id::mod_source(0), poly::source::LFO1 as f64),
        (id::mod_target(0), target::POSITION[0] as f64),
        (id::mod_amount(0), 0.5),
        (id::LFO_RATE, 3.0),
    ]));
    p.send(on(60));
    let mut seen = Vec::new();
    for _ in 0..20 {
        p.run(0.02);
        seen.push(p.p.tap.as_ref().unwrap().value(value::POSITION[0]));
    }
    let (lo, hi) = seen
        .iter()
        .fold((1f32, 0f32), |(a, b), v| (a.min(*v), b.max(*v)));
    assert!(hi - lo > 0.3, "the position moves: {lo}…{hi}");
    // Seven copies spread across the sides: the sides differ.
    let mut p = play(&steady(&[
        (id::osc(0, id::UNISON), 7.0),
        (id::osc(0, id::DETUNE), 40.0),
        (id::osc(0, id::SPREAD), 1.0),
        (id::osc(0, id::RANDOM), 1.0),
    ]));
    p.send(on(57));
    let (l, r) = p.run(0.5);
    let side: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
    assert!(rms(&side) > 0.2 * rms(&l), "a wide unison");
}

/// A WAV with Serum's `clm ` chunk naming its frame size.
fn clm_wav(path: &std::path::Path, frames: usize, size: usize) {
    let mut samples = Vec::new();
    for f in 0..frames {
        for i in 0..size {
            let ph = i as f64 / size as f64;
            let x = (std::f64::consts::TAU * ph).sin()
                + f as f64 / frames as f64 * (std::f64::consts::TAU * 5.0 * ph).sin();
            samples.push((x * 0.4 * 32767.0) as i16);
        }
    }
    let clm = format!("<!>{size} 10000000 wavetable (test)");
    let mut clm_bytes = clm.into_bytes();
    if clm_bytes.len() % 2 == 1 {
        clm_bytes.push(0);
    }
    let data_len = samples.len() * 2;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&((4 + 24 + 8 + clm_bytes.len() + 8 + data_len) as u32).to_le_bytes());
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&44_100u32.to_le_bytes());
    b.extend_from_slice(&88_200u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"clm ");
    b.extend_from_slice(&(clm_bytes.len() as u32).to_le_bytes());
    b.extend_from_slice(&clm_bytes);
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, b).unwrap();
}

#[test]
fn a_user_table_loads_by_its_clm_chunk_and_plays() {
    let dir = samples::fixtures::dir("wavetable-clm");
    let path = dir.join("table.wav");
    clm_wav(&path, 8, 1024);
    assert_eq!(table::clm_frame_size(&path), Some(1024));
    let mut doc = SampleDoc::default();
    doc.set(1, Some(path.to_string_lossy().into_owned()));
    let set = samples::load_with(&doc, Some(prepare));
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    let t = set.tables[1].as_ref().unwrap();
    assert_eq!(t.frames, 8, "8 frames of 1024");
    // The document's size wins over the chunk.
    doc.set_cycle(1, 2048);
    let set = samples::load_with(&doc, Some(prepare));
    assert_eq!(set.tables[1].as_ref().unwrap().frames, 4);
    doc.set_cycle(1, 0);
    // Played by oscillator 2 set to its own table.
    let mut host = samples::SampleHost::with_prepare(prepare);
    host.set_doc(doc, None);
    let mut p = play_with(
        &steady(&[
            (id::osc(0, id::LEVEL), 0.0),
            (id::osc(1, id::LEVEL), 1.0),
            (id::osc(1, id::TABLE), USER as f64),
            (id::osc(1, id::POSITION), 1.0),
            (id::osc(1, id::RANDOM), 0.0),
        ]),
        Arc::clone(&host.shared),
    );
    p.send(on(45));
    let (l, r) = p.run(0.5);
    let x = mono(&l, &r);
    let (mags, bin) = crate::devices::poly::rig::spectrum(&x);
    let at = |f: f64| mags[(f / bin).round() as usize];
    // The last frame has the fifth harmonic nearly as loud as the first.
    assert!(at(550.0) > 0.5 * at(110.0), "{} {}", at(550.0), at(110.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn per_note_modulation_moves_only_its_voice() {
    let mut p = play(&steady(&[]));
    // Key 60's oscillator taken right down; key 64 untouched.
    p.mods = vec![NoteParamMod {
        parameter: ParameterId(id::osc(0, id::LEVEL)),
        channel: 0,
        key: 60,
        amount: -1.0,
        sample_offset: 0,
    }];
    p.send(on(60));
    let (l, _) = p.run(0.3);
    assert!(rms(&l[l.len() / 2..]) < 1e-4, "key 60 silenced");
    p.send(on(64));
    let (l, _) = p.run(0.3);
    assert!(rms(&l[l.len() / 2..]) > 1e-2, "key 64 sounds");
}

#[test]
fn notes_play_the_tuning() {
    use faderframe_midi::tuning::Tuning;
    // A = 432 Hz.
    let mut p = play(&steady(&[(id::osc(0, id::POSITION), 0.0)]));
    p.p.set_tuning(&Tuning::twelve_tet().table(432.0));
    p.send(on(69));
    let (l, r) = p.run(0.5);
    assert!((peak_hz(&mono(&l, &r)) - 432.0).abs() < 4.0);
    p.send(off(69));
    p.run(1.0);
    p.p.set_tuning(&Tuning::twelve_tet().table(440.0));
    p.send(on(69));
    // The pitch wheel bends by its range (2 semitones).
    p.send(MidiEvent::PitchBend {
        channel: 0,
        value: 16_383,
    });
    let (l, r) = p.run(0.5);
    let f = peak_hz(&mono(&l, &r));
    assert!((f - 493.9).abs() < 5.0, "{f}");
}

/// A note's own expressions reach only it: tuning, and an MPE-style
/// member channel's own bend over the ±48 semitones its zone gives.
#[test]
fn notes_take_their_expressions_and_mpe_bends() {
    use faderframe_midi::{ExpressionValue, NoteExpressionKind};
    let mut p = play(&steady(&[(id::osc(0, id::POSITION), 0.0)]));
    p.send(on(57));
    p.send(MidiEvent::NoteExpression {
        channel: 0,
        key: 57,
        kind: NoteExpressionKind::Tuning,
        value: ExpressionValue::new(12.0),
    });
    let (l, r) = p.run(0.5);
    assert!((peak_hz(&mono(&l, &r)) - 440.0).abs() < 4.0, "an octave up");
    // MPE: the zone on the master channel, a note on member channel 2
    // bent up a semitone (1/48 of the wheel).
    let mut p = play(&steady(&[(id::osc(0, id::POSITION), 0.0)]));
    for (cc, v) in [(101, 0), (100, 6), (6, 15)] {
        p.send(MidiEvent::ControlChange {
            channel: 0,
            controller: cc,
            value: v,
        });
    }
    p.send(MidiEvent::NoteOn {
        channel: 2,
        key: 69,
        velocity: 100,
    });
    p.send(MidiEvent::PitchBend {
        channel: 2,
        value: 8192 + 8192 / 48,
    });
    let (l, r) = p.run(0.5);
    let f = peak_hz(&mono(&l, &r));
    assert!((f - 466.2).abs() < 5.0, "{f}");
}

/// The cost of a voice (by hand: `cargo test -p faderframe-plugin-host
/// --release wavetable_cost -- --ignored --nocapture`).
#[test]
#[ignore]
fn wavetable_cost() {
    for (name, unison, warp) in [
        ("one copy", 1.0, warp::OFF),
        ("one copy, sync", 1.0, warp::SYNC),
        ("8 copies", 8.0, warp::OFF),
        ("2 × 8 copies, filter", 8.0, warp::BEND_UP),
    ] {
        let both = name.starts_with("2 ×");
        let mut p = play(&[
            (id::osc(0, id::UNISON), unison),
            (id::osc(0, id::WARP), warp as f64),
            (id::osc(0, id::WARP_AMOUNT), 0.5),
            (id::osc(1, id::UNISON), unison),
            (id::osc(1, id::LEVEL), if both { 0.7 } else { 0.0 }),
            (id::FILTER_TYPE, if both { 2.0 } else { 0.0 }),
            (id::SUSTAIN, 1.0),
            (id::POLYPHONY, 32.0),
        ]);
        for k in 0..16u8 {
            p.send(on(40 + 2 * k));
        }
        p.run(0.2);
        let seconds = 4.0;
        let t = std::time::Instant::now();
        p.run(seconds);
        let used = t.elapsed().as_secs_f64() / seconds;
        println!(
            "{name:24} {:6.3} % of a core per voice",
            100.0 * used / 16.0
        );
    }
    let _ = SR;
}
