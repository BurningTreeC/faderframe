#![allow(clippy::unwrap_used)]

use super::*;
use crate::launcher::Scene;
use crate::{Clip, ClipContent, Command, MidiClip, Section, Track, TrackColor, TrackKind};
use faderframe_timeline::{MeterChange, TempoCurve, TempoPoint, TimeSignature};

const SR: f64 = 48_000.0;

fn q(n: i64) -> MusicalTime {
    MusicalTime::from_quarters_i(n)
}

fn state(id: u64, name: &str) -> MusicState {
    MusicState::new(
        MusicStateId(id),
        name,
        StateSource::Section {
            section: SectionId(id + 100),
        },
    )
}

fn rule(id: u64, from: Option<u64>, to: Option<u64>, sync: SyncPoint) -> TransitionRule {
    let end = |s: Option<u64>| {
        s.map_or(RuleEnd::Any, |s| RuleEnd::State {
            state: MusicStateId(s),
        })
    };
    TransitionRule {
        sync,
        ..TransitionRule::new(TransitionId(id), end(from), end(to))
    }
}

#[test]
fn the_most_specific_rule_wins_and_the_first_of_equals() {
    let mut score = InteractiveScore {
        states: vec![state(1, "Explore"), state(2, "Combat"), state(3, "Boss")],
        ..InteractiveScore::default()
    };
    // Without a rule: the next bar, from the start.
    let none = score.rule_between(Some(MusicStateId(1)), MusicStateId(2));
    assert_eq!(none.sync, SyncPoint::Bar);
    assert_eq!(none.entry, Entry::Start);
    score.rules = vec![
        rule(10, None, None, SyncPoint::Beat),
        rule(11, None, Some(2), SyncPoint::Grid { bars: 4 }),
        rule(12, Some(1), None, SyncPoint::Marker),
        rule(13, Some(1), Some(2), SyncPoint::Exit),
        rule(14, Some(1), Some(2), SyncPoint::Immediate),
    ];
    let pick = |from: Option<u64>, to: u64| {
        score
            .rule_between(from.map(MusicStateId), MusicStateId(to))
            .id
            .0
    };
    assert_eq!(
        pick(Some(1), 2),
        13,
        "both ends named, the first of the two"
    );
    assert_eq!(
        pick(Some(1), 3),
        12,
        "the source named beats the destination"
    );
    assert_eq!(pick(Some(3), 2), 11);
    assert_eq!(pick(Some(3), 1), 10);
    assert_eq!(pick(None, 2), 11, "nothing playing: rules from any state");
}

/// 120 BPM 4/4 for two bars, then 90 BPM; 3/4 from bar 4.
fn changing_timeline() -> Timeline {
    let mut tl = Timeline::default();
    tl.tempo.set_point(TempoPoint {
        position: q(8),
        bpm: 90.0,
        curve: TempoCurve::Constant,
    });
    tl.meter.set_change(MeterChange {
        bar: 4,
        signature: TimeSignature::new(3, 4).unwrap(),
    });
    tl
}

fn looping(tl: &Timeline, start: MusicalTime, end: MusicalTime) -> Placement {
    Placement::of_span(
        &StateSpan {
            start,
            end,
            loop_start: start,
            loop_end: end,
        },
        true,
        tl,
        SR,
    )
}

#[test]
fn beats_and_bars_follow_tempo_and_metre_changes() {
    let tl = changing_timeline();
    let s = |t: MusicalTime| tl.to_samples(t, SR);
    // A long section over the changes: bars 0–8.
    let place = looping(&tl, q(0), tl.meter.bar_start(8));
    let cx = SyncContext::default();
    let at = |sync: SyncPoint, pos: i64, wait: i64| {
        sync_point(&sync, &place, &tl, SR, &cx, pos, wait).unwrap()
    };
    // In bar 0 at 120 BPM: a beat is 24 000 samples, a bar 96 000.
    assert_eq!(at(SyncPoint::Bar, 10, 0), 96_000);
    assert_eq!(at(SyncPoint::Beat, 10, 0), 24_000);
    // On a bar line: that bar, unless it must wait.
    assert_eq!(at(SyncPoint::Bar, 96_000, 0), 96_000);
    assert_eq!(at(SyncPoint::Bar, 96_000, 1), s(q(8)));
    // Past the tempo change (bar 2 at 90 BPM): beats of 32 000 samples.
    let bar2 = s(q(8));
    assert_eq!(bar2, 192_000);
    assert_eq!(at(SyncPoint::Beat, bar2 + 1, 0), bar2 + 32_000);
    assert_eq!(at(SyncPoint::Bar, bar2 + 1, 0), bar2 + 4 * 32_000);
    // In 3/4 (bar 4 on): three beats a bar.
    let bar4 = s(tl.meter.bar_start(4));
    assert_eq!(at(SyncPoint::Bar, bar4 + 1, 0), bar4 + 3 * 32_000);
    assert_eq!(
        at(SyncPoint::Beat, bar4 + 3 * 32_000 - 5, 0),
        bar4 + 3 * 32_000
    );
    // Waiting: the first bar at least that far away.
    assert_eq!(at(SyncPoint::Bar, 10, 100_000), bar2);
    // Immediate: exactly as far as asked.
    assert_eq!(at(SyncPoint::Immediate, 10, 1234), 1244);
}

