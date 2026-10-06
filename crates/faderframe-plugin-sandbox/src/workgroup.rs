//! The audio device's workgroup in helper processes (macOS).
//!
//! FaderFrame's DSP workers join the CoreAudio device's audio workgroup
//! (`faderframe_realtime::Workgroup`), so the scheduler treats them as part
//! of the IO thread's realtime work. A helper's audio thread does the same
//! work for its plugin, so it joins too. Workgroups cross processes as Mach
//! ports, which a socket cannot carry; so FaderFrame checks in a bootstrap
//! service of its own (named after its process id, given to helpers in
//! [`SERVICE_ENV`]) whose thread answers each request with a send right to
//! the current workgroup (`os_workgroup_copy_port`), and a helper looks the
//! service up and asks — on a thread of its own, never the audio thread.
//!
//! The host's audio thread writes the process's workgroup generation into
//! the shared block (`Header::wg_gen`) with every request; a helper whose
//! audio thread sees a new one wakes its [`Fetcher`], which fetches the
//! workgroup (`os_workgroup_create_with_port`) and hands it over; the audio
//! thread leaves the old one and joins it at the start of a block, and
//! reports the generation it joined (`Header::wg_joined`). Elsewhere all of
//! this does nothing.

use faderframe_realtime::Workgroup;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The environment variable naming FaderFrame's workgroup service.
pub const SERVICE_ENV: &str = "FADERFRAME_WORKGROUP_SERVICE";

/// Highest workgroup generation a helper reported joining (tests).
static JOINED: AtomicU64 = AtomicU64::new(0);

/// The highest workgroup generation a helper's audio thread joined (0:
/// none yet).
pub fn helpers_joined() -> u64 {
    JOINED.load(Ordering::Acquire)
}

/// A helper reported joining generation `generation` (host audio thread).
pub(crate) fn note_joined(generation: u32) {
    JOINED.fetch_max(u64::from(generation), Ordering::AcqRel);
}

/// FaderFrame's side: the service helpers ask, started on first use
/// (`None`: no workgroups here, or the service could not be made). Unix
/// helpers are told it; Windows has no workgroups.
#[cfg(unix)]
pub(crate) fn service_name() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        NAME.get_or_init(mach::serve).as_deref()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// A helper's side: fetches the workgroup for its audio thread.
pub(crate) struct Fetcher {
    /// The generation the audio thread wants.
    want: AtomicU32,
    /// The fetched workgroup and its generation.
    slot: Mutex<(u32, Option<Workgroup>)>,
    thread: Option<std::thread::Thread>,
}

impl Fetcher {
    /// Start fetching (in a helper FaderFrame gave a service to; otherwise
    /// a fetcher that never fetches).
    pub(crate) fn start() -> Arc<Self> {
        let service = std::env::var(SERVICE_ENV).ok().filter(|s| !s.is_empty());
        Arc::new_cyclic(|weak: &std::sync::Weak<Self>| {
            let thread = service.and_then(|service| {
                let weak = weak.clone();
                std::thread::Builder::new()
                    .name("faderframe-workgroup".into())
                    .spawn(move || fetch_loop(&weak, &service))
                    .ok()
                    .map(|h| h.thread().clone())
            });
            Self {
                want: AtomicU32::new(0),
                slot: Mutex::new((0, None)),
                thread,
            }
        })
    }

    /// The audio thread wants generation `generation` (realtime-safe).
    pub(crate) fn want(&self, generation: u32) {
        if self.want.swap(generation, Ordering::AcqRel) != generation
            && let Some(t) = &self.thread
        {
            t.unpark();
        }
    }

    /// A workgroup fetched for a generation other than `seen` (audio
    /// thread; never waits).
    pub(crate) fn take(&self, seen: u32) -> Option<(u32, Option<Workgroup>)> {
        let mut slot = self.slot.try_lock().ok()?;
        if slot.0 == seen || slot.0 == 0 {
            return None;
        }
        let generation = slot.0;
        Some((generation, slot.1.take()))
    }
}

fn fetch_loop(fetcher: &std::sync::Weak<Fetcher>, service: &str) {
    let mut done = 0u32;
    loop {
        std::thread::park_timeout(std::time::Duration::from_millis(500));
        let Some(f) = fetcher.upgrade() else {
            return;
        };
        let want = f.want.load(Ordering::Acquire);
        if want == 0 || want == done {
            continue;
        }
        let wg = fetch(service);
        if let Ok(mut slot) = f.slot.lock() {
            *slot = (want, wg);
        }
        done = want;
    }
}

