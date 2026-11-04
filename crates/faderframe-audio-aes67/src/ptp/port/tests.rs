//! Ports exchanging messages over a simulated network: clocks of their
//! own (rate off, started apart), path delays with jitter, software
//! timestamps that wake late now and then.

use super::*;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

const MS: u64 = 1_000_000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct Node {
    port: Port,
    /// Local = true · (1 + skew) + start.
    skew: f64,
    start: u64,
}

impl Node {
    fn local(&self, t: u64) -> u64 {
        (t as f64 * (1.0 + self.skew)) as u64 + self.start
    }

    /// The PTP time it shows at true time `t`.
    fn shows(&self, t: u64) -> i64 {
        self.port.clock().ptp(self.local(t)) as i64
    }
}

struct Net {
    nodes: Vec<Option<Node>>,
    /// (arrival true ns, order, to, message)
    queue: BinaryHeap<Reverse<(u64, u64, usize, MessageKey)>>,
    messages: Vec<Message>,
    order: u64,
    rng: Rng,
    path_ns: f64,
    jitter_ns: f64,
}

/// A message by its index in `Net::messages` (heap entries must be `Ord`).
type MessageKey = usize;

impl Net {
    fn new(path_ns: f64, jitter_ns: f64) -> Self {
        Self {
            nodes: Vec::new(),
            queue: BinaryHeap::new(),
            messages: Vec::new(),
            order: 0,
            rng: Rng(0x9E37_79B9_7F4A_7C15),
            path_ns,
            jitter_ns,
        }
    }

    fn add(&mut self, mac_last: u8, config: impl Fn(ClockIdentity) -> PortConfig, skew: f64) {
        let id = ClockIdentity::from_mac([0x02, 0, 0, 0, 0, mac_last]);
        let start = 1_000 * MS * u64::from(mac_last);
        // Each starts from its own idea of the time (seconds apart).
        let clock = VirtualClock::at(start, 1_700_000_000_000_000_000 + start * 3);
        self.nodes.push(Some(Node {
            port: Port::new(config(id), clock, start),
            skew,
            start,
        }));
    }

    /// Software timestamps: a few tens of µs late, now and then a lot.
    fn wake(&mut self) -> u64 {
        let r = self.rng.next();
        if r < 0.02 {
            900_000
        } else {
            (10_000.0 + 50_000.0 * self.rng.next()) as u64
        }
    }

    fn send(&mut self, from: usize, t: u64, outs: Vec<Outgoing>) {
        for o in outs {
            let leaves = t + (5_000.0 + 25_000.0 * self.rng.next()) as u64;
            if o.stamp {
                // Stamped just before the send call.
                let mut more = Vec::new();
                if let Some(n) = self.nodes[from].as_mut() {
                    let l = n.local(t);
                    n.port.sent(&o.message, l, &mut more);
                }
                self.send(from, t, more);
            }
            let key = self.messages.len();
            self.messages.push(o.message);
            for to in 0..self.nodes.len() {
                if to == from || self.nodes[to].is_none() {
                    continue;
                }
                let at = leaves + (self.path_ns + self.jitter_ns * self.rng.next()) as u64;
                self.order += 1;
                self.queue.push(Reverse((at, self.order, to, key)));
            }
        }
    }

    /// Run from true time `from` to `to` in 1 ms ticks.
    fn run(&mut self, from: u64, to: u64) {
        let mut t = from;
        while t < to {
            while let Some(Reverse((at, _, _, _))) = self.queue.peek()
                && *at <= t
            {
                let Some(Reverse((at, _, node, key))) = self.queue.pop() else {
                    break;
                };
                let wake = self.wake();
                let msg = self.messages[key];
                let mut outs = Vec::new();
                if let Some(n) = self.nodes[node].as_mut() {
                    let rx = n.local(at + wake);
                    n.port.handle(&msg, rx, &mut outs);
                }
                self.send(node, at + wake, outs);
            }
            for k in 0..self.nodes.len() {
                let mut outs = Vec::new();
                if let Some(n) = self.nodes[k].as_mut() {
                    let l = n.local(t);
                    n.port.tick(l, &mut outs);
                }
                self.send(k, t, outs);
            }
            t += MS;
        }
    }

    fn node(&self, k: usize) -> &Node {
        self.nodes[k].as_ref().expect("node")
    }
}

fn leader(id: ClockIdentity) -> PortConfig {
    let mut c = PortConfig::new(id, 0, Profile::Media, true);
    c.priority1 = 128;
    c
}

fn follower(id: ClockIdentity) -> PortConfig {
    PortConfig::new(id, 0, Profile::Media, false)
}

