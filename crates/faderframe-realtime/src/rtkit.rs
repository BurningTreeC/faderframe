//! Realtime scheduling through RealtimeKit, for when the process may not
//! take it itself (no `RLIMIT_RTPRIO`: a user outside the `realtime` /
//! `audio` group): the `rtkit-daemon` on the system bus, or — inside a
//! Flatpak, where the system bus is out of reach — the desktop portal's
//! Realtime interface, which asks rtkit on the program's behalf. A small
//! D-Bus client of its own (the wire protocol for a few method calls), no
//! library.
//!
//! RealtimeKit wants the process's `RLIMIT_RTTIME` set: a thread running
//! realtime that long without sleeping is stopped. It is set to rtkit's
//! maximum (hard) and half of it (soft): at the soft limit the kernel sends
//! the thread `SIGXCPU`, whose handler here returns it to normal scheduling
//! (counted as a priority failure) before the hard limit's `SIGKILL`.
//!
//! Each request is a few round trips to the daemon (around a millisecond),
//! made where a thread takes its priority (its start, or when the audio
//! thread's changes); rtkit caps the priority (20 by default) and how often
//! a user may ask (25 times in 20 s by default), so a thread already where
//! it would be put does not ask. `FADERFRAME_RTKIT=0` turns it off.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// Where a request went.
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

/// Per service: 0 not tried, 1 usable, 2 not there (not asked again).
static STATE: [AtomicU8; 2] = [AtomicU8::new(0), AtomicU8::new(0)];
static LIMITS: Mutex<[Option<Limits>; 2]> = Mutex::new([None, None]);

fn index(via: Via) -> usize {
    match via {
        Via::RtKit => 0,
        Via::Portal => 1,
    }
}

/// The calling thread realtime (`SCHED_RR`, at `priority` or the service's
/// cap below it) through rtkit or the portal. Whether it is now.
pub fn make_thread_realtime(priority: i32) -> bool {
    if priority <= 0 || std::env::var("FADERFRAME_RTKIT").as_deref() == Ok("0") {
        return false;
    }
    for via in [Via::RtKit, Via::Portal] {
        let i = index(via);
        if STATE[i].load(Ordering::Relaxed) == 2 {
            continue;
        }
        match ask(via, priority) {
            Ok(()) => {
                STATE[i].store(1, Ordering::Relaxed);
                return true;
            }
            Err(Refusal::Absent) => STATE[i].store(2, Ordering::Relaxed),
            Err(Refusal::Refused) => {}
        }
    }
    false
}

/// What the services allow, `None` where there is none (for diagnostics).
pub fn limits(via: Via) -> Option<Limits> {
    let mut bus = via.bus().ok()?;
    read_limits(&mut bus, via).ok()
}

enum Refusal {
    /// No such service (or it allows nothing): not asked again.
    Absent,
    /// It said no this time (its rate limit, a priority too high).
    Refused,
}

fn ask(via: Via, priority: i32) -> Result<(), Refusal> {
    let bus = via.bus().map_err(|_| Refusal::Absent)?;
    let cached = LIMITS.lock().ok().and_then(|l| l[index(via)]);
    ask_on(bus, via, priority, cached, |l| {
        if let Ok(mut all) = LIMITS.lock() {
            all[index(via)] = Some(l);
        }
    })
}

