//! The audio device through the session, on pretend interfaces
//! (`DummyDevices`): settings checked against what a device takes, changes
//! applied live where the backend can and by reopening otherwise (never
//! while recording), and recovery when a device is unplugged — the same
//! device as soon as it is back, the default one after a while, and back
//! to the chosen one when it returns.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::{DummyBackend, DummyDevices, mock_device};
use faderframe_audio::{AudioBackend, AudioError};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, InputRouting, Project, TrackKind};
use faderframe_session::{
    Applied, AudioHealth, AudioPreferences, BackendFactory, Session, TransportAction,
    audio_device::FALLBACK_AFTER,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Interface "desk" (the default: 4 in, 6 out, 44.1/48 kHz, 64–1024
/// frames) and "phones" (output only, 48 kHz).
fn devices() -> DummyDevices {
    let mut desk = mock_device("desk", 4, 6, &[48_000, 44_100], Some((64, 1024)));
    desk.is_default = true;
    let phones = mock_device("phones", 0, 2, &[48_000], None);
    DummyDevices::new(vec![desk, phones])
}

fn factory(devices: &DummyDevices) -> BackendFactory {
    let devices = devices.clone();
    Arc::new(move || {
        vec![Box::new(DummyBackend::with_devices(devices.clone())) as Box<dyn AudioBackend>]
    })
}

fn session() -> Session {
    Session::new(
        Project::new("Device", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap()
}

/// Tick until `done` or panic after 10 s.
fn wait_until(s: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        s.tick(0.01);
        if done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "timed out waiting for {what}: {:?}",
        s.audio_status().health
    );
}

fn device(s: &Session) -> String {
    s.stream_info().map(|i| i.device).unwrap_or_default()
}

fn noticed(s: &Session, part: &str) -> bool {
    s.notices().any(|n| n.text.contains(part))
}

#[test]
fn the_stream_opens_the_devices_channels_with_their_names() {
    let d = devices();
    let mut s = session();
    let info = s
        .start_audio_with(
            factory(&d),
            &(AudioPreferences {
                ..AudioPreferences::default()
            })
            .with_devices("dummy", &["desk"], &[]),
        )
        .unwrap();
    assert_eq!((info.input_channels, info.output_channels), (4, 6));
    assert_eq!(s.input_channel_name(1).as_deref(), Some("desk · In 2"));
    assert_eq!(s.output_channel_name(5).as_deref(), Some("desk · Out 6"));
    assert_eq!(s.audio_status().health, AudioHealth::Running);
    // Fewer channels when asked for, but never fewer than the project
    // needs: a (mono) track on output 3 keeps three.
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTrackOutput {
        track: t,
        output: faderframe_project::OutputRouting::Hardware { first_channel: 2 },
    })
    .unwrap();
    let applied = s
        .apply_audio(
            (AudioPreferences {
                input_channels: Some(2),
                output_channels: Some(2),
                ..AudioPreferences::default()
            })
            .with_devices("dummy", &["desk"], &[]),
        )
        .unwrap();
    assert_eq!(applied, Applied::Restarted);
    let info = s.stream_info().unwrap();
    assert_eq!((info.input_channels, info.output_channels), (2, 3));
}

#[test]
fn a_buffer_size_applies_live_and_a_rate_reopens() {
    let d = devices();
    let mut s = session();
    let prefs = (AudioPreferences {
        buffer_size: Some(256),
        ..AudioPreferences::default()
    })
    .with_devices("dummy", &["desk"], &[]);
    s.start_audio_with(factory(&d), &prefs).unwrap();
    s.dispatch(faderframe_session::Action::Transport(TransportAction::Play))
        .unwrap();
    wait_until(&mut s, "playing", |s| s.transport().position > 4_800);
    let callbacks = s.stream_status().unwrap().callbacks;
    // The same settings: nothing to do.
    assert_eq!(s.apply_audio(prefs.clone()).unwrap(), Applied::Unchanged);
    let smaller = AudioPreferences {
        buffer_size: Some(128),
        ..prefs.clone()
    };
    assert_eq!(s.apply_audio(smaller.clone()).unwrap(), Applied::Live);
    wait_until(&mut s, "the new buffer size", |s| {
        s.stream_info().is_some_and(|i| i.buffer_size == 128)
    });
    assert!(s.transport().playing, "playback went on");
    assert!(
        s.stream_status().unwrap().callbacks > callbacks,
        "the same stream"
    );
    // Beyond what the device takes: refused, the stream stays as it was.
    let huge = AudioPreferences {
        buffer_size: Some(2048),
        ..prefs.clone()
    };
    assert!(s.apply_audio(huge).is_err());
    assert_eq!(s.stream_info().unwrap().buffer_size, 128);
    // The dummy device opens at a rate: another one reopens it.
    let slower = AudioPreferences {
        sample_rate: Some(44_100),
        ..smaller
    };
    assert_eq!(s.apply_audio(slower.clone()).unwrap(), Applied::Restarted);
    assert_eq!(s.stream_info().unwrap().sample_rate, 44_100);
    assert_eq!(s.sample_rate(), 44_100, "the engine follows");
    s.tick(0.01);
    assert!(!s.transport().playing, "reopening stops playback");
    assert_eq!(s.audio_preferences(), &slower);
}

#[test]
fn a_rate_or_buffer_the_device_does_not_take_opens_at_what_it_does() {
    let d = devices();
    let mut s = session();
    let info = s
        .start_audio_with(
            factory(&d),
            &AudioPreferences {
                sample_rate: Some(96_000),
                buffer_size: Some(4096),
                ..AudioPreferences::default()
            },
        )
        .unwrap();
    assert_eq!(info.device, "desk", "the default device");
    assert_eq!(info.sample_rate, 48_000, "its own rate");
    assert_eq!(info.buffer_size, 1024, "the largest it takes");
    let status = s.audio_status();
    assert_eq!(status.notes.len(), 2, "{:?}", status.notes);
    assert!(noticed(&s, "cannot run at 96 kHz"));
    // As asked for is kept (Preferences shows both).
    assert_eq!(status.requested.sample_rate, Some(96_000));
}

#[test]
fn an_unplugged_device_is_reopened_when_it_returns() {
    let d = devices();
    let mut s = session();
    s.start_audio_with(
        factory(&d),
        &(AudioPreferences {
            ..AudioPreferences::default()
        })
        .with_devices("dummy", &["phones"], &["desk"]),
    )
    .unwrap();
    assert_eq!(device(&s), "phones");
    d.unplug("phones");
    wait_until(&mut s, "the loss", |s| {
        matches!(s.audio_status().health, AudioHealth::Reconnecting { .. })
    });
    assert!(s.stream_info().is_none());
    assert!(noticed(&s, "The audio device stopped"));
    // Back within the grace time: the same device again.
    std::thread::sleep(Duration::from_millis(300));
    d.plug(mock_device("phones", 0, 2, &[48_000], None));
    wait_until(&mut s, "the device again", |s| {
        s.audio_status().health == AudioHealth::Running
    });
    assert_eq!(device(&s), "phones");
    assert!(noticed(&s, "Audio is back on phones"));
}

#[test]
fn with_the_device_gone_audio_goes_on_on_the_default_and_moves_back() {
    let d = devices();
    let mut s = session();
    s.start_audio_with(
        factory(&d),
        &(AudioPreferences {
            ..AudioPreferences::default()
        })
        .with_devices("dummy", &["phones"], &[]),
    )
    .unwrap();
    let lost = Instant::now();
    d.unplug("phones");
    wait_until(&mut s, "the default device", |s| {
        matches!(s.audio_status().health, AudioHealth::Fallback { .. })
    });
    assert!(lost.elapsed() >= FALLBACK_AFTER, "waited for it first");
    assert_eq!(device(&s), "desk");
    assert!(noticed(&s, "phones is not connected: playing on desk"));
    // It returns while playing: the switch waits for the stop.
    s.dispatch(faderframe_session::Action::Transport(TransportAction::Play))
        .unwrap();
    d.plug(mock_device("phones", 0, 2, &[48_000], None));
    wait_until(&mut s, "the news", |s| {
        noticed(s, "phones is back: audio moves to it when playback stops")
    });
    assert_eq!(device(&s), "desk");
    assert!(s.transport().playing);
    s.dispatch(faderframe_session::Action::Transport(TransportAction::Stop))
        .unwrap();
    wait_until(&mut s, "the chosen device", |s| device(s) == "phones");
    assert_eq!(s.audio_status().health, AudioHealth::Running);
}

#[test]
fn a_device_missing_at_start_up_is_replaced_by_the_default() {
    let d = devices();
    d.unplug("phones");
    let mut s = session();
    s.start_audio_with(
        factory(&d),
        &(AudioPreferences {
            ..AudioPreferences::default()
        })
        .with_devices("dummy", &["phones"], &[]),
    )
    .unwrap();
    assert_eq!(device(&s), "desk");
    assert_eq!(
        s.audio_status().health,
        AudioHealth::Fallback {
            missing: vec!["phones".into()]
        }
    );
    assert!(noticed(&s, "phones is not connected"));
    d.plug(mock_device("phones", 0, 2, &[48_000], None));
    wait_until(&mut s, "the chosen device", |s| device(s) == "phones");
}

#[test]
fn the_device_is_not_reopened_while_recording() {
    let d = devices();
    let mut s = session();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTrackInput {
        track: t,
        input: InputRouting::Hardware { first_channel: 0 },
    })
    .unwrap();
    s.edit(Command::SetTrackRecordArm { track: t, on: true })
        .unwrap();
    let prefs = (AudioPreferences {
        buffer_size: Some(256),
        ..AudioPreferences::default()
    })
    .with_devices("dummy", &["desk"], &[]);
    s.start_audio_with(factory(&d), &prefs).unwrap();
    s.dispatch(faderframe_session::Action::Transport(
        TransportAction::ToggleRecord,
    ))
    .unwrap();
    s.dispatch(faderframe_session::Action::Transport(TransportAction::Play))
        .unwrap();
    wait_until(&mut s, "recording", |s| s.recording_view().is_some());
    let other = (AudioPreferences { ..prefs.clone() }).with_devices("dummy", &["phones"], &[]);
    let refused = s.apply_audio(other).unwrap_err();
    assert!(
        refused.to_string().contains("Stop recording first"),
        "{refused}"
    );
    assert!(s.recording_view().is_some(), "the take goes on");
    // A buffer size the stream takes live is fine meanwhile.
    let live = AudioPreferences {
        buffer_size: Some(512),
        ..prefs
    };
    assert_eq!(s.apply_audio(live).unwrap(), Applied::Live);
    s.dispatch(faderframe_session::Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
}

/// A backend that refuses one buffer size outright (a driver error, not a
/// format it could fall back from).
struct Picky(DummyBackend);

impl AudioBackend for Picky {
    fn id(&self) -> &'static str {
        self.0.id()
    }
    fn display_name(&self) -> &'static str {
        "Picky"
    }
    fn capabilities(&self) -> faderframe_audio::Capabilities {
        faderframe_audio::Capabilities {
            live_buffer_size: false,
            ..self.0.capabilities()
        }
    }
    fn is_available(&self) -> bool {
        true
    }
    fn enumerate_devices(&self) -> Result<Vec<faderframe_audio::DeviceInfo>, AudioError> {
        self.0.enumerate_devices()
    }
    fn open_stream(
        &mut self,
        config: faderframe_audio::StreamConfig,
        callback: Box<dyn faderframe_audio::AudioCallback>,
    ) -> Result<Box<dyn faderframe_audio::AudioStream>, AudioError> {
        if config.buffer_size == Some(333) {
            return Err(AudioError::Stream("the driver crashed".into()));
        }
        self.0.open_stream(config, callback)
    }
}

