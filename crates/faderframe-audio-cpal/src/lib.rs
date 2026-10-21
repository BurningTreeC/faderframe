//! The operating system's own audio API through cpal: WASAPI on Windows,
//! CoreAudio on macOS, ALSA on Linux — and Steinberg ASIO drivers on
//! Windows when built with the `asio` feature (opt-in: it compiles the ASIO
//! SDK, whose licence — Steinberg's own or GPLv3 — then applies to the
//! binary; see the README).
//!
//! cpal opens playback and capture as separate streams. The playback
//! callback drives the engine; captured frames reach it through a lock-free
//! ring (so recording works, with the ring's few milliseconds of extra input
//! latency). Devices that do not take `f32` get converted `i16`/`i32`
//! samples. The streams live on a control thread that owns them, so the
//! returned stream is `Send` on every platform.
//!
//! Capture comes from the device asked for, else from the output device
//! itself where it records (one clock), else from the system's default
//! input. Two devices run on two clocks and nothing here resamples between
//! them: the ring then slowly fills or empties, and each overflow or gap
//! is counted as an xrun (an aggregate device made by the system — macOS
//! Audio MIDI Setup — or one interface for both directions avoids it).
//! Latencies come from cpal's callback timestamps.
//!
//! No cpal type escapes this crate: the engine only sees
//! [`faderframe_audio::DeviceBuffers`].

#![forbid(unsafe_code)]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, SupportedBufferSize};
use faderframe_audio::{
    AudioBackend, AudioCallback, AudioError, AudioStream, Capabilities, DeviceBuffers, DeviceInfo,
    MAX_BUFFER_SIZE, RateControl, STANDARD_SAMPLE_RATES, StreamConfig, StreamInfo, StreamMonitor,
    StreamRouting, StreamStatus,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Seconds of capture the input ring holds.
const INPUT_RING_SECONDS: f32 = 0.5;
/// Seconds of smaller callbacks before the nominal block size shrinks.
const SHRINK_AFTER_SECONDS: u64 = 3;

/// The block size the stream reports. Callbacks vary in size on some
/// systems (WASAPI's shared mode, ALSA through cpal, a callback larger
/// than [`MAX_BUFFER_SIZE`] run in chunks), and every size reported is a
/// graph rebuild for the buffered devices (preamps, buses rendered
/// ahead) and a step in the output latency. So: the largest callback,
/// larger at once (the buffered devices need room for it), smaller only
/// after [`SHRINK_AFTER_SECONDS`] of smaller callbacks.
#[derive(Debug)]
struct Nominal {
    size: u32,
    /// The largest callback since the window began, and its frames.
    window_max: u32,
    window_frames: u64,
}

impl Nominal {
    fn new(size: u32) -> Self {
        Self {
            size,
            window_max: 0,
            window_frames: 0,
        }
    }

    /// A callback of `n` frames at `rate`: the new nominal size when it
    /// changes.
    fn see(&mut self, n: u32, rate: u32) -> Option<u32> {
        if n > self.size {
            self.size = n;
            self.window_max = 0;
            self.window_frames = 0;
            return Some(n);
        }
        self.window_max = self.window_max.max(n);
        self.window_frames += u64::from(n);
        if self.window_frames < u64::from(rate) * SHRINK_AFTER_SECONDS {
            return None;
        }
        let max = std::mem::take(&mut self.window_max);
        self.window_frames = 0;
        (max > 0 && max < self.size).then(|| {
            self.size = max;
            max
        })
    }
}

/// Which of cpal's audio APIs a backend uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Api {
    /// The platform's default (WASAPI, CoreAudio or ALSA).
    #[default]
    System,
    /// Steinberg ASIO drivers.
    #[cfg(all(windows, feature = "asio"))]
    Asio,
}

