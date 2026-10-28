//! Auto-Align: microphones of one source — a drum kit's close, overhead
//! and room microphones, a guitar cabinet's close and room ones, a bass's
//! DI and microphone — set to hear it together: each track's own delay
//! (`Track::delay_ms`, to a fraction of a sample) and polarity, in one
//! undo step.
//!
//! The tracks' input-stage audio (their clips as they play, before any
//! device, fader or delay) is rendered over the analysed span — the edit
//! selection's range, else where the tracks' clips overlap, else all of
//! them — on a thread, and compared there
//! (`faderframe_analysis::align`). The reference is the user's choice,
//! else the track hearing the source first (the closest microphone); the
//! others are moved earlier by how much later they hear it, the reference
//! keeping its place (so the kit stays where the player played it), and
//! turned over where they hear it the other way up. Tracks without a
//! common signal are left alone. The window (`UiRequest::AutoAlign`)
//! shows what was found while it is open ([`Session::auto_align`]).

use crate::{Action, GroupMenuEntry, NoticeLevel, Result, Session, SessionError, UiRequest};
use faderframe_analysis::align::{AlignSettings, Analysis};
use faderframe_core::TrackId;
use faderframe_project::{Command, OutputRouting, Project, Track, TrackKind};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

/// Longest stretch analysed (seconds): longer spans are searched for
/// their busiest stretch of this length first.
pub const MAX_SECONDS: f64 = 30.0;

/// Default furthest delay searched (ms): about ten metres of air.
pub const DEFAULT_MAX_LAG_MS: f64 = 30.0;

/// What the Auto-Align window asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum AlignOp {
    /// Open the window for `tracks` (empty: the selected tracks) and
    /// analyse them, against `reference` (`None`: the first to hear it).
    Open {
        tracks: Vec<TrackId>,
        reference: Option<TrackId>,
    },
    /// Analyse the tracks again (the span, or the search, changed).
    Analyse,
    /// Align to this track (`None`: the first to hear the source).
    SetReference(Option<TrackId>),
    /// Include a track in what Apply sets, or leave it out.
    Include { track: TrackId, on: bool },
    /// The furthest delay searched either way (ms; analysing again uses
    /// it).
    SetMaxLag(f64),
    /// Set the delays and polarities found (one undo step); while still
    /// analysing, once it is done.
    Apply,
    /// The window closed: stop analysing, forget the findings.
    Close,
}

/// One track as the window lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignRow {
    pub track: TrackId,
    pub name: String,
    /// The tracks are aligned to this one.
    pub reference: bool,
    /// How much later it hears the source than the reference (ms;
    /// `None`: not analysed yet, or no common signal).
    pub lag_ms: Option<f64>,
    /// The same in samples at the project's rate.
    pub lag_samples: Option<f64>,
    /// It hears the source the other way up from the reference.
    pub inverted: bool,
    /// 0 … 1.
    pub confidence: f32,
    /// Found through the track hearing everything (the overheads) rather
    /// than against the reference directly.
    pub via_hub: bool,
    /// Silent over the analysed span.
    pub silent: bool,
    /// Apply sets it.
    pub include: bool,
    /// What Apply sets: its delay (ms) and polarity (`None`: left alone).
    pub delay_ms: Option<f64>,
    pub phase_invert: bool,
}

impl AlignRow {
    /// "+2.310 ms" (or "—").
    pub fn lag_text(&self) -> String {
        match self.lag_ms {
            _ if self.reference => "reference".into(),
            Some(ms) => format!("{ms:+.3} ms"),
            None if self.silent => "silent".into(),
            None => "—".into(),
        }
    }

    /// "+110.88" samples (or "").
    pub fn samples_text(&self) -> String {
        match self.lag_samples {
            Some(s) if !self.reference => format!("{s:+.2}"),
            _ => String::new(),
        }
    }

    /// "inverted" / "normal" (as found, against the reference).
    pub fn polarity_text(&self) -> String {
        if self.reference || self.lag_ms.is_none() {
            String::new()
        } else if self.inverted {
            "inverted".into()
        } else {
            "same".into()
        }
    }

