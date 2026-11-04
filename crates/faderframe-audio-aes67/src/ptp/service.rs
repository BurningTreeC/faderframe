//! The PTP clock running: sockets and threads around a [`Port`], or the
//! system clock followed when another program (ptp4l + phc2sys) keeps it
//! on PTP time. The clock is published in a [`ClockMap`] the audio and
//! network threads read.
//!
//! Threads: one receives event messages (port 319) and takes their
//! timestamps the moment `recv` returns — it follows the audio thread a
//! step below, so the stamps are not late by a busy desktop —, one
//! receives general messages (320), one runs the timers. The port sits
//! behind a mutex only these threads and the status display take; the
//! audio side never does. If the ports cannot be opened (an unprivileged
//! program, another PTP daemon) the clock runs free on the system time
//! and the status says why.

use super::clock::{ClockMap, ClockState, VirtualClock, system_tai_ns};
use super::message::{ClockIdentity, Message};
use super::port::{Outgoing, Port, PortConfig, PortStatus};
use super::servo::{Adjust, Servo};
use crate::net;
use crate::settings::{ClockSource, NetSettings, PtpSettings};
use faderframe_audio::follow::now_ns;
use faderframe_realtime::priority::{Helper, follow_audio};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the timers run.
const TICK: Duration = Duration::from_millis(5);
/// How often the system clock is read (System source).
const SYSTEM_EVERY: Duration = Duration::from_millis(100);

/// What a service is started with (equal configs share one service).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceConfig {
    pub interface: Ipv4Addr,
    pub ptp: PtpSettings,
    pub net: NetSettings,
}

/// The clock's state for the status display.
#[derive(Clone, Debug, PartialEq)]
pub struct PtpStatus {
    pub source: ClockSource,
    pub state: ClockState,
    pub interface: Ipv4Addr,
    pub identity: ClockIdentity,
    /// The port's view (PTP source).
    pub port: Option<PortStatus>,
    /// The system clock's offset from the followed time (ns) and its rate
    /// (System source).
    pub system_offset_ns: Option<i64>,
    /// Why the PTP ports are not open.
    pub error: Option<String>,
}

impl PtpStatus {
    /// What the clock is doing, in a line or two.
    pub fn summary(&self) -> String {
        let mut s = format!("Clock {}: {}", self.identity, self.state.label());
        match (&self.port, self.source) {
            (_, ClockSource::System) => {
                if let Some(o) = self.system_offset_ns {
                    s.push_str(&format!(
                        " · system clock read {:+.1} µs off",
                        o as f64 / 1e3
                    ));
                }
            }
            (Some(p), _) => {
                if let Some(gm) = &p.grandmaster
                    && gm.grandmaster != self.identity
                {
                    s.push_str(&format!(
                        "\nGrandmaster {} (priority {}, class {})",
                        gm.grandmaster, gm.priority1, gm.quality.class
                    ));
                }
                if p.state == super::port::PortState::Slave {
                    s.push_str(&format!(
                        " · offset {:+.1} µs · path {} · drift {:+.2} ppm",
                        p.offset_ns as f64 / 1e3,
                        p.path_delay_ns
                            .map_or("measuring…".to_string(), |d| format!(
                                "{:.1} µs",
                                d as f64 / 1e3
                            )),
                        p.drift_ppm
                    ));
                }
                if p.masters_heard > 0 && p.state != super::port::PortState::Slave {
                    s.push_str(&format!(" · {} master(s) heard", p.masters_heard));
                }
            }
            (None, _) => {}
        }
        if let Some(e) = &self.error {
            s.push_str(&format!("\nPTP: {e}"));
        }
        s
    }
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Shared {
    port: Option<Port>,
    /// System source: the servo and its last offset.
    system: Option<(Servo, i64)>,
    error: Option<String>,
}

struct Sockets {
    event_tx: UdpSocket,
    general_tx: UdpSocket,
    event_to: SocketAddrV4,
    general_to: SocketAddrV4,
}

/// A running clock (see the module docs). Dropping it stops its threads.
pub struct PtpService {
    config: ServiceConfig,
    identity: ClockIdentity,
    map: Arc<ClockMap>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for PtpService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtpService")
            .field("config", &self.config)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

/// A clock identity for this process on `interface` (EUI-64 shaped,
/// locally administered: no MAC address needs reading).
pub fn identity_for(interface: Ipv4Addr) -> ClockIdentity {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let host = std::env::var("HOSTNAME").unwrap_or_default();
    let seed = format!(
        "{interface}/{host}/{}/{}",
        std::process::id(),
        system_tai_ns()
    );
    for b in seed.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let b = h.to_be_bytes();
    ClockIdentity([
        (b[0] & 0xFC) | 0x02,
        b[1],
        b[2],
        0xFF,
        0xFE,
        b[5],
        b[6],
        b[7],
    ])
}

fn send_all(port: &mut Port, outs: &mut Vec<Outgoing>, sockets: &Sockets) {
    let mut buf = [0u8; 128];
    let mut i = 0;
    while i < outs.len() {
        let o = outs[i];
        let len = o.message.write(&mut buf);
        let (socket, to) = if o.is_event() {
            (&sockets.event_tx, sockets.event_to)
        } else {
            (&sockets.general_tx, sockets.general_to)
        };
        // Stamped just before it goes (software timestamps).
        let tx = now_ns();
        if socket.send_to(&buf[..len], to).is_ok() && o.stamp {
            port.sent(&o.message, tx, outs);
        }
        i += 1;
    }
    outs.clear();
}

fn publish(map: &ClockMap, port: &mut Port) {
    let stepped = port.take_stepped();
    let clock = port.clock();
    if stepped || map.read() != clock {
        map.publish(clock, stepped);
    }
    map.set_state(port.clock_state());
}

impl PtpService {
    /// Start a clock as `config` says.
    pub fn start(config: ServiceConfig) -> Self {
        let identity = identity_for(config.interface);
        let local = now_ns();
        let map = Arc::new(ClockMap::new(VirtualClock::from_system(local)));
        map.set_state(ClockState::FreeRunning);
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared {
            port: None,
            system: None,
            error: None,
        }));
        let mut service = Self {
            config,
            identity,
            map,
            shared,
            stop,
            threads: Vec::new(),
        };
        match service.config.ptp.clock {
            ClockSource::System => service.start_system(),
            ClockSource::Ptp => {
                if let Err(e) = service.start_ptp(local) {
                    tracing::warn!("PTP: {e}; the clock runs free");
                    locked(&service.shared).error = Some(e.to_string());
                }
            }
        }
        service
    }

