#![allow(clippy::unwrap_used)]

use super::synth::{Syllable, phrase};
use super::*;

const RATE: f64 = 16_000.0;

/// A guide's syllables: consonants, vowels and notes changing, a pause
/// now and then.
fn guide_syllables() -> Vec<Syllable> {
    let mut out = Vec::new();
    let mut t = 0.3;
    for k in 0..14u32 {
        let consonant = [1u8, 0, 2, 0, 1, 2, 0][k as usize % 7];
        let length = 0.22 + 0.09 * f64::from((k * 7) % 5);
        out.push(Syllable {
            consonant,
            vowel: (k % 5) as u8,
            start: t,
            length,
            note: 57.0 + f64::from((k * 5) % 9),
        });
        t += length + if k % 4 == 3 { 0.25 } else { 0.06 };
    }
    out
}

/// The same syllables sung again: `offset` later, each start moved by its
/// own amount and its length changed (a double never quite matches), a
/// little sharp and another voice.
fn dub_of(guide: &[Syllable], offset: f64, wander: f64) -> Vec<Syllable> {
    guide
        .iter()
        .enumerate()
        .map(|(k, s)| {
            let jitter = wander * [0.6, -0.8, 0.3, 1.0, -0.4, -1.0, 0.8][k % 7];
            let scale = 1.0 + 3.0 * wander * [0.5, -1.0, 0.8, -0.3, 1.0, -0.6][k % 6];
            Syllable {
                start: s.start + offset + jitter,
                length: s.length * scale,
                note: s.note + 0.3,
                ..*s
            }
        })
        .collect()
}

/// Where a syllable's vowel starts.
fn vowel_onset(s: &Syllable) -> f64 {
    s.start
        + match s.consonant {
            1 => 0.07,
            2 => 0.02,
            _ => 0.0,
        }
}

fn render(syllables: &[Syllable], brightness: f64, seed: u64) -> Vec<f32> {
    phrase(syllables, 8.0, RATE, brightness, seed)
}