impl Api {
    pub fn name(self) -> &'static str {
        match self {
            Api::System => api_name(),
            #[cfg(all(windows, feature = "asio"))]
            Api::Asio => "ASIO",
        }
    }

    fn host(self) -> Result<cpal::Host, AudioError> {
        match self {
            Api::System => Ok(cpal::default_host()),
            #[cfg(all(windows, feature = "asio"))]
            Api::Asio => cpal::host_from_id(cpal::HostId::Asio)
                .map_err(|e| AudioError::BackendUnavailable(e.to_string())),
        }
    }

    /// Capture comes from the playback device itself (an ASIO driver is
    /// one duplex device), not from the system's default input.
    fn duplex(self) -> bool {
        self != Api::System
    }
}

/// This build can drive ASIO devices.
pub const ASIO: bool = cfg!(all(windows, feature = "asio"));

#[derive(Debug, Default)]
pub struct CpalBackend {
    api: Api,
}

impl CpalBackend {
    pub fn new(api: Api) -> Self {
        Self { api }
    }
}

/// The platform's default API name.
pub fn api_name() -> &'static str {
    if cfg!(windows) {
        "WASAPI"
    } else if cfg!(target_os = "macos") {
        "CoreAudio"
    } else {
        "ALSA"
    }
}

fn stream_err(e: impl std::fmt::Display) -> AudioError {
    AudioError::Stream(e.to_string())
}

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|desc| desc.to_string())
        .unwrap_or_else(|_| d.to_string())
}

fn device_id(d: &cpal::Device) -> String {
    d.id().map_or_else(|_| device_name(d), |id| id.to_string())
}

/// Rates out of the standard list a set of config ranges covers.
fn rates(ranges: &[cpal::SupportedStreamConfigRange]) -> Vec<u32> {
    STANDARD_SAMPLE_RATES
        .into_iter()
        .filter(|r| {
            ranges
                .iter()
                .any(|c| (c.min_sample_rate()..=c.max_sample_rate()).contains(r))
        })
        .collect()
}

/// The buffer sizes a set of config ranges agree on, if they say.
fn buffer_range(ranges: &[cpal::SupportedStreamConfigRange]) -> Option<(u32, u32)> {
    ranges
        .iter()
        .filter_map(|c| match c.buffer_size() {
            SupportedBufferSize::Range { min, max } => Some((*min, *max)),
            SupportedBufferSize::Unknown => None,
        })
        .reduce(|(a, b), (lo, hi)| (a.min(lo), b.max(hi)))
}

/// The config range to open: at `rate`, the fewest channels that give
/// `wanted` (else the most there are), `f32` before `i32` before `i16`.
fn pick_config(
    ranges: &[cpal::SupportedStreamConfigRange],
    rate: u32,
    wanted: u16,
) -> Option<&cpal::SupportedStreamConfigRange> {
    let format_rank = |f: SampleFormat| match f {
        SampleFormat::F32 => 0,
        SampleFormat::I32 => 1,
        SampleFormat::I16 => 2,
        _ => 3,
    };
    ranges
        .iter()
        .filter(|c| {
            format_rank(c.sample_format()) < 3
                && (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate)
        })
        .min_by_key(|c| {
            let ch = c.channels();
            // Enough channels first (the fewest of those), else the most.
            let fit = if ch >= wanted {
                (0, ch - wanted)
            } else {
                (1, wanted - ch)
            };
            (fit, format_rank(c.sample_format()))
        })
}

