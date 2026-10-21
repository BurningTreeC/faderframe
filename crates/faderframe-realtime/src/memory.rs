//! Memory the audio thread touches, mapped before it does.
//!
//! A page the kernel has not mapped yet faults on its first touch: the
//! kernel stops the thread, zeroes a page and maps it (around a
//! microsecond; much longer under memory pressure, and a page swapped out
//! is read back from disk or zram). Buffers are made on control threads
//! (the audio thread never allocates) but filled on the audio thread — a
//! delay line as its write head moves on, a new graph's buffers in its
//! first cycle — so without care the audio thread takes those faults.
//!
//! * [`Prefaulting`] — a global allocator that touches every page of an
//!   allocation as it is made (on the thread that makes it, never the
//!   audio thread).
//! * [`prefault`] — the same for memory that exists already (thread
//!   stacks, rings made elsewhere).
//! * [`lock_process_memory`] — keeps every page the process has touched
//!   resident (`mlockall` with `MCL_ONFAULT`): nothing the audio path
//!   needs is swapped out (decoded samples played once an hour included),
//!   while memory never touched stays unmapped. Only where the
//!   memory-lock limit is unlimited; elsewhere a later allocation could
//!   fail.
//! * [`prefault_code`] — code and constants are mapped from the files as
//!   they are first used, too: the first time the audio thread runs a
//!   path (recording starting, a device's rarely taken branch) it faults
//!   the page in. This maps the program's and the audio path's libraries'
//!   and plugins' code and data at once (locked with the rest).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

thread_local! {
    /// Counting this thread's Rust allocations (the plugin check: what the
    /// host's own code allocates while it hosts a plugin), and the count.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static COUNTED: Cell<u64> = const { Cell::new(0) };
}

/// Whether the program's allocator is [`Prefaulting`] (it has allocated).
static INSTALLED: AtomicBool = AtomicBool::new(false);

#[inline(always)]
fn note() {
    if COUNTING.with(Cell::get) {
        COUNTED.with(|c| c.set(c.get() + 1));
    }
}

/// Count (or stop counting) the calling thread's allocations through
/// [`Prefaulting`]: Rust code's, FaderFrame's own.
pub fn count_allocations(on: bool) {
    COUNTING.with(|c| c.set(on));
}

/// The calling thread's allocations counted so far (`None`: the program's
/// allocator is not [`Prefaulting`], nothing is counted).
pub fn allocations_counted() -> Option<u64> {
    INSTALLED
        .load(Ordering::Relaxed)
        .then(|| COUNTED.with(Cell::get))
}

/// Allocations from this size on are mapped as they are made: all of
/// them (small ones can sit on fresh heap pages too — a graph's many
/// small buffers); one byte written per page costs next to nothing beside
/// the allocation itself.
pub const PREFAULT_FROM: usize = 1;

const PAGE: usize = 4096;

/// Map `len` bytes at `p` by writing one byte per page (and the last).
///
/// # Safety
/// `p..p + len` must be writable memory the caller owns whose contents do
/// not matter yet (uninitialised or zeroed: zero is written).
unsafe fn touch_zero(p: *mut u8, len: usize) {
    if len == 0 {
        return;
    }
    // The first byte of every page the range covers.
    let start = p as usize;
    let mut at = start;
    let end = start + len;
    while at < end {
        // SAFETY: inside the caller's range.
        unsafe { std::ptr::write_volatile(at as *mut u8, 0) };
        at = (at / PAGE + 1) * PAGE;
    }
}

/// The system allocator, with large allocations mapped as they are made.
/// FaderFrame's programs install it (`#[global_allocator]`).
pub struct Prefaulting;

// SAFETY: forwards to the system allocator; only writes bytes the
// allocation's contract leaves free (fresh, uninitialised or zeroed).
unsafe impl GlobalAlloc for Prefaulting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        if !INSTALLED.load(Ordering::Relaxed) {
            INSTALLED.store(true, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() && layout.size() >= PREFAULT_FROM {
            // SAFETY: fresh, uninitialised memory of this size.
            unsafe { touch_zero(p, layout.size()) };
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: the caller's contract.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() && layout.size() >= PREFAULT_FROM {
            // SAFETY: zeroed memory of this size; zero is written.
            unsafe { touch_zero(p, layout.size()) };
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        // SAFETY: the caller's contract.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: the caller's contract.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() && new_size >= PREFAULT_FROM && new_size > layout.size() {
            // SAFETY: the grown part is uninitialised and ours.
            unsafe { touch_zero(p.add(layout.size()), new_size - layout.size()) };
        }
        p
    }
}

