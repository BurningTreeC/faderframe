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
//!
//! [`DummyDevices`] stands in for a machine's interfaces in tests: devices
//! with their own rates, buffer sizes and channels that can be unplugged
//! (a stream on one shuts down, like a USB interface pulled out) and
//! plugged in again.

use crate::{
    AudioBackend, AudioCallback, AudioError, AudioStream, Capabilities, DeviceBuffers, DeviceInfo,
    OwnedBuffers, RateControl, STANDARD_SAMPLE_RATES, StreamConfig, StreamInfo, StreamMonitor,
    StreamRouting, StreamStatus, validate_format,
};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How far the device clock may fall behind before it gives up catching up.
const CATCH_UP: Duration = Duration::from_millis(500);

pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
pub const DEFAULT_BUFFER_SIZE: u32 = 256;

/// A pretend device and its presence (cleared when it is unplugged; its
/// streams watch it).
type Plugged = (DeviceInfo, Arc<AtomicBool>);

/// Pretend interfaces for a [`DummyBackend`], shared with the test that
/// plugs and unplugs them.
#[derive(Clone, Debug, Default)]
pub struct DummyDevices(Arc<Mutex<Vec<Plugged>>>);

impl DummyDevices {
    pub fn new(devices: Vec<DeviceInfo>) -> Self {
        let this = Self::default();
        for d in devices {
            this.plug(d);
        }
        this
    }

    /// A device appears (again).
    pub fn plug(&self, device: DeviceInfo) {
        let mut list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|(d, _)| d.id != device.id);
        list.push((device, Arc::new(AtomicBool::new(true))));
    }

    /// A device goes away: streams on it shut down.
    pub fn unplug(&self, id: &str) {
        let mut list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|(d, present)| {
            if d.id == id {
                present.store(false, Ordering::Relaxed);
            }
            d.id != id
        });
    }

    pub fn list(&self) -> Vec<DeviceInfo> {
        let list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        list.iter().map(|(d, _)| d.clone()).collect()
    }

    /// A device by id (or name), with its presence flag.
    fn find(&self, wanted: &str) -> Option<Plugged> {
        let list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        list.iter()
            .find(|(d, _)| d.id == wanted)
            .or_else(|| list.iter().find(|(d, _)| d.name == wanted))
            .cloned()
    }

    /// The default device of a direction.
    fn default_for(&self, output: bool) -> Option<Plugged> {
        let list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let has = |d: &DeviceInfo| {
            if output {
                d.output_channels > 0
            } else {
                d.input_channels > 0
            }
        };
        list.iter()
            .find(|(d, _)| d.is_default && has(d))
            .or_else(|| list.iter().find(|(d, _)| has(d)))
            .cloned()
    }
}

/// A pretend interface: `rates` and `buffers` are what it takes (empty /
/// `None`: anything), the first rate its current one.
pub fn mock_device(
    id: &str,
    inputs: u16,
    outputs: u16,
    rates: &[u32],
    buffers: Option<(u32, u32)>,
) -> DeviceInfo {
    DeviceInfo {
        id: id.into(),
        name: id.into(),
        input_channels: inputs,
        output_channels: outputs,
        input_names: (1..=inputs).map(|n| format!("In {n}")).collect(),
        output_names: (1..=outputs).map(|n| format!("Out {n}")).collect(),
        sample_rates: rates.to_vec(),
        current_sample_rate: rates.first().copied(),
        current_buffer_size: None,
        buffer_sizes: buffers,
        clock: Some(id.into()),
        is_default: false,
    }
}

#[derive(Clone, Debug, Default)]
pub struct DummyBackend {
    /// Sine frequency fed to every input channel (amplitude 0.5), if any.
    input_tone: Option<f32>,
    /// Shape the tone into plucked notes (twice a second).
    pluck: bool,
    /// Input n's tone this many semitones above input n − 1's.
    spread: f32,
    /// A cable from every output to the input of the same number, `n`
    /// frames longer than the device's own buffer (hardware inserts
    /// without hardware).
    loopback: Option<u32>,
    /// Pretend interfaces (`None`: the one silent device).
    devices: Option<DummyDevices>,
}

impl DummyBackend {
    /// Inputs carry a steady 0.5-amplitude sine of `hz` instead of silence.
    pub fn with_input_tone(hz: f32) -> Self {
        Self {
            input_tone: Some(hz),
            ..Self::default()
        }
    }

