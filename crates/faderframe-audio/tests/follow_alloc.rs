//! Joining followers must not allocate or free on the audio threads: the
//! master's callback (reading and feeding followers through their rings
//! and resamplers, taking a new link when a follower comes back, giving
//! the old one back) and the followers' own callbacks.
#![allow(clippy::unwrap_used)]

use faderframe_audio::follow::{Direction, FollowerEnd, FollowerSpec, Place, join};
use faderframe_audio::{AudioCallback, DeviceBuffers, OwnedBuffers, StreamInfo};
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

/// Copies the last input to every output.
struct Through;

impl AudioCallback for Through {
    fn prepare(&mut self, _info: &StreamInfo) {}
    fn process(&mut self, io: &mut dyn DeviceBuffers) {
        let n = io.frames();
        let mut x = [0.0f32; 8192];
        let last = io.input_channels() - 1;
        x[..n].copy_from_slice(io.input(last));
        for c in 0..io.output_channels() {
            io.output(c).copy_from_slice(&x[..n]);
        }
    }
}

#[test]
fn joining_followers_does_not_allocate() {
    let spec = |direction, rate| FollowerSpec {
        device: "box".into(),
        name: "Box".into(),
        direction,
        channels: 2,
        sample_rate: rate,
        block: 441,
    };
    let info = StreamInfo {
        backend: "test",
        device: "desk".into(),
        sample_rate: 48_000,
        buffer_size: 256,
        input_channels: 1,
        output_channels: 2,
        input_latency: 0,
        output_latency: 0,
    };
    let (mut joined, mut followers, ends) = join(
        Box::new(Through),
        &info,
        vec![
            Place::Master(0),
            Place::Follower {
                follower: 0,
                channel: 0,
            },
            Place::Follower {
                follower: 0,
                channel: 1,
            },
        ],
        vec![
            Place::Master(0),
            Place::Master(1),
            Place::Follower {
                follower: 1,
                channel: 0,
            },
            Place::Follower {
                follower: 1,
                channel: 1,
            },
        ],
        vec![
            spec(Direction::Input, 44_100),
            spec(Direction::Output, 44_100),
        ],
    );
    let mut ends = ends.into_iter();
    let Some(FollowerEnd::Input(mut input)) = ends.next() else {
        panic!("input end");
    };
    let Some(FollowerEnd::Output(mut output)) = ends.next() else {
        panic!("output end");
    };
    let mut io = OwnedBuffers::new(1, 2, 256);
    let mut next = 0.0f64;
    let rate = 44_100.0 * (1.0 + 70e-6);
    let mut played = [0.0f32; 441 * 2];
    let mut run = |from: usize,
                   to: usize,
                   input: &mut faderframe_audio::follow::FollowerInput,
                   output: &mut faderframe_audio::follow::FollowerOutput,
                   joined: &mut faderframe_audio::follow::Joined,
                   next: &mut f64| {
        for k in from..to {
            let t = k as f64 * 256.0 / 48_000.0;
            while *next <= t {
                let ns = (*next * 1e9) as u64;
                input.capture_with(441, ns, |i, c| ((i + c) as f32 * 0.01).sin());
                output.play_with(441, ns, |i, c, v| played[i * 2 + c] = v);
                *next += 441.0 / rate;
            }
            joined.process_at(&mut io, (t * 1e9) as u64);
        }
    };
    // Settle (the links start, the rings fill), then count.
    run(0, 200, &mut input, &mut output, &mut joined, &mut next);
    let (_, allocs) = armed(|| run(200, 3000, &mut input, &mut output, &mut joined, &mut next));
    assert_eq!(allocs, 0, "allocations joining followers");
    // A follower comes back: the new links are taken and the old ones go
    // back to the control side, which drops them. The output claims a rate
    // it does not run at: its ring runs dry again and again (gaps).
    let Some(FollowerEnd::Input(mut input)) = followers.attach(0, 44_100, 441) else {
        panic!("attached");
    };
    let Some(FollowerEnd::Output(mut output)) = followers.attach(1, 48_000, 480) else {
        panic!("attached");
    };
    let (_, allocs) = armed(|| run(3000, 4000, &mut input, &mut output, &mut joined, &mut next));
    assert_eq!(allocs, 0, "allocations taking new links");
    followers.collect();
    assert_eq!(followers.state(0).unwrap().attached, 2);
}