/// Map every page of `buf` now (a ring or buffer made where the
/// [`Prefaulting`] allocator did not, before the audio thread gets it).
pub fn prefault<T: Copy>(buf: &mut [T]) {
    let len = std::mem::size_of_val(buf);
    let p = buf.as_mut_ptr().cast::<u8>();
    let start = p as usize;
    let mut at = start;
    while at < start + len {
        // SAFETY: inside `buf`; the byte is written back as it was (`T:
        // Copy` has no invariants a byte rewrite could break).
        unsafe {
            let b = std::ptr::read_volatile(at as *const u8);
            std::ptr::write_volatile(at as *mut u8, b);
        }
        at = (at / PAGE + 1) * PAGE;
    }
}

/// Stack kept free below what [`prefault_stack`] maps (the guard page, the
/// system's stack guarantee for exception handling, the frames of the
/// mapping itself).
const STACK_MARGIN: usize = 64 * 1024;

/// Bytes of the calling thread's stack below the caller's frame, where the
/// system says (`None`: it does not).
#[inline(never)]
pub fn stack_left() -> Option<usize> {
    // A local's address: about where the stack pointer is.
    let marker = std::hint::black_box(0u8);
    let here = std::hint::black_box(&marker) as *const u8 as usize;
    let lowest = stack::lowest()?;
    Some(here.saturating_sub(lowest))
}

/// Map `bytes` of this thread's stack below the caller's frame now, so
/// that deep calls on a realtime thread do not fault the stack in. At most
/// the stack the thread has left (less a margin): audio callbacks run on
/// threads other programs made — CoreAudio's, ASIO drivers', PipeWire's —
/// whose stacks may be smaller than asked for, and going past one is a
/// crash.
#[inline(never)]
pub fn prefault_stack(bytes: usize) {
    let bytes = match stack_left() {
        Some(left) => bytes.min(left.saturating_sub(STACK_MARGIN)),
        None => bytes,
    };
    if bytes == 0 {
        return;
    }
    prefault_stack_unchecked(bytes);
}

#[inline(never)]
fn prefault_stack_unchecked(bytes: usize) {
    const CHUNK: usize = 16 * 1024;
    fn go(left: usize) {
        let mut chunk = [0u8; CHUNK];
        // Touch every page of this frame's chunk.
        let mut i = 0;
        while i < CHUNK {
            // SAFETY: inside the local array.
            unsafe { std::ptr::write_volatile(chunk.as_mut_ptr().add(i), 1) };
            i += PAGE;
        }
        std::hint::black_box(&chunk);
        if left > CHUNK {
            go(left - CHUNK);
        }
    }
    go(bytes);
}

/// The lowest usable address of the calling thread's stack (above its
/// guard).
mod stack {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn lowest() -> Option<usize> {
        // SAFETY: the attributes of the calling thread, read into a struct
        // we own and destroyed after.
        unsafe {
            let mut attr: libc::pthread_attr_t = std::mem::zeroed();
            if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
                return None;
            }
            let mut addr: *mut libc::c_void = std::ptr::null_mut();
            let mut size = 0usize;
            let mut guard = 0usize;
            let ok = libc::pthread_attr_getstack(&attr, &mut addr, &mut size) == 0;
            libc::pthread_attr_getguardsize(&attr, &mut guard);
            libc::pthread_attr_destroy(&mut attr);
            (ok && !addr.is_null()).then(|| addr as usize + guard)
        }
    }

    #[cfg(target_os = "macos")]
    pub fn lowest() -> Option<usize> {
        // SAFETY: plain queries about the calling thread.
        let (top, size) = unsafe {
            let me = libc::pthread_self();
            (
                libc::pthread_get_stackaddr_np(me) as usize,
                libc::pthread_get_stacksize_np(me),
            )
        };
        (top > size).then(|| top - size)
    }

    #[cfg(windows)]
    pub fn lowest() -> Option<usize> {
        let (mut low, mut high) = (0usize, 0usize);
        // SAFETY: two valid out pointers.
        unsafe {
            windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits(&mut low, &mut high)
        };
        (low != 0).then_some(low)
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        windows
    )))]
    pub fn lowest() -> Option<usize> {
        None
    }
}

