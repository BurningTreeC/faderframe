//! Piano roll: the MIDI editor for the clip opened from the arranger
//! (`Session::editor_clip`).
//!
//! Everything is painted directly (no widget per note) and only what is
//! visible is visited. The view owns presentation state only — zoom,
//! scroll, the current tool, drags in progress (shown as previews and
//! committed as one session action on release); every edit goes through a
//! session `Action` (the session allocates note ids, so copy, paste,
//! duplicate and chords are one undoable step each).
//!
//! Features: tools (select, draw, erase, split, mute), snap with any grid
//! (triplets included) and Shift to bypass it, chords and scales (scale
//! highlighting, scale snap, folding to the scale or to used keys),
//! quantize (strength, swing, ends), velocity editing (stems, line ramps),
//! controller lanes (mod wheel, pitch bend, sustain, any CC: freehand,
//! lines, erase), ghost notes of the track's other clips, auditioning,
//! step input from a MIDI keyboard, live keys on the keyboard, clip-length
//! handle, inspector with numeric entry, follow playhead.

#![forbid(unsafe_code)]

mod edit;
mod paint;
mod toolbar;

#[cfg(test)]
mod tests;

use faderframe_core::{ClipId, NoteId};
use faderframe_project::{Clip, ExpressionKind, MidiClip, MidiController, MidiNote, TrackColor};
use faderframe_session::{KeyFold, Session};
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{Color, Point, Rect, Size, Theme};

/// Width of the grab zone at a note's edges.
const EDGE_GRAB: f32 = 6.0;
const DRAG_THRESHOLD: f32 = 3.0;
/// Height of the grid/lane splitter.
const SPLITTER: f32 = 5.0;
const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// "C4" style name (middle C = C4 = MIDI 60).
pub fn note_name(key: u8) -> String {
    format!("{}{}", NAMES[(key % 12) as usize], key as i32 / 12 - 1)
}

/// What the pointer does in the note area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Select, move, resize; double-click adds a note.
    #[default]
    Pointer,
    /// Click to add notes (or chords); drag sets the length.
    Pencil,
    /// Click or drag over notes to delete them.
    Eraser,
    /// Click a note to split it.
    Knife,
    /// Click or drag over notes to mute or unmute them.
    Mute,
}

impl Tool {
    pub const ALL: [Tool; 5] = [
        Tool::Pointer,
        Tool::Pencil,
        Tool::Eraser,
        Tool::Knife,
        Tool::Mute,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Pointer => "Select",
            Tool::Pencil => "Draw",
            Tool::Eraser => "Erase",
            Tool::Knife => "Split",
            Tool::Mute => "Mute",
        }
    }

    pub fn key(self) -> char {
        match self {
            Tool::Pointer => '1',
            Tool::Pencil => '2',
            Tool::Eraser => '3',
            Tool::Knife => '4',
            Tool::Mute => '5',
        }
    }
}

/// The lane under the notes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LaneKind {
    #[default]
    Velocity,
    Controller(MidiController),
    /// Per-note expression (MPE) of the selected notes.
    Expression(ExpressionKind),
}

impl LaneKind {
    pub fn label(self) -> String {
        match self {
            LaneKind::Velocity => "Velocity".into(),
            LaneKind::Controller(c) => c.label(),
            LaneKind::Expression(k) => k.label().into(),
        }
    }
}

/// A drag in progress (committed on release).
#[derive(Clone, Debug, PartialEq)]
enum Drag {
    /// Moving (or, with Alt, copying) the selected notes.
    Move {
        origin: Point,
        grab: MidiNote,
        notes: Vec<NoteId>,
        dt: MusicalTime,
        dk: i32,
        moved: bool,
        copy: bool,
    },
    /// Changing the selection's starts (`start`) or ends.
    Resize {
        grab: MidiNote,
        notes: Vec<NoteId>,
        start: bool,
        delta: MusicalTime,
    },
    /// A note (or chord) being drawn: its length follows the pointer.
    Draw {
        start: MusicalTime,
        key: u8,
        length: MusicalTime,
    },
    /// Rubber-band selection.
    Select { from: Point, to: Point, add: bool },
    /// Eraser or mute tool sweeping over notes.
    Sweep { hit: Vec<NoteId> },
    /// Dragging a velocity stem.
    Velocity {
        note: MidiNote,
        origin_y: f32,
        value: u8,
    },
    /// Drawing a velocity line across notes.
    VelocityLine { from: Point, to: Point },
    /// Drawing controller values (freehand, line, or erasing).
    Controller {
        points: Vec<(MusicalTime, u16)>,
        from: Point,
        to: Point,
        line: bool,
        erase: bool,
    },
    /// Drawing the expression of `notes` (id, start, end; clip-relative).
    Expression {
        notes: Vec<(NoteId, MusicalTime, MusicalTime)>,
        points: Vec<(MusicalTime, f32)>,
        from: Point,
        to: Point,
        line: bool,
        erase: bool,
    },
    /// Resizing the lane.
    Splitter { origin_y: f32, origin_h: f32 },
    /// Dragging the clip end in the ruler.
    ClipEnd { length: MusicalTime },
    /// Locating with the ruler.
    Scrub,
    /// Playing keys on the keyboard.
    Keys { key: u8 },
}

