//! Session announcements (SAP, RFC 2974): how AES67 senders tell the
//! network about their streams — an SDP description sent to
//! 239.255.255.255:9875 every 30 seconds, and a deletion when the stream
//! ends. [`Packet`] is the wire format; [`Directory`] keeps what was
//! heard, forgetting a stream deleted or not announced for ten of its
//! intervals.

use crate::sdp::StreamDescription;
use std::net::Ipv4Addr;

/// The global-scope SAP group and port AES67 devices use.
pub const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 255);
pub const PORT: u16 = 9875;
/// Seconds between announcements of one stream.
pub const INTERVAL_S: u64 = 30;
const MIME: &[u8] = b"application/sdp";

/// A SAP packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub deletion: bool,
    /// The message id hash: changes when the description changes.
    pub hash: u16,
    pub origin: Ipv4Addr,
    pub sdp: String,
}

/// Why bytes are not a SAP packet this host reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SapError {
    #[error("too short")]
    Short,
    #[error("SAP version {0}, not 1")]
    Version(u8),
    #[error("IPv6, encrypted or compressed announcements are not read")]
    Unsupported,
    #[error("the payload is not SDP")]
    NotSdp,
}

/// A 16-bit hash of a description (FNV-1a folded): the message id hash.
pub fn hash(sdp: &str) -> u16 {
    let mut h: u32 = 0x811C_9DC5;
    for b in sdp.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    ((h >> 16) ^ (h & 0xFFFF)) as u16
}

impl Packet {
    pub fn announce(origin: Ipv4Addr, sdp: String) -> Self {
        Self {
            deletion: false,
            hash: hash(&sdp),
            origin,
            sdp,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + MIME.len() + 1 + self.sdp.len());
        // V=1, A=0 (IPv4), R=0, T, E=0, C=0.
        out.push(0x20 | if self.deletion { 0x04 } else { 0 });
        out.push(0); // no authentication
        out.extend_from_slice(&self.hash.to_be_bytes());
        out.extend_from_slice(&self.origin.octets());
        out.extend_from_slice(MIME);
        out.push(0);
        out.extend_from_slice(self.sdp.as_bytes());
        out
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, SapError> {
        if bytes.len() < 8 {
            return Err(SapError::Short);
        }
        let flags = bytes[0];
        let version = flags >> 5;
        if version != 1 {
            return Err(SapError::Version(version));
        }
        // IPv6 origin, encryption, compression.
        if flags & 0x10 != 0 || flags & 0x02 != 0 || flags & 0x01 != 0 {
            return Err(SapError::Unsupported);
        }
        let auth = usize::from(bytes[1]) * 4;
        let hash = u16::from_be_bytes([bytes[2], bytes[3]]);
        let origin = Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]);
        let mut rest = bytes.get(8 + auth..).ok_or(SapError::Short)?;
        // The payload type is optional: a payload starting "v=0" has none.
        if !rest.starts_with(b"v=0") {
            let end = rest.iter().position(|&b| b == 0).ok_or(SapError::NotSdp)?;
            if !rest[..end].eq_ignore_ascii_case(MIME) {
                return Err(SapError::NotSdp);
            }
            rest = &rest[end + 1..];
        }
        let sdp = String::from_utf8_lossy(rest).into_owned();
        Ok(Self {
            deletion: flags & 0x04 != 0,
            hash,
            origin,
            sdp,
        })
    }
}

/// A stream heard announced.
#[derive(Clone, Debug, PartialEq)]
pub struct Heard {
    pub description: StreamDescription,
    /// The SDP text as announced.
    pub sdp: String,
    pub origin: Ipv4Addr,
    pub hash: u16,
    /// Seconds (any monotonic count) of the last announcement, and the
    /// interval seen between the last two.
    pub last: f64,
    pub interval: f64,
}

/// The streams heard announced, by [`StreamDescription::key`].
#[derive(Clone, Debug, Default)]
pub struct Directory {
    pub streams: Vec<Heard>,
}

