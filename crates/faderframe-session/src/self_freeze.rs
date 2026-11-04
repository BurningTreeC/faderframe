//! Self-freezing projects: tracks nobody touches play from a disk cache of
//! their processed output, so a heavy project plays on a small machine.
//!
//! **What is cached.** A track's chain — its clips, MIDI effects,
//! instrument and inserts — up to its strip, as a freeze renders it
//! ([`job::trimmed`]: the track and what reaches its chain alone, at unity
//! into a plain master, the latency kept at the front): fader, pan, sends,
//! mute and polarity stay live. The engine plays the file in the chain's
//! place ([`faderframe_engine::cache`]), reporting the chain's live
//! latency, so delay compensation and everything downstream are as they
//! were; the plugins stay loaded.
//!
//! **When.** Automatically and invisibly. Once the project has been left
//! alone for [`SETTLE`] (no edit in the undo history), a background job
//! renders one track at a time — on a thread below the UI's priority,
//! tracks playing around the playhead first while playing, then the
//! costliest — and only chains worth it: the render measures what the
//! chain's devices cost (node timing) and drops caches of chains cheaper
//! than the minimum. A cache found in the folder for the same key is used
//! without rendering (after undo, or reopening the project).
//!
//! **Exactly what the chain makes.** A cache is played only while the
//! project still makes what it was made from: every sync compares the
//! track's [`key::ChainView`] with the project in place (clips, media,
//! devices and their state, the automation and modulators driving them,
//! the MIDI tracks playing it, sidechain and follower sources, tempo, key
//! and chords, the rates) and an edit sends the track back to its chain
//! in the same sync, before it is heard (the edit fades in once the
//! chain's latency is filled). A track also goes back when anything live
//! touches it ([`Live`]): an editor open, automation being written,
//! record arm, input monitoring, live MIDI or an audition, a key or
//! followed track that is live — gently then: the cache, still right,
//! stays heard while the chain plays through its devices' tails.
//!
//! **Third-party plugins.** Their state blobs are part of the key; a
//! sandboxed plugin whose state is still being fetched is not rendered
//! yet. A plugin that reads the room, rolls dice or follows the clock is
//! rendered as one performance of it (a freeze would be too); the user
//! can keep a track live ([`faderframe_project::Track::no_self_freeze`])
//! or every track using a plugin ([`SelfFreezeSettings::never`]). A track
//! whose plugins' saved state changes when nothing was edited (the cache
//! falls stale on its own, [`CHURN`] times) stays live for the session. A
//! chain that never falls silent (a generator, hiss) is never cached: a
//! file could not stand for it outside its span.
//!
//! **Storage.** [`store`]: `<project>/Cache`, a size limit with the least
//! recently used files going first, cleaned with unused media.

mod job;
mod key;
pub mod store;

pub use store::CACHE_FOLDER;

use crate::{NoticeLevel, Session};
use faderframe_core::{ChannelLayout, PluginInstanceId, TrackId};
use faderframe_engine::cache::{CacheState, CachedChain};
use faderframe_project::{Impact, InputRouting, MonitorMode, SendTap, TrackKind};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The project left alone this long before a background render starts.
pub const SETTLE: Duration = Duration::from_secs(2);
/// After a stop, tails ring on this long before a track fades to its cache.
pub const STOP_QUIET: Duration = Duration::from_secs(2);
/// Caches that fall stale with nothing edited, before a track stays live.
pub const CHURN: u32 = 3;
/// How often the scheduler looks for work.
const LOOK_EVERY: Duration = Duration::from_millis(250);
/// When Needed: the audio load (share of the callback's time, smoothed)
/// that has the machine struggling …
pub const NEED_LOAD: f64 = 0.6;
/// … held this long (a moment's peak is not a struggle).
pub const NEED_FOR: Duration = Duration::from_secs(3);
/// When Needed: how long caches go on being made after the load or a
/// dropout asked for them (one track is rendered at a time; the load is
/// looked at again with what was cached).
pub const NEED_HOLD: Duration = Duration::from_secs(30);

/// When self-freezing makes caches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelfFreezeMode {
    /// Only while the machine struggles: the audio load above
    /// [`NEED_LOAD`] for [`NEED_FOR`], or a dropout while playing. Caches
    /// made stay while they are right; when the load is low again no new
    /// ones are made, so a machine that copes hears every chain live.
    #[default]
    WhenNeeded,
    /// Every track worth a cache, whatever the load.
    Always,
}

impl SelfFreezeMode {
    pub fn label(self) -> &'static str {
        match self {
            SelfFreezeMode::WhenNeeded => "When Needed",
            SelfFreezeMode::Always => "Always",
        }
    }
}

/// Self-freezing as the user set it (Preferences → Audio).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SelfFreezeSettings {
    pub on: bool,
    /// When caches are made (settings saved before it: When Needed).
    pub mode: SelfFreezeMode,
    /// The cache folder's size limit.
    pub max_megabytes: u64,
    /// Chains cheaper than this (percent of one core in real time) are not
    /// worth a cache.
    pub min_cost_percent: f32,
    /// Plugins (by plugin id) whose tracks always play live.
    pub never: Vec<String>,
}

impl Default for SelfFreezeSettings {
    fn default() -> Self {
        Self {
            on: true,
            mode: SelfFreezeMode::WhenNeeded,
            max_megabytes: 4096,
            min_cost_percent: 3.0,
            never: Vec::new(),
        }
    }
}

impl SelfFreezeSettings {
    fn limit_bytes(&self) -> u64 {
        self.max_megabytes.saturating_mul(1024 * 1024)
    }

    fn min_cost(&self) -> f64 {
        f64::from(self.min_cost_percent.max(0.0)) / 100.0
    }
}

