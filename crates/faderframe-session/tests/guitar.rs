#![allow(clippy::unwrap_used)]
//! The Guitar Station in a live session: it plays, a pedal added while
//! playing restarts the line with its stage's latency and keeps playing, the
//! DI is an output a track can take, and an offline render does not depend
//! on how fast its pedals were built.

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_files::{WavFormat, read_wav};
use faderframe_core::{ParameterId, PluginInstanceId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_guitar::pedal::Stomp;
use faderframe_guitar::voice::Pedal;
use faderframe_plugin_host::devices::guitar::id;
use faderframe_project::{Command, PluginRef, Project};
use faderframe_session::render::{self, RenderChannels, RenderRange, RenderSettings};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use std::time::{Duration, Instant};

fn run(s: &mut Session, seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The demo's "Pluck" with the Guitar Station on it.
fn rig() -> (Session, TrackId, PluginInstanceId) {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let t = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Pluck")
        .unwrap()
        .id;
    s.dispatch(Action::InsertPlugin {
        track: t,
        index: 0,
        plugin: PluginRef::builtin(builtin::GUITAR_STATION, "Guitar Station"),
    })
    .unwrap();
    let plugin = s.project().track(t).unwrap().inserts[0].id;
    (s, t, plugin)
}

fn set(s: &mut Session, t: TrackId, plugin: PluginInstanceId, id: u32, v: f64) {
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: t,
        plugin,
        parameter: ParameterId(id),
        value: Some(v),
    }))
    .unwrap();
}

#[test]
fn it_plays_live_and_a_pedal_added_while_playing_restarts_the_line() {
    let (mut s, t, plugin) = rig();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, 1.5);
    let level = s.meter(t).left.level_db;
    assert!(level > -50.0, "the amplifier sounds: {level:.1} dB");
    let latency = s.engine().plugin_latency(plugin).unwrap();
    assert_eq!(s.plugin_output_buses(plugin).len(), 2, "main and DI");
    // A pedal: one more stage, one more buffer of latency.
    set(
        &mut s,
        t,
        plugin,
        id::slot(0, id::STOMP),
        Stomp::Pedal(Pedal::Green808).index() as f64,
    );
    run(&mut s, 1.5);
    let more = s.engine().plugin_latency(plugin).unwrap();
    assert!(more > latency, "{more} after {latency}");
    let level = s.meter(t).left.level_db;
    assert!(level > -50.0, "still sounding: {level:.1} dB");
    // Its footswitch does not restart anything.
    set(&mut s, t, plugin, id::slot(0, id::ON), 0.0);
    run(&mut s, 0.3);
    assert_eq!(s.engine().plugin_latency(plugin).unwrap(), more);
    // The DI on a track of its own.
    s.dispatch(Action::CreateOutputTracks {
        plugin,
        buses: None,
    })
    .unwrap();
    let di = s.project().plugin_output_tracks(plugin)[0].id;
    run(&mut s, 1.0);
    let level = s.meter(di).left.level_db;
    assert!(
        level > -60.0,
        "the DI track takes the guitar: {level:.1} dB"
    );
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
}

/// Two bars of the master, rendered offline.
fn master(project: &Project, name: &str) -> Vec<f32> {
    let path = std::env::temp_dir().join(format!("ff-guitar-{}-{name}.wav", std::process::id()));
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
fn renders_are_the_same_every_time() {
    let (mut s, t, plugin) = rig();
    set(
        &mut s,
        t,
        plugin,
        id::slot(0, id::STOMP),
        Stomp::Pedal(Pedal::Rodent).index() as f64,
    );
    set(
        &mut s,
        t,
        plugin,
        id::slot(1, id::STOMP),
        Stomp::Pedal(Pedal::BlueChorus).index() as f64,
    );
    set(&mut s, t, plugin, id::AMP, 12.0);
    let a = master(s.project(), "a");
    let b = master(s.project(), "b");
    assert_eq!(a.len(), b.len());
    assert!(a == b, "two renders differ");
    assert!(a.iter().any(|x| x.abs() > 1e-4));
}
