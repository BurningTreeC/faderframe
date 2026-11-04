//! Drum maps: a track's drum sounds by the key they are written on — each
//! with a name, a place in the editor's list (the map's order, top to
//! bottom), a grid for drawing on its row, and where it goes when it
//! plays: another note, a channel, an external MIDI output.
//!
//! Projects hold their maps ([`crate::Project::drum_maps`]); a track names
//! one ([`crate::Track::drum_map`]). Notes stay written on the map's
//! pitches; the engine's timeline snapshot plays each on its sound's
//! output note and channel, live input is mapped the same way, and a
//! sound with its own port goes to that output (a MIDI track with an
//! external output). The piano roll's drum mode lists the sounds by name
//! in the map's order. Maps come from the GM key map, from a Drum
//! Sampler's pads, or from user files ([`DrumMap::to_file`]).

use faderframe_core::DrumMapId;
use faderframe_timeline::GridDivision;
use serde::{Deserialize, Serialize};

/// What a user file holds (its `format` field).
const FILE_FORMAT: &str = "faderframe-drum-map";

/// One drum sound of a map.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrumSound {
    /// The key its notes are written on.
    pub pitch: u8,
    pub name: String,
    /// The grid for drawing on its row (`None`: the editor's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantize: Option<GridDivision>,
    /// The note it plays (`None`: its pitch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_note: Option<u8>,
    /// The channel it plays on (0-based; `None`: the note's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// An external MIDI output it goes to instead of the track's (a port
    /// key, as [`crate::MidiOutputRouting::port`]; MIDI tracks with an
    /// output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
}

impl DrumSound {
    pub fn new(pitch: u8, name: impl Into<String>) -> Self {
        Self {
            pitch: pitch.min(127),
            name: name.into(),
            quantize: None,
            output_note: None,
            channel: None,
            port: None,
        }
    }

    /// Where it plays: (channel, key) of a note on `channel`.
    pub fn output(&self, channel: u8) -> (u8, u8) {
        (
            self.channel.unwrap_or(channel) & 15,
            self.output_note.unwrap_or(self.pitch) & 127,
        )
    }

    /// Plays as written (its own note, the note's channel).
    pub fn plays_as_written(&self) -> bool {
        self.output_note.is_none_or(|n| n == self.pitch) && self.channel.is_none()
    }
}

/// A track's drum sounds in display order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrumMap {
    pub id: DrumMapId,
    pub name: String,
    /// Top to bottom as the editor lists them; one per pitch.
    #[serde(default)]
    pub sounds: Vec<DrumSound>,
}

#[derive(Serialize, Deserialize)]
struct MapFile {
    format: String,
    version: u32,
    name: String,
    #[serde(default)]
    sounds: Vec<DrumSound>,
}