/// Why a track plays its chain rather than a cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Live {
    /// Self-freezing is off.
    Off,
    /// Only audio and instrument tracks have chains to cache.
    Kind,
    Frozen,
    /// The user keeps it live (the track's menu).
    KeptLive,
    /// One of its plugins is set never to be cached.
    PluginKeptLive,
    /// Not mono or stereo after its devices.
    Format,
    /// A track delay plays before its devices.
    Delay,
    Armed,
    Monitoring,
    LiveMidi,
    Editing,
    Auditioning,
    WritingAutomation,
    PreFxSend,
    MidiOutput,
    Launcher,
    PluginOutputs,
    Outboard,
    /// Other tracks' audio reaches its input.
    Fed,
    /// Modulators move its fader or pan.
    StripModulation,
    /// A track keying or followed by it is live.
    SourceLive,
    /// No device to run.
    NoDevices,
    NoClips,
    /// Its devices' saved state changes on its own.
    Unstable,
    /// Its devices never fall silent.
    Sounding,
    /// Not worth a cache.
    Cheap,
    /// When Needed, and the machine copes.
    NotNeeded,
    /// An ARA plug-in renders it (its edits live in the plug-in's
    /// document, which a cache would not follow).
    Ara,
}

impl Live {
    pub fn label(self) -> &'static str {
        match self {
            Live::Off => "self-freezing is off",
            Live::Kind => "no chain to cache",
            Live::Frozen => "frozen",
            Live::KeptLive => "kept live",
            Live::PluginKeptLive => "a plugin kept live",
            Live::Format => "surround chain",
            Live::Delay => "track delay",
            Live::Armed => "armed",
            Live::Monitoring => "input monitoring",
            Live::LiveMidi => "live MIDI",
            Live::Editing => "editor open",
            Live::Auditioning => "auditioning",
            Live::WritingAutomation => "writing automation",
            Live::PreFxSend => "pre-FX send",
            Live::MidiOutput => "MIDI output",
            Live::Launcher => "launcher clips",
            Live::PluginOutputs => "plugin outputs",
            Live::Outboard => "outboard gear",
            Live::Fed => "fed by other tracks",
            Live::StripModulation => "modulated fader or pan",
            Live::SourceLive => "its key is live",
            Live::NoDevices => "no devices",
            Live::NoClips => "nothing to play",
            Live::Unstable => "plugin state keeps changing",
            Live::Sounding => "never falls silent",
            Live::Cheap => "light enough",
            Live::NotNeeded => "the machine copes (self-freezing when needed)",
            Live::Ara => "an ARA plug-in renders it",
        }
    }
}

/// Where a track is with self-freezing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackCache {
    Live(Live),
    /// Waiting for its render (or for the project to settle).
    Waiting,
    /// Being rendered (done share).
    Rendering(f32),
    /// Fading to its cache.
    ToCache,
    /// Playing from its cache; what its chain cost live (a share of one
    /// core).
    Cached {
        cost: f32,
    },
    /// Fading back to its chain.
    ToLive,
}

impl TrackCache {
    /// Played from a cache now (fading to or from it included).
    pub fn is_cached(self) -> bool {
        matches!(
            self,
            TrackCache::Cached { .. } | TrackCache::ToCache | TrackCache::ToLive
        )
    }
}

/// Self-freezing at a glance (the Performance view, the status line).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelfFreezeSummary {
    pub on: bool,
    pub mode: SelfFreezeMode,
    /// Caches are being made now (always, or the machine struggles).
    pub needed: bool,
    /// Tracks playing from their caches.
    pub cached: Vec<TrackId>,
    /// Tracks fading to or from them.
    pub switching: usize,
    pub rendering: Option<(TrackId, f32)>,
    /// What the cached tracks' chains cost live (shares of one core).
    pub saved: f32,
    /// The cache folder's size and limit.
    pub bytes: u64,
    pub limit: u64,
    pub files: usize,
}

struct Installed {
    key: u64,
    view: key::ChainView,
    cost: f32,
    /// The history revision when it was installed.
    revision: u64,
}

pub(crate) struct SelfFreezeState {
    settings: SelfFreezeSettings,
    store: Option<store::Store>,
    installed: HashMap<TrackId, Installed>,
    job: Option<job::Job>,
    /// Keys found not worth a cache or sounding on their own.
    verdicts: HashMap<u64, Live>,
    /// The latest of those per track (its key then: shown until the
    /// scheduler finds the track's key changed).
    judged: HashMap<TrackId, (u64, Live)>,
    /// Tracks' keys as of the history revision `keys_at` (hashing a view
    /// is not free: worked out again only after an edit; a render and an
    /// install check the key afresh).
    keys: HashMap<TrackId, u64>,
    keys_at: Option<u64>,
    /// Keys whose render failed (and how often).
    failed: HashMap<u64, u32>,
    /// The latest measured cost per track (whatever its key): the order
    /// renders go in.
    costs: HashMap<TrackId, f32>,
    editing: HashSet<PluginInstanceId>,
    churn: HashMap<TrackId, u32>,
    unstable: HashSet<TrackId>,
    /// The history revision last seen, and since when.
    revision: (u64, Instant),
    stopped_at: Option<Instant>,
    was_playing: bool,
    looked: Option<Instant>,
    engine_id: usize,
    /// Do not wait for the project to settle (a dev action, tests).
    hurry: bool,
    /// The scheduler found nothing to do last time it looked.
    idle: bool,
    /// When Needed: what the load said.
    need: Need,
}

/// When Needed's reading of the machine.
#[derive(Default)]
struct Need {
    /// The load above [`NEED_LOAD`] since.
    high_since: Option<Instant>,
    /// Caches are made until then.
    until: Option<Instant>,
    /// Dropouts counted so far (xruns and late callbacks).
    dropouts: u64,
}

