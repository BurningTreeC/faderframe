//! ADR (dialogue replaced in the studio): the cue list — each line to
//! record again, who says it and where — and how the talent is cued in
//! (beeps before the line, a streamer across the picture and a punch where
//! it starts).

use faderframe_core::{AdrCueId, TrackId};
use faderframe_timeline::MusicalTime;
use serde::{Deserialize, Serialize};

/// A line to record again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdrCue {
    pub id: AdrCueId,
    /// As on the cue sheet ("12", "12A").
    pub number: String,
    /// Who says it.
    #[serde(default)]
    pub character: String,
    /// The line.
    pub text: String,
    pub start: MusicalTime,
    pub end: MusicalTime,
    /// The track its takes go on (`None`: not chosen yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackId>,
    /// Notes for the session (reason, direction).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Recorded to everyone's liking.
    #[serde(default)]
    pub done: bool,
}

/// How the talent is cued in.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdrSettings {
    /// Beeps a second apart before the line (its start is the silent one
    /// after them).
    pub beeps: u8,
    pub beep_hz: f32,
    /// How long the streamer takes to cross the picture (seconds).
    pub streamer: f64,
    /// Played before the first beep and after the line (seconds).
    pub preroll: f64,
    pub postroll: f64,
    /// Streamers, punches and the line over the picture while playing.
    pub on_picture: bool,
}

impl Default for AdrSettings {
    fn default() -> Self {
        Self {
            beeps: 3,
            beep_hz: 1000.0,
            streamer: 2.0,
            preroll: 1.0,
            postroll: 1.0,
            on_picture: true,
        }
    }
}

/// The cue list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Adr {
    /// By start.
    pub cues: Vec<AdrCue>,
    #[serde(default)]
    pub settings: AdrSettings,
}

impl Adr {
    pub fn is_empty(&self) -> bool {
        self.cues.is_empty() && self.settings == AdrSettings::default()
    }

    pub fn cue(&self, id: AdrCueId) -> Option<&AdrCue> {
        self.cues.iter().find(|c| c.id == id)
    }

    /// Cues by start; numbers given to those without, in order.
    pub fn tidy(&mut self) {
        self.cues.sort_by_key(|c| c.start);
        let mut n = self
            .cues
            .iter()
            .filter_map(|c| c.number.trim().parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        for c in &mut self.cues {
            if c.number.trim().is_empty() {
                n += 1;
                c.number = n.to_string();
            }
        }
    }
}
