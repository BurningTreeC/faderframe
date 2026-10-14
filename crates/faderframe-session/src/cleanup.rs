//! Unused media: audio files in the project's `Audio` folder that neither
//! the saved project nor any of its saved versions uses. While a session
//! is open its undo history may still need a file a clip no longer plays
//! (a deleted clip comes back on Undo); once it closes, only the saved
//! files speak for them. A source a saved file lists counts as used when
//! something there plays it or keeps it to go back to
//! ([`faderframe_project::Project::used_sources`]); besides, a file whose
//! name appears anywhere else in a saved file (an album's song files) is
//! kept, subfolders (the samplers' `Samples`) are left alone, and nothing
//! is found when a saved file cannot be read.

use crate::{MEDIA_FOLDER, Session};
use faderframe_project::SourceSpec;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Extensions of the audio files looked at.
const AUDIO: [&str; 9] = [
    "wav", "wave", "flac", "aif", "aiff", "mp3", "ogg", "opus", "m4a",
];

/// Files no saved state of the project uses, with their peaks files.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnusedMedia {
    pub files: Vec<PathBuf>,
    /// Their size together.
    pub bytes: u64,
}

impl UnusedMedia {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

fn same(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl Session {
    /// The audio files in the saved project's `Audio` folder that its
    /// saved file and versions do not use (see the module documentation);
    /// `None` for a project never saved or when a saved file cannot be
    /// read.
    pub fn unused_media(&self) -> Option<UnusedMedia> {
        let path = self.path()?;
        let dir = path.parent()?;
        let media = dir.join(MEDIA_FOLDER);
        let mut saved = vec![path.to_path_buf()];
        saved.extend(self.versions().into_iter().map(|v| v.path));
        let mut used: HashSet<PathBuf> = HashSet::new();
        // Each saved file as text without the sources nothing uses: a file
        // named there is named by something else.
        let mut texts = Vec::with_capacity(saved.len());
        for f in &saved {
            let text = std::fs::read_to_string(f).ok()?;
            let mut loaded = faderframe_project::file::from_str(&text).ok()?;
            let p = &mut loaded.project;
            let keep = p.used_sources();
            p.sources.retain(|id, _| keep.contains(id));
            for s in p.sources.values() {
                if let SourceSpec::File { path, .. } = &s.spec {
                    // Relative paths are from the project's folder (versions
                    // too).
                    let p = if path.is_absolute() {
                        path.clone()
                    } else {
                        dir.join(path)
                    };
                    used.insert(same(&p));
                }
            }
            texts.push(faderframe_project::file::to_string(p, None).ok()?);
        }
        let mut out = UnusedMedia::default();
        let entries = std::fs::read_dir(&media).ok()?;
        let mut candidates: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| AUDIO.contains(&e.to_ascii_lowercase().as_str()))
            })
            .collect();
        candidates.sort();
        for file in candidates {
            if used.contains(&same(&file)) {
                continue;
            }
            let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // Named anywhere (a field this does not read): kept.
            if texts.iter().any(|t| t.contains(name)) {
                continue;
            }
            out.bytes += std::fs::metadata(&file).map_or(0, |m| m.len());
            let peaks = faderframe_audio_files::import::peaks_path_for(&file);
            out.files.push(file);
            if peaks.is_file() {
                out.bytes += std::fs::metadata(&peaks).map_or(0, |m| m.len());
                out.files.push(peaks);
            }
        }
        Some(out)
    }
}
