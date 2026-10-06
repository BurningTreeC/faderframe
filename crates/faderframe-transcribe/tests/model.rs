//! The converted model gives what onnxruntime gives for the original
//! (`reference.json`, made by `scripts/basic_pitch_model.py`), and notes
//! come out of it as basic-pitch makes them.
#![allow(clippy::unwrap_used)]

use faderframe_transcribe::{Model, RATE, Settings, notes};

/// The converter's test signal: C4 E4 G4 for a second, then A3.
fn signal() -> Vec<f32> {
    (0..43_844)
        .map(|i| {
            let t = i as f64 / RATE;
            let keys: &[u8] = if t < 1.0 { &[60, 64, 67] } else { &[57] };
            let v: f64 = keys
                .iter()
                .map(|k| {
                    let f = 440.0 * 2f64.powf((f64::from(*k) - 69.0) / 12.0);
                    (1..=5)
                        .map(|h| (std::f64::consts::TAU * f * h as f64 * t).sin() / h as f64)
                        .sum::<f64>()
                })
                .sum();
            (0.1 * v) as f32
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct Reference {
    shape: Vec<usize>,
    frame_sums: Vec<f64>,
    bin_sums: Vec<f64>,
    samples: Vec<(usize, f64)>,
}

#[test]
fn the_model_matches_the_original() {
    let reference: std::collections::HashMap<String, Reference> =
        serde_json::from_str(include_str!("reference.json")).unwrap();
    let model = Model::load().unwrap();
    let [note, onset, contour] = model.window(&signal()).unwrap();
    for (suffix, got) in [(":1", &note), (":2", &onset), (":0", &contour)] {
        let (name, r) = reference.iter().find(|(n, _)| n.ends_with(suffix)).unwrap();
        let bins = r.shape[1];
        assert_eq!(got.len(), r.shape[0] * bins, "{name}");
        let mut worst = 0.0f64;
        for (i, want) in &r.samples {
            worst = worst.max((f64::from(got[*i]) - want).abs());
        }
        assert!(worst < 1e-3, "{name}: sampled values off by {worst}");
        for (f, want) in r.frame_sums.iter().enumerate() {
            let sum: f64 = got[f * bins..(f + 1) * bins]
                .iter()
                .map(|v| f64::from(*v))
                .sum();
            assert!(
                (sum - want).abs() < 1e-2 + want.abs() * 1e-3,
                "{name} frame {f}: {sum} vs {want}"
            );
        }
        for (b, want) in r.bin_sums.iter().enumerate() {
            let sum: f64 = (0..r.shape[0]).map(|f| f64::from(got[f * bins + b])).sum();
            assert!(
                (sum - want).abs() < 1e-2 + want.abs() * 1e-3,
                "{name} bin {b}: {sum} vs {want}"
            );
        }
    }
}

#[test]
fn a_chord_then_a_note_become_notes() {
    let model = Model::load().unwrap();
    let act = model.activations(&signal(), 2).unwrap();
    let found = notes(&act, &Settings::default());
    // The notes played, strongly; these tones' loud harmonics also give
    // a few weak octave notes, as basic-pitch hears them.
    let keys_at = |t: f64| {
        let mut k: Vec<u8> = found
            .iter()
            .filter(|n| n.start <= t && t < n.end && n.amplitude > 0.6)
            .map(|n| n.key)
            .collect();
        k.sort_unstable();
        k
    };
    assert_eq!(keys_at(0.5), [60, 64, 67], "{found:?}");
    assert_eq!(keys_at(1.5), [57], "{found:?}");
    for n in found.iter().filter(|n| n.amplitude <= 0.6) {
        assert!(n.amplitude < 0.45, "{n:?}");
        assert!(
            found
                .iter()
                .any(|m| m.amplitude > 0.6 && (n.key - m.key) % 12 == 0
                    || [7, 19, 24, 28].contains(&(n.key as i32 - m.key as i32))),
            "{n:?} is no harmonic"
        );
    }
    // Starts near where they were played.
    let c = found.iter().find(|n| n.key == 60).unwrap();
    assert!(c.start < 0.1, "{c:?}");
    let a = found.iter().find(|n| n.key == 57).unwrap();
    assert!((a.start - 1.0).abs() < 0.06, "{a:?}");
}
