//! JACK backend.
//!
//! Works with JACK2 and with PipeWire's JACK implementation
//! (`pipewire-jack`), which makes it the primary low-latency backend on
//! modern Linux desktops. libjack is loaded at runtime (`jack` crate's
//! `dynamic_loading`), so FaderFrame still starts when it is not installed.
//!
//! JACK owns the sample rate and the buffer size. The engine adapts to
//! whatever the server runs at; a buffer-size change can be *requested*
//! (`jack_set_buffer_size`), which affects the whole JACK graph.
//!
//! No JACK type escapes this crate: the engine only sees
//! [`faderframe_audio::DeviceBuffers`].

#![cfg(target_os = "linux")]

use faderframe_audio::{
    AudioBackend, AudioCallback, AudioError, AudioStream, DeviceBuffers, DeviceInfo, StreamConfig,
    StreamInfo, StreamMonitor, StreamStatus, validate_format,
};
use jack::{
    AsyncClient, AudioIn, AudioOut, Client, ClientOptions, ClientStatus, Control, Frames,
    LatencyType, NotificationHandler, Port, PortFlags, ProcessHandler, ProcessScope,
};
use std::sync::Arc;

const AUDIO_TYPE: &str = "32 bit float mono audio";

#[derive(Debug, Default)]
pub struct JackBackend;

fn open_client(name: &str) -> Result<Client, AudioError> {
    match Client::new(name, ClientOptions::NO_START_SERVER) {
        Ok((client, _status)) => Ok(client),
        Err(e) => Err(AudioError::BackendUnavailable(format!(
            "cannot connect to a JACK server ({e}); is JACK or pipewire-jack running?"
        ))),
    }
}

fn physical_ports(client: &Client, flags: PortFlags) -> Vec<String> {
    client.ports(None, Some(AUDIO_TYPE), flags | PortFlags::IS_PHYSICAL)
}

impl AudioBackend for JackBackend {
    fn id(&self) -> &'static str {
        "jack"
    }

    fn display_name(&self) -> &'static str {
        "JACK (JACK2 / PipeWire)"
    }

    fn is_available(&self) -> bool {
        open_client("faderframe-probe").is_ok()
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        let client = open_client("faderframe-probe")?;
        // Physical *output* ports are capture sources, *input* ports are
        // playback sinks (JACK's port direction is from the graph's view).
        let capture = physical_ports(&client, PortFlags::IS_OUTPUT).len() as u16;
        let playback = physical_ports(&client, PortFlags::IS_INPUT).len() as u16;
        Ok(vec![DeviceInfo {
            id: "jack".into(),
            name: "JACK server".into(),
            input_channels: capture,
            output_channels: playback,
            sample_rates: Vec::new(),
            current_sample_rate: Some(client.sample_rate()),
            current_buffer_size: Some(client.buffer_size()),
            is_default: true,
        }])
    }

    fn open_stream(
        &mut self,
        config: StreamConfig,
        mut callback: Box<dyn AudioCallback>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let client = open_client(&config.client_name)?;
        if let Some(frames) = config.buffer_size
            && frames != client.buffer_size()
        {
            validate_format(client.sample_rate(), frames)?;
            if let Err(e) = client.set_buffer_size(frames) {
                tracing::warn!("JACK refused buffer size {frames}: {e}");
            }
        }
        let sample_rate = client.sample_rate();
        if let Some(wanted) = config.sample_rate
            && wanted != sample_rate
        {
            tracing::warn!(
                "JACK server runs at {sample_rate} Hz; requested {wanted} Hz is ignored (the server owns the rate)"
            );
        }
        let buffer_size = client.buffer_size();
        validate_format(sample_rate, buffer_size)?;

        let mut inputs = Vec::with_capacity(config.input_channels as usize);
        for i in 0..config.input_channels {
            inputs.push(
                client
                    .register_port(&format!("in_{}", i + 1), AudioIn::default())
                    .map_err(|e| AudioError::Stream(format!("register input port: {e}")))?,
            );
        }
        let mut outputs = Vec::with_capacity(config.output_channels as usize);
        for i in 0..config.output_channels {
            outputs.push(
                client
                    .register_port(&format!("out_{}", i + 1), AudioOut::default())
                    .map_err(|e| AudioError::Stream(format!("register output port: {e}")))?,
            );
        }
        let in_names: Vec<String> = inputs.iter().filter_map(|p| p.name().ok()).collect();
        let out_names: Vec<String> = outputs.iter().filter_map(|p| p.name().ok()).collect();
        let output_latency = outputs
            .first()
            .map_or(0, |p| p.get_latency_range(LatencyType::Playback).1);
        let input_latency = inputs
            .first()
            .map_or(0, |p| p.get_latency_range(LatencyType::Capture).1);

        let info = StreamInfo {
            backend: "jack",
            device: client.name().to_string(),
            sample_rate,
            buffer_size,
            input_channels: config.input_channels,
            output_channels: config.output_channels,
            input_latency,
            output_latency,
        };
        let monitor = StreamMonitor::new(sample_rate, buffer_size);
        callback.prepare(&info);

        let process = JackProcess {
            callback,
            inputs,
            outputs,
            monitor: Arc::clone(&monitor),
            info: info.clone(),
        };
        let notifications = JackNotifications {
            monitor: Arc::clone(&monitor),
        };
        let active = client
            .activate_async(notifications, process)
            .map_err(|e| AudioError::Stream(format!("activate JACK client: {e}")))?;
        monitor.set_running(true);

        if config.auto_connect {
            let c = active.as_client();
            for (ours, theirs) in out_names.iter().zip(physical_ports(c, PortFlags::IS_INPUT)) {
                if let Err(e) = c.connect_ports_by_name(ours, &theirs) {
                    tracing::warn!("cannot connect {ours} → {theirs}: {e}");
                }
            }
            for (theirs, ours) in physical_ports(c, PortFlags::IS_OUTPUT)
                .into_iter()
                .zip(in_names.iter())
            {
                if let Err(e) = c.connect_ports_by_name(&theirs, ours) {
                    tracing::warn!("cannot connect {theirs} → {ours}: {e}");
                }
            }
        }
        tracing::info!("JACK stream open: {info}");
        Ok(Box::new(JackStream {
            client: active,
            info,
            monitor,
        }))
    }
}

