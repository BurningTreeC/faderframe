//! Realtime scheduling through RealtimeKit, for when the process may not
//! take it itself (no `RLIMIT_RTPRIO`: a user outside the `realtime` /
//! `audio` group): the `rtkit-daemon` on the system bus, or — inside a
//! Flatpak, where the system bus is out of reach — the desktop portal's
//! Realtime interface, which asks rtkit on the program's behalf. A small
//! D-Bus client of its own (the wire protocol for a few method calls), no
//! library.
//!
//! **Asked off the audio path.** A thread that wants realtime posts its
//! thread id and priority ([`request`]: atomics and a futex wake — no
//! allocation, lock or wait, so the audio thread itself may) and carries
//! on at its scheduling until then; the process's broker thread
//! ([`start`], `ff-rtkit`) asks the service *for* that thread
//! (`MakeThreadRealtime` takes any thread of the caller's process — as
//! PipeWire's own client does from a thread of its own), over one
//! connection it keeps. A thread asking for itself waited for the round
//! trips (a millisecond or more, seconds when the daemon is slow): a
//! sandboxed plugin's audio thread on its first block (late: a silent
//! block), cpal's audio thread in its first callback, DSP workers as they
//! took their first job.
//!
//! **Within RealtimeKit's limits.** rtkit allows each user 25 requests
//! (and one) in 20 s, 25 realtime threads and 15 processes at once —
//! counting every program of the user, PipeWire's own threads too — and
//! answers others with `AccessDenied` ("Device or resource busy"; it says
//! `AccessDenied` for a policy's no as well). Such an answer is a refusal
//! for now, never the service's absence (taken for one, the first refused
//! request turned rtkit off for the rest of the process). The broker asks
//! at most [`Config::burst`] times in [`Config::window`], highest priority
//! first (requests posted together are gathered for a moment), keeps at
//! most [`Config::threads`] threads realtime through the service — a
//! higher priority takes the place of the lowest one granted, which goes
//! back to normal —, and after a refusal pauses past rtkit's window (21 s,
//! doubling while refusals go on, up to about 5 minutes) and asks again:
//! refused requests are retried, never given up. A thread that is already
//! where it would be put is not asked for, one that has gone is forgotten,
//! and one waiting longer than [`Config::failure_after`] counts as a
//! priority failure (`priority::priority_failures`).
//!
//! RealtimeKit wants the process's `RLIMIT_RTTIME` set: a thread running
//! realtime that long without sleeping is stopped. It is set to rtkit's
//! maximum (hard) and half of it (soft): at the soft limit the kernel sends
//! the thread `SIGXCPU`, whose handler (`rttime`) returns it to normal
//! scheduling (counted as a priority failure) before the hard limit's
//! `SIGKILL` — and that margin is kept when others set the limit again
//! (PipeWire's client does on every context it makes).
//! `FADERFRAME_RTKIT=0` turns it off.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Where a request goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// `org.freedesktop.RealtimeKit1` on the system bus.
    RtKit,
    /// `org.freedesktop.portal.Realtime` on the session bus.
    Portal,
}

impl Via {
    fn destination(self) -> &'static str {
        match self {
            Via::RtKit => "org.freedesktop.RealtimeKit1",
            Via::Portal => "org.freedesktop.portal.Desktop",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Via::RtKit => "/org/freedesktop/RealtimeKit1",
            Via::Portal => "/org/freedesktop/portal/desktop",
        }
    }

    fn interface(self) -> &'static str {
        match self {
            Via::RtKit => "org.freedesktop.RealtimeKit1",
            Via::Portal => "org.freedesktop.portal.Realtime",
        }
    }

    fn bus(self) -> io::Result<Bus> {
        match self {
            Via::RtKit => Bus::system(),
            Via::Portal => Bus::session(),
        }
    }
}

/// What a service allows (read once).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_priority: i32,
    /// Microseconds a realtime thread may run without sleeping.
    pub rttime_max_us: i64,
}

/// What the services allow, `None` where there is none (for diagnostics;
/// a few round trips on the calling thread).
pub fn limits(via: Via) -> Option<Limits> {
    let mut bus = via.bus().ok()?;
    read_limits(&mut bus, via).ok()
}

/// Why a request was not granted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No such service (or it allows nothing): not asked again.
    Absent,
    /// Not now: the user's requests or realtime threads used up, a
    /// policy, rtkit's watchdog after a lockup (the service's words).
    Refused(String),
    /// The thread is gone.
    Gone,
}

// --- Requests ----------------------------------------------------------------

/// Threads that may have a request at once (more are not taken).
pub const MAX_REQUESTS: usize = 256;

/// In a request: the audio thread (whose scheduling the helpers follow,
/// `priority::note_audio_thread`, published when granted).
const AUDIO: u32 = 1 << 31;

struct Slot {
    /// `tid << 32 | n` (0: free; `n` = 0: claimed, nothing asked yet):
    /// the thread and how many requests it posted.
    who: AtomicU64,
    /// Its newest request: the priority, with [`AUDIO`].
    what: AtomicU32,
}

/// The requests of a process (tests make tables of their own).
pub(crate) struct Table {
    slots: [Slot; MAX_REQUESTS],
    /// Bumped by every request; the broker sleeps on it.
    posted: AtomicU32,
}

const THREAD: u64 = 0xFFFF_FFFF_0000_0000;

impl Table {
    pub(crate) const fn new() -> Self {
        Self {
            slots: [const {
                Slot {
                    who: AtomicU64::new(0),
                    what: AtomicU32::new(0),
                }
            }; MAX_REQUESTS],
            posted: AtomicU32::new(0),
        }
    }

    /// Post thread `tid`'s request for realtime at `priority`: its slot
    /// (asked again) or a free one, then the broker woken. Realtime-safe:
    /// atomics and a futex wake, which never blocks. False when it could
    /// not be posted (no room).
    pub(crate) fn post(&self, tid: i32, priority: i32, audio: bool) -> bool {
        if tid <= 0 || priority <= 0 {
            return false;
        }
        let what = priority.min(99) as u32 | if audio { AUDIO } else { 0 };
        let me = u64::from(tid as u32) << 32;
        // A slot may change under us (the broker freeing a gone thread's,
        // another thread taking a free one): a few tries.
        for _ in 0..8 {
            let mut free = None;
            let mut mine = None;
            for (i, s) in self.slots.iter().enumerate() {
                let w = s.who.load(Ordering::Acquire);
                if w == 0 {
                    free = free.or(Some(i));
                } else if w & THREAD == me {
                    mine = Some((i, w));
                    break;
                }
            }
            if let Some((i, w)) = mine {
                let s = &self.slots[i];
                s.what.store(what, Ordering::Release);
                let n = match (w as u32).wrapping_add(1) {
                    0 => 1,
                    n => n,
                };
                if s.who
                    .compare_exchange(w, me | u64::from(n), Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    self.wake();
                    return true;
                }
                continue;
            }
            let Some(i) = free else {
                return false;
            };
            let s = &self.slots[i];
            // Claimed first (the broker passes a claimed slot by), the
            // request written, then posted.
            if s.who
                .compare_exchange(0, me, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            s.what.store(what, Ordering::Release);
            s.who.store(me | 1, Ordering::Release);
            self.wake();
            return true;
        }
        false
    }

    fn wake(&self) {
        self.posted.fetch_add(1, Ordering::Release);
        futex::wake(&self.posted);
    }
}

/// This process's requests.
static TABLE: Table = Table::new();
/// `FADERFRAME_RTKIT=0`: nothing is posted.
static OFF: AtomicBool = AtomicBool::new(false);
static STARTED: std::sync::Once = std::sync::Once::new();
static COUNTERS: Counters = Counters::new();

/// Ask for thread `tid` (a thread of this process, `gettid`) to run
/// realtime at `priority` (capped by the service): posted to the broker,
/// which asks RealtimeKit or the portal. Never blocks, allocates or locks
/// — any thread may, the audio thread too. Whether it was posted.
pub fn request(tid: i32, priority: i32) -> bool {
    !OFF.load(Ordering::Relaxed) && TABLE.post(tid, priority, false)
}

/// [`request`] for the calling thread; `audio`: it is the audio callback's
/// thread (granted, its scheduling is published for the helpers to follow).
pub fn request_for_this_thread(priority: i32, audio: bool) -> bool {
    !OFF.load(Ordering::Relaxed) && TABLE.post(gettid(), priority, audio)
}

/// Start the broker that serves [`request`]s (a control thread; once per
/// process — later calls do nothing). Requests posted before it starts
/// wait for it.
pub fn start() {
    STARTED.call_once(|| {
        if std::env::var("FADERFRAME_RTKIT").as_deref() == Ok("0") {
            OFF.store(true, Ordering::Relaxed);
            return;
        }
        let broker = Broker::new(
            &TABLE,
            DbusService::default(),
            Kernel,
            Config::DEFAULT,
            &COUNTERS,
        )
        .publishing();
        let _ = std::thread::Builder::new()
            .name("ff-rtkit".into())
            .spawn(move || broker.run(None));
    });
}

/// What the broker did so far (diagnostics).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Requests sent to the service.
    pub asked: u64,
    pub granted: u64,
    /// Refused by it (each retried later).
    pub refused: u64,
    /// Grants given up for a higher priority's.
    pub displaced: u64,
    /// Threads realtime through the service now.
    pub realtime: usize,
    /// Threads waiting for their request.
    pub waiting: usize,
}

