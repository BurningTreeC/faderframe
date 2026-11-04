//! Streams on the network: a receiver puts each packet's frames into a
//! [`MediaRing`] at the media frames its RTP timestamp names; a sender
//! takes frames out of one at the media clock's pace and sends a packet
//! as soon as its last frame's time has passed. Each runs on a thread of
//! its own that follows the audio thread a step below; the audio side
//! only touches the ring.

use crate::net;
use crate::ptp::clock::{ClockMap, frame_ns, media_frame, unwrap_timestamp};
use crate::ring::MediaRing;
use crate::rtp::{self, Encoding};
use crate::sdp::StreamDescription;
use faderframe_audio::follow::now_ns;
use faderframe_realtime::priority::{Helper, follow_audio};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Seconds a ring holds (at least: link offset and jitter fit many
/// times over).
pub const RING_SECONDS: f64 = 1.0;
/// A receiver that has heard nothing for this long is away.
pub const AWAY_NS: u64 = 500_000_000;

/// How a received stream is getting on (written by its thread).
#[derive(Debug, Default)]
pub struct RxStats {
    pub packets: AtomicU64,
    /// Packets missing by their sequence numbers.
    pub lost: AtomicU64,
    /// Packets that came after their frames were played.
    pub late: AtomicU64,
    /// Packets too far from the media clock to place (another clock, a
    /// stream restarted elsewhere).
    pub outside: AtomicU64,
    /// Packets not of the stream's format.
    pub malformed: AtomicU64,
    /// Local time of the last packet (ns; 0: none yet).
    pub last: AtomicU64,
    /// How long after its timestamp the latest packet arrived, and the
    /// longest lately (ns; what the link offset must cover).
    pub arrival_ns: AtomicI64,
    pub worst_ns: AtomicI64,
    /// Frames the audio side found missing.
    pub missing: AtomicU64,
    /// Local time the receiver started (a stream gets [`AWAY_NS`] to
    /// arrive before it counts as away).
    pub started: AtomicU64,
}

impl RxStats {
    /// Packets are arriving (or the receiver has only just started).
    pub fn present(&self, now: u64) -> bool {
        let last = self.last.load(Ordering::Relaxed);
        let since = if last == 0 {
            self.started.load(Ordering::Relaxed)
        } else {
            last
        };
        since != 0 && now.saturating_sub(since) < AWAY_NS
    }
}

/// A stream being received.
pub struct Receiver {
    pub description: StreamDescription,
    pub ring: Arc<MediaRing>,
    pub stats: Arc<RxStats>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Receiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Receiver")
            .field("name", &self.description.name)
            .finish_non_exhaustive()
    }
}

/// The sender a packet must come from, where the socket does not filter
/// by group (and the description names one).
fn wanted_source(d: &StreamDescription) -> Option<Ipv4Addr> {
    d.source
        .or_else(|| d.origin.address.parse().ok())
        .filter(|_| !cfg!(target_os = "linux") && d.address.is_multicast())
}

impl Receiver {
    /// Join the stream `d` on `interface`; its frames land in a ring on
    /// the media clock of `clock` (at the stream's rate). `link` = link
    /// offset in frames (for the late count).
    pub fn start(
        d: &StreamDescription,
        interface: Ipv4Addr,
        clock: Arc<ClockMap>,
        link: u64,
    ) -> std::io::Result<Self> {
        let ring = Arc::new(MediaRing::new(
            usize::from(d.channels),
            (f64::from(d.sample_rate) * RING_SECONDS) as usize,
        ));
        Self::start_into(d, interface, clock, link, ring, Arc::default())
    }

    /// Follow the stream to where a new description of it says (another
    /// group or port, offset, format of the same width): the same ring
    /// and stats, a new socket.
    pub fn follow(
        &mut self,
        d: &StreamDescription,
        interface: Ipv4Addr,
        clock: Arc<ClockMap>,
        link: u64,
    ) -> std::io::Result<()> {
        if usize::from(d.channels) != self.ring.channels() {
            return Err(std::io::Error::other(
                "the stream changed its channel count",
            ));
        }
        let new = Self::start_into(
            d,
            interface,
            clock,
            link,
            Arc::clone(&self.ring),
            Arc::clone(&self.stats),
        )?;
        *self = new;
        Ok(())
    }

