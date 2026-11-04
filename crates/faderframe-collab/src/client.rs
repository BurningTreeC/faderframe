//! A peer's side: one connection to the host.
//!
//! [`Client::connect`] returns at once; connecting, the handshake and the
//! Hello happen on a thread, and [`ClientEvent::Connected`] or
//! [`ClientEvent::Closed`] says how it went. Media parts are written to
//! their files on the connection's reading thread ([`Fetches`]); the
//! session hears only that a file arrived (or failed).

use crate::code::InviteCode;
use crate::link::{Flow, Link};
use crate::media::{Fetched, Fetches, Sending, Shelf};
use crate::secure::Channel;
use crate::wire::{self, Hello, ToHost, ToPeer};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// How long connecting to an address may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
/// The port a host listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 47_400;

#[derive(Debug)]
pub enum ClientEvent {
    /// The handshake is done and the Hello sent (a Welcome follows).
    Connected {
        to: SocketAddr,
    },
    Message(ToPeer),
    Media(Fetched),
    /// The connection ended (or never came about), and why.
    Closed {
        reason: String,
    },
}

pub struct Client {
    link: Arc<Mutex<Option<Link>>>,
    events: Receiver<ClientEvent>,
    fetches: Fetches,
    shelf: Shelf,
}

/// `host:port`, `host` (the default port), `[v6]:port`, `v6`.
pub fn resolve(address: &str) -> std::io::Result<Vec<SocketAddr>> {
    let a = address.trim();
    if let Ok(sa) = a.parse::<SocketAddr>() {
        return Ok(vec![sa]);
    }
    if let Ok(ip) = a.parse::<std::net::IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, DEFAULT_PORT)]);
    }
    let with_port = match a.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() && !h.contains(':') => a.to_string(),
        _ => format!("{a}:{DEFAULT_PORT}"),
    };
    Ok(with_port.to_socket_addrs()?.collect())
}

fn media_message(part: crate::wire::MediaPart) -> Option<Vec<u8>> {
    wire::encode(&ToHost::Media(part)).ok()
}

impl Client {
    pub fn connect(address: &str, code: InviteCode, hello: Hello) -> Self {
        let (tx, events) = mpsc::channel();
        let link: Arc<Mutex<Option<Link>>> = Arc::default();
        let fetches = Fetches::default();
        let shelf = Shelf::default();
        let (l, f, sh, address) = (
            Arc::clone(&link),
            fetches.clone(),
            shelf.clone(),
            address.to_string(),
        );
        let spawned = std::thread::Builder::new()
            .name("ff-collab-connect".into())
            .spawn({
                let tx = tx.clone();
                move || {
                    if let Err(reason) = open(&address, &code, &hello, &l, &f, &sh, &tx) {
                        let _ = tx.send(ClientEvent::Closed { reason });
                    }
                }
            });
        if let Err(e) = spawned {
            let _ = tx.send(ClientEvent::Closed {
                reason: e.to_string(),
            });
        }
        Self {
            link,
            events,
            fetches,
            shelf,
        }
    }

    /// Let the host fetch the file at `path` by `key` (media brought in
    /// here).
    pub fn offer_media(&self, key: &str, path: &Path) {
        self.shelf.put(key, path);
    }

    pub fn poll(&self) -> Option<ClientEvent> {
        self.events.try_recv().ok()
    }

    fn with(&self, f: impl FnOnce(&Link)) {
        if let Some(l) = self
            .link
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            f(l);
        }
    }

    pub fn is_connected(&self) -> bool {
        let mut on = false;
        self.with(|l| on = !l.outbox.is_closed());
        on
    }

    pub fn send(&self, message: &ToHost) {
        match wire::encode(message) {
            Ok(m) => self.with(|l| l.outbox.send(m)),
            Err(e) => tracing::warn!("collaboration: {e}"),
        }
    }

    pub fn send_presence(&self, message: &ToHost) {
        if let Ok(m) = wire::encode(message) {
            self.with(|l| l.outbox.send_presence(0, m));
        }
    }

    /// Fetch the file `key` into `dest` (from where a part kept from an
    /// earlier try ends).
    pub fn fetch(&self, key: &str, dest: &Path) {
        if self.fetches.is_fetching(key) {
            return;
        }
        let from = self.fetches.start(key, dest);
        self.send(&ToHost::WantMedia {
            key: key.to_string(),
            from,
        });
    }

    pub fn is_fetching(&self, key: &str) -> bool {
        self.fetches.is_fetching(key)
    }

    /// Say goodbye and close.
    pub fn close(&self) {
        if let Ok(m) = wire::encode(&ToHost::Bye) {
            self.with(|l| l.outbox.send_last(m));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.with(Link::close);
    }
}

