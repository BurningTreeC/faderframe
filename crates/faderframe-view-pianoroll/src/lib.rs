//! Piano roll: custom-rendered MIDI note editor for the clip opened from
//! the arranger (`Session::editor_clip`). No widget per note: notes,
//! keyboard, grid and velocity lane are painted directly and only what is
//! visible is visited.

#![forbid(unsafe_code)]

use faderframe_core::{ClipId, NoteId};
use faderframe_project::{Command, MidiClip, MidiNote, TrackColor};
use faderframe_session::{Action, SelectMode, Session, TransportAction};
use faderframe_timeline::{GridLineKind, MusicalTime, for_each_grid_line};
use faderframe_ui_canvas::{
    Align, CanvasView, Color, Cursor, EventCx, FontFamily, HostRequest, Key, MenuItem, Modifiers,
    Paint, Painter, Point, PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme,
    ViewEvent,
};

const KEYS: i32 = 128;
const EDGE_GRAB: f32 = 6.0;
const DRAG_THRESHOLD: f32 = 3.0;
const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// "C4" style name (middle C = C4 = MIDI 60).
pub fn note_name(key: u8) -> String {
    format!("{}{}", NAMES[(key % 12) as usize], key as i32 / 12 - 1)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Ruler(MusicalTime),
    Keyboard(u8),
    Note { note: NoteId, edge: bool },
    Grid { at: MusicalTime, key: u8 },
    Velocity(Option<NoteId>),
}

#[derive(Clone, Debug, PartialEq)]
enum Drag {
    /// Moving the selection; originals are captured at press time.
    Move {
        grab_note: NoteId,
        origin: Point,
        grab_start: MusicalTime,
        grab_key: u8,
        originals: Vec<MidiNote>,
        moved: bool,
    },
    Resize {
        note: MidiNote,
    },
    /// A freshly drawn note whose length follows the pointer.
    Draw {
        start: MusicalTime,
        key: u8,
    },
    Velocity {
        note: MidiNote,
        origin_y: f32,
    },
    Scrub,
}

pub struct PianoRollView {
    theme: Theme,
    ppq: f32,
    scroll_x: f32,
    scroll_y: f32,
    drag: Option<Drag>,
    centred_for: Option<ClipId>,
    /// Keep re-centring on layout changes until the user scrolls or zooms.
    auto_centre: bool,
    last_size: Size,
    hover_key: Option<u8>,
}

