//! Mix Intelligence beyond one profile per track, on the analysis' own
//! thread after the profiles are diagnosed:
//!
//! * **Sources beside the processed signals.** In a processed analysis,
//!   each track in focus whose path holds devices (its own, a bus's, the
//!   master's) is also measured before processing — its source render, the
//!   same one (and the same cache key) a source analysis makes — so its
//!   attacks can be compared ([`faderframe_mix_intelligence::rules_dynamics`]).
//!   These renders are small (a track and a plain master) and run side by
//!   side on a few threads, each render on its own thread.
//! * **Pairs.** The pairs the crate chooses
//!   ([`faderframe_mix_intelligence::phase::candidates`]: selection, roles,
//!   kick and bass, masking findings, levels moving together; at most
//!   `PhaseRule::max_pairs`) are rendered over their excerpts (at most
//!   `excerpt_seconds` each, a second of run-up before it so devices are
//!   playing; the signal the analysis measures) and measured
//!   ([`faderframe_mix_intelligence::phase::measure_pair`], the pairs on a
//!   few threads). Processed: every track an excerpt needs is tapped in
//!   *one* render of the whole mix over it (`faderframe_engine::capture`:
//!   each where it reaches its destination, aligned with the master by the
//!   engine's delay compensation); sources: each track's source rendered
//!   once per excerpt. All excerpts held at once stay under
//!   [`MAX_EXCERPT_BYTES`]. Renders take the graph's latency off the front
//!   (as every Mix Intelligence render does), so both tracks share the
//!   range's time reference: the delay found is what the mix hears, after
//!   the engine's delay compensation, never a second one.
//!
//! Excerpts come from the signals the analysis' renders held
//! ([`super::held`]: the one pass, the sources, earlier analyses' signals
//! that did not change) whenever they are there; only the tracks missing
//! there are rendered for their excerpts, in a render of what they need
//! ([`super::capture::pruned`]).
//!
//! Pair measurements are cached (like the profiles: by the pair, the
//! signal, the two tracks' fingerprints — their processed ones, or their
//! source renders' —, the excerpt, the rate, the analysis settings and the
//! pair settings with [`faderframe_mix_intelligence::ALGORITHM_VERSION`]);
//! the results' staleness is the analysis' (finding by finding).

use super::capture::{needed, pruned};
use super::held::HeldAudio;
use super::{CacheKey, Fingerprints, Planned, Plays, fingerprint, source_project};
use crate::render::{RenderProgress, render_captured, render_each_on};
use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_engine::capture::CapturePlan;
use faderframe_mix_intelligence::phase::{self, PairPhase, PairSettings};
use faderframe_mix_intelligence::profile::ProfileBuilder;
use faderframe_mix_intelligence::rules::DiagnoseInput;
use faderframe_mix_intelligence::{
    AnalysisConfig, ExtraInput, Fnv, PairInput, Report, Rules, SignalPoint, SourceInput,
    TrackProfile, diagnose_extra,
};
use faderframe_project::{Project, TrackKind};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Excerpt audio held at once at most (bytes); pairs beyond it are left
/// out (said so).
pub const MAX_EXCERPT_BYTES: usize = 192 << 20;
/// Pair measurements kept for reuse.
const PAIRS_KEPT: usize = 512;
/// Run-up rendered before an excerpt and dropped (seconds).
const RUN_UP: f64 = 1.0;

/// The speed comparison's switch: run the stage as it was before the one
/// pass (sources one after the other, each pair's tracks solo in place
/// over their excerpt, pairs measured in turn). Tests only.
#[cfg(test)]
pub(super) static AS_BEFORE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn as_before() -> bool {
    #[cfg(test)]
    {
        AS_BEFORE.load(Ordering::Relaxed)
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// `f` over `items`: on a few threads, or in turn as it was.
fn map_maybe_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if as_before() {
        items.iter().map(f).collect()
    } else {
        faderframe_mix_intelligence::map_parallel(items, f)
    }
}

/// A pair's measurement in the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct PairKey {
    a: u64,
    b: u64,
    signal: u8,
    project: u64,
    start: i64,
    frames: usize,
    rate: u32,
    config: u64,
    settings: u64,
}

/// Pair measurements (least recently used out).
#[derive(Default)]
pub(crate) struct PairCache {
    entries: Vec<(PairKey, Arc<PairPhase>, u64)>,
    tick: u64,
}

