#![allow(clippy::unwrap_used)]
//! Ambisonic fields in the session: the Channel Format menu offers them,
//! tracks feeding one are panned by direction (lanes, undo, files), a
//! field decodes into the master, and the listener's head turns by hand,
//! by a head tracker over OSC — on its own port or an OSC surface's — and
//! is recentred.

use faderframe_control::osc::{Arg, Message, encode};
use faderframe_core::ambisonics::Orientation;
use faderframe_core::{AmbiOrder, ChannelLayout, SurroundPan, SurroundParam, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, OutputRouting, Project, TrackKind, file};
use faderframe_session::control::{SurfaceKind, SurfaceSettings};
use faderframe_session::headtrack::{HeadOp, HeadTrackingSettings};
use faderframe_session::{Action, AutomationTarget, Session};
use std::net::UdpSocket;
use std::time::Duration;

fn session(master: ChannelLayout) -> Session {
    let mut s = Session::new(Project::new("Field", 48_000), None, EngineConfig::default()).unwrap();
    let m = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: m,
        layout: master,
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

/// A free UDP port on this machine.
fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn yaw(s: &Session) -> f32 {
    s.head_orientation().ypr().0
}

#[test]
fn the_channel_format_menu_offers_the_ambisonic_orders() {
    let mut s = session(ChannelLayout::Stereo);
    let bus = s.add_track(TrackKind::Bus).unwrap();
    let choices = s.format_choices(bus);
    for o in AmbiOrder::ALL {
        let c = choices
            .iter()
            .find(|c| c.label.starts_with(&format!("AmbiX{} ", o.order())))
            .unwrap_or_else(|| {
                panic!(
                    "no {o:?} in {:?}",
                    choices.iter().map(|c| &c.label).collect::<Vec<_>>()
                )
            });
        assert!(!c.checked);
    }
    let third = choices
        .into_iter()
        .find(|c| c.label.starts_with("AmbiX3"))
        .unwrap();
    s.dispatch(third.action).unwrap();
    assert_eq!(
        s.project().track(bus).unwrap().layout,
        ChannelLayout::Ambisonic(AmbiOrder::Third)
    );
    assert!(
        s.format_choices(bus)
            .iter()
            .any(|c| c.checked && c.label.starts_with("AmbiX3"))
    );
    // Kept in the file.
    let text = file::to_string(s.project(), None).unwrap();
    assert!(text.contains("\"ambisonic\": 3"), "{text}");
    let loaded = file::from_str(&text).unwrap().project;
    assert_eq!(
        loaded.track(bus).unwrap().layout,
        ChannelLayout::Ambisonic(AmbiOrder::Third)
    );
}

#[test]
fn a_track_into_a_field_is_panned_and_automated_by_direction() {
    let mut s = session(ChannelLayout::Ambisonic(AmbiOrder::Second));
    let t = track(&mut s, ChannelLayout::Mono);
    let tr = s.project().track(t).unwrap();
    assert_eq!(s.project().ambisonic_panned(tr), Some(AmbiOrder::Second));
    assert_eq!(s.project().surround_panned(tr), None);
    let n = names(&s, t);
    for want in [
        "Surround Azimuth",
        "Surround Elevation",
        "Surround Spread",
        "Surround Distance",
    ] {
        assert!(n.contains(&want.to_string()), "{want} in {n:?}");
    }
    assert!(!n.contains(&"Surround Left/Right".to_string()));
    assert!(!n.contains(&"LFE Send".to_string()));
    assert!(!n.contains(&"Pan".to_string()), "{n:?}");
    // The whole sphere: elevation down to −90°.
    let el = s
        .automation_param(t, AutomationTarget::Surround(SurroundParam::Elevation))
        .unwrap();
    assert_eq!((el.min, el.max), (-90.0, 90.0));
    let pan = SurroundPan {
        azimuth: -120.0,
        elevation: -30.0,
        distance: 3.0,
        ..SurroundPan::default()
    };
    s.dispatch(Action::Edit(Command::SetTrackSurround { track: t, pan }))
        .unwrap();
    let text = file::to_string(s.project(), None).unwrap();
    let loaded = file::from_str(&text).unwrap().project;
    assert_eq!(loaded.track(t).unwrap().surround, pan);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project().track(t).unwrap().surround,
        SurroundPan::default()
    );
    // MIDI learn moves one value and keeps the rest.
    match s.command_for(t, AutomationTarget::Surround(SurroundParam::Distance), 2.0) {
        Some(Command::SetTrackSurround { pan, .. }) => assert_eq!(pan.distance, 2.0),
        other => panic!("{other:?}"),
    }
    // A stereo track balanced into the field automates its balance.
    let st = track(&mut s, ChannelLayout::Stereo);
    s.dispatch(Action::Edit(Command::SetTrackSurround {
        track: st,
        pan: SurroundPan {
            stereo: faderframe_core::surround::StereoSource::Balance,
            ..SurroundPan::default()
        },
    }))
    .unwrap();
    assert!(
        s.project()
            .balanced_into_bed(s.project().track(st).unwrap())
    );
    assert!(names(&s, st).contains(&"Balance".to_string()));
}

