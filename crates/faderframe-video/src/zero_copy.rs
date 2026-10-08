//! Zero-copy frames (Linux): VA-API decodes and scales the picture into a
//! dmabuf the display imports as it is, so a frame never travels through
//! the CPU. The shell says which dmabuf formats its display takes
//! ([`set_display_formats`]); the playback decoder then asks VA-API's
//! post-processor for an RGB one of those at the shown size (its colour
//! conversion knows the picture's matrix, which a YUV import would have to
//! guess). When anything is missing — no VA-API, no format both sides know,
//! an import that fails ([`failed`]) — frames come as RGBA, as before.

use std::os::fd::RawFd;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

static FORMATS: RwLock<Vec<(u32, u64)>> = RwLock::new(Vec::new());
static FAILED: AtomicBool = AtomicBool::new(false);
static ALLOWED: AtomicBool = AtomicBool::new(true);

/// RGB formats asked for (8 bits a channel; GTK takes them all).
const RGB: [&str; 4] = ["XR24", "AR24", "XB24", "AB24"];

/// The dmabuf formats (fourcc, modifier) the display imports.
pub fn set_display_formats(formats: Vec<(u32, u64)>) {
    *FORMATS.write().unwrap_or_else(|p| p.into_inner()) = formats;
}

/// Whether zero-copy frames are wanted (the preference).
pub fn set_allowed(on: bool) {
    ALLOWED.store(on, Ordering::Relaxed);
}

/// Whether playback decodes into dmabufs (`FADERFRAME_VIDEO_DMABUF=0`
/// turns it off).
pub fn enabled() -> bool {
    ALLOWED.load(Ordering::Relaxed)
        && !FAILED.load(Ordering::Relaxed)
        && std::env::var("FADERFRAME_VIDEO_DMABUF").map_or(true, |v| v != "0")
        && !FORMATS.read().unwrap_or_else(|p| p.into_inner()).is_empty()
}

/// The display could not take a frame: RGBA from now on.
pub fn failed(why: &str) {
    if !FAILED.swap(true, Ordering::Relaxed) {
        tracing::warn!("video frames go through memory from now on: {why}");
    }
}

/// A DRM fourcc from its four letters.
pub fn fourcc(code: &str) -> u32 {
    let b = code.as_bytes();
    let at = |i: usize| b.get(i).copied().unwrap_or(b' ') as u32;
    at(0) | (at(1) << 8) | (at(2) << 16) | (at(3) << 24)
}

/// "XR24:0x0200000000000901" → (fourcc, modifier); no modifier: linear.
fn parse_drm_format(s: &str) -> Option<(u32, u64)> {
    let (code, modifier) = match s.split_once(':') {
        Some((c, m)) => (c, u64::from_str_radix(m.trim_start_matches("0x"), 16).ok()?),
        None => (s, 0),
    };
    (code.len() == 4).then(|| (fourcc(code), modifier))
}

/// The `drm-format` values of `caps` (DMABuf structures).
fn drm_formats(caps: &gst::CapsRef) -> Vec<String> {
    let mut out = Vec::new();
    for (s, features) in caps.iter_with_features() {
        if !features.contains("memory:DMABuf") {
            continue;
        }
        if let Ok(list) = s.get::<gst::List>("drm-format") {
            out.extend(list.iter().filter_map(|v| v.get::<String>().ok()));
        } else if let Ok(one) = s.get::<String>("drm-format") {
            out.push(one);
        }
    }
    out
}

/// The RGB dmabuf formats VA-API's post-processor makes here (empty
/// without VA-API).
pub fn postproc_formats() -> Vec<(u32, u64)> {
    use gst::prelude::*;
    let Ok(post) = gst::ElementFactory::make("vapostproc").build() else {
        return Vec::new();
    };
    let Some(pad) = post.static_pad("src") else {
        return Vec::new();
    };
    drm_formats(&pad.pad_template_caps())
        .iter()
        .filter(|f| RGB.iter().any(|c| f.starts_with(c)))
        .filter_map(|f| parse_drm_format(f))
        .collect()
}

/// Caps for `post`'s output at `width`×`height`: the RGB formats it makes
/// that the display takes, or `None` when there is none.
pub(crate) fn caps_for(post: &gst::Element, width: u32, height: u32) -> Option<gst::Caps> {
    use gst::prelude::*;
    let made = post.static_pad("src")?.pad_template_caps();
    let display = FORMATS.read().unwrap_or_else(|p| p.into_inner()).clone();
    let wanted: Vec<String> = drm_formats(&made)
        .into_iter()
        .filter(|f| RGB.iter().any(|c| f.starts_with(c)))
        .filter(|f| parse_drm_format(f).is_some_and(|x| display.contains(&x)))
        .collect();
    if wanted.is_empty() {
        return None;
    }
    let mut caps = gst::Caps::builder("video/x-raw")
        .features(["memory:DMABuf"])
        .field("format", "DMA_DRM")
        .field("drm-format", gst::List::new(wanted))
        .field("width", width as i32)
        .field("height", height as i32)
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build();
    caps.fixate();
    Some(caps)
}

/// One plane of a dmabuf frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    /// Valid while the frame lives.
    pub fd: RawFd,
    pub offset: u32,
    pub stride: u32,
}

/// A frame in a dmabuf: the planes, and the buffer that keeps them.
#[derive(Clone)]
pub struct GpuFrame {
    pub fourcc: u32,
    pub modifier: u64,
    pub planes: Vec<Plane>,
    buffer: gst::Buffer,
}

impl PartialEq for GpuFrame {
    fn eq(&self, other: &Self) -> bool {
        self.buffer.as_ptr() == other.buffer.as_ptr()
    }
}

impl Eq for GpuFrame {}

impl std::fmt::Debug for GpuFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GpuFrame({:08x}:{:#x}, {} planes)",
            self.fourcc,
            self.modifier,
            self.planes.len()
        )
    }
}

impl GpuFrame {
    /// The dmabuf planes of `buffer` (DMA_DRM caps described by `info`).
    pub(crate) fn of(buffer: &gst::Buffer, info: &gst_video::VideoInfoDmaDrm) -> Option<Self> {
        let meta = buffer.meta::<gst_video::VideoMeta>();
        let n = meta
            .as_ref()
            .map_or(info.n_planes() as usize, |m| m.n_planes() as usize);
        let mut planes = Vec::with_capacity(n);
        for i in 0..n {
            let (offset, stride) = match &meta {
                Some(m) => (m.offset()[i], m.stride()[i]),
                None => (info.offset()[i], info.stride()[i]),
            };
            let (range, skip) = buffer.find_memory(offset..offset + 1)?;
            let mem = buffer.peek_memory(range.start);
            let dma = mem.downcast_memory_ref::<gst_allocators::DmaBufMemory>()?;
            planes.push(Plane {
                fd: dma.fd(),
                offset: (mem.offset() + skip) as u32,
                stride: stride.max(0) as u32,
            });
        }
        Some(Self {
            fourcc: info.fourcc(),
            modifier: info.modifier(),
            planes,
            buffer: buffer.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drm_formats_read() {
        assert_eq!(fourcc("XR24"), 0x3432_5258);
        assert_eq!(
            parse_drm_format("XR24:0x0200000000000901"),
            Some((0x3432_5258, 0x0200_0000_0000_0901))
        );
        assert_eq!(parse_drm_format("YUYV"), Some((fourcc("YUYV"), 0)));
        assert_eq!(parse_drm_format("R8"), None);
    }
}
