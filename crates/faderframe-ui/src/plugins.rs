//! Plugin formats available to the application: CLAP (scanned in helper
//! processes, cached) next to the built-ins.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

fn cache_path() -> PathBuf {
    gtk::glib::user_cache_dir()
        .join("faderframe")
        .join("clap-scan.json")
}

/// Install the plugin registry (built-ins + CLAP) and publish the cached
/// CLAP catalog. Must run before the first engine is created.
pub fn install() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(faderframe_plugin_clap::ClapFactory::new()));
        r
    });
    let cache = faderframe_plugin_clap::scan::ScanCache::load(&cache_path());
    faderframe_plugin_clap::set_catalog(cache.plugins().cloned().collect());
}

/// Result of a background scan, for the status bar.
pub struct ScanReport {
    pub plugins: usize,
    pub new_errors: Vec<String>,
}

/// Rescan the CLAP folders in the background (each new or changed bundle
/// in a `faderframe --scan-clap` helper process). The catalog is updated
/// when it finishes.
pub fn scan_in_background() -> mpsc::Receiver<ScanReport> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("faderframe-plugin-scan".into())
        .spawn(move || {
            let Ok(exe) = std::env::current_exe() else {
                return;
            };
            let path = cache_path();
            let mut cache = faderframe_plugin_clap::scan::ScanCache::load(&path);
            let bundles = faderframe_plugin_clap::scan::find_bundles(
                &faderframe_plugin_clap::scan::default_paths(),
            );
            let errors = cache.update(&bundles, |b| {
                tracing::info!("scanning CLAP bundle {}", b.display());
                faderframe_plugin_clap::scan::scan_in_subprocess(&exe, b, Duration::from_secs(60))
            });
            if let Err(e) = cache.save(&path) {
                tracing::warn!("cannot save the plugin scan cache: {e}");
            }
            let plugins: Vec<_> = cache.plugins().cloned().collect();
            let n = plugins.len();
            faderframe_plugin_clap::set_catalog(plugins);
            let _ = tx.send(ScanReport {
                plugins: n,
                new_errors: errors,
            });
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start the plugin scan: {e}");
    }
    rx
}
