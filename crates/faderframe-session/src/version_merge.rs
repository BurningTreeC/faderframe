//! Merging versions: another version of the project (one of its own, or a
//! copy someone else worked on) joined into the project as it is, by the
//! pure three-way merge [`faderframe_project::merge`].
//!
//! **Where states come from.** Every save stamps the file with a
//! [`Lineage`] ([`faderframe_project::lineage`]): the project file, each
//! version, and a hidden copy of each saved state in `Versions/.bases`
//! (the newest [`KEPT_BASES`], hard links where the file system has them —
//! saving writes a new file, so the link keeps the old one). The base of a
//! merge is the newest state both sides descend from whose content is here
//! (a base copy, a version or the project file); the point the two lines
//! branched tells which of the other side's ids are its own. A file from
//! someone else is kept as a base copy too once merged (its ids as they
//! are in the merged project), so their next file merges from it.
//!
//! **Media.** Every save also stamps content hashes of the media it names
//! ([`content_hash`]: the length and SHA-256 of the file, of 64 KiB blocks
//! spread over it past 4 MiB — at most 4 MiB read a file, once a session).
//! A copy from another folder names its media
//! where they were there: each file is found here by its hash (the
//! project's media and media folders), else used where it is, else listed
//! as missing.
//!
//! **The result** is applied as one undo step ([`Command::ReplaceProject`],
//! "Merge …") and left unsaved. Conflicts are resolved as merged (ours) and
//! listed; a choice made while that step is still the last one merges
//! again with it and replaces the step's result in place
//! ([`History::apply_amending`]).
//!
//! [`History::apply_amending`]: faderframe_project::History::apply_amending

use crate::versions::{ProjectVersion, VERSIONS_FOLDER};
use crate::{NoticeLevel, Result, Session, SessionError, UiRequest, media, samples};
use faderframe_project::lineage::Lineage;
use faderframe_project::merge::{self, Conflict, MergeOptions, Merged, Resolution};
use faderframe_project::{Command, Project, SourceSpec, file};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The folder (inside `Versions`) of saved states kept as merge bases.
pub const BASES_FOLDER: &str = ".bases";
/// How many saved states are kept as bases (the newest).
pub const KEPT_BASES: usize = 8;
/// Files up to this size are hashed whole.
const WHOLE_HASH_BYTES: u64 = 4 << 20;
/// Larger ones: this many blocks spread over them …
const HASH_BLOCKS: u64 = 64;
/// … of this size.
const HASH_BLOCK: usize = 64 << 10;

/// A media file's content hash: `<length>:<sha-256 hex>`, of the whole
/// file up to 4 MiB, else of 64 blocks of 64 KiB spread over it (the first
/// at the start, the last at the end).
pub fn content_hash(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut h = faderframe_qc::sha256::Sha256::new();
    if len <= WHOLE_HASH_BYTES {
        let mut all = Vec::with_capacity(len as usize);
        f.read_to_end(&mut all)?;
        h.update(&all);
    } else {
        let mut block = vec![0; HASH_BLOCK];
        let last = len - HASH_BLOCK as u64;
        for i in 0..HASH_BLOCKS {
            f.seek(SeekFrom::Start(last * i / (HASH_BLOCKS - 1)))?;
            f.read_exact(&mut block)?;
            h.update(&block);
        }
    }
    Ok(format!("{len}:{}", h.hex()))
}

/// The length a content hash names.
fn hashed_length(hash: &str) -> Option<u64> {
    hash.split_once(':')?.0.parse().ok()
}

/// What happened to the other side's media.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaReport {
    /// Found here by content: (their name, the file here).
    pub found: Vec<(String, String)>,
    /// Not here: used where they are.
    pub elsewhere: Vec<String>,
    /// Nowhere to be found.
    pub missing: Vec<String>,
}

