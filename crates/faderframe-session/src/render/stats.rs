//! Statistics of rendered files (the report after Render / Export). Each
//! file is measured as written: read back from disk on the render's
//! worker once it is complete — header and audio — through
//! `faderframe_analysis::levels` (BS.1770 loudness with each speaker's
//! weight, true and sample peaks, RMS, samples at full scale, overs).
//! Nothing comes from the live signal or the tracks. What a file cannot
//! tell is left out with the reason ([`FileStats::notes`]): loudness of an
//! object-based master (only a renderer places its objects), any level of
//! a coded IAMF file (there is no decoder here — its format figures are
//! the encoder's, `from_file` false).
//!
//! The same [`FileStats`] serves every render the window starts — one
//! track, the master, stems (a row per file), ADM and IAMF masters; the
//! album and other workers can hand theirs the same way.

use super::Rendered;
use faderframe_analysis::levels::{FileLevels, FileMeter, full_scale};
use faderframe_audio_files::WavFormat;
use faderframe_audio_files::wavstream::{WavFile, read_chunk};
use faderframe_core::SurroundFormat;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Frames read back at a time.
const CHUNK: usize = 65_536;

/// What a render wrote, so it can be read back.
#[derive(Clone, Debug)]
pub(crate) enum Written {
    /// A WAVE file; `bed`: the master's format when it is a bed (its
    /// speakers, when the file's channel mask names them).
    Wav { bed: Option<SurroundFormat> },
    /// A BW64 file of an object-based master: `bed` channels, then
    /// objects.
    Adm { bed: usize, objects: usize },
    /// A coded IAMF file (nothing here decodes it).
    Iamf {
        codec: faderframe_iamf::Codec,
        layout: faderframe_iamf::Layout,
        sample_rate: u32,
        frames: u64,
    },
}

/// One rendered file.
#[derive(Clone, Debug, PartialEq)]
pub struct FileStats {
    pub path: PathBuf,
    /// Its size on disk.
    pub bytes: u64,
    /// "WAV", "BW64", "IAMF in MP4", …
    pub container: String,
    /// "24-bit PCM", "32-bit float", "Opus, 192 kb/s per stereo pair".
    pub encoding: String,
    pub sample_rate: u32,
    pub channels: usize,
    /// "Stereo", "5.1 (L R C LFE Ls Rs)", "14 channels: a 7.1.4 bed and 2
    /// objects".
    pub layout: String,
    /// The file's WAVE channel mask.
    pub channel_mask: Option<u32>,
    pub frames: u64,
    /// The figures above come from the file's header (false: the
    /// encoder's, the file was not read back).
    pub from_file: bool,
    /// Measured from the file's audio (`None`: see `notes`).
    pub levels: Option<FileLevels>,
    /// Wall time of rendering and writing this file (seconds).
    pub render_seconds: f64,
    /// What was not measured, and why.
    pub notes: Vec<String>,
}

impl FileStats {
    /// Its duration (seconds).
    pub fn seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames as f64 / f64::from(self.sample_rate)
        }
    }

    /// Seconds of audio per second of rendering.
    pub fn realtime_factor(&self) -> Option<f64> {
        (self.render_seconds > 0.0).then(|| self.seconds() / self.render_seconds)
    }

    /// What its levels warn of: clipping, overs, samples past full scale,
    /// true peaks over 0 dBTP, silence.
    pub fn warnings(&self) -> Vec<String> {
        let Some(l) = &self.levels else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let float = self.encoding.contains("float");
        if l.full_scale_samples > 0 && !float {
            out.push(format!(
                "{} sample{} at full scale ({} over{} of {}+ in a row): clipped",
                l.full_scale_samples,
                plural(l.full_scale_samples),
                l.overs,
                plural(l.overs),
                faderframe_analysis::levels::OVER_RUN
            ));
        }
        if l.above_full_scale > 0 {
            out.push(format!(
                "{} sample{} above 0 dBFS (peak {:+.1} dBFS): kept by the float file, \
                 clipped by any fixed-point conversion",
                l.above_full_scale,
                plural(l.above_full_scale),
                l.sample_peak
            ));
        } else if float && l.full_scale_samples > 0 {
            out.push(format!(
                "{} sample{} at exactly 0 dBFS",
                l.full_scale_samples,
                plural(l.full_scale_samples)
            ));
        }
        if l.true_peak > 0.0 {
            out.push(format!(
                "true peak {:+.1} dBTP: inter-sample peaks over full scale (they clip in \
                 conversion and lossy coding)",
                l.true_peak
            ));
        }
        if l.sample_peak == f64::NEG_INFINITY && l.frames > 0 {
            out.push("the file is silent".into());
        }
        out
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A render's files together.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderSummary {
    pub files: usize,
    pub bytes: u64,
    /// Audio rendered (seconds, all files).
    pub seconds: f64,
    /// Rendering and writing (seconds, all files).
    pub render_seconds: f64,
    /// Files with warnings.
    pub warned: usize,
}

