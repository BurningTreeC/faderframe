//! `faderframe-bench` — headless engine benchmark.
//!
//! Builds a session with N audio tracks (generated material), optional
//! buses, sends and plugin inserts, then drives the realtime processing
//! entry point exactly like an audio backend would and reports callback
//! timing percentiles against the deadline. Averages hide xruns; the
//! interesting numbers are p99, max and deadline misses.
//!
//! Example: `cargo run -p faderframe-bench --release -- --tracks 128 --block 64 --rate 96000`

#![forbid(unsafe_code)]

use faderframe_audio::OwnedBuffers;
use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{ChannelLayout, TrackId, builtin};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::{
    AudioClip, AudioSource, AuxSend, Clip, ClipContent, ClipFades, OutputRouting, PluginRef,
    PluginSlot, Project, SendTap, SourceSpec, StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::time::Instant;

struct Args {
    tracks: usize,
    block: usize,
    rate: u32,
    seconds: f64,
    buses: usize,
    inserts: bool,
    sends: bool,
    /// Per-node timing on, as in the live engine (performance meter).
    measure: bool,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        tracks: 64,
        block: 128,
        rate: 48_000,
        seconds: 10.0,
        buses: 4,
        inserts: true,
        sends: true,
        measure: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut num = |name: &str| -> Result<String, String> {
            it.next().ok_or(format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--tracks" => a.tracks = num("--tracks")?.parse().map_err(|_| "bad --tracks")?,
            "--block" => a.block = num("--block")?.parse().map_err(|_| "bad --block")?,
            "--rate" => a.rate = num("--rate")?.parse().map_err(|_| "bad --rate")?,
            "--seconds" => a.seconds = num("--seconds")?.parse().map_err(|_| "bad --seconds")?,
            "--buses" => a.buses = num("--buses")?.parse().map_err(|_| "bad --buses")?,
            "--no-inserts" => a.inserts = false,
            "--no-sends" => a.sends = false,
            "--measure-nodes" => a.measure = true,
            "-h" | "--help" => {
                println!(
                    "faderframe-bench [--tracks N] [--block FRAMES] [--rate HZ] [--seconds S] [--buses N] [--no-inserts] [--no-sends] [--measure-nodes]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    faderframe_audio::validate_format(a.rate, a.block as u32).map_err(|e| e.to_string())?;
    Ok(a)
}

fn build(args: &Args) -> Project {
    let mut p = Project::new("bench", args.rate);
    let bars = ((args.seconds * 2.0 / 4.0).ceil() as u32).max(1); // 120 BPM
    let drum: faderframe_core::AudioSourceId = p.ids.allocate();
    p.sources.insert(
        drum,
        AudioSource {
            id: drum,
            name: "drums".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars,
                    seed: 3,
                },
            },
        },
    );
    let frames = p.sources[&drum].frames(args.rate);
    let mut buses = Vec::new();
    for b in 0..args.buses {
        let id: TrackId = p.ids.allocate();
        let mut t = Track::new(
            id,
            TrackKind::Bus,
            format!("Bus {b}"),
            TrackColor::palette(b),
        )
        .with_layout(ChannelLayout::Stereo);
        if args.inserts {
            t.inserts.push(PluginSlot {
                id: p.ids.allocate(),
                plugin: PluginRef::builtin(builtin::GAIN, "Gain"),
                bypass: false,
                parameters: vec![],
                state: None,
            });
        }
        buses.push(id);
        p.tracks.insert(0, t);
    }
    let aux: TrackId = p.ids.allocate();
    let mut aux_track = Track::new(aux, TrackKind::Aux, "Echo", TrackColor::palette(1))
        .with_layout(ChannelLayout::Stereo);
    aux_track.inserts.push(PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(builtin::ECHO, "Echo"),
        bypass: false,
        parameters: vec![],
        state: None,
    });
    p.tracks.insert(0, aux_track);
    for i in 0..args.tracks {
        let id: TrackId = p.ids.allocate();
        let mut t = Track::new(
            id,
            TrackKind::Audio,
            format!("T{i}"),
            TrackColor::palette(i),
        )
        .with_layout(ChannelLayout::Stereo);
        t.pan = ((i as f32 * 0.37).sin()).clamp(-1.0, 1.0);
        if let Some(bus) = buses.get(i % buses.len().max(1)) {
            t.output = OutputRouting::Track { track: *bus };
        }
        if args.sends {
            t.sends.push(AuxSend {
                id: p.ids.allocate(),
                target: aux,
                level_db: -18.0,
                tap: SendTap::PostFader,
                enabled: true,
            });
        }
        if args.inserts {
            t.inserts.push(PluginSlot {
                id: p.ids.allocate(),
                plugin: PluginRef::builtin(builtin::GAIN, "Gain"),
                bypass: false,
                parameters: vec![],
                state: None,
            });
        }
        let clip = Clip {
            id: p.ids.allocate(),
            track: id,
            name: "loop".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: drum,
                source_offset: 0,
                length: frames,
                gain_db: -12.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
            }),
        };
        t.clips.push(clip.id);
        p.clips.insert(clip.id, clip);
        let at = p.tracks.len() - 1;
        p.tracks.insert(at, t);
    }
    p
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("faderframe-bench: {e}");
            std::process::exit(2);
        }
    };
    let project = build(&args);
    let sources = render_generated_sources(&project, args.rate);
    let config = EngineConfig {
        sample_rate: args.rate,
        max_block_size: args.block.max(16),
        measure_nodes: args.measure,
        ..EngineConfig::default()
    };
    let mut r = match OfflineRenderer::new(&project, &sources, config, args.block, 2) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("faderframe-bench: {e}");
            std::process::exit(1);
        }
    };
    let stats = r.controller.graph_stats().clone();
    let _ = r.play_from(0);
    let mut bufs = OwnedBuffers::new(2, 2, args.block);
    let callbacks = (args.seconds * args.rate as f64 / args.block as f64) as usize;
    let budget_ns = args.block as f64 * 1e9 / args.rate as f64;
    // Warm up caches and branch predictors.
    for _ in 0..64 {
        r.processor.process_device(&mut bufs);
    }
    r.controller.reset_metrics();
    let mut times = Vec::with_capacity(callbacks);
    let wall = Instant::now();
    for _ in 0..callbacks {
        let t = Instant::now();
        r.processor.process_device(&mut bufs);
        times.push(t.elapsed().as_nanos() as u64);
    }
    let wall = wall.elapsed().as_secs_f64();
    r.controller.collect_garbage();
    times.sort_unstable();
    let pct =
        |p: f64| times[((times.len() as f64 * p) as usize).min(times.len() - 1)] as f64 / 1000.0;
    let misses = times.iter().filter(|&&t| t as f64 > budget_ns).count();
    let mean = times.iter().sum::<u64>() as f64 / times.len().max(1) as f64 / 1000.0;
    println!("FaderFrame engine benchmark");
    println!(
        "  config      : {} tracks, {} buses, inserts {}, sends {}, {} Hz, block {} frames",
        args.tracks,
        args.buses,
        if args.inserts { "on" } else { "off" },
        if args.sends { "on" } else { "off" },
        args.rate,
        args.block
    );
    println!(
        "  graph       : {} nodes, {} edges, critical path {} nodes, max parallel width {}",
        stats.nodes, stats.edges, stats.levels, stats.max_width
    );
    println!(
        "  callbacks   : {} ({:.1} s audio in {:.2} s wall, {:.1}× realtime)",
        times.len(),
        args.seconds,
        wall,
        args.seconds / wall
    );
    println!("  deadline    : {:.1} µs per callback", budget_ns / 1000.0);
    println!(
        "  callback µs : mean {:.1} · p50 {:.1} · p95 {:.1} · p99 {:.1} · p99.9 {:.1} · max {:.1}",
        mean,
        pct(0.50),
        pct(0.95),
        pct(0.99),
        pct(0.999),
        times.last().copied().unwrap_or(0) as f64 / 1000.0
    );
    println!(
        "  load        : mean {:.1}% · p99 {:.1}% of the deadline",
        mean * 1000.0 / budget_ns * 100.0,
        pct(0.99) * 1000.0 / budget_ns * 100.0
    );
    println!("  misses      : {misses} callbacks over the deadline (single-threaded engine)");
    let m = r.controller.metrics();
    println!(
        "  engine view : {} callbacks, histogram p99 ≤ {:.1} µs, {} deadline misses",
        m.callbacks,
        m.p99_ns as f64 / 1000.0,
        m.deadline_misses
    );
}
