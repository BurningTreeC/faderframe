//! The parallel executor against the serial one: random graphs with state,
//! latency compensation and events must produce bit-identical output, jobs
//! must be fused sensibly, and wide graphs must actually spread over
//! threads.
#![allow(clippy::unwrap_used)]

use faderframe_audio_graph::{
    CompiledGraph, GraphBuilder, NodeId, NodeIo, NodeRole, NodeSpec, PrepareConfig, ProcessContext,
    Processor,
};
use faderframe_core::ChannelLayout::{Mono, Stereo};
use faderframe_midi::{MidiEvent, TimedMidiEvent};
use faderframe_realtime::{PoolConfig, WorkerPool};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Ctx = ();

/// Deterministic node: a one-pole filter of its input plus a per-node
/// signature, optional latency, events shifted by one key.
struct Node {
    seed: u32,
    state: f32,
    latency: u32,
    spin: Duration,
    threads: Option<Arc<Mutex<HashSet<std::thread::ThreadId>>>>,
    log: Option<Arc<Mutex<Vec<u32>>>>,
}

impl Processor<Ctx> for Node {
    fn latency(&self) -> u32 {
        self.latency
    }

    fn process(&mut self, _cx: &ProcessContext<'_, Ctx>, io: &mut NodeIo<'_>) {
        if let Some(t) = &self.threads
            && let Ok(mut t) = t.lock()
        {
            t.insert(std::thread::current().id());
        }
        if let Some(l) = &self.log
            && let Ok(mut l) = l.lock()
        {
            l.push(self.seed);
        }
        if !self.spin.is_zero() {
            let start = Instant::now();
            while start.elapsed() < self.spin {
                std::hint::spin_loop();
            }
        }
        let k = (self.seed % 97) as f32 / 97.0;
        for c in 0..io.audio_out[0].num_channels() {
            let mut st = self.state;
            for f in 0..io.frames {
                let x = io.audio_in.first().map_or(0.0, |b| {
                    let ch = c.min(b.num_channels().saturating_sub(1));
                    b.channel(ch)[f]
                });
                st = 0.7 * st + 0.3 * x + k * 0.01 * ((f as u32 + self.seed) % 13) as f32;
                io.audio_out[0].channel_mut(c)[f] = st;
            }
            if c == 0 {
                self.state = st;
            }
        }
        if let (Some(inp), Some(out)) = (io.events_in.first(), io.events_out.first_mut()) {
            for e in inp.iter() {
                if let MidiEvent::NoteOn {
                    channel,
                    key,
                    velocity,
                } = e.event
                {
                    let _ = out.push(TimedMidiEvent {
                        sample_offset: e.sample_offset,
                        event: MidiEvent::NoteOn {
                            channel,
                            key: (key + 1) % 128,
                            velocity,
                        },
                    });
                }
            }
            if self.seed.is_multiple_of(5) {
                let _ = out.push(TimedMidiEvent {
                    sample_offset: (self.seed % 32),
                    event: MidiEvent::NoteOn {
                        channel: 0,
                        key: (self.seed % 128) as u8,
                        velocity: 100,
                    },
                });
            }
        }
    }
}

/// xorshift for reproducible graphs.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

const BLOCK: usize = 64;

/// A random layered DAG with fan-in, fan-out, latency and event edges.
fn random_graph(seed: u64, nodes: usize) -> (CompiledGraph<Ctx>, Vec<NodeId>) {
    let mut rng = Rng(seed);
    let mut b = GraphBuilder::<Ctx>::new();
    let mut ids = Vec::new();
    for i in 0..nodes {
        let layout = if rng.below(3) == 0 { Mono } else { Stereo };
        let id = b.add_node(
            NodeSpec::new(format!("n{i}"))
                .audio_in(layout)
                .audio_out(layout)
                .events_in(1)
                .events_out(1),
            Box::new(Node {
                seed: rng.next() as u32,
                state: 0.0,
                latency: if rng.below(6) == 0 {
                    rng.below(200) as u32
                } else {
                    0
                },
                spin: Duration::ZERO,
                threads: None,
                log: None,
            }),
        );
        // Edges from earlier nodes (acyclic by construction).
        if i > 0 {
            for _ in 0..rng.below(3) {
                let from = ids[rng.below(i as u64) as usize];
                let _ = b.connect_audio(from, 0, id, 0);
                if rng.below(3) == 0 {
                    let _ = b.connect_events(from, 0, id, 0);
                }
            }
        }
        ids.push(id);
    }
    let out = b.add_node(
        NodeSpec::new("out")
            .audio_in(Stereo)
            .role(NodeRole::DeviceOutput { first_channel: 0 }),
        Box::new(faderframe_audio_graph::nodes::Passthrough),
    );
    // Every node without a consumer feeds the output.
    for &id in &ids {
        let _ = b.connect_audio(id, 0, out, 0);
    }
    ids.push(out);
    // Cheap nodes: force the parallel path anyway.
    let mut config = PrepareConfig::new(48_000.0, BLOCK);
    config.parallel_min_ns = 0;
    (b.compile(&config).unwrap(), ids)
}