#[test]
fn settings_that_fail_to_open_bring_the_previous_ones_back() {
    let d = devices();
    let factory: BackendFactory = {
        let d = d.clone();
        Arc::new(move || {
            vec![Box::new(Picky(DummyBackend::with_devices(d.clone()))) as Box<dyn AudioBackend>]
        })
    };
    let mut s = session();
    let good = AudioPreferences {
        buffer_size: Some(256),
        ..AudioPreferences::default()
    }
    .with_devices("dummy", &["desk"], &[]);
    s.start_audio_with(factory, &good).unwrap();
    let bad = AudioPreferences {
        buffer_size: Some(333),
        ..good.clone()
    };
    let e = s.apply_audio(bad).unwrap_err();
    assert!(
        e.to_string().contains("previous audio settings are back"),
        "{e}"
    );
    let info = s.stream_info().expect("audio runs");
    assert_eq!(info.buffer_size, 256);
    assert_eq!(s.audio_preferences(), &good);
}

#[test]
fn stopping_audio_stops_looking_for_a_lost_device() {
    let d = devices();
    let mut s = session();
    s.start_audio_with(
        factory(&d),
        &(AudioPreferences {
            ..AudioPreferences::default()
        })
        .with_devices("dummy", &["phones"], &[]),
    )
    .unwrap();
    d.unplug("phones");
    wait_until(&mut s, "the loss", |s| {
        matches!(s.audio_status().health, AudioHealth::Reconnecting { .. })
    });
    s.stop_audio();
    d.plug(mock_device("phones", 0, 2, &[48_000], None));
    let until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < until {
        s.tick(0.01);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(s.stream_info().is_none(), "stays stopped");
    assert_eq!(s.audio_status().health, AudioHealth::Stopped);
}
