use faderframe_audio_graph::nodes::{Constant, Gain, LatencyProbe, Passthrough};
use faderframe_audio_graph::{
    GraphBuilder, GraphError, NodeId, NodeIo, NodeKey, NodeRole, NodeSpec, PrepareConfig,
    ProcessContext, Processor,
};
use faderframe_core::ChannelLayout::{Mono, Stereo};
use faderframe_midi::{MidiEvent, TimedMidiEvent};

type Ctx = ();

fn config(block: usize) -> PrepareConfig {
    PrepareConfig::new(48_000.0, block)
}

fn run(graph: &mut faderframe_audio_graph::CompiledGraph<Ctx>, frames: usize) {
    graph.process(&ProcessContext {
        frames,
        sample_rate: 48_000.0,
        data: &(),
    });
}

fn source(b: &mut GraphBuilder<Ctx>, label: &str, value: f32) -> NodeId {
    b.add_node(
        NodeSpec::new(label).audio_out(Mono),
        Box::new(Constant { value }),
    )
}

fn bus(b: &mut GraphBuilder<Ctx>, label: &str) -> NodeId {
    b.add_node(
        NodeSpec::new(label).audio_in(Stereo).audio_out(Stereo),
        Box::new(Passthrough),
    )
}

/// Track A, B, C → Drum Bus ┐
/// Track D ─────────────────┴→ Master → Output
#[test]
fn tracks_sum_through_bus_into_master() {
    let mut b = GraphBuilder::<Ctx>::new();
    let a = source(&mut b, "A", 0.1);
    let bb = source(&mut b, "B", 0.2);
    let c = source(&mut b, "C", 0.3);
    let d = source(&mut b, "D", 1.0);
    let drum_bus = bus(&mut b, "Drum Bus");
    let master = bus(&mut b, "Master");
    let out = b.add_node(
        NodeSpec::new("Out")
            .audio_in(Stereo)
            .role(NodeRole::DeviceOutput { first_channel: 0 }),
        Box::new(Passthrough),
    );
    for t in [a, bb, c] {
        b.connect_audio(t, 0, drum_bus, 0).unwrap();
    }
    b.connect_audio(drum_bus, 0, master, 0).unwrap();
    b.connect_audio(d, 0, master, 0).unwrap();
    b.connect_audio(master, 0, out, 0).unwrap();

    let mut g = b.compile(&config(64)).unwrap();
    run(&mut g, 32);

    let bus_out = g.audio_output(drum_bus, 0).unwrap();
    assert_eq!(bus_out.len(), 32);
    assert!((bus_out.channel(0)[0] - 0.6).abs() < 1e-6);
    assert!((bus_out.channel(1)[31] - 0.6).abs() < 1e-6);

    let mut seen = false;
    g.read_device_outputs(|first, buf| {
        assert_eq!(first, 0);
        assert!((buf.channel(0)[5] - 1.6).abs() < 1e-6);
        seen = true;
    });
    assert!(seen);

    // Scheduling information: A, B, C, D are independent; the bus waits on
    // three nodes, master on two.
    let stats = g.stats().clone();
    assert_eq!(stats.nodes, 7);
    assert_eq!(stats.levels, 4);
    assert_eq!(stats.max_width, 4);
    let bus_idx = g.index_of(drum_bus).unwrap();
    let master_idx = g.index_of(master).unwrap();
    assert_eq!(g.dependency_count(bus_idx), 3);
    assert_eq!(g.dependency_count(master_idx), 2);
    for t in [a, bb, c] {
        let i = g.index_of(t).unwrap();
        assert_eq!(g.dependency_count(i), 0);
        assert_eq!(g.dependents(i), &[bus_idx as u32]);
    }
}