/// A merge as the views show it.
#[derive(Clone, Debug, PartialEq)]
pub struct MergeStatus {
    /// The undo step's name ("Merge Version 3 ‘Chorus rework’").
    pub label: String,
    /// What was merged in, and from what.
    pub theirs: String,
    pub base: String,
    pub conflicts: Vec<Conflict>,
    /// What the merge changed, in words.
    pub changes: Vec<String>,
    pub notes: Vec<String>,
    pub media: MediaReport,
    /// Its step is still the last: choices change it in place.
    pub open: bool,
}

/// The other side, read.
struct Theirs {
    project: Project,
    lineage: Option<Lineage>,
    /// In words ("version 3 ‘Chorus rework’", "‘song.ffproj’").
    name: String,
    label: String,
    /// From another folder (kept as a base once merged).
    foreign: bool,
    media: MediaReport,
}

/// A merge applied, its inputs kept for other choices.
pub(crate) struct PendingMerge {
    base: Project,
    ours: Project,
    theirs: Project,
    options: MergeOptions,
    merged: Merged,
    label: String,
    theirs_name: String,
    base_name: String,
    media: MediaReport,
    /// The lineages before (the merged one is made anew per choice).
    ours_lineage: Option<Lineage>,
    theirs_lineage: Option<Lineage>,
    /// `History::apply_amending`'s token while the step is the last.
    token: u64,
}

#[derive(Default)]
pub(crate) struct MergeState {
    /// Where the project comes from as saved or opened (`None`: never
    /// saved, or saved before stamps).
    lineage: Option<Lineage>,
    /// Merges since: each step's id and the line it makes (the newest
    /// done is the project's; undone, the project is as before it).
    merges: Vec<(u64, Lineage)>,
    pub(crate) pending: Option<PendingMerge>,
    /// Content hashes by file (length and time checked).
    hashes: HashMap<PathBuf, (u64, Option<SystemTime>, String)>,
}

impl MergeState {
    /// The project comes from `lineage` (opened, saved, restored, new).
    pub(crate) fn set_lineage(&mut self, lineage: Option<Lineage>) {
        self.lineage = lineage;
        self.merges.clear();
    }

    /// Where the project as it is comes from: through the newest merge
    /// whose step is done, else as saved.
    fn current(&self, history: &faderframe_project::History) -> Option<&Lineage> {
        self.merges
            .iter()
            .rev()
            .find(|(step, _)| history.is_done(*step))
            .map(|(_, l)| l)
            .or(self.lineage.as_ref())
    }

    /// The content hash of `path` (remembered while the file stays).
    fn hash(&mut self, path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        let (len, time) = (meta.len(), meta.modified().ok());
        if let Some((l, t, h)) = self.hashes.get(path)
            && *l == len
            && *t == time
        {
            return Some(h.clone());
        }
        let h = content_hash(path).ok()?;
        self.hashes
            .insert(path.to_path_buf(), (len, time, h.clone()));
        Some(h)
    }
}

/// The media files a project names (absolute in memory): its sources'
/// files and its samplers' samples.
fn media_of(project: &Project) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = project
        .sources
        .values()
        .filter_map(|s| match &s.spec {
            SourceSpec::File { path, .. } => Some(path.clone()),
            SourceSpec::Generated { .. } => None,
        })
        .collect();
    let seen = std::cell::RefCell::new(Vec::new());
    let mut copy = project.clone();
    samples::map_states(&mut copy, |p| {
        seen.borrow_mut().push(p.to_path_buf());
        p.to_path_buf()
    });
    out.extend(seen.into_inner());
    out.sort();
    out.dedup();
    out
}

fn version_words(v: &Path) -> String {
    match v
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.split_once(' '))
    {
        Some((n, name)) if n.parse::<u32>().is_ok() => {
            format!("version {} ‘{name}’", n.trim_start_matches('0'))
        }
        _ => format!(
            "‘{}’",
            v.file_name().and_then(|s| s.to_str()).unwrap_or("?")
        ),
    }
}

