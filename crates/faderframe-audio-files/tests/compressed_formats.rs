#![allow(clippy::unwrap_used)]
//! Imports real compressed/container formats produced by `ffmpeg`.
//! Skipped (with a message) when `ffmpeg` is not installed.

use faderframe_audio_files::import::{ImportProgress, import_file};
use faderframe_audio_files::wavstream::WavFile;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn encode(out: &Path, extra: &[&str]) -> bool {
    let mut args = vec![
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=44100:duration=2",
        "-ac",
        "2",
    ];
    args.extend_from_slice(extra);
    let out_s = out.to_string_lossy().to_string();
    args.push(&out_s);
    Command::new("ffmpeg")
        .args(&args)
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn imports_flac_mp3_ogg_aac_aiff() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not installed; skipping compressed-format import test");
        return;
    }
    let dir = std::env::temp_dir().join(format!("ff-formats-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let media = dir.join("Audio");
    let cases: [(&str, &[&str]); 5] = [
        ("tone.flac", &[]),
        ("tone.mp3", &["-b:a", "192k"]),
        ("tone.ogg", &["-c:a", "libvorbis"]),
        ("tone.m4a", &["-c:a", "aac", "-b:a", "160k"]),
        ("tone.aiff", &[]),
    ];
    let mut checked = 0;
    for (name, extra) in cases {
        let src = dir.join(name);
        if !encode(&src, extra) {
            eprintln!("ffmpeg cannot encode {name}; skipping it");
            continue;
        }
        let out = import_file(
            &src,
            &media,
            48_000,
            &ImportProgress::default(),
            &AtomicBool::new(false),
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(out.channels, 2, "{name}");
        assert_eq!(out.original_rate, 44_100, "{name}");
        // Lossy codecs add encoder delay/padding; allow 0.1 s of slack.
        let expected = 96_000i64;
        assert!(
            (out.frames as i64 - expected).abs() < 4_800,
            "{name}: {} frames",
            out.frames
        );
        let f = WavFile::open(&out.path).unwrap();
        let mut l = vec![0.0f32; 4_800];
        f.read(48_000, &mut [&mut l], &mut Vec::new()).unwrap();
        let peak = l.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05, "{name}: decoded audio is silent");
        let crossings = l.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!(
            (crossings as i64 - 44).abs() <= 2,
            "{name}: 440 Hz expected, {crossings} crossings in 0.1 s"
        );
        checked += 1;
    }
    assert!(
        checked >= 3,
        "at least FLAC, MP3 and AIFF should be testable"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
