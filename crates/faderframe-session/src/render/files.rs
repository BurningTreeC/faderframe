//! Writing render and export destinations safely. Every file a render or
//! an export writes goes to a temporary name in its destination's folder
//! first ([`StagedFile`]; a folder of files — a DDP fileset, a vinyl
//! premaster — to a temporary folder, [`StagedFile::new_dir`]) and moves
//! into place only once the whole job — every file of it — has been
//! written and measured: a rename on the same file system, so a file that
//! was there before is replaced in one step or not at all, never left
//! half written. A cancelled or failed job removes its temporary files
//! and leaves what was there untouched.
//!
//! Whether a destination that exists may be replaced is the job's
//! [`Overwrite`]; it is checked again right before the files move (a file
//! may have appeared since the job was started). One that may not be
//! replaced stops the job with its files kept aside
//! ([`RenderError::Exists`] holding a [`Staged`]): ask, then
//! [`Staged::commit`] or [`Staged::discard`].
//!
//! Moving without replacing uses a hard link (fails when the name is
//! taken, atomically, on POSIX and NTFS); where the file system has none
//! (FAT, some network shares) the check and the rename are as close
//! together as they get. Replacing is `std::fs::rename`: POSIX `rename`,
//! on Windows a rename that replaces an existing file (it fails, leaving
//! the old file, while another program holds that file open). A folder
//! (no rename replaces a folder that holds files) is swapped: the old one
//! renamed aside under a temporary name, the new one renamed into place,
//! the old one removed — or renamed back when the new one cannot move.
//!
//! Temporary names are `.<name>.ffrender-<pid>-<n>[.<ext>]`
//! ([`is_temporary`]). A program killed while rendering leaves them
//! behind; staging into a folder sweeps the ones of processes that no
//! longer run ([`sweep_stale`]).

use super::{RenderError, Rendered};
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// What a render may do with destinations that exist when it is done.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Overwrite {
    /// Replace none: an existing destination stops the render before
    /// anything moves ([`RenderError::Exists`]).
    #[default]
    Never,
    /// Replace these (the user agreed); any other that exists stops the
    /// render as with `Never`.
    Confirmed(Vec<PathBuf>),
    /// Replace any: files the session names and owns (freezes, bounces,
    /// clip effects, scratch renders), never a destination the user chose.
    Always,
}

impl Overwrite {
    /// May the file at `path` be replaced?
    pub fn allows(&self, path: &Path) -> bool {
        match self {
            Overwrite::Never => false,
            Overwrite::Confirmed(paths) => paths.iter().any(|p| p == path),
            Overwrite::Always => true,
        }
    }
}

/// Is anything at `path` (a file, a folder, a link — even a dangling one)?
pub fn occupied(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// The ones of `paths` that something is at now.
pub fn existing(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().filter(|p| occupied(p)).cloned().collect()
}

/// The first of `files` (file destinations) a folder is at: no file
/// replaces a folder, so a job writing there is refused before it starts.
pub fn folder_in_the_way(files: &[PathBuf]) -> Option<PathBuf> {
    files
        .iter()
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_dir()))
        .cloned()
}

/// Is `path` one of the temporary files renders write (left behind only
/// when the program was killed while rendering)?
pub fn is_temporary(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.') && n.contains(TEMP_MARK))
}

/// The process that made the temporary `name` (its pid).
fn temporary_owner(name: &str) -> Option<u32> {
    let (_, rest) = name.rsplit_once(TEMP_MARK)?;
    rest.split('-').next()?.parse().ok()
}

const TEMP_MARK: &str = ".ffrender-";

/// A temporary of a process that is gone, untouched this long, is swept
/// (where the system says the process is gone: Linux). The wait covers a
/// process the system cannot see — another PID namespace, a sandboxed
/// copy of the program — whose render waits for an answer.
pub const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

/// Where the system cannot say whether a process runs, a temporary is
/// swept once it is this old.
pub const STALE_UNKNOWN_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Does process `pid` run? `None`: the system does not say.
fn process_runs(pid: u32) -> Option<bool> {
    if cfg!(target_os = "linux") {
        Some(Path::new(&format!("/proc/{pid}")).exists())
    } else {
        None
    }
}

