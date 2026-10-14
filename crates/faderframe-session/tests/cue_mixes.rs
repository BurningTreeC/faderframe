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
    // Every track feeding cue mixes has its send, as the main mix has it.
    for t in &s.project().tracks {
        let send = t.cue_send(id).copied();
        if feeds_cues(t.kind) {
            assert_eq!(send, Some(s.project().main_mix_send(t, &c)), "{}", t.name);
        } else {
            assert_eq!(send, None, "{}", t.name);
        }
    }
    let lead = s.project().track(track(&s, "Bass")).unwrap();
    let send = lead.cue_send(id).unwrap();
    assert!(send.on);
    assert_eq!(send.level_db, lead.volume_db);
    // Buses and returns stay off (they carry the main mix's solos).
    let bus = s.project().track(track(&s, "Drum Bus")).unwrap();
    assert!(!bus.cue_send(id).unwrap().on);
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
    // A bus joins switched off; a MIDI track has no audio to send.
    let bus = s.add_track(TrackKind::Bus).unwrap();
    assert!(!s.project().track(bus).unwrap().cue_send(a).unwrap().on);
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
