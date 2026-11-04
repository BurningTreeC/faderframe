//! Ambisonics: a sound field as spherical harmonics, up to third order.
//!
//! FaderFrame's ambisonic channels are AmbiX: the harmonics in ACN order
//! (channel `k = n² + n + m` for order `n`, degree `m`) with SN3D
//! normalisation — a plane wave of unit amplitude has `W = 1` and every
//! order's channels together the same power. Directions are those of the
//! surround panner and the speaker table (azimuth in degrees, 0 ahead and
//! positive to the left; elevation above ear level); the ambisonic frame
//! is `x` front, `y` left, `z` up, as AmbiX defines it.
//!
//! * **Encoding** ([`harmonics`], [`encode`]): a source in a direction is
//!   the harmonics there. [`spread_gains`] widens it by tapering the
//!   higher orders (at a spread of one only `W` is left: every direction
//!   alike), keeping the power a max-rE decode plays.
//! * **Rotation** ([`Rotator`]): a field turned by an [`Orientation`]
//!   (a unit quaternion; yaw, pitch and roll) is a block-diagonal matrix
//!   per order. It is found by sampling: the harmonics of a fixed set of
//!   probe directions, turned, against the pseudo-inverse of the unturned
//!   ones (computed once, off the audio thread) — exact for any rotation
//!   and consistent with [`harmonics`] by construction; making a matrix
//!   allocates nothing.
//! * **Decoding** ([`Decoder`]): AllRAD (F. Zotter and M. Frank,
//!   "All-Round Ambisonic Panning and Decoding", JAES 60(10), 2012) — the
//!   field sampled, max-rE weighted, at a dense nearly uniform grid of
//!   virtual speakers, each of them panned onto the real speakers by VBAP
//!   ([`crate::vbap`]), scaled so a plane wave plays at unit power on
//!   average over the sphere. Below ear level a layout without speakers
//!   there plays at ear level (as VBAP pans there). [`Decoder::sampling`]
//!   is a fallback that needs no geometry.
//! * **FuMa** ([`FUMA_TO_AMBIX`]): first-order B-format in Furse–Malham
//!   order and normalisation (`W X Y Z`, `W` at −3 dB) to AmbiX.

use crate::surround::{MAX_SPEAKERS, Speaker};
use crate::vbap::Vbap;
use serde::{Deserialize, Serialize};

/// The highest order FaderFrame carries.
pub const MAX_ORDER: usize = 3;
/// Channels of a third-order field.
pub const MAX_CHANNELS: usize = (MAX_ORDER + 1) * (MAX_ORDER + 1);

/// An ambisonic order (its channels in AmbiX order and normalisation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AmbiOrder {
    First,
    Second,
    Third,
}

impl AmbiOrder {
    pub const ALL: [AmbiOrder; 3] = [AmbiOrder::First, AmbiOrder::Second, AmbiOrder::Third];

    /// Its order `N`.
    pub const fn order(self) -> usize {
        match self {
            AmbiOrder::First => 1,
            AmbiOrder::Second => 2,
            AmbiOrder::Third => 3,
        }
    }

    /// Its channels, `(N + 1)²`.
    pub const fn channels(self) -> usize {
        let n = self.order() + 1;
        n * n
    }

    pub fn from_order(n: usize) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.order() == n)
    }

    /// The order a channel count is (4, 9 or 16).
    pub fn from_channels(n: usize) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.channels() == n)
    }

    /// "1st-order ambisonics" …
    pub fn name(self) -> &'static str {
        match self {
            AmbiOrder::First => "1st-order ambisonics",
            AmbiOrder::Second => "2nd-order ambisonics",
            AmbiOrder::Third => "3rd-order ambisonics",
        }
    }

    /// "AmbiX 1" … (where little room is left).
    pub fn short_name(self) -> &'static str {
        match self {
            AmbiOrder::First => "AmbiX 1",
            AmbiOrder::Second => "AmbiX 2",
            AmbiOrder::Third => "AmbiX 3",
        }
    }

    /// Its id in scripts and menus: `ambix1` …
    pub fn id(self) -> &'static str {
        match self {
            AmbiOrder::First => "ambix1",
            AmbiOrder::Second => "ambix2",
            AmbiOrder::Third => "ambix3",
        }
    }
}

impl Serialize for AmbiOrder {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.order() as u8)
    }
}

impl<'de> Deserialize<'de> for AmbiOrder {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let n = u8::deserialize(deserializer)?;
        AmbiOrder::from_order(usize::from(n))
            .ok_or_else(|| serde::de::Error::custom(format!("no ambisonic order {n}")))
    }
}

/// The order of ACN channel `k`.
pub const fn order_of(k: usize) -> usize {
    match k {
        0 => 0,
        1..=3 => 1,
        4..=8 => 2,
        _ => 3,
    }
}

