//! Following an object through a clip's frames: normalised
//! cross-correlation template tracking, deterministic (no learned model).
//!
//! The object is the box drawn on one frame (its key). Its picture, at a
//! level of an image pyramid where the box is about [`TEMPLATE`] pixels
//! across, is the template. On every later frame the template is looked
//! for around where the object should be by now (its last place plus its
//! speed), at three sizes (it may come nearer or go away), first on every
//! other position, then around the best one; the correlation peak is
//! refined to a fraction of a pixel by a parabola, and the size between
//! the three by another. While the match is good the template takes on a
//! little of what it found (the object turns, the light changes) and is
//! pulled a little back to the key's picture (so it never drifts off onto
//! the background); a poor match (the object hidden) moves on at the last
//! speed, slowing, and is marked so the user can correct it there.
//!
//! Corrections are keys too: a key starts the tracker afresh, and the path
//! up to it is bent onto it ([`bend`]: the error at the key spread over
//! the frames since the key before), so the path stays continuous.
//!
//! [`follow_in_file`] decodes a clip's frames in order at a small size and
//! feeds them through a tracker (on a job thread; GStreamer only there).

use crate::{Decoder, FrameIndex, Result, VideoError};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// The template's longest side at its pyramid level (pixels).
pub const TEMPLATE: f32 = 40.0;
/// The sizes tried around the last one (ratio per step).
const SCALE_STEP: f32 = 1.04;
/// A match at least this good updates the template.
const UPDATE_SCORE: f32 = 0.8;
/// A match below this is lost.
pub const LOST_SCORE: f32 = 0.45;
/// How much of what is found an update takes on, and how much the key's
/// picture pulls back.
const LEARN: f32 = 0.1;
const ANCHOR: f32 = 0.02;

/// A grey picture (luma 0…255).
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub width: usize,
    pub height: usize,
    pub px: Vec<u8>,
}

impl Gray {
    /// The luma (BT.709) of RGBA rows.
    pub fn from_rgba(width: usize, height: usize, rgba: &[u8]) -> Self {
        let px = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .take(width * height)
            .map(|p| {
                let y = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
                y.round().clamp(0.0, 255.0) as u8
            })
            .collect();
        Self { width, height, px }
    }

    /// Half the size (2×2 means).
    fn half(&self) -> Gray {
        let (w, h) = ((self.width / 2).max(1), (self.height / 2).max(1));
        let mut px = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let at = |xx: usize, yy: usize| {
                    let xx = xx.min(self.width - 1);
                    let yy = yy.min(self.height - 1);
                    self.px[yy * self.width + xx] as u32
                };
                let s = at(2 * x, 2 * y)
                    + at(2 * x + 1, 2 * y)
                    + at(2 * x, 2 * y + 1)
                    + at(2 * x + 1, 2 * y + 1);
                px[y * w + x] = ((s + 2) / 4) as u8;
            }
        }
        Gray {
            width: w,
            height: h,
            px,
        }
    }

    /// The value at `(u, v)` (pixel centres at +0.5; bilinear, edges
    /// held).
    fn sample(&self, u: f32, v: f32) -> f32 {
        let x = (u - 0.5).clamp(0.0, (self.width - 1) as f32);
        let y = (v - 0.5).clamp(0.0, (self.height - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let p = |xx: usize, yy: usize| self.px[yy * self.width + xx] as f32;
        let top = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
        let bottom = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

/// A box on a picture as shares of its width and height: the top-left
/// corner and the size.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Area {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn centre(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// The same size about another centre.
    pub fn at_centre(&self, cx: f32, cy: f32) -> Self {
        Self::new(cx - self.w / 2.0, cy - self.h / 2.0, self.w, self.h)
    }

    /// Whether it is a box at all and lies (mostly) on the picture.
    pub fn is_valid(&self) -> bool {
        let (cx, cy) = self.centre();
        self.w.is_finite()
            && self.h.is_finite()
            && self.w > 0.0
            && self.h > 0.0
            && (0.0..=1.0).contains(&cx)
            && (0.0..=1.0).contains(&cy)
    }

    /// Between `a` (t = 0) and `b` (t = 1).
    pub fn lerp(a: Area, b: Area, t: f32) -> Self {
        let l = |x: f32, y: f32| x + (y - x) * t;
        Self::new(l(a.x, b.x), l(a.y, b.y), l(a.w, b.w), l(a.h, b.h))
    }
}

/// Where the object was on one frame, and how well the template matched
/// there (1 = exactly; below [`LOST_SCORE`]: lost, guessed).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Followed {
    pub area: Area,
    pub score: f32,
}

impl Followed {
    pub fn lost(&self) -> bool {
        self.score < LOST_SCORE
    }
}

/// A tracker of one object, started on its key frame.
#[derive(Clone, Debug)]
pub struct Tracker {
    /// The pyramid level matched on (pictures halved this often).
    level: u32,
    /// The template (`tw` × `th` at the level) and the key's picture.
    template: Vec<f32>,
    key: Vec<f32>,
    tw: usize,
    th: usize,
    /// The box's size at the key (full-size pixels) and its scale since.
    base: (f32, f32),
    scale: f32,
    /// Its centre and speed (full-size pixels, per frame).
    centre: (f32, f32),
    speed: (f32, f32),
    /// The full picture's size.
    size: (usize, usize),
}

/// `frame` halved `level` times.
fn pyramid(frame: &Gray, level: u32) -> std::borrow::Cow<'_, Gray> {
    let mut g = std::borrow::Cow::Borrowed(frame);
    for _ in 0..level {
        g = std::borrow::Cow::Owned(g.half());
    }
    g
}

