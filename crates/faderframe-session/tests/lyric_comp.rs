//! Comping by lyric: three synthetic takes of a sung line heard by a
//! stand-in transcriber (no model needed), lined up word by word, the
//! comp chosen per word with boundaries in the gaps between words, the
//! best take suggested per word with its reasons, and the syllables under
//! the pitch editor's notes.
//!
//! The opt-in path through Whisper uses the speech crate's variables:
//!
//!     FADERFRAME_TEST_WHISPER=<checkpoint dir> \
//!     FADERFRAME_TEST_SPEECH=<16 kHz mono f32 raw file> \
//!     cargo test -p faderframe-session --test lyric_comp -- --ignored --nocapture
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::ClipId;
use faderframe_engine::EngineConfig;
use faderframe_project::{ClipContent, Command, Project, Take, TakeFolder};
use faderframe_session::lyric_comp::{LyricCompOp, Transcriber, WhisperTranscriber};
use faderframe_session::{Action, Session};
use faderframe_speech::{Segment, Word};
use faderframe_timeline::MusicalTime;
use std::f64::consts::TAU;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const SR: u32 = 48_000;
/// The line: each word sung on its own note (MIDI), so the stand-in can
/// tell them apart.
const LINE: [(&str, i32, f64); 5] = [
    ("Shine", 57, 0.35),
    ("a", 60, 0.20),
    ("light", 64, 0.40),
    ("on", 67, 0.30),
    ("me.", 71, 0.60),
];
/// Where each take starts each word (seconds; `None`: left out).
const STARTS: [[Option<f64>; 5]; 3] = [
    [Some(0.50), Some(1.00), Some(1.50), Some(2.00), Some(2.50)],
    [Some(0.54), Some(1.04), Some(1.46), Some(2.04), Some(2.56)],
    [Some(0.50), Some(1.00), None, Some(2.00), Some(2.50)],
];
const LENGTH: f64 = 4.0;

fn hz(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

/// Take `t` (0-based): take 1 sings "light" a quarter tone flat, take 3
/// has a pop on "Shine" and clips "me".
fn take(t: usize) -> Vec<f32> {
    let n = (LENGTH * f64::from(SR)) as usize;
    let mut x = vec![0.0f32; n];
    let mut seed = 11u32 + t as u32;
    for v in x.iter_mut() {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *v = (f64::from(seed >> 8) / f64::from(1u32 << 24) * 2.0 - 1.0) as f32 * 0.0003;
    }
    for (w, &(_, note, dur)) in LINE.iter().enumerate() {
        let Some(at) = STARTS[t][w] else { continue };
        let detune = if t == 0 && w == 2 { -0.5 } else { 0.0 };
        let level = if t == 2 && w == 4 { 1.6 } else { 0.4 };
        let f = hz(f64::from(note) + detune);
        let (i0, len) = (
            (at * f64::from(SR)) as usize,
            (dur * f64::from(SR)) as usize,
        );
        for i in 0..len {
            let s = i as f64 / f64::from(SR);
            let env = (s / 0.015).min(1.0).min((dur - s) / 0.03).max(0.0);
            let v = level * env * ((TAU * f * s).sin() + 0.2 * (TAU * 2.0 * f * s).sin());
            x[i0 + i] = (f64::from(x[i0 + i]) + v).clamp(-1.0, 1.0) as f32;
        }
        if t == 2 && w == 0 {
            // A pop: a thump of air at the word's start.
            for i in 0..(0.04 * f64::from(SR)) as usize {
                let s = i as f64 / f64::from(SR);
                let v = 0.8 * (TAU * 45.0 * s).sin() * (-s * 60.0).exp();
                let k = i0.saturating_sub(480) + i;
                x[k] = (f64::from(x[k]) + v).clamp(-1.0, 1.0) as f32;
            }
        }
    }
    x
}

/// Hears words the way the model does, coarsely: a burst of tone is a
/// word, named by its pitch; each starts 60 ms early and runs to the next.
struct StandIn {
    calls: AtomicUsize,
}

impl Transcriber for StandIn {
    fn ready(&self) -> Result<(), String> {
        Ok(())
    }

    fn transcribe(
        &self,
        parts: &[Vec<f32>],
        progress: &dyn Fn(f32),
    ) -> Result<Vec<Vec<Segment>>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        progress(0.5);
        Ok(parts.iter().map(|p| vec![listen(p)]).collect())
    }
}