impl PairCache {
    fn touch(&mut self, key: &PairKey) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(e) = self.entries.iter_mut().find(|e| e.0 == *key) {
            e.2 = tick;
        }
    }

    fn put(&mut self, key: PairKey, p: Arc<PairPhase>) {
        self.tick += 1;
        self.entries.retain(|e| e.0 != key);
        self.entries.push((key, p, self.tick));
        while self.entries.len() > PAIRS_KEPT {
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
}

/// A track whose source is measured beside its processed signal.
#[derive(Clone, Debug)]
pub(super) struct Companion {
    track: TrackId,
    /// Devices its source render keeps (an instrument and its MIDI
    /// effects).
    keep: usize,
    sends: bool,
}

/// What the extra stage needs, made on the session's thread.
#[derive(Default)]
pub(super) struct ExtraPlan {
    /// The tracks the user selected to analyse (that scope only).
    selected: Vec<TrackId>,
    companions: Vec<Companion>,
    master_devices: bool,
    /// The pair cache as it was.
    pairs: Vec<(PairKey, Arc<PairPhase>)>,
}

/// Where the extra stage is.
#[derive(Default)]
pub(super) struct ExtraProgress {
    stage: AtomicU32,
    done: AtomicU32,
    total: AtomicU32,
}

const SOURCES: u32 = 1;
const PAIRS: u32 = 2;

/// What the extra stage did (shown with the results).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ExtraCounts {
    /// Source profiles rendered for the dynamics comparison, and reused.
    pub sources_rendered: usize,
    pub sources_reused: usize,
    /// Pairs measured, reused from the cache, left out.
    pub pairs_measured: usize,
    pub pairs_reused: usize,
    pub pairs_skipped: usize,
    /// Renders made for the pairs' excerpts.
    pub excerpt_renders: usize,
    /// Excerpts read from signals held from a render instead.
    pub excerpts_held: usize,
    /// Seconds spent on the sources, the excerpts' renders and measuring
    /// the pairs.
    pub sources_seconds: f64,
    pub excerpts_seconds: f64,
    pub pairs_seconds: f64,
}

/// What the extra stage hands back to the session.
#[derive(Default)]
pub(super) struct ExtraOutcome {
    pub(super) profiles: Vec<(CacheKey, Arc<TrackProfile>)>,
    pub(super) reused: Vec<CacheKey>,
    pairs: Vec<(PairKey, Arc<PairPhase>)>,
    reused_pairs: Vec<PairKey>,
    pub(super) counts: ExtraCounts,
}

impl ExtraOutcome {
    /// Take in the sources an earlier run of the stage in the same job
    /// rendered (the later run found them as reused); its pairs are left
    /// out (measured on a first look's signals, measured again).
    pub(super) fn absorb_sources(&mut self, earlier: ExtraOutcome) {
        let e = earlier.counts;
        let c = &mut self.counts;
        c.sources_reused = c.sources_reused.saturating_sub(e.sources_rendered);
        c.sources_rendered += e.sources_rendered;
        c.sources_seconds += e.sources_seconds;
        self.profiles.extend(earlier.profiles);
    }

    /// The source profiles it made (for a later run's cache).
    pub(super) fn made_profiles(&self) -> impl Iterator<Item = &(CacheKey, Arc<TrackProfile>)> {
        self.profiles.iter()
    }
}

/// Plan the extra stage of an analysis (see the module docs).
pub(super) fn plan(
    is_instrument: &dyn Fn(PluginInstanceId) -> bool,
    base: &Project,
    planned: &[Planned],
    signal: SignalPoint,
    focus: &[TrackId],
    cache: &PairCache,
) -> ExtraPlan {
    let master_devices = base.tracks.iter().any(|t| {
        matches!(t.kind, TrackKind::Bus | TrackKind::Aux | TrackKind::Master)
            && (!t.inserts.is_empty() || t.preamp.is_some())
    });
    let companions = if signal == SignalPoint::Processed {
        planned
            .iter()
            .filter(|p| p.focus && matches!(p.plays, Plays::Tapped))
            .filter_map(|p| base.track(p.track))
            .filter(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument))
            .filter(|t| {
                master_devices
                    || !t.inserts.is_empty()
                    || t.preamp.is_some()
                    || !t.containers.is_empty()
            })
            .map(|t| Companion {
                track: t.id,
                keep: if t.kind == TrackKind::Instrument {
                    t.inserts
                        .iter()
                        .position(|s| is_instrument(s.id))
                        .map_or(t.inserts.len(), |i| i + 1)
                } else {
                    0
                },
                sends: !t.sends.is_empty(),
            })
            .collect()
    } else {
        Vec::new()
    };
    ExtraPlan {
        selected: focus.to_vec(),
        companions,
        master_devices,
        pairs: cache
            .entries
            .iter()
            .map(|e| (e.0, Arc::clone(&e.1)))
            .collect(),
    }
}

