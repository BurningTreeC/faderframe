//! The Batch Processing window (File → Batch Processing…): a list of files
//! (added with a chooser, a folder or dropped in), a chain of steps —
//! plugins, normalising, loudness, conversion, trimming silence, fades —
//! each with its settings, the output (folder, format, depth, dither, a
//! rename pattern, what to do with names that are taken, how many files
//! at once), presets of chains, and Run/Cancel with every file's progress.
//! The work is `faderframe_session::batch`'s: it runs on worker threads
//! of its own, never touches the project, and the window only shows what
//! it reports.
//!
//! A plugin step is chosen from the installed and built-in effects; a
//! built-in takes its factory presets, any plugin its values in a generic
//! parameter window, and the state and values of one of the project's
//! inserts of the same plugin ("Take Settings From") — set it up with its
//! own editor on a track, then take it into the chain.
//!
//! What the window holds is kept while the program runs: closed and opened
//! again, it shows the same files and chain.
//!
//! Development aid: `batch:<preset>=<file>[|<file>…]` runs a preset on the
//! files into a "Batch" folder beside the first one and logs how each went
//! ([`dev_batch`]).

use crate::state::AppState;
use faderframe_audio_files::{Dither, WavFormat};
use faderframe_plugin_host::{ParameterInfo, ParameterUnit};
use faderframe_project::{FadeShape, PluginFormat, PluginRef};
use faderframe_session::batch::{
    self, BatchChain, BatchPreset, BatchRequest, BatchRun, BatchStep, Channels, FileFormat,
    FileState, IfExists, PeakKind, PluginStep, SAMPLE_RATES, Stamp, StepKind,
};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

/// What the window holds (kept while the program runs).
#[derive(Clone, Debug)]
struct Kept {
    files: Vec<PathBuf>,
    chain: BatchChain,
    /// `None`: a "Batch" folder beside the first file.
    folder: Option<PathBuf>,
    existing: IfExists,
    workers: usize,
    /// The preset last loaded or saved (`{preset}`).
    preset: String,
}

impl Default for Kept {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            chain: BatchChain::default(),
            folder: None,
            existing: IfExists::default(),
            workers: batch::default_workers(),
            preset: String::new(),
        }
    }
}

thread_local! {
    static OPEN: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
    static KEPT: RefCell<Option<Kept>> = const { RefCell::new(None) };
}

/// The preset list's first entry.
const NO_PRESET: &str = "Presets…";

/// A file's row.
struct FileRow {
    status: gtk::Label,
    progress: gtk::ProgressBar,
}

struct Ui {
    win: gtk::Window,
    model: RefCell<Kept>,
    run: RefCell<Option<BatchRun>>,
    /// The files the running batch took (rows follow them).
    running: RefCell<Vec<PathBuf>>,
    presets: gtk::DropDown,
    preset_list: RefCell<Vec<BatchPreset>>,
    delete_preset: gtk::Button,
    files: gtk::ListBox,
    file_rows: RefCell<Vec<FileRow>>,
    steps: gtk::Box,
    /// Each step's summary line (updated as its settings change).
    summaries: RefCell<Vec<gtk::Label>>,
    /// Parameter windows of plugin steps (closed when the steps change).
    parameter_windows: RefCell<Vec<glib::WeakRef<gtk::Window>>>,
    /// The effects a plugin step can be.
    plugins: Vec<PluginRef>,
    plugin_choice: gtk::DropDown,
    folder: gtk::Label,
    format: gtk::DropDown,
    depth: gtk::DropDown,
    dither: gtk::DropDown,
    pattern: gtk::Entry,
    existing: gtk::DropDown,
    workers: gtk::SpinButton,
    progress: gtk::ProgressBar,
    status: gtk::Label,
    run_button: gtk::Button,
    cancel_button: gtk::Button,
    show_folder: gtk::Button,
    /// Set while the window itself changes its controls.
    updating: Cell<bool>,
}

fn margins(w: &impl IsA<gtk::Widget>, m: i32) {
    w.set_margin_top(m);
    w.set_margin_bottom(m);
    w.set_margin_start(m);
    w.set_margin_end(m);
}

fn heading(text: &str) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_markup(&format!("<b>{}</b>", glib::markup_escape_text(text)));
    l.set_xalign(0.0);
    l
}

fn spin(min: f64, max: f64, step: f64, digits: u32, value: f64) -> gtk::SpinButton {
    let s = gtk::SpinButton::with_range(min, max, step);
    s.set_digits(digits);
    s.set_value(value);
    s
}

fn labelled(text: &str, w: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dim-label");
    b.append(&l);
    b.append(w);
    b
}

/// The searchable drop-down of strings.
fn searchable(names: &[String]) -> gtk::DropDown {
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&refs);
    let expression = gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<&gtk::Expression>,
        "string",
    );
    let d = gtk::DropDown::new(Some(model), Some(expression));
    d.set_enable_search(true);
    d.set_search_match_mode(gtk::StringFilterMatchMode::Substring);
    d
}

