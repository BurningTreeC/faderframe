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
