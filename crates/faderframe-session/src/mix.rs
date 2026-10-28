//! Mix Intelligence in the session: the mix analysed offline, findings
//! with their evidence, experiments auditioned in the real engine and
//! applied as one undo step.
//!
//! **What is measured.** An analysis renders, on a thread of its own
//! (never the audio thread, never the live engine: renders build their
//! own, as exports do), the audio of each track over the range:
//!
//! * *Processed* (the default): each track as it reaches the master
//!   output alone — solo in place over a copy of the project, so its
//!   devices, automation, sidechains (keyed from tracks that stay muted),
//!   fader, pan, sends, buses, returns and master chain all play as in the
//!   mix — and the master itself (the whole mix, stereo; a surround master
//!   folded down as the device would). The engine's delay compensation
//!   places everything; renders take the graph's latency off the front, so
//!   every signal starts on the range's first sample: one time reference,
//!   no second compensation.
//! * *Source*: each track's clips as they play (clip gain, fades, warp,
//!   an instrument's notes through its MIDI effects and instrument) with
//!   no audio device after them, no fader, pan, sends, modulators or
//!   console: what the material is, not what the mix makes of it.
//!
//! Renders stream into [`faderframe_mix_intelligence::profile::ProfileBuilder`]
//! (a frame of audio held at a time, whatever the range's length); the
//! profiles go through the rules ([`faderframe_mix_intelligence::rules`]).
//! The live project is never touched to analyse it.
//!
//! **Caching and staleness.** Profiles are kept (up to [`CACHE_BYTES`],
//! least recently used out) under a key of the track, the signal, the
//! range, the rate, the analysis settings and a fingerprint of everything
//! in the project that can change the audio (its JSON without names,
//! colours, markers, sections, roles, solo, ids …: [`fingerprint`]).
//! Processed renders depend on the whole mix, so their fingerprint is the
//! project's; a source render's is its own one-track project's (changing
//! one track's devices re-renders only the processed profiles). Results
//! say which project serial, history revision and fingerprint they came
//! from: a job for a project that was replaced, or superseded by a newer
//! job (generation), is dropped; results whose project has changed since
//! are shown as stale, and their experiments refused until analysed again.
//!
//! **Auditioning.** An experiment is applied to the live project *without*
//! an undo step (its inverse kept here), so the engine plays it for real:
//! a new built-in EQ at the end of the track's devices, or the fader. A/B
//! switches the EQ's band between on and bypassed, or the fader between
//! its two values — parameter changes, instant, no rebuild. Meanwhile the
//! loudness of the master with and without it is measured offline (BS.1770
//! integrated, over the finding's time, up to [`MEASURE_SECONDS`]); with
//! matching on, the candidate is played that much louder or quieter on the
//! monitor only (`EngineController::set_monitor_trim`), the difference
//! shown, the project never changed by it. Anything else that edits the
//! project, undoes, saves or renders ends the audition first (the
//! candidate taken out again), so no undo step or file ever holds it;
//! edits of the candidate's own EQ stay part of the candidate. Apply takes
//! the candidate out and makes it again as one recorded step ("Mix
//! Intelligence: …"): undo restores the project as it was.

use crate::editing::{EditRange, ZoomRequest};
use crate::render::{RenderProgress, render_each};
use crate::{Action, NoticeLevel, Result, SelectMode, Session, SessionError};
use faderframe_automation::{AutomationMode, AutomationTarget};
use faderframe_core::ClipId;
use faderframe_core::{ChannelLayout, ParameterId, PluginInstanceId, TrackId, TrackRole, builtin};
use faderframe_mix_intelligence::profile::ProfileBuilder;
use faderframe_mix_intelligence::rules::{DiagnoseInput, TrackInput};
use faderframe_mix_intelligence::{
    AnalysisConfig, Experiment, Finding, Fnv, Report, Rules, SignalPoint, TimeSpan, TrackProfile,
};
use faderframe_plugin_host::eq::{Field, band_id};
use faderframe_project::{
    Command, Impact, InputRouting, OutputRouting, PluginRef, PluginSlot, Project, SavedParameter,
    Track, TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

pub use faderframe_mix_intelligence as intelligence;

/// The longest range analysed at once (seconds): longer ones are asked to
/// be analysed by selection.
pub const MAX_SECONDS: f64 = 20.0 * 60.0;
/// Profiles kept for reuse (bytes).
pub const CACHE_BYTES: usize = 256 << 20;
/// The longest stretch an audition's loudness is measured over (seconds).
pub const MEASURE_SECONDS: f64 = 30.0;
/// … and the shortest (BS.1770's gating needs some time).
const MEASURE_MIN_SECONDS: f64 = 4.0;
/// The built-in EQ's name as a slot names it.
const EQ_NAME: &str = "EQ";

/// What to analyse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixScope {
    /// Every track over the whole project.
    Project,
    /// Every track over the edit selection's time range.
    Selection,
    /// The selected tracks (against every other track), over the edit
    /// selection's range when it has one, else the whole project.
    SelectedTracks,
}

impl MixScope {
    pub fn label(self) -> &'static str {
        match self {
            MixScope::Project => "the whole project",
            MixScope::Selection => "the selected range",
            MixScope::SelectedTracks => "the selected tracks",
        }
    }
}

/// What the Mix Intelligence view (and its dev actions) ask.
#[derive(Clone, Debug, PartialEq)]
pub enum MixOp {
    Analyse {
        scope: MixScope,
        signal: SignalPoint,
    },
    Cancel,
    /// The finding the details show (`None`: none).
    Select(Option<u64>),
    /// Select a finding's tracks and its time on the arranger.
    Show(u64),
    /// …or one of its events.
    ShowEvent {
        finding: u64,
        event: usize,
    },
    /// Play an experiment in the real engine (the project unchanged).
    Audition {
        finding: u64,
        recommendation: usize,
    },
    /// While auditioning: `true` the candidate, `false` the original.
    Compare(bool),
    /// While auditioning: play the candidate loudness-matched.
    LoudnessMatch(bool),
    /// Make an experiment (the auditioned one as it is now) one undo step.
    Apply {
        finding: u64,
        recommendation: usize,
    },
    /// End the audition; the project as it was.
    Reject,
    /// Hide a finding (for this project; shown again with
    /// [`MixOp::ShowDismissed`]).
    Dismiss(u64),
    ShowDismissed(bool),
    /// Forget the results.
    Clear,
}

/// Where an analysis is.
#[derive(Clone, Debug, PartialEq)]
pub enum MixStatus {
    Idle,
    Running { what: String, fraction: f64 },
    Cancelled,
    Failed(String),
}

/// A finished analysis.
#[derive(Debug)]
pub struct MixResults {
    pub report: Report,
    pub signal: SignalPoint,
    pub scope: MixScope,
    /// The analysed range on the timeline and in samples.
    pub range: (MusicalTime, MusicalTime),
    pub start_sample: i64,
    pub frames: usize,
    pub rate: u32,
    /// Tracks analysed, and those of them in focus.
    pub tracks: Vec<TrackId>,
    pub focus: Vec<TrackId>,
    /// Tracks left out, and why.
    pub skipped: Vec<(TrackId, String)>,
    /// Tracks rendered now, and profiles reused from earlier analyses.
    pub rendered: usize,
    pub reused: usize,
    pub config: AnalysisConfig,
    pub algorithm: u32,
    /// What it was made from.
    pub serial: u64,
    pub history_revision: u64,
    pub fingerprint: Option<u64>,
}

impl MixResults {
    /// The timeline position `seconds` into the analysed range.
    pub fn position(&self, project: &Project, seconds: f64) -> MusicalTime {
        let rate = f64::from(self.rate.max(1));
        project
            .timeline
            .to_musical(self.start_sample + (seconds * rate).round() as i64, rate)
    }

