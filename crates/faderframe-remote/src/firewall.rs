//! Whether this computer's firewall lets phones and tablets reach the
//! remote, read from the firewall's own settings: nothing is run, nothing
//! needs root. Two firewalls are read: ufw (on by default on Omarchy and
//! often on Ubuntu, Mint and Arch) and firewalld (Fedora's, openSUSE's).
//!
//! A test from this computer proves nothing: its own connections come in
//! over loopback, which firewalls let through, so the remote answers here
//! while a phone's connection is dropped without a word (the page only
//! keeps loading). The settings are all there is to go by.
//!
//! The answer leans towards silence: a rule this does not understand
//! counts as letting the phone in when it allows and as missing it when
//! it denies, so a note means the firewall is in the way (ufw), or very
//! likely is (firewalld, whose zone for an interface and whose runtime-only
//! rules are not in its files). The command a note gives lets in only the
//! local network, on the remote's port, over TCP, on the interface the
//! computer's network address is on (ufw).
//!
//! The remote listens on IPv4 only, so only IPv4 rules matter.

use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;

/// The firewalls read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Firewall {
    Ufw,
    Firewalld,
}

impl Firewall {
    pub fn name(self) -> &'static str {
        match self {
            Firewall::Ufw => "ufw",
            Firewall::Firewalld => "firewalld",
        }
    }
}

/// The local network phones come from: the route this computer's network
/// address is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanNet {
    pub interface: String,
    pub network: Ipv4Addr,
    pub prefix: u8,
}

impl LanNet {
    /// `192.168.178.0/24`.
    pub fn cidr(&self) -> String {
        format!("{}/{}", self.network, self.prefix)
    }

    fn range(&self) -> Cidr {
        Cidr::new(u32::from(self.network), self.prefix)
    }
}

/// A firewall in the way of the remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirewallNote {
    pub firewall: Firewall,
    /// The remote's port.
    pub port: u16,
    /// Read from its rules: it lets nothing in on the port. `false`: it is
    /// on, and what it lets in could not all be read.
    pub certain: bool,
    /// firewalld's zone.
    pub zone: Option<String>,
    /// The local network, when known (the command names it).
    pub lan: Option<LanNet>,
    /// What lets devices on the local network in (run in a terminal).
    pub command: String,
}

impl FirewallNote {
    /// What to tell the user, ending where the command follows.
    pub fn text(&self) -> String {
        let p = self.port;
        let fix = "To let in devices on this network, run in a terminal:";
        match (self.firewall, self.certain) {
            (Firewall::Ufw, true) => format!(
                "The firewall (ufw) lets nothing in on port {p}, so phones and tablets \
                 cannot open the remote: their browsers keep loading. {fix}"
            ),
            (Firewall::Ufw, false) => format!(
                "The firewall (ufw) is on, and its rules cannot be read here. If phones \
                 and tablets cannot open the remote, it does not let them in on port {p}. \
                 {fix}"
            ),
            (Firewall::Firewalld, _) => format!(
                "The firewall (firewalld) does not open port {p} in its zone “{}”, so \
                 phones and tablets most likely cannot open the remote. {fix}",
                self.zone.as_deref().unwrap_or("public")
            ),
        }
    }
}

/// What this computer's firewall does to the remote's `port`, for devices
/// on the network `lan` (this computer's address there) is on. `None`: no
/// firewall in the way that this can tell (always off Linux: Windows and
/// macOS ask the user themselves when a program first listens).
pub fn check(port: u16, lan: Option<Ipv4Addr>) -> Option<FirewallNote> {
    #[cfg(target_os = "linux")]
    {
        check_in(Path::new("/"), port, lan)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (port, lan);
        None
    }
}

/// [`check`] with the system's files under `root` (tests).
pub fn check_in(root: &Path, port: u16, lan: Option<Ipv4Addr>) -> Option<FirewallNote> {
    let net = lan.and_then(|ip| lan_net(&read(root, "proc/net/route")?, ip));
    ufw(root, port, lan, net.as_ref()).or_else(|| firewalld(root, port, net.as_ref()))
}

