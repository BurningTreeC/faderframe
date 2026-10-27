#![allow(clippy::unwrap_used)]
//! Counting in before playback: the transport stands while the click
//! counts, then plays from there, and the take holds what was played from
//! that moment. A count-in of its own length comes before the pre-roll;
//! what a pre-roll cannot play before the project's start is counted in
//! too (recording from the start, or closer to it than the pre-roll).
//! With the dummy device in real time (120 BPM 4/4: a beat is 24 000
//! frames, a bar 96 000 — two seconds).

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, InputRouting, Project, TrackKind};
use faderframe_session::launcher::LauncherOp;
use faderframe_session::{Action, AudioPreferences, EditFlag, Roll, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;
const BAR: i64 = 96_000;
const BEAT: i64 = 24_000;

fn session() -> (Session, TrackId) {
    let mut s = Session::new(Project::new("Count", SR), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.edit(Command::SetTrackInput {
        track: t,
        input: InputRouting::Hardware { first_channel: 0 },
    })
    .unwrap();
    s.edit(Command::SetTrackRecordArm { track: t, on: true })
        .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::with_input_tone(1_000.0))],
        &AudioPreferences {
            sample_rate: Some(SR),
            buffer_size: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
    s.dispatch(Action::SetEditFlag(
        EditFlag::InsertionFollowsPlayback,
        false,
    ))
    .unwrap();
    (s, t)
}

fn preroll(s: &mut Session, roll: Roll, on_bar: bool) {
    let mut r = s.record;
    r.preroll = roll;
    r.preroll_on_bar = on_bar;
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
}

fn count_in(s: &mut Session, length: Option<Roll>) {
    let mut r = s.record;
    r.count_in = length.is_some();
    r.count_in_length = length.unwrap_or(r.count_in_length);
    s.dispatch(Action::SetRecordSettings(r)).unwrap();
}

fn transport(s: &mut Session, a: TransportAction) {
    s.dispatch(Action::Transport(a)).unwrap();
}

fn tick_for(s: &mut Session, d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end {
        s.tick(0.005);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// What the transport did, tick by tick, until `done` (or 10 s).
#[derive(Default, Debug)]
struct Watch {
    /// When the engine was first seen counting in, and first seen playing.
    counting_from: Option<Instant>,
    playing_from: Option<Instant>,
    /// While counting in: the processed positions seen (all the start), and
    /// the count-in positions shown (`Session::count_in_position`).
    counting_positions: Vec<i64>,
    shown: Vec<i64>,
    /// The session said playing (started) all through the count-in.
    started_throughout: bool,
}

fn watch(s: &mut Session, mut done: impl FnMut(&Session, &Watch) -> bool) -> Watch {
    let mut w = Watch {
        started_throughout: true,
        ..Watch::default()
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        s.tick(0.005);
        let e = s.engine().transport_snapshot();
        if e.counting_in > 0 {
            w.counting_from.get_or_insert_with(Instant::now);
            w.counting_positions.push(e.position);
            w.started_throughout &= s.transport().playing;
        }
        if e.playing {
            w.playing_from.get_or_insert_with(Instant::now);
        }
        if let Some(at) = s.count_in_position() {
            w.shown.push(at);
        }
        if done(s, &w) {
            return w;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out: {w:?}");
}

fn counted(w: &Watch) -> Duration {
    w.playing_from.unwrap() - w.counting_from.expect("it counted in")
}

fn takes(s: &Session, t: TrackId) -> Vec<faderframe_project::Clip> {
    let mut v: Vec<_> = s.project().clips_of(t).into_iter().cloned().collect();
    v.sort_by_key(|c| c.start);
    v
}

fn records(s: &Session) -> usize {
    s.history_steps()
        .0
        .iter()
        .filter(|l| l.as_str() == "Record")
        .count()
}

#[test]
fn the_pre_roll_before_the_start_is_a_count_in() {
    let (s, _) = session();
    let pre = |from, r, on_bar| s.pre_roll_for_tests(from, r, on_bar);
    // From the start: all of it is counted in.
    assert_eq!(pre(0, Roll::bars(1.0), true), (0, BAR as u64));
    assert_eq!(pre(0, Roll::bars(2.0), false), (0, 2 * BAR as u64));
    assert_eq!(pre(0, Roll::beats(2.0), false), (0, 2 * BEAT as u64));
    assert_eq!(pre(0, Roll::seconds(1.5), false), (0, 72_000));
    // From beat 3 of bar 1: two beats play, the rest is counted in — to the
    // bar line before it on a bar.
    assert_eq!(pre(2 * BEAT, Roll::bars(2.0), false), (0, 6 * BEAT as u64));
    assert_eq!(pre(2 * BEAT, Roll::bars(2.0), true), (0, 2 * BAR as u64));
    assert_eq!(pre(2 * BEAT, Roll::bars(1.0), false), (0, 2 * BEAT as u64));
    assert_eq!(pre(2 * BEAT, Roll::beats(2.0), false), (0, 0));
    // Far enough in: no count-in, as before.
    assert_eq!(pre(5 * BAR, Roll::bars(1.0), true), (4 * BAR, 0));
    assert_eq!(pre(BAR, Roll::bars(1.0), true), (0, 0));
    assert_eq!(pre(BEAT, Roll::OFF, true), (BEAT, 0));
}

/// The reported bug: Record from bar 1 with a one-bar pre-roll used to
/// play and record from the start at once — the first bar of the click,
/// taken for the count-in, was the take's first bar, and the part played
/// after it landed a bar late.
#[test]
fn recording_from_the_start_counts_a_bar_in_then_plays_and_records_from_it() {
    let (mut s, t) = session();
    preroll(&mut s, Roll::bars(1.0), true);
    transport(&mut s, TransportAction::ToggleRecord);
    assert!(s.transport().playing, "started at once");
    let w = watch(&mut s, |s, w| {
        w.playing_from.is_some() && s.processing_position() >= SR as i64 / 2
    });
    // Two seconds standing at the start while the click counts.
    let c = counted(&w);
    assert!(
        (Duration::from_millis(1_850)..Duration::from_millis(2_300)).contains(&c),
        "counted in for {c:?}"
    );
    assert!(w.counting_positions.iter().all(|p| *p == 0), "{w:?}");
    assert!(
        w.started_throughout,
        "playing (started) as the session sees it"
    );
    // Shown as the bar before bar 1, counting up to it.
    assert!(!w.shown.is_empty());
    assert!(
        w.shown.iter().all(|p| (-BAR..0).contains(p)),
        "{:?}",
        w.shown
    );
    assert!(*w.shown.first().unwrap() < -BAR * 3 / 4, "{:?}", w.shown);
    assert!(*w.shown.last().unwrap() > -BAR / 4, "{:?}", w.shown);
    let label = s
        .project()
        .timeline
        .format_bbt(s.engine().samples_to_musical(s.project(), w.shown[0]));
    assert!(label.starts_with("0.1."), "the bar before bar 1: {label}");
    assert_eq!(s.count_in_position(), None, "playing now");
    let played = Instant::now() - w.playing_from.unwrap();
    transport(&mut s, TransportAction::Stop);
    s.wait_for_recordings();
    // The take is what was played from the start: half a second or so,
    // not the count-in's two seconds as well.
    let c = takes(&s, t);
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0].start, MusicalTime::ZERO);
    let length = c[0].as_audio().unwrap().length;
    let expected = played.as_secs_f64() * SR as f64;
    assert!(
        (length as f64 - expected).abs() < 0.15 * SR as f64,
        "{length} frames, played {expected}"
    );
    assert_eq!(records(&s), 1);
    // Return to Start on Stop: where playback started (the start).
    tick_for(&mut s, Duration::from_millis(100));
    assert_eq!(s.processing_position(), 0);
}

/// From beat 3 of bar 1 with a one-bar pre-roll (not on a bar): two beats
/// are counted in, two play from the start before the take — and Stop
/// returns to beat 3.
#[test]
fn a_pre_roll_reaching_past_the_start_counts_in_the_rest() {
    let (mut s, t) = session();
    preroll(&mut s, Roll::bars(1.0), false);
    transport(
        &mut s,
        TransportAction::Locate(MusicalTime::from_quarters_i(2)),
    );
    tick_for(&mut s, Duration::from_millis(50));
    transport(&mut s, TransportAction::ToggleRecord);
    let w = watch(&mut s, |s, w| {
        w.playing_from.is_some() && s.processing_position() >= 3 * BEAT
    });
    let c = counted(&w);
    assert!(
        (Duration::from_millis(900)..Duration::from_millis(1_300)).contains(&c),
        "two beats counted in: {c:?}"
    );
    assert!(w.counting_positions.iter().all(|p| *p == 0), "{w:?}");
    assert!(
        w.shown.iter().all(|p| (-2 * BEAT..0).contains(p)),
        "{:?}",
        w.shown
    );
    transport(&mut s, TransportAction::Stop);
    s.wait_for_recordings();
    let c = takes(&s, t);
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(
        c[0].start,
        MusicalTime::from_quarters_i(2),
        "the take at beat 3"
    );
    tick_for(&mut s, Duration::from_millis(100));
    assert_eq!(s.processing_position(), 2 * BEAT, "back at beat 3");
}

/// The reported wish (#23): a count-in without a pre-roll. Recording from
/// bar 3 with a one-bar count-in stands at bar 3 for a bar while the click
/// counts (shown as bar 2), then plays and records from there.
#[test]
fn a_count_in_counts_its_bars_then_records_from_the_playhead() {
    let (mut s, t) = session();
    count_in(&mut s, Some(Roll::bars(1.0)));
    transport(
        &mut s,
        TransportAction::Locate(MusicalTime::from_quarters_i(8)),
    );
    tick_for(&mut s, Duration::from_millis(50));
    transport(&mut s, TransportAction::ToggleRecord);
    let w = watch(&mut s, |s, w| {
        w.playing_from.is_some() && s.processing_position() >= 2 * BAR + BEAT
    });
    let c = counted(&w);
    assert!(
        (Duration::from_millis(1_850)..Duration::from_millis(2_300)).contains(&c),
        "a bar counted in: {c:?}"
    );
    assert!(w.counting_positions.iter().all(|p| *p == 2 * BAR), "{w:?}");
    assert!(
        w.shown.iter().all(|p| (BAR..2 * BAR).contains(p)),
        "shown in bar 2: {:?}",
        w.shown
    );
    transport(&mut s, TransportAction::Stop);
    s.wait_for_recordings();
    let c = takes(&s, t);
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(
        c[0].start,
        MusicalTime::from_quarters_i(8),
        "the take at bar 3"
    );
    tick_for(&mut s, Duration::from_millis(100));
    assert_eq!(s.processing_position(), 2 * BAR, "back at bar 3");
}

/// A count-in comes before the pre-roll: it stands at the pre-roll's
/// start while it counts. From the project's start, the pre-roll that
/// cannot play there is counted in as well (both, back to back).
#[test]
fn a_count_in_comes_before_the_pre_roll() {
    let (mut s, _) = session();
    count_in(&mut s, Some(Roll::beats(2.0)));
    preroll(&mut s, Roll::bars(1.0), true);
    // From bar 3: two beats standing at bar 2, then bar 2 plays.
    transport(
        &mut s,
        TransportAction::Locate(MusicalTime::from_quarters_i(8)),
    );
    tick_for(&mut s, Duration::from_millis(50));
    transport(&mut s, TransportAction::ToggleRecord);
    let w = watch(&mut s, |s, w| {
        w.playing_from.is_some() && s.processing_position() >= BAR + BEAT
    });
    let c = counted(&w);
    assert!(
        (Duration::from_millis(900)..Duration::from_millis(1_300)).contains(&c),
        "two beats counted in: {c:?}"
    );
    assert!(w.counting_positions.iter().all(|p| *p == BAR), "{w:?}");
    transport(&mut s, TransportAction::AbortRecording);
    s.wait_for_discarded_recordings();
    tick_for(&mut s, Duration::from_millis(100));
    // From the start: the bar of pre-roll and the two beats before it.
    transport(&mut s, TransportAction::Locate(MusicalTime::ZERO));
    tick_for(&mut s, Duration::from_millis(50));
    transport(&mut s, TransportAction::ToggleRecord);
    let w = watch(&mut s, |_, w| !w.counting_positions.is_empty());
    let first = s.engine().transport_snapshot().counting_in as i64;
    assert!(
        (BAR + 2 * BEAT - 4_800..=BAR + 2 * BEAT).contains(&first),
        "a bar and two beats to count: {first} ({w:?})"
    );
    transport(&mut s, TransportAction::AbortRecording);
    s.wait_for_discarded_recordings();
}

/// Stop while counting in: nothing plays later, nothing is recorded, no
/// undo step; Abort the same; Abort and Restart counts in again and
/// records.
#[test]
fn stop_and_abort_cancel_a_count_in_and_a_restart_counts_in_again() {
    let (mut s, t) = session();
    preroll(&mut s, Roll::bars(1.0), true);
    for how in [
        TransportAction::Stop,
        TransportAction::AbortRecording,
        TransportAction::TogglePlay,
    ] {
        transport(&mut s, TransportAction::ToggleRecord);
        watch(&mut s, |s, w| {
            w.counting_from
                .is_some_and(|at| at.elapsed() > Duration::from_millis(500))
                && s.transport().counting_in > 0
        });
        transport(&mut s, how.clone());
        s.wait_for_recordings();
        s.wait_for_discarded_recordings();
        // Past where the count-in would have ended: still stopped.
        let end = Instant::now() + Duration::from_millis(2_000);
        while Instant::now() < end {
            s.tick(0.005);
            let e = s.engine().transport_snapshot();
            assert!(!e.started(), "{how:?}: it never plays");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!s.transport().playing && s.transport().counting_in == 0);
        assert_eq!(s.count_in_position(), None);
        assert!(s.recording_view().is_none());
        assert!(takes(&s, t).is_empty(), "{how:?}: nothing recorded");
        assert_eq!(records(&s), 0);
        assert_eq!(s.processing_position(), 0);
    }
    // Abort and Restart while counting in: it counts in again, then
    // records from the start.
    transport(&mut s, TransportAction::ToggleRecord);
    let first = watch(&mut s, |s, w| {
        w.counting_from
            .is_some_and(|at| at.elapsed() > Duration::from_millis(500))
            && s.transport().counting_in > 0
    });
    transport(&mut s, TransportAction::AbortAndRestartRecording);
    let again = watch(&mut s, |s, w| {
        w.counting_from
            .is_some_and(|at| at > first.counting_from.unwrap())
            && w.playing_from.is_some()
            && s.processing_position() >= BEAT
    });
    let c = counted(&again);
    assert!(
        (Duration::from_millis(1_850)..Duration::from_millis(2_300)).contains(&c),
        "counted in again for {c:?}"
    );
    transport(&mut s, TransportAction::Stop);
    s.wait_for_recordings();
    s.wait_for_discarded_recordings();
    let c = takes(&s, t);
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0].start, MusicalTime::ZERO);
    assert_eq!(records(&s), 1);
}

/// Recording into a launcher slot while stopped at the start: the slot's
/// count-in is counted in before playback starts there.
#[test]
fn a_slot_recording_at_the_start_counts_in_first() {
    let (mut s, t) = session();
    s.dispatch(Action::Launcher(LauncherOp::AddScene { after: None }))
        .unwrap();
    s.dispatch(Action::Launcher(LauncherOp::SetQuantize(
        faderframe_project::launcher::LaunchQuantize::Beat,
    )))
    .unwrap();
    s.dispatch(Action::Launcher(LauncherOp::SetRecordOptions {
        bars: 0,
        count_in: 1,
    }))
    .unwrap();
    let scene = s.project().launcher.scenes[0].id;
    s.dispatch(Action::Launcher(LauncherOp::Record { track: t, scene }))
        .unwrap();
    assert_eq!(s.launcher_recording(), Some((t, scene, false)));
    let w = watch(&mut s, |s, w| {
        w.playing_from.is_some() && s.processing_position() >= SR as i64 / 2
    });
    let c = counted(&w);
    assert!(
        (Duration::from_millis(1_850)..Duration::from_millis(2_300)).contains(&c),
        "a bar counted in: {c:?}"
    );
    assert!(w.counting_positions.iter().all(|p| *p == 0), "{w:?}");
    // Ends on the next beat: the slot's clip, recorded from the start.
    s.dispatch(Action::Launcher(LauncherOp::Record { track: t, scene }))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.project().launcher.clip(t, scene).is_none() {
        assert!(Instant::now() < deadline, "the slot's clip");
        s.tick(0.005);
        std::thread::sleep(Duration::from_millis(2));
    }
    let id = s.project().launcher.clip(t, scene).unwrap();
    let a = s.project().clips[&id].as_audio().unwrap().clone();
    assert_eq!(
        a.length % BEAT,
        0,
        "whole beats from the start: {}",
        a.length
    );
    assert!(
        a.length <= 2 * BEAT,
        "not the count-in's bar as well: {}",
        a.length
    );
}
