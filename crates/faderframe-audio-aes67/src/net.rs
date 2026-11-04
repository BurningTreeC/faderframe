//! Sockets: multicast receivers joined on the chosen interface, senders
//! that leave through it, with the DSCP classes AES67 recommends (PTP
//! events EF, media AF41) and address reuse so other programs on the host
//! can listen to the same groups.
//!
//! On Linux a receiver binds to its group (and leaves `IP_MULTICAST_ALL`
//! off), so several streams on port 5004 never see each other's packets;
//! elsewhere it binds to the port and receivers also check the sender.

use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

/// DSCP classes (AES67 §D.2), as the IPv4 TOS byte.
pub const DSCP_EF: u32 = 46 << 2;
pub const DSCP_AF41: u32 = 34 << 2;

/// How long a receiving socket blocks before its thread looks at its stop
/// flag.
pub const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// The interface address the system routes `towards` through (connecting
/// a UDP socket sends nothing; it only asks the routing table). `None`
/// without a route.
pub fn route_to(towards: Ipv4Addr) -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((towards, 5004)).ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(a) if !a.is_unspecified() => Some(a),
        _ => None,
    }
}

/// The interface to use: the chosen one, else the route to AES67's
/// multicast range, else loopback.
pub fn interface(chosen: Option<Ipv4Addr>) -> Ipv4Addr {
    chosen
        .or_else(|| route_to(Ipv4Addr::new(239, 69, 0, 1)))
        .unwrap_or(Ipv4Addr::LOCALHOST)
}

/// The host's IPv4 addresses (Linux: from `/proc/net/fib_trie`'s local
/// host routes; elsewhere the routed one and loopback).
pub fn local_addresses() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/proc/net/fib_trie") {
        let mut last: Option<Ipv4Addr> = None;
        for line in text.lines() {
            let t = line.trim_start_matches([' ', '|', '+', '-']).trim();
            if let Ok(a) = t.parse::<Ipv4Addr>() {
                last = Some(a);
            } else if t.starts_with("/32 host LOCAL")
                && let Some(a) = last
                && !out.contains(&a)
            {
                out.push(a);
            }
        }
    }
    if let Some(a) = route_to(Ipv4Addr::new(239, 69, 0, 1))
        && !out.contains(&a)
    {
        out.insert(0, a);
    }
    if !out.contains(&Ipv4Addr::LOCALHOST) {
        out.push(Ipv4Addr::LOCALHOST);
    }
    out
}

fn udp() -> io::Result<Socket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    s.set_reuse_port(true)?;
    Ok(s)
}

/// Explain a refused bind of a privileged port (PTP's 319/320).
fn bind_error(e: io::Error, port: u16) -> io::Error {
    if e.kind() == io::ErrorKind::PermissionDenied && port < 1024 {
        io::Error::new(
            e.kind(),
            format!(
                "port {port} is reserved for privileged programs on this system: allow \
                 FaderFrame to open it (sudo setcap cap_net_bind_service=+ep <program>), \
                 lower net.ipv4.ip_unprivileged_port_start to {port}, or let ptp4l and \
                 phc2sys keep the system clock and choose \"System clock\""
            ),
        )
    } else {
        e
    }
}

/// A socket receiving `group:port` (multicast, joined on `interface`) or,
/// for a unicast address, `port` on that address.
pub fn receiver(group: Ipv4Addr, port: u16, interface: Ipv4Addr) -> io::Result<UdpSocket> {
    let s = udp()?;
    if group.is_multicast() {
        let bind = if cfg!(target_os = "linux") {
            group
        } else {
            Ipv4Addr::UNSPECIFIED
        };
        s.bind(&SockAddr::from(SocketAddrV4::new(bind, port)))
            .map_err(|e| bind_error(e, port))?;
        s.join_multicast_v4(&group, &interface)?;
        #[cfg(target_os = "linux")]
        s.set_multicast_all_v4(false)?;
    } else {
        s.bind(&SockAddr::from(SocketAddrV4::new(group, port)))
            .map_err(|e| bind_error(e, port))?;
    }
    s.set_read_timeout(Some(READ_TIMEOUT))?;
    Ok(s.into())
}

/// A socket sending through `interface` from `port` (0: any), multicast
/// with `ttl` hops, looped back to this host if `looped`, marked `tos`.
pub fn sender(
    interface: Ipv4Addr,
    port: u16,
    ttl: u32,
    looped: bool,
    tos: u32,
) -> io::Result<UdpSocket> {
    let s = udp()?;
    s.bind(&SockAddr::from(SocketAddrV4::new(interface, port)))
        .map_err(|e| bind_error(e, port))?;
    s.set_multicast_if_v4(&interface)?;
    s.set_multicast_ttl_v4(ttl)?;
    s.set_multicast_loop_v4(looped)?;
    // DSCP is a request; a system that refuses it still sends.
    let _ = s.set_tos_v4(tos);
    Ok(s.into())
}

/// A 239.69.x.y group for send stream `index`, from the interface's
/// address (two hosts rarely pick the same; a chosen address overrides).
pub fn default_group(interface: Ipv4Addr, index: usize) -> Ipv4Addr {
    let o = interface.octets();
    let n = (u16::from(o[3]) << 4).wrapping_add(index as u16 + 1);
    Ipv4Addr::new(239, 69, o[2] ^ (n >> 8) as u8, n as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multicast_reaches_a_receiver_on_loopback() {
        let lo = Ipv4Addr::LOCALHOST;
        let group = Ipv4Addr::new(239, 69, 251, 17);
        let port = 25_017;
        let rx = receiver(group, port, lo).unwrap();
        // Another listener on the same group and port (address reuse).
        let rx2 = receiver(group, port, lo).unwrap();
        let tx = sender(lo, 0, 0, true, DSCP_AF41).unwrap();
        tx.send_to(b"hello", (group, port)).unwrap();
        let mut buf = [0u8; 16];
        let (n, _) = rx.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
        let (n, _) = rx2.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[test]
    fn groups_and_addresses() {
        let a = default_group(Ipv4Addr::new(192, 168, 178, 68), 0);
        let b = default_group(Ipv4Addr::new(192, 168, 178, 68), 1);
        assert!(a.is_multicast() && b.is_multicast() && a != b);
        assert_eq!(a.octets()[..2], [239, 69]);
        assert!(local_addresses().contains(&Ipv4Addr::LOCALHOST));
        assert_eq!(interface(Some(Ipv4Addr::LOCALHOST)), Ipv4Addr::LOCALHOST);
    }
}
