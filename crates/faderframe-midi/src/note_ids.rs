//! Note ids for plugin APIs that address sounding notes by id (CLAP note
//! events and note expressions, VST3 note and note expression events): one
//! per note-on, found again by channel and key. Fixed tables, realtime-safe.

/// The ids of the sounding notes per channel and key.
#[derive(Clone, Debug)]
pub struct NoteIds {
    ids: [[i32; 128]; 16],
    next: i32,
}

impl Default for NoteIds {
    fn default() -> Self {
        Self::new()
    }
}

impl NoteIds {
    /// No note (CLAP and VST3 both use −1 for "unspecified").
    pub const NONE: i32 = -1;

    pub fn new() -> Self {
        Self {
            ids: [[Self::NONE; 128]; 16],
            next: 0,
        }
    }

    /// A fresh id for a note-on (it replaces a note still sounding on the
    /// same channel and key).
    pub fn start(&mut self, channel: u8, key: u8) -> i32 {
        let id = self.next;
        self.next = if self.next == i32::MAX {
            0
        } else {
            self.next + 1
        };
        self.ids[(channel & 15) as usize][(key & 127) as usize] = id;
        id
    }

    /// The sounding note's id ([`NoteIds::NONE`] if none).
    pub fn get(&self, channel: u8, key: u8) -> i32 {
        self.ids[(channel & 15) as usize][(key & 127) as usize]
    }

    /// The id of a note that ends (then forgotten).
    pub fn end(&mut self, channel: u8, key: u8) -> i32 {
        std::mem::replace(
            &mut self.ids[(channel & 15) as usize][(key & 127) as usize],
            Self::NONE,
        )
    }

    /// Forget every note (all notes off, reset).
    pub fn clear(&mut self) {
        self.ids = [[Self::NONE; 128]; 16];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_keep_their_ids_until_they_end() {
        let mut ids = NoteIds::new();
        let a = ids.start(0, 60);
        let b = ids.start(1, 60);
        assert_ne!(a, b);
        assert_eq!(ids.get(0, 60), a);
        assert_eq!(ids.end(0, 60), a);
        assert_eq!(ids.get(0, 60), NoteIds::NONE);
        assert_eq!(ids.end(0, 60), NoteIds::NONE);
        assert_eq!(ids.get(1, 60), b);
        ids.clear();
        assert_eq!(ids.get(1, 60), NoteIds::NONE);
    }
}
