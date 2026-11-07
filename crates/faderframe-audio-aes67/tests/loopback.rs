//! End to end on this host: FaderFrame sends an AES67 stream to a
//! multicast group on the loopback interface (TTL 0: nothing leaves the
//! machine) and receives it back as another stream, first with AES67 as
//! the audio system, then beside the dummy device.
#![allow(clippy::unwrap_used)]

use faderframe_audio::{
    AudioBackend, AudioCallback, AudioError, AudioStream, DeviceBuffers, DeviceInfo, OwnedBuffers,
    StreamConfig, StreamInfo, StreamMonitor, StreamStatus,
};
use faderframe_audio_aes67::settings::{
    Aes67Settings, NetSettings, PtpSettings, ReceiveSettings, SendSettings,
};
use faderframe_audio_aes67::{Aes67Backend, Alongside};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn settings(base: u16) -> Aes67Settings {
    let group = Ipv4Addr::new(239, 69, 254, (base % 200) as u8);
    let sdp = format!(
        "v=0\r\no=- 77 1 IN IP4 127.0.0.1\r\ns=Back\r\nc=IN IP4 {group}/0\r\nt=0 0\r\n\
         m=audio {} RTP/AVP 96\r\na=rtpmap:96 L24/48000/2\r\na=ptime:1\r\n\
         a=mediaclk:direct=0\r\n",
        base + 4
    );
    Aes67Settings {
        interface: Some(Ipv4Addr::LOCALHOST),
        ptp: PtpSettings {
            lead: false,
            ..PtpSettings::default()
        },
        // Generous for a machine busy with builds.
        link_offset_ms: 10.0,
        sends: vec![SendSettings {
            name: "Out".into(),
            channels: 2,
            address: Some(group),
            port: base + 4,
            enabled: true,
        }],
        receives: vec![ReceiveSettings {
            key: "loop".into(),
            name: "Back".into(),
            sdp,
            enabled: true,
        }],
        announce: true,
        net: NetSettings {
            ptp_group: Ipv4Addr::new(239, 69, 254, 250),
            ptp_event_port: base,
            ptp_general_port: base + 1,
            sap_group: Ipv4Addr::new(239, 69, 254, 251),
            sap_port: base + 2,
            ttl: 0,
            multicast_loop: true,
        },
        ..Aes67Settings::default()
    }
}

/// Plays a frame count on the second-last output (and its negative on the
/// last, for the channel order) and keeps what comes in on the last two
/// inputs.
struct Probe {
    frame: u64,
    /// Output `c` (0: the second-last, 1: the last) at frame `f`.
    signal: fn(u64, usize) -> f32,
    heard: Arc<Mutex<Vec<(u64, f32, f32)>>>,
    info: Arc<Mutex<Option<StreamInfo>>>,
}

/// Exact in 24 bits: multiples of 2⁻¹³ below 1.
fn code(f: u64) -> f32 {
    ((f % 8191) as f32 + 1.0) / 8192.0
}

impl AudioCallback for Probe {
    fn prepare(&mut self, info: &StreamInfo) {
        *self.info.lock().unwrap() = Some(info.clone());
    }

    fn process(&mut self, io: &mut dyn DeviceBuffers) {
        let n = io.frames();
        let ins = io.input_channels();
        let mut heard = self.heard.lock().unwrap();
        for i in 0..n {
            heard.push((
                self.frame + i as u64,
                io.input(ins - 2)[i],
                io.input(ins - 1)[i],
            ));
        }
        let outs = io.output_channels();
        for c in 0..outs - 2 {
            io.output(c).fill(0.0);
        }
        for i in 0..n {
            let f = self.frame + i as u64;
            io.output(outs - 2)[i] = (self.signal)(f, 0);
            io.output(outs - 1)[i] = (self.signal)(f, 1);
        }
        self.frame += n as u64;
    }
}

fn counted(f: u64, c: usize) -> f32 {
    if c == 0 { code(f) } else { -code(f) }
}

/// A 50 Hz sine and cosine: their phase says where in its period a frame
/// came from, also after resampling.
const PERIOD: f64 = 960.0;

fn circled(f: u64, c: usize) -> f32 {
    let phase = std::f64::consts::TAU * f as f64 / PERIOD;
    0.5 * if c == 0 { phase.sin() } else { phase.cos() } as f32
}

