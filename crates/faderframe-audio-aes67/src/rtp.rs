//! RTP packets of linear PCM (RFC 3550, RFC 3190 for L24, RFC 3551 for
//! L16): the 12-byte header and big-endian, interleaved samples.
//!
//! AES67 streams are L16 or L24 at 48 kHz (also 44.1 and 96 kHz), with a
//! packet time of 1 ms by default and 125 µs, 250 µs, 333 µs or 4 ms
//! where both ends agree ([`PacketTime`]). A packet carries at most
//! [`MAX_PAYLOAD`] bytes so it fits a 1500-byte Ethernet frame. The RTP
//! timestamp is the media clock (the PTP time in samples, `mediaclk:
//! direct=0`) of the packet's first frame, modulo 2³².

use std::fmt;

/// Bytes of the fixed RTP header.
pub const HEADER_LEN: usize = 12;
/// Payload bytes a packet may carry (AES67 §7.2: 1440 for a 1500-byte
/// MTU with headroom).
pub const MAX_PAYLOAD: usize = 1440;
/// The dynamic payload type used when sending.
pub const PAYLOAD_TYPE: u8 = 96;

/// Sample encoding.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Encoding {
    L16,
    #[default]
    L24,
}

impl Encoding {
    pub fn bytes(self) -> usize {
        match self {
            Encoding::L16 => 2,
            Encoding::L24 => 3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Encoding::L16 => "L16",
            Encoding::L24 => "L24",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_uppercase().as_str() {
            "L16" => Some(Encoding::L16),
            "L24" => Some(Encoding::L24),
            _ => None,
        }
    }

    /// Write `v` (−1…1, clipped) at `out` (`bytes()` long).
    #[inline]
    pub fn put(self, out: &mut [u8], v: f32) {
        let v = if v.is_finite() {
            v.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        match self {
            Encoding::L16 => {
                let s = (v * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16;
                out[..2].copy_from_slice(&s.to_be_bytes());
            }
            Encoding::L24 => {
                let s = (v * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32;
                out[..3].copy_from_slice(&s.to_be_bytes()[1..]);
            }
        }
    }

    /// Read the sample at `b`.
    #[inline]
    pub fn get(self, b: &[u8]) -> f32 {
        match self {
            Encoding::L16 => f32::from(i16::from_be_bytes([b[0], b[1]])) / 32_768.0,
            Encoding::L24 => {
                // Sign-extend through the top byte.
                let s = i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8;
                s as f32 / 8_388_608.0
            }
        }
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The packet times AES67 knows.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum PacketTime {
    Us125,
    Us250,
    Us333,
    #[default]
    Ms1,
    Ms4,
}

impl PacketTime {
    pub const ALL: [PacketTime; 5] = [
        PacketTime::Us125,
        PacketTime::Us250,
        PacketTime::Us333,
        PacketTime::Ms1,
        PacketTime::Ms4,
    ];

    /// Frames a packet carries at `rate` (333 µs is 16 frames at 48 kHz).
    pub fn frames(self, rate: u32) -> usize {
        let per_48k = match self {
            PacketTime::Us125 => 6,
            PacketTime::Us250 => 12,
            PacketTime::Us333 => 16,
            PacketTime::Ms1 => 48,
            PacketTime::Ms4 => 192,
        };
        // 44.1 kHz takes the 48 kHz family's counts scaled (1 ms = 44).
        ((per_48k as u64 * u64::from(rate) + 24_000) / 48_000).max(1) as usize
    }

    /// The SDP `a=ptime` value (milliseconds).
    pub fn sdp(self) -> &'static str {
        match self {
            PacketTime::Us125 => "0.125",
            PacketTime::Us250 => "0.25",
            PacketTime::Us333 => "0.333",
            PacketTime::Ms1 => "1",
            PacketTime::Ms4 => "4",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PacketTime::Us125 => "125 µs",
            PacketTime::Us250 => "250 µs",
            PacketTime::Us333 => "333 µs",
            PacketTime::Ms1 => "1 ms",
            PacketTime::Ms4 => "4 ms",
        }
    }

    /// The packet time a `ptime` (ms) and frame count stand for.
    pub fn from_frames(frames: usize, rate: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.frames(rate) == frames)
    }
}

/// The fixed RTP header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

impl Header {
    pub fn write(&self, out: &mut [u8]) {
        out[0] = 0x80; // version 2, no padding, extension or CSRCs
        out[1] = (u8::from(self.marker) << 7) | (self.payload_type & 0x7F);
        out[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        out[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        out[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
    }
}

/// Why bytes are not an RTP packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RtpError {
    #[error("too short")]
    Short,
    #[error("RTP version {0}, not 2")]
    Version(u8),
    #[error("padding longer than the packet")]
    Padding,
}

/// Read a packet: its header and payload (CSRCs, extension and padding
/// skipped).
pub fn parse(packet: &[u8]) -> Result<(Header, &[u8]), RtpError> {
    if packet.len() < HEADER_LEN {
        return Err(RtpError::Short);
    }
    let version = packet[0] >> 6;
    if version != 2 {
        return Err(RtpError::Version(version));
    }
    let padding = packet[0] & 0x20 != 0;
    let extension = packet[0] & 0x10 != 0;
    let csrcs = usize::from(packet[0] & 0x0F);
    let header = Header {
        marker: packet[1] & 0x80 != 0,
        payload_type: packet[1] & 0x7F,
        sequence: u16::from_be_bytes([packet[2], packet[3]]),
        timestamp: u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
        ssrc: u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]),
    };
    let mut start = HEADER_LEN + 4 * csrcs;
    if extension {
        let words = packet
            .get(start + 2..start + 4)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
            .ok_or(RtpError::Short)?;
        start += 4 + 4 * words;
    }
    let mut end = packet.len();
    if padding {
        let pad = usize::from(packet[end - 1]);
        end = end.checked_sub(pad).ok_or(RtpError::Padding)?;
    }
    if start > end {
        return Err(RtpError::Short);
    }
    Ok((header, &packet[start..end]))
}

/// Write a packet of `frames` frames into `out` (header + payload), sample
/// `(frame, channel)` from `sample`; returns its length.
pub fn write_packet(
    out: &mut [u8],
    header: &Header,
    encoding: Encoding,
    channels: usize,
    frames: usize,
    mut sample: impl FnMut(usize, usize) -> f32,
) -> usize {
    header.write(out);
    let b = encoding.bytes();
    let mut at = HEADER_LEN;
    for f in 0..frames {
        for c in 0..channels {
            encoding.put(&mut out[at..at + b], sample(f, c));
            at += b;
        }
    }
    at
}

/// Read a payload of `channels`-channel frames, giving `(frame, channel,
/// value)` to `put`; returns the frames read.
pub fn read_payload(
    payload: &[u8],
    encoding: Encoding,
    channels: usize,
    mut put: impl FnMut(usize, usize, f32),
) -> usize {
    let b = encoding.bytes();
    let stride = b * channels.max(1);
    let frames = payload.len() / stride;
    for f in 0..frames {
        for c in 0..channels {
            let at = f * stride + c * b;
            put(f, c, encoding.get(&payload[at..at + b]));
        }
    }
    frames
}

/// The most channels a packet of `frames` frames can carry.
pub fn max_channels(encoding: Encoding, frames: usize) -> usize {
    MAX_PAYLOAD / (encoding.bytes() * frames.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_round_trip_within_a_step() {
        for enc in [Encoding::L16, Encoding::L24] {
            let step = 1.0 / (1u32 << (enc.bytes() * 8 - 1)) as f32;
            let mut b = [0u8; 3];
            for v in [-1.0f32, -0.5, -step, 0.0, step, 0.123_456_7, 0.999_99, 1.0] {
                enc.put(&mut b, v);
                let back = enc.get(&b);
                assert!((back - v).abs() <= step, "{enc}: {v} → {back}");
            }
            // Clipped, not wrapped.
            enc.put(&mut b, 3.0);
            assert!(enc.get(&b) > 0.99);
            enc.put(&mut b, f32::NAN);
            assert_eq!(enc.get(&b), 0.0);
        }
        // L24's −1 is 0x800000.
        let mut b = [0u8; 3];
        Encoding::L24.put(&mut b, -1.0);
        assert_eq!(b, [0x80, 0, 0]);
    }

    #[test]
    fn a_packet_round_trips() {
        let header = Header {
            marker: false,
            payload_type: 97,
            sequence: 65_535,
            timestamp: 0xFFFF_FFF0,
            ssrc: 0x1234_5678,
        };
        let mut out = [0u8; HEADER_LEN + MAX_PAYLOAD];
        let frames = PacketTime::Ms1.frames(48_000);
        let len = write_packet(&mut out, &header, Encoding::L24, 8, frames, |f, c| {
            ((f * 8 + c) as f32 * 0.001).sin() * 0.9
        });
        assert_eq!(len, HEADER_LEN + 48 * 8 * 3);
        let (h, payload) = parse(&out[..len]).unwrap();
        assert_eq!(h, header);
        let mut got = vec![0.0f32; 48 * 8];
        let n = read_payload(payload, Encoding::L24, 8, |f, c, v| got[f * 8 + c] = v);
        assert_eq!(n, 48);
        for (i, v) in got.iter().enumerate() {
            let want = (i as f32 * 0.001).sin() * 0.9;
            assert!((v - want).abs() < 1.0 / 8_388_608.0 + 1e-7, "{i}");
        }
    }

    #[test]
    fn csrcs_extensions_and_padding_are_skipped() {
        let mut p = vec![0x80 | 0x20 | 0x10 | 1, 96, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3];
        p.extend_from_slice(&[9, 9, 9, 9]); // a CSRC
        p.extend_from_slice(&[0xBE, 0xDE, 0, 1, 7, 7, 7, 7]); // one extension word
        p.extend_from_slice(&[0x12, 0x34]); // one L16 sample
        p.extend_from_slice(&[0, 0, 3]); // padding of 3
        let (h, payload) = parse(&p).unwrap();
        assert_eq!((h.sequence, h.timestamp, h.ssrc), (1, 2, 3));
        assert_eq!(payload, &[0x12, 0x34]);
        assert_eq!(parse(&[0x40; 12]), Err(RtpError::Version(1)));
        assert_eq!(parse(&[0x80; 4]), Err(RtpError::Short));
    }

    #[test]
    fn packet_times_and_channel_limits() {
        assert_eq!(PacketTime::Us125.frames(48_000), 6);
        assert_eq!(PacketTime::Us333.frames(48_000), 16);
        assert_eq!(PacketTime::Ms1.frames(96_000), 96);
        assert_eq!(PacketTime::Ms1.frames(44_100), 44);
        assert_eq!(PacketTime::Ms4.frames(48_000), 192);
        assert_eq!(PacketTime::from_frames(12, 48_000), Some(PacketTime::Us250));
        // AES67's 1 ms of L24 carries 8 channels with room for 10.
        assert_eq!(max_channels(Encoding::L24, 48), 10);
        assert_eq!(max_channels(Encoding::L16, 48), 15);
        assert_eq!(max_channels(Encoding::L24, 6), 80);
    }
}
