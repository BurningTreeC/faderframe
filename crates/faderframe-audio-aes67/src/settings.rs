//! What the user chooses for AES67 (Preferences → Network Audio), kept in
//! the preferences file.

use crate::ptp::port::Profile;
use crate::rtp::{Encoding, PacketTime};
use crate::sdp::StreamDescription;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// Where the PTP time comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClockSource {
    /// FaderFrame's own PTP clock follows the network's grandmaster (and
    /// leads when there is none and leading is allowed).
    #[default]
    Ptp,
    /// The system clock, kept on PTP time by another program (linuxptp's
    /// ptp4l with phc2sys, hardware timestamps): the most precise where
    /// the network card stamps packets itself.
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PtpSettings {
    pub domain: u8,
    pub profile: Profile,
    /// Lead the domain when no grandmaster is there (priority 1 of 250:
    /// any real grandmaster wins).
    pub lead: bool,
    pub clock: ClockSource,
}

impl Default for PtpSettings {
    fn default() -> Self {
        Self {
            domain: 0,
            profile: Profile::Media,
            lead: true,
            clock: ClockSource::Ptp,
        }
    }
}

/// A stream FaderFrame sends (the engine's outputs, in order: the first
/// send stream's channels are the first outputs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SendSettings {
    pub name: String,
    pub channels: u16,
    /// The multicast group (`None`: one of 239.69.0.0/16 chosen from the
    /// interface's address and the stream's place).
    pub address: Option<Ipv4Addr>,
    pub port: u16,
    pub enabled: bool,
}

impl Default for SendSettings {
    fn default() -> Self {
        Self {
            name: "FaderFrame".into(),
            channels: 2,
            address: None,
            port: 5004,
            enabled: true,
        }
    }
}

/// A stream FaderFrame receives (the engine's inputs, in order).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReceiveSettings {
    /// [`StreamDescription::key`]: the announced stream it follows (a new
    /// version of its description replaces `sdp`).
    pub key: String,
    pub name: String,
    /// The description as last known (it opens without hearing the
    /// announcement again).
    pub sdp: String,
    pub enabled: bool,
}

impl ReceiveSettings {
    pub fn from_description(d: &StreamDescription, sdp: &str) -> Self {
        Self {
            key: d.key(),
            name: d.name.clone(),
            sdp: sdp.to_string(),
            enabled: true,
        }
    }
}

/// Ports and groups (the standard ones; tests and unusual networks may
/// move them).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetSettings {
    pub ptp_group: Ipv4Addr,
    pub ptp_event_port: u16,
    pub ptp_general_port: u16,
    pub sap_group: Ipv4Addr,
    pub sap_port: u16,
    /// Hops multicast may take (0: this host only).
    pub ttl: u32,
    /// This host's other programs hear what is sent (and so does
    /// FaderFrame itself).
    pub multicast_loop: bool,
}

impl Default for NetSettings {
    fn default() -> Self {
        Self {
            ptp_group: Ipv4Addr::new(224, 0, 1, 129),
            ptp_event_port: 319,
            ptp_general_port: 320,
            sap_group: crate::sap::GROUP,
            sap_port: crate::sap::PORT,
            ttl: 32,
            multicast_loop: true,
        }
    }
}

/// Everything AES67.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Aes67Settings {
    /// The network interface's IPv4 address (`None`: the one the system
    /// routes AES67's multicast range through).
    pub interface: Option<Ipv4Addr>,
    pub ptp: PtpSettings,
    /// The streams' rate (and the engine's, as the audio system).
    pub sample_rate: u32,
    /// Receive: how long after its timestamp a frame is played (covers
    /// the sender's packet time, the network and jitter).
    pub link_offset_ms: f64,
    /// Send: the packet time and sample format of FaderFrame's streams.
    pub packet_time: PacketTime,
    pub encoding: Encoding,
    pub sends: Vec<SendSettings>,
    pub receives: Vec<ReceiveSettings>,
    /// Use the streams beside the audio system chosen on the Audio page
    /// (resampled onto its clock), instead of as the audio system.
    pub alongside: bool,
    /// Announce FaderFrame's streams (SAP) and listen for others'.
    pub announce: bool,
    pub net: NetSettings,
}

impl Default for Aes67Settings {
    fn default() -> Self {
        Self {
            interface: None,
            ptp: PtpSettings::default(),
            sample_rate: 48_000,
            link_offset_ms: 2.0,
            packet_time: PacketTime::Ms1,
            encoding: Encoding::L24,
            sends: vec![SendSettings::default()],
            receives: Vec::new(),
            alongside: false,
            announce: true,
            net: NetSettings::default(),
        }
    }
}

/// Rates AES67 streams run at here.
pub const SAMPLE_RATES: [u32; 3] = [44_100, 48_000, 96_000];
/// Link offsets offered (ms).
pub const LINK_OFFSETS_MS: [f64; 8] = [0.5, 1.0, 2.0, 3.0, 4.0, 5.0, 10.0, 20.0];
/// The longest link offset (ms): the rings hold this and more.
pub const MAX_LINK_OFFSET_MS: f64 = 100.0;

impl Aes67Settings {
    /// The link offset in frames at the streams' rate.
    pub fn link_offset_frames(&self) -> u64 {
        let ms = self.link_offset_ms.clamp(0.0, MAX_LINK_OFFSET_MS);
        (ms * f64::from(self.sample_rate) / 1000.0).round() as u64
    }

    /// Sends and receives that are on.
    pub fn active_sends(&self) -> impl Iterator<Item = &SendSettings> {
        self.sends.iter().filter(|s| s.enabled && s.channels > 0)
    }

    pub fn active_receives(&self) -> impl Iterator<Item = &ReceiveSettings> {
        self.receives.iter().filter(|r| r.enabled)
    }

    pub fn input_channels(&self) -> u16 {
        self.active_receives()
            .filter_map(|r| StreamDescription::parse(&r.sdp).ok())
            .map(|d| d.channels)
            .sum()
    }

    pub fn output_channels(&self) -> u16 {
        self.active_sends().map(|s| s.channels).sum()
    }

    /// Something to send or receive.
    pub fn has_streams(&self) -> bool {
        self.active_sends().next().is_some() || self.active_receives().next().is_some()
    }

    /// The most channels one send stream carries at the chosen packet
    /// time and format.
    pub fn max_send_channels(&self) -> u16 {
        crate::rtp::max_channels(self.encoding, self.packet_time.frames(self.sample_rate)).min(64)
            as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_old_files_read() {
        let mut s = Aes67Settings::default();
        s.sends.push(SendSettings {
            name: "Monitors".into(),
            channels: 8,
            address: Some(Ipv4Addr::new(239, 69, 3, 4)),
            ..SendSettings::default()
        });
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Aes67Settings>(&json).unwrap(), s);
        let old: Aes67Settings = serde_json::from_str(r#"{"link_offset_ms": 4.0}"#).unwrap();
        assert_eq!(old.link_offset_ms, 4.0);
        assert_eq!(old.ptp, PtpSettings::default());
        assert_eq!(old.link_offset_frames(), 192);
        assert_eq!(s.output_channels(), 10);
        assert_eq!(s.max_send_channels(), 10);
    }
}