/// The workgroup FaderFrame's service hands out now.
fn fetch(service: &str) -> Option<Workgroup> {
    #[cfg(target_os = "macos")]
    {
        mach::request(service)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = service;
        None
    }
}

/// The audio thread's membership (see the module docs).
#[derive(Default)]
pub(crate) struct AudioMembership {
    want: u32,
    seen: u32,
    joined: Option<(Workgroup, faderframe_realtime::Membership)>,
}

impl AudioMembership {
    /// Before a block: follow the host's generation `generation`; returns the
    /// generation joined now, if it changed (0: none).
    pub(crate) fn update(&mut self, generation: u32, fetcher: &Fetcher) -> Option<u32> {
        if generation != self.want {
            self.want = generation;
            fetcher.want(generation);
        }
        let (got, wg) = fetcher.take(self.seen)?;
        self.seen = got;
        self.leave();
        let joined = wg.and_then(|w| w.join().map(|m| (w, m)));
        let reported = if joined.is_some() { got } else { 0 };
        self.joined = joined;
        Some(reported)
    }

    pub(crate) fn leave(&mut self) {
        if let Some((w, m)) = self.joined.take() {
            w.leave(m);
        }
    }
}

impl Drop for AudioMembership {
    fn drop(&mut self) {
        self.leave();
    }
}

#[cfg(target_os = "macos")]
mod mach {
    //! The bootstrap service and its one request (see the module docs).

    use faderframe_realtime::Workgroup;
    use std::ffi::{CString, c_char};

