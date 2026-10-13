//! Vector base amplitude panning (V. Pulkki, "Virtual sound source
//! positioning using vector base amplitude panning", JAES 45(6), 1997).
//!
//! A source in direction `p` plays on the two (horizontal layouts) or
//! three (layouts with height speakers) speakers whose directions enclose
//! it: `p = g₁s₁ + g₂s₂ (+ g₃s₃)`, solved with the inverse of the speakers'
//! direction matrix and normalised to constant power (`Σ g² = 1`). At a
//! speaker's own direction that speaker plays alone; moving across the
//! edge between two regions the gains change continuously (the speaker
//! being left fades to zero exactly at the edge).
//!
//! Directions are azimuth (degrees, 0 ahead, positive to the left) and
//! elevation (degrees above ear level), as [`crate::surround::Speaker`]s
//! give them; vectors are `x` right, `y` front, `z` up. The LFE never takes
//! part: it has no direction.
//!
//! * **2D** (no speaker above ear level): the speakers are sorted by
//!   azimuth and each neighbouring pair less than 180° apart is a panning
//!   region. A gap of 180° or more (LCR's back, stereo around the back)
//!   cannot be represented by positive gains: there the source crossfades
//!   (constant power, by angle) between the gap's two end speakers, so
//!   moving through it stays continuous and nothing else plays — the image
//!   cannot come from that direction, and [`Vbap::covers`] says so.
//!   Elevation is ignored.
//! * **3D**: the regions are the faces of the speakers' convex hull, with
//!   a virtual speaker straight below the listener (FaderFrame's formats
//!   have nothing below ear level: the hull then closes, the ear-level
//!   ring's neighbours stay edges, and whatever the virtual speaker would
//!   get is left out; a layout with speakers below ear level has no
//!   virtual one and is panned over the whole sphere). A face with more
//!   than three speakers on one plane
//!   (7.1.4's four tops) is split into triangles around a virtual speaker
//!   at its centre, whose gain goes to each of the face's `k` speakers at
//!   `1/√k` before the result is normalised — so symmetric layouts stay
//!   symmetric (overhead in 7.1.4: the four tops alike, −6 dB each) where
//!   cutting the face along a diagonal would favour two of them.
//!   Elevation is limited to 0…90° — down to −90° where speakers stand
//!   below ear level ([`Vbap::min_elevation`]); at 0° a layout with tops
//!   only pans as its ring would in 2D.
//!
//! Everything layout-dependent — directions, regions and their inverse
//! matrices — is worked out by [`Vbap::new`] (off the audio thread: it
//! searches every triple of speakers); [`Vbap::gains`] then only finds the
//! region and multiplies, allocates nothing, and never returns NaN or
//! infinity.

use crate::surround::{MAX_SPEAKERS, Speaker};

/// Most panning regions (the hull of every speaker and the virtual one
/// below has at most 2V − 4 triangles; faces split around their centre
/// add two each).
const MAX_TRIANGLES: usize = 64;
/// Most faces of four or more speakers.
const MAX_FACES: usize = 12;
/// The virtual speaker below the listener (3D).
const BELOW: u8 = u8::MAX;
/// Vertex ids from here on are the centres of faces (`FACE + n`).
const FACE: u8 = 200;
/// Speakers closer than this (degrees) are one direction twice.
const MIN_SEPARATION_DEG: f32 = 1.0;
/// Gains this far below zero still count as inside a region (rounding at
/// its edges).
const EDGE: f32 = 1e-5;

/// The unit vector of a direction (degrees; x right, y front, z up).
pub fn unit(azimuth: f32, elevation: f32) -> [f32; 3] {
    let (a, e) = (azimuth.to_radians(), elevation.to_radians());
    let (sa, ca) = a.sin_cos();
    let (se, ce) = e.sin_cos();
    [-sa * ce, ca * ce, se]
}

/// The direction of a vector: `(azimuth, elevation)` in degrees.
pub fn direction_of(v: [f32; 3]) -> (f32, f32) {
    let h = (v[0] * v[0] + v[1] * v[1]).sqrt();
    ((-v[0]).atan2(v[1]).to_degrees(), v[2].atan2(h).to_degrees())
}