impl Session {
    fn bases_dir(&self) -> Option<PathBuf> {
        self.project_dir()
            .map(|d| d.join(VERSIONS_FOLDER).join(BASES_FOLDER))
    }

    /// Where the project as it is comes from: the stamp of the saved state
    /// it descends from, or the line a merge done since makes.
    pub fn project_lineage(&self) -> Option<&Lineage> {
        self.merging.current(&self.history)
    }

    /// The extras a save of `stored` (paths as stored relative to `dir`)
    /// stamps: a new state of this line, the media's content hashes.
    pub(crate) fn stamp_for_save(&mut self, dir: &Path) -> file::FileExtras {
        let lineage = Lineage::child_of(self.project_lineage(), self.project.ids.peek());
        let mut hashes = BTreeMap::new();
        for p in media_of(&self.project) {
            if let Some(h) = self.merging.hash(&p) {
                let stored = media::to_stored(&p, Some(dir));
                hashes.insert(stored.to_string_lossy().into_owned(), h);
            }
        }
        file::FileExtras {
            lineage: Some(lineage),
            media: hashes,
        }
    }

    /// After a save of the project file at `path` stamped `extras`: the
    /// state is the project's line now, kept as a merge base.
    pub(crate) fn note_saved_state(&mut self, path: &Path, extras: &file::FileExtras) {
        let Some(l) = extras.lineage.clone() else {
            return;
        };
        if let Some(dir) = self.bases_dir() {
            keep_base(&dir, path, &l.id);
        }
        self.merging.set_lineage(Some(l));
    }

    /// A version saved stamped `extras`: the project descends from it.
    pub(crate) fn note_saved_version(&mut self, extras: &file::FileExtras) {
        if let Some(l) = extras.lineage.clone() {
            self.merging.set_lineage(Some(l));
        }
    }

