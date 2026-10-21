//! The audio device: what the user asked for ([`AudioPreferences`]),
//! checked against what the backend reports before it opens, changes
//! applied to the running stream where the backend can take them (a
//! buffer size, PipeWire's rate) and by reopening it otherwise — never
//! while recording — and recovery when the device goes away: the same
//! device again as soon as it is back, the default one after
//! [`FALLBACK_AFTER`] with a notice, and back to the chosen one when it
//! returns (once playback stops).
//!
//! Probing for devices runs on a thread of its own (enumerating a system
//! API's devices can take a while); nothing here touches the audio
//! thread: streams are only opened and closed through the backends.

use crate::{AudioPreferences, NoticeLevel, Result, Session, SessionError};
use faderframe_audio::{
    AudioBackend, AudioError, Capabilities, DeviceInfo, RateControl, Request, StreamConfig,
    StreamInfo, StreamRouting, find_device,
};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Makes the backends a session opens (the shell's choice — PipeWire,
/// JACK, the system API, … — or pretend devices in tests). Called again to
/// reopen after a device went away, also on a probing thread.
pub type BackendFactory = Arc<dyn Fn() -> Vec<Box<dyn AudioBackend>> + Send + Sync>;

/// How often a device that went away is looked for.
pub const PROBE_EVERY: Duration = Duration::from_millis(750);
/// How long the session waits for a device that went away before it plays
/// on the default one.
pub const FALLBACK_AFTER: Duration = Duration::from_secs(2);
/// How often a running stream may reconnect its channels (JACK).
const MAINTAIN_EVERY: Duration = Duration::from_secs(1);

/// How a change of the audio settings was applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// Nothing to change.
    Unchanged,
    /// Passed to the running stream (playback goes on).
    Live,
    /// The device was reopened (playback stopped; the playhead stays).
    Restarted,
}

/// Where the audio device stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioHealth {
    /// No stream (stopped, or never started).
    Stopped,
    Running,
    /// Running on the default device: the chosen one is not connected.
    Fallback {
        missing: Vec<String>,
    },
    /// The stream went away; the device is looked for again.
    Reconnecting {
        since: Instant,
        device: String,
    },
}

/// The audio device as Preferences shows it.
#[derive(Clone, Debug)]
pub struct AudioDeviceStatus {
    pub health: AudioHealth,
    /// The backend in use (or last used), by id.
    pub backend: Option<&'static str>,
    pub capabilities: Option<Capabilities>,
    /// As asked for (Preferences), not necessarily as running.
    pub requested: AudioPreferences,
    /// What was changed when the stream opened (a rate the device does not
    /// take, a device not connected, …).
    pub notes: Vec<String>,
    /// How the stream's channels are connected.
    pub routing: StreamRouting,
    /// The devices the backend reported when the stream opened.
    pub devices: Vec<DeviceInfo>,
}

/// A probe's answer: is the backend there, and its devices.
struct Probe {
    available: bool,
    devices: Option<Vec<DeviceInfo>>,
}

struct Lost {
    since: Instant,
    device: String,
}

/// The session's side of the audio device (see the module docs).
#[derive(Default)]
pub(crate) struct AudioDevice {
    factory: Option<BackendFactory>,
    /// As asked for.
    prefs: AudioPreferences,
    backend: Option<&'static str>,
    caps: Option<Capabilities>,
    notes: Vec<String>,
    devices: Vec<DeviceInfo>,
    lost: Option<Lost>,
    /// Asked-for devices missing when the stream opened (it plays on the
    /// default meanwhile).
    missing: Vec<String>,
    /// "It is back" said (while playback holds the switch back).
    told_back: bool,
    probe: Option<mpsc::Receiver<Probe>>,
    next_probe: Option<Instant>,
    next_maintain: Option<Instant>,
}

impl std::fmt::Debug for AudioDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioDevice")
            .field("prefs", &self.prefs)
            .field("backend", &self.backend)
            .field("missing", &self.missing)
            .finish_non_exhaustive()
    }
}

