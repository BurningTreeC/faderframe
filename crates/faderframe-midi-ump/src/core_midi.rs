//! CoreMIDI's MIDI 2.0 API (macOS 11 and later), through the `coremidi`
//! crate (which holds the FFI; nothing here is `unsafe`).
//!
//! One CoreMIDI client: an input port speaking MIDI 2.0
//! (`MIDIInputPortCreateWithProtocol`) connected to the MIDI 2.0 sources
//! FaderFrame reads, an output port sending event lists
//! (`MIDISendEventList`) to MIDI 2.0 destinations, and a virtual
//! destination of its own speaking MIDI 2.0 ("MIDI 2.0 In",
//! `MIDIDestinationCreateWithProtocol`) that programs can send to.
//! CoreMIDI calls the input on a thread of its own, so there is no reader
//! thread here. Endpoints have no sequencer addresses: [`Addresses`] gives
//! each one a stable `(client, port)` pair by its unique id — FaderFrame's
//! own destination is `(0, 0)`.

use crate::{Claimed, UmpPort};
use coremidi::{
    Client, Destination, Destinations, EventBuffer, EventList, InputPortWithContext, Object,
    OutputPort, Properties, Protocol, Source, Sources, VirtualDestination,
};
use std::io;
use std::sync::{Arc, Mutex};

/// The address of FaderFrame's own destination (what programs send to).
const OWN_INPUT: (u8, u8) = (0, 0);
/// FaderFrame has no source of its own on CoreMIDI (as on Linux, nothing
/// is published on its output): this address names none.
const OWN_OUTPUT: (u8, u8) = (0, 1);

fn err(what: &str, status: i32) -> io::Error {
    io::Error::other(format!("{what}: CoreMIDI error {status}"))
}

type OnPacket = Box<dyn FnMut((u8, u8), &[u32]) + Send>;

/// Hands what CoreMIDI delivers to the reader [`UmpClient::start_input`]
/// named, packet by packet.
#[derive(Default)]
struct Reader(Mutex<Option<OnPacket>>);

impl Reader {
    fn deliver(&self, from: (u8, u8), list: &EventList) {
        let Ok(mut reader) = self.0.lock() else {
            return;
        };
        let Some(on_packet) = reader.as_mut() else {
            return;
        };
        for p in list.iter() {
            for ump in faderframe_midi::ump::packets(p.data()) {
                on_packet(from, ump.words());
            }
        }
    }
}

/// FaderFrame's MIDI 2.0 CoreMIDI client.
pub struct UmpClient {
    /// Kept for the ports' lifetime (CoreMIDI disposes of a client's ports
    /// with it).
    _client: Client,
    input: Mutex<InputPortWithContext<(u8, u8)>>,
    output: Arc<OutputPort>,
    /// Programs send here; disposed of when the client goes.
    own: VirtualDestination,
    own_id: Option<u32>,
    reader: Arc<Reader>,
    addresses: Mutex<crate::addresses::Addresses>,
}

/// Writes packets to one destination.
pub struct UmpWriter {
    output: Arc<OutputPort>,
    destination: Option<Destination>,
    address: (u8, u8),
}

impl UmpClient {
    /// Open the client (named `name`, see [`crate::CLIENT_NAME`]) with its
    /// ports and its destination.
    pub fn open(name: &str) -> io::Result<Self> {
        let client = Client::new(name).map_err(|s| err("cannot open CoreMIDI", s))?;
        let reader = Arc::new(Reader::default());
        let r = Arc::clone(&reader);
        let input = client
            .input_port_with_protocol(
                &format!("{name} In"),
                Protocol::Midi20,
                move |list: &EventList, from: &mut (u8, u8)| r.deliver(*from, list),
            )
            .map_err(|s| err("MIDI 2.0 input port (macOS 11 or later)", s))?;
        let output = client
            .output_port(&format!("{name} Out"))
            .map_err(|s| err("MIDI 2.0 output port", s))?;
        let r = Arc::clone(&reader);
        let own = client
            .virtual_destination_with_protocol(
                &format!("{name} In"),
                Protocol::Midi20,
                move |list: &EventList| r.deliver(OWN_INPUT, list),
            )
            .map_err(|s| err("MIDI 2.0 destination", s))?;
        let own_id = own.unique_id();
        Ok(Self {
            _client: client,
            input: Mutex::new(input),
            output: Arc::new(output),
            own,
            own_id,
            reader,
            addresses: Mutex::default(),
        })
    }

    /// CoreMIDI has no client numbers.
    pub fn client_id(&self) -> u8 {
        0
    }

    /// The other MIDI 2.0 endpoints (sources readable, destinations
    /// writable), offline and private ones left out.
    pub fn ports(&self) -> Vec<UmpPort> {
        let Ok(mut addresses) = self.addresses.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for s in Sources {
            if let Some(p) = self.port_of(&s, true, &mut addresses) {
                out.push(p);
            }
        }
        for d in Destinations {
            if let Some(p) = self.port_of(&d, false, &mut addresses) {
                out.push(p);
            }
        }
        out
    }

