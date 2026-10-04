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
    let (v, n) = armed(|| std::hint::black_box(Vec::<u64>::with_capacity(16)));
    drop(v);
    assert_eq!(n, 1);
}

#[test]
fn processing_does_not_allocate() {
    const SR: u32 = 48_000;
    const BLOCK: usize = 256;
    let mut project = demo_project(SR);
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
fn live_midi_input_and_midi_recording_do_not_allocate() {
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
        .find(|t| t.instrument.is_some())
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
        "allocations/frees with live MIDI, recording and MIDI clock"
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
    assert_eq!(n, 0, "allocations/frees while playing warped audio");
}
