#![allow(clippy::unwrap_used)]
//! The Guitar Station in a live session: it plays, a pedal added while
//! playing restarts the line with its stage's latency and keeps playing, the
//! DI is an output a track can take, an offline render does not depend
//! on how fast its pedals were built, and a mono track is stereo after it.

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_files::{WavFormat, read_wav};
use faderframe_core::{ChannelLayout, ParameterId, PluginInstanceId, TrackId, builtin};
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

/// Two bars of the master in stereo, rendered offline.
fn master_stereo(project: &Project, name: &str) -> [Vec<f32>; 2] {
    let path = std::env::temp_dir().join(format!(
        "ff-guitar-{}-{name}-stereo.wav",
        std::process::id()
    ));
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
    let mut c = wav.channels.into_iter();
    [c.next().unwrap(), c.next().unwrap()]
}

fn rms_db(x: &[f32]) -> f64 {
    let ms = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64;
    10.0 * ms.max(1e-30).log10()
}

/// On a mono track the Guitar Station makes the signal stereo from its
/// slot on: its microphones pan, live and in renders, and a freeze keeps
/// both sides.
#[test]
fn a_mono_track_is_stereo_from_the_guitar_station_on() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let t = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Pluck")
        .unwrap()
        .id;
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: t,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    assert_eq!(
        s.project().track(t).unwrap().chain_layout(),
        ChannelLayout::Mono
    );
    // After the instrument, so it plays the guitar's part.
    let index = s.project().track(t).unwrap().inserts.len();
    s.dispatch(Action::InsertPlugin {
        track: t,
        index,
        plugin: PluginRef::builtin(builtin::GUITAR_STATION, "Guitar Station"),
    })
    .unwrap();
    let plugin = s.project().track(t).unwrap().inserts[index].id;
    assert_eq!(
        s.project().track(t).unwrap().chain_layout(),
        ChannelLayout::Stereo
    );
    // One microphone, hard left.
    set(&mut s, t, plugin, id::A_PAN, -1.0);
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    // Rendered ahead (the default), then on the audio thread.
    for ahead in [true, false] {
        if !ahead {
            s.set_render_ahead(None).unwrap();
        }
        // Until it sounds: a busy runner's debug build may take a while to
        // bring the restarted line up (it passed after 2 s here, failed at
        // CI); a line that never sounds still fails.
        run(&mut s, 2.0);
        let end = Instant::now() + Duration::from_secs(20);
        let mut m = s.meter(t);
        while m.left.level_db <= -50.0 && Instant::now() < end {
            run(&mut s, 0.05);
            m = s.meter(t);
        }
        assert!(
            m.left.level_db > -50.0,
            "left {:.1} dB (ahead {ahead})",
            m.left.level_db
        );
        assert!(
            m.right.level_db < m.left.level_db - 30.0,
            "panned left, live (ahead {ahead}): {:.1} / {:.1} dB",
            m.left.level_db,
            m.right.level_db
        );
    }
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    // Only the Pluck: the master's left and right are the microphone's.
    for other in s
        .project()
        .tracks
        .iter()
        .filter(|o| o.id != t && o.kind != faderframe_project::TrackKind::Master)
        .map(|o| o.id)
        .collect::<Vec<_>>()
    {
        s.dispatch(Action::Edit(Command::SetTrackMute {
            track: other,
            on: true,
        }))
        .unwrap();
    }
    let [l, r] = master_stereo(s.project(), "panned");
    let (l, r) = (rms_db(&l), rms_db(&r));
    assert!(
        l > -60.0 && r < l - 30.0,
        "panned left, rendered: {l:.1} / {r:.1} dB"
    );
    // Frozen: the rendered audio is stereo, and sounds the same.
    s.dispatch(Action::FreezeTrack(t)).unwrap();
    let end = Instant::now() + Duration::from_secs(120);
    while !s.bouncing().is_empty() {
        assert!(Instant::now() < end, "freeze timed out");
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
    let f = s
        .project()
        .track(t)
        .unwrap()
        .freeze
        .clone()
        .expect("frozen");
    assert_eq!(s.project().sources[&f.source].channels(), 2);
    let [fl, fr] = master_stereo(s.project(), "frozen");
    let (fl, fr) = (rms_db(&fl), rms_db(&fr));
    assert!(
        (fl - l).abs() < 0.5 && fr < fl - 30.0,
        "frozen: {fl:.1} / {fr:.1} dB, playing {l:.1} / {r:.1}"
    );
}

/// Every factory rig on the demo's Pluck, played: what render-ahead misses,
/// what the device callbacks miss, how loud. Run in release with --ignored
/// --nocapture.
#[test]
#[ignore]
fn presets_in_the_demo() {
    let (mut s, t, plugin) = rig();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    let presets = faderframe_plugin_host::presets::factory_presets(builtin::GUITAR_STATION);
    for (i, preset) in presets.iter().enumerate() {
        s.dispatch(Action::SelectPluginProgram { plugin, index: i })
            .unwrap();
        run(&mut s, 0.5);
        s.dispatch(Action::Transport(TransportAction::Play))
            .unwrap();
        let (ahead0, xruns0) = (s.engine().ahead_misses(), s.performance().xruns);
        let mut peak = f32::NEG_INFINITY;
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            s.tick(0.016);
            peak = peak
                .max(s.meter(t).left.hold_db)
                .max(s.meter(t).right.hold_db);
            std::thread::sleep(Duration::from_millis(5));
        }
        s.poll_performance();
        let p = s.performance();
        eprintln!(
            "{:<20} ahead misses {:>6}, xruns {:>4}, deadline misses {:>4}, load max {:>5.2}, track peak {:>6.1} dBFS",
            preset.name,
            s.engine().ahead_misses() - ahead0,
            p.xruns - xruns0,
            p.deadline_misses,
            p.max_load,
            peak
        );
        s.dispatch(Action::Transport(TransportAction::Stop))
            .unwrap();
        run(&mut s, 0.3);
    }
}
