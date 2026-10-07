//! Finding LV2 plugins: bundles (`*.lv2` directories) on the LV2 path,
//! their `manifest.ttl` and the files it points to, described without
//! loading any plugin code (so scanning needs no helper process).

use crate::ttl::{self, DOAP_NAME, Graph, LV2, Node, RDF_TYPE, RDFS_LABEL, RDFS_SEE_ALSO, UI};
use faderframe_plugin_host::scan::ScannedPlugin;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const PG: &str = "http://lv2plug.in/ns/ext/port-groups#";
const PPROPS: &str = "http://lv2plug.in/ns/ext/port-props#";
const MIDI_EVENT: &str = "http://lv2plug.in/ns/ext/midi#MidiEvent";
const UNITS: &str = "http://lv2plug.in/ns/extensions/units#";
const FOAF_NAME: &str = "http://xmlns.com/foaf/0.1/name";
const DOAP_MAINTAINER: &str = "http://usefulinc.com/ns/doap#maintainer";

/// What a port carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortKind {
    Audio,
    Control,
    /// Atoms (a sequence): `midi` when it takes MIDI events.
    Atom {
        midi: bool,
    },
    Cv,
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Port {
    pub index: u32,
    pub symbol: String,
    pub name: String,
    pub kind: PortKind,
    pub input: bool,
    pub default: Option<f64>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub integer: bool,
    pub toggled: bool,
    pub enumeration: bool,
    pub logarithmic: bool,
    pub optional: bool,
    /// Never automated (`pprops:notAutomatic`) or not shown
    /// (`pprops:notOnGUI`).
    pub hidden: bool,
    /// Its value is the plugin's latency (`lv2:reportsLatency`,
    /// designation `lv2:latency`).
    pub latency: bool,
    /// `lv2:designation lv2:enabled` (bypass) / `lv2:freeWheeling`.
    pub enabled: bool,
    pub free_wheeling: bool,
    /// A sidechain input (`lv2:isSideChain`, or in a group that is
    /// `pg:sideChainOf` another).
    pub sidechain: bool,
    pub scale_points: Vec<(f64, String)>,
    /// Its unit's symbol (`units:unit`), when it is one the host knows.
    pub unit: Option<String>,
    /// Buffer size the plugin asks for (`rsz:minimumSize`, atoms).
    pub minimum_size: Option<u32>,
    /// An atom input that takes the transport (`time:Position`).
    pub time: bool,
    /// Its port group (`pg:group`) and whether that is the main one
    /// (`pg:mainInput`/`pg:mainOutput` of the plugin).
    pub group: Option<String>,
    pub main_group: bool,
}

/// A user interface of a plugin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ui {
    pub uri: String,
    pub required_features: Vec<String>,
    /// Its class (`ui:X11UI`, `ui:GtkUI`, …).
    pub class: String,
    pub binary: PathBuf,
    pub bundle: PathBuf,
}

impl Ui {
    pub fn is_x11(&self) -> bool {
        self.class == format!("{UI}X11UI")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub uri: String,
    pub label: String,
    /// The bundle describing it (the plugin's, or a preset bundle).
    pub bundle: PathBuf,
}

/// Everything the host needs of a plugin before loading it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lv2Plugin {
    pub uri: String,
    pub name: String,
    pub author: String,
    pub version: String,
    pub bundle: PathBuf,
    pub binary: PathBuf,
    pub classes: Vec<String>,
    pub ports: Vec<Port>,
    pub required_features: Vec<String>,
    pub optional_features: Vec<String>,
    pub extension_data: Vec<String>,
    pub uis: Vec<Ui>,
    pub presets: Vec<Preset>,
}

impl Lv2Plugin {
    /// Classed an instrument, or taking notes and making audio from them
    /// alone (no audio input).
    pub fn is_instrument(&self) -> bool {
        self.classes
            .iter()
            .any(|c| c == &format!("{LV2}InstrumentPlugin"))
            || (self.midi_port(true).is_some()
                && self.audio(true).next().is_none()
                && self.audio(false).next().is_some())
    }

    pub fn audio(&self, input: bool) -> impl Iterator<Item = &Port> {
        self.ports
            .iter()
            .filter(move |p| p.kind == PortKind::Audio && p.input == input)
    }

    /// Audio inputs: (main, sidechain) port indices in order.
    pub fn audio_inputs(&self) -> (Vec<u32>, Vec<u32>) {
        let (side, main): (Vec<&Port>, Vec<&Port>) = self.audio(true).partition(|p| p.sidechain);
        (
            main.iter().map(|p| p.index).collect(),
            side.iter().map(|p| p.index).collect(),
        )
    }

