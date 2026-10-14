//! Lead sheets from a recording: a phrase sung (harmonic tones) in time
//! is heard as its notes, written in bars with the chord track's chords
//! over it and the lyrics under it, and exported as MusicXML and PDF.
//! Several clips of a track make one line; several tracks a score.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::{ClipId, NoteId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_leadsheet::LeadSheet;
use faderframe_leadsheet::score::Clef;
use faderframe_project::lyrics::LyricLine;
use faderframe_project::{
    ChordEvent, Clip, ClipContent, Command, MidiClip, MidiNote, Project, Take, TakeFolder,
    TrackKind,
};
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
    assert_eq!(sheet.bars(), 2, "the clip's two bars");
    // The notes in order, at their values (quarter, quarter, half; two
    // eighths, a quarter, a half).
    let notes: Vec<(u8, u32)> = sheet.parts[0]
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
    let chords: Vec<(usize, u32, String)> = sheet.parts[0]
        .measures
        .iter()
        .enumerate()
        .flat_map(|(i, m)| m.chords.iter().map(move |c| (i, c.at, c.text())))
        .collect();
    assert_eq!(
        chords,
        [(0, 0, "C".into()), (1, 0, "F".into()), (1, 24, "C".into())]
    );
    let words: Vec<String> = sheet.parts[0]
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
    assert_eq!(doc.sheet.bars(), 3);
    let keys: Vec<u8> = doc.sheet.parts[0]
        .measures
        .iter()
        .flat_map(|m| m.events.iter())
        .filter(|e| !e.is_rest())
        .map(|e| e.pitch.unwrap().midi)
        .collect();
    assert_eq!(keys, [64, 67, 65, 69], "both clips' melody");
    assert!(doc.sheet.parts[0].measures[1].events[0].bar_rest);
    // The chords the keys play.
    let heard: Vec<String> = doc.sheet.parts[0]
        .measures
        .iter()
        .flat_map(|m| m.chords.iter().map(|c| c.text()))
        .collect();
    assert_eq!(heard, ["C", "F"], "{heard:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A MIDI clip on `track` at `at` (quarters), `length` long, with notes
/// `(start, length, key)` in quarters from its start.
fn midi_clip(id: u64, track: TrackId, at: f64, length: f64, notes: &[(f64, f64, u8)]) -> Action {
    let q = MusicalTime::from_quarters;
    Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id: ClipId(id),
            track,
            name: format!("clip {id}"),
            color: None,
            start: q(at),
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: q(length),
                notes: notes
                    .iter()
                    .enumerate()
                    .map(|(i, &(a, l, key))| MidiNote {
                        id: NoteId(id * 100 + i as u64),
                        start: q(a),
                        length: q(l),
                        key,
                        velocity: 100,
                        channel: 0,
                        muted: false,
                        release: None,
                    })
                    .collect(),
                ..MidiClip::default()
            }),
        }),
    })
}

/// A part's notes in order (sounding keys, a tied note once).
fn keys_of(sheet: &LeadSheet, part: usize) -> Vec<u8> {
    sheet.parts[part]
        .measures
        .iter()
        .flat_map(|m| m.events.iter())
        .filter(|e| !e.is_rest() && !e.tied_from)
        .map(|e| e.pitch.unwrap().midi)
        .collect()
}

fn named_track(s: &mut Session, kind: TrackKind, name: &str) -> TrackId {
    let t = s.add_track(kind).unwrap();
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: t,
        name: name.into(),
    }))
    .unwrap();
    t
}

