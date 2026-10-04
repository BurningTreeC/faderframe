//! Finding and describing VST3 plugins (in the `faderframe --scan-vst3
//! <bundle>` helper process, see [`run_scan_subprocess`]; caching and the
//! helper protocol are in [`faderframe_plugin_host::scan`]).

use crate::com::{HostApp, context};
use crate::instance::{bus_channels, create, event_buses};
use crate::module;
use crate::util::{cstr, tuid_hex};
pub use faderframe_plugin_host::scan::{ScanCache, ScanError, ScannedPlugin};
use std::path::{Path, PathBuf};
use std::time::Duration;
use vst3::Steinberg::Vst::IComponent;
use vst3::Steinberg::{
    IPluginBaseTrait, IPluginFactory2, IPluginFactory2Trait, IPluginFactory3, IPluginFactory3Trait,
    IPluginFactoryTrait, PClassInfo, PClassInfo2, PFactoryInfo, kResultOk,
};

/// The VST3 class category of processors.
const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// Standard VST3 locations of this platform, `$VST3_PATH` and a portable
/// installation's `Plug-Ins/VST3` first.
pub fn default_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::env::var_os("VST3_PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    out.extend(faderframe_core::paths::portable_plugins("VST3"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(windows) {
        if let Some(p) = std::env::var_os("COMMONPROGRAMFILES") {
            out.push(PathBuf::from(p).join("VST3"));
        }
        if let Some(p) = std::env::var_os("LOCALAPPDATA") {
            out.push(PathBuf::from(p).join("Programs/Common/VST3"));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(h) = &home {
            out.push(h.join("Library/Audio/Plug-Ins/VST3"));
        }
        out.push("/Library/Audio/Plug-Ins/VST3".into());
    } else {
        if let Some(h) = &home {
            out.push(h.join(".vst3"));
        }
        out.push("/usr/lib/vst3".into());
        out.push("/usr/local/lib/vst3".into());
    }
    out
}

pub fn find_bundles(paths: &[PathBuf]) -> Vec<PathBuf> {
    faderframe_plugin_host::scan::find_bundles(paths, "vst3")
}

/// VST3 sub-categories ("Fx|EQ", "Instrument|Synth") as feature tags in
/// CLAP's vocabulary, which the rest of FaderFrame uses.
pub fn features_of(sub_categories: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in sub_categories
        .split('|')
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        let tag = match c {
            "Fx" => "audio-effect",
            "Instrument" => "instrument",
            "Analyzer" => "analyzer",
            "EQ" => "equalizer",
            "Synth" => "synthesizer",
            "Sampler" => "sampler",
            "Drum" => "drum",
            "Pitch Shift" => "pitch-shifter",
            "Tools" => "utility",
            other => {
                out.push(other.to_lowercase().replace(' ', "-"));
                continue;
            }
        };
        out.push(tag.to_string());
    }
    // Analyzers and instruments are also listed under "Fx" by some plugins.
    if out.iter().any(|f| f == "instrument") {
        out.retain(|f| f != "audio-effect");
    }
    out.dedup();
    out
}

struct ClassInfo {
    cid: vst3::Steinberg::TUID,
    category: String,
    name: String,
    sub: String,
    vendor: String,
    version: String,
}

