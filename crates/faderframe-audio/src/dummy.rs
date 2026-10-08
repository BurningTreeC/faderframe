//! A hardware-less backend driven by a timer thread.
//!
//! It calls the engine at the real-time rate implied by the configured
//! sample rate and buffer size, feeding silence (or a test tone, see
//! [`DummyBackend::with_input_tone`]) and discarding output. This keeps the
//! transport, meters and DSP load display alive on machines without an
//! audio server, and is used in CI and recording tests.
//!
//! The thread sleeps *between* callbacks to emulate a device clock; nothing
//! sleeps inside the processing callback itself.

use crate::{
    AudioBackend, AudioCallback, AudioError, AudioStream, DeviceBuffers, DeviceInfo, OwnedBuffers,
    STANDARD_SAMPLE_RATES, StreamConfig, StreamInfo, StreamMonitor, StreamStatus, validate_format,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How far the device clock may fall behind before it gives up catching up.
const CATCH_UP: Duration = Duration::from_millis(500);

pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
pub const DEFAULT_BUFFER_SIZE: u32 = 256;

#[derive(Clone, Copy, Debug, Default)]
pub struct DummyBackend {
    /// Sine frequency fed to every input channel (amplitude 0.5), if any.
    input_tone: Option<f32>,
    /// Shape the tone into plucked notes (twice a second).
    pluck: bool,
    /// A cable from every output to the input of the same number, `n`
    /// frames longer than the device's own buffer (hardware inserts
    /// without hardware).
    loopback: Option<u32>,
}

impl DummyBackend {
    /// Inputs carry a steady 0.5-amplitude sine of `hz` instead of silence.
    pub fn with_input_tone(hz: f32) -> Self {
        Self {
            input_tone: Some(hz),
            ..Self::default()
        }
    }

    /// Outputs come back on the inputs of the same number (a patch cable
    /// on every channel), `delay` frames after the next callback's start:
    /// a round trip of one buffer plus `delay`.
    pub fn with_loopback(delay: u32) -> Self {
        Self {
            loopback: Some(delay),
            ..Self::default()
        }
    }

    /// Inputs carry plucked notes of `hz` (decaying, twice a second, input
    /// 2 an octave up) — a test signal with a recognisable waveform.
    pub fn with_input_pluck(hz: f32) -> Self {
        Self {
            input_tone: Some(hz),
            pluck: true,
            ..Self::default()
        }
    }
}

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
        let tone = self.input_tone;
        let pluck = self.pluck;
        let loopback = self.loopback;
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
                    let mut phase = 0.0f64;
                    let mut t = 0u64;
                    // The loopback's cable: a ring per channel, made once
                    // (long enough for any buffer and the delay).
                    // Output at stream frame n is input at n + buffer +
                    // delay: the write head that far ahead of the read
                    // head (for the buffer size the stream starts with).
                    let cable = loopback.map(|d| d as usize);
                    let ahead = cable.map_or(0, |d| d + info.buffer_size as usize);
                    let ring_len = ahead + 16_384;
                    let mut rings = vec![vec![0.0f32; ring_len]; ins.min(outs)];
                    let mut write = ahead;
                    let mut read = 0usize;
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
                        if let Some(hz) = tone {
                            let sr = info.sample_rate as f64;
                            let inc = std::f64::consts::TAU * hz as f64 / sr;
                            let start = phase;
                            for c in 0..ins {
                                let mut ph = start;
                                let octave = if pluck && c % 2 == 1 { 2.0 } else { 1.0 };
                                for (i, s) in bufs.input_mut(c).iter_mut().enumerate() {
                                    let env = if pluck {
                                        let x = ((t + i as u64) as f64 / sr) % 0.5;
                                        0.05 + 0.8 * (-x * 9.0).exp()
                                    } else {
                                        1.0
                                    };
                                    *s = ((ph * octave).sin() * 0.5 * env) as f32;
                                    ph += inc;
                                }
                                phase = ph % std::f64::consts::TAU;
                            }
                            t += info.buffer_size as u64;
                        }
                        if cable.is_some() {
                            let n = info.buffer_size as usize;
                            for (c, ring) in rings.iter_mut().enumerate() {
                                for (i, s) in bufs.input_mut(c).iter_mut().enumerate() {
                                    *s = ring[(read + i) % ring_len];
                                }
                            }
                            read = (read + n) % ring_len.max(1);
                        }
                        callback.process(&mut bufs);
                        if cable.is_some() {
                            let n = info.buffer_size as usize;
                            for (c, ring) in rings.iter_mut().enumerate() {
                                for (i, s) in bufs.output(c).iter().enumerate().take(n) {
                                    ring[(write + i) % ring_len] = *s;
                                }
                            }
                            write = (write + n) % ring_len.max(1);
                        }
                        monitor.record_callback();
                        next += period;
                        let now = Instant::now();
                        if next > now {
                            std::thread::sleep(next - now);
                        } else if now - next > (period * 4).max(CATCH_UP) {
                            // Fell far behind (system suspend, debugger): count
                            // it like an xrun and resynchronise. Shorter delays
                            // (a busy machine oversleeping) are caught up, so
                            // audio time keeps pace with the wall clock.
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
        let mut backend = DummyBackend::default();
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
        let mut backend = DummyBackend::default();
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
