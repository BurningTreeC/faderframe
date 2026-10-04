//! Plugin formats available to the application: CLAP and VST3 (scanned in
//! helper processes, cached) next to the built-ins, and Audio Units on
//! macOS (listed from the system's component registry). CLAP and VST3
//! instances run in helper processes while sandboxing is on
//! (`faderframe-plugin-sandbox`; Preferences → General); a helper is this
//! program started as `faderframe --plugin-sandbox` ([`sandbox_helper`]).

use faderframe_plugin_host::scan::{ScanCache, ScannedPlugin};
use faderframe_project::PluginFormat;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

fn cache_path(name: &str) -> PathBuf {
    crate::paths::cache_dir().join(name)
}

const CLAP_CACHE: &str = "clap-scan.json";
const VST3_CACHE: &str = "vst3-scan.json";

/// The application's plugin factories hosting in this process.
pub fn in_process_registry() -> faderframe_plugin_host::PluginRegistry {
    let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
    r.add_factory(Box::new(faderframe_plugin_clap::ClapFactory::new()));
    r.add_factory(Box::new(faderframe_plugin_vst3::Vst3Factory::new()));
    #[cfg(target_os = "macos")]
    r.add_factory(Box::new(faderframe_plugin_au::AuFactory::new()));
    r
}

fn load_catalogs() {
    let clap = ScanCache::load(&cache_path(CLAP_CACHE));
    faderframe_plugin_clap::set_catalog(clap.plugins().cloned().collect());
    let vst3 = ScanCache::load(&cache_path(VST3_CACHE));
    faderframe_plugin_vst3::set_catalog(vst3.plugins().cloned().collect());
}

/// Install the plugin registry (built-ins in process; CLAP and VST3 in
/// helper processes while sandboxing is on) and publish the cached
/// catalogs. Must run before the first engine is created.
pub fn install() {
    use faderframe_plugin_sandbox::SandboxedFactory;
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(SandboxedFactory::new(Box::new(
            faderframe_plugin_clap::ClapFactory::new(),
        ))));
        r.add_factory(Box::new(SandboxedFactory::new(Box::new(
            faderframe_plugin_vst3::Vst3Factory::new(),
        ))));
        #[cfg(target_os = "macos")]
        r.add_factory(Box::new(faderframe_plugin_au::AuFactory::new()));
        r
    });
    load_catalogs();
    if let Ok(exe) = std::env::current_exe() {
        faderframe_plugin_sandbox::set_launcher(faderframe_plugin_sandbox::Launcher {
            exe,
            args: vec!["--plugin-sandbox".into()],
            env: Vec::new(),
        });
    }
    faderframe_plugin_sandbox::set_enabled(crate::prefs::Preferences::load().sandbox_plugins);
}

/// `faderframe --plugin-sandbox`: host the one plugin FaderFrame asks for;
/// returns the exit code.
pub fn sandbox_helper() -> i32 {
    load_catalogs();
    #[cfg(unix)]
    {
        faderframe_plugin_sandbox::child::run(in_process_registry())
    }
    #[cfg(not(unix))]
    {
        eprintln!("plugin sandboxing is not available on this platform yet");
        2
    }
}

/// Changes whenever a catalog changes (views refresh on change).
pub fn catalog_generation() -> u64 {
    let g = faderframe_plugin_clap::catalog_generation()
        .wrapping_add(faderframe_plugin_vst3::catalog_generation() << 32);
    #[cfg(target_os = "macos")]
    let g = g.wrapping_add(faderframe_plugin_au::catalog_generation() << 48);
    g
}

/// What the scan found about a CLAP or VST3 plugin.
pub fn scanned(format: PluginFormat, id: &str) -> Option<ScannedPlugin> {
    let catalog = match format {
        PluginFormat::Clap => faderframe_plugin_clap::catalog(),
        PluginFormat::Vst3 => faderframe_plugin_vst3::catalog(),
        #[cfg(target_os = "macos")]
        PluginFormat::AudioUnit => faderframe_plugin_au::catalog(),
        _ => return None,
    };
    catalog.into_iter().find(|p| p.id == id)
}

/// Result of a background scan, for the status bar.
pub struct ScanReport {
    pub clap: usize,
    pub vst3: usize,
    pub new_errors: Vec<String>,
}

/// Rescan the CLAP and VST3 folders in the background (each new or changed
/// bundle in a `faderframe --scan-clap` / `--scan-vst3` helper process).
/// The catalogs are updated when it finishes.
pub fn scan_in_background() -> mpsc::Receiver<ScanReport> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("faderframe-plugin-scan".into())
        .spawn(move || {
            let Ok(exe) = std::env::current_exe() else {
                return;
            };
            let timeout = Duration::from_secs(60);
            let mut errors = Vec::new();
            let mut scan = |name: &str, bundles: Vec<PathBuf>, flag: &str| {
                let path = cache_path(name);
                let mut cache = ScanCache::load(&path);
                errors.extend(cache.update(&bundles, |b| {
                    tracing::info!("scanning {}", b.display());
                    faderframe_plugin_host::scan::scan_in_subprocess(&exe, flag, b, timeout)
                }));
                if let Err(e) = cache.save(&path) {
                    tracing::warn!("cannot save the plugin scan cache: {e}");
                }
                cache.plugins().cloned().collect::<Vec<_>>()
            };
            let clap = scan(
                CLAP_CACHE,
                faderframe_plugin_clap::scan::find_bundles(
                    &faderframe_plugin_clap::scan::default_paths(),
                ),
                "--scan-clap",
            );
            let vst3 = scan(
                VST3_CACHE,
                faderframe_plugin_vst3::scan::find_bundles(
                    &faderframe_plugin_vst3::scan::default_paths(),
                ),
                "--scan-vst3",
            );
            let report = ScanReport {
                clap: clap.len(),
                vst3: vst3.len(),
                new_errors: errors,
            };
            faderframe_plugin_clap::set_catalog(clap);
            faderframe_plugin_vst3::set_catalog(vst3);
            let _ = tx.send(report);
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start the plugin scan: {e}");
    }
    rx
}