#[test]
fn cycles_are_rejected_with_labels() {
    let mut b = GraphBuilder::<Ctx>::new();
    let x = bus(&mut b, "Bus X");
    let y = bus(&mut b, "Bus Y");
    let z = bus(&mut b, "Bus Z");
    b.connect_audio(x, 0, y, 0).unwrap();
    b.connect_audio(y, 0, z, 0).unwrap();
    b.connect_audio(z, 0, x, 0).unwrap();
    let err = b.compile(&config(64)).err().unwrap();
    match err {
        GraphError::Cycle(mut labels) => {
            labels.sort();
            assert_eq!(labels, vec!["Bus X", "Bus Y", "Bus Z"]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn invalid_ports_and_duplicates_are_rejected() {
    let mut b = GraphBuilder::<Ctx>::new();
    let s = source(&mut b, "S", 1.0);
    let x = bus(&mut b, "X");
    assert!(matches!(
        b.connect_audio(s, 1, x, 0),
        Err(GraphError::InvalidPort { .. })
    ));
    assert!(matches!(
        b.connect_events(s, 0, x, 0),
        Err(GraphError::InvalidPort { .. })
    ));
    b.connect_audio(s, 0, x, 0).unwrap();
    assert!(matches!(
        b.connect_audio(s, 0, x, 0),
        Err(GraphError::DuplicateEdge { .. })
    ));
}

/// The example from the architecture spec: latencies 0 / 256 / 2048 meeting
/// at one summing point need compensation 2048 / 1792 / 0.
#[test]
fn latency_compensation_aligns_summing_point() {
    let mut b = GraphBuilder::<Ctx>::new();
    let mut tracks = Vec::new();
    for (name, lat) in [("A", 0u32), ("B", 256), ("C", 2048)] {
        let src = source(&mut b, name, 0.0);
        let fx = b.add_node(
            NodeSpec::new(format!("{name} FX"))
                .audio_in(Mono)
                .audio_out(Mono),
            Box::new(LatencyProbe::new(lat)),
        );
        b.connect_audio(src, 0, fx, 0).unwrap();
        tracks.push(fx);
    }
    let master = bus(&mut b, "Master");
    for &t in &tracks {
        b.connect_audio(t, 0, master, 0).unwrap();
    }
    let g = b.compile(&config(64)).unwrap();
    let comp = g.compensation_into(master);
    let lookup = |id| comp.iter().find(|(n, _)| *n == id).unwrap().1;
    assert_eq!(lookup(tracks[0]), 2048);
    assert_eq!(lookup(tracks[1]), 1792);
    assert_eq!(lookup(tracks[2]), 0);
    assert_eq!(g.output_latency(master), Some(2048));
    assert_eq!(g.stats().max_compensation, 2048);
}

/// An impulse sent through paths with different latency arrives at the
/// summing point at the same sample after compensation.
#[test]
fn compensated_paths_are_sample_aligned() {
    struct Impulse {
        done: bool,
    }
    impl Processor<Ctx> for Impulse {
        fn process(&mut self, _cx: &ProcessContext<'_, Ctx>, io: &mut NodeIo<'_>) {
            let out = &mut io.audio_out[0];
            out.clear();
            if !self.done {
                out.channel_mut(0)[3] = 1.0;
                self.done = true;
            }
        }
    }

    let block = 16;
    let mut b = GraphBuilder::<Ctx>::new();
    let imp = b.add_node(
        NodeSpec::new("Impulse").audio_out(Mono),
        Box::new(Impulse { done: false }),
    );
    let short = b.add_node(
        NodeSpec::new("Short").audio_in(Mono).audio_out(Mono),
        Box::new(LatencyProbe::new(5)),
    );
    let long = b.add_node(
        NodeSpec::new("Long").audio_in(Mono).audio_out(Mono),
        Box::new(LatencyProbe::new(37)),
    );
    let sum = b.add_node(
        NodeSpec::new("Sum").audio_in(Mono).audio_out(Mono),
        Box::new(Passthrough),
    );
    b.connect_audio(imp, 0, short, 0).unwrap();
    b.connect_audio(imp, 0, long, 0).unwrap();
    b.connect_audio(short, 0, sum, 0).unwrap();
    b.connect_audio(long, 0, sum, 0).unwrap();
    let mut g = b.compile(&config(block)).unwrap();

    let mut out = Vec::new();
    for _ in 0..6 {
        run(&mut g, block);
        out.extend_from_slice(g.audio_output(sum, 0).unwrap().channel(0));
    }
    let hits: Vec<(usize, f32)> = out
        .iter()
        .enumerate()
        .filter(|(_, v)| **v != 0.0)
        .map(|(i, v)| (i, *v))
        .collect();
    assert_eq!(hits, vec![(3 + 37, 2.0)]);
}

#[test]
fn events_merge_in_time_order_across_edges() {
    struct Notes(u32, u8);
    impl Processor<Ctx> for Notes {
        fn process(&mut self, _cx: &ProcessContext<'_, Ctx>, io: &mut NodeIo<'_>) {
            let _ = io.events_out[0].push(TimedMidiEvent::new(
                self.0,
                MidiEvent::NoteOn {
                    channel: 0,
                    key: self.1,
                    velocity: 100,
                },
            ));
        }
    }
    let mut b = GraphBuilder::<Ctx>::new();
    let n1 = b.add_node(NodeSpec::new("N1").events_out(1), Box::new(Notes(9, 60)));
    let n2 = b.add_node(NodeSpec::new("N2").events_out(1), Box::new(Notes(2, 64)));
    let thru = b.add_node(
        NodeSpec::new("Thru").events_in(1).events_out(1),
        Box::new(Passthrough),
    );
    b.connect_events(n1, 0, thru, 0).unwrap();
    b.connect_events(n2, 0, thru, 0).unwrap();
    let mut g = b.compile(&config(32)).unwrap();
    run(&mut g, 32);
    let out = g.event_output(thru, 0).unwrap();
    let offsets: Vec<u32> = out.iter().map(|e| e.sample_offset).collect();
    assert_eq!(offsets, vec![2, 9]);
}

#[test]
fn processors_with_matching_keys_are_adopted() {
    struct Counter(u32);
    impl Processor<Ctx> for Counter {
        fn process(&mut self, _cx: &ProcessContext<'_, Ctx>, io: &mut NodeIo<'_>) {
            self.0 += 1;
            let v = self.0 as f32;
            io.audio_out[0].channel_mut(0).fill(v);
        }
    }
    let build = || {
        let mut b = GraphBuilder::<Ctx>::new();
        let n = b.add_node(
            NodeSpec::new("Counter").key(NodeKey(42)).audio_out(Mono),
            Box::new(Counter(0)),
        );
        let g = b.add_node(
            NodeSpec::new("Gain").audio_in(Mono).audio_out(Mono),
            Box::new(Gain { gain: 1.0 }),
        );
        b.connect_audio(n, 0, g, 0).unwrap();
        (b.compile(&config(8)).unwrap(), n)
    };
    let (mut old, _) = build();
    run(&mut old, 8);
    run(&mut old, 8);
    let (mut new, n) = build();
    assert_eq!(new.adopt_state_from(&mut old), 1);
    run(&mut new, 8);
    // The counter kept its state (third call) instead of restarting at 1.
    assert_eq!(new.audio_output(n, 0).unwrap().channel(0)[0], 3.0);
}

#[test]
fn timings_are_published_per_callback_for_nodes_and_groups() {
    /// Spends at least `us` microseconds per call.
    struct Busy {
        us: u64,
    }
    impl Processor<Ctx> for Busy {
        fn process(&mut self, _cx: &ProcessContext<'_, Ctx>, io: &mut NodeIo<'_>) {
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_micros(self.us) {
                std::hint::spin_loop();
            }
            for out in io.audio_out.iter_mut() {
                out.clear();
            }
        }
    }
    let mut b = GraphBuilder::<Ctx>::new();
    let a = b.add_node(
        NodeSpec::new("a").group(0).audio_out(Mono),
        Box::new(Busy { us: 200 }),
    );
    let c = b.add_node(
        NodeSpec::new("c").group(0).audio_in(Mono).audio_out(Mono),
        Box::new(Busy { us: 100 }),
    );
    let lone = b.add_node(
        NodeSpec::new("lone").audio_out(Mono),
        Box::new(Busy { us: 50 }),
    );
    b.connect_audio(a, 0, c, 0).unwrap();
    let mut g = b.compile(&config(64)).unwrap();
    let t = g.timings();
    assert_eq!(t.group_count(), 1);

    let (ia, ic, il) = (
        g.index_of(a).unwrap(),
        g.index_of(c).unwrap(),
        g.index_of(lone).unwrap(),
    );
    let i = |id| {
        if id == a {
            ia
        } else if id == c {
            ic
        } else {
            il
        }
    };
    // Two chunks of one callback count as one cycle.
    run(&mut g, 64);
    run(&mut g, 64);
    assert_eq!(
        t.total_ns(i(a)),
        0,
        "nothing is published before the cycle ends"
    );
    let budget_ns = 10_000_000; // 10 ms
    g.finish_cycle(budget_ns);

    let (na, nc, nl) = (t.total_ns(i(a)), t.total_ns(i(c)), t.total_ns(i(lone)));
    assert!(
        na >= 400_000 && nc >= 200_000 && nl >= 100_000,
        "{na} {nc} {nl}"
    );
    assert_eq!(t.group_of(i(a)), Some(0));
    assert_eq!(t.group_of(i(lone)), None);
    assert_eq!(
        t.group_total_ns(0),
        na + nc,
        "a group is the sum of its nodes"
    );
    // Peaks: share of the callback budget.
    let peak = t.take_peak(i(a));
    assert!((peak - na as f64 / budget_ns as f64).abs() < 1e-6, "{peak}");
    assert_eq!(t.take_peak(i(a)), 0.0, "taking resets the peak");
    let gp = t.take_group_peak(0);
    assert!(
        (gp - (na + nc) as f64 / budget_ns as f64).abs() < 1e-6,
        "{gp}"
    );
}

#[test]
fn optional_dependencies_are_checked_together_without_mutating_graph() {
    let mut b = GraphBuilder::<Ctx>::new();
    let a = bus(&mut b, "A");
    let c = bus(&mut b, "C");
    assert!(!b.would_cycle_with(&[(a, c)]).unwrap());
    assert!(b.would_cycle_with(&[(a, c), (c, a)]).unwrap());
    assert!(b.would_cycle_with(&[(NodeId(99), a)]).is_err());
    assert_eq!(b.edge_count(), 0);
    b.connect_audio(a, 0, c, 0).unwrap();
    assert!(b.would_cycle_with(&[(c, a)]).unwrap());
    assert_eq!(b.edge_count(), 1);
    b.compile(&config(64)).unwrap();
}

#[test]
fn audio_summing_order_does_not_depend_on_upstream_chain_depth() {
    let render = |deep: bool| {
        let mut b = GraphBuilder::<Ctx>::new();
        let a = if deep {
            bus(&mut b, "A")
        } else {
            source(&mut b, "A", 1e8)
        };
        let neg = source(&mut b, "B", -1e8);
        let small = source(&mut b, "C", 1.0);
        let sum = bus(&mut b, "Sum");
        for input in [a, neg, small] {
            b.connect_audio(input, 0, sum, 0).unwrap();
        }
        if deep {
            let upstream = source(&mut b, "Upstream", 1e8);
            b.connect_audio(upstream, 0, a, 0).unwrap();
        }
        let mut graph = b.compile(&config(64)).unwrap();
        run(&mut graph, 64);
        graph.audio_output(sum, 0).unwrap().channel(0).to_vec()
    };
    let shallow = render(false);
    assert!(shallow.iter().all(|&x| x == 1.0));
    assert_eq!(shallow, render(true));
}

/// A hardware insert: the chain into a send (device output), the return
/// (device input) declared `after` it with the round trip as latency, and
/// the dry signal meeting the return: the dry path is held back by the
/// round trip plus what came before the send, whatever the node order.
#[test]
fn a_return_counts_its_latency_from_its_send() {
    let mut b = GraphBuilder::<Ctx>::new();
    let src = source(&mut b, "Track", 0.0);
    let pre = b.add_node(
        NodeSpec::new("Plugin before")
            .audio_in(Mono)
            .audio_out(Mono),
        Box::new(LatencyProbe::new(100)),
    );
    let send = b.add_node(
        NodeSpec::new("Send")
            .audio_in(Mono)
            .role(NodeRole::DeviceOutput { first_channel: 2 }),
        Box::new(Passthrough),
    );
    let ret = b.add_node(
        NodeSpec::new("Return")
            .audio_out(Mono)
            .role(NodeRole::DeviceInput { first_channel: 2 })
            .after(send),
        Box::new(LatencyProbe::new(64)),
    );
    let mix = b.add_node(
        NodeSpec::new("Mix")
            .audio_in(Mono)
            .audio_in(Mono)
            .audio_out(Mono),
        Box::new(Passthrough),
    );
    b.connect_audio(src, 0, pre, 0).unwrap();
    b.connect_audio(pre, 0, send, 0).unwrap();
    b.connect_audio(pre, 0, mix, 0).unwrap();
    b.connect_audio(ret, 0, mix, 1).unwrap();
    let g = b.compile(&config(64)).unwrap();
    assert_eq!(
        g.output_latency(ret),
        Some(164),
        "the send's 100 and the round trip"
    );
    let comp = g.compensation_into(mix);
    let of = |id| comp.iter().find(|(n, _)| *n == id).unwrap().1;
    assert_eq!(of(pre), 64, "the dry path waits for the return");
    assert_eq!(of(ret), 0);
}

/// One impulse at frame `at` of the first block (then silence).
struct ImpulseAt {
    left: usize,
}

impl Processor<Ctx> for ImpulseAt {
    fn process(&mut self, _cx: &ProcessContext<'_, Ctx>, io: &mut NodeIo<'_>) {
        let out = &mut io.audio_out[0];
        out.clear();
        if self.left != usize::MAX && self.left < io.frames {
            out.channel_mut(0)[self.left] = 1.0;
            self.left = usize::MAX;
        } else if self.left != usize::MAX {
            self.left -= io.frames;
        }
    }
}

fn impulse_at(b: &mut GraphBuilder<Ctx>, at: usize) -> NodeId {
    b.add_node(
        NodeSpec::new("Impulse").audio_out(Mono),
        Box::new(ImpulseAt { left: at }),
    )
}

fn hits(out: &[f32]) -> Vec<(usize, f32)> {
    out.iter()
        .enumerate()
        .filter(|(_, v)| **v != 0.0)
        .map(|(i, v)| (i, *v))
        .collect()
}

/// Two hardware outputs (the master on 1/2, a direct out on 3/4) are heard
/// together: the one with less latency waits for the other.
#[test]
fn heard_outputs_are_aligned_with_each_other() {
    let block = 64;
    let mut b = GraphBuilder::<Ctx>::new();
    let imp = impulse_at(&mut b, 3);
    let fast = b.add_node(
        NodeSpec::new("Direct").audio_in(Mono).audio_out(Mono),
        Box::new(Passthrough),
    );
    let slow = b.add_node(
        NodeSpec::new("Linear EQ").audio_in(Mono).audio_out(Mono),
        Box::new(LatencyProbe::new(300)),
    );
    let out = |b: &mut GraphBuilder<Ctx>, ch: u16| {
        b.add_node(
            NodeSpec::new(format!("Out {ch}"))
                .audio_in(Mono)
                .role(NodeRole::DeviceOutput { first_channel: ch }),
            Box::new(Passthrough),
        )
    };
    let (o1, o2) = (out(&mut b, 0), out(&mut b, 2));
    b.connect_audio(imp, 0, fast, 0).unwrap();
    b.connect_audio(imp, 0, slow, 0).unwrap();
    b.connect_audio(fast, 0, o1, 0).unwrap();
    b.connect_audio(slow, 0, o2, 0).unwrap();
    let mut g = b.compile(&config(block)).unwrap();
    assert_eq!(g.stats().output_latency, 300);
    assert_eq!(g.input_latency(o1), Some(300));
    assert_eq!(g.input_latency(o2), Some(300));
    let (mut a, mut c) = (Vec::new(), Vec::new());
    for _ in 0..8 {
        run(&mut g, block);
        a.extend_from_slice(g.audio_input(o1, 0).unwrap().channel(0));
        c.extend_from_slice(g.audio_input(o2, 0).unwrap().channel(0));
    }
    assert_eq!(hits(&a), vec![(303, 1.0)]);
    assert_eq!(hits(&c), vec![(303, 1.0)]);
}

/// A hardware insert's send keeps its own timing (its return counts from
/// it); the outputs heard are aligned without it.
#[test]
fn a_hardware_send_is_not_held_back_for_the_outputs() {
    let mut b = GraphBuilder::<Ctx>::new();
    let src = source(&mut b, "Track", 0.0);
    let late = b.add_node(
        NodeSpec::new("Late").audio_in(Mono).audio_out(Mono),
        Box::new(LatencyProbe::new(1000)),
    );
    let send = b.add_node(
        NodeSpec::new("Send")
            .audio_in(Mono)
            .role(NodeRole::DeviceOutput { first_channel: 4 }),
        Box::new(Passthrough),
    );
    let ret = b.add_node(
        NodeSpec::new("Return")
            .audio_out(Mono)
            .role(NodeRole::DeviceInput { first_channel: 4 })
            .after(send),
        Box::new(LatencyProbe::new(64)),
    );
    let out = b.add_node(
        NodeSpec::new("Out")
            .audio_in(Mono)
            .audio_in(Mono)
            .role(NodeRole::DeviceOutput { first_channel: 0 }),
        Box::new(Passthrough),
    );
    b.connect_audio(src, 0, send, 0).unwrap();
    b.connect_audio(src, 0, late, 0).unwrap();
    b.connect_audio(late, 0, out, 0).unwrap();
    b.connect_audio(ret, 0, out, 1).unwrap();
    let g = b.compile(&config(64)).unwrap();
    assert_eq!(g.compensation_into(send), vec![(src, 0)]);
    assert_eq!(g.output_latency(ret), Some(64));
    assert_eq!(g.stats().output_latency, 1000);
}

/// What a node is told about the latency arriving at it: set when the
/// graph is compiled, and again when the processor (with its state) is
/// adopted into a graph where it sits later.
#[test]
fn a_node_knows_how_late_its_input_is() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    struct Told(Arc<AtomicU32>);
    impl Processor<Ctx> for Told {
        fn set_input_latency(&mut self, frames: u32) {
            self.0.store(frames, Ordering::Relaxed);
        }
        fn process(&mut self, _cx: &ProcessContext<'_, Ctx>, _io: &mut NodeIo<'_>) {}
    }
    let build = |latency: u32, told: Arc<AtomicU32>| {
        let mut b = GraphBuilder::<Ctx>::new();
        let src = source(&mut b, "Track", 0.0);
        let fx = b.add_node(
            NodeSpec::new("FX").audio_in(Mono).audio_out(Mono),
            Box::new(LatencyProbe::new(latency)),
        );
        let dry = source(&mut b, "Dry", 0.0);
        let strip = b.add_node(
            NodeSpec::new("Strip")
                .key(NodeKey(7))
                .audio_in(Mono)
                .audio_out(Mono),
            Box::new(Told(told)),
        );
        b.connect_audio(src, 0, fx, 0).unwrap();
        b.connect_audio(fx, 0, strip, 0).unwrap();
        b.connect_audio(dry, 0, strip, 0).unwrap();
        b.compile(&config(64)).unwrap()
    };
    let first = Arc::new(AtomicU32::new(u32::MAX));
    let mut old = build(123, first.clone());
    assert_eq!(first.load(Ordering::Relaxed), 123);
    let second = Arc::new(AtomicU32::new(u32::MAX));
    let mut new = build(6400, second.clone());
    assert_eq!(new.adopt_state_from(&mut old), 1);
    assert_eq!(
        first.load(Ordering::Relaxed),
        6400,
        "the adopted processor sits behind 6400 now"
    );
}

/// The alignment does not depend on how the host cuts the audio into
/// blocks: 6400 frames of latency against a dry path, blocks of 1 to the
/// maximum, ragged sizes too.
#[test]
fn compensation_is_the_same_for_every_block_size() {
    let render = |sizes: &[usize], max: usize| {
        let mut b = GraphBuilder::<Ctx>::new();
        let imp = impulse_at(&mut b, 1000);
        let eq = b.add_node(
            NodeSpec::new("Linear EQ").audio_in(Mono).audio_out(Mono),
            Box::new(LatencyProbe::new(6400)),
        );
        let gain = b.add_node(
            NodeSpec::new("Dry").audio_in(Mono).audio_out(Mono),
            Box::new(Gain { gain: 0.5 }),
        );
        let sum = b.add_node(
            NodeSpec::new("Sum").audio_in(Mono).audio_out(Mono),
            Box::new(Passthrough),
        );
        b.connect_audio(imp, 0, eq, 0).unwrap();
        b.connect_audio(imp, 0, gain, 0).unwrap();
        b.connect_audio(eq, 0, sum, 0).unwrap();
        b.connect_audio(gain, 0, sum, 0).unwrap();
        let mut g = b.compile(&config(max)).unwrap();
        let mut out = Vec::new();
        let mut k = 0;
        while out.len() < 9000 {
            let n = sizes[k % sizes.len()];
            k += 1;
            run(&mut g, n);
            out.extend_from_slice(g.audio_output(sum, 0).unwrap().channel(0));
        }
        out.truncate(9000);
        out
    };
    let want = render(&[64], 64);
    assert_eq!(hits(&want), vec![(7400, 1.5)]);
    for (sizes, max) in [
        (&[1usize][..], 1usize),
        (&[7], 7),
        (&[1024], 1024),
        (&[8192], 8192),
        (&[1, 31, 64, 5, 1000, 17], 1024),
    ] {
        assert_eq!(render(sizes, max), want, "blocks {sizes:?}");
    }
}

/// Low-latency monitoring through a bus: a live vocal (early) joins a bus
/// that also plays a track; a track elsewhere has 6400 frames of latency.
/// The vocal comes out at once — the bus's other audio is held back where
/// it joins, not the bus after — and both playback tracks still meet.
#[test]
fn early_audio_passes_every_summing_point_after_it() {
    let block = 64;
    let mut b = GraphBuilder::<Ctx>::new();
    let vocal = impulse_at(&mut b, 100);
    let keys = impulse_at(&mut b, 1000);
    let drums = impulse_at(&mut b, 1000);
    let late = b.add_node(
        NodeSpec::new("Linear EQ").audio_in(Mono).audio_out(Mono),
        Box::new(LatencyProbe::new(6400)),
    );
    let bus = b.add_node(
        NodeSpec::new("Bus").audio_in(Mono).audio_out(Mono),
        Box::new(Passthrough),
    );
    let master = b.add_node(
        NodeSpec::new("Master").audio_in(Mono).audio_out(Mono),
        Box::new(Passthrough),
    );
    let out = b.add_node(
        NodeSpec::new("Out")
            .audio_in(Mono)
            .role(NodeRole::DeviceOutput { first_channel: 0 }),
        Box::new(Passthrough),
    );
    b.connect_audio_early(vocal, 0, bus, 0).unwrap();
    b.connect_audio(keys, 0, bus, 0).unwrap();
    b.connect_audio(bus, 0, master, 0).unwrap();
    b.connect_audio(drums, 0, late, 0).unwrap();
    b.connect_audio(late, 0, master, 0).unwrap();
    b.connect_audio(master, 0, out, 0).unwrap();
    let mut g = b.compile(&config(block)).unwrap();
    assert_eq!(g.stats().output_latency, 6400);
    assert_eq!(
        g.compensation_into(master)
            .iter()
            .find(|(n, _)| *n == bus)
            .unwrap()
            .1,
        0,
        "the bus is not held back at the master"
    );
    let mut heard = Vec::new();
    for _ in 0..(8000 / block + 1) {
        run(&mut g, block);
        heard.extend_from_slice(g.audio_input(out, 0).unwrap().channel(0));
    }
    // The vocal at once; keys and drums together, 6400 late.
    assert_eq!(hits(&heard), vec![(100, 1.0), (7400, 2.0)]);
}

/// Counts its resets.
struct Resets(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Processor<Ctx> for Resets {
    fn process(&mut self, _cx: &ProcessContext<'_, Ctx>, _io: &mut NodeIo<'_>) {}

    fn reset(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A panic's resets go a share of a callback at a time: past the time
/// given, one node per call, each node once, in processing order.
#[test]
fn resets_can_be_spread_over_calls() {
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut b = GraphBuilder::<Ctx>::new();
    let nodes = 5;
    for i in 0..nodes {
        b.add_node(
            NodeSpec::new(format!("N{i}")).audio_out(Mono),
            Box::new(Resets(count.clone())),
        );
    }
    let mut g = b.compile(&config(64)).unwrap();
    let past = std::time::Instant::now();
    let mut from = 0;
    let mut calls = 0;
    loop {
        calls += 1;
        match g.reset_nodes(from, past) {
            Some(next) => {
                assert_eq!(next, from + 1, "one node past the time");
                from = next;
            }
            None => break,
        }
    }
    assert_eq!(calls, nodes);
    assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), nodes);
    // With time to spare, all at once.
    let later = std::time::Instant::now() + std::time::Duration::from_secs(10);
    assert_eq!(g.reset_nodes(0, later), None);
    assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), 2 * nodes);
}