/// Show the window (raised if it is open).
pub fn open(app: &Rc<AppState>) {
    if let Some(w) = OPEN.with(|o| o.borrow().as_ref().and_then(glib::WeakRef::upgrade)) {
        w.present();
        return;
    }
    let main = app.window.borrow().clone();
    let win = gtk::Window::builder()
        .application(&app.app)
        .title("Batch Processing")
        .default_width(1120)
        .default_height(760)
        .build();
    if let Some(main) = &main {
        win.set_transient_for(Some(main));
    }
    let kept = KEPT.with(|k| k.borrow().clone()).unwrap_or_default();

    // The effects a step can be: built-ins first, then by name.
    let mut plugins: Vec<PluginRef> = app
        .session
        .borrow()
        .available_plugins()
        .into_iter()
        .filter(|p| {
            !p.instrument && !p.midi_effect && p.audio_inputs > 0 && batch::batchable(&p.plugin)
        })
        .map(|p| p.plugin)
        .collect();
    plugins.sort_by_key(|p| (p.format != PluginFormat::Builtin, p.name.to_lowercase()));
    let plugin_names: Vec<String> = plugins
        .iter()
        .map(|p| match p.format {
            PluginFormat::Builtin => p.name.clone(),
            f => format!("{} ({f:?})", p.name),
        })
        .collect();

    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    margins(&body, 14);

    // Presets.
    let preset_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let presets = gtk::DropDown::from_strings(&[NO_PRESET]);
    presets.set_hexpand(true);
    let save_preset = gtk::Button::with_label("Save Preset…");
    let delete_preset = gtk::Button::with_label("Delete Preset");
    preset_row.append(&labelled("Preset", &presets));
    preset_row.append(&save_preset);
    preset_row.append(&delete_preset);
    body.append(&preset_row);

    // Files | steps.
    let columns = gtk::Paned::new(gtk::Orientation::Horizontal);
    columns.set_vexpand(true);
    columns.set_wide_handle(true);
    // Never below a column's own width: the window grows instead of
    // cutting the buttons off.
    columns.set_shrink_start_child(false);
    columns.set_shrink_end_child(false);
    let left = gtk::Box::new(gtk::Orientation::Vertical, 6);
    left.append(&heading("Files"));
    let files = gtk::ListBox::new();
    files.set_selection_mode(gtk::SelectionMode::Multiple);
    let placeholder = gtk::Label::new(Some(
        "Add files with the buttons below, or drop files and folders here.",
    ));
    placeholder.set_wrap(true);
    placeholder.add_css_class("dim-label");
    margins(&placeholder, 24);
    files.set_placeholder(Some(&placeholder));
    let file_scroll = gtk::ScrolledWindow::builder()
        .child(&files)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(300)
        .build();
    left.append(&file_scroll);
    let file_buttons = gtk::FlowBox::new();
    file_buttons.set_selection_mode(gtk::SelectionMode::None);
    file_buttons.set_column_spacing(6);
    file_buttons.set_row_spacing(6);
    file_buttons.set_max_children_per_line(4);
    let add_files = gtk::Button::with_label("Add Files…");
    let add_folder = gtk::Button::with_label("Add Folder…");
    let remove_files = gtk::Button::with_label("Remove");
    let clear_files = gtk::Button::with_label("Clear");
    for b in [&add_files, &add_folder, &remove_files, &clear_files] {
        file_buttons.append(b);
    }
    left.append(&file_buttons);
    margins(&left, 4);
    columns.set_start_child(Some(&left));

    let right = gtk::Box::new(gtk::Orientation::Vertical, 6);
    right.append(&heading("Steps (in order)"));
    let steps = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let step_scroll = gtk::ScrolledWindow::builder()
        .child(&steps)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(380)
        .build();
    right.append(&step_scroll);
    // The step buttons wrap when the column is narrow.
    let add_row = gtk::FlowBox::new();
    add_row.set_selection_mode(gtk::SelectionMode::None);
    add_row.set_column_spacing(6);
    add_row.set_row_spacing(6);
    add_row.set_max_children_per_line(StepKind::ALL.len() as u32);
    let add_label = gtk::Label::new(Some("Add"));
    add_label.add_css_class("dim-label");
    add_row.append(&add_label);
    let mut kind_buttons = Vec::new();
    for kind in StepKind::ALL {
        if kind == StepKind::Plugin {
            continue;
        }
        let b = gtk::Button::with_label(kind.label());
        add_row.append(&b);
        kind_buttons.push((kind, b));
    }
    right.append(&add_row);
    let plugin_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let plugin_choice = searchable(&plugin_names);
    plugin_choice.set_hexpand(true);
    plugin_choice.set_tooltip_text(Some("Type to search the installed and built-in effects"));
    let add_plugin = gtk::Button::with_label("Add Plugin");
    plugin_row.append(&labelled("Plugin", &plugin_choice));
    plugin_row.append(&add_plugin);
    right.append(&plugin_row);
    margins(&right, 4);
    columns.set_end_child(Some(&right));
    columns.set_position(440);
    body.append(&columns);

    // Output.
    body.append(&heading("Output"));
    let out = gtk::Grid::new();
    out.set_row_spacing(6);
    out.set_column_spacing(10);
    let label = |text: &str, x: i32, y: i32| {
        let l = gtk::Label::new(Some(text));
        l.set_halign(gtk::Align::End);
        out.attach(&l, x, y, 1, 1);
    };
    let folder = gtk::Label::new(None);
    folder.set_xalign(0.0);
    folder.set_hexpand(true);
    folder.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let choose_folder = gtk::Button::with_label("Choose…");
    let folder_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    folder_box.append(&folder);
    folder_box.append(&choose_folder);
    label("Folder", 0, 0);
    out.attach(&folder_box, 1, 0, 3, 1);
    let format_names: Vec<&str> = FileFormat::ALL.iter().map(|f| f.label()).collect();
    let format = gtk::DropDown::from_strings(&format_names);
    label("Format", 0, 1);
    out.attach(&format, 1, 1, 1, 1);
    let depth = gtk::DropDown::from_strings(&[]);
    label("Depth", 2, 1);
    out.attach(&depth, 3, 1, 1, 1);
    let dither_names: Vec<&str> = Dither::ALL.iter().map(|d| d.label()).collect();
    let dither = gtk::DropDown::from_strings(&dither_names);
    label("Dither", 0, 2);
    out.attach(&dither, 1, 2, 1, 1);
    let existing_names: Vec<&str> = IfExists::ALL.iter().map(|e| e.label()).collect();
    let existing = gtk::DropDown::from_strings(&existing_names);
    label("Name Taken", 2, 2);
    out.attach(&existing, 3, 2, 1, 1);
    let pattern = gtk::Entry::new();
    pattern.set_hexpand(true);
    let tokens: Vec<String> = batch::TOKENS
        .iter()
        .map(|(t, what)| format!("{t} — {what}"))
        .collect();
    pattern.set_tooltip_text(Some(&format!(
        "The written files' names (the extension is added):\n{}",
        tokens.join("\n")
    )));
    label("Name", 0, 3);
    out.attach(&pattern, 1, 3, 1, 1);
    let workers = spin(1.0, batch::MAX_WORKERS as f64, 1.0, 0, kept.workers as f64);
    workers.set_tooltip_text(Some(
        "Files processed at once (each is held in memory while it is processed)",
    ));
    label("Files at Once", 2, 3);
    out.attach(&workers, 3, 3, 1, 1);
    body.append(&out);

    // Progress and the buttons.
    let progress = gtk::ProgressBar::new();
    progress.set_show_text(true);
    body.append(&progress);
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_wrap(true);
    body.append(&status);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let show_folder = gtk::Button::with_label("Show Folder");
    let close = gtk::Button::with_label("Close");
    let cancel_button = gtk::Button::with_label("Cancel");
    let run_button = gtk::Button::with_label("Run");
    run_button.add_css_class("suggested-action");
    for b in [&show_folder, &close, &cancel_button, &run_button] {
        buttons.append(b);
    }
    body.append(&buttons);
    win.set_child(Some(&body));

    let ui = Rc::new(Ui {
        win: win.clone(),
        model: RefCell::new(kept),
        run: RefCell::new(None),
        running: RefCell::new(Vec::new()),
        presets,
        preset_list: RefCell::new(Vec::new()),
        delete_preset,
        files,
        file_rows: RefCell::new(Vec::new()),
        steps,
        summaries: RefCell::new(Vec::new()),
        parameter_windows: RefCell::new(Vec::new()),
        plugins,
        plugin_choice,
        folder,
        format,
        depth,
        dither,
        pattern,
        existing,
        workers,
        progress,
        status,
        run_button,
        cancel_button,
        show_folder,
        updating: Cell::new(false),
    });

    // Presets.
    {
        let (app, u) = (Rc::clone(app), Rc::clone(&ui));
        ui.presets.connect_selected_notify(move |d| {
            if u.updating.get() {
                return;
            }
            let chosen = (d.selected() as usize)
                .checked_sub(1)
                .and_then(|i| u.preset_list.borrow().get(i).cloned());
            if let Some(p) = chosen {
                {
                    let mut m = u.model.borrow_mut();
                    m.chain = p.chain.clone();
                    m.preset = p.name.clone();
                }
                show_all(&app, &u);
            }
            u.delete_preset
                .set_sensitive(chosen_user_preset(&u).is_some());
        });
    }
    {
        let u = Rc::clone(&ui);
        save_preset.connect_clicked(move |_| save_preset_dialog(&u));
    }
    {
        let u = Rc::clone(&ui);
        ui.delete_preset
            .connect_clicked(move |_| delete_preset_asked(&u));
    }
    // Files.
    {
        let u = Rc::clone(&ui);
        add_files.connect_clicked(move |_| choose_files(&u));
    }
    {
        let u = Rc::clone(&ui);
        add_folder.connect_clicked(move |_| choose_folder_of_files(&u));
    }
    {
        let u = Rc::clone(&ui);
        remove_files.connect_clicked(move |_| {
            let mut gone: Vec<usize> = u
                .files
                .selected_rows()
                .iter()
                .filter_map(|r| usize::try_from(r.index()).ok())
                .collect();
            gone.sort_unstable();
            {
                let mut m = u.model.borrow_mut();
                for i in gone.into_iter().rev() {
                    if i < m.files.len() {
                        m.files.remove(i);
                    }
                }
            }
            show_files(&u);
        });
    }
    {
        let u = Rc::clone(&ui);
        clear_files.connect_clicked(move |_| {
            u.model.borrow_mut().files.clear();
            show_files(&u);
        });
    }
    {
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let u = Rc::clone(&ui);
        drop.connect_drop(move |_, value, _, _| {
            let Ok(list) = value.get::<gdk::FileList>() else {
                return false;
            };
            let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
            add_paths(&u, &paths);
            true
        });
        file_scroll.add_controller(drop);
    }
    // Steps.
    for (kind, b) in kind_buttons {
        let (app, u) = (Rc::clone(app), Rc::clone(&ui));
        b.connect_clicked(move |_| {
            if let Some(step) = kind.default_step() {
                u.model.borrow_mut().chain.steps.push(step);
                show_steps(&app, &u);
            }
        });
    }
    {
        let (app, u) = (Rc::clone(app), Rc::clone(&ui));
        add_plugin.connect_clicked(move |_| {
            let Some(plugin) = u.plugins.get(u.plugin_choice.selected() as usize).cloned() else {
                return;
            };
            u.model
                .borrow_mut()
                .chain
                .steps
                .push(BatchStep::Plugin(PluginStep::new(plugin)));
            show_steps(&app, &u);
        });
    }
    // Output.
    {
        let u = Rc::clone(&ui);
        choose_folder.connect_clicked(move |_| choose_output_folder(&u));
    }
    {
        let u = Rc::clone(&ui);
        ui.format.connect_selected_notify(move |d| {
            if u.updating.get() {
                return;
            }
            if let Some(f) = FileFormat::ALL.get(d.selected() as usize) {
                let mut m = u.model.borrow_mut();
                m.chain.output.format = *f;
                if !f.depths().contains(&m.chain.output.depth) {
                    m.chain.output.depth = WavFormat::Pcm24;
                }
            }
            show_output(&u);
        });
    }
    {
        let u = Rc::clone(&ui);
        ui.depth.connect_selected_notify(move |d| {
            if u.updating.get() {
                return;
            }
            let integer = {
                let mut m = u.model.borrow_mut();
                if let Some(depth) = m.chain.output.format.depths().get(d.selected() as usize) {
                    m.chain.output.depth = *depth;
                }
                m.chain.output.depth.is_integer()
            };
            u.dither.set_sensitive(integer);
            show_state(&u);
        });
    }
    {
        let u = Rc::clone(&ui);
        ui.dither.connect_selected_notify(move |d| {
            if !u.updating.get()
                && let Some(dither) = Dither::ALL.get(d.selected() as usize)
            {
                u.model.borrow_mut().chain.output.dither = *dither;
            }
        });
    }
    {
        let u = Rc::clone(&ui);
        ui.existing.connect_selected_notify(move |d| {
            if !u.updating.get()
                && let Some(e) = IfExists::ALL.get(d.selected() as usize)
            {
                u.model.borrow_mut().existing = *e;
            }
        });
    }
    {
        let u = Rc::clone(&ui);
        ui.pattern.connect_changed(move |e| {
            if u.updating.get() {
                return;
            }
            let text = e.text().to_string();
            match batch::check_pattern(&text) {
                Ok(()) => e.remove_css_class("error"),
                Err(_) => e.add_css_class("error"),
            }
            u.model.borrow_mut().chain.output.pattern = text;
            show_state(&u);
        });
    }
    {
        let u = Rc::clone(&ui);
        ui.workers.connect_value_changed(move |s| {
            if !u.updating.get() {
                u.model.borrow_mut().workers = s.value() as usize;
            }
        });
    }
    // Running.
    {
        let u = Rc::clone(&ui);
        ui.run_button.connect_clicked(move |_| run(&u));
    }
    {
        let u = Rc::clone(&ui);
        ui.cancel_button.connect_clicked(move |_| {
            if let Some(r) = u.run.borrow().as_ref() {
                r.cancel();
            }
        });
    }
    {
        let u = Rc::clone(&ui);
        ui.show_folder.connect_clicked(move |_| {
            let dir = output_folder(&u.model.borrow());
            if let Some(dir) = dir {
                gtk::FileLauncher::new(Some(&gio::File::for_path(&dir))).launch(
                    Some(&u.win),
                    gio::Cancellable::NONE,
                    |res| {
                        if let Err(e) = res {
                            tracing::warn!("batch: could not open the folder: {e}");
                        }
                    },
                );
            }
        });
    }
    {
        let win = win.clone();
        close.connect_clicked(move |_| win.close());
    }
    {
        let u = Rc::clone(&ui);
        win.connect_close_request(move |w| {
            if u.run.borrow().as_ref().is_some_and(|r| !r.is_finished()) {
                ask_to_stop(&u, w);
                return glib::Propagation::Stop;
            }
            keep(&u);
            for p in u.parameter_windows.borrow_mut().drain(..) {
                if let Some(p) = p.upgrade() {
                    p.close();
                }
            }
            OPEN.with(|o| o.borrow_mut().take());
            glib::Propagation::Proceed
        });
    }
    OPEN.with(|o| *o.borrow_mut() = Some(win.downgrade()));
    refresh_presets(&ui, None);
    show_all(app, &ui);
    {
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(u) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            poll(&u);
            glib::ControlFlow::Continue
        });
    }
    win.present();
}