impl PianoRollView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            ppq: 90.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            drag: None,
            centred_for: None,
            auto_centre: true,
            last_size: Size::default(),
            hover_key: None,
        }
    }

    fn kb_w(&self) -> f32 {
        self.theme.piano.keyboard_width
    }

    fn ruler_h(&self) -> f32 {
        self.theme.piano.ruler_height
    }

    fn row_h(&self) -> f32 {
        self.theme.piano.row_height
    }

    fn vel_h(&self, size: Size) -> f32 {
        self.theme.piano.velocity_height.min(size.h * 0.3)
    }

    fn grid_rect(&self, size: Size) -> Rect {
        Rect::new(
            self.kb_w(),
            self.ruler_h(),
            (size.w - self.kb_w()).max(0.0),
            (size.h - self.ruler_h() - self.vel_h(size)).max(0.0),
        )
    }

    fn velocity_rect(&self, size: Size) -> Rect {
        let g = self.grid_rect(size);
        Rect::new(g.x, g.bottom(), g.w, self.vel_h(size))
    }

    /// x of a clip-relative time.
    pub fn x_of(&self, rel: MusicalTime) -> f32 {
        self.kb_w() + rel.quarters() as f32 * self.ppq - self.scroll_x
    }

    pub fn time_at(&self, x: f32) -> MusicalTime {
        MusicalTime::from_quarters(((x - self.kb_w() + self.scroll_x) / self.ppq) as f64)
    }

    pub fn y_of(&self, key: u8) -> f32 {
        self.ruler_h() + (127 - key as i32) as f32 * self.row_h() - self.scroll_y
    }

    pub fn key_at(&self, y: f32) -> u8 {
        let row = ((y - self.ruler_h() + self.scroll_y) / self.row_h()).floor() as i32;
        (127 - row).clamp(0, 127) as u8
    }

    fn clip(model: &Session) -> Option<(ClipId, &faderframe_project::Clip, &MidiClip)> {
        let id = model.editor_clip()?;
        let clip = model.project().clip(id)?;
        Some((id, clip, clip.as_midi()?))
    }

    fn clamp_scroll(&mut self, size: Size, clip_len: MusicalTime) {
        let g = self.grid_rect(size);
        let max_y = (KEYS as f32 * self.row_h() - g.h).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
        let max_x = ((clip_len.quarters() as f32 + 8.0) * self.ppq - g.w * 0.5).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
    }

    /// Centre the view on a newly opened clip's notes.
    fn centre_on(&mut self, size: Size, m: &MidiClip) {
        let g = self.grid_rect(size);
        let mid = if m.notes.is_empty() {
            60.0
        } else {
            let lo = m.notes.iter().map(|n| n.key).min().unwrap_or(60) as f32;
            let hi = m.notes.iter().map(|n| n.key).max().unwrap_or(72) as f32;
            (lo + hi) / 2.0
        };
        self.scroll_y = (127.0 - mid) * self.row_h() - g.h / 2.0;
        self.scroll_x = 0.0;
        let fit = g.w / (m.length.quarters() as f32 + 1.0).max(1.0);
        self.ppq = fit.clamp(24.0, 160.0);
    }

    fn note_rect(&self, n: &MidiNote) -> Rect {
        let x0 = self.x_of(n.start);
        let x1 = self.x_of(n.end());
        Rect::new(
            x0,
            self.y_of(n.key) + 1.0,
            (x1 - x0).max(3.0),
            self.row_h() - 2.0,
        )
    }

    pub fn hit_test(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        let (_, _, m) = Self::clip(model)?;
        let g = self.grid_rect(size);
        if pos.y < self.ruler_h() {
            return (pos.x >= self.kb_w())
                .then(|| Hit::Ruler(self.time_at(pos.x).max(MusicalTime::ZERO)));
        }
        if pos.x < self.kb_w() {
            return (pos.y < g.bottom()).then(|| Hit::Keyboard(self.key_at(pos.y)));
        }
        if pos.y >= g.bottom() {
            let note = m.notes.iter().rev().find(|n| {
                let x = self.x_of(n.start);
                (pos.x - x).abs() <= 4.0
            });
            return Some(Hit::Velocity(note.map(|n| n.id)));
        }
        for n in m.notes.iter().rev() {
            let r = self.note_rect(n);
            if r.contains(pos) {
                return Some(Hit::Note {
                    note: n.id,
                    edge: pos.x >= r.right() - EDGE_GRAB.min(r.w * 0.4),
                });
            }
        }
        Some(Hit::Grid {
            at: self.time_at(pos.x).max(MusicalTime::ZERO),
            key: self.key_at(pos.y),
        })
    }

    fn snap_rel(
        &self,
        rel: MusicalTime,
        clip_start: MusicalTime,
        model: &Session,
        mods: Modifiers,
    ) -> MusicalTime {
        if mods.alt {
            return rel.max(MusicalTime::ZERO);
        }
        let abs = model
            .editor
            .snap(clip_start + rel, &model.project().timeline.meter);
        (abs - clip_start).max(MusicalTime::ZERO)
    }

    fn grid_step(&self, clip_start: MusicalTime, model: &Session) -> MusicalTime {
        model
            .editor
            .step(clip_start, &model.project().timeline.meter)
    }

    // --- painting --------------------------------------------------------------

    fn paint_keyboard(&self, p: &mut dyn Painter, rect: Rect, held: &[u8]) {
        let pr = &self.theme.piano;
        p.fill(rect, pr.key_white_shade);
        p.push_clip(rect);
        let top_key = self.key_at(rect.y);
        let bottom_key = self.key_at(rect.bottom());
        for key in bottom_key..=top_key {
            let y = self.y_of(key);
            let row = Rect::new(rect.x, y, rect.w, self.row_h());
            let lit = held.contains(&key) || self.hover_key == Some(key);
            if is_black(key) {
                p.fill(row, pr.key_white);
                let black = Rect::new(rect.x, y + 1.0, rect.w * 0.62, self.row_h() - 2.0);
                let c = if lit {
                    self.theme.ui.selection.darken(0.3)
                } else {
                    pr.key_black
                };
                p.fill_rounded(black, 1.5, &Paint::horizontal(black, c.lighten(0.15), c));
            } else {
                let c = if lit {
                    self.theme.ui.selection.lighten(0.5)
                } else {
                    pr.key_white
                };
                p.fill_rect(row, &Paint::horizontal(row, c.darken(0.06), c));
                p.hline(
                    rect.x,
                    rect.right(),
                    row.bottom() - 0.5,
                    pr.key_white_shade.darken(0.2),
                );
            }
            if key % 12 == 0 {
                p.text(
                    &note_name(key),
                    Rect::new(rect.x, y, rect.w - 4.0, self.row_h()),
                    &TextStyle::new(self.theme.fonts.tiny, pr.key_text).right(),
                );
            }
        }
        p.pop_clip();
        p.vline(
            rect.right() - 1.0,
            rect.y,
            rect.bottom(),
            Color::rgba(0.0, 0.0, 0.0, 0.6),
        );
    }

    fn paint_grid(
        &self,
        p: &mut dyn Painter,
        g: Rect,
        clip_start: MusicalTime,
        len: MusicalTime,
        model: &Session,
    ) {
        let pr = &self.theme.piano;
        let top_key = self.key_at(g.y);
        let bottom_key = self.key_at(g.bottom());
        for key in bottom_key..=top_key {
            let y = self.y_of(key);
            let row = Rect::new(g.x, y, g.w, self.row_h());
            p.fill(
                row,
                if is_black(key) {
                    pr.black_row
                } else {
                    pr.white_row
                },
            );
            if key % 12 == 0 {
                p.hline(g.x, g.right(), row.bottom(), pr.octave_line);
            }
        }
        let meter = &model.project().timeline.meter;
        let start = clip_start + self.time_at(g.x).max(MusicalTime::ZERO);
        let end = clip_start + self.time_at(g.right());
        let grid = if self.ppq >= 120.0 {
            faderframe_timeline::GridDivision::Note(16)
        } else if self.ppq >= 50.0 {
            faderframe_timeline::GridDivision::Note(8)
        } else {
            faderframe_timeline::GridDivision::Beat
        };
        for_each_grid_line(start, end, grid, meter, |t, kind| {
            let x = self.x_of(t - clip_start);
            let c = match kind {
                GridLineKind::Bar => pr.bar_line,
                GridLineKind::Beat => pr.beat_line,
                GridLineKind::Subdivision => pr.sub_line,
            };
            p.vline(x, g.y, g.bottom(), c);
        });
        // Dim everything past the clip end.
        let x_end = self.x_of(len);
        if x_end < g.right() {
            let r = Rect::new(x_end.max(g.x), g.y, g.right() - x_end.max(g.x), g.h);
            p.fill(r, Color::rgba(0.0, 0.0, 0.0, 0.35));
            p.vline(x_end, g.y, g.bottom(), self.theme.ui.accent.with_alpha(0.6));
        }
    }

    fn note_color(base: Color, velocity: u8) -> Color {
        base.darken(0.25)
            .mix(base.lighten(0.35), velocity as f32 / 127.0)
    }

    fn paint_notes(
        &self,
        p: &mut dyn Painter,
        g: Rect,
        m: &MidiClip,
        color: Color,
        model: &Session,
    ) {
        let top_key = self.key_at(g.y);
        let bottom_key = self.key_at(g.bottom());
        for n in &m.notes {
            if n.key < bottom_key || n.key > top_key {
                continue;
            }
            let r = self.note_rect(n);
            if r.right() < g.x || r.x > g.right() {
                continue;
            }
            let selected = model.selection.notes.contains(&n.id);
            let c = Self::note_color(color, n.velocity);
            p.shadow(r, 2.0, Color::rgba(0.0, 0.0, 0.0, 0.4), 0.0, 1.0, 2.0);
            p.fill_rounded(r, 2.0, &Paint::vertical(r, c.lighten(0.12), c.darken(0.08)));
            if selected {
                p.stroke_rounded(r, 2.0, 1.5, self.theme.ui.selection.lighten(0.4));
            } else {
                p.stroke_rounded(r, 2.0, 1.0, c.darken(0.45));
            }
            if r.w > 28.0 && self.row_h() >= 11.0 {
                let text_c = if c.luminance() > 0.45 {
                    Color::hex(0x141414)
                } else {
                    Color::hex(0xf0eee8)
                };
                p.text(
                    &note_name(n.key),
                    r.inset_xy(3.0, 0.0),
                    &TextStyle::new(self.theme.fonts.tiny, text_c).family(FontFamily::Condensed),
                );
            }
            // Resize grip.
            p.fill(
                Rect::new(r.right() - 2.0, r.y + 2.0, 1.0, r.h - 4.0),
                Color::rgba(1.0, 1.0, 1.0, 0.35),
            );
        }
    }

    fn paint_velocity(
        &self,
        p: &mut dyn Painter,
        v: Rect,
        m: &MidiClip,
        color: Color,
        model: &Session,
    ) {
        let pr = &self.theme.piano;
        p.fill(v, pr.velocity_bg);
        p.hline(v.x, v.right(), v.y, Color::rgba(0.0, 0.0, 0.0, 0.7));
        for frac in [0.25f32, 0.5, 0.75] {
            p.hline(
                v.x,
                v.right(),
                v.bottom() - (v.h - 8.0) * frac,
                Color::rgba(1.0, 1.0, 1.0, 0.04),
            );
        }
        p.push_clip(v);
        for n in &m.notes {
            let x = self.x_of(n.start);
            if x < v.x - 4.0 || x > v.right() + 4.0 {
                continue;
            }
            let h = (v.h - 8.0) * n.velocity as f32 / 127.0;
            let selected = model.selection.notes.contains(&n.id);
            let c = if selected {
                self.theme.ui.selection
            } else {
                Self::note_color(color, n.velocity)
            };
            p.fill(Rect::new(x - 1.0, v.bottom() - h, 3.0, h), c);
            p.circle(Point::new(x + 0.5, v.bottom() - h), 3.0, c.lighten(0.2));
        }
        p.pop_clip();
    }

    fn paint_ruler(&self, p: &mut dyn Painter, r: Rect, clip_start: MusicalTime, model: &Session) {
        let th = &self.theme;
        p.fill_rect(
            r,
            &Paint::vertical(r, th.arranger.ruler_bg.lighten(0.04), th.arranger.ruler_bg),
        );
        p.push_clip(r);
        let meter = &model.project().timeline.meter;
        let start = clip_start + self.time_at(r.x).max(MusicalTime::ZERO);
        let end = clip_start + self.time_at(r.right());
        let style = TextStyle::new(th.fonts.small, th.arranger.ruler_text).family(FontFamily::Mono);
        for_each_grid_line(
            start,
            end,
            faderframe_timeline::GridDivision::Beat,
            meter,
            |t, kind| {
                let x = self.x_of(t - clip_start);
                match kind {
                    GridLineKind::Bar => {
                        p.vline(
                            x,
                            r.y + 4.0,
                            r.bottom(),
                            th.arranger.ruler_text.with_alpha(0.45),
                        );
                        p.text(
                            &format!("{}", meter.bar_at(t) + 1),
                            Rect::new(x + 4.0, r.y, 50.0, r.h),
                            &style,
                        );
                    }
                    _ => p.vline(
                        x,
                        r.bottom() - 5.0,
                        r.bottom(),
                        th.arranger.ruler_text.with_alpha(0.25),
                    ),
                }
            },
        );
        p.pop_clip();
        p.hline(r.x, r.right(), r.bottom() - 1.0, th.arranger.header_border);
    }

    // --- interaction ---------------------------------------------------------------

    fn selected_notes(m: &MidiClip, model: &Session) -> Vec<MidiNote> {
        m.notes
            .iter()
            .filter(|n| model.selection.notes.contains(&n.id))
            .copied()
            .collect()
    }

    fn press(
        &mut self,
        pos: Point,
        clicks: u32,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        cx.request(HostRequest::GrabFocus);
        let Some((clip_id, clip, m)) = Self::clip(model) else {
            return false;
        };
        let Some(hit) = self.hit_test(pos, size, model) else {
            return false;
        };
        match hit {
            Hit::Ruler(rel) => {
                cx.emit(Action::Transport(TransportAction::Locate(clip.start + rel)));
                self.drag = Some(Drag::Scrub);
            }
            Hit::Keyboard(key) => {
                // Select every note of that pitch (auditioning needs live
                // MIDI input to the engine, which is planned).
                let notes: Vec<NoteId> = m
                    .notes
                    .iter()
                    .filter(|n| n.key == key)
                    .map(|n| n.id)
                    .collect();
                let mode = if mods.toggle() {
                    SelectMode::Toggle
                } else {
                    SelectMode::Replace
                };
                cx.emit(Action::SelectNotes { notes, mode });
            }
            Hit::Note { note, edge } => {
                let Some(n) = m.note(note).copied() else {
                    return false;
                };
                if clicks >= 2 {
                    cx.emit(Action::Edit(Command::RemoveNote {
                        clip: clip_id,
                        note,
                    }));
                    return true;
                }
                let already = model.selection.notes.contains(&note);
                if mods.toggle() {
                    cx.emit(Action::SelectNotes {
                        notes: vec![note],
                        mode: SelectMode::Toggle,
                    });
                } else if !already {
                    cx.emit(Action::SelectNotes {
                        notes: vec![note],
                        mode: SelectMode::Replace,
                    });
                }
                if edge {
                    cx.emit(Action::BeginGesture("Resize Note".into()));
                    self.drag = Some(Drag::Resize { note: n });
                    cx.set_cursor(Cursor::ResizeHorizontal);
                } else {
                    let mut originals = if already {
                        Self::selected_notes(m, model)
                    } else {
                        vec![n]
                    };
                    if !originals.iter().any(|o| o.id == n.id) {
                        originals.push(n);
                    }
                    self.drag = Some(Drag::Move {
                        grab_note: note,
                        origin: pos,
                        grab_start: n.start,
                        grab_key: n.key,
                        originals,
                        moved: false,
                    });
                    cx.set_cursor(Cursor::Grabbing);
                }
            }
            Hit::Grid { at, key } => {
                let step = self.grid_step(clip.start, model);
                let start = if mods.alt {
                    at
                } else {
                    let abs = faderframe_timeline::snap_floor(
                        clip.start + at,
                        model.editor.grid,
                        &model.project().timeline.meter,
                    );
                    (abs - clip.start).max(MusicalTime::ZERO)
                };
                if start >= m.length {
                    return true;
                }
                cx.emit(Action::BeginGesture("Draw Note".into()));
                cx.emit(Action::AddNote {
                    clip: clip_id,
                    start,
                    length: step.min(m.length - start),
                    key,
                    velocity: 100,
                });
                self.drag = Some(Drag::Draw { start, key });
            }
            Hit::Velocity(Some(note)) => {
                let Some(n) = m.note(note).copied() else {
                    return false;
                };
                cx.emit(Action::SelectNotes {
                    notes: vec![note],
                    mode: SelectMode::Replace,
                });
                cx.emit(Action::BeginGesture("Velocity".into()));
                self.drag = Some(Drag::Velocity {
                    note: n,
                    origin_y: pos.y,
                });
                cx.set_cursor(Cursor::ResizeVertical);
            }
            Hit::Velocity(None) => {
                cx.emit(Action::SelectNotes {
                    notes: vec![],
                    mode: SelectMode::Replace,
                });
            }
        }
        true
    }

    fn drag_move(
        &mut self,
        pos: Point,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some((clip_id, clip, m)) = Self::clip(model) else {
            return;
        };
        let drag = self.drag.clone();
        match drag {
            Some(Drag::Scrub) => {
                cx.emit(Action::Transport(TransportAction::Locate(
                    clip.start + self.time_at(pos.x).max(MusicalTime::ZERO),
                )));
            }
            Some(Drag::Move {
                grab_note,
                origin,
                grab_start,
                grab_key,
                originals,
                moved,
            }) => {
                if !moved {
                    if pos.distance(origin) < DRAG_THRESHOLD {
                        return;
                    }
                    cx.emit(Action::BeginGesture("Move Notes".into()));
                    if let Some(Drag::Move { moved, .. }) = &mut self.drag {
                        *moved = true;
                    }
                }
                let raw = grab_start + (self.time_at(pos.x) - self.time_at(origin.x));
                let new_start = self.snap_rel(raw, clip.start, model, mods);
                let dt = new_start - grab_start;
                let dk = self.key_at(pos.y) as i32 - grab_key as i32;
                let _ = grab_note;
                for o in &originals {
                    let start = (o.start + dt).max(MusicalTime::ZERO);
                    let key = (o.key as i32 + dk).clamp(0, 127) as u8;
                    if let Some(cur) = m.note(o.id)
                        && (cur.start != start || cur.key != key)
                    {
                        cx.emit(Action::Edit(Command::UpdateNote {
                            clip: clip_id,
                            note: MidiNote { start, key, ..*cur },
                        }));
                    }
                }
            }
            Some(Drag::Resize { note }) => {
                let end = self.snap_rel(self.time_at(pos.x), clip.start, model, mods);
                let min = if mods.alt {
                    MusicalTime(TICK_MIN)
                } else {
                    self.grid_step(clip.start, model)
                };
                let length = (end - note.start).max(min);
                if let Some(cur) = m.note(note.id)
                    && cur.length != length
                {
                    cx.emit(Action::Edit(Command::UpdateNote {
                        clip: clip_id,
                        note: MidiNote { length, ..*cur },
                    }));
                }
            }
            Some(Drag::Draw { start, key }) => {
                // The note added at press time is the single selected note.
                let Some(cur) = m
                    .notes
                    .iter()
                    .find(|n| {
                        model.selection.notes.contains(&n.id) && n.start == start && n.key == key
                    })
                    .copied()
                else {
                    return;
                };
                let step = self.grid_step(clip.start, model);
                let end = self.snap_rel(self.time_at(pos.x), clip.start, model, mods);
                let length = (end - start)
                    .max(step)
                    .min(m.length - start)
                    .max(MusicalTime(1));
                if cur.length != length {
                    cx.emit(Action::Edit(Command::UpdateNote {
                        clip: clip_id,
                        note: MidiNote { length, ..cur },
                    }));
                }
            }
            Some(Drag::Velocity { note, origin_y }) => {
                let v = self.velocity_rect(size);
                let delta = (origin_y - pos.y) / (v.h - 8.0).max(1.0) * 127.0;
                let velocity = (note.velocity as f32 + delta).round().clamp(1.0, 127.0) as u8;
                if let Some(cur) = m.note(note.id)
                    && cur.velocity != velocity
                {
                    cx.emit(Action::Edit(Command::UpdateNote {
                        clip: clip_id,
                        note: MidiNote { velocity, ..*cur },
                    }));
                }
            }
            None => {}
        }
    }

    fn release(&mut self, cx: &mut EventCx<'_, Action>) {
        match self.drag.take() {
            Some(Drag::Move { moved: true, .. })
            | Some(Drag::Resize { .. })
            | Some(Drag::Draw { .. })
            | Some(Drag::Velocity { .. }) => cx.emit(Action::EndGesture),
            _ => {}
        }
        cx.set_cursor(Cursor::Default);
    }

    fn nudge(
        &self,
        m: &MidiClip,
        clip: ClipId,
        model: &Session,
        dt: MusicalTime,
        dk: i32,
        cx: &mut EventCx<'_, Action>,
    ) {
        let sel = Self::selected_notes(m, model);
        if sel.is_empty() {
            return;
        }
        cx.emit(Action::BeginGesture("Nudge Notes".into()));
        for n in sel {
            cx.emit(Action::Edit(Command::UpdateNote {
                clip,
                note: MidiNote {
                    start: (n.start + dt).max(MusicalTime::ZERO),
                    key: (n.key as i32 + dk).clamp(0, 127) as u8,
                    ..n
                },
            }));
        }
        cx.emit(Action::EndGesture);
    }
}