#[test]
fn a_follower_locks_to_the_grandmaster_through_jitter() {
    let mut net = Net::new(150_000.0, 40_000.0);
    net.add(1, leader, 0.0);
    // 60 ppm fast, started seconds off.
    net.add(2, follower, 60e-6);
    net.run(0, 90_000 * MS);
    let (m, s) = (net.node(0), net.node(1));
    assert_eq!(m.port.state(), PortState::Master);
    assert_eq!(s.port.state(), PortState::Slave);
    let status = s.port.status();
    assert_eq!(status.clock, ClockState::Locked, "{status:?}");
    assert_eq!(status.steps, 1, "one step at the first lock");
    assert!(
        (status.drift_ppm - 60.0).abs() < 1.0,
        "drift {}",
        status.drift_ppm
    );
    let delay = status.path_delay_ns.unwrap_or(0);
    assert!(
        (150_000..260_000).contains(&delay),
        "path delay {delay} ns (150 µs + jitter and timestamping)"
    );
    // The time it shows against the master's, over the last 10 s.
    let mut worst = 0i64;
    for k in 0..100u64 {
        let t = 80_000 * MS + k * 100 * MS;
        worst = worst.max((s.shows(t) - m.shows(t)).abs());
    }
    assert!(worst < 60_000, "followed within {worst} ns");
}

#[test]
fn the_default_profile_locks_too() {
    let profile = |p: fn(ClockIdentity) -> PortConfig| {
        move |id| PortConfig {
            profile: Profile::Default,
            ..p(id)
        }
    };
    let mut net = Net::new(80_000.0, 20_000.0);
    net.add(1, profile(leader), 0.0);
    net.add(2, profile(follower), -35e-6);
    net.run(0, 150_000 * MS);
    let s = net.node(1);
    let status = s.port.status();
    assert_eq!(status.clock, ClockState::Locked, "{status:?}");
    assert!(
        (status.drift_ppm + 35.0).abs() < 1.0,
        "{}",
        status.drift_ppm
    );
    let t = 149_000 * MS;
    let off = s.shows(t) - net.node(0).shows(t);
    assert!(off.abs() < 60_000, "{off} ns");
}

#[test]
fn the_best_clock_leads_and_another_takes_over_when_it_goes() {
    let fallback = |id| PortConfig::new(id, 0, Profile::Media, true);
    let mut net = Net::new(100_000.0, 20_000.0);
    net.add(3, fallback, 20e-6);
    net.add(4, fallback, -10e-6);
    net.add(5, follower, 5e-6);
    net.run(0, 30_000 * MS);
    // Between equals the lower identity leads.
    assert_eq!(net.node(0).port.state(), PortState::Master);
    assert_eq!(net.node(1).port.state(), PortState::Slave);
    assert_eq!(net.node(2).port.state(), PortState::Slave);
    assert_eq!(
        net.node(2).port.status().grandmaster.map(|g| g.grandmaster),
        Some(net.node(0).port.config().identity.clock)
    );
    // A real grandmaster (priority 128) joins: everyone follows it.
    net.add(9, leader, 0.0);
    net.run(30_000 * MS, 60_000 * MS);
    let gm = net.node(3).port.config().identity.clock;
    for k in 0..3 {
        let n = net.node(k);
        assert_eq!(n.port.state(), PortState::Slave, "node {k}");
        assert_eq!(n.port.status().grandmaster.map(|g| g.grandmaster), Some(gm));
    }
    assert_eq!(net.node(3).port.state(), PortState::Master);
    // It goes away: the best fallback leads again, the others follow it
    // (each stepping onto the new time once).
    net.nodes[3] = None;
    net.run(60_000 * MS, 100_000 * MS);
    assert_eq!(net.node(0).port.state(), PortState::Master);
    assert_eq!(net.node(1).port.state(), PortState::Slave);
    let s = net.node(2);
    assert_eq!(
        s.port.status().clock,
        ClockState::Locked,
        "{:?} {:?}",
        s.port.status(),
        s.port.servo
    );
    let t = 99_000 * MS;
    let off = s.shows(t) - net.node(0).shows(t);
    assert!(off.abs() < 60_000, "{off} ns");
}

#[test]
fn a_follower_alone_keeps_listening() {
    let mut net = Net::new(100_000.0, 0.0);
    net.add(1, follower, 0.0);
    net.run(0, 20_000 * MS);
    assert_eq!(net.node(0).port.state(), PortState::Listening);
    assert_eq!(net.node(0).port.clock_state(), ClockState::FreeRunning);
}

#[test]
fn other_domains_are_not_heard() {
    let mut net = Net::new(100_000.0, 0.0);
    net.add(1, leader, 0.0);
    net.add(2, |id| PortConfig::new(id, 1, Profile::Media, false), 0.0);
    net.run(0, 20_000 * MS);
    assert_eq!(net.node(1).port.state(), PortState::Listening);
}

#[test]
fn datasets_compare_as_the_standard_says() {
    let d = |p1, class, gm: u8| Dataset {
        priority1: p1,
        quality: ClockQuality {
            class,
            accuracy: 0xFE,
            variance: 0xFFFF,
        },
        priority2: 128,
        grandmaster: ClockIdentity([0, 0, 0, 0, 0, 0, 0, gm]),
        steps_removed: 0,
        sender: PortIdentity::default(),
    };
    assert_eq!(d(128, 248, 9).compare(&d(250, 6, 1)), Ordering::Less);
    assert_eq!(d(128, 6, 9).compare(&d(128, 248, 1)), Ordering::Less);
    assert_eq!(d(128, 6, 1).compare(&d(128, 6, 9)), Ordering::Less);
    let mut far = d(128, 6, 1);
    far.steps_removed = 2;
    assert_eq!(far.compare(&d(128, 6, 1)), Ordering::Greater);
}
