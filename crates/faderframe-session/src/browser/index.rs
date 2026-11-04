//! The media browser's index: what is known about each file seen (read
//! from its container, analysed from its audio), kept in a small JSON file
//! in the data folder; and the user's own notes (tags, ratings, recent
//! files, folders), kept apart so the index can be thrown away.
//!
//! An entry holds for as long as the file's size and modification time
//! are those it was made from ([`Stamp`]): a file changed on disk is read
//! and analysed again. Entries not seen for the longest go first when the
//! index grows past [`MAX_ENTRIES`].

use faderframe_analysis::sound_print::SoundPrint;
use faderframe_project::harmony::Key;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The index file's version: entries of another are analysed again.
pub const INDEX_VERSION: u32 = 1;
/// Most files the index keeps.
pub const MAX_ENTRIES: usize = 100_000;
/// Points of a waveform thumbnail.
pub const THUMB_POINTS: usize = 128;
/// Recent files kept.
pub const MAX_RECENT: usize = 50;
/// Highest rating.
pub const MAX_RATING: u8 = 5;

/// A file as it was when it was read: its size and modification time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    /// Modification time (ns since the epoch; 0 unknown).
    pub modified: u128,
}

impl Stamp {
    pub fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            size: meta.len(),
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos()),
        }
    }

    pub fn read(path: &Path) -> Option<Self> {
        std::fs::metadata(path).ok().map(|m| Self::of(&m))
    }
}

/// What a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Audio,
    Midi,
}

impl FileKind {
    /// The kind of `path` by its extension (`None`: not media the browser
    /// shows).
    pub fn of(path: &Path) -> Option<Self> {
        if crate::is_midi_file(path) {
            Some(Self::Midi)
        } else if faderframe_audio_files::decode::is_supported(path) {
            Some(Self::Audio)
        } else {
            None
        }
    }
}

/// What the container (or the MIDI file's header) says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Facts {
    /// "WAV", "FLAC", "MP3", "MIDI" …
    pub format: String,
    /// Bits a sample (lossless audio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<u32>,
    pub channels: u16,
    /// 0 for MIDI files.
    pub rate: u32,
    /// Length in seconds (MIDI: at the file's own tempo).
    pub seconds: f64,
}

/// What the audio (or the notes) was found to be.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    /// Beats a minute, when there is a clear beat (MIDI: the file's tempo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    /// The key, when the sound holds one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Key>,
    /// The waveform (or the notes' density), [`THUMB_POINTS`] levels as
    /// hex bytes (0–255).
    #[serde(default)]
    pub thumb: String,
    /// The [`SoundPrint`]'s values (empty: none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub print: Vec<f32>,
    /// Exact length in seconds (a container that did not say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
}

impl Analysis {
    /// The thumbnail's levels (0…1).
    pub fn thumb_levels(&self) -> Vec<f32> {
        thumb_levels(&self.thumb)
    }

    pub fn sound_print(&self) -> Option<SoundPrint> {
        SoundPrint::from_values(&self.print)
    }
}

/// Levels (0…1) as the hex thumbnail an index keeps.
pub fn thumb_text(levels: &[f32]) -> String {
    levels
        .iter()
        .map(|v| format!("{:02x}", (v.clamp(0.0, 1.0) * 255.0).round() as u8))
        .collect()
}

/// The levels of a hex thumbnail.
pub fn thumb_levels(text: &str) -> Vec<f32> {
    text.as_bytes()
        .chunks(2)
        .filter_map(|c| std::str::from_utf8(c).ok())
        .filter_map(|h| u8::from_str_radix(h, 16).ok())
        .map(|b| f32::from(b) / 255.0)
        .collect()
}

/// What the index knows of one file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub stamp: Stamp,
    pub kind: FileKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Facts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<Analysis>,
    /// Neither could be read (not media after all, damaged).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unreadable: bool,
    /// When it was last listed (seconds since the epoch).
    #[serde(default)]
    pub seen: u64,
}

impl Entry {
    pub fn new(stamp: Stamp, kind: FileKind) -> Self {
        Self {
            stamp,
            kind,
            facts: None,
            analysis: None,
            unreadable: false,
            seen: now_secs(),
        }
    }

    /// The length in seconds (the analysis's when the container did not
    /// say).
    pub fn seconds(&self) -> Option<f64> {
        self.analysis
            .as_ref()
            .and_then(|a| a.seconds)
            .or_else(|| self.facts.as_ref().map(|f| f.seconds))
            .filter(|s| *s > 0.0)
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The index file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Index {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub entries: BTreeMap<PathBuf, Entry>,
}

impl Index {
    /// Read `path` (none, or of another version: an empty index).
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::fresh();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(i) if i.version == INDEX_VERSION => i,
            Ok(_) => Self::fresh(),
            Err(e) => {
                tracing::warn!("media browser index {}: {e}", path.display());
                Self::fresh()
            }
        }
    }

    pub fn fresh() -> Self {
        Self {
            version: INDEX_VERSION,
            entries: BTreeMap::new(),
        }
    }

    /// The file's entry while it is as it was (`stamp` = now).
    pub fn current(&self, path: &Path, stamp: Stamp) -> Option<&Entry> {
        self.entries.get(path).filter(|e| e.stamp == stamp)
    }