fn listen(x: &[f32]) -> Segment {
    let rate = 16_000.0;
    let frame = 160;
    let loud: Vec<bool> = x
        .chunks(frame)
        .map(|c| c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32 > 1e-3)
        .collect();
    let mut bursts = Vec::new();
    let mut i = 0;
    while i < loud.len() {
        if loud[i] {
            let a = i;
            while i < loud.len() && loud[i] {
                i += 1;
            }
            bursts.push((a * frame, (i * frame).min(x.len())));
        }
        i += 1;
    }
    let mut words: Vec<Word> = Vec::new();
    for &(a, b) in &bursts {
        let part = &x[a..b];
        let crossings = part
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        let f = crossings as f64 / 2.0 / (part.len() as f64 / rate);
        let note = 69.0 + 12.0 * (f / 440.0).log2();
        let text = LINE
            .iter()
            .min_by(|p, q| {
                (f64::from(p.1) - note)
                    .abs()
                    .total_cmp(&(f64::from(q.1) - note).abs())
            })
            .unwrap()
            .0;
        words.push(Word {
            start: (a as f64 / rate - 0.06).max(0.0),
            end: b as f64 / rate,
            text: format!(" {text}"),
            probability: 0.9,
        });
    }
    for i in 0..words.len().saturating_sub(1) {
        words[i].end = words[i + 1].start;
    }
    Segment {
        start: words.first().map_or(0.0, |w| w.start),
        end: words.last().map_or(0.0, |w| w.end),
        text: words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<String>()
            .trim()
            .into(),
        words,
    }
}

fn import(s: &mut Session, name: &str, samples: Vec<f32>) -> PathBuf {
    // A folder per import: the tests run side by side.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ff-session-lyric-comp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("{name}.wav"));
    write_wav(&file, &[samples], SR, WavFormat::Float32, false).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file.clone()],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    file
}

/// A session with the three takes in one folder (take 1 in the comp) and
/// the stand-in transcriber.
fn session() -> (Session, ClipId, Arc<StandIn>) {
    let mut s = Session::new(Project::new("Lyrics", SR), None, EngineConfig::default()).unwrap();
    for t in 0..3 {
        import(&mut s, &format!("Vocal take {}", t + 1), take(t));
    }
    let p = s.project();
    let mut clips: Vec<_> = p.clips.values().cloned().collect();
    clips.sort_by_key(|c| c.name.clone());
    let first = clips[0].clone();
    let len = first.as_audio().unwrap().length;
    let mut f = TakeFolder::new(len);
    for (i, c) in clips.iter().enumerate() {
        let a = c.as_audio().unwrap();
        f.add_take(Take {
            name: format!("Take {}", i + 1),
            source: a.source,
            source_offset: 0,
            start: 0,
            end: len,
            gain_db: 0.0,
            rating: 0,
        });
    }
    f.use_take(0);
    s.edit(Command::SetClipContent {
        clip: first.id,
        start: first.start,
        content: Box::new(ClipContent::Takes(f)),
    })
    .unwrap();
    let stand_in = Arc::new(StandIn {
        calls: AtomicUsize::new(0),
    });
    s.set_transcriber(stand_in.clone());
    (s, first.id, stand_in)
}

fn folder(s: &Session, clip: ClipId) -> TakeFolder {
    s.project().clip(clip).unwrap().as_takes().unwrap().clone()
}

fn secs(f: i64) -> f64 {
    f as f64 / f64::from(SR)
}

/// The folder's invariants.
fn holds(f: &TakeFolder) {
    assert_eq!(f.comp[0].start, 0);
    assert!(
        f.comp
            .windows(2)
            .all(|p| p[0].start < p[1].start && p[0].take != p[1].take)
    );
    assert!(f.comp.iter().all(|s| s.start < f.length));
    assert!(
        f.comp
            .iter()
            .all(|s| s.take.is_none_or(|t| t < f.takes.len()))
    );
}

fn hear(s: &mut Session, clip: ClipId) {
    s.dispatch(Action::LyricComp(LyricCompOp::Hear { clip }))
        .unwrap();
    assert!(s.hearing_words(clip).is_some());
    s.wait_for_words();
}

