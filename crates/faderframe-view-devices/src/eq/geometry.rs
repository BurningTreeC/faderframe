//! Where things are: the editor's layout, the frequency and gain axes, and
//! musical notes (the piano display, typed values like "A4" or "C#2+13").

use faderframe_ui_canvas::{Rect, Size};

pub(crate) const TOP_H: f32 = 30.0;
pub(crate) const BOTTOM_H: f32 = 34.0;
/// The curves' gain scale (left).
pub(crate) const LEFT: f32 = 36.0;
/// The analyser's scale and the output meter (right).
pub(crate) const RIGHT: f32 = 46.0;
pub(crate) const AXIS_H: f32 = 18.0;
pub(crate) const PIANO_H: f32 = 30.0;
pub(crate) const NODE_R: f32 = 7.5;
/// The whole frequency range the display can show.
pub(crate) const F_MIN: f64 = 10.0;
pub(crate) const F_MAX: f64 = 30_000.0;

/// The editor's layout.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub top: Rect,
    /// The response display.
    pub graph: Rect,
    /// The frequency scale (or the piano) under it.
    pub axis: Rect,
    pub bottom: Rect,
    /// The output meter at the right.
    pub meter: Rect,
}

impl Layout {
    pub fn new(size: Size, piano: bool) -> Self {
        let top = Rect::new(0.0, 0.0, size.w, TOP_H);
        let bottom = Rect::new(0.0, size.h - BOTTOM_H, size.w, BOTTOM_H);
        let axis_h = if piano { PIANO_H } else { AXIS_H };
        let graph = Rect::new(
            LEFT,
            TOP_H + 6.0,
            (size.w - LEFT - RIGHT).max(10.0),
            (size.h - TOP_H - BOTTOM_H - axis_h - 6.0).max(10.0),
        );
        let axis = Rect::new(graph.x, graph.bottom(), graph.w, axis_h);
        let meter = Rect::new(size.w - 12.0, graph.y, 7.0, graph.h);
        Self {
            top,
            graph,
            axis,
            bottom,
            meter,
        }
    }
}

/// The visible frequency range (zoomable) on a logarithmic axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FreqAxis {
    pub lo: f64,
    pub hi: f64,
}

impl FreqAxis {
    pub fn x(&self, g: &Rect, f: f64) -> f32 {
        g.x + g.w * ((f / self.lo).ln() / (self.hi / self.lo).ln()) as f32
    }

    pub fn f(&self, g: &Rect, x: f32) -> f64 {
        let t = f64::from((x - g.x) / g.w);
        self.lo * (self.hi / self.lo).powf(t)
    }

    /// Zoom by `factor` (> 1 in) round frequency `at`, inside the whole
    /// range.
    pub fn zoomed(&self, at: f64, factor: f64, top: f64) -> Self {
        let span = (self.hi / self.lo).ln() / factor;
        let t = (at / self.lo).ln() / (self.hi / self.lo).ln();
        let lo = at.ln() - t * span;
        let mut out = Self {
            lo: lo.exp(),
            hi: (lo + span).exp(),
        };
        out.clamp(top);
        out
    }

    /// Move by `octaves` (positive: up).
    pub fn panned(&self, octaves: f64, top: f64) -> Self {
        let k = 2f64.powf(octaves);
        let mut out = Self {
            lo: self.lo * k,
            hi: self.hi * k,
        };
        out.clamp(top);
        out
    }

    fn clamp(&mut self, top: f64) {
        let span = (self.hi / self.lo).clamp(2.0, top / F_MIN);
        if self.lo < F_MIN {
            self.lo = F_MIN;
            self.hi = F_MIN * span;
        }
        if self.hi > top {
            self.hi = top;
            self.lo = top / span;
        }
    }
}

/// The gain axis of the curves: ±`range` dB over most of the height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GainAxis {
    pub range: f32,
}

impl GainAxis {
    pub fn y(&self, g: &Rect, db: f32) -> f32 {
        g.y + g.h * 0.5 - db / self.range * g.h * 0.46
    }

    pub fn db(&self, g: &Rect, y: f32) -> f32 {
        (g.y + g.h * 0.5 - y) / (g.h * 0.46) * self.range
    }

    /// dB per pixel.
    pub fn per_px(&self, g: &Rect) -> f32 {
        self.range / (g.h * 0.46)
    }
}

/// The analyser's level axis: 0 dBFS at the top, `-range` at the bottom.
pub(crate) fn analyser_y(g: &Rect, dbfs: f32, range: f32) -> f32 {
    g.y + g.h * (-dbfs / range).clamp(0.0, 1.0)
}

/// Points at `n` frequencies across an axis.
pub(crate) fn sweep(axis: &FreqAxis, n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| axis.lo * (axis.hi / axis.lo).powf(i as f64 / (n - 1).max(1) as f64))
        .collect()
}

pub(crate) use crate::values::{
    db_text, freq_of, is_black, note_label, note_name, note_of, parse_db, parse_freq, parse_value,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_axis_zooms_and_pans_inside_its_range() {
        let a = FreqAxis {
            lo: F_MIN,
            hi: F_MAX,
        };
        let z = a.zoomed(1_000.0, 4.0, F_MAX);
        assert!(z.lo > 100.0 && z.hi < 10_000.0, "{z:?}");
        let g = Rect::new(0.0, 0.0, 600.0, 100.0);
        // The frequency under the pointer stays put.
        let x = a.x(&g, 1_000.0);
        assert!((z.f(&g, x) - 1_000.0).abs() < 1.0);
        let p = z.panned(20.0, F_MAX);
        assert_eq!(p.hi, F_MAX);
        let back = z.zoomed(1_000.0, 1e-6, F_MAX);
        assert_eq!((back.lo, back.hi), (F_MIN, F_MAX));
    }
}