/// A track's clips — or some of them — make one line on one timeline:
/// rests between them, notes past a clip's end left out, and where two
/// overlap the later one's notes.
#[test]
fn the_clips_of_a_track_make_one_line() {
    let mut s = Session::new(Project::new("Line", SR), None, EngineConfig::default()).unwrap();
    let t = named_track(&mut s, TrackKind::Midi, "Melody");
    // Bar 1; a note after the clip's end is not played.
    s.dispatch(midi_clip(
        1,
        t,
        0.0,
        4.0,
        &[
            (0.0, 1.0, 60),
            (1.0, 1.0, 62),
            (2.0, 2.0, 64),
            (4.0, 1.0, 72),
        ],
    ))
    .unwrap();
    // Bar 3.
    s.dispatch(midi_clip(2, t, 8.0, 4.0, &[(0.0, 2.0, 65), (2.0, 2.0, 67)]))
        .unwrap();
    // On top of bar 3's second half.
    s.dispatch(midi_clip(3, t, 10.0, 2.0, &[(0.0, 2.0, 69)]))
        .unwrap();
    s.dispatch(Action::MakeLeadSheet {
        of: LeadSheetOf::Track(t),
        grid: Grid::Auto,
    })
    .unwrap();
    let doc = s.lead_sheet().expect("a lead sheet");
    assert_eq!(doc.sheet.parts.len(), 1);
    assert_eq!(doc.sheet.bars(), 3);
    assert!(doc.sheet.parts[0].measures[1].events[0].bar_rest, "the gap");
    assert_eq!(
        keys_of(&doc.sheet, 0),
        [60, 62, 64, 65, 69],
        "the clip on top"
    );
    // Two of its clips, chosen in any order: the line of those.
    s.dispatch(Action::MakeLeadSheet {
        of: LeadSheetOf::Clips(vec![ClipId(2), ClipId(1)]),
        grid: Grid::Auto,
    })
    .unwrap();
    let doc = s.lead_sheet().unwrap();
    assert_eq!(doc.of, LeadSheetOf::Clips(vec![ClipId(2), ClipId(1)]));
    assert_eq!(doc.sheet.bars(), 3);
    assert_eq!(keys_of(&doc.sheet, 0), [60, 62, 64, 65, 67]);
    assert_eq!(doc.parts(), "Melody");
}

