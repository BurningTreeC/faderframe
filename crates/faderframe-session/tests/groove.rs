//! Quantize and Humanize of whole clips: audio by its transients, MIDI by
//! its notes, with strength, swing and humanize amounts; one undo step.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ClipId, NoteId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::midi_ops::QuantizeSettings;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, MidiClip, MidiNote, Project,
    SourceSpec, StretchSettings, TrackKind,
};
use faderframe_session::groove::HumanizeSettings;
use faderframe_session::{Action, EditFlag, Session};
use faderframe_timeline::{GridDivision, MusicalTime};
use std::collections::HashMap;

const SR: u32 = 48_000;

fn q(x: f64) -> MusicalTime {
    MusicalTime::from_quarters(x)
}

/// A 120 BPM session with a two-bar drum loop clip and an audio track.
fn session() -> (Session, TrackId, ClipId) {
    let mut s = Session::new(Project::new("Groove", SR), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    let source = AudioSourceId(9000);
    s.edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: source,
            name: "loop".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars: 4,
                    seed: 3,
                },
            },
        }),
    })
    .unwrap();
    let clip = ClipId(10_000);
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id: clip,
            track: t,
            name: "loop".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: 0,
                length: 4 * SR as i64,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
            }),
        }),
    })
    .unwrap();
    s.dispatch(Action::SetEditFlag(EditFlag::ShowTransients, true))
        .unwrap();
    for _ in 0..500 {
        s.tick(0.01);
        if !s.analysing_transients() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!s.analysing_transients());
    (s, t, clip)
}

fn audio(s: &Session, c: ClipId) -> AudioClip {
    match &s.project().clip(c).unwrap().content {
        ClipContent::Audio(a) => a.clone(),
        _ => panic!("audio"),
    }
}

/// Source frame → where it plays (clip-relative frames).
fn markers(s: &Session, c: ClipId) -> HashMap<i64, i64> {
    audio(s, c)
        .warp
        .unwrap()
        .markers
        .iter()
        .map(|m| (m.source, m.at))
        .collect()
}

fn midi_clip(s: &mut Session, starts: &[f64]) -> ClipId {
    let t = s.add_track(TrackKind::Instrument).unwrap();
    let clip = ClipId(20_000);
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id: clip,
            track: t,
            name: "notes".into(),
            color: None,
            start: q(4.0),
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: q(8.0),
                notes: starts
                    .iter()
                    .enumerate()
                    .map(|(i, &st)| MidiNote {
                        id: NoteId(100 + i as u64),
                        start: q(st),
                        length: q(0.25),
                        key: 60,
                        velocity: 100,
                        channel: 0,
                        muted: false,
                    })
                    .collect(),
                ..MidiClip::default()
            }),
        }),
    })
    .unwrap();
    clip
}

fn notes(s: &Session, c: ClipId) -> Vec<MidiNote> {
    let mut n = s
        .project()
        .clip(c)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .clone();
    n.sort_by_key(|n| n.id);
    n
}

#[test]
fn humanize_then_half_strength_quantize_moves_transients() {
    let (mut s, _, c) = session();
    s.dispatch(Action::SetHumanize(HumanizeSettings {
        timing_ms: 20.0,
        velocity: 0,
    }))
    .unwrap();
    s.dispatch(Action::HumanizeClips(vec![c])).unwrap();
    let human = markers(&s, c);
    assert!(human.len() >= 6, "{human:?}");
    let spread = (0.020 * SR as f64) as i64;
    let mut moved = 0;
    for (&src, &at) in &human {
        assert!((at - src).abs() <= spread, "{src} → {at}");
        moved += usize::from(at != src);
    }
    assert!(moved * 2 > human.len(), "most hits move");
    // One undo step back to the unwarped clip.
    s.dispatch(Action::Undo).unwrap();
    assert!(audio(&s, c).warp.is_none());
    s.dispatch(Action::Redo).unwrap();

    // Half way to the sixteenths from where they play now.
    s.dispatch(Action::SetGrid(GridDivision::Note(16))).unwrap();
    s.dispatch(Action::SetQuantize(QuantizeSettings {
        strength: 0.5,
        ..QuantizeSettings::default()
    }))
    .unwrap();
    s.dispatch(Action::QuantizeClips(vec![c])).unwrap();
    let half = markers(&s, c);
    let sixteenth = SR as i64 / 8;
    let mut checked = 0;
    for (src, at) in &half {
        let Some(&h) = human.get(src) else { continue };
        let grid = ((h as f64 / sixteenth as f64).round() as i64) * sixteenth;
        assert!(
            (2 * at - (h + grid)).abs() <= 4,
            "{src}: humanized {h}, grid {grid}, now {at}"
        );
        checked += 1;
    }
    assert!(checked >= 6);
    // Full strength: on the grid.
    s.dispatch(Action::SetQuantize(QuantizeSettings::default()))
        .unwrap();
    s.dispatch(Action::QuantizeClips(vec![c])).unwrap();
    assert!(markers(&s, c).values().all(|at| at % sixteenth == 0));
}

#[test]
fn midi_clips_swing_and_humanize() {
    let (mut s, _, audio_clip) = session();
    let eighths: Vec<f64> = (0..8).map(|i| i as f64 * 0.5 + 0.03).collect();
    let c = midi_clip(&mut s, &eighths);
    s.dispatch(Action::SetGrid(GridDivision::Note(8))).unwrap();
    s.dispatch(Action::SetQuantize(QuantizeSettings {
        swing: 0.5,
        ..QuantizeSettings::default()
    }))
    .unwrap();
    // Audio and MIDI together: one undo step.
    s.dispatch(Action::QuantizeClips(vec![audio_clip, c]))
        .unwrap();
    assert!(audio(&s, audio_clip).warp.is_some());
    let n = notes(&s, c);
    for (i, note) in n.iter().enumerate() {
        // Every second eighth comes a sixteenth's half later (50 % swing).
        let expected = i as f64 * 0.5 + if i % 2 == 1 { 0.125 } else { 0.0 };
        assert!(
            (note.start.quarters() - expected).abs() < 1e-3,
            "{i}: {}",
            note.start.quarters()
        );
    }
    s.dispatch(Action::Undo).unwrap();
    assert!(audio(&s, audio_clip).warp.is_none());
    assert!((notes(&s, c)[0].start.quarters() - 0.03).abs() < 1e-3);

    // Humanize: ±10 ms at 120 BPM is ±0.02 quarters; velocity ±8.
    s.dispatch(Action::SetHumanize(HumanizeSettings {
        timing_ms: 10.0,
        velocity: 8,
    }))
    .unwrap();
    let before = notes(&s, c);
    s.dispatch(Action::HumanizeClips(vec![c])).unwrap();
    let after = notes(&s, c);
    let mut changed = 0;
    for (a, b) in before.iter().zip(&after) {
        assert!((a.start.quarters() - b.start.quarters()).abs() <= 0.0201);
        assert!((a.velocity as i32 - b.velocity as i32).abs() <= 8);
        changed += usize::from(a != b);
    }
    assert!(changed >= 4, "{changed}");
}
