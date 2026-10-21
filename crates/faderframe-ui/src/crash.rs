//! Crash reports: what a crash leaves for the user to send, and the offer
//! to show it at the next start.
//!
//! In `<data>/Crash Reports` (the per-user data folder, or the portable
//! folder's):
//!
//! * `crash-<start>-<pid>.log` — written when something goes wrong: a
//!   panic on any thread (the panic hook: message, place, backtrace, the
//!   end of the log), a fatal signal (Linux, macOS: `SIGSEGV`, `SIGBUS`,
//!   `SIGILL`, `SIGFPE`, `SIGABRT` — written async-signal-safely: the
//!   signal, the address, the thread and, where the stack has room, the
//!   native backtrace as module + offset for `addr2line`), or an exception
//!   nothing handled (Windows: code, address, thread, backtrace). Sandbox
//!   and check helpers write theirs too (`…-plugin-sandbox-<pid>.log`).
//! * `session-<start>-<pid>.log` — this run's log (also where Windows'
//!   GUI build, which has no console, keeps it).
//! * `running-<pid>` — while the program runs. Left behind, it says that
//!   run ended without closing: at the next start the window offers its
//!   report, or says that there is none (the system killed it — out of
//!   memory, a realtime time limit's `SIGKILL` — or the machine stopped)
//!   and points to its log.
//!
//! Old files are pruned (the newest few kept).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Which program this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// FaderFrame itself: reports, its log, the running marker.
    App,
    /// A helper process (`plugin-sandbox`, `check-plugin`): reports only.
    Helper(&'static str),
}

/// What a run that did not close left behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviousRun {
    /// When it started (UTC, `YYYYMMDD-HHMMSS`).
    pub started: String,
    /// Its crash report, if it wrote one.
    pub report: Option<PathBuf>,
    /// Its log, if it is there.
    pub log: Option<PathBuf>,
}

struct Paths {
    dir: PathBuf,
    report: PathBuf,
    marker: Option<PathBuf>,
    log: Option<PathBuf>,
    /// Version, system, process: the first lines of a report.
    header: String,
}

static PATHS: OnceLock<Paths> = OnceLock::new();
static PREVIOUS: OnceLock<Option<PreviousRun>> = OnceLock::new();

/// Reports and logs kept.
const KEEP_REPORTS: usize = 20;
const KEEP_LOGS: usize = 8;

/// Where crash reports go.
pub fn reports_dir() -> PathBuf {
    faderframe_session::media::data_dir().join("Crash Reports")
}

/// Install the crash reporter (first thing in `main`, before other
/// threads start). The log file this run writes to, for the app.
pub fn install(role: Role) -> Option<std::fs::File> {
    install_in(&reports_dir(), role)
}

/// [`install`] with reports in `dir`.
pub fn install_in(dir: &Path, role: Role) -> Option<std::fs::File> {
    let _ = std::fs::create_dir_all(dir);
    let pid = std::process::id();
    let started = utc_stamp(now_secs());
    let (report, marker, log) = match role {
        Role::App => (
            dir.join(format!("crash-{started}-{pid}.log")),
            Some(dir.join(format!("running-{pid}"))),
            Some(dir.join(format!("session-{started}-{pid}.log"))),
        ),
        Role::Helper(name) => (
            dir.join(format!("crash-{started}-{name}-{pid}.log")),
            None,
            None,
        ),
    };
    if role == Role::App {
        let _ = PREVIOUS.set(previous_runs(dir).into_iter().next_back());
        prune(dir, "crash-", KEEP_REPORTS);
        prune(dir, "session-", KEEP_LOGS);
    }
    let header = header(role, pid, &started, log.as_deref());
    let file = log.as_ref().and_then(|l| std::fs::File::create(l).ok());
    if let Some(m) = &marker {
        let mut text = format!("started={started}\nreport={}\n", report.display());
        if let Some(l) = &log {
            text.push_str(&format!("log={}\n", l.display()));
        }
        let _ = std::fs::write(m, text);
    }
    let paths = Paths {
        dir: dir.to_path_buf(),
        report,
        marker,
        log,
        header,
    };
    if PATHS.set(paths).is_ok() {
        install_panic_hook();
        fatal::install();
    }
    file
}

/// The program is closing normally: no report is offered next time.
pub fn finished() {
    if let Some(m) = PATHS.get().and_then(|p| p.marker.as_ref()) {
        let _ = std::fs::remove_file(m);
    }
}

/// The last run that ended without closing, if any (found at
/// [`install`]).
pub fn previous_run() -> Option<PreviousRun> {
    PREVIOUS.get().cloned().flatten()
}

