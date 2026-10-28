//! Which renderer GTK draws the windows with (Preferences → General →
//! Renderer): GTK's own choice, OpenGL, Vulkan, or software (cairo, on the
//! CPU). GTK reads it once, from `GSK_RENDERER`, so it is chosen before GTK
//! starts and applies from the next start.
//!
//! A graphics driver that crashes under GTK's renderer takes the program
//! down while it draws. Mesa's crocus, the driver of Intel's graphics from
//! the GMA X3000 to Haswell, does (#22: Mesa 26.1 growing its shader cache
//! while GTK uploads a texture), so on those chips GTK's choice is software.
//! When the last run crashed inside the graphics stack and the renderer is
//! GTK's choice, this run draws in software instead, kept as the
//! preference, and the crash dialog says so.

use std::path::Path;
use std::sync::OnceLock;
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

/// This start draws in software, the renderer being GTK's choice, because
/// Mesa's crocus driver draws with the graphics chip: the chip's family.
static CROCUS_CHIP: OnceLock<&'static str> = OnceLock::new();

/// The family of the graphics chip this start draws in software for (see
/// [`CROCUS`]), the renderer being GTK's choice.
pub fn software_for_chip() -> Option<&'static str> {
    CROCUS_CHIP.get().copied()
}

/// Whether GTK draws in software (no GPU textures reach it: frames go to
/// it as pixels).
pub fn software() -> bool {
    std::env::var("GSK_RENDERER").as_deref() == Ok("cairo")
}

/// Choose GTK's renderer: `GSK_RENDERER` when set, else the preference,
/// else software after a crash in the graphics driver or on a chip Mesa's
/// crocus driver draws with. Call it first in `main` (the program's own
/// run, not a helper's), before any thread exists.
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
    let mut chosen = prefs.renderer.as_str();
    if chosen.is_empty()
        && let Some(family) = crocus_family(&chips())
    {
        let _ = CROCUS_CHIP.set(family);
        chosen = "software";
    }
    let Some((_, _, value)) = RENDERERS.iter().find(|r| r.0 == chosen) else {
        return;
    };
    if !value.is_empty() {
        // SAFETY: no other thread exists yet (the caller's promise), so
        // nothing reads the environment meanwhile.
        unsafe { std::env::set_var("GSK_RENDERER", value) };
    }
}

/// Intel's graphics chips Mesa draws with its crocus driver, by family
/// (PCI device ids from Mesa's `include/pci_ids/crocus_pci_ids.h`, which
/// Mesa's loader maps to crocus).
const CROCUS: [(&str, &[u16]); 8] = [
    (
        "GMA X3000",
        &[0x29a2, 0x2992, 0x2982, 0x2972, 0x2a02, 0x2a12],
    ),
    (
        "GMA 4500",
        &[0x2a42, 0x2e02, 0x2e12, 0x2e22, 0x2e32, 0x2e42, 0x2e92],
    ),
    ("Ironlake", &[0x0042, 0x0046]),
    (
        "Sandy Bridge",
        &[0x0102, 0x0112, 0x0122, 0x0106, 0x0116, 0x0126, 0x010a],
    ),
    (
        "Ivy Bridge",
        &[0x0152, 0x0162, 0x0156, 0x0166, 0x015a, 0x016a],
    ),
    (
        "Haswell",
        &[
            0x0402, 0x0412, 0x0422, 0x0406, 0x0416, 0x0426, 0x040a, 0x041a, 0x042a, 0x040b, 0x041b,
            0x042b, 0x040e, 0x041e, 0x042e, 0x0c02, 0x0c12, 0x0c22, 0x0c06, 0x0c16, 0x0c26, 0x0c0a,
            0x0c1a, 0x0c2a, 0x0c0b, 0x0c1b, 0x0c2b, 0x0c0e, 0x0c1e, 0x0c2e, 0x0a02, 0x0a12, 0x0a22,
            0x0a06, 0x0a16, 0x0a26, 0x0a0a, 0x0a1a, 0x0a2a, 0x0a0b, 0x0a1b, 0x0a2b, 0x0a0e, 0x0a1e,
            0x0a2e, 0x0d02, 0x0d12, 0x0d22, 0x0d06, 0x0d16, 0x0d26, 0x0d0a, 0x0d1a, 0x0d2a, 0x0d0b,
            0x0d1b, 0x0d2b, 0x0d0e, 0x0d1e, 0x0d2e,
        ],
    ),
    ("Bay Trail", &[0x0f31, 0x0f32, 0x0f33, 0x0157, 0x0155]),
    ("Cherry View", &[0x22b0, 0x22b1, 0x22b2, 0x22b3]),
];

/// A graphics chip of the system (Linux: `/sys/class/drm/card<n>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chip {
    /// `card<n>`.
    pub card: String,
    pub vendor: u16,
    pub device: u16,
    /// The kernel's driver (`i915`, `amdgpu`, …), if it says.
    pub driver: Option<String>,
    /// A display is connected to it.
    pub connected: bool,
}

impl Chip {
    /// Its family, when Mesa's crocus driver draws with it.
    pub fn crocus_family(&self) -> Option<&'static str> {
        if self.vendor != 0x8086 {
            return None;
        }
        CROCUS
            .iter()
            .find(|(_, ids)| ids.contains(&self.device))
            .map(|(family, _)| *family)
    }
}

