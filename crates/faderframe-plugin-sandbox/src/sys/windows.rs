//! Windows: a named pipe for control (overlapped I/O, so every wait has a
//! deadline and also watches the other process), auto-reset events for
//! the per-block wake-ups, a pagefile-backed file mapping for the audio.
//!
//! The helper finds its connections by name in its environment
//! (`FADERFRAME_PLUGIN_SANDBOX=<pipe>|<go event>|<done event>|<FaderFrame's
//! process id>|<DPI awareness>`). The pipe admits one local client and is
//! created before the helper starts, so nothing can take its place.

use super::{Connected, ENV_HELPER, Link, Ready, unique_name};
use crate::Launcher;
use std::cell::Cell;
use std::io::{self, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE,
    ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateEventW, EVENT_MODIFY_STATE, GetCurrentProcess, INFINITE, OpenEventW,
    OpenProcess, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE, SetEvent, TerminateProcess,
    WaitForMultipleObjects, WaitForSingleObject,
};

/// How long FaderFrame waits for a starting helper to connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

/// A kernel handle we own.
pub struct Owned(HANDLE);

// SAFETY: kernel handles may be used and closed from any thread.
unsafe impl Send for Owned {}
// SAFETY: as above; every call through `&Owned` is thread-safe in Win32.
unsafe impl Sync for Owned {}

impl Owned {
    /// Take `h` (from a call returning null or `INVALID_HANDLE_VALUE` on
    /// failure).
    fn new(h: HANDLE) -> io::Result<Self> {
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            Err(last_error())
        } else {
            Ok(Self(h))
        }
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }

    /// Another handle to the same object.
    fn duplicate(h: HANDLE) -> io::Result<Self> {
        let mut out: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicates a valid handle within this process.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                h,
                GetCurrentProcess(),
                &mut out,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        Self::new(out)
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: closes a handle only we own.
        unsafe { CloseHandle(self.0) };
    }
}

/// An auto-reset event (unnamed unless `name`).
pub fn event(name: Option<&str>) -> io::Result<Owned> {
    let name = name.map(wide);
    // SAFETY: plain creation; the name (if any) is NUL-terminated.
    let h = unsafe {
        CreateEventW(
            std::ptr::null(),
            0,
            0,
            name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
        )
    };
    let h = Owned::new(h)?;
    // SAFETY: plain query right after the call that set it.
    if name.is_some() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "event exists"));
    }
    Ok(h)
}

pub fn set_event(e: &Owned) -> bool {
    // SAFETY: a valid event handle.
    unsafe { SetEvent(e.raw()) != 0 }
}

/// Milliseconds for a wait, rounded up (a sub-millisecond rest must not
/// become 0 and spin).
fn millis(timeout: Option<Duration>) -> u32 {
    timeout.map_or(INFINITE, |d| {
        (d.as_micros().div_ceil(1000)).min(u128::from(INFINITE - 1)) as u32
    })
}

/// Wait for `first`, or for `peer` (a process) to end.
fn wait_two(first: HANDLE, peer: Option<HANDLE>, timeout: Option<Duration>) -> Ready {
    let handles = [first, peer.unwrap_or(first)];
    let n = if peer.is_some() { 2 } else { 1 };
    // SAFETY: `n` valid handles.
    let r = unsafe { WaitForMultipleObjects(n, handles.as_ptr(), 0, millis(timeout)) };
    if r == WAIT_OBJECT_0 {
        Ready::Woken
    } else if r == WAIT_TIMEOUT {
        Ready::TimedOut
    } else {
        Ready::HungUp
    }
}

/// Like [`wait_two`], but window messages *sent* to this thread meanwhile
/// are handled (posted ones stay queued): an editor's window in the helper
/// is a child of one of ours and sends it messages — when it is created,
/// resized or destroyed — while we wait for the helper's answer; neither
/// side would get on otherwise.
fn wait_two_pumping(first: HANDLE, peer: Option<HANDLE>, timeout: Option<Duration>) -> Ready {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MSG, MsgWaitForMultipleObjectsEx, PM_NOREMOVE, PM_QS_SENDMESSAGE, PeekMessageW,
        QS_SENDMESSAGE,
    };
    let handles = [first, peer.unwrap_or(first)];
    let n = if peer.is_some() { 2 } else { 1 };
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        // SAFETY: `n` valid handles.
        let r = unsafe {
            MsgWaitForMultipleObjectsEx(n, handles.as_ptr(), millis(left), QS_SENDMESSAGE, 0)
        };
        if r == WAIT_OBJECT_0 {
            return Ready::Woken;
        }
        if r == WAIT_OBJECT_0 + n {
            // Peeking delivers the sent messages to their windows.
            // SAFETY: a zeroed MSG to peek into; nothing is removed.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                PeekMessageW(
                    &mut msg,
                    std::ptr::null_mut(),
                    0,
                    0,
                    PM_NOREMOVE | PM_QS_SENDMESSAGE,
                );
            }
            continue;
        }
        return if r == WAIT_TIMEOUT {
            Ready::TimedOut
        } else {
            Ready::HungUp
        };
    }
}