    /// The analysed range's length (seconds).
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / f64::from(self.rate.max(1))
    }
}

/// The audition's loudness measurement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measured {
    pub original_lufs: f64,
    pub candidate_lufs: f64,
    /// The master's level change in the finding's frequency range (dB).
    pub band_change_db: Option<f64>,
    /// Seconds measured.
    pub seconds: f64,
}

impl Measured {
    /// Candidate minus original (LU).
    pub fn change(&self) -> f64 {
        self.candidate_lufs - self.original_lufs
    }
}

/// The audition, as views show it.
#[derive(Clone, Debug, PartialEq)]
pub struct AuditionInfo {
    pub finding: u64,
    pub recommendation: usize,
    /// The candidate is heard (else the original).
    pub candidate: bool,
    pub matching: bool,
    pub measuring: bool,
    pub measured: Option<std::result::Result<Measured, String>>,
    /// The monitor trim playing now (dB; 0 when none).
    pub trim_db: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    track: u64,
    signal: u8,
    project: u64,
    start: i64,
    frames: usize,
    rate: u32,
    config: u64,
    spans: u64,
}

struct Cache {
    entries: Vec<(CacheKey, Arc<TrackProfile>, u64)>,
    tick: u64,
    budget: usize,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            tick: 0,
            budget: CACHE_BYTES,
        }
    }
}

impl Cache {
    fn get(&mut self, key: &CacheKey) -> Option<Arc<TrackProfile>> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.iter_mut().find(|e| e.0 == *key).map(|e| {
            e.2 = tick;
            Arc::clone(&e.1)
        })
    }

    fn put(&mut self, key: CacheKey, p: Arc<TrackProfile>) {
        self.tick += 1;
        self.entries.retain(|e| e.0 != key);
        self.entries.push((key, p, self.tick));
        while self.bytes() > self.budget && self.entries.len() > 1 {
            if let Some(i) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.2)
                .map(|(i, _)| i)
            {
                self.entries.remove(i);
            }
        }
    }

    fn bytes(&self) -> usize {
        self.entries.iter().map(|e| e.1.bytes()).sum()
    }
}

/// What a planned render plays.
#[derive(Clone, Copy, Debug)]
enum Plays {
    /// The track solo in place through the mix.
    Solo,
    /// The track's source through its first `keep` devices.
    Source { keep: usize },
    /// The whole mix.
    Master,
}

/// One track (or the master) of a job. Its project (a copy per track)
/// and fingerprint are made on the job's thread, not the UI's.
struct Planned {
    track: TrackId,
    plays: Plays,
    name: String,
    role: Option<TrackRole>,
    focus: bool,
    spans: Vec<(usize, usize)>,
}

/// What every render of a job shares.
struct Shared {
    start: i64,
    frames: usize,
    rate: u32,
    signal: SignalPoint,
    config: AnalysisConfig,
    rules: Rules,
    sections: Vec<(String, TimeSpan)>,
    /// The cache as it was when the job started (profiles found there are
    /// not rendered again).
    cache: Vec<(CacheKey, Arc<TrackProfile>)>,
}

struct Progress {
    render: RenderProgress,
    track: AtomicU32,
    diagnosing: AtomicBool,
}

struct Output {
    report: Report,
    /// Profiles made now, and the keys of those reused.
    profiles: Vec<(CacheKey, Arc<TrackProfile>)>,
    reused: Vec<CacheKey>,
    rendered: usize,
    /// The analysed project's fingerprint ([`fingerprint`]).
    fingerprint: Option<u64>,
}

struct Job {
    generation: u64,
    serial: u64,
    revision: u64,
    scope: MixScope,
    signal: SignalPoint,
    range: (MusicalTime, MusicalTime),
    start: i64,
    frames: usize,
    rate: u32,
    config: AnalysisConfig,
    tracks: Vec<TrackId>,
    focus: Vec<TrackId>,
    skipped: Vec<(TrackId, String)>,
    names: Vec<String>,
    progress: Arc<Progress>,
    handle: JoinHandle<std::result::Result<Output, String>>,
}

/// How A/B switches a candidate.
#[derive(Clone, Copy, Debug)]
enum Toggle {
    Band {
        track: TrackId,
        plugin: PluginInstanceId,
        band: usize,
    },
    Volume {
        track: TrackId,
        original: f32,
        candidate: f32,
    },
}

struct MeasureJob {
    progress: Arc<RenderProgress>,
    handle: JoinHandle<std::result::Result<Measured, String>>,
}

struct Audition {
    finding: u64,
    recommendation: usize,
    serial: u64,
    /// Inverses of what was applied (in order of application).
    inverses: Vec<Command>,
    toggle: Toggle,
    candidate: bool,
    matching: bool,
    measure: Option<MeasureJob>,
    measured: Option<std::result::Result<Measured, String>>,
    trim_db: f64,
}

impl Audition {
    fn plugin(&self) -> Option<PluginInstanceId> {
        match self.toggle {
            Toggle::Band { plugin, .. } => Some(plugin),
            Toggle::Volume { .. } => None,
        }
    }
}

/// Mix Intelligence's state in the session.
pub(crate) struct MixState {
    generation: u64,
    job: Option<Job>,
    /// Cancelled or superseded jobs still finishing.
    retired: Vec<Job>,
    results: Option<Arc<MixResults>>,
    cache: Cache,
    status: MixStatus,
    selected: Option<u64>,
    dismissed: BTreeSet<u64>,
    show_dismissed: bool,
    applied: BTreeSet<(u64, usize)>,
    audition: Option<Audition>,
    /// The history revision staleness was last checked at.
    checked: Option<u64>,
    stale: Option<String>,
    serial: u64,
    config: Option<AnalysisConfig>,
    rules: Rules,
}

impl Default for MixState {
    fn default() -> Self {
        Self {
            generation: 0,
            job: None,
            retired: Vec::new(),
            results: None,
            cache: Cache::default(),
            status: MixStatus::Idle,
            selected: None,
            dismissed: BTreeSet::new(),
            show_dismissed: false,
            applied: BTreeSet::new(),
            audition: None,
            checked: None,
            stale: None,
            serial: 0,
            config: None,
            rules: Rules::default(),
        }
    }
}

impl MixState {
    pub(crate) fn auditioning(&self) -> bool {
        self.audition.is_some()
    }
}

/// Top-level project fields that do not change what any track sounds like.
const PRESENTATION: [&str; 19] = [
    "name",
    "markers",
    "loop_range",
    "loop_enabled",
    "punch_range",
    "punch_enabled",
    "play_start",
    "midi_mappings",
    "sections",
    "groups",
    "album",
    "lyrics",
    "adr",
    "setlist",
    "scratch_pads",
    "clip_links",
    "video",
    "timecode",
    "ids",
];
/// Track fields that do not change what it sounds like in a render.
const TRACK_PRESENTATION: [&str; 7] = [
    "name",
    "color",
    "solo",
    "role",
    "record_arm",
    "monitor",
    "group",
];

/// What an analysis of `project` depends on, as a number: its JSON with
/// the presentation left out (names, colours, markers, sections, roles,
/// solo, ids …). `None` when it cannot be serialised (nothing is cached
/// then).
pub fn fingerprint(project: &Project) -> Option<u64> {
    let mut v = serde_json::to_value(project).ok()?;
    if let Some(o) = v.as_object_mut() {
        for k in PRESENTATION {
            o.remove(k);
        }
        if let Some(tracks) = o.get_mut("tracks").and_then(|t| t.as_array_mut()) {
            for t in tracks.iter_mut().filter_map(|t| t.as_object_mut()) {
                for k in TRACK_PRESENTATION {
                    t.remove(k);
                }
            }
        }
        if let Some(clips) = o.get_mut("clips").and_then(|c| c.as_object_mut()) {
            for c in clips.values_mut().filter_map(|c| c.as_object_mut()) {
                c.remove("name");
                c.remove("color");
            }
        }
    }
    let bytes = serde_json::to_vec(&v).ok()?;
    let mut h = Fnv::new();
    h.write(&bytes);
    Some(h.finish())
}

