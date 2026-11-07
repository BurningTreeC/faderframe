//! Mono → Stereo inserts: where the switch is offered, what it does to the
//! track's layout, undo, and frozen tracks keeping theirs.
#![allow(clippy::unwrap_used)]

use faderframe_core::{ChannelLayout, PluginInstanceId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, PluginRef, Project, TrackKind};
use faderframe_session::{Action, Session};
use std::time::{Duration, Instant};

fn insert(s: &mut Session, track: TrackId, id: &str) -> PluginInstanceId {
    let index = s.project().track(track).unwrap().inserts.len();
    s.dispatch(Action::InsertPlugin {
        track,
        index,
        plugin: PluginRef::builtin(id, id),
    })
    .unwrap();
    s.project().track(track).unwrap().inserts[index].id
}

fn set(s: &mut Session, track: TrackId, plugin: PluginInstanceId, on: bool) -> Result<(), String> {
    s.dispatch(Action::Edit(Command::SetPluginMonoToStereo {
        track,
        plugin,
        on,
    }))
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn layout(s: &Session, track: TrackId) -> ChannelLayout {
    s.project().track(track).unwrap().chain_layout()
}

#[test]
fn a_mono_track_turns_stereo_at_the_insert_set_to_mono_to_stereo() {
    let mut s = Session::new(Project::new("Amp", 48_000), None, EngineConfig::default()).unwrap();
    let guitar = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: guitar,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    let reverb = insert(&mut s, guitar, builtin::REVERB);
    let echo = insert(&mut s, guitar, builtin::ECHO);
    assert!(
        s.mono_to_stereo_offered(reverb),
        "a stereo device on a mono track"
    );
    assert!(s.mono_to_stereo_offered(echo));
    assert_eq!(layout(&s, guitar), ChannelLayout::Mono);

    set(&mut s, guitar, reverb, true).unwrap();
    assert_eq!(layout(&s, guitar), ChannelLayout::Stereo);
    let track = s.project().track(guitar).unwrap();
    assert_eq!(track.layout_into(echo), Some(ChannelLayout::Stereo));
    assert!(
        s.mono_to_stereo_offered(reverb),
        "still offered, to turn it off"
    );
    assert!(
        !s.mono_to_stereo_offered(echo),
        "the insert after it is reached by stereo already"
    );
    assert!(
        s.history_steps().0.iter().any(|l| l == "Mono → Stereo"),
        "{:?}",
        s.history_steps().0
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(layout(&s, guitar), ChannelLayout::Mono);
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(layout(&s, guitar), ChannelLayout::Stereo);
    set(&mut s, guitar, reverb, false).unwrap();
    assert_eq!(layout(&s, guitar), ChannelLayout::Mono);

    // Devices that widen by themselves or have their own switch.
    let station = insert(&mut s, guitar, builtin::GUITAR_STATION);
    assert!(!s.mono_to_stereo_offered(station));
    let other = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: other,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    let outboard = insert(&mut s, other, builtin::HARDWARE_INSERT);
    assert!(!s.mono_to_stereo_offered(outboard));
    // A stereo track has nothing to widen.
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: other,
        layout: ChannelLayout::Stereo,
    }))
    .unwrap();
    let reverb = insert(&mut s, other, builtin::REVERB);
    assert!(!s.mono_to_stereo_offered(reverb));
}

#[test]
fn a_frozen_track_keeps_its_layout() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let t = s
        .project()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio && t.layout == ChannelLayout::Mono)
        .map(|t| t.id)
        .expect("a mono audio track in the demo");
    let reverb = insert(&mut s, t, builtin::REVERB);
    s.dispatch(Action::FreezeTrack(t)).unwrap();
    let end = Instant::now() + Duration::from_secs(120);
    while !s.bouncing().is_empty() {
        assert!(Instant::now() < end, "the freeze timed out");
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(s.project().track(t).unwrap().freeze.is_some());
    // Its frozen audio is mono: the switch would leave it behind.
    assert!(set(&mut s, t, reverb, true).is_err());
    assert_eq!(layout(&s, t), ChannelLayout::Mono);
}
