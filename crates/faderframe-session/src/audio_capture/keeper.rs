//! The keeper: a thread draining the engine's capture rings into the
//! store, a step below the audio thread like the recording writer (the
//! rings hold [`TRANSFER_SECONDS`]; a keeper held up longer loses frames,
//! counted and kept as silence).
//!
//! The session hands it the ends of every capture the engine starts
//! ([`Keeper::feed`]); it finishes reading one before it takes the next
//! (the engine lets go of the old rings when it starts on the new ones),
//! and tells the store which channels the new one carries.

use super::store::Store;
use faderframe_engine::input_capture::CaptureStreams;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

/// Seconds the engine's rings hold for the keeper.
pub const TRANSFER_SECONDS: f64 = 2.0;
/// Most blocks read under one lock of the store.
const BLOCKS_PER_LOCK: usize = 64;

struct Shared {
    stop: AtomicBool,
    /// Captures taken in so far, and how many when the keeper last ran
    /// out of them (`u64::MAX`: it has one).
    taken: AtomicU64,
    idle_after: AtomicU64,
}

pub struct Keeper {
    store: Arc<Mutex<Store>>,
    tx: Option<Sender<CaptureStreams>>,
    shared: Arc<Shared>,
    /// Captures handed over.
    fed: u64,
    handle: Option<JoinHandle<()>>,
}

impl Keeper {
    pub fn start(store: Store) -> std::io::Result<Self> {
        let store = Arc::new(Mutex::new(store));
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            taken: AtomicU64::new(0),
            idle_after: AtomicU64::new(0),
        });
        let (tx, rx) = channel();
        let handle = {
            let (store, shared) = (Arc::clone(&store), Arc::clone(&shared));
            std::thread::Builder::new()
                .name("faderframe-capture".into())
                .spawn(move || run(&store, &shared, &rx))?
        };
        Ok(Self {
            store,
            tx: Some(tx),
            shared,
            fed: 0,
            handle: Some(handle),
        })
    }

    /// The ends of a capture the engine starts on.
    pub fn feed(&mut self, streams: CaptureStreams) {
        if let Some(tx) = &self.tx
            && tx.send(streams).is_ok()
        {
            self.fed += 1;
        }
    }

    /// Every capture handed over has ended on the engine's side: the
    /// engine let go of it without being asked (a new engine).
    pub fn orphaned(&self) -> bool {
        self.shared.idle_after.load(Ordering::Acquire) == self.fed
            && self.shared.taken.load(Ordering::Acquire) == self.fed
            && self.fed > 0
    }

    pub fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.tx = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run(store: &Mutex<Store>, shared: &Shared, rx: &Receiver<CaptureStreams>) {
    let mut current: Option<CaptureStreams> = None;
    let mut pending: VecDeque<CaptureStreams> = VecDeque::new();
    let mut block: Vec<f32> = Vec::new();
    let mut zeros: Vec<f32> = Vec::new();
    let mut taken = 0u64;
    let mut gone = false;
    while !shared.stop.load(Ordering::Acquire) {
        // A step below the audio thread (its rings hold seconds).
        faderframe_realtime::priority::follow_audio(faderframe_realtime::priority::Helper::Feeder);
        loop {
            match rx.try_recv() {
                Ok(s) => pending.push_back(s),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    gone = true;
                    break;
                }
            }
        }
        let worked = match current.as_mut() {
            Some(c) => drain(c, store, &mut block, &mut zeros),
            None => false,
        };
        if current.as_ref().is_none_or(CaptureStreams::is_finished) {
            if let Some(next) = pending.pop_front() {
                lock(store).set_channels(&next.channels);
                current = Some(next);
                taken += 1;
                shared.taken.store(taken, Ordering::Release);
                shared.idle_after.store(u64::MAX, Ordering::Release);
                continue;
            }
            if current.take().is_some() {
                shared.idle_after.store(taken, Ordering::Release);
            }
            if gone {
                return;
            }
            // Nothing to read: wait for the next capture.
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(s) => pending.push_back(s),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => gone = true,
            }
            continue;
        }
        if !worked {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn lock(store: &Mutex<Store>) -> MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

/// Move what came in into the store; whether anything did.
fn drain(
    c: &mut CaptureStreams,
    store: &Mutex<Store>,
    block: &mut Vec<f32>,
    zeros: &mut Vec<f32>,
) -> bool {
    let mut worked = false;
    let channels = c.channels.len();
    loop {
        if c.headers.is_empty() {
            return worked;
        }
        let mut s = lock(store);
        for _ in 0..BLOCKS_PER_LOCK {
            let Ok(h) = c.headers.pop() else {
                return worked;
            };
            worked = true;
            // Catching up never sleeps: within the realtime time limit.
            faderframe_realtime::priority::breathe();
            let n = h.frames as usize;
            let samples = if h.lost {
                None
            } else {
                block.clear();
                block.resize(n * channels, 0.0);
                // A header is pushed after its samples: they are there.
                match c.data.pop_entire_slice(block) {
                    Ok(()) => Some(&block[..]),
                    Err(_) => None,
                }
            };
            s.push(
                h.frame,
                n,
                h.position,
                h.recording,
                &c.channels,
                samples,
                zeros,
            );
        }
    }
}
