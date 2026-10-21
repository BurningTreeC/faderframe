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

/// What a grid line represents (for drawing emphasis), strongest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GridLineKind {
    Bar,
    Beat,
    /// A position of the grid between the beats.
    Subdivision,
    /// Finer than the grid — a position nothing snaps to — drawn where the
    /// grid's lines are far apart (see [`for_each_visible_line`]).
    Fine,
}

/// How dense a drawn grid may get, in pixels (see
/// [`for_each_visible_line`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridDensity {
    /// Lines of a kind closer than this are thinned (bars, the grid) or
    /// left out (beats).
    pub min_px: f64,
    /// Fine lines are at least this far apart; they appear once the
    /// finest lines drawn are twice as far apart.
    pub fine_px: f64,
}

impl Default for GridDensity {
    fn default() -> Self {
        Self {
            min_px: 8.0,
            fine_px: 18.0,
        }
    }
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

/// Every 2ⁿth bar is drawn when bars are closer than `min_px` (from the bar
/// at `at`): the stride.
pub fn bar_stride(
    at: MusicalTime,
    meter: &TimeSignatureMap,
    px_per_quarter: f64,
    min_px: f64,
) -> i32 {
    let bar = meter.signature_at(at).bar_length().quarters() * px_per_quarter;
    let mut stride = 1;
    while (stride as f64) * bar < min_px && stride < 1 << 20 {
        stride *= 2;
    }
    stride
}

/// Visit the grid an editor draws over `[start, end)` at `px_per_quarter`
/// pixels per quarter note — in time order, each position once with its
/// strongest kind:
///
/// * bar lines, every 2ⁿth bar when bars are closer than
///   [`GridDensity::min_px`] ([`bar_stride`]);
/// * beat lines, where beats are that far apart;
/// * the lines of `grid` — exactly the positions [`snap_nearest`] snaps to
///   (restarting at every bar) — where they are that far apart, else every
///   2ⁿth of them (still positions it snaps to) while that is finer than a
///   beat: a hierarchy bar > beat > subdivision;
/// * [`GridLineKind::Fine`] lines halving the finest cells drawn (the
///   grid's when it is finer than a beat, else the beats') until about
///   [`GridDensity::fine_px`] apart, once those cells are twice as wide —
///   so a grid shows at every zoom, down to single samples. Nothing snaps
///   to them.
///
/// The lines follow the time-signature map (bars and beats of each bar's
/// signature); triplet and dotted grids keep their own steps. Wide views
/// never flood: no two lines of a kind are closer than `min_px` (fine ones
/// than `fine_px`).
pub fn for_each_visible_line(
    start: MusicalTime,
    end: MusicalTime,
    grid: GridDivision,
    meter: &TimeSignatureMap,
    px_per_quarter: f64,
    density: GridDensity,
    mut visit: impl FnMut(MusicalTime, GridLineKind),
) {
    if end <= start || !(px_per_quarter.is_finite() && px_per_quarter > 0.0) {
        return;
    }
    let px = |ticks: i64| ticks as f64 / TICKS_PER_QUARTER as f64 * px_per_quarter;
    let min = density.min_px.max(1.0);
    let fine_min = density.fine_px.max(min);
    let stride = bar_stride(start, meter, px_per_quarter, min);
    let mut lines: Vec<(MusicalTime, GridLineKind)> = Vec::new();
    let mut bar = meter.bar_at(start);
    if stride > 1 {
        bar -= bar.rem_euclid(stride);
    }
    loop {
        let bar_start = meter.bar_start(bar);
        if bar_start >= end {
            break;
        }
        let next_bar = meter.bar_start(bar + 1);
        if bar_start >= start {
            lines.push((bar_start, GridLineKind::Bar));
        }
        if stride > 1 {
            bar += stride;
            continue;
        }
        // The visible part of the bar.
        let (lo, hi) = (start.max(bar_start), end.min(next_bar));
        let sig = meter.signature_of_bar(bar);
        let beat = sig.beat_length().ticks().max(1);
        let step = grid.step(sig).ticks().max(1);
        let kind = |t: MusicalTime| {
            let off = (t - bar_start).ticks();
            if off == 0 {
                GridLineKind::Bar
            } else if off % beat == 0 {
                GridLineKind::Beat
            } else {
                GridLineKind::Subdivision
            }
        };
        // Lines `every` ticks apart from the bar start, inside [lo, hi).
        let mut lattice = |every: i64, kind: &dyn Fn(MusicalTime) -> GridLineKind| {
            let first = ((lo - bar_start).ticks() + every - 1).div_euclid(every);
            let mut i = first.max(0);
            loop {
                let t = bar_start + MusicalTime(i * every);
                if t >= hi {
                    break;
                }
                lines.push((t, kind(t)));
                i += 1;
            }
        };
        let beats = px(beat) >= min;
        if beats {
            lattice(beat, &kind);
        }
        // The grid, or every 2ⁿth of its lines while finer than a beat.
        let mut every = step;
        while px(every) < min && every < beat {
            every *= 2;
        }
        let shown = px(every) >= min && (every == step || every < beat);
        if shown {
            lattice(every, &kind);
        }
        // Fine lines in the finest cells drawn.
        let cell = if shown && step < beat {
            Some(every)
        } else if beats {
            Some(beat)
        } else {
            None
        };
        if let Some(cell) = cell
            && px(cell) >= 2.0 * fine_min
        {
            let parts = 1i64 << ((px(cell) / fine_min).log2().floor() as u32).min(40);
            let fine = cell as f64 / parts as f64;
            // Cells start on the lattice from the bar start (the last one of
            // a bar may be short).
            let mut c = (lo - bar_start).ticks().div_euclid(cell);
            loop {
                let cell_start = bar_start + MusicalTime(c * cell);
                if cell_start >= hi {
                    break;
                }
                let cell_end = (cell_start + MusicalTime(cell)).min(next_bar);
                let from = ((lo - cell_start).ticks() as f64 / fine).ceil().max(1.0) as i64;
                for j in from..parts {
                    let t = cell_start + MusicalTime((j as f64 * fine).round() as i64);
                    if t >= hi || t >= cell_end {
                        break;
                    }
                    lines.push((t, GridLineKind::Fine));
                }
                c += 1;
            }
        }
        bar += 1;
    }
    lines.sort_unstable();
    lines.dedup_by_key(|l| l.0);
    for (t, k) in lines {
        visit(t, k);
    }
}

/// [`for_each_visible_line`] into a list.
pub fn visible_lines(
    start: MusicalTime,
    end: MusicalTime,
    grid: GridDivision,
    meter: &TimeSignatureMap,
    px_per_quarter: f64,
    density: GridDensity,
) -> Vec<(MusicalTime, GridLineKind)> {
    let mut out = Vec::new();
    for_each_visible_line(start, end, grid, meter, px_per_quarter, density, |t, k| {
        out.push((t, k));
    });
    out
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

    /// Meters to check grids in: plain, odd with a change, compound.
    fn meters() -> Vec<TimeSignatureMap> {
        let mut odd = TimeSignatureMap::new(TimeSignature::new(7, 8).unwrap());
        odd.set_change(MeterChange {
            bar: 3,
            signature: TimeSignature::FOUR_FOUR,
        });
        vec![
            TimeSignatureMap::new(TimeSignature::FOUR_FOUR),
            odd,
            TimeSignatureMap::new(TimeSignature::new(6, 8).unwrap()),
        ]
    }

    /// The drawn grid is the snap grid: every subdivision line is a
    /// position snapping goes to, no fine line is, and wherever the grid's
    /// own lines are far enough apart to be drawn, every position snapping
    /// can reach is drawn (as a bar, beat or subdivision line).
    #[test]
    fn drawn_grid_lines_are_the_snap_positions() {
        let density = GridDensity::default();
        for meter in meters() {
            for grid in GridDivision::all() {
                for ppq in [3.0, 20.0, 80.0, 300.0, 2_000.0, 40_000.0, 400_000.0] {
                    let width = 1500.0;
                    let start = q(9.3);
                    let end = start + MusicalTime::from_quarters(width / ppq);
                    let lines = visible_lines(start, end, grid, &meter, ppq, density);
                    for &(t, kind) in &lines {
                        assert!(t >= start && t < end);
                        let snapped = snap_nearest(t, grid, &meter);
                        match kind {
                            GridLineKind::Subdivision => assert_eq!(
                                snapped, t,
                                "{grid:?} at {ppq}: a subdivision is a snap position"
                            ),
                            GridLineKind::Fine => assert_ne!(
                                snapped, t,
                                "{grid:?} at {ppq}: nothing snaps to a fine line"
                            ),
                            _ => {}
                        }
                    }
                    assert!(lines.windows(2).all(|w| w[0].0 < w[1].0), "in order, once");
                    // Where the grid itself is wide enough to draw, all of it is.
                    let step = grid.step(meter.signature_at(start)).quarters() * ppq;
                    let bar = meter.signature_at(start).bar_length().quarters() * ppq;
                    if step >= density.min_px && bar >= density.min_px {
                        for_each_grid_line(start, end, grid, &meter, |t, _| {
                            assert!(
                                lines
                                    .iter()
                                    .any(|&(l, k)| l == t && k != GridLineKind::Fine),
                                "{grid:?} at {ppq}: the snap position {t:?} is drawn"
                            );
                        });
                    }
                }
            }
        }
    }

    /// Zoomed far in — down to single samples — the grid still shows
    /// (finer lines between the grid's), and zoomed far out it does not
    /// flood: no two lines of a kind closer than the density allows.
    #[test]
    fn the_grid_shows_at_every_zoom_without_flooding() {
        let meter = TimeSignatureMap::new(TimeSignature::FOUR_FOUR);
        let density = GridDensity::default();
        let width = 1500.0;
        let mut ppq = 1.5;
        while ppq <= 400_000.0 {
            for grid in [
                GridDivision::Bar,
                GridDivision::Beat,
                GridDivision::Note(16),
                GridDivision::Triplet(8),
                GridDivision::Dotted(8),
                GridDivision::Note(256),
            ] {
                let start = q(41.7);
                let end = start + MusicalTime::from_quarters(width / ppq);
                let lines = visible_lines(start, end, grid, &meter, ppq, density);
                assert!(
                    lines.len() as f64 >= width / (2.0 * density.fine_px + 4.0),
                    "{grid:?} at {ppq} px/quarter: only {} lines",
                    lines.len()
                );
                assert!(
                    lines.len() as f64 <= 2.0 * width / density.min_px + 2.0,
                    "{grid:?} at {ppq}: {} lines",
                    lines.len()
                );
                for kind in [
                    GridLineKind::Bar,
                    GridLineKind::Beat,
                    GridLineKind::Subdivision,
                    GridLineKind::Fine,
                ] {
                    let xs: Vec<f64> = lines
                        .iter()
                        .filter(|l| l.1 == kind)
                        .map(|l| l.0.quarters() * ppq)
                        .collect();
                    let least = if kind == GridLineKind::Fine {
                        density.fine_px
                    } else {
                        density.min_px
                    };
                    // Dotted grids restart at the bar: their last cell is short.
                    let least = if matches!(grid, GridDivision::Dotted(_)) {
                        least / 3.0
                    } else {
                        least
                    };
                    assert!(
                        xs.windows(2).all(|w| w[1] - w[0] >= least - 0.01),
                        "{grid:?} at {ppq}: {kind:?} lines closer than {least} px"
                    );
                }
            }
            ppq *= 1.7;
        }
    }

    #[test]
    fn visible_lines_keep_bar_beat_and_subdivision_apart() {
        let meter = TimeSignatureMap::new(TimeSignature::FOUR_FOUR);
        let d = GridDensity::default();
        // Eighth triplets, a beat 120 px wide: triplet subdivisions, and
        // fine lines between them (40 px cells → halves).
        let lines = visible_lines(q(0.0), q(4.0), GridDivision::Triplet(8), &meter, 120.0, d);
        let of = |k| {
            lines
                .iter()
                .filter(|l| l.1 == k)
                .map(|l| l.0)
                .collect::<Vec<_>>()
        };
        assert_eq!(of(GridLineKind::Bar), vec![q(0.0)]);
        assert_eq!(of(GridLineKind::Beat), vec![q(1.0), q(2.0), q(3.0)]);
        let subs = of(GridLineKind::Subdivision);
        assert_eq!(subs.len(), 8);
        assert!(subs.contains(&MusicalTime(TICKS_PER_QUARTER / 3)));
        assert_eq!(of(GridLineKind::Fine).len(), 12, "a line between each pair");
        // Wide: 6 px bars thin to every second bar, nothing else.
        let wide = visible_lines(q(0.0), q(400.0), GridDivision::Note(16), &meter, 1.5, d);
        assert!(wide.iter().all(|l| l.1 == GridLineKind::Bar));
        assert!(wide.iter().all(|l| meter.bar_at(l.0) % 2 == 0));
        assert_eq!(bar_stride(q(0.0), &meter, 1.5, d.min_px), 2);
        // 7/8: beats are eighths, bars 3.5 quarters.
        let seven = TimeSignatureMap::new(TimeSignature::new(7, 8).unwrap());
        let lines = visible_lines(q(0.0), q(7.0), GridDivision::Beat, &seven, 40.0, d);
        let bars: Vec<_> = lines
            .iter()
            .filter(|l| l.1 == GridLineKind::Bar)
            .map(|l| l.0)
            .collect();
        assert_eq!(bars, vec![q(0.0), q(3.5)]);
        assert_eq!(
            lines.iter().filter(|l| l.1 == GridLineKind::Beat).count(),
            12
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
