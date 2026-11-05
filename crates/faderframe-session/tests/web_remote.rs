//! The phone/web remote on the demo, over a WebSocket on localhost: a
//! device pairs and gets the state, then patches as the project changes;
//! a fader drag from it is one undo step; the transport, markers and the
//! setlist answer it; a device that has not paired changes nothing; a
//! performer sees and moves only their own cue mix; a device's role
//! changes while it is connected and forgetting it disconnects it; a
//! firewall in the way of devices on the network is told.
#![allow(clippy::unwrap_used)]

use faderframe_core::{FaderLaw, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::Command;
use faderframe_remote::Role;
use faderframe_remote::client::Client;
use faderframe_session::cues::CueOp;
use faderframe_session::web_remote::{RemoteListen, RemoteOp, WebRemoteSettings};
use faderframe_session::{Action, Session};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

/// The demo with the remote on, on this computer only.
fn session() -> Session {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.set_web_remote(WebRemoteSettings {
        on: true,
        port: 0,
        listen: RemoteListen::ThisComputer,
        ..WebRemoteSettings::default()
    });
    assert!(s.web_remote_status().running, "{:?}", s.web_remote_status());
    s
}

/// Tick the session until the client gets a message of type `t`.
#[track_caller]
fn until(s: &mut Session, c: &mut Client, t: &str) -> Value {
    let end = Instant::now() + WAIT;
    while Instant::now() < end {
        s.tick(0.01);
        if let Some(v) = c.recv(Duration::from_millis(15)).unwrap()
            && v["t"] == t
        {
            return v;
        }
    }
    panic!("no {t} message");
}

/// Tick the session until `done` says so.
#[track_caller]
fn tick_until(s: &mut Session, c: &mut Client, done: impl Fn(&Session) -> bool) {
    let end = Instant::now() + WAIT;
    while Instant::now() < end {
        s.tick(0.01);
        let _ = c.recv(Duration::from_millis(5));
        if done(s) {
            return;
        }
    }
    panic!("never came about");
}

/// Ticks for a while (what should not happen has its chance).
fn idle(s: &mut Session, c: &mut Client, ms: u64) -> Vec<Value> {
    let end = Instant::now() + Duration::from_millis(ms);
    let mut got = Vec::new();
    while Instant::now() < end {
        s.tick(0.01);
        if let Ok(Some(v)) = c.recv(Duration::from_millis(5)) {
            got.push(v);
        }
    }
    got
}

/// Pair a device with the code offered now: its client and the state it
/// got.
fn pair(s: &mut Session, name: &str) -> (Client, Value) {
    let addr = s.web_remote_address().unwrap();
    let code = s.web_remote_status().code.unwrap();
    let mut c = Client::connect(addr).unwrap();
    c.send(&json!({"t": "pair", "code": code, "name": name}))
        .unwrap();
    let state = until(s, &mut c, "state");
    (c, state["state"].clone())
}

#[test]
fn a_paired_phone_follows_the_project_and_its_fader_drag_is_one_undo_step() {
    let mut s = session();
    let status = s.web_remote_status();
    assert!(status.url.unwrap().starts_with("http://127.0.0.1:"));
    assert!(status.pair_url.unwrap().contains("#pair="));
    let (mut phone, state) = pair(&mut s, "Phone");
    // Kept for the preferences.
    let devices = &s.web_remote_settings().devices;
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "Phone");
    assert_eq!(devices[0].role, Role::Full);
    assert_eq!(s.web_remote_status().connected.len(), 1);
    // The mixer as the demo has it.
    let bass = track(&s, "Bass");
    let key = bass.0.to_string();
    assert_eq!(state["role"]["kind"], json!("full"));
    assert_eq!(state["tracks"][&key]["name"], json!("Bass"));
    assert!(state["order"].as_array().unwrap().contains(&json!(bass.0)));
    assert!(state["master"].is_u64());
    let law = FaderLaw::console();
    let at = law.db_to_position(s.project().track(bass).unwrap().volume_db);
    let sent = state["tracks"][&key]["fader"].as_f64().unwrap() as f32;
    assert!((sent - at).abs() < 1e-3, "{sent} {at}");

    // Changed in FaderFrame: only that is sent.
    s.dispatch(Action::Edit(Command::SetTrackMute {
        track: bass,
        on: true,
    }))
    .unwrap();
    let patch = until(&mut s, &mut phone, "patch");
    let ops = patch["ops"].as_array().unwrap();
    assert!(
        ops.contains(&json!({"p": ["tracks", key, "mute"], "v": true})),
        "{ops:?}"
    );
    assert!(!patch.to_string().contains("\"Bass\""), "{patch}");

    // A fader drag: touch, moves, release — one undo step.
    let steps = s.history_steps().0.len();
    phone.send(&json!({"t": "touch", "on": true})).unwrap();
    for v in [0.5, 0.55, 0.6] {
        phone
            .send(&json!({"t": "fader", "track": bass.0, "travel": v}))
            .unwrap();
    }
    phone.send(&json!({"t": "touch", "on": false})).unwrap();
    let want = law.position_to_db(0.6);
    tick_until(&mut s, &mut phone, |s| {
        (s.project().track(bass).unwrap().volume_db - want).abs() < 0.01
    });
    idle(&mut s, &mut phone, 100);
    assert_eq!(s.history_steps().0.len(), steps + 1);
    assert_eq!(s.history_steps().0.last().unwrap(), "Web Remote");
    // Moves without a touch end their gesture at rest.
    phone
        .send(&json!({"t": "pan", "track": bass.0, "pan": -0.5}))
        .unwrap();
    tick_until(&mut s, &mut phone, |s| {
        (s.project().track(bass).unwrap().pan + 0.5).abs() < 1e-4
    });
    idle(&mut s, &mut phone, 600);
    assert_eq!(s.history_steps().0.len(), steps + 2);

    // The transport and markers.
    let looping = s.project().loop_enabled;
    phone
        .send(&json!({"t": "transport", "button": "loop"}))
        .unwrap();
    tick_until(&mut s, &mut phone, |s| s.project().loop_enabled != looping);
    let markers = s.project().markers.len();
    phone
        .send(&json!({"t": "transport", "button": "add_marker"}))
        .unwrap();
    // The patch can go out in the very tick that adds the marker: watch
    // for it while waiting for the marker.
    let end = Instant::now() + WAIT;
    let mut told = false;
    while !(told && s.project().markers.len() == markers + 1) {
        assert!(Instant::now() < end, "no patch with the marker");
        s.tick(0.01);
        if let Some(v) = phone.recv(Duration::from_millis(15)).unwrap()
            && v["t"] == "patch"
            && v.to_string().contains("markers")
        {
            told = true;
        }
    }
    // Undo from the phone takes the marker back.
    phone
        .send(&json!({"t": "transport", "button": "undo"}))
        .unwrap();
    tick_until(&mut s, &mut phone, |s| s.project().markers.len() == markers);

    // Meters go to a screen that shows them.
    phone
        .send(&json!({"t": "view", "screen": "mixer"}))
        .unwrap();
    let meters = until(&mut s, &mut phone, "meters");
    assert!(meters["m"].get(&key).is_some(), "{meters}");
}

