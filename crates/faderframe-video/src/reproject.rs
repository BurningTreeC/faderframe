//! A flat view into a 360° frame, drawn on the CPU: each pixel of the
//! view takes the frame's colour in the direction it looks through
//! (bilinear between the frame's pixels).
//!
//! Where each view pixel looks in the frame is worked out exactly on a
//! grid every [`GRID`] pixels and interpolated between: a gnomonic view
//! maps smoothly onto an equirectangular picture or a cube face away
//! from the poles, and the trigonometry is then a sixtieth of the work.
//! Cells near a pole, across the picture's seam, between two cube faces
//! or reaching past a crop are worked out pixel by pixel.

use faderframe_core::spherical::{Mapping, Projection, Viewport};

/// Pixels between the points worked out exactly.
pub const GRID: usize = 8;

/// Views larger than this are drawn by several threads.
const PARALLEL_FROM: usize = 200_000;

/// Where a grid point looks in the frame.
#[derive(Clone, Copy, Default)]
struct Point {
    x: f32,
    y: f32,
    region: u8,
    /// In the frame (not past a crop).
    ok: bool,
    /// Near a pole (equirectangular): directions change too fast there to
    /// interpolate.
    pole: bool,
}

/// A frame to look into: RGBA rows, `width × 4` bytes each.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// The view's camera: rays through its pixels into the scene.
struct Camera {
    look: [[f32; 3]; 3],
    focal: f32,
    half: (f32, f32),
}

impl Camera {
    fn new(view: &Viewport) -> Self {
        Self {
            look: view.look.matrix(),
            focal: view.focal(),
            half: (view.width * 0.5, view.height * 0.5),
        }
    }

    /// The scene direction through view point (`x`, `y`) (not unit).
    fn ray(&self, x: f32, y: f32) -> [f32; 3] {
        let c = [self.focal, self.half.0 - x, self.half.1 - y];
        let m = &self.look;
        std::array::from_fn(|r| m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2])
    }
}

/// The frame's pixels, sampled between them.
struct Sampler<'a> {
    rgba: &'a [u8],
    stride: usize,
    /// The shown eye (whole pixels): x, y, width, height.
    eye: (i32, i32, i32, i32),
    wraps: bool,
}

impl Sampler<'_> {
    /// The colour at frame point (`x`, `y`) (pixel middles at n + 0.5).
    #[inline(always)]
    fn sample(&self, x: f32, y: f32) -> [u8; 4] {
        let (ex, ey, ew, eh) = self.eye;
        let fx = x - 0.5;
        let fy = y - 0.5;
        let (x0, y0) = (floor(fx), floor(fy));
        let tx = ((fx - x0 as f32) * 256.0) as u32;
        let ty = ((fy - y0 as f32) * 256.0) as u32;
        let (c0, c1) = if x0 >= ex && x0 + 1 < ex + ew {
            (x0, x0 + 1)
        } else if self.wraps {
            (
                ex + (x0 - ex).rem_euclid(ew),
                ex + (x0 + 1 - ex).rem_euclid(ew),
            )
        } else {
            (x0.clamp(ex, ex + ew - 1), (x0 + 1).clamp(ex, ex + ew - 1))
        };
        let r0 = y0.clamp(ey, ey + eh - 1) as usize * self.stride;
        let r1 = (y0 + 1).clamp(ey, ey + eh - 1) as usize * self.stride;
        let (c0, c1) = (c0 as usize * 4, c1 as usize * 4);
        let top = lerp(self.texel(r0 + c0), self.texel(r0 + c1), tx);
        let bottom = lerp(self.texel(r1 + c0), self.texel(r1 + c1), tx);
        (lerp(top, bottom, ty) | 0xff00_0000).to_le_bytes()
    }

    #[inline]
    fn texel(&self, at: usize) -> u32 {
        match self.rgba.get(at..at + 4) {
            Some(&[r, g, b, a]) => u32::from_le_bytes([r, g, b, a]),
            _ => 0,
        }
    }
}

/// The whole number at or below `v` (a call to `floorf` without SSE 4.1
/// costs more than the rest of a sample).
#[inline]
fn floor(v: f32) -> i32 {
    let t = v as i32;
    t - (v < t as f32) as i32
}

