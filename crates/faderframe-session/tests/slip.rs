//! Slip editing (issue #9): the audio or MIDI inside clips moves, the
//! clips keep their place, length, fades and gain.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ClipId, NoteId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, MidiClip, MidiNote, Project,
    SourceSpec, StretchSettings, TakeFolder, TrackKind, Warp, WarpMarker,
};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
const BAR: i64 = 2 * SR as i64;
/// Ticks of a bar (four quarters).
fn bars(n: f64) -> i64 {
    MusicalTime::from_quarters(4.0 * n).ticks()
}

fn setup() -> (Session, TrackId, TrackId, AudioSourceId) {
    let mut s = Session::new(Project::new("Slip", SR), None, EngineConfig::default()).unwrap();
    let audio = s.add_track(TrackKind::Audio).unwrap();
    let keys = s.add_track(TrackKind::Instrument).unwrap();
    let source = AudioSourceId(9000);
    s.dispatch(Action::Edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: source,
            name: "loop".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars: 8,
                    seed: 2,
                },
            },
        }),
    }))
    .unwrap();
    (s, audio, keys, source)
}

fn audio(source: AudioSourceId, offset_bars: f64, length_bars: f64) -> AudioClip {
    AudioClip {
        source,
        source_offset: (offset_bars * BAR as f64) as i64,
        length: (length_bars * BAR as f64) as i64,
        gain_db: -6.0,
        fades: ClipFades {
            fade_in: 2_400,
            fade_out: 4_800,
            ..ClipFades::default()
        },
        stretch: StretchSettings::Off,
        reversed: false,
        warp: None,
        pitch: None,
        effects: None,
        spectral: None,
    }
}

fn add(s: &mut Session, track: TrackId, start_bars: f64, content: ClipContent) -> ClipId {
    let id = ClipId(30_000 + s.project().clips.len() as u64);
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id,
            track,
            name: "c".into(),
            color: None,
            start: MusicalTime::from_quarters(4.0 * start_bars),
            muted: false,
            content,
        }),
    }))
    .unwrap();
    id
}

fn clip(s: &Session, id: ClipId) -> Clip {
    s.project().clip(id).unwrap().clone()
}

