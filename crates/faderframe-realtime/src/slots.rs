/// First-fit allocator for contiguous ranges of slots in a fixed-size table.
///
/// Control-thread only. Used to hand out [`crate::ParamTable`] and
/// [`crate::MeterBank`] slots that stay stable across graph rebuilds.
#[derive(Clone, Debug)]
pub struct SlotAllocator {
    capacity: u32,
    /// Free ranges `(start, len)`, sorted by start and coalesced.
    free: Vec<(u32, u32)>,
}

impl SlotAllocator {
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity,
            free: if capacity > 0 {
                vec![(0, capacity)]
            } else {
                Vec::new()
            },
        }
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn free_slots(&self) -> u32 {
        self.free.iter().map(|&(_, l)| l).sum()
    }

    /// Allocate `len` contiguous slots; `None` if the table is exhausted.
    pub fn allocate(&mut self, len: u32) -> Option<u32> {
        if len == 0 {
            return None;
        }
        let i = self.free.iter().position(|&(_, l)| l >= len)?;
        let (start, l) = self.free[i];
        if l == len {
            self.free.remove(i);
        } else {
            self.free[i] = (start + len, l - len);
        }
        Some(start)
    }

    /// Return a range previously handed out by [`Self::allocate`].
    pub fn release(&mut self, start: u32, len: u32) {
        if len == 0 || start.saturating_add(len) > self.capacity {
            return;
        }
        let i = self.free.partition_point(|&(s, _)| s < start);
        self.free.insert(i, (start, len));
        // Coalesce with neighbours.
        if i + 1 < self.free.len() && self.free[i].0 + self.free[i].1 == self.free[i + 1].0 {
            self.free[i].1 += self.free[i + 1].1;
            self.free.remove(i + 1);
        }
        if i > 0 && self.free[i - 1].0 + self.free[i - 1].1 == self.free[i].0 {
            self.free[i - 1].1 += self.free[i].1;
            self.free.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_release_coalesce() {
        let mut a = SlotAllocator::new(10);
        let x = a.allocate(3).unwrap();
        let y = a.allocate(4).unwrap();
        let z = a.allocate(3).unwrap();
        assert_eq!((x, y, z), (0, 3, 7));
        assert_eq!(a.allocate(1), None);
        a.release(y, 4);
        assert_eq!(a.allocate(5), None);
        a.release(x, 3);
        assert_eq!(a.free_slots(), 7);
        assert_eq!(a.allocate(7), Some(0));
        a.release(0, 7);
        a.release(z, 3);
        assert_eq!(a.allocate(10), Some(0));
    }
}