    /// A version's project and what its file carries beside it, its media
    /// found in the project folder.
    pub(crate) fn load_version_with(&self, path: &Path) -> Result<(Project, file::FileExtras)> {
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
            let _ = self.adopt_instrument(t);
        }
        Ok((project, loaded.extras))
    }

    /// The saved state with this stamp whose content is here: a base copy,
    /// a version or the project file.
    fn state_with_id(&self, id: &str) -> Option<(PathBuf, String)> {
        if let Some(dir) = self.bases_dir() {
            let p = dir.join(format!("{id}.{}", file::FILE_EXTENSION));
            if p.exists() {
                return Some((p, "the state both come from".into()));
            }
        }
        for v in self.versions() {
            if file::read_lineage(&v.path).is_some_and(|l| l.id == id) {
                return Some((v.path.clone(), format!("version {} ‘{}’", v.number, v.name)));
            }
        }
        let path = self.path.clone()?;
        file::read_lineage(&path)
            .is_some_and(|l| l.id == id)
            .then(|| (path, "the project as last saved".into()))
    }

    /// The base for merging `theirs` into the project: the newest state
    /// both descend from that is here.
    fn find_base(&self, theirs: Option<&Lineage>) -> Result<(PathBuf, String)> {
        let (Some(ours), Some(theirs)) = (self.project_lineage(), theirs) else {
            return Err(SessionError::Other(
                "these versions were saved before FaderFrame noted where versions come from: \
                 choose the version both come from"
                    .into(),
            ));
        };
        let common = ours.common_ancestors(theirs);
        if common.is_empty() {
            return Err(SessionError::Other(
                "the two have no saved state in common: they are not versions of one project"
                    .into(),
            ));
        }
        common
            .iter()
            .find_map(|a| self.state_with_id(&a.id))
            .ok_or_else(|| {
                SessionError::Other(
                    "the state both come from is not here any more: choose a version both come \
                     from"
                        .into(),
                )
            })
    }

    /// Merge the version at `theirs` into the project (`base`: the version
    /// both come from; `None`: found by their stamps).
    pub(crate) fn merge_version(&mut self, theirs: &Path, base: Option<&Path>) -> Result<()> {
        if self.project_dir().is_none() {
            return Err(SessionError::Other(
                "save the project first: its versions are kept next to it".into(),
            ));
        }
        let (project, extras) = self.load_version_with(theirs)?;
        let name = version_words(theirs);
        let label = format!("Merge {}", capitalised(&name));
        self.merge_in(
            Theirs {
                project,
                lineage: extras.lineage,
                name,
                label,
                foreign: false,
                media: MediaReport::default(),
            },
            base,
        )
    }

    /// Merge a project file from elsewhere (someone else's copy) into the
    /// project.
    pub(crate) fn merge_project_file(&mut self, path: &Path) -> Result<()> {
        if self.project_dir().is_none() {
            return Err(SessionError::Other(
                "save the project first: merges start from its saved states".into(),
            ));
        }
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let loaded = file::load(&path)?;
        let their_dir = path.parent().map(Path::to_path_buf);
        let mut project = loaded.project;
        // Their media where their file names them, then found here.
        let mut stored_of: HashMap<PathBuf, String> = HashMap::new();
        for s in project.sources.values_mut() {
            if let SourceSpec::File { path: p, .. } = &mut s.spec {
                let resolved = media::resolve(p, their_dir.as_deref());
                stored_of.insert(resolved.clone(), p.to_string_lossy().into_owned());
                *p = resolved;
            }
        }
        let stored_samples = std::cell::RefCell::new(Vec::new());
        samples::map_states(&mut project, |p| {
            let resolved = media::resolve(p, their_dir.as_deref());
            stored_samples
                .borrow_mut()
                .push((resolved.clone(), p.to_string_lossy().into_owned()));
            resolved
        });
        stored_of.extend(stored_samples.into_inner());
        for v in project.video.sources.values_mut() {
            v.path = media::resolve(&v.path, their_dir.as_deref());
        }
        for t in &mut project.tracks {
            let _ = self.adopt_instrument(t);
        }
        let report = self.find_media(&mut project, &stored_of, &loaded.extras.media);
        let name = format!(
            "‘{}’",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("?")
        );
        self.merge_in(
            Theirs {
                project,
                lineage: loaded.extras.lineage,
                label: format!("Merge {name}"),
                name,
                foreign: true,
                media: report,
            },
            None,
        )
    }

    /// Their media found here by content (or where they are, or missing).
    fn find_media(
        &mut self,
        theirs: &mut Project,
        stored_of: &HashMap<PathBuf, String>,
        hashes: &BTreeMap<String, String>,
    ) -> MediaReport {
        let mut report = MediaReport::default();
        // Files here that could be theirs: the project's media and what is
        // in its media folders.
        let mut here: Vec<PathBuf> = media_of(&self.project);
        if let Some(dir) = self.project_dir() {
            for sub in [crate::MEDIA_FOLDER, "Samples"] {
                let d = dir.join(sub);
                here.extend(files_in(&d));
                here.extend(files_in(&d.join("Samples")));
            }
        }
        here.sort();
        here.dedup();
        let mut map: HashMap<PathBuf, PathBuf> = HashMap::new();
        for p in media_of(theirs) {
            if here.contains(&p) {
                continue;
            }
            let wanted = stored_of
                .get(&p)
                .and_then(|s| hashes.get(s).cloned())
                .or_else(|| self.merging.hash(&p));
            let shown = p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            // A file whose blocks match is compared whole when theirs is
            // here (a copy changed between blocks — a redrawn sample — is
            // another file).
            let found = wanted.as_deref().and_then(|w| {
                let len = hashed_length(w)?;
                here.iter()
                    .filter(|h| std::fs::metadata(h).is_ok_and(|m| m.len() == len))
                    .find(|h| {
                        self.merging.hash(h).as_deref() == Some(w)
                            && (!p.exists() || same_content(&p, h).unwrap_or(false))
                    })
                    .cloned()
            });
            match found {
                Some(f) => {
                    report.found.push((shown, f.display().to_string()));
                    map.insert(p, f);
                }
                None if p.exists() => report.elsewhere.push(p.display().to_string()),
                None => report.missing.push(shown),
            }
        }
        if !map.is_empty() {
            for s in theirs.sources.values_mut() {
                if let SourceSpec::File { path, .. } = &mut s.spec
                    && let Some(f) = map.get(path.as_path())
                {
                    *path = f.clone();
                }
            }
            samples::map_states(theirs, |p| {
                map.get(p).cloned().unwrap_or_else(|| p.to_path_buf())
            });
        }
        report
    }

    fn merge_in(&mut self, theirs: Theirs, base: Option<&Path>) -> Result<()> {
        let (base_path, base_name) = match base {
            Some(b) => (b.to_path_buf(), version_words(b)),
            None => self.find_base(theirs.lineage.as_ref())?,
        };
        let (base_project, _) = self.load_version_with(&base_path)?;
        let ours_lineage = self.project_lineage().cloned();
        let mut options = MergeOptions::default();
        if let (Some(o), Some(t)) = (&ours_lineage, &theirs.lineage) {
            options.known = o.known_ranges(t);
            options.branch_ids = o.branch_point(t).map(|a| a.ids);
            if o.knows(&t.id) {
                return Err(SessionError::Other(format!(
                    "the project already has everything in {}",
                    theirs.name
                )));
            }
        }
        // A Mix Intelligence candidate or a mix scene's work never goes
        // into a merge.
        self.end_mix_audition();
        self.settle_scenes()?;
        let ours = self.absolute_copy();
        let merged = merge::merge(&base_project, &ours, &theirs.project, &options)
            .map_err(|e| SessionError::Other(e.to_string()))?;
        let mut pending = PendingMerge {
            base: base_project,
            ours,
            theirs: theirs.project,
            options,
            merged,
            label: theirs.label,
            theirs_name: theirs.name,
            base_name,
            media: theirs.media,
            ours_lineage,
            theirs_lineage: theirs.lineage,
            token: 0,
        };
        // Someone else's state is kept as a base for their next file (its
        // ids as merged).
        if theirs.foreign
            && let Some(t) = &pending.theirs_lineage
        {
            self.keep_merged_base(&pending.merged.theirs, t);
        }
        let changes = pending.merged.changes.len();
        let conflicts = pending.merged.conflicts.len();
        if changes == 0 && conflicts == 0 {
            // Nothing changes: the project is that line as it is.
            let merged = merged_lineage(&pending);
            self.merging.set_lineage(merged);
            self.notify(
                NoticeLevel::Info,
                format!(
                    "nothing to merge: the project has everything in {}",
                    pending.theirs_name
                ),
            );
            return Ok(());
        }
        self.apply_merge(&mut pending, None)?;
        self.notify(
            NoticeLevel::Info,
            format!(
                "merged {}: {changes} change{}, {conflicts} conflict{}",
                pending.theirs_name,
                if changes == 1 { "" } else { "s" },
                if conflicts == 1 { "" } else { "s" },
            ),
        );
        if conflicts > 0 {
            self.ui_requests.push(UiRequest::MergeConflicts);
        }
        self.merging.pending = Some(pending);
        Ok(())
    }

    /// Apply the pending merge's result as its undo step (`token`: the
    /// step to replace in place).
    fn apply_merge(&mut self, pending: &mut PendingMerge, token: Option<u64>) -> Result<()> {
        let cmd = Command::ReplaceProject {
            project: Box::new(pending.merged.project.clone()),
            label: pending.label.clone(),
        };
        let (impact, token) =
            self.history
                .apply_amending(&mut self.project, cmd, &pending.label, token)?;
        pending.token = token;
        if let (Some(step), Some(l)) = (self.history.last_step(), merged_lineage(pending)) {
            self.merging.merges.retain(|(s, _)| *s != step);
            self.merging.merges.push((step, l));
        }
        self.sync(impact)?;
        self.revision += 1;
        Ok(())
    }

    /// The merge's result with another choice for one conflict.
    pub(crate) fn resolve_merge_conflict(
        &mut self,
        key: &str,
        resolution: Resolution,
    ) -> Result<()> {
        let Some(mut pending) = self.merging.pending.take() else {
            return Err(SessionError::Other("no merge to resolve".into()));
        };
        if pending.token != self.history.revision() {
            self.merging.pending = Some(pending);
            return Err(SessionError::Other(
                "the project changed since the merge: undo back to it, or merge again".into(),
            ));
        }
        let Some(conflict) = pending.merged.conflicts.iter().find(|c| c.key == key) else {
            self.merging.pending = Some(pending);
            return Err(SessionError::Other(format!(
                "the merge has no conflict {key}"
            )));
        };
        if !conflict.choices.contains(&resolution) {
            let what = conflict.what.clone();
            self.merging.pending = Some(pending);
            return Err(SessionError::Other(format!(
                "{what}: “{}” is not a choice here",
                resolution.label()
            )));
        }
        pending
            .options
            .resolutions
            .insert(key.to_string(), resolution);
        let result = merge::merge(
            &pending.base,
            &pending.ours,
            &pending.theirs,
            &pending.options,
        )
        .map_err(|e| SessionError::Other(e.to_string()))
        .and_then(|m| {
            pending.merged = m;
            let token = Some(pending.token);
            self.apply_merge(&mut pending, token)
        });
        self.merging.pending = Some(pending);
        result
    }

    /// The merge as it stands (the last one, while it is open).
    pub fn merge_status(&self) -> Option<MergeStatus> {
        let p = self.merging.pending.as_ref()?;
        Some(MergeStatus {
            label: p.label.clone(),
            theirs: p.theirs_name.clone(),
            base: p.base_name.clone(),
            conflicts: p.merged.conflicts.clone(),
            changes: p.merged.changes.clone(),
            notes: p.merged.notes.clone(),
            media: p.media.clone(),
            open: p.token == self.history.revision(),
        })
    }

    /// Forget the merge (its step stays).
    pub(crate) fn close_merge(&mut self) {
        self.merging.pending = None;
        self.revision += 1;
    }

    /// The project as it would be with another choice for one conflict
    /// (for auditioning it; nothing changes).
    pub(crate) fn merge_alternative(&self, key: &str) -> Result<(Project, String)> {
        let p = self
            .merging
            .pending
            .as_ref()
            .ok_or_else(|| SessionError::Other("no merge to audition".into()))?;
        let c = p
            .merged
            .conflicts
            .iter()
            .find(|c| c.key == key)
            .ok_or_else(|| SessionError::Other(format!("the merge has no conflict {key}")))?;
        let other = if c.chosen == Resolution::Ours {
            Resolution::Theirs
        } else {
            Resolution::Ours
        };
        let mut options = p.options.clone();
        options.resolutions.insert(key.to_string(), other);
        let m = merge::merge(&p.base, &p.ours, &p.theirs, &options)
            .map_err(|e| SessionError::Other(e.to_string()))?;
        Ok((m.project, format!("{} with “{}”", c.what, other.label())))
    }

    /// Keep their state (ids as merged) as a base copy under its own id.
    fn keep_merged_base(&mut self, theirs: &Project, lineage: &Lineage) {
        let (Some(dir), Some(project_dir)) = (self.bases_dir(), self.project_dir()) else {
            return;
        };
        let mut stored = theirs.clone();
        for s in stored.sources.values_mut() {
            if let SourceSpec::File { path: p, .. } = &mut s.spec {
                *p = media::to_stored(p, Some(&project_dir));
            }
        }
        samples::map_states(&mut stored, |p| media::to_stored(p, Some(&project_dir)));
        let extras = file::FileExtras {
            lineage: Some(lineage.clone()),
            media: BTreeMap::new(),
        };
        let path = dir.join(format!("{}.{}", lineage.id, file::FILE_EXTENSION));
        if let Err(e) = std::fs::create_dir_all(&dir)
            .map_err(file::FileError::Io)
            .and_then(|_| file::save_with(&path, &stored, None, &extras))
        {
            tracing::warn!("could not keep {} as a merge base: {e}", path.display());
        }
        prune_bases(&dir);
    }

    /// The versions with the base found for merging `theirs` (for the
    /// Versions window: which one a merge would start from).
    pub fn merge_base_for(&self, theirs: &ProjectVersion) -> Option<String> {
        let l = file::read_lineage(&theirs.path)?;
        self.find_base(Some(&l)).ok().map(|(_, name)| name)
    }
}