    pub fn audio_outputs(&self) -> Vec<u32> {
        self.audio(false).map(|p| p.index).collect()
    }

    /// The audio outputs as buses, main first: by port group where the
    /// plugin has groups (the `pg:mainOutput` one first), else the first
    /// two ports as the main bus and the rest in pairs.
    pub fn output_buses(&self) -> Vec<Vec<u32>> {
        let outs: Vec<&Port> = self.audio(false).collect();
        if outs.is_empty() {
            return Vec::new();
        }
        if outs.iter().all(|p| p.group.is_some()) {
            let mut buses: Vec<(bool, String, Vec<u32>)> = Vec::new();
            for p in &outs {
                let g = p.group.clone().unwrap_or_default();
                match buses.iter_mut().find(|b| b.1 == g) {
                    Some(b) => b.2.push(p.index),
                    None => buses.push((p.main_group, g, vec![p.index])),
                }
            }
            // The main group first; the others in port order.
            buses.sort_by_key(|b| !b.0);
            return buses.into_iter().map(|b| b.2).collect();
        }
        let first = outs.len().min(2);
        let mut buses = vec![outs[..first].iter().map(|p| p.index).collect::<Vec<_>>()];
        for pair in outs[first..].chunks(2) {
            buses.push(pair.iter().map(|p| p.index).collect());
        }
        buses
    }

    /// The atom input taking MIDI (the first) and the atom output giving
    /// MIDI (the first).
    pub fn midi_port(&self, input: bool) -> Option<u32> {
        self.ports
            .iter()
            .find(|p| p.input == input && p.kind == PortKind::Atom { midi: true })
            .map(|p| p.index)
    }

    /// The catalog's description of it.
    pub fn scanned(&self) -> ScannedPlugin {
        let (main, side) = self.audio_inputs();
        let outs = self.audio_outputs();
        let mut audio_inputs = Vec::new();
        if !main.is_empty() {
            audio_inputs.push(main.len().min(u16::MAX as usize) as u16);
        }
        if !side.is_empty() {
            if audio_inputs.is_empty() {
                audio_inputs.push(0);
            }
            audio_inputs.push(side.len() as u16);
        }
        let audio_outputs: Vec<u16> = self
            .output_buses()
            .iter()
            .map(|b| b.len().min(u16::MAX as usize) as u16)
            .collect();
        let mut features = Vec::new();
        if self.is_instrument() {
            features.push("instrument".to_string());
        } else if self
            .classes
            .iter()
            .any(|c| c.ends_with("#AnalyserPlugin") || c.ends_with("#SpectralPlugin"))
        {
            features.push("analyzer".to_string());
        } else if !outs.is_empty() {
            features.push("audio-effect".to_string());
        }
        ScannedPlugin {
            id: self.uri.clone(),
            name: self.name.clone(),
            vendor: self.author.clone(),
            version: self.version.clone(),
            features,
            bundle: self.bundle.clone(),
            audio_inputs,
            audio_outputs,
            note_inputs: u16::from(self.midi_port(true).is_some()),
            note_outputs: u16::from(self.midi_port(false).is_some()),
        }
    }
}

/// The LV2 path: `$LV2_PATH`, or a portable installation's `Plug-Ins/LV2`,
/// `~/.lv2` and the system directories.
pub fn default_paths() -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os("LV2_PATH") {
        return std::env::split_paths(&v).collect();
    }
    let mut out: Vec<PathBuf> = faderframe_core::paths::portable_plugins("LV2")
        .into_iter()
        .collect();
    if let Some(h) = std::env::var_os("HOME").map(PathBuf::from) {
        out.push(h.join(".lv2"));
    }
    for d in [
        "/usr/local/lib/lv2",
        "/usr/local/lib64/lv2",
        "/usr/lib/lv2",
        "/usr/lib64/lv2",
        "/usr/lib/x86_64-linux-gnu/lv2",
        "/usr/lib/aarch64-linux-gnu/lv2",
    ] {
        out.push(d.into());
    }
    out
}

/// Every bundle (a directory with a `manifest.ttl`) in `paths`.
pub fn find_bundles(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in paths {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.join("manifest.ttl").is_file())
            .collect();
        found.sort();
        out.extend(found);
    }
    out
}

fn lv2(local: &str) -> String {
    format!("{LV2}{local}")
}

