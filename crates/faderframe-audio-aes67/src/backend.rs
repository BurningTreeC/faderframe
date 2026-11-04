//! AES67 as the audio system: the engine's callback runs on a timer
//! locked to the PTP media clock, its inputs are the streams received and
//! its outputs the streams sent.
//!
//! Timing: callback k runs when the media clock reaches `N` (a frame
//! count, advancing exactly one buffer per callback). Its inputs are the
//! received frames stamped `[N − L − B, N − L)` (`L` = the link offset,
//! `B` = the buffer): every frame is played `L` after its timestamp, as
//! AES67 receivers do, whatever the network's jitter within `L`. Its
//! outputs are stamped `[N + B, N + 2B)` — the block's deadline is `N +
//! B`, and a packet goes out once its last frame's time has passed — so
//! senders pace packets evenly and never send a frame before it was
//! computed. Receivers elsewhere play them their own link offset later:
//! the stream reports `L` both ways (the network's receivers are taken to
//! use the same link offset).
//!
//! The timer thread is the audio thread: made realtime, it sleeps until
//! the media clock reaches the next callback, then reads the receive
//! rings, runs the engine and fills the send rings ([`Clocked::block`]:
//! no allocation, no locks). A clock step (the first lock to a
//! grandmaster, a new one) starts the count again — counted as an xrun —,
//! as does falling far behind (a suspended machine). A new buffer size
//! keeps the inputs continuous; the outputs skip or repeat the difference
//! once.

use crate::net;
use crate::ptp::clock::{ClockMap, ClockState, frame_ns, media_frame};
use crate::ptp::message::ClockIdentity;
use crate::ptp::service::{PtpService, PtpStatus};
use crate::ring::MediaRing;
use crate::sdp::{Origin, RefClock, StreamDescription};
use crate::settings::Aes67Settings;
use crate::stream::{Receiver, RxStats, SendFormat, Sender, TxStats};
use crate::{Services, discovery::Discovery};
use faderframe_audio::follow::now_ns;
use faderframe_audio::{
    AudioBackend, AudioCallback, AudioError, AudioStream, Capabilities, DeviceBuffers, DeviceInfo,
    DeviceStatus, MAX_BUFFER_SIZE, OwnedBuffers, RateControl, StreamConfig, StreamInfo,
    StreamMonitor, StreamRouting, StreamStatus, validate_format,
};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

/// The backend's id (`AudioBackend::id`).
pub const BACKEND_ID: &str = "aes67";
/// Its one device's id.
pub const DEVICE_ID: &str = "aes67";
/// The buffer a stream opens with when none is asked for.
pub const DEFAULT_BUFFER: u32 = 256;

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A received stream's place among the engine's inputs.
struct InPlace {
    ring: Arc<MediaRing>,
    first: usize,
    channels: usize,
    stats: Arc<RxStats>,
}

/// A sent stream's place among the engine's outputs.
struct OutPlace {
    ring: Arc<MediaRing>,
    first: usize,
}

/// The audio thread's work, one callback at a time (see the module docs).
/// Realtime safe once made.
pub struct Clocked {
    callback: Box<dyn AudioCallback>,
    info: StreamInfo,
    bufs: OwnedBuffers,
    inputs: Box<[InPlace]>,
    outputs: Box<[OutPlace]>,
    /// The link offset (frames).
    link: u64,
}

