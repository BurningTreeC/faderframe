//! Native file dialogs, confirmations and the about dialog.

use crate::state::AppState;
use faderframe_project::file::FILE_EXTENSION;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::rc::Rc;

fn project_filters() -> gio::ListStore {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("FaderFrame projects"));
    filter.add_pattern(&format!("*.{FILE_EXTENSION}"));
    let all = gtk::FileFilter::new();
    all.set_name(Some("All files"));
    all.add_pattern("*");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&filter);
    store.append(&all);
    store
}

pub fn open_project(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Open Project")
        .modal(true)
        .filters(&project_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        if let (Ok(file), Some(app)) = (res, weak.upgrade())
            && let Some(path) = file.path()
        {
            app.with_session(|s| s.open(&path));
        }
    });
}

fn audio_filters() -> gio::ListStore {
    let audio = gtk::FileFilter::new();
    audio.set_name(Some("Audio files"));
    for ext in faderframe_audio_files::decode::SUPPORTED_EXTENSIONS {
        audio.add_suffix(ext);
    }
    let all = gtk::FileFilter::new();
    all.set_name(Some("All files"));
    all.add_pattern("*");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&audio);
    store.append(&all);
    store
}

fn midi_filters() -> gio::ListStore {
    let midi = gtk::FileFilter::new();
    midi.set_name(Some("MIDI files"));
    for ext in ["mid", "midi", "smf", "kar"] {
        midi.add_suffix(ext);
    }
    let all = gtk::FileFilter::new();
    all.set_name(Some("All files"));
    all.add_pattern("*");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&midi);
    store.append(&all);
    store
}

/// File → Import MIDI File…: new instrument tracks at the playhead; into
/// a project without clips at bar 1 with the file's tempo and meter.
pub fn import_midi(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Import MIDI File")
        .modal(true)
        .filters(&midi_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(path) = file.path() else { return };
        let (empty, playhead) = {
            let s = app.session.borrow();
            (s.project().clips.is_empty(), s.playhead())
        };
        let at = if empty {
            faderframe_timeline::MusicalTime::ZERO
        } else {
            playhead
        };
        app.dispatch(faderframe_session::Action::ImportMidiFile {
            path,
            at,
            tempo: empty,
        });
    });
}

/// File → Export MIDI File…: every instrument/MIDI track (or the selected
/// clips).
pub fn export_midi(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let name = format!("{}.mid", app.session.borrow().project().name);
    let dialog = gtk::FileDialog::builder()
        .title("Export MIDI File")
        .modal(true)
        .initial_name(name)
        .filters(&midi_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(path) = file.path() else { return };
        export_midi_to(&app, &path);
    });
}

/// Write the MIDI file (selected MIDI clips only when some are selected).
pub fn export_midi_to(app: &Rc<AppState>, path: &std::path::Path) {
    let clips: Vec<faderframe_core::ClipId> = {
        let s = app.session.borrow();
        s.selection
            .clips
            .iter()
            .copied()
            .filter(|c| s.project().clip(*c).is_some_and(|c| c.as_midi().is_some()))
            .collect()
    };
    let only = (!clips.is_empty()).then_some(clips.as_slice());
    let result = app.session.borrow().export_midi_file(path, only);
    match result {
        Ok(n) => app.session.borrow_mut().notify(
            faderframe_session::NoticeLevel::Info,
            format!(
                "exported {n} track{} to {}",
                if n == 1 { "" } else { "s" },
                path.display()
            ),
        ),
        Err(e) => app.report(e, true),
    }
    app.after_change();
}

/// File → Import Audio…: onto the selected audio track (others on new
/// tracks) at the playhead.
pub fn import_audio(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Import Audio")
        .accept_label("Import")
        .modal(true)
        .filters(&audio_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.open_multiple(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(list), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let files: Vec<std::path::PathBuf> = (0..list.n_items())
            .filter_map(|i| list.item(i).and_downcast::<gio::File>())
            .filter_map(|f| f.path())
            .collect();
        let action = {
            let s = app.session.borrow();
            let p = s.project();
            let track = s.selection.tracks.iter().copied().find(|t| {
                p.track(*t)
                    .is_some_and(|t| t.kind == faderframe_project::TrackKind::Audio)
            });
            faderframe_session::Action::ImportFiles {
                files,
                track,
                at: s.playhead(),
            }
        };
        app.dispatch(action);
    });
}

