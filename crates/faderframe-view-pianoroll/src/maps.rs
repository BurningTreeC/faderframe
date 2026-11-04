//! Articulation and drum maps in the piano roll.
//!
//! * The articulation lane lists the track's articulations, a row each
//!   (their names in the lane's header column): directions as bars from
//!   where they are set to the next one of their group (each group's first
//!   articulation, faint, before any), attributes as blocks under the
//!   notes that carry them. A click in a direction's row sets it there (on
//!   the grid) or removes the one there; a click in an attribute's row
//!   gives it to the notes sounding there (or takes it back). A
//!   right-click offers every articulation for the selected notes.
//! * With a drum map, drum mode lists the map's sounds by name in its
//!   order instead of a keyboard, and drawing on a sound's row keeps to
//!   its own grid.

use crate::PianoRollView;
use faderframe_project::articulation::{ArticulationKind, ArticulationMap};
use faderframe_project::{Clip, MidiClip};
use faderframe_session::maps::MapOp;
use faderframe_session::{Action, Session};
use faderframe_timeline::{GridDivision, MusicalTime};
use faderframe_ui_canvas::{
    Align, EventCx, HostRequest, MenuItem, Paint, Painter, Point, Rect, TextStyle,
};

/// The articulation lane's rows: the smallest a row gets.
const MIN_ROW: f32 = 9.0;
/// The title line over the lane's names.
const TITLE_H: f32 = 14.0;

impl PianoRollView {
    /// Row `i` of the articulation lane (of `n` rows) inside `area`.
    pub(crate) fn articulation_row(area: Rect, i: usize, n: usize) -> Rect {
        let h = (area.h / n.max(1) as f32).max(MIN_ROW);
        Rect::new(area.x, area.y + i as f32 * h, area.w, h)
    }

    /// The articulation lane's row under `y` (`None`: below the rows).
    pub(crate) fn articulation_row_at(area: Rect, y: f32, n: usize) -> Option<usize> {
        let h = (area.h / n.max(1) as f32).max(MIN_ROW);
        let i = ((y - area.y) / h).floor();
        (i >= 0.0 && (i as usize) < n).then_some(i as usize)
    }

    /// The articulation lane: directions as bars, attributes under their
    /// notes, the names in the header column.
    pub(crate) fn paint_articulations(
        &self,
        p: &mut dyn Painter,
        header: Rect,
        area: Rect,
        m: &MidiClip,
        model: &Session,
    ) {
        let th = &self.theme;
        let Some(map) = model.editor_articulation_map() else {
            p.text(
                "No articulation map: choose one in the track's menu (Articulation Map)",
                area,
                &TextStyle::new(th.fonts.small, th.ui.text_faint).center(),
            );
            return;
        };
        let n = map.articulations.len();
        let accent = th.piano.lane_curve;
        let names = Rect::new(header.x, header.y + TITLE_H, header.w, header.h - TITLE_H);
        for (i, a) in map.articulations.iter().enumerate() {
            let row = Self::articulation_row(area, i, n);
            if row.y > area.bottom() {
                break;
            }
            if i % 2 == 1 {
                p.fill(row, th.ui.text.with_alpha(0.03));
            }
            // Its directions: from each to the next of its group.
            if a.kind == ArticulationKind::Direction {
                let group = a.group;
                let mut starts: Vec<(MusicalTime, bool)> = m
                    .articulations
                    .iter()
                    .filter_map(|e| {
                        let x = map.get(e.articulation)?;
                        (x.group == group).then_some((e.time, e.articulation == a.id))
                    })
                    .collect();
                starts.sort_by_key(|(t, _)| *t);
                let first = starts.first().map_or(m.length, |(t, _)| *t);
                // The group's default before any direction (faint).
                if map.default_state()[usize::from(group).min(3)] == Some(a.id)
                    && first > MusicalTime::ZERO
                {
                    let bar = self.lane_bar(row, MusicalTime::ZERO, first);
                    p.fill_rounded(bar, 2.0, &Paint::Solid(accent.with_alpha(0.18)));
                }
                for (k, (t, mine)) in starts.iter().enumerate() {
                    if !mine {
                        continue;
                    }
                    let end = starts.get(k + 1).map_or(m.length, |(t, _)| *t);
                    let bar = self.lane_bar(row, *t, end);
                    p.fill_rounded(bar, 2.0, &Paint::Solid(accent.with_alpha(0.55)));
                    p.fill(Rect::new(bar.x, row.y + 1.0, 2.0, row.h - 2.0), accent);
                }
            } else {
                for note in m.notes.iter().filter(|x| x.articulation == Some(a.id)) {
                    let bar = self.lane_bar(row, note.start, note.end());
                    p.fill_rounded(bar, 2.0, &Paint::Solid(accent.with_alpha(0.8)));
                }
            }
            p.hline(
                row.x,
                row.right(),
                row.bottom() - 0.5,
                th.ui.text.with_alpha(0.06),
            );
            // Its name beside the row.
            let label = Rect::new(names.x, row.y, names.w, row.h);
            if row.h >= MIN_ROW && label.bottom() <= header.bottom() + 0.5 {
                let mark = match a.kind {
                    ArticulationKind::Attribute => "•",
                    ArticulationKind::Direction => "→",
                };
                p.text(
                    &format!("{mark} {}", a.name),
                    label.inset_xy(6.0, 0.0),
                    &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
                );
            }
        }
    }

