//! Portable mode: when a folder named [`PORTABLE_FOLDER`] sits next to the
//! program, FaderFrame keeps everything it would put into the user's
//! profile there instead — settings, caches, presets, recordings of
//! unsaved projects — and also looks for plugins in it. The folder counts
//! next to the executable, one level up (the `bin/` layout of the Windows
//! zip and the Linux tarball) or next to the macOS app bundle.
//! `FADERFRAME_DATA_DIR` names such a folder explicitly.
//!
//! Without it every crate keeps its usual per-user locations.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The folder that makes an installation portable.
pub const PORTABLE_FOLDER: &str = "FaderFrame Data";

/// The portable data folder, if this installation is portable (decided
/// once per process).
pub fn portable_root() -> Option<&'static Path> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        if let Some(dir) = std::env::var_os("FADERFRAME_DATA_DIR").filter(|d| !d.is_empty()) {
            let dir = PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&dir);
            return Some(dir);
        }
        let exe = std::env::current_exe().ok()?;
        let exe = exe.canonicalize().unwrap_or(exe);
        find_portable(&exe)
    })
    .as_deref()
}

/// The portable folder for an executable at `exe`, if there is one.
pub fn find_portable(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let mut candidates = vec![dir.join(PORTABLE_FOLDER)];
    if let Some(up) = dir.parent() {
        candidates.push(up.join(PORTABLE_FOLDER));
    }
    // <here>/FaderFrame.app/Contents/MacOS/<exe>
    if dir.ends_with("Contents/MacOS")
        && let Some(beside_app) = dir.parent().and_then(Path::parent).and_then(Path::parent)
    {
        candidates.push(beside_app.join(PORTABLE_FOLDER));
    }
    candidates.into_iter().find(|c| c.is_dir())
}

/// `<portable folder>/<sub>` in portable mode.
pub fn portable(sub: &str) -> Option<PathBuf> {
    portable_root().map(|r| r.join(sub))
}

/// Plugin folders inside the portable folder (`Plug-Ins/<format>`).
pub fn portable_plugins(format: &str) -> Option<PathBuf> {
    portable_root().map(|r| r.join("Plug-Ins").join(format))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_is_found_beside_the_program() {
        let base = std::env::temp_dir().join(format!("ff-portable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // Windows zip / Linux tarball: <root>/bin/exe, <root>/FaderFrame Data.
        let root = base.join("FaderFrame");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let exe = root.join("bin").join("faderframe");
        assert_eq!(find_portable(&exe), None, "not portable without the folder");
        std::fs::create_dir_all(root.join(PORTABLE_FOLDER)).unwrap();
        assert_eq!(find_portable(&exe), Some(root.join(PORTABLE_FOLDER)));
        // macOS: the folder next to FaderFrame.app.
        let stick = base.join("Stick");
        let macos = stick.join("FaderFrame.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::create_dir_all(stick.join(PORTABLE_FOLDER)).unwrap();
        assert_eq!(
            find_portable(&macos.join("faderframe-bin")),
            Some(stick.join(PORTABLE_FOLDER))
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
