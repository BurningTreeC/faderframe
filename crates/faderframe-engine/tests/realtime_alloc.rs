//! The realtime path must not allocate or free memory.
//!
//! A counting global allocator is armed (per thread) around calls to the
//! engine's processing entry point while the demo project plays — including
//! MIDI/synth voices, the echo plugin, a loop wrap, a stop/start and a graph
//! swap with state adoption.

mod common;

use faderframe_audio::OwnedBuffers;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::demo::demo_project;
use faderframe_project::{Command, History};
use faderframe_transport::TransportCommand;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Counting;

static EVENTS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    /// DSP worker threads of the test pool (counted while `WORKERS_ARMED`).
    static WORKER: Cell<bool> = const { Cell::new(false) };
}

static WORKERS_ARMED: AtomicBool = AtomicBool::new(false);

fn note() {
    let worker =
        WORKER.try_with(|w| w.get()).unwrap_or(false) && WORKERS_ARMED.load(Ordering::Relaxed);
    if worker || ARMED.try_with(|a| a.get()).unwrap_or(false) {
        EVENTS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: forwards to the system allocator; only adds counting.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The counter and the workers' flag are process-wide: the tests of this
/// file take turns, or one test would count another's allocations (a pool
/// shutting down while another test is armed).
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn armed<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = EVENTS.load(Ordering::Relaxed);
    ARMED.with(|a| a.set(true));
    WORKERS_ARMED.store(true, Ordering::SeqCst);
    let r = f();
    WORKERS_ARMED.store(false, Ordering::SeqCst);
    ARMED.with(|a| a.set(false));
    (r, EVENTS.load(Ordering::Relaxed) - before)
}

#[test]
fn counting_allocator_detects_allocations() {
    let _serial = serial();
    let (v, n) = armed(|| std::hint::black_box(Vec::<u64>::with_capacity(16)));
    drop(v);
    assert_eq!(n, 1);
}

#[test]
fn processing_does_not_allocate() {
    let _serial = serial();
    const SR: u32 = 48_000;
    const BLOCK: usize = 256;
    let mut project = demo_project(SR);
    project.crosstalk = true;
    // Every melody note glides, swells and pans: native note expressions
    // reach the built-in synth through the realtime path.
    for clip in project.clips.values_mut() {
        if let faderframe_project::ClipContent::Midi(m) = &mut clip.content {
            let point = |q: f64, value: f32| faderframe_project::ExpressionPoint {
                time: faderframe_timeline::MusicalTime::from_quarters(q),
                value,
            };
            m.expressions = m
                .notes
                .iter()
                .map(|n| {
                    let mut e = faderframe_project::NoteExpression::new(n.id);
                    e.pitch = vec![point(0.0, -1.0), point(0.25, 0.0)];
                    e.volume = vec![point(0.0, -12.0), point(0.5, 0.0)];
                    e.pan = vec![point(0.0, -0.5), point(0.5, 0.5)];
                    e
                })
                .collect();
            // SysEx for the instruments: bytes copied through the graph's
            // buffers, never allocated.
            for q in [0.1, 0.3, 0.7, 1.5] {
                m.sysex.push(faderframe_project::SysexEvent {
                    time: faderframe_timeline::MusicalTime::from_quarters(q),
                    data: vec![0xF0, 0x7D, 0x01, 0x02, 0xF7],
                });
            }
        }
    }
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);

    // Start one second before the loop end so a wrap happens while armed.
    let loop_end = project.loop_range.unwrap().end;
    let end = project.timeline.to_samples(loop_end, SR as f64);
    r.play_from(end - SR as i64).unwrap();
    // Warm-up (first-touch effects are not the subject of this test).
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }

    let (_, n) = armed(|| {
        for _ in 0..400 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(n, 0, "allocations/frees during playback incl. loop wrap");

    // Per-node timing (performance meter) on: still nothing allocates.
    r.controller.set_node_timing(true);
    let (_, n) = armed(|| {
        for _ in 0..100 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(n, 0, "allocations/frees with per-node timing");
    let timings = r.controller.node_timings().unwrap();
    assert!((0..timings.len()).any(|i| timings.total_ns(i) > 0));
    assert!(timings.group_count() > 0);

    // Stop/start and a graph swap queued from the control side.
    r.controller.transport(TransportCommand::Stop).unwrap();
    r.controller.transport(TransportCommand::Play).unwrap();
    let mut h = History::default();
    let bass = project.tracks.iter().find(|t| t.name == "Bass").unwrap().id;
    let impact = h
        .apply(
            &mut project,
            Command::SetTrackPhaseInvert {
                track: bass,
                on: true,
            },
        )
        .unwrap();
    r.controller.sync(&project, &sources, impact).unwrap();
    r.controller.rebuild_graph(&project).unwrap();
    let (_, n) = armed(|| {
        for _ in 0..50 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(
        n, 0,
        "allocations/frees while applying messages and swapping graphs"
    );
    assert!(
        r.controller.collect_garbage() >= 1,
        "old graph returned for dropping"
    );
    assert_eq!(r.controller.leaked_objects(), 0);
}

#[test]
fn parallel_processing_does_not_allocate() {
    let _serial = serial();
    use faderframe_realtime::{PoolConfig, WorkerPool};
    use std::sync::Arc;
    const SR: u32 = 48_000;
    const BLOCK: usize = 256;
    let project = demo_project(SR);
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        // The demo is light: use the workers anyway.
        parallel_min_ns: 0,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    let mut pool = PoolConfig::new(3);
    pool.on_start = Some(|| WORKER.with(|w| w.set(true)));
    r.processor
        .set_worker_pool(Some(Arc::new(WorkerPool::new(pool))));
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    r.play_from(0).unwrap();
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    r.controller.set_node_timing(true);
    let (_, n) = armed(|| {
        for _ in 0..400 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(n, 0, "allocations/frees on the audio or worker threads");
    assert!(bufs.output_ref(0).iter().any(|s| s.abs() > 1e-4), "audible");
}

#[test]
fn streamed_playback_does_not_allocate() {
    let _serial = serial();
    use faderframe_audio_files::{PAGE_FRAMES, WavFormat, write_wav};
    use faderframe_core::ChannelLayout;
    use faderframe_project::TrackKind;
    use faderframe_realtime::Reclaimer;
    use faderframe_timeline::MusicalTime;

    const SR: u32 = 48_000;
    let path = std::env::temp_dir().join(format!("ff-rt-stream-{}.wav", std::process::id()));
    let n = PAGE_FRAMES * 3;
    let data: Vec<f32> = (0..n).map(|i| (i as f32 * 0.01).sin() * 0.3).collect();
    write_wav(&path, &[data.clone(), data], SR, WavFormat::Float32, false).unwrap();
    let mut tp = common::TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let src = tp.stream(&path);
    tp.clip(t, src, MusicalTime::ZERO, n as i64);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    r.play_from(0).unwrap();
    let plan = r.controller.stream_plan();
    let shared = r.controller.shared();
    let mut rec = Reclaimer::default();
    plan.ensure(&[(0, n as i64)], &shared.epoch, &mut rec, &mut Vec::new())
        .unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    let (_, allocs) = armed(|| {
        for _ in 0..100 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations while reading streamed pages");
    assert!(
        bufs.output_ref(0).iter().any(|s| s.abs() > 0.01),
        "streamed audio audible"
    );
    assert_eq!(plan.sources()[0].misses(), 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn recording_and_metronome_do_not_allocate() {
    let _serial = serial();
    use faderframe_core::ChannelLayout;
    use faderframe_engine::{MetronomeMode, RecordTarget};
    use faderframe_project::TrackKind;
    use faderframe_transport::TransportCommand;

    const SR: u32 = 48_000;
    let mut tp = common::TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "In", ChannelLayout::Stereo);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    let mut streams = r
        .controller
        .begin_recording(
            vec![RecordTarget {
                track: t,
                first_channel: 0,
                channels: 2,
            }],
            0,
            i64::MAX,
            4.0,
        )
        .unwrap();
    r.controller.metronome().set_mode(MetronomeMode::Always);
    r.controller
        .transport(TransportCommand::SetRecording(true))
        .unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    let (_, allocs) = armed(|| {
        for _ in 0..200 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations while recording");
    assert!(streams.headers.pop().is_ok(), "blocks were captured");
}

#[test]
fn automation_does_not_allocate() {
    let _serial = serial();
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    use faderframe_core::{ChannelLayout, ParameterId, builtin};
    use faderframe_project::{PluginRef, PluginSlot, TrackKind};
    use faderframe_timeline::MusicalTime;

    const SR: u32 = 48_000;
    let mut tp = common::TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.3, 200_000);
    tp.clip(t, src, MusicalTime::ZERO, 200_000);
    let plugin = tp.project.ids.allocate();
    tp.project.track_mut(t).unwrap().inserts.push(PluginSlot {
        id: plugin,
        plugin: PluginRef::builtin(builtin::GAIN, "Gain"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    });
    let ramp = || {
        AutomationCurve::from_points(
            (0..20)
                .map(|i| AutomationPoint {
                    time: MusicalTime::from_quarters(i as f64 * 0.25),
                    value: -(i % 4) as f64 * 3.0,
                    shape: if i % 3 == 0 {
                        CurveShape::Step
                    } else {
                        CurveShape::Smooth
                    },
                })
                .collect(),
        )
    };
    for target in [
        AutomationTarget::TrackVolume,
        AutomationTarget::TrackPan,
        AutomationTarget::TrackMute,
        AutomationTarget::PluginParameter {
            plugin,
            parameter: ParameterId(0),
        },
        AutomationTarget::PluginBypass(plugin),
    ] {
        let id = tp.project.ids.allocate();
        tp.project
            .track_mut(t)
            .unwrap()
            .automation
            .lanes
            .push(AutomationLane {
                id,
                target,
                curve: ramp(),
                mode: AutomationMode::Read,
                visible: true,
            });
    }
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    let (_, allocs) = armed(|| {
        for _ in 0..300 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations while automating");
}

#[test]
fn modulation_does_not_allocate() {
    let _serial = serial();
    use faderframe_core::{ChannelLayout, ParameterId, builtin};
    use faderframe_project::modulation::{
        FollowSource, LfoShape, ModRate, ModRoute, ModSource, ModTarget, Modulator,
    };
    use faderframe_project::{PluginRef, PluginSlot, TrackKind};
    use faderframe_timeline::MusicalTime;

    const SR: u32 = 48_000;
    let mut tp = common::TestProject::new(SR);
    let key = tp.track(TrackKind::Audio, "Key", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.3, 200_000);
    tp.clip(key, src, MusicalTime::ZERO, 200_000);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    tp.clip(t, src, MusicalTime::ZERO, 200_000);
    let plugin = tp.project.ids.allocate();
    tp.project.track_mut(t).unwrap().inserts.push(PluginSlot {
        id: plugin,
        plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    });
    let param = |p: u32| ModTarget::Plugin {
        plugin,
        parameter: ParameterId(p),
    };
    let targets = [ModTarget::Volume, ModTarget::Pan, param(0), param(2)];
    let sources = [
        ModSource::Lfo {
            shape: LfoShape::Triangle,
            rate: ModRate::Sync { beats: 0.5 },
            phase: 0.25,
        },
        ModSource::Lfo {
            shape: LfoShape::Sine,
            rate: ModRate::Hz { hz: 3.0 },
            phase: 0.0,
        },
        ModSource::Follower {
            source: FollowSource::Track { track: key },
            attack_ms: 5.0,
            release_ms: 80.0,
            gain_db: 3.0,
        },
        ModSource::Follower {
            source: FollowSource::Input,
            attack_ms: 1.0,
            release_ms: 20.0,
            gain_db: 0.0,
        },
        ModSource::Steps {
            steps: vec![1.0, -1.0, 0.5, 0.0],
            rate: ModRate::Sync { beats: 0.25 },
            glide: 0.3,
        },
        ModSource::Random {
            rate: ModRate::Sync { beats: 0.25 },
            smooth: 0.5,
        },
        ModSource::Macro { value: 0.7 },
    ];
    let modulators: Vec<Modulator> = sources
        .into_iter()
        .enumerate()
        .map(|(i, source)| {
            let mut m = Modulator::new(tp.project.ids.allocate(), source);
            m.routes = targets
                .iter()
                .map(|&target| ModRoute {
                    target,
                    depth: 0.1 * (i as f32 - 3.0),
                })
                .collect();
            m
        })
        .collect();
    tp.project.track_mut(t).unwrap().modulators = modulators;
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    let (_, allocs) = armed(|| {
        for _ in 0..200 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations while modulating");
    // A new set (a macro turned, a modulator switched off) mid-play, then
    // stopped (the modulators run on).
    let tm = tp.project.track_mut(t).unwrap();
    tm.modulators[6].source = ModSource::Macro { value: 0.2 };
    tm.modulators[0].enabled = false;
    tm.modulators.swap(1, 2);
    r.controller
        .sync(&tp.project, &tp.sources, faderframe_project::Impact::Params)
        .unwrap();
    let (_, allocs) = armed(|| {
        for _ in 0..100 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations taking a new modulation set");
    r.controller.transport(TransportCommand::Stop).unwrap();
    for _ in 0..2 {
        r.processor.process_device(&mut bufs);
    }
    let (_, allocs) = armed(|| {
        for _ in 0..100 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations while stopped");
}

#[test]
fn note_modulation_does_not_allocate() {
    let _serial = serial();
    use faderframe_core::{ModulatorId, ParameterId, PluginInstanceId, TrackId};
    use faderframe_engine::modulation::{
        MAX_NOTE_MODS, MAX_PARAM_MODS, ModSpec, NoteVoices, RouteSpec, TrackModulation,
    };
    use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
    use faderframe_project::modulation::{
        LfoShape, ModRate, ModRoute, ModSource, ModTarget, Modulator,
    };

    // The voices of a plugin that takes per-note modulation: every source,
    // more notes than voices.
    let sources = [
        ModSource::Velocity,
        ModSource::Key,
        ModSource::NoteEnvelope {
            attack_ms: 5.0,
            decay_ms: 50.0,
            sustain: 0.5,
            release_ms: 100.0,
        },
        ModSource::NoteLfo {
            shape: LfoShape::Triangle,
            rate: ModRate::Hz { hz: 6.0 },
            phase: 0.0,
        },
        ModSource::NoteRandom,
    ];
    let plugin = PluginInstanceId(5);
    let tm = TrackModulation {
        track: TrackId(1),
        modulators: sources
            .iter()
            .enumerate()
            .map(|(i, source)| ModSpec {
                id: ModulatorId(i as u64),
                source: source.clone(),
                enabled: true,
            })
            .collect(),
        routes: (0..sources.len())
            .flat_map(|i| {
                [true, false].map(|per_note| RouteSpec {
                    modulator: i,
                    target: ModTarget::Plugin {
                        plugin,
                        parameter: ParameterId(i as u32 + if per_note { 0 } else { 10 }),
                    },
                    depth: 0.3,
                    range: 10.0,
                    per_note,
                })
            })
            .collect(),
        bus: Default::default(),
    };
    let mut voices = Box::<NoteVoices>::default();
    let mut midi = MidiBuffer::with_capacity(64);
    let mut mods = Vec::with_capacity(MAX_PARAM_MODS);
    let mut out = Vec::with_capacity(MAX_NOTE_MODS);
    let transport = faderframe_transport::TransportInfo {
        sample_rate: 48_000.0,
        tempo: 120.0,
        ..Default::default()
    };
    let (_, allocs) = armed(|| {
        for b in 0..400u32 {
            midi.clear();
            for k in 0..3u32 {
                let key = (36 + (b * 3 + k) % 60) as u8;
                let event = if b % 2 == 0 {
                    MidiEvent::NoteOn {
                        channel: 0,
                        key,
                        velocity: 90,
                    }
                } else {
                    MidiEvent::NoteOff {
                        channel: 0,
                        key: (36 + ((b - 1) * 3 + k) % 60) as u8,
                        velocity: 0,
                    }
                };
                let _ = midi.push(TimedMidiEvent::new(k * 40, event));
            }
            voices.process(
                &tm,
                plugin,
                Some(&midi),
                256,
                &transport,
                b % 97 == 0,
                &mut mods,
                &mut out,
            );
        }
    });
    assert_eq!(allocs, 0, "allocations following notes");
    assert!(!out.is_empty());

    // The demo's Lead Synth with note modulators (built-in: the newest
    // note's), playing its melody.
    const SR: u32 = 48_000;
    let mut project = demo_project(SR);
    let lead = project
        .tracks
        .iter()
        .position(|t| t.name == "Lead Synth")
        .unwrap();
    let synth = project.tracks[lead].inserts[0].id;
    let modulators: Vec<Modulator> = sources
        .iter()
        .enumerate()
        .map(|(i, source)| {
            let mut m = Modulator::new(project.ids.allocate(), source.clone());
            m.routes = vec![ModRoute {
                target: ModTarget::Plugin {
                    plugin: synth,
                    parameter: ParameterId(i as u32 + 1),
                },
                depth: 0.2,
            }];
            m
        })
        .collect();
    project.tracks[lead].modulators = modulators;
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, 256, 2).unwrap();
    r.play_from(i64::from(SR) * 8).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..8 {
        r.processor.process_device(&mut bufs);
    }
    let (_, allocs) = armed(|| {
        for _ in 0..400 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations playing note modulation");
}

#[test]
fn containers_do_not_allocate() {
    let _serial = serial();
    use faderframe_core::{ChannelLayout, builtin};
    use faderframe_project::container::Chain;
    use faderframe_project::{PluginRef, PluginSlot, TrackKind};
    use faderframe_timeline::MusicalTime;

    const SR: u32 = 48_000;
    let mut tp = common::TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.3, 200_000);
    tp.clip(t, src, MusicalTime::ZERO, 200_000);
    let mut slot = |id: &str| PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(id, id),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    };
    let (outer, inner) = (slot(builtin::CONTAINER), slot(builtin::CONTAINER));
    let (utility, reverb, gate) = (
        slot(builtin::GAIN),
        slot(builtin::REVERB),
        slot(builtin::GATE),
    );
    let track = tp.project.track_mut(t).unwrap();
    let mut a = Chain::new("A");
    a.inserts = vec![utility, inner.clone()];
    let mut b = Chain::new("B");
    b.inserts = vec![reverb];
    track
        .containers
        .insert(outer.id, vec![a, b, Chain::new("Dry")]);
    let mut c = Chain::new("C");
    c.inserts = vec![gate];
    track
        .containers
        .insert(inner.id, vec![c, Chain::new("Dry")]);
    track.inserts.push(outer.clone());
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    let (_, allocs) = armed(|| {
        for _ in 0..200 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations in containers");
    // A chain's level, mute and solo move (parameters, no new graph).
    let chains = tp
        .project
        .track_mut(t)
        .unwrap()
        .containers
        .get_mut(&outer.id)
        .unwrap();
    chains[0].gain_db = -12.0;
    chains[1].mute = true;
    chains[2].solo = true;
    r.controller
        .sync(&tp.project, &tp.sources, faderframe_project::Impact::Params)
        .unwrap();
    let (_, allocs) = armed(|| {
        for _ in 0..100 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations mixing chains");

    // Instruments in key ranges: the demo's lead split between two synths.
    let mut project = demo_project(SR);
    let lead = project
        .tracks
        .iter()
        .position(|t| t.name == "Lead Synth")
        .unwrap();
    let slot = |p: &mut faderframe_project::Project, id: &str| PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(id, id),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    };
    let split = slot(&mut project, builtin::CONTAINER);
    let (a, b) = (
        slot(&mut project, builtin::SYNTH),
        slot(&mut project, builtin::SYNTH),
    );
    let mut low = Chain::new("Low");
    low.key_high = 71;
    low.inserts.push(a);
    let mut high = Chain::new("High");
    high.key_low = 72;
    high.inserts.push(b);
    let t = &mut project.tracks[lead];
    t.containers.insert(split.id, vec![low, high]);
    t.inserts = vec![split];
    let sources = render_generated_sources(&project, SR);
    let mut r = OfflineRenderer::new(&project, &sources, config, 256, 2).unwrap();
    r.play_from(i64::from(SR) * 8).unwrap();
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    let (_, allocs) = armed(|| {
        for _ in 0..300 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations playing instruments in chains");
}

#[test]
fn live_midi_input_and_midi_recording_do_not_allocate() {
    let _serial = serial();
    use faderframe_engine::midi::{MidiFilter, MidiRecordTarget};
    use faderframe_project::Impact;
    use faderframe_transport::TransportCommand;
    use std::collections::HashSet;

    const SR: u32 = 48_000;
    let project = demo_project(SR);
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, 256, 2).unwrap();
    let synth = project
        .tracks
        .iter()
        .find(|t| t.kind == faderframe_project::TrackKind::Instrument)
        .unwrap()
        .id;
    let (tx, q, _feed) = faderframe_midi::midi_input_queue(256);
    let (oq, mut orx) = faderframe_midi::midi_output_queue(4096, tx.clock());
    r.controller.set_midi_input(q).unwrap();
    // MIDI clock out on port 0 while playing.
    r.controller.set_midi_output(oq).unwrap();
    r.controller
        .midi_shared()
        .clock_ports
        .store(1, std::sync::atomic::Ordering::Relaxed);
    r.controller.set_midi_live(HashSet::from([synth]));
    r.controller
        .sync(&project, &sources, Impact::Params)
        .unwrap();
    let mut rx = r
        .controller
        .begin_midi_recording(
            vec![MidiRecordTarget {
                track: synth,
                filter: MidiFilter {
                    port: None,
                    channel: None,
                },
            }],
            0,
            i64::MAX,
        )
        .unwrap();
    r.controller
        .transport(TransportCommand::SetRecording(true))
        .unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    let mut total = 0;
    for key in 48..72u8 {
        // Sent from this thread but outside the armed section (sending is
        // not realtime code).
        tx.send(0, &[0x90, key, 100]);
        tx.send(0, &[0xB0, 1, key]);
        // Live SysEx to the synth (it plays from every port).
        assert!(r.controller.send_live_sysex(0, &[0xF0, 0x7D, key, 0xF7]));
        let (_, n) = armed(|| {
            for _ in 0..3 {
                r.processor.process_device(&mut bufs);
            }
        });
        total += n;
        tx.send(0, &[0x80, key, 0]);
    }
    let (_, n) = armed(|| {
        for _ in 0..20 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(
        total + n,
        0,
        "allocations/frees with live MIDI and SysEx, recording and MIDI clock"
    );
    let clock = std::iter::from_fn(|| orx.pop().ok())
        .filter(|e| e.bytes() == [0xF8])
        .count();
    assert!(clock > 0, "clock went out");
    let recorded = std::iter::from_fn(|| rx.pop().ok()).count();
    assert!(recorded >= 24 * 3 - 1, "{recorded}");
}

#[test]
fn warped_playback_does_not_allocate() {
    let _serial = serial();
    use faderframe_project::{ClipContent, Warp, WarpAlgorithm, WarpMarker};
    const SR: u32 = 48_000;
    const BLOCK: usize = 256;
    // Warp the demo's clips: the drums stretched (polyphonic) with
    // markers, the plucks rhythmic, the bass varispeed.
    let mut project = demo_project(SR);
    let names = [
        ("Drums", WarpAlgorithm::Polyphonic),
        ("Pluck", WarpAlgorithm::Rhythmic),
        ("Bass", WarpAlgorithm::Varispeed),
    ];
    for (name, algorithm) in names {
        let t = project.tracks.iter().find(|t| t.name == name).unwrap();
        let id = t.clips[0];
        let Some(ClipContent::Audio(a)) = project.clips.get_mut(&id).map(|c| &mut c.content) else {
            panic!("audio clip");
        };
        let src = a.length;
        a.length = src * 5 / 4;
        a.warp = Some(Warp {
            source_length: src,
            markers: vec![WarpMarker {
                at: a.length / 3,
                source: a.source_offset + src / 2,
            }],
            algorithm,
        });
    }
    // Pitch edits (PSOLA voices): the pad's notes moved, and the warped
    // plucks' too, their formants as well.
    for name in ["Pad", "Pluck"] {
        use faderframe_project::pitch::{PitchEdit, PitchNote};
        let t = project.tracks.iter().find(|t| t.name == name).unwrap();
        let id = t.clips[0];
        let Some(ClipContent::Audio(a)) = project.clips.get_mut(&id).map(|c| &mut c.content) else {
            panic!("audio clip");
        };
        let (from, span) = (a.source_offset, a.source_span());
        let note = |start: i64, end: i64, pitch: f32, shift: f32, formant: f32| PitchNote {
            start,
            end,
            pitch,
            shift,
            drift: 0.5,
            formant,
            curve: vec![12; ((end - start) / 240) as usize],
        };
        a.pitch = Some(PitchEdit {
            hop: 240,
            notes: vec![
                note(from, from + span / 2, 57.0, 3.0, 2.0),
                note(from + span / 2, from + span, 45.0, -2.0, -12.0),
            ],
            keep_formants: name == "Pad",
        });
    }
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    r.play_from(SR as i64).unwrap();
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    // Playing, crossing warp markers, locating (re-priming the voices),
    // stopping and starting.
    let (_, n) = armed(|| {
        for i in 0..600 {
            if i == 200 {
                let _ = r
                    .controller
                    .transport(TransportCommand::Locate(SR as i64 * 7));
            }
            if i == 400 {
                let _ = r.controller.transport(TransportCommand::Stop);
                let _ = r.controller.transport(TransportCommand::Play);
            }
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(
        n, 0,
        "allocations/frees while playing warped or pitch-edited audio"
    );
}

#[test]
fn the_clip_launcher_does_not_allocate() {
    let _serial = serial();
    use faderframe_engine::launch::{LaunchCommand, Quantize};
    use faderframe_project::launcher::{Scene, SlotKey};
    use faderframe_project::{ClipContent, Warp, WarpAlgorithm};
    const SR: u32 = 48_000;
    const BLOCK: usize = 256;
    // Two scenes of copies of the demo's clips: audio (the second scene's
    // warped) and MIDI.
    let mut project = demo_project(SR);
    let scenes: Vec<faderframe_core::SceneId> = (0..2).map(|_| project.ids.allocate()).collect();
    for (i, id) in scenes.iter().enumerate() {
        project.launcher.scenes.push(Scene {
            id: *id,
            name: format!("Scene {i}"),
        });
    }
    let mut slots = Vec::new();
    let picks: Vec<(faderframe_core::TrackId, faderframe_core::ClipId)> = project
        .tracks
        .iter()
        .filter_map(|t| t.clips.first().map(|c| (t.id, *c)))
        .take(6)
        .collect();
    for (track, clip) in picks {
        for (k, scene) in scenes.iter().enumerate() {
            let mut c = project.clips[&clip].clone();
            c.id = project.ids.allocate();
            if let (1, ClipContent::Audio(a)) = (k, &mut c.content) {
                a.warp = Some(Warp {
                    source_length: a.length,
                    markers: Vec::new(),
                    algorithm: WarpAlgorithm::Polyphonic,
                });
                a.length = a.length * 3 / 2;
            }
            let key = SlotKey {
                track,
                scene: *scene,
            };
            project.launcher.slots.insert(key, c.id);
            project.clips.insert(c.id, c);
            slots.push((track, k, key.hash()));
            // Follow actions too (random ones on the first scene).
            use faderframe_project::launcher::{FollowAction, FollowKind};
            let kind = if k == 0 {
                FollowKind::Any
            } else {
                FollowKind::Previous
            };
            project.launcher.follow.insert(
                key,
                FollowAction {
                    kind,
                    bars: 1,
                    ..FollowAction::default()
                },
            );
        }
    }
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    r.play_from(0).unwrap();
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    let launch_as =
        |r: &mut OfflineRenderer, scene: usize, quantize: Quantize, legato: bool, repeat: i64| {
            for &(track, k, slot) in &slots {
                if k == scene {
                    let _ = r.controller.launch(LaunchCommand::Launch {
                        track,
                        slot,
                        quantize,
                        legato,
                        repeat,
                    });
                }
            }
        };
    let launch = |r: &mut OfflineRenderer, scene: usize, quantize: Quantize| {
        launch_as(r, scene, quantize, false, 0);
    };
    // Launching scenes (at once and on the beat), looping, stopping a
    // track and all, locating, stopping the transport, back to the
    // arrangement.
    let (_, n) = armed(|| {
        for i in 0..1_200 {
            match i {
                10 => launch(&mut r, 0, Quantize::None),
                200 => launch(&mut r, 1, Quantize::Beat),
                400 => {
                    let _ = r.controller.launch(LaunchCommand::Stop {
                        track: slots[0].0,
                        quantize: Quantize::Bars(1),
                    });
                }
                600 => {
                    let _ = r
                        .controller
                        .transport(TransportCommand::Locate(SR as i64 * 3));
                }
                700 => {
                    let _ = r.controller.launch(LaunchCommand::StopAll {
                        quantize: Quantize::Beat,
                    });
                }
                800 => launch(&mut r, 0, Quantize::Bars(1)),
                900 => {
                    let _ = r.controller.transport(TransportCommand::Stop);
                    let _ = r.controller.transport(TransportCommand::Play);
                    launch(&mut r, 1, Quantize::None);
                }
                // Legato into the other scene, repeating, then let go.
                950 => launch_as(&mut r, 0, Quantize::Beat, true, SR as i64 / 4),
                980 => {
                    for &(track, k, slot) in &slots {
                        if k == 0 {
                            let _ = r.controller.launch(LaunchCommand::Release {
                                track,
                                slot,
                                quantize: Quantize::Beat,
                            });
                        }
                    }
                }
                1_000 => {
                    let _ = r.controller.launch(LaunchCommand::BackToArrangement);
                }
                _ => {}
            }
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(n, 0, "allocations/frees while launching clips");
}

#[test]
fn varispeed_does_not_allocate() {
    let _serial = serial();
    const SR: u32 = 48_000;
    const BLOCK: usize = 256;
    let project = demo_project(SR);
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    r.controller.set_varispeed(true).unwrap();
    r.play_from(0).unwrap();
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    // Speeds up and down, a locate, stop and start.
    let (_, n) = armed(|| {
        for i in 0..800 {
            let speed = 1.0 + 0.015 * ((i as f64) * 0.05).sin();
            r.controller.set_speed(speed);
            if i == 300 {
                let _ = r
                    .controller
                    .transport(TransportCommand::Locate(SR as i64 * 3));
            }
            if i == 500 {
                let _ = r.controller.transport(TransportCommand::Stop);
                let _ = r.controller.transport(TransportCommand::Play);
            }
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(n, 0, "allocations/frees under varispeed");
}

#[test]
fn rendering_ahead_does_not_allocate_on_the_audio_thread() {
    let _serial = serial();
    render_ahead_without_allocating(false);
    // Buses too: strips rendered ahead, their meters, scope and automated
    // values echoed on the audio thread.
    render_ahead_without_allocating(true);
}

#[allow(clippy::unwrap_used)]
fn render_ahead_without_allocating(buses: bool) {
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    const SR: u32 = 48_000;
    const BLOCK: usize = 256;
    let mut project = demo_project(SR);
    // Something to render ahead on the audio tracks.
    for (i, name) in ["Drums", "Bass", "Pluck"].iter().enumerate() {
        let t = project.tracks.iter_mut().find(|t| t.name == *name).unwrap();
        t.inserts.push(faderframe_project::PluginSlot {
            id: faderframe_core::PluginInstanceId(9_100 + i as u64),
            plugin: faderframe_project::PluginRef::builtin(faderframe_core::builtin::ECHO, "Echo"),
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
        });
    }
    if buses {
        // The pad ducks under the drums: that would keep the drums live.
        let pad = project.tracks.iter_mut().find(|t| t.name == "Pad").unwrap();
        pad.modulators.clear();
    }
    // The drums' fader automated (what it shows comes from the echo).
    let lane = project.ids.allocate();
    let drums = project
        .tracks
        .iter_mut()
        .find(|t| t.name == "Drums")
        .unwrap();
    drums.automation.lanes.push(AutomationLane {
        id: lane,
        target: AutomationTarget::TrackVolume,
        curve: AutomationCurve::from_points(
            (0..16)
                .map(|i| AutomationPoint {
                    time: faderframe_timeline::MusicalTime::from_quarters(i as f64),
                    value: -(i % 4) as f64 * 2.0,
                    shape: CurveShape::Smooth,
                })
                .collect(),
        ),
        mode: AutomationMode::Read,
        visible: true,
    });
    let drums = drums.id;
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: BLOCK,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    r.controller.set_render_ahead_buses(buses);
    // A lookahead that survives the renderer thread being starved for a
    // while (the whole test suite runs beside it).
    r.controller
        .set_render_ahead(Some(std::time::Duration::from_millis(400)), 1);
    r.controller
        .sync(&project, &sources, faderframe_project::Impact::Graph)
        .unwrap();
    assert!(!r.controller.ahead_tracks().is_empty());
    assert_eq!(r.controller.ahead_strips().contains(&drums), buses);
    r.controller.scope().set_source(Some(drums.raw()));
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let pace = std::time::Duration::from_secs_f64(BLOCK as f64 / SR as f64);
    // Warm up (the link arrives, the first sequence starts).
    for _ in 0..30 {
        r.processor.process_device(&mut bufs);
        std::thread::sleep(pace);
    }
    let mut total = 0;
    for cmd in [
        TransportCommand::Play,
        TransportCommand::Locate(96_000),
        TransportCommand::Stop,
        TransportCommand::Play,
    ] {
        r.controller.transport(cmd).unwrap();
        for _ in 0..40 {
            let (_, n) = armed(|| r.processor.process_device(&mut bufs));
            total += n;
            std::thread::sleep(pace);
        }
        r.controller.collect_garbage();
    }
    let (misses, scope) = (r.controller.ahead_misses(), r.controller.scope().written());
    let facts = format!("buses {buses}: {total} allocations/frees, {misses} misses, scope {scope}");
    assert_eq!(total, 0, "allocations/frees on the audio thread ({facts})");
    assert_eq!(misses, 0, "render-ahead misses ({facts})");
    assert!(scope > 0, "the drums reached the scope ({facts})");
}

#[test]
fn the_equalisers_do_not_allocate() {
    let _serial = serial();
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    use faderframe_core::{ChannelLayout, ParameterId, builtin};
    use faderframe_plugin_host::eq::{Field, PhaseMode, band_id, global, global_id, listen_key};
    use faderframe_project::{PluginRef, PluginSlot, SavedParameter, TrackKind};
    use faderframe_timeline::MusicalTime;

    const SR: u32 = 48_000;
    let mut tp = common::TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.3, 200_000);
    tp.clip(t, src, MusicalTime::ZERO, 200_000);
    // A track keying the EQs' sidechains.
    let key_track = tp.track(TrackKind::Audio, "Key", ChannelLayout::Stereo);
    let key_src = tp.dc(2, 0.5, 200_000);
    tp.clip(key_track, key_src, MusicalTime::ZERO, 200_000);
    let set = |id: ParameterId, value: f64| SavedParameter { id, value };
    let band = |b: usize, f: Field, v: f64| set(band_id(b, f), v);
    // Every kind of band: dynamic (auto, custom, keyed by the sidechain,
    // freely triggered), spectral, mid/side, steep, fractional and
    // brickwall cuts, tilts, an all pass.
    let mut bands = Vec::new();
    for (b, kind, placement, range, slope) in [
        (0, 0.0, 0.0, -6.0, 12.0),
        (1, 1.0, 3.0, 0.0, 24.0),
        (2, 3.0, 0.0, 0.0, 96.0),
        (3, 7.0, 4.0, 4.0, 12.0),
        (4, 5.0, 1.0, 0.0, 36.0),
        (5, 4.0, 2.0, 0.0, 15.5),
        (6, 3.0, 0.0, 0.0, 100.0),
        (7, 8.0, 0.0, 3.0, 12.0),
        (8, 9.0, 0.0, 0.0, 24.0),
        (9, 0.0, 0.0, -9.0, 12.0),
        (10, 2.0, 3.0, -4.0, 12.0),
    ] {
        bands.extend([
            band(b, Field::Enabled, 1.0),
            band(b, Field::Type, kind),
            band(b, Field::Placement, placement),
            band(b, Field::Range, range),
            band(b, Field::Gain, 4.0),
            band(b, Field::Slope, slope),
        ]);
    }
    bands.extend([
        // Band 9 spectral, keyed by the sidechain.
        band(9, Field::Spectral, 1.0),
        band(9, Field::Dynamics, 1.0),
        band(9, Field::Key, 1.0),
        band(9, Field::Threshold, -30.0),
        // Band 10 custom, freely triggered from the sidechain.
        band(10, Field::Dynamics, 1.0),
        band(10, Field::Key, 1.0),
        band(10, Field::Trigger, 1.0),
        band(10, Field::TriggerLow, 300.0),
        band(10, Field::TriggerHigh, 4_000.0),
        band(10, Field::Attack, 0.2),
        // Band 0 custom with its own threshold.
        band(0, Field::Dynamics, 1.0),
        band(0, Field::Threshold, -24.0),
        set(global_id(global::AUTO_GAIN), 1.0),
        set(global_id(global::CHARACTER), 2.0),
        set(global_id(global::PAN), 0.3),
        set(global_id(global::PAN_MODE), 1.0),
        set(global_id(global::GAIN_Q), 1.0),
    ]);
    let eq = tp.project.ids.allocate();
    let program = tp.project.ids.allocate();
    let linear_eq = tp.project.ids.allocate();
    let natural_eq = tp.project.ids.allocate();
    // EQs in linear and natural phase: their kernels arrive from the
    // design thread while a band is automated.
    let mut linear = bands.clone();
    linear.push(set(global_id(global::PHASE), PhaseMode::Linear.value()));
    linear.push(set(global_id(global::QUALITY), 0.0));
    let mut natural = bands.clone();
    natural.push(set(global_id(global::PHASE), PhaseMode::Natural.value()));
    natural.push(set(global_id(global::CHARACTER), 1.0));
    let track = tp.project.track_mut(t).unwrap();
    for (id, parameters) in [(linear_eq, linear), (natural_eq, natural), (eq, bands)] {
        track.inserts.push(PluginSlot {
            id,
            plugin: PluginRef::builtin(builtin::EQ, "EQ"),
            bypass: false,
            parameters,
            state: None,
            sidechain: Some(key_track),
        });
    }
    track.inserts.push(PluginSlot {
        id: program,
        plugin: PluginRef::builtin(builtin::PROGRAM_EQ, "Program EQ"),
        bypass: false,
        parameters: vec![
            set(ParameterId(1), 6.0),
            set(ParameterId(2), 7.0),
            set(ParameterId(12), 3.0),
        ],
        state: None,
        sidechain: None,
    });
    let ramp = |lo: f64, hi: f64| {
        AutomationCurve::from_points(
            (0..20)
                .map(|i| AutomationPoint {
                    time: MusicalTime::from_quarters(i as f64 * 0.25),
                    value: if i % 2 == 0 { lo } else { hi },
                    shape: CurveShape::Smooth,
                })
                .collect(),
        )
    };
    for (target, curve) in [
        (
            AutomationTarget::PluginParameter {
                plugin: eq,
                parameter: band_id(0, Field::Freq),
            },
            ramp(200.0, 8_000.0),
        ),
        (
            AutomationTarget::PluginParameter {
                plugin: eq,
                parameter: band_id(1, Field::Gain),
            },
            ramp(-12.0, 12.0),
        ),
        (
            AutomationTarget::PluginParameter {
                plugin: program,
                parameter: ParameterId(5),
            },
            ramp(0.0, 10.0),
        ),
        (
            AutomationTarget::PluginParameter {
                plugin: linear_eq,
                parameter: band_id(1, Field::Gain),
            },
            ramp(-12.0, 12.0),
        ),
        (
            AutomationTarget::PluginParameter {
                plugin: natural_eq,
                parameter: band_id(3, Field::Freq),
            },
            ramp(300.0, 3_000.0),
        ),
        (
            AutomationTarget::PluginParameter {
                plugin: eq,
                parameter: band_id(5, Field::Slope),
            },
            ramp(9.0, 40.0),
        ),
        (
            AutomationTarget::PluginParameter {
                plugin: eq,
                parameter: global_id(global::BYPASS),
            },
            ramp(0.0, 1.0),
        ),
        (
            AutomationTarget::PluginParameter {
                plugin: eq,
                parameter: global_id(global::CHARACTER),
            },
            ramp(0.0, 2.0),
        ),
    ] {
        let id = tp.project.ids.allocate();
        tp.project
            .track_mut(t)
            .unwrap()
            .automation
            .lanes
            .push(AutomationLane {
                id,
                target,
                curve,
                mode: AutomationMode::Read,
                visible: true,
            });
    }
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    let tap = r.controller.plugin_tap(eq).expect("the EQ has a tap");
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..8 {
        tap.watch();
        r.processor.process_device(&mut bufs);
    }
    let (_, allocs) = armed(|| {
        for i in 0..400 {
            // An editor watching: the analyser rings fill; one band heard
            // on its own for a while.
            tap.watch();
            tap.set_listen(match i {
                100..200 => Some(1),
                200..260 => Some(listen_key(10)),
                _ => None,
            });
            r.processor.process_device(&mut bufs);
            // Give the linear phase design thread time to send kernels.
            if i % 40 == 0 {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    });
    assert_eq!(allocs, 0, "allocations in the equalisers");
    assert!(tap.output.written() > 0, "the analyser was fed");
    assert!(tap.sidechain.written() > 0, "the sidechain reached the EQ");
}

/// The stock devices (each with a few of its options on, one parameter
/// automated, an editor watching, keyed by a sidechain where it has one).
#[test]
fn the_stock_devices_do_not_allocate() {
    let _serial = serial();
    use faderframe_audio_files::AudioData;
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    use faderframe_core::{ChannelLayout, ParameterId, builtin};
    use faderframe_project::{PluginRef, PluginSlot, SavedParameter, TrackKind};
    use faderframe_timeline::MusicalTime;

    const SR: u32 = 48_000;
    let mut tp = common::TestProject::new(SR);
    // Bursts of a tone with a bright edge (something to compress, gate,
    // de-ess and limit).
    let frames = 200_000;
    let burst = |f: f64, amp: f64| -> Vec<f32> {
        (0..frames)
            .map(|n| {
                let t = n as f64 / f64::from(SR);
                let on = (n / 6_000) % 2 == 0;
                let x = amp * (std::f64::consts::TAU * f * t).sin()
                    + 0.3 * amp * (std::f64::consts::TAU * 7_000.0 * t).sin();
                if on { x as f32 } else { (x * 0.01) as f32 }
            })
            .collect()
    };
    let key_track = tp.track(TrackKind::Audio, "Key", ChannelLayout::Stereo);
    let key = tp.source(AudioData::from_channels(
        SR,
        vec![burst(60.0, 0.8), burst(60.0, 0.8)],
    ));
    tp.clip(key_track, key, MusicalTime::ZERO, frames as i64);
    let set = |id: u32, value: f64| SavedParameter {
        id: ParameterId(id),
        value,
    };
    // A device, its parameters, whether keyed, an automated parameter and
    // its range.
    type Device = (&'static str, Vec<SavedParameter>, bool, u32, (f64, f64));
    let devices: Vec<Device> = vec![
        // The 76: all four ratio buttons in, the fastest times, the
        // sidechain filter, half mix; its Input automated.
        (
            builtin::COMPRESSOR_76,
            vec![
                set(4, 15.0),
                set(2, 7.0),
                set(3, 7.0),
                set(8, 2.0),
                set(5, 0.5),
            ],
            false,
            0,
            (0.0, 30.0),
        ),
        (
            builtin::COMPRESSOR,
            vec![
                set(0, -30.0),
                set(6, 2.0),
                set(10, 2.0),
                set(18, 0.5),
                set(7, 1.0),
                set(8, 1.0),
            ],
            true,
            0,
            (-40.0, -10.0),
        ),
        (
            builtin::COMPRESSOR,
            vec![
                set(0, -20.0),
                set(6, 4.0),
                set(11, 1.0),
                set(13, 0.0),
                set(16, 1.0),
            ],
            false,
            1,
            (2.0, 20.0),
        ),
        (
            builtin::LIMITER,
            vec![set(0, 12.0), set(4, 1.0), set(6, 0.5)],
            false,
            1,
            (-6.0, -0.3),
        ),
        (
            builtin::LIMITER,
            vec![set(0, 6.0), set(5, 0.0), set(8, 1.0), set(7, 0.0)],
            false,
            2,
            (5.0, 500.0),
        ),
        (
            builtin::GATE,
            vec![set(0, -30.0), set(8, 3.0)],
            true,
            0,
            (-60.0, -10.0),
        ),
        (
            builtin::GATE,
            vec![set(2, 1.0), set(9, 0.0)],
            false,
            3,
            (1.5, 8.0),
        ),
        (
            builtin::GATE,
            vec![set(2, 2.0), set(10, 100.0), set(11, 2_000.0)],
            true,
            1,
            (-30.0, -3.0),
        ),
        // The channel strip: gate, compressor, bands, drive; then keyed
        // from the sidechain with the dynamics first and filters to the key.
        (
            builtin::CHANNEL_STRIP,
            vec![
                set(5, 1.0),
                set(6, -40.0),
                set(13, -30.0),
                set(14, 4.0),
                set(23, 4.0),
                set(29, -3.0),
                set(1, 80.0),
                set(37, 0.4),
            ],
            false,
            13,
            (-30.0, 0.0),
        ),
        (
            builtin::CHANNEL_STRIP,
            vec![
                set(21, 1.0),
                set(4, 1.0),
                set(36, 1.0),
                set(19, 1.0),
                set(35, 1.0),
                set(32, 6.0),
                set(34, 1.0),
                set(20, 0.5),
            ],
            true,
            30,
            (1_000.0, 5_000.0),
        ),
        (
            builtin::DEESSER,
            vec![set(0, -40.0), set(10, 2.0), set(8, 0.0)],
            false,
            2,
            (3_000.0, 9_000.0),
        ),
        (
            builtin::DEESSER,
            vec![set(3, 1.0), set(4, 1.0), set(5, 1.0)],
            false,
            0,
            (-50.0, -10.0),
        ),
        (
            builtin::SATURATOR,
            vec![
                set(1, 1.0),
                set(2, 0.4),
                set(3, 0.5),
                set(7, 3.0),
                set(8, 80.0),
                set(9, 9_000.0),
                set(4, 0.6),
            ],
            false,
            0,
            (0.0, 30.0),
        ),
        (
            builtin::SATURATOR,
            vec![set(1, 4.0), set(7, 0.0), set(6, 0.0)],
            false,
            1,
            (0.0, 5.0),
        ),
        (
            builtin::GAIN,
            vec![
                set(3, 1.0),
                set(5, 1.0),
                set(7, 3.0),
                set(8, 1.0),
                set(1, -0.3),
            ],
            false,
            2,
            (0.0, 2.0),
        ),
        (builtin::GAIN, vec![set(9, 0.0)], false, 3, (0.0, 1.0)),
        (
            builtin::ECHO,
            vec![
                set(9, 0.5),
                set(10, 0.6),
                set(12, 0.7),
                set(15, 1.0),
                set(8, 200.0),
                set(7, 0.2),
            ],
            false,
            0,
            (20.0, 800.0),
        ),
        (
            builtin::ECHO,
            vec![set(5, 1.0), set(4, 2.0), set(15, 2.0), set(14, 1.0)],
            false,
            1,
            (0.2, 1.05),
        ),
        (
            builtin::REVERB,
            vec![set(0, 2.0), set(15, 0.5), set(11, 200.0), set(12, 6_000.0)],
            false,
            1,
            (0.0, 1.0),
        ),
        (
            builtin::REVERB,
            vec![set(0, 4.0), set(14, 1.0)],
            false,
            2,
            (0.2, 8.0),
        ),
        (
            builtin::MODULATION,
            vec![set(0, 3.0), set(5, 0.6), set(8, 5.0)],
            false,
            4,
            (0.0, 1.0),
        ),
        (
            builtin::MODULATION,
            vec![set(0, 1.0), set(2, 1.0)],
            false,
            6,
            (2.0, 20.0),
        ),
        (
            builtin::MODULATION,
            vec![set(0, 2.0), set(5, -0.7), set(11, 5.0)],
            false,
            1,
            (0.1, 8.0),
        ),
        (builtin::TUNER, vec![set(1, 1.0)], false, 0, (430.0, 450.0)),
    ];
    let mut ids = Vec::new();
    for (i, (plugin, parameters, keyed, target, (lo, hi))) in devices.into_iter().enumerate() {
        let t = tp.track(TrackKind::Audio, &format!("T{i}"), ChannelLayout::Stereo);
        let src = tp.source(AudioData::from_channels(
            SR,
            vec![burst(220.0, 0.5), burst(330.0, 0.5)],
        ));
        tp.clip(t, src, MusicalTime::ZERO, frames as i64);
        let id = tp.project.ids.allocate();
        let lane = tp.project.ids.allocate();
        let track = tp.project.track_mut(t).unwrap();
        track.inserts.push(PluginSlot {
            id,
            plugin: PluginRef::builtin(plugin, plugin),
            bypass: false,
            parameters,
            state: None,
            sidechain: keyed.then_some(key_track),
        });
        track.automation.lanes.push(AutomationLane {
            id: lane,
            target: AutomationTarget::PluginParameter {
                plugin: id,
                parameter: ParameterId(target),
            },
            curve: AutomationCurve::from_points(
                (0..20)
                    .map(|i| AutomationPoint {
                        time: MusicalTime::from_quarters(i as f64 * 0.25),
                        value: if i % 2 == 0 { lo } else { hi },
                        shape: CurveShape::Smooth,
                    })
                    .collect(),
            ),
            mode: AutomationMode::Read,
            visible: true,
        });
        ids.push(id);
    }
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    let taps: Vec<_> = ids
        .iter()
        .map(|id| {
            r.controller
                .plugin_tap(*id)
                .expect("a stock device has a tap")
        })
        .collect();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..8 {
        taps.iter().for_each(|t| t.watch());
        r.processor.process_device(&mut bufs);
    }
    let (_, allocs) = armed(|| {
        for _ in 0..400 {
            taps.iter().for_each(|t| t.watch());
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations in the stock devices");
    for t in &taps {
        assert!(t.input.written() > 0, "an editor's rings were fed");
    }
}

/// The instruments (the synth with everything on, the sampler and the drum
/// sampler with samples), played live with editors watching.
#[test]
fn the_instruments_do_not_allocate() {
    let _serial = serial();
    use faderframe_core::{ChannelLayout, ParameterId, builtin};
    use faderframe_plugin_host::ParamValues;
    use faderframe_plugin_host::devices::samples::{SampleDoc, pack};
    use faderframe_project::{Impact, PluginRef, PluginSlot, SavedParameter, TrackKind};
    use std::collections::HashSet;

    const SR: u32 = 48_000;
    let dir = std::env::temp_dir().join(format!("ff-alloc-samples-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = Vec::new();
    for (i, f) in [110.0f64, 220.0, 330.0, 4_000.0].iter().enumerate() {
        let x: Vec<f32> = (0..SR as usize / 2)
            .map(|n| (0.4 * (std::f64::consts::TAU * f * n as f64 / f64::from(SR)).sin()) as f32)
            .collect();
        let path = dir.join(format!("{i}.wav"));
        faderframe_audio_files::write_wav(
            &path,
            &[x],
            SR,
            faderframe_audio_files::WavFormat::Pcm16,
            false,
        )
        .unwrap();
        files.push(path.to_string_lossy().into_owned());
    }
    let state = |id: &str, set: &[(u32, f64)], doc: &SampleDoc| {
        let params = ParamValues::new(
            faderframe_plugin_host::builtin::BuiltinFactory
                .instantiate(id)
                .unwrap()
                .parameters()
                .to_vec(),
        );
        for (k, v) in set {
            params.set_by_id(ParameterId(*k), *v).unwrap();
        }
        faderframe_engine::encode_state(&pack(&params.save(), doc))
    };
    let set = |id: u32, value: f64| SavedParameter {
        id: ParameterId(id),
        value,
    };
    let mut tp = common::TestProject::new(SR);
    let mut sampler_doc = SampleDoc::default();
    sampler_doc.set(0, Some(files[1].clone()));
    // An SFZ with every kind of opcode the sampler plays.
    let sfz = format!(
        "<control>set_cc1=64 default_path={dir}/\n<curve>curve_index=7 v000=0 v127=1\n\
         <global>ampeg_attack=0.002 ampeg_hold=0.01 ampeg_decay=0.2 ampeg_sustain=60 ampeg_release=0.05 \
         amp_velcurve_100=0.8 sw_lokey=24 sw_hikey=25 sw_default=24\n\
         <group>lokey=40 hikey=70 pitch_keycenter=57 sw_last=24 fil_type=lpf_4p cutoff=800 resonance=6 \
         cutoff_oncc74=2400 cutoff_curvecc74=7 fil_veltrack=1200 fileg_depth=1200 fileg_decay=0.3 \
         fil2_type=hsh cutoff2=4000 fil2_gain=-6 eq1_gain=3 eq2_gain=-4 eq3_bw=2 eq3_gain=2 \
         pitchlfo_freq=5 pitchlfo_depth=20 pitchlfo_depthcc1=30 amplfo_freq=3 amplfo_depth=2 \
         fillfo_freq=1 fillfo_depth=600 pitcheg_depth=50 pitcheg_decay=0.1 volume_oncc7=-6 pan_oncc10=50 \
         width=60 position=-20 xfin_locc11=0 xfin_hicc11=127 bend_up=1200 offset_random=200 \
         note_polyphony=2 polyphony=12 cutoff_chanaft=600 pitch_veltrack=10 amp_random=1 fil_random=50\n\
         <region>sample=1.wav loop_mode=loop_continuous loop_start=1000 loop_end=20000 loop_crossfade=0.01\n\
         <region>sample=2.wav sw_last=25 direction=reverse delay=0.01 count=2\n\
         <region>sample=0.wav lokey=40 hikey=70 trigger=release rt_decay=6\n\
         <region>sample=*saw key=30 on_locc20=64 on_hicc20=127\n\
         <region>sample=*sine lokey=40 hikey=70 trigger=legato fil_type=bpf_2p cutoff=1000\n\
         <region>sample=*noise lokey=71 hikey=80 fil_type=hpf_6p cutoff=3000 group=2 off_by=2 off_mode=normal",
        dir = dir.display()
    );
    let sfz_path = dir.join("all.sfz");
    std::fs::write(&sfz_path, sfz).unwrap();
    let mut sfz_doc = SampleDoc::default();
    sfz_doc.set(0, Some(sfz_path.to_string_lossy().into_owned()));
    let mut drum_doc = SampleDoc::default();
    for (pad, f) in files.iter().enumerate() {
        drum_doc.set(pad, Some(f.clone()));
    }
    use faderframe_plugin_host::PluginFactory;
    let instruments = [
        (
            builtin::SYNTH,
            None,
            vec![
                set(19, 7.0),
                set(17, 0.5),
                set(18, 0.2),
                set(22, 1.0),
                set(23, 0.5),
                set(26, 1.0),
                set(36, 2.0),
                set(38, 0.3),
                set(9, 1.0),
                set(39, 0.0),
                set(40, 50.0),
            ],
        ),
        (
            builtin::SAMPLER,
            Some(state(
                builtin::SAMPLER,
                &[
                    (13, 1.0),
                    (14, 0.2),
                    (15, 0.8),
                    (9, 1.0),
                    (7, 2_000.0),
                    (10, 0.5),
                ],
                &sampler_doc,
            )),
            vec![],
        ),
        (
            builtin::DRUMS,
            Some(state(
                builtin::DRUMS,
                &[
                    (1, 48.0),
                    (106, 1.0),
                    (122, 1.0),
                    (109, 3_000.0),
                    (107, 1.0),
                ],
                &drum_doc,
            )),
            vec![],
        ),
        (
            builtin::SAMPLER,
            Some(state(builtin::SAMPLER, &[(9, 1.0), (7, 6_000.0)], &sfz_doc)),
            vec![],
        ),
        // Keep Length: pitch by stretchers, not speed.
        (
            builtin::SAMPLER,
            Some(state(
                builtin::SAMPLER,
                &[(22, 1.0), (13, 1.0), (14, 0.2), (15, 0.8)],
                &sampler_doc,
            )),
            vec![],
        ),
        (
            builtin::DRUMS,
            Some(state(
                builtin::DRUMS,
                // Pads 1 and 2 on extra outputs (tracks take them below).
                &[
                    (1, 48.0),
                    (102, 7.0),
                    (112, 1.0),
                    (118, -5.0),
                    (128, 1.0),
                    (113, 2.0),
                    (129, 1.0),
                ],
                &drum_doc,
            )),
            vec![],
        ),
    ];
    let mut tracks = HashSet::new();
    let mut ids = Vec::new();
    for (i, (plugin, state, parameters)) in instruments.into_iter().enumerate() {
        let t = tp.track(
            TrackKind::Instrument,
            &format!("I{i}"),
            ChannelLayout::Stereo,
        );
        let id = tp.project.ids.allocate();
        tp.project.track_mut(t).unwrap().instrument = Some(PluginSlot {
            id,
            plugin: PluginRef::builtin(plugin, plugin),
            bypass: false,
            parameters,
            state,
            sidechain: None,
        });
        tracks.insert(t);
        ids.push(id);
    }
    // The last kit's extra outputs on tracks of their own.
    for bus in [1u16, 2] {
        let t = tp.track(TrackKind::Aux, &format!("Out {bus}"), ChannelLayout::Stereo);
        tp.project.track_mut(t).unwrap().input = faderframe_project::InputRouting::Plugin {
            plugin: ids[5],
            bus,
        };
    }
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    let taps: Vec<_> = ids
        .iter()
        .map(|id| {
            r.controller
                .plugin_tap(*id)
                .expect("an instrument with a tap")
        })
        .collect();
    let (tx, q, _feed) = faderframe_midi::midi_input_queue(256);
    r.controller.set_midi_input(q).unwrap();
    r.controller.set_midi_live(tracks);
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..4 {
        taps.iter().for_each(|t| t.watch());
        r.processor.process_device(&mut bufs);
    }
    let mut total = 0;
    let mut sfz_zones = 0.0f32;
    for key in 44..80u8 {
        // Controllers, bend, aftertouch, the pedal and a keyswitch move
        // between the notes (the SFZ instrument follows them all).
        tx.send(0, &[0xB0, 74, key]);
        tx.send(0, &[0xB0, 20, if key % 4 == 0 { 100 } else { 0 }]);
        tx.send(0, &[0xB0, 64, if key % 5 == 0 { 127 } else { 0 }]);
        tx.send(0, &[0xE0, 0, key]);
        tx.send(0, &[0xD0, key]);
        tx.send(0, &[0xA0, key, 64]);
        if key % 7 == 0 {
            tx.send(0, &[0x90, 24 + key % 2, 100]);
        }
        tx.send(0, &[0x90, key, 40 + key]);
        let (_, n) = armed(|| {
            for _ in 0..4 {
                taps.iter().for_each(|t| t.watch());
                r.processor.process_device(&mut bufs);
            }
        });
        total += n;
        sfz_zones =
            sfz_zones.max(taps[3].value(faderframe_plugin_host::devices::sampler::value::ZONE));
        if key % 3 != 0 {
            tx.send(0, &[0x80, key, 0]);
        }
    }
    assert!(sfz_zones >= 1.0, "the SFZ's regions played");
    let (_, n) = armed(|| {
        for _ in 0..40 {
            taps.iter().for_each(|t| t.watch());
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(total + n, 0, "allocations in the instruments");
    for t in &taps {
        assert!(t.output.written() > 0, "an editor's rings were fed");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Album playback: taking the file, playing, locating, pausing and giving
/// it back allocate nothing on the audio thread.
#[test]
fn album_playback_does_not_allocate() {
    let _serial = serial();
    use faderframe_audio_files::{PAGE_FRAMES, StreamSource, WavFormat, write_wav};
    use faderframe_realtime::{Epoch, Reclaimer};
    const SR: u32 = 48_000;
    let n = PAGE_FRAMES * 3;
    let x: Vec<f32> = (0..n).map(|i| (i as f32 * 0.01).sin() * 0.3).collect();
    let dir = std::env::temp_dir().join(format!("ff-alloc-preview-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("album.wav");
    write_wav(&path, &[x.clone(), x], SR, WavFormat::Float32, false).unwrap();
    let src = StreamSource::open(&path).unwrap();
    src.ensure(
        src.page_range(0, n as i64),
        &Epoch::new(),
        &mut Reclaimer::default(),
        &mut Vec::new(),
    )
    .unwrap();
    let project = demo_project(SR);
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, 256, 2).unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..8 {
        r.processor.process_device(&mut bufs);
    }
    r.controller.set_preview(Some(src)).unwrap();
    r.controller.preview().play(true);
    let (_, allocs) = armed(|| {
        for i in 0..200 {
            if i == 50 {
                r.controller.preview().locate(PAGE_FRAMES as i64 - 10);
            }
            if i == 80 {
                r.controller.preview().play(false);
            }
            if i == 90 {
                r.controller.preview().play(true);
            }
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations in album playback");
    r.controller.set_preview(None).unwrap();
    let (_, allocs) = armed(|| {
        for _ in 0..4 {
            r.processor.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "handing the file back allocates nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn microphone_preamps_do_not_allocate_while_automating_or_resetting() {
    let _serial = serial();
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    use faderframe_core::{ChannelLayout, ParameterId, builtin};
    use faderframe_project::{PluginRef, PluginSlot, TrackKind};
    use faderframe_timeline::MusicalTime;
    for (name, label, _) in builtin::PREAMPS {
        let mut tp = common::TestProject::new(48_000);
        let t = tp.track(TrackKind::Audio, label, ChannelLayout::Stereo);
        // Wake the dormant channel after gain automation, then return to
        // equal input. Copying solver/FIR history must also be allocation-free.
        let mut right = vec![0.01; 8192];
        right[2048..2560].fill(-0.01);
        let src = tp.source(faderframe_audio_files::AudioData::from_channels(
            48_000,
            vec![vec![0.01; 8192], right],
        ));
        tp.clip(t, src, MusicalTime::ZERO, 8192);
        let plugin = tp.project.ids.allocate();
        let lane = tp.project.ids.allocate();
        let track = tp.project.track_mut(t).unwrap();
        track.preamp = Some(PluginSlot {
            id: plugin,
            plugin: PluginRef::builtin(name, label),
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
        });
        track.automation.lanes.push(AutomationLane {
            id: lane,
            target: AutomationTarget::PluginParameter {
                plugin,
                parameter: ParameterId(0),
            },
            curve: AutomationCurve::from_points(vec![
                AutomationPoint {
                    time: MusicalTime::ZERO,
                    value: 0.2,
                    shape: CurveShape::Step,
                },
                AutomationPoint {
                    time: MusicalTime::from_quarters(0.04),
                    value: 0.8,
                    shape: CurveShape::Step,
                },
                AutomationPoint {
                    time: MusicalTime::from_quarters(0.08),
                    value: 0.4,
                    shape: CurveShape::Step,
                },
            ]),
            mode: AutomationMode::Read,
            visible: true,
        });
        for realtime in [false, true] {
            let mut r =
                OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 128, 2)
                    .unwrap();
            r.controller.plugins().set_realtime(realtime);
            r.controller.rebuild_graph(&tp.project).unwrap();
            let mut buffers = OwnedBuffers::new(2, 2, 128);
            r.play_from(0).unwrap();
            let (_, count) = armed(|| {
                for _ in 0..24 {
                    r.processor.process_device(&mut buffers);
                }
            });
            assert_eq!(count, 0, "{label}: startup and gain automation allocate");
            r.play_from(0).unwrap();
            let (_, count) = armed(|| {
                for _ in 0..24 {
                    r.processor.process_device(&mut buffers);
                }
            });
            assert_eq!(count, 0, "{label}: transport reset allocates");
        }
    }
}

/// Console summing: every family's channel stage on a stereo track and its
/// bus amplifier on the master, inline and on its workers, the drive
/// turned while playing, a transport reset.
#[test]
fn the_console_does_not_allocate() {
    let _serial = serial();
    use faderframe_core::{ChannelLayout, builtin};
    use faderframe_project::{PluginRef, PluginSlot, TrackKind, console::Console};
    use faderframe_timeline::MusicalTime;
    for (family, (name, label, _)) in builtin::CONSOLE_BUSES.into_iter().enumerate() {
        let mut tp = common::TestProject::new(48_000);
        let t = tp.track(TrackKind::Audio, "Voice", ChannelLayout::Stereo);
        let wave: Vec<f32> = (0..16_384).map(|i| 0.7 * (i as f32 * 0.05).sin()).collect();
        let src = tp.source(faderframe_audio_files::AudioData::from_channels(
            48_000,
            vec![wave.clone(), wave],
        ));
        tp.clip(t, src, MusicalTime::ZERO, 16_384);
        tp.project.console = Some(Console {
            drive_db: 6.0,
            ..Console::new(family as u8)
        });
        let plugin = tp.project.ids.allocate();
        let master = tp.project.master_id().unwrap();
        tp.project.track_mut(master).unwrap().preamp = Some(PluginSlot {
            id: plugin,
            plugin: PluginRef::builtin(name, label),
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
        });
        for realtime in [false, true] {
            let mut r =
                OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 128, 2)
                    .unwrap();
            r.controller.plugins().set_realtime(realtime);
            r.controller.rebuild_graph(&tp.project).unwrap();
            r.controller.update_params(&tp.project).unwrap();
            let mut buffers = OwnedBuffers::new(2, 2, 128);
            r.play_from(0).unwrap();
            let (_, count) = armed(|| {
                for _ in 0..24 {
                    r.processor.process_device(&mut buffers);
                }
            });
            assert_eq!(count, 0, "{label}: playing allocates");
            let mut hotter = tp.project.clone();
            if let Some(c) = &mut hotter.console {
                c.drive_db = -9.0;
            }
            r.controller.update_params(&hotter).unwrap();
            r.play_from(0).unwrap();
            let (_, count) = armed(|| {
                for _ in 0..24 {
                    r.processor.process_device(&mut buffers);
                }
            });
            assert_eq!(count, 0, "{label}: the drive or a reset allocates");
        }
    }
}

/// The Guitar Station in a graph, inline and on its stages' workers: a line
/// of three pedals, a wah's treadle and the amplifier's drive automated, a
/// footswitch, a stereo signal waking the second channel, a reset.
#[test]
fn the_guitar_station_does_not_allocate() {
    let _serial = serial();
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    use faderframe_core::{ChannelLayout, ParameterId, builtin};
    use faderframe_guitar::pedal::Stomp;
    use faderframe_guitar::voice::Pedal;
    use faderframe_plugin_host::devices::guitar::id;
    use faderframe_project::{PluginRef, PluginSlot, SavedParameter, TrackKind};
    use faderframe_timeline::MusicalTime;
    let mut tp = common::TestProject::new(48_000);
    let t = tp.track(TrackKind::Audio, "Guitar", ChannelLayout::Stereo);
    let left: Vec<f32> = (0..48_000)
        .map(|n| (0.2 * (std::f64::consts::TAU * 110.0 * n as f64 / 48_000.0).sin()) as f32)
        .collect();
    let mut right = left.clone();
    right[6_000..9_000].iter_mut().for_each(|x| *x *= 0.5);
    let src = tp.source(faderframe_audio_files::AudioData::from_channels(
        48_000,
        vec![left, right],
    ));
    tp.clip(t, src, MusicalTime::ZERO, 48_000);
    let plugin = tp.project.ids.allocate();
    let set = |id: u32, value: f64| SavedParameter {
        id: ParameterId(id),
        value,
    };
    let lanes: Vec<_> = (0..3).map(|_| tp.project.ids.allocate()).collect();
    let track = tp.project.track_mut(t).unwrap();
    track.inserts.push(PluginSlot {
        id: plugin,
        plugin: PluginRef::builtin(builtin::GUITAR_STATION, "Guitar Station"),
        bypass: false,
        parameters: vec![
            set(
                id::slot(0, id::STOMP),
                Stomp::Wah(faderframe_guitar::circuits::wah::Build::V847).index() as f64,
            ),
            set(
                id::slot(1, id::STOMP),
                Stomp::Pedal(Pedal::BlueChorus).index() as f64,
            ),
            set(
                id::slot(2, id::STOMP),
                Stomp::Pedal(Pedal::MetalZone).index() as f64,
            ),
            set(id::AMP, 8.0),
            set(id::REVERB, 0.3),
            set(id::INTENSITY, 0.4),
            set(id::MIC_B, 6.0),
        ],
        state: None,
        sidechain: None,
    });
    let automate = |lane, parameter: u32, values: [f64; 3]| AutomationLane {
        id: lane,
        target: AutomationTarget::PluginParameter {
            plugin,
            parameter: ParameterId(parameter),
        },
        curve: AutomationCurve::from_points(
            values
                .iter()
                .enumerate()
                .map(|(i, v)| AutomationPoint {
                    time: MusicalTime::from_quarters(0.04 * i as f64),
                    value: *v,
                    shape: CurveShape::Step,
                })
                .collect(),
        ),
        mode: AutomationMode::Read,
        visible: true,
    };
    track
        .automation
        .lanes
        .push(automate(lanes[0], id::DRIVE, [0.2, 0.8, 0.4]));
    track.automation.lanes.push(automate(
        lanes[1],
        id::slot(0, id::TREADLE),
        [0.1, 0.9, 0.5],
    ));
    track
        .automation
        .lanes
        .push(automate(lanes[2], id::slot(2, id::ON), [1.0, 0.0, 1.0]));
    for realtime in [false, true] {
        let mut r = OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 128, 2)
            .unwrap();
        r.controller.plugins().set_realtime(realtime);
        r.controller.rebuild_graph(&tp.project).unwrap();
        let mut buffers = OwnedBuffers::new(2, 2, 128);
        r.play_from(0).unwrap();
        for _ in 0..8 {
            r.processor.process_device(&mut buffers);
        }
        let (_, count) = armed(|| {
            for _ in 0..96 {
                r.processor.process_device(&mut buffers);
            }
        });
        assert_eq!(
            count, 0,
            "realtime {realtime}: playing and automating allocate"
        );
        r.play_from(0).unwrap();
        let (_, count) = armed(|| {
            for _ in 0..24 {
                r.processor.process_device(&mut buffers);
            }
        });
        assert_eq!(count, 0, "realtime {realtime}: a transport reset allocates");
    }
}

/// The MIDI effects, chained before a synth, played live and from the key
/// track: arpeggios, strummed chords, scale moves and echoes allocate
/// nothing.
#[test]
fn the_midi_effects_do_not_allocate() {
    let _serial = serial();
    use faderframe_core::{ChannelLayout, ParameterId, builtin};
    use faderframe_project::harmony::{Key, Scale};
    use faderframe_project::{Impact, KeyChange, PluginRef, PluginSlot, SavedParameter, TrackKind};
    use faderframe_timeline::MusicalTime;
    use std::collections::HashSet;
    const SR: u32 = 48_000;
    let mut tp = common::TestProject::new(SR);
    let t = tp.track(TrackKind::Instrument, "Fx", ChannelLayout::Stereo);
    let mut slot = |id: &str, set: &[(u32, f64)]| PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(id, id),
        bypass: false,
        parameters: set
            .iter()
            .map(|(k, v)| SavedParameter {
                id: ParameterId(*k),
                value: *v,
            })
            .collect(),
        state: None,
        sidechain: None,
    };
    let inserts = vec![
        slot(
            builtin::ARPEGGIATOR,
            &[(0, 2.0), (3, 2.0), (4, 0.5), (6, 1.0)],
        ),
        slot(builtin::CHORD, &[(0, 1.0), (9, 20.0)]),
        // The chord track's chords (a slash chord's bass too).
        slot(builtin::CHORD, &[(0, 3.0)]),
        slot(builtin::SCALE, &[(4, 2.0)]),
        slot(builtin::NOTE_ECHO, &[(3, 8.0), (4, 0.9), (5, 7.0)]),
        slot(builtin::SYNTH, &[]),
    ];
    tp.project.track_mut(t).unwrap().inserts = inserts;
    tp.project.chords = ["Am7/G", "F", "Cmaj9", "G"]
        .iter()
        .enumerate()
        .map(|(i, c)| faderframe_project::ChordEvent {
            start: MusicalTime::from_quarters(i as f64),
            end: MusicalTime::from_quarters(i as f64 + 1.0),
            chord: faderframe_project::harmony::Chord::parse(c).unwrap(),
        })
        .collect();
    tp.project.keys = vec![
        KeyChange {
            at: MusicalTime::ZERO,
            key: Key::new(2, Scale::Major),
        },
        KeyChange {
            at: MusicalTime::from_quarters(2.0),
            key: Key::new(9, Scale::Minor),
        },
    ];
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    let (tx, q, _feed) = faderframe_midi::midi_input_queue(256);
    r.controller.set_midi_input(q).unwrap();
    r.controller.set_midi_live(HashSet::from([t]));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 256);
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    let mut total = 0;
    for key in 48..72u8 {
        tx.send(0, &[0x90, key, 60 + key]);
        let (_, n) = armed(|| {
            for _ in 0..12 {
                r.processor.process_device(&mut bufs);
            }
        });
        total += n;
        if key % 4 != 0 {
            tx.send(0, &[0x80, key, 0]);
        }
    }
    let mut loud = 0.0f32;
    let (_, n) = armed(|| {
        for _ in 0..200 {
            r.processor.process_device(&mut bufs);
            loud = bufs.output_ref(0).iter().fold(loud, |m, v| m.max(v.abs()));
        }
    });
    assert_eq!(total + n, 0, "allocations in the MIDI effects");
    assert!(loud > 1e-3, "the synth plays the echoes: {loud}");
}

/// Surround strips (the panner's matrix made anew as it moves, a bed
/// folded into another) do not allocate.
#[test]
fn surround_strips_do_not_allocate() {
    let _serial = serial();
    use faderframe_core::{ChannelLayout, SurroundFormat, SurroundPan};
    use faderframe_project::{OutputRouting, TrackKind};
    use faderframe_timeline::MusicalTime;
    let mut tp = common::TestProject::new(48_000);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = ChannelLayout::Surround(SurroundFormat::S51);
    let bus = tp.track(
        TrackKind::Bus,
        "Bed",
        ChannelLayout::Surround(SurroundFormat::S714),
    );
    let t = tp.track(TrackKind::Audio, "Stereo", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.3, 96_000);
    tp.clip(t, src, MusicalTime::ZERO, 96_000);
    tp.project.track_mut(t).unwrap().output = OutputRouting::Track { track: bus };
    // An object beside it: it joins the master in its renderer.
    let o = tp.track(TrackKind::Audio, "Object", ChannelLayout::Mono);
    let src = tp.dc(1, 0.3, 96_000);
    tp.clip(o, src, MusicalTime::ZERO, 96_000);
    tp.project.track_mut(o).unwrap().object = true;
    assert!(tp.project.is_object(tp.project.track(o).unwrap()));
    // A pre-fader send that follows the moving panner into a 5.1 reverb.
    let verb = tp.track(
        TrackKind::Aux,
        "Verb",
        ChannelLayout::Surround(SurroundFormat::S51),
    );
    let send = tp.project.ids.allocate();
    tp.project
        .track_mut(t)
        .unwrap()
        .sends
        .push(faderframe_project::AuxSend {
            id: send,
            target: verb,
            level_db: -6.0,
            tap: faderframe_project::SendTap::PreFader,
            enabled: true,
        });
    let config = EngineConfig {
        sample_rate: 48_000,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 6).unwrap();
    // Heard on headphones (with a room, another head, a correction of
    // filters and a graphic EQ) and in mono.
    r.controller
        .set_binaural(Some(faderframe_binaural::Room::Mid));
    r.controller
        .set_head(faderframe_binaural::Head::builtin("sadie-h5").unwrap());
    r.controller.set_headphone_correction(Some(std::sync::Arc::new(
        faderframe_binaural::Correction::parse(
            "test",
            "Preamp: -3 dB\nFilter 1: ON PK Fc 1000 Hz Gain 3 dB Q 1\nGraphicEQ: 20 0; 1000 2; 20000 -2",
        )
        .unwrap(),
    )));
    r.controller
        .sync(&tp.project, &tp.sources, faderframe_project::Impact::Graph)
        .unwrap();
    r.controller.set_mono_check(true).unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 6, 256);
    for _ in 0..4 {
        r.processor.process_device(&mut bufs);
    }
    let mut total = 0;
    for i in 0..32 {
        // The panner moves every block.
        tp.project.track_mut(t).unwrap().surround = SurroundPan {
            x: (i as f32 * 0.3).sin(),
            y: (i as f32 * 0.2).cos(),
            z: (i % 4) as f32 / 4.0,
            spread: 0.2,
            ..SurroundPan::default()
        };
        tp.project.track_mut(o).unwrap().surround.x = -(i as f32 * 0.3).sin();
        r.controller.update_params(&tp.project).unwrap();
        let (_, n) = armed(|| r.processor.process_device(&mut bufs));
        total += n;
    }
    assert_eq!(total, 0, "allocations/frees on the audio thread");
}

/// A hardware insert on a live (four-channel) device — its send, return
/// and mix — and a ping, on the audio thread.
#[test]
fn the_hardware_insert_and_its_ping_do_not_allocate() {
    let _serial = serial();
    use faderframe_audio::{AudioCallback, StreamInfo};
    use faderframe_core::{ChannelLayout, ParameterId, builtin};
    use faderframe_project::{Impact, PluginRef, PluginSlot, SavedParameter, TrackKind};
    use faderframe_transport::TransportCommand;

    const SR: u32 = 48_000;
    let mut tp = common::TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Vox", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.25, 100_000);
    tp.clip(t, src, faderframe_timeline::MusicalTime::ZERO, 100_000);
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::HARDWARE_INSERT, "Hardware Insert"),
        bypass: false,
        parameters: vec![
            SavedParameter {
                id: ParameterId(4),
                value: 0.5,
            },
            SavedParameter {
                id: ParameterId(6),
                value: 300.0,
            },
        ],
        state: None,
        sidechain: None,
    };
    tp.project.track_mut(t).unwrap().inserts.push(slot);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let (mut c, mut p) = faderframe_engine::create(config);
    AudioCallback::prepare(
        &mut p,
        &StreamInfo {
            backend: "test",
            device: "four".into(),
            sample_rate: SR,
            buffer_size: 256,
            input_channels: 4,
            output_channels: 4,
            input_latency: 0,
            output_latency: 0,
        },
    );
    c.sync(&tp.project, &tp.sources, Impact::Graph).unwrap();
    c.transport(TransportCommand::Locate(0)).unwrap();
    c.transport(TransportCommand::Play).unwrap();
    let mut bufs = OwnedBuffers::new(4, 4, 256);
    for _ in 0..8 {
        p.process_device(&mut bufs);
        c.collect_garbage();
    }
    c.ping(2, 2);
    let (_, allocs) = armed(|| {
        for _ in 0..200 {
            p.process_device(&mut bufs);
        }
    });
    assert_eq!(allocs, 0, "allocations with a hardware insert and a ping");
}
