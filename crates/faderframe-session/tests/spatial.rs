#![allow(clippy::unwrap_used)]
//! Surround panning in the session: VBAP and the stereo modes saved,
//! undone and automated; the project's downmix for new and older
//! projects; listening on headphones without a usable head.

use faderframe_core::surround::{Downmix, StereoSource, SurroundPanner};
use faderframe_core::{ChannelLayout, SurroundFormat, SurroundPan, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, TrackKind, file};
use faderframe_session::{Action, AutomationTarget, Session};

fn session(format: SurroundFormat) -> Session {
    let mut s = Session::new(Project::new("Room", 48_000), None, EngineConfig::default()).unwrap();
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Surround(format),
    }))
    .unwrap();
    s
}

fn track(s: &mut Session, layout: ChannelLayout) -> TrackId {
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout { track: t, layout }))
        .unwrap();
    t
}

fn names(s: &Session, t: TrackId) -> Vec<String> {
    s.automatable_parameters(t)
        .into_iter()
        .map(|p| p.name)
        .collect()
}

#[test]
fn vbap_is_saved_undone_and_automated_by_direction() {
    let mut s = session(SurroundFormat::S714);
    let t = track(&mut s, ChannelLayout::Mono);
    let room = names(&s, t);
    assert!(room.contains(&"Surround Left/Right".to_string()));
    assert!(!room.contains(&"Surround Azimuth".to_string()));
    let pan = SurroundPan {
        panner: SurroundPanner::Vbap,
        azimuth: 110.0,
        elevation: 20.0,
        ..SurroundPan::default()
    };
    s.dispatch(Action::Edit(Command::SetTrackSurround { track: t, pan }))
        .unwrap();
    let vbap = names(&s, t);
    for want in [
        "Surround Azimuth",
        "Surround Elevation",
        "Surround Spread",
        "LFE Send",
    ] {
        assert!(vbap.contains(&want.to_string()), "{want} in {vbap:?}");
    }
    assert!(!vbap.contains(&"Surround Left/Right".to_string()));
    let param = s
        .automation_param(
            t,
            AutomationTarget::Surround(faderframe_core::SurroundParam::Azimuth),
        )
        .unwrap();
    assert_eq!((param.min, param.max, param.unit), (-180.0, 180.0, "°"));
    // Saved as it is, read back alike.
    let text = file::to_string(s.project(), None).unwrap();
    assert!(text.contains("\"panner\": \"vbap\""), "{text}");
    let loaded = file::from_str(&text).unwrap().project;
    assert_eq!(loaded.track(t).unwrap().surround, pan);
    // One undo: back in the room, where it was.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project().track(t).unwrap().surround,
        SurroundPan::default()
    );
    // MIDI learn sets one direction and keeps the rest.
    match s.command_for(
        t,
        AutomationTarget::Surround(faderframe_core::SurroundParam::Azimuth),
        -45.0,
    ) {
        Some(Command::SetTrackSurround { pan, .. }) => assert_eq!(pan.azimuth, -45.0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_stereo_track_balanced_into_a_bed_automates_its_balance() {
    let mut s = session(SurroundFormat::S51);
    let t = track(&mut s, ChannelLayout::Stereo);
    s.dispatch(Action::Edit(Command::SetTrackSurround {
        track: t,
        pan: SurroundPan {
            stereo: StereoSource::Balance,
            ..SurroundPan::default()
        },
    }))
    .unwrap();
    assert!(s.project().balanced_into_bed(s.project().track(t).unwrap()));
    let n = names(&s, t);
    assert!(n.contains(&"Balance".to_string()), "{n:?}");
    assert!(n.contains(&"LFE Send".to_string()));
    assert!(!n.iter().any(|n| n.starts_with("Surround ")), "{n:?}");
    // Dual pan offers each channel's direction (VBAP only).
    s.dispatch(Action::Edit(Command::SetTrackSurround {
        track: t,
        pan: SurroundPan {
            panner: SurroundPanner::Vbap,
            stereo: StereoSource::Dual,
            ..SurroundPan::default()
        },
    }))
    .unwrap();
    let n = names(&s, t);
    assert!(n.contains(&"Left Channel Azimuth".to_string()));
    assert!(n.contains(&"Right Channel Azimuth".to_string()));
    assert!(!n.contains(&"Balance".to_string()));
}

#[test]
fn new_projects_fold_by_coefficients_older_ones_by_place() {
    let mut s = session(SurroundFormat::S51);
    assert_eq!(s.project().downmix, Downmix::STANDARD);
    let text = file::to_string(s.project(), None).unwrap();
    assert!(text.contains("\"downmix\""), "a new project says so");
    // A file from before the setting: by place, as it always played.
    let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    doc["project"].as_object_mut().unwrap().remove("downmix");
    let old = file::from_str(&doc.to_string()).unwrap().project;
    assert_eq!(old.downmix, Downmix::BY_PLACE);
    // Saved again, it stays so (nothing written: still the older fold).
    let again = file::to_string(&old, None).unwrap();
    assert!(!again.contains("\"downmix\""));
    // An edit, undone.
    s.dispatch(Action::Edit(Command::SetDownmix {
        downmix: Downmix {
            surround: f32::NAN,
            lfe: 9.0,
            ..Downmix::STANDARD
        },
    }))
    .unwrap();
    let d = s.project().downmix;
    assert_eq!((d.surround, d.lfe), (0.0, 2.0), "made safe");
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().downmix, Downmix::STANDARD);
}

#[test]
fn a_head_that_cannot_be_read_leaves_listening_as_it_was() {
    let mut s = session(SurroundFormat::S51);
    let before = s.head().id().to_string();
    assert!(s.set_head("no-such-head").is_err());
    assert!(s.set_head("sofa:/no/such/file.sofa").is_err());
    assert_eq!(s.head().id(), before);
    // Headphones on and off: listening only, the project untouched.
    let project = s.project().clone();
    s.set_headphones(Some(faderframe_binaural::Room::Near))
        .unwrap();
    assert_eq!(s.headphones(), Some(faderframe_binaural::Room::Near));
    s.set_headphones(None).unwrap();
    assert_eq!(s.project(), &project);
}

#[test]
fn bass_management_is_listening_only() {
    use faderframe_core::surround::BassManagement;
    let mut s = Session::new(Project::new("Bass", 48_000), None, EngineConfig::default()).unwrap();
    // A stereo master has no subwoofer channel.
    assert!(!s.bass_management_applies());
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Surround(SurroundFormat::S51),
    }))
    .unwrap();
    assert!(s.bass_management_applies());
    let project = s.project().clone();
    let undo = s.history_steps().0.len();
    s.dispatch(Action::SetBassManagement(BassManagement {
        enabled: true,
        crossover: 500.0,
        redirect_db: f32::NAN,
        lfe_lowpass: 100.0,
    }))
    .unwrap();
    let b = s.bass_management();
    assert!(b.enabled);
    assert_eq!(
        (b.crossover, b.redirect_db, b.lfe_lowpass),
        (200.0, -10.0, 100.0)
    );
    // Not part of the mix: the project and its history untouched.
    assert_eq!(s.project(), &project);
    assert_eq!(s.history_steps().0.len(), undo);
}