/// Something of `track` sounds between `a` and `b`: a clip, a freeze, a
/// MIDI track playing it, a plugin's extra output.
fn plays_in(p: &Project, t: &Track, a: MusicalTime, b: MusicalTime) -> bool {
    let clip_in = |id: &ClipId| {
        p.clip(*id)
            .is_some_and(|c| !c.muted && c.start < b && c.end(&p.timeline, p.sample_rate) > a)
    };
    t.clips.iter().any(clip_in)
        || t.freeze.is_some()
        || matches!(t.input, InputRouting::Plugin { .. })
        || p.tracks.iter().any(|m| {
            m.kind == TrackKind::Midi
                && m.output == OutputRouting::Track { track: t.id }
                && m.clips.iter().any(clip_in)
        })
}

/// `track` as its source sounds (see the module docs): alone, its clips
/// (and the MIDI tracks playing it) through only its first `keep` devices
/// (MIDI effects and instrument) into a plain master, mono when the track
/// is. The channels to render.
fn source_project(base: &Project, track: TrackId, keep: usize) -> Option<(Project, usize)> {
    let t = base.track(track)?;
    let mono = t.layout == ChannelLayout::Mono && keep == 0;
    let master = base.master_id()?;
    let feeders: BTreeSet<TrackId> = base
        .tracks
        .iter()
        .filter(|m| m.kind == TrackKind::Midi && m.output == OutputRouting::Track { track })
        .map(|m| m.id)
        .collect();
    let mut p = base.clone();
    p.tracks
        .retain(|t| t.id == track || t.id == master || feeders.contains(&t.id));
    // Only what the kept tracks play: another track's clip edited leaves
    // this render's fingerprint alone.
    let kept: BTreeSet<TrackId> = p.tracks.iter().map(|t| t.id).collect();
    p.clips.retain(|_, c| kept.contains(&c.track));
    p.launcher = Default::default();
    p.scratch_pads = Default::default();
    p.cue_mixes.clear();
    let used = p.used_sources();
    p.sources.retain(|id, _| used.contains(id));
    p.console = None;
    p.groups.clear();
    for t in &mut p.tracks {
        t.vca = None;
        t.folder = None;
        t.group = None;
        t.mute = false;
        t.solo = t.id == track;
        if t.id == master {
            if mono {
                t.layout = ChannelLayout::Mono;
            }
            t.volume_db = 0.0;
            t.pan = 0.0;
            t.pan_mode = faderframe_core::PanMode::Balance;
            t.phase_invert = false;
            t.inserts.clear();
            t.preamp = None;
            t.containers.clear();
            t.modulators.clear();
            t.automation.lanes.clear();
        } else if t.id == track {
            t.volume_db = 0.0;
            t.pan = 0.0;
            t.pan_mode = faderframe_core::PanMode::Balance;
            t.dual_pan = faderframe_core::DualPan::ORIGINAL;
            t.phase_invert = false;
            t.freeze = None;
            t.sends.clear();
            t.cue_sends.clear();
            t.preamp = None;
            t.inserts.truncate(keep);
            let kept: BTreeSet<PluginInstanceId> = t.inserts.iter().map(|s| s.id).collect();
            t.containers.retain(|id, _| kept.contains(id));
            t.modulators.clear();
            t.output = OutputRouting::Track { track: master };
            t.automation.lanes.retain(|l| match l.target {
                AutomationTarget::PluginParameter { plugin, .. }
                | AutomationTarget::PluginBypass(plugin) => kept.contains(&plugin),
                _ => false,
            });
        }
    }
    let outputs = if mono { 1 } else { 2 };
    Some((p, outputs))
}

/// The built-in EQ with `params` set, as a new slot.
fn eq_slot(
    project: &mut Project,
    params: &[(ParameterId, f64)],
    sidechain: Option<TrackId>,
) -> PluginSlot {
    PluginSlot {
        id: project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::EQ, EQ_NAME),
        bypass: false,
        parameters: params
            .iter()
            .map(|&(id, value)| SavedParameter { id, value })
            .collect(),
        state: None,
        sidechain,
    }
}

fn job_worker(
    planned: Vec<Planned>,
    base: Project,
    shared: Shared,
    progress: Arc<Progress>,
) -> std::result::Result<Output, String> {
    let Shared {
        start,
        frames,
        rate,
        signal,
        config,
        rules,
        sections,
        cache,
    } = shared;
    let mut sources = faderframe_engine::render_generated_sources(&base, rate);
    for (_, path, e) in crate::media::open_file_sources(&base, None, &mut sources) {
        tracing::warn!("mix analysis: {}: {e}", path.display());
    }
    let base_fp = fingerprint(&base);
    let key = |track: u64, project: Option<u64>, spans: &[(usize, usize)]| {
        let mut h = Fnv::new();
        for (x, y) in spans {
            h.write_u64(*x as u64);
            h.write_u64(*y as u64);
        }
        project.map(|project| CacheKey {
            track,
            signal: signal as u8,
            project,
            start,
            frames,
            rate,
            config: config.fingerprint(),
            spans: h.finish(),
        })
    };
    let mut profiles: Vec<Arc<TrackProfile>> = Vec::with_capacity(planned.len());
    let (mut made, mut reused) = (Vec::new(), Vec::new());
    let mut rendered = 0;
    for (i, p) in planned.iter().enumerate() {
        progress.track.store(i as u32, Ordering::Relaxed);
        if progress.render.cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        // What it plays, and the fingerprint of that.
        let (project, outputs, fp, id) = match p.plays {
            Plays::Solo => {
                let mut x = base.clone();
                for t in &mut x.tracks {
                    t.solo = t.id == p.track;
                }
                (Some(x), 2, base_fp, p.track.0)
            }
            Plays::Master => (None, 2, base_fp, u64::MAX),
            Plays::Source { keep } => match source_project(&base, p.track, keep) {
                Some((x, outputs)) => {
                    let fp = fingerprint(&x);
                    (Some(x), outputs, fp, p.track.0)
                }
                None => return Err(format!("{}: no master to render through", p.name)),
            },
        };
        let k = key(id, fp, &p.spans);
        if let Some(found) = k.and_then(|k| cache.iter().find(|e| e.0 == k)) {
            reused.push(found.0);
            profiles.push(Arc::clone(&found.1));
            continue;
        }
        let project = project.as_ref().unwrap_or(&base);
        let mut builder =
            ProfileBuilder::new(rate, outputs, &config, &p.spans).map_err(|e| e.to_string())?;
        let mut failed = None;
        render_each(
            project,
            rate,
            &sources,
            (start, frames),
            &progress.render,
            outputs,
            None,
            false,
            &mut |piece| {
                if failed.is_none()
                    && let Err(e) = builder.push(piece)
                {
                    failed = Some(e);
                }
            },
        )
        .map_err(|e| e.to_string())?;
        if let Some(e) = failed {
            return Err(e.to_string());
        }
        let profile = Arc::new(builder.finish());
        if let Some(k) = k {
            made.push((k, Arc::clone(&profile)));
        }
        profiles.push(profile);
        rendered += 1;
    }
    progress.diagnosing.store(true, Ordering::Relaxed);
    let mut tracks = Vec::new();
    let mut master = None;
    for (p, prof) in planned.iter().zip(&profiles) {
        match p.plays {
            Plays::Master => master = Some((Some(p.track), prof.as_ref())),
            _ => tracks.push(TrackInput {
                track: p.track,
                role: p.role,
                profile: prof,
                compare: true,
                focus: p.focus,
            }),
        }
    }
    let input = DiagnoseInput {
        tracks,
        master,
        signal,
        sections,
        rules: &rules,
        cancel: Some(&progress.render.cancel),
    };
    let report = faderframe_mix_intelligence::diagnose(&input).map_err(|e| e.to_string())?;
    Ok(Output {
        report,
        profiles: made,
        reused,
        rendered,
        fingerprint: base_fp,
    })
}

