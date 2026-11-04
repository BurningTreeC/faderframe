//! Delivery QC in the session: the last delivery (a render the Render
//! window finished, an album export) is kept with what made it, so its QC
//! report can be written for any spec and its stems null-tested later.
//!
//! * [`Session::delivered`] notes a render's files (the shell calls it as
//!   the render's report opens; the album export notes its own).
//! * [`Session::qc_report`] is the report as it reads now for a spec,
//!   without receipts — the render report window shows its checks.
//! * [`Action::SaveQcReport`] writes it: on a worker every delivered file
//!   is hashed (SHA-256) and any not measured yet is read back, then the
//!   PDF and the JSON go beside each other through the exports' staging
//!   (asked before replacing).
//! * [`Action::NullTestStems`] null-tests a stems render that was not
//!   tested: the mix is rendered from the project as it was rendered
//!   then (a copy kept with the delivery) and compared with the stems as
//!   written.
//!
//! The figures come from the files as written (`render::stats` measures
//! them as it reads them back); nothing comes from the live mix.

use crate::render::stats::{self, FileStats, Written};
use crate::render::{
    RenderChannels, RenderProgress, RenderRange, RenderSettings, RenderSource, Rendered,
};
use crate::{Action, NoticeLevel, Result, Session, SessionError};
use faderframe_core::TrackId;
use faderframe_project::Project;
use faderframe_qc::report::{ProjectInfo, Setting};
use faderframe_qc::{NullTest, QcFile, QcReport, Receipt, Role, Spec, Utc, Verdict};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

pub use faderframe_qc::spec::{SPECS, Spec as DeliverySpec};

/// What a later null test needs: the project as the stems were rendered,
/// the render's settings and the stem files by track.
#[derive(Debug)]
struct Retest {
    project: Project,
    settings: RenderSettings,
    stems: Vec<(TrackId, PathBuf)>,
}

/// A finished delivery (see the module).
#[derive(Clone, Debug)]
pub struct Delivery {
    /// "Stems", "Master mix", "Album “Live”".
    pub title: String,
    /// The audio files as written, read back and measured.
    pub files: Vec<Rendered>,
    /// What each file is in the delivery.
    pub roles: Vec<Role>,
    /// Delivered files that are not measured audio (a cue sheet, a DDP
    /// fileset's files, a cutting sheet): receipts only.
    pub others: Vec<PathBuf>,
    /// A stems render's null test.
    pub null_test: Option<NullTest>,
    /// How it was rendered, as the report lists it.
    pub settings: Vec<Setting>,
    pub when: Utc,
    retest: Option<Arc<Retest>>,
}

impl Delivery {
    /// A stems render whose null test can still be made.
    pub fn can_null_test(&self) -> bool {
        self.null_test.is_none() && self.retest.is_some()
    }

    /// The files are stems.
    pub fn stems(&self) -> bool {
        self.roles.contains(&Role::Stem) && self.files.len() > 1
    }
}

/// A report being written: the worker's result.
struct Written_ {
    pdf: (PathBuf, Vec<u8>),
    json: (PathBuf, Vec<u8>),
    summary: String,
    verdict: Verdict,
}

type ReportJob = JoinHandle<std::result::Result<Written_, String>>;
type NullJob = (
    Arc<RenderProgress>,
    JoinHandle<std::result::Result<NullTest, String>>,
);

/// The session's delivery QC state.
#[derive(Default)]
pub(crate) struct QcState {
    last: Option<Delivery>,
    /// The render `Session::render` started last (paired with the files
    /// when [`Session::delivered`] notes them).
    pending: Option<(Arc<Project>, RenderSettings)>,
    /// The report being written, its stop flag and the destinations the
    /// user agreed to replace when it was asked for (a save dialog).
    report: Option<(ReportJob, Arc<AtomicBool>, Vec<PathBuf>)>,
    null_job: Option<NullJob>,
}

