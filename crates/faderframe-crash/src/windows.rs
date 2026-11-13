//! The Windows side: an exception's record and context, the stack walked
//! from it, the modules of the process.
//!
//! Everything here runs in a process that just crashed, on the thread
//! that crashed: memory is read only where `VirtualQuery` says it reads,
//! the stack is walked only while it stays within the thread's stack, and
//! a report is handed out section by section so that a fault while making
//! one part leaves the parts before it written.

use crate::{Module, describe_modules, describe_stack, pe, place};
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BAD_LENGTH, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::Debug::{CONTEXT, EXCEPTION_POINTERS};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
    VirtualQuery,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

/// The most frames walked (a stack overflow's are many).
const MAX_FRAMES: usize = 4096;
/// The most modules listed.
const MAX_MODULES: usize = 4096;

const READABLE: u32 = PAGE_READONLY
    | PAGE_READWRITE
    | PAGE_WRITECOPY
    | PAGE_EXECUTE_READ
    | PAGE_EXECUTE_READWRITE
    | PAGE_EXECUTE_WRITECOPY;

/// Whether `len` bytes at `address` read: committed, readable, no guard
/// page.
fn readable(address: usize, len: usize) -> bool {
    let Some(end) = address.checked_add(len) else {
        return false;
    };
    let mut at = address;
    while at < end {
        // SAFETY: plain data, filled in below.
        let mut m: MEMORY_BASIC_INFORMATION = unsafe { zeroed() };
        // SAFETY: VirtualQuery describes any address of the process.
        let got = unsafe {
            VirtualQuery(
                at as *const c_void,
                &mut m,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        let region_end = (m.BaseAddress as usize).saturating_add(m.RegionSize);
        if got == 0
            || m.State != MEM_COMMIT
            || m.Protect & READABLE == 0
            || m.Protect & PAGE_GUARD != 0
            || region_end <= at
        {
            return false;
        }
        at = region_end;
    }
    true
}

/// A module's image where it is mapped.
struct Mapped {
    base: usize,
    size: usize,
}

impl Mapped {
    fn of(m: &Module) -> Self {
        Self {
            base: m.base as usize,
            size: m.size as usize,
        }
    }
}

impl pe::Image for Mapped {
    fn read(&self, rva: u32, len: u32) -> Option<&[u8]> {
        let (rva, len) = (rva as usize, len as usize);
        if rva.checked_add(len)? > self.size {
            return None;
        }
        let at = self.base.checked_add(rva)?;
        if len > 0 && !readable(at, len) {
            return None;
        }
        // SAFETY: inside the module's mapping and readable (checked just
        // now). The module stays mapped while the report is made, unless
        // another thread unloads it at that very moment — a crash report
        // takes that chance.
        Some(unsafe { std::slice::from_raw_parts(at as *const u8, len) })
    }
}

fn wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// The modules loaded in this process, by address.
pub fn modules() -> Vec<Module> {
    let mut snapshot = INVALID_HANDLE_VALUE;
    for _ in 0..8 {
        // SAFETY: a snapshot of this process's modules (closed below).
        snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, 0) };
        // A module loading meanwhile fails it with ERROR_BAD_LENGTH: again.
        // SAFETY: reads this thread's last error.
        if snapshot != INVALID_HANDLE_VALUE || unsafe { GetLastError() } != ERROR_BAD_LENGTH {
            break;
        }
    }
    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // SAFETY: plain data; the size is set before it is used.
    let mut entry: MODULEENTRY32W = unsafe { zeroed() };
    entry.dwSize = size_of::<MODULEENTRY32W>() as u32;
    // SAFETY: the snapshot, and an entry of the size it says.
    let mut more = unsafe { Module32FirstW(snapshot, &mut entry) } != 0;
    while more && out.len() < MAX_MODULES {
        let image = Mapped {
            base: entry.modBaseAddr as usize,
            size: entry.modBaseSize as usize,
        };
        let headers = pe::headers(&image);
        out.push(Module {
            base: image.base as u64,
            size: image.size as u64,
            name: wide(&entry.szModule),
            path: wide(&entry.szExePath),
            time_stamp: headers.map(|h| h.time_stamp),
            pdb: headers.and_then(|h| pe::codeview(&image, &h)),
        });
        // SAFETY: as above.
        more = unsafe { Module32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: the snapshot made above, closed once.
    unsafe { CloseHandle(snapshot) };
    out.sort_by_key(|m| m.base);
    out
}

/// The export of `m` nearest below `offset` (its name and how far past
/// it), read from the module's image in memory.
pub fn export_below(m: &Module, offset: u32) -> Option<(String, u32)> {
    let image = Mapped::of(m);
    let h = pe::headers(&image)?;
    pe::export_below(&image, &h, offset)
}

/// This thread's id as Windows numbers it.
pub fn os_thread_id() -> u32 {
    // SAFETY: no arguments, no failure.
    unsafe { GetCurrentThreadId() }
}

/// `CONTEXT` as Windows wants it: 16-byte aligned (its registers are
/// saved with aligned stores).
#[repr(C, align(16))]
struct Context(CONTEXT);

/// The stack here: the addresses of its frames, from this function's
/// caller (or this function) out to the thread's start.
#[inline(never)]
pub fn stack_here() -> Vec<u64> {
    #[cfg(target_arch = "x86_64")]
    {
        use windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureContext;
        // SAFETY: plain data, filled in below.
        let mut c = Box::new(Context(unsafe { zeroed() }));
        // SAFETY: fills the (aligned) context with this thread's registers.
        unsafe { RtlCaptureContext(&mut c.0) };
        walk(c)
    }
    #[cfg(not(target_arch = "x86_64"))]
    captured()
}

/// The stack from an exception's context: its first frame the faulting
/// instruction.
fn stack_from(c: &CONTEXT) -> Vec<u64> {
    #[cfg(target_arch = "x86_64")]
    {
        walk(Box::new(Context(*c)))
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = c;
        captured()
    }
}

/// Walks the stack from `c` with the modules' unwind tables. A frame no
/// table describes is a leaf function or code of no module (a call
/// through a bad pointer): its return address is on top of the stack. The
/// walk ends at the thread's start, when it leaves the thread's stack or
/// stops going up it.
#[cfg(target_arch = "x86_64")]
fn walk(mut c: Box<Context>) -> Vec<u64> {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        RtlLookupFunctionEntry, RtlVirtualUnwind, UNW_FLAG_NHANDLER,
    };
    use windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits;
    let (mut low, mut high) = (0usize, 0usize);
    // SAFETY: writes the two limits.
    unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) };
    let on_stack = |sp: u64| (sp as usize) >= low && (sp as usize) < high;
    let mut frames = Vec::new();
    while frames.len() < MAX_FRAMES {
        let (pc, sp) = (c.0.Rip, c.0.Rsp);
        if pc == 0 || !on_stack(sp) {
            break;
        }
        frames.push(pc);
        let mut image_base = 0u64;
        // SAFETY: looks `pc` up in the loaded modules' unwind tables.
        let entry = unsafe { RtlLookupFunctionEntry(pc, &mut image_base, null_mut()) };
        if entry.is_null() {
            if !readable(sp as usize, 8) {
                break;
            }
            // SAFETY: on the stack and readable (checked just now).
            c.0.Rip = unsafe { std::ptr::read_unaligned(sp as *const u64) };
            c.0.Rsp = sp + 8;
        } else {
            let mut data: *mut c_void = null_mut();
            let mut establisher = 0u64;
            // SAFETY: the function's own unwind entry, on a context of ours.
            unsafe {
                RtlVirtualUnwind(
                    UNW_FLAG_NHANDLER,
                    image_base,
                    pc,
                    entry,
                    &mut c.0,
                    &mut data,
                    &mut establisher,
                    null_mut(),
                )
            };
        }
        if c.0.Rsp <= sp {
            break;
        }
    }
    frames
}