#[test]
fn a_looping_state_comes_round_again_for_its_sync_points() {
    let tl = Timeline::default(); // 120 BPM: a bar is 96 000 samples
    // A two-bar loop from bar 4 (a pickup bar before it from bar 3).
    let span = StateSpan {
        start: tl.meter.bar_start(3),
        end: tl.meter.bar_start(6),
        loop_start: tl.meter.bar_start(4),
        loop_end: tl.meter.bar_start(6),
    };
    let place = Placement::of_span(&span, true, &tl, SR);
    assert_eq!(
        (place.start, place.loop_start, place.loop_end),
        (288_000, 384_000, 576_000)
    );
    let cx = SyncContext::default();
    let at =
        |sync: SyncPoint, pos: i64, wait: i64| sync_point(&sync, &place, &tl, SR, &cx, pos, wait);
    // The exit is the loop's end; too near, the next time round.
    assert_eq!(at(SyncPoint::Exit, 500_000, 0), Some(576_000));
    assert_eq!(at(SyncPoint::Exit, 500_000, 76_000), Some(576_000));
    assert_eq!(at(SyncPoint::Exit, 500_000, 76_001), Some(576_000));
    // A bar near the end: the bar in the loop after the wrap (bar 5 at
    // 480 000), not the loop's start again (that is the end, waited for).
    assert_eq!(at(SyncPoint::Bar, 560_000, 20_000), Some(480_000));
    // Waiting more than a whole loop: a bar line of a later time round.
    assert_eq!(at(SyncPoint::Bar, 400_000, 400_000), Some(480_000));
    // From the pickup, into the loop.
    assert_eq!(at(SyncPoint::Bar, 300_000, 0), Some(384_000));
    // A one-shot state ends at its exit: nothing after it.
    let once = Placement::of_span(&span, false, &tl, SR);
    assert_eq!(
        sync_point(&SyncPoint::Bar, &once, &tl, SR, &cx, 560_000, 20_000),
        None
    );
    assert_eq!(
        sync_point(&SyncPoint::Exit, &once, &tl, SR, &cx, 300_000, 0),
        Some(576_000)
    );
}

#[test]
fn grids_count_from_the_state_and_markers_and_cues_are_where_they_are() {
    let tl = Timeline::default();
    let s = |bar: i32| tl.to_samples(tl.meter.bar_start(bar), SR);
    // From bar 1 to bar 9.
    let place = looping(&tl, tl.meter.bar_start(1), tl.meter.bar_start(9));
    let markers = [s(6) + 1000];
    let cues = [
        ("Hit".to_string(), 3 * 96_000 + 500),
        ("Fill".to_string(), 96_000),
    ];
    let cx = SyncContext {
        markers: &markers,
        cues: &cues,
    };
    let at = |sync: SyncPoint, pos: i64| sync_point(&sync, &place, &tl, SR, &cx, pos, 0);
    // Every four bars from bar 1: bars 1, 5, 9 (= the loop's end).
    assert_eq!(at(SyncPoint::Grid { bars: 4 }, s(1) + 1), Some(s(5)));
    assert_eq!(at(SyncPoint::Grid { bars: 4 }, s(5) + 1), Some(s(9)));
    assert_eq!(at(SyncPoint::Marker, s(2)), Some(markers[0]));
    // Past the only marker: the next time round.
    assert_eq!(at(SyncPoint::Marker, s(7)), Some(markers[0]));
    let named = |name: &str| SyncPoint::Cue { name: name.into() };
    assert_eq!(at(named("hit"), s(1)), Some(s(1) + 3 * 96_000 + 500));
    assert_eq!(at(named(""), s(1)), Some(s(2)), "any cue: the first");
    assert_eq!(at(named("nothing"), s(1)), None);
    // No markers at all: none.
    let bare = SyncContext::default();
    assert_eq!(
        sync_point(&SyncPoint::Marker, &place, &tl, SR, &bare, s(2), 0),
        None
    );
}