/// This process's [`Stats`].
pub fn stats() -> Stats {
    COUNTERS.read()
}

pub(crate) struct Counters {
    asked: AtomicU64,
    granted: AtomicU64,
    refused: AtomicU64,
    displaced: AtomicU64,
    realtime: AtomicUsize,
    waiting: AtomicUsize,
}

impl Counters {
    pub(crate) const fn new() -> Self {
        Self {
            asked: AtomicU64::new(0),
            granted: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            displaced: AtomicU64::new(0),
            realtime: AtomicUsize::new(0),
            waiting: AtomicUsize::new(0),
        }
    }

    pub(crate) fn read(&self) -> Stats {
        Stats {
            asked: self.asked.load(Ordering::Relaxed),
            granted: self.granted.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
            displaced: self.displaced.load(Ordering::Relaxed),
            realtime: self.realtime.load(Ordering::Relaxed),
            waiting: self.waiting.load(Ordering::Relaxed),
        }
    }
}

fn gettid() -> i32 {
    // SAFETY: plain system call without arguments.
    unsafe { libc::syscall(libc::SYS_gettid) as i32 }
}

mod futex {
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    /// Sleep while `word` is `expected`, at most `timeout`.
    pub fn wait(word: &AtomicU32, expected: u32, timeout: Duration) {
        let timeout = libc::timespec {
            tv_sec: timeout.as_secs().min(3600) as libc::time_t,
            tv_nsec: timeout.subsec_nanos() as libc::c_long,
        };
        // SAFETY: FUTEX_WAIT on a live, aligned u32 with a valid timeout;
        // returns at once if the value is no longer `expected`.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                word.as_ptr(),
                libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                expected,
                &timeout as *const libc::timespec,
            );
        }
    }

    /// Wake a sleeper on `word` (never blocks).
    pub fn wake(word: &AtomicU32) {
        // SAFETY: FUTEX_WAKE on a live, aligned u32.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                word.as_ptr(),
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                1,
            );
        }
    }
}

// --- The broker ----------------------------------------------------------------

/// How the broker paces itself; [`Config::DEFAULT`] fits rtkit's own
/// defaults.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    /// Requests to the service in any `window`, at most.
    pub burst: usize,
    pub window: Duration,
    /// After a refusal none for this long, doubling with every refusal in
    /// a row, up to `pause_max`.
    pub pause: Duration,
    pub pause_max: Duration,
    /// Threads realtime through the service at once, at most.
    pub threads: usize,
    /// Requests posted together are gathered this long (and then served
    /// highest priority first).
    pub gather: Duration,
    /// Gone threads and grants lost (rtkit's watchdog, the realtime time
    /// limit's handler) are looked for this often.
    pub sweep: Duration,
    /// A thread still waiting this long counts as a priority failure.
    pub failure_after: Duration,
}

impl Config {
    pub(crate) const DEFAULT: Config = Config {
        // rtkit: 25 + 1 per user in 20 s; the rest for the user's other
        // programs (PipeWire's daemon, its clients' data loops).
        burst: 16,
        window: Duration::from_secs(20),
        // Past rtkit's window, wherever in it the refusal came.
        pause: Duration::from_secs(21),
        pause_max: Duration::from_secs(336),
        // rtkit: 25 realtime threads per user, PipeWire's among them.
        threads: 16,
        gather: Duration::from_millis(20),
        sweep: Duration::from_secs(2),
        failure_after: Duration::from_secs(30),
    };
}

/// A realtime service: RealtimeKit or the portal (tests: a stand-in).
pub(crate) trait Service: Send {
    /// What it allows.
    fn limits(&mut self) -> Result<Limits, Refusal>;
    /// Make `tid`, a thread of this process, realtime at `priority`.
    fn make_realtime(&mut self, tid: i32, priority: i32) -> Result<(), Refusal>;
}

/// What the broker needs to know of the process's threads (the kernel's;
/// tests: a model).
pub(crate) trait Threads: Send {
    /// The thread's start time, `None` when it is gone.
    fn started(&self, tid: i32) -> Option<u64>;
    /// Its scheduling as `pool::sched::current` packs it (0: unknown).
    fn scheduling(&self, tid: i32) -> u64;
    /// Back to normal scheduling (a grant given up for a higher one).
    fn demote(&self, tid: i32);
    /// `RLIMIT_RTTIME` as the service wants it (`max_us` hard), once.
    fn limit_rttime(&self, max_us: i64) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// To ask for (new, refused, displaced, waiting for room).
    Pending,
    /// Made realtime through the service.
    Granted,
    /// Nothing to do: already there, lost after a grant (not asked again
    /// until it asks), or no service.
    Done,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    tid: i32,
    started: u64,
    /// The slot's word when last read.
    who: u64,
    priority: i32,
    audio: bool,
    state: State,
    /// Waiting since.
    since: Instant,
    /// Counted as a priority failure (once a request).
    counted: bool,
}

/// Serves a [`Table`]'s requests (see the module docs).
pub(crate) struct Broker<S, T> {
    table: &'static Table,
    service: S,
    threads: T,
    config: Config,
    counters: &'static Counters,
    entries: Vec<Option<Entry>>,
    limits: Option<Limits>,
    absent: bool,
    /// When requests were sent (within the window).
    asked: VecDeque<Instant>,
    paused_until: Option<Instant>,
    refused_in_a_row: u32,
    swept: Instant,
    /// Granted audio threads' scheduling published (`priority`).
    publish: bool,
}

impl<S: Service, T: Threads> Broker<S, T> {
    pub(crate) fn new(
        table: &'static Table,
        service: S,
        threads: T,
        config: Config,
        counters: &'static Counters,
    ) -> Self {
        Self {
            table,
            service,
            threads,
            config,
            counters,
            entries: vec![None; MAX_REQUESTS],
            limits: None,
            absent: false,
            asked: VecDeque::new(),
            paused_until: None,
            refused_in_a_row: 0,
            swept: Instant::now(),
            publish: false,
        }
    }

    /// Publish a granted audio thread's scheduling (the process's own
    /// broker; tests leave the process's state alone).
    fn publishing(mut self) -> Self {
        self.publish = true;
        self
    }