    fn spawn(&mut self, name: &str, f: impl FnOnce() + Send + 'static) {
        match std::thread::Builder::new().name(name.into()).spawn(f) {
            Ok(t) => self.threads.push(t),
            Err(e) => locked(&self.shared).error = Some(format!("cannot start {name}: {e}")),
        }
    }

    fn start_ptp(&mut self, local: u64) -> std::io::Result<()> {
        let c = &self.config;
        let n = &c.net;
        let event_rx = net::receiver(n.ptp_group, n.ptp_event_port, c.interface)?;
        let general_rx = net::receiver(n.ptp_group, n.ptp_general_port, c.interface)?;
        let sockets = Arc::new(Sockets {
            event_tx: net::sender(
                c.interface,
                n.ptp_event_port,
                n.ttl,
                n.multicast_loop,
                net::DSCP_EF,
            )?,
            general_tx: net::sender(
                c.interface,
                n.ptp_general_port,
                n.ttl,
                n.multicast_loop,
                net::DSCP_EF,
            )?,
            event_to: SocketAddrV4::new(n.ptp_group, n.ptp_event_port),
            general_to: SocketAddrV4::new(n.ptp_group, n.ptp_general_port),
        });
        let config = PortConfig::new(self.identity, c.ptp.domain, c.ptp.profile, c.ptp.lead);
        locked(&self.shared).port = Some(Port::new(config, self.map.read(), local));
        for (name, socket, helper) in [
            ("faderframe-ptp-event", event_rx, Helper::Near),
            ("faderframe-ptp-general", general_rx, Helper::Ahead),
        ] {
            let (shared, map, stop, sockets) = (
                Arc::clone(&self.shared),
                Arc::clone(&self.map),
                Arc::clone(&self.stop),
                Arc::clone(&sockets),
            );
            self.spawn(name, move || {
                let mut buf = [0u8; 1500];
                let mut outs = Vec::with_capacity(8);
                while !stop.load(Ordering::Relaxed) {
                    follow_audio(helper);
                    let Ok((len, _)) = socket.recv_from(&mut buf) else {
                        continue;
                    };
                    let rx = now_ns();
                    let Ok(msg) = Message::parse(&buf[..len]) else {
                        continue;
                    };
                    let mut g = locked(&shared);
                    if let Some(port) = g.port.as_mut() {
                        port.handle(&msg, rx, &mut outs);
                        send_all(port, &mut outs, &sockets);
                        publish(&map, port);
                    }
                }
            });
        }
        let (shared, map, stop) = (
            Arc::clone(&self.shared),
            Arc::clone(&self.map),
            Arc::clone(&self.stop),
        );
        self.spawn("faderframe-ptp", move || {
            let mut outs = Vec::with_capacity(8);
            while !stop.load(Ordering::Relaxed) {
                follow_audio(Helper::Ahead);
                {
                    let mut g = locked(&shared);
                    if let Some(port) = g.port.as_mut() {
                        port.tick(now_ns(), &mut outs);
                        send_all(port, &mut outs, &sockets);
                        publish(&map, port);
                    }
                }
                std::thread::sleep(TICK);
            }
        });
        Ok(())
    }

