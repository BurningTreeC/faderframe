#![allow(clippy::unwrap_used)]
//! A virtual camera through a real capture pipeline: the flashes it
//! films come out at the moments they happened, so the frames' stamps
//! (running time on the pipeline's clock plus its base time) are related
//! to `Instant` correctly.

use faderframe_video::calibration::{LightFrame, flashes};
use faderframe_video::camera::{CameraSource, Capture, GRID, Scene, VirtualCamera};
use std::time::{Duration, Instant};

/// Flashes of `length` every `every` from `first`.
struct Flashing {
    first: Instant,
    every: Duration,
    length: Duration,
}

impl Scene for Flashing {
    fn light(&mut self, at: Instant) -> f32 {
        let Some(since) = at.checked_duration_since(self.first) else {
            return 0.0;
        };
        let into = since.as_nanos() % self.every.as_nanos();
        if into < self.length.as_nanos() {
            1.0
        } else {
            0.0
        }
    }
}

fn film(readout: f32) -> (Vec<i64>, faderframe_video::calibration::Flashes) {
    let first = Instant::now() + Duration::from_millis(700);
    let every = Duration::from_millis(500);
    let scene = Flashing {
        first,
        every,
        length: Duration::from_millis(200),
    };
    let camera = VirtualCamera {
        fps: 30,
        readout,
        ..VirtualCamera::default()
    };
    let capture = Capture::start(CameraSource::Scene(Box::new(scene), camera)).unwrap();
    let origin = Instant::now();
    let mut frames = Vec::new();
    while origin.elapsed() < Duration::from_millis(3300) {
        std::thread::sleep(Duration::from_millis(20));
        frames.extend(capture.take());
        assert!(capture.error().is_none());
    }
    drop(capture);
    assert!(frames.len() > 60, "{} frames", frames.len());
    // On one time line: ns after `first`.
    let ns = |t: Instant| -> i64 {
        if t >= first {
            t.duration_since(first).as_nanos() as i64
        } else {
            -(first.duration_since(t).as_nanos() as i64)
        }
    };
    let light: Vec<LightFrame> = frames
        .iter()
        .map(|f| LightFrame {
            at: ns(f.at),
            exposure: f.exposure.as_nanos() as i64,
            cells: f.cells.clone(),
        })
        .collect();
    // Frames come a period apart, in order.
    for w in light.windows(2) {
        let d = w[1].at - w[0].at;
        assert!((d - 33_333_333).abs() < 1_000_000, "{d} ns apart");
    }
    let seen = flashes(&light, GRID);
    // Every flash the scene makes (a camera starting late under load
    // films later ones).
    let truth: Vec<i64> = (0..20).map(|k| k * every.as_nanos() as i64).collect();
    (truth, seen)
}

#[test]
fn a_filmed_flash_comes_out_when_it_happened() {
    let (truth, seen) = film(0.0);
    assert!(seen.flashes.len() >= 4, "{seen:?}");
    for f in &seen.flashes {
        let near = truth
            .iter()
            .map(|t| f.at - t)
            .min_by_key(|d| d.abs())
            .unwrap();
        assert!(
            near.abs() < 1_000_000,
            "a flash {:.3} ms off",
            near as f64 / 1e6
        );
    }
}

#[test]
fn a_rolling_shutter_shows_as_the_slope_down_the_picture() {
    // Rows read out over half a frame: the screen's middle row (y = 0.5)
    // is exposed a quarter of a frame after the first.
    let (truth, seen) = film(0.5);
    assert!(seen.flashes.len() >= 4);
    let readout = 0.5 * 33_333_333.0;
    assert!(
        (seen.slope + readout).abs() < 2e6,
        "slope {:.2} ms",
        seen.slope / 1e6
    );
    for f in &seen.flashes {
        let near = truth
            .iter()
            .map(|t| f.at - t)
            .min_by_key(|d| d.abs())
            .unwrap();
        // Read at the screen's middle less that row's readout.
        let expected = -(readout * 0.5) as i64;
        assert!(
            (near - expected).abs() < 1_500_000,
            "{:.3} ms",
            (near - expected) as f64 / 1e6
        );
    }
}

/// The cameras plugged into this machine, and a second of the first one's
/// frames (opt-in: `cargo test -p faderframe-video --test camera -- --ignored
/// --nocapture`).
#[test]
#[ignore = "needs a camera"]
fn the_cameras_on_this_machine() {
    let found = faderframe_video::camera::cameras().unwrap();
    for c in &found {
        eprintln!("{} [{}]", c.label(), c.id);
    }
    if found.is_empty() {
        return;
    }
    let capture = Capture::start(CameraSource::First).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let frames = capture.take();
    eprintln!(
        "{}: {} frames in a second ({} untimed), exposure {:?}",
        capture.name,
        frames.len(),
        capture.untimed(),
        frames.first().map(|f| f.exposure)
    );
    assert!(!frames.is_empty());
}
