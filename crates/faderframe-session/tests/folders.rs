//! Folder tracks: they hold tracks, open and close, keep their tracks
//! when removed, and become buses summing what they hold on request.
#![allow(clippy::unwrap_used)]

use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, OutputRouting, TrackKind};
use faderframe_session::{Action, SelectMode, Session};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

/// The demo without the folder it comes with (its tracks stay).
fn demo() -> Session {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let folder = track(&s, "Arpeggios");
    s.dispatch(Action::Edit(Command::RemoveTrack { track: folder }))
        .unwrap();
    s
}

fn order(s: &Session) -> Vec<String> {
    s.project()
        .folder_order()
        .iter()
        .map(|t| t.name.clone())
        .collect()
}

#[test]
fn a_folder_holds_the_selected_tracks_and_keeps_them_when_removed() {
    let mut s = demo();
    let (bass, pad) = (track(&s, "Bass"), track(&s, "Pad"));
    s.dispatch(Action::NewFolder {
        tracks: vec![pad, bass],
    })
    .unwrap();
    let folder = track(&s, "Folder 1");
    let f = s.project().track(folder).unwrap();
    assert_eq!(f.kind, TrackKind::Folder);
    for t in [bass, pad] {
        assert_eq!(s.project().track(t).unwrap().folder, Some(folder));
    }
    // Shown right under it, in project order, where the first one was.
    let names = order(&s);
    let at = names.iter().position(|n| n == "Folder 1").unwrap();
    assert_eq!(names[at + 1..at + 3], ["Bass", "Pad"]);
    assert_eq!(s.folder_contents(folder), [bass, pad]);
    // Closed: its tracks are hidden in the editors.
    assert!(s.folder_open(folder));
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    assert!(!s.track_shown(s.project().track(bass).unwrap()));
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    // A folder in the folder; it cannot go into itself.
    s.dispatch(Action::NewFolder { tracks: vec![pad] }).unwrap();
    let inner = track(&s, "Folder 2");
    assert_eq!(s.project().track(inner).unwrap().folder, Some(folder));
    assert!(
        s.project()
            .in_folder(s.project().track(pad).unwrap(), folder)
    );
    assert!(
        s.dispatch(Action::Edit(Command::SetTrackFolder {
            track: folder,
            folder: Some(inner),
        }))
        .is_err()
    );
    // Out of the inner folder: a level up, into the outer one.
    s.dispatch(Action::MoveToFolder {
        tracks: vec![pad],
        folder: None,
    })
    .unwrap();
    assert_eq!(s.project().track(pad).unwrap().folder, Some(folder));
    // Removing the outer folder keeps its tracks (one step).
    s.dispatch(Action::Edit(Command::RemoveTrack { track: folder }))
        .unwrap();
    assert!(s.project().track(folder).is_none());
    for t in [bass, pad, inner] {
        assert_eq!(s.project().track(t).unwrap().folder, None, "{t}");
    }
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track(bass).unwrap().folder, Some(folder));
    assert_eq!(s.project().track(inner).unwrap().folder, Some(folder));
}

/// A folder becomes a bus (one step, undone and redone): its kind and
/// output, the master's channel format, and the tracks it holds playing
/// through it; "Sum Folder" now does the same (no separate bus).
#[test]
fn a_folder_becomes_a_bus_and_back() {
    let mut s = demo();
    let (pluck, pad, lead) = (
        track(&s, "Pluck"),
        track(&s, "Pad"),
        track(&s, "Lead Synth"),
    );
    s.selection
        .select_tracks(&[pluck, pad, lead], SelectMode::Replace);
    let choices = s.add_track_choices();
    let (_, new, _) = choices
        .iter()
        .find(|c| c.0 == "Folder with the Selected Tracks")
        .unwrap();
    s.dispatch(new.clone()).unwrap();
    let folder = track(&s, "Folder 1");
    let tracks_before = s.project().tracks.len();
    let steps = s.history_steps().0.len();
    let to_master: Vec<TrackId> = [pluck, pad, lead]
        .into_iter()
        .filter(|t| s.project().track(*t).unwrap().output == OutputRouting::Master)
        .collect();
    assert!(!to_master.is_empty());
    s.dispatch(Action::SetFolderBus { folder, on: true })
        .unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1, "one undo step");
    assert_eq!(s.project().tracks.len(), tracks_before, "no separate bus");
    let p = s.project();
    let f = p.track(folder).unwrap();
    assert!(f.is_folder_bus() && f.is_folder());
    assert_eq!(f.kind, TrackKind::Bus);
    assert_eq!(f.output, OutputRouting::Master);
    assert_eq!(f.layout, p.master().unwrap().layout);
    assert_eq!(p.output_target(f), p.master_id());
    for t in &to_master {
        assert_eq!(p.output_target(p.track(*t).unwrap()), Some(folder));
    }
    // Still a folder: its tracks show under it, it closes.
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    assert!(!s.track_shown(s.project().track(pad).unwrap()));
    s.dispatch(Action::ToggleFolder(folder)).unwrap();
    let names: Vec<String> = s
        .folder_choices(track(&s, "Bass"))
        .into_iter()
        .map(|c| c.0)
        .collect();
    assert!(
        names.iter().any(|n| n == "Move into ‘Folder 1’"),
        "{names:?}"
    );
    // Undone: a plain folder, its tracks to the master again.
    s.dispatch(Action::Undo).unwrap();
    let p = s.project();
    assert_eq!(p.track(folder).unwrap().kind, TrackKind::Folder);
    assert_eq!(p.track(folder).unwrap().output, OutputRouting::None);
    for t in &to_master {
        assert_eq!(p.output_target(p.track(*t).unwrap()), p.master_id());
    }
    s.dispatch(Action::Redo).unwrap();
    assert!(s.project().track(folder).unwrap().is_folder_bus());
    // Plain again (its settings kept for the next time), and "Sum Folder".
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: folder,
        db: -4.0,
    }))
    .unwrap();
    s.dispatch(Action::SetFolderBus { folder, on: false })
        .unwrap();
    let f = s.project().track(folder).unwrap();
    assert!(!f.is_folder_bus() && f.kind == TrackKind::Folder);
    assert_eq!(f.volume_db, -4.0);
    s.dispatch(Action::SumFolder(folder)).unwrap();
    assert!(s.project().track(folder).unwrap().is_folder_bus());
    assert_eq!(s.project().tracks.len(), tracks_before);
}