/// Keep what the window holds for the next time it opens.
fn keep(ui: &Ui) {
    let m = ui.model.borrow().clone();
    KEPT.with(|k| *k.borrow_mut() = Some(m));
}

/// Closing while a batch runs: stop it, or keep the window.
fn ask_to_stop(ui: &Rc<Ui>, w: &gtk::Window) {
    let dialog = gtk::AlertDialog::builder()
        .message("A batch is running")
        .detail("Closing the window stops it: files not yet written are not written.")
        .buttons(["Keep It Running", "Stop and Close"])
        .cancel_button(0)
        .default_button(0)
        .modal(true)
        .build();
    let (u, w2) = (Rc::clone(ui), w.clone());
    dialog.choose(Some(w), gio::Cancellable::NONE, move |res| {
        if res == Ok(1) {
            if let Some(r) = u.run.borrow_mut().take() {
                r.cancel();
            }
            w2.close();
        }
    });
}

/// Where the files go: the chosen folder, else "Batch" beside the first
/// file.
fn output_folder(m: &Kept) -> Option<PathBuf> {
    m.folder.clone().or_else(|| {
        m.files
            .first()
            .and_then(|f| f.parent())
            .map(|p| p.join("Batch"))
    })
}

fn show_all(app: &Rc<AppState>, ui: &Rc<Ui>) {
    show_files(ui);
    show_steps(app, ui);
    show_output(ui);
}

