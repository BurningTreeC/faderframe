//! Clip envelopes through the session: drawn as one undo step, kept with
//! the audio by splits, trims and time-stretch, copied with their clips,
//! cleared, refused on MIDI clips, saved and opened again; one take of a
//! take folder has an envelope of its own.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::clip_envelope::{ClipEnvelope, EnvelopeLane, EnvelopePoint};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Project, SourceSpec,
    StretchSettings, Take, TakeFolder, TrackKind,
};
use faderframe_session::{Action, ClipEdge, SelectMode, Session};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// Frames of a beat at 120 BPM.
const BEAT: i64 = 24_000;

fn setup() -> (Session, TrackId, ClipId) {
    let mut s = Session::new(Project::new("Envelopes", SR), None, EngineConfig::default()).unwrap();
    let track = s.add_track(TrackKind::Audio).unwrap();
    let source = AudioSourceId(9000);
    s.dispatch(Action::Edit(Command::AddSource {
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
    }))
    .unwrap();
    let clip: ClipId = ClipId(7000);
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id: clip,
            track,
            name: "Loop".into(),
            color: None,
            start: MusicalTime::from_quarters(4.0),
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: 0,
                length: 8 * BEAT,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
                envelope: None,
            }),
        }),
    }))
    .unwrap();
    (s, track, clip)
}

fn envelope(s: &Session, clip: ClipId) -> Option<ClipEnvelope> {
    s.clip_envelope(clip.into()).cloned()
}

fn points(p: &[(i64, f64)]) -> Vec<EnvelopePoint> {
    p.iter().map(|&(a, v)| EnvelopePoint::new(a, v)).collect()
}