    /// Input n carries a steady sine `n × semitones` above `hz` (several
    /// singers, one on each input), from one second into the stream (a
    /// test can listen and start recording before the first note).
    pub fn with_input_spread(hz: f32, semitones: f32) -> Self {
        Self {
            input_tone: Some(hz),
            spread: semitones,
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

    /// Offer these pretend interfaces instead of the one silent device:
    /// streams open on one of them (by id), refuse rates and buffer sizes
    /// it does not take, and shut down when it is unplugged.
    pub fn with_devices(devices: DummyDevices) -> Self {
        Self {
            devices: Some(devices),
            ..Self::default()
        }
    }

    /// The pretend device a stream opens on for a direction.
    fn pick(&self, wanted: &[String], output: bool) -> Result<Option<Plugged>, AudioError> {
        let Some(devices) = &self.devices else {
            return Ok(None);
        };
        match wanted.first() {
            Some(id) => devices
                .find(id)
                .map(Some)
                .ok_or_else(|| AudioError::DeviceNotFound(id.clone())),
            None => Ok(devices.default_for(output)),
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

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            rate: RateControl::Device,
            live_buffer_size: true,
            live_sample_rate: false,
            output_devices: 1,
            input_devices: 1,
            clocked_by_server: false,
            follows_devices: false,
            free_channel_counts: true,
            force_format: false,
        }
    }

    fn is_available(&self) -> bool {
        true
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        if let Some(devices) = &self.devices {
            return Ok(devices.list());
        }
        Ok(vec![DeviceInfo {
            id: "dummy".into(),
            name: "Silent dummy device".into(),
            // Any count opens; these are what a stream opens by default.
            input_channels: 8,
            output_channels: 2,
            sample_rates: STANDARD_SAMPLE_RATES.to_vec(),
            current_sample_rate: None,
            current_buffer_size: None,
            is_default: true,
            ..DeviceInfo::default()
        }])
    }

    fn open_stream(
        &mut self,
        config: StreamConfig,
        mut callback: Box<dyn AudioCallback>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let output = self.pick(&config.output_devices, true)?;
        // The output device records where it can.
        let input = match output.clone().filter(|(d, _)| d.input_channels > 0) {
            Some(same) if config.input_devices.is_empty() => Some(same),
            _ => self.pick(&config.input_devices, false)?,
        };
        let device = output.as_ref().map(|(d, _)| d);
        let sample_rate = config
            .sample_rate
            .or(device.and_then(|d| d.current_sample_rate))
            .unwrap_or(DEFAULT_SAMPLE_RATE);
        let buffer_size = config
            .buffer_size
            .or(device.and_then(|d| d.current_buffer_size))
            .unwrap_or(DEFAULT_BUFFER_SIZE);
        validate_format(sample_rate, buffer_size)?;
        for (d, _) in output.iter().chain(input.iter()) {
            if !d.sample_rates.is_empty() && !d.sample_rates.contains(&sample_rate) {
                return Err(AudioError::UnsupportedConfig(format!(
                    "{} cannot run at {sample_rate} Hz",
                    d.name
                )));
            }
            if let Some((lo, hi)) = d.buffer_sizes
                && !(lo..=hi).contains(&buffer_size)
            {
                return Err(AudioError::UnsupportedConfig(format!(
                    "{} takes buffers of {lo}–{hi} frames, not {buffer_size}",
                    d.name
                )));
            }
        }
        let routing = output.as_ref().map(|(o, _)| {
            let name = |d: &DeviceInfo, names: &[String], c: usize| {
                names.get(c).map(|n| format!("{} · {n}", d.name))
            };
            StreamRouting {
                outputs: (0..config.output_channels as usize)
                    .map(|c| name(o, &o.output_names, c))
                    .collect(),
                inputs: (0..config.input_channels as usize)
                    .map(|c| input.as_ref().and_then(|(i, _)| name(i, &i.input_names, c)))
                    .collect(),
                missing: Vec::new(),
                devices: std::iter::once(o.name.clone())
                    .chain(input.iter().map(|(i, _)| i.name.clone()))
                    .fold(Vec::new(), |mut v, n| {
                        if !v.contains(&n) {
                            v.push(n);
                        }
                        v
                    }),
            }
        });
        let present: Vec<Arc<AtomicBool>> = output
            .iter()
            .chain(input.iter())
            .map(|(_, p)| Arc::clone(p))
            .collect();
        let buffers = output
            .iter()
            .chain(input.iter())
            .filter_map(|(d, _)| d.buffer_sizes)
            .fold(None, |acc: Option<(u32, u32)>, (lo, hi)| {
                Some(acc.map_or((lo, hi), |(a, b)| (a.max(lo), b.min(hi))))
            });
        let info = StreamInfo {
            backend: "dummy",
            device: device.map_or_else(|| "Silent dummy device".into(), |d| d.name.clone()),
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
        let spread = self.spread;
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
                    // Spread tones: a phase per input, and frames so far.
                    let mut phases = vec![0.0f64; ins];
                    let mut spread_frames = 0u64;
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
                        // A pretend interface pulled out: the stream ends
                        // as a real one would.
                        if present.iter().any(|p| !p.load(Ordering::Relaxed)) {
                            monitor.mark_shut_down();
                            return;
                        }
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
                        if let Some(hz) = tone.filter(|_| spread != 0.0) {
                            let sr = info.sample_rate as f64;
                            let begin = info.sample_rate as u64;
                            for (c, ph) in phases.iter_mut().enumerate() {
                                let f = hz as f64 * 2f64.powf(c as f64 * spread as f64 / 12.0);
                                let inc = std::f64::consts::TAU * f / sr;
                                for (i, s) in bufs.input_mut(c).iter_mut().enumerate() {
                                    if spread_frames + (i as u64) < begin {
                                        *s = 0.0;
                                        continue;
                                    }
                                    *s = (ph.sin() * 0.5) as f32;
                                    *ph = (*ph + inc) % std::f64::consts::TAU;
                                }
                            }
                            spread_frames += info.buffer_size as u64;
                        } else if let Some(hz) = tone {
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
            buffers,
            routing,
            thread: Some(thread),
        }))
    }
}

struct DummyStream {
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
    stop: Arc<AtomicBool>,
    requested: Arc<AtomicU32>,
    /// Buffer sizes the pretend devices take.
    buffers: Option<(u32, u32)>,
    routing: Option<StreamRouting>,
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
        if let Some((lo, hi)) = self.buffers
            && !(lo..=hi).contains(&frames)
        {
            return Err(AudioError::UnsupportedConfig(format!(
                "{} takes buffers of {lo}–{hi} frames, not {frames}",
                self.info.device
            )));
        }
        self.requested.store(frames, Ordering::Relaxed);
        Ok(())
    }