/// `a` to `b` by `t`/256, two channels at a time.
#[inline]
fn lerp(a: u32, b: u32, t: u32) -> u32 {
    let u = 256 - t;
    let rb = (((a & 0x00ff_00ff) * u + (b & 0x00ff_00ff) * t) >> 8) & 0x00ff_00ff;
    let ga = (((a >> 8) & 0x00ff_00ff) * u + ((b >> 8) & 0x00ff_00ff) * t) & 0xff00_ff00;
    rb | ga
}

/// Draw `view` of `frame` (laid out as `mapping`, which must be for the
/// frame's size) into `out`: RGBA rows of the view's size (whole
/// pixels), resized as needed. Directions the frame does not hold (past a
/// crop) are black.
pub fn reproject(frame: Source<'_>, mapping: &Mapping, view: &Viewport, out: &mut Vec<u8>) {
    let (w, h) = (view.width as usize, view.height as usize);
    out.resize(w * h * 4, 0);
    if w == 0 || h == 0 {
        return;
    }
    let need = frame.width as usize * frame.height as usize * 4;
    if frame.rgba.len() < need || frame.width == 0 || frame.height == 0 {
        out.fill(0);
        return;
    }
    let (ex, ey, ew, eh) = mapping.eye();
    let sampler = Sampler {
        rgba: frame.rgba,
        stride: frame.width as usize * 4,
        eye: (
            ex.round() as i32,
            ey.round() as i32,
            (ew.round() as i32).max(1),
            (eh.round() as i32).max(1),
        ),
        wraps: mapping.wraps(),
    };
    let camera = Camera::new(view);
    let equirect = matches!(
        mapping.spherical().projection,
        Projection::Equirectangular { .. }
    );
    // The grid: points at pixel middles every GRID pixels, one past the
    // last pixel.
    let cols = (w - 1) / GRID + 2;
    let rows = (h - 1) / GRID + 2;
    let near = eh * 0.04;
    let mut grid = vec![Point::default(); cols * rows];
    for gy in 0..rows {
        for gx in 0..cols {
            let (px, py) = ((gx * GRID) as f32 + 0.5, (gy * GRID) as f32 + 0.5);
            grid[gy * cols + gx] = match mapping.pixel_of(camera.ray(px, py)) {
                Some((x, y, region)) => Point {
                    x,
                    y,
                    region,
                    ok: true,
                    pole: equirect && (y - ey < near || ey + eh - y < near),
                },
                None => Point::default(),
            };
        }
    }
    let job = Job {
        grid: &grid,
        cols,
        width: w,
        sampler: &sampler,
        camera: &camera,
        mapping,
        seam: ew * 0.25,
    };
    let threads = threads_for(w * h);
    if threads == 1 {
        job.rows(0, h, out);
        return;
    }
    let band_rows = h.div_ceil(threads);
    std::thread::scope(|scope| {
        let mut rest: &mut [u8] = out;
        let mut y0 = 0;
        while y0 < h {
            let n = band_rows.min(h - y0);
            let (band, tail) = rest.split_at_mut(n * w * 4);
            rest = tail;
            let job = &job;
            scope.spawn(move || job.rows(y0, y0 + n, band));
            y0 += n;
        }
    });
}

/// Threads to draw `pixels` with.
fn threads_for(pixels: usize) -> usize {
    if pixels < PARALLEL_FROM {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .clamp(1, 4)
    }
}

struct Job<'a> {
    grid: &'a [Point],
    cols: usize,
    width: usize,
    sampler: &'a Sampler<'a>,
    camera: &'a Camera,
    mapping: &'a Mapping,
    /// Corners further apart across than this straddle the seam.
    seam: f32,
}