#[test]
fn every_take_is_heard_and_lined_up_word_by_word() {
    let (mut s, clip, stand_in) = session();
    let steps = s.history_steps().0.len();
    hear(&mut s, clip);
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    assert_eq!(s.history_steps().0.last().unwrap(), "Hear Words");
    assert_eq!(s.project().source_words.len(), 3);
    let comp = s.lyric_comp(clip).unwrap();
    let texts: Vec<&str> = comp
        .alignment
        .columns
        .iter()
        .map(|c| c.text.as_str())
        .collect();
    assert_eq!(texts, ["Shine", "a", "light", "on", "me."]);
    assert_eq!(comp.alignment.columns[2].takes, [Some(2), Some(2), None]);
    // The coarse edges moved to the voice.
    for (t, starts) in STARTS.iter().enumerate() {
        for (c, at) in starts.iter().enumerate() {
            let Some(at) = at else { continue };
            let (a, b) = comp.alignment.span(c, t).unwrap();
            assert!(
                (secs(a) - at).abs() < 0.015,
                "take {t} word {c}: {}",
                secs(a)
            );
            let end = at + LINE[c].2;
            assert!(
                (secs(b) - end).abs() < 0.04,
                "take {t} word {c} ends {}",
                secs(b)
            );
        }
    }
    // Heard once: asking again hears nothing new.
    let calls = stand_in.calls.load(Ordering::Relaxed);
    hear(&mut s, clip);
    assert_eq!(stand_in.calls.load(Ordering::Relaxed), calls);
    assert_eq!(s.history_steps().0.len(), steps + 1);
    // Undo forgets the words; redo brings them back.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.lyric_comp(clip).is_none());
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.lyric_comp(clip).unwrap().alignment.columns.len(), 5);
}

#[test]
fn the_best_take_is_suggested_per_word_with_its_reasons() {
    let (mut s, clip, _) = session();
    hear(&mut s, clip);
    let comp = s.lyric_comp(clip).unwrap();
    assert_eq!(comp.best, [Some(0), Some(0), Some(1), Some(0), Some(0)]);
    let why = |c: usize, t: usize| comp.reasons(c, t, 8).join("; ");
    assert!(why(2, 1).contains("in tune"), "{}", why(2, 1));
    assert!(why(2, 0).contains("off pitch"), "{}", why(2, 0));
    assert!(why(4, 2).contains("clipped"), "{}", why(4, 2));
    assert!(why(0, 2).contains("plosive pop"), "{}", why(0, 2));
    assert!(why(4, 1).contains("late"), "{}", why(4, 1));
    assert!(comp.judged[2][2].is_none(), "take 3 left “light” out");
    // Nothing applied until asked: then one step.
    assert_eq!(folder(&s, clip).comp.len(), 1);
    let steps = s.history_steps().0.len();
    s.dispatch(Action::LyricComp(LyricCompOp::UseBest {
        clip,
        words: None,
    }))
    .unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1);
    let f = folder(&s, clip);
    holds(&f);
    let comp = s.lyric_comp(clip).unwrap();
    for c in 0..5 {
        assert_eq!(comp.alignment.chosen(c, &f), comp.best[c], "word {c}");
    }
}

#[test]
fn a_word_comps_with_its_boundaries_in_the_gaps() {
    let (mut s, clip, _) = session();
    hear(&mut s, clip);
    let steps = s.history_steps().0.len();
    s.dispatch(Action::LyricComp(LyricCompOp::Comp {
        clip,
        from: 2,
        to: 2,
        take: 1,
    }))
    .unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1);
    assert_eq!(
        s.history_steps().0.last().unwrap(),
        "Comp “light” from Take 2"
    );
    let f = folder(&s, clip);
    holds(&f);
    let comp = s.lyric_comp(clip).unwrap();
    assert_eq!(comp.alignment.chosen(2, &f), Some(1));
    for c in [0, 1, 3, 4] {
        assert_eq!(comp.alignment.chosen(c, &f), Some(0), "word {c}");
    }
    let bounds: Vec<f64> = f.comp.iter().skip(1).map(|x| secs(x.start)).collect();
    assert_eq!(bounds.len(), 2, "{bounds:?}");
    // In: after both takes' "a" (1.20, 1.24), before both "light"s (1.50,
    // 1.46); out: after both (1.90, 1.86), before both "on"s (2.00, 2.04).
    assert!(bounds[0] > 1.24 && bounds[0] < 1.46, "{bounds:?}");
    assert!(bounds[1] > 1.90 && bounds[1] < 2.00, "{bounds:?}");
    // On take 2's zero crossings.
    let audio = take(1);
    for x in f.comp.iter().skip(1) {
        let i = x.start as usize;
        let near = (i.saturating_sub(1)..=i + 1)
            .any(|k| (audio[k] <= 0.0) != (audio[k + 1] <= 0.0) || audio[k] == 0.0);
        assert!(near, "boundary at {} is a crossing", secs(x.start));
    }
    // The line from take 3 (it has no "light": the rest of the line is its).
    s.dispatch(Action::LyricComp(LyricCompOp::CompLine {
        clip,
        word: 0,
        take: 2,
    }))
    .unwrap();
    let f = folder(&s, clip);
    holds(&f);
    let comp = s.lyric_comp(clip).unwrap();
    for c in [0, 1, 3, 4] {
        assert_eq!(comp.alignment.chosen(c, &f), Some(2), "word {c}");
    }
    // A take that does not sing the word: refused, nothing changes.
    let before = folder(&s, clip);
    assert!(
        s.dispatch(Action::LyricComp(LyricCompOp::Comp {
            clip,
            from: 2,
            to: 2,
            take: 2,
        }))
        .is_err()
    );
    assert_eq!(folder(&s, clip), before);
    // Undo, step by step.
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(folder(&s, clip).comp.len(), 1);
}

