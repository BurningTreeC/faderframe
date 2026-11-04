//! A small blocking client: what the tests (this crate's and the
//! session's) talk to the server with, as the web app would — the
//! WebSocket handshake, masked text frames out, messages in with a
//! timeout — and plain `GET`s for the app's files.

use crate::secret;
use crate::ws::{self, Message, MessageReader};
use base64::Engine as _;
use serde_json::Value;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// A WebSocket client.
pub struct Client {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    messages: MessageReader,
}

/// A response to a `GET`.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The status line and headers of a response.
fn read_head(r: &mut impl BufRead) -> io::Result<(u16, Vec<(String, String)>)> {
    let mut first = String::new();
    r.read_line(&mut first)?;
    let status = first
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other(format!("not a response: {first:?}")))?;
    let mut headers = Vec::new();
    loop {
        let mut l = String::new();
        if r.read_line(&mut l)? == 0 {
            break;
        }
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((n, v)) = l.split_once(':') {
            headers.push((n.to_string(), v.trim().to_string()));
        }
    }
    Ok((status, headers))
}

/// `GET path` with `host` as the `Host` header.
pub fn get(addr: SocketAddr, path: &str, host: &str) -> io::Result<Response> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n")?;
    let mut r = BufReader::new(s);
    let (status, headers) = read_head(&mut r)?;
    let mut body = Vec::new();
    r.read_to_end(&mut body)?;
    Ok(Response {
        status,
        headers,
        body,
    })
}

impl Client {
    /// Connect to the server at `addr` as a program does (no origin).
    pub fn connect(addr: SocketAddr) -> io::Result<Client> {
        Self::connect_as(addr, &addr.to_string(), None).and_then(|r| {
            r.map_err(|status| io::Error::other(format!("handshake refused: {status}")))
        })
    }

    /// Connect naming `host`, from `origin` (a page's): the client, or the
    /// status the server refused the handshake with.
    pub fn connect_as(
        addr: SocketAddr,
        host: &str,
        origin: Option<&str>,
    ) -> io::Result<Result<Client, u16>> {
        let mut stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let key = base64::engine::general_purpose::STANDARD
            .encode(secret::random_u64()?.to_le_bytes().repeat(2));
        let mut req = format!(
            "GET /ws HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
        );
        if let Some(o) = origin {
            req.push_str(&format!("Origin: {o}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes())?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let (status, headers) = read_head(&mut reader)?;
        if status != 101 {
            return Ok(Err(status));
        }
        let accept = headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("Sec-WebSocket-Accept"))
            .map(|(_, v)| v.as_str());
        if accept != Some(ws::accept_key(&key).as_str()) {
            return Err(io::Error::other("wrong accept key"));
        }
        Ok(Ok(Client {
            stream,
            reader,
            messages: MessageReader::client(1 << 24),
        }))
    }

    /// Send a JSON message.
    pub fn send(&mut self, v: &Value) -> io::Result<()> {
        self.send_text(&v.to_string())
    }

    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        let mask = secret::random_u64()?.to_le_bytes();
        ws::write_frame(
            &mut self.stream,
            ws::TEXT,
            text.as_bytes(),
            Some([mask[0], mask[1], mask[2], mask[3]]),
        )
    }

    /// Send raw bytes (tests of broken frames).
    pub fn send_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes)
    }

    /// The next message within `timeout` (`Ok(None)`: none came). Pings are
    /// answered; a close is an error of kind `ConnectionAborted`.
    pub fn recv(&mut self, timeout: Duration) -> io::Result<Option<Value>> {
        let until = Instant::now() + timeout;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            // Wait for a frame to begin without taking any of it.
            self.reader.get_ref().set_read_timeout(Some(left))?;
            match self.reader.fill_buf() {
                Ok([]) => return Err(io::ErrorKind::ConnectionAborted.into()),
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
            self.reader
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(5)))?;
            let m = self
                .messages
                .read(&mut self.reader)
                .map_err(|e| io::Error::new(io::ErrorKind::ConnectionAborted, e.to_string()))?;
            match m {
                Message::Text(t) => {
                    return serde_json::from_str(&t).map(Some).map_err(io::Error::other);
                }
                Message::Ping(p) => {
                    let mask = secret::random_u64()?.to_le_bytes();
                    ws::write_frame(
                        &mut self.stream,
                        ws::PONG,
                        &p,
                        Some([mask[0], mask[1], mask[2], mask[3]]),
                    )?;
                }
                Message::Close(code) => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        format!("closed ({code:?})"),
                    ));
                }
                Message::Pong(_) | Message::Binary(_) => {}
            }
        }
    }

    /// Messages until one of type `t` (returned) or the timeout (`None`);
    /// the others are dropped.
    pub fn recv_type(&mut self, t: &str, timeout: Duration) -> io::Result<Option<Value>> {
        let until = Instant::now() + timeout;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.recv(left)? {
                Some(v) if v["t"] == t => return Ok(Some(v)),
                Some(_) => {}
                None => return Ok(None),
            }
        }
    }

    /// Close politely.
    pub fn close(mut self) -> io::Result<()> {
        ws::write_frame(
            &mut self.stream,
            ws::CLOSE,
            &ws::close_payload(ws::NORMAL, ""),
            Some([1, 2, 3, 4]),
        )
    }
}

/// The HTTP response code of a raw request (tests of malformed ones).
pub fn raw_request(addr: SocketAddr, bytes: &[u8]) -> io::Result<u16> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(15)))?;
    s.write_all(bytes)?;
    let mut r = BufReader::new(s);
    read_head(&mut r).map(|(status, _)| status)
}
