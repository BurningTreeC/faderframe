//! MIDI 2.0 devices: FaderFrame as a Universal MIDI Packet client.
//!
//! On Linux this is a MIDI 2.0 client of the ALSA sequencer
//! ([`UmpClient`]): one input and one output port, the other UMP clients
//! (MIDI 2.0 hardware through the kernel's UMP clients, MIDI 2.0
//! programs) listed as [`UmpPort`]s, packets read on a thread of its own
//! and written straight to a port. The sequencer translates between it and
//! MIDI 1.0 clients, so a MIDI 1.0 sender reaches it as MIDI 2.0 too.
//!
//! On macOS it is a CoreMIDI client (macOS 11 and later): an input port
//! and an output port speaking MIDI 2.0, the MIDI 2.0 endpoints listed as
//! [`UmpPort`]s with addresses of FaderFrame's (`addresses`), and a
//! destination of its own programs send to. Elsewhere (Windows: see
//! docs/ARCHITECTURE.md, "MIDI 2.0") [`UmpClient::open`] reports that
//! there is no MIDI 2.0 here.
//!
//! Packets are plain words ([`faderframe_midi::ump`] reads and writes
//! them); this crate only moves them.

#![deny(unsafe_code)]

// The ALSA C API.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod alsa;

#[cfg(target_os = "linux")]
pub use alsa::{UmpClient, UmpWriter};

// CoreMIDI through the `coremidi` crate (safe code).
#[cfg(target_os = "macos")]
mod core_midi;

#[cfg(target_os = "macos")]
pub use core_midi::{UmpClient, UmpWriter};

#[cfg(any(target_os = "macos", test))]
mod addresses;

/// The client's name in the sequencer.
pub const CLIENT_NAME: &str = "FaderFrame MIDI 2.0";

/// A port of another MIDI 2.0 client.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UmpPort {
    pub client: u8,
    pub port: u8,
    pub client_name: String,
    pub port_name: String,
    /// Packets can be read from it (an input for FaderFrame).
    pub readable: bool,
    /// Packets can be written to it (an output for FaderFrame).
    pub writable: bool,
    /// The port of a whole endpoint (every group), as kernel UMP clients
    /// have for MIDI 2.0 hardware.
    pub endpoint: bool,
    /// The port's group (1–16), 0 for every group.
    pub group: u8,
    /// A kernel client (hardware).
    pub hardware: bool,
}

impl UmpPort {
    /// The port's address as `client:port`.
    pub fn address(&self) -> (u8, u8) {
        (self.client, self.port)
    }

    /// A stable key (the names: client numbers change between boots).
    pub fn key(&self) -> String {
        format!("ump:{}:{}", self.client_name, self.port_name)
    }

    /// The name shown: the endpoint's for an endpoint port, else the
    /// client's and the port's.
    pub fn display_name(&self) -> String {
        if self.endpoint || self.port_name.is_empty() || self.port_name == self.client_name {
            format!("{} (MIDI 2.0)", self.client_name)
        } else {
            format!("{} {} (MIDI 2.0)", self.client_name, self.port_name)
        }
    }
}

/// The sequencer client number at the end of a port's full name as midir
/// gives it on Linux ("MPK mini 3:MPK mini 3 MIDI 1 32:0" → 32).
pub fn sequencer_client(full: &str) -> Option<u8> {
    let (_, tail) = full.rsplit_once(' ')?;
    let (c, p) = tail.split_once(':')?;
    p.parse::<u32>().ok()?;
    c.parse().ok()
}

/// The MIDI 1.0 ports (as midir lists them) of devices read as MIDI 2.0,
/// not to be opened again as MIDI 1.0 ones: by sequencer client on Linux,
/// by name on macOS.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Claimed {
    pub clients: Vec<u8>,
    pub names: Vec<String>,
}

impl Claimed {
    /// A port by its full midir name.
    pub fn contains(&self, full: &str) -> bool {
        sequencer_client(full).is_some_and(|c| self.clients.contains(&c))
            || self.names.iter().any(|n| n == full)
    }
}

