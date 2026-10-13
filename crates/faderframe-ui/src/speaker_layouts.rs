//! Audio → Speaker Layouts…: the project's own speaker layouts — their
//! channels in order (the order of the device's outputs and of files),
//! each a direction (azimuth: 0 ahead, positive to the left; elevation
//! above ear level) or the LFE. A layout is checked as it is edited (VBAP
//! must be able to pan over it); saved, it is a channel format like the
//! built-in ones, and tracks on a layout follow its new version.

use crate::state::AppState;
use faderframe_core::SurroundFormat;
use faderframe_core::surround::{LayoutDef, SpeakerDef};
use faderframe_session::Action;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// The layout being edited (`replace`: the saved one it is a version of).
struct Draft {
    replace: Option<String>,
    def: LayoutDef,
}

struct Ui {
    app: std::rc::Weak<AppState>,
    list: gtk::ListBox,
    name: gtk::Entry,
    rows: gtk::Grid,
    status: gtk::Label,
    save: gtk::Button,
    delete: gtk::Button,
    draft: RefCell<Draft>,
}

fn hexagon() -> LayoutDef {
    let spk = |label: &str, azimuth: f32| SpeakerDef {
        label: label.into(),
        azimuth,
        elevation: 0.0,
        lfe: false,
    };
    LayoutDef {
        name: "Hexagon".into(),
        speakers: vec![
            spk("F", 0.0),
            spk("FL", 60.0),
            spk("BL", 120.0),
            spk("B", 180.0),
            spk("BR", -120.0),
            spk("FR", -60.0),
        ],
    }
}