/// The errors (seconds) between where the map plays each dub syllable's
/// vowel onset and where the dub sang it.
fn onset_errors(map: &TimeMap, guide: &[Syllable], dub: &[Syllable]) -> Vec<f64> {
    guide
        .iter()
        .zip(dub)
        .map(|(g, d)| (map.at(vowel_onset(g)) - vowel_onset(d)).abs())
        .collect()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
fn a_dub_late_by_a_constant_offset_is_found_to_the_millisecond() {
    let guide = guide_syllables();
    let dub = dub_of(&guide, 0.037, 0.0);
    let map = align(
        &render(&guide, 1.0, 1),
        &render(&dub, 1.08, 2),
        RATE,
        &Settings {
            tightness: 0.5,
            ..Settings::default()
        },
    )
    .unwrap();
    let errors = onset_errors(&map, &guide, &dub);
    assert!(median(errors.clone()) < 0.0025, "{errors:?}");
    assert!(errors.iter().all(|e| *e < 0.004), "{errors:?}");
    assert!((map.largest_move() - 0.037).abs() < 0.012);
}

#[test]
fn a_tight_alignment_puts_every_syllable_where_the_guide_has_it() {
    let guide = guide_syllables();
    let dub = dub_of(&guide, 0.045, 0.025);
    let map = align(
        &render(&guide, 1.0, 1),
        &render(&dub, 1.08, 2),
        RATE,
        &Settings {
            tightness: 1.0,
            ..Settings::default()
        },
    )
    .unwrap();
    let errors = onset_errors(&map, &guide, &dub);
    assert!(median(errors.clone()) < 0.003, "{errors:?}");
    assert!(errors.iter().all(|e| *e < 0.008), "{errors:?}");
    // Strictly increasing, never more than twice as fast or slow.
    for w in map.dub.windows(2) {
        let speed = (w[1] - w[0]) / map.hop;
        assert!((0.49..=2.01).contains(&speed), "{speed}");
    }
}

#[test]
fn a_stretched_dub_is_followed_through_its_whole_length() {
    let guide = guide_syllables();
    // Sung 5 % slower throughout, starting 20 ms early.
    let dub: Vec<Syllable> = guide
        .iter()
        .map(|s| Syllable {
            start: 0.28 + (s.start - 0.3) * 1.05,
            length: s.length * 1.05,
            ..*s
        })
        .collect();
    let map = align(
        &render(&guide, 1.0, 1),
        &render(&dub, 0.95, 3),
        RATE,
        &Settings {
            tightness: 0.7,
            ..Settings::default()
        },
    )
    .unwrap();
    let errors = onset_errors(&map, &guide, &dub);
    assert!(median(errors.clone()) < 0.003, "{errors:?}");
    assert!(errors.iter().all(|e| *e < 0.005), "{errors:?}");
}

#[test]
fn a_harmony_a_fifth_above_aligns_as_well() {
    let guide = guide_syllables();
    let dub: Vec<Syllable> = dub_of(&guide, 0.03, 0.02)
        .into_iter()
        .map(|s| Syllable {
            note: s.note + 7.0,
            ..s
        })
        .collect();
    let map = align(
        &render(&guide, 1.0, 1),
        &render(&dub, 1.12, 4),
        RATE,
        &Settings {
            tightness: 0.9,
            ..Settings::default()
        },
    )
    .unwrap();
    let errors = onset_errors(&map, &guide, &dub);
    assert!(median(errors.clone()) < 0.004, "{errors:?}");
    assert!(errors.iter().all(|e| *e < 0.010), "{errors:?}");
}

#[test]
fn a_loose_alignment_moves_phrases_and_keeps_the_syllables_own_timing() {
    let guide = guide_syllables();
    let dub = dub_of(&guide, 0.06, 0.04);
    let settings = |tightness| Settings {
        tightness,
        ..Settings::default()
    };
    let (g, d) = (render(&guide, 1.0, 1), render(&dub, 1.08, 2));
    let loose = align(&g, &d, RATE, &settings(0.0)).unwrap();
    let tight = align(&g, &d, RATE, &settings(1.0)).unwrap();
    // The loose map's speed stays near 1 (within its 1.25 limit and well
    // inside it on average); the tight one follows each syllable.
    let wobble = |m: &TimeMap| {
        m.dub
            .windows(2)
            .map(|w| ((w[1] - w[0]) / m.hop - 1.0).abs())
            .sum::<f64>()
            / m.dub.len() as f64
    };
    assert!(wobble(&loose) < 0.5 * wobble(&tight));
    for w in loose.dub.windows(2) {
        let speed = (w[1] - w[0]) / loose.hop;
        assert!((0.79..=1.26).contains(&speed), "{speed}");
    }
    // The overall offset is taken all the same.
    let errors = onset_errors(&loose, &guide, &dub);
    assert!(median(errors) < 0.035);
}

#[test]
fn sibilants_play_at_their_own_speed() {
    let guide = guide_syllables();
    let dub = dub_of(&guide, 0.03, 0.04);
    let d = frames(&render(&dub, 1.08, 2), RATE);
    let runs = sibilants(&d);
    // Every "s" of the dub is found.
    let esses = dub.iter().filter(|s| s.consonant == 1).count();
    assert!(runs.len() >= esses, "{runs:?}");
    let g = frames(&render(&guide, 1.0, 1), RATE);
    let map = align_frames(
        &g,
        &d,
        &Settings {
            tightness: 1.0,
            protect_consonants: true,
            ..Settings::default()
        },
    )
    .unwrap();
    // Inside each sibilant, the map moves one dub frame per guide frame.
    for &(a, b) in &runs {
        let (ta, tb) = (a as f64 * HOP_SECONDS, b as f64 * HOP_SECONDS);
        let i = map.dub.iter().position(|v| *v >= ta).unwrap();
        let k = map.dub.iter().position(|v| *v >= tb - 2.0 * HOP_SECONDS);
        let Some(k) = k else { continue };
        for w in map.dub[i..k.max(i + 1)].windows(2) {
            let speed = (w[1] - w[0]) / map.hop;
            assert!((speed - 1.0).abs() < 0.02, "{speed} in {a}..{b}");
        }
    }
}

#[test]
fn silence_holds_nothing_to_align_by() {
    let quiet = vec![0.0f32; 16_000];
    let guide = render(&guide_syllables(), 1.0, 1);
    assert!(align(&guide, &quiet, RATE, &Settings::default()).is_none());
    assert!(align(&quiet, &guide, RATE, &Settings::default()).is_none());
}

#[test]
fn the_map_is_simplified_within_its_tolerance() {
    let map = TimeMap {
        hop: 0.005,
        dub: (0..400)
            .map(|i| {
                let t = i as f64 * 0.005;
                t + 0.03 * (t * 3.0).sin()
            })
            .collect(),
    };
    let points = map.points(0.0005);
    assert!(points.len() < 80, "{}", points.len());
    assert_eq!(points[0].0, 0.0);
    assert_eq!(points.last().unwrap().0, 399.0 * 0.005);
    for i in 0..400 {
        let t = i as f64 * 0.005;
        let k = points
            .partition_point(|p| p.0 <= t)
            .clamp(1, points.len() - 1);
        let (a, b) = (points[k - 1], points[k]);
        let line = a.1 + (b.1 - a.1) * (t - a.0) / (b.0 - a.0);
        assert!((line - map.dub[i]).abs() <= 0.0005 + 1e-12);
    }
    assert!((map.at(-0.1) - (map.dub[0] - 0.1)).abs() < 1e-12);
}