#[test]
fn a_device_that_has_not_paired_changes_nothing() {
    let mut s = session();
    let bass = track(&s, "Bass");
    let addr = s.web_remote_address().unwrap();
    let mut stranger = Client::connect(addr).unwrap();
    stranger
        .send(&json!({"t": "mute", "track": bass.0, "on": true}))
        .unwrap();
    stranger
        .send(&json!({"t": "pair", "code": "x", "name": "x"}))
        .unwrap();
    stranger
        .send(&json!({"t": "hello", "token": "f".repeat(64)}))
        .unwrap();
    let got = idle(&mut s, &mut stranger, 2500);
    assert!(!s.project().track(bass).unwrap().mute);
    assert!(s.web_remote_settings().devices.is_empty());
    assert!(got.iter().all(|m| m["t"] != "state" && m["t"] != "patch"));
    let refused: Vec<&Value> = got.iter().filter(|m| m["t"] == "refused").collect();
    assert_eq!(refused.len(), 3, "{got:?}");
    assert_eq!(s.web_remote_status().wrong_codes, 1);
}

#[test]
fn a_performer_sees_and_moves_only_their_own_cue_mix() {
    let mut s = session();
    s.dispatch(Action::Cue(CueOp::New)).unwrap();
    s.dispatch(Action::Cue(CueOp::New)).unwrap();
    let cues: Vec<_> = s.project().cue_mixes.iter().map(|c| c.id).collect();
    let (mine, theirs) = (cues[0], cues[1]);
    assert_eq!(s.project().cue(mine).unwrap().name, "Cue 1");
    s.dispatch(Action::WebRemote(RemoteOp::NewCode(Role::Performer {
        cue: "cue 1".into(),
    })))
    .unwrap();
    let (mut drummer, state) = pair(&mut s, "Drummer");
    assert_eq!(state["role"]["kind"], json!("performer"));
    assert_eq!(state["role"]["cue"], json!(mine.0));
    let shown = state["cues"].as_array().unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0]["id"], json!(mine.0));
    assert_eq!(state["launcher"]["columns"], json!([]));
    assert_eq!(state["master"], Value::Null);

    // Their cue mix moves; the main mix and the other cue mix do not.
    let bass = track(&s, "Bass");
    let main = s.project().track(bass).unwrap().volume_db;
    let other = s.project().track(bass).unwrap().cue_send(theirs).copied();
    drummer
        .send(&json!({"t": "fader", "track": bass.0, "travel": 0.1}))
        .unwrap();
    drummer
        .send(&json!({"t": "cue_send", "cue": theirs.0, "track": bass.0, "travel": 0.1}))
        .unwrap();
    drummer
        .send(&json!({"t": "transport", "button": "play"}))
        .unwrap();
    let notices = idle(&mut s, &mut drummer, 300);
    assert!(
        notices.iter().filter(|m| m["t"] == "notice").count() >= 3,
        "{notices:?}"
    );
    drummer.send(&json!({"t": "touch", "on": true})).unwrap();
    drummer
        .send(&json!({"t": "cue_send", "cue": mine.0, "track": bass.0, "travel": 0.25}))
        .unwrap();
    drummer
        .send(&json!({"t": "cue_on", "cue": mine.0, "track": bass.0, "on": false}))
        .unwrap();
    drummer.send(&json!({"t": "touch", "on": false})).unwrap();
    let want = FaderLaw::console().position_to_db(0.25);
    tick_until(&mut s, &mut drummer, |s| {
        s.project()
            .track(bass)
            .unwrap()
            .cue_send(mine)
            .is_some_and(|c| (c.level_db - want).abs() < 0.01 && !c.on)
    });
    let t = s.project().track(bass).unwrap();
    assert_eq!(t.volume_db, main);
    assert_eq!(t.cue_send(theirs).copied(), other);
    assert!(!s.transport().playing);

    // Made the engineer's: the whole mixer, at once.
    let device = s.web_remote_settings().devices[0].id;
    s.dispatch(Action::WebRemote(RemoteOp::SetRole {
        device,
        role: Role::Full,
    }))
    .unwrap();
    let welcome = until(&mut s, &mut drummer, "welcome");
    assert_eq!(welcome["role"]["kind"], json!("full"));
    let state = until(&mut s, &mut drummer, "state");
    assert_eq!(state["state"]["cues"].as_array().unwrap().len(), 2);

    // Forgotten: told, and gone.
    s.dispatch(Action::WebRemote(RemoteOp::Forget(device)))
        .unwrap();
    let bye = until(&mut s, &mut drummer, "bye");
    assert_eq!(bye["why"], json!("forgotten"));
    assert!(s.web_remote_settings().devices.is_empty());
}

