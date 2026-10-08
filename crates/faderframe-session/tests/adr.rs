//! ADR: cues from the transcript, the beeps' track, what goes over the
//! picture before and during a line, a rehearsal that stops by itself, and
//! takes rated.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{AudioSourceId, ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::lyrics::LyricLine;
use faderframe_project::{Clip, ClipContent, Command, Project, Take, TakeFolder, TrackKind};
use faderframe_session::adr::{AdrOp, BEEPS_TRACK};
use faderframe_session::{Action, AudioPreferences, Session};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;
const S: i64 = SR as i64;

fn at(s: &Session, samples: i64) -> MusicalTime {
    s.project().timeline.to_musical(samples, SR as f64)
}

/// A session with a dialogue track and three transcribed lines.
fn setup() -> (Session, TrackId) {
    let mut s = Session::new(Project::new("Scene 4", SR), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    let lines = [
        (5, 7, "Where were you?"),
        (8, 9, "Out."),
        (12, 15, "All night?"),
    ]
    .map(|(a, b, text)| LyricLine {
        start: at(&s, a * S),
        end: at(&s, b * S),
        text: text.into(),
    })
    .to_vec();
    s.dispatch(Action::Edit(Command::SetLyrics { lyrics: lines }))
        .unwrap();
    (s, t)
}

#[test]
fn cues_from_the_transcript_and_their_beeps() {
    let (mut s, t) = setup();
    s.dispatch(Action::Adr(AdrOp::FromTranscript { track: Some(t) }))
        .unwrap();
    let cues = &s.adr().cues;
    assert_eq!(cues.len(), 3);
    assert_eq!(
        cues.iter().map(|c| c.number.as_str()).collect::<Vec<_>>(),
        ["1", "2", "3"]
    );
    assert_eq!(cues[1].text, "Out.");
    assert_eq!(cues[2].track, Some(t));
    // Lines a cue covers are not cued again.
    s.dispatch(Action::Adr(AdrOp::FromTranscript { track: Some(t) }))
        .unwrap();
    assert_eq!(s.adr().cues.len(), 3);
    // Three beeps a second apart before each cue, on their own track; made
    // again, not added to.
    for _ in 0..2 {
        s.dispatch(Action::Adr(AdrOp::MakeBeeps)).unwrap();
    }
    let p = s.project();
    let beeps = p.tracks.iter().find(|t| t.name == BEEPS_TRACK).unwrap();
    assert_eq!(beeps.clips.len(), 9);
    let mut starts: Vec<i64> = beeps
        .clips
        .iter()
        .map(|c| p.timeline.to_samples(p.clips[c].start, SR as f64))
        .collect();
    starts.sort();
    assert_eq!(&starts[..3], &[2 * S, 3 * S, 4 * S]);
    // One undo step each.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert!(!s.project().tracks.iter().any(|t| t.name == BEEPS_TRACK));
}

#[test]
fn the_picture_cues_the_line_and_a_rehearsal_stops_by_itself() {
    let (mut s, t) = setup();
    s.dispatch(Action::Adr(AdrOp::FromTranscript { track: Some(t) }))
        .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    let cue = s.adr().cues[1].clone(); // 8–9 s
    s.dispatch(Action::Adr(AdrOp::Run {
        cue: cue.id,
        record: false,
    }))
    .unwrap();
    assert_eq!(s.adr_running(), Some((cue.id, false)));
    // A second before the line: the streamer halfway, one beep to come.
    let o = s.adr_overlay(7 * S).unwrap();
    assert_eq!(o.number, "2");
    assert!((o.streamer.unwrap() - 0.5).abs() < 1e-3);
    assert_eq!(o.beeps_left, Some(1));
    assert!(!o.punch && !o.speaking);
    // The line starts: the punch.
    let o = s.adr_overlay(8 * S).unwrap();
    assert!(o.punch && o.speaking && o.streamer.is_none());
    assert!(
        s.adr_overlay(10 * S + S / 2)
            .is_none_or(|o| o.number != "2")
    );
    // It plays from the pre-roll (3 beeps + 1 s) and stops a second
    // after the line.
    let start = Instant::now();
    let mut from = None;
    while start.elapsed() < Duration::from_secs(12) {
        s.tick(0.01);
        if s.transport().playing && from.is_none() {
            from = Some(s.transport().position);
        }
        if from.is_some() && s.adr_running().is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let from = from.unwrap();
    assert!((from - 4 * S).abs() < S / 4, "pre-roll from {from}");
    assert!(s.adr_running().is_none(), "stopped by itself");
    let stopped = Instant::now();
    while s.transport().playing && stopped.elapsed() < Duration::from_secs(1) {
        s.tick(0.01);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!s.transport().playing);
    s.stop_audio();
}

#[test]
fn takes_are_rated() {
    let (mut s, t) = setup();
    s.dispatch(Action::Adr(AdrOp::FromTranscript { track: Some(t) }))
        .unwrap();
    let cue = s.adr().cues[0].clone(); // 5–7 s
    let mut folder = TakeFolder::new(2 * S);
    for i in 0..3 {
        s.dispatch(Action::Edit(Command::AddSource {
            source: Box::new(faderframe_project::AudioSource {
                id: AudioSourceId(500 + i),
                name: format!("take {i}"),
                spec: faderframe_project::SourceSpec::Generated {
                    generator: faderframe_audio_files::GeneratorSpec::Silence {
                        seconds: 2.0,
                        channels: 1,
                    },
                },
            }),
        }))
        .unwrap();
        folder.add_take(Take {
            name: format!("Take {}", i + 1),
            source: AudioSourceId(500 + i),
            source_offset: 0,
            start: 0,
            end: 2 * S,
            gain_db: 0.0,
            rating: 0,
        });
    }
    let clip = ClipId(9_000);
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id: clip,
            track: t,
            name: "ADR 1".into(),
            color: None,
            start: cue.start,
            muted: false,
            content: ClipContent::Takes(folder),
        }),
    }))
    .unwrap();
    assert_eq!(s.adr_takes(cue.id).len(), 3);
    s.dispatch(Action::Adr(AdrOp::RateTake {
        clip,
        take: 1,
        rating: 4,
    }))
    .unwrap();
    let takes = s.adr_takes(cue.id);
    assert_eq!(takes[1].3, 4);
    assert_eq!(takes[0].3, 0);
}