impl AudioBackend for CpalBackend {
    fn id(&self) -> &'static str {
        match self.api {
            Api::System => "system",
            #[cfg(all(windows, feature = "asio"))]
            Api::Asio => "asio",
        }
    }

    fn display_name(&self) -> &'static str {
        self.api.name()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            rate: RateControl::Device,
            live_buffer_size: false,
            live_sample_rate: false,
            output_devices: 1,
            // An ASIO driver records from itself.
            input_devices: usize::from(!self.api.duplex()),
            clocked_by_server: false,
            follows_devices: false,
            free_channel_counts: false,
            force_format: false,
        }
    }

    fn is_available(&self) -> bool {
        self.api
            .host()
            .is_ok_and(|h| h.default_output_device().is_some())
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        let host = self.api.host()?;
        let default_out = host.default_output_device().map(|d| device_id(&d));
        let default_in = host.default_input_device().map(|d| device_id(&d));
        let devices = host
            .devices()
            .map_err(|e| AudioError::BackendUnavailable(e.to_string()))?;
        Ok(devices
            .filter_map(|d| {
                let outs: Vec<_> = d.supported_output_configs().into_iter().flatten().collect();
                let ins: Vec<_> = d.supported_input_configs().into_iter().flatten().collect();
                if outs.is_empty() && ins.is_empty() {
                    return None;
                }
                let current = d
                    .default_output_config()
                    .or_else(|_| d.default_input_config())
                    .ok();
                let id = device_id(&d);
                let either = if outs.is_empty() { &ins } else { &outs };
                Some(DeviceInfo {
                    is_default: default_out.as_deref() == Some(id.as_str())
                        || default_in.as_deref() == Some(id.as_str()),
                    name: device_name(&d),
                    input_channels: ins.iter().map(|c| c.channels()).max().unwrap_or(0),
                    output_channels: outs.iter().map(|c| c.channels()).max().unwrap_or(0),
                    sample_rates: rates(either),
                    current_sample_rate: current.as_ref().map(|c| c.sample_rate()),
                    current_buffer_size: None,
                    buffer_sizes: buffer_range(either),
                    // Each device its own clock, as far as anyone here can
                    // tell.
                    clock: Some(id.clone()),
                    id,
                    ..DeviceInfo::default()
                })
            })
            .collect())
    }

    fn open_stream(
        &mut self,
        config: StreamConfig,
        callback: Box<dyn AudioCallback>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let api = self.api;
        let thread = std::thread::Builder::new()
            .name("audio-control".into())
            .spawn(move || match open(api, &config, callback) {
                Ok((streams, info, monitor, workgroup, routing)) => {
                    let _ = ready_tx.send(Ok((info, monitor, workgroup, routing)));
                    // Keep the streams until asked to stop.
                    let _ = stop_rx.recv();
                    drop(streams);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(stream_err)?;
        let (info, monitor, workgroup, routing) =
            match ready_rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    let _ = thread.join();
                    return Err(e);
                }
                Err(_) => return Err(AudioError::Stream("the device did not open in time".into())),
            };
        tracing::info!("{} stream open: {info}", self.api.name());
        Ok(Box::new(CpalStream {
            thread: Some(thread),
            stop: stop_tx,
            info,
            monitor,
            workgroup,
            routing,
        }))
    }
}

/// What `open` returns: the streams to keep, their description, the
/// stream monitor, the device's audio workgroup and the devices used.
type Opened = (
    Vec<cpal::Stream>,
    StreamInfo,
    Arc<StreamMonitor>,
    Option<faderframe_realtime::Workgroup>,
    StreamRouting,
);

/// A device by id (or name) among `devices`.
fn find(mut devices: impl Iterator<Item = cpal::Device>, wanted: &str) -> Option<cpal::Device> {
    let all: Vec<cpal::Device> = devices.by_ref().collect();
    all.iter()
        .find(|d| device_id(d) == wanted)
        .or_else(|| all.iter().find(|d| device_name(d) == wanted))
        .cloned()
}