    /// "97 %" (or "no common signal").
    pub fn confidence_text(&self) -> String {
        if self.reference {
            String::new()
        } else if self.lag_ms.is_none() && !self.silent && self.confidence < 1.0 {
            if self.confidence > 0.0 || self.lag_samples.is_none() {
                "no common signal".into()
            } else {
                String::new()
            }
        } else {
            format!("{:.0} %", self.confidence * 100.0)
        }
    }
}

/// The window's view of Auto-Align.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignView {
    /// The tracks analysed, in order.
    pub tracks: Vec<(TrackId, String)>,
    /// The reference chosen (`None`: the first to hear the source).
    pub reference: Option<TrackId>,
    /// The reference used.
    pub effective: Option<TrackId>,
    pub rows: Vec<AlignRow>,
    pub analysing: bool,
    /// 0 … 1 while analysing.
    pub progress: f32,
    /// What it is doing or found (a line under the list).
    pub status: String,
    pub max_lag_ms: f64,
    /// The span analysed (seconds from the project's start).
    pub span: Option<(f64, f64)>,
    /// Apply would change something.
    pub can_apply: bool,
    /// Changes whenever anything shown does.
    pub revision: u64,
}

/// Progress of a job: rendering (0 … 1000), then comparing.
#[derive(Default)]
struct Progress {
    render: AtomicU32,
    analysis: AtomicU32,
    cancel: AtomicBool,
}

impl Progress {
    fn fraction(&self) -> f32 {
        let r = self.render.load(Ordering::Relaxed) as f32 / 1000.0;
        let a = self.analysis.load(Ordering::Relaxed) as f32 / 1000.0;
        (0.4 * r + 0.6 * a).clamp(0.0, 1.0)
    }
}

/// What a job found.
struct Found {
    analysis: Analysis,
    /// The span analysed (project frames).
    span: (i64, i64),
}

struct Job {
    handle: JoinHandle<std::result::Result<Found, String>>,
    progress: Arc<Progress>,
}

/// The open Auto-Align window's state.
pub(crate) struct Aligning {
    tracks: Vec<TrackId>,
    reference: Option<TrackId>,
    max_lag_ms: f64,
    job: Option<Job>,
    found: Option<Found>,
    /// Rows left out (or put back) by the user.
    include: HashMap<TrackId, bool>,
    error: Option<String>,
    apply_when_done: bool,
    revision: u64,
}

#[derive(Default)]
pub(crate) struct AlignState {
    open: Option<Aligning>,
}

impl Session {
    /// Auto-Align as its window shows it (`None`: not open).
    pub fn auto_align(&self) -> Option<AlignView> {
        let a = self.auto_align.open.as_ref()?;
        let rate = f64::from(self.project.sample_rate.max(1));
        let tracks: Vec<(TrackId, String)> = a
            .tracks
            .iter()
            .filter_map(|id| self.project.track(*id).map(|t| (*id, t.name.clone())))
            .collect();
        let effective = self.align_reference(a);
        let rows = self.align_rows(a);
        let analysing = a.job.is_some();
        let progress = a.job.as_ref().map_or(0.0, |j| j.progress.fraction());
        let can_apply = rows.iter().any(|r| self.row_changes(r));
        let status = if analysing {
            format!("Analysing {} tracks…", a.tracks.len())
        } else if let Some(e) = &a.error {
            e.clone()
        } else if a.found.is_some() {
            let aligned = rows
                .iter()
                .filter(|r| r.lag_ms.is_some() && !r.reference)
                .count();
            let unrelated = rows.iter().filter(|r| r.lag_ms.is_none()).count();
            let name = |id: Option<TrackId>| {
                id.and_then(|i| self.project.track(i))
                    .map_or_else(|| "?".into(), |t| format!("‘{}’", t.name))
            };
            let mut s = format!(
                "{aligned} track{} found against {}",
                if aligned == 1 { "" } else { "s" },
                name(effective)
            );
            if unrelated > 0 {
                s.push_str(&format!(
                    "; {unrelated} without a common signal (left alone)"
                ));
            }
            s.push_str(". Apply moves each ticked track earlier by its delay and sets its polarity, in one undo step.");
            s
        } else {
            String::new()
        };
        Some(AlignView {
            tracks,
            reference: a.reference,
            effective,
            rows,
            analysing,
            progress,
            status,
            max_lag_ms: a.max_lag_ms,
            span: a
                .found
                .as_ref()
                .map(|f| (f.span.0 as f64 / rate, f.span.1 as f64 / rate)),
            can_apply,
            revision: a.revision,
        })
    }