/// The preset drop-down from the presets on disk (`select`: the one to
/// show chosen).
fn refresh_presets(ui: &Rc<Ui>, select: Option<&str>) {
    let list = batch::presets();
    let mut names = vec![NO_PRESET.to_string()];
    names.extend(list.iter().map(|p| {
        if p.factory {
            p.name.clone()
        } else {
            format!("{} (yours)", p.name)
        }
    }));
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    ui.updating.set(true);
    ui.presets.set_model(Some(&gtk::StringList::new(&refs)));
    let at = select
        .and_then(|n| {
            list.iter()
                .position(|p| p.name.to_lowercase() == n.to_lowercase())
        })
        .map_or(0, |i| i + 1);
    ui.presets.set_selected(at as u32);
    *ui.preset_list.borrow_mut() = list;
    ui.updating.set(false);
    ui.delete_preset
        .set_sensitive(chosen_user_preset(ui).is_some());
}

/// The user's preset the drop-down shows.
fn chosen_user_preset(ui: &Ui) -> Option<BatchPreset> {
    (ui.presets.selected() as usize)
        .checked_sub(1)
        .and_then(|i| ui.preset_list.borrow().get(i).cloned())
        .filter(|p| !p.factory)
}

fn save_preset_dialog(ui: &Rc<Ui>) {
    let w = gtk::Window::builder()
        .title("Save Batch Preset")
        .transient_for(&ui.win)
        .modal(true)
        .default_width(380)
        .build();
    let b = gtk::Box::new(gtk::Orientation::Vertical, 10);
    margins(&b, 14);
    b.append(&gtk::Label::new(Some(
        "The chain and the output, under a name:",
    )));
    let entry = gtk::Entry::new();
    entry.set_text(&ui.model.borrow().preset);
    b.append(&entry);
    let note = gtk::Label::new(None);
    note.set_wrap(true);
    note.set_xalign(0.0);
    b.append(&note);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    row.append(&cancel);
    row.append(&save);
    b.append(&row);
    w.set_child(Some(&b));
    {
        let w = w.clone();
        cancel.connect_clicked(move |_| w.close());
    }
    let commit = {
        let (u, w, entry, note) = (Rc::clone(ui), w.clone(), entry.clone(), note.clone());
        move || {
            let name = entry.text().trim().to_string();
            if name.is_empty() {
                return;
            }
            let chain = u.model.borrow().chain.clone();
            match batch::save_preset(&name, &chain) {
                Ok(_) => {
                    u.model.borrow_mut().preset = name.clone();
                    refresh_presets(&u, Some(&name));
                    w.close();
                }
                Err(e) => note.set_text(&e.to_string()),
            }
        }
    };
    let commit = Rc::new(commit);
    {
        let c = Rc::clone(&commit);
        save.connect_clicked(move |_| c());
    }
    entry.connect_activate(move |_| commit());
    w.present();
}

fn delete_preset_asked(ui: &Rc<Ui>) {
    let Some(p) = chosen_user_preset(ui) else {
        return;
    };
    let dialog = gtk::AlertDialog::builder()
        .message(format!("Delete the preset “{}”?", p.name))
        .buttons(["Cancel", "Delete"])
        .cancel_button(0)
        .default_button(0)
        .modal(true)
        .build();
    let u = Rc::clone(ui);
    dialog.choose(Some(&ui.win), gio::Cancellable::NONE, move |res| {
        if res != Ok(1) {
            return;
        }
        match batch::delete_preset(&p.name) {
            Ok(()) => refresh_presets(&u, None),
            Err(e) => say(&u, &e.to_string(), true),
        }
    });
}

/// A line under the progress bar (`error`: shown as one).
fn say(ui: &Ui, text: &str, error: bool) {
    ui.status.set_text(text);
    if error {
        ui.status.add_css_class("error");
    } else {
        ui.status.remove_css_class("error");
    }
}

/// Audio files in `paths`, folders searched through (sorted by name).
fn audio_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        found.sort();
        for p in found {
            if p.is_dir() {
                if depth < 8 {
                    walk(&p, out, depth + 1);
                }
            } else if faderframe_audio_files::decode::is_supported(&p) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk(p, &mut out, 0);
        } else {
            out.push(p.clone());
        }
    }
    out
}

fn add_paths(ui: &Rc<Ui>, paths: &[PathBuf]) {
    if ui.run.borrow().as_ref().is_some_and(|r| !r.is_finished()) {
        say(ui, "Files are added once the batch is done.", true);
        return;
    }
    let found = audio_files(paths);
    {
        let mut m = ui.model.borrow_mut();
        for f in found {
            if !m.files.contains(&f) {
                m.files.push(f);
            }
        }
    }
    show_files(ui);
}

fn choose_files(ui: &Rc<Ui>) {
    let dialog = gtk::FileDialog::builder()
        .title("Add Files to the Batch")
        .accept_label("Add")
        .modal(true)
        .filters(&crate::dialogs::audio_filters())
        .build();
    let u = Rc::clone(ui);
    dialog.open_multiple(Some(&ui.win), gio::Cancellable::NONE, move |res| {
        let Ok(list) = res else { return };
        let paths: Vec<PathBuf> = (0..list.n_items())
            .filter_map(|i| list.item(i).and_downcast::<gio::File>())
            .filter_map(|f| f.path())
            .collect();
        add_paths(&u, &paths);
    });
}

fn choose_folder_of_files(ui: &Rc<Ui>) {
    let dialog = gtk::FileDialog::builder()
        .title("Add a Folder's Audio Files")
        .accept_label("Add")
        .modal(true)
        .build();
    let u = Rc::clone(ui);
    dialog.select_folder(Some(&ui.win), gio::Cancellable::NONE, move |res| {
        if let Some(p) = res.ok().and_then(|f| f.path()) {
            add_paths(&u, &[p]);
        }
    });
}

fn choose_output_folder(ui: &Rc<Ui>) {
    let dialog = gtk::FileDialog::builder()
        .title("Write the Files Into")
        .accept_label("Choose")
        .modal(true)
        .build();
    if let Some(dir) = output_folder(&ui.model.borrow()).filter(|d| d.is_dir()) {
        dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
    }
    let u = Rc::clone(ui);
    dialog.select_folder(Some(&ui.win), gio::Cancellable::NONE, move |res| {
        if let Some(p) = res.ok().and_then(|f| f.path()) {
            u.model.borrow_mut().folder = Some(p);
            show_output(&u);
        }
    });
}

/// The file list (rows for the files, or for the running batch's).
fn show_files(ui: &Rc<Ui>) {
    ui.files.remove_all();
    let running = ui.running.borrow().clone();
    let files = if running.is_empty() {
        ui.model.borrow().files.clone()
    } else {
        running
    };
    let mut rows = Vec::with_capacity(files.len());
    for f in &files {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
        margins(&row, 4);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = gtk::Label::new(Some(&f.file_name().map_or_else(
            || f.display().to_string(),
            |n| n.to_string_lossy().to_string(),
        )));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.set_tooltip_text(Some(&f.display().to_string()));
        let progress = gtk::ProgressBar::new();
        progress.set_size_request(90, -1);
        progress.set_valign(gtk::Align::Center);
        progress.set_visible(false);
        top.append(&name);
        top.append(&progress);
        row.append(&top);
        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.add_css_class("dim-label");
        status.set_visible(false);
        row.append(&status);
        ui.files.append(&row);
        rows.push(FileRow { status, progress });
    }
    *ui.file_rows.borrow_mut() = rows;
    show_state(ui);
}

