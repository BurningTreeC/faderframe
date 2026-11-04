#![allow(clippy::unwrap_used)]
//! Several audio interfaces at once through the session, on pretend
//! devices: the first output device's clock drives the engine, another
//! device records through its ring and drift-steered resampler. Minutes
//! of simulated time recorded without a slip, takes of the master's and
//! a follower's inputs lined up by per-channel latency, and a follower
//! unplugged and plugged back while the master plays on.

use faderframe_audio::AudioBackend;
use faderframe_audio::dummy::{DummyBackend, DummyDevices, PretendClock, mock_device};
use faderframe_audio_files::wavstream::WavFile;
use faderframe_core::ChannelLayout;
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, InputRouting, Project, SourceSpec, TrackKind};
use faderframe_session::{
    Action, AudioHealth, AudioPreferences, BackendFactory, Session, TransportAction,
};
use std::f64::consts::TAU;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

/// "desk" (the master: `ins` inputs, 2 outputs, 48 kHz) and "mic" (one
/// input at `mic_rate`, its clock `ppm` off, 10 ms-ish callbacks).
fn devices(ins: u16, mic_rate: u32, ppm: f64) -> DummyDevices {
    let mut desk = mock_device("desk", ins, 2, &[48_000], None);
    desk.is_default = true;
    let mic = mock_device("mic", 1, 0, &[mic_rate], None);
    let d = DummyDevices::new(vec![desk, mic]);
    d.set_clock(
        "mic",
        PretendClock {
            ppm,
            jitter: 0.0004,
            block: Some(mic_rate / 100),
        },
    );
    d
}

fn factory(backend: DummyBackend) -> BackendFactory {
    Arc::new(move || vec![Box::new(backend.clone()) as Box<dyn AudioBackend>])
}

fn session(backend: DummyBackend, inputs: &[&str]) -> Session {
    let mut s = Session::new(Project::new("Devices", SR), None, EngineConfig::default()).unwrap();
    s.start_audio_with(
        factory(backend),
        &AudioPreferences {
            sample_rate: Some(SR),
            buffer_size: Some(256),
            ..AudioPreferences::default()
        }
        .with_devices("dummy", &["desk"], inputs),
    )
    .unwrap();
    // A take may wait long in its ring while the simulated clock races.
    let mut r = s.record;
    r.buffer_seconds = 60.0;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
    s
}

fn armed_track(s: &mut Session, input: u16) -> TrackId {
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTrackLayout {
        track: t,
        layout: ChannelLayout::Mono,
    })
    .unwrap();
    s.edit(Command::SetTrackInput {
        track: t,
        input: InputRouting::Hardware {
            first_channel: input,
        },
    })
    .unwrap();
    s.edit(Command::SetTrackRecordArm { track: t, on: true })
        .unwrap();
    t
}

