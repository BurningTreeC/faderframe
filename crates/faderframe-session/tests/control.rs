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

/// A Launchpad plays the launcher: it is put in programmer mode, its pads
/// light in the clips' colours (green pulsing once one plays), a pad
/// launches its slot, the arrows move its own bank (the Mackie's stays),
/// a scene button launches its scene.
#[test]
fn a_launchpad_plays_the_launcher_from_its_own_bank() {
    use faderframe_audio::dummy::DummyBackend;
    use faderframe_session::AudioPreferences;
    use faderframe_session::launcher::LauncherOp;
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    let mackie = s.add_virtual_surface(SurfaceKind::Mackie);
    let pad = s.add_virtual_surface(SurfaceKind::LaunchpadMiniMk3);
    let columns: Vec<TrackId> = s.launcher_tracks().iter().map(|t| t.id).collect();
    let first = s.project().clips_of(columns[0])[0].id;
    s.dispatch(Action::Launcher(LauncherOp::SendClips(vec![first])))
        .unwrap();
    s.dispatch(Action::Launcher(LauncherOp::SetQuantize(
        faderframe_project::launcher::LaunchQuantize::None,
    )))
    .unwrap();
    s.tick(0.01);
    let sent = pad.take();
    assert_eq!(
        sent[0],
        vec![0xF0, 0x00, 0x20, 0x29, 0x02, 0x0D, 0x0E, 0x01, 0xF7]
    );
    let lights: Vec<u8> = sent[1..].concat();
    let c = s.project().clips[&s
        .project()
        .launcher
        .clip(columns[0], s.project().launcher.scenes[0].id)
        .unwrap()]
        .color
        .unwrap_or(s.project().track(columns[0]).unwrap().color);
    assert!(
        lights
            .windows(5)
            .any(|w| w == [3, 81, c.r >> 1, c.g >> 1, c.b >> 1]),
        "the top-left pad in the clip's colour: {lights:?}"
    );
    // The pad launches it; it pulses green.
    pad.send(&[0x90, 81, 127]);
    pad.send(&[0x90, 81, 0]);
    let start = std::time::Instant::now();
    loop {
        s.tick(0.005);
        if s.launch_state(columns[0])
            .is_some_and(|l| l.playing.is_some())
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "launched by the pad"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    s.tick(0.005);
    let lights: Vec<u8> = pad.take().concat();
    assert!(lights.windows(3).any(|w| w == [2, 81, 21]), "{lights:?}");
    // Its bank moves; the Mackie's does not.
    let _ = mackie.take();
    pad.send(&[0xB0, 94, 127]);
    pad.send(&[0xB0, 94, 0]);
    s.tick(0.01);
    assert_eq!(s.surface_bank(), 1);
    let lights: Vec<u8> = pad.take().concat();
    assert!(
        lights.windows(3).any(|w| w == [0, 81, 0]),
        "a new first column: {lights:?}"
    );
    // The scene button: its scene.
    s.dispatch(Action::Launcher(LauncherOp::StopAll)).unwrap();
    pad.send(&[0xB0, 89, 127]);
    let start = std::time::Instant::now();
    loop {
        s.tick(0.005);
        if s.launch_state(columns[0])
            .and_then(|l| l.queued)
            .is_some_and(|q| q.0.is_some())
            || s.launch_state(columns[0])
                .is_some_and(|l| l.playing.is_some())
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the scene launched"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The Mackie's other buttons: the plug-in page puts the selected
/// track's device on the pots and the LCD (a pot moves its parameter),
/// the track types filter the strips, SMPTE/Beats switches the time
/// display, Shift + Undo redoes, F2 switches the workspace, the arrows
/// zoom in zoom mode, Option + Solo solos every track and the global
/// Solo clears them.
#[test]
fn the_mackies_other_buttons() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let m = s.add_virtual_surface(SurfaceKind::Mackie);
    let press = |m: &faderframe_midi_io::VirtualSurface, note: u8| {
        m.send(&[0x90, note, 0x7F]);
        m.send(&[0x90, note, 0x00]);
    };
    // A track with a device.
    let t = s
        .project()
        .tracks
        .iter()
        .find(|t| t.kind == faderframe_project::TrackKind::Audio && !t.slots().is_empty())
        .map(|t| t.id)
        .expect("a track with a device");
    s.dispatch(Action::SelectTracks {
        tracks: vec![t],
        mode: faderframe_session::SelectMode::Replace,
    })
    .unwrap();
    s.tick(0.01);
    let before = lcd_names(&m.take()).unwrap();
    press(&m, 0x2B);
    s.tick(0.01);
    let sent = m.take();
    let names = lcd_names(&sent).unwrap();
    assert_ne!(names, before, "the device's parameters: {names}");
    assert!(sent.contains(&vec![0x90, 0x2B, 0x7F]), "the plug-in LED");
    // The first pot moves the device's first parameter.
    let plugin = s.project().track(t).unwrap().slots()[0].id;
    let first = s
        .automatable_parameters(t)
        .into_iter()
        .find(|p| {
            matches!(p.target, faderframe_automation::AutomationTarget::PluginParameter { plugin: q, .. } if q == plugin)
        })
        .unwrap();
    let was = s.display_value(t, first.target).unwrap();
    m.send(&[0xB0, 0x10, 0x10]);
    s.tick(0.01);
    let now = s.display_value(t, first.target).unwrap();
    assert_ne!(was, now, "{} moved", first.name);
    // Shift + Undo: undone and redone.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.display_value(t, first.target).unwrap(), was);
    m.send(&[0x90, 0x46, 0x7F]);
    press(&m, 0x51);
    m.send(&[0x90, 0x46, 0x00]);
    s.tick(0.01);
    assert_eq!(s.display_value(t, first.target).unwrap(), now, "redone");
    // Back to pan; only the audio tracks on the strips.
    press(&m, 0x2A);
    press(&m, 0x40);
    s.tick(0.01);
    assert!(
        s.surface_tracks()
            .iter()
            .all(|t| t.kind == faderframe_project::TrackKind::Audio)
    );
    press(&m, 0x33);
    s.tick(0.01);
    assert!(s.surface_tracks().len() > 3, "every track again");
    // SMPTE: its LED, BEATS off.
    press(&m, 0x35);
    s.tick(0.01);
    let sent = m.take();
    assert!(sent.contains(&vec![0x90, 0x71, 0x7F]) && sent.contains(&vec![0x90, 0x72, 0x00]));
    // F2: the second workspace.
    press(&m, 0x37);
    s.tick(0.01);
    assert_eq!(s.workspace().active, 1);
    // Zoom mode: the right arrow zooms in.
    let zoom = s.editor.zoom_request.0;
    press(&m, 0x64);
    press(&m, 0x63);
    s.tick(0.01);
    assert_ne!(s.editor.zoom_request.0, zoom);
    // Option + Solo: every strip soloed; the global Solo clears it.
    m.send(&[0x90, 0x47, 0x7F]);
    press(&m, 0x08);
    m.send(&[0x90, 0x47, 0x00]);
    s.tick(0.01);
    assert!(s.surface_tracks().iter().all(|t| t.solo));
    press(&m, 0x5A);
    s.tick(0.01);
    assert!(s.project().tracks.iter().all(|t| !t.solo));
}

/// HUI: the keypad locates to a bar and recalls markers, edit keys edit,
/// the window keys show views; a surface that never answers the pings
/// shows as off line.
#[test]
fn a_hui_keypad_and_its_other_zones() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let h = s.add_virtual_surface(SurfaceKind::Hui);
    let press = |h: &faderframe_midi_io::VirtualSurface, zone: u8, port: u8| {
        h.send(&[0xB0, 0x0F, zone]);
        h.send(&[0xB0, 0x2F, 0x40 | port]);
        h.send(&[0xB0, 0x2F, port]);
    };
    // "5", enter: bar 5.
    press(&h, 0x13, 4);
    press(&h, 0x14, 0);
    s.tick(0.01);
    let bar5 = s.project().timeline.meter.bar_start(4);
    assert!((s.playhead().quarters() - bar5.quarters()).abs() < 0.01);
    // Enter alone: a marker here.
    let markers = s.project().markers.len();
    press(&h, 0x14, 0);
    s.tick(0.01);
    assert_eq!(s.project().markers.len(), markers + 1);
    // ".1.": the first marker.
    s.dispatch(Action::Transport(
        faderframe_session::TransportAction::Locate(
            faderframe_timeline::MusicalTime::from_quarters(1.0),
        ),
    ))
    .unwrap();
    press(&h, 0x13, 5);
    press(&h, 0x13, 1);
    press(&h, 0x13, 5);
    s.tick(0.01);
    let mut at: Vec<_> = s.project().markers.iter().map(|m| m.position).collect();
    at.sort();
    assert!((s.playhead().quarters() - at[0].quarters()).abs() < 0.01);
    // The transport window key shows the launcher (a tab behind the
    // mixer).
    let layout = s.layout_revision();
    press(&h, 0x09, 2);
    s.tick(0.01);
    assert_ne!(s.layout_revision(), layout);
    // Nobody answers the pings: off line after a few seconds.
    assert_eq!(s.control_surface_online(), vec![None]);
}

/// A dual-panned track on the pan page: the pot moves the image as a
/// whole (stopping at the side, keeping its width); with Shift held only
/// the left side, with Option only the right.
#[test]
fn a_pan_pot_moves_a_dual_panned_image() {
    use faderframe_core::{DualPan, PanMode};
    use faderframe_project::Command;
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let drums = track(&s, "Drums");
    s.dispatch(Action::Edit(Command::SetTrackPanMode {
        track: drums,
        mode: PanMode::Dual,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackDualPan {
        track: drums,
        pan: DualPan {
            left: -0.6,
            right: 0.2,
        },
    }))
    .unwrap();
    let surface = s.add_virtual_surface(SurfaceKind::Mackie);
    s.tick(0.01);
    surface.take();
    let dual = |s: &Session| s.project().track(drums).unwrap().dual_pan;
    // Five ticks right: both sides move alike.
    surface.send(&[0xB0, 0x10, 0x05]);
    s.tick(0.01);
    let d = dual(&s);
    assert!(d.left > -0.6 && d.right > 0.2, "{d:?}");
    assert!(((d.right - d.left) - 0.8).abs() < 1e-5, "width kept: {d:?}");
    // Shift held: the left side alone.
    let before = dual(&s);
    surface.send(&[0x90, 0x46, 0x7F]);
    surface.send(&[0xB0, 0x10, 0x45]);
    s.tick(0.01);
    surface.send(&[0x90, 0x46, 0x00]);
    let d = dual(&s);
    assert!(d.left < before.left && d.right == before.right, "{d:?}");
    // Option held: the right alone.
    let before = d;
    surface.send(&[0x90, 0x47, 0x7F]);
    surface.send(&[0xB0, 0x10, 0x03]);
    s.tick(0.01);
    surface.send(&[0x90, 0x47, 0x00]);
    let d = dual(&s);
    assert!(d.right > before.right && d.left == before.left, "{d:?}");
    // Against the right side the image stops, as wide as it was.
    s.dispatch(Action::Edit(Command::SetTrackDualPan {
        track: drums,
        pan: DualPan {
            left: 0.4,
            right: 0.95,
        },
    }))
    .unwrap();
    for _ in 0..10 {
        surface.send(&[0xB0, 0x10, 0x0F]);
        s.tick(0.01);
    }
    let d = dual(&s);
    assert!(
        (d.right - 1.0).abs() < 1e-5 && (d.left - 0.45).abs() < 1e-5,
        "{d:?}"
    );
}

/// A KeyLab Essential mk3 in its DAW program: connected on its first
/// update, its transport plays, its absolute knob sets the pan where it
/// is, its fader the volume; Redo redoes.
#[test]
fn a_keylab_essential_mk3_runs_the_transport_and_the_strips() {
    use faderframe_control::controllers::Controller;
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let surface = s.add_virtual_surface(SurfaceKind::Controller(Controller::KeyLabEssentialMk3));
    s.tick(0.01);
    let sent = surface.take();
    assert_eq!(
        sent[0],
        [
            0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x02, 0x0F, 0x40, 0x5A, 0x01, 0xF7
        ],
        "the DAW connection"
    );
    let bass = track(&s, "Bass");
    // Knob 2 hard left (absolute 0): pan −1; fader 2 at the top.
    surface.send(&[0xB0, 97, 0]);
    surface.send(&[0xB0, 106, 127]);
    s.tick(0.01);
    let t = s.project().track(bass).unwrap();
    assert!((t.pan + 1.0).abs() < 1e-6, "{}", t.pan);
    let top = FaderLaw::console().position_to_db(1.0);
    assert!((t.volume_db - top).abs() < 0.01, "{}", t.volume_db);
    // Undo the fader and the knob, then Redo.
    s.dispatch(Action::Undo).unwrap();
    let undone = s.project().track(bass).unwrap().volume_db;
    surface.send(&[0xB0, 43, 127]);
    surface.send(&[0xB0, 43, 0]);
    s.tick(0.01);
    assert_ne!(s.project().track(bass).unwrap().volume_db, undone, "redone");
    // Play.
    surface.send(&[0xB0, 21, 127]);
    surface.send(&[0xB0, 21, 0]);
    s.tick(0.01);
    assert!(s.transport().playing || s.engine().transport_snapshot().playing);
}

/// An X-Touch One shows the selected track on its one strip and moves it.
#[test]
fn an_x_touch_one_follows_the_selection() {
    use faderframe_control::controllers::Controller;
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let surface = s.add_virtual_surface(SurfaceKind::Controller(Controller::XTouchOne));
    let bass = track(&s, "Bass");
    s.dispatch(Action::SelectTracks {
        tracks: vec![bass],
        mode: faderframe_session::SelectMode::Replace,
    })
    .unwrap();
    s.tick(0.01);
    let names = lcd_names(&surface.take()).unwrap();
    assert!(names.starts_with("Bass"), "{names}");
    // Its fader (pitch bend channel 1) moves the selected track.
    surface.send(&[0x90, 0x68, 0x7F]);
    surface.send(&[0xE0, 0x00, 0x20]);
    s.tick(0.01);
    surface.send(&[0x90, 0x68, 0x00]);
    s.tick(0.01);
    let law = FaderLaw::console();
    let db = s.project().track(bass).unwrap().volume_db;
    assert!(
        (db - law.position_to_db(4096.0 / 16383.0)).abs() < 0.01,
        "{db}"
    );
}

/// Find Controllers sets up a known keyboard on its DAW port (none here:
/// no hardware; the catalogue's own detection is tested in the crate).
#[test]
fn finding_controllers_without_any_finds_none() {
    let s = Session::demo(EngineConfig::default()).unwrap();
    assert!(s.detect_control_surfaces().is_empty());
}

/// The FaderPort (2018): its fader on the selected track; Marker mode's
/// Next goes to the next marker; Bypass bypasses the track's devices and
/// again brings them back; Shift + Mute unmutes every track.
#[test]
fn a_faderport_with_one_fader_navigates_by_its_modes() {
    use faderframe_control::controllers::Controller;
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let surface = s.add_virtual_surface(SurfaceKind::Controller(Controller::FaderPort));
    // A track with devices (for Bypass).
    let drums = s
        .project()
        .tracks
        .iter()
        .find(|t| !t.inserts.is_empty())
        .unwrap()
        .id;
    s.dispatch(Action::SelectTracks {
        tracks: vec![drums],
        mode: faderframe_session::SelectMode::Replace,
    })
    .unwrap();
    s.tick(0.01);
    assert!(
        surface.take().contains(&vec![0xA0, 0x00, 0x00]),
        "kept alive"
    );
    // The fader moves the selected track.
    surface.send(&[0x90, 0x68, 0x7F]);
    surface.send(&[0xE0, 0x00, 0x30]);
    s.tick(0.01);
    surface.send(&[0x90, 0x68, 0x00]);
    s.tick(0.01);
    let law = FaderLaw::console();
    let db = s.project().track(drums).unwrap().volume_db;
    assert!(
        (db - law.position_to_db(6144.0 / 16383.0)).abs() < 0.01,
        "{db}"
    );
    // Marker mode, Next: the first marker after the playhead.
    let first = s
        .project()
        .markers
        .iter()
        .map(|m| m.position)
        .min()
        .expect("the demo has markers");
    surface.send(&[0x90, 0x3D, 0x7F]);
    surface.send(&[0x90, 0x2F, 0x7F]);
    s.tick(0.01);
    assert!(
        (s.playhead().quarters() - first.quarters()).abs() < 1e-3,
        "{} vs {}",
        s.playhead().quarters(),
        first.quarters()
    );
    // Bypass: every device of the track off, then on again.
    let devices = s.project().track(drums).unwrap().inserts.len();
    assert!(devices > 0);
    surface.send(&[0x90, 0x03, 0x7F]);
    s.tick(0.01);
    assert!(
        s.project()
            .track(drums)
            .unwrap()
            .inserts
            .iter()
            .all(|i| i.bypass)
    );
    surface.send(&[0x90, 0x03, 0x7F]);
    s.tick(0.01);
    assert!(
        s.project()
            .track(drums)
            .unwrap()
            .inserts
            .iter()
            .all(|i| !i.bypass)
    );
    // Mute, then Shift + Mute: every mute off.
    surface.send(&[0x90, 0x10, 0x7F]);
    s.tick(0.01);
    assert!(s.project().track(drums).unwrap().mute);
    surface.send(&[0x90, 0x46, 0x7F]);
    surface.send(&[0x90, 0x10, 0x7F]);
    surface.send(&[0x90, 0x46, 0x00]);
    s.tick(0.01);
    assert!(s.project().tracks.iter().all(|t| !t.mute));
}

/// The FaderPort (2018)'s F1–F4 (Shift + Master/Click/Section/Marker)
/// switch workspaces, as the session's function keys do.
#[test]
fn a_faderports_function_keys_switch_workspaces() {
    use faderframe_control::controllers::Controller;
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let surface = s.add_virtual_surface(SurfaceKind::Controller(Controller::FaderPort));
    s.tick(0.01);
    let before = s.playhead();
    let layout = s.layout_revision();
    surface.send(&[0x90, 0x46, 0x7F]);
    surface.send(&[0x90, 0x3B, 0x7F]);
    surface.send(&[0x90, 0x46, 0x00]);
    s.tick(0.01);
    assert_ne!(s.layout_revision(), layout, "a workspace switch");
    assert_eq!(s.playhead(), before, "no marker jump");
}