/// Open playback (and capture) on the calling thread.
fn open(
    api: Api,
    config: &StreamConfig,
    callback: Box<dyn AudioCallback>,
) -> Result<Opened, AudioError> {
    let host = api.host()?;
    let device = match config.output_devices.first() {
        Some(id) => find(host.output_devices().map_err(stream_err)?, id)
            .ok_or_else(|| AudioError::DeviceNotFound(id.clone()))?,
        None => host
            .default_output_device()
            .ok_or_else(|| AudioError::DeviceNotFound("no default output device".into()))?,
    };
    let default = device.default_output_config().map_err(stream_err)?;
    let wanted_rate = config.sample_rate.unwrap_or(default.sample_rate());
    let ranges: Vec<_> = device
        .supported_output_configs()
        .map_err(stream_err)?
        .collect();
    // As many channels as the stream wants where the device has them.
    let wanted_outputs = config.output_channels.max(default.channels());
    let range = pick_config(&ranges, wanted_rate, wanted_outputs).ok_or_else(|| {
        AudioError::UnsupportedConfig(format!(
            "{} cannot run at {wanted_rate} Hz in a supported sample format",
            device_name(&device)
        ))
    })?;
    let format = range.sample_format();
    let channels = range.channels();
    let buffer_size = match (config.buffer_size, range.buffer_size()) {
        (Some(n), SupportedBufferSize::Range { min, max }) if (*min..=*max).contains(&n) => {
            cpal::BufferSize::Fixed(n)
        }
        (Some(n), SupportedBufferSize::Unknown) => cpal::BufferSize::Fixed(n),
        _ => cpal::BufferSize::Default,
    };
    let nominal = match buffer_size {
        cpal::BufferSize::Fixed(n) => n,
        cpal::BufferSize::Default => 512,
    };
    faderframe_audio::validate_format(wanted_rate, nominal.min(MAX_BUFFER_SIZE))?;
    let out_config = cpal::StreamConfig {
        channels,
        sample_rate: wanted_rate,
        buffer_size,
    };
    let monitor = StreamMonitor::new(wanted_rate, nominal);

    // Capture: the device asked for; else the output device itself where
    // it records (one clock, no drift); else the system's default input
    // (a clock of its own: see `Rt::run`). An ASIO driver is one duplex
    // device.
    let mut streams = Vec::new();
    let mut input_channels = 0u16;
    let mut input_rx = None;
    let input = if api.duplex() {
        Some(device.clone())
    } else if let Some(id) = config.input_devices.first() {
        Some(
            find(host.input_devices().map_err(stream_err)?, id)
                .ok_or_else(|| AudioError::DeviceNotFound(id.clone()))?,
        )
    } else if device.supports_input() {
        Some(device.clone())
    } else {
        host.default_input_device()
    };
    let hw_input_latency = Arc::new(AtomicU32::new(0));
    let mut input_name = None;
    if config.input_channels > 0
        && let Some(input) = input
        && let Ok(in_default) = input.default_input_config()
    {
        let in_ranges: Vec<_> = input
            .supported_input_configs()
            .into_iter()
            .flatten()
            .collect();
        let (in_channels, in_format) =
            match pick_config(&in_ranges, wanted_rate, config.input_channels) {
                Some(c) => (c.channels(), c.sample_format()),
                None => (in_default.channels(), in_default.sample_format()),
            };
        let in_config = cpal::StreamConfig {
            channels: in_channels,
            sample_rate: wanted_rate,
            buffer_size: cpal::BufferSize::Default,
        };
        let cap = (wanted_rate as f32 * INPUT_RING_SECONDS) as usize * in_config.channels as usize;
        let (tx, rx) = rtrb::RingBuffer::new(cap.max(1024));
        let feed = Feed {
            tx,
            monitor: Arc::clone(&monitor),
            latency: Arc::clone(&hw_input_latency),
            channels: usize::from(in_channels.max(1)),
            rate: wanted_rate,
            promoted: false,
        };
        match build_input(&input, &in_config, in_format, feed) {
            Ok(s) => {
                input_channels = in_config.channels;
                input_rx = Some(rx);
                input_name = Some(device_name(&input));
                streams.push(s);
            }
            Err(e) => tracing::warn!("{}: no capture ({e})", api.name()),
        }
    }

    let output_name = device_name(&device);
    let info = StreamInfo {
        backend: if api.duplex() { "asio" } else { "system" },
        device: match &input_name {
            Some(i) if *i != output_name => format!("{output_name} + {i}"),
            _ => output_name.clone(),
        },
        sample_rate: wanted_rate,
        buffer_size: nominal,
        input_channels,
        output_channels: channels,
        input_latency: 0,
        output_latency: 0,
    };
    let routing = StreamRouting {
        outputs: (1..=channels)
            .map(|c| Some(format!("{output_name} · {c}")))
            .collect(),
        inputs: (1..=input_channels)
            .map(|c| input_name.as_ref().map(|n| format!("{n} · {c}")))
            .collect(),
        missing: Vec::new(),
        devices: std::iter::once(output_name.clone())
            .chain(input_name.clone().filter(|i| *i != output_name))
            .collect(),
    };
    let mut callback = callback;
    callback.prepare(&info);
    let rt = Rt {
        callback,
        info: info.clone(),
        monitor: Arc::clone(&monitor),
        inputs: vec![vec![0.0; MAX_BUFFER_SIZE as usize]; input_channels as usize],
        outputs: vec![vec![0.0; MAX_BUFFER_SIZE as usize]; channels as usize],
        input_rx,
        frame: vec![0.0; input_channels as usize],
        promoted: false,
        nominal: Nominal::new(nominal),
        hw_input_latency,
        captured: false,
    };
    let output = match format {
        SampleFormat::F32 => build_output::<f32>(&device, &out_config, rt),
        SampleFormat::I32 => build_output::<i32>(&device, &out_config, rt),
        _ => build_output::<i16>(&device, &out_config, rt),
    }?;
    for s in &streams {
        s.play().map_err(stream_err)?;
    }
    output.play().map_err(stream_err)?;
    streams.push(output);
    monitor.set_running(true);
    // CoreAudio: the device's IO-thread workgroup (cpal's device id is the
    // CoreAudio UID), for the DSP workers.
    let workgroup = device
        .id()
        .ok()
        .and_then(|id| faderframe_realtime::Workgroup::of_device(id.id()));
    if workgroup.is_some() {
        tracing::debug!("the DSP workers join the device's audio workgroup");
    }
    Ok((streams, info, monitor, workgroup, routing))
}