impl Default for SelfFreezeState {
    fn default() -> Self {
        Self {
            settings: SelfFreezeSettings {
                on: false,
                ..SelfFreezeSettings::default()
            },
            store: None,
            installed: HashMap::new(),
            job: None,
            verdicts: HashMap::new(),
            judged: HashMap::new(),
            keys: HashMap::new(),
            keys_at: None,
            failed: HashMap::new(),
            costs: HashMap::new(),
            editing: HashSet::new(),
            churn: HashMap::new(),
            unstable: HashSet::new(),
            revision: (0, Instant::now()),
            stopped_at: None,
            was_playing: false,
            looked: None,
            engine_id: 0,
            hurry: false,
            idle: true,
            need: Need::default(),
        }
    }
}

fn engine_id(s: &Session) -> usize {
    Arc::as_ptr(&s.engine.shared()) as usize
}

impl Session {
    // --- settings and status ---------------------------------------------------------

    pub fn self_freeze_settings(&self) -> &SelfFreezeSettings {
        &self.self_freeze.settings
    }

    /// Change self-freezing (off: every track back to its chain, gently).
    pub fn set_self_freeze(&mut self, settings: SelfFreezeSettings) {
        let was = std::mem::replace(&mut self.self_freeze.settings, settings);
        let s = &self.self_freeze.settings;
        if was.never != s.never || was.min_cost_percent != s.min_cost_percent {
            // Cheapness was judged against the old minimum.
            self.self_freeze.verdicts.clear();
            self.self_freeze.judged.clear();
        }
        if !self.self_freeze.settings.on {
            if let Some(job) = self.self_freeze.job.take() {
                job.cancel();
            }
            self.release_all_caches();
        }
        self.evict_caches();
        self.self_freeze.idle = false;
        self.revision += 1;
    }

    /// Render now what can be cached, without waiting for the project to
    /// settle (`self-freeze:now`).
    pub fn self_freeze_now(&mut self) {
        self.self_freeze.need.until = Some(Instant::now() + NEED_HOLD);
        self.self_freeze.hurry = true;
        self.self_freeze.idle = false;
        self.self_freeze.looked = None;
    }

    /// Where `track` is with self-freezing.
    pub fn self_freeze_track(&self, track: TrackId) -> TrackCache {
        match self.engine.cache_state(track) {
            Some(CacheState::ToCache) => return TrackCache::ToCache,
            Some(CacheState::ToLive) => return TrackCache::ToLive,
            Some(CacheState::Cached) => {
                let cost = self
                    .self_freeze
                    .installed
                    .get(&track)
                    .map_or(0.0, |i| i.cost);
                return TrackCache::Cached { cost };
            }
            None => {}
        }
        if let Some(j) = self.self_freeze.job.as_ref().filter(|j| j.track == track) {
            return TrackCache::Rendering(j.progress.fraction());
        }
        if let Some(why) = self.why_live(track) {
            return TrackCache::Live(why);
        }
        if !self.self_freeze_needed() {
            return TrackCache::Live(Live::NotNeeded);
        }
        match self.self_freeze.judged.get(&track) {
            Some((_, v)) => TrackCache::Live(*v),
            None => TrackCache::Waiting,
        }
    }

    /// Played from its cache now (fading to or from it included): cheap
    /// enough for every paint.
    pub fn is_self_frozen(&self, track: TrackId) -> bool {
        self.engine.cache_state(track).is_some()
    }

    /// `track`'s key now (from the history revision's keys when known).
    fn key_of(&mut self, track: TrackId) -> u64 {
        let revision = self.history.revision();
        if self.self_freeze.keys_at != Some(revision) {
            self.self_freeze.keys.clear();
            self.self_freeze.keys_at = Some(revision);
        }
        if let Some(k) = self.self_freeze.keys.get(&track) {
            return *k;
        }
        let k = key::ChainView::of(&self.project, track, self.engine.sample_rate()).key();
        self.self_freeze.keys.insert(track, k);
        k
    }

    /// Self-freezing at a glance.
    pub fn self_freeze_summary(&self) -> SelfFreezeSummary {
        let states = self.engine.cache_states();
        let cached: Vec<TrackId> = states
            .iter()
            .filter(|(_, s)| *s == CacheState::Cached)
            .map(|(t, _)| *t)
            .collect();
        let saved = cached
            .iter()
            .filter_map(|t| self.self_freeze.installed.get(t))
            .map(|i| i.cost)
            .sum();
        let store = self.self_freeze.store.as_ref();
        SelfFreezeSummary {
            on: self.self_freeze.settings.on,
            mode: self.self_freeze.settings.mode,
            needed: self.self_freeze.settings.on && self.self_freeze_needed(),
            switching: states.len() - cached.len(),
            cached,
            rendering: self
                .self_freeze
                .job
                .as_ref()
                .map(|j| (j.track, j.progress.fraction())),
            saved,
            bytes: store.map_or(0, store::Store::bytes),
            limit: self.self_freeze.settings.limit_bytes(),
            files: store.map_or(0, store::Store::len),
        }
    }

    /// The cache a track plays (tests, the benchmark).
    pub fn self_freeze_chain(&self, track: TrackId) -> Option<Arc<CachedChain>> {
        self.engine.cached_chain(track)
    }

