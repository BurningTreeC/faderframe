//! Ambisonic audio files: imported as fields, written as AmbiX.
//!
//! AmbiX files are plain multichannel WAV (or any format the importer
//! reads) whose 4, 9 or 16 channels are the harmonics of a first- to
//! third-order field in ACN order with SN3D normalisation — what
//! FaderFrame carries — so an AmbiX import is an ordinary import onto a
//! new track of that order. First-order B-format in the older Furse–Malham
//! convention (`W X Y Z`, `W` at −3 dB) is converted on the import thread
//! (`faderframe_core::ambisonics::FUMA_TO_AMBIX`) before the track sees
//! it; its waveforms are made again from the converted file. Rendering an
//! ambisonic master "as the master is" writes AmbiX (no speaker mask:
//! the channels are not speakers); stereo and mono renders decode it.

use crate::{ImportTarget, NoticeLevel, Session};
use faderframe_audio_files::wavstream::{WavFile, WavWriter};
use faderframe_audio_files::{Dither, WavFormat};
use std::path::{Path, PathBuf};

/// How an ambisonic file's channels are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmbiFile {
    /// ACN order, SN3D (first to third order: 4, 9 or 16 channels).
    AmbiX,
    /// First-order Furse–Malham B-format (`W X Y Z`, `W` −3 dB).
    FuMa,
}

impl AmbiFile {
    pub fn label(self) -> &'static str {
        match self {
            AmbiFile::AmbiX => "AmbiX",
            AmbiFile::FuMa => "FuMa B-format",
        }
    }
}

/// Rewrite a first-order FuMa file (float32, as the importer writes) as
/// AmbiX, in place.
pub(crate) fn fuma_to_ambix(path: &Path) -> std::io::Result<()> {
    let src = WavFile::open(path)?;
    if src.channels() != 4 {
        return Err(std::io::Error::other(format!(
            "FuMa B-format is read as first order (4 channels), not {}",
            src.channels()
        )));
    }
    let tmp = path.with_extension("ambix.tmp");
    let mut out =
        WavWriter::create_with(&tmp, 4, src.sample_rate(), WavFormat::Float32, Dither::Off)?;
    let chunk = 1 << 15;
    let mut fuma = vec![vec![0.0f32; chunk]; 4];
    let mut ambix = vec![vec![0.0f32; chunk]; 4];
    let mut scratch = Vec::new();
    let mut pos = 0u64;
    let m = faderframe_core::ambisonics::FUMA_TO_AMBIX;
    while pos < src.frames() {
        let n = chunk.min((src.frames() - pos) as usize);
        {
            let mut slices: Vec<&mut [f32]> = fuma.iter_mut().map(|c| &mut c[..n]).collect();
            src.read(pos, &mut slices, &mut scratch)?;
        }
        for (r, row) in m.iter().enumerate() {
            for i in 0..n {
                ambix[r][i] = row.iter().zip(&fuma).map(|(g, c)| g * c[i]).sum();
            }
        }
        let views: Vec<&[f32]> = ambix.iter().map(|c| &c[..n]).collect();
        out.write_planar(&views, n)?;
        pos += n as u64;
    }
    out.finish()?;
    drop(src);
    std::fs::rename(&tmp, path)
}

impl Session {
    /// Import ambisonic files in the background, each onto a new track of
    /// its order at `target.at` (`kind`: how their channels are laid out).
    pub fn import_ambisonic(&mut self, files: Vec<PathBuf>, target: ImportTarget, kind: AmbiFile) {
        let files: Vec<PathBuf> = files.into_iter().filter(|f| f.is_file()).collect();
        if files.is_empty() {
            self.notify(NoticeLevel::Warning, "nothing to import");
            return;
        }
        let n = files.len();
        let job = crate::media::ImportJob::spawn_ambisonic(
            files,
            self.media_dir.clone(),
            self.project.sample_rate,
            ImportTarget {
                track: None,
                ..target
            },
            kind,
        );
        self.imports.push(job);
        self.notify(
            NoticeLevel::Info,
            format!(
                "importing {n} {} file{}…",
                kind.label(),
                if n == 1 { "" } else { "s" }
            ),
        );
    }
}
