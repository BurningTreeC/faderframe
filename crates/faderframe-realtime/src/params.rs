use crate::AtomicF32;
use std::sync::atomic::Ordering;

/// Index of a value in a [`ParamTable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParamSlot(pub u32);

/// Fixed-size table of continuously controllable values (fader gain, pan,
/// mute state, send level ...), written by the control thread and read by
/// the audio thread at block boundaries.
///
/// This is the "latest value wins" channel for user gestures. Processors
/// smooth towards the value they read, so a missed intermediate value is
/// harmless. Sample-accurate automation uses event lists instead.
#[derive(Debug)]
pub struct ParamTable {
    values: Box<[AtomicF32]>,
}

impl ParamTable {
    pub fn new(capacity: u32) -> Self {
        Self {
            values: (0..capacity).map(|_| AtomicF32::new(0.0)).collect(),
        }
    }

    pub fn capacity(&self) -> u32 {
        self.values.len() as u32
    }

    /// Read a value (audio thread). Out-of-range slots read as 0.
    #[inline]
    pub fn get(&self, slot: ParamSlot) -> f32 {
        self.values
            .get(slot.0 as usize)
            .map_or(0.0, |v| v.load(Ordering::Relaxed))
    }

    /// Write a value (control thread).
    #[inline]
    pub fn set(&self, slot: ParamSlot, value: f32) {
        if let Some(v) = self.values.get(slot.0 as usize) {
            v.store(value, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_and_out_of_range() {
        let t = ParamTable::new(4);
        t.set(ParamSlot(2), 0.5);
        assert_eq!(t.get(ParamSlot(2)), 0.5);
        t.set(ParamSlot(9), 1.0);
        assert_eq!(t.get(ParamSlot(9)), 0.0);
    }
}