/// The stack as Windows captures it from here (where there is no walk of
/// our own).
#[cfg(not(target_arch = "x86_64"))]
fn captured() -> Vec<u64> {
    use windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;
    let mut frames = vec![null_mut(); 62];
    // SAFETY: room for the 62 frames asked for.
    let n = unsafe { RtlCaptureStackBackTrace(0, 62, frames.as_mut_ptr(), null_mut()) };
    frames.truncate(usize::from(n));
    frames.into_iter().map(|p| p as u64).collect()
}

/// The registers of an exception's context.
fn registers(c: &CONTEXT) -> String {
    #[cfg(target_arch = "x86_64")]
    {
        let r = [
            ("rax", c.Rax),
            ("rbx", c.Rbx),
            ("rcx", c.Rcx),
            ("rdx", c.Rdx),
            ("rsi", c.Rsi),
            ("rdi", c.Rdi),
            ("rbp", c.Rbp),
            ("rsp", c.Rsp),
            ("r8", c.R8),
            ("r9", c.R9),
            ("r10", c.R10),
            ("r11", c.R11),
            ("r12", c.R12),
            ("r13", c.R13),
            ("r14", c.R14),
            ("r15", c.R15),
        ];
        let mut s = String::from("registers:\n");
        for row in r.chunks(4) {
            s.push(' ');
            for (name, v) in row {
                s.push_str(&format!(" {name:>3} 0x{v:016x}"));
            }
            s.push('\n');
        }
        s.push_str(&format!(
            "  rip 0x{:016x} eflags 0x{:08x}\n\n",
            c.Rip, c.EFlags
        ));
        s
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = c;
        String::new()
    }
}

