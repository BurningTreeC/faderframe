//! Keeping to the local network: who may connect (peers in the private,
//! link-local and loopback ranges), which host names a request may carry
//! (no public names: a web page elsewhere cannot rebind its name to this
//! computer and talk to the remote), and the address phones are told.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// A peer on this computer or the local network: loopback, the private
/// IPv4 ranges (10/8, 172.16/12, 192.168/16), link-local addresses and
/// IPv6 unique-local ones.
pub fn is_local_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_local_peer(IpAddr::V4(v4)),
            None => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
        },
    }
}

/// A peer on this computer.
pub fn is_this_computer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map_or(v6.is_loopback(), |v4| v4.is_loopback()),
    }
}

/// The host a request names, without its port (`[::1]:80` → `::1`).
pub fn host_name(host: &str) -> &str {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => {
            name
        }
        _ => host,
    }
}

/// Whether a request's `Host` names this computer the way a local client
/// does: an address, `localhost`, a name without dots (the machine's), or
/// a local domain (`.local`, `.lan`, `.home.arpa`, `.internal`). A public
/// name is refused — that is a page elsewhere whose name was pointed here.
pub fn host_allowed(host: &str) -> bool {
    let name = host_name(host).trim_end_matches('.').to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    if name.parse::<IpAddr>().is_ok() || name == "localhost" || !name.contains('.') {
        return true;
    }
    [".local", ".lan", ".home.arpa", ".internal", ".localhost"]
        .iter()
        .any(|d| name.ends_with(d))
}

/// This computer's address on the local network: the one its route out
/// leaves from. Nothing is sent: connecting a UDP socket only picks the
/// route. `None` without a network.
pub fn lan_address() -> Option<Ipv4Addr> {
    // The documentation range (never answered) for the default route, then
    // the private ranges for a network without one.
    let probes = [
        Ipv4Addr::new(192, 0, 2, 1),
        Ipv4Addr::new(192, 168, 0, 1),
        Ipv4Addr::new(10, 0, 0, 1),
        Ipv4Addr::new(172, 16, 0, 1),
    ];
    for probe in probes {
        let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
            return None;
        };
        if socket.connect(SocketAddr::from((probe, 9))).is_err() {
            continue;
        }
        if let Ok(SocketAddr::V4(local)) = socket.local_addr() {
            let ip = *local.ip();
            if !ip.is_unspecified() && !ip.is_loopback() {
                return Some(ip);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_local_peers_are_answered() {
        for local in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "172.31.255.1",
            "192.168.1.20",
            "169.254.3.4",
            "::1",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:192.168.1.2",
        ] {
            assert!(is_local_peer(ip(local)), "{local}");
        }
        for public in [
            "8.8.8.8",
            "172.32.0.1",
            "100.64.0.1",
            "2001:db8::1",
            "2a00:1450::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_local_peer(ip(public)), "{public}");
        }
        assert!(is_this_computer(ip("127.0.0.1")));
        assert!(is_this_computer(ip("::1")));
        assert!(!is_this_computer(ip("192.168.1.2")));
    }

    #[test]
    fn public_host_names_are_refused() {
        for ok in [
            "192.168.1.20:8642",
            "192.168.1.20",
            "[fe80::1]:8642",
            "localhost:8642",
            "studio:8642",
            "studio.local",
            "studio.local.",
            "studio.lan:80",
            "box.home.arpa",
        ] {
            assert!(host_allowed(ok), "{ok}");
        }
        for refused in ["evil.example.com", "evil.com:8642", "", ":8642"] {
            assert!(!host_allowed(refused), "{refused}");
        }
        assert_eq!(host_name("[::1]:80"), "::1");
        assert_eq!(host_name("a:80"), "a");
        assert_eq!(host_name("a"), "a");
    }
}
