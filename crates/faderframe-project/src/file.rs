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
//!
//! Beside the project a file may carry [`FileExtras`]: where the state comes
//! from ([`crate::lineage`]) and content hashes of the media it names, which
//! a merge with a copy edited elsewhere goes by. Older readers ignore them.

use crate::Project;
use crate::lineage::Lineage;
use faderframe_workspace::WorkspaceSet;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub const FORMAT_ID: &str = "faderframe-project";
pub const CURRENT_VERSION: u32 = 3;
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
    pub extras: FileExtras,
}

/// What a file carries beside the project.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileExtras {
    /// Where this saved state comes from (none in files from before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<Lineage>,
    /// Content hashes of the media files it names, by the path as stored
    /// (`<length>:<hash>`; see `faderframe_session::version_merge`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub media: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct FileOut<'a> {
    format: &'static str,
    version: u32,
    generator: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    lineage: Option<&'a Lineage>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    media: &'a BTreeMap<String, String>,
    project: &'a Project,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<&'a WorkspaceSet>,
}

#[derive(Deserialize)]
struct FileIn {
    project: Project,
    #[serde(default)]
    workspace: Option<WorkspaceSet>,
    #[serde(default)]
    lineage: Option<Lineage>,
    #[serde(default)]
    media: BTreeMap<String, String>,
}

/// Only the stamp of a file (the rest is skipped, not built).
#[derive(Deserialize)]
struct StampIn {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    lineage: Option<Lineage>,
}

type Migration = fn(Value) -> Result<Value, FileError>;

/// `MIGRATIONS[i]` upgrades a document from version `i` to `i + 1`.
/// Version 0 never existed publicly; the entry documents the pattern.
const MIGRATIONS: &[Migration] = &[migrate_v0_to_v1, migrate_v1_to_v2, migrate_v2_to_v3];

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

/// Version 2 added live MIDI input: instrument and MIDI tracks of older
/// projects get every MIDI input and play live when armed or selected, as
/// new tracks do.
fn migrate_v1_to_v2(mut doc: Value) -> Result<Value, FileError> {
    let tracks = doc
        .get_mut("project")
        .and_then(|p| p.get_mut("tracks"))
        .and_then(Value::as_array_mut);
    for t in tracks.into_iter().flatten() {
        let Some(t) = t.as_object_mut() else { continue };
        let musical = matches!(
            t.get("kind").and_then(Value::as_str),
            Some("instrument" | "midi")
        );
        let no_input = t
            .get("input")
            .is_none_or(|i| i.get("type").and_then(Value::as_str) == Some("none"));
        if musical && no_input {
            t.insert("input".into(), serde_json::json!({ "type": "midi" }));
            if t.get("monitor").is_none_or(|m| m.as_str() == Some("off")) {
                t.insert("monitor".into(), Value::String("auto".into()));
            }
        }
    }
    Ok(doc)
}