/// A folder bus is saved and opened again as it was: the flag, what it
/// holds and where that goes.
#[test]
fn a_folder_bus_is_saved_and_opened_again() {
    let mut s = demo();
    let (bass, pad) = (track(&s, "Bass"), track(&s, "Pad"));
    s.dispatch(Action::NewFolder {
        tracks: vec![bass, pad],
    })
    .unwrap();
    let folder = track(&s, "Folder 1");
    s.dispatch(Action::SetFolderBus { folder, on: true })
        .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackPan {
        track: folder,
        pan: -0.25,
    }))
    .unwrap();
    let dir = std::env::temp_dir().join(format!("ff-folder-bus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("folder-bus.ffproj");
    s.save_as(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("folder_bus").count(), 1, "written once");
    let mut t = Session::demo(EngineConfig::default()).unwrap();
    t.open(&path).unwrap();
    let p = t.project();
    let f = p.track(folder).unwrap();
    assert!(f.is_folder_bus());
    assert_eq!(f.pan, -0.25);
    for id in [bass, pad] {
        let x = p.track(id).unwrap();
        assert_eq!(x.folder, Some(folder));
        if x.output == OutputRouting::Master {
            assert_eq!(p.output_target(x), Some(folder));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// With a console set, a folder made a bus gets the console's bus
/// amplifier as buses do; moving a track into a folder bus that would
/// loop is refused.
#[test]
fn a_folder_bus_sums_through_the_console_and_never_loops() {
    let mut s = demo();
    s.dispatch(Action::SetConsole { family: Some(2) }).unwrap();
    let bass = track(&s, "Bass");
    s.dispatch(Action::NewFolder { tracks: vec![bass] })
        .unwrap();
    let folder = track(&s, "Folder 1");
    let steps = s.history_steps().0.len();
    s.dispatch(Action::SetFolderBus { folder, on: true })
        .unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1);
    let amp = s.project().track(folder).unwrap().preamp.clone();
    assert!(
        amp.is_some_and(|a| faderframe_core::builtin::is_input_stage(&a.plugin.id)),
        "the console's bus amplifier"
    );
    // A bus the folder bus feeds by name cannot go into it.
    let sub = s.add_track(TrackKind::Bus).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackOutput {
        track: folder,
        output: OutputRouting::Track { track: sub },
    }))
    .unwrap();
    assert!(
        s.dispatch(Action::MoveToFolder {
            tracks: vec![sub],
            folder: Some(folder),
        })
        .is_err()
    );
    assert_eq!(s.project().track(sub).unwrap().folder, None);
}

/// Renders: a folder bus is a stem of its own (what it holds through
/// it), a track in it is rendered through it, and a track render (freeze,
/// bounce) goes straight to the master past it.
#[test]
fn a_folder_bus_renders_as_a_stem_and_tracks_render_past_it() {
    use faderframe_audio_files::{GeneratorSpec, WavFormat, read_wav};
    use faderframe_project::{
        AudioClip, AudioSource, Clip, ClipContent, ClipFades, Project, SourceSpec, StretchSettings,
        Track, TrackColor,
    };
    use faderframe_session::render::{
        self, RenderChannels, RenderRange, RenderSettings, RenderSource,
    };
    use faderframe_timeline::MusicalTime;
    let mut p = Project::new("Stems", 48_000);
    let add = |p: &mut Project, kind, name: &str| {
        let id: TrackId = p.ids.allocate();
        let at = p.tracks.len() - 1;
        p.tracks
            .insert(at, Track::new(id, kind, name, TrackColor::palette(0)));
        id
    };
    let f = add(&mut p, TrackKind::Bus, "Keys");
    let a = add(&mut p, TrackKind::Audio, "Organ");
    let b = add(&mut p, TrackKind::Audio, "Bass");
    {
        let t = p.track_mut(f).unwrap();
        t.folder_bus = true;
        t.volume_db = -6.020_6;
    }
    p.track_mut(a).unwrap().folder = Some(f);
    for (t, frequency) in [(a, 440.0), (b, 110.0)] {
        let source: faderframe_core::AudioSourceId = p.ids.allocate();
        p.sources.insert(
            source,
            AudioSource {
                id: source,
                name: "sine".into(),
                spec: SourceSpec::Generated {
                    generator: GeneratorSpec::Sine {
                        frequency,
                        seconds: 4.0,
                        amplitude: 0.5,
                    },
                },
            },
        );
        let clip: faderframe_core::ClipId = p.ids.allocate();
        p.clips.insert(
            clip,
            Clip {
                id: clip,
                track: t,
                name: "sine".into(),
                color: None,
                start: MusicalTime::ZERO,
                muted: false,
                content: ClipContent::Audio(AudioClip {
                    source,
                    source_offset: 0,
                    length: 4 * 48_000,
                    gain_db: 0.0,
                    fades: ClipFades::default(),
                    stretch: StretchSettings::Off,
                    reversed: false,
                    warp: None,
                    pitch: None,
                    effects: None,
                    spectral: None,
                }),
            },
        );
        p.track_mut(t).unwrap().clips.push(clip);
    }
    let dir = std::env::temp_dir().join(format!("ff-folder-stems-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let settings = RenderSettings {
        range: RenderRange::Bars { start: 0, end: 1 },
        source: RenderSource::Stems,
        channels: RenderChannels::Stereo,
        tail_seconds: 0.0,
        normalize_db: None,
        format: WavFormat::Float32,
        ..RenderSettings::defaults_for(&p, dir.clone())
    };
    let files = render::start(p.clone(), settings).unwrap().join().unwrap();
    assert_eq!(files.len(), 3, "a stem a track (the folder bus too)");
    let read = |name: &str| {
        read_wav(&dir.join(format!("Stems - {name}.wav")))
            .unwrap()
            .channels
    };
    let peak = |x: &[f32]| x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let (keys, organ, bass) = (read("Keys"), read("Organ"), read("Bass"));
    // The bass straight to the master; the folder bus's stem the organ
    // (as loud a sine) through it at −6 dB, no bass; the organ's stem
    // through its folder bus: the same samples.
    let (b, k) = (peak(&bass[0]), peak(&keys[0]));
    assert!(b > 0.3, "{b}");
    assert!((k - 0.5 * b).abs() < 2e-3, "{k} vs {b}");
    assert_eq!(keys, organ);
    let _ = std::fs::remove_dir_all(&dir);
    // A track render goes past the folder bus to the master.
    let (copy, _, _) = render::track_render_project(&p, a).unwrap();
    let t = copy.track(a).unwrap();
    assert_eq!(copy.output_target(t), copy.master_id());
}

#[test]
fn the_track_menu_offers_folders() {
    let mut s = demo();
    let (bass, pad) = (track(&s, "Bass"), track(&s, "Pad"));
    s.dispatch(Action::NewFolder { tracks: vec![bass] })
        .unwrap();
    let labels: Vec<String> = s.folder_choices(pad).into_iter().map(|c| c.0).collect();
    assert_eq!(
        labels,
        ["New Folder with the Track", "Move into ‘Folder 1’"]
    );
    let labels: Vec<String> = s.folder_choices(bass).into_iter().map(|c| c.0).collect();
    assert_eq!(
        labels,
        ["New Folder with the Track", "Move out of ‘Folder 1’"]
    );
}

/// Mixer drag and drop: every strip dropped into every gap between the
/// strips (folders nested two deep, tracks at the top level between them)
/// shows exactly there afterwards — one undo step each, undone cleanly.
#[test]
fn every_strip_lands_where_it_is_dropped() {
    let mut s = Session::new(
        faderframe_project::Project::new("Order", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let add = |s: &mut Session, kind| s.add_track(kind).unwrap();
    let a = add(&mut s, TrackKind::Audio);
    let f1 = add(&mut s, TrackKind::Folder);
    let b = add(&mut s, TrackKind::Audio);
    let c = add(&mut s, TrackKind::Audio);
    let f2 = add(&mut s, TrackKind::Folder);
    let d = add(&mut s, TrackKind::Audio);
    let e = add(&mut s, TrackKind::Audio);
    let g = add(&mut s, TrackKind::Bus);
    let h = add(&mut s, TrackKind::Audio);
    for (t, f) in [(b, f1), (c, f1), (f2, f1), (d, f2), (e, f2)] {
        s.dispatch(Action::Edit(Command::SetTrackFolder {
            track: t,
            folder: Some(f),
        }))
        .unwrap();
    }
    // What the mixer shows: folder order without folders and the master.
    let strips = |s: &Session| -> Vec<TrackId> {
        s.project()
            .folder_order()
            .into_iter()
            .filter(|t| !matches!(t.kind, TrackKind::Master | TrackKind::Folder))
            .map(|t| t.id)
            .collect()
    };
    let start = strips(&s);
    assert_eq!(start, [a, b, c, d, e, g, h]);
    let before_all = s.project().clone();
    for &t in &start {
        let others: Vec<TrackId> = start.iter().copied().filter(|x| *x != t).collect();
        for gap in 0..=others.len() {
            let after = gap.checked_sub(1).map(|i| others[i]);
            let before = others.get(gap).copied();
            let mut want = others.clone();
            want.insert(gap, t);
            let steps = s.history_steps().0.len();
            s.dispatch(Action::PlaceTrack {
                track: t,
                after,
                before,
            })
            .unwrap();
            assert_eq!(strips(&s), want, "{t:?} into gap {gap}");
            // One step (none when it was there already), and back.
            let now = s.history_steps().0.len();
            assert!(now - steps <= 1);
            if now > steps {
                s.dispatch(Action::Undo).unwrap();
            }
            assert_eq!(strips(&s), start);
            assert_eq!(s.project().tracks, before_all.tracks);
        }
    }
}

/// Arranger drag and drop: rows include the folders (open), and a folder
/// moves with what it holds. Every track and folder dropped into every gap
/// between the other rows shows exactly there afterwards — under a
/// folder's header first in it — each one undo step.
#[test]
fn every_row_lands_where_it_is_dropped_folders_included() {
    let mut s = Session::new(
        faderframe_project::Project::new("Rows", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let add = |s: &mut Session, kind| s.add_track(kind).unwrap();
    let a = add(&mut s, TrackKind::Audio);
    let f1 = add(&mut s, TrackKind::Folder);
    let b = add(&mut s, TrackKind::Audio);
    let f2 = add(&mut s, TrackKind::Folder);
    let c = add(&mut s, TrackKind::Audio);
    let d = add(&mut s, TrackKind::Audio);
    let e = add(&mut s, TrackKind::Bus);
    for (t, f) in [(b, f1), (f2, f1), (c, f2)] {
        s.dispatch(Action::Edit(Command::SetTrackFolder {
            track: t,
            folder: Some(f),
        }))
        .unwrap();
    }
    let rows = |s: &Session| -> Vec<TrackId> {
        s.project()
            .folder_order()
            .into_iter()
            .filter(|t| t.kind != TrackKind::Master)
            .map(|t| t.id)
            .collect()
    };
    let start = rows(&s);
    assert_eq!(start, [a, f1, b, f2, c, d, e]);
    let before_all = s.project().tracks.clone();
    for &t in &start {
        // The block that moves: the track, and what it holds.
        let block: Vec<TrackId> = start
            .iter()
            .copied()
            .filter(|x| {
                *x == t
                    || s.project()
                        .track(*x)
                        .is_some_and(|xt| s.project().in_folder(xt, t))
            })
            .collect();
        let others: Vec<TrackId> = start
            .iter()
            .copied()
            .filter(|x| !block.contains(x))
            .collect();
        for gap in 0..=others.len() {
            let after = gap.checked_sub(1).map(|i| others[i]);
            let before = others.get(gap).copied();
            let mut want = others.clone();
            for (k, x) in block.iter().enumerate() {
                want.insert(gap + k, *x);
            }
            let steps = s.history_steps().0.len();
            s.dispatch(Action::PlaceTrack {
                track: t,
                after,
                before,
            })
            .unwrap();
            assert_eq!(
                rows(&s),
                want,
                "{t:?} into gap {gap} ({after:?} | {before:?})"
            );
            let now = s.history_steps().0.len();
            assert!(now - steps <= 1);
            if now > steps {
                s.dispatch(Action::Undo).unwrap();
            }
            assert_eq!(s.project().tracks, before_all);
        }
    }
    // A folder never goes into itself or a folder inside it.
    s.dispatch(Action::PlaceTrack {
        track: f1,
        after: Some(f2),
        before: Some(c),
    })
    .unwrap();
    assert_eq!(s.project().tracks, before_all);
}
