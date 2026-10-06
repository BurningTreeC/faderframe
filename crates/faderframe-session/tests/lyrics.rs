//! Lyrics: lines edited and removed one at a time (one undo step each)
//! and written out as LRC or SRT; transcribing without the model says how
//! to get it.
#![allow(clippy::unwrap_used)]

use faderframe_engine::EngineConfig;
use faderframe_project::lyrics::LyricLine;
use faderframe_project::{Command, Project};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;

fn session() -> Session {
    let mut s = Session::new(Project::new("Words", 48_000), None, EngineConfig::default()).unwrap();
    // 120 BPM: a quarter is half a second.
    let line = |a: f64, b: f64, t: &str| LyricLine {
        start: MusicalTime::from_quarters(a),
        end: MusicalTime::from_quarters(b),
        text: t.into(),
    };
    s.dispatch(Action::Edit(Command::SetLyrics {
        lyrics: vec![line(4.0, 7.0, "Hello there"), line(0.0, 2.0, "  First  ")],
    }))
    .unwrap();
    s
}

#[test]
fn lines_are_edited_and_written_out() {
    let mut s = session();
    let texts = |s: &Session| {
        s.project()
            .lyrics
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(texts(&s), ["First", "Hello there"], "sorted and trimmed");
    assert_eq!(
        s.lyrics_text(false),
        "[00:00.00]First\n[00:02.00]Hello there\n"
    );
    assert_eq!(
        s.lyrics_text(true),
        "1\n00:00:00,000 --> 00:00:01,000\nFirst\n\n2\n00:00:02,000 --> 00:00:03,500\nHello there\n\n"
    );
    s.dispatch(Action::EditLyric {
        index: 1,
        text: Some("Hello again".into()),
    })
    .unwrap();
    assert_eq!(texts(&s), ["First", "Hello again"]);
    s.dispatch(Action::EditLyric {
        index: 0,
        text: None,
    })
    .unwrap();
    assert_eq!(texts(&s), ["Hello again"]);
    s.dispatch(Action::Undo).unwrap();
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(texts(&s), ["First", "Hello there"]);
    // Written next to the project (here, unsaved: its media folder).
    let lrc = s.export_lyrics().unwrap();
    assert_eq!(std::fs::read_to_string(&lrc).unwrap(), s.lyrics_text(false));
    assert_eq!(
        std::fs::read_to_string(lrc.with_extension("srt")).unwrap(),
        s.lyrics_text(true)
    );
}

#[test]
fn transcribing_without_the_model_says_how_to_get_it() {
    if faderframe_session::speech::model_ready() {
        return;
    }
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let clip = *s.project().clips.keys().next().unwrap();
    let err = s
        .dispatch(Action::Transcribe(clip))
        .unwrap_err()
        .to_string();
    assert!(err.contains("Download Speech Model"), "{err}");
}