impl Tracker {
    /// Start on `frame` with the object in `area` (`None`: no box there).
    pub fn new(frame: &Gray, area: Area) -> Option<Self> {
        if !area.is_valid() || frame.width < 8 || frame.height < 8 {
            return None;
        }
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        let base = ((area.w * fw).max(4.0), (area.h * fh).max(4.0));
        let longest = base.0.max(base.1);
        let level = (longest / TEMPLATE).log2().ceil().max(0.0) as u32;
        let k = (1u32 << level) as f32;
        let tw = ((base.0 / k).round() as usize).max(6);
        let th = ((base.1 / k).round() as usize).max(6);
        let (cx, cy) = area.centre();
        let mut t = Self {
            level,
            template: Vec::new(),
            key: Vec::new(),
            tw,
            th,
            base,
            scale: 1.0,
            centre: (cx * fw, cy * fh),
            speed: (0.0, 0.0),
            size: (frame.width, frame.height),
        };
        let img = pyramid(frame, level);
        t.template = t.patch(&img, t.centre, 1.0);
        t.key = t.template.clone();
        Some(t)
    }

    /// The box now (shares of the picture).
    pub fn area(&self) -> Area {
        let (fw, fh) = (self.size.0 as f32, self.size.1 as f32);
        let (w, h) = (self.base.0 * self.scale, self.base.1 * self.scale);
        Area::new(
            (self.centre.0 - w / 2.0) / fw,
            (self.centre.1 - h / 2.0) / fh,
            w / fw,
            h / fh,
        )
    }

    /// The picture in the box about `centre` (full-size pixels) at `scale`,
    /// resampled to the template's size, from the level image `img`.
    fn patch(&self, img: &Gray, centre: (f32, f32), scale: f32) -> Vec<f32> {
        let k = (1u32 << self.level) as f32;
        let (cx, cy) = (centre.0 / k, centre.1 / k);
        let (bw, bh) = (self.tw as f32 * scale, self.th as f32 * scale);
        let mut out = Vec::with_capacity(self.tw * self.th);
        for j in 0..self.th {
            for i in 0..self.tw {
                let u = cx - bw / 2.0 + (i as f32 + 0.5) * bw / self.tw as f32;
                let v = cy - bh / 2.0 + (j as f32 + 0.5) * bh / self.th as f32;
                out.push(img.sample(u, v));
            }
        }
        out
    }