impl AudioDevice {
    /// The user stopped audio: a device that went away is not reopened.
    pub(crate) fn stop_watching(&mut self) {
        self.lost = None;
        self.missing.clear();
        self.probe = None;
    }

    pub(crate) fn asked_buffer_size(&mut self, frames: u32) {
        self.prefs.buffer_size = Some(frames);
    }
}

/// Look for `backend`'s devices on a thread of its own.
fn spawn_probe(factory: &BackendFactory, backend: &'static str) -> Option<mpsc::Receiver<Probe>> {
    let (tx, rx) = mpsc::channel();
    let factory = Arc::clone(factory);
    std::thread::Builder::new()
        .name("audio-probe".into())
        .spawn(move || {
            let probe = factory().into_iter().find(|b| b.id() == backend).map_or(
                Probe {
                    available: false,
                    devices: None,
                },
                |b| {
                    let available = b.is_available();
                    Probe {
                        available,
                        devices: available.then(|| b.enumerate_devices().ok()).flatten(),
                    }
                },
            );
            let _ = tx.send(probe);
        })
        .ok()?;
    Some(rx)
}

/// The channels of the devices a request plays on (or records from), as
/// the device list says; `None` when not known.
fn device_channels(devices: &[DeviceInfo], chosen: &[String], output: bool) -> Option<u16> {
    let width = |d: &DeviceInfo| {
        if output {
            d.output_channels
        } else {
            d.input_channels
        }
    };
    if chosen.is_empty() {
        return None;
    }
    let mut total = 0u16;
    for id in chosen {
        total = total.saturating_add(width(find_device(devices, id)?));
    }
    Some(total)
}

/// The default device of a direction in a list: the output device itself
/// records where it can.
fn default_device<'a>(
    devices: &'a [DeviceInfo],
    outputs: &[String],
    output: bool,
) -> Option<&'a DeviceInfo> {
    let out = outputs
        .first()
        .and_then(|id| find_device(devices, id))
        .or_else(|| {
            devices
                .iter()
                .find(|d| d.is_default && d.output_channels > 0)
        });
    if output {
        return out;
    }
    out.filter(|d| d.input_channels > 0).or_else(|| {
        devices
            .iter()
            .find(|d| d.is_default && d.input_channels > 0)
    })
}