/// Why a set of speakers cannot be panned over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VbapError {
    NoSpeakers,
    TooMany,
    BadDirection(usize),
    SameDirection(usize, usize),
    NotAround,
}

impl std::fmt::Display for VbapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VbapError::NoSpeakers => write!(f, "no speaker with a direction"),
            VbapError::TooMany => write!(f, "too many speakers or regions"),
            VbapError::BadDirection(i) => write!(f, "speaker {i} has no usable direction"),
            VbapError::SameDirection(a, b) => write!(f, "speakers {a} and {b} point the same way"),
            VbapError::NotAround => write!(f, "the speakers do not surround the listener"),
        }
    }
}

impl std::error::Error for VbapError {}

/// A neighbouring pair of a horizontal layout (channel indices), from
/// `start` (radians, 0…2π) over `span`; `inverse`: the pair's matrix
/// inverted, `None` for a gap of 180° or more (crossfaded).
#[derive(Clone, Copy, Debug, Default)]
struct Arc {
    a: u8,
    b: u8,
    start: f32,
    span: f32,
    inverse: Option<[[f32; 2]; 2]>,
}

/// A triangle of the hull (channel indices, [`BELOW`] for the virtual
/// speaker below, `FACE + n` for the centre of face `n`) and its matrix
/// inverted.
#[derive(Clone, Copy, Debug, Default)]
struct Triangle {
    v: [u8; 3],
    inverse: [[f32; 3]; 3],
}

/// The panning geometry of a set of speakers.
#[derive(Clone, Debug)]
pub struct Vbap {
    /// Channels in the layout (gains are written for each).
    channels: usize,
    /// Each channel's direction (zero for the LFE).
    dirs: [[f32; 3]; MAX_SPEAKERS],
    /// Whether a channel is a main speaker (not the LFE).
    mains: [bool; MAX_SPEAKERS],
    three_d: bool,
    /// The lowest elevation a source can take (0, or −90 with speakers
    /// below ear level).
    min_elevation: f32,
    /// The one speaker of a layout that has only one.
    single: Option<u8>,
    arcs: [Arc; MAX_SPEAKERS],
    arc_count: usize,
    triangles: [Triangle; MAX_TRIANGLES],
    triangle_count: usize,
    /// The speakers of each face split around its centre.
    faces: [[u8; MAX_SPEAKERS]; MAX_FACES],
    face_len: [u8; MAX_FACES],
    face_count: usize,
}

impl Vbap {
    /// The geometry of `speakers` (channel order; the LFE is skipped): 3D
    /// when any stands above ear level.
    pub fn new(speakers: &[Speaker]) -> Result<Self, VbapError> {
        if speakers.len() > MAX_SPEAKERS {
            return Err(VbapError::TooMany);
        }
        let mut v = Self {
            channels: speakers.len(),
            dirs: [[0.0; 3]; MAX_SPEAKERS],
            mains: [false; MAX_SPEAKERS],
            three_d: false,
            min_elevation: 0.0,
            single: None,
            arcs: [Arc::default(); MAX_SPEAKERS],
            arc_count: 0,
            triangles: [Triangle::default(); MAX_TRIANGLES],
            triangle_count: 0,
            faces: [[0; MAX_SPEAKERS]; MAX_FACES],
            face_len: [0; MAX_FACES],
            face_count: 0,
        };
        for (i, s) in speakers.iter().enumerate() {
            if s.lfe {
                continue;
            }
            if !s.azimuth.is_finite() || !s.elevation.is_finite() || s.elevation.abs() > 90.0 {
                return Err(VbapError::BadDirection(i));
            }
            v.dirs[i] = s.direction();
            v.mains[i] = true;
            v.three_d |= s.elevation.abs() > 0.5;
            if s.elevation < -0.5 {
                v.min_elevation = -90.0;
            }
        }
        let mains: Vec<usize> = (0..speakers.len()).filter(|&i| v.mains[i]).collect();
        if mains.is_empty() {
            return Err(VbapError::NoSpeakers);
        }
        let min_cos = MIN_SEPARATION_DEG.to_radians().cos();
        for (k, &i) in mains.iter().enumerate() {
            for &j in &mains[k + 1..] {
                if dot(v.dirs[i], v.dirs[j]) > min_cos {
                    return Err(VbapError::SameDirection(i, j));
                }
            }
        }
        if mains.len() == 1 {
            v.single = Some(mains[0] as u8);
            v.three_d = false;
            return Ok(v);
        }
        if v.three_d {
            v.hull(speakers, &mains)?;
        } else {
            v.ring(speakers, &mains);
        }
        Ok(v)
    }

