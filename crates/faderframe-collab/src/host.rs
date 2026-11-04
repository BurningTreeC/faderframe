//! The host's side: a listening socket, a connection per peer.
//!
//! Connections are numbered as they come ([`ConnId`]); the session decides
//! who they are (a new participant or one coming back) when their
//! [`Hello`] arrives, and answers through [`Host::send`]. Media requests
//! are answered on the connection's own threads from the [`Shelf`] of
//! files the session put there, without the session.

use crate::code::InviteCode;
use crate::link::{Flow, Link, Outbox};
use crate::media::{Fetched, Fetches, Sending, Shelf};
use crate::secure::Channel;
use crate::wire::{self, Hello, MediaPart, PeerId, ToHost, ToPeer};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// A connection to the host, by the order it came in.
pub type ConnId = u64;

#[derive(Debug)]
pub enum HostEvent {
    /// A peer with the code said hello.
    Hello {
        conn: ConnId,
        hello: Hello,
        from: SocketAddr,
    },
    Message {
        conn: ConnId,
        message: ToHost,
    },
    Closed {
        conn: ConnId,
        reason: String,
    },
    /// A peer asks for a file that is not on the shelf (yet): answer
    /// with [`Host::serve`] once it is here, or [`Host::missing`].
    WantMedia {
        conn: ConnId,
        key: String,
        from: u64,
    },
    /// A file fetched from a peer ([`Host::fetch`]) arrived, progressed or
    /// failed.
    Media {
        conn: ConnId,
        fetched: Fetched,
    },
    /// Someone was turned away (no code, wrong code, another version).
    Refused {
        from: SocketAddr,
        reason: String,
    },
}

/// A connection's link and the files fetched over it.
struct Conn {
    link: Link,
    fetches: Fetches,
}

type Conns = Arc<Mutex<HashMap<ConnId, Conn>>>;

pub struct Host {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    code: Arc<Mutex<InviteCode>>,
    conns: Conns,
    events: Receiver<HostEvent>,
    shelf: Shelf,
    listener: Option<JoinHandle<()>>,
}

fn media_message(part: MediaPart) -> Option<Vec<u8>> {
    wire::encode(&ToPeer::Media(part)).ok()
}

impl Host {
    /// Listen on `bind` (port 0: any free port) for peers with `code`.
    pub fn start(bind: SocketAddr, code: InviteCode) -> std::io::Result<Self> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let code = Arc::new(Mutex::new(code));
        let conns: Conns = Arc::default();
        let shelf = Shelf::default();
        let (tx, events) = mpsc::channel();
        let listener = {
            let (stop, code, conns, shelf) = (
                Arc::clone(&stop),
                Arc::clone(&code),
                Arc::clone(&conns),
                shelf.clone(),
            );
            std::thread::Builder::new()
                .name("ff-collab-listen".into())
                .spawn(move || listen(&listener, &stop, &code, &conns, &shelf, &tx))?
        };
        Ok(Self {
            addr,
            stop,
            code,
            conns,
            events,
            shelf,
            listener: Some(listener),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// A new invite code: joiners need it from now on (connected peers
    /// stay).
    pub fn set_code(&self, code: InviteCode) {
        *self.code.lock().unwrap_or_else(PoisonError::into_inner) = code;
    }

    pub fn poll(&self) -> Option<HostEvent> {
        self.events.try_recv().ok()
    }

    fn with(&self, conn: ConnId, f: impl FnOnce(&Arc<Outbox>)) {
        if let Some(l) = self
            .conns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&conn)
        {
            f(&l.link.outbox);
        }
    }

    pub fn send(&self, conn: ConnId, message: &ToPeer) {
        match wire::encode(message) {
            Ok(m) => self.with(conn, |o| o.send(m)),
            Err(e) => tracing::warn!("collaboration: {e}"),
        }
    }

    /// Send already encoded bytes (one encoding for every peer).
    pub fn send_bytes(&self, conn: ConnId, message: Vec<u8>) {
        self.with(conn, |o| o.send(message));
    }

    /// Where `of` is now (replaces what of it still waits).
    pub fn send_presence(&self, conn: ConnId, of: PeerId, message: Vec<u8>) {
        self.with(conn, |o| o.send_presence(of, message));
    }

    pub fn send_audio(&self, conn: ConnId, message: Vec<u8>) {
        self.with(conn, |o| o.send_audio(message));
    }

    /// Bytes waiting to go to `conn` in order.
    pub fn waiting(&self, conn: ConnId) -> usize {
        let mut n = 0;
        self.with(conn, |o| n = o.waiting());
        n
    }

    /// Say why and close.
    pub fn close(&self, conn: ConnId, reason: &str) {
        if let Some(l) = self
            .conns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&conn)
        {
            match wire::encode(&ToPeer::Bye {
                reason: reason.into(),
            }) {
                Ok(m) => l.link.outbox.send_last(m),
                Err(_) => l.link.close(),
            }
        }
    }

    /// Send the file at `path` as `key` (an answer to
    /// [`HostEvent::WantMedia`]).
    pub fn serve(&self, conn: ConnId, key: &str, path: &std::path::Path, from: u64) {
        match Sending::open(key, path, from) {
            Ok(s) => self.with(conn, |o| o.send_file(s)),
            Err(_) => self.missing(conn, key),
        }
    }

    /// Tell `conn` the host has no file `key`.
    pub fn missing(&self, conn: ConnId, key: &str) {
        if let Some(m) = media_message(MediaPart::Missing {
            key: key.to_string(),
        }) {
            self.with(conn, |o| o.send(m));
        }
    }

