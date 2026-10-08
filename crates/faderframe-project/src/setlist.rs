//! A setlist: the songs of a show in the order they are played, each a
//! range of the project (one project holds the show, a song a stretch of
//! it — usually a section), with what happens after it and the
//! performer's notes. Show mode (in the session) plays it.

use faderframe_timeline::MusicalTime;
use serde::{Deserialize, Serialize};

/// What happens when a song ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum AfterSong {
    /// Stop on its last frame and stand on the next song.
    #[default]
    Stop,
    /// Stop, then play the next song after `gap` seconds.
    Next { gap: f32 },
    /// Play straight on (the next song follows in the project).
    Continue,
}

impl AfterSong {
    pub fn label(self) -> String {
        match self {
            AfterSong::Stop => "Stop".into(),
            AfterSong::Next { gap } if gap <= 0.0 => "Next song at once".into(),
            AfterSong::Next { gap } => format!("Next song after {gap:.0} s"),
            AfterSong::Continue => "Play on".into(),
        }
    }
}

/// One song of the show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetSong {
    pub name: String,
    /// Where it is in the project.
    pub start: MusicalTime,
    pub end: MusicalTime,
    #[serde(default)]
    pub then: AfterSong,
    /// The performer's notes (shown in show mode).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

/// The show's songs in order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Setlist {
    #[serde(default)]
    pub songs: Vec<SetSong>,
}

impl Setlist {
    pub fn is_empty(&self) -> bool {
        self.songs.is_empty()
    }

    /// Ranges kept sane (start before end, not before the project start).
    pub fn tidy(&mut self) {
        for s in &mut self.songs {
            s.start = s.start.max(MusicalTime::ZERO);
            if s.end < s.start {
                std::mem::swap(&mut s.start, &mut s.end);
            }
            if let AfterSong::Next { gap } = &mut s.then {
                *gap = gap.clamp(0.0, 600.0);
            }
        }
    }
}