fn cx(frames: usize) -> ProcessContext<'static, Ctx> {
    ProcessContext {
        frames,
        sample_rate: 48_000.0,
        data: &(),
    }
}

fn snapshot(g: &CompiledGraph<Ctx>, ids: &[NodeId]) -> Vec<u32> {
    let mut out = Vec::new();
    for &id in ids {
        if let Some(b) = g.audio_output(id, 0) {
            for c in 0..b.num_channels() {
                out.extend(b.channel(c).iter().map(|v| v.to_bits()));
            }
        }
        if let Some(b) = g.audio_input(id, 0) {
            out.extend(b.channel(0).iter().map(|v| v.to_bits()));
        }
        if let Some(e) = g.event_output(id, 0) {
            for ev in e.iter() {
                out.push(ev.sample_offset);
                out.push(ev.event.to_bytes().0[1] as u32);
            }
        }
    }
    out
}

#[test]
fn parallel_output_is_bit_identical_to_serial() {
    let pool = WorkerPool::new(PoolConfig::new(5));
    for seed in 1..=12u64 {
        let (mut serial, ids) = random_graph(seed * 7919, 120);
        let (mut parallel, _) = random_graph(seed * 7919, 120);
        assert!(parallel.stats().jobs <= parallel.node_count());
        for cycle in 0..40 {
            // Varying block sizes, like chunked callbacks.
            let frames = [BLOCK, 17, 1, BLOCK, 33][cycle % 5];
            serial.process(&cx(frames));
            parallel.process_parallel(&cx(frames), &pool);
            assert_eq!(
                snapshot(&serial, &ids),
                snapshot(&parallel, &ids),
                "seed {seed} cycle {cycle}"
            );
        }
    }
}

fn chain(
    b: &mut GraphBuilder<Ctx>,
    len: usize,
    spin: Duration,
    threads: &Arc<Mutex<HashSet<std::thread::ThreadId>>>,
) -> (NodeId, NodeId) {
    let mut first = None;
    let mut prev: Option<NodeId> = None;
    for i in 0..len {
        let id = b.add_node(
            NodeSpec::new(format!("c{i}"))
                .audio_in(Stereo)
                .audio_out(Stereo),
            Box::new(Node {
                seed: i as u32,
                state: 0.0,
                latency: 0,
                spin,
                threads: Some(Arc::clone(threads)),
                log: None,
            }),
        );
        if let Some(p) = prev {
            b.connect_audio(p, 0, id, 0).unwrap();
        }
        first.get_or_insert(id);
        prev = Some(id);
    }
    (first.unwrap(), prev.unwrap())
}