fn preset_filters() -> gio::ListStore {
    let f = gtk::FileFilter::new();
    f.set_name(Some("FaderFrame track presets"));
    f.add_suffix(faderframe_project::preset::PRESET_EXTENSION);
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&f);
    store
}

/// Pick a track preset: add it as a new track, or (`apply`) apply it to
/// the selected track.
pub fn track_preset(app: &Rc<AppState>, apply: bool) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let target = app.session.borrow().selection.tracks.iter().next().copied();
    if apply && target.is_none() {
        app.report(
            faderframe_session::SessionError::Other("select the track to apply a preset to".into()),
            false,
        );
        return;
    }
    let dir = app.session.borrow().track_preset_dir().to_path_buf();
    let _ = std::fs::create_dir_all(&dir);
    let dialog = gtk::FileDialog::builder()
        .title(if apply {
            "Apply Track Preset"
        } else {
            "New Track from Preset"
        })
        .modal(true)
        .initial_folder(&gio::File::for_path(&dir))
        .filters(&preset_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(path) = file.path() else { return };
        app.dispatch(match (apply, target) {
            (true, Some(track)) => faderframe_session::Action::ApplyTrackPreset { track, path },
            _ => faderframe_session::Action::AddTrackFromPreset { path },
        });
    });
}

/// Save the selected track as a preset file anywhere.
pub fn export_track_preset(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let target = {
        let s = app.session.borrow();
        s.selection
            .tracks
            .iter()
            .next()
            .and_then(|t| s.project().track(*t))
            .map(|t| (t.id, t.name.clone()))
    };
    let Some((track, name)) = target else {
        app.report(
            faderframe_session::SessionError::Other("select a track to export".into()),
            false,
        );
        return;
    };
    let dir = app.session.borrow().track_preset_dir().to_path_buf();
    let _ = std::fs::create_dir_all(&dir);
    let dialog = gtk::FileDialog::builder()
        .title("Export Track Preset")
        .modal(true)
        .initial_folder(&gio::File::for_path(&dir))
        .initial_name(format!(
            "{name}.{}",
            faderframe_project::preset::PRESET_EXTENSION
        ))
        .filters(&preset_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(mut path) = file.path() else { return };
        if path.extension().is_none() {
            path.set_extension(faderframe_project::preset::PRESET_EXTENSION);
        }
        if app
            .with_session(|s| {
                s.export_track_preset(track, &path)?;
                s.rescan_track_presets();
                Ok(())
            })
            .is_some()
        {
            app.session.borrow_mut().notify(
                faderframe_session::NoticeLevel::Info,
                format!("exported {}", path.display()),
            );
        }
    });
}

/// Save to the current file, or ask for one.
pub fn save(app: &Rc<AppState>) {
    let has_path = app.session.borrow().path().is_some();
    if has_path {
        app.with_session(|s| s.save());
    } else {
        save_as(app, None);
    }
}

/// Continuation run after a successful "save as".
pub type AfterSave = Box<dyn FnOnce(&Rc<AppState>)>;

/// Ask for a file name and save; run `then` after a successful save.
pub fn save_as(app: &Rc<AppState>, then: Option<AfterSave>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let name = format!("{}.{FILE_EXTENSION}", app.session.borrow().project().name);
    let dialog = gtk::FileDialog::builder()
        .title("Save Project")
        .modal(true)
        .initial_name(name)
        .filters(&project_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(path) = file.path() else { return };
        if app.with_session(|s| s.save_as(&path)).is_some()
            && let Some(then) = then
        {
            then(&app);
        }
    });
}