/// Version 3 made every take of a take folder an audio clip of its own
/// (so it can carry a clip's edits): `{ name, source, source_offset (the
/// source frame at the folder's start), start, end, gain_db, rating }`
/// becomes `{ name, start, audio: { source, source_offset (at the take's
/// start), length, gain_db }, rating }`, each with an id of its own in
/// its folder (1, 2, …; the folder's `next_take` after them). Folders are
/// found wherever they are (clips, scratch pads put aside, launcher
/// slots).
fn migrate_v2_to_v3(mut doc: Value) -> Result<Value, FileError> {
    fn take(t: &mut Value, id: usize) -> Result<(), FileError> {
        let Some(t) = t.as_object_mut() else {
            return Ok(());
        };
        if t.contains_key("audio") {
            return Ok(());
        }
        t.insert("id".into(), Value::from(id));
        let int = |t: &serde_json::Map<String, Value>, k: &str| {
            t.get(k)
                .and_then(Value::as_i64)
                .ok_or_else(|| FileError::Corrupt(format!("a take without {k}")))
        };
        let (offset, start, end) = (int(t, "source_offset")?, int(t, "start")?, int(t, "end")?);
        let source = t
            .remove("source")
            .ok_or_else(|| FileError::Corrupt("a take without source".into()))?;
        let gain = t.remove("gain_db").unwrap_or(Value::from(0.0));
        t.remove("source_offset");
        t.remove("end");
        t.insert(
            "audio".into(),
            serde_json::json!({
                "source": source,
                "source_offset": offset + start,
                "length": (end - start).max(0),
                "gain_db": gain,
            }),
        );
        Ok(())
    }
    fn walk(v: &mut Value) -> Result<(), FileError> {
        match v {
            Value::Object(o) => {
                let folder = o.contains_key("comp");
                let mut count = 0;
                for (k, child) in o.iter_mut() {
                    if folder && k == "takes" {
                        for (i, t) in child.as_array_mut().into_iter().flatten().enumerate() {
                            take(t, i + 1)?;
                            count = i + 1;
                        }
                    } else {
                        walk(child)?;
                    }
                }
                if folder && !o.contains_key("next_take") {
                    o.insert("next_take".into(), Value::from(count + 1));
                }
            }
            Value::Array(a) => {
                for child in a {
                    walk(child)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    if let Some(project) = doc.get_mut("project") {
        walk(project)?;
    }
    Ok(doc)
}

/// Serialise to the current file format. The song is always written in
/// place (a scratch pad shown is put aside and named as the one to show
/// when the file is opened, see [`crate::scratch`]).
pub fn to_string(project: &Project, workspace: Option<&WorkspaceSet>) -> Result<String, FileError> {
    to_string_with(project, workspace, &FileExtras::default())
}

/// [`to_string`] with the extras a save stamps.
pub fn to_string_with(
    project: &Project,
    workspace: Option<&WorkspaceSet>,
    extras: &FileExtras,
) -> Result<String, FileError> {
    let shown = project.shown_pad();
    let mut song = project.song();
    if shown.is_some() {
        song.to_mut().scratch_pads.open = shown;
    }
    let out = FileOut {
        format: FORMAT_ID,
        version: CURRENT_VERSION,
        generator: concat!("FaderFrame ", env!("CARGO_PKG_VERSION")).to_string(),
        lineage: extras.lineage.as_ref(),
        media: &extras.media,
        project: &song,
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
        lineage,
        media,
    } = serde_json::from_value(doc)?;
    notes.extend(project.repair());
    // The scratch pad that was shown when it was saved.
    if let Some(pad) = project.scratch_pads.open.take()
        && let Err(e) = project.show_arrangement(Some(pad))
    {
        notes.push(format!("the scratch pad shown last is gone: {e}"));
    }
    if let Some(ws) = &mut workspace {
        ws.sanitise();
    }
    Ok(LoadedProject {
        project,
        workspace,
        notes,
        extras: FileExtras { lineage, media },
    })
}

/// The stamp of the project file at `path` (`None`: none, or not a
/// project file), without building the project.
pub fn read_lineage(path: &Path) -> Option<Lineage> {
    let text = std::fs::read_to_string(path).ok()?;
    let stamp: StampIn = serde_json::from_str(&text).ok()?;
    if stamp.format.as_deref() != Some(FORMAT_ID) {
        return None;
    }
    stamp.lineage
}

/// Atomically write a project file.
pub fn save(
    path: &Path,
    project: &Project,
    workspace: Option<&WorkspaceSet>,
) -> Result<(), FileError> {
    save_with(path, project, workspace, &FileExtras::default())
}

/// [`save`] with the extras a save stamps.
pub fn save_with(
    path: &Path,
    project: &Project,
    workspace: Option<&WorkspaceSet>,
    extras: &FileExtras,
) -> Result<(), FileError> {
    let text = to_string_with(project, workspace, extras)?;
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
