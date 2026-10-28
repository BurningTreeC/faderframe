#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_audio_files::GeneratorSpec;
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Project, SourceSpec, StretchSettings,
    Track, TrackColor, TrackKind,
};
use faderframe_ui_canvas::{Modifiers, RecordingPainter};

const SR: u32 = 48_000;

fn sine(p: &mut Project, name: &str, hz: f64, amp: f32) -> TrackId {
    let src = p.ids.allocate();
    p.sources.insert(
        src,
        AudioSource {
            id: src,
            name: name.into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::Sine {
                    frequency: hz,
                    seconds: 6.0,
                    amplitude: 0.7,
                },
            },
        },
    );
    let id: TrackId = p.ids.allocate();
    let mut t = Track::new(id, TrackKind::Audio, name, TrackColor::palette(2));
    let clip = Clip {
        id: p.ids.allocate(),
        track: id,
        name: name.into(),
        color: None,
        start: faderframe_timeline::MusicalTime::ZERO,
        muted: false,
        content: ClipContent::Audio(AudioClip {
            source: src,
            source_offset: 0,
            length: 6 * i64::from(SR),
            gain_db: 20.0 * (amp / 0.7).log10(),
            fades: ClipFades::default(),
            stretch: StretchSettings::Off,
            reversed: false,
            warp: None,
            pitch: None,
            effects: None,
            spectral: None,
        }),
    };
    t.clips.push(clip.id);
    p.clips.insert(clip.id, clip);
    p.tracks.push(t);
    id
}

fn session() -> (Session, TrackId, TrackId) {
    let mut p = Project::new("View", SR);
    let a = sine(&mut p, "Vocal", 80.0, 0.3);
    let b = sine(&mut p, "Keys", 84.0, 0.3);
    (
        Session::new(p, None, EngineConfig::default()).unwrap(),
        a,
        b,
    )
}

/// Tall enough for a finding's details without scrolling.
const SIZE: Size = Size {
    w: 1200.0,
    h: 2400.0,
};

fn paint(view: &mut MixView, s: &Session) -> RecordingPainter {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, s, &Theme::default());
    p
}

fn click(view: &mut MixView, s: &Session, hit: Hit) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let r = view
        .rect_of(hit)
        .unwrap_or_else(|| panic!("{hit:?} not painted"));
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(
        &ViewEvent::PointerDown {
            pos: r.center(),
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        },
        SIZE,
        s,
        &mut cx,
    );
    (actions, requests)
}

#[test]
fn analysis_controls_ask_the_session() {
    let (s, _, _) = session();
    let mut view = MixView::new(Theme::default());
    let p = paint(&mut view, &s);
    let texts = p.texts();
    assert!(texts.contains(&"Analyse Project"));
    assert!(texts.iter().any(|t| t.starts_with("Nothing analysed yet")));
    // Without a range or tracks selected those buttons are off.
    assert_eq!(view.enabled(Hit::Analyse(MixScope::Selection)), Some(false));
    assert_eq!(view.enabled(Hit::Cancel), Some(false));
    let (a, _) = click(&mut view, &s, Hit::Analyse(MixScope::Project));
    assert_eq!(
        a,
        [Action::Mix(MixOp::Analyse {
            scope: MixScope::Project,
            signal: SignalPoint::Processed
        })]
    );
    // A disabled button does nothing.
    let (a, _) = click(&mut view, &s, Hit::Analyse(MixScope::Selection));
    assert!(a.is_empty());
    // Before processing.
    let _ = click(&mut view, &s, Hit::Signal(SignalPoint::Source));
    paint(&mut view, &s);
    let (a, _) = click(&mut view, &s, Hit::Analyse(MixScope::Project));
    assert_eq!(
        a,
        [Action::Mix(MixOp::Analyse {
            scope: MixScope::Project,
            signal: SignalPoint::Source
        })]
    );
}