/// Several tracks make a score: a staff each in the order asked (clips
/// chosen on several tracks: as the arranger lists them), each with its
/// clef, the words under the voice, the chords — with no chord track and
/// no other tracks — from what the staves play together over the top one,
/// and a MusicXML part a staff.
#[test]
fn tracks_make_a_score_a_staff_each() {
    let mut s = Session::new(Project::new("Duet", SR), None, EngineConfig::default()).unwrap();
    let voice = named_track(&mut s, TrackKind::Midi, "Lead Vocal");
    let bass = named_track(&mut s, TrackKind::Midi, "Bass");
    s.dispatch(midi_clip(
        1,
        voice,
        0.0,
        8.0,
        &[
            (0.0, 1.0, 64),
            (1.0, 1.0, 67),
            (2.0, 2.0, 72),
            (4.0, 1.0, 74),
            (5.0, 1.0, 71),
            (6.0, 2.0, 67),
        ],
    ))
    .unwrap();
    s.dispatch(midi_clip(
        2,
        bass,
        0.0,
        8.0,
        &[(0.0, 4.0, 36), (4.0, 4.0, 43)],
    ))
    .unwrap();
    let q = MusicalTime::from_quarters;
    s.dispatch(Action::Edit(Command::SetLyrics {
        lyrics: vec![LyricLine {
            start: q(0.0),
            end: q(8.0),
            text: "Here comes the sun a gain".into(),
        }],
    }))
    .unwrap();
    s.dispatch(Action::MakeLeadSheet {
        of: LeadSheetOf::Tracks(vec![voice, bass]),
        grid: Grid::Auto,
    })
    .unwrap();
    let doc = s.lead_sheet().expect("a score");
    let sheet = &doc.sheet;
    assert_eq!(doc.parts(), "Lead Vocal, Bass");
    assert_eq!(sheet.bars(), 2);
    assert_eq!(sheet.parts[0].clef, Clef::Treble);
    assert_eq!(sheet.parts[1].clef, Clef::Bass);
    assert_eq!(keys_of(sheet, 0), [64, 67, 72, 74, 71, 67]);
    assert_eq!(keys_of(sheet, 1), [36, 43]);
    assert!(sheet.parts[0].has_lyrics() && !sheet.parts[1].has_lyrics());
    let chords: Vec<String> = sheet.parts[0]
        .measures
        .iter()
        .flat_map(|m| m.chords.iter().map(|c| c.text()))
        .collect();
    assert_eq!(chords, ["C", "G"], "{chords:?}");
    assert!(sheet.parts[1].measures.iter().all(|m| m.chords.is_empty()));
    assert!(!doc.pages.is_empty());
    let dir = std::env::temp_dir().join(format!("ff-session-score-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["score.pdf", "score.musicxml"] {
        s.dispatch(Action::ExportLeadSheet(dir.join(name))).unwrap();
        assert!(std::fs::metadata(dir.join(name)).unwrap().len() > 1000);
    }
    let xml = std::fs::read_to_string(dir.join("score.musicxml")).unwrap();
    assert_eq!(xml.matches("<part id=").count(), 2);
    assert!(xml.contains("<part-name>Lead Vocal</part-name>"));
    assert!(xml.contains("<score-part id=\"P2\"><part-name>Bass</part-name>"));
    if let Ok(out) = std::env::var("FADERFRAME_LEADSHEET_OUT") {
        let _ = std::fs::copy(
            dir.join("score.pdf"),
            std::path::Path::new(&out).join("session-score.pdf"),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    // The order asked is the order on the page; the words still go to the
    // voice.
    s.dispatch(Action::MakeLeadSheet {
        of: LeadSheetOf::Tracks(vec![bass, voice]),
        grid: Grid::Auto,
    })
    .unwrap();
    let sheet = &s.lead_sheet().unwrap().sheet;
    assert_eq!(sheet.parts[0].name, "Bass");
    assert!(sheet.parts[1].has_lyrics());
    // Clips chosen on both tracks: the tracks' order.
    s.dispatch(Action::MakeLeadSheet {
        of: LeadSheetOf::Clips(vec![ClipId(2), ClipId(1)]),
        grid: Grid::Straight,
    })
    .unwrap();
    let doc = s.lead_sheet().unwrap();
    assert_eq!(doc.parts(), "Lead Vocal, Bass");
    assert_eq!(doc.grid, Grid::Straight);
}

/// A take folder is heard as its comp plays: the first half of one take,
/// the second of another.
#[test]
fn a_take_folder_is_heard_as_its_comp_plays() {
    let mut s = Session::new(Project::new("Comp", SR), None, EngineConfig::default()).unwrap();
    let bpm = s.project().timeline.tempo.bpm_at(MusicalTime::ZERO);
    let dir = std::env::temp_dir().join(format!("ff-session-lead-takes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let q = MusicalTime::from_quarters;
    let mut sources = Vec::new();
    for (name, tune) in [("A", [60, 62, 64, 65]), ("B", [67, 69, 71, 72])] {
        let file = dir.join(format!("{name}.wav"));
        let parts: Vec<(u8, f64)> = tune.iter().map(|k| (*k, 1.0)).collect();
        write_wav(&file, &[phrase(&parts, bpm)], SR, WavFormat::Float32, false).unwrap();
        s.dispatch(Action::ImportFiles {
            files: vec![file],
            track: None,
            at: q(0.0),
        })
        .unwrap();
        s.wait_for_imports();
        let c = s
            .project()
            .clips
            .values()
            .find(|c| c.name.starts_with(name))
            .unwrap()
            .clone();
        sources.push((c.id, c.track, c.as_audio().unwrap().source));
    }
    let len = (4.0 * 60.0 / bpm * f64::from(SR)) as i64;
    let mut folder = TakeFolder::new(len);
    for (i, (_, _, source)) in sources.iter().enumerate() {
        folder.add_take(Take {
            name: format!("Take {}", i + 1),
            source: *source,
            source_offset: 0,
            start: 0,
            end: len,
            gain_db: 0.0,
            rating: 0,
        });
    }
    folder.set_comp(0, len / 2, Some(0));
    folder.set_comp(len / 2, len, Some(1));
    let track = sources[0].1;
    for (clip, _, _) in &sources {
        s.dispatch(Action::Edit(Command::RemoveClip { clip: *clip }))
            .unwrap();
    }
    s.dispatch(Action::Edit(Command::AddClip {
        clip: Box::new(Clip {
            id: ClipId(7000),
            track,
            name: "Comp".into(),
            color: None,
            start: q(4.0),
            muted: false,
            content: ClipContent::Takes(folder),
        }),
    }))
    .unwrap();
    s.dispatch(Action::MakeLeadSheet {
        of: LeadSheetOf::Track(track),
        grid: Grid::Auto,
    })
    .unwrap();
    assert!(s.making_lead_sheet());
    s.wait_for_lead_sheet();
    let doc = s.lead_sheet().expect("a lead sheet");
    assert_eq!(doc.sheet.bars(), 1);
    assert_eq!(keys_of(&doc.sheet, 0), [60, 62, 71, 72]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Guitar tab: in a score the guitar's staff gets its chords and their
/// tablature (the voice keeps its staff and the words, a syllable a note);
/// the style changes to tablature alone and another tuning on the sheet
/// made; plain-text tab comes out.
#[test]
fn a_guitar_staff_becomes_tab_with_the_words_on_the_voice() {
    use faderframe_leadsheet::score::Syllabic;
    use faderframe_session::leadsheet::{Notation, SheetStyle};
    let mut s = Session::new(Project::new("Tab", SR), None, EngineConfig::default()).unwrap();
    let voice = named_track(&mut s, TrackKind::Midi, "Vocals");
    let guitar = named_track(&mut s, TrackKind::Midi, "Acoustic Guitar");
    s.dispatch(midi_clip(
        1,
        voice,
        0.0,
        4.0,
        &[
            (0.0, 1.0, 67),
            (1.0, 1.0, 69),
            (2.0, 1.0, 67),
            (3.0, 1.0, 64),
        ],
    ))
    .unwrap();
    // C for two beats, G for two.
    let mut strums = Vec::new();
    for (at, chord) in [
        (0.0, &[48u8, 52, 55, 60, 64][..]),
        (2.0, &[43, 47, 50, 55, 59, 67][..]),
    ] {
        for k in chord {
            strums.push((at, 2.0, *k));
        }
    }
    s.dispatch(midi_clip(2, guitar, 0.0, 4.0, &strums)).unwrap();
    s.dispatch(Action::Edit(Command::SetLyrics {
        lyrics: vec![LyricLine {
            start: MusicalTime::ZERO,
            end: MusicalTime::from_quarters(4.0),
            text: "beautiful day".into(),
        }],
    }))
    .unwrap();
    s.dispatch(Action::MakeTab {
        of: LeadSheetOf::Tracks(vec![voice, guitar]),
    })
    .unwrap();
    s.wait_for_lead_sheet();
    let doc = s.lead_sheet().unwrap();
    assert_eq!(doc.style.notation, Notation::WithTab);
    let parts = &doc.sheet.parts;
    assert_eq!(parts.len(), 3, "voice, guitar, the guitar's tab");
    assert!(parts[0].tab.is_none() && parts[1].tab.is_none());
    assert_eq!(parts[1].name, "Acoustic Guitar");
    let tab = &parts[2];
    assert!(tab.tab.is_some());
    let first = &tab.measures[0].events[0];
    assert_eq!(first.frets.len(), 5, "the whole C chord: {first:?}");
    // The words under the voice, a syllable a note.
    let words: Vec<(String, Syllabic)> = parts[0].measures[0]
        .events
        .iter()
        .filter_map(|e| e.lyric.as_ref().map(|l| (l.text.clone(), l.syllabic)))
        .collect();
    assert_eq!(
        words,
        [
            ("beau".into(), Syllabic::Begin),
            ("ti".into(), Syllabic::Middle),
            ("ful".into(), Syllabic::End),
            ("day".into(), Syllabic::Single),
        ]
    );
    assert!(!parts[1].has_lyrics() && !tab.has_lyrics());
    // Tablature alone, in Drop D, a capo at 2: written again.
    s.dispatch(Action::SetSheetStyle(SheetStyle {
        notation: Notation::Tab,
        tuning: 1,
        capo: 2,
    }))
    .unwrap();
    s.wait_for_lead_sheet();
    let doc = s.lead_sheet().unwrap();
    assert_eq!(doc.sheet.parts.len(), 2);
    let t = doc.sheet.parts[1].tab.as_ref().unwrap();
    assert!(t.rhythm);
    assert_eq!(t.capo, 2);
    assert_eq!(*t.strings.last().unwrap(), 38, "Drop D");
    let dir = std::env::temp_dir().join(format!("ff-session-tab-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let txt = dir.join("tab.txt");
    s.dispatch(Action::ExportLeadSheet(txt.clone())).unwrap();
    let text = std::fs::read_to_string(&txt).unwrap();
    assert!(text.contains("Capo 2") && text.contains("e|"), "{text}");
    let xml = dir.join("tab.musicxml");
    s.dispatch(Action::ExportLeadSheet(xml.clone())).unwrap();
    let xml = std::fs::read_to_string(&xml).unwrap();
    assert!(xml.contains("<capo>2</capo>") && xml.contains("<sign>TAB</sign>"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A recorded guitar's chords are heard as chords for its tab, not as a
/// melody.
#[test]
fn recorded_chords_are_heard_as_chords_for_tab() {
    let mut s = Session::new(Project::new("Strum", SR), None, EngineConfig::default()).unwrap();
    let bpm = s.project().timeline.tempo.bpm_at(MusicalTime::ZERO);
    let dir = std::env::temp_dir().join(format!("ff-session-tab-audio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Guitar.wav");
    write_wav(
        &file,
        &[chords(
            &[(&[48, 52, 55, 60, 64], 4.0), (&[43, 47, 50, 55, 59], 4.0)],
            bpm,
        )],
        SR,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    s.dispatch(Action::MakeTab {
        of: LeadSheetOf::Clip(clip),
    })
    .unwrap();
    s.wait_for_lead_sheet();
    let doc = s.lead_sheet().expect("a tab");
    let tab = doc.sheet.parts.iter().find(|p| p.tab.is_some()).unwrap();
    let widest = tab
        .measures
        .iter()
        .flat_map(|m| &m.events)
        .map(|e| e.frets.len())
        .max()
        .unwrap();
    assert!(widest >= 3, "chords, not a melody: {widest} notes at most");
    let _ = std::fs::remove_dir_all(&dir);
}