/// Take the stage's results into the caches (from the session's tick).
pub(super) fn keep(
    cache: &mut super::Cache,
    pairs: &mut PairCache,
    out: &mut ExtraOutcome,
) -> ExtraCounts {
    for k in &out.reused {
        let _ = cache.get(k);
    }
    for (k, p) in out.profiles.drain(..) {
        cache.put(k, p);
    }
    for k in &out.reused_pairs {
        pairs.touch(k);
    }
    for (k, p) in out.pairs.drain(..) {
        pairs.put(k, p);
    }
    out.counts
}

/// The status line while the stage runs (`None`: not in it).
pub(super) fn status(p: &ExtraProgress) -> Option<String> {
    let (done, total) = (
        p.done.load(Ordering::Relaxed),
        p.total.load(Ordering::Relaxed),
    );
    match p.stage.load(Ordering::Relaxed) {
        SOURCES => Some(format!(
            "Rendering sources to compare the dynamics, side by side ({} of {total})",
            done.min(total)
        )),
        PAIRS => Some(format!(
            "Comparing phase and timing: excerpts rendered once each, pairs measured side by \
             side ({} of {total} steps)",
            (done + 1).min(total.max(1))
        )),
        _ => None,
    }
}

/// Everything the stage reads from the job.
pub(super) struct Stage<'a> {
    pub(super) planned: &'a [Planned],
    pub(super) base: &'a Project,
    /// The processed signals' fingerprints (each track's: pair keys).
    pub(super) fps: Option<&'a Fingerprints>,
    pub(super) sources: &'a faderframe_engine::SourceMap,
    pub(super) start: i64,
    pub(super) frames: usize,
    pub(super) rate: u32,
    pub(super) signal: SignalPoint,
    pub(super) config: &'a AnalysisConfig,
    pub(super) rules: &'a Rules,
    pub(super) cache: &'a [(CacheKey, Arc<TrackProfile>)],
    pub(super) plan: &'a ExtraPlan,
    /// Each signal held from a render over the whole range (by track).
    pub(super) held: &'a HashMap<TrackId, Arc<HeldAudio>>,
    pub(super) render: &'a RenderProgress,
    pub(super) progress: &'a ExtraProgress,
    /// The threads working (shown in the progress).
    pub(super) threads: &'a AtomicU32,
}

impl Stage<'_> {
    fn cancelled(&self) -> bool {
        self.render.cancel.load(Ordering::Relaxed)
    }

    /// The source render of `track` keeping `keep` devices, its outputs
    /// and fingerprint.
    fn source(&self, track: TrackId, keep: usize) -> Option<(Project, usize, Option<u64>)> {
        let (p, outputs) = source_project(self.base, track, keep)?;
        let fp = fingerprint(&p);
        Some((p, outputs, fp))
    }

    /// What `track`'s signal depends on: its processed fingerprint (the
    /// tap), or its source render's (with the project rendering it).
    fn signal_of(&self, track: TrackId) -> Option<Signal> {
        let p = self.planned.iter().find(|p| p.track == track)?;
        match p.plays {
            Plays::Tapped => Some(Signal::Tapped(self.fps.map(|f| f.of(track)))),
            Plays::Source { keep } => {
                let (project, outputs, fp) = self.source(track, keep)?;
                Some(Signal::Source(Box::new(project), outputs, fp))
            }
            Plays::Master => None,
        }
    }

    /// A render alone on the calling thread (it runs beside others).
    fn render(
        &self,
        project: &Project,
        outputs: usize,
        (start, frames): (i64, usize),
        each: &mut dyn FnMut(&[&[f32]]),
    ) -> Result<(), String> {
        // Beside the others: streams of its own.
        let sources = super::own_sources(project, self.sources);
        render_each_on(
            project,
            self.rate,
            &sources,
            (start, frames),
            self.render,
            outputs,
            // As it was: every core for each render in turn.
            if as_before() {
                faderframe_realtime::default_worker_count()
            } else {
                0
            },
            each,
        )
        .map_err(|e| e.to_string())
    }
}

