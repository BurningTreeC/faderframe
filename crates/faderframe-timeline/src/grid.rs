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
    /// Dotted 1/n note (one and a half `Note(n)`).
    Dotted(u16),
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
            GridDivision::Dotted(n) => {
                MusicalTime(TICKS_PER_QUARTER * 4 * 3 / (2 * n.max(1) as i64))
            }
        }
    }

    /// Note values from a whole note down to 1/256 (2, 4, … 256).
    pub const NOTE_VALUES: [u16; 9] = [1, 2, 4, 8, 16, 32, 64, 128, 256];

    /// Every musical grid value an editor offers: bar and beat, straight
    /// notes down to 1/256, triplets and dotted values.
    pub fn all() -> Vec<GridDivision> {
        let mut out = vec![GridDivision::Bar, GridDivision::Beat];
        out.extend(
            Self::NOTE_VALUES[1..]
                .iter()
                .map(|&n| GridDivision::Note(n)),
        );
        out.extend(
            Self::NOTE_VALUES[1..]
                .iter()
                .map(|&n| GridDivision::Triplet(n)),
        );
        out.extend(
            Self::NOTE_VALUES[1..8]
                .iter()
                .map(|&n| GridDivision::Dotted(n)),
        );
        out
    }

    /// The 1/n note value a straight, triplet or dotted division is based
    /// on.
    pub fn base_note(self) -> Option<u16> {
        match self {
            GridDivision::Note(n) | GridDivision::Triplet(n) | GridDivision::Dotted(n) => Some(n),
            GridDivision::Bar | GridDivision::Beat => None,
        }
    }

    /// Toggle triplet (straight ↔ triplet; dotted becomes triplet; bar and
    /// beat become eighth triplets).
    pub fn toggled_triplet(self) -> Self {
        match self {
            GridDivision::Triplet(n) => GridDivision::Note(n),
            GridDivision::Note(n) | GridDivision::Dotted(n) => GridDivision::Triplet(n),
            _ => GridDivision::Triplet(8),
        }
    }

    /// Toggle dotted (dotted values go down to 1/128).
    pub fn toggled_dotted(self) -> Self {
        match self {
            GridDivision::Dotted(n) => GridDivision::Note(n),
            GridDivision::Note(n) | GridDivision::Triplet(n) => GridDivision::Dotted(n.min(128)),
            _ => GridDivision::Dotted(8),
        }
    }

    /// A grid menu: Bar, Beat, 1/2 … 1/256 (keeping the current triplet or
    /// dotted modifier), then Triplet and Dotted toggles.
    pub fn menu(current: GridDivision) -> Vec<GridMenuEntry> {
        let mut out = vec![
            GridMenuEntry::new("Bar", GridDivision::Bar, current == GridDivision::Bar),
            GridMenuEntry::new("Beat", GridDivision::Beat, current == GridDivision::Beat),
        ];
        for &n in &Self::NOTE_VALUES[1..] {
            let division = match current {
                GridDivision::Triplet(_) => GridDivision::Triplet(n),
                GridDivision::Dotted(_) if n <= 128 => GridDivision::Dotted(n),
                _ => GridDivision::Note(n),
            };
            out.push(GridMenuEntry::new(
                &format!("1/{n}"),
                division,
                current.base_note() == Some(n),
            ));
        }
        let mut triplet = GridMenuEntry::new(
            "Triplet",
            current.toggled_triplet(),
            matches!(current, GridDivision::Triplet(_)),
        );
        triplet.separated = true;
        out.push(triplet);
        out.push(GridMenuEntry::new(
            "Dotted",
            current.toggled_dotted(),
            matches!(current, GridDivision::Dotted(_)),
        ));
        out
    }

    pub fn label(self) -> String {
        match self {
            GridDivision::Bar => "Bar".into(),
            GridDivision::Beat => "Beat".into(),
            GridDivision::Note(n) => format!("1/{n}"),
            GridDivision::Triplet(n) => format!("1/{n}T"),
            GridDivision::Dotted(n) => format!("1/{n}D"),
        }
    }
}

/// One entry of a grid menu ([`GridDivision::menu`]).
#[derive(Clone, Debug, PartialEq)]
pub struct GridMenuEntry {
    pub label: String,
    /// The division choosing the entry sets.
    pub division: GridDivision,
    pub checked: bool,
    /// A separator goes before the entry.
    pub separated: bool,
}

impl GridMenuEntry {
    fn new(label: &str, division: GridDivision, checked: bool) -> Self {
        Self {
            label: label.into(),
            division,
            checked,
            separated: false,
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

    #[test]
    fn fine_grid_values_are_exact() {
        let sig = crate::TimeSignature::FOUR_FOUR;
        assert_eq!(
            GridDivision::Note(256).step(sig).ticks() * 64,
            TICKS_PER_QUARTER
        );
        assert_eq!(
            GridDivision::Triplet(256).step(sig).ticks() * 96,
            TICKS_PER_QUARTER
        );
        assert_eq!(
            GridDivision::Dotted(16).step(sig).ticks() * 2,
            GridDivision::Note(16).step(sig).ticks() * 3
        );
        let all = GridDivision::all();
        assert!(all.contains(&GridDivision::Note(256)));
        assert_eq!(GridDivision::Dotted(8).label(), "1/8D");
    }

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

    #[test]
    fn grid_menu_keeps_the_modifier() {
        let m = GridDivision::menu(GridDivision::Triplet(16));
        let e = |l: &str| m.iter().find(|e| e.label == l).unwrap().clone();
        assert!(e("1/16").checked);
        assert_eq!(e("1/256").division, GridDivision::Triplet(256));
        assert!(e("Triplet").checked && e("Triplet").separated);
        assert_eq!(e("Triplet").division, GridDivision::Note(16));
        assert_eq!(e("Dotted").division, GridDivision::Dotted(16));
        let m = GridDivision::menu(GridDivision::Dotted(8));
        assert_eq!(
            m.iter().find(|e| e.label == "1/256").unwrap().division,
            GridDivision::Note(256),
            "no dotted 1/256"
        );
    }
}
