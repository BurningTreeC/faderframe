//! Following an external MIDI clock and MTC end to end: virtual input,
//! session, engine on the dummy backend (real time). The test streams MIDI
//! with millisecond timing, which the shared macOS and Windows CI machines
//! cannot keep (sleeps overshoot by tens of milliseconds), so these run on
//! Linux only; the sync logic itself is the same everywhere.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_engine::EngineConfig;
use faderframe_project::Project;
use faderframe_session::{AudioPreferences, Session, SyncSettings, SyncSource, Timecode};
use std::time::{Duration, Instant};

fn session() -> Session {
    let mut s = Session::new(Project::new("Sync", 48_000), None, EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            threads: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    // Let the stream come up.
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(100) {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
    s
}

/// Send `msgs(i)` every `period` for `count` steps from `start`, ticking
/// the session; returns when the next step would be due.
fn stream_from(
    s: &mut Session,
    start: Instant,
    msgs: impl Fn(usize) -> Vec<Vec<u8>>,
    period: Duration,
    count: usize,
) -> Instant {
    for i in 0..count {
        let due = start + period * i as u32;
        while Instant::now() < due {
            std::thread::sleep(Duration::from_micros(500));
        }
        for m in msgs(i) {
            s.midi_keyboard().send(&m);
        }
        if i % 4 == 0 {
            s.tick(0.005);
        }
    }
    s.tick(0.005);
    start + period * count as u32
}

fn stream(s: &mut Session, msg: impl Fn(usize) -> Vec<u8>, period: Duration, count: usize) {
    stream_from(s, Instant::now(), |i| vec![msg(i)], period, count);
}

/// A 120 BPM MIDI clock master whose clock keeps its grid across calls.
struct Master(Instant);

impl Master {
    fn new() -> Self {
        Self(Instant::now())
    }

    /// `count` clocks, with Start sent right before clock `start_at`.
    fn clock(&mut self, s: &mut Session, start_at: usize, count: usize) {
        let tick = Duration::from_micros(500_000 / 24);
        self.0 = stream_from(
            s,
            self.0,
            |i| {
                if i == start_at {
                    vec![vec![0xFA], vec![0xF8]]
                } else {
                    vec![vec![0xF8]]
                }
            },
            tick,
            count,
        );
    }
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "millisecond MIDI timing; the macOS/Windows CI machines oversleep"
)]
fn follows_midi_clock_start_stop_and_song_position() {
    let mut s = session();
    s.set_sync_settings(SyncSettings {
        source: SyncSource::MidiClock,
        ..SyncSettings::default()
    });
    // Clock before start: tempo known, transport still.
    let mut master = Master::new();
    master.clock(&mut s, usize::MAX, 24);
    let st = s.sync_status();
    assert!((st.tempo.unwrap() - 120.0).abs() < 1.0, "{:?}", st.tempo);
    assert!(!s.transport().playing);
    // Start: the next clock is beat 0; two beats of clock (continuing the
    // grid).
    master.clock(&mut s, 0, 48);
    assert!(s.transport().playing, "started by the master");
    let st = s.sync_status();
    assert!(st.running);
    assert!(
        st.error_ms.abs() < 15.0,
        "within the tolerance: {:.2} ms",
        st.error_ms
    );
    assert_eq!(st.relocks, 0);
    // Playing about a second in (48 clocks = 2 beats at 120 BPM).
    let pos = s.transport().position as f64 / 48_000.0;
    assert!((0.85..1.2).contains(&pos), "position {pos:.3} s");
    // Stop, then song position 32 sixteenths = bar 3.
    s.midi_keyboard().send(&[0xFC]);
    s.midi_keyboard().send(&[0xF2, 32, 0]);
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(60) {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
    assert!(!s.transport().playing);
    // Bar 3 at the (followed, ~120 BPM) project tempo.
    let pos = s.transport().position as f64;
    assert!((pos - 192_000.0).abs() < 192.0, "position {pos}");
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "millisecond MIDI timing; the macOS/Windows CI machines oversleep"
)]
fn chases_mtc_and_stops_when_it_ends() {
    let mut s = session();
    s.set_sync_settings(SyncSettings {
        source: SyncSource::Mtc,
        offset: Timecode::parse("00:00:00:00").unwrap(),
        ..SyncSettings::default()
    });
    // 25 fps from 00:00:05:00: a quarter frame every 10 ms.
    let qf = |i: usize| {
        let frames = 5 * 25 + 2 * (i / 8);
        let (sec, fr) = ((frames / 25) as u8, (frames % 25) as u8);
        let piece = (i % 8) as u8;
        let value = match piece {
            0 => fr & 0x0F,
            1 => fr >> 4,
            2 => sec & 0x0F,
            3 => sec >> 4,
            7 => 1 << 1, // 25 fps
            _ => 0,
        };
        vec![0xF1, piece << 4 | value]
    };
    stream(&mut s, qf, Duration::from_millis(10), 100);
    assert!(s.transport().playing, "chasing the timecode");
    let st = s.sync_status();
    let (tc, _) = st.timecode.unwrap();
    assert_eq!((tc.seconds, tc.minutes, tc.hours), (5, 0, 0));
    assert!(st.error_ms.abs() < 15.0, "{:.2} ms", st.error_ms);
    let pos = s.transport().position as f64 / 48_000.0;
    assert!((5.8..6.3).contains(&pos), "position {pos:.3} s");
    // The timecode stops: so does the transport.
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(250) {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
    assert!(!s.transport().playing);
    assert!(!s.sync_status().running);
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "millisecond MIDI timing; the macOS/Windows CI machines oversleep"
)]
fn takes_the_masters_tempo_when_it_starts() {
    let mut s = session();
    s.dispatch(faderframe_session::Action::Edit(
        faderframe_project::Command::SetTempo { bpm: 100.0 },
    ))
    .unwrap();
    s.set_sync_settings(SyncSettings {
        source: SyncSource::MidiClock,
        ..SyncSettings::default()
    });
    let mut master = Master::new();
    master.clock(&mut s, usize::MAX, 72);
    assert!(s.sync_status().tempo_differs);
    master.clock(&mut s, 0, 72);
    let bpm = s.project().timeline.tempo.points()[0].bpm;
    assert!((bpm - 120.0).abs() < 0.5, "project tempo {bpm}");
    let st = s.sync_status();
    assert!(!st.tempo_differs);
    assert_eq!(st.relocks, 0, "in step after following the tempo");
    // One undo step restores the old tempo.
    assert_eq!(s.history().undo_label(), Some("Change Tempo"));
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "millisecond MIDI timing; the macOS/Windows CI machines oversleep"
)]
fn follows_a_drifting_clock_by_varispeed_without_jumps() {
    let mut s = session();
    s.set_sync_settings(SyncSettings {
        source: SyncSource::MidiClock,
        follow_tempo: false,
        ..SyncSettings::default()
    });
    // A master whose clock runs 0.5 % fast against ours (no shared word
    // clock): 120 BPM by its crystal.
    let tick = Duration::from_secs_f64(0.5 / 24.0 / 1.005);
    let mut at = Instant::now();
    at = stream_from(
        &mut s,
        at,
        |i| {
            if i == 24 {
                vec![vec![0xFA], vec![0xF8]]
            } else {
                vec![vec![0xF8]]
            }
        },
        tick,
        24 + 48 * 6,
    );
    let st = s.sync_status();
    assert!(st.running);
    // It keeps up by speed, not by jumping.
    assert_eq!(st.relocks, 0, "no relocks");
    assert!(
        (st.speed - 1.005).abs() < 0.002,
        "playing at the master's speed: {:.4}",
        st.speed
    );
    assert!(st.error_ms.abs() < 10.0, "{:.2} ms", st.error_ms);
    let _ = at;
}
