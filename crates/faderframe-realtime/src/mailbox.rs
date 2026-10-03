//! Single-slot, latest-value-wins handoff of boxed objects to the audio
//! thread.
//!
//! Used for things where only the newest version matters (compiled graphs,
//! timeline snapshots): if the control side publishes twice before the audio
//! thread looks, the older, never-consumed object is handed back to the
//! *sender* to be dropped on the control thread. Taking the value is one
//! atomic swap — wait-free, no allocation, no freeing.

use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};

struct Slot<T> {
    ptr: AtomicPtr<T>,
}

// SAFETY: the slot only ever transfers ownership of a `Box<T>` between
// threads (never shares a `&T`), which is sound for `T: Send`.
unsafe impl<T: Send> Send for Slot<T> {}
// SAFETY: see above; all access goes through atomic swaps.
unsafe impl<T: Send> Sync for Slot<T> {}

impl<T> Drop for Slot<T> {
    fn drop(&mut self) {
        let p = *self.ptr.get_mut();
        if !p.is_null() {
            // SAFETY: a non-null pointer in the slot always came from
            // `Box::into_raw` and has not been reclaimed (every swap that
            // removes it reclaims it exactly once).
            drop(unsafe { Box::from_raw(p) });
        }
    }
}

/// Control-thread end.
pub struct MailboxSender<T> {
    slot: Arc<Slot<T>>,
}

/// Audio-thread end.
pub struct MailboxReceiver<T> {
    slot: Arc<Slot<T>>,
}

/// Create a connected sender/receiver pair.
pub fn mailbox<T: Send>() -> (MailboxSender<T>, MailboxReceiver<T>) {
    let slot = Arc::new(Slot {
        ptr: AtomicPtr::new(ptr::null_mut()),
    });
    (
        MailboxSender {
            slot: Arc::clone(&slot),
        },
        MailboxReceiver { slot },
    )
}

impl<T: Send> MailboxSender<T> {
    /// Publish `value`. Returns the previous value if the receiver had not
    /// taken it yet; drop it here, on the control thread.
    #[must_use = "a replaced value must be dropped on the control thread"]
    pub fn send(&self, value: Box<T>) -> Option<Box<T>> {
        let old = self.slot.ptr.swap(Box::into_raw(value), Ordering::AcqRel);
        if old.is_null() {
            None
        } else {
            // SAFETY: `old` came from `Box::into_raw` and the swap removed it
            // from the slot, so this is its unique owner.
            Some(unsafe { Box::from_raw(old) })
        }
    }
}

impl<T: Send> MailboxReceiver<T> {
    /// Take the newest value, if any (wait-free; audio thread).
    #[inline]
    pub fn take(&self) -> Option<Box<T>> {
        let p = self.slot.ptr.swap(ptr::null_mut(), Ordering::AcqRel);
        if p.is_null() {
            None
        } else {
            // SAFETY: as in `send`, the swap transferred unique ownership.
            Some(unsafe { Box::from_raw(p) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static DROPS: AtomicUsize = AtomicUsize::new(0);

    struct Tracked(u32);

    impl Drop for Tracked {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn latest_value_wins_and_nothing_leaks() {
        let (tx, rx) = mailbox::<Tracked>();
        assert!(rx.take().is_none());
        assert!(tx.send(Box::new(Tracked(1))).is_none());
        let replaced = tx.send(Box::new(Tracked(2))).unwrap();
        assert_eq!(replaced.0, 1);
        drop(replaced);
        assert_eq!(rx.take().unwrap().0, 2);
        assert!(rx.take().is_none());
        let _ = tx.send(Box::new(Tracked(3)));
        let before = DROPS.load(Ordering::SeqCst);
        drop(tx);
        drop(rx); // pending value freed with the slot
        assert_eq!(DROPS.load(Ordering::SeqCst), before + 1);
    }

    #[test]
    fn works_across_threads() {
        let (tx, rx) = mailbox::<u64>();
        let reader = std::thread::spawn(move || {
            let mut last = 0;
            while last != 10_000 {
                if let Some(v) = rx.take() {
                    assert!(*v >= last);
                    last = *v;
                }
            }
        });
        for i in 1..=10_000u64 {
            drop(tx.send(Box::new(i)));
        }
        reader.join().unwrap();
    }
}