/// The output controls from the model.
fn show_output(ui: &Rc<Ui>) {
    ui.updating.set(true);
    {
        let m = ui.model.borrow();
        let o = &m.chain.output;
        ui.folder.set_text(&match (&m.folder, output_folder(&m)) {
            (Some(f), _) => f.display().to_string(),
            (None, Some(f)) => format!("{} (beside the first file)", f.display()),
            (None, None) => "A “Batch” folder beside the first file".into(),
        });
        ui.format.set_selected(
            FileFormat::ALL
                .iter()
                .position(|f| *f == o.format)
                .unwrap_or(0) as u32,
        );
        let depths: Vec<&str> = o
            .format
            .depths()
            .iter()
            .map(|d| match d {
                WavFormat::Pcm16 => "16-bit",
                WavFormat::Pcm24 => "24-bit",
                WavFormat::Float32 => "32-bit float",
            })
            .collect();
        ui.depth.set_model(Some(&gtk::StringList::new(&depths)));
        ui.depth.set_selected(
            o.format
                .depths()
                .iter()
                .position(|d| *d == o.depth)
                .unwrap_or(0) as u32,
        );
        ui.dither
            .set_selected(Dither::ALL.iter().position(|d| *d == o.dither).unwrap_or(0) as u32);
        ui.dither.set_sensitive(o.depth.is_integer());
        if ui.pattern.text() != o.pattern {
            ui.pattern.set_text(&o.pattern);
        }
        ui.existing.set_selected(
            IfExists::ALL
                .iter()
                .position(|e| *e == m.existing)
                .unwrap_or(0) as u32,
        );
        ui.workers.set_value(m.workers as f64);
    }
    ui.updating.set(false);
    show_state(ui);
}

/// Run's and Cancel's sensitivity, the line under the progress bar.
fn show_state(ui: &Rc<Ui>) {
    let running = ui.run.borrow().as_ref().is_some_and(|r| !r.is_finished());
    let m = ui.model.borrow();
    let problem = if m.files.is_empty() {
        Some("Add the files to process.".to_string())
    } else {
        m.chain.check().err().map(|e| e.to_string())
    };
    ui.run_button.set_sensitive(!running && problem.is_none());
    ui.cancel_button.set_sensitive(running);
    ui.show_folder
        .set_sensitive(output_folder(&m).is_some_and(|d| d.is_dir()));
    if !running && ui.run.borrow().is_none() {
        match &problem {
            Some(p) => say(ui, p, m.chain.check().is_err()),
            None => say(
                ui,
                &format!(
                    "{} file{} through {} step{}.",
                    m.files.len(),
                    if m.files.len() == 1 { "" } else { "s" },
                    m.chain.steps.len(),
                    if m.chain.steps.len() == 1 { "" } else { "s" }
                ),
                false,
            ),
        }
    }
}

/// The step cards, made again.
fn show_steps(app: &Rc<AppState>, ui: &Rc<Ui>) {
    for p in ui.parameter_windows.borrow_mut().drain(..) {
        if let Some(p) = p.upgrade() {
            p.close();
        }
    }
    while let Some(c) = ui.steps.first_child() {
        ui.steps.remove(&c);
    }
    let steps = ui.model.borrow().chain.steps.clone();
    let mut summaries = Vec::with_capacity(steps.len());
    if steps.is_empty() {
        let l = gtk::Label::new(Some(
            "No steps: the files are converted to the output's format as they are.",
        ));
        l.set_wrap(true);
        l.add_css_class("dim-label");
        margins(&l, 24);
        ui.steps.append(&l);
    }
    for (i, step) in steps.iter().enumerate() {
        let (card, summary) = step_card(app, ui, i, step, steps.len());
        ui.steps.append(&card);
        summaries.push(summary);
    }
    *ui.summaries.borrow_mut() = summaries;
    show_state(ui);
}

/// Change step `index` in the model and its summary line.
fn edit_step(ui: &Rc<Ui>, index: usize, f: impl FnOnce(&mut BatchStep)) {
    let summary = {
        let mut m = ui.model.borrow_mut();
        let Some(step) = m.chain.steps.get_mut(index) else {
            return;
        };
        f(step);
        step.summary()
    };
    if let Some(l) = ui.summaries.borrow().get(index) {
        l.set_text(&summary);
    }
    show_state(ui);
}

fn step_card(
    app: &Rc<AppState>,
    ui: &Rc<Ui>,
    index: usize,
    step: &BatchStep,
    count: usize,
) -> (gtk::Frame, gtk::Label) {
    let frame = gtk::Frame::new(None);
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    margins(&b, 8);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let title = heading(&format!("{}. {}", index + 1, step.label()));
    title.set_hexpand(true);
    top.append(&title);
    let up = gtk::Button::from_icon_name("go-up-symbolic");
    up.set_tooltip_text(Some("Earlier"));
    up.set_sensitive(index > 0);
    let down = gtk::Button::from_icon_name("go-down-symbolic");
    down.set_tooltip_text(Some("Later"));
    down.set_sensitive(index + 1 < count);
    let remove = gtk::Button::from_icon_name("window-close-symbolic");
    remove.set_tooltip_text(Some("Remove the step"));
    for w in [&up, &down, &remove] {
        w.add_css_class("flat");
        top.append(w);
    }
    b.append(&top);
    let summary = gtk::Label::new(Some(&step.summary()));
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    summary.add_css_class("dim-label");
    b.append(&summary);
    let settings = gtk::FlowBox::new();
    settings.set_selection_mode(gtk::SelectionMode::None);
    settings.set_column_spacing(12);
    settings.set_row_spacing(6);
    settings.set_max_children_per_line(4);
    b.append(&settings);
    step_settings(app, ui, index, step, &settings);
    frame.set_child(Some(&b));

    let moved = |delta: isize| {
        let (app, u) = (Rc::clone(app), Rc::clone(ui));
        move |_: &gtk::Button| {
            {
                let mut m = u.model.borrow_mut();
                let to = index as isize + delta;
                if to < 0 || to as usize >= m.chain.steps.len() {
                    return;
                }
                m.chain.steps.swap(index, to as usize);
            }
            show_steps(&app, &u);
        }
    };
    up.connect_clicked(moved(-1));
    down.connect_clicked(moved(1));
    {
        let (app, u) = (Rc::clone(app), Rc::clone(ui));
        remove.connect_clicked(move |_| {
            {
                let mut m = u.model.borrow_mut();
                if index < m.chain.steps.len() {
                    m.chain.steps.remove(index);
                }
            }
            show_steps(&app, &u);
        });
    }
    (frame, summary)
}

/// A spin button writing into step `index`.
fn step_spin(
    ui: &Rc<Ui>,
    index: usize,
    (min, max, step, digits): (f64, f64, f64, u32),
    value: f64,
    set: impl Fn(&mut BatchStep, f64) + 'static,
) -> gtk::SpinButton {
    let s = spin(min, max, step, digits, value);
    let u = Rc::clone(ui);
    s.connect_value_changed(move |s| {
        let v = s.value();
        edit_step(&u, index, |st| set(st, v));
    });
    s
}

/// A check button writing into step `index`.
fn step_check(
    ui: &Rc<Ui>,
    index: usize,
    label: &str,
    on: bool,
    set: impl Fn(&mut BatchStep, bool) + 'static,
) -> gtk::CheckButton {
    let c = gtk::CheckButton::with_label(label);
    c.set_active(on);
    let u = Rc::clone(ui);
    c.connect_toggled(move |c| {
        let on = c.is_active();
        edit_step(&u, index, |st| set(st, on));
    });
    c
}

