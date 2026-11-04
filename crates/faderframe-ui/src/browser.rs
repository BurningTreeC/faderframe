//! The media browser's actions: showing it (View → Browser, Ctrl+Shift+B),
//! asking for a new tag, and the development aids that drive it from a
//! script (search, preview, folders, Find Similar, placing).

use crate::state::AppState;
use faderframe_session::browser::{BrowserOp, Place};
use faderframe_session::{Action, WorkspaceAction};
use faderframe_workspace::ViewId;
use gtk::gio;
use gtk::prelude::*;
use std::path::PathBuf;
use std::rc::Rc;

pub(crate) fn install(app: &Rc<AppState>) {
    let simple = |name: &'static str, action: Action| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .activate(move |_, _, _| {
                if let Some(a) = weak.upgrade() {
                    a.dispatch(action.clone());
                }
            })
            .build()
    };
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
    let browse = |op: BrowserOp| Action::Browser(op);
    app.app.add_action_entries([
        simple(
            "show-browser",
            Action::Workspace(WorkspaceAction::ShowView(ViewId::browser())),
        ),
        simple("browser-stop", browse(BrowserOp::StopPreview)),
        // Development aid: `browser-search:<text>` searches (empty: none).
        named("browser-search", |a, arg| {
            a.dispatch(Action::Several(vec![
                Action::Workspace(WorkspaceAction::ShowView(ViewId::browser())),
                Action::Browser(BrowserOp::Search(arg.to_string())),
            ]));
        }),
        // Development aid: `browser-preview:<path>` hears a file (MIDI
        // files through the selected instrument).
        named("browser-preview", |a, arg| {
            a.dispatch(Action::Browser(BrowserOp::Preview(PathBuf::from(
                arg.trim(),
            ))));
        }),
        // Development aid: `browser-folder:<path>` adds a folder as a place
        // and looks in it; `browser-place:<project|samples|recent|home>`.
        named("browser-folder", |a, arg| {
            a.dispatch(Action::Several(vec![
                Action::Workspace(WorkspaceAction::ShowView(ViewId::browser())),
                Action::Browser(BrowserOp::AddFolder(PathBuf::from(arg.trim()))),
            ]));
        }),
        named("browser-place", |a, arg| {
            let place = match arg.trim() {
                "project" => Place::ProjectMedia,
                "samples" => Place::Samples,
                "recent" => Place::Recent,
                "home" => Place::Home,
                other => Place::Folder(PathBuf::from(other)),
            };
            a.dispatch(Action::Browser(BrowserOp::Open(place)));
        }),
        // Development aid: `browser-similar:<path>` lists the sounds most
        // like it (`x`: back to the folder).
        named("browser-similar", |a, arg| {
            let op = match arg.trim() {
                "x" | "" => BrowserOp::ClearSimilar,
                p => BrowserOp::FindSimilar(PathBuf::from(p)),
            };
            a.dispatch(Action::Browser(op));
        }),
        // Development aid: `browser-drop:<path>[|<path>…]` places files at
        // the playhead (on the selected audio track, else new tracks),
        // matched to the project as the browser's settings say.
        named("browser-drop", |a, arg| {
            let files: Vec<PathBuf> = arg
                .split('|')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect();
            let (track, at) = {
                let s = a.session.borrow();
                let track = s.selection.tracks.iter().copied().find(|t| {
                    s.project()
                        .track(*t)
                        .is_some_and(|t| t.kind == faderframe_project::TrackKind::Audio)
                });
                (track, s.playhead())
            };
            a.dispatch(Action::Browser(BrowserOp::Place { files, track, at }));
        }),
        // Development aid: `browser-sync:<on|off>`, `browser-key:<on|off>`.
        named("browser-sync", |a, arg| {
            let mut s = a.session.borrow().browser().settings();
            s.sync = arg.trim() != "off";
            a.dispatch(Action::Browser(BrowserOp::Settings(s)));
        }),
        named("browser-key", |a, arg| {
            let mut s = a.session.borrow().browser().settings();
            s.key = arg.trim() != "off";
            a.dispatch(Action::Browser(BrowserOp::Settings(s)));
        }),
        // Development aid: `browser-log:x` logs the rows shown.
        named("browser-log", |a, _| {
            let s = a.session.borrow();
            for r in s.browser().rows() {
                tracing::info!(
                    "browser: {} | {} | {:?} s | {:?} BPM | {} | {} stars | {}",
                    r.name,
                    r.format,
                    r.seconds,
                    r.bpm,
                    r.key.map(|k| k.name()).unwrap_or_default(),
                    r.rating,
                    r.tags.join(", ")
                );
            }
            if let Some(p) = s.browser_preview() {
                tracing::info!("browser: hearing {} ({})", p.name, p.fit);
            }
        }),
    ]);
}

/// Ask for a tag for `files` ([`faderframe_session::UiRequest::NewBrowserTag`]).
pub(crate) fn new_tag(app: &Rc<AppState>, files: Vec<PathBuf>) {
    crate::dialogs::name_prompt_checked(
        app,
        "New Tag",
        "Tag:",
        "",
        "Tag",
        |_, _| None,
        move |tag| {
            Action::Browser(BrowserOp::Tag {
                paths: files.clone(),
                tag,
                on: true,
            })
        },
    );
}
