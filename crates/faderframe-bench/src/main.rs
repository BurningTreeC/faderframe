//! `faderframe-bench` — headless engine benchmark.
//!
//! Builds a session with N audio tracks (generated material), optional
//! buses, sends and plugin inserts, then drives the realtime processing
//! entry point exactly like an audio backend would and reports callback
//! timing percentiles against the deadline. Averages hide xruns; the
//! interesting numbers are p99, max and deadline misses.
//!
//! The graph runs on `--threads` threads (the audio thread plus a worker
//! pool, as in the application); `--fx N` adds N echo inserts per track and
//! `--plugin clap:<id>` / `vst3:<id>` / `lv2:<uri>` an installed plugin (from the
//! application's scan caches) to every track, to measure plugin load.
//!
//! `--pan mono|balance|dual` sets how the tracks are panned (mono tracks,
//! stereo balance — the default — or stereo dual pan) and
//! `--pan-automation` sweeps their pan lanes (steps and slopes), to compare
//! the panners' cost. `--bed 5.1|7.1|7.1.4` makes the master and the buses
//! that surround format (the tracks are panned into them by
//! `--panner room|vbap`: mono tracks as points, stereo ones (`balance`) as
//! images, `dual` each channel at its own direction) and `--headphones`
//! listens to the master binaurally. `--latency N` puts N frames of plugin
//! latency first on every other track (plugin delay compensation: the
//! others are delayed to meet them; automation behind it is read back
//! through the play history).
//!
//! Example: `cargo run -p faderframe-bench --release -- --tracks 128 --block 64 --rate 96000`

#![forbid(unsafe_code)]