#[test]
fn a_field_bus_into_a_bed_is_decoded_not_panned() {
    let mut s = session(ChannelLayout::Surround(
        faderframe_core::SurroundFormat::S714,
    ));
    let bus = s.add_track(TrackKind::Bus).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: bus,
        layout: ChannelLayout::Ambisonic(AmbiOrder::First),
    }))
    .unwrap();
    let t = track(&mut s, ChannelLayout::Mono);
    s.dispatch(Action::Edit(Command::SetTrackOutput {
        track: t,
        output: OutputRouting::Track { track: bus },
    }))
    .unwrap();
    let b = s.project().track(bus).unwrap();
    // The field itself is decoded into the bed: no panner of its own.
    assert_eq!(s.project().spatially_panned(b), None);
    assert!(!names(&s, bus).iter().any(|n| n.starts_with("Surround")));
    // The track in it is.
    let tr = s.project().track(t).unwrap();
    assert_eq!(s.project().ambisonic_panned(tr), Some(AmbiOrder::First));
    // Its pan values reach a bed panner only through the field.
    assert_eq!(s.project().surround_panned(tr), None);
}

#[test]
fn the_head_turns_by_hand_and_straightens() {
    let mut s = session(ChannelLayout::Ambisonic(AmbiOrder::Third));
    assert!(s.head_orientation().is_identity());
    s.dispatch(Action::Head(HeadOp::Turn {
        yaw: 40.0,
        pitch: 10.0,
        roll: 0.0,
    }))
    .unwrap();
    let (y, p, _) = s.head_orientation().ypr();
    assert!((y - 40.0).abs() < 1e-3 && (p - 10.0).abs() < 1e-3);
    // The engine has it (the listening node reads it every block).
    assert!(
        s.engine()
            .head_orientation()
            .angle_to(&Orientation::from_ypr(40.0, 10.0, 0.0))
            < 1e-3
    );
    // Not an edit: nothing to undo.
    let (undo, _) = s.history_steps();
    assert!(!undo.iter().any(|l| l.contains("Head")), "{undo:?}");
    s.dispatch(Action::Head(HeadOp::Straight)).unwrap();
    assert!(s.head_orientation().is_identity());
    assert!(s.engine().head_orientation().is_identity());
}

#[test]
fn a_head_tracker_turns_the_head_and_is_recentred() {
    let mut s = session(ChannelLayout::Ambisonic(AmbiOrder::First));
    let port = free_port();
    s.dispatch(Action::Head(HeadOp::Tracking(HeadTrackingSettings {
        enabled: true,
        port,
        smoothing_ms: 40.0,
        ..HeadTrackingSettings::default()
    })))
    .unwrap();
    let st = s.head_status();
    assert_eq!(st.listening, Some(port));
    assert!(!st.shared && st.error.is_none());
    assert!((s.engine().head_smoothing() - 0.04).abs() < 1e-6);
    let tracker = UdpSocket::bind("127.0.0.1:0").unwrap();
    let send = |m: Message| {
        tracker.send_to(&encode(&m), ("127.0.0.1", port)).unwrap();
    };
    let floats = |v: &[f32]| v.iter().map(|v| Arg::Float(*v)).collect::<Vec<_>>();
    send(Message::new("/SceneRotator/ypr", floats(&[30.0, 0.0, 0.0])));
    std::thread::sleep(Duration::from_millis(50));
    s.tick(0.01);
    assert!((yaw(&s) - 30.0).abs() < 1e-3, "{}", yaw(&s));
    assert!(s.head_status().messages >= 1);
    // Recentred: where it points now is ahead; the next reading counts
    // from there.
    s.dispatch(Action::Head(HeadOp::Recentre)).unwrap();
    assert!(s.head_orientation().is_identity());
    send(Message::new("/ypr", floats(&[50.0, 0.0, 0.0])));
    std::thread::sleep(Duration::from_millis(50));
    s.tick(0.01);
    assert!((yaw(&s) - 20.0).abs() < 1e-3, "{}", yaw(&s));
    // A quaternion: 90° left about z.
    let h = std::f32::consts::FRAC_1_SQRT_2;
    send(Message::new("/quaternion", floats(&[h, 0.0, 0.0, h])));
    std::thread::sleep(Duration::from_millis(50));
    s.tick(0.01);
    assert!((yaw(&s) - 60.0).abs() < 1e-2, "{}", yaw(&s));
    // Yaw inverted: the tracker's left is our right.
    let mut inverted = s.head_tracking().clone();
    inverted.invert_yaw = true;
    s.dispatch(Action::Head(HeadOp::Tracking(inverted)))
        .unwrap();
    s.dispatch(Action::Head(HeadOp::Recentre)).unwrap();
    send(Message::new("/ypr", floats(&[10.0, 0.0, 0.0])));
    std::thread::sleep(Duration::from_millis(50));
    s.tick(0.01);
    // (Recentred at the quaternion's +90°, now −10°: 100° to the right.)
    assert!((yaw(&s) + 100.0).abs() < 1e-2, "{}", yaw(&s));
    // Off: the head as set by hand, the port let go.
    let mut off = s.head_tracking().clone();
    off.enabled = false;
    s.dispatch(Action::Head(HeadOp::Tracking(off))).unwrap();
    assert!(s.head_orientation().is_identity());
    assert_eq!(s.head_status().listening, None);
    assert!(UdpSocket::bind(("0.0.0.0", port)).is_ok());
}

