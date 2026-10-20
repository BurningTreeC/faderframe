//! Cue mixes in the session: a new one is a copy of the main mix on the
//! next free outputs and the mixer shows it; its sends are set without
//! touching the main mix; Copy Main Mix resets them; pre/post-fader keeps
//! the sound; new tracks join; every change undoes in one step; a saved
//! project opens with its cue mixes; renders never carry them.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav};
use faderframe_core::{ChannelLayout, CueId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::cue::feeds_cues;
use faderframe_project::{Command, OutputRouting, Project, SendTap, TrackKind};
use faderframe_session::cues::CueOp;
use faderframe_session::render::{self, RenderChannels, RenderRange, RenderSettings};
use faderframe_session::{Action, Session};

fn demo() -> Session {
    Session::demo(EngineConfig::default()).unwrap()
}

fn new_cue(s: &mut Session) -> CueId {
    s.dispatch(Action::Cue(CueOp::New)).unwrap();
    s.mixer_cue().unwrap().id
}

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

#[test]
fn a_new_cue_mix_is_a_copy_of_the_main_mix_on_the_next_free_outputs() {
    let mut s = demo();
    let before = s.project().clone();
    let steps = s.history_steps().0.len();
    let id = new_cue(&mut s);
    assert_eq!(s.history_steps().0.len(), steps + 1, "one undo step");
    let c = s.project().cue(id).unwrap().clone();
    assert_eq!(c.name, "Cue 1");
    assert_eq!(c.tap, SendTap::PreFader);
    assert_eq!(c.layout, ChannelLayout::Stereo);
    // The master plays on 1-2: the cue mix on 3-4.
    assert_eq!((c.first_channel, c.output_label().as_str()), (2, "Out 3-4"));
    // Every track feeding cue mixes has its send, as the main mix has it
    // now: automated levels as they play (the demo's Pad rides its fader),
    // not the static faders.
    for t in &s.project().tracks {
        let send = t.cue_send(id).copied();
        if feeds_cues(t.kind) {
            assert_eq!(send, Some(s.main_mix_send(t, &c)), "{}", t.name);
            if t.name == "Pad" {
                assert_ne!(send, Some(s.project().main_mix_send(t, &c)));
            }
        } else {
            assert_eq!(send, None, "{}", t.name);
        }
    }
    let lead = s.project().track(track(&s, "Bass")).unwrap();
    let send = lead.cue_send(id).unwrap();
    assert!(send.on);
    assert_eq!(send.level_db, lead.volume_db);
    // Buses and returns are in (solo-safe and mute-safe: their cue paths
    // sum their sources as the cue hears them), at their faders; a track
    // reaching the master through a bus is sent by the bus, its own send
    // off (the console rule: never doubled).
    let bus = s.project().track(track(&s, "Drum Bus")).unwrap();
    assert!(bus.cue_send(id).unwrap().on);
    assert_eq!(bus.cue_send(id).unwrap().level_db, bus.volume_db);
    let drums = s.project().track(track(&s, "Drums")).unwrap();
    assert_eq!(
        drums.output,
        OutputRouting::Track { track: bus.id },
        "the demo's drums go through the drum bus"
    );
    assert!(!drums.cue_send(id).unwrap().on, "sent by the bus");
    assert_eq!(drums.cue_send(id).unwrap().level_db, drums.volume_db);
    // A second one takes the next pair.
    let second = new_cue(&mut s);
    assert_eq!(s.project().cue(second).unwrap().first_channel, 4);
    assert_eq!(s.project().cue(second).unwrap().name, "Cue 2");
    assert_eq!(s.wanted_channels().1, 6, "the device opens six outputs");
    // Undone: as before.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    // (Ids handed out stay handed out.)
    assert_eq!(s.project().tracks, before.tracks);
    assert!(s.project().cue_mixes.is_empty());
    assert!(s.mixer_cue().is_none(), "the mixer back on the main mix");
}

#[test]
fn cue_mode_edits_set_the_cue_and_never_the_main_mix() {
    let mut s = demo();
    let id = new_cue(&mut s);
    let lead = track(&s, "Bass");
    let main = s.project().track(lead).unwrap().clone();
    for cmd in [
        Command::SetCueSendLevel {
            track: lead,
            cue: id,
            db: 4.0,
        },
        Command::SetCueSendPan {
            track: lead,
            cue: id,
            pan: -0.5,
        },
        Command::SetCueSendOn {
            track: lead,
            cue: id,
            on: false,
        },
        Command::SetCueLevel { cue: id, db: -3.0 },
        Command::SetCueMute { cue: id, on: true },
        Command::SetCueClick {
            cue: id,
            on: false,
            db: -12.0,
        },
    ] {
        s.dispatch(Action::Edit(cmd)).unwrap();
    }
    let t = s.project().track(lead).unwrap();
    let send = t.cue_send(id).unwrap();
    assert_eq!((send.level_db, send.pan, send.on), (4.0, -0.5, false));
    assert_eq!(t.volume_db, main.volume_db);
    assert_eq!((t.pan, t.mute), (main.pan, main.mute));
    let c = s.project().cue(id).unwrap();
    assert_eq!(
        (c.level_db, c.mute, c.click, c.click_db),
        (-3.0, true, false, -12.0)
    );
    // A fader drag in cue mode is one step.
    s.dispatch(Action::BeginGesture("Cue Level".into()))
        .unwrap();
    for db in [-1.0, -2.0, -3.0] {
        s.dispatch(Action::Edit(Command::SetCueSendLevel {
            track: lead,
            cue: id,
            db,
        }))
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project()
            .track(lead)
            .unwrap()
            .cue_send(id)
            .unwrap()
            .level_db,
        4.0
    );
    // Renamed, moved to a mono output, removed and back.
    s.dispatch(Action::Cue(CueOp::Rename {
        cue: id,
        name: "  Drummer ".into(),
    }))
    .unwrap();
    s.dispatch(Action::Cue(CueOp::SetOutput {
        cue: id,
        first_channel: 6,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    let c = s.project().cue(id).unwrap();
    assert_eq!(
        (c.name.as_str(), c.output_label().as_str()),
        ("Drummer", "Out 7")
    );
    let with = s.project().clone();
    s.dispatch(Action::Cue(CueOp::Remove(id))).unwrap();
    assert!(s.project().cue_mixes.is_empty());
    assert!(s.project().tracks.iter().all(|t| t.cue_sends.is_empty()));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project(), &with);
    // The mixer shows a cue mix, then the main mix.
    s.dispatch(Action::Cue(CueOp::Show(Some(id)))).unwrap();
    assert_eq!(s.mixer_cue().unwrap().id, id);
    s.dispatch(Action::Cue(CueOp::Show(None))).unwrap();
    assert!(s.mixer_cue().is_none());
}

#[test]
fn copy_main_mix_resets_the_sends_and_post_fader_keeps_the_sound() {
    let mut s = demo();
    let id = new_cue(&mut s);
    let lead = track(&s, "Bass");
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: lead,
        db: -8.0,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetCueSendLevel {
        track: lead,
        cue: id,
        db: 6.0,
    }))
    .unwrap();
    let steps = s.history_steps().0.len();
    s.dispatch(Action::Cue(CueOp::CopyMainMix(id))).unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1);
    assert_eq!(s.history_steps().0.last().unwrap(), "Copy Main Mix");
    let send = *s.project().track(lead).unwrap().cue_send(id).unwrap();
    assert_eq!((send.level_db, send.on), (-8.0, true));
    // Post-fader: the fader is in the tap, the send at 0 dB — the same
    // level in the headphones. Back pre-fader: −8 dB again.
    s.dispatch(Action::Cue(CueOp::SetTap {
        cue: id,
        tap: SendTap::PostFader,
    }))
    .unwrap();
    assert!(s.project().cue(id).unwrap().post_fader());
    let send = *s.project().track(lead).unwrap().cue_send(id).unwrap();
    assert!(send.level_db.abs() < 1e-4, "{}", send.level_db);
    s.dispatch(Action::Cue(CueOp::SetTap {
        cue: id,
        tap: SendTap::PreFader,
    }))
    .unwrap();
    let send = *s.project().track(lead).unwrap().cue_send(id).unwrap();
    assert!((send.level_db + 8.0).abs() < 1e-4);
    assert!(
        s.dispatch(Action::Cue(CueOp::SetTap {
            cue: id,
            tap: SendTap::PreFx,
        }))
        .is_err()
    );
}

