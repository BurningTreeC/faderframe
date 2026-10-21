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
//! Devices are the clients that own physical ports (JACK2's `system`; under
//! pipewire-jack one per interface). With auto-connect the stream's ports
//! are connected to the devices asked for (their channels one after
//! another) or the first one, and [`AudioStream::maintain`] connects them
//! again when a device comes back.
//!
//! No JACK type escapes this crate: the engine only sees
//! [`faderframe_audio::DeviceBuffers`].

#![cfg(target_os = "linux")]

mod plan;

use faderframe_audio::{
    AudioBackend, AudioCallback, AudioError, AudioStream, Capabilities, DeviceBuffers, DeviceInfo,
    RateControl, StreamConfig, StreamInfo, StreamMonitor, StreamRouting, StreamStatus,
    validate_format,
};
use jack::{
    AsyncClient, AudioIn, AudioOut, Client, ClientOptions, ClientStatus, Control, Frames,
    LatencyType, NotificationHandler, Port, PortFlags, ProcessHandler, ProcessScope,
};
use std::collections::{HashMap, HashSet};
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

/// The devices: physical *output* ports are capture sources, *input*
/// ports are playback sinks (JACK's port direction is from the graph's
/// view).
fn devices_of(client: &Client) -> Vec<plan::Group> {
    plan::groups(
        &physical_ports(client, PortFlags::IS_OUTPUT),
        &physical_ports(client, PortFlags::IS_INPUT),
    )
}

impl AudioBackend for JackBackend {
    fn id(&self) -> &'static str {
        "jack"
    }

    fn display_name(&self) -> &'static str {
        "JACK (JACK2 / PipeWire)"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            rate: RateControl::Server,
            live_buffer_size: true,
            live_sample_rate: false,
            output_devices: 4,
            input_devices: 4,
            // One server clock (JACK2 drives one device; pipewire-jack's
            // devices are kept in step by PipeWire).
            clocked_by_server: true,
            follows_devices: true,
            free_channel_counts: true,
            force_format: false,
        }
    }

    fn is_available(&self) -> bool {
        open_client("faderframe-probe").is_ok()
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        let client = open_client("faderframe-probe")?;
        let groups = devices_of(&client);
        let first_capture = groups.iter().position(|g| !g.capture.is_empty());
        let first_playback = groups.iter().position(|g| !g.playback.is_empty());
        Ok(groups
            .iter()
            .enumerate()
            .map(|(i, g)| DeviceInfo {
                id: g.client.clone(),
                name: g.client.clone(),
                input_channels: g.capture.len() as u16,
                output_channels: g.playback.len() as u16,
                input_names: g.capture.iter().map(|p| plan::short(p).into()).collect(),
                output_names: g.playback.iter().map(|p| plan::short(p).into()).collect(),
                sample_rates: Vec::new(),
                current_sample_rate: Some(client.sample_rate()),
                current_buffer_size: Some(client.buffer_size()),
                buffer_sizes: None,
                clock: Some("jack".into()),
                is_default: Some(i) == first_capture || Some(i) == first_playback,
            })
            .collect())
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

        let mut stream = JackStream {
            client: active,
            info,
            monitor,
            in_names,
            out_names,
            wanted_in: config.input_devices.clone(),
            wanted_out: config.output_devices.clone(),
            auto_connect: config.auto_connect,
            made: HashSet::new(),
            known: HashMap::new(),
            missing: Vec::new(),
            latencies: (0, 0),
            devices: None,
        };
        stream.maintain();
        tracing::info!("JACK stream open: {}", stream.info);
        Ok(Box::new(stream))
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
    /// The stream's own ports (full names).
    in_names: Vec<String>,
    out_names: Vec<String>,
    /// Devices (clients) asked for.
    wanted_in: Vec<String>,
    wanted_out: Vec<String>,
    auto_connect: bool,
    /// Connections this stream made: (source, destination).
    made: HashSet<(String, String)>,
    /// Channel counts of devices seen (by client and direction: playback
    /// true), so a missing one keeps its place.
    known: HashMap<(String, bool), usize>,
    /// Devices asked for that are not there.
    missing: Vec<String>,
    /// As the server reported them at the last `maintain` (latencies in
    /// and out; the devices connected): `info` is asked often.
    latencies: (u32, u32),
    devices: Option<String>,
}

