//! Show mode's gear changes: sent when the show stands on a song, spaced
//! as a MIDI cable carries them; timed on a song's first frame (or on the
//! first frame of a song played into straight on), cancelled when the
//! transport jumps; edited as text, from `.syx` files and as controller
//! snapshots.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_engine::EngineConfig;
use faderframe_project::setlist::{AfterSong, GearMessage, GearTiming};
use faderframe_project::{ControllerLane, ControllerPoint, MidiController, Project, TrackKind};
use faderframe_session::setlist::{SetlistOp, ShowOp};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;
const SYNTH: &str = "virtual:Synth";

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

/// 240 BPM (a bar a second): songs A (bar 1), B (bar 3) and C (bar 4,
/// right after B), a virtual output to send to.
fn session() -> (Session, faderframe_midi_io::Captured) {
    let mut p = Project::new("Show", SR);
    p.timeline.tempo.set_initial_bpm(240.0);
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    let captured = s.add_virtual_midi_output("Synth");
    for (a, b) in [(0.0, 4.0), (8.0, 12.0), (12.0, 16.0)] {
        s.dispatch(Action::AddSection {
            start: q(a),
            end: q(b),
        })
        .unwrap();
    }
    s.dispatch(Action::Setlist(SetlistOp::FromSections))
        .unwrap();
    (s, captured)
}

fn audio(s: &mut Session) {
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            sample_rate: Some(SR),
            buffer_size: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
}

fn wait(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        s.tick(0.003);
        if done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out waiting for {what}");
}

fn sent(c: &faderframe_midi_io::Captured) -> Vec<(u64, Vec<u8>)> {
    c.lock().unwrap().clone()
}

/// Wait (ticking) until `n` messages went out, at most two seconds.
fn until_sent(s: &mut Session, c: &faderframe_midi_io::Captured, n: usize) -> Vec<(u64, Vec<u8>)> {
    let start = Instant::now();
    while sent(c).len() < n && start.elapsed() < Duration::from_secs(2) {
        s.tick(0.003);
        std::thread::sleep(Duration::from_millis(2));
    }
    sent(c)
}

#[test]
fn standing_on_a_song_changes_its_gear_spaced_as_a_cable_carries_it() {
    let (mut s, captured) = session();
    s.dispatch(Action::Setlist(SetlistOp::GearText(
        1,
        format!("on \"{SYNTH}\"\npc 5 bank 1\ncc 7=100 ch 2\nsysex F0 43 10 4C 00 00 7E 00 F7"),
    )))
    .unwrap();
    assert_eq!(s.history().undo_label(), Some("Song Gear"));
    assert_eq!(s.setlist().songs[1].gear.changes.len(), 3);
    s.dispatch(Action::Show(ShowOp::Enter)).unwrap();
    // Song A changes nothing.
    std::thread::sleep(Duration::from_millis(30));
    assert!(sent(&captured).is_empty());
    let asked = s.midi_clock_ns();
    s.dispatch(Action::Show(ShowOp::Next)).unwrap();
    assert_eq!(s.show().current, 1);
    let got = until_sent(&mut s, &captured, 5);
    let bytes: Vec<&[u8]> = got.iter().map(|(_, b)| b.as_slice()).collect();
    assert_eq!(
        bytes,
        [
            &[0xB0, 0, 0][..],
            &[0xB0, 32, 1],
            &[0xC0, 4],
            &[0xB1, 7, 100],
            &[0xF0, 0x43, 0x10, 0x4C, 0x00, 0x00, 0x7E, 0x00, 0xF7],
        ]
    );
    // Each after the one before had gone down the cable (320 µs a byte):
    // the dump after 11 bytes, 3.52 ms after the gear was sent — never
    // sooner (the sender counts 200 µs early as on time; a busy machine
    // may send any of them late, which this does not judge).
    let after = got[4].0.saturating_sub(asked);
    assert!(after >= 3_520_000 - 200_000, "{after} ns");
    // Standing on it again (stopped back on its start) sends nothing new;
    // Send Now does, and standing on it after another song.
    s.dispatch(Action::Show(ShowOp::Cue(1))).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(sent(&captured).len(), 5);
    s.dispatch(Action::Show(ShowOp::SendGear(1))).unwrap();
    assert_eq!(until_sent(&mut s, &captured, 10).len(), 10);
    s.dispatch(Action::Show(ShowOp::Cue(0))).unwrap();
    s.dispatch(Action::Show(ShowOp::Cue(1))).unwrap();
    assert_eq!(until_sent(&mut s, &captured, 15).len(), 15);
    assert_eq!(s.gear_scheduled().0, 15);
    // A song without gear: Send Now says so.
    assert!(s.dispatch(Action::Show(ShowOp::SendGear(0))).is_err());
}

#[test]
fn first_frame_changes_go_out_as_the_song_starts_and_straight_on_songs_get_theirs_there() {
    let (mut s, captured) = session();
    // B plays straight on into C; C's gear (timed for the cue) has no cue
    // there: it goes on C's first frame. A's goes on its own first frame.
    s.dispatch(Action::Setlist(SetlistOp::Then(1, AfterSong::Continue)))
        .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::GearText(
        0,
        format!("on \"{SYNTH}\"; pc 1"),
    )))
    .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::GearTiming(0, GearTiming::Start)))
        .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::GearText(
        2,
        format!("on \"{SYNTH}\"; pc 3"),
    )))
    .unwrap();
    audio(&mut s);
    s.dispatch(Action::Show(ShowOp::Enter)).unwrap();
    // Standing on A (first-frame timing): nothing yet.
    for _ in 0..20 {
        s.tick(0.003);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(sent(&captured).is_empty(), "{:?}", sent(&captured));
    let started = Instant::now();
    s.dispatch(Action::Show(ShowOp::PlayStop)).unwrap();
    let got = until_sent(&mut s, &captured, 1);
    assert_eq!(got[0].1, vec![0xC0, 0]);
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "A's change came as it started"
    );
    // B from its start, on into C: C's program change around its first
    // frame (a second after B's start), not when B was stood on.
    s.dispatch(Action::Show(ShowOp::Go(1))).unwrap();
    let b = Instant::now();
    wait(&mut s, |_| sent(&captured).len() >= 2, "C's change");
    let at = b.elapsed();
    assert_eq!(sent(&captured)[1].1, vec![0xC0, 2]);
    assert!(
        at > Duration::from_millis(700),
        "C's change came at C's start, after {at:?}"
    );
    assert!(s.gear_scheduled().1 >= 2);
    s.stop_audio();
}

