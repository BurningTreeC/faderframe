//! A/B listening: something heard instead of the mix in the monitor path
//! (`faderframe_engine::nodes::compare`), switched in an instant and in
//! step with the transport — never in a render, an export, a bounce or a
//! freeze, never on a meter.
//!
//! Two things are compared with the mix:
//!
//! * **The undo history** ([`Action::AuditionNode`], or
//!   [`Action::AuditionHistory`] by its place on the current line): the
//!   project as it is in any step of the history tree, made on a copy of
//!   the project as going there would make it
//!   ([`faderframe_project::History::project_at_node`]: neither the
//!   project nor the history moves), rendered offline on its own engine
//!   over [`Session::audition_range`] with two seconds of run-up (tails
//!   from before it sound), into a file in a per-session temporary folder.
//!   The mix as it is ("Now") is rendered over the same stretch beside it,
//!   to match the two in loudness. "Then" and "Now" then flip with
//!   [`CompareOp::Flip`].
//! * **Reference tracks** ([`crate::references`]).
//!
//! Audio Bisect ([`crate::bisect`]) uses the same path to play two states
//! of the history against each other, neither of them the mix.
//!
//! The session hands the engine the file and its settings
//! (`CompareLink::set`, crossfaded there), keeps its pages resident through
//! the disk loader, sets the gain (a loudness match: the other side is
//! brought to the mix's integrated loudness, EBU R128, measured offline on
//! both) and hands everything again to an engine made anew.
//!
//! **The listening gain** ([`Session::listening_gain`]) is the monitor
//! path's one gain, per side, and this module owns it: the other side's
//! (a reference or the past brought to the mix's loudness, plus a
//! reference's trim) and the mix's (Mix Intelligence playing a candidate
//! at the original's loudness: [`crate::mix`]). Both are fairness gains for
//! a comparison — heard only, never in a render, a file or a meter — and
//! shown beside the real level difference they make up for.

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
    /// The project as it is in step `node` of the history ("Then"; the mix
    /// is "Now").
    History { node: u64 },
    /// A reference track.
    Reference(ReferenceId),
    /// Two states of the history against each other (Audio Bisect).
    Bisect,
    /// The last merge of versions with another choice for one of its
    /// conflicts ("Other"; the merge as applied is the mix, see
    /// [`crate::version_merge`]).
    MergeChoice,
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

/// The listening gain: what the monitor path plays each side at (dB) —
/// fairness gains for comparisons, never in the project, a render or a
/// meter.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ListeningGain {
    /// The mix (a Mix Intelligence candidate at the original's loudness).
    pub mix_db: f32,
    /// The other side (a reference or the past, matched to the mix and
    /// trimmed).
    pub other_db: f32,
    /// The other side's is a loudness match.
    pub other_matched: bool,
}

/// An audition of the undo history, as the views show it.
#[derive(Clone, Debug, PartialEq)]
pub struct AuditionInfo {
    /// The step auditioned ([`faderframe_project::History::ROOT`]: the
    /// project as opened).
    pub node: u64,
    /// Its steps from the start (on the current line: the steps done
    /// there).
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
    /// What it is heard as ([`Comparing::History`] or
    /// [`Comparing::MergeChoice`]).
    what: Comparing,
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
    pub(crate) what: Option<Comparing>,
    /// The file heard on the other side and how (once prepared).
    pub(crate) source: Option<CompareSource>,
    /// Where it was handed (an engine made anew gets it again) and whether
    /// it changed since.
    sent: Option<Arc<CompareLink>>,
    dirty: bool,
    pub(crate) hear: bool,
    /// An audition played as loud as the mix.
    match_audition: bool,
    gain_db: f32,
    pub(crate) matched: bool,
    /// The mix side's listening gain (dB; Mix Intelligence's candidate at
    /// the original's loudness).
    mix_gain_db: f32,
    /// The project it belongs to (`Session::project_serial`).
    serial: u64,
    audition: Option<Audition>,
    pub(crate) references: crate::references::ReferenceState,
    /// The session's temporary folder (renders, resampled references).
    dir: Option<PathBuf>,
    /// File frames from where the source plays to other places in it the
    /// disk loader keeps resident too (Audio Bisect's other side).
    pub(crate) also: Vec<i64>,
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
            mix_gain_db: 0.0,
            serial: 0,
            audition: None,
            references: Default::default(),
            dir: None,
            also: Vec::new(),
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
        // A field's loudness is its omnidirectional W (SN3D: the
        // pressure at the listener); the other channels are directions.
        ChannelLayout::Ambisonic(o) => (0..o.channels())
            .map(|k| if k == 0 { 1.0 } else { 0.0 })
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

/// A stretch of the timeline as heard now: the range compared, in samples
/// `[a, b)`, rendered from `start` (the run-up before it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stretch {
    pub range: (MusicalTime, MusicalTime),
    pub a: i64,
    pub b: i64,
    pub start: i64,
    pub rate: u32,
}