impl Session {
    /// Open an audio stream on the first backend that works, with
    /// `prefs` checked against what it reports (see
    /// [`faderframe_audio::resolve`]); a device that refuses its format
    /// gets its own, a device not there the default (with a notice each).
    pub(crate) fn open_audio(
        &mut self,
        backends: Vec<Box<dyn AudioBackend>>,
        prefs: &AudioPreferences,
    ) -> Result<StreamInfo> {
        self.close_stream();
        self.device.prefs = prefs.clone();
        let mut last_err: Option<SessionError> = None;
        for mut backend in backends {
            if !backend.is_available() {
                last_err = Some(SessionError::Audio(AudioError::BackendUnavailable(
                    format!("{} is not available", backend.display_name()),
                )));
                continue;
            }
            let caps = backend.capabilities();
            let choice = prefs.devices_for(backend.id());
            let asks = !choice.is_empty()
                || prefs.sample_rate.is_some()
                || prefs.buffer_size.is_some()
                || caps.free_channel_counts;
            let devices = if asks {
                backend.enumerate_devices().unwrap_or_else(|e| {
                    tracing::warn!("{}: no device list ({e})", backend.display_name());
                    Vec::new()
                })
            } else {
                Vec::new()
            };
            let request = Request {
                sample_rate: prefs.sample_rate,
                buffer_size: prefs.buffer_size,
                output_devices: choice.outputs.clone(),
                input_devices: choice.inputs.clone(),
            };
            let resolved = faderframe_audio::resolve(&caps, &devices, &request);
            // What to try, in turn: as resolved; the device's own format
            // (a device refusing what it said it takes); the defaults (a
            // device gone between the list and the open).
            let mut attempts = vec![resolved.request.clone()];
            if resolved.request.sample_rate.is_some() || resolved.request.buffer_size.is_some() {
                attempts.push(Request {
                    sample_rate: None,
                    buffer_size: None,
                    ..resolved.request.clone()
                });
            }
            if !resolved.request.output_devices.is_empty()
                || !resolved.request.input_devices.is_empty()
            {
                attempts.push(Request::default());
            }
            let mut refused: Vec<String> = Vec::new();
            for attempt in &attempts {
                if self.pending.is_none() {
                    self.recreate_engine()?;
                }
                let Some(mut processor) = self.pending.take() else {
                    continue;
                };
                processor.set_worker_pool(self.worker_pool(prefs.threads));
                let (inputs, outputs) = self.stream_channels(&caps, &devices, attempt, prefs);
                let config = StreamConfig {
                    output_devices: attempt.output_devices.clone(),
                    input_devices: attempt.input_devices.clone(),
                    sample_rate: attempt.sample_rate,
                    buffer_size: attempt.buffer_size,
                    input_channels: inputs,
                    output_channels: outputs,
                    force_format: prefs.force_format && caps.force_format,
                    ..StreamConfig::default()
                };
                match backend.open_stream(config, Box::new(processor)) {
                    Ok(stream) => {
                        let mut notes = resolved.notes.clone();
                        notes.extend(refused);
                        let mut missing = resolved.missing.clone();
                        if attempt.output_devices.is_empty() && attempt.input_devices.is_empty() {
                            for id in choice.outputs.iter().chain(&choice.inputs) {
                                if !missing.contains(id) {
                                    missing.push(id.clone());
                                }
                            }
                        }
                        return self.opened(backend.id(), caps, stream, notes, missing, devices);
                    }
                    Err(e) => {
                        let retry = matches!(
                            e,
                            AudioError::UnsupportedConfig(_) | AudioError::DeviceNotFound(_)
                        );
                        refused.push(format!("{}: {e}", backend.display_name()));
                        last_err = Some(e.into());
                        if !retry {
                            break;
                        }
                    }
                }
            }
            self.notify(
                NoticeLevel::Warning,
                refused
                    .last()
                    .cloned()
                    .unwrap_or_else(|| format!("{} did not open", backend.display_name())),
            );
        }
        Err(last_err.unwrap_or_else(|| SessionError::Other("no audio backend available".into())))
    }

    /// A stream opened: keep it, say what changed on the way.
    fn opened(
        &mut self,
        backend: &'static str,
        caps: Capabilities,
        stream: Box<dyn faderframe_audio::AudioStream>,
        notes: Vec<String>,
        missing: Vec<String>,
        devices: Vec<DeviceInfo>,
    ) -> Result<StreamInfo> {
        // The audio server's client may have set the realtime time limit
        // again (PipeWire's does with every context: soft = hard, no
        // warning before the kill).
        faderframe_realtime::priority::note_rttime_limit();
        // The audio path's threads may ask RealtimeKit (through its broker).
        faderframe_realtime::priority::start_requests();
        let info = stream.info();
        // macOS: the workers join the device's audio workgroup, and so do
        // sandboxed plugins' audio threads.
        let workgroup = stream.io_workgroup();
        faderframe_realtime::set_process_workgroup(workgroup.clone());
        if let Some(pool) = &self.pool {
            pool.set_workgroup(workgroup);
        }
        self.audio = Some(crate::ActiveAudio {
            stream,
            backend,
            _wake: faderframe_realtime::priority::CpuWakeLatency::hold(),
        });
        let d = &mut self.device;
        d.backend = Some(backend);
        d.caps = Some(caps);
        d.devices = devices;
        d.lost = None;
        d.told_back = false;
        d.probe = None;
        d.next_probe = None;
        d.missing = missing;
        d.notes = notes.clone();
        self.set_engine_rate(info.sample_rate)?;
        for n in notes {
            self.notify(NoticeLevel::Warning, n);
        }
        self.notify(NoticeLevel::Info, format!("audio: {info}"));
        Ok(info)
    }

