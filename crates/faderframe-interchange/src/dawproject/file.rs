//! The `.dawproject` container: a zip of `project.xml`, `metadata.xml`
//! and the files they refer to (media, plug-in states), in whatever
//! folders the writer chose.

use super::{MetaData, Project, read_metadata, read_project, write_metadata, write_project};
use crate::zip::{ZipReader, ZipWriter};
use crate::{InterchangeError, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const PROJECT_XML: &str = "project.xml";
pub const METADATA_XML: &str = "metadata.xml";
/// The file extension.
pub const EXTENSION: &str = "dawproject";

/// A file put into the container besides the XML.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    Bytes(Vec<u8>),
    /// Copied from disk (stored, not compressed: media is).
    File(PathBuf),
}

impl Payload {
    fn len(&self) -> u64 {
        match self {
            Payload::Bytes(b) => b.len() as u64,
            Payload::File(p) => std::fs::metadata(p).map_or(0, |m| m.len()),
        }
    }
}

/// Write a `.dawproject` at `path`: the XML first, then `files` (by the
/// paths the XML names them with). `progress` hears bytes written of the
/// whole; `cancel` stops between files (the file is then incomplete: the
/// caller writes to a temporary and throws it away).
pub fn write(
    path: &Path,
    project: &Project,
    metadata: &MetaData,
    files: &[(String, Payload)],
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let mut zip = ZipWriter::create(path)?;
    let xml = write_project(project);
    let meta = write_metadata(metadata);
    let total: u64 =
        xml.len() as u64 + meta.len() as u64 + files.iter().map(|(_, p)| p.len()).sum::<u64>();
    let mut done = 0;
    zip.add(PROJECT_XML, xml.as_bytes(), true)?;
    zip.add(METADATA_XML, meta.as_bytes(), true)?;
    done += (xml.len() + meta.len()) as u64;
    progress(done, total);
    for (name, payload) in files {
        if cancel.load(Ordering::Relaxed) {
            return Err(InterchangeError::Format("cancelled".into()));
        }
        match payload {
            Payload::Bytes(b) => zip.add(name, b, true)?,
            Payload::File(p) => zip.add_file(name, p)?,
        }
        done += payload.len();
        progress(done, total);
    }
    zip.finish()?;
    Ok(())
}

/// An open `.dawproject`: its documents read, its files there to read.
pub struct DawProjectFile {
    pub project: Project,
    pub metadata: MetaData,
    /// What the reading left out.
    pub notes: Vec<String>,
    path: PathBuf,
    archive: ZipReader<std::fs::File>,
}

impl DawProjectFile {
    pub fn open(path: &Path) -> Result<Self> {
        let mut archive = ZipReader::open(path)?;
        let xml = archive.read(PROJECT_XML).map_err(|_| {
            InterchangeError::Format(format!(
                "{}: no {PROJECT_XML} (not a DAWproject file)",
                path.display()
            ))
        })?;
        let xml = String::from_utf8_lossy(&xml);
        let (project, mut notes) = read_project(&xml)?;
        let metadata = match archive.read(METADATA_XML) {
            Ok(m) => read_metadata(&String::from_utf8_lossy(&m)).unwrap_or_else(|e| {
                notes.push(format!("{METADATA_XML}: {e}"));
                MetaData::default()
            }),
            Err(_) => MetaData::default(),
        };
        Ok(Self {
            project,
            metadata,
            notes,
            path: path.to_path_buf(),
            archive,
        })
    }

    /// Is there a file `name` in the container?
    pub fn contains(&self, name: &str) -> bool {
        self.archive.find(name).is_some()
    }

    pub fn names(&self) -> Vec<String> {
        self.archive
            .entries()
            .iter()
            .filter(|e| !e.is_dir())
            .map(|e| e.name.clone())
            .collect()
    }

    /// A file from the container.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        self.archive.read(name)
    }

    /// Copy a file of the container to `to`.
    pub fn extract(&mut self, name: &str, to: &Path) -> Result<u64> {
        let mut f = std::fs::File::create(to).map_err(|e| InterchangeError::io(to.display(), e))?;
        self.archive.copy_to(name, &mut f)
    }

    /// Where a file reference marked external points: relative to the
    /// `.dawproject`'s folder, or absolute.
    pub fn external(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        let windows_absolute = path.len() > 2
            && path.as_bytes()[1] == b':'
            && path.as_bytes()[0].is_ascii_alphabetic();
        if p.is_absolute() || windows_absolute {
            p.to_path_buf()
        } else {
            self.path
                .parent()
                .map_or_else(|| p.to_path_buf(), |d| d.join(p))
        }
    }
}
