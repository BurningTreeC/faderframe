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

/// File → Import ADM BWF…: an object-based master's bed and objects as
/// tracks (the master takes a format that holds them).
pub fn import_adm(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let f = gtk::FileFilter::new();
    f.set_name(Some("ADM BWF masters (.wav)"));
    f.add_suffix("wav");
    f.add_suffix("WAV");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&f);
    let dialog = gtk::FileDialog::builder()
        .title("Import ADM BWF Master")
        .accept_label("Import")
        .modal(true)
        .filters(&filters)
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        if let Some(path) = file.path() {
            app.dispatch(faderframe_session::Action::ImportAdm(path));
        }
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

/// Ask for a version's name, then keep the project as it is under it.
pub fn save_version(app: &Rc<AppState>) {
    let next = app
        .session
        .borrow()
        .versions()
        .last()
        .map_or(1, |v| v.number + 1);
    name_prompt(
        app,
        "Save Version",
        "Keep the project as it is now as a version named:",
        &format!("Version {next}"),
        "Save Version",
        |name| faderframe_session::Action::SaveVersion { name },
    );
}

/// How long ago `t` was, in words.
fn ago(t: Option<std::time::SystemTime>) -> String {
    let Some(secs) = t.and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()) else {
        return String::new();
    };
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// The project's versions: save another, compare one with the project as
/// it is now, go back to one (the present is kept as a version first).
pub fn versions(app: &Rc<AppState>) {
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let win = gtk::Window::builder()
        .title("Versions")
        .application(&app.app)
        .transient_for(&main)
        .default_width(620)
        .default_height(520)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let intro = gtk::Label::new(Some(
        "Snapshots of this project, kept in its Versions folder. Restoring one keeps the project as it is as a version first.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.set_hexpand(true);
    intro.add_css_class("dim-label");
    let save = gtk::Button::with_label("Save Version…");
    save.add_css_class("suggested-action");
    save.set_valign(gtk::Align::Center);
    top.append(&intro);
    top.append(&save);
    body.append(&top);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    body.append(&scroll);
    let details = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(8)
        .top_margin(6)
        .build();
    details
        .buffer()
        .set_text("Compare a version to see what the project has changed since.");
    let details_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(150)
        .child(&details)
        .build();
    body.append(&details_scroll);
    win.set_child(Some(&body));

    // The rows, rebuilt when the versions change.
    let shown = Rc::new(std::cell::Cell::new(usize::MAX));
    let fill: Rc<dyn Fn()> = {
        let (weak, list, details, shown) = (
            Rc::downgrade(app),
            list.clone(),
            details.clone(),
            Rc::clone(&shown),
        );
        Rc::new(move || {
            let Some(app) = weak.upgrade() else { return };
            let versions = app.session.borrow().versions();
            shown.set(versions.len());
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            if versions.is_empty() {
                let none = gtk::Label::new(Some(
                    "No versions yet: Save Version keeps the project as it is now.",
                ));
                none.set_margin_top(18);
                none.set_margin_bottom(18);
                none.add_css_class("dim-label");
                list.append(&none);
            }
            for v in versions.into_iter().rev() {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                row.set_margin_top(6);
                row.set_margin_bottom(6);
                row.set_margin_start(10);
                row.set_margin_end(10);
                let number = gtk::Label::new(Some(&v.number.to_string()));
                number.add_css_class("dim-label");
                number.set_width_chars(3);
                let name = gtk::Label::new(Some(&v.name));
                name.set_xalign(0.0);
                name.set_hexpand(true);
                name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                name.add_css_class("heading");
                let when = gtk::Label::new(Some(&ago(v.saved)));
                when.add_css_class("dim-label");
                let compare = gtk::Button::with_label("Compare");
                let restore = gtk::Button::with_label("Restore");
                for w in [
                    &number.clone().upcast::<gtk::Widget>(),
                    &name.upcast(),
                    &when.upcast(),
                    &compare.clone().upcast(),
                    &restore.clone().upcast(),
                ] {
                    row.append(w);
                }
                let (weak, details, path) = (Rc::downgrade(&app), details.clone(), v.path.clone());
                let (num, vname) = (v.number, v.name.clone());
                compare.connect_clicked(move |_| {
                    let Some(app) = weak.upgrade() else { return };
                    let text = match app.session.borrow_mut().compare_version(&path) {
                        Ok(lines) if lines.is_empty() => {
                            format!("The project is as it was in version {num} ‘{vname}’.")
                        }
                        Ok(lines) => {
                            format!("Since version {num} ‘{vname}’:\n\n• {}", lines.join("\n• "))
                        }
                        Err(e) => format!("Cannot read version {num}: {e}"),
                    };
                    details.buffer().set_text(&text);
                });
                let (weak, path) = (Rc::downgrade(&app), v.path.clone());
                restore.connect_clicked(move |_| {
                    if let Some(app) = weak.upgrade() {
                        app.dispatch(faderframe_session::Action::RestoreVersion(path.clone()));
                    }
                });
                list.append(&row);
            }
        })
    };
    fill();
    let weak = Rc::downgrade(app);
    save.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(faderframe_session::Action::PromptSaveVersion);
        }
    });
    // New versions (saved here or anywhere) show up.
    let weak = Rc::downgrade(app);
    let w = win.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(700), move || {
        let (Some(app), Some(_)) = (weak.upgrade(), w.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        let n = app.session.borrow().versions().len();
        if n != shown.get() {
            fill();
        }
        glib::ControlFlow::Continue
    });
    win.present();
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
    album_details_in(app, song, None);
}