    /// Serve until `stop` (never, for the process's broker).
    pub(crate) fn run(mut self, stop: Option<&AtomicBool>) {
        // Never realtime itself (a maker that was, inherited): the
        // reset-on-fork flag kept, an unprivileged thread may not clear it.
        let normal = libc::sched_param { sched_priority: 0 };
        // SAFETY: the calling thread (0) to normal scheduling, a valid
        // param.
        unsafe {
            libc::syscall(
                libc::SYS_sched_setscheduler,
                0,
                libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK,
                &normal as *const libc::sched_param,
            )
        };
        let mut seen = self.table.posted.load(Ordering::Acquire);
        loop {
            if stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
                return;
            }
            let due = self.step(Instant::now());
            let wait = due
                .map_or(self.config.sweep, |d| {
                    d.saturating_duration_since(Instant::now())
                })
                .min(self.config.sweep)
                .max(Duration::from_millis(1));
            futex::wait(&self.table.posted, seen, wait);
            if self.table.posted.load(Ordering::Acquire) != seen {
                // Others come with it (a pool's workers, the helpers of a
                // new graph): all of them, then by priority.
                std::thread::sleep(self.config.gather);
                seen = self.table.posted.load(Ordering::Acquire);
            }
        }
    }

    /// Read the table, look after the threads, ask what may be asked;
    /// when the next request is due (a pause, the window), if one waits.
    pub(crate) fn step(&mut self, now: Instant) -> Option<Instant> {
        self.scan(now);
        if now.saturating_duration_since(self.swept) >= self.config.sweep {
            self.swept = now;
            self.sweep();
        }
        let due = self.serve(now);
        let (mut realtime, mut waiting) = (0, 0);
        for e in self.entries.iter_mut().flatten() {
            match e.state {
                State::Granted => realtime += 1,
                State::Pending => waiting += 1,
                State::Done => {}
            }
            if e.state == State::Pending
                && !e.counted
                && now.saturating_duration_since(e.since) >= self.config.failure_after
            {
                e.counted = true;
                crate::priority::note_failure();
            }
        }
        self.counters.realtime.store(realtime, Ordering::Relaxed);
        self.counters.waiting.store(waiting, Ordering::Relaxed);
        due
    }

    /// New requests in the table.
    fn scan(&mut self, now: Instant) {
        for (i, slot) in self.table.slots.iter().enumerate() {
            let who = slot.who.load(Ordering::Acquire);
            if who == 0 {
                self.entries[i] = None;
                continue;
            }
            if who as u32 == 0 || self.entries[i].is_some_and(|e| e.who == who) {
                continue;
            }
            let what = slot.what.load(Ordering::Acquire);
            let tid = (who >> 32) as u32 as i32;
            let Some(started) = self.threads.started(tid) else {
                // Gone before it was served.
                let _ = slot
                    .who
                    .compare_exchange(who, 0, Ordering::AcqRel, Ordering::Acquire);
                self.entries[i] = None;
                continue;
            };
            let priority = (what & !AUDIO) as i32;
            let audio = what & AUDIO != 0;
            let same = self.entries[i].filter(|e| {
                e.tid == tid
                    && e.started == started
                    && e.state == State::Granted
                    && e.priority == priority
            });
            self.entries[i] = Some(match same {
                // Asked again for what it still has: stays (asked again
                // after losing it — the time limit's demotion —: asked for).
                Some(e) if self.holds(&e) => Entry { who, audio, ..e },
                _ => Entry {
                    tid,
                    started,
                    who,
                    priority,
                    audio,
                    state: State::Pending,
                    since: now,
                    counted: false,
                },
            });
        }
    }

    /// Gone threads forgotten, grants lost noted.
    fn sweep(&mut self) {
        for (i, slot) in self.table.slots.iter().enumerate() {
            let Some(e) = self.entries[i] else {
                continue;
            };
            if self.threads.started(e.tid) != Some(e.started) {
                // Only if it did not ask again meanwhile (a new thread
                // with its id: its request stays).
                if slot
                    .who
                    .compare_exchange(e.who, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    self.entries[i] = None;
                }
                continue;
            }
            if e.state == State::Granted && !self.holds(&e) {
                // Demoted since (it ran too long, rtkit's watchdog): not
                // asked for again until the thread asks.
                if let Some(e) = self.entries[i].as_mut() {
                    e.state = State::Done;
                }
            }
        }
    }

    /// The priority `e` is given (the service's cap).
    fn capped(&self, e: &Entry) -> i32 {
        self.limits
            .map_or(e.priority, |l| e.priority.min(l.max_priority))
    }

    /// Whether `e`'s thread runs realtime at the priority it would be given.
    fn holds(&self, e: &Entry) -> bool {
        let packed = self.threads.scheduling(e.tid);
        crate::pool::sched::is_realtime(packed) && (packed - 1) as u32 as i32 == self.capped(e)
    }

    /// When a request may be sent next (`None`: now).
    fn blocked(&mut self, now: Instant) -> Option<Instant> {
        if let Some(t) = self.paused_until {
            if now < t {
                return Some(t);
            }
            self.paused_until = None;
        }
        while self
            .asked
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.config.window)
        {
            self.asked.pop_front();
        }
        (self.asked.len() >= self.config.burst)
            .then(|| self.asked.front().map(|t| *t + self.config.window))
            .flatten()
    }

    /// Ask for the pending requests, highest priority first, as far as
    /// the pace and the room allow.
    fn serve(&mut self, now: Instant) -> Option<Instant> {
        let mut pending: Vec<usize> = (0..MAX_REQUESTS)
            .filter(|&i| self.entries[i].is_some_and(|e| e.state == State::Pending))
            .collect();
        if pending.is_empty() {
            return None;
        }
        if self.absent {
            self.no_service(&pending);
            return None;
        }
        if self.limits.is_none() {
            match self.service.limits() {
                Ok(l)
                    if l.max_priority > 0
                        && l.rttime_max_us > 0
                        && self.threads.limit_rttime(l.rttime_max_us) =>
                {
                    self.limits = Some(l);
                }
                Ok(_) | Err(Refusal::Absent | Refusal::Gone) => {
                    self.absent = true;
                    self.no_service(&pending);
                    return None;
                }
                Err(Refusal::Refused(_)) => {
                    return Some(self.pause(now));
                }
            }
        }
        pending.sort_by_key(|&i| self.entries[i].map(|e| (std::cmp::Reverse(e.priority), e.since)));
        for i in pending {
            let Some(e) = self.entries[i] else {
                continue;
            };
            if self.holds(&e) {
                // Already there (asked for itself, or before).
                self.set_state(i, State::Done);
                continue;
            }
            if let Some(due) = self.blocked(now) {
                return Some(due);
            }
            let granted = self
                .entries
                .iter()
                .flatten()
                .filter(|e| e.state == State::Granted)
                .count();
            if granted >= self.config.threads {
                // The lowest grant below it makes room, else it waits.
                let lowest = (0..MAX_REQUESTS)
                    .filter_map(|j| self.entries[j].map(|g| (j, g)))
                    .filter(|(_, g)| g.state == State::Granted && g.priority < e.priority)
                    .min_by_key(|(_, g)| g.priority);
                let Some((j, g)) = lowest else {
                    continue;
                };
                self.threads.demote(g.tid);
                if let Some(g) = self.entries[j].as_mut() {
                    g.state = State::Pending;
                    g.since = now;
                }
                self.counters.displaced.fetch_add(1, Ordering::Relaxed);
            }
            let priority = self.capped(&e);
            self.counters.asked.fetch_add(1, Ordering::Relaxed);
            let answer = self.service.make_realtime(e.tid, priority);
            // Counted from its answer: the service took it before then, so
            // requests a window apart by this clock are a window apart by
            // the service's too, whatever the round trips took.
            let answered = Instant::now();
            self.asked.push_back(answered);
            match answer {
                Ok(()) => {
                    self.refused_in_a_row = 0;
                    self.counters.granted.fetch_add(1, Ordering::Relaxed);
                    self.set_state(i, State::Granted);
                    if self.publish {
                        // Pools look at their caller again (it may be
                        // this thread), helpers follow a granted audio
                        // thread.
                        crate::priority::note_grant();
                        if e.audio {
                            crate::priority::audio_thread_granted(self.threads.scheduling(e.tid));
                        }
                    }
                }
                Err(Refusal::Gone) => {
                    let _ = self.table.slots[i].who.compare_exchange(
                        e.who,
                        0,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                    self.entries[i] = None;
                }
                Err(Refusal::Refused(_)) => {
                    self.counters.refused.fetch_add(1, Ordering::Relaxed);
                    return Some(self.pause(answered));
                }
                Err(Refusal::Absent) => {
                    self.absent = true;
                    let rest: Vec<usize> = (0..MAX_REQUESTS)
                        .filter(|&i| self.entries[i].is_some_and(|e| e.state == State::Pending))
                        .collect();
                    self.no_service(&rest);
                    return None;
                }
            }
        }
        None
    }

    /// No request for a while after a refusal (longer while they go on).
    fn pause(&mut self, now: Instant) -> Instant {
        let pause = self
            .config
            .pause
            .saturating_mul(1 << self.refused_in_a_row.min(8))
            .min(self.config.pause_max);
        self.refused_in_a_row += 1;
        let until = now + pause;
        self.paused_until = Some(until);
        until
    }

    /// No service: the requests are failures (once each).
    fn no_service(&mut self, pending: &[usize]) {
        for &i in pending {
            if let Some(e) = self.entries[i].as_mut() {
                if !e.counted {
                    e.counted = true;
                    crate::priority::note_failure();
                }
                e.state = State::Done;
            }
        }
    }

    fn set_state(&mut self, i: usize, state: State) {
        if let Some(e) = self.entries[i].as_mut() {
            e.state = state;
        }
    }
}

/// The kernel's view of this process's threads.
pub(crate) struct Kernel;

impl Threads for Kernel {
    fn started(&self, tid: i32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).ok()?;
        // After the command's closing parenthesis: field 3 on; the start
        // time is field 22.
        let rest = &stat[stat.rfind(')')? + 1..];
        rest.split_whitespace().nth(19)?.parse().ok()
    }

    fn scheduling(&self, tid: i32) -> u64 {
        // SAFETY: plain queries of a thread of this process with a valid
        // out pointer.
        let policy = unsafe { libc::syscall(libc::SYS_sched_getscheduler, tid) };
        let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
        let r = unsafe {
            libc::syscall(
                libc::SYS_sched_getparam,
                tid,
                &mut param as *mut libc::sched_param,
            )
        };
        if policy < 0 || r != 0 {
            return 0;
        }
        crate::pool::sched::pack(policy as i32, param.sched_priority)
    }

    fn demote(&self, tid: i32) {
        let param = libc::sched_param { sched_priority: 0 };
        // SAFETY: a thread of this process back to normal scheduling with a
        // valid param (lowering needs no privilege; the reset-on-fork flag
        // kept: rtkit set it, and it may not be cleared).
        unsafe {
            libc::syscall(
                libc::SYS_sched_setscheduler,
                tid,
                libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK,
                &param as *const libc::sched_param,
            )
        };
    }

    fn limit_rttime(&self, max_us: i64) -> bool {
        limit_rttime(max_us)
    }
}

/// RealtimeKit on the system bus, else the portal, over one connection
/// kept open.
#[derive(Default)]
pub(crate) struct DbusService {
    /// The service that answered, and its connection.
    via: Option<Via>,
    bus: Option<Bus>,
    /// Tests: this address instead of the buses (as RealtimeKit).
    address: Option<String>,
}

impl DbusService {
    #[cfg(test)]
    fn at(address: String) -> Self {
        Self {
            address: Some(address),
            ..Self::default()
        }
    }

    fn connect(&self, via: Via) -> io::Result<Bus> {
        match &self.address {
            Some(a) => Bus::open(a),
            None => via.bus(),
        }
    }
}

impl Service for DbusService {
    fn limits(&mut self) -> Result<Limits, Refusal> {
        let services: &[Via] = if self.address.is_some() {
            &[Via::RtKit]
        } else {
            &[Via::RtKit, Via::Portal]
        };
        for &via in services {
            let Ok(mut bus) = self.connect(via) else {
                continue;
            };
            match read_limits(&mut bus, via) {
                Ok(l) if l.max_priority > 0 && l.rttime_max_us > 0 => {
                    self.via = Some(via);
                    self.bus = Some(bus);
                    return Ok(l);
                }
                // Allows nothing (the portal where rtkit is missing), or
                // is not there: the next.
                _ => {}
            }
        }
        Err(Refusal::Absent)
    }

