//! Repeated GTK renders of the actual meter controls, without audio:
//! every 64th frame draws the same check levels, and each check
//! frame must come out as the first one did.
//!
//! `meter_probe [auto|gl|cairo] [frames] [--random] [--resize]
//! [--scale <s>] [--out <dir>] [--present] [--clipped] [--rms] [--gpu]
//! [--mixer] [--theme <id>]`;
//! needs a display connection.
//! - `--random`: the frames between checks draw levels of their own
//!   (continuous, as playback does: new glow and gradient sizes all the
//!   time), not a repeating cycle.
//! - `--resize`: they also draw the meters at other sizes and places.
//! - `--scale`: the whole picture at a surface scale (1.5 for a 150 %
//!   screen).
//! - `--present`: on Linux/X11, check pixels read back from a real window,
//!   exercising incremental redraws and buffer reuse. Set `GDK_BACKEND=x11`,
//!   `GDK_SCALE=1` and `GSK_RENDERER` to the desired backend before starting.
//!   Use `--scale` for larger meter geometry. On-screen/offscreen text
//!   rasterization may differ; inspect the saved images on a failure.
//! - `--clipped`: compare small redraw regions with the full frame.
//! - `--rms`: also draw the RMS columns and glowing peak lines.
//! - `--gpu`: use the Tools panel's Vello painter and GTK texture handover.
//! - `--mixer`: play the demo through a silent backend and paint the
//!   actual mixer, including paths/text sharing the renderer's caches.
//!   Use with `--present` to compare changing frames with fresh renders.
//!
//! The first check frame is saved as `meter-baseline.png`, a differing
//! one as `meter-corrupted.png` (and its node file) in `--out` (the temp
//! directory by default).
use faderframe_ui::painter::{PathCache, SnapshotPainter, TextCache};
use faderframe_ui_canvas::{MeterKind, Painter, Rect, Theme, controls};
use gtk::{gdk, graphene, gsk, prelude::*};
use std::cell::RefCell;

const W: f32 = 800.0;
const H: f32 = 800.0;