fn on_error(monitor: &StreamMonitor, e: cpal::Error) {
    match e.kind() {
        cpal::ErrorKind::Xrun => monitor.record_xrun(),
        cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied => {}
        _ => monitor.mark_shut_down(),
    }
}

/// Frames in a duration at a rate.
fn frames_in(d: Duration, rate: u32) -> u32 {
    (d.as_secs_f64() * f64::from(rate))
        .round()
        .min(f64::from(u32::MAX - 1)) as u32
}

/// The capture callback's side of the input ring.
struct Feed {
    tx: rtrb::Producer<f32>,
    monitor: Arc<StreamMonitor>,
    /// Capture to callback, beyond the callback's own frames (frames).
    latency: Arc<AtomicU32>,
    channels: usize,
    rate: u32,
    promoted: bool,
}

impl Feed {
    fn push<T: Copy>(&mut self, data: &[T], info: &cpal::InputCallbackInfo, f: impl Fn(T) -> f32) {
        // The capture thread is on the audio path too.
        if !self.promoted {
            self.promoted = true;
            faderframe_realtime::priority::make_audio_thread_realtime();
        }
        let ts = info.timestamp();
        let own = (data.len() / self.channels) as u32;
        let since = frames_in(ts.callback.duration_since(ts.capture), self.rate);
        self.latency
            .store(since.saturating_sub(own), Ordering::Relaxed);
        let mut full = false;
        for v in data {
            full |= self.tx.push(f(*v)).is_err();
        }
        if full {
            // The playback side fell behind (another clock running slower,
            // or a stall): captured audio was lost.
            self.monitor.record_xrun();
        }
    }
}

fn build_input(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: SampleFormat,
    mut feed: Feed,
) -> Result<cpal::Stream, AudioError> {
    let m = Arc::clone(&feed.monitor);
    let err = move |e| on_error(&m, e);
    let stream = match format {
        SampleFormat::F32 => device.build_input_stream::<f32, _, _>(
            *config,
            move |data, info| feed.push(data, info, |v| v),
            err,
            None,
        ),
        SampleFormat::I32 => device.build_input_stream::<i32, _, _>(
            *config,
            move |data, info| feed.push(data, info, f32::from_sample),
            err,
            None,
        ),
        _ => device.build_input_stream::<i16, _, _>(
            *config,
            move |data, info| feed.push(data, info, f32::from_sample),
            err,
            None,
        ),
    };
    stream.map_err(stream_err)
}