pub struct PianoRollView {
    theme: Theme,
    /// Pixels per quarter note.
    ppq: f32,
    /// Pixels per key row.
    row_h: f32,
    scroll_x: f32,
    scroll_y: f32,
    tool: Tool,
    drag: Option<Drag>,
    lane: LaneKind,
    lane_channel: u8,
    lane_h: f32,
    centred_for: Option<ClipId>,
    /// Keep re-centring on layout changes until the user scrolls or zooms.
    auto_centre: bool,
    last_size: Size,
    hover: Option<Point>,
    /// Length of the last drawn or resized note ("Last" note length).
    last_length: MusicalTime,
    /// Visible keys, top to bottom (all 128, or folded).
    rows: Vec<u8>,
    /// Rows the toolbar wraps into at the current width.
    toolbar_rows: usize,
}

/// The regions of the view.
#[derive(Clone, Copy, Debug)]
struct Layout {
    toolbar: Rect,
    corner: Rect,
    ruler: Rect,
    keys: Rect,
    grid: Rect,
    splitter: Rect,
    lane_header: Rect,
    lane: Rect,
}

impl PianoRollView {
    pub fn new(theme: Theme) -> Self {
        let row_h = theme.piano.row_height;
        let lane_h = theme.piano.velocity_height;
        Self {
            theme,
            ppq: 90.0,
            row_h,
            scroll_x: 0.0,
            scroll_y: 0.0,
            tool: Tool::Pointer,
            drag: None,
            lane: LaneKind::Velocity,
            lane_channel: 0,
            lane_h,
            centred_for: None,
            auto_centre: true,
            last_size: Size::default(),
            hover: None,
            last_length: MusicalTime::from_quarters(0.25),
            rows: (0..=127u8).rev().collect(),
            toolbar_rows: 1,
        }
    }

    pub fn tool(&self) -> Tool {
        self.tool
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
    }

    pub fn lane(&self) -> LaneKind {
        self.lane
    }

    /// Height of the (wrapping) toolbar.
    fn toolbar_h(&self) -> f32 {
        self.theme.piano.toolbar_height * self.toolbar_rows.max(1) as f32
    }

    /// Re-wrap the toolbar for `width` (before painting and events).
    fn update_toolbar(&mut self, width: f32, model: &Session) {
        let r = Rect::new(0.0, 0.0, width, self.theme.piano.toolbar_height);
        self.toolbar_rows = self.toolbar_layout(r, model).1;
    }

    fn kb_w(&self) -> f32 {
        self.theme.piano.keyboard_width
    }

    fn layout(&self, size: Size) -> Layout {
        let pr = &self.theme.piano;
        let mut r = Rect::from_size(size);
        let toolbar = r.take_top(self.toolbar_h());
        let head = r.take_top(pr.ruler_height);
        let lane_h = self.lane_h.clamp(36.0, (r.h * 0.6).max(36.0));
        let lane_row = r.take_bottom(lane_h);
        let splitter = r.take_bottom(SPLITTER);
        let (corner, ruler) = head.split_left(self.kb_w());
        let (keys, grid) = r.split_left(self.kb_w());
        let (lane_header, lane) = lane_row.split_left(self.kb_w());
        Layout {
            toolbar,
            corner,
            ruler,
            keys,
            grid,
            splitter,
            lane_header,
            lane,
        }
    }

    /// x of a clip-relative time.
    pub fn x_of(&self, rel: MusicalTime) -> f32 {
        self.kb_w() + rel.quarters() as f32 * self.ppq - self.scroll_x
    }

