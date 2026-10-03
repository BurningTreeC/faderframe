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

/// Capture the window; if GTK has no frame of it (hidden workspace while
/// animating), render the editor canvases directly instead.
pub fn capture(
    window: &gtk::ApplicationWindow,
    canvases: &[crate::canvas::CanvasWidget],
    path: &Path,
) {
    let result = window_to_png(window, path).or_else(|_| canvases_to_png(window, canvases, path));
    match result {
        Ok(()) => tracing::info!("screenshot saved to {}", path.display()),
        Err(e) => tracing::warn!("screenshot failed: {e}"),
    }
}
