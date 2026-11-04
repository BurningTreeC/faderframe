//! Audio Bisect: a planted bad edit found from answers given by a
//! scripted listener who only hears the two sides (the joined file the
//! compare path plays, region by region), "Can't tell" asking again and
//! then skipping, the blind assignment hidden but kept, the search across
//! a branch of the undo tree, the step found taken back or brought back,
//! and a cancel that leaves everything as it was.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{GeneratorSpec, WavFormat, read_wav, write_wav};
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, History, Project, SourceSpec,
    StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_session::bisect::{
    BisectAnswer, BisectOp, BisectSide, BisectStage, BisectStatus, BisectVerdict,
};
use faderframe_session::compare::{CompareOp, Comparing};
use faderframe_session::references::ReferenceOp;
use faderframe_session::{Action, Session};
use std::time::Duration;

const SR: u32 = 48_000;
const WAIT: Duration = Duration::from_secs(300);

/// A sine track: `hz` at `amp` for `seconds` (generators play at a peak of
/// 0.7: the clip's gain sets the amplitude).
fn sine(p: &mut Project, name: &str, hz: f64, amp: f32, seconds: f64) -> TrackId {
    let src = p.ids.allocate();
    p.sources.insert(
        src,
        AudioSource {
            id: src,
            name: name.into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::Sine {
                    frequency: hz,
                    seconds,
                    amplitude: amp,
                },
            },
        },
    );
    let id: TrackId = p.ids.allocate();
    let mut t = Track::new(id, TrackKind::Audio, name, TrackColor::palette(1));
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
            length: (seconds * f64::from(SR)) as i64,
            gain_db: 20.0 * (amp / 0.7).log10(),
            fades: ClipFades::default(),
            stretch: StretchSettings::Off,
            reversed: false,
            warp: None,
            pitch: None,
            effects: None,
            spectral: None,
            envelope: None,
        }),
    };
    t.clips.push(clip.id);
    p.clips.insert(clip.id, clip);
    p.tracks.push(t);
    id
}

/// Three tones; the second bar selected (with the run-up, renders of four
/// seconds).
fn session() -> (Session, [TrackId; 3]) {
    let mut p = Project::new("Bisect", SR);
    let low = sine(&mut p, "Low", 110.0, 0.4, 8.0);
    let mid = sine(&mut p, "Mid", 440.0, 0.2, 8.0);
    let high = sine(&mut p, "High", 2000.0, 0.1, 8.0);
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    let meter = s.project().timeline.meter.clone();
    s.selection.range = Some(faderframe_session::editing::EditRange::new(
        meter.bar_start(1),
        meter.bar_start(2),
    ));
    s.set_bisect_seed(7);
    (s, [low, mid, high])
}

fn edit(s: &mut Session, cmd: Command) -> u64 {
    s.dispatch(Action::Edit(cmd)).unwrap();
    s.history().last_step().unwrap()
}

fn rename(s: &mut Session, track: TrackId, name: &str) -> u64 {
    edit(
        s,
        Command::RenameTrack {
            track,
            name: name.into(),
        },
    )
}

fn volume(s: &mut Session, track: TrackId, db: f32) -> u64 {
    edit(s, Command::SetTrackVolume { track, db })
}

fn status(s: &Session) -> BisectStatus {
    s.bisect_status().unwrap()
}

/// The stretch of the side heard now, as the compare path plays it.
fn heard(s: &Session) -> Vec<Vec<f32>> {
    let w = s
        .compare_link()
        .wanted()
        .expect("a side is handed to the engine");
    let file = read_wav(w.source.path()).unwrap();
    let (from, to) = (w.settings.from as usize, w.settings.to as usize);
    file.channels
        .iter()
        .map(|c| c[from.min(c.len())..to.min(c.len())].to_vec())
        .collect()
}

fn rms_db(c: &[Vec<f32>]) -> f64 {
    let (sum, n) = c.iter().flatten().fold((0.0f64, 0usize), |(s, n), v| {
        (s + f64::from(*v) * f64::from(*v), n + 1)
    });
    10.0 * (sum / n.max(1) as f64).max(1e-20).log10()
}

