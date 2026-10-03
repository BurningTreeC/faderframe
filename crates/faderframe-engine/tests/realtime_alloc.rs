//! The realtime path must not allocate or free memory.
//!
//! A counting global allocator is armed (per thread) around calls to the
//! engine's processing entry point while the demo project plays — including
//! MIDI/synth voices, the echo plugin, a loop wrap, a stop/start and a graph
//! swap with state adoption.

use faderframe_audio::OwnedBuffers;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::demo::demo_project;
use faderframe_project::{Command, History};
use faderframe_transport::TransportCommand;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static EVENTS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

fn note() {
    if ARMED.try_with(|a| a.get()).unwrap_or(false) {
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
    let r = f();
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