    /// The geometry of a built-in format (they are all valid).
    pub fn of(format: crate::SurroundFormat) -> Self {
        match Self::new(format.speakers()) {
            Ok(v) => v,
            // Unreachable for the built-in formats (tested); a centre
            // speaker keeps the result safe all the same.
            Err(_) => Self {
                channels: format.channels(),
                dirs: [[0.0; 3]; MAX_SPEAKERS],
                mains: [false; MAX_SPEAKERS],
                three_d: false,
                min_elevation: 0.0,
                single: Some(0),
                arcs: [Arc::default(); MAX_SPEAKERS],
                arc_count: 0,
                triangles: [Triangle::default(); MAX_TRIANGLES],
                triangle_count: 0,
                faces: [[0; MAX_SPEAKERS]; MAX_FACES],
                face_len: [0; MAX_FACES],
                face_count: 0,
            },
        }
    }

    /// Whether elevation does anything (a layout with height speakers).
    pub fn is_3d(&self) -> bool {
        self.three_d
    }

    /// The lowest elevation a source can take: 0 (ear level), or −90
    /// where the layout has speakers below ear level.
    pub fn min_elevation(&self) -> f32 {
        self.min_elevation
    }

    /// The horizontal regions: neighbours sorted by azimuth.
    fn ring(&mut self, speakers: &[Speaker], mains: &[usize]) {
        let mut sorted: Vec<(f32, usize)> = mains
            .iter()
            .map(|&i| (speakers[i].azimuth.to_radians().rem_euclid(TAU), i))
            .collect();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        let n = sorted.len();
        for k in 0..n {
            let (start, a) = sorted[k];
            let (end, b) = sorted[(k + 1) % n];
            let span = (end - start).rem_euclid(TAU);
            let span = if span == 0.0 { TAU } else { span };
            let inverse = (span < std::f32::consts::PI - 1e-3).then(|| {
                let (sa, sb) = (self.dirs[a], self.dirs[b]);
                let det = sa[0] * sb[1] - sa[1] * sb[0];
                [[sb[1] / det, -sa[1] / det], [-sb[0] / det, sa[0] / det]]
            });
            self.arcs[k] = Arc {
                a: a as u8,
                b: b as u8,
                start,
                span,
                inverse,
            };
        }
        self.arc_count = n;
    }

