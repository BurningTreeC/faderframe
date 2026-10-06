//! Preferences → MIDI → Control Surfaces: Mackie Control, its extender,
//! HUI (their MIDI ports) and OSC (UDP ports). Changes apply at once and
//! are saved.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_session::control::{SurfaceKind, SurfaceSettings};
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// The section and what it keeps.
struct Section {
    list: gtk::ListBox,
    /// The surfaces' status labels, refreshed while the page is open.
    status: RefCell<Vec<gtk::Label>>,
}

fn apply(app: &AppState, settings: Vec<SurfaceSettings>) {
    app.session
        .borrow_mut()
        .set_control_surfaces(settings.clone());
    let mut p = Preferences::load();
    p.control_surfaces = settings;
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
}

/// A port chooser: "None", then the ports (key, name); `current` selected
/// (shown as absent when it is not there).
fn port_menu(ports: &[(String, String)], current: &str) -> (gtk::DropDown, Vec<String>) {
    let mut keys = vec![String::new()];
    let mut names = vec!["None".to_string()];
    for (k, n) in ports {
        keys.push(k.clone());
        names.push(n.clone());
    }
    if !current.is_empty() && !keys.iter().any(|k| k == current) {
        keys.push(current.to_string());
        names.push(format!("{current} (absent)"));
    }
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let d = gtk::DropDown::from_strings(&refs);
    d.set_selected(keys.iter().position(|k| k == current).unwrap_or(0) as u32);
    (d, keys)
}

fn spin(value: u16, min: f64, max: f64, tip: &str) -> gtk::SpinButton {
    let s = gtk::SpinButton::with_range(min, max, 1.0);
    s.set_value(f64::from(value));
    s.set_tooltip_text(Some(tip));
    s
}

fn rebuild(section: &Rc<Section>, app: &Rc<AppState>) {
    while let Some(c) = section.list.first_child() {
        section.list.remove(&c);
    }
    section.status.borrow_mut().clear();
    let (settings, inputs, outputs) = {
        let s = app.session.borrow();
        let inputs: Vec<(String, String)> = s
            .midi_ports()
            .into_iter()
            .filter(|p| !p.is_virtual)
            .map(|p| (p.key, p.name))
            .collect();
        let outputs: Vec<(String, String)> = s
            .midi_outputs()
            .into_iter()
            .filter(|p| !p.is_virtual)
            .map(|p| (p.key, p.name))
            .collect();
        (s.control_surfaces().to_vec(), inputs, outputs)
    };
    if settings.is_empty() {
        let empty = gtk::Label::new(Some("No control surfaces"));
        empty.add_css_class("dim-label");
        empty.set_margin_top(8);
        empty.set_margin_bottom(8);
        section.list.append(&empty);
    }
    // A change to surface `i` (applied, then the section rebuilt when its
    // fields change shape).
    let change = {
        let weak_app = Rc::downgrade(app);
        let weak = Rc::downgrade(section);
        move |i: usize, f: &dyn Fn(&mut SurfaceSettings), reshape: bool| {
            let (Some(app), Some(section)) = (weak_app.upgrade(), weak.upgrade()) else {
                return;
            };
            let mut all = app.session.borrow().control_surfaces().to_vec();
            if let Some(s) = all.get_mut(i) {
                f(s);
            }
            apply(&app, all);
            if reshape {
                // Not from inside the handler of a widget it removes.
                glib::idle_add_local_once(move || rebuild(&section, &app));
            }
        }
    };
    let change = Rc::new(change);
    for (i, s) in settings.iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.set_margin_start(8);
        row.set_margin_end(8);
        let names: Vec<&str> = SurfaceKind::ALL.iter().map(|k| k.label()).collect();
        let kind = gtk::DropDown::from_strings(&names);
        kind.set_selected(
            SurfaceKind::ALL
                .iter()
                .position(|k| *k == s.kind)
                .unwrap_or(0) as u32,
        );
        {
            let change = Rc::clone(&change);
            kind.connect_selected_notify(move |d| {
                let k = SurfaceKind::ALL[d.selected() as usize % SurfaceKind::ALL.len()];
                change(i, &|s| s.kind = k, true);
            });
        }
        row.append(&kind);
        if s.kind.is_midi() {
            let label = |t: &str| {
                let l = gtk::Label::new(Some(t));
                l.add_css_class("dim-label");
                l
            };
            let (input, in_keys) = port_menu(&inputs, &s.input);
            let (output, out_keys) = port_menu(&outputs, &s.output);
            {
                let change = Rc::clone(&change);
                input.connect_selected_notify(move |d| {
                    let key = in_keys
                        .get(d.selected() as usize)
                        .cloned()
                        .unwrap_or_default();
                    change(i, &|s| s.input = key.clone(), false);
                });
            }
            {
                let change = Rc::clone(&change);
                output.connect_selected_notify(move |d| {
                    let key = out_keys
                        .get(d.selected() as usize)
                        .cloned()
                        .unwrap_or_default();
                    change(i, &|s| s.output = key.clone(), false);
                });
            }
            row.append(&label("From"));
            row.append(&input);
            row.append(&label("To"));
            row.append(&output);
        } else {
            let listen = spin(
                s.listen,
                1024.0,
                65535.0,
                "FaderFrame listens on this UDP port",
            );
            let reply = spin(
                s.reply,
                1024.0,
                65535.0,
                "Answers go to the sender on this port",
            );
            let strips = spin(
                u16::from(s.strips),
                1.0,
                64.0,
                "Channel strips the surface shows",
            );
            for (w, what) in [(&listen, 0), (&reply, 1), (&strips, 2)] {
                let change = Rc::clone(&change);
                w.connect_value_changed(move |w| {
                    let v = w.value() as u16;
                    change(
                        i,
                        &|s| match what {
                            0 => s.listen = v,
                            1 => s.reply = v,
                            _ => s.strips = v.min(64) as u8,
                        },
                        false,
                    );
                });
            }
            for (t, w) in [("Listen", &listen), ("Reply", &reply), ("Strips", &strips)] {
                let l = gtk::Label::new(Some(t));
                l.add_css_class("dim-label");
                row.append(&l);
                row.append(w);
            }
        }
        let status = gtk::Label::new(None);
        status.set_hexpand(true);
        status.set_xalign(0.0);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status.add_css_class("dim-label");
        row.append(&status);
        section.status.borrow_mut().push(status);
        let remove = gtk::Button::from_icon_name("list-remove-symbolic");
        remove.set_tooltip_text(Some("Remove this surface"));
        {
            let weak_app = Rc::downgrade(app);
            let weak = Rc::downgrade(section);
            remove.connect_clicked(move |_| {
                let (Some(app), Some(section)) = (weak_app.upgrade(), weak.upgrade()) else {
                    return;
                };
                let mut all = app.session.borrow().control_surfaces().to_vec();
                if i < all.len() {
                    all.remove(i);
                }
                apply(&app, all);
                glib::idle_add_local_once(move || rebuild(&section, &app));
            });
        }
        row.append(&remove);
        section.list.append(&row);
    }
    update_status(section, app);
}

