//! MIDI input devices.
//!
//! [`MidiHub`] finds the system's MIDI inputs (the ALSA sequencer on Linux,
//! via `midir`, which also sees PipeWire's MIDI bridges), connects every
//! enabled one and forwards channel messages, stamped on arrival, into the
//! engine's MIDI queue ([`faderframe_midi::midi_input_queue`]). Ports are
//! identified by name without the sequencer's client:port numbers, so a
//! keyboard keeps its identity (and its enable state, and track routings)
//! when it is unplugged and plugged in again. [`MidiHub::refresh`] picks up
//! such changes; call it now and then from the control thread.
//!
//! [`MidiHub::virtual_input`] adds an input that the program itself feeds —
//! for tests and on-screen keyboards.

#![forbid(unsafe_code)]

use faderframe_midi::MidiInputSender;
use std::collections::{HashMap, HashSet};

/// The sequencer client name FaderFrame shows up as.
pub const CLIENT_NAME: &str = "FaderFrame";

/// One known input port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPort {
    /// Stable identity ("MPK mini 3:MPK mini 3 MIDI 1").
    pub key: String,
    /// What to show ("MPK mini 3 MIDI 1").
    pub name: String,
    /// Index used in MIDI events (stable for the hub's lifetime).
    pub index: u16,
    pub enabled: bool,
    /// Present and receiving.
    pub connected: bool,
    pub is_virtual: bool,
}

/// "MPK mini 3:MPK mini 3 MIDI 1 32:0" → ("MPK mini 3:MPK mini 3 MIDI 1",
/// "MPK mini 3 MIDI 1").
pub fn port_identity(full: &str) -> (String, String) {
    let key = match full.rsplit_once(' ') {
        Some((head, tail))
            if tail
                .split_once(':')
                .is_some_and(|(a, b)| a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok()) =>
        {
            head.to_string()
        }
        _ => full.to_string(),
    };
    let name = match key.split_once(':') {
        Some((_, port)) if !port.is_empty() => port.to_string(),
        _ => key.clone(),
    };
    (key, name)
}

/// Feeds a virtual input port.
#[derive(Clone)]
pub struct VirtualMidiInput {
    port: u16,
    tx: MidiInputSender,
}

impl VirtualMidiInput {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Send one channel message (e.g. `[0x90, 60, 100]`).
    pub fn send(&self, msg: &[u8]) -> bool {
        self.tx.send(self.port, msg)
    }
}

struct Known {
    key: String,
    name: String,
    is_virtual: bool,
}

pub struct MidiHub {
    tx: MidiInputSender,
    known: Vec<Known>,
    by_key: HashMap<String, u16>,
    connections: HashMap<String, midir::MidiInputConnection<()>>,
    disabled: HashSet<String>,
    /// Enumerates ports (connections need their own clients).
    lister: Option<midir::MidiInput>,
    error: Option<String>,
}

impl MidiHub {
    /// A hub delivering into `tx`, with virtual inputs only until
    /// [`Self::start_system`] opens the system's MIDI devices.
    pub fn new(tx: MidiInputSender) -> Self {
        Self {
            tx,
            known: Vec::new(),
            by_key: HashMap::new(),
            connections: HashMap::new(),
            disabled: HashSet::new(),
            lister: None,
            error: None,
        }
    }

    /// Open the system's MIDI inputs (ALSA sequencer) and connect them.
    /// Without a MIDI system the hub keeps working with virtual inputs;
    /// [`Self::error`] says why.
    pub fn start_system(&mut self) {
        if self.lister.is_some() {
            return;
        }
        match midir::MidiInput::new(CLIENT_NAME) {
            Ok(l) => {
                self.lister = Some(l);
                self.error = None;
                self.refresh();
            }
            Err(e) => {
                let e = format!("MIDI is unavailable: {e}");
                tracing::warn!("{e}");
                self.error = Some(e);
            }
        }
    }

    /// Disconnect every system input.
    pub fn stop_system(&mut self) {
        self.connections.clear();
        self.lister = None;
    }

    pub fn system_started(&self) -> bool {
        self.lister.is_some()
    }

