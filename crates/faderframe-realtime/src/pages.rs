//! Lock-free page table with epoch-based reclamation, for disk streaming.
//!
//! A [`PageTable`] holds optional, immutable pages (e.g. a few thousand
//! decoded frames of an audio file). One **loader** thread installs and
//! evicts pages; one **reader** (the audio thread) borrows pages for the
//! duration of a closure. Reading is a single atomic load — wait-free, no
//! allocation, no locking.
//!
//! Evicted pages are not freed immediately: the loader wraps them in a
//! [`Retired`] stamped with the reader's [`Epoch`] and frees them only once
//! the reader has completed at least one more processing cycle, at which
//! point it can no longer hold a reference.
//!
//! # Protocol
//!
//! * The reader only accesses pages inside [`PageTable::with`] and never
//!   keeps a reference beyond the closure (enforced by the closure lifetime).
//! * The reader calls [`Epoch::advance`] after every processing cycle in
//!   which it may have read pages (the engine does this at the end of every
//!   callback).
//! * Exactly one loader mutates a given table at a time.
//! * Retired pages are freed only via [`Retired::try_free`] /
//!   [`Reclaimer`], which checks the epoch.
//!
//! # Ordering
//!
//! The slot load and the epoch increment of the reader, and the unlinking
//! swap and the epoch read of the loader, are sequentially consistent. With
//! acquire/release alone the loader's epoch read may return a stale count:
//! a page the reader loaded in its current cycle would then look safe one
//! cycle early and be freed while it is read (seen as torn pages on
//! Apple Silicon). In the single total order, a reader load that still saw
//! the old pointer precedes the unlink, so the epoch read after it sees at
//! least every increment before that load. Loads cost the same as acquire
//! loads on x86 and ARMv8; the increment happens once per callback.

use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

/// Counts completed reader cycles.
#[derive(Debug, Default)]
pub struct Epoch {
    done: AtomicU64,
}

impl Epoch {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Reader: a processing cycle is complete (no page references are held).
    #[inline]
    pub fn advance(&self) {
        self.done.fetch_add(1, Ordering::SeqCst);
    }

    /// Number of completed reader cycles.
    #[inline]
    pub fn completed(&self) -> u64 {
        self.done.load(Ordering::SeqCst)
    }
}

/// A page removed from a table, waiting until the reader cannot see it.
pub struct Retired<T> {
    page: Box<T>,
    /// Reader cycles completed when the page was unlinked.
    seen: u64,
}

impl<T> Retired<T> {
    /// Free the page if the reader has completed a cycle since it was
    /// unlinked; otherwise give it back.
    pub fn try_free(self, epoch: &Epoch) -> Result<(), Self> {
        if epoch.completed() > self.seen {
            drop(self.page);
            Ok(())
        } else {
            Err(self)
        }
    }

    /// Free unconditionally. Only valid when the reader is known to be gone
    /// (e.g. its engine was destroyed).
    pub fn free_now(self) {
        drop(self.page);
    }
}

/// Fixed-size table of optional pages.
pub struct PageTable<T> {
    slots: Box<[AtomicPtr<T>]>,
}

// SAFETY: the table only hands out shared references to `T` (requires
// `T: Sync`) and transfers ownership of boxed pages between threads
// (requires `T: Send`).
unsafe impl<T: Send + Sync> Send for PageTable<T> {}
// SAFETY: as above; all slot access is atomic.
unsafe impl<T: Send + Sync> Sync for PageTable<T> {}

impl<T> Drop for PageTable<T> {
    fn drop(&mut self) {
        for slot in self.slots.iter_mut() {
            let p = *slot.get_mut();
            if !p.is_null() {
                // SAFETY: we have exclusive access (`&mut self`), so no reader
                // can hold a reference; the pointer came from `Box::into_raw`.
                drop(unsafe { Box::from_raw(p) });
            }
        }
    }
}