#[test]
fn speaker_layouts_are_kept_edited_and_offered() {
    use faderframe_core::surround::{LayoutDef, SpeakerDef};
    let spk = |label: &str, azimuth: f32| SpeakerDef {
        label: label.into(),
        azimuth,
        elevation: 0.0,
        lfe: false,
    };
    let ring = |name: &str, n: usize| LayoutDef {
        name: name.into(),
        speakers: (0..n)
            .map(|i| spk(&format!("S{i}"), 180.0 - 360.0 * i as f32 / n as f32 - 0.5))
            .collect(),
    };
    let mut s = Session::new(Project::new("Rings", 48_000), None, EngineConfig::default()).unwrap();
    let master = s.project().master_id().unwrap();
    // A broken layout is refused, the library untouched.
    let mut bad = ring("Bad", 4);
    bad.speakers[1].label = "S0".into();
    assert!(
        s.dispatch(Action::SaveSpeakerLayout {
            replace: None,
            layout: bad
        })
        .is_err()
    );
    assert!(s.speaker_layouts().is_empty());
    s.dispatch(Action::SaveSpeakerLayout {
        replace: None,
        layout: ring("Ring", 6),
    })
    .unwrap();
    // Offered as a channel format; the master takes it.
    let choice = s
        .format_choices(master)
        .into_iter()
        .find(|c| c.label == "Ring (6 channels)")
        .expect("offered");
    s.dispatch(choice.action).unwrap();
    let six = s.project().track(master).unwrap().layout;
    assert_eq!(six.channel_count(), 6);
    // A second one with the same name is refused.
    assert!(
        s.dispatch(Action::SaveSpeakerLayout {
            replace: None,
            layout: ring("ring", 4)
        })
        .is_err()
    );
    // Edited to eight speakers: the master follows, in one undo step.
    s.dispatch(Action::SaveSpeakerLayout {
        replace: Some("Ring".into()),
        layout: ring("Ring", 8),
    })
    .unwrap();
    assert_eq!(s.project().track(master).unwrap().layout.channel_count(), 8);
    assert_eq!(s.speaker_layouts().len(), 1);
    // In use: not removed.
    assert!(
        s.dispatch(Action::RemoveSpeakerLayout("Ring".into()))
            .is_err()
    );
    // Saved and read back, the master is on the same layout.
    let text = file::to_string(s.project(), None).unwrap();
    let loaded = file::from_str(&text).unwrap().project;
    assert_eq!(
        loaded.track(master).unwrap().layout,
        s.project().track(master).unwrap().layout
    );
    assert_eq!(loaded.speaker_layouts, s.speaker_layouts());
    // Undo: six speakers again.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track(master).unwrap().layout, six);
    // Back to stereo, then it can go.
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Stereo,
    }))
    .unwrap();
    s.dispatch(Action::RemoveSpeakerLayout("Ring".into()))
        .unwrap();
    assert!(s.speaker_layouts().is_empty());
}

