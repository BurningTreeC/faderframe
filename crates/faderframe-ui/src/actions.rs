//! Application actions (menus, buttons, shortcuts).

use crate::state::AppState;
use faderframe_project::TrackKind;
use faderframe_session::launcher::LauncherOp;
use faderframe_session::shuttle::ShuttleOp;
use faderframe_session::{Action, TransportAction, WorkspaceAction};
use faderframe_workspace::{DockAreaId, ViewId};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::rc::Rc;

fn entry(
    app: &Rc<AppState>,
    name: &str,
    f: impl Fn(&Rc<AppState>) + 'static,
) -> gio::ActionEntry<gtk::Application> {
    let weak = Rc::downgrade(app);
    gio::ActionEntry::builder(name)
        .activate(move |_, _, _| {
            if let Some(app) = weak.upgrade() {
                f(&app);
            }
        })
        .build()
}

fn dispatch(app: &Rc<AppState>, name: &str, action: Action) -> gio::ActionEntry<gtk::Application> {
    entry(app, name, move |a| a.dispatch(action.clone()))
}

pub fn install(app: &Rc<AppState>) {
    use Action as A;
    use TransportAction as T;
    use WorkspaceAction as W;
    let mut entries = vec![
        entry(app, "new", |a| {
            crate::dialogs::confirm_discard(a, crate::templates::new_project)
        }),
        entry(app, "new-demo", |a| {
            crate::dialogs::confirm_discard(a, |a| {
                a.with_session(|s| s.new_project(true));
            })
        }),
        entry(app, "open", |a| {
            crate::dialogs::confirm_discard(a, crate::dialogs::open_project)
        }),
        entry(app, "clear-recent", crate::recent::clear),
        entry(app, "import-audio", crate::dialogs::import_audio),
        entry(app, "import-adm", crate::dialogs::import_adm),
        entry(app, "import-midi", crate::dialogs::import_midi),
        entry(app, "export-midi", crate::dialogs::export_midi),
        entry(app, "export-midi2", crate::dialogs::export_midi2),
        entry(app, "cancel-import", |a| {
            a.session.borrow().cancel_imports()
        }),
        entry(app, "save", crate::dialogs::save),
        entry(app, "save-as", |a| crate::dialogs::save_as(a, None)),
        entry(app, "quit", |a| {
            if let Some(w) = a.window.borrow().as_ref() {
                w.close();
            }
        }),
        // Development aid for scripted runs: quit without the unsaved-changes
        // question.
        entry(app, "quit-discard", |a| {
            a.session.borrow_mut().stop_audio();
            a.app.quit();
        }),
        entry(app, "render", crate::render::open),
        entry(app, "preferences", |a| crate::preferences::open(a, None)),
        entry(app, "audio-settings", |a| {
            crate::preferences::open(a, Some("audio"))
        }),
        entry(app, "midi-settings", |a| {
            crate::preferences::open(a, Some("midi"))
        }),
        entry(app, "restart-audio", |a| a.start_audio()),
        entry(app, "about", crate::dialogs::about),
        entry(app, "varispeed", crate::dialogs::varispeed),
        // Development aid: render the main window into the PNG named by
        // $FADERFRAME_SCREENSHOT (the app's own pixels only).
        entry(app, "screenshot", |a| {
            let Ok(pattern) = std::env::var("FADERFRAME_SCREENSHOT") else {
                tracing::warn!("screenshot: set FADERFRAME_SCREENSHOT to a .png path");
                return;
            };
            // "{n}" in the path numbers successive screenshots.
            static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
            let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = pattern.replace("{n}", &n.to_string());
            let canvases: Vec<_> = {
                let dock = a.dock.borrow();
                let mut hosts: Vec<_> = dock.hosts.values().collect();
                // Arranger first, then the other editors.
                hosts.sort_by_key(|(kind, _)| {
                    !matches!(kind, faderframe_workspace::ViewKind::Arranger)
                });
                // The edit toolbar (when shown) above everything.
                let mut list: Vec<_> = a
                    .chrome
                    .borrow()
                    .as_ref()
                    .filter(|c| c.edit_bar.is_visible())
                    .map(|c| c.edit_bar.clone())
                    .into_iter()
                    .collect();
                list.extend(
                    hosts
                        .into_iter()
                        .filter(|(_, h)| h.canvas.is_mapped())
                        .map(|(_, h)| h.canvas.clone()),
                );
                list
            };
            let main = a.window.borrow().clone();
            if let Some(w) = main.as_ref() {
                crate::screenshot::capture(w, &canvases, std::path::Path::new(&path));
                // Other open windows (plugin browser, preferences, …).
                for (i, other) in a
                    .app
                    .windows()
                    .into_iter()
                    .filter(|o| {
                        o.upcast_ref::<gtk::Window>() != w.upcast_ref::<gtk::Window>()
                            && o.is_visible()
                    })
                    .enumerate()
                {
                    let p = std::path::Path::new(&path);
                    let stem = p
                        .file_stem()
                        .map_or_else(String::new, |s| s.to_string_lossy().to_string());
                    let file = p.with_file_name(format!("{stem}-w{}.png", i + 2));
                    match crate::screenshot::window_to_png(&other, &file) {
                        Ok(()) => tracing::info!("screenshot saved to {}", file.display()),
                        Err(e) => tracing::warn!("screenshot of a window failed: {e}"),
                    }
                }
                crate::plugin_window::screenshot(std::path::Path::new(&path));
                // An open context menu.
                if let Some((menu, level)) = crate::canvas::open_menu() {
                    let p = std::path::Path::new(&path);
                    let stem = p
                        .file_stem()
                        .map_or_else(String::new, |s| s.to_string_lossy().to_string());
                    let file = p.with_file_name(format!("{stem}-menu.png"));
                    match crate::screenshot::popover_to_png(&menu, &file) {
                        Ok(()) => tracing::info!("menu ({level}) saved to {}", file.display()),
                        Err(e) => tracing::warn!("screenshot of the menu failed: {e}"),
                    }
                }
            }
        }),
        dispatch(app, "undo", A::Undo),
        dispatch(app, "redo", A::Redo),
        dispatch(app, "delete", A::DeleteSelection),
        dispatch(app, "split", A::SplitSelectedAtPlayhead),
        dispatch(app, "toggle-snap", A::ToggleSnap),
        entry(app, "toggle-edit-toolbar", |a| {
            let on = !a.session.borrow().editor.show_edit_toolbar;
            a.dispatch(A::SetEditFlag(
                faderframe_session::EditFlag::EditToolbar,
                on,
            ));
            let mut p = crate::prefs::Preferences::load();
            p.show_edit_toolbar = on;
            if let Err(e) = p.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
        }),
        dispatch(app, "toggle-follow", A::ToggleFollowPlayhead),
        dispatch(app, "add-audio", A::AddTrack(TrackKind::Audio)),
        dispatch(
            app,
            "add-audio-stereo",
            A::AddTrackWithLayout(TrackKind::Audio, faderframe_core::ChannelLayout::Stereo),
        ),
        // A new instrument track asks for its instrument right away.
        entry(app, "add-instrument", |a| {
            a.dispatch(A::AddTrack(TrackKind::Instrument));
            let track = a.session.borrow().selection.primary_track();
            if let Some(track) = track {
                a.dispatch(A::OpenPluginBrowser {
                    track,
                    target: faderframe_session::PluginTarget::Instrument,
                });
            }
        }),
        dispatch(app, "add-midi", A::AddTrack(TrackKind::Midi)),
        dispatch(app, "add-bus", A::AddTrack(TrackKind::Bus)),
        dispatch(app, "add-aux", A::AddTrack(TrackKind::Aux)),
        dispatch(app, "add-vca", A::AddTrack(TrackKind::Vca)),
        dispatch(app, "group-selected", A::GroupSelectedTracks),
        dispatch(app, "remove-tracks", A::RemoveSelectedTracks),
        dispatch(app, "arm-selected", A::ToggleArmSelected),
        entry(app, "save-track-preset", |a| {
            let tracks: Vec<_> = a
                .session
                .borrow()
                .selection
                .tracks
                .iter()
                .copied()
                .collect();
            crate::dialogs::save_track_presets(a, tracks);
        }),
        entry(app, "track-from-preset", |a| {
            crate::dialogs::track_preset(a, false)
        }),
        entry(app, "apply-track-preset", |a| {
            crate::dialogs::track_preset(a, true)
        }),
        entry(
            app,
            "export-track-preset",
            crate::dialogs::export_track_preset,
        ),
        entry(app, "plugin-browser", |a| {
            let target = {
                let s = a.session.borrow();
                let p = s.project();
                s.selection
                    .tracks
                    .iter()
                    .filter_map(|t| p.track(*t))
                    .chain(
                        p.tracks
                            .iter()
                            .filter(|t| t.kind.has_audio() && t.kind != TrackKind::Master),
                    )
                    .next()
                    .map(|t| {
                        let target =
                            if t.kind == TrackKind::Instrument && s.instrument_slot(t).is_none() {
                                faderframe_session::PluginTarget::Instrument
                            } else {
                                faderframe_session::PluginTarget::Insert(t.inserts.len())
                            };
                        (t.id, target)
                    })
            };
            if let Some((track, target)) = target {
                crate::plugin_browser::open(a, track, target);
            }
        }),
        entry(app, "toggle-automation", |a| {
            let tracks: Vec<_> = {
                let s = a.session.borrow();
                let sel: Vec<_> = s.selection.tracks.iter().copied().collect();
                if sel.is_empty() {
                    s.project()
                        .tracks
                        .iter()
                        .filter(|t| t.automation.lanes.iter().any(|l| !l.curve.is_empty()))
                        .map(|t| t.id)
                        .collect()
                } else {
                    sel
                }
            };
            for t in tracks {
                a.dispatch(Action::ToggleTrackAutomation(t));
            }
        }),
        entry(app, "toggle-take-lanes", |a| {
            let folders: Vec<_> = {
                let s = a.session.borrow();
                s.selection
                    .clips
                    .iter()
                    .copied()
                    .filter(|c| s.project().clip(*c).is_some_and(|c| c.as_takes().is_some()))
                    .collect()
            };
            for c in folders {
                a.dispatch(Action::ToggleTakeLanes(c));
            }
        }),
        dispatch(app, "play", A::Transport(T::TogglePlay)),
        dispatch(app, "stop", A::Transport(T::Stop)),
        dispatch(app, "to-start", A::Transport(T::ReturnToStart)),
        dispatch(app, "shuttle-reverse", A::Shuttle(ShuttleOp::Reverse)),
        dispatch(app, "shuttle-stop", A::Shuttle(ShuttleOp::Stop)),
        dispatch(app, "shuttle-forward", A::Shuttle(ShuttleOp::Forward)),
        dispatch(app, "previous-frame", A::Shuttle(ShuttleOp::Step(-1))),
        dispatch(app, "next-frame", A::Shuttle(ShuttleOp::Step(1))),
        dispatch(app, "loop", A::Transport(T::ToggleLoop)),
        dispatch(app, "record", A::Transport(T::ToggleRecord)),
        dispatch(app, "capture-midi", A::CaptureMidi),
        dispatch(app, "download-speech-model", A::DownloadSpeechModel),
        dispatch(app, "save-version", A::PromptSaveVersion),
        entry(app, "command-palette", crate::palette::open),
        entry(app, "shortcuts", crate::palette::shortcuts),
        dispatch(app, "show-versions", A::ShowVersions),
        dispatch(app, "save-template", A::PromptSaveTemplate),
        dispatch(
            app,
            "show-video",
            A::Workspace(W::ShowView(ViewId::video())),
        ),
        dispatch(app, "show-adr", A::Workspace(W::ShowView(ViewId::adr()))),
        dispatch(
            app,
            "show-lead-sheet",
            A::Workspace(W::ShowView(ViewId::lead_sheet())),
        ),
        dispatch(
            app,
            "show-events",
            A::Workspace(W::ShowView(ViewId::events())),
        ),
        dispatch(
            app,
            "show-spectral",
            A::Workspace(W::ShowView(ViewId::spectral())),
        ),
        dispatch(
            app,
            "show-setlist",
            A::Workspace(W::ShowView(ViewId::setlist())),
        ),
        dispatch(
            app,
            "show-mode",
            A::Show(faderframe_session::setlist::ShowOp::Enter),
        ),
        entry(app, "import-video", crate::video::import),
        entry(app, "conform-lists", crate::video::conform_lists),
        dispatch(
            app,
            "export-movie",
            A::Video(faderframe_session::video::VideoOp::ChooseExport),
        ),
        dispatch(
            app,
            "video-sync-test",
            A::Video(faderframe_session::video::VideoOp::SyncTest),
        ),
        dispatch(app, "video-full-screen", A::FullScreen(ViewId::video())),
        dispatch(app, "new-from-template", A::ShowTemplates),
        entry(app, "panic", |a| {
            a.with_session(|s| {
                s.engine_reset_processors()?;
                Ok(())
            });
        }),
        dispatch(
            app,
            "show-mixer",
            A::Workspace(W::ShowView(ViewId::mixer())),
        ),
        dispatch(
            app,
            "show-piano-roll",
            A::Workspace(W::ShowView(ViewId::piano_roll())),
        ),
        dispatch(
            app,
            "show-pitch",
            A::Workspace(W::ShowView(ViewId::pitch())),
        ),
        dispatch(
            app,
            "show-clip-fx",
            A::Workspace(W::ShowView(ViewId::clip_fx())),
        ),
        dispatch(
            app,
            "show-launcher",
            A::Workspace(W::ShowView(ViewId::launcher())),
        ),
        dispatch(
            app,
            "show-automation",
            A::Workspace(W::ShowView(ViewId::automation())),
        ),
        dispatch(
            app,
            "show-performance",
            A::Workspace(W::ShowView(ViewId::performance())),
        ),
        dispatch(
            app,
            "show-history",
            A::Workspace(W::ShowView(ViewId::history())),
        ),
        dispatch(app, "show-ddp", A::Workspace(W::ShowView(ViewId::ddp()))),
        dispatch(
            app,
            "show-surround",
            A::Workspace(W::ShowView(ViewId::surround())),
        ),
        dispatch(
            app,
            "show-modulators",
            A::Workspace(W::ShowView(ViewId::modulators())),
        ),
        dispatch(
            app,
            "show-tools",
            A::Workspace(W::ShowView(ViewId::tools())),
        ),
        dispatch(
            app,
            "show-album",
            A::Workspace(W::ShowView(ViewId::album())),
        ),
        dispatch(
            app,
            "detach-tools",
            A::Workspace(W::Detach(ViewId::tools())),
        ),
        dispatch(
            app,
            "detach-performance",
            A::Workspace(W::Detach(ViewId::performance())),
        ),
        dispatch(
            app,
            "toggle-dock",
            A::Workspace(W::ToggleArea(DockAreaId::bottom())),
        ),
        dispatch(
            app,
            "detach-mixer",
            A::Workspace(W::Detach(ViewId::mixer())),
        ),
        dispatch(
            app,
            "detach-piano-roll",
            A::Workspace(W::Detach(ViewId::piano_roll())),
        ),
        entry(app, "dock-all", |a| {
            let ids: Vec<_> = a
                .session
                .borrow()
                .workspace()
                .active_layout()
                .floating
                .iter()
                .map(|f| f.id)
                .collect();
            for id in ids {
                a.dispatch(Action::Workspace(WorkspaceAction::CloseWindow(id)));
            }
        }),
        dispatch(app, "workspace-reset", A::Workspace(W::ResetActive)),
    ];
    for (i, (_, h)) in faderframe_view_arranger::TRACK_HEIGHTS.iter().enumerate() {
        entries.push(dispatch(
            app,
            &format!("track-height-{i}"),
            A::SetTrackHeight {
                track: None,
                height: *h,
            },
        ));
    }
    for i in 0..5 {
        entries.push(dispatch(
            app,
            &format!("workspace-{}", i + 1),
            A::Workspace(W::Switch(i)),
        ));
    }
    app.app.add_action_entries(entries);
    // Development aid: insert a plugin (by id) on the selected or first
    // audio track, e.g. FADERFRAME_STARTUP_ACTIONS="insert-plugin:com.vendor.plugin".
    let weak = Rc::downgrade(app);
    let insert = gio::ActionEntry::builder("insert-plugin")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            let (Some(a), Some(id)) = (weak.upgrade(), param.and_then(|p| p.get::<String>()))
            else {
                return;
            };
            let found = {
                let s = a.session.borrow();
                let plugin = s
                    .available_plugins()
                    .into_iter()
                    .find(|p| p.plugin.id == id);
                let p = s.project();
                let track = s
                    .selection
                    .tracks
                    .iter()
                    .filter_map(|t| p.track(*t))
                    .chain(p.tracks.iter().filter(|t| t.kind == TrackKind::Audio))
                    .next()
                    .map(|t| (t.id, t.inserts.len()));
                plugin.zip(track)
            };
            match found {
                Some((plugin, (track, index))) => {
                    let placed = a.session.borrow_mut().place_plugin(
                        track,
                        faderframe_session::PluginTarget::Insert(index),
                        plugin.plugin,
                    );
                    if let Err(e) = placed {
                        a.report(e, false);
                    }
                    a.after_change();
                }
                None => tracing::warn!("insert-plugin: no plugin '{id}' or no audio track"),
            }
        })
        .build();
    // Development aid: `show-insert:<n>` / `show-insert-params:<n>` open the
    // editor of insert n of the selected (or first audio) track.
    /// Insert `n` of the selected (or the first audio) track.
    fn insert_of(a: &AppState, n: usize) -> Option<faderframe_core::PluginInstanceId> {
        let s = a.session.borrow();
        let p = s.project();
        s.selection
            .tracks
            .iter()
            .filter_map(|t| p.track(*t))
            .chain(p.tracks.iter().filter(|t| t.kind == TrackKind::Audio))
            .find(|t| t.inserts.len() > n)
            .map(|t| t.inserts[n].id)
    }
    // Development aid: `show-instrument:x` opens the editor of the selected
    // (or first) instrument track's instrument.
    let weak = Rc::downgrade(app);
    let show_instrument = gio::ActionEntry::builder("show-instrument")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, _| {
            let Some(a) = weak.upgrade() else { return };
            let found = {
                let s = a.session.borrow();
                let p = s.project();
                s.selection
                    .tracks
                    .iter()
                    .filter_map(|t| p.track(*t))
                    .chain(p.tracks.iter())
                    .find_map(|t| s.instrument_slot(t).map(|i| (t.id, i.id)))
            };
            match found {
                Some((track, plugin)) => a.dispatch(Action::OpenPluginEditor {
                    track,
                    plugin,
                    generic: false,
                }),
                None => tracing::warn!("show-instrument: no instrument"),
            }
        })
        .build();
    let show = |name: &'static str, generic: bool| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .parameter_type(Some(&String::static_variant_type()))
            .activate(move |_, _, param| {
                let Some(a) = weak.upgrade() else { return };
                let n: usize = param
                    .and_then(|p| p.get::<String>())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let found = {
                    let s = a.session.borrow();
                    let p = s.project();
                    s.selection
                        .tracks
                        .iter()
                        .filter_map(|t| p.track(*t))
                        .chain(p.tracks.iter().filter(|t| t.kind == TrackKind::Audio))
                        .find(|t| t.inserts.len() > n)
                        .map(|t| (t.id, t.inserts[n].id))
                };
                match found {
                    Some((track, plugin)) => a.dispatch(Action::OpenPluginEditor {
                        track,
                        plugin,
                        generic,
                    }),
                    None => tracing::warn!("{name}: no insert {n}"),
                }
            })
            .build()
    };
    // Development aid: `midi:90 3c 64` plays bytes through the built-in
    // keyboard input (as if a MIDI keyboard sent them).
    let weak = Rc::downgrade(app);
    let midi = gio::ActionEntry::builder("midi")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            let (Some(a), Some(text)) = (weak.upgrade(), param.and_then(|p| p.get::<String>()))
            else {
                return;
            };
            let bytes: Vec<u8> = text
                .split([' ', ','])
                .filter(|t| !t.is_empty())
                .filter_map(|t| u8::from_str_radix(t, 16).ok())
                .collect();
            // A status byte starts each message (a SysEx runs to its F7).
            let s = a.session.borrow();
            let mut msg: Vec<u8> = Vec::new();
            for b in bytes {
                let in_sysex = msg.first() == Some(&0xF0);
                if b >= 0x80 && !msg.is_empty() && !(in_sysex && b == 0xF7) {
                    s.midi_keyboard().send(&msg);
                    msg.clear();
                }
                msg.push(b);
                if in_sysex && b == 0xF7 {
                    s.midi_keyboard().send(&msg);
                    msg.clear();
                }
            }
            if !msg.is_empty() {
                s.midi_keyboard().send(&msg);
            }
        })
        .build();
    // Editing by name (menus, scripts): `edit-mode:<shuffle|slip|spot|grid>`,
    // `edit-tool:<smart|trim|stretch|select|grab|separate|scrub|pencil|zoom>`,
    // `edit-flag:<warp|transients|tab-transients|link|insertion-follows|follow>`
    // (toggles), `edit:<separate|trim|clear|silence|copy|cut|paste|duplicate|
    // quantize|humanize|separate-transients|unwarp>`, and the development aid
    // `select-clip:<track>` (adds the track's first clip to the selection).
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
    let edit_entries = [
        named("edit-mode", |a, arg| {
            use faderframe_session::EditMode as M;
            match arg {
                "shuffle" => a.dispatch(Action::SetEditMode(M::Shuffle)),
                "slip" => a.dispatch(Action::SetEditMode(M::Slip)),
                "spot" => a.dispatch(Action::SetEditMode(M::Spot)),
                "grid" => a.dispatch(Action::SetEditMode(M::Grid)),
                _ => tracing::warn!("edit-mode: unknown '{arg}'"),
            }
        }),
        named("edit-tool", |a, arg| {
            use faderframe_session::EditTool as T;
            let tool = match arg {
                "smart" => T::Smart,
                "trim" => T::Trim,
                "stretch" => T::TrimStretch,
                "select" => T::Select,
                "grab" => T::Grab,
                "separate" => T::GrabSeparation,
                "scrub" => T::Scrub,
                "pencil" => T::Pencil,
                "zoom" => T::Zoom,
                _ => return tracing::warn!("edit-tool: unknown '{arg}'"),
            };
            a.dispatch(Action::SetEditTool(tool));
        }),
        named("edit-flag", |a, arg| {
            use faderframe_session::EditFlag as F;
            let e = a.session.borrow().editor;
            let (flag, on) = match arg {
                "warp" => (F::Warp, e.warp),
                "transients" => (F::ShowTransients, e.show_transients),
                "tab-transients" => (F::TabToTransients, e.tab_to_transients),
                "link" => (F::LinkTimeline, e.link_timeline),
                "insertion-follows" => (F::InsertionFollowsPlayback, e.insertion_follows_playback),
                "follow" => (F::FollowPlayhead, e.follow_playhead),
                "samples" => (F::SnapToSamples, e.snap_samples),
                "zero-crossings" => (F::SnapToZeroCrossings, e.zero_crossings),
                _ => return tracing::warn!("edit-flag: unknown '{arg}'"),
            };
            a.dispatch(Action::SetEditFlag(flag, !on));
        }),
        named("edit", |a, arg| {
            let clips: Vec<faderframe_core::ClipId> =
                a.session.borrow().selection.clips.iter().copied().collect();
            let action = match arg {
                "separate" => Action::Separate,
                "trim" => Action::TrimToSelection,
                "clear" => Action::ClearRange,
                "silence" => Action::InsertSilence,
                "copy" => Action::CopyRange,
                "cut" => Action::CutRange,
                "paste" => Action::PasteRange,
                "duplicate" => Action::RepeatRange(1),
                "quantize" => Action::QuantizeClips(clips),
                "humanize" => Action::HumanizeClips(clips),
                "separate-transients" => Action::SeparateAtTransients(clips),
                "unwarp" => Action::ClearWarp(clips),
                _ => return tracing::warn!("edit: unknown '{arg}'"),
            };
            a.dispatch(action);
        }),
        // Development aid: `fade-demo:x` gives the selected clips long
        // fades (one drawn), +3 dB clip gain and, once transients are
        // known, a warp marker pulling a hit later.
        named("fade-demo", |a, _| {
            use faderframe_session::ClipEdge;
            let (clips, rate) = {
                let s = a.session.borrow();
                (
                    s.selection.clips.iter().copied().collect::<Vec<_>>(),
                    s.project().sample_rate as i64,
                )
            };
            a.dispatch(Action::SetFade {
                clips: clips.clone(),
                edge: ClipEdge::Start,
                length: Some(rate),
                shape: Some(faderframe_project::FadeShape::SCurve),
                bend: Some(45),
            });
            a.dispatch(Action::SetFade {
                clips: clips.clone(),
                edge: ClipEdge::End,
                length: Some(rate * 3 / 2),
                shape: Some(faderframe_project::FadeShape::EqualPower),
                bend: None,
            });
            a.dispatch(Action::ClipGain {
                clips: clips.clone(),
                delta_db: 3.0,
            });
            let target = {
                let s = a.session.borrow();
                clips.iter().find_map(|c| {
                    let clip = s.project().clip(*c)?;
                    let faderframe_project::ClipContent::Audio(au) = &clip.content else {
                        return None;
                    };
                    let hits = s.source_transients(au.source)?;
                    let hit = hits
                        .into_iter()
                        .find(|h| *h > au.source_offset + rate * 2)?;
                    Some((*c, hit, hit - au.source_offset + rate / 10))
                })
            };
            if let Some((clip, source, to)) = target {
                a.dispatch(Action::WarpTo {
                    clip,
                    source,
                    to,
                    drag: faderframe_session::warping::WarpDrag::Transients,
                });
            }
        }),
        // Development aid: `album:<sections|project|analyse|export|details|
        // ddp>` drives the album; `album:file=<path>` adds a file,
        // `crossfade=<s>@<n>`, `monitor=<n>` and `insert=<n>:<plugin id>`
        // work on song n (1-based).
        named("album", |a, arg| {
            use faderframe_session::album::AlbumAction as AA;
            // `details-in=<language code, hex>`: the release's details in a
            // language (made a translation when missing).
            if let Some(code) = arg.trim().strip_prefix("details-in=") {
                if let Ok(l) = u8::from_str_radix(code.trim_start_matches("0x"), 16) {
                    crate::dialogs::album_details_in(a, None, Some(l));
                }
                return;
            }
            let action = match arg.trim() {
                "sections" => AA::AddSections,
                "project" => AA::AddThisProject,
                "analyse" => AA::Analyse,
                "export" => AA::Export,
                "play" => AA::Play(None),
                "pause" => AA::Pause,
                "next" => AA::Skip(1),
                "details" => AA::Details(None),
                // The CD master on (with demo codes when there are none).
                "ddp" => {
                    let (mut settings, mut info) = {
                        let s = a.session.borrow();
                        let album = &s.project().album;
                        (album.settings.clone(), album.info.clone())
                    };
                    if info.upc.is_empty() {
                        info.upc = "0036000291452".into();
                        a.dispatch(Action::Album(AA::Info(info)));
                    }
                    settings.ddp = true;
                    AA::Settings(settings)
                }
                // The vinyl premaster: on (with a format index), and a
                // side break before song n (1-based).
                "vinyl" | "vinyl=0" | "vinyl=1" | "vinyl=2" | "vinyl=3" => {
                    let mut settings = a.session.borrow().project().album.settings.clone();
                    settings.vinyl.enabled = true;
                    if let Some(k) = arg
                        .trim()
                        .strip_prefix("vinyl=")
                        .and_then(|k| k.parse::<usize>().ok())
                    {
                        settings.vinyl.format = faderframe_project::album::VinylFormat::ALL[k];
                    }
                    AA::Settings(settings)
                }
                other if other.starts_with("side-break=") => {
                    let n = other
                        .strip_prefix("side-break=")
                        .and_then(|n| n.parse::<usize>().ok())
                        .unwrap_or(0);
                    let song = a
                        .session
                        .borrow()
                        .project()
                        .album
                        .songs
                        .get(n.saturating_sub(1))
                        .map(|s| s.id);
                    match song {
                        Some(song) => AA::SideBreak { song, on: true },
                        None => return,
                    }
                }
                other => {
                    if let Some(path) = other.strip_prefix("file=") {
                        AA::AddFiles(vec![std::path::PathBuf::from(path)])
                    } else if let Some((n, rest)) = other
                        .strip_prefix("crossfade=")
                        .and_then(|r| r.split_once('@'))
                    {
                        // `crossfade=<seconds>@<song n>` (1-based).
                        let s = a.session.borrow();
                        let song = n.parse::<f32>().ok().zip(
                            rest.parse::<usize>()
                                .ok()
                                .and_then(|i| s.project().album.songs.get(i.wrapping_sub(1))),
                        );
                        let Some((seconds, song)) = song else {
                            tracing::warn!("album: bad '{other}'");
                            return;
                        };
                        let mut song = song.clone();
                        song.crossfade = seconds;
                        AA::Update(song)
                    } else if let Some(n) = other.strip_prefix("monitor=") {
                        // `monitor=<song n>` hears its inserts (0: none).
                        let s = a.session.borrow();
                        let id = n
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| s.project().album.songs.get(i.wrapping_sub(1)))
                            .map(|x| x.id);
                        AA::Monitor(id)
                    } else if let Some(rest) = other.strip_prefix("insert=") {
                        // `insert=<song n>:<plugin id>` (shows its editor).
                        let s = a.session.borrow();
                        let parsed = rest.split_once(':').and_then(|(n, id)| {
                            let i = n.parse::<usize>().ok()?;
                            let song = s.project().album.songs.get(i.wrapping_sub(1))?;
                            let plugin = s
                                .available_plugins()
                                .into_iter()
                                .find(|p| p.plugin.id == id)?
                                .plugin;
                            Some((song.id, plugin))
                        });
                        let Some((song, plugin)) = parsed else {
                            tracing::warn!("album: bad '{other}'");
                            return;
                        };
                        AA::AddInsert { song, plugin }
                    } else {
                        tracing::warn!("album: unknown '{other}'");
                        return;
                    }
                }
            };
            a.dispatch(Action::Album(action));
        }),
        // Development aid: `device-demo:<eq|eq-sc|program-eq>` puts the
        // device on the first audio track, set up to show what it does, and
        // opens its editor (`eq-sc`: keyed from the next audio track, which
        // the analyser shows too).
        named("device-demo", |a, arg| {
            use faderframe_core::{ParameterId, builtin};
            use faderframe_plugin_host::eq::{Field, band_id};
            use faderframe_project::{Command, PluginRef};
            let keyed = arg.trim() == "eq-sc";
            let (id, name) = match arg.trim() {
                "eq" | "eq-sc" => (builtin::EQ, "EQ"),
                "program-eq" => (builtin::PROGRAM_EQ, "Program EQ"),
                other => {
                    tracing::warn!("device-demo: unknown '{other}'");
                    return;
                }
            };
            let Some((track, index)) = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Audio)
                .map(|t| (t.id, t.inserts.len()))
            else {
                return;
            };
            let placed = a.session.borrow_mut().place_plugin(
                track,
                faderframe_session::PluginTarget::Insert(index),
                PluginRef::builtin(id, name),
            );
            if let Err(e) = placed {
                a.report(e, false);
                return;
            }
            let Some(plugin) = a
                .session
                .borrow()
                .project()
                .track(track)
                .and_then(|t| t.inserts.get(index))
                .map(|s| s.id)
            else {
                return;
            };
            let mut commands = Vec::new();
            let values: Vec<(ParameterId, f64)> = if id == builtin::EQ {
                let mut v = Vec::new();
                // (type, freq, gain, q, slope, placement, range)
                for (b, band) in [
                    (3.0, 35.0, 0.0, 0.707, 24.0, 0.0, 0.0),
                    (0.0, 240.0, -3.5, 1.4, 12.0, 0.0, 0.0),
                    (0.0, 3_200.0, 4.0, 0.8, 12.0, 0.0, -5.0),
                    (2.0, 9_000.0, 3.0, 0.707, 12.0, 3.0, 0.0),
                    (5.0, 1_150.0, 0.0, 6.0, 12.0, 4.0, 0.0),
                    (0.0, 6_500.0, 0.0, 1.2, 12.0, 0.0, -6.0),
                ]
                .into_iter()
                .enumerate()
                {
                    for (f, x) in [
                        (Field::Type, band.0),
                        (Field::Freq, band.1),
                        (Field::Gain, band.2),
                        (Field::Q, band.3),
                        (Field::Slope, band.4),
                        (Field::Placement, band.5),
                        (Field::Range, band.6),
                        (Field::Enabled, 1.0),
                    ] {
                        v.push((band_id(b, f), x));
                    }
                }
                // Band 6 is spectral; band 3 has its own threshold.
                v.push((band_id(5, Field::Spectral), 1.0));
                v.push((band_id(2, Field::Dynamics), 1.0));
                v.push((band_id(2, Field::Threshold), -42.0));
                if keyed {
                    // Keyed from the next audio track, shown as the
                    // external spectrum.
                    v.push((band_id(2, Field::Key), 1.0));
                    let source = a
                        .session
                        .borrow()
                        .project()
                        .tracks
                        .iter()
                        .filter(|t| t.kind == TrackKind::Audio && t.id != track)
                        .map(|t| t.id)
                        .next();
                    if let Some(source) = source {
                        commands.push(Command::SetPluginSidechain {
                            track,
                            plugin,
                            source: Some(source),
                        });
                    }
                    a.dispatch(Action::SetDeviceView {
                        plugin,
                        values: vec![
                            ("eq.analyser.external".into(), 1.0),
                            ("eq.analyser.source".into(), -1.0),
                        ],
                    });
                }
                v
            } else {
                faderframe_plugin_host::program_eq::PRESETS[0]
                    .1
                    .iter()
                    .map(|(p, x)| (ParameterId(*p as u32), *x))
                    .collect()
            };
            commands.extend(values.into_iter().map(|(parameter, value)| {
                Command::SetPluginParameter {
                    track,
                    plugin,
                    parameter,
                    value: Some(value),
                }
            }));
            a.dispatch(Action::Edit(Command::Batch {
                label: "Device Demo".into(),
                commands,
            }));
        }),
        // Development aid: `device-click:<x>/<y>[/right|middle]` clicks into
        // the open device editor (view pixels below its header bar).
        named("device-click", |_, arg| {
            let mut parts = arg.split('/').map(str::trim);
            let x = parts.next().and_then(|v| v.parse::<f32>().ok());
            let y = parts.next().and_then(|v| v.parse::<f32>().ok());
            let button = match parts.next() {
                Some("right") => faderframe_ui_canvas::PointerButton::Secondary,
                Some("middle") => faderframe_ui_canvas::PointerButton::Middle,
                _ => faderframe_ui_canvas::PointerButton::Primary,
            };
            if let (Some(x), Some(y)) = (x, y) {
                crate::plugin_window::click_device(x, y, button);
            }
        }),
        // Development aid: `device-set:<id>=<value>[/<id>=<value>…]` sets
        // parameters of the device editor opened last.
        named("device-set", |a, arg| {
            let Some(plugin) = crate::plugin_window::latest_device() else {
                tracing::warn!("device-set: no device editor is open");
                return;
            };
            let Some(track) = a.session.borrow().plugin_owner(plugin).map(|(t, _)| t) else {
                return;
            };
            let commands = arg
                .split('/')
                .filter_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    Some(faderframe_project::Command::SetPluginParameter {
                        track,
                        plugin,
                        parameter: faderframe_core::ParameterId(k.trim().parse().ok()?),
                        value: Some(v.trim().parse().ok()?),
                    })
                })
                .collect();
            a.dispatch(Action::Edit(faderframe_project::Command::Batch {
                label: "Device Set".into(),
                commands,
            }));
        }),
        // Development aids: `detect-key:x`, `detect-chords:x` (from the
        // selected MIDI clips, else all), `set-key:<key>` (from the start).
        named("detect-key", |a, _| a.dispatch(Action::DetectKey)),
        named("detect-chords", |a, _| a.dispatch(Action::DetectChords)),
        // `midi-tool:<label>` opens the piano roll's MIDI Tools panel on a
        // tool (by its label, any case; `off` closes it), `midi-tool:apply`
        // applies it to the open clip's selection.
        named("midi-tool", |a, arg| {
            use faderframe_project::midi_tools::Tool;
            let arg = arg.trim().to_lowercase();
            if arg == "apply" {
                let (clip, notes) = {
                    let s = a.session.borrow();
                    let Some(clip) = s.editor_clip() else { return };
                    (clip, s.selection.notes.iter().copied().collect::<Vec<_>>())
                };
                a.dispatch(Action::ApplyMidiTool { clip, notes });
                return;
            }
            let mut pr = a.session.borrow().editor.piano;
            pr.tool = Tool::TRANSFORMS
                .iter()
                .chain(Tool::GENERATORS.iter())
                .find(|t| t.label().to_lowercase() == arg)
                .copied();
            a.dispatch(Action::SetPianoRoll(pr));
        }),
        // `set-chord:<from>-<to>=<chord>` (quarters): the chord track.
        named("set-chord", |a, arg| {
            let parsed = arg.split_once('=').and_then(|(span, chord)| {
                let (from, to) = span.split_once('-')?;
                Some((
                    from.trim().parse::<f64>().ok()?,
                    to.trim().parse::<f64>().ok()?,
                    faderframe_project::harmony::Chord::parse(chord)?,
                ))
            });
            let Some((from, to, chord)) = parsed else {
                tracing::warn!("set-chord: '{arg}' is not <from>-<to>=<chord>");
                return;
            };
            let q = faderframe_timeline::MusicalTime::from_quarters;
            let chords = faderframe_project::harmony::set_chord(
                &a.session.borrow().project().chords,
                q(from),
                q(to),
                Some(chord),
            );
            a.dispatch(Action::Edit(faderframe_project::Command::SetChords {
                chords,
            }));
        }),
        // Development aid: `make-sample:<track>=<sampler|drums>` samples the
        // track's selection (or its first clip).
        named("make-sample", |a, arg| {
            let Some((name, to)) = arg.split_once('=') else {
                tracing::warn!("make-sample: '{arg}' is not <track>=<sampler|drums>");
                return;
            };
            let action = {
                let s = a.session.borrow();
                let track = s.project().tracks.iter().find(|t| t.name == name);
                track.and_then(|t| {
                    let clip = s.project().clips_of(t.id).first().map(|c| c.id);
                    let index = usize::from(to == "drums");
                    s.sample_choices(t.id, clip)
                        .into_iter()
                        .nth(index)
                        .map(|c| c.1)
                })
            };
            match action {
                Some(action) => a.dispatch(action),
                None => tracing::warn!("make-sample: nothing to sample on '{name}'"),
            }
        }),
        // Development aids: `add-modulator:<lfo|follower|steps|random|macro>`
        // (on the first selected track) and `mod-route:<n>:<target>@<depth>`
        // (modulator n of that track moves the target named as in its
        // "+ Target" menu, e.g. `Volume` or `FaderFrame Synth · Cutoff`).
        named("add-modulator", |a, arg| {
            let track = a.session.borrow().selection.tracks.iter().next().copied();
            let source = faderframe_project::modulation::ModSource::defaults()
                .into_iter()
                .find(|s| {
                    s.kind_label()
                        .to_lowercase()
                        .starts_with(&arg.to_lowercase())
                        || (arg == "follower" && s.kind_label() == "Envelope Follower")
                });
            match (track, source) {
                (Some(track), Some(source)) => a.dispatch(Action::AddModulator { track, source }),
                _ => tracing::warn!("add-modulator: no selected track or no kind '{arg}'"),
            }
        }),
        named("mod-route", |a, arg| {
            let parsed = arg.split_once(':').and_then(|(n, rest)| {
                let (target, depth) = rest.rsplit_once('@')?;
                Some((n.parse::<usize>().ok()?, target, depth.parse::<f32>().ok()?))
            });
            let Some((n, label, depth)) = parsed else {
                tracing::warn!("mod-route: '{arg}' is not <n>:<target>@<depth>");
                return;
            };
            let action = {
                let s = a.session.borrow();
                let track = s
                    .selection
                    .tracks
                    .iter()
                    .next()
                    .and_then(|t| s.project().track(*t));
                track.and_then(|t| {
                    let mut m = t.modulators.get(n)?.clone();
                    let target = s.modulation_targets(t.id).into_iter().find(|c| {
                        c.name == label || format!("{} · {}", c.group, c.name) == label
                    })?;
                    m.routes.push(faderframe_project::modulation::ModRoute {
                        target: target.target,
                        depth,
                    });
                    Some(Action::SetModulator {
                        track: t.id,
                        modulator: m,
                    })
                })
            };
            match action {
                Some(action) => a.dispatch(action),
                None => tracing::warn!("mod-route: no modulator {n} or target '{label}'"),
            }
        }),
        // Development aid: `chain-insert:<n>=<builtin id>` puts a built-in
        // device into chain n of the first container of the selected (or
        // first audio) track.
        named("chain-insert", |a, arg| {
            let parsed = arg
                .split_once('=')
                .and_then(|(n, id)| Some((n.parse::<usize>().ok()?, id)));
            let Some((chain, id)) = parsed else {
                tracing::warn!("chain-insert: '{arg}' is not <n>=<builtin id>");
                return;
            };
            let action = {
                let s = a.session.borrow();
                let p = s.project();
                let plugin = s
                    .available_plugins()
                    .into_iter()
                    .find(|x| x.plugin.id == id)
                    .map(|x| x.plugin);
                let at = s
                    .selection
                    .tracks
                    .iter()
                    .filter_map(|t| p.track(*t))
                    .chain(p.tracks.iter().filter(|t| t.kind == TrackKind::Audio))
                    .find_map(|t| {
                        let c = t.inserts.iter().find(|x| x.plugin.is_container())?;
                        let len = t.containers.get(&c.id)?.get(chain)?.inserts.len();
                        Some((t.id, c.id, len))
                    });
                plugin.zip(at).map(
                    |(plugin, (track, container, index))| Action::InsertIntoChain {
                        track,
                        container,
                        chain,
                        index,
                        plugin,
                    },
                )
            };
            match action {
                Some(action) => a.dispatch(action),
                None => tracing::warn!("chain-insert: no container chain {chain} or no '{id}'"),
            }
        }),
        // Development aid: `video:<add-track|compare=<single|side|wipe>|
        // to-track=<clip>@<track>|trim=<clip>@<from frame>/<to frame>|
        // cuts=<clip>>` (clips and tracks counted from 0).
        named("video", |a, arg| {
            use faderframe_session::video::{VideoCompare, VideoOp};
            let (cmd, rest) = arg.split_once('=').unwrap_or((arg, ""));
            let (clips, tracks) = {
                let s = a.session.borrow();
                let v = &s.project().video;
                let clips: Vec<_> = v
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter().cloned())
                    .collect();
                let tracks: Vec<_> = v.tracks.iter().map(|t| t.id).collect();
                (clips, tracks)
            };
            let nth = |t: &str| t.trim().parse::<usize>().ok();
            let op = match cmd {
                "add-track" => Some(VideoOp::AddTrack),
                "compare" => Some(VideoOp::SetCompare(match rest {
                    "side" => VideoCompare::SideBySide,
                    "wipe" => VideoCompare::Wipe,
                    _ => VideoCompare::Single,
                })),
                "to-track" => rest.split_once('@').and_then(|(c, t)| {
                    let c = clips.get(nth(c)?)?;
                    Some(VideoOp::MoveClip {
                        clip: c.id,
                        start: c.start,
                        track: Some(*tracks.get(nth(t)?)?),
                    })
                }),
                "trim" => rest.split_once('@').and_then(|(c, span)| {
                    let c = clips.get(nth(c)?)?;
                    let (from, to) = span.split_once('/')?;
                    let (from, to) = (nth(from)? as i64, nth(to)? as i64);
                    let s = a.session.borrow();
                    let rate = s.project().sample_rate;
                    let fr = s.timecode().rate;
                    let ns = |f: i64| (fr.seconds_of(f) * 1e9) as i64;
                    Some(VideoOp::TrimClip {
                        clip: c.id,
                        start: c.start
                            + faderframe_project::video::ns_to_samples(ns(from) - c.offset, rate),
                        offset: ns(from),
                        length: ns(to) - ns(from),
                    })
                }),
                "cuts" => clips
                    .get(nth(rest).unwrap_or(0))
                    .map(|c| VideoOp::DetectCuts(c.id)),
                "conform" => rest.split_once('@').and_then(|(o, n)| {
                    Some(VideoOp::ConformPicture {
                        old: clips.get(nth(o)?)?.id,
                        new: clips.get(nth(n)?)?.id,
                    })
                }),
                _ => None,
            };
            match op {
                Some(op) => a.dispatch(Action::Video(op)),
                None => tracing::warn!("video: cannot do '{arg}'"),
            }
        }),
        // Development aid: `preferences-page:<general|audio|video|…>`.
        named("preferences-page", |a, arg| {
            crate::preferences::open(a, Some(arg));
        }),
        // Development aids: `import-video-from:<path>` imports a video with
        // its sound, `video-offset:<ms>` sets the picture offset,
        // `export-movie-to:<path>` writes the movie (container by the
        // extension).
        named("import-video-from", |a, arg| {
            a.dispatch(Action::Video(faderframe_session::video::VideoOp::Import {
                path: arg.into(),
                sound: true,
            }));
        }),
        // Development aid: `adr:<from-transcript|add=<s>-<s>[=<line>]|beeps|
        // run=<n>|record=<n>|track=<n>=<track name>>` (cues counted from 0).
        named("adr", |a, arg| {
            use faderframe_session::adr::AdrOp;
            let (cmd, rest) = arg.split_once('=').unwrap_or((arg, ""));
            let (cues, at, track_of) = {
                let s = a.session.borrow();
                let p = s.project();
                let cues: Vec<_> = p.adr.cues.clone();
                let rate = p.sample_rate as f64;
                let tl = p.timeline.clone();
                let track_of = |name: &str| p.tracks.iter().find(|t| t.name == name).map(|t| t.id);
                let first_audio = p
                    .tracks
                    .iter()
                    .find(|t| t.kind == faderframe_project::TrackKind::Audio)
                    .map(|t| t.id);
                (
                    cues,
                    move |secs: f64| tl.to_musical((secs * rate) as i64, rate),
                    (
                        track_of(rest.split_once('=').map_or("", |x| x.1)),
                        first_audio,
                    ),
                )
            };
            let nth = |t: &str| t.trim().parse::<usize>().ok().and_then(|n| cues.get(n));
            let op = match cmd {
                "from-transcript" => Some(AdrOp::FromTranscript { track: track_of.1 }),
                "beeps" => Some(AdrOp::MakeBeeps),
                "add" => {
                    let (span, text) = rest.split_once('=').unwrap_or((rest, ""));
                    span.split_once('-').and_then(|(x, y)| {
                        Some(AdrOp::Add {
                            start: at(x.trim().parse().ok()?),
                            end: at(y.trim().parse().ok()?),
                            text: text.to_string(),
                            track: track_of.1,
                        })
                    })
                }
                "run" | "record" => nth(rest).map(|c| AdrOp::Run {
                    cue: c.id,
                    record: cmd == "record",
                }),
                "track" => rest.split_once('=').and_then(|(n, _)| {
                    let c = nth(n)?;
                    Some(AdrOp::Set(faderframe_project::adr::AdrCue {
                        track: track_of.0,
                        ..c.clone()
                    }))
                }),
                _ => None,
            };
            match op {
                Some(op) => a.dispatch(Action::Adr(op)),
                None => tracing::warn!("adr: cannot do '{arg}'"),
            }
        }),
        // Development aid: `conform-lists:<old list>|<new list>`.
        named("conform-lists", |a, arg| match arg.split_once('|') {
            Some((old, new)) => a.dispatch(Action::Conform(
                faderframe_session::conform::ConformOp::Lists {
                    old: old.trim().into(),
                    new: new.trim().into(),
                },
            )),
            None => tracing::warn!("conform-lists: give <old>|<new>"),
        }),
        // Development aid: `shuttle-speed:<v>` (negative: reverse; 0: stop).
        named("shuttle-speed", |a, arg| match arg.trim().parse::<f64>() {
            Ok(v) => a.dispatch(Action::Shuttle(ShuttleOp::Speed(v))),
            Err(_) => tracing::warn!("shuttle-speed: cannot read '{arg}'"),
        }),
        // Development aid: `counter:<main|sub>=<bars|minsec|timecode|samples|auto>`.
        named("counter", |a, arg| {
            use faderframe_session::CounterUnit;
            let (which, unit) = arg.split_once('=').unwrap_or(("sub", arg));
            let unit = match unit.trim() {
                "bars" => Some(CounterUnit::BarsBeats),
                "minsec" => Some(CounterUnit::MinSecs),
                "timecode" => Some(CounterUnit::Timecode),
                "samples" => Some(CounterUnit::Samples),
                _ => None,
            };
            a.dispatch(Action::SetTransportCounter {
                sub: which.trim() != "main",
                unit,
            });
        }),
        // Development aid: `timecode:<rate id>@<hh:mm:ss:ff>` (the project's).
        named("timecode", |a, arg| {
            use faderframe_core::timecode::{FrameRate, Timecode};
            let (rate, start) = arg.split_once('@').unwrap_or((arg, "00:00:00:00"));
            let Some(rate) = FrameRate::from_id(rate.trim()) else {
                tracing::warn!("timecode: unknown rate '{rate}'");
                return;
            };
            let Some(start) = Timecode::parse(start, rate) else {
                tracing::warn!("timecode: cannot read '{start}'");
                return;
            };
            a.dispatch(Action::Edit(faderframe_project::Command::SetTimecode {
                timecode: Some(faderframe_project::video::ProjectTimecode { rate, start }),
            }));
        }),
        named("video-offset", |a, arg| match arg.trim().parse::<f64>() {
            Ok(ms) => a.dispatch(Action::Video(
                faderframe_session::video::VideoOp::SetOffset(ms),
            )),
            Err(_) => tracing::warn!("video-offset: not a number: {arg}"),
        }),
        named("export-movie-to", |a, arg| {
            let path = std::path::PathBuf::from(arg);
            let container = faderframe_video::mux::Container::for_path(&path).unwrap_or_default();
            a.dispatch(Action::Video(faderframe_session::video::VideoOp::Export {
                clip: None,
                path,
                container,
            }));
        }),
        // Development aids: `template:<name>` saves the project as a
        // template, `from-template:<name>` starts a new project from one
        // (unsaved changes are discarded).
        named("template", |a, arg| {
            a.dispatch(Action::SaveTemplate {
                name: arg.to_string(),
            });
        }),
        named(
            "from-template",
            |a, arg| match faderframe_session::templates::existing_template(arg) {
                Some(t) => {
                    a.with_session(|s| s.new_from_template(&t.path));
                }
                None => tracing::warn!("from-template: no template '{arg}'"),
            },
        ),
        // Development aid: `version:<name>` saves a version.
        named("version", |a, arg| {
            a.dispatch(Action::SaveVersion {
                name: arg.to_string(),
            });
        }),
        // Development aid: `alias-clip:<clip name>` (an alias right after it).
        // Development aids: `edit-pitch:<track>` (its first audio clip in
        // the pitch editor), `pitch:<correct|straighten|reset|move=<n>@<by>|
        // formant=<n>@<st>|keep|move-formants>` on the clip shown.
        named("edit-pitch", |a, arg| {
            let clip = {
                let s = a.session.borrow();
                let p = s.project();
                p.tracks.iter().find(|t| t.name == arg).and_then(|t| {
                    t.clips
                        .iter()
                        .copied()
                        .find(|c| p.clip(*c).is_some_and(|c| c.as_audio().is_some()))
                })
            };
            match clip {
                Some(c) => a.dispatch(Action::OpenPitchEditor(c)),
                None => tracing::warn!("edit-pitch: no audio clip on {arg}"),
            }
        }),
        // `add-surface:<mackie|xt|hui|osc[@<listen port>]>`: a control
        // surface without ports (MIDI) or listening on that UDP port.
        // `set-varispeed:<percent|off>`: the manual varispeed.
        named("set-varispeed", |a, arg| {
            let p = if arg == "off" {
                None
            } else {
                arg.parse::<f64>().ok()
            };
            a.session.borrow_mut().set_manual_speed(p);
        }),
        named("add-surface", |a, arg| {
            use faderframe_session::control::{SurfaceKind, SurfaceSettings};
            let (kind, port) = arg.split_once('@').unwrap_or((arg, ""));
            let kind = match kind {
                "mackie" => SurfaceKind::Mackie,
                "xt" => SurfaceKind::MackieExtender,
                "hui" => SurfaceKind::Hui,
                "osc" => SurfaceKind::Osc,
                _ => return,
            };
            let mut all = a.session.borrow().control_surfaces().to_vec();
            let mut s = SurfaceSettings {
                kind,
                ..SurfaceSettings::default()
            };
            if let Ok(p) = port.parse() {
                s.listen = p;
            }
            all.push(s);
            a.session.borrow_mut().set_control_surfaces(all);
        }),
        // Development aids for the clip launcher: `send-to-launcher:<track>`
        // (its first clip), `launch:<track>@<scene n>` (1-based),
        // `launch-scene:<n>`, `launcher:<stop-all|back|record|scene|name=<n>=<name>|follow=<track>@<n>=<kind>[/<bars>]>`.
        named("send-to-launcher", |a, arg| {
            let clip = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .and_then(|t| t.clips.first().copied());
            if let Some(clip) = clip {
                a.dispatch(Action::Launcher(LauncherOp::SendClips(vec![clip])));
            }
        }),
        named("launch", |a, arg| {
            let Some((name, n)) = arg.split_once('@') else {
                return;
            };
            let found = {
                let s = a.session.borrow();
                let p = s.project();
                let track = p.tracks.iter().find(|t| t.name == name).map(|t| t.id);
                let scene = n
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| p.launcher.scenes.get(n.saturating_sub(1)))
                    .map(|s| s.id);
                track.zip(scene)
            };
            if let Some((track, scene)) = found {
                a.dispatch(Action::Launcher(LauncherOp::Launch { track, scene }));
            }
        }),
        // `launch-record:<track>@<scene n>`: record into (or end recording
        // into) that slot.
        named("launch-record", |a, arg| {
            let Some((name, n)) = arg.split_once('@') else {
                return;
            };
            let found = {
                let s = a.session.borrow();
                let p = s.project();
                let track = p.tracks.iter().find(|t| t.name == name).map(|t| t.id);
                let scene = n
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| p.launcher.scenes.get(n.saturating_sub(1)))
                    .map(|s| s.id);
                track.zip(scene)
            };
            if let Some((track, scene)) = found {
                a.dispatch(Action::Launcher(LauncherOp::Record { track, scene }));
            }
        }),
        named("launch-scene", |a, arg| {
            let scene = {
                let s = a.session.borrow();
                arg.parse::<usize>()
                    .ok()
                    .and_then(|n| s.project().launcher.scenes.get(n.saturating_sub(1)))
                    .map(|s| s.id)
            };
            if let Some(scene) = scene {
                a.dispatch(Action::Launcher(LauncherOp::LaunchScene(scene)));
            }
        }),
        named("launcher", |a, arg| {
            let records = a.session.borrow().launcher_records();
            // `follow=<track>@<n>=<kind>[/<bars>][:<other>@<chance>]`: a
            // slot's follow action (kind as in its menu, e.g. next, or
            // `jump<scene>`; `none` clears it). `mode=<track>@<n>=<mode>
            // [/<quantize index>][/legato]`: its launch settings.
            if let Some(rest) = arg.strip_prefix("mode=") {
                use faderframe_project::launcher::{ClipLaunch, LaunchMode, LaunchQuantize};
                let Some((slot, what)) = rest.split_once('=') else {
                    return;
                };
                let mut parts = what.split('/');
                let mode = parts.next().and_then(|m| {
                    LaunchMode::ALL
                        .into_iter()
                        .find(|k| k.label().eq_ignore_ascii_case(m))
                });
                let quantize = parts
                    .next()
                    .and_then(|q| q.parse::<usize>().ok())
                    .and_then(|i| LaunchQuantize::ALL.get(i).copied());
                let legato = parts.next() == Some("legato");
                let found = slot_by_name(a, slot);
                if let (Some(mode), Some((track, scene))) = (mode, found) {
                    a.dispatch(Action::Launcher(LauncherOp::SetClipLaunch {
                        track,
                        scene,
                        launch: Some(ClipLaunch {
                            mode,
                            quantize,
                            legato,
                            tempo: None,
                        }),
                    }));
                }
                return;
            }
            if let Some(rest) = arg.strip_prefix("follow=") {
                use faderframe_project::launcher::{FollowAction, FollowKind};
                let Some((slot, what)) = rest.split_once('=') else {
                    return;
                };
                let Some((name, n)) = slot.split_once('@') else {
                    return;
                };
                let (what, second) = what
                    .split_once(':')
                    .map_or((what, None), |(a, b)| (a, Some(b)));
                let (kind, bars) = what.split_once('/').unwrap_or((what, "0"));
                let parse_kind = |k: &str| {
                    if let Some(n) = k.strip_prefix("jump") {
                        return n
                            .parse::<u16>()
                            .ok()
                            .map(|n| FollowKind::Jump(n.saturating_sub(1)));
                    }
                    FollowKind::ALL
                        .into_iter()
                        .find(|f| f.label().eq_ignore_ascii_case(k))
                };
                let (other, chance) = match second.and_then(|b| b.split_once('@')) {
                    Some((k, c)) => (parse_kind(k), c.parse().unwrap_or(50)),
                    None => (None, 100),
                };
                let follow = parse_kind(kind).map(|kind| FollowAction {
                    kind,
                    bars: bars.parse().unwrap_or(0),
                    other,
                    chance,
                });
                let found = {
                    let s = a.session.borrow();
                    let p = s.project();
                    let track = p.tracks.iter().find(|t| t.name == name).map(|t| t.id);
                    let scene = n
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| p.launcher.scenes.get(n.saturating_sub(1)))
                        .map(|s| s.id);
                    track.zip(scene)
                };
                if let Some((track, scene)) = found {
                    a.dispatch(Action::Launcher(LauncherOp::SetFollow {
                        track,
                        scene,
                        follow,
                    }));
                }
                return;
            }
            // `name=<n>=<name>`: scene n (1-based) renamed.
            if let Some((n, name)) = arg.strip_prefix("name=").and_then(|r| r.split_once('=')) {
                let scene = n.parse::<usize>().ok().and_then(|n| {
                    let s = a.session.borrow();
                    s.project()
                        .launcher
                        .scenes
                        .get(n.saturating_sub(1))
                        .map(|s| s.id)
                });
                if let Some(scene) = scene {
                    a.dispatch(Action::Launcher(LauncherOp::RenameScene {
                        scene,
                        name: name.to_string(),
                    }));
                }
                return;
            }
            let op = match arg {
                "stop-all" => LauncherOp::StopAll,
                "back" => LauncherOp::BackToArrangement,
                "record" => LauncherOp::SetRecord(!records),
                "scene" => LauncherOp::AddScene { after: None },
                _ => return,
            };
            a.dispatch(Action::Launcher(op));
        }),
        // Development aids: `clip-fx:<track>` (its first clip in the clip
        // effects editor), `clip-fx-add:<plugin id>`,
        // `clip-fx-set:<n>:<parameter>=<value>` (on that clip).
        named("spectral", |a, arg| {
            let clip = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .and_then(|t| t.clips.first().copied());
            if let Some(clip) = clip {
                a.dispatch(Action::OpenSpectralEditor(clip));
            }
        }),
        named("spectral-add", |a, arg| {
            // `<remove|attenuate|heal|gain=<db>>@<from s>-<to s>/<low Hz>-<high Hz>`
            // on the spectral editor's clip (seconds from the clip's start).
            use faderframe_project::spectral::{SpectralEdit, SpectralOp, SpectralShape};
            let parsed = (|| {
                let (op, rest) = arg.split_once('@')?;
                let (time, band) = rest.split_once('/')?;
                let (t0, t1) = time.split_once('-')?;
                let (f0, f1) = band.split_once('-')?;
                let op = match op {
                    "remove" => SpectralOp::Remove,
                    "attenuate" => SpectralOp::Attenuate,
                    "heal" => SpectralOp::Heal,
                    g => SpectralOp::Gain {
                        db: g.strip_prefix("gain=")?.parse().ok()?,
                    },
                };
                Some((
                    op,
                    t0.parse::<f64>().ok()?,
                    t1.parse::<f64>().ok()?,
                    f0.parse::<f32>().ok()?,
                    f1.parse::<f32>().ok()?,
                ))
            })();
            let Some((op, t0, t1, f0, f1)) = parsed else {
                tracing::warn!("spectral-add: {arg}?");
                return;
            };
            let found = {
                let s = a.session.borrow();
                s.spectral_clip().and_then(|clip| {
                    let au = s.project().clip(clip)?.as_audio()?;
                    let (now, _) = s.spectral_sources(clip)?;
                    let (rate, _, _) = s.source_format(now)?;
                    Some((clip, au.source_offset, f64::from(rate)))
                })
            };
            if let Some((clip, offset, rate)) = found {
                let at = |t: f64| offset + (t * rate) as i64;
                a.dispatch(Action::EditSpectral {
                    clip,
                    change: faderframe_session::spectral::SpectralChange::Add(SpectralEdit::new(
                        SpectralShape::Rect {
                            start: at(t0),
                            end: at(t1),
                            low: f0,
                            high: f1,
                        },
                        op,
                    )),
                });
            }
        }),
        named("clip-fx", |a, arg| {
            let clip = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .and_then(|t| t.clips.first().copied());
            if let Some(clip) = clip {
                a.dispatch(Action::OpenClipEffects(clip));
            }
        }),
        named("clip-fx-add", |a, arg| {
            let found = {
                let s = a.session.borrow();
                s.clip_fx_clip().and_then(|clip| {
                    s.available_plugins()
                        .into_iter()
                        .find(|p| p.plugin.id == arg)
                        .map(|p| (clip, p.plugin))
                })
            };
            if let Some((clip, plugin)) = found {
                a.dispatch(Action::ClipEffects {
                    clip,
                    op: faderframe_session::clip_fx::ClipFxOp::Add(plugin),
                });
            }
        }),
        named("clip-fx-set", |a, arg| {
            let Some(clip) = a.session.borrow().clip_fx_clip() else {
                return;
            };
            let parsed = (|| {
                let (n, rest) = arg.split_once(':')?;
                let (p, v) = rest.split_once('=')?;
                Some((n.parse().ok()?, p.parse().ok()?, v.parse().ok()?))
            })();
            if let Some((index, parameter, value)) = parsed {
                a.dispatch(Action::ClipEffects {
                    clip,
                    op: faderframe_session::clip_fx::ClipFxOp::SetParameter {
                        index,
                        parameter: faderframe_core::ParameterId(parameter),
                        value,
                    },
                });
            }
        }),
        // Development aids: `transcribe:<track>` (its first clip's words to
        // the lyrics), `export-lyrics:<path>` (.srt, else LRC).
        named("transcribe", |a, arg| {
            let clip = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .and_then(|t| t.clips.first().copied());
            if let Some(clip) = clip {
                a.dispatch(Action::Transcribe(clip));
            }
        }),
        named("export-lyrics", |a, arg| {
            let text = a.session.borrow().lyrics_text(arg.ends_with(".srt"));
            if let Err(e) = std::fs::write(arg, text) {
                tracing::warn!("export-lyrics: {e}");
            }
        }),
        // Development aid: `to-midi:<melody|harmony|drums>=<track>` (its
        // first clip).
        named("to-midi", |a, arg| {
            use faderframe_session::to_midi::ToMidi;
            let Some((how, name)) = arg.split_once('=') else {
                return;
            };
            let how = match how {
                "melody" => ToMidi::Melody,
                "harmony" => ToMidi::Harmony,
                "drums" => ToMidi::Drums,
                _ => return,
            };
            let clip = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == name)
                .and_then(|t| t.clips.first().copied());
            if let Some(clip) = clip {
                a.dispatch(Action::ConvertToMidi { clip, how });
            }
        }),
        // Development aid: `tempo-from-hits:<markers|cuts|range>[@beats|
        // eighths|bars][/<slowest>-<fastest>][/near=<bpm>][/move]` (`move`:
        // audio follows the beats), `tempo-from-hits:dialog` opens the form.
        named("tempo-from-hits", |a, arg| {
            use faderframe_session::hits::{HitRequest, HitSource};
            use faderframe_timeline::hits::HitGrid;
            if arg.trim() == "dialog" {
                crate::dialogs::tempo_from_hits(a);
                return;
            }
            let mut parts = arg.split('/');
            let head = parts.next().unwrap_or("");
            let (source, grid) = head.split_once('@').unwrap_or((head, "beats"));
            let mut req = HitRequest {
                source: match source {
                    "cuts" => HitSource::Cuts,
                    "range" => HitSource::Range,
                    _ => HitSource::Markers,
                },
                ..HitRequest::default()
            };
            req.settings.grid = match grid {
                "eighths" => HitGrid::Eighths,
                "bars" => HitGrid::Bars(4.0),
                _ => HitGrid::Beats,
            };
            for p in parts {
                if p == "move" {
                    req.keep_audio = false;
                } else if let Some(n) = p.strip_prefix("near=") {
                    req.settings.preferred = n.parse().ok();
                } else if let Some((lo, hi)) = p.split_once('-') {
                    req.settings.min_bpm = lo.parse().unwrap_or(req.settings.min_bpm);
                    req.settings.max_bpm = hi.parse().unwrap_or(req.settings.max_bpm);
                }
            }
            a.dispatch(Action::TempoFromHits(req));
        }),
        // Development aid: `lead-sheet:<track>[@auto|straight|triplets]`
        // (its first clip) and `export-lead-sheet:<path.pdf|.musicxml>`.
        named("lead-sheet", |a, arg| {
            use faderframe_session::leadsheet::Grid;
            let (name, grid) = match arg.split_once('@') {
                Some((n, "straight")) => (n, Grid::Straight),
                Some((n, "triplets")) => (n, Grid::Triplets),
                Some((n, _)) => (n, Grid::Auto),
                None => (arg, Grid::Auto),
            };
            // `track:<name>` for the whole track, else its first clip.
            let (whole, name) = match name.strip_prefix("track:") {
                Some(n) => (true, n),
                None => (false, name),
            };
            let of = {
                let s = a.session.borrow();
                let t = s.project().tracks.iter().find(|t| t.name == name);
                match (t, whole) {
                    (Some(t), true) => {
                        Some(faderframe_session::leadsheet::LeadSheetOf::Track(t.id))
                    }
                    (Some(t), false) => t
                        .clips
                        .first()
                        .map(|c| faderframe_session::leadsheet::LeadSheetOf::Clip(*c)),
                    (None, _) => None,
                }
            };
            match of {
                Some(of) => a.dispatch(Action::MakeLeadSheet { of, grid }),
                None => tracing::warn!("lead-sheet: no clip on '{name}'"),
            }
        }),
        named("export-lead-sheet", |a, arg| {
            a.dispatch(Action::ExportLeadSheet(std::path::PathBuf::from(arg)));
        }),
        // Development aid: `song-structure:<track>[|<track>…]` (their
        // first clips).
        named("song-structure", |a, arg| {
            let clips: Vec<_> = {
                let s = a.session.borrow();
                arg.split('|')
                    .filter_map(|name| {
                        s.project()
                            .tracks
                            .iter()
                            .find(|t| t.name == name)
                            .and_then(|t| t.clips.first().copied())
                    })
                    .collect()
            };
            a.dispatch(Action::SongStructure { clips });
        }),
        // Development aid: `from-clip:<tempo|warp|key>=<track>` (its first
        // clip).
        named("from-clip", |a, arg| {
            use faderframe_session::detect::FromClip;
            let Some((what, name)) = arg.split_once('=') else {
                return;
            };
            let what = match what {
                "tempo" => FromClip::SetTempo,
                "warp" => FromClip::WarpToTempo,
                "key" => FromClip::SetKey,
                _ => return,
            };
            let clip = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == name)
                .and_then(|t| t.clips.first().copied());
            if let Some(clip) = clip {
                a.dispatch(Action::FromClip { clip, what });
            }
        }),
        named("pitch", |a, arg| {
            use faderframe_session::pitch::PitchOp;
            let Some(clip) = a.session.borrow().pitch_clip() else {
                tracing::warn!("pitch: no clip in the pitch editor");
                return;
            };
            let num = |s: &str| s.parse::<f32>().ok();
            let pair = |v: &str| {
                let (n, x) = v.split_once('@')?;
                Some((n.parse::<usize>().ok()?, num(x)?))
            };
            let op = match arg.split_once('=') {
                None if arg == "correct" => PitchOp::Correct {
                    notes: Vec::new(),
                    amount: 1.0,
                    drift: 0.5,
                },
                None if arg == "straighten" => PitchOp::Set {
                    notes: Vec::new(),
                    shift: None,
                    drift: Some(1.0),
                    formant: None,
                },
                None if arg == "reset" => PitchOp::Reset { notes: Vec::new() },
                None if arg == "keep" => PitchOp::KeepFormants(true),
                None if arg == "move-formants" => PitchOp::KeepFormants(false),
                Some(("move", v)) => match pair(v) {
                    Some((n, by)) => PitchOp::Move { notes: vec![n], by },
                    None => return,
                },
                Some(("formant", v)) => match pair(v) {
                    Some((n, st)) => PitchOp::Set {
                        notes: vec![n],
                        shift: None,
                        drift: None,
                        formant: Some(st),
                    },
                    None => return,
                },
                _ => {
                    tracing::warn!("pitch: unknown {arg}");
                    return;
                }
            };
            a.dispatch(Action::EditPitch { clip, op });
        }),
        named("alias-clip", |a, arg| {
            let id = a
                .session
                .borrow()
                .project()
                .clips
                .values()
                .find(|c| c.name == arg)
                .map(|c| c.id);
            if let Some(id) = id {
                a.dispatch(Action::DuplicateAsAlias(vec![id]));
            }
        }),
        // Development aids: `new-folder:<track>|<track>…`,
        // `toggle-folder:<folder>`, `move-to-folder:<track>=<folder|none>`.
        named("new-folder", |a, arg| {
            let tracks: Vec<_> = {
                let s = a.session.borrow();
                arg.split('|')
                    .filter_map(|n| {
                        s.project()
                            .tracks
                            .iter()
                            .find(|t| t.name == n)
                            .map(|t| t.id)
                    })
                    .collect()
            };
            a.dispatch(Action::NewFolder { tracks });
        }),
        named("toggle-folder", |a, arg| {
            let id = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .map(|t| t.id);
            if let Some(id) = id {
                a.dispatch(Action::ToggleFolder(id));
            }
        }),
        named("move-to-folder", |a, arg| {
            let Some((name, folder)) = arg.split_once('=') else {
                return;
            };
            let (track, folder) = {
                let s = a.session.borrow();
                let id = |n: &str| {
                    s.project()
                        .tracks
                        .iter()
                        .find(|t| t.name == n)
                        .map(|t| t.id)
                };
                (id(name), id(folder))
            };
            if let Some(track) = track {
                a.dispatch(Action::MoveToFolder {
                    tracks: vec![track],
                    folder,
                });
            }
        }),
        // Development aid: `midi-plays:<MIDI track>=<instrument track|none>`.
        named("midi-plays", |a, arg| {
            let Some((from, to)) = arg.split_once('=') else {
                tracing::warn!("midi-plays: '{arg}' is not <MIDI track>=<instrument track>");
                return;
            };
            let action = {
                let s = a.session.borrow();
                let id = |name: &str| {
                    s.project()
                        .tracks
                        .iter()
                        .find(|t| t.name == name)
                        .map(|t| t.id)
                };
                id(from).and_then(|track| {
                    s.midi_instrument_choices(track)
                        .into_iter()
                        .find(|c| {
                            c.label == "None" && to == "none"
                                || id(to).is_some_and(|_| c.label.starts_with(to))
                        })
                        .map(|c| c.action)
                })
            };
            match action {
                Some(action) => a.dispatch(action),
                None => tracing::warn!("midi-plays: no choice '{arg}'"),
            }
        }),
        // Development aid: `right-click:<view id>@<x>/<y>` (a secondary
        // click in a docked view; negative coordinates count from its right
        // and bottom edges), then `menu:<label>` clicks an entry of the
        // context menu it opened and `menu-state:x` logs whether it is open.
        // Development aid: `access-key:<view id>=<tab|backtab|enter|up|down|
        // left|right|escape>[/…]` — the keyboard on a view's accessible
        // controls (as Tab, Enter and the arrows reach them).
        // Development aid: `voice-input:<track>=<n|off>` — the track takes
        // MIDI from input n's voice port (singing into MIDI).
        named("voice-input", |a, arg| {
            let Some((name, n)) = arg.split_once('=') else {
                return tracing::warn!("voice-input: '{arg}' is not <track>=<n|off>");
            };
            let track = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == name)
                .map(|t| t.id);
            let Some(track) = track else {
                return tracing::warn!("voice-input: no track '{name}'");
            };
            let port = n
                .parse::<u16>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .map(faderframe_session::voice::voice_port_key);
            a.dispatch(Action::Edit(faderframe_project::Command::SetTrackInput {
                track,
                input: faderframe_project::InputRouting::Midi {
                    port,
                    channel: None,
                },
            }));
        }),
        named("access-key", |a, arg| {
            use faderframe_ui_canvas::{Key, Modifiers};
            let Some((view, keys)) = arg.split_once('=') else {
                return tracing::warn!("access-key: '{arg}' is not <view id>=<keys>");
            };
            let canvas = a
                .dock
                .borrow()
                .hosts
                .get(&faderframe_workspace::ViewId::new(view))
                .map(|(_, h)| h.canvas.clone());
            let Some(canvas) = canvas else {
                return tracing::warn!("access-key: no view '{view}'");
            };
            for k in keys.split('/') {
                let key = match k {
                    "tab" => {
                        canvas.tab(true);
                        continue;
                    }
                    "backtab" => {
                        canvas.tab(false);
                        continue;
                    }
                    "enter" => Key::Enter,
                    "up" => Key::Up,
                    "down" => Key::Down,
                    "left" => Key::Left,
                    "right" => Key::Right,
                    "escape" => Key::Escape,
                    other => {
                        tracing::warn!("access-key: unknown key '{other}'");
                        continue;
                    }
                };
                canvas.access_key(key, Modifiers::NONE);
            }
            if let Some(n) = canvas.focused_control() {
                tracing::info!("access: on {}", n);
            }
        }),
        named("right-click", |a, arg| {
            use faderframe_ui_canvas::{Modifiers, Point, PointerButton, ViewEvent};
            let Some((view, at)) = arg.split_once('@') else {
                tracing::warn!("right-click: '{arg}' is not <view id>@<x>/<y>");
                return;
            };
            let canvas = a
                .dock
                .borrow()
                .hosts
                .get(&faderframe_workspace::ViewId::new(view))
                .map(|(_, h)| h.canvas.clone());
            let Some(canvas) = canvas else {
                tracing::warn!("right-click: no view '{view}'");
                return;
            };
            let mut xy = at.split('/').filter_map(|v| v.trim().parse::<f32>().ok());
            let (Some(x), Some(y)) = (xy.next(), xy.next()) else {
                return;
            };
            let x = if x < 0.0 {
                canvas.width() as f32 + x
            } else {
                x
            };
            let y = if y < 0.0 {
                canvas.height() as f32 + y
            } else {
                y
            };
            let pos = Point::new(x, y);
            canvas.deliver(ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                modifiers: Modifiers::NONE,
                clicks: 1,
            });
            canvas.deliver(ViewEvent::PointerUp {
                pos,
                button: PointerButton::Secondary,
                modifiers: Modifiers::NONE,
            });
        }),
        named("menu", |_, arg| {
            if !crate::canvas::activate_menu_entry(arg) {
                tracing::warn!("menu: no entry '{arg}' shown");
            }
        }),
        named("menu-state", |_, _| match crate::canvas::open_menu() {
            Some((menu, level)) => tracing::info!(
                "menu: open ({}) showing {level}, {}×{}",
                menu.is_visible(),
                menu.width(),
                menu.height()
            ),
            None => tracing::info!("menu: closed"),
        }),
        // Development aid: `track-format:<track>=<mono|stereo|5.1|7.1.4|…>`
        // (a track's channel format, as its Channel Format menu sets it).
        named("track-format", |a, arg| {
            let Some((name, format)) = arg.split_once('=') else {
                tracing::warn!("track-format: '{arg}' is not <track>=<format>");
                return;
            };
            let action = {
                let s = a.session.borrow();
                s.project()
                    .tracks
                    .iter()
                    .find(|t| t.name == name)
                    .and_then(|t| {
                        s.format_choices(t.id).into_iter().find(|c| {
                            c.label.eq_ignore_ascii_case(format)
                                || c.label
                                    .split(' ')
                                    .next()
                                    .is_some_and(|l| l.eq_ignore_ascii_case(format))
                        })
                    })
                    .map(|c| c.action)
            };
            match action {
                Some(action) => a.dispatch(action),
                None => tracing::warn!("track-format: no choice '{arg}'"),
            }
        }),
        // Development aid: `surround:<track>=<x>/<y>[/<z>/<spread>/<width>/<lfe dB>]`
        // (where the track sits in the bed it feeds).
        named("surround", |a, arg| {
            let Some((name, values)) = arg.split_once('=') else {
                tracing::warn!("surround: '{arg}' is not <track>=<x>/<y>/…");
                return;
            };
            let action = {
                let s = a.session.borrow();
                s.project().tracks.iter().find(|t| t.name == name).map(|t| {
                    let pan = values
                        .split('/')
                        .zip(faderframe_core::SurroundParam::ALL)
                        .fold(t.surround, |pan, (v, p)| match v.trim().parse::<f32>() {
                            Ok(v) => p.set(pan, v),
                            Err(_) => pan,
                        });
                    faderframe_session::Action::Edit(
                        faderframe_project::Command::SetTrackSurround { track: t.id, pan },
                    )
                })
            };
            match action {
                Some(action) => a.dispatch(action),
                None => tracing::warn!("surround: no track '{name}'"),
            }
        }),
        // Development aid: `object:<track>=<on|off>` (deliver the track as
        // an object), `import-adm-from:<path>`, `render-adm:<path>[|itu]`
        // (the whole project as an object-based master).
        named("object", |a, arg| {
            let Some((name, on)) = arg.split_once('=') else {
                return;
            };
            let track = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == name)
                .map(|t| t.id);
            match track {
                Some(track) => a.dispatch(faderframe_session::Action::Edit(
                    faderframe_project::Command::SetTrackObject {
                        track,
                        on: on == "on",
                    },
                )),
                None => tracing::warn!("object: no track '{name}'"),
            }
        }),
        named("import-adm-from", |a, arg| {
            a.dispatch(faderframe_session::Action::ImportAdm(arg.into()));
        }),
        named("render-adm", |a, arg| {
            let (path, profile) = match arg.split_once('|') {
                Some((p, "itu")) => (p, faderframe_adm::Profile::Itu),
                _ => (arg, faderframe_adm::Profile::DolbyAtmos),
            };
            let job = {
                let mut s = a.session.borrow_mut();
                let settings = faderframe_session::render::RenderSettings {
                    channels: faderframe_session::render::RenderChannels::Adm(profile),
                    ..faderframe_session::render::RenderSettings::defaults_for(
                        s.project(),
                        path.into(),
                    )
                };
                s.render(settings)
            };
            match job.and_then(faderframe_session::render::RenderJob::join) {
                Ok(_) => tracing::info!("render-adm: wrote {path}"),
                Err(e) => tracing::warn!("render-adm: {e}"),
            }
        }),
        // Development aid: `render-iamf:<path.mp4|path.iamf>[|opus|flac|lpcm]`
        // (an IAMF master of the project; Opus by default).
        named("render-iamf", |a, arg| {
            use faderframe_iamf::Codec;
            let (path, codec) = match arg.split_once('|') {
                Some((p, "flac")) => (p, Codec::Flac { bits: 24 }),
                Some((p, "lpcm")) => (p, Codec::Lpcm { bits: 24 }),
                Some((p, _)) => (
                    p,
                    Codec::Opus {
                        stereo_bitrate: 192_000,
                    },
                ),
                None => (
                    arg,
                    Codec::Opus {
                        stereo_bitrate: 192_000,
                    },
                ),
            };
            let job = {
                let mut s = a.session.borrow_mut();
                let settings = faderframe_session::render::RenderSettings {
                    channels: faderframe_session::render::RenderChannels::Iamf(codec),
                    ..faderframe_session::render::RenderSettings::defaults_for(
                        s.project(),
                        path.into(),
                    )
                };
                s.render(settings)
            };
            match job.and_then(faderframe_session::render::RenderJob::join) {
                Ok(_) => tracing::info!("render-iamf: wrote {path}"),
                Err(e) => tracing::warn!("render-iamf: {e}"),
            }
        }),
        // Development aid: `show-surround-panner:<track>`.
        named("show-surround-panner", |a, arg| {
            let track = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .map(|t| t.id);
            match track {
                Some(t) => a.dispatch(faderframe_session::Action::ShowSurroundPanner(t)),
                None => tracing::warn!("show-surround-panner: no track '{arg}'"),
            }
        }),
        // Development aid: `strip-width:<track name|all>=<px|default>`.
        named("strip-width", |a, arg| {
            let Some((name, width)) = arg.split_once('=') else {
                tracing::warn!("strip-width: '{arg}' is not <track>=<px>");
                return;
            };
            let width = width.trim().parse::<f32>().ok();
            let track = if name == "all" {
                None
            } else {
                let id = a
                    .session
                    .borrow()
                    .project()
                    .tracks
                    .iter()
                    .find(|t| t.name == name)
                    .map(|t| t.id);
                let Some(id) = id else {
                    tracing::warn!("strip-width: no track '{name}'");
                    return;
                };
                Some(id)
            };
            a.dispatch(Action::SetStripWidth { track, width });
        }),
        // Development aid: `meter-mode:<track|all>=<peak|peak-rms|vu|ppm|k20|k14|k12>`.
        named("meter-mode", |a, arg| {
            use faderframe_session::MeterMode as M;
            let Some((name, mode)) = arg.split_once('=') else {
                tracing::warn!("meter-mode: '{arg}' is not <track>=<mode>");
                return;
            };
            let mode = match mode {
                "peak" => M::Peak,
                "peak-rms" => M::PeakRms,
                "vu" => M::Vu,
                "ppm" => M::Ppm,
                "k20" => M::K20,
                "k14" => M::K14,
                "k12" => M::K12,
                _ => return tracing::warn!("meter-mode: unknown mode '{mode}'"),
            };
            let track = if name == "all" {
                None
            } else {
                let id = a
                    .session
                    .borrow()
                    .project()
                    .tracks
                    .iter()
                    .find(|t| t.name == name)
                    .map(|t| t.id);
                let Some(id) = id else {
                    return tracing::warn!("meter-mode: no track '{name}'");
                };
                Some(id)
            };
            a.dispatch(Action::SetMeterMode {
                track,
                mode: Some(mode),
            });
        }),
        // Development aid: `setlist:<from-sections|clear>` and
        // `show:<enter|leave|next|previous|play|go=<n>>`.
        named("setlist", |a, arg| {
            use faderframe_session::setlist::SetlistOp;
            match arg {
                "from-sections" => a.dispatch(Action::Setlist(SetlistOp::FromSections)),
                "clear" => a.dispatch(Action::Setlist(SetlistOp::Clear)),
                _ => tracing::warn!("setlist: unknown '{arg}'"),
            }
        }),
        named("show", |a, arg| {
            use faderframe_session::setlist::ShowOp;
            let op = match arg {
                "enter" => ShowOp::Enter,
                "leave" => ShowOp::Leave,
                "next" => ShowOp::Next,
                "previous" => ShowOp::Previous,
                "play" => ShowOp::PlayStop,
                g => match g.strip_prefix("go=").and_then(|n| n.parse::<usize>().ok()) {
                    Some(n) => ShowOp::Go(n.saturating_sub(1)),
                    None => return tracing::warn!("show: unknown '{arg}'"),
                },
            };
            a.dispatch(Action::Show(op));
        }),
        named("meter-bridge", |a, arg| {
            a.dispatch(Action::SetMeterBridge(arg != "off"));
        }),
        // Development aid: `track-height:<track|all>=<px>` (arranger rows).
        named("track-height", |a, arg| {
            let Some((name, height)) = arg.split_once('=') else {
                tracing::warn!("track-height: '{arg}' is not <track>=<px>");
                return;
            };
            let Ok(height) = height.trim().parse::<f32>() else {
                return;
            };
            let track = if name == "all" {
                None
            } else {
                let id = a
                    .session
                    .borrow()
                    .project()
                    .tracks
                    .iter()
                    .find(|t| t.name == name)
                    .map(|t| t.id);
                let Some(id) = id else {
                    tracing::warn!("track-height: no track '{name}'");
                    return;
                };
                Some(id)
            };
            a.dispatch(Action::SetTrackHeight { track, height });
        }),
        named("set-key", |a, arg| {
            let Some(key) = faderframe_project::harmony::Key::parse(arg) else {
                tracing::warn!("set-key: '{arg}' is not a key");
                return;
            };
            let keys = faderframe_project::harmony::set_key(
                &a.session.borrow().project().keys,
                faderframe_timeline::MusicalTime::ZERO,
                Some(key),
            );
            a.dispatch(Action::Edit(faderframe_project::Command::SetKeys { keys }));
        }),
        // Development aid: `set-instrument:<plugin id>` makes the plugin the
        // instrument of the selected (or first) instrument track.
        named("set-instrument", |a, id| {
            let found = {
                let s = a.session.borrow();
                let plugin = s
                    .available_plugins()
                    .into_iter()
                    .find(|p| p.plugin.id == id);
                let p = s.project();
                let track = s
                    .selection
                    .tracks
                    .iter()
                    .filter_map(|t| p.track(*t))
                    .chain(p.tracks.iter())
                    .find(|t| t.kind == TrackKind::Instrument)
                    .map(|t| t.id);
                plugin.zip(track)
            };
            let Some((plugin, track)) = found else {
                tracing::warn!("set-instrument: no plugin '{id}' or no instrument track");
                return;
            };
            let placed = a.session.borrow_mut().place_plugin(
                track,
                faderframe_session::PluginTarget::Instrument,
                plugin.plugin,
            );
            if let Err(e) = placed {
                a.report(e, false);
            }
            a.after_change();
        }),
        // Development aid: `device-samples:<slot>=<path>[|<slot>=<path>…]`
        // loads samples into the device editor opened last.
        named("device-samples", |a, arg| {
            let Some(plugin) = crate::plugin_window::latest_device() else {
                tracing::warn!("device-samples: no device editor is open");
                return;
            };
            for kv in arg.split('|') {
                if let Some((k, v)) = kv.split_once('=')
                    && let Ok(slot) = k.trim().parse::<usize>()
                {
                    a.dispatch(Action::LoadDeviceSamples {
                        plugin,
                        slot,
                        files: vec![std::path::PathBuf::from(v.trim())],
                    });
                }
            }
        }),
        // Development aid: `reload-plugins:x` starts every plugin again (a
        // crashed one, or after switching sandboxing).
        named("reload-plugins", |a, _| {
            a.dispatch(Action::ReloadAllPlugins);
        }),
        // Development aid: `render-preset:<n>` opens Render / Export with
        // delivery preset n (1-based).
        named("render-preset", |a, arg| {
            crate::render::open_with_preset(a, arg.trim().parse().unwrap_or(0));
        }),
        // Development aid: `log-levels:x` logs every track's meter (peak
        // dBFS, left/right) — scripted checks that a track makes sound.
        named("log-levels", |a, _| {
            let s = a.session.borrow();
            for t in &s.project().tracks {
                let m = s.meter(t.id);
                tracing::info!(
                    "level {}: {:.1} / {:.1} dBFS (hold {:.1})",
                    t.name,
                    m.left.level_db,
                    m.right.level_db,
                    m.left.hold_db.max(m.right.hold_db)
                );
            }
        }),
        // Presets (menus): `save-preset:<plugin id>`, `load-preset:<id>\n<path>`,
        // `delete-preset:<path>` (asks first).
        // Tracks for a multi-output plugin's extra outputs (menus):
        // `create-output-tracks:<plugin id>[\n<bus>]`, or `insert=<n>` for
        // insert n of the selected (or first audio) track.
        named("create-output-tracks", |a, arg| {
            let (id, bus) = match arg.split_once('\n') {
                Some((id, bus)) => (id, bus.trim().parse::<u16>().ok()),
                None => (arg, None),
            };
            let plugin = match id.strip_prefix("insert=") {
                Some(n) => n.trim().parse::<usize>().ok().and_then(|n| insert_of(a, n)),
                None => id
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .map(faderframe_core::PluginInstanceId),
            };
            match plugin {
                Some(plugin) => a.dispatch(Action::CreateOutputTracks {
                    plugin,
                    buses: bus.map(|b| vec![b]),
                }),
                None => tracing::warn!("create-output-tracks: no plugin {arg}"),
            }
        }),
        named("remove-output-track", |a, arg| {
            let (id, bus) = arg.split_once('\n').unwrap_or((arg, "0"));
            let plugin = match id.strip_prefix("insert=") {
                Some(n) => n.trim().parse::<usize>().ok().and_then(|n| insert_of(a, n)),
                None => id
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .map(faderframe_core::PluginInstanceId),
            };
            match (plugin, bus.trim().parse::<u16>()) {
                (Some(plugin), Ok(bus)) => a.dispatch(Action::RemoveOutputTrack { plugin, bus }),
                _ => tracing::warn!("remove-output-track: no plugin output {arg}"),
            }
        }),
        named("delete-preset", |a, arg| {
            a.dispatch(Action::PromptDeletePreset {
                path: std::path::PathBuf::from(arg),
            });
        }),
        named("save-preset", |a, arg| {
            if let Ok(id) = arg.parse::<u64>() {
                a.dispatch(Action::PromptSavePluginPreset(
                    faderframe_core::PluginInstanceId(id),
                ));
            }
        }),
        named("load-preset", |a, arg| {
            if let Some((id, path)) = arg.split_once('\n')
                && let Ok(id) = id.parse::<u64>()
            {
                a.dispatch(Action::LoadPluginPreset {
                    plugin: faderframe_core::PluginInstanceId(id),
                    path: std::path::PathBuf::from(path),
                });
            }
        }),
        // A plugin's own program (menus): `select-program:<plugin id>\n<index>`.
        named("select-program", |a, arg| {
            if let Some((id, index)) = arg.split_once('\n')
                && let (Ok(id), Ok(index)) = (id.parse::<u64>(), index.parse::<usize>())
            {
                a.dispatch(Action::SelectPluginProgram {
                    plugin: faderframe_core::PluginInstanceId(id),
                    index,
                });
            }
        }),
        // Development aids on insert n of the selected (or first audio)
        // track: `insert-program:<n>:<program>`, `log-programs:<n>`.
        named("insert-program", |a, arg| {
            let mut it = arg.split(':').map(|v| v.trim().parse::<usize>());
            let (Some(Ok(n)), Some(Ok(index))) = (it.next(), it.next()) else {
                return;
            };
            match insert_of(a, n) {
                Some(plugin) => a.dispatch(Action::SelectPluginProgram { plugin, index }),
                None => tracing::warn!("insert-program: no insert {n}"),
            }
        }),
        named("log-programs", |a, arg| {
            let n = arg.trim().parse::<usize>().unwrap_or(0);
            let Some(plugin) = insert_of(a, n) else {
                tracing::warn!("log-programs: no insert {n}");
                return;
            };
            let s = a.session.borrow();
            tracing::info!(
                "programs of insert {n}: {:?}, selected {:?}",
                s.plugin_programs(plugin),
                s.plugin_current_program(plugin)
            );
        }),
        // Development aid: `log-ahead:x` logs which tracks are rendered
        // ahead and how many of their blocks were late.
        named("log-ahead", |a, _| {
            let s = a.session.borrow();
            let (tracks, late) = s.render_ahead_status();
            tracing::info!(
                "render ahead {:?} (buses: {}): {tracks} tracks, {} strips, {late} late blocks",
                s.render_ahead(),
                s.render_ahead_buses(),
                s.render_ahead_strips(),
            );
        }),
        // Development aid: `ddp:<open=<folder>|play|pause|stop|next|previous|import|close>`
        // (the DDP player; `open` also shows it).
        named("ddp", |a, arg| {
            use faderframe_session::ddp::DdpAction as D;
            let arg = arg.trim();
            let action = if let Some(dir) = arg.strip_prefix("open=") {
                a.dispatch(Action::Workspace(
                    faderframe_session::WorkspaceAction::ShowView(
                        faderframe_workspace::ViewId::ddp(),
                    ),
                ));
                D::Open(std::path::PathBuf::from(dir))
            } else {
                match arg {
                    "play" => D::Play(None),
                    "pause" => D::Pause,
                    "stop" => D::Stop,
                    "next" => D::Skip(1),
                    "previous" => D::Skip(-1),
                    "import" => D::Import,
                    "close" => D::Close,
                    _ => return,
                }
            };
            a.dispatch(Action::Ddp(action));
        }),
        // Development aid: `render-ahead-buses:<0|1>`.
        named("render-ahead-buses", |a, arg| {
            let on = arg.trim() == "1";
            a.with_session(|s| s.set_render_ahead_buses(on));
        }),
        // Development aid: `plugin-precision:<0|1>` (64-bit processing).
        named("plugin-precision", |a, arg| {
            let on = arg.trim() == "1";
            a.with_session(|s| s.set_plugin_double_precision(on));
        }),
        // Development aids: `import-midi-from:<path>` / `export-midi-to:<path>`.
        // Development aid: `import-audio-from:<path>` (a new track, at the
        // start).
        named("import-audio-from", |a, arg| {
            a.dispatch(Action::ImportFiles {
                files: vec![std::path::PathBuf::from(arg)],
                track: None,
                at: faderframe_timeline::MusicalTime::ZERO,
            });
        }),
        named("import-midi-from", |a, arg| {
            let empty = a.session.borrow().project().clips.is_empty();
            a.dispatch(Action::ImportMidiFile {
                path: std::path::PathBuf::from(arg),
                at: faderframe_timeline::MusicalTime::ZERO,
                tempo: empty,
            });
        }),
        named("export-midi2-to", |a, arg| {
            crate::dialogs::export_midi2_to(a, std::path::Path::new(arg));
        }),
        named("export-midi-to", |a, arg| {
            crate::dialogs::export_midi_to(a, std::path::Path::new(arg));
        }),
        // Development aid: `save-to:<path>` saves the project there.
        named("save-to", |a, arg| {
            let path = std::path::PathBuf::from(arg);
            a.with_session(|s| s.save_as(&path));
        }),
        // Development aids: `add-marker:<quarters>`, `add-section:<a>-<b>`
        // (quarters) and `add-tempo:<quarters>`.
        named("add-marker", |a, arg| {
            if let Ok(q) = arg.parse::<f64>() {
                a.dispatch(Action::AddMarker(
                    faderframe_timeline::MusicalTime::from_quarters(q),
                ));
            }
        }),
        named("add-section", |a, arg| {
            let mut parts = arg.split('-').filter_map(|v| v.parse::<f64>().ok());
            if let (Some(x), Some(y)) = (parts.next(), parts.next()) {
                a.dispatch(Action::AddSection {
                    start: faderframe_timeline::MusicalTime::from_quarters(x),
                    end: faderframe_timeline::MusicalTime::from_quarters(y),
                });
            }
        }),
        // `move-section:<n>@<quarters>` / `copy-section:<n>@<quarters>`:
        // section number n (0-based) with its content.
        named("move-section", |a, arg| section_op(a, arg, false)),
        named("copy-section", |a, arg| section_op(a, arg, true)),
        named("add-tempo", |a, arg| {
            if let Ok(q) = arg.parse::<f64>() {
                a.dispatch(Action::AddTempoPoint(
                    faderframe_timeline::MusicalTime::from_quarters(q),
                ));
            }
        }),
        // Development aids: `zoom:<in|out|fit|selection>` zooms the
        // editors; `locate:<quarters>` moves the playhead.
        named("zoom", |a, arg| {
            use faderframe_session::ZoomRequest as Z;
            let z = match arg {
                "in" => Z::In,
                "out" => Z::Out,
                "selection" => Z::Selection,
                _ => Z::Fit,
            };
            a.dispatch(Action::Zoom(z));
        }),
        named("locate", |a, arg| {
            if let Ok(q) = arg.parse::<f64>() {
                a.dispatch(Action::Transport(
                    faderframe_session::TransportAction::Locate(
                        faderframe_timeline::MusicalTime::from_quarters(q),
                    ),
                ));
            }
        }),
        // Development aids: `select-also:<name>` adds a track to the
        // selection; `assign-vca:<name>` assigns the selection to a VCA.
        named("select-also", |a, arg| {
            let id = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .map(|t| t.id);
            if let Some(id) = id {
                a.dispatch(Action::SelectTracks {
                    tracks: vec![id],
                    mode: faderframe_session::SelectMode::Add,
                });
            }
        }),
        named("assign-vca", |a, arg| {
            let id = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .map(|t| t.id);
            if let Some(id) = id {
                a.dispatch(Action::AssignSelectedToVca(id));
            }
        }),
        // Development aids: `freeze-track:<name>`, `bounce-track:<name>`.
        named("freeze-track", |a, arg| {
            let id = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .map(|t| t.id);
            if let Some(id) = id {
                a.dispatch(Action::FreezeTrack(id));
            }
        }),
        named("bounce-track", |a, arg| {
            let id = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .map(|t| t.id);
            if let Some(id) = id {
                a.dispatch(Action::BounceTrack(id));
            }
        }),
        named("select-clip", |a, arg| {
            let clip = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == arg)
                .and_then(|t| t.clips.first().copied());
            match clip {
                Some(c) => a.dispatch(Action::SelectClips {
                    clips: vec![c],
                    mode: faderframe_session::SelectMode::Add,
                }),
                None => tracing::warn!("select-clip: no clip on '{arg}'"),
            }
        }),
    ];
    // Development aid: `window-size:<w>x<h>` resizes the main window.
    let weak = Rc::downgrade(app);
    let window_size = gio::ActionEntry::builder("window-size")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            let (Some(a), Some(text)) = (weak.upgrade(), param.and_then(|p| p.get::<String>()))
            else {
                return;
            };
            let size = text.split_once('x').and_then(|(w, h)| {
                Some((w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?))
            });
            let window = a.window.borrow().clone();
            match (size, window) {
                (Some((w, h)), Some(window)) => {
                    window.unmaximize();
                    window.set_default_size(w, h);
                }
                _ => tracing::warn!("window-size: expected <w>x<h>, got '{text}'"),
            }
        })
        .build();
    // Development aid: select a dedicated preamp on the selected track.
    let weak = Rc::downgrade(app);
    let preamp = gio::ActionEntry::builder("set-preamp")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            let (Some(a), Some(value)) = (weak.upgrade(), param.and_then(|p| p.get::<String>()))
            else {
                return;
            };
            let model = if value == "none" {
                None
            } else {
                let Ok(index) = value.parse::<usize>() else {
                    return;
                };
                Some(index)
            };
            let track = {
                let s = a.session.borrow();
                s.selection
                    .tracks
                    .iter()
                    .find(|id| s.project().track(**id).is_some_and(|t| t.kind.has_audio()))
                    .copied()
            };
            if let Some(track) = track {
                a.dispatch(Action::SetPreamp { track, model });
            }
        })
        .build();
    app.app.add_action_entries([preamp]);

    // Development aid: `select-track:<name>` selects a track by name.
    let weak = Rc::downgrade(app);
    let select = gio::ActionEntry::builder("select-track")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            let (Some(a), Some(name)) = (weak.upgrade(), param.and_then(|p| p.get::<String>()))
            else {
                return;
            };
            let id = a
                .session
                .borrow()
                .project()
                .tracks
                .iter()
                .find(|t| t.name == name)
                .map(|t| t.id);
            match id {
                Some(id) => a.dispatch(Action::SelectTracks {
                    tracks: vec![id],
                    mode: faderframe_session::SelectMode::Replace,
                }),
                None => tracing::warn!("select-track: no track '{name}'"),
            }
        })
        .build();
    // Development aid: `piano-lane:<velocity|pitch|pressure|timbre|volume|
    // pan|vibrato|expression>` picks
    // the piano roll's lane; `mpe-demo` turns MPE on for the edited clip's
    // track and gives its first notes glides and swells.
    let weak = Rc::downgrade(app);
    let lane = gio::ActionEntry::builder("piano-lane")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            let (Some(a), Some(name)) = (weak.upgrade(), param.and_then(|p| p.get::<String>()))
            else {
                return;
            };
            let mut pr = a.session.borrow().editor.piano;
            pr.expression = faderframe_project::ExpressionKind::ALL
                .into_iter()
                .find(|k| k.label().eq_ignore_ascii_case(&name));
            if pr.expression.is_none() {
                pr.lane = None;
            }
            a.dispatch(Action::SetPianoRoll(pr));
        })
        .build();
    let weak = Rc::downgrade(app);
    let mpe_demo = gio::ActionEntry::builder("mpe-demo")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                expression_demo(&a, true);
            }
        })
        .build();
    // Development aid: `expression-demo` gives the edited clip's first notes
    // glides, swells, pan sweeps and pressure as native note expressions
    // (the track stays a plain one).
    let weak = Rc::downgrade(app);
    let note_expression_demo = gio::ActionEntry::builder("expression-demo")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                expression_demo(&a, false);
            }
        })
        .build();
    app.app.add_action_entries(edit_entries);
    let weak = Rc::downgrade(app);
    let open_recent = gio::ActionEntry::builder("open-recent")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            if let (Some(a), Some(path)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) {
                crate::recent::open(&a, path);
            }
        })
        .build();
    app.app.add_action_entries([open_recent]);
    // View → Theme (radio items; also `theme:<id>` in start-up scripts).
    let weak = Rc::downgrade(app);
    let current = app.theme.borrow().id.to_string();
    let theme = gio::ActionEntry::builder("theme")
        .parameter_type(Some(&String::static_variant_type()))
        .state(current.to_variant())
        .activate(move |_, _, param| {
            if let (Some(a), Some(id)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) {
                a.set_theme(&id);
            }
        })
        .build();
    app.app.add_action_entries([theme]);
    // View → Master Strip at the Side (per workspace; a checkbox).
    let weak = Rc::downgrade(app);
    let shown = app.session.borrow().master_panel();
    let master_panel = gio::ActionEntry::builder("master-panel")
        .state(shown.to_variant())
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::Workspace(
                    faderframe_session::WorkspaceAction::ToggleMasterPanel,
                ));
            }
        })
        .build();
    app.app.add_action_entries([master_panel]);
    // View → the dock's views as check items (ticked while on screen),
    // and the bottom dock.
    for (name, id) in crate::dock::DOCK_VIEWS {
        let weak = Rc::downgrade(app);
        let entry = gio::ActionEntry::builder(&format!("view-{name}"))
            .state(false.to_variant())
            .activate(move |_, _, _| {
                if let Some(a) = weak.upgrade() {
                    a.dispatch(Action::Workspace(
                        faderframe_session::WorkspaceAction::ToggleView(id()),
                    ));
                    crate::dock::sync_view_actions(&a);
                }
            })
            .build();
        app.app.add_action_entries([entry]);
    }
    let weak = Rc::downgrade(app);
    let dock_bottom = gio::ActionEntry::builder("dock-bottom")
        .state(true.to_variant())
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::Workspace(
                    faderframe_session::WorkspaceAction::ToggleArea(
                        faderframe_workspace::DockAreaId::bottom(),
                    ),
                ));
                crate::dock::sync_view_actions(&a);
            }
        })
        .build();
    app.app.add_action_entries([dock_bottom]);
    crate::dock::sync_view_actions(app);
    app.app.add_action_entries([
        insert,
        midi,
        select,
        window_size,
        lane,
        mpe_demo,
        note_expression_demo,
        show("show-insert", false),
        show("show-insert-params", true),
        show_instrument,
    ]);

    // The default shortcuts, then the user's (the shortcut editor's).
    crate::palette::apply_shortcuts(&app.app, &crate::prefs::Preferences::load().shortcuts);
}