/// The unit vector of a direction in the ambisonic frame (`x` front, `y`
/// left, `z` up; degrees, azimuth positive to the left). NaN: ahead.
pub fn direction(azimuth: f32, elevation: f32) -> [f32; 3] {
    let a = if azimuth.is_finite() { azimuth } else { 0.0 };
    let e = if elevation.is_finite() {
        elevation.clamp(-90.0, 90.0)
    } else {
        0.0
    };
    let (sa, ca) = a.to_radians().sin_cos();
    let (se, ce) = e.to_radians().sin_cos();
    [ca * ce, sa * ce, se]
}

/// The direction of a vector in the ambisonic frame: `(azimuth,
/// elevation)` in degrees.
pub fn angles(v: [f32; 3]) -> (f32, f32) {
    let h = (v[0] * v[0] + v[1] * v[1]).sqrt();
    let (a, e) = (v[1].atan2(v[0]).to_degrees(), v[2].atan2(h).to_degrees());
    (
        if a.is_finite() { a } else { 0.0 },
        if e.is_finite() { e } else { 0.0 },
    )
}

/// Every harmonic up to third order at the unit vector `v` (ambisonic
/// frame), SN3D, in ACN order.
pub fn harmonics(v: [f32; 3]) -> [f32; MAX_CHANNELS] {
    let [x, y, z] = v;
    let s3 = 3f32.sqrt();
    let s15 = 15f32.sqrt();
    let s5_8 = (5.0f32 / 8.0).sqrt();
    let s3_8 = (3.0f32 / 8.0).sqrt();
    let (xx, yy, zz) = (x * x, y * y, z * z);
    [
        1.0,
        y,
        z,
        x,
        s3 * x * y,
        s3 * y * z,
        0.5 * (3.0 * zz - 1.0),
        s3 * x * z,
        0.5 * s3 * (xx - yy),
        s5_8 * y * (3.0 * xx - yy),
        s15 * x * y * z,
        s3_8 * y * (5.0 * zz - 1.0),
        0.5 * z * (5.0 * zz - 3.0),
        s3_8 * x * (5.0 * zz - 1.0),
        0.5 * s15 * z * (xx - yy),
        s5_8 * x * (xx - 3.0 * yy),
    ]
}

/// A plane wave from (`azimuth`, `elevation`) as `order`'s channels
/// (into the first `order.channels()` of `out`).
pub fn encode(order: AmbiOrder, azimuth: f32, elevation: f32, out: &mut [f32]) {
    let h = harmonics(direction(azimuth, elevation));
    for (o, v) in out.iter_mut().zip(h).take(order.channels()) {
        *o = v;
    }
}

/// The Legendre polynomial `Pₙ(x)` (`n` ≤ 3).
fn legendre(n: usize, x: f32) -> f32 {
    match n {
        0 => 1.0,
        1 => x,
        2 => 0.5 * (3.0 * x * x - 1.0),
        _ => 0.5 * x * (5.0 * x * x - 3.0),
    }
}

/// The max-rE weights of each order up to `order` (zeros beyond): `Pₙ(rE)`
/// with `rE` the largest root of `P_{N+1}` — the energy vector as long as
/// it gets, the side lobes as small (Zotter and Frank).
pub fn max_re(order: AmbiOrder) -> [f32; MAX_ORDER + 1] {
    let re = match order {
        AmbiOrder::First => 0.577_350_26,
        AmbiOrder::Second => 0.774_596_7,
        AmbiOrder::Third => 0.861_136_3,
    };
    std::array::from_fn(|n| {
        if n <= order.order() {
            legendre(n, re)
        } else {
            0.0
        }
    })
}

/// The per-order gains of a source widened by `spread` (0 a point … 1
/// every direction alike): order `n` tapered by `(1 − spread)ⁿ`, all
/// scaled so a max-rE decode plays the same power as for a point
/// (`Σ (2n + 1) aₙ² tₙ²` kept). NaN: a point.
pub fn spread_gains(order: AmbiOrder, spread: f32) -> [f32; MAX_ORDER + 1] {
    let s = if spread.is_nan() {
        0.0
    } else {
        spread.clamp(0.0, 1.0)
    };
    let a = max_re(order);
    let mut t = [0.0f32; MAX_ORDER + 1];
    for (n, v) in t.iter_mut().enumerate().take(order.order() + 1) {
        *v = (1.0 - s).powi(n as i32);
    }
    let power = |t: &[f32; MAX_ORDER + 1]| -> f32 {
        (0..=order.order())
            .map(|n| (2 * n + 1) as f32 * a[n] * a[n] * t[n] * t[n])
            .sum()
    };
    let point = power(&[1.0; MAX_ORDER + 1]);
    let now = power(&t).max(1e-12);
    let k = (point / now).sqrt();
    t.map(|v| v * k)
}

/// The distance gain and the spread it adds: beyond the reference
/// distance (1) the level falls as `1 / d`; inside it the source opens up
/// towards every direction alike (spread `1 − d`) at unity. NaN: 1.
pub fn distance_effect(distance: f32) -> (f32, f32) {
    let d = if distance.is_nan() {
        1.0
    } else {
        distance.max(0.0)
    };
    if d >= 1.0 {
        (1.0 / d, 0.0)
    } else {
        (1.0, 1.0 - d)
    }
}

