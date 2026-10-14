//! Proxies: an all-intra copy of a picture at display size (MJPEG in
//! Matroska), where any frame is a millisecond away; the original stays
//! for full-size viewing and export. Frames keep their times.

use crate::streams::make;
use crate::{Result, VideoError};
use gst::prelude::*;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// How proxies are made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxySpec {
    /// Their height at most (pixels; the width follows the picture).
    pub height: u32,
    /// JPEG quality, 1–100.
    pub quality: u32,
}

impl Default for ProxySpec {
    fn default() -> Self {
        Self {
            height: 540,
            quality: 80,
        }
    }
}

/// Make the proxy of `src`'s picture (`width`×`height` stored, pixel
/// aspect `par`) at `dst`; written beside and moved in place when done,
/// so a proxy file is always whole. `progress`: the share done.
pub fn make_proxy(
    src: &Path,
    dst: &Path,
    picture: (u32, u32, (u32, u32)),
    spec: ProxySpec,
    colour: crate::colour::Colour,
    cancel: &AtomicBool,
    progress: impl FnMut(f64),
) -> Result<()> {
    let (w, h) = crate::fit(picture.0, picture.1, picture.2, u32::MAX, spec.height);
    let partial = dst.with_extension("partial");
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let r = decoded_proxy(src, &partial, (w, h), spec, colour, cancel, progress);
    finish(r, &partial, dst)
}

/// Move a whole proxy in place, or drop what there is of it.
fn finish(result: Result<()>, partial: &Path, dst: &Path) -> Result<()> {
    match result {
        Ok(()) => {
            std::fs::rename(partial, dst)?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(partial);
            Err(e)
        }
    }
}

/// The proxy, decoded here by a [`crate::Decoder`] — as it is shown (HDR
/// and wide gamut mapped for the screen, JPEG at its own levels), every
/// frame at its stream time (the times the index reads), those past a
/// demuxer's own end too — and written as MJPEG in Matroska. (Matroska
/// readers start a stream at its first block: a proxy is read relative to
/// its first frame, see `service`.)
fn decoded_proxy(
    src: &Path,
    out: &Path,
    (w, h): (u32, u32),
    spec: ProxySpec,
    colour: crate::colour::Colour,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f64),
) -> Result<()> {
    use std::sync::atomic::Ordering;
    let info = crate::probe::probe(src)?;
    let duration = info.duration_ns.max(1);
    // The source's rate where it has one: Matroska keeps it, and readers
    // reckon a frame's length from it (a JPEG parser drops the blocks'
    // own); without one a frame lasts 40 ms to them, so at 29.97 a seek
    // into a frame gave the one before it.
    let (num, den) = info
        .video
        .as_ref()
        .map(|v| v.fps)
        .filter(|&(n, d)| n > 0 && d > 0)
        .unwrap_or((0, 1));
    let mut dec = crate::Decoder::open_colour(src, w, h, colour)?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("width", w as i32)
        .field("height", h as i32)
        .field("framerate", gst::Fraction::new(num as i32, den as i32))
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build();
    let appsrc = gst_app::AppSrc::builder()
        .caps(&caps)
        .format(gst::Format::Time)
        .block(true)
        .max_bytes((w * h * 4 * 8) as u64)
        .build();
    let pipeline = gst::Pipeline::new();
    let jpeg_caps = gst::Caps::builder("video/x-raw")
        .field("format", "I420")
        .field("colorimetry", "1:4:0:0")
        .build();
    let filter = gst::ElementFactory::make("capsfilter")
        .property("caps", &jpeg_caps)
        .build()
        .map_err(|_| VideoError::Missing("capsfilter".into()))?;
    let encode = gst::ElementFactory::make("jpegenc")
        .property("quality", spec.quality.clamp(1, 100) as i32)
        .build()
        .map_err(|_| VideoError::Missing("jpegenc".into()))?;
    let sink = gst::ElementFactory::make("filesink")
        .property("location", out.to_string_lossy().as_ref())
        .build()
        .map_err(|_| VideoError::Missing("filesink".into()))?;
    let elements = [
        appsrc.clone().upcast::<gst::Element>(),
        make("videoconvert")?,
        filter,
        encode,
        make("matroskamux")?,
        sink,
    ];
    pipeline.add_many(&elements)?;
    gst::Element::link_many(&elements)?;
    pipeline.set_state(gst::State::Playing)?;
    let result = (|| {
        dec.play_from(0)?;
        let mut last: Option<crate::Frame> = None;
        // How long the last frame lasts: as the one before it.
        let mut spacing = 40_000_000;
        let push = |f: crate::Frame, end: i64| -> Result<()> {
            let mut buf = gst::Buffer::from_mut_slice(f.rgba);
            if let Some(b) = buf.get_mut() {
                b.set_pts(crate::clock(f.time));
                b.set_duration(crate::clock((end - f.time).max(1)));
            }
            appsrc
                .push_buffer(buf)
                .map_err(|e| VideoError::Gst(format!("proxy frames: {e}")))?;
            Ok(())
        };
        while let Some(f) = dec.next_frame()? {
            if cancel.load(Ordering::Relaxed) {
                return Err(VideoError::Cancelled);
            }
            progress((f.time as f64 / duration as f64).clamp(0.0, 1.0));
            if let Some(prev) = last.take() {
                spacing = (f.time - prev.time).max(1);
                let end = f.time;
                push(prev, end)?;
            }
            last = Some(f);
        }
        if let Some(prev) = last {
            let end = prev.time + spacing;
            push(prev, end)?;
        }
        appsrc
            .end_of_stream()
            .map_err(|e| VideoError::Gst(format!("proxy frames: {e}")))?;
        let bus = pipeline
            .bus()
            .ok_or_else(|| VideoError::Gst("a pipeline without a bus".into()))?;
        match bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(60),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) {
            Some(m) => match m.view() {
                gst::MessageView::Error(e) => Err(VideoError::Gst(e.error().to_string())),
                _ => Ok(()),
            },
            None => Err(VideoError::Gst("the proxy did not finish".into())),
        }
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}