/// What the QC side is busy with.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QcStatus {
    /// A report is being written.
    pub writing: bool,
    /// A null test runs (its progress 0–1).
    pub null_testing: Option<f64>,
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// How a render was set up, as the report lists it.
pub fn describe_render(project: &Project, settings: &RenderSettings) -> Vec<Setting> {
    let mut out = Vec::new();
    let mut set = |name: &str, value: String| {
        out.push(Setting {
            name: name.into(),
            value,
        })
    };
    let range = match settings.range {
        RenderRange::Project => "the project".to_string(),
        RenderRange::Loop => "the loop range".to_string(),
        RenderRange::Bars { start, end } => format!("bars {}–{}", start + 1, end),
        RenderRange::Span { .. } => "a span".to_string(),
    };
    let span = crate::render::resolve_range(project, settings.range)
        .map(|(a, b)| {
            format!(
                " ({} → {})",
                project.timeline.format_bbt(a),
                project.timeline.format_bbt(b)
            )
        })
        .unwrap_or_default();
    set(
        "Range",
        format!("{range}{span}, tail {:.1} s", settings.tail_seconds),
    );
    set(
        "Source",
        match settings.source {
            RenderSource::Master => "the master mix".into(),
            RenderSource::Stems => "stems (a file per track, soloed in place)".into(),
            RenderSource::Track(t) => format!(
                "the track “{}” alone, as its stem",
                project.track(t).map_or("?", |t| t.name.as_str())
            ),
        },
    );
    let channels = match settings.channels {
        RenderChannels::Master => "as the master",
        RenderChannels::Stereo => "stereo",
        RenderChannels::Mono => "mono (summed)",
        RenderChannels::First => "the first channel",
        RenderChannels::Adm(_) => "ADM master",
        RenderChannels::Binaural(_) => "binaural (headphones)",
        RenderChannels::Iamf(_) => "IAMF master",
    };
    set(
        "Format",
        format!(
            "{}, {}, {channels}",
            settings.format.label(),
            faderframe_audio::format_sample_rate(settings.sample_rate)
        ),
    );
    if settings.format.is_integer() {
        set("Dither", settings.dither.label().into());
    }
    let level = match (settings.finish.loudness, settings.finish.ceiling) {
        (Some(l), Some(c)) => format!("loudness {l:.1} LUFS, true peak at most {c:.1} dBTP"),
        (Some(l), None) => format!("loudness {l:.1} LUFS"),
        (None, Some(c)) => format!("true peak limited to {c:.1} dBTP"),
        (None, None) => match settings.normalize_db {
            Some(p) => format!("peak normalised to {p:.1} dBFS"),
            None => "unchanged".into(),
        },
    };
    set("Level", level.replace('-', "−"));
    if settings.source == RenderSource::Stems {
        set(
            "Stems",
            if settings.stems.shared_stem {
                "made to null: the mix's gain on every stem, the rest in the Shared Processing \
                 stem"
                    .into()
            } else if settings.stems.null_test {
                "null-tested against the mix".into()
            } else {
                "not null-tested".into()
            },
        );
    }
    out
}

/// How an album export was set up, as the report lists it.
fn describe_album(s: &faderframe_project::album::AlbumSettings, rate: u32) -> Vec<Setting> {
    let mut out = vec![
        Setting {
            name: "Level".into(),
            value: format!(
                "{} levelling{}{}",
                match s.level {
                    faderframe_project::album::AlbumLevel::Album => "album",
                    _ => "per-song",
                },
                s.loudness
                    .map_or_else(String::new, |l| format!(", {l:.1} LUFS")),
                s.ceiling
                    .map_or_else(String::new, |c| format!(", at most {c:.1} dBTP"))
            )
            .replace('-', "−"),
        },
        Setting {
            name: "Format".into(),
            value: format!(
                "{}, {}",
                s.format.label(),
                faderframe_audio::format_sample_rate(rate)
            ),
        },
    ];
    if s.format.is_integer() {
        out.push(Setting {
            name: "Dither".into(),
            value: s.dither.label().into(),
        });
    }
    out
}