#[test]
fn a_repeating_scene_exits_at_the_end_of_each_time_round() {
    let tl = Timeline::default();
    // Launched at 100 000, two bars long.
    let place = Placement::repeating(100_000, 192_000);
    let cues = [("Half".to_string(), 96_000)];
    let cx = SyncContext {
        markers: &[],
        cues: &cues,
    };
    let at = |sync: SyncPoint, pos: i64| sync_point(&sync, &place, &tl, SR, &cx, pos, 0);
    assert_eq!(at(SyncPoint::Exit, 150_000), Some(292_000));
    assert_eq!(at(SyncPoint::Exit, 292_001), Some(484_000));
    assert_eq!(
        at(
            SyncPoint::Cue {
                name: "half".into()
            },
            250_000
        ),
        Some(388_000)
    );
    // Bars are the timeline's (the launcher's quantisation).
    assert_eq!(at(SyncPoint::Bar, 150_000), Some(192_000));
    assert_eq!(place.into_loop(388_000), 96_000);
}

#[test]
fn layers_follow_their_parameters_curves() {
    let (low, high) = (MusicLayerId(1), MusicLayerId(2));
    let mut intensity = ScoreParameter::new(ScoreParamId(5), "Intensity");
    intensity.curves = vec![
        LayerCurve::fade_in(high, 0.0, 1.0),
        LayerCurve {
            layer: low,
            points: vec![
                CurvePoint {
                    value: 0.0,
                    db: 0.0,
                },
                CurvePoint {
                    value: 1.0,
                    db: -6.0,
                },
            ],
        },
    ];
    let score = InteractiveScore {
        layers: vec![
            MusicLayer {
                id: low,
                name: "Pads".into(),
                tracks: vec![TrackId(7)],
            },
            MusicLayer {
                id: high,
                name: "Drums".into(),
                tracks: vec![TrackId(7), TrackId(8)],
            },
        ],
        parameters: vec![intensity],
        ..InteractiveScore::default()
    };
    // At the default (0): the drums silent.
    let gains = score.layer_gains(&BTreeMap::new());
    assert_eq!(gains[&high], FLOOR_DB);
    assert_eq!(gains[&low], 0.0);
    // Half way: the fade is in amplitude (half the gain, −6 dB).
    let mut values = BTreeMap::new();
    values.insert(ScoreParamId(5), 0.5);
    let gains = score.layer_gains(&values);
    assert!((gains[&high] - -6.0206).abs() < 0.01, "{gains:?}");
    let tracks = score.track_gains(&values);
    assert!((tracks[&TrackId(8)] - 0.5).abs() < 1e-4);
    // A track in two layers takes both.
    let pads = faderframe_core::db_to_gain(gains[&low]);
    assert!((tracks[&TrackId(7)] - 0.5 * pads).abs() < 1e-4);
    // Out of range: held at the ends.
    values.insert(ScoreParamId(5), 7.0);
    assert!((score.layer_gains(&values)[&high]).abs() < 1e-4);
    // Sampled for middleware: the ends kept, points between on the curve.
    let curve = &score.parameters[0].curves[0];
    let pts = curve.sampled(4);
    assert_eq!(pts.len(), 5);
    assert_eq!(pts[0].db, FLOOR_DB);
    assert_eq!(pts[4].db, 0.0);
    assert!((pts[2].db - -6.0206).abs() < 0.01);
}

#[test]
fn sync_points_read_back_as_written() {
    for s in SyncPoint::COMMON {
        assert_eq!(SyncPoint::parse(&s.short()), Some(s.clone()), "{s:?}");
    }
    assert_eq!(
        SyncPoint::parse("cue=Downbeat"),
        Some(SyncPoint::Cue {
            name: "Downbeat".into()
        })
    );
    assert_eq!(SyncPoint::parse("8bars"), Some(SyncPoint::Grid { bars: 8 }));
    assert_eq!(SyncPoint::parse("sometime"), None);
}