/// A rendered state of the project over a [`Stretch`]: the file, where it
/// plays and the stretch heard in it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StateFile {
    pub path: PathBuf,
    /// Engine samples: the file's first frame plays at `anchor`; the
    /// stretch heard is file frames `[from, to)`.
    pub anchor: i64,
    pub from: i64,
    pub to: i64,
    /// The master's layout in that state (for its loudness).
    pub layout: ChannelLayout,
}

/// A state of the project being rendered over a stretch.
pub(crate) struct StateRender {
    pub job: RenderJob,
    pub file: StateFile,
    /// What is rendered, on the project's own timeline.
    pub span: (MusicalTime, MusicalTime),
}

/// Start rendering `project` (a state of the history, absolute paths) over
/// `stretch` into `path`: the same samples heard now on its own timeline (a
/// tempo changed since moves the music, not the clock).
pub(crate) fn render_state(
    project: Project,
    stretch: Stretch,
    path: PathBuf,
) -> Result<StateRender> {
    let sr = f64::from(stretch.rate);
    let span = (
        project.timeline.to_musical(stretch.start, sr),
        project.timeline.to_musical(stretch.b, sr),
    );
    // Where the render really starts (as `render::start` computes it).
    let anchor = project.timeline.to_samples(span.0, sr);
    let layout = master_layout(&project);
    let settings = compare_render_settings(&project, span, stretch.rate, path.clone());
    let job = render::start(project, settings).map_err(|e| SessionError::Other(e.to_string()))?;
    Ok(StateRender {
        job,
        file: StateFile {
            path,
            anchor,
            from: stretch.a - anchor,
            to: stretch.b - anchor,
            layout,
        },
        span,
    })
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
            Comparing::Bisect => {
                let (label, name) = self.bisect_heard();
                ("Mix", label, name)
            }
            Comparing::MergeChoice => ("Merged", "Other", self.audition_label()),
        };
        let progress = match what {
            Comparing::History { .. } | Comparing::MergeChoice => {
                self.audition_info().and_then(|a| a.progress)
            }
            Comparing::Reference(id) => self.reference_progress(id),
            Comparing::Bisect => self.bisect_progress(),
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
            .map_or_else(String::new, |a| {
                if a.what == Comparing::MergeChoice {
                    a.info.label.clone()
                } else if a.info.node == faderframe_project::History::ROOT {
                    "the project as opened".into()
                } else {
                    format!("just after ‘{}’", a.info.label)
                }
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

    /// The listening gain on each side now (see [`ListeningGain`]).
    pub fn listening_gain(&self) -> ListeningGain {
        ListeningGain {
            mix_db: self.compare.mix_gain_db,
            other_db: self.compare.gain_db,
            other_matched: self.compare.matched,
        }
    }

    /// The engine's compare controls (what is heard, the gain applied).
    pub fn compare_link(&self) -> &Arc<CompareLink> {
        self.engine.listening().link.compare()
    }

    // --- actions ------------------------------------------------------------------

    pub(crate) fn compare_op(&mut self, op: CompareOp) -> Result<()> {
        self.compare_follows_project();
        if self.compare.what == Some(Comparing::Bisect) {
            // Audio Bisect's two sides: A and B, not the mix and another.
            return self.bisect_compare_op(op);
        }
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
        self.compare.also.clear();
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

    /// The mix side's listening gain (dB; 0 = none). Kept within ±24 dB.
    pub(crate) fn set_listening_mix_gain(&mut self, db: f32) {
        let db = if db.is_finite() {
            db.clamp(-24.0, 24.0)
        } else {
            0.0
        };
        if self.compare.mix_gain_db != db {
            self.compare.mix_gain_db = db;
            self.send_compare();
        }
    }

    /// Hear the mix (not the other side), when something else is heard.
    pub(crate) fn hear_the_mix(&mut self) {
        if self.compare.hear {
            self.compare.hear = false;
            self.send_compare();
            self.revision += 1;
        }
    }

    /// Compare with `what` from now on (a different thing: its source is
    /// prepared anew).
    pub(crate) fn compare_with(&mut self, what: Comparing) {
        if self.compare.what != Some(what) {
            let history =
                |w: &Comparing| matches!(w, Comparing::History { .. } | Comparing::MergeChoice);
            if self.compare.what.as_ref().is_some_and(history) && !history(&what) {
                self.compare.drop_audition();
            }
            // Another kind of comparison starts on the mix (another step or
            // another reference keeps the side heard).
            let kind = |w: &Comparing| match w {
                Comparing::History { .. } => 0,
                Comparing::Reference(_) => 1,
                Comparing::Bisect => 2,
                Comparing::MergeChoice => 3,
            };
            if self
                .compare
                .what
                .as_ref()
                .is_none_or(|w| kind(w) != kind(&what))
            {
                self.compare.hear = false;
            }
            self.compare.what = Some(what);
            self.compare.source = None;
            self.compare.also.clear();
            self.compare.dirty = true;
            self.compare.gain_db = 0.0;
            self.compare.matched = false;
        }
        self.compare.serial = self.project_serial;
    }

    pub(crate) fn comparing(&self) -> Option<Comparing> {
        self.compare.what
    }

    /// Something heard in the mix's place that compares nothing (A/V
    /// calibration's beeps: [`crate::av_calibration`]), or the mix again
    /// (`None`). Only while nothing is compared.
    pub(crate) fn hear_instead(&mut self, source: Option<CompareSource>) {
        if self.compare.what.is_some() {
            return;
        }
        self.compare.hear = source.is_some();
        self.compare.source = source;
        self.compare.also.clear();
        self.compare.dirty = true;
        self.compare.gain_db = 0.0;
        self.compare.matched = false;
        self.send_compare();
    }

    /// Hand the engine (and the disk loader) what is wanted — again to an
    /// engine made anew — and the switch and gain.
    pub(crate) fn send_compare(&mut self) {
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
                self.compare.also.clone(),
            );
            link.set(wanted);
            self.compare.sent = Some(Arc::clone(&link));
            self.compare.dirty = false;
        }
        let shared = link.shared();
        shared.hear(self.compare.hear && self.compare.source.is_some());
        // The listening gain, both sides.
        shared.set_gain(db_to_gain(self.compare.gain_db));
        shared.set_mix_gain(db_to_gain(self.compare.mix_gain_db));
        link.collect();
    }

    /// Render the project as it was with `done` steps done (on the current
    /// line), and the mix as it is, over [`Self::audition_range`], to
    /// compare them.
    pub(crate) fn audition_history(&mut self, done: usize) -> Result<()> {
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
        let node = self.history.line_node(done);
        self.audition_node(node)
    }

    /// The stretch an audition (or Audio Bisect) renders, in the samples
    /// heard now: [`Self::audition_range`] with the run-up before it.
    pub(crate) fn audition_stretch(&self, now: &Project) -> Stretch {
        let range = self.audition_range();
        let rate = self.engine.sample_rate();
        let sr = f64::from(rate);
        let a = now.timeline.to_samples(range.0, sr);
        let b = now.timeline.to_samples(range.1, sr);
        let run_up = (AUDITION_RUN_UP_SECONDS * sr) as i64;
        Stretch {
            range,
            a,
            b,
            start: (a - run_up).max(0),
            rate,
        }
    }

    /// Render the project as it is in step `node` of the history (any
    /// branch), and the mix as it is, over [`Self::audition_range`], to
    /// compare them.
    pub(crate) fn audition_node(&mut self, node: u64) -> Result<()> {
        self.compare_follows_project();
        if !self.history.knows(node) && node != faderframe_project::History::ROOT {
            return Err(SessionError::Other(format!(
                "the history has no step {node}"
            )));
        }
        if node == self.history.current() {
            return Err(SessionError::Other(
                "that is the project as it is now: choose another step to audition".into(),
            ));
        }
        let label = self.history_step_label(node);
        let done = self.history.depth_of(node).unwrap_or(0);
        let now_project = self.absolute_copy();
        let then_project = self.history.project_at_node(&now_project, node)?;
        self.start_audition(
            Comparing::History { node },
            then_project,
            now_project,
            label.clone(),
            node,
            done,
        )?;
        self.notify(
            NoticeLevel::Info,
            format!(
                "rendering the mix as it was {} to compare…",
                if node == faderframe_project::History::ROOT {
                    "when opened".to_string()
                } else {
                    format!("just after ‘{label}’")
                }
            ),
        );
        Ok(())
    }

    /// Render the last merge with another choice for the conflict `key`,
    /// and the mix as it is (the merge as applied), to compare them.
    pub(crate) fn audition_merge_conflict(&mut self, key: &str) -> Result<()> {
        self.compare_follows_project();
        let (mut other, label) = self.merge_alternative(key)?;
        let now_project = self.absolute_copy();
        for s in other.sources.values_mut() {
            if let faderframe_project::SourceSpec::File { path, .. } = &mut s.spec
                && path.is_relative()
                && let Some(dir) = self.project_dir()
            {
                *path = dir.join(&*path);
            }
        }
        let node = self.history.current();
        let done = self.history.depth_of(node).unwrap_or(0);
        self.start_audition(
            Comparing::MergeChoice,
            other,
            now_project,
            label.clone(),
            node,
            done,
        )?;
        self.notify(
            NoticeLevel::Info,
            format!("rendering the merge with {label} to compare…"),
        );
        Ok(())
    }

    /// Render `then_project` and `now_project` over the audition's stretch
    /// and compare them as `what`.
    fn start_audition(
        &mut self,
        what: Comparing,
        then_project: Project,
        now_project: Project,
        label: String,
        node: u64,
        done: usize,
    ) -> Result<()> {
        let stretch = self.audition_stretch(&now_project);
        let dir = self.compare.dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", dir.display())))?;
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let layout = master_layout(&now_project);
        let then = render_state(then_project, stretch, dir.join(format!("then-{n}.wav")))?;
        let now = render_state(now_project, stretch, dir.join(format!("now-{n}.wav")))?;
        // The last one goes (its files too).
        self.compare.drop_audition();
        self.compare_with(what);
        // A new audition of another step: its source comes when rendered.
        self.compare.source = None;
        self.compare.dirty = true;
        self.compare.audition = Some(Audition {
            what,
            info: AuditionInfo {
                node,
                done,
                label: label.clone(),
                range: stretch.range,
                span: then.span,
                path: then.file.path.clone(),
                rate: stretch.rate,
                progress: Some(0.0),
                then_lufs: None,
                now_lufs: None,
                error: None,
            },
            now_path: now.file.path.clone(),
            anchor: then.file.anchor,
            from: then.file.from,
            to: then.file.to,
            job: Some(AuditionJob {
                then: then.job,
                now: Some(now.job),
            }),
            layout,
        });
        self.send_compare();
        self.revision += 1;
        Ok(())
    }

    fn apply_audition_gain(&mut self) {
        let Some(a) = &self.compare.audition else {
            return;
        };
        if !matches!(
            self.compare.what,
            Some(Comparing::History { .. } | Comparing::MergeChoice)
        ) {
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
        let (what, error) = (a.what, a.info.error.clone());
        if let Some(e) = error {
            self.notify(NoticeLevel::Error, format!("audition: {e}"));
        } else if self.compare.what == Some(what) {
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
        self.poll_bisect();
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