/// A small deterministic generator (frames between checks).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    gtk::init()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let backend = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "auto".into());
    let frames = args
        .get(1)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3000);
    let random = flag("--random");
    let resize = flag("--resize");
    let clipped = flag("--clipped");
    let rms = flag("--rms");
    let gpu = flag("--gpu");
    let theme = Theme::by_id(value("--theme").as_deref().unwrap_or("vintage"));
    let mut mixer = if flag("--mixer") {
        let mut session =
            faderframe_session::Session::demo(faderframe_engine::EngineConfig::default())?;
        session.start_audio(
            vec![Box::new(faderframe_audio::dummy::DummyBackend::default())],
            &faderframe_session::AudioPreferences {
                threads: Some(2),
                ..Default::default()
            },
        )?;
        session.dispatch(faderframe_session::Action::Transport(
            faderframe_session::TransportAction::Play,
        ))?;
        Some((
            faderframe_view_mixer::MixerView::new(theme.clone()),
            session,
        ))
    } else {
        None
    };
    if clipped && flag("--present") {
        return Err("--clipped and --present are separate checks".into());
    }
    if clipped && mixer.is_some() {
        return Err("--clipped checks the standalone meter controls; omit --mixer".into());
    }
    let presented = if flag("--present") {
        Some(presentation::Presented::new()?)
    } else {
        None
    };
    let scale: f32 = value("--scale").and_then(|s| s.parse().ok()).unwrap_or(1.0);
    if !scale.is_finite() || scale < 0.25 || scale > 8.0 {
        return Err("--scale must be between 0.25 and 8".into());
    }
    let out = value("--out")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&out)?;
    let display = gdk::Display::default().ok_or("no display")?;
    let host = gtk::Window::new();
    gtk::prelude::WidgetExt::realize(&host);
    let surface = host.surface().ok_or("no surface")?;
    let renderer: gsk::Renderer = match backend.as_str() {
        "gl" => gsk::GLRenderer::new().upcast(),
        "cairo" => gsk::CairoRenderer::new().upcast(),
        _ => gsk::Renderer::for_surface(&surface).ok_or("no renderer")?,
    };
    if !renderer.is_realized() {
        renderer.realize_for_display(&display)?;
    }
    eprintln!(
        "GTK {}.{}.{}, renderer: {}, random {random}, resize {resize}, scale {scale}, gpu {gpu}",
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version(),
        renderer.type_().name()
    );
    let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let text = RefCell::new(TextCache::default());
    let paths = RefCell::new(PathCache::default());
    let (pw, ph) = ((W * scale).ceil(), (H * scale).ceil());
    let viewport = graphene::Rect::new(0.0, 0.0, pw, ph);
    let (pw, ph) = (pw as usize, ph as usize);
    let mut baseline: Option<Vec<u8>> = None;
    let mut rng = Lcg(1);
    let started = std::time::Instant::now();
    let mut last_frame = started;
    for frame in 0..=frames {
        let check = frame % 64 == 0;
        if let Some((_, session)) = &mut mixer {
            let now = std::time::Instant::now();
            session.tick(now.duration_since(last_frame).as_secs_f32().min(0.1));
            last_frame = now;
        }
        text.borrow_mut().begin_frame();
        paths.borrow_mut().begin_frame();
        let snapshot = gtk::Snapshot::new();
        snapshot.push_clip(&viewport);
        snapshot.scale(scale, scale);
        let mut paint = |p: &mut dyn Painter| {
            let mut th = theme.clone();
            p.fill(Rect::new(0.0, 0.0, W, H), th.console.panel_bottom);
            if let Some((view, session)) = &mut mixer {
                use faderframe_ui_canvas::{CanvasView, Size};
                view.paint(p, Size::new(W, H), session, &th);
                return;
            }
            for (row, kind) in [MeterKind::Plasma, MeterKind::Bar, MeterKind::Ladder]
                .iter()
                .enumerate()
            {
                th.console.look.meter = *kind;
                for col in 0..16 {
                    let (level, hold) = if check || !random {
                        let phase = ((frame % 64) as f32 * 0.071 + col as f32).sin();
                        (-35.0 + 36.0 * phase, -6.0)
                    } else {
                        (-70.0 + 72.0 * rng.next(), -40.0 + 40.0 * rng.next())
                    };
                    let mut levels = [
                        controls::MeterLevel::new(level, hold, false),
                        controls::MeterLevel::new(level - 3.0, hold + 3.0, false),
                    ];
                    if rms {
                        for lv in &mut levels {
                            lv.inner_db = Some(lv.level_db - 8.0);
                        }
                    }
                    let mut rect = Rect::new(
                        10.25 + col as f32 * 49.0,
                        10.5 + row as f32 * 160.0,
                        18.0,
                        147.0,
                    );
                    if resize && !check {
                        rect.x += 6.0 * rng.next();
                        rect.y += 6.0 * rng.next();
                        rect.w = 8.0 + 30.0 * rng.next();
                        rect.h = 60.0 + 90.0 * rng.next();
                    }
                    controls::meter(p, rect, &levels, &th);
                }
            }
        };
        if gpu {
            if !faderframe_ui::gpu::paint(widget.upcast_ref(), &snapshot, W, H, scale, &mut paint) {
                return Err("GPU painter unavailable".into());
            }
        } else {
            let mut p = SnapshotPainter::new(&snapshot, widget.upcast_ref(), &text, &paths);
            paint(&mut p);
        }
        snapshot.pop();
        let node = snapshot.to_node().ok_or("empty snapshot")?;
        let texture: Option<gdk::Texture> = if let Some(presented) = &presented {
            presented.draw(&node, pw as i32, ph as i32)?;
            None
        } else {
            Some(renderer.render_texture(&node, Some(&viewport)))
        };
        // Returning every meter to the same geometry can itself trigger
        // a full redraw. Also inspect changing frames, comparing the
        // window's pixels with a fresh render of that exact scene.
        if let Some(presented) = &presented
            && frame % 37 == 17
        {
            let mut actual = presented.pixels(pw, ph)?;
            for pixel in actual.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
            let reference = renderer.render_texture(&node, Some(&viewport));
            let mut expected = vec![0; pw * ph * 4];
            reference.download(&mut expected, pw * 4);
            let differ = actual
                .as_chunks::<4>()
                .0
                .iter()
                .zip(expected.as_chunks::<4>().0)
                .filter(|(a, b)| a.iter().zip(b.iter()).any(|(a, b)| a.abs_diff(*b) > 2))
                .count();
            // Separate on-screen/offscreen rasterization can differ at
            // a handful of antialiased edge pixels. Corrupted tiles or
            // stale meter segments affect a larger region.
            if differ > 16 {
                gdk::MemoryTexture::new(
                    pw as i32,
                    ph as i32,
                    gdk::MemoryFormat::B8g8r8a8,
                    &gtk::glib::Bytes::from_owned(actual),
                    pw * 4,
                )
                .save_to_png(out.join("meter-presented-corrupted.png"))?;
                reference.save_to_png(out.join("meter-presented-reference.png"))?;
                node.write_to_file(out.join("meter-presented-corrupted.node"))?;
                renderer.unrealize();
                host.destroy();
                return Err(format!(
                    "displayed frame {frame} differs from a fresh render ({differ} pixels)"
                )
                .into());
            }
        }
        if check && mixer.is_none() {
            let (pixels, texture) = if let Some(texture) = texture {
                let mut pixels = vec![0; pw * ph * 4];
                texture.download(&mut pixels, pw * 4);
                (pixels, texture)
            } else {
                let mut pixels = presented.as_ref().ok_or("no window")?.pixels(pw, ph)?;
                for pixel in pixels.as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
                let texture = gdk::MemoryTexture::new(
                    pw as i32,
                    ph as i32,
                    gdk::MemoryFormat::B8g8r8a8,
                    &gtk::glib::Bytes::from(&pixels),
                    pw * 4,
                )
                .upcast();
                (pixels, texture)
            };
            if clipped {
                let (x, y, w, h) = (
                    (rng.next() * (pw - 128) as f32) as usize,
                    (rng.next() * (ph - 128) as f32) as usize,
                    128,
                    128,
                );
                let tile = graphene::Rect::new(x as f32, y as f32, w as f32, h as f32);
                let part = renderer.render_texture(&node, Some(&tile));
                let mut part_pixels = vec![0; w * h * 4];
                part.download(&mut part_pixels, w * 4);
                let mut differ = 0;
                for j in 1..h - 1 {
                    for i in 1..w - 1 {
                        let a = &pixels[((y + j) * pw + x + i) * 4..][..4];
                        let b = &part_pixels[(j * w + i) * 4..][..4];
                        if a.iter().zip(b).any(|(a, b)| a.abs_diff(*b) > 2) {
                            differ += 1;
                        }
                    }
                }
                if differ > 0 {
                    part.save_to_png(out.join("meter-clipped.png"))?;
                    texture.save_to_png(out.join("meter-full.png"))?;
                    node.write_to_file(out.join("meter-clipped.node"))?;
                    return Err(format!(
                        "frame {frame}: clipped redraw ({x}, {y}) differs in {differ} pixels"
                    )
                    .into());
                }
            }
            if let Some(ref expected) = baseline {
                if expected
                    .iter()
                    .zip(&pixels)
                    .any(|(a, b)| a.abs_diff(*b) > 2)
                {
                    let differ = expected
                        .chunks(4)
                        .zip(pixels.chunks(4))
                        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(a, b)| a.abs_diff(*b) > 2))
                        .count();
                    texture.save_to_png(out.join("meter-corrupted.png"))?;
                    node.write_to_file(out.join("meter-corrupted.node"))?;
                    renderer.unrealize();
                    host.destroy();
                    return Err(format!(
                        "{backend}: check frame {frame} differs from the first ({differ} pixels)"
                    )
                    .into());
                }
            } else {
                texture.save_to_png(out.join("meter-baseline.png"))?;
                baseline = Some(pixels);
            }
        }
        while gtk::glib::MainContext::default().pending() {
            gtk::glib::MainContext::default().iteration(false);
        }
        if frame > 0 && frame % 2048 == 0 {
            eprintln!("{frame}/{frames} frames checked ({:?})", started.elapsed());
        }
    }
    renderer.unrealize();
    host.destroy();
    println!(
        "{backend}: {frames} frames, rendering checks passed, {:?}",
        started.elapsed()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
mod presentation {
    use super::*;
    use gtk::glib::translate::ToGlibPtr;
    use std::{cell::Cell, rc::Rc, time::Duration};
    use x11rb::{connection::Connection, protocol::xproto::ConnectionExt};

    pub struct Presented {
        window: gtk::Window,
        picture: gtk::Picture,
        painted: Rc<Cell<bool>>,
        conn: x11rb::rust_connection::RustConnection,
    }

    impl Presented {
        pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let display = gdk::Display::default().ok_or("no display")?;
            if display.type_().name() != "GdkX11Display" {
                return Err("--present requires GDK_BACKEND=x11".into());
            }
            let picture = gtk::Picture::new();
            picture.set_can_shrink(false);
            let window = gtk::Window::builder()
                .title("FaderFrame meter presentation probe")
                .child(&picture)
                .decorated(false)
                .resizable(false)
                .build();
            window.present();
            let painted = Rc::new(Cell::new(false));
            let ready = painted.clone();
            window
                .frame_clock()
                .ok_or("no frame clock")?
                .connect_after_paint(move |_| ready.set(true));
            let (conn, _) = x11rb::connect(None)?;
            Ok(Self {
                window,
                picture,
                painted,
                conn,
            })
        }

        pub fn draw(
            &self,
            node: &gsk::RenderNode,
            w: i32,
            h: i32,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let snapshot = gtk::Snapshot::new();
            snapshot.append_node(node);
            self.picture.set_paintable(
                snapshot
                    .to_paintable(Some(&graphene::Size::new(w as f32, h as f32)))
                    .as_ref(),
            );
            self.painted.set(false);
            let started = std::time::Instant::now();
            while !self.painted.get() {
                gtk::glib::MainContext::default().iteration(false);
                if started.elapsed() > Duration::from_secs(10) {
                    return Err("window did not paint within 10 seconds".into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        }

        pub fn pixels(&self, w: usize, h: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            let surface = self.window.surface().ok_or("no surface")?;
            if surface.scale_factor() != 1 {
                return Err(
                    "--present requires GDK_SCALE=1; use --scale for larger geometry".into(),
                );
            }
            // SAFETY: the display was checked to be X11 in new(), and the
            // surface stays alive for this call. GTK exports this X11 API.
            let xid = unsafe {
                let library = libloading::os::unix::Library::this();
                let get_xid = library.get::<unsafe extern "C" fn(
                    *mut gdk::ffi::GdkSurface,
                ) -> std::ffi::c_ulong>(
                    b"gdk_x11_surface_get_xid\0"
                )?;
                get_xid(surface.to_glib_none().0)
            } as u32;
            gtk::prelude::WidgetExt::display(&self.window).sync();
            self.conn.flush()?;
            let image = self
                .conn
                .get_image(
                    x11rb::protocol::xproto::ImageFormat::Z_PIXMAP,
                    xid,
                    0,
                    0,
                    w as u16,
                    h as u16,
                    u32::MAX,
                )?
                .reply()?;
            if image.data.len() != w * h * 4 {
                return Err("expected a 32-bit X11 window image".into());
            }
            Ok(image.data)
        }
    }

    impl Drop for Presented {
        fn drop(&mut self) {
            self.window.destroy();
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod presentation {
    use super::*;
    pub struct Presented;
    impl Presented {
        pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
            Err("--present is supported on Linux/X11".into())
        }
        pub fn draw(
            &self,
            _: &gsk::RenderNode,
            _: i32,
            _: i32,
        ) -> Result<(), Box<dyn std::error::Error>> {
            unreachable!()
        }
        pub fn pixels(&self, _: usize, _: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            unreachable!()
        }
    }
}
