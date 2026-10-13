#![allow(clippy::unwrap_used)]
//! Plugin delay compensation in the session: what the player hears comes
//! the graph's output latency after it is processed, so takes recorded
//! along with playback move back by it — a take of the playback itself
//! (through a patch cable from the outputs to the inputs) lands on the
//! same sample with 6400 frames of plugin latency as without; and the
//! playhead shown is the position heard.

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::{AudioSourceId, ClipId, ParameterId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, InputRouting, PluginRef,
    PluginSlot, Project, SavedParameter, SourceSpec, StretchSettings, TrackKind,
};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;
/// Where the impulse is on the timeline.
const AT: usize = 12_000;

fn wait_until(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        s.tick(0.005);
        if done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out waiting for {what}");
}

/// A session playing an impulse (behind `latency` frames of plugin) into
/// the device, whose outputs come back on its inputs, recorded on "Rec".
fn session(latency: u32, dir: &std::path::Path) -> (Session, TrackId) {
    let mut s = Session::new(Project::new("PDC", SR), None, EngineConfig::default()).unwrap();
    let mut v = vec![0.0f32; SR as usize];
    v[AT] = 0.8;
    let path = dir.join(format!("impulse-{latency}.wav"));
    write_wav(&path, &[v], SR, WavFormat::Float32, false).unwrap();
    let click = s.add_track(TrackKind::Audio).unwrap();
    let source = AudioSourceId(9000);
    s.edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: source,
            name: "impulse".into(),
            spec: SourceSpec::File {
                path,
                channels: 1,
                frames: i64::from(SR),
                sample_rate: SR,
            },
        }),
    })
    .unwrap();
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id: ClipId(9001),
            track: click,
            name: "impulse".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: 0,
                length: i64::from(SR),
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
    })
    .unwrap();
    if latency > 0 {
        s.edit(Command::InsertPlugin {
            track: click,
            index: 0,
            slot: PluginSlot {
                id: faderframe_core::PluginInstanceId(9002),
                plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
                bypass: false,
                parameters: vec![SavedParameter {
                    id: ParameterId(0),
                    value: f64::from(latency),
                }],
                state: None,
                sidechain: None,
            },
        })
        .unwrap();
    }
    let rec = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTrackInput {
        track: rec,
        input: InputRouting::Hardware { first_channel: 0 },
    })
    .unwrap();
    s.edit(Command::SetTrackRecordArm {
        track: rec,
        on: true,
    })
    .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::with_loopback(100))],
        &AudioPreferences {
            sample_rate: Some(SR),
            buffer_size: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
    (s, rec)
}

/// Record the playback to 0.6 s; where the impulse is in the take.
fn take_of_the_impulse(latency: u32, dir: &std::path::Path) -> i64 {
    let (mut s, rec) = session(latency, dir);
    wait_until(
        &mut s,
        |s| s.engine().graph_latency() == latency,
        "the graph",
    );
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    wait_until(&mut s, |s| s.transport().playing, "playing");
    wait_until(
        &mut s,
        |s| s.transport().position > (0.6 * f64::from(SR)) as i64,
        "0.6 s heard",
    );
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    let clip = s.project().clips_of(rec)[0].clone();
    let a = clip.as_audio().unwrap();
    let SourceSpec::File { path, .. } = &s.project().sources[&a.source].spec else {
        panic!("a file");
    };
    let data = read_wav(path).unwrap();
    let i = data.channels[0]
        .iter()
        .position(|v| v.abs() > 0.4)
        .expect("the impulse came back");
    let start = s.engine().musical_to_samples(s.project(), clip.start);
    start + i as i64 - a.source_offset
}