/// The files of `dir` (a DDP fileset), in name order.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| {
            r.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// One file of the report from its statistics (`hash`: its SHA-256,
/// `None` while not computed).
fn qc_file(
    path: &Path,
    stats: Option<&FileStats>,
    role: Role,
    spec: &Spec,
    hash: Option<std::result::Result<String, String>>,
) -> QcFile {
    let mut notes = Vec::new();
    let (sha256, facts, levels) = match stats {
        Some(st) => {
            notes.extend(st.notes.iter().cloned());
            (None, Some(st.facts()), st.qc_levels())
        }
        None => {
            notes.push("not measured: the file could not be read back".into());
            (None, None, None)
        }
    };
    let sha256 = match hash {
        Some(Ok(h)) => Some(h),
        Some(Err(e)) => {
            notes.push(format!("no receipt: {e}"));
            None
        }
        None => sha256,
    };
    let checks = match &facts {
        Some(f) => faderframe_qc::checks(f, levels.as_ref(), spec, role),
        None => Vec::new(),
    };
    QcFile {
        path: path.display().to_string(),
        name: name_of(path),
        role,
        sha256,
        facts,
        levels,
        checks,
        verdict: Verdict::Info,
        notes,
    }
}

/// The report of `d` for `spec` (`hashes`: each file's SHA-256 by path;
/// `measured`: statistics read now for files the delivery had none of).
fn build(
    d: &Delivery,
    project: ProjectInfo,
    spec: &Spec,
    hashes: &[(PathBuf, std::result::Result<String, String>)],
    measured: &[(PathBuf, FileStats)],
) -> QcReport {
    let hash_of = |p: &Path| hashes.iter().find(|(x, _)| x == p).map(|(_, h)| h.clone());
    let files = d
        .files
        .iter()
        .zip(&d.roles)
        .map(|(r, role)| {
            let st = r
                .stats
                .as_ref()
                .and_then(|s| s.as_ref().ok())
                .or_else(|| measured.iter().find(|(p, _)| *p == r.path).map(|(_, s)| s));
            qc_file(&r.path, st, *role, spec, hash_of(&r.path))
        })
        .collect();
    let mut report = QcReport::new(&d.title, project, d.when, spec, files);
    report.settings = d.settings.clone();
    report.receipts = d
        .others
        .iter()
        .map(|p| {
            let h = hash_of(p);
            Receipt {
                path: p.display().to_string(),
                name: name_of(p),
                bytes: std::fs::metadata(p).map_or(0, |m| m.len()),
                sha256: h.and_then(|h| h.ok()),
            }
        })
        .collect();
    report.null_test = d.null_test.clone().map(|mut n| {
        // The mix is the programme: checked against this spec.
        if let Some(m) = &mut n.mix
            && let Some(f) = &m.facts
        {
            m.checks = faderframe_qc::checks(f, m.levels.as_ref(), spec, Role::Programme);
        }
        n
    });
    report.tally();
    report
}

/// The PDF's and the JSON's paths for `path` (either, or neither: the
/// same name with each extension).
pub fn report_paths(path: &Path) -> (PathBuf, PathBuf) {
    let base = match path.extension().map(|e| e.to_ascii_lowercase()) {
        Some(e) if e == "pdf" || e == "json" => path.with_extension(""),
        _ => path.to_path_buf(),
    };
    let with = |ext: &str| {
        let mut s = base.clone().into_os_string();
        s.push(".");
        s.push(ext);
        PathBuf::from(s)
    };
    (with("pdf"), with("json"))
}

impl Session {
    fn project_info(&self) -> ProjectInfo {
        ProjectInfo {
            name: self.project().name.clone(),
            path: self.path().map(|p| p.display().to_string()),
            program: format!("FaderFrame {}", env!("CARGO_PKG_VERSION")),
            file_version: faderframe_project::file::CURRENT_VERSION,
        }
    }

