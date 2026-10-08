use crate::builder::{EdgeKind, GraphBuilder, NodeDesc};
use crate::delay::{AudioDelay, EventDelay};
use crate::topology::topological_order;
use crate::{
    AudioBuffer, GraphError, NodeId, NodeIo, NodeKey, NodeRole, PrepareConfig, ProcessContext,
    Processor,
};
use faderframe_midi::MidiBuffer;
use faderframe_realtime::{PoolJob, TaskCells, WorkerPool};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// Accumulated time and peak load of one node or group.
#[derive(Debug, Default)]
struct Timing {
    total_ns: AtomicU64,
    /// Highest share of a callback's time budget used in one callback, in
    /// millionths, since the control side last took it.
    peak_ppm: AtomicU64,
}

/// Per-node (and per-group) processing time, shared with the control side.
///
/// The executor sums each node's time — gathering its inputs plus its
/// processor — over all chunks of a device callback; the driver publishes
/// the sums with [`CompiledGraph::finish_cycle`], which also records the
/// peak share of the callback's budget. Groups (see [`NodeSpec::group`])
/// get the same per callback, so a group's peak is exact, not a sum of its
/// nodes' peaks.
///
/// [`NodeSpec::group`]: crate::NodeSpec::group
#[derive(Debug)]
pub struct NodeTimings {
    /// Measuring costs a clock read per node; it can be switched off at
    /// run time (on by default when the graph was compiled with
    /// `measure_nodes`).
    enabled: AtomicBool,
    labels: Vec<String>,
    groups_of: Vec<Option<u32>>,
    nodes: Box<[Timing]>,
    groups: Box<[Timing]>,
}

impl NodeTimings {
    /// Switch per-node measurement on or off (any thread).
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn len(&self) -> usize {
        self.labels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// Node label at topological index `i`.
    pub fn label(&self, i: usize) -> &str {
        &self.labels[i]
    }

    /// Accounting group of node `i`.
    pub fn group_of(&self, i: usize) -> Option<u32> {
        self.groups_of[i]
    }

    /// Accumulated processing time of node `i` since the graph went live.
    pub fn total_ns(&self, i: usize) -> u64 {
        self.nodes[i].total_ns.load(Ordering::Relaxed)
    }

    /// Peak share of a callback's budget node `i` used since the last call
    /// (resets it).
    pub fn take_peak(&self, i: usize) -> f64 {
        self.nodes[i].peak_ppm.swap(0, Ordering::Relaxed) as f64 / 1e6
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub fn group_total_ns(&self, g: usize) -> u64 {
        self.groups[g].total_ns.load(Ordering::Relaxed)
    }

    pub fn take_group_peak(&self, g: usize) -> f64 {
        self.groups[g].peak_ppm.swap(0, Ordering::Relaxed) as f64 / 1e6
    }
}

/// Structural statistics of a compiled graph (for diagnostics / UI).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphStats {
    pub nodes: usize,
    pub edges: usize,
    /// Length of the longest dependency chain (critical path, in nodes).
    pub levels: usize,
    /// Largest number of nodes sharing a level (upper bound on parallelism).
    pub max_width: usize,
    /// Highest latency arriving at a device output.
    pub output_latency: u32,
    pub compensated_edges: usize,
    pub max_compensation: u32,
    /// Scheduling units of the parallel executor (fused node chains).
    pub jobs: usize,
    /// Largest number of jobs sharing a level: the threads worth using.
    pub job_width: usize,
}

struct AudioSource {
    to_port: u16,
    from_node: u32,
    from_port: u16,
    delay: Option<AudioDelay>,
}

struct EventSource {
    to_port: u16,
    from_node: u32,
    from_port: u16,
    delay: Option<EventDelay>,
}

/// What never changes while the graph runs.
struct NodeInfo {
    id: NodeId,
    key: Option<NodeKey>,
    role: NodeRole,
    output_latency: u32,
    /// (upstream node, compensation) per audio edge, for diagnostics.
    compensation: Vec<(u32, u32)>,
}

/// A node's buffers: written by the node's own job, then read by its
/// dependents (outputs) and the driver (device outputs' inputs).
struct NodeBuffers {
    audio_in: Vec<AudioBuffer>,
    audio_out: Vec<AudioBuffer>,
    events_in: Vec<MidiBuffer>,
    events_out: Vec<MidiBuffer>,
}

/// What only the node's own job touches.
struct NodeWork<C> {
    processor: Box<dyn Processor<C>>,
    audio_sources: Vec<AudioSource>,
    event_sources: Vec<EventSource>,
    /// Time used in the current callback.
    cycle_ns: u64,
}

/// A scheduling unit: a chain of nodes run in order on one thread.
struct Job {
    nodes: Box<[u32]>,
    dependents: Box<[u32]>,
    deps: u32,
}

/// Dependency-driven parallel schedule (state reset every cycle).
///
/// Ranks are the measured cost from a job to the end of the graph (its own
/// time plus the most expensive path through its dependents): roots start
/// in rank order and a thread finishing a job continues with the released
/// dependent of highest rank, so long plugin chains start first instead
/// of becoming the tail of the cycle.
///
/// Affinity: every thread has a seat of its own (the caller 0, pool worker
/// `i` seat `i + 1`, folded onto the queues) and a ready job goes to the
/// queue of the seat that ran it last, so its processors' state and
/// buffers tend to stay in that core's caches. A thread takes its own
/// queue's jobs first and steals from the others when it has none (a seat
/// whose thread did not join this cycle is emptied that way).
struct Schedule {
    jobs: Box<[Job]>,
    /// Jobs in topological order.
    order: Box<[u32]>,
    /// Jobs without dependencies, highest rank first.
    roots: Box<[u32]>,
    /// Time each job took in the last cycle (written by its thread).
    cost: Box<[AtomicU64]>,
    /// Smoothed cost (audio thread only).
    average: Box<[u64]>,
    rank: Box<[AtomicU64]>,
    cycles: u32,
    /// Worth spreading over threads (hysteresis on the total cost).
    parallel: bool,
    remaining: Box<[AtomicU32]>,
    /// Ready jobs per seat (the first `active` this cycle).
    queues: Box<[SeatQueue]>,
    active: usize,
    /// The seat that last ran each job (`u32::MAX`: none yet).
    owner: Box<[AtomicU32]>,
    /// Tags this cycle's queue entries (no clearing between cycles).
    cycle: u32,
    finished: AtomicUsize,
    /// Threads still allowed to join this cycle.
    seats: AtomicUsize,
}

impl Schedule {
    /// After a cycle (audio thread): smooth the measured costs and, every
    /// 16 cycles, recompute ranks, the root order and whether parallel
    /// processing pays off. No allocation.
    fn after_cycle(&mut self, parallel_min_ns: u64) {
        for (a, c) in self.average.iter_mut().zip(self.cost.iter_mut()) {
            let c = *c.get_mut();
            *a = if *a == 0 { c } else { (*a * 7 + c) / 8 };
        }
        self.cycles = self.cycles.wrapping_add(1);
        if self.cycles % 16 != 1 {
            return;
        }
        for &j in self.order.iter().rev() {
            let j = j as usize;
            let tail = self.jobs[j]
                .dependents
                .iter()
                .map(|&d| *self.rank[d as usize].get_mut())
                .max()
                .unwrap_or(0);
            // At least 1 ns per node, so unmeasured jobs keep their shape.
            let own = self.average[j].max(self.jobs[j].nodes.len() as u64);
            *self.rank[j].get_mut() = own + tail;
        }
        // Insertion sort, highest rank first (roots are few).
        for i in 1..self.roots.len() {
            let mut k = i;
            while k > 0
                && *self.rank[self.roots[k - 1] as usize].get_mut()
                    < *self.rank[self.roots[k] as usize].get_mut()
            {
                self.roots.swap(k - 1, k);
                k -= 1;
            }
        }
        let total: u64 = self.average.iter().sum();
        if self.parallel && total < parallel_min_ns / 2 {
            self.parallel = false;
        } else if !self.parallel && total >= parallel_min_ns {
            self.parallel = true;
        }
    }

