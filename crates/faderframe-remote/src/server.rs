//! The server: a thread accepting connections, a thread per connection
//! reading it (HTTP for the app's files, then the WebSocket), and a writer
//! thread per WebSocket fed through a bounded queue — the session never
//! waits on the network. What paired devices send arrives as [`Event`]s
//! for the session to take on its own thread ([`Server::poll`]); what it
//! sends goes out with [`Server::send`], which drops (and says so) when a
//! client's queue is full rather than block.
//!
//! Who is answered: peers on the local network only (unless
//! [`ServerConfig::any_network`]), requests naming this computer (no
//! public host names: DNS rebinding), WebSocket handshakes from the app's
//! own origin (or from programs, which send none), and at most
//! [`MAX_CONNECTIONS`] at once. A WebSocket says nothing but `pair` and
//! `hello` until it is paired; a wrong code costs a second, five end the
//! connection, and an unpaired connection is dropped after two minutes.

use crate::pairing::{PairError, PairedDevice, Pairing, Role};
use crate::protocol::{self, ClientMessage, RemoteInput, Screen};
use crate::ws::{self, Message, MessageReader, WsError};
use crate::{http, net, web};
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, BufReader, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Connections open at once (the app's files and WebSockets).
pub const MAX_CONNECTIONS: usize = 32;
/// Messages waiting for a client before more are dropped.
const QUEUE: usize = 256;
/// Events waiting for the session (a connection waits beyond that).
const EVENTS: usize = 4096;
/// How long a request's headers may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// A write that takes longer ends the connection.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Pings to an idle WebSocket.
const PING_EVERY: Duration = Duration::from_secs(15);
/// Nothing heard for this long: the client is gone.
const IDLE: Duration = Duration::from_secs(45);
/// An unpaired WebSocket is dropped after this.
const UNPAIRED: Duration = Duration::from_secs(120);
/// What a wrong code or token costs (the connection waits).
const WRONG_DELAY: Duration = Duration::from_secs(1);
/// Wrong codes a connection may try.
const WRONG_PER_CONNECTION: u32 = 5;

/// A connection's number (for this run of the server).
pub type ClientId = u64;

/// Where and to whom the server answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    /// The address listened on (`0.0.0.0`: the network; `127.0.0.1`: this
    /// computer only).
    pub bind: IpAddr,
    /// 0: any free port (tests).
    pub port: u16,
    /// Answer peers outside the local network too (a VPN's addresses).
    pub any_network: bool,
}

/// What happened, for the session.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A paired device connected (or a device just paired).
    Joined {
        client: ClientId,
        device: u64,
    },
    Input {
        client: ClientId,
        input: RemoteInput,
    },
    View {
        client: ClientId,
        screen: Screen,
    },
    /// A joined client went.
    Left {
        client: ClientId,
    },
    /// A device paired: keep it.
    Paired {
        device: PairedDevice,
    },
}

/// A connected WebSocket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    pub client: ClientId,
    /// The device, once paired.
    pub device: Option<u64>,
    pub peer: SocketAddr,
}

enum Out {
    Text(String),
    Pong(Vec<u8>),
    Close(u16, String),
}

struct Conn {
    device: Option<u64>,
    tx: SyncSender<Out>,
    stream: TcpStream,
    peer: SocketAddr,
}