/// What [`lock_process_memory`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryLock {
    /// Every page touched stays resident.
    Locked,
    /// The memory-lock limit is not unlimited (`ulimit -l`): not tried —
    /// with a limit, an allocation past it would fail.
    Limited(u64),
    /// Turned off (`FADERFRAME_MLOCK=0`) or not on this system.
    Off,
    /// The call failed (errno).
    Failed(i32),
}

static LOCKED: AtomicU8 = AtomicU8::new(0);

/// Keep every page the process touches resident from now on (Linux:
/// `mlockall(MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT)` — locked as they are
/// touched, so untouched stacks and mappings stay unmapped). Only with an
/// unlimited memory-lock limit (the `realtime`/`audio` groups' setting).
pub fn lock_process_memory() -> MemoryLock {
    if std::env::var("FADERFRAME_MLOCK").as_deref() == Ok("0") {
        return MemoryLock::Off;
    }
    let r = imp::lock();
    if r == MemoryLock::Locked {
        LOCKED.store(1, Ordering::Relaxed);
    }
    r
}

/// Whether [`lock_process_memory`] locked the process's memory.
pub fn process_memory_locked() -> bool {
    LOCKED.load(Ordering::Relaxed) == 1
}

/// Map the code and constant data the audio path may run (Linux): the
/// program's own (all of FaderFrame's Rust code is in it), the C and C++
/// runtimes', the audio libraries' (PipeWire and its SPA plugins, JACK,
/// ALSA) and every plugin binary loaded so far (`.clap`, `.vst3`, `.lv2`,
/// …). Private data of those files is copied now (a first write would
/// copy it). Mappings done before are skipped, so call it again after
/// loading plugins (control thread; it reads `/proc/self/maps`). Bytes
/// newly mapped.
pub fn prefault_code() -> usize {
    imp::prefault_code()
}

#[cfg(target_os = "linux")]
mod imp {
    use super::MemoryLock;
    use std::collections::HashSet;
    use std::sync::Mutex;

    /// Largest zero-filled data taken for a file's `.bss`.
    const BSS_MAX: usize = 16 << 20;

    /// Mappings populated already (start, end).
    static DONE: Mutex<Option<HashSet<(usize, usize)>>> = Mutex::new(None);

    /// Whether a mapped file is one the audio path runs code of.
    fn wanted(path: &str, exe: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        path == exe
            || [
                "libc.so",
                "libm.so",
                "libstdc++.so",
                "libgcc_s.so",
                "libpipewire-",
                "libjack.so",
                "libjackserver.so",
                "libasound.so",
            ]
            .iter()
            .any(|p| name.starts_with(p))
            || path.contains("/spa-0.2/")
            || [".clap", ".vst3/", ".lv2/", ".component/"]
                .iter()
                .any(|p| path.contains(p))
    }

