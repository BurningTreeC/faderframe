//! Self-freezing in the arranger: the track menu's Self-Freeze and the
//! star before a cached track's name.

use super::*;
use faderframe_ui_canvas::{DrawOp, RecordingPainter};

const SIZE: Size = Size::new(1400.0, 700.0);

/// A saved session (its cache folder in the temp folder) with one short
/// loop through a utility, self-freezing on for every chain.
fn small_session(name: &str) -> (Session, TrackId, std::path::PathBuf) {
    use faderframe_project::{
        AudioClip, AudioSource, Clip, ClipContent, ClipFades, PluginRef, PluginSlot, Project,
        SourceSpec, StretchSettings, TrackColor,
    };
    let dir =
        std::env::temp_dir().join(format!("ff-view-self-freeze-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut p = Project::new("Small", 48_000);
    let source = p.ids.allocate();
    p.sources.insert(
        source,
        AudioSource {
            id: source,
            name: "Tone".into(),
            spec: SourceSpec::Generated {
                generator: faderframe_audio_files::GeneratorSpec::Sine {
                    frequency: 220.0,
                    seconds: 1.0,
                    amplitude: 0.5,
                },
            },
        },
    );
    let id: TrackId = p.ids.allocate();
    let clip = p.ids.allocate();
    let mut t = Track::new(id, TrackKind::Audio, "Loop", TrackColor::palette(0));
    t.inserts.push(PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(faderframe_core::builtin::GAIN, "Utility"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    });
    t.clips.push(clip);
    let at = p.tracks.len() - 1;
    p.tracks.insert(at, t);
    p.clips.insert(
        clip,
        Clip {
            id: clip,
            track: id,
            name: "Tone".into(),
            color: None,
            start: faderframe_timeline::MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: 0,
                length: 48_000,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
                envelope: None,
            }),
        },
    );
    let mut s = Session::new(p, None, faderframe_engine::EngineConfig::default()).unwrap();
    s.save_as(&dir.join("small.ffproj")).unwrap();
    s.set_self_freeze(faderframe_session::self_freeze::SelfFreezeSettings {
        min_cost_percent: 0.0,
        mode: faderframe_session::self_freeze::SelfFreezeMode::Always,
        ..Default::default()
    });
    s.self_freeze_now();
    (s, id, dir)
}

#[test]
fn a_cached_track_shows_a_star_and_its_menu_keeps_it_live() {
    let (mut s, t, dir) = small_session("arranger");
    let track = s.project().track(t).unwrap().clone();
    let HostRequest::ContextMenu { items, .. } =
        ArrangerView::track_menu(&s, &track, Point::new(0.0, 0.0))
    else {
        panic!("the track menu")
    };
    let auto = items
        .into_iter()
        .find(|i| i.label == "Self-Freeze")
        .map(|i| i.children)
        .unwrap();
    assert_eq!(auto.len(), 2);
    assert_eq!(auto[1].label, "Keep Live");
    let mut view = ArrangerView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    let l = view.header_layout(&s, t, SIZE).unwrap();
    let (_, rest) = header::lock_rect(l.name);
    let name_at = |p: &RecordingPainter| {
        p.ops.iter().find_map(|o| match o {
            DrawOp::Text(text, r) if text == "Loop" => Some(r.x),
            _ => None,
        })
    };
    assert_eq!(name_at(&p), Some(l.name.x), "no star: the name first");
    assert!(s.wait_for_self_freeze(std::time::Duration::from_secs(120)));
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert_eq!(name_at(&p), Some(rest.x), "the star before the name");
    // Kept live: the star goes.
    s.dispatch(auto[1].action.clone().unwrap()).unwrap();
    s.tick(0.01);
    s.tick(0.01);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &s, &Theme::default());
    assert_eq!(name_at(&p), Some(l.name.x));
    let _ = std::fs::remove_dir_all(&dir);
}