/// Wakes the other side.
pub struct Signal(Owned);

impl Signal {
    /// Wait-free.
    pub fn signal(&self) -> bool {
        set_event(&self.0)
    }
}

/// Waits for the other side's wake-up, or for its process to end.
pub struct Waiter {
    event: Owned,
    peer: Option<Owned>,
}

impl Waiter {
    /// No allocation.
    pub fn wait(&self, timeout: Option<Duration>) -> Ready {
        wait_two(
            self.event.raw(),
            self.peer.as_ref().map(Owned::raw),
            timeout,
        )
    }
}

/// The control stream: one end of a duplex byte pipe.
pub struct Control {
    pipe: Arc<Owned>,
    /// The process at the other end (FaderFrame's side only): waits end
    /// when it does.
    peer: Option<Arc<Owned>>,
    read_timeout: Cell<Option<Duration>>,
    write_timeout: Cell<Option<Duration>>,
}

impl Control {
    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.read_timeout.set(t);
        Ok(())
    }

    pub fn set_write_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.write_timeout.set(t);
        Ok(())
    }

    /// Another handle on the same stream (reads and writes may run on
    /// different threads at once).
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            pipe: Arc::clone(&self.pipe),
            peer: self.peer.clone(),
            read_timeout: Cell::new(self.read_timeout.get()),
            write_timeout: Cell::new(self.write_timeout.get()),
        })
    }

    /// One overlapped read or write, waited for with the timeout.
    fn transfer(&self, read: bool, ptr: *mut u8, len: usize) -> io::Result<usize> {
        let h = self.pipe.raw();
        let len = len.min(1 << 20) as u32;
        let timeout = if read {
            self.read_timeout.get()
        } else {
            self.write_timeout.get()
        };
        let done = event(None)?;
        // SAFETY: a zeroed OVERLAPPED is the documented initial state.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = done.raw();
        let mut n = 0u32;
        // SAFETY: `ptr` holds `len` bytes and, like `ov`, outlives the
        // operation: it is waited for (or cancelled and waited for) below.
        let ok = unsafe {
            if read {
                ReadFile(h, ptr, len, &mut n, &mut ov)
            } else {
                WriteFile(h, ptr, len, &mut n, &mut ov)
            }
        };
        if ok == 0 {
            // SAFETY: plain query right after the failed call.
            let e = unsafe { GetLastError() };
            if e != ERROR_IO_PENDING {
                return closed(read, e);
            }
        }
        let peer = self.peer.as_ref().map(|p| p.raw());
        let r = wait_two_pumping(done.raw(), peer, timeout);
        // SAFETY: `ov` is the pending operation's; after CancelIoEx the
        // blocking GetOverlappedResult waits until the system is done with
        // the buffer.
        let ok = unsafe {
            if r != Ready::Woken {
                CancelIoEx(h, &ov);
            }
            GetOverlappedResult(h, &ov, &mut n, 1)
        };
        if ok != 0 {
            return Ok(n as usize);
        }
        // SAFETY: plain query right after the failed call.
        let e = unsafe { GetLastError() };
        match r {
            Ready::TimedOut => Err(io::Error::new(io::ErrorKind::TimedOut, "no answer")),
            Ready::HungUp => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the other process ended",
            )),
            Ready::Woken => closed(read, e),
        }
    }
}

/// A failed transfer: a closed pipe reads as the end of the stream.
fn closed(read: bool, e: u32) -> io::Result<usize> {
    if matches!(
        e,
        ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
    ) {
        if read {
            return Ok(0);
        }
        return Err(io::ErrorKind::BrokenPipe.into());
    }
    Err(io::Error::from_raw_os_error(e as i32))
}

impl Read for Control {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.transfer(true, buf.as_mut_ptr(), buf.len())
    }
}

