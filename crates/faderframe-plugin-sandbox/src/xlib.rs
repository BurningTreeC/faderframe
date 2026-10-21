//! Plugins' X errors are logged, not fatal.
//!
//! Plugin GUIs on Linux/BSD mostly talk to the X server through Xlib
//! (JUCE, VSTGUI, many others). Xlib reports an error the server answers
//! with — a request naming a window that is gone, say — to the process's
//! error handler, and its default handler prints "X Error of failed
//! request" and calls `exit(1)`: one stale window id in a plugin's GUI
//! ends the whole process (issue #12: the plugin's sandbox helper, and
//! FaderFrame itself for plugins it hosts in process). Toolkits made for
//! applications install a handler of their own; plugin toolkits leave that
//! to the host (JUCE installs one only in standalone applications). Hosts
//! install one that logs and carries on, and so does FaderFrame: an X
//! error is the plugin's GUI asking for something the server refused, and
//! the request simply has no effect.
//!
//! The handler lives in libX11's process-wide state. libX11 is opened here
//! (the same library every plugin links, by its soname) and kept loaded;
//! a plugin that installs a handler of its own later replaces it, which is
//! its right. In FaderFrame's own process a handler someone else installed
//! first (GDK on an X11 display) is left in place.

/// Whose process this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Process {
    /// A sandbox helper: nothing but the plugin talks to X; the handler is
    /// installed whatever was there.
    Helper,
    /// FaderFrame (plugins hosted in process): only over Xlib's default.
    Host,
}

/// What installing the handler found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tolerance {
    /// Ours is installed.
    Installed,
    /// Someone else's handler was there and stays.
    Kept,
    /// No libX11 on this system (nothing can make X errors), or not a
    /// platform with X11.
    NoXlib,
}

/// Install the non-fatal X error handler (first call decides; later ones
/// return the same).
pub fn tolerate_x_errors(process: Process) -> Tolerance {
    imp::install(process)
}

