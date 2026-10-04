//! The album: songs in release order, and how they are delivered.
//!
//! A song is a section of this project, the whole project, another
//! FaderFrame project (rendered from its saved state) or a finished mix (an
//! audio file). Paths are absolute. Songs follow each other after a pause
//! or overlap in a crossfade. The release information (title, performer,
//! credits, UPC/EAN and per song ISRC) goes into the cue sheet, the CD
//! master (DDP: PQ codes and CD-Text) and the files. The album is edited
//! as a whole through [`crate::Command::SetAlbum`].

use crate::PluginSlot;
use faderframe_audio_files::{Dither, WavFormat};
use faderframe_core::{PluginInstanceId, SectionId, SongId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where a song's audio comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SongSource {
    /// A section of this project.
    Section(SectionId),
    /// This project from its start to the end of its content.
    ThisProject,
    /// Another FaderFrame project file, start to end.
    Project(PathBuf),
    /// A finished mix.
    AudioFile(PathBuf),
}

impl SongSource {
    /// A source for `path`: a project file by its extension, else audio.
    pub fn for_path(path: &Path) -> Self {
        let project = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(crate::file::FILE_EXTENSION));
        if project {
            SongSource::Project(path.to_path_buf())
        } else {
            SongSource::AudioFile(path.to_path_buf())
        }
    }
}

fn default_pause() -> f32 {
    2.0
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn is_true(v: &bool) -> bool {
    *v
}

fn yes() -> bool {
    true
}

/// Credits of the album or a song (CD-Text and file tags; empty: none).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Credits {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub performer: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub songwriter: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub composer: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub arranger: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
}

impl Credits {
    pub fn is_empty(&self) -> bool {
        *self == Credits::default()
    }
}

/// The release as a whole.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlbumInfo {
    /// Empty: the project's name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "Credits::is_empty")]
    pub credits: Credits,
    /// UPC-A or EAN-13 (the CD's media catalog number); empty: none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub upc: String,
}

