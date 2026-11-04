//! Addresses for endpoints that have none of their own (CoreMIDI's): a
//! `(client, port)` pair per endpoint, given by its unique id and kept
//! while the client is open — a device unplugged and plugged in again
//! (same unique id) keeps its address. Clients 1–254 (0 is FaderFrame's
//! own, 255 the inputs' "sent straight to FaderFrame"); port 0 is a
//! source, 1 a destination.

use std::collections::HashMap;

#[derive(Debug, Default)]
pub(crate) struct Addresses {
    by_id: HashMap<(u32, bool), (u8, u8)>,
    next: u8,
}

impl Addresses {
    /// The address of endpoint `id` (a source or a destination); `None`
    /// once 254 endpoints have one.
    pub(crate) fn of(&mut self, id: u32, source: bool) -> Option<(u8, u8)> {
        if let Some(a) = self.by_id.get(&(id, source)) {
            return Some(*a);
        }
        if self.next >= 254 {
            return None;
        }
        self.next += 1;
        let a = (self.next, u8::from(!source));
        self.by_id.insert((id, source), a);
        Some(a)
    }

    /// The unique id of the endpoint at `addr`.
    pub(crate) fn id(&self, addr: (u8, u8)) -> Option<u32> {
        self.by_id
            .iter()
            .find(|(_, a)| **a == addr)
            .map(|((id, _), _)| *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_keep_their_addresses() {
        let mut a = Addresses::default();
        let src = a.of(0xBEEF, true).unwrap();
        let dst = a.of(0xBEEF, false).unwrap();
        assert_ne!(src, dst);
        assert_eq!((src.1, dst.1), (0, 1));
        assert_eq!(a.of(0xBEEF, true), Some(src), "the same again");
        assert_eq!(a.id(dst), Some(0xBEEF));
        assert_eq!(a.id((200, 0)), None);
        for id in 0..300u32 {
            let _ = a.of(id, true);
        }
        assert_eq!(a.of(9999, true), None, "254 at most");
        assert!(a.by_id.values().all(|(c, _)| (1..=254).contains(c)));
    }
}
