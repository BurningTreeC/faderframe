//! Whole files: written with every codec in both containers, read back by
//! our reader and — when ffmpeg is installed — by FFmpeg's IAMF support
//! (`FADERFRAME_IAMF_OUT=<dir>` keeps the files).
#![allow(clippy::unwrap_used)]

use faderframe_iamf::{Codec, Container, Layout, Loudness, Master, read, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

fn signal(layout: Layout, rate: u32, seconds: f32) -> Vec<Vec<f32>> {
    let n = (rate as f32 * seconds) as usize;
    (0..layout.channels().len())
        .map(|c| {
            let f = 200.0 + 110.0 * c as f32;
            (0..n)
                .map(|i| 0.25 * (i as f32 * f * std::f32::consts::TAU / rate as f32).sin())
                .collect()
        })
        .collect()
}

fn master(layout: Layout, codec: Codec) -> Master {
    let l = |layout| Loudness {
        layout,
        integrated: -20.0,
        digital_peak: -12.0,
        true_peak: -11.9,
    };
    let mut loudness = vec![l(Layout::Stereo)];
    if layout != Layout::Stereo && layout != Layout::Mono {
        loudness.push(l(layout));
    }
    Master {
        layout,
        codec,
        sample_rate: codec.rate_for(48_000),
        pre_skip: 0,
        label: "Test".into(),
        loudness,
    }
}

fn dir() -> PathBuf {
    let d = std::env::var("FADERFRAME_IAMF_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join(format!("ff-iamf-{}", std::process::id())));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ffprobe() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// FFmpeg's decode of substream `index` (f32, interleaved).
fn ffmpeg_substream(path: &Path, index: usize, channels: usize) -> Vec<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", &format!("0:a:{index}"), "-f", "f32le", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let samples: Vec<f32> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    (0..channels)
        .map(|c| samples.iter().skip(c).step_by(channels).copied().collect())
        .collect()
}

#[test]
fn every_codec_writes_files_that_read_back() {
    let d = dir();
    for codec in [
        Codec::Lpcm { bits: 24 },
        Codec::Flac { bits: 24 },
        Codec::Opus {
            stereo_bitrate: 192_000,
        },
    ] {
        for layout in [Layout::S714, Layout::Stereo, Layout::Mono] {
            let audio = signal(layout, codec.rate_for(48_000), 1.3);
            let refs: Vec<&[f32]> = audio.iter().map(Vec::as_slice).collect();
            for container in [Container::Raw, Container::Mp4] {
                let ext = if container == Container::Raw {
                    "iamf"
                } else {
                    "mp4"
                };
                let path = d.join(format!("{}-{}.{ext}", codec.name(), layout.name()));
                let mut m = master(layout, codec);
                write_file(&path, &mut m, &refs, container, |_| {}).unwrap();
                if container == Container::Raw {
                    let seq = read::parse(&std::fs::read(&path).unwrap()).unwrap();
                    let subs = layout.substreams().len();
                    let n = codec.frame_size() as usize;
                    let frames = (audio[0].len() + usize::from(m.pre_skip)).div_ceil(n);
                    assert_eq!(seq.frames.len(), frames * subs);
                    assert_eq!(seq.delimiters, frames);
                    let first = &seq.frames[0];
                    let last = seq.frames.last().unwrap();
                    assert_eq!(first.1, u32::from(m.pre_skip));
                    assert_eq!(
                        frames * n - last.2 as usize - usize::from(m.pre_skip),
                        audio[0].len()
                    );
                    if matches!(codec, Codec::Lpcm { .. }) && layout.substreams()[0].len() == 2 {
                        // The first substream's first sample of each channel.
                        let s = |b: &[u8]| {
                            (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0
                        };
                        assert!((s(&first.3[3..6]) - audio[1][0]).abs() < 1e-6);
                        assert!((s(&first.3[9..12]) - audio[1][1]).abs() < 1e-6);
                    }
                }
                if !ffprobe() {
                    continue;
                }
                // FFmpeg reads the stream groups and decodes each substream.
                let probe = Command::new("ffprobe")
                    .args([
                        "-v",
                        "error",
                        "-show_stream_groups",
                        "-show_streams",
                        "-of",
                        "json",
                    ])
                    .arg(&path)
                    .output()
                    .unwrap();
                assert!(
                    probe.status.success(),
                    "{}: {}",
                    path.display(),
                    String::from_utf8_lossy(&probe.stderr)
                );
                let json = String::from_utf8_lossy(&probe.stdout);
                assert!(
                    json.contains("IAMF Audio Element"),
                    "{}: {json}",
                    path.display()
                );
                assert!(json.contains("IAMF Mix Presentation"), "{}", path.display());
                let decoded = ffmpeg_substream(&path, 0, layout.substreams()[0].len());
                let want = &audio[0];
                let got = &decoded[0];
                assert!(
                    got.len().abs_diff(want.len()) <= codec.frame_size() as usize,
                    "{} {} {}: {} samples, wanted {}",
                    codec.name(),
                    layout.name(),
                    ext,
                    got.len(),
                    want.len()
                );
                // Compare where both have audio (Opus: lossy, by SNR).
                let k = got.len().min(want.len());
                let err: f64 = (0..k).map(|i| f64::from(got[i] - want[i]).powi(2)).sum();
                let sig: f64 = (0..k).map(|i| f64::from(want[i]).powi(2)).sum();
                let snr = 10.0 * (sig / err.max(1e-30)).log10();
                eprintln!(
                    "{} {} {ext}: {} samples, SNR {snr:.1} dB",
                    codec.name(),
                    layout.name(),
                    got.len()
                );
                let floor = if let Codec::Opus { .. } = codec {
                    15.0
                } else {
                    100.0
                };
                assert!(
                    snr > floor,
                    "{} {} {ext}: SNR {snr:.1} dB",
                    codec.name(),
                    layout.name()
                );
            }
        }
    }
}