    fn start_into(
        d: &StreamDescription,
        interface: Ipv4Addr,
        clock: Arc<ClockMap>,
        link: u64,
        ring: Arc<MediaRing>,
        stats: Arc<RxStats>,
    ) -> std::io::Result<Self> {
        let socket = net::receiver(d.address, d.port, interface)?;
        let rate = d.sample_rate;
        let stop = Arc::new(AtomicBool::new(false));
        stats.started.store(now_ns().max(1), Ordering::Relaxed);
        let (ring2, stats2, stop2) = (Arc::clone(&ring), Arc::clone(&stats), Arc::clone(&stop));
        let desc = d.clone();
        let source = wanted_source(d);
        let thread = std::thread::Builder::new()
            .name("faderframe-aes67-rx".into())
            .spawn(move || {
                let ring = ring2;
                let stats = stats2;
                let ch = usize::from(desc.channels);
                let bytes = desc.encoding.bytes();
                let len = ring.len() as u64;
                let mut buf = [0u8; 9000];
                let mut last_seq: Option<u16> = None;
                while !stop2.load(Ordering::Relaxed) {
                    follow_audio(Helper::Near);
                    let Ok((n, from)) = socket.recv_from(&mut buf) else {
                        continue;
                    };
                    let local = now_ns();
                    if let (Some(want), std::net::SocketAddr::V4(f)) = (source, from)
                        && *f.ip() != want
                    {
                        continue;
                    }
                    let Ok((h, payload)) = rtp::parse(&buf[..n]) else {
                        stats.malformed.fetch_add(1, Ordering::Relaxed);
                        continue;
                    };
                    if h.payload_type != desc.payload_type
                        || payload.is_empty()
                        || !payload.len().is_multiple_of(ch * bytes)
                    {
                        stats.malformed.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let frames = payload.len() / (ch * bytes);
                    let now_ptp = clock.ptp(local);
                    let media_now = media_frame(now_ptp, rate);
                    let frame =
                        unwrap_timestamp(h.timestamp.wrapping_sub(desc.media_offset), media_now);
                    // Only within the window the ring holds around now.
                    if frame + len / 2 < media_now || frame > media_now + len / 4 {
                        stats.outside.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    if let Some(prev) = last_seq {
                        let gap = h.sequence.wrapping_sub(prev).wrapping_sub(1);
                        if gap > 0 && gap < 1_000 {
                            stats.lost.fetch_add(u64::from(gap), Ordering::Relaxed);
                        }
                    }
                    last_seq = Some(h.sequence);
                    let arrival = now_ptp as i64 - frame_ns(frame, rate) as i64;
                    stats.arrival_ns.store(arrival, Ordering::Relaxed);
                    let worst = stats.worst_ns.load(Ordering::Relaxed);
                    // The worst decays slowly (a second's packets).
                    let decayed = worst - worst / 1_000;
                    stats
                        .worst_ns
                        .store(decayed.max(arrival), Ordering::Relaxed);
                    if frame + frames as u64 + link <= media_now {
                        stats.late.fetch_add(1, Ordering::Relaxed);
                    }
                    let enc = desc.encoding;
                    ring.write(frame, frames, |f, c| {
                        let at = (f * ch + c) * bytes;
                        enc.get(&payload[at..at + bytes])
                    });
                    stats.packets.fetch_add(1, Ordering::Relaxed);
                    stats.last.store(local, Ordering::Relaxed);
                }
            })?;
        Ok(Self {
            description: d.clone(),
            ring,
            stats,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// How a sent stream is getting on.
#[derive(Debug, Default)]
pub struct TxStats {
    pub packets: AtomicU64,
    /// Frames sent as silence because the audio side had not written them
    /// (it was not running, or it missed its deadline).
    pub silent: AtomicU64,
    /// Packets skipped after the thread fell far behind.
    pub skipped: AtomicU64,
    pub errors: AtomicU64,
}

/// What a sender sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendFormat {
    pub encoding: Encoding,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames_per_packet: usize,
    pub ttl: u32,
    pub multicast_loop: bool,
}

/// A stream being sent.
pub struct Sender {
    pub group: SocketAddrV4,
    pub ring: Arc<MediaRing>,
    pub stats: Arc<TxStats>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Sender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sender")
            .field("group", &self.group)
            .finish_non_exhaustive()
    }
}

/// Wait until local time `local` (never longer than `most`).
fn sleep_until(local: u64, most: Duration) {
    let now = now_ns();
    if local > now {
        std::thread::sleep(Duration::from_nanos(local - now).min(most));
    }
}

impl Sender {
    /// Send `format` to `group` through `interface`, from a ring the audio
    /// side fills on the media clock of `clock`.
    pub fn start(
        group: SocketAddrV4,
        interface: Ipv4Addr,
        format: &SendFormat,
        clock: Arc<ClockMap>,
    ) -> std::io::Result<Self> {
        let socket = net::sender(
            interface,
            0,
            format.ttl,
            format.multicast_loop,
            net::DSCP_AF41,
        )?;
        let rate = format.sample_rate;
        let ring = Arc::new(MediaRing::new(
            usize::from(format.channels),
            (f64::from(rate) * RING_SECONDS) as usize,
        ));
        let stats = Arc::new(TxStats::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (ring2, stats2, stop2) = (Arc::clone(&ring), Arc::clone(&stats), Arc::clone(&stop));
        let f = format.clone();
        let ssrc = (now_ns() as u32) ^ u32::from_be_bytes(group.ip().octets()).rotate_left(7);
        let thread = std::thread::Builder::new()
            .name("faderframe-aes67-tx".into())
            .spawn(move || {
                let ring = ring2;
                let stats = stats2;
                let p = f.frames_per_packet.max(1) as u64;
                let ch = usize::from(f.channels);
                let mut buf = vec![0u8; rtp::HEADER_LEN + f.frames_per_packet * ch * 3];
                let mut scratch = vec![0.0f32; f.frames_per_packet * ch];
                let mut seq: u16 = (ssrc >> 16) as u16;
                let mut next: Option<u64> = None;
                let mut steps = clock.steps();
                let behind = ring.len() as u64 / 4;
                while !stop2.load(Ordering::Relaxed) {
                    follow_audio(Helper::Near);
                    let media_now = media_frame(clock.now(), rate);
                    let s = clock.steps();
                    let start_over = match next {
                        None => true,
                        Some(n) => s != steps || n + behind < media_now || n > media_now + p,
                    };
                    if start_over {
                        if next.is_some() {
                            stats.skipped.fetch_add(1, Ordering::Relaxed);
                        }
                        steps = s;
                        next = Some(media_now / p * p);
                    }
                    let mut at = next.unwrap_or(media_now);
                    while at + p <= media_now {
                        let header = rtp::Header {
                            marker: false,
                            payload_type: rtp::PAYLOAD_TYPE,
                            sequence: seq,
                            timestamp: at as u32,
                            ssrc,
                        };
                        let missing = ring.read(at, p as usize, |i, c, v| {
                            scratch[i * ch + c] = v;
                        });
                        let len = rtp::write_packet(
                            &mut buf,
                            &header,
                            f.encoding,
                            ch,
                            p as usize,
                            |i, c| scratch[i * ch + c],
                        );
                        if missing > 0 {
                            stats.silent.fetch_add(missing as u64, Ordering::Relaxed);
                        }
                        if socket.send_to(&buf[..len], group).is_err() {
                            stats.errors.fetch_add(1, Ordering::Relaxed);
                        }
                        stats.packets.fetch_add(1, Ordering::Relaxed);
                        seq = seq.wrapping_add(1);
                        at += p;
                    }
                    next = Some(at);
                    // The next packet is due when its last frame's time
                    // has passed.
                    let due = clock.local(frame_ns(at + p, rate));
                    sleep_until(due, Duration::from_millis(20));
                }
            })?;
        Ok(Self {
            group,
            ring,
            stats,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
