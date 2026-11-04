//! 360° pictures: how a spherical video's frame holds the directions round
//! the viewer, and a flat view into it.
//!
//! Directions are those of the ambisonic frame ([`crate::ambisonics`]):
//! `x` front, `y` left, `z` up; azimuth positive to the left, elevation
//! up. A spherical picture's front (its projection's middle) is the
//! scene's front, so a sound panned to a direction is where the picture
//! shows it.
//!
//! * **Equirectangular**: longitude across (the front in the middle,
//!   right towards the right of the frame), latitude down; the frame may
//!   be a crop of the whole projection ([`Crop`], shares cut from each
//!   edge, as Spherical Video V2's `equi` box says them).
//! * **Cubemap** (Spherical Video V2 layout 0): six faces in a 3×2 grid —
//!   right, left, up over down, front, back — the four round the viewer
//!   upright, the up face with its top forwards and the down face with
//!   its top to the back, `padding` inside each face's edges.
//! * **Pose**: the projection turned in the scene (yaw left, pitch up,
//!   roll to the right, composed as an [`Orientation`]'s): a direction of
//!   the projection is shown at `pose · d`.
//! * **Stereo**: a frame holding two eyes shows the left one.
//!
//! [`Mapping`] turns directions into a frame's pixels and back (made once
//! per frame size); [`Viewport`] is a flat (gnomonic) view with a
//! horizontal field of view, looking where an [`Orientation`] points —
//! the listener's head, so picture and sound turn together.

use crate::ambisonics::Orientation;
use serde::{Deserialize, Serialize};

/// What part of the whole equirectangular projection the frame leaves out
/// (shares of the whole, cut from each edge).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Crop {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

impl Crop {
    /// Whether the frame is the whole projection.
    pub fn is_none(&self) -> bool {
        *self == Crop::default()
    }

    /// Sane shares: each 0…1, the two of an axis leaving some of it.
    pub fn clamped(self) -> Crop {
        let c = |v: f64| {
            if v.is_finite() {
                v.clamp(0.0, 0.99)
            } else {
                0.0
            }
        };
        let (mut top, mut bottom, mut left, mut right) =
            (c(self.top), c(self.bottom), c(self.left), c(self.right));
        if top + bottom > 0.99 {
            top = 0.0;
            bottom = 0.0;
        }
        if left + right > 0.99 {
            left = 0.0;
            right = 0.0;
        }
        Crop {
            top,
            bottom,
            left,
            right,
        }
    }
}

/// How the sphere is laid out in the frame.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Projection {
    Equirectangular {
        #[serde(default, skip_serializing_if = "Crop::is_none")]
        crop: Crop,
    },
    /// Spherical Video V2 cubemap layout 0; `padding` is the share of a
    /// face's width left at each of its edges.
    Cubemap {
        #[serde(default)]
        padding: f32,
    },
}

/// How a frame holds the eyes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StereoMode {
    #[default]
    Mono,
    /// Left eye on top.
    TopBottom,
    /// Left eye on the left.
    LeftRight,
    /// Right eye on the left.
    RightLeft,
}

impl StereoMode {
    /// The left eye's part of the frame (shares: x, y, width, height).
    pub fn left_eye(self) -> (f32, f32, f32, f32) {
        match self {
            StereoMode::Mono => (0.0, 0.0, 1.0, 1.0),
            StereoMode::TopBottom => (0.0, 0.0, 1.0, 0.5),
            StereoMode::LeftRight => (0.0, 0.0, 0.5, 1.0),
            StereoMode::RightLeft => (0.5, 0.0, 0.5, 1.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StereoMode::Mono => "Monoscopic",
            StereoMode::TopBottom => "Stereo Top-Bottom",
            StereoMode::LeftRight => "Stereo Left-Right",
            StereoMode::RightLeft => "Stereo Right-Left",
        }
    }
}

/// The projection's turn in the scene (degrees, as [`Orientation::from_ypr`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pose {
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
}

