//! The operating system's own audio API through cpal: WASAPI on Windows,
//! CoreAudio on macOS, ALSA on Linux.
//!
//! cpal opens playback and capture as separate streams. The playback
//! callback drives the engine; captured frames reach it through a lock-free
//! ring (so recording works, with the ring's few milliseconds of extra input
//! latency). Devices that do not take `f32` get converted `i16`/`i32`
//! samples. The streams live on a control thread that owns them, so the
//! returned stream is `Send` on every platform.
//!
//! No cpal type escapes this crate: the engine only sees
//! [`faderframe_audio::DeviceBuffers`].

#![forbid(unsafe_code)]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, SupportedBufferSize};
use faderframe_audio::{
    AudioBackend, AudioCallback, AudioError, AudioStream, DeviceBuffers, DeviceInfo,
    MAX_BUFFER_SIZE, STANDARD_SAMPLE_RATES, StreamConfig, StreamInfo, StreamMonitor, StreamStatus,
};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Seconds of capture the input ring holds.
const INPUT_RING_SECONDS: f32 = 0.5;

#[derive(Debug, Default)]
pub struct CpalBackend;

/// The platform's API name.
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

impl AudioBackend for CpalBackend {
    fn id(&self) -> &'static str {
        "system"
    }

    fn display_name(&self) -> &'static str {
        api_name()
    }

    fn is_available(&self) -> bool {
        cpal::default_host().default_output_device().is_some()
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        let host = cpal::default_host();
        let default = host.default_output_device().map(|d| device_id(&d));
        let devices = host
            .output_devices()
            .map_err(|e| AudioError::BackendUnavailable(e.to_string()))?;
        Ok(devices
            .map(|d| {
                let outs: Vec<_> = d.supported_output_configs().into_iter().flatten().collect();
                let ins: Vec<_> = d.supported_input_configs().into_iter().flatten().collect();
                let out_cfg = d.default_output_config().ok();
                let id = device_id(&d);
                DeviceInfo {
                    is_default: default.as_deref() == Some(id.as_str()),
                    id,
                    name: device_name(&d),
                    input_channels: ins.iter().map(|c| c.channels()).max().unwrap_or(0),
                    output_channels: outs.iter().map(|c| c.channels()).max().unwrap_or(0),
                    sample_rates: rates(&outs),
                    current_sample_rate: out_cfg.as_ref().map(|c| c.sample_rate()),
                    current_buffer_size: None,
                }
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
        let thread = std::thread::Builder::new()
            .name("audio-control".into())
            .spawn(move || match open(&config, callback) {
                Ok((streams, info, monitor)) => {
                    let _ = ready_tx.send(Ok((info, monitor)));
                    // Keep the streams until asked to stop.
                    let _ = stop_rx.recv();
                    drop(streams);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(stream_err)?;
        let (info, monitor) = match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => return Err(AudioError::Stream("the device did not open in time".into())),
        };
        tracing::info!("{} stream open: {info}", api_name());
        Ok(Box::new(CpalStream {
            thread: Some(thread),
            stop: stop_tx,
            info,
            monitor,
        }))
    }
}

/// Open playback (and capture) on the calling thread.
fn open(
    config: &StreamConfig,
    callback: Box<dyn AudioCallback>,
) -> Result<(Vec<cpal::Stream>, StreamInfo, Arc<StreamMonitor>), AudioError> {
    let host = cpal::default_host();
    let device = match &config.device {
        Some(id) => host
            .output_devices()
            .map_err(stream_err)?
            .find(|d| device_id(d) == *id || device_name(d) == *id)
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
    // Prefer f32 at the wanted rate, with the device's channel count.
    let pick = |fmt: SampleFormat| {
        ranges.iter().find(|c| {
            c.sample_format() == fmt
                && c.channels() == default.channels()
                && (c.min_sample_rate()..=c.max_sample_rate()).contains(&wanted_rate)
        })
    };
    let range = [SampleFormat::F32, SampleFormat::I32, SampleFormat::I16]
        .into_iter()
        .find_map(pick)
        .ok_or_else(|| {
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

    // Capture: the default input device at the same rate, if any.
    let mut streams = Vec::new();
    let mut input_channels = 0u16;
    let mut input_rx = None;
    if config.input_channels > 0
        && let Some(input) = host.default_input_device()
        && let Ok(in_default) = input.default_input_config()
    {
        let in_config = cpal::StreamConfig {
            channels: in_default.channels(),
            sample_rate: wanted_rate,
            buffer_size: cpal::BufferSize::Default,
        };
        let cap = (wanted_rate as f32 * INPUT_RING_SECONDS) as usize * in_config.channels as usize;
        let (tx, rx) = rtrb::RingBuffer::new(cap.max(1024));
        let m = Arc::clone(&monitor);
        match build_input(&input, &in_config, in_default.sample_format(), tx, m) {
            Ok(s) => {
                input_channels = in_config.channels;
                input_rx = Some(rx);
                streams.push(s);
            }
            Err(e) => tracing::warn!("{}: no capture ({e})", api_name()),
        }
    }

    let info = StreamInfo {
        backend: "system",
        device: device_name(&device),
        sample_rate: wanted_rate,
        buffer_size: nominal,
        input_channels,
        output_channels: channels,
        input_latency: 0,
        output_latency: 0,
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
    Ok((streams, info, monitor))
}

fn on_error(monitor: &StreamMonitor, e: cpal::Error) {
    match e.kind() {
        cpal::ErrorKind::Xrun => monitor.record_xrun(),
        cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied => {}
        _ => monitor.mark_shut_down(),
    }
}

fn build_input(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: SampleFormat,
    mut tx: rtrb::Producer<f32>,
    monitor: Arc<StreamMonitor>,
) -> Result<cpal::Stream, AudioError> {
    let m = Arc::clone(&monitor);
    let err = move |e| on_error(&m, e);
    let stream = match format {
        SampleFormat::F32 => device.build_input_stream::<f32, _, _>(
            *config,
            move |data, _| {
                for v in data {
                    let _ = tx.push(*v);
                }
            },
            err,
            None,
        ),
        SampleFormat::I32 => device.build_input_stream::<i32, _, _>(
            *config,
            move |data, _| {
                for v in data {
                    let _ = tx.push(f32::from_sample(*v));
                }
            },
            err,
            None,
        ),
        _ => device.build_input_stream::<i16, _, _>(
            *config,
            move |data, _| {
                for v in data {
                    let _ = tx.push(f32::from_sample(*v));
                }
            },
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
    /// One chunk of at most `MAX_BUFFER_SIZE` frames into `out`
    /// (interleaved).
    fn run<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T]) {
        let ch = self.outputs.len().max(1);
        let n = out.len() / ch;
        if n as u32 != self.info.buffer_size {
            self.info.buffer_size = n as u32;
            self.monitor.set_buffer_size(n as u32);
            self.callback.prepare(&self.info);
        }
        // Captured frames, or silence when the ring runs dry.
        let in_ch = self.inputs.len();
        for i in 0..n {
            let got = match &mut self.input_rx {
                Some(rx) if rx.slots() >= in_ch => {
                    for v in self.frame.iter_mut() {
                        *v = rx.pop().unwrap_or(0.0);
                    }
                    true
                }
                _ => false,
            };
            for (c, buf) in self.inputs.iter_mut().enumerate() {
                buf[i] = if got { self.frame[c] } else { 0.0 };
            }
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
            move |data: &mut [T], _| {
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
}

impl AudioStream for CpalStream {
    fn info(&self) -> StreamInfo {
        let status = self.monitor.status();
        let mut info = self.info.clone();
        info.buffer_size = status.buffer_size;
        info
    }

    fn status(&self) -> StreamStatus {
        self.monitor.status()
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
