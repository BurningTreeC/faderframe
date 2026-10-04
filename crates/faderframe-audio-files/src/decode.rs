//! Audio file decoding through Symphonia (WAV, AIFF, CAF, FLAC, MP3, Ogg
//! Vorbis, MP4/AAC, ALAC, ADPCM).

use crate::import::ImportError;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// What the container says about the audio track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeInfo {
    pub channels: usize,
    pub sample_rate: u32,
    /// Frame count if the container knows it (used for progress only).
    pub frames: Option<u64>,
}

/// File extensions accepted by the importer (lower case).
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "wav", "wave", "aif", "aiff", "aifc", "caf", "flac", "mp3", "ogg", "oga", "m4a", "mp4", "aac",
    "alac",
];

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Decode `path` completely, handing non-interleaved f32 blocks to `sink`.
///
/// `on_info` is called once, before the first block, with the stream
/// parameters. Decoding stops early (with [`ImportError::Cancelled`]) when
/// `cancel` is set.
pub fn decode_file(
    path: &Path,
    cancel: &AtomicBool,
    mut on_info: impl FnMut(&ProbeInfo) -> Result<(), ImportError>,
    mut sink: impl FnMut(&[Vec<f32>], usize) -> Result<(), ImportError>,
) -> Result<ProbeInfo, ImportError> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| ImportError::Unsupported(format!("{}: {e}", path.display())))?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| {
        ImportError::Unsupported(format!("{} has no audio track", path.display()))
    })?;
    let track_id = track.id;
    let frames = track.num_frames;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .cloned()
        .ok_or_else(|| {
            ImportError::Unsupported(format!("{}: unknown codec parameters", path.display()))
        })?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| ImportError::Unsupported(format!("{}: {e}", path.display())))?;

    let mut info = ProbeInfo {
        channels: params.channels.as_ref().map_or(0, |c| c.count()),
        sample_rate: params.sample_rate.unwrap_or(0),
        frames,
    };
    let mut announced = false;
    let mut planes: Vec<Vec<f32>> = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(ImportError::Cancelled);
        }
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) | Err(SymError::ResetRequired) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(ImportError::Unsupported(format!("{}: {e}", path.display()))),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Corrupt packets are skipped, as players do.
            Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
            Err(e) => return Err(ImportError::Unsupported(format!("{}: {e}", path.display()))),
        };
        if !announced {
            info.sample_rate = decoded.spec().rate();
            info.channels = decoded.spec().channels().count().max(1);
            if info.sample_rate == 0 {
                return Err(ImportError::Unsupported(format!(
                    "{}: unknown sample rate",
                    path.display()
                )));
            }
            on_info(&info)?;
            announced = true;
        }
        let n = decoded.frames();
        if n == 0 {
            continue;
        }
        decoded.copy_to_vecs_planar::<f32>(&mut planes);
        sink(&planes, n)?;
    }
    if !announced {
        return Err(ImportError::Unsupported(format!(
            "{} contains no decodable audio",
            path.display()
        )));
    }
    Ok(info)
}

/// Decode `path` completely into memory (planar) at `rate`, resampling
/// when the file has another rate.
pub fn decode_at_rate(
    path: &Path,
    rate: u32,
    cancel: &AtomicBool,
) -> Result<Vec<Vec<f32>>, ImportError> {
    use crate::resample::StreamResampler;
    use std::cell::RefCell;
    let out: RefCell<Vec<Vec<f32>>> = RefCell::new(Vec::new());
    let resampler: RefCell<Option<StreamResampler>> = RefCell::new(None);
    let append = |planes: &[&[f32]], n: usize| -> Result<(), ImportError> {
        for (o, p) in out.borrow_mut().iter_mut().zip(planes) {
            o.extend_from_slice(&p[..n.min(p.len())]);
        }
        Ok(())
    };
    decode_file(
        path,
        cancel,
        |info| {
            *out.borrow_mut() = vec![Vec::new(); info.channels.max(1)];
            if info.sample_rate != rate {
                *resampler.borrow_mut() =
                    Some(StreamResampler::new(info.sample_rate, rate, info.channels)?);
            }
            Ok(())
        },
        |planes, frames| match resampler.borrow_mut().as_mut() {
            Some(r) => r.push(planes, frames, &mut |p: &[&[f32]], n| append(p, n)),
            None => {
                let slices: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
                append(&slices, frames)
            }
        },
    )?;
    if let Some(r) = resampler.into_inner() {
        r.finish(&mut |p: &[&[f32]], n| append(p, n))?;
    }
    Ok(out.into_inner())
}