impl Pose {
    pub fn is_none(&self) -> bool {
        *self == Pose::default()
    }

    pub fn orientation(&self) -> Orientation {
        Orientation::from_ypr(self.yaw, self.pitch, self.roll)
    }
}

/// A 360° picture's layout.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spherical {
    pub projection: Projection,
    #[serde(default, skip_serializing_if = "is_mono")]
    pub stereo: StereoMode,
    #[serde(default, skip_serializing_if = "Pose::is_none")]
    pub pose: Pose,
}

fn is_mono(s: &StereoMode) -> bool {
    *s == StereoMode::Mono
}

impl Spherical {
    /// A whole, monoscopic equirectangular picture.
    pub const EQUIRECTANGULAR: Spherical = Spherical {
        projection: Projection::Equirectangular {
            crop: Crop {
                top: 0.0,
                bottom: 0.0,
                left: 0.0,
                right: 0.0,
            },
        },
        stereo: StereoMode::Mono,
        pose: Pose {
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
        },
    };

    /// A monoscopic cubemap without padding.
    pub const CUBEMAP: Spherical = Spherical {
        projection: Projection::Cubemap { padding: 0.0 },
        stereo: StereoMode::Mono,
        pose: Pose {
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
        },
    };

    pub fn is_cubemap(&self) -> bool {
        matches!(self.projection, Projection::Cubemap { .. })
    }

    /// "360° equirectangular", "360° cubemap, stereo top-bottom" …
    pub fn label(&self) -> String {
        let p = match self.projection {
            Projection::Equirectangular { crop } if !crop.is_none() => "equirectangular, cropped",
            Projection::Equirectangular { .. } => "equirectangular",
            Projection::Cubemap { .. } => "cubemap",
        };
        match self.stereo {
            StereoMode::Mono => format!("360° {p}"),
            s => format!("360° {p}, {}", s.label().to_lowercase()),
        }
    }

    /// The frame width (pixels) that shows a view `view_width` pixels wide
    /// with a field of view of `fov` degrees at its own sharpness.
    pub fn width_for(&self, view_width: f32, fov: f32) -> f32 {
        let fov = fov.clamp(MIN_FOV, MAX_FOV);
        let (_, _, ew, _) = self.stereo.left_eye();
        let eye = match self.projection {
            Projection::Equirectangular { crop } => {
                view_width * 360.0 / fov * (1.0 - crop.left - crop.right) as f32
            }
            // A face is 90° wide.
            Projection::Cubemap { padding } => {
                3.0 * view_width * 90.0 / fov / (1.0 - 2.0 * padding.clamp(0.0, 0.45))
            }
        };
        eye / ew
    }
}

/// The narrowest and widest field of view a viewport takes (degrees).
pub const MIN_FOV: f32 = 20.0;
pub const MAX_FOV: f32 = 150.0;
/// The field of view a 360° picture opens with.
pub const DEFAULT_FOV: f32 = 90.0;

/// A cube face: its middle, its right and its up (the ambisonic frame),
/// and its cell in the 3×2 grid (column, row).
struct Face {
    centre: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    cell: (u8, u8),
}