fn read(root: &Path, file: &str) -> Option<String> {
    fs::read_to_string(root.join(file)).ok()
}

/// `KEY=value` (or `KEY="value"`) from a shell-style settings file.
fn setting<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (k, v) = line.trim().split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches(|c| c == '"' || c == '\''))
    })
}

/// Whether systemd starts `unit` at boot. Without systemd's files for it,
/// yes (the firewall's own setting decides).
fn unit_enabled(root: &Path, unit: &str) -> bool {
    let etc = root.join("etc/systemd/system");
    if fs::read_link(etc.join(unit)).is_ok_and(|to| to == Path::new("/dev/null")) {
        return false; // masked
    }
    let known = ["usr/lib/systemd/system", "lib/systemd/system"]
        .iter()
        .any(|d| root.join(d).join(unit).exists());
    if !known {
        return true;
    }
    let Ok(dirs) = fs::read_dir(&etc) else {
        return false;
    };
    dirs.flatten().any(|d| {
        d.file_name().to_string_lossy().ends_with(".wants")
            && fs::symlink_metadata(d.path().join(unit)).is_ok()
    })
}

/// Whether a process named `name` runs.
fn running(root: &Path, name: &str) -> bool {
    let Ok(procs) = fs::read_dir(root.join("proc")) else {
        return false;
    };
    procs.flatten().any(|p| {
        p.file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
            && fs::read_to_string(p.path().join("comm")).is_ok_and(|c| c.trim() == name)
    })
}

// --- addresses ---------------------------------------------------------

/// An IPv4 address range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cidr {
    net: u32,
    prefix: u8,
}

impl Cidr {
    fn new(addr: u32, prefix: u8) -> Cidr {
        let prefix = prefix.min(32);
        Cidr {
            net: addr & Self::mask(prefix),
            prefix,
        }
    }

    fn mask(prefix: u8) -> u32 {
        if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - u32::from(prefix.min(32)))
        }
    }

    /// `10.0.0.0/8`, `192.168.1.5`.
    fn parse(s: &str) -> Option<Cidr> {
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (a, p.parse::<u8>().ok().filter(|p| *p <= 32)?),
            None => (s, 32),
        };
        Some(Cidr::new(u32::from(addr.parse::<Ipv4Addr>().ok()?), prefix))
    }

    fn contains(self, ip: u32) -> bool {
        ip & Self::mask(self.prefix) == self.net
    }

    fn overlaps(self, other: Cidr) -> bool {
        self.contains(other.net) || other.contains(self.net)
    }

    fn covers(self, other: Cidr) -> bool {
        self.prefix <= other.prefix && self.contains(other.net)
    }
}

/// The network `ip` is on, from `/proc/net/route` (the narrowest route
/// holding it that is not the default one).
fn lan_net(route: &str, ip: Ipv4Addr) -> Option<LanNet> {
    // The kernel prints each address as the u32 its bytes (network order)
    // make on this machine.
    let addr = |h: &str| {
        u32::from_str_radix(h, 16)
            .ok()
            .map(|v| u32::from(Ipv4Addr::from(v.to_ne_bytes())))
    };
    route
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let mask = addr(f.get(7)?)?;
            let prefix = mask.leading_ones() as u8;
            if prefix == 0 || mask.count_ones() != u32::from(prefix) {
                return None;
            }
            let range = Cidr::new(addr(f.get(1)?)?, prefix);
            let interface = f.first()?;
            range.contains(u32::from(ip)).then(|| LanNet {
                interface: (*interface).to_string(),
                network: Ipv4Addr::from(range.net),
                prefix,
            })
        })
        .max_by_key(|n| n.prefix)
}

