//! Per-cycle task cells for parallel graph execution.
//!
//! A [`TaskCells`] array holds one value per task (a graph node's buffers
//! or processor). During a processing cycle every cell goes
//! `pending → running → done` exactly once:
//!
//! * [`TaskCells::claim`] moves a pending cell to running and hands out
//!   exclusive access (`&mut T`) to the one thread that won;
//! * dropping the [`Claim`] marks the cell done (release);
//! * [`TaskCells::done`] gives shared access (`&T`) to a finished cell
//!   (acquire), so a node can read its upstream outputs on any thread;
//! * [`TaskCells::reset`] (needs `&mut self`) starts the next cycle.
//!
//! The states make the API sound on their own: a cell is either borrowed
//! mutably by its single claimant or shared read-only after it finished —
//! never both — whatever the scheduler does. A scheduler bug shows up as a
//! failed `claim`/`done` (the caller skips work), not as a data race.

use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU8, Ordering};

const PENDING: u8 = 0;
const RUNNING: u8 = 1;
const DONE: u8 = 2;

struct Cell<T> {
    state: AtomicU8,
    value: UnsafeCell<T>,
}

pub struct TaskCells<T> {
    cells: Box<[Cell<T>]>,
}

// SAFETY: a value is reached either through `&mut self`, through exactly
// one `Claim` (exclusive, used by whichever thread claimed it: needs
// `T: Send`), or through `done`, which shares it between threads and is
// only available for `T: Sync`. The state transitions are atomic with
// acquire/release ordering, so the claimant's writes happen-before every
// reader's access. Claim-only cells (e.g. processors, `Send` but not
// `Sync`) are therefore fine to share.
unsafe impl<T: Send> Send for TaskCells<T> {}
// SAFETY: see above.
unsafe impl<T: Send> Sync for TaskCells<T> {}

/// Exclusive access to one cell for the current cycle.
pub struct Claim<'a, T> {
    cell: &'a Cell<T>,
}

impl<T> TaskCells<T> {
    pub fn new(values: Vec<T>) -> Self {
        Self {
            cells: values
                .into_iter()
                .map(|v| Cell {
                    state: AtomicU8::new(PENDING),
                    value: UnsafeCell::new(v),
                })
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Start a new cycle: every cell pending.
    pub fn reset(&mut self) {
        for c in self.cells.iter_mut() {
            *c.state.get_mut() = PENDING;
        }
    }

    /// Exclusive access to a pending cell; `None` if it already ran (or is
    /// running) this cycle.
    #[inline]
    pub fn claim(&self, i: usize) -> Option<Claim<'_, T>> {
        let cell = self.cells.get(i)?;
        cell.state
            .compare_exchange(PENDING, RUNNING, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some(Claim { cell })
    }

    /// Shared access to a cell that finished this cycle.
    #[inline]
    pub fn done(&self, i: usize) -> Option<&T>
    where
        T: Sync,
    {
        let cell = self.cells.get(i)?;
        if cell.state.load(Ordering::Acquire) == DONE {
            // SAFETY: done cells are never claimed again before a `reset`
            // (which needs `&mut self`), so no `&mut T` can exist.
            Some(unsafe { &*cell.value.get() })
        } else {
            None
        }
    }

    #[inline]
    pub fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        self.cells.get_mut(i).map(|c| c.value.get_mut())
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.cells.iter_mut().map(|c| c.value.get_mut())
    }
}

impl<T> Deref for Claim<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: the claim is the only access while the cell is running.
        unsafe { &*self.cell.value.get() }
    }
}

impl<T> DerefMut for Claim<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as above.
        unsafe { &mut *self.cell.value.get() }
    }
}

impl<T> Drop for Claim<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.cell.state.store(DONE, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_go_pending_running_done_once_per_cycle() {
        let mut cells = TaskCells::new(vec![1, 2, 3]);
        assert!(cells.done(0).is_none());
        {
            let mut c = cells.claim(0).unwrap();
            *c += 10;
            assert!(cells.claim(0).is_none(), "exclusive");
            assert!(cells.done(0).is_none(), "not readable while running");
        }
        assert_eq!(cells.done(0), Some(&11));
        assert!(cells.claim(0).is_none(), "once per cycle");
        cells.reset();
        assert!(cells.done(0).is_none());
        assert!(cells.claim(0).is_some());
        assert_eq!(cells.get_mut(1), Some(&mut 2));
    }

    #[test]
    fn claims_publish_writes_to_other_threads() {
        let cells = TaskCells::new((0..64).map(|_| vec![0u64; 16]).collect());
        std::thread::scope(|s| {
            s.spawn(|| {
                for i in 0..64 {
                    let mut c = cells.claim(i).unwrap();
                    c.fill(i as u64 + 1);
                }
            });
            s.spawn(|| {
                for i in 0..64 {
                    let v = loop {
                        if let Some(v) = cells.done(i) {
                            break v;
                        }
                        std::hint::spin_loop();
                    };
                    assert!(v.iter().all(|&x| x == i as u64 + 1));
                }
            });
        });
    }
}