use faderframe_audio::OwnedBuffers;
use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{ChannelLayout, TrackId, builtin};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::{
    AudioClip, AudioSource, AuxSend, Clip, ClipContent, ClipFades, OutputRouting, PluginFormat,
    PluginRef, PluginSlot, Project, SendTap, SourceSpec, StretchSettings, Track, TrackColor,
    TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::time::Instant;

struct Args {
    tracks: usize,
    crosstalk: bool,
    preamp: Option<usize>,
    /// Feed a mono tone into the stereo tracks, exercising duplicated input.
    mono_source: bool,
    block: usize,
    rate: u32,
    seconds: f64,
    buses: usize,
    inserts: bool,
    sends: bool,
    /// Per-node timing on, as in the live engine (performance meter).
    measure: bool,
    /// Processing threads including the audio thread.
    threads: usize,
    /// Echo inserts per track.
    fx: usize,
    /// A third-party plugin on every track.
    plugin: Option<PluginRef>,
    /// Wait for each callback's deadline like a sound card (idle time
    /// between callbacks: workers sleep and must be woken).
    paced: bool,
    /// Render tracks ahead by this many milliseconds (implies `paced`).
    ahead: Option<u64>,
    /// Render buses ahead too.
    ahead_buses: bool,
    /// How the tracks are panned.
    pan: PanBench,
    /// Pan lanes sweeping every track.
    pan_automation: bool,
    /// The master and buses are this bed.
    bed: Option<faderframe_core::SurroundFormat>,
    /// How tracks are placed into the bed.
    panner: faderframe_core::surround::SurroundPanner,
    /// The master heard binaurally.
    headphones: bool,
    /// Plugin latency first on every other track (frames; 0: none).
    latency: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum PanBench {
    /// Mono tracks: mono pan by the pan law.
    Mono,
    /// Stereo tracks balanced.
    Balance,
    /// Stereo tracks dual-panned.
    Dual,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        tracks: 64,
        crosstalk: false,
        preamp: None,
        mono_source: false,
        block: 128,
        rate: 48_000,
        seconds: 10.0,
        buses: 4,
        inserts: true,
        sends: true,
        measure: false,
        threads: 1 + faderframe_realtime::default_worker_count(),
        fx: 0,
        plugin: None,
        paced: false,
        ahead: None,
        ahead_buses: false,
        pan: PanBench::Balance,
        pan_automation: false,
        bed: None,
        panner: faderframe_core::surround::SurroundPanner::Room,
        headphones: false,
        latency: 0,
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
            "--crosstalk" => a.crosstalk = true,
            "--preamp" => {
                let model: usize = num("--preamp")?
                    .parse()
                    .map_err(|_| "invalid preamp index")?;
                if model >= builtin::PREAMPS.len() {
                    return Err("preamp index must be 0..5".into());
                }
                a.preamp = Some(model);
            }
            "--no-inserts" => a.inserts = false,
            "--mono-source" => a.mono_source = true,
            "--no-sends" => a.sends = false,
            "--measure-nodes" => a.measure = true,
            "--threads" => {
                a.threads = num("--threads")?
                    .parse()
                    .ok()
                    .filter(|&t| t >= 1)
                    .ok_or("bad --threads")?
            }
            "--fx" => a.fx = num("--fx")?.parse().map_err(|_| "bad --fx")?,
            "--paced" => a.paced = true,
            "--pan" => {
                a.pan = match num("--pan")?.as_str() {
                    "mono" => PanBench::Mono,
                    "balance" => PanBench::Balance,
                    "dual" => PanBench::Dual,
                    _ => return Err("--pan needs mono, balance or dual".into()),
                }
            }
            "--pan-automation" => a.pan_automation = true,
            "--bed" => {
                let v = num("--bed")?;
                a.bed = Some(
                    faderframe_core::SurroundFormat::ALL
                        .into_iter()
                        .find(|f| f.name() == v)
                        .ok_or("--bed needs a format like 5.1, 7.1 or 7.1.4")?,
                );
            }
            "--panner" => {
                a.panner = match num("--panner")?.as_str() {
                    "room" => faderframe_core::surround::SurroundPanner::Room,
                    "vbap" => faderframe_core::surround::SurroundPanner::Vbap,
                    _ => return Err("--panner needs room or vbap".into()),
                }
            }
            "--headphones" => a.headphones = true,
            "--latency" => a.latency = num("--latency")?.parse().map_err(|_| "bad --latency")?,
            "--ahead-buses" => a.ahead_buses = true,
            "--ahead" => {
                a.ahead = Some(num("--ahead")?.parse().map_err(|_| "bad --ahead")?);
                a.paced = true;
            }
            "--plugin" => {
                let v = num("--plugin")?;
                let (format, id) = v
                    .split_once(':')
                    .ok_or("--plugin needs clap:<id>, vst3:<id> or lv2:<uri>")?;
                let format = match format {
                    "clap" => PluginFormat::Clap,
                    "vst3" => PluginFormat::Vst3,
                    "lv2" => PluginFormat::Lv2,
                    _ => return Err("--plugin needs clap:<id>, vst3:<id> or lv2:<uri>".into()),
                };
                a.plugin = Some(PluginRef {
                    format,
                    id: id.into(),
                    name: id.into(),
                });
            }
            "-h" | "--help" => {
                println!(
                    "faderframe-bench [--tracks N] [--block FRAMES] [--rate HZ] [--seconds S] [--buses N] [--no-inserts] [--no-sends] [--measure-nodes] [--threads N] [--fx N] [--plugin clap:<id>|vst3:<id>|lv2:<uri>] [--paced] [--ahead MS] [--ahead-buses] [--crosstalk] [--preamp 0..5] [--mono-source] [--pan mono|balance|dual] [--pan-automation] [--bed 5.1|7.1|7.1.4] [--panner room|vbap] [--headphones] [--latency FRAMES]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    faderframe_audio::validate_format(a.rate, a.block as u32).map_err(|e| e.to_string())?;
    Ok(a)
}

/// The track panned as `--pan` asks, with sweeping lanes for
/// `--pan-automation` (a step and a slope every quarter).
fn pan_track(p: &mut Project, t: &mut Track, args: &Args, i: usize) {
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    use faderframe_core::SurroundParam as P;
    use faderframe_core::surround::{StereoSource, SurroundPanner};
    if args.bed.is_some() {
        let azimuth = 180.0 * ((i as f32 * 0.37).sin());
        t.surround = faderframe_core::SurroundPan {
            panner: args.panner,
            azimuth,
            x: (i as f32 * 0.37).sin(),
            stereo: match args.pan {
                PanBench::Dual => StereoSource::Dual,
                PanBench::Balance => StereoSource::Image,
                PanBench::Mono => StereoSource::Image,
            },
            left: azimuth + 40.0,
            right: azimuth - 40.0,
            ..faderframe_core::SurroundPan::default()
        }
        .clamped();
        if args.pan == PanBench::Mono {
            t.layout = ChannelLayout::Mono;
        }
    }
    let surround_targets: &[AutomationTarget] = match (args.pan, args.panner) {
        (PanBench::Dual, SurroundPanner::Vbap) => &[
            AutomationTarget::Surround(P::LeftAzimuth),
            AutomationTarget::Surround(P::RightAzimuth),
        ],
        (_, SurroundPanner::Vbap) => &[AutomationTarget::Surround(P::Azimuth)],
        (_, SurroundPanner::Room) => &[AutomationTarget::Surround(P::X)],
    };
    let targets: &[AutomationTarget] = match args.pan {
        _ if args.bed.is_some() => surround_targets,
        PanBench::Mono => {
            t.layout = ChannelLayout::Mono;
            &[AutomationTarget::TrackPan]
        }
        PanBench::Balance => &[AutomationTarget::TrackPan],
        PanBench::Dual => {
            t.pan_mode = faderframe_core::PanMode::Dual;
            let width = 0.3 + 0.7 * ((i as f32 * 0.53).cos() * 0.5 + 0.5);
            t.dual_pan = faderframe_core::DualPan {
                left: (t.pan - width).clamp(-1.0, 1.0),
                right: (t.pan + width).clamp(-1.0, 1.0),
            };
            &[
                AutomationTarget::DualPanLeft,
                AutomationTarget::DualPanRight,
            ]
        }
    };
    if !args.pan_automation {
        return;
    }
    let quarters = (args.seconds * 2.0).ceil() as usize + 1; // 120 BPM
    for (k, &target) in targets.iter().enumerate() {
        let flip = if k == 0 { 1.0 } else { -1.0 };
        let curve = AutomationCurve::from_points(
            (0..=quarters)
                .map(|q| AutomationPoint {
                    time: MusicalTime::from_quarters(q as f64),
                    value: flip
                        * (((q + i) as f64 * 0.9).sin())
                        * match target {
                            AutomationTarget::Surround(p) if p.wraps() => 180.0,
                            _ => 1.0,
                        },
                    shape: if q % 2 == 0 {
                        CurveShape::Step
                    } else {
                        CurveShape::Linear
                    },
                })
                .collect(),
        );
        t.automation.lanes.push(AutomationLane {
            id: p.ids.allocate(),
            target,
            curve,
            mode: AutomationMode::Read,
            visible: false,
        });
    }
}

fn build(args: &Args) -> Project {
    let mut p = Project::new("bench", args.rate);
    p.crosstalk = args.crosstalk;
    let bed = args.bed.map(ChannelLayout::Surround);
    if let Some(bed) = bed
        && let Some(master) = p.master_id()
        && let Some(m) = p.track_mut(master)
    {
        m.layout = bed;
    }
    let bars = ((args.seconds * 2.0 / 4.0).ceil() as u32).max(1); // 120 BPM
    let drum: faderframe_core::AudioSourceId = p.ids.allocate();
    p.sources.insert(
        drum,
        AudioSource {
            id: drum,
            name: "drums".into(),
            spec: SourceSpec::Generated {
                generator: if args.mono_source {
                    GeneratorSpec::Sine {
                        frequency: 220.0,
                        seconds: args.seconds,
                        amplitude: 0.125,
                    }
                } else {
                    GeneratorSpec::DrumLoop {
                        bpm: 120.0,
                        bars,
                        seed: 3,
                    }
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
        .with_layout(bed.unwrap_or(ChannelLayout::Stereo));
        if args.inserts {
            t.inserts.push(PluginSlot {
                id: p.ids.allocate(),
                plugin: PluginRef::builtin(builtin::GAIN, "Gain"),
                bypass: false,
                parameters: vec![],
                state: None,
                sidechain: None,
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
        sidechain: None,
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
        if let Some(model) = args.preamp {
            t.preamp = Some(PluginSlot {
                id: p.ids.allocate(),
                plugin: PluginRef::builtin(builtin::PREAMPS[model].0, builtin::PREAMPS[model].1),
                bypass: false,
                parameters: vec![],
                state: None,
                sidechain: None,
            });
        }
        t.pan = ((i as f32 * 0.37).sin()).clamp(-1.0, 1.0);
        pan_track(&mut p, &mut t, args, i);
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
        if args.latency > 0 && i % 2 == 0 {
            t.inserts.push(PluginSlot {
                id: p.ids.allocate(),
                plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
                bypass: false,
                parameters: vec![faderframe_project::SavedParameter {
                    id: faderframe_core::ParameterId(0),
                    value: f64::from(args.latency),
                }],
                state: None,
                sidechain: None,
            });
        }
        if args.inserts {
            t.inserts.push(PluginSlot {
                id: p.ids.allocate(),
                plugin: PluginRef::builtin(builtin::GAIN, "Gain"),
                bypass: false,
                parameters: vec![],
                state: None,
                sidechain: None,
            });
        }
        for _ in 0..args.fx {
            t.inserts.push(PluginSlot {
                id: p.ids.allocate(),
                plugin: PluginRef::builtin(builtin::ECHO, "Echo"),
                bypass: false,
                parameters: vec![],
                state: None,
                sidechain: None,
            });
        }
        if let Some(plugin) = &args.plugin {
            t.inserts.push(PluginSlot {
                id: p.ids.allocate(),
                plugin: plugin.clone(),
                bypass: false,
                parameters: vec![],
                state: None,
                sidechain: None,
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
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
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
    // Third-party plugins: the application's scan results.
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(faderframe_plugin_clap::ClapFactory::new()));
        r.add_factory(Box::new(faderframe_plugin_vst3::Vst3Factory::new()));
        #[cfg(target_os = "linux")]
        r.add_factory(Box::new(faderframe_plugin_lv2::Lv2Factory::new()));
        r
    });
    if let Some(cache) =
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache/faderframe"))
    {
        let load = |f: &str| {
            faderframe_plugin_host::scan::ScanCache::load(&cache.join(f))
                .plugins()
                .cloned()
                .collect::<Vec<_>>()
        };
        faderframe_plugin_clap::set_catalog(load("clap-scan.json"));
        faderframe_plugin_vst3::set_catalog(load("vst3-scan.json"));
        #[cfg(target_os = "linux")]
        faderframe_plugin_lv2::set_catalog(
            faderframe_plugin_lv2::scan::Cache::load(&cache.join("lv2-scan.json")).plugins(),
        );
    }
    let project = build(&args);
    let sources = render_generated_sources(&project, args.rate);
    let config = EngineConfig {
        sample_rate: args.rate,
        max_block_size: args.block.max(16),
        measure_nodes: args.measure,
        ..EngineConfig::default()
    };
    let outputs = args.bed.map_or(2, |f| f.channels());
    let mut r = match OfflineRenderer::new(&project, &sources, config, args.block, outputs) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("faderframe-bench: {e}");
            std::process::exit(1);
        }
    };
    if args.paced {
        // A simulated sound card uses the same asynchronous plugin mode as a
        // live device. Unpaced runs measure the complete synchronous DSP cost.
        r.controller.plugins().set_realtime(true);
        if let Err(e) = r.controller.rebuild_graph(&project) {
            eprintln!("faderframe-bench: {e}");
            std::process::exit(1);
        }
    }
    if args.threads > 1 {
        r.processor.set_worker_pool(Some(std::sync::Arc::new(
            faderframe_realtime::WorkerPool::new(faderframe_realtime::PoolConfig::new(
                args.threads - 1,
            )),
        )));
    }
    let failed = r.controller.failed_plugins();
    if !failed.is_empty() {
        eprintln!("faderframe-bench: plugins failed to load: {failed:?}");
    }
    if args.headphones {
        r.controller
            .set_binaural(Some(faderframe_binaural::Room::Mid));
        r.controller.relisten(&project);
    }
    let budget_ns = args.block as f64 * 1e9 / args.rate as f64;
    let mut bufs = OwnedBuffers::new(2, outputs, args.block);
    if let Some(ms) = args.ahead {
        // The anticipator gets the cores the audio thread's pool leaves.
        let helpers = faderframe_realtime::physical_cores()
            .saturating_sub(args.threads)
            .max(1);
        r.controller.set_render_ahead_buses(args.ahead_buses);
        r.controller
            .set_render_ahead(Some(std::time::Duration::from_millis(ms)), helpers);
        if let Err(e) = r
            .controller
            .sync(&project, &sources, faderframe_project::Impact::Graph)
        {
            eprintln!("faderframe-bench: {e}");
            std::process::exit(1);
        }
        // Let it start before playing.
        for _ in 0..(0.2 * args.rate as f64 / args.block as f64) as usize {
            r.processor.process_device(&mut bufs);
            std::thread::sleep(std::time::Duration::from_nanos(budget_ns as u64));
        }
    }
    let stats = r.controller.graph_stats().clone();
    let _ = r.play_from(0);
    let callbacks = (args.seconds * args.rate as f64 / args.block as f64) as usize;
    // Warm up caches and branch predictors.
    for _ in 0..64 {
        r.processor.process_device(&mut bufs);
        if args.paced {
            std::thread::sleep(std::time::Duration::from_nanos(budget_ns as u64));
        }
    }
    r.controller.reset_metrics();
    let mut times = Vec::with_capacity(callbacks);
    let wall = Instant::now();
    let period = std::time::Duration::from_nanos(budget_ns as u64);
    for i in 0..callbacks {
        if args.paced {
            // The next "interrupt" of the virtual sound card.
            let due = wall + period * i as u32;
            while Instant::now() < due {
                let left = due - Instant::now();
                if left > std::time::Duration::from_micros(200) {
                    std::thread::sleep(left - std::time::Duration::from_micros(100));
                } else {
                    std::hint::spin_loop();
                }
            }
        }
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
        "  config      : {} tracks, {} buses, inserts {}, sends {}, {} echo/track{}, {} Hz, block {} frames",
        args.tracks,
        args.buses,
        if args.inserts { "on" } else { "off" },
        if args.sends { "on" } else { "off" },
        args.fx,
        args.plugin.as_ref().map_or(String::new(), |p| format!(
            ", plugin {:?} {}",
            p.format, p.id
        )),
        args.rate,
        args.block
    );
    if let Some(model) = args.preamp {
        println!(
            "  preamp      : {} per track (2x oversampling, Gain 50%, Master 0 dB)",
            builtin::PREAMPS[model].1
        );
    }
    println!(
        "  source      : {}",
        if args.mono_source {
            "mono 220 Hz tone into stereo tracks"
        } else {
            "stereo drum loop"
        }
    );
    println!(
        "  graph       : {} nodes in {} jobs, {} edges, critical path {} nodes, parallel width {} jobs",
        stats.nodes, stats.jobs, stats.edges, stats.levels, stats.job_width
    );
    println!(
        "  threads     : {} (audio thread{})",
        args.threads,
        if args.threads > 1 {
            format!(" + {} workers", args.threads - 1)
        } else {
            String::new()
        }
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
    println!("  misses      : {misses} callbacks over the deadline");
    let m = r.controller.metrics();
    println!(
        "  engine view : {} callbacks, histogram p99 ≤ {:.1} µs, {} deadline misses, {} xruns (including DSP workers)",
        m.callbacks,
        m.p99_ns as f64 / 1000.0,
        m.deadline_misses,
        m.xruns
    );
    if let Some(ms) = args.ahead {
        println!(
            "  render ahead: {} ms, {} of {} tracks ({} strips), {} late blocks",
            ms,
            r.controller.ahead_tracks().len(),
            project.tracks.len(),
            r.controller.ahead_strips().len(),
            r.controller.ahead_misses()
        );
    }
}
