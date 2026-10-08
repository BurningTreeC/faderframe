//! Picture outputs: frames pushed at an output's own rate — to a
//! Blackmagic DeckLink card (SDI or HDMI to a broadcast monitor or a
//! projector, through GStreamer's `decklinkvideosink`, which loads the
//! card's driver library), or to a sink tests read. The sink paces the
//! pushing (it takes a frame when it has room), so whoever pushes picks
//! each frame for when it will be shown.

use crate::{Result, VideoError};
use gst::glib;
use gst::prelude::*;

/// Where the picture goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sink {
    /// DeckLink device `device` (from 0) in `mode` ("1080p25", "1080i50",
    /// "720p5994", "pal", "ntsc", "2160p25", …).
    DeckLink { device: u32, mode: String },
    /// Frames kept for [`Output::pull`] (tests), at `size` and `fps`.
    Test { size: (u32, u32), fps: (u32, u32) },
}

impl Sink {
    /// The frame size and rate it takes.
    pub fn format(&self) -> Option<((u32, u32), (u32, u32))> {
        match self {
            Sink::DeckLink { mode, .. } => mode_format(mode),
            Sink::Test { size, fps } => Some((*size, *fps)),
        }
    }
}

/// A DeckLink mode's frame size and frame rate ("1080i50": 1920×1080 at
/// 25 frames a second).
pub fn mode_format(mode: &str) -> Option<((u32, u32), (u32, u32))> {
    match mode {
        "ntsc" | "ntsc2398" => return Some(((720, 486), (30_000, 1001))),
        "pal" => return Some(((720, 576), (25, 1))),
        _ => {}
    }
    let (lines, rest) = mode.split_at(mode.find(['p', 'i'])?);
    let interlaced = rest.starts_with('i');
    let rate = &rest[1..];
    let size = match lines {
        "720" => (1280, 720),
        "1080" => (1920, 1080),
        "1556" => (2048, 1556),
        "2160" => (3840, 2160),
        "4320" => (7680, 4320),
        _ => return None,
    };
    let mut fps = match rate {
        "2398" => (24_000, 1001),
        "2997" => (30_000, 1001),
        "5994" => (60_000, 1001),
        "11988" => (120_000, 1001),
        r => (r.parse().ok()?, 1),
    };
    // An interlaced mode names its field rate.
    if interlaced {
        fps.0 /= 2;
    }
    Some((size, fps))
}

/// The modes `decklinkvideosink` offers (ids for [`Sink::DeckLink`], and
/// labels), or none without the element.
pub fn decklink_modes() -> Vec<(String, String)> {
    if crate::init().is_err() {
        return Vec::new();
    }
    let Ok(sink) = gst::ElementFactory::make("decklinkvideosink").build() else {
        return Vec::new();
    };
    let Some(spec) = sink.find_property("mode") else {
        return Vec::new();
    };
    let Some(e) = spec.downcast_ref::<glib::ParamSpecEnum>() else {
        return Vec::new();
    };
    e.enum_class()
        .values()
        .iter()
        .filter(|v| mode_format(v.nick()).is_some())
        .map(|v| (v.nick().to_string(), v.name().to_string()))
        .collect()
}

/// The DeckLink outputs on this computer (device number, name).
pub fn decklink_devices() -> Vec<(u32, String)> {
    if crate::init().is_err() || gst::ElementFactory::find("decklinkvideosink").is_none() {
        return Vec::new();
    }
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Sink"), None);
    let _ = monitor.start();
    let mut out: Vec<(u32, String)> = monitor
        .devices()
        .into_iter()
        .filter_map(|d| {
            let props = d.properties()?;
            let n = props.get::<i32>("device-number").ok()?;
            Some((n.max(0) as u32, d.display_name().to_string()))
        })
        .collect();
    monitor.stop();
    out.sort();
    out
}

/// An open output.
pub struct Output {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    test: Option<gst_app::AppSink>,
    size: (u32, u32),
    fps: (u32, u32),
    n: u64,
}