#[test]
fn forgetting_and_hearing_again() {
    let (mut s, clip, stand_in) = session();
    hear(&mut s, clip);
    let calls = stand_in.calls.load(Ordering::Relaxed);
    s.dispatch(Action::LyricComp(LyricCompOp::HearAgain { clip }))
        .unwrap();
    s.wait_for_words();
    assert!(stand_in.calls.load(Ordering::Relaxed) > calls, "heard anew");
    assert_eq!(s.lyric_comp(clip).unwrap().alignment.columns.len(), 5);
    s.dispatch(Action::LyricComp(LyricCompOp::Forget { clip }))
        .unwrap();
    assert!(s.project().source_words.is_empty());
    assert!(s.lyric_comp(clip).is_none());
}

/// Without the model the request is refused with what to do.
#[test]
fn without_the_model_hearing_says_what_to_do() {
    let (mut s, clip, _) = session();
    let empty = std::env::temp_dir().join(format!("ff-no-model-{}", std::process::id()));
    s.set_transcriber(Arc::new(WhisperTranscriber::new(empty)));
    let e = s
        .dispatch(Action::LyricComp(LyricCompOp::Hear { clip }))
        .unwrap_err();
    assert!(e.to_string().contains("Download Speech Model"), "{e}");
    assert!(!s.hearing_any_words());
}