fn classes(m: &module::Module) -> Vec<ClassInfo> {
    let f = &m.factory;
    let f2 = f.cast::<IPluginFactory2>();
    let mut out = Vec::new();
    // SAFETY: factory queries with valid out pointers.
    unsafe {
        let mut fi: PFactoryInfo = std::mem::zeroed();
        let factory_vendor = if f.getFactoryInfo(&mut fi) == kResultOk {
            cstr(&fi.vendor)
        } else {
            String::new()
        };
        for i in 0..f.countClasses().clamp(0, 4096) {
            let info = match &f2 {
                Some(f2) => {
                    let mut c: PClassInfo2 = std::mem::zeroed();
                    (f2.getClassInfo2(i, &mut c) == kResultOk).then(|| ClassInfo {
                        cid: c.cid,
                        category: cstr(&c.category),
                        name: cstr(&c.name),
                        sub: cstr(&c.subCategories),
                        vendor: cstr(&c.vendor),
                        version: cstr(&c.version),
                    })
                }
                None => {
                    let mut c: PClassInfo = std::mem::zeroed();
                    (f.getClassInfo(i, &mut c) == kResultOk).then(|| ClassInfo {
                        cid: c.cid,
                        category: cstr(&c.category),
                        name: cstr(&c.name),
                        sub: String::new(),
                        vendor: String::new(),
                        version: String::new(),
                    })
                }
            };
            if let Some(mut info) = info {
                if info.vendor.is_empty() {
                    info.vendor = factory_vendor.clone();
                }
                out.push(info);
            }
        }
    }
    out
}

/// Describe the plugins of a loaded module.
pub fn describe_module(m: &module::Module) -> Vec<ScannedPlugin> {
    let host = HostApp::new();
    if let Some(f3) = m.factory.cast::<IPluginFactory3>() {
        // SAFETY: the host object outlives the scan.
        unsafe { f3.setHostContext(context(&host)) };
    }
    let mut out = Vec::new();
    for c in classes(m) {
        if c.category != AUDIO_MODULE_CLASS {
            continue;
        }
        let mut features = features_of(&c.sub);
        // Some vendors list themselves as a sub-category.
        let vendor = c.vendor.to_lowercase().replace(' ', "-");
        features.retain(|f| *f != vendor);
        if features.is_empty() {
            features.push("audio-effect".into());
        }
        let mut p = ScannedPlugin {
            id: tuid_hex(&c.cid),
            name: c.name,
            vendor: c.vendor,
            version: c.version,
            features,
            bundle: m.bundle.clone(),
            audio_inputs: Vec::new(),
            audio_outputs: Vec::new(),
            note_inputs: 0,
            note_outputs: 0,
        };
        // Buses need a component.
        if let Some(component) = create::<IComponent>(m, &c.cid) {
            // SAFETY: initialise/terminate around the queries.
            if unsafe { component.initialize(context(&host)) } == kResultOk {
                p.audio_inputs = bus_channels(&component, true);
                p.audio_outputs = bus_channels(&component, false);
                p.note_inputs = event_buses(&component, true);
                p.note_outputs = event_buses(&component, false);
                // SAFETY: as above.
                unsafe { component.terminate() };
            }
        }
        if p.note_inputs > 0 && !p.is_instrument() && p.audio_inputs.is_empty() {
            p.features.insert(0, "instrument".into());
        }
        p.default_ports_if_missing();
        out.push(p);
    }
    out
}

/// Load `bundle` *in this process* and describe its plugins. Use it only in
/// the scan helper process.
pub fn describe_bundle(bundle: &Path) -> Result<Vec<ScannedPlugin>, ScanError> {
    let m = module::load(bundle)?;
    Ok(describe_module(m))
}

/// Entry point of the scan helper process.
pub fn run_scan_subprocess(bundle: &Path) -> i32 {
    faderframe_plugin_host::scan::print_scan_result(describe_bundle(bundle))
}

/// Describe `bundle` by running `exe --scan-vst3 <bundle>`.
pub fn scan_in_subprocess(
    exe: &Path,
    bundle: &Path,
    timeout: Duration,
) -> Result<Vec<ScannedPlugin>, ScanError> {
    faderframe_plugin_host::scan::scan_in_subprocess(exe, "--scan-vst3", bundle, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_categories_map_to_features() {
        assert_eq!(features_of("Fx|EQ"), vec!["audio-effect", "equalizer"]);
        assert_eq!(
            features_of("Instrument|Synth"),
            vec!["instrument", "synthesizer"]
        );
        assert_eq!(
            features_of("Fx|Instrument|Drum"),
            vec!["instrument", "drum"]
        );
        assert_eq!(
            features_of("Fx|Spatial|Mono"),
            vec!["audio-effect", "spatial", "mono"]
        );
    }
}
