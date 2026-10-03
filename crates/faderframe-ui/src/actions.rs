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
        entry(app, "import-audio", crate::dialogs::import_audio),
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
        dispatch(app, "add-instrument", A::AddTrack(TrackKind::Instrument)),
        dispatch(app, "add-midi", A::AddTrack(TrackKind::Midi)),
        dispatch(app, "add-bus", A::AddTrack(TrackKind::Bus)),
        dispatch(app, "add-aux", A::AddTrack(TrackKind::Aux)),
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
                        let target = if t.kind == TrackKind::Instrument && t.instrument.is_none() {
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
            // A status byte starts each message.
            let s = a.session.borrow();
            let mut msg: Vec<u8> = Vec::new();
            for b in bytes {
                if b >= 0x80 && !msg.is_empty() {
                    s.midi_keyboard().send(&msg);
                    msg.clear();
                }
                msg.push(b);
            }
            if !msg.is_empty() {
                s.midi_keyboard().send(&msg);
            }
        })
        .build();
    // Editing by name (menus, scripts): `edit-mode:<shuffle|slip|spot|grid>`,
    // `edit-tool:<smart|trim|stretch|select|grab|separate|scrub|pencil|zoom>`,
    // `edit-flag:<warp|transients|tab-transients|link|insertion-follows>`
    // (toggles), `edit:<separate|trim|clear|silence|copy|cut|paste|duplicate|
    // quantize|separate-transients|unwarp>`, and the development aid
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
                "quantize" => Action::QuantizeWarp(clips),
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
    // Development aid: `piano-lane:<velocity|pitch|pressure|timbre>` picks
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
            use faderframe_project::{ExpressionKind, ExpressionPoint};
            use faderframe_timeline::MusicalTime;
            let Some(a) = weak.upgrade() else { return };
            let found = {
                let s = a.session.borrow();
                s.editor_clip().and_then(|c| {
                    let clip = s.project().clip(c)?;
                    let notes: Vec<_> = clip.as_midi()?.notes.iter().take(4).copied().collect();
                    Some((c, clip.track, notes))
                })
            };
            let Some((clip, track, notes)) = found else {
                tracing::warn!("mpe-demo: no MIDI clip in the editor");
                return;
            };
            a.dispatch(Action::Edit(faderframe_project::Command::SetTrackMpe {
                track,
                mpe: Some(faderframe_project::MpeConfig::default()),
            }));
            let pt = |q: f64, value: f32| ExpressionPoint {
                time: MusicalTime::from_quarters(q),
                value,
            };
            for (i, n) in notes.iter().enumerate() {
                let len = n.length.ticks() as f64 / faderframe_timeline::TICKS_PER_QUARTER as f64;
                let glide = [2.0, -1.0, 3.0, -2.0][i % 4];
                for (kind, points) in [
                    (
                        ExpressionKind::Pitch,
                        vec![pt(0.0, 0.0), pt(len * 0.4, 0.0), pt(len, glide)],
                    ),
                    (
                        ExpressionKind::Pressure,
                        vec![pt(0.0, 0.2), pt(len * 0.5, 0.9), pt(len, 0.4)],
                    ),
                ] {
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
        })
        .build();
    app.app.add_action_entries(edit_entries);
    app.app.add_action_entries([
        insert,
        midi,
        select,
        window_size,
        lane,
        mpe_demo,
        show("show-insert", false),
        show("show-insert-params", true),
    ]);

    let accels: &[(&str, &[&str])] = &[
        ("app.new", &["<Control>n"]),
        ("app.open", &["<Control>o"]),
        ("app.save", &["<Control>s"]),
        ("app.toggle-edit-toolbar", &["<Control>e"]),
        ("app.import-audio", &["<Control>i"]),
        ("app.save-as", &["<Control><Shift>s"]),
        ("app.quit", &["<Control>q"]),
        ("app.render", &["<Control><Shift>r"]),
        ("app.preferences", &["<Control>comma"]),
        ("app.undo", &["<Control>z"]),
        ("app.redo", &["<Control><Shift>z", "<Control>y"]),
        ("app.add-audio", &["<Control>t"]),
        ("app.add-instrument", &["<Control><Shift>t"]),
        ("app.toggle-dock", &["F2"]),
        ("app.show-mixer", &["F3"]),
        ("app.show-piano-roll", &["F4"]),
        ("app.show-performance", &["F8"]),
        ("app.workspace-1", &["<Control>1"]),
        ("app.workspace-2", &["<Control>2"]),
        ("app.workspace-3", &["<Control>3"]),
        ("app.workspace-4", &["<Control>4"]),
        ("app.workspace-5", &["<Control>5"]),
    ];
    for (action, keys) in accels {
        app.app.set_accels_for_action(action, keys);
    }
}

/// Single-key transport shortcuts, handled in the bubble phase so focused
/// text fields always get their keys first.
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
            gdk::Key::space => TransportAction::TogglePlay,
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