/// Remove what is at `path` (a folder with what it holds).
fn remove_any(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Remove the render temporaries in `dir` that processes left behind
/// when they were killed: never this process's, never one touched in the
/// last [`STALE_AFTER`]; of a process the system says is gone, or (where
/// it cannot say) once [`STALE_UNKNOWN_AFTER`] old. Returns how many went.
pub fn sweep_stale(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let me = std::process::id();
    let mut removed = 0;
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with('.') || !name.contains(TEMP_MARK) {
            continue;
        }
        let Some(pid) = temporary_owner(name) else {
            continue;
        };
        if pid == me {
            continue;
        }
        let age = std::fs::symlink_metadata(e.path())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok());
        let Some(age) = age else { continue };
        let stale = match process_runs(pid) {
            Some(true) => false,
            Some(false) => age >= STALE_AFTER,
            None => age >= STALE_UNKNOWN_AFTER,
        };
        if stale && remove_any(&e.path()).is_ok() {
            tracing::info!("removed a stale render temporary: {}", e.path().display());
            removed += 1;
        }
    }
    removed
}

/// A free temporary name beside `dest` (`dir`: for a folder — the whole
/// name kept, no extension split off), created by `make` (fails with
/// `AlreadyExists` when taken).
fn claim_beside(
    dest: &Path,
    dir: bool,
    make: impl Fn(&Path) -> std::io::Result<()>,
) -> Result<PathBuf, RenderError> {
    let parent = match dest.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    // What fails here is the folder (read-only, gone, full), not the file.
    let io = |source| RenderError::Folder {
        path: parent.clone(),
        source,
    };
    let (stem, ext) = if dir {
        (
            dest.file_name()
                .map_or_else(|| "export".into(), |s| s.to_string_lossy().into_owned()),
            String::new(),
        )
    } else {
        (
            dest.file_stem()
                .map_or_else(|| "render".into(), |s| s.to_string_lossy().into_owned()),
            dest.extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default(),
        )
    };
    for _ in 0..1000 {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".{stem}{TEMP_MARK}{}-{n}{ext}", std::process::id()));
        match make(&temp) {
            Ok(()) => return Ok(temp),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(e)),
        }
    }
    Err(io(std::io::Error::new(
        ErrorKind::AlreadyExists,
        "no free temporary name beside it",
    )))
}

/// One file (or folder of files) of a render or an export, written under
/// a temporary name beside its destination. Dropped before it was placed,
/// the temporary goes.
#[derive(Debug)]
pub struct StagedFile {
    temp: PathBuf,
    dest: PathBuf,
    /// A folder of files (moved as a whole).
    dir: bool,
    placed: bool,
}

impl StagedFile {
    /// Claim a temporary name beside `dest`: created (empty) now, so a
    /// folder that cannot be written to fails before anything is
    /// rendered. It keeps the destination's extension (writers choose
    /// their container by it). Stale temporaries of killed processes in
    /// that folder go first ([`sweep_stale`]).
    pub(crate) fn new(dest: &Path) -> Result<Self, RenderError> {
        Self::claim(dest, false)
    }

    /// Claim a temporary folder beside `dest` (a folder of files the job
    /// fills, e.g. a DDP fileset): it replaces `dest` as a whole.
    pub(crate) fn new_dir(dest: &Path) -> Result<Self, RenderError> {
        Self::claim(dest, true)
    }

