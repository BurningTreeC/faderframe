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

#[test]
fn health_findings_name_their_device_and_references_hand_over() {
    use faderframe_project::PluginRef;
    use faderframe_session::references::ReferenceOp;
    let (mut s, vocal, _) = session();
    // A Utility at rest on the vocal: it changes nothing.
    s.dispatch(Action::InsertPlugin {
        track: vocal,
        index: 0,
        plugin: PluginRef::builtin(faderframe_core::builtin::GAIN, "Utility"),
    })
    .unwrap();
    let utility = s.project().track(vocal).unwrap().inserts[0].id;
    // A reference: a louder record of a tone.
    let dir = std::env::temp_dir().join(format!("ff-view-mix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Record.wav");
    let n = 6 * SR as usize;
    let x: Vec<f32> = (0..n)
        .map(|i| (0.8 * (std::f64::consts::TAU * 80.0 * i as f64 / f64::from(SR)).sin()) as f32)
        .collect();
    faderframe_audio_files::write_wav(
        &file,
        &[x.clone(), x],
        SR,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )
    .unwrap();
    s.dispatch(Action::Reference(ReferenceOp::Add(vec![file])))
        .unwrap();
    assert!(s.wait_for_compare(std::time::Duration::from_secs(300)));
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    s.wait_for_mix();
    let mut view = MixView::new(Theme::default());
    // The health check: the device by its name, in the list and details.
    let health = s
        .mix_findings()
        .into_iter()
        .find(|f| f.rule == faderframe_mix_intelligence::RuleId::DeviceNoChange)
        .unwrap()
        .clone();
    assert_eq!(health.device(), Some(utility));
    s.dispatch(Action::Mix(MixOp::Select(Some(health.id))))
        .unwrap();
    paint(&mut view, &s);
    let _ = click(&mut view, &s, Hit::Category(Some(Category::Health)));
    let p = paint(&mut view, &s);
    let texts = p.texts();
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("Utility on Vocal changes nothing")),
        "{texts:?}"
    );
    assert!(texts.contains(&"Utility"), "the device evidence by name");
    // A reference finding: its hand-offs are buttons asking the session.
    let reference = s
        .mix_findings()
        .into_iter()
        .find(|f| f.category() == Category::Reference)
        .unwrap()
        .clone();
    s.dispatch(Action::Mix(MixOp::Select(Some(reference.id))))
        .unwrap();
    let _ = click(&mut view, &s, Hit::Category(Some(Category::Reference)));
    let p = paint(&mut view, &s);
    assert!(p.texts().contains(&"Listen A/B"), "{:?}", p.texts());
    let i = reference
        .recommendations
        .iter()
        .position(|r| r.experiment.hand_off().is_some())
        .unwrap();
    let (a, _) = click(&mut view, &s, Hit::Open(reference.id, i));
    assert_eq!(
        a,
        [Action::Mix(MixOp::Open {
            finding: reference.id,
            recommendation: i
        })]
    );
    // The vocal's device changes: its findings are marked stale.
    s.dispatch(Action::Edit(Command::SetPluginBypass {
        track: vocal,
        plugin: utility,
        bypass: true,
    }))
    .unwrap();
    s.tick(0.016);
    s.dispatch(Action::Mix(MixOp::Select(Some(health.id))))
        .unwrap();
    let _ = click(&mut view, &s, Hit::Category(Some(Category::Health)));
    let p = paint(&mut view, &s);
    assert!(
        p.texts().iter().any(|t| t.starts_with("Stale:")),
        "{:?}",
        p.texts()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two microphones on one snare, the bottom one inverted and 3 dB down.
fn snares() -> (Session, TrackId, TrackId) {
    let mut p = Project::new("Phase", SR);
    let mut add = |name: &str| -> TrackId {
        let src = p.ids.allocate();
        p.sources.insert(
            src,
            AudioSource {
                id: src,
                name: name.into(),
                spec: SourceSpec::Generated {
                    generator: GeneratorSpec::DrumLoop {
                        bpm: 120.0,
                        bars: 4,
                        seed: 9,
                    },
                },
            },
        );
        let id: TrackId = p.ids.allocate();
        let mut t = Track::new(id, TrackKind::Audio, name, TrackColor::palette(3))
            .with_layout(faderframe_core::ChannelLayout::Stereo);
        t.role = Some(TrackRole::Snare);
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
                length: 8 * i64::from(SR),
                gain_db: -6.0,
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
    };
    let a = add("Top");
    let b = add("Bottom");
    if let Some(t) = p.tracks.iter_mut().find(|t| t.id == b) {
        t.phase_invert = true;
        t.volume_db = -3.0;
    }
    (
        Session::new(p, None, EngineConfig::default()).unwrap(),
        a,
        b,
    )
}

#[test]
fn phase_findings_show_their_chart_and_filter_by_category() {
    use faderframe_mix_intelligence::RuleId;
    let (mut s, _, _) = snares();
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    s.wait_for_mix();
    let f = s
        .mix_findings()
        .into_iter()
        .find(|f| f.rule == RuleId::PolarityInverted)
        .unwrap()
        .clone();
    s.dispatch(Action::Mix(MixOp::Select(Some(f.id)))).unwrap();
    let mut view = MixView::new(Theme::default());
    let p = paint(&mut view, &s);
    let texts = p.texts();
    // Evidence with its units, the chart's legend, the experiment.
    assert!(
        texts.contains(&"Delay of the second track (Bottom)"),
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t.ends_with(" ms")));
    for legend in ["Sum now", "Corrected", "In mono"] {
        assert!(texts.contains(&legend), "{legend}: {texts:?}");
    }
    assert!(
        texts
            .iter()
            .any(|t| t.contains("polarity inverted") || t.contains("earlier")),
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t.contains("Phase and Timing")));
    let i = f
        .recommendations
        .iter()
        .position(|r| r.experiment.previewable())
        .unwrap();
    assert_eq!(view.enabled(Hit::Audition(f.id, i)), Some(true));
    // The status line counts the pairs compared.
    assert!(
        texts.iter().any(|t| t.contains("1 pair compared")),
        "{texts:?}"
    );
    // Only phase and timing: the other categories' rows go.
    let _ = click(&mut view, &s, Hit::Category(Some(Category::Phase)));
    paint(&mut view, &s);
    for g in s.mix_findings() {
        let shown = view.rect_of(Hit::Row(g.id)).is_some();
        assert_eq!(shown, g.category() == Category::Phase, "{:?}", g.rule);
    }
    let _ = click(&mut view, &s, Hit::Category(Some(Category::Dynamics)));
    paint(&mut view, &s);
    assert!(view.rect_of(Hit::Row(f.id)).is_none());
}

#[test]
fn the_levels_panel_shows_dynamics_width_and_the_pairs_compared() {
    let (mut s, _, _) = snares();
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    s.wait_for_mix();
    let mut view = MixView::new(Theme::default());
    paint(&mut view, &s);
    let _ = click(&mut view, &s, Hit::Levels);
    let p = paint(&mut view, &s);
    let texts = p.texts();
    for t in [
        "Dynamics",
        "TSR",
        "Δ TSR",
        "Stereo width by region (side − mid, dB)",
        "Mono fold-down change by region (dB)",
        "low mids",
        "Pairs compared (phase and timing)",
        "Top × Bottom",
    ] {
        assert!(texts.iter().any(|x| x.contains(t)), "{t}: {texts:?}");
    }
}
