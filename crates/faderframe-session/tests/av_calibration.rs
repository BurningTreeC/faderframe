#![allow(clippy::unwrap_used)]
//! A/V calibration on the dummy device: the beeps go out through the
//! monitor path and come back through a patch cable (a known round trip),
//! a virtual camera sees the video window's flashes a known time late —
//! the picture offset comes out as their difference, one undo step.

use faderframe_audio::dummy::DummyBackend;
use faderframe_engine::EngineConfig;
use faderframe_project::Project;
use faderframe_session::av_calibration::{CalibrationSettings, CalibrationStatus, CameraChoice};
use faderframe_session::video::VideoOp;
use faderframe_session::{Action, AudioPreferences, Session};
use std::time::Duration;

fn calibrate(s: &mut Session, display_ms: f64, flashes: u32) -> CalibrationStatus {
    s.dispatch(Action::Video(VideoOp::Calibrate(CalibrationSettings {
        camera: CameraChoice::Virtual {
            display_ms,
            readout: 0.0,
        },
        input: 0,
        flashes,
    })))
    .unwrap();
    assert!(s.av_calibrating());
    assert!(s.wait_for_av_calibration(Duration::from_secs(30)));
    s.av_calibration().unwrap()
}

#[test]
fn a_virtual_camera_and_a_patch_cable_set_the_picture_offset() {
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    // Without a device: refused.
    assert!(
        s.dispatch(Action::Video(VideoOp::Calibrate(
            CalibrationSettings::default()
        )))
        .is_err()
    );
    let cable = 96;
    s.start_audio(
        vec![Box::new(DummyBackend::with_loopback(cable))],
        &AudioPreferences::default(),
    )
    .unwrap();
    let info = s.stream_info().unwrap();
    assert!(info.input_channels >= 1);
    // The dummy device reports no latency, and its cable brings one
    // callback's output back in the next callback's input: a buffer sooner
    // than a real cable could (the input of a callback came in before it
    // started, its output is heard a buffer after) — the beeps arrive the
    // cable less a buffer after they are heard. The screen shows a flash
    // 30 ms late.
    let back_ms =
        (f64::from(cable) - f64::from(info.buffer_size)) * 1000.0 / f64::from(info.sample_rate);
    let display = 30.0;
    let expected = display - back_ms;
    let r = match calibrate(&mut s, display, 5) {
        CalibrationStatus::Done(r) => r,
        other => panic!("{other:?}"),
    };
    eprintln!("{r:?} (expected {expected:.2} ms)");
    assert!(
        (r.late_ms - expected).abs() < 2.5,
        "the picture {:.2} ms late, expected {expected:.2}",
        r.late_ms
    );
    assert!(r.used >= 4, "{} pairs", r.used);
    assert!(r.spread_ms < 2.0, "spread {:.2} ms", r.spread_ms);
    assert_eq!(r.before_ms, 0.0);
    assert!((s.project().video.offset_ms - r.offset_ms).abs() < 1e-9);
    assert!((r.offset_ms + expected).abs() < 2.6);
    // The transport is back, the mix heard again.
    assert!(!s.transport().playing);
    assert!(s.compare_link().wanted().is_none());
    // One undo step.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().video.offset_ms, 0.0);
    s.dispatch(Action::Redo).unwrap();
    // Again with the offset set: what is left is within the spread.
    let again = match calibrate(&mut s, display, 4) {
        CalibrationStatus::Done(r) => r,
        other => panic!("{other:?}"),
    };
    assert!(
        again.late_ms.abs() < 2.5,
        "left over: {:.2} ms",
        again.late_ms
    );
    s.stop_audio();
}

#[test]
fn a_microphone_hearing_nothing_leaves_the_offset_alone() {
    let mut s = Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
    // No cable: the input is silent.
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    match calibrate(&mut s, 0.0, 3) {
        CalibrationStatus::Failed(e) => assert!(e.contains("heard no beeps"), "{e}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(s.project().video.offset_ms, 0.0);
    // A calibration can be stopped.
    s.dispatch(Action::Video(VideoOp::Calibrate(CalibrationSettings {
        camera: CameraChoice::Virtual {
            display_ms: 0.0,
            readout: 0.0,
        },
        input: 0,
        flashes: 10,
    })))
    .unwrap();
    s.dispatch(Action::Video(VideoOp::CancelCalibration))
        .unwrap();
    assert!(!s.av_calibrating());
    assert!(matches!(
        s.av_calibration(),
        Some(CalibrationStatus::Failed(_))
    ));
    s.stop_audio();
}