    fn make_realtime(&mut self, tid: i32, priority: i32) -> Result<(), Refusal> {
        let via = self.via.ok_or(Refusal::Absent)?;
        let mut body = Enc::default();
        let (member, signature) = match via {
            Via::RtKit => {
                body.u64(tid as u32 as u64);
                ("MakeThreadRealtime", "tu")
            }
            Via::Portal => {
                body.u64(u64::from(std::process::id()));
                body.u64(tid as u32 as u64);
                ("MakeThreadRealtimeWithPID", "ttu")
            }
        };
        body.u32(priority as u32);
        let call = Call {
            destination: via.destination(),
            path: via.path(),
            interface: via.interface(),
            member,
            signature,
        };
        // A connection gone stale: once more on a new one.
        for _ in 0..2 {
            let bus = match self.bus.as_mut() {
                Some(b) => b,
                None => match self.connect(via) {
                    Ok(b) => self.bus.insert(b),
                    Err(e) => return Err(Refusal::Refused(format!("no connection: {e}"))),
                },
            };
            match bus.call(&call, &body.buf) {
                Ok(_) => return Ok(()),
                Err(Failed::Io(_)) => self.bus = None,
                Err(Failed::Error(name, text)) => return Err(refusal(&name, &text)),
            }
        }
        Err(Refusal::Refused("connection lost".into()))
    }
}

/// What a service's error means. rtkit answers its refusals —
/// `EBUSY` (a limit of the user's), `EPERM`, `EACCES` (policy) — with
/// `AccessDenied` and `strerror`'s words, a thread gone (`ENOENT`) with
/// `Failed`; only a missing service, or the bus refusing to carry the
/// call at all, is an absence.
fn refusal(name: &str, text: &str) -> Refusal {
    if text.contains("No such file or directory") || text.contains("No such process") {
        return Refusal::Gone;
    }
    match name {
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner"
        | "org.freedesktop.DBus.Error.UnknownMethod"
        | "org.freedesktop.DBus.Error.UnknownInterface"
        | "org.freedesktop.DBus.Error.UnknownObject"
        | "org.freedesktop.DBus.Error.Spawn.ServiceNotFound" => Refusal::Absent,
        // The bus's own policy: "Rejected send message, …".
        "org.freedesktop.DBus.Error.AccessDenied" if text.starts_with("Rejected") => {
            Refusal::Absent
        }
        _ => Refusal::Refused(format!("{name}: {text}")),
    }
}

fn read_limits(bus: &mut Bus, via: Via) -> Result<Limits, Failed> {
    let mut get = |name: &str| -> Result<i64, Failed> {
        let mut body = Enc::default();
        body.string(via.interface());
        body.string(name);
        let reply = bus.call(
            &Call {
                destination: via.destination(),
                path: via.path(),
                interface: "org.freedesktop.DBus.Properties",
                member: "Get",
                signature: "ss",
            },
            &body.buf,
        )?;
        reply
            .variant_integer()
            .ok_or_else(|| Failed::Io(io::Error::other("unexpected property value")))
    };
    Ok(Limits {
        max_priority: get("MaxRealtimePriority")? as i32,
        rttime_max_us: get("RTTimeUSecMax")?,
    })
}

/// Set `RLIMIT_RTTIME` as rtkit wants it (hard at most its maximum, soft
/// below the hard limit) and catch `SIGXCPU` (once per process).
fn limit_rttime(max_us: i64) -> bool {
    static DONE: Mutex<bool> = Mutex::new(false);
    let Ok(mut done) = DONE.lock() else {
        return false;
    };
    if *done {
        return true;
    }
    let Some((soft, hard)) = crate::rttime::limits() else {
        return false;
    };
    let max = max_us as libc::rlim_t;
    let hard = if hard == libc::RLIM_INFINITY {
        max
    } else {
        hard.min(max)
    };
    let new = libc::rlimit {
        rlim_cur: soft.min(hard),
        rlim_max: hard,
    };
    // SAFETY: a valid rlimit; lowering limits needs no privilege.
    if unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &new) } != 0 {
        return false;
    }
    // The soft limit below the hard one, `SIGXCPU` caught (`rttime`), and
    // helpers that compute for long stretches breathe within it.
    crate::priority::note_rttime_limit();
    *done = true;
    true
}

// --- D-Bus -----------------------------------------------------------------

const TIMEOUT: Duration = Duration::from_secs(5);

/// A method call's addressing.
struct Call<'a> {
    destination: &'a str,
    path: &'a str,
    interface: &'a str,
    member: &'a str,
    signature: &'a str,
}

#[derive(Debug)]
enum Failed {
    Io(io::Error),
    /// The error's name and message.
    Error(String, String),
}

impl From<io::Error> for Failed {
    fn from(e: io::Error) -> Self {
        Failed::Io(e)
    }
}

/// A connection to a bus, past authentication and `Hello`.
struct Bus {
    socket: UnixStream,
    serial: u32,
}

impl Bus {
    fn system() -> io::Result<Self> {
        let address = std::env::var("DBUS_SYSTEM_BUS_ADDRESS")
            .unwrap_or_else(|_| "unix:path=/run/dbus/system_bus_socket".into());
        Self::open(&address)
    }

    fn session() -> io::Result<Self> {
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "no session bus"))?;
        Self::open(&address)
    }

    /// The first address of `address` (`;`-separated) that connects.
    fn open(address: &str) -> io::Result<Self> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no usable bus address");
        for one in address.split(';') {
            match Self::connect(one) {
                Ok(bus) => return Ok(bus),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn connect(address: &str) -> io::Result<Self> {
        let unsupported = || io::Error::new(io::ErrorKind::Unsupported, "bus address");
        let rest = address.strip_prefix("unix:").ok_or_else(unsupported)?;
        let mut socket = None;
        for pair in rest.split(',') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = unescape(value);
            match key {
                "path" => socket = Some(UnixStream::connect(&value)?),
                "abstract" => {
                    use std::os::linux::net::SocketAddrExt;
                    let addr = std::os::unix::net::SocketAddr::from_abstract_name(&value)?;
                    socket = Some(UnixStream::connect_addr(&addr)?);
                }
                _ => {}
            }
        }
        let socket = socket.ok_or_else(unsupported)?;
        socket.set_read_timeout(Some(TIMEOUT))?;
        socket.set_write_timeout(Some(TIMEOUT))?;
        let mut bus = Self { socket, serial: 0 };
        bus.authenticate()?;
        bus.call(
            &Call {
                destination: "org.freedesktop.DBus",
                path: "/org/freedesktop/DBus",
                interface: "org.freedesktop.DBus",
                member: "Hello",
                signature: "",
            },
            &[],
        )
        .map_err(|e| match e {
            Failed::Io(e) => e,
            Failed::Error(name, text) => io::Error::other(format!("{name}: {text}")),
        })?;
        Ok(bus)
    }

    /// SASL `EXTERNAL`: the kernel vouches for our user id.
    fn authenticate(&mut self) -> io::Result<()> {
        // SAFETY: plain call.
        let uid = unsafe { libc::getuid() }.to_string();
        let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
        self.socket
            .write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())?;
        let line = self.line()?;
        if !line.starts_with("OK ") {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("bus authentication: {line}"),
            ));
        }
        self.socket.write_all(b"BEGIN\r\n")
    }

    fn line(&mut self) -> io::Result<String> {
        let mut line = Vec::new();
        let mut b = [0u8];
        while !line.ends_with(b"\r\n") {
            self.socket.read_exact(&mut b)?;
            line.push(b[0]);
            if line.len() > 4096 {
                return Err(io::Error::other("bus line too long"));
            }
        }
        line.truncate(line.len() - 2);
        Ok(String::from_utf8_lossy(&line).into_owned())
    }

    /// Call and wait for the answer (messages in between — signals — are
    /// passed over).
    fn call(&mut self, call: &Call<'_>, body: &[u8]) -> Result<Reply, Failed> {
        self.serial += 1;
        let serial = self.serial;
        self.socket.write_all(&encode(serial, call, body))?;
        loop {
            let m = self.read()?;
            if m.reply_to != Some(serial) {
                continue;
            }
            return match m.kind {
                METHOD_RETURN => Ok(Reply {
                    body: m.body,
                    little: m.little,
                }),
                ERROR => {
                    let text = Dec::new(&m.body, m.little).string().unwrap_or_default();
                    Err(Failed::Error(m.error.unwrap_or_default(), text))
                }
                _ => continue,
            };
        }
    }

    fn read(&mut self) -> io::Result<Message> {
        let mut head = [0u8; 16];
        self.socket.read_exact(&mut head)?;
        let little = match head[0] {
            b'l' => true,
            b'B' => false,
            _ => return Err(io::Error::other("not a D-Bus message")),
        };
        let word = |i: usize| {
            let b = [head[i], head[i + 1], head[i + 2], head[i + 3]];
            if little {
                u32::from_le_bytes(b)
            } else {
                u32::from_be_bytes(b)
            }
        };
        let (body_len, fields_len) = (word(4) as usize, word(12) as usize);
        if body_len > 1 << 20 || fields_len > 1 << 16 {
            return Err(io::Error::other("D-Bus message too large"));
        }
        let padded = (16 + fields_len).next_multiple_of(8) - 16;
        let mut rest = vec![0u8; padded + body_len];
        self.socket.read_exact(&mut rest)?;
        let mut whole = head.to_vec();
        whole.extend_from_slice(&rest[..fields_len]);
        let fields = header_fields(&whole, little).unwrap_or_default();
        Ok(Message {
            serial: word(8),
            member: fields.member,
            reply_to: fields.reply_to,
            error: fields.error,
            kind: head[1],
            body: rest[padded..].to_vec(),
            little,
        })
    }
}

