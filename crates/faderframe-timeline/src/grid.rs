use crate::meter::TimeSignatureMap;
use crate::time::{MusicalTime, TICKS_PER_QUARTER};
use serde::{Deserialize, Serialize};

/// Grid / snap resolution. Grids restart at every bar line, so odd meters
/// (7/8 with a quarter grid) still snap to bar starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridDivision {
    Bar,
    /// One beat of the current time signature.
    Beat,
    /// 1/n note (4 = quarter, 16 = sixteenth).
    Note(u16),
    /// Triplet 1/n note (two thirds of `Note(n)`).
    Triplet(u16),
}

impl GridDivision {
    /// Step length inside a bar with time signature `sig`.
    pub fn step(self, sig: crate::TimeSignature) -> MusicalTime {
        match self {
            GridDivision::Bar => sig.bar_length(),
            GridDivision::Beat => sig.beat_length(),
            GridDivision::Note(n) => MusicalTime(TICKS_PER_QUARTER * 4 / n.max(1) as i64),
            GridDivision::Triplet(n) => {
                MusicalTime(TICKS_PER_QUARTER * 4 * 2 / (3 * n.max(1) as i64))
            }
        }
    }

    pub fn label(self) -> String {
        match self {
            GridDivision::Bar => "Bar".into(),
            GridDivision::Beat => "Beat".into(),
            GridDivision::Note(n) => format!("1/{n}"),
            GridDivision::Triplet(n) => format!("1/{n}T"),
        }
    }
}

/// What a grid line represents (for drawing emphasis).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridLineKind {
    Bar,
    Beat,
    Subdivision,
}

/// Bar-relative grid points around `pos`: (previous-or-equal, next).
fn grid_bounds(
    pos: MusicalTime,
    div: GridDivision,
    meter: &TimeSignatureMap,
) -> (MusicalTime, MusicalTime) {
    let bar = meter.bar_at(pos);
    let bar_start = meter.bar_start(bar);
    let next_bar = meter.bar_start(bar + 1);
    let step = div.step(meter.signature_of_bar(bar)).ticks().max(1);
    let k = (pos - bar_start).ticks() / step;
    let floor = bar_start + MusicalTime(k * step);
    let ceil = (floor + MusicalTime(step)).min(next_bar);
    (floor, ceil)
}

/// Snap to the nearest grid point.
pub fn snap_nearest(pos: MusicalTime, div: GridDivision, meter: &TimeSignatureMap) -> MusicalTime {
    let (floor, ceil) = grid_bounds(pos, div, meter);
    if (pos - floor) <= (ceil - pos) {
        floor
    } else {
        ceil
    }
}

/// Snap down to the previous (or equal) grid point.
pub fn snap_floor(pos: MusicalTime, div: GridDivision, meter: &TimeSignatureMap) -> MusicalTime {
    grid_bounds(pos, div, meter).0
}

/// Visit every grid line in `[start, end)`.
pub fn for_each_grid_line(
    start: MusicalTime,
    end: MusicalTime,
    div: GridDivision,
    meter: &TimeSignatureMap,
    mut visit: impl FnMut(MusicalTime, GridLineKind),
) {
    if end <= start {
        return;
    }
    let mut bar = meter.bar_at(start);
    loop {
        let bar_start = meter.bar_start(bar);
        if bar_start >= end {
            break;
        }
        let next_bar = meter.bar_start(bar + 1);
        let sig = meter.signature_of_bar(bar);
        let beat = sig.beat_length().ticks();
        let step = div.step(sig).ticks().max(1);
        let mut t = bar_start;
        while t < next_bar && t < end {
            if t >= start {
                let off = (t - bar_start).ticks();
                let kind = if off == 0 {
                    GridLineKind::Bar
                } else if off % beat == 0 {
                    GridLineKind::Beat
                } else {
                    GridLineKind::Subdivision
                };
                visit(t, kind);
            }
            t += MusicalTime(step);
        }
        bar += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MeterChange, TimeSignature};

    fn q(n: f64) -> MusicalTime {
        MusicalTime::from_quarters(n)
    }

    #[test]
    fn snapping_in_four_four() {
        let meter = TimeSignatureMap::new(TimeSignature::FOUR_FOUR);
        assert_eq!(snap_nearest(q(1.4), GridDivision::Beat, &meter), q(1.0));
        assert_eq!(snap_nearest(q(1.6), GridDivision::Beat, &meter), q(2.0));
        assert_eq!(snap_nearest(q(2.9), GridDivision::Bar, &meter), q(4.0));
        assert_eq!(snap_floor(q(3.99), GridDivision::Bar, &meter), q(0.0));
        assert_eq!(
            snap_nearest(q(0.30), GridDivision::Note(16), &meter),
            q(0.25)
        );
        assert_eq!(
            snap_nearest(q(0.70), GridDivision::Triplet(8), &meter),
            q(2.0 / 3.0)
        );
    }

    #[test]
    fn odd_meter_snaps_to_bar_start() {
        let mut meter = TimeSignatureMap::new(TimeSignature::new(7, 8).unwrap());
        meter.set_change(MeterChange {
            bar: 10,
            signature: TimeSignature::FOUR_FOUR,
        });
        // A 7/8 bar is 3.5 quarters; with a quarter grid the last cell is short.
        assert_eq!(snap_nearest(q(3.4), GridDivision::Note(4), &meter), q(3.5));
        assert_eq!(snap_floor(q(3.6), GridDivision::Note(4), &meter), q(3.5));
    }

    #[test]
    fn grid_lines_classified() {
        let meter = TimeSignatureMap::new(TimeSignature::FOUR_FOUR);
        let mut lines = Vec::new();
        for_each_grid_line(q(3.0), q(5.0), GridDivision::Note(8), &meter, |t, k| {
            lines.push((t, k))
        });
        assert_eq!(
            lines,
            vec![
                (q(3.0), GridLineKind::Beat),
                (q(3.5), GridLineKind::Subdivision),
                (q(4.0), GridLineKind::Bar),
                (q(4.5), GridLineKind::Subdivision),
            ]
        );
    }
}
