//! What the sandbox needs from the operating system, behind one interface:
//! named shared memory ([`Shm`]), the per-block wake-ups ([`Signal`] /
//! [`Waiter`]), the control stream ([`Control`]), and starting a helper
//! ([`spawn`]) or joining FaderFrame from one ([`connect`]).
//!
//! * Unix: a socket pair for control and a pipe each way for the
//!   wake-ups, at fixed descriptor numbers in the helper; POSIX shared
//!   memory.
//! * Windows: a named pipe (overlapped, so every wait has a deadline and
//!   watches the other process), auto-reset events for the wake-ups, a
//!   pagefile-backed file mapping.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

use std::process::Child;
use std::sync::atomic::{AtomicU32, Ordering};

/// Set in a helper's environment (on Windows it also names the helper's
/// connections).
pub const ENV_HELPER: &str = "FADERFRAME_PLUGIN_SANDBOX";

/// What waiting for a wake-up ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ready {
    Woken,
    /// The other side is gone.
    HungUp,
    TimedOut,
}

/// FaderFrame's side of a started helper.
pub struct Link {
    pub child: Child,
    pub control: Control,
    /// Wakes the helper's audio thread.
    pub go: Signal,
    /// The helper's audio thread finished a block.
    pub done: Waiter,
}

/// A helper's side of the connection.
pub struct Connected {
    pub control: Control,
    pub go: Waiter,
    pub done: Signal,
}

static COUNT: AtomicU32 = AtomicU32::new(0);

/// A name for this process's next shared object (at most 30 characters:
/// macOS limits POSIX shared memory names to 31).
pub(crate) fn unique_name() -> String {
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("ffsb.{}.{n}.{:x}", std::process::id(), nanos & 0xFFFF)
}
