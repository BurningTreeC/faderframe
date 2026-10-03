//! Toolbar layout that wraps: controls are placed left to right in groups;
//! a group that does not fit on the current row moves to the next one (a
//! group wider than the whole toolbar is broken between its controls). A
//! fill control takes the rest of its row.

use crate::{Rect, Size};

/// Horizontal gap between controls of one group.
pub const ITEM_GAP: f32 = 1.0;
/// Horizontal gap between groups.
pub const GROUP_GAP: f32 = 12.0;

enum Entry<T> {
    Item(T, f32),
    Fill(T, f32),
    Group,
}

/// Collects controls, then lays them out for a width.
pub struct Flow<T> {
    entries: Vec<Entry<T>>,
}

impl<T> Default for Flow<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

/// Row geometry: height of a row and the margins around controls.
#[derive(Clone, Copy, Debug)]
pub struct FlowMetrics {
    pub row_height: f32,
    /// Left/right margin of every row.
    pub pad_x: f32,
    /// Space above and below the controls inside a row.
    pub pad_y: f32,
}

impl<T> Flow<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// A control of width `w` in the current group.
    pub fn item(&mut self, t: T, w: f32) -> &mut Self {
        self.entries.push(Entry::Item(t, w));
        self
    }

    /// A control taking the rest of its row (at least `min_w`); ends the
    /// row.
    pub fn fill(&mut self, t: T, min_w: f32) -> &mut Self {
        self.group();
        self.entries.push(Entry::Fill(t, min_w));
        self
    }

    /// Start a new group.
    pub fn group(&mut self) -> &mut Self {
        if !matches!(self.entries.last(), None | Some(Entry::Group)) {
            self.entries.push(Entry::Group);
        }
        self
    }

    /// Controls with their rectangles inside `area` (only its x, y and
    /// width are used), and the number of rows.
    pub fn layout(self, area: Rect, m: FlowMetrics) -> (Vec<(T, Rect)>, usize) {
        // Split into groups.
        let mut groups: Vec<Vec<Entry<T>>> = vec![Vec::new()];
        for e in self.entries {
            match e {
                Entry::Group => groups.push(Vec::new()),
                other => {
                    if let Some(g) = groups.last_mut() {
                        g.push(other);
                    }
                }
            }
        }
        let width = |g: &[Entry<T>]| -> f32 {
            let mut w = 0.0;
            for (i, e) in g.iter().enumerate() {
                if i > 0 {
                    w += ITEM_GAP;
                }
                w += match e {
                    Entry::Item(_, w) | Entry::Fill(_, w) => *w,
                    Entry::Group => 0.0,
                };
            }
            w
        };
        let left = area.x + m.pad_x;
        let right = area.x + area.w - m.pad_x;
        let h = (m.row_height - 2.0 * m.pad_y).max(1.0);
        let mut out = Vec::new();
        let mut row = 0usize;
        let mut x = left;
        let mut row_used = false;
        let new_row = |row: &mut usize, x: &mut f32, used: &mut bool| {
            *row += 1;
            *x = left;
            *used = false;
        };
        for g in groups.into_iter().filter(|g| !g.is_empty()) {
            let gw = width(&g);
            let start = if row_used { x + GROUP_GAP } else { x };
            if row_used && start + gw > right {
                new_row(&mut row, &mut x, &mut row_used);
            } else {
                x = start;
            }
            for (i, e) in g.into_iter().enumerate() {
                match e {
                    Entry::Item(t, w) => {
                        if i > 0 {
                            x += ITEM_GAP;
                        }
                        // Break an oversized group between its controls.
                        if row_used && x + w > right {
                            new_row(&mut row, &mut x, &mut row_used);
                        }
                        let y = area.y + row as f32 * m.row_height + m.pad_y;
                        out.push((t, Rect::new(x, y, w, h)));
                        x += w;
                        row_used = true;
                    }
                    Entry::Fill(t, min_w) => {
                        if row_used && right - x < min_w {
                            new_row(&mut row, &mut x, &mut row_used);
                        }
                        let y = area.y + row as f32 * m.row_height + m.pad_y;
                        let w = (right - x).max(min_w.min(right - left));
                        out.push((t, Rect::new(x, y, w, h)));
                        x += w;
                        row_used = true;
                    }
                    Entry::Group => {}
                }
            }
        }
        (out, row + 1)
    }
}

/// Height of `rows` rows.
pub fn flow_height(rows: usize, m: FlowMetrics) -> f32 {
    rows.max(1) as f32 * m.row_height
}

/// The size a flow needs at `width` (convenience for hosts).
pub fn flow_size(rows: usize, width: f32, m: FlowMetrics) -> Size {
    Size::new(width, flow_height(rows, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: FlowMetrics = FlowMetrics {
        row_height: 30.0,
        pad_x: 10.0,
        pad_y: 4.0,
    };

    fn flow() -> Flow<u32> {
        let mut f = Flow::new();
        f.item(1, 50.0).item(2, 50.0).group();
        f.item(3, 100.0).group();
        f.item(4, 60.0).item(5, 60.0).fill(6, 80.0);
        f
    }

    #[test]
    fn one_row_when_wide() {
        let (items, rows) = flow().layout(Rect::new(0.0, 0.0, 1000.0, 30.0), M);
        assert_eq!(rows, 1);
        assert_eq!(items.len(), 6);
        // The fill takes the rest.
        let fill = items.iter().find(|(t, _)| *t == 6).unwrap().1;
        assert_eq!(fill.right(), 990.0);
    }

    #[test]
    fn groups_wrap_together() {
        // 10 + 101 + 12 + 100 = 223 fits 240; the next group (121) wraps.
        let (items, rows) = flow().layout(Rect::new(0.0, 0.0, 240.0, 30.0), M);
        let r = |id| items.iter().find(|(t, _)| *t == id).unwrap().1;
        assert_eq!(r(3).y, r(1).y);
        assert!(r(4).y > r(1).y);
        assert_eq!(r(4).y, r(5).y, "a group stays together");
        assert_eq!(rows, 2, "the fill fits after them: {items:?}");
        let (_, rows) = flow().layout(Rect::new(0.0, 0.0, 200.0, 30.0), M);
        assert_eq!(rows, 4, "every group and the fill wrap when narrower");
        assert!(items.iter().all(|(_, rect)| rect.right() <= 230.0 + 0.01));
    }

    #[test]
    fn oversized_groups_break_between_controls() {
        let mut f = Flow::new();
        for i in 0..10 {
            f.item(i, 40.0);
        }
        let (items, rows) = f.layout(Rect::new(0.0, 0.0, 150.0, 30.0), M);
        assert_eq!(rows, 4);
        assert!(items.iter().all(|(_, r)| r.right() <= 140.0));
    }
}
