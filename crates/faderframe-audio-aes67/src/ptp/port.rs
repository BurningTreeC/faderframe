//! One PTP port (IEEE 1588-2008, ordinary clock, end-to-end delay
//! mechanism) as a state machine without I/O: messages and timer ticks go
//! in with their local timestamps, messages to send come out. The service
//! (`super::service`) feeds it from sockets; tests feed it simulated
//! exchanges with delays and jitter.
//!
//! * **Best master** (9.3, simplified for one port): Announces from
//!   foreign masters are kept while they keep coming (a master is
//!   qualified after two within four of its announce intervals and
//!   dropped after `announceReceiptTimeout` intervals without one); the
//!   best by the data set comparison (priority 1, class, accuracy,
//!   variance, priority 2, identity, then steps removed and sender) is
//!   followed — unless this clock may lead and its own data set is
//!   better, or none is there for the announce timeout: then it leads.
//! * **Following** (slave): two-step Syncs are matched with their
//!   Follow_Ups (either may arrive first: they come on different
//!   sockets), Delay_Req/Delay_Resp measure the path; offsets go to the
//!   [`Servo`], which moves the [`VirtualClock`].
//! * **Leading** (master): Announce and two-step Sync/Follow_Up at the
//!   profile's intervals, a Delay_Resp for every Delay_Req, the clock left
//!   running as it was (a takeover is continuous).

use super::clock::{ClockState, TAI_OFFSET_S, VirtualClock};
use super::message::{
    Announce, Body, ClockIdentity, ClockQuality, Header, Message, MessageType, PortIdentity,
    Timestamp,
};
use super::servo::{Adjust, DelayFilter, Servo};
use std::cmp::Ordering;

/// The PTP profile: message intervals (as log₂ seconds).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    /// IEEE 1588 Annex J.3 (Sync every second).
    Default,
    /// AES67 Annex A, the media profile (Sync every 125 ms).
    #[default]
    Media,
}

impl Profile {
    pub fn announce_log(self) -> i8 {
        1
    }

    pub fn sync_log(self) -> i8 {
        match self {
            Profile::Default => 0,
            Profile::Media => -3,
        }
    }

    /// `logMinDelayReqInterval` this clock asks of its followers when it
    /// leads (a follower uses the master's).
    pub fn delay_req_log(self) -> i8 {
        0
    }

    /// `announceReceiptTimeout`.
    pub fn announce_timeout(self) -> u32 {
        3
    }

    pub fn label(self) -> &'static str {
        match self {
            Profile::Default => "IEEE 1588 default",
            Profile::Media => "AES67 media",
        }
    }
}

/// `timeSource` for a free-running oscillator (7.6.2.6).
const INTERNAL_OSCILLATOR: u8 = 0xA0;
/// Foreign masters kept at most.
const MAX_FOREIGN: usize = 8;

/// How this clock takes part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortConfig {
    pub domain: u8,
    pub identity: PortIdentity,
    pub profile: Profile,
    /// Lead when no better grandmaster is there (else only follow).
    pub may_lead: bool,
    pub priority1: u8,
    pub priority2: u8,
    pub quality: ClockQuality,
}

impl PortConfig {
    /// A clock that follows any grandmaster and leads only when none is
    /// there (priorities 250: real grandmasters win).
    pub fn new(identity: ClockIdentity, domain: u8, profile: Profile, may_lead: bool) -> Self {
        Self {
            domain,
            identity: PortIdentity {
                clock: identity,
                port: 1,
            },
            profile,
            may_lead,
            priority1: 250,
            priority2: 250,
            quality: ClockQuality::default(),
        }
    }
}

/// Where the port stands (9.2.5, the states this clock uses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortState {
    /// Waiting for a master's Announce.
    Listening,
    /// Leading the domain.
    Master,
    /// Following `parent`.
    Slave,
}

/// A message to send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub message: Message,
    /// The sender reports when it went (software timestamp) with
    /// [`Port::sent`].
    pub stamp: bool,
}

impl Outgoing {
    /// Event messages go to port 319, the others to 320.
    pub fn is_event(&self) -> bool {
        self.message.header.kind.is_event()
    }
}

/// A grandmaster as its Announces describe it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dataset {
    pub priority1: u8,
    pub quality: ClockQuality,
    pub priority2: u8,
    pub grandmaster: ClockIdentity,
    pub steps_removed: u16,
    pub sender: PortIdentity,
}

