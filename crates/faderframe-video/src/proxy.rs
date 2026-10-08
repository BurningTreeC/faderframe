//! Proxies: an all-intra copy of a picture at display size (MJPEG in
//! Matroska), where any frame is a millisecond away; the original stays
//! for full-size viewing and export. Frames keep their times.

use crate::streams::{Kind, Pipeline, make};
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
    if colour.needs_mapping() {
        let r = mapped_proxy(src, &partial, (w, h), spec, colour, cancel, progress);
        return finish(r, &partial, dst);
    }
    let p = Pipeline::new(src)?;
    let convert = make("videoconvertscale")?;
    // JPEG's own colours (full range, the BT.601 matrix): a picture's
    // video-range YUV written as it is would come back washed out. Through
    // RGB, since a YUV-to-YUV conversion keeps the range as it is.
    let capsfilter = |caps: gst::Caps| {
        gst::ElementFactory::make("capsfilter")
            .property("caps", &caps)
            .build()
            .map_err(|_| VideoError::Missing("capsfilter".into()))
    };
    let rgb = capsfilter(
        gst::Caps::builder("video/x-raw")
            .field("format", "RGBx")
            .field("width", w as i32)
            .field("height", h as i32)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build(),
    )?;
    let to_jpeg = make("videoconvert")?;
    let filter = capsfilter(
        gst::Caps::builder("video/x-raw")
            .field("format", "I420")
            .field("colorimetry", "1:4:0:0")
            .build(),
    )?;
    let encode = gst::ElementFactory::make("jpegenc")
        .property("quality", spec.quality.clamp(1, 100) as i32)
        .build()
        .map_err(|_| VideoError::Missing("jpegenc".into()))?;
    let mux = make("matroskamux")?;
    let sink = gst::ElementFactory::make("filesink")
        .property("location", partial.to_string_lossy().as_ref())
        .build()
        .map_err(|_| VideoError::Missing("filesink".into()))?;
    let elements = [convert, rgb, to_jpeg, filter, encode, mux, sink];
    p.pipeline.add_many(&elements)?;
    gst::Element::link_many(&elements)?;
    let next = elements[0]
        .static_pad("sink")
        .ok_or_else(|| VideoError::Gst("videoconvertscale without a sink".into()))?;
    p.parse(move |p, kind, n, pad| {
        (kind == Kind::Picture && n == 0)
            .then(|| p.decoder_into(pad, next.clone()).ok())
            .flatten()
    })?;
    let result = p.run(cancel, progress);
    drop(p);
    finish(result, &partial, dst)
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

/// The proxy of an HDR or wide-gamut picture: decoded and mapped for the
/// screen here (as it is shown), then written as any proxy.
fn mapped_proxy(
    src: &Path,
    out: &Path,
    (w, h): (u32, u32),
    spec: ProxySpec,
    colour: crate::colour::Colour,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f64),
) -> Result<()> {
    use std::sync::atomic::Ordering;
    let duration = crate::probe::probe(src)?.duration_ns.max(1);
    let mut dec = crate::Decoder::open_colour(src, w, h, colour)?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("width", w as i32)
        .field("height", h as i32)
        .field("framerate", gst::Fraction::new(0, 1))
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
        let push = |f: crate::Frame, end: i64| -> Result<()> {
            let mut buf = gst::Buffer::from_mut_slice(f.rgba);
            if let Some(b) = buf.get_mut() {
                b.set_pts(crate::clock(f.time.max(0)));
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
                let end = f.time;
                push(prev, end)?;
            }
            last = Some(f);
        }
        if let Some(prev) = last {
            // The last frame lasts to the end (or a frame's length).
            let end = (prev.time + 40_000_000).min(duration.max(prev.time + 1));
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