#[test]
fn a_drag_of_points_is_one_undo_step() {
    let (mut s, _, clip) = setup();
    s.dispatch(Action::ShowClipEnvelope(Some(EnvelopeLane::Volume)))
        .unwrap();
    assert_eq!(s.clip_envelope_shown(), Some(EnvelopeLane::Volume));
    s.dispatch(Action::BeginGesture("Clip Envelope".into()))
        .unwrap();
    for v in [-1.0, -2.0, -3.0] {
        s.dispatch(Action::SetClipEnvelope {
            audio: clip.into(),
            lane: EnvelopeLane::Volume,
            points: points(&[(0, 0.0), (BEAT, v)]),
        })
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!(
        envelope(&s, clip).unwrap().volume,
        points(&[(0, 0.0), (BEAT, -3.0)])
    );
    s.dispatch(Action::Undo).unwrap();
    assert!(envelope(&s, clip).is_none());
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(envelope(&s, clip).unwrap().volume.len(), 2);
}

#[test]
fn splits_trims_and_stretches_keep_the_envelope_with_the_audio() {
    let (mut s, track, clip) = setup();
    s.dispatch(Action::SetClipEnvelope {
        audio: clip.into(),
        lane: EnvelopeLane::Volume,
        points: points(&[(0, 0.0), (4 * BEAT, -12.0), (8 * BEAT, -24.0)]),
    })
    .unwrap();
    // A start trim by a beat: the points move a beat earlier.
    s.dispatch(Action::TrimClip {
        clip,
        edge: ClipEdge::Start,
        to: MusicalTime::from_quarters(5.0),
    })
    .unwrap();
    let e = envelope(&s, clip).unwrap();
    assert_eq!(e.volume[1].at, 3 * BEAT);
    assert_eq!(e.value_at(EnvelopeLane::Volume, 0.0), Some(-3.0));
    // Trimmed back: as before (the point outside was kept).
    s.dispatch(Action::TrimClip {
        clip,
        edge: ClipEdge::Start,
        to: MusicalTime::from_quarters(4.0),
    })
    .unwrap();
    assert_eq!(envelope(&s, clip).unwrap().volume[1].at, 4 * BEAT);
    // Split at the middle: each piece plays its half of the curve.
    let right = ClipId(s.project().ids.peek());
    s.dispatch(Action::Edit(Command::SplitClip {
        clip,
        at: MusicalTime::from_quarters(6.0),
        new_clip: right,
    }))
    .unwrap();
    let (l, r) = (envelope(&s, clip).unwrap(), envelope(&s, right).unwrap());
    assert_eq!(l.volume, points(&[(0, 0.0), (2 * BEAT, -6.0)]));
    assert_eq!(
        r.volume,
        points(&[(0, -6.0), (2 * BEAT, -12.0), (6 * BEAT, -24.0)])
    );
    // Time-stretching the right piece to double length doubles its times.
    let end = MusicalTime::from_quarters(6.0 + 12.0);
    s.dispatch(Action::StretchClip {
        clip: right,
        edge: ClipEdge::End,
        to: end,
    })
    .unwrap();
    let r = envelope(&s, right).unwrap();
    assert_eq!(r.volume[1].at, 4 * BEAT);
    assert_eq!(r.volume[2].at, 12 * BEAT);
    assert_eq!(s.project().clips_of(track).len(), 2);
}

#[test]
fn envelopes_are_cleared_copied_and_saved() {
    let (mut s, _, clip) = setup();
    s.dispatch(Action::SetClipEnvelope {
        audio: clip.into(),
        lane: EnvelopeLane::Pan,
        points: points(&[(0, -0.5), (BEAT, 2.0)]),
    })
    .unwrap();
    // Clamped to the lane's range.
    assert_eq!(envelope(&s, clip).unwrap().pan[1].value, 1.0);
    // Copied with the clip.
    s.dispatch(Action::SelectClips {
        clips: vec![clip],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.dispatch(Action::Repeat(1)).unwrap();
    let copies: Vec<ClipId> = s
        .project()
        .clips
        .keys()
        .copied()
        .filter(|&c| c != clip)
        .collect();
    assert_eq!(copies.len(), 1);
    assert_eq!(envelope(&s, copies[0]), envelope(&s, clip));
    // Saved and opened again.
    let dir = std::env::temp_dir().join(format!("ff-clip-envelope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("e.ffproj");
    s.save_as(&path).unwrap();
    let mut opened =
        Session::new(Project::new("Other", SR), None, EngineConfig::default()).unwrap();
    opened.open(&path).unwrap();
    assert_eq!(
        opened.clip_envelope(clip.into()).map(|e| e.pan.clone()),
        Some(points(&[(0, -0.5), (BEAT, 1.0)]))
    );
    // Cleared: the lane, then nothing left.
    s.dispatch(Action::ClearClipEnvelope {
        audio: vec![clip.into()],
        lane: Some(EnvelopeLane::Pan),
    })
    .unwrap();
    assert!(envelope(&s, clip).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn midi_clips_have_no_envelopes() {
    let (mut s, _, _) = setup();
    let keys = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::CreateMidiClip {
        track: keys,
        start: MusicalTime::ZERO,
        length: MusicalTime::from_quarters(4.0),
    })
    .unwrap();
    let id = s.project().clips_of(keys)[0].id;
    assert!(
        s.dispatch(Action::SetClipEnvelope {
            audio: id.into(),
            lane: EnvelopeLane::Volume,
            points: points(&[(0, -6.0)]),
        })
        .is_err()
    );
}

/// A take is a clip of its own: one take of a folder has an envelope of
/// its own (a drag one step), the comp plays it where it plays that take,
/// the folder itself refuses one, and clearing it is one step.
#[test]
fn one_take_of_a_folder_has_an_envelope_of_its_own() {
    let (mut s, _, clip) = setup();
    let c = s.project().clip(clip).unwrap().clone();
    let a = c.as_audio().unwrap().clone();
    let mut f = TakeFolder::new(a.length);
    f.add_take(Take::of_clip("Take 1", 0, a.clone()));
    f.add_take(Take::of_clip("Take 2", 0, a.clone()));
    f.use_take(0);
    f.set_comp(4 * BEAT, 8 * BEAT, Some(1));
    s.edit(Command::SetClipContent {
        clip,
        start: c.start,
        content: Box::new(ClipContent::Takes(f)),
    })
    .unwrap();
    let (one, two) = {
        let p = s.project();
        (p.take_ref(clip, 0).unwrap(), p.take_ref(clip, 1).unwrap())
    };
    let steps = s.history_steps().0.len();
    s.dispatch(Action::BeginGesture("Clip Envelope".into()))
        .unwrap();
    for v in [-3.0, -6.0, -12.0] {
        s.dispatch(Action::SetClipEnvelope {
            audio: two.into(),
            lane: EnvelopeLane::Volume,
            points: points(&[(0, 0.0), (8 * BEAT, v)]),
        })
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    assert_eq!(
        s.clip_envelope(two.into()).unwrap().volume,
        points(&[(0, 0.0), (8 * BEAT, -12.0)])
    );
    assert!(s.clip_envelope(one.into()).is_none(), "take 1 has none");
    // The comp plays take 2's envelope from where it plays take 2.
    let f = s.project().clip(clip).unwrap().as_takes().unwrap().clone();
    let pieces = f.played();
    assert!(pieces[0].audio.envelope.is_none());
    let piece = &pieces[1];
    let e = piece.audio.envelope.as_ref().unwrap();
    let at_start = e.value_at(EnvelopeLane::Volume, 0.0).unwrap();
    let want = -12.0 * piece.start as f64 / (8 * BEAT) as f64;
    assert!((at_start - want).abs() < 1e-6, "{at_start} vs {want}");
    // The folder is no audio clip: its takes have the envelopes.
    let refused = s.dispatch(Action::SetClipEnvelope {
        audio: clip.into(),
        lane: EnvelopeLane::Volume,
        points: points(&[(0, -6.0)]),
    });
    assert!(format!("{:?}", refused.unwrap_err()).contains("take folder"));
    // Cleared: one step.
    let steps = s.history_steps().0.len();
    s.dispatch(Action::ClearClipEnvelope {
        audio: vec![one.into(), two.into()],
        lane: None,
    })
    .unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1);
    assert!(s.clip_envelope(two.into()).is_none());
    s.dispatch(Action::Undo).unwrap();
    assert!(s.clip_envelope(two.into()).is_some());
}