#[test]
fn track_chains_fuse_into_jobs_and_spread_over_threads() {
    let threads = Arc::new(Mutex::new(HashSet::new()));
    let mut b = GraphBuilder::<Ctx>::new();
    let master = b.add_node(
        NodeSpec::new("master").audio_in(Stereo).audio_out(Stereo),
        Box::new(faderframe_audio_graph::nodes::Passthrough),
    );
    let tracks = 8;
    for _ in 0..tracks {
        // A source feeding a 4-node chain: one job per track.
        let src = b.add_node(
            NodeSpec::new("src").audio_out(Stereo),
            Box::new(faderframe_audio_graph::nodes::Constant { value: 0.5 }),
        );
        let (first, last) = chain(&mut b, 4, Duration::from_micros(150), &threads);
        b.connect_audio(src, 0, first, 0).unwrap();
        b.connect_audio(last, 0, master, 0).unwrap();
    }
    let mut g = b.compile(&PrepareConfig::new(48_000.0, BLOCK)).unwrap();
    // 8 track jobs (source + chain) and the master.
    assert_eq!(g.stats().jobs, tracks + 1);
    assert_eq!(g.stats().job_width, tracks);
    assert!(g.jobs().all(|j| j.len() == 5 || j.len() == 1));

    let pool = WorkerPool::new(PoolConfig::new(3));
    // Serial: 8 tracks × 4 nodes × 150 µs ≈ 4.8 ms.
    let t = Instant::now();
    g.process(&cx(BLOCK));
    let serial = t.elapsed();
    // Parked workers can take a while to wake on a loaded virtual machine:
    // give them a few dozen runs to join in.
    let mut best = Duration::MAX;
    let mut used = 0;
    for _ in 0..40 {
        threads.lock().unwrap().clear();
        let t = Instant::now();
        g.process_parallel(&cx(BLOCK), &pool);
        best = best.min(t.elapsed());
        used = used.max(threads.lock().unwrap().len());
        if used >= 2 && best < serial.mul_f64(0.75) {
            break;
        }
    }
    assert!(used >= 2, "only {used} thread(s) used");
    // Four threads: about a quarter of the serial time (allow scheduling
    // noise on busy CI machines; checked where the pool has its realtime
    // wake-up).
    if cfg!(target_os = "linux") {
        assert!(
            best < serial.mul_f64(0.75),
            "parallel {best:?} vs serial {serial:?}"
        );
    }
}

#[test]
fn narrow_graphs_stay_on_the_calling_thread() {
    let threads = Arc::new(Mutex::new(HashSet::new()));
    let mut b = GraphBuilder::<Ctx>::new();
    chain(&mut b, 20, Duration::ZERO, &threads);
    let mut g = b.compile(&PrepareConfig::new(48_000.0, BLOCK)).unwrap();
    assert_eq!(g.stats().jobs, 1);
    let pool = WorkerPool::new(PoolConfig::new(3));
    g.process_parallel(&cx(BLOCK), &pool);
    let used = threads.lock().unwrap().clone();
    assert_eq!(used.len(), 1);
    assert!(used.contains(&std::thread::current().id()));
}

#[test]
fn measured_cost_decides_what_starts_first() {
    // One expensive single-node job (a heavy plugin) next to six cheaper
    // three-node chains: by node count the chains look longer, by measured
    // time the heavy job is the critical path and must start first.
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut b = GraphBuilder::<Ctx>::new();
    let master = b.add_node(
        NodeSpec::new("master").audio_in(Stereo).audio_out(Stereo),
        Box::new(faderframe_audio_graph::nodes::Passthrough),
    );
    let node = |seed: u32, spin: u64| Node {
        seed,
        state: 0.0,
        latency: 0,
        spin: Duration::from_micros(spin),
        threads: None,
        log: Some(Arc::clone(&log)),
    };
    for t in 0..6u32 {
        let mut prev: Option<NodeId> = None;
        for k in 0..3u32 {
            let id = b.add_node(
                NodeSpec::new("light").audio_in(Stereo).audio_out(Stereo),
                Box::new(node(100 + t * 10 + k, 60)),
            );
            if let Some(p) = prev {
                b.connect_audio(p, 0, id, 0).unwrap();
            }
            prev = Some(id);
        }
        b.connect_audio(prev.unwrap(), 0, master, 0).unwrap();
    }
    let heavy = b.add_node(
        NodeSpec::new("heavy").audio_in(Stereo).audio_out(Stereo),
        Box::new(node(1, 1500)),
    );
    b.connect_audio(heavy, 0, master, 0).unwrap();
    let mut g = b.compile(&PrepareConfig::new(48_000.0, BLOCK)).unwrap();
    // One worker: the order of the roots decides the cycle's length.
    let pool = WorkerPool::new(PoolConfig::new(1));
    let first_two = |g: &mut CompiledGraph<Ctx>| {
        log.lock().unwrap().clear();
        g.process_parallel(&cx(BLOCK), &pool);
        log.lock().unwrap()[..2].to_vec()
    };
    let initial = first_two(&mut g);
    assert!(
        !initial.contains(&1),
        "by node count the chains go first: {initial:?}"
    );
    // Ranks are refreshed every 16 cycles from measured times.
    for _ in 0..17 {
        g.process_parallel(&cx(BLOCK), &pool);
    }
    let later = first_two(&mut g);
    assert!(later.contains(&1), "the heavy job starts first: {later:?}");
}