/// This run's report file (it exists once something went wrong).
pub fn report_path() -> Option<PathBuf> {
    PATHS.get().map(|p| p.report.clone())
}

/// Runs whose marker is left behind and whose process is gone (oldest
/// first); their markers are removed.
fn previous_runs(dir: &Path) -> Vec<PreviousRun> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut runs = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_prefix("running-")
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() || alive(pid) {
            continue;
        }
        let text = std::fs::read_to_string(e.path()).unwrap_or_default();
        let field = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
                .map(str::to_string)
        };
        let existing = |key: &str| field(key).map(PathBuf::from).filter(|p| p.is_file());
        runs.push(PreviousRun {
            started: field("started").unwrap_or_default(),
            report: existing("report"),
            log: existing("log"),
        });
        let _ = std::fs::remove_file(e.path());
    }
    runs.sort_by(|a, b| a.started.cmp(&b.started));
    runs
}

/// Keep the newest `keep` files named `<prefix>…` (names sort by time).
fn prune(dir: &Path, prefix: &str, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix))
        .collect();
    names.sort();
    let extra = names.len().saturating_sub(keep);
    for n in &names[..extra] {
        let _ = std::fs::remove_file(dir.join(n));
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks whether the process exists.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: a query handle, closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return false;
    }
    let mut code = 0u32;
    // SAFETY: a valid handle and out pointer.
    let ok = unsafe { GetExitCodeProcess(h, &mut code) } != 0;
    // SAFETY: opened above.
    unsafe { CloseHandle(h) };
    ok && code == STILL_ACTIVE as u32
}

#[cfg(not(any(unix, windows)))]
fn alive(_pid: u32) -> bool {
    false
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `YYYYMMDD-HHMMSS` (UTC) of `secs` since 1970 — plain arithmetic, so the
/// signal handler can use it too.
fn stamp_into(secs: u64, out: &mut [u8; 15]) {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let digits = |out: &mut [u8], v: i64| {
        let mut v = v;
        for b in out.iter_mut().rev() {
            *b = b'0' + (v % 10) as u8;
            v /= 10;
        }
    };
    digits(&mut out[0..4], year);
    digits(&mut out[4..6], month);
    digits(&mut out[6..8], day);
    out[8] = b'-';
    digits(&mut out[9..11], (rest / 3600) as i64);
    digits(&mut out[11..13], (rest / 60 % 60) as i64);
    digits(&mut out[13..15], (rest % 60) as i64);
}

fn utc_stamp(secs: u64) -> String {
    let mut b = [0u8; 15];
    stamp_into(secs, &mut b);
    String::from_utf8_lossy(&b).into_owned()
}

/// The first lines of every report of this process.
fn header(role: Role, pid: u32, started: &str, log: Option<&Path>) -> String {
    let what = match role {
        Role::App => "FaderFrame".to_string(),
        Role::Helper(name) => format!("FaderFrame {name} helper"),
    };
    let mut h = format!(
        "{what} {}\nsystem: {}\ngtk: {}.{}.{}\nprocess: {pid}, started {started} UTC\n",
        env!("CARGO_PKG_VERSION"),
        system(),
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version(),
    );
    if let Some(l) = log {
        h.push_str(&format!("log: {}\n", l.display()));
    }
    h
}

/// The operating system, as precisely as it says.
fn system() -> String {
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut s = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    #[cfg(target_os = "linux")]
    {
        if let Some(name) = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_string())
            })
        {
            s.push_str(&format!(", {name}"));
        }
        if let Ok(kernel) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
            s.push_str(&format!(", kernel {}", kernel.trim()));
        }
        #[cfg(target_env = "gnu")]
        {
            // SAFETY: returns a static NUL-terminated string.
            let v = unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) };
            s.push_str(&format!(", glibc {}", v.to_string_lossy()));
        }
    }
    s
}

