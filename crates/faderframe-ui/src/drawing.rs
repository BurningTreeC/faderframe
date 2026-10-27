//! Which renderer GTK draws the windows with (Preferences → General →
//! Renderer): GTK's own choice, OpenGL, Vulkan, or software (cairo, on the
//! CPU). GTK reads it once, from `GSK_RENDERER`, so it is chosen before GTK
//! starts and applies from the next start.
//!
//! A graphics driver that crashes under GTK's renderer (GTK 4.22 with Mesa
//! 26.1's `libgallium` copying past a buffer GTK uploads, #22) takes the
//! program down while it draws. When the last run crashed inside the
//! graphics stack and the renderer is GTK's choice, this run draws in
//! software instead, kept as the preference, and the crash dialog says so.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// The renderers offered: preference id, label, `GSK_RENDERER` value.
pub const RENDERERS: [(&str, &str, &str); 4] = [
    ("", "Automatic (GTK's choice)", ""),
    ("gl", "OpenGL", "gl"),
    ("vulkan", "Vulkan", "vulkan"),
    ("software", "Software (slower, no graphics driver)", "cairo"),
];

/// This start switched to software drawing after a crash in the graphics
/// driver.
static SWITCHED: AtomicBool = AtomicBool::new(false);

/// Whether this start switched to software drawing after the last run
/// crashed in the graphics driver.
pub fn switched_to_software() -> bool {
    SWITCHED.load(Ordering::Relaxed)
}

/// Whether GTK draws in software (no GPU textures reach it: frames go to
/// it as pixels).
pub fn software() -> bool {
    std::env::var("GSK_RENDERER").as_deref() == Ok("cairo")
}

/// Choose GTK's renderer: `GSK_RENDERER` when set, else the preference,
/// else software after a crash in the graphics driver. Call it first in
/// `main` (the program's own run, not a helper's), before any thread
/// exists.
pub fn prepare_renderer() {
    if std::env::var_os("GSK_RENDERER").is_some() {
        return;
    }
    let mut prefs = crate::prefs::Preferences::load();
    if prefs.renderer.is_empty() && crash_in_the_graphics_driver(&crate::crash::reports_dir()) {
        prefs.renderer = "software".into();
        if let Err(e) = prefs.save() {
            eprintln!("faderframe: cannot save preferences: {e}");
        }
        SWITCHED.store(true, Ordering::Relaxed);
    }
    let Some((_, _, value)) = RENDERERS.iter().find(|r| r.0 == prefs.renderer) else {
        return;
    };
    if !value.is_empty() {
        // SAFETY: no other thread exists yet (the caller's promise), so
        // nothing reads the environment meanwhile.
        unsafe { std::env::set_var("GSK_RENDERER", value) };
    }
}

/// Whether a run that did not close left a crash report in `dir` whose
/// backtrace runs through the graphics stack (its markers stay for the
/// crash reporter).
fn crash_in_the_graphics_driver(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let gone = name
            .strip_prefix("running-")
            .and_then(|p| p.parse::<u32>().ok())
            .is_some_and(|pid| pid != std::process::id() && !crate::crash::alive(pid));
        gone && std::fs::read_to_string(e.path())
            .ok()
            .and_then(|marker| {
                marker
                    .lines()
                    .find_map(|l| l.strip_prefix("report=").map(str::to_string))
            })
            .and_then(|report| std::fs::read_to_string(report).ok())
            .is_some_and(|report| in_the_graphics_driver(&report))
    })
}

/// Whether a crash report's native backtrace runs through a graphics
/// driver (Mesa's, NVIDIA's, AMD's or Intel's GL or Vulkan; the GL
/// dispatch libraries).
fn in_the_graphics_driver(report: &str) -> bool {
    const DRIVERS: [&str; 14] = [
        "libgallium",
        "_dri.so",
        "libGLX_",
        "libEGL_",
        "libGLdispatch",
        "libnvidia-glcore",
        "libnvidia-eglcore",
        "libvulkan_",
        "libnvidia-gpucomp",
        "nvoglv64",
        "atio6axx",
        "amdvlk64",
        "ig9icd64",
        "igxelpicd64",
    ];
    report.contains("CRASH:")
        && report
            .lines()
            .skip_while(|l| !l.starts_with("native backtrace"))
            .any(|l| DRIVERS.iter().any(|d| l.contains(d)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GALLIUM: &str = "FaderFrame 0.30.0
CRASH: SIGSEGV (invalid memory access) at address 0x7fbf6ff58000
thread 'faderframe' (tid 19988), time 20261024-091822 UTC
native backtrace (module(+offset) [address]):
/opt/faderframe/bin/faderframe(+0x251e942) [0x55efc9eff942]
/lib/x86_64-linux-gnu/libc.so.6(+0x3fd70) [0x7fbfcf15bd70]
/lib/x86_64-linux-gnu/libgallium-26.1.6-1~bpo13+1.so(+0x19d244d) [0x7fbfba7d244d]
/opt/faderframe/lib/libgtk-4.so.1(gsk_renderer_render+0x142) [0x7fbfd03ecd92]
";

    /// The report of #22 is a crash in the graphics driver; a crash in
    /// FaderFrame's own code, or a panic, is not.
    #[test]
    fn a_crash_in_mesa_is_told_from_others() {
        assert!(in_the_graphics_driver(GALLIUM));
        let ours = GALLIUM.replace("libgallium-26.1.6-1~bpo13+1.so", "libfoo.so");
        assert!(!in_the_graphics_driver(&ours));
        assert!(!in_the_graphics_driver("PANIC at libgallium"));
    }

    /// A run that did not close with such a report is found (the marker
    /// stays for the crash reporter); a running one, or one that closed,
    /// is not.
    #[test]
    fn the_last_runs_report_is_read_where_its_marker_points() {
        let dir = std::env::temp_dir().join(format!("ff-drawing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!crash_in_the_graphics_driver(&dir));
        let report = dir.join("crash-20261024-091747-19988.log");
        std::fs::write(&report, GALLIUM).unwrap();
        // Our own process is running: never a run that did not close.
        let own = dir.join(format!("running-{}", std::process::id()));
        std::fs::write(&own, format!("report={}\n", report.display())).unwrap();
        assert!(!crash_in_the_graphics_driver(&dir));
        std::fs::remove_file(&own).unwrap();
        // A process that is gone.
        let marker = dir.join(format!("running-{}", u32::MAX - 7));
        std::fs::write(
            &marker,
            format!("started=20261024-091747\nreport={}\n", report.display()),
        )
        .unwrap();
        assert!(crash_in_the_graphics_driver(&dir));
        assert!(marker.exists(), "left for the crash reporter");
        std::fs::write(&report, "FaderFrame 0.30.0\nPANIC: index out of bounds\n").unwrap();
        assert!(!crash_in_the_graphics_driver(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