/// Brightness: the energy of the first difference over the energy.
fn brightness(c: &[Vec<f32>]) -> f64 {
    let (mut d, mut e) = (0.0f64, 0.0f64);
    for ch in c {
        for w in ch.windows(2) {
            d += f64::from(w[1] - w[0]).powi(2);
            e += f64::from(w[1]).powi(2);
        }
    }
    d / e.max(1e-20)
}

/// What a listener hears of A and B: the measure of each side.
fn listen(s: &mut Session, measure: fn(&[Vec<f32>]) -> f64) -> (f64, f64) {
    s.dispatch(Action::Bisect(BisectOp::Hear(BisectSide::A)))
        .unwrap();
    let a = measure(&heard(s));
    s.dispatch(Action::Bisect(BisectOp::Hear(BisectSide::B)))
        .unwrap();
    let b = measure(&heard(s));
    (a, b)
}

/// Louder is better; within `same` of each other: can't tell.
fn answer_for(a: f64, b: f64, same: f64) -> BisectAnswer {
    if (a - b).abs() < same {
        BisectAnswer::CantTell
    } else if a > b {
        BisectAnswer::A
    } else {
        BisectAnswer::B
    }
}

/// Answer every round by loudness until the search is done (`force`:
/// "Can't tell" for these comparisons, by number).
fn run(s: &mut Session, force: &[usize]) {
    for _ in 0..40 {
        assert!(s.wait_for_bisect(WAIT));
        let st = status(s);
        assert_eq!(st.error, None);
        if st.stage != BisectStage::Comparing {
            return;
        }
        assert!(st.ready && st.blind);
        let (a, b) = listen(s, rms_db);
        let answer = if force.contains(&st.round) {
            BisectAnswer::CantTell
        } else {
            answer_for(a, b, 0.2)
        };
        s.dispatch(Action::Bisect(BisectOp::Answer(answer)))
            .unwrap();
    }
    panic!("the search did not end");
}

#[test]
fn a_planted_bad_edit_is_found_from_blind_answers_and_taken_back() {
    let (mut s, [low, mid, high]) = session();
    rename(&mut s, low, "Low 1");
    volume(&mut s, mid, 1.0);
    rename(&mut s, mid, "Mid 1");
    let bad = edit(
        &mut s,
        Command::SetTrackMute {
            track: low,
            on: true,
        },
    );
    volume(&mut s, high, 1.0);
    rename(&mut s, high, "High 1");
    volume(&mut s, mid, 2.0);
    let project = s.project().clone();
    let steps = s.history_steps();
    // Loudness is what tells them apart here.
    s.dispatch(Action::Bisect(BisectOp::MatchLoudness(false)))
        .unwrap();
    s.dispatch(Action::Bisect(BisectOp::Start {
        good: History::ROOT,
        bad: None,
    }))
    .unwrap();
    s.dispatch(Action::Compare(CompareOp::MatchLoudness(false)))
        .unwrap();
    let st = status(&s);
    assert_eq!(st.stage, BisectStage::Comparing);
    assert_eq!(st.path.len(), 8, "the start and seven steps");
    assert_eq!(s.compare_status().unwrap().what, Comparing::Bisect);
    // While the first round is prepared the mix is heard.
    assert!(!st.ready);
    run(&mut s, &[]);
    // Nothing was touched by the search.
    assert_eq!(s.project(), &project);
    assert_eq!(s.history_steps(), steps);
    let st = status(&s);
    assert_eq!(st.stage, BisectStage::Found);
    let found = st.found.unwrap();
    assert!(found.exact);
    assert_eq!(found.steps[0].node, bad);
    assert_eq!(found.steps[0].label, "Toggle Mute");
    assert!(!found.steps[0].undone);
    assert!(st.round <= 4, "{} comparisons for six candidates", st.round);
    assert!(st.text.starts_with("Found: ‘Toggle Mute’"), "{}", st.text);
    assert_eq!(found.take_back.as_deref(), Ok("Undo ‘Toggle Mute’"));
    // Its before and after, named: the after is the quiet one.
    s.dispatch(Action::Bisect(BisectOp::AuditionFound)).unwrap();
    assert!(s.wait_for_bisect(WAIT));
    let st = status(&s);
    assert!(st.ready && !st.blind);
    assert_eq!(st.hearing, Some(BisectSide::A));
    assert_eq!(s.compare_status().unwrap().other_label, "Before");
    let (before, after) = listen(&mut s, rms_db);
    assert!(before > after + 3.0, "{before} vs {after}");
    // Taken back alone: a new step; the later ones stay.
    s.dispatch(Action::Bisect(BisectOp::UndoFound)).unwrap();
    assert!(!s.bisecting());
    assert!(s.compare_status().is_none());
    let t = |id| s.project().track(id).unwrap().clone();
    assert!(!t(low).mute);
    assert_eq!((t(high).volume_db, t(mid).volume_db), (1.0, 2.0));
    assert_eq!(t(high).name, "High 1");
    assert_eq!(s.history_steps().0.len(), steps.0.len() + 1);
    assert_eq!(s.history().undo_label(), Some("Undo ‘Toggle Mute’"));
}

