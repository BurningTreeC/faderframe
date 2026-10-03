use crate::builder::{EdgeKind, GraphBuilder, NodeDesc};
use crate::delay::{AudioDelay, EventDelay};
use crate::topology::topological_order;
use crate::{
    AudioBuffer, GraphError, NodeId, NodeIo, NodeKey, NodeRole, PrepareConfig, ProcessContext,
    Processor,
};
use faderframe_midi::MidiBuffer;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

struct CompiledNode<C> {
    id: NodeId,
    key: Option<NodeKey>,
    role: NodeRole,
    processor: Box<dyn Processor<C>>,
    audio_in: Vec<AudioBuffer>,
    audio_out: Vec<AudioBuffer>,
    events_in: Vec<MidiBuffer>,
    events_out: Vec<MidiBuffer>,
    audio_sources: Vec<AudioSource>,
    event_sources: Vec<EventSource>,
    latency: u32,
    output_latency: u32,
}

/// An immutable-topology, preallocated, ready-to-run graph.
///
/// Nodes are stored in topological order, so every node's upstream nodes
/// live at lower indices. The serial executor exploits that with
/// `split_at_mut`; a future parallel executor uses
/// [`CompiledGraph::dependency_count`] / [`CompiledGraph::dependents`].
pub struct CompiledGraph<C> {
    nodes: Vec<CompiledNode<C>>,
    /// `NodeId.0` → topological index.
    position: Vec<u32>,
    key_index: Vec<(NodeKey, u32)>,
    dependency_counts: Vec<u32>,
    dependents: Vec<Vec<u32>>,
    levels: Vec<u32>,
    config: PrepareConfig,
    stats: GraphStats,
    timings: Arc<NodeTimings>,
    /// Time per node / group in the current callback (audio thread only).
    cycle_node_ns: Vec<u64>,
    cycle_group_ns: Vec<u64>,
}

