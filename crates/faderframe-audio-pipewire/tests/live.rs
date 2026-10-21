//! Against the running PipeWire server (skipped without one): an unlinked
//! node is still scheduled, reports the graph's format and closes cleanly.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use faderframe_audio::{AudioBackend, AudioCallback, DeviceBuffers, StreamConfig, StreamInfo};
use faderframe_audio_pipewire::PipeWireBackend;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

struct Count {
    frames: Arc<AtomicU64>,
    channels: Arc<AtomicU64>,
}

impl AudioCallback for Count {
    fn prepare(&mut self, _info: &StreamInfo) {}

    fn process(&mut self, io: &mut dyn DeviceBuffers) {
        self.channels.store(
            (io.input_channels() * 100 + io.output_channels()) as u64,
            Ordering::Relaxed,
        );
        for c in 0..io.output_channels() {
            io.output(c).fill(0.0);
        }
        let silent = (0..io.input_channels()).all(|c| io.input(c).len() == io.frames());
        assert!(silent);
        self.frames.fetch_add(io.frames() as u64, Ordering::Relaxed);
    }
}

#[test]
fn an_unlinked_node_runs_and_closes() {
    let mut backend = PipeWireBackend;
    if !backend.is_available() {
        eprintln!("no PipeWire server: skipped");
        return;
    }
    // The graph's sinks and sources, each with its channels' names.
    let devices = backend.enumerate_devices().unwrap();
    for d in &devices {
        assert!(!d.id.is_empty() && !d.name.is_empty(), "{d:?}");
        assert!(d.input_channels > 0 || d.output_channels > 0, "{d:?}");
        assert_eq!(d.output_names.len(), usize::from(d.output_channels));
        assert_eq!(d.input_names.len(), usize::from(d.input_channels));
    }
    if devices.iter().any(|d| d.output_channels > 0) {
        assert!(
            devices
                .iter()
                .any(|d| d.is_default && d.output_channels > 0),
            "a default sink: {devices:?}"
        );
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
    let channels = Arc::new(AtomicU64::new(0));
    let stream = backend
        .open_stream(
            StreamConfig {
                client_name: format!("FaderFrame test {}", std::process::id()),
                input_channels: 3,
                output_channels: 2,
                auto_connect: false,
                ..StreamConfig::default()
            },
            Box::new(Count {
                frames: Arc::clone(&frames),
                channels: Arc::clone(&channels),
            }),
        )
        .unwrap();
    let start = Instant::now();
    while frames.load(Ordering::Relaxed) < 48_000 && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let info = stream.info();
    assert!(
        frames.load(Ordering::Relaxed) >= 48_000,
        "processed {} frames",
        frames.load(Ordering::Relaxed)
    );
    assert_eq!(channels.load(Ordering::Relaxed), 302);
    assert!(info.sample_rate >= 8_000 && info.buffer_size > 0, "{info}");
    assert!(stream.status().running);
    // Not linked: nothing on its channels.
    let routing = stream.routing();
    assert_eq!(routing.outputs, vec![None, None]);
    assert_eq!(routing.inputs, vec![None, None, None]);
    drop(stream);
}