/// Remaining single-key shortcuts run after focused widgets. Space is
/// captured separately by `transport_keys` before GTK button activation.
/// With picture in the project, J/K/L shuttle (K held with J or L steps a
/// frame) in place of L's loop and K's metronome.
pub fn install_window_keys(app: &Rc<AppState>, window: &impl IsA<gtk::Widget>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let k_held = Rc::new(std::cell::Cell::new(false));
    let held = Rc::clone(&k_held);
    keys.connect_key_released(move |_, key, _, _| {
        if matches!(key, gdk::Key::k | gdk::Key::K) {
            held.set(false);
        }
    });
    let weak = Rc::downgrade(app);
    keys.connect_key_pressed(move |_, key, _, state| {
        let Some(app) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let mods = state
            & (gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK);
        if !mods.is_empty() {
            return glib::Propagation::Proceed;
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        if key == gdk::Key::Escape && app.session.borrow().midi_learning().is_some() {
            app.dispatch(Action::CancelMidiLearn);
            return glib::Propagation::Stop;
        }
        let picture = !app.session.borrow().project().video.tracks.is_empty();
        if picture && !shift {
            let op = match key {
                gdk::Key::j | gdk::Key::J if k_held.get() => Some(ShuttleOp::Step(-1)),
                gdk::Key::l | gdk::Key::L if k_held.get() => Some(ShuttleOp::Step(1)),
                gdk::Key::j | gdk::Key::J => Some(ShuttleOp::Reverse),
                gdk::Key::l | gdk::Key::L => Some(ShuttleOp::Forward),
                gdk::Key::k | gdk::Key::K => {
                    k_held.set(true);
                    Some(ShuttleOp::Stop)
                }
                _ => None,
            };
            if let Some(op) = op {
                app.dispatch(Action::Shuttle(op));
                return glib::Propagation::Stop;
            }
        }
        let action = match key {
            gdk::Key::Home => TransportAction::ReturnToStart,
            gdk::Key::l | gdk::Key::L if !shift => TransportAction::ToggleLoop,
            gdk::Key::k | gdk::Key::K | gdk::Key::KP_7 | gdk::Key::KP_Home if !shift => {
                crate::recording::toggle_metronome(&app);
                return glib::Propagation::Stop;
            }
            gdk::Key::R if shift => TransportAction::ToggleRecord,
            gdk::Key::KP_0 | gdk::Key::KP_Insert => TransportAction::Stop,
            gdk::Key::t | gdk::Key::T if !shift => {
                app.app.activate_action("toggle-take-lanes", None);
                return glib::Propagation::Stop;
            }
            gdk::Key::a | gdk::Key::A if !shift => {
                app.app.activate_action("toggle-automation", None);
                return glib::Propagation::Stop;
            }
            _ => return glib::Propagation::Proceed,
        };
        app.dispatch(Action::Transport(action));
        glib::Propagation::Stop
    });
    window.add_controller(keys);
}

/// `<n>@<quarters>`: move (or copy) section n with its content there.
/// Glides and pressure swells on the edited clip's first notes; with `mpe`
/// the track plays them over MPE, else as native note expressions (with a
/// volume swell and a pan sweep too).
fn expression_demo(a: &Rc<AppState>, mpe: bool) {
    use faderframe_project::{ExpressionKind, ExpressionPoint};
    use faderframe_timeline::MusicalTime;
    let found = {
        let s = a.session.borrow();
        s.editor_clip().and_then(|c| {
            let clip = s.project().clip(c)?;
            let notes: Vec<_> = clip.as_midi()?.notes.iter().take(4).copied().collect();
            Some((c, clip.track, notes))
        })
    };
    let Some((clip, track, notes)) = found else {
        tracing::warn!("expression demo: no MIDI clip in the editor");
        return;
    };
    if mpe {
        a.dispatch(Action::Edit(faderframe_project::Command::SetTrackMpe {
            track,
            mpe: Some(faderframe_project::MpeConfig::default()),
        }));
    }
    let pt = |q: f64, value: f32| ExpressionPoint {
        time: MusicalTime::from_quarters(q),
        value,
    };
    for (i, n) in notes.iter().enumerate() {
        let len = n.length.ticks() as f64 / faderframe_timeline::TICKS_PER_QUARTER as f64;
        let glide = [2.0, -1.0, 3.0, -2.0][i % 4];
        let mut curves = vec![
            (
                ExpressionKind::Pitch,
                vec![pt(0.0, 0.0), pt(len * 0.4, 0.0), pt(len, glide)],
            ),
            (
                ExpressionKind::Pressure,
                vec![pt(0.0, 0.2), pt(len * 0.5, 0.9), pt(len, 0.4)],
            ),
        ];
        if !mpe {
            curves.push((
                ExpressionKind::Volume,
                vec![pt(0.0, -18.0), pt(len * 0.6, 0.0)],
            ));
            let side = if i % 2 == 0 { 1.0 } else { -1.0 };
            curves.push((
                ExpressionKind::Pan,
                vec![pt(0.0, -0.8 * side), pt(len, 0.8 * side)],
            ));
        }
        for (kind, points) in curves {
            a.dispatch(Action::SetNoteExpression {
                clip,
                note: n.id,
                kind,
                from: MusicalTime::ZERO,
                to: n.length + MusicalTime(1),
                points,
            });
        }
    }
}

fn section_op(app: &Rc<AppState>, arg: &str, copy: bool) {
    let Some((n, at)) = arg.split_once('@') else {
        return tracing::warn!("section: expected <n>@<quarters>, got '{arg}'");
    };
    let (Ok(n), Ok(at)) = (n.parse::<usize>(), at.parse::<f64>()) else {
        return tracing::warn!("section: expected <n>@<quarters>, got '{arg}'");
    };
    let section = app.session.borrow().project().sections.get(n).map(|s| s.id);
    if let Some(section) = section {
        app.dispatch(Action::MoveSection {
            section,
            to: faderframe_timeline::MusicalTime::from_quarters(at),
            copy,
        });
    }
}

/// `<track name>@<scene number>` as a slot.
fn slot_by_name(
    a: &AppState,
    slot: &str,
) -> Option<(faderframe_core::TrackId, faderframe_core::SceneId)> {
    let (name, n) = slot.split_once('@')?;
    let s = a.session.borrow();
    let p = s.project();
    let track = p.tracks.iter().find(|t| t.name == name)?.id;
    let scene = n
        .parse::<usize>()
        .ok()
        .and_then(|n| p.launcher.scenes.get(n.saturating_sub(1)))?
        .id;
    Some((track, scene))
}