impl Ui {
    /// The list of saved layouts.
    fn fill_list(&self) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        let Some(app) = self.app.upgrade() else {
            return;
        };
        for d in app.session.borrow().speaker_layouts() {
            let label = gtk::Label::new(Some(&format!("{} ({})", d.name, d.speakers.len())));
            label.set_xalign(0.0);
            label.set_margin_start(6);
            label.set_margin_top(4);
            label.set_margin_bottom(4);
            self.list.append(&label);
        }
    }

    /// The editor's rows from the draft.
    fn fill_rows(self: &Rc<Self>) {
        while let Some(child) = self.rows.first_child() {
            self.rows.remove(&child);
        }
        let head = ["", "Label", "Azimuth °", "Elevation °", "LFE", ""];
        for (c, h) in head.iter().enumerate() {
            let l = gtk::Label::new(Some(h));
            l.add_css_class("dim-label");
            l.set_xalign(0.0);
            self.rows.attach(&l, c as i32, 0, 1, 1);
        }
        let draft = self.draft.borrow();
        self.name.set_text(&draft.def.name);
        for (i, s) in draft.def.speakers.iter().enumerate() {
            let y = i as i32 + 1;
            let n = gtk::Label::new(Some(&format!("{}", i + 1)));
            n.add_css_class("dim-label");
            let label = gtk::Entry::new();
            label.set_text(&s.label);
            label.set_width_chars(6);
            let az = gtk::SpinButton::with_range(-180.0, 180.0, 1.0);
            az.set_value(f64::from(s.azimuth));
            let el = gtk::SpinButton::with_range(-90.0, 90.0, 1.0);
            el.set_value(f64::from(s.elevation));
            let lfe = gtk::CheckButton::new();
            lfe.set_active(s.lfe);
            az.set_sensitive(!s.lfe);
            el.set_sensitive(!s.lfe);
            let remove = gtk::Button::from_icon_name("list-remove-symbolic");
            remove.set_tooltip_text(Some("Remove this speaker"));
            for (c, w) in [
                n.upcast_ref::<gtk::Widget>(),
                label.upcast_ref(),
                az.upcast_ref(),
                el.upcast_ref(),
                lfe.upcast_ref(),
                remove.upcast_ref(),
            ]
            .into_iter()
            .enumerate()
            {
                self.rows.attach(w, c as i32, y, 1, 1);
            }
            let ui = Rc::downgrade(self);
            label.connect_changed(move |e| {
                if let Some(ui) = ui.upgrade() {
                    ui.edit(i, |s| s.label = e.text().to_string());
                }
            });
            let ui = Rc::downgrade(self);
            az.connect_value_changed(move |b| {
                if let Some(ui) = ui.upgrade() {
                    ui.edit(i, |s| s.azimuth = b.value() as f32);
                }
            });
            let ui = Rc::downgrade(self);
            el.connect_value_changed(move |b| {
                if let Some(ui) = ui.upgrade() {
                    ui.edit(i, |s| s.elevation = b.value() as f32);
                }
            });
            let ui = Rc::downgrade(self);
            let (az2, el2) = (az.clone(), el.clone());
            lfe.connect_toggled(move |b| {
                if let Some(ui) = ui.upgrade() {
                    az2.set_sensitive(!b.is_active());
                    el2.set_sensitive(!b.is_active());
                    ui.edit(i, |s| s.lfe = b.is_active());
                }
            });
            let ui = Rc::downgrade(self);
            remove.connect_clicked(move |_| {
                if let Some(ui) = ui.upgrade() {
                    ui.draft.borrow_mut().def.speakers.remove(i);
                    ui.fill_rows();
                }
            });
        }
        drop(draft);
        self.check();
    }

    fn edit(&self, i: usize, f: impl FnOnce(&mut SpeakerDef)) {
        if let Some(s) = self.draft.borrow_mut().def.speakers.get_mut(i) {
            f(s);
        }
        self.check();
    }

    /// Say what is wrong, if anything; Save only for a usable layout.
    fn check(&self) {
        let draft = self.draft.borrow();
        let taken = self.app.upgrade().is_some_and(|app| {
            app.session.borrow().speaker_layouts().iter().any(|d| {
                d.name.eq_ignore_ascii_case(draft.def.name.trim())
                    && Some(d.name.as_str()) != draft.replace.as_deref()
            })
        });
        let verdict = if taken {
            Err(format!(
                "There is a layout called {} already.",
                draft.def.name.trim()
            ))
        } else {
            draft.def.validate().map_err(|e| e.to_string())
        };
        match verdict {
            Ok(()) => {
                let lfe = draft.def.speakers.iter().any(|s| s.lfe);
                self.status.set_text(&format!(
                    "{} channels{} — panned by VBAP ({}).",
                    draft.def.speakers.len(),
                    if lfe { " with an LFE" } else { "" },
                    if draft
                        .def
                        .speakers
                        .iter()
                        .any(|s| !s.lfe && s.elevation != 0.0)
                    {
                        "in three dimensions"
                    } else {
                        "around the listener"
                    }
                ));
                self.save.set_sensitive(true);
            }
            Err(e) => {
                self.status.set_text(&e);
                self.save.set_sensitive(false);
            }
        }
        self.delete.set_sensitive(draft.replace.is_some());
    }

    fn start(self: &Rc<Self>, replace: Option<String>, def: LayoutDef) {
        *self.draft.borrow_mut() = Draft { replace, def };
        self.fill_rows();
    }
}