    fn reset(&mut self, seats: usize) {
        self.active = seats.clamp(1, self.queues.len());
        for (r, j) in self.remaining.iter_mut().zip(self.jobs.iter()) {
            *r.get_mut() = j.deps;
        }
        self.cycle = self.cycle.wrapping_add(1);
        if self.cycle == 0 || self.cycle == u32::MAX {
            // Wrapped (after months): no slot may still carry a live tag.
            for q in self.queues.iter_mut() {
                for slot in q.slots.iter_mut() {
                    *slot.get_mut() = u64::MAX;
                }
            }
            self.cycle = 1;
        }
        let cycle = self.cycle;
        for q in self.queues.iter_mut() {
            *q.head.get_mut() = 0;
            *q.tail.get_mut() = 0;
        }
        // Roots in rank order, each to the seat that ran it (new ones
        // spread over the seats).
        let n = self.active;
        for (i, &r) in self.roots.iter().enumerate() {
            let owner = *self.owner[r as usize].get_mut();
            let q = &mut self.queues[if owner == u32::MAX { i } else { owner as usize } % n];
            let t = q.tail.get_mut();
            if let Some(slot) = q.slots.get_mut(*t) {
                *slot.get_mut() = entry(cycle, r);
            }
            *t += 1;
        }
        *self.finished.get_mut() = 0;
        *self.seats.get_mut() = seats;
    }

    /// A ready job to the queue of the seat that ran it (else `seat`'s).
    #[inline]
    fn push(&self, job: u32, seat: usize) {
        let owner = self.owner[job as usize].load(Ordering::Relaxed);
        let to = if owner == u32::MAX {
            seat
        } else {
            owner as usize
        };
        self.queues[to % self.active].push(self.cycle, job);
    }