    fn routing(&self) -> StreamRouting {
        self.routing.clone().unwrap_or_default()
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
    fn pretend_devices_check_formats_and_go_away() {
        let devices = DummyDevices::new(vec![
            mock_device("a", 2, 2, &[44_100], Some((64, 512))),
            mock_device("b", 8, 8, &[48_000, 96_000], None),
        ]);
        let mut backend = DummyBackend::with_devices(devices.clone());
        assert_eq!(backend.enumerate_devices().unwrap().len(), 2);
        let counting = || {
            Box::new(Counting {
                frames: Arc::default(),
                prepared: Arc::default(),
            })
        };
        let open = |backend: &mut DummyBackend, device: &str, rate: Option<u32>| {
            backend.open_stream(
                StreamConfig {
                    output_devices: vec![device.into()],
                    sample_rate: rate,
                    buffer_size: Some(128),
                    ..StreamConfig::default()
                },
                counting(),
            )
        };
        assert!(matches!(
            open(&mut backend, "a", Some(48_000)),
            Err(AudioError::UnsupportedConfig(_))
        ));
        assert!(matches!(
            open(&mut backend, "c", None),
            Err(AudioError::DeviceNotFound(_))
        ));
        let mut stream = open(&mut backend, "a", None).unwrap();
        assert_eq!(stream.info().sample_rate, 44_100, "the device's own rate");
        assert_eq!(stream.info().device, "a");
        assert_eq!(stream.routing().output(1), Some("a · Out 2"));
        assert!(
            stream.request_buffer_size(1024).is_err(),
            "beyond the device"
        );
        stream.request_buffer_size(256).unwrap();
        devices.unplug("a");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !stream.status().shut_down && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(stream.status().shut_down, "unplugged");
        assert_eq!(backend.enumerate_devices().unwrap().len(), 1);
        devices.plug(mock_device("a", 2, 2, &[44_100], Some((64, 512))));
        if let Err(e) = open(&mut backend, "a", None) {
            panic!("back: {e}");
        }
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
