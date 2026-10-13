//! Counting the C allocator, for the plugin check
//! (`faderframe-plugin-check`): plugins written in C or C++ allocate
//! through `malloc` and its kin, not through Rust's allocator. The program
//! defines those functions itself (Linux with glibc), so every call in the
//! process — plugins loaded later included — comes here first, and passes
//! straight on to glibc's own (`__libc_malloc` …): one thread-local check
//! per call. A thread being checked arms its counter; no other thread ever
//! counts anything.

use std::cell::Cell;
use std::ffi::c_void;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

#[inline(always)]
fn note() {
    // Constant thread-locals without destructors: plain TLS reads, valid
    // from the process's first allocation to its thread's last.
    if ARMED.with(Cell::get) {
        COUNT.with(|c| c.set(c.get() + 1));
    }
}

/// The counter the plugin check uses.
pub fn counter() -> faderframe_plugin_check::AllocCounter {
    faderframe_plugin_check::AllocCounter {
        arm: |on| ARMED.with(|a| a.set(on)),
        count: || COUNT.with(Cell::get),
    }
}

unsafe extern "C" {
    fn __libc_malloc(size: usize) -> *mut c_void;
    fn __libc_calloc(n: usize, size: usize) -> *mut c_void;
    fn __libc_realloc(p: *mut c_void, size: usize) -> *mut c_void;
    fn __libc_free(p: *mut c_void);
    fn __libc_memalign(align: usize, size: usize) -> *mut c_void;
    fn __libc_valloc(size: usize) -> *mut c_void;
    fn __libc_pvalloc(size: usize) -> *mut c_void;
}

/// # Safety
/// The C function's contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    note();
    // SAFETY: glibc's malloc, as the caller asked.
    unsafe { __libc_malloc(size) }
}

/// # Safety
/// The C function's contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calloc(n: usize, size: usize) -> *mut c_void {
    note();
    // SAFETY: glibc's calloc, as the caller asked.
    unsafe { __libc_calloc(n, size) }
}

/// # Safety
/// The C function's contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(p: *mut c_void, size: usize) -> *mut c_void {
    note();
    // SAFETY: glibc's realloc, as the caller asked.
    unsafe { __libc_realloc(p, size) }
}

/// # Safety
/// The C function's contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(p: *mut c_void) {
    if !p.is_null() {
        note();
    }
    // SAFETY: glibc's free, as the caller asked.
    unsafe { __libc_free(p) }
}

/// # Safety
/// The C function's contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memalign(align: usize, size: usize) -> *mut c_void {
    note();
    // SAFETY: glibc's memalign, as the caller asked.
    unsafe { __libc_memalign(align, size) }
}

/// # Safety
/// The C function's contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aligned_alloc(align: usize, size: usize) -> *mut c_void {
    note();
    // SAFETY: glibc's aligned_alloc is its memalign.
    unsafe { __libc_memalign(align, size) }
}

/// # Safety
/// The C function's contract (`out` writable).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> i32 {
    note();
    if !align.is_power_of_two() || !align.is_multiple_of(std::mem::size_of::<*mut c_void>()) {
        return libc::EINVAL;
    }
    // SAFETY: glibc's memalign with a valid alignment.
    let p = unsafe { __libc_memalign(align, size) };
    if p.is_null() && size != 0 {
        return libc::ENOMEM;
    }
    // SAFETY: the caller's pointer to write the result to.
    unsafe { *out = p };
    0
}

/// # Safety
/// The C function's contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn valloc(size: usize) -> *mut c_void {
    note();
    // SAFETY: glibc's valloc, as the caller asked.
    unsafe { __libc_valloc(size) }
}

/// # Safety
/// The C function's contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvalloc(size: usize) -> *mut c_void {
    note();
    // SAFETY: glibc's pvalloc, as the caller asked.
    unsafe { __libc_pvalloc(size) }
}