#[test]
fn cant_tell_asks_against_the_bad_state_then_skips() {
    let (mut s, [low, mid, _]) = session();
    for i in 0..4 {
        rename(&mut s, mid, &format!("Mid {i}"));
    }
    let bad = edit(
        &mut s,
        Command::SetTrackMute {
            track: low,
            on: true,
        },
    );
    for i in 0..3 {
        rename(&mut s, low, &format!("Low {i}"));
    }
    s.dispatch(Action::Bisect(BisectOp::Start {
        good: History::ROOT,
        bad: None,
    }))
    .unwrap();
    s.dispatch(Action::Bisect(BisectOp::MatchLoudness(false)))
        .unwrap();
    // The first candidate (the middle of eight: step 4, which sounds as
    // the start does) gets "Can't tell" twice: skipped.
    run(&mut s, &[1, 2]);
    let st = status(&s);
    let log: Vec<(u64, bool, BisectVerdict)> = st
        .log
        .iter()
        .map(|l| (l.node, l.against_good, l.verdict))
        .collect();
    let fourth = st.path[4];
    assert_eq!(log[0], (fourth, true, BisectVerdict::Undecided));
    assert_eq!(log[1], (fourth, false, BisectVerdict::Skipped));
    assert_eq!(st.skipped, vec![fourth]);
    // Rounds after: the nearest to the middle not skipped; a candidate
    // sounding as the good state is told apart against the bad one.
    assert_eq!(log[2].0, st.path[3]);
    // The step skipped and the bad one are what is left: not one step.
    let found = st.found.unwrap();
    assert!(!found.exact);
    let nodes: Vec<u64> = found.steps.iter().map(|f| f.node).collect();
    assert_eq!(nodes, vec![fourth, bad]);
    assert!(found.take_back.is_err());
    assert!(st.text.starts_with("It is one of 2 steps"), "{}", st.text);
}