    /// The next job for `seat`: its own, else one taken from another seat.
    #[inline]
    fn pop(&self, seat: usize) -> Option<u32> {
        let n = self.active;
        (0..n).find_map(|k| self.queues[(seat + k) % n].pop(self.cycle))
    }
}

/// One seat's ready jobs: a bounded queue (every job is ready once per
/// cycle, so it never overflows). Entries carry the cycle, so a slot of an
/// earlier cycle reads as not written yet.
struct SeatQueue {
    slots: Box<[AtomicU64]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

#[inline]
fn entry(cycle: u32, job: u32) -> u64 {
    (u64::from(cycle) << 32) | u64::from(job)
}

impl SeatQueue {
    fn new(capacity: usize) -> Self {
        Self {
            slots: (0..capacity).map(|_| AtomicU64::new(u64::MAX)).collect(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn push(&self, cycle: u32, job: u32) {
        let t = self.tail.fetch_add(1, Ordering::AcqRel);
        if let Some(slot) = self.slots.get(t) {
            slot.store(entry(cycle, job), Ordering::Release);
        }
    }

    #[inline]
    fn pop(&self, cycle: u32) -> Option<u32> {
        loop {
            let h = self.head.load(Ordering::Acquire);
            if h >= self.tail.load(Ordering::Acquire) {
                return None;
            }
            let v = self.slots.get(h)?.load(Ordering::Acquire);
            if (v >> 32) as u32 != cycle {
                // Reserved by a pusher that has not stored yet.
                return None;
            }
            if self
                .head
                .compare_exchange_weak(h, h + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(v as u32);
            }
        }
    }
}

/// Most seats a schedule has queues for.
const MAX_SEATS: usize = 32;

/// An immutable-topology, preallocated, ready-to-run graph.
///
/// Nodes are stored in topological order. The serial executor runs them in
/// that order; the parallel one runs *jobs* — chains of nodes fused at
/// compile time (a node whose only dependent has no other input, plus
/// source nodes feeding a single job) — as soon as their upstream jobs are
/// done, on the calling thread and a [`WorkerPool`]. Both use the same node
/// runner, so results are bit-identical: every node sums its inputs in a
/// fixed order whatever thread ran its upstream nodes.
pub struct CompiledGraph<C> {
    info: Vec<NodeInfo>,
    bufs: TaskCells<NodeBuffers>,
    work: TaskCells<NodeWork<C>>,
    /// `NodeId.0` → topological index.
    position: Vec<u32>,
    key_index: Vec<(NodeKey, u32)>,
    dependency_counts: Vec<u32>,
    dependents: Vec<Vec<u32>>,
    levels: Vec<u32>,
    schedule: Schedule,
    config: PrepareConfig,
    processing_quantum: usize,
    stats: GraphStats,
    timings: Arc<NodeTimings>,
    cycle_group_ns: Vec<u64>,
}

/// Build node and job tables, compensation and timing for `builder`.
pub(crate) fn compile<C>(
    builder: GraphBuilder<C>,
    config: &PrepareConfig,
) -> Result<CompiledGraph<C>, GraphError> {
    let GraphBuilder { nodes, edges } = builder;
    if nodes.len() >= u32::MAX as usize {
        return Err(GraphError::Limit("too many nodes"));
    }
    // Edges, and the nodes that follow another without one.
    let pairs: Vec<(usize, usize)> = edges
        .iter()
        .map(|e| (e.from.0 as usize, e.to.0 as usize))
        .chain(
            nodes
                .iter()
                .enumerate()
                .filter_map(|(i, n)| n.spec.after.map(|a| (a.0 as usize, i))),
        )
        .filter(|(a, _)| *a < nodes.len())
        .collect();
    let order = topological_order(nodes.len(), &pairs).map_err(|err| {
        GraphError::Cycle(
            err.cycle
                .iter()
                .map(|&i| nodes[i].spec.label.clone())
                .collect(),
        )
    })?;

    let mut position = vec![0u32; nodes.len()];
    for (topo, &n) in order.iter().enumerate() {
        position[n] = topo as u32;
    }

    // Move descriptors into topological order.
    struct Building<C> {
        info: NodeInfo,
        bufs: NodeBuffers,
        work: NodeWork<C>,
        latency: u32,
        /// The node it follows without an edge (topological position).
        after: Option<usize>,
    }
    let mut slots: Vec<Option<NodeDesc<C>>> = nodes.into_iter().map(Some).collect();
    let max_block = config.max_block_size.max(1);
    let mut processing_quantum = max_block;
    let mut compiled: Vec<Building<C>> = Vec::with_capacity(order.len());
    let mut labels: Vec<String> = Vec::with_capacity(order.len());
    let mut groups_of: Vec<Option<u32>> = Vec::with_capacity(order.len());
    for &n in &order {
        let Some(NodeDesc {
            spec,
            mut processor,
        }) = slots[n].take()
        else {
            return Err(GraphError::Limit("node visited twice"));
        };
        processor.prepare(config);
        processing_quantum = processing_quantum.min(processor.preferred_block_size().max(1));
        let latency = processor.latency();
        compiled.push(Building {
            info: NodeInfo {
                id: NodeId(n as u32),
                key: spec.key,
                role: spec.role,
                output_latency: 0,
                compensation: Vec::new(),
            },
            bufs: NodeBuffers {
                audio_in: spec
                    .audio_inputs
                    .iter()
                    .map(|&l| AudioBuffer::new(l, max_block))
                    .collect(),
                audio_out: spec
                    .audio_outputs
                    .iter()
                    .map(|&l| AudioBuffer::new(l, max_block))
                    .collect(),
                events_in: (0..spec.event_inputs)
                    .map(|_| MidiBuffer::with_capacity(config.event_capacity))
                    .collect(),
                events_out: (0..spec.event_outputs)
                    .map(|_| MidiBuffer::with_capacity(config.event_capacity))
                    .collect(),
            },
            work: NodeWork {
                processor,
                audio_sources: Vec::new(),
                event_sources: Vec::new(),
                cycle_ns: 0,
            },
            latency,
            after: spec.after.map(|a| position[a.0 as usize] as usize),
        });
        labels.push(spec.label);
        groups_of.push(spec.group);
    }

    // Audio summing follows the sources' keys (then builder order), not
    // topological position or the order nodes were added in: moving a
    // chain into a render-ahead graph (a reader in its place, its strip and
    // buses built in another order) must not change floating-point
    // addition order where it is summed. Keep event tie ordering in
    // topological order as before.
    let key_of: Vec<(bool, u64)> = compiled
        .iter()
        .map(|b| b.info.key.map_or((true, 0), |k| (false, k.0)))
        .collect();
    let mut sorted_edges = edges.clone();
    sorted_edges.sort_by_key(|e| {
        let from = position[e.from.0 as usize] as usize;
        (
            position[e.to.0 as usize],
            e.to_port,
            match e.kind {
                EdgeKind::Audio => key_of[from],
                EdgeKind::Events => (false, 0),
            },
            match e.kind {
                EdgeKind::Audio => e.from.0,
                EdgeKind::Events => position[e.from.0 as usize],
            },
            e.from_port,
        )
    });
    for e in &sorted_edges {
        let to = position[e.to.0 as usize] as usize;
        let from = position[e.from.0 as usize];
        match e.kind {
            EdgeKind::Audio => compiled[to].work.audio_sources.push(AudioSource {
                to_port: e.to_port,
                from_node: from,
                from_port: e.from_port,
                delay: None,
            }),
            EdgeKind::Events => compiled[to].work.event_sources.push(EventSource {
                to_port: e.to_port,
                from_node: from,
                from_port: e.from_port,
                delay: None,
            }),
        }
    }

    // Latency propagation and compensation. All inputs of a node (main,
    // sidechain, events) are aligned to the latest-arriving one.
    let mut stats = GraphStats {
        nodes: compiled.len(),
        edges: edges.len(),
        ..GraphStats::default()
    };
    for i in 0..compiled.len() {
        let (done, rest) = compiled.split_at_mut(i);
        let node = &mut rest[0];
        let arrival = node
            .work
            .audio_sources
            .iter()
            .map(|s| done[s.from_node as usize].info.output_latency)
            .chain(
                node.work
                    .event_sources
                    .iter()
                    .map(|s| done[s.from_node as usize].info.output_latency),
            )
            .chain(node.after.map(|a| done[a].info.output_latency))
            .max()
            .unwrap_or(0);
        for s in &mut node.work.audio_sources {
            let src = &done[s.from_node as usize];
            let comp = arrival - src.info.output_latency;
            if comp > 0 {
                let channels = src.bufs.audio_out[s.from_port as usize].num_channels();
                s.delay = Some(AudioDelay::new(channels, comp as usize, max_block));
                stats.compensated_edges += 1;
                stats.max_compensation = stats.max_compensation.max(comp);
            }
            node.info.compensation.push((s.from_node, comp));
        }
        for s in &mut node.work.event_sources {
            let comp = arrival - done[s.from_node as usize].info.output_latency;
            if comp > 0 {
                s.delay = Some(EventDelay::new(comp as usize, config.event_capacity));
                stats.compensated_edges += 1;
                stats.max_compensation = stats.max_compensation.max(comp);
            }
        }
        node.info.output_latency = arrival + node.latency;
        if matches!(node.info.role, NodeRole::DeviceOutput { .. }) {
            stats.output_latency = stats.output_latency.max(arrival);
        }
    }

    // Dependencies.
    let n = compiled.len();
    let mut dependency_counts = vec![0u32; n];
    let mut upstream: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut dependents: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut levels = vec![0u32; n];
    for (i, node) in compiled.iter().enumerate() {
        let mut ups: Vec<u32> = node
            .work
            .audio_sources
            .iter()
            .map(|s| s.from_node)
            .chain(node.work.event_sources.iter().map(|s| s.from_node))
            .collect();
        ups.sort_unstable();
        ups.dedup();
        dependency_counts[i] = ups.len() as u32;
        levels[i] = ups
            .iter()
            .map(|&u| levels[u as usize] + 1)
            .max()
            .unwrap_or(0);
        for &u in &ups {
            dependents[u as usize].push(i as u32);
        }
        upstream[i] = ups;
    }
    stats.levels = levels.iter().map(|&l| l as usize + 1).max().unwrap_or(0);
    let mut width = vec![0usize; stats.levels];
    for &l in &levels {
        width[l as usize] += 1;
    }
    stats.max_width = width.into_iter().max().unwrap_or(0);

    let schedule = build_schedule(&upstream, &dependents, &mut stats);

    let mut key_index: Vec<(NodeKey, u32)> = compiled
        .iter()
        .enumerate()
        .filter_map(|(i, nd)| nd.info.key.map(|k| (k, i as u32)))
        .collect();
    key_index.sort_by_key(|&(k, i)| (k, i));
    key_index.dedup_by_key(|&mut (k, _)| k);

    let group_count = groups_of
        .iter()
        .flatten()
        .max()
        .map_or(0, |&g| g as usize + 1);
    let timings = Arc::new(NodeTimings {
        enabled: AtomicBool::new(config.measure_nodes),
        labels,
        groups_of,
        nodes: (0..n).map(|_| Timing::default()).collect(),
        groups: (0..group_count).map(|_| Timing::default()).collect(),
    });

    let mut info = Vec::with_capacity(n);
    let mut bufs = Vec::with_capacity(n);
    let mut work = Vec::with_capacity(n);
    for b in compiled {
        info.push(b.info);
        bufs.push(b.bufs);
        work.push(b.work);
    }
    Ok(CompiledGraph {
        info,
        bufs: TaskCells::new(bufs),
        work: TaskCells::new(work),
        position,
        key_index,
        dependency_counts,
        dependents,
        levels,
        schedule,
        config: *config,
        processing_quantum,
        stats,
        timings,
        cycle_group_ns: vec![0; group_count],
    })
}

/// Fuse nodes into jobs and derive the job graph.
fn build_schedule(
    upstream: &[Vec<u32>],
    dependents: &[Vec<u32>],
    stats: &mut GraphStats,
) -> Schedule {
    let n = upstream.len();
    // Chains: a node whose only dependent has no other input continues
    // that dependent's job (topological order: `job[i]` is final here).
    let mut job: Vec<u32> = (0..n as u32).collect();
    for i in 0..n {
        if let [d] = dependents[i][..]
            && upstream[d as usize].len() == 1
        {
            job[d as usize] = job[i];
        }
    }
    // Sources feeding exactly one node run at the start of its job — when
    // that node depends on sources only (a track input fed by clip players
    // and live input). Otherwise the source would wait for the node's other
    // inputs instead of running in parallel with them.
    let mut size = vec![0usize; n];
    for &j in &job {
        size[j as usize] += 1;
    }
    for i in 0..n {
        if upstream[i].is_empty()
            && size[job[i] as usize] == 1
            && let [d] = dependents[i][..]
            && upstream[d as usize]
                .iter()
                .all(|&u| upstream[u as usize].is_empty())
        {
            size[job[i] as usize] -= 1;
            job[i] = job[d as usize];
            size[job[i] as usize] += 1;
        }
    }
    // Compact job ids (in order of first node: topological).
    let mut id = vec![u32::MAX; n];
    let mut members: Vec<Vec<u32>> = Vec::new();
    for (i, &root) in job.iter().enumerate() {
        let root = root as usize;
        if id[root] == u32::MAX {
            id[root] = members.len() as u32;
            members.push(Vec::new());
        }
        members[id[root] as usize].push(i as u32);
    }
    let job_of: Vec<u32> = job.iter().map(|&j| id[j as usize]).collect();
    let jobs_n = members.len();
    let mut deps: Vec<Vec<u32>> = vec![Vec::new(); jobs_n];
    for (j, nodes) in members.iter().enumerate() {
        for &v in nodes {
            for &u in &upstream[v as usize] {
                let ju = job_of[u as usize];
                if ju as usize != j {
                    deps[j].push(ju);
                }
            }
        }
        deps[j].sort_unstable();
        deps[j].dedup();
    }
    let mut job_dependents: Vec<Vec<u32>> = vec![Vec::new(); jobs_n];
    for (j, ds) in deps.iter().enumerate() {
        for &d in ds {
            job_dependents[d as usize].push(j as u32);
        }
    }
    // Jobs are numbered in topological order of their first node, but a
    // fused job's later nodes may come after a dependent's first node:
    // levels and ranks need a proper order.
    let job_order = {
        let pairs: Vec<(usize, usize)> = deps
            .iter()
            .enumerate()
            .flat_map(|(j, ds)| ds.iter().map(move |&d| (d as usize, j)))
            .collect();
        topological_order(jobs_n, &pairs).unwrap_or_else(|_| (0..jobs_n).collect())
    };
    let mut level = vec![0usize; jobs_n];
    for &j in &job_order {
        level[j] = deps[j]
            .iter()
            .map(|&d| level[d as usize] + 1)
            .max()
            .unwrap_or(0);
    }
    let mut width = vec![0usize; level.iter().max().map_or(0, |l| l + 1)];
    for &l in &level {
        width[l] += 1;
    }
    // Longest path (in nodes) from a job to the end.
    let mut rank = vec![0usize; jobs_n];
    for &j in job_order.iter().rev() {
        rank[j] = members[j].len()
            + job_dependents[j]
                .iter()
                .map(|&d| rank[d as usize])
                .max()
                .unwrap_or(0);
    }
    let mut roots: Vec<u32> = (0..jobs_n as u32)
        .filter(|&j| deps[j as usize].is_empty())
        .collect();
    roots.sort_by_key(|&j| std::cmp::Reverse(rank[j as usize]));
    stats.jobs = jobs_n;
    stats.job_width = width.into_iter().max().unwrap_or(0);
    Schedule {
        order: job_order.iter().map(|&j| j as u32).collect(),
        cost: (0..jobs_n).map(|_| AtomicU64::new(0)).collect(),
        average: vec![0; jobs_n].into(),
        rank: rank.iter().map(|&r| AtomicU64::new(r as u64)).collect(),
        cycles: 0,
        parallel: true,
        jobs: members
            .into_iter()
            .zip(deps)
            .zip(job_dependents)
            .map(|((nodes, deps), dependents)| Job {
                nodes: nodes.into(),
                dependents: dependents.into(),
                deps: deps.len() as u32,
            })
            .collect(),
        roots: roots.into(),
        remaining: (0..jobs_n).map(|_| AtomicU32::new(0)).collect(),
        queues: (0..stats.job_width.clamp(1, MAX_SEATS))
            .map(|_| SeatQueue::new(jobs_n))
            .collect(),
        owner: (0..jobs_n).map(|_| AtomicU32::new(u32::MAX)).collect(),
        active: 1,
        cycle: 0,
        finished: AtomicUsize::new(0),
        seats: AtomicUsize::new(0),
    }
}

/// Graphs below this many nodes always run serially.
const MIN_PARALLEL_NODES: usize = 8;

/// One parallel cycle, shared by the participating threads.
/// Decrement `seats` if it is positive (a compare-exchange loop:
/// `fetch_update` is deprecated on newer toolchains, `try_update` missing on
/// older ones).
#[inline]
fn take_seat(seats: &std::sync::atomic::AtomicUsize) -> bool {
    let mut n = seats.load(Ordering::Acquire);
    while n > 0 {
        match seats.compare_exchange_weak(n, n - 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(now) => n = now,
        }
    }
    false
}

struct Exec<'a, 'c, C> {
    graph: &'a CompiledGraph<C>,
    cx: &'a ProcessContext<'c, C>,
    frames: usize,
    measure: bool,
}

impl<C: Sync> PoolJob for Exec<'_, '_, C> {
    fn work(&self) {
        let s = &self.graph.schedule;
        // Threads beyond the graph's useful width leave at once.
        if !take_seat(&s.seats) {
            return;
        }
        let total = s.jobs.len();
        let seat = faderframe_realtime::worker_seat();
        let mut next: Option<u32> = None;
        loop {
            let j = match next.take().or_else(|| s.pop(seat)) {
                Some(j) => j,
                None => {
                    if s.finished.load(Ordering::Acquire) >= total {
                        return;
                    }
                    std::hint::spin_loop();
                    continue;
                }
            };
            let Some(job) = s.jobs.get(j as usize) else {
                continue;
            };
            self.graph
                .run_job(j as usize, self.cx, self.frames, self.measure);
            s.owner[j as usize].store(seat as u32, Ordering::Relaxed);
            // Continue with the most expensive released dependent (it reads
            // what this thread just wrote), queue the others.
            let rank = |d: u32| s.rank[d as usize].load(Ordering::Relaxed);
            for &d in job.dependents.iter() {
                if s.remaining[d as usize].fetch_sub(1, Ordering::AcqRel) == 1 {
                    match next {
                        None => next = Some(d),
                        Some(n) if rank(d) > rank(n) => {
                            s.push(n, seat);
                            next = Some(d);
                        }
                        Some(_) => s.push(d, seat),
                    }
                }
            }
            s.finished.fetch_add(1, Ordering::AcqRel);
        }
    }
}

impl<C> CompiledGraph<C> {
    pub fn config(&self) -> &PrepareConfig {
        &self.config
    }

    /// Driver chunk size chosen from the prepared capacity and node preferences.
    pub fn processing_quantum(&self) -> usize {
        self.processing_quantum
    }

    pub fn stats(&self) -> &GraphStats {
        &self.stats
    }

    pub fn timings(&self) -> Arc<NodeTimings> {
        Arc::clone(&self.timings)
    }

    pub fn node_count(&self) -> usize {
        self.info.len()
    }

    /// Topological index of a builder node id.
    pub fn index_of(&self, id: NodeId) -> Option<usize> {
        self.position.get(id.0 as usize).map(|&p| p as usize)
    }

    /// Node id at topological index `i`.
    pub fn id_at(&self, i: usize) -> NodeId {
        self.info[i].id
    }

    /// Number of distinct upstream nodes of topological node `i`.
    pub fn dependency_count(&self, i: usize) -> u32 {
        self.dependency_counts[i]
    }

    /// Topological indices of nodes that depend on node `i`.
    pub fn dependents(&self, i: usize) -> &[u32] {
        &self.dependents[i]
    }

    /// Dependency level (0 = no inputs) of node `i`.
    pub fn level(&self, i: usize) -> u32 {
        self.levels[i]
    }

    /// Topological indices of the nodes of each parallel job.
    pub fn jobs(&self) -> impl Iterator<Item = &[u32]> {
        self.schedule.jobs.iter().map(|j| &j.nodes[..])
    }

    /// Total latency at a node's outputs (its own plus upstream).
    pub fn output_latency(&self, id: NodeId) -> Option<u32> {
        self.index_of(id).map(|i| self.info[i].output_latency)
    }

    /// Delay-compensation applied on each edge into `id`
    /// (diagnostics; allocates).
    pub fn compensation_into(&self, id: NodeId) -> Vec<(NodeId, u32)> {
        let Some(i) = self.index_of(id) else {
            return Vec::new();
        };
        self.info[i]
            .compensation
            .iter()
            .map(|&(from, comp)| (self.info[from as usize].id, comp))
            .collect()
    }

    /// A node's output after processing.
    pub fn audio_output(&self, id: NodeId, port: usize) -> Option<&AudioBuffer> {
        let i = self.index_of(id)?;
        self.bufs.done(i)?.audio_out.get(port)
    }

    /// A node's summed input after processing.
    pub fn audio_input(&self, id: NodeId, port: usize) -> Option<&AudioBuffer> {
        let i = self.index_of(id)?;
        self.bufs.done(i)?.audio_in.get(port)
    }

    pub fn event_output(&self, id: NodeId, port: usize) -> Option<&MidiBuffer> {
        let i = self.index_of(id)?;
        self.bufs.done(i)?.events_out.get(port)
    }

    /// Give the driver mutable access to device-input nodes' output buffers
    /// for `frames` frames (audio thread, before [`Self::process`]).
    #[inline]
    pub fn fill_device_inputs(
        &mut self,
        frames: usize,
        mut fill: impl FnMut(u16, &mut AudioBuffer),
    ) {
        for (i, info) in self.info.iter().enumerate() {
            if let NodeRole::DeviceInput { first_channel } = info.role
                && let Some(buf) = self.bufs.get_mut(i).and_then(|b| b.audio_out.first_mut())
            {
                buf.set_len(frames);
                fill(first_channel, buf);
            }
        }
    }

    /// Visit device-output nodes' input buffers (audio thread, after
    /// [`Self::process`]).
    #[inline]
    pub fn read_device_outputs(&self, mut read: impl FnMut(u16, &AudioBuffer)) {
        for (i, info) in self.info.iter().enumerate() {
            if let NodeRole::DeviceOutput { first_channel } = info.role
                && let Some(buf) = self.bufs.done(i).and_then(|b| b.audio_in.first())
            {
                read(first_channel, buf);
            }
        }
    }

    /// Visit event-output nodes' first event output (audio thread, after
    /// [`Self::process`]).
    #[inline]
    pub fn read_event_outputs(&self, mut read: impl FnMut(u16, &MidiBuffer)) {
        for (i, info) in self.info.iter().enumerate() {
            if let NodeRole::EventOutput { port } = info.role
                && let Some(buf) = self.bufs.done(i).and_then(|b| b.events_out.first())
            {
                read(port, buf);
            }
        }
    }

    /// Gather node `i`'s inputs from its (finished) upstream nodes and run
    /// its processor. `last` chains clock reads: a node's end is the next
    /// one's start on the same thread.
    #[inline]
    fn run_node(
        &self,
        i: usize,
        cx: &ProcessContext<'_, C>,
        frames: usize,
        last: &mut Option<Instant>,
    ) {
        let (Some(mut w), Some(mut b)) = (self.work.claim(i), self.bufs.claim(i)) else {
            // Scheduling error (cannot happen): skip rather than race.
            return;
        };
        let NodeWork {
            processor,
            audio_sources,
            event_sources,
            cycle_ns,
        } = &mut *w;
        let NodeBuffers {
            audio_in,
            audio_out,
            events_in,
            events_out,
        } = &mut *b;
        for buf in audio_in.iter_mut() {
            buf.set_len(frames);
            buf.clear();
        }
        for src in audio_sources.iter_mut() {
            let Some(up) = self.bufs.done(src.from_node as usize) else {
                continue;
            };
            let from = &up.audio_out[src.from_port as usize];
            let dst = &mut audio_in[src.to_port as usize];
            match &mut src.delay {
                None => dst.mix_from(from),
                Some(delay) => delay.process_mix(from, dst),
            }
        }
        for buf in events_in.iter_mut() {
            buf.clear();
        }
        for src in event_sources.iter_mut() {
            let Some(up) = self.bufs.done(src.from_node as usize) else {
                continue;
            };
            let from = &up.events_out[src.from_port as usize];
            let dst = &mut events_in[src.to_port as usize];
            match &mut src.delay {
                None => dst.merge_from(from, 0),
                Some(delay) => delay.process_merge(from, dst, frames),
            }
        }
        let is_device_input = matches!(self.info[i].role, NodeRole::DeviceInput { .. });
        for buf in audio_out.iter_mut() {
            if !is_device_input || buf.len() != frames {
                buf.set_len(frames);
            }
        }
        for buf in events_out.iter_mut() {
            buf.clear();
        }
        let mut io = NodeIo {
            frames,
            audio_in,
            audio_out,
            events_in,
            events_out,
        };
        processor.process(cx, &mut io);
        if let Some(start) = *last {
            let now = Instant::now();
            *cycle_ns += now.duration_since(start).as_nanos() as u64;
            *last = Some(now);
        }
    }

    /// Run one job's nodes in order and record its time.
    #[inline]
    fn run_job(&self, j: usize, cx: &ProcessContext<'_, C>, frames: usize, measure_nodes: bool) {
        let s = &self.schedule;
        let start = Instant::now();
        let mut last = measure_nodes.then_some(start);
        for &node in s.jobs[j].nodes.iter() {
            self.run_node(node as usize, cx, frames, &mut last);
        }
        s.cost[j].store(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    fn measuring(&self) -> bool {
        self.config.measure_nodes && self.timings.enabled.load(Ordering::Relaxed)
    }

    /// Run every node once on this thread, job by job in topological
    /// order (audio thread). Realtime-safe.
    pub fn process(&mut self, cx: &ProcessContext<'_, C>) {
        let frames = cx.frames.min(self.config.max_block_size);
        self.bufs.reset();
        self.work.reset();
        let measure = self.measuring();
        for k in 0..self.schedule.order.len() {
            let j = self.schedule.order[k] as usize;
            self.run_job(j, cx, frames, measure);
        }
        self.schedule.after_cycle(self.config.parallel_min_ns);
    }

    /// Run every node once, spreading independent jobs over this thread
    /// and `pool`'s workers (audio thread). Falls back to [`Self::process`]
    /// for graphs that cannot use more than one thread. Realtime-safe.
    pub fn process_parallel(&mut self, cx: &ProcessContext<'_, C>, pool: &WorkerPool)
    where
        C: Sync,
    {
        let helpers = self
            .stats
            .job_width
            .saturating_sub(1)
            .min(pool.useful_helpers());
        if helpers == 0 || self.info.len() < MIN_PARALLEL_NODES || !self.schedule.parallel {
            self.process(cx);
            return;
        }
        let frames = cx.frames.min(self.config.max_block_size);
        self.bufs.reset();
        self.work.reset();
        self.schedule.reset(helpers + 1);
        let exec = Exec {
            graph: &*self,
            cx,
            frames,
            measure: self.measuring(),
        };
        pool.run(&exec, helpers);
        self.schedule.after_cycle(self.config.parallel_min_ns);
    }

    /// Whether the last cycles were worth spreading over threads.
    pub fn runs_parallel(&self) -> bool {
        self.schedule.parallel
    }

    /// End of a device callback (audio thread): publish the time each node
    /// and group used in it, and their peak share of `budget_ns` (the
    /// callback's duration). Realtime-safe.
    pub fn finish_cycle(&mut self, budget_ns: u64) {
        if !self.config.measure_nodes
            || budget_ns == 0
            || !self.timings.enabled.load(Ordering::Relaxed)
        {
            return;
        }
        let ppm = |ns: u64| (ns as u128 * 1_000_000 / budget_ns as u128) as u64;
        // The audio thread is the only writer (the control side only reads
        // totals and swaps peaks to zero), so plain loads and stores do —
        // no locked read-modify-write per node. A peak taken between the
        // load and the store lands in the next window.
        let publish = |t: &Timing, ns: &mut u64| {
            if *ns > 0 {
                let total = t.total_ns.load(Ordering::Relaxed);
                t.total_ns.store(total + *ns, Ordering::Relaxed);
                let p = ppm(*ns);
                if p > t.peak_ppm.load(Ordering::Relaxed) {
                    t.peak_ppm.store(p, Ordering::Relaxed);
                }
                *ns = 0;
            }
        };
        for (i, w) in self.work.iter_mut().enumerate() {
            if let Some(g) = self.timings.groups_of[i] {
                self.cycle_group_ns[g as usize] += w.cycle_ns;
            }
            publish(&self.timings.nodes[i], &mut w.cycle_ns);
        }
        for (t, ns) in self.timings.groups.iter().zip(&mut self.cycle_group_ns) {
            publish(t, ns);
        }
    }

    /// Carry processors over from the graph being replaced (audio thread).
    ///
    /// For every node in `self` whose [`NodeKey`] also exists in `old`, the
    /// processors are swapped: the running instance moves into `self` and
    /// the freshly built one ends up in `old`, which is then dropped on the
    /// control thread. Only pointer swaps and binary searches — no
    /// allocation. Returns the number of adopted processors.
    pub fn adopt_state_from(&mut self, old: &mut CompiledGraph<C>) -> usize {
        if old.config.sample_rate != self.config.sample_rate
            || old.config.max_block_size != self.config.max_block_size
        {
            return 0;
        }
        // The new graph starts with the old one's verdict on threading.
        self.schedule.parallel = old.schedule.parallel;
        let mut adopted = 0;
        for &(key, idx) in &self.key_index {
            if let Ok(pos) = old.key_index.binary_search_by_key(&key, |&(k, _)| k) {
                let old_idx = old.key_index[pos].1 as usize;
                if let (Some(new), Some(prev)) =
                    (self.work.get_mut(idx as usize), old.work.get_mut(old_idx))
                {
                    std::mem::swap(&mut new.processor, &mut prev.processor);
                    adopted += 1;
                }
            }
        }
        adopted
    }

    /// Reset every processor (audio thread), e.g. for an "all notes off /
    /// panic" request.
    pub fn reset_all(&mut self) {
        for w in self.work.iter_mut() {
            w.processor.reset();
        }
    }
}