#[test]
fn a_take_of_the_playback_lands_on_the_same_sample_behind_plugin_latency() {
    let dir = std::env::temp_dir().join(format!("ff-session-pdc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let plain = take_of_the_impulse(0, &dir);
    // The dummy reports no device latency: the take is the patch cable's
    // round trip (a buffer and 100 frames) late — and no later with 6400
    // frames of plugin latency in the graph.
    assert!(
        (plain - AT as i64 - 164).abs() <= 64,
        "the round trip: {}",
        plain - AT as i64
    );
    let late = take_of_the_impulse(6400, &dir);
    assert_eq!(late, plain, "plugin latency taken off the take");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The playhead shown is what is heard: playing from zero behind 6400
/// frames of latency it waits at zero while that much audio is on its way,
/// then trails the processed position by the latency.
#[test]
fn the_playhead_shows_what_is_heard() {
    let dir = std::env::temp_dir().join(format!("ff-session-pdc-heard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut s, _) = session(6400, &dir);
    wait_until(&mut s, |s| s.engine().graph_latency() == 6400, "the graph");
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    wait_until(&mut s, |s| s.processing_position() > 3000, "processing");
    assert_eq!(s.transport().position, 0, "nothing heard yet");
    wait_until(&mut s, |s| s.processing_position() > 30_000, "30 000");
    let (heard, processed) = (s.transport().position, s.processing_position());
    let behind = processed - heard;
    // 6400 frames of plugin latency and the device's 64 (+ callback
    // timing).
    assert!(
        (6400..6400 + 64 + 1500).contains(&behind),
        "behind by {behind}"
    );
    // A locate shows its place at once and holds it until heard.
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters(16.0),
    )))
    .unwrap();
    let to = s
        .engine()
        .musical_to_samples(s.project(), MusicalTime::from_quarters(16.0));
    assert_eq!(s.transport().position, to);
    s.tick(0.005);
    assert!(s.transport().position >= to);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dry track playing beside one with half a second of plugin latency is
/// heard half a second after it is processed: its meter waits for that.
#[test]
fn a_meter_shows_its_track_when_it_is_heard() {
    let dir = std::env::temp_dir().join(format!("ff-session-pdc-meter-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut s, _) = session(24_000, &dir);
    // A steady tone on another track, without latency of its own.
    let dry = s.add_track(TrackKind::Audio).unwrap();
    let tone = AudioSourceId(9100);
    s.edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: tone,
            name: "tone".into(),
            spec: SourceSpec::Generated {
                generator: faderframe_audio_files::GeneratorSpec::Sine {
                    frequency: 440.0,
                    seconds: 5.0,
                    amplitude: 0.5,
                },
            },
        }),
    })
    .unwrap();
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id: ClipId(9101),
            track: dry,
            name: "tone".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: tone,
                source_offset: 0,
                length: 5 * i64::from(SR),
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
    })
    .unwrap();
    wait_until(
        &mut s,
        |s| s.engine().graph_latency() == 24_000,
        "the graph",
    );
    assert_eq!(s.engine().meter_delay(dry), 24_000);
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let started = Instant::now();
    let lit = |s: &Session| s.meter(dry).left.level_db > -20.0;
    // Processed at once, heard half a second later.
    wait_until(
        &mut s,
        |s| s.processing_position() > 9600,
        "0.2 s processed",
    );
    assert!(!lit(&s), "lit before it is heard");
    wait_until(&mut s, lit, "the meter");
    let after = started.elapsed();
    assert!(after >= Duration::from_millis(450), "lit after {after:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Low-latency monitoring applies to the tracks heard live: an armed track
/// monitoring its input (auto) loses its effect with latency while it is
/// on; disarmed, or with the setting off, nothing is bypassed.
#[test]
fn low_latency_monitoring_follows_the_tracks_heard_live() {
    let dir = std::env::temp_dir().join(format!("ff-session-pdc-llm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut s, rec) = session(0, &dir);
    s.edit(Command::SetTrackMonitor {
        track: rec,
        mode: faderframe_project::MonitorMode::Auto,
    })
    .unwrap();
    let plugin = faderframe_core::PluginInstanceId(9200);
    s.edit(Command::InsertPlugin {
        track: rec,
        index: 0,
        slot: PluginSlot {
            id: plugin,
            plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
            bypass: false,
            parameters: vec![SavedParameter {
                id: ParameterId(0),
                value: 3000.0,
            }],
            state: None,
            sidechain: None,
        },
    })
    .unwrap();
    assert_eq!(s.engine().graph_stats().output_latency, 3000);
    s.dispatch(Action::SetLowLatencyMonitoring(true)).unwrap();
    assert!(s.low_latency_monitoring());
    assert_eq!(s.low_latency_tracks(), (vec![rec], vec![plugin]));
    // Its effect bypassed: nothing has latency now.
    assert_eq!(s.engine().graph_stats().output_latency, 0);
    // Disarmed: not heard live any more.
    s.edit(Command::SetTrackRecordArm {
        track: rec,
        on: false,
    })
    .unwrap();
    assert_eq!(s.low_latency_tracks(), (vec![], vec![]));
    assert_eq!(s.engine().graph_stats().output_latency, 3000);
    s.edit(Command::SetTrackRecordArm {
        track: rec,
        on: true,
    })
    .unwrap();
    s.dispatch(Action::SetLowLatencyMonitoring(false)).unwrap();
    assert_eq!(s.low_latency_tracks(), (vec![], vec![]));
    assert_eq!(s.engine().graph_stats().output_latency, 3000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Tools view analyses what is heard: a dry track beside one with half
/// a second of plugin latency shows in the analyser half a second after it
/// is processed.
#[test]
fn the_analyser_reads_its_track_when_it_is_heard() {
    let dir = std::env::temp_dir().join(format!("ff-session-pdc-tools-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut s, _) = session(24_000, &dir);
    let dry = s.add_track(TrackKind::Audio).unwrap();
    let tone = AudioSourceId(9300);
    s.edit(Command::AddSource {
        source: Box::new(AudioSource {
            id: tone,
            name: "tone".into(),
            spec: SourceSpec::Generated {
                generator: faderframe_audio_files::GeneratorSpec::Sine {
                    frequency: 440.0,
                    seconds: 5.0,
                    amplitude: 0.5,
                },
            },
        }),
    })
    .unwrap();
    s.edit(Command::AddClip {
        clip: Box::new(Clip {
            id: ClipId(9301),
            track: dry,
            name: "tone".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: tone,
                source_offset: 0,
                length: 5 * i64::from(SR),
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
    })
    .unwrap();
    s.dispatch(Action::SetAnalysisSource(Some(dry))).unwrap();
    wait_until(
        &mut s,
        |s| s.engine().graph_latency() == 24_000,
        "the graph",
    );
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let started = Instant::now();
    let heard = |s: &Session| s.analysis_levels()[0].peak > -30.0;
    wait_until(
        &mut s,
        |s| s.processing_position() > 9600,
        "0.2 s processed",
    );
    assert!(!heard(&s), "analysed before it is heard");
    wait_until(&mut s, heard, "the analyser");
    assert!(started.elapsed() >= Duration::from_millis(450));
    let _ = std::fs::remove_dir_all(&dir);
}