/// Which ports of a client list stand for it: its endpoint port when it has
/// one (it carries every group), else every port.
pub fn chosen_ports(ports: &[UmpPort]) -> Vec<UmpPort> {
    let mut out = Vec::new();
    let mut clients: Vec<u8> = ports.iter().map(|p| p.client).collect();
    clients.sort_unstable();
    clients.dedup();
    for c in clients {
        let mine: Vec<&UmpPort> = ports.iter().filter(|p| p.client == c).collect();
        if let Some(ep) = mine.iter().find(|p| p.endpoint) {
            out.push((*ep).clone());
        } else {
            out.extend(mine.into_iter().cloned());
        }
    }
    out
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod none {
    use super::{Claimed, UmpPort};
    use std::io;

    /// No MIDI 2.0 client on this system yet.
    pub struct UmpClient;

    /// Writes nothing (no client).
    pub struct UmpWriter;

    impl UmpClient {
        pub fn open(_name: &str) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "MIDI 2.0 devices are not supported on this system yet",
            ))
        }

        pub fn client_id(&self) -> u8 {
            0
        }

        pub fn ports(&self) -> Vec<UmpPort> {
            Vec::new()
        }

        pub fn ump_clients(&self) -> Vec<u8> {
            Vec::new()
        }

        pub fn claimed(&self) -> Claimed {
            Claimed::default()
        }

        pub fn connect_input(&self, _addr: (u8, u8)) -> io::Result<()> {
            Ok(())
        }

        pub fn disconnect_input(&self, _addr: (u8, u8)) {}

        pub fn start_input(
            &self,
            _on_packet: impl FnMut((u8, u8), &[u32]) + Send + 'static,
        ) -> io::Result<()> {
            Ok(())
        }

        pub fn writer(&self, _dest: (u8, u8)) -> UmpWriter {
            UmpWriter
        }

        pub fn input_address(&self) -> (u8, u8) {
            (0, 0)
        }

        pub fn output_address(&self) -> (u8, u8) {
            (0, 0)
        }
    }

    impl UmpWriter {
        pub fn write(&mut self, _words: &[u32]) -> bool {
            false
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use none::{UmpClient, UmpWriter};

#[cfg(test)]
mod tests {
    use super::*;

    fn port(client: u8, p: u8, endpoint: bool) -> UmpPort {
        UmpPort {
            client,
            port: p,
            client_name: format!("Synth {client}"),
            port_name: if endpoint {
                "MIDI 2.0".into()
            } else {
                format!("Group {p}")
            },
            readable: true,
            writable: true,
            endpoint,
            group: if endpoint { 0 } else { p },
            hardware: true,
        }
    }

    #[test]
    fn an_endpoint_port_stands_for_its_groups() {
        let ports = [
            port(24, 0, true),
            port(24, 1, false),
            port(24, 2, false),
            port(130, 0, false),
            port(130, 1, false),
        ];
        let chosen = chosen_ports(&ports);
        assert_eq!(chosen.len(), 3);
        assert_eq!(chosen[0].address(), (24, 0));
        assert_eq!(chosen[0].display_name(), "Synth 24 (MIDI 2.0)");
        assert_eq!(chosen[1].display_name(), "Synth 130 Group 0 (MIDI 2.0)");
        assert_eq!(chosen[0].key(), "ump:Synth 24:MIDI 2.0");
    }

    #[test]
    fn claimed_ports_go_by_client_or_by_name() {
        let linux = Claimed {
            clients: vec![32],
            names: Vec::new(),
        };
        assert!(linux.contains("MPK mini 3:MPK mini 3 MIDI 1 32:0"));
        assert!(!linux.contains("Midi Through:Midi Through Port-0 14:0"));
        let mac = Claimed {
            clients: Vec::new(),
            names: vec!["Keystage".into()],
        };
        assert!(mac.contains("Keystage") && !mac.contains("Keystage 2"));
        assert_eq!(sequencer_client("Plain"), None);
    }
}