/// The words survive saving and opening; their sounds are measured again
/// from the tick once the folder's lanes are open.
#[test]
fn the_words_are_saved_with_the_project() {
    let (mut s, clip, stand_in) = session();
    hear(&mut s, clip);
    let dir = std::env::temp_dir().join(format!("ff-lyric-comp-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("Lyrics.ffproj");
    s.save_as(&path).unwrap();
    let words = s.project().source_words.clone();
    let mut t = Session::new(Project::new("Other", SR), None, EngineConfig::default()).unwrap();
    t.set_transcriber(stand_in.clone());
    t.open(&path).unwrap();
    t.wait_for_imports();
    assert_eq!(t.project().source_words, words);
    let comp = t.lyric_comp(clip).unwrap();
    assert!(comp.best.iter().all(Option::is_none), "not measured yet");
    let calls = stand_in.calls.load(Ordering::Relaxed);
    t.dispatch(Action::ToggleTakeLanes(clip)).unwrap();
    t.tick(0.01);
    t.wait_for_words();
    assert_eq!(
        stand_in.calls.load(Ordering::Relaxed),
        calls,
        "measured only"
    );
    assert_eq!(
        t.lyric_comp(clip).unwrap().best,
        [Some(0), Some(0), Some(1), Some(0), Some(0)]
    );
}

#[test]
fn the_pitch_editor_shows_the_syllable_under_each_note() {
    let mut s = Session::new(Project::new("Syllables", SR), None, EngineConfig::default()).unwrap();
    s.set_transcriber(Arc::new(StandIn {
        calls: AtomicUsize::new(0),
    }));
    import(&mut s, "Vocal", take(1));
    s.dispatch(Action::SetEditFlag(
        faderframe_session::EditFlag::PolyphonicPitch,
        false,
    ))
    .unwrap();
    let clip = *s.project().clips.keys().next().unwrap();
    hear(&mut s, clip);
    s.dispatch(Action::DetectPitch { clips: vec![clip] })
        .unwrap();
    let start = std::time::Instant::now();
    while s.detecting_pitch() {
        assert!(start.elapsed().as_secs() < 30, "detection hangs");
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.tick(0.005);
    }
    let syllables = s.pitch_syllables(clip);
    let texts: Vec<String> = syllables
        .iter()
        .flatten()
        .filter(|x| !x.held)
        .map(|x| x.text.clone())
        .collect();
    assert_eq!(texts, ["Shine", "a", "light", "on", "me."], "{syllables:?}");
}

/// Whisper itself, opt-in: three takes of the same speech (the second a
/// little later, the third a little quieter) heard, lined up and comped.
#[test]
#[ignore]
fn whisper_hears_the_takes() {
    let dir = PathBuf::from(std::env::var("FADERFRAME_TEST_WHISPER").unwrap());
    let raw = std::fs::read(std::env::var("FADERFRAME_TEST_SPEECH").unwrap()).unwrap();
    let speech: Vec<f32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    // To the project rate (48 kHz) by repeating samples is good enough to
    // be heard; the model hears it at 16 kHz again.
    let up = |delay: f64, gain: f32| {
        let mut x = vec![0.0f32; (delay * f64::from(SR)) as usize];
        x.extend(speech.iter().flat_map(|v| [v * gain; 3]));
        x
    };
    let mut s = Session::new(Project::new("Whisper", SR), None, EngineConfig::default()).unwrap();
    for (i, (d, g)) in [(0.0, 1.0), (0.12, 1.0), (0.0, 0.5)]
        .into_iter()
        .enumerate()
    {
        import(&mut s, &format!("Speech {}", i + 1), up(d, g));
    }
    let p = s.project();
    let mut clips: Vec<_> = p.clips.values().cloned().collect();
    clips.sort_by_key(|c| c.name.clone());
    let len = clips
        .iter()
        .map(|c| c.as_audio().unwrap().length)
        .max()
        .unwrap();
    let mut f = TakeFolder::new(len);
    for (i, c) in clips.iter().enumerate() {
        f.add_take(Take {
            name: format!("Take {}", i + 1),
            source: c.as_audio().unwrap().source,
            source_offset: 0,
            start: 0,
            end: c.as_audio().unwrap().length,
            gain_db: 0.0,
            rating: 0,
        });
    }
    f.use_take(0);
    let clip = clips[0].id;
    s.edit(Command::SetClipContent {
        clip,
        start: clips[0].start,
        content: Box::new(ClipContent::Takes(f)),
    })
    .unwrap();
    s.set_transcriber(Arc::new(WhisperTranscriber::new(dir)));
    let t0 = std::time::Instant::now();
    hear(&mut s, clip);
    eprintln!("heard three takes in {:?}", t0.elapsed());
    let comp = s.lyric_comp(clip).unwrap();
    for (c, col) in comp.alignment.columns.iter().enumerate() {
        let spans: Vec<String> = (0..3)
            .map(|t| {
                comp.alignment.span(c, t).map_or("—".into(), |(a, b)| {
                    format!("{:5.2}–{:5.2}", secs(a), secs(b))
                })
            })
            .collect();
        eprintln!(
            "{:>12} {} best {:?}",
            col.text,
            spans.join("  "),
            comp.best[c].map(|t| t + 1)
        );
    }
    let text: String = comp
        .alignment
        .columns
        .iter()
        .map(|c| c.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("ask not what your country"), "{text}");
    // Most words are sung by every take, the second take ~120 ms later.
    let all = comp
        .alignment
        .columns
        .iter()
        .filter(|c| c.takes.iter().all(Option::is_some))
        .count();
    assert!(all * 10 >= comp.alignment.columns.len() * 8, "{all}");
    let c = comp
        .alignment
        .columns
        .iter()
        .position(|c| c.text.to_lowercase().starts_with("country"))
        .unwrap();
    let (a0, _) = comp.alignment.span(c, 0).unwrap();
    let (a1, _) = comp.alignment.span(c, 1).unwrap();
    assert!((secs(a1 - a0) - 0.12).abs() < 0.05, "{}", secs(a1 - a0));
    s.dispatch(Action::LyricComp(LyricCompOp::Comp {
        clip,
        from: c,
        to: c,
        take: 1,
    }))
    .unwrap();
    let f = folder(&s, clip);
    holds(&f);
    assert_eq!(comp.alignment.chosen(c, &f), Some(1));
}
