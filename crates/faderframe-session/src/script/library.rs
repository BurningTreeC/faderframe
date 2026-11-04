//! The scripts the session knows: the user's (`<settings>/scripts`, the
//! shell sets the folder with [`set_user_dir`]), the project's
//! (`<project>/Scripts`) and the recorded macros (`<settings>/scripts/
//! macros`). Only the `.lua` files at a folder's top are scripts; folders
//! inside hold modules for `require`.

use faderframe_script::{ScriptInfo, header};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where a script lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScriptOrigin {
    /// The user's scripts folder.
    User,
    /// The project's `Scripts` folder.
    Project,
    /// A recorded macro (the user's `macros` folder).
    Macro,
}

/// One script of the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptEntry {
    /// What names it for good: `user/<file>`, `project/<file>` or
    /// `macro/<number>` (the Scripts menu's actions and their shortcuts).
    pub key: String,
    /// Its header's name, else the file's name without `.lua`.
    pub name: String,
    pub description: Option<String>,
    pub path: PathBuf,
    pub origin: ScriptOrigin,
    /// Its number (`--! macro: n`): what MIDI controllers and surfaces
    /// run (`MappingTarget::RunMacro`).
    pub number: Option<u16>,
}

/// The project's scripts folder's name.
pub const PROJECT_FOLDER: &str = "Scripts";

static USER_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The user's scripts folder (the shell sets `<settings>/scripts`; tests
/// set their own).
pub fn set_user_dir(dir: Option<PathBuf>) {
    if let Ok(mut d) = USER_DIR.lock() {
        *d = dir;
    }
}

/// The user's scripts folder: as set, else `scripts` in the per-user
/// configuration folder (the portable folder's `Settings` in portable
/// mode).
pub fn user_dir() -> PathBuf {
    if let Some(d) = USER_DIR.lock().ok().and_then(|d| d.clone()) {
        return d;
    }
    if let Some(dir) = faderframe_core::paths::portable("Settings") {
        return dir.join("scripts");
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("APPDATA").map(PathBuf::from)
            } else if cfg!(target_os = "macos") {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join("Library/Application Support"))
            } else {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
            }
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("faderframe").join("scripts")
}

/// Where recorded macros go.
pub fn macro_dir() -> PathBuf {
    user_dir().join("macros")
}

/// Every script of the library: the user's, the project's, the macros,
/// each by name.
pub(crate) fn scan(project_dir: Option<&Path>) -> Vec<ScriptEntry> {
    let mut out = Vec::new();
    folder(&user_dir(), ScriptOrigin::User, "user", &mut out);
    if let Some(p) = project_dir {
        folder(
            &p.join(PROJECT_FOLDER),
            ScriptOrigin::Project,
            "project",
            &mut out,
        );
    }
    folder(&macro_dir(), ScriptOrigin::Macro, "macro", &mut out);
    out.sort_by(|a, b| {
        (a.origin, a.name.to_lowercase(), &a.key).cmp(&(b.origin, b.name.to_lowercase(), &b.key))
    });
    out
}

fn folder(dir: &Path, origin: ScriptOrigin, prefix: &str, out: &mut Vec<ScriptEntry>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_file() || path.extension().and_then(|x| x.to_str()) != Some("lua") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let info = header::read_header(&source);
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = file.trim_end_matches(".lua").to_string();
        // A macro is named by its number (a file renamed keeps it).
        let key = match (origin, info.macro_number) {
            (ScriptOrigin::Macro, Some(n)) => format!("{prefix}/{n}"),
            _ => format!("{prefix}/{file}"),
        };
        out.push(ScriptEntry {
            key,
            name: info.name.clone().unwrap_or(stem),
            description: info.description.clone(),
            path,
            origin,
            number: info.macro_number,
        });
    }
}

/// The number the next macro gets (one past the highest any script has).
pub(crate) fn next_number(library: &[ScriptEntry]) -> u16 {
    library
        .iter()
        .filter_map(|e| e.number)
        .max()
        .map_or(1, |n| n.saturating_add(1))
}

/// A file name for a script named `name` (no folder separators or
/// characters file systems refuse).
pub(crate) fn file_name(name: &str) -> String {
    let n: String = name
        .trim()
        .chars()
        .map(|c| {
            if "/\\:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let n = n.trim_matches('.').trim();
    if n.is_empty() {
        "Macro.lua".into()
    } else {
        format!("{n}.lua")
    }
}

/// Write a recorded macro: its header (name, number, description) and
/// `body`, in the macros folder under a name of its own.
pub(crate) fn save_macro(
    name: &str,
    number: u16,
    description: &str,
    body: &str,
) -> std::io::Result<PathBuf> {
    let dir = macro_dir();
    std::fs::create_dir_all(&dir)?;
    let mut path = dir.join(file_name(name));
    let mut n = 2;
    while path.exists() {
        path = dir.join(file_name(&format!("{name} {n}")));
        n += 1;
    }
    let info = ScriptInfo {
        name: Some(name.to_string()),
        description: Some(description.to_string()),
        macro_number: Some(number),
        other: Vec::new(),
    };
    std::fs::write(&path, format!("{}\n{body}", header::write_header(&info)))?;
    Ok(path)
}

/// Name a script anew (its header, and its file in the macros folder).
pub(crate) fn rename(entry: &ScriptEntry, name: &str) -> std::io::Result<PathBuf> {
    let source = std::fs::read_to_string(&entry.path)?;
    let mut info = header::read_header(&source);
    info.name = Some(name.trim().to_string());
    let text = header::replace_header(&source, &info);
    let mut path = entry.path.clone();
    if entry.origin == ScriptOrigin::Macro
        && let Some(dir) = entry.path.parent()
    {
        let wanted = dir.join(file_name(name));
        if wanted != entry.path && !wanted.exists() {
            path = wanted;
        }
    }
    std::fs::write(&path, text)?;
    if path != entry.path {
        std::fs::remove_file(&entry.path)?;
    }
    Ok(path)
}
