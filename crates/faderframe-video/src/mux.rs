//! Movies with new sound: the picture copied as it is coded (no
//! re-encoding, so nothing is lost and it takes seconds), next to one or
//! more sound tracks (the mix, stems) from WAV files.
//!
//! Sounds of more than two channels go in as they are, every channel kept
//! and none given a speaker (QuickTime's PCM and Opus's discrete mapping
//! take them so; Matroska keeps the WAV's speakers up to eight channels).
//! A 360° picture and an ambisonic first sound are marked in MP4 and
//! QuickTime movies ([`crate::spherical`]); Matroska movies carry neither
//! mark.

use crate::streams::{Kind, Pipeline, make};
use crate::{Result, VideoError};
use gst::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// What the movie is written as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Container {
    /// QuickTime: 24-bit PCM, up to 16 channels a track.
    #[default]
    Mov,
    /// Matroska: 32-bit float PCM, any channels.
    Mkv,
    /// MPEG-4: Opus (MP4 has no PCM), up to 8 channels a track.
    Mp4,
}

impl Container {
    pub const ALL: [Container; 3] = [Self::Mov, Self::Mkv, Self::Mp4];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mov => "mov",
            Self::Mkv => "mkv",
            Self::Mp4 => "mp4",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mov => "QuickTime (.mov), PCM",
            Self::Mkv => "Matroska (.mkv), PCM",
            Self::Mp4 => "MPEG-4 (.mp4), Opus",
        }
    }

    /// The container of a file name's extension.
    pub fn for_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Self::ALL.into_iter().find(|c| c.extension() == ext)
    }

    /// Whether the sound goes in losslessly.
    pub fn lossless(self) -> bool {
        self != Self::Mp4
    }

    /// Whether it carries 360° and spatial audio metadata.
    pub fn carries_spherical(self) -> bool {
        self != Self::Mkv
    }

    /// Channels a sound track can have.
    pub fn max_channels(self) -> usize {
        match self {
            Self::Mov => 16,
            Self::Mkv => 64,
            Self::Mp4 => 8,
        }
    }
}

/// What of the picture goes in, its timecode, and how picture and sound
/// are marked.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MuxOptions {
    /// The picture from the keyframe starting at `from` (ns, the file's
    /// timeline) up to the keyframe at `to` (not included; `None`: the
    /// end). Copying cuts only at keyframes; the movie starts at 0.
    pub from: i64,
    pub to: Option<i64>,
    /// The label at the movie's start, written as a timecode track
    /// (QuickTime).
    pub timecode: Option<(
        faderframe_core::timecode::Timecode,
        faderframe_core::timecode::FrameRate,
    )>,
    /// The picture is 360°, laid out so (MP4 and QuickTime).
    pub spherical: Option<faderframe_core::spherical::Spherical>,
    /// The first sound is an AmbiX field of this order (MP4 and
    /// QuickTime; its channels as many as the order has).
    pub ambisonic: Option<faderframe_core::AmbiOrder>,
}

