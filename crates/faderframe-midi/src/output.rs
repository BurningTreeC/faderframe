//! Timestamped MIDI output from the audio thread to device threads.
//!
//! The engine pushes every outgoing message with the time it should leave
//! the computer (on the shared [`MidiClock`]): the moment the audio of the
//! same callback is heard, so external instruments play in time with the
//! mix. A sender thread waits until each message is due.

use crate::MidiClock;

/// One message for an output port (channel or system real-time/common).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiOutputEvent {
    /// Output port index (see the session's output port map).
    pub port: u16,
    /// When to send it ([`MidiClock`] nanoseconds).
    pub due_ns: u64,
    pub len: u8,
    pub bytes: [u8; 3],
}

impl MidiOutputEvent {
    pub fn new(port: u16, due_ns: u64, msg: &[u8]) -> Option<Self> {
        if msg.is_empty() || msg.len() > 3 || msg[0] < 0x80 {
            return None;
        }
        let mut bytes = [0u8; 3];
        bytes[..msg.len()].copy_from_slice(msg);
        Some(Self {
            port,
            due_ns,
            len: msg.len() as u8,
            bytes,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

/// The audio thread's end.
pub struct MidiOutputQueue {
    pub producer: rtrb::Producer<MidiOutputEvent>,
    pub clock: MidiClock,
}

/// A queue from the engine to a sender thread (`capacity` messages).
pub fn midi_output_queue(
    capacity: usize,
    clock: MidiClock,
) -> (MidiOutputQueue, rtrb::Consumer<MidiOutputEvent>) {
    let (tx, rx) = rtrb::RingBuffer::new(capacity);
    (
        MidiOutputQueue {
            producer: tx,
            clock,
        },
        rx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_of_one_to_three_bytes() {
        assert!(MidiOutputEvent::new(0, 0, &[0xF8]).is_some(), "clock");
        assert!(
            MidiOutputEvent::new(0, 0, &[0xF2, 1, 2]).is_some(),
            "song position"
        );
        assert!(MidiOutputEvent::new(0, 0, &[0x40]).is_none(), "no status");
        let e = MidiOutputEvent::new(2, 5, &[0x90, 60, 1]).unwrap();
        assert_eq!(e.bytes(), &[0x90, 60, 1]);
        let (mut q, mut rx) = midi_output_queue(4, MidiClock::new());
        q.producer.push(e).unwrap();
        assert_eq!(rx.pop(), Ok(e));
    }
}
