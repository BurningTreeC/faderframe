//! Windows: GStreamer's Direct3D 12 converter (`d3d12convert`) scales the
//! decoded picture and converts it to RGBA in a texture, whatever decoded
//! it (a D3D12 or D3D11 decoder hands it over on the GPU, a software one is
//! uploaded once). Its own pool makes the textures on a shared heap
//! (`D3D12_HEAP_FLAG_SHARED`, GStreamer's default for a filter's pool),
//! which GTK needs to import them. The shell takes the texture from the
//! frame's memory through GStreamer's d3d12 library (`faderframe-ui`'s
//! `video_d3d12`), says whether it can at all ([`set_d3d12_display`]) and
//! falls back to RGBA frames when anything fails ([`super::failed`]).
//!
//! UNTESTED: written against GStreamer 1.26's d3d12 library and GTK 4.20's
//! `GdkD3D12Texture` as documented; nothing that built it could run
//! Direct3D 12.

use super::{ALLOWED, FAILED};
use gst_video::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// The caps feature of GStreamer's D3D12 memory.
pub const D3D12_MEMORY: &str = "memory:D3D12Memory";

static DISPLAY: AtomicBool = AtomicBool::new(false);

/// Whether the shell can show frames in D3D12 textures (it found
/// GStreamer's d3d12 library).
pub fn set_d3d12_display(on: bool) {
    DISPLAY.store(on, Ordering::Relaxed);
}

/// Whether playback converts into D3D12 textures
/// (`FADERFRAME_VIDEO_D3D12=0` turns it off).
pub fn enabled() -> bool {
    ALLOWED.load(Ordering::Relaxed)
        && !FAILED.load(Ordering::Relaxed)
        && DISPLAY.load(Ordering::Relaxed)
        && std::env::var("FADERFRAME_VIDEO_D3D12").map_or(true, |v| v != "0")
}

/// Caps for the converter's output: RGBA in D3D12 memory at
/// `width`×`height`.
pub(crate) fn caps_for(width: u32, height: u32) -> gst::Caps {
    gst::Caps::builder("video/x-raw")
        .features([D3D12_MEMORY])
        .field("format", "RGBA")
        .field("width", width as i32)
        .field("height", height as i32)
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build()
}

/// A frame in a D3D12 texture: the buffer that keeps it (and keeps the
/// texture out of the converter's pool) and how it is laid out.
#[derive(Clone)]
pub struct GpuFrame {
    buffer: gst::Buffer,
    info: gst_video::VideoInfo,
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
            "GpuFrame(D3D12, {}x{})",
            self.info.width(),
            self.info.height()
        )
    }
}

impl GpuFrame {
    /// `buffer` (one D3D12 memory; RGBA as `info` says).
    pub(crate) fn of(buffer: &gst::Buffer, info: &gst_video::VideoInfo) -> Option<Self> {
        (buffer.n_memory() == 1).then(|| Self {
            buffer: buffer.clone(),
            info: info.clone(),
        })
    }

    /// The frame's memory, a `GstD3D12Memory` (as a `GstMemory*`), for the
    /// shell's calls into GStreamer's d3d12 library. Valid while this frame
    /// or a clone of it lives.
    pub fn memory(&self) -> *mut std::ffi::c_void {
        self.buffer.peek_memory(0).as_ptr() as *mut std::ffi::c_void
    }

    pub fn width(&self) -> u32 {
        self.info.width()
    }

    pub fn height(&self) -> u32 {
        self.info.height()
    }

    /// The picture as rows of RGBA, `width * 4` bytes each, through memory
    /// (GStreamer copies the texture back): the shell's check of its first
    /// import.
    pub fn read_back(&self) -> Option<Vec<u8>> {
        let frame =
            gst_video::VideoFrameRef::from_buffer_ref_readable(self.buffer.as_ref(), &self.info)
                .ok()?;
        let stride = usize::try_from(frame.plane_stride()[0]).ok()?;
        let data = frame.plane_data(0).ok()?;
        let row = self.info.width() as usize * 4;
        let mut out = Vec::with_capacity(row * self.info.height() as usize);
        for y in 0..self.info.height() as usize {
            out.extend_from_slice(data.get(y * stride..y * stride + row)?);
        }
        Some(out)
    }
}
