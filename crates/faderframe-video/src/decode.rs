//! Frames as RGBA at a requested size: one exactly (a seek decodes from the
//! keyframe before it), a keyframe at once (scrubbing), or in order from a
//! start (playback, never seeking).

use crate::streams::{Kind, Pipeline, make};
use crate::{Result, VideoError};
use gst::prelude::*;
use gst_video::prelude::*;
use std::path::Path;
use std::time::Duration;

/// A decoded frame: rows of RGBA (opaque), `width * 4` bytes each.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Its start in the file's timeline (ns, stream time).
    pub time: i64,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Frame({}x{} @ {} ns)",
            self.width, self.height, self.time
        )
    }
}

impl Frame {
    /// The pixel at (`x`, `y`) as RGBA.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        let mut p = [0; 4];
        p.copy_from_slice(&self.rgba[i..i + 4]);
        p
    }
}

/// The size a picture of `width`×`height` (pixel aspect `par`) is shown at
/// within `max_w`×`max_h`, aspect kept, never larger than its own; even.
pub fn fit(width: u32, height: u32, par: (u32, u32), max_w: u32, max_h: u32) -> (u32, u32) {
    let w = width as f64 * par.0.max(1) as f64 / par.1.max(1) as f64;
    let h = height as f64;
    let scale = (max_w as f64 / w).min(max_h as f64 / h).min(1.0);
    let even = |x: f64| (((x * scale) / 2.0).round() as u32 * 2).max(2);
    (even(w), even(h))
}

/// A decoder of one file's picture at one size.
pub struct Decoder {
    p: Pipeline,
    sink: gst_app::AppSink,
    width: u32,
    height: u32,
    playing: bool,
    timeout: gst::ClockTime,
}

impl Decoder {
    /// Open `path`'s picture, scaled to `width`×`height` (see [`fit`]);
    /// shows the first frame.
    pub fn open(path: &Path, width: u32, height: u32) -> Result<Self> {
        let p = Pipeline::new(path)?;
        let convert = make("videoconvertscale")?;
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", width as i32)
            .field("height", height as i32)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        let filter = gst::ElementFactory::make("capsfilter")
            .property("caps", &caps)
            .build()
            .map_err(|_| VideoError::Missing("capsfilter".into()))?;
        let sink = gst_app::AppSink::builder()
            .sync(false)
            .max_buffers(3)
            .drop(false)
            .enable_last_sample(false)
            .build();
        let elements = [convert, filter, sink.clone().upcast()];
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
        p.pipeline.set_state(gst::State::Paused)?;
        p.settle(Duration::from_secs(20))?;
        Ok(Self {
            p,
            sink,
            width,
            height,
            playing: false,
            timeout: gst::ClockTime::from_seconds(10),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn seek(&mut self, t: i64, exact: bool) -> Result<()> {
        let flags = if exact {
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
        } else {
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_BEFORE
        };
        if self.playing {
            self.p.pipeline.set_state(gst::State::Paused)?;
            self.playing = false;
        }
        self.p
            .pipeline
            .seek_simple(flags, crate::clock(t))
            .map_err(|_| VideoError::media(&self.p.path, "cannot seek"))?;
        self.p.settle(Duration::from_secs(20))
    }

    fn frame_of(&self, sample: &gst::Sample) -> Result<Frame> {
        let caps = sample
            .caps()
            .ok_or_else(|| VideoError::Gst("a frame without caps".into()))?;
        let info = gst_video::VideoInfo::from_caps(caps)?;
        let buffer = sample
            .buffer()
            .ok_or_else(|| VideoError::Gst("a sample without a frame".into()))?;
        let time = match (sample.segment(), buffer.pts()) {
            (Some(seg), Some(pts)) => seg
                .downcast_ref::<gst::ClockTime>()
                .and_then(|s| s.to_stream_time(pts))
                .map_or(0, crate::ns),
            _ => 0,
        };
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
            .map_err(|_| VideoError::Gst("an unreadable frame".into()))?;
        let (w, h) = (info.width(), info.height());
        let stride = frame.plane_stride()[0] as usize;
        let data = frame
            .plane_data(0)
            .map_err(|_| VideoError::Gst("an unreadable frame".into()))?;
        let row = w as usize * 4;
        let mut rgba = Vec::with_capacity(row * h as usize);
        for y in 0..h as usize {
            rgba.extend_from_slice(&data[y * stride..y * stride + row]);
        }
        // Opaque: alpha as decoded may be anything for formats without it.
        for px in rgba.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Ok(Frame {
            width: w,
            height: h,
            time,
            rgba,
        })
    }

    /// The frame showing at `t` ns: exactly (`exact`) or the keyframe at or
    /// before it (at once, for scrubbing). `None` past the end.
    ///
    /// Ask for a frame by its start ([`crate::FrameIndex::times`]): some
    /// decoders stamp a frame a seek cut into with the seek's time, so its
    /// [`Frame::time`] is only its own start when asked for that.
    pub fn frame_at(&mut self, t: i64, exact: bool) -> Result<Option<Frame>> {
        self.seek(t, exact)?;
        match self.sink.try_pull_preroll(self.timeout) {
            Some(s) => self.frame_of(&s).map(Some),
            None => Ok(None),
        }
    }

    /// Play from the frame showing at `t`: [`Self::next_frame`] then gives it
    /// and the frames after it in order.
    pub fn play_from(&mut self, t: i64) -> Result<()> {
        self.seek(t, true)?;
        self.p.pipeline.set_state(gst::State::Playing)?;
        self.playing = true;
        Ok(())
    }

    /// The next frame while playing (`None` at the end).
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        if !self.playing {
            return Err(VideoError::Gst("not playing".into()));
        }
        match self.sink.try_pull_sample(self.timeout) {
            Some(s) => self.frame_of(&s).map(Some),
            None if self.sink.is_eos() => Ok(None),
            None => Err(VideoError::media(&self.p.path, "a frame timed out")),
        }
    }
}
