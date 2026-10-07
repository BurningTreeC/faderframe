//! Project versions: named snapshots of a saved project, kept in its
//! `Versions` folder (`003 Before the chorus rework.ffproj`, numbered in
//! the order they were saved). A version is a project file whose media
//! paths are relative to the project folder (they share its media), so the
//! folder can move with its versions.
//!
//! Compare names what the project has changed since a version; restoring
//! one first keeps the project as it is as a version of its own ("Before
//! restoring …"), so nothing is lost, and leaves the session unsaved: the
//! project file changes when it is saved.

use crate::{NoticeLevel, Result, SessionError, media, samples};
use faderframe_project::{Project, SourceSpec, file};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The folder next to the project file that holds its versions.
pub const VERSIONS_FOLDER: &str = "Versions";

/// A saved version of the project.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectVersion {
    pub path: PathBuf,
    pub number: u32,
    pub name: String,
    /// When it was saved (the file's time).
    pub saved: Option<SystemTime>,
}

/// Its number and name, from a version's file name.
fn parse(path: &Path) -> Option<(u32, String)> {
    if path.extension().and_then(|e| e.to_str()) != Some(file::FILE_EXTENSION) {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let (n, name) = stem.split_once(' ').unwrap_or((stem, ""));
    Some((n.parse().ok()?, name.to_string()))
}

/// A name that can be part of a file name.
fn clean(name: &str) -> String {
    let c: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let c = c.trim().trim_matches('.').to_string();
    if c.is_empty() {
        "Version".into()
    } else {
        c.chars().take(80).collect()
    }
}

impl crate::Session {
    fn versions_dir(&self) -> Option<PathBuf> {
        self.project_dir().map(|d| d.join(VERSIONS_FOLDER))
    }

    /// The project's versions, oldest first (none before its first save).
    pub fn versions(&self) -> Vec<ProjectVersion> {
        let Some(dir) = self.versions_dir() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out: Vec<ProjectVersion> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let (number, name) = parse(&path)?;
                let saved = e.metadata().and_then(|m| m.modified()).ok();
                Some(ProjectVersion {
                    path,
                    number,
                    name,
                    saved,
                })
            })
            .collect();
        out.sort_by_key(|v| v.number);
        out
    }

    /// Keep the project as it is now as a version named `name`.
    pub(crate) fn save_version(&mut self, name: &str) -> Result<ProjectVersion> {
        let (Some(project_dir), Some(dir)) = (self.project_dir(), self.versions_dir()) else {
            return Err(SessionError::Other(
                "save the project first: its versions are kept next to it".into(),
            ));
        };
        std::fs::create_dir_all(&dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", dir.display())))?;
        let number = self.versions().last().map_or(1, |v| v.number + 1);
        let name = clean(name);
        let path = dir.join(format!("{number:03} {name}.{}", file::FILE_EXTENSION));
        self.capture_plugin_states();
        let mut stored = self.project.clone();
        for s in stored.sources.values_mut() {
            if let SourceSpec::File { path: p, .. } = &mut s.spec {
                *p = media::to_stored(p, Some(&project_dir));
            }
        }
        samples::map_states(&mut stored, |p| media::to_stored(p, Some(&project_dir)));
        for v in stored.video.sources.values_mut() {
            v.path = media::to_stored(&v.path, Some(&project_dir));
        }
        file::save(&path, &stored, Some(&self.workspace))?;
        self.notify(
            NoticeLevel::Info,
            format!("saved version {number} ‘{name}’"),
        );
        Ok(ProjectVersion {
            saved: std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
            path,
            number,
            name,
        })
    }

    /// A version's project, its media found in the project folder.
    fn load_version(&self, path: &Path) -> Result<Project> {
        let dir = self.project_dir();
        let loaded = file::load(path)?;
        let mut project = loaded.project;
        for s in project.sources.values_mut() {
            if let SourceSpec::File { path: p, .. } = &mut s.spec {
                *p = media::resolve(p, dir.as_deref());
            }
        }
        samples::map_states(&mut project, |p| media::resolve(p, dir.as_deref()));
        for v in project.video.sources.values_mut() {
            v.path = media::resolve(&v.path, dir.as_deref());
        }
        for t in &mut project.tracks {
            // Older versions may keep an instrument apart (see `open`).
            let _ = self.adopt_instrument(t);
        }
        Ok(project)
    }

    /// What the project has changed since the version at `path`.
    pub fn compare_version(&mut self, path: &Path) -> Result<Vec<String>> {
        let then = self.load_version(path)?;
        self.capture_plugin_states();
        Ok(faderframe_project::compare::differences(
            &then,
            &self.project,
        ))
    }

    /// Go back to the version at `path` (the project as it is now is kept
    /// as a version first).
    pub(crate) fn restore_version(&mut self, path: &Path) -> Result<()> {
        let name =
            parse(path).map_or_else(|| "a version".into(), |(n, name)| format!("{n} ‘{name}’"));
        let project = self.load_version(path)?;
        self.save_version(&format!(
            "Before restoring {}",
            parse(path).map_or(0, |v| v.0)
        ))?;
        self.replace_project(project, None)?;
        // Restored, not saved: the project file changes on saving.
        self.saved_revision = u64::MAX;
        self.notify(NoticeLevel::Info, format!("restored version {name}"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_files_carry_their_number_and_name() {
        assert_eq!(
            parse(Path::new("/p/Versions/003 Before the chorus.ffproj")),
            Some((3, "Before the chorus".into()))
        );
        assert_eq!(parse(Path::new("/p/Versions/notes.txt")), None);
        assert_eq!(clean(" a/b: c? "), "a-b- c-");
        assert_eq!(clean("   "), "Version");
    }
}
