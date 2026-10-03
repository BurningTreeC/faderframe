//! Performance statistics: DSP load in total, per track and per plugin
//! instance.
//!
//! The audio thread measures (callback metrics, per-node and per-track
//! graph timings, see `faderframe_audio_graph::NodeTimings`); the session
//! polls a few times a second and turns the counters into loads — shares of
//! the time a callback may take (its buffer's duration). Averages are over
//! the poll window (lightly smoothed), peaks are the worst single callback,
//! held for a moment so short spikes stay readable.

use crate::Session;
use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_engine::{EngineController, GraphProfile, NodeWork};
use faderframe_project::{PluginFormat, Project, TrackColor, TrackKind};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the counters are read.
pub const PERF_POLL: Duration = Duration::from_millis(250);
/// Load history kept for graphs (60 s at the poll rate).
pub const PERF_HISTORY: usize = 240;

/// Smoothing of averages between polls (weight of the newest window).
const SMOOTHING: f64 = 0.45;
/// Held peaks fall by this factor per poll once the spike is over.
const PEAK_DECAY: f64 = 0.82;
/// Per-node timing stays on this long after the detailed figures were last
/// read.
const DETAIL_LINGER: Duration = Duration::from_secs(2);

/// A load as a share of the callback budget (1.0 = the whole buffer time).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Load {
    pub average: f64,
    pub peak: f64,
}

