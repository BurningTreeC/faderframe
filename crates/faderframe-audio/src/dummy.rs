//! A hardware-less backend driven by a timer thread.
//!
//! It calls the engine at the real-time rate implied by the configured
//! sample rate and buffer size, feeding silence and discarding output. This
//! keeps the transport, meters and DSP load display alive on machines
//! without an audio server, and is used in CI.
//!
//! The thread sleeps *between* callbacks to emulate a device clock; nothing
//! sleeps inside the processing callback itself.

use crate::{
    AudioBackend, AudioCallback, AudioError, AudioStream, DeviceInfo, OwnedBuffers,
    STANDARD_SAMPLE_RATES, StreamConfig, StreamInfo, StreamMonitor, StreamStatus, validate_format,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
pub const DEFAULT_BUFFER_SIZE: u32 = 256;

#[derive(Debug, Default)]
pub struct DummyBackend;

impl AudioBackend for DummyBackend {
    fn id(&self) -> &'static str {
        "dummy"
    }

    fn display_name(&self) -> &'static str {
        "No audio device (silent)"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        Ok(vec![DeviceInfo {
            id: "dummy".into(),
            name: "Silent dummy device".into(),
            input_channels: 2,
            output_channels: 2,
            sample_rates: STANDARD_SAMPLE_RATES.to_vec(),
            current_sample_rate: None,
            current_buffer_size: None,
            is_default: true,
        }])
    }

    fn open_stream(
        &mut self,
        config: StreamConfig,
        mut callback: Box<dyn AudioCallback>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let sample_rate = config.sample_rate.unwrap_or(DEFAULT_SAMPLE_RATE);
        let buffer_size = config.buffer_size.unwrap_or(DEFAULT_BUFFER_SIZE);
        validate_format(sample_rate, buffer_size)?;
        let info = StreamInfo {
            backend: "dummy",
            device: "Silent dummy device".into(),
            sample_rate,
            buffer_size,
            input_channels: config.input_channels,
            output_channels: config.output_channels,
            input_latency: 0,
            output_latency: 0,
        };
        let monitor = StreamMonitor::new(sample_rate, buffer_size);
        let stop = Arc::new(AtomicBool::new(false));
        let requested = Arc::new(AtomicU32::new(buffer_size));

        let thread = {
            let (monitor, stop, requested, mut info) = (
                Arc::clone(&monitor),
                Arc::clone(&stop),
                Arc::clone(&requested),
                info.clone(),
            );
            std::thread::Builder::new()
                .name("faderframe-dummy-audio".into())
                .spawn(move || {
                    let ins = info.input_channels as usize;
                    let outs = info.output_channels as usize;
                    let mut bufs = OwnedBuffers::new(ins, outs, info.buffer_size as usize);
                    callback.prepare(&info);
                    monitor.set_running(true);
                    let mut next = Instant::now();
                    while !stop.load(Ordering::Relaxed) {
                        let want = requested.load(Ordering::Relaxed);
                        if want != info.buffer_size {
                            info.buffer_size = want;
                            bufs = OwnedBuffers::new(ins, outs, want as usize);
                            monitor.set_buffer_size(want);
                            callback.prepare(&info);
                        }
                        let period = Duration::from_secs_f64(
                            info.buffer_size as f64 / info.sample_rate as f64,
                        );
                        bufs.set_frames(info.buffer_size as usize);
                        callback.process(&mut bufs);
                        monitor.record_callback();
                        next += period;
                        let now = Instant::now();
                        if next > now {
                            std::thread::sleep(next - now);
                        } else if now - next > period * 4 {
                            // Fell far behind (system suspend, debugger): count
                            // it like an xrun and resynchronise.
                            monitor.record_xrun();
                            next = now;
                        }
                    }
                    monitor.set_running(false);
                })
                .map_err(|e| AudioError::Stream(format!("cannot spawn dummy audio thread: {e}")))?
        };

        Ok(Box::new(DummyStream {
            info,
            monitor,
            stop,
            requested,
            thread: Some(thread),
        }))
    }
}

struct DummyStream {
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
    stop: Arc<AtomicBool>,
    requested: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl AudioStream for DummyStream {
    fn info(&self) -> StreamInfo {
        let mut info = self.info.clone();
        info.buffer_size = self.monitor.status().buffer_size;
        info
    }

    fn status(&self) -> StreamStatus {
        self.monitor.status()
    }

    fn request_buffer_size(&mut self, frames: u32) -> Result<(), AudioError> {
        validate_format(self.info.sample_rate, frames)?;
        self.requested.store(frames, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for DummyStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceBuffers;
    use std::sync::atomic::AtomicU64;

    struct Counting {
        frames: Arc<AtomicU64>,
        prepared: Arc<AtomicU32>,
    }

    impl AudioCallback for Counting {
        fn prepare(&mut self, info: &StreamInfo) {
            self.prepared.store(info.buffer_size, Ordering::Relaxed);
        }
        fn process(&mut self, io: &mut dyn DeviceBuffers) {
            for c in 0..io.output_channels() {
                io.output(c).fill(0.0);
            }
            self.frames.fetch_add(io.frames() as u64, Ordering::Relaxed);
        }
    }

    #[test]
    fn drives_callback_and_changes_buffer_size() {
        let frames = Arc::new(AtomicU64::new(0));
        let prepared = Arc::new(AtomicU32::new(0));
        let mut backend = DummyBackend;
        let mut stream = backend
            .open_stream(
                StreamConfig {
                    sample_rate: Some(96_000),
                    buffer_size: Some(128),
                    ..StreamConfig::default()
                },
                Box::new(Counting {
                    frames: Arc::clone(&frames),
                    prepared: Arc::clone(&prepared),
                }),
            )
            .unwrap();
        assert_eq!(stream.info().sample_rate, 96_000);
        let deadline = Instant::now() + Duration::from_secs(5);
        while frames.load(Ordering::Relaxed) < 128 * 8 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(frames.load(Ordering::Relaxed) >= 128 * 8);
        stream.request_buffer_size(512).unwrap();
        while prepared.load(Ordering::Relaxed) != 512 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(prepared.load(Ordering::Relaxed), 512);
        assert!(stream.request_buffer_size(100_000).is_err());
        drop(stream);
    }

    #[test]
    fn rejects_invalid_formats() {
        let mut backend = DummyBackend;
        let res = backend.open_stream(
            StreamConfig {
                sample_rate: Some(1),
                ..StreamConfig::default()
            },
            Box::new(Counting {
                frames: Arc::default(),
                prepared: Arc::default(),
            }),
        );
        assert!(matches!(res, Err(AudioError::UnsupportedConfig(_))));
    }
}