    pub fn time_at(&self, x: f32) -> MusicalTime {
        MusicalTime::from_quarters(((x - self.kb_w() + self.scroll_x) / self.ppq) as f64)
    }

    fn grid_top(&self) -> f32 {
        self.toolbar_h() + self.theme.piano.ruler_height
    }

    /// Row index of a key (`None`: folded away).
    fn row_of(&self, key: u8) -> Option<usize> {
        // Rows are sorted descending.
        self.rows.binary_search_by(|k| key.cmp(k)).ok()
    }

    /// Top y of a key's row.
    pub fn y_of(&self, key: u8) -> Option<f32> {
        self.row_of(key)
            .map(|i| self.grid_top() + i as f32 * self.row_h - self.scroll_y)
    }

    /// Key under y (clamped to the rows).
    pub fn key_at(&self, y: f32) -> u8 {
        let i = ((y - self.grid_top() + self.scroll_y) / self.row_h).floor();
        let i = (i.max(0.0) as usize).min(self.rows.len().saturating_sub(1));
        self.rows.get(i).copied().unwrap_or(60)
    }

    /// Recompute the visible rows from the fold setting.
    fn update_rows(&mut self, model: &Session, m: &MidiClip) {
        let pr = &model.editor.piano;
        let mut rows: Vec<u8> = match pr.fold {
            KeyFold::Off => (0..=127u8).collect(),
            KeyFold::Scale => {
                let mut v = pr.scale.keys();
                v.extend(m.notes.iter().map(|n| n.key));
                v
            }
            KeyFold::Used => m.notes.iter().map(|n| n.key).collect(),
        };
        if rows.is_empty() {
            rows = (48..=72u8).collect();
        }
        rows.sort_unstable_by(|a, b| b.cmp(a));
        rows.dedup();
        self.rows = rows;
    }

    fn content_h(&self) -> f32 {
        self.rows.len() as f32 * self.row_h
    }

    fn clip(model: &Session) -> Option<(ClipId, &Clip, &MidiClip)> {
        let id = model.editor_clip()?;
        let clip = model.project().clip(id)?;
        Some((id, clip, clip.as_midi()?))
    }

    fn clamp_scroll(&mut self, size: Size, clip_len: MusicalTime) {
        let g = self.layout(size).grid;
        let max_y = (self.content_h() - g.h).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
        let max_x = ((clip_len.quarters() as f32 + 8.0) * self.ppq - g.w * 0.5).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
    }

    /// Centre the view on a newly opened clip's notes, fitting its length.
    fn centre_on(&mut self, size: Size, m: &MidiClip) {
        let g = self.layout(size).grid;
        let mid = if m.notes.is_empty() {
            60
        } else {
            let lo = m.notes.iter().map(|n| n.key).min().unwrap_or(60);
            let hi = m.notes.iter().map(|n| n.key).max().unwrap_or(72);
            ((lo as u16 + hi as u16) / 2) as u8
        };
        let row = self
            .rows
            .iter()
            .position(|k| *k <= mid)
            .unwrap_or(self.rows.len() / 2);
        self.scroll_y = row as f32 * self.row_h - g.h / 2.0;
        self.scroll_x = 0.0;
        let fit = g.w / (m.length.quarters() as f32 + 0.5).max(1.0);
        self.ppq = fit.clamp(16.0, 200.0);
    }

    fn note_rect(&self, n: &MidiNote) -> Option<Rect> {
        let y = self.y_of(n.key)?;
        let x0 = self.x_of(n.start);
        let x1 = self.x_of(n.end());
        Some(Rect::new(x0, y + 1.0, (x1 - x0).max(3.0), self.row_h - 2.0))
    }

    /// Topmost note under `pos` and whether an edge was grabbed
    /// (`Some(true)` = end, `Some(false)` = start).
    fn note_at(&self, m: &MidiClip, pos: Point) -> Option<(MidiNote, Option<bool>)> {
        m.notes.iter().rev().find_map(|n| {
            let r = self.note_rect(n)?;
            if !r.contains(pos) {
                return None;
            }
            let grab = EDGE_GRAB.min(r.w * 0.3);
            let edge = if pos.x >= r.right() - grab {
                Some(true)
            } else if pos.x <= r.x + grab.min(4.0) && r.w > 12.0 {
                Some(false)
            } else {
                None
            };
            Some((*n, edge))
        })
    }

