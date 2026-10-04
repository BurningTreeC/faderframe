//! The system calls the sandbox needs (Unix): pipes, `poll`, named shared
//! memory, and descriptors at fixed numbers in a child process.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

fn check(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

fn set_fd_flag(fd: RawFd, get: libc::c_int, set: libc::c_int, flag: libc::c_int) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor we own, with integer arguments.
    let flags = check(unsafe { libc::fcntl(fd, get) })?;
    // SAFETY: as above.
    check(unsafe { libc::fcntl(fd, set, flags | flag) })?;
    Ok(())
}

/// A pipe: (read end, write end), both close-on-exec.
pub fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` holds the two descriptors the call writes.
    check(unsafe { libc::pipe(fds.as_mut_ptr()) })?;
    // SAFETY: fresh descriptors nobody else owns.
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&r, &w] {
        set_fd_flag(
            fd.as_raw_fd(),
            libc::F_GETFD,
            libc::F_SETFD,
            libc::FD_CLOEXEC,
        )?;
    }
    Ok((r, w))
}

pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    set_fd_flag(fd, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK)
}

/// Write one byte to a non-blocking pipe; a full pipe is fine (the reader
/// has a wake-up pending). `false` when the reader is gone.
pub fn signal(fd: RawFd) -> bool {
    let b = 1u8;
    // SAFETY: writes one byte from a valid buffer.
    let r = unsafe { libc::write(fd, (&b as *const u8).cast(), 1) };
    r == 1 || io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN)
}

/// Read and discard what waits in a non-blocking pipe. `false` at end of
/// file (the writer is gone).
pub fn drain(fd: RawFd) -> bool {
    let mut buf = [0u8; 64];
    loop {
        // SAFETY: reads into a valid buffer of its length.
        let r = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if r == 0 {
            return false;
        }
        if r < 0 {
            return io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) || drain(fd);
        }
        if (r as usize) < buf.len() {
            return true;
        }
    }
}

/// What waiting for a descriptor ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ready {
    Readable,
    /// The other end is gone.
    HungUp,
    TimedOut,
}

/// Wait until `fd` is readable, at most `timeout` (`None`: forever).
pub fn wait_readable(fd: RawFd, timeout: Option<Duration>) -> Ready {
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        let ms = match deadline {
            None => -1,
            Some(d) => {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ready::TimedOut;
                }
                left.as_millis().clamp(1, i32::MAX as u128) as i32
            }
        };
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut p, 1, ms) };
        if r < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Ready::HungUp;
        }
        if r == 0 {
            continue;
        }
        if p.revents & libc::POLLIN != 0 {
            return Ready::Readable;
        }
        if p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Ready::HungUp;
        }
    }
}

/// `poll` over several descriptors; returns how many are ready (0 on
/// timeout). `timeout_ms < 0` waits forever.
pub fn poll(fds: &mut [libc::pollfd], timeout_ms: i32) -> io::Result<usize> {
    loop {
        // SAFETY: a valid slice of pollfds and its length.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if r >= 0 {
            return Ok(r as usize);
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINTR) {
            return Err(e);
        }
    }
}

/// Make the child of `cmd` find each `(fd, number)` at `number` (the
/// descriptors stay close-on-exec in this process).
pub fn place_fds(cmd: &mut Command, fds: Vec<(RawFd, RawFd)>) {
    let above = fds.iter().map(|&(_, n)| n).max().unwrap_or(2) + 1;
    // SAFETY: the closure runs between fork and exec and only calls fcntl
    // and dup2, which are async-signal-safe. Each descriptor is first moved
    // above every target number, so placing one cannot clobber another.
    unsafe {
        cmd.pre_exec(move || {
            let mut temp = [0 as libc::c_int; 8];
            for (i, &(fd, _)) in fds.iter().enumerate().take(temp.len()) {
                temp[i] = check(libc::fcntl(fd, libc::F_DUPFD, above))?;
            }
            for (i, &(_, number)) in fds.iter().enumerate().take(temp.len()) {
                check(libc::dup2(temp[i], number))?;
                libc::close(temp[i]);
            }
            Ok(())
        })
    };
}

/// Take descriptor `number` (placed by [`place_fds`]) if it is open.
pub fn take_fd(number: RawFd) -> Option<OwnedFd> {
    // SAFETY: only asks whether the descriptor exists.
    let open = unsafe { libc::fcntl(number, libc::F_GETFD) } >= 0;
    // SAFETY: the parent placed it for this process; nothing else owns it.
    open.then(|| unsafe { OwnedFd::from_raw_fd(number) })
}

static SHM_COUNT: AtomicU32 = AtomicU32::new(0);

/// A named POSIX shared memory object, mapped read-write.
pub struct Shm {
    ptr: *mut u8,
    len: usize,
    name: CString,
    /// Created here: unlinked at the latest when dropped.
    owner: bool,
    unlinked: bool,
}

// SAFETY: the mapping is plain memory; who touches what when is decided
// by the protocol of its users (`shm::Block`).
unsafe impl Send for Shm {}
// SAFETY: as above.
unsafe impl Sync for Shm {}

impl Shm {
    /// A new object of `len` bytes (zeroed) under a fresh name.
    pub fn create(len: usize) -> io::Result<Self> {
        let n = SHM_COUNT.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        // At most 31 characters (macOS).
        let name = format!("/ffsb.{}.{n}.{:x}", std::process::id(), nanos & 0xFFFF);
        let cname = CString::new(name).map_err(io::Error::other)?;
        // SAFETY: a valid C string; flags and mode are plain integers.
        let fd = check(unsafe {
            libc::shm_open(
                cname.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600 as libc::c_uint,
            )
        })?;
        // SAFETY: a fresh descriptor nobody else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut shm = Self {
            ptr: std::ptr::null_mut(),
            len,
            name: cname,
            owner: true,
            unlinked: false,
        };
        // SAFETY: resizes the object behind our descriptor.
        check(unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) })?;
        shm.ptr = map(&fd, len)?;
        Ok(shm)
    }

    /// Open an object another process created; it must hold `len` bytes.
    pub fn open(name: &str, len: usize) -> io::Result<Self> {
        let cname = CString::new(name).map_err(io::Error::other)?;
        // SAFETY: a valid C string.
        let fd = check(unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0) })?;
        // SAFETY: a fresh descriptor nobody else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: plain C struct filled by fstat.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid descriptor and out pointer.
        check(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) })?;
        if (st.st_size as u64) < len as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "shared memory too small",
            ));
        }
        Ok(Self {
            ptr: map(&fd, len)?,
            len,
            name: cname,
            owner: false,
            unlinked: true,
        })
    }

    pub fn name(&self) -> &str {
        self.name.to_str().unwrap_or("")
    }

    /// Remove the name (the mapping stays valid).
    pub fn unlink(&mut self) {
        if self.owner && !self.unlinked {
            // SAFETY: a valid C string.
            unsafe { libc::shm_unlink(self.name.as_ptr()) };
            self.unlinked = true;
        }
    }

    pub fn ptr(&self) -> *mut u8 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

fn map(fd: &OwnedFd, len: usize) -> io::Result<*mut u8> {
    // SAFETY: maps `len` bytes of the object shared; checked below.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    Ok(p.cast())
}

impl Drop for Shm {
    fn drop(&mut self) {
        self.unlink();
        if !self.ptr.is_null() {
            // SAFETY: unmaps exactly what `map` mapped.
            unsafe { libc::munmap(self.ptr.cast(), self.len) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_memory_is_shared_and_pipes_signal() {
        let mut a = Shm::create(4096).unwrap();
        let b = Shm::open(a.name(), 4096).unwrap();
        // SAFETY: both map the same 4096 bytes.
        unsafe {
            a.ptr().write(42);
            assert_eq!(b.ptr().read(), 42);
        }
        a.unlink();
        assert!(Shm::open(a.name(), 4096).is_err(), "the name is gone");
        assert!(Shm::open("/ffsb.nope", 16).is_err());

        let (r, w) = pipe().unwrap();
        set_nonblocking(r.as_raw_fd()).unwrap();
        set_nonblocking(w.as_raw_fd()).unwrap();
        assert_eq!(
            wait_readable(r.as_raw_fd(), Some(Duration::from_millis(5))),
            Ready::TimedOut
        );
        assert!(signal(w.as_raw_fd()));
        assert_eq!(wait_readable(r.as_raw_fd(), None), Ready::Readable);
        assert!(drain(r.as_raw_fd()));
        drop(w);
        assert_eq!(wait_readable(r.as_raw_fd(), None), Ready::HungUp);
        assert!(!drain(r.as_raw_fd()));
    }
}