/// The lineage of the project after `pending`'s merge.
fn merged_lineage(p: &PendingMerge) -> Option<Lineage> {
    match (&p.ours_lineage, &p.theirs_lineage) {
        (Some(o), Some(t)) => Some(o.merged(t, p.merged.moved, p.merged.project.ids.peek())),
        (o, _) => o.clone(),
    }
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase().collect::<String>() + c.as_str()
    })
}

/// Do the two files hold the same bytes?
fn same_content(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = fa.read(&mut ba)?;
        if n == 0 {
            return Ok(true);
        }
        fb.read_exact(&mut bb[..n])?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
}

/// The files directly in `dir`.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default()
}

/// Keep the saved project file at `saved` as the base copy `id` (a hard
/// link where it can be), the oldest copies past [`KEPT_BASES`] gone.
fn keep_base(dir: &Path, saved: &Path, id: &str) {
    let to = dir.join(format!("{id}.{}", file::FILE_EXTENSION));
    let kept = std::fs::create_dir_all(dir).and_then(|_| {
        std::fs::hard_link(saved, &to).or_else(|_| std::fs::copy(saved, &to).map(|_| ()))
    });
    if let Err(e) = kept {
        tracing::warn!("could not keep {} as a merge base: {e}", to.display());
    }
    prune_bases(dir);
}