impl DrumMap {
    pub fn new(id: DrumMapId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            sounds: Vec::new(),
        }
    }

    /// The General MIDI key map (keys 35–81), kick and snare first, then
    /// the toms high to low, the hi-hats, cymbals, the rest in key order.
    pub fn gm(id: DrumMapId) -> Self {
        let first = [36u8, 35, 38, 40, 37, 39, 50, 48, 47, 45, 43, 41, 42, 44, 46];
        let mut order: Vec<u8> = first.to_vec();
        order.extend(
            faderframe_midi::gm::DRUMS
                .iter()
                .map(|(k, _)| *k)
                .filter(|k| !first.contains(k)),
        );
        Self {
            id,
            name: "General MIDI".into(),
            sounds: order
                .into_iter()
                .filter_map(|k| {
                    faderframe_midi::gm::drum_name(k).map(|n| DrumSound::new(k, n.to_string()))
                })
                .collect(),
        }
    }

    /// A Drum Sampler's pads: `pads` consecutive keys from `first`, named
    /// by `names` (a pad without a name is "Pad n"), the first pad on top.
    pub fn pads(id: DrumMapId, first: u8, names: &[Option<String>], pads: usize) -> Self {
        Self {
            id,
            name: "Drum Sampler Pads".into(),
            sounds: (0..pads)
                .filter_map(|p| {
                    let key = u8::try_from(usize::from(first) + p)
                        .ok()
                        .filter(|k| *k < 128)?;
                    let name = names
                        .get(p)
                        .cloned()
                        .flatten()
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| format!("Pad {}", p + 1));
                    Some(DrumSound::new(key, name))
                })
                .collect(),
        }
    }

    /// The sound on `pitch`.
    pub fn sound(&self, pitch: u8) -> Option<&DrumSound> {
        self.sounds.iter().find(|s| s.pitch == pitch)
    }

    pub fn sound_mut(&mut self, pitch: u8) -> Option<&mut DrumSound> {
        self.sounds.iter_mut().find(|s| s.pitch == pitch)
    }

    /// The name of `pitch` (`None`: not in the map).
    pub fn name_of(&self, pitch: u8) -> Option<&str> {
        self.sound(pitch).map(|s| s.name.as_str())
    }

    /// Where a note plays: (channel, key).
    pub fn output(&self, channel: u8, pitch: u8) -> (u8, u8) {
        self.sound(pitch)
            .map_or((channel & 15, pitch & 127), |s| s.output(channel))
    }

    /// Every sound plays as written and on the track's port (the map only
    /// names and orders).
    pub fn plays_as_written(&self) -> bool {
        self.sounds
            .iter()
            .all(|s| s.plays_as_written() && s.port.is_none())
    }

    /// The pitches in display order.
    pub fn order(&self) -> Vec<u8> {
        self.sounds.iter().map(|s| s.pitch).collect()
    }

    /// Put the `index`th sound on `pitch`; a sound already there takes
    /// its old pitch (the two swap: one sound a pitch). False: no such
    /// sound.
    pub fn set_pitch(&mut self, index: usize, pitch: u8) -> bool {
        let pitch = pitch.min(127);
        let Some(old) = self.sounds.get(index).map(|s| s.pitch) else {
            return false;
        };
        if let Some(other) = self.sounds.iter_mut().find(|s| s.pitch == pitch) {
            other.pitch = old;
        }
        self.sounds[index].pitch = pitch;
        true
    }

    /// A pitch no sound has: the first free one from `near` upwards, else
    /// downwards (`None`: all 128 taken).
    pub fn free_pitch(&self, near: u8) -> Option<u8> {
        let taken = |k: u8| self.sounds.iter().any(|s| s.pitch == k);
        let near = near.min(127);
        (near..=127).chain((0..near).rev()).find(|k| !taken(*k))
    }

    /// Kept sane: pitches, notes and channels in range, one sound per
    /// pitch (the first kept), empty ports gone.
    pub fn tidy(&mut self) {
        let mut seen = [false; 128];
        self.sounds.retain_mut(|s| {
            s.pitch = s.pitch.min(127);
            s.output_note = s.output_note.map(|n| n.min(127));
            s.channel = s.channel.map(|c| c.min(15));
            if s.port.as_deref().is_some_and(|p| p.trim().is_empty()) {
                s.port = None;
            }
            !std::mem::replace(&mut seen[usize::from(s.pitch)], true)
        });
    }

    /// The map as a user file (JSON; without its project id).
    pub fn to_file(&self) -> String {
        let f = MapFile {
            format: FILE_FORMAT.into(),
            version: 1,
            name: self.name.clone(),
            sounds: self.sounds.clone(),
        };
        serde_json::to_string_pretty(&f).unwrap_or_default()
    }

    /// A map read from a user file, given `id`.
    pub fn from_file(id: DrumMapId, text: &str) -> Result<Self, String> {
        let f: MapFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if f.format != FILE_FORMAT {
            return Err(format!("not a drum map ({})", f.format));
        }
        let mut m = Self {
            id,
            name: f.name,
            sounds: f.sounds,
        };
        m.tidy();
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gm_map_names_every_gm_sound_kick_and_snare_first() {
        let m = DrumMap::gm(DrumMapId(1));
        assert_eq!(m.sounds.len(), faderframe_midi::gm::DRUMS.len());
        assert_eq!(m.sounds[0].name, "Bass Drum 1");
        assert_eq!(m.sounds[2].pitch, 38);
        assert_eq!(m.name_of(42), Some("Closed Hi-Hat"));
        assert!(m.plays_as_written());
        let mut keys = m.order();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), m.sounds.len(), "one sound per key");
    }

    #[test]
    fn sounds_play_on_their_own_note_and_channel() {
        let mut m = DrumMap::pads(DrumMapId(2), 36, &[Some("Kick".into()), None], 4);
        assert_eq!(m.sounds.len(), 4);
        assert_eq!(m.sounds[0].name, "Kick");
        assert_eq!(m.sounds[1].name, "Pad 2");
        let s = m.sound_mut(37).unwrap();
        s.output_note = Some(60);
        s.channel = Some(9);
        assert_eq!(m.output(0, 37), (9, 60));
        assert_eq!(m.output(3, 36), (3, 36));
        // Not in the map: as written.
        assert_eq!(m.output(1, 100), (1, 100));
        assert!(!m.plays_as_written());
    }

    #[test]
    fn tidy_keeps_one_sound_a_pitch_and_files_round_trip() {
        let mut m = DrumMap::gm(DrumMapId(3));
        m.sounds.push(DrumSound::new(36, "Another kick"));
        m.sounds[1].port = Some("  ".into());
        m.sounds[3].quantize = Some(GridDivision::Note(16));
        m.tidy();
        assert_eq!(m.sounds.len(), faderframe_midi::gm::DRUMS.len());
        assert_eq!(m.name_of(36), Some("Bass Drum 1"));
        assert_eq!(m.sounds[1].port, None);
        let back = DrumMap::from_file(DrumMapId(9), &m.to_file()).unwrap();
        assert_eq!(back.sounds, m.sounds);
        assert_eq!(back.id, DrumMapId(9));
        assert!(DrumMap::from_file(DrumMapId(9), "{}").is_err());
    }

    #[test]
    fn a_sound_moved_to_a_taken_pitch_swaps_with_the_sound_there() {
        let mut m = DrumMap::pads(DrumMapId(4), 36, &[], 3);
        assert!(m.set_pitch(0, 38));
        assert_eq!(m.order(), vec![38, 37, 36], "the kick and pad 3 swap");
        assert_eq!(m.sounds[0].name, "Pad 1");
        assert!(m.set_pitch(1, 60));
        assert_eq!(m.order(), vec![38, 60, 36]);
        assert!(!m.set_pitch(9, 40), "no tenth sound");
        assert_eq!(m.free_pitch(36), Some(37));
        assert_eq!(m.free_pitch(200), Some(127));
        let mut full = DrumMap::pads(DrumMapId(5), 0, &[], 128);
        assert_eq!(full.free_pitch(60), None);
        full.sounds.pop();
        assert_eq!(full.free_pitch(0), Some(127), "upwards first, then down");
    }

    #[test]
    fn a_drum_map_named_by_a_track_stays_and_audio_tracks_take_none() {
        use crate::{Command, Project, Track, TrackColor, TrackKind};
        let mut p = Project::new("Drums", 48_000);
        let drums: faderframe_core::TrackId = p.ids.allocate();
        let audio: faderframe_core::TrackId = p.ids.allocate();
        for (id, kind) in [(drums, TrackKind::Midi), (audio, TrackKind::Audio)] {
            Command::AddTrack {
                track: Box::new(Track::new(id, kind, "T", TrackColor::palette(0))),
                index: 0,
            }
            .apply(&mut p)
            .unwrap();
        }
        let map: DrumMapId = p.ids.allocate();
        Command::SetDrumMap {
            map,
            value: Some(Box::new(DrumMap::gm(map))),
            index: 0,
        }
        .apply(&mut p)
        .unwrap();
        Command::SetTrackDrumMap {
            track: drums,
            map: Some(map),
        }
        .apply(&mut p)
        .unwrap();
        assert_eq!(p.track_drum_map(drums).map(|m| m.sounds.len()), Some(47));
        assert!(
            Command::SetTrackDrumMap {
                track: audio,
                map: Some(map)
            }
            .apply(&mut p)
            .is_err()
        );
        assert!(
            Command::SetDrumMap {
                map,
                value: None,
                index: 0
            }
            .apply(&mut p)
            .is_err()
        );
        // A file naming a map that is gone loads without it.
        p.drum_maps.clear();
        let notes = p.repair();
        assert_eq!(p.track(drums).unwrap().drum_map, None);
        assert!(notes.iter().any(|n| n.contains("drum map")), "{notes:?}");
    }
}