/// Whether a port list (`8642`, `6000:6007`, `80,443`, firewalld's
/// `6000-6007`) holds `port`.
fn ports_hold(list: &str, port: u16) -> bool {
    list.split(',').any(|p| {
        let p = p.trim();
        match p.split_once([':', '-']) {
            Some((a, b)) => {
                let a = if a.is_empty() {
                    Ok(0)
                } else {
                    a.parse::<u16>()
                };
                let b = if b.is_empty() {
                    Ok(u16::MAX)
                } else {
                    b.parse::<u16>()
                };
                matches!((a, b), (Ok(a), Ok(b)) if a <= port && port <= b)
            }
            None => p.parse::<u16>() == Ok(port),
        }
    })
}

// --- ufw -----------------------------------------------------------------

fn ufw(
    root: &Path,
    port: u16,
    lan: Option<Ipv4Addr>,
    net: Option<&LanNet>,
) -> Option<FirewallNote> {
    let conf = read(root, "etc/ufw/ufw.conf")?;
    if !setting(&conf, "ENABLED").is_some_and(|v| v.eq_ignore_ascii_case("yes")) {
        return None;
    }
    if !unit_enabled(root, "ufw.service") {
        return None;
    }
    let policy = read(root, "etc/default/ufw")
        .and_then(|d| setting(&d, "DEFAULT_INPUT_POLICY").map(str::to_ascii_uppercase))
        .unwrap_or_else(|| "DROP".into());
    if policy == "ACCEPT" {
        return None;
    }
    let certain = match read(root, "etc/ufw/user.rules") {
        Some(rules) => {
            if ufw_lets_in(&rules, port, lan, net) {
                return None;
            }
            true
        }
        None => false,
    };
    let command = match net {
        Some(n) => format!(
            "sudo ufw allow in on {} from {} to any port {port} proto tcp comment 'FaderFrame remote'",
            n.interface,
            n.cidr()
        ),
        None => format!("sudo ufw allow {port}/tcp comment 'FaderFrame remote'"),
    };
    Some(FirewallNote {
        firewall: Firewall::Ufw,
        port,
        certain,
        zone: None,
        lan: net.cloned(),
        command,
    })
}

/// Whether ufw's own rules (`user.rules`, read in order: the first rule
/// that decides does) let a TCP connection from the local network in on
/// `port`. The policy behind them is not ACCEPT (looked at before).
fn ufw_lets_in(rules: &str, port: u16, lan: Option<Ipv4Addr>, net: Option<&LanNet>) -> bool {
    for line in rules.lines() {
        let Some(rule) = line.trim().strip_prefix("-A ufw-user-input ") else {
            continue;
        };
        if let Some(lets_in) = UfwRule::parse(rule).and_then(|r| r.decides(port, lan, net)) {
            return lets_in;
        }
    }
    false
}

/// What a rule of ufw's says (the iptables options ufw writes).
#[derive(Default)]
struct UfwRule<'a> {
    proto: Option<&'a str>,
    ports: Option<&'a str>,
    source: Option<&'a str>,
    dest: Option<&'a str>,
    interface: Option<&'a str>,
    target: Option<&'a str>,
}