/// State of the playback callback (allocated before the stream starts).
struct Rt {
    callback: Box<dyn AudioCallback>,
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
    input_rx: Option<rtrb::Consumer<f32>>,
    /// One interleaved capture frame.
    frame: Vec<f32>,
    /// The callback's thread made realtime (cpal leaves it as it comes:
    /// its own promotion is a feature not built), on the first callback.
    promoted: bool,
    /// The block size reported (`info.buffer_size` follows it).
    nominal: Nominal,
    /// The capture side's latency beyond its callbacks (frames).
    hw_input_latency: Arc<AtomicU32>,
    /// Captured frames have arrived (a dry ring after that is a dropout).
    captured: bool,
}

struct Io<'a> {
    frames: usize,
    inputs: &'a [Vec<f32>],
    outputs: &'a mut [Vec<f32>],
}

impl DeviceBuffers for Io<'_> {
    fn frames(&self) -> usize {
        self.frames
    }

    fn input_channels(&self) -> usize {
        self.inputs.len()
    }

    fn output_channels(&self) -> usize {
        self.outputs.len()
    }

    fn input(&self, channel: usize) -> &[f32] {
        &self.inputs[channel][..self.frames]
    }

    fn output(&mut self, channel: usize) -> &mut [f32] {
        &mut self.outputs[channel][..self.frames]
    }
}

impl Rt {
    /// Latencies from this callback's timestamps: until the first frame
    /// written is heard, beyond the callback's own frames; and captured
    /// audio's way here, through the ring (realtime safe: atomics).
    fn measure(&mut self, info: &cpal::OutputCallbackInfo, frames: usize) {
        let ts = info.timestamp();
        let rate = self.info.sample_rate;
        let ahead = frames_in(ts.playback.duration_since(ts.callback), rate);
        let output = ahead.saturating_sub(frames as u32);
        self.monitor.set_output_latency(output);
        if let Some(rx) = &self.input_rx {
            let waiting = (rx.slots() / self.frame.len().max(1)) as u32;
            self.monitor
                .set_input_latency(self.hw_input_latency.load(Ordering::Relaxed) + waiting);
        }
        // The engine hears of a change of more than 2 ms (the timestamps
        // jitter by less than that from callback to callback).
        if output.abs_diff(self.info.output_latency) > rate / 500 {
            self.info.output_latency = output;
            self.callback.prepare(&self.info);
        }
    }

    /// One chunk of at most `MAX_BUFFER_SIZE` frames into `out`
    /// (interleaved).
    fn run<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T]) {
        if !self.promoted {
            // Once, at the stream's start (system calls): realtime, and
            // the stack mapped before deep calls need it.
            self.promoted = true;
            faderframe_realtime::priority::make_audio_thread_realtime();
            faderframe_realtime::memory::prefault_stack(256 * 1024);
        }
        let ch = self.outputs.len().max(1);
        let n = out.len() / ch;
        if let Some(size) = self.nominal.see(n as u32, self.info.sample_rate) {
            self.info.buffer_size = size;
            self.monitor.set_buffer_size(size);
            self.callback.prepare(&self.info);
        }
        // Captured frames, or silence when the ring runs dry.
        let in_ch = self.inputs.len();
        let mut dry = false;
        for i in 0..n {
            let got = match &mut self.input_rx {
                Some(rx) if rx.slots() >= in_ch => {
                    for v in self.frame.iter_mut() {
                        *v = rx.pop().unwrap_or(0.0);
                    }
                    true
                }
                Some(_) => {
                    dry = true;
                    false
                }
                None => false,
            };
            self.captured |= got;
            for (c, buf) in self.inputs.iter_mut().enumerate() {
                buf[i] = if got { self.frame[c] } else { 0.0 };
            }
        }
        if dry && self.captured {
            // Capture fell behind (another clock running faster here):
            // the input had a gap.
            self.monitor.record_xrun();
        }
        let mut io = Io {
            frames: n,
            inputs: &self.inputs,
            outputs: &mut self.outputs,
        };
        self.callback.process(&mut io);
        for (i, frame) in out.chunks_exact_mut(ch).enumerate() {
            for (c, s) in frame.iter_mut().enumerate() {
                *s = T::from_sample(self.outputs[c][i]);
            }
        }
        self.monitor.record_callback();
    }
}