    /// Run the session's tick until self-freezing has nothing left to do
    /// (renders done, fades over) or `timeout` passes: `true` when idle.
    pub fn wait_for_self_freeze(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            self.tick(0.01);
            let fading = self
                .engine
                .cache_states()
                .iter()
                .any(|(_, s)| *s != CacheState::Cached);
            if self.self_freeze.job.is_none() && self.self_freeze.idle && !fading {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // --- what keeps a track live ---------------------------------------------------

    /// Why `track` plays its chain now (`None`: it may be cached).
    pub fn why_live(&self, track: TrackId) -> Option<Live> {
        let p = &self.project;
        if !self.self_freeze.settings.on {
            return Some(Live::Off);
        }
        let t = p.track(track)?;
        if !matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) {
            return Some(Live::Kind);
        }
        if t.freeze.is_some() {
            return Some(Live::Frozen);
        }
        if t.no_self_freeze {
            return Some(Live::KeptLive);
        }
        if self.self_freeze.unstable.contains(&track) {
            return Some(Live::Unstable);
        }
        let slots = t.slots();
        let never = &self.self_freeze.settings.never;
        if slots.iter().any(|s| never.contains(&s.plugin.id)) {
            return Some(Live::PluginKeptLive);
        }
        if slots.iter().any(|s| p.ara.slots.contains(&s.id)) {
            return Some(Live::Ara);
        }
        let plain = |l: ChannelLayout| matches!(l, ChannelLayout::Mono | ChannelLayout::Stereo);
        if !plain(t.layout) || !plain(t.chain_layout()) {
            return Some(Live::Format);
        }
        if t.delay_ms != 0.0 {
            return Some(Live::Delay);
        }
        if let Some(why) = self.touched_live(t) {
            return Some(why);
        }
        if slots
            .iter()
            .any(|s| self.self_freeze.editing.contains(&s.id))
        {
            return Some(Live::Editing);
        }
        if self
            .take_audition
            .as_ref()
            .is_some_and(|a| p.clip(a.clip).is_some_and(|c| c.track == track))
            || self.auditioning(track)
        {
            return Some(Live::Auditioning);
        }
        if t.automation.lanes.iter().any(|l| {
            self.writing_automation(l.id) || l.mode == faderframe_automation::AutomationMode::Write
        }) {
            return Some(Live::WritingAutomation);
        }
        let prefx = |tap: SendTap| tap == SendTap::PreFx;
        if t.sends.iter().any(|s| s.enabled && prefx(s.tap))
            || t.cue_sends
                .iter()
                .any(|s| p.cue_mixes.iter().any(|c| c.id == s.cue && prefx(c.tap)))
        {
            return Some(Live::PreFxSend);
        }
        if t.midi_output.is_some() {
            return Some(Live::MidiOutput);
        }
        if p.launcher.slots.keys().any(|k| k.track == track) {
            return Some(Live::Launcher);
        }
        if t.input.plugin_output().is_some()
            || slots
                .iter()
                .any(|s| !p.plugin_output_tracks(s.id).is_empty())
        {
            return Some(Live::PluginOutputs);
        }
        if slots
            .iter()
            .any(|s| s.plugin.id == faderframe_core::builtin::HARDWARE_INSERT)
        {
            return Some(Live::Outboard);
        }
        // (MIDI tracks playing it are part of its chain's closure.)
        if p.tracks.iter().any(|f| {
            f.id != track
                && f.kind != TrackKind::Midi
                && (p.output_target(f) == Some(track) || f.sends.iter().any(|s| s.target == track))
        }) {
            return Some(Live::Fed);
        }
        if t.modulators.iter().any(|m| {
            m.routes.iter().any(|r| {
                !matches!(
                    r.target,
                    faderframe_project::modulation::ModTarget::Plugin { .. }
                )
            })
        }) {
            return Some(Live::StripModulation);
        }
        // What reaches its chain: live, or fed itself.
        for id in key::chain_closure(p, track) {
            if id == track {
                continue;
            }
            let Some(c) = p.track(id) else { continue };
            if self.touched_live(c).is_some()
                || (c.kind != TrackKind::Midi
                    && !matches!(c.kind, TrackKind::Audio | TrackKind::Instrument))
            {
                return Some(Live::SourceLive);
            }
            if c.kind != TrackKind::Midi
                && c.slots()
                    .iter()
                    .any(|s| self.self_freeze.editing.contains(&s.id))
            {
                return Some(Live::SourceLive);
            }
        }
        if !slots.iter().any(|s| !s.bypass) {
            return Some(Live::NoDevices);
        }
        if self.chain_span(track).is_none() {
            return Some(Live::NoClips);
        }
        None
    }

    /// Armed, monitored or played live.
    fn touched_live(&self, t: &faderframe_project::Track) -> Option<Live> {
        if t.record_arm {
            return Some(Live::Armed);
        }
        if matches!(t.input, InputRouting::Hardware { .. }) && t.monitor != MonitorMode::Off {
            return Some(Live::Monitoring);
        }
        if self.engine.midi_live().contains(&t.id) {
            return Some(Live::LiveMidi);
        }
        None
    }

    /// The timeline samples (at the engine's rate) the clips playing into
    /// `track`'s chain span: its own and the MIDI tracks' playing it.
    fn chain_span(&self, track: TrackId) -> Option<(i64, i64)> {
        let p = &self.project;
        let rate = f64::from(self.engine.sample_rate());
        let mut span: Option<(i64, i64)> = None;
        for id in key::chain_closure(p, track) {
            let Some(t) = p.track(id) else { continue };
            if id != track && t.kind != TrackKind::Midi {
                continue;
            }
            for c in t.clips.iter().filter_map(|c| p.clips.get(c)) {
                if c.muted {
                    continue;
                }
                let a = p.timeline.to_samples(c.start, rate);
                let b = p
                    .timeline
                    .to_samples(c.end(&p.timeline, p.sample_rate), rate);
                span = Some(span.map_or((a, b), |(x, y)| (x.min(a), y.max(b))));
            }
        }
        span.filter(|(a, b)| b > a)
    }

    // --- hooks from the rest of the session --------------------------------------------

    /// The plugins whose editors are open (from the UI, every tick).
    pub(crate) fn note_plugins_being_edited(&mut self, plugins: &HashSet<PluginInstanceId>) {
        if &self.self_freeze.editing != plugins {
            self.self_freeze.editing = plugins.clone();
        }
    }

    /// A track is played by hand (an audition): back to its chain at
    /// once.
    pub(crate) fn wake_cached(&mut self, track: TrackId) {
        if self.self_freeze.installed.remove(&track).is_some()
            && self.engine.set_cached(track, None)
            && let Err(e) = self.sync(Impact::Graph)
        {
            self.notify(NoticeLevel::Error, e.to_string());
        }
    }

    /// Before every sync: tracks whose cache no longer stands for their
    /// chain go back to it in this very sync (`impact` grows to `Graph`).
    pub(crate) fn self_freeze_before_sync(&mut self, impact: Impact) -> Impact {
        self.notice_new_engine();
        if self.self_freeze.installed.is_empty() {
            return impact;
        }
        let rate = self.engine.sample_rate();
        let revision = self.history.revision();
        let mut stale = Vec::new();
        let mut touched = Vec::new();
        for (t, i) in &self.self_freeze.installed {
            if !i.view.holds(&self.project, *t, rate) {
                stale.push((*t, i.revision == revision));
            } else if self.why_live(*t).is_some() {
                touched.push(*t);
            }
        }
        if stale.is_empty() && touched.is_empty() {
            return impact;
        }
        for (t, on_its_own) in stale {
            self.self_freeze.installed.remove(&t);
            self.engine.set_cached(t, None);
            if on_its_own {
                let n = self.self_freeze.churn.entry(t).or_insert(0);
                *n += 1;
                if *n >= CHURN {
                    self.self_freeze.unstable.insert(t);
                }
            }
        }
        for t in touched {
            self.self_freeze.installed.remove(&t);
            self.engine.release_cache(&self.project, t);
        }
        self.self_freeze.idle = false;
        Impact::Graph
    }

    /// Another project is about to be played: every cache forgotten (the
    /// new project's folder is opened on the next tick).
    pub(crate) fn self_freeze_project_replaced(&mut self) {
        if let Some(job) = self.self_freeze.job.take() {
            job.cancel();
        }
        self.self_freeze.installed.clear();
        self.engine.clear_cache();
        self.self_freeze.store = None;
        self.self_freeze.verdicts.clear();
        self.self_freeze.judged.clear();
        self.self_freeze.keys.clear();
        self.self_freeze.keys_at = None;
        self.self_freeze.failed.clear();
        self.self_freeze.costs.clear();
        self.self_freeze.churn.clear();
        self.self_freeze.unstable.clear();
        self.self_freeze.idle = false;
    }

    /// The engine's rate changes: caches made at the old one are not
    /// played (the engine forgets them before its next graph).
    pub(crate) fn self_freeze_rate_changing(&mut self) {
        if let Some(job) = self.self_freeze.job.take() {
            job.cancel();
        }
        self.self_freeze.installed.clear();
        self.engine.clear_cache();
        self.self_freeze.idle = false;
    }

    /// A save put the project into `dir`: the cache folder goes along
    /// (before a scratch folder is removed).
    pub(crate) fn move_self_freeze_cache(&mut self, dir: &Path) {
        let to = dir.join(CACHE_FOLDER);
        if self.self_freeze.store.is_none() {
            let from = self.cache_dir();
            if from.join("index.json").is_file() {
                self.self_freeze.store = Some(store::Store::open(&from));
            }
        }
        if let Some(s) = &mut self.self_freeze.store {
            s.move_to(&to);
        }
    }

    /// Cleaning up unused media: cache files no track would play now go.
    /// Returns the bytes freed.
    pub fn clean_self_freeze_cache(&mut self) -> u64 {
        let keep = self.current_keys();
        self.open_store();
        self.self_freeze
            .store
            .as_mut()
            .map_or(0, |s| s.keep_only(&keep))
    }

    /// The keys of the caches the project's tracks would play now, and of
    /// what is played or rendered.
    fn current_keys(&self) -> HashSet<u64> {
        let rate = self.engine.sample_rate();
        let mut keep: HashSet<u64> = self.self_freeze.installed.values().map(|i| i.key).collect();
        keep.extend(self.self_freeze.job.as_ref().map(|j| j.key));
        for t in &self.project.tracks {
            if matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) {
                keep.insert(key::ChainView::of(&self.project, t.id, rate).key());
            }
        }
        keep
    }