/// Processed: every tapped track of every excerpt in *one* render over the
/// stretch the excerpts span (each excerpt's frames kept, a second of run-up
/// before the first), when that renders fewer frames than a render per
/// excerpt and the excerpts fit the budget. `true` when done (the audio is
/// in `audio`).
fn tapped_together(
    st: &Stage<'_>,
    cands: &[phase::PairCandidate],
    todo: &[(usize, phase::Excerpt, Option<PairKey>)],
    excerpts: &[phase::Excerpt],
    signals: &HashMap<TrackId, Option<Signal>>,
    audio: &mut HashMap<(TrackId, usize, usize), Vec<Vec<f32>>>,
) -> Result<bool, String> {
    if as_before() || excerpts.len() < 2 {
        return Ok(false);
    }
    let fr = f64::from(st.rate.max(1));
    let run_up = (RUN_UP * fr) as i64;
    // Each excerpt's tapped tracks.
    let windows: Vec<(phase::Excerpt, Vec<TrackId>)> = excerpts
        .iter()
        .map(|ex| {
            let mut tracks: Vec<TrackId> = Vec::new();
            for (i, e, _) in todo {
                if same_span(e, ex) {
                    for t in [cands[*i].a, cands[*i].b] {
                        if matches!(signals.get(&t), Some(Some(Signal::Tapped(_))))
                            && !tracks.contains(&t)
                            && !audio.contains_key(&(t, ex.start, ex.frames))
                        {
                            tracks.push(t);
                        }
                    }
                }
            }
            (*ex, tracks)
        })
        .filter(|(_, t)| !t.is_empty())
        .collect();
    if windows.len() < 2 {
        return Ok(false);
    }
    let first = windows
        .iter()
        .map(|(e, _)| e.start as i64)
        .min()
        .unwrap_or(0);
    let last = windows
        .iter()
        .map(|(e, _)| (e.start + e.frames) as i64)
        .max()
        .unwrap_or(0);
    let lead = run_up.min(st.start + first).max(0);
    let span = last - first + lead;
    let separate: i64 = windows
        .iter()
        .map(|(e, _)| e.frames as i64 + run_up.min(st.start + e.start as i64).max(0))
        .sum();
    let width = |t: TrackId| {
        st.base
            .track(t)
            .map_or(2, |x| st.base.destination_layout(x).channel_count().max(1))
    };
    let bytes: usize = windows
        .iter()
        .map(|(e, ts)| ts.iter().map(|t| 4 * width(*t) * e.frames).sum::<usize>())
        .sum();
    if span >= separate || bytes > MAX_EXCERPT_BYTES {
        return Ok(false);
    }
    let mut tracks: Vec<TrackId> = Vec::new();
    for (_, ts) in &windows {
        for t in ts {
            if !tracks.contains(t) {
                tracks.push(*t);
            }
        }
    }
    let begin = st.start + first - lead;
    st.render
        .total
        .fetch_add(span.max(0) as u64, Ordering::Relaxed);
    // Each tap's frames so far (from `begin`), each window's audio.
    let mut pos: HashMap<TrackId, i64> = HashMap::new();
    let mut kept: HashMap<(TrackId, usize), Vec<Vec<f32>>> = HashMap::new();
    // Only what the tapped tracks need sounds.
    let project = pruned(st.base, &needed(st.base, &tracks));
    render_captured(
        &project,
        st.rate,
        st.sources,
        (begin, span.max(0) as usize),
        st.render,
        2,
        CapturePlan {
            tracks,
            devices: false,
            block: 0,
        },
        &mut |who, piece| {
            let Some(t) = who else {
                return;
            };
            let n = piece.first().map_or(0, |c| c.len()) as i64;
            let at = pos.entry(t).or_insert(0);
            let (a, b) = (*at, *at + n);
            *at = b;
            for (w, (e, ts)) in windows.iter().enumerate() {
                if !ts.contains(&t) {
                    continue;
                }
                // The window in render frames.
                let ws = e.start as i64 - first + lead;
                let (x, y) = (a.max(ws), b.min(ws + e.frames as i64));
                if y <= x {
                    continue;
                }
                let dst = kept.entry((t, w)).or_insert_with(|| {
                    (0..piece.len())
                        .map(|_| Vec::with_capacity(e.frames))
                        .collect()
                });
                for (o, c) in dst.iter_mut().zip(piece) {
                    o.extend_from_slice(&c[(x - a) as usize..(y - a) as usize]);
                }
            }
        },
    )
    .map_err(|e| e.to_string())?;
    for ((t, w), chans) in kept {
        let e = windows[w].0;
        audio.insert((t, e.start, e.frames), chans);
    }
    Ok(true)
}