    pub fn prefault_code() -> usize {
        let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
            return 0;
        };
        let exe = std::fs::read_link("/proc/self/exe")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(mut done) = DONE.lock() else {
            return 0;
        };
        let done = done.get_or_insert_with(HashSet::new);
        let mut total = 0;
        // The file of the mapping before (an anonymous one right after a
        // file's writable data is its zero-filled rest: `.bss`).
        let mut last: Option<(bool, usize)> = None;
        for line in maps.lines() {
            let mut parts = line.split_whitespace();
            let (Some(range), Some(perms)) = (parts.next(), parts.next()) else {
                continue;
            };
            let path = parts.nth(3).unwrap_or("");
            let Some((start, end)) = range.split_once('-').and_then(|(a, b)| {
                Some((
                    usize::from_str_radix(a, 16).ok()?,
                    usize::from_str_radix(b, 16).ok()?,
                ))
            }) else {
                continue;
            };
            let perms = perms.as_bytes();
            let readable = perms.first() == Some(&b'r');
            let writable = perms.get(1) == Some(&b'w');
            let private = perms.get(3) == Some(&b'p');
            let ours = if path.starts_with('/') {
                wanted(path, &exe)
            } else {
                // The zero-filled data right after a wanted file's (small:
                // not a large buffer that happens to lie next to it).
                path.is_empty()
                    && end - start <= BSS_MAX
                    && last.is_some_and(|(w, e)| w && e == start)
            };
            last = Some((ours && writable, end));
            if !ours || !readable || !done.insert((start, end)) {
                continue;
            }
            total += populate(start, end, writable && private);
        }
        total
    }

    /// Whether the kernel populates mappings on request
    /// (`MADV_POPULATE_READ`/`WRITE`, Linux 5.14): asked once, on a page
    /// of our own.
    fn kernel_populates() -> bool {
        use std::sync::atomic::{AtomicU8, Ordering};
        // 0: not asked yet, 1: yes, 2: no.
        static KNOWN: AtomicU8 = AtomicU8::new(0);
        match KNOWN.load(Ordering::Relaxed) {
            1 => return true,
            2 => return false,
            _ => {}
        }
        // SAFETY: a fresh private anonymous page, unmapped below.
        let page = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                super::PAGE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if page == libc::MAP_FAILED {
            return false;
        }
        // SAFETY: advice on the page just mapped.
        let yes = unsafe { libc::madvise(page, super::PAGE, libc::MADV_POPULATE_READ) } == 0;
        // SAFETY: mapped above with this length.
        unsafe { libc::munmap(page, super::PAGE) };
        KNOWN.store(if yes { 1 } else { 2 }, Ordering::Relaxed);
        yes
    }

    /// Map `start..end` (copying it now when `write`: private data). The
    /// range comes from a reading of `/proc/self/maps` that other threads
    /// may have made stale meanwhile (a library unloaded, memory freed and
    /// mapped again, a guard page put there): the kernel is asked, and
    /// refuses what is no longer mapped or no longer readable (`ENOMEM`,
    /// `EINVAL` — incompatible or inaccessible mappings), and nothing is
    /// ever touched directly — a read of a range gone `PROT_NONE` would be
    /// a `SIGSEGV`.
    pub(super) fn populate(start: usize, end: usize, write: bool) -> usize {
        let len = end - start;
        if !kernel_populates() {
            // A kernel before 5.14: the files' pages read in, at least
            // (a hint; mapped on first use, from memory).
            // SAFETY: advice only; the kernel checks the range.
            unsafe { libc::madvise(start as *mut libc::c_void, len, libc::MADV_WILLNEED) };
            return 0;
        }
        let advice = if write {
            libc::MADV_POPULATE_WRITE
        } else {
            libc::MADV_POPULATE_READ
        };
        // SAFETY: a range of this process's mappings (or what replaced them:
        // the kernel checks); populating changes no contents.
        if unsafe { libc::madvise(start as *mut libc::c_void, len, advice) } == 0 {
            len
        } else {
            0
        }
    }

    pub fn lock() -> MemoryLock {
        // SAFETY: getrlimit fills the struct it is given.
        let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
        // SAFETY: a valid pointer to an rlimit.
        if unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut lim) } != 0 {
            return MemoryLock::Failed(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
        }
        if lim.rlim_cur != libc::RLIM_INFINITY {
            return MemoryLock::Limited(lim.rlim_cur);
        }
        // SAFETY: plain call with valid flags.
        let r = unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE | libc::MCL_ONFAULT) };
        if r == 0 {
            MemoryLock::Locked
        } else {
            MemoryLock::Failed(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::MemoryLock;

    pub fn lock() -> MemoryLock {
        MemoryLock::Off
    }

    pub fn prefault_code() -> usize {
        0
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod stack_tests {
    use super::*;

    /// Audio callbacks run on threads other programs made (CoreAudio's,
    /// an ASIO driver's), whose stacks may be smaller than the stack mapped
    /// for deep calls: the mapping keeps within the stack left.
    #[test]
    fn prefaulting_a_small_stack_stays_within_it() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                // (Systems round small stacks up: Windows to its own
                // granularity.)
                let left = stack_left().unwrap();
                assert!(left > 32 * 1024, "{left}");
                // More than the thread has.
                prefault_stack(left + (256 << 10));
                prefault_stack(256 << 10);
            })
            .unwrap()
            .join()
            .unwrap();
        // A thread with room maps all of it.
        let left = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(|| {
                prefault_stack(256 * 1024);
                stack_left().unwrap()
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(left > 1 << 20, "{left}");
    }
}

// Every check reads Linux's per-thread fault counts.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    fn minor_faults() -> i64 {
        // SAFETY: getrusage fills the struct it is given.
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: a valid pointer to a rusage.
        unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut u) };
        u.ru_minflt
    }

    /// Memory from the prefaulting allocator is mapped when it arrives:
    /// writing all of it takes no faults (the system allocator's would).
    #[cfg(target_os = "linux")]
    #[test]
    fn prefaulted_memory_takes_no_faults_when_written() {
        let layout = Layout::from_size_align(8 << 20, 64).unwrap();
        for zeroed in [false, true] {
            // SAFETY: a valid layout; freed below.
            let p = unsafe {
                if zeroed {
                    Prefaulting.alloc_zeroed(layout)
                } else {
                    Prefaulting.alloc(layout)
                }
            };
            assert!(!p.is_null());
            let before = minor_faults();
            // SAFETY: writes inside the allocation.
            unsafe { std::ptr::write_bytes(p, 7, layout.size()) };
            let faults = minor_faults() - before;
            // SAFETY: allocated above with this layout.
            unsafe { Prefaulting.dealloc(p, layout) };
            assert!(faults < 4, "{faults} faults (zeroed: {zeroed})");
        }
        // Fresh memory written as it is faults page by page.
        let fresh = Fresh::new(8 << 20);
        let before = minor_faults();
        // SAFETY: writes inside the mapping.
        unsafe { std::ptr::write_bytes(fresh.0, 7, fresh.1) };
        let faults = minor_faults() - before;
        assert!(faults > 1000, "the comparison: {faults} faults");
    }

    /// A fresh anonymous mapping (no page mapped yet).
    #[cfg(target_os = "linux")]
    struct Fresh(*mut u8, usize);

    #[cfg(target_os = "linux")]
    impl Fresh {
        fn new(len: usize) -> Self {
            // SAFETY: an anonymous private mapping; unmapped on drop.
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(p, libc::MAP_FAILED);
            // Small pages (with transparent huge pages a fault maps 2 MB).
            // SAFETY: advice on the mapping just made.
            unsafe { libc::madvise(p, len, libc::MADV_NOHUGEPAGE) };
            Self(p.cast(), len)
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for Fresh {
        fn drop(&mut self) {
            // SAFETY: mapped in `new` with this length.
            unsafe { libc::munmap(self.0.cast(), self.1) };
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn prefault_maps_existing_memory_and_keeps_its_contents() {
        let fresh = Fresh::new(4 << 20);
        // SAFETY: the mapping, as f32s (zero-filled is 0.0).
        let buf = unsafe { std::slice::from_raw_parts_mut(fresh.0.cast::<f32>(), fresh.1 / 4) };
        buf[12345] = 0.5;
        prefault(buf);
        assert_eq!(buf[12345], 0.5);
        assert_eq!(buf[12346], 0.0);
        let before = minor_faults();
        buf.fill(0.25);
        let faults = minor_faults() - before;
        assert!(faults < 4, "{faults}");
        prefault_stack(256 * 1024);
    }

    /// `prefault_code` populates ranges read from `/proc/self/maps`, which
    /// other threads may have changed since: a range gone `PROT_NONE` (a
    /// guard page, an allocator's reservation where freed memory was) is
    /// refused by the kernel and must never be touched (that is a
    /// `SIGSEGV`).
    #[test]
    fn a_range_gone_inaccessible_is_not_touched() {
        let fresh = Fresh::new(1 << 20);
        // SAFETY: our mapping, made inaccessible.
        assert_eq!(
            unsafe { libc::mprotect(fresh.0.cast(), fresh.1, libc::PROT_NONE) },
            0
        );
        let start = fresh.0 as usize;
        assert_eq!(imp::populate(start, start + fresh.1, false), 0);
        assert_eq!(imp::populate(start, start + fresh.1, true), 0);
        // And a range no longer mapped at all.
        let gone = Fresh::new(1 << 16);
        let at = gone.0 as usize;
        drop(gone);
        assert_eq!(imp::populate(at, at + (1 << 16), false), 0);
    }
}
