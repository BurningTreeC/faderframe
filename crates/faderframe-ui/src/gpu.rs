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
    #[cfg(target_os = "linux")]
    use super::dmabuf_texture;
    use super::imports_dmabufs;
    use faderframe_ui_gpu::GpuRenderer;
    use gtk::prelude::*;
    use gtk::{gdk, glib, graphene};
    use std::cell::RefCell;

    enum State {
        Untried,
        /// Never dropped: at exit, when thread-locals are torn down, wgpu's
        /// own may be gone already (the system reclaims the GPU's memory).
        /// `dmabuf`: frames go to GTK as dmabufs (Linux, when the GPU
        /// exports and the display imports them). `share`: as shared D3D12
        /// textures (Windows), `checked` once the first matched.
        Ready {
            renderer: std::mem::ManuallyDrop<Box<GpuRenderer>>,
            dmabuf: bool,
            #[cfg(windows)]
            share: Option<crate::gpu_win32::Handover>,
            #[cfg(windows)]
            checked: bool,
        },
        Failed,
    }

    thread_local! {
        static GPU: RefCell<State> = const { RefCell::new(State::Untried) };
        /// Views rendering ahead: the last texture shown (and its size),
        /// shown again until a newer frame is finished.
        static LAST: RefCell<std::collections::HashMap<u64, Last>> =
            RefCell::new(std::collections::HashMap::new());
    }

    /// A view's last texture: the view (it may go), the texture, its size.
    type Last = (glib::WeakRef<gtk::Widget>, gdk::Texture, (u32, u32));

    fn view_key(widget: &gtk::Widget) -> u64 {
        widget.as_ptr() as usize as u64
    }

    /// Whether the view shows a frame behind the newest (it rendered ahead
    /// and stopped animating): one more paint brings it up to date.
    pub fn behind(widget: &gtk::Widget) -> bool {
        LAST.with(|l| l.borrow().contains_key(&view_key(widget)))
    }

    /// Paint `width × height` logical pixels (`scale` device pixels each)
    /// with `paint` on the GPU into `snapshot`; false when it cannot (the
    /// caller paints with GTK).
    pub fn paint(
        widget: &gtk::Widget,
        snapshot: &gtk::Snapshot,
        width: f32,
        height: f32,
        scale: f32,
        animating: bool,
        paint: impl FnOnce(&mut dyn Painter),
    ) -> bool {
        GPU.with(|g| {
            let mut g = g.borrow_mut();
            if matches!(*g, State::Untried) {
                // Windows: on the GPU GTK's GL draws with, to hand frames
                // over.
                #[cfg(windows)]
                let (share, luid) = crate::gpu_win32::handover(widget);
                #[cfg(not(windows))]
                let (luid, _) = (None, widget);
                *g = match GpuRenderer::new_on(luid) {
                    Ok(r) => {
                        let dmabuf = r.exports_dmabufs() && imports_dmabufs(snapshot);
                        #[cfg(windows)]
                        let share = share.filter(|s| match s {
                            crate::gpu_win32::Handover::Gl(gl) => {
                                r.shared_luid() == Some(gl.luid())
                            }
                            crate::gpu_win32::Handover::D3d12 => r.shared_luid().is_some(),
                        });
                        #[cfg(windows)]
                        let shared = share.is_some();
                        #[cfg(not(windows))]
                        let shared = false;
                        tracing::info!(
                            "dense views drawn on {}{}",
                            r.adapter(),
                            if dmabuf {
                                ", handed over as dmabufs"
                            } else if shared {
                                ", handed over as shared D3D12 textures"
                            } else {
                                ", read back"
                            }
                        );
                        State::Ready {
                            renderer: std::mem::ManuallyDrop::new(Box::new(r)),
                            dmabuf,
                            #[cfg(windows)]
                            share,
                            #[cfg(windows)]
                            checked: false,
                        }
                    }
                    Err(e) => {
                        tracing::warn!("GPU painter unavailable: {e}");
                        State::Failed
                    }
                };
            }
            let State::Ready {
                renderer: r,
                dmabuf,
                #[cfg(windows)]
                share,
                #[cfg(windows)]
                checked,
            } = &mut *g
            else {
                return false;
            };
            let pw = (width * scale).ceil().max(1.0) as u32;
            let ph = (height * scale).ceil().max(1.0) as u32;
            #[cfg(windows)]
            let handover = *dmabuf || share.is_some();
            #[cfg(not(windows))]
            let handover = *dmabuf;
            // An animating view whose last frame went out as a dmabuf of this
            // size renders ahead (the UI thread does not wait for the GPU);
            // any other paint waits for its frame.
            let key = view_key(widget);
            let ahead = animating
                && *dmabuf
                && LAST.with(|l| l.borrow().get(&key).is_some_and(|e| e.2 == (pw, ph)));
            let output = if ahead {
                r.render_ahead(key, pw, ph, scale, paint)
            } else {
                r.settle(key);
                LAST.with(|l| {
                    let mut l = l.borrow_mut();
                    l.remove(&key);
                    // Views gone while animating.
                    l.retain(|_, e| e.0.upgrade().is_some());
                });
                r.render_to(pw, ph, scale, handover, paint)
            };
            #[cfg(target_os = "linux")]
            let keep = |texture: &gdk::Texture| {
                if animating {
                    LAST.with(|l| {
                        l.borrow_mut()
                            .insert(key, (widget.downgrade(), texture.clone(), (pw, ph)))
                    });
                }
            };
            match output {
                Ok(faderframe_ui_gpu::Output::Pending) => {
                    let last = LAST.with(|l| l.borrow().get(&key).map(|e| e.1.clone()));
                    match last {
                        Some(texture) => {
                            snapshot.append_texture(
                                &texture,
                                &graphene::Rect::new(0.0, 0.0, width, height),
                            );
                            true
                        }
                        None => false,
                    }
                }
                Ok(faderframe_ui_gpu::Output::Pixels(frame)) => {
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
                #[cfg(target_os = "linux")]
                Ok(faderframe_ui_gpu::Output::Dmabuf(frame)) => match dmabuf_texture(frame) {
                    Ok(texture) => {
                        snapshot.append_texture(
                            &texture,
                            &graphene::Rect::new(0.0, 0.0, width, height),
                        );
                        keep(&texture);
                        true
                    }
                    Err(e) => {
                        // This frame is lost; the next ones are read back.
                        tracing::warn!("GTK did not take the dmabuf ({e}): reading frames back");
                        *dmabuf = false;
                        false
                    }
                },
                #[cfg(windows)]
                Ok(faderframe_ui_gpu::Output::Shared(frame)) => {
                    match super::shared_texture(r, share, checked, frame) {
                        Ok(texture) => {
                            snapshot.append_texture(
                                &texture,
                                &graphene::Rect::new(0.0, 0.0, width, height),
                            );
                            true
                        }
                        Err(e) => {
                            // This frame is lost; the next ones are read back.
                            tracing::warn!(
                                "GTK did not take the shared texture ({e}): reading frames back"
                            );
                            *share = None;
                            false
                        }
                    }
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

/// A shared frame as a GTK texture by the hand-over in use; the first one
/// is checked against the painter's own readback.
#[cfg(all(feature = "gpu-painter", windows))]
fn shared_texture(
    r: &faderframe_ui_gpu::GpuRenderer,
    share: &mut Option<crate::gpu_win32::Handover>,
    checked: &mut bool,
    frame: faderframe_ui_gpu::SharedFrame,
) -> Result<gtk::gdk::Texture, String> {
    use crate::gpu_win32::{Handover, d3d12_texture, download, same};
    let expected = if *checked {
        None
    } else {
        Some(r.read_shared(&frame).map_err(|e| e.to_string())?)
    };
    let texture = match share.as_mut().ok_or("no hand-over")? {
        Handover::Gl(gl) => {
            if let Some(want) = &expected
                && !same(&gl.read(&frame)?, want)
            {
                return Err("GL sees other pixels".into());
            }
            gl.texture(frame)?
        }
        Handover::D3d12 => {
            let t = d3d12_texture(frame)?;
            if let Some(want) = &expected
                && !same(&download(&t), want)
            {
                return Err("GTK sees other pixels".into());
            }
            t
        }
    };
    *checked = true;
    Ok(texture)
}

#[cfg(feature = "gpu-painter")]
pub use imp::{behind, paint};

/// Whether the display imports linear RGBA dmabufs (what the GPU painter
/// hands over); `FADERFRAME_GPU_DMABUF=0` turns the hand-over off.
#[cfg(all(feature = "gpu-painter", target_os = "linux"))]
fn imports_dmabufs(snapshot: &gtk::Snapshot) -> bool {
    use gtk::prelude::*;
    let _ = snapshot;
    std::env::var("FADERFRAME_GPU_DMABUF").as_deref() != Ok("0")
        && gtk::gdk::Display::default().is_some_and(|d| {
            d.dmabuf_formats().contains(
                faderframe_ui_gpu::FOURCC_AB24,
                faderframe_ui_gpu::MODIFIER_LINEAR,
            )
        })
}

#[cfg(all(feature = "gpu-painter", not(target_os = "linux")))]
fn imports_dmabufs(_snapshot: &gtk::Snapshot) -> bool {
    false
}

/// A GTK texture of a dmabuf frame; its buffer goes back to the painter's
/// pool when GTK releases the texture.
#[cfg(all(feature = "gpu-painter", target_os = "linux"))]
fn dmabuf_texture(frame: faderframe_ui_gpu::DmabufFrame) -> Result<gtk::gdk::Texture, String> {
    use gtk::gdk;
    let display = gdk::Display::default().ok_or("no display")?;
    let builder = gdk::DmabufTextureBuilder::new()
        .set_display(&display)
        .set_width(frame.width)
        .set_height(frame.height)
        .set_fourcc(frame.fourcc)
        .set_modifier(frame.modifier)
        .set_n_planes(1)
        .set_offset(0, frame.offset)
        .set_stride(0, frame.stride)
        .set_premultiplied(false);
    let release = frame.release;
    // SAFETY: the descriptor stays open while `release` lives (the pool
    // keeps the buffer until then); GTK drops the closure, and with it
    // `release`, when it no longer uses the descriptor.
    unsafe {
        builder
            .set_fd(0, frame.fd)
            .build_with_release_func(move || drop(release))
    }
    .map_err(|e| e.to_string())
}

#[cfg(not(feature = "gpu-painter"))]
pub fn paint(
    _widget: &gtk::Widget,
    _snapshot: &gtk::Snapshot,
    _width: f32,
    _height: f32,
    _scale: f32,
    _animating: bool,
    _paint: impl FnOnce(&mut dyn Painter),
) -> bool {
    false
}

#[cfg(not(feature = "gpu-painter"))]
pub fn behind(_widget: &gtk::Widget) -> bool {
    false
}