    fn lane_bar(&self, row: Rect, from: MusicalTime, to: MusicalTime) -> Rect {
        let x0 = self.x_of(from);
        let x1 = self.x_of(to);
        Rect::new(x0, row.y + 2.0, (x1 - x0).max(3.0), (row.h - 4.0).max(2.0))
    }

    /// A click in the articulation lane (see the module docs).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn articulation_press(
        &self,
        pos: Point,
        area: Rect,
        clip: &Clip,
        m: &MidiClip,
        model: &Session,
        bypass_snap: bool,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(map) = model.editor_articulation_map() else {
            return;
        };
        let Some(row) = Self::articulation_row_at(area, pos.y, map.articulations.len()) else {
            return;
        };
        let a = &map.articulations[row];
        let t = self.time_at(pos.x).max(MusicalTime::ZERO);
        match a.kind {
            ArticulationKind::Direction => {
                // One of its own near the pointer: remove it.
                let near = m
                    .articulations
                    .iter()
                    .find(|e| e.articulation == a.id && (self.x_of(e.time) - pos.x).abs() <= 5.0);
                let action = match near {
                    Some(e) => MapOp::SetDirection {
                        clip: clip.id,
                        at: e.time,
                        articulation: None,
                    },
                    None => MapOp::SetDirection {
                        clip: clip.id,
                        at: self.snap_rel(t, clip.start, model, bypass_snap || !model.editor.snap),
                        articulation: Some(a.id),
                    },
                };
                cx.emit(Action::Maps(action));
            }
            ArticulationKind::Attribute => {
                let under: Vec<&faderframe_project::MidiNote> = m
                    .notes
                    .iter()
                    .filter(|n| n.start <= t && n.end() > t)
                    .collect();
                if under.is_empty() {
                    return;
                }
                let has = under.iter().all(|n| n.articulation == Some(a.id));
                cx.emit(Action::Maps(MapOp::SetNoteArticulation {
                    clip: clip.id,
                    notes: under.iter().map(|n| n.id).collect(),
                    articulation: (!has).then_some(a.id),
                }));
            }
        }
    }

    /// The Articulation entries for `notes` (empty without a map).
    pub(crate) fn articulation_items(
        model: &Session,
        clip: faderframe_core::ClipId,
        notes: &[faderframe_core::NoteId],
    ) -> Vec<MenuItem<Action>> {
        model
            .articulation_choices(clip, notes)
            .into_iter()
            .map(|c| {
                let item = MenuItem::new(c.label, c.action).checked(c.checked);
                if c.group_start {
                    item.separated()
                } else {
                    item
                }
            })
            .collect()
    }

    /// A right-click in the articulation lane: the articulations for the
    /// selected notes (else those sounding there).
    pub(crate) fn articulation_menu(
        &self,
        pos: Point,
        clip: &Clip,
        m: &MidiClip,
        model: &Session,
    ) -> HostRequest<Action> {
        let t = self.time_at(pos.x).max(MusicalTime::ZERO);
        let mut notes = Self::selection_ids(m, model);
        if notes.is_empty() {
            notes = m
                .notes
                .iter()
                .filter(|n| n.start <= t && n.end() > t)
                .map(|n| n.id)
                .collect();
        }
        let mut items = if notes.is_empty() {
            vec![MenuItem::disabled("No notes chosen")]
        } else {
            Self::articulation_items(model, clip.id, &notes)
        };
        if items.is_empty() {
            items.push(MenuItem::disabled(
                "No articulation map: choose one in the track's menu",
            ));
        }
        // The map itself, in the map editor.
        if let Some(map) = model.project().track_articulation_map(clip.track) {
            items.push(
                MenuItem::new(
                    "Edit Articulation Map…",
                    Action::Maps(MapOp::OpenEditor {
                        map: faderframe_session::maps::MapRef::Articulation(map.id),
                        track: Some(clip.track),
                    }),
                )
                .separated(),
            );
        }
        HostRequest::ContextMenu { at: pos, items }
    }

    /// What a row of the articulation lane is.
    pub(crate) fn articulation_tooltip(&self, area: Rect, y: f32, map: &ArticulationMap) -> String {
        match Self::articulation_row_at(area, y, map.articulations.len())
            .and_then(|i| map.articulations.get(i))
        {
            Some(a) => match a.kind {
                ArticulationKind::Direction => format!(
                    "{} (direction, group {}): sends {} · click to set it here, click one to remove it",
                    a.name,
                    a.group + 1,
                    a.output.describe()
                ),
                ArticulationKind::Attribute => format!(
                    "{} (attribute, group {}): sends {} · click under notes to give it to them",
                    a.name,
                    a.group + 1,
                    a.output.describe()
                ),
            },
            None => "Articulations · right-click: for the selected notes".into(),
        }
    }

    /// Drum mode with a drum map: the sounds by name in the map's order,
    /// where the keyboard is.
    pub(crate) fn paint_drum_names(&self, p: &mut dyn Painter, rect: Rect, model: &Session) {
        let th = &self.theme;
        let pr = &th.piano;
        let Some(map) = model.editor_drum_map() else {
            return;
        };
        p.fill(rect, pr.key_white_shade);
        p.push_clip(rect);
        let top = (((rect.y - self.grid_top() + self.scroll_y) / self.row_h)
            .floor()
            .max(0.0)) as usize;
        let bottom = (((rect.bottom() - self.grid_top() + self.scroll_y) / self.row_h).ceil()
            as usize)
            .min(self.rows.len());
        let hover = self
            .hover
            .filter(|h| rect.contains(*h))
            .map(|h| self.key_at(h.y));
        let held = model.held_midi_keys();
        for i in top..bottom {
            let key = self.rows[i];
            let y = self.grid_top() + i as f32 * self.row_h - self.scroll_y;
            let row = Rect::new(rect.x, y, rect.w, self.row_h);
            let c = if held & (1u128 << key) != 0 {
                th.ui.accent.lighten(0.3)
            } else if hover == Some(key) {
                th.ui.selection.lighten(0.5)
            } else if i % 2 == 0 {
                pr.key_white
            } else {
                pr.key_white.darken(0.06)
            };
            p.fill(row, c);
            p.hline(
                rect.x,
                rect.right(),
                row.bottom() - 0.5,
                pr.key_white_shade.darken(0.2),
            );
            if self.row_h >= 8.0 {
                let name = map
                    .name_of(key)
                    .map_or_else(|| crate::note_name(key), str::to_string);
                p.text(
                    &name,
                    row.inset_xy(5.0, 0.0),
                    &TextStyle::new(th.fonts.tiny, pr.key_text),
                );
                faderframe_ui_canvas::controls::engraved(
                    p,
                    &crate::note_name(key),
                    Rect::new(row.right() - 34.0, row.y, 30.0, row.h),
                    th,
                    Align::End,
                );
            }
        }
        p.pop_clip();
        p.vline(
            rect.right() - 1.0,
            rect.y,
            rect.bottom(),
            pr.key_white_shade.darken(0.5),
        );
    }

    /// The drum sound's own grid on `key`'s row (drum mode with a map).
    pub(crate) fn drum_grid(&self, model: &Session, key: u8) -> Option<GridDivision> {
        if !self.drum_rows {
            return None;
        }
        model.editor_drum_map()?.sound(key)?.quantize
    }
}