    fn claim(dest: &Path, dir: bool) -> Result<Self, RenderError> {
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
            sweep_stale(parent);
        }
        let temp = claim_beside(dest, dir, |temp| {
            if dir {
                std::fs::create_dir(temp)
            } else {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(temp)
                    .map(drop)
            }
        })?;
        Ok(Self {
            temp,
            dest: dest.to_path_buf(),
            dir,
            placed: false,
        })
    }

    /// Where the job writes it.
    pub fn temp(&self) -> &Path {
        &self.temp
    }

    /// Where it goes.
    pub fn dest(&self) -> &Path {
        &self.dest
    }

    /// Move it into place, replacing what is there only when `replace`.
    /// `Ok(false)`: something is there and stays (nothing moved).
    fn place(&mut self, replace: bool) -> Result<bool, RenderError> {
        let io = |source| RenderError::Io {
            path: self.dest.clone(),
            source,
        };
        if self.dir {
            if occupied(&self.dest) {
                if !replace {
                    return Ok(false);
                }
                self.swap_in()?;
            } else {
                std::fs::rename(&self.temp, &self.dest).map_err(io)?;
            }
        } else if replace {
            std::fs::rename(&self.temp, &self.dest).map_err(io)?;
        } else {
            match std::fs::hard_link(&self.temp, &self.dest) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&self.temp);
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => return Ok(false),
                // No hard links on this file system: check, then rename.
                Err(_) => {
                    if occupied(&self.dest) {
                        return Ok(false);
                    }
                    std::fs::rename(&self.temp, &self.dest).map_err(io)?;
                }
            }
        }
        self.placed = true;
        Ok(true)
    }

    /// Replace what is at the destination by this folder: the old one
    /// aside, this one in, the old one removed (back again when this one
    /// cannot move).
    fn swap_in(&self) -> Result<(), RenderError> {
        let io = |source| RenderError::Io {
            path: self.dest.clone(),
            source,
        };
        // A name beside it, not created: the old one moves there.
        let aside = claim_beside(&self.dest, true, |p| {
            if occupied(p) {
                Err(std::io::Error::from(ErrorKind::AlreadyExists))
            } else {
                Ok(())
            }
        })?;
        std::fs::rename(&self.dest, &aside).map_err(io)?;
        if let Err(e) = std::fs::rename(&self.temp, &self.dest) {
            if let Err(back) = std::fs::rename(&aside, &self.dest) {
                tracing::warn!(
                    "{} could not move back from {}: {back}",
                    self.dest.display(),
                    aside.display()
                );
            }
            return Err(io(e));
        }
        if let Err(e) = remove_any(&aside) {
            tracing::warn!("the replaced {}: {e}", aside.display());
        }
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.placed {
            let _ = remove_any(&self.temp);
        }
    }
}

