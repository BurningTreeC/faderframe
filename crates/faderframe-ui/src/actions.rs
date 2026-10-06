//! Application actions (menus, buttons, shortcuts).

use crate::state::AppState;
use faderframe_project::TrackKind;
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
            crate::dialogs::confirm_discard(a, |a| {
                a.with_session(|s| s.new_project(false));
            })
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
        entry(app, "import-midi", crate::dialogs::import_midi),
        entry(app, "export-midi", crate::dialogs::export_midi),
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
            if tracks.is_empty() {
                a.report(
                    faderframe_session::SessionError::Other(
                        "select a track to save as a preset".into(),
                    ),
                    false,
                );
            }
            for track in tracks {
                a.dispatch(Action::SaveTrackPreset { track });
            }
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
        dispatch(app, "loop", A::Transport(T::ToggleLoop)),
        dispatch(app, "record", A::Transport(T::ToggleRecord)),
        dispatch(app, "capture-midi", A::CaptureMidi),
        dispatch(app, "save-version", A::PromptSaveVersion),
        entry(app, "command-palette", crate::palette::open),
        entry(app, "shortcuts", crate::palette::shortcuts),
        dispatch(app, "show-versions", A::ShowVersions),
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
        // Development aid: `version:<name>` saves a version.
        named("version", |a, arg| {
            a.dispatch(Action::SaveVersion {
                name: arg.to_string(),
            });
        }),
        // Development aid: `alias-clip:<clip name>` (an alias right after it).
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
        // Presets (menus): `save-preset:<plugin id>`, `load-preset:<id>\n<path>`.
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
                "render ahead {:?}: {tracks} tracks, {late} late blocks",
                s.render_ahead()
            );
        }),
        // Development aid: `plugin-precision:<0|1>` (64-bit processing).
        named("plugin-precision", |a, arg| {
            let on = arg.trim() == "1";
            a.with_session(|s| s.set_plugin_double_precision(on));
        }),
        // Development aids: `import-midi-from:<path>` / `export-midi-to:<path>`.
        named("import-midi-from", |a, arg| {
            let empty = a.session.borrow().project().clips.is_empty();
            a.dispatch(Action::ImportMidiFile {
                path: std::path::PathBuf::from(arg),
                at: faderframe_timeline::MusicalTime::ZERO,
                tempo: empty,
            });
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
pub fn install_window_keys(app: &Rc<AppState>, window: &impl IsA<gtk::Widget>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Bubble);
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