/// A bundle's metadata: its manifest and every file the plugins, UIs and
/// presets there point to.
pub fn bundle_graph(bundle: &Path) -> Result<Graph, String> {
    let mut g = Graph::new();
    g.load(&bundle.join("manifest.ttl"))?;
    // Everything the manifest points to (plugin data, UIs, presets).
    let see_also: Vec<PathBuf> = g
        .of_type(&lv2("Plugin"))
        .iter()
        .chain(g.of_type(&format!("{}Preset", ttl::PSET)).iter())
        .chain(
            [
                format!("{UI}X11UI"),
                format!("{UI}GtkUI"),
                format!("{UI}Gtk3UI"),
                format!("{UI}Qt5UI"),
            ]
            .iter()
            .flat_map(|c| g.of_type(c))
            .collect::<Vec<_>>()
            .iter(),
        )
        .flat_map(|s| {
            g.objects(s, RDFS_SEE_ALSO)
                .filter_map(|o| o.iri().and_then(ttl::url_path))
                .collect::<Vec<_>>()
        })
        .collect();
    for f in see_also {
        if let Err(e) = g.load(&f) {
            tracing::warn!("LV2: {e}");
        }
    }
    Ok(g)
}

/// The presets described in a bundle's graph: (plugin URI, preset).
fn presets_in(g: &Graph, bundle: &Path) -> Vec<(String, Preset)> {
    g.of_type(&format!("{}Preset", ttl::PSET))
        .into_iter()
        .filter_map(|ps| {
            let applies = g.object(&ps, &lv2("appliesTo"))?.iri()?.to_string();
            let label = g.text(&ps, RDFS_LABEL)?;
            Some((
                applies,
                Preset {
                    uri: ps.iri()?.to_string(),
                    label,
                    bundle: bundle.to_path_buf(),
                },
            ))
        })
        .collect()
}

/// What a bundle holds: plugins (with their own presets) and presets of
/// plugins described elsewhere (a user's preset bundle).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub plugins: Vec<Lv2Plugin>,
    pub presets: Vec<(String, Preset)>,
}

/// The plugins of `bundle`.
pub fn describe_bundle(bundle: &Path) -> Result<Bundle, String> {
    let g = bundle_graph(bundle)?;
    let mut out = Bundle::default();
    for p in g.of_type(&lv2("Plugin")) {
        let Some(uri) = p.iri().map(str::to_string) else {
            continue;
        };
        match describe_plugin(&g, &p, &uri, bundle) {
            Ok(d) => out.plugins.push(d),
            Err(e) => tracing::warn!("LV2 {uri}: {e}"),
        }
    }
    out.presets = presets_in(&g, bundle)
        .into_iter()
        .filter(|(applies, _)| !out.plugins.iter().any(|p| &p.uri == applies))
        .collect();
    Ok(out)
}

/// Plugins with the presets other bundles have for them; the first of
/// two plugins with one URI wins.
fn merge<'a>(bundles: impl Iterator<Item = &'a Bundle>) -> Vec<Lv2Plugin> {
    let bundles: Vec<&Bundle> = bundles.collect();
    let mut out: Vec<Lv2Plugin> = Vec::new();
    for b in &bundles {
        for p in &b.plugins {
            if !out.iter().any(|q| q.uri == p.uri) {
                out.push(p.clone());
            }
        }
    }
    for b in &bundles {
        for (applies, preset) in &b.presets {
            if let Some(p) = out.iter_mut().find(|p| &p.uri == applies)
                && !p.presets.iter().any(|q| q.uri == preset.uri)
            {
                p.presets.push(preset.clone());
            }
        }
    }
    for p in &mut out {
        p.presets.sort_by(|a, b| a.label.cmp(&b.label));
    }
    out
}

/// (Total size, latest modification) of a bundle's Turtle files and the
/// directory itself.
fn stamp(bundle: &Path) -> Option<(u64, u64)> {
    fn secs(m: &std::fs::Metadata) -> u64 {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    }
    let dir = std::fs::metadata(bundle).ok()?;
    let mut acc = (0, secs(&dir));
    for e in std::fs::read_dir(bundle).ok()?.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "ttl")
            && let Ok(m) = e.metadata()
        {
            acc.0 += m.len();
            acc.1 = acc.1.max(secs(&m));
        }
    }
    Some(acc)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime: u64,
    bundle: Bundle,
    #[serde(default)]
    error: Option<String>,
}

/// Descriptions by bundle, read again only when a bundle's Turtle files
/// change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cache {
    version: u32,
    bundles: BTreeMap<PathBuf, CacheEntry>,
}

const CACHE_VERSION: u32 = 1;

