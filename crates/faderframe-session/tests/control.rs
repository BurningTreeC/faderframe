//! Control surfaces: a Mackie Control shows the demo's first strips and
//! moves them (a fader move from touch to release is one undo step),
//! banks, toggles and runs the transport; OSC does the same over UDP and
//! answers on the reply port.
#![allow(clippy::unwrap_used)]

use faderframe_control::osc::{Arg, Message, decode, encode};
use faderframe_core::{FaderLaw, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_session::control::{SurfaceKind, SurfaceSettings};
use faderframe_session::{Action, Session};
use std::net::UdpSocket;
use std::time::Duration;

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

/// The LCD's upper line (names) as last sent.
fn lcd_names(sent: &[Vec<u8>]) -> Option<String> {
    sent.iter()
        .rev()
        .find(|m| m.len() > 7 && m[..7] == [0xF0, 0, 0, 0x66, 0x14, 0x12, 0])
        .map(|m| String::from_utf8_lossy(&m[7..m.len() - 1]).to_string())
}

#[test]
fn a_mackie_control_shows_and_moves_the_mixer() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let surface = s.add_virtual_surface(SurfaceKind::Mackie);
    s.tick(0.01);
    let sent = surface.take();
    let names = lcd_names(&sent).unwrap();
    assert!(names.starts_with("Drums  Bass   Pluck"), "{names}");
    // The faders where the tracks are.
    let law = FaderLaw::console();
    let bass = track(&s, "Bass");
    let at = law.db_to_position(s.project().track(bass).unwrap().volume_db);
    let v = (at * 16383.0).round() as u16;
    assert!(sent.contains(&vec![0xE1, (v & 0x7F) as u8, (v >> 7) as u8]));
    // Touch, move, release: one undo step.
    let steps = s.history_steps().0.len();
    surface.send(&[0x90, 0x69, 0x7F]);
    for v in [9000u16, 10000, 12000] {
        surface.send(&[0xE1, (v & 0x7F) as u8, (v >> 7) as u8]);
    }
    s.tick(0.01);
    surface.send(&[0x90, 0x69, 0x00]);
    s.tick(0.01);
    let db = s.project().track(bass).unwrap().volume_db;
    assert!(
        (db - law.position_to_db(12000.0 / 16383.0)).abs() < 0.01,
        "{db}"
    );
    assert_eq!(s.history_steps().0.len(), steps + 1);
    // Mute 2 and solo 1 toggle; the LEDs follow.
    surface.take();
    surface.send(&[0x90, 0x11, 0x7F]);
    surface.send(&[0x90, 0x11, 0x00]);
    s.tick(0.01);
    assert!(s.project().track(bass).unwrap().mute);
    assert!(surface.take().contains(&vec![0x90, 0x11, 0x7F]));
    // A pan pot turned left.
    surface.send(&[0xB0, 0x11, 0x45]);
    s.tick(0.01);
    let pan = s.project().track(bass).unwrap().pan;
    assert!(pan < 0.0, "{pan}");
    // Bank right: the next eight strips.
    surface.send(&[0x90, 0x2F, 0x7F]);
    s.tick(0.01);
    assert_eq!(s.surface_bank(), 9);
    let names = lcd_names(&surface.take()).unwrap();
    assert!(!names.starts_with("Drums"), "{names}");
    surface.send(&[0x90, 0x2E, 0x7F]);
    s.tick(0.01);
    assert_eq!(s.surface_bank(), 1);
    // Undo from the surface (the pot's gesture ended once it rested).
    std::thread::sleep(std::time::Duration::from_millis(450));
    s.tick(0.01);
    surface.send(&[0x90, 0x51, 0x7F]);
    s.tick(0.01);
    assert_eq!(s.project().track(bass).unwrap().pan, pan_before(&s, bass));
}

/// The pan before the pot (the demo's).
fn pan_before(s: &Session, t: TrackId) -> f32 {
    faderframe_project::demo::demo_project(s.project().sample_rate)
        .tracks
        .iter()
        .find(|x| x.name == s.project().track(t).unwrap().name)
        .unwrap()
        .pan
}