impl Output {
    pub fn open(sink: &Sink) -> Result<Self> {
        crate::init()?;
        let (size, fps) = sink
            .format()
            .ok_or_else(|| VideoError::Unavailable("an output mode this does not know".into()))?;
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", size.0 as i32)
            .field("height", size.1 as i32)
            .field("framerate", gst::Fraction::new(fps.0 as i32, fps.1 as i32))
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        let src = gst_app::AppSrc::builder()
            .caps(&caps)
            .format(gst::Format::Time)
            .is_live(true)
            .block(true)
            // Two frames queued at most.
            .max_bytes(size.0 as u64 * size.1 as u64 * 4 * 2)
            .build();
        let pipeline = gst::Pipeline::new();
        let convert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|_| VideoError::Missing("videoconvert".into()))?;
        let mut test = None;
        let out: gst::Element = match sink {
            Sink::DeckLink { device, mode } => {
                let s = gst::ElementFactory::make("decklinkvideosink")
                    .property("device-number", *device as i32)
                    .build()
                    .map_err(|_| VideoError::Missing("decklinkvideosink".into()))?;
                s.set_property_from_str("mode", mode);
                s
            }
            Sink::Test { .. } => {
                let s = gst_app::AppSink::builder()
                    .sync(true)
                    .max_buffers(512)
                    .drop(false)
                    .build();
                test = Some(s.clone());
                s.upcast()
            }
        };
        let elements = [src.clone().upcast::<gst::Element>(), convert, out];
        pipeline.add_many(&elements)?;
        gst::Element::link_many(&elements)?;
        pipeline.set_state(gst::State::Playing).map_err(|_| {
            VideoError::Unavailable(match sink {
                Sink::DeckLink { device, .. } => {
                    format!("the DeckLink output {device} did not start (is the card's driver installed?)")
                }
                Sink::Test { .. } => "the test output did not start".into(),
            })
        })?;
        Ok(Self {
            pipeline,
            src,
            test,
            size,
            fps,
            n: 0,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// One frame's length (ns).
    pub fn frame_ns(&self) -> i64 {
        1_000_000_000 * self.fps.1 as i64 / self.fps.0.max(1) as i64
    }

    /// How far ahead of being shown a frame is pushed (ns): the frames
    /// queued and the sink's own.
    pub fn lead_ns(&self) -> i64 {
        let mut q = gst::query::Latency::new();
        let sink = if self.pipeline.query(&mut q) {
            q.result().1.nseconds() as i64
        } else {
            0
        };
        sink + 2 * self.frame_ns()
    }

    /// Push the next frame: `picture` (RGBA, at most the output's size,
    /// letterboxed in black) or black. Waits while the output is full.
    pub fn push(&mut self, picture: Option<&crate::Frame>) -> Result<()> {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let mut data = vec![0u8; w * h * 4];
        for px in data.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        if let Some(f) = picture.filter(|f| !f.rgba.is_empty()) {
            let (fw, fh) = ((f.width as usize).min(w), (f.height as usize).min(h));
            let (x0, y0) = ((w - fw) / 2, (h - fh) / 2);
            for y in 0..fh {
                let from = &f.rgba[y * f.width as usize * 4..][..fw * 4];
                data[((y0 + y) * w + x0) * 4..][..fw * 4].copy_from_slice(from);
            }
        }
        let mut buf = gst::Buffer::from_mut_slice(data);
        if let Some(b) = buf.get_mut() {
            let d = self.frame_ns();
            b.set_pts(crate::clock(self.n as i64 * d));
            b.set_duration(crate::clock(d));
        }
        self.n += 1;
        self.src
            .push_buffer(buf)
            .map_err(|e| VideoError::Gst(format!("picture output: {e}")))?;
        Ok(())
    }

    /// A frame the test sink took (its time and RGBA), waiting up to
    /// `timeout_ms`.
    pub fn pull(&self, timeout_ms: u64) -> Option<(i64, Vec<u8>)> {
        let sink = self.test.as_ref()?;
        let s = sink.try_pull_sample(gst::ClockTime::from_mseconds(timeout_ms))?;
        let b = s.buffer()?;
        let map = b.map_readable().ok()?;
        Some((b.pts().map_or(0, crate::ns), map.to_vec()))
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.src.end_of_stream();
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_and_modes_ask_safely() {
        // Whatever this computer has: never a panic (GStreamer starts
        // itself).
        let _ = decklink_devices();
        let modes = decklink_modes();
        assert!(modes.iter().all(|(id, _)| mode_format(id).is_some()));
    }

    #[test]
    fn modes_say_their_format() {
        assert_eq!(mode_format("1080p25"), Some(((1920, 1080), (25, 1))));
        assert_eq!(mode_format("1080i50"), Some(((1920, 1080), (25, 1))));
        assert_eq!(
            mode_format("1080p2997"),
            Some(((1920, 1080), (30_000, 1001)))
        );
        assert_eq!(
            mode_format("1080i5994"),
            Some(((1920, 1080), (30_000, 1001)))
        );
        assert_eq!(
            mode_format("2160p2398"),
            Some(((3840, 2160), (24_000, 1001)))
        );
        assert_eq!(mode_format("pal"), Some(((720, 576), (25, 1))));
        assert_eq!(mode_format("auto"), None);
    }
}
