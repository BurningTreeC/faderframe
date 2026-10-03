//! Versioned project file format.
//!
//! A project file is UTF-8 JSON:
//!
//! ```json
//! { "format": "faderframe-project", "version": 1, "generator": "faderframe 0.1.0",
//!   "project": { ... }, "workspace": { ... } }
//! ```
//!
//! Loading parses into an untyped JSON value first, upgrades it through the
//! migration chain (`MIGRATIONS[v]` turns version `v` into `v + 1`), and only
//! then deserialises into the current types. Files from newer versions are
//! rejected rather than misread. Saving writes to a temporary file in the
//! same directory and renames it over the target, so a crash never leaves a
//! truncated project behind.

use crate::Project;
use faderframe_workspace::WorkspaceSet;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

pub const FORMAT_ID: &str = "faderframe-project";
pub const CURRENT_VERSION: u32 = 1;
pub const FILE_EXTENSION: &str = "ffproj";

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("cannot read or write project file: {0}")]
    Io(#[from] std::io::Error),
    #[error("project file is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not a FaderFrame project file")]
    WrongFormat,
    #[error(
        "project file version {found} is newer than this FaderFrame (supports up to {supported})"
    )]
    TooNew { found: u32, supported: u32 },
    #[error("project file is corrupted: {0}")]
    Corrupt(String),
}

/// A loaded project plus anything the loader had to repair.
#[derive(Debug)]
pub struct LoadedProject {
    pub project: Project,
    pub workspace: Option<WorkspaceSet>,
    pub notes: Vec<String>,
}

#[derive(Serialize)]
struct FileOut<'a> {
    format: &'static str,
    version: u32,
    generator: String,
    project: &'a Project,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<&'a WorkspaceSet>,
}

#[derive(Deserialize)]
struct FileIn {
    project: Project,
    #[serde(default)]
    workspace: Option<WorkspaceSet>,
}

type Migration = fn(Value) -> Result<Value, FileError>;

/// `MIGRATIONS[i]` upgrades a document from version `i` to `i + 1`.
/// Version 0 never existed publicly; the entry documents the pattern.
const MIGRATIONS: &[Migration] = &[migrate_v0_to_v1];

fn migrate_v0_to_v1(mut doc: Value) -> Result<Value, FileError> {
    // Pre-release drafts stored the track list as "channels".
    if let Some(project) = doc.get_mut("project").and_then(Value::as_object_mut)
        && !project.contains_key("tracks")
        && let Some(channels) = project.remove("channels")
    {
        project.insert("tracks".into(), channels);
    }
    Ok(doc)
}

/// Serialise to the current file format.
pub fn to_string(project: &Project, workspace: Option<&WorkspaceSet>) -> Result<String, FileError> {
    let out = FileOut {
        format: FORMAT_ID,
        version: CURRENT_VERSION,
        generator: concat!("FaderFrame ", env!("CARGO_PKG_VERSION")).to_string(),
        project,
        workspace,
    };
    Ok(serde_json::to_string_pretty(&out)?)
}

/// Parse, migrate and repair a project document.
pub fn from_str(text: &str) -> Result<LoadedProject, FileError> {
    let mut doc: Value = serde_json::from_str(text)?;
    if doc.get("format").and_then(Value::as_str) != Some(FORMAT_ID) {
        return Err(FileError::WrongFormat);
    }
    let version = doc
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| FileError::Corrupt("missing version".into()))? as u32;
    if version > CURRENT_VERSION {
        return Err(FileError::TooNew {
            found: version,
            supported: CURRENT_VERSION,
        });
    }
    let mut notes = Vec::new();
    for v in version..CURRENT_VERSION {
        let step = MIGRATIONS
            .get(v as usize)
            .ok_or_else(|| FileError::Corrupt(format!("no migration from version {v}")))?;
        doc = step(doc)?;
        notes.push(format!(
            "upgraded project file from version {v} to {}",
            v + 1
        ));
    }
    let FileIn {
        mut project,
        mut workspace,
    } = serde_json::from_value(doc)?;
    notes.extend(project.repair());
    if let Some(ws) = &mut workspace {
        ws.sanitise();
    }
    Ok(LoadedProject {
        project,
        workspace,
        notes,
    })
}

/// Atomically write a project file.
pub fn save(
    path: &Path,
    project: &Project,
    workspace: Option<&WorkspaceSet>,
) -> Result<(), FileError> {
    let text = to_string(project, workspace)?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| FileError::Io(std::io::Error::other("project path has no file name")))?;
    let tmp = dir.join(format!(".{}.tmp", file_name.to_string_lossy()));
    std::fs::write(&tmp, text.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

pub fn load(path: &Path) -> Result<LoadedProject, FileError> {
    from_str(&std::fs::read_to_string(path)?)
}