    /// Fetch the file `key` from the peer on `conn` into `dest` (from where
    /// a part kept from an earlier try ends).
    pub fn fetch(&self, conn: ConnId, key: &str, dest: &std::path::Path) {
        let from = {
            let conns = self.conns.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(c) = conns.get(&conn) else {
                return;
            };
            if c.fetches.is_fetching(key) {
                return;
            }
            c.fetches.start(key, dest)
        };
        self.send(
            conn,
            &ToPeer::WantMedia {
                key: key.to_string(),
                from,
            },
        );
    }

    /// Let peers fetch the file at `path` by `key`.
    pub fn offer_media(&self, key: &str, path: &std::path::Path) {
        self.shelf.put(key, path);
    }

    pub fn shelf(&self) -> &Shelf {
        &self.shelf
    }

    /// Close every connection and stop listening.
    pub fn stop(&mut self, reason: &str) {
        self.stop.store(true, Ordering::Relaxed);
        let ids: Vec<ConnId> = self
            .conns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .copied()
            .collect();
        for c in ids {
            self.close(c, reason);
        }
        if let Some(l) = self.listener.take() {
            let _ = l.join();
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop("the host left");
    }
}

fn listen(
    listener: &TcpListener,
    stop: &AtomicBool,
    code: &Arc<Mutex<InviteCode>>,
    conns: &Conns,
    shelf: &Shelf,
    events: &Sender<HostEvent>,
) {
    let mut next: ConnId = 1;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, from)) => {
                let conn = next;
                next += 1;
                let code = code.lock().unwrap_or_else(PoisonError::into_inner).clone();
                let (conns, shelf, events) = (Arc::clone(conns), shelf.clone(), events.clone());
                let _ = std::thread::Builder::new()
                    .name("ff-collab-greet".into())
                    .spawn(move || {
                        if let Err(reason) =
                            greet(stream, from, conn, &code, &conns, &shelf, &events)
                        {
                            let _ = events.send(HostEvent::Refused { from, reason });
                        }
                    });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                tracing::warn!("collaboration: accepting failed: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// The handshake and the Hello, then the connection's threads.
fn greet(
    stream: std::net::TcpStream,
    from: SocketAddr,
    conn: ConnId,
    code: &InviteCode,
    conns: &Conns,
    shelf: &Shelf,
    events: &Sender<HostEvent>,
) -> Result<(), String> {
    stream.set_nonblocking(false).map_err(|e| e.to_string())?;
    let mut channel = Channel::respond(stream, code).map_err(|e| e.to_string())?;
    let first = channel
        .reader
        .read(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let Ok(ToHost::Hello(hello)) = wire::decode::<ToHost>(&first) else {
        return Err("the first message was no hello".into());
    };
    if hello.protocol != wire::PROTOCOL {
        let bye = wire::encode(&ToPeer::Bye {
            reason: format!(
                "the host speaks collaboration protocol {}, {} speaks {}: use the same FaderFrame version",
                wire::PROTOCOL,
                hello.name,
                hello.protocol
            ),
        })
        .map_err(|e| e.to_string())?;
        let _ = channel.writer.write(&bye);
        return Err(format!("{} uses another protocol version", hello.name));
    }
    let ping = wire::encode(&ToPeer::Ping).map_err(|e| e.to_string())?;
    let (ev, ev2, sh) = (events.clone(), events.clone(), shelf.clone());
    let fetches = Fetches::default();
    let (f1, f2) = (fetches.clone(), fetches.clone());
    // Nothing the peer says is passed on before its Hello.
    let (open, gate) = mpsc::channel::<()>();
    let mut gate = Some(gate);
    let link = Link::spawn(
        channel,
        ping,
        media_message,
        move |outbox, bytes| {
            if let Some(g) = gate.take() {
                let _ = g.recv();
            }
            handle(conn, &ev, &sh, &f1, outbox, &bytes)
        },
        move |reason| {
            f2.clear();
            let _ = ev2.send(HostEvent::Closed { conn, reason });
        },
    )
    .map_err(|e| e.to_string())?;
    conns
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(conn, Conn { link, fetches });
    let _ = events.send(HostEvent::Hello { conn, hello, from });
    let _ = open.send(());
    Ok(())
}

/// A message from a peer: pings and media requests answered here, the
/// rest passed on.
fn handle(
    conn: ConnId,
    ev: &Sender<HostEvent>,
    sh: &Shelf,
    fetches: &Fetches,
    outbox: &Arc<Outbox>,
    bytes: &[u8],
) -> Flow {
    match wire::decode::<ToHost>(bytes) {
        Ok(ToHost::Ping) => Flow::Go,
        Ok(ToHost::WantMedia { key, from }) => {
            match sh.get(&key).map(|p| Sending::open(&key, &p, from)) {
                Some(Ok(s)) => outbox.send_file(s),
                // Not here (yet): the session answers.
                _ => {
                    let _ = ev.send(HostEvent::WantMedia { conn, key, from });
                }
            }
            Flow::Go
        }
        Ok(ToHost::Media(part)) => {
            if let Some(fetched) = fetches.receive(part) {
                let _ = ev.send(HostEvent::Media { conn, fetched });
            }
            Flow::Go
        }
        Ok(ToHost::Bye) => {
            let _ = ev.send(HostEvent::Message {
                conn,
                message: ToHost::Bye,
            });
            Flow::Stop
        }
        Ok(message) => {
            let _ = ev.send(HostEvent::Message { conn, message });
            Flow::Go
        }
        Err(e) => {
            tracing::warn!("collaboration: from connection {conn}: {e}");
            Flow::Go
        }
    }
}