/// The end of this run's log (`lines` lines).
fn log_tail(log: &Path, lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    // The last 64 KiB at most (a log of hours is long).
    const TAIL: u64 = 64 * 1024;
    let Ok(mut f) = std::fs::File::open(log) else {
        return String::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let mut bytes = Vec::new();
    if f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).is_err()
        || f.read_to_end(&mut bytes).is_err()
    {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

fn append(path: &Path, text: &str) -> bool {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .is_ok()
}

thread_local! {
    /// A report of this thread's is being written (a panic while writing
    /// one is left to Rust's default).
    static WRITING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        if WRITING.with(|w| w.replace(true)) {
            return;
        }
        if let Some(p) = PATHS.get() {
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "(no message)".into());
            let place = info
                .location()
                .map_or_else(String::new, |l| format!(" at {l}"));
            let thread = std::thread::current();
            let backtrace = std::backtrace::Backtrace::force_capture();
            let tail = p
                .log
                .as_deref()
                .map(|l| log_tail(l, 150))
                .unwrap_or_default();
            let text = format!(
                "{}\nPANIC (time {} UTC) on thread '{}'{place}:\n{message}\n\n\
                 (On a worker thread the program may have gone on without that \
                 thread's work; anywhere else it ended here.)\n\nbacktrace:\n{backtrace}\n\
                 \nthe end of the log:\n{tail}\n\n",
                p.header,
                utc_stamp(now_secs()),
                thread.name().unwrap_or("<unnamed>"),
            );
            if append(&p.report, &text) {
                let _ = writeln!(
                    std::io::stderr(),
                    "FaderFrame: crash report written to {}",
                    p.report.display()
                );
            }
        }
        WRITING.with(|w| w.set(false));
    }));
}

/// The fatal-signal handlers (Linux, macOS).
#[cfg(unix)]
mod fatal {
    use super::PATHS;
    use std::ffi::{CString, c_int, c_void};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    const SIGNALS: [c_int; 5] = [
        libc::SIGSEGV,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
        libc::SIGABRT,
    ];

    /// Everything the handler needs, made before it can run (it may not
    /// allocate).
    struct Ready {
        report: CString,
        header: Vec<u8>,
        previous: [libc::sigaction; SIGNALS.len()],
    }

    static READY: OnceLock<Ready> = OnceLock::new();
    static REPORTING: AtomicBool = AtomicBool::new(false);

    #[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
    unsafe extern "C" {
        fn backtrace(buffer: *mut *mut c_void, size: c_int) -> c_int;
        fn backtrace_symbols_fd(buffer: *const *mut c_void, size: c_int, fd: c_int);
    }

    /// Room a native backtrace wants on the stack it runs on.
    const BACKTRACE_STACK: usize = 64 * 1024;
    /// The main thread's signal stack (Rust's own is a few KiB: too small
    /// to walk the stack on).
    const MAIN_SIGNAL_STACK: usize = 256 * 1024;

