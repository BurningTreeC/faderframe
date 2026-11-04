//! SAP on the network: listening for other senders' announcements (the
//! streams Preferences offers to receive) and announcing FaderFrame's own
//! every [`sap::INTERVAL_S`] seconds, with a deletion when a stream ends.
//! One thread; announcements are added and withdrawn from the control
//! side.

use crate::net;
use crate::sap::{self, Directory, Heard, Packet};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a discovery service listens on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryConfig {
    pub interface: Ipv4Addr,
    pub group: Ipv4Addr,
    pub port: u16,
    pub ttl: u32,
    pub multicast_loop: bool,
}

struct Own {
    key: String,
    packet: Packet,
    next: Instant,
}

struct Shared {
    directory: Directory,
    own: Vec<Own>,
    /// Deletions to send.
    gone: Vec<Packet>,
}

/// Announcements heard and sent (see the module docs).
pub struct Discovery {
    config: DiscoveryConfig,
    shared: Arc<Mutex<Shared>>,
    changes: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    error: Option<String>,
}

impl std::fmt::Debug for Discovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Discovery")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Discovery {
    pub fn start(config: DiscoveryConfig) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            directory: Directory::default(),
            own: Vec::new(),
            gone: Vec::new(),
        }));
        let changes = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let mut d = Self {
            config,
            shared,
            changes,
            stop,
            thread: None,
            error: None,
        };
        if let Err(e) = d.run() {
            tracing::warn!("SAP: {e}");
            d.error = Some(e.to_string());
        }
        d
    }

    fn run(&mut self) -> std::io::Result<()> {
        let c = &self.config;
        let rx = net::receiver(c.group, c.port, c.interface)?;
        let tx = net::sender(c.interface, 0, c.ttl, c.multicast_loop, 0)?;
        let to = SocketAddrV4::new(c.group, c.port);
        let (shared, changes, stop) = (
            Arc::clone(&self.shared),
            Arc::clone(&self.changes),
            Arc::clone(&self.stop),
        );
        let start = Instant::now();
        let thread = std::thread::Builder::new()
            .name("faderframe-sap".into())
            .spawn(move || {
                let mut buf = vec![0u8; 65_536];
                while !stop.load(Ordering::Relaxed) {
                    let got = rx.recv_from(&mut buf).ok();
                    let now = start.elapsed().as_secs_f64();
                    let mut g = locked(&shared);
                    let mut changed = false;
                    if let Some((len, _)) = got
                        && let Ok(p) = Packet::parse(&buf[..len])
                    {
                        // Our own come back where the group loops: not
                        // listed as others'.
                        let ours = g
                            .own
                            .iter()
                            .any(|o| o.packet.origin == p.origin && o.packet.hash == p.hash);
                        if !ours {
                            changed |= g.directory.heard(&p, now);
                        }
                    }
                    changed |= g.directory.expire(now);
                    send_due(&mut g, &tx, to);
                    drop(g);
                    if changed {
                        changes.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // Gone with the service: say so.
                let mut g = locked(&shared);
                let own: Vec<Packet> = g.own.drain(..).map(|o| o.packet).collect();
                for mut p in own {
                    p.deletion = true;
                    let _ = tx.send_to(&p.to_bytes(), to);
                }
            })?;
        self.thread = Some(thread);
        Ok(())
    }

    pub fn config(&self) -> &DiscoveryConfig {
        &self.config
    }

    /// Why it does not listen, if it does not.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Counts changes of the streams heard (to refresh a list).
    pub fn changes(&self) -> u64 {
        self.changes.load(Ordering::Relaxed)
    }

    /// The streams heard, by name.
    pub fn streams(&self) -> Vec<Heard> {
        let mut v = locked(&self.shared).directory.streams.clone();
        v.sort_by(|a, b| a.description.name.cmp(&b.description.name));
        v
    }

    /// Announce `sdp` under `key` (again at once when it changed).
    pub fn announce(&self, key: &str, sdp: String) {
        let packet = Packet::announce(self.config.interface, sdp);
        let mut g = locked(&self.shared);
        match g.own.iter_mut().find(|o| o.key == key) {
            Some(o) if o.packet.hash == packet.hash => {}
            // A new version of the description (a new hash) replaces the
            // old one where it is heard.
            Some(o) => {
                o.packet = packet;
                o.next = Instant::now();
            }
            None => g.own.push(Own {
                key: key.to_string(),
                packet,
                next: Instant::now(),
            }),
        }
    }

    /// Stop announcing `key` (a deletion goes out).
    pub fn withdraw(&self, key: &str) {
        let mut g = locked(&self.shared);
        if let Some(i) = g.own.iter().position(|o| o.key == key) {
            let mut p = g.own.remove(i).packet;
            p.deletion = true;
            g.gone.push(p);
        }
    }
}

fn send_due(g: &mut Shared, tx: &UdpSocket, to: SocketAddrV4) {
    for p in g.gone.drain(..) {
        let _ = tx.send_to(&p.to_bytes(), to);
    }
    let now = Instant::now();
    for o in &mut g.own {
        if now >= o.next {
            o.next = now + Duration::from_secs(sap::INTERVAL_S);
            let _ = tx.send_to(&o.packet.to_bytes(), to);
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(port: u16) -> DiscoveryConfig {
        DiscoveryConfig {
            interface: Ipv4Addr::LOCALHOST,
            group: Ipv4Addr::new(239, 69, 252, 9),
            port,
            ttl: 0,
            multicast_loop: true,
        }
    }

    fn sdp(name: &str) -> String {
        format!(
            "v=0\r\no=- 42 1 IN IP4 127.0.0.1\r\ns={name}\r\nc=IN IP4 239.69.9.9/32\r\n\
             t=0 0\r\nm=audio 5004 RTP/AVP 96\r\na=rtpmap:96 L24/48000/2\r\n"
        )
    }

    fn wait(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn one_hears_what_another_announces_and_withdraws() {
        let a = Discovery::start(config(29_875));
        let b = Discovery::start(config(29_875));
        assert_eq!(a.error(), None);
        a.announce("s1", sdp("Stage box"));
        wait("heard", || b.streams().len() == 1);
        assert_eq!(b.streams()[0].description.name, "Stage box");
        // Not listed by the one that sent it.
        assert!(a.streams().is_empty());
        a.announce("s1", sdp("Stage box 2"));
        wait("renamed", || {
            b.streams()
                .first()
                .is_some_and(|h| h.description.name == "Stage box 2")
        });
        a.withdraw("s1");
        wait("withdrawn", || b.streams().is_empty());
    }
}