/// A drag as the arranger sends it: steps of one gesture, then its end.
fn drag(s: &mut Session, clips: &[ClipId], steps: &[f64]) {
    s.dispatch(Action::BeginGesture("Slip Clip".into()))
        .unwrap();
    for by in steps {
        s.dispatch(Action::SlipClips {
            clips: clips.to_vec(),
            by: bars(*by),
        })
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
}

#[test]
fn the_audio_moves_inside_the_clip_and_the_clip_stays() {
    let (mut s, t, _, src) = setup();
    let id = add(&mut s, t, 2.0, ClipContent::Audio(audio(src, 2.0, 2.0)));
    let before = clip(&s, id);
    // The audio a bar later: the clip plays from a bar earlier in the
    // source.
    drag(&mut s, &[id], &[0.25, 0.75, 1.0]);
    let after = clip(&s, id);
    let (a0, a1) = (before.as_audio().unwrap(), after.as_audio().unwrap());
    assert_eq!(a1.source_offset, BAR);
    assert_eq!(after.start, before.start, "the clip keeps its place");
    assert_eq!(
        (a1.length, a1.fades, a1.gain_db),
        (a0.length, a0.fades, a0.gain_db),
        "its length, fades and gain"
    );
    assert_eq!(s.history().undo_label(), Some("Slip Clip"));
    // One undo step, back exactly; redo again.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(clip(&s, id), before);
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(clip(&s, id), after);
    // Dragged there and back: as it was.
    drag(&mut s, &[id], &[-0.5, 0.3, 0.0]);
    assert_eq!(clip(&s, id), after);
    // The source ends where it ends: the drag stops there.
    drag(&mut s, &[id], &[5.0]);
    assert_eq!(clip(&s, id).as_audio().unwrap().source_offset, 0);
    drag(&mut s, &[id], &[-20.0]);
    assert_eq!(
        clip(&s, id).as_audio().unwrap().source_offset,
        8 * BAR - 2 * BAR,
        "the clip's end at the source's end"
    );
}

#[test]
fn selected_clips_slip_together_audio_and_midi() {
    let (mut s, t, keys, src) = setup();
    let a = add(&mut s, t, 1.0, ClipContent::Audio(audio(src, 3.0, 1.0)));
    let q = MusicalTime::from_quarters;
    let notes = vec![
        MidiNote {
            id: NoteId(1),
            start: q(0.0),
            length: q(1.0),
            key: 60,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
        },
        MidiNote {
            id: NoteId(2),
            start: q(2.0),
            length: q(1.0),
            key: 62,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
        },
    ];
    let m = add(
        &mut s,
        keys,
        1.0,
        ClipContent::Midi(MidiClip {
            length: q(4.0),
            notes,
            ..MidiClip::default()
        }),
    );
    let start = clip(&s, m).start;
    let steps = s.history().undo_labels().len();
    drag(&mut s, &[a, m], &[0.25]);
    assert_eq!(s.history().undo_labels().len(), steps + 1, "one undo step");
    assert_eq!(
        clip(&s, a).as_audio().unwrap().source_offset,
        3 * BAR - BAR / 4
    );
    let moved = clip(&s, m);
    assert_eq!(moved.start, start);
    assert_eq!(
        moved
            .as_midi()
            .unwrap()
            .notes
            .iter()
            .map(|n| n.start)
            .collect::<Vec<_>>(),
        vec![q(1.0), q(3.0)]
    );
    assert_eq!(moved.as_midi().unwrap().length, q(4.0));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(clip(&s, m).as_midi().unwrap().notes[0].start, q(0.0));
    assert_eq!(clip(&s, a).as_audio().unwrap().source_offset, 3 * BAR);
}

#[test]
fn warped_reversed_and_edited_audio_slip_correctly() {
    let (mut s, t, _, src) = setup();
    // Warped: a marker pins a hit; it moves with the audio.
    let mut w = audio(src, 2.0, 2.0);
    let marker = WarpMarker {
        at: BAR / 2,
        source: 2 * BAR + BAR / 2 + 1_000,
    };
    w.warp = Some(Warp {
        source_length: 2 * BAR,
        markers: vec![marker],
        algorithm: Default::default(),
    });
    let warped = add(&mut s, t, 0.0, ClipContent::Audio(w.clone()));
    drag(&mut s, &[warped], &[0.25]);
    let after = clip(&s, warped);
    let a = after.as_audio().unwrap();
    let wm = a.warp.as_ref().unwrap();
    assert_eq!(
        wm.markers,
        vec![WarpMarker {
            at: marker.at + BAR / 4,
            source: marker.source
        }],
        "the marker keeps its audio and moves in the clip"
    );
    assert_eq!(a.length, w.length);
    assert!((a.source_at((marker.at + BAR / 4) as f64) - marker.source as f64).abs() < 1.0);
    // Reversed: the offset moves the other way.
    let mut r = audio(src, 2.0, 2.0);
    r.reversed = true;
    let reversed = add(&mut s, t, 4.0, ClipContent::Audio(r));
    drag(&mut s, &[reversed], &[0.5]);
    assert_eq!(
        clip(&s, reversed).as_audio().unwrap().source_offset,
        2 * BAR + BAR / 2
    );
    // Spectral edits are in source frames: they stay with the audio, the
    // clip keeps them as they are.
    let mut e = audio(src, 2.0, 1.0);
    e.spectral = Some(Box::new(faderframe_project::spectral::SpectralEdits {
        original: src,
        edits: Vec::new(),
    }));
    // Clip effects: the clip plays their render, whose original audio
    // starts after the devices' latency (here a quarter bar in).
    let latency = BAR / 4;
    e.effects = Some(Box::new(faderframe_project::clip_fx::ClipEffects {
        chain: Vec::new(),
        original: faderframe_project::clip_fx::OriginalAudio::of(&e),
        rendered_offset: latency,
    }));
    e.source_offset = latency + BAR / 2;
    let fx = add(&mut s, t, 6.0, ClipContent::Audio(e.clone()));
    drag(&mut s, &[fx], &[1.0]);
    let a = clip(&s, fx);
    let a = a.as_audio().unwrap();
    assert_eq!(a.source_offset, latency, "never into the latency before it");
    assert_eq!((&a.spectral, &a.effects), (&e.spectral, &e.effects));
}

#[test]
fn take_folders_do_not_slip() {
    let (mut s, t, _, src) = setup();
    let mut folder = TakeFolder::new(2 * BAR);
    for n in 0..2 {
        folder.add_take(faderframe_project::Take {
            name: format!("Take {n}"),
            source: src,
            source_offset: n * BAR,
            start: 0,
            end: 2 * BAR,
            gain_db: 0.0,
            rating: 0,
        });
    }
    folder.use_take(1);
    let f = add(&mut s, t, 0.0, ClipContent::Takes(folder));
    let before = clip(&s, f);
    drag(&mut s, &[f], &[0.5]);
    assert_eq!(clip(&s, f), before);
    assert!(
        s.latest_notice()
            .is_some_and(|n| n.text.contains("Take folders do not slip"))
    );
}
