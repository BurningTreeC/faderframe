//! FaderFrame's own MIDI 2.0 destination on CoreMIDI, made here rather
//! than with `coremidi::Client::virtual_destination_with_protocol`: that
//! one turns the source-connection context CoreMIDI passes with every event
//! list into a `&mut ()`, and a destination has no connection — the pointer
//! is null, undefined behaviour that aborts a debug build at the first
//! message ("null reference produced", coremidi 0.9.2's
//! `Client::receive_block`). Here the block reads the event list only.

use block2::RcBlock;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use coremidi_sys::{
    MIDIClientCreate, MIDIClientRef, MIDIDestinationCreateWithProtocol, MIDIEndpointDispose,
    MIDIEndpointRef, MIDIEventList, MIDIEventPacket, MIDIEventPacketNext,
    MIDIObjectGetIntegerProperty, MIDIProtocolID, MIDIReceiveBlock, kMIDIPropertyUniqueID,
    kMIDIProtocol_2_0,
};
use std::ffi::c_void;
use std::ptr::addr_of;

/// A MIDI 2.0 destination of FaderFrame's own (on a CoreMIDI client of its
/// own); what programs send to it reaches `deliver` packet by packet, on
/// CoreMIDI's thread. Disposed of when dropped.
pub(crate) struct OwnDestination {
    endpoint: MIDIEndpointRef,
}

impl OwnDestination {
    /// Make the destination `name` (its client named `client`).
    pub(crate) fn create(
        client: &str,
        name: &str,
        deliver: impl Fn(&[u32]) + Send + Sync + 'static,
    ) -> Result<Self, i32> {
        let client_name = CFString::new(client);
        let mut client_ref: MIDIClientRef = 0;
        // SAFETY: a valid name, no notification callback, and an out
        // pointer to a live local. The client is never disposed of (Apple:
        // disposing of a program's last client may stop the MIDI server
        // for it); CoreMIDI disposes of it when the program ends.
        let status = unsafe {
            MIDIClientCreate(
                client_name.as_concrete_TypeRef(),
                None,
                std::ptr::null_mut(),
                &mut client_ref,
            )
        };
        if status != 0 {
            return Err(status);
        }
        let block = RcBlock::new(move |list: *const c_void, _context: *mut c_void| {
            if list.is_null() {
                return;
            }
            // SAFETY: CoreMIDI hands a valid event list for the length of
            // the call: `numPackets` packets one after another, each
            // `wordCount` words long (`MIDIEventPacketNext` steps over
            // them). Packets are 4-byte aligned (their sizes are whole
            // words), so their words can be read as a `[u32]`; the packed
            // fields are read unaligned.
            unsafe {
                let list = list.cast::<MIDIEventList>();
                let count = addr_of!((*list).numPackets).read_unaligned();
                let mut packet = addr_of!((*list).packet).cast::<MIDIEventPacket>();
                for _ in 0..count {
                    let n = addr_of!((*packet).wordCount).read_unaligned() as usize;
                    let words =
                        std::slice::from_raw_parts(addr_of!((*packet).words).cast::<u32>(), n);
                    deliver(words);
                    packet = MIDIEventPacketNext(packet);
                }
            }
        });
        let name = CFString::new(name);
        let mut endpoint: MIDIEndpointRef = 0;
        // SAFETY: the client made above, a valid name, an out pointer to a
        // live local and a heap block; CoreMIDI keeps its own reference to
        // the block (ours goes at the end of this call, as the coremidi
        // crate's does).
        let status = unsafe {
            MIDIDestinationCreateWithProtocol(
                client_ref,
                name.as_concrete_TypeRef(),
                kMIDIProtocol_2_0 as MIDIProtocolID,
                &mut endpoint,
                &*block as *const _ as MIDIReceiveBlock,
            )
        };
        if status != 0 {
            return Err(status);
        }
        Ok(Self { endpoint })
    }

    /// Its unique id (how other programs and FaderFrame's addresses know
    /// it).
    pub(crate) fn unique_id(&self) -> Option<u32> {
        let mut id = 0i32;
        // SAFETY: a live endpoint, CoreMIDI's own property name (a static
        // it exports) and an out pointer to a live local.
        let status =
            unsafe { MIDIObjectGetIntegerProperty(self.endpoint, kMIDIPropertyUniqueID, &mut id) };
        (status == 0).then_some(id as u32)
    }
}

impl Drop for OwnDestination {
    fn drop(&mut self) {
        // SAFETY: the endpoint made in `create`, disposed of once.
        unsafe {
            MIDIEndpointDispose(self.endpoint);
        }
    }
}