fn prune_bases(dir: &Path) {
    let mut copies: Vec<(SystemTime, PathBuf)> = files_in(dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == file::FILE_EXTENSION))
        .map(|p| {
            let t = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (t, p)
        })
        .collect();
    copies.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, p) in copies.into_iter().skip(KEPT_BASES) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hashes_name_the_length_and_differ_with_the_content() {
        let dir = std::env::temp_dir().join(format!("ff-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
        std::fs::write(&a, b"one take").unwrap();
        std::fs::write(&b, b"one take").unwrap();
        std::fs::write(&c, b"two take").unwrap();
        let ha = content_hash(&a).unwrap();
        assert!(ha.starts_with("8:"));
        assert_eq!(hashed_length(&ha), Some(8));
        assert_eq!(ha, content_hash(&b).unwrap());
        assert_ne!(ha, content_hash(&c).unwrap());
        // Big files: blocks spread over them; a change between blocks is
        // found by comparing the files.
        let big: Vec<u8> = (0..(9u32 << 20)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&a, &big).unwrap();
        let mut changed = big.clone();
        let last = changed.len() - 1;
        changed[last] ^= 1;
        std::fs::write(&b, &changed).unwrap();
        assert_ne!(content_hash(&a).unwrap(), content_hash(&b).unwrap());
        let mut between = big.clone();
        between[100_000] ^= 1;
        std::fs::write(&c, &between).unwrap();
        assert_eq!(content_hash(&a).unwrap(), content_hash(&c).unwrap());
        assert!(!same_content(&a, &c).unwrap());
        std::fs::write(&c, &big).unwrap();
        assert!(same_content(&a, &c).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_are_named_by_number() {
        assert_eq!(
            version_words(Path::new("/p/Versions/003 Chorus.ffproj")),
            "version 3 ‘Chorus’"
        );
        assert_eq!(version_words(Path::new("/x/song.ffproj")), "‘song.ffproj’");
    }
}