/// Realtime side: owned by JACK's process thread.
struct JackProcess {
    callback: Box<dyn AudioCallback>,
    inputs: Vec<Port<AudioIn>>,
    outputs: Vec<Port<AudioOut>>,
    monitor: Arc<StreamMonitor>,
    info: StreamInfo,
}

struct JackIo<'a> {
    ps: &'a ProcessScope,
    inputs: &'a [Port<AudioIn>],
    outputs: &'a mut [Port<AudioOut>],
}

impl DeviceBuffers for JackIo<'_> {
    fn frames(&self) -> usize {
        self.ps.n_frames() as usize
    }

    fn input_channels(&self) -> usize {
        self.inputs.len()
    }

    fn output_channels(&self) -> usize {
        self.outputs.len()
    }

    fn input(&self, channel: usize) -> &[f32] {
        self.inputs[channel].as_slice(self.ps)
    }

    fn output(&mut self, channel: usize) -> &mut [f32] {
        self.outputs[channel].as_mut_slice(self.ps)
    }
}

impl ProcessHandler for JackProcess {
    fn process(&mut self, _client: &Client, ps: &ProcessScope) -> Control {
        let mut io = JackIo {
            ps,
            inputs: &self.inputs,
            outputs: &mut self.outputs,
        };
        self.callback.process(&mut io);
        self.monitor.record_callback();
        Control::Continue
    }

    fn buffer_size(&mut self, _client: &Client, size: Frames) -> Control {
        self.info.buffer_size = size;
        self.monitor.set_buffer_size(size);
        self.callback.prepare(&self.info);
        Control::Continue
    }
}

/// Notification side: lock-free updates of the shared monitor only.
struct JackNotifications {
    monitor: Arc<StreamMonitor>,
}

impl NotificationHandler for JackNotifications {
    fn xrun(&mut self, _client: &Client) -> Control {
        self.monitor.record_xrun();
        Control::Continue
    }

    fn sample_rate(&mut self, _client: &Client, srate: Frames) -> Control {
        // The control side notices the change through `status()` and
        // rebuilds the engine graph for the new rate.
        self.monitor.set_sample_rate(srate);
        Control::Continue
    }

    unsafe fn shutdown(&mut self, _status: ClientStatus, _reason: &str) {
        self.monitor.mark_shut_down();
    }
}

struct JackStream {
    client: AsyncClient<JackNotifications, JackProcess>,
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
}

impl AudioStream for JackStream {
    fn info(&self) -> StreamInfo {
        let status = self.monitor.status();
        let mut info = self.info.clone();
        info.sample_rate = status.sample_rate;
        info.buffer_size = status.buffer_size;
        info
    }

    fn status(&self) -> StreamStatus {
        self.monitor.status()
    }

    fn request_buffer_size(&mut self, frames: u32) -> Result<(), AudioError> {
        validate_format(self.monitor.status().sample_rate, frames)?;
        self.client
            .as_client()
            .set_buffer_size(frames)
            .map_err(|e| AudioError::Stream(format!("JACK refused buffer size {frames}: {e}")))
    }
}