#[test]
fn osc_moves_the_mixer_and_answers() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    // Free ports for FaderFrame and for the "tablet".
    let tablet = UdpSocket::bind("127.0.0.1:0").unwrap();
    tablet
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let listen = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    s.set_control_surfaces(vec![SurfaceSettings {
        kind: SurfaceKind::Osc,
        listen,
        reply: tablet.local_addr().unwrap().port(),
        strips: 4,
        ..SurfaceSettings::default()
    }]);
    assert_eq!(s.control_surface_errors(), [None]);
    let send = |m: Message| {
        tablet.send_to(&encode(&m), ("127.0.0.1", listen)).unwrap();
    };
    send(Message::new("/refresh", vec![]));
    send(Message::new("/strip/1/mute", vec![Arg::Int(1)]));
    send(Message::new("/strip/2/fader", vec![Arg::Float(0.5)]));
    std::thread::sleep(Duration::from_millis(50));
    s.tick(0.01);
    let drums = track(&s, "Drums");
    let bass = track(&s, "Bass");
    assert!(s.project().track(drums).unwrap().mute);
    let db = s.project().track(bass).unwrap().volume_db;
    assert!((db - FaderLaw::console().position_to_db(0.5)).abs() < 0.01);
    // The answers: names, the mute, the fader.
    let mut got = Vec::new();
    let mut buf = [0u8; 2048];
    while let Ok((n, _)) = tablet.recv_from(&mut buf) {
        got.extend(decode(&buf[..n]));
    }
    assert!(got.contains(&Message::new(
        "/strip/1/name",
        vec![Arg::Str("Drums".into())]
    )));
    assert!(got.contains(&Message::new("/strip/1/mute", vec![Arg::Int(1)])));
    assert!(got.contains(&Message::new("/strip/2/fader", vec![Arg::Float(0.5)])));
    // Gestures end once the fader rests.
    std::thread::sleep(Duration::from_millis(450));
    s.tick(0.01);
    s.dispatch(Action::Undo).unwrap();
    let db_after = s.project().track(bass).unwrap().volume_db;
    assert!((db_after - db).abs() > 0.01, "the move was its own step");
}

#[test]
fn send_pages_flip_and_automation_from_a_mackie_control() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let surface = s.add_virtual_surface(SurfaceKind::Mackie);
    s.tick(0.01);
    // A shown track with a send.
    let tracks: Vec<TrackId> = s.surface_tracks().iter().map(|t| t.id).collect();
    let (i, t) = tracks
        .iter()
        .take(8)
        .enumerate()
        .find(|(_, t)| !s.project().track(**t).unwrap().sends.is_empty())
        .map(|(i, t)| (i, *t))
        .unwrap();
    let send_db = |s: &Session| s.project().track(t).unwrap().sends[0].level_db;
    let before = send_db(&s);
    // Send: the pots move the first send.
    surface.send(&[0x90, 0x29, 0x7F]);
    s.tick(0.01);
    surface.send(&[0xB0, 0x10 + i as u8, 0x05]);
    s.tick(0.01);
    assert!(send_db(&s) > before, "{} → {}", before, send_db(&s));
    // Flip: the fader moves the send, to the top.
    surface.send(&[0x90, 0x32, 0x7F]);
    s.tick(0.01);
    let vol = s.project().track(t).unwrap().volume_db;
    surface.send(&[0x90, 0x68 + i as u8, 0x7F]);
    surface.send(&[0xE0 | i as u8, 0x7F, 0x7F]);
    surface.send(&[0x90, 0x68 + i as u8, 0x00]);
    s.tick(0.01);
    assert!((send_db(&s) - FaderLaw::console().max_db()).abs() < 0.01);
    assert_eq!(
        s.project().track(t).unwrap().volume_db,
        vol,
        "not the volume"
    );
    // The LEDs: Send and Flip lit, Pan dark; "S1" on the display.
    let sent = surface.take();
    assert!(sent.contains(&vec![0x90, 0x29, 0x7F]));
    assert!(sent.contains(&vec![0x90, 0x32, 0x7F]));
    assert!(sent.contains(&vec![0xB0, 0x4B, b'S' - 0x40]));
    // Touch on the selected track: its lanes (a volume lane made) in Touch.
    s.dispatch(Action::SelectTracks {
        tracks: vec![t],
        mode: faderframe_session::SelectMode::Replace,
    })
    .unwrap();
    surface.send(&[0x90, 0x4D, 0x7F]);
    s.tick(0.01);
    let lanes = &s.project().track(t).unwrap().automation.lanes;
    assert!(!lanes.is_empty());
    assert!(
        lanes
            .iter()
            .all(|l| l.mode == faderframe_session::AutomationMode::Touch)
    );
    s.tick(0.01);
    assert!(surface.take().contains(&vec![0x90, 0x4D, 0x7F]));
}