#[test]
fn elevation_reaches_below_ear_level_only_where_speakers_are() {
    use faderframe_core::SurroundParam;
    use faderframe_core::surround::{LayoutDef, SpeakerDef};
    let mut s = session(SurroundFormat::S714);
    let t = track(&mut s, ChannelLayout::Mono);
    s.dispatch(Action::Edit(Command::SetTrackSurround {
        track: t,
        pan: SurroundPan {
            panner: SurroundPanner::Vbap,
            ..SurroundPan::default()
        },
    }))
    .unwrap();
    let elevation = |s: &Session| {
        s.automation_param(t, AutomationTarget::Surround(SurroundParam::Elevation))
            .map(|p| (p.min, p.max))
    };
    assert_eq!(elevation(&s), Some((0.0, 90.0)));
    let spk = |label: &str, azimuth: f32, elevation: f32| SpeakerDef {
        label: label.into(),
        azimuth,
        elevation,
        lfe: false,
    };
    let sphere = LayoutDef {
        name: "Session Sphere".into(),
        speakers: vec![
            spk("F", 0.0, 0.0),
            spk("L", 120.0, 0.0),
            spk("R", -120.0, 0.0),
            spk("Up", 0.0, 90.0),
            spk("DL", 60.0, -50.0),
            spk("DR", -60.0, -50.0),
            spk("DB", 180.0, -50.0),
        ],
    };
    s.dispatch(Action::SaveSpeakerLayout {
        replace: None,
        layout: sphere.clone(),
    })
    .unwrap();
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Surround(sphere.intern().unwrap()),
    }))
    .unwrap();
    assert_eq!(elevation(&s), Some((-90.0, 90.0)));
}