/// Layout 0's faces: right, left, up over down, front, back.
const FACES: [Face; 6] = [
    // Front.
    Face {
        centre: [1.0, 0.0, 0.0],
        right: [0.0, -1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        cell: (1, 1),
    },
    // Back.
    Face {
        centre: [-1.0, 0.0, 0.0],
        right: [0.0, 1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        cell: (2, 1),
    },
    // Left.
    Face {
        centre: [0.0, 1.0, 0.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 0.0, 1.0],
        cell: (1, 0),
    },
    // Right.
    Face {
        centre: [0.0, -1.0, 0.0],
        right: [-1.0, 0.0, 0.0],
        up: [0.0, 0.0, 1.0],
        cell: (0, 0),
    },
    // Up: its top forwards.
    Face {
        centre: [0.0, 0.0, 1.0],
        right: [0.0, 1.0, 0.0],
        up: [1.0, 0.0, 0.0],
        cell: (2, 0),
    },
    // Down: its top to the back.
    Face {
        centre: [0.0, 0.0, -1.0],
        right: [0.0, 1.0, 0.0],
        up: [-1.0, 0.0, 0.0],
        cell: (0, 1),
    },
];

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The face a direction meets.
fn face_of(d: [f32; 3]) -> usize {
    let (ax, ay, az) = (d[0].abs(), d[1].abs(), d[2].abs());
    if ax >= ay && ax >= az {
        if d[0] >= 0.0 { 0 } else { 1 }
    } else if ay >= az {
        if d[1] >= 0.0 { 2 } else { 3 }
    } else if d[2] >= 0.0 {
        4
    } else {
        5
    }
}

/// Where a region of a frame is: the cubemap face (0–5; equirectangular
/// pictures have one region) — cells meeting in a view across regions are
/// mapped pixel by pixel.
pub type Region = u8;

/// A spherical layout on a frame of a size: directions to its pixels and
/// back. Pixel coordinates are continuous (a pixel's middle at `n + 0.5`).
#[derive(Clone, Copy, Debug)]
pub struct Mapping {
    spherical: Spherical,
    /// The projection's frame from the scene's (`pose⁻¹`).
    unpose: [[f32; 3]; 3],
    posed: bool,
    /// The shown eye's part of the frame (pixels: x, y, width, height).
    eye: (f32, f32, f32, f32),
    /// The frame's size (pixels).
    size: (f32, f32),
}

impl Mapping {
    pub fn new(spherical: Spherical, width: u32, height: u32) -> Self {
        let (w, h) = (width.max(1) as f32, height.max(1) as f32);
        let (ex, ey, ew, eh) = spherical.stereo.left_eye();
        let pose = spherical.pose.orientation();
        Mapping {
            spherical,
            unpose: pose.inverse().matrix(),
            posed: !spherical.pose.is_none(),
            eye: (ex * w, ey * h, ew * w, eh * h),
            size: (w, h),
        }
    }

    pub fn spherical(&self) -> &Spherical {
        &self.spherical
    }

    /// The frame's size (pixels).
    pub fn size(&self) -> (f32, f32) {
        self.size
    }

    /// The matrix taking scene directions into the projection's frame.
    pub fn unpose(&self) -> [[f32; 3]; 3] {
        self.unpose
    }

    /// Whether the picture wraps round across (a whole equirectangular
    /// width: x is taken modulo the eye's width).
    pub fn wraps(&self) -> bool {
        matches!(self.spherical.projection,
            Projection::Equirectangular { crop } if crop.left == 0.0 && crop.right == 0.0)
    }

    /// The shown eye's part of the frame (pixels: x, y, width, height).
    pub fn eye(&self) -> (f32, f32, f32, f32) {
        self.eye
    }

    /// The pixel showing scene direction `d` (any length), and its region;
    /// `None` where the frame does not hold it (a crop). x may lie outside
    /// the eye where the picture wraps.
    pub fn pixel_of(&self, d: [f32; 3]) -> Option<(f32, f32, Region)> {
        let p = if self.posed {
            let m = &self.unpose;
            [dot(m[0], d), dot(m[1], d), dot(m[2], d)]
        } else {
            d
        };
        self.pixel_of_projected(p)
    }

    /// [`Self::pixel_of`] for a direction in the projection's own frame.
    pub fn pixel_of_projected(&self, p: [f32; 3]) -> Option<(f32, f32, Region)> {
        let (ex, ey, ew, eh) = self.eye;
        match self.spherical.projection {
            Projection::Equirectangular { crop } => {
                let h = (p[0] * p[0] + p[1] * p[1]).sqrt();
                let az = p[1].atan2(p[0]);
                let el = p[2].atan2(h);
                if !az.is_finite() || !el.is_finite() {
                    return None;
                }
                // Shares of the whole projection: the front in the middle,
                // the right to the right, up at the top.
                let u = 0.5 - az / std::f32::consts::TAU;
                let v = 0.5 - el / std::f32::consts::PI;
                let (l, r, t, b) = (
                    crop.left as f32,
                    crop.right as f32,
                    crop.top as f32,
                    crop.bottom as f32,
                );
                let fu = (u - l) / (1.0 - l - r);
                let fv = (v - t) / (1.0 - t - b);
                if !(0.0..=1.0).contains(&fv) || (!self.wraps() && !(0.0..=1.0).contains(&fu)) {
                    return None;
                }
                Some((ex + fu * ew, ey + fv * eh, 0))
            }
            Projection::Cubemap { padding } => {
                let f = face_of(p);
                let face = &FACES[f];
                let c = dot(p, face.centre);
                if c <= 0.0 || !c.is_finite() {
                    return None;
                }
                let s = (dot(p, face.right) / c).clamp(-1.0, 1.0);
                let t = (dot(p, face.up) / c).clamp(-1.0, 1.0);
                let (fw, fh) = (ew / 3.0, eh / 2.0);
                let pad = padding.clamp(0.0, 0.45);
                let (px, py) = (pad * fw, pad * fw);
                let x = ex + face.cell.0 as f32 * fw + px + (s + 1.0) * 0.5 * (fw - 2.0 * px);
                let y = ey + face.cell.1 as f32 * fh + py + (1.0 - t) * 0.5 * (fh - 2.0 * py);
                Some((x, y, f as Region))
            }
        }
    }

    /// The scene direction pixel (`x`, `y`) of the frame shows (unit), if
    /// it is in the shown eye's picture.
    pub fn direction_at(&self, x: f32, y: f32) -> Option<[f32; 3]> {
        let (ex, ey, ew, eh) = self.eye;
        let (u, v) = ((x - ex) / ew, (y - ey) / eh);
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return None;
        }
        let p = match self.spherical.projection {
            Projection::Equirectangular { crop } => {
                let fu = crop.left + u as f64 * (1.0 - crop.left - crop.right);
                let fv = crop.top + v as f64 * (1.0 - crop.top - crop.bottom);
                let az = (0.5 - fu) as f32 * 360.0;
                let el = (0.5 - fv) as f32 * 180.0;
                crate::ambisonics::direction(az, el)
            }
            Projection::Cubemap { padding } => {
                let (cx, cy) = ((u * 3.0).floor().min(2.0), (v * 2.0).floor().min(1.0));
                let face = FACES.iter().find(|f| f.cell == (cx as u8, cy as u8))?;
                let pad = padding.clamp(0.0, 0.45);
                let (fw, fh) = (ew / 3.0, eh / 2.0);
                let (px, py) = (pad * fw, pad * fw);
                let lx = x - ex - cx * fw - px;
                let ly = y - ey - cy * fh - py;
                let s = (lx / (fw - 2.0 * px) * 2.0 - 1.0).clamp(-1.0, 1.0);
                let t = (1.0 - ly / (fh - 2.0 * py) * 2.0).clamp(-1.0, 1.0);
                let v: [f32; 3] =
                    std::array::from_fn(|i| face.centre[i] + s * face.right[i] + t * face.up[i]);
                let n = dot(v, v).sqrt();
                v.map(|c| c / n)
            }
        };
        Some(if self.posed {
            // pose = unpose transposed.
            let m = &self.unpose;
            std::array::from_fn(|r| m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2])
        } else {
            p
        })
    }
}

