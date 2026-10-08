//! Tempo through hit points: markers at hits a film editor cut become
//! beats of the steadiest tempo map, each still where it sounded; audio
//! stays where it sounds, MIDI follows the beats; one undo step.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ClipId, MarkerId, NoteId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Marker, MidiClip, MidiNote,
    Project, SourceSpec, StretchSettings, TrackKind,
};
use faderframe_session::hits::{HitRequest, HitSource};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use faderframe_timeline::hits::HitGrid;

#[test]
fn hits_land_on_beats_and_audio_stays_where_it_sounds() {
    let mut s = Session::new(Project::new("Cue", 48_000), None, EngineConfig::default()).unwrap();
    let secs = |s: &Session, q: MusicalTime| s.project().timeline.tempo.musical_to_seconds(q);
    let at = |s: &Session, t: f64| s.project().timeline.tempo.seconds_to_musical(t);
    // Cuts at uneven times, and a note that is no hit.
    let hits = [2.13, 4.71, 6.02, 9.38, 12.4];
    for (i, t) in hits.iter().enumerate() {
        let position = at(&s, *t);
        s.dispatch(Action::Edit(Command::AddMarker {
            marker: Marker {
                id: MarkerId(500 + i as u64),
                position,
                name: format!("Cut {}", i + 1),
            },
        }))
        .unwrap();
    }
    let note_at = at(&s, 7.5);
    s.dispatch(Action::Edit(Command::AddMarker {
        marker: Marker {
            id: MarkerId(600),
            position: note_at,
            name: "Door slam".into(),
        },
    }))
    .unwrap();
    // Dialogue (audio) at 3 s, a MIDI clip at bar 3.
    let a = s.add_track(TrackKind::Audio).unwrap();
    let source = AudioSourceId(700);
    s.dispatch(Action::Edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: source,
            name: "line".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::Sine {
                    frequency: 200.0,
                    seconds: 2.0,
                    amplitude: 0.1,
                },
            },
        }),
    }))
    .unwrap();
    let dialogue = ClipId(701);
    let dialogue_at = at(&s, 3.0);
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id: dialogue,
            track: a,
            name: "line".into(),
            color: None,
            start: dialogue_at,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: 0,
                length: 96_000,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
            }),
        }),
    }))
    .unwrap();
    let m = s.add_track(TrackKind::Midi).unwrap();
    let music = ClipId(702);
    let bar3 = MusicalTime::from_quarters(8.0);
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id: music,
            track: m,
            name: "score".into(),
            color: None,
            start: bar3,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: MusicalTime::from_quarters(4.0),
                notes: vec![MidiNote {
                    id: NoteId(703),
                    start: MusicalTime::ZERO,
                    length: MusicalTime::from_quarters(1.0),
                    key: 60,
                    velocity: 90,
                    channel: 0,
                    muted: false,
                }],
                ..MidiClip::default()
            }),
        }),
    }))
    .unwrap();
    let req = HitRequest {
        source: HitSource::Cuts,
        ..HitRequest::default()
    };
    let preview = s.preview_hit_tempo(&req).unwrap();
    assert_eq!(preview.hits, 5);
    assert!(
        preview.slowest >= 70.0 && preview.fastest <= 160.0,
        "{preview:?}"
    );
    let before = s.project().clone();
    s.dispatch(Action::TempoFromHits(req)).unwrap();
    let p = s.project();
    // Every cut on a beat, still where it sounded.
    for (i, t) in hits.iter().enumerate() {
        let m = p
            .markers
            .iter()
            .find(|m| m.id == MarkerId(500 + i as u64))
            .unwrap();
        let q = m.position.quarters();
        assert!((q - q.round()).abs() < 1e-6, "cut {i} at {q} quarters");
        assert!(
            (secs(&s, m.position) - t).abs() < 1e-3,
            "cut {i} sounds at {t}"
        );
    }
    // The other marker and the dialogue sound where they did.
    let slam = p.markers.iter().find(|m| m.id == MarkerId(600)).unwrap();
    assert!((secs(&s, slam.position) - 7.5).abs() < 1e-3);
    let line = p.clip(dialogue).unwrap();
    assert!((secs(&s, line.start) - 3.0).abs() < 1e-3);
    // The score follows the beats: still at bar 3.
    assert_eq!(p.clip(music).unwrap().start, bar3);
    assert!(
        p.timeline.tempo.points().len() > 2,
        "a tempo for each stretch"
    );
    // On bar lines, too.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().timeline, before.timeline);
    assert_eq!(s.project().markers, before.markers);
    let bars = HitRequest {
        source: HitSource::Cuts,
        settings: faderframe_timeline::hits::HitSettings {
            grid: HitGrid::Bars(4.0),
            min_bpm: 40.0,
            max_bpm: 240.0,
            ..Default::default()
        },
        keep_audio: true,
    };
    s.dispatch(Action::TempoFromHits(bars)).unwrap();
    for i in 0..hits.len() {
        let m = s
            .project()
            .markers
            .iter()
            .find(|m| m.id == MarkerId(500 + i as u64))
            .unwrap();
        let bars = m.position.quarters() / 4.0;
        assert!((bars - bars.round()).abs() < 1e-6, "{bars}");
    }
    // No markers of the kind: refused with a reason.
    assert!(
        s.dispatch(Action::TempoFromHits(HitRequest {
            source: HitSource::Range,
            ..HitRequest::default()
        }))
        .is_err()
    );
}
