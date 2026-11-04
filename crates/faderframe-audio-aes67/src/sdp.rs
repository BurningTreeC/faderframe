//! Stream descriptions (SDP, RFC 4566, with AES67 §8 and RFC 7273's
//! clock attributes): what a sender announces and a receiver needs —
//! where (multicast group, port, source), what (encoding, rate, channels,
//! packet time) and on which clock (`ts-refclk` grandmaster and domain,
//! `mediaclk` offset).
//!
//! Reading is lenient (unknown lines ignored, `\n` or `\r\n`, the first
//! audio stream of several, session- or media-level `c=`); writing gives
//! the attributes Dante's AES67 mode and RAVENNA devices read.

use crate::ptp::message::ClockIdentity;
use crate::rtp::{Encoding, PacketTime};
use std::fmt::Write as _;
use std::net::Ipv4Addr;

/// The `o=` line: who made the description, and which version it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Origin {
    pub username: String,
    pub session_id: u64,
    pub version: u64,
    pub address: String,
}

/// The clock a stream's timestamps follow (`a=ts-refclk`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefClock {
    /// A PTP grandmaster (or "traceable": any one locked to TAI).
    Ptp {
        version: String,
        grandmaster: Option<ClockIdentity>,
        domain: Option<u8>,
    },
    /// A sender's own oscillator, by MAC address.
    LocalMac(String),
    Other(String),
}

/// One AES67 stream.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamDescription {
    pub origin: Origin,
    /// `s=`: the stream's name.
    pub name: String,
    /// `i=` (media level, else session level): a label, often the
    /// channels' names.
    pub info: Option<String>,
    pub keywords: Option<String>,
    /// Where the stream goes (a multicast group, or this host unicast).
    pub address: Ipv4Addr,
    pub ttl: Option<u8>,
    pub port: u16,
    /// Only packets from this host (`a=source-filter: incl`).
    pub source: Option<Ipv4Addr>,
    pub payload_type: u8,
    pub encoding: Encoding,
    pub sample_rate: u32,
    pub channels: u16,
    /// Frames per packet (from `a=framecount`, else `a=ptime`).
    pub frames_per_packet: usize,
    pub refclk: Option<RefClock>,
    /// `a=mediaclk:direct=<offset>`: RTP timestamp = media clock + offset.
    pub media_offset: u32,
    /// PTP domain (`ts-refclk`'s or `a=clock-domain:PTPv2 n`).
    pub domain: Option<u8>,
}

/// Why a description cannot be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SdpError {
    #[error("no audio stream (m=audio) in the description")]
    NoAudio,
    #[error("no connection address (c=) for the stream")]
    NoAddress,
    #[error("{0} streams are not AES67 PCM (L16/L24)")]
    Encoding(String),
    #[error("the payload type has no a=rtpmap")]
    NoRtpmap,
    #[error("unreadable line: {0}")]
    Line(String),
}

fn parse_connection(value: &str) -> Option<(Ipv4Addr, Option<u8>)> {
    // "IN IP4 239.69.161.58/32[/n]"
    let mut it = value.split_whitespace();
    if it.next()? != "IN" || it.next()? != "IP4" {
        return None;
    }
    let mut parts = it.next()?.split('/');
    let addr = parts.next()?.parse().ok()?;
    let ttl = parts.next().and_then(|t| t.parse().ok());
    Some((addr, ttl))
}

fn parse_refclk(value: &str) -> (RefClock, Option<u8>) {
    if let Some(rest) = value.strip_prefix("ptp=") {
        let mut parts = rest.split(':');
        let version = parts.next().unwrap_or("").to_string();
        let second = parts.next();
        let (grandmaster, domain) = match second {
            Some("traceable") | None => (None, None),
            Some(gm) => {
                let domain = parts.next().and_then(|d| {
                    d.trim()
                        .trim_start_matches("domain-nmbr=")
                        .parse::<u8>()
                        .ok()
                });
                (ClockIdentity::parse(gm), domain)
            }
        };
        return (
            RefClock::Ptp {
                version,
                grandmaster,
                domain,
            },
            domain,
        );
    }
    if let Some(mac) = value.strip_prefix("localmac=") {
        return (RefClock::LocalMac(mac.trim().to_string()), None);
    }
    (RefClock::Other(value.trim().to_string()), None)
}

