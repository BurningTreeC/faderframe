//! Audio workgroups (macOS 11+): CoreAudio's IO thread belongs to its
//! device's `os_workgroup`; threads that do part of the IO thread's work —
//! the DSP workers — join it, so the scheduler treats them as one realtime
//! workload with the IO thread's deadline (on Apple silicon: performance
//! cores, no throttling mid-cycle). Elsewhere there are no workgroups and
//! [`Workgroup`] cannot be made.
//!
//! The process keeps the current device's workgroup ([`set_process`]) with
//! a generation that changes with it; plugin helper processes get it as a
//! Mach port ([`Workgroup::copy_port`], [`Workgroup::from_port`]) and their
//! audio threads join it too.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

static PROCESS: Mutex<Option<Workgroup>> = Mutex::new(None);
static GENERATION: AtomicU32 = AtomicU32::new(0);

/// The audio device's workgroup for the whole process (`None`: none); every
/// call starts a new generation.
pub fn set_process(workgroup: Option<Workgroup>) {
    if let Ok(mut g) = PROCESS.lock() {
        *g = workgroup;
    }
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// The process's current workgroup.
pub fn process() -> Option<Workgroup> {
    PROCESS.lock().ok().and_then(|g| g.clone())
}

/// Changes with every [`set_process`] (0: never set); realtime-safe.
pub fn process_generation() -> u32 {
    GENERATION.load(Ordering::Acquire)
}

/// A retained `os_workgroup_t` (`Send`/`Sync`: workgroups are thread-safe).
pub struct Workgroup {
    #[cfg(target_os = "macos")]
    raw: *mut std::ffi::c_void,
    #[cfg(not(target_os = "macos"))]
    _none: std::convert::Infallible,
}

// SAFETY: os_workgroup objects may be used from any thread.
unsafe impl Send for Workgroup {}
// SAFETY: as above.
unsafe impl Sync for Workgroup {}

impl std::fmt::Debug for Workgroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Workgroup")
    }
}

/// Membership of the calling thread, from [`Workgroup::join`]; leave with
/// [`Workgroup::leave`] on the same thread.
pub struct Membership {
    #[cfg(target_os = "macos")]
    token: Box<mac::JoinToken>,
    #[cfg(not(target_os = "macos"))]
    _none: std::convert::Infallible,
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_char, c_void};

    /// `os_workgroup_join_token_s`: a signature and 36 opaque bytes.
    #[repr(C)]
    pub struct JoinToken {
        sig: u32,
        opaque: [u8; 36],
    }

    impl JoinToken {
        pub fn new() -> Box<Self> {
            Box::new(Self {
                sig: 0,
                opaque: [0; 36],
            })
        }
    }

    #[repr(C)]
    pub struct PropertyAddress {
        pub selector: u32,
        pub scope: u32,
        pub element: u32,
    }

    pub const SYSTEM_OBJECT: u32 = 1;
    /// 'uidd'
    pub const TRANSLATE_UID_TO_DEVICE: u32 = u32::from_be_bytes(*b"uidd");
    /// 'dOut'
    pub const DEFAULT_OUTPUT_DEVICE: u32 = u32::from_be_bytes(*b"dOut");
    /// 'oswg'
    pub const IO_THREAD_OS_WORKGROUP: u32 = u32::from_be_bytes(*b"oswg");
    /// 'glob'
    pub const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    pub const ELEMENT_MAIN: u32 = 0;
    pub const UTF8: u32 = 0x0800_0100;
    /// `OS_CLOCK_MACH_ABSOLUTE_TIME`
    pub const MACH_ABSOLUTE_TIME: u32 = 32;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        pub fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    #[link(name = "AudioToolbox", kind = "framework")]
    unsafe extern "C" {
        pub fn AudioWorkIntervalCreate(
            name: *const c_char,
            clock: u32,
            attributes: *const c_void,
        ) -> *mut c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> *const c_void;
        pub fn CFRelease(cf: *const c_void);
    }

    unsafe extern "C" {
        pub fn os_retain(object: *mut c_void) -> *mut c_void;
        pub fn os_release(object: *mut c_void);
        pub fn os_workgroup_join(wg: *mut c_void, token: *mut JoinToken) -> i32;
        pub fn os_workgroup_leave(wg: *mut c_void, token: *mut JoinToken);
        pub fn os_workgroup_copy_port(wg: *mut c_void, port: *mut u32) -> i32;
        pub fn os_workgroup_create_with_port(name: *const c_char, port: u32) -> *mut c_void;
    }

    /// A CoreAudio device's IO-thread workgroup (+1 retained), if it has one.
    pub fn device_workgroup(device: u32) -> *mut c_void {
        let address = PropertyAddress {
            selector: IO_THREAD_OS_WORKGROUP,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        };
        let mut wg: *mut c_void = std::ptr::null_mut();
        let mut size = std::mem::size_of::<*mut c_void>() as u32;
        // SAFETY: the property is a retained os_workgroup_t written into
        // `wg` (pointer-sized, as `size` says).
        let status = unsafe {
            AudioObjectGetPropertyData(
                device,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&mut wg as *mut *mut c_void).cast(),
            )
        };
        if status == 0 {
            wg
        } else {
            std::ptr::null_mut()
        }
    }

    /// The device with this UID (0: none).
    pub fn device_by_uid(uid: &str) -> u32 {
        // SAFETY: creates a CFString from valid UTF-8 bytes; released
        // below.
        let cf = unsafe {
            CFStringCreateWithBytes(std::ptr::null(), uid.as_ptr(), uid.len() as isize, UTF8, 0)
        };
        if cf.is_null() {
            return 0;
        }
        let address = PropertyAddress {
            selector: TRANSLATE_UID_TO_DEVICE,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        };
        let mut device = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: the qualifier is the CFStringRef (pointer-sized), the
        // result an AudioObjectID.
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &address,
                std::mem::size_of::<*const c_void>() as u32,
                (&cf as *const *const c_void).cast(),
                &mut size,
                (&mut device as *mut u32).cast(),
            )
        };
        // SAFETY: we created it.
        unsafe { CFRelease(cf) };
        if status == 0 { device } else { 0 }
    }

    pub fn default_output_device() -> u32 {
        let address = PropertyAddress {
            selector: DEFAULT_OUTPUT_DEVICE,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        };
        let mut device = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: the result is an AudioObjectID.
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&mut device as *mut u32).cast(),
            )
        };
        if status == 0 { device } else { 0 }
    }
}