    /// The channels a stream opens (inputs, outputs): as chosen, or as
    /// many as its devices have (8 in and 2 out where that is not known),
    /// and never fewer than the project needs.
    fn stream_channels(
        &self,
        caps: &Capabilities,
        devices: &[DeviceInfo],
        request: &Request,
        prefs: &AudioPreferences,
    ) -> (u16, u16) {
        let (need_in, need_out) = self.required_channels();
        let known = |output: bool| {
            let chosen = if output {
                &request.output_devices
            } else {
                &request.input_devices
            };
            device_channels(devices, chosen, output).or_else(|| {
                default_device(devices, &request.output_devices, output).map(|d| {
                    if output {
                        d.output_channels
                    } else {
                        d.input_channels
                    }
                })
            })
        };
        let pick = |chosen: Option<u16>, output: bool, base: u16| {
            let auto = known(output).filter(|n| *n > 0 || caps.free_channel_counts);
            chosen.unwrap_or_else(|| auto.unwrap_or(base))
        };
        let ins = pick(prefs.input_channels, false, 8).max(need_in);
        let outs = pick(prefs.output_channels, true, 2).max(need_out).max(1);
        (ins.min(256), outs.min(256))
    }

    /// Start audio with backends `factory` makes, which the session also
    /// uses to reopen the device after it went away.
    pub fn start_audio_with(
        &mut self,
        factory: BackendFactory,
        prefs: &AudioPreferences,
    ) -> Result<StreamInfo> {
        self.device.factory = Some(Arc::clone(&factory));
        self.device.lost = None;
        self.open_audio(factory(), prefs)
    }

    /// What applying `prefs` would do: nothing, pass them to the running
    /// stream, or reopen the device; `Err` says why it cannot now.
    pub fn plan_audio(&self, prefs: &AudioPreferences) -> std::result::Result<Applied, String> {
        let running = self.audio.is_some() && self.device.lost.is_none();
        let settled = running && self.device.missing.is_empty();
        let old = &self.device.prefs;
        let backend = self.device.backend;
        // Everything but the format, as far as the running backend goes
        // (another backend's devices wait until it is chosen).
        let rest_same = prefs.threads == old.threads
            && prefs.input_channels == old.input_channels
            && prefs.output_channels == old.output_channels
            && prefs.force_format == old.force_format
            && match backend {
                Some(id) => prefs.devices_for(id) == old.devices_for(id),
                None => prefs.devices == old.devices,
            };
        let rate_same = prefs.sample_rate == old.sample_rate;
        let buffer_same = prefs.buffer_size == old.buffer_size;
        if settled && rest_same && rate_same && buffer_same {
            return Ok(Applied::Unchanged);
        }
        let live = match (&self.device.caps, settled && rest_same) {
            (Some(caps), true) => {
                let buffer_ok =
                    buffer_same || (caps.live_buffer_size && prefs.buffer_size.is_some());
                let rate_ok = rate_same
                    || match caps.rate {
                        // The server keeps its own: nothing to change.
                        RateControl::Server => true,
                        RateControl::Requested => {
                            caps.live_sample_rate && prefs.sample_rate.is_some()
                        }
                        RateControl::Device => false,
                    };
                buffer_ok && rate_ok
            }
            _ => false,
        };
        if live {
            return Ok(Applied::Live);
        }
        if self.recording.is_some() {
            return Err(
                "Stop recording first: reopening the audio device would end the take".into(),
            );
        }
        if self.device.factory.is_none() {
            return Err("no audio backends to open (start audio first)".into());
        }
        Ok(Applied::Restarted)
    }

