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

/// Map `bytes` of this thread's stack below the caller's frame now, so
/// that deep calls on a realtime thread do not fault the stack in.
#[inline(never)]
pub fn prefault_stack(bytes: usize) {
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

    /// Map `start..end` (copying it now when `write`: private data).
    fn populate(start: usize, end: usize, write: bool) -> usize {
        let len = end - start;
        let advice = if write {
            libc::MADV_POPULATE_WRITE
        } else {
            libc::MADV_POPULATE_READ
        };
        // SAFETY: a range of this process's mappings; populating changes
        // no contents.
        if unsafe { libc::madvise(start as *mut libc::c_void, len, advice) } == 0 {
            return len;
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL) {
            return 0;
        }
        // A kernel before 5.14: touch each page (written back as it was
        // for private data).
        let mut at = start;
        while at < end {
            // SAFETY: inside a readable mapping; a writable private one
            // gets its own byte back.
            unsafe {
                let b = std::ptr::read_volatile(at as *const u8);
                if write {
                    std::ptr::write_volatile(at as *mut u8, b);
                }
            }
            at += super::PAGE;
        }
        len
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
}
