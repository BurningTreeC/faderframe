//! Timing alignment of doubles: a dub sung late and loose against a lead
//! is warped onto it in one step, its notes moved toward the lead's pitch
//! when asked, and the step undoes.
#![allow(clippy::unwrap_used)]

use faderframe_analysis::timing_align::synth::{Syllable, phrase};
use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::{ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{AudioClip, Project};
use faderframe_session::timing_align::TimingAlignSettings;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
const SECONDS: f64 = 7.0;

/// The lead's syllables.
fn lead() -> Vec<Syllable> {
    let mut out = Vec::new();
    let mut t = 0.4;
    for k in 0..12u32 {
        let length = 0.24 + 0.08 * f64::from((k * 3) % 4);
        out.push(Syllable {
            consonant: [1u8, 0, 2, 0][k as usize % 4],
            vowel: (k % 5) as u8,
            start: t,
            length,
            note: 60.0 + f64::from((k * 5) % 7),
        });
        t += length + if k % 4 == 3 { 0.3 } else { 0.07 };
    }
    out
}

/// The double: 50 ms late, each syllable off by its own amount, a little
/// longer or shorter, another voice `sharp` semitones off.
fn double(sharp: f64) -> Vec<Syllable> {
    lead()
        .iter()
        .enumerate()
        .map(|(k, s)| Syllable {
            start: s.start + 0.05 + 0.025 * [0.6, -0.8, 0.3, 1.0, -0.4, -1.0][k % 6],
            length: s.length * (1.0 + 0.06 * [0.5, -1.0, 0.8, -0.3][k % 4]),
            note: s.note + sharp,
            ..*s
        })
        .collect()
}

fn vowel_onset(s: &Syllable) -> f64 {
    s.start
        + match s.consonant {
            1 => 0.07,
            2 => 0.02,
            _ => 0.0,
        }
}

/// A session with the lead and the double on tracks of their own.
fn session(name: &str, sharp: f64) -> (Session, TrackId, ClipId) {
    let dir = std::env::temp_dir().join(format!("ff-align-timing-{name}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let rate = f64::from(SR);
    let (a, b) = (dir.join("Lead.wav"), dir.join("Double.wav"));
    write_wav(
        &a,
        &[phrase(&lead(), SECONDS, rate, 1.0, 1)],
        SR,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    write_wav(
        &b,
        &[phrase(&double(sharp), SECONDS, rate, 1.1, 2)],
        SR,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    let mut s = Session::new(Project::new("Doubles", SR), None, EngineConfig::default()).unwrap();
    for file in [a, b] {
        s.dispatch(Action::ImportFiles {
            files: vec![file],
            track: None,
            at: MusicalTime::ZERO,
        })
        .unwrap();
    }
    s.wait_for_imports();
    let p = s.project();
    let lead = p.tracks.iter().find(|t| t.name == "Lead").unwrap().id;
    let dub = p.tracks.iter().find(|t| t.name == "Double").unwrap().id;
    let clip = p.clips_of(dub)[0].id;
    (s, lead, clip)
}

fn audio(s: &Session, clip: ClipId) -> AudioClip {
    s.project().clip(clip).unwrap().as_audio().unwrap().clone()
}

/// How far (ms) each double syllable's vowel onset now plays from the
/// lead's.
fn errors_ms(a: &AudioClip) -> Vec<f64> {
    let rate = f64::from(SR);
    let warp = a.warp.as_ref().unwrap();
    lead()
        .iter()
        .zip(double(0.0))
        .map(|(g, d)| {
            let source = (vowel_onset(&d) * rate).round() as i64;
            let out = warp.output_of(a.source_offset, a.length, source);
            (out as f64 - vowel_onset(g) * rate).abs() / rate * 1000.0
        })
        .collect()
}

#[test]
fn a_double_is_warped_onto_the_lead_in_one_step() {
    let (mut s, lead, clip) = session("timing", 0.0);
    let before = audio(&s, clip);
    let steps = s.history_steps().0.len();
    s.dispatch(Action::AlignTiming {
        guide: lead,
        dubs: vec![clip],
        tightness: 1.0,
    })
    .unwrap();
    assert!(s.aligning_timing());
    s.wait_for_timing_alignment();
    assert!(!s.aligning_timing());
    let (undo, _) = s.history_steps();
    assert_eq!(undo.len(), steps + 1, "one step");
    assert_eq!(undo.last().unwrap(), "Align Timing");
    let after = audio(&s, clip);
    assert!(after.warp.is_some());
    assert!(after.pitch.is_none(), "pitch is left alone unless asked");
    let errors = errors_ms(&after);
    let mut sorted = errors.clone();
    sorted.sort_by(f64::total_cmp);
    assert!(sorted[sorted.len() / 2] < 4.0, "{errors:?}");
    assert!(errors.iter().all(|e| *e < 10.0), "{errors:?}");
    // The clip keeps its place and length.
    assert_eq!(after.length, before.length);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(audio(&s, clip), before);
}

#[test]
fn notes_move_toward_the_leads_pitch_when_asked() {
    let (mut s, lead, clip) = session("pitch", 0.4);
    s.dispatch(Action::SetTimingAlign(TimingAlignSettings {
        pitch: 1.0,
        ..TimingAlignSettings::default()
    }))
    .unwrap();
    s.dispatch(Action::AlignTiming {
        guide: lead,
        dubs: vec![clip],
        tightness: 0.8,
    })
    .unwrap();
    s.wait_for_timing_alignment();
    let a = audio(&s, clip);
    let edit = a.pitch.as_ref().unwrap();
    assert!(edit.polyphonic.is_none(), "a melodic edit, played live");
    let moved: Vec<f32> = edit
        .notes
        .iter()
        .filter(|n| n.shift != 0.0)
        .map(|n| n.shift)
        .collect();
    assert!(moved.len() >= edit.notes.len() * 2 / 3, "{moved:?}");
    for m in &moved {
        assert!((m + 0.4).abs() < 0.15, "{moved:?}");
    }
    assert!(a.warp.is_some());
}

#[test]
fn the_clip_menu_offers_the_other_tracks_and_the_settings() {
    let (mut s, lead, clip) = session("menu", 0.0);
    let menu = s.align_timing_menu(&[clip]);
    let to_lead = menu
        .iter()
        .find(|e| e.label == "To ‘Lead’")
        .and_then(|e| e.action.clone())
        .unwrap();
    assert_eq!(
        to_lead,
        Action::AlignTiming {
            guide: lead,
            dubs: vec![clip],
            tightness: TimingAlignSettings::default().tightness,
        }
    );
    assert!(!menu.iter().any(|e| e.label == "To ‘Double’"));
    // Tight chosen there is what the next alignment takes.
    let tight = menu
        .iter()
        .find(|e| e.label == "Tight")
        .and_then(|e| e.action.clone())
        .unwrap();
    s.dispatch(tight).unwrap();
    assert_eq!(s.timing_align_settings().tightness, 1.0);
    let menu = s.align_timing_menu(&[clip]);
    assert_eq!(
        menu.iter().find(|e| e.label == "Tight").unwrap().checked,
        Some(true)
    );
    let protect = menu
        .iter()
        .find(|e| e.label == "Protect Consonants")
        .unwrap();
    assert_eq!(protect.checked, Some(true));
    s.dispatch(protect.action.clone().unwrap()).unwrap();
    assert!(!s.timing_align_settings().protect_consonants);
    // Settings are not steps.
    assert!(s.history_steps().0.iter().all(|l| l != "Align Timing"));
}

#[test]
fn nothing_to_align_to_is_said() {
    let (mut s, lead, clip) = session("refused", 0.0);
    // A dub on the guide's own track is no dub.
    let own = s.project().clips_of(lead)[0].id;
    assert!(
        s.dispatch(Action::AlignTiming {
            guide: lead,
            dubs: vec![own],
            tightness: 0.5,
        })
        .is_err()
    );
    // A guide without audio.
    let before: Vec<TrackId> = s.project().tracks.iter().map(|t| t.id).collect();
    s.dispatch(Action::AddTrack(faderframe_project::TrackKind::Audio))
        .unwrap();
    let empty = s
        .project()
        .tracks
        .iter()
        .find(|t| !before.contains(&t.id))
        .unwrap()
        .id;
    assert!(
        s.dispatch(Action::AlignTiming {
            guide: empty,
            dubs: vec![clip],
            tightness: 0.5,
        })
        .is_err()
    );
    assert!(!s.aligning_timing());
}