    /// Apply new audio settings: what the running stream can take is
    /// passed to it (playback goes on), anything else reopens the device
    /// (playback stops). Refused while recording.
    pub fn apply_audio(&mut self, prefs: AudioPreferences) -> Result<Applied> {
        let plan = self.plan_audio(&prefs).map_err(SessionError::Other)?;
        let old = self.device.prefs.clone();
        match plan {
            Applied::Unchanged => {
                self.device.prefs = prefs;
            }
            Applied::Live => {
                let rate_requested = self
                    .device
                    .caps
                    .is_some_and(|c| c.rate == RateControl::Requested);
                if let Some(a) = &mut self.audio {
                    if prefs.buffer_size != old.buffer_size
                        && let Some(frames) = prefs.buffer_size
                    {
                        a.stream.request_buffer_size(frames)?;
                    }
                    if prefs.sample_rate != old.sample_rate
                        && rate_requested
                        && let Some(rate) = prefs.sample_rate
                    {
                        a.stream.request_sample_rate(rate)?;
                    }
                }
                self.device.prefs = prefs;
            }
            Applied::Restarted => {
                let Some(factory) = self.device.factory.clone() else {
                    return Err(SessionError::Other("no audio backends to open".into()));
                };
                if let Err(e) = self.open_audio(factory(), &prefs) {
                    // What ran before, rather than no audio at all.
                    let back = self.open_audio(factory(), &old).is_ok();
                    self.revision += 1;
                    return Err(if back {
                        SessionError::Other(format!("{e} — the previous audio settings are back"))
                    } else {
                        e
                    });
                }
            }
        }
        self.revision += 1;
        Ok(plan)
    }

    /// The audio settings as asked for (not necessarily as running: see
    /// [`Self::stream_info`]).
    pub fn audio_preferences(&self) -> &AudioPreferences {
        &self.device.prefs
    }

    /// Where the audio device stands, for Preferences.
    pub fn audio_status(&self) -> AudioDeviceStatus {
        let d = &self.device;
        let health = match (&d.lost, &self.audio) {
            (Some(l), _) => AudioHealth::Reconnecting {
                since: l.since,
                device: l.device.clone(),
            },
            (None, Some(_)) if !d.missing.is_empty() => AudioHealth::Fallback {
                missing: d.missing.clone(),
            },
            (None, Some(_)) => AudioHealth::Running,
            (None, None) => AudioHealth::Stopped,
        };
        AudioDeviceStatus {
            health,
            backend: d.backend,
            capabilities: d.caps,
            requested: d.prefs.clone(),
            notes: d.notes.clone(),
            routing: self.stream_routing(),
            devices: d.devices.clone(),
        }
    }

    /// How the running stream's channels are connected (empty without
    /// one).
    pub fn stream_routing(&self) -> StreamRouting {
        self.audio
            .as_ref()
            .map(|a| a.stream.routing())
            .unwrap_or_default()
    }

    /// The device channel input `channel` of the stream comes from, if the
    /// backend names it ("SSL 2+ · capture_AUX0").
    pub fn input_channel_name(&self, channel: u16) -> Option<String> {
        self.stream_routing()
            .input(usize::from(channel))
            .map(str::to_string)
    }

    /// The device channel output `channel` goes to, if the backend names
    /// it.
    pub fn output_channel_name(&self, channel: u16) -> Option<String> {
        self.stream_routing()
            .output(usize::from(channel))
            .map(str::to_string)
    }

    /// Close the stream, keeping what recovery needs.
    pub(crate) fn close_stream(&mut self) {
        self.stop_voice();
        self.audio = None;
        faderframe_realtime::set_process_workgroup(None);
        if let Some(pool) = &self.pool {
            pool.set_workgroup(None);
        }
    }

