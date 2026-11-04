//! WebSocket (RFC 6455): the opening handshake's accept key and the
//! framing — frames read and written, fragmented messages joined, control
//! frames answered between them. No extensions are offered, so every RSV
//! bit must be clear. A server reads only masked frames (§5.1); a client
//! (the tests') writes masked ones.

use crate::hash;
use base64::Engine as _;
use std::io::{self, Read, Write};

/// The GUID the accept key is made with (§1.3).
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// `Sec-WebSocket-Accept` for a `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    let mut s = key.trim().to_string();
    s.push_str(GUID);
    base64::engine::general_purpose::STANDARD.encode(hash::sha1(s.as_bytes()))
}

pub const CONTINUATION: u8 = 0x0;
pub const TEXT: u8 = 0x1;
pub const BINARY: u8 = 0x2;
pub const CLOSE: u8 = 0x8;
pub const PING: u8 = 0x9;
pub const PONG: u8 = 0xA;

/// Close codes (§7.4.1).
pub const NORMAL: u16 = 1000;
pub const GOING_AWAY: u16 = 1001;
pub const PROTOCOL_ERROR: u16 = 1002;
pub const NOT_UTF8: u16 = 1007;
pub const POLICY: u16 = 1008;
pub const TOO_BIG: u16 = 1009;

/// A whole message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    /// The peer closes (with its code, if it gave one).
    Close(Option<u16>),
}

/// Why reading stopped.
#[derive(Debug)]
pub enum WsError {
    /// The connection ended without a close frame.
    Closed,
    /// The peer broke the protocol: close with this code.
    Protocol(u16, &'static str),
    Io(io::Error),
}

impl std::fmt::Display for WsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WsError::Closed => write!(f, "connection closed"),
            WsError::Protocol(code, why) => write!(f, "{why} (close {code})"),
            WsError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for WsError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            WsError::Closed
        } else {
            WsError::Io(e)
        }
    }
}

/// One frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: u8,
    pub payload: Vec<u8>,
}

/// Read one frame: its payload unmasked, at most `max` bytes of it.
/// `masked`: the frame must be masked (a server reading a client), or must
/// not be (a client reading a server).
pub fn read_frame(r: &mut impl Read, max: usize, masked: bool) -> Result<Frame, WsError> {
    let mut head = [0u8; 2];
    match r.read(&mut head[..1]) {
        Ok(0) => return Err(WsError::Closed),
        Ok(_) => {}
        Err(e) => return Err(e.into()),
    }
    r.read_exact(&mut head[1..])?;
    let fin = head[0] & 0x80 != 0;
    if head[0] & 0x70 != 0 {
        return Err(WsError::Protocol(PROTOCOL_ERROR, "reserved bits set"));
    }
    let opcode = head[0] & 0x0F;
    if !matches!(opcode, CONTINUATION | TEXT | BINARY | CLOSE | PING | PONG) {
        return Err(WsError::Protocol(PROTOCOL_ERROR, "unknown opcode"));
    }
    let has_mask = head[1] & 0x80 != 0;
    if has_mask != masked {
        return Err(WsError::Protocol(
            PROTOCOL_ERROR,
            if masked {
                "unmasked frame from a client"
            } else {
                "masked frame from a server"
            },
        ));
    }
    let len = match head[1] & 0x7F {
        126 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            u64::from(u16::from_be_bytes(b))
        }
        127 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)?;
            let n = u64::from_be_bytes(b);
            if n >> 63 != 0 {
                return Err(WsError::Protocol(PROTOCOL_ERROR, "length too large"));
            }
            n
        }
        n => u64::from(n),
    };
    let control = opcode >= CLOSE;
    if control && (len > 125 || !fin) {
        return Err(WsError::Protocol(PROTOCOL_ERROR, "bad control frame"));
    }
    if len > max as u64 {
        return Err(WsError::Protocol(TOO_BIG, "message too large"));
    }
    let mut key = [0u8; 4];
    if has_mask {
        r.read_exact(&mut key)?;
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    if has_mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= key[i % 4];
        }
    }
    Ok(Frame {
        fin,
        opcode,
        payload,
    })
}