/// First-order B-format in Furse–Malham order and normalisation (`W X Y
/// Z`, `W` at −3 dB) to AmbiX (`W Y Z X`, SN3D): `ambix[r] = Σ m[r][c] ·
/// fuma[c]`.
pub const FUMA_TO_AMBIX: [[f32; 4]; 4] = [
    [std::f32::consts::SQRT_2, 0.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
    [0.0, 1.0, 0.0, 0.0],
];

/// An orientation (a listener's head, a scene's turn) as a unit
/// quaternion in the ambisonic frame (`x` front, `y` left, `z` up).
///
/// Yaw, pitch and roll (degrees) compose as `Rz(yaw) · Ry(−pitch) ·
/// Rx(roll)` (turned first, then tilted, then rolled): yaw positive to the
/// left (as azimuths), pitch positive up, roll positive towards the right
/// shoulder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Orientation {
    pub w: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Default for Orientation {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Orientation {
    pub const IDENTITY: Orientation = Orientation {
        w: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// From a quaternion's components (normalised; zero or NaN: none).
    pub fn from_quaternion(w: f32, x: f32, y: f32, z: f32) -> Self {
        let n = (w * w + x * x + y * y + z * z).sqrt();
        if !n.is_finite() || n < 1e-9 {
            return Self::IDENTITY;
        }
        Self {
            w: w / n,
            x: x / n,
            y: y / n,
            z: z / n,
        }
    }

    /// A turn of `angle` degrees about a unit `axis` (right-handed).
    fn about(axis: [f32; 3], angle: f32) -> Self {
        let h = if angle.is_finite() {
            angle.to_radians() * 0.5
        } else {
            0.0
        };
        let (s, c) = h.sin_cos();
        Self {
            w: c,
            x: axis[0] * s,
            y: axis[1] * s,
            z: axis[2] * s,
        }
    }

    /// From yaw, pitch and roll in degrees (see the type).
    pub fn from_ypr(yaw: f32, pitch: f32, roll: f32) -> Self {
        Self::about([0.0, 0.0, 1.0], yaw)
            .then(Self::about([0.0, 1.0, 0.0], -pitch))
            .then(Self::about([1.0, 0.0, 0.0], roll))
    }

    /// Its yaw, pitch and roll in degrees (pitch ±90° at most).
    pub fn ypr(&self) -> (f32, f32, f32) {
        let m = self.matrix();
        let yaw = m[1][0].atan2(m[0][0]).to_degrees();
        let pitch = m[2][0].clamp(-1.0, 1.0).asin().to_degrees();
        let roll = m[2][1].atan2(m[2][2]).to_degrees();
        (yaw, pitch, roll)
    }

    /// `self` then `other` in its own frame (`self · other`).
    pub fn then(self, o: Orientation) -> Self {
        Self {
            w: self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
            x: self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            y: self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            z: self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
        }
    }

    /// The opposite turn.
    pub fn inverse(self) -> Self {
        Self {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    /// Its rotation matrix (`v' = M v`).
    pub fn matrix(&self) -> [[f32; 3]; 3] {
        let Orientation { w, x, y, z } = *self;
        [
            [
                1.0 - 2.0 * (y * y + z * z),
                2.0 * (x * y - w * z),
                2.0 * (x * z + w * y),
            ],
            [
                2.0 * (x * y + w * z),
                1.0 - 2.0 * (x * x + z * z),
                2.0 * (y * z - w * x),
            ],
            [
                2.0 * (x * z - w * y),
                2.0 * (y * z + w * x),
                1.0 - 2.0 * (x * x + y * y),
            ],
        ]
    }

    /// `v` turned by it.
    pub fn rotate(&self, v: [f32; 3]) -> [f32; 3] {
        let m = self.matrix();
        std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
    }

    /// The angle between two orientations (degrees).
    pub fn angle_to(&self, o: &Orientation) -> f32 {
        // From the turn between them (precise near zero, where acos is
        // not).
        let r = self.inverse().then(*o);
        let v = (r.x * r.x + r.y * r.y + r.z * r.z).sqrt();
        2.0 * v.atan2(r.w.abs()).to_degrees()
    }

    /// Whether it turns nothing (within a thousandth of a degree).
    pub fn is_identity(&self) -> bool {
        self.angle_to(&Self::IDENTITY) < 1e-3
    }

    /// `t` (0…1) of the way to `to`, along the shortest arc.
    pub fn slerp(self, to: Orientation, t: f32) -> Self {
        let t = if t.is_nan() { 1.0 } else { t.clamp(0.0, 1.0) };
        let mut d = self.w * to.w + self.x * to.x + self.y * to.y + self.z * to.z;
        let to = if d < 0.0 {
            d = -d;
            to.inverse_sign()
        } else {
            to
        };
        if d > 0.9995 {
            // Nearly the same: a normalised straight line.
            return Self::from_quaternion(
                self.w + (to.w - self.w) * t,
                self.x + (to.x - self.x) * t,
                self.y + (to.y - self.y) * t,
                self.z + (to.z - self.z) * t,
            );
        }
        let theta = d.min(1.0).acos();
        let s = theta.sin();
        let (a, b) = (((1.0 - t) * theta).sin() / s, (t * theta).sin() / s);
        Self::from_quaternion(
            self.w * a + to.w * b,
            self.x * a + to.x * b,
            self.y * a + to.y * b,
            self.z * a + to.z * b,
        )
    }

    /// The same orientation with every component negated.
    fn inverse_sign(self) -> Self {
        Self {
            w: -self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }
}

/// Probe directions the rotation matrices are sampled at.
const PROBES: usize = 24;

/// The probes: a Fibonacci spiral over the sphere (nearly uniform).
fn probes() -> [[f32; 3]; PROBES] {
    std::array::from_fn(|i| fibonacci(i, PROBES))
}

/// Point `i` of an `n`-point Fibonacci spiral on the unit sphere.
fn fibonacci(i: usize, n: usize) -> [f32; 3] {
    let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
    let z = 1.0 - (2.0 * i as f64 + 1.0) / n as f64;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let a = golden * i as f64;
    [(r * a.cos()) as f32, (r * a.sin()) as f32, z as f32]
}

/// Inverts the leading `m` × `m` of `a` (Gauss–Jordan with pivoting;
/// `None` when singular).
fn invert(a: &[[f64; 7]; 7], m: usize) -> Option<[[f64; 7]; 7]> {
    let mut a = *a;
    let mut inv = [[0.0f64; 7]; 7];
    for (i, row) in inv.iter_mut().enumerate().take(m) {
        row[i] = 1.0;
    }
    for col in 0..m {
        let pivot = (col..m).max_by(|&p, &q| a[p][col].abs().total_cmp(&a[q][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        inv.swap(col, pivot);
        let d = a[col][col];
        for j in 0..m {
            a[col][j] /= d;
            inv[col][j] /= d;
        }
        for r in 0..m {
            if r != col {
                let f = a[r][col];
                if f != 0.0 {
                    for j in 0..m {
                        a[r][j] -= f * a[col][j];
                        inv[r][j] -= f * inv[col][j];
                    }
                }
            }
        }
    }
    Some(inv)
}

/// Turns sound fields: makes the rotation matrix of an [`Orientation`]
/// for each order (see the module docs). Made off the audio thread;
/// [`Rotator::matrix`] then allocates nothing.
#[derive(Clone, Debug)]
pub struct Rotator {
    probes: [[f32; 3]; PROBES],
    /// Per order `n`: the probes' harmonics of that order, pseudo-inverted
    /// and transposed (`PROBES` × `2n + 1`).
    solve: [[[f32; 7]; PROBES]; MAX_ORDER + 1],
}

impl Default for Rotator {
    fn default() -> Self {
        Self::new()
    }
}

impl Rotator {
    pub fn new() -> Self {
        let probes = probes();
        let h: Vec<[f32; MAX_CHANNELS]> = probes.iter().map(|p| harmonics(*p)).collect();
        let mut solve = [[[0.0f32; 7]; PROBES]; MAX_ORDER + 1];
        for (n, block) in solve.iter_mut().enumerate() {
            let (first, m) = (n * n, 2 * n + 1);
            // AᵀA and its inverse (A: probes × the order's channels).
            let mut ata = [[0.0f64; 7]; 7];
            for (i, row) in ata.iter_mut().enumerate().take(m) {
                for (j, v) in row.iter_mut().enumerate().take(m) {
                    *v = h
                        .iter()
                        .map(|r| f64::from(r[first + i]) * f64::from(r[first + j]))
                        .sum();
                }
            }
            let Some(inv) = invert(&ata, m) else {
                continue;
            };
            // G = A (AᵀA)⁻¹.
            for (p, g) in block.iter_mut().enumerate() {
                for (j, v) in g.iter_mut().enumerate().take(m) {
                    *v = (0..m)
                        .map(|i| f64::from(h[p][first + i]) * inv[i][j])
                        .sum::<f64>() as f32;
                }
            }
        }
        Self { probes, solve }
    }

    /// Into `out`: the matrix that turns a field of `order` by `turn` —
    /// `b' = M b`, so a plane wave from `θ` comes from `turn(θ)` after it
    /// (block-diagonal per order, zeros beyond `order`).
    pub fn matrix(
        &self,
        order: AmbiOrder,
        turn: &Orientation,
        out: &mut [[f32; MAX_CHANNELS]; MAX_CHANNELS],
    ) {
        for row in out.iter_mut() {
            row.fill(0.0);
        }
        out[0][0] = 1.0;
        let mut turned = [[0.0f32; MAX_CHANNELS]; PROBES];
        for (t, p) in turned.iter_mut().zip(&self.probes) {
            *t = harmonics(turn.rotate(*p));
        }
        for n in 1..=order.order() {
            let (first, m) = (n * n, 2 * n + 1);
            let g = &self.solve[n];
            for i in 0..m {
                for j in 0..m {
                    let mut acc = 0.0f32;
                    for p in 0..PROBES {
                        acc += turned[p][first + i] * g[p][j];
                    }
                    out[first + i][first + j] = acc;
                }
            }
        }
    }
}

/// Virtual speakers an AllRAD decoder samples the field at.
const GRID: usize = 1200;

/// A decoder from a field of one order to a set of speakers: each
/// speaker's gain from each channel (SN3D input).
#[derive(Clone, Debug, PartialEq)]
pub struct Decoder {
    order: AmbiOrder,
    speakers: usize,
    gains: [[f32; MAX_CHANNELS]; MAX_SPEAKERS],
}

impl Decoder {
    /// AllRAD onto `speakers` (channel order; the LFE gets nothing) with
    /// their VBAP geometry `vbap`. Off the audio thread (it samples a
    /// grid of [`GRID`] directions).
    pub fn allrad(order: AmbiOrder, speakers: &[Speaker], vbap: &Vbap) -> Self {
        let channels = order.channels();
        let count = speakers.len().min(MAX_SPEAKERS);
        let a = max_re(order);
        let mut gains = [[0.0f32; MAX_CHANNELS]; MAX_SPEAKERS];
        let mut g = [0.0f32; MAX_SPEAKERS];
        for i in 0..GRID {
            let v = fibonacci(i, GRID);
            let (az, el) = angles(v);
            vbap.gains(az, el, &mut g[..count]);
            let h = harmonics(v);
            for (s, row) in gains.iter_mut().enumerate().take(count) {
                if g[s] == 0.0 {
                    continue;
                }
                for (k, v) in row.iter_mut().enumerate().take(channels) {
                    let n = order_of(k);
                    *v += g[s] * a[n] * (2 * n + 1) as f32 * h[k];
                }
            }
        }
        let mut d = Self {
            order,
            speakers: count,
            gains,
        };
        for row in d.gains.iter_mut() {
            for v in row.iter_mut() {
                *v /= GRID as f32;
            }
        }
        d.normalise();
        d
    }

    /// Without geometry (allocation-free; where no AllRAD decoder was
    /// made): each main speaker samples the field at its own direction,
    /// max-rE weighted, scaled to unit power on average. Fine for regular
    /// layouts, uneven for others.
    pub fn sampling(order: AmbiOrder, speakers: &[Speaker]) -> Self {
        let channels = order.channels();
        let count = speakers.len().min(MAX_SPEAKERS);
        let a = max_re(order);
        let mut gains = [[0.0f32; MAX_CHANNELS]; MAX_SPEAKERS];
        for (row, s) in gains.iter_mut().zip(speakers).take(count) {
            if s.lfe {
                continue;
            }
            let h = harmonics(direction(s.azimuth, s.elevation));
            for (k, v) in row.iter_mut().enumerate().take(channels) {
                let n = order_of(k);
                *v = a[n] * (2 * n + 1) as f32 * h[k];
            }
        }
        let mut d = Self {
            order,
            speakers: count,
            gains,
        };
        d.normalise();
        d
    }

    /// Scaled so a plane wave plays at unit power on average over the
    /// sphere.
    fn normalise(&mut self) {
        const CHECK: usize = 240;
        let mut total = 0.0f64;
        for i in 0..CHECK {
            let h = harmonics(fibonacci(i, CHECK));
            for row in self.gains.iter().take(self.speakers) {
                let s: f32 = row.iter().zip(&h).map(|(g, y)| g * y).sum();
                total += f64::from(s * s);
            }
        }
        let mean = total / CHECK as f64;
        if mean > 1e-12 {
            let k = (1.0 / mean).sqrt() as f32;
            for row in self.gains.iter_mut() {
                for v in row.iter_mut() {
                    *v *= k;
                }
            }
        }
    }

    pub fn order(&self) -> AmbiOrder {
        self.order
    }

    /// How many speakers it feeds.
    pub fn speakers(&self) -> usize {
        self.speakers
    }

    /// Each speaker's gain from each channel.
    pub fn gains(&self) -> &[[f32; MAX_CHANNELS]; MAX_SPEAKERS] {
        &self.gains
    }

    /// The speakers' gains for a plane wave from (`azimuth`, `elevation`)
    /// (into `out`).
    pub fn plane_wave(&self, azimuth: f32, elevation: f32, out: &mut [f32]) {
        let h = harmonics(direction(azimuth, elevation));
        let channels = self.order.channels();
        for (o, row) in out.iter_mut().zip(&self.gains).take(self.speakers) {
            *o = row.iter().zip(&h).take(channels).map(|(g, y)| g * y).sum();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SurroundFormat;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    /// The harmonics agree with their definition in angles (SN3D, ACN) at
    /// a few directions, and the first order is the direction itself.
    #[test]
    fn the_harmonics_are_ambix() {
        for (az, el) in [
            (0.0f32, 0.0f32),
            (30.0, 10.0),
            (-120.0, 45.0),
            (170.0, -60.0),
        ] {
            let h = harmonics(direction(az, el));
            let (a, e) = (az.to_radians(), el.to_radians());
            assert!(close(h[0], 1.0, 1e-6));
            assert!(close(h[1], a.sin() * e.cos(), 1e-5));
            assert!(close(h[2], e.sin(), 1e-5));
            assert!(close(h[3], a.cos() * e.cos(), 1e-5));
            // Order 2, m = −2 and 0; order 3, m = 3.
            assert!(close(
                h[4],
                3f32.sqrt() / 2.0 * e.cos().powi(2) * (2.0 * a).sin(),
                1e-5
            ));
            assert!(close(h[6], 0.5 * (3.0 * e.sin().powi(2) - 1.0), 1e-5));
            assert!(close(
                h[15],
                (5.0f32 / 8.0).sqrt() * e.cos().powi(3) * (3.0 * a).cos(),
                1e-5
            ));
        }
        // Left is +y, up is +z.
        let left = harmonics(direction(90.0, 0.0));
        assert!(close(left[1], 1.0, 1e-6) && close(left[3], 0.0, 1e-6));
    }

    /// SN3D: each order's harmonics together have unit power in every
    /// direction (the addition theorem), so every order carries a plane
    /// wave at the same level.
    #[test]
    fn every_order_has_unit_power_everywhere() {
        for i in 0..50 {
            let h = harmonics(fibonacci(i, 50));
            for n in 0..=MAX_ORDER {
                let p: f32 = (n * n..(n + 1) * (n + 1)).map(|k| h[k] * h[k]).sum();
                assert!(close(p, 1.0, 1e-5), "order {n}: {p}");
            }
        }
    }

    /// Orthogonality over the sphere: the harmonics' mean products are
    /// `δ / (2n + 1)` (SN3D).
    #[test]
    fn the_harmonics_are_orthogonal() {
        let n = 4000;
        let mut gram = [[0.0f64; MAX_CHANNELS]; MAX_CHANNELS];
        for i in 0..n {
            let h = harmonics(fibonacci(i, n));
            for a in 0..MAX_CHANNELS {
                for b in 0..MAX_CHANNELS {
                    gram[a][b] += f64::from(h[a] * h[b]) / n as f64;
                }
            }
        }
        for (a, row) in gram.iter().enumerate() {
            for (b, v) in row.iter().enumerate() {
                let want = if a == b {
                    1.0 / (2 * order_of(a) + 1) as f64
                } else {
                    0.0
                };
                assert!((v - want).abs() < 2e-3, "{a} {b}: {v}");
            }
        }
    }

    #[test]
    fn orders_and_their_channels() {
        assert_eq!(AmbiOrder::First.channels(), 4);
        assert_eq!(AmbiOrder::Second.channels(), 9);
        assert_eq!(AmbiOrder::Third.channels(), MAX_CHANNELS);
        assert_eq!(AmbiOrder::from_channels(9), Some(AmbiOrder::Second));
        assert_eq!(AmbiOrder::from_channels(6), None);
        assert_eq!((0..16).map(order_of).max(), Some(3));
        let json = serde_json::to_string(&AmbiOrder::Third).unwrap();
        assert_eq!(json, "3");
        assert_eq!(
            serde_json::from_str::<AmbiOrder>("2").unwrap(),
            AmbiOrder::Second
        );
        assert!(serde_json::from_str::<AmbiOrder>("4").is_err());
    }

    /// Yaw, pitch and roll go where their names say and come back.
    #[test]
    fn orientations_turn_as_named() {
        let front = [1.0, 0.0, 0.0];
        // Turned 90° left: the front is on the left.
        let v = Orientation::from_ypr(90.0, 0.0, 0.0).rotate(front);
        assert!(close(v[1], 1.0, 1e-5), "{v:?}");
        // Looking up 30°: the front rises.
        let v = Orientation::from_ypr(0.0, 30.0, 0.0).rotate(front);
        assert!(close(angles(v).1, 30.0, 1e-3), "{v:?}");
        // Rolled to the right: the left ear goes up.
        let v = Orientation::from_ypr(0.0, 0.0, 20.0).rotate([0.0, 1.0, 0.0]);
        assert!(v[2] > 0.3, "{v:?}");
        for (y, p, r) in [(10.0, 20.0, 30.0), (-150.0, -40.0, 5.0), (90.0, 0.0, -90.0)] {
            let o = Orientation::from_ypr(y, p, r);
            let (y2, p2, r2) = o.ypr();
            assert!(
                close(y, y2, 1e-2) && close(p, p2, 1e-2) && close(r, r2, 1e-2),
                "{y} {p} {r} → {y2} {p2} {r2}"
            );
            let back = o.then(o.inverse());
            assert!(back.is_identity());
        }
        assert!(Orientation::from_quaternion(0.0, 0.0, 0.0, 0.0).is_identity());
        assert!(Orientation::from_quaternion(f32::NAN, 1.0, 0.0, 0.0).is_identity());
    }

    /// Slerp goes the short way and ends where it should.
    #[test]
    fn slerp_moves_along_the_shortest_arc() {
        let a = Orientation::from_ypr(170.0, 0.0, 0.0);
        let b = Orientation::from_ypr(-170.0, 0.0, 0.0);
        let half = a.slerp(b, 0.5);
        assert!(close(half.ypr().0.abs(), 180.0, 1e-2), "{:?}", half.ypr());
        assert!(a.slerp(b, 1.0).angle_to(&b) < 1e-3);
        assert!(a.slerp(b, 0.0).angle_to(&a) < 1e-3);
        assert!(close(a.angle_to(&b), 20.0, 1e-2));
    }

    /// A plane wave turned is the plane wave from the turned direction,
    /// for every order and arbitrary turns; turns compose, and the matrix
    /// keeps the field's power (it is orthogonal per order).
    #[test]
    fn a_turned_plane_wave_comes_from_the_turned_direction() {
        let rot = Rotator::new();
        let mut m = [[0.0f32; MAX_CHANNELS]; MAX_CHANNELS];
        for (y, p, r) in [
            (30.0, 0.0, 0.0),
            (0.0, 45.0, 0.0),
            (0.0, 0.0, 60.0),
            (-123.0, 17.0, 71.0),
            (179.0, -80.0, -30.0),
        ] {
            let turn = Orientation::from_ypr(y, p, r);
            for order in AmbiOrder::ALL {
                rot.matrix(order, &turn, &mut m);
                for (az, el) in [(0.0f32, 0.0f32), (77.0, 20.0), (-140.0, -35.0)] {
                    let b = harmonics(direction(az, el));
                    let want = harmonics(turn.rotate(direction(az, el)));
                    let c = order.channels();
                    for i in 0..c {
                        let got: f32 = (0..c).map(|j| m[i][j] * b[j]).sum();
                        assert!(
                            close(got, want[i], 2e-4),
                            "{order:?} ypr {y}/{p}/{r} at {az}/{el}: ch {i} {got} vs {}",
                            want[i]
                        );
                    }
                    let power =
                        |v: &dyn Fn(usize) -> f32| -> f32 { (0..c).map(|i| v(i) * v(i)).sum() };
                    let turned = power(&|i| (0..c).map(|j| m[i][j] * b[j]).sum());
                    assert!(close(turned, power(&|i| b[i]), 1e-3));
                }
                // Nothing leaks between orders or past the order.
                for (i, row) in m.iter().enumerate() {
                    for (j, v) in row.iter().enumerate() {
                        if order_of(i) != order_of(j) || i >= order.channels() {
                            assert_eq!(*v, 0.0);
                        }
                    }
                }
            }
        }
        // Two turns are their product.
        let (a, b) = (
            Orientation::from_ypr(40.0, 10.0, 0.0),
            Orientation::from_ypr(-15.0, 0.0, 25.0),
        );
        let (mut ma, mut mb, mut mab) = ([[0.0; 16]; 16], [[0.0; 16]; 16], [[0.0; 16]; 16]);
        rot.matrix(AmbiOrder::Third, &a, &mut ma);
        rot.matrix(AmbiOrder::Third, &b, &mut mb);
        rot.matrix(AmbiOrder::Third, &a.then(b), &mut mab);
        for i in 0..16 {
            for j in 0..16 {
                let prod: f32 = (0..16).map(|k| ma[i][k] * mb[k][j]).sum();
                assert!(close(prod, mab[i][j], 1e-4), "{i} {j}");
            }
        }
        // No turn: the identity.
        rot.matrix(AmbiOrder::Third, &Orientation::IDENTITY, &mut m);
        for (i, row) in m.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                assert!(close(*v, if i == j { 1.0 } else { 0.0 }, 1e-5));
            }
        }
    }

    /// A spread source keeps the power a max-rE decode plays, and at full
    /// spread only W is left.
    #[test]
    fn spread_tapers_the_orders_and_keeps_the_power() {
        for order in AmbiOrder::ALL {
            let a = max_re(order);
            let power = |t: [f32; 4]| -> f32 {
                (0..=order.order())
                    .map(|n| (2 * n + 1) as f32 * (a[n] * t[n]).powi(2))
                    .sum()
            };
            let point = spread_gains(order, 0.0);
            assert_eq!(point[..=order.order()], [1.0; 4][..=order.order()]);
            for s in [0.25, 0.5, 0.9, 1.0] {
                let t = spread_gains(order, s);
                assert!(close(power(t), power(point), 1e-4), "{order:?} {s}");
                for n in 1..=order.order() {
                    assert!(t[n] / t[0] <= point[n] / point[0] + 1e-6);
                }
            }
            let full = spread_gains(order, 1.0);
            assert!(full[1..].iter().all(|v| *v == 0.0));
        }
        assert_eq!(spread_gains(AmbiOrder::First, f32::NAN)[1], 1.0);
        assert_eq!(distance_effect(2.0), (0.5, 0.0));
        assert_eq!(distance_effect(0.25), (1.0, 0.75));
        assert_eq!(distance_effect(f32::NAN), (1.0, 0.0));
    }

    /// AllRAD onto a speaker layout: a plane wave from a speaker plays
    /// loudest on it, the power stays near one round the ring, left
    /// stays left, and the LFE gets nothing.
    #[test]
    fn allrad_plays_a_plane_wave_where_it_comes_from() {
        for f in [SurroundFormat::S51, SurroundFormat::S714] {
            let sp = f.speakers();
            let vbap = Vbap::of(f);
            for order in AmbiOrder::ALL {
                let d = Decoder::allrad(order, sp, &vbap);
                let mut g = vec![0.0f32; sp.len()];
                for (i, s) in sp.iter().enumerate() {
                    if s.lfe {
                        assert!(d.gains()[i].iter().all(|v| *v == 0.0));
                        continue;
                    }
                    if order != AmbiOrder::Third {
                        // Lower orders are too broad to pick out a
                        // front speaker among its close neighbours.
                        continue;
                    }
                    d.plane_wave(s.azimuth, s.elevation, &mut g);
                    let loudest = (0..sp.len())
                        .max_by(|a, b| g[*a].total_cmp(&g[*b]))
                        .unwrap();
                    assert_eq!(loudest, i, "{} {order:?} {}: {g:?}", f.name(), s.label);
                }
                // Power round the horizon within ±2 dB of one.
                for az in (0..360).step_by(15) {
                    d.plane_wave(az as f32, 0.0, &mut g);
                    let p: f32 = g.iter().map(|v| v * v).sum();
                    let db = 10.0 * p.log10();
                    assert!(
                        db.abs() < 2.5,
                        "{} {order:?} at {az}°: {db:.2} dB",
                        f.name()
                    );
                }
                // Left on the left.
                d.plane_wave(70.0, 0.0, &mut g);
                let (l, r) = (g[0], g[1]);
                assert!(l > r, "{} {order:?}: L {l} R {r}", f.name());
            }
        }
    }

    /// Into stereo the decode keeps left and right and the front centred;
    /// into mono it is the W channel.
    #[test]
    fn allrad_into_stereo_and_mono() {
        let st = crate::surround::speakers_of(crate::ChannelLayout::Stereo).unwrap();
        let vbap = Vbap::new(st).unwrap();
        let d = Decoder::allrad(AmbiOrder::First, st, &vbap);
        let mut g = [0.0f32; 2];
        d.plane_wave(0.0, 0.0, &mut g);
        assert!(close(g[0], g[1], 1e-3), "{g:?}");
        d.plane_wave(60.0, 0.0, &mut g);
        assert!(g[0] > 2.0 * g[1].abs(), "{g:?}");
        let mono = crate::surround::speakers_of(crate::ChannelLayout::Mono).unwrap();
        let d = Decoder::allrad(AmbiOrder::Third, mono, &Vbap::new(mono).unwrap());
        let row = d.gains()[0];
        assert!(close(row[0], 1.0, 1e-3), "{row:?}");
        assert!(row[1..].iter().all(|v| v.abs() < 1e-2), "{row:?}");
    }

    /// The fallback without geometry still places a speaker's own
    /// direction on it.
    #[test]
    fn the_sampling_decoder_is_a_usable_fallback() {
        let sp = SurroundFormat::S714.speakers();
        let d = Decoder::sampling(AmbiOrder::Third, sp);
        let mut g = vec![0.0f32; sp.len()];
        d.plane_wave(30.0, 0.0, &mut g);
        let loudest = (0..sp.len())
            .max_by(|a, b| g[*a].total_cmp(&g[*b]))
            .unwrap();
        assert_eq!(sp[loudest].label, "L");
        assert!(d.gains()[3].iter().all(|v| *v == 0.0));
    }

    /// FuMa's first order becomes AmbiX: W up 3 dB, the axes reordered.
    #[test]
    fn fuma_becomes_ambix() {
        // A plane wave from the left in FuMa: W = 1/√2, X = 0, Y = 1, Z = 0.
        let fuma = [std::f32::consts::FRAC_1_SQRT_2, 0.0, 1.0, 0.0];
        let ambix: Vec<f32> = FUMA_TO_AMBIX
            .iter()
            .map(|r| r.iter().zip(&fuma).map(|(m, v)| m * v).sum())
            .collect();
        let want = harmonics(direction(90.0, 0.0));
        for k in 0..4 {
            assert!(close(ambix[k], want[k], 1e-6), "{ambix:?}");
        }
    }
}