impl Clocked {
    /// The engine's `callback` with these rings: `inputs` = (ring, its
    /// stats) in the order of the stream's input channels, `outputs` the
    /// rings of the output channels. Channels beyond them are silent
    /// (inputs) or go nowhere (outputs).
    pub fn new(
        callback: Box<dyn AudioCallback>,
        info: StreamInfo,
        inputs: Vec<(Arc<MediaRing>, Arc<RxStats>)>,
        outputs: Vec<Arc<MediaRing>>,
        link: u64,
    ) -> Self {
        let mut first = 0;
        let inputs: Box<[InPlace]> = inputs
            .into_iter()
            .map(|(ring, stats)| {
                let channels = ring.channels();
                let p = InPlace {
                    ring,
                    first,
                    channels,
                    stats,
                };
                first += channels;
                p
            })
            .collect();
        let mut first = 0;
        let outputs: Box<[OutPlace]> = outputs
            .into_iter()
            .map(|ring| {
                let channels = ring.channels();
                let p = OutPlace { ring, first };
                first += channels;
                p
            })
            .collect();
        let bufs = OwnedBuffers::new(
            usize::from(info.input_channels),
            usize::from(info.output_channels),
            MAX_BUFFER_SIZE as usize,
        );
        Self {
            callback,
            info,
            bufs,
            inputs,
            outputs,
            link,
        }
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    pub fn prepare(&mut self) {
        self.callback.prepare(&self.info);
    }

    /// A new buffer size (frames; realtime safe: the buffers hold the
    /// largest).
    pub fn set_block(&mut self, frames: u32) {
        self.info.buffer_size = frames.clamp(1, MAX_BUFFER_SIZE);
        self.callback.prepare(&self.info);
    }

    /// The callback that runs when the media clock reaches `now`.
    pub fn block(&mut self, now: u64) {
        let n = self.info.buffer_size as usize;
        self.bufs.set_frames(n);
        let from = now.saturating_sub(self.link + n as u64);
        let ins = self.bufs.input_channels();
        for p in self.inputs.iter() {
            let mut missing = 0;
            for c in 0..p.channels {
                if p.first + c < ins {
                    missing = missing.max(p.ring.read_channel(
                        from,
                        c,
                        self.bufs.input_mut(p.first + c),
                    ));
                }
            }
            if missing > 0 {
                p.stats.missing.fetch_add(missing as u64, Ordering::Relaxed);
            }
        }
        self.callback.process(&mut self.bufs);
        let outs = self.bufs.output_channels();
        let bufs = &self.bufs;
        for p in self.outputs.iter() {
            p.ring.write(now + n as u64, n, |i, c| {
                if p.first + c < outs {
                    bufs.output_ref(p.first + c)[i]
                } else {
                    0.0
                }
            });
        }
    }
}

/// Wait until local time `local` (at most `most`).
fn sleep_until(local: u64, most: Duration) {
    let now = now_ns();
    if local > now {
        std::thread::sleep(Duration::from_nanos(local - now).min(most));
    }
}

/// The audio thread (see the module docs).
fn run(
    mut clocked: Clocked,
    clock: Arc<ClockMap>,
    monitor: Arc<StreamMonitor>,
    stop: Arc<AtomicBool>,
    requested: Arc<AtomicU32>,
) {
    faderframe_realtime::priority::make_audio_thread_realtime();
    faderframe_realtime::memory::prefault_stack(256 * 1024);
    clocked.prepare();
    monitor.set_running(true);
    let rate = clocked.info().sample_rate;
    let mut block = u64::from(clocked.info().buffer_size);
    let mut steps = clock.steps();
    let mut next = media_frame(clock.now(), rate) + block;
    while !stop.load(Ordering::Relaxed) {
        let want = requested.load(Ordering::Relaxed);
        if u64::from(want) != block {
            // The inputs carry on where the last callback left them (a
            // take records on without a seam); the outputs jump by the
            // difference once.
            next = next - block + u64::from(want);
            clocked.set_block(want);
            block = u64::from(want);
            monitor.set_buffer_size(want);
        }
        let s = clock.steps();
        if s != steps {
            // The clock jumped: count from where it is now.
            steps = s;
            monitor.record_xrun();
            next = media_frame(clock.now(), rate) + block;
            continue;
        }
        let due = clock.local(frame_ns(next, rate));
        if now_ns() < due {
            sleep_until(due, Duration::from_millis(50));
            continue;
        }
        let media_now = media_frame(clock.now(), rate);
        if media_now > next + 8 * block.max(64) {
            // Far behind (suspended, stopped in a debugger).
            monitor.record_xrun();
            next = media_now;
        }
        clocked.block(next);
        monitor.record_callback();
        next += block;
    }
    monitor.set_running(false);
}

/// AES67 as the audio system (see the module docs).
pub struct Aes67Backend {
    settings: Aes67Settings,
}

impl Default for Aes67Backend {
    fn default() -> Self {
        Self::new(crate::settings())
    }
}

impl Aes67Backend {
    pub fn new(settings: Aes67Settings) -> Self {
        Self { settings }
    }

