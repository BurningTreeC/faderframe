//! PTPv2 messages on the wire (IEEE 1588-2008, clause 13): the common
//! header and the messages a delay request–response clock uses —
//! Announce, Sync, Follow_Up, Delay_Req and Delay_Resp. Everything else
//! (peer delay, signalling, management) is recognised and ignored.
//!
//! Event messages (Sync, Delay_Req) go to UDP port 319, general ones
//! (Announce, Follow_Up, Delay_Resp) to 320, both on the group
//! 224.0.1.129 (Annex D). Multi-byte fields are big-endian.

use std::fmt;

/// `versionPTP`.
pub const VERSION: u8 = 2;
/// Bytes of the common header.
pub const HEADER_LEN: usize = 34;

/// The message types this clock speaks (`messageType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    Sync,
    DelayReq,
    FollowUp,
    DelayResp,
    Announce,
    /// Pdelay_Req/Resp, Signaling, Management: not used here.
    Other(u8),
}

impl MessageType {
    pub fn code(self) -> u8 {
        match self {
            MessageType::Sync => 0x0,
            MessageType::DelayReq => 0x1,
            MessageType::FollowUp => 0x8,
            MessageType::DelayResp => 0x9,
            MessageType::Announce => 0xB,
            MessageType::Other(c) => c & 0x0F,
        }
    }

    pub fn from_code(code: u8) -> Self {
        match code & 0x0F {
            0x0 => MessageType::Sync,
            0x1 => MessageType::DelayReq,
            0x8 => MessageType::FollowUp,
            0x9 => MessageType::DelayResp,
            0xB => MessageType::Announce,
            c => MessageType::Other(c),
        }
    }

    /// Event messages are timestamped and travel on port 319.
    pub fn is_event(self) -> bool {
        matches!(self, MessageType::Sync | MessageType::DelayReq) || self.code() < 0x8
    }

    /// The obsolete `controlField` (13.3.2.10).
    fn control(self) -> u8 {
        match self {
            MessageType::Sync => 0,
            MessageType::DelayReq => 1,
            MessageType::FollowUp => 2,
            MessageType::DelayResp => 3,
            _ => 5,
        }
    }
}

/// A clock's identity: an EUI-64 (from a MAC address with FF-FE in the
/// middle, 7.5.2.2.2).
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClockIdentity(pub [u8; 8]);

impl ClockIdentity {
    /// From a MAC address (FF-FE inserted).
    pub fn from_mac(mac: [u8; 6]) -> Self {
        Self([mac[0], mac[1], mac[2], 0xFF, 0xFE, mac[3], mac[4], mac[5]])
    }

    /// Parse "00-1D-C1-FF-FE-12-34-56" (SDP's `ts-refclk`; colons too).
    pub fn parse(text: &str) -> Option<Self> {
        let mut out = [0u8; 8];
        let mut parts = text.trim().split(['-', ':']);
        for b in &mut out {
            *b = u8::from_str_radix(parts.next()?, 16).ok()?;
        }
        parts.next().is_none().then_some(Self(out))
    }
}

impl fmt::Display for ClockIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("-")?;
            }
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ClockIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A port of a clock (`sourcePortIdentity`, `requestingPortIdentity`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortIdentity {
    pub clock: ClockIdentity,
    pub port: u16,
}

/// A PTP timestamp: TAI nanoseconds since 1970 (seconds are 48 bits on the
/// wire; nanoseconds since the epoch fit a `u64` until 2554).
pub type Timestamp = u64;

/// The quality a grandmaster announces (`ClockQuality`, 5.3.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockQuality {
    pub class: u8,
    pub accuracy: u8,
    pub variance: u16,
}

impl Default for ClockQuality {
    /// A clock that can lead but is not traceable to anything (class 248,
    /// accuracy unknown, variance the largest).
    fn default() -> Self {
        Self {
            class: 248,
            accuracy: 0xFE,
            variance: 0xFFFF,
        }
    }
}

/// `flagField` bits (13.3.2.6), as this clock uses them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub two_step: bool,
    pub unicast: bool,
    pub leap61: bool,
    pub leap59: bool,
    pub utc_offset_valid: bool,
    pub ptp_timescale: bool,
    pub time_traceable: bool,
    pub frequency_traceable: bool,
}