    /// `Session::render` starts a render: kept for the delivery it makes.
    pub(crate) fn note_render(&mut self, project: &Project, settings: &RenderSettings) {
        self.qc.pending = Some((Arc::new(project.clone()), settings.clone()));
    }

    /// A render finished with `files` (`title`: "Stems", "Master mix");
    /// `null_test`: its null test ([`crate::render::RenderJob::null_test`]).
    /// It is the delivery the QC report is of from now on.
    pub fn delivered(&mut self, title: &str, files: Vec<Rendered>, null_test: Option<NullTest>) {
        let pending = self.qc.pending.take();
        let shared = null_test.as_ref().and_then(|n| n.shared_stem.clone());
        let source = pending.as_ref().map(|(_, s)| s.source);
        let roles = files
            .iter()
            .map(|f| match source {
                _ if shared.as_deref() == Some(name_of(&f.path).as_str()) => Role::Part,
                Some(RenderSource::Stems | RenderSource::Track(_)) => Role::Stem,
                _ => Role::Programme,
            })
            .collect();
        let (settings, retest) = match &pending {
            Some((project, settings)) => {
                let retest = (settings.source == RenderSource::Stems && null_test.is_none())
                    .then(|| crate::render::planned_tracks(project, settings).ok())
                    .flatten()
                    .map(|stems| {
                        Arc::new(Retest {
                            project: (**project).clone(),
                            settings: settings.clone(),
                            stems,
                        })
                    });
                (describe_render(project, settings), retest)
            }
            None => (Vec::new(), None),
        };
        self.qc.last = Some(Delivery {
            title: title.into(),
            files,
            roles,
            others: Vec::new(),
            null_test,
            settings,
            when: Utc::now(),
            retest,
        });
        self.revision += 1;
    }

    /// The album export `done` is the delivery now (its song files and
    /// album file as programmes, vinyl sides as parts; the cue sheet, the
    /// DDP fileset and the cutting sheet as receipts).
    pub(crate) fn note_album_export(&mut self, done: &crate::album::AlbumExport) {
        self.note_album_export_quietly(done);
        // The album's spec: its report goes with every export.
        if let Some(spec) = self.project.album.settings.qc_spec.clone() {
            let path = done.folder.join(format!(
                "{} QC Report",
                crate::render::sanitize(&self.project.name)
            ));
            if let Err(e) = self.save_qc_report(&path, &spec) {
                self.notify(NoticeLevel::Warning, format!("QC report: {e}"));
            }
        }
    }

    /// [`Session::note_album_export`] without writing its report.
    pub(crate) fn note_album_export_quietly(&mut self, done: &crate::album::AlbumExport) {
        let sides: Vec<&PathBuf> = done.vinyl.iter().flat_map(|v| &v.sides).collect();
        let roles = done
            .report
            .iter()
            .map(|r| {
                if sides.contains(&&r.path) {
                    Role::Part
                } else {
                    Role::Programme
                }
            })
            .collect();
        let mut others: Vec<PathBuf> = done.cue.iter().cloned().collect();
        if let Some((dir, _)) = &done.ddp {
            others.extend(files_in(dir));
        }
        if let Some(v) = &done.vinyl {
            others.push(v.sheet.clone());
        }
        let s = &self.project.album.settings;
        let rate = s.sample_rate.unwrap_or(self.project.sample_rate);
        self.qc.last = Some(Delivery {
            title: format!("Album “{}”", self.project.name),
            files: done.report.clone(),
            roles,
            others,
            null_test: None,
            settings: describe_album(s, rate),
            when: Utc::now(),
            retest: None,
        });
    }

    /// The last delivery (a render or an album export), if any.
    pub fn delivery(&self) -> Option<&Delivery> {
        self.qc.last.as_ref()
    }

    /// The QC report of the last delivery for `spec` as it reads now —
    /// every check, no receipts (those are computed when it is written).
    pub fn qc_report(&self, spec: &Spec) -> Option<QcReport> {
        let d = self.qc.last.as_ref()?;
        Some(build(d, self.project_info(), spec, &[], &[]))
    }