    pub fn install() {
        let Some(p) = PATHS.get() else {
            return;
        };
        let Ok(report) = CString::new(p.report.as_os_str().as_encoded_bytes()) else {
            return;
        };
        #[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
        {
            // The first call loads the unwinder (allocating): now, not in
            // the handler.
            let mut frames = [std::ptr::null_mut::<c_void>(); 4];
            // SAFETY: room for 4 frames.
            unsafe { backtrace(frames.as_mut_ptr(), 4) };
        }
        main_signal_stack();
        // SAFETY: zeroed sigactions are valid (filled by the calls below).
        let mut previous: [libc::sigaction; SIGNALS.len()] = unsafe { std::mem::zeroed() };
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_fatal as extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void)
            as libc::sighandler_t;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        // Reading the previous actions first, so the handler finds them
        // whenever it runs.
        for (old, &sig) in previous.iter_mut().zip(SIGNALS.iter()) {
            // SAFETY: reads the current action into a valid struct.
            unsafe { libc::sigaction(sig, std::ptr::null(), old) };
        }
        if READY
            .set(Ready {
                report,
                header: p.header.clone().into_bytes(),
                previous,
            })
            .is_err()
        {
            return;
        }
        for &sig in &SIGNALS {
            // SAFETY: a valid action for a signal the handler handles.
            unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) };
        }
    }

    /// A signal stack for the main thread big enough to walk its stack on
    /// (its own stays: it is for the stack overflow handler of Rust, which
    /// runs on it after ours).
    fn main_signal_stack() {
        // SAFETY: a fresh private mapping, kept for the life of the
        // process (the main thread's signal stack).
        let stack = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                MAIN_SIGNAL_STACK,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if stack == libc::MAP_FAILED {
            return;
        }
        let ss = libc::stack_t {
            ss_sp: stack,
            ss_flags: 0,
            ss_size: MAIN_SIGNAL_STACK,
        };
        // SAFETY: the mapping above, as this thread's signal stack.
        unsafe { libc::sigaltstack(&ss, std::ptr::null_mut()) };
    }

    fn write(fd: c_int, bytes: &[u8]) {
        let mut at = 0;
        while at < bytes.len() {
            // SAFETY: a valid buffer of this length.
            let n = unsafe { libc::write(fd, bytes[at..].as_ptr().cast(), bytes.len() - at) };
            if n <= 0 {
                return;
            }
            at += n as usize;
        }
    }

    /// `v` in decimal, or in hexadecimal with `0x`.
    fn number(fd: c_int, v: u64, hex: bool) {
        let mut buf = [0u8; 24];
        let mut i = buf.len();
        let mut v = v;
        let base = if hex { 16 } else { 10 };
        loop {
            i -= 1;
            buf[i] = b"0123456789abcdef"[(v % base) as usize];
            v /= base;
            if v == 0 {
                break;
            }
        }
        if hex {
            write(fd, b"0x");
        }
        write(fd, &buf[i..]);
    }

    fn name_of(sig: c_int) -> &'static [u8] {
        match sig {
            libc::SIGSEGV => b"SIGSEGV (invalid memory access)",
            libc::SIGBUS => b"SIGBUS (bus error)",
            libc::SIGILL => b"SIGILL (illegal instruction)",
            libc::SIGFPE => b"SIGFPE (arithmetic error)",
            libc::SIGABRT => b"SIGABRT (aborted)",
            _ => b"signal",
        }
    }

    /// The calling thread's name and id.
    fn thread(fd: c_int) {
        let mut name = [0u8; 64];
        #[cfg(target_os = "linux")]
        {
            // SAFETY: PR_GET_NAME writes at most 16 bytes, NUL included.
            unsafe { libc::prctl(libc::PR_GET_NAME, name.as_mut_ptr()) };
        }
        #[cfg(target_os = "macos")]
        {
            // SAFETY: a buffer of this length for this thread's name.
            unsafe {
                libc::pthread_getname_np(libc::pthread_self(), name.as_mut_ptr().cast(), name.len())
            };
        }
        let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        write(fd, b"thread '");
        write(fd, &name[..len]);
        write(fd, b"'");
        #[cfg(target_os = "linux")]
        {
            write(fd, b" (tid ");
            // SAFETY: plain system call.
            number(fd, unsafe { libc::syscall(libc::SYS_gettid) } as u64, false);
            write(fd, b")");
        }
    }

    /// Whether the handler runs with room for a native backtrace: on the
    /// thread's own stack, or on a signal stack with room.
    fn room_for_backtrace() -> bool {
        // SAFETY: a query into a struct we own.
        let mut ss: libc::stack_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaltstack(std::ptr::null(), &mut ss) } != 0 {
            return false;
        }
        if ss.ss_flags & libc::SS_ONSTACK == 0 {
            return true;
        }
        let here = &ss as *const libc::stack_t as usize;
        here.saturating_sub(ss.ss_sp as usize) > BACKTRACE_STACK
    }

    /// How near the stack pointer a fault counts as a possible stack
    /// overflow (a frame's reach below it: its stores land above the new
    /// stack pointer, at most a frame's size).
    const OVERFLOW_REACH: usize = 1 << 20;

    /// Whether a fault at `address` may be a stack overflow: near the
    /// stack pointer it happened at (where that is unknown: may be).
    fn maybe_overflow(address: usize, context: *mut c_void) -> bool {
        // SAFETY: the context the kernel gave the handler.
        match unsafe { interrupted_sp(context) } {
            Some(sp) => address.abs_diff(sp) < OVERFLOW_REACH,
            None => true,
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe fn interrupted_sp(context: *mut c_void) -> Option<usize> {
        let uc = context.cast::<libc::ucontext_t>();
        // SAFETY: a SA_SIGINFO handler's third argument is the
        // interrupted context.
        (!uc.is_null()).then(|| unsafe { (*uc).uc_mcontext.gregs[libc::REG_RSP as usize] } as usize)
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    unsafe fn interrupted_sp(context: *mut c_void) -> Option<usize> {
        let uc = context.cast::<libc::ucontext_t>();
        // SAFETY: as above.
        (!uc.is_null()).then(|| unsafe { (*uc).uc_mcontext.sp } as usize)
    }

    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    unsafe fn interrupted_sp(_: *mut c_void) -> Option<usize> {
        None
    }

    /// Set once a fault was sent back to run on its thread's own stack.
    static ON_OWN_STACK: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_fatal(sig: c_int, info: *mut libc::siginfo_t, context: *mut c_void) {
        let Some(r) = READY.get() else {
            return;
        };
        let index = SIGNALS.iter().position(|&s| s == sig);
        // SAFETY: the kernel's siginfo for this signal.
        let (code, address) = unsafe { ((*info).si_code, fault_address(info) as usize) };
        if code > 0
            && !REPORTING.load(Ordering::SeqCst)
            && !room_for_backtrace()
            && !maybe_overflow(address, context)
            && !ON_OWN_STACK.swap(true, Ordering::SeqCst)
        {
            // A fault (not a stack overflow) on a thread whose signal stack
            // is too small to walk its stack on — Rust gives every thread
            // it starts one of a few KiB: once more on the thread's own
            // stack. This handler without SA_ONSTACK; the instruction runs
            // again and faults again.
            // SAFETY: a zeroed sigaction is valid (an empty mask), filled
            // in below; sigaction(2) is async-signal-safe.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = on_fatal
                as extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void)
                as libc::sighandler_t;
            action.sa_flags = libc::SA_SIGINFO;
            // SAFETY: a valid action for a signal this handler handles.
            unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) };
            return;
        }
        if !REPORTING.swap(true, Ordering::SeqCst) {
            // SAFETY: a NUL-terminated path made before; open(2) is
            // async-signal-safe.
            let fd = unsafe {
                libc::open(
                    r.report.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC,
                    0o644 as libc::c_uint,
                )
            };
            if fd >= 0 {
                write(fd, &r.header);
                write(fd, b"\nCRASH: ");
                write(fd, name_of(sig));
                if code > 0 {
                    write(fd, b" at address ");
                    number(fd, address as u64, true);
                } else {
                    write(fd, b" (sent by the program or another process)");
                }
                write(fd, b"\n");
                thread(fd);
                // SAFETY: time(2) with no out pointer.
                let now = unsafe { libc::time(std::ptr::null_mut()) };
                let mut stamp = [0u8; 15];
                super::stamp_into(now.max(0) as u64, &mut stamp);
                write(fd, b", time ");
                write(fd, &stamp);
                write(fd, b" UTC\n");
                backtrace_into(fd);
                write(fd, b"\n");
                // SAFETY: opened above.
                unsafe { libc::close(fd) };
            }
            write(2, b"FaderFrame crashed; crash report: ");
            write(2, r.report.as_bytes());
            write(2, b"\n");
        }
        if code > 0 {
            // A fault: as if this handler had not been there — the previous
            // action (Rust's stack overflow report, else the default) meets
            // the fault again as the instruction runs again.
            if let Some(i) = index {
                // SAFETY: restores the action saved at install.
                unsafe { libc::sigaction(sig, &r.previous[i], std::ptr::null_mut()) };
            }
        } else {
            // Sent (`abort`, `kill`): there is nothing to run again, and a
            // previous handler may just return (Rust's does for anything but
            // a stack overflow) — the default action, and the signal again.
            // SAFETY: the default for this signal, then the signal to this
            // thread (delivered once the handler returns).
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
        }
    }

    #[cfg(target_os = "linux")]
    unsafe fn fault_address(info: *mut libc::siginfo_t) -> *mut c_void {
        // SAFETY: the caller's siginfo.
        unsafe { (*info).si_addr() }
    }

    #[cfg(not(target_os = "linux"))]
    unsafe fn fault_address(info: *mut libc::siginfo_t) -> *mut c_void {
        // SAFETY: the caller's siginfo.
        unsafe { (*info).si_addr }
    }

    #[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
    fn backtrace_into(fd: c_int) {
        if !room_for_backtrace() {
            write(
                fd,
                b"(no backtrace: the signal stack is small - a stack overflow?)\n",
            );
            return;
        }
        write(fd, b"native backtrace (module(+offset) [address]):\n");
        let mut frames = [std::ptr::null_mut::<c_void>(); 64];
        // SAFETY: room for 64 frames; loaded at install (no allocation).
        let n = unsafe { backtrace(frames.as_mut_ptr(), frames.len() as c_int) };
        // SAFETY: the frames just taken; writes straight to `fd`.
        unsafe { backtrace_symbols_fd(frames.as_ptr(), n, fd) };
    }

    #[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
    fn backtrace_into(fd: c_int) {
        let _ = room_for_backtrace();
        write(fd, b"(no native backtrace on this system)\n");
    }
}