/// What a probe heard (frame, the last two inputs) and was prepared
/// with.
type Heard = Arc<Mutex<Vec<(u64, f32, f32)>>>;
type Prepared = Arc<Mutex<Option<StreamInfo>>>;

fn probe(signal: fn(u64, usize) -> f32) -> (Probe, Heard, Prepared) {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let info = Arc::new(Mutex::new(None));
    (
        Probe {
            frame: 0,
            signal,
            heard: Arc::clone(&heard),
            info: Arc::clone(&info),
        },
        heard,
        info,
    )
}

fn wait_for(frames: usize, heard: &Mutex<Vec<(u64, f32, f32)>>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while heard.lock().unwrap().len() < frames {
        assert!(Instant::now() < deadline, "the stream did not run");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A device of its own clock (the wall clock) whose thread is realtime
/// where the system allows, like an interface's: the dummy device's
/// thread is not, and a machine busy with builds holds it up for longer
/// than a follower's ring covers.
struct Desk;

struct DeskStream {
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioBackend for Desk {
    fn id(&self) -> &'static str {
        "desk"
    }
    fn display_name(&self) -> &'static str {
        "Desk"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        Ok(Vec::new())
    }
    fn open_stream(
        &mut self,
        config: StreamConfig,
        mut callback: Box<dyn AudioCallback>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let info = StreamInfo {
            backend: "desk",
            device: "Desk".into(),
            sample_rate: config.sample_rate.unwrap_or(48_000),
            buffer_size: config.buffer_size.unwrap_or(256),
            input_channels: config.input_channels,
            output_channels: config.output_channels,
            input_latency: 0,
            output_latency: 0,
        };
        let monitor = StreamMonitor::new(info.sample_rate, info.buffer_size);
        let stop = Arc::new(AtomicBool::new(false));
        let (i, m, st) = (info.clone(), Arc::clone(&monitor), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            faderframe_realtime::priority::make_audio_thread_realtime();
            let mut bufs = OwnedBuffers::new(
                usize::from(i.input_channels),
                usize::from(i.output_channels),
                i.buffer_size as usize,
            );
            callback.prepare(&i);
            let period =
                Duration::from_secs_f64(f64::from(i.buffer_size) / f64::from(i.sample_rate));
            let mut next = Instant::now();
            while !st.load(Ordering::Relaxed) {
                callback.process(&mut bufs);
                m.record_callback();
                next += period;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                }
            }
        });
        Ok(Box::new(DeskStream {
            info,
            monitor,
            stop,
            thread: Some(thread),
        }))
    }
}

impl AudioStream for DeskStream {
    fn info(&self) -> StreamInfo {
        self.info.clone()
    }
    fn status(&self) -> StreamStatus {
        self.monitor.status()
    }
}

impl Drop for DeskStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[test]
// macOS CI runners (shared VMs) keep no 1 ms media clock: their timer
// threads wake late enough for xruns, each resynchronising the streams
// (about 80 ms of silence each; the frames that do arrive are all in place).
#[cfg_attr(
    target_os = "macos",
    ignore = "needs a machine whose timers keep a 1 ms media clock (macOS CI runners do not)"
)]
fn a_stream_sent_comes_back_frame_for_frame() {
    let s = settings(27_300);
    let mut backend = Aes67Backend::new(s.clone());
    let devices = backend.enumerate_devices().unwrap();
    assert_eq!(devices[0].input_channels, 2);
    assert_eq!(devices[0].output_channels, 2);
    assert_eq!(devices[0].input_names[1], "Back · 2");
    let (p, heard, info) = probe(counted);
    let stream = backend
        .open_stream(
            StreamConfig {
                sample_rate: Some(48_000),
                buffer_size: Some(96),
                input_channels: 2,
                output_channels: 2,
                ..StreamConfig::default()
            },
            Box::new(p),
        )
        .unwrap();
    let si = stream.info();
    assert_eq!((si.input_latency, si.output_latency), (480, 480));
    wait_for(48_000, &heard);
    let status = stream.status();
    let devices = stream.devices();
    drop(stream);
    assert!(info.lock().unwrap().is_some());
    assert!(devices[0].master, "the PTP clock first: {devices:?}");
    assert!(devices.iter().any(|d| d.name == "Back" && d.present));
    // Played at stamp N + B, heard back at N' − L − B: 2B + L later.
    let lag = 2 * 96 + 480;
    let heard = heard.lock().unwrap();
    let (mut right, mut silent, mut wrong) = (0, 0, 0);
    for &(f, a, b) in heard.iter().skip(9_600) {
        if a == 0.0 {
            silent += 1;
            continue;
        }
        let want = code(f.wrapping_sub(lag));
        if f >= lag && a == want && b == -want {
            right += 1;
        } else {
            wrong += 1;
        }
    }
    assert_eq!(
        wrong, 0,
        "frames out of place ({right} right, {silent} silent)"
    );
    assert!(
        right > 9 * (right + silent) / 10,
        "{right} right, {silent} silent; {status:?}"
    );
}

