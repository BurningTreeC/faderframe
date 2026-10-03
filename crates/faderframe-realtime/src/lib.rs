//! Realtime-safe building blocks shared by the engine and its control side.
//!
//! Everything in this crate is usable from the audio thread: no allocation,
//! no locks, no syscalls beyond reading the monotonic clock. Allocation
//! happens only in constructors, which must run on a control thread.
//!
//! * [`AtomicF32`] — lock-free `f32` cell.
//! * [`ParamTable`] — control → RT continuous parameter values.
//! * [`MeterBank`] — RT → control peak/RMS meter values.
//! * [`CallbackMetrics`] — callback duration histogram, deadline misses,
//!   xruns; percentiles are computed on the control side.
//! * [`SlotAllocator`] — control-side allocator for table slots.
//! * [`mailbox`] — latest-value handoff of boxed objects (graphs, snapshots).
//!
//! `mailbox` contains the crate's only `unsafe` code (pointer ownership
//! transfer through an `AtomicPtr`), with its invariants documented inline.

mod atomic;
mod mailbox;
mod meters;
mod metrics;
mod params;
mod slots;

pub use atomic::AtomicF32;
pub use mailbox::{MailboxReceiver, MailboxSender, mailbox};
pub use meters::{MeterBank, MeterRange, MeterReading};
pub use metrics::{CallbackMetrics, MetricsSnapshot};
pub use params::{ParamSlot, ParamTable};
pub use slots::SlotAllocator;
