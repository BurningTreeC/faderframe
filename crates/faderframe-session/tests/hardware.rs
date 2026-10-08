//! Hardware inserts: on a device whose outputs come back on its inputs (a
//! patch cable), a ping measures the round trip, and once it is known the
//! return lines up with the dry signal (a tone mixed half and half is at
//! full level; before, the late return partly cancels it).
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ChannelLayout, ClipId, ParameterId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::devices::hardware_insert::id;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, PluginRef, Project, SourceSpec,
    StretchSettings, TrackKind,
};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: i64 = 48_000;

fn run(s: &mut Session, ms: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        s.tick(0.01);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_round_trip_is_pinged_and_compensated() {
    let mut s = Session::new(
        Project::new("Outboard", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: t,
        layout: ChannelLayout::Mono,
    }))
    .unwrap();
    // A steady 1 kHz tone on the track.
    let source = AudioSourceId(7000);
    s.dispatch(Action::Edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: source,
            name: "tone".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::Sine {
                    frequency: 1000.0,
                    seconds: 20.0,
                    amplitude: 0.5,
                },
            },
        }),
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id: ClipId(7001),
            track: t,
            name: "tone".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: 0,
                length: 20 * SR,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
            }),
        }),
    }))
    .unwrap();
    s.dispatch(Action::InsertPlugin {
        track: t,
        index: 0,
        plugin: PluginRef::builtin(builtin::HARDWARE_INSERT, "Hardware Insert"),
    })
    .unwrap();
    let insert = s.project().track(t).unwrap().inserts[0].id;
    // Half dry, half through the "gear".
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: t,
        plugin: insert,
        parameter: ParameterId(id::MIX),
        value: Some(0.5),
    }))
    .unwrap();
    // Send on output 3, return on input 3: three outputs open.
    assert_eq!(s.wanted_channels().1, 3);
    s.start_audio(
        vec![Box::new(DummyBackend::with_loopback(100))],
        &AudioPreferences::default(),
    )
    .unwrap();
    let buffer = i64::from(s.stream_info().unwrap().buffer_size);
    // Not measured yet: nothing held back for the return.
    run(&mut s, 300);
    assert_eq!(s.engine().graph_stats().max_compensation, 0);
    // The ping: one buffer and the cable's 100 frames.
    s.dispatch(Action::PingHardwareInsert {
        track: t,
        plugin: insert,
    })
    .unwrap();
    let start = Instant::now();
    while s.pinging() && start.elapsed() < Duration::from_secs(3) {
        run(&mut s, 20);
    }
    let trip = s
        .plugin_parameter_value(insert, ParameterId(id::ROUND_TRIP))
        .unwrap();
    assert_eq!(trip as i64, buffer + 100, "round trip");
    // The graph built again with it: the dry signal into the device waits
    // for the return (the engine's tests check the samples line up).
    let start = Instant::now();
    while s.engine().graph_stats().max_compensation != trip as u32
        && start.elapsed() < Duration::from_secs(3)
    {
        run(&mut s, 20);
    }
    assert_eq!(s.engine().graph_stats().max_compensation, trip as u32);
    // And it plays: the tone through it is heard (its peak: the macOS
    // runners' dummy device runs in bursts, which the 300 ms RMS reads
    // low).
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters(4.0),
    )))
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, 1000);
    assert!(s.meter(t).left.level_db > -20.0, "{:?}", s.meter(t));
    s.stop_audio();
}