impl Load {
    fn update(&mut self, average: f64, peak: f64) {
        self.average = SMOOTHING * average + (1.0 - SMOOTHING) * self.average;
        self.peak = peak.max(self.peak * PEAK_DECAY);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginPerformance {
    pub plugin: PluginInstanceId,
    pub name: String,
    pub format: PluginFormat,
    pub instrument: bool,
    pub load: Load,
    pub latency: u32,
    pub bypassed: bool,
    pub failed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackPerformance {
    pub track: TrackId,
    pub name: String,
    pub color: TrackColor,
    pub kind: TrackKind,
    /// Everything the track does: clips, input/summing, plugins, strip,
    /// sends.
    pub load: Load,
    /// The track's own work without its plugins (playback, mixing).
    pub mixing: Load,
    pub plugins: Vec<PluginPerformance>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PerformanceReport {
    /// An audio stream is running (otherwise nothing is measured).
    pub running: bool,
    pub sample_rate: u32,
    pub buffer_size: u32,
    /// Whole callbacks.
    pub total: Load,
    /// The graph's share: every track's nodes.
    pub graph: Load,
    /// Outside the graph: device I/O, recording, metronome, scheduling.
    pub engine: f64,
    /// Total load per poll, oldest first.
    pub history: Vec<Load>,
    pub tracks: Vec<TrackPerformance>,
    /// Callback statistics since the last reset.
    pub p99_load: f64,
    pub max_load: f64,
    pub callbacks: u64,
    pub deadline_misses: u64,
    pub xruns: u64,
    pub late_disk_reads: u64,
    pub graph_nodes: usize,
    pub graph_levels: usize,
    pub output_latency: u32,
}

impl PerformanceReport {
    /// Every plugin instance, highest average load first.
    pub fn plugins_by_load(&self) -> Vec<(&TrackPerformance, &PluginPerformance)> {
        let mut all: Vec<_> = self
            .tracks
            .iter()
            .flat_map(|t| t.plugins.iter().map(move |p| (t, p)))
            .collect();
        all.sort_by(|a, b| b.1.load.average.total_cmp(&a.1.load.average));
        all
    }
}

/// Counters of the graph being measured.
struct Baseline {
    profile: GraphProfile,
    nodes: Vec<u64>,
    groups: Vec<u64>,
}

#[derive(Default)]
pub(crate) struct PerformanceMonitor {
    last_poll: Option<Instant>,
    callback_ns: u64,
    budget_ns: u64,
    baseline: Option<Baseline>,
    total: Load,
    graph: Load,
    tracks: HashMap<TrackId, (Load, Load)>,
    plugins: HashMap<PluginInstanceId, Load>,
    history: VecDeque<Load>,
    report: PerformanceReport,
    /// When the per-track/plugin figures were last read.
    detail_wanted: Cell<Option<Instant>>,
}

impl PerformanceMonitor {
    fn measure(&mut self, engine: &mut EngineController, project: &Project, running: bool) {
        let m = engine.metrics();
        let d_callback = m.total_ns.saturating_sub(self.callback_ns);
        let d_budget = m.total_budget_ns.saturating_sub(self.budget_ns);
        // A metrics reset makes the totals smaller: start over.
        let reset = m.total_budget_ns < self.budget_ns;
        self.callback_ns = m.total_ns;
        self.budget_ns = m.total_budget_ns;
        let peak = engine.take_peak_load();
        let share = |ns: u64| {
            if d_budget == 0 || reset {
                0.0
            } else {
                ns as f64 / d_budget as f64
            }
        };
        self.total.update(share(d_callback), peak);

        // Graph nodes: deltas against the last poll of the same graph.
        let profile = engine.graph_profile();
        let fresh = match (&self.baseline, &profile) {
            (Some(b), Some(p)) => !Arc::ptr_eq(&b.profile.timings, &p.timings),
            _ => true,
        };
        if fresh {
            self.baseline = profile.map(|profile| Baseline {
                nodes: vec![0; profile.timings.len()],
                groups: vec![0; profile.timings.group_count()],
                profile,
            });
        }
        let mut track_loads: HashMap<TrackId, (f64, f64)> = HashMap::new();
        let mut track_mixing: HashMap<TrackId, f64> = HashMap::new();
        let mut plugin_loads: HashMap<PluginInstanceId, (f64, f64)> = HashMap::new();
        let mut graph_avg = 0.0;
        if let Some(b) = &mut self.baseline {
            let t = &b.profile.timings;
            for g in 0..t.group_count() {
                let total = t.group_total_ns(g);
                let delta = total.saturating_sub(b.groups[g]);
                b.groups[g] = total;
                let peak = t.take_group_peak(g);
                if let Some(track) = b.profile.groups.get(g) {
                    let avg = share(delta);
                    graph_avg += avg;
                    track_loads.insert(*track, (avg, peak));
                }
            }
            for i in 0..t.len() {
                let total = t.total_ns(i);
                let delta = total.saturating_sub(b.nodes[i]);
                b.nodes[i] = total;
                let peak = t.take_peak(i);
                let Some(owner) = b.profile.owners.get(i).copied().flatten() else {
                    continue;
                };
                match (owner.plugin, owner.work) {
                    (Some(p), NodeWork::Insert | NodeWork::Instrument) => {
                        plugin_loads.insert(p, (share(delta), peak));
                    }
                    _ => *track_mixing.entry(owner.track).or_default() += share(delta),
                }
            }
        }
        let graph_peak = track_loads.values().map(|l| l.1).sum::<f64>().min(peak);
        self.graph.update(graph_avg, graph_peak);

        let failed = engine.failed_plugins();
        let mut tracks = Vec::with_capacity(project.tracks.len());
        for t in &project.tracks {
            let (avg, peak) = track_loads.get(&t.id).copied().unwrap_or_default();
            let mixing_avg = track_mixing.get(&t.id).copied().unwrap_or_default();
            let entry = self.tracks.entry(t.id).or_default();
            entry.0.update(avg, peak);
            entry.1.update(mixing_avg, 0.0);
            let (load, mixing) = *entry;
            let plugins = t
                .instrument
                .iter()
                .map(|s| (s, true))
                .chain(t.inserts.iter().map(|s| (s, false)))
                .map(|(slot, instrument)| {
                    let (avg, peak) = plugin_loads.get(&slot.id).copied().unwrap_or_default();
                    let load = self.plugins.entry(slot.id).or_default();
                    load.update(avg, peak);
                    PluginPerformance {
                        plugin: slot.id,
                        name: slot.plugin.name.clone(),
                        format: slot.plugin.format,
                        instrument,
                        load: *load,
                        latency: engine.plugin_latency(slot.id).unwrap_or(0),
                        bypassed: slot.bypass,
                        failed: failed.contains(&slot.id),
                    }
                })
                .collect();
            tracks.push(TrackPerformance {
                track: t.id,
                name: t.name.clone(),
                color: t.color,
                kind: t.kind,
                load,
                mixing,
                plugins,
            });
        }
        // Forget removed tracks and plugins.
        self.tracks.retain(|id, _| project.track(*id).is_some());
        self.plugins.retain(|id, _| {
            project.tracks.iter().any(|t| {
                t.inserts
                    .iter()
                    .chain(t.instrument.iter())
                    .any(|s| s.id == *id)
            })
        });

        self.history.push_back(self.total);
        while self.history.len() > PERF_HISTORY {
            self.history.pop_front();
        }
        let stats = engine.graph_stats();
        let budget = m.last_budget_ns.max(1) as f64;
        self.report = PerformanceReport {
            running,
            sample_rate: engine.stream_sample_rate(),
            buffer_size: engine.stream_buffer_size(),
            total: self.total,
            graph: self.graph,
            engine: (self.total.average - self.graph.average).max(0.0),
            history: self.history.iter().copied().collect(),
            tracks,
            p99_load: m.p99_ns as f64 / budget,
            max_load: m.max_ns as f64 / budget,
            callbacks: m.callbacks,
            deadline_misses: m.deadline_misses,
            xruns: m.xruns,
            late_disk_reads: 0,
            graph_nodes: stats.nodes,
            graph_levels: stats.levels,
            output_latency: stats.output_latency,
        };
    }

    /// Clear peaks, history and the callback statistics.
    pub(crate) fn reset(&mut self, engine: &EngineController) {
        engine.reset_metrics();
        let _ = engine.take_peak_load();
        self.total = Load::default();
        self.graph = Load::default();
        for (load, mixing) in self.tracks.values_mut() {
            load.peak = 0.0;
            mixing.peak = 0.0;
        }
        for load in self.plugins.values_mut() {
            load.peak = 0.0;
        }
        self.history.clear();
        self.last_poll = None;
    }
}

impl Session {
    /// The latest performance figures (updated a few times per second).
    ///
    /// Reading them keeps per-node timing switched on for a moment, so
    /// tracks and plugins are only measured while someone looks; use
    /// [`Self::dsp_load`] for the total alone.
    pub fn performance(&self) -> &PerformanceReport {
        self.perf.detail_wanted.set(Some(Instant::now()));
        &self.perf.report
    }

    /// Total DSP load (always measured).
    pub fn dsp_load(&self) -> Load {
        self.perf.report.total
    }

    /// Measure now instead of at the next poll (tests, benchmarks).
    pub fn poll_performance(&mut self) {
        let running = self.stream_status().is_some_and(|s| s.running);
        let late = self.streaming_stats().1;
        self.perf.measure(&mut self.engine, &self.project, running);
        self.perf.last_poll = Some(Instant::now());
        self.perf.report.late_disk_reads = late;
    }

    pub(crate) fn tick_performance(&mut self) {
        let detail = self
            .perf
            .detail_wanted
            .get()
            .is_some_and(|t| t.elapsed() < DETAIL_LINGER);
        if detail != self.engine.node_timing() {
            self.engine.set_node_timing(detail);
        }
        let due = self.perf.last_poll.is_none_or(|t| t.elapsed() >= PERF_POLL);
        if due {
            self.poll_performance();
        }
    }

    pub(crate) fn reset_performance(&mut self) {
        self.perf.reset(&self.engine);
        self.revision += 1;
    }
}