impl Flags {
    fn to_bits(self) -> u16 {
        let mut hi = 0u8;
        if self.two_step {
            hi |= 0x02;
        }
        if self.unicast {
            hi |= 0x04;
        }
        let mut lo = 0u8;
        for (bit, on) in [
            (0x01, self.leap61),
            (0x02, self.leap59),
            (0x04, self.utc_offset_valid),
            (0x08, self.ptp_timescale),
            (0x10, self.time_traceable),
            (0x20, self.frequency_traceable),
        ] {
            if on {
                lo |= bit;
            }
        }
        u16::from_be_bytes([hi, lo])
    }

    fn from_bits(bits: u16) -> Self {
        let [hi, lo] = bits.to_be_bytes();
        Self {
            two_step: hi & 0x02 != 0,
            unicast: hi & 0x04 != 0,
            leap61: lo & 0x01 != 0,
            leap59: lo & 0x02 != 0,
            utc_offset_valid: lo & 0x04 != 0,
            ptp_timescale: lo & 0x08 != 0,
            time_traceable: lo & 0x10 != 0,
            frequency_traceable: lo & 0x20 != 0,
        }
    }
}

/// The common header (13.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: MessageType,
    pub transport_specific: u8,
    pub domain: u8,
    pub flags: Flags,
    /// `correctionField` in nanoseconds × 2¹⁶ (residence and path
    /// corrections of transparent clocks; added to the timestamps).
    pub correction: i64,
    pub source: PortIdentity,
    pub sequence: u16,
    pub log_interval: i8,
}

impl Header {
    pub fn new(kind: MessageType, domain: u8, source: PortIdentity, sequence: u16) -> Self {
        Self {
            kind,
            transport_specific: 0,
            domain,
            flags: Flags::default(),
            correction: 0,
            source,
            sequence,
            log_interval: 0x7F,
        }
    }

    /// The correction in whole nanoseconds (rounded towards zero).
    pub fn correction_ns(&self) -> i64 {
        self.correction / 65_536
    }
}

/// What an Announce says about its grandmaster (13.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Announce {
    pub origin: Timestamp,
    pub utc_offset: i16,
    pub priority1: u8,
    pub quality: ClockQuality,
    pub priority2: u8,
    pub grandmaster: ClockIdentity,
    pub steps_removed: u16,
    pub time_source: u8,
}

/// A PTP message: the header and its body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    Sync {
        origin: Timestamp,
    },
    DelayReq {
        origin: Timestamp,
    },
    FollowUp {
        precise_origin: Timestamp,
    },
    DelayResp {
        receive: Timestamp,
        requesting: PortIdentity,
    },
    Announce(Announce),
    /// A type this clock does not use (its body is not read).
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub header: Header,
    pub body: Body,
}

/// Why bytes are not a message this clock reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("too short")]
    Short,
    #[error("PTP version {0}, not 2")]
    Version(u8),
    #[error("message length {0} does not fit the packet")]
    Length(u16),
}

fn put_timestamp(out: &mut [u8], t: Timestamp) {
    let secs = t / 1_000_000_000;
    let ns = (t % 1_000_000_000) as u32;
    out[..6].copy_from_slice(&secs.to_be_bytes()[2..]);
    out[6..10].copy_from_slice(&ns.to_be_bytes());
}

fn get_timestamp(b: &[u8]) -> Timestamp {
    let mut s = [0u8; 8];
    s[2..].copy_from_slice(&b[..6]);
    let secs = u64::from_be_bytes(s);
    let ns = u32::from_be_bytes([b[6], b[7], b[8], b[9]]).min(999_999_999);
    secs.saturating_mul(1_000_000_000)
        .saturating_add(u64::from(ns))
}

fn put_port(out: &mut [u8], p: PortIdentity) {
    out[..8].copy_from_slice(&p.clock.0);
    out[8..10].copy_from_slice(&p.port.to_be_bytes());
}

fn get_port(b: &[u8]) -> PortIdentity {
    let mut clock = [0u8; 8];
    clock.copy_from_slice(&b[..8]);
    PortIdentity {
        clock: ClockIdentity(clock),
        port: u16::from_be_bytes([b[8], b[9]]),
    }
}

impl Message {
    pub fn new(header: Header, body: Body) -> Self {
        Self { header, body }
    }