    pub fn settings(&self) -> &Aes67Settings {
        &self.settings
    }
}

/// The streams of `settings` as the device list shows them: channel
/// names "stream · n".
fn channel_names(settings: &Aes67Settings) -> (Vec<String>, Vec<String>) {
    let mut inputs = Vec::new();
    for r in settings.active_receives() {
        if let Ok(d) = StreamDescription::parse(&r.sdp) {
            for c in 0..d.channels {
                inputs.push(format!("{} · {}", d.name, c + 1));
            }
        }
    }
    let mut outputs = Vec::new();
    for s in settings.active_sends() {
        for c in 0..s.channels {
            outputs.push(format!("{} · {}", s.name, c + 1));
        }
    }
    (inputs, outputs)
}

/// The description of send stream `index` as announced.
pub fn send_description(
    settings: &Aes67Settings,
    index: usize,
    interface: Ipv4Addr,
    grandmaster: Option<ClockIdentity>,
    session_id: u64,
    version: u64,
) -> Option<StreamDescription> {
    let s = settings.active_sends().nth(index)?;
    let group = s
        .address
        .unwrap_or_else(|| net::default_group(interface, index));
    Some(StreamDescription {
        origin: Origin {
            username: "-".into(),
            session_id,
            version,
            address: interface.to_string(),
        },
        name: s.name.clone(),
        info: Some(format!("FaderFrame · {} channels", s.channels)),
        keywords: Some("FaderFrame".into()),
        address: group,
        ttl: Some(settings.net.ttl.min(255) as u8),
        port: s.port,
        source: Some(interface),
        payload_type: crate::rtp::PAYLOAD_TYPE,
        encoding: settings.encoding,
        sample_rate: settings.sample_rate,
        channels: s.channels,
        frames_per_packet: settings.packet_time.frames(settings.sample_rate),
        refclk: Some(RefClock::Ptp {
            version: "IEEE1588-2008".into(),
            grandmaster,
            domain: Some(settings.ptp.domain),
        }),
        media_offset: 0,
        domain: Some(settings.ptp.domain),
    })
}

/// A session id for a send stream: stable for its name and place on this
/// interface (receivers keep following it across restarts).
fn session_id(interface: Ipv4Addr, name: &str, index: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in format!("{interface}/{name}/{index}").bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h % 4_000_000_000 + 1
}

/// A stream received, as the running stream keeps it.
struct Received {
    key: String,
    receiver: Receiver,
    first: u16,
}

/// A stream sent.
struct Sent {
    key: String,
    name: String,
    sender: Sender,
    first: u16,
    channels: u16,
    description: StreamDescription,
}

/// What both modes keep of the network side: receivers, senders, their
/// announcements, the clock.
pub(crate) struct Network {
    settings: Aes67Settings,
    interface: Ipv4Addr,
    pub(crate) services: Services,
    received: Vec<Received>,
    sent: Vec<Sent>,
    /// Streams asked for that could not be opened (with why).
    pub(crate) failed: Vec<String>,
    grandmaster: Option<ClockIdentity>,
}

impl Network {
    /// Open the streams of `settings` at `rate` (`None`: each at its own —
    /// the follower mode).
    pub(crate) fn open(settings: &Aes67Settings, rate: Option<u32>) -> Self {
        let services = crate::services(settings);
        let interface = services.ptp.config().interface;
        let clock = services.ptp.clock();
        let link = settings.link_offset_frames();
        let mut failed = Vec::new();
        let mut received = Vec::new();
        let mut first = 0u16;
        for r in settings.active_receives() {
            // A newer description heard announced wins over the one kept.
            let sdp = services
                .discovery
                .as_ref()
                .and_then(|d| {
                    d.streams()
                        .into_iter()
                        .find(|h| h.description.key() == r.key)
                })
                .map_or_else(|| r.sdp.clone(), |h| h.sdp);
            let d = match StreamDescription::parse(&sdp) {
                Ok(d) => d,
                Err(e) => {
                    failed.push(format!("{}: {e}", r.name));
                    continue;
                }
            };
            if let Some(rate) = rate
                && d.sample_rate != rate
            {
                failed.push(format!(
                    "{}: runs at {} Hz, not {rate} Hz",
                    d.name, d.sample_rate
                ));
                continue;
            }
            let link = link * u64::from(d.sample_rate) / u64::from(settings.sample_rate.max(1));
            match Receiver::start(&d, interface, Arc::clone(&clock), link) {
                Ok(receiver) => {
                    received.push(Received {
                        key: r.key.clone(),
                        receiver,
                        first,
                    });
                    first += d.channels;
                }
                Err(e) => failed.push(format!("{}: {e}", d.name)),
            }
        }
        let grandmaster = services.ptp.grandmaster().or(Some(services.ptp.identity()));
        let mut sent = Vec::new();
        let mut first = 0u16;
        let version = crate::ptp::clock::system_tai_ns() / 1_000_000_000;
        for (k, s) in settings.active_sends().enumerate() {
            let id = session_id(interface, &s.name, k);
            let Some(d) = send_description(settings, k, interface, grandmaster, id, version) else {
                continue;
            };
            let format = SendFormat {
                encoding: d.encoding,
                sample_rate: d.sample_rate,
                channels: d.channels,
                frames_per_packet: d.frames_per_packet,
                ttl: settings.net.ttl,
                multicast_loop: settings.net.multicast_loop,
            };
            match Sender::start(
                SocketAddrV4::new(d.address, d.port),
                interface,
                &format,
                Arc::clone(&clock),
            ) {
                Ok(sender) => {
                    if let Some(disc) = &services.discovery {
                        disc.announce(&d.key(), d.write());
                    }
                    sent.push(Sent {
                        key: d.key(),
                        name: s.name.clone(),
                        sender,
                        first,
                        channels: s.channels,
                        description: d,
                    });
                }
                Err(e) => failed.push(format!("{}: {e}", s.name)),
            }
            first += s.channels;
        }
        for f in &failed {
            tracing::warn!("AES67: {f}");
        }
        Self {
            settings: settings.clone(),
            interface,
            services,
            received,
            sent,
            failed,
            grandmaster,
        }
    }

