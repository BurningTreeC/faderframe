//! A value shared between threads that is only ever *tried*, never waited
//! for.
//!
//! [`TryCell::try_lock`] is wait-free: it either grants exclusive access or
//! fails immediately. The audio thread uses it for state that several graph
//! generations reference (a plugin's live processor survives graph
//! rebuilds); if the cell is busy — the control thread is reconfiguring the
//! plugin — the audio thread skips that block instead of blocking. The
//! control thread may retry (it is allowed to wait).

use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct TryCell<T> {
    busy: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: access to `value` is exclusive while `busy` is held (acquired with
// a compare-exchange, released on guard drop), so sharing the cell between
// threads is sound whenever `T` may be sent between them.
unsafe impl<T: Send> Sync for TryCell<T> {}
// SAFETY: moving the cell moves the value.
unsafe impl<T: Send> Send for TryCell<T> {}

impl<T> TryCell<T> {
    pub fn new(value: T) -> Self {
        Self {
            busy: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Exclusive access, or `None` if someone else holds it. Wait-free.
    #[inline]
    pub fn try_lock(&self) -> Option<TryCellGuard<'_, T>> {
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| TryCellGuard { cell: self })
    }

    /// Control side: retry until free, sleeping between attempts (never
    /// call this on the audio thread). Gives up after `attempts`.
    pub fn lock_blocking(&self, attempts: usize) -> Option<TryCellGuard<'_, T>> {
        for _ in 0..attempts {
            if let Some(g) = self.try_lock() {
                return Some(g);
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        None
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

pub struct TryCellGuard<'a, T> {
    cell: &'a TryCell<T>,
}

impl<T> Deref for TryCellGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the guard holds the `busy` flag, so no other reference exists.
        unsafe { &*self.cell.value.get() }
    }
}

impl<T> DerefMut for TryCellGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard holds the `busy` flag, so no other reference exists.
        unsafe { &mut *self.cell.value.get() }
    }
}

impl<T> Drop for TryCellGuard<'_, T> {
    fn drop(&mut self) {
        self.cell.busy.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn exclusive_and_never_blocking() {
        let c = TryCell::new(5);
        let mut g = c.try_lock().unwrap();
        *g += 1;
        assert!(
            c.try_lock().is_none(),
            "second access fails instead of waiting"
        );
        drop(g);
        assert_eq!(*c.try_lock().unwrap(), 6);
    }

    #[test]
    fn concurrent_increments_are_never_lost() {
        let c = Arc::new(TryCell::new(0u64));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let c = Arc::clone(&c);
            handles.push(std::thread::spawn(move || {
                let mut done = 0;
                while done < 10_000 {
                    if let Some(mut g) = c.try_lock() {
                        *g += 1;
                        done += 1;
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(*c.lock_blocking(10).unwrap(), 40_000);
    }
}
