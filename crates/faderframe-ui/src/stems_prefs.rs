//! The Stems page of the Preferences window: which separation model splits
//! clips into stems (its licence, who made it, what it learnt from), its
//! download, and how far the Wiener filter refines the stems. Changes
//! apply at once and are remembered.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_session::Action;
use faderframe_session::separate::{MAX_ITERATIONS, ModelInfo, SeparationSettings, models};
use gtk::glib;
use gtk::prelude::*;
use std::rc::Rc;
use std::time::Duration;

/// Change how separations run everywhere: the session, the saved
/// preferences.
fn apply(app: &Rc<AppState>, settings: SeparationSettings) {
    app.session
        .borrow_mut()
        .set_separation_settings(settings.clone());
    let mut p = Preferences::load();
    p.separation = settings;
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
}

/// The refinement choices, by passes.
const PASSES: [&str; 4] = [
    "None — fastest (the stems may not add up to the original)",
    "One pass — as the model's authors separate",
    "Two passes — a cleaner stereo image, slower",
    "Three passes",
];

fn about(model: &ModelInfo) -> String {
    let mut s = format!(
        "{}\nLicence: {} — {}\nTrained on {}.",
        model.attribution, model.licence, model.licence_url, model.training
    );
    if let Some(c) = model.caveat {
        s.push_str(&format!("\n{c}"));
    }
    s
}

pub fn page(app: &Rc<AppState>, row: impl Fn(&gtk::Grid, i32, &str, &gtk::Widget)) -> gtk::Widget {
    let g = gtk::Grid::new();
    g.set_row_spacing(10);
    g.set_column_spacing(14);
    g.add_css_class("audio-settings");
    let now = app.session.borrow().separation_settings().clone();

    let names: Vec<&str> = models::MODELS.iter().map(|m| m.name).collect();
    let model = gtk::DropDown::from_strings(&names);
    model.set_selected(
        models::MODELS
            .iter()
            .position(|m| m.id == now.model)
            .unwrap_or(0) as u32,
    );
    row(&g, 0, "Model", model.upcast_ref());

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_hexpand(true);
    let download = gtk::Button::with_label("Download");
    let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    status_row.append(&status);
    status_row.append(&download);
    row(&g, 1, "On this computer", status_row.upcast_ref());

    let credit = gtk::Label::new(None);
    credit.set_wrap(true);
    credit.set_xalign(0.0);
    credit.set_selectable(true);
    credit.add_css_class("dim-label");
    g.attach(&credit, 1, 2, 1, 1);

    let passes = gtk::DropDown::from_strings(&PASSES);
    passes.set_selected(u32::from(now.iterations.min(MAX_ITERATIONS)));
    row(&g, 3, "Refinement", passes.upcast_ref());

    let how = gtk::Label::new(Some(
        "Separate a clip from its menu (Stems) or a whole track from the track's menu \
         (Separate Stems): the stems arrive on new tracks where the audio was, the \
         original muted, as one undo step. A model not yet on this computer is \
         downloaded first. Separating runs beside playback on all but one core.",
    ));
    how.set_wrap(true);
    how.set_xalign(0.0);
    how.add_css_class("dim-label");
    g.attach(&how, 0, 4, 2, 1);

    let chosen = |m: &gtk::DropDown| {
        models::MODELS
            .get(m.selected() as usize)
            .unwrap_or(models::default_model())
    };
    {
        let app = Rc::clone(app);
        let passes = passes.clone();
        model.connect_selected_notify(move |m| {
            apply(
                &app,
                SeparationSettings {
                    model: chosen(m).id.to_string(),
                    iterations: passes.selected().min(u32::from(MAX_ITERATIONS)) as u8,
                },
            );
        });
    }
    {
        let app = Rc::clone(app);
        let model = model.clone();
        passes.connect_selected_notify(move |p| {
            apply(
                &app,
                SeparationSettings {
                    model: chosen(&model).id.to_string(),
                    iterations: p.selected().min(u32::from(MAX_ITERATIONS)) as u8,
                },
            );
        });
    }
    {
        let app = Rc::downgrade(app);
        let model = model.clone();
        download.connect_clicked(move |_| {
            if let Some(a) = app.upgrade() {
                a.dispatch(Action::DownloadSeparationModel(Some(
                    chosen(&model).id.to_string(),
                )));
            }
        });
    }
    // Live: the model's state and the download's progress.
    let weak = Rc::downgrade(app);
    let alive = g.downgrade();
    let refresh = move || {
        let (Some(a), Some(_)) = (weak.upgrade(), alive.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        let m = chosen(&model);
        let s = a.session.borrow();
        let (text, can) = match s.separation_download() {
            Some((d, done, total)) if d.id == m.id => (
                format!(
                    "Downloading… {} of {} MB",
                    done / 1_000_000,
                    total / 1_000_000
                ),
                false,
            ),
            Some((d, _, _)) => (format!("Waiting for {} to download", d.name), false),
            None if s.separation_model_ready(m.id) => ("Downloaded".to_string(), false),
            None => (
                format!("Not downloaded ({} MB)", m.bytes.div_ceil(1_000_000)),
                true,
            ),
        };
        if status.text() != text {
            status.set_text(&text);
        }
        download.set_sensitive(can);
        let about = about(m);
        if credit.text() != about {
            credit.set_text(&about);
        }
        glib::ControlFlow::Continue
    };
    refresh();
    glib::timeout_add_local(Duration::from_millis(300), refresh);
    g.upcast()
}