    /// Bytes on the wire.
    pub fn len(&self) -> usize {
        HEADER_LEN
            + match self.body {
                Body::Sync { .. } | Body::DelayReq { .. } | Body::FollowUp { .. } => 10,
                Body::DelayResp { .. } => 20,
                Body::Announce(_) => 30,
                Body::Other => 0,
            }
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Write the message into `out` (at least [`Self::len`] bytes);
    /// returns its length.
    pub fn write(&self, out: &mut [u8]) -> usize {
        let len = self.len();
        let out = &mut out[..len];
        out.fill(0);
        let h = &self.header;
        out[0] = (h.transport_specific << 4) | h.kind.code();
        out[1] = VERSION;
        out[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        out[4] = h.domain;
        out[6..8].copy_from_slice(&h.flags.to_bits().to_be_bytes());
        out[8..16].copy_from_slice(&h.correction.to_be_bytes());
        put_port(&mut out[20..30], h.source);
        out[30..32].copy_from_slice(&h.sequence.to_be_bytes());
        out[32] = h.kind.control();
        out[33] = h.log_interval as u8;
        let b = &mut out[HEADER_LEN..];
        match self.body {
            Body::Sync { origin } | Body::DelayReq { origin } => put_timestamp(b, origin),
            Body::FollowUp { precise_origin } => put_timestamp(b, precise_origin),
            Body::DelayResp {
                receive,
                requesting,
            } => {
                put_timestamp(b, receive);
                put_port(&mut b[10..20], requesting);
            }
            Body::Announce(a) => {
                put_timestamp(b, a.origin);
                b[10..12].copy_from_slice(&a.utc_offset.to_be_bytes());
                b[13] = a.priority1;
                b[14] = a.quality.class;
                b[15] = a.quality.accuracy;
                b[16..18].copy_from_slice(&a.quality.variance.to_be_bytes());
                b[18] = a.priority2;
                b[19..27].copy_from_slice(&a.grandmaster.0);
                b[27..29].copy_from_slice(&a.steps_removed.to_be_bytes());
                b[29] = a.time_source;
            }
            Body::Other => {}
        }
        len
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = vec![0u8; self.len()];
        self.write(&mut v);
        v
    }

    /// Read a message (13.3); bodies of types not used here are left out.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        if bytes.len() < HEADER_LEN {
            return Err(ParseError::Short);
        }
        let version = bytes[1] & 0x0F;
        if version != VERSION {
            return Err(ParseError::Version(version));
        }
        let length = u16::from_be_bytes([bytes[2], bytes[3]]);
        if usize::from(length) > bytes.len() || usize::from(length) < HEADER_LEN {
            return Err(ParseError::Length(length));
        }
        let bytes = &bytes[..usize::from(length)];
        let kind = MessageType::from_code(bytes[0]);
        let mut correction = [0u8; 8];
        correction.copy_from_slice(&bytes[8..16]);
        let header = Header {
            kind,
            transport_specific: bytes[0] >> 4,
            domain: bytes[4],
            flags: Flags::from_bits(u16::from_be_bytes([bytes[6], bytes[7]])),
            correction: i64::from_be_bytes(correction),
            source: get_port(&bytes[20..30]),
            sequence: u16::from_be_bytes([bytes[30], bytes[31]]),
            log_interval: bytes[33] as i8,
        };
        let b = &bytes[HEADER_LEN..];
        let need = |n: usize| {
            if b.len() < n {
                Err(ParseError::Short)
            } else {
                Ok(())
            }
        };
        let body = match kind {
            MessageType::Sync => {
                need(10)?;
                Body::Sync {
                    origin: get_timestamp(b),
                }
            }
            MessageType::DelayReq => {
                need(10)?;
                Body::DelayReq {
                    origin: get_timestamp(b),
                }
            }
            MessageType::FollowUp => {
                need(10)?;
                Body::FollowUp {
                    precise_origin: get_timestamp(b),
                }
            }
            MessageType::DelayResp => {
                need(20)?;
                Body::DelayResp {
                    receive: get_timestamp(b),
                    requesting: get_port(&b[10..20]),
                }
            }
            MessageType::Announce => {
                need(30)?;
                let mut gm = [0u8; 8];
                gm.copy_from_slice(&b[19..27]);
                Body::Announce(Announce {
                    origin: get_timestamp(b),
                    utc_offset: i16::from_be_bytes([b[10], b[11]]),
                    priority1: b[13],
                    quality: ClockQuality {
                        class: b[14],
                        accuracy: b[15],
                        variance: u16::from_be_bytes([b[16], b[17]]),
                    },
                    priority2: b[18],
                    grandmaster: ClockIdentity(gm),
                    steps_removed: u16::from_be_bytes([b[27], b[28]]),
                    time_source: b[29],
                })
            }
            MessageType::Other(_) => Body::Other,
        };
        Ok(Self { header, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(last: u8) -> PortIdentity {
        PortIdentity {
            clock: ClockIdentity::from_mac([0x00, 0x1D, 0xC1, 0x12, 0x34, last]),
            port: 1,
        }
    }

    #[test]
    fn messages_round_trip() {
        let mut h = Header::new(MessageType::Announce, 0, port(0x56), 0x1234);
        h.flags.ptp_timescale = true;
        h.flags.utc_offset_valid = true;
        h.log_interval = 1;
        h.correction = -5 * 65_536;
        let announce = Message::new(
            h,
            Body::Announce(Announce {
                origin: 1_700_000_000_123_456_789,
                utc_offset: 37,
                priority1: 128,
                quality: ClockQuality {
                    class: 6,
                    accuracy: 0x21,
                    variance: 0x4E5D,
                },
                priority2: 127,
                grandmaster: port(0x56).clock,
                steps_removed: 0,
                time_source: 0x20,
            }),
        );
        let bytes = announce.to_bytes();
        assert_eq!(bytes.len(), 64);
        assert_eq!(Message::parse(&bytes), Ok(announce));
        assert_eq!(announce.header.correction_ns(), -5);

        let mut h = Header::new(MessageType::Sync, 3, port(1), 7);
        h.flags.two_step = true;
        h.log_interval = -3;
        let sync = Message::new(h, Body::Sync { origin: 42 });
        assert_eq!(sync.to_bytes().len(), 44);
        assert_eq!(Message::parse(&sync.to_bytes()), Ok(sync));

        let resp = Message::new(
            Header::new(MessageType::DelayResp, 0, port(1), 9),
            Body::DelayResp {
                receive: 5_000_000_001,
                requesting: port(9),
            },
        );
        assert_eq!(resp.to_bytes().len(), 54);
        assert_eq!(Message::parse(&resp.to_bytes()), Ok(resp));
    }

    #[test]
    fn a_captured_sync_reads() {
        // A two-step Sync from a grandmaster on domain 0, as captured
        // (logSyncInterval −3: the AES67 media profile's 125 ms).
        let bytes: [u8; 44] = [
            0x00, 0x02, 0x00, 0x2C, 0x00, 0x00, 0x02, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0x00, 0x1D, 0xC1, 0xFF, 0xFE, 0x12, 0x34, 0x56, 0x00, 0x01, 0x0B, 0xB8, 0x00, 0xFD,
            0x00, 0x00, 0x65, 0x53, 0xF1, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let m = Message::parse(&bytes).unwrap();
        assert_eq!(m.header.kind, MessageType::Sync);
        assert!(m.header.flags.two_step && m.header.flags.ptp_timescale);
        assert_eq!(m.header.sequence, 3000);
        assert_eq!(m.header.log_interval, -3);
        assert_eq!(
            m.header.source.clock,
            ClockIdentity::parse("00-1D-C1-FF-FE-12-34-56").unwrap()
        );
        // The origin is approximate in a two-step Sync (Follow_Up has it).
        assert_eq!(
            m.body,
            Body::Sync {
                origin: 1_700_000_000_000_000_000
            }
        );
        assert!(m.header.kind.is_event());
        assert!(!MessageType::FollowUp.is_event());
    }

    #[test]
    fn bad_packets_are_refused() {
        assert_eq!(Message::parse(&[0u8; 10]), Err(ParseError::Short));
        let mut v1 = [0u8; 44];
        v1[1] = 1;
        assert_eq!(Message::parse(&v1), Err(ParseError::Version(1)));
        let mut long = Message::new(
            Header::new(MessageType::Sync, 0, port(1), 1),
            Body::Sync { origin: 0 },
        )
        .to_bytes();
        long[3] = 200;
        assert_eq!(Message::parse(&long), Err(ParseError::Length(200)));
        assert_eq!(ClockIdentity::parse("00-1D"), None);
        assert_eq!(
            ClockIdentity::parse("00:1d:c1:ff:fe:12:34:56").map(|c| c.to_string()),
            Some("00-1D-C1-FF-FE-12-34-56".into())
        );
    }
}