#[test]
fn new_tracks_join_every_cue_mix() {
    let mut s = demo();
    let a = new_cue(&mut s);
    let b = new_cue(&mut s);
    let t = s.add_track(TrackKind::Audio).unwrap();
    let new = s.project().track(t).unwrap();
    for cue in [a, b] {
        let send = new.cue_send(cue).unwrap();
        assert!(send.on, "heard there");
        assert_eq!(send.level_db, new.volume_db, "at its fader level");
    }
    // A bus (into the master) joins switched on; a MIDI track has no
    // audio to send.
    let bus = s.add_track(TrackKind::Bus).unwrap();
    assert!(s.project().track(bus).unwrap().cue_send(a).unwrap().on);
    let midi = s.add_track(TrackKind::Midi).unwrap();
    assert!(s.project().track(midi).unwrap().cue_sends.is_empty());
    // A track removed and back comes with its sends.
    let with = s.project().clone();
    s.dispatch(Action::Edit(Command::RemoveTrack { track: t }))
        .unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track(t), with.track(t));
    // A duplicate brings its own (as the original's are).
    s.dispatch(Action::Edit(Command::SetCueSendLevel {
        track: t,
        cue: a,
        db: -20.0,
    }))
    .unwrap();
    s.dispatch(Action::DuplicateTracks {
        tracks: vec![t],
        content: false,
    })
    .unwrap();
    let copy = s.project().tracks.iter().rev().find(|x| {
        x.id != t
            && x.kind == TrackKind::Audio
            && x.cue_send(a).is_some_and(|s| s.level_db == -20.0)
    });
    assert!(
        copy.is_some(),
        "the duplicate's cue sends are the original's"
    );
}

