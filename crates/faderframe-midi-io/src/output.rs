//! MIDI output devices and the sender thread.
//!
//! The engine queues messages with the time they are due (the moment the
//! audio of the same callback is heard); [`MidiOutputs`] runs a thread that
//! keeps them in time order and sends each when it is due, to every enabled
//! output port (ALSA sequencer via `midir`) or a virtual capture port.
//! SysEx comes from the control thread instead ([`MidiOutputs::send_sysex`]),
//! scheduled ahead with its due time; scheduled SysEx can be cancelled (the
//! transport stopped or jumped).

use crate::{CLIENT_NAME, port_identity};
use faderframe_midi::{MidiClock, MidiOutputEvent, MidiOutputQueue, midi_output_queue};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Messages queued between the engine and the sender thread.
pub const OUTPUT_CAPACITY: usize = 8192;

/// One known output port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiOutputPort {
    pub key: String,
    pub name: String,
    pub index: u16,
    pub enabled: bool,
    pub connected: bool,
    pub is_virtual: bool,
}

/// What a virtual output received: (time sent on the [`MidiClock`], bytes).
pub type Captured = Arc<Mutex<Vec<(u64, Vec<u8>)>>>;

enum Sink {
    Device(midir::MidiOutputConnection),
    Capture(Captured),
}

impl Sink {
    fn send(&mut self, now: u64, bytes: &[u8]) {
        match self {
            Sink::Device(c) => {
                let _ = c.send(bytes);
            }
            Sink::Capture(c) => {
                if let Ok(mut v) = c.lock() {
                    v.push((now, bytes.to_vec()));
                }
            }
        }
    }
}

type Sinks = Arc<Mutex<HashMap<u16, Sink>>>;

/// A queued message: (due, sequence, port, length, bytes).
type Pending = (u64, u64, u16, u8, [u8; 3]);

/// Scheduled SysEx: (due, sequence, port, generation, bytes).
type PendingSysex = (u64, u64, u16, u64, Vec<u8>);

struct Known {
    key: String,
    name: String,
    is_virtual: bool,
}