/// [`ask`] on a bus, with the service's limits if known (`learnt` gets
/// them when read).
fn ask_on(
    mut bus: Bus,
    via: Via,
    priority: i32,
    known: Option<Limits>,
    learnt: impl FnOnce(Limits),
) -> Result<(), Refusal> {
    let limits = match known {
        Some(l) => l,
        None => {
            let l = read_limits(&mut bus, via).map_err(|_| Refusal::Absent)?;
            learnt(l);
            l
        }
    };
    if limits.max_priority <= 0 || limits.rttime_max_us <= 0 {
        return Err(Refusal::Absent);
    }
    let priority = priority.min(limits.max_priority);
    if already(priority) {
        return Ok(());
    }
    if !limit_rttime(limits.rttime_max_us) {
        return Err(Refusal::Absent);
    }
    // SAFETY: plain system call without arguments.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u64;
    let mut body = Enc::default();
    let (member, signature) = match via {
        Via::RtKit => {
            body.u64(tid);
            ("MakeThreadRealtime", "tu")
        }
        Via::Portal => {
            body.u64(u64::from(std::process::id()));
            body.u64(tid);
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
    match bus.call(&call, &body.buf) {
        Ok(_) => Ok(()),
        Err(Failed::Io(_)) => Err(Refusal::Absent),
        Err(Failed::Error(name, _)) if is_absence(&name) => Err(Refusal::Absent),
        Err(Failed::Error(..)) => Err(Refusal::Refused),
    }
}

/// Errors meaning the service is not there (as opposed to a refusal).
fn is_absence(name: &str) -> bool {
    matches!(
        name,
        "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.UnknownMethod"
            | "org.freedesktop.DBus.Error.UnknownInterface"
            | "org.freedesktop.DBus.Error.UnknownObject"
            | "org.freedesktop.DBus.Error.AccessDenied"
            | "org.freedesktop.DBus.Error.Spawn.ServiceNotFound"
    )
}

/// Whether the calling thread runs realtime at `priority` already.
fn already(priority: i32) -> bool {
    let mut policy = 0;
    // SAFETY: plain query on the calling thread with valid out pointers.
    let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    let r = unsafe { libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param) };
    let policy = policy & !libc::SCHED_RESET_ON_FORK;
    r == 0
        && matches!(policy, libc::SCHED_FIFO | libc::SCHED_RR)
        && param.sched_priority == priority
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
    // SAFETY: getrlimit fills the struct it is given.
    let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: a valid pointer to an rlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut lim) } != 0 {
        return false;
    }
    let max = max_us as libc::rlim_t;
    let hard = if lim.rlim_max == libc::RLIM_INFINITY {
        max
    } else {
        lim.rlim_max.min(max)
    };
    let soft = if lim.rlim_cur < hard {
        lim.rlim_cur
    } else {
        hard / 2
    };
    let new = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: a valid rlimit; lowering limits needs no privilege.
    if unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &new) } != 0 {
        return false;
    }
    SOFT.store(soft as u64, std::sync::atomic::Ordering::Relaxed);
    catch_sigxcpu();
    // Helpers that compute for long stretches breathe within it.
    crate::priority::note_rttime_limit();
    *done = true;
    true
}

/// The soft limit set (put back after each `SIGXCPU`: the kernel raises it
/// by a second, past the hard limit).
static SOFT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

extern "C" fn on_sigxcpu(_: libc::c_int) {
    // A thread ran realtime too long without sleeping: normal scheduling
    // for it (only system calls and atomics: signal-safe).
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: the calling thread (0), a valid param.
    unsafe { libc::sched_setscheduler(0, libc::SCHED_OTHER, &param) };
    let soft = SOFT.load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: getrlimit/setrlimit with valid pointers (thin system calls).
    unsafe {
        let mut lim: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_RTTIME, &mut lim) == 0 && soft < lim.rlim_max {
            lim.rlim_cur = soft as libc::rlim_t;
            libc::setrlimit(libc::RLIMIT_RTTIME, &lim);
        }
    }
    crate::priority::note_failure();
}

