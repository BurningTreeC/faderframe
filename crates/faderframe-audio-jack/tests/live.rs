//! Against a running JACK server (JACK2 or pipewire-jack; skipped without
//! one): the physical ports are listed as devices with their channels'
//! names, and an unconnected client runs and closes.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use faderframe_audio::{AudioBackend, AudioCallback, DeviceBuffers, StreamConfig, StreamInfo};
use faderframe_audio_jack::JackBackend;
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
fn devices_are_listed_and_an_unconnected_client_runs() {
    let mut backend = JackBackend;
    if std::env::var_os("CI").is_some() || !backend.is_available() {
        eprintln!("no JACK server: skipped");
        return;
    }
    let devices = backend.enumerate_devices().unwrap();
    for d in &devices {
        assert!(d.input_channels > 0 || d.output_channels > 0, "{d:?}");
        assert_eq!(d.input_names.len(), usize::from(d.input_channels));
        assert_eq!(d.output_names.len(), usize::from(d.output_channels));
        assert!(d.current_sample_rate.is_some());
    }
    eprintln!(
        "{} devices: {}",
        devices.len(),
        devices
            .iter()
            .map(|d| format!(
                "{} ({} in / {} out)",
                d.name, d.input_channels, d.output_channels
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let frames = Arc::new(AtomicU64::new(0));
    let stream = backend
        .open_stream(
            StreamConfig {
                client_name: format!("FaderFrame test {}", std::process::id()),
                input_channels: 2,
                output_channels: 2,
                auto_connect: false,
                ..StreamConfig::default()
            },
            Box::new(Count(Arc::clone(&frames))),
        )
        .unwrap();
    let start = Instant::now();
    while frames.load(Ordering::Relaxed) < 4_800 && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(frames.load(Ordering::Relaxed) >= 4_800);
    let routing = stream.routing();
    assert_eq!(routing.outputs, vec![None, None], "not connected");
    assert_eq!(routing.inputs, vec![None, None]);
    drop(stream);
}