/// A frame's bytes (`mask`: a client's masking key).
pub fn frame(opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | (opcode & 0x0F));
    let m = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => out.push(m | n as u8),
        n if n <= usize::from(u16::MAX) => {
            out.push(m | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(m | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    match mask {
        Some(key) => {
            out.extend_from_slice(&key);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    out
}

/// A close frame's payload: the code and a reason.
pub fn close_payload(code: u16, reason: &str) -> Vec<u8> {
    let mut p = code.to_be_bytes().to_vec();
    // A control frame holds 125 bytes.
    let mut reason = reason;
    while reason.len() > 123 {
        let mut cut = 123;
        while !reason.is_char_boundary(cut) {
            cut -= 1;
        }
        reason = &reason[..cut];
    }
    p.extend_from_slice(reason.as_bytes());
    p
}

/// Write a frame.
pub fn write_frame(
    w: &mut impl Write,
    opcode: u8,
    payload: &[u8],
    mask: Option<[u8; 4]>,
) -> io::Result<()> {
    w.write_all(&frame(opcode, payload, mask))?;
    w.flush()
}

/// Reads whole messages: data frames joined across continuations (at most
/// `max` bytes in all), control frames handed out as they come.
pub struct MessageReader {
    max: usize,
    masked: bool,
    partial: Option<(u8, Vec<u8>)>,
}

impl MessageReader {
    /// A server's reader (masked frames) of messages up to `max` bytes.
    pub fn server(max: usize) -> Self {
        Self {
            max,
            masked: true,
            partial: None,
        }
    }

    /// A client's reader (unmasked frames).
    pub fn client(max: usize) -> Self {
        Self {
            max,
            masked: false,
            partial: None,
        }
    }

    pub fn read(&mut self, r: &mut impl Read) -> Result<Message, WsError> {
        loop {
            let left = self.max - self.partial.as_ref().map_or(0, |(_, b)| b.len());
            // Control frames may come between fragments: up to 125 bytes.
            let f = read_frame(r, left.max(125), self.masked)?;
            match f.opcode {
                CLOSE => {
                    let code = (f.payload.len() >= 2)
                        .then(|| u16::from_be_bytes([f.payload[0], f.payload[1]]));
                    if f.payload.len() == 1 {
                        return Err(WsError::Protocol(PROTOCOL_ERROR, "bad close frame"));
                    }
                    return Ok(Message::Close(code));
                }
                PING => return Ok(Message::Ping(f.payload)),
                PONG => return Ok(Message::Pong(f.payload)),
                _ => {}
            }
            if f.payload.len() > left {
                return Err(WsError::Protocol(TOO_BIG, "message too large"));
            }
            let (opcode, mut data) = match (f.opcode, self.partial.take()) {
                (CONTINUATION, Some((op, mut data))) => {
                    data.extend_from_slice(&f.payload);
                    (op, data)
                }
                (CONTINUATION, None) => {
                    return Err(WsError::Protocol(PROTOCOL_ERROR, "continuation of nothing"));
                }
                (_, Some(_)) => {
                    return Err(WsError::Protocol(PROTOCOL_ERROR, "message interrupted"));
                }
                (op, None) => (op, f.payload),
            };
            if !f.fin {
                self.partial = Some((opcode, std::mem::take(&mut data)));
                continue;
            }
            return match opcode {
                TEXT => String::from_utf8(data)
                    .map(Message::Text)
                    .map_err(|_| WsError::Protocol(NOT_UTF8, "text is not UTF-8")),
                _ => Ok(Message::Binary(data)),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn the_accept_key_is_the_rfcs() {
        // RFC 6455 §1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn frames_go_round_at_every_length() {
        for n in [0usize, 5, 125, 126, 200, 65535, 65536, 70000] {
            let payload: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
            let bytes = frame(BINARY, &payload, Some([1, 2, 3, 4]));
            let f = read_frame(&mut Cursor::new(&bytes), 1 << 20, true).unwrap();
            assert_eq!(f.payload, payload, "{n}");
            assert!(f.fin);
            let bytes = frame(TEXT, &payload, None);
            let f = read_frame(&mut Cursor::new(&bytes), 1 << 20, false).unwrap();
            assert_eq!(f.payload, payload, "{n}");
        }
        // RFC 6455 §5.7: a masked "Hello".
        let hello = [
            0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        let mut r = MessageReader::server(1024);
        assert_eq!(
            r.read(&mut Cursor::new(&hello)).unwrap(),
            Message::Text("Hello".into())
        );
    }

    #[test]
    fn fragments_join_round_control_frames() {
        let key = Some([9, 8, 7, 6]);
        let mut bytes = Vec::new();
        let mut first = frame(TEXT, b"Hel", key);
        first[0] &= 0x7F; // not final
        bytes.extend(first);
        bytes.extend(frame(PING, b"p", key));
        bytes.extend(frame(CONTINUATION, b"lo", key));
        bytes.extend(frame(CLOSE, &close_payload(NORMAL, "bye"), key));
        let mut c = Cursor::new(bytes);
        let mut r = MessageReader::server(1024);
        assert_eq!(r.read(&mut c).unwrap(), Message::Ping(b"p".to_vec()));
        assert_eq!(r.read(&mut c).unwrap(), Message::Text("Hello".into()));
        assert_eq!(r.read(&mut c).unwrap(), Message::Close(Some(NORMAL)));
        assert!(matches!(r.read(&mut c), Err(WsError::Closed)));
    }

    #[test]
    fn broken_frames_are_refused() {
        let key = Some([1, 1, 1, 1]);
        // Unmasked from a client.
        let plain = frame(TEXT, b"x", None);
        assert!(matches!(
            MessageReader::server(64).read(&mut Cursor::new(plain)),
            Err(WsError::Protocol(PROTOCOL_ERROR, _))
        ));
        // Too large.
        let big = frame(TEXT, &[b'a'; 100], key);
        assert!(matches!(
            MessageReader::server(64).read(&mut Cursor::new(big)),
            Err(WsError::Protocol(TOO_BIG, _))
        ));
        // Too large across fragments.
        let mut two = frame(TEXT, &[b'a'; 40], key);
        two[0] &= 0x7F;
        two.extend(frame(CONTINUATION, &[b'a'; 40], key));
        assert!(matches!(
            MessageReader::server(64).read(&mut Cursor::new(two)),
            Err(WsError::Protocol(TOO_BIG, _))
        ));
        // Reserved bits, a fragmented ping, a stray continuation, bad UTF-8.
        let mut rsv = frame(TEXT, b"x", key);
        rsv[0] |= 0x40;
        let mut ping = frame(PING, b"x", key);
        ping[0] &= 0x7F;
        for bad in [
            rsv,
            ping,
            frame(CONTINUATION, b"x", key),
            frame(TEXT, &[0xff, 0xfe], key),
            frame(0x3, b"x", key),
        ] {
            assert!(matches!(
                MessageReader::server(64).read(&mut Cursor::new(bad)),
                Err(WsError::Protocol(..))
            ));
        }
        assert_eq!(close_payload(1000, &"é".repeat(100)).len(), 124);
    }
}