struct Shared {
    config: ServerConfig,
    stop: AtomicBool,
    pairing: Mutex<Pairing>,
    conns: Mutex<HashMap<ClientId, Conn>>,
    next: AtomicU64,
    open: AtomicUsize,
    events: SyncSender<Event>,
    started: Instant,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn millis(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

/// The running server (stopped when dropped).
pub struct Server {
    shared: Arc<Shared>,
    events: Receiver<Event>,
    addr: SocketAddr,
    accept: Option<JoinHandle<()>>,
}

impl Server {
    /// Listen as `config` says, with the devices paired before and a code
    /// for `role`.
    pub fn start(
        config: ServerConfig,
        devices: Vec<PairedDevice>,
        role: Role,
    ) -> io::Result<Server> {
        let listener = TcpListener::bind((config.bind, config.port))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let (events_tx, events) = mpsc::sync_channel(EVENTS);
        let shared = Arc::new(Shared {
            config,
            stop: AtomicBool::new(false),
            pairing: Mutex::new(Pairing::new(devices, role)),
            conns: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            open: AtomicUsize::new(0),
            events: events_tx,
            started: Instant::now(),
        });
        let s = Arc::clone(&shared);
        let accept = thread::Builder::new()
            .name("ff-remote".into())
            .spawn(move || accept_loop(&s, listener))?;
        Ok(Server {
            shared,
            events,
            addr,
            accept: Some(accept),
        })
    }

    /// Where it listens.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// What happened since the last call.
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Send `text` to a client: `false` when it is gone or its queue is
    /// full (the message is dropped).
    pub fn send(&self, client: ClientId, text: String) -> bool {
        lock(&self.shared.conns)
            .get(&client)
            .is_some_and(|c| c.tx.try_send(Out::Text(text)).is_ok())
    }

    /// The WebSockets connected now.
    pub fn clients(&self) -> Vec<ClientInfo> {
        let mut v: Vec<ClientInfo> = lock(&self.shared.conns)
            .iter()
            .map(|(id, c)| ClientInfo {
                client: *id,
                device: c.device,
                peer: c.peer,
            })
            .collect();
        v.sort_by_key(|c| c.client);
        v
    }

    /// The pairing code offered now.
    pub fn code(&self) -> Option<String> {
        lock(&self.shared.pairing).code().map(str::to_string)
    }

    pub fn code_role(&self) -> Role {
        lock(&self.shared.pairing).code_role().clone()
    }

    /// Wrong codes tried since the code was drawn.
    pub fn wrong_codes(&self) -> u32 {
        lock(&self.shared.pairing).wrong_codes()
    }

    /// Changes whenever the code or the devices do.
    pub fn pairing_revision(&self) -> u64 {
        lock(&self.shared.pairing).revision()
    }

    /// Offer a new code for `role`.
    pub fn new_code(&self, role: Role) {
        lock(&self.shared.pairing).new_code(role);
    }

    pub fn devices(&self) -> Vec<PairedDevice> {
        lock(&self.shared.pairing).devices().to_vec()
    }

    /// The devices as the settings have them: a device no longer among
    /// them is disconnected.
    pub fn set_devices(&self, devices: Vec<PairedDevice>) {
        let gone: Vec<u64> = {
            let mut p = lock(&self.shared.pairing);
            let gone = p
                .devices()
                .iter()
                .filter(|d| !devices.iter().any(|n| n.id == d.id))
                .map(|d| d.id)
                .collect();
            p.set_devices(devices);
            gone
        };
        for id in gone {
            self.disconnect_device(id, "forgotten");
        }
    }

    /// Say `bye` to a device's connections and close them.
    pub fn disconnect_device(&self, device: u64, why: &str) {
        for c in lock(&self.shared.conns).values() {
            if c.device == Some(device) {
                let _ =
                    c.tx.try_send(Out::Text(protocol::message("bye", json!({ "why": why }))));
                if c.tx.try_send(Out::Close(ws::POLICY, why.into())).is_err() {
                    let _ = c.stream.shutdown(Shutdown::Both);
                }
            }
        }
    }

    /// Close a client.
    pub fn close(&self, client: ClientId) {
        if let Some(c) = lock(&self.shared.conns).get(&client)
            && c.tx
                .try_send(Out::Close(ws::NORMAL, String::new()))
                .is_err()
        {
            let _ = c.stream.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        for c in lock(&self.shared.conns).values() {
            if c.tx
                .try_send(Out::Close(ws::GOING_AWAY, "stopped".into()))
                .is_err()
            {
                let _ = c.stream.shutdown(Shutdown::Both);
            }
        }
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

fn accept_loop(shared: &Arc<Shared>, listener: TcpListener) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let ip = peer.ip();
                let allowed = net::is_local_peer(ip)
                    || shared.config.any_network
                    || (shared.config.bind.is_loopback() && net::is_this_computer(ip));
                if !allowed {
                    tracing::info!("remote: refused {peer} (not on the local network)");
                    continue;
                }
                if shared.open.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    let mut stream = stream;
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                    let _ = stream.write_all(&http::error(503, "Service Unavailable"));
                    continue;
                }
                shared.open.fetch_add(1, Ordering::SeqCst);
                let s = Arc::clone(shared);
                let spawned =
                    thread::Builder::new()
                        .name("ff-remote-conn".into())
                        .spawn(move || {
                            if let Err(e) = connection(&s, stream, peer) {
                                tracing::debug!("remote: {peer}: {e}");
                            }
                            s.open.fetch_sub(1, Ordering::SeqCst);
                        });
                if spawned.is_err() {
                    shared.open.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(40));
            }
            Err(e) => {
                tracing::debug!("remote: accept: {e}");
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// One connection: a request for a file, or the WebSocket.
fn connection(shared: &Arc<Shared>, stream: TcpStream, peer: SocketAddr) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let req = match http::read_request(&mut reader) {
        Ok(r) => r,
        Err(http::HttpError::Closed) => return Ok(()),
        Err(http::HttpError::TooLarge) => {
            return out.write_all(&http::error(431, "Request Header Fields Too Large"));
        }
        Err(e) => {
            let _ = out.write_all(&http::error(400, "Bad Request"));
            return Err(io::Error::other(e.to_string()));
        }
    };
    let host = req.header("Host").unwrap_or("").to_string();
    if !net::host_allowed(&host) {
        return out.write_all(&http::error(421, "Misdirected Request"));
    }
    if req.is_websocket() {
        if req.path() != "/ws" {
            return out.write_all(&http::error(404, "Not Found"));
        }
        if req.header("Sec-WebSocket-Version").map(str::trim) != Some("13") {
            return out.write_all(&http::response(
                426,
                "Upgrade Required",
                "text/plain; charset=utf-8",
                &[("Sec-WebSocket-Version", "13")],
                b"426 Upgrade Required\n",
            ));
        }
        // A page elsewhere may not talk to the remote (a program sends no
        // origin and still has to pair).
        if let Some(origin) = req.header("Origin") {
            let same = ["http://", "https://"]
                .iter()
                .any(|s| origin.eq_ignore_ascii_case(&format!("{s}{host}")));
            if !same {
                tracing::info!("remote: refused a WebSocket from {origin} ({peer})");
                return out.write_all(&http::error(403, "Forbidden"));
            }
        }
        let key = req.header("Sec-WebSocket-Key").unwrap_or("").trim();
        let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, key);
        if decoded.map(|k| k.len()) != Ok(16) {
            return out.write_all(&http::error(400, "Bad Request"));
        }
        let accept = ws::accept_key(key);
        out.write_all(
            format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                 Sec-WebSocket-Accept: {accept}\r\n\r\n"
            )
            .as_bytes(),
        )?;
        out.flush()?;
        stream.set_read_timeout(None)?;
        return websocket(shared, stream, reader, peer);
    }
    if req.method != "GET" {
        return out.write_all(&http::response(
            405,
            "Method Not Allowed",
            "text/plain; charset=utf-8",
            &[("Allow", "GET")],
            b"405 Method Not Allowed\n",
        ));
    }
    match web::asset(req.path()) {
        Some(a) => {
            let extra: &[(&str, &str)] = if a.content_type.starts_with("text/html") {
                &[
                    ("Content-Security-Policy", web::CSP),
                    ("X-Frame-Options", "DENY"),
                ]
            } else {
                &[]
            };
            out.write_all(&http::response(200, "OK", a.content_type, extra, a.body))
        }
        None => out.write_all(&http::error(404, "Not Found")),
    }
}

/// What a WebSocket connection knows about itself (shared with its
/// writer).
struct Flags {
    /// When something last came (milliseconds of the server's clock).
    heard: AtomicU64,
    joined: AtomicBool,
    opened: u64,
}

fn websocket(
    shared: &Arc<Shared>,
    stream: TcpStream,
    mut reader: BufReader<TcpStream>,
    peer: SocketAddr,
) -> io::Result<()> {
    let id = shared.next.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::sync_channel::<Out>(QUEUE);
    let flags = Arc::new(Flags {
        heard: AtomicU64::new(shared.millis()),
        joined: AtomicBool::new(false),
        opened: shared.millis(),
    });
    lock(&shared.conns).insert(
        id,
        Conn {
            device: None,
            tx: tx.clone(),
            stream: stream.try_clone()?,
            peer,
        },
    );
    let writer = {
        let s = Arc::clone(shared);
        let f = Arc::clone(&flags);
        let stream = stream.try_clone()?;
        thread::Builder::new()
            .name("ff-remote-out".into())
            .spawn(move || write_loop(&s, stream, rx, &f))
    };
    if writer.is_err() {
        lock(&shared.conns).remove(&id);
        let _ = stream.shutdown(Shutdown::Both);
        return Err(io::Error::other("no writer thread"));
    }
    let say = |t: &str, fields: serde_json::Value| {
        let _ = tx.try_send(Out::Text(protocol::message(t, fields)));
    };
    let mut messages = MessageReader::server(protocol::MAX_MESSAGE);
    let mut device: Option<u64> = None;
    let mut wrong = 0u32;
    let join = |d: &PairedDevice, device: &mut Option<u64>| {
        *device = Some(d.id);
        if let Some(c) = lock(&shared.conns).get_mut(&id) {
            c.device = Some(d.id);
        }
        flags.joined.store(true, Ordering::SeqCst);
        say(
            "welcome",
            json!({ "name": d.name, "role": d.role, "version": env!("CARGO_PKG_VERSION") }),
        );
        shared
            .events
            .send(Event::Joined {
                client: id,
                device: d.id,
            })
            .is_ok()
    };
    loop {
        let m = match messages.read(&mut reader) {
            Ok(m) => m,
            Err(WsError::Protocol(code, why)) => {
                let _ = tx.try_send(Out::Close(code, why.into()));
                break;
            }
            Err(_) => break,
        };
        flags.heard.store(shared.millis(), Ordering::SeqCst);
        let text = match m {
            Message::Text(t) => t,
            Message::Binary(_) => {
                let _ = tx.try_send(Out::Close(ws::POLICY, "text only".into()));
                break;
            }
            Message::Ping(p) => {
                let _ = tx.try_send(Out::Pong(p));
                continue;
            }
            Message::Pong(_) => continue,
            Message::Close(_) => {
                let _ = tx.try_send(Out::Close(ws::NORMAL, String::new()));
                break;
            }
        };
        let parsed = match protocol::parse(&text) {
            Ok(p) => p,
            Err(e) => {
                say("notice", json!({ "text": format!("not understood: {e}") }));
                continue;
            }
        };
        match parsed {
            ClientMessage::Pair { code, name } if device.is_none() => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let paired = lock(&shared.pairing).pair(&code, &name, now);
                match paired {
                    Ok((d, token)) => {
                        tracing::info!(
                            "remote: {} paired from {peer} ({})",
                            d.name,
                            d.role.label()
                        );
                        if shared
                            .events
                            .send(Event::Paired { device: d.clone() })
                            .is_err()
                        {
                            break;
                        }
                        say("paired", json!({ "token": token, "name": d.name }));
                        if !join(&d, &mut device) {
                            break;
                        }
                    }
                    Err(e) => {
                        if e == PairError::WrongCode {
                            wrong += 1;
                            thread::sleep(WRONG_DELAY);
                        }
                        say("refused", json!({ "why": e.word() }));
                        if wrong >= WRONG_PER_CONNECTION {
                            let _ = tx.try_send(Out::Close(ws::POLICY, "too many codes".into()));
                            break;
                        }
                    }
                }
            }
            ClientMessage::Hello { token } if device.is_none() => {
                let found = lock(&shared.pairing).authenticate(&token).cloned();
                match found {
                    Some(d) => {
                        if !join(&d, &mut device) {
                            break;
                        }
                    }
                    None => {
                        thread::sleep(WRONG_DELAY);
                        say("refused", json!({ "why": "unknown_token" }));
                    }
                }
            }
            ClientMessage::Pair { .. } | ClientMessage::Hello { .. } => {}
            _ if device.is_none() => say("refused", json!({ "why": "not_paired" })),
            ClientMessage::View { screen } => {
                if shared
                    .events
                    .send(Event::View { client: id, screen })
                    .is_err()
                {
                    break;
                }
            }
            ClientMessage::Input(input) => {
                if shared
                    .events
                    .send(Event::Input { client: id, input })
                    .is_err()
                {
                    break;
                }
            }
        }
    }
    lock(&shared.conns).remove(&id);
    drop(tx);
    if flags.joined.load(Ordering::SeqCst) {
        let _ = shared.events.send(Event::Left { client: id });
    }
    Ok(())
}

