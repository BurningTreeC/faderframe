//! Reference tracks: finished records in the project
//! ([`faderframe_project::reference`]) heard instead of the mix — a
//! loudness-matched A/B in the monitor path ([`crate::compare`]) — their
//! spectral balance set beside the mix's and handed to EQ Match.
//!
//! * **Importing** ([`ReferenceOp::Add`]) decodes files into the media
//!   folder at the project's rate like any import; the list changes in one
//!   undoable step (`AddSource` + `SetReferences`).
//! * **Choosing** one ([`ReferenceOp::Choose`]) makes it what the mix is
//!   compared with. It plays from the media file — or, when the engine
//!   runs at another rate than the project, from a copy resampled into the
//!   session's temporary folder — following the timeline at its offset or
//!   looping its region ([`ReferenceSync`]).
//! * **Measuring** (on choosing, and on [`ReferenceOp::Measure`]): a job
//!   renders the mix over the same stretch — where the reference plays
//!   along the timeline, or the loop (else the song) for one looping on its
//!   own — and measures both as heard (the reference mapped onto the
//!   master's channels as the listening node maps it): integrated loudness
//!   (EBU R128, the BS.1770 channel weights of the master's layout) and the
//!   long-term average spectrum ([`faderframe_analysis::ltas`]). The match
//!   is mix − reference (+ the trim) on the reference side; the mix is
//!   never touched. A measurement belongs to the mix as it was: edits since
//!   are shown as stale, measuring again follows them.
//! * **Send to EQ Match** ([`ReferenceOp::SendToEqMatch`]): the master's
//!   (or a track's) first FaderFrame EQ — one is added when there is none —
//!   takes the reference's spectrum as its EQ Match reference
//!   ([`MATCH_REFERENCE_KEY`] = [`MATCH_FROM_REFERENCE`], the reference in
//!   [`MATCH_TRACK_KEY`]) and its editor opens on EQ Match
//!   ([`MATCH_OPEN_KEY`]).

use crate::compare::{Comparing, as_heard, integrated, master_layout};
use crate::media::{ImportJob, ImportTarget};
use crate::render::{self, RenderJob, RenderProgress};
use crate::{Action, NoticeLevel, Result, Session, SessionError, UiRequest};
use faderframe_analysis::ltas::Ltas;
use faderframe_audio_files::StreamSource;
use faderframe_core::{AudioSourceId, ChannelLayout, ReferenceId, TrackId};
use faderframe_engine::nodes::{CompareSettings, CompareSource, CompareSync};
use faderframe_project::reference::{ReferenceSync, ReferenceTrack, References};
use faderframe_project::{AudioSource, Command, PluginRef, SourceSpec};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

/// The EQ editor's EQ Match reference (a device view value; the editor's
/// `eq.match.reference`).
pub const MATCH_REFERENCE_KEY: &str = "eq.match.reference";
/// … this value: a reference track's spectrum …
pub const MATCH_FROM_REFERENCE: f64 = -3.0;
/// … this one (its id).
pub const MATCH_TRACK_KEY: &str = "eq.match.reference.track";
/// Raised to open the editor's EQ Match panel.
pub const MATCH_OPEN_KEY: &str = "eq.match.open";

/// Changes to the references and what is heard of them.
#[derive(Clone, Debug, PartialEq)]
pub enum ReferenceOp {
    /// Import files as references.
    Add(Vec<PathBuf>),
    Remove(ReferenceId),
    /// Compare the mix with it (`None`: with nothing).
    Choose(Option<ReferenceId>),
    Rename(ReferenceId, String),
    SetSync(ReferenceId, ReferenceSync),
    /// The file frames that play (`None`: all of it).
    SetRegion(ReferenceId, Option<(i64, i64)>),
    SetMatch(ReferenceId, bool),
    SetTrim(ReferenceId, f32),
    /// Measure it and the mix again (loudness, spectra).
    Measure(ReferenceId),
    /// A looping reference from this file frame on.
    Seek(i64),
    /// Its spectrum as EQ Match's reference in the first EQ of `track`
    /// (`None`: the master).
    SendToEqMatch {
        reference: ReferenceId,
        track: Option<TrackId>,
    },
}