fn update_status(section: &Section, app: &AppState) {
    let s = app.session.borrow();
    let errors = s.control_surface_errors();
    let settings = s.control_surfaces();
    for (i, label) in section.status.borrow().iter().enumerate() {
        let (text, tip) = match (errors.get(i), settings.get(i)) {
            (Some(Some(e)), _) => ("Not running".to_string(), e.clone()),
            (Some(None), Some(c))
                if c.kind.is_midi() && c.input.is_empty() && c.output.is_empty() =>
            {
                (
                    "Choose its ports".to_string(),
                    "The surface's MIDI input and output".to_string(),
                )
            }
            (Some(None), Some(c)) => (
                if c.kind.is_midi() {
                    "Running".to_string()
                } else {
                    "Listening".to_string()
                },
                format!("Its strips start at track {}", s.surface_bank()),
            ),
            _ => (String::new(), String::new()),
        };
        if label.text() != text {
            label.set_text(&text);
        }
        if label.tooltip_text().as_deref() != Some(tip.as_str()) {
            label.set_tooltip_text(Some(&tip));
        }
    }
}

/// The section for the MIDI page.
pub fn section(app: &Rc<AppState>) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let section = Rc::new(Section {
        list: gtk::ListBox::new(),
        status: RefCell::new(Vec::new()),
    });
    section.list.set_selection_mode(gtk::SelectionMode::None);
    section.list.add_css_class("boxed-list");
    root.append(&section.list);
    let add = gtk::Button::with_label("Add Control Surface");
    add.set_halign(gtk::Align::Start);
    {
        let weak_app = Rc::downgrade(app);
        let weak = Rc::downgrade(&section);
        add.connect_clicked(move |_| {
            let (Some(app), Some(section)) = (weak_app.upgrade(), weak.upgrade()) else {
                return;
            };
            let mut all = app.session.borrow().control_surfaces().to_vec();
            all.push(SurfaceSettings::default());
            apply(&app, all);
            rebuild(&section, &app);
        });
    }
    root.append(&add);
    let hint = gtk::Label::new(Some(
        "Mackie Control (an extender shows the next strips) and HUI: choose the surface's MIDI ports — they then serve only the surface. OSC: FaderFrame listens on one UDP port and answers on the reply port (e.g. TouchOSC sending to 8000, listening on 9000).",
    ));
    hint.set_wrap(true);
    hint.set_natural_wrap_mode(gtk::NaturalWrapMode::Word);
    hint.set_xalign(0.0);
    hint.add_css_class("dim-label");
    root.append(&hint);
    rebuild(&section, app);
    // Status while the page lives.
    let weak_app = Rc::downgrade(app);
    let weak = Rc::downgrade(&section);
    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        match (weak_app.upgrade(), weak.upgrade()) {
            (Some(app), Some(section)) => {
                update_status(&section, &app);
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        }
    });
    keep_with(&root, section);
    root
}

/// Keep `section` alive as long as `widget` (dropped with it).
fn keep_with(widget: &gtk::Box, section: Rc<Section>) {
    let holder = RefCell::new(Some(section));
    widget.connect_destroy(move |_| {
        holder.borrow_mut().take();
    });
}