/// Run `next` immediately, or after asking what to do with unsaved changes.
pub fn confirm_discard(app: &Rc<AppState>, next: impl Fn(&Rc<AppState>) + 'static) {
    if !app.session.borrow().is_dirty() {
        next(app);
        return;
    }
    let Some(win) = app.window.borrow().clone() else {
        next(app);
        return;
    };
    let dialog = gtk::AlertDialog::builder()
        .message("Save changes to the current project?")
        .detail("Unsaved changes will be lost otherwise.")
        .buttons(["Cancel", "Discard", "Save"])
        .cancel_button(0)
        .default_button(2)
        .modal(true)
        .build();
    let weak = Rc::downgrade(app);
    let next = Rc::new(next);
    dialog.choose(Some(&win), gio::Cancellable::NONE, move |res| {
        let Some(app) = weak.upgrade() else { return };
        match res {
            Ok(1) => next(&app),
            Ok(2) => {
                let has_path = app.session.borrow().path().is_some();
                if has_path {
                    if app.with_session(|s| s.save()).is_some() {
                        next(&app);
                    }
                } else {
                    let next = Rc::clone(&next);
                    save_as(&app, Some(Box::new(move |a: &Rc<AppState>| next(a))));
                }
            }
            _ => {}
        }
    });
}

/// Ask for a name and save a plugin's settings as a preset.
pub fn save_preset(app: &Rc<AppState>, plugin: faderframe_core::PluginInstanceId) {
    let plugin_name = app
        .session
        .borrow()
        .plugin_owner(plugin)
        .map(|(_, s)| s.plugin.name.clone())
        .unwrap_or_default();
    name_prompt(
        app,
        &format!("Save Preset — {plugin_name}"),
        "Save the current settings as a preset:",
        "",
        "Save",
        move |name| faderframe_session::Action::SavePluginPreset { plugin, name },
    );
}

pub fn rename_group(app: &Rc<AppState>, group: faderframe_core::GroupId) {
    let Some(name) = app
        .session
        .borrow()
        .project()
        .group(group)
        .map(|g| g.name.clone())
    else {
        return;
    };
    name_prompt(
        app,
        "Rename Group",
        "Group name:",
        &name,
        "Rename",
        move |name| faderframe_session::Action::RenameGroup { group, name },
    );
}

/// A small modal window asking for a name.
fn name_prompt(
    app: &Rc<AppState>,
    title: &str,
    prompt: &str,
    initial: &str,
    ok: &str,
    action: impl Fn(String) -> faderframe_session::Action + 'static,
) {
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let win = gtk::Window::builder()
        .title(title)
        .modal(true)
        .transient_for(&main)
        .resizable(false)
        .default_width(360)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let entry = gtk::Entry::new();
    entry.set_text(initial);
    entry.set_activates_default(true);
    let label = gtk::Label::new(Some(prompt));
    label.set_halign(gtk::Align::Start);
    body.append(&label);
    body.append(&entry);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label(ok);
    confirm.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&confirm);
    body.append(&buttons);
    win.set_child(Some(&body));
    win.set_default_widget(Some(&confirm));
    let w = win.clone();
    cancel.connect_clicked(move |_| w.close());
    let weak = Rc::downgrade(app);
    let w = win.clone();
    confirm.connect_clicked(move |_| {
        let name = entry.text().trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Some(app) = weak.upgrade() {
            app.dispatch(action(name));
        }
        w.close();
    });
    win.present();
}

