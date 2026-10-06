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

#[test]
fn rendering_ahead_does_not_allocate_on_the_audio_thread() {
    let _serial = serial();
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
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: BLOCK,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    r.controller
        .set_render_ahead(Some(std::time::Duration::from_millis(100)), 1);
    r.controller
        .sync(&project, &sources, faderframe_project::Impact::Graph)
        .unwrap();
    assert!(!r.controller.ahead_tracks().is_empty());
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
    assert_eq!(total, 0, "allocations/frees on the audio thread");
    assert_eq!(r.controller.ahead_misses(), 0);
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
    for key in 44..80u8 {
        tx.send(0, &[0x90, key, 40 + key]);
        let (_, n) = armed(|| {
            for _ in 0..4 {
                taps.iter().for_each(|t| t.watch());
                r.processor.process_device(&mut bufs);
            }
        });
        total += n;
        if key % 3 != 0 {
            tx.send(0, &[0x80, key, 0]);
        }
    }
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
        slot(builtin::SCALE, &[(4, 2.0)]),
        slot(builtin::NOTE_ECHO, &[(3, 8.0), (4, 0.9), (5, 7.0)]),
        slot(builtin::SYNTH, &[]),
    ];
    tp.project.track_mut(t).unwrap().inserts = inserts;
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
