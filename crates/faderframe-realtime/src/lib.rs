//! Realtime-safe building blocks shared by the engine and its control side.
//!
//! Everything in this crate is usable from the audio thread: no allocation,
//! no locks, no syscalls beyond reading the monotonic clock. Allocation
//! happens only in constructors, which must run on a control thread.
//!
//! * [`AtomicF32`] — lock-free `f32` cell.
//! * [`ParamTable`] — control → RT continuous parameter values.
//! * [`MeterBank`] — RT → control peak/RMS meter values.
//! * [`ScopeRing`] — RT → control stereo audio of one source (analysers).
//! * [`CallbackMetrics`] — callback duration histogram, deadline misses,
//!   xruns; percentiles are computed on the control side.
//! * [`SlotAllocator`] — control-side allocator for table slots.
//! * [`mailbox`] — latest-value handoff of boxed objects (graphs, snapshots).
//!
//! * [`PageTable`] / [`Epoch`] — lock-free page table with epoch-based
//!   reclamation for disk streaming.
//! * [`WorkerPool`] — fixed DSP worker threads running scoped jobs (futex
//!   wake-up, the audio thread's priority) and [`TaskCells`], the per-cycle
//!   pending/running/done cells a parallel graph executor works on.
//! * [`ScopedFlushDenormals`] — flush-to-zero on DSP threads.
//! * [`Workgroup`] — the audio device's `os_workgroup` (macOS) the DSP
//!   workers join.
//!
//! The pool's wake-up is the only syscall made on the audio thread besides
//! reading the clock (`FUTEX_WAKE`, which never blocks). `unsafe` code is
//! confined to `mailbox` and `pages` (pointer ownership transfer through
//! `AtomicPtr`s), `cells` (interior mutability guarded by atomic states),
//! `pool` (scoped job pointer, futex, thread scheduling), `denormals`
//! (the FP control register) and `workgroup` (CoreAudio and
//! `os_workgroup` calls), with invariants documented inline.

mod atomic;
mod cells;
mod denormals;
mod mailbox;
mod meters;
mod metrics;
mod pages;
mod params;
mod pool;
mod scope;
mod slots;
mod trycell;
mod workgroup;

pub use atomic::AtomicF32;
pub use cells::{Claim, TaskCells};
pub use denormals::{ScopedFlushDenormals, flush_denormals_on_this_thread};
pub use mailbox::{MailboxReceiver, MailboxSender, mailbox};
pub use meters::{MeterBank, MeterRange, MeterReading};
pub use metrics::{CallbackMetrics, MetricsSnapshot};
pub use pages::{Epoch, PageTable, Reclaimer, Retired};
pub use params::{ParamSlot, ParamTable};
pub use pool::{
    PoolConfig, PoolJob, WorkerPool, apply_thread_scheduling, default_worker_count, physical_cores,
    thread_scheduling,
};
pub use scope::ScopeRing;
pub use slots::SlotAllocator;
pub use trycell::{TryCell, TryCellGuard};
pub use workgroup::{Membership, Workgroup};