/// The unhandled-exception filter (Windows).
#[cfg(windows)]
mod fatal {
    use super::PATHS;
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        EXCEPTION_POINTERS, SetUnhandledExceptionFilter,
    };

    static REPORTING: AtomicBool = AtomicBool::new(false);
    /// `EXCEPTION_CONTINUE_SEARCH`: Windows Error Reporting goes on.
    const CONTINUE_SEARCH: i32 = 0;

    pub fn install() {
        // SAFETY: a filter with the signature Windows wants.
        unsafe { SetUnhandledExceptionFilter(Some(filter)) };
    }

    fn name_of(code: u32) -> &'static str {
        match code {
            0xC000_0005 => "access violation",
            0xC000_00FD => "stack overflow",
            0xC000_001D => "illegal instruction",
            0xC000_0094 => "integer division by zero",
            0xC000_0096 => "privileged instruction",
            0xC000_0006 => "in-page error",
            0x8000_0003 => "breakpoint",
            0xE06D_7363 => "C++ exception",
            _ => "exception",
        }
    }

    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        if REPORTING.swap(true, Ordering::SeqCst) {
            return CONTINUE_SEARCH;
        }
        let Some(p) = PATHS.get() else {
            return CONTINUE_SEARCH;
        };
        // SAFETY: Windows passes the exception's record.
        let (code, address) = unsafe {
            info.as_ref()
                .and_then(|i| i.ExceptionRecord.as_ref())
                .map_or((0, std::ptr::null_mut()), |r| {
                    (r.ExceptionCode as u32, r.ExceptionAddress)
                })
        };
        let thread = std::thread::current();
        let mut text = format!(
            "{}\nCRASH: {} (0x{code:08x}) at {address:p}\nthread '{}', time {} UTC\n",
            p.header,
            name_of(code),
            thread.name().unwrap_or("<unnamed>"),
            super::utc_stamp(super::now_secs()),
        );
        // A stack overflow leaves too little stack to walk it.
        if code != 0xC000_00FD {
            text.push_str(&format!(
                "backtrace:\n{}\n",
                std::backtrace::Backtrace::force_capture()
            ));
        }
        super::append(&p.report, &text);
        CONTINUE_SEARCH
    }
}