impl Dataset {
    fn of(header: &Header, a: &Announce) -> Self {
        Self {
            priority1: a.priority1,
            quality: a.quality,
            priority2: a.priority2,
            grandmaster: a.grandmaster,
            steps_removed: a.steps_removed,
            sender: header.source,
        }
    }

    /// The data set comparison (9.3.4): `Less` = `self` is better.
    pub fn compare(&self, other: &Self) -> Ordering {
        let key = |d: &Self| {
            (
                d.priority1,
                d.quality.class,
                d.quality.accuracy,
                d.quality.variance,
                d.priority2,
                d.grandmaster,
            )
        };
        key(self)
            .cmp(&key(other))
            .then(self.steps_removed.cmp(&other.steps_removed))
            .then(self.sender.cmp(&other.sender))
    }
}

#[derive(Clone, Copy, Debug)]
struct Foreign {
    data: Dataset,
    /// Local times of the last two Announces.
    last: u64,
    before: Option<u64>,
    /// Its announce interval (ns).
    interval: u64,
    utc_offset: i16,
}

impl Foreign {
    fn qualified(&self) -> bool {
        self.before
            .is_some_and(|b| self.last.saturating_sub(b) <= 4 * self.interval)
    }
}

/// How the port is getting on (for the status display).
#[derive(Clone, Debug, PartialEq)]
pub struct PortStatus {
    pub state: PortState,
    pub clock: ClockState,
    /// The grandmaster followed (or this clock, leading).
    pub grandmaster: Option<Dataset>,
    /// The last offset from it (ns) and the path delay.
    pub offset_ns: i64,
    pub path_delay_ns: Option<i64>,
    /// The local clock against the grandmaster (ppm; positive: fast).
    pub drift_ppm: f64,
    /// Times the clock stepped (the first lock, a new grandmaster).
    pub steps: u64,
    pub syncs: u64,
    pub delay_responses: u64,
    /// Masters heard (qualified or not).
    pub masters_heard: usize,
}

fn interval_ns(log: i8) -> u64 {
    let log = log.clamp(-7, 7);
    if log >= 0 {
        1_000_000_000u64 << log
    } else {
        1_000_000_000u64 >> (-log)
    }
}

/// One PTP port (see the module docs).
#[derive(Debug)]
pub struct Port {
    config: PortConfig,
    state: PortState,
    clock: VirtualClock,
    servo: Servo,
    delay: DelayFilter,
    foreign: Vec<Foreign>,
    parent: Option<Dataset>,
    /// A two-step Sync waiting for its Follow_Up: (sequence, local rx,
    /// correction ns).
    pending_sync: Option<(u16, u64, i64)>,
    /// A Follow_Up that came before its Sync: (sequence, t1 + its
    /// correction).
    early_follow_up: Option<(u16, Timestamp)>,
    /// The last complete Sync: master's send time and our receive time
    /// (PTP ns, corrected).
    last_sync: Option<(i128, i128)>,
    /// The Delay_Req in flight: sequence and its send time (PTP ns; `None`
    /// until the sender reported it).
    delay_req: Option<(u16, Option<i128>)>,
    delay_req_log: i8,
    next_delay_req: u64,
    sync_seq: u16,
    announce_seq: u16,
    delay_seq: u16,
    next_announce: u64,
    next_sync: u64,
    /// When the port began listening (it leads after the announce timeout
    /// with nobody better there).
    listening_since: u64,
    /// The clock jumped since `take_stepped` last looked.
    stepped: bool,
    steps: u64,
    syncs: u64,
    responses: u64,
    rng: u64,
}

impl Port {
    /// A port listening from local time `now`, its clock starting at
    /// `clock`.
    pub fn new(config: PortConfig, clock: VirtualClock, now: u64) -> Self {
        let seed = u64::from_be_bytes(config.identity.clock.0) ^ now;
        Self {
            config,
            state: PortState::Listening,
            clock,
            servo: Servo::new(),
            delay: DelayFilter::default(),
            foreign: Vec::new(),
            parent: None,
            pending_sync: None,
            early_follow_up: None,
            last_sync: None,
            delay_req: None,
            delay_req_log: 0,
            next_delay_req: now,
            sync_seq: 0,
            announce_seq: 0,
            delay_seq: 0,
            next_announce: now,
            next_sync: now,
            listening_since: now,
            stepped: false,
            steps: 0,
            syncs: 0,
            responses: 0,
            rng: seed | 1,
        }
    }