    /// The 3D regions: the convex hull of the speakers and the virtual one
    /// below, faces split into triangles.
    fn hull(&mut self, speakers: &[Speaker], mains: &[usize]) -> Result<(), VbapError> {
        let mut points: Vec<([f32; 3], u8)> =
            mains.iter().map(|&i| (self.dirs[i], i as u8)).collect();
        if !mains.iter().any(|&i| speakers[i].elevation < -0.5) {
            points.push(([0.0, 0.0, -1.0], BELOW));
        }
        let n = points.len();
        let mut faces: Vec<u32> = Vec::new();
        let eps = 1e-4;
        for i in 0..n {
            for j in i + 1..n {
                for k in j + 1..n {
                    let (pi, pj, pk) = (points[i].0, points[j].0, points[k].0);
                    let mut normal = cross(sub(pj, pi), sub(pk, pi));
                    let len = dot(normal, normal).sqrt();
                    if len < 1e-6 {
                        continue;
                    }
                    normal = scale(normal, 1.0 / len);
                    let mut d = dot(normal, pi);
                    let (mut above, mut below, mut on) = (false, false, 0u32);
                    for (m, (q, _)) in points.iter().enumerate() {
                        let s = dot(normal, *q) - d;
                        if s > eps {
                            above = true;
                        } else if s < -eps {
                            below = true;
                        } else {
                            on |= 1 << m;
                        }
                    }
                    if above && below {
                        continue;
                    }
                    if above {
                        // Facing out: everything else on the inside.
                        d = -d;
                    }
                    if d <= eps {
                        // The plane passes the listener (or lies beyond):
                        // these speakers leave a side open.
                        return Err(VbapError::NotAround);
                    }
                    if !faces.contains(&on) {
                        faces.push(on);
                    }
                }
            }
        }
        if faces.is_empty() {
            return Err(VbapError::NotAround);
        }
        for face in faces {
            let members: Vec<usize> = (0..n).filter(|m| face & (1 << m) != 0).collect();
            if members.len() == 3 {
                let tri = [members[0], members[1], members[2]];
                self.add_triangle(tri.map(|m| points[m]))?;
                continue;
            }
            // Four or more on one plane: around a virtual speaker at the
            // face's centre (on the sphere), in order around it.
            if self.face_count == MAX_FACES || members.iter().any(|&m| points[m].1 == BELOW) {
                return Err(VbapError::TooMany);
            }
            let id = FACE + self.face_count as u8;
            let k = self.face_count;
            for (slot, &m) in self.faces[k].iter_mut().zip(&members) {
                *slot = points[m].1;
            }
            self.face_len[k] = members.len() as u8;
            self.face_count += 1;
            let centre = normalise(members.iter().fold([0.0; 3], |c, &m| add(c, points[m].0)));
            let u = normalise(sub(points[members[0]].0, centre));
            let w = cross(centre, u);
            let mut ordered: Vec<(f32, usize)> = members
                .iter()
                .map(|&m| {
                    let r = sub(points[m].0, centre);
                    (dot(r, w).atan2(dot(r, u)), m)
                })
                .collect();
            ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
            for t in 0..ordered.len() {
                let (a, b) = (ordered[t].1, ordered[(t + 1) % ordered.len()].1);
                self.add_triangle([(centre, id), points[a], points[b]])?;
            }
        }
        Ok(())
    }

    fn add_triangle(&mut self, v: [([f32; 3], u8); 3]) -> Result<(), VbapError> {
        let Some(inverse) = invert3([v[0].0, v[1].0, v[2].0]) else {
            // No area: nothing can be inside it.
            return Ok(());
        };
        if self.triangle_count == MAX_TRIANGLES {
            return Err(VbapError::TooMany);
        }
        self.triangles[self.triangle_count] = Triangle {
            v: [v[0].1, v[1].1, v[2].1],
            inverse,
        };
        self.triangle_count += 1;
        Ok(())
    }

    /// Whether the direction lies inside a region the speakers can form
    /// (false in a horizontal layout's gap of 180° or more).
    pub fn covers(&self, azimuth: f32) -> bool {
        if self.single.is_some() || self.three_d {
            return true;
        }
        let a = azimuth.to_radians().rem_euclid(TAU);
        self.arc_at(a).is_some_and(|arc| arc.inverse.is_some())
    }

    fn arc_at(&self, a: f32) -> Option<&Arc> {
        self.arcs[..self.arc_count]
            .iter()
            .find(|arc| (a - arc.start).rem_euclid(TAU) < arc.span)
            .or_else(|| self.arcs[..self.arc_count].first())
    }

    /// The gain of every channel (into `out`, layout order; the LFE and
    /// channels beyond `out` stay 0) for a source in direction
    /// (`azimuth`, `elevation`) in degrees. Non-negative; the squares sum
    /// to one.
    pub fn gains(&self, azimuth: f32, elevation: f32, out: &mut [f32]) {
        for o in out.iter_mut() {
            *o = 0.0;
        }
        let azimuth = if azimuth.is_finite() { azimuth } else { 0.0 };
        let elevation = if elevation.is_finite() {
            elevation.clamp(self.min_elevation, 90.0)
        } else {
            0.0
        };
        if let Some(s) = self.single {
            put(out, s, 1.0);
            return;
        }
        if self.three_d {
            self.gains_3d(unit(azimuth, elevation), out);
        } else {
            self.gains_2d(azimuth.to_radians().rem_euclid(TAU), out);
        }
    }

