//! MIDI output devices and the sender thread.
//!
//! The engine queues messages with the time they are due (the moment the
//! audio of the same callback is heard); [`MidiOutputs`] runs a thread that
//! keeps them in time order and sends each when it is due, to every enabled
//! output port (ALSA sequencer via `midir`; MIDI 2.0 ports through the
//! UMP client, with note expressions as per-note controllers) or a virtual
//! capture port.
//! SysEx comes from the control thread instead ([`MidiOutputs::send_sysex`]),
//! scheduled ahead with its due time; scheduled SysEx can be cancelled (the
//! transport stopped or jumped).

use crate::{CLIENT_NAME, UmpClient, port_identity, ump_ports};
use faderframe_midi::ump::Midi1ToMidi2;
use faderframe_midi::{MidiClock, MidiOutputEvent, MidiOutputQueue, midi_output_queue};
use faderframe_midi_ump::UmpWriter;
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

/// What a virtual MIDI 2.0 output received: (time sent, packets' words).
pub type CapturedUmp = Arc<Mutex<Vec<(u64, Vec<u32>)>>>;

enum Sink {
    Device(midir::MidiOutputConnection),
    Capture(Captured),
    Ump(UmpWriter, Midi1ToMidi2),
    UmpCapture(CapturedUmp, Midi1ToMidi2),
}

impl Sink {
    /// Bytes as they are (SysEx, timecode, surfaces' displays).
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
            Sink::Ump(w, t) => {
                let mut words = Vec::new();
                ump_ports::bytes_words(t, bytes, &mut words);
                w.write(&words);
            }
            Sink::UmpCapture(c, t) => {
                let mut words = Vec::new();
                ump_ports::bytes_words(t, bytes, &mut words);
                if let Ok(mut v) = c.lock() {
                    v.push((now, words));
                }
            }
        }
    }

    /// An event of the engine's (expressions only reach MIDI 2.0 ports).
    fn send_event(&mut self, now: u64, ev: &MidiOutputEvent) {
        match self {
            Sink::Device(_) | Sink::Capture(_) => {
                if ev.len > 0 {
                    self.send(now, ev.bytes());
                }
            }
            Sink::Ump(w, t) => {
                let mut words = Vec::with_capacity(4);
                ump_ports::event_words(t, ev, &mut words);
                if !words.is_empty() {
                    w.write(&words);
                }
            }
            Sink::UmpCapture(c, t) => {
                let mut words = Vec::with_capacity(4);
                ump_ports::event_words(t, ev, &mut words);
                if !words.is_empty()
                    && let Ok(mut v) = c.lock()
                {
                    v.push((now, words));
                }
            }
        }
    }

    fn is_virtual(&self) -> bool {
        matches!(self, Sink::Capture(_) | Sink::UmpCapture(..))
    }
}

type Sinks = Arc<Mutex<HashMap<u16, Sink>>>;

/// A queued message, ordered by (due, sequence).
#[derive(Clone, Copy, Debug)]
struct Pending {
    due: u64,
    seq: u64,
    event: MidiOutputEvent,
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        (self.due, self.seq) == (other.due, other.seq)
    }
}

impl Eq for Pending {}

impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Pending {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.due, self.seq).cmp(&(other.due, other.seq))
    }
}

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
    /// The MIDI 2.0 client (the input hub's).
    ump: Option<Arc<UmpClient>>,
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
                pending.push(Reverse(Pending {
                    due: m.due_ns,
                    seq,
                    event: m,
                }));
            }
        }
        let now = clock.now_ns();
        if let Ok(mut s) = sinks.lock() {
            while let Some(Reverse(p)) = pending.peek().copied() {
                // Within 200 µs is on time.
                if p.due > now + 200_000 {
                    break;
                }
                pending.pop();
                if let Some(sink) = s.get_mut(&p.event.port) {
                    sink.send_event(now, &p.event);
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
            .map(|Reverse(p)| p.due)
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
            ump: None,
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

    /// Send a message to output `port` now, past the timed queue (full-
    /// frame timecode, control surfaces' displays).
    pub fn send_now(&self, port: u16, bytes: &[u8]) {
        if let Ok(mut sinks) = self.sinks.lock()
            && let Some(s) = sinks.get_mut(&port)
        {
            s.send(self.clock.now_ns(), bytes);
        }
    }

    /// How many SysEx messages were handed to the sender so far.
    pub fn sysex_scheduled(&self) -> u64 {
        self.sysex_seq
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

    /// An output that records the MIDI 2.0 packets it is sent (tests).
    pub fn virtual_ump_output(&mut self, name: &str) -> (u16, CapturedUmp) {
        let key = format!("virtual:{name}");
        let index = self.index_for(&key, name, true);
        let captured: CapturedUmp = Arc::default();
        if let Ok(mut s) = self.sinks.lock() {
            s.insert(
                index,
                Sink::UmpCapture(Arc::clone(&captured), Midi1ToMidi2::new()),
            );
        }
        (index, captured)
    }

    /// Send to MIDI 2.0 ports through `client` (the input hub's), or not.
    pub fn set_ump(&mut self, client: Option<Arc<UmpClient>>) {
        self.ump = client;
        self.refresh();
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
        self.ump = None;
        if let Ok(mut s) = self.sinks.lock() {
            s.retain(|_, sink| sink.is_virtual());
        }
    }

    pub fn set_disabled(&mut self, keys: impl IntoIterator<Item = String>) {
        self.disabled = keys.into_iter().collect();
        self.refresh();
    }

    /// Connect new enabled ports, drop vanished or disabled ones.
    pub fn refresh(&mut self) -> bool {
        if self.lister.is_none() && self.ump.is_none() {
            return false;
        }
        let lister = self.lister.as_ref();
        let ump_clients: HashSet<u8> = self
            .ump
            .as_ref()
            .map(|c| c.ump_clients().into_iter().collect())
            .unwrap_or_default();
        let mut present = Vec::new();
        let ports = lister.map(midir::MidiOutput::ports).unwrap_or_default();
        for p in ports {
            let Some(Ok(full)) = lister.map(|l| l.port_name(&p)) else {
                continue;
            };
            if full.starts_with(CLIENT_NAME)
                || ump_ports::sequencer_client(&full).is_some_and(|c| ump_clients.contains(&c))
            {
                continue;
            }
            let (key, name) = port_identity(&full);
            present.push((key, name, p));
        }
        let ump_present: Vec<faderframe_midi_ump::UmpPort> = self
            .ump
            .as_ref()
            .map(|c| crate::ump_ports(c, |p| p.writable))
            .unwrap_or_default();
        let mut changed = false;
        let keys: HashSet<String> = present
            .iter()
            .map(|(k, _, _)| k.clone())
            .chain(ump_present.iter().map(faderframe_midi_ump::UmpPort::key))
            .collect();
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
        if let Some(client) = self.ump.clone() {
            for p in ump_present {
                let key = p.key();
                let index = self.index_for(&key, &p.display_name(), false);
                if self.disabled.contains(&key) {
                    continue;
                }
                if let Ok(mut s) = self.sinks.lock()
                    && !s.contains_key(&index)
                {
                    tracing::info!("MIDI 2.0 output connected: {}", p.display_name());
                    s.insert(
                        index,
                        Sink::Ump(client.writer(p.address()), Midi1ToMidi2::new()),
                    );
                    changed = true;
                }
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
