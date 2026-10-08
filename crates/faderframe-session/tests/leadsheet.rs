//! Lead sheets from a recording: a phrase sung (harmonic tones) in time
//! is heard as its notes, written in bars with the chord track's chords
//! over it and the lyrics under it, and exported as MusicXML and PDF.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_project::lyrics::LyricLine;
use faderframe_project::{ChordEvent, Command, Project};
use faderframe_session::leadsheet::{Grid, LeadSheetOf};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

/// Harmonic tones, each `(key, quarters)` in turn at `bpm`, a short
/// breath between them.
fn phrase(parts: &[(u8, f64)], bpm: f64) -> Vec<f32> {
    let quarter = 60.0 / bpm;
    let mut out = Vec::new();
    for &(key, quarters) in parts {
        let secs = quarters * quarter;
        let n = (secs * f64::from(SR)) as usize;
        let f = 440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0);
        for i in 0..n {
            let t = i as f64 / f64::from(SR);
            let env = (t / 0.01).min(1.0) * ((secs - 0.03 - t) / 0.02).clamp(0.0, 1.0);
            let v: f64 = (1..=6)
                .map(|h| (std::f64::consts::TAU * f * h as f64 * t).sin() / (h * h) as f64)
                .sum();
            out.push((v * 0.3 * env) as f32);
        }
    }
    out
}

