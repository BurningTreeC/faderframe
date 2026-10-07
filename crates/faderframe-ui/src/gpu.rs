//! The GPU painter for dense views (feature `gpu-painter`, see
//! `faderframe-ui-gpu`): one renderer per UI thread, made when a dense view
//! first paints with the preference on. Without a usable GPU, or after any
//! error, those views keep the GTK painter.

use faderframe_ui_canvas::Painter;

/// Whether dense views are drawn on the GPU: `FADERFRAME_GPU_PAINTER=1` or
/// `0` overrides the preference.
pub fn wanted(preference: bool) -> bool {
    match std::env::var("FADERFRAME_GPU_PAINTER").ok().as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => preference,
    }
}

/// Built with the GPU painter at all.
pub fn available() -> bool {
    cfg!(feature = "gpu-painter")
}

#[cfg(feature = "gpu-painter")]
mod imp {
    use super::Painter;
    use faderframe_ui_gpu::GpuRenderer;
    use gtk::prelude::*;
    use gtk::{gdk, glib, graphene};
    use std::cell::RefCell;

    enum State {
        Untried,
        /// Never dropped: at exit, when thread-locals are torn down, wgpu's
        /// own may be gone already (the system reclaims the GPU's memory).
        Ready(std::mem::ManuallyDrop<Box<GpuRenderer>>),
        Failed,
    }

    thread_local! {
        static GPU: RefCell<State> = const { RefCell::new(State::Untried) };
    }

    /// Paint `width × height` logical pixels (`scale` device pixels each)
    /// with `paint` on the GPU into `snapshot`; false when it cannot (the
    /// caller paints with GTK).
    pub fn paint(
        snapshot: &gtk::Snapshot,
        width: f32,
        height: f32,
        scale: f32,
        paint: impl FnOnce(&mut dyn Painter),
    ) -> bool {
        GPU.with(|g| {
            let mut g = g.borrow_mut();
            if matches!(*g, State::Untried) {
                *g = match GpuRenderer::new() {
                    Ok(r) => {
                        tracing::info!("dense views drawn on {}", r.adapter());
                        State::Ready(std::mem::ManuallyDrop::new(Box::new(r)))
                    }
                    Err(e) => {
                        tracing::warn!("GPU painter unavailable: {e}");
                        State::Failed
                    }
                };
            }
            let State::Ready(r) = &mut *g else {
                return false;
            };
            let pw = (width * scale).ceil().max(1.0) as u32;
            let ph = (height * scale).ceil().max(1.0) as u32;
            match r.render(pw, ph, scale, paint) {
                Ok(frame) => {
                    let stride = frame.stride();
                    let texture = gdk::MemoryTexture::new(
                        frame.width as i32,
                        frame.height as i32,
                        gdk::MemoryFormat::R8g8b8a8,
                        &glib::Bytes::from_owned(frame.pixels),
                        stride,
                    );
                    snapshot
                        .append_texture(&texture, &graphene::Rect::new(0.0, 0.0, width, height));
                    true
                }
                Err(e) => {
                    tracing::warn!("GPU painter failed, using GTK's: {e}");
                    *g = State::Failed;
                    false
                }
            }
        })
    }
}

#[cfg(feature = "gpu-painter")]
pub use imp::paint;

#[cfg(not(feature = "gpu-painter"))]
pub fn paint(
    _snapshot: &gtk::Snapshot,
    _width: f32,
    _height: f32,
    _scale: f32,
    _paint: impl FnOnce(&mut dyn Painter),
) -> bool {
    false
}