pub(crate) fn compile<C>(
    builder: GraphBuilder<C>,
    config: &PrepareConfig,
) -> Result<CompiledGraph<C>, GraphError> {
    let GraphBuilder { nodes, edges } = builder;
    if nodes.len() >= u32::MAX as usize {
        return Err(GraphError::Limit("too many nodes"));
    }
    let pairs: Vec<(usize, usize)> = edges
        .iter()
        .map(|e| (e.from.0 as usize, e.to.0 as usize))
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
    let mut slots: Vec<Option<NodeDesc<C>>> = nodes.into_iter().map(Some).collect();
    let max_block = config.max_block_size.max(1);
    let mut compiled: Vec<CompiledNode<C>> = Vec::with_capacity(order.len());
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
        let latency = processor.latency();
        compiled.push(CompiledNode {
            id: NodeId(n as u32),
            key: spec.key,
            role: spec.role,
            processor,
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
            audio_sources: Vec::new(),
            event_sources: Vec::new(),
            latency,
            output_latency: 0,
        });
        labels.push(spec.label);
        groups_of.push(spec.group);
    }

    // Attach edges to their destination nodes (sorted for determinism).
    let mut sorted_edges = edges.clone();
    sorted_edges.sort_by_key(|e| {
        (
            position[e.to.0 as usize],
            e.to_port,
            position[e.from.0 as usize],
            e.from_port,
        )
    });
    for e in &sorted_edges {
        let to = position[e.to.0 as usize] as usize;
        let from = position[e.from.0 as usize];
        match e.kind {
            EdgeKind::Audio => compiled[to].audio_sources.push(AudioSource {
                to_port: e.to_port,
                from_node: from,
                from_port: e.from_port,
                delay: None,
            }),
            EdgeKind::Events => compiled[to].event_sources.push(EventSource {
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
            .audio_sources
            .iter()
            .map(|s| done[s.from_node as usize].output_latency)
            .chain(
                node.event_sources
                    .iter()
                    .map(|s| done[s.from_node as usize].output_latency),
            )
            .max()
            .unwrap_or(0);
        for s in &mut node.audio_sources {
            let src = &done[s.from_node as usize];
            let comp = arrival - src.output_latency;
            if comp > 0 {
                let channels = src.audio_out[s.from_port as usize].num_channels();
                s.delay = Some(AudioDelay::new(channels, comp as usize, max_block));
                stats.compensated_edges += 1;
                stats.max_compensation = stats.max_compensation.max(comp);
            }
        }
        for s in &mut node.event_sources {
            let comp = arrival - done[s.from_node as usize].output_latency;
            if comp > 0 {
                s.delay = Some(EventDelay::new(comp as usize, config.event_capacity));
                stats.compensated_edges += 1;
                stats.max_compensation = stats.max_compensation.max(comp);
            }
        }
        node.output_latency = arrival + node.latency;
        if matches!(node.role, NodeRole::DeviceOutput { .. }) {
            stats.output_latency = stats.output_latency.max(arrival);
        }
    }

    // Dependency information for parallel scheduling.
    let n = compiled.len();
    let mut dependency_counts = vec![0u32; n];
    let mut dependents: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut levels = vec![0u32; n];
    for (i, node) in compiled.iter().enumerate() {
        let mut ups: Vec<u32> = node
            .audio_sources
            .iter()
            .map(|s| s.from_node)
            .chain(node.event_sources.iter().map(|s| s.from_node))
            .collect();
        ups.sort_unstable();
        ups.dedup();
        dependency_counts[i] = ups.len() as u32;
        levels[i] = ups
            .iter()
            .map(|&u| levels[u as usize] + 1)
            .max()
            .unwrap_or(0);
        for u in ups {
            dependents[u as usize].push(i as u32);
        }
    }
    stats.levels = levels.iter().map(|&l| l as usize + 1).max().unwrap_or(0);
    let mut width = vec![0usize; stats.levels];
    for &l in &levels {
        width[l as usize] += 1;
    }
    stats.max_width = width.into_iter().max().unwrap_or(0);

    let mut key_index: Vec<(NodeKey, u32)> = compiled
        .iter()
        .enumerate()
        .filter_map(|(i, nd)| nd.key.map(|k| (k, i as u32)))
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

    Ok(CompiledGraph {
        nodes: compiled,
        position,
        key_index,
        dependency_counts,
        dependents,
        levels,
        config: *config,
        stats,
        timings,
        cycle_node_ns: vec![0; n],
        cycle_group_ns: vec![0; group_count],
    })
}

impl<C> CompiledGraph<C> {
    pub fn config(&self) -> &PrepareConfig {
        &self.config
    }

    pub fn stats(&self) -> &GraphStats {
        &self.stats
    }

    pub fn timings(&self) -> Arc<NodeTimings> {
        Arc::clone(&self.timings)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Topological index of a builder node id.
    pub fn index_of(&self, id: NodeId) -> Option<usize> {
        self.position.get(id.0 as usize).map(|&p| p as usize)
    }

    /// Node id at topological index `i`.
    pub fn id_at(&self, i: usize) -> NodeId {
        self.nodes[i].id
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

    /// Total latency at a node's outputs (its own plus upstream).
    pub fn output_latency(&self, id: NodeId) -> Option<u32> {
        self.index_of(id).map(|i| self.nodes[i].output_latency)
    }

    /// Delay-compensation applied on each edge into `id`
    /// (diagnostics; allocates).
    pub fn compensation_into(&self, id: NodeId) -> Vec<(NodeId, u32)> {
        let Some(i) = self.index_of(id) else {
            return Vec::new();
        };
        let node = &self.nodes[i];
        node.audio_sources
            .iter()
            .map(|s| {
                (
                    self.nodes[s.from_node as usize].id,
                    s.delay.as_ref().map_or(0, |d| d.delay() as u32),
                )
            })
            .collect()
    }

    pub fn audio_output(&self, id: NodeId, port: usize) -> Option<&AudioBuffer> {
        let i = self.index_of(id)?;
        self.nodes[i].audio_out.get(port)
    }

    pub fn audio_input(&self, id: NodeId, port: usize) -> Option<&AudioBuffer> {
        let i = self.index_of(id)?;
        self.nodes[i].audio_in.get(port)
    }

    pub fn event_output(&self, id: NodeId, port: usize) -> Option<&MidiBuffer> {
        let i = self.index_of(id)?;
        self.nodes[i].events_out.get(port)
    }

    /// Give the driver mutable access to device-input nodes' output buffers
    /// for `frames` frames (audio thread, before [`Self::process`]).
    #[inline]
    pub fn fill_device_inputs(
        &mut self,
        frames: usize,
        mut fill: impl FnMut(u16, &mut AudioBuffer),
    ) {
        for node in &mut self.nodes {
            if let NodeRole::DeviceInput { first_channel } = node.role
                && let Some(buf) = node.audio_out.first_mut()
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
        for node in &self.nodes {
            if let NodeRole::DeviceOutput { first_channel } = node.role
                && let Some(buf) = node.audio_in.first()
            {
                read(first_channel, buf);
            }
        }
    }

    /// Run every node once (audio thread). Realtime-safe.
    pub fn process(&mut self, cx: &ProcessContext<'_, C>) {
        let frames = cx.frames.min(self.config.max_block_size);
        let measure = self.config.measure_nodes && self.timings.enabled.load(Ordering::Relaxed);
        // One clock read per node: a node's end is the next one's start.
        let mut last = measure.then(Instant::now);
        for i in 0..self.nodes.len() {
            let (done, rest) = self.nodes.split_at_mut(i);
            let node = &mut rest[0];

            for buf in &mut node.audio_in {
                buf.set_len(frames);
                buf.clear();
            }
            for src in &mut node.audio_sources {
                let from = &done[src.from_node as usize].audio_out[src.from_port as usize];
                let dst = &mut node.audio_in[src.to_port as usize];
                match &mut src.delay {
                    None => dst.mix_from(from),
                    Some(delay) => delay.process_mix(from, dst),
                }
            }
            for buf in &mut node.events_in {
                buf.clear();
            }
            for src in &mut node.event_sources {
                let from = &done[src.from_node as usize].events_out[src.from_port as usize];
                let dst = &mut node.events_in[src.to_port as usize];
                match &mut src.delay {
                    None => dst.merge_from(from, 0),
                    Some(delay) => delay.process_merge(from, dst, frames),
                }
            }
            let is_device_input = matches!(node.role, NodeRole::DeviceInput { .. });
            for buf in &mut node.audio_out {
                if !is_device_input || buf.len() != frames {
                    buf.set_len(frames);
                }
            }
            for buf in &mut node.events_out {
                buf.clear();
            }

            let mut io = NodeIo {
                frames,
                audio_in: &node.audio_in,
                audio_out: &mut node.audio_out,
                events_in: &node.events_in,
                events_out: &mut node.events_out,
            };
            node.processor.process(cx, &mut io);
            if let Some(start) = last {
                let now = Instant::now();
                let ns = now.duration_since(start).as_nanos() as u64;
                last = Some(now);
                self.cycle_node_ns[i] += ns;
                if let Some(g) = self.timings.groups_of[i] {
                    self.cycle_group_ns[g as usize] += ns;
                }
            }
        }
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
        for (t, ns) in self.timings.nodes.iter().zip(&mut self.cycle_node_ns) {
            publish(t, ns);
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
        let mut adopted = 0;
        for &(key, idx) in &self.key_index {
            if let Ok(pos) = old.key_index.binary_search_by_key(&key, |&(k, _)| k) {
                let old_idx = old.key_index[pos].1 as usize;
                std::mem::swap(
                    &mut self.nodes[idx as usize].processor,
                    &mut old.nodes[old_idx].processor,
                );
                adopted += 1;
            }
        }
        adopted
    }

    /// Reset every processor (audio thread), e.g. for an "all notes off /
    /// panic" request.
    pub fn reset_all(&mut self) {
        for node in &mut self.nodes {
            node.processor.reset();
        }
    }
}