    fn port_of(
        &self,
        e: &Object,
        source: bool,
        addresses: &mut crate::addresses::Addresses,
    ) -> Option<UmpPort> {
        let id = e.unique_id()?;
        let flag = |p: coremidi::BooleanProperty| e.get_property::<bool>(&p).unwrap_or(false);
        if Some(id) == self.own_id
            || !speaks_midi2(e)
            || flag(Properties::offline())
            || flag(Properties::private())
        {
            return None;
        }
        let name = e.display_name().or_else(|| e.name())?;
        let (client, port) = addresses.of(id, source)?;
        Some(UmpPort {
            client,
            port,
            client_name: name,
            port_name: String::new(),
            readable: source,
            writable: !source,
            endpoint: true,
            group: 0,
            // A driver's endpoint (hardware), not a program's.
            hardware: e
                .get_property::<String>(&Properties::driver_owner())
                .is_ok(),
        })
    }

    /// No sequencer clients here (see [`Self::claimed`]).
    pub fn ump_clients(&self) -> Vec<u8> {
        Vec::new()
    }

    /// The MIDI 1.0 ports (as midir names them: by display name) of the
    /// endpoints read as MIDI 2.0.
    pub fn claimed(&self) -> Claimed {
        Claimed {
            clients: Vec::new(),
            names: self.ports().into_iter().map(|p| p.client_name).collect(),
        }
    }

    /// Receive from a source.
    pub fn connect_input(&self, addr: (u8, u8)) -> io::Result<()> {
        let source = self.source(addr)?;
        let mut input = self
            .input
            .lock()
            .map_err(|_| io::Error::other("input port lock"))?;
        input
            .connect_source(&source, addr)
            .map_err(|s| err("cannot connect the MIDI 2.0 input", s))
    }

    pub fn disconnect_input(&self, addr: (u8, u8)) {
        if let (Ok(source), Ok(mut input)) = (self.source(addr), self.input.lock()) {
            let _ = input.disconnect_source(&source);
        }
    }

    fn source(&self, addr: (u8, u8)) -> io::Result<Source> {
        let id = self
            .addresses
            .lock()
            .ok()
            .and_then(|a| a.id(addr))
            .ok_or_else(|| io::Error::other("no such MIDI 2.0 source"))?;
        Source::from_unique_id(id).ok_or_else(|| io::Error::other("the MIDI 2.0 source is gone"))
    }

    /// Call `on_packet` with the sender's address and each packet's words
    /// (on CoreMIDI's thread; once: later calls keep the first reader).
    pub fn start_input(
        &self,
        on_packet: impl FnMut((u8, u8), &[u32]) + Send + 'static,
    ) -> io::Result<()> {
        let mut reader = self
            .reader
            .0
            .lock()
            .map_err(|_| io::Error::other("reader lock"))?;
        if reader.is_none() {
            *reader = Some(Box::new(on_packet));
        }
        Ok(())
    }

    /// A writer to a destination.
    pub fn writer(&self, dest: (u8, u8)) -> UmpWriter {
        let id = self.addresses.lock().ok().and_then(|a| a.id(dest));
        let destination =
            id.and_then(|id| Destinations.into_iter().find(|d| d.unique_id() == Some(id)));
        UmpWriter {
            output: Arc::clone(&self.output),
            destination,
            address: dest,
        }
    }

    /// FaderFrame's own destination (programs send here).
    pub fn input_address(&self) -> (u8, u8) {
        OWN_INPUT
    }

    /// No source of FaderFrame's own (see [`OWN_OUTPUT`]).
    pub fn output_address(&self) -> (u8, u8) {
        OWN_OUTPUT
    }

    /// The name of FaderFrame's own destination as other programs see it.
    pub fn input_name(&self) -> Option<String> {
        self.own.display_name()
    }
}

/// The endpoint's native protocol is MIDI 2.0 (`kMIDIPropertyProtocolID`).
fn speaks_midi2(e: &Object) -> bool {
    e.get_property::<i32>(&Properties::protocol_id()).ok() == Some(2)
}

impl UmpWriter {
    /// Send the packets of `words` now (one event list); `false` if the
    /// destination is gone or CoreMIDI refused them.
    pub fn write(&mut self, words: &[u32]) -> bool {
        let Some(d) = &self.destination else {
            return false;
        };
        let mut list = EventBuffer::new(Protocol::Midi20);
        for p in faderframe_midi::ump::packets(words) {
            list.push(0, p.words());
        }
        self.output.send(d, &list).is_ok()
    }

    pub fn destination(&self) -> (u8, u8) {
        self.address
    }
}
