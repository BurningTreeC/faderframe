//! The walk, the modules and a real crash's report (Windows; Wine too).
#![cfg(windows)]

use faderframe_crash::{export_below, module_of, modules, place, report_exception, stack_here};
use std::ffi::c_void;
use windows_sys::Win32::System::Diagnostics::Debug::{
    EXCEPTION_POINTERS, SetUnhandledExceptionFilter,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

/// Set in the child that crashes: where its report goes.
const CHILD: &str = "FADERFRAME_CRASH_CHILD";

fn program_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

#[inline(never)]
fn inner() -> Vec<u64> {
    std::hint::black_box(stack_here())
}

#[inline(never)]
fn outer() -> Vec<u64> {
    std::hint::black_box(inner())
}

/// The first frame within `reach` bytes after `f`'s start.
fn frame_in(frames: &[u64], f: usize, reach: u64) -> Option<usize> {
    frames
        .iter()
        .position(|&pc| pc > f as u64 && pc - (f as u64) < reach)
}

#[test]
fn the_stack_is_walked_through_the_functions_that_called() {
    let frames = outer();
    let modules = modules();
    let me = program_name();
    let program = modules
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(&me))
        .expect("the program is among the modules");
    assert!(program.contains(frames[0]), "{frames:x?}");
    let i = frame_in(&frames, inner as fn() -> Vec<u64> as usize, 0x200);
    let o = frame_in(&frames, outer as fn() -> Vec<u64> as usize, 0x200);
    assert!(
        matches!((i, o), (Some(i), Some(o)) if i < o),
        "inner then outer: {i:?} {o:?} in {frames:x?}"
    );
    assert!(
        frames.len() > o.unwrap_or(0) + 2,
        "on past the test to the thread's start: {frames:x?}"
    );
    // The walk leaves the program for the system's thread start.
    assert!(
        frames
            .iter()
            .any(|&pc| module_of(&modules, pc).is_some_and(|m| !m.name.eq_ignore_ascii_case(&me))),
        "{frames:x?}"
    );
}

#[test]
fn the_modules_are_listed_with_their_headers() {
    let modules = modules();
    let me = program_name();
    for name in [me.as_str(), "ntdll.dll", "kernel32.dll"] {
        let m = modules
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| panic!("{name} is listed: {modules:#?}"));
        assert!(m.size > 0x1000, "{m:?}");
        assert!(m.time_stamp.is_some(), "its headers were read: {m:?}");
    }
    assert!(
        modules.windows(2).all(|w| w[0].base < w[1].base),
        "by address"
    );
}

#[test]
fn a_system_function_is_named_by_its_export() {
    let kernel32: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
    // SAFETY: a module every process has, a NUL-terminated name.
    let address = unsafe {
        let k = GetModuleHandleW(kernel32.as_ptr());
        GetProcAddress(k, c"GetCurrentProcessId".as_ptr().cast())
    }
    .expect("GetCurrentProcessId") as usize as u64;
    let modules = modules();
    let m = module_of(&modules, address).expect("in a module");
    let (name, past) = export_below(m, (address - m.base) as u32).expect("an export");
    assert_eq!(past, 0, "{name}");
    // The name found is one of the function's own (it may have several).
    let mut cname = name.clone().into_bytes();
    cname.push(0);
    // SAFETY: the module's base is its handle; a NUL-terminated name.
    let again = unsafe { GetProcAddress(m.base as usize as *mut c_void, cname.as_ptr()) };
    assert_eq!(again.map(|f| f as usize as u64), Some(address), "{name}");
    let shown = place(&modules, address, false, &export_below);
    assert!(shown.contains(&format!("(near {name}+0x0)")), "{shown}");
}

unsafe extern "C" {
    fn qsort(
        base: *mut c_void,
        count: usize,
        width: usize,
        compare: extern "C" fn(*const c_void, *const c_void) -> i32,
    );
}

/// Reads a field of a null pointer.
#[inline(never)]
extern "C" fn compare_and_crash(_: *const c_void, _: *const c_void) -> i32 {
    // SAFETY: none — the point is to crash.
    unsafe { std::ptr::read_volatile(std::hint::black_box(0x10usize) as *const i32) }
}

/// Crashes inside the C runtime's `qsort`, in the comparison it calls.
#[inline(never)]
fn crash_in_a_callback() {
    let mut v = [2u32, 1];
    // SAFETY: two elements of their width; the comparison crashes.
    unsafe { qsort(v.as_mut_ptr().cast(), 2, 4, compare_and_crash) };
    std::hint::black_box(v);
}

unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
    let Some(path) = std::env::var_os(CHILD) else {
        return 0;
    };
    let mut text = String::new();
    // SAFETY: what Windows passed this filter.
    unsafe { report_exception(info, &mut |s| text.push_str(s)) };
    let _ = std::fs::write(path, text);
    // EXCEPTION_EXECUTE_HANDLER: the process ends, no debugger is asked.
    1
}

/// The child's part of [`a_crash_is_reported_from_where_it_happened`]
/// (nothing in any other run).
#[test]
fn crash_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    // SAFETY: a filter with the signature Windows wants.
    unsafe { SetUnhandledExceptionFilter(Some(filter)) };
    crash_in_a_callback();
}

#[test]
fn a_crash_is_reported_from_where_it_happened() {
    if std::env::var_os(CHILD).is_some() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("faderframe-crash-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a folder");
    let report = dir.join("report.txt");
    let status = std::process::Command::new(std::env::current_exe().expect("this program"))
        .args(["crash_child", "--exact", "--test-threads=1", "--nocapture"])
        .env(CHILD, &report)
        .status()
        .expect("the child ran");
    assert!(!status.success(), "it crashed: {status:?}");
    let text = std::fs::read_to_string(&report).expect("a report");
    let _ = std::fs::remove_dir_all(&dir);
    let me = program_name();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with(&format!("in {me}+0x")), "{text}");
    assert_eq!(
        lines[1],
        "reading 0x0000000000000010 (an address near zero: a null pointer, or a field of one)"
    );
    assert!(text.contains("registers:\n"), "{text}");
    let stack: Vec<&str> = lines
        .iter()
        .skip_while(|l| !l.starts_with("stack, from the crash ("))
        .skip(1)
        .take_while(|l| !l.is_empty())
        .copied()
        .collect();
    let in_program = |l: &str| l.contains(&format!("  {me}+0x"));
    assert!(in_program(stack[0]), "the comparison crashed: {text}");
    let runtime = stack
        .iter()
        .position(|l| !in_program(l))
        .expect("a frame in the C runtime");
    assert!(
        stack[runtime..].iter().any(|l| in_program(l)),
        "back in the program after the C runtime: {text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains(" ntdll.dll ") && l.starts_with("  0x")),
        "the modules are listed: {text}"
    );
}