    /// Note `path` listed as `stamp`: its entry, made anew when the file
    /// changed. Returns whether it needs reading (no facts) and analysing.
    pub fn note(&mut self, path: &Path, stamp: Stamp, kind: FileKind) -> (bool, bool) {
        let fresh = |e: &Entry| e.stamp == stamp && e.kind == kind;
        let e = match self.entries.get_mut(path) {
            Some(e) if fresh(e) => e,
            _ => {
                self.entries
                    .insert(path.to_path_buf(), Entry::new(stamp, kind));
                self.entries.get_mut(path).expect("just inserted")
            }
        };
        e.seen = now_secs();
        if e.unreadable {
            return (false, false);
        }
        (e.facts.is_none(), e.analysis.is_none())
    }

    /// Keep within [`MAX_ENTRIES`] (the longest unseen go).
    pub fn prune(&mut self) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        let mut by_age: Vec<(u64, PathBuf)> = self
            .entries
            .iter()
            .map(|(p, e)| (e.seen, p.clone()))
            .collect();
        by_age.sort();
        let excess = self.entries.len() - MAX_ENTRIES;
        for (_, p) in by_age.into_iter().take(excess) {
            self.entries.remove(&p);
        }
    }

    /// Write it (whole or not at all).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        write_json(
            path,
            &serde_json::to_string(self).map_err(std::io::Error::other)?,
        )
    }
}

/// `text` into `path` through a temporary file renamed over it.
pub(crate) fn write_json(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The user's own: tags and ratings by file, recent files, folders added
/// as places, and how previews play.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserData {
    #[serde(default)]
    pub tags: BTreeMap<PathBuf, BTreeSet<String>>,
    #[serde(default)]
    pub ratings: BTreeMap<PathBuf, u8>,
    /// Newest first.
    #[serde(default)]
    pub recent: Vec<PathBuf>,
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    #[serde(default)]
    pub preview: super::PreviewSettings,
    /// Search the folders inside too.
    #[serde(default = "yes")]
    pub deep: bool,
}

fn yes() -> bool {
    true
}

impl Default for UserData {
    fn default() -> Self {
        Self {
            tags: BTreeMap::new(),
            ratings: BTreeMap::new(),
            recent: Vec::new(),
            folders: Vec::new(),
            preview: super::PreviewSettings::default(),
            deep: true,
        }
    }
}

impl UserData {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        write_json(
            path,
            &serde_json::to_string_pretty(self).map_err(std::io::Error::other)?,
        )
    }

    /// Every tag in use, in order.
    pub fn all_tags(&self) -> BTreeSet<String> {
        self.tags.values().flatten().cloned().collect()
    }

    pub fn tags_of(&self, path: &Path) -> impl Iterator<Item = &String> {
        self.tags.get(path).into_iter().flatten()
    }

    pub fn rating(&self, path: &Path) -> u8 {
        self.ratings.get(path).copied().unwrap_or(0)
    }

    /// `path` used (previewed, placed): first in the recent list.
    pub fn used(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn an_entry_holds_while_the_file_is_as_it_was() {
        let dir = std::env::temp_dir().join(format!("ff-browser-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("kick.wav");
        std::fs::write(&file, b"one").unwrap();
        let stamp = Stamp::read(&file).unwrap();
        let mut index = Index::fresh();
        assert_eq!(index.note(&file, stamp, FileKind::Audio), (true, true));
        let e = index.entries.get_mut(&file).unwrap();
        e.facts = Some(Facts {
            format: "WAV".into(),
            bits: Some(24),
            channels: 2,
            rate: 48_000,
            seconds: 1.5,
        });
        e.analysis = Some(Analysis {
            bpm: Some(120.0),
            key: None,
            thumb: thumb_text(&[0.0, 0.5, 1.0]),
            print: vec![1.0; 3],
            seconds: None,
        });
        assert_eq!(index.note(&file, stamp, FileKind::Audio), (false, false));
        // Saved and read back.
        let path = dir.join("index.json");
        index.save(&path).unwrap();
        let back = Index::load(&path);
        assert_eq!(back, index);
        assert!(back.current(&file, stamp).is_some());
        // The file changes: read and analysed again.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, b"longer now").unwrap();
        let changed = Stamp::read(&file).unwrap();
        assert_ne!(changed, stamp);
        assert!(back.current(&file, changed).is_none());
        let mut index = back;
        assert_eq!(index.note(&file, changed, FileKind::Audio), (true, true));
        // Another version's index: started afresh.
        std::fs::write(&path, r#"{"version":0,"entries":{}}"#).unwrap();
        assert!(Index::load(&path).entries.is_empty());
        // Thumbnails keep their levels to a 255th.
        let t = thumb_levels(&thumb_text(&[0.0, 0.25, 1.0]));
        assert!((t[1] - 0.25).abs() < 0.003 && t[2] == 1.0 && t[0] == 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_oldest_seen_go_first_and_recent_files_keep_their_order() {
        let mut index = Index::fresh();
        for i in 0..MAX_ENTRIES + 3 {
            let mut e = Entry::new(Stamp::default(), FileKind::Audio);
            e.seen = i as u64;
            index.entries.insert(PathBuf::from(format!("/s/{i}")), e);
        }
        index.prune();
        assert_eq!(index.entries.len(), MAX_ENTRIES);
        for i in 0..3 {
            assert!(!index.entries.contains_key(Path::new(&format!("/s/{i}"))));
        }
        let mut user = UserData::default();
        for p in ["/a", "/b", "/a"] {
            user.used(Path::new(p));
        }
        assert_eq!(user.recent, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}