/// Two excerpts over the same frames (rendered once).
fn same_span(a: &phase::Excerpt, b: &phase::Excerpt) -> bool {
    (a.start, a.frames) == (b.start, b.frames)
}

/// How a pair's track is rendered for its excerpt.
enum Signal {
    /// Tapped in a render of the whole mix (its fingerprint).
    Tapped(Option<u64>),
    /// Its source: the project, its outputs, its fingerprint.
    Source(Box<Project>, usize, Option<u64>),
}

impl Signal {
    fn fingerprint(&self) -> Option<u64> {
        match self {
            Signal::Tapped(fp) | Signal::Source(_, _, fp) => *fp,
        }
    }
}

/// The source profiles of the companions (rendered or reused): the renders
/// side by side on a few threads.
fn companions(
    st: &Stage<'_>,
    out: &mut ExtraOutcome,
) -> Result<Vec<(TrackId, Arc<TrackProfile>, bool)>, String> {
    let list = &st.plan.companions;
    st.progress.stage.store(SOURCES, Ordering::Relaxed);
    st.progress.done.store(0, Ordering::Relaxed);
    st.progress
        .total
        .store(list.len() as u32, Ordering::Relaxed);
    st.threads.store(
        super::measure::threads(list.len()) as u32,
        Ordering::Relaxed,
    );
    type One = Result<Option<(Arc<TrackProfile>, Option<CacheKey>, bool)>, String>;
    let done = map_maybe_parallel(list, |c| -> One {
        if st.cancelled() {
            return Err("cancelled".into());
        }
        let Some((project, outputs, fp)) = st.source(c.track, c.keep) else {
            return Ok(None);
        };
        // The key a source analysis gives the same render.
        let key = fp.map(|project| CacheKey {
            track: c.track.0,
            signal: SignalPoint::Source as u8,
            project,
            start: st.start,
            frames: st.frames,
            rate: st.rate,
            config: st.config.fingerprint(),
            spans: Fnv::new().finish(),
        });
        if let Some(found) = key.and_then(|k| st.cache.iter().find(|e| e.0 == k)) {
            st.progress.done.fetch_add(1, Ordering::Relaxed);
            return Ok(Some((Arc::clone(&found.1), Some(found.0), false)));
        }
        st.render
            .total
            .fetch_add(st.frames as u64, Ordering::Relaxed);
        let mut builder =
            ProfileBuilder::new(st.rate, outputs, st.config, &[]).map_err(|e| e.to_string())?;
        let mut failed = None;
        st.render(&project, outputs, (st.start, st.frames), &mut |piece| {
            if failed.is_none()
                && let Err(e) = builder.push(piece)
            {
                failed = Some(e);
            }
        })?;
        if let Some(e) = failed {
            return Err(e.to_string());
        }
        st.progress.done.fetch_add(1, Ordering::Relaxed);
        Ok(Some((Arc::new(builder.finish()), key, true)))
    });
    let mut made = Vec::new();
    for (c, r) in list.iter().zip(done) {
        let Some((profile, key, fresh)) = r? else {
            continue;
        };
        match (fresh, key) {
            (true, k) => {
                if let Some(k) = k {
                    out.profiles.push((k, Arc::clone(&profile)));
                }
                out.counts.sources_rendered += 1;
            }
            (false, k) => {
                out.reused.extend(k);
                out.counts.sources_reused += 1;
            }
        }
        made.push((c.track, profile, c.sends));
    }
    Ok(made)
}

