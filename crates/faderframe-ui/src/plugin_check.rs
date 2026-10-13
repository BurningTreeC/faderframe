//! The plugin check in the shell: checks run one after another in
//! `faderframe --check-plugin` helper processes on a background thread
//! (`faderframe_plugin_check`: a plugin is played for a few seconds while
//! its allocations, waits, page faults and time per block are counted).
//! Reports are kept per plugin version in the cache folder
//! (`plugin-checks.json`) and shown by the plugin browser.

use crate::state::AppState;
use faderframe_plugin_check::CheckReport;
use faderframe_project::{PluginFormat, PluginRef};
use gtk::glib;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

const CACHE: &str = "plugin-checks.json";
/// A plugin taking longer than this in its check is stopped (and failed).
const TIMEOUT: Duration = Duration::from_secs(90);

/// The format as `--check-plugin` names it.
fn format_id(f: PluginFormat) -> &'static str {
    match f {
        PluginFormat::Builtin => "builtin",
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
        PluginFormat::AudioUnit => "audio_unit",
        PluginFormat::Lv2 => "lv2",
    }
}

fn key(f: PluginFormat, id: &str) -> String {
    format!("{}:{id}", format_id(f))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Cached {
    version: String,
    report: CheckReport,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Cache {
    reports: BTreeMap<String, Cached>,
}

fn cache_path() -> PathBuf {
    crate::paths::cache_dir().join(CACHE)
}

impl Cache {
    fn load() -> Self {
        std::fs::read_to_string(cache_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let path = cache_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    tracing::warn!("cannot save the plugin checks: {e}");
                }
            }
            Err(e) => tracing::warn!("cannot save the plugin checks: {e}"),
        }
    }
}

/// What the background thread reports.
enum Event {
    Started(String),
    Done {
        key: String,
        version: String,
        report: Box<CheckReport>,
    },
    Finished,
}

struct Running {
    rx: mpsc::Receiver<Event>,
    total: usize,
    done: usize,
    current: String,
}

#[derive(Default)]
struct State {
    cache: Option<Cache>,
    running: Option<Running>,
    /// Bumped with every new report (views refresh).
    generation: u64,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with_cache<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    STATE.with(|s| f(s.borrow_mut().cache.get_or_insert_with(Cache::load)))
}

/// The report for this plugin version, if it was checked.
pub fn report(plugin: &PluginRef, version: &str) -> Option<CheckReport> {
    with_cache(|c| {
        c.reports
            .get(&key(plugin.format, &plugin.id))
            .filter(|r| r.version == version)
            .map(|r| r.report.clone())
    })
}

/// Changes with every new report.
pub fn generation() -> u64 {
    STATE.with(|s| s.borrow().generation)
}

/// Checks done and to do, and the one under way (while checks run).
pub fn progress() -> Option<(usize, usize, String)> {
    STATE.with(|s| {
        s.borrow()
            .running
            .as_ref()
            .map(|r| (r.done, r.total, r.current.clone()))
    })
}

/// Take what the background thread found (on the UI thread).
fn poll() -> bool {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let mut finished = false;
        let mut new = Vec::new();
        if let Some(r) = s.running.as_mut() {
            while let Ok(e) = r.rx.try_recv() {
                match e {
                    Event::Started(name) => r.current = name,
                    Event::Done {
                        key,
                        version,
                        report,
                    } => {
                        r.done += 1;
                        new.push((
                            key,
                            Cached {
                                version,
                                report: *report,
                            },
                        ));
                    }
                    Event::Finished => finished = true,
                }
            }
        }
        let changed = !new.is_empty() || finished;
        if !new.is_empty() {
            let cache = s.cache.get_or_insert_with(Cache::load);
            cache.reports.extend(new);
            cache.save();
        }
        if finished {
            s.running = None;
        }
        if changed {
            s.generation += 1;
        }
        !finished
    })
}

/// Check `plugins` (with their versions) in the background, one after
/// another; nothing when checks are already running. The device's rate and
/// block size if it runs (else 48 kHz, 128 frames).
pub fn check(app: &Rc<AppState>, plugins: Vec<(PluginRef, String)>) -> bool {
    if plugins.is_empty() || progress().is_some() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let (rate, block) = {
        let s = app.session.borrow();
        let e = s.engine();
        let rate = match e.stream_sample_rate() {
            0 => 48_000,
            r => r,
        };
        let block = match e.stream_buffer_size() {
            0 => 128,
            b => b,
        };
        (rate, block)
    };
    let (tx, rx) = mpsc::channel();
    let total = plugins.len();
    let spawned = std::thread::Builder::new()
        .name("faderframe-plugin-check".into())
        .spawn(move || {
            for (plugin, version) in plugins {
                let _ = tx.send(Event::Started(plugin.name.clone()));
                tracing::info!("checking {}", plugin.name);
                let args = vec![
                    "--check-plugin".into(),
                    format_id(plugin.format).into(),
                    plugin.id.clone(),
                    rate.to_string(),
                    block.to_string(),
                ];
                let report = faderframe_plugin_check::in_subprocess(&exe, &args, TIMEOUT);
                let done = Event::Done {
                    key: key(plugin.format, &plugin.id),
                    version,
                    report: Box::new(report),
                };
                if tx.send(done).is_err() {
                    return;
                }
            }
            let _ = tx.send(Event::Finished);
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start the plugin check: {e}");
        return false;
    }
    STATE.with(|s| {
        s.borrow_mut().running = Some(Running {
            rx,
            total,
            done: 0,
            current: String::new(),
        });
        s.borrow_mut().generation += 1;
    });
    // Reports are taken (and kept) whether or not a window shows them.
    glib::timeout_add_local(Duration::from_millis(250), || {
        if poll() {
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
    true
}
