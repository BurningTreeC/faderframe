//! Movies with new sound: the picture copied as it is coded (no
//! re-encoding, so nothing is lost and it takes seconds), next to one or
//! more sound tracks (the mix, stems) from WAV files.

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

    /// Channels a sound track can have.
    pub fn max_channels(self) -> usize {
        match self {
            Self::Mov => 16,
            Self::Mkv => 64,
            Self::Mp4 => 8,
        }
    }
}

/// Write `out`: the picture of `video` (copied) and the sound of each of
/// `sounds` (WAV files, one track each, in order), as `container`.
pub fn mux(
    video: &Path,
    sounds: &[PathBuf],
    out: &Path,
    container: Container,
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

    for wav in sounds {
        if !wav.is_file() {
            return Err(VideoError::media(wav, "no such file"));
        }
        let src = gst::ElementFactory::make("filesrc")
            .property("location", wav.to_string_lossy().as_ref())
            .build()
            .map_err(|_| VideoError::Missing("filesrc".into()))?;
        let mut caps = gst::Caps::builder("audio/x-raw")
            .field("format", format)
            .field("layout", "interleaved");
        if container == Container::Mp4 {
            caps = caps.field("rate", 48_000);
        }
        let caps = caps.build();
        let filter = gst::ElementFactory::make("capsfilter")
            .property("caps", &caps)
            .build()
            .map_err(|_| VideoError::Missing("capsfilter".into()))?;
        let mut row = vec![
            src,
            make("wavparse")?,
            make("audioconvert")?,
            make("audioresample")?,
            filter,
        ];
        if container == Container::Mp4 {
            row.push(
                gst::ElementFactory::make("opusenc")
                    .property("bitrate", 256_000)
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
    // A frame without a duration (Matroska's last) would end the movie a
    // frame early in QuickTime and MPEG-4: it gets one frame's length.
    next.add_probe(gst::PadProbeType::BUFFER, |pad, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_mut()
            && buffer.duration().is_none()
        {
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