impl Write for Control {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // WriteFile only reads the buffer.
        self.transfer(false, buf.as_ptr().cast_mut(), buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Wait for the helper to connect to the pipe (or to die trying).
fn accept(pipe: &Owned, peer: &Owned) -> io::Result<()> {
    let done = event(None)?;
    // SAFETY: a zeroed OVERLAPPED is the documented initial state.
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.hEvent = done.raw();
    // SAFETY: `ov` outlives the operation (waited for below).
    if unsafe { ConnectNamedPipe(pipe.raw(), &mut ov) } == 0 {
        // SAFETY: plain query right after the failed call.
        match unsafe { GetLastError() } {
            ERROR_PIPE_CONNECTED => return Ok(()),
            ERROR_IO_PENDING => {}
            e => return Err(io::Error::from_raw_os_error(e as i32)),
        }
    }
    let r = wait_two_pumping(done.raw(), Some(peer.raw()), Some(CONNECT_TIMEOUT));
    let mut n = 0u32;
    // SAFETY: as in `Control::transfer`.
    let ok = unsafe {
        if r != Ready::Woken {
            CancelIoEx(pipe.raw(), &ov);
        }
        GetOverlappedResult(pipe.raw(), &ov, &mut n, 1)
    };
    if ok != 0 {
        return Ok(());
    }
    Err(match r {
        Ready::HungUp => io::Error::other("the plugin process ended at start-up"),
        _ => io::Error::new(
            io::ErrorKind::TimedOut,
            "the plugin process did not connect",
        ),
    })
}

/// FaderFrame's DPI awareness, for the helper to copy (its editors are
/// child windows of FaderFrame's): the `DPI_AWARENESS_CONTEXT` value, 0
/// when unknown.
fn dpi_awareness() -> isize {
    use windows_sys::Win32::UI::HiDpi::{
        AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_CONTEXT_SYSTEM_AWARE,
        DPI_AWARENESS_CONTEXT_UNAWARE, GetThreadDpiAwarenessContext,
    };
    // SAFETY: plain queries and comparisons of context values.
    unsafe {
        let current = GetThreadDpiAwarenessContext();
        [
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,
            DPI_AWARENESS_CONTEXT_SYSTEM_AWARE,
            DPI_AWARENESS_CONTEXT_UNAWARE,
        ]
        .into_iter()
        .find(|&c| AreDpiAwarenessContextsEqual(current, c) != 0)
        .map_or(0, |c| c as isize)
    }
}

/// Start a helper and wait until it has connected.
pub fn spawn(launcher: &Launcher) -> io::Result<Link> {
    let id = unique_name();
    let pipe_name = format!(r"\\.\pipe\{id}");
    let go_name = format!(r"Local\{id}.go");
    let done_name = format!(r"Local\{id}.done");
    let pipe_w = wide(&pipe_name);
    // SAFETY: plain creation with a NUL-terminated name.
    let pipe = Owned::new(unsafe {
        CreateNamedPipeW(
            pipe_w.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            std::ptr::null(),
        )
    })?;
    let go = event(Some(&go_name))?;
    let done = event(Some(&done_name))?;
    let mut cmd = Command::new(&launcher.exe);
    cmd.args(&launcher.args)
        .envs(launcher.env.iter().map(|(k, v)| (k, v)))
        .env(
            ENV_HELPER,
            format!(
                "{pipe_name}|{go_name}|{done_name}|{}|{}",
                std::process::id(),
                dpi_awareness()
            ),
        )
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd.spawn()?;
    let process = match Owned::duplicate(child.as_raw_handle()) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    if let Err(e) = accept(&pipe, &process) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    let waiter_peer = Owned::duplicate(process.raw())?;
    Ok(Link {
        child,
        control: Control {
            pipe: Arc::new(pipe),
            peer: Some(process),
            read_timeout: Cell::new(None),
            write_timeout: Cell::new(None),
        },
        go: Signal(go),
        done: Waiter {
            event: done,
            peer: Some(waiter_peer),
        },
    })
}

/// In a helper: open what FaderFrame named in the environment, copy its
/// DPI awareness, and end this process should FaderFrame go away.
pub fn connect() -> Option<Connected> {
    let spec = std::env::var(ENV_HELPER).ok()?;
    let mut parts = spec.split('|');
    let (pipe_name, go_name, done_name, pid, dpi) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?.parse::<u32>().ok()?,
        parts.next()?.parse::<isize>().ok()?,
    );
    if dpi != 0 {
        // SAFETY: sets this process's awareness to a context value
        // FaderFrame reported (fails harmlessly if already set).
        unsafe { windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(dpi as _) };
    }
    let pipe_w = wide(pipe_name);
    // SAFETY: opens the named pipe FaderFrame created for this process.
    let pipe = Owned::new(unsafe {
        CreateFileW(
            pipe_w.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    })
    .ok()?;
    let open_event = |name: &str, access: u32| {
        let w = wide(name);
        // SAFETY: opens an event by its NUL-terminated name.
        Owned::new(unsafe { OpenEventW(access, 0, w.as_ptr()) }).ok()
    };
    let go = open_event(go_name, SYNCHRONIZATION_SYNCHRONIZE)?;
    let done = open_event(done_name, EVENT_MODIFY_STATE)?;
    // SAFETY: opens FaderFrame's process only to wait for its end.
    let host = Owned::new(unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) }).ok()?;
    let watched = Owned::duplicate(host.raw()).ok()?;
    let _ = std::thread::Builder::new()
        .name("faderframe-sandbox-watch".into())
        .spawn(move || {
            // SAFETY: waits on a process handle we own, then ends this
            // process at once (nothing a stuck plugin holds can stop it).
            unsafe {
                WaitForSingleObject(watched.raw(), INFINITE);
                TerminateProcess(GetCurrentProcess(), 0);
            }
        });
    Some(Connected {
        control: Control {
            pipe: Arc::new(pipe),
            peer: None,
            read_timeout: Cell::new(None),
            write_timeout: Cell::new(None),
        },
        go: Waiter {
            event: go,
            peer: Some(host),
        },
        done: Signal(done),
    })
}