pub struct MidiOutputs {
    known: Vec<Known>,
    by_key: HashMap<String, u16>,
    sinks: Sinks,
    disabled: HashSet<String>,
    lister: Option<midir::MidiOutput>,
    clock: MidiClock,
    queues: Sender<rtrb::Consumer<MidiOutputEvent>>,
    sysex: Sender<PendingSysex>,
    /// Scheduled SysEx of older generations is dropped.
    generation: Arc<AtomicU64>,
    sysex_seq: u64,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct Channels {
    queues: Receiver<rtrb::Consumer<MidiOutputEvent>>,
    sysex: Receiver<PendingSysex>,
    generation: Arc<AtomicU64>,
}

fn run(sinks: Sinks, clock: MidiClock, ch: Channels, stop: Arc<AtomicBool>) {
    let mut rx: Option<rtrb::Consumer<MidiOutputEvent>> = None;
    // (due, sequence) keeps equal-time messages in arrival order.
    let mut pending: BinaryHeap<Reverse<Pending>> = BinaryHeap::new();
    let mut sysex: BinaryHeap<Reverse<PendingSysex>> = BinaryHeap::new();
    let mut seq = 0u64;
    while !stop.load(Ordering::Relaxed) {
        loop {
            match ch.queues.try_recv() {
                Ok(q) => rx = Some(q),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        while let Ok(x) = ch.sysex.try_recv() {
            sysex.push(Reverse(x));
        }
        let generation = ch.generation.load(Ordering::Acquire);
        if let Some(rx) = rx.as_mut() {
            while let Ok(m) = rx.pop() {
                seq += 1;
                pending.push(Reverse((m.due_ns, seq, m.port, m.len, m.bytes)));
            }
        }
        let now = clock.now_ns();
        if let Ok(mut s) = sinks.lock() {
            while let Some(Reverse((due, _, port, len, bytes))) = pending.peek().copied() {
                // Within 200 µs is on time.
                if due > now + 200_000 {
                    break;
                }
                pending.pop();
                if let Some(sink) = s.get_mut(&port) {
                    sink.send(now, &bytes[..len as usize]);
                }
            }
            while let Some(Reverse((due, _, port, g, _))) = sysex.peek() {
                if *due > now + 200_000 {
                    break;
                }
                let (port, g) = (*port, *g);
                let Some(Reverse((.., data))) = sysex.pop() else {
                    break;
                };
                if g == generation
                    && let Some(sink) = s.get_mut(&port)
                {
                    sink.send(now, &data);
                }
            }
        }
        let next = pending
            .peek()
            .map(|Reverse((due, ..))| *due)
            .into_iter()
            .chain(sysex.peek().map(|Reverse((due, ..))| *due))
            .min();
        let wait = next.map_or(1_000_000, |due| due.saturating_sub(now).min(1_000_000));
        std::thread::sleep(Duration::from_nanos(wait.max(100_000)));
    }
}

impl MidiOutputs {
    /// Outputs on `clock` (shared with the inputs and the engine), with a
    /// running sender thread.
    pub fn new(clock: MidiClock) -> Self {
        let sinks: Sinks = Arc::new(Mutex::new(HashMap::new()));
        let (queues, rx) = channel();
        let (sysex, sysex_rx) = channel();
        let generation = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (sinks, stop) = (Arc::clone(&sinks), Arc::clone(&stop));
            let ch = Channels {
                queues: rx,
                sysex: sysex_rx,
                generation: Arc::clone(&generation),
            };
            std::thread::Builder::new()
                .name("midi-out".into())
                .spawn(move || run(sinks, clock, ch, stop))
                .ok()
        };
        Self {
            known: Vec::new(),
            by_key: HashMap::new(),
            sinks,
            disabled: HashSet::new(),
            lister: None,
            clock,
            queues,
            sysex,
            generation,
            sysex_seq: 0,
            stop,
            thread,
        }
    }

    /// Send a SysEx message (`F0 … F7`) to output `port` at `due_ns` on the
    /// MIDI clock (now if it is past).
    pub fn send_sysex(&mut self, port: u16, due_ns: u64, data: Vec<u8>) {
        self.sysex_seq += 1;
        let g = self.generation.load(Ordering::Acquire);
        let _ = self.sysex.send((due_ns, self.sysex_seq, port, g, data));
    }

    /// Drop every SysEx message scheduled so far that is not sent yet.
    pub fn cancel_sysex(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn clock(&self) -> MidiClock {
        self.clock
    }

    /// A fresh engine-side queue; the sender reads from it from now on.
    pub fn renew_queue(&self) -> MidiOutputQueue {
        let (q, rx) = midi_output_queue(OUTPUT_CAPACITY, self.clock);
        let _ = self.queues.send(rx);
        q
    }

    fn index_for(&mut self, key: &str, name: &str, is_virtual: bool) -> u16 {
        if let Some(&i) = self.by_key.get(key) {
            return i;
        }
        let i = self.known.len() as u16;
        self.known.push(Known {
            key: key.into(),
            name: name.into(),
            is_virtual,
        });
        self.by_key.insert(key.into(), i);
        i
    }

    /// An output that records what it is sent (tests, monitoring).
    pub fn virtual_output(&mut self, name: &str) -> (u16, Captured) {
        let key = format!("virtual:{name}");
        let index = self.index_for(&key, name, true);
        let captured: Captured = Arc::default();
        if let Ok(mut s) = self.sinks.lock() {
            s.insert(index, Sink::Capture(Arc::clone(&captured)));
        }
        (index, captured)
    }

    pub fn start_system(&mut self) {
        if self.lister.is_none() {
            match midir::MidiOutput::new(CLIENT_NAME) {
                Ok(l) => self.lister = Some(l),
                Err(e) => tracing::warn!("MIDI output is unavailable: {e}"),
            }
        }
        self.refresh();
    }

    pub fn stop_system(&mut self) {
        self.lister = None;
        if let Ok(mut s) = self.sinks.lock() {
            s.retain(|_, sink| matches!(sink, Sink::Capture(_)));
        }
    }

    pub fn set_disabled(&mut self, keys: impl IntoIterator<Item = String>) {
        self.disabled = keys.into_iter().collect();
        self.refresh();
    }

    /// Connect new enabled ports, drop vanished or disabled ones.
    pub fn refresh(&mut self) -> bool {
        let Some(lister) = &self.lister else {
            return false;
        };
        let mut present = Vec::new();
        for p in lister.ports() {
            let Ok(full) = lister.port_name(&p) else {
                continue;
            };
            if full.starts_with(CLIENT_NAME) {
                continue;
            }
            let (key, name) = port_identity(&full);
            present.push((key, name, p));
        }
        let mut changed = false;
        let keys: HashSet<String> = present.iter().map(|(k, _, _)| k.clone()).collect();
        let disabled = self.disabled.clone();
        let indices: Vec<(String, u16)> = self
            .known
            .iter()
            .enumerate()
            .filter(|(_, k)| !k.is_virtual)
            .map(|(i, k)| (k.key.clone(), i as u16))
            .collect();
        if let Ok(mut s) = self.sinks.lock() {
            for (key, i) in &indices {
                if s.contains_key(i) && (!keys.contains(key) || disabled.contains(key)) {
                    s.remove(i);
                    changed = true;
                }
            }
        }
        for (key, name, port) in present {
            let index = self.index_for(&key, &name, false);
            let connected = self.sinks.lock().is_ok_and(|s| s.contains_key(&index));
            if connected || self.disabled.contains(&key) {
                continue;
            }
            let out = match midir::MidiOutput::new(CLIENT_NAME) {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!("MIDI output {name}: {e}");
                    continue;
                }
            };
            match out.connect(&port, &format!("{CLIENT_NAME} out: {key}")) {
                Ok(conn) => {
                    tracing::info!("MIDI output connected: {name}");
                    if let Ok(mut s) = self.sinks.lock() {
                        s.insert(index, Sink::Device(conn));
                    }
                    changed = true;
                }
                Err(e) => tracing::warn!("MIDI output {name}: {e}"),
            }
        }
        changed
    }

    pub fn ports(&self) -> Vec<MidiOutputPort> {
        let sinks = self.sinks.lock();
        self.known
            .iter()
            .enumerate()
            .map(|(i, k)| MidiOutputPort {
                key: k.key.clone(),
                name: k.name.clone(),
                index: i as u16,
                enabled: !self.disabled.contains(&k.key),
                connected: sinks.as_ref().is_ok_and(|s| s.contains_key(&(i as u16))),
                is_virtual: k.is_virtual,
            })
            .collect()
    }

    pub fn port_index(&self, key: &str) -> Option<u16> {
        self.by_key.get(key).copied()
    }
}

impl Drop for MidiOutputs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_sent_when_due_in_time_order() {
        let clock = MidiClock::new();
        let mut outs = MidiOutputs::new(clock);
        let (port, captured) = outs.virtual_output("Monitor");
        let mut q = outs.renew_queue();
        let now = clock.now_ns();
        let ms = 1_000_000;
        // Queued out of order: sent by due time.
        for (due, key) in [
            (now + 30 * ms, 64u8),
            (now + 10 * ms, 60),
            (now + 20 * ms, 62),
        ] {
            q.producer
                .push(MidiOutputEvent::new(port, due, &[0x90, key, 100]).unwrap())
                .unwrap();
        }
        // (Whether anything went out early is judged from the send times
        // below: sleeping can overshoot on some systems.)
        std::thread::sleep(Duration::from_millis(65));
        let got = captured.lock().unwrap().clone();
        let keys: Vec<u8> = got.iter().map(|(_, b)| b[1]).collect();
        assert_eq!(keys, vec![60, 62, 64]);
        // Each was sent close to its due time (scheduling slack allowed).
        let first = got[0].0;
        assert!(first + 2 * ms >= now + 10 * ms, "not early");
        assert!(first <= now + 25 * ms, "not very late");
        assert!(outs.ports().iter().any(|p| p.is_virtual && p.connected));
    }

    #[test]
    fn sysex_is_sent_when_due_and_can_be_cancelled() {
        let clock = MidiClock::new();
        let mut outs = MidiOutputs::new(clock);
        let (port, captured) = outs.virtual_output("Synth");
        let ms = 1_000_000;
        let now = clock.now_ns();
        let dump = vec![
            0xF0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x00, 0x41, 0xF7,
        ];
        outs.send_sysex(port, now + 15 * ms, dump.clone());
        outs.send_sysex(port, now + 1000 * ms, vec![0xF0, 1, 0xF7]);
        // (Early sends show in the send times: sleeping can overshoot.)
        std::thread::sleep(Duration::from_millis(60));
        let got = captured.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, dump);
        assert!(got[0].0 + 2 * ms >= now + 15 * ms, "not early");
        // The transport stopped: the later one never goes out.
        outs.cancel_sysex();
        while clock.now_ns() < now + 1100 * ms {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(captured.lock().unwrap().len(), 1);
        // New messages after a cancel are sent.
        outs.send_sysex(port, clock.now_ns(), vec![0xF0, 2, 0xF7]);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(captured.lock().unwrap().len(), 2);
    }
}