#[test]
fn the_blind_assignment_is_hidden_but_kept() {
    let (mut s, [low, _, high]) = session();
    for i in 0..6 {
        rename(&mut s, low, &format!("Low {i}"));
    }
    // The bad edit: the bright tone gone (heard even when matched).
    edit(
        &mut s,
        Command::SetTrackMute {
            track: high,
            on: true,
        },
    );
    rename(&mut s, high, "High 1");
    s.dispatch(Action::Bisect(BisectOp::Start {
        good: History::ROOT,
        bad: None,
    }))
    .unwrap();
    assert!(status(&s).match_loudness, "matched by default");
    let mut sides = Vec::new();
    for _ in 0..12 {
        assert!(s.wait_for_bisect(WAIT));
        let st = status(&s);
        if st.stage != BisectStage::Comparing {
            break;
        }
        // Nothing shown tells which side is which: no step named, the
        // round not in the log yet.
        assert!(st.log.iter().all(|l| l.round < st.round));
        let text = format!("{} {}", st.text, s.compare_status().unwrap().text);
        assert!(!text.contains("Low") && !text.contains("High"), "{text}");
        assert!(text.contains("blind"), "{text}");
        // A and B stay where they are: heard again, flipped, and joined
        // anew at other gains.
        s.dispatch(Action::Bisect(BisectOp::Hear(BisectSide::A)))
            .unwrap();
        let a = s.compare_link().wanted().unwrap().settings;
        let (ba, bb) = listen(&mut s, brightness);
        s.dispatch(Action::Compare(CompareOp::Flip)).unwrap();
        assert_eq!(s.compare_link().wanted().unwrap().settings, a);
        s.dispatch(Action::Bisect(BisectOp::MatchLoudness(false)))
            .unwrap();
        assert!(s.wait_for_bisect(WAIT));
        let (ua, ub) = listen(&mut s, brightness);
        assert_eq!((ua > ub), (ba > bb), "the same side is the brighter");
        s.dispatch(Action::Bisect(BisectOp::MatchLoudness(true)))
            .unwrap();
        assert!(s.wait_for_bisect(WAIT));
        let answer = answer_for(ba, bb, 0.01 * ba.max(bb));
        s.dispatch(Action::Bisect(BisectOp::Answer(answer)))
            .unwrap();
        // Told now: the side the candidate played on.
        let st = status(&s);
        sides.push(st.log.last().unwrap().was);
    }
    let st = status(&s);
    assert_eq!(st.stage, BisectStage::Found);
    assert_eq!(st.found.unwrap().steps[0].label, "Toggle Mute");
    assert!(
        sides.contains(&BisectSide::A) && sides.contains(&BisectSide::B),
        "drawn at random: {sides:?}"
    );
}

#[test]
fn a_good_state_on_another_branch_is_reached_through_the_fork() {
    let (mut s, [low, mid, _]) = session();
    rename(&mut s, mid, "Mid 1");
    let good = rename(&mut s, mid, "Mid 2");
    s.dispatch(Action::Undo).unwrap();
    // A branch: the bad edit after the fork.
    let bad = edit(
        &mut s,
        Command::SetTrackMute {
            track: low,
            on: true,
        },
    );
    rename(&mut s, low, "Low 1");
    assert!(s.history().knows(good) && !s.history().is_done(good));
    s.dispatch(Action::Bisect(BisectOp::Start { good, bad: None }))
        .unwrap();
    s.dispatch(Action::Bisect(BisectOp::MatchLoudness(false)))
        .unwrap();
    assert_eq!(status(&s).path.len(), 4, "up to the fork, down the branch");
    run(&mut s, &[]);
    let found = status(&s).found.unwrap();
    assert!(found.exact);
    assert_eq!(found.steps[0].node, bad);
    // Go to just before it (the last good state).
    s.dispatch(Action::Bisect(BisectOp::GoToFound { after: false }))
        .unwrap();
    assert!(!s.bisecting());
    assert_eq!(s.history_current(), found.before);
    assert!(!s.project().track(low).unwrap().mute);
}

#[test]
fn a_step_undone_on_the_way_is_found_and_brought_back() {
    let (mut s, [low, mid, _]) = session();
    // The good state: the low tone louder; then undone, another branch.
    let good = volume(&mut s, low, 6.0);
    s.dispatch(Action::Undo).unwrap();
    rename(&mut s, mid, "Mid 1");
    s.dispatch(Action::Bisect(BisectOp::Start { good, bad: None }))
        .unwrap();
    s.dispatch(Action::Bisect(BisectOp::MatchLoudness(false)))
        .unwrap();
    run(&mut s, &[]);
    let found = status(&s).found.unwrap();
    assert!(found.exact && found.steps[0].undone);
    assert_eq!(found.steps[0].node, good);
    assert_eq!(found.take_back.as_deref(), Ok("Bring Back ‘Change Volume’"));
    s.dispatch(Action::Bisect(BisectOp::UndoFound)).unwrap();
    let t = |id| s.project().track(id).unwrap().clone();
    assert_eq!(t(low).volume_db, 6.0);
    assert_eq!(t(mid).name, "Mid 1", "the branch's own step stays");
    assert_eq!(s.history().undo_label(), Some("Bring Back ‘Change Volume’"));
}