/// A drop-down writing its choice into step `index`.
fn step_choice(
    ui: &Rc<Ui>,
    index: usize,
    names: &[&str],
    selected: usize,
    set: impl Fn(&mut BatchStep, usize) + 'static,
) -> gtk::DropDown {
    let d = gtk::DropDown::from_strings(names);
    d.set_selected(selected as u32);
    let u = Rc::clone(ui);
    d.connect_selected_notify(move |d| {
        let i = d.selected() as usize;
        edit_step(&u, index, |st| set(st, i));
    });
    d
}

fn shape_names() -> Vec<&'static str> {
    FadeShape::ALL.iter().map(|s| s.label()).collect()
}

fn shape_index(s: FadeShape) -> usize {
    FadeShape::ALL.iter().position(|x| *x == s).unwrap_or(0)
}

fn step_settings(
    app: &Rc<AppState>,
    ui: &Rc<Ui>,
    index: usize,
    step: &BatchStep,
    into: &gtk::FlowBox,
) {
    let add = |text: &str, w: &gtk::Widget| into.insert(&labelled(text, w), -1);
    match step {
        BatchStep::Normalize(n) => {
            let peak = step_choice(
                ui,
                index,
                &["True Peak", "Sample Peak"],
                usize::from(n.peak == PeakKind::Sample),
                |st, i| {
                    if let BatchStep::Normalize(n) = st {
                        n.peak = if i == 1 {
                            PeakKind::Sample
                        } else {
                            PeakKind::True
                        };
                    }
                },
            );
            add("Peak", peak.upcast_ref());
            let ceiling = step_spin(
                ui,
                index,
                (-40.0, 0.0, 0.1, 1),
                f64::from(n.ceiling_db),
                |st, v| {
                    if let BatchStep::Normalize(n) = st {
                        n.ceiling_db = v as f32;
                    }
                },
            );
            add("To (dB)", ceiling.upcast_ref());
        }
        BatchStep::Loudness(l) => {
            let target = step_spin(
                ui,
                index,
                (-40.0, -5.0, 0.5, 1),
                f64::from(l.target_lufs),
                |st, v| {
                    if let BatchStep::Loudness(l) = st {
                        l.target_lufs = v as f32;
                    }
                },
            );
            add("Target (LUFS)", target.upcast_ref());
            let ceiling = step_spin(
                ui,
                index,
                (-10.0, 0.0, 0.1, 1),
                f64::from(l.true_peak_db),
                |st, v| {
                    if let BatchStep::Loudness(l) = st {
                        l.true_peak_db = v as f32;
                    }
                },
            );
            add("True Peak at Most (dBTP)", ceiling.upcast_ref());
            let lower = step_check(
                ui,
                index,
                "Less gain rather than limiting",
                l.lower_gain,
                |st, on| {
                    if let BatchStep::Loudness(l) = st {
                        l.lower_gain = on;
                    }
                },
            );
            into.insert(&lower, -1);
        }
        BatchStep::Convert(c) => {
            let mut rates: Vec<String> = vec!["Keep".into()];
            rates.extend(SAMPLE_RATES.iter().map(|r| format!("{r} Hz")));
            let refs: Vec<&str> = rates.iter().map(String::as_str).collect();
            let at = c
                .sample_rate
                .and_then(|r| SAMPLE_RATES.iter().position(|x| *x == r))
                .map_or(0, |i| i + 1);
            let rate = step_choice(ui, index, &refs, at, |st, i| {
                if let BatchStep::Convert(c) = st {
                    c.sample_rate = i.checked_sub(1).and_then(|i| SAMPLE_RATES.get(i).copied());
                }
            });
            add("Sample Rate", rate.upcast_ref());
            let names: Vec<&str> = Channels::ALL.iter().map(|c| c.label()).collect();
            let at = Channels::ALL
                .iter()
                .position(|x| *x == c.channels)
                .unwrap_or(0);
            let channels = step_choice(ui, index, &names, at, |st, i| {
                if let BatchStep::Convert(c) = st {
                    c.channels = Channels::ALL.get(i).copied().unwrap_or_default();
                }
            });
            add("Channels", channels.upcast_ref());
        }
        BatchStep::TrimSilence(t) => {
            let threshold = step_spin(
                ui,
                index,
                (-96.0, -10.0, 1.0, 0),
                f64::from(t.threshold_db),
                |st, v| {
                    if let BatchStep::TrimSilence(t) = st {
                        t.threshold_db = v as f32;
                    }
                },
            );
            add("Below (dBFS)", threshold.upcast_ref());
            let ms = |v: f64| (v * 1000.0).round();
            let pre = step_spin(ui, index, (0.0, 2000.0, 5.0, 0), ms(t.pre_pad), |st, v| {
                if let BatchStep::TrimSilence(t) = st {
                    t.pre_pad = v / 1000.0;
                }
            });
            add("Keep Before (ms)", pre.upcast_ref());
            let post = step_spin(
                ui,
                index,
                (0.0, 5000.0, 10.0, 0),
                ms(t.post_pad),
                |st, v| {
                    if let BatchStep::TrimSilence(t) = st {
                        t.post_pad = v / 1000.0;
                    }
                },
            );
            add("Keep After (ms)", post.upcast_ref());
            let min = step_spin(
                ui,
                index,
                (0.0, 1000.0, 5.0, 0),
                ms(t.min_length),
                |st, v| {
                    if let BatchStep::TrimSilence(t) = st {
                        t.min_length = v / 1000.0;
                    }
                },
            );
            min.set_tooltip_text(Some("Shorter sounds (a click in the silence) do not count"));
            add("Shortest Sound (ms)", min.upcast_ref());
            let start = step_check(ui, index, "Start", t.start, |st, on| {
                if let BatchStep::TrimSilence(t) = st {
                    t.start = on;
                }
            });
            into.insert(&start, -1);
            let end = step_check(ui, index, "End", t.end, |st, on| {
                if let BatchStep::TrimSilence(t) = st {
                    t.end = on;
                }
            });
            into.insert(&end, -1);
        }
        BatchStep::Fade(f) => {
            let ms = |v: f64| (v * 1000.0).round();
            let fade_in = step_spin(
                ui,
                index,
                (0.0, 60_000.0, 5.0, 0),
                ms(f.fade_in),
                |st, v| {
                    if let BatchStep::Fade(f) = st {
                        f.fade_in = v / 1000.0;
                    }
                },
            );
            add("In (ms)", fade_in.upcast_ref());
            let shapes = shape_names();
            let shape_in = step_choice(ui, index, &shapes, shape_index(f.shape_in), |st, i| {
                if let BatchStep::Fade(f) = st {
                    f.shape_in = FadeShape::ALL.get(i).copied().unwrap_or_default();
                }
            });
            add("Shape", shape_in.upcast_ref());
            let fade_out = step_spin(
                ui,
                index,
                (0.0, 60_000.0, 5.0, 0),
                ms(f.fade_out),
                |st, v| {
                    if let BatchStep::Fade(f) = st {
                        f.fade_out = v / 1000.0;
                    }
                },
            );
            add("Out (ms)", fade_out.upcast_ref());
            let shape_out = step_choice(ui, index, &shapes, shape_index(f.shape_out), |st, i| {
                if let BatchStep::Fade(f) = st {
                    f.shape_out = FadeShape::ALL.get(i).copied().unwrap_or_default();
                }
            });
            add("Shape", shape_out.upcast_ref());
        }
        BatchStep::Plugin(p) => plugin_settings(app, ui, index, p, into),
    }
}