#[test]
fn the_setlist_and_lyrics_reach_a_viewer_that_changes_nothing() {
    let mut s = session();
    s.dispatch(Action::Setlist(
        faderframe_session::setlist::SetlistOp::Add {
            name: "Opener".into(),
            start: faderframe_timeline::MusicalTime::ZERO,
            end: faderframe_timeline::MusicalTime::from_quarters(16.0),
        },
    ))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetLyrics {
        lyrics: vec![faderframe_project::lyrics::LyricLine {
            start: faderframe_timeline::MusicalTime::from_quarters(4.0),
            end: faderframe_timeline::MusicalTime::from_quarters(8.0),
            text: "First line".into(),
        }],
    }))
    .unwrap();
    s.dispatch(Action::WebRemote(RemoteOp::NewCode(Role::Viewer)))
        .unwrap();
    let (mut viewer, state) = pair(&mut s, "Singer");
    assert_eq!(state["setlist"]["songs"][0]["name"], json!("Opener"));
    assert_eq!(state["lyrics"][0]["text"], json!("First line"));
    // At the demo's tempo, four quarters in.
    let bpm = state["transport"]["bpm"].as_f64().unwrap();
    let start = state["lyrics"][0]["start"].as_f64().unwrap();
    assert!((start - 4.0 * 60.0 / bpm).abs() < 1e-3, "{start} {bpm}");
    assert_eq!(state["tracks"], json!({}));
    viewer
        .send(&json!({"t": "show", "control": "enter"}))
        .unwrap();
    idle(&mut s, &mut viewer, 300);
    assert!(!s.show_mode());

    // The engineer's phone runs the show.
    s.dispatch(Action::WebRemote(RemoteOp::NewCode(Role::Full)))
        .unwrap();
    let (mut phone, _) = pair(&mut s, "Phone");
    phone
        .send(&json!({"t": "show_song", "song": 0, "go": false}))
        .unwrap();
    tick_until(&mut s, &mut phone, Session::show_mode);
    // The viewer sees show mode stand on the song.
    let end = Instant::now() + WAIT;
    let mut seen = false;
    while Instant::now() < end && !seen {
        s.tick(0.01);
        if let Ok(Some(v)) = viewer.recv(Duration::from_millis(10)) {
            seen = v["t"] == "patch" && v.to_string().contains("\"show\"");
        }
    }
    assert!(seen);
    phone
        .send(&json!({"t": "show", "control": "leave"}))
        .unwrap();
    tick_until(&mut s, &mut phone, |s| !s.show_mode());
}