    pub(crate) fn clock(&self) -> Arc<ClockMap> {
        self.services.ptp.clock()
    }

    pub(crate) fn input_channels(&self) -> u16 {
        self.received
            .iter()
            .map(|r| r.receiver.description.channels)
            .sum()
    }

    pub(crate) fn output_channels(&self) -> u16 {
        self.sent.iter().map(|s| s.channels).sum()
    }

    /// The receive rings with their stats, in input order.
    pub(crate) fn input_rings(&self) -> Vec<(Arc<MediaRing>, Arc<RxStats>, u32)> {
        self.received
            .iter()
            .map(|r| {
                (
                    Arc::clone(&r.receiver.ring),
                    Arc::clone(&r.receiver.stats),
                    r.receiver.description.sample_rate,
                )
            })
            .collect()
    }

    /// The send rings, in output order.
    pub(crate) fn output_rings(&self) -> Vec<(Arc<MediaRing>, Arc<TxStats>, u32)> {
        self.sent
            .iter()
            .map(|s| {
                (
                    Arc::clone(&s.sender.ring),
                    Arc::clone(&s.sender.stats),
                    s.description.sample_rate,
                )
            })
            .collect()
    }

    pub(crate) fn names(&self) -> (Vec<String>, Vec<String>) {
        let mut ins = Vec::new();
        for r in &self.received {
            for c in 0..r.receiver.description.channels {
                ins.push(format!("{} · {}", r.receiver.description.name, c + 1));
            }
        }
        let mut outs = Vec::new();
        for s in &self.sent {
            for c in 0..s.channels {
                outs.push(format!("{} · {}", s.name, c + 1));
            }
        }
        (ins, outs)
    }

    /// Received streams: key, name, channels, rate.
    pub(crate) fn received_streams(&self) -> Vec<(String, String, u16, u32)> {
        self.received
            .iter()
            .map(|r| {
                let d = &r.receiver.description;
                (r.key.clone(), d.name.clone(), d.channels, d.sample_rate)
            })
            .collect()
    }

    /// Sent streams: key, name, channels, rate.
    pub(crate) fn sent_streams(&self) -> Vec<(String, String, u16, u32)> {
        self.sent
            .iter()
            .map(|s| {
                (
                    s.key.clone(),
                    s.name.clone(),
                    s.channels,
                    s.description.sample_rate,
                )
            })
            .collect()
    }

    pub(crate) fn stream_names(&self) -> Vec<String> {
        self.received
            .iter()
            .map(|r| r.receiver.description.name.clone())
            .chain(self.sent.iter().map(|s| s.name.clone()))
            .collect()
    }

    /// Received streams not arriving now, and streams that could not be
    /// opened (with why).
    pub(crate) fn missing(&self) -> Vec<String> {
        let now = now_ns();
        self.received
            .iter()
            .filter(|r| !r.receiver.stats.present(now))
            .map(|r| r.receiver.description.name.clone())
            .chain(self.failed.iter().cloned())
            .collect()
    }

