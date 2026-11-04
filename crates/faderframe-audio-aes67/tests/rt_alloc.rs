//! The AES67 audio thread must not allocate or free: reading the receive
//! rings (frames there and frames missing), running the callback, filling
//! the send rings, changing the buffer size.
#![allow(clippy::unwrap_used)]

use faderframe_audio::{AudioCallback, DeviceBuffers, StreamInfo};
use faderframe_audio_aes67::backend::Clocked;
use faderframe_audio_aes67::ring::MediaRing;
use faderframe_audio_aes67::stream::RxStats;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
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

/// Inputs to outputs, crossed.
struct Through;

impl AudioCallback for Through {
    fn prepare(&mut self, _info: &StreamInfo) {}
    fn process(&mut self, io: &mut dyn DeviceBuffers) {
        let n = io.frames();
        let mut x = [0.0f32; 8192];
        for c in 0..io.output_channels() {
            let from = io.input_channels() - 1 - c % io.input_channels();
            x[..n].copy_from_slice(io.input(from));
            io.output(c).copy_from_slice(&x[..n]);
        }
    }
}

#[test]
fn the_aes67_audio_thread_does_not_allocate() {
    let rx = Arc::new(MediaRing::new(2, 65_536));
    let stats = Arc::new(RxStats::default());
    let tx = Arc::new(MediaRing::new(2, 65_536));
    let info = StreamInfo {
        backend: "aes67",
        device: "AES67 network".into(),
        sample_rate: 48_000,
        buffer_size: 64,
        // One more input than the stream has (a silent channel).
        input_channels: 3,
        output_channels: 2,
        input_latency: 96,
        output_latency: 96,
    };
    let link = 96u64;
    let mut clocked = Clocked::new(
        Box::new(Through),
        info,
        vec![(Arc::clone(&rx), Arc::clone(&stats))],
        vec![Arc::clone(&tx)],
        link,
    );
    clocked.prepare();
    let start = 81_600_000_000_000u64;
    // Packets for the first half; the rest never came.
    for k in 0..200u64 {
        rx.write(start + k * 48, 48, |i, c| (i + c) as f32 * 0.01);
    }
    let mut now = start + link + 64;
    let run = |clocked: &mut Clocked, now: &mut u64, blocks: usize| {
        for _ in 0..blocks {
            clocked.block(*now);
            *now += u64::from(clocked.info().buffer_size);
        }
    };
    run(&mut clocked, &mut now, 4);
    let (_, allocs) = armed(|| {
        run(&mut clocked, &mut now, 300);
        clocked.set_block(256);
        run(&mut clocked, &mut now, 100);
    });
    assert_eq!(allocs, 0, "allocations on the AES67 audio thread");
    assert!(
        stats.missing.load(Ordering::Relaxed) > 0,
        "missing frames counted"
    );
    // What came in went out a block ahead of the callback, on the same
    // media clock.
    assert!(tx.has(start + link + 64 + 64));
}
