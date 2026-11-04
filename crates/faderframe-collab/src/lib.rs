//! Live collaboration: several people working in one project at once,
//! over a LAN or the internet.
//!
//! One session hosts: it holds the project's authoritative order of edits.
//! Peers join with the host's address and an invite code, send the edits
//! they make, and get every edit back in the host's order (theirs too).
//! This crate is the network half — it knows nothing of projects:
//!
//! * [`code`]: invite codes, the secret of a session.
//! * [`secure`]: the encrypted channel (the Noise protocol, keyed by the
//!   code; nothing else is accepted).
//! * [`wire`]: what host and peers say ([`ToHost`], [`ToPeer`]): the
//!   host's ordered [`Entry`]s, proposals, presence, chat, media, the
//!   host's mix.
//! * [`host`] / [`client`]: the connections, a thread pair each, events
//!   polled by the session.
//! * [`media`]: files fetched by their content key in chunks between
//!   everything else, resumed after a lost connection, checked by SHA-256.
//!
//! No relay server: peers reach the host directly. Over the internet the
//! host forwards its port (TCP [`DEFAULT_PORT`] unless chosen) on its
//! router, or everyone joins one VPN (WireGuard, Tailscale, ZeroTier …).
//!
//! The session side (ordering, rebasing, media by content, presence) is
//! `faderframe-session`'s `collab` module.

#![forbid(unsafe_code)]

pub mod client;
pub mod code;
pub mod host;
mod link;
pub mod media;
pub mod secure;
pub mod wire;

pub use client::{Client, ClientEvent, DEFAULT_PORT, resolve};
pub use code::{CodeError, InviteCode};
pub use host::{ConnId, Host, HostEvent};
pub use link::{PING_EVERY, SILENCE_LIMIT};
pub use media::{Fetched, part_of, sha256, sha256_of};
/// A random number from the operating system (tokens).
pub fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    if getrandom::fill(&mut b).is_err() {
        // No randomness to be had: the time is better than nothing.
        return std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
    }
    u64::from_le_bytes(b)
}

/// This machine's addresses others on its networks can reach it at (for
/// invites; loopback last). Found by asking the routing table which
/// address a datagram socket would send from — nothing is sent.
pub fn local_addresses() -> Vec<std::net::IpAddr> {
    use std::net::{IpAddr, UdpSocket};
    let mut out: Vec<IpAddr> = Vec::new();
    let probes = [
        ("0.0.0.0:0", "10.255.255.255:9"),
        ("0.0.0.0:0", "172.31.255.255:9"),
        ("0.0.0.0:0", "192.168.255.255:9"),
        ("[::]:0", "[fd00::1]:9"),
    ];
    for (bind, to) in probes {
        if let Ok(s) = UdpSocket::bind(bind)
            && s.connect(to).is_ok()
            && let Ok(a) = s.local_addr()
        {
            let ip = a.ip();
            if !ip.is_unspecified() && !ip.is_loopback() && !out.contains(&ip) {
                out.push(ip);
            }
        }
    }
    out.push(IpAddr::from([127, 0, 0, 1]));
    out
}

pub use wire::{
    Entry, EntryBody, Hello, MediaPart, PROTOCOL, Participant, PeerId, Resume, ToHost, ToPeer,
    Welcome,
};