    /// Cache files no track of the project would play now, with their
    /// sizes: they go with the unused media when the project closes.
    pub fn self_freeze_stale_files(&self) -> Vec<(PathBuf, u64)> {
        let keep = self.current_keys();
        match &self.self_freeze.store {
            Some(s) if s.dir() == self.cache_dir() => s.files_except(&keep),
            _ => store::Store::open(&self.cache_dir()).files_except(&keep),
        }
    }

    // --- the tick --------------------------------------------------------------------

    /// From the session's tick: fades that ended, renders that finished,
    /// tracks touched live, the next render.
    pub(crate) fn tick_self_freeze(&mut self) {
        self.notice_new_engine();
        // No device: nothing would move a fade.
        if self.audio.is_none() {
            self.engine.skip_cache_fades();
        }
        if self.engine.settle_cache()
            && let Err(e) = self.sync(Impact::Graph)
        {
            self.notify(NoticeLevel::Error, e.to_string());
        }
        let playing = self.transport.playing;
        if self.self_freeze.was_playing && !playing {
            self.self_freeze.stopped_at = Some(Instant::now());
        }
        self.self_freeze.was_playing = playing;
        self.poll_self_freeze_job();
        if !self.self_freeze.settings.on {
            if !self.self_freeze.installed.is_empty() {
                self.release_all_caches();
            }
            self.self_freeze.idle = true;
            return;
        }
        // Touched live without a sync (an editor opened, a track made
        // live for MIDI): back gently.
        let touched: Vec<TrackId> = self
            .self_freeze
            .installed
            .keys()
            .copied()
            .filter(|t| self.why_live(*t).is_some())
            .collect();
        if !touched.is_empty() {
            for t in touched {
                self.self_freeze.installed.remove(&t);
                self.engine.release_cache(&self.project, t);
            }
            if let Err(e) = self.sync(Impact::Graph) {
                self.notify(NoticeLevel::Error, e.to_string());
            }
        }
        self.read_need();
        let revision = self.history.revision();
        if self.self_freeze.revision.0 != revision {
            self.self_freeze.revision = (revision, Instant::now());
        }
        if self
            .self_freeze
            .looked
            .is_some_and(|at| at.elapsed() < LOOK_EVERY)
        {
            return;
        }
        self.self_freeze.looked = Some(Instant::now());
        self.schedule_self_freeze();
    }