/// A named, pagefile-backed file mapping, mapped read-write.
pub struct Shm {
    ptr: *mut u8,
    len: usize,
    name: String,
    _mapping: Owned,
}

// SAFETY: the mapping is plain memory; who touches what when is decided
// by the protocol of its users (`shm::Block`).
unsafe impl Send for Shm {}
// SAFETY: as above.
unsafe impl Sync for Shm {}

impl Shm {
    /// A new object of `len` bytes (zeroed) under a fresh name.
    pub fn create(len: usize) -> io::Result<Self> {
        let name = format!(r"Local\{}", unique_name());
        let w = wide(&name);
        // SAFETY: plain creation with a NUL-terminated name.
        let h = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                (len as u64 >> 32) as u32,
                len as u32,
                w.as_ptr(),
            )
        };
        let mapping = Owned::new(h)?;
        // SAFETY: plain query right after the call that set it.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "mapping exists",
            ));
        }
        Self::map(mapping, name, len)
    }

    /// Open an object another process created; it must hold `len` bytes.
    pub fn open(name: &str, len: usize) -> io::Result<Self> {
        let w = wide(name);
        // SAFETY: opens a mapping by its NUL-terminated name.
        let mapping = Owned::new(unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, w.as_ptr()) })?;
        // Mapping more than the object holds fails.
        Self::map(mapping, name.to_string(), len)
    }

    fn map(mapping: Owned, name: String, len: usize) -> io::Result<Self> {
        // SAFETY: maps `len` bytes of a valid mapping; checked below.
        let view = unsafe { MapViewOfFile(mapping.raw(), FILE_MAP_ALL_ACCESS, 0, 0, len) };
        if view.Value.is_null() {
            return Err(last_error());
        }
        Ok(Self {
            ptr: view.Value.cast(),
            len,
            name,
            _mapping: mapping,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Names last as long as a handle: nothing to remove.
    pub fn unlink(&mut self) {}

    pub fn ptr(&self) -> *mut u8 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the view `map` made.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ptr.cast(),
            })
        };
    }
}

/// The helper's main thread: wait for `wake`, a window message or the
/// timeout, then hand the window messages to their windows.
pub mod ui {
    use super::{Owned, millis};
    use std::time::Duration;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
        PeekMessageW, QS_ALLINPUT, TranslateMessage,
    };

    /// Set up the main thread for plugin editors: OLE (drag and drop,
    /// clipboard) and no system error dialogs should a plugin crash.
    pub fn prepare() {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
        };
        // SAFETY: process-wide settings, made once at start-up on the
        // main thread.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
            windows_sys::Win32::System::Ole::OleInitialize(std::ptr::null());
        }
    }

    pub fn wait(wake: &Owned, timeout: Option<Duration>) {
        let h = [wake.raw()];
        // SAFETY: one valid handle.
        unsafe {
            MsgWaitForMultipleObjectsEx(
                1,
                h.as_ptr(),
                millis(timeout),
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
    }

    pub fn pump() {
        // SAFETY: the standard message loop on a zeroed MSG.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
