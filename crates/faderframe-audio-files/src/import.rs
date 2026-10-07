//! Importing audio files into a project's media folder.
//!
//! Every imported file is decoded once, converted to the project sample
//! rate if necessary, and written as 32-bit float WAV next to its waveform
//! peak cache (`.ffpk`). Playback then streams from that file, so any
//! frame range is one positional read and the original file may move.

use crate::decode::{ProbeInfo, decode_file};
use crate::resample::{ResampleError, StreamResampler};
use crate::wav::WavFormat;
use crate::wavstream::WavWriter;
use crate::{PeakBuilder, PeakCache};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported audio: {0}")]
    Unsupported(String),
    #[error(transparent)]
    Resample(#[from] ResampleError),
    #[error("import cancelled")]
    Cancelled,
}

/// Progress of one import, shared with the UI thread.
#[derive(Debug, Default)]
pub struct ImportProgress {
    /// Source frames decoded so far.
    pub done: AtomicU64,
    /// Expected source frames (0 if unknown).
    pub total: AtomicU64,
}

impl ImportProgress {
    pub fn fraction(&self) -> f64 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 {
            0.0
        } else {
            (self.done.load(Ordering::Relaxed) as f64 / t as f64).min(1.0)
        }
    }
}

/// Result of a successful import.
#[derive(Clone, Debug)]
pub struct ImportedAudio {
    pub original: PathBuf,
    pub name: String,
    /// The converted media file (float32 WAV at `sample_rate`).
    pub path: PathBuf,
    pub peaks_path: PathBuf,
    pub channels: usize,
    pub frames: u64,
    pub sample_rate: u32,
    pub original_rate: u32,
    pub peaks: PeakCache,
}

/// `<file>.wav` → `<file>.ffpk`.
pub fn peaks_path_for(media: &Path) -> PathBuf {
    media.with_extension("ffpk")
}

/// `stem` as a file name: characters outside letters, digits and
/// ` -_.()` become `_` ("audio" when nothing is left).
pub fn clean_stem(stem: &str) -> String {
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim().trim_matches('.');
    if clean.is_empty() {
        "audio".into()
    } else {
        clean.into()
    }
}

/// A file name in `dir` based on `stem` that does not exist yet.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let clean = clean_stem(stem);
    let mut candidate = dir.join(format!("{clean}.{ext}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{clean} {n}.{ext}"));
        n += 1;
    }
    candidate
}