impl RenderSummary {
    pub fn of(rendered: &[Rendered]) -> Self {
        let stats: Vec<&FileStats> = rendered
            .iter()
            .filter_map(|r| r.stats.as_ref()?.as_ref().ok())
            .collect();
        Self {
            files: rendered.len(),
            bytes: stats.iter().map(|s| s.bytes).sum(),
            seconds: stats.iter().map(|s| s.seconds()).sum(),
            render_seconds: stats.iter().map(|s| s.render_seconds).sum(),
            warned: stats.iter().filter(|s| !s.warnings().is_empty()).count(),
        }
    }

    /// Seconds of audio per second of rendering.
    pub fn realtime_factor(&self) -> Option<f64> {
        (self.render_seconds > 0.0).then(|| self.seconds / self.render_seconds)
    }
}

/// The speakers a channel mask names: the bed the render made when it
/// matches, else the built-in format with that mask.
fn speakers_of(mask: u32, channels: usize, bed: Option<SurroundFormat>) -> Option<SurroundFormat> {
    bed.into_iter()
        .chain(SurroundFormat::ALL)
        .find(|f| f.channel_mask() == mask && f.channels() == channels)
}

fn encoding_of(format: WavFormat) -> &'static str {
    match format {
        WavFormat::Pcm16 => "16-bit PCM",
        WavFormat::Pcm24 => "24-bit PCM",
        WavFormat::Float32 => "32-bit float",
    }
}

/// The channel mask of a WAVE file (`WAVE_FORMAT_EXTENSIBLE`).
fn channel_mask(path: &Path) -> std::io::Result<Option<u32>> {
    let fmt = read_chunk(path, b"fmt ")?.unwrap_or_default();
    Ok(
        (fmt.len() >= 24 && u16::from_le_bytes([fmt[0], fmt[1]]) == 0xFFFE)
            .then(|| u32::from_le_bytes([fmt[20], fmt[21], fmt[22], fmt[23]])),
    )
}

/// The RIFF form of a WAVE file ("WAV", "RF64", "BW64").
fn container_of(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut id = [0u8; 4];
    std::fs::File::open(path)?.read_exact(&mut id)?;
    Ok(match &id {
        b"RIFF" => "WAV".into(),
        other => String::from_utf8_lossy(other).into_owned(),
    })
}