#[test]
fn cue_mixes_are_saved_and_open_again() {
    let dir = std::env::temp_dir().join(format!("ff-cues-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cues.ffproj");
    let mut s = demo();
    let id = new_cue(&mut s);
    let lead = track(&s, "Bass");
    s.dispatch(Action::Edit(Command::SetCueSendPan {
        track: lead,
        cue: id,
        pan: 0.25,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetCueClick {
        cue: id,
        on: true,
        db: -5.0,
    }))
    .unwrap();
    s.save_as(&path).unwrap();
    let saved = s.project().clone();
    let mut other = demo();
    other.open(&path).unwrap();
    assert_eq!(other.project().cue_mixes, saved.cue_mixes);
    for t in &saved.tracks {
        assert_eq!(
            other.project().track(t.id).unwrap().cue_sends,
            t.cue_sends,
            "{}",
            t.name
        );
    }
    assert!(
        other.mixer_cue().is_none(),
        "a project opens on the main mix"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A render, a track's stem: nothing of a cue mix, even on the master's
/// outputs with every send up.
#[test]
fn renders_never_carry_a_cue_mix() {
    let render = |project: &Project, name: &str| {
        let path =
            std::env::temp_dir().join(format!("ff-cue-render-{}-{name}.wav", std::process::id()));
        let settings = RenderSettings {
            range: RenderRange::Bars { start: 0, end: 2 },
            channels: RenderChannels::Stereo,
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
        wav.channels
    };
    let mut s = demo();
    let plain = render(s.project(), "plain");
    let id = new_cue(&mut s);
    s.dispatch(Action::Cue(CueOp::SetOutput {
        cue: id,
        first_channel: 0,
        layout: ChannelLayout::Stereo,
    }))
    .unwrap();
    for t in s.cue_tracks() {
        s.dispatch(Action::Edit(Command::SetCueSendLevel {
            track: t,
            cue: id,
            db: 12.0,
        }))
        .unwrap();
    }
    let with = render(s.project(), "with");
    assert_eq!(plain, with);
    // The master is still where it was: a cue mix never takes it over.
    assert!(matches!(
        s.project().master().unwrap().output,
        OutputRouting::Hardware { first_channel: 0 }
    ));
}

/// Removing a track and undoing it after a cue mix was made and undone:
/// its sends as they were; and a track restored while a newer cue mix
/// exists joins it as the main mix has it (`Project::join_cue_mixes`,
/// covered on the command level in the project crate) — never a silent
/// track.
#[test]
fn a_removed_track_comes_back_into_every_cue_mix() {
    let mut s = demo();
    let a = new_cue(&mut s);
    let bass = track(&s, "Bass");
    s.dispatch(Action::Edit(Command::SetCueSendLevel {
        track: bass,
        cue: a,
        db: -11.0,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::RemoveTrack { track: bass }))
        .unwrap();
    let b = new_cue(&mut s);
    assert!(s.project().track(bass).is_none());
    // Undo the cue mix, undo the removal: the bass with its send to the
    // first cue mix as it was.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    let t = s.project().track(bass).unwrap();
    assert_eq!(t.cue_send(a).unwrap().level_db, -11.0);
    assert!(t.cue_send(b).is_none(), "undone with the cue mix");
    // Redo both: the bass gone, the second cue mix back; a duplicate of
    // another track made now joins both.
    s.dispatch(Action::Redo).unwrap();
    s.dispatch(Action::Redo).unwrap();
    assert!(s.project().cue(b).is_some());
    let pad = track(&s, "Pad");
    s.dispatch(Action::DuplicateTracks {
        tracks: vec![pad],
        content: false,
    })
    .unwrap();
    let copy = s
        .project()
        .tracks
        .iter()
        .rev()
        .find(|t| t.id != pad && t.name.starts_with("Pad"))
        .unwrap();
    assert!(copy.cue_send(a).is_some() && copy.cue_send(b).is_some());
}

/// A cue send's level written in Touch mode while playing: the lane's
/// points follow the fader in cue mode (the same way the main faders'
/// do), and the mixer's shown value is the automated one afterwards.
#[test]
fn cue_send_levels_are_written_in_touch_mode() {
    use faderframe_audio::dummy::DummyBackend;
    use faderframe_automation::CueSendParam;
    use faderframe_session::{AudioPreferences, AutomationMode, AutomationTarget, TransportAction};
    use std::time::{Duration, Instant};
    let mut s = demo();
    let id = new_cue(&mut s);
    let bass = track(&s, "Bass");
    let target = AutomationTarget::CueSend {
        cue: id,
        what: CueSendParam::Level,
    };
    assert!(
        s.automatable_parameters(bass)
            .iter()
            .any(|p| p.target == target && p.name == "Cue → Cue 1"),
        "listed"
    );
    let master = s.project().master_id().unwrap();
    assert!(
        s.automatable_parameters(master)
            .iter()
            .any(|p| p.target == AutomationTarget::CueLevel { cue: id }),
        "the cue mix's level on the master"
    );
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::ShowAutomation {
        track: bass,
        target,
    })
    .unwrap();
    let lane = s.shown_lanes(bass)[0].id;
    s.dispatch(Action::SetAutomationMode {
        track: bass,
        lane,
        mode: AutomationMode::Touch,
    })
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let started = Instant::now();
    while !s.transport().playing && started.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
        s.tick(0.01);
    }
    // A fader gesture: a ramp to −20 dB over 0.8 quarters of the playhead
    // (by the playhead, not the wall clock: a loaded machine still writes
    // a ramp).
    s.dispatch(Action::BeginGesture("Cue Level".into()))
        .unwrap();
    let from = s.playhead();
    let ramp = faderframe_timeline::MusicalTime::from_quarters(0.8);
    let started = Instant::now();
    while s.playhead() - from < ramp && started.elapsed() < Duration::from_secs(8) {
        let done = ((s.playhead() - from).ticks() as f64 / ramp.ticks() as f64).clamp(0.0, 1.0);
        s.dispatch(Action::Edit(Command::SetCueSendLevel {
            track: bass,
            cue: id,
            db: -20.0 * done as f32,
        }))
        .unwrap();
        std::thread::sleep(Duration::from_millis(10));
        s.tick(0.01);
    }
    s.dispatch(Action::Edit(Command::SetCueSendLevel {
        track: bass,
        cue: id,
        db: -20.0,
    }))
    .unwrap();
    assert!(s.is_lane_writing(lane));
    s.dispatch(Action::EndGesture).unwrap();
    assert!(!s.is_lane_writing(lane));
    let pts = s
        .project()
        .track(bass)
        .unwrap()
        .automation
        .lane(target)
        .unwrap()
        .curve
        .points()
        .to_vec();
    assert!(pts.len() >= 2, "{} points", pts.len());
    assert!(
        (pts.last().unwrap().value - -20.0).abs() < 2.0,
        "{:?}",
        pts.last()
    );
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.stop_audio();
    // What the mixer shows in cue mode is the lane's value.
    let t = s.project().track(bass).unwrap();
    let shown = s.shown_cue_send(t, id);
    assert!(s.is_automated(bass, target));
    assert!((shown.level_db - s.display_value(bass, target).unwrap() as f32).abs() < 1e-6);
}

/// MIDI learn: a controller on a cue send's level (in the mixer's cue
/// mode the fader's menu learns the send), a pad on the talkback —
/// momentary: talking while the pad is down.
#[test]
fn cue_sends_and_the_talkback_learn_midi_controllers() {
    use faderframe_automation::CueSendParam;
    use faderframe_project::MappingTarget;
    use faderframe_session::AutomationTarget;
    use faderframe_session::cues::TalkbackSettings;
    let mut s = demo();
    let id = new_cue(&mut s);
    let bass = track(&s, "Bass");
    let target = MappingTarget::Parameter {
        track: bass,
        target: AutomationTarget::CueSend {
            cue: id,
            what: CueSendParam::Level,
        },
    };
    s.dispatch(Action::MidiLearn(target)).unwrap();
    s.midi_keyboard().send(&[0xB0, 11, 64]);
    s.tick(0.0);
    assert_eq!(s.midi_mappings_for(target).len(), 1);
    assert_eq!(s.mapping_target_label(&target), "Bass · Cue → Cue 1");
    let before = s
        .project()
        .track(bass)
        .unwrap()
        .cue_send(id)
        .unwrap()
        .level_db;
    let main = s.project().track(bass).unwrap().volume_db;
    s.midi_keyboard().send(&[0xB0, 11, 127]);
    s.tick(0.0);
    let top = s
        .project()
        .track(bass)
        .unwrap()
        .cue_send(id)
        .unwrap()
        .level_db;
    assert!(top > before, "{top} > {before}");
    assert_eq!(
        s.project().track(bass).unwrap().volume_db,
        main,
        "the main fader is left alone"
    );
    // Talkback on a pad.
    s.set_talkback_settings(TalkbackSettings {
        input: Some(2),
        level_db: -3.0,
        dim_db: -12.0,
    })
    .unwrap();
    s.dispatch(Action::MidiLearn(MappingTarget::Talkback))
        .unwrap();
    s.midi_keyboard().send(&[0x99, 36, 100]);
    s.tick(0.0);
    assert_eq!(s.midi_mappings_for(MappingTarget::Talkback).len(), 1);
    assert_eq!(s.mapping_target_label(&MappingTarget::Talkback), "Talkback");
    assert!(!s.talking());
    s.midi_keyboard().send(&[0x99, 36, 100]);
    s.tick(0.0);
    assert!(s.talking(), "down: talking");
    s.midi_keyboard().send(&[0x89, 36, 0]);
    s.tick(0.0);
    assert!(!s.talking(), "up: done");
}

/// A Mackie Control in the mixer's cue mode: its faders show and set the
/// shown cue mix's sends, the mute buttons their switches, the master
/// fader the cue mix's level; back on the main mix, the main faders.
#[test]
fn a_control_surface_follows_the_mixers_cue_mode() {
    use faderframe_core::FaderLaw;
    use faderframe_session::control::SurfaceKind;
    let mut s = demo();
    let id = new_cue(&mut s);
    let bass = track(&s, "Bass");
    s.dispatch(Action::Edit(Command::SetCueSendLevel {
        track: bass,
        cue: id,
        db: -15.0,
    }))
    .unwrap();
    let surface = s.add_virtual_surface(SurfaceKind::Mackie);
    s.tick(0.01);
    let law = FaderLaw::console();
    let sent = surface.take();
    // The bass is the second strip: its fader at the cue send's level.
    let at = law.db_to_position(-15.0);
    let v = (at * 16383.0).round() as u16;
    assert!(
        sent.contains(&vec![0xE1, (v & 0x7F) as u8, (v >> 7) as u8]),
        "the cue send on the fader"
    );
    // Moving it sets the send, not the main fader.
    let main = s.project().track(bass).unwrap().volume_db;
    surface.send(&[0x90, 0x69, 0x7F]);
    surface.send(&[0xE1, 0x00, 0x60]);
    s.tick(0.01);
    surface.send(&[0x90, 0x69, 0x00]);
    s.tick(0.01);
    let send = s
        .project()
        .track(bass)
        .unwrap()
        .cue_send(id)
        .unwrap()
        .level_db;
    assert!(
        (send - law.position_to_db(f32::from(0x60u16 << 7) / 16383.0)).abs() < 0.1,
        "{send}"
    );
    assert_eq!(s.project().track(bass).unwrap().volume_db, main);
    // Mute 2: the send off (and on again).
    surface.send(&[0x90, 0x11, 0x7F]);
    surface.send(&[0x90, 0x11, 0x00]);
    s.tick(0.01);
    assert!(!s.project().track(bass).unwrap().cue_send(id).unwrap().on);
    assert!(
        !s.project().track(bass).unwrap().mute,
        "the main mute untouched"
    );
    surface.send(&[0x90, 0x11, 0x7F]);
    surface.send(&[0x90, 0x11, 0x00]);
    s.tick(0.01);
    assert!(s.project().track(bass).unwrap().cue_send(id).unwrap().on);
    // The master fader: the cue mix's level.
    surface.send(&[0x90, 0x70, 0x7F]);
    surface.send(&[0xE8, 0x00, 0x40]);
    s.tick(0.01);
    surface.send(&[0x90, 0x70, 0x00]);
    s.tick(0.01);
    let level = s.project().cue(id).unwrap().level_db;
    assert!(
        (level - law.position_to_db(f32::from(0x40u16 << 7) / 16383.0)).abs() < 0.1,
        "{level}"
    );
    // Back on the main mix: the fader moves the main fader again.
    s.dispatch(Action::Cue(CueOp::Show(None))).unwrap();
    s.tick(0.01);
    surface.send(&[0x90, 0x69, 0x7F]);
    surface.send(&[0xE1, 0x00, 0x50]);
    s.tick(0.01);
    surface.send(&[0x90, 0x69, 0x00]);
    s.tick(0.01);
    assert!(
        (s.project().track(bass).unwrap().volume_db
            - law.position_to_db(f32::from(0x50u16 << 7) / 16383.0))
        .abs()
            < 0.1
    );
    assert!(
        (s.project()
            .track(bass)
            .unwrap()
            .cue_send(id)
            .unwrap()
            .level_db
            - send)
            .abs()
            < 0.1,
        "the send stays"
    );
}

/// Talkback settings: the input sizes the device, the button talks while
/// held or latched, and a cue mix can be deaf to it (undoable).
#[test]
fn talkback_settings_and_the_talk_button() {
    use faderframe_session::cues::{TalkOp, TalkbackSettings};
    let mut s = demo();
    let id = new_cue(&mut s);
    let ins = s.wanted_channels().0;
    s.set_talkback_settings(TalkbackSettings {
        input: Some(11),
        level_db: 0.0,
        dim_db: -18.0,
    })
    .unwrap();
    assert_eq!(s.wanted_channels().0, 12.max(ins), "the microphone's input");
    assert_eq!(s.talkback_settings().dim_db, -18.0);
    assert!(!s.talking());
    s.dispatch(Action::Talkback(TalkOp::Press)).unwrap();
    assert!(s.talking());
    s.dispatch(Action::Talkback(TalkOp::Release)).unwrap();
    assert!(!s.talking());
    s.dispatch(Action::Talkback(TalkOp::Latch(true))).unwrap();
    assert!(s.talking() && s.talkback_latched());
    s.dispatch(Action::Talkback(TalkOp::Release)).unwrap();
    assert!(s.talking(), "latched: a release does not end it");
    s.dispatch(Action::Talkback(TalkOp::Latch(false))).unwrap();
    assert!(!s.talking());
    let steps = s.history_steps().0.len();
    s.dispatch(Action::Cue(CueOp::SetTalkback { cue: id, on: false }))
        .unwrap();
    assert!(!s.project().cue(id).unwrap().talkback);
    assert_eq!(s.history_steps().0.len(), steps + 1);
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().cue(id).unwrap().talkback);
}
