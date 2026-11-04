//! The encrypted channel over one TCP connection.
//!
//! **Handshake.** `Noise_NNpsk0_25519_ChaChaPoly_SHA256` with the invite
//! code's key ([`InviteCode::psk`]) as the pre-shared key and the
//! protocol's name as the prologue: two messages, fresh ephemeral keys on
//! both sides (forward secrecy), and a joiner that lacks the code fails at
//! the first message — the host cannot read it and closes the connection.
//! Nobody between the two can read or change what follows, nor pose as
//! the host to someone holding the code.
//!
//! **Frames.** Every Noise message travels as a 2-byte big-endian length
//! and the ciphertext (at most 65535 bytes, the Noise limit). Inside, one
//! byte says whether the frame ends a message; a message larger than a
//! frame is cut into frames and joined again. Each direction counts its
//! own nonces (the stateless transport mode), so the reading and the
//! writing half run on threads of their own.

use crate::code::InviteCode;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

/// The Noise protocol used.
pub const NOISE: &str = "Noise_NNpsk0_25519_ChaChaPoly_SHA256";
/// Bound into the handshake: both sides speak this protocol.
const PROLOGUE: &[u8] = b"FaderFrame collaboration 1";
/// Noise's limit for one message.
const MAX_NOISE: usize = 65535;
/// The authentication tag of each frame.
const TAG: usize = 16;
/// Message bytes in one frame (after the tag and the frame's flag byte).
const FRAME_DATA: usize = MAX_NOISE - TAG - 1;
/// The largest message read (a project snapshot can be large; anything
/// past this is a broken or hostile peer).
pub const MAX_MESSAGE: usize = 1 << 30;
/// How long a handshake may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

fn noise_err(e: snow::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("noise: {e}"))
}

fn write_frame(stream: &mut TcpStream, data: &[u8]) -> io::Result<()> {
    let len = u16::try_from(data.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    let mut out = Vec::with_capacity(2 + data.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(data);
    stream.write_all(&out)
}

fn read_frame(stream: &mut TcpStream, buf: &mut Vec<u8>) -> io::Result<()> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len)?;
    buf.resize(usize::from(u16::from_be_bytes(len)), 0);
    stream.read_exact(buf)
}

/// Both halves of an established channel.
pub struct Channel {
    pub reader: ChannelReader,
    pub writer: ChannelWriter,
}

pub struct ChannelReader {
    stream: TcpStream,
    state: Arc<snow::StatelessTransportState>,
    nonce: u64,
    frame: Vec<u8>,
    plain: Vec<u8>,
}

pub struct ChannelWriter {
    stream: TcpStream,
    state: Arc<snow::StatelessTransportState>,
    nonce: u64,
    frame: Vec<u8>,
}

fn params() -> io::Result<snow::params::NoiseParams> {
    NOISE
        .parse()
        .map_err(|e: snow::Error| io::Error::other(e.to_string()))
}

impl Channel {
    /// The joining side's handshake.
    pub fn initiate(mut stream: TcpStream, code: &InviteCode) -> io::Result<Self> {
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_nodelay(true)?;
        let psk = code.psk();
        let mut hs = snow::Builder::new(params()?)
            .prologue(PROLOGUE)
            .and_then(|b| b.psk(0, &psk))
            .and_then(|b| b.build_initiator())
            .map_err(noise_err)?;
        let mut msg = vec![0u8; MAX_NOISE];
        let n = hs.write_message(&[], &mut msg).map_err(noise_err)?;
        write_frame(&mut stream, &msg[..n])?;
        let mut answer = Vec::new();
        read_frame(&mut stream, &mut answer).map_err(|e| {
            if matches!(
                e.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
            ) {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the host closed the connection: the invite code is wrong or no longer valid",
                )
            } else {
                e
            }
        })?;
        hs.read_message(&answer, &mut msg).map_err(noise_err)?;
        Self::established(stream, hs)
    }

    /// The host's side of a handshake.
    pub fn respond(mut stream: TcpStream, code: &InviteCode) -> io::Result<Self> {
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_nodelay(true)?;
        let psk = code.psk();
        let mut hs = snow::Builder::new(params()?)
            .prologue(PROLOGUE)
            .and_then(|b| b.psk(0, &psk))
            .and_then(|b| b.build_responder())
            .map_err(noise_err)?;
        let mut first = Vec::new();
        read_frame(&mut stream, &mut first)?;
        let mut msg = vec![0u8; MAX_NOISE];
        hs.read_message(&first, &mut msg).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a joiner without the invite code was turned away",
            )
        })?;
        let n = hs.write_message(&[], &mut msg).map_err(noise_err)?;
        write_frame(&mut stream, &msg[..n])?;
        Self::established(stream, hs)
    }

    fn established(stream: TcpStream, hs: snow::HandshakeState) -> io::Result<Self> {
        stream.set_read_timeout(None)?;
        let state = Arc::new(hs.into_stateless_transport_mode().map_err(noise_err)?);
        Ok(Self {
            reader: ChannelReader {
                stream: stream.try_clone()?,
                state: Arc::clone(&state),
                nonce: 0,
                frame: Vec::new(),
                plain: vec![0u8; MAX_NOISE],
            },
            writer: ChannelWriter {
                stream,
                state,
                nonce: 0,
                frame: vec![0u8; MAX_NOISE],
            },
        })
    }
}