/// Open the Speaker Layouts window.
pub fn open(app: &Rc<AppState>) {
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let win = gtk::Window::builder()
        .title("Speaker Layouts")
        .application(&app.app)
        .transient_for(&main)
        .default_width(780)
        .default_height(680)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    // The saved layouts, and new ones.
    let side = gtk::Box::new(gtk::Orientation::Vertical, 8);
    side.set_width_request(200);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    side.append(&scroll);
    let new = gtk::Button::with_label("New Layout");
    side.append(&new);
    let mut from_names = vec!["New from…".to_string()];
    from_names.extend(SurroundFormat::ALL.iter().map(|f| f.name().to_string()));
    let refs: Vec<&str> = from_names.iter().map(String::as_str).collect();
    let from = gtk::DropDown::from_strings(&refs);
    from.set_tooltip_text(Some("Start from a built-in format's speakers"));
    side.append(&from);
    body.append(&side);
    // The editor.
    let editor = gtk::Box::new(gtk::Orientation::Vertical, 10);
    editor.set_hexpand(true);
    let intro = gtk::Label::new(Some(
        "Channels in order — the order of the device's outputs and of files. Azimuth: 0° ahead, positive to the left; elevation above ear level. Panned by VBAP; folded to stereo and mono by the project's downmix; heard on headphones through the nearest measured directions.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.add_css_class("dim-label");
    editor.append(&intro);
    let name_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    name_row.append(&gtk::Label::new(Some("Name")));
    let name = gtk::Entry::new();
    name.set_hexpand(true);
    name_row.append(&name);
    editor.append(&name_row);
    let rows = gtk::Grid::new();
    rows.set_row_spacing(4);
    rows.set_column_spacing(8);
    let rows_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&rows)
        .build();
    editor.append(&rows_scroll);
    let add = gtk::Button::with_label("Add Speaker");
    add.set_halign(gtk::Align::Start);
    editor.append(&add);
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    editor.append(&status);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let delete = gtk::Button::with_label("Delete");
    delete.add_css_class("destructive-action");
    let save = gtk::Button::with_label("Save Layout");
    save.add_css_class("suggested-action");
    buttons.append(&delete);
    buttons.append(&save);
    editor.append(&buttons);
    body.append(&editor);
    win.set_child(Some(&body));

    let ui = Rc::new(Ui {
        app: Rc::downgrade(app),
        list,
        name,
        rows,
        status,
        save,
        delete,
        draft: RefCell::new(Draft {
            replace: None,
            def: hexagon(),
        }),
    });
    ui.fill_list();
    // The first saved layout, if there is one (else a new hexagon).
    let first = app.session.borrow().speaker_layouts().first().cloned();
    match first {
        Some(def) => ui.start(Some(def.name.clone()), def),
        None => ui.fill_rows(),
    }
    {
        let w = Rc::downgrade(&ui);
        ui.name.connect_changed(move |e| {
            if let Some(ui) = w.upgrade()
                && let Ok(mut d) = ui.draft.try_borrow_mut()
            {
                d.def.name = e.text().to_string();
                drop(d);
                ui.check();
            }
        });
    }
    {
        let w = Rc::downgrade(&ui);
        ui.list.connect_row_activated(move |_, row| {
            let Some(ui) = w.upgrade() else { return };
            let Some(app) = ui.app.upgrade() else { return };
            let def = app
                .session
                .borrow()
                .speaker_layouts()
                .get(row.index() as usize)
                .cloned();
            if let Some(def) = def {
                ui.start(Some(def.name.clone()), def);
            }
        });
    }
    {
        let w = Rc::downgrade(&ui);
        new.connect_clicked(move |_| {
            if let Some(ui) = w.upgrade() {
                ui.start(None, hexagon());
            }
        });
    }
    {
        let w = Rc::downgrade(&ui);
        from.connect_selected_notify(move |d| {
            let Some(ui) = w.upgrade() else { return };
            let Some(f) = (d.selected() as usize)
                .checked_sub(1)
                .and_then(|i| SurroundFormat::ALL.get(i))
            else {
                return;
            };
            ui.start(
                None,
                LayoutDef::from_format(*f, &format!("My {}", f.name())),
            );
            d.set_selected(0);
        });
    }
    {
        let w = Rc::downgrade(&ui);
        add.connect_clicked(move |_| {
            if let Some(ui) = w.upgrade() {
                let n = ui.draft.borrow().def.speakers.len() + 1;
                ui.draft.borrow_mut().def.speakers.push(SpeakerDef {
                    label: format!("S{n}"),
                    azimuth: 0.0,
                    elevation: 0.0,
                    lfe: false,
                });
                ui.fill_rows();
            }
        });
    }
    {
        let w = Rc::downgrade(&ui);
        ui.save.connect_clicked(move |_| {
            let Some(ui) = w.upgrade() else { return };
            let Some(app) = ui.app.upgrade() else { return };
            let (replace, layout) = {
                let d = ui.draft.borrow();
                (d.replace.clone(), d.def.clone())
            };
            let name = layout.name.trim().to_string();
            app.dispatch(Action::SaveSpeakerLayout { replace, layout });
            ui.draft.borrow_mut().replace = Some(name);
            ui.fill_list();
            ui.check();
        });
    }
    {
        let w = Rc::downgrade(&ui);
        ui.delete.connect_clicked(move |_| {
            let Some(ui) = w.upgrade() else { return };
            let Some(app) = ui.app.upgrade() else { return };
            let name = ui.draft.borrow().replace.clone();
            if let Some(name) = name {
                app.dispatch(Action::RemoveSpeakerLayout(name.clone()));
                if !app
                    .session
                    .borrow()
                    .speaker_layouts()
                    .iter()
                    .any(|d| d.name == name)
                {
                    ui.start(None, hexagon());
                }
                ui.fill_list();
            }
        });
    }
    win.present();
}