/// Tick until `done` (the session's own clock may be simulated: `limit`
/// is real time).
fn wait_until(
    s: &mut Session,
    what: &str,
    limit: Duration,
    mut done: impl FnMut(&Session) -> bool,
) {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        s.tick(0.01);
        if done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

fn mic_locked(s: &Session) -> bool {
    s.stream_devices()
        .iter()
        .any(|d| d.id == "mic" && d.present && d.locked)
}

/// Record from the start until the engine passes `seconds`.
fn record(s: &mut Session, seconds: f64) {
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let until = (seconds * f64::from(SR)) as i64;
    wait_until(s, "the recording", Duration::from_secs(240), |s| {
        s.processing_position() >= until
    });
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
}

/// A take on `t` read whole, and where its first frame is on the timeline
/// (engine samples).
fn take(s: &Session, t: TrackId) -> (Vec<f32>, i64) {
    let clips = s.project().clips_of(t);
    assert_eq!(clips.len(), 1, "one take");
    let clip = clips[0];
    let a = clip.as_audio().unwrap();
    let SourceSpec::File { path, .. } = &s.project().sources[&a.source].spec else {
        panic!("a file");
    };
    let f = WavFile::open(path).unwrap();
    let n = f.frames() as usize;
    let mut v = vec![0.0f32; n];
    f.read(0, &mut [&mut v], &mut Vec::new()).unwrap();
    let start = s.engine().musical_to_samples(s.project(), clip.start);
    (v, start - a.source_offset)
}

#[test]
fn a_drifting_follower_records_a_tone_for_minutes_without_a_slip() {
    // The interface recording runs 180 ppm fast with 10 ms callbacks
    // jittered by ±0.4 ms; three simulated minutes, at up to 60 × speed.
    let tone = 997.0;
    let backend = DummyBackend::with_input_tone(tone as f32)
        .devices(devices(0, 48_000, 180.0))
        .simulated(60.0);
    let mut s = session(backend, &["mic"]);
    let t = armed_track(&mut s, 0);
    assert_eq!(s.stream_info().unwrap().device, "desk", "the clock master");
    assert_eq!(s.stream_routing().devices, vec!["desk", "mic"]);
    wait_until(
        &mut s,
        "the follower to lock",
        Duration::from_secs(60),
        mic_locked,
    );
    record(&mut s, 180.0);
    let (v, _) = take(&s, t);
    assert!(v.len() as f64 > 179.0 * f64::from(SR), "{} frames", v.len());
    // Its tone, in the master's frames: 180 ppm higher.
    let w = TAU * tone * (1.0 + 180e-6) / f64::from(SR);
    let c = 2.0 * w.cos();
    let mut worst = (0.0f64, 0usize);
    for (i, x) in v.windows(3).enumerate().skip(SR as usize) {
        let r = (f64::from(x[2]) + f64::from(x[0]) - c * f64::from(x[1])).abs();
        if r > worst.0 {
            worst = (r, i);
        }
    }
    assert!(
        worst.0 < 2e-3,
        "a slip or click at frame {} ({:.1} s): residual {:.5}",
        worst.1,
        worst.1 as f64 / f64::from(SR),
        worst.0
    );
    let peak = v[SR as usize..].iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert!((peak - 0.5).abs() < 0.01, "the tone at its level: {peak}");
    let mic = s
        .stream_devices()
        .into_iter()
        .find(|d| d.id == "mic")
        .unwrap();
    assert_eq!(mic.gaps, 0, "no gap: {mic:?}");
    assert!(
        (mic.drift_ppm - 180.0).abs() < 2.0,
        "the drift found: {:.2} ppm",
        mic.drift_ppm
    );
}

/// The pulse centres in a take (timeline samples), by a centroid over
/// each pulse.
fn pulses(v: &[f32], start: i64) -> Vec<f64> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if v[i] > 0.05 {
            let end = (i..v.len()).find(|&j| v[j] <= 0.0005).unwrap_or(v.len());
            let (mut m0, mut m1) = (0.0f64, 0.0f64);
            for (j, x) in v.iter().enumerate().take(end).skip(i.saturating_sub(48)) {
                m0 += f64::from(*x);
                m1 += f64::from(*x) * j as f64;
            }
            out.push(start as f64 + m1 / m0);
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn a_followers_take_lines_up_with_the_masters() {
    // Pulses captured at the same moments by the master's own input and
    // by a 44.1 kHz interface 90 ppm slow: their takes put them in the
    // same places, the follower's moved back by its own latency.
    let backend = DummyBackend::with_input_pulses(0.5)
        .devices(devices(1, 44_100, -90.0))
        .simulated(60.0);
    let mut s = session(backend, &["desk", "mic"]);
    let own = armed_track(&mut s, 0);
    let far = armed_track(&mut s, 1);
    wait_until(
        &mut s,
        "the follower to lock",
        Duration::from_secs(60),
        mic_locked,
    );
    // And settle (the loop narrows over the first half minute): 30
    // simulated seconds of 256-frame callbacks.
    let settled = s.stream_status().unwrap().callbacks + 30 * u64::from(SR) / 256;
    wait_until(&mut s, "half a minute", Duration::from_secs(60), |s| {
        s.stream_status().unwrap().callbacks >= settled
    });
    let master = s.input_latency_of(0);
    let follower = s.input_latency_of(1);
    assert!(
        follower > master + 441,
        "the follower's inputs come later: {master} vs {follower}"
    );
    record(&mut s, 20.0);
    let (a, a0) = take(&s, own);
    let (b, b0) = take(&s, far);
    let pa = pulses(&a, a0);
    let pb = pulses(&b, b0);
    assert!(
        pa.len() > 30 && pb.len() > 30,
        "{} and {} pulses",
        pa.len(),
        pb.len()
    );
    let mut compared = 0;
    for x in &pa {
        if let Some(y) = pb.iter().find(|y| (*y - x).abs() < 2_400.0) {
            assert!(
                (y - x).abs() < 1.5,
                "the master's pulse at {x:.1}, the follower's at {y:.1}"
            );
            compared += 1;
        }
    }
    assert!(compared > 30, "{compared} pulses compared");
}

#[test]
fn a_follower_unplugged_while_playing_leaves_the_master_running() {
    let d = devices(2, 48_000, 60.0);
    let backend = DummyBackend::with_input_tone(440.0).devices(d.clone());
    let mut s = session(backend, &["desk", "mic"]);
    wait_until(&mut s, "the follower", Duration::from_secs(20), |s| {
        s.stream_devices()
            .iter()
            .any(|d| d.id == "mic" && d.present)
    });
    assert_eq!(s.stream_info().unwrap().input_channels, 3);
    assert!(s.input_latency_of(2) > s.input_latency_of(0));
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    d.unplug("mic");
    wait_until(&mut s, "the follower gone", Duration::from_secs(10), |s| {
        s.stream_devices()
            .iter()
            .any(|d| d.id == "mic" && !d.present)
    });
    wait_until(&mut s, "the notice", Duration::from_secs(5), |s| {
        s.notices().any(|n| n.text.contains("mic went away"))
    });
    assert_eq!(s.audio_status().health, AudioHealth::Running);
    assert!(s.transport().playing, "the master plays on");
    assert_eq!(s.stream_routing().missing, vec!["mic".to_string()]);
    let before = s.stream_status().unwrap().callbacks;
    d.plug(mock_device("mic", 1, 0, &[48_000], None));
    wait_until(&mut s, "the follower back", Duration::from_secs(10), |s| {
        s.notices().any(|n| n.text.contains("mic is back"))
    });
    let status = s.stream_status().unwrap();
    assert!(
        status.callbacks > before && !status.shut_down,
        "never reopened"
    );
    assert!(s.transport().playing);
    assert!(s.audio_status().joined.iter().all(|d| d.present));
}