    pub fn config(&self) -> &PortConfig {
        &self.config
    }

    pub fn state(&self) -> PortState {
        self.state
    }

    pub fn clock(&self) -> VirtualClock {
        self.clock
    }

    /// Whether the clock jumped since the last call.
    pub fn take_stepped(&mut self) -> bool {
        std::mem::take(&mut self.stepped)
    }

    pub fn clock_state(&self) -> ClockState {
        match self.state {
            PortState::Master => ClockState::Master,
            PortState::Slave if self.servo.locked() => ClockState::Locked,
            PortState::Slave if self.servo.is_tracking() => ClockState::Locking,
            _ => ClockState::FreeRunning,
        }
    }

    pub fn status(&self) -> PortStatus {
        PortStatus {
            state: self.state,
            clock: self.clock_state(),
            grandmaster: match self.state {
                PortState::Master => Some(self.own()),
                _ => self.parent,
            },
            offset_ns: self.servo.last_offset,
            path_delay_ns: self.delay.delay(),
            drift_ppm: self.servo.drift_ppm(),
            steps: self.steps,
            syncs: self.syncs,
            delay_responses: self.responses,
            masters_heard: self.foreign.len(),
        }
    }

    /// This clock's own data set.
    pub fn own(&self) -> Dataset {
        Dataset {
            priority1: self.config.priority1,
            quality: self.config.quality,
            priority2: self.config.priority2,
            grandmaster: self.config.identity.clock,
            steps_removed: 0,
            sender: self.config.identity,
        }
    }

    fn random(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }

    fn header(&self, kind: MessageType, sequence: u16, log: i8) -> Header {
        let mut h = Header::new(kind, self.config.domain, self.config.identity, sequence);
        h.log_interval = log;
        h
    }

    /// A message `msg` arrived at local time `rx`.
    pub fn handle(&mut self, msg: &Message, rx: u64, out: &mut Vec<Outgoing>) {
        let h = &msg.header;
        if h.domain != self.config.domain || h.source.clock == self.config.identity.clock {
            return;
        }
        match msg.body {
            Body::Announce(a) => self.announce(h, &a, rx),
            Body::Sync { origin } => self.sync(h, origin, rx),
            Body::FollowUp { precise_origin } => self.follow_up(h, precise_origin),
            Body::DelayReq { .. } => self.delay_request(h, rx, out),
            Body::DelayResp {
                receive,
                requesting,
            } => self.delay_response(h, receive, requesting),
            Body::Other => {}
        }
    }