    /// Follow the system clock (kept on PTP by another program): the
    /// same servo, fed the system time read beside the local one.
    fn start_system(&mut self) {
        self.map.set_state(ClockState::System);
        locked(&self.shared).system = Some((Servo::new(), 0));
        let (shared, map, stop) = (
            Arc::clone(&self.shared),
            Arc::clone(&self.map),
            Arc::clone(&self.stop),
        );
        self.spawn("faderframe-ptp", move || {
            while !stop.load(Ordering::Relaxed) {
                let local = now_ns();
                let system = system_tai_ns();
                let clock = map.read();
                let offset = clock.ptp(local) as i64 - system as i64;
                {
                    let mut g = locked(&shared);
                    if let Some((servo, last)) = g.system.as_mut() {
                        *last = offset;
                        match servo.sample(offset, local) {
                            Adjust::Hold => {}
                            Adjust::Rate(r) => map.publish(clock.retuned(local, r, 0), false),
                            Adjust::Step { step, rate } => {
                                map.publish(clock.retuned(local, rate, step), true)
                            }
                        }
                    }
                }
                map.set_state(ClockState::System);
                std::thread::sleep(SYSTEM_EVERY);
            }
        });
    }

    pub fn config(&self) -> &ServiceConfig {
        &self.config
    }

    pub fn identity(&self) -> ClockIdentity {
        self.identity
    }

    /// The clock the streams read.
    pub fn clock(&self) -> Arc<ClockMap> {
        Arc::clone(&self.map)
    }

    /// The grandmaster followed (or this clock leading), for stream
    /// descriptions.
    pub fn grandmaster(&self) -> Option<ClockIdentity> {
        let g = locked(&self.shared);
        g.port
            .as_ref()
            .and_then(|p| p.status().grandmaster.map(|d| d.grandmaster))
    }

    pub fn status(&self) -> PtpStatus {
        let g = locked(&self.shared);
        PtpStatus {
            source: self.config.ptp.clock,
            state: self.map.state(),
            interface: self.config.interface,
            identity: self.identity,
            port: g.port.as_ref().map(Port::status),
            system_offset_ns: g.system.as_ref().map(|(_, o)| *o),
            error: g.error.clone(),
        }
    }
}

impl Drop for PtpService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ptp::port::PortState;

    fn config(event: u16, lead: bool) -> ServiceConfig {
        ServiceConfig {
            interface: Ipv4Addr::LOCALHOST,
            ptp: PtpSettings {
                lead,
                ..PtpSettings::default()
            },
            net: NetSettings {
                ptp_group: Ipv4Addr::new(239, 69, 252, 1),
                ptp_event_port: event,
                ptp_general_port: event + 1,
                ttl: 0,
                ..NetSettings::default()
            },
        }
    }

    /// Two clocks on the loopback interface (this host only): one leads,
    /// the other locks to it.
    #[test]
    fn two_clocks_on_loopback_lock() {
        let leader = PtpService::start(config(25_319, true));
        let follower = PtpService::start(config(25_319, false));
        assert_eq!(leader.status().error, None);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let locked = loop {
            let s = follower.status();
            if s.state == ClockState::Locked {
                break s;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "not locked: {s:?} / {:?}",
                leader.status()
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(
            leader.status().port.map(|p| p.state),
            Some(PortState::Master)
        );
        let said = locked.summary();
        assert!(said.contains("locked to the grandmaster"), "{said}");
        assert!(said.contains(&leader.identity().to_string()), "{said}");
        let p = locked.port.unwrap_or_else(|| panic!("a port"));
        assert_eq!(
            p.grandmaster.map(|g| g.grandmaster),
            Some(leader.identity())
        );
        // Both read the same time (one host: a few tens of µs apart).
        let now = now_ns();
        let d = follower.clock().ptp(now) as i64 - leader.clock().ptp(now) as i64;
        assert!(d.abs() < 200_000, "{d} ns apart");
    }

    #[test]
    fn the_system_clock_can_be_followed() {
        let mut c = config(25_419, false);
        c.ptp.clock = ClockSource::System;
        let s = PtpService::start(c);
        std::thread::sleep(Duration::from_millis(1_500));
        assert_eq!(s.status().state, ClockState::System);
        let d = s.clock().now() as i64 - system_tai_ns() as i64;
        assert!(d.abs() < 2_000_000, "{d} ns from the system clock");
    }
}