fn build_output<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut rt: Rt,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let m = Arc::clone(&rt.monitor);
    let ch = config.channels.max(1) as usize;
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], info| {
                rt.measure(info, data.len() / ch);
                for chunk in data.chunks_mut(MAX_BUFFER_SIZE as usize * ch) {
                    rt.run(chunk);
                }
            },
            move |e| on_error(&m, e),
            None,
        )
        .map_err(stream_err)
}

struct CpalStream {
    thread: Option<JoinHandle<()>>,
    stop: mpsc::Sender<()>,
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
    workgroup: Option<faderframe_realtime::Workgroup>,
    routing: StreamRouting,
}

impl AudioStream for CpalStream {
    fn info(&self) -> StreamInfo {
        let status = self.monitor.status();
        let mut info = self.info.clone();
        info.buffer_size = status.buffer_size;
        // As measured from the callbacks' timestamps.
        info.input_latency = status.input_latency.unwrap_or(0);
        info.output_latency = status.output_latency.unwrap_or(0);
        info
    }

    fn status(&self) -> StreamStatus {
        self.monitor.status()
    }

    fn routing(&self) -> StreamRouting {
        self.routing.clone()
    }

    fn io_workgroup(&self) -> Option<faderframe_realtime::Workgroup> {
        self.workgroup.clone()
    }
}

impl Drop for CpalStream {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A multichannel interface whose default is stereo opens with as many
    /// channels as the session wants (cue mixes, hardware inserts), the
    /// fewest that give them, in f32 where it can.
    #[test]
    fn the_config_opened_has_the_channels_wanted() {
        let range = |ch, fmt| {
            cpal::SupportedStreamConfigRange::new(
                ch,
                44_100,
                96_000,
                SupportedBufferSize::Range { min: 32, max: 4096 },
                fmt,
            )
        };
        let ranges = [
            range(2, SampleFormat::I32),
            range(2, SampleFormat::F32),
            range(8, SampleFormat::I32),
            range(18, SampleFormat::F32),
        ];
        let pick = |rate, wanted| {
            pick_config(&ranges, rate, wanted).map(|c| (c.channels(), c.sample_format()))
        };
        assert_eq!(pick(48_000, 2), Some((2, SampleFormat::F32)));
        assert_eq!(pick(48_000, 6), Some((8, SampleFormat::I32)));
        assert_eq!(
            pick(48_000, 32),
            Some((18, SampleFormat::F32)),
            "the most there are"
        );
        assert_eq!(pick(192_000, 2), None, "not at that rate");
        assert_eq!(buffer_range(&ranges), Some((32, 4096)));
    }

    /// Callbacks varying in size give one nominal size, not one per
    /// callback: larger at once, smaller after a few seconds of smaller
    /// ones.
    #[test]
    fn the_nominal_block_size_follows_varying_callbacks_slowly() {
        let rate = 44_100;
        let mut n = Nominal::new(512);
        let mut changes = Vec::new();
        // Two sizes taking turns for 10 s.
        let mut frames = 0u64;
        let mut i = 0;
        while frames < 10 * u64::from(rate) {
            let size = if i % 2 == 0 { 441 } else { 448 };
            frames += u64::from(size);
            i += 1;
            if let Some(s) = n.see(size, rate) {
                changes.push(s);
            }
        }
        assert_eq!(changes, vec![448], "one change, to the largest");
        // A larger callback goes in at once.
        assert_eq!(n.see(1024, rate), Some(1024));
        assert_eq!(n.see(448, rate), None);
    }
}
