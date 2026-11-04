//! A device's A/B compare and its own undo in the header every editor
//! window shares (`plugin_window::editor_header`), and the development aids
//! for them and for slicing (`faderframe_session::{device_states,
//! slicing}`):
//!
//! * `device-ab:<switch|a|b|copy|match=on|off|undo|redo|watch|status>[@<track>/<n>]`
//!   — on insert n (from 1) of the track, else the device editor opened
//!   last; `watch` keeps its history as an open window would, `status`
//!   logs what its header shows;
//! * `slice-to-pads:<track>[@transients|grid=<quarters>|parts=<n>|clips]`
//!   — the track's selection (or its first clip) sliced onto a new Drum
//!   Sampler with a MIDI clip (transients by default);
//! * `slice-to-sampler:<track>[@…]` — likewise onto a new Sampler, a key
//!   a slice;
//! * `slice-device:<transients|parts=<n>|clear|to-midi>[@pad <n>]` — the
//!   newest device editor's sampler: its sample sliced in place (a pad's
//!   onto the free pads), its slices cleared or played by a MIDI clip.

use crate::state::AppState;
use faderframe_core::PluginInstanceId;
use faderframe_project::device_compare::Side;
use faderframe_session::Action;
use faderframe_session::device_states::{DeviceOp, DeviceStatus};
use faderframe_session::slicing::{SliceBy, SliceSource, SliceTarget};
use faderframe_timeline::MusicalTime;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

pub(crate) fn install(app: &Rc<AppState>) {
    let named = |name: &'static str, f: fn(&Rc<AppState>, &str)| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .parameter_type(Some(&String::static_variant_type()))
            .activate(move |_, _, param| {
                if let (Some(a), Some(arg)) =
                    (weak.upgrade(), param.and_then(|p| p.get::<String>()))
                {
                    f(&a, &arg);
                }
            })
            .build()
    };
    app.app.add_action_entries([
        named("device-ab", device_ab),
        named("slice-to-pads", |a, arg| {
            slice_track(a, arg, SliceTarget::NewDrums)
        }),
        named("slice-to-sampler", |a, arg| {
            slice_track(a, arg, SliceTarget::NewSampler)
        }),
        named("slice-device", slice_device),
    ]);
}

/// The device a development aid names: `<track>/<n>` (insert n from 1),
/// else the device editor opened last.
fn device_named(a: &Rc<AppState>, at: Option<&str>) -> Option<PluginInstanceId> {
    let Some(at) = at else {
        return crate::plugin_window::latest_device();
    };
    let (name, n) = at.rsplit_once('/')?;
    let n: usize = n.trim().parse().ok()?;
    let s = a.session.borrow();
    let t = s.project().tracks.iter().find(|t| t.name == name.trim())?;
    t.inserts.get(n.checked_sub(1)?).map(|slot| slot.id)
}

fn device_ab(a: &Rc<AppState>, arg: &str) {
    let (what, at) = match arg.split_once('@') {
        Some((w, at)) => (w.trim(), Some(at)),
        None => (arg.trim(), None),
    };
    let Some(plugin) = device_named(a, at) else {
        return tracing::warn!("device-ab: no device '{}'", at.unwrap_or("(open editor)"));
    };
    let op = match what {
        "switch" => DeviceOp::Switch(plugin),
        "a" => DeviceOp::Hear {
            plugin,
            side: Side::A,
        },
        "b" => DeviceOp::Hear {
            plugin,
            side: Side::B,
        },
        "copy" => DeviceOp::CopyToOther(plugin),
        "match=on" => DeviceOp::MatchLevels { plugin, on: true },
        "match=off" => DeviceOp::MatchLevels { plugin, on: false },
        "undo" => DeviceOp::Undo(plugin),
        "redo" => DeviceOp::Redo(plugin),
        "watch" => DeviceOp::Watch { plugin, on: true },
        "status" => {
            let status = a.session.borrow().device_status(plugin);
            return tracing::info!("device-ab: {status:?}");
        }
        _ => {
            return tracing::warn!(
                "device-ab: '{what}' is not switch|a|b|copy|match=on|match=off|undo|redo|watch|status"
            );
        }
    };
    a.dispatch(Action::Device(op));
}