    fn announce(&mut self, h: &Header, a: &Announce, rx: u64) {
        if a.grandmaster == self.config.identity.clock {
            // Our own time come back through a boundary clock.
            return;
        }
        let data = Dataset::of(h, a);
        let interval = interval_ns(h.log_interval);
        match self
            .foreign
            .iter_mut()
            .find(|f| f.data.sender == data.sender)
        {
            Some(f) => {
                f.before = Some(f.last);
                f.last = rx;
                f.data = data;
                f.interval = interval;
                f.utc_offset = a.utc_offset;
            }
            None => {
                if self.foreign.len() >= MAX_FOREIGN {
                    // Forget the one heard least recently.
                    if let Some(i) = self
                        .foreign
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, f)| f.last)
                        .map(|(i, _)| i)
                    {
                        self.foreign.swap_remove(i);
                    }
                }
                self.foreign.push(Foreign {
                    data,
                    last: rx,
                    before: None,
                    interval,
                    utc_offset: a.utc_offset,
                });
            }
        }
        self.decide(rx);
    }

    /// The UTC offset the followed grandmaster announces (seconds).
    pub fn utc_offset(&self) -> i16 {
        self.parent
            .and_then(|p| self.foreign.iter().find(|f| f.data.sender == p.sender))
            .map_or(TAI_OFFSET_S as i16, |f| f.utc_offset)
    }

    /// Choose the state (9.3.3, for one port).
    fn decide(&mut self, now: u64) {
        let timeout = u64::from(self.config.profile.announce_timeout());
        self.foreign
            .retain(|f| now.saturating_sub(f.last) <= timeout * f.interval);
        let best = self
            .foreign
            .iter()
            .filter(|f| f.qualified())
            .map(|f| f.data)
            .min_by(Dataset::compare);
        let own = self.own();
        let lead_ok = self.config.may_lead && self.config.quality.class < 255;
        match best {
            Some(b) if !lead_ok || b.compare(&own) == Ordering::Less => {
                let new_parent = self.parent.is_none_or(|p| p.sender != b.sender);
                if new_parent || self.state != PortState::Slave {
                    self.follow(b, now);
                } else {
                    self.parent = Some(b);
                }
            }
            _ if lead_ok => {
                let waited = now.saturating_sub(self.listening_since)
                    >= timeout * interval_ns(self.config.profile.announce_log());
                if self.state != PortState::Master && (best.is_some() || waited) {
                    self.lead(now);
                }
            }
            _ => {
                if self.state == PortState::Slave {
                    self.listen(now);
                }
            }
        }
    }

    fn follow(&mut self, parent: Dataset, now: u64) {
        self.state = PortState::Slave;
        self.parent = Some(parent);
        self.servo.reset();
        self.delay.clear();
        self.pending_sync = None;
        self.early_follow_up = None;
        self.last_sync = None;
        self.delay_req = None;
        self.next_delay_req = now;
    }

    fn lead(&mut self, now: u64) {
        if self.state == PortState::Slave && self.servo.is_tracking() {
            // Run on at the master's frequency as found, without the
            // loop's momentary correction (followers see no jump).
            self.clock = self.clock.retuned(now, self.servo.steady_rate(), 0);
        }
        self.state = PortState::Master;
        self.parent = None;
        self.next_announce = now;
        self.next_sync = now;
    }

    fn listen(&mut self, now: u64) {
        self.state = PortState::Listening;
        self.parent = None;
        self.listening_since = now;
    }

    fn is_parent(&self, h: &Header) -> bool {
        self.state == PortState::Slave && self.parent.is_some_and(|p| p.sender == h.source)
    }

    fn sync(&mut self, h: &Header, origin: Timestamp, rx: u64) {
        if !self.is_parent(h) {
            return;
        }
        let corr = h.correction_ns();
        if h.flags.two_step {
            if let Some((seq, t1)) = self.early_follow_up
                && seq == h.sequence
            {
                self.early_follow_up = None;
                self.synced(i128::from(t1) + i128::from(corr), rx);
                return;
            }
            self.pending_sync = Some((h.sequence, rx, corr));
        } else {
            self.synced(i128::from(origin) + i128::from(corr), rx);
        }
    }

    fn follow_up(&mut self, h: &Header, precise: Timestamp) {
        if !self.is_parent(h) {
            return;
        }
        let t1 = precise as i128 + i128::from(h.correction_ns());
        match self.pending_sync {
            Some((seq, rx, corr)) if seq == h.sequence => {
                self.pending_sync = None;
                self.synced(t1 + i128::from(corr), rx);
            }
            _ => self.early_follow_up = Some((h.sequence, t1.max(0) as u64)),
        }
    }

    /// A complete Sync: sent at `t1` (master time), received at local `rx`.
    fn synced(&mut self, t1: i128, rx: u64) {
        self.syncs += 1;
        let t2 = i128::from(self.clock.ptp(rx));
        self.last_sync = Some((t1, t2));
        let Some(delay) = self.delay.delay() else {
            return;
        };
        let offset = (t2 - t1 - i128::from(delay)).clamp(i64::MIN.into(), i64::MAX.into()) as i64;
        match self.servo.sample(offset, rx) {
            Adjust::Hold => {}
            Adjust::Rate(rate) => self.clock = self.clock.retuned(rx, rate, 0),
            Adjust::Step { step, rate } => {
                self.clock = self.clock.retuned(rx, rate, step);
                self.stepped = true;
                self.steps += 1;
                // The Sync in hand is in the old time.
                self.last_sync = Some((t1, t2 + i128::from(step)));
            }
        }
    }

    fn delay_request(&mut self, h: &Header, rx: u64, out: &mut Vec<Outgoing>) {
        if self.state != PortState::Master {
            return;
        }
        let mut header = self.header(
            MessageType::DelayResp,
            h.sequence,
            self.config.profile.delay_req_log(),
        );
        header.correction = h.correction;
        out.push(Outgoing {
            message: Message::new(
                header,
                Body::DelayResp {
                    receive: self.clock.ptp(rx),
                    requesting: h.source,
                },
            ),
            stamp: false,
        });
    }

    fn delay_response(&mut self, h: &Header, receive: Timestamp, requesting: PortIdentity) {
        if !self.is_parent(h) || requesting != self.config.identity {
            return;
        }
        let Some((seq, Some(t3))) = self.delay_req else {
            return;
        };
        if seq != h.sequence {
            return;
        }
        self.delay_req = None;
        if (-7..=7).contains(&h.log_interval) {
            self.delay_req_log = h.log_interval;
        }
        let Some((t1, t2)) = self.last_sync else {
            return;
        };
        let t4 = i128::from(receive) - i128::from(h.correction_ns());
        let delay = ((t2 - t1) + (t4 - t3)) / 2;
        self.responses += 1;
        self.delay.push(delay.clamp(0, 1_000_000_000) as i64);
    }

    /// The timers at local time `now`: Announce/Sync when leading,
    /// Delay_Req when following, masters that went quiet.
    pub fn tick(&mut self, now: u64, out: &mut Vec<Outgoing>) {
        self.decide(now);
        match self.state {
            PortState::Master => {
                let p = self.config.profile;
                if now >= self.next_announce {
                    self.next_announce = now + interval_ns(p.announce_log());
                    let mut h = self.header(MessageType::Announce, self.announce_seq, 1);
                    h.log_interval = p.announce_log();
                    h.flags.ptp_timescale = true;
                    h.flags.utc_offset_valid = true;
                    self.announce_seq = self.announce_seq.wrapping_add(1);
                    out.push(Outgoing {
                        message: Message::new(
                            h,
                            Body::Announce(Announce {
                                origin: self.clock.ptp(now),
                                utc_offset: TAI_OFFSET_S as i16,
                                priority1: self.config.priority1,
                                quality: self.config.quality,
                                priority2: self.config.priority2,
                                grandmaster: self.config.identity.clock,
                                steps_removed: 0,
                                time_source: INTERNAL_OSCILLATOR,
                            }),
                        ),
                        stamp: false,
                    });
                }
                if now >= self.next_sync {
                    self.next_sync = now + interval_ns(p.sync_log());
                    let mut h = self.header(MessageType::Sync, self.sync_seq, p.sync_log());
                    h.flags.two_step = true;
                    h.flags.ptp_timescale = true;
                    self.sync_seq = self.sync_seq.wrapping_add(1);
                    out.push(Outgoing {
                        message: Message::new(
                            h,
                            Body::Sync {
                                origin: self.clock.ptp(now),
                            },
                        ),
                        stamp: true,
                    });
                }
            }
            PortState::Slave => {
                if now >= self.next_delay_req && self.last_sync.is_some() {
                    // Uniformly within twice the interval (9.5.11.2).
                    let spread = 2.0 * self.random();
                    self.next_delay_req =
                        now + (interval_ns(self.delay_req_log) as f64 * spread.max(0.1)) as u64;
                    let seq = self.delay_seq;
                    self.delay_seq = self.delay_seq.wrapping_add(1);
                    self.delay_req = Some((seq, None));
                    let h = self.header(MessageType::DelayReq, seq, 0x7F);
                    out.push(Outgoing {
                        message: Message::new(
                            h,
                            Body::DelayReq {
                                origin: self.clock.ptp(now),
                            },
                        ),
                        stamp: true,
                    });
                }
            }
            PortState::Listening => {}
        }
    }

    /// An outgoing message marked `stamp` left at local time `tx`: a Sync
    /// gets its Follow_Up, a Delay_Req's send time is kept.
    pub fn sent(&mut self, message: &Message, tx: u64, out: &mut Vec<Outgoing>) {
        let seq = message.header.sequence;
        match message.body {
            Body::Sync { .. } if self.state == PortState::Master => {
                let mut h = self.header(MessageType::FollowUp, seq, self.config.profile.sync_log());
                h.flags.ptp_timescale = true;
                out.push(Outgoing {
                    message: Message::new(
                        h,
                        Body::FollowUp {
                            precise_origin: self.clock.ptp(tx),
                        },
                    ),
                    stamp: false,
                });
            }
            Body::DelayReq { .. } => {
                if let Some((s, t3)) = &mut self.delay_req
                    && *s == seq
                {
                    *t3 = Some(i128::from(self.clock.ptp(tx)));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