#[test]
fn turning_the_remote_off_closes_its_port() {
    let mut s = session();
    let addr = s.web_remote_address().unwrap();
    let (mut phone, _) = pair(&mut s, "Phone");
    let mut off = s.web_remote_settings().clone();
    off.on = false;
    s.dispatch(Action::WebRemote(RemoteOp::Settings(off)))
        .unwrap();
    assert!(!s.web_remote_status().running);
    assert!(s.web_remote_address().is_none());
    assert!(phone.recv(WAIT).is_err());
    assert!(Client::connect(addr).is_err());
    // The device stays paired for the next time.
    assert_eq!(s.web_remote_settings().devices.len(), 1);
}

/// Serves the demo's remote on this computer for a browser to look at
/// (screenshots, trying the app): `FADERFRAME_REMOTE_PORT=8642
/// FADERFRAME_REMOTE_SECONDS=120 cargo test -p faderframe-session --test
/// web_remote -- --ignored serve_the_demo --nocapture` prints the address
/// with a pairing code (also written to `FADERFRAME_REMOTE_URL_FILE` when
/// set). The demo gets two cue mixes, a setlist, lyrics and a launcher
/// scene to show.
#[test]
#[ignore = "serves for a browser until FADERFRAME_REMOTE_SECONDS pass"]
fn serve_the_demo_for_a_browser() {
    use faderframe_session::setlist::SetlistOp;
    use faderframe_timeline::MusicalTime;
    let env = |k: &str| std::env::var(k).ok();
    let port = env("FADERFRAME_REMOTE_PORT")
        .and_then(|p| p.parse().ok())
        .unwrap_or(8642);
    let seconds: u64 = env("FADERFRAME_REMOTE_SECONDS")
        .and_then(|p| p.parse().ok())
        .unwrap_or(60);
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.dispatch(Action::Cue(CueOp::New)).unwrap();
    s.dispatch(Action::Cue(CueOp::New)).unwrap();
    let q = MusicalTime::from_quarters;
    for (name, a, b) in [
        ("Intro", 0.0, 16.0),
        ("Verse", 16.0, 48.0),
        ("Chorus", 48.0, 80.0),
    ] {
        s.dispatch(Action::Setlist(SetlistOp::Add {
            name: name.into(),
            start: q(a),
            end: q(b),
        }))
        .unwrap();
    }
    let words = [
        "Here comes the morning light",
        "Over the hills and far away",
        "Every road leads home tonight",
        "Sing it once and let it stay",
    ];
    s.dispatch(Action::Edit(Command::SetLyrics {
        lyrics: words
            .iter()
            .enumerate()
            .map(|(i, w)| faderframe_project::lyrics::LyricLine {
                start: q(i as f64 * 4.0),
                end: q(i as f64 * 4.0 + 3.5),
                text: (*w).into(),
            })
            .collect(),
    }))
    .unwrap();
    for name in ["Drums", "Bass"] {
        let clip = s
            .project()
            .track(track(&s, name))
            .unwrap()
            .clips
            .first()
            .copied();
        if let Some(clip) = clip {
            s.dispatch(Action::Launcher(
                faderframe_session::launcher::LauncherOp::SendClips(vec![clip]),
            ))
            .unwrap();
        }
    }
    s.set_web_remote(WebRemoteSettings {
        on: true,
        port,
        listen: RemoteListen::ThisComputer,
        ..WebRemoteSettings::default()
    });
    let url = s.web_remote_status().pair_url.unwrap();
    println!("remote: {url}");
    if let Some(f) = env("FADERFRAME_REMOTE_URL_FILE") {
        std::fs::write(f, &url).unwrap();
    }
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        s.tick(0.02);
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A firewall in the way of devices on the network (ufw's files under a
/// pretend root) is told when the remote starts, once while it stays on,
/// and shown with the command that lets them in until it does. Answering
/// this computer only, it is not in the way. (Linux: the remote answers
/// the network here, on 0.0.0.0.)
#[cfg(target_os = "linux")]
#[test]
fn a_firewall_in_the_way_is_told_with_the_command_that_lets_devices_in() {
    use faderframe_session::NoticeLevel;
    use std::fs;
    let root =
        std::env::temp_dir().join(format!("faderframe-remote-firewall-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let write = |file: &str, text: &str| {
        let path = root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write("etc/ufw/ufw.conf", "ENABLED=yes\n");
    write("etc/default/ufw", "DEFAULT_INPUT_POLICY=\"DROP\"\n");
    let localsend = "-A ufw-user-input -p tcp --dport 53317 -j ACCEPT\n";
    write("etc/ufw/user.rules", localsend);
    let warnings = |s: &Session| {
        s.notices()
            .filter(|n| n.level == NoticeLevel::Warning && n.text.contains("firewall (ufw)"))
            .count()
    };
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.set_firewall_root(Some(root.clone()));
    let on = |listen| WebRemoteSettings {
        on: true,
        port: 0,
        listen,
        ..WebRemoteSettings::default()
    };
    s.set_web_remote(on(RemoteListen::Network));
    let st = s.web_remote_status();
    assert!(st.running, "{st:?}");
    let note = st.firewall.expect("ufw is in the way");
    assert!(note.certain);
    assert!(
        note.command.starts_with("sudo ufw allow"),
        "{}",
        note.command
    );
    assert!(
        note.command.contains(&st.port.unwrap().to_string()),
        "{}",
        note.command
    );
    assert_eq!(warnings(&s), 1);
    // This computer only: loopback passes the firewall.
    s.set_web_remote(on(RemoteListen::ThisComputer));
    assert_eq!(s.web_remote_status().firewall, None);
    // Back to the network: shown again, not told again.
    s.set_web_remote(on(RemoteListen::Network));
    let port = s.web_remote_status().port.unwrap();
    assert!(s.web_remote_status().firewall.is_some());
    assert_eq!(warnings(&s), 1);
    // The port let in: gone, and said so.
    write(
        "etc/ufw/user.rules",
        &format!("{localsend}-A ufw-user-input -p tcp --dport {port} -j ACCEPT\n"),
    );
    s.set_firewall_root(Some(root.clone()));
    assert_eq!(s.web_remote_status().firewall, None);
    assert!(
        s.notices()
            .any(|n| n.level == NoticeLevel::Info && n.text.contains("reach the remote now"))
    );
    // Read again as it runs (every few seconds), whoever changes the rules.
    write("etc/ufw/user.rules", localsend);
    let end = Instant::now() + Duration::from_secs(10);
    while s.web_remote_status().firewall.is_none() && Instant::now() < end {
        s.tick(0.05);
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(s.web_remote_status().firewall.is_some());
    // Off and on again: told again.
    s.set_web_remote(WebRemoteSettings::default());
    assert_eq!(s.web_remote_status().firewall, None);
    s.set_web_remote(on(RemoteListen::Network));
    assert_eq!(warnings(&s), 2);
    let _ = fs::remove_dir_all(&root);
}
