//! Mix Intelligence in the session: the mix analysed offline, findings
//! with their evidence, experiments auditioned in the real engine and
//! applied as one undo step.
//!
//! **What is measured.** An analysis renders, on a thread of its own
//! (never the audio thread, never the live engine: renders build their
//! own, as exports do), over the range:
//!
//! * *Processed* (the default): **one** render of the whole mix
//!   ([`capture`], `faderframe_engine::capture`) with a tap on every track,
//!   bus and return analysed — its signal as it reaches its destination:
//!   after its devices, automation, sidechains, fader and pan, before it is
//!   summed there — the master as the render's output (stereo; a surround
//!   master folded down as the device would), and a probe after every
//!   device (the health check: overs, flattened peaks, invalid samples,
//!   silence, no change, each device's processing time from the engine's
//!   node timing). Bus compressors and the master chain react to the whole
//!   mix, as they do when it plays. The taps are aligned with the master by
//!   the graph's own delay compensation and the render takes the graph's
//!   latency off the front of all of them alike: one time reference, no
//!   second compensation. With a reference track the master is compared
//!   with it ([`faderframe_mix_intelligence::reference`], loudness-matched).
//! * *Source*: each track's clips as they play (clip gain, fades, warp,
//!   an instrument's notes through its MIDI effects and instrument) with
//!   no audio device after them, no fader, pan, sends, modulators or
//!   console: what the material is, not what the mix makes of it — a
//!   render per track ([`render_each`]).
//!
//! Renders stream into [`faderframe_mix_intelligence::profile::ProfileBuilder`]
//! (a frame of audio held at a time, whatever the range's length); the
//! profiles go through the rules ([`faderframe_mix_intelligence::rules`],
//! `health`, `reference`). The live project is never touched to analyse it.
//! A second stage ([`extra`]) measures what needs more than one profile per
//! track: the sources of processed tracks (their attacks compared; rendered
//! side by side on a few threads) and pairs of tracks over excerpts for
//! phase and timing (processed: every track of an excerpt tapped in one
//! render of it).
//!
//! **Caching and staleness.** Profiles are kept (up to [`CACHE_BYTES`],
//! least recently used out) under a key of the track, the signal, the
//! range, the rate, the analysis settings and a fingerprint of what the
//! signal depends on ([`fingerprint`]): a processed track's is its
//! upstream closure's (itself and everything reaching it) plus the global
//! settings — nothing downstream changes a tap — the master's the whole
//! project's (without names, colours, markers, sections, roles, solo, ids,
//! references …); a source render's is its own one-track project's. So a
//! change re-measures only the tracks it reaches (the render itself runs
//! again whenever anything is missing: the master needs the whole mix),
//! and the health check is kept for the project fingerprint it was made
//! with. Results say which project serial, history revision and
//! fingerprints they came from: a job for a project that was replaced, or
//! superseded by a newer job (generation), is dropped; a finding whose
//! tracks' fingerprints changed since is stale ([`Session::mix_finding_stale`])
//! and its experiments are refused until analysed again — findings on
//! tracks the change did not reach stay valid.
//!
//! **Auditioning.** An experiment is applied to the live project *without*
//! an undo step (its inverse kept here), so the engine plays it for real:
//! a new built-in EQ at the end of the track's devices, or the fader. A/B
//! switches the EQ's band between on and bypassed, or the fader between
//! its two values — parameter changes, instant, no rebuild (gain staging:
//! both trims; a device's bypass: a rebuild). Meanwhile the loudness of
//! the master with and without it is measured offline (BS.1770
//! integrated, over the finding's time, up to [`MEASURE_SECONDS`]); with
//! matching on, the candidate is played that much louder or quieter on the
//! monitor only — the mix side of the one listening gain the A/B
//! comparisons use too ([`crate::compare`], [`Session::listening_gain`]) —
//! the real level change shown beside the fairness gain, the project never
//! changed by it. Anything else that edits the
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
use faderframe_project::reference::ReferenceSync;
use faderframe_project::{
    Command, Impact, InputRouting, OutputRouting, PluginRef, PluginSlot, Project, SavedParameter,
    SourceSpec, Track, TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

mod capture;
mod fingerprint;
mod measure;

pub use faderframe_mix_intelligence as intelligence;
use faderframe_mix_intelligence::health::DeviceRecord;
use faderframe_mix_intelligence::reference::ReferenceComparison;
pub use fingerprint::{Fingerprints, fingerprints};

mod extra;
pub use extra::{ExtraCounts, MAX_EXCERPT_BYTES};

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
/// The built-in Utility's.
const UTILITY_NAME: &str = "Utility";

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
    /// Do what a hand-off experiment says (`Experiment::hand_off`): hear
    /// the reference A/B, open EQ Match with it.
    Open {
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
    /// What each analysed signal depended on (a finding is stale when one
    /// of its tracks' changed).
    pub fingerprints: Option<Fingerprints>,
    /// What the probes measured of every device (processed analyses).
    pub devices: Arc<Vec<DeviceRecord>>,
    /// The master against the reference (processed, with a reference).
    pub reference: Option<ReferenceComparison>,
    /// Offline renders it took (one for a processed analysis, one per
    /// track for sources; none when everything was reused) and how long
    /// the analysis took (seconds).
    pub renders: usize,
    pub elapsed: f64,
    /// The graph's latency the render took off the front of the master and
    /// every tap alike (frames; `None`: nothing rendered).
    pub latency: Option<u32>,
    /// Seconds each stage took ("render and measure", "reference",
    /// "rules" …).
    pub phases: Vec<(&'static str, f64)>,
    /// What could not be measured (a reference that is not there …).
    pub notes: Vec<String>,
    /// `false` while the second stage (sources beside the processed
    /// signals, phase and timing between pairs) still runs: the findings of
    /// the first are shown meanwhile.
    pub complete: bool,
    /// Sources measured beside the processed signals, pairs compared
    /// (rendered now or reused).
    pub extra: ExtraCounts,
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

/// What a planned signal is.
#[derive(Clone, Copy, Debug)]
enum Plays {
    /// The track (bus, return) where it reaches its destination, tapped in
    /// the one render of the whole mix.
    Tapped,
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
    /// Compared with the others for masking (tracks and returns: a bus
    /// summing tracks is not — its tracks are).
    compare: bool,
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
    /// The reference the master is compared with.
    reference: Option<capture::ReferencePlan>,
    /// The last health check and its key (reused for the same project).
    health: Option<(u64, Arc<Vec<DeviceRecord>>)>,
    /// Sources beside processed signals, pairs ([`extra`]).
    extra: extra::ExtraPlan,
}

impl Shared {
    /// What the extra stage reads ([`extra`]).
    fn stage<'a>(
        &'a self,
        planned: &'a [Planned],
        base: &'a Project,
        fps: Option<&'a Fingerprints>,
        sources: &'a faderframe_engine::SourceMap,
        progress: &'a Progress,
    ) -> extra::Stage<'a> {
        extra::Stage {
            planned,
            base,
            fps,
            sources,
            start: self.start,
            frames: self.frames,
            rate: self.rate,
            signal: self.signal,
            config: &self.config,
            rules: &self.rules,
            cache: &self.cache,
            plan: &self.extra,
            render: &progress.render,
            progress: &progress.extra,
            threads: &progress.threads,
        }
    }

    /// The cache key of a signal of `track` (`u64::MAX`: the master) whose
    /// fingerprint is `project`.
    fn key(&self, track: u64, project: u64, spans: &[(usize, usize)]) -> CacheKey {
        let mut h = Fnv::new();
        for (x, y) in spans {
            h.write_u64(*x as u64);
            h.write_u64(*y as u64);
        }
        CacheKey {
            track,
            signal: self.signal as u8,
            project,
            start: self.start,
            frames: self.frames,
            rate: self.rate,
            config: self.config.fingerprint(),
            spans: h.finish(),
        }
    }
}

struct Progress {
    render: RenderProgress,
    track: AtomicU32,
    diagnosing: AtomicBool,
    /// What the job does now ([`STAGE_RENDER`] …).
    stage: AtomicU32,
    /// Signals measured now (not reused), the threads measuring them;
    /// sources rendered so far.
    measuring: AtomicU32,
    threads: AtomicU32,
    done: AtomicU32,
    /// The first stage's findings, handed over once.
    first: std::sync::Mutex<Option<Partial>>,
    extra: extra::ExtraProgress,
}

/// What a processed job found before its second stage, shown while that
/// runs ([`Session::mix_results`] with `complete: false`).
struct Partial {
    report: Report,
    rendered: usize,
    reused: usize,
    fingerprint: Option<u64>,
    fingerprints: Option<Fingerprints>,
    devices: Arc<Vec<DeviceRecord>>,
    reference: Option<ReferenceComparison>,
    renders: usize,
    latency: Option<u32>,
}

/// A job's stages (what its progress says).
const STAGE_RENDER: u32 = 0;
const STAGE_REFERENCE: u32 = 1;
const STAGE_RULES: u32 = 2;

struct Output {
    report: Report,
    /// Profiles made now, and the keys of those reused.
    profiles: Vec<(CacheKey, Arc<TrackProfile>)>,
    reused: Vec<CacheKey>,
    rendered: usize,
    /// The analysed project's fingerprint ([`fingerprint`]) and its
    /// tracks'.
    fingerprint: Option<u64>,
    fingerprints: Option<Fingerprints>,
    devices: Arc<Vec<DeviceRecord>>,
    /// The key the health check was made for (kept for the next job).
    health_key: Option<u64>,
    reference: Option<ReferenceComparison>,
    renders: usize,
    latency: Option<u32>,
    notes: Vec<String>,
    /// Seconds each stage took.
    phases: Vec<(&'static str, f64)>,
    extra: extra::ExtraOutcome,
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
    started: std::time::Instant,
}

/// How A/B switches a candidate.
#[derive(Clone, Debug)]
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
    /// Parameters of the candidate's own devices: (device, parameter,
    /// original, candidate) — gain staging's two trims, a DC filter's
    /// switch.
    Params {
        track: TrackId,
        params: Vec<(PluginInstanceId, ParameterId, f64, f64)>,
    },
    /// A device bypassed (the candidate) or playing (the original): the
    /// graph rebuilds each time.
    Bypass {
        track: TrackId,
        plugin: PluginInstanceId,
    },
    /// The track's delay (ms) and polarity, as they were and as tried.
    Alignment {
        track: TrackId,
        original: (f64, bool),
        candidate: (f64, bool),
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
    /// The candidate's own devices (edits of them stay part of it).
    fn plugins(&self) -> Vec<PluginInstanceId> {
        match &self.toggle {
            Toggle::Band { plugin, .. } => vec![*plugin],
            Toggle::Params { params, .. } => params.iter().map(|p| p.0).collect(),
            Toggle::Volume { .. } | Toggle::Bypass { .. } | Toggle::Alignment { .. } => Vec::new(),
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
    /// The last health check and the key it was made for.
    health: Option<(u64, Arc<Vec<DeviceRecord>>)>,
    /// Findings no longer describing the project (their tracks changed).
    stale_findings: BTreeSet<u64>,
    /// Pair measurements kept for reuse ([`extra`]).
    pairs: extra::PairCache,
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
            health: None,
            stale_findings: BTreeSet::new(),
            pairs: extra::PairCache::default(),
        }
    }
}

impl MixState {
    pub(crate) fn auditioning(&self) -> bool {
        self.audition.is_some()
    }
}

/// What an analysis of `project` depends on, as a number: its JSON with
/// the presentation left out (names, colours, markers, sections, roles,
/// solo, ids, references …). `None` when it cannot be serialised (nothing
/// is cached then). Each track's is in [`fingerprints`].
pub fn fingerprint(project: &Project) -> Option<u64> {
    fingerprints(project).map(|f| f.project)
}

/// Kinds of track Mix Intelligence analyses: tracks, buses and returns.
fn analysable(kind: TrackKind) -> bool {
    matches!(
        kind,
        TrackKind::Audio | TrackKind::Instrument | TrackKind::Bus | TrackKind::Aux
    )
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

/// The built-in Utility with `params` set, as a new slot.
fn utility_slot(project: &mut Project, params: &[(u32, f64)]) -> PluginSlot {
    PluginSlot {
        id: project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::GAIN, UTILITY_NAME),
        bypass: false,
        parameters: params
            .iter()
            .map(|&(id, value)| SavedParameter {
                id: ParameterId(id),
                value,
            })
            .collect(),
        state: None,
        sidechain: None,
    }
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
    let mut sources = faderframe_engine::render_generated_sources(&base, shared.rate);
    for (_, path, e) in crate::media::open_file_sources(&base, None, &mut sources) {
        tracing::warn!("mix analysis: {}: {e}", path.display());
    }
    match shared.signal {
        SignalPoint::Processed => capture::processed(&planned, &base, shared, &progress, &sources),
        SignalPoint::Source => source_worker(&planned, &base, shared, &progress, &sources),
    }
}

/// The source analysis: each track's clips through what makes their
/// sound (an instrument), a render per track.
fn source_worker(
    planned: &[Planned],
    base: &Project,
    shared: Shared,
    progress: &Progress,
    sources: &faderframe_engine::SourceMap,
) -> std::result::Result<Output, String> {
    // The sources side by side on a few threads, each render on its own
    // (a track and a plain master: little in a graph that small to share
    // between cores), its profile built on the same thread.
    let t0 = std::time::Instant::now();
    let n = planned.len();
    let threads = measure::threads(n);
    progress.threads.store(threads as u32, Ordering::Relaxed);
    progress.measuring.store(n as u32, Ordering::Relaxed);
    type Done = std::result::Result<(Arc<TrackProfile>, Option<CacheKey>, bool), String>;
    let results: std::sync::Mutex<Vec<Option<Done>>> =
        std::sync::Mutex::new((0..n).map(|_| None).collect());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let one = |p: &Planned| -> Done {
        let Plays::Source { keep } = p.plays else {
            return Err(format!("{}: not a source", p.name));
        };
        let Some((project, outputs)) = source_project(base, p.track, keep) else {
            return Err(format!("{}: no master to render through", p.name));
        };
        let k = fingerprint(&project).map(|fp| shared.key(p.track.0, fp, &p.spans));
        if let Some(found) = k.and_then(|k| shared.cache.iter().find(|e| e.0 == k)) {
            return Ok((Arc::clone(&found.1), Some(found.0), false));
        }
        let mut builder = ProfileBuilder::new(shared.rate, outputs, &shared.config, &p.spans)
            .map_err(|e| e.to_string())?;
        let mut failed = None;
        crate::render::render_each_on(
            &project,
            shared.rate,
            sources,
            (shared.start, shared.frames),
            &progress.render,
            outputs,
            0,
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
        Ok((Arc::new(builder.finish()), k, true))
    };
    std::thread::scope(|scope| {
        for w in 0..threads {
            let (next, results, one) = (&next, &results, &one);
            let spawned = std::thread::Builder::new()
                .name(format!("faderframe-mix-source-{w}"))
                .spawn_scoped(scope, move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n || progress.render.cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let r = one(&planned[i]);
                        progress.done.fetch_add(1, Ordering::Relaxed);
                        if let Ok(mut v) = results.lock() {
                            v[i] = Some(r);
                        }
                    }
                });
            if let Err(e) = spawned {
                tracing::warn!("mix analysis: {e}");
            }
        }
    });
    if progress.render.cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let results = results
        .into_inner()
        .map_err(|_| "a source render failed".to_string())?;
    let mut profiles: Vec<Arc<TrackProfile>> = Vec::with_capacity(n);
    let (mut made, mut reused) = (Vec::new(), Vec::new());
    let mut rendered = 0;
    let mut renders = 0;
    for (p, r) in planned.iter().zip(results) {
        let (profile, key, fresh) =
            r.unwrap_or_else(|| Err(format!("{}: not rendered", p.name)))?;
        match (fresh, key) {
            (true, k) => {
                renders += 1;
                rendered += 1;
                if let Some(k) = k {
                    made.push((k, Arc::clone(&profile)));
                }
            }
            (false, Some(k)) => reused.push(k),
            (false, None) => {}
        }
        profiles.push(profile);
    }
    let mut phases: Vec<(&'static str, f64)> =
        vec![("render and measure", t0.elapsed().as_secs_f64())];
    progress.stage.store(STAGE_RULES, Ordering::Relaxed);
    let t0 = std::time::Instant::now();
    progress.diagnosing.store(true, Ordering::Relaxed);
    let sourced: Vec<&Planned> = planned
        .iter()
        .filter(|p| matches!(p.plays, Plays::Source { .. }))
        .collect();
    let tracks: Vec<TrackInput<'_>> = sourced
        .iter()
        .zip(&profiles)
        .map(|(p, prof)| TrackInput {
            track: p.track,
            role: p.role,
            profile: prof,
            compare: p.compare,
            focus: p.focus,
        })
        .collect();
    let input = DiagnoseInput {
        tracks,
        master: None,
        signal: SignalPoint::Source,
        sections: shared.sections.clone(),
        rules: &shared.rules,
        cancel: Some(&progress.render.cancel),
        related: Vec::new(),
    };
    let mut report = faderframe_mix_intelligence::diagnose(&input).map_err(|e| e.to_string())?;
    // DC offsets in the material itself.
    report.merge(faderframe_mix_intelligence::health::diagnose(
        &faderframe_mix_intelligence::health::HealthInput {
            subjects: sourced
                .iter()
                .zip(&profiles)
                .filter(|(p, _)| p.focus)
                .map(
                    |(p, prof)| faderframe_mix_intelligence::health::HealthSubject {
                        subject: faderframe_mix_intelligence::Subject::Track(p.track),
                        profile: prof,
                    },
                )
                .collect(),
            devices: &[],
            signal: SignalPoint::Source,
            rules: &shared.rules.health,
        },
    ));
    phases.push(("rules", t0.elapsed().as_secs_f64()));
    let fps = fingerprints(base);
    // Pairs for phase and timing, over excerpts of their sources.
    let t0 = std::time::Instant::now();
    let extra = extra::run(
        &shared.stage(planned, base, fps.as_ref(), sources, progress),
        &input,
        &mut report,
    )?;
    phases.push(("phase and timing", t0.elapsed().as_secs_f64()));
    Ok(Output {
        report,
        profiles: made,
        reused,
        rendered,
        fingerprint: fps.as_ref().map(|f| f.project),
        fingerprints: fps,
        devices: Arc::new(Vec::new()),
        health_key: None,
        reference: None,
        renders,
        latency: None,
        notes: Vec::new(),
        phases,
        extra,
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
        if let Some(what) = extra::status(&p.extra) {
            return MixStatus::Running {
                what,
                fraction: p.render.fraction().min(1.0),
            };
        }
        match p.stage.load(Ordering::Relaxed) {
            STAGE_REFERENCE => {
                return MixStatus::Running {
                    what: "Measuring the reference beside the master…".into(),
                    fraction: 1.0,
                };
            }
            STAGE_RULES => {
                return MixStatus::Running {
                    what: "Comparing the tracks…".into(),
                    fraction: 1.0,
                };
            }
            _ => {}
        }
        if p.diagnosing.load(Ordering::Relaxed) {
            return MixStatus::Running {
                what: "Comparing the tracks…".into(),
                fraction: 1.0,
            };
        }
        let n = j.names.len().max(1);
        let measuring = p.measuring.load(Ordering::Relaxed) as usize;
        let threads = p.threads.load(Ordering::Relaxed).max(1);
        if j.signal == SignalPoint::Processed {
            let what = if measuring == 0 {
                "Everything measured is reused: checking the devices once more".to_string()
            } else {
                format!(
                    "Rendering the mix once and measuring {measuring} of {n} signals on \
                     {threads} thread{} ({} reused)",
                    if threads == 1 { "" } else { "s" },
                    n - measuring.min(n)
                )
            };
            return MixStatus::Running {
                what,
                fraction: p.render.fraction(),
            };
        }
        MixStatus::Running {
            what: format!(
                "Rendering the sources, {threads} at a time ({} of {n} done)",
                p.done.load(Ordering::Relaxed).min(n as u32)
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

    /// A finding no longer describes the project: a track it is about
    /// changed (or went) since the analysis — findings on tracks the
    /// change did not reach stay valid.
    pub fn mix_finding_stale(&self, id: u64) -> bool {
        self.mix.stale_findings.contains(&id)
    }

    /// What the probes measured of every device (processed analyses).
    pub fn mix_devices(&self) -> &[DeviceRecord] {
        self.mix_results().map_or(&[], |r| r.devices.as_slice())
    }

    /// The master against the reference (processed, with a reference).
    pub fn mix_reference(&self) -> Option<&ReferenceComparison> {
        self.mix_results()?.reference.as_ref()
    }

    /// A device's name now ("a removed device" when it is gone).
    pub fn mix_device_name(&self, plugin: PluginInstanceId) -> String {
        self.device_name(plugin)
            .unwrap_or_else(|| "a removed device".into())
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

    /// The tracks Mix Intelligence analyses (roles can be given to them):
    /// audio and instrument tracks, buses and returns.
    pub fn mix_tracks(&self) -> Vec<(TrackId, String, Option<TrackRole>)> {
        self.project
            .tracks
            .iter()
            .filter(|t| analysable(t.kind))
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
        if let Some(what) = r.experiment.hand_off() {
            return Err(format!(
                "“{what}” hands over to another tool: nothing to apply here"
            ));
        }
        if !r.experiment.previewable() {
            return Err("a suggestion to try by hand: nothing to apply".into());
        }
        if self.mix_finding_stale(finding) {
            return Err(self.mix_stale().map_or_else(
                || "the tracks it is about changed: analyse again".to_string(),
                str::to_string,
            ));
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
            // A frozen track still plays through its delay and polarity.
            let aligns = matches!(r.experiment, Experiment::Alignment { .. });
            if t.freeze.is_some() && !aligns {
                return Err(format!("'{}' is frozen: unfreeze it to change it", t.name));
            }
            if let Experiment::Alignment { change_ms, .. } = r.experiment {
                if !t.takes_delay() {
                    return Err(format!("'{}' has no delay or polarity of its own", t.name));
                }
                let now = if t.delay_ms.is_finite() {
                    t.delay_ms
                } else {
                    0.0
                };
                if (now + change_ms).abs() > faderframe_project::MAX_TRACK_DELAY_MS {
                    return Err(format!(
                        "'{}''s delay would go past ±{:.0} ms",
                        t.name,
                        faderframe_project::MAX_TRACK_DELAY_MS
                    ));
                }
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
            if let Some(d) = r.experiment.device() {
                if !t.inserts.iter().any(|s| s.id == d) {
                    return Err(format!(
                        "{} is no longer one of '{}''s devices",
                        self.mix_device_name(d),
                        t.name
                    ));
                }
                if let Experiment::Bypass { .. } = r.experiment
                    && t.inserts.iter().any(|s| s.id == d && s.bypass)
                {
                    return Err(format!("{} is bypassed already", self.mix_device_name(d)));
                }
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
            MixOp::Open {
                finding,
                recommendation,
            } => self.open_mix_hand_off(finding, recommendation)?,
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
                self.mix.stale_findings.clear();
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
                .filter(|t| self.project.track(*t).is_some_and(|t| analysable(t.kind)))
                .collect();
            if f.is_empty() {
                return Err(SessionError::Other(
                    "select the audio or instrument tracks (buses, returns) to analyse first"
                        .into(),
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
                SignalPoint::Processed => Plays::Tapped,
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
                compare: true,
                spans: Vec::new(),
            });
        }
        if analysed.is_empty() {
            return Err(SessionError::Other(
                "no audio or instrument track plays in the range: nothing to analyse".into(),
            ));
        }
        // Buses and returns (processed: tapped in the same render) that an
        // analysed track reaches. A bus is not compared for masking — the
        // tracks it sums are; a return is (its reverb or delay is a sound
        // of its own), never with the tracks it carries.
        if signal == SignalPoint::Processed {
            let edges = base.routing_edges();
            let reaches = |from: TrackId, to: TrackId| {
                let mut stack = vec![from];
                let mut seen = BTreeSet::new();
                while let Some(n) = stack.pop() {
                    if n == to {
                        return true;
                    }
                    if seen.insert(n) {
                        stack.extend(edges.iter().filter(|(x, _)| *x == n).map(|(_, y)| *y));
                    }
                }
                false
            };
            let tracks = analysed.clone();
            for t in base
                .tracks
                .iter()
                .filter(|t| matches!(t.kind, TrackKind::Bus | TrackKind::Aux))
            {
                if !tracks.iter().any(|x| reaches(*x, t.id)) {
                    continue;
                }
                if t.mute {
                    skipped.push((t.id, "muted".into()));
                    continue;
                }
                analysed.push(t.id);
                planned.push(Planned {
                    track: t.id,
                    plays: Plays::Tapped,
                    name: t.name.clone(),
                    role: t.role,
                    focus: focus.is_empty() || focus.contains(&t.id),
                    compare: t.kind == TrackKind::Aux,
                    spans: Vec::new(),
                });
            }
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
                compare: false,
                spans,
            });
        }
        let reference = if signal == SignalPoint::Processed {
            self.mix_reference_plan()
        } else {
            None
        };
        // A newer analysis replaces a running one.
        if let Some(j) = self.mix.job.take() {
            j.progress.render.cancel.store(true, Ordering::Relaxed);
            self.mix.retired.push(j);
        }
        self.mix.generation += 1;
        let extra_plan = extra::plan(
            &|id| self.engine.plugin_is_instrument(id),
            &base,
            &planned,
            signal,
            &focus,
            &self.mix.pairs,
        );
        let progress = Arc::new(Progress {
            render: RenderProgress::default(),
            stage: AtomicU32::new(STAGE_RENDER),
            measuring: AtomicU32::new(0),
            threads: AtomicU32::new(0),
            done: AtomicU32::new(0),
            first: std::sync::Mutex::new(None),
            track: AtomicU32::new(0),
            diagnosing: AtomicBool::new(false),
            extra: extra::ExtraProgress::default(),
        });
        // One pass over the range, or one per track's source.
        let passes = match signal {
            SignalPoint::Processed => 1,
            SignalPoint::Source => planned.len(),
        };
        progress
            .render
            .total
            .store((frames * passes) as u64, Ordering::Relaxed);
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
            reference,
            health: self.mix.health.clone(),
            extra: extra_plan,
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
            started: std::time::Instant::now(),
        });
        self.mix.status = MixStatus::Idle;
        Ok(())
    }

    /// The reference a processed analysis compares the master with: the one
    /// the mix is compared with now, else the project's first.
    fn mix_reference_plan(&self) -> Option<capture::ReferencePlan> {
        let id = self
            .current_reference()
            .or_else(|| self.project.references.tracks.first().map(|r| r.id))?;
        let r = self.project.references.get(id)?;
        let SourceSpec::File { path, frames, .. } = &self.project.sources.get(&r.source)?.spec
        else {
            return None;
        };
        let path = crate::media::resolve(path, self.project_dir().as_deref());
        let (from, to) = r.span(*frames);
        let anchor = match r.sync {
            ReferenceSync::Follow { at } => Some(at - from),
            ReferenceSync::Loop => None,
        };
        Some(capture::ReferencePlan {
            id,
            path,
            from,
            to,
            anchor,
        })
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
            self.set_listening_mix_gain(0.0);
            self.mix.results = None;
            self.mix.selected = None;
            self.mix.dismissed.clear();
            self.mix.applied.clear();
            self.mix.stale = None;
            self.mix.stale_findings.clear();
            self.mix.health = None;
            self.mix.checked = None;
            self.mix.status = MixStatus::Idle;
            self.revision += 1;
        }
        self.mix.retired.retain(|j| !j.handle.is_finished());
        self.take_first_stage();
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
                Ok(mut out) if current => {
                    for k in &out.reused {
                        let _ = self.mix.cache.get(k);
                    }
                    for (k, p) in out.profiles {
                        self.mix.cache.put(k, p);
                    }
                    if let Some(k) = out.health_key {
                        self.mix.health = Some((k, Arc::clone(&out.devices)));
                    }
                    let extra_counts =
                        extra::keep(&mut self.mix.cache, &mut self.mix.pairs, &mut out.extra);
                    let n = out.report.findings.len();
                    let elapsed = job.started.elapsed().as_secs_f64();
                    for note in &out.notes {
                        self.notify(NoticeLevel::Warning, format!("Mix Intelligence: {note}"));
                    }
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
                        fingerprints: out.fingerprints,
                        devices: out.devices,
                        reference: out.reference,
                        renders: out.renders,
                        elapsed,
                        latency: out.latency,
                        phases: out.phases,
                        notes: out.notes,
                        extra: extra_counts,
                        complete: true,
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
                    self.mix.stale_findings.clear();
                    self.mix.checked = None;
                    self.mix.status = MixStatus::Idle;
                    self.notify(
                        NoticeLevel::Info,
                        format!(
                            "Mix Intelligence: {n} finding{} ({}, {elapsed:.1} s)",
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

    /// The first stage's findings of the running job, shown while its
    /// second stage runs (the final results replace them; ids are stable,
    /// so a selection stays).
    fn take_first_stage(&mut self) {
        let Some(job) = &self.mix.job else {
            return;
        };
        if job.generation != self.mix.generation || job.serial != self.project_serial {
            return;
        }
        let Some(first) = job.progress.first.lock().ok().and_then(|mut f| f.take()) else {
            return;
        };
        let results = MixResults {
            report: first.report,
            signal: job.signal,
            scope: job.scope,
            range: job.range,
            start_sample: job.start,
            frames: job.frames,
            rate: job.rate,
            tracks: job.tracks.clone(),
            focus: job.focus.clone(),
            skipped: job.skipped.clone(),
            rendered: first.rendered,
            reused: first.reused,
            config: job.config.clone(),
            algorithm: faderframe_mix_intelligence::ALGORITHM_VERSION,
            serial: job.serial,
            history_revision: job.revision,
            fingerprint: first.fingerprint,
            fingerprints: first.fingerprints,
            devices: first.devices,
            reference: first.reference,
            renders: first.renders,
            elapsed: job.started.elapsed().as_secs_f64(),
            latency: first.latency,
            phases: Vec::new(),
            notes: Vec::new(),
            extra: ExtraCounts::default(),
            complete: false,
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
        self.mix.stale_findings.clear();
        self.mix.checked = None;
        self.revision += 1;
    }

    /// Mark the results stale when the project's audio changed since —
    /// finding by finding: one whose tracks' fingerprints changed (or that
    /// went) no longer describes the project; one on tracks the change did
    /// not reach still does.
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
        let gone: Vec<TrackId> = r
            .tracks
            .iter()
            .copied()
            .filter(|t| self.project.track(*t).is_none())
            .collect();
        let now = if rev == r.history_revision {
            r.fingerprints.clone()
        } else {
            fingerprints(&self.project.song())
        };
        let (project_changed, changed): (bool, Box<dyn Fn(TrackId) -> bool>) =
            match (now, r.fingerprints.clone()) {
                (Some(a), Some(b)) => (
                    a.project != b.project,
                    Box::new(move |t| a.of(t) != b.of(t)),
                ),
                // Nothing to compare by: any change is one.
                _ => {
                    let c = rev != r.history_revision;
                    (c, Box::new(move |_| c))
                }
            };
        let stale: BTreeSet<u64> = r
            .report
            .findings
            .iter()
            .filter(|f| {
                f.tracks.iter().any(|t| gone.contains(t) || changed(*t))
                    || (f.tracks.is_empty() && project_changed)
            })
            .map(|f| f.id)
            .collect();
        let message = if !gone.is_empty() {
            Some("A track analysed was deleted: analyse again to refresh the findings.".to_string())
        } else if !stale.is_empty() {
            Some(format!(
                "The project changed after this analysis: {} of {} findings no longer describe it \
                 (marked) — analyse again to see the mix as it is now (unchanged tracks are \
                 reused).",
                stale.len(),
                r.report.findings.len()
            ))
        } else if project_changed {
            Some(
                "The project changed after this analysis: the findings still describe their \
                 tracks, the master's levels are from before — analyse again to refresh \
                 (unchanged tracks are reused)."
                    .to_string(),
            )
        } else {
            None
        };
        if message != self.mix.stale || stale != self.mix.stale_findings {
            self.mix.stale = message;
            self.mix.stale_findings = stale;
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
            Experiment::GainStage {
                track,
                device,
                trim_db,
            } => {
                use faderframe_plugin_host::devices::utility::id;
                let index = self
                    .project
                    .track(track)
                    .and_then(|t| t.inserts.iter().position(|s| s.id == device))
                    .ok_or_else(|| SessionError::Other("the device is gone".into()))?;
                let before = utility_slot(&mut self.project, &[(id::GAIN, trim_db)]);
                let after = utility_slot(&mut self.project, &[(id::GAIN, -trim_db)]);
                let gain = ParameterId(id::GAIN);
                let toggle = Toggle::Params {
                    track,
                    params: vec![
                        (before.id, gain, 0.0, trim_db),
                        (after.id, gain, 0.0, -trim_db),
                    ],
                };
                Ok((
                    vec![
                        Command::InsertPlugin {
                            track,
                            index,
                            slot: before,
                        },
                        // After the device (now one further on).
                        Command::InsertPlugin {
                            track,
                            index: index + 2,
                            slot: after,
                        },
                    ],
                    toggle,
                ))
            }
            Experiment::DcFilter { track } => {
                use faderframe_plugin_host::devices::utility::id;
                let index = self
                    .project
                    .track(track)
                    .map(|t| t.inserts.len())
                    .ok_or_else(|| SessionError::Other("the track is gone".into()))?;
                let slot = utility_slot(&mut self.project, &[(id::DC, 1.0)]);
                let toggle = Toggle::Params {
                    track,
                    params: vec![(slot.id, ParameterId(id::DC), 0.0, 1.0)],
                };
                Ok((vec![Command::InsertPlugin { track, index, slot }], toggle))
            }
            Experiment::Bypass { track, device } => Ok((
                vec![Command::SetPluginBypass {
                    track,
                    plugin: device,
                    bypass: true,
                }],
                Toggle::Bypass {
                    track,
                    plugin: device,
                },
            )),
            Experiment::CompareReference { .. } | Experiment::ReferenceEqMatch { .. } => Err(
                SessionError::Other("this hands over to another tool: nothing to apply".into()),
            ),
            Experiment::Manual { .. } => Err(SessionError::Other(
                "a suggestion to try by hand: nothing to apply".into(),
            )),
            Experiment::Alignment {
                track,
                change_ms,
                invert,
            } => {
                let t = self
                    .project
                    .track(track)
                    .ok_or_else(|| SessionError::Other("the track is gone".into()))?;
                let delay = if t.delay_ms.is_finite() {
                    t.delay_ms
                } else {
                    0.0
                };
                let original = (delay, t.phase_invert);
                let candidate = (delay + change_ms, t.phase_invert != invert);
                let mut commands = Vec::new();
                if change_ms != 0.0 {
                    commands.push(Command::SetTrackDelay {
                        track,
                        ms: candidate.0,
                    });
                }
                if invert {
                    commands.push(Command::SetTrackPhaseInvert {
                        track,
                        on: candidate.1,
                    });
                }
                Ok((
                    commands,
                    Toggle::Alignment {
                        track,
                        original,
                        candidate,
                    },
                ))
            }
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
        // The candidate is heard against the original, not a reference.
        self.hear_the_mix();
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

    /// The listening gain on the mix the audition asks for now (the
    /// candidate at the original's loudness; see [`Session::listening_gain`]).
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
        self.set_listening_mix_gain(db as f32);
    }

    fn mix_compare(&mut self, on: bool) -> Result<()> {
        let Some(a) = &self.mix.audition else {
            return Ok(());
        };
        if a.candidate == on {
            return Ok(());
        }
        let cmds = match &a.toggle {
            Toggle::Band {
                track,
                plugin,
                band,
            } => vec![Command::SetPluginParameter {
                track: *track,
                plugin: *plugin,
                parameter: band_id(*band, Field::Enabled),
                // On, or bypassed (shown, not heard).
                value: Some(if on { 1.0 } else { 2.0 }),
            }],
            Toggle::Volume {
                track,
                original,
                candidate,
            } => vec![Command::SetTrackVolume {
                track: *track,
                db: if on { *candidate } else { *original },
            }],
            Toggle::Params { track, params } => params
                .iter()
                .map(
                    |&(plugin, parameter, original, candidate)| Command::SetPluginParameter {
                        track: *track,
                        plugin,
                        parameter,
                        value: Some(if on { candidate } else { original }),
                    },
                )
                .collect(),
            Toggle::Bypass { track, plugin } => vec![Command::SetPluginBypass {
                track: *track,
                plugin: *plugin,
                bypass: on,
            }],
            // The delay follows its slot (a crossfade), the polarity at
            // once: both parameter changes.
            Toggle::Alignment {
                track,
                original,
                candidate,
            } => {
                let (ms, invert) = if on { *candidate } else { *original };
                let mut v = Vec::new();
                if original.0 != candidate.0 {
                    v.push(Command::SetTrackDelay { track: *track, ms });
                }
                if original.1 != candidate.1 {
                    v.push(Command::SetTrackPhaseInvert {
                        track: *track,
                        on: invert,
                    });
                }
                v
            }
        };
        // Parameter changes (a bypass): their inverses are the other way,
        // nothing to keep (the audition's inverses restore the project in
        // any case).
        self.apply_unrecorded_mix(cmds)?;
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
        self.set_listening_mix_gain(0.0);
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
        let candidates = a.plugins();
        let own = matches!(
            &action,
            Action::Edit(Command::SetPluginParameter { plugin, .. }) if candidates.contains(plugin)
        );
        if own && let Action::Edit(cmd) = action {
            self.apply_unrecorded_mix(vec![cmd])?;
            self.revision += 1;
            return Ok(None);
        }
        self.end_mix_audition();
        Ok(Some(action))
    }

    /// A hand-off experiment: the reference heard instead of the mix (A/B,
    /// loudness-matched as the reference is set to be), EQ Match opened with
    /// it. Nothing is applied by Mix Intelligence itself.
    fn open_mix_hand_off(&mut self, finding: u64, recommendation: usize) -> Result<()> {
        let exp = self
            .mix_finding(finding)
            .and_then(|f| f.recommendations.get(recommendation))
            .map(|r| r.experiment.clone())
            .ok_or_else(|| SessionError::Other("no such experiment".into()))?;
        match exp {
            Experiment::CompareReference { reference } => {
                self.end_mix_audition();
                if self.project.references.get(reference).is_none() {
                    return Err(SessionError::Other("the reference is gone".into()));
                }
                if self.current_reference() != Some(reference) {
                    self.choose_reference(Some(reference))?;
                }
                self.compare_op(crate::compare::CompareOp::Hear(true))?;
                let matched = self
                    .project
                    .references
                    .get(reference)
                    .is_some_and(|r| r.match_loudness);
                self.notify(
                    NoticeLevel::Info,
                    format!(
                        "Hearing the reference instead of the mix{} — Ctrl+Alt+A flips back",
                        if matched {
                            ", loudness-matched"
                        } else {
                            " (not loudness-matched: its setting is off)"
                        }
                    ),
                );
                Ok(())
            }
            Experiment::ReferenceEqMatch { reference, track } => {
                self.reference_op(crate::references::ReferenceOp::SendToEqMatch {
                    reference,
                    track: Some(track),
                })
            }
            _ => Err(SessionError::Other(
                "not a hand-off: audition or apply it instead".into(),
            )),
        }
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
                self.mix.audition.as_ref().map(|a| a.toggle.clone())
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

    /// The processed analysis as it was before the one pass: each analysed
    /// track solo in place through the whole mix, then the master (N + 1
    /// renders), every profile built on the render's thread, then the
    /// rules. Seconds.
    fn analyse_as_before(base: &Project) -> (f64, usize) {
        use crate::render::{RenderProgress, render_each};
        let rate = base.sample_rate;
        let t0 = std::time::Instant::now();
        let mut sources = faderframe_engine::render_generated_sources(base, rate);
        let failed = crate::media::open_file_sources(base, None, &mut sources);
        assert!(failed.is_empty(), "the media open");
        let frames = base
            .timeline
            .to_samples(base.content_end(), f64::from(rate))
            .max(1) as usize;
        let config = AnalysisConfig::for_rate(rate);
        let progress = RenderProgress::default();
        let tracks: Vec<TrackId> = base
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) && !t.mute)
            .map(|t| t.id)
            .collect();
        let mut profiles = Vec::new();
        for t in tracks.iter().map(Some).chain([None]) {
            let mut x = base.clone();
            for u in &mut x.tracks {
                u.solo = t == Some(&u.id);
            }
            let mut b =
                ProfileBuilder::new(rate, 2, &config, &[]).unwrap_or_else(|e| panic!("{e}"));
            render_each(
                &x,
                rate,
                &sources,
                (0, frames),
                &progress,
                2,
                None,
                false,
                &mut |piece| {
                    let _ = b.push(piece);
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
            profiles.push(b.finish());
        }
        let rules = Rules::default();
        let master = profiles.pop().unwrap_or_else(|| panic!("the master"));
        let input = DiagnoseInput {
            tracks: tracks
                .iter()
                .zip(&profiles)
                .map(|(t, p)| TrackInput {
                    track: *t,
                    role: None,
                    profile: p,
                    compare: true,
                    focus: true,
                })
                .collect(),
            master: Some((None, &master)),
            signal: SignalPoint::Processed,
            sections: Vec::new(),
            rules: &rules,
            cancel: None,
            related: Vec::new(),
        };
        let _ = faderframe_mix_intelligence::diagnose(&input).unwrap_or_else(|e| panic!("{e}"));
        (t0.elapsed().as_secs_f64(), tracks.len() + 1)
    }

    /// The source analysis as it was: each track's source rendered after
    /// the other (each render on every core). Seconds.
    fn sources_as_before(s: &Session, base: &Project) -> f64 {
        use crate::render::{RenderProgress, render_each};
        let rate = base.sample_rate;
        let t0 = std::time::Instant::now();
        let mut sources = faderframe_engine::render_generated_sources(base, rate);
        let failed = crate::media::open_file_sources(base, None, &mut sources);
        assert!(failed.is_empty(), "the media open");
        let frames = base
            .timeline
            .to_samples(base.content_end(), f64::from(rate))
            .max(1) as usize;
        let config = AnalysisConfig::for_rate(rate);
        let progress = RenderProgress::default();
        for t in base
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) && !t.mute)
        {
            let keep = if t.kind == TrackKind::Instrument {
                t.inserts
                    .iter()
                    .position(|x| s.engine.plugin_is_instrument(x.id))
                    .map_or(t.inserts.len(), |i| i + 1)
            } else {
                0
            };
            let Some((project, outputs)) = source_project(base, t.id, keep) else {
                continue;
            };
            let mut b =
                ProfileBuilder::new(rate, outputs, &config, &[]).unwrap_or_else(|e| panic!("{e}"));
            render_each(
                &project,
                rate,
                &sources,
                (0, frames),
                &progress,
                outputs,
                None,
                false,
                &mut |piece| {
                    let _ = b.push(piece);
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
            let _ = b.finish();
        }
        t0.elapsed().as_secs_f64()
    }

    /// The analysis as it is now, through the session: seconds of the job,
    /// its stages, renders, signals.
    fn analyse_now(s: &mut Session) -> (f64, Vec<(&'static str, f64)>, usize, usize) {
        analyse_now_as(s, SignalPoint::Processed)
    }

    fn analyse_now_as(
        s: &mut Session,
        signal: SignalPoint,
    ) -> (f64, Vec<(&'static str, f64)>, usize, usize) {
        s.dispatch(Action::Mix(MixOp::Analyse {
            scope: MixScope::Project,
            signal,
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        s.wait_for_mix();
        let r = s.mix_results().unwrap_or_else(|| panic!("results"));
        (r.elapsed, r.phases.clone(), r.renders, r.tracks.len() + 1)
    }

    /// The demo, and the demo with every track copied three times (four
    /// times the tracks, the same buses and returns).
    fn speed_projects() -> Vec<(&'static str, Session)> {
        let demo = Session::demo(faderframe_engine::EngineConfig::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let mut large = Session::demo(faderframe_engine::EngineConfig::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let tracks: Vec<TrackId> = large
            .project()
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument))
            .map(|t| t.id)
            .collect();
        for _ in 0..3 {
            large
                .dispatch(Action::DuplicateTracks {
                    tracks: tracks.clone(),
                    content: true,
                })
                .unwrap_or_else(|e| panic!("{e}"));
        }
        vec![("demo", demo), ("demo ×4 tracks", large)]
    }

    /// What the one pass, the measuring on several threads, the second
    /// stage's capture renders and the caches save: the processed analysis
    /// (with the phase, timing and dynamics stage) of the demo and of a
    /// project with four times its tracks, as it was (a render per track and
    /// the master, profiles on the render's thread; then sources one after
    /// the other and each pair's tracks solo in place over their excerpt) and
    /// as it is (one render, profiles beside it; sources side by side, one
    /// render per excerpt), then again unchanged and after one fader moved.
    /// Timings only: `cargo test -p faderframe-session --lib one_pass_speed
    /// -- --ignored --nocapture` (and with `--release`).
    #[test]
    #[ignore]
    fn one_pass_speed() {
        let stage = |phases: &[(&'static str, f64)]| {
            phases
                .iter()
                .filter(|(n, _)| *n == "sources and pairs")
                .map(|(_, t)| *t)
                .sum::<f64>()
        };
        let fresh = |s: &Session| {
            Session::new(
                s.project().clone(),
                None,
                faderframe_engine::EngineConfig::default(),
            )
            .unwrap_or_else(|e| panic!("{e}"))
        };
        for (label, warm) in speed_projects() {
            // A first analysis (untimed) loads what every one shares.
            let mut s = fresh(&warm);
            let _ = analyse_now(&mut s);
            let base = {
                let mut b = s.render_copy();
                b.loop_enabled = false;
                for t in &mut b.tracks {
                    t.solo = false;
                }
                b
            };
            let audio = base
                .timeline
                .to_samples(base.content_end(), f64::from(base.sample_rate))
                as f64
                / f64::from(base.sample_rate);
            // Now, on a session that has measured nothing yet.
            let mut s = fresh(&warm);
            let (now, phases, renders, signals) = analyse_now(&mut s);
            let counts = s.mix_results().map(|r| r.extra).unwrap_or_default();
            let (again, _, again_renders, _) = analyse_now(&mut s);
            let first = s
                .project()
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Audio)
                .map(|t| (t.id, t.volume_db))
                .unwrap_or_else(|| panic!("an audio track"));
            s.dispatch(Action::Edit(Command::SetTrackVolume {
                track: first.0,
                db: first.1 - 1.0,
            }))
            .unwrap_or_else(|e| panic!("{e}"));
            let (changed, _, _, _) = analyse_now(&mut s);
            let measured = s.mix_results().map_or(0, |r| r.rendered);
            // Before: the main pass as it was, and the second stage as it
            // was (on a session that has measured nothing).
            let skip_before = std::env::var_os("FADERFRAME_SPEED_NOW_ONLY").is_some();
            let (main_before, renders_before) = if skip_before {
                (f64::NAN, 0)
            } else {
                analyse_as_before(&base)
            };
            let stage_before = if skip_before {
                f64::NAN
            } else {
                let mut old = fresh(&warm);
                extra::AS_BEFORE.store(true, Ordering::Relaxed);
                let (_, p, _, _) = analyse_now(&mut old);
                extra::AS_BEFORE.store(false, Ordering::Relaxed);
                stage(&p)
            };
            let before = main_before + stage_before;
            // Sources (a render per track): one after the other before,
            // side by side now.
            let sources_before = if skip_before {
                f64::NAN
            } else {
                sources_as_before(&s, &base)
            };
            // (On a session of its own: the processed analysis's second
            // stage measured most sources already.)
            let mut src = fresh(&warm);
            let (sources_now, _, source_renders, _) = analyse_now_as(&mut src, SignalPoint::Source);
            println!(
                "{label}: {audio:.1} s of audio, {signals} signals, {} sources beside them, \
                 {} pairs compared\n  \
                 before: {renders_before} renders + the second stage, profiles on the render \
                 thread: {before:.2} s (main {main_before:.2} s, second stage {stage_before:.2} \
                 s; {:.2}× real time)\n  \
                 now: {renders} render + the second stage, profiles on {} threads: {now:.2} s \
                 ({:.2}× real time, {:.1}× faster) — {}\n  \
                 unchanged, everything reused: {again:.2} s ({again_renders} renders)\n  \
                 second stage now: sources {:.2} s, excerpts {:.2} s ({} renders), pairs \
                 {:.2} s\n  \
                 one fader moved: {changed:.2} s ({measured} signals measured again)\n  \
                 source analysis: {source_renders} renders one after the other \
                 {sources_before:.2} s, side by side {sources_now:.2} s ({:.1}× faster)",
                counts.sources_rendered + counts.sources_reused,
                counts.pairs_measured + counts.pairs_reused,
                audio / before.max(1e-9),
                measure::threads(signals),
                audio / now.max(1e-9),
                before / now.max(1e-9),
                phases
                    .iter()
                    .map(|(n, t)| format!("{n} {t:.2} s"))
                    .collect::<Vec<_>>()
                    .join(", "),
                counts.sources_seconds,
                counts.excerpts_seconds,
                counts.excerpt_renders,
                counts.pairs_seconds,
                sources_before / sources_now.max(1e-9)
            );
        }
    }
}
