//! The ALSA sequencer's UMP API (alsa-lib 1.2.10 and later).
//!
//! One sequencer handle, behind a mutex: the input thread waits in
//! `poll` without it and takes it only to read what arrived; writers and
//! the control thread take it for their calls (alsa-lib's handles are not
//! thread-safe).

use crate::UmpPort;
use alsa_sys as a;
use std::ffi::{CStr, CString};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

// From alsa/seq.h and alsa/seq_event.h (macros bindgen leaves out).
const SND_SEQ_OPEN_DUPLEX: i32 = 3;
const SND_SEQ_NONBLOCK: i32 = 1;
const SND_SEQ_CLIENT_UMP_MIDI_2_0: i32 = 2;
const CAP_READ: u32 = 1 << 0;
const CAP_WRITE: u32 = 1 << 1;
const CAP_SUBS_READ: u32 = 1 << 5;
const CAP_SUBS_WRITE: u32 = 1 << 6;
const CAP_NO_EXPORT: u32 = 1 << 7;
const CAP_INACTIVE: u32 = 1 << 8;
const CAP_UMP_ENDPOINT: u32 = 1 << 9;
const TYPE_MIDI_GENERIC: u32 = 1 << 1;
const TYPE_MIDI_UMP: u32 = 1 << 7;
const TYPE_APPLICATION: u32 = 1 << 20;
const EVENT_UMP: u8 = 1 << 5;
const QUEUE_DIRECT: u8 = 253;

/// The sequencer handle.
struct Seq(*mut a::snd_seq_t);

// SAFETY: the handle is only used behind the client's mutex, one thread at
// a time, which is what alsa-lib asks of a handle.
unsafe impl Send for Seq {}

impl Drop for Seq {
    fn drop(&mut self) {
        // SAFETY: the handle came from `snd_seq_open` and is closed once.
        unsafe {
            a::snd_seq_close(self.0);
        }
    }
}

fn check(r: i32, what: &str) -> io::Result<i32> {
    if r < 0 {
        Err(io::Error::other(format!(
            "{what}: {}",
            io::Error::from_raw_os_error(-r)
        )))
    } else {
        Ok(r)
    }
}

struct Shared {
    seq: Mutex<Seq>,
    stop: AtomicBool,
}

/// FaderFrame's MIDI 2.0 sequencer client.
pub struct UmpClient {
    shared: Arc<Shared>,
    id: u8,
    in_port: i32,
    out_port: i32,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// Writes packets to one port.
pub struct UmpWriter {
    shared: Arc<Shared>,
    source: u8,
    dest: (u8, u8),
}

impl UmpClient {
    /// Open the client (named `name`, see [`crate::CLIENT_NAME`]) with its
    /// ports.
    pub fn open(name: &str) -> io::Result<Self> {
        let mut handle: *mut a::snd_seq_t = std::ptr::null_mut();
        let default = c"default";
        // SAFETY: `handle` is a valid out pointer, the name a C string.
        check(
            unsafe {
                a::snd_seq_open(
                    &mut handle,
                    default.as_ptr(),
                    SND_SEQ_OPEN_DUPLEX,
                    SND_SEQ_NONBLOCK,
                )
            },
            "cannot open the ALSA sequencer",
        )?;
        let seq = Seq(handle);
        let cname = CString::new(name).unwrap_or_else(|_| CString::from(c"FaderFrame MIDI 2.0"));
        // SAFETY: `seq.0` is an open handle; the strings outlive the calls.
        let (id, in_port, out_port) = unsafe {
            check(
                a::snd_seq_set_client_midi_version(seq.0, SND_SEQ_CLIENT_UMP_MIDI_2_0),
                "this ALSA has no MIDI 2.0 (alsa-lib 1.2.10 and a 6.5 kernel are needed)",
            )?;
            check(
                a::snd_seq_set_client_name(seq.0, cname.as_ptr()),
                "client name",
            )?;
            let kind = TYPE_MIDI_GENERIC | TYPE_MIDI_UMP | TYPE_APPLICATION;
            let in_port = check(
                a::snd_seq_create_simple_port(
                    seq.0,
                    c"MIDI 2.0 In".as_ptr(),
                    CAP_WRITE | CAP_SUBS_WRITE,
                    kind,
                ),
                "input port",
            )?;
            let out_port = check(
                a::snd_seq_create_simple_port(
                    seq.0,
                    c"MIDI 2.0 Out".as_ptr(),
                    CAP_READ | CAP_SUBS_READ,
                    kind,
                ),
                "output port",
            )?;
            let id = check(a::snd_seq_client_id(seq.0), "client id")?;
            (id as u8, in_port, out_port)
        };
        Ok(Self {
            shared: Arc::new(Shared {
                seq: Mutex::new(seq),
                stop: AtomicBool::new(false),
            }),
            id,
            in_port,
            out_port,
            thread: Mutex::new(None),
        })
    }