    fn gains_2d(&self, a: f32, out: &mut [f32]) {
        let Some(arc) = self.arc_at(a) else {
            return;
        };
        let (ga, gb) = match arc.inverse {
            Some(inv) => {
                let p = [-a.sin(), a.cos()];
                let ga = p[0] * inv[0][0] + p[1] * inv[1][0];
                let gb = p[0] * inv[0][1] + p[1] * inv[1][1];
                (snap(ga), snap(gb))
            }
            // A gap: crossfaded by angle.
            None => {
                let t = ((a - arc.start).rem_euclid(TAU) / arc.span).clamp(0.0, 1.0);
                let q = t * std::f32::consts::FRAC_PI_2;
                (q.cos(), q.sin())
            }
        };
        let norm = (ga * ga + gb * gb).sqrt();
        if norm > 1e-9 {
            put(out, arc.a, ga / norm);
            put(out, arc.b, gb / norm);
        } else {
            put(out, arc.a, 1.0);
        }
    }

    fn gains_3d(&self, p: [f32; 3], out: &mut [f32]) {
        let solve = |t: &Triangle| -> [f32; 3] {
            std::array::from_fn(|j| (0..3).map(|i| p[i] * t.inverse[i][j]).sum::<f32>())
        };
        let tris = &self.triangles[..self.triangle_count];
        let found = tris
            .iter()
            .find_map(|t| {
                let g = solve(t);
                (g.iter().all(|&v| v >= -EDGE)).then_some((t, g))
            })
            // Rounding left the direction outside every region: the one it
            // is least outside of.
            .or_else(|| {
                tris.iter()
                    .map(|t| (t, solve(t)))
                    .max_by(|a, b| min3(a.1).total_cmp(&min3(b.1)))
            });
        if let Some((t, g)) = found {
            // The real speakers' shares (a face centre's spread over its
            // speakers at 1/√k each; below the listener: left out).
            for (&v, &gain) in t.v.iter().zip(&g) {
                let gain = snap(gain);
                if gain == 0.0 || v == BELOW {
                    continue;
                }
                if v >= FACE {
                    let f = (v - FACE) as usize;
                    let members = &self.faces[f][..self.face_len[f] as usize];
                    let share = gain / (members.len() as f32).sqrt();
                    for &m in members {
                        add_to(out, m, share);
                    }
                } else {
                    add_to(out, v, gain);
                }
            }
            let norm = out.iter().map(|g| g * g).sum::<f32>().sqrt();
            if norm > 1e-9 {
                for o in out.iter_mut() {
                    *o /= norm;
                }
                return;
            }
            for o in out.iter_mut() {
                *o = 0.0;
            }
        }
        // Nothing usable: the nearest speaker.
        let nearest = (0..self.channels)
            .filter(|&i| self.mains[i])
            .max_by(|&a, &b| dot(self.dirs[a], p).total_cmp(&dot(self.dirs[b], p)));
        if let Some(i) = nearest {
            put(out, i as u8, 1.0);
        }
    }
}

const TAU: f32 = std::f32::consts::TAU;

fn put(out: &mut [f32], channel: u8, g: f32) {
    if let Some(o) = out.get_mut(channel as usize) {
        *o = g;
    }
}

fn add_to(out: &mut [f32], channel: u8, g: f32) {
    if let Some(o) = out.get_mut(channel as usize) {
        *o += g;
    }
}

/// A gain with the rounding at a region's edge taken off (never negative).
fn snap(g: f32) -> f32 {
    if g <= EDGE { 0.0 } else { g }
}

