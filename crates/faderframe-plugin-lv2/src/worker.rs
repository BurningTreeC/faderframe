//! The worker extension: work the plugin schedules from `run` (loading a
//! sample, say) done on a thread of its own, the responses handed back on
//! the audio thread after `run`. Messages travel through byte rings
//! (length-prefixed), so the audio thread never blocks or allocates.

use crate::sys::{self, LV2_Handle, LV2_Worker_Interface};
use faderframe_realtime::TryCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// Bytes each ring holds.
const RING: usize = 1 << 18;
/// The largest message.
pub const MESSAGE: usize = 1 << 16;

/// What the schedule feature points to.
pub struct Shared {
    requests: TryCell<rtrb::Producer<u8>>,
    thread: OnceLock<std::thread::Thread>,
}

/// An instance's worker: the schedule feature, the thread, the responses.
pub struct Worker {
    shared: Arc<Shared>,
    /// For the thread (until started).
    pending: Option<(rtrb::Consumer<u8>, rtrb::Producer<u8>)>,
    /// Responses for the audio thread.
    pub(crate) responses: Arc<TryCell<rtrb::Consumer<u8>>>,
    stop: Arc<AtomicBool>,
    /// Held around `work` (and by state restores, which must not overlap
    /// it).
    gate: Arc<Mutex<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

/// The plugin's handle and interface, used on the worker thread (the
/// extension calls `work` alongside `run`).
#[derive(Clone, Copy)]
struct Target {
    handle: LV2_Handle,
    iface: *const LV2_Worker_Interface,
}
// SAFETY: LV2's worker extension has `work` called from a thread of its
// own; the plugin and its interface live until the worker is stopped.
unsafe impl Send for Target {}

/// Write one length-prefixed message (`false`: no room).
fn push(tx: &mut rtrb::Producer<u8>, body: &[u8]) -> bool {
    if body.len() > MESSAGE || tx.slots() < 4 + body.len() {
        return false;
    }
    match tx.write_chunk_uninit(4 + body.len()) {
        Ok(chunk) => {
            chunk.fill_from_iter(
                (body.len() as u32)
                    .to_le_bytes()
                    .into_iter()
                    .chain(body.iter().copied()),
            );
            true
        }
        Err(_) => false,
    }
}

/// One length-prefixed message into `out` (`false`: none).
pub(crate) fn pop(rx: &mut rtrb::Consumer<u8>, out: &mut Vec<u8>) -> bool {
    if rx.slots() < 4 {
        return false;
    }
    let mut len = [0u8; 4];
    if let Ok(chunk) = rx.read_chunk(4) {
        let (a, b) = chunk.as_slices();
        for (d, s) in len.iter_mut().zip(a.iter().chain(b)) {
            *d = *s;
        }
        chunk.commit_all();
    }
    let n = (u32::from_le_bytes(len) as usize).min(rx.slots());
    out.clear();
    if let Ok(chunk) = rx.read_chunk(n) {
        let (a, b) = chunk.as_slices();
        out.extend_from_slice(a);
        out.extend_from_slice(b);
        chunk.commit_all();
    }
    true
}

/// The bytes a plugin hands over.
///
/// # Safety
/// `data` holds `size` readable bytes (or `size` is 0).
unsafe fn bytes<'a>(data: *const c_void, size: u32) -> Option<&'a [u8]> {
    if size == 0 {
        return Some(&[]);
    }
    if data.is_null() {
        return None;
    }
    // SAFETY: as documented.
    Some(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) })
}

unsafe extern "C" fn schedule(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    // SAFETY: the feature was made with a `Shared` that lives as long as
    // the instance.
    let shared = unsafe { &*handle.cast::<Shared>() };
    // SAFETY: the plugin's message.
    let Some(body) = (unsafe { bytes(data, size) }) else {
        return sys::LV2_WORKER_ERR_NO_SPACE;
    };
    let pushed = match shared.requests.try_lock() {
        Some(mut tx) => push(&mut tx, body),
        None => false,
    };
    if !pushed {
        return sys::LV2_WORKER_ERR_NO_SPACE;
    }
    if let Some(t) = shared.thread.get() {
        t.unpark();
    }
    sys::LV2_WORKER_SUCCESS
}