/// `transients`, `grid=<quarters>`, `parts=<n>`, `clips`.
fn slice_by(what: &str) -> Option<SliceBy> {
    let (kind, value) = what.split_once('=').unwrap_or((what, ""));
    Some(match kind.trim() {
        "" | "transients" => SliceBy::Transients,
        "grid" => SliceBy::Grid(MusicalTime::from_quarters(
            value.trim().parse::<f64>().ok().filter(|q| *q > 0.0)?,
        )),
        "parts" => SliceBy::Parts(value.trim().parse().ok()?),
        "clips" => SliceBy::Clips,
        _ => return None,
    })
}

fn slice_track(a: &Rc<AppState>, arg: &str, target: SliceTarget) {
    let (name, how) = arg.split_once('@').unwrap_or((arg, ""));
    let Some(by) = slice_by(how) else {
        return tracing::warn!("slice: '{how}' is not transients|grid=<q>|parts=<n>|clips");
    };
    let source = {
        let s = a.session.borrow();
        let track = s.project().tracks.iter().find(|t| t.name == name.trim());
        track.and_then(|t| {
            let clip = s.project().clips_of(t.id).first().map(|c| c.id);
            s.slice_choices(t.id, clip)
                .into_iter()
                .find_map(|(_, action, _)| match action {
                    Action::Slice { source, .. } => Some(source),
                    _ => None,
                })
        })
    };
    match source {
        Some(source) => a.dispatch(Action::Slice {
            source,
            by,
            target,
            midi: true,
        }),
        None => tracing::warn!("slice: nothing to slice on '{name}'"),
    }
}

fn slice_device(a: &Rc<AppState>, arg: &str) {
    let (what, pad) = match arg.split_once("@pad") {
        Some((w, n)) => (w.trim(), n.trim().parse::<usize>().ok()),
        None => (arg.trim(), None),
    };
    let Some(plugin) = crate::plugin_window::latest_device() else {
        return tracing::warn!("slice-device: no device editor is open");
    };
    let slot = pad.map_or(0, |p| p.saturating_sub(1));
    let is_drums = a
        .session
        .borrow()
        .plugin_owner(plugin)
        .is_some_and(|(_, s)| s.plugin.id == faderframe_core::builtin::DRUMS);
    let action = match what {
        "clear" => Action::SetSampleSlices {
            plugin,
            slices: Vec::new(),
        },
        "to-midi" => Action::SlicesToMidi(plugin),
        how => {
            let Some(by) = slice_by(how) else {
                return tracing::warn!(
                    "slice-device: '{how}' is not transients|parts=<n>|clear|to-midi"
                );
            };
            Action::Slice {
                source: SliceSource::Sample { plugin, slot },
                by,
                target: if is_drums {
                    SliceTarget::Pads(plugin)
                } else {
                    SliceTarget::InPlace
                },
                midi: is_drums,
            }
        }
    };
    a.dispatch(action);
}

// --- the header ------------------------------------------------------------------