fn write_loop(shared: &Arc<Shared>, mut stream: TcpStream, rx: Receiver<Out>, flags: &Flags) {
    let mut last_ping = Instant::now();
    loop {
        let wrote = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Out::Text(t)) => ws::write_frame(&mut stream, ws::TEXT, t.as_bytes(), None),
            Ok(Out::Pong(p)) => ws::write_frame(&mut stream, ws::PONG, &p, None),
            Ok(Out::Close(code, why)) => {
                let _ =
                    ws::write_frame(&mut stream, ws::CLOSE, &ws::close_payload(code, &why), None);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                let now = shared.millis();
                let quiet = now.saturating_sub(flags.heard.load(Ordering::SeqCst));
                if shared.stop.load(Ordering::SeqCst) {
                    let _ = ws::write_frame(
                        &mut stream,
                        ws::CLOSE,
                        &ws::close_payload(ws::GOING_AWAY, "stopped"),
                        None,
                    );
                    break;
                }
                if quiet > IDLE.as_millis() as u64 {
                    break;
                }
                if !flags.joined.load(Ordering::SeqCst)
                    && now.saturating_sub(flags.opened) > UNPAIRED.as_millis() as u64
                {
                    let _ = ws::write_frame(
                        &mut stream,
                        ws::CLOSE,
                        &ws::close_payload(ws::POLICY, "not paired"),
                        None,
                    );
                    break;
                }
                if last_ping.elapsed() >= PING_EVERY {
                    last_ping = Instant::now();
                    ws::write_frame(&mut stream, ws::PING, b"", None)
                } else {
                    Ok(())
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if wrote.is_err() {
            break;
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}