#[cfg(not(any(unix, windows)))]
mod fatal {
    pub fn install() {}
}

/// At start-up, in the window: the last run ended without closing — offer
/// its crash report (or say there is none) and where its files are.
pub fn offer_previous_run(window: &gtk::Window) {
    use gtk::{gio, prelude::*};
    let Some(run) = previous_run() else {
        return;
    };
    let dir = PATHS.get().map_or_else(reports_dir, |p| p.dir.clone());
    let detail = match (&run.report, &run.log) {
        (Some(report), _) => format!(
            "It wrote a crash report:\n{}\n\nPlease attach it when you report the problem \
             (the log of that run is beside it).",
            report.display()
        ),
        (None, Some(log)) => format!(
            "It left no crash report: the system ended it (for example when memory ran out, \
             or a time limit for realtime threads was reached), or the computer stopped. \
             The log of that run:\n{}",
            log.display()
        ),
        (None, None) => format!(
            "It left no crash report: the system ended it (for example when memory ran out), \
             or the computer stopped. Crash reports are kept in\n{}",
            dir.display()
        ),
    };
    tracing::warn!(
        "the run started {} UTC did not close: report {}, log {}",
        run.started,
        run.report
            .as_ref()
            .map_or_else(|| "none".into(), |p| p.display().to_string()),
        run.log
            .as_ref()
            .map_or_else(|| "none".into(), |p| p.display().to_string()),
    );
    let shown = run.report.clone().or_else(|| run.log.clone());
    let mut buttons = vec!["Close", "Open Folder"];
    if run.report.is_some() {
        buttons.push("Show Report");
    } else if run.log.is_some() {
        buttons.push("Show Log");
    }
    let dialog = gtk::AlertDialog::builder()
        .message(format!(
            "FaderFrame closed unexpectedly last time (started {} UTC)",
            run.started
        ))
        .detail(detail)
        .buttons(buttons)
        .cancel_button(0)
        .default_button(0)
        .modal(false)
        .build();
    let parent = window.downgrade();
    dialog.choose(Some(window), gio::Cancellable::NONE, move |res| {
        let open = match res {
            Ok(1) => Some(dir.clone()),
            Ok(2) => shown.clone(),
            _ => None,
        };
        let Some(path) = open else {
            return;
        };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
        launcher.launch(
            parent.upgrade().as_ref(),
            gio::Cancellable::NONE,
            move |r| {
                if let Err(e) = r {
                    tracing::warn!("could not open {}: {e}", path.display());
                }
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_dates() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        // 2026-10-20 12:34:56 UTC.
        assert_eq!(utc_stamp(1_792_499_696), "20261020-123456");
        // A leap day.
        assert_eq!(utc_stamp(951_782_400), "20000229-000000");
    }

    /// A run that did not close is found once (its marker goes), with its
    /// report when it wrote one; a closed run leaves nothing; the running
    /// process itself and other live ones are never taken for gone.
    #[test]
    fn a_run_that_did_not_close_is_found_with_its_report() {
        let dir = std::env::temp_dir().join(format!("ff-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A process that is gone: a child that ended.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let gone = child.id();
        child.wait().unwrap();
        let report = dir.join(format!("crash-20261019-204026-{gone}.log"));
        std::fs::write(&report, "CRASH").unwrap();
        std::fs::write(
            dir.join(format!("running-{gone}")),
            format!(
                "started=20261019-204026\nreport={}\nlog={}\n",
                report.display(),
                dir.join("session-missing.log").display()
            ),
        )
        .unwrap();
        // This process: alive.
        std::fs::write(
            dir.join(format!("running-{}", std::process::id())),
            "started=x\n",
        )
        .unwrap();
        let runs = previous_runs(&dir);
        assert_eq!(
            runs,
            vec![PreviousRun {
                started: "20261019-204026".into(),
                report: Some(report),
                log: None,
            }]
        );
        assert!(previous_runs(&dir).is_empty(), "found once");
        assert!(dir.join(format!("running-{}", std::process::id())).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    const CHILD: &str = "FADERFRAME_CRASH_TEST";

    /// Run `test` again in a child with the reporter installed in a fresh
    /// folder (the child does what `test` does under `CHILD`); the folder
    /// and how the child ended.
    #[cfg(unix)]
    fn in_child(test: &str, case: &str) -> (PathBuf, std::process::ExitStatus, u32) {
        let dir = std::env::temp_dir().join(format!("ff-crash-{case}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, &dir)
            .env("FADERFRAME_CRASH_CASE", case)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let status = child.wait().unwrap();
        (dir, status, pid)
    }

    #[cfg(unix)]
    fn report_in(dir: &Path) -> String {
        let report = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("crash-"))
            .map(|e| e.path())
            .expect("a crash report");
        std::fs::read_to_string(report).unwrap()
    }

    /// A native crash (an access to memory the process may not touch) is
    /// written down from the signal handler — what, where, which thread,
    /// the native stack — and the process still ends with its signal (as
    /// it would have: core dumps and the system's crash reporting go on).
    /// The next start finds that run.
    #[cfg(unix)]
    #[test]
    fn a_fatal_signal_leaves_a_report_and_the_run_is_found() {
        if let Some(dir) = std::env::var_os(CHILD) {
            let mut log = install_in(Path::new(&dir), Role::App).unwrap();
            writeln!(log, "the last thing logged").unwrap();
            // No core dump of this child for the system's crash collector.
            #[cfg(target_os = "linux")]
            // SAFETY: a flag of this process.
            unsafe {
                libc::prctl(libc::PR_SET_DUMPABLE, 0);
            }
            // SAFETY: a page the process may not touch, then a write to it
            // (the crash this test is about).
            unsafe {
                let page = libc::mmap(
                    std::ptr::null_mut(),
                    4096,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                );
                std::ptr::write_volatile(page.cast::<u8>(), 1);
            }
            unreachable!();
        }
        let (dir, status, pid) = in_child(
            "crash::tests::a_fatal_signal_leaves_a_report_and_the_run_is_found",
            "segv",
        );
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status:?}");
        let text = report_in(&dir);
        assert!(text.contains("CRASH: SIGSEGV"), "{text}");
        assert!(text.contains("at address 0x"), "{text}");
        assert!(text.contains(&format!("process: {pid}")), "{text}");
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        assert!(text.contains("native backtrace"), "{text}");
        let runs = previous_runs(&dir);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].report.is_some() && runs[0].log.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A crash on another thread than the one that installed the reporter
    /// — the audio engine's, a video job's, a library's — has its native
    /// backtrace too: Rust gives each thread it starts a signal stack of a
    /// few KiB, too small to walk a stack on, and the report of a video
    /// job's crash on Debian 13 (GStreamer's MXF demuxer) said only "no
    /// backtrace".
    #[cfg(all(
        target_os = "linux",
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn a_crash_on_another_thread_has_its_backtrace() {
        if let Some(dir) = std::env::var_os(CHILD) {
            install_in(Path::new(&dir), Role::App).unwrap();
            // SAFETY: a flag of this process (no core dump of the child).
            unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) };
            let _ = std::thread::Builder::new()
                .name("ff-test-worker".into())
                .spawn(|| {
                    // SAFETY: a page the process may not touch, then a
                    // write to it (the crash this test is about).
                    unsafe {
                        let page = libc::mmap(
                            std::ptr::null_mut(),
                            4096,
                            libc::PROT_NONE,
                            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                            -1,
                            0,
                        );
                        std::ptr::write_volatile(page.cast::<u8>(), 1);
                    }
                })
                .unwrap()
                .join();
            unreachable!();
        }
        let test = "crash::tests::a_crash_on_another_thread_has_its_backtrace";
        let (dir, status, _) = in_child(test, "worker");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status:?}");
        let text = report_in(&dir);
        assert!(text.contains("CRASH: SIGSEGV"), "{text}");
        assert!(text.contains("thread 'ff-test-worker'"), "{text}");
        assert!(text.contains("native backtrace"), "{text}");
        assert_eq!(text.matches("CRASH:").count(), 1, "one report: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A thread that runs out of stack still ends the way Rust ends it
    /// (its "has overflowed its stack", then `abort`), with a report.
    #[cfg(all(
        target_os = "linux",
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn a_stack_overflow_is_reported_and_ends_as_rusts() {
        fn deep(n: u64) -> u64 {
            let frame = std::hint::black_box([n; 512]);
            if std::hint::black_box(n) == 0 {
                0
            } else {
                deep(n - 1).wrapping_add(frame[7])
            }
        }
        if let Some(dir) = std::env::var_os(CHILD) {
            install_in(Path::new(&dir), Role::App).unwrap();
            // SAFETY: a flag of this process (no core dump of the child).
            unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) };
            let _ = std::thread::Builder::new()
                .name("ff-test-deep".into())
                .stack_size(256 * 1024)
                .spawn(|| deep(u64::MAX))
                .unwrap()
                .join();
            unreachable!();
        }
        let test = "crash::tests::a_stack_overflow_is_reported_and_ends_as_rusts";
        let (dir, status, _) = in_child(test, "overflow");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGABRT), "{status:?}");
        let text = report_in(&dir);
        assert!(text.contains("CRASH: SIGSEGV"), "{text}");
        assert!(text.contains("thread 'ff-test-deep'"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Signals sent rather than caused (`abort`, as a panic in an
    /// `extern "C"` callback ends; `kill`) are written down too, and still
    /// end the process with that signal — Rust's own `SIGSEGV` handler
    /// returns for anything but a stack overflow, so handing such a signal
    /// back to it would let the process run on.
    #[cfg(unix)]
    #[test]
    fn a_sent_fatal_signal_still_ends_the_process() {
        if let Some(dir) = std::env::var_os(CHILD) {
            install_in(Path::new(&dir), Role::App).unwrap();
            #[cfg(target_os = "linux")]
            // SAFETY: a flag of this process (no core dump of the child).
            unsafe {
                libc::prctl(libc::PR_SET_DUMPABLE, 0);
            }
            if std::env::var("FADERFRAME_CRASH_CASE").as_deref() == Ok("abort") {
                std::process::abort();
            }
            // SAFETY: the signal to this thread.
            unsafe { libc::raise(libc::SIGSEGV) };
            // Only when the signal let the process run on.
            std::process::exit(3);
        }
        use std::os::unix::process::ExitStatusExt;
        let test = "crash::tests::a_sent_fatal_signal_still_ends_the_process";
        let (dir, status, _) = in_child(test, "sent");
        assert_eq!(status.signal(), Some(libc::SIGSEGV), "{status:?}");
        let text = report_in(&dir);
        assert!(text.contains("CRASH: SIGSEGV"), "{text}");
        assert!(text.contains("sent by the program"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
        let (dir, status, _) = in_child(test, "abort");
        assert_eq!(status.signal(), Some(libc::SIGABRT), "{status:?}");
        assert!(report_in(&dir).contains("CRASH: SIGABRT"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A panic on a thread is written down by the hook (message, place,
    /// backtrace, the end of the log) even where the program goes on.
    #[cfg(unix)]
    #[test]
    fn a_panic_leaves_a_report() {
        if let Some(dir) = std::env::var_os(CHILD) {
            let mut log = install_in(Path::new(&dir), Role::App).unwrap();
            writeln!(log, "the last thing logged").unwrap();
            let r = std::thread::Builder::new()
                .name("ff-test-panics".into())
                .spawn(|| panic!("an index past the end"))
                .unwrap()
                .join();
            assert!(r.is_err());
            // Ending without `finished`: the run did not close.
            std::process::exit(0);
        }
        let (dir, status, _) = in_child("crash::tests::a_panic_leaves_a_report", "panic");
        assert!(status.success(), "{status:?}");
        let text = report_in(&dir);
        assert!(text.contains("PANIC"), "{text}");
        assert!(text.contains("thread 'ff-test-panics'"), "{text}");
        assert!(text.contains("an index past the end"), "{text}");
        assert!(text.contains("crash.rs"), "the place: {text}");
        assert!(text.contains("the last thing logged"), "{text}");
        assert_eq!(previous_runs(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_files_are_pruned_newest_kept() {
        let dir = std::env::temp_dir().join(format!("ff-crash-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for d in 10..30 {
            std::fs::write(dir.join(format!("session-202610{d}-000000-1.log")), "").unwrap();
        }
        prune(&dir, "session-", 3);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "session-20261027-000000-1.log",
                "session-20261028-000000-1.log",
                "session-20261029-000000-1.log"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