/// A plugin step's settings: a built-in's factory presets, the generic
/// parameters, an insert's settings taken over, the tail.
fn plugin_settings(
    app: &Rc<AppState>,
    ui: &Rc<Ui>,
    index: usize,
    p: &PluginStep,
    into: &gtk::FlowBox,
) {
    if p.plugin.format == PluginFormat::Builtin {
        let presets = faderframe_plugin_host::presets::factory_presets(&p.plugin.id);
        if !presets.is_empty() {
            let mut names = vec!["Choose…".to_string()];
            names.extend(presets.iter().map(|f| match f.group {
                Some(g) => format!("{g} · {}", f.name),
                None => f.name.to_string(),
            }));
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let d = gtk::DropDown::from_strings(&refs);
            let (app, u) = (Rc::clone(app), Rc::clone(ui));
            d.connect_selected_notify(move |d| {
                let Some(n) = (d.selected() as usize).checked_sub(1) else {
                    return;
                };
                edit_step(&u, index, |st| {
                    if let BatchStep::Plugin(p) = st {
                        p.use_factory_preset(n);
                    }
                });
                // The parameter windows show the new values.
                show_steps(&app, &u);
            });
            into.insert(&labelled("Factory Preset", &d), -1);
        }
    }
    // The project's inserts of this plugin.
    let inserts: Vec<(
        String,
        Option<String>,
        Vec<faderframe_project::SavedParameter>,
    )> = {
        let s = app.session.borrow();
        s.project()
            .tracks
            .iter()
            .flat_map(|t| {
                t.slots()
                    .into_iter()
                    .filter(|s| s.plugin.format == p.plugin.format && s.plugin.id == p.plugin.id)
                    .map(|s| {
                        (
                            format!("{} · {}", t.name, t.name_of(s)),
                            s.state.clone(),
                            s.parameters.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    if !inserts.is_empty() {
        let mut names = vec!["Choose…".to_string()];
        names.extend(inserts.iter().map(|(n, _, _)| n.clone()));
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let d = gtk::DropDown::from_strings(&refs);
        d.set_tooltip_text(Some(
            "The state and values of one of the project's inserts of this plugin \
             (set it up with its own editor on a track first)",
        ));
        let (app, u) = (Rc::clone(app), Rc::clone(ui));
        let plugin = p.plugin.clone();
        d.connect_selected_notify(move |d| {
            let Some(n) = (d.selected() as usize).checked_sub(1) else {
                return;
            };
            // The hosted instances' states as they are now.
            app.session.borrow_mut().capture_plugin_states();
            let taken = {
                let s = app.session.borrow();
                s.project()
                    .tracks
                    .iter()
                    .flat_map(|t| {
                        t.slots()
                            .into_iter()
                            .filter(|s| {
                                s.plugin.format == plugin.format && s.plugin.id == plugin.id
                            })
                            .map(|s| (s.state.clone(), s.parameters.clone()))
                            .collect::<Vec<_>>()
                    })
                    .nth(n)
            };
            if let Some((state, parameters)) = taken {
                edit_step(&u, index, |st| {
                    if let BatchStep::Plugin(p) = st {
                        p.state = state;
                        p.parameters = parameters;
                    }
                });
                show_steps(&app, &u);
            }
        });
        into.insert(&labelled("Take Settings From", &d), -1);
    }
    let params = gtk::Button::with_label("Parameters…");
    {
        let (app, u) = (Rc::clone(app), Rc::clone(ui));
        params.connect_clicked(move |_| parameters_window(&app, &u, index));
    }
    into.insert(&params, -1);
    let reset = gtk::Button::with_label("Defaults");
    reset.set_tooltip_text(Some("Forget the state and every value set"));
    {
        let (app, u) = (Rc::clone(app), Rc::clone(ui));
        reset.connect_clicked(move |_| {
            edit_step(&u, index, |st| {
                if let BatchStep::Plugin(p) = st {
                    p.state = None;
                    p.parameters.clear();
                }
            });
            show_steps(&app, &u);
        });
    }
    into.insert(&reset, -1);
    let tail = step_spin(
        ui,
        index,
        (0.0, 60.0, 0.5, 1),
        f64::from(p.tail_seconds),
        |st, v| {
            if let BatchStep::Plugin(p) = st {
                p.tail_seconds = v as f32;
            }
        },
    );
    tail.set_tooltip_text(Some(
        "Seconds kept after the audio for what the plugin rings on (a reverb's tail), \
         cut where it falls silent",
    ));
    into.insert(&labelled("Tail (s)", &tail), -1);
}

fn unit_text(u: ParameterUnit) -> &'static str {
    match u {
        ParameterUnit::None => "",
        ParameterUnit::Decibels => "dB",
        ParameterUnit::Milliseconds => "ms",
        ParameterUnit::Hertz => "Hz",
        ParameterUnit::Percent => "%",
        ParameterUnit::Samples => "samples",
    }
}

/// Shown value for a parameter's plain value (percentages × 100).
fn shown(p: &ParameterInfo, v: f64) -> f64 {
    if p.unit == ParameterUnit::Percent {
        v * 100.0
    } else {
        v
    }
}

/// A plugin step's every parameter, set by number.
fn parameters_window(app: &Rc<AppState>, ui: &Rc<Ui>, index: usize) {
    let Some(BatchStep::Plugin(step)) = ui.model.borrow().chain.steps.get(index).cloned() else {
        return;
    };
    let infos = app.session.borrow_mut().clip_fx_parameters(&step.plugin);
    let w = gtk::Window::builder()
        .title(format!("{} — Batch Step {}", step.plugin.name, index + 1))
        .transient_for(&ui.win)
        .default_width(520)
        .default_height(560)
        .build();
    let grid = gtk::Grid::new();
    grid.set_row_spacing(4);
    grid.set_column_spacing(10);
    margins(&grid, 12);
    if step.state.is_some() {
        let l = gtk::Label::new(Some("Values not set here are the plugin's saved state's."));
        l.add_css_class("dim-label");
        l.set_xalign(0.0);
        grid.attach(&l, 0, 0, 4, 1);
    }
    let first_row = i32::from(step.state.is_some());
    if infos.is_empty() {
        let l = gtk::Label::new(Some(
            "The plugin lists no parameters (or could not be loaded).",
        ));
        l.set_xalign(0.0);
        grid.attach(&l, 0, first_row, 4, 1);
    }
    for (row, p) in infos.iter().enumerate() {
        let y = row as i32 + first_row;
        let name = gtk::Label::new(Some(&p.name));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        grid.attach(&name, 0, y, 1, 1);
        let (lo, hi) = (shown(p, p.min), shown(p, p.max));
        let digits = if p.stepped {
            0
        } else if hi - lo <= 2.0 {
            3
        } else if hi - lo <= 100.0 {
            2
        } else {
            1
        };
        let step_size = if p.stepped {
            1.0
        } else {
            ((hi - lo) / 200.0).max(1e-3)
        };
        let value = step.parameter(p.id).unwrap_or(p.default);
        let s = spin(lo, hi.max(lo + 1e-9), step_size, digits, shown(p, value));
        let explicit = step.parameter(p.id).is_some();
        if !explicit {
            s.add_css_class("dim-label");
        }
        grid.attach(&s, 1, y, 1, 1);
        grid.attach(&gtk::Label::new(Some(unit_text(p.unit))), 2, y, 1, 1);
        let reset = gtk::Button::from_icon_name("edit-undo-symbolic");
        reset.add_css_class("flat");
        reset.set_tooltip_text(Some("Back to the plugin's own value"));
        grid.attach(&reset, 3, y, 1, 1);
        {
            let (u, p, s2) = (Rc::clone(ui), p.clone(), s.clone());
            s.connect_value_changed(move |s| {
                let v = if p.unit == ParameterUnit::Percent {
                    s.value() / 100.0
                } else {
                    s.value()
                };
                let v = p.clamp(v);
                edit_step(&u, index, |st| {
                    if let BatchStep::Plugin(step) = st {
                        step.set_parameter(p.id, v);
                    }
                });
                s2.remove_css_class("dim-label");
            });
        }
        {
            let (u, p, s) = (Rc::clone(ui), p.clone(), s.clone());
            reset.connect_clicked(move |_| {
                edit_step(&u, index, |st| {
                    if let BatchStep::Plugin(step) = st {
                        step.parameters.retain(|x| x.id != p.id);
                    }
                });
                s.set_value(shown(&p, p.default));
                // Setting the value above made it explicit again.
                edit_step(&u, index, |st| {
                    if let BatchStep::Plugin(step) = st {
                        step.parameters.retain(|x| x.id != p.id);
                    }
                });
                s.add_css_class("dim-label");
            });
        }
    }
    let scroll = gtk::ScrolledWindow::builder()
        .child(&grid)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    w.set_child(Some(&scroll));
    ui.parameter_windows.borrow_mut().push(w.downgrade());
    w.present();
}

/// Start the batch.
fn run(ui: &Rc<Ui>) {
    if ui.run.borrow().as_ref().is_some_and(|r| !r.is_finished()) {
        return;
    }
    let m = ui.model.borrow().clone();
    let Some(folder) = output_folder(&m) else {
        say(ui, "Add the files to process.", true);
        return;
    };
    let request = BatchRequest {
        files: m.files.clone(),
        chain: m.chain.clone(),
        folder,
        existing: m.existing,
        workers: m.workers,
        preset: m.preset.clone(),
        stamp: local_stamp(),
    };
    match batch::start(request) {
        Ok(r) => {
            *ui.running.borrow_mut() = m.files;
            *ui.run.borrow_mut() = Some(r);
            show_files(ui);
            say(ui, "Processing…", false);
        }
        Err(e) => say(ui, &e.to_string(), true),
    }
}

/// Now in local time (names' `{date}` and `{time}`).
fn local_stamp() -> Stamp {
    glib::DateTime::now_local()
        .ok()
        .and_then(|now| {
            Some(Stamp {
                date: now.format("%Y-%m-%d").ok()?.to_string(),
                time: now.format("%H-%M-%S").ok()?.to_string(),
            })
        })
        .unwrap_or_else(Stamp::utc_now)
}

/// Show the running batch's state (from the window's timer).
fn poll(ui: &Rc<Ui>) {
    let status = match ui.run.borrow().as_ref() {
        Some(r) => r.status(),
        None => return,
    };
    {
        let rows = ui.file_rows.borrow();
        for (row, file) in rows.iter().zip(&status.files) {
            let working = matches!(file.state, FileState::Working { .. });
            row.progress.set_visible(working);
            row.progress.set_fraction(f64::from(file.state.fraction()));
            let text = match &file.state {
                FileState::Waiting => String::new(),
                FileState::Done(r) if !r.notes.is_empty() => {
                    format!("{}\n{}", r.summary(), r.notes.join("; "))
                }
                other => other.text(),
            };
            row.status.set_visible(!text.is_empty());
            row.status.set_text(&text);
            if matches!(file.state, FileState::Failed(_)) {
                row.status.add_css_class("error");
            } else {
                row.status.remove_css_class("error");
            }
        }
    }
    ui.progress.set_fraction(status.fraction());
    ui.progress
        .set_text(Some(&format!("{:.0} %", status.fraction() * 100.0)));
    if status.finished {
        ui.run.borrow_mut().take();
        ui.running.borrow_mut().clear();
        let failed = status.failed() > 0;
        show_state(ui);
        say(ui, &format!("{}.", status.summary()), failed);
    } else {
        say(ui, &format!("Processing: {}.", status.summary()), false);
    }
    ui.run_button
        .set_sensitive(status.finished && ui.model.borrow().chain.check().is_ok());
    ui.cancel_button.set_sensitive(!status.finished);
}

/// Development aid: `batch:<preset>=<file>[|<file>…]` runs the preset on
/// the files into a "Batch" folder beside the first one; how each file
/// went is logged.
pub(crate) fn dev_batch(_app: &Rc<AppState>, arg: &str) {
    let (name, files, folder) = match dev_request(arg) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("batch: {e}");
            return;
        }
    };
    let Some(preset) = batch::preset_named(&name) else {
        let known: Vec<String> = batch::presets().into_iter().map(|p| p.name).collect();
        tracing::warn!(
            "batch: no preset '{name}' (there are: {})",
            known.join(", ")
        );
        return;
    };
    let mut request = BatchRequest::new(files, preset.chain, folder);
    request.preset = preset.name.clone();
    request.stamp = local_stamp();
    let run = match batch::start(request) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("batch: {e}");
            return;
        }
    };
    tracing::info!("batch: '{}' into {}", preset.name, run.folder().display());
    let mut run = Some(run);
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let finished = run.as_ref().is_none_or(BatchRun::is_finished);
        if !finished {
            return glib::ControlFlow::Continue;
        }
        if let Some(r) = run.take() {
            let status = r.wait();
            for f in &status.files {
                tracing::info!("batch: {} — {}", f.input.display(), f.state.text());
            }
            tracing::info!("batch: {}", status.summary());
        }
        glib::ControlFlow::Break
    });
}