/// Write `out`: the picture of `video` (copied, `options` say which of
/// it) and the sound of each of `sounds` (WAV files from the movie's
/// start, one track each, in order), as `container`.
pub fn mux(
    video: &Path,
    sounds: &[PathBuf],
    out: &Path,
    container: Container,
    options: MuxOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(f64),
) -> Result<()> {
    let info = crate::probe::probe(video)?;
    if info.video.is_none() {
        return Err(VideoError::NoPicture(video.to_path_buf()));
    }
    let p = Pipeline::new(video)?;
    let (factory, format) = match container {
        Container::Mov => ("qtmux", "S24LE"),
        Container::Mkv => ("matroskamux", "F32LE"),
        // Opus takes 16-bit at its own rates.
        Container::Mp4 => ("mp4mux", "S16LE"),
    };
    let mux = make(factory)?;
    // The container must carry the picture as it is coded.
    if let Some(coded) = picture_caps(video)?
        && !mux
            .pad_template("video_%u")
            .is_some_and(|t| carries(t.caps(), &coded))
    {
        let codec = info
            .video
            .as_ref()
            .map_or("this picture", |v| v.codec.as_str());
        return Err(VideoError::media(
            video,
            format!(
                "{} cannot carry {codec} as it is; use another container",
                container.label()
            ),
        ));
    }
    if container != Container::Mkv {
        // The index first: the movie plays while it downloads.
        mux.set_property("faststart", true);
    }
    let partial = out.with_extension(format!("{}.partial", container.extension()));
    let sink = gst::ElementFactory::make("filesink")
        .property("location", partial.to_string_lossy().as_ref())
        .build()
        .map_err(|_| VideoError::Missing("filesink".into()))?;
    p.pipeline.add_many([&mux, &sink])?;
    mux.link(&sink)?;

    for (i, wav) in sounds.iter().enumerate() {
        if !wav.is_file() {
            return Err(VideoError::media(wav, "no such file"));
        }
        let wav_info = faderframe_audio_files::wavstream::WavFile::open(wav)
            .map_err(|e| VideoError::media(wav, e.to_string()))?;
        let channels = wav_info.channels();
        let field = i == 0 && options.ambisonic.is_some();
        if channels > container.max_channels() {
            return Err(VideoError::media(
                wav,
                format!(
                    "{} takes up to {} channels a track, the sound has {channels}",
                    container.label(),
                    container.max_channels()
                ),
            ));
        }
        // Every channel as it is: no speakers for QuickTime (its PCM takes
        // no more than two placed channels), a field, or more channels than
        // GStreamer has default places for.
        let unplaced = field || channels > 8 || (channels > 2 && container == Container::Mov);
        let mut caps = gst::Caps::builder("audio/x-raw")
            .field("format", format)
            .field("layout", "interleaved");
        if container == Container::Mp4 {
            caps = caps.field("rate", 48_000);
        }
        if unplaced {
            caps = caps
                .field("channels", channels as i32)
                .field("channel-mask", gst::Bitmask::new(0));
        }
        let caps = caps.build();
        let filter = gst::ElementFactory::make("capsfilter")
            .property("caps", &caps)
            .build()
            .map_err(|_| VideoError::Missing("capsfilter".into()))?;
        let mut row = if unplaced {
            vec![unplaced_source(wav, &wav_info)?]
        } else {
            let src = gst::ElementFactory::make("filesrc")
                .property("location", wav.to_string_lossy().as_ref())
                .build()
                .map_err(|_| VideoError::Missing("filesrc".into()))?;
            vec![src, make("wavparse")?]
        };
        row.extend([make("audioconvert")?, make("audioresample")?, filter]);
        if container == Container::Mp4 {
            row.push(
                gst::ElementFactory::make("opusenc")
                    // 256 kb/s a pair, as far as Opus goes.
                    .property(
                        "bitrate",
                        (128_000 * (channels as i32 + 1) / 2 * 2).clamp(256_000, 650_000),
                    )
                    .build()
                    .map_err(|_| VideoError::Missing("opusenc".into()))?,
            );
        }
        row.push(make("queue")?);
        p.pipeline.add_many(&row)?;
        gst::Element::link_many(&row)?;
        let pad = mux
            .request_pad_simple("audio_%u")
            .ok_or_else(|| VideoError::Gst(format!("{factory} takes no more sound")))?;
        row.last()
            .and_then(|q| q.static_pad("src"))
            .ok_or_else(|| VideoError::Gst("a queue without a source".into()))?
            .link(&pad)
            .map_err(|e| VideoError::Gst(format!("sound into {factory}: {e:?}")))?;
    }

    let queue = make("queue")?;
    p.pipeline.add(&queue)?;
    let vpad = mux
        .request_pad_simple("video_%u")
        .ok_or_else(|| VideoError::Gst(format!("{factory} takes no picture")))?;
    queue
        .static_pad("src")
        .ok_or_else(|| VideoError::Gst("a queue without a source".into()))?
        .link(&vpad)
        .map_err(|e| VideoError::Gst(format!("picture into {factory}: {e:?}")))?;
    let next = queue
        .static_pad("sink")
        .ok_or_else(|| VideoError::Gst("a queue without a sink".into()))?;
    // The span (whole GOPs, in decoding order): from the keyframe at
    // `from` until the keyframe at `to`; the movie starts at 0.
    let tc = options.timecode.and_then(|(t, rate)| {
        let (n, d) = rate.ratio();
        let flags = if rate.is_drop() {
            gst_video::VideoTimeCodeFlags::DROP_FRAME
        } else {
            gst_video::VideoTimeCodeFlags::empty()
        };
        gst_video::ValidVideoTimeCode::new(
            gst::Fraction::new(n as i32, d as i32),
            None,
            flags,
            u32::from(t.hours),
            u32::from(t.minutes),
            u32::from(t.seconds),
            u32::from(t.frames),
            0,
        )
        .ok()
    });
    let span = std::sync::Mutex::new((false, false, tc));
    let (from, to) = (options.from, options.to);
    queue
        .static_pad("src")
        .ok_or_else(|| VideoError::Gst("a queue without a source".into()))?
        .set_offset(-from);
    next.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_mut() else {
            return gst::PadProbeReturn::Ok;
        };
        let key = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
        let time = buffer.pts().map(|pts| {
            pad.sticky_event::<gst::event::Segment>(0)
                .and_then(|e| {
                    e.segment()
                        .downcast_ref::<gst::ClockTime>()
                        .and_then(|s| s.to_stream_time(pts))
                })
                .map_or(crate::ns(pts), crate::ns)
        });
        let Ok(mut state) = span.lock() else {
            return gst::PadProbeReturn::Ok;
        };
        let (started, stopped, tc) = &mut *state;
        if !*started {
            if key && time.is_some_and(|t| t >= from - 1_000_000) {
                *started = true;
            } else {
                return gst::PadProbeReturn::Drop;
            }
        }
        if let Some(to) = to
            && key
            && time.is_some_and(|t| t >= to - 1_000_000)
        {
            *stopped = true;
        }
        if *stopped {
            return gst::PadProbeReturn::Drop;
        }
        if let Some(tc) = tc.take() {
            gst_video::VideoTimeCodeMeta::add(buffer.make_mut(), &tc);
        }
        // A frame without a duration (Matroska's last) would end the movie
        // a frame early in QuickTime and MPEG-4: it gets one frame's
        // length.
        if buffer.duration().is_none() {
            let len = pad
                .current_caps()
                .and_then(|c| c.structure(0)?.get::<gst::Fraction>("framerate").ok())
                .filter(|f| f.numer() > 0 && f.denom() > 0)
                .map_or(40_000_000, |f| {
                    1_000_000_000u64 * f.denom() as u64 / f.numer() as u64
                });
            buffer
                .make_mut()
                .set_duration(gst::ClockTime::from_nseconds(len));
        }
        gst::PadProbeReturn::Ok
    });
    p.parse(move |_, kind, n, _| (kind == Kind::Picture && n == 0).then(|| next.clone()))?;
    let result = p.run(cancel, progress);
    drop(p);
    let meta = crate::spherical::Metadata {
        video: options.spherical,
        audio: options
            .ambisonic
            .filter(|_| !sounds.is_empty())
            .map(crate::spherical::SpatialAudio::ambix),
    };
    let result = result.and_then(|()| {
        if container.carries_spherical() {
            crate::spherical::write(&partial, &meta)
        } else {
            Ok(())
        }
    });
    match result {
        Ok(()) => {
            std::fs::rename(&partial, out)?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

/// Samples per buffer of an unplaced sound.
const BLOCK_FRAMES: u64 = 4096;

/// A WAV file's samples as they are stored, its channels given no
/// speakers (`wavparse` would place more than two, and nothing converts
/// placed channels to unplaced ones).
fn unplaced_source(
    wav: &Path,
    info: &faderframe_audio_files::wavstream::WavFile,
) -> Result<gst::Element> {
    use faderframe_audio_files::WavFormat;
    use std::io::{Read, Seek, SeekFrom};
    let (_, data, size) = faderframe_audio_files::wavstream::chunks(wav)
        .map_err(|e| VideoError::media(wav, e.to_string()))?
        .into_iter()
        .find(|(id, _, _)| id == b"data")
        .ok_or_else(|| VideoError::media(wav, "no sound in the file"))?;
    let (format, bytes) = match info.format() {
        WavFormat::Pcm16 => ("S16LE", 2),
        WavFormat::Pcm24 => ("S24LE", 3),
        WavFormat::Float32 => ("F32LE", 4),
    };
    let channels = info.channels().max(1);
    let rate = info.sample_rate().max(1);
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", format)
        .field("layout", "interleaved")
        .field("rate", rate as i32)
        .field("channels", channels as i32)
        .field("channel-mask", gst::Bitmask::new(0))
        .build();
    let src = gst_app::AppSrc::builder()
        .caps(&caps)
        .format(gst::Format::Time)
        .build();
    let frame = (channels * bytes) as u64;
    let mut file = std::fs::File::open(wav)?;
    file.seek(SeekFrom::Start(data))?;
    let total = size / frame;
    let state = std::sync::Mutex::new((file, 0u64));
    let ns = move |frames: u64| (frames as u128 * 1_000_000_000 / rate as u128) as u64;
    src.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .need_data(move |src, _| {
                let Ok(mut s) = state.lock() else {
                    let _ = src.end_of_stream();
                    return;
                };
                let (file, done) = &mut *s;
                let n = BLOCK_FRAMES.min(total - *done);
                if n == 0 {
                    let _ = src.end_of_stream();
                    return;
                }
                let mut data = vec![0u8; (n * frame) as usize];
                if let Err(e) = file.read_exact(&mut data) {
                    tracing::warn!("reading the sound: {e}");
                    let _ = src.end_of_stream();
                    return;
                }
                let mut buf = gst::Buffer::from_mut_slice(data);
                if let Some(b) = buf.get_mut() {
                    b.set_pts(gst::ClockTime::from_nseconds(ns(*done)));
                    b.set_duration(gst::ClockTime::from_nseconds(ns(*done + n) - ns(*done)));
                }
                *done += n;
                let _ = src.push_buffer(buf);
            })
            .build(),
    );
    Ok(src.upcast())
}

/// The picture's coded caps, as the parser hands them on.
fn picture_caps(path: &Path) -> Result<Option<gst::Caps>> {
    let disc = gst_pbutils::Discoverer::new(gst::ClockTime::from_seconds(15))?;
    let uri = gst::glib::filename_to_uri(path, None)?;
    let info = disc
        .discover_uri(&uri)
        .map_err(|e| VideoError::media(path, e.to_string()))?;
    use gst_pbutils::prelude::*;
    Ok(info
        .video_streams()
        .into_iter()
        .find(|v| !v.is_image())
        .and_then(|v| v.caps()))
}

/// Whether a muxer's picture caps take a stream of `coded` (by its media
/// type: the parser converts stream formats).
fn carries(template: &gst::Caps, coded: &gst::Caps) -> bool {
    let Some(name) = coded.structure(0).map(|s| s.name().to_string()) else {
        return false;
    };
    template.iter().any(|s| s.name() == name.as_str())
}