const METHOD_RETURN: u8 = 2;
const ERROR: u8 = 3;

struct Message {
    /// Read by the tests' stand-in service (a client only needs answers).
    #[cfg_attr(not(test), allow(dead_code))]
    serial: u32,
    #[cfg_attr(not(test), allow(dead_code))]
    member: Option<String>,
    kind: u8,
    reply_to: Option<u32>,
    error: Option<String>,
    body: Vec<u8>,
    little: bool,
}

struct Reply {
    body: Vec<u8>,
    little: bool,
}

impl Reply {
    /// A `Properties.Get` answer: a variant holding an integer.
    fn variant_integer(&self) -> Option<i64> {
        let mut d = Dec::new(&self.body, self.little);
        let signature = d.signature()?;
        match signature.as_str() {
            "i" => d.u32().map(|v| i64::from(v as i32)),
            "u" => d.u32().map(i64::from),
            "x" | "t" => d.u64().map(|v| v as i64),
            _ => None,
        }
    }
}

/// Header fields a call or answer is known by.
#[derive(Debug, Default, PartialEq, Eq)]
struct Fields {
    reply_to: Option<u32>,
    error: Option<String>,
    member: Option<String>,
}

/// The reply serial, error name and member among a message's header
/// fields (`whole` = the fixed header and the fields).
fn header_fields(whole: &[u8], little: bool) -> Option<Fields> {
    let mut d = Dec::new(whole, little);
    d.pos = 16;
    let mut f = Fields::default();
    while d.pos < whole.len() {
        d.align(8);
        if d.pos >= whole.len() {
            break;
        }
        let code = d.u8()?;
        let signature = d.signature()?;
        match signature.as_str() {
            "u" => {
                let v = d.u32()?;
                if code == 5 {
                    f.reply_to = Some(v);
                }
            }
            "s" | "o" => {
                let v = d.string()?;
                match code {
                    3 => f.member = Some(v),
                    4 => f.error = Some(v),
                    _ => {}
                }
            }
            "g" => {
                d.signature()?;
            }
            _ => return None,
        }
    }
    Some(f)
}

/// A method call, little-endian.
fn encode(serial: u32, call: &Call<'_>, body: &[u8]) -> Vec<u8> {
    let mut m = Enc::default();
    m.buf.extend_from_slice(&[b'l', 1, 0, 1]);
    m.u32(body.len() as u32);
    m.u32(serial);
    let length_at = m.buf.len();
    m.u32(0);
    let start = m.buf.len();
    m.field(1, b'o', call.path);
    m.field(2, b's', call.interface);
    m.field(3, b's', call.member);
    m.field(6, b's', call.destination);
    if !call.signature.is_empty() {
        m.align(8);
        m.buf.push(8);
        m.signature("g");
        m.signature(call.signature);
    }
    let length = (m.buf.len() - start) as u32;
    m.buf[length_at..length_at + 4].copy_from_slice(&length.to_le_bytes());
    m.align(8);
    m.buf.extend_from_slice(body);
    m.buf
}

/// Little-endian marshalling (offsets from the message's start: bodies
/// start 8-aligned, so a body's own offsets align the same).
#[derive(Default)]
struct Enc {
    buf: Vec<u8>,
}

impl Enc {
    fn align(&mut self, n: usize) {
        while !self.buf.len().is_multiple_of(n) {
            self.buf.push(0);
        }
    }