    type Port = u32;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Header {
        bits: u32,
        size: u32,
        remote: Port,
        local: Port,
        voucher: Port,
        id: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct PortDescriptor {
        name: Port,
        pad1: u32,
        /// pad2 (16 bits), disposition (8), type (8).
        bits: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Reply {
        header: Header,
        descriptors: u32,
        port: PortDescriptor,
    }

    /// Room for a message and the trailer the kernel appends.
    #[repr(C)]
    struct Buffer {
        reply: Reply,
        trailer: [u8; 128],
    }

    const SEND_MSG: i32 = 0x1;
    const RCV_MSG: i32 = 0x2;
    const SEND_TIMEOUT: i32 = 0x10;
    const RCV_TIMEOUT: i32 = 0x100;
    const TYPE_MOVE_SEND: u32 = 17;
    const TYPE_MOVE_SEND_ONCE: u32 = 18;
    const TYPE_COPY_SEND: u32 = 19;
    const TYPE_MAKE_SEND_ONCE: u32 = 21;
    const BITS_COMPLEX: u32 = 0x8000_0000;
    const PORT_DESCRIPTOR: u32 = 0;
    const RIGHT_RECEIVE: u32 = 1;
    const ID_REQUEST: i32 = 0x4657_4731;
    const ID_WORKGROUP: i32 = 0x4657_4732;
    const ID_NONE: i32 = 0x4657_4730;

    unsafe extern "C" {
        static bootstrap_port: Port;
        static mach_task_self_: Port;
        fn bootstrap_check_in(bp: Port, name: *const c_char, sp: *mut Port) -> i32;
        fn bootstrap_look_up(bp: Port, name: *const c_char, sp: *mut Port) -> i32;
        fn mach_port_allocate(task: Port, right: u32, name: *mut Port) -> i32;
        fn mach_port_mod_refs(task: Port, name: Port, right: u32, delta: i32) -> i32;
        fn mach_port_deallocate(task: Port, name: Port) -> i32;
        fn mach_msg(
            msg: *mut Header,
            option: i32,
            send_size: u32,
            rcv_size: u32,
            rcv_name: Port,
            timeout: u32,
            notify: Port,
        ) -> i32;
    }

    fn task() -> Port {
        // SAFETY: a process-wide constant set up by libSystem.
        unsafe { mach_task_self_ }
    }

    const fn bits(remote: u32, local: u32) -> u32 {
        remote | local << 8
    }

    /// Check in the service and answer requests on a thread; its name.
    pub(super) fn serve() -> Option<String> {
        let name = format!(
            "io.github.BurningTreeC.FaderFrame.workgroup.{}",
            std::process::id()
        );
        let c = CString::new(name.clone()).ok()?;
        let mut port: Port = 0;
        // SAFETY: a valid name; the receive right is written on success.
        let r = unsafe { bootstrap_check_in(bootstrap_port, c.as_ptr(), &mut port) };
        if r != 0 {
            tracing::debug!("workgroup service: bootstrap_check_in failed ({r})");
            return None;
        }
        std::thread::Builder::new()
            .name("faderframe-workgroup-service".into())
            .spawn(move || answer(port))
            .ok()?;
        Some(name)
    }

    /// Answer requests on `port` forever.
    fn answer(port: Port) {
        loop {
            let mut buf = Buffer {
                reply: Reply::default(),
                trailer: [0; 128],
            };
            // SAFETY: receives into a buffer of the size given.
            let r = unsafe {
                mach_msg(
                    &mut buf.reply.header,
                    RCV_MSG,
                    0,
                    std::mem::size_of::<Buffer>() as u32,
                    port,
                    0,
                    0,
                )
            };
            if r != 0 {
                continue;
            }
            let request = buf.reply.header;
            if request.id != ID_REQUEST || request.remote == 0 {
                continue;
            }
            // A send right to the current workgroup, moved into the reply.
            let wg_port = faderframe_realtime::process_workgroup().and_then(|w| w.copy_port());
            let mut reply = Reply {
                header: Header {
                    remote: request.remote,
                    ..Header::default()
                },
                ..Reply::default()
            };
            match wg_port {
                Some(p) => {
                    reply.header.bits = bits(TYPE_MOVE_SEND_ONCE, 0) | BITS_COMPLEX;
                    reply.header.size = std::mem::size_of::<Reply>() as u32;
                    reply.header.id = ID_WORKGROUP;
                    reply.descriptors = 1;
                    reply.port = PortDescriptor {
                        name: p,
                        pad1: 0,
                        bits: TYPE_MOVE_SEND << 16 | PORT_DESCRIPTOR << 24,
                    };
                }
                None => {
                    reply.header.bits = bits(TYPE_MOVE_SEND_ONCE, 0);
                    reply.header.size = std::mem::size_of::<Header>() as u32;
                    reply.header.id = ID_NONE;
                }
            }
            let size = reply.header.size;
            // SAFETY: sends a well-formed message (its rights are ours).
            let r = unsafe {
                mach_msg(
                    &mut reply.header,
                    SEND_MSG | SEND_TIMEOUT,
                    size,
                    0,
                    0,
                    1000,
                    0,
                )
            };
            if r != 0
                && let Some(p) = wg_port
            {
                // Not sent: the right is still ours.
                // SAFETY: a send right we hold.
                unsafe { mach_port_deallocate(task(), p) };
            }
        }
    }

    /// Ask the service `name` for the workgroup.
    pub(super) fn request(name: &str) -> Option<Workgroup> {
        let c = CString::new(name).ok()?;
        let mut server: Port = 0;
        // SAFETY: a valid name; a send right is written on success.
        let r = unsafe { bootstrap_look_up(bootstrap_port, c.as_ptr(), &mut server) };
        if r != 0 {
            tracing::debug!("workgroup service: bootstrap_look_up failed ({r})");
            return None;
        }
        let mut reply_port: Port = 0;
        // SAFETY: allocates a receive right for the reply.
        if unsafe { mach_port_allocate(task(), RIGHT_RECEIVE, &mut reply_port) } != 0 {
            // SAFETY: a send right we hold.
            unsafe { mach_port_deallocate(task(), server) };
            return None;
        }
        let mut buf = Buffer {
            reply: Reply::default(),
            trailer: [0; 128],
        };
        buf.reply.header = Header {
            bits: bits(TYPE_COPY_SEND, TYPE_MAKE_SEND_ONCE),
            size: std::mem::size_of::<Header>() as u32,
            remote: server,
            local: reply_port,
            voucher: 0,
            id: ID_REQUEST,
        };
        // SAFETY: sends the request and receives the answer into a buffer
        // of the size given (two seconds at most).
        let r = unsafe {
            mach_msg(
                &mut buf.reply.header,
                SEND_MSG | RCV_MSG | SEND_TIMEOUT | RCV_TIMEOUT,
                std::mem::size_of::<Header>() as u32,
                std::mem::size_of::<Buffer>() as u32,
                reply_port,
                2000,
                0,
            )
        };
        let got = (r == 0
            && buf.reply.header.id == ID_WORKGROUP
            && buf.reply.header.bits & BITS_COMPLEX != 0
            && buf.reply.descriptors == 1)
            .then_some(buf.reply.port.name)
            .filter(|p| *p != 0);
        let wg = got.and_then(|p| {
            let wg = Workgroup::from_port(c"FaderFrame helper", p);
            // SAFETY: the send right the reply carried, ours to drop.
            unsafe { mach_port_deallocate(task(), p) };
            wg
        });
        // SAFETY: the rights we made or looked up.
        unsafe {
            mach_port_mod_refs(task(), reply_port, RIGHT_RECEIVE, -1);
            mach_port_deallocate(task(), server);
        }
        wg
    }
}