impl<'a> UfwRule<'a> {
    fn parse(rule: &'a str) -> Option<UfwRule<'a>> {
        let words: Vec<&str> = rule.split_whitespace().collect();
        let mut r = UfwRule::default();
        let mut i = 0;
        while i < words.len() {
            let next = words.get(i + 1).copied();
            let field = match words[i] {
                // ufw writes no negations: a rule with one is not its.
                "!" => return None,
                "-p" | "--protocol" => &mut r.proto,
                "--dport" | "--dports" | "--destination-port" | "--destination-ports" => {
                    &mut r.ports
                }
                "-s" | "--source" => &mut r.source,
                "-d" | "--destination" => &mut r.dest,
                "-i" | "--in-interface" => &mut r.interface,
                "-j" | "--jump" => &mut r.target,
                "--comment" => {
                    i += 2;
                    continue;
                }
                _ => {
                    i += 1;
                    continue;
                }
            };
            *field = next;
            i += 2;
        }
        Some(r)
    }

    /// `Some(true)`: lets the phone in, `Some(false)`: turns it away,
    /// `None`: not about it (or not understood). Where the rule names what
    /// is not known here, an allowing rule counts and a denying one does
    /// not.
    fn decides(&self, port: u16, lan: Option<Ipv4Addr>, net: Option<&LanNet>) -> Option<bool> {
        let allows = match self.target? {
            "ACCEPT" | "ufw-user-limit-accept" => true,
            "DROP" | "REJECT" => false,
            // Logging, the rate limit's own check, …
            _ => return None,
        };
        if let Some(p) = self.proto
            && !matches!(p.to_ascii_lowercase().as_str(), "tcp" | "all" | "6")
        {
            return None;
        }
        if let Some(ports) = self.ports
            && !ports_hold(ports, port)
        {
            return None;
        }
        if let Some(i) = self.interface {
            match net {
                Some(n) => {
                    let same = match i.strip_suffix('+') {
                        Some(prefix) => n.interface.starts_with(prefix),
                        None => n.interface == i,
                    };
                    if !same {
                        return None;
                    }
                }
                None if !allows => return None,
                None => {}
            }
        }
        if let Some(d) = self.dest {
            let range = Cidr::parse(d)?;
            match lan {
                Some(ip) if !range.contains(u32::from(ip)) => return None,
                None if !allows => return None,
                _ => {}
            }
        }
        if let Some(s) = self.source {
            let range = Cidr::parse(s)?;
            match net {
                Some(n) if allows && !range.overlaps(n.range()) => return None,
                Some(n) if !allows && !range.covers(n.range()) => return None,
                None if !allows => return None,
                _ => {}
            }
        }
        Some(allows)
    }
}

// --- firewalld -------------------------------------------------------------

fn firewalld(root: &Path, port: u16, net: Option<&LanNet>) -> Option<FirewallNote> {
    let conf = read(root, "etc/firewalld/firewalld.conf")?;
    if !running(root, "firewalld") {
        return None;
    }
    let zone = net
        .and_then(|n| zone_of_interface(root, &n.interface))
        .or_else(|| setting(&conf, "DefaultZone").map(str::to_string))
        .filter(|z| plain_name(z))
        .unwrap_or_else(|| "public".into());
    let xml = firewalld_file(root, "zones", &zone)?;
    if zone_opens(root, &xml, port) {
        return None;
    }
    let command = match net {
        Some(n) => format!(
            "sudo firewall-cmd --permanent --zone={zone} --add-rich-rule='rule family=\"ipv4\" \
             source address=\"{}\" port port=\"{port}\" protocol=\"tcp\" accept' && \
             sudo firewall-cmd --reload",
            n.cidr()
        ),
        None => format!(
            "sudo firewall-cmd --permanent --zone={zone} --add-port={port}/tcp && \
             sudo firewall-cmd --reload"
        ),
    };
    Some(FirewallNote {
        firewall: Firewall::Firewalld,
        port,
        // Runtime-only rules and NetworkManager's zones are not in files.
        certain: false,
        zone: Some(zone),
        lan: net.cloned(),
        command,
    })
}

/// A zone or service name (no paths).
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A zone's or service's file: the administrator's, else the package's.
fn firewalld_file(root: &Path, kind: &str, name: &str) -> Option<String> {
    if !plain_name(name) {
        return None;
    }
    read(root, &format!("etc/firewalld/{kind}/{name}.xml"))
        .or_else(|| read(root, &format!("usr/lib/firewalld/{kind}/{name}.xml")))
}

/// The zone an administrator's zone file binds `interface` to.
fn zone_of_interface(root: &Path, interface: &str) -> Option<String> {
    let dir = fs::read_dir(root.join("etc/firewalld/zones")).ok()?;
    dir.flatten().find_map(|f| {
        let name = f.file_name().to_string_lossy().to_string();
        let zone = name.strip_suffix(".xml")?;
        let xml = fs::read_to_string(f.path()).ok()?;
        elements(&xml, "interface")
            .iter()
            .any(|e| attribute(e, "name") == Some(interface))
            .then(|| zone.to_string())
    })
}

/// Whether a zone lets TCP in on `port`: it accepts everything, or one of
/// its ports, rich rules' ports or services' ports holds it.
fn zone_opens(root: &Path, xml: &str, port: u16) -> bool {
    if elements(xml, "zone")
        .first()
        .and_then(|z| attribute(z, "target"))
        .is_some_and(|t| t.eq_ignore_ascii_case("ACCEPT"))
    {
        return true;
    }
    let tcp_port = |e: &&str| {
        attribute(e, "protocol").is_none_or(|p| p.eq_ignore_ascii_case("tcp"))
            && attribute(e, "port").is_some_and(|p| ports_hold(p, port))
    };
    if elements(xml, "port").iter().any(tcp_port) {
        return true;
    }
    elements(xml, "service").iter().any(|s| {
        attribute(s, "name")
            .and_then(|name| firewalld_file(root, "services", name))
            .is_some_and(|svc| elements(&svc, "port").iter().any(tcp_port))
    })
}

/// The inside of each `<tag …>` in `xml` (enough XML for firewalld's
/// files: elements are found by name, attributes read from them).
fn elements<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        let ends_name = after
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == '/' || c == '>');
        let Some(end) = after.find('>') else {
            break;
        };
        if ends_name {
            found.push(&after[..end]);
        }
        rest = &after[end..];
    }
    found
}

