//! What a crash looks like from inside, for crash reports.
//!
//! On Windows ([`report_exception`], [`report_stack`]): the exception (its
//! name, the module and offset it happened at, what it touched — "reading
//! 0x10" says a null pointer), the registers, the stack walked from the
//! faulting instruction with the unwind tables (`RtlVirtualUnwind`) as
//! module + offset, the nearest exported name for libraries that export
//! any (GTK, GLib, plugins, the system), and the loaded modules with their
//! link time stamps and PDB ids. Nothing is resolved in the process: the
//! program is stripped, so its frames are looked up afterwards in the
//! release's symbols (`scripts/symbolize_crash.py`), which a frame's
//! module + offset names exactly.
//!
//! The parts that only arrange what was found (PE images, names, the
//! report's layout) are pure and tested everywhere; the walk and the
//! module list are Windows code, tested there (and under Wine).

#![deny(unsafe_code)]

pub mod pe;

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[cfg(windows)]
pub use windows::{
    export_below, modules, os_thread_id, report_exception, report_stack, stack_here,
};

/// Exception codes the report says more about.
pub mod code {
    pub const ACCESS_VIOLATION: u32 = 0xC000_0005;
    pub const IN_PAGE_ERROR: u32 = 0xC000_0006;
    pub const STACK_OVERFLOW: u32 = 0xC000_00FD;
}

/// What an exception code means, in a few words.
pub fn exception_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "access violation",
        0xC000_0006 => "in-page error (the file behind mapped memory could not be read)",
        0xC000_0008 => "invalid handle",
        0xC000_001D => "illegal instruction",
        0xC000_0025 => "noncontinuable exception",
        0xC000_008C => "array bounds exceeded",
        0xC000_008D => "floating-point denormal operand",
        0xC000_008E => "floating-point division by zero",
        0xC000_008F => "floating-point inexact result",
        0xC000_0090 => "floating-point invalid operation",
        0xC000_0091 => "floating-point overflow",
        0xC000_0092 => "floating-point stack check",
        0xC000_0093 => "floating-point underflow",
        0xC000_0094 => "integer division by zero",
        0xC000_0095 => "integer overflow",
        0xC000_0096 => "privileged instruction",
        0xC000_00FD => "stack overflow",
        0xC000_0135 => "a DLL was not found",
        0xC000_0139 => "an entry point was not found",
        0xC000_0142 => "a DLL failed to initialise",
        0xC000_0374 => "heap corruption",
        0xC000_0409 => "stack buffer overrun (a fast fail)",
        0xC000_0420 => "assertion failure",
        0x8000_0002 => "misaligned data",
        0x8000_0003 => "breakpoint",
        0xE06D_7363 => "C++ exception (Microsoft C++)",
        0x2047_4343 => "C++ exception (GCC)",
        0xE043_4352 => ".NET exception",
        _ => "exception",
    }
}

/// What an access violation or in-page error touched (from the record's
/// parameters), e.g. "reading 0x0000000000000010 (…a null pointer…)".
pub fn access(code: u32, info: &[u64]) -> Option<String> {
    if !matches!(code, code::ACCESS_VIOLATION | code::IN_PAGE_ERROR) || info.len() < 2 {
        return None;
    }
    let what = match info[0] {
        0 => "reading",
        1 => "writing",
        8 => "executing (the data execution prevention stopped it)",
        _ => "touching",
    };
    let mut s = format!("{what} 0x{:016x}", info[1]);
    // Windows never maps the first 64 KiB.
    if info[1] < 0x1_0000 {
        s.push_str(" (an address near zero: a null pointer, or a field of one)");
    }
    if code == code::IN_PAGE_ERROR && info.len() >= 3 {
        s.push_str(&format!(", status 0x{:08x}", info[2]));
    }
    Some(s)
}

/// A module loaded in the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub base: u64,
    pub size: u64,
    /// Its file name (`faderframe.exe`, `libgtk-4-1.dll`).
    pub name: String,
    /// Where it was loaded from.
    pub path: String,
    /// The link time its headers name.
    pub time_stamp: Option<u32>,
    /// The PDB it was linked with, if it names one.
    pub pdb: Option<pe::CodeView>,
}

impl Module {
    pub fn contains(&self, address: u64) -> bool {
        address >= self.base && address - self.base < self.size
    }
}

/// The module `address` is in.
pub fn module_of(modules: &[Module], address: u64) -> Option<&Module> {
    modules.iter().find(|m| m.contains(address))
}