fn catch_sigxcpu() {
    // SAFETY: reads the current disposition into a valid struct.
    let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: as above (no new action).
    if unsafe { libc::sigaction(libc::SIGXCPU, std::ptr::null(), &mut old) } != 0
        || old.sa_sigaction != libc::SIG_DFL
    {
        // Someone else's handler: theirs.
        return;
    }
    // SAFETY: a zeroed sigaction with the handler and flags set.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_sigxcpu as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: a valid action for SIGXCPU.
    unsafe { libc::sigaction(libc::SIGXCPU, &action, std::ptr::null_mut()) };
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

    /// The whole request against a stand-in for rtkit on a bus of its own:
    /// authentication, `Hello`, the limits read, then `MakeThreadRealtime`
    /// with the calling thread and the priority capped by the service.
    #[test]
    fn a_thread_asks_rtkit_on_the_wire() {
        let dir = std::env::temp_dir().join(format!("ff-rtkit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("bus");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut bus = Bus { socket, serial: 0 };
            let mut nul = [0u8];
            bus.socket.read_exact(&mut nul).unwrap();
            assert_eq!(nul[0], 0);
            let auth = bus.line().unwrap();
            assert!(auth.starts_with("AUTH EXTERNAL "), "{auth}");
            bus.socket.write_all(b"OK 0123456789abcdef\r\n").unwrap();
            assert_eq!(bus.line().unwrap(), "BEGIN");
            let mut asked = None;
            while asked.is_none() {
                let m = bus.read().unwrap();
                let member = m.member.clone().unwrap_or_default();
                let reply = match member.as_str() {
                    "Hello" => {
                        let mut b = Enc::default();
                        b.string(":1.7");
                        answer(m.serial, "s", &b.buf)
                    }
                    "Get" => {
                        let mut d = Dec::new(&m.body, m.little);
                        let interface = d.string().unwrap();
                        assert_eq!(interface, "org.freedesktop.RealtimeKit1");
                        let mut b = Enc::default();
                        match d.string().unwrap().as_str() {
                            "MaxRealtimePriority" => {
                                b.signature("i");
                                b.u32(20);
                            }
                            "RTTimeUSecMax" => {
                                // Long: the test process keeps working.
                                b.signature("x");
                                b.u64(60_000_000);
                            }
                            other => panic!("{other}"),
                        }
                        answer(m.serial, "v", &b.buf)
                    }
                    "MakeThreadRealtime" => {
                        let mut d = Dec::new(&m.body, m.little);
                        asked = Some((d.u64().unwrap(), d.u32().unwrap()));
                        answer(m.serial, "", &[])
                    }
                    other => panic!("{other}"),
                };
                bus.socket.write_all(&reply).unwrap();
            }
            asked
        });
        let address = format!("unix:path={}", path.display());
        let (result, tid) = std::thread::spawn(move || {
            let bus = Bus::open(&address).unwrap();
            let mut seen = None;
            let r = ask_on(bus, Via::RtKit, 80, None, |l| seen = Some(l));
            assert_eq!(
                seen,
                Some(Limits {
                    max_priority: 20,
                    rttime_max_us: 60_000_000
                })
            );
            // SAFETY: plain system call.
            (r.is_ok(), unsafe { libc::syscall(libc::SYS_gettid) } as u64)
        })
        .join()
        .unwrap();
        assert!(result);
        assert_eq!(
            server.join().unwrap(),
            Some((tid, 20)),
            "the caller, capped"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bus_addresses_are_unescaped() {
        assert_eq!(unescape("/run/user/1000/bus"), "/run/user/1000/bus");
        assert_eq!(unescape("/tmp/a%2cb"), "/tmp/a,b");
    }

    /// Against the buses of the machine the tests run on: the properties
    /// read where a service answers (as `busctl get-property` shows them),
    /// and a missing service is told apart. Opt-in: needs a desktop session.
    #[test]
    #[ignore]
    fn the_services_answer_on_this_machine() {
        for via in [Via::RtKit, Via::Portal] {
            match via.bus() {
                Ok(mut bus) => match read_limits(&mut bus, via) {
                    Ok(l) => eprintln!("{via:?}: {l:?}"),
                    Err(Failed::Error(name, text)) => {
                        eprintln!("{via:?}: {name}: {text} (absent: {})", is_absence(&name))
                    }
                    Err(Failed::Io(e)) => panic!("{via:?}: {e}"),
                },
                Err(e) => eprintln!("{via:?}: no bus ({e})"),
            }
        }
        let ok = std::thread::spawn(|| make_thread_realtime(10))
            .join()
            .unwrap();
        eprintln!("a thread made realtime: {ok}");
    }
}
