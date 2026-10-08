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
    cancel: &AtomicBool,
    progress: impl FnMut(f64),
) -> Result<()> {
    let (w, h) = crate::fit(picture.0, picture.1, picture.2, u32::MAX, spec.height);
    let partial = dst.with_extension("partial");
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)?;
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
    match result {
        Ok(()) => {
            std::fs::rename(&partial, dst)?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}