impl Directory {
    /// A packet arrived at `now` (seconds). Returns whether the list
    /// changed (a stream new, gone or described anew).
    pub fn heard(&mut self, packet: &Packet, now: f64) -> bool {
        let Ok(description) = StreamDescription::parse(&packet.sdp) else {
            // Deletions may carry only the origin line.
            if packet.deletion {
                let before = self.streams.len();
                self.streams
                    .retain(|h| !(h.origin == packet.origin && h.hash == packet.hash));
                return self.streams.len() != before;
            }
            return false;
        };
        let key = description.key();
        let at = self.streams.iter().position(|h| h.description.key() == key);
        if packet.deletion {
            if let Some(i) = at {
                self.streams.remove(i);
                return true;
            }
            return false;
        }
        match at {
            Some(i) => {
                let h = &mut self.streams[i];
                let changed = h.hash != packet.hash || h.description != description;
                h.interval = (now - h.last).max(1.0);
                h.last = now;
                h.hash = packet.hash;
                h.origin = packet.origin;
                if changed {
                    h.description = description;
                    h.sdp = packet.sdp.clone();
                }
                changed
            }
            None => {
                self.streams.push(Heard {
                    description,
                    sdp: packet.sdp.clone(),
                    origin: packet.origin,
                    hash: packet.hash,
                    last: now,
                    interval: INTERVAL_S as f64,
                });
                true
            }
        }
    }

    /// Forget streams not announced for ten of their intervals. Returns
    /// whether any went.
    pub fn expire(&mut self, now: f64) -> bool {
        let before = self.streams.len();
        self.streams.retain(|h| now - h.last <= 10.0 * h.interval);
        self.streams.len() != before
    }

    pub fn find(&self, key: &str) -> Option<&Heard> {
        self.streams.iter().find(|h| h.description.key() == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sdp(name: &str, session: u64, version: u64) -> String {
        format!(
            "v=0\r\no=- {session} {version} IN IP4 192.168.1.30\r\ns={name}\r\n\
             c=IN IP4 239.69.1.{session}/32\r\nt=0 0\r\nm=audio 5004 RTP/AVP 96\r\n\
             a=rtpmap:96 L24/48000/2\r\na=ptime:1\r\na=mediaclk:direct=0\r\n"
        )
    }

    #[test]
    fn packets_round_trip() {
        let p = Packet::announce(Ipv4Addr::new(192, 168, 1, 30), sdp("A", 1, 1));
        let bytes = p.to_bytes();
        assert_eq!(bytes[0], 0x20);
        assert_eq!(Packet::parse(&bytes), Ok(p.clone()));
        let del = Packet {
            deletion: true,
            ..p.clone()
        };
        assert_eq!(Packet::parse(&del.to_bytes()), Ok(del));
        // Without the payload type, with an authentication word.
        let mut raw = vec![0x20, 1, 0x12, 0x34, 10, 0, 0, 1, 0xAA, 0xBB, 0xCC, 0xDD];
        raw.extend_from_slice(sdp("B", 2, 1).as_bytes());
        let q = Packet::parse(&raw).unwrap();
        assert_eq!((q.hash, q.origin), (0x1234, Ipv4Addr::new(10, 0, 0, 1)));
        assert!(q.sdp.starts_with("v=0"));
        assert_eq!(Packet::parse(&[0x00; 8]), Err(SapError::Version(0)));
        assert_eq!(Packet::parse(&[0x30; 8]), Err(SapError::Unsupported));
    }

    #[test]
    fn the_directory_follows_announcements() {
        let host = Ipv4Addr::new(192, 168, 1, 30);
        let mut d = Directory::default();
        assert!(d.heard(&Packet::announce(host, sdp("A", 1, 1)), 0.0));
        assert!(d.heard(&Packet::announce(host, sdp("B", 2, 1)), 1.0));
        // The same again: nothing new.
        assert!(!d.heard(&Packet::announce(host, sdp("A", 1, 1)), 30.0));
        // A new version replaces it.
        assert!(d.heard(&Packet::announce(host, sdp("A renamed", 1, 2)), 60.0));
        assert_eq!(d.streams.len(), 2);
        assert_eq!(
            d.find("192.168.1.30/1").unwrap().description.name,
            "A renamed"
        );
        // Deleted.
        let mut del = Packet::announce(host, sdp("B", 2, 1));
        del.deletion = true;
        assert!(d.heard(&del, 61.0));
        assert_eq!(d.streams.len(), 1);
        // Silent for ten intervals: gone.
        assert!(!d.expire(300.0));
        assert!(d.expire(400.0));
        assert!(d.streams.is_empty());
    }
}