/// Finds the export nearest below an offset into a module (its name and
/// how far past it): [`pe::export_below`] on the module's image.
pub type Exports<'a> = &'a dyn Fn(&Module, u32) -> Option<(String, u32)>;

/// An export further away than this names nothing useful (a static
/// function between exports, far from any of them).
const NEAR: u32 = 0x1_0000;

/// Where `address` is: `module+0xoffset`, and the nearest export when
/// there is one close below. `returns` says that the address is one a
/// call returns to: its export is found for the call itself (the byte
/// before), which may be the last of the function before.
pub fn place(modules: &[Module], address: u64, returns: bool, exports: Exports) -> String {
    let Some(m) = module_of(modules, address) else {
        return "(in no module)".into();
    };
    let offset = address - m.base;
    let mut s = format!("{}+0x{offset:x}", m.name);
    let back = u64::from(returns && offset > 0);
    if let Ok(looked_up) = u32::try_from(offset - back)
        && let Some((name, past)) = exports(m, looked_up)
        && past < NEAR
    {
        s.push_str(&format!(" (near {name}+0x{:x})", u64::from(past) + back));
    }
    s
}

/// A stack shows at most this many frames from its top and from its
/// bottom (a stack overflow's thousands are the same few).
const SHOWN_FIRST: usize = 96;
const SHOWN_LAST: usize = 32;

/// A stack, a frame a line: its number, its address and [`place`]. Runs
/// of the same address (a function calling itself) are written once.
pub fn describe_stack(title: &str, frames: &[u64], modules: &[Module], exports: Exports) -> String {
    let mut s =
        format!("{title} (module+offset; after the first, the addresses calls return to):\n");
    if frames.is_empty() {
        s.push_str("  (no frames: the stack could not be walked)\n");
        return s;
    }
    let n = frames.len();
    let left_out = (n > SHOWN_FIRST + SHOWN_LAST).then(|| SHOWN_FIRST..n - SHOWN_LAST);
    let mut i = 0;
    while i < n {
        if let Some(r) = &left_out
            && i == r.start
        {
            s.push_str(&format!("  ... {} frames left out ...\n", r.len()));
            i = r.end;
            continue;
        }
        let pc = frames[i];
        let mut run = 1;
        while i + run < n
            && frames[i + run] == pc
            && left_out.as_ref().is_none_or(|r| i + run != r.start)
        {
            run += 1;
        }
        s.push_str(&format!(
            "  {i:>4}  0x{pc:016x}  {}\n",
            place(modules, pc, i > 0, exports)
        ));
        if run > 1 {
            s.push_str(&format!("        (the same {} more times)\n", run - 1));
        }
        i += run;
    }
    s
}