    /// Each frame: notice a stream that went away and bring it back, move
    /// back to the chosen device when it returns, let the stream reconnect
    /// its channels.
    pub(crate) fn tick_audio_device(&mut self) {
        let now = Instant::now();
        if let Some(status) = self.stream_status() {
            if status.shut_down {
                let device = self.stream_info().map(|i| i.device).unwrap_or_default();
                self.close_stream();
                if let Err(e) = self.recreate_engine() {
                    self.notify(NoticeLevel::Error, e.to_string());
                }
                let text = if self.device.factory.is_some() {
                    format!(
                        "The audio device stopped ({device}): unplugged, or its server went \
                         away. FaderFrame opens it again as soon as it is back"
                    )
                } else {
                    "the audio server shut down the stream".to_string()
                };
                self.notify(NoticeLevel::Warning, text);
                self.device.lost = Some(Lost { since: now, device });
                self.device.next_probe = Some(now);
                self.device.probe = None;
                self.revision += 1;
            } else if status.sample_rate != 0
                && status.sample_rate != self.engine.sample_rate()
                && let Err(e) = self.set_engine_rate(status.sample_rate)
            {
                self.notify(NoticeLevel::Error, e.to_string());
            }
        }
        if self.device.next_maintain.is_none_or(|t| now >= t) {
            self.device.next_maintain = Some(now + MAINTAIN_EVERY);
            if let Some(a) = &mut self.audio {
                a.stream.maintain();
            }
        }
        self.poll_probe(now);
    }

    fn poll_probe(&mut self, now: Instant) {
        let watching = self.device.lost.is_some() || !self.device.missing.is_empty();
        let (Some(factory), Some(backend)) = (self.device.factory.clone(), self.device.backend)
        else {
            return;
        };
        if !watching {
            self.device.probe = None;
            return;
        }
        if let Some(rx) = &self.device.probe {
            match rx.try_recv() {
                Ok(p) => {
                    self.device.probe = None;
                    self.on_probe(p, now, &factory, backend);
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => self.device.probe = None,
            }
        }
        if self.device.probe.is_none() && self.device.next_probe.is_none_or(|t| now >= t) {
            self.device.next_probe = Some(now + PROBE_EVERY);
            self.device.probe = spawn_probe(&factory, backend);
        }
    }

    fn on_probe(&mut self, p: Probe, now: Instant, factory: &BackendFactory, backend: &str) {
        if !p.available {
            return;
        }
        let prefs = self.device.prefs.clone();
        let choice = prefs.devices_for(backend);
        let wanted: Vec<&String> = choice.outputs.iter().chain(&choice.inputs).collect();
        let back = p
            .devices
            .as_deref()
            .is_some_and(|d| wanted.iter().all(|id| find_device(d, id).is_some()));
        let reopen = |s: &mut Session| -> Result<StreamInfo> {
            let backends = factory()
                .into_iter()
                .filter(|b| b.id() == backend)
                .collect();
            s.open_audio(backends, &prefs)
        };
        if let Some(lost) = &self.device.lost {
            let waited = now.duration_since(lost.since);
            if !back && waited < FALLBACK_AFTER {
                return;
            }
            if self.recording.is_some() {
                return;
            }
            let since = lost.since;
            let device = lost.device.clone();
            match reopen(self) {
                Ok(info) => {
                    let text = if self.device.missing.is_empty() {
                        format!("Audio is back on {}", info.device)
                    } else {
                        format!(
                            "{} is not connected: playing on {} until it is back",
                            self.device.missing.join(", "),
                            info.device
                        )
                    };
                    self.notify(NoticeLevel::Info, text);
                }
                Err(e) => {
                    // Still away: keep looking (the error said once).
                    tracing::info!("audio: reopening failed ({e})");
                    self.device.lost = Some(Lost { since, device });
                }
            }
            self.revision += 1;
            return;
        }
        // Playing on the default: the chosen device is back.
        if !back {
            return;
        }
        if self.transport.playing || self.recording.is_some() {
            if !self.device.told_back {
                self.device.told_back = true;
                self.notify(
                    NoticeLevel::Info,
                    format!(
                        "{} is back: audio moves to it when playback stops",
                        self.device.missing.join(", ")
                    ),
                );
            }
            return;
        }
        let missing = self.device.missing.join(", ");
        match reopen(self) {
            Ok(info) => {
                self.notify(
                    NoticeLevel::Info,
                    format!("{missing} is back: audio on {}", info.device),
                );
            }
            Err(e) => self.notify(NoticeLevel::Warning, format!("{missing}: {e}")),
        }
        self.revision += 1;
    }
}