    /// The template resized to `w` × `h`, zero-mean and of unit energy
    /// (`None`: flat).
    fn resized(&self, w: usize, h: usize) -> Option<Vec<f32>> {
        let mut t = Vec::with_capacity(w * h);
        for j in 0..h {
            for i in 0..w {
                let u = (i as f32 + 0.5) * self.tw as f32 / w as f32 - 0.5;
                let v = (j as f32 + 0.5) * self.th as f32 / h as f32 - 0.5;
                let x = u.clamp(0.0, (self.tw - 1) as f32);
                let y = v.clamp(0.0, (self.th - 1) as f32);
                let (x0, y0) = (x.floor() as usize, y.floor() as usize);
                let (x1, y1) = ((x0 + 1).min(self.tw - 1), (y0 + 1).min(self.th - 1));
                let (fx, fy) = (x - x0 as f32, y - y0 as f32);
                let p = |xx: usize, yy: usize| self.template[yy * self.tw + xx];
                let top = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
                let bottom = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
                t.push(top * (1.0 - fy) + bottom * fy);
            }
        }
        let mean = t.iter().sum::<f32>() / t.len() as f32;
        let mut energy = 0.0f32;
        for v in &mut t {
            *v -= mean;
            energy += *v * *v;
        }
        if energy < 1e-3 {
            return None;
        }
        let k = 1.0 / energy.sqrt();
        for v in &mut t {
            *v *= k;
        }
        Some(t)
    }

    /// Follow the object onto the next frame.
    pub fn next(&mut self, frame: &Gray) -> Followed {
        if frame.width != self.size.0 || frame.height != self.size.1 {
            return Followed {
                area: self.area(),
                score: 0.0,
            };
        }
        let img = pyramid(frame, self.level);
        let k = (1u32 << self.level) as f32;
        let predicted = (self.centre.0 + self.speed.0, self.centre.1 + self.speed.1);
        // How far to look (level pixels): the box's size and the speed.
        let reach_full = (0.6 * (self.base.0.max(self.base.1)) * self.scale).max(12.0)
            + self.speed.0.abs().max(self.speed.1.abs());
        let reach = (reach_full / k).ceil() as i32;
        let mut best: Option<Match> = None;
        let mut by_scale = [f32::NEG_INFINITY; 3];
        for (n, s) in [self.scale / SCALE_STEP, self.scale, self.scale * SCALE_STEP]
            .into_iter()
            .enumerate()
        {
            let w = ((self.tw as f32 * s).round() as usize).max(4);
            let h = ((self.th as f32 * s).round() as usize).max(4);
            let Some(t) = self.resized(w, h) else {
                continue;
            };
            let cx = predicted.0 / k;
            let cy = predicted.1 / k;
            let x0 = (cx - w as f32 / 2.0).round() as i32;
            let y0 = (cy - h as f32 / 2.0).round() as i32;
            if let Some(m) = search(&img, &t, w, h, (x0, y0), reach) {
                by_scale[n] = m.score;
                let m = Match {
                    scale: w as f32 / self.tw as f32,
                    ..m
                };
                if best.as_ref().is_none_or(|b| m.score > b.score) {
                    best = Some(m);
                }
            }
        }
        let Some(m) = best else {
            return self.coast();
        };
        if m.score < LOST_SCORE {
            return self.coast();
        }
        // The size between the three tried (a parabola through their
        // scores), when the middle one is the best.
        let mut scale = m.scale;
        if by_scale.iter().all(|s| s.is_finite())
            && by_scale[1] >= by_scale[0]
            && by_scale[1] >= by_scale[2]
        {
            let d = by_scale[0] - 2.0 * by_scale[1] + by_scale[2];
            if d < -1e-6 {
                let delta = (0.5 * (by_scale[0] - by_scale[2]) / d).clamp(-1.0, 1.0);
                scale = self.scale * SCALE_STEP.powf(delta);
            }
        }
        let scale = scale.clamp(0.25, 4.0);
        let centre = (m.cx * k, m.cy * k);
        let moved = (centre.0 - self.centre.0, centre.1 - self.centre.1);
        self.speed = (
            0.6 * self.speed.0 + 0.4 * moved.0,
            0.6 * self.speed.1 + 0.4 * moved.1,
        );
        self.centre = centre;
        self.scale = scale;
        if m.score >= UPDATE_SCORE {
            let found = self.patch(&img, self.centre, self.scale);
            for ((t, f), key) in self.template.iter_mut().zip(&found).zip(&self.key) {
                *t = (1.0 - LEARN) * *t + LEARN * f;
                *t = (1.0 - ANCHOR) * *t + ANCHOR * key;
            }
        }
        Followed {
            area: self.area(),
            score: m.score,
        }
    }