fn min3(g: [f32; 3]) -> f32 {
    g[0].min(g[1]).min(g[2])
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalise(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    if l > 0.0 { scale(a, 1.0 / l) } else { a }
}

/// The inverse of the matrix whose rows are `m` (`None`: singular).
fn invert3(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let c0 = cross(m[1], m[2]);
    let c1 = cross(m[2], m[0]);
    let c2 = cross(m[0], m[1]);
    let det = dot(m[0], c0);
    if det.abs() < 1e-6 {
        return None;
    }
    // (M⁻¹)ᵀ rows are the cofactor vectors / det, so M⁻¹'s columns are.
    Some(std::array::from_fn(|i| {
        [c0[i] / det, c1[i] / det, c2[i] / det]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SurroundFormat;

    fn gains(f: SurroundFormat, az: f32, el: f32) -> Vec<f32> {
        let v = Vbap::of(f);
        let mut g = vec![0.0; f.channels()];
        v.gains(az, el, &mut g);
        g
    }

    fn at(f: SurroundFormat, label: &str) -> usize {
        f.speakers().iter().position(|s| s.label == label).unwrap()
    }

    fn power(g: &[f32]) -> f32 {
        g.iter().map(|v| v * v).sum()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn every_built_in_format_has_a_geometry() {
        for f in SurroundFormat::ALL {
            let v = Vbap::new(f.speakers()).unwrap_or_else(|e| panic!("{}: {e}", f.name()));
            assert_eq!(v.is_3d(), f.has_heights(), "{}", f.name());
        }
        for l in [crate::ChannelLayout::Mono, crate::ChannelLayout::Stereo] {
            let s = crate::surround::speakers_of(l).unwrap();
            assert!(Vbap::new(s).is_ok());
        }
    }

    #[test]
    fn a_source_at_a_speaker_plays_on_it_alone() {
        for f in SurroundFormat::ALL {
            for (i, s) in f.speakers().iter().enumerate() {
                if s.lfe {
                    continue;
                }
                let g = gains(f, s.azimuth, s.elevation);
                for (j, &v) in g.iter().enumerate() {
                    let want = if i == j { 1.0 } else { 0.0 };
                    assert!(close(v, want), "{} {}: {g:?}", f.name(), s.label);
                }
            }
        }
    }

    #[test]
    fn between_two_speakers_the_pair_shares_it() {
        // 5.1: halfway between L (+30°) and C (0°): equal, power one.
        let f = SurroundFormat::S51;
        let g = gains(f, 15.0, 0.0);
        let (l, c) = (g[at(f, "L")], g[at(f, "C")]);
        assert!(
            close(l, c) && close(l, std::f32::consts::FRAC_1_SQRT_2),
            "{g:?}"
        );
        assert!(close(power(&g), 1.0));
        // The reference value for an unequal split: 20° of the L–Ls pair
        // (30°…110°) at 50°, solved by hand: g = p·M⁻¹.
        let g = gains(f, 50.0, 0.0);
        let (a, b) = (
            50f64.to_radians(),
            (30f64.to_radians(), 110f64.to_radians()),
        );
        let det = (-b.0.sin()) * b.1.cos() - b.0.cos() * (-b.1.sin());
        let gl = ((-a.sin()) * b.1.cos() - a.cos() * (-b.1.sin())) / det;
        let gs = ((-b.0.sin()) * a.cos() - b.0.cos() * (-a.sin())) / det;
        let n = (gl * gl + gs * gs).sqrt();
        assert!(close(g[at(f, "L")], (gl / n) as f32), "{g:?}");
        assert!(close(g[at(f, "Ls")], (gs / n) as f32), "{g:?}");
        // Nothing else plays, never the LFE.
        for (i, &v) in g.iter().enumerate() {
            if i != at(f, "L") && i != at(f, "Ls") {
                assert_eq!(v, 0.0, "{g:?}");
            }
        }
    }

    #[test]
    fn mirror_images_and_continuity() {
        for (f, el) in [
            (SurroundFormat::S51, 0.0),
            (SurroundFormat::S71, 0.0),
            (SurroundFormat::S714, 0.0),
            (SurroundFormat::S714, 20.0),
            (SurroundFormat::S714, 60.0),
            (SurroundFormat::S512, 35.0),
        ] {
            let speakers = f.speakers();
            let mut last: Option<Vec<f32>> = None;
            for step in 0..=720 {
                let az = -180.0 + step as f32 * 0.5;
                let g = gains(f, az, el);
                assert!(g.iter().all(|v| v.is_finite() && *v >= 0.0));
                assert!(close(power(&g), 1.0), "{} {az}", f.name());
                // The mirror image: left and right swapped.
                let m = gains(f, -az, el);
                for (i, s) in speakers.iter().enumerate() {
                    let j = speakers
                        .iter()
                        .position(|o| {
                            o.lfe == s.lfe
                                && close(o.azimuth, -s.azimuth)
                                && close(o.elevation, s.elevation)
                        })
                        .unwrap();
                    assert!(close(g[i], m[j]), "{} {az}: {g:?} vs {m:?}", f.name());
                }
                // Half a degree on, no gain jumps.
                if let Some(prev) = &last {
                    let jump = prev
                        .iter()
                        .zip(&g)
                        .map(|(a, b)| (a - b).abs())
                        .fold(0.0f32, f32::max);
                    assert!(jump < 0.05, "{} {az}: {jump}", f.name());
                }
                last = Some(g);
            }
        }
    }

    #[test]
    fn five_one_and_seven_one_differ_where_their_speakers_do() {
        // 90° left: between L and Ls in 5.1, on Lss alone in 7.1.
        let g = gains(SurroundFormat::S51, 90.0, 0.0);
        let f = SurroundFormat::S51;
        assert!(
            g[at(f, "L")] > 0.0 && g[at(f, "Ls")] > g[at(f, "L")],
            "{g:?}"
        );
        let f = SurroundFormat::S71;
        let g = gains(f, 90.0, 0.0);
        assert!(close(g[at(f, "Lss")], 1.0), "{g:?}");
        // Straight behind: the two rear speakers alike in both.
        let f = SurroundFormat::S51;
        let g = gains(f, 180.0, 0.0);
        assert!(close(g[at(f, "Ls")], g[at(f, "Rs")]) && g[at(f, "Ls")] > 0.7);
        let f = SurroundFormat::S71;
        let g = gains(f, 180.0, 0.0);
        assert!(close(g[at(f, "Lrs")], g[at(f, "Rrs")]) && g[at(f, "Lrs")] > 0.7);
        assert_eq!(g[at(f, "Lss")], 0.0);
    }

    #[test]
    fn gaps_crossfade_and_say_so() {
        // LCR has nothing behind: around the back it crossfades L → R.
        let f = SurroundFormat::Lcr;
        let v = Vbap::of(f);
        assert!(v.covers(10.0) && !v.covers(90.0) && !v.covers(180.0));
        let g = gains(f, 180.0, 0.0);
        assert!(
            close(g[at(f, "L")], g[at(f, "R")]) && g[at(f, "C")] == 0.0,
            "{g:?}"
        );
        let g = gains(f, 31.0, 0.0);
        assert!(g[at(f, "L")] > 0.99, "{g:?}");
        // Stereo: the same around the back.
        let s = crate::surround::speakers_of(crate::ChannelLayout::Stereo).unwrap();
        let v = Vbap::new(s).unwrap();
        let mut g = [0.0; 2];
        v.gains(-90.0, 0.0, &mut g);
        assert!(g[1] > g[0] && close(power(&g), 1.0), "{g:?}");
        // Mono: always the one speaker.
        let m = crate::surround::speakers_of(crate::ChannelLayout::Mono).unwrap();
        let v = Vbap::new(m).unwrap();
        let mut g = [0.0; 1];
        v.gains(123.0, 40.0, &mut g);
        assert_eq!(g, [1.0]);
    }

    #[test]
    fn heights_pan_in_three_dimensions() {
        let f = SurroundFormat::S714;
        assert!(Vbap::of(f).is_3d());
        // At ear level as the ring alone, the tops silent.
        for az in [-170.0f32, -100.0, -45.0, 0.0, 12.0, 60.0, 135.0] {
            let ring = gains(SurroundFormat::S71, az, 0.0);
            let g = gains(f, az, 0.0);
            for (a, b) in g[..8].iter().zip(&ring) {
                assert!(close(*a, *b), "{az}: {g:?} vs {ring:?}");
            }
            assert!(g[8..].iter().all(|v| *v == 0.0), "{az}: {g:?}");
        }
        // Overhead: the four tops alike.
        let g = gains(f, 0.0, 90.0);
        let tops: Vec<f32> = ["Ltf", "Rtf", "Ltr", "Rtr"]
            .iter()
            .map(|l| g[at(f, l)])
            .collect();
        assert!(tops.iter().all(|v| close(*v, 0.5)), "{g:?}");
        // Between L and Ltf (same azimuth side): both, nothing behind.
        let g = gains(f, 38.0, 15.0);
        assert!(g[at(f, "Ltf")] > 0.2 && g[at(f, "L")] > 0.2, "{g:?}");
        assert_eq!(g[at(f, "Rtr")], 0.0);
        assert!(close(power(&g), 1.0));
        // Elevation sweeps stay smooth and finite in every height format.
        for f in [
            SurroundFormat::S512,
            SurroundFormat::S514,
            SurroundFormat::S712,
            SurroundFormat::S714,
        ] {
            for az in [-170.0f32, -90.0, -20.0, 0.0, 45.0, 135.0, 180.0] {
                let mut last: Option<Vec<f32>> = None;
                for e in 0..=90 {
                    let g = gains(f, az, e as f32);
                    assert!(g.iter().all(|v| v.is_finite() && *v >= 0.0));
                    assert!(close(power(&g), 1.0), "{} {az} {e}: {g:?}", f.name());
                    assert_eq!(g[at(f, "LFE")], 0.0);
                    if let Some(prev) = &last {
                        let jump = prev
                            .iter()
                            .zip(&g)
                            .map(|(a, b)| (a - b).abs())
                            .fold(0.0f32, f32::max);
                        assert!(jump < 0.08, "{} {az} {e}: {jump}", f.name());
                    }
                    last = Some(g);
                }
            }
        }
        // A 2D layout ignores elevation.
        assert_eq!(
            gains(SurroundFormat::S51, 20.0, 60.0),
            gains(SurroundFormat::S51, 20.0, 0.0)
        );
    }

    #[test]
    fn bad_layouts_are_refused_and_bad_values_stay_safe() {
        let base = SurroundFormat::S51.speakers();
        assert_eq!(Vbap::new(&[]).err(), Some(VbapError::NoSpeakers));
        assert_eq!(Vbap::new(&base[3..4]).err(), Some(VbapError::NoSpeakers));
        let mut twice = base.to_vec();
        twice[1].azimuth = 30.5; // R on top of L
        assert_eq!(
            Vbap::new(&twice).err(),
            Some(VbapError::SameDirection(0, 1))
        );
        let mut bad = base.to_vec();
        bad[2].azimuth = f32::NAN;
        assert_eq!(Vbap::new(&bad).err(), Some(VbapError::BadDirection(2)));
        // Speakers all to the front and one up: the hull leaves the
        // listener outside.
        let mut front = SurroundFormat::S512.speakers()[..3].to_vec();
        front[0].azimuth = 10.0;
        front[1].azimuth = -10.0;
        front.push(Speaker {
            elevation: 20.0,
            ..front[2]
        });
        assert_eq!(Vbap::new(&front).err(), Some(VbapError::NotAround));
        let many = [base[0]; MAX_SPEAKERS + 1];
        assert_eq!(Vbap::new(&many).err(), Some(VbapError::TooMany));
        // Nonsense directions: finite, one speaker's worth of power.
        for f in [SurroundFormat::S51, SurroundFormat::S714] {
            for (az, el) in [
                (f32::NAN, 0.0),
                (f32::INFINITY, f32::NAN),
                (1e9, -40.0),
                (-725.0, 400.0),
            ] {
                let g = gains(f, az, el);
                assert!(g.iter().all(|v| v.is_finite() && *v >= 0.0), "{g:?}");
                assert!(close(power(&g), 1.0));
            }
        }
        // Fewer outputs than channels: what fits, never past the end.
        let mut short = [0.0; 2];
        Vbap::of(SurroundFormat::S51).gains(110.0, 0.0, &mut short);
        assert_eq!(short, [0.0, 0.0]);
    }

    #[test]
    fn directions_round_trip() {
        for (az, el) in [
            (0.0f32, 0.0f32),
            (30.0, 0.0),
            (-110.0, 0.0),
            (135.0, 30.0),
            (-90.0, 45.0),
        ] {
            let (a, e) = direction_of(unit(az, el));
            assert!(
                (a - az).abs() < 1e-3 && (e - el).abs() < 1e-3,
                "{az} {el}: {a} {e}"
            );
        }
        // Left is −x, front +y, up +z.
        let l = unit(90.0, 0.0);
        assert!(close(l[0], -1.0) && close(l[1], 0.0));
        assert!(close(unit(0.0, 90.0)[2], 1.0));
    }
}