fn measure_worker(
    projects: [Project; 2],
    (start, frames, rate): (i64, usize, u32),
    band: Option<(f64, f64)>,
    progress: Arc<RenderProgress>,
) -> std::result::Result<Measured, String> {
    let mut sources = faderframe_engine::render_generated_sources(&projects[0], rate);
    for (_, path, e) in crate::media::open_file_sources(&projects[0], None, &mut sources) {
        tracing::warn!("mix audition: {}: {e}", path.display());
    }
    let config = AnalysisConfig::for_rate(rate);
    let mut out = Vec::new();
    for p in &projects {
        let mut builder = ProfileBuilder::new(rate, 2, &config, &[]).map_err(|e| e.to_string())?;
        render_each(
            p,
            rate,
            &sources,
            (start, frames),
            &progress,
            2,
            None,
            false,
            &mut |piece| {
                let _ = builder.push(piece);
            },
        )
        .map_err(|e| e.to_string())?;
        out.push(builder.finish());
    }
    let lufs = |p: &TrackProfile| {
        p.levels
            .loudness
            .map(|l| l.integrated)
            .filter(|v| v.is_finite())
    };
    let (Some(a), Some(b)) = (lufs(&out[0]), lufs(&out[1])) else {
        return Err("the master is silent there: nothing to match".into());
    };
    let band_change_db = band.map(|(lo, hi)| {
        let power = |p: &TrackProfile| -> f64 {
            let mean = p.mean_powers(None);
            p.layout
                .bands()
                .iter()
                .zip(&mean)
                .filter(|(b, _)| b.centre >= lo && b.centre < hi)
                .map(|(_, v)| *v)
                .sum()
        };
        faderframe_mix_intelligence::profile::power_db(power(&out[1]))
            - faderframe_mix_intelligence::profile::power_db(power(&out[0]))
    });
    Ok(Measured {
        original_lufs: a,
        candidate_lufs: b,
        band_change_db,
        seconds: frames as f64 / f64::from(rate.max(1)),
    })
}

/// Actions that leave an audition playing (they do not edit the project).
fn keeps_audition(action: &Action) -> bool {
    if let Action::Several(all) = action {
        return all.iter().all(keeps_audition);
    }
    matches!(
        action,
        Action::Mix(_)
            | Action::Transport(_)
            | Action::Workspace(_)
            | Action::SelectTracks { .. }
            | Action::SelectClips { .. }
            | Action::SelectNotes { .. }
            | Action::ClearSelection
            | Action::Zoom(_)
            | Action::SetEditRange(_)
            | Action::SetMonoCheck(_)
            | Action::SetHeadphones(_)
            | Action::OpenPluginEditor { .. }
            | Action::SetDeviceView { .. }
            | Action::SetAnalysisSource(_)
            | Action::ResetAnalysis
            | Action::ShowGlobalLane(..)
            | Action::SetGrid(_)
            | Action::ToggleSnap
            | Action::SetStripWidth { .. }
            | Action::AuditionOff
            | Action::ResetPerformance
    )
}

impl Session {
    // --- reading ---------------------------------------------------------

    pub fn mix_results(&self) -> Option<&MixResults> {
        self.mix.results.as_deref()
    }

    /// Where the analysis is (with progress while it runs).
    pub fn mix_status(&self) -> MixStatus {
        let Some(j) = &self.mix.job else {
            return self.mix.status.clone();
        };
        let p = &j.progress;
        if p.render.cancel.load(Ordering::Relaxed) {
            return MixStatus::Running {
                what: "Cancelling…".into(),
                fraction: p.render.fraction(),
            };
        }
        if p.diagnosing.load(Ordering::Relaxed) {
            return MixStatus::Running {
                what: "Comparing the tracks…".into(),
                fraction: 1.0,
            };
        }
        let i = p.track.load(Ordering::Relaxed) as usize;
        let n = j.names.len().max(1);
        MixStatus::Running {
            what: format!(
                "Rendering {} ({} of {n})",
                j.names.get(i).map_or("", String::as_str),
                (i + 1).min(n)
            ),
            fraction: p.render.fraction(),
        }
    }

    pub fn mix_analysing(&self) -> bool {
        self.mix.job.is_some()
    }

    /// Why the results no longer describe the project (`None`: they do).
    pub fn mix_stale(&self) -> Option<&str> {
        self.mix.stale.as_deref()
    }

    pub fn mix_selected(&self) -> Option<&Finding> {
        let id = self.mix.selected?;
        self.mix_results()?
            .report
            .findings
            .iter()
            .find(|f| f.id == id)
    }

    pub fn mix_selected_id(&self) -> Option<u64> {
        self.mix.selected
    }

    pub fn mix_finding(&self, id: u64) -> Option<&Finding> {
        self.mix_results()?
            .report
            .findings
            .iter()
            .find(|f| f.id == id)
    }

    pub fn mix_dismissed(&self, id: u64) -> bool {
        self.mix.dismissed.contains(&id)
    }

    pub fn mix_dismissed_count(&self) -> usize {
        self.mix_results().map_or(0, |r| {
            r.report
                .findings
                .iter()
                .filter(|f| self.mix.dismissed.contains(&f.id))
                .count()
        })
    }

    pub fn mix_shows_dismissed(&self) -> bool {
        self.mix.show_dismissed
    }

    /// An experiment has been applied.
    pub fn mix_applied(&self, finding: u64, recommendation: usize) -> bool {
        self.mix.applied.contains(&(finding, recommendation))
    }

    /// The findings to list (most relevant first; dismissed ones unless
    /// asked for).
    pub fn mix_findings(&self) -> Vec<&Finding> {
        self.mix_results().map_or_else(Vec::new, |r| {
            r.report
                .findings
                .iter()
                .filter(|f| self.mix.show_dismissed || !self.mix.dismissed.contains(&f.id))
                .collect()
        })
    }

    pub fn mix_audition(&self) -> Option<AuditionInfo> {
        self.mix.audition.as_ref().map(|a| AuditionInfo {
            finding: a.finding,
            recommendation: a.recommendation,
            candidate: a.candidate,
            matching: a.matching,
            measuring: a.measure.is_some(),
            measured: a.measured.clone(),
            trim_db: a.trim_db,
        })
    }

    /// A track's name now ("deleted track" when it is gone).
    pub fn mix_track_name(&self, track: TrackId) -> String {
        self.project
            .track(track)
            .map_or_else(|| "(deleted track)".into(), |t| t.name.clone())
    }