    /// Each stream as a device of the running stream (the PTP clock
    /// first: it drives).
    pub(crate) fn devices(&self, rate: u32, offset_in: u16, offset_out: u16) -> Vec<DeviceStatus> {
        let status = self.services.ptp.status();
        let locked = status.state.is_shared();
        let link = self.settings.link_offset_frames() as u32;
        let now = now_ns();
        let mut out = Vec::new();
        for r in &self.received {
            let d = &r.receiver.description;
            let st = &r.receiver.stats;
            out.push(DeviceStatus {
                id: format!("aes67:{}", r.key),
                name: d.name.clone(),
                master: false,
                present: st.present(now),
                inputs: (0..d.channels).map(|c| offset_in + r.first + c).collect(),
                outputs: Vec::new(),
                sample_rate: d.sample_rate,
                input_latency: link * rate / d.sample_rate.max(1),
                output_latency: 0,
                // On the media clock itself: no drift of its own.
                drift_ppm: 0.0,
                locked,
                gaps: st.lost.load(Ordering::Relaxed) + st.late.load(Ordering::Relaxed),
            });
        }
        for s in &self.sent {
            out.push(DeviceStatus {
                id: format!("aes67:{}", s.key),
                name: s.name.clone(),
                master: false,
                present: true,
                inputs: Vec::new(),
                outputs: (0..s.channels).map(|c| offset_out + s.first + c).collect(),
                sample_rate: s.description.sample_rate,
                input_latency: 0,
                output_latency: link,
                drift_ppm: 0.0,
                locked,
                gaps: s.sender.stats.skipped.load(Ordering::Relaxed),
            });
        }
        out
    }

    /// About once a second: follow a stream whose announcement moved it
    /// (a new address or port), announce again under a new grandmaster.
    pub(crate) fn maintain(&mut self) {
        let gm = self
            .services
            .ptp
            .grandmaster()
            .or(Some(self.services.ptp.identity()));
        if gm != self.grandmaster {
            self.grandmaster = gm;
            let version = crate::ptp::clock::system_tai_ns() / 1_000_000_000;
            for s in &mut self.sent {
                if let Some(RefClock::Ptp { grandmaster, .. }) = &mut s.description.refclk {
                    *grandmaster = gm;
                }
                s.description.origin.version = version;
                if let Some(d) = &self.services.discovery {
                    d.announce(&s.key, s.description.write());
                }
            }
        }
        let Some(disc) = self.services.discovery.clone() else {
            return;
        };
        let heard = disc.streams();
        let clock = self.clock();
        let link = self.settings.link_offset_frames();
        for r in &mut self.received {
            let Some(h) = heard.iter().find(|h| h.description.key() == r.key) else {
                continue;
            };
            let now = &r.receiver.description;
            let new = &h.description;
            let moved = new.address != now.address
                || new.port != now.port
                || new.media_offset != now.media_offset
                || new.payload_type != now.payload_type
                || new.encoding != now.encoding
                || new.frames_per_packet != now.frames_per_packet;
            if moved && new.channels == now.channels && new.sample_rate == now.sample_rate {
                tracing::info!("AES67: {} moved; following it", new.name);
                if let Err(e) = r
                    .receiver
                    .follow(new, self.interface, Arc::clone(&clock), link)
                {
                    tracing::warn!("AES67: {}: {e}", new.name);
                }
            }
        }
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        if let Some(d) = &self.services.discovery {
            for s in &self.sent {
                d.withdraw(&s.key);
            }
        }
        crate::linger(Arc::clone(&self.services.ptp));
    }
}

/// A running AES67 stream.
pub struct Aes67Stream {
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
    stop: Arc<AtomicBool>,
    requested: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
    network: Mutex<Network>,
}

impl AudioBackend for Aes67Backend {
    fn id(&self) -> &'static str {
        BACKEND_ID
    }