/// `<preset>=<file>[|<file>…]`: the preset's name, the files and the
/// "Batch" folder beside the first.
fn dev_request(arg: &str) -> Result<(String, Vec<PathBuf>, PathBuf), String> {
    let (name, files) = arg
        .split_once('=')
        .ok_or_else(|| format!("expected <preset>=<file>[|<file>…], got '{arg}'"))?;
    let files: Vec<PathBuf> = files
        .split('|')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(PathBuf::from)
        .collect();
    let folder = files
        .first()
        .and_then(|f| f.parent())
        .map(|p| p.join("Batch"))
        .ok_or_else(|| "no files".to_string())?;
    Ok((name.trim().to_string(), files, folder))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_development_aid_reads_its_preset_and_files() {
        let (name, files, folder) = dev_request("CD 44.1/16=/a/x.wav|/b/y.flac").unwrap();
        assert_eq!(name, "CD 44.1/16");
        assert_eq!(
            files,
            [PathBuf::from("/a/x.wav"), PathBuf::from("/b/y.flac")]
        );
        assert_eq!(folder, PathBuf::from("/a/Batch"));
        assert!(dev_request("no files").is_err());
        assert!(dev_request("Preset=").is_err());
    }

    #[test]
    fn the_output_folder_is_beside_the_first_file_unless_chosen() {
        let mut k = Kept::default();
        assert_eq!(output_folder(&k), None);
        k.files = vec![PathBuf::from("/takes/a.wav")];
        assert_eq!(output_folder(&k), Some(PathBuf::from("/takes/Batch")));
        k.folder = Some(PathBuf::from("/out"));
        assert_eq!(output_folder(&k), Some(PathBuf::from("/out")));
    }
}