fn project_with_states() -> Project {
    let mut p = Project::new("Game", 48_000);
    let section = p.ids.allocate();
    p.sections.push(Section {
        id: section,
        name: "Explore".into(),
        start: q(16),
        end: q(32),
        color: TrackColor::palette(0),
        time_base: Default::default(),
    });
    let track: TrackId = p.ids.allocate();
    p.tracks.insert(
        0,
        Track::new(track, TrackKind::Instrument, "Keys", TrackColor::palette(1)),
    );
    let scene: SceneId = p.ids.allocate();
    p.launcher.scenes.push(Scene {
        id: scene,
        name: "Fight".into(),
    });
    let clip = p.ids.allocate();
    p.clips.insert(
        clip,
        Clip {
            id: clip,
            track,
            name: "Riff".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: q(8),
                ..MidiClip::default()
            }),
        },
    );
    p.launcher.slots.insert(SlotKey { track, scene }, clip);
    p
}

#[test]
fn spans_come_from_sections_and_scenes() {
    let p = project_with_states();
    let section = p.sections[0].id;
    let scene = p.launcher.scenes[0].id;
    let mut s = MusicState::new(MusicStateId(1), "Explore", StateSource::Section { section });
    s.entry = q(4);
    let span = p.state_span(&s).unwrap();
    assert_eq!(
        (span.start, span.loop_start, span.loop_end, span.end),
        (q(16), q(20), q(32), q(32))
    );
    // An exit past the end is held at it; one before the entry dropped.
    s.exit = Some(q(40));
    assert_eq!(p.state_span(&s).unwrap().loop_end, q(32));
    s.exit = Some(q(2));
    let span = p.state_span(&s).unwrap();
    assert_eq!((span.loop_start, span.loop_end), (q(16), q(32)));
    // A scene: its longest clip from the song's start.
    let fight = MusicState::new(MusicStateId(2), "Fight", StateSource::Scene { scene });
    let span = p.state_span(&fight).unwrap();
    assert_eq!((span.start, span.end), (MusicalTime::ZERO, q(8)));
    assert_eq!(p.scene_length(scene), Some(q(8)));
    assert_eq!(p.state_source_name(fight.source), "Scene ‘Fight’");
    // Gone: none.
    let gone = MusicState::new(
        MusicStateId(3),
        "Gone",
        StateSource::Section {
            section: SectionId(999),
        },
    );
    assert_eq!(p.state_span(&gone), None);
}

#[test]
fn the_score_is_one_command_with_its_inverse_and_survives_files() {
    let mut p = project_with_states();
    let empty = serde_json::to_value(&p).unwrap();
    assert!(
        empty.get("interactive").is_none(),
        "no score, nothing written"
    );
    let section = p.sections[0].id;
    let mut explore = MusicState::new(
        MusicStateId(50),
        "Explore",
        StateSource::Section { section },
    );
    explore.cues.push(StateCue {
        name: "Hit".into(),
        at: q(6),
    });
    let mut r = rule(51, Some(50), None, SyncPoint::Grid { bars: 2 });
    r.fade_out = FadeLength::Millis { ms: 500.0 };
    r.fade_in = FadeLength::Beats { beats: 2.0 };
    r.entry = Entry::SameTime;
    let score = InteractiveScore {
        states: vec![explore],
        rules: vec![r],
        start: Some(MusicStateId(50)),
        ..InteractiveScore::default()
    };
    let cmd = Command::SetInteractiveScore {
        score: Box::new(score.clone()),
    };
    assert_eq!(cmd.impact(), crate::Impact::None);
    let undo = cmd.apply(&mut p).unwrap();
    assert_eq!(p.interactive, score);
    let json = serde_json::to_string(&p).unwrap();
    let back: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(back.interactive, score);
    undo.apply(&mut p).unwrap();
    assert!(p.interactive.is_empty());
    // Tidied on the way in: a dangling start and segment dropped.
    let mut odd = score.clone();
    odd.start = Some(MusicStateId(999));
    odd.rules[0].segment = Some(MusicStateId(998));
    Command::SetInteractiveScore {
        score: Box::new(odd),
    }
    .apply(&mut p)
    .unwrap();
    assert_eq!(p.interactive.start, None);
    assert_eq!(p.interactive.rules[0].segment, None);
}