/// A flat view into the sphere: `size` pixels, `fov` degrees across,
/// looking where `look` turns the front.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub look: Orientation,
    pub fov: f32,
    pub width: f32,
    pub height: f32,
}

impl Viewport {
    pub fn new(look: Orientation, fov: f32, width: f32, height: f32) -> Self {
        Self {
            look,
            fov: fov.clamp(MIN_FOV, MAX_FOV),
            width: width.max(1.0),
            height: height.max(1.0),
        }
    }

    /// Pixels from the middle to a direction one radian's tangent off it.
    pub fn focal(&self) -> f32 {
        self.width * 0.5 / (self.fov.to_radians() * 0.5).tan()
    }

    /// The ray through view point (`x`, `y`) in the viewer's frame (not
    /// unit: the focal length ahead).
    pub fn camera_ray(&self, x: f32, y: f32) -> [f32; 3] {
        [self.focal(), self.width * 0.5 - x, self.height * 0.5 - y]
    }

    /// The scene direction (unit) seen at view point (`x`, `y`).
    pub fn direction_at(&self, x: f32, y: f32) -> [f32; 3] {
        let c = self.camera_ray(x, y);
        let n = dot(c, c).sqrt();
        self.look.rotate(c.map(|v| v / n))
    }

    /// Where the view shows scene direction `d` (`None`: behind, or not
    /// within 89° of the middle).
    pub fn point_of(&self, d: [f32; 3]) -> Option<(f32, f32)> {
        let c = self.look.inverse().rotate(d);
        let n = dot(c, c).sqrt();
        if !n.is_finite() || n <= 0.0 || c[0] < n * 0.0175 {
            return None;
        }
        let f = self.focal();
        Some((
            self.width * 0.5 - f * c[1] / c[0],
            self.height * 0.5 - f * c[2] / c[0],
        ))
    }