    /// No match: on at the last speed, slowing.
    fn coast(&mut self) -> Followed {
        self.centre.0 += self.speed.0;
        self.centre.1 += self.speed.1;
        self.speed = (self.speed.0 * 0.8, self.speed.1 * 0.8);
        Followed {
            area: self.area(),
            score: 0.0,
        }
    }
}

/// The best match found (centre in level pixels).
#[derive(Clone, Copy, Debug)]
struct Match {
    cx: f32,
    cy: f32,
    score: f32,
    scale: f32,
}

/// Look for the zero-mean, unit-energy template `t` (`w` × `h`) in `img`
/// with its top-left corner within `reach` of `at`: every other position,
/// then the ones around the best; the peak refined by parabolas.
fn search(img: &Gray, t: &[f32], w: usize, h: usize, at: (i32, i32), reach: i32) -> Option<Match> {
    let (iw, ih) = (img.width as i32, img.height as i32);
    if w as i32 > iw || h as i32 > ih {
        return None;
    }
    let lo_x = (at.0 - reach).max(0);
    let hi_x = (at.0 + reach).min(iw - w as i32);
    let lo_y = (at.1 - reach).max(0);
    let hi_y = (at.1 + reach).min(ih - h as i32);
    if lo_x > hi_x || lo_y > hi_y {
        return None;
    }
    // Sums and sums of squares over the region searched (integral images).
    let rx0 = lo_x as usize;
    let ry0 = lo_y as usize;
    let rw = (hi_x - lo_x) as usize + w;
    let rh = (hi_y - lo_y) as usize + h;
    let stride = rw + 1;
    let mut s1 = vec![0.0f64; stride * (rh + 1)];
    let mut s2 = vec![0.0f64; stride * (rh + 1)];
    for y in 0..rh {
        let (mut a, mut b) = (0.0f64, 0.0f64);
        for x in 0..rw {
            let v = img.px[(ry0 + y) * img.width + rx0 + x] as f64;
            a += v;
            b += v * v;
            s1[(y + 1) * stride + x + 1] = s1[y * stride + x + 1] + a;
            s2[(y + 1) * stride + x + 1] = s2[y * stride + x + 1] + b;
        }
    }
    let n = (w * h) as f64;
    let score = |x: i32, y: i32| -> f32 {
        let (lx, ly) = ((x - lo_x) as usize, (y - lo_y) as usize);
        let rect = |s: &[f64]| {
            s[(ly + h) * stride + lx + w] - s[ly * stride + lx + w] - s[(ly + h) * stride + lx]
                + s[ly * stride + lx]
        };
        let sum = rect(&s1);
        let var = rect(&s2) - sum * sum / n;
        if var < 1e-3 {
            return 0.0;
        }
        let mut dot = 0.0f32;
        for j in 0..h {
            let row = &img.px[(y as usize + j) * img.width + x as usize..][..w];
            let tr = &t[j * w..(j + 1) * w];
            dot += row.iter().zip(tr).map(|(p, q)| *p as f32 * q).sum::<f32>();
        }
        (dot as f64 / var.sqrt()) as f32
    };
    // Every other position, then around the best.
    let mut best = (lo_x, lo_y, f32::NEG_INFINITY);
    let mut y = lo_y;
    while y <= hi_y {
        let mut x = lo_x;
        while x <= hi_x {
            let s = score(x, y);
            if s > best.2 {
                best = (x, y, s);
            }
            x += 2;
        }
        y += 2;
    }
    let (bx, by) = (best.0, best.1);
    for y in (by - 1).max(lo_y)..=(by + 1).min(hi_y) {
        for x in (bx - 1).max(lo_x)..=(bx + 1).min(hi_x) {
            let s = score(x, y);
            if s > best.2 {
                best = (x, y, s);
            }
        }
    }
    let (bx, by, peak) = best;
    // A fraction of a pixel: a parabola through the peak and its
    // neighbours, each way.
    let refine = |m: Option<f32>, p: Option<f32>| -> f32 {
        match (m, p) {
            (Some(m), Some(p)) => {
                let d = m - 2.0 * peak + p;
                if d < -1e-6 {
                    (0.5 * (m - p) / d).clamp(-0.5, 0.5)
                } else {
                    0.0
                }
            }
            _ => 0.0,
        }
    };
    let at =
        |x: i32, y: i32| (x >= lo_x && x <= hi_x && y >= lo_y && y <= hi_y).then(|| score(x, y));
    let dx = refine(at(bx - 1, by), at(bx + 1, by));
    let dy = refine(at(bx, by - 1), at(bx, by + 1));
    Some(Match {
        cx: bx as f32 + dx + w as f32 / 2.0,
        cy: by as f32 + dy + h as f32 / 2.0,
        score: peak,
        scale: 1.0,
    })
}