    /// The tracks Mix Intelligence compares (roles can be given to them).
    pub fn mix_tracks(&self) -> Vec<(TrackId, String, Option<TrackRole>)> {
        self.project
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument))
            .map(|t| (t.id, t.name.clone(), t.role))
            .collect()
    }

    /// Whether an experiment can be auditioned or applied now, and why not.
    pub fn mix_can_try(
        &self,
        finding: u64,
        recommendation: usize,
    ) -> std::result::Result<(), String> {
        let f = self
            .mix_finding(finding)
            .ok_or_else(|| "the finding is gone".to_string())?;
        let r = f
            .recommendations
            .get(recommendation)
            .ok_or_else(|| "no such experiment".to_string())?;
        if !r.experiment.previewable() {
            return Err("a suggestion to try by hand: nothing to apply".into());
        }
        if let Some(why) = self.mix_stale() {
            return Err(why.to_string());
        }
        let nyquist = f64::from(self.project.sample_rate) / 2.0;
        r.experiment.validate(nyquist)?;
        for t in r.experiment.tracks() {
            let track = self
                .project
                .track(t)
                .ok_or_else(|| "a track it involves was deleted".to_string())?;
            if !track.kind.has_audio() {
                return Err(format!("'{}' has no audio", track.name));
            }
        }
        let target = r.experiment.track().and_then(|t| self.project.track(t));
        if let Some(t) = target {
            if t.freeze.is_some() {
                return Err(format!("'{}' is frozen: unfreeze it to change it", t.name));
            }
            if let Experiment::Level { .. } = r.experiment
                && t.automation
                    .lane(AutomationTarget::TrackVolume)
                    .is_some_and(|l| l.mode != AutomationMode::Off && !l.curve.points().is_empty())
            {
                return Err(format!(
                    "'{}''s fader is automated: offset its volume automation by hand",
                    t.name
                ));
            }
        }
        Ok(())
    }

    /// The spectral resolution analyses use (`None`: the default for the
    /// project's rate, [`AnalysisConfig::for_rate`]); refused when it is not
    /// usable at the project's rate. Cached profiles of other settings are
    /// not reused (the settings are part of their key).
    pub fn set_mix_config(&mut self, config: Option<AnalysisConfig>) -> Result<()> {
        if let Some(c) = &config {
            c.validate(self.project.sample_rate)
                .map_err(|e| SessionError::Other(e.to_string()))?;
        }
        self.mix.config = config;
        Ok(())
    }

    /// The rules' thresholds the next analysis uses.
    pub fn set_mix_rules(&mut self, rules: Rules) {
        self.mix.rules = rules;
    }

    pub fn mix_rules(&self) -> &Rules {
        &self.mix.rules
    }

    /// Wait for the analysis (tests, scripts).
    pub fn wait_for_mix(&mut self) {
        let start = std::time::Instant::now();
        while self.mix.job.is_some() && start.elapsed().as_secs() < 600 {
            self.poll_mix();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.poll_mix();
    }

    /// Wait for the audition's loudness measurement (tests, scripts).
    pub fn wait_for_mix_measure(&mut self) {
        let start = std::time::Instant::now();
        while self
            .mix
            .audition
            .as_ref()
            .is_some_and(|a| a.measure.is_some())
            && start.elapsed().as_secs() < 600
        {
            self.poll_mix();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    // --- acting ----------------------------------------------------------

    pub(crate) fn mix_op(&mut self, op: MixOp) -> Result<()> {
        match op {
            MixOp::Analyse { scope, signal } => self.start_mix_analysis(scope, signal)?,
            MixOp::Cancel => {
                if let Some(j) = self.mix.job.take() {
                    j.progress.render.cancel.store(true, Ordering::Relaxed);
                    self.mix.retired.push(j);
                    self.mix.status = MixStatus::Cancelled;
                }
            }
            MixOp::Select(id) => self.mix.selected = id,
            MixOp::Show(id) => self.show_mix_finding(id, None)?,
            MixOp::ShowEvent { finding, event } => self.show_mix_finding(finding, Some(event))?,
            MixOp::Audition {
                finding,
                recommendation,
            } => self.start_mix_audition(finding, recommendation)?,
            MixOp::Compare(on) => self.mix_compare(on)?,
            MixOp::LoudnessMatch(on) => {
                if let Some(a) = &mut self.mix.audition {
                    a.matching = on;
                }
                self.apply_mix_trim();
            }
            MixOp::Apply {
                finding,
                recommendation,
            } => self.apply_mix_experiment(finding, recommendation)?,
            MixOp::Reject => {
                if self.mix.audition.is_some() {
                    self.end_mix_audition();
                    self.notify(
                        NoticeLevel::Info,
                        "Mix Intelligence: the experiment was taken out; the project is as it was",
                    );
                }
            }
            MixOp::Dismiss(id) => {
                self.mix.dismissed.insert(id);
                if self.mix.selected == Some(id) && !self.mix.show_dismissed {
                    self.mix.selected = None;
                }
            }
            MixOp::ShowDismissed(on) => self.mix.show_dismissed = on,
            MixOp::Clear => {
                self.end_mix_audition();
                self.mix.results = None;
                self.mix.selected = None;
                self.mix.stale = None;
                self.mix.status = MixStatus::Idle;
            }
        }
        self.revision += 1;
        Ok(())
    }

    fn start_mix_analysis(&mut self, scope: MixScope, signal: SignalPoint) -> Result<()> {
        // Never analyse a candidate: the audition ends first.
        self.end_mix_audition();
        let rate = self.project.sample_rate.max(1);
        let config = self
            .mix
            .config
            .clone()
            .unwrap_or_else(|| AnalysisConfig::for_rate(rate));
        config
            .validate(rate)
            .map_err(|e| SessionError::Other(e.to_string()))?;
        let selection = self.selection.range.filter(|r| !r.is_empty());
        let (a, b) = match (scope, selection) {
            (MixScope::Selection, None) => {
                return Err(SessionError::Other(
                    "select a time range to analyse first".into(),
                ));
            }
            (MixScope::Selection | MixScope::SelectedTracks, Some(r)) => (r.start, r.end),
            _ => (MusicalTime::ZERO, self.project.content_end()),
        };
        let focus: Vec<TrackId> = if scope == MixScope::SelectedTracks {
            let f: Vec<TrackId> = self
                .selection
                .tracks
                .iter()
                .copied()
                .filter(|t| {
                    self.project
                        .track(*t)
                        .is_some_and(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument))
                })
                .collect();
            if f.is_empty() {
                return Err(SessionError::Other(
                    "select the audio or instrument tracks to analyse first".into(),
                ));
            }
            f
        } else {
            Vec::new()
        };
        // The project as renders see it (plugin states captured), looped
        // nowhere, nothing soloed.
        let mut base = self.render_copy();
        base.loop_enabled = false;
        for t in &mut base.tracks {
            t.solo = false;
        }
        let fr = f64::from(rate);
        let start = base.timeline.to_samples(a, fr);
        let end = base.timeline.to_samples(b, fr);
        let frames = (end - start).max(0) as usize;
        if frames == 0 {
            return Err(SessionError::Other("the range to analyse is empty".into()));
        }
        if frames as f64 / fr > MAX_SECONDS {
            return Err(SessionError::Other(format!(
                "{:.0} minutes is more than Mix Intelligence analyses at once ({:.0}): select a range",
                frames as f64 / fr / 60.0,
                MAX_SECONDS / 60.0
            )));
        }
        let mut planned = Vec::new();
        let mut skipped = Vec::new();
        let mut analysed = Vec::new();
        for t in &base.tracks {
            if !matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) {
                continue;
            }
            if t.mute {
                skipped.push((t.id, "muted".into()));
                continue;
            }
            if !plays_in(&base, t, a, b) {
                skipped.push((t.id, "nothing plays in the range".into()));
                continue;
            }
            let plays = match signal {
                SignalPoint::Processed => Plays::Solo,
                SignalPoint::Source => Plays::Source {
                    // An instrument's notes need it (and the MIDI effects
                    // before it).
                    keep: if t.kind == TrackKind::Instrument {
                        t.inserts
                            .iter()
                            .position(|s| self.engine.plugin_is_instrument(s.id))
                            .map_or(t.inserts.len(), |i| i + 1)
                    } else {
                        0
                    },
                },
            };
            analysed.push(t.id);
            planned.push(Planned {
                track: t.id,
                plays,
                name: t.name.clone(),
                role: t.role,
                focus: focus.is_empty() || focus.contains(&t.id),
                spans: Vec::new(),
            });
        }
        if analysed.is_empty() {
            return Err(SessionError::Other(
                "no audio or instrument track plays in the range: nothing to analyse".into(),
            ));
        }
        // Sections in the range (seconds from its start; samples for the
        // master's loudness per section).
        let mut sections = Vec::new();
        let mut spans = Vec::new();
        let mut secs: Vec<&faderframe_project::Section> = base
            .sections
            .iter()
            .filter(|s| s.start < b && s.end > a)
            .collect();
        secs.sort_by_key(|s| s.start);
        for s in secs {
            let x = (base.timeline.to_samples(s.start.max(a), fr) - start).max(0) as usize;
            let y = (base.timeline.to_samples(s.end.min(b), fr) - start).max(0) as usize;
            if y > x {
                sections.push((s.name.clone(), TimeSpan::new(x as f64 / fr, y as f64 / fr)));
                spans.push((x, y));
            }
        }
        // The master: processed only (the whole mix).
        if signal == SignalPoint::Processed
            && let Some(m) = base.master_id()
        {
            planned.push(Planned {
                track: m,
                plays: Plays::Master,
                name: "the master".into(),
                role: None,
                focus: true,
                spans,
            });
        }
        // A newer analysis replaces a running one.
        if let Some(j) = self.mix.job.take() {
            j.progress.render.cancel.store(true, Ordering::Relaxed);
            self.mix.retired.push(j);
        }
        self.mix.generation += 1;
        let progress = Arc::new(Progress {
            render: RenderProgress::default(),
            track: AtomicU32::new(0),
            diagnosing: AtomicBool::new(false),
        });
        progress
            .render
            .total
            .store((frames * planned.len()) as u64, Ordering::Relaxed);
        let names: Vec<String> = planned.iter().map(|p| p.name.clone()).collect();
        let p2 = Arc::clone(&progress);
        let shared = Shared {
            start,
            frames,
            rate,
            signal,
            config: config.clone(),
            rules: self.mix.rules.clone(),
            sections,
            cache: self
                .mix
                .cache
                .entries
                .iter()
                .map(|e| (e.0, Arc::clone(&e.1)))
                .collect(),
        };
        let handle = std::thread::Builder::new()
            .name("faderframe-mix".into())
            .spawn(move || job_worker(planned, base, shared, p2))
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.mix.job = Some(Job {
            generation: self.mix.generation,
            serial: self.project_serial,
            revision: self.history.revision(),
            scope,
            signal,
            range: (a, b),
            start,
            frames,
            rate,
            config,
            tracks: analysed,
            focus,
            skipped,
            names,
            progress,
            handle,
        });
        self.mix.status = MixStatus::Idle;
        Ok(())
    }

    /// Take finished analyses and measurements in (from the session tick).
    pub(crate) fn poll_mix(&mut self) {
        // Another project took this one's place: nothing of the old one
        // stays (its candidate went with it).
        if self.mix.serial != self.project_serial {
            self.mix.serial = self.project_serial;
            if let Some(j) = self.mix.job.take() {
                j.progress.render.cancel.store(true, Ordering::Relaxed);
                self.mix.retired.push(j);
            }
            if let Some(a) = self.mix.audition.take()
                && let Some(m) = a.measure
            {
                m.progress.cancel.store(true, Ordering::Relaxed);
            }
            let _ = self.engine.set_monitor_trim(0.0);
            self.mix.results = None;
            self.mix.selected = None;
            self.mix.dismissed.clear();
            self.mix.applied.clear();
            self.mix.stale = None;
            self.mix.checked = None;
            self.mix.status = MixStatus::Idle;
            self.revision += 1;
        }
        self.mix.retired.retain(|j| !j.handle.is_finished());
        if self
            .mix
            .job
            .as_ref()
            .is_some_and(|j| j.handle.is_finished())
            && let Some(job) = self.mix.job.take()
        {
            let cancelled = job.progress.render.cancel.load(Ordering::Relaxed);
            let result = job
                .handle
                .join()
                .unwrap_or_else(|_| Err("the analysis failed".into()));
            let current =
                job.generation == self.mix.generation && job.serial == self.project_serial;
            match result {
                Ok(out) if current => {
                    for k in &out.reused {
                        let _ = self.mix.cache.get(k);
                    }
                    for (k, p) in out.profiles {
                        self.mix.cache.put(k, p);
                    }
                    let n = out.report.findings.len();
                    let results = MixResults {
                        report: out.report,
                        signal: job.signal,
                        scope: job.scope,
                        range: job.range,
                        start_sample: job.start,
                        frames: job.frames,
                        rate: job.rate,
                        tracks: job.tracks,
                        focus: job.focus,
                        skipped: job.skipped,
                        rendered: out.rendered,
                        reused: out.reused.len(),
                        config: job.config,
                        algorithm: faderframe_mix_intelligence::ALGORITHM_VERSION,
                        serial: job.serial,
                        history_revision: job.revision,
                        fingerprint: out.fingerprint,
                    };
                    if !self
                        .mix
                        .selected
                        .is_some_and(|s| results.report.findings.iter().any(|f| f.id == s))
                    {
                        self.mix.selected = results
                            .report
                            .findings
                            .iter()
                            .find(|f| !self.mix.dismissed.contains(&f.id))
                            .map(|f| f.id);
                    }
                    self.mix.results = Some(Arc::new(results));
                    self.mix.stale = None;
                    self.mix.checked = None;
                    self.mix.status = MixStatus::Idle;
                    self.notify(
                        NoticeLevel::Info,
                        format!(
                            "Mix Intelligence: {n} finding{} ({})",
                            if n == 1 { "" } else { "s" },
                            job.signal.label()
                        ),
                    );
                }
                Ok(_) => {}
                Err(e) if current => {
                    self.mix.status = if cancelled {
                        MixStatus::Cancelled
                    } else {
                        self.notify(NoticeLevel::Error, format!("Mix Intelligence: {e}"));
                        MixStatus::Failed(e)
                    };
                }
                Err(_) => {}
            }
            self.revision += 1;
        }
        self.check_mix_stale();
        self.poll_mix_measure();
    }

    /// Mark the results stale when the project's audio changed since.
    fn check_mix_stale(&mut self) {
        let Some(r) = self.mix.results.clone() else {
            return;
        };
        // The candidate is in the project while auditioning: not now.
        if self.mix.audition.is_some() {
            return;
        }
        let rev = self.history.revision();
        if self.mix.checked == Some(rev) {
            return;
        }
        self.mix.checked = Some(rev);
        let gone = r.tracks.iter().any(|t| self.project.track(*t).is_none());
        let changed = rev != r.history_revision && {
            let now = fingerprint(&self.project.song());
            now.is_none() || now != r.fingerprint
        };
        let stale = if gone {
            Some("A track analysed was deleted: analyse again to refresh the findings.".into())
        } else if changed {
            Some(
                "The project changed after this analysis: analyse again to see the mix as it is \
                 now (unchanged tracks are reused)."
                    .into(),
            )
        } else {
            None
        };
        if stale != self.mix.stale {
            self.mix.stale = stale;
            self.revision += 1;
        }
    }

    fn poll_mix_measure(&mut self) {
        let Some(a) = &mut self.mix.audition else {
            return;
        };
        if !a.measure.as_ref().is_some_and(|m| m.handle.is_finished()) {
            return;
        }
        if let Some(m) = a.measure.take() {
            a.measured = Some(
                m.handle
                    .join()
                    .unwrap_or_else(|_| Err("the measurement failed".into())),
            );
        }
        self.apply_mix_trim();
        self.revision += 1;
    }

    /// The finding's tracks selected and its time (or one event) made the
    /// edit selection, the arranger zoomed to it.
    fn show_mix_finding(&mut self, id: u64, event: Option<usize>) -> Result<()> {
        let Some(r) = self.mix.results.clone() else {
            return Ok(());
        };
        let Some(f) = r.report.findings.iter().find(|f| f.id == id) else {
            return Ok(());
        };
        self.mix.selected = Some(id);
        let tracks: Vec<TrackId> = f
            .tracks
            .iter()
            .copied()
            .filter(|t| self.project.track(*t).is_some())
            .collect();
        if !tracks.is_empty() {
            self.dispatch(Action::SelectTracks {
                tracks,
                mode: SelectMode::Replace,
            })?;
        }
        let span = match event {
            Some(e) => f.events.get(e).copied(),
            None => f.time,
        };
        if let Some(s) = span {
            let a = r.position(&self.project, s.start);
            let b = r.position(&self.project, s.end);
            if b > a {
                self.set_edit_range(Some(EditRange::new(a, b)))?;
                self.dispatch(Action::Zoom(ZoomRequest::Selection))?;
            }
        }
        Ok(())
    }

    /// The candidate's edits for an experiment, the A/B switch, and the
    /// plugin it adds.
    fn mix_candidate(&mut self, exp: &Experiment) -> Result<(Vec<Command>, Toggle)> {
        let nyquist = f64::from(self.project.sample_rate) / 2.0;
        exp.validate(nyquist).map_err(SessionError::Other)?;
        let band = 0;
        let eq = |this: &mut Self,
                  track: TrackId,
                  params: &[(Field, f64)],
                  key: Option<TrackId>|
         -> Result<(Vec<Command>, Toggle)> {
            let index = this
                .project
                .track(track)
                .map(|t| t.inserts.len())
                .ok_or_else(|| SessionError::Other("the track is gone".into()))?;
            let params: Vec<(ParameterId, f64)> =
                params.iter().map(|&(f, v)| (band_id(band, f), v)).collect();
            let slot = eq_slot(&mut this.project, &params, key);
            let plugin = slot.id;
            Ok((
                vec![Command::InsertPlugin { track, index, slot }],
                Toggle::Band {
                    track,
                    plugin,
                    band,
                },
            ))
        };
        match *exp {
            Experiment::EqCut {
                track,
                freq_hz,
                gain_db,
                q,
            } => eq(
                self,
                track,
                &[
                    (Field::Enabled, 1.0),
                    (Field::Type, 0.0),
                    (Field::Freq, freq_hz),
                    (Field::Gain, gain_db),
                    (Field::Q, q),
                ],
                None,
            ),
            Experiment::DynamicEqCut {
                track,
                key,
                freq_hz,
                range_db,
                q,
            } => eq(
                self,
                track,
                &[
                    (Field::Enabled, 1.0),
                    (Field::Type, 0.0),
                    (Field::Freq, freq_hz),
                    (Field::Gain, 0.0),
                    (Field::Q, q),
                    (Field::Range, range_db),
                    (Field::Key, 1.0),
                    // Automatic threshold and times.
                    (Field::Threshold, 0.0),
                    (Field::Dynamics, 0.0),
                ],
                Some(key),
            ),
            Experiment::SideLowCut { track, freq_hz } => eq(
                self,
                track,
                &[
                    (Field::Enabled, 1.0),
                    // Low cut, 24 dB/oct, on the side signal.
                    (Field::Type, 3.0),
                    (Field::Freq, freq_hz),
                    (Field::Slope, 24.0),
                    (Field::Placement, 4.0),
                ],
                None,
            ),
            Experiment::Level { track, change_db } => {
                let original = self
                    .project
                    .track(track)
                    .map(|t| t.volume_db)
                    .ok_or_else(|| SessionError::Other("the track is gone".into()))?;
                let candidate = (f64::from(original) + change_db) as f32;
                Ok((
                    vec![Command::SetTrackVolume {
                        track,
                        db: candidate,
                    }],
                    Toggle::Volume {
                        track,
                        original,
                        candidate,
                    },
                ))
            }
            Experiment::Manual { .. } => Err(SessionError::Other(
                "a suggestion to try by hand: nothing to apply".into(),
            )),
        }
    }

    /// Apply commands without an undo step; their inverses.
    fn apply_unrecorded_mix(&mut self, commands: Vec<Command>) -> Result<Vec<Command>> {
        let mut impact = Impact::None;
        let mut inverses = Vec::new();
        for cmd in commands {
            impact = impact.max(cmd.impact());
            match cmd.apply(&mut self.project) {
                Ok(inv) => inverses.push(inv),
                Err(e) => {
                    // Undo what was done.
                    for inv in inverses.into_iter().rev() {
                        let _ = inv.apply(&mut self.project);
                    }
                    self.sync(impact)?;
                    return Err(e.into());
                }
            }
        }
        self.sync(impact)?;
        Ok(inverses)
    }

    fn start_mix_audition(&mut self, finding: u64, recommendation: usize) -> Result<()> {
        self.end_mix_audition();
        self.check_mix_stale();
        self.mix_can_try(finding, recommendation)
            .map_err(SessionError::Other)?;
        let (f, exp) = {
            let f = self
                .mix_finding(finding)
                .ok_or_else(|| SessionError::Other("the finding is gone".into()))?;
            (
                f.clone(),
                f.recommendations
                    .get(recommendation)
                    .map(|r| r.experiment.clone())
                    .ok_or_else(|| SessionError::Other("no such experiment".into()))?,
            )
        };
        // The original, as renders see it (for the loudness match).
        let mut original = self.render_copy();
        let (commands, toggle) = self.mix_candidate(&exp)?;
        let inverses = self.apply_unrecorded_mix(commands)?;
        let mut candidate = self.render_copy();
        for p in [&mut original, &mut candidate] {
            p.loop_enabled = false;
            for t in &mut p.tracks {
                t.solo = false;
            }
        }
        let measure = self.mix_measure_span(&f).map(|span| {
            let progress = Arc::new(RenderProgress::default());
            let p2 = Arc::clone(&progress);
            let band = f.frequency.map(|r| (r.lo, r.hi));
            let handle = std::thread::Builder::new()
                .name("faderframe-mix-measure".into())
                .spawn(move || measure_worker([original, candidate], span, band, p2));
            (progress, handle)
        });
        let measure = match measure {
            Some((progress, Ok(handle))) => Some(MeasureJob { progress, handle }),
            _ => None,
        };
        let measured = if measure.is_none() {
            Some(Err("no time to measure the loudness over".into()))
        } else {
            None
        };
        self.mix.audition = Some(Audition {
            finding,
            recommendation,
            serial: self.project_serial,
            inverses,
            toggle,
            candidate: true,
            matching: true,
            measure,
            measured,
            trim_db: 0.0,
        });
        self.mix.selected = Some(finding);
        self.apply_mix_trim();
        Ok(())
    }

    /// Where an audition's loudness is measured: the finding's time (or
    /// the analysed range's start), [`MEASURE_MIN_SECONDS`] to
    /// [`MEASURE_SECONDS`], inside the analysed range.
    fn mix_measure_span(&self, f: &Finding) -> Option<(i64, usize, u32)> {
        let r = self.mix_results()?;
        let rate = r.rate;
        let total = r.seconds();
        let (mut a, mut b) = f.time.map_or((0.0, total), |t| (t.start, t.end));
        b = b.min(a + MEASURE_SECONDS);
        if b - a < MEASURE_MIN_SECONDS {
            b = (a + MEASURE_MIN_SECONDS).min(total);
            a = (b - MEASURE_MIN_SECONDS).max(0.0);
        }
        let fr = f64::from(rate);
        let frames = ((b - a) * fr) as usize;
        (frames > 0).then(|| (r.start_sample + (a * fr) as i64, frames, rate))
    }

    /// The monitor trim the audition asks for now.
    fn apply_mix_trim(&mut self) {
        let db = self.mix.audition.as_ref().map_or(0.0, |a| {
            match (&a.measured, a.candidate && a.matching) {
                (Some(Ok(m)), true) => -m.change(),
                _ => 0.0,
            }
        });
        if let Some(a) = &mut self.mix.audition {
            a.trim_db = db;
        }
        if let Err(e) = self.engine.set_monitor_trim(db as f32) {
            tracing::warn!("mix audition: {e}");
        }
    }

    fn mix_compare(&mut self, on: bool) -> Result<()> {
        let Some(a) = &self.mix.audition else {
            return Ok(());
        };
        if a.candidate == on {
            return Ok(());
        }
        let cmd = match a.toggle {
            Toggle::Band {
                track,
                plugin,
                band,
            } => Command::SetPluginParameter {
                track,
                plugin,
                parameter: band_id(band, Field::Enabled),
                // On, or bypassed (shown, not heard).
                value: Some(if on { 1.0 } else { 2.0 }),
            },
            Toggle::Volume {
                track,
                original,
                candidate,
            } => Command::SetTrackVolume {
                track,
                db: if on { candidate } else { original },
            },
        };
        // A parameter change: its inverse is the other way, nothing to
        // keep (the audition's inverses restore the project in any case).
        self.apply_unrecorded_mix(vec![cmd])?;
        if let Some(a) = &mut self.mix.audition {
            a.candidate = on;
        }
        self.apply_mix_trim();
        Ok(())
    }

    /// Take the candidate out: the project as it was. No undo step.
    pub(crate) fn end_mix_audition(&mut self) {
        let Some(a) = self.mix.audition.take() else {
            return;
        };
        if let Some(m) = &a.measure {
            m.progress.cancel.store(true, Ordering::Relaxed);
        }
        if a.serial == self.project_serial {
            let mut impact = Impact::None;
            for inv in a.inverses.into_iter().rev() {
                impact = impact.max(inv.impact());
                if let Err(e) = inv.apply(&mut self.project) {
                    tracing::warn!("mix audition: could not take the candidate out: {e}");
                }
            }
            if let Err(e) = self.sync(impact) {
                tracing::warn!("mix audition: {e}");
            }
        }
        let _ = self.engine.set_monitor_trim(0.0);
        self.revision += 1;
    }

    /// Before `action` is dispatched: an audition ends unless the action
    /// leaves the project alone; an edit of the candidate's own EQ stays
    /// part of the candidate (`None`: it was taken).
    pub(crate) fn mix_before(&mut self, action: Action) -> Result<Option<Action>> {
        let Some(a) = &self.mix.audition else {
            return Ok(Some(action));
        };
        if keeps_audition(&action) {
            return Ok(Some(action));
        }
        let candidate = a.plugin();
        let own = matches!(
            &action,
            Action::Edit(Command::SetPluginParameter { plugin, .. }) if candidate == Some(*plugin)
        );
        if own && let Action::Edit(cmd) = action {
            self.apply_unrecorded_mix(vec![cmd])?;
            self.revision += 1;
            return Ok(None);
        }
        self.end_mix_audition();
        Ok(Some(action))
    }

    /// Make an experiment one undo step (the auditioned candidate as it is
    /// now, the candidate's switch on).
    fn apply_mix_experiment(&mut self, finding: u64, recommendation: usize) -> Result<()> {
        let auditioned = self
            .mix
            .audition
            .as_ref()
            .is_some_and(|a| a.finding == finding && a.recommendation == recommendation);
        // The candidate's slot as it is (its EQ may have been edited).
        let mut kept: Option<(TrackId, PluginSlot)> = None;
        if auditioned {
            self.mix_compare(true)?;
            if let Some(Toggle::Band { track, plugin, .. }) =
                self.mix.audition.as_ref().map(|a| a.toggle)
            {
                self.capture_plugin_state(plugin);
                kept = self
                    .project
                    .track(track)
                    .and_then(|t| t.inserts.iter().find(|s| s.id == plugin))
                    .map(|s| (track, s.clone()));
            }
        }
        self.end_mix_audition();
        self.check_mix_stale();
        self.mix_can_try(finding, recommendation)
            .map_err(SessionError::Other)?;
        let exp = self
            .mix_finding(finding)
            .and_then(|f| f.recommendations.get(recommendation))
            .map(|r| r.experiment.clone())
            .ok_or_else(|| SessionError::Other("no such experiment".into()))?;
        let commands = match kept {
            Some((track, slot)) => {
                let index = self
                    .project
                    .track(track)
                    .map(|t| t.inserts.len())
                    .ok_or_else(|| SessionError::Other("the track is gone".into()))?;
                vec![Command::InsertPlugin { track, index, slot }]
            }
            None => self.mix_candidate(&exp)?.0,
        };
        let name = |id: TrackId| self.mix_track_name(id);
        let label = format!("Mix Intelligence: {}", exp.title(&name));
        self.edit(Command::Batch {
            label: label.clone(),
            commands,
        })?;
        self.mix.applied.insert((finding, recommendation));
        self.notify(
            NoticeLevel::Info,
            format!("{label} — applied as one step (Undo takes it back)"),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_ignores_presentation_and_sees_sound() {
        let p = faderframe_project::demo::demo_project(48_000);
        let fp = fingerprint(&p);
        assert!(fp.is_some());
        let mut renamed = p.clone();
        renamed.tracks[0].name = "Something else".into();
        renamed.tracks[0].solo = true;
        renamed.tracks[0].role = Some(TrackRole::Drums);
        renamed.name = "Other".into();
        let _: TrackId = renamed.ids.allocate();
        assert_eq!(fingerprint(&renamed), fp, "names, solo, roles, ids");
        let mut louder = p.clone();
        louder.tracks[0].volume_db += 1.0;
        assert_ne!(fingerprint(&louder), fp);
    }

    #[test]
    fn the_cache_keeps_to_its_budget() {
        let config = AnalysisConfig::for_rate(48_000);
        let audio = vec![0.1f32; 48_000];
        let profile = Arc::new(
            faderframe_mix_intelligence::analyse(&faderframe_mix_intelligence::ProfileRequest {
                channels: &[&audio],
                rate: 48_000,
                config: &config,
                spans: &[],
                cancel: None,
            })
            .unwrap_or_else(|e| panic!("{e}")),
        );
        let mut c = Cache {
            budget: profile.bytes() * 20,
            ..Cache::default()
        };
        let key = |i: u64| CacheKey {
            track: i,
            signal: 0,
            project: 1,
            start: 0,
            frames: 48_000,
            rate: 48_000,
            config: 2,
            spans: 0,
        };
        let n = 30;
        for i in 0..n as u64 {
            c.put(key(i), Arc::clone(&profile));
        }
        assert!(c.bytes() <= c.budget);
        assert_eq!(c.entries.len(), 20);
        assert!(c.get(&key(0)).is_none(), "the oldest went");
        assert!(c.get(&key(n as u64 - 1)).is_some());
    }
}