/// Decode `path`, convert to `target_rate`, write into `dest_dir`.
pub fn import_file(
    path: &Path,
    dest_dir: &Path,
    target_rate: u32,
    progress: &ImportProgress,
    cancel: &AtomicBool,
) -> Result<ImportedAudio, ImportError> {
    std::fs::create_dir_all(dest_dir)?;
    let name = path
        .file_stem()
        .map_or_else(|| "audio".to_string(), |s| s.to_string_lossy().to_string());
    let out_path = unique_path(dest_dir, &name, "wav");

    struct Pipeline {
        writer: WavWriter,
        peaks: PeakBuilder,
        resampler: Option<StreamResampler>,
        original_rate: u32,
    }

    impl Pipeline {
        fn write(&mut self, s: &[&[f32]], frames: usize) -> Result<(), ImportError> {
            self.writer.write_planar(s, frames)?;
            self.peaks.push(s, frames);
            Ok(())
        }
    }

    let pipeline: std::cell::RefCell<Option<Pipeline>> = std::cell::RefCell::new(None);
    let result = decode_file(
        path,
        cancel,
        |info: &ProbeInfo| {
            progress
                .total
                .store(info.frames.unwrap_or(0), Ordering::Relaxed);
            let resampler = if info.sample_rate != target_rate {
                Some(StreamResampler::new(
                    info.sample_rate,
                    target_rate,
                    info.channels,
                )?)
            } else {
                None
            };
            *pipeline.borrow_mut() = Some(Pipeline {
                writer: WavWriter::create(
                    &out_path,
                    info.channels as u16,
                    target_rate,
                    WavFormat::Float32,
                    false,
                )?,
                peaks: PeakBuilder::new(info.channels),
                resampler,
                original_rate: info.sample_rate,
            });
            Ok(())
        },
        |planes, n| {
            progress.done.fetch_add(n as u64, Ordering::Relaxed);
            let mut guard = pipeline.borrow_mut();
            let Some(p) = guard.as_mut() else {
                return Ok(());
            };
            match p.resampler.take() {
                Some(mut r) => {
                    let res = r.push(planes, n, &mut |s, f| p.write(s, f));
                    p.resampler = Some(r);
                    res
                }
                None => {
                    let s: Vec<&[f32]> = planes.iter().map(|c| &c[..n.min(c.len())]).collect();
                    p.write(&s, n)
                }
            }
        },
    );

    let cleanup = |e: ImportError| {
        let _ = std::fs::remove_file(&out_path);
        e
    };
    let info = result.map_err(cleanup)?;
    let Some(mut p) = pipeline.into_inner() else {
        return Err(cleanup(ImportError::Unsupported("no audio decoded".into())));
    };
    if let Some(r) = p.resampler.take() {
        r.finish(&mut |s, f| p.write(s, f)).map_err(cleanup)?;
    }
    let original_rate = p.original_rate;
    let w = p.writer;
    let frames = w.frames();
    let path_written = w.finish().map_err(|e| cleanup(e.into()))?;
    let peaks = p.peaks.finish();
    let peaks_path = peaks_path_for(&path_written);
    peaks.save(&peaks_path).map_err(|e| cleanup(e.into()))?;
    progress
        .done
        .store(progress.total.load(Ordering::Relaxed), Ordering::Relaxed);
    Ok(ImportedAudio {
        original: path.to_path_buf(),
        name,
        path: path_written,
        peaks_path,
        channels: info.channels,
        frames,
        sample_rate: target_rate,
        original_rate,
        peaks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wavstream::WavFile;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ff-import-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn imports_24bit_44k1_stereo_into_48k_float() {
        let dir = tmp("a");
        let src = dir.join("Guitar Take.wav");
        let n = 44_100usize;
        let l: Vec<f32> = (0..n).map(|i| (i as f32 * 0.03).sin() * 0.6).collect();
        let r: Vec<f32> = l.iter().map(|v| -v).collect();
        crate::write_wav(&src, &[l, r], 44_100, WavFormat::Pcm24, false).unwrap();
        let media = dir.join("Audio");
        let progress = ImportProgress::default();
        let out = import_file(&src, &media, 48_000, &progress, &AtomicBool::new(false)).unwrap();
        assert_eq!(out.name, "Guitar Take");
        assert_eq!(out.channels, 2);
        assert_eq!(out.original_rate, 44_100);
        assert_eq!(out.frames, 48_000);
        assert!((progress.fraction() - 1.0).abs() < 1e-9);
        let f = WavFile::open(&out.path).unwrap();
        assert_eq!(
            (f.sample_rate(), f.frames(), f.channels()),
            (48_000, 48_000, 2)
        );
        assert_eq!(out.peaks.frames(), 48_000);
        assert_eq!(
            PeakCache::load(&out.peaks_path, 48_000, 2).unwrap(),
            out.peaks
        );
        // Importing the same name again does not overwrite.
        let again = import_file(&src, &media, 44_100, &progress, &AtomicBool::new(false)).unwrap();
        assert_ne!(again.path, out.path);
        assert_eq!(again.frames, 44_100, "no conversion at the native rate");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unsupported_and_cancelled_imports_leave_no_files() {
        let dir = tmp("b");
        let junk = dir.join("notes.wav");
        std::fs::write(&junk, b"this is not audio").unwrap();
        let media = dir.join("Audio");
        let err = import_file(
            &junk,
            &media,
            48_000,
            &ImportProgress::default(),
            &AtomicBool::new(false),
        );
        assert!(matches!(err, Err(ImportError::Unsupported(_))), "{err:?}");
        let src = dir.join("tone.wav");
        crate::write_wav(&src, &[vec![0.1; 10_000]], 48_000, WavFormat::Pcm16, false).unwrap();
        let err = import_file(
            &src,
            &media,
            48_000,
            &ImportProgress::default(),
            &AtomicBool::new(true),
        );
        assert!(matches!(err, Err(ImportError::Cancelled)));
        let left: Vec<_> = std::fs::read_dir(&media)
            .map(|d| d.count())
            .into_iter()
            .collect();
        assert_eq!(left, vec![0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