    /// Why hardware MIDI is unavailable, if it is.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn index_for(&mut self, key: &str, name: &str, is_virtual: bool) -> u16 {
        if let Some(&i) = self.by_key.get(key) {
            return i;
        }
        let i = self.known.len() as u16;
        self.known.push(Known {
            key: key.to_string(),
            name: name.to_string(),
            is_virtual,
        });
        self.by_key.insert(key.to_string(), i);
        i
    }

    /// An input fed by the program (`key` gets a "virtual:" prefix).
    pub fn virtual_input(&mut self, name: &str) -> VirtualMidiInput {
        let key = format!("virtual:{name}");
        let port = self.index_for(&key, name, true);
        VirtualMidiInput {
            port,
            tx: self.tx.clone(),
        }
    }

    /// Ports (by key) that must not be connected.
    pub fn set_disabled(&mut self, keys: impl IntoIterator<Item = String>) {
        self.disabled = keys.into_iter().collect();
        self.refresh();
    }

    pub fn is_enabled(&self, key: &str) -> bool {
        !self.disabled.contains(key)
    }

    /// Connect new enabled ports, drop vanished or disabled ones. Returns
    /// whether the set of ports or connections changed.
    pub fn refresh(&mut self) -> bool {
        let Some(lister) = &self.lister else {
            return false;
        };
        let mut present: Vec<(String, String, midir::MidiInputPort)> = Vec::new();
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
        let keys: HashSet<&str> = present.iter().map(|(k, _, _)| k.as_str()).collect();
        let before = self.connections.len();
        self.connections
            .retain(|k, _| keys.contains(k.as_str()) && !self.disabled.contains(k));
        changed |= self.connections.len() != before;
        for (key, name, port) in present {
            let index = self.index_for(&key, &name, false);
            if self.connections.contains_key(&key) || self.disabled.contains(&key) {
                continue;
            }
            match self.connect(index, &key, &port) {
                Ok(conn) => {
                    tracing::info!("MIDI input connected: {name}");
                    self.connections.insert(key, conn);
                    changed = true;
                }
                Err(e) => tracing::warn!("MIDI input {name}: {e}"),
            }
        }
        changed
    }

    fn connect(
        &self,
        index: u16,
        key: &str,
        port: &midir::MidiInputPort,
    ) -> Result<midir::MidiInputConnection<()>, String> {
        let mut input = midir::MidiInput::new(CLIENT_NAME).map_err(|e| e.to_string())?;
        // Channel messages only: no SysEx, clock or active sensing.
        input.ignore(midir::Ignore::All);
        let tx = self.tx.clone();
        input
            .connect(
                port,
                &format!("{CLIENT_NAME} in: {key}"),
                move |_, msg, _| {
                    tx.send(index, msg);
                },
                (),
            )
            .map_err(|e| e.to_string())
    }

    /// Every port seen so far (virtual ones included), in index order.
    pub fn ports(&self) -> Vec<MidiPort> {
        self.known
            .iter()
            .enumerate()
            .map(|(i, k)| MidiPort {
                key: k.key.clone(),
                name: k.name.clone(),
                index: i as u16,
                enabled: !self.disabled.contains(&k.key),
                connected: k.is_virtual || self.connections.contains_key(&k.key),
                is_virtual: k.is_virtual,
            })
            .collect()
    }

    pub fn port_index(&self, key: &str) -> Option<u16> {
        self.by_key.get(key).copied()
    }

    /// Port key of an index.
    pub fn port_key(&self, index: u16) -> Option<&str> {
        self.known.get(index as usize).map(|k| k.key.as_str())
    }

    /// Messages lost because the audio thread did not keep up.
    pub fn dropped(&self) -> u64 {
        self.tx.dropped()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_names_lose_their_sequencer_numbers() {
        assert_eq!(
            port_identity("MPK mini 3:MPK mini 3 MIDI 1 32:0"),
            (
                "MPK mini 3:MPK mini 3 MIDI 1".to_string(),
                "MPK mini 3 MIDI 1".to_string()
            )
        );
        assert_eq!(
            port_identity("Plain Name"),
            ("Plain Name".to_string(), "Plain Name".to_string())
        );
        assert_eq!(port_identity("Dev:Port 1 x:y").0, "Dev:Port 1 x:y");
    }

    #[test]
    fn virtual_inputs_feed_the_engine_queue() {
        let (tx, mut rx, _feed) = faderframe_midi::midi_input_queue(16);
        let mut hub = MidiHub::new(tx);
        let keys = hub.virtual_input("Keys");
        let pads = hub.virtual_input("Pads");
        assert_ne!(keys.port(), pads.port());
        assert_eq!(
            hub.virtual_input("Keys").port(),
            keys.port(),
            "stable index"
        );
        assert!(keys.send(&[0x90, 64, 90]));
        assert!(pads.send(&[0xB0, 7, 100]));
        let a = rx.consumer.pop().unwrap();
        let b = rx.consumer.pop().unwrap();
        assert_eq!((a.port, b.port), (keys.port(), pads.port()));
        let ports = hub.ports();
        assert!(
            ports
                .iter()
                .any(|p| p.key == "virtual:Keys" && p.connected && p.is_virtual)
        );
        assert_eq!(hub.port_key(pads.port()), Some("virtual:Pads"));
    }
}