    /// What the QC side is doing.
    pub fn qc_status(&self) -> QcStatus {
        QcStatus {
            writing: self.qc.report.is_some(),
            null_testing: self.qc.null_job.as_ref().map(|(p, _)| p.fraction()),
        }
    }

    /// Write the QC report of the last delivery for the spec with id
    /// `spec` to `path` (a PDF and a JSON file beside it — see
    /// [`report_paths`]): receipts and readings on a worker, written when
    /// done ([`Session::tick`]).
    pub fn save_qc_report(&mut self, path: &Path, spec: &str) -> Result<()> {
        let spec = Spec::by_id(spec)
            .ok_or_else(|| SessionError::Other(format!("QC report: no spec “{spec}”")))?;
        let delivery = self
            .qc
            .last
            .clone()
            .ok_or_else(|| SessionError::Other("QC report: nothing delivered yet".into()))?;
        if self.qc.report.is_some() {
            return Err(SessionError::Other(
                "QC report: one is being written".into(),
            ));
        }
        let (pdf_path, json_path) = report_paths(path);
        crate::exports::files_only(&[pdf_path.clone(), json_path.clone()])?;
        // A save dialog's answer holds for when the report is written.
        let mut agreed: Vec<PathBuf> = [&pdf_path, &json_path]
            .into_iter()
            .filter(|p| self.agreed_to(p))
            .cloned()
            .collect();
        if self.agreed_to(path) && !agreed.contains(&pdf_path) {
            agreed.push(pdf_path.clone());
        }
        let project = self.project_info();
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("faderframe-qc".into())
            .spawn(move || {
                let stopped = || cancel.load(Ordering::Relaxed);
                let mut hashes = Vec::new();
                let mut measured = Vec::new();
                for r in &delivery.files {
                    if stopped() {
                        return Err("cancelled".into());
                    }
                    hashes.push((
                        r.path.clone(),
                        faderframe_qc::sha256::file(&r.path, &stopped).map_err(|e| e.to_string()),
                    ));
                    // Files the delivery did not measure are read now.
                    if !matches!(r.stats, Some(Ok(_))) {
                        let wav = r
                            .path
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("wav"));
                        if wav
                            && let Ok(st) = stats::measure(
                                &r.path,
                                &r.path,
                                &Written::Wav { bed: None },
                                0.0,
                                &cancel,
                            )
                        {
                            measured.push((r.path.clone(), st));
                        }
                    }
                }
                for p in &delivery.others {
                    hashes.push((
                        p.clone(),
                        faderframe_qc::sha256::file(p, &stopped).map_err(|e| e.to_string()),
                    ));
                }
                let report = build(&delivery, project, spec, &hashes, &measured);
                Ok(Written_ {
                    pdf: (pdf_path, faderframe_qc::pdf::write(&report)),
                    json: (json_path, report.to_json().into_bytes()),
                    summary: report.summary(),
                    verdict: report.verdict,
                })
            })
            .map_err(|e| SessionError::Other(format!("QC report: {e}")))?;
        self.qc.report = Some((handle, stop, agreed));
        self.revision += 1;
        Ok(())
    }

    /// Null-test the last delivery's stems (a stems render made without
    /// the test): the mix rendered from the project as it was then.
    pub fn start_null_test(&mut self) -> Result<()> {
        if self.qc.null_job.is_some() {
            return Ok(());
        }
        let retest = self
            .qc
            .last
            .as_ref()
            .filter(|d| d.null_test.is_none())
            .and_then(|d| d.retest.clone())
            .ok_or_else(|| {
                SessionError::Other("null test: the last render made no stems to test".into())
            })?;
        let progress = Arc::new(RenderProgress::default());
        let p = Arc::clone(&progress);
        let handle = std::thread::Builder::new()
            .name("faderframe-null-test".into())
            .spawn(move || {
                crate::render::null::test_written(
                    &retest.project,
                    &retest.settings,
                    &retest.stems,
                    &p,
                )
                .map_err(|e| e.to_string())
            })
            .map_err(|e| SessionError::Other(format!("null test: {e}")))?;
        self.qc.null_job = Some((progress, handle));
        self.revision += 1;
        Ok(())
    }

    /// Stop a running null test (and a report being written).
    pub fn cancel_qc(&mut self) {
        if let Some((p, _)) = &self.qc.null_job {
            p.cancel.store(true, Ordering::Relaxed);
        }
        if let Some((_, stop, _)) = &self.qc.report {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// Block until the QC work running now is done (tests, scripted
    /// runs).
    pub fn wait_for_qc(&mut self) {
        while self.qc.report.is_some() || self.qc.null_job.is_some() {
            let done = self
                .qc
                .report
                .as_ref()
                .is_none_or(|(h, _, _)| h.is_finished())
                && self
                    .qc
                    .null_job
                    .as_ref()
                    .is_none_or(|(_, h)| h.is_finished());
            if done {
                self.poll_qc();
            } else {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }

    /// Finished QC work: the report written, the null test noted.
    pub(crate) fn poll_qc(&mut self) {
        if self
            .qc
            .report
            .as_ref()
            .is_some_and(|(h, _, _)| h.is_finished())
            && let Some((handle, _, agreed)) = self.qc.report.take()
        {
            self.revision += 1;
            match handle.join() {
                Ok(Ok(w)) => {
                    let summary = w.summary.clone();
                    let level = match w.verdict {
                        Verdict::Fail => NoticeLevel::Warning,
                        _ => NoticeLevel::Info,
                    };
                    let name = name_of(&w.pdf.0);
                    self.agree_to_replace(agreed);
                    let r = self.write_export("QC report", vec![w.pdf, w.json], move |s, _| {
                        s.notify(level, format!("QC report written ({name}): {summary}"));
                    });
                    self.forget_agreement();
                    if let Err(e) = r {
                        self.notify(NoticeLevel::Error, format!("QC report: {e}"));
                    }
                }
                Ok(Err(e)) => self.notify(NoticeLevel::Warning, format!("QC report: {e}")),
                Err(_) => self.notify(NoticeLevel::Error, "QC report: the worker failed"),
            }
        }
        if self
            .qc
            .null_job
            .as_ref()
            .is_some_and(|(_, h)| h.is_finished())
            && let Some((_, handle)) = self.qc.null_job.take()
        {
            self.revision += 1;
            match handle.join() {
                Ok(Ok(test)) => {
                    let level = match test.verdict {
                        Verdict::Fail => NoticeLevel::Warning,
                        _ => NoticeLevel::Info,
                    };
                    self.notify(level, format!("Null test: {}", test.summary));
                    if let Some(d) = &mut self.qc.last {
                        d.null_test = Some(test);
                        d.retest = None;
                        if let Some(row) = d.settings.iter_mut().find(|r| r.name == "Stems") {
                            row.value = "null-tested against the mix after the render".into();
                        }
                    }
                }
                Ok(Err(e)) => self.notify(NoticeLevel::Warning, format!("null test: {e}")),
                Err(_) => self.notify(NoticeLevel::Error, "null test: the worker failed"),
            }
        }
    }

    /// Why stems rendered with `settings` would not sum to the mix, as the
    /// project shows it now (the Render window warns before rendering).
    pub fn null_causes(&self, settings: &RenderSettings) -> Vec<faderframe_qc::Cause> {
        crate::render::null::causes(&self.project, settings)
    }

    /// QC actions ([`Action::SaveQcReport`], [`Action::NullTestStems`]).
    pub(crate) fn qc_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::SaveQcReport { path, spec } => self.save_qc_report(&path, &spec),
            Action::NullTestStems => self.start_null_test(),
            _ => Ok(()),
        }
    }
}