unsafe extern "C" fn respond(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    // SAFETY: the worker thread's response producer, alive during `work`.
    let tx = unsafe { &mut *handle.cast::<rtrb::Producer<u8>>() };
    // SAFETY: the plugin's message.
    match unsafe { bytes(data, size) } {
        Some(body) if push(tx, body) => sys::LV2_WORKER_SUCCESS,
        _ => sys::LV2_WORKER_ERR_NO_SPACE,
    }
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

impl Worker {
    pub fn new() -> Worker {
        let (req_tx, req_rx) = rtrb::RingBuffer::new(RING);
        let (res_tx, res_rx) = rtrb::RingBuffer::new(RING);
        Worker {
            shared: Arc::new(Shared {
                requests: TryCell::new(req_tx),
                thread: OnceLock::new(),
            }),
            pending: Some((req_rx, res_tx)),
            responses: Arc::new(TryCell::new(res_rx)),
            stop: Arc::new(AtomicBool::new(false)),
            gate: Arc::new(Mutex::new(())),
            join: None,
        }
    }

    /// Keep the thread from calling `work` while the guard lives.
    pub fn pause(&self) -> Option<MutexGuard<'_, ()>> {
        self.gate.lock().ok()
    }

    /// The schedule feature's data.
    pub fn feature(&self) -> sys::LV2_Worker_Schedule {
        sys::LV2_Worker_Schedule {
            handle: Arc::as_ptr(&self.shared).cast_mut().cast(),
            schedule_work: Some(schedule),
        }
    }

    /// Start the thread calling `iface.work` on `handle`.
    ///
    /// # Safety
    /// `handle` and `iface` stay valid until the worker is dropped (it is
    /// dropped before the instance is cleaned up).
    pub unsafe fn start(&mut self, handle: LV2_Handle, iface: *const LV2_Worker_Interface) {
        let Some((mut rx, mut tx)) = self.pending.take() else {
            return;
        };
        let target = Target { handle, iface };
        let stop = Arc::clone(&self.stop);
        let gate = Arc::clone(&self.gate);
        let spawned = std::thread::Builder::new()
            .name("faderframe-lv2-worker".into())
            .spawn(move || {
                let target = target;
                let mut message = Vec::with_capacity(MESSAGE);
                while !stop.load(Ordering::Acquire) {
                    while pop(&mut rx, &mut message) {
                        let _held = gate.lock();
                        // SAFETY: the interface and handle are valid (see
                        // `start`); `work` gets the message and our ring.
                        unsafe {
                            if let Some(work) = (*target.iface).work {
                                work(
                                    target.handle,
                                    respond,
                                    (&mut tx as *mut rtrb::Producer<u8>).cast(),
                                    message.len() as u32,
                                    message.as_ptr().cast(),
                                );
                            }
                        }
                    }
                    std::thread::park_timeout(std::time::Duration::from_millis(50));
                }
            });
        match spawned {
            Ok(j) => {
                let _ = self.shared.thread.set(j.thread().clone());
                self.join = Some(j);
            }
            Err(e) => tracing::warn!("LV2 worker: {e}"),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(j) = self.join.take() {
            j.thread().unpark();
            let _ = j.join();
        }
    }
}

/// The audio thread's half: hand the responses to the plugin after `run`.
pub struct Responses {
    responses: Arc<TryCell<rtrb::Consumer<u8>>>,
    iface: *const LV2_Worker_Interface,
    scratch: Vec<u8>,
}

// SAFETY: moved to the audio thread with the processor; the interface is
// the plugin's static table.
unsafe impl Send for Responses {}

impl Responses {
    pub fn new(worker: &Worker, iface: *const LV2_Worker_Interface) -> Responses {
        Responses {
            responses: Arc::clone(&worker.responses),
            iface,
            scratch: Vec::with_capacity(MESSAGE),
        }
    }

    /// After `run`: every response, then `end_run`.
    ///
    /// # Safety
    /// `handle` is the instance the interface belongs to, on its audio
    /// thread.
    pub unsafe fn deliver(&mut self, handle: LV2_Handle) {
        if let Some(mut rx) = self.responses.try_lock() {
            while pop(&mut rx, &mut self.scratch) {
                // SAFETY: as documented.
                unsafe {
                    if let Some(f) = (*self.iface).work_response {
                        f(
                            handle,
                            self.scratch.len() as u32,
                            self.scratch.as_ptr().cast(),
                        );
                    }
                }
            }
        }
        // SAFETY: as documented.
        unsafe {
            if let Some(f) = (*self.iface).end_run {
                f(handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_go_through_the_rings_with_their_length() {
        let (mut tx, mut rx) = rtrb::RingBuffer::new(64);
        assert!(push(&mut tx, b"hello"));
        assert!(push(&mut tx, b""));
        assert!(!push(&mut tx, &[0u8; 64]), "no room");
        let mut out = Vec::new();
        assert!(pop(&mut rx, &mut out));
        assert_eq!(out, b"hello");
        assert!(pop(&mut rx, &mut out));
        assert!(out.is_empty());
        assert!(!pop(&mut rx, &mut out));
    }
}