impl JackStream {
    /// The largest latency JACK reports between our ports of one
    /// direction and the devices.
    fn latency(&self, output: bool) -> u32 {
        let c = self.client.as_client();
        let (names, mode) = if output {
            (&self.out_names, LatencyType::Playback)
        } else {
            (&self.in_names, LatencyType::Capture)
        };
        names
            .iter()
            .filter_map(|n| c.port_by_name(n))
            .map(|p| p.get_latency_range(mode).1)
            .max()
            .unwrap_or(0)
    }

    /// Connect the wanted devices' ports (again), and undo connections of
    /// ours that are not wanted any more (a stand-in device's, once the
    /// wanted one is back).
    fn connect(&mut self) {
        if !self.auto_connect {
            return;
        }
        let c = self.client.as_client();
        let groups = devices_of(c);
        for g in &groups {
            for (len, playback) in [(g.playback.len(), true), (g.capture.len(), false)] {
                if len > 0 {
                    self.known.insert((g.client.clone(), playback), len);
                }
            }
        }
        let known = |playback: bool| -> HashMap<String, usize> {
            self.known
                .iter()
                .filter(|((_, p), _)| *p == playback)
                .map(|((c, _), n)| (c.clone(), *n))
                .collect()
        };
        let outs = plan::assign(
            &groups,
            &self.wanted_out,
            true,
            self.out_names.len(),
            &known(true),
        );
        let ins = plan::assign(
            &groups,
            &self.wanted_in,
            false,
            self.in_names.len(),
            &known(false),
        );
        let mut wanted: HashSet<(String, String)> = HashSet::new();
        for (ours, theirs) in self.out_names.iter().zip(&outs.ports) {
            if let Some(t) = theirs {
                wanted.insert((ours.clone(), t.clone()));
            }
        }
        for (ours, theirs) in self.in_names.iter().zip(&ins.ports) {
            if let Some(t) = theirs {
                wanted.insert((t.clone(), ours.clone()));
            }
        }
        for (src, dst) in self.made.difference(&wanted) {
            let _ = c.disconnect_ports_by_name(src, dst);
        }
        for (src, dst) in &wanted {
            let connected = c
                .port_by_name(src)
                .is_some_and(|p| p.is_connected_to(dst).unwrap_or(false));
            if !connected && let Err(e) = c.connect_ports_by_name(src, dst) {
                tracing::warn!("cannot connect {src} → {dst}: {e}");
            }
        }
        self.made = wanted;
        let mut missing = outs.missing;
        missing.extend(ins.missing);
        missing.dedup();
        self.missing = missing;
    }
}

impl AudioStream for JackStream {
    fn info(&self) -> StreamInfo {
        let status = self.monitor.status();
        let mut info = self.info.clone();
        info.sample_rate = status.sample_rate;
        info.buffer_size = status.buffer_size;
        // As the server reported it lately (connections change it).
        (info.input_latency, info.output_latency) = self.latencies;
        if let Some(d) = &self.devices {
            info.device = d.clone();
        }
        info
    }

    fn routing(&self) -> StreamRouting {
        let c = self.client.as_client();
        let mut r = StreamRouting {
            missing: self.missing.clone(),
            ..StreamRouting::default()
        };
        for (names, list) in [
            (&self.in_names, &mut r.inputs),
            (&self.out_names, &mut r.outputs),
        ] {
            for n in names {
                let peer = c
                    .port_by_name(n)
                    .and_then(|p| p.get_connections().into_iter().next());
                list.push(peer.map(|peer| {
                    let client = peer.split_once(':').map_or(peer.as_str(), |(c, _)| c);
                    if !r.devices.iter().any(|d| d == client) {
                        r.devices.push(client.to_string());
                    }
                    format!("{client} · {}", plan::short(&peer))
                }));
            }
        }
        r
    }

    /// Connect the wanted devices (again) and note the latencies and
    /// devices `info` reports. Never from a JACK callback.
    fn maintain(&mut self) {
        self.connect();
        self.latencies = (self.latency(false), self.latency(true));
        let devices = self.routing().devices;
        self.devices = (!devices.is_empty()).then(|| devices.join(" + "));
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
