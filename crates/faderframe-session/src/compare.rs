//! A/B listening: something heard instead of the mix in the monitor path
//! (`faderframe_engine::nodes::compare`), switched in an instant and in
//! step with the transport — never in a render, an export, a bounce or a
//! freeze, never on a meter.
//!
//! Two things are compared with the mix:
//!
//! * **The undo history** ([`Action::AuditionHistory`]): the project as it
//!   was some steps ago, made on a copy of the project as undoing would
//!   make it ([`faderframe_project::History::project_at`]: neither the
//!   project nor the history moves), rendered offline on its own engine
//!   over [`Session::audition_range`] with two seconds of run-up (tails
//!   from before it sound), into a file in a per-session temporary folder.
//!   The mix as it is ("Now") is rendered over the same stretch beside it,
//!   to match the two in loudness. "Then" and "Now" then flip with
//!   [`CompareOp::Flip`].
//! * **Reference tracks** ([`crate::references`]).
//!
//! The session hands the engine the file and its settings
//! (`CompareLink::set`, crossfaded there), keeps its pages resident through
//! the disk loader, sets the gain (a loudness match: the other side is
//! brought to the mix's integrated loudness, EBU R128, measured offline on
//! both) and hands everything again to an engine made anew.

use crate::render::{self, RenderChannels, RenderJob, RenderRange, RenderSettings};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_audio_files::{StreamSource, WavFormat};
use faderframe_core::{ChannelLayout, ReferenceId, db_to_gain};
use faderframe_engine::nodes::{CompareLink, CompareSettings, CompareSource, CompareSync};
use faderframe_project::Project;
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Run-up rendered before an audition's stretch (reverb and delay tails
/// from before it sound in it, as they do in the mix playing through).
pub const AUDITION_RUN_UP_SECONDS: f64 = 2.0;

/// What the mix is compared with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparing {
    /// The project as it was with `done` steps of the history done
    /// ("Then"; the mix is "Now").
    History { done: usize },
    /// A reference track.
    Reference(ReferenceId),
}

/// Switching between the mix and what it is compared with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareOp {
    /// Hear the other side (`true`: "Then", the reference) or the mix.
    Hear(bool),
    /// From one to the other.
    Flip,
    /// An audition played as loud as the mix (references have their own
    /// setting: `ReferenceTrack::match_loudness`).
    MatchLoudness(bool),
    /// Compare with nothing: the mix alone (an audition's files go).
    Off,
}

/// What the views show of the comparison.
#[derive(Clone, Debug, PartialEq)]
pub struct CompareStatus {
    pub what: Comparing,
    /// The other side is asked for.
    pub hearing_other: bool,
    /// How much of it is heard now (0: the mix; crossfading between).
    pub heard: f32,
    /// The other side's gain (dB) …
    pub gain_db: f32,
    /// … a loudness match.
    pub matched: bool,
    /// The two sides' names ("Now"/"Then", "Mix"/"Reference").
    pub mix_label: &'static str,
    pub other_label: &'static str,
    /// What is heard, in words.
    pub text: String,
    /// Playable (else being prepared: `progress`).
    pub ready: bool,
    pub progress: Option<f64>,
}

/// An audition of the undo history, as the views show it.
#[derive(Clone, Debug, PartialEq)]
pub struct AuditionInfo {
    /// Steps done in the project auditioned.
    pub done: usize,
    /// The step it is just after ("Project opened" for none).
    pub label: String,
    /// The stretch compared (heard range, musical, on the project now).
    pub range: (MusicalTime, MusicalTime),
    /// What was rendered of "Then" (its own musical times: run-up
    /// included) and the file.
    pub span: (MusicalTime, MusicalTime),
    pub path: PathBuf,
    pub rate: u32,
    /// Rendering (0…1), else done.
    pub progress: Option<f64>,
    /// Integrated loudness of "Then" and "Now" over the stretch (LUFS).
    pub then_lufs: Option<f64>,
    pub now_lufs: Option<f64>,
    pub error: Option<String>,
}

/// A render of "Then" and "Now" for an audition.
struct AuditionJob {
    then: RenderJob,
    now: Option<RenderJob>,
}