    fn display_name(&self) -> &'static str {
        "AES67 network audio"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            rate: RateControl::Device,
            live_buffer_size: true,
            live_sample_rate: false,
            output_devices: 1,
            input_devices: 1,
            clocked_by_server: false,
            resamples_followers: false,
            // Streams come and go by themselves (their channels fall
            // silent meanwhile).
            follows_devices: true,
            free_channel_counts: true,
            force_format: false,
        }
    }

    fn is_available(&self) -> bool {
        true
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        let (input_names, output_names) = channel_names(&self.settings);
        let interface = net::interface(self.settings.interface);
        Ok(vec![DeviceInfo {
            id: DEVICE_ID.into(),
            name: format!("AES67 network ({interface})"),
            input_channels: input_names.len() as u16,
            output_channels: output_names.len() as u16,
            input_names,
            output_names,
            sample_rates: vec![self.settings.sample_rate],
            current_sample_rate: Some(self.settings.sample_rate),
            current_buffer_size: None,
            buffer_sizes: Some((16, MAX_BUFFER_SIZE)),
            clock: Some("ptp".into()),
            is_default: true,
        }])
    }

    fn open_stream(
        &mut self,
        config: StreamConfig,
        callback: Box<dyn AudioCallback>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let rate = self.settings.sample_rate;
        if let Some(r) = config.sample_rate
            && r != rate
        {
            return Err(AudioError::UnsupportedConfig(format!(
                "the AES67 streams run at {rate} Hz (Preferences → Network Audio), not {r} Hz"
            )));
        }
        let buffer = config.buffer_size.unwrap_or(DEFAULT_BUFFER);
        validate_format(rate, buffer)?;
        let network = Network::open(&self.settings, Some(rate));
        let link = self.settings.link_offset_frames();
        let inputs = network.input_channels().max(config.input_channels);
        let outputs = network.output_channels().max(config.output_channels);
        let info = StreamInfo {
            backend: BACKEND_ID,
            device: "AES67 network".into(),
            sample_rate: rate,
            buffer_size: buffer,
            input_channels: inputs,
            output_channels: outputs,
            input_latency: link as u32,
            output_latency: link as u32,
        };
        let clocked = Clocked::new(
            callback,
            info.clone(),
            network
                .input_rings()
                .into_iter()
                .map(|(r, s, _)| (r, s))
                .collect(),
            network
                .output_rings()
                .into_iter()
                .map(|(r, _, _)| r)
                .collect(),
            link,
        );
        let monitor = StreamMonitor::new(rate, buffer);
        monitor.set_input_latency(link as u32);
        monitor.set_output_latency(link as u32);
        let stop = Arc::new(AtomicBool::new(false));
        let requested = Arc::new(AtomicU32::new(buffer));
        let clock = network.clock();
        let (m, s, r) = (
            Arc::clone(&monitor),
            Arc::clone(&stop),
            Arc::clone(&requested),
        );
        let thread = std::thread::Builder::new()
            .name("faderframe-aes67-audio".into())
            .spawn(move || run(clocked, clock, m, s, r))
            .map_err(|e| AudioError::Stream(format!("cannot start the audio thread: {e}")))?;
        Ok(Box::new(Aes67Stream {
            info,
            monitor,
            stop,
            requested,
            thread: Some(thread),
            network: Mutex::new(network),
        }))
    }
}

impl Aes67Stream {
    /// The clock's state (Preferences).
    pub fn ptp_status(&self) -> PtpStatus {
        locked(&self.network).services.ptp.status()
    }
}

impl AudioStream for Aes67Stream {
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

    fn routing(&self) -> StreamRouting {
        let net = locked(&self.network);
        let (ins, outs) = net.names();
        let pad = |names: Vec<String>, n: u16| {
            let mut v: Vec<Option<String>> = names.into_iter().map(Some).collect();
            v.resize(usize::from(n), None);
            v
        };
        StreamRouting {
            inputs: pad(ins, self.info.input_channels),
            outputs: pad(outs, self.info.output_channels),
            missing: net.missing(),
            devices: net.stream_names(),
        }
    }

    fn maintain(&mut self) {
        locked(&self.network).maintain();
    }

    fn devices(&self) -> Vec<DeviceStatus> {
        let net = locked(&self.network);
        let status = net.services.ptp.status();
        let name = match status.port.as_ref().and_then(|p| p.grandmaster) {
            Some(gm) if status.state != ClockState::Master => {
                format!("PTP clock, grandmaster {}", gm.grandmaster)
            }
            _ => format!("PTP clock ({})", status.state.label()),
        };
        let mut out = vec![DeviceStatus {
            id: "aes67:ptp".into(),
            name,
            master: true,
            present: true,
            sample_rate: self.info.sample_rate,
            input_latency: self.info.input_latency,
            output_latency: self.info.output_latency,
            locked: status.state.is_shared(),
            drift_ppm: status.port.map_or(0.0, |p| p.drift_ppm),
            ..DeviceStatus::default()
        }];
        out.extend(net.devices(self.info.sample_rate, 0, 0));
        out
    }
}

impl Drop for Aes67Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The discovery service of a running network side (for tests).
#[doc(hidden)]
pub fn discovery_of(stream: &Aes67Stream) -> Option<Arc<Discovery>> {
    locked(&stream.network).services.discovery.clone()
}

/// The PTP service of a running stream.
pub fn ptp_of(stream: &Aes67Stream) -> Arc<PtpService> {
    Arc::clone(&locked(&stream.network).services.ptp)
}