impl Workgroup {
    /// The IO-thread workgroup of the CoreAudio device with this UID (what
    /// cpal reports as the device's id).
    pub fn of_device(uid: &str) -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            let device = mac::device_by_uid(uid);
            Self::of_device_id(device)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = uid;
            None
        }
    }

    /// The default output device's IO-thread workgroup.
    pub fn of_default_output() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            Self::of_device_id(mac::default_output_device())
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    #[cfg(target_os = "macos")]
    fn of_device_id(device: u32) -> Option<Self> {
        if device == 0 {
            return None;
        }
        let raw = mac::device_workgroup(device);
        (!raw.is_null()).then_some(Self { raw })
    }

    /// A workgroup of our own (an audio work interval): what tests join
    /// where no audio device is present.
    pub fn new_interval(name: &std::ffi::CStr) -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: a valid C string; no attributes. The result is +1.
            let raw = unsafe {
                mac::AudioWorkIntervalCreate(
                    name.as_ptr(),
                    mac::MACH_ABSOLUTE_TIME,
                    std::ptr::null(),
                )
            };
            (!raw.is_null()).then_some(Self { raw })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = name;
            None
        }
    }

    /// A send right to a Mach port standing for this workgroup, for another
    /// process ([`Workgroup::from_port`] there); the caller owns the right.
    pub fn copy_port(&self) -> Option<u32> {
        #[cfg(target_os = "macos")]
        {
            let mut port = 0u32;
            // SAFETY: a live workgroup; the port is written on success.
            let r = unsafe { mac::os_workgroup_copy_port(self.raw, &mut port) };
            (r == 0 && port != 0).then_some(port)
        }
        #[cfg(not(target_os = "macos"))]
        {
            match self._none {}
        }
    }

    /// The workgroup another process sent as a Mach port (its send right
    /// stays the caller's).
    pub fn from_port(name: &std::ffi::CStr, port: u32) -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: a valid C string and a send right we hold; the result
            // is +1.
            let raw = unsafe { mac::os_workgroup_create_with_port(name.as_ptr(), port) };
            (!raw.is_null()).then_some(Self { raw })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (name, port);
            None
        }
    }

    /// Join with the calling thread (`None` if the system refuses, e.g. the
    /// workgroup was cancelled because its device went away).
    pub fn join(&self) -> Option<Membership> {
        #[cfg(target_os = "macos")]
        {
            let mut token = mac::JoinToken::new();
            // SAFETY: a live workgroup and a token that outlives the
            // membership (boxed: it does not move).
            let r = unsafe { mac::os_workgroup_join(self.raw, &mut *token) };
            (r == 0).then_some(Membership { token })
        }
        #[cfg(not(target_os = "macos"))]
        {
            match self._none {}
        }
    }

    /// End the calling thread's membership.
    pub fn leave(&self, membership: Membership) {
        #[cfg(target_os = "macos")]
        {
            let mut m = membership;
            // SAFETY: the token of this workgroup's join on this thread.
            unsafe { mac::os_workgroup_leave(self.raw, &mut *m.token) };
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = membership;
            match self._none {}
        }
    }
}

impl Clone for Workgroup {
    fn clone(&self) -> Self {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: retains a live object.
            unsafe { mac::os_retain(self.raw) };
            Self { raw: self.raw }
        }
        #[cfg(not(target_os = "macos"))]
        {
            match self._none {}
        }
    }
}

impl Drop for Workgroup {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        // SAFETY: releases our reference.
        unsafe {
            mac::os_release(self.raw)
        };
    }
}