impl AlbumInfo {
    pub fn is_empty(&self) -> bool {
        *self == AlbumInfo::default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Song {
    pub id: SongId,
    pub title: String,
    pub source: SongSource,
    /// Trim before the album's level processing (dB).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gain_db: f32,
    /// Silence before the song in the album (seconds; not before the
    /// first).
    #[serde(default = "default_pause")]
    pub pause: f32,
    /// Fade in and out (seconds).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_in: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_out: f32,
    /// Overlap with the previous song (seconds, equal-power): when set,
    /// the song starts this much before the previous one ends instead of
    /// after a pause.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub crossfade: f32,
    /// International Standard Recording Code; empty: none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub isrc: String,
    #[serde(default, skip_serializing_if = "Credits::is_empty")]
    pub credits: Credits,
    /// The song's own plugin chain: after its trim, before its fades and
    /// the album's level processing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inserts: Vec<PluginSlot>,
}

impl Song {
    pub fn new(id: SongId, title: impl Into<String>, source: SongSource) -> Self {
        Self {
            id,
            title: title.into(),
            source,
            gain_db: 0.0,
            pause: default_pause(),
            fade_in: 0.0,
            fade_out: 0.0,
            crossfade: 0.0,
            isrc: String::new(),
            credits: Credits::default(),
            inserts: Vec::new(),
        }
    }

    /// Does `other` sound the same (only the title or pause differ)?
    pub fn same_audio(&self, other: &Song) -> bool {
        self.source == other.source
            && self.gain_db == other.gain_db
            && self.fade_in == other.fade_in
            && self.fade_out == other.fade_out
            && self.inserts == other.inserts
    }
}

/// What the loudness target applies to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlbumLevel {
    /// One gain for every song: the album reaches the target and the songs
    /// keep their levels relative to each other.
    #[default]
    Album,
    /// Each song reaches the target on its own.
    PerSong,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlbumSettings {
    pub level: AlbumLevel,
    /// Integrated loudness target (LUFS).
    pub loudness: Option<f32>,
    /// True-peak ceiling (dBTP).
    pub ceiling: Option<f32>,
    /// Over the ceiling: limit the peaks (else use less gain).
    pub limit: bool,
    pub format: WavFormat,
    /// `None`: the project's rate.
    pub sample_rate: Option<u32>,
    pub dither: Dither,
    /// Rendered after a section or project for reverb tails (seconds).
    pub tail: f32,
    /// Folder the files go to (`None`: next to the project).
    pub output: Option<PathBuf>,
    /// Also one file of the whole album, with a CUE sheet.
    pub album_file: bool,
    /// Also a CD master: a DDP 2.00 fileset (44.1 kHz, 16-bit).
    pub ddp: bool,
    /// CD-Text on the CD master.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub cd_text: bool,
    /// The CD's tracks may be copied digitally (the DCP flag).
    pub copy_permitted: bool,
}

impl Default for AlbumSettings {
    fn default() -> Self {
        Self {
            level: AlbumLevel::Album,
            loudness: Some(-14.0),
            ceiling: Some(-1.0),
            limit: true,
            format: WavFormat::Pcm24,
            sample_rate: None,
            dither: Dither::Tpdf,
            tail: 2.0,
            output: None,
            album_file: true,
            ddp: false,
            cd_text: true,
            copy_permitted: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Album {
    pub songs: Vec<Song>,
    pub settings: AlbumSettings,
    #[serde(skip_serializing_if = "AlbumInfo::is_empty")]
    pub info: AlbumInfo,
}

impl Album {
    pub fn is_default(&self) -> bool {
        *self == Album::default()
    }

    pub fn song(&self, id: SongId) -> Option<&Song> {
        self.songs.iter().find(|s| s.id == id)
    }

    pub fn song_mut(&mut self, id: SongId) -> Option<&mut Song> {
        self.songs.iter_mut().find(|s| s.id == id)
    }

    pub fn index(&self, id: SongId) -> Option<usize> {
        self.songs.iter().position(|s| s.id == id)
    }

    /// A song's insert and the song.
    pub fn insert(&self, plugin: PluginInstanceId) -> Option<(&Song, &PluginSlot)> {
        self.songs
            .iter()
            .find_map(|s| s.inserts.iter().find(|p| p.id == plugin).map(|p| (s, p)))
    }

    pub fn insert_mut(&mut self, plugin: PluginInstanceId) -> Option<&mut PluginSlot> {
        self.songs
            .iter_mut()
            .flat_map(|s| s.inserts.iter_mut())
            .find(|p| p.id == plugin)
    }

    /// Every song's inserts.
    pub fn inserts(&self) -> impl Iterator<Item = &PluginSlot> {
        self.songs.iter().flat_map(|s| s.inserts.iter())
    }

    /// Does the album use this section?
    pub fn has_section(&self, section: SectionId) -> bool {
        self.songs
            .iter()
            .any(|s| s.source == SongSource::Section(section))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn songs_round_trip_and_default_compactly() {
        let mut album = Album::default();
        assert!(album.is_default());
        album.songs.push(Song::new(
            SongId(7),
            "Opener",
            SongSource::Section(SectionId(3)),
        ));
        album.songs.push(Song {
            gain_db: -1.5,
            ..Song::new(
                SongId(8),
                "Closer",
                SongSource::for_path(Path::new("/m/closer.flac")),
            )
        });
        let json = serde_json::to_string(&album).unwrap();
        assert!(
            !json.contains("fade_in"),
            "zero fields are left out: {json}"
        );
        let back: Album = serde_json::from_str(&json).unwrap();
        assert_eq!(back, album);
        assert!(back.has_section(SectionId(3)));
        assert_eq!(back.index(SongId(8)), Some(1));
        assert_eq!(
            SongSource::for_path(Path::new("/p/song.ffproj")),
            SongSource::Project(PathBuf::from("/p/song.ffproj"))
        );
        // Settings missing in old files take their defaults.
        let old: Album = serde_json::from_str(r#"{"songs":[]}"#).unwrap();
        assert_eq!(old.settings, AlbumSettings::default());
        assert!(old.settings.cd_text);
        // Release information round-trips and stays out when empty.
        album.info.upc = "0123456789012".into();
        album.info.credits.performer = "The Band".into();
        album.songs[0].isrc = "USABC2600001".into();
        album.songs[0].crossfade = 1.5;
        let json = serde_json::to_string(&album).unwrap();
        assert!(!json.contains("songwriter"), "{json}");
        let back: Album = serde_json::from_str(&json).unwrap();
        assert_eq!(back, album);
    }
}