    /// Auto-Align is analysing.
    pub fn auto_aligning(&self) -> bool {
        self.auto_align
            .open
            .as_ref()
            .is_some_and(|a| a.job.is_some())
    }

    /// Wait for the analysis (tests, scripts).
    pub fn wait_for_auto_align(&mut self) {
        let start = std::time::Instant::now();
        while self.auto_aligning() && start.elapsed().as_secs() < 300 {
            self.poll_auto_align();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// The track menus' entries: Auto-Align for audio tracks (the
    /// selected ones with this one), the delay's reset when it has one.
    pub fn auto_align_menu(&self, track: TrackId) -> Vec<GroupMenuEntry> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if t.kind == TrackKind::Audio {
            let tracks: Vec<TrackId> = if self.selection.tracks.contains(&track) {
                self.selection.tracks.iter().copied().collect()
            } else {
                vec![track]
            };
            out.push(GroupMenuEntry {
                label: "Auto-Align Microphones…".into(),
                action: Some(Action::AutoAlign(AlignOp::Open {
                    tracks,
                    reference: None,
                })),
                checked: None,
                separated: true,
            });
        }
        if t.delay_ms != 0.0 {
            out.push(GroupMenuEntry {
                label: format!("Reset Track Delay ({:+.3} ms)", t.delay_ms),
                action: Some(Action::Edit(Command::SetTrackDelay { track, ms: 0.0 })),
                checked: None,
                separated: out.is_empty(),
            });
        }
        out
    }

    pub(crate) fn auto_align_op(&mut self, op: AlignOp) -> Result<()> {
        match op {
            AlignOp::Open { tracks, reference } => {
                let tracks = if tracks.is_empty() {
                    self.selection.tracks.iter().copied().collect()
                } else {
                    tracks
                };
                // Audio tracks in the editors' order.
                let mut tracks: Vec<TrackId> = self
                    .project
                    .folder_order()
                    .into_iter()
                    .filter(|t| t.kind == TrackKind::Audio && tracks.contains(&t.id))
                    .map(|t| t.id)
                    .collect();
                tracks.dedup();
                let max_lag_ms = self
                    .auto_align
                    .open
                    .as_ref()
                    .map_or(DEFAULT_MAX_LAG_MS, |a| a.max_lag_ms);
                self.close_auto_align();
                self.auto_align.open = Some(Aligning {
                    tracks,
                    reference: reference.filter(|r| self.project.track(*r).is_some()),
                    max_lag_ms,
                    job: None,
                    found: None,
                    include: HashMap::new(),
                    error: None,
                    apply_when_done: false,
                    revision: 1,
                });
                self.ui_requests.push(UiRequest::AutoAlign);
                self.start_auto_align();
            }
            AlignOp::Analyse => self.start_auto_align(),
            AlignOp::SetReference(reference) => {
                if let Some(a) = self.auto_align.open.as_mut() {
                    a.reference = reference;
                    a.revision += 1;
                }
            }
            AlignOp::Include { track, on } => {
                if let Some(a) = self.auto_align.open.as_mut() {
                    a.include.insert(track, on);
                    a.revision += 1;
                }
            }
            AlignOp::SetMaxLag(ms) => {
                if let Some(a) = self.auto_align.open.as_mut() {
                    a.max_lag_ms = ms.clamp(1.0, 200.0);
                    a.revision += 1;
                }
            }
            AlignOp::Apply => {
                let Some(a) = self.auto_align.open.as_mut() else {
                    return Ok(());
                };
                if a.job.is_some() {
                    a.apply_when_done = true;
                } else {
                    self.apply_auto_align()?;
                }
            }
            AlignOp::Close => self.close_auto_align(),
        }
        self.revision += 1;
        Ok(())
    }

    fn close_auto_align(&mut self) {
        if let Some(a) = self.auto_align.open.take()
            && let Some(job) = a.job
        {
            job.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Start (or restart) analysing the open window's tracks.
    fn start_auto_align(&mut self) {
        let Some(tracks) = self.auto_align.open.as_ref().map(|a| a.tracks.clone()) else {
            return;
        };
        let span = self.align_span(&tracks);
        let fail = |s: &mut Self, e: String| {
            if let Some(a) = s.auto_align.open.as_mut() {
                a.error = Some(e);
                a.found = None;
                a.revision += 1;
            }
        };
        if tracks.len() < 2 {
            return fail(
                self,
                "Auto-Align needs two or more audio tracks: select the microphones of one source \
                 (with their clips) and choose it again."
                    .into(),
            );
        }
        let Some(span) = span else {
            return fail(self, "The tracks have no clips to analyse.".into());
        };
        let max_lag_ms = self
            .auto_align
            .open
            .as_ref()
            .map_or(DEFAULT_MAX_LAG_MS, |a| a.max_lag_ms);
        let project = self.absolute_copy().into_song();
        let progress = Arc::new(Progress::default());
        let p = Arc::clone(&progress);
        let settings = AlignSettings {
            max_lag: max_lag_ms / 1000.0,
            ..AlignSettings::default()
        };
        let handle = std::thread::Builder::new()
            .name("faderframe-auto-align".into())
            .spawn(move || analyse(&project, &tracks, span, &settings, &p));
        let Some(a) = self.auto_align.open.as_mut() else {
            return;
        };
        if let Some(old) = a.job.take() {
            old.progress.cancel.store(true, Ordering::Relaxed);
        }
        match handle {
            Ok(handle) => {
                a.job = Some(Job { handle, progress });
                a.error = None;
            }
            Err(e) => a.error = Some(e.to_string()),
        }
        a.revision += 1;
    }

    /// Take a finished analysis (from the session tick).
    pub(crate) fn poll_auto_align(&mut self) {
        let Some(a) = self.auto_align.open.as_mut() else {
            return;
        };
        let Some(job) = a.job.as_ref() else {
            return;
        };
        if !job.handle.is_finished() {
            // The progress bar moves.
            a.revision += 1;
            return;
        }
        let Some(job) = a.job.take() else {
            return;
        };
        match job.handle.join() {
            Ok(Ok(found)) => {
                a.found = Some(found);
                a.error = None;
            }
            Ok(Err(e)) => {
                a.found = None;
                a.error = Some(e);
            }
            Err(_) => {
                a.found = None;
                a.error = Some("the analysis failed".into());
            }
        }
        a.revision += 1;
        self.revision += 1;
        let apply = std::mem::take(&mut a.apply_when_done);
        if apply && let Err(e) = self.apply_auto_align() {
            self.notify(NoticeLevel::Error, format!("Auto-Align: {e}"));
        }
    }

    /// The span analysed for `tracks` (project frames): the edit
    /// selection's range, else where their clips overlap (half a second
    /// at least), else everything they play.
    fn align_span(&self, tracks: &[TrackId]) -> Option<(i64, i64)> {
        let sr = self.project.sample_rate;
        let rate = f64::from(sr.max(1));
        let tl = &self.project.timeline;
        if let Some(r) = self.selection.range.filter(|r| r.end > r.start) {
            return Some((tl.to_samples(r.start, rate), tl.to_samples(r.end, rate)));
        }
        let extents: Vec<(i64, i64)> = tracks
            .iter()
            .filter_map(|id| {
                let clips = self.project.clips_of(*id);
                let a = clips.iter().map(|c| tl.to_samples(c.start, rate)).min()?;
                let b = clips
                    .iter()
                    .map(|c| tl.to_samples(c.end(tl, sr), rate))
                    .max()?;
                Some((a, b))
            })
            .collect();
        if extents.len() < 2 {
            return None;
        }
        let lo = extents.iter().map(|e| e.0).max()?;
        let hi = extents.iter().map(|e| e.1).min()?;
        if hi - lo >= (rate / 2.0) as i64 {
            return Some((lo, hi));
        }
        let a = extents.iter().map(|e| e.0).min()?;
        let b = extents.iter().map(|e| e.1).max()?;
        (b > a).then_some((a.max(0), b))
    }

    /// The reference used: the chosen one, else the first to hear it.
    fn align_reference(&self, a: &Aligning) -> Option<TrackId> {
        if let Some(r) = a.reference.filter(|r| a.tracks.contains(r)) {
            return Some(r);
        }
        let found = a.found.as_ref()?;
        found
            .analysis
            .earliest()
            .and_then(|i| a.tracks.get(i).copied())
            .or_else(|| a.tracks.first().copied())
    }

    fn align_rows(&self, a: &Aligning) -> Vec<AlignRow> {
        let rate = f64::from(self.project.sample_rate.max(1));
        let reference = self.align_reference(a);
        let ref_track = reference.and_then(|r| self.project.track(r));
        let found = a.found.as_ref();
        let lags = match (
            found,
            reference.and_then(|r| a.tracks.iter().position(|t| *t == r)),
        ) {
            (Some(f), Some(i)) => f.analysis.relative_to(i),
            _ => Vec::new(),
        };
        a.tracks
            .iter()
            .enumerate()
            .filter_map(|(i, id)| {
                let t = self.project.track(*id)?;
                let is_ref = Some(*id) == reference;
                let lag = lags.get(i);
                let lag_samples = lag.and_then(|l| l.lag);
                let silent = found.is_some_and(|f| f.analysis.silent.get(i) == Some(&true));
                let inverted = lag.is_some_and(|l| l.inverted);
                let include =
                    a.include.get(id).copied().unwrap_or(true) && lag_samples.is_some() && !is_ref;
                // Heard with the reference: its own delay less the lag, its
                // polarity the reference's (turned over when inverted).
                let (delay_ms, phase_invert) = match (lag_samples, ref_track) {
                    (Some(l), Some(r)) if !is_ref => (
                        Some(r.delay_ms - l / rate * 1000.0),
                        r.phase_invert != inverted,
                    ),
                    _ => (None, t.phase_invert),
                };
                Some(AlignRow {
                    track: *id,
                    name: t.name.clone(),
                    reference: is_ref,
                    lag_ms: lag_samples.map(|l| l / rate * 1000.0),
                    lag_samples,
                    inverted,
                    confidence: lag.map_or(0.0, |l| l.confidence),
                    via_hub: lag.is_some_and(|l| l.via_hub),
                    silent,
                    include,
                    delay_ms,
                    phase_invert,
                })
            })
            .collect()
    }

    /// Would applying `row` change its track?
    fn row_changes(&self, row: &AlignRow) -> bool {
        let Some(t) = self.project.track(row.track) else {
            return false;
        };
        row.include
            && (row.delay_ms.is_some_and(|d| (d - t.delay_ms).abs() > 1e-9)
                || row.phase_invert != t.phase_invert)
    }

    /// Set the delays and polarities found, as one undo step.
    fn apply_auto_align(&mut self) -> Result<()> {
        let Some(a) = self.auto_align.open.as_ref() else {
            return Ok(());
        };
        if a.found.is_none() {
            return Err(SessionError::Other(
                a.error
                    .clone()
                    .unwrap_or_else(|| "nothing analysed to apply".into()),
            ));
        }
        let rows = self.align_rows(a);
        let reference = self
            .align_reference(a)
            .and_then(|r| self.project.track(r))
            .map(|t| t.name.clone())
            .unwrap_or_default();
        let mut commands = Vec::new();
        let mut moved = Vec::new();
        for r in rows.iter().filter(|r| self.row_changes(r)) {
            let Some(t) = self.project.track(r.track) else {
                continue;
            };
            if let Some(ms) = r.delay_ms
                && (ms - t.delay_ms).abs() > 1e-9
            {
                commands.push(Command::SetTrackDelay { track: r.track, ms });
            }
            if r.phase_invert != t.phase_invert {
                commands.push(Command::SetTrackPhaseInvert {
                    track: r.track,
                    on: r.phase_invert,
                });
            }
            moved.push(format!(
                "‘{}’ {:+.3} ms{}",
                r.name,
                r.delay_ms.unwrap_or(t.delay_ms),
                if r.phase_invert {
                    ", polarity inverted"
                } else {
                    ""
                }
            ));
        }
        if commands.is_empty() {
            self.notify(
                NoticeLevel::Info,
                "Auto-Align: the tracks are aligned already".to_string(),
            );
            return Ok(());
        }
        self.batch("Auto-Align Microphones", commands)?;
        self.notify(
            NoticeLevel::Info,
            format!("Auto-Align to ‘{reference}’: {}", moved.join("; ")),
        );
        if let Some(a) = self.auto_align.open.as_mut() {
            a.revision += 1;
        }
        Ok(())
    }
}

/// The project rendering only `tracks`' input stages, each to device
/// outputs of its own: the tracks with their clips (and freezes), no
/// devices, fader, pan, delay or polarity; the master to outputs after
/// theirs. Returns it, each track's first output and width, and the
/// outputs in all.
fn input_stages(project: &Project, tracks: &[TrackId]) -> (Project, Vec<(usize, usize)>, usize) {
    let mut p = Project::new(project.name.clone(), project.sample_rate);
    p.timeline = project.timeline.clone();
    p.sources = project.sources.clone();
    p.ids = project.ids.clone();
    let mut outputs = Vec::new();
    let mut next = 0usize;
    let mut kept = Vec::new();
    for id in tracks {
        let Some(t) = project.track(*id) else {
            continue;
        };
        let mut t: Track = t.clone();
        t.inserts.clear();
        t.preamp = None;
        t.instrument = None;
        t.containers.clear();
        t.sends.clear();
        t.cue_sends.clear();
        t.automation.lanes.clear();
        t.modulators.clear();
        t.input = faderframe_project::InputRouting::None;
        t.monitor = faderframe_project::MonitorMode::Off;
        t.record_arm = false;
        t.volume_db = 0.0;
        t.pan = 0.0;
        t.pan_mode = faderframe_core::PanMode::Balance;
        t.mute = false;
        t.solo = false;
        t.phase_invert = false;
        t.delay_ms = 0.0;
        t.object = false;
        t.vca = None;
        t.group = None;
        t.folder = None;
        t.output = OutputRouting::Hardware {
            first_channel: next as u16,
        };
        let width = t.layout.channel_count().max(1);
        outputs.push((next, width));
        next += width;
        for c in &t.clips {
            if let Some(clip) = project.clips.get(c) {
                p.clips.insert(*c, clip.clone());
            }
        }
        kept.push(t);
    }
    // The original master (its id), out of the way and plain.
    if let Some(m) = project.tracks.iter().find(|t| t.kind == TrackKind::Master) {
        let mut m = m.clone();
        m.inserts.clear();
        m.preamp = None;
        m.containers.clear();
        m.automation.lanes.clear();
        m.modulators.clear();
        m.output = OutputRouting::Hardware {
            first_channel: next as u16,
        };
        p.tracks = vec![m];
    }
    let master_width = p.tracks.first().map_or(2, |m| m.layout.channel_count());
    p.tracks.extend(kept);
    (p, outputs, next + master_width)
}

/// Frames rendered per step (whole callbacks of [`BLOCK`]).
const BLOCK: usize = 1024;
const STEP: usize = 16 * BLOCK;

/// Render `frames` of the input stages from `from`, handing each step's
/// per-track mono mix to `take`. `progress` goes from `done` by the
/// share of `total`.
fn render_stages(
    project: &Project,
    sources: &faderframe_engine::SourceMap,
    (outputs, width): (&[(usize, usize)], usize),
    (from, frames): (i64, usize),
    progress: (&Progress, usize, usize),
    mut take: impl FnMut(usize, &[f32]),
) -> std::result::Result<(), String> {
    let rate = project.sample_rate;
    let config = faderframe_engine::EngineConfig {
        sample_rate: rate,
        max_block_size: BLOCK,
        measure_nodes: false,
        ..faderframe_engine::EngineConfig::default()
    };
    let mut r = faderframe_engine::offline::OfflineRenderer::new(
        project,
        sources,
        config,
        BLOCK,
        width.max(1),
    )
    .map_err(|e| e.to_string())?;
    let mut skip = r.controller.graph_stats().output_latency as usize;
    r.play_from(from).map_err(|e| e.to_string())?;
    let (p, done, total) = progress;
    let mut rendered = 0usize;
    let mut mono = Vec::with_capacity(STEP);
    while rendered < frames {
        if p.cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let chunk = r.render(STEP);
        let start = skip.min(STEP);
        skip -= start;
        let n = (STEP - start).min(frames - rendered);
        for (t, (first, w)) in outputs.iter().enumerate() {
            mono.clear();
            mono.extend((start..start + n).map(|i| {
                let sum: f32 = (0..*w)
                    .filter_map(|c| chunk.get(first + c).map(|ch| ch[i]))
                    .sum();
                sum / *w as f32
            }));
            take(t, &mono);
        }
        rendered += n;
        let share = (done + rendered) as f64 / total.max(1) as f64;
        p.render
            .store((share * 1000.0).min(1000.0) as u32, Ordering::Relaxed);
    }
    Ok(())
}

/// The analysis job: render the tracks' input stages over `span` (its
/// busiest stretch when long), compare them.
fn analyse(
    project: &Project,
    tracks: &[TrackId],
    span: (i64, i64),
    settings: &AlignSettings,
    progress: &Progress,
) -> std::result::Result<Found, String> {
    let (p, outputs, width) = input_stages(project, tracks);
    let rate = f64::from(p.sample_rate.max(1));
    let mut sources = faderframe_engine::render_generated_sources(&p, p.sample_rate);
    for (_, path, e) in crate::media::open_file_sources(&p, None, &mut sources) {
        tracing::warn!("auto-align: {}: {e}", path.display());
    }
    let frames = (span.1 - span.0).max(0) as usize;
    let longest = (MAX_SECONDS * rate) as usize;
    let (from, frames, done, total) = if frames > longest {
        // The busiest stretch: seconds where most tracks play, loudest.
        let second = rate as usize;
        let seconds = frames.div_ceil(second);
        let mut energy = vec![vec![0.0f64; seconds]; outputs.len()];
        let mut at = vec![0usize; outputs.len()];
        render_stages(
            &p,
            &sources,
            (&outputs, width),
            (span.0, frames),
            (progress, 0, frames + longest),
            |t, x| {
                for v in x {
                    let s = (at[t] / second).min(seconds - 1);
                    energy[t][s] += f64::from(*v).powi(2);
                    at[t] += 1;
                }
            },
        )?;
        let busy: Vec<f64> = (0..seconds)
            .map(|s| {
                energy
                    .iter()
                    .map(|e| {
                        let loudest = e.iter().copied().fold(1e-20, f64::max);
                        let r = e[s] / loudest;
                        if r > 1e-4 { 1.0 + r } else { 0.0 }
                    })
                    .sum()
            })
            .collect();
        let w = (MAX_SECONDS as usize).min(seconds);
        let best = (0..=seconds - w)
            .max_by(|a, b| {
                let sa: f64 = busy[*a..*a + w].iter().sum();
                let sb: f64 = busy[*b..*b + w].iter().sum();
                sa.total_cmp(&sb).then(b.cmp(a))
            })
            .unwrap_or(0);
        (
            span.0 + (best * second) as i64,
            longest,
            frames,
            frames + longest,
        )
    } else {
        (span.0, frames, 0, frames)
    };
    let mut audio = vec![Vec::with_capacity(frames); outputs.len()];
    render_stages(
        &p,
        &sources,
        (&outputs, width),
        (from, frames),
        (progress, done, total),
        |t, x| audio[t].extend_from_slice(x),
    )?;
    progress.render.store(1000, Ordering::Relaxed);
    let views: Vec<&[f32]> = audio.iter().map(Vec::as_slice).collect();
    let analysis =
        faderframe_analysis::align::analyse(&views, rate, settings, Some(&progress.analysis));
    Ok(Found {
        analysis,
        span: (from, from + frames as i64),
    })
}
