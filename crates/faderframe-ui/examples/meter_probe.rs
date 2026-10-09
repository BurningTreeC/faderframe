//! Repeated GTK renders of the actual meter controls, without audio or a window.
//! `meter_probe [vulkan|gl|cairo] [frames]`; needs a display connection.
use faderframe_ui::painter::{PathCache, SnapshotPainter, TextCache};
use faderframe_ui_canvas::{MeterKind, Painter, Rect, Theme, controls};
use gtk::{gdk, graphene, gsk, prelude::*};
use std::cell::RefCell;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    gtk::init()?;
    let backend = std::env::args().nth(1).unwrap_or_else(|| "vulkan".into());
    let frames = std::env::args()
        .nth(2)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3000);
    let display = gdk::Display::default().ok_or("no display")?;
    let surface = gdk::Surface::new_toplevel(&display);
    let renderer: gsk::Renderer = match backend.as_str() {
        "gl" => gsk::GLRenderer::new().upcast(),
        "cairo" => gsk::CairoRenderer::new().upcast(),
        _ => gsk::Renderer::for_surface(&surface).ok_or("no renderer")?,
    };
    if !renderer.is_realized() {
        renderer.realize_for_display(&display)?;
    }
    eprintln!("renderer: {}", renderer.type_().name());
    let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let text = RefCell::new(TextCache::default());
    let paths = RefCell::new(PathCache::default());
    let viewport = graphene::Rect::new(0.0, 0.0, 800.0, 500.0);
    let mut baseline = None;
    let started = std::time::Instant::now();
    for frame in 0..=frames {
        text.borrow_mut().begin_frame();
        paths.borrow_mut().begin_frame();
        let snapshot = gtk::Snapshot::new();
        snapshot.push_clip(&viewport);
        let mut p = SnapshotPainter::new(&snapshot, widget.upcast_ref(), &text, &paths);
        let mut th = Theme::by_id("vintage");
        p.fill(Rect::new(0.0, 0.0, 800.0, 500.0), th.console.panel_bottom);
        for (row, kind) in [MeterKind::Plasma, MeterKind::Bar, MeterKind::Ladder]
            .iter()
            .enumerate()
        {
            th.console.look.meter = *kind;
            for col in 0..16 {
                let phase = ((frame % 64) as f32 * 0.071 + col as f32).sin();
                let level = -35.0 + 36.0 * phase;
                let levels = [
                    controls::MeterLevel::new(level, -6.0, false),
                    controls::MeterLevel::new(level - 3.0, -3.0, false),
                ];
                controls::meter(
                    &mut p,
                    Rect::new(
                        10.25 + col as f32 * 49.0,
                        10.5 + row as f32 * 160.0,
                        18.0,
                        147.0,
                    ),
                    &levels,
                    &th,
                );
            }
        }
        snapshot.pop();
        let node = snapshot.to_node().ok_or("empty snapshot")?;
        let texture = renderer.render_texture(&node, Some(&viewport));
        if frame % 64 == 0 {
            let mut pixels = vec![0u8; 800 * 500 * 4];
            texture.download(&mut pixels, 800 * 4);
            if let Some(ref expected) = baseline {
                if expected != &pixels {
                    texture.save_to_png("/tmp/faderframe-meter-corrupted.png")?;
                    node.write_to_file("/tmp/faderframe-meter-corrupted.node")?;
                    renderer.unrealize();
                    return Err(format!(
                        "{backend}: frame {frame} differs from the identical first frame"
                    )
                    .into());
                }
            } else {
                texture.save_to_png("/tmp/faderframe-meter-baseline.png")?;
                baseline = Some(pixels);
            }
        }
        while gtk::glib::MainContext::default().pending() {
            gtk::glib::MainContext::default().iteration(false);
        }
    }
    renderer.unrealize();
    println!(
        "{backend}: {frames} frames, identical repeated frames, {:?}",
        started.elapsed()
    );
    Ok(())
}
