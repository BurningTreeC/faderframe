//! AES67 streams beside another audio system's device: the device's
//! clock drives the engine, and each stream joins it as a follower of
//! `faderframe_audio::follow` — its own drift loop and resampler carry
//! the PTP media clock's frames onto the device's (and back), exactly as
//! a second USB interface would be joined.
//!
//! [`Alongside`] wraps any backend: its stream is the inner backend's
//! with the streams' channels after the device's (inputs: the received
//! streams in order; outputs: the sent ones). The engine's callback is
//! joined once the inner stream is open (its real rate and buffer known):
//! until then — a callback or two — the device plays silence.
//!
//! The follower side of every stream is a pacer thread on the media
//! clock: each millisecond it hands a received stream's frames whose
//! playout time has come (now less the link offset) to its follower
//! input, stamped with the local time that frame boundary was reached —
//! so the drift loop measures the PTP clock against the device's without
//! the pacer's own wake-up jitter —, and takes from each sent stream's
//! follower output the frames up to a little ahead of now, into its send
//! ring. A stream's latency is reported as its device latency (the link
//! offset; sending: the lead and the receivers' link offset), so the
//! follower's channels carry their own latency in `AudioStream::devices`.

use crate::backend::Network;
use crate::ptp::clock::{ClockMap, frame_ns, media_frame};
use crate::ring::MediaRing;
use crate::settings::Aes67Settings;
use crate::stream::RxStats;
use faderframe_audio::follow::{
    self, Direction, FollowerEnd, FollowerSpec, Followers, Joined, Place, now_ns,
};
use faderframe_audio::{
    AudioBackend, AudioCallback, AudioError, AudioStream, Capabilities, DeviceBuffers, DeviceInfo,
    DeviceStatus, StreamConfig, StreamInfo, StreamRouting, StreamStatus,
};
use faderframe_realtime::priority::{Helper, follow_audio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

/// The pacer's period.
pub const TICK: Duration = Duration::from_millis(1);
/// Sent frames are taken this far ahead of the media clock (frames are
/// sent once their time has passed, so they must be in the ring before).
pub const LEAD_MS: f64 = 2.0;

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The engine's callback once joined; silence before.
struct Forward {
    incoming: rtrb::Consumer<Box<Joined>>,
    joined: Option<Box<Joined>>,
    /// The device's stream as last prepared (numbers only are updated:
    /// no allocation on the device's thread).
    info: StreamInfo,
}

impl AudioCallback for Forward {
    fn prepare(&mut self, info: &StreamInfo) {
        self.info.sample_rate = info.sample_rate;
        self.info.buffer_size = info.buffer_size;
        self.info.input_latency = info.input_latency;
        self.info.output_latency = info.output_latency;
        if let Some(j) = self.joined.as_mut() {
            j.prepare(&self.info);
        }
    }

    fn process(&mut self, io: &mut dyn DeviceBuffers) {
        if self.joined.is_none()
            && let Ok(mut j) = self.incoming.pop()
        {
            j.prepare(&self.info);
            self.joined = Some(j);
        }
        match self.joined.as_mut() {
            Some(j) => j.process(io),
            None => {
                for c in 0..io.output_channels() {
                    io.output(c).fill(0.0);
                }
            }
        }
    }
}

/// One stream's follower end, as the pacer drives it.
struct Paced {
    end: FollowerEnd,
    ring: Arc<MediaRing>,
    rate: u32,
    /// Receiving: the link offset; sending: the lead (frames).
    offset: u64,
    /// The next media frame to hand over (`None`: start from now).
    at: Option<u64>,
    scratch: Vec<f32>,
}

impl Paced {
    /// One tick at local time `now`.
    fn tick(&mut self, clock: &ClockMap, now: u64, restart: bool) {
        let media_now = media_frame(clock.ptp(now), self.rate);
        let ch = self.ring.channels();
        let tick = u64::from(self.rate) / 1000;
        let limit = (self.scratch.len() / ch.max(1)) as u64;
        match &mut self.end {
            FollowerEnd::Input(end) => {
                // Frames whose playout time has come.
                let target = media_now.saturating_sub(self.offset);
                let at = match self.at {
                    Some(a) if !restart && a <= target && target - a <= 64 * tick => a,
                    _ => target.saturating_sub(tick),
                };
                let n = (target - at).min(limit) as usize;
                // Missing frames are silence (the receiver counts them).
                self.ring
                    .read(at, n, |i, c, v| self.scratch[i * ch + c] = v);
                // The moment the last frame's playout came.
                let stamp = clock.local(frame_ns(at + n as u64 + self.offset, self.rate));
                end.set_device_latency(self.offset as u32);
                let scratch = &self.scratch;
                end.capture_with(n, stamp, |i, c| scratch[i * ch + c]);
                self.at = Some(at + n as u64);
            }
            FollowerEnd::Output(end) => {
                // Frames stamped up to a little ahead of now.
                let target = media_now + self.offset;
                let at = match self.at {
                    Some(a) if !restart && a <= target && target - a <= 64 * tick => a,
                    _ => target.saturating_sub(tick),
                };
                let n = (target - at).min(limit) as usize;
                let stamp = clock.local(frame_ns(at + n as u64 - self.offset, self.rate));
                let scratch = &mut self.scratch;
                end.play_with(n, stamp, |i, c, v| scratch[i * ch + c] = v);
                let scratch = &self.scratch;
                self.ring.write(at, n, |i, c| scratch[i * ch + c]);
                self.at = Some(at + n as u64);
            }
        }
    }
}

/// The pacer thread: its followers, new ends from the control side.
fn pace(
    mut paced: Vec<Option<Paced>>,
    clock: Arc<ClockMap>,
    stop: Arc<AtomicBool>,
    fresh: mpsc::Receiver<(usize, FollowerEnd)>,
) {
    let mut steps = clock.steps();
    let tick = TICK.as_nanos() as u64;
    let mut next = now_ns();
    while !stop.load(Ordering::Relaxed) {
        follow_audio(Helper::Near);
        while let Ok((k, end)) = fresh.try_recv() {
            if let Some(Some(p)) = paced.get_mut(k) {
                p.end = end;
                p.at = None;
            }
        }
        let s = clock.steps();
        let restart = s != steps;
        steps = s;
        let now = now_ns();
        for p in paced.iter_mut().flatten() {
            p.tick(&clock, now, restart);
        }
        next += tick;
        let now = now_ns();
        if next > now {
            std::thread::sleep(Duration::from_nanos(next - now));
        } else if now - next > 50 * tick {
            next = now;
        }
    }
}

/// Another backend with the AES67 streams joined to its device (see the
/// module docs).
pub struct Alongside {
    inner: Box<dyn AudioBackend>,
    settings: Aes67Settings,
}

impl Alongside {
    pub fn new(inner: Box<dyn AudioBackend>, settings: Aes67Settings) -> Self {
        Self { inner, settings }
    }

    /// `backends` with the streams of `settings` joined to each, where the
    /// settings ask for streams alongside (else as they are).
    pub fn wrap_all(
        backends: Vec<Box<dyn AudioBackend>>,
        settings: &Aes67Settings,
    ) -> Vec<Box<dyn AudioBackend>> {
        if !settings.alongside || !settings.has_streams() {
            return backends;
        }
        backends
            .into_iter()
            .map(|b| {
                if b.id() == crate::BACKEND_ID {
                    b
                } else {
                    Box::new(Self::new(b, settings.clone())) as Box<dyn AudioBackend>
                }
            })
            .collect()
    }
}

/// The places and specs of the streams beside a device with `ins`
/// inputs and `outs` outputs.
fn layout(network: &Network, ins: u16, outs: u16) -> (Vec<Place>, Vec<Place>, Vec<FollowerSpec>) {
    let mut inputs: Vec<Place> = (0..ins).map(Place::Master).collect();
    let mut outputs: Vec<Place> = (0..outs).map(Place::Master).collect();
    let mut specs = Vec::new();
    for (key, name, channels, rate, direction) in network.follower_streams() {
        let k = specs.len() as u16;
        let places = (0..channels).map(|channel| Place::Follower {
            follower: k,
            channel,
        });
        match direction {
            Direction::Input => inputs.extend(places),
            Direction::Output => outputs.extend(places),
        }
        specs.push(FollowerSpec {
            device: format!("aes67:{key}"),
            name,
            direction,
            channels,
            sample_rate: rate,
            block: (rate / 1000).max(1),
        });
    }
    (inputs, outputs, specs)
}

impl AudioBackend for Alongside {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn display_name(&self) -> &'static str {
        self.inner.display_name()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn is_available(&self) -> bool {
        self.inner.is_available()
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        self.inner.enumerate_devices()
    }

    fn open_stream(
        &mut self,
        config: StreamConfig,
        callback: Box<dyn AudioCallback>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        if !self.settings.has_streams() {
            return self.inner.open_stream(config, callback);
        }
        let network = Network::open(&self.settings, None);
        let (to_device, incoming) = rtrb::RingBuffer::new(1);
        let template = StreamInfo {
            backend: "aes67",
            device: String::new(),
            sample_rate: config.sample_rate.unwrap_or(48_000),
            buffer_size: config.buffer_size.unwrap_or(256),
            input_channels: 0,
            output_channels: 0,
            input_latency: 0,
            output_latency: 0,
        };
        let stream = self.inner.open_stream(
            config,
            Box::new(Forward {
                incoming,
                joined: None,
                info: template,
            }),
        )?;
        let device = stream.info();
        let (inputs, outputs, specs) =
            layout(&network, device.input_channels, device.output_channels);
        let (joined, followers, ends) = follow::join(
            callback,
            &device,
            inputs.clone(),
            outputs.clone(),
            specs.clone(),
        );
        let mut info = device.clone();
        info.input_channels = joined.info().input_channels;
        info.output_channels = joined.info().output_channels;
        let mut to_device = to_device;
        to_device
            .push(Box::new(joined))
            .map_err(|_| AudioError::Stream("the device did not take the streams".into()))?;
        let clock = network.clock();
        let mut paced = Vec::new();
        for ((end, spec), ring) in ends.into_iter().zip(&specs).zip(network.follower_rings()) {
            let rate = spec.sample_rate;
            let offset = match spec.direction {
                Direction::Input => {
                    self.settings.link_offset_frames() * u64::from(rate)
                        / u64::from(self.settings.sample_rate.max(1))
                }
                Direction::Output => (LEAD_MS * f64::from(rate) / 1000.0) as u64,
            };
            if let FollowerEnd::Output(o) = &end {
                // Taken this far ahead, played by receivers their link
                // offset after the stamp.
                o.set_device_latency(
                    (offset
                        + self.settings.link_offset_frames() * u64::from(rate)
                            / u64::from(self.settings.sample_rate.max(1)))
                        as u32,
                );
            }
            let ch = usize::from(spec.channels.max(1));
            paced.push(Some(Paced {
                end,
                ring,
                rate,
                offset,
                at: None,
                // Room for a long stall's catching up.
                scratch: vec![0.0; ch * 64 * (rate as usize / 1000).max(1)],
            }));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (fresh_tx, fresh_rx) = mpsc::channel();
        let (c, s) = (Arc::clone(&clock), Arc::clone(&stop));
        let pacer = std::thread::Builder::new()
            .name("faderframe-aes67-pace".into())
            .spawn(move || pace(paced, c, s, fresh_rx))
            .map_err(|e| AudioError::Stream(format!("cannot start the AES67 pacer: {e}")))?;
        Ok(Box::new(AlongsideStream {
            inner: stream,
            info,
            inputs,
            outputs,
            followers: Mutex::new(followers),
            network: Mutex::new(network),
            master_rate: device.sample_rate,
            fresh: fresh_tx,
            stop,
            pacer: Some(pacer),
        }))
    }
}

/// A device's stream with AES67 streams joined.
pub struct AlongsideStream {
    inner: Box<dyn AudioStream>,
    /// The device's info with the joined channel counts.
    info: StreamInfo,
    inputs: Vec<Place>,
    outputs: Vec<Place>,
    followers: Mutex<Followers>,
    network: Mutex<Network>,
    master_rate: u32,
    fresh: mpsc::Sender<(usize, FollowerEnd)>,
    stop: Arc<AtomicBool>,
    pacer: Option<JoinHandle<()>>,
}

impl AlongsideStream {
    fn away(&self) -> impl Fn(&str) -> bool + '_ {
        let net = locked(&self.network);
        let gone: Vec<String> = net.away_ids();
        move |id: &str| gone.iter().any(|g| g == id)
    }
}

impl AudioStream for AlongsideStream {
    fn info(&self) -> StreamInfo {
        let mut info = self.inner.info();
        info.input_channels = self.info.input_channels;
        info.output_channels = self.info.output_channels;
        info
    }

    fn status(&self) -> StreamStatus {
        self.inner.status()
    }

    fn request_buffer_size(&mut self, frames: u32) -> Result<(), AudioError> {
        self.inner.request_buffer_size(frames)
    }

    fn request_sample_rate(&mut self, rate: u32) -> Result<(), AudioError> {
        self.inner.request_sample_rate(rate)
    }

    fn routing(&self) -> StreamRouting {
        let mut r = self.inner.routing();
        let net = locked(&self.network);
        let (ins, outs) = net.names();
        let device_in = self
            .inputs
            .iter()
            .filter(|p| matches!(p, Place::Master(_)))
            .count();
        let device_out = self
            .outputs
            .iter()
            .filter(|p| matches!(p, Place::Master(_)))
            .count();
        r.inputs.resize(device_in, None);
        r.outputs.resize(device_out, None);
        r.inputs.extend(ins.into_iter().map(Some));
        r.outputs.extend(outs.into_iter().map(Some));
        r.missing.extend(net.missing());
        r.devices.extend(net.stream_names());
        r
    }

    fn maintain(&mut self) {
        self.inner.maintain();
        locked(&self.network).maintain();
        // The device's rate changed under us: every stream joins again
        // resampled to the new one.
        let device = self.inner.info();
        if device.sample_rate != self.master_rate {
            self.master_rate = device.sample_rate;
            let mut f = locked(&self.followers);
            f.set_master(device.sample_rate, device.buffer_size);
            for k in 0..f.len() {
                let Some(spec) = f.spec(k).cloned() else {
                    continue;
                };
                if let Some(end) = f.attach(k, spec.sample_rate, spec.block) {
                    let _ = self.fresh.send((k, end));
                }
            }
        }
        locked(&self.followers).collect();
    }

    fn io_workgroup(&self) -> Option<faderframe_realtime::Workgroup> {
        self.inner.io_workgroup()
    }

    fn devices(&self) -> Vec<DeviceStatus> {
        let inner = self.inner.devices();
        let device = self.inner.info();
        let channels = |places: &[Place]| -> Vec<u16> {
            places
                .iter()
                .enumerate()
                .filter(|(_, p)| matches!(p, Place::Master(_)))
                .map(|(c, _)| c as u16)
                .collect()
        };
        let master = DeviceStatus {
            id: device.device.clone(),
            name: device.device.clone(),
            master: true,
            present: true,
            inputs: channels(&self.inputs),
            outputs: channels(&self.outputs),
            sample_rate: device.sample_rate,
            input_latency: device.input_latency,
            output_latency: device.output_latency,
            locked: true,
            ..DeviceStatus::default()
        };
        let away = self.away();
        let joined = locked(&self.followers).describe(master, &self.inputs, &self.outputs, away);
        if inner.is_empty() {
            joined
        } else {
            inner
                .into_iter()
                .chain(joined.into_iter().skip(1))
                .collect()
        }
    }
}

impl Drop for AlongsideStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.pacer.take() {
            let _ = t.join();
        }
    }
}