/// The loaded modules, a line each: where, how long, the link time stamp
/// and PDB id (a symbol server's keys) and the file.
pub fn describe_modules(modules: &[Module]) -> String {
    let width = modules
        .iter()
        .map(|m| m.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(40);
    let mut s = String::from("modules (where, how long, link time stamp, PDB id, file):\n");
    for m in modules {
        s.push_str(&format!(
            "  0x{:016x} 0x{:08x} {:<width$}",
            m.base, m.size, m.name
        ));
        if let Some(t) = m.time_stamp {
            s.push_str(&format!(" stamp {t:08x}"));
        }
        match &m.pdb {
            Some(p) if p.file_name().is_empty() => s.push_str(&format!(" pdb {}", p.id())),
            Some(p) => s.push_str(&format!(" pdb {} {}", p.file_name(), p.id())),
            None => {}
        }
        s.push_str(&format!("  {}\n", m.path));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modules() -> Vec<Module> {
        vec![
            Module {
                base: 0x1_4000_0000,
                size: 0x10_0000,
                name: "faderframe.exe".into(),
                path: r"C:\FaderFrame\bin\faderframe.exe".into(),
                time_stamp: Some(0x6730_f1a2),
                pdb: None,
            },
            Module {
                base: 0x7ffb_1500_0000,
                size: 0x40_0000,
                name: "libgtk-4-1.dll".into(),
                path: r"C:\FaderFrame\bin\libgtk-4-1.dll".into(),
                time_stamp: Some(0),
                pdb: Some(pe::CodeView {
                    guid: [0; 16],
                    age: 1,
                    pdb: r"D:\build\gtk.pdb".into(),
                }),
            },
        ]
    }

    /// GTK exports `gtk_widget_snapshot` at 0x1000 and nothing after
    /// 0x30000.
    fn exports(m: &Module, offset: u32) -> Option<(String, u32)> {
        (m.name == "libgtk-4-1.dll" && offset >= 0x1000)
            .then(|| ("gtk_widget_snapshot".into(), offset - 0x1000))
    }

    #[test]
    fn an_address_is_placed_in_its_module_and_near_its_export() {
        let m = modules();
        assert_eq!(
            place(&m, 0x1_4000_1234, false, &exports),
            "faderframe.exe+0x1234"
        );
        assert_eq!(
            place(&m, 0x7ffb_1500_1048, false, &exports),
            "libgtk-4-1.dll+0x1048 (near gtk_widget_snapshot+0x48)"
        );
        assert_eq!(
            place(&m, 0x7ffb_1520_0000, false, &exports),
            "libgtk-4-1.dll+0x200000",
            "too far from the export to name it"
        );
        assert_eq!(place(&m, 0x10, false, &exports), "(in no module)");
        // A return address right after the export's start: its call was
        // before it, so the export below that is looked for.
        assert_eq!(
            place(&m, 0x7ffb_1500_1000, true, &exports),
            "libgtk-4-1.dll+0x1000"
        );
    }

    #[test]
    fn a_stack_is_a_frame_a_line_with_runs_written_once() {
        let m = modules();
        let frames = [
            0x7ffb_1500_1048,
            0x1_4000_0100,
            0x1_4000_0100,
            0x1_4000_0100,
            0x1_4000_0200,
        ];
        let s = describe_stack("stack", &frames, &m, &exports);
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines[0].starts_with("stack (module+offset"), "{s}");
        assert_eq!(
            lines[1],
            "     0  0x00007ffb15001048  libgtk-4-1.dll+0x1048 (near gtk_widget_snapshot+0x48)"
        );
        assert_eq!(lines[2], "     1  0x0000000140000100  faderframe.exe+0x100");
        assert_eq!(lines[3], "        (the same 2 more times)");
        assert_eq!(lines[4], "     4  0x0000000140000200  faderframe.exe+0x200");
        assert_eq!(lines.len(), 5);
        assert!(describe_stack("stack", &[], &m, &exports).contains("could not be walked"));
    }

    #[test]
    fn a_deep_stack_shows_its_top_and_its_bottom() {
        let m = modules();
        let frames: Vec<u64> = (0..1000).map(|i| 0x1_4000_0000 + 16 * i).collect();
        let s = describe_stack("stack", &frames, &m, &exports);
        assert!(s.contains(&format!(
            "  ... {} frames left out ...",
            1000 - SHOWN_FIRST - SHOWN_LAST
        )));
        assert!(s.contains("     0  0x0000000140000000"));
        assert!(s.contains("   999  0x0000000140003e70"));
        assert_eq!(s.lines().count(), 1 + SHOWN_FIRST + 1 + SHOWN_LAST);
    }

    #[test]
    fn modules_are_listed_with_what_finds_their_symbols() {
        let s = describe_modules(&modules());
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(
            lines[1],
            r"  0x0000000140000000 0x00100000 faderframe.exe stamp 6730f1a2  C:\FaderFrame\bin\faderframe.exe"
        );
        let pdb = format!(
            "libgtk-4-1.dll stamp 00000000 pdb gtk.pdb {}1",
            "0".repeat(32)
        );
        assert!(lines[2].contains(&pdb), "{s}");
    }

    #[test]
    fn an_access_violation_says_what_it_touched() {
        assert_eq!(
            access(code::ACCESS_VIOLATION, &[0, 0x10]).as_deref(),
            Some(
                "reading 0x0000000000000010 (an address near zero: a null pointer, or a field of one)"
            )
        );
        assert_eq!(
            access(code::ACCESS_VIOLATION, &[1, 0x7ff0_0000_1000]).as_deref(),
            Some("writing 0x00007ff000001000")
        );
        assert!(
            access(code::ACCESS_VIOLATION, &[8, 0xdead_0000])
                .is_some_and(|s| s.starts_with("executing"))
        );
        assert_eq!(access(code::STACK_OVERFLOW, &[0, 0]), None);
        assert_eq!(access(code::ACCESS_VIOLATION, &[0]), None);
        assert_eq!(exception_name(0xC000_0005), "access violation");
        assert_eq!(exception_name(0x1234_5678), "exception");
    }
}