const TICK_MIN: i64 = 1000;

fn track_color(model: &Session, clip: &faderframe_project::Clip) -> Color {
    let c: TrackColor = clip
        .color
        .or_else(|| model.project().track(clip.track).map(|t| t.color))
        .unwrap_or(TrackColor::palette(0));
    Color::rgb8(c.r, c.g, c.b)
}

impl CanvasView<Session, Action> for PianoRollView {
    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        p.fill(Rect::from_size(size), theme.piano.background);
        let Some((id, clip, m)) = Self::clip(model) else {
            p.text(
                "Double-click a MIDI clip in the arranger to edit it — or double-click an instrument lane to create one",
                Rect::from_size(size),
                &TextStyle::new(theme.fonts.normal, theme.ui.text_faint).center(),
            );
            return;
        };
        if self.centred_for != Some(id) {
            self.auto_centre = true;
        }
        if self.centred_for != Some(id) || (self.auto_centre && size != self.last_size) {
            self.centre_on(size, m);
            self.centred_for = Some(id);
        }
        self.last_size = size;
        self.clamp_scroll(size, m.length);
        let g = self.grid_rect(size);
        let color = track_color(model, clip);
        p.push_clip(g);
        self.paint_grid(p, g, clip.start, m.length, model);
        self.paint_notes(p, g, m, color, model);
        // Playhead (clip-relative).
        let ph = model.playhead();
        if ph >= clip.start && ph <= clip.start + m.length {
            let x = self.x_of(ph - clip.start);
            p.fill(Rect::new(x - 0.75, g.y, 1.5, g.h), theme.arranger.playhead);
        }
        p.pop_clip();
        let held: Vec<u8> = Self::selected_notes(m, model)
            .iter()
            .map(|n| n.key)
            .collect();
        self.paint_keyboard(p, Rect::new(0.0, g.y, self.kb_w(), g.h), &held);
        self.paint_velocity(p, self.velocity_rect(size), m, color, model);
        let vlabel = Rect::new(0.0, g.bottom(), self.kb_w(), self.vel_h(size));
        p.fill(vlabel, theme.piano.velocity_bg.darken(0.2));
        faderframe_ui_canvas::controls::engraved(
            p,
            "VELOCITY",
            vlabel.inset_xy(6.0, 0.0),
            theme,
            Align::Start,
        );
        self.paint_ruler(
            p,
            Rect::new(self.kb_w(), 0.0, size.w - self.kb_w(), self.ruler_h()),
            clip.start,
            model,
        );
        let corner = Rect::new(0.0, 0.0, self.kb_w(), self.ruler_h());
        p.fill(corner, theme.arranger.ruler_bg);
        p.text(
            &clip.name,
            corner.inset_xy(6.0, 0.0),
            &TextStyle::new(theme.fonts.small, theme.ui.text).bold(),
        );
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                modifiers,
                clicks,
            } => self.press(pos, clicks, modifiers, size, model, cx),
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                let Some((clip, _, m)) = Self::clip(model) else {
                    return false;
                };
                if let Some(Hit::Note { note, .. }) = self.hit_test(pos, size, model) {
                    let n = m.note(note).copied();
                    let mut items = vec![MenuItem::new(
                        "Delete Note",
                        Action::Edit(Command::RemoveNote { clip, note }),
                    )];
                    if let Some(n) = n {
                        for v in [40u8, 64, 90, 110, 127] {
                            let mut item = MenuItem::new(
                                format!("Velocity {v}"),
                                Action::Edit(Command::UpdateNote {
                                    clip,
                                    note: MidiNote { velocity: v, ..n },
                                }),
                            )
                            .checked(n.velocity == v);
                            if v == 40 {
                                item = item.separated();
                            }
                            items.push(item);
                        }
                    }
                    cx.request(HostRequest::ContextMenu { at: pos, items });
                    return true;
                }
                false
            }
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } => {
                self.drag_move(pos, modifiers, size, model, cx);
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let hit = self.hit_test(pos, size, model);
                let key = match hit {
                    Some(Hit::Keyboard(k)) => Some(k),
                    _ => None,
                };
                if key != self.hover_key {
                    self.hover_key = key;
                    cx.redraw();
                }
                cx.set_cursor(match hit {
                    Some(Hit::Note { edge: true, .. }) => Cursor::ResizeHorizontal,
                    Some(Hit::Note { .. }) => Cursor::Grab,
                    Some(Hit::Grid { .. }) => Cursor::Crosshair,
                    Some(Hit::Velocity(Some(_))) => Cursor::ResizeVertical,
                    _ => Cursor::Default,
                });
                false
            }
            ViewEvent::PointerUp { .. } => {
                self.release(cx);
                true
            }
            ViewEvent::PointerLeave => {
                if self.hover_key.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll {
                pos,
                dx,
                dy,
                modifiers,
                precise,
            } => {
                let len = Self::clip(model).map_or(MusicalTime::QUARTER, |(_, _, m)| m.length);
                self.auto_centre = false;
                if modifiers.ctrl {
                    let steps = if precise { dy / 30.0 } else { dy };
                    let t = self.time_at(pos.x);
                    self.ppq = (self.ppq * 1.18f32.powf(-steps)).clamp(8.0, 900.0);
                    self.scroll_x = t.quarters() as f32 * self.ppq - (pos.x - self.kb_w());
                } else {
                    let (mut hx, mut vy) = (dx, dy);
                    if modifiers.shift && dx == 0.0 {
                        hx = dy;
                        vy = 0.0;
                    }
                    let unit = if precise { 1.0 } else { self.row_h() * 3.0 };
                    self.scroll_x += hx * unit;
                    self.scroll_y += vy * unit;
                }
                self.clamp_scroll(size, len);
                cx.redraw();
                true
            }
            ViewEvent::Key { key, modifiers } => {
                let Some((clip, c, m)) = Self::clip(model) else {
                    return false;
                };
                let step = self.grid_step(c.start, model);
                match key {
                    Key::Delete | Key::Backspace => cx.emit(Action::DeleteSelection),
                    Key::Char('a') | Key::Char('A') if modifiers.ctrl => {
                        cx.emit(Action::SelectNotes {
                            notes: m.notes.iter().map(|n| n.id).collect(),
                            mode: SelectMode::Replace,
                        });
                    }
                    Key::Up => self.nudge(
                        m,
                        clip,
                        model,
                        MusicalTime::ZERO,
                        if modifiers.shift { 12 } else { 1 },
                        cx,
                    ),
                    Key::Down => self.nudge(
                        m,
                        clip,
                        model,
                        MusicalTime::ZERO,
                        if modifiers.shift { -12 } else { -1 },
                        cx,
                    ),
                    Key::Left => self.nudge(m, clip, model, -step, 0, cx),
                    Key::Right => self.nudge(m, clip, model, step, 0, cx),
                    _ => return false,
                }
                true
            }
            _ => false,
        }
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.transport().playing
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let (_, clip, m) = Self::clip(model)?;
        match self.hit_test(pos, size, model)? {
            Hit::Note { note, .. } => {
                let n = m.note(note)?;
                Some(format!(
                    "{} · velocity {} · {}",
                    note_name(n.key),
                    n.velocity,
                    model.project().timeline.format_bbt(clip.start + n.start)
                ))
            }
            Hit::Grid { key, .. } => {
                Some(format!("{} · click/drag to draw a note", note_name(key)))
            }
            Hit::Keyboard(key) => Some(note_name(key)),
            _ => None,
        }
    }

    fn min_size(&self) -> Size {
        Size::new(self.kb_w() + 200.0, 200.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        let (_, _, m) = Self::clip(model)?;
        let g = self.grid_rect(size);
        Some(match axis {
            ScrollAxis::Horizontal => ScrollInfo {
                content: (m.length.quarters() as f32 + 8.0) * self.ppq,
                viewport: g.w,
                offset: self.scroll_x,
            },
            ScrollAxis::Vertical => ScrollInfo {
                content: KEYS as f32 * self.row_h(),
                viewport: g.h,
                offset: self.scroll_y,
            },
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        self.auto_centre = false;
        match axis {
            ScrollAxis::Horizontal => self.scroll_x = offset.max(0.0),
            ScrollAxis::Vertical => self.scroll_y = offset.max(0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_engine::EngineConfig;
    use faderframe_ui_canvas::RecordingPainter;

    fn session() -> Session {
        Session::demo(EngineConfig::default()).unwrap()
    }

    fn run(view: &mut PianoRollView, ev: ViewEvent, size: Size, s: &Session) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(&ev, size, s, &mut cx);
        actions
    }

    #[test]
    fn note_names() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(69), "A4");
        assert_eq!(note_name(0), "C-1");
        assert!(is_black(61) && !is_black(64));
    }

    #[test]
    fn empty_state_and_painting() {
        let s = session();
        let theme = Theme::default();
        let mut view = PianoRollView::new(theme.clone());
        let size = Size::new(900.0, 500.0);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &theme);
        assert!(
            p.texts().iter().any(|t| t.starts_with("C")),
            "note names drawn"
        );
        assert!(p.balanced_clips());
        let mut empty = Session::new(
            faderframe_project::Project::new("x", 48_000),
            None,
            EngineConfig::default(),
        )
        .unwrap();
        empty.tick(0.0);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &empty, &theme);
        assert!(p.texts()[0].starts_with("Double-click"));
    }

    #[test]
    fn draw_note_then_drag_length_is_one_undo_step() {
        let mut s = session();
        let theme = Theme::default();
        let mut view = PianoRollView::new(theme.clone());
        let size = Size::new(1000.0, 600.0);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &theme); // centres the view
        let clip = s.editor_clip().unwrap();
        let before = s
            .project()
            .clip(clip)
            .unwrap()
            .as_midi()
            .unwrap()
            .notes
            .len();
        // Find an empty spot: key 40 is far below the melody.
        let key = 40u8;
        view.set_scroll(
            ScrollAxis::Vertical,
            (127.0 - key as f32) * view.row_h() - 100.0,
        );
        let y = view.y_of(key) + view.row_h() / 2.0;
        let x = view.x_of(MusicalTime::from_quarters(0.1));
        let press = ViewEvent::PointerDown {
            pos: Point::new(x, y),
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks: 1,
        };
        for a in run(&mut view, press, size, &s) {
            s.dispatch(a).unwrap();
        }
        let x2 = view.x_of(MusicalTime::from_quarters(3.0));
        let drag = ViewEvent::PointerMove {
            pos: Point::new(x2, y),
            modifiers: Modifiers::NONE,
            dragging: true,
        };
        for a in run(&mut view, drag, size, &s) {
            s.dispatch(a).unwrap();
        }
        let up = ViewEvent::PointerUp {
            pos: Point::new(x2, y),
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        };
        for a in run(&mut view, up, size, &s) {
            s.dispatch(a).unwrap();
        }
        let m = s.project().clip(clip).unwrap().as_midi().unwrap().clone();
        assert_eq!(m.notes.len(), before + 1);
        let n = m.notes.iter().find(|n| n.key == key).unwrap();
        assert_eq!(n.start, MusicalTime::ZERO, "snapped to the beat");
        assert_eq!(n.length, MusicalTime::from_quarters_i(3));
        s.dispatch(Action::Undo).unwrap();
        assert_eq!(
            s.project()
                .clip(clip)
                .unwrap()
                .as_midi()
                .unwrap()
                .notes
                .len(),
            before
        );
    }
}