/// `bytes` written to a temporary file beside `dest`, to be placed.
pub(crate) fn stage_bytes(dest: &Path, bytes: &[u8]) -> Result<StagedFile, RenderError> {
    let file = StagedFile::new(dest)?;
    std::fs::write(file.temp(), bytes).map_err(|source| RenderError::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    Ok(file)
}

/// A file written (not audio: no statistics).
pub(crate) fn written(file: StagedFile) -> (StagedFile, Rendered) {
    let path = file.dest().to_path_buf();
    (
        file,
        Rendered {
            path,
            finished: None,
            stats: None,
        },
    )
}

/// A finished render's files: written and measured, not yet in place.
#[derive(Debug)]
pub struct Staged {
    files: Vec<(StagedFile, Rendered)>,
    /// Already in place (a commit that stopped part-way).
    placed: Vec<Rendered>,
}

impl Staged {
    pub(crate) fn new(files: Vec<(StagedFile, Rendered)>) -> Self {
        Self {
            files,
            placed: Vec::new(),
        }
    }

    /// Every destination not yet in place.
    pub fn destinations(&self) -> Vec<PathBuf> {
        self.files.iter().map(|(f, _)| f.dest.clone()).collect()
    }

    /// The destinations something is at now.
    pub fn conflicts(&self) -> Vec<PathBuf> {
        existing(&self.destinations())
    }

    /// Move every file into place as `overwrite` allows. A destination it
    /// does not allow replacing that is taken (now, or the moment before
    /// the move) moves nothing more and returns the rest as
    /// [`RenderError::Exists`]; an I/O error (permissions, a folder in the
    /// way, a file held open on Windows) leaves that destination as it was
    /// and drops the files not yet moved.
    pub fn commit(mut self, overwrite: &Overwrite) -> Result<Vec<Rendered>, RenderError> {
        if self
            .files
            .iter()
            .any(|(f, _)| occupied(&f.dest) && !overwrite.allows(&f.dest))
        {
            return Err(RenderError::Exists(Box::new(self)));
        }
        self.files.reverse();
        while let Some((mut file, rendered)) = self.files.pop() {
            match file.place(overwrite.allows(&file.dest)) {
                Ok(true) => self.placed.push(rendered),
                Ok(false) => {
                    self.files.push((file, rendered));
                    self.files.reverse();
                    return Err(RenderError::Exists(Box::new(self)));
                }
                Err(e) => {
                    if !self.placed.is_empty() {
                        tracing::warn!(
                            "render: {} of the files were in place when this one failed",
                            self.placed.len()
                        );
                    }
                    return Err(e);
                }
            }
        }
        Ok(std::mem::take(&mut self.placed))
    }

    /// Throw the rendered files away; the destinations stay as they are.
    pub fn discard(self) {}
}

/// "Song.wav already exists" / "3 files already exist: a, b, c".
pub(crate) fn exists_message(staged: &Staged) -> String {
    let names: Vec<String> = staged
        .conflicts()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    match names.len() {
        0 => "the destination was taken while rendering".into(),
        1 => format!("{} already exists", names[0]),
        n => format!("{n} files already exist: {}", names.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ff-render-files-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rendered(path: &Path) -> Rendered {
        Rendered {
            path: path.to_path_buf(),
            finished: None,
            stats: None,
        }
    }

    fn staged(dest: &Path, body: &str) -> (StagedFile, Rendered) {
        let f = StagedFile::new(dest).unwrap();
        std::fs::write(f.temp(), body).unwrap();
        (f, rendered(dest))
    }

    fn entries(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_new_file_moves_into_place_and_no_temporary_stays() {
        let d = dir("new");
        let dest = d.join("Mix.wav");
        let (f, r) = staged(&dest, "render");
        assert!(is_temporary(f.temp()));
        assert_eq!(f.temp().extension().unwrap(), "wav", "the extension kept");
        let done = Staged::new(vec![(f, r)]).commit(&Overwrite::Never).unwrap();
        assert_eq!(done[0].path, dest);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "render");
        assert_eq!(entries(&d), ["Mix.wav"]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_existing_file_is_replaced_only_when_allowed() {
        let d = dir("existing");
        let dest = d.join("Mix.wav");
        std::fs::write(&dest, "old").unwrap();
        let s = Staged::new(vec![staged(&dest, "new")]);
        let Err(RenderError::Exists(s)) = s.commit(&Overwrite::Never) else {
            panic!("replaced without consent");
        };
        assert_eq!(s.conflicts(), std::slice::from_ref(&dest));
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");
        // Consent for another file is no consent for this one.
        let Err(RenderError::Exists(s)) =
            s.commit(&Overwrite::Confirmed(vec![d.join("Other.wav")]))
        else {
            panic!("replaced without consent");
        };
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");
        s.commit(&Overwrite::Confirmed(vec![dest.clone()])).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
        assert_eq!(entries(&d), ["Mix.wav"]);
        // Discarded: the old file stays, the temporary goes.
        let s = Staged::new(vec![staged(&dest, "newer")]);
        s.discard();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
        assert_eq!(entries(&d), ["Mix.wav"]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn several_files_move_only_when_none_is_in_the_way() {
        let d = dir("several");
        let (a, b) = (d.join("A.wav"), d.join("B.wav"));
        std::fs::write(&b, "old b").unwrap();
        let s = Staged::new(vec![staged(&a, "a"), staged(&b, "b")]);
        let Err(RenderError::Exists(s)) = s.commit(&Overwrite::Never) else {
            panic!("B replaced");
        };
        assert!(!a.exists(), "nothing moves while one is in the way");
        assert_eq!(s.destinations(), [a.clone(), b.clone()]);
        let done = s.commit(&Overwrite::Confirmed(vec![b.clone()])).unwrap();
        assert_eq!(done.len(), 2);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b");
        assert_eq!(entries(&d), ["A.wav", "B.wav"]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_folder_in_the_way_is_an_error_and_stays() {
        let d = dir("folder");
        let dest = d.join("Mix.wav");
        std::fs::create_dir_all(dest.join("inside")).unwrap();
        let s = Staged::new(vec![staged(&dest, "new")]);
        assert!(matches!(
            s.commit(&Overwrite::Always),
            Err(RenderError::Io { .. })
        ));
        assert!(dest.join("inside").is_dir());
        assert_eq!(entries(&d), ["Mix.wav"], "the temporary file went");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_written_fails_before_rendering() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("readonly");
        let dest = d.join("Mix.wav");
        std::fs::write(&dest, "old").unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();
        let claimed = StagedFile::new(&dest);
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        // (Root may write anywhere: then there is nothing to check.)
        if let Err(e) = claimed {
            assert!(matches!(e, RenderError::Folder { .. }), "{e}");
            assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_folder_moves_into_place_and_replaces_one_only_when_allowed() {
        let d = dir("folders");
        let dest = d.join("Album (DDP)");
        let fill = |f: &StagedFile, name: &str, body: &str| {
            assert!(f.temp().is_dir());
            std::fs::write(f.temp().join(name), body).unwrap();
        };
        let f = StagedFile::new_dir(&dest).unwrap();
        assert!(is_temporary(f.temp()));
        fill(&f, "IMAGE.DAT", "one");
        Staged::new(vec![written(f)])
            .commit(&Overwrite::Never)
            .unwrap();
        assert_eq!(entries(&dest), ["IMAGE.DAT"]);
        assert_eq!(entries(&d), ["Album (DDP)"]);
        // Another fileset: refused, then replaced as a whole (the old
        // fileset's other files go with it).
        std::fs::write(dest.join("OLD.TXT"), "stale").unwrap();
        let f = StagedFile::new_dir(&dest).unwrap();
        fill(&f, "IMAGE.DAT", "two");
        fill(&f, "DDPID", "id");
        let Err(RenderError::Exists(s)) = Staged::new(vec![written(f)]).commit(&Overwrite::Never)
        else {
            panic!("replaced without consent");
        };
        assert_eq!(s.conflicts(), std::slice::from_ref(&dest));
        assert_eq!(entries(&dest), ["IMAGE.DAT", "OLD.TXT"]);
        s.commit(&Overwrite::Confirmed(vec![dest.clone()])).unwrap();
        assert_eq!(entries(&dest), ["DDPID", "IMAGE.DAT"]);
        assert_eq!(
            std::fs::read_to_string(dest.join("IMAGE.DAT")).unwrap(),
            "two"
        );
        assert_eq!(entries(&d), ["Album (DDP)"], "nothing aside stays");
        // Thrown away: the temporary folder goes with what it holds.
        let f = StagedFile::new_dir(&dest).unwrap();
        fill(&f, "IMAGE.DAT", "three");
        Staged::new(vec![written(f)]).discard();
        assert_eq!(entries(&d), ["Album (DDP)"]);
        assert_eq!(
            std::fs::read_to_string(dest.join("IMAGE.DAT")).unwrap(),
            "two"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn staging_sweeps_the_temporaries_of_processes_that_are_gone() {
        let d = dir("sweep");
        let aged = |p: &Path, by: Duration| {
            let t = std::time::SystemTime::now() - by;
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        };
        let hour = Duration::from_secs(3600);
        // pid_max is at most 2^22 on Linux: this process never ran.
        let gone = d.join(".Mix.ffrender-4000000000-7.wav");
        let gone_fresh = d.join(".Mix.ffrender-4000000001-0.wav");
        let mine = d.join(format!(".Mix.ffrender-{}-99999.wav", std::process::id()));
        let not_ours = d.join(".notes.txt");
        for p in [&gone, &gone_fresh, &mine, &not_ours] {
            std::fs::write(p, "x").unwrap();
        }
        aged(&gone, 2 * hour);
        aged(&mine, 48 * hour);
        aged(&not_ours, 48 * hour);
        // A folder left by a killed DDP export, a day and a half old.
        let gone_dir = d.join(".Album (DDP).ffrender-4000000002-3");
        std::fs::create_dir(&gone_dir).unwrap();
        std::fs::write(gone_dir.join("IMAGE.DAT"), "x").unwrap();
        std::fs::File::open(&gone_dir)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - 36 * hour)
            .unwrap();
        let f = StagedFile::new(&d.join("Mix.wav")).unwrap();
        let left = entries(&d);
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        // This process's own (it may be writing it) and others' files stay.
        assert!(left.contains(&name(&mine)), "{left:?}");
        assert!(left.contains(&name(&not_ours)), "{left:?}");
        if cfg!(target_os = "linux") {
            // Gone and untouched for an hour: swept; a fresh one waits.
            assert!(!left.contains(&name(&gone)), "{left:?}");
            assert!(left.contains(&name(&gone_fresh)), "{left:?}");
        }
        // A day and a half: swept wherever the system cannot tell.
        assert!(!left.contains(&name(&gone_dir)), "{left:?}");
        assert_eq!(temporary_owner(&name(&gone_fresh)), Some(4_000_000_001));
        drop(f);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
