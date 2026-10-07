//! Frames rendered on the GPU: shapes land where the painter put them,
//! clips clip, text draws, HiDPI scales. Skipped without a GPU adapter.
#![allow(clippy::unwrap_used)]

use faderframe_ui_canvas::{Align, Color, Paint, Rect, TextStyle};
use faderframe_ui_gpu::{Frame, GpuRenderer};

fn renderer() -> Option<GpuRenderer> {
    match GpuRenderer::new() {
        Ok(r) => {
            eprintln!("rendering on {}", r.adapter());
            Some(r)
        }
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

fn px(f: &Frame, x: u32, y: u32) -> [u8; 4] {
    let i = (y as usize * f.width as usize + x as usize) * 4;
    let p = f.pixels.as_ref();
    [p[i], p[i + 1], p[i + 2], p[i + 3]]
}

#[test]
fn shapes_land_where_painted_with_straight_alpha() {
    let Some(mut r) = renderer() else { return };
    let f = r
        .render(64, 64, 1.0, |p| {
            p.fill(Rect::new(10.0, 10.0, 20.0, 20.0), Color::rgb(1.0, 0.0, 0.0));
            p.fill(
                Rect::new(40.0, 40.0, 10.0, 10.0),
                Color::rgba(0.0, 0.0, 1.0, 0.5),
            );
        })
        .unwrap();
    assert_eq!(
        (f.width, f.height, f.pixels.as_ref().len()),
        (64, 64, 64 * 64 * 4)
    );
    assert_eq!(px(&f, 20, 20), [255, 0, 0, 255]);
    assert_eq!(px(&f, 5, 5), [0, 0, 0, 0]);
    let half = px(&f, 45, 45);
    assert!((126..=129).contains(&half[3]), "{half:?}");
    assert_eq!(half[2], 255, "straight alpha: {half:?}");
}

#[test]
fn clips_and_transforms_apply_and_unbalanced_clips_do_not_leak() {
    let Some(mut r) = renderer() else { return };
    for _ in 0..2 {
        let f = r
            .render(40, 40, 1.0, |p| {
                p.push_clip(Rect::new(0.0, 0.0, 20.0, 40.0));
                p.fill(Rect::new(0.0, 0.0, 40.0, 40.0), Color::rgb(0.0, 1.0, 0.0));
                p.pop_clip();
                p.push_transform(30.0, 30.0, 2.0);
                p.fill(Rect::new(0.0, 0.0, 3.0, 3.0), Color::rgb(1.0, 1.0, 1.0));
                p.pop_transform();
                // Left open on purpose.
                p.push_clip(Rect::new(0.0, 0.0, 1.0, 1.0));
            })
            .unwrap();
        assert_eq!(px(&f, 10, 10)[1], 255);
        assert_eq!(px(&f, 30, 10), [0, 0, 0, 0]);
        assert_eq!(px(&f, 34, 34), [255, 255, 255, 255]);
        assert_eq!(px(&f, 37, 37), [0, 0, 0, 0]);
    }
}

#[test]
fn text_is_drawn_measured_and_ellipsised() {
    let Some(mut r) = renderer() else { return };
    let style = TextStyle {
        align: Align::Start,
        ..TextStyle::new(14.0, Color::rgb(1.0, 1.0, 1.0))
    };
    let mut width = 0.0;
    let mut narrow = 0.0;
    let f = r
        .render(200, 40, 1.0, |p| {
            width = p.text_width("FaderFrame", &style);
            narrow = p.text_width("i", &style);
            p.text("FaderFrame", Rect::new(0.0, 0.0, 200.0, 40.0), &style);
        })
        .unwrap();
    assert!(width > 50.0 && width < 120.0, "width {width}");
    assert!(narrow < width / 4.0);
    let lit = |f: &Frame, x0: u32, x1: u32| {
        (x0..x1)
            .flat_map(|x| (0..f.height).map(move |y| (x, y)))
            .filter(|&(x, y)| px(f, x, y)[3] > 64)
            .count()
    };
    assert!(lit(&f, 0, width as u32) > 50, "glyphs drawn");
    assert_eq!(lit(&f, width as u32 + 4, 200), 0, "nothing after the text");
    // Too narrow: ellipsised to what fits.
    let f = r
        .render(200, 40, 1.0, |p| {
            p.text("FaderFrame Studio", Rect::new(0.0, 0.0, 60.0, 40.0), &style);
        })
        .unwrap();
    assert_eq!(lit(&f, 62, 200), 0, "kept within the rectangle");
    assert!(lit(&f, 0, 60) > 30);
}

#[test]
fn hidpi_frames_have_the_device_pixels() {
    let Some(mut r) = renderer() else { return };
    let f = r
        .render(64, 64, 2.0, |p| {
            p.fill(Rect::new(0.0, 0.0, 16.0, 16.0), Color::rgb(1.0, 0.0, 0.0));
            p.fill_rect(
                Rect::new(16.0, 16.0, 16.0, 16.0),
                &Paint::Solid(Color::rgb(0.0, 0.0, 1.0)),
            );
        })
        .unwrap();
    assert_eq!(px(&f, 30, 30), [255, 0, 0, 255]);
    assert_eq!(px(&f, 34, 34), [0, 0, 255, 255]);
    assert_eq!(px(&f, 63, 63), [0, 0, 255, 255]);
    assert_eq!(px(&f, 63, 0), [0, 0, 0, 0]);
}

/// A frame of a tall filmstrip (taller than vello's image atlas) lands on
/// its destination.
#[test]
fn a_filmstrip_frame_is_drawn() {
    let Some(mut r) = renderer() else { return };
    // 4 × 16384 pixels: frame k (4 × 4) is grey level k.
    let (w, h) = (4u32, 16_384u32);
    let mut raw = Vec::new();
    {
        let mut e = png::Encoder::new(&mut raw, w, h);
        e.set_color(png::ColorType::Rgba);
        let mut wr = e.write_header().unwrap();
        let data: Vec<u8> = (0..h)
            .flat_map(|y| {
                let g = ((y / 4) % 256) as u8;
                (0..w).flat_map(move |_| [g, g, g, 255])
            })
            .collect();
        wr.write_image_data(&data).unwrap();
    }
    let png: &'static [u8] = Box::leak(raw.into_boxed_slice());
    let image = faderframe_ui_canvas::Image {
        key: "test/filmstrip",
        png,
        width: w,
        height: h,
    };
    let f = r
        .render(32, 32, 1.0, |p| {
            // Frame 200 (grey 200), drawn 16 × 16 at (8, 8).
            p.image(
                &image,
                Rect::new(0.0, 800.0, 4.0, 4.0),
                Rect::new(8.0, 8.0, 16.0, 16.0),
                1.0,
            );
        })
        .unwrap();
    let c = px(&f, 16, 16);
    assert!((195..=205).contains(&c[0]) && c[3] == 255, "{c:?}");
    assert_eq!(px(&f, 2, 2), [0, 0, 0, 0]);
}

/// On Linux a frame can go out as a dmabuf: the same pixels as read back,
/// in linear rows padded to 256 bytes, in a real dma-buf; its buffer is
/// reused only once the toolkit lets go of it.
#[cfg(target_os = "linux")]
#[test]
fn frames_go_out_as_dmabufs() {
    use faderframe_ui_gpu::Output;
    let Some(mut r) = renderer() else { return };
    if !r.exports_dmabufs() {
        eprintln!("skipped: no dmabuf export on {}", r.adapter());
        return;
    }
    let paint = |p: &mut dyn faderframe_ui_canvas::Painter| {
        p.fill(Rect::new(0.0, 0.0, 30.0, 70.0), Color::rgb(1.0, 0.5, 0.0));
        p.fill(
            Rect::new(30.0, 20.0, 40.0, 10.0),
            Color::rgba(0.0, 0.0, 1.0, 0.5),
        );
    };
    let pixels = r.render(70, 70, 1.0, paint).unwrap();
    let Output::Dmabuf(frame) = r.render_to(70, 70, 1.0, true, paint).unwrap() else {
        panic!("no dmabuf");
    };
    assert_eq!((frame.width, frame.height), (70, 70));
    assert_eq!(frame.stride, 512, "rows padded to 256 bytes");
    assert_eq!(frame.fourcc, faderframe_ui_gpu::FOURCC_AB24);
    let link = std::fs::read_link(format!("/proc/self/fd/{}", frame.fd)).unwrap();
    assert!(link.to_string_lossy().contains("dmabuf"), "{link:?}");
    let rows = r.read_dmabuf(&frame).unwrap();
    for y in 0..70usize {
        let a = &rows[y * 512..y * 512 + 280];
        let b = &pixels.pixels.as_ref()[y * 280..(y + 1) * 280];
        assert_eq!(a, b, "row {y}");
    }
    // Shown: the next frames take other buffers, at most three at once.
    let second = r.render_to(70, 70, 1.0, true, paint).unwrap();
    let third = r.render_to(70, 70, 1.0, true, paint).unwrap();
    let Output::Dmabuf(second) = second else {
        panic!()
    };
    assert_ne!(second.fd, frame.fd);
    assert!(matches!(third, Output::Dmabuf(_)));
    assert!(
        matches!(
            r.render_to(70, 70, 1.0, true, paint).unwrap(),
            Output::Pixels(_)
        ),
        "every buffer shown: read back"
    );
    // Released, a buffer is used again.
    let fd = frame.fd;
    drop(frame);
    let Output::Dmabuf(again) = r.render_to(70, 70, 1.0, true, paint).unwrap() else {
        panic!("no dmabuf after a release");
    };
    assert_eq!(again.fd, fd);
    drop((second, third, again));
}