#[test]
// macOS CI runners (shared VMs) keep no 1 ms media clock: their timer
// threads wake late enough for xruns, each resynchronising the streams
// (about 80 ms of silence each; the frames that do arrive are all in place).
#[cfg_attr(
    target_os = "macos",
    ignore = "needs a machine whose timers keep a 1 ms media clock (macOS CI runners do not)"
)]
fn streams_join_another_device_as_followers() {
    let s = Aes67Settings {
        alongside: true,
        ..settings(27_400)
    };
    let backends = Alongside::wrap_all(vec![Box::new(Desk)], &s);
    let mut backend = backends.into_iter().next().unwrap();
    assert_eq!(backend.id(), "desk");
    let (p, heard, _) = probe(circled);
    let stream = backend
        .open_stream(
            StreamConfig {
                sample_rate: Some(48_000),
                buffer_size: Some(256),
                input_channels: 2,
                output_channels: 2,
                ..StreamConfig::default()
            },
            Box::new(p),
        )
        .unwrap();
    let info = stream.info();
    // The device's own 2 + 2, the streams' after them.
    assert_eq!((info.input_channels, info.output_channels), (4, 4));
    let routing = stream.routing();
    assert_eq!(routing.inputs[2].as_deref(), Some("Back · 1"));
    assert_eq!(routing.outputs[3].as_deref(), Some("Out · 2"));
    wait_for(10 * 48_000, &heard);
    let devices = stream.devices();
    drop(stream);
    assert!(devices[0].master);
    let back = devices.iter().find(|d| d.name == "Back").unwrap();
    assert_eq!(back.inputs, vec![2, 3]);
    assert!(back.present, "{devices:?}");
    let out = devices.iter().find(|d| d.name == "Out").unwrap();
    assert_eq!(out.outputs, vec![2, 3]);
    // The circle went out through the follower, over the network and
    // back through the other follower: resampled twice (the device's
    // clock and the media clock are both this host's). Once the drift
    // loops have settled it comes back whole, at one steady place.
    let heard = heard.lock().unwrap();
    let mut lags: Vec<(u64, f64)> = Vec::new();
    let mut weak = 0;
    for &(f, a, b) in heard.iter().skip(7 * 48_000) {
        let (a, b) = (f64::from(a), f64::from(b));
        if (a.hypot(b) - 0.5).abs() > 0.01 {
            weak += 1;
            continue;
        }
        let at = a.atan2(b).rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU * PERIOD;
        lags.push((f, (f as f64 - at).rem_euclid(PERIOD)));
    }
    let gaps: u64 = devices.iter().skip(1).map(|d| d.gaps).sum();
    assert!(
        weak < 48,
        "{weak} of {} frames not at full level ({gaps} gaps)",
        lags.len()
    );
    // While the drift loops settle the lag moves, but smoothly: no frame
    // lost or repeated anywhere (that would move it a whole frame).
    let mut worst = 0.0f64;
    for w in lags.windows(2) {
        let ((f0, l0), (f1, l1)) = (w[0], w[1]);
        if f1 == f0 + 1 {
            let d = (l1 - l0 + PERIOD / 2.0).rem_euclid(PERIOD) - PERIOD / 2.0;
            worst = worst.max(d.abs());
        }
    }
    assert!(
        worst < 0.05,
        "the lag jumped {worst:.3} frames between frames"
    );
}
