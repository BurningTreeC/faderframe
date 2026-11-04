//! End to end on a recording, opt-in (the model is not in the repository):
//!
//!     FADERFRAME_TEST_WHISPER=<dir with the checkpoint's files> \
//!     FADERFRAME_TEST_SPEECH=<16 kHz mono f32 raw file> \
//!     cargo test -p faderframe-speech --test jfk -- --ignored --nocapture
#![allow(clippy::unwrap_used)]

use faderframe_speech::{Options, Whisper};
use std::path::PathBuf;

#[test]
#[ignore]
fn a_speech_is_transcribed() {
    let dir = PathBuf::from(std::env::var("FADERFRAME_TEST_WHISPER").unwrap());
    let raw = std::fs::read(std::env::var("FADERFRAME_TEST_SPEECH").unwrap()).unwrap();
    let audio: Vec<f32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    let t0 = std::time::Instant::now();
    let w = Whisper::load(&dir).unwrap();
    let t1 = t0.elapsed();
    let segments = w.transcribe(&audio, &Options::default(), |_| {});
    eprintln!("loaded in {t1:?}, transcribed in {:?}", t0.elapsed() - t1);
    for s in &segments {
        eprintln!("[{:6.2} → {:6.2}] {}", s.start, s.end, s.text);
    }
    let text: String = segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    assert!(
        text.contains("ask not what your country can do for you"),
        "{text}"
    );
}

/// Word timings: each word inside its segment, in order, and "Americans"
/// heard across 1.8 s (as the reference implementation's test has it).
#[test]
#[ignore]
fn the_words_are_timed() {
    let dir = PathBuf::from(std::env::var("FADERFRAME_TEST_WHISPER").unwrap());
    let raw = std::fs::read(std::env::var("FADERFRAME_TEST_SPEECH").unwrap()).unwrap();
    let audio: Vec<f32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    let w = Whisper::load(&dir).unwrap();
    let t0 = std::time::Instant::now();
    let options = Options {
        word_timestamps: true,
        ..Options::default()
    };
    let segments = w.transcribe(&audio, &options, |_| {});
    eprintln!("transcribed with word times in {:?}", t0.elapsed());
    let mut checked = false;
    let mut last = 0.0;
    for s in &segments {
        eprintln!("[{:6.2} → {:6.2}] {}", s.start, s.end, s.text);
        let joined: String = s.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(joined.trim(), s.text, "the words make the line");
        for word in &s.words {
            eprintln!(
                "    {:6.2} {:6.2} {:4.2} {}",
                word.start, word.end, word.probability, word.text
            );
            assert!(word.start <= word.end, "{word:?}");
            assert!(word.start >= last - 1e-9, "in order: {word:?}");
            last = word.start;
            if word.text.trim().trim_matches(',') == "Americans" {
                assert!(word.start <= 1.8 && word.end >= 1.8, "{word:?}");
                checked = true;
            }
        }
    }
    assert!(checked, "“Americans” is heard");
}