impl<T: Send + Sync> PageTable<T> {
    pub fn new(len: usize) -> Self {
        Self {
            slots: (0..len).map(|_| AtomicPtr::new(ptr::null_mut())).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Is page `i` resident?
    #[inline]
    pub fn is_resident(&self, i: usize) -> bool {
        self.slots
            .get(i)
            .is_some_and(|s| !s.load(Ordering::Acquire).is_null())
    }

    /// Reader: borrow page `i` (or `None` if not resident) for the closure.
    #[inline]
    pub fn with<R>(&self, i: usize, f: impl FnOnce(Option<&T>) -> R) -> R {
        let p = self
            .slots
            .get(i)
            .map_or(ptr::null_mut(), |s| s.load(Ordering::SeqCst));
        // SAFETY: a non-null pointer was installed from `Box::into_raw` and
        // is only freed after the reader completes a cycle following its
        // removal (see the module protocol); the reference cannot escape `f`.
        let page = unsafe { p.as_ref() };
        f(page)
    }

    /// Loader: install page `i`. Returns the previous page, retired.
    pub fn install(&self, i: usize, page: Box<T>, epoch: &Epoch) -> Option<Retired<T>> {
        let slot = self.slots.get(i)?;
        let old = slot.swap(Box::into_raw(page), Ordering::SeqCst);
        Self::retire(old, epoch)
    }

    /// Loader: remove page `i`.
    pub fn evict(&self, i: usize, epoch: &Epoch) -> Option<Retired<T>> {
        let slot = self.slots.get(i)?;
        let old = slot.swap(ptr::null_mut(), Ordering::SeqCst);
        Self::retire(old, epoch)
    }

    fn retire(old: *mut T, epoch: &Epoch) -> Option<Retired<T>> {
        if old.is_null() {
            return None;
        }
        // Read the epoch *after* unlinking. A reader that obtained the old
        // pointer did so in a cycle that had not completed yet, so once the
        // completed count exceeds `seen` that cycle (and its reference) is
        // over; later cycles can no longer observe the unlinked pointer.
        let seen = epoch.completed();
        // SAFETY: the swap removed the pointer from the table, so this is
        // its unique owner; it came from `Box::into_raw`.
        Some(Retired {
            page: unsafe { Box::from_raw(old) },
            seen,
        })
    }
}

/// Holds retired pages until their epoch has passed.
pub struct Reclaimer<T> {
    pending: Vec<Retired<T>>,
}

impl<T> Default for Reclaimer<T> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
        }
    }
}

impl<T> Reclaimer<T> {
    pub fn push(&mut self, r: Retired<T>) {
        self.pending.push(r);
    }

    pub fn extend(&mut self, it: impl IntoIterator<Item = Retired<T>>) {
        self.pending.extend(it);
    }

    /// Free everything whose epoch has passed; returns how many remain.
    pub fn collect(&mut self, epoch: &Epoch) -> usize {
        let pending = std::mem::take(&mut self.pending);
        for r in pending {
            if let Err(r) = r.try_free(epoch) {
                self.pending.push(r);
            }
        }
        self.pending.len()
    }

    /// The reader is gone: free everything.
    pub fn free_all(&mut self) {
        for r in self.pending.drain(..) {
            r.free_now();
        }
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn install_read_evict_reclaim() {
        let epoch = Epoch::new();
        let table = PageTable::<Vec<f32>>::new(4);
        assert!(table.with(1, |p| p.is_none()));
        assert!(table.install(1, Box::new(vec![1.0, 2.0]), &epoch).is_none());
        assert_eq!(table.with(1, |p| p.map(|v| v[1])), Some(2.0));
        let mut rec = Reclaimer::default();
        rec.extend(table.install(1, Box::new(vec![3.0]), &epoch));
        // Not freed until the reader completes a cycle.
        assert_eq!(rec.collect(&epoch), 1);
        epoch.advance();
        assert_eq!(rec.collect(&epoch), 0);
        rec.extend(table.evict(1, &epoch));
        assert!(!table.is_resident(1));
        assert!(
            table.with(9, |p| p.is_none()),
            "out of range reads as missing"
        );
        rec.free_all();
    }

    #[test]
    fn concurrent_reader_and_loader() {
        let epoch = Epoch::new();
        let table = Arc::new(PageTable::<[u64; 64]>::new(8));
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (table, epoch, stop) = (Arc::clone(&table), Arc::clone(&epoch), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut reads = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    for i in 0..8 {
                        table.with(i, |p| {
                            if let Some(p) = p {
                                // Every page is internally consistent.
                                assert!(p.iter().all(|v| *v == p[0]));
                                reads += 1;
                            }
                        });
                    }
                    epoch.advance();
                }
                reads
            })
        };
        let mut rec = Reclaimer::default();
        for n in 0..20_000u64 {
            let i = (n % 8) as usize;
            if n % 3 == 0 {
                rec.extend(table.evict(i, &epoch));
            } else {
                rec.extend(table.install(i, Box::new([n; 64]), &epoch));
            }
            rec.collect(&epoch);
        }
        stop.store(true, Ordering::Relaxed);
        assert!(reader.join().unwrap() > 0);
        epoch.advance();
        rec.collect(&epoch);
        assert_eq!(rec.pending(), 0);
    }
}