/// The system's graphics chips (none known outside Linux).
pub(crate) fn chips() -> Vec<Chip> {
    if cfg!(target_os = "linux") {
        chips_in(Path::new("/sys/class/drm"))
    } else {
        Vec::new()
    }
}

/// The graphics chips in `drm` (a `/sys/class/drm`), by card.
fn chips_in(drm: &Path) -> Vec<Chip> {
    let Ok(entries) = std::fs::read_dir(drm) else {
        return Vec::new();
    };
    let names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let id = |path: std::path::PathBuf| {
        let text = std::fs::read_to_string(path).ok()?;
        u16::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
    };
    let mut chips: Vec<Chip> = names
        .iter()
        .filter(|n| {
            n.strip_prefix("card")
                .is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|card| {
            let device = drm.join(card).join("device");
            let connector = format!("{card}-");
            Some(Chip {
                vendor: id(device.join("vendor"))?,
                device: id(device.join("device"))?,
                driver: std::fs::read_link(device.join("driver"))
                    .ok()
                    .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned())),
                connected: names.iter().any(|n| {
                    n.starts_with(&connector)
                        && std::fs::read_to_string(drm.join(n).join("status"))
                            .is_ok_and(|s| s.trim() == "connected")
                }),
                card: card.clone(),
            })
        })
        .collect();
    chips.sort_by(|a, b| a.card.cmp(&b.card));
    chips
}

/// The family of the crocus chip GTK draws with, if it is one: among the
/// chips with a display connected (all of them when none says so).
fn crocus_family(chips: &[Chip]) -> Option<&'static str> {
    let any_connected = chips.iter().any(|c| c.connected);
    chips
        .iter()
        .filter(|c| c.connected || !any_connected)
        .find_map(Chip::crocus_family)
}

/// The graphics chips for a crash report's header (`None`: none known).
pub(crate) fn chips_line() -> Option<String> {
    let chips = chips();
    (!chips.is_empty()).then(|| {
        chips
            .iter()
            .map(|c| {
                let mut s = format!("{} {:04x}:{:04x}", c.card, c.vendor, c.device);
                if let Some(d) = &c.driver {
                    s.push_str(&format!(" {d}"));
                }
                if let Some(f) = c.crocus_family() {
                    s.push_str(&format!(" (crocus, {f})"));
                }
                if c.connected {
                    s.push_str(", display");
                }
                s
            })
            .collect::<Vec<_>>()
            .join("; ")
    })
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

    /// A fake `/sys/class/drm`: per card (vendor, device, driver,
    /// connector statuses).
    #[cfg(unix)]
    fn drm(name: &str, cards: &[(u16, u16, &str, &[&str])]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ff-drm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (k, (vendor, device, driver, statuses)) in cards.iter().enumerate() {
            let card = dir.join(format!("card{k}"));
            let dev = card.join("device");
            std::fs::create_dir_all(dev.join("drivers").join(driver)).unwrap();
            std::fs::write(dev.join("vendor"), format!("0x{vendor:04x}\n")).unwrap();
            std::fs::write(dev.join("device"), format!("0x{device:04x}\n")).unwrap();
            std::os::unix::fs::symlink(dev.join("drivers").join(driver), dev.join("driver"))
                .unwrap();
            for (n, status) in statuses.iter().enumerate() {
                let c = dir.join(format!("card{k}-HDMI-A-{n}"));
                std::fs::create_dir_all(&c).unwrap();
                std::fs::write(c.join("status"), format!("{status}\n")).unwrap();
            }
        }
        std::fs::create_dir_all(dir.join("renderD128")).unwrap();
        dir
    }

    /// Haswell graphics (#22's crocus) draw in software; another chip
    /// driving the display, or a newer Intel one, do not.
    #[test]
    #[cfg(unix)]
    fn crocus_chips_are_found_where_the_display_is() {
        let haswell = drm("haswell", &[(0x8086, 0x0416, "i915", &["connected"])]);
        let chips = chips_in(&haswell);
        assert_eq!(chips.len(), 1);
        assert_eq!(chips[0].driver.as_deref(), Some("i915"));
        assert_eq!(crocus_family(&chips), Some("Haswell"));
        // A discrete card drives the display, the old chip idles.
        let beside = drm(
            "beside",
            &[
                (0x8086, 0x0162, "i915", &["disconnected"]),
                (0x1002, 0x1638, "amdgpu", &["connected", "disconnected"]),
            ],
        );
        let chips = chips_in(&beside);
        assert_eq!(chips[0].crocus_family(), Some("Ivy Bridge"));
        assert_eq!(crocus_family(&chips), None);
        // No connector says: the chip is all there is to go by.
        let headless = drm("headless", &[(0x8086, 0x0102, "i915", &[])]);
        assert_eq!(crocus_family(&chips_in(&headless)), Some("Sandy Bridge"));
        // Newer Intel graphics (iris), and no such folder.
        let iris = drm("iris", &[(0x8086, 0x9a49, "i915", &["connected"])]);
        assert_eq!(crocus_family(&chips_in(&iris)), None);
        assert!(chips_in(&haswell.join("nothing")).is_empty());
        for d in [haswell, beside, headless, iris] {
            let _ = std::fs::remove_dir_all(d);
        }
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