    /// This client's number.
    pub fn client_id(&self) -> u8 {
        self.id
    }

    /// The ports of the other MIDI 2.0 (UMP) clients, inactive ones and
    /// ones closed to routing left out.
    pub fn ports(&self) -> Vec<UmpPort> {
        let Ok(seq) = self.shared.seq.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // SAFETY: the infos are allocated and freed here; the handle is
        // open and held under the mutex; names are C strings owned by the
        // infos, copied before they are freed.
        unsafe {
            let mut client: *mut a::snd_seq_client_info_t = std::ptr::null_mut();
            let mut port: *mut a::snd_seq_port_info_t = std::ptr::null_mut();
            if a::snd_seq_client_info_malloc(&mut client) < 0 {
                return out;
            }
            if a::snd_seq_port_info_malloc(&mut port) < 0 {
                a::snd_seq_client_info_free(client);
                return out;
            }
            a::snd_seq_client_info_set_client(client, -1);
            while a::snd_seq_query_next_client(seq.0, client) >= 0 {
                let c = a::snd_seq_client_info_get_client(client);
                if c == i32::from(self.id) || a::snd_seq_client_info_get_midi_version(client) < 1 {
                    continue;
                }
                let client_name = CStr::from_ptr(a::snd_seq_client_info_get_name(client))
                    .to_string_lossy()
                    .trim()
                    .to_string();
                let hardware = a::snd_seq_client_info_get_type(client) == a::SND_SEQ_KERNEL_CLIENT;
                a::snd_seq_port_info_set_client(port, c);
                a::snd_seq_port_info_set_port(port, -1);
                while a::snd_seq_query_next_port(seq.0, port) >= 0 {
                    let caps = a::snd_seq_port_info_get_capability(port);
                    if caps & (CAP_NO_EXPORT | CAP_INACTIVE) != 0 {
                        continue;
                    }
                    let readable = caps & (CAP_READ | CAP_SUBS_READ) == CAP_READ | CAP_SUBS_READ;
                    let writable = caps & CAP_WRITE != 0;
                    if !readable && !writable {
                        continue;
                    }
                    let port_name = CStr::from_ptr(a::snd_seq_port_info_get_name(port))
                        .to_string_lossy()
                        .trim()
                        .to_string();
                    out.push(UmpPort {
                        client: c as u8,
                        port: a::snd_seq_port_info_get_port(port) as u8,
                        client_name: client_name.clone(),
                        port_name,
                        readable,
                        writable,
                        endpoint: caps & CAP_UMP_ENDPOINT != 0,
                        group: a::snd_seq_port_info_get_ump_group(port).clamp(0, 16) as u8,
                        hardware,
                    });
                }
            }
            a::snd_seq_port_info_free(port);
            a::snd_seq_client_info_free(client);
        }
        out
    }

    /// The numbers of the MIDI 2.0 clients (their ports are MIDI 2.0 ports,
    /// not to be opened again as MIDI 1.0 ones).
    pub fn ump_clients(&self) -> Vec<u8> {
        let mut c: Vec<u8> = self.ports().iter().map(|p| p.client).collect();
        c.push(self.id);
        c.sort_unstable();
        c.dedup();
        c
    }

    /// Receive from a port.
    pub fn connect_input(&self, addr: (u8, u8)) -> io::Result<()> {
        let seq = self
            .shared
            .seq
            .lock()
            .map_err(|_| io::Error::other("sequencer lock"))?;
        // SAFETY: the handle is open and held under the mutex.
        check(
            unsafe {
                a::snd_seq_connect_from(seq.0, self.in_port, i32::from(addr.0), i32::from(addr.1))
            },
            "cannot connect the MIDI 2.0 input",
        )
        .map(|_| ())
    }

    pub fn disconnect_input(&self, addr: (u8, u8)) {
        if let Ok(seq) = self.shared.seq.lock() {
            // SAFETY: the handle is open and held under the mutex.
            unsafe {
                a::snd_seq_disconnect_from(
                    seq.0,
                    self.in_port,
                    i32::from(addr.0),
                    i32::from(addr.1),
                );
            }
        }
    }