fn open(
    address: &str,
    code: &InviteCode,
    hello: &Hello,
    link: &Arc<Mutex<Option<Link>>>,
    fetches: &Fetches,
    shelf: &Shelf,
    tx: &Sender<ClientEvent>,
) -> Result<(), String> {
    let addrs = resolve(address).map_err(|e| format!("{address}: {e}"))?;
    let mut last = format!("{address}: no address");
    let mut stream = None;
    for a in &addrs {
        match TcpStream::connect_timeout(a, CONNECT_TIMEOUT) {
            Ok(s) => {
                stream = Some((s, *a));
                break;
            }
            Err(e) => last = format!("{a}: {e}"),
        }
    }
    let (stream, to) = stream.ok_or(last)?;
    let mut channel = Channel::initiate(stream, code).map_err(|e| e.to_string())?;
    channel
        .writer
        .write(&wire::encode(&ToHost::Hello(hello.clone())).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let _ = tx.send(ClientEvent::Connected { to });
    let ping = wire::encode(&ToHost::Ping).map_err(|e| e.to_string())?;
    let (t1, t2, f1, f2, sh) = (
        tx.clone(),
        tx.clone(),
        fetches.clone(),
        fetches.clone(),
        shelf.clone(),
    );
    // Held while the threads start: what the session sends once the
    // Welcome is read waits for the link, it is not lost.
    let mut held = link.lock().unwrap_or_else(PoisonError::into_inner);
    let l = Link::spawn(
        channel,
        ping,
        media_message,
        move |outbox, bytes| match wire::decode::<ToPeer>(&bytes) {
            Ok(ToPeer::Ping) => Flow::Go,
            Ok(ToPeer::WantMedia { key, from }) => {
                match sh.get(&key).map(|p| Sending::open(&key, &p, from)) {
                    Some(Ok(s)) => outbox.send_file(s),
                    _ => {
                        if let Some(m) = media_message(crate::wire::MediaPart::Missing { key }) {
                            outbox.send(m);
                        }
                    }
                }
                Flow::Go
            }
            Ok(ToPeer::Media(part)) => {
                if let Some(f) = f1.receive(part) {
                    let _ = t1.send(ClientEvent::Media(f));
                }
                Flow::Go
            }
            Ok(m @ ToPeer::Bye { .. }) => {
                let _ = t1.send(ClientEvent::Message(m));
                Flow::Stop
            }
            Ok(m) => {
                let _ = t1.send(ClientEvent::Message(m));
                Flow::Go
            }
            Err(e) => {
                tracing::warn!("collaboration: from the host: {e}");
                Flow::Go
            }
        },
        move |reason| {
            f2.clear();
            let _ = t2.send(ClientEvent::Closed { reason });
        },
    )
    .map_err(|e| e.to_string())?;
    *held = Some(l);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_take_the_default_port() {
        let a = resolve("127.0.0.1").unwrap();
        assert_eq!(a, vec![SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT))]);
        let a = resolve("127.0.0.1:5000").unwrap();
        assert_eq!(a[0].port(), 5000);
        let a = resolve("::1").unwrap();
        assert_eq!(a[0].port(), DEFAULT_PORT);
        let a = resolve("[::1]:6000").unwrap();
        assert_eq!(a[0].port(), 6000);
        let a = resolve("localhost:6001").unwrap();
        assert!(a.iter().all(|a| a.port() == 6001 && a.ip().is_loopback()));
    }
}
