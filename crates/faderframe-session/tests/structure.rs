//! Song structure from a jam: a multitrack recording's parts found and
//! laid on the section lane, on the project's bars, in one step.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_project::Project;
use faderframe_session::{Action, SelectMode, Session};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// 120 BPM, 4/4.
const BAR: f64 = 2.0;

fn hz(midi: i32) -> f64 {
    440.0 * 2f64.powf((f64::from(midi) - 69.0) / 12.0)
}

/// A jam in two recordings — chords; drums and bass — of Verse, Chorus,
/// Verse, Chorus (8 bars each), the chorus louder, brighter, with hats.
fn jam() -> (Vec<f32>, Vec<f32>) {
    let am = [57, 60, 64];
    let f = [53, 57, 60];
    let c = [48, 52, 55];
    let g = [55, 59, 62];
    let parts: [(&[[i32; 3]; 4], bool); 4] = [
        (&[am, f, c, g], false),
        (&[f, g, c, am], true),
        (&[am, f, c, g], false),
        (&[f, g, c, am], true),
    ];
    let bar = (BAR * f64::from(SR)) as usize;
    let beat = bar / 4;
    let (mut chords, mut rhythm) = (Vec::new(), Vec::new());
    let mut seed = 5u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from((seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0)
    };
    for (progression, chorus) in parts {
        for b in 0..8 {
            let chord = progression[b % 4];
            for i in 0..bar {
                let t = chords.len() as f64 / f64::from(SR);
                let harmonics = if chorus { 5 } else { 2 };
                let mut v = 0.0;
                for &n in &chord {
                    for h in 1..=harmonics {
                        v += (std::f64::consts::TAU * hz(n) * h as f64 * t).sin() / h as f64;
                    }
                }
                chords.push((v * if chorus { 0.09 } else { 0.06 }) as f32);
                let mut r = 0.15 * (std::f64::consts::TAU * hz(chord[0] - 24) * t).sin();
                let k = i % (beat * 2);
                if k < (0.12 * f64::from(SR)) as usize {
                    let kt = k as f64 / f64::from(SR);
                    r += 0.5
                        * (std::f64::consts::TAU * (50.0 + 70.0 * (-kt * 30.0).exp()) * kt).sin()
                        * (-kt * 18.0).exp();
                }
                if chorus && i % (beat / 2) < 1500 {
                    r += 0.12 * noise() * (-((i % (beat / 2)) as f64 / 1500.0) * 4.0).exp();
                }
                rhythm.push((r * if chorus { 1.0 } else { 0.8 }) as f32);
            }
        }
    }
    (chords, rhythm)
}

#[test]
fn a_multitrack_jam_gets_its_sections() {
    let dir = std::env::temp_dir().join(format!("ff-session-structure-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let (chords, rhythm) = jam();
    let (a, b) = (dir.join("Keys.wav"), dir.join("Drums and Bass.wav"));
    write_wav(&a, &[chords], SR, WavFormat::Float32, false).unwrap();
    write_wav(&b, &[rhythm], SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new("Jam", SR), None, EngineConfig::default()).unwrap();
    for file in [a, b] {
        s.dispatch(Action::ImportFiles {
            files: vec![file],
            track: None,
            at: MusicalTime::ZERO,
        })
        .unwrap();
    }
    s.wait_for_imports();
    let clips: Vec<_> = s.project().clips.keys().copied().collect();
    assert_eq!(clips.len(), 2);
    s.dispatch(Action::SelectClips {
        clips: clips.clone(),
        mode: SelectMode::Replace,
    })
    .unwrap();
    // A section there already goes; one elsewhere stays.
    s.dispatch(Action::AddSection {
        start: MusicalTime::from_quarters(8.0),
        end: MusicalTime::from_quarters(16.0),
    })
    .unwrap();
    s.dispatch(Action::AddSection {
        start: MusicalTime::from_quarters(200.0),
        end: MusicalTime::from_quarters(208.0),
    })
    .unwrap();
    let steps = s.history_steps().0.len();
    s.dispatch(Action::SongStructure { clips }).unwrap();
    assert!(s.finding_structure());
    s.wait_for_structure();
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    let mut sections: Vec<_> = s.project().sections.clone();
    sections.sort_by_key(|x| x.start);
    let shown: Vec<(f64, f64, &str)> = sections
        .iter()
        .map(|x| {
            (
                x.start.quarters() / 4.0,
                x.end.quarters() / 4.0,
                x.name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (0.0, 8.0, "Verse 1"),
            (8.0, 16.0, "Chorus 1"),
            (16.0, 24.0, "Verse 2"),
            (24.0, 32.0, "Chorus 2"),
            (50.0, 52.0, "Verse"),
        ],
        "{shown:?}"
    );
    // The verses look alike, the choruses too.
    assert_eq!(sections[0].color, sections[2].color);
    assert_eq!(sections[1].color, sections[3].color);
    assert_ne!(sections[0].color, sections[1].color);
    // One undo: the sections as they were.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().sections.len(), 2);
}