impl Job<'_> {
    /// View rows `y0..y1` into `out` (those rows only).
    fn rows(&self, y0: usize, y1: usize, out: &mut [u8]) {
        let w = self.width;
        for y in y0..y1 {
            let gy = y / GRID;
            let ty = (y % GRID) as f32 / GRID as f32;
            let line = &mut out[(y - y0) * w * 4..(y - y0 + 1) * w * 4];
            let mut gx = 0;
            while gx * GRID < w {
                let x_from = gx * GRID;
                let x_to = (x_from + GRID).min(w);
                let c = |dx: usize, dy: usize| self.grid[(gy + dy) * self.cols + gx + dx];
                let (a, b, cc, d) = (c(0, 0), c(1, 0), c(0, 1), c(1, 1));
                let smooth = a.ok
                    && b.ok
                    && cc.ok
                    && d.ok
                    && !(a.pole || b.pole || cc.pole || d.pole)
                    && a.region == b.region
                    && a.region == cc.region
                    && a.region == d.region;
                let corners = smooth.then(|| self.unwrapped([a, b, cc, d])).flatten();
                match corners {
                    Some([a, b, cc, d]) => {
                        // Left and right edges at this row, then across.
                        let lx = a.0 + (cc.0 - a.0) * ty;
                        let ly = a.1 + (cc.1 - a.1) * ty;
                        let rx = b.0 + (d.0 - b.0) * ty;
                        let ry = b.1 + (d.1 - b.1) * ty;
                        let step = 1.0 / GRID as f32;
                        let (dx, dy) = ((rx - lx) * step, (ry - ly) * step);
                        let (mut fx, mut fy) = (lx, ly);
                        for x in x_from..x_to {
                            let p = self.sampler.sample(fx, fy);
                            line[x * 4..x * 4 + 4].copy_from_slice(&p);
                            fx += dx;
                            fy += dy;
                        }
                    }
                    None => {
                        for x in x_from..x_to {
                            let ray = self.camera.ray(x as f32 + 0.5, y as f32 + 0.5);
                            let p = match self.mapping.pixel_of(ray) {
                                Some((fx, fy, _)) => self.sampler.sample(fx, fy),
                                None => [0, 0, 0, 255],
                            };
                            line[x * 4..x * 4 + 4].copy_from_slice(&p);
                        }
                    }
                }
                gx += 1;
            }
        }
    }

    /// A cell's corners with x taken round the picture to lie together
    /// (`None`: they still straddle the seam).
    fn unwrapped(&self, c: [Point; 4]) -> Option<[(f32, f32); 4]> {
        let mut out = c.map(|p| (p.x, p.y));
        if self.sampler.wraps {
            let ew = self.sampler.eye.2 as f32;
            let x0 = out[0].0;
            for p in &mut out[1..] {
                p.0 += ew * ((x0 - p.0) / ew).round();
            }
        }
        let (lo, hi) = out.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p.0), hi.max(p.0))
        });
        (hi - lo < self.seam).then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::ambisonics::Orientation;
    use faderframe_core::spherical::{Spherical, StereoMode};

    /// An equirectangular picture coloured by direction: red ahead, green
    /// to the left, blue to the right, yellow behind; white above 60°,
    /// grey below −60°.
    fn compass(w: u32, h: u32) -> Vec<u8> {
        let m = Mapping::new(Spherical::EQUIRECTANGULAR, w, h);
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let d = m.direction_at(x as f32 + 0.5, y as f32 + 0.5).unwrap();
                out.extend_from_slice(&colour(d));
            }
        }
        out
    }

    fn colour(d: [f32; 3]) -> [u8; 4] {
        let (az, el) = faderframe_core::ambisonics::angles(d);
        if el > 60.0 {
            [255, 255, 255, 255]
        } else if el < -60.0 {
            [128, 128, 128, 255]
        } else if az.abs() <= 45.0 {
            [255, 0, 0, 255]
        } else if az > 45.0 && az <= 135.0 {
            [0, 255, 0, 255]
        } else if (-135.0..-45.0).contains(&az) {
            [0, 0, 255, 255]
        } else {
            [255, 255, 0, 255]
        }
    }

    fn at(out: &[u8], w: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * w + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    }

    /// The view's middle shows what it looks at, whichever way it turns;
    /// a look up shows the sky; the seam behind leaves no line.
    #[test]
    fn a_view_shows_the_picture_where_it_looks() {
        let (w, h) = (720u32, 360u32);
        let frame = compass(w, h);
        let src = Source {
            rgba: &frame,
            width: w,
            height: h,
        };
        let m = Mapping::new(Spherical::EQUIRECTANGULAR, w, h);
        let mut out = Vec::new();
        for (yaw, pitch, want) in [
            (0.0, 0.0, [255, 0, 0, 255]),
            (90.0, 0.0, [0, 255, 0, 255]),
            (-90.0, 0.0, [0, 0, 255, 255]),
            (180.0, 0.0, [255, 255, 0, 255]),
            (0.0, 80.0, [255, 255, 255, 255]),
            (30.0, -85.0, [128, 128, 128, 255]),
        ] {
            let view = Viewport::new(Orientation::from_ypr(yaw, pitch, 0.0), 60.0, 320.0, 180.0);
            reproject(src, &m, &view, &mut out);
            assert_eq!(out.len(), 320 * 180 * 4);
            assert_eq!(at(&out, 320, 160, 90), want, "{yaw} {pitch}");
        }
        // Behind, the seam in the middle of the view: yellow throughout
        // the middle row (no line of another colour).
        let view = Viewport::new(Orientation::from_ypr(180.0, 0.0, 0.0), 40.0, 320.0, 180.0);
        reproject(src, &m, &view, &mut out);
        for x in 0..320 {
            assert_eq!(at(&out, 320, x, 90), [255, 255, 0, 255], "x {x}");
        }
    }

    /// Every view pixel is within a frame pixel of where the exact
    /// direction says, near the poles and across the seam too.
    #[test]
    fn interpolation_stays_on_the_exact_directions() {
        let (w, h) = (1440u32, 720u32);
        // A frame whose colours say their own coordinates.
        let mut frame = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                frame.extend_from_slice(&[(x % 256) as u8, (x / 256) as u8, (y % 256) as u8, 255]);
            }
        }
        let src = Source {
            rgba: &frame,
            width: w,
            height: h,
        };
        let m = Mapping::new(Spherical::EQUIRECTANGULAR, w, h);
        let mut out = Vec::new();
        for (yaw, pitch, fov) in [(10.0, 5.0, 90.0), (175.0, 0.0, 70.0), (0.0, 75.0, 100.0)] {
            let view = Viewport::new(Orientation::from_ypr(yaw, pitch, 0.0), fov, 400.0, 300.0);
            reproject(src, &m, &view, &mut out);
            let mut worst = 0.0f32;
            for y in (0..300).step_by(7) {
                for x in (0..400).step_by(5) {
                    let d = view.direction_at(x as f32 + 0.5, y as f32 + 0.5);
                    let (_, fy, _) = m.pixel_of(d).unwrap();
                    let p = at(&out, 400, x, y);
                    // Away from the pixel grid's own bilinear blend:
                    // compare the row (blue) only where the column is
                    // not near a wrap of the low byte.
                    let row = p[2] as f32;
                    let want = (fy - 0.5).clamp(0.0, h as f32 - 1.0) % 256.0;
                    if want > 2.0 && want < 253.0 {
                        worst = worst.max((row - want).abs());
                    }
                }
            }
            assert!(worst < 1.5, "{yaw} {pitch}: {worst}");
        }
    }

    /// A cubemap face shows its colour; stereo shows the left eye.
    #[test]
    fn cubemaps_and_stereo_frames_are_viewed() {
        // A 3×2 cubemap of flat faces, each its own colour.
        let (w, h) = (384u32, 256u32);
        let cube = Spherical::CUBEMAP;
        let m = Mapping::new(cube, w, h);
        let mut frame = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let d = m.direction_at(x as f32 + 0.5, y as f32 + 0.5).unwrap();
                frame.extend_from_slice(&colour(d));
            }
        }
        let src = Source {
            rgba: &frame,
            width: w,
            height: h,
        };
        let mut out = Vec::new();
        for (yaw, pitch, want) in [
            (0.0, 0.0, [255, 0, 0, 255]),
            (90.0, 0.0, [0, 255, 0, 255]),
            (0.0, 89.0, [255, 255, 255, 255]),
            (200.0, 10.0, [255, 255, 0, 255]),
        ] {
            let view = Viewport::new(Orientation::from_ypr(yaw, pitch, 0.0), 60.0, 200.0, 120.0);
            reproject(src, &m, &view, &mut out);
            assert_eq!(at(&out, 200, 100, 60), want, "{yaw} {pitch}");
        }
        // Top-bottom stereo: the top half is the left eye.
        let (w, h) = (720u32, 720u32);
        let mut frame = compass(w, h / 2);
        frame.extend(std::iter::repeat_n(0u8, (w * h / 2 * 4) as usize));
        let s = Spherical {
            stereo: StereoMode::TopBottom,
            ..Spherical::EQUIRECTANGULAR
        };
        let m = Mapping::new(s, w, h);
        let view = Viewport::new(Orientation::from_ypr(-90.0, 0.0, 0.0), 60.0, 100.0, 100.0);
        reproject(
            Source {
                rgba: &frame,
                width: w,
                height: h,
            },
            &m,
            &view,
            &mut out,
        );
        assert_eq!(at(&out, 100, 50, 50), [0, 0, 255, 255]);
        // Looking at the pole of the left eye never reads the right eye.
        let view = Viewport::new(Orientation::from_ypr(0.0, -89.0, 0.0), 90.0, 100.0, 100.0);
        reproject(
            Source {
                rgba: &frame,
                width: w,
                height: h,
            },
            &m,
            &view,
            &mut out,
        );
        assert_eq!(at(&out, 100, 50, 50), [128, 128, 128, 255]);
    }

    /// Large views are drawn in bands on several threads, the same as one.
    #[test]
    fn bands_draw_what_one_thread_draws() {
        let (w, h) = (1024u32, 512u32);
        let frame = compass(w, h);
        let src = Source {
            rgba: &frame,
            width: w,
            height: h,
        };
        let m = Mapping::new(Spherical::EQUIRECTANGULAR, w, h);
        let view = Viewport::new(
            Orientation::from_ypr(150.0, 30.0, 10.0),
            100.0,
            800.0,
            450.0,
        );
        let mut big = Vec::new();
        reproject(src, &m, &view, &mut big);
        // The same rows drawn alone.
        let camera = Camera::new(&view);
        let mut grid_out = vec![0u8; 800 * 450 * 4];
        let (ex, ey, ew, eh) = m.eye();
        let sampler = Sampler {
            rgba: &frame,
            stride: w as usize * 4,
            eye: (ex as i32, ey as i32, ew as i32, eh as i32),
            wraps: true,
        };
        let cols = (800 - 1) / GRID + 2;
        let rows = (450 - 1) / GRID + 2;
        let mut grid = vec![Point::default(); cols * rows];
        for gy in 0..rows {
            for gx in 0..cols {
                let r = camera.ray((gx * GRID) as f32 + 0.5, (gy * GRID) as f32 + 0.5);
                let (x, y, region) = m.pixel_of(r).unwrap();
                grid[gy * cols + gx] = Point {
                    x,
                    y,
                    region,
                    ok: true,
                    pole: y - ey < eh * 0.04 || ey + eh - y < eh * 0.04,
                };
            }
        }
        Job {
            grid: &grid,
            cols,
            width: 800,
            sampler: &sampler,
            camera: &camera,
            mapping: &m,
            seam: ew * 0.25,
        }
        .rows(0, 450, &mut grid_out);
        assert!(big == grid_out);
    }

    /// What a frame costs: a 1280×720 view (and 1920×1080) of a 4K
    /// equirectangular frame, one thread and as drawn.
    #[test]
    #[ignore = "a measurement: cargo test -p faderframe-video --lib reprojection_cost -- --ignored --nocapture"]
    fn reprojection_cost() {
        let (w, h) = (3840u32, 1920u32);
        let frame = compass(w, h);
        let src = Source {
            rgba: &frame,
            width: w,
            height: h,
        };
        let m = Mapping::new(Spherical::EQUIRECTANGULAR, w, h);
        let mut out = Vec::new();
        for (vw, vh) in [(1280.0, 720.0), (1920.0, 1080.0)] {
            let view = Viewport::new(Orientation::from_ypr(20.0, 10.0, 0.0), 90.0, vw, vh);
            reproject(src, &m, &view, &mut out);
            let n = 30;
            let t = std::time::Instant::now();
            for i in 0..n {
                let view = Viewport::new(Orientation::from_ypr(i as f32, 10.0, 0.0), 90.0, vw, vh);
                reproject(src, &m, &view, &mut out);
            }
            let each = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
            println!(
                "{vw}×{vh}: {each:.2} ms a frame ({} threads)",
                threads_for((vw * vh) as usize)
            );
        }
    }
}