struct Audition {
    info: AuditionInfo,
    now_path: PathBuf,
    /// Engine samples: the file's first frame plays at `anchor`; the
    /// stretch heard is file frames `[from, to)`.
    anchor: i64,
    from: i64,
    to: i64,
    job: Option<AuditionJob>,
    /// The master's layout when it was rendered (for the loudness).
    layout: ChannelLayout,
}

pub(crate) struct CompareState {
    what: Option<Comparing>,
    /// The file heard on the other side and how (once prepared).
    pub(crate) source: Option<CompareSource>,
    /// Where it was handed (an engine made anew gets it again) and whether
    /// it changed since.
    sent: Option<Arc<CompareLink>>,
    dirty: bool,
    hear: bool,
    /// An audition played as loud as the mix.
    match_audition: bool,
    gain_db: f32,
    matched: bool,
    /// The project it belongs to (`Session::project_serial`).
    serial: u64,
    audition: Option<Audition>,
    pub(crate) references: crate::references::ReferenceState,
    /// The session's temporary folder (renders, resampled references).
    dir: Option<PathBuf>,
    /// Files to delete once nothing holds them (Windows keeps open files).
    trash: Vec<PathBuf>,
    /// A crossfade was moving when last polled (views repaint).
    moving: bool,
}

impl Default for CompareState {
    fn default() -> Self {
        Self {
            what: None,
            source: None,
            sent: None,
            dirty: false,
            hear: false,
            match_audition: true,
            gain_db: 0.0,
            matched: false,
            serial: 0,
            audition: None,
            references: Default::default(),
            dir: None,
            trash: Vec::new(),
            moving: false,
        }
    }
}

impl CompareState {
    /// The session's temporary folder for what is compared.
    pub(crate) fn dir(&mut self) -> PathBuf {
        self.dir
            .get_or_insert_with(|| {
                static NEXT: AtomicUsize = AtomicUsize::new(0);
                let n = NEXT.fetch_add(1, Ordering::Relaxed);
                std::env::temp_dir().join(format!("faderframe-compare-{}-{n}", std::process::id()))
            })
            .clone()
    }

    pub(crate) fn throw_away(&mut self, path: PathBuf) {
        if std::fs::remove_file(&path).is_err() && path.exists() {
            self.trash.push(path);
        }
    }

    fn empty_trash(&mut self) {
        self.trash
            .retain(|p| std::fs::remove_file(p).is_err() && p.exists());
    }

    fn drop_audition(&mut self) {
        if let Some(a) = self.audition.take() {
            if let Some(job) = a.job {
                job.then.cancel();
                if let Some(now) = &job.now {
                    now.cancel();
                }
            }
            self.throw_away(a.info.path);
            self.throw_away(a.now_path);
        }
    }
}