impl StreamDescription {
    /// Read a description (see the module docs).
    pub fn parse(text: &str) -> Result<Self, SdpError> {
        let mut origin = Origin::default();
        let mut name = String::new();
        let mut session_info = None;
        let mut media_info = None;
        let mut keywords = None;
        let mut session_c = None;
        let mut media_c = None;
        let mut session_refclk = None;
        let mut media_refclk = None;
        let mut session_offset = None;
        let mut media_offset = None;
        let mut session_source = None;
        let mut media_source = None;
        let mut clock_domain = None;
        let mut port = None;
        let mut payload = None;
        let mut rtpmaps: Vec<(u8, String)> = Vec::new();
        let mut ptime: Option<f64> = None;
        let mut framecount: Option<usize> = None;
        // 0: session level, 1: the first audio stream, 2: past it.
        let mut level = 0;
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            let Some((kind, value)) = line.split_once('=') else {
                continue;
            };
            let kind = kind.trim();
            if kind == "m" {
                if level == 0 && value.starts_with("audio ") {
                    level = 1;
                    let mut it = value.split_whitespace().skip(1);
                    port = it.next().and_then(|p| p.split('/').next()?.parse().ok());
                    let _proto = it.next();
                    payload = it.next().and_then(|p| p.parse::<u8>().ok());
                } else if level == 1 {
                    level = 2;
                }
                continue;
            }
            if level == 2 {
                continue;
            }
            match kind {
                "o" => {
                    let f: Vec<&str> = value.split_whitespace().collect();
                    if f.len() >= 6 {
                        origin = Origin {
                            username: f[0].to_string(),
                            session_id: f[1].parse().unwrap_or(0),
                            version: f[2].parse().unwrap_or(0),
                            address: f[5].to_string(),
                        };
                    }
                }
                "s" => name = value.trim().to_string(),
                "i" if level == 0 => session_info = Some(value.trim().to_string()),
                "i" => media_info = Some(value.trim().to_string()),
                "c" => {
                    let c = parse_connection(value).ok_or_else(|| SdpError::Line(line.into()))?;
                    if level == 0 {
                        session_c = Some(c);
                    } else {
                        media_c = Some(c);
                    }
                }
                "a" => {
                    let (attr, val) = value.split_once(':').unwrap_or((value, ""));
                    match attr {
                        "rtpmap" => {
                            if let Some((pt, enc)) = val.split_once(' ')
                                && let Ok(pt) = pt.trim().parse()
                            {
                                rtpmaps.push((pt, enc.trim().to_string()));
                            }
                        }
                        "ptime" => ptime = val.trim().parse().ok(),
                        "framecount" => framecount = val.trim().parse().ok(),
                        "keywds" => keywords = Some(val.trim().to_string()),
                        "ts-refclk" => {
                            let r = parse_refclk(val.trim());
                            if level == 0 {
                                session_refclk = Some(r);
                            } else {
                                media_refclk = Some(r);
                            }
                        }
                        "mediaclk" => {
                            let offset = val
                                .split_whitespace()
                                .find_map(|p| p.strip_prefix("direct="))
                                .and_then(|o| o.parse::<u64>().ok())
                                .map(|o| o as u32);
                            if level == 0 {
                                session_offset = offset;
                            } else {
                                media_offset = offset;
                            }
                        }
                        "source-filter" => {
                            // " incl IN IP4 <dest> <source>"
                            let f: Vec<&str> = val.split_whitespace().collect();
                            if f.len() >= 5 && f[0] == "incl" {
                                let src = f[4].parse().ok();
                                if level == 0 {
                                    session_source = src;
                                } else {
                                    media_source = src;
                                }
                            }
                        }
                        "clock-domain" => {
                            // "PTPv2 0"
                            clock_domain = val
                                .split_whitespace()
                                .nth(1)
                                .and_then(|d| d.parse::<u8>().ok());
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if level == 0 {
            return Err(SdpError::NoAudio);
        }
        let payload_type = payload.ok_or(SdpError::NoAudio)?;
        let (address, ttl) = media_c.or(session_c).ok_or(SdpError::NoAddress)?;
        let map = rtpmaps
            .iter()
            .find(|(pt, _)| *pt == payload_type)
            .map(|(_, m)| m.as_str())
            .ok_or(SdpError::NoRtpmap)?;
        let mut parts = map.split('/');
        let enc_name = parts.next().unwrap_or("");
        let encoding =
            Encoding::parse(enc_name).ok_or_else(|| SdpError::Encoding(enc_name.into()))?;
        let sample_rate = parts
            .next()
            .and_then(|r| r.parse().ok())
            .ok_or_else(|| SdpError::Line(map.into()))?;
        let channels = parts.next().and_then(|c| c.parse().ok()).unwrap_or(1);
        let frames_per_packet = framecount.unwrap_or_else(|| {
            let ms = ptime.unwrap_or(1.0);
            ((ms * f64::from(sample_rate) / 1000.0).round() as usize).max(1)
        });
        let (refclk, ref_domain) = match media_refclk.or(session_refclk) {
            Some((r, d)) => (Some(r), d),
            None => (None, None),
        };
        Ok(Self {
            origin,
            name,
            info: media_info.or(session_info),
            keywords,
            address,
            ttl,
            port: port.unwrap_or(5004),
            source: media_source.or(session_source),
            payload_type,
            encoding,
            sample_rate,
            channels,
            frames_per_packet,
            refclk,
            media_offset: media_offset.or(session_offset).unwrap_or(0),
            domain: ref_domain.or(clock_domain),
        })
    }

    /// The packet time, if it is one AES67 names.
    pub fn packet_time(&self) -> Option<PacketTime> {
        PacketTime::from_frames(self.frames_per_packet, self.sample_rate)
    }

    /// A key that stays the same across versions of one stream: its
    /// origin's host and session id (else its name and address).
    pub fn key(&self) -> String {
        if self.origin.session_id != 0 || !self.origin.address.is_empty() {
            format!("{}/{}", self.origin.address, self.origin.session_id)
        } else {
            format!("{}@{}:{}", self.name, self.address, self.port)
        }
    }

    /// One line about the stream: its format, where it comes from.
    pub fn summary(&self) -> String {
        let ms = self.frames_per_packet as f64 * 1000.0 / f64::from(self.sample_rate.max(1));
        format!(
            "{} ch · {} · {} · {} ms · {}:{}{}",
            self.channels,
            self.encoding,
            faderframe_audio::format_sample_rate(self.sample_rate),
            (ms * 1000.0).round() / 1000.0,
            self.address,
            self.port,
            if self.origin.address.is_empty() {
                String::new()
            } else {
                format!(" from {}", self.origin.address)
            }
        )
    }

    /// Bytes of payload a packet carries.
    pub fn payload_bytes(&self) -> usize {
        self.frames_per_packet * usize::from(self.channels) * self.encoding.bytes()
    }

    /// The description as text (CRLF lines).
    pub fn write(&self) -> String {
        let mut s = String::new();
        let o = &self.origin;
        let user = if o.username.is_empty() {
            "-"
        } else {
            &o.username
        };
        let _ = write!(
            s,
            "v=0\r\no={user} {} {} IN IP4 {}\r\ns={}\r\n",
            o.session_id, o.version, o.address, self.name
        );
        if let Some(i) = &self.info {
            let _ = write!(s, "i={i}\r\n");
        }
        let c = match self.ttl {
            Some(ttl) if self.address.is_multicast() => format!("{}/{ttl}", self.address),
            _ => self.address.to_string(),
        };
        let _ = write!(s, "c=IN IP4 {c}\r\nt=0 0\r\n");
        if let Some(k) = &self.keywords {
            let _ = write!(s, "a=keywds:{k}\r\n");
        }
        if let Some(d) = self.domain {
            let _ = write!(s, "a=clock-domain:PTPv2 {d}\r\n");
        }
        let _ = write!(s, "m=audio {} RTP/AVP {}\r\n", self.port, self.payload_type);
        let _ = write!(
            s,
            "a=rtpmap:{} {}/{}/{}\r\n",
            self.payload_type, self.encoding, self.sample_rate, self.channels
        );
        let ptime = match self.packet_time() {
            Some(p) => p.sdp().to_string(),
            None => {
                let ms = self.frames_per_packet as f64 * 1000.0 / f64::from(self.sample_rate);
                format!("{ms:.3}")
            }
        };
        let _ = write!(
            s,
            "a=ptime:{ptime}\r\na=framecount:{}\r\n",
            self.frames_per_packet
        );
        if let Some(src) = self.source
            && self.address.is_multicast()
        {
            let _ = write!(s, "a=source-filter: incl IN IP4 {} {src}\r\n", self.address);
        }
        match &self.refclk {
            Some(RefClock::Ptp {
                version,
                grandmaster: Some(gm),
                domain,
            }) => {
                let _ = write!(
                    s,
                    "a=ts-refclk:ptp={version}:{gm}:{}\r\n",
                    domain.or(self.domain).unwrap_or(0)
                );
            }
            Some(RefClock::Ptp { version, .. }) => {
                let _ = write!(s, "a=ts-refclk:ptp={version}:traceable\r\n");
            }
            Some(RefClock::LocalMac(m)) => {
                let _ = write!(s, "a=ts-refclk:localmac={m}\r\n");
            }
            Some(RefClock::Other(o)) => {
                let _ = write!(s, "a=ts-refclk:{o}\r\n");
            }
            None => {}
        }
        let _ = write!(
            s,
            "a=mediaclk:direct={}\r\na=recvonly\r\n",
            self.media_offset
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As a Dante device in AES67 mode announces a two-channel flow.
    const DANTE: &str = "v=0\r
o=- 1311738121 1311738121 IN IP4 192.168.1.30\r
s=AVIO-USB-12bd4 : 2\r
c=IN IP4 239.69.161.58/32\r
t=0 0\r
a=keywds:Dante\r
m=audio 5004 RTP/AVP 97\r
i=2 channels: Left, Right\r
a=recvonly\r
a=rtpmap:97 L24/48000/2\r
a=ptime:1\r
a=ts-refclk:ptp=IEEE1588-2008:00-1D-C1-FF-FE-12-34-56:0\r
a=mediaclk:direct=0\r
";

    /// A RAVENNA device: clock domain at session level, a media-clock
    /// offset, a source filter, LF line ends, a second (MIDI) stream.
    const RAVENNA: &str = "v=0
o=- 1423986 1423994 IN IP4 169.254.98.63
s=AOIP44-serial-1614 : 2
c=IN IP4 239.65.125.63/32
t=0 0
a=clock-domain:PTPv2 0
m=audio 5004 RTP/AVP 98
c=IN IP4 239.65.125.63/32
a=rtpmap:98 L16/96000/8
a=sync-time:0
a=framecount:12
a=palign:0
a=ptime:0.125
a=source-filter: incl IN IP4 239.65.125.63 169.254.98.63
a=ts-refclk:ptp=IEEE1588-2008:00-1D-C1-FF-FE-51-9E-F7:0
a=mediaclk:direct=1266592
a=recvonly
m=application 5006 RTP/AVP 99
a=rtpmap:99 rtp-midi/31250
";

    /// A unicast stream with a "traceable" reference and no ptime.
    const PLAIN: &str = "v=0
o=studio 7 1 IN IP4 10.0.0.5
s=Desk mix
c=IN IP4 10.0.0.9
t=0 0
m=audio 6000 RTP/AVP 96
a=rtpmap:96 L24/44100
a=ts-refclk:ptp=IEEE1588-2008:traceable
";

    #[test]
    fn a_dante_flow_reads() {
        let d = StreamDescription::parse(DANTE).unwrap();
        assert_eq!(d.name, "AVIO-USB-12bd4 : 2");
        assert_eq!(d.origin.session_id, 1_311_738_121);
        assert_eq!(d.origin.address, "192.168.1.30");
        assert_eq!(d.address, Ipv4Addr::new(239, 69, 161, 58));
        assert_eq!(d.ttl, Some(32));
        assert_eq!((d.port, d.payload_type), (5004, 97));
        assert_eq!(
            (d.encoding, d.sample_rate, d.channels),
            (Encoding::L24, 48_000, 2)
        );
        assert_eq!(d.frames_per_packet, 48);
        assert_eq!(d.packet_time(), Some(PacketTime::Ms1));
        assert_eq!(d.info.as_deref(), Some("2 channels: Left, Right"));
        assert_eq!(d.keywords.as_deref(), Some("Dante"));
        assert_eq!(d.domain, Some(0));
        assert_eq!(
            d.refclk,
            Some(RefClock::Ptp {
                version: "IEEE1588-2008".into(),
                grandmaster: ClockIdentity::parse("00-1D-C1-FF-FE-12-34-56"),
                domain: Some(0),
            })
        );
        assert_eq!(d.media_offset, 0);
        assert_eq!(d.key(), "192.168.1.30/1311738121");
        assert_eq!(
            d.summary(),
            "2 ch · L24 · 48 kHz · 1 ms · 239.69.161.58:5004 from 192.168.1.30"
        );
    }

    #[test]
    fn a_ravenna_stream_reads() {
        let d = StreamDescription::parse(RAVENNA).unwrap();
        assert_eq!(
            (d.encoding, d.sample_rate, d.channels),
            (Encoding::L16, 96_000, 8)
        );
        assert_eq!(d.frames_per_packet, 12);
        assert_eq!(d.packet_time(), Some(PacketTime::Us125));
        assert_eq!(d.source, Some(Ipv4Addr::new(169, 254, 98, 63)));
        assert_eq!(d.media_offset, 1_266_592);
        assert_eq!(d.domain, Some(0));
        assert_eq!(d.payload_bytes(), 12 * 8 * 2);
    }

    #[test]
    fn a_plain_unicast_stream_reads() {
        let d = StreamDescription::parse(PLAIN).unwrap();
        assert_eq!(d.address, Ipv4Addr::new(10, 0, 0, 9));
        assert_eq!(d.channels, 1);
        assert_eq!(d.frames_per_packet, 44);
        assert!(matches!(
            d.refclk,
            Some(RefClock::Ptp {
                grandmaster: None,
                ..
            })
        ));
    }

    #[test]
    fn what_is_written_reads_back() {
        let d = StreamDescription::parse(DANTE).unwrap();
        let again = StreamDescription::parse(&d.write()).unwrap();
        assert_eq!(again, d);
        let r = StreamDescription::parse(RAVENNA).unwrap();
        assert_eq!(StreamDescription::parse(&r.write()).unwrap(), r);
        assert!(
            r.write()
                .contains("a=source-filter: incl IN IP4 239.65.125.63 169.254.98.63\r\n")
        );
    }

    #[test]
    fn unusable_descriptions_say_why() {
        assert_eq!(
            StreamDescription::parse("v=0\ns=x\n"),
            Err(SdpError::NoAudio)
        );
        assert_eq!(
            StreamDescription::parse("v=0\nm=audio 5004 RTP/AVP 96\na=rtpmap:96 L24/48000/2\n"),
            Err(SdpError::NoAddress)
        );
        assert_eq!(
            StreamDescription::parse(
                "v=0\nc=IN IP4 239.1.1.1/32\nm=audio 5004 RTP/AVP 96\na=rtpmap:96 AM824/48000/2\n"
            ),
            Err(SdpError::Encoding("AM824".into()))
        );
    }
}