/// A reference measured beside the mix.
#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceMeasure {
    /// Integrated loudness (LUFS) of its region as heard, and of the mix
    /// over the same stretch.
    pub reference_lufs: Option<f64>,
    pub mix_lufs: Option<f64>,
    /// Long-term average spectra.
    pub reference: Option<Ltas>,
    pub mix: Option<Ltas>,
    /// The stretch of the mix measured.
    pub mix_span: (MusicalTime, MusicalTime),
    /// The project's history revision then (later edits: stale).
    pub revision: u64,
    key: Key,
}

impl ReferenceMeasure {
    /// The gain bringing the reference to the mix (dB).
    pub fn match_db(&self) -> Option<f32> {
        crate::compare::match_gain(self.mix_lufs, self.reference_lufs)
    }
}

/// What a measurement depends on besides the mix.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Key {
    source: AudioSourceId,
    region: Option<(i64, i64)>,
    sync: ReferenceSync,
    rate: u32,
}

/// A file the engine plays a reference from (at its rate).
struct Playable {
    source: AudioSourceId,
    rate: u32,
    stream: Arc<StreamSource>,
    /// Engine frames per file frame of the project's media.
    scale: f64,
    /// A resampled copy in the temporary folder.
    temp: Option<PathBuf>,
}

struct Measured {
    reference_lufs: Option<f64>,
    mix_lufs: Option<f64>,
    reference: Option<Ltas>,
    mix: Option<Ltas>,
    /// The resampled copy made (engine rate ≠ project rate).
    resampled: Option<PathBuf>,
}

struct MeasureJob {
    reference: ReferenceId,
    key: Key,
    revision: u64,
    mix_span: (MusicalTime, MusicalTime),
    progress: Arc<RenderProgress>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<JoinHandle<std::result::Result<Measured, String>>>,
}

#[derive(Default)]
pub(crate) struct ReferenceState {
    imports: Vec<ImportJob>,
    jobs: Vec<MeasureJob>,
    measures: HashMap<ReferenceId, ReferenceMeasure>,
    playable: HashMap<ReferenceId, Playable>,
    /// EQ Match sends waiting for a measurement.
    pending_eq: Vec<(ReferenceId, Option<TrackId>)>,
}

impl ReferenceState {
    pub(crate) fn busy(&self) -> bool {
        !self.imports.is_empty() || !self.jobs.is_empty()
    }

    pub(crate) fn cancel(&mut self) {
        for j in &mut self.jobs {
            j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            j.progress
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(h) = j.handle.take() {
                let _ = h.join();
            }
        }
        self.jobs.clear();
        for p in self.playable.values() {
            if let Some(t) = &p.temp {
                let _ = std::fs::remove_file(t);
            }
        }
        self.playable.clear();
    }
}

/// Where a reference plays and what of the mix it is measured against.
struct Plan {
    settings: CompareSettings,
    mix_span: (MusicalTime, MusicalTime),
    key: Key,
    /// The media file (absolute) and its rate.
    media: PathBuf,
    media_rate: u32,
}

impl Session {
    // --- what the views read ---------------------------------------------------

    /// The project's references.
    pub fn references(&self) -> &References {
        &self.project.references
    }

    /// The reference the mix is compared with.
    pub fn current_reference(&self) -> Option<ReferenceId> {
        match self.comparing() {
            Some(Comparing::Reference(id)) => Some(id),
            _ => None,
        }
    }

    /// A reference's last measurement beside the mix.
    pub fn reference_measure(&self, id: ReferenceId) -> Option<&ReferenceMeasure> {
        self.compare.references.measures.get(&id)
    }

    /// Whether the mix changed since it was measured against `id`.
    pub fn reference_measure_stale(&self, id: ReferenceId) -> bool {
        self.reference_measure(id)
            .is_none_or(|m| m.revision != self.history.revision())
    }

    /// A reference's long-term average spectrum (EQ Match's reference).
    pub fn reference_spectrum(&self, id: ReferenceId) -> Option<&Ltas> {
        self.reference_measure(id)?.reference.as_ref()
    }

    /// A reference being measured: how far (0…1).
    pub fn reference_progress(&self, id: ReferenceId) -> Option<f64> {
        self.compare
            .references
            .jobs
            .iter()
            .find(|j| j.reference == id)
            .map(|j| j.progress.fraction())
    }

    /// Files being imported as references.
    pub fn importing_references(&self) -> bool {
        !self.compare.references.imports.is_empty()
    }

