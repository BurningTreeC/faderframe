//! Against the system's default output device (skipped without one): the
//! callback runs at the device's pace, silently, and the stream closes.
#![allow(clippy::unwrap_used)]

use faderframe_audio::{AudioBackend, AudioCallback, DeviceBuffers, StreamConfig, StreamInfo};
use faderframe_audio_cpal::CpalBackend;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

struct Count(Arc<AtomicU64>);

impl AudioCallback for Count {
    fn prepare(&mut self, _info: &StreamInfo) {}

    fn process(&mut self, io: &mut dyn DeviceBuffers) {
        for c in 0..io.output_channels() {
            io.output(c).fill(0.0);
        }
        self.0.fetch_add(io.frames() as u64, Ordering::Relaxed);
    }
}

#[test]
fn the_default_device_runs_and_closes() {
    let mut backend = CpalBackend;
    if std::env::var_os("CI").is_some() || !backend.is_available() {
        eprintln!("no audio device: skipped");
        return;
    }
    assert!(!backend.enumerate_devices().unwrap().is_empty());
    let frames = Arc::new(AtomicU64::new(0));
    let stream = match backend.open_stream(
        StreamConfig {
            input_channels: 0,
            ..StreamConfig::default()
        },
        Box::new(Count(Arc::clone(&frames))),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("device busy or unsupported ({e}): skipped");
            return;
        }
    };
    let info = stream.info();
    let start = Instant::now();
    while frames.load(Ordering::Relaxed) < info.sample_rate as u64 / 4
        && start.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        frames.load(Ordering::Relaxed) >= info.sample_rate as u64 / 4,
        "{info}"
    );
    assert!(stream.status().running);
    drop(stream);
}