    fn selected<'m>(m: &'m MidiClip, model: &Session) -> Vec<&'m MidiNote> {
        m.notes
            .iter()
            .filter(|n| model.selection.notes.contains(&n.id))
            .collect()
    }
}

fn track_color(model: &Session, clip: &Clip) -> Color {
    let c: TrackColor = clip
        .color
        .or_else(|| model.project().track(clip.track).map(|t| t.color))
        .unwrap_or(TrackColor::palette(0));
    Color::rgb8(c.r, c.g, c.b)
}

impl faderframe_ui_canvas::CanvasView<Session, faderframe_session::Action> for PianoRollView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(
        &mut self,
        p: &mut dyn faderframe_ui_canvas::Painter,
        size: Size,
        model: &Session,
        theme: &Theme,
    ) {
        // The lane may have been chosen from a menu since the last event.
        self.sync_lane(model);
        self.update_toolbar(size.w, model);
        self.paint_view(p, size, model, theme);
    }

    fn event(
        &mut self,
        ev: &faderframe_ui_canvas::ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut faderframe_ui_canvas::EventCx<'_, faderframe_session::Action>,
    ) -> bool {
        self.update_toolbar(size.w, model);
        self.handle_event(ev, size, model, cx)
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.transport().playing || self.drag.is_some()
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        self.tooltip_at(pos, size, model)
    }

    fn min_size(&self) -> Size {
        Size::new(self.kb_w() + 320.0, 240.0)
    }

    fn scroll_info(
        &self,
        axis: faderframe_ui_canvas::ScrollAxis,
        size: Size,
        model: &Session,
    ) -> Option<faderframe_ui_canvas::ScrollInfo> {
        let (_, _, m) = Self::clip(model)?;
        let g = self.layout(size).grid;
        Some(match axis {
            faderframe_ui_canvas::ScrollAxis::Horizontal => faderframe_ui_canvas::ScrollInfo {
                content: (m.length.quarters() as f32 + 8.0) * self.ppq,
                viewport: g.w,
                offset: self.scroll_x,
                start: g.x,
                end: (size.w - g.right()).max(0.0),
            },
            faderframe_ui_canvas::ScrollAxis::Vertical => faderframe_ui_canvas::ScrollInfo {
                content: self.content_h(),
                viewport: g.h,
                offset: self.scroll_y,
                start: g.y,
                end: (size.h - g.bottom()).max(0.0),
            },
        })
    }

    fn set_scroll(&mut self, axis: faderframe_ui_canvas::ScrollAxis, offset: f32) {
        self.auto_centre = false;
        match axis {
            faderframe_ui_canvas::ScrollAxis::Horizontal => self.scroll_x = offset.max(0.0),
            faderframe_ui_canvas::ScrollAxis::Vertical => self.scroll_y = offset.max(0.0),
        }
    }
}

/// Values the expression lane shows (pitch: ±12 semitones; volume −24 to
/// +12 dB).
pub(crate) fn expression_span(kind: ExpressionKind) -> (f32, f32) {
    match kind {
        ExpressionKind::Pitch => (-12.0, 12.0),
        ExpressionKind::Volume => (-24.0, 12.0),
        ExpressionKind::Pan => (-1.0, 1.0),
        ExpressionKind::Pressure
        | ExpressionKind::Timbre
        | ExpressionKind::Vibrato
        | ExpressionKind::Expression => (0.0, 1.0),
    }
}

pub(crate) fn expression_y(area: Rect, kind: ExpressionKind, v: f32) -> f32 {
    let (lo, hi) = expression_span(kind);
    area.bottom() - area.h * ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
}

pub(crate) fn expression_value(area: Rect, kind: ExpressionKind, y: f32) -> f32 {
    let (lo, hi) = expression_span(kind);
    let v = lo + (hi - lo) * ((area.bottom() - y) / area.h.max(1.0)).clamp(0.0, 1.0);
    // Pitch snaps to whole semitones near them, volume to 0 dB, pan to the
    // centre.
    match kind {
        ExpressionKind::Pitch if (v - v.round()).abs() < 0.12 => v.round(),
        ExpressionKind::Volume if v.abs() < 0.4 => 0.0,
        ExpressionKind::Pan if v.abs() < 0.03 => 0.0,
        _ => v,
    }
}