#[test]
fn a_jump_cancels_first_frame_changes_not_yet_sent() {
    let (mut s, captured) = session();
    // A long dump on C's first frame (a second of cable time).
    let dump: String = std::iter::once("F0".to_string())
        .chain((0..3000).map(|_| "01".to_string()))
        .chain(std::iter::once("F7".to_string()))
        .collect::<Vec<_>>()
        .join(" ");
    s.dispatch(Action::Setlist(SetlistOp::GearText(
        2,
        format!("on \"{SYNTH}\"\nsysex {dump}\npc 9"),
    )))
    .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::GearTiming(2, GearTiming::Start)))
        .unwrap();
    audio(&mut s);
    s.dispatch(Action::Show(ShowOp::Enter)).unwrap();
    s.dispatch(Action::Show(ShowOp::Go(2))).unwrap();
    wait(
        &mut s,
        |s| s.gear_scheduled().1 >= 2,
        "C's changes scheduled",
    );
    wait(&mut s, |_| !sent(&captured).is_empty(), "the dump");
    // Jump away before the program change (a second behind the dump) went.
    s.dispatch(Action::Transport(TransportAction::Locate(q(1.0))))
        .unwrap();
    for _ in 0..400 {
        s.tick(0.003);
        std::thread::sleep(Duration::from_millis(3));
    }
    let got = sent(&captured);
    assert_eq!(
        got.len(),
        1,
        "the program change was cancelled: {}",
        got.len()
    );
    s.stop_audio();
}

#[test]
fn gear_comes_from_programs_files_and_controller_snapshots() {
    let (mut s, _captured) = session();
    // The dev action's way: a program on the default output's channel 1,
    // set again rather than added.
    s.dispatch(Action::Setlist(SetlistOp::GearProgram {
        song: 0,
        program: 4,
        bank: Some(2),
    }))
    .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::GearProgram {
        song: 0,
        program: 7,
        bank: None,
    }))
    .unwrap();
    let gear = &s.setlist().songs[0].gear;
    assert_eq!(gear.changes.len(), 1);
    assert_eq!(gear.changes[0].output, SYNTH);
    assert_eq!(
        gear.changes[0].message,
        GearMessage::Program {
            channel: 0,
            program: 7,
            bank: None
        }
    );
    // A .syx file: its messages, named by the file.
    let dir = std::env::temp_dir().join(format!("ff-gear-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("patch.syx");
    std::fs::write(&file, [0xF0, 1, 2, 0xF7, 0xF0, 3, 0xF7]).unwrap();
    s.dispatch(Action::Setlist(SetlistOp::GearSysexFile(1, file)))
        .unwrap();
    let gear = &s.setlist().songs[1].gear;
    assert_eq!(gear.changes.len(), 2);
    assert!(matches!(&gear.changes[0].message,
        GearMessage::SysEx { name, data } if name == "patch.syx" && data == &[0xF0, 1, 2, 0xF7]));
    // A snapshot of the controllers a track's clips set by B's start.
    let t = s.add_track(TrackKind::Midi).unwrap();
    s.dispatch(Action::CreateMidiClip {
        track: t,
        start: MusicalTime::ZERO,
        length: q(16.0),
    })
    .unwrap();
    let clip = s.project().clips_of(t)[0].id;
    let mut m = s.project().clip(clip).unwrap().as_midi().unwrap().clone();
    let mut volume = ControllerLane::new(MidiController::Cc { number: 7 }, 1);
    volume.points = vec![
        ControllerPoint {
            time: q(0.0),
            value: 50,
            fine: None,
        },
        ControllerPoint {
            time: q(6.0),
            value: 90,
            fine: None,
        },
        ControllerPoint {
            time: q(10.0),
            value: 10,
            fine: None,
        },
    ];
    m.controllers.push(volume);
    s.dispatch(Action::Edit(faderframe_project::Command::SetClipContent {
        clip,
        start: MusicalTime::ZERO,
        content: Box::new(faderframe_project::ClipContent::Midi(m)),
    }))
    .unwrap();
    s.dispatch(Action::Setlist(SetlistOp::GearSnapshot {
        song: 1,
        track: t,
    }))
    .unwrap();
    let gear = &s.setlist().songs[1].gear;
    assert_eq!(
        gear.changes.last().unwrap().message,
        GearMessage::Control {
            channel: 1,
            controller: 7,
            value: 90
        }
    );
    // Text that does not read is refused, the gear left as it was.
    let before = s.setlist().songs[1].gear.clone();
    assert!(
        s.dispatch(Action::Setlist(SetlistOp::GearText(1, "pc 200".into())))
            .is_err()
    );
    assert_eq!(s.setlist().songs[1].gear, before);
    // Saved with the project.
    let text = serde_json::to_string(s.project()).unwrap();
    let back: Project = serde_json::from_str(&text).unwrap();
    assert_eq!(back.setlist, s.project().setlist);
    let _ = std::fs::remove_dir_all(&dir);
}
