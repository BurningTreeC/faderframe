//! The album: songs in release order, and how they are delivered.
//!
//! A song is a section of this project, the whole project, another
//! FaderFrame project (rendered from its saved state) or a finished mix (an
//! audio file). Paths are absolute. The album is edited as a whole through
//! [`crate::Command::SetAlbum`].

use faderframe_audio_files::{Dither, WavFormat};
use faderframe_core::{SectionId, SongId};
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
        }
    }

    /// Does `other` sound the same (only the title or pause differ)?
    pub fn same_audio(&self, other: &Song) -> bool {
        self.source == other.source
            && self.gain_db == other.gain_db
            && self.fade_in == other.fade_in
            && self.fade_out == other.fade_out
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
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Album {
    pub songs: Vec<Song>,
    pub settings: AlbumSettings,
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
    }
}