/// A path followed from one key, bent onto the next key (`to`, the box
/// the user drew on the frame after the path's last): the error there
/// spread over the path, nothing at its start (its own key), growing
/// frame by frame towards the next key.
pub fn bend(path: &[Followed], to: Option<Area>) -> Vec<Area> {
    let Some(to) = to else {
        return path.iter().map(|f| f.area).collect();
    };
    let n = path.len();
    let Some(last) = path.last() else {
        return Vec::new();
    };
    // The error is the key less the path's last place; the path's first
    // frame (its own key) keeps where it was drawn.
    let (lx, ly) = last.area.centre();
    let (tx, ty) = to.centre();
    let (ex, ey) = (tx - lx, ty - ly);
    let (ew, eh) = (to.w - last.area.w, to.h - last.area.h);
    path.iter()
        .enumerate()
        .map(|(i, f)| {
            let t = i as f32 / n as f32;
            let (cx, cy) = f.area.centre();
            let w = f.area.w + ew * t;
            let h = f.area.h + eh * t;
            Area::new(cx + ex * t - w / 2.0, cy + ey * t - h / 2.0, w, h)
        })
        .collect()
}

/// A clip's picture to follow an object through.
#[derive(Clone, Debug)]
pub struct FollowJob<'a> {
    /// The file decoded (the original, or its proxy).
    pub file: &'a Path,
    /// Whether `file` is the proxy (its times count from its first frame).
    pub proxy: bool,
    /// The original's frame index.
    pub index: &'a FrameIndex,
    /// The size frames are decoded at.
    pub size: (u32, u32),
    /// The first frame (the key) and the last followed to.
    pub from: usize,
    pub to: usize,
    /// The object on the first frame.
    pub start: Area,
}

