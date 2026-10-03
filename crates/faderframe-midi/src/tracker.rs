use crate::{MidiBuffer, MidiEvent, TimedMidiEvent};

/// Tracks sounding notes so that hanging notes can be released when the
/// transport stops, locates or loops. Fixed size, realtime-safe.
#[derive(Clone, Debug, Default)]
pub struct NoteTracker {
    /// One 128-bit mask per channel.
    active: [u128; 16],
}

impl NoteTracker {
    /// Update state from an outgoing event.
    #[inline]
    pub fn observe(&mut self, event: MidiEvent) {
        match event {
            MidiEvent::NoteOn { channel, key, .. } => {
                self.active[(channel & 15) as usize] |= 1u128 << (key & 127);
            }
            MidiEvent::NoteOff { channel, key, .. } => {
                self.active[(channel & 15) as usize] &= !(1u128 << (key & 127));
            }
            _ => {}
        }
    }

    pub fn any_active(&self) -> bool {
        self.active.iter().any(|&m| m != 0)
    }

    pub fn is_active(&self, channel: u8, key: u8) -> bool {
        self.active[(channel & 15) as usize] & (1u128 << (key & 127)) != 0
    }

    /// Emit note-offs for every sounding note at `sample_offset` and clear.
    pub fn release_all(&mut self, out: &mut MidiBuffer, sample_offset: u32) {
        for (channel, mask) in self.active.iter_mut().enumerate() {
            let mut m = *mask;
            while m != 0 {
                let key = m.trailing_zeros() as u8;
                m &= m - 1;
                let _ = out.push(TimedMidiEvent::new(
                    sample_offset,
                    MidiEvent::NoteOff {
                        channel: channel as u8,
                        key,
                        velocity: 0,
                    },
                ));
            }
            *mask = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_hanging_notes() {
        let mut t = NoteTracker::default();
        t.observe(MidiEvent::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1,
        });
        t.observe(MidiEvent::NoteOn {
            channel: 9,
            key: 36,
            velocity: 1,
        });
        t.observe(MidiEvent::NoteOn {
            channel: 0,
            key: 64,
            velocity: 1,
        });
        t.observe(MidiEvent::NoteOff {
            channel: 0,
            key: 64,
            velocity: 0,
        });
        assert!(t.is_active(0, 60) && !t.is_active(0, 64));
        let mut out = MidiBuffer::with_capacity(16);
        t.release_all(&mut out, 7);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|e| e.sample_offset == 7));
        assert!(!t.any_active());
    }
}