#[test]
fn an_osc_surface_on_the_trackers_port_hands_its_messages_over() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let port = free_port();
    let tablet = UdpSocket::bind("127.0.0.1:0").unwrap();
    // Head tracking first, on its own socket …
    s.dispatch(Action::Head(HeadOp::Tracking(HeadTrackingSettings {
        enabled: true,
        port,
        ..HeadTrackingSettings::default()
    })))
    .unwrap();
    assert!(!s.head_status().shared);
    // … then a surface wants the port: the tracker's socket lets go of it.
    s.set_control_surfaces(vec![SurfaceSettings {
        kind: SurfaceKind::Osc,
        listen: port,
        reply: tablet.local_addr().unwrap().port(),
        strips: 4,
        ..SurfaceSettings::default()
    }]);
    assert_eq!(s.control_surface_errors(), [None]);
    let st = s.head_status();
    assert!(st.shared && st.listening == Some(port), "{st:?}");
    let drums = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Drums")
        .unwrap()
        .id;
    for m in [
        Message::new(
            "/SceneRotator/ypr",
            vec![Arg::Float(-45.0), Arg::Float(0.0), Arg::Float(0.0)],
        ),
        Message::new("/strip/1/mute", vec![Arg::Int(1)]),
    ] {
        tablet.send_to(&encode(&m), ("127.0.0.1", port)).unwrap();
    }
    std::thread::sleep(Duration::from_millis(50));
    s.tick(0.01);
    // Both: the head turned, the surface's mute pressed.
    assert!((yaw(&s) + 45.0).abs() < 1e-3, "{}", yaw(&s));
    assert!(s.project().track(drums).unwrap().mute);
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ff-ambisonics-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A file of constant channels (`values`), `frames` long.
fn file(dir: &std::path::Path, name: &str, values: &[f32], frames: usize) -> std::path::PathBuf {
    let path = dir.join(format!("{name}.wav"));
    let channels: Vec<Vec<f32>> = values.iter().map(|v| vec![*v; frames]).collect();
    faderframe_audio_files::write_wav(
        &path,
        &channels,
        48_000,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )
    .unwrap();
    path
}

