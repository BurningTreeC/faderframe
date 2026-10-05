//! Freezing and bouncing tracks.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav};
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::render::{self, RenderChannels, RenderRange, RenderSettings};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

fn wait(s: &mut Session) {
    let end = Instant::now() + Duration::from_secs(60);
    while !s.bouncing().is_empty() {
        assert!(Instant::now() < end, "render timed out");
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

/// Two bars of the master, rendered offline.
fn master(project: &Project, name: &str) -> Vec<f32> {
    let path = std::env::temp_dir().join(format!("ff-freeze-{}-{name}.wav", std::process::id()));
    let settings = RenderSettings {
        range: RenderRange::Bars { start: 0, end: 2 },
        channels: RenderChannels::Mono,
        tail_seconds: 0.0,
        normalize_db: None,
        format: WavFormat::Float32,
        ..RenderSettings::defaults_for(project, path.clone())
    };
    render::start(project.clone(), settings)
        .unwrap()
        .join()
        .unwrap();
    let wav = read_wav(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    wav.channels.into_iter().next().unwrap()
}

#[test]
fn freezing_keeps_the_sound_and_blocks_edits_until_unfrozen() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let pluck = track(&s, "Pluck");
    s.dispatch(Action::SetPreamp {
        track: pluck,
        model: Some(1),
    })
    .unwrap();
    let before = master(s.project(), "before");
    s.dispatch(Action::FreezeTrack(pluck)).unwrap();
    wait(&mut s);
    let f = s
        .project()
        .track(pluck)
        .unwrap()
        .freeze
        .clone()
        .expect("frozen");
    assert_eq!(f.start, MusicalTime::ZERO);
    assert!(s.project().sources.contains_key(&f.source));
    let after = master(s.project(), "after");
    assert_eq!(before.len(), after.len());
    let diff = before
        .iter()
        .zip(&after)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        diff < 1e-3,
        "frozen audio sounds the same (max diff {diff})"
    );

    // Clips of a frozen track cannot be edited.
    let clip = s.project().track(pluck).unwrap().clips[0];
    let mv = Command::MoveClip {
        clip,
        track: pluck,
        start: MusicalTime::from_quarters(4.0),
    };
    let err = s.dispatch(Action::Edit(mv.clone())).unwrap_err();
    assert!(err.to_string().contains("frozen"), "{err}");
    s.dispatch(Action::UnfreezeTrack(pluck)).unwrap();
    assert!(!s.is_frozen(pluck));
    s.dispatch(Action::Edit(mv)).unwrap();
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert!(s.is_frozen(pluck), "undo refreezes");
}

#[test]
fn bouncing_an_instrument_makes_an_audio_track_and_mutes_the_original() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let lead = track(&s, "Lead Synth");
    s.dispatch(Action::BounceTrack(lead)).unwrap();
    wait(&mut s);
    let p = s.project();
    let new = p
        .tracks
        .iter()
        .find(|t| t.name == "Lead Synth Bounce")
        .unwrap();
    assert_eq!(new.kind, TrackKind::Audio);
    assert!(p.track(lead).unwrap().mute);
    let clip = p.clips_of(new.id)[0];
    let melody = p.clips_of(lead)[0];
    assert_eq!(clip.start, melody.start);
    let audio = clip.as_audio().unwrap();
    let faderframe_project::SourceSpec::File { path, .. } = &p.sources[&audio.source].spec else {
        panic!()
    };
    let wav = read_wav(path).unwrap();
    let peak = wav.channels[0].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.05, "the synth was rendered (peak {peak})");
    s.dispatch(Action::Undo).unwrap();
    assert!(!s.project().track(lead).unwrap().mute);
}