/// Follow the object of `job` from its key to its last frame: every frame
/// the decoder gives, as `(frame number, where)` (the key first, as drawn).
pub fn follow_in_file(
    job: &FollowJob<'_>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f64),
) -> Result<Vec<(usize, Followed)>> {
    if job.to < job.from || job.from >= job.index.len() {
        return Ok(Vec::new());
    }
    let mut dec = Decoder::open(job.file, job.size.0, job.size.1)?;
    let first = job.index.times.first().copied().unwrap_or(0);
    // Times of the original as the file reads them, and back.
    let in_file = |t: i64| {
        if job.proxy {
            t - first + dec.first_time()
        } else {
            t
        }
    };
    let start_at = job.index.times.get(job.from).copied().unwrap_or(0);
    let start_in_file = in_file(start_at);
    let file_first = dec.first_time();
    dec.play_from(start_in_file)?;
    let mut tracker: Option<Tracker> = None;
    let mut out = Vec::new();
    let span = (job.to - job.from).max(1) as f64;
    while let Some(f) = dec.next_frame()? {
        if cancel.load(Ordering::Relaxed) {
            return Err(VideoError::Cancelled);
        }
        let t = if job.proxy {
            f.time - file_first + first
        } else {
            f.time
        };
        let Some(n) = job.index.frame_at(t) else {
            continue;
        };
        if n < job.from {
            continue;
        }
        if n > job.to {
            break;
        }
        let gray = Gray::from_rgba(f.width as usize, f.height as usize, &f.rgba);
        match tracker.as_mut() {
            None => {
                let Some(t) = Tracker::new(&gray, job.start) else {
                    return Err(VideoError::Gst("the box is not on the picture".into()));
                };
                out.push((
                    n,
                    Followed {
                        area: job.start,
                        score: 1.0,
                    },
                ));
                tracker = Some(t);
            }
            Some(t) => {
                if out.last().is_some_and(|(m, _)| *m == n) {
                    continue;
                }
                out.push((n, t.next(&gray)));
            }
        }
        progress((n - job.from) as f64 / span);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic texture value at (x, y): smooth blobs of noise.
    fn texture(x: f32, y: f32, seed: u32) -> f32 {
        let h = |i: i32, j: i32| {
            let mut v = (i as u32).wrapping_mul(73_856_093)
                ^ (j as u32).wrapping_mul(19_349_663)
                ^ seed.wrapping_mul(83_492_791);
            v ^= v >> 13;
            v = v.wrapping_mul(0x5bd1_e995);
            v ^= v >> 15;
            (v & 0xff) as f32
        };
        // Value noise on a 4-pixel lattice, bilinear.
        let (u, v) = (x / 4.0, y / 4.0);
        let (i, j) = (u.floor() as i32, v.floor() as i32);
        let (fx, fy) = (u - i as f32, v - j as f32);
        let s = |t: f32| t * t * (3.0 - 2.0 * t);
        let (sx, sy) = (s(fx), s(fy));
        let a = h(i, j) * (1.0 - sx) + h(i + 1, j) * sx;
        let b = h(i, j + 1) * (1.0 - sx) + h(i + 1, j + 1) * sx;
        a * (1.0 - sy) + b * sy
    }

    /// A `w`×`h` frame: a dim textured background, and a bright textured
    /// square of side `side` centred at (`cx`, `cy`), drawn with
    /// supersampling (sub-pixel places).
    fn frame(w: usize, h: usize, cx: f32, cy: f32, side: f32, noise: u32) -> Gray {
        let mut px = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for sy in 0..3 {
                    for sx in 0..3 {
                        let fx = x as f32 + (sx as f32 + 0.5) / 3.0;
                        let fy = y as f32 + (sy as f32 + 0.5) / 3.0;
                        let (dx, dy) = (fx - cx, fy - cy);
                        acc += if dx.abs() < side / 2.0 && dy.abs() < side / 2.0 {
                            // The object's own texture, scaled with it.
                            let k = 48.0 / side;
                            60.0 + 0.7 * texture(dx * k + 100.0, dy * k + 100.0, 1)
                        } else {
                            0.25 * texture(fx, fy, 2)
                        };
                    }
                }
                let n = if noise > 0 {
                    ((x * 31 + y * 17 + noise as usize * 7) % 11) as f32 - 5.0
                } else {
                    0.0
                };
                px[y * w + x] = (acc / 9.0 + n).clamp(0.0, 255.0) as u8;
            }
        }
        Gray {
            width: w,
            height: h,
            px,
        }
    }

    fn area_of(w: usize, h: usize, cx: f32, cy: f32, side: f32) -> Area {
        let (w, h) = (w as f32, h as f32);
        Area::new(
            (cx - side / 2.0) / w,
            (cy - side / 2.0) / h,
            side / w,
            side / h,
        )
    }

    #[test]
    fn an_object_moving_by_fractions_of_a_pixel_is_followed_within_half_a_pixel() {
        let (w, h) = (320, 180);
        let path = |n: usize| {
            let t = n as f32;
            (60.0 + 2.37 * t, 70.0 + 18.0 * (t * 0.11).sin())
        };
        let side = 40.0;
        let (x0, y0) = path(0);
        let mut tracker = Tracker::new(&frame(w, h, x0, y0, side, 0), area_of(w, h, x0, y0, side))
            .expect("a tracker");
        let mut errors = Vec::new();
        for n in 1..80 {
            let (x, y) = path(n);
            let f = tracker.next(&frame(w, h, x, y, side, n as u32));
            assert!(!f.lost(), "lost at frame {n}: {f:?}");
            let (cx, cy) = f.area.centre();
            let e = ((cx * w as f32 - x).powi(2) + (cy * h as f32 - y).powi(2)).sqrt();
            errors.push(e);
        }
        let mean = errors.iter().sum::<f32>() / errors.len() as f32;
        let worst = errors.iter().copied().fold(0.0, f32::max);
        eprintln!("mean {mean:.3} px, worst {worst:.3} px");
        assert!(mean < 0.5, "mean error {mean} px");
        assert!(worst < 1.5, "worst error {worst} px");
    }

    #[test]
    fn an_object_coming_nearer_is_followed_and_grows() {
        let (w, h) = (320, 180);
        let state = |n: usize| {
            let t = n as f32;
            (100.0 + 1.5 * t, 90.0 - 0.4 * t, 30.0 * 1.012f32.powf(t))
        };
        let (x0, y0, s0) = state(0);
        let mut tracker =
            Tracker::new(&frame(w, h, x0, y0, s0, 0), area_of(w, h, x0, y0, s0)).expect("tracker");
        let mut last = None;
        let mut worst = 0.0f32;
        for n in 1..60 {
            let (x, y, s) = state(n);
            let f = tracker.next(&frame(w, h, x, y, s, 0));
            assert!(!f.lost(), "lost at {n}");
            let (cx, cy) = f.area.centre();
            let e = ((cx * w as f32 - x).powi(2) + (cy * h as f32 - y).powi(2)).sqrt();
            worst = worst.max(e);
            last = Some((f, s));
        }
        let (f, s) = last.unwrap();
        let size = f.area.w * w as f32;
        eprintln!("worst {worst:.2} px; size {size:.1} vs {s:.1}");
        assert!(worst < 1.5, "worst {worst} px");
        assert!((size / s - 1.0).abs() < 0.08, "size {size} vs {s}");
    }

    #[test]
    fn a_hidden_object_is_marked_lost_and_found_again() {
        let (w, h) = (320, 180);
        let side = 36.0;
        let pos = |n: usize| (80.0 + 2.0 * n as f32, 90.0);
        let (x0, y0) = pos(0);
        let mut tracker = Tracker::new(&frame(w, h, x0, y0, side, 0), area_of(w, h, x0, y0, side))
            .expect("tracker");
        let mut lost = 0;
        let mut found_after = None;
        for n in 1..40 {
            let (x, y) = pos(n);
            let mut f = frame(w, h, x, y, side, 0);
            if (15..19).contains(&n) {
                // A flat grey card in front of it.
                for yy in 50..130 {
                    for xx in 90..200 {
                        f.px[yy * w + xx] = 128;
                    }
                }
            }
            let got = tracker.next(&f);
            if got.lost() {
                lost += 1;
            } else if lost > 0 && found_after.is_none() {
                found_after = Some(n);
            }
            if n == 30 {
                let (cx, _) = got.area.centre();
                assert!((cx * w as f32 - x).abs() < 2.0, "found again at {x}");
            }
        }
        assert!(lost >= 3, "{lost} frames lost");
        assert!(found_after.is_some_and(|n| n <= 20), "{found_after:?}");
    }

    #[test]
    fn a_path_is_bent_onto_the_next_key() {
        let path: Vec<Followed> = (0..4)
            .map(|i| Followed {
                area: Area::new(0.1 + 0.01 * i as f32, 0.2, 0.1, 0.1),
                score: 0.9,
            })
            .collect();
        let to = Area::new(0.25, 0.2, 0.1, 0.1);
        let bent = bend(&path, Some(to));
        // The key is 0.12 right of the last place: none of it at the
        // path's own key, 3/4 at its last frame.
        assert!((bent[0].x - 0.1).abs() < 1e-5);
        assert!((bent[3].x - (0.13 + 0.12 * 3.0 / 4.0)).abs() < 1e-5);
        assert_eq!(bend(&path, None)[2], path[2].area);
    }
}
