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

/// Renders start where their range does, whatever the graph's latency: a
/// click on frame 1000 through a limiter with lookahead (on the master,
/// then on the track) lands on frame 1000 of the export, of a frozen
/// track's playback and of a bounce.
#[test]
fn renders_take_the_latency_off_the_front() {
    let dir = std::env::temp_dir().join(format!("ff-latency-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::new(
        Project::new("Latency", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let file = dir.join("click.wav");
    let mut x = vec![0.0f32; 192_000];
    x[1000] = 0.25;
    faderframe_audio_files::write_wav(&file, &[x], 48_000, WavFormat::Float32, false).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: t,
        layout: faderframe_core::ChannelLayout::Mono,
    }))
    .unwrap();
    s.import_audio(
        vec![file],
        faderframe_session::ImportTarget {
            track: Some(t),
            at: MusicalTime::ZERO,
        },
    );
    s.wait_for_imports();
    let limiter = s
        .available_plugins()
        .into_iter()
        .find(|p| p.plugin.id == "faderframe.limiter")
        .unwrap()
        .plugin;
    let onset = |x: &[f32]| x.iter().position(|v| v.abs() > 1e-4);
    let master_id = s.project().master_id().unwrap();
    s.dispatch(Action::InsertPlugin {
        track: master_id,
        index: 0,
        plugin: limiter.clone(),
    })
    .unwrap();
    assert!(s.engine().graph_stats().output_latency > 100);
    assert_eq!(onset(&master(s.project(), "master-limited")), Some(1000));
    s.dispatch(Action::Edit(Command::RemovePlugin {
        track: master_id,
        plugin: s.project().master().unwrap().inserts[0].id,
    }))
    .unwrap();
    s.dispatch(Action::InsertPlugin {
        track: t,
        index: 0,
        plugin: limiter,
    })
    .unwrap();
    s.dispatch(Action::BounceTrack(t)).unwrap();
    wait(&mut s);
    s.dispatch(Action::FreezeTrack(t)).unwrap();
    wait(&mut s);
    assert!(s.project().track(t).unwrap().freeze.is_some());
    // The frozen track and the bounce play together: the click twice at
    // once, so the onset stays where it was.
    let both = master(s.project(), "frozen-and-bounced");
    assert_eq!(onset(&both), Some(1000));
    assert!(
        s.project()
            .tracks
            .iter()
            .any(|x| x.name.ends_with("Bounce"))
    );
    s.dispatch(Action::Edit(Command::SetTrackMute { track: t, on: true }))
        .unwrap();
    assert_eq!(
        onset(&master(s.project(), "bounce-only")),
        Some(1000),
        "the bounce"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