    /// Caches are made now: always, or When Needed while the machine
    /// struggles (or `self_freeze_now` asked).
    pub fn self_freeze_needed(&self) -> bool {
        match self.self_freeze.settings.mode {
            SelfFreezeMode::Always => true,
            SelfFreezeMode::WhenNeeded => self
                .self_freeze
                .need
                .until
                .is_some_and(|until| Instant::now() < until),
        }
    }

    /// When Needed: read the audio load and the dropouts (the performance
    /// figures are measured whether or not anyone looks).
    fn read_need(&mut self) {
        let now = Instant::now();
        let report = &self.perf.report;
        let dropouts = report.xruns + report.deadline_misses;
        let running = report.running;
        let load = report.total.average;
        let playing = self.transport.playing;
        let was = self.self_freeze_needed();
        let need = &mut self.self_freeze.need;
        if dropouts > need.dropouts && running && playing {
            need.until = Some(now + NEED_HOLD);
        }
        need.dropouts = dropouts;
        if running && load >= NEED_LOAD {
            let since = *need.high_since.get_or_insert(now);
            if now.duration_since(since) >= NEED_FOR {
                need.until = Some(now + NEED_HOLD);
            }
        } else {
            need.high_since = None;
        }
        if !was && self.self_freeze_needed() {
            self.wake_self_freeze();
        }
    }

    /// Caches are wanted from now: the scheduler looks at once.
    fn wake_self_freeze(&mut self) {
        self.self_freeze.idle = false;
        self.self_freeze.looked = None;
    }

    /// When Needed as if the load had held `load` for long enough (tests:
    /// the dummy device's load is whatever the machine gives it).
    pub fn simulate_self_freeze_load(&mut self, load: f64) {
        let now = Instant::now();
        let need = &mut self.self_freeze.need;
        if load >= NEED_LOAD {
            need.until = Some(now + NEED_HOLD);
            self.wake_self_freeze();
        } else {
            need.until = None;
            need.high_since = None;
        }
    }

    /// A new engine (replaced when the device changed): it plays every
    /// chain, so nothing is installed any more.
    fn notice_new_engine(&mut self) {
        let id = engine_id(self);
        if self.self_freeze.engine_id != id {
            self.self_freeze.engine_id = id;
            self.self_freeze.installed.clear();
        }
    }

    /// Where the caches are written (inside the media folder while the
    /// project is unsaved: the media browser leaves it out).
    pub(crate) fn cache_dir(&self) -> PathBuf {
        match self.project_dir() {
            Some(dir) if !self.unsaved_media => dir.join(CACHE_FOLDER),
            _ => self.media_dir.join(CACHE_FOLDER),
        }
    }

    fn open_store(&mut self) {
        let dir = self.cache_dir();
        if self
            .self_freeze
            .store
            .as_ref()
            .is_none_or(|s| s.dir() != dir)
        {
            self.self_freeze.store = Some(store::Store::open(&dir));
        }
    }

    fn release_all_caches(&mut self) {
        let tracks: Vec<TrackId> = self.self_freeze.installed.keys().copied().collect();
        self.self_freeze.installed.clear();
        let mut changed = false;
        for t in tracks {
            changed |= self.engine.release_cache(&self.project, t);
        }
        if changed && let Err(e) = self.sync(Impact::Graph) {
            self.notify(NoticeLevel::Error, e.to_string());
        }
    }

    fn evict_caches(&mut self) {
        let mut keep: HashSet<u64> = self.self_freeze.installed.values().map(|i| i.key).collect();
        keep.extend(self.self_freeze.job.as_ref().map(|j| j.key));
        let limit = self.self_freeze.settings.limit_bytes();
        if let Some(s) = &mut self.self_freeze.store {
            s.evict(limit, &keep);
        }
    }