#[test]
fn findings_show_their_evidence_and_experiments() {
    let (mut s, vocal, _) = session();
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    let mut view = MixView::new(Theme::default());
    // Running: progress in the status line, Cancel on.
    let p = paint(&mut view, &s);
    if s.mix_analysing() {
        assert_eq!(view.enabled(Hit::Cancel), Some(true));
        assert!(p.texts().iter().any(|t| t.contains('%')));
    }
    s.wait_for_mix();
    let p = paint(&mut view, &s);
    let texts = p.texts();
    let f = s.mix_selected().unwrap().clone();
    let title = f.title(&|t| s.mix_track_name(t));
    assert!(texts.iter().any(|t| *t == title), "{texts:?}");
    for heading in [
        "Measured",
        "Interpretation",
        "Limitations and caveats",
        "Experiments (proposed, not applied)",
    ] {
        assert!(texts.contains(&heading), "{heading}");
    }
    assert!(texts.iter().any(|t| t.starts_with("Time covered")));
    assert!(texts.iter().any(|t| t.contains("Processed (as mixed)")));
    // A row: selected and shown on the arranger.
    let (a, _) = click(&mut view, &s, Hit::Row(f.id));
    assert_eq!(
        a,
        [
            Action::Mix(MixOp::Select(Some(f.id))),
            Action::Mix(MixOp::Show(f.id))
        ]
    );
    for x in a {
        s.dispatch(x).unwrap();
    }
    assert!(s.selection.tracks.contains(&vocal));
    // Audition the first experiment: A/B, matching and Reject appear.
    let i = f
        .recommendations
        .iter()
        .position(|r| r.experiment.previewable())
        .unwrap();
    let (a, _) = click(&mut view, &s, Hit::Audition(f.id, i));
    assert_eq!(
        a,
        [Action::Mix(MixOp::Audition {
            finding: f.id,
            recommendation: i
        })]
    );
    s.dispatch(a[0].clone()).unwrap();
    s.wait_for_mix_measure();
    let p = paint(&mut view, &s);
    assert!(p.texts().contains(&"B: Candidate"));
    assert!(
        p.texts().iter().any(|t| t.contains("LU")),
        "{:?}",
        p.texts()
    );
    let (a, _) = click(&mut view, &s, Hit::Compare(false));
    assert_eq!(a, [Action::Mix(MixOp::Compare(false))]);
    let (a, _) = click(&mut view, &s, Hit::Reject);
    assert_eq!(a, [Action::Mix(MixOp::Reject)]);
    s.dispatch(a[0].clone()).unwrap();
    assert!(s.mix_audition().is_none());
    // Dismissed: gone from the list, a footer to show it again.
    let (a, _) = click(&mut view, &s, Hit::Dismiss(f.id));
    s.dispatch(a[0].clone()).unwrap();
    let p = paint(&mut view, &s);
    assert!(view.rect_of(Hit::Row(f.id)).is_none());
    assert!(p.texts().iter().any(|t| t.contains("dismissed")));
    // Filters: no level findings here.
    let _ = click(&mut view, &s, Hit::Category(Some(Category::Levels)));
    let p = paint(&mut view, &s);
    assert!(p.texts().iter().any(|t| t.starts_with("No findings match")));
    // An edit makes the results stale: said in the status line.
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: vocal,
        db: -2.0,
    }))
    .unwrap();
    s.tick(0.016);
    let p = paint(&mut view, &s);
    assert!(
        p.texts()
            .iter()
            .any(|t| t.starts_with("The project changed"))
    );
}

#[test]
fn roles_are_chosen_from_a_menu() {
    let (s, vocal, _) = session();
    let mut view = MixView::new(Theme::default());
    paint(&mut view, &s);
    let _ = click(&mut view, &s, Hit::Roles);
    let p = paint(&mut view, &s);
    assert!(p.texts().contains(&"Vocal") && p.texts().contains(&"No role"));
    let (_, requests) = click(&mut view, &s, Hit::Role(vocal));
    let Some(HostRequest::ContextMenu { items, .. }) = requests.first() else {
        panic!("a menu");
    };
    let lead = items.iter().find(|i| i.label == "Lead Vocal").unwrap();
    assert_eq!(
        lead.action,
        Some(Action::Edit(Command::SetTrackRole {
            track: vocal,
            role: Some(TrackRole::LeadVocal)
        }))
    );
    assert_eq!(items[0].checked, Some(true), "no role yet");
    // Accessible: the buttons and the (empty) list.
    let nodes = view.accessible(SIZE, &s);
    assert!(!nodes.is_empty());
}

#[test]
fn the_levels_panel_lists_every_track_and_the_master() {
    let (mut s, _, _) = session();
    let mut view = MixView::new(Theme::default());
    paint(&mut view, &s);
    assert_eq!(
        view.enabled(Hit::Levels),
        Some(false),
        "nothing measured yet"
    );
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    s.wait_for_mix();
    paint(&mut view, &s);
    let _ = click(&mut view, &s, Hit::Levels);
    let p = paint(&mut view, &s);
    let texts = p.texts();
    for t in ["Vocal", "Keys", "Master (mix)", "True peak", "LUFS"] {
        assert!(texts.contains(&t), "{t}: {texts:?}");
    }
    // Two tones at 0.3, centred (−3 dB): each peaks at −13.5 dBFS.
    assert!(texts.contains(&"-13.5"), "{texts:?}");
}

#[test]
fn places_scrolled_out_of_view_cannot_be_clicked() {
    let (mut s, _, _) = session();
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    s.wait_for_mix();
    let f = s.mix_selected().unwrap().id;
    let mut view = MixView::new(Theme::default());
    let small = Size::new(1200.0, 500.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, small, &s, &Theme::default());
    // The experiments are below the fold: not clickable until scrolled to.
    assert!(view.rect_of(Hit::Audition(f, 0)).is_none());
    for r in &view.spots {
        assert!(r.rect.bottom() <= small.h + 0.5, "{:?}", r.hit);
    }
    view.set_scroll(ScrollAxis::Vertical, 10_000.0);
    let mut p = RecordingPainter::new();
    view.paint(&mut p, small, &s, &Theme::default());
    let r = view.rect_of(Hit::Audition(f, 0)).unwrap();
    assert!(r.y >= HEADER_H + STATUS_H, "under the header: {r:?}");
}