impl Cache {
    pub fn load(path: &Path) -> Cache {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<Cache>(&t).ok())
            .filter(|c| c.version == CACHE_VERSION)
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// Bring it up to date with `bundles`; returns whether anything
    /// changed and the errors of bundles that failed.
    pub fn update(&mut self, bundles: &[PathBuf]) -> (bool, Vec<String>) {
        self.version = CACHE_VERSION;
        let before = self.bundles.len();
        self.bundles.retain(|p, _| bundles.contains(p));
        let mut changed = self.bundles.len() != before;
        let mut errors = Vec::new();
        for b in bundles {
            let Some((size, mtime)) = stamp(b) else {
                continue;
            };
            if self
                .bundles
                .get(b)
                .is_some_and(|e| e.size == size && e.mtime == mtime)
            {
                continue;
            }
            changed = true;
            let (bundle, error) = match describe_bundle(b) {
                Ok(d) => (d, None),
                Err(e) => {
                    errors.push(format!("{}: {e}", b.display()));
                    (Bundle::default(), Some(e))
                }
            };
            self.bundles.insert(
                b.clone(),
                CacheEntry {
                    size,
                    mtime,
                    bundle,
                    error,
                },
            );
        }
        (changed, errors)
    }

    pub fn plugins(&self) -> Vec<Lv2Plugin> {
        merge(self.bundles.values().map(|e| &e.bundle))
    }
}