#[test]
fn cancel_leaves_everything_as_it_was() {
    let dir = std::env::temp_dir().join(format!("ff-bisect-cancel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut s, [low, mid, _]) = session();
    // A reference compared before the search: chosen again after it.
    let tone: Vec<f32> = (0..SR as usize * 4)
        .map(|i| 0.3 * (i as f32 * 0.05).sin())
        .collect();
    let file = dir.join("Record.wav");
    write_wav(&file, &[tone.clone(), tone], SR, WavFormat::Float32, false).unwrap();
    s.dispatch(Action::Reference(ReferenceOp::Add(vec![file])))
        .unwrap();
    assert!(s.wait_for_compare(WAIT));
    let reference = s.current_reference().unwrap();
    for i in 0..3 {
        volume(&mut s, mid, i as f32);
    }
    edit(
        &mut s,
        Command::SetTrackMute {
            track: low,
            on: true,
        },
    );
    let project = s.project().clone();
    let steps = s.history_steps();
    s.dispatch(Action::Bisect(BisectOp::Start {
        good: History::ROOT,
        bad: None,
    }))
    .unwrap();
    assert!(s.wait_for_bisect(WAIT));
    let joined = s
        .compare_link()
        .wanted()
        .unwrap()
        .source
        .path()
        .to_path_buf();
    assert!(joined.exists());
    s.dispatch(Action::Bisect(BisectOp::Answer(BisectAnswer::A)))
        .unwrap();
    s.dispatch(Action::Bisect(BisectOp::Cancel)).unwrap();
    assert!(s.bisect_status().is_none());
    assert_eq!(s.project(), &project);
    assert_eq!(s.history_steps(), steps);
    let c = s.compare_status().unwrap();
    assert_eq!(c.what, Comparing::Reference(reference));
    assert!(!c.hearing_other);
    // Its files go (some once the renders left running end).
    assert!(s.wait_for_compare(WAIT));
    for _ in 0..200 {
        s.wait_for_bisect(WAIT);
        if !joined.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!joined.exists());
    // Not searching: nothing to answer.
    assert!(
        s.dispatch(Action::Bisect(BisectOp::Answer(BisectAnswer::B)))
            .is_err()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn getting_ready_marks_the_good_and_bad_states() {
    let (mut s, [low, _, _]) = session();
    let first = rename(&mut s, low, "Low 1");
    let second = rename(&mut s, low, "Low 2");
    s.dispatch(Action::Bisect(BisectOp::Setup)).unwrap();
    let st = status(&s);
    assert_eq!(
        (st.stage, st.good, st.bad),
        (BisectStage::Setup, None, None)
    );
    assert!(st.text.starts_with("Audio Bisect: click"), "{}", st.text);
    s.dispatch(Action::Bisect(BisectOp::Mark {
        node: first,
        good: true,
    }))
    .unwrap();
    s.dispatch(Action::Bisect(BisectOp::Mark {
        node: second,
        good: false,
    }))
    .unwrap();
    let st = status(&s);
    // The project now is the bad state by default: marking it is no mark.
    assert_eq!((st.good, st.bad), (Some(first), None));
    assert!(st.text.contains("the project now"), "{}", st.text);
    // The same state twice: refused.
    assert!(
        s.dispatch(Action::Bisect(BisectOp::Start {
            good: second,
            bad: None,
        }))
        .is_err()
    );
    // One step between them: found at once, nothing to compare.
    s.dispatch(Action::Bisect(BisectOp::Start {
        good: first,
        bad: None,
    }))
    .unwrap();
    let st = status(&s);
    assert_eq!(st.stage, BisectStage::Found);
    assert_eq!(st.found.unwrap().steps[0].node, second);
    s.dispatch(Action::Compare(CompareOp::Off)).unwrap();
    assert!(!s.bisecting());
}