/// The details form showing the text in `language` (`None`: the main one;
/// a translation into it is made when missing).
pub fn album_details_in(
    app: &Rc<AppState>,
    song: Option<faderframe_core::SongId>,
    language: Option<u8>,
) {
    use faderframe_project::album::{AlbumInfo, Credits, Song};
    use faderframe_session::album::{AlbumAction, Language, normalize_isrc, normalize_upc};
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    /// What the form edits: the release's information with its
    /// translations and (for a song's form) the song; the language shown
    /// (`None`: the main one).
    struct Edit {
        info: AlbumInfo,
        song: Option<Song>,
        current: Option<u8>,
    }
    let (info, song_now, heading) = {
        let s = app.session.borrow();
        let album = &s.project().album;
        match song.map(|id| album.song(id)) {
            Some(Some(x)) => (
                album.info.clone(),
                Some(x.clone()),
                format!("Song — {}", x.title),
            ),
            Some(None) => return,
            None => (album.info.clone(), None, "Release".to_string()),
        }
    };
    let project_name = app.session.borrow().project().name.clone();
    let mut info = info;
    let current = language.filter(|l| *l != info.language);
    if let Some(l) = current {
        info.translation_mut(l);
    }
    let edit = Rc::new(std::cell::RefCell::new(Edit {
        info,
        song: song_now,
        current,
    }));
    let win = gtk::Window::builder()
        .application(&app.app)
        .title(format!("{heading} — Details"))
        .modal(true)
        .transient_for(&main)
        .resizable(false)
        .default_width(460)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(10);
    let label = |text: &str, row: i32| {
        let l = gtk::Label::new(Some(text));
        l.set_halign(gtk::Align::End);
        grid.attach(&l, 0, row, 1, 1);
    };
    // The languages: shown, added, the main one.
    let shown = gtk::DropDown::from_strings(&[]);
    let remove = gtk::Button::with_label("Remove");
    let shown_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    shown.set_hexpand(true);
    shown_row.append(&shown);
    shown_row.append(&remove);
    label("Language", 0);
    grid.attach(&shown_row, 1, 0, 1, 1);
    let add = gtk::DropDown::from_strings(&[]);
    label("Add language", 1);
    grid.attach(&add, 1, 1, 1, 1);
    let main_language = gtk::DropDown::from_strings(&[]);
    if song.is_none() {
        label("Main language", 2);
        grid.attach(&main_language, 1, 2, 1, 1);
    }
    let code_label = if song.is_some() { "ISRC" } else { "UPC / EAN" };
    let names = [
        "Title",
        code_label,
        "Performer",
        "Songwriter",
        "Composer",
        "Arranger",
        "Message",
    ];
    let mut entries = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let row = 3 + i as i32;
        label(name, row);
        let e = gtk::Entry::new();
        e.set_hexpand(true);
        e.set_activates_default(true);
        grid.attach(&e, 1, row, 1, 1);
        entries.push(e);
    }
    let entries = Rc::new(entries);
    let note = gtk::Label::new(Some(
        "Written to the cue sheet and, as CD-Text and PQ codes, to the CD master. Text in \
         further languages becomes further CD-Text blocks (up to eight); a field left empty \
         there is the main language's.",
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

    // The fields of the language shown, and back.
    let credits_of = |c: &Credits| {
        [
            c.performer.clone(),
            c.songwriter.clone(),
            c.composer.clone(),
            c.arranger.clone(),
            c.message.clone(),
        ]
    };
    let set_credits = |c: &mut Credits, v: &[String]| {
        c.performer.clone_from(&v[0]);
        c.songwriter.clone_from(&v[1]);
        c.composer.clone_from(&v[2]);
        c.arranger.clone_from(&v[3]);
        c.message.clone_from(&v[4]);
    };
    // (title, code, credits) of the main text.
    let main_text = move |e: &Edit| -> (String, String, [String; 5]) {
        match &e.song {
            Some(x) => (x.title.clone(), x.isrc.clone(), credits_of(&x.credits)),
            None => (
                e.info.title.clone(),
                e.info.upc.clone(),
                credits_of(&e.info.credits),
            ),
        }
    };
    let load = {
        let entries = Rc::clone(&entries);
        let project_name = project_name.clone();
        move |e: &Edit| {
            let (title, code, credits) = main_text(e);
            let (own_title, own_credits) = match e.current {
                None => (title.clone(), credits.clone()),
                Some(l) => {
                    let tr = e.info.translation(l);
                    match &e.song {
                        Some(x) => {
                            let t = tr.and_then(|t| t.song(x.id));
                            (
                                t.map(|t| t.title.clone()).unwrap_or_default(),
                                t.map_or_else(Default::default, |t| credits_of(&t.credits)),
                            )
                        }
                        None => (
                            tr.map(|t| t.title.clone()).unwrap_or_default(),
                            tr.map_or_else(Default::default, |t| credits_of(&t.credits)),
                        ),
                    }
                }
            };
            entries[0].set_text(&own_title);
            entries[1].set_text(if e.current.is_none() { &code } else { "" });
            entries[1].set_sensitive(e.current.is_none());
            for (i, v) in own_credits.iter().enumerate() {
                entries[2 + i].set_text(v);
            }
            // A translation shows what an empty field will be.
            let hint = |main: &str, fallback: &str| {
                if main.is_empty() {
                    fallback.to_string()
                } else {
                    main.to_string()
                }
            };
            match e.current {
                None => {
                    if e.song.is_none() {
                        entries[0].set_placeholder_text(Some(&project_name));
                        entries[1].set_placeholder_text(Some("12 or 13 digits"));
                    } else {
                        entries[0].set_placeholder_text(None);
                        entries[1].set_placeholder_text(Some("CC-XXX-YY-NNNNN"));
                    }
                    for e in &entries[2..] {
                        e.set_placeholder_text(None);
                    }
                }
                Some(_) => {
                    entries[0].set_placeholder_text(Some(&hint(&title, &project_name)));
                    entries[1].set_placeholder_text(Some("only in the main language"));
                    for (i, v) in credits.iter().enumerate() {
                        entries[2 + i].set_placeholder_text(Some(v));
                    }
                }
            }
        }
    };
    let store = {
        let entries = Rc::clone(&entries);
        move |e: &mut Edit| {
            let v: Vec<String> = entries
                .iter()
                .map(|x| x.text().trim().to_string())
                .collect();
            match (e.current, &mut e.song) {
                (None, Some(x)) => {
                    if !v[0].is_empty() {
                        x.title.clone_from(&v[0]);
                    }
                    x.isrc.clone_from(&v[1]);
                    set_credits(&mut x.credits, &v[2..]);
                }
                (None, None) => {
                    e.info.title.clone_from(&v[0]);
                    e.info.upc.clone_from(&v[1]);
                    set_credits(&mut e.info.credits, &v[2..]);
                }
                (Some(l), Some(x)) => {
                    let t = e.info.translation_mut(l).song_mut(x.id);
                    t.title.clone_from(&v[0]);
                    set_credits(&mut t.credits, &v[2..]);
                }
                (Some(l), None) => {
                    let t = e.info.translation_mut(l);
                    t.title.clone_from(&v[0]);
                    set_credits(&mut t.credits, &v[2..]);
                }
            }
        }
    };
    let load = Rc::new(load);
    let store = Rc::new(store);
    // The dropdowns' lists from the languages in use.
    let busy = Rc::new(std::cell::Cell::new(false));
    let refill = {
        let (shown, add, main_language, remove) = (
            shown.clone(),
            add.clone(),
            main_language.clone(),
            remove.clone(),
        );
        let busy = Rc::clone(&busy);
        move |e: &Edit| {
            busy.set(true);
            let main = Language(e.info.language);
            let mut list = vec![format!("{} (main)", main.name())];
            list.extend(
                e.info
                    .translations
                    .iter()
                    .map(|t| Language(t.language).name().to_string()),
            );
            let refs: Vec<&str> = list.iter().map(String::as_str).collect();
            shown.set_model(Some(&gtk::StringList::new(&refs)));
            let at = e
                .current
                .and_then(|l| e.info.translations.iter().position(|t| t.language == l))
                .map_or(0, |i| i + 1);
            shown.set_selected(at as u32);
            remove.set_sensitive(e.current.is_some());
            let used: Vec<u8> = std::iter::once(e.info.language)
                .chain(e.info.translations.iter().map(|t| t.language))
                .collect();
            let mut free = vec!["—".to_string()];
            free.extend(
                Language::all()
                    .filter(|l| !used.contains(&l.0))
                    .map(|l| l.name().to_string()),
            );
            let refs: Vec<&str> = free.iter().map(String::as_str).collect();
            add.set_model(Some(&gtk::StringList::new(&refs)));
            add.set_selected(0);
            add.set_sensitive(e.info.translations.len() < 7);
            // The main language: any not translated into.
            let mains: Vec<Language> = Language::all()
                .filter(|l| l.0 == e.info.language || e.info.translation(l.0).is_none())
                .collect();
            let names: Vec<&str> = mains.iter().map(|l| l.name()).collect();
            main_language.set_model(Some(&gtk::StringList::new(&names)));
            main_language.set_selected(
                mains
                    .iter()
                    .position(|l| l.0 == e.info.language)
                    .unwrap_or(0) as u32,
            );
            busy.set(false);
        }
    };
    let refill = Rc::new(refill);
    refill(&edit.borrow());
    load(&edit.borrow());
    {
        let (edit, load, store, busy) = (
            Rc::clone(&edit),
            Rc::clone(&load),
            Rc::clone(&store),
            Rc::clone(&busy),
        );
        let refill = Rc::clone(&refill);
        shown.connect_selected_notify(move |d| {
            if busy.get() {
                return;
            }
            let mut e = edit.borrow_mut();
            store(&mut e);
            let i = d.selected() as usize;
            e.current = i
                .checked_sub(1)
                .and_then(|i| e.info.translations.get(i))
                .map(|t| t.language);
            refill(&e);
            load(&e);
        });
    }
    {
        let (edit, load, store, busy) = (
            Rc::clone(&edit),
            Rc::clone(&load),
            Rc::clone(&store),
            Rc::clone(&busy),
        );
        let refill = Rc::clone(&refill);
        add.connect_selected_notify(move |d| {
            if busy.get() || d.selected() == 0 {
                return;
            }
            let mut e = edit.borrow_mut();
            store(&mut e);
            let used: Vec<u8> = std::iter::once(e.info.language)
                .chain(e.info.translations.iter().map(|t| t.language))
                .collect();
            let Some(l) = Language::all()
                .filter(|l| !used.contains(&l.0))
                .nth(d.selected() as usize - 1)
            else {
                return;
            };
            e.info.translation_mut(l.0);
            e.current = Some(l.0);
            refill(&e);
            load(&e);
        });
    }
    {
        let (edit, load, busy) = (Rc::clone(&edit), Rc::clone(&load), Rc::clone(&busy));
        let refill = Rc::clone(&refill);
        remove.connect_clicked(move |_| {
            if busy.get() {
                return;
            }
            let mut e = edit.borrow_mut();
            if let Some(l) = e.current.take() {
                e.info.translations.retain(|t| t.language != l);
            }
            refill(&e);
            load(&e);
        });
    }
    {
        let (edit, store, busy) = (Rc::clone(&edit), Rc::clone(&store), Rc::clone(&busy));
        let refill = Rc::clone(&refill);
        main_language.connect_selected_notify(move |d| {
            if busy.get() {
                return;
            }
            let mut e = edit.borrow_mut();
            store(&mut e);
            let current = e.info.language;
            let mains: Vec<Language> = Language::all()
                .filter(|l| l.0 == current || e.info.translation(l.0).is_none())
                .collect();
            if let Some(l) = mains.get(d.selected() as usize) {
                e.info.language = l.0;
            }
            refill(&e);
        });
    }
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
        let mut e = edit.borrow_mut();
        store(&mut e);
        // Codes are checked before anything is saved.
        let code = match &e.song {
            Some(x) => x.isrc.clone(),
            None => e.info.upc.clone(),
        };
        let checked = match (code.as_str(), &e.song) {
            ("", _) => Ok(String::new()),
            (c, Some(_)) => normalize_isrc(c),
            (c, None) => normalize_upc(c),
        };
        let code = match checked {
            Ok(c) => c,
            Err(err) => {
                error.set_text(&err.to_string());
                if e.current.is_none() {
                    entries[1].grab_focus();
                }
                return;
            }
        };
        match &mut e.song {
            Some(x) => x.isrc = code,
            None => e.info.upc = code,
        }
        let action = AlbumAction::Texts {
            info: e.info.clone(),
            song: e.song.clone().map(Box::new),
        };
        drop(e);
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

/// Transport → Varispeed: the song faster or slower (and higher or lower)
/// by up to ±10 %, like a tape machine.
pub fn varispeed(app: &Rc<AppState>) {
    let range = faderframe_session::MANUAL_SPEED_RANGE;
    let win = gtk::Window::builder()
        .application(&app.app)
        .title("Varispeed")
        .resizable(false)
        .default_width(380)
        .build();
    if let Some(main) = app.window.borrow().as_ref() {
        win.set_transient_for(Some(main));
    }
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let on = gtk::CheckButton::with_label("Varispeed");
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, -range, range, 0.1);
    scale.set_draw_value(false);
    scale.set_hexpand(true);
    for (v, label) in [(-range, "-10 %"), (0.0, "0"), (range, "+10 %")] {
        scale.add_mark(v, gtk::PositionType::Bottom, Some(label));
    }
    let readout = gtk::Label::new(None);
    readout.add_css_class("dim-label");
    let current = app.session.borrow().manual_speed();
    on.set_active(current.is_some());
    scale.set_value(current.unwrap_or(0.0));
    scale.set_sensitive(current.is_some());
    let show = {
        let readout = readout.clone();
        move |p: f64| {
            let semitones = 12.0 * (1.0 + p / 100.0).log2();
            readout.set_text(&format!("{p:+.1} % · {semitones:+.2} semitones"));
        }
    };
    show(current.unwrap_or(0.0));
    let apply = {
        let weak = Rc::downgrade(app);
        let (on, scale) = (on.clone(), scale.clone());
        let show = show.clone();
        move || {
            let Some(a) = weak.upgrade() else { return };
            let p = scale.value();
            scale.set_sensitive(on.is_active());
            show(if on.is_active() { p } else { 0.0 });
            a.session
                .borrow_mut()
                .set_manual_speed(on.is_active().then_some(p));
        }
    };
    let apply = Rc::new(apply);
    {
        let apply = Rc::clone(&apply);
        on.connect_toggled(move |_| apply());
    }
    {
        let apply = Rc::clone(&apply);
        scale.connect_value_changed(move |_| apply());
    }
    let reset = gtk::Button::with_label("Normal Speed");
    {
        let scale = scale.clone();
        reset.connect_clicked(move |_| scale.set_value(0.0));
    }
    let hint = gtk::Label::new(Some(
        "Following MIDI clock or MTC, the master sets the speed instead.",
    ));
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&on);
    row.append(&readout);
    body.append(&row);
    body.append(&scale);
    body.append(&reset);
    body.append(&hint);
    win.set_child(Some(&body));
    win.present();
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

/// Ask where to save a sample of `track` (24-bit WAV), then make it.
pub fn save_sample(
    app: &Rc<AppState>,
    track: faderframe_core::TrackId,
    start: faderframe_timeline::MusicalTime,
    end: faderframe_timeline::MusicalTime,
    name: &str,
) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let wav = gtk::FileFilter::new();
    wav.set_name(Some("WAV files (.wav)"));
    wav.add_suffix("wav");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&wav);
    let dialog = gtk::FileDialog::builder()
        .title("Save Sample")
        .accept_label("Save")
        .modal(true)
        .initial_name(name)
        .filters(&filters)
        .build();
    let weak = Rc::downgrade(app);
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        if let (Ok(file), Some(app)) = (res, weak.upgrade())
            && let Some(mut path) = file.path()
        {
            if path.extension().is_none() {
                path.set_extension("wav");
            }
            app.dispatch(faderframe_session::Action::MakeSample {
                track,
                start,
                end,
                target: faderframe_session::sampling::SampleTarget::File(path),
            });
        }
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