/// The release's details (`None`: title, credits, UPC/EAN) or a song's
/// (title, ISRC, credits) for the cue sheet, CD-Text and the CD master's
/// codes. Codes are checked before anything is changed.
pub fn album_details(app: &Rc<AppState>, song: Option<faderframe_core::SongId>) {
    use faderframe_project::album::Credits;
    use faderframe_session::album::{AlbumAction, normalize_isrc, normalize_upc};
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let (title, credits, code, heading) = {
        let s = app.session.borrow();
        let album = &s.project().album;
        match song.and_then(|id| album.song(id)) {
            Some(x) => (
                x.title.clone(),
                x.credits.clone(),
                x.isrc.clone(),
                format!("Song — {}", x.title),
            ),
            None if song.is_some() => return,
            None => (
                album.info.title.clone(),
                album.info.credits.clone(),
                album.info.upc.clone(),
                "Release".to_string(),
            ),
        }
    };
    let project_name = app.session.borrow().project().name.clone();
    let win = gtk::Window::builder()
        .application(&app.app)
        .title(format!("{heading} — Details"))
        .modal(true)
        .transient_for(&main)
        .resizable(false)
        .default_width(440)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(10);
    let code_label = if song.is_some() { "ISRC" } else { "UPC / EAN" };
    let fields = [
        ("Title", title.as_str()),
        (code_label, code.as_str()),
        ("Performer", credits.performer.as_str()),
        ("Songwriter", credits.songwriter.as_str()),
        ("Composer", credits.composer.as_str()),
        ("Arranger", credits.arranger.as_str()),
        ("Message", credits.message.as_str()),
    ];
    let mut entries = Vec::new();
    for (row, (label, value)) in fields.iter().enumerate() {
        let l = gtk::Label::new(Some(label));
        l.set_halign(gtk::Align::End);
        let e = gtk::Entry::new();
        e.set_text(value);
        e.set_hexpand(true);
        e.set_activates_default(true);
        grid.attach(&l, 0, row as i32, 1, 1);
        grid.attach(&e, 1, row as i32, 1, 1);
        entries.push(e);
    }
    if song.is_none() {
        entries[0].set_placeholder_text(Some(&project_name));
        entries[1].set_placeholder_text(Some("12 or 13 digits"));
    } else {
        entries[1].set_placeholder_text(Some("CC-XXX-YY-NNNNN"));
    }
    let note = gtk::Label::new(Some(
        "Written to the cue sheet and, as CD-Text and PQ codes, to the CD master.",
    ));
    note.add_css_class("dim-label");
    note.set_wrap(true);
    note.set_xalign(0.0);
    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_xalign(0.0);
    body.append(&grid);
    body.append(&note);
    body.append(&error);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label("Save");
    confirm.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&confirm);
    body.append(&buttons);
    win.set_child(Some(&body));
    win.set_default_widget(Some(&confirm));
    let w = win.clone();
    cancel.connect_clicked(move |_| w.close());
    let weak = Rc::downgrade(app);
    let w = win.clone();
    confirm.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let text: Vec<String> = entries
            .iter()
            .map(|e| e.text().trim().to_string())
            .collect();
        let code = match (&text[1], song) {
            (c, _) if c.is_empty() => Ok(String::new()),
            (c, Some(_)) => normalize_isrc(c),
            (c, None) => normalize_upc(c),
        };
        let code = match code {
            Ok(c) => c,
            Err(e) => {
                error.set_text(&e.to_string());
                entries[1].grab_focus();
                return;
            }
        };
        let credits = Credits {
            performer: text[2].clone(),
            songwriter: text[3].clone(),
            composer: text[4].clone(),
            arranger: text[5].clone(),
            message: text[6].clone(),
        };
        let action = {
            let s = app.session.borrow();
            let album = &s.project().album;
            match song {
                Some(id) => {
                    let Some(mut x) = album.song(id).cloned() else {
                        return;
                    };
                    if !text[0].is_empty() {
                        x.title = text[0].clone();
                    }
                    x.isrc = code;
                    x.credits = credits;
                    AlbumAction::Update(x)
                }
                None => {
                    let mut info = album.info.clone();
                    info.title = text[0].clone();
                    info.upc = code;
                    info.credits = credits;
                    AlbumAction::Info(info)
                }
            }
        };
        app.dispatch(faderframe_session::Action::Album(action));
        w.close();
    });
    win.present();
}

/// GTK's colour chooser for a track (the selected tracks follow, as with
/// any edit of one of them) or a section.
pub fn pick_color(app: &Rc<AppState>, target: faderframe_session::ColorTarget) {
    use faderframe_project::{Command, TrackColor};
    use faderframe_session::ColorTarget;
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let (title, initial) = {
        let s = app.session.borrow();
        let p = s.project();
        match target {
            ColorTarget::Track(t) => match p.track(t) {
                Some(t) => (format!("Colour of {}", t.name), t.color),
                None => return,
            },
            ColorTarget::Section(id) => match p.sections.iter().find(|x| x.id == id) {
                Some(x) => (format!("Colour of {}", x.name), x.color),
                None => return,
            },
        }
    };
    let dialog = gtk::ColorDialog::new();
    dialog.set_title(&title);
    dialog.set_modal(true);
    dialog.set_with_alpha(false);
    let rgba = gdk::RGBA::new(
        initial.r as f32 / 255.0,
        initial.g as f32 / 255.0,
        initial.b as f32 / 255.0,
        1.0,
    );
    let weak = Rc::downgrade(app);
    dialog.choose_rgba(
        Some(&main),
        Some(&rgba),
        gio::Cancellable::NONE,
        move |res| {
            let (Ok(c), Some(app)) = (res, weak.upgrade()) else {
                return;
            };
            let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            let color = TrackColor::rgb(to8(c.red()), to8(c.green()), to8(c.blue()));
            let action = match target {
                ColorTarget::Track(track) => {
                    faderframe_session::Action::Edit(Command::SetTrackColor { track, color })
                }
                ColorTarget::Section(id) => {
                    let section = app
                        .session
                        .borrow()
                        .project()
                        .sections
                        .iter()
                        .find(|x| x.id == id)
                        .cloned();
                    let Some(mut section) = section else { return };
                    section.color = color;
                    faderframe_session::Action::Edit(Command::UpdateSection { section })
                }
            };
            app.dispatch(action);
        },
    );
}