    /// Read packets on a thread of its own, calling `on_packet` with the
    /// sender's address and each packet's words (once; later calls keep
    /// the first reader).
    pub fn start_input(
        &self,
        mut on_packet: impl FnMut((u8, u8), &[u32]) + Send + 'static,
    ) -> io::Result<()> {
        let mut thread = self
            .thread
            .lock()
            .map_err(|_| io::Error::other("thread lock"))?;
        if thread.is_some() {
            return Ok(());
        }
        let shared = Arc::clone(&self.shared);
        let fds = {
            let seq = shared
                .seq
                .lock()
                .map_err(|_| io::Error::other("sequencer lock"))?;
            // SAFETY: the handle is open; the vector has room for `n`.
            unsafe {
                let n = a::snd_seq_poll_descriptors_count(seq.0, libc::POLLIN);
                let mut fds = vec![
                    libc::pollfd {
                        fd: -1,
                        events: 0,
                        revents: 0
                    };
                    n.max(0) as usize
                ];
                let got = a::snd_seq_poll_descriptors(
                    seq.0,
                    fds.as_mut_ptr(),
                    fds.len() as u32,
                    libc::POLLIN,
                );
                fds.truncate(got.max(0) as usize);
                fds
            }
        };
        let handle = std::thread::Builder::new()
            .name("faderframe-ump-in".into())
            .spawn(move || {
                let mut fds = fds;
                while !shared.stop.load(Ordering::Relaxed) {
                    // SAFETY: `fds` holds `len` initialised descriptors.
                    let ready =
                        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 100) };
                    if ready <= 0 {
                        continue;
                    }
                    let Ok(seq) = shared.seq.lock() else {
                        return;
                    };
                    loop {
                        let mut ev: *mut a::snd_seq_ump_event_t = std::ptr::null_mut();
                        // SAFETY: the handle is open and held; on success
                        // `ev` points at an event alsa-lib keeps valid
                        // until the next input call, read before it.
                        let r = unsafe { a::snd_seq_ump_event_input(seq.0, &mut ev) };
                        if r < 0 || ev.is_null() {
                            break;
                        }
                        // SAFETY: as above.
                        let e = unsafe { &*ev };
                        if e.flags & EVENT_UMP == 0 {
                            continue;
                        }
                        // SAFETY: a UMP event's union holds its words.
                        let words = unsafe { e.__bindgen_anon_1.ump };
                        let n = faderframe_midi::ump::words_for((words[0] >> 28) as u8);
                        on_packet((e.source.client, e.source.port), &words[..n]);
                    }
                }
            })?;
        *thread = Some(handle);
        Ok(())
    }

    /// A writer to a port.
    pub fn writer(&self, dest: (u8, u8)) -> UmpWriter {
        UmpWriter {
            shared: Arc::clone(&self.shared),
            source: self.out_port as u8,
            dest,
        }
    }

    /// The client's own address for its input port (others send here).
    pub fn input_address(&self) -> (u8, u8) {
        (self.id, self.in_port as u8)
    }

    /// The client's own address for its output port.
    pub fn output_address(&self) -> (u8, u8) {
        (self.id, self.out_port as u8)
    }
}

impl Drop for UmpClient {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.get_mut().ok().and_then(Option::take) {
            let _ = t.join();
        }
    }
}

impl UmpWriter {
    /// Write the packets of `words` now; `false` if one was refused.
    pub fn write(&mut self, words: &[u32]) -> bool {
        let Ok(seq) = self.shared.seq.lock() else {
            return false;
        };
        let mut ok = true;
        for p in faderframe_midi::ump::packets(words) {
            // SAFETY: an all-zero event is a valid plain value of this C
            // struct; the fields set below make it a direct UMP event.
            let mut ev: a::snd_seq_ump_event_t = unsafe { std::mem::zeroed() };
            ev.flags = EVENT_UMP;
            ev.queue = QUEUE_DIRECT;
            ev.source.port = self.source;
            ev.dest.client = self.dest.0;
            ev.dest.port = self.dest.1;
            let mut w = [0u32; 4];
            w[..p.words().len()].copy_from_slice(p.words());
            ev.__bindgen_anon_1.ump = w;
            // SAFETY: the handle is open and held; the event is valid.
            ok &= unsafe { a::snd_seq_ump_event_output_direct(seq.0, &mut ev) } >= 0;
        }
        ok
    }

    pub fn destination(&self) -> (u8, u8) {
        self.dest
    }
}