/// An attribute's value in an element's inside (`name="v"` or `name='v'`).
fn attribute<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = element;
    loop {
        let at = rest.find(name)?;
        let before = rest[..at].chars().next_back();
        let after = rest[at + name.len()..].trim_start();
        if before.is_some_and(char::is_whitespace)
            && let Some(value) = after.strip_prefix('=')
        {
            let value = value.trim_start();
            let quote = value.chars().next().filter(|q| *q == '"' || *q == '\'')?;
            let value = &value[1..];
            return value.find(quote).map(|end| &value[..end]);
        }
        rest = &rest[at + name.len()..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const LAN: Ipv4Addr = Ipv4Addr::new(192, 168, 178, 68);

    fn net() -> LanNet {
        LanNet {
            interface: "enp4s0".into(),
            network: Ipv4Addr::new(192, 168, 178, 0),
            prefix: 24,
        }
    }

    /// ufw's own file with these rules.
    fn rules(lines: &[&str]) -> String {
        let mut s = String::from("*filter\n:ufw-user-input - [0:0]\n### RULES ###\n");
        for l in lines {
            s.push_str(l);
            s.push('\n');
        }
        s.push_str("### END RULES ###\nCOMMIT\n");
        s
    }

    fn lets_in(lines: &[&str]) -> bool {
        ufw_lets_in(&rules(lines), 8642, Some(LAN), Some(&net()))
    }

    /// Omarchy's rules: LocalSend and Docker's DNS.
    const OMARCHY: [&str; 4] = [
        "-A ufw-user-input -p udp --dport 53317 -j ACCEPT",
        "-A ufw-user-input -p tcp --dport 53317 -j ACCEPT",
        "-A ufw-user-input -p udp -d 172.17.0.1 --dport 53 -s 172.16.0.0/12 -j ACCEPT",
        "-A ufw-user-input -p udp -d 172.17.0.1 --dport 53 -s 192.168.0.0/16 -j ACCEPT",
    ];

    #[test]
    fn ufw_rules_decide_in_order() {
        assert!(!lets_in(&OMARCHY));
        // What the note's command writes.
        let mut opened = OMARCHY.to_vec();
        opened
            .push("-A ufw-user-input -i enp4s0 -p tcp --dport 8642 -s 192.168.178.0/24 -j ACCEPT");
        assert!(lets_in(&opened));
        for open in [
            "-A ufw-user-input -p tcp --dport 8642 -j ACCEPT",
            "-A ufw-user-input -p tcp -m multiport --dports 80,8000:9000 -j ACCEPT -m comment --comment 'dapp_Web'",
            "-A ufw-user-input -s 192.168.178.0/24 -j ACCEPT",
            "-A ufw-user-input -i enp+ -p tcp --dport 8642 -j ACCEPT",
            "-A ufw-user-input -p tcp -d 192.168.178.68 --dport 8642 -j ACCEPT",
            // One phone only: a deliberate rule, counted.
            "-A ufw-user-input -p tcp --dport 8642 -s 192.168.178.40 -j ACCEPT",
            "-A ufw-user-input -p tcp --dport 8642 -j ufw-user-limit-accept",
        ] {
            assert!(lets_in(&[open]), "{open}");
        }
        for closed in [
            "-A ufw-user-input -p udp --dport 8642 -j ACCEPT",
            "-A ufw-user-input -p tcp --dport 8643 -j ACCEPT",
            "-A ufw-user-input -p tcp -m multiport --dports 80,443 -j ACCEPT",
            "-A ufw-user-input -i tailscale0 -p tcp --dport 8642 -j ACCEPT",
            "-A ufw-user-input -p tcp --dport 8642 -s 10.0.0.0/8 -j ACCEPT",
            "-A ufw-user-input -p tcp -d 172.17.0.1 --dport 8642 -j ACCEPT",
            "-A ufw-user-input -p tcp ! --dport 8642 -j ACCEPT",
            // Rate limiting's own check decides nothing.
            "-A ufw-user-input -p tcp --dport 8642 -m conntrack --ctstate NEW -m recent --update --seconds 30 --hitcount 6 -j ufw-user-limit",
        ] {
            assert!(!lets_in(&[closed]), "{closed}");
        }
        // The first rule that decides does.
        let allow = "-A ufw-user-input -p tcp --dport 8642 -j ACCEPT";
        assert!(!lets_in(&[
            "-A ufw-user-input -p tcp --dport 8642 -j DROP",
            allow
        ]));
        assert!(!lets_in(&[
            "-A ufw-user-input -s 192.168.0.0/16 -j REJECT --reject-with tcp-reset",
            allow
        ]));
        // A denial of one address does not stop the others.
        assert!(lets_in(&[
            "-A ufw-user-input -p tcp --dport 8642 -s 192.168.178.40 -j DROP",
            allow
        ]));
        // Logging decides nothing.
        assert!(lets_in(&[
            "-A ufw-user-input -p tcp --dport 8642 -j ufw-user-logging-input",
            allow
        ]));
        // Not knowing the network: allowing rules count, denying ones not.
        let unknown = |lines: &[&str]| ufw_lets_in(&rules(lines), 8642, None, None);
        assert!(unknown(&[
            "-A ufw-user-input -i enp4s0 -p tcp --dport 8642 -s 192.168.178.0/24 -j ACCEPT"
        ]));
        assert!(unknown(&[
            "-A ufw-user-input -i tailscale0 -p tcp --dport 8642 -j DROP",
            allow
        ]));
        assert!(!unknown(&OMARCHY));
    }

    #[test]
    fn port_lists_and_ranges() {
        assert!(ports_hold("8642", 8642));
        assert!(ports_hold("80,443,8000:9000", 8642));
        assert!(ports_hold("8000-9000", 8642));
        assert!(ports_hold(":9000", 8642));
        assert!(ports_hold("8000:", 8642));
        assert!(!ports_hold("80,443", 8642));
        assert!(!ports_hold("x", 8642));
    }

    #[test]
    fn addresses_ranges_and_routes() {
        let lan = Cidr::parse("192.168.178.0/24").unwrap();
        assert!(lan.contains(u32::from(LAN)));
        assert!(Cidr::parse("192.168.0.0/16").unwrap().covers(lan));
        assert!(!Cidr::parse("192.168.178.40").unwrap().covers(lan));
        assert!(Cidr::parse("192.168.178.40").unwrap().overlaps(lan));
        assert!(!Cidr::parse("10.0.0.0/8").unwrap().overlaps(lan));
        assert!(Cidr::parse("0.0.0.0/0").unwrap().covers(lan));
        assert_eq!(Cidr::parse("192.168.178.7/24"), Some(lan));
        assert_eq!(Cidr::parse("1.2.3.4/33"), None);
        assert_eq!(Cidr::parse("nonsense"), None);
    }

    /// `/proc/net/route` as a little-endian machine prints it.
    #[cfg(target_endian = "little")]
    const ROUTE: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
         enp4s0\t00000000\t01B2A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
         enp4s0\t00B2A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
         docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n";

    #[cfg(target_endian = "little")]
    #[test]
    fn the_network_is_the_route_the_address_is_on() {
        assert_eq!(lan_net(ROUTE, LAN), Some(net()));
        assert_eq!(
            lan_net(ROUTE, Ipv4Addr::new(172, 17, 0, 1)).map(|n| n.cidr()),
            Some("172.17.0.0/16".into())
        );
        assert_eq!(lan_net(ROUTE, Ipv4Addr::new(10, 1, 2, 3)), None);
    }

    /// A pretend system root, removed when dropped.
    struct Root(PathBuf);

    impl Root {
        fn new(name: &str) -> Root {
            let dir = std::env::temp_dir()
                .join(format!("faderframe-firewall-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Root(dir)
        }

        fn write(&self, file: &str, text: &str) -> &Root {
            let path = self.0.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
            self
        }

        fn remove(&self, file: &str) -> &Root {
            fs::remove_file(self.0.join(file)).unwrap();
            self
        }

        fn check(&self) -> Option<FirewallNote> {
            check_in(&self.0, 8642, Some(LAN))
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(target_endian = "little")]
    #[test]
    fn ufw_in_the_way_is_told_with_a_command_for_the_local_network() {
        let root = Root::new("ufw");
        assert_eq!(root.check(), None, "no firewall");
        root.write("proc/net/route", ROUTE)
            .write("etc/ufw/ufw.conf", "# comment\nENABLED=yes\nLOGLEVEL=low\n")
            .write(
                "etc/default/ufw",
                "IPV6=yes\nDEFAULT_INPUT_POLICY=\"DROP\"\n",
            )
            .write("etc/ufw/user.rules", &rules(&OMARCHY));
        let note = root.check().unwrap();
        assert_eq!(note.firewall, Firewall::Ufw);
        assert!(note.certain);
        assert_eq!(
            note.command,
            "sudo ufw allow in on enp4s0 from 192.168.178.0/24 to any port 8642 proto tcp \
             comment 'FaderFrame remote'"
        );
        assert!(note.text().contains("port 8642"), "{}", note.text());
        // The command's rule lets the phone in.
        let mut opened = OMARCHY.to_vec();
        opened
            .push("-A ufw-user-input -i enp4s0 -p tcp --dport 8642 -s 192.168.178.0/24 -j ACCEPT");
        root.write("etc/ufw/user.rules", &rules(&opened));
        assert_eq!(root.check(), None);
        // Rules that cannot be read: perhaps.
        root.remove("etc/ufw/user.rules");
        assert!(!root.check().unwrap().certain);
        root.write("etc/ufw/user.rules", &rules(&OMARCHY));
        // Letting everything in, or off.
        root.write("etc/default/ufw", "DEFAULT_INPUT_POLICY=\"ACCEPT\"\n");
        assert_eq!(root.check(), None);
        root.write("etc/default/ufw", "DEFAULT_INPUT_POLICY=\"REJECT\"\n");
        assert!(root.check().is_some());
        root.write("etc/ufw/ufw.conf", "ENABLED=no\n");
        assert_eq!(root.check(), None);
        root.write("etc/ufw/ufw.conf", "ENABLED=yes\n");
        // Installed for systemd but not started at boot: not running.
        root.write("usr/lib/systemd/system/ufw.service", "[Unit]\n");
        assert_eq!(root.check(), None);
        root.write("etc/systemd/system/multi-user.target.wants/ufw.service", "");
        assert!(root.check().is_some());
        // Without the network: the port alone.
        let note = check_in(&root.0, 8642, None).unwrap();
        assert_eq!(
            note.command,
            "sudo ufw allow 8642/tcp comment 'FaderFrame remote'"
        );
    }

    #[cfg(target_endian = "little")]
    #[test]
    fn firewalld_zones_ports_and_services_are_read() {
        let root = Root::new("firewalld");
        root.write("proc/net/route", ROUTE)
            .write("etc/firewalld/firewalld.conf", "DefaultZone=public\n")
            .write(
                "usr/lib/firewalld/zones/public.xml",
                r#"<?xml version="1.0" encoding="utf-8"?>
<zone>
  <short>Public</short>
  <service name="ssh"/>
  <service name="dhcpv6-client"/>
  <forward-port port="8642" protocol="tcp" to-port="22"/>
</zone>"#,
            )
            .write(
                "usr/lib/firewalld/services/ssh.xml",
                r#"<service><port protocol="tcp" port="22"/></service>"#,
            );
        // Not running: nothing to say.
        assert_eq!(root.check(), None);
        root.write("proc/812/comm", "firewalld\n");
        let note = root.check().unwrap();
        assert_eq!(note.firewall, Firewall::Firewalld);
        assert_eq!(note.zone.as_deref(), Some("public"));
        assert!(note.command.contains("--zone=public"), "{}", note.command);
        assert!(
            note.command.contains(
                r#"source address="192.168.178.0/24" port port="8642" protocol="tcp" accept"#
            ),
            "{}",
            note.command
        );
        // The rich rule the command adds opens it.
        root.write(
            "etc/firewalld/zones/public.xml",
            r#"<zone><service name="ssh"/><rule family="ipv4"><source address="192.168.178.0/24"/><port port="8642" protocol="tcp"/><accept/></rule></zone>"#,
        );
        assert_eq!(root.check(), None);
        // Fedora Workstation's zone opens the high ports.
        root.write(
            "etc/firewalld/zones/public.xml",
            r#"<zone><port protocol="udp" port="1025-65535"/><port protocol='tcp' port='1025-65535'/></zone>"#,
        );
        assert_eq!(root.check(), None);
        // A service holding the port.
        root.write(
            "etc/firewalld/zones/public.xml",
            r#"<zone><service name="faderframe"/></zone>"#,
        )
        .write(
            "etc/firewalld/services/faderframe.xml",
            r#"<service><port protocol="tcp" port="8642"/></service>"#,
        );
        assert_eq!(root.check(), None);
        // The zone the interface is bound to, trusted.
        root.write(
            "etc/firewalld/zones/trusted.xml",
            r#"<zone target="ACCEPT"><interface name="enp4s0"/></zone>"#,
        )
        .write("etc/firewalld/zones/public.xml", "<zone/>");
        assert_eq!(root.check(), None);
        root.write(
            "etc/firewalld/zones/home.xml",
            r#"<zone><interface name="wlan0"/></zone>"#,
        );
        root.remove("etc/firewalld/zones/trusted.xml");
        assert_eq!(root.check().unwrap().zone.as_deref(), Some("public"));
    }

    #[test]
    fn xml_attributes_are_read() {
        let xml =
            r#"<zone target="ACCEPT"><port protocol='tcp' port="1-2"/><portal port="9"/></zone>"#;
        let ports = elements(xml, "port");
        assert_eq!(ports.len(), 1);
        assert_eq!(attribute(ports[0], "port"), Some("1-2"));
        assert_eq!(attribute(ports[0], "protocol"), Some("tcp"));
        assert_eq!(
            attribute(elements(xml, "zone")[0], "target"),
            Some("ACCEPT")
        );
        assert_eq!(attribute(ports[0], "name"), None);
        assert!(!plain_name("../etc"));
        assert!(plain_name("FedoraWorkstation"));
    }

    #[test]
    fn settings_files_are_read() {
        let text = "# ENABLED=no\nENABLED=yes\nDEFAULT_INPUT_POLICY=\"DROP\"\n";
        assert_eq!(setting(text, "ENABLED"), Some("yes"));
        assert_eq!(setting(text, "DEFAULT_INPUT_POLICY"), Some("DROP"));
        assert_eq!(setting(text, "MISSING"), None);
    }
}