/// An unhandled exception's report, after the line naming it: where it
/// happened (module + offset, the nearest export), what it touched, the
/// registers, the stack from the faulting instruction and the modules —
/// each part given to `write` as soon as it is made.
///
/// # Safety
/// `info` is what Windows passed an exception filter, or null.
pub unsafe fn report_exception(info: *const EXCEPTION_POINTERS, write: &mut dyn FnMut(&str)) {
    // SAFETY: what Windows passed the filter (or null), valid during it.
    let (record, context) = unsafe {
        info.as_ref().map_or((None, None), |i| {
            (i.ExceptionRecord.as_ref(), i.ContextRecord.as_ref())
        })
    };
    let modules = modules();
    if let Some(r) = record {
        let mut s = format!(
            "in {}\n",
            place(&modules, r.ExceptionAddress as u64, false, &export_below)
        );
        let n = (r.NumberParameters as usize).min(r.ExceptionInformation.len());
        let parameters: Vec<u64> = r.ExceptionInformation[..n]
            .iter()
            .map(|&p| p as u64)
            .collect();
        if let Some(a) = crate::access(r.ExceptionCode as u32, &parameters) {
            s.push_str(&a);
            s.push('\n');
        }
        s.push('\n');
        write(&s);
    }
    let frames = match context {
        Some(c) => {
            write(&registers(c));
            stack_from(c)
        }
        None => stack_here(),
    };
    let title = if context.is_some() {
        "stack, from the crash"
    } else {
        "stack, from the crash report"
    };
    write(&format!(
        "{}\n",
        describe_stack(title, &frames, &modules, &export_below)
    ));
    write(&format!("{}\n", describe_modules(&modules)));
}

/// The stack here and the modules (a report of a panic): each part given
/// to `write` as soon as it is made.
#[inline(never)]
pub fn report_stack(write: &mut dyn FnMut(&str)) {
    let frames = stack_here();
    let modules = modules();
    write(&format!(
        "{}\n",
        describe_stack("stack", &frames, &modules, &export_below)
    ));
    write(&format!("{}\n", describe_modules(&modules)));
}