    /// Look for work: tracks whose caches are in the folder are played
    /// from them; otherwise the most worthwhile track is rendered.
    fn schedule_self_freeze(&mut self) {
        if self.self_freeze.job.is_some() {
            return;
        }
        // When Needed with a machine that copes: what is cached stays,
        // nothing new is made (a render under way finishes).
        if !self.self_freeze_needed() {
            self.self_freeze.idle = true;
            self.self_freeze.hurry = false;
            return;
        }
        let settled = self.self_freeze.hurry || self.self_freeze.revision.1.elapsed() >= SETTLE;
        if !settled || self.transport.recording || self.history.in_gesture() {
            self.self_freeze.idle = false;
            return;
        }
        self.open_store();
        let rate = self.engine.sample_rate();
        let playing = self.transport.playing;
        // Tails still ringing after a stop: fading to a cache now would
        // cut them.
        let quiet = playing
            || self
                .self_freeze
                .stopped_at
                .is_none_or(|at| at.elapsed() >= STOP_QUIET);
        let mut best: Option<(bool, f32, TrackId, u64)> = None;
        let mut installs = Vec::new();
        let mut waiting = false;
        let candidates: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument))
            .map(|t| t.id)
            .filter(|t| self.engine.cache_state(*t).is_none())
            .collect();
        for t in candidates {
            if self.why_live(t).is_some() {
                continue;
            }
            let k = self.key_of(t);
            if self
                .self_freeze
                .judged
                .get(&t)
                .is_some_and(|(judged, _)| *judged != k)
            {
                self.self_freeze.judged.remove(&t);
            }
            if self.self_freeze.verdicts.contains_key(&k)
                || self.self_freeze.failed.get(&k).is_some_and(|n| *n >= 2)
            {
                continue;
            }
            if self
                .self_freeze
                .store
                .as_ref()
                .and_then(|s| s.get(k))
                .is_some_and(|e| e.rate == rate)
            {
                if quiet {
                    installs.push((t, k));
                } else {
                    waiting = true;
                }
                continue;
            }
            let around = playing
                && self
                    .chain_span(t)
                    .is_some_and(|(a, b)| (a..b).contains(&self.transport.position));
            let cost = self.self_freeze.costs.get(&t).copied().unwrap_or_else(|| {
                self.project
                    .track(t)
                    .map_or(0, |t| t.slots().iter().filter(|s| !s.bypass).count())
                    as f32
                    * 1e-3
            });
            if best.as_ref().is_none_or(|b| (around, cost) > (b.0, b.1)) {
                best = Some((around, cost, t, k));
            }
        }
        let mut changed = false;
        for (t, k) in installs {
            changed |= self.install_cache(t, k);
        }
        if changed && let Err(e) = self.sync(Impact::Graph) {
            self.notify(NoticeLevel::Error, e.to_string());
        }
        match best {
            Some((_, _, t, _)) => {
                self.self_freeze.idle = false;
                if let Err(e) = self.start_self_freeze_render(t) {
                    tracing::warn!("self-freeze: {e}");
                }
            }
            None => {
                self.self_freeze.idle = !waiting;
                self.self_freeze.hurry = false;
            }
        }
    }

    /// Play `track` from the cache under `key` (in the folder). Returns
    /// whether the engine took it (the caller syncs the graph).
    fn install_cache(&mut self, track: TrackId, key: u64) -> bool {
        let Some(store) = &mut self.self_freeze.store else {
            return false;
        };
        let Some(e) = store.get(key).cloned() else {
            return false;
        };
        let path = store.path_of(&e);
        // The chain's latency as the live graph has it (buffered devices
        // add a device callback there): reported, so nothing downstream
        // moves.
        let latency = self.engine.track_latency(track);
        if latency == 0 && e.chain_latency > 0 {
            return false;
        }
        let stream = match crate::media::open_stream(&path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!("self-freeze: {}: {err}", path.display());
                return false;
            }
        };
        let rate = self.engine.sample_rate();
        let view = key::ChainView::of(&self.project, track, rate);
        if view.key() != key {
            return false;
        }
        store.touch(key);
        let chain = CachedChain {
            source: faderframe_engine::Source::Stream(stream),
            start: e.start,
            file_latency: e.file_latency,
            latency,
            key,
        };
        self.self_freeze.costs.insert(track, e.cost);
        self.self_freeze.installed.insert(
            track,
            Installed {
                key,
                view,
                cost: e.cost,
                revision: self.history.revision(),
            },
        );
        self.engine.set_cached(track, Some(chain))
    }

    /// Start rendering `track`'s chain in the background.
    fn start_self_freeze_render(&mut self, track: TrackId) -> Result<(), String> {
        let closure = key::chain_closure(&self.project, track);
        // The devices' state as they are now: in-process plugins are asked
        // here; a sandboxed one whose state is on its way waits.
        let plugins: Vec<PluginInstanceId> = closure
            .iter()
            .filter_map(|id| self.project.track(*id))
            .flat_map(|t| t.slots().into_iter().map(|s| (s.id, s.plugin.format)))
            .filter(|(_, f)| *f != faderframe_project::PluginFormat::Builtin)
            .map(|(id, _)| id)
            .collect();
        for id in plugins {
            if self.engine.plugin_sandboxed(id) {
                if self.plugin_state_pending(id) {
                    return Ok(());
                }
            } else {
                self.capture_plugin_state(id);
            }
        }
        let rate = self.engine.sample_rate();
        let view = key::ChainView::of(&self.project, track, rate);
        let key = view.key();
        // The states just taken may have changed it.
        self.self_freeze.keys.insert(track, key);
        if self.self_freeze.verdicts.contains_key(&key) {
            return Ok(());
        }
        if self
            .self_freeze
            .store
            .as_ref()
            .and_then(|s| s.get(key))
            .is_some_and(|e| e.rate == rate)
        {
            if self.install_cache(track, key) {
                self.sync(Impact::Graph).map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        let (start, end) = self
            .chain_span(track)
            .ok_or_else(|| "nothing to render".to_string())?;
        let store = self
            .self_freeze
            .store
            .as_ref()
            .ok_or_else(|| "no cache folder".to_string())?;
        let temp = store.temp_path(key);
        let dest = store.dir().join(store.file_for(key));
        let mut project = self.project.clone();
        for s in project.sources.values_mut() {
            if let faderframe_project::SourceSpec::File { path, .. } = &mut s.spec
                && path.is_relative()
                && let Some(dir) = self.project_dir()
            {
                *path = dir.join(&*path);
            }
        }
        let project = job::trimmed(&project, track, &closure);
        let block = match self.engine.stream_buffer_size() {
            0 => 256,
            n => n as usize,
        };
        let input = job::JobInput {
            project,
            track,
            key,
            rate,
            block,
            max_block: self.engine.config().max_block_size,
            start,
            end,
            min_cost: self.self_freeze.settings.min_cost(),
            temp,
            dest,
        };
        self.self_freeze.job = Some(job::Job::spawn(input)?);
        self.revision += 1;
        Ok(())
    }

    /// A finished render: listed, and played when its track still makes
    /// what it rendered.
    fn poll_self_freeze_job(&mut self) {
        if !self
            .self_freeze
            .job
            .as_ref()
            .is_some_and(job::Job::is_finished)
        {
            return;
        }
        let Some(job) = self.self_freeze.job.take() else {
            return;
        };
        let (track, key, start, rate) = (job.track, job.key, job.start, job.rate);
        self.revision += 1;
        match job.join() {
            Ok(job::Outcome::Cached {
                frames,
                channels,
                file_latency,
                chain_latency,
                cost,
            }) => {
                self.self_freeze.costs.insert(track, cost as f32);
                let Some(store) = &mut self.self_freeze.store else {
                    return;
                };
                let file = store.file_for(key);
                let bytes = frames * u64::from(channels) * 4 + 44;
                store.add(
                    key,
                    store::Entry {
                        file,
                        frames,
                        channels,
                        rate,
                        start,
                        file_latency,
                        chain_latency,
                        cost: cost as f32,
                        bytes,
                        used: 0,
                    },
                );
                self.evict_caches();
                let now = key::ChainView::of(&self.project, track, self.engine.sample_rate()).key();
                let stopped_quiet = self.transport.playing
                    || self
                        .self_freeze
                        .stopped_at
                        .is_none_or(|at| at.elapsed() >= STOP_QUIET);
                if now == key
                    && stopped_quiet
                    && self.why_live(track).is_none()
                    && self.install_cache(track, key)
                    && let Err(e) = self.sync(Impact::Graph)
                {
                    self.notify(NoticeLevel::Error, e.to_string());
                }
            }
            Ok(job::Outcome::Cheap { cost }) => {
                self.self_freeze.costs.insert(track, cost as f32);
                self.self_freeze.verdicts.insert(key, Live::Cheap);
                self.self_freeze.judged.insert(track, (key, Live::Cheap));
            }
            Ok(job::Outcome::Sounding) => {
                self.self_freeze.verdicts.insert(key, Live::Sounding);
                self.self_freeze.judged.insert(track, (key, Live::Sounding));
            }
            Err(e) => {
                if e != "cancelled" {
                    tracing::warn!("self-freeze: {e}");
                    *self.self_freeze.failed.entry(key).or_insert(0) += 1;
                }
            }
        }
        self.self_freeze.idle = false;
        self.self_freeze.looked = None;
    }

    /// Keep every track using `plugin` (a plugin id) live, or not.
    pub(crate) fn keep_plugin_live(&mut self, plugin: String, on: bool) {
        let mut s = self.self_freeze.settings.clone();
        s.never.retain(|p| *p != plugin);
        if on {
            s.never.push(plugin);
        }
        self.set_self_freeze(s);
    }

    /// Tracks using this plugin (by plugin id) always play live.
    pub fn plugin_kept_live(&self, plugin: &str) -> bool {
        self.self_freeze.settings.never.iter().any(|p| p == plugin)
    }

    /// The track menu's Self-Freeze entries: automatic, or kept live (audio
    /// and instrument tracks).
    pub fn self_freeze_choices(&self, track: TrackId) -> Vec<crate::InputChoice> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        if !matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) {
            return Vec::new();
        }
        let now = match self.self_freeze_track(track) {
            TrackCache::Cached { .. } | TrackCache::ToCache => "Automatic (cached now)",
            TrackCache::Rendering(_) => "Automatic (rendering)",
            _ => "Automatic",
        };
        [(now, false), ("Keep Live", true)]
            .into_iter()
            .map(|(label, off)| crate::InputChoice {
                label: label.into(),
                action: crate::Action::Edit(faderframe_project::Command::SetTrackSelfFreeze {
                    track,
                    off,
                }),
                checked: t.no_self_freeze == off,
                group_start: false,
            })
            .collect()
    }

    /// One line per track for `self-freeze:status`.
    pub fn self_freeze_report(&self) -> Vec<String> {
        let mut out = Vec::new();
        let s = self.self_freeze_summary();
        out.push(format!(
            "self-freeze {}: {} cached, {} switching, {:.1} % CPU saved, {:.1} of {} MB",
            match (s.on, s.mode, s.needed) {
                (false, ..) => "off",
                (true, SelfFreezeMode::Always, _) => "always",
                (true, SelfFreezeMode::WhenNeeded, true) => "when needed (needed now)",
                (true, SelfFreezeMode::WhenNeeded, false) => "when needed (the machine copes)",
            },
            s.cached.len(),
            s.switching,
            s.saved * 100.0,
            s.bytes as f64 / 1_048_576.0,
            s.limit / 1_048_576
        ));
        for t in &self.project.tracks {
            if !matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) {
                continue;
            }
            let what = match self.self_freeze_track(t.id) {
                TrackCache::Live(why) => format!("live ({})", why.label()),
                TrackCache::Waiting => "waiting".into(),
                TrackCache::Rendering(f) => format!("rendering {:.0} %", f * 100.0),
                TrackCache::ToCache => "fading to its cache".into(),
                TrackCache::Cached { cost } => {
                    format!("cached (saves {:.1} %)", cost * 100.0)
                }
                TrackCache::ToLive => "fading back to its chain".into(),
            };
            out.push(format!("  {}: {what}", t.name));
        }
        out
    }
}