    fn u32(&mut self, v: u32) {
        self.align(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.align(8);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    fn signature(&mut self, s: &str) {
        self.buf.push(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// A header field: code, variant of a string type.
    fn field(&mut self, code: u8, kind: u8, value: &str) {
        self.align(8);
        self.buf.push(code);
        self.buf.extend_from_slice(&[1, kind, 0]);
        self.string(value);
    }
}

struct Dec<'a> {
    buf: &'a [u8],
    pos: usize,
    little: bool,
}

impl<'a> Dec<'a> {
    fn new(buf: &'a [u8], little: bool) -> Self {
        Self {
            buf,
            pos: 0,
            little,
        }
    }

    fn align(&mut self, n: usize) {
        self.pos = self.pos.next_multiple_of(n);
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.buf.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u32(&mut self) -> Option<u32> {
        self.align(4);
        let b: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn u64(&mut self) -> Option<u64> {
        self.align(8);
        let b: [u8; 8] = self.take(8)?.try_into().ok()?;
        Some(if self.little {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }

    fn string(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let s = self.take(n + 1)?;
        Some(String::from_utf8_lossy(&s[..n]).into_owned())
    }

    fn signature(&mut self) -> Option<String> {
        let n = usize::from(self.u8()?);
        let s = self.take(n + 1)?;
        Some(String::from_utf8_lossy(&s[..n]).into_owned())
    }
}

/// A bus address value with `%xx` escapes undone.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A call marshals as the specification lays it out: fixed header,
    /// fields (8-aligned structs of code and variant), body 8-aligned.
    #[test]
    fn a_call_is_marshalled_by_the_book() {
        let mut body = Enc::default();
        body.u64(1234);
        body.u32(20);
        let m = encode(
            7,
            &Call {
                destination: "org.freedesktop.RealtimeKit1",
                path: "/org/freedesktop/RealtimeKit1",
                interface: "org.freedesktop.RealtimeKit1",
                member: "MakeThreadRealtime",
                signature: "tu",
            },
            &body.buf,
        );
        assert_eq!(&m[..4], &[b'l', 1, 0, 1]);
        assert_eq!(u32::from_le_bytes(m[4..8].try_into().unwrap()), 12);
        assert_eq!(u32::from_le_bytes(m[8..12].try_into().unwrap()), 7);
        let fields = u32::from_le_bytes(m[12..16].try_into().unwrap()) as usize;
        let body_at = (16 + fields).next_multiple_of(8);
        assert_eq!(m.len(), body_at + 12);
        // The first field: PATH, an object path.
        assert_eq!(&m[16..20], &[1, 1, b'o', 0]);
        assert_eq!(u32::from_le_bytes(m[20..24].try_into().unwrap()), 29);
        assert_eq!(&m[24..53], b"/org/freedesktop/RealtimeKit1");
        // The signature field, last.
        assert_eq!(
            &m[16 + fields - 8..16 + fields],
            &[8, 1, b'g', 0, 2, b't', b'u', 0]
        );
        // The body: the thread (8-aligned), the priority.
        assert_eq!(
            u64::from_le_bytes(m[body_at..body_at + 8].try_into().unwrap()),
            1234
        );
        assert_eq!(
            u32::from_le_bytes(m[body_at + 8..body_at + 12].try_into().unwrap()),
            20
        );
        // Its header fields read back.
        assert_eq!(
            header_fields(&m[..16 + fields], true),
            Some(Fields {
                member: Some("MakeThreadRealtime".into()),
                ..Fields::default()
            })
        );
    }

    #[test]
    fn replies_and_errors_are_read() {
        // A reply to serial 3 with an error name, as a big-endian peer
        // would send it.
        let mut m = vec![b'B', ERROR, 0, 1, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0];
        let start = m.len();
        m.extend_from_slice(&[5, 1, b'u', 0, 0, 0, 0, 3]);
        let name = "org.freedesktop.DBus.Error.ServiceUnknown";
        m.extend_from_slice(&[4, 1, b's', 0]);
        m.extend_from_slice(&(name.len() as u32).to_be_bytes());
        m.extend_from_slice(name.as_bytes());
        m.push(0);
        let len = (m.len() - start) as u32;
        m[12..16].copy_from_slice(&len.to_be_bytes());
        assert_eq!(
            header_fields(&m, false),
            Some(Fields {
                reply_to: Some(3),
                error: Some(name.to_string()),
                member: None,
            })
        );
        // A variant holding an int64.
        let mut v = Enc::default();
        v.signature("x");
        v.u64(200_000);
        let r = Reply {
            body: v.buf,
            little: true,
        };
        assert_eq!(r.variant_integer(), Some(200_000));
    }

    /// An answer to `serial` (a method return with `signature`/`body`).
    fn answer(reply_to: u32, signature: &str, body: &[u8]) -> Vec<u8> {
        let mut m = Enc::default();
        m.buf.extend_from_slice(&[b'l', METHOD_RETURN, 0, 1]);
        m.u32(body.len() as u32);
        m.u32(reply_to + 1000);
        let length_at = m.buf.len();
        m.u32(0);
        let start = m.buf.len();
        m.align(8);
        m.buf.extend_from_slice(&[5, 1, b'u', 0]);
        m.u32(reply_to);
        if !signature.is_empty() {
            m.align(8);
            m.buf.push(8);
            m.signature("g");
            m.signature(signature);
        }
        let length = (m.buf.len() - start) as u32;
        m.buf[length_at..length_at + 4].copy_from_slice(&length.to_le_bytes());
        m.align(8);
        m.buf.extend_from_slice(body);
        m.buf
    }

    /// An error answer to `reply_to` (as rtkit sends its refusals).
    fn error(reply_to: u32, name: &str, text: &str) -> Vec<u8> {
        let mut body = Enc::default();
        body.string(text);
        let mut m = Enc::default();
        m.buf.extend_from_slice(&[b'l', ERROR, 0, 1]);
        m.u32(body.buf.len() as u32);
        m.u32(reply_to + 2000);
        let length_at = m.buf.len();
        m.u32(0);
        let start = m.buf.len();
        m.align(8);
        m.buf.extend_from_slice(&[5, 1, b'u', 0]);
        m.u32(reply_to);
        m.field(4, b's', name);
        m.align(8);
        m.buf.push(8);
        m.signature("g");
        m.signature("s");
        let length = (m.buf.len() - start) as u32;
        m.buf[length_at..length_at + 4].copy_from_slice(&length.to_le_bytes());
        m.align(8);
        m.buf.extend_from_slice(&body.buf);
        m.buf
    }

    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    /// rtkit's limits as a stand-in keeps them (its defaults: 20, 200 ms,
    /// 25 + 1 requests in 20 s, 25 threads; tests shrink the time).
    #[derive(Clone, Copy)]
    struct Model {
        max_priority: i32,
        rttime_us: i64,
        /// Requests of the user in a window: the one opening it and
        /// `burst` more.
        burst: usize,
        window: Duration,
        threads: usize,
        /// Before each answer (a slow daemon).
        delay: Duration,
        /// Grant for real (`sched_setscheduler`) rather than in the model.
        kernel: bool,
        /// Realtime threads the user's other programs hold (PipeWire's).
        others: usize,
    }

    impl Model {
        fn quick() -> Self {
            Self {
                max_priority: 20,
                rttime_us: 60_000_000,
                burst: 25,
                window: Duration::from_millis(600),
                threads: 25,
                delay: Duration::ZERO,
                kernel: false,
                others: 0,
            }
        }
    }

    /// The process's threads as the tests have them: which live (and
    /// since when), which run realtime at what (the stand-in grants into
    /// it), which the broker demoted.
    #[derive(Clone, Default)]
    struct World(Arc<Mutex<Inhabitants>>);

    #[derive(Default)]
    struct Inhabitants {
        alive: HashMap<i32, u64>,
        realtime: HashMap<i32, i32>,
        demoted: Vec<i32>,
    }

    impl World {
        fn with_threads(tids: impl IntoIterator<Item = i32>) -> Self {
            let w = Self::default();
            w.0.lock()
                .unwrap()
                .alive
                .extend(tids.into_iter().map(|t| (t, 1000 + t as u64)));
            w
        }

        fn realtime(&self) -> HashMap<i32, i32> {
            self.0.lock().unwrap().realtime.clone()
        }
    }

    impl Threads for World {
        fn started(&self, tid: i32) -> Option<u64> {
            self.0.lock().unwrap().alive.get(&tid).copied()
        }

        fn scheduling(&self, tid: i32) -> u64 {
            let w = self.0.lock().unwrap();
            match w.realtime.get(&tid) {
                Some(&p) => crate::pool::sched::pack(libc::SCHED_RR | libc::SCHED_RESET_ON_FORK, p),
                None => crate::pool::sched::pack(libc::SCHED_OTHER, 0),
            }
        }

        fn demote(&self, tid: i32) {
            let mut w = self.0.lock().unwrap();
            w.realtime.remove(&tid);
            w.demoted.push(tid);
        }

        fn limit_rttime(&self, _: i64) -> bool {
            true
        }
    }

    /// A `MakeThreadRealtime` the stand-in had: when, the thread, the
    /// priority, granted.
    type Asked = (Instant, i32, i32, bool);

    /// A stand-in for rtkit on a bus of its own: authentication, `Hello`,
    /// its properties, and `MakeThreadRealtime` under its limits, refused
    /// as it refuses (the thread limit, the priority, the burst).
    struct StandIn {
        address: String,
        dir: std::path::PathBuf,
        /// Every `MakeThreadRealtime`: when, the thread, the priority,
        /// granted.
        calls: Arc<Mutex<Vec<Asked>>>,
        /// The user's window: its start and requests in it.
        burst: Arc<Mutex<(Instant, usize)>>,
    }

    impl StandIn {
        fn start(name: &str, model: Model, world: World) -> Self {
            let dir = std::env::temp_dir().join(format!("ff-rtkit-{}-{name}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("bus");
            let _ = std::fs::remove_file(&path);
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let burst = Arc::new(Mutex::new((Instant::now() - model.window, 0)));
            let (c, b) = (Arc::clone(&calls), Arc::clone(&burst));
            std::thread::spawn(move || {
                // The user's realtime threads (rtkit's list, collected).
                let mut granted: HashSet<i32> = HashSet::new();
                for socket in listener.incoming() {
                    let Ok(socket) = socket else { return };
                    let mut bus = Bus { socket, serial: 0 };
                    let mut nul = [0u8];
                    if bus.socket.read_exact(&mut nul).is_err() {
                        continue;
                    }
                    let Ok(auth) = bus.line() else { continue };
                    assert!(auth.starts_with("AUTH EXTERNAL "), "{auth}");
                    bus.socket.write_all(b"OK 0123456789abcdef\r\n").unwrap();
                    if bus.line().ok().as_deref() != Some("BEGIN") {
                        continue;
                    }
                    while let Ok(m) = bus.read() {
                        let member = m.member.clone().unwrap_or_default();
                        let reply = match member.as_str() {
                            "Hello" => {
                                let mut body = Enc::default();
                                body.string(":1.7");
                                answer(m.serial, "s", &body.buf)
                            }
                            "Get" => {
                                let mut d = Dec::new(&m.body, m.little);
                                assert_eq!(d.string().unwrap(), "org.freedesktop.RealtimeKit1");
                                let mut body = Enc::default();
                                match d.string().unwrap().as_str() {
                                    "MaxRealtimePriority" => {
                                        body.signature("i");
                                        body.u32(model.max_priority as u32);
                                    }
                                    "RTTimeUSecMax" => {
                                        body.signature("x");
                                        body.u64(model.rttime_us as u64);
                                    }
                                    other => panic!("{other}"),
                                }
                                answer(m.serial, "v", &body.buf)
                            }
                            "MakeThreadRealtime" => {
                                std::thread::sleep(model.delay);
                                let mut d = Dec::new(&m.body, m.little);
                                let tid = d.u64().unwrap() as i32;
                                let priority = d.u32().unwrap() as i32;
                                let now = Instant::now();
                                // Threads no longer realtime leave its list.
                                if !model.kernel {
                                    let rt = world.realtime();
                                    granted.retain(|t| rt.contains_key(t));
                                }
                                let busy = "Device or resource busy";
                                let verdict = if !granted.contains(&tid)
                                    && granted.len() + model.others >= model.threads
                                {
                                    Err(busy)
                                } else if priority > model.max_priority {
                                    Err("Operation not permitted")
                                } else {
                                    let mut w = b.lock().unwrap();
                                    if now >= w.0 + model.window {
                                        *w = (now, 0);
                                        Ok(())
                                    } else if w.1 >= model.burst {
                                        Err(busy)
                                    } else {
                                        w.1 += 1;
                                        Ok(())
                                    }
                                };
                                let verdict = verdict.and_then(|()| {
                                    if !model.kernel {
                                        world.0.lock().unwrap().realtime.insert(tid, priority);
                                        return Ok(());
                                    }
                                    let param = libc::sched_param {
                                        sched_priority: priority,
                                    };
                                    // SAFETY: a thread of this process, a
                                    // valid param (as rtkit sets it).
                                    let r = unsafe {
                                        libc::syscall(
                                            libc::SYS_sched_setscheduler,
                                            tid,
                                            libc::SCHED_RR | libc::SCHED_RESET_ON_FORK,
                                            &param as *const libc::sched_param,
                                        )
                                    };
                                    if r == 0 {
                                        Ok(())
                                    } else {
                                        Err("Operation not permitted")
                                    }
                                });
                                c.lock()
                                    .unwrap()
                                    .push((now, tid, priority, verdict.is_ok()));
                                match verdict {
                                    Ok(()) => {
                                        granted.insert(tid);
                                        answer(m.serial, "", &[])
                                    }
                                    Err(text) => error(
                                        m.serial,
                                        "org.freedesktop.DBus.Error.AccessDenied",
                                        text,
                                    ),
                                }
                            }
                            other => panic!("{other}"),
                        };
                        if bus.socket.write_all(&reply).is_err() {
                            break;
                        }
                    }
                }
            });
            Self {
                address: format!("unix:path={}", path.display()),
                dir,
                calls,
                burst,
            }
        }

        /// Other programs of the user have used up its window just now.
        fn spend_window(&self, model: &Model) {
            *self.burst.lock().unwrap() = (Instant::now(), model.burst);
        }

        fn calls(&self) -> Vec<Asked> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Drop for StandIn {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A broker over a table of its own (not the process's), in a thread
    /// until dropped.
    struct Running {
        table: &'static Table,
        counters: &'static Counters,
        stop: &'static AtomicBool,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Running {
        fn start<T: Threads + 'static>(address: &str, threads: T, config: Config) -> Self {
            let table: &'static Table = Box::leak(Box::new(Table::new()));
            let counters: &'static Counters = Box::leak(Box::new(Counters::new()));
            let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
            let broker = Broker::new(
                table,
                DbusService::at(address.to_string()),
                threads,
                config,
                counters,
            );
            let thread = std::thread::spawn(move || broker.run(Some(stop)));
            Self {
                table,
                counters,
                stop,
                thread: Some(thread),
            }
        }

        /// Wait (at most `seconds`) for `done`.
        fn wait_for(&self, seconds: u64, mut done: impl FnMut() -> bool) -> bool {
            let until = Instant::now() + Duration::from_secs(seconds);
            while Instant::now() < until {
                if done() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            done()
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.table.wake();
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn quick_config() -> Config {
        Config {
            burst: 100,
            window: Duration::from_millis(600),
            pause: Duration::from_millis(300),
            pause_max: Duration::from_secs(2),
            threads: 25,
            gather: Duration::from_millis(5),
            sweep: Duration::from_millis(30),
            failure_after: Duration::from_secs(30),
        }
    }

    /// The service's answers, told apart: rtkit's refusals (its `EBUSY`,
    /// `EPERM`, `EACCES` all come as `AccessDenied`) are for now — taking
    /// them for an absent service turned rtkit off for the process after
    /// the first one —; a missing service and the bus's policy are
    /// absences; a gone thread is gone.
    #[test]
    fn refusals_are_told_from_absences() {
        let denied = "org.freedesktop.DBus.Error.AccessDenied";
        assert!(matches!(
            refusal(denied, "Device or resource busy"),
            Refusal::Refused(_)
        ));
        assert!(matches!(
            refusal(denied, "Operation not permitted"),
            Refusal::Refused(_)
        ));
        assert_eq!(
            refusal(denied, "Rejected send message, 1 matched rules"),
            Refusal::Absent
        );
        assert_eq!(
            refusal("org.freedesktop.DBus.Error.ServiceUnknown", "x"),
            Refusal::Absent
        );
        assert_eq!(
            refusal(
                "org.freedesktop.DBus.Error.Failed",
                "No such file or directory"
            ),
            Refusal::Gone
        );
    }

    /// Posting a request is a few atomics and a futex wake: no heap, no
    /// wait for the service — whatever the service does meanwhile.
    #[test]
    fn posting_a_request_never_waits_nor_allocates() {
        let table: &'static Table = Box::leak(Box::new(Table::new()));
        let started = Instant::now();
        crate::test_allocations::assert_no_heap(|| {
            assert!(table.post(4242, 10, false));
            assert!(table.post(4242, 12, true), "asked again: its own slot");
            assert!(table.post(4243, 10, false));
        });
        assert!(started.elapsed() < Duration::from_millis(5));
        let used: Vec<u64> = table
            .slots
            .iter()
            .map(|s| s.who.load(Ordering::Relaxed))
            .filter(|&w| w != 0)
            .collect();
        assert_eq!(used, vec![(4242u64 << 32) | 2, (4243u64 << 32) | 1]);
        assert_eq!(table.slots[0].what.load(Ordering::Relaxed), 12 | AUDIO);
        // A full table refuses (the caller counts a failure).
        for t in 0..MAX_REQUESTS as i32 {
            table.post(10_000 + t, 1, false);
        }
        assert!(!table.post(99_999, 1, false));
    }

    /// More requests than rtkit allows a user in its window (5 here, as
    /// its 25 + 1 in 20 s): the broker paces them within its own budget —
    /// none is refused, every one granted — at the priority rtkit caps.
    #[test]
    fn requests_beyond_rtkits_burst_are_paced_not_refused() {
        let model = Model {
            burst: 4,
            ..Model::quick()
        };
        let tids: Vec<i32> = (5001..5011).collect();
        let world = World::with_threads(tids.clone());
        let stand_in = StandIn::start("paced", model, world.clone());
        let config = Config {
            burst: 3,
            ..quick_config()
        };
        let broker = Running::start(&stand_in.address, world.clone(), config);
        let started = Instant::now();
        for &t in &tids {
            assert!(broker.table.post(t, 80, false));
        }
        assert!(broker.wait_for(10, || world.realtime().len() == tids.len()));
        let calls = stand_in.calls();
        assert!(calls.iter().all(|c| c.3), "none refused: {calls:?}");
        assert_eq!(calls.len(), tids.len());
        assert!(calls.iter().all(|c| c.2 == 20), "capped at rtkit's 20");
        // Never more than 3 in 600 ms.
        for (i, c) in calls.iter().enumerate() {
            let within = calls[i..]
                .iter()
                .take_while(|d| d.0.duration_since(c.0) < model.window)
                .count();
            assert!(within <= 3, "{within} in a window");
        }
        assert!(started.elapsed() >= Duration::from_millis(1100));
        let s = broker.counters.read();
        assert_eq!(
            (s.granted, s.refused, s.realtime, s.waiting),
            (10, 0, 10, 0)
        );
    }

    /// Other programs used the user's window up: rtkit refuses with
    /// `AccessDenied` — a refusal, not an absent service. The broker pauses
    /// (no request meanwhile) and asks again until every thread is granted.
    #[test]
    fn a_refused_request_is_asked_again_after_a_pause() {
        let model = Model {
            burst: 4,
            ..Model::quick()
        };
        let tids: Vec<i32> = (6001..6004).collect();
        let world = World::with_threads(tids.clone());
        let stand_in = StandIn::start("refused", model, world.clone());
        stand_in.spend_window(&model);
        let config = quick_config();
        let broker = Running::start(&stand_in.address, world.clone(), config);
        for &t in &tids {
            broker.table.post(t, 10, false);
        }
        assert!(broker.wait_for(10, || world.realtime().len() == tids.len()));
        let calls = stand_in.calls();
        assert!(!calls[0].3, "the first is refused: {calls:?}");
        for w in calls.windows(2) {
            if !w[0].3 {
                assert!(
                    w[1].0.duration_since(w[0].0) >= config.pause,
                    "asked again only after a pause"
                );
            }
        }
        let s = broker.counters.read();
        assert!(s.refused >= 1 && s.granted == 3, "{s:?}");
    }

    /// Room for three: requests posted together go highest priority
    /// first; a higher priority later takes the place of the lowest grant
    /// (back to normal), and that one gets its place again when a thread
    /// goes.
    #[test]
    fn higher_priorities_go_first_and_take_the_place_of_lower_ones() {
        let world = World::with_threads(7001..7008);
        let stand_in = StandIn::start("order", Model::quick(), world.clone());
        let config = Config {
            threads: 3,
            gather: Duration::from_millis(50),
            ..quick_config()
        };
        let broker = Running::start(&stand_in.address, world.clone(), config);
        // Together: two low, then three higher ones.
        for (t, p) in [(7001, 5), (7002, 5), (7003, 15), (7004, 16), (7005, 17)] {
            broker.table.post(t, p, false);
        }
        assert!(broker.wait_for(5, || world.realtime().len() == 3));
        std::thread::sleep(Duration::from_millis(100));
        let rt = world.realtime();
        assert_eq!(
            rt.keys().copied().collect::<HashSet<_>>(),
            HashSet::from([7003, 7004, 7005]),
            "the highest three; the low ones never asked"
        );
        assert!(stand_in.calls().iter().all(|c| c.1 >= 7003));
        // Later, higher still: the lowest grant goes back to normal.
        broker.table.post(7006, 19, false);
        assert!(broker.wait_for(5, || world.realtime().contains_key(&7006)));
        let rt = world.realtime();
        assert_eq!(rt.len(), 3);
        assert!(!rt.contains_key(&7003), "{rt:?}");
        assert_eq!(world.0.lock().unwrap().demoted, vec![7003]);
        // A thread goes: the displaced one has room again.
        world.0.lock().unwrap().alive.remove(&7005);
        world.0.lock().unwrap().realtime.remove(&7005);
        assert!(broker.wait_for(5, || world.realtime().contains_key(&7003)));
        assert_eq!(broker.counters.read().displaced, 1);
    }

    /// A grant lost (the time limit's `SIGXCPU` demotion, rtkit's watchdog)
    /// is not asked for again on its own — the thread ran too long — but
    /// when the thread asks again, it is.
    #[test]
    fn a_lost_grant_is_asked_for_again_when_the_thread_asks() {
        let world = World::with_threads([7501]);
        let stand_in = StandIn::start("lost", Model::quick(), world.clone());
        let broker = Running::start(&stand_in.address, world.clone(), quick_config());
        broker.table.post(7501, 10, false);
        assert!(broker.wait_for(5, || world.realtime().contains_key(&7501)));
        world.0.lock().unwrap().realtime.remove(&7501);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(stand_in.calls().len(), 1, "not asked again by itself");
        assert_eq!(broker.counters.read().realtime, 0, "the loss is seen");
        broker.table.post(7501, 10, false);
        assert!(broker.wait_for(5, || world.realtime().contains_key(&7501)));
        assert_eq!(stand_in.calls().len(), 2);
    }

    /// A thread gone before its request is served is not asked for, and
    /// its slot is free again.
    #[test]
    fn a_gone_thread_is_not_asked_for() {
        let world = World::with_threads([8001]);
        let stand_in = StandIn::start("gone", Model::quick(), world.clone());
        let broker = Running::start(&stand_in.address, world.clone(), quick_config());
        broker.table.post(8002, 10, false);
        broker.table.post(8001, 10, false);
        assert!(broker.wait_for(5, || world.realtime().contains_key(&8001)));
        assert!(broker.wait_for(5, || {
            broker
                .table
                .slots
                .iter()
                .all(|s| s.who.load(Ordering::Relaxed) >> 32 != 8002)
        }));
        assert!(stand_in.calls().iter().all(|c| c.1 == 8001));
    }

    /// On the kernel (needs realtime permission, which the stand-in uses
    /// as rtkit uses its own): a thread posts its request and carries on —
    /// the post does not wait for the (slow) service — and the broker,
    /// another thread, makes it realtime.
    #[test]
    fn another_thread_makes_the_asking_thread_realtime() {
        // SAFETY: getrlimit fills the struct it is given.
        let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
        unsafe { libc::getrlimit(libc::RLIMIT_RTPRIO, &mut lim) };
        if lim.rlim_cur < 20 {
            eprintln!("skipped: no realtime permission");
            return;
        }
        let model = Model {
            delay: Duration::from_millis(200),
            kernel: true,
            ..Model::quick()
        };
        let stand_in = StandIn::start("kernel", model, World::default());
        let broker = Running::start(&stand_in.address, Kernel, quick_config());
        let table = broker.table;
        let (posted, became) = std::thread::spawn(move || {
            let t = Instant::now();
            assert!(table.post(gettid(), 12, false));
            let posted = t.elapsed();
            let until = Instant::now() + Duration::from_secs(5);
            let mut became = None;
            while Instant::now() < until {
                let (packed, _) = crate::pool::sched::current();
                if crate::pool::sched::is_realtime(packed) {
                    became = Some(((packed - 1) as u32 as i32, t.elapsed()));
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            (posted, became)
        })
        .join()
        .unwrap();
        assert!(posted < Duration::from_millis(5), "posting took {posted:?}");
        let (priority, after) = became.expect("made realtime");
        assert_eq!(priority, 12);
        assert!(
            after >= model.delay,
            "{after:?}: by the broker, after the service"
        );
    }

    /// Measurement: a session on a 16-thread, 8-core machine under
    /// PipeWire (the audio thread at rtkit's 20) against rtkit's default
    /// limits — 25 + 1 requests a window, 25 threads, 4 of them held by
    /// PipeWire and the desktop — with time shrunk 33 times (a 20 s window
    /// is 600 ms): 15 DSP workers (20), the shallow tier, its 4 workers and
    /// 2 reservoirs (19), the deep tier, its 7 workers and MIDI in and out
    /// (15), the disk and recording writers (10); half a window later
    /// Restart Audio (new workers and tiers). 0.22.0: every thread asks
    /// for itself, and the first refusal (`AccessDenied`) counted as no
    /// service, failing every later request. Now: the broker; workers past
    /// the cores and the writers do not ask. Run with `--ignored
    /// --nocapture`.
    #[test]
    #[ignore]
    fn a_sessions_requests_against_rtkits_limits() {
        let model = Model {
            others: 4,
            ..Model::quick()
        };
        // (count, priority, what, of them asking now) in the order the
        // threads start; the last five come again with Restart Audio.
        let demand: [(usize, i32, &str, usize); 8] = [
            (2, 10, "writers", 0),
            (2, 15, "MIDI", 2),
            (15, 20, "DSP workers", 7),
            (1, 19, "shallow tier", 1),
            (4, 19, "shallow workers", 4),
            (2, 19, "reservoirs", 2),
            (1, 15, "deep tier", 1),
            (7, 15, "deep workers", 7),
        ];
        let restarted = [
            "DSP workers",
            "shallow tier",
            "shallow workers",
            "deep tier",
            "deep workers",
        ];
        // (tid, priority, what, asks now, phase)
        let mut threads = Vec::new();
        for phase in 0..2 {
            for &(n, priority, what, asking) in &demand {
                if phase == 1 && !restarted.contains(&what) {
                    continue;
                }
                for k in 0..n {
                    threads.push((
                        9000 + threads.len() as i32,
                        priority,
                        what,
                        k < asking,
                        phase,
                    ));
                }
            }
        }
        let report = |name: &str, phase: usize, world: &World, asked: usize, refused: usize| {
            let granted = world.realtime();
            eprintln!("{name}: {asked} asked, {refused} refused");
            for &(_, priority, what, _) in &demand {
                let all: Vec<i32> = threads
                    .iter()
                    .filter(|t| {
                        t.2 == what && (t.4 == phase || (phase == 1 && !restarted.contains(&what)))
                    })
                    .map(|t| t.0)
                    .collect();
                let rt = all.iter().filter(|t| granted.contains_key(t)).count();
                eprintln!("  {what:<16} ({priority}): {rt} of {} realtime", all.len());
            }
        };
        let end_phase_one = |world: &World| {
            let mut w = world.0.lock().unwrap();
            for t in threads
                .iter()
                .filter(|t| t.4 == 0 && restarted.contains(&t.2))
            {
                w.alive.remove(&t.0);
                w.realtime.remove(&t.0);
            }
        };
        // 0.22.0.
        let world = World::with_threads(threads.iter().map(|t| t.0));
        let stand_in = StandIn::start("old", model, world.clone());
        let mut service = DbusService::at(stand_in.address.clone());
        service.limits().unwrap();
        let mut absent = false;
        for phase in 0..2 {
            if phase == 1 {
                end_phase_one(&world);
                std::thread::sleep(model.window / 2);
            }
            for &(tid, priority, _, _, _) in threads.iter().filter(|t| t.4 == phase) {
                if absent {
                    continue;
                }
                if let Err(Refusal::Refused(_) | Refusal::Absent) =
                    service.make_realtime(tid, priority.min(model.max_priority))
                {
                    absent = true;
                }
            }
            let calls = stand_in.calls();
            report(
                &format!("0.22.0, {}", ["start", "after Restart Audio"][phase]),
                phase,
                &world,
                calls.len(),
                calls.iter().filter(|c| !c.3).count(),
            );
        }
        // The broker.
        let world = World::with_threads(threads.iter().map(|t| t.0));
        let stand_in = StandIn::start("new", model, world.clone());
        let scale = |d: Duration| d / 33;
        let config = Config {
            window: scale(Config::DEFAULT.window),
            pause: scale(Config::DEFAULT.pause),
            pause_max: scale(Config::DEFAULT.pause_max),
            sweep: scale(Config::DEFAULT.sweep),
            failure_after: scale(Config::DEFAULT.failure_after),
            ..Config::DEFAULT
        };
        let broker = Running::start(&stand_in.address, world.clone(), config);
        let started = Instant::now();
        let settle = || {
            // Until nothing changes for five windows.
            let mut last = (stand_in.calls().len(), Instant::now());
            while last.1.elapsed() < config.window * 5 {
                let n = stand_in.calls().len();
                if n != last.0 {
                    last = (n, Instant::now());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        for phase in 0..2 {
            if phase == 1 {
                std::thread::sleep(model.window / 2);
                end_phase_one(&world);
            }
            for &(tid, priority, _, asks, _) in threads.iter().filter(|t| t.4 == phase) {
                if asks {
                    broker.table.post(tid, priority, false);
                }
            }
            if phase == 1 {
                settle();
            } else {
                std::thread::sleep(model.window / 2);
            }
            let calls = stand_in.calls();
            let last = calls.last().map_or(Duration::ZERO, |c| c.0 - started);
            report(
                &format!(
                    "broker, {} (last request {:.1} windows in)",
                    ["start", "after Restart Audio"][phase],
                    last.as_secs_f64() / config.window.as_secs_f64()
                ),
                phase,
                &world,
                calls.len(),
                calls.iter().filter(|c| !c.3).count(),
            );
        }
        eprintln!("  {:?}", broker.counters.read());
    }

    #[test]
    fn bus_addresses_are_unescaped() {
        assert_eq!(unescape("/run/user/1000/bus"), "/run/user/1000/bus");
        assert_eq!(unescape("/tmp/a%2cb"), "/tmp/a,b");
    }

    /// Against the buses of the machine the tests run on: the properties
    /// read where a service answers (as `busctl get-property` shows them),
    /// and a missing service is told apart; then a thread asks through the
    /// process's broker. Opt-in: needs a desktop session.
    #[test]
    #[ignore]
    fn the_services_answer_on_this_machine() {
        for via in [Via::RtKit, Via::Portal] {
            match via.bus() {
                Ok(mut bus) => match read_limits(&mut bus, via) {
                    Ok(l) => eprintln!("{via:?}: {l:?}"),
                    Err(Failed::Error(name, text)) => {
                        eprintln!("{via:?}: {name}: {text} ({:?})", refusal(&name, &text))
                    }
                    Err(Failed::Io(e)) => panic!("{via:?}: {e}"),
                },
                Err(e) => eprintln!("{via:?}: no bus ({e})"),
            }
        }
        start();
        let ok = std::thread::spawn(|| {
            request_for_this_thread(10, false);
            let until = Instant::now() + Duration::from_secs(3);
            while Instant::now() < until {
                if crate::pool::sched::is_realtime(crate::pool::sched::current().0) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            false
        })
        .join()
        .unwrap();
        eprintln!("a thread made realtime: {ok}; {:?}", stats());
    }
}