/// AmbiX files become tracks of their order as they are; first-order
/// FuMa is converted (W up 3 dB, `W X Y Z` to ACN); a file of no
/// ambisonic order is refused.
#[test]
fn ambisonic_files_import_as_fields() {
    use faderframe_session::ambix::AmbiFile;
    let d = temp_dir("import");
    let mut s = session(ChannelLayout::Ambisonic(AmbiOrder::Third));
    let h = std::f32::consts::FRAC_1_SQRT_2;
    // A plane wave from the left in FuMa: W = 0.5/√2, X = 0, Y = 0.5, Z = 0.
    let fuma = file(&d, "fuma", &[0.5 * h, 0.0, 0.5, 0.0], 4800);
    let ambix: Vec<f32> = (0..9).map(|k| k as f32 * 0.1).collect();
    let second = file(&d, "second", &ambix, 4800);
    let six = file(&d, "six", &[0.1; 6], 4800);
    let before = s.project().tracks.len();
    let at = faderframe_timeline::MusicalTime::ZERO;
    s.dispatch(Action::ImportAmbisonic {
        files: vec![fuma],
        at,
        kind: AmbiFile::FuMa,
    })
    .unwrap();
    s.dispatch(Action::ImportAmbisonic {
        files: vec![second, six],
        at,
        kind: AmbiFile::AmbiX,
    })
    .unwrap();
    s.wait_for_imports();
    let added: Vec<_> = s.project().tracks[..]
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .collect();
    assert_eq!(
        s.project().tracks.len(),
        before + 2,
        "the six-channel file is refused"
    );
    let layouts: Vec<ChannelLayout> = added.iter().map(|t| t.layout).collect();
    assert_eq!(
        layouts,
        [
            ChannelLayout::Ambisonic(AmbiOrder::First),
            ChannelLayout::Ambisonic(AmbiOrder::Second)
        ]
    );
    // The FuMa file's media is AmbiX now: W 0.5, Y (ACN 1) 0.5.
    let clip = s.project().clips_of(added[0].id)[0];
    let source = clip.as_audio().unwrap().source;
    let path = match &s.project().sources[&source].spec {
        faderframe_project::SourceSpec::File { path, .. } => path.clone(),
        other => panic!("{other:?}"),
    };
    let wav = faderframe_audio_files::read_wav(&path).unwrap();
    let at = |c: usize| wav.channels[c][100];
    assert!(
        (at(0) - 0.5).abs() < 1e-5 && (at(1) - 0.5).abs() < 1e-5,
        "{:?}",
        (0..4).map(at).collect::<Vec<_>>()
    );
    assert!(at(2).abs() < 1e-6 && at(3).abs() < 1e-6);
}

/// An ambisonic master renders "as it is" as AmbiX (every channel, the
/// field undecoded) and stereo decoded.
#[test]
fn an_ambisonic_master_renders_as_ambix_or_decoded() {
    use faderframe_session::render::{RenderChannels, RenderRange, RenderSettings};
    let d = temp_dir("render");
    let mut s = session(ChannelLayout::Ambisonic(AmbiOrder::First));
    let mono = file(&d, "mono", &[0.5], 48_000);
    s.import_audio(
        vec![mono],
        faderframe_session::ImportTarget {
            track: None,
            at: faderframe_timeline::MusicalTime::ZERO,
        },
    );
    s.wait_for_imports();
    let t = s
        .project()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .unwrap()
        .id;
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: t,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackSurround {
        track: t,
        pan: SurroundPan {
            azimuth: 90.0,
            ..SurroundPan::default()
        },
    }))
    .unwrap();
    let base = RenderSettings::defaults_for(s.project(), d.join("x.wav"));
    let settings = |out: std::path::PathBuf, channels| RenderSettings {
        range: RenderRange::Span {
            start: faderframe_timeline::MusicalTime::ZERO,
            end: faderframe_timeline::MusicalTime::from_quarters(1.0),
        },
        tail_seconds: 0.0,
        format: faderframe_audio_files::WavFormat::Float32,
        channels,
        output: out,
        ..base.clone()
    };
    assert_eq!(base.channels, RenderChannels::Master);
    let ambix = d.join("ambix.wav");
    s.render(settings(ambix.clone(), RenderChannels::Master))
        .unwrap()
        .join()
        .unwrap();
    let wav = faderframe_audio_files::read_wav(&ambix).unwrap();
    assert_eq!(wav.channels.len(), 4);
    assert_eq!(wav.channel_mask, None);
    let mid = wav.channels[0].len() / 2;
    // From the left: W 0.5, Y 0.5, Z and X silent.
    let v: Vec<f32> = wav.channels.iter().map(|c| c[mid]).collect();
    assert!(
        (v[0] - 0.5).abs() < 1e-4 && (v[1] - 0.5).abs() < 1e-4,
        "{v:?}"
    );
    assert!(v[2].abs() < 1e-4 && v[3].abs() < 1e-4, "{v:?}");
    let stereo = d.join("stereo.wav");
    s.render(settings(stereo.clone(), RenderChannels::Stereo))
        .unwrap()
        .join()
        .unwrap();
    let wav = faderframe_audio_files::read_wav(&stereo).unwrap();
    assert_eq!(wav.channels.len(), 2);
    let (l, r) = (wav.channels[0][mid], wav.channels[1][mid]);
    assert!(l > 2.0 * r.abs(), "decoded: {l} {r}");
}