impl Network {
    /// The streams as followers, in follower order: received ones first
    /// (key, name, channels, rate, direction).
    pub(crate) fn follower_streams(&self) -> Vec<(String, String, u16, u32, Direction)> {
        let mut out = Vec::new();
        for (key, name, channels, rate) in self.received_streams() {
            out.push((key, name, channels, rate, Direction::Input));
        }
        for (key, name, channels, rate) in self.sent_streams() {
            out.push((key, name, channels, rate, Direction::Output));
        }
        out
    }

    /// Their rings, in the same order.
    pub(crate) fn follower_rings(&self) -> Vec<Arc<MediaRing>> {
        self.input_rings()
            .into_iter()
            .map(|(r, _, _)| r)
            .chain(self.output_rings().into_iter().map(|(r, _, _)| r))
            .collect()
    }

    /// Follower device ids of received streams not arriving now.
    pub(crate) fn away_ids(&self) -> Vec<String> {
        let now = now_ns();
        self.received_stats()
            .into_iter()
            .filter(|(_, s)| !s.present(now))
            .map(|(k, _)| format!("aes67:{k}"))
            .collect()
    }

    fn received_stats(&self) -> Vec<(String, Arc<RxStats>)> {
        self.received_streams()
            .into_iter()
            .map(|(k, _, _, _)| k)
            .zip(self.input_rings().into_iter().map(|(_, s, _)| s))
            .collect()
    }
}