/// Run the extra stage (see the module docs): the findings go into
/// `report`; what to cache comes back.
pub(super) fn run(
    st: &Stage<'_>,
    input: &DiagnoseInput<'_>,
    report: &mut Report,
) -> Result<ExtraOutcome, String> {
    let mut out = ExtraOutcome::default();
    let t0 = std::time::Instant::now();
    let sources = companions(st, &mut out)?;
    out.counts.sources_seconds = t0.elapsed().as_secs_f64();
    // The pairs and their excerpts.
    let rules = st.rules;
    // Never a track with a bus or return it reaches (that carries it).
    let cands: Vec<_> = phase::candidates(
        &input.tracks,
        &report.findings,
        &st.plan.selected,
        &rules.phase,
    )
    .into_iter()
    .filter(|c| {
        !input
            .related
            .iter()
            .any(|&(x, y)| (x, y) == (c.a, c.b) || (y, x) == (c.a, c.b))
    })
    .collect();
    let profile_of = |t: TrackId| {
        input
            .tracks
            .iter()
            .find(|x| x.track == t)
            .map(|x| x.profile)
    };
    let excerpts = phase::excerpts(&cands, &profile_of, &rules.phase);
    let settings = PairSettings::of(rules);
    let fr = f64::from(st.rate.max(1));
    st.progress.stage.store(PAIRS, Ordering::Relaxed);
    st.progress.done.store(0, Ordering::Relaxed);
    st.progress
        .total
        .store(cands.len() as u32, Ordering::Relaxed);
    // How each track is rendered (and its fingerprint), found once.
    let mut signals: HashMap<TrackId, Option<Signal>> = HashMap::new();
    let mut audio: HashMap<(TrackId, usize, usize), Vec<Vec<f32>>> = HashMap::new();
    let mut held = 0usize;
    let mut measured: Vec<(usize, Arc<PairPhase>)> = Vec::new();
    let mut skipped = Vec::new();
    // The pairs to measure now: (candidate, excerpt, key).
    let mut todo: Vec<(usize, phase::Excerpt, Option<PairKey>)> = Vec::new();
    for (i, (c, ex)) in cands.iter().zip(&excerpts).enumerate() {
        if st.cancelled() {
            return Err("cancelled".into());
        }
        let Some(ex) = ex else {
            skipped.push((
                c.a,
                c.b,
                format!(
                    "they play together for less than {:.0} s",
                    rules.phase.min_together_seconds
                ),
            ));
            continue;
        };
        for t in [c.a, c.b] {
            signals.entry(t).or_insert_with(|| st.signal_of(t));
        }
        let fp = |t: TrackId| {
            signals
                .get(&t)
                .and_then(Option::as_ref)
                .and_then(Signal::fingerprint)
        };
        let project_fp = match (fp(c.a), fp(c.b)) {
            (Some(x), Some(y)) => {
                let mut h = Fnv::new();
                h.write_u64(x);
                h.write_u64(y);
                Some(h.finish())
            }
            _ => None,
        };
        let key = project_fp.map(|project| PairKey {
            a: c.a.0,
            b: c.b.0,
            signal: st.signal as u8,
            project,
            start: st.start + ex.start as i64,
            frames: ex.frames,
            rate: st.rate,
            config: st.config.fingerprint(),
            settings: settings.fingerprint(),
        });
        if let Some(found) = key.and_then(|k| st.plan.pairs.iter().find(|e| e.0 == k)) {
            out.reused_pairs.push(found.0);
            out.counts.pairs_reused += 1;
            measured.push((i, Arc::clone(&found.1)));
            continue;
        }
        todo.push((i, *ex, key));
    }
    // The excerpts' audio: each excerpt rendered once — processed, with
    // every track it needs tapped in that one render; sources, each track's
    // source once — within the budget.
    let mut excerpt_list: Vec<phase::Excerpt> = Vec::new();
    for (_, ex, _) in &todo {
        if !excerpt_list.iter().any(|e| same_span(e, ex)) {
            excerpt_list.push(*ex);
        }
    }
    st.progress
        .total
        .store((excerpt_list.len() + todo.len()) as u32, Ordering::Relaxed);
    let t0 = std::time::Instant::now();
    // What the renders held already (never as it was before).
    if !as_before() {
        for (i, ex, _) in &todo {
            for t in [cands[*i].a, cands[*i].b] {
                let k = (t, ex.start, ex.frames);
                if audio.contains_key(&k) {
                    continue;
                }
                if let Some(h) = st.held.get(&t).filter(|h| h.frames() >= st.frames) {
                    let bytes = 4 * h.channels() * ex.frames;
                    if held + bytes > MAX_EXCERPT_BYTES {
                        continue;
                    }
                    held += bytes;
                    audio.insert(k, h.excerpt(ex.start, ex.frames));
                    out.counts.excerpts_held += 1;
                }
            }
        }
    }
    let together = tapped_together(st, &cands, &todo, &excerpt_list, &signals, &mut audio)?;
    if together {
        out.counts.excerpt_renders += 1;
    }
    let mut over_budget: Vec<usize> = Vec::new();
    for (n, ex) in excerpt_list.iter().enumerate() {
        st.progress.done.store(n as u32, Ordering::Relaxed);
        if st.cancelled() {
            return Err("cancelled".into());
        }
        let mut wanted: Vec<TrackId> = Vec::new();
        for (i, e, _) in &todo {
            if same_span(e, ex) {
                for t in [cands[*i].a, cands[*i].b] {
                    if !wanted.contains(&t) && !audio.contains_key(&(t, ex.start, ex.frames)) {
                        wanted.push(t);
                    }
                }
            }
        }
        if wanted.is_empty() {
            continue;
        }
        let at = st.start + ex.start as i64;
        let run_up = ((RUN_UP * fr) as i64).min(at.max(0)) as usize;
        let tapped: Vec<TrackId> = wanted
            .iter()
            .copied()
            .filter(|t| matches!(signals.get(t), Some(Some(Signal::Tapped(_)))))
            .collect();
        let width = |t: TrackId| {
            st.base
                .track(t)
                .map_or(2, |x| st.base.destination_layout(x).channel_count().max(1))
        };
        let bytes: usize = wanted.iter().map(|t| 4 * width(*t) * ex.frames).sum();
        if held + bytes > MAX_EXCERPT_BYTES {
            over_budget.push(n);
            continue;
        }
        if as_before() {
            // As it was: each track solo in place through the mix.
            for &t in &tapped {
                let mut solo = st.base.clone();
                for x in &mut solo.tracks {
                    x.solo = x.id == t;
                }
                let mut chans: Vec<Vec<f32>> =
                    (0..2).map(|_| Vec::with_capacity(ex.frames)).collect();
                let mut skip = run_up;
                st.render(
                    &solo,
                    2,
                    (at - run_up as i64, ex.frames + run_up),
                    &mut |piece| {
                        let n = piece.first().map_or(0, |c| c.len());
                        let from = skip.min(n);
                        skip -= from;
                        for (o, c) in chans.iter_mut().zip(piece) {
                            o.extend_from_slice(&c[from..]);
                        }
                    },
                )?;
                audio.insert((t, ex.start, ex.frames), chans);
            }
        } else if !tapped.is_empty() && !together {
            out.counts.excerpt_renders += 1;
            st.render
                .total
                .fetch_add((ex.frames + run_up) as u64, Ordering::Relaxed);
            let mut chans: HashMap<TrackId, Vec<Vec<f32>>> = HashMap::new();
            let mut skip: HashMap<TrackId, usize> = HashMap::new();
            // Only what the tapped tracks need sounds.
            let project = pruned(st.base, &needed(st.base, &tapped));
            render_captured(
                &project,
                st.rate,
                st.sources,
                (at - run_up as i64, ex.frames + run_up),
                st.render,
                2,
                CapturePlan {
                    tracks: tapped.clone(),
                    devices: false,
                    block: 0,
                },
                &mut |who, piece| {
                    let Some(t) = who else {
                        return;
                    };
                    let n = piece.first().map_or(0, |c| c.len());
                    let left = skip.entry(t).or_insert(run_up);
                    let from = (*left).min(n);
                    *left -= from;
                    let dst = chans.entry(t).or_insert_with(|| {
                        (0..piece.len())
                            .map(|_| Vec::with_capacity(ex.frames))
                            .collect()
                    });
                    for (o, c) in dst.iter_mut().zip(piece) {
                        o.extend_from_slice(&c[from..]);
                    }
                },
            )
            .map_err(|e| e.to_string())?;
            for (t, c) in chans {
                audio.insert((t, ex.start, ex.frames), c);
            }
        }
        for t in wanted.iter().filter(|t| !tapped.contains(t)) {
            let Some(Some(Signal::Source(project, outputs, _))) = signals.get(t) else {
                continue;
            };
            st.render
                .total
                .fetch_add((ex.frames + run_up) as u64, Ordering::Relaxed);
            let mut chans: Vec<Vec<f32>> = (0..*outputs)
                .map(|_| Vec::with_capacity(ex.frames))
                .collect();
            let mut skip = run_up;
            st.render(
                project,
                *outputs,
                (at - run_up as i64, ex.frames + run_up),
                &mut |piece| {
                    let n = piece.first().map_or(0, |c| c.len());
                    let from = skip.min(n);
                    skip -= from;
                    for (o, c) in chans.iter_mut().zip(piece) {
                        o.extend_from_slice(&c[from..]);
                    }
                },
            )?;
            audio.insert((*t, ex.start, ex.frames), chans);
        }
        held += bytes;
    }
    out.counts.excerpts_seconds = t0.elapsed().as_secs_f64();
    // The pairs measured on a few threads.
    let t0 = std::time::Instant::now();
    let ready: Vec<&(usize, phase::Excerpt, Option<PairKey>)> = todo
        .iter()
        .filter(|(i, ex, _)| {
            let c = &cands[*i];
            let have = |t: TrackId| audio.contains_key(&(t, ex.start, ex.frames));
            let ok = have(c.a) && have(c.b);
            if !ok {
                let n = excerpt_list
                    .iter()
                    .position(|e| same_span(e, ex))
                    .unwrap_or(0);
                let why = if over_budget.contains(&n) {
                    "too much audio to compare at once (more pairs than the memory allows)"
                } else {
                    "a track of the pair could not be rendered"
                };
                skipped.push((c.a, c.b, why.into()));
            }
            ok
        })
        .collect();
    st.threads.store(
        super::measure::threads(ready.len()) as u32,
        Ordering::Relaxed,
    );
    let pairs_done = std::sync::atomic::AtomicU32::new(excerpt_list.len() as u32);
    let results = map_maybe_parallel(&ready, |&&(i, ex, _)| {
        let c = &cands[i];
        let view = |t: TrackId| -> Vec<&[f32]> {
            audio
                .get(&(t, ex.start, ex.frames))
                .map(|v| v.iter().map(Vec::as_slice).collect())
                .unwrap_or_default()
        };
        let r = phase::measure_pair(
            &view(c.a),
            &view(c.b),
            st.rate,
            st.config,
            &settings,
            ex.start as f64 / fr,
            Some(&st.render.cancel),
        );
        st.progress.done.store(
            pairs_done.fetch_add(1, Ordering::Relaxed) + 1,
            Ordering::Relaxed,
        );
        r
    });
    for (&&(i, _, key), r) in ready.iter().zip(results) {
        let c = &cands[i];
        match r {
            Ok(m) => {
                let m = Arc::new(m);
                if let Some(k) = key {
                    out.pairs.push((k, Arc::clone(&m)));
                }
                out.counts.pairs_measured += 1;
                measured.push((i, m));
            }
            Err(phase::PhaseError::Cancelled) => return Err("cancelled".into()),
            Err(e) => skipped.push((c.a, c.b, e.to_string())),
        }
    }
    // In the candidates' order, whatever was reused and measured.
    measured.sort_by_key(|(i, _)| *i);
    out.counts.pairs_seconds = t0.elapsed().as_secs_f64();
    drop(audio);
    out.counts.pairs_skipped = skipped.len();
    let extra = ExtraInput {
        pairs: measured
            .iter()
            .map(|(i, m)| PairInput {
                candidate: &cands[*i],
                phase: m,
            })
            .collect(),
        sources: sources
            .iter()
            .map(|(t, p, sends)| SourceInput {
                track: *t,
                profile: p,
                sends: *sends,
            })
            .collect(),
        master_devices: st.plan.master_devices,
        skipped,
    };
    diagnose_extra(input, &extra, report).map_err(|e| e.to_string())?;
    st.progress.stage.store(0, Ordering::Relaxed);
    Ok(out)
}