/// The header's A/B compare and the device's own undo: its Undo and Redo,
/// A and B (the side heard), Copy (the side heard to the other) and Match
/// (side B heard at side A's level). They follow the session — another
/// window, the project's undo — every 200 ms. `None` for a device they do
/// not work on (a clip effect's own window).
pub(crate) fn header_controls(app: &Rc<AppState>, plugin: PluginInstanceId) -> Option<gtk::Box> {
    app.session.borrow().device_status(plugin)?;
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let history = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    history.add_css_class("linked");
    let undo = gtk::Button::from_icon_name("edit-undo-symbolic");
    let redo = gtk::Button::from_icon_name("edit-redo-symbolic");
    history.append(&undo);
    history.append(&redo);
    let sides = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    sides.add_css_class("linked");
    let a = gtk::ToggleButton::with_label("A");
    let b = gtk::ToggleButton::with_label("B");
    b.set_group(Some(&a));
    a.set_tooltip_text(Some("Hear setting A"));
    b.set_tooltip_text(Some("Hear setting B (the first time: a copy of A)"));
    sides.append(&a);
    sides.append(&b);
    let copy = gtk::Button::with_label("A → B");
    let matching = gtk::ToggleButton::with_label("Match");
    matching.set_tooltip_text(Some(
        "Hear B at A's level while comparing (measured as it plays; listening only)",
    ));
    row.append(&history);
    row.append(&sides);
    row.append(&copy);
    row.append(&matching);

    let quiet = Rc::new(Cell::new(false));
    let send = |app: &Rc<AppState>, op: DeviceOp| {
        let weak = Rc::downgrade(app);
        move || {
            if let Some(app) = weak.upgrade() {
                app.dispatch(Action::Device(op.clone()));
            }
        }
    };
    let f = send(app, DeviceOp::Undo(plugin));
    undo.connect_clicked(move |_| f());
    let f = send(app, DeviceOp::Redo(plugin));
    redo.connect_clicked(move |_| f());
    let f = send(app, DeviceOp::CopyToOther(plugin));
    copy.connect_clicked(move |_| f());
    for (button, side) in [(&a, Side::A), (&b, Side::B)] {
        let f = send(app, DeviceOp::Hear { plugin, side });
        let quiet = Rc::clone(&quiet);
        button.connect_toggled(move |t| {
            if t.is_active() && !quiet.get() {
                f();
            }
        });
    }
    let weak = Rc::downgrade(app);
    let q = Rc::clone(&quiet);
    matching.connect_toggled(move |t| {
        if q.get() {
            return;
        }
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::Device(DeviceOp::MatchLevels {
                plugin,
                on: t.is_active(),
            }));
        }
    });

    // Its history is kept while the window shows it.
    let weak = Rc::downgrade(app);
    glib::idle_add_local_once(move || {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::Device(DeviceOp::Watch { plugin, on: true }));
        }
    });
    let weak = Rc::downgrade(app);
    row.connect_destroy(move |_| {
        let weak = weak.clone();
        glib::idle_add_local_once(move || {
            if let Some(app) = weak.upgrade() {
                app.dispatch(Action::Device(DeviceOp::Watch { plugin, on: false }));
            }
        });
    });

    let show = {
        let (undo, redo, a, b, copy, matching) = (
            undo.downgrade(),
            redo.downgrade(),
            a.downgrade(),
            b.downgrade(),
            copy.downgrade(),
            matching.downgrade(),
        );
        move |status: &DeviceStatus, quiet: &Cell<bool>| -> bool {
            let (Some(undo), Some(redo), Some(a), Some(b), Some(copy), Some(matching)) = (
                undo.upgrade(),
                redo.upgrade(),
                a.upgrade(),
                b.upgrade(),
                copy.upgrade(),
                matching.upgrade(),
            ) else {
                return false;
            };
            undo.set_sensitive(status.undo.is_some());
            undo.set_tooltip_text(Some(&match &status.undo {
                Some(l) => format!("Undo {l} in this device"),
                None => "Nothing to undo in this device".into(),
            }));
            redo.set_sensitive(status.redo.is_some());
            redo.set_tooltip_text(Some(&match &status.redo {
                Some(l) => format!("Redo {l} in this device"),
                None => "Nothing to redo in this device".into(),
            }));
            quiet.set(true);
            a.set_active(status.heard == Side::A);
            b.set_active(status.heard == Side::B);
            matching.set_active(status.matching);
            quiet.set(false);
            let (from, to) = (status.heard.label(), status.heard.other().label());
            copy.set_label(&format!("{from} → {to}"));
            copy.set_sensitive(!status.compared || status.differ);
            copy.set_tooltip_text(Some(&format!("Copy setting {from} to {to}")));
            matching.set_tooltip_text(Some(&match status.levels {
                Some(l) => match (l.a_db, l.b_db) {
                    (Some(a), Some(b)) => format!(
                        "A {a:.1} dB, B {b:.1} dB RMS: B heard at {:+.1} dB (listening only)",
                        l.gain_db
                    ),
                    _ => "Measuring each side as it plays…".into(),
                },
                None => {
                    "Hear B at A's level while comparing (measured as it plays; listening only)"
                        .into()
                }
            }));
            true
        }
    };
    if let Some(status) = app.session.borrow().device_status(plugin) {
        show(&status, &quiet);
    }
    let weak = Rc::downgrade(app);
    let alive = row.downgrade();
    glib::timeout_add_local(Duration::from_millis(200), move || {
        let (Some(app), Some(_)) = (weak.upgrade(), alive.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        let status = app
            .session
            .try_borrow()
            .ok()
            .and_then(|s| s.device_status(plugin));
        match status {
            // (The widgets gone: the window closed.)
            Some(status) if !show(&status, &quiet) => glib::ControlFlow::Break,
            _ => glib::ControlFlow::Continue,
        }
    });
    Some(row)
}
