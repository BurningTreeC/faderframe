//! Frames as RGBA at a requested size: one exactly (a seek decodes from the
//! keyframe before it), a keyframe at once (scrubbing), or in order from a
//! start (playback, never seeking).

use crate::streams::{Kind, Pipeline, make};
use crate::{Result, VideoError};
use gst::prelude::*;
use gst_video::prelude::*;
use std::path::Path;
use std::time::Duration;

/// A frame in video memory ([`crate::zero_copy`], Linux).
#[cfg(target_os = "linux")]
pub type Gpu = crate::zero_copy::GpuFrame;
/// No frames in video memory here.
#[cfg(not(target_os = "linux"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gpu {}

/// A decoded frame: rows of RGBA (opaque), `width * 4` bytes each — or,
/// from a zero-copy decoder, a dmabuf (`gpu`, `rgba` empty).
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Its start in the file's timeline (ns, stream time).
    pub time: i64,
    pub rgba: Vec<u8>,
    pub gpu: Option<Gpu>,
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
    /// The memory it takes (in video memory for a dmabuf frame).
    pub fn bytes(&self) -> usize {
        if self.gpu.is_some() {
            self.width as usize * self.height as usize * 4
        } else {
            self.rgba.len()
        }
    }

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
    /// HDR or wide gamut: decoded at 16 bits and mapped for the screen.
    map: Option<std::sync::Arc<crate::colour::ToneMap>>,
    width: u32,
    height: u32,
    playing: bool,
    timeout: gst::ClockTime,
}

impl Decoder {
    /// Open `path`'s picture, scaled to `width`×`height` (see [`fit`]);
    /// shows the first frame. Its colours are taken as SDR BT.709 (see
    /// [`Self::open_colour`]).
    pub fn open(path: &Path, width: u32, height: u32) -> Result<Self> {
        Self::open_colour(path, width, height, crate::colour::Colour::default())
    }

    /// [`Self::open`] for a picture whose colours mean `colour`: HDR and
    /// wide-gamut ones are mapped for an SDR BT.709 screen.
    pub fn open_colour(
        path: &Path,
        width: u32,
        height: u32,
        colour: crate::colour::Colour,
    ) -> Result<Self> {
        let convert = make("videoconvertscale")?;
        // Mapped: 16 bits a channel, still in the picture's own transfer
        // and primaries (the converter changes only matrix and range).
        let map = colour
            .needs_mapping()
            .then(|| crate::colour::tone_map(colour));
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", if map.is_some() { "RGBA64_LE" } else { "RGBA" })
            .field("width", width as i32)
            .field("height", height as i32)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        let mut d = Self::open_with(path, width, height, convert, caps, false)?;
        d.map = map;
        Ok(d)
    }

    /// Open `path`'s picture decoded into dmabufs at `width`×`height`
    /// (VA-API scales and converts; see [`crate::zero_copy`]).
    #[cfg(target_os = "linux")]
    pub fn open_dmabuf(path: &Path, width: u32, height: u32) -> Result<Self> {
        let post = make("vapostproc")?;
        let caps = crate::zero_copy::caps_for(&post, width, height).ok_or_else(|| {
            VideoError::Unavailable("no dmabuf format both VA-API and the display know".into())
        })?;
        let d = Self::open_with(path, width, height, post, caps, true)?;
        // A pipeline that negotiates something else is no use.
        let caps = d
            .sink
            .static_pad("sink")
            .and_then(|p| p.current_caps())
            .ok_or_else(|| VideoError::Unavailable("nothing negotiated".into()))?;
        if !caps
            .features(0)
            .is_some_and(|f| f.contains("memory:DMABuf"))
        {
            return Err(VideoError::Unavailable(format!("negotiated {caps}")));
        }
        tracing::debug!("{}: frames in dmabufs: {caps}", path.display());
        Ok(d)
    }

    fn open_with(
        path: &Path,
        width: u32,
        height: u32,
        convert: gst::Element,
        caps: gst::Caps,
        video_meta: bool,
    ) -> Result<Self> {
        let p = Pipeline::new(path)?;
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
        if video_meta && let Some(pad) = sink.static_pad("sink") {
            // Planes are described by a video meta (dmabufs need it).
            pad.add_probe(gst::PadProbeType::QUERY_DOWNSTREAM, |_, info| {
                if let Some(gst::PadProbeData::Query(q)) = info.data.as_mut()
                    && let gst::QueryViewMut::Allocation(a) = q.view_mut()
                {
                    a.add_allocation_meta::<gst_video::VideoMeta>(None);
                }
                gst::PadProbeReturn::Ok
            });
        }
        let elements = [convert, filter, sink.clone().upcast()];
        p.pipeline.add_many(&elements)?;
        gst::Element::link_many(&elements)?;
        let next = elements[0]
            .static_pad("sink")
            .ok_or_else(|| VideoError::Gst("a converter without a sink".into()))?;
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
            map: None,
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
        #[cfg(target_os = "linux")]
        if caps
            .features(0)
            .is_some_and(|f| f.contains("memory:DMABuf"))
        {
            let info = gst_video::VideoInfoDmaDrm::from_caps(caps)?;
            let owned = sample
                .buffer_owned()
                .ok_or_else(|| VideoError::Gst("a sample without a frame".into()))?;
            let gpu = crate::zero_copy::GpuFrame::of(&owned, &info)
                .ok_or_else(|| VideoError::Gst("a frame that is not in dmabufs".into()))?;
            return Ok(Frame {
                width: info.width(),
                height: info.height(),
                time,
                rgba: Vec::new(),
                gpu: Some(gpu),
            });
        }
        let info = gst_video::VideoInfo::from_caps(caps)?;
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
            .map_err(|_| VideoError::Gst("an unreadable frame".into()))?;
        let (w, h) = (info.width(), info.height());
        let stride = frame.plane_stride()[0] as usize;
        let data = frame
            .plane_data(0)
            .map_err(|_| VideoError::Gst("an unreadable frame".into()))?;
        if let Some(map) = &self.map {
            let mut rgba = Vec::new();
            map.map(data, stride, w as usize, h as usize, &mut rgba);
            return Ok(Frame {
                width: w,
                height: h,
                time,
                rgba,
                gpu: None,
            });
        }
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
            gpu: None,
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
        let first = match self.sink.try_pull_preroll(self.timeout) {
            Some(s) => self.frame_of(&s)?,
            None => return Ok(None),
        };
        // Past it: the demuxer started at the keyframe after (some MXF
        // index tables) — from further back, decoded up to the frame.
        const HAIR: i64 = 1_000_000;
        if first.time <= t + HAIR {
            return Ok(Some(first));
        }
        let mut back = 1_000_000_000;
        for _ in 0..4 {
            self.play_from((t - back).max(0))?;
            let mut last = None;
            while let Some(f) = self.next_frame()? {
                if f.time > t + HAIR {
                    break;
                }
                let at = f.time >= t - HAIR;
                last = Some(f);
                if at {
                    break;
                }
            }
            match last {
                Some(f) if !exact || f.time >= t - HAIR => return Ok(Some(f)),
                Some(_) | None if t - back <= 0 => return Ok(Some(first)),
                _ => back *= 4,
            }
        }
        Ok(Some(first))
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