impl ChannelReader {
    /// The next whole message (blocks; `timeout` bounds the wait for each
    /// frame).
    pub fn read(&mut self, timeout: Option<Duration>) -> io::Result<Vec<u8>> {
        self.stream.set_read_timeout(timeout)?;
        let mut message = Vec::new();
        loop {
            read_frame(&mut self.stream, &mut self.frame)?;
            let n = self
                .state
                .read_message(self.nonce, &self.frame, &mut self.plain)
                .map_err(noise_err)?;
            self.nonce += 1;
            let Some((&flag, data)) = self.plain[..n].split_first() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "empty frame"));
            };
            if message.len() + data.len() > MAX_MESSAGE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a message larger than allowed",
                ));
            }
            message.extend_from_slice(data);
            if flag == 1 {
                return Ok(message);
            }
        }
    }

    /// Unblock a read and end the connection (from another thread).
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    pub fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.stream.peer_addr()
    }

    /// A handle that can end the connection from elsewhere.
    pub fn stopper(&self) -> io::Result<TcpStream> {
        self.stream.try_clone()
    }
}

impl ChannelWriter {
    /// Send one message (cut into frames as needed).
    pub fn write(&mut self, message: &[u8]) -> io::Result<()> {
        let mut chunks = message.chunks(FRAME_DATA).peekable();
        let mut plain = Vec::with_capacity(FRAME_DATA + 1);
        let mut out = Vec::new();
        if chunks.peek().is_none() {
            return self.write_frame(&[1], &mut out);
        }
        while let Some(chunk) = chunks.next() {
            plain.clear();
            plain.push(u8::from(chunks.peek().is_none()));
            plain.extend_from_slice(chunk);
            self.write_frame(&plain, &mut out)?;
        }
        Ok(())
    }

    fn write_frame(&mut self, plain: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        let n = self
            .state
            .write_message(self.nonce, plain, &mut self.frame)
            .map_err(noise_err)?;
        self.nonce += 1;
        out.clear();
        out.extend_from_slice(&(n as u16).to_be_bytes());
        out.extend_from_slice(&self.frame[..n]);
        self.stream.write_all(out)
    }

    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn pair(host: &InviteCode, guest: &InviteCode) -> (io::Result<Channel>, io::Result<Channel>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let host = host.clone();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            Channel::respond(s, &host)
        });
        let c = Channel::initiate(TcpStream::connect(addr).unwrap(), guest);
        (t.join().unwrap(), c)
    }

    #[test]
    fn messages_of_any_size_cross_both_ways() {
        let code = InviteCode::new().unwrap();
        let (h, c) = pair(&code, &code);
        let (mut h, mut c) = (h.unwrap(), c.unwrap());
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7) as u8).collect();
        for m in [&b""[..], b"hello", &big[..FRAME_DATA], &big] {
            c.writer.write(m).unwrap();
            assert_eq!(h.reader.read(None).unwrap(), m);
            h.writer.write(m).unwrap();
            assert_eq!(c.reader.read(None).unwrap(), m);
        }
    }

    #[test]
    fn a_wrong_code_is_turned_away() {
        let (h, c) = pair(&InviteCode::new().unwrap(), &InviteCode::new().unwrap());
        let h = h.err().unwrap();
        assert_eq!(h.kind(), io::ErrorKind::PermissionDenied);
        assert!(c.is_err());
    }
}