    /// The field of view top to bottom (degrees).
    pub fn vertical_fov(&self) -> f32 {
        let half = (self.fov.to_radians() * 0.5).tan() * self.height / self.width;
        (half.atan() * 2.0).to_degrees()
    }

    /// Degrees a pointer moving one pixel turns the view (at its middle).
    pub fn degrees_per_pixel(&self) -> f32 {
        (1.0 / self.focal()).atan().to_degrees()
    }
}

/// The yaw and pitch a view looking at `look` turns to when the picture is
/// dragged by (`dx`, `dy`) pixels in `view` (grabbed: what was under the
/// pointer follows it). Pitch stays within ±90°, yaw within ±180°.
pub fn dragged(yaw: f32, pitch: f32, dx: f32, dy: f32, view: &Viewport) -> (f32, f32) {
    let k = view.degrees_per_pixel();
    let mut yaw = yaw + dx * k;
    yaw = (yaw + 180.0).rem_euclid(360.0) - 180.0;
    (yaw, (pitch + dy * k).clamp(-90.0, 90.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ambisonics::{angles, direction};

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    /// Known directions land on known pixels of a 3600×1800 picture: the
    /// front in the middle, the left a quarter in, the back at the edges,
    /// up and down along the top and bottom.
    #[test]
    fn equirectangular_directions_land_on_their_pixels() {
        let m = Mapping::new(Spherical::EQUIRECTANGULAR, 3600, 1800);
        let at = |az: f32, el: f32| {
            let (x, y, r) = m.pixel_of(direction(az, el)).unwrap();
            assert_eq!(r, 0);
            (x, y)
        };
        let (x, y) = at(0.0, 0.0);
        assert!(close(x, 1800.0, 0.01) && close(y, 900.0, 0.01), "{x} {y}");
        // Left is left of the middle, a quarter of the width in.
        let (x, y) = at(90.0, 0.0);
        assert!(close(x, 900.0, 0.01) && close(y, 900.0, 0.01));
        let (x, _) = at(-90.0, 0.0);
        assert!(close(x, 2700.0, 0.01));
        // 10° per 100 pixels both ways.
        let (x, y) = at(-30.0, 45.0);
        assert!(close(x, 2100.0, 0.05) && close(y, 450.0, 0.05), "{x} {y}");
        let (_, y) = at(0.0, -60.0);
        assert!(close(y, 1500.0, 0.05));
        // Behind: at an edge.
        let (x, _) = at(180.0, 0.0);
        assert!(close(x, 0.0, 0.05) || close(x, 3600.0, 0.05), "{x}");
        // And back: every pixel's direction lands on it again.
        for (x, y) in [
            (10.5, 20.5),
            (1800.5, 900.5),
            (3000.0, 1700.0),
            (2222.0, 333.0),
        ] {
            let d = m.direction_at(x, y).unwrap();
            let (bx, by, _) = m.pixel_of(d).unwrap();
            assert!(
                close(bx, x, 0.05) && close(by, y, 0.05),
                "{x},{y} → {bx},{by}"
            );
        }
    }

    /// A crop leaves part of the sphere out: the frame starts at the
    /// crop, and directions outside it are not in the picture.
    #[test]
    fn a_cropped_picture_holds_only_its_part() {
        let s = Spherical {
            projection: Projection::Equirectangular {
                crop: Crop {
                    top: 0.25,
                    bottom: 0.25,
                    left: 0.0,
                    right: 0.0,
                },
            },
            ..Spherical::EQUIRECTANGULAR
        };
        let m = Mapping::new(s, 3600, 900);
        // The horizon in the middle of the frame, 45° up at its top.
        let (_, y, _) = m.pixel_of(direction(0.0, 0.0)).unwrap();
        assert!(close(y, 450.0, 0.05));
        let (_, y, _) = m.pixel_of(direction(0.0, 44.9)).unwrap();
        assert!(close(y, 1.0, 1.0), "{y}");
        assert!(m.pixel_of(direction(0.0, 60.0)).is_none());
        let d = m.direction_at(1800.0, 0.0).unwrap();
        assert!(close(angles(d).1, 45.0, 0.01));
        // Still wraps round across.
        assert!(m.wraps());
        let sides = Spherical {
            projection: Projection::Equirectangular {
                crop: Crop {
                    left: 0.25,
                    right: 0.25,
                    ..Crop::default()
                },
            },
            ..Spherical::EQUIRECTANGULAR
        };
        let m = Mapping::new(sides, 1800, 1800);
        assert!(!m.wraps());
        assert!(m.pixel_of(direction(180.0, 0.0)).is_none());
        let (x, _, _) = m.pixel_of(direction(0.0, 0.0)).unwrap();
        assert!(close(x, 900.0, 0.05));
    }

    /// The cube's faces are where layout 0 puts them, turned as it says:
    /// a direction slightly up and right of each face's middle lands up
    /// and right of the cell's middle (for the four round the viewer), the
    /// up face's top forwards, the down face's to the back.
    #[test]
    fn cubemap_faces_sit_in_their_cells_turned_as_the_layout_says() {
        let m = Mapping::new(Spherical::CUBEMAP, 3000, 2000);
        let cell_middle = |c: (f32, f32)| (c.0 * 1000.0 + 500.0, c.1 * 1000.0 + 500.0);
        for (az, el, cell) in [
            (0.0, 0.0, (1.0, 1.0)),
            (180.0, 0.0, (2.0, 1.0)),
            (90.0, 0.0, (1.0, 0.0)),
            (-90.0, 0.0, (0.0, 0.0)),
            (0.0, 90.0, (2.0, 0.0)),
            (0.0, -90.0, (0.0, 1.0)),
        ] {
            let (x, y, _) = m.pixel_of(direction(az, el)).unwrap();
            let (mx, my) = cell_middle(cell);
            assert!(close(x, mx, 0.5) && close(y, my, 0.5), "{az},{el}: {x},{y}");
            if el == 0.0 {
                // A little to the right (azimuth down) and up.
                let (x2, y2, _) = m.pixel_of(direction(az - 5.0, 5.0)).unwrap();
                assert!(x2 > x && y2 < y, "{az}: {x2},{y2}");
            }
        }
        // Up: looking up, forwards is towards the face's top.
        let (_, y0, _) = m.pixel_of(direction(0.0, 90.0)).unwrap();
        let (_, y1, _) = m.pixel_of(direction(0.0, 80.0)).unwrap();
        assert!(y1 < y0, "the up face's top is forwards");
        // Down: forwards is towards its bottom.
        let (_, y0, _) = m.pixel_of(direction(0.0, -90.0)).unwrap();
        let (_, y1, _) = m.pixel_of(direction(0.0, -80.0)).unwrap();
        assert!(y1 > y0, "the down face's top is to the back");
        // A face's edge: 45° off its middle.
        let (x, _, r) = m.pixel_of(direction(-44.9, 0.0)).unwrap();
        assert_eq!(r, 0);
        assert!(close(x, 2000.0, 2.0), "{x}");
        // Back again, padding too.
        let padded = Spherical {
            projection: Projection::Cubemap { padding: 0.02 },
            ..Spherical::CUBEMAP
        };
        for s in [Spherical::CUBEMAP, padded] {
            let m = Mapping::new(s, 3000, 2000);
            for (az, el) in [(10.0, 20.0), (-100.0, -30.0), (170.0, 50.0), (33.0, 80.0)] {
                let (x, y, _) = m.pixel_of(direction(az, el)).unwrap();
                let (a, e) = angles(m.direction_at(x, y).unwrap());
                assert!(
                    close(a, az, 0.05) && close(e, el, 0.05),
                    "{az},{el} → {a},{e}"
                );
            }
        }
        // Padding keeps the face's edges inside its cell.
        let m = Mapping::new(padded, 3000, 2000);
        let (x, _, _) = m.pixel_of(direction(-44.99, 0.0)).unwrap();
        assert!(close(x, 2000.0 - 20.0, 1.0), "{x}");
    }

    /// The pose turns the projection in the scene: a picture posed 90° to
    /// the left shows its middle at the scene's left.
    #[test]
    fn the_pose_turns_the_projection() {
        let s = Spherical {
            pose: Pose {
                yaw: 90.0,
                pitch: 0.0,
                roll: 0.0,
            },
            ..Spherical::EQUIRECTANGULAR
        };
        let m = Mapping::new(s, 3600, 1800);
        let (x, y, _) = m.pixel_of(direction(90.0, 0.0)).unwrap();
        assert!(close(x, 1800.0, 0.05) && close(y, 900.0, 0.05), "{x} {y}");
        let (a, e) = angles(m.direction_at(1800.0, 900.0).unwrap());
        assert!(close(a, 90.0, 0.01) && close(e, 0.0, 0.01));
        let tilted = Spherical {
            pose: Pose {
                yaw: 0.0,
                pitch: 30.0,
                roll: 0.0,
            },
            ..Spherical::EQUIRECTANGULAR
        };
        let m = Mapping::new(tilted, 3600, 1800);
        let (a, e) = angles(m.direction_at(1800.0, 900.0).unwrap());
        assert!(close(a, 0.0, 0.01) && close(e, 30.0, 0.01));
    }

    /// Stereo frames show the left eye: top half, left half, right half.
    #[test]
    fn stereo_frames_show_the_left_eye() {
        for (stereo, x, y) in [
            (StereoMode::TopBottom, 1800.0, 450.0),
            (StereoMode::LeftRight, 900.0, 900.0),
            (StereoMode::RightLeft, 2700.0, 900.0),
        ] {
            let s = Spherical {
                stereo,
                ..Spherical::EQUIRECTANGULAR
            };
            let m = Mapping::new(s, 3600, 1800);
            let (px, py, _) = m.pixel_of(direction(0.0, 0.0)).unwrap();
            assert!(close(px, x, 0.05) && close(py, y, 0.05), "{stereo:?}");
        }
    }

    /// A view's middle looks where it is turned; the edges are half the
    /// field of view off; points go back to their directions.
    #[test]
    fn a_viewport_looks_where_it_is_turned() {
        let v = Viewport::new(Orientation::IDENTITY, 90.0, 1600.0, 900.0);
        let (a, e) = angles(v.direction_at(800.0, 450.0));
        assert!(close(a, 0.0, 1e-3) && close(e, 0.0, 1e-3));
        // The right edge: 45° right; the top edge less than 45° up.
        let (a, _) = angles(v.direction_at(1600.0, 450.0));
        assert!(close(a, -45.0, 1e-3), "{a}");
        let (_, e) = angles(v.direction_at(800.0, 0.0));
        assert!(close(e, v.vertical_fov() / 2.0, 1e-3), "{e}");
        assert!(v.vertical_fov() < 90.0);
        // Turned left and up.
        let v = Viewport::new(Orientation::from_ypr(60.0, 20.0, 0.0), 90.0, 1600.0, 900.0);
        let (a, e) = angles(v.direction_at(800.0, 450.0));
        assert!(close(a, 60.0, 1e-3) && close(e, 20.0, 1e-3), "{a} {e}");
        for (x, y) in [(100.0, 100.0), (1500.0, 800.0), (800.0, 450.0)] {
            let (bx, by) = v.point_of(v.direction_at(x, y)).unwrap();
            assert!(close(bx, x, 0.01) && close(by, y, 0.01));
        }
        // Behind is nowhere.
        assert!(v.point_of(direction(-120.0, -20.0)).is_none());
        // Rolled to the right: up in the scene is up-left in the view.
        let v = Viewport::new(Orientation::from_ypr(0.0, 0.0, 30.0), 90.0, 1600.0, 900.0);
        let (x, y) = v.point_of(direction(0.0, 20.0)).unwrap();
        assert!(x < 800.0 && y < 450.0, "{x} {y}");
    }

    /// Dragging the picture right turns the view left by the degrees the
    /// pointer crossed; pitch stops at the poles, yaw wraps.
    #[test]
    fn dragging_turns_the_view_with_the_pointer() {
        let v = Viewport::new(Orientation::IDENTITY, 90.0, 1600.0, 900.0);
        let (yaw, pitch) = dragged(0.0, 0.0, 100.0, 0.0, &v);
        assert!(close(yaw, 100.0 * v.degrees_per_pixel(), 1e-4));
        assert!(yaw > 7.0 && yaw < 7.2, "{yaw}");
        assert_eq!(pitch, 0.0);
        let (_, pitch) = dragged(0.0, 80.0, 0.0, 400.0, &v);
        assert_eq!(pitch, 90.0);
        let (yaw, _) = dragged(175.0, 0.0, 200.0, 0.0, &v);
        assert!(yaw < -150.0, "{yaw}");
    }

    #[test]
    fn layouts_read_back_from_their_files() {
        for s in [
            Spherical::EQUIRECTANGULAR,
            Spherical::CUBEMAP,
            Spherical {
                projection: Projection::Equirectangular {
                    crop: Crop {
                        top: 0.1,
                        ..Crop::default()
                    },
                },
                stereo: StereoMode::TopBottom,
                pose: Pose {
                    yaw: 10.0,
                    pitch: -5.0,
                    roll: 2.0,
                },
            },
        ] {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(
                serde_json::from_str::<Spherical>(&json).unwrap(),
                s,
                "{json}"
            );
        }
        assert_eq!(
            serde_json::to_string(&Spherical::EQUIRECTANGULAR).unwrap(),
            r#"{"projection":{"type":"equirectangular"}}"#
        );
        assert_eq!(Spherical::EQUIRECTANGULAR.label(), "360° equirectangular");
        // A 90° view 1280 wide wants a 5120-wide equirectangular frame.
        assert!(close(
            Spherical::EQUIRECTANGULAR.width_for(1280.0, 90.0),
            5120.0,
            0.5
        ));
    }
}