pub fn about(app: &Rc<AppState>) {
    let dialog = gtk::AboutDialog::builder()
        .program_name("FaderFrame")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("A multitrack recording, editing and mixing environment.\nLinux first · Wayland native · GTK 4")
        .website("https://github.com/BurningTreeC/faderframe")
        .website_label("github.com/BurningTreeC/faderframe")
        .license_type(gtk::License::MitX11)
        .authors(["BurningTreeC and the FaderFrame contributors"])
        .modal(true)
        .build();
    if let Some(win) = app.window.borrow().as_ref() {
        dialog.set_transient_for(Some(win));
    }
    dialog.present();
}

/// Ask before closing the main window with unsaved changes.
pub fn install_close_guard(app: &Rc<AppState>, window: &gtk::ApplicationWindow) {
    let weak = Rc::downgrade(app);
    let confirmed = Rc::new(std::cell::Cell::new(false));
    window.connect_close_request(move |w| {
        let Some(app) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if confirmed.get() || !app.session.borrow().is_dirty() {
            app.app.quit();
            return glib::Propagation::Proceed;
        }
        let confirmed = Rc::clone(&confirmed);
        let w = w.clone();
        confirm_discard(&app, move |a| {
            confirmed.set(true);
            a.session.borrow_mut().stop_audio();
            w.close();
        });
        glib::Propagation::Stop
    });
}

fn sysex_filters() -> gio::ListStore {
    let syx = gtk::FileFilter::new();
    syx.set_name(Some("SysEx files (.syx)"));
    syx.add_suffix("syx");
    let all = gtk::FileFilter::new();
    all.set_name(Some("All files"));
    all.add_pattern("*");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&syx);
    store.append(&all);
    store
}

/// Read a `.syx` file into complete messages.
fn read_sysex(path: &std::path::Path) -> Result<Vec<Vec<u8>>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let messages = faderframe_project::SysexEvent::split_messages(&bytes);
    if messages.is_empty() {
        return Err(format!("{}: no SysEx messages", path.display()));
    }
    Ok(messages)
}

/// Add the messages of a `.syx` file to `clip` at `at`.
pub fn import_sysex(
    app: &Rc<AppState>,
    clip: faderframe_core::ClipId,
    at: faderframe_timeline::MusicalTime,
) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Import SysEx")
        .accept_label("Import")
        .modal(true)
        .filters(&sysex_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        if let (Ok(file), Some(app)) = (res, weak.upgrade())
            && let Some(path) = file.path()
        {
            match read_sysex(&path) {
                Ok(messages) => {
                    app.dispatch(faderframe_session::Action::AddSysex { clip, at, messages })
                }
                Err(e) => app
                    .session
                    .borrow_mut()
                    .notify(faderframe_session::NoticeLevel::Warning, e),
            }
        }
    });
}

/// Send the messages of a `.syx` file to MIDI output `output` (port key).
pub fn send_sysex_file(app: &Rc<AppState>, output: String) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Send SysEx File")
        .accept_label("Send")
        .modal(true)
        .filters(&sysex_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        if let (Ok(file), Some(app)) = (res, weak.upgrade())
            && let Some(path) = file.path()
        {
            match read_sysex(&path) {
                Ok(messages) => {
                    app.dispatch(faderframe_session::Action::SendSysex { output, messages })
                }
                Err(e) => app
                    .session
                    .borrow_mut()
                    .notify(faderframe_session::NoticeLevel::Warning, e),
            }
        }
    });
}