fn describe_plugin(g: &Graph, p: &Node, uri: &str, bundle: &Path) -> Result<Lv2Plugin, String> {
    let binary = g
        .object(p, &lv2("binary"))
        .and_then(Node::iri)
        .and_then(ttl::url_path)
        .ok_or("no lv2:binary")?;
    let name = g.text(p, DOAP_NAME).unwrap_or_else(|| uri.to_string());
    let author = g
        .object(p, DOAP_MAINTAINER)
        .and_then(|m| g.text(m, FOAF_NAME))
        .or_else(|| {
            g.object(p, &lv2("project"))
                .and_then(|pr| g.object(pr, DOAP_MAINTAINER))
                .and_then(|m| g.text(m, FOAF_NAME))
        })
        .unwrap_or_default();
    let minor = g.number(p, &lv2("minorVersion")).unwrap_or(0.0);
    let micro = g.number(p, &lv2("microVersion")).unwrap_or(0.0);
    let classes: Vec<String> = g
        .objects(p, RDF_TYPE)
        .filter_map(Node::iri)
        .map(str::to_string)
        .collect();
    // Sidechain groups (`pg:sideChainOf`).
    let mut ports = Vec::new();
    for port in g.objects(p, &lv2("port")) {
        let types: Vec<&str> = g.objects(port, RDF_TYPE).filter_map(Node::iri).collect();
        let is = |t: &str| types.contains(&t);
        let kind = if is(&lv2("AudioPort")) {
            PortKind::Audio
        } else if is(&lv2("ControlPort")) {
            PortKind::Control
        } else if is(&format!("{}AtomPort", ttl::ATOM)) {
            let midi = g
                .objects(port, &format!("{}supports", ttl::ATOM))
                .any(|o| o.iri() == Some(MIDI_EVENT));
            PortKind::Atom { midi }
        } else if is(&lv2("CVPort")) {
            PortKind::Cv
        } else {
            PortKind::Other
        };
        let props: Vec<&str> = g
            .objects(port, &lv2("portProperty"))
            .filter_map(Node::iri)
            .collect();
        let prop = |x: &str| props.contains(&x);
        let designation = g.object(port, &lv2("designation")).and_then(Node::iri);
        let group_node = g.object(port, &format!("{PG}group"));
        let group_sidechain =
            group_node.is_some_and(|grp| g.object(grp, &format!("{PG}sideChainOf")).is_some());
        let group = group_node.map(|n| match n {
            Node::Iri(s) | Node::Blank(s) => s.clone(),
            Node::Literal { value, .. } => value.clone(),
        });
        let main_group = group_node.is_some_and(|grp| {
            g.objects(p, &format!("{PG}mainOutput"))
                .chain(g.objects(p, &format!("{PG}mainInput")))
                .any(|m| m == grp)
        });
        let supports_time = g
            .objects(port, &format!("{}supports", ttl::ATOM))
            .any(|o| o.iri() == Some("http://lv2plug.in/ns/ext/time#Position"));
        let mut scale_points: Vec<(f64, String)> = g
            .objects(port, &lv2("scalePoint"))
            .filter_map(|sp| {
                Some((
                    g.number(sp, "http://www.w3.org/1999/02/22-rdf-syntax-ns#value")?,
                    g.text(sp, RDFS_LABEL).unwrap_or_default(),
                ))
            })
            .collect();
        scale_points.sort_by(|a, b| a.0.total_cmp(&b.0));
        let unit = g
            .object(port, &format!("{UNITS}unit"))
            .and_then(Node::iri)
            .and_then(|u| u.strip_prefix(UNITS))
            .map(str::to_string);
        ports.push(Port {
            index: g
                .number(port, &lv2("index"))
                .ok_or("a port without lv2:index")? as u32,
            symbol: g.text(port, &lv2("symbol")).unwrap_or_default(),
            name: g
                .text(port, &lv2("name"))
                .or_else(|| g.text(port, &lv2("symbol")))
                .unwrap_or_default(),
            kind,
            input: is(&lv2("InputPort")),
            default: g.number(port, &lv2("default")),
            minimum: g.number(port, &lv2("minimum")),
            maximum: g.number(port, &lv2("maximum")),
            integer: prop(&lv2("integer")),
            toggled: prop(&lv2("toggled")),
            enumeration: prop(&lv2("enumeration")),
            logarithmic: prop(&format!("{PPROPS}logarithmic")),
            optional: prop(&lv2("connectionOptional")),
            hidden: prop(&format!("{PPROPS}notOnGUI")) || prop(&format!("{PPROPS}notAutomatic")),
            latency: prop(&lv2("reportsLatency")) || designation == Some(lv2("latency").as_str()),
            enabled: designation == Some(lv2("enabled").as_str()),
            free_wheeling: designation == Some(lv2("freeWheeling").as_str()),
            sidechain: prop(&lv2("isSideChain")) || group_sidechain,
            scale_points,
            unit,
            minimum_size: g
                .number(port, "http://lv2plug.in/ns/ext/resize-port#minimumSize")
                .map(|v| v as u32),
            time: supports_time,
            group,
            main_group,
        });
    }
    ports.sort_by_key(|p| p.index);
    for (i, port) in ports.iter().enumerate() {
        if port.index as usize != i {
            return Err(format!("port indices are not 0…{}", ports.len() - 1));
        }
    }
    let iris = |pred: &str| -> Vec<String> {
        g.objects(p, pred)
            .filter_map(Node::iri)
            .map(str::to_string)
            .collect()
    };
    let uis = g
        .objects(p, &format!("{UI}ui"))
        .filter_map(|u| {
            let uri = u.iri()?.to_string();
            let class = g
                .objects(u, RDF_TYPE)
                .filter_map(Node::iri)
                .find(|c| c.starts_with(UI))?
                .to_string();
            let binary = g
                .object(u, &format!("{UI}binary"))
                .and_then(Node::iri)
                .and_then(ttl::url_path)?;
            Some(Ui {
                uri,
                required_features: g
                    .objects(u, &lv2("requiredFeature"))
                    .filter_map(Node::iri)
                    .map(str::to_string)
                    .collect(),
                class,
                bundle: binary
                    .parent()
                    .map_or_else(|| bundle.to_path_buf(), Path::to_path_buf),
                binary,
            })
        })
        .collect();
    let mut presets: Vec<Preset> = presets_in(g, bundle)
        .into_iter()
        .filter(|(applies, _)| applies == uri)
        .map(|(_, p)| p)
        .collect();
    presets.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(Lv2Plugin {
        uri: uri.to_string(),
        name,
        author,
        version: format!("{minor}.{micro}"),
        bundle: bundle.to_path_buf(),
        binary,
        classes,
        ports,
        required_features: iris(&lv2("requiredFeature")),
        optional_features: iris(&lv2("optionalFeature")),
        extension_data: iris(&lv2("extensionData")),
        uis,
        presets,
    })
}

/// A preset's port values (by symbol) and state properties, read from its
/// files.
pub fn preset_values(
    bundle_graph_files: &[PathBuf],
    preset: &str,
) -> Result<Vec<(String, f64)>, String> {
    let mut g = Graph::new();
    for f in bundle_graph_files {
        g.load(f)?;
    }
    let p = Node::Iri(preset.to_string());
    // The preset's own files.
    let more: Vec<PathBuf> = g
        .objects(&p, RDFS_SEE_ALSO)
        .filter_map(|o| o.iri().and_then(ttl::url_path))
        .collect();
    for f in more {
        g.load(&f)?;
    }
    Ok(g.objects(&p, &lv2("port"))
        .filter_map(|port| {
            Some((
                g.text(port, &lv2("symbol"))?,
                g.number(port, &format!("{}value", ttl::PSET))?,
            ))
        })
        .collect())
}

/// Every plugin on `paths`.
pub fn scan(paths: &[PathBuf]) -> Vec<Lv2Plugin> {
    let mut cache = Cache::default();
    for e in cache.update(&find_bundles(paths)).1 {
        tracing::warn!("LV2 {e}");
    }
    cache.plugins()
}
