//! Rendering a window's widget tree to a PNG (development aid for visual
//! checks; captures only FaderFrame's own pixels, never the desktop).

use gtk::prelude::*;
use std::path::Path;

pub fn window_to_png(window: &impl IsA<gtk::Window>, path: &Path) -> Result<(), String> {
    let window = window.upcast_ref::<gtk::Window>();
    let (w, h) = (window.width(), window.height());
    if w <= 0 || h <= 0 {
        return Err("the window has no size yet".into());
    }
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let node = snapshot
        .to_node()
        .ok_or_else(|| "nothing to render".to_string())?;
    let renderer = window
        .native()
        .and_then(|n| n.renderer())
        .ok_or_else(|| "the window is not realized".to_string())?;
    let texture = renderer.render_texture(node, None);
    texture.save_to_png(path).map_err(|e| e.to_string())
}

/// Render an open popover (a context menu) as it is shown.
pub fn popover_to_png(popover: &gtk::Popover, path: &Path) -> Result<(), String> {
    let (w, h) = (popover.width(), popover.height());
    if w <= 0 || h <= 0 {
        return Err("the menu has no size".into());
    }
    let paintable = gtk::WidgetPaintable::new(Some(popover));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let node = snapshot
        .to_node()
        .ok_or_else(|| "nothing to render".to_string())?;
    let renderer = popover
        .native()
        .and_then(|n| n.renderer())
        .ok_or_else(|| "the menu is not realized".to_string())?;
    renderer
        .render_texture(node, None)
        .save_to_png(path)
        .map_err(|e| e.to_string())
}

/// Render the given canvases stacked top to bottom (fallback when GTK has
/// no current frame of the window, e.g. while it is on a hidden workspace).
pub fn canvases_to_png(
    window: &gtk::ApplicationWindow,
    canvases: &[crate::canvas::CanvasWidget],
    path: &Path,
) -> Result<(), String> {
    let snapshot = gtk::Snapshot::new();
    let mut y = 0.0f32;
    for c in canvases {
        let Some(node) = c.render_node() else {
            continue;
        };
        snapshot.save();
        snapshot.translate(&gtk::graphene::Point::new(0.0, y));
        snapshot.append_node(&node);
        snapshot.restore();
        y += c.height() as f32 + 4.0;
    }
    let node = snapshot
        .to_node()
        .ok_or_else(|| "no canvas rendered".to_string())?;
    let renderer = window
        .native()
        .and_then(|n| n.renderer())
        .ok_or_else(|| "the window is not realized".to_string())?;
    renderer
        .render_texture(node, None)
        .save_to_png(path)
        .map_err(|e| e.to_string())
}

/// Capture the window. A window that is being redrawn (playback animates
/// the editors) has no current frame between paints, so it is captured
/// right after its next paint; if no paint comes (a hidden workspace gets
/// no frames), the editor canvases are rendered directly instead.
pub fn capture(
    window: &gtk::ApplicationWindow,
    canvases: &[crate::canvas::CanvasWidget],
    path: &Path,
) {
    if window_to_png(window, path).is_ok() {
        tracing::info!("screenshot saved to {}", path.display());
        return;
    }
    let done = std::rc::Rc::new(std::cell::Cell::new(false));
    let fallback = {
        let (window, canvases, path, done) = (
            window.clone(),
            canvases.to_vec(),
            path.to_path_buf(),
            std::rc::Rc::clone(&done),
        );
        move || {
            if done.replace(true) {
                return;
            }
            let result = window_to_png(&window, &path).or_else(|e| {
                tracing::info!("full-window capture failed ({e}); rendering the canvases");
                canvases_to_png(&window, &canvases, &path)
            });
            match result {
                Ok(()) => tracing::info!("screenshot saved to {}", path.display()),
                Err(e) => tracing::warn!("screenshot failed: {e}"),
            }
        }
    };
    if let Some(clock) = window.frame_clock() {
        let handler = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (h, w, p, d) = (
            std::rc::Rc::clone(&handler),
            window.clone(),
            path.to_path_buf(),
            std::rc::Rc::clone(&done),
        );
        let id = clock.connect_after_paint(move |clock| {
            if let Some(id) = h.borrow_mut().take() {
                clock.disconnect(id);
            }
            if !d.get() && window_to_png(&w, &p).is_ok() {
                d.set(true);
                tracing::info!("screenshot saved to {}", p.display());
            }
        });
        *handler.borrow_mut() = Some(id);
        window.queue_draw();
    }
    // No paint within half a second: render what can be rendered.
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(500), fallback);
}