/// X errors logged and survived in this process so far.
pub fn x_errors_survived() -> u64 {
    imp::survived()
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::{Process, Tolerance};
    use std::ffi::{c_int, c_uchar, c_ulong, c_void};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Xlib's `XErrorEvent` (Xlib.h).
    #[repr(C)]
    pub struct XErrorEvent {
        kind: c_int,
        display: *mut c_void,
        resource_id: c_ulong,
        serial: c_ulong,
        error_code: c_uchar,
        request_code: c_uchar,
        minor_code: c_uchar,
    }

    type Handler = unsafe extern "C" fn(*mut c_void, *mut XErrorEvent) -> c_int;
    type SetHandler = unsafe extern "C" fn(Option<Handler>) -> Option<Handler>;

    static RESULT: OnceLock<Tolerance> = OnceLock::new();
    static SURVIVED: AtomicU64 = AtomicU64::new(0);

    /// The first errors are logged in full, then every hundredth (a GUI
    /// drawing into a dead window can make one per frame).
    const LOGGED_IN_FULL: u64 = 20;

    pub fn survived() -> u64 {
        SURVIVED.load(Ordering::Relaxed)
    }

    pub fn install(process: Process) -> Tolerance {
        *RESULT.get_or_init(|| {
            let t = install_now(process);
            match t {
                Tolerance::Installed => {
                    tracing::debug!("X errors of plugins are logged, not fatal")
                }
                Tolerance::Kept => {
                    tracing::debug!("X errors: the handler installed before stays (GDK's)")
                }
                Tolerance::NoXlib => {}
            }
            t
        })
    }

    fn install_now(process: Process) -> Tolerance {
        // SAFETY: dlopen with a NUL-terminated name; the handle is kept for
        // the life of the process (never closed: libX11 holds our handler).
        let lib =
            unsafe { libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if lib.is_null() {
            return Tolerance::NoXlib;
        }
        // SAFETY: looking up symbols in the library just opened.
        let (set, default) = unsafe {
            (
                libc::dlsym(lib, c"XSetErrorHandler".as_ptr()),
                libc::dlsym(lib, c"_XDefaultError".as_ptr()),
            )
        };
        if set.is_null() {
            return Tolerance::NoXlib;
        }
        // SAFETY: `XSetErrorHandler`'s C signature: it takes and returns an
        // error handler (NULL: Xlib's default).
        let set: SetHandler = unsafe { std::mem::transmute::<*mut c_void, SetHandler>(set) };
        // SAFETY: installing a handler with the signature Xlib calls; it is
        // a plain function of this program, valid for its whole life.
        let previous = unsafe { set(Some(on_x_error)) };
        let was_default = match previous {
            None => true,
            Some(f) => !default.is_null() && f as usize == default as usize,
        };
        if process == Process::Host && !was_default {
            // GDK's (X11 display) or another library's: it stays.
            // SAFETY: putting back the handler that was installed.
            unsafe { set(previous) };
            return Tolerance::Kept;
        }
        Tolerance::Installed
    }

    /// Log the error and carry on (Xlib ignores the return value).
    unsafe extern "C" fn on_x_error(_display: *mut c_void, event: *mut XErrorEvent) -> c_int {
        // SAFETY: Xlib passes the event it read, valid during the call.
        let Some(e) = (unsafe { event.as_ref() }) else {
            return 0;
        };
        let n = SURVIVED.fetch_add(1, Ordering::Relaxed) + 1;
        if n <= LOGGED_IN_FULL || n.is_multiple_of(100) {
            tracing::warn!(
                "a plugin's GUI caused an X error, ignored: {} in {} (resource 0x{:x}, serial {}){}",
                error_name(e.error_code),
                request_name(e.request_code, e.minor_code),
                e.resource_id,
                e.serial,
                if n > 1 {
                    format!(" — {n} so far")
                } else {
                    String::new()
                }
            );
        }
        0
    }

    /// The core protocol's error names (X.h).
    fn error_name(code: u8) -> String {
        const NAMES: [&str; 18] = [
            "Success",
            "BadRequest",
            "BadValue",
            "BadWindow",
            "BadPixmap",
            "BadAtom",
            "BadCursor",
            "BadFont",
            "BadMatch",
            "BadDrawable",
            "BadAccess",
            "BadAlloc",
            "BadColor",
            "BadGC",
            "BadIDChoice",
            "BadName",
            "BadLength",
            "BadImplementation",
        ];
        NAMES.get(usize::from(code)).map_or_else(
            || format!("error {code} (an extension's)"),
            |s| (*s).to_string(),
        )
    }

    /// The core requests GUIs make most (Xproto.h); others by number.
    fn request_name(major: u8, minor: u8) -> String {
        let name = match major {
            1 => "CreateWindow",
            2 => "ChangeWindowAttributes",
            3 => "GetWindowAttributes",
            4 => "DestroyWindow",
            7 => "ReparentWindow",
            8 => "MapWindow",
            10 => "UnmapWindow",
            12 => "ConfigureWindow",
            14 => "GetGeometry",
            15 => "QueryTree",
            18 => "ChangeProperty",
            19 => "DeleteProperty",
            20 => "GetProperty",
            25 => "SendEvent",
            38 => "QueryPointer",
            40 => "TranslateCoords",
            42 => "SetInputFocus",
            53 => "CreatePixmap",
            54 => "FreePixmap",
            55 => "CreateGC",
            60 => "FreeGC",
            62 => "CopyArea",
            72 => "PutImage",
            73 => "GetImage",
            _ if major >= 128 => return format!("extension request {major}.{minor}"),
            _ => return format!("request {major}"),
        };
        format!("X_{name}")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn errors_are_named() {
            assert_eq!(error_name(3), "BadWindow");
            assert_eq!(error_name(9), "BadDrawable");
            assert!(error_name(160).contains("extension"));
            assert_eq!(request_name(20, 0), "X_GetProperty");
            assert_eq!(request_name(131, 4), "extension request 131.4");
        }

        #[test]
        fn the_handler_counts_and_returns() {
            let before = survived();
            let mut e = XErrorEvent {
                kind: 0,
                display: std::ptr::null_mut(),
                resource_id: 0x160_0009,
                serial: 122,
                error_code: 3,
                request_code: 20,
                minor_code: 0,
            };
            // SAFETY: a valid event, as Xlib would pass it.
            assert_eq!(unsafe { on_x_error(std::ptr::null_mut(), &mut e) }, 0);
            // SAFETY: Xlib never passes none; the handler copes anyway.
            assert_eq!(
                unsafe { on_x_error(std::ptr::null_mut(), std::ptr::null_mut()) },
                0
            );
            assert!(survived() > before);
        }
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
mod imp {
    use super::{Process, Tolerance};

    pub fn install(_process: Process) -> Tolerance {
        Tolerance::NoXlib
    }

    pub fn survived() -> u64 {
        0
    }
}