#[test]
fn a_recorded_melody_becomes_a_lead_sheet() {
    let mut s = Session::new(Project::new("Tune", SR), None, EngineConfig::default()).unwrap();
    let bpm = s.project().timeline.tempo.bpm_at(MusicalTime::ZERO);
    let tune = [
        (60, 1.0),
        (62, 1.0),
        (64, 2.0),
        (65, 0.5),
        (67, 0.5),
        (69, 1.0),
        (67, 2.0),
    ];
    let dir = std::env::temp_dir().join(format!("ff-session-lead-sheet-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Tune.wav");
    write_wav(&file, &[phrase(&tune, bpm)], SR, WavFormat::Float32, false).unwrap();
    let q = MusicalTime::from_quarters;
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: q(4.0),
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    // C for a bar, then F and C over the second.
    let chord = |a: f64, b: f64, text: &str| ChordEvent {
        start: q(a),
        end: q(b),
        chord: faderframe_project::harmony::Chord::parse(text).unwrap(),
    };
    s.dispatch(Action::Edit(Command::SetChords {
        chords: vec![
            chord(4.0, 8.0, "C"),
            chord(8.0, 10.0, "F"),
            chord(10.0, 12.0, "C"),
        ],
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetLyrics {
        lyrics: vec![LyricLine {
            start: q(4.0),
            end: q(12.0),
            text: "Do re mi fa so la so".into(),
        }],
    }))
    .unwrap();
    s.dispatch(Action::MakeLeadSheet {
        of: LeadSheetOf::Clip(clip),
        grid: Grid::Auto,
    })
    .unwrap();
    assert!(s.making_lead_sheet());
    s.wait_for_lead_sheet();
    let doc = s.lead_sheet().expect("a lead sheet");
    let sheet = &doc.sheet;
    assert_eq!(sheet.measures.len(), 2, "the clip's two bars");
    // The notes in order, at their values (quarter, quarter, half; two
    // eighths, a quarter, a half).
    let notes: Vec<(u8, u32)> = sheet
        .measures
        .iter()
        .flat_map(|m| m.events.iter())
        .filter(|e| !e.is_rest())
        .map(|e| (e.pitch.unwrap().midi, e.duration.divisions()))
        .collect();
    assert_eq!(
        notes,
        [
            (60, 12),
            (62, 12),
            (64, 24),
            (65, 6),
            (67, 6),
            (69, 12),
            (67, 24)
        ],
        "{notes:?}"
    );
    // C major, the chords over their beats, the words under their notes.
    assert_eq!(sheet.key.fifths, 0);
    let chords: Vec<(usize, u32, String)> = sheet
        .measures
        .iter()
        .enumerate()
        .flat_map(|(i, m)| m.chords.iter().map(move |c| (i, c.at, c.text())))
        .collect();
    assert_eq!(
        chords,
        [(0, 0, "C".into()), (1, 0, "F".into()), (1, 24, "C".into())]
    );
    let words: Vec<String> = sheet
        .measures
        .iter()
        .flat_map(|m| m.events.iter())
        .filter_map(|e| e.lyric.as_ref().map(|l| l.text.clone()))
        .collect();
    assert_eq!(words, ["Do", "re", "mi", "fa", "so", "la", "so"]);
    assert!(!doc.pages.is_empty());
    // Exports; the triplet grid writes it again without listening.
    for name in ["tune.pdf", "tune.musicxml"] {
        let path = dir.join(name);
        s.dispatch(Action::ExportLeadSheet(path.clone())).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 1000);
    }
    let xml = std::fs::read_to_string(dir.join("tune.musicxml")).unwrap();
    assert!(xml.contains("<text>la</text>"));
    s.dispatch(Action::MakeLeadSheet {
        of: LeadSheetOf::Clip(clip),
        grid: Grid::Straight,
    })
    .unwrap();
    assert!(!s.making_lead_sheet(), "the melody is kept");
    assert_eq!(s.lead_sheet().unwrap().grid, Grid::Straight);
    if let Ok(out) = std::env::var("FADERFRAME_LEADSHEET_OUT") {
        let _ = std::fs::copy(
            dir.join("tune.pdf"),
            std::path::Path::new(&out).join("session.pdf"),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Harmonic chords, each `(keys, quarters)` in turn at `bpm`.
fn chords(parts: &[(&[u8], f64)], bpm: f64) -> Vec<f32> {
    let quarter = 60.0 / bpm;
    let mut out = Vec::new();
    for (keys, quarters) in parts {
        let secs = quarters * quarter;
        let n = (secs * f64::from(SR)) as usize;
        for i in 0..n {
            let t = i as f64 / f64::from(SR);
            let env = (t / 0.01).min(1.0) * ((secs - 0.03 - t) / 0.02).clamp(0.0, 1.0);
            let v: f64 = keys
                .iter()
                .map(|k| {
                    let f = 440.0 * 2f64.powf((f64::from(*k) - 69.0) / 12.0);
                    (1..=6)
                        .map(|h| (std::f64::consts::TAU * f * h as f64 * t).sin() / (h * h) as f64)
                        .sum::<f64>()
                })
                .sum();
            out.push((v * 0.2 * env) as f32);
        }
    }
    out
}

/// A whole audio track (two clips, apart) becomes one lead sheet, and with
/// no chord track and no MIDI the chords are heard from another audio
/// track.
#[test]
fn a_track_of_clips_with_chords_heard_from_the_audio() {
    let mut s = Session::new(Project::new("Song", SR), None, EngineConfig::default()).unwrap();
    let bpm = s.project().timeline.tempo.bpm_at(MusicalTime::ZERO);
    let dir = std::env::temp_dir().join(format!("ff-session-lead-track-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let q = MusicalTime::from_quarters;
    let import = |s: &mut Session, name: &str, samples: Vec<f32>, at: f64, track| {
        let file = dir.join(format!("{name}.wav"));
        write_wav(&file, &[samples], SR, WavFormat::Float32, false).unwrap();
        s.dispatch(Action::ImportFiles {
            files: vec![file],
            track,
            at: q(at),
        })
        .unwrap();
        s.wait_for_imports();
    };
    // The voice: a bar at bar 2, another at bar 4, on one track.
    import(
        &mut s,
        "Verse",
        phrase(&[(64, 2.0), (67, 2.0)], bpm),
        4.0,
        None,
    );
    let voice = s.project().clips.values().next().unwrap().track;
    import(
        &mut s,
        "Chorus",
        phrase(&[(65, 2.0), (69, 2.0)], bpm),
        12.0,
        Some(voice),
    );
    assert_eq!(s.project().clips_of(voice).len(), 2);
    // Keys on an audio track of their own: C for bars 2–3, F for 4.
    import(
        &mut s,
        "Keys",
        chords(&[(&[48, 52, 55], 8.0), (&[53, 57, 60], 4.0)], bpm),
        4.0,
        None,
    );
    s.dispatch(Action::MakeLeadSheet {
        of: LeadSheetOf::Track(voice),
        grid: Grid::Auto,
    })
    .unwrap();
    s.wait_for_lead_sheet();
    let doc = s.lead_sheet().expect("a lead sheet");
    assert_eq!(doc.of, LeadSheetOf::Track(voice));
    // Bars 2 to 4: three bars, the second all rest.
    assert_eq!(doc.sheet.measures.len(), 3);
    let keys: Vec<u8> = doc
        .sheet
        .measures
        .iter()
        .flat_map(|m| m.events.iter())
        .filter(|e| !e.is_rest())
        .map(|e| e.pitch.unwrap().midi)
        .collect();
    assert_eq!(keys, [64, 67, 65, 69], "both clips' melody");
    assert!(doc.sheet.measures[1].events[0].bar_rest);
    // The chords the keys play.
    let heard: Vec<String> = doc
        .sheet
        .measures
        .iter()
        .flat_map(|m| m.chords.iter().map(|c| c.text()))
        .collect();
    assert_eq!(heard, ["C", "F"], "{heard:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