    /// The file frame a looping reference plays (its playhead in the
    /// reference's own time; `Follow`: where the timeline puts it), at the
    /// project's rate.
    pub fn reference_position(&self, id: ReferenceId) -> Option<i64> {
        let p = self.compare.references.playable.get(&id)?;
        (self.current_reference() == Some(id)).then(|| {
            (self.compare_link().shared().position() as f64 / p.scale.max(1e-9)).round() as i64
        })
    }

    /// A reference's length (frames at the project's rate).
    pub fn reference_frames(&self, id: ReferenceId) -> Option<i64> {
        let r = self.project.references.get(id)?;
        match &self.project.sources.get(&r.source)?.spec {
            SourceSpec::File { frames, .. } => Some(*frames),
            _ => None,
        }
    }

    // --- actions -----------------------------------------------------------------

    pub(crate) fn reference_op(&mut self, op: ReferenceOp) -> Result<()> {
        self.compare_follows_project();
        match op {
            ReferenceOp::Add(files) => self.import_references(files)?,
            ReferenceOp::Remove(id) => {
                if self.current_reference() == Some(id) {
                    self.compare_off();
                }
                self.change_references(|r| r.tracks.retain(|t| t.id != id))?;
                self.compare.references.measures.remove(&id);
                if let Some(p) = self.compare.references.playable.remove(&id)
                    && let Some(t) = p.temp
                {
                    self.compare.throw_away(t);
                }
            }
            ReferenceOp::Choose(None) => self.compare_off(),
            ReferenceOp::Choose(Some(id)) => self.choose_reference(Some(id))?,
            ReferenceOp::Rename(id, name) => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Ok(());
                }
                self.change_reference(id, |r| r.name = name)?;
            }
            ReferenceOp::SetSync(id, sync) => self.change_reference(id, |r| r.sync = sync)?,
            ReferenceOp::SetRegion(id, region) => {
                self.change_reference(id, |r| r.region = region)?;
            }
            ReferenceOp::SetMatch(id, on) => {
                self.change_reference(id, |r| r.match_loudness = on)?
            }
            ReferenceOp::SetTrim(id, db) => self.change_reference(id, |r| r.trim_db = db)?,
            ReferenceOp::Measure(id) => self.measure_reference(id)?,
            ReferenceOp::Seek(frame) => {
                if let Some(id) = self.current_reference()
                    && let Some(p) = self.compare.references.playable.get(&id)
                {
                    let f = (frame as f64 * p.scale).round() as i64;
                    self.compare_link().shared().seek(f);
                }
            }
            ReferenceOp::SendToEqMatch { reference, track } => {
                self.send_to_eq_match(reference, track)?;
            }
        }
        self.revision += 1;
        Ok(())
    }

    fn change_references(&mut self, f: impl FnOnce(&mut References)) -> Result<()> {
        let mut list = self.project.references.clone();
        f(&mut list);
        if list == self.project.references {
            return Ok(());
        }
        self.edit(Command::SetReferences {
            references: Box::new(list),
        })
    }

    fn change_reference(
        &mut self,
        id: ReferenceId,
        f: impl FnOnce(&mut ReferenceTrack),
    ) -> Result<()> {
        if self.project.references.get(id).is_none() {
            return Err(SessionError::Other(format!("no reference {id}")));
        }
        self.change_references(|list| {
            if let Some(r) = list.get_mut(id) {
                f(r);
            }
        })
    }

    fn import_references(&mut self, files: Vec<PathBuf>) -> Result<()> {
        let files: Vec<PathBuf> = files.into_iter().filter(|f| f.is_file()).collect();
        if files.is_empty() {
            return Err(SessionError::Other("no file to add as a reference".into()));
        }
        let n = files.len();
        let job = ImportJob::spawn(
            files,
            self.media_dir.clone(),
            self.project.sample_rate,
            ImportTarget {
                track: None,
                at: MusicalTime::ZERO,
            },
        );
        self.compare.references.imports.push(job);
        self.notify(
            NoticeLevel::Info,
            format!("adding {n} reference{}…", if n == 1 { "" } else { "s" }),
        );
        Ok(())
    }

    /// Imports done: the references in one step (the first chosen when
    /// nothing is compared yet).
    fn finish_reference_imports(&mut self) {
        let mut i = 0;
        while i < self.compare.references.imports.len() {
            if !self.compare.references.imports[i].is_finished() {
                i += 1;
                continue;
            }
            let job = self.compare.references.imports.remove(i);
            let mut commands = Vec::new();
            let mut list = self.project.references.clone();
            let mut added = Vec::new();
            for r in job.join() {
                let audio = match r {
                    Ok(a) => a,
                    Err((path, e)) => {
                        self.notify(
                            NoticeLevel::Error,
                            format!("reference {}: {e}", path.display()),
                        );
                        continue;
                    }
                };
                let p = &mut self.project;
                let source = AudioSource {
                    id: p.ids.allocate(),
                    name: audio.name.clone(),
                    spec: SourceSpec::File {
                        path: audio.path.clone(),
                        channels: audio.channels as u16,
                        frames: audio.frames as i64,
                        sample_rate: audio.sample_rate,
                    },
                };
                let id: ReferenceId = p.ids.allocate();
                list.tracks
                    .push(ReferenceTrack::new(id, audio.name.clone(), source.id));
                self.peaks.insert(source.id, Arc::new(audio.peaks));
                commands.push(Command::AddSource {
                    source: Box::new(source),
                });
                added.push(id);
            }
            if added.is_empty() {
                continue;
            }
            commands.push(Command::SetReferences {
                references: Box::new(list),
            });
            let label = if added.len() == 1 {
                "Add Reference"
            } else {
                "Add References"
            };
            if let Err(e) = self.edit(Command::Batch {
                label: label.into(),
                commands,
            }) {
                self.notify(NoticeLevel::Error, format!("references: {e}"));
                continue;
            }
            if self.comparing().is_none()
                && let Err(e) = self.choose_reference(added.first().copied())
            {
                self.notify(NoticeLevel::Error, e.to_string());
            }
            self.revision += 1;
        }
    }

    /// Compare the mix with reference `id` (prepared: played, measured).
    pub(crate) fn choose_reference(&mut self, id: Option<ReferenceId>) -> Result<()> {
        let Some(id) = id else {
            self.compare_off();
            return Ok(());
        };
        if self.project.references.get(id).is_none() {
            return Err(SessionError::Other(format!("no reference {id}")));
        }
        self.compare_with(Comparing::Reference(id));
        self.follow_reference();
        let fresh = self
            .reference_measure(id)
            .zip(self.reference_plan(id))
            .is_some_and(|(m, p)| m.key == p.key);
        if !fresh
            && !self
                .compare
                .references
                .jobs
                .iter()
                .any(|j| j.reference == id)
        {
            self.measure_reference(id)?;
        }
        Ok(())
    }

    /// Where reference `id` plays (engine samples) and the mix it is
    /// measured against.
    fn reference_plan(&self, id: ReferenceId) -> Option<Plan> {
        let r = self.project.references.get(id)?;
        let (media, frames, media_rate) = match &self.project.sources.get(&r.source)?.spec {
            SourceSpec::File {
                path,
                frames,
                sample_rate,
                ..
            } => (path.clone(), *frames, *sample_rate),
            _ => return None,
        };
        let media = crate::media::resolve(&media, self.project_dir().as_deref());
        let rate = self.engine.sample_rate();
        let scale = f64::from(rate) / f64::from(media_rate.max(1));
        let (from, to) = r.span(frames);
        let (from_e, to_e) = (
            (from as f64 * scale).round() as i64,
            (to as f64 * scale).round() as i64,
        );
        let p = &self.project;
        let sr = f64::from(rate);
        let end = p.timeline.to_samples(p.content_end(), sr);
        let whole = (MusicalTime::ZERO, p.content_end());
        let (sync, mix_span) = match r.sync {
            ReferenceSync::Follow { at } => {
                // `at` is at the project's rate.
                let at_e =
                    (at as f64 * f64::from(rate) / f64::from(p.sample_rate.max(1))).round() as i64;
                let (a, b) = (at_e.max(0), (at_e + (to_e - from_e)).min(end));
                let span = if b > a {
                    (p.timeline.to_musical(a, sr), p.timeline.to_musical(b, sr))
                } else {
                    whole
                };
                (
                    CompareSync::Follow {
                        anchor: at_e - from_e,
                    },
                    span,
                )
            }
            ReferenceSync::Loop => {
                let span = match p.loop_range {
                    Some(l) if p.loop_enabled && l.end > l.start => (l.start, l.end),
                    _ => whole,
                };
                (CompareSync::Loop, span)
            }
        };
        Some(Plan {
            settings: CompareSettings {
                sync,
                from: from_e,
                to: to_e,
            },
            mix_span,
            key: Key {
                source: r.source,
                region: r.region,
                sync: r.sync,
                rate,
            },
            media,
            media_rate,
        })
    }

    /// The compared reference's file, settings and gain as the project
    /// has them now (a change crossfades in).
    fn follow_reference(&mut self) {
        let Some(id) = self.current_reference() else {
            return;
        };
        let Some(r) = self.project.references.get(id).cloned() else {
            // Removed (an undo, say): nothing to compare with.
            self.compare_off();
            return;
        };
        let Some(plan) = self.reference_plan(id) else {
            return;
        };
        let rate = self.engine.sample_rate();
        let usable = self
            .compare
            .references
            .playable
            .get(&id)
            .is_some_and(|p| p.source == r.source && p.rate == rate);
        if !usable {
            if let Some(old) = self.compare.references.playable.remove(&id)
                && let Some(t) = old.temp
            {
                self.compare.throw_away(t);
            }
            // The media at the engine's rate plays as it is; another rate
            // waits for the measuring job's resampled copy.
            if plan.media_rate == rate {
                match StreamSource::open(&plan.media) {
                    Ok(stream) => {
                        self.compare.references.playable.insert(
                            id,
                            Playable {
                                source: r.source,
                                rate,
                                stream,
                                scale: 1.0,
                                temp: None,
                            },
                        );
                    }
                    Err(e) => {
                        self.notify(
                            NoticeLevel::Error,
                            format!("reference {}: {e}", plan.media.display()),
                        );
                        self.compare_off();
                        return;
                    }
                }
            }
        }
        let source = self
            .compare
            .references
            .playable
            .get(&id)
            .map(|p| CompareSource {
                source: Arc::clone(&p.stream),
                settings: plan.settings,
            });
        if source.is_some() {
            self.set_compare_source(source);
        }
        let measured = self
            .reference_measure(id)
            .filter(|m| m.key == plan.key)
            .and_then(ReferenceMeasure::match_db);
        match measured {
            Some(g) if r.match_loudness => self.set_compare_gain(g + r.trim_db, true),
            _ => self.set_compare_gain(r.trim_db, false),
        }
    }

    /// Measure reference `id` and the mix beside it (a job).
    fn measure_reference(&mut self, id: ReferenceId) -> Result<()> {
        let plan = self
            .reference_plan(id)
            .ok_or_else(|| SessionError::Other(format!("reference {id} has no file")))?;
        // One at a time per reference: the newest asked for.
        if let Some(i) = self
            .compare
            .references
            .jobs
            .iter()
            .position(|j| j.reference == id)
        {
            let mut old = self.compare.references.jobs.remove(i);
            old.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            old.progress
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            // Its thread ends by itself; its result is not wanted.
            drop(old.handle.take());
        }
        let rate = self.engine.sample_rate();
        let dir = self.compare.dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", dir.display())))?;
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mix_path = dir.join(format!("mix-{n}.wav"));
        let project = self.absolute_copy();
        let layout = master_layout(&project);
        let settings = crate::compare::compare_render_settings(
            &project,
            plan.mix_span,
            rate,
            mix_path.clone(),
        );
        let mix =
            render::start(project, settings).map_err(|e| SessionError::Other(e.to_string()))?;
        let progress = Arc::clone(&mix.progress);
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let resample = (plan.media_rate != rate).then(|| dir.join(format!("reference-{n}")));
        let region = (plan.settings.from, plan.settings.to);
        let media = plan.media.clone();
        let stop = Arc::clone(&cancel);
        let handle = std::thread::Builder::new()
            .name("faderframe-reference".into())
            .spawn(move || {
                measure(
                    mix, &mix_path, &media, resample, region, layout, rate, &stop,
                )
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.compare.references.jobs.push(MeasureJob {
            reference: id,
            key: plan.key,
            revision: self.history.revision(),
            mix_span: plan.mix_span,
            progress,
            cancel,
            handle: Some(handle),
        });
        Ok(())
    }

    fn finish_measures(&mut self) {
        let mut i = 0;
        while i < self.compare.references.jobs.len() {
            let done = self.compare.references.jobs[i]
                .handle
                .as_ref()
                .is_none_or(JoinHandle::is_finished);
            if !done {
                i += 1;
                continue;
            }
            let mut job = self.compare.references.jobs.remove(i);
            let result = match job.handle.take().map(JoinHandle::join) {
                Some(Ok(r)) => r,
                _ => Err("the measurement stopped".into()),
            };
            match result {
                Ok(m) => {
                    if let Some(path) = m.resampled {
                        // The copy at the engine's rate plays.
                        match StreamSource::open(&path) {
                            Ok(stream) => {
                                let scale = f64::from(job.key.rate)
                                    / f64::from(self.project.sample_rate.max(1));
                                if let Some(old) = self.compare.references.playable.insert(
                                    job.reference,
                                    Playable {
                                        source: job.key.source,
                                        rate: job.key.rate,
                                        stream,
                                        scale,
                                        temp: Some(path),
                                    },
                                ) && let Some(t) = old.temp
                                {
                                    self.compare.throw_away(t);
                                }
                            }
                            Err(e) => self.notify(NoticeLevel::Error, format!("reference: {e}")),
                        }
                    }
                    self.compare.references.measures.insert(
                        job.reference,
                        ReferenceMeasure {
                            reference_lufs: m.reference_lufs,
                            mix_lufs: m.mix_lufs,
                            reference: m.reference,
                            mix: m.mix,
                            mix_span: job.mix_span,
                            revision: job.revision,
                            key: job.key,
                        },
                    );
                    let waiting: Vec<_> = self
                        .compare
                        .references
                        .pending_eq
                        .iter()
                        .filter(|(r, _)| *r == job.reference)
                        .copied()
                        .collect();
                    self.compare
                        .references
                        .pending_eq
                        .retain(|(r, _)| *r != job.reference);
                    for (reference, track) in waiting {
                        if let Err(e) = self.send_to_eq_match(reference, track) {
                            self.notify(NoticeLevel::Error, e.to_string());
                        }
                    }
                }
                Err(e) if !job.cancel.load(std::sync::atomic::Ordering::Relaxed) => {
                    self.notify(NoticeLevel::Error, format!("measuring the reference: {e}"));
                }
                Err(_) => {}
            }
            self.revision += 1;
        }
    }

    /// The tick's share: imports, measurements, the compared reference
    /// following its settings.
    pub(crate) fn poll_references(&mut self) {
        if !self.compare.references.imports.is_empty() {
            self.finish_reference_imports();
        }
        if !self.compare.references.jobs.is_empty() {
            self.revision += 1;
            self.finish_measures();
        }
        if let Some(id) = self.current_reference() {
            self.follow_reference();
            // Its region or sync changed: measured again.
            let stale_key = self
                .reference_plan(id)
                .zip(self.reference_measure(id))
                .is_some_and(|(p, m)| p.key != m.key);
            if stale_key
                && !self
                    .compare
                    .references
                    .jobs
                    .iter()
                    .any(|j| j.reference == id)
                && let Err(e) = self.measure_reference(id)
            {
                self.notify(NoticeLevel::Error, e.to_string());
            }
        }
    }

    /// The reference's spectrum as EQ Match's reference in the first
    /// FaderFrame EQ of `track` (the master by default; one is added when
    /// there is none), its editor opened on EQ Match.
    fn send_to_eq_match(&mut self, reference: ReferenceId, track: Option<TrackId>) -> Result<()> {
        if self.project.references.get(reference).is_none() {
            return Err(SessionError::Other(format!("no reference {reference}")));
        }
        let track = track
            .or_else(|| self.project.master_id())
            .ok_or_else(|| SessionError::Other("no track for the EQ".into()))?;
        if self.reference_spectrum(reference).is_none() {
            // Measured first, then sent.
            if !self
                .compare
                .references
                .jobs
                .iter()
                .any(|j| j.reference == reference)
            {
                self.measure_reference(reference)?;
            }
            self.compare
                .references
                .pending_eq
                .push((reference, Some(track)));
            self.notify(
                NoticeLevel::Info,
                "measuring the reference's spectrum for EQ Match…",
            );
            return Ok(());
        }
        let eq = |s: &Session| {
            s.project.track(track).and_then(|t| {
                t.slots()
                    .into_iter()
                    .find(|p| {
                        p.plugin.format == faderframe_project::PluginFormat::Builtin
                            && p.plugin.id == faderframe_core::builtin::EQ
                    })
                    .map(|p| p.id)
            })
        };
        let plugin = match eq(self) {
            Some(p) => p,
            None => {
                let index = self.project.track(track).map_or(0, |t| t.inserts.len());
                self.dispatch(Action::InsertPlugin {
                    track,
                    index,
                    plugin: PluginRef::builtin(faderframe_core::builtin::EQ, "EQ"),
                })?;
                eq(self).ok_or_else(|| SessionError::Other("the EQ could not be added".into()))?
            }
        };
        let open = self.device_view(plugin, MATCH_OPEN_KEY).unwrap_or(0.0) + 1.0;
        self.dispatch(Action::SetDeviceView {
            plugin,
            values: vec![
                (MATCH_REFERENCE_KEY.into(), MATCH_FROM_REFERENCE),
                (MATCH_TRACK_KEY.into(), reference.raw() as f64),
                (MATCH_OPEN_KEY.into(), open),
            ],
        })?;
        self.ui_requests.push(UiRequest::PluginEditor {
            track,
            plugin,
            generic: false,
        });
        let name = self
            .project
            .references
            .get(reference)
            .map_or_else(String::new, |r| r.name.clone());
        self.notify(
            NoticeLevel::Info,
            format!("EQ Match: ‘{name}’ is the reference — play the mix to match it"),
        );
        Ok(())
    }
}

/// The job: the reference's region (resampled first when the engine runs
/// at another rate) and the rendered mix, measured as heard.
#[allow(clippy::too_many_arguments)]
fn measure(
    mix: RenderJob,
    mix_path: &Path,
    media: &Path,
    resample: Option<PathBuf>,
    region: (i64, i64),
    layout: ChannelLayout,
    rate: u32,
    cancel: &std::sync::atomic::AtomicBool,
) -> std::result::Result<Measured, String> {
    let mut resampled = None;
    let playable = match resample {
        Some(dir) => {
            let progress = faderframe_audio_files::import::ImportProgress::default();
            let a =
                faderframe_audio_files::import::import_file(media, &dir, rate, &progress, cancel)
                    .map_err(|e| e.to_string())?;
            resampled = Some(a.path.clone());
            a.path
        }
        None => media.to_path_buf(),
    };
    let count = layout.channel_count().max(1);
    let file = faderframe_audio_files::wavstream::WavFile::open(&playable)
        .map_err(|e| format!("{}: {e}", playable.display()))?;
    let (from, to) = (
        region.0.clamp(0, file.frames() as i64),
        region.1.clamp(0, file.frames() as i64),
    );
    let len = (to - from).max(0) as usize;
    let mut channels = vec![vec![0.0f32; len]; file.channels()];
    {
        let mut slices: Vec<&mut [f32]> = channels.iter_mut().map(Vec::as_mut_slice).collect();
        file.read(from as u64, &mut slices, &mut Vec::new())
            .map_err(|e| e.to_string())?;
    }
    let heard = as_heard(channels, count);
    let reference_lufs = Some(integrated(&heard, layout, rate)).filter(|l| l.is_finite());
    let views: Vec<&[f32]> = heard.iter().map(Vec::as_slice).collect();
    let reference = faderframe_analysis::ltas::ltas(&views, rate);
    drop(heard);
    let rendered = mix.join();
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        let _ = std::fs::remove_file(mix_path);
        return Err("cancelled".into());
    }
    rendered.map_err(|e| e.to_string())?;
    let (mix_channels, mix_rate) = crate::compare::read_file(mix_path)?;
    let _ = std::fs::remove_file(mix_path);
    let mix_lufs = Some(integrated(&mix_channels, layout, mix_rate)).filter(|l| l.is_finite());
    let views: Vec<&[f32]> = mix_channels.iter().map(Vec::as_slice).collect();
    let mix = faderframe_analysis::ltas::ltas(&views, mix_rate);
    Ok(Measured {
        reference_lufs,
        mix_lufs,
        reference,
        mix,
        resampled,
    })
}