/// Read `file` (written for `dest`) back and measure it; `cancel` stops
/// the reading (an error).
pub(crate) fn measure(
    file: &Path,
    dest: &Path,
    written: &Written,
    render_seconds: f64,
    cancel: &AtomicBool,
) -> Result<FileStats, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", dest.display());
    let bytes = std::fs::metadata(file).map_err(fail)?.len();
    if let Written::Iamf {
        codec,
        layout,
        sample_rate,
        frames,
    } = written
    {
        let container = if file
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("iamf"))
        {
            "IAMF sequence"
        } else {
            "IAMF in MP4"
        };
        let encoding = match codec {
            faderframe_iamf::Codec::Opus { stereo_bitrate } => {
                format!("Opus, {} kb/s per stereo pair", stereo_bitrate / 1000)
            }
            faderframe_iamf::Codec::Flac { bits } => format!("FLAC, {bits}-bit"),
            faderframe_iamf::Codec::Lpcm { bits } => format!("LPCM, {bits}-bit"),
        };
        return Ok(FileStats {
            path: dest.to_path_buf(),
            bytes,
            container: container.into(),
            encoding,
            sample_rate: *sample_rate,
            channels: layout.channels().len(),
            layout: layout.name().into(),
            channel_mask: None,
            frames: *frames,
            from_file: false,
            levels: None,
            render_seconds,
            notes: vec![
                "Levels not measured: the coded IAMF file is not decoded here. Rate, channels \
                 and duration are the encoder's; the loudness written into the file was \
                 measured before coding."
                    .into(),
            ],
        });
    }
    let wav = WavFile::open(file).map_err(fail)?;
    let channels = wav.channels();
    let format = wav.format();
    let mask = channel_mask(file).map_err(fail)?;
    let mut notes = Vec::new();
    let (layout, weights): (String, Option<Vec<f64>>) = match (written, mask) {
        (Written::Adm { bed, objects }, _) => {
            notes.push(
                "Loudness not measured: an object-based master sounds as a renderer places \
                 its objects; peaks and RMS are each channel's."
                    .into(),
            );
            (
                format!(
                    "{channels} channels: {bed} bed channel{} and {objects} object{}",
                    plural(*bed as u64),
                    plural(*objects as u64)
                ),
                None,
            )
        }
        (_, None | Some(0x4)) if channels == 1 => ("Mono".into(), Some(vec![1.0])),
        (_, None | Some(0x3)) if channels == 2 => ("Stereo".into(), Some(vec![1.0, 1.0])),
        (Written::Wav { bed }, Some(mask)) => match speakers_of(mask, channels, *bed) {
            Some(f) => {
                let labels: Vec<&str> = f.speakers().iter().map(|s| s.label).collect();
                let weights = f
                    .speakers()
                    .iter()
                    .map(|s| {
                        if s.lfe {
                            0.0
                        } else {
                            faderframe_analysis::speaker_weight(
                                f64::from(s.azimuth),
                                f64::from(s.elevation),
                            )
                        }
                    })
                    .collect();
                (
                    format!("{} ({})", f.name(), labels.join(" ")),
                    Some(weights),
                )
            }
            None => {
                notes.push(
                    "Loudness not measured: the file's channel mask names no layout known here."
                        .into(),
                );
                (format!("{channels} channels (mask 0x{mask:X})"), None)
            }
        },
        _ => {
            notes.push("Loudness not measured: the file names no speakers.".into());
            (format!("{channels} channels"), None)
        }
    };
    let mut meter = FileMeter::new(
        wav.sample_rate(),
        channels,
        weights.as_deref(),
        full_scale(format.bits(), !format.is_integer()),
    );
    let mut buffers = vec![vec![0.0f32; CHUNK]; channels];
    let mut scratch = Vec::new();
    let mut at = 0u64;
    while at < wav.frames() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = (wav.frames() - at).min(CHUNK as u64) as usize;
        let mut out: Vec<&mut [f32]> = buffers.iter_mut().map(|b| &mut b[..n]).collect();
        wav.read(at, &mut out, &mut scratch).map_err(fail)?;
        let read: Vec<&[f32]> = buffers.iter().map(|b| &b[..n]).collect();
        meter.add(&read);
        at += n as u64;
    }
    let mut container = container_of(file).map_err(fail)?;
    if let Written::Adm { .. } = written {
        // Under 4 GiB the BW64 writer leaves a plain RIFF header.
        if container == "WAV" {
            container = "BWF".into();
        }
        container += " (ADM)";
    }
    Ok(FileStats {
        path: dest.to_path_buf(),
        bytes,
        container,
        encoding: encoding_of(format).into(),
        sample_rate: wav.sample_rate(),
        channels,
        layout,
        channel_mask: mask,
        frames: wav.frames(),
        from_file: true,
        levels: Some(meter.finish()),
        render_seconds,
        notes,
    })
}

/// "1.4 MB", "820 kB".
pub fn size_text(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} kB", b / 1e3)
    } else {
        format!("{bytes} bytes")
    }
}

/// "3:25.120" (minutes, seconds with milliseconds) or "12.500 s".
pub fn duration_text(seconds: f64) -> String {
    if seconds >= 60.0 {
        let m = (seconds / 60.0).floor();
        format!("{m:.0}:{:06.3}", seconds - m * 60.0)
    } else {
        format!("{seconds:.3} s")
    }
}

/// A level for the report: "−6.0 dBFS", "−inf" for silence.
pub fn db_text(db: f64, unit: &str) -> String {
    if db.is_finite() {
        format!("{db:.1} {unit}").replace('-', "−")
    } else {
        format!("−∞ {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        assert_eq!(size_text(1_400_000), "1.4 MB");
        assert_eq!(size_text(820_000), "820 kB");
        assert_eq!(duration_text(205.12), "3:25.120");
        assert_eq!(duration_text(12.5), "12.500 s");
        assert_eq!(db_text(-6.04, "dBFS"), "−6.0 dBFS");
        assert_eq!(db_text(f64::NEG_INFINITY, "LUFS"), "−∞ LUFS");
    }
}