impl Drop for CompareState {
    fn drop(&mut self) {
        if let Some(a) = self.audition.take()
            && let Some(job) = a.job
        {
            job.then.cancel();
            if let Some(now) = job.now {
                now.cancel();
                let _ = now.join();
            }
            let _ = job.then.join();
        }
        self.references.cancel();
        if let Some(d) = self.dir.take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// Render settings for a stretch of `project` heard on the other side: the
/// master as it is (a bed's every channel), float, nothing done to it.
pub fn compare_render_settings(
    project: &Project,
    span: (MusicalTime, MusicalTime),
    rate: u32,
    output: PathBuf,
) -> RenderSettings {
    RenderSettings {
        range: RenderRange::Span {
            start: span.0,
            end: span.1,
        },
        channels: RenderChannels::Master,
        format: WavFormat::Float32,
        sample_rate: rate,
        tail_seconds: 0.0,
        normalize_db: None,
        finish: Default::default(),
        dither: faderframe_audio_files::Dither::Off,
        report: false,
        overwrite: render::Overwrite::Always,
        keep_latency: false,
        head: None,
        ..RenderSettings::defaults_for(project, output)
    }
}

/// The weights of a layout's channels in its integrated loudness (BS.1770:
/// the surrounds 1.41, the LFE nothing).
pub(crate) fn loudness_weights(layout: ChannelLayout) -> Vec<f64> {
    match layout {
        ChannelLayout::Surround(f) => f
            .speakers()
            .iter()
            .map(|s| {
                if s.lfe {
                    0.0
                } else {
                    faderframe_analysis::speaker_weight(
                        f64::from(s.azimuth),
                        f64::from(s.elevation),
                    )
                }
            })
            .collect(),
        other => vec![1.0; other.channel_count().max(1)],
    }
}

/// The channels of a file as the compare source plays them into a mix of
/// `count` channels (as the listening node maps them).
pub(crate) fn as_heard(file: Vec<Vec<f32>>, count: usize) -> Vec<Vec<f32>> {
    let fc = file.len();
    if fc == count || fc == 0 {
        return file;
    }
    let len = file.iter().map(Vec::len).min().unwrap_or(0);
    if count == 1 {
        let mid = (0..len).map(|i| 0.5 * (file[0][i] + file[1][i])).collect();
        return vec![mid];
    }
    (0..count)
        .map(|c| {
            let ch = if fc == 1 {
                (c < 2).then_some(0)
            } else {
                (c < fc).then_some(c)
            };
            ch.map_or_else(|| vec![0.0; len], |ch| file[ch][..len].to_vec())
        })
        .collect()
}

/// Integrated loudness (LUFS) of `channels` of a mix of `layout`.
pub(crate) fn integrated(channels: &[Vec<f32>], layout: ChannelLayout, rate: u32) -> f64 {
    let views: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
    faderframe_analysis::integrated_weighted(&views, &loudness_weights(layout), rate).0
}

/// The master's layout in `project` (stereo without a master).
pub(crate) fn master_layout(project: &Project) -> ChannelLayout {
    project
        .master_id()
        .and_then(|m| project.track(m))
        .map_or(ChannelLayout::Stereo, |t| t.layout)
}

/// A loudness match: the gain bringing `other` to `mix` (0 when either is
/// silence).
pub(crate) fn match_gain(mix: Option<f64>, other: Option<f64>) -> Option<f32> {
    match (mix, other) {
        (Some(m), Some(o)) if m.is_finite() && o.is_finite() => Some((m - o) as f32),
        _ => None,
    }
}

/// Read a rendered file whole (control or worker thread).
pub(crate) fn read_file(path: &Path) -> std::result::Result<(Vec<Vec<f32>>, u32), String> {
    faderframe_audio_files::read_wav(path)
        .map(|w| (w.channels, w.sample_rate))
        .map_err(|e| format!("{}: {e}", path.display()))
}

impl Session {
    // --- what the views read ---------------------------------------------------

    /// What the mix is compared with, and what is heard.
    pub fn compare_status(&self) -> Option<CompareStatus> {
        let what = self.compare.what?;
        let shared = self.engine.listening().link.compare().shared();
        let (mix_label, other_label, other_name) = match what {
            Comparing::History { .. } => ("Now", "Then", self.audition_label()),
            Comparing::Reference(id) => (
                "Mix",
                "Reference",
                self.project
                    .references
                    .get(id)
                    .map_or_else(|| "the reference".into(), |r| format!("‘{}’", r.name)),
            ),
        };
        let progress = match what {
            Comparing::History { .. } => self.audition_info().and_then(|a| a.progress),
            Comparing::Reference(id) => self.reference_progress(id),
        };
        let ready = self.compare.source.is_some();
        let hearing_other = self.compare.hear;
        let gain = if self.compare.gain_db.abs() >= 0.05 {
            format!(" at {:+.1} dB", self.compare.gain_db)
        } else {
            String::new()
        };
        let matched = if self.compare.matched {
            " (loudness-matched)"
        } else {
            ""
        };
        let text = match (hearing_other, ready) {
            (false, _) => format!("Hearing: {mix_label} — the mix as it is"),
            (true, true) => format!("Hearing: {other_label} — {other_name}{gain}{matched}"),
            (true, false) => format!("Hearing: {mix_label} ({other_label} is being prepared)"),
        };
        Some(CompareStatus {
            what,
            hearing_other,
            heard: shared.heard(),
            gain_db: self.compare.gain_db,
            matched: self.compare.matched,
            mix_label,
            other_label,
            text,
            ready,
            progress,
        })
    }

    fn audition_label(&self) -> String {
        self.compare
            .audition
            .as_ref()
            .map_or_else(String::new, |a| match a.info.done {
                0 => "the project as opened".into(),
                _ => format!("just after ‘{}’", a.info.label),
            })
    }

    /// The undo history's audition (the latest asked for).
    pub fn audition_info(&self) -> Option<AuditionInfo> {
        let a = self.compare.audition.as_ref()?;
        let mut info = a.info.clone();
        info.progress = a.job.as_ref().map(|j| {
            let now = j.now.as_ref().map_or(1.0, |n| n.progress.fraction());
            0.5 * (j.then.progress.fraction() + now)
        });
        Some(info)
    }

    /// An audition plays as loud as the mix.
    pub fn audition_matches_loudness(&self) -> bool {
        self.compare.match_audition
    }

    /// The stretch an audition renders and compares: the loop when it is
    /// on, else the edit selection, else sixteen bars from two bars before
    /// the playhead.
    pub fn audition_range(&self) -> (MusicalTime, MusicalTime) {
        let p = &self.project;
        if p.loop_enabled
            && let Some(r) = p.loop_range
            && r.end > r.start
        {
            return (r.start, r.end);
        }
        if let Some(r) = self.selection.range
            && r.end > r.start
        {
            return (r.start, r.end);
        }
        let meter = &p.timeline.meter;
        let bar = (meter.bar_at(self.playhead()) - 2).max(0);
        (meter.bar_start(bar), meter.bar_start(bar + 16))
    }

    /// The engine's compare controls (what is heard, the gain applied).
    pub fn compare_link(&self) -> &Arc<CompareLink> {
        self.engine.listening().link.compare()
    }

    // --- actions ------------------------------------------------------------------

    pub(crate) fn compare_op(&mut self, op: CompareOp) -> Result<()> {
        self.compare_follows_project();
        match op {
            CompareOp::Hear(on) => {
                if self.compare.what.is_none() {
                    return Err(SessionError::Other(
                        "nothing to compare with: audition a step of the history or choose a \
                         reference"
                            .into(),
                    ));
                }
                self.compare.hear = on;
            }
            CompareOp::Flip => {
                if self.compare.what.is_none() {
                    // Flipping with nothing chosen: the first reference.
                    match self.project.references.tracks.first().map(|r| r.id) {
                        Some(id) => {
                            self.choose_reference(Some(id))?;
                            self.compare.hear = true;
                        }
                        None => {
                            return Err(SessionError::Other(
                                "nothing to compare with: audition a step of the history or \
                                 add a reference"
                                    .into(),
                            ));
                        }
                    }
                } else {
                    self.compare.hear = !self.compare.hear;
                }
            }
            CompareOp::MatchLoudness(on) => {
                self.compare.match_audition = on;
                self.apply_audition_gain();
            }
            CompareOp::Off => self.compare_off(),
        }
        self.send_compare();
        self.revision += 1;
        Ok(())
    }

    /// The mix alone again (an audition's files go).
    pub(crate) fn compare_off(&mut self) {
        self.compare.what = None;
        self.compare.hear = false;
        self.compare.source = None;
        self.compare.dirty = true;
        self.compare.drop_audition();
        self.send_compare();
    }

    /// Set what plays on the other side (the engine takes it at the next
    /// poll — at once here).
    pub(crate) fn set_compare_source(&mut self, source: Option<CompareSource>) {
        let same = match (&self.compare.source, &source) {
            (Some(a), Some(b)) => Arc::ptr_eq(&a.source, &b.source) && a.settings == b.settings,
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.compare.source = source;
            self.compare.dirty = true;
            self.send_compare();
        }
    }

    pub(crate) fn set_compare_gain(&mut self, gain_db: f32, matched: bool) {
        self.compare.gain_db = gain_db;
        self.compare.matched = matched;
        self.send_compare();
    }

    /// Compare with `what` from now on (a different thing: its source is
    /// prepared anew).
    pub(crate) fn compare_with(&mut self, what: Comparing) {
        if self.compare.what != Some(what) {
            let history = |w: &Comparing| matches!(w, Comparing::History { .. });
            if self.compare.what.as_ref().is_some_and(history) && !history(&what) {
                self.compare.drop_audition();
            }
            // Another kind of comparison starts on the mix (another step or
            // another reference keeps the side heard).
            if self
                .compare
                .what
                .as_ref()
                .is_none_or(|w| history(w) != history(&what))
            {
                self.compare.hear = false;
            }
            self.compare.what = Some(what);
            self.compare.source = None;
            self.compare.dirty = true;
            self.compare.gain_db = 0.0;
            self.compare.matched = false;
        }
        self.compare.serial = self.project_serial;
    }

    pub(crate) fn comparing(&self) -> Option<Comparing> {
        self.compare.what
    }

    /// Hand the engine (and the disk loader) what is wanted — again to an
    /// engine made anew — and the switch and gain.
    fn send_compare(&mut self) {
        let link = Arc::clone(self.engine.listening().link.compare());
        let new_link = !self
            .compare
            .sent
            .as_ref()
            .is_some_and(|l| Arc::ptr_eq(l, &link));
        if new_link || self.compare.dirty {
            let wanted = self.compare.source.clone();
            self.loader.set_compare(
                wanted
                    .as_ref()
                    .map(|w| (Arc::clone(&w.source), Arc::clone(link.shared()))),
            );
            link.set(wanted);
            self.compare.sent = Some(Arc::clone(&link));
            self.compare.dirty = false;
        }
        let shared = link.shared();
        shared.hear(self.compare.hear && self.compare.source.is_some());
        shared.set_gain(db_to_gain(self.compare.gain_db));
        link.collect();
    }

    /// Render the project as it was with `done` steps done, and the mix as
    /// it is, over [`Self::audition_range`], to compare them.
    pub(crate) fn audition_history(&mut self, done: usize) -> Result<()> {
        self.compare_follows_project();
        let (steps, redo) = self.history_steps();
        let now = steps.len();
        if done > now + redo.len() {
            return Err(SessionError::Other(format!(
                "the history has no step {done}"
            )));
        }
        if done == now {
            return Err(SessionError::Other(
                "that is the project as it is now: choose an earlier step to audition".into(),
            ));
        }
        let label = match done {
            0 => "Project opened".to_string(),
            d if d <= now => steps[d - 1].clone(),
            d => redo[d - now - 1].clone(),
        };
        let range = self.audition_range();
        let rate = self.engine.sample_rate();
        let sr = f64::from(rate);
        let now_project = self.absolute_copy();
        let then_project = self.history.project_at(&now_project, done)?;
        // The stretch in the samples heard now, on each project's own
        // timeline (a tempo changed since moves the music, not the clock).
        let a = now_project.timeline.to_samples(range.0, sr);
        let b = now_project.timeline.to_samples(range.1, sr);
        let run_up = (AUDITION_RUN_UP_SECONDS * sr) as i64;
        let start = (a - run_up).max(0);
        let span_on = |p: &Project| {
            (
                p.timeline.to_musical(start, sr),
                p.timeline.to_musical(b, sr),
            )
        };
        let then_span = span_on(&then_project);
        let now_span = span_on(&now_project);
        // Where the render really starts (as `render::start` computes it).
        let anchor = then_project.timeline.to_samples(then_span.0, sr);
        let dir = self.compare.dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", dir.display())))?;
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let then_path = dir.join(format!("then-{n}.wav"));
        let now_path = dir.join(format!("now-{n}.wav"));
        let layout = master_layout(&now_project);
        let then_job = render::start(
            then_project.clone(),
            compare_render_settings(&then_project, then_span, rate, then_path.clone()),
        )
        .map_err(|e| SessionError::Other(e.to_string()))?;
        let now_job = render::start(
            now_project.clone(),
            compare_render_settings(&now_project, now_span, rate, now_path.clone()),
        )
        .map_err(|e| SessionError::Other(e.to_string()))?;
        // The last one goes (its files too).
        self.compare.drop_audition();
        self.compare_with(Comparing::History { done });
        // A new audition of another step: its source comes when rendered.
        self.compare.source = None;
        self.compare.dirty = true;
        self.compare.audition = Some(Audition {
            info: AuditionInfo {
                done,
                label: label.clone(),
                range,
                span: then_span,
                path: then_path,
                rate,
                progress: Some(0.0),
                then_lufs: None,
                now_lufs: None,
                error: None,
            },
            now_path,
            anchor,
            from: a - anchor,
            to: b - anchor,
            job: Some(AuditionJob {
                then: then_job,
                now: Some(now_job),
            }),
            layout,
        });
        self.send_compare();
        self.notify(
            NoticeLevel::Info,
            format!(
                "rendering the mix as it was {} to compare…",
                if done == 0 {
                    "when opened".to_string()
                } else {
                    format!("just after ‘{label}’")
                }
            ),
        );
        self.revision += 1;
        Ok(())
    }

    fn apply_audition_gain(&mut self) {
        let Some(a) = &self.compare.audition else {
            return;
        };
        if !matches!(self.compare.what, Some(Comparing::History { .. })) {
            return;
        }
        let gain = match_gain(a.info.now_lufs, a.info.then_lufs);
        match gain {
            Some(g) if self.compare.match_audition => self.set_compare_gain(g, true),
            _ => self.set_compare_gain(0.0, false),
        }
    }

    /// An audition's renders done: measured, and "Then" handed over.
    fn poll_audition(&mut self) {
        let Some(a) = self.compare.audition.as_mut() else {
            return;
        };
        let Some(job) = a.job.as_ref() else {
            return;
        };
        // The progress moves.
        self.revision += 1;
        if !(job.then.is_finished() && job.now.as_ref().is_none_or(RenderJob::is_finished)) {
            return;
        }
        let Some(job) = a.job.take() else {
            return;
        };
        let then = job.then.join();
        let now = job.now.map(RenderJob::join);
        let (rate, layout, from, to) = (a.info.rate, a.layout, a.from, a.to);
        // Measured over the stretch heard (the run-up left out).
        let measure = |path: &Path| -> Option<f64> {
            let (channels, r) = read_file(path).ok()?;
            let (from, to) = (from.max(0) as usize, to.max(0) as usize);
            let part: Vec<Vec<f32>> = channels
                .iter()
                .map(|c| c[from.min(c.len())..to.min(c.len())].to_vec())
                .collect();
            Some(integrated(&part, layout, if r == 0 { rate } else { r }))
        };
        let opened = then.map_err(|e| e.to_string()).and_then(|_| {
            StreamSource::open(&a.info.path).map_err(|e| format!("{}: {e}", a.info.path.display()))
        });
        let source = match opened {
            Ok(source) => {
                a.info.then_lufs = measure(&a.info.path);
                if let Some(Ok(_)) = &now {
                    a.info.now_lufs = measure(&a.now_path);
                }
                Some(CompareSource {
                    source,
                    settings: CompareSettings {
                        sync: CompareSync::Follow { anchor: a.anchor },
                        from,
                        to,
                    },
                })
            }
            Err(e) => {
                a.info.error = Some(e.clone());
                None
            }
        };
        let (done, error) = (a.info.done, a.info.error.clone());
        if let Some(e) = error {
            self.notify(NoticeLevel::Error, format!("audition: {e}"));
        } else if self.compare.what == Some(Comparing::History { done }) {
            self.set_compare_source(source);
            self.apply_audition_gain();
        }
        self.revision += 1;
    }

    /// The tick: jobs, a project replaced, an engine made anew, references
    /// changed, files to delete.
    pub(crate) fn poll_compare(&mut self) {
        self.compare_follows_project();
        self.poll_audition();
        self.poll_references();
        self.send_compare();
        let heard = self.compare_link().shared().heard();
        let moving = heard > 0.0 && heard < 1.0;
        if moving || self.compare.moving {
            self.shown_changed();
        }
        self.compare.moving = moving;
        if !self.compare.trash.is_empty() {
            self.compare.empty_trash();
        }
    }

    /// Another project took this one's place: nothing of the last one is
    /// compared, measured or imported any more.
    pub(crate) fn compare_follows_project(&mut self) {
        if self.compare.serial == self.project_serial {
            return;
        }
        self.compare.serial = self.project_serial;
        if self.compare.what.is_some() || self.compare.audition.is_some() {
            self.compare_off();
        }
        self.compare.references.cancel();
        self.compare.references = Default::default();
    }

    /// Wait until auditions and reference measurements are done (tests,
    /// scripts).
    pub fn wait_for_compare(&mut self, timeout: std::time::Duration) -> bool {
        let until = std::time::Instant::now() + timeout;
        loop {
            self.poll_compare();
            let busy = self
                .compare
                .audition
                .as_ref()
                .is_some_and(|a| a.job.is_some())
                || self.compare.references.busy();
            if !busy {
                return true;
            }
            if std::time::Instant::now() > until {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
