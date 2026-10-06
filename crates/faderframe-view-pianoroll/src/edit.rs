//! Interaction: tools, drags (previewed, committed on release as one
//! session action), lanes, menus, keyboard shortcuts, zoom and scroll.
//!
//! Modifiers while dragging notes: Shift = no snap, Alt = copy. Clicking
//! with Ctrl toggles a note in the selection, Shift adds to it.

use crate::{DRAG_THRESHOLD, Drag, LaneKind, PianoRollView, Tool, note_name};
use faderframe_core::NoteId;
use faderframe_project::midi_ops::QuantizeSettings;
use faderframe_project::{Clip, ControllerPoint, MidiClip, MidiController, MidiNote};
use faderframe_session::{
    Action, NoteLength, NoteOp, PianoRollSettings, SelectMode, Session, StepInput,
};
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{
    Cursor, EventCx, HostRequest, Key, MenuItem, Modifiers, Point, PointerButton, Rect, Size,
    ViewEvent,
};

/// Controllers offered in the lane menu.
const LANES: [MidiController; 10] = [
    MidiController::MOD_WHEEL,
    MidiController::PitchBend,
    MidiController::SUSTAIN,
    MidiController::Cc { number: 11 },
    MidiController::Cc { number: 7 },
    MidiController::Cc { number: 10 },
    MidiController::ChannelPressure,
    MidiController::Cc { number: 2 },
    MidiController::Cc { number: 74 },
    MidiController::Cc { number: 71 },
];

impl PianoRollView {
    /// Follow the lane chosen in the editor settings.
    pub(crate) fn sync_lane(&mut self, model: &Session) {
        if let Some(k) = model.editor.piano.expression {
            self.lane = LaneKind::Expression(k);
            return;
        }
        match model.editor.piano.lane {
            Some((c, ch)) => {
                self.lane = LaneKind::Controller(c);
                self.lane_channel = ch;
            }
            None => {
                self.lane = LaneKind::Velocity;
                self.lane_channel = 0;
            }
        }
    }

    fn step_at(&self, model: &Session, abs: MusicalTime) -> MusicalTime {
        model.editor.step(abs, &model.project().timeline.meter)
    }

    /// Length of new notes at `abs`.
    pub(crate) fn new_note_length(&self, model: &Session, abs: MusicalTime) -> MusicalTime {
        let meter = &model.project().timeline.meter;
        match model.editor.piano.note_length {
            NoteLength::Grid => self.step_at(model, abs),
            NoteLength::Last => self.last_length,
            NoteLength::Fixed(d) => d.step(meter.signature_at(abs)),
        }
        .max(MusicalTime(1))
    }

    /// Nearest grid point (clip-relative), unless snapping is off or
    /// bypassed.
    pub(crate) fn snap_rel(
        &self,
        rel: MusicalTime,
        clip_start: MusicalTime,
        model: &Session,
        bypass: bool,
    ) -> MusicalTime {
        if bypass {
            return rel;
        }
        let abs = model
            .editor
            .snap(clip_start + rel, &model.project().timeline.meter);
        abs - clip_start
    }

    /// Previous grid point (clip-relative).
    pub(crate) fn snap_floor_rel(
        &self,
        rel: MusicalTime,
        clip_start: MusicalTime,
        model: &Session,
    ) -> MusicalTime {
        if !model.editor.snap {
            return rel.max(MusicalTime::ZERO);
        }
        let abs = faderframe_timeline::snap_floor(
            clip_start + rel,
            model.editor.grid,
            &model.project().timeline.meter,
        );
        (abs - clip_start).max(MusicalTime::ZERO)
    }

    /// `key`, moved into the scale (the key track's at `at`, project time)
    /// when scale snap is on.
    fn scale_key(&self, key: u8, at: MusicalTime, model: &Session) -> u8 {
        let scale = model.piano_scale_at(at);
        if model.editor.piano.scale_snap && !scale.is_chromatic() {
            scale.nearest(key)
        } else {
            key
        }
    }

    /// Where the earliest of `notes` starts (project time; the clip's
    /// start without any).
    fn notes_at(model: &Session, notes: &[NoteId]) -> MusicalTime {
        Self::clip(model).map_or(MusicalTime::ZERO, |(_, c, m)| {
            c.start
                + m.notes
                    .iter()
                    .filter(|n| notes.contains(&n.id))
                    .map(|n| n.start)
                    .min()
                    .unwrap_or(MusicalTime::ZERO)
        })
    }

    fn select(cx: &mut EventCx<'_, Action>, notes: Vec<NoteId>, mode: SelectMode) {
        cx.emit(Action::SelectNotes { notes, mode });
    }

    pub(crate) fn selection_ids(m: &MidiClip, model: &Session) -> Vec<NoteId> {
        Self::selected(m, model).iter().map(|n| n.id).collect()
    }

    fn audition(&self, model: &Session, clip: &Clip, key: u8, cx: &mut EventCx<'_, Action>) {
        if model.editor.piano.audition {
            cx.emit(Action::Audition {
                track: clip.track,
                key,
                velocity: model.editor.piano.velocity,
                channel: 0,
            });
        }
    }

    // --- events ----------------------------------------------------------------------

    pub(crate) fn handle_event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        self.sync_lane(model);
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
            } => self.secondary(pos, size, model, cx),
            ViewEvent::PointerDown { .. } => false,
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } if self.value_drag.is_some() => self.tools_drag(pos, model, cx),
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } => {
                self.hover = Some(pos);
                self.drag_to(pos, modifiers, size, model, cx);
                cx.redraw();
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                self.hover = Some(pos);
                cx.set_cursor(self.cursor_at(pos, size, model));
                cx.redraw();
                false
            }
            ViewEvent::PointerUp { .. } if self.value_drag.take().is_some() => {
                cx.redraw();
                true
            }
            ViewEvent::PointerUp { pos, modifiers, .. } => {
                self.release(pos, modifiers, size, model, cx);
                cx.redraw();
                true
            }
            ViewEvent::PointerLeave => {
                self.hover = None;
                cx.redraw();
                false
            }
            ViewEvent::Scroll { pos, dy, .. }
                if self.tools_open && self.layout(size).tools.contains(pos) =>
            {
                self.tools_scroll(self.layout(size).tools, pos, dy, model, cx)
            }
            ViewEvent::Scroll {
                pos,
                dx,
                dy,
                modifiers,
                precise,
            } => {
                self.scroll(pos, dx, dy, modifiers, precise, size, model);
                cx.redraw();
                true
            }
            ViewEvent::Key { key, modifiers } => self.key(key, modifiers, size, model, cx),
            ViewEvent::FocusLost => {
                if self.drag.take().is_some() {
                    cx.emit(Action::AuditionOff);
                }
                false
            }
        }
    }

    fn cursor_at(&self, pos: Point, size: Size, model: &Session) -> Cursor {
        let l = self.layout(size);
        let Some((_, _, m)) = Self::clip(model) else {
            return Cursor::Default;
        };
        if l.splitter.contains(pos) {
            return Cursor::ResizeVertical;
        }
        if l.ruler.contains(pos) && (pos.x - self.x_of(m.length)).abs() <= 6.0 {
            return Cursor::ResizeHorizontal;
        }
        if l.toolbar.contains(pos) || l.lane_header.contains(pos) {
            return Cursor::Pointer;
        }
        if !l.grid.contains(pos) {
            return Cursor::Default;
        }
        match (self.tool, self.note_at(m, pos)) {
            (Tool::Eraser | Tool::Mute | Tool::Knife, _) => Cursor::Crosshair,
            (_, Some((_, Some(_)))) => Cursor::ResizeHorizontal,
            (_, Some(_)) => Cursor::Grab,
            (Tool::Pencil, None) => Cursor::Crosshair,
            _ => Cursor::Default,
        }
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
        let l = self.layout(size);
        if l.toolbar.contains(pos) {
            if let Some((item, rect, ..)) = self
                .toolbar_items(l.toolbar, model)
                .into_iter()
                .find(|(_, r, ..)| r.contains(pos))
            {
                self.toolbar_press(item, rect, model, cx);
            }
            return true;
        }
        if l.tools.w > 0.0 && l.tools.contains(pos) {
            return self.tools_press(l.tools, pos, model, cx);
        }
        let Some((clip_id, clip, m)) = Self::clip(model) else {
            return false;
        };
        if l.splitter.contains(pos) {
            self.drag = Some(Drag::Splitter {
                origin_y: pos.y,
                origin_h: self.lane_h,
            });
            return true;
        }
        if l.ruler.contains(pos) {
            if (pos.x - self.x_of(m.length)).abs() <= 6.0 {
                self.drag = Some(Drag::ClipEnd { length: m.length });
            } else {
                cx.emit(Action::Transport(
                    faderframe_session::TransportAction::Locate(
                        clip.start + self.time_at(pos.x).max(MusicalTime::ZERO),
                    ),
                ));
                self.drag = Some(Drag::Scrub);
            }
            return true;
        }
        if l.keys.contains(pos) {
            let key = self.key_at(pos.y);
            if mods.toggle() || mods.shift {
                let notes = m
                    .notes
                    .iter()
                    .filter(|n| n.key == key)
                    .map(|n| n.id)
                    .collect();
                Self::select(
                    cx,
                    notes,
                    if mods.toggle() {
                        SelectMode::Toggle
                    } else {
                        SelectMode::Add
                    },
                );
            } else {
                self.audition(model, clip, key, cx);
                self.drag = Some(Drag::Keys { key });
            }
            return true;
        }
        if l.lane_header.contains(pos) {
            cx.request(self.lane_menu(Point::new(pos.x, pos.y), clip_id, m, model));
            return true;
        }
        if l.lane.contains(pos) {
            return self.lane_press(pos, mods, &l, clip_id, m, model, cx);
        }
        if !l.grid.contains(pos) {
            return false;
        }
        let at = self.time_at(pos.x).max(MusicalTime::ZERO);
        let hit = self.note_at(m, pos);
        match (self.tool, hit) {
            (Tool::Eraser, _) => {
                self.drag = Some(Drag::Sweep {
                    hit: hit.map(|(n, _)| vec![n.id]).unwrap_or_default(),
                });
            }
            (Tool::Mute, _) => {
                self.drag = Some(Drag::Sweep {
                    hit: hit.map(|(n, _)| vec![n.id]).unwrap_or_default(),
                });
            }
            (Tool::Knife, Some((n, _))) => {
                let cut = self.snap_rel(at, clip.start, model, mods.shift || !model.editor.snap);
                let notes = if mods.shift && model.selection.notes.contains(&n.id) {
                    Self::selection_ids(m, model)
                } else {
                    vec![n.id]
                };
                cx.emit(Action::SplitNotes {
                    clip: clip_id,
                    notes,
                    at: cut,
                });
            }
            (Tool::Knife, None) => {}
            (_, Some((n, edge))) => {
                if clicks >= 2 {
                    cx.emit(Action::RemoveNotes {
                        clip: clip_id,
                        notes: vec![n.id],
                    });
                    return true;
                }
                let already = model.selection.notes.contains(&n.id);
                let mut notes: Vec<NoteId> = if already {
                    Self::selection_ids(m, model)
                } else if mods.shift {
                    let mut v = Self::selection_ids(m, model);
                    v.push(n.id);
                    v
                } else {
                    vec![n.id]
                };
                if mods.toggle() {
                    Self::select(cx, vec![n.id], SelectMode::Toggle);
                    if already {
                        return true;
                    }
                    notes = {
                        let mut v = Self::selection_ids(m, model);
                        v.push(n.id);
                        v
                    };
                } else if mods.shift {
                    Self::select(cx, vec![n.id], SelectMode::Add);
                } else if !already {
                    Self::select(cx, vec![n.id], SelectMode::Replace);
                }
                match edge {
                    Some(end) => {
                        self.drag = Some(Drag::Resize {
                            grab: n,
                            notes,
                            start: !end,
                            delta: MusicalTime::ZERO,
                        });
                    }
                    None => {
                        self.audition(model, clip, n.key, cx);
                        self.drag = Some(Drag::Move {
                            origin: pos,
                            grab: n,
                            notes,
                            dt: MusicalTime::ZERO,
                            dk: 0,
                            moved: false,
                            copy: mods.alt,
                        });
                    }
                }
            }
            (Tool::Pencil, None) => {
                let start = self.snap_floor_rel(at, clip.start, model);
                if start >= m.length {
                    return true;
                }
                let key = self.scale_key(self.key_at(pos.y), clip.start + start, model);
                let length = self.new_note_length(model, clip.start + start);
                self.audition(model, clip, key, cx);
                self.drag = Some(Drag::Draw { start, key, length });
            }
            (Tool::Pointer, None) => {
                if clicks >= 2 {
                    let start = self.snap_floor_rel(at, clip.start, model);
                    if start < m.length {
                        let key = self.scale_key(self.key_at(pos.y), clip.start + start, model);
                        let length = self.new_note_length(model, clip.start + start);
                        cx.emit(Action::AddChord {
                            clip: clip_id,
                            start,
                            length,
                            key,
                            velocity: model.editor.piano.velocity,
                        });
                    }
                    return true;
                }
                self.drag = Some(Drag::Select {
                    from: pos,
                    to: pos,
                    add: mods.shift || mods.toggle(),
                });
            }
        }
        true
    }

    fn lane_value(area: Rect, y: f32, max: u16) -> u16 {
        (((area.bottom() - y) / area.h).clamp(0.0, 1.0) * max as f32).round() as u16
    }

    #[allow(clippy::too_many_arguments)]
    fn lane_press(
        &mut self,
        pos: Point,
        mods: Modifiers,
        l: &crate::Layout,
        _clip: faderframe_core::ClipId,
        m: &MidiClip,
        model: &Session,
        _cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let area = Self::lane_value_rect(l.lane);
        match self.lane {
            LaneKind::Velocity => {
                let stem = m
                    .notes
                    .iter()
                    .rev()
                    .find(|n| (pos.x - self.x_of(n.start)).abs() <= 4.0);
                match stem {
                    Some(n) if !mods.shift => {
                        self.drag = Some(Drag::Velocity {
                            note: *n,
                            origin_y: pos.y,
                            value: n.velocity,
                        });
                    }
                    _ => {
                        self.drag = Some(Drag::VelocityLine { from: pos, to: pos });
                    }
                }
            }
            LaneKind::Controller(c) => {
                let t = self.time_at(pos.x).max(MusicalTime::ZERO);
                let v = Self::lane_value(area, pos.y, c.max());
                self.drag = Some(Drag::Controller {
                    points: vec![(t, v)],
                    from: pos,
                    to: pos,
                    line: mods.shift,
                    erase: mods.alt,
                });
            }
            LaneKind::Expression(k) => {
                // The selected notes, else the notes sounding here.
                let t = self.time_at(pos.x).max(MusicalTime::ZERO);
                let selected: Vec<&MidiNote> = m
                    .notes
                    .iter()
                    .filter(|n| model.selection.notes.contains(&n.id))
                    .collect();
                let notes: Vec<(NoteId, MusicalTime, MusicalTime)> = if selected.is_empty() {
                    m.notes
                        .iter()
                        .filter(|n| n.start <= t && n.end() > t)
                        .map(|n| (n.id, n.start, n.end()))
                        .collect()
                } else {
                    selected.iter().map(|n| (n.id, n.start, n.end())).collect()
                };
                if !notes.is_empty() {
                    let v = crate::expression_value(area, k, pos.y);
                    self.drag = Some(Drag::Expression {
                        notes,
                        points: vec![(t, v)],
                        from: pos,
                        to: pos,
                        line: mods.shift,
                        erase: mods.alt,
                    });
                }
            }
        }
        true
    }

    fn drag_to(
        &mut self,
        pos: Point,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some((_, clip, m)) = Self::clip(model) else {
            return;
        };
        let l = self.layout(size);
        // Scroll while dragging past the note area's edges.
        if matches!(
            self.drag,
            Some(Drag::Move { .. } | Drag::Resize { .. } | Drag::Draw { .. } | Drag::Select { .. })
        ) {
            if pos.x > l.grid.right() - 12.0 {
                self.scroll_x += 8.0;
            } else if pos.x < l.grid.x + 12.0 {
                self.scroll_x = (self.scroll_x - 8.0).max(0.0);
            }
            if pos.y > l.grid.bottom() - 10.0 {
                self.scroll_y += 6.0;
            } else if pos.y < l.grid.y + 10.0 {
                self.scroll_y = (self.scroll_y - 6.0).max(0.0);
            }
        }
        let bypass = mods.shift || !model.editor.snap;
        let mut drag = self.drag.take();
        match &mut drag {
            Some(Drag::Move {
                origin,
                grab,
                dt,
                dk,
                moved,
                copy,
                ..
            }) => {
                if !*moved && pos.distance(*origin) < DRAG_THRESHOLD {
                    self.drag = drag;
                    return;
                }
                *moved = true;
                *copy |= mods.alt;
                let raw = grab.start + (self.time_at(pos.x) - self.time_at(origin.x));
                let start = self.snap_rel(raw.max(MusicalTime::ZERO), clip.start, model, bypass);
                let key = self.scale_key(self.key_at(pos.y), clip.start + start, model);
                let new_dk = key as i32 - grab.key as i32;
                if new_dk != *dk {
                    self.audition(model, clip, key, cx);
                }
                *dt = start - grab.start;
                *dk = new_dk;
            }
            Some(Drag::Resize {
                grab, start, delta, ..
            }) => {
                let t = self.snap_rel(
                    self.time_at(pos.x).max(MusicalTime::ZERO),
                    clip.start,
                    model,
                    bypass,
                );
                *delta = if *start {
                    t - grab.start
                } else {
                    t - grab.end()
                };
            }
            Some(Drag::Draw { start, length, .. }) => {
                let step = self.new_note_length(model, clip.start + *start);
                let end = self.snap_rel(self.time_at(pos.x), clip.start, model, bypass);
                let min = if bypass {
                    MusicalTime(1000)
                } else {
                    step.min(self.step_at(model, clip.start + *start))
                };
                *length = (end - *start)
                    .max(min)
                    .min(m.length - *start)
                    .max(MusicalTime(1));
            }
            Some(Drag::Select { to, .. }) => *to = pos,
            Some(Drag::Sweep { hit }) => {
                if let Some((n, _)) = self.note_at(m, pos)
                    && !hit.contains(&n.id)
                {
                    hit.push(n.id);
                }
            }
            Some(Drag::Velocity {
                note,
                origin_y,
                value,
            }) => {
                let area = Self::lane_value_rect(l.lane);
                let delta = (*origin_y - pos.y) / area.h.max(1.0) * 127.0;
                *value = (note.velocity as f32 + delta).round().clamp(1.0, 127.0) as u8;
            }
            Some(Drag::VelocityLine { to, .. }) => *to = pos,
            Some(Drag::Controller {
                points,
                to,
                line,
                erase,
                ..
            }) => {
                *to = pos;
                if !*line && !*erase {
                    let LaneKind::Controller(c) = self.lane else {
                        self.drag = drag;
                        return;
                    };
                    let area = Self::lane_value_rect(l.lane);
                    let t = self.time_at(pos.x).max(MusicalTime::ZERO);
                    let v = Self::lane_value(area, pos.y, c.max());
                    let far = points
                        .last()
                        .is_none_or(|(lt, _)| (self.x_of(t) - self.x_of(*lt)).abs() >= 3.0);
                    if far {
                        points.push((t, v));
                    }
                }
            }
            Some(Drag::Expression {
                points,
                to,
                line,
                erase,
                ..
            }) => {
                *to = pos;
                if !*line && !*erase {
                    let LaneKind::Expression(k) = self.lane else {
                        self.drag = drag;
                        return;
                    };
                    let area = Self::lane_value_rect(l.lane);
                    let t = self.time_at(pos.x).max(MusicalTime::ZERO);
                    let v = crate::expression_value(area, k, pos.y);
                    let far = points
                        .last()
                        .is_none_or(|(lt, _)| (self.x_of(t) - self.x_of(*lt)).abs() >= 3.0);
                    if far {
                        points.push((t, v));
                    }
                }
            }
            Some(Drag::Splitter { origin_y, origin_h }) => {
                self.lane_h = (*origin_h + (*origin_y - pos.y)).clamp(36.0, size.h * 0.6);
            }
            Some(Drag::ClipEnd { length }) => {
                let t = self.snap_rel(self.time_at(pos.x), clip.start, model, bypass);
                *length = t.max(self.step_at(model, clip.start));
            }
            Some(Drag::Scrub) => {
                cx.emit(Action::Transport(
                    faderframe_session::TransportAction::Locate(
                        clip.start + self.time_at(pos.x).max(MusicalTime::ZERO),
                    ),
                ));
            }
            Some(Drag::Keys { key }) => {
                let k = self.key_at(pos.y);
                if k != *key {
                    *key = k;
                    self.audition(model, clip, k, cx);
                }
            }
            None => {}
        }
        self.drag = drag;
    }

    fn release(
        &mut self,
        _pos: Point,
        _mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let Some((clip_id, clip, m)) = Self::clip(model) else {
            return;
        };
        let l = self.layout(size);
        match drag {
            Drag::Move {
                grab,
                notes,
                dt,
                dk,
                moved,
                copy,
                ..
            } => {
                cx.emit(Action::AuditionOff);
                if !moved {
                    // A plain click on one of several selected notes selects
                    // just that one.
                    if notes.len() > 1 && model.selection.notes.contains(&grab.id) {
                        Self::select(cx, vec![grab.id], SelectMode::Replace);
                    }
                } else if copy {
                    cx.emit(Action::DuplicateNotes {
                        clip: clip_id,
                        notes,
                        offset: Some(dt),
                        keys: dk,
                    });
                } else if dt != MusicalTime::ZERO || dk != 0 {
                    cx.emit(Action::NoteOperation {
                        clip: clip_id,
                        notes,
                        op: NoteOp::Move { by: dt, keys: dk },
                    });
                }
            }
            Drag::Resize {
                grab,
                notes,
                start,
                delta,
            } => {
                if delta != MusicalTime::ZERO {
                    let op = if start {
                        NoteOp::Resize {
                            start: delta,
                            end: MusicalTime::ZERO,
                        }
                    } else {
                        NoteOp::Resize {
                            start: MusicalTime::ZERO,
                            end: delta,
                        }
                    };
                    self.last_length = if start {
                        grab.length - delta
                    } else {
                        grab.length + delta
                    }
                    .max(MusicalTime(1));
                    cx.emit(Action::NoteOperation {
                        clip: clip_id,
                        notes,
                        op,
                    });
                }
            }
            Drag::Draw { start, key, length } => {
                cx.emit(Action::AuditionOff);
                self.last_length = length;
                cx.emit(Action::AddChord {
                    clip: clip_id,
                    start,
                    length,
                    key,
                    velocity: model.editor.piano.velocity,
                });
            }
            Drag::Select { from, to, add } => {
                let band = Rect::from_points(from, to);
                if band.w < DRAG_THRESHOLD && band.h < DRAG_THRESHOLD {
                    if !add {
                        Self::select(cx, Vec::new(), SelectMode::Replace);
                    }
                } else {
                    let notes = m
                        .notes
                        .iter()
                        .filter(|n| self.note_rect(n).is_some_and(|r| r.intersects(&band)))
                        .map(|n| n.id)
                        .collect();
                    Self::select(
                        cx,
                        notes,
                        if add {
                            SelectMode::Add
                        } else {
                            SelectMode::Replace
                        },
                    );
                }
            }
            Drag::Sweep { hit } => {
                if !hit.is_empty() {
                    match self.tool {
                        Tool::Mute => cx.emit(Action::NoteOperation {
                            clip: clip_id,
                            notes: hit,
                            op: NoteOp::ToggleMuted,
                        }),
                        _ => cx.emit(Action::RemoveNotes {
                            clip: clip_id,
                            notes: hit,
                        }),
                    }
                }
            }
            Drag::Velocity { note, value, .. } => {
                if value != note.velocity {
                    let sel = Self::selection_ids(m, model);
                    if sel.len() > 1 && sel.contains(&note.id) {
                        cx.emit(Action::NoteOperation {
                            clip: clip_id,
                            notes: sel,
                            op: NoteOp::ScaleVelocity {
                                factor: 1.0,
                                offset: value as i32 - note.velocity as i32,
                            },
                        });
                    } else {
                        cx.emit(Action::NoteOperation {
                            clip: clip_id,
                            notes: vec![note.id],
                            op: NoteOp::SetVelocity(value),
                        });
                    }
                }
            }
            Drag::VelocityLine { from, to } => {
                let area = Self::lane_value_rect(l.lane);
                let (a, b) = if from.x <= to.x {
                    (from, to)
                } else {
                    (to, from)
                };
                let sel = Self::selection_ids(m, model);
                let mut hits: Vec<&MidiNote> = m
                    .notes
                    .iter()
                    .filter(|n| {
                        let x = self.x_of(n.start);
                        x >= a.x - 2.0 && x <= b.x + 2.0 && (sel.is_empty() || sel.contains(&n.id))
                    })
                    .collect();
                hits.sort_by_key(|n| n.start);
                if let (Some(first), Some(last)) = (hits.first(), hits.last()) {
                    let at = |x: f32| {
                        let t = if (b.x - a.x).abs() < 1.0 {
                            0.0
                        } else {
                            ((x - a.x) / (b.x - a.x)).clamp(0.0, 1.0)
                        };
                        let y = a.y + (b.y - a.y) * t;
                        Self::lane_value(area, y, 127).clamp(1, 127) as u8
                    };
                    let (v0, v1) = (at(self.x_of(first.start)), at(self.x_of(last.start)));
                    cx.emit(Action::NoteOperation {
                        clip: clip_id,
                        notes: hits.iter().map(|n| n.id).collect(),
                        op: NoteOp::RampVelocity { from: v0, to: v1 },
                    });
                }
            }
            Drag::Controller {
                points,
                from,
                to,
                line,
                erase,
            } => {
                let LaneKind::Controller(c) = self.lane else {
                    return;
                };
                let area = Self::lane_value_rect(l.lane);
                let ta = self.time_at(from.x.min(to.x)).max(MusicalTime::ZERO);
                let tb = self.time_at(from.x.max(to.x)).max(MusicalTime::ZERO);
                let (range_from, range_to, pts) = if erase {
                    (ta, tb, Vec::new())
                } else if line {
                    // Straight line: a point every 1/64 note.
                    let step = MusicalTime::from_quarters(1.0 / 16.0);
                    let (va, vb) = if from.x <= to.x {
                        (
                            Self::lane_value(area, from.y, c.max()),
                            Self::lane_value(area, to.y, c.max()),
                        )
                    } else {
                        (
                            Self::lane_value(area, to.y, c.max()),
                            Self::lane_value(area, from.y, c.max()),
                        )
                    };
                    let n = ((tb - ta).ticks() / step.ticks().max(1)).max(1);
                    let pts = (0..=n)
                        .map(|i| ControllerPoint {
                            time: ta + MusicalTime(step.ticks() * i).min(tb - ta),
                            value: (va as f64 + (vb as f64 - va as f64) * i as f64 / n as f64)
                                .round() as u16,
                        })
                        .collect();
                    (ta, tb + MusicalTime(1), pts)
                } else {
                    let mut pts: Vec<ControllerPoint> = points
                        .iter()
                        .map(|&(time, value)| ControllerPoint { time, value })
                        .collect();
                    pts.sort_by_key(|p| p.time);
                    let a = pts.first().map_or(ta, |p| p.time);
                    let b = pts.last().map_or(tb, |p| p.time);
                    (a, b + MusicalTime(1), pts)
                };
                cx.emit(Action::SetControllerPoints {
                    clip: clip_id,
                    controller: c,
                    channel: self.lane_channel,
                    from: range_from,
                    to: range_to,
                    points: pts,
                });
            }
            Drag::Expression {
                notes,
                points,
                from,
                to,
                line,
                erase,
            } => {
                let LaneKind::Expression(k) = self.lane else {
                    return;
                };
                let area = Self::lane_value_rect(l.lane);
                let ta = self.time_at(from.x.min(to.x)).max(MusicalTime::ZERO);
                let tb = self.time_at(from.x.max(to.x)).max(MusicalTime::ZERO);
                // Clip-relative points of the gesture.
                let pts: Vec<(MusicalTime, f32)> = if line {
                    let (va, vb) = if from.x <= to.x {
                        (
                            crate::expression_value(area, k, from.y),
                            crate::expression_value(area, k, to.y),
                        )
                    } else {
                        (
                            crate::expression_value(area, k, to.y),
                            crate::expression_value(area, k, from.y),
                        )
                    };
                    vec![(ta, va), (tb, vb)]
                } else {
                    let mut p = points;
                    p.sort_by_key(|(t, _)| *t);
                    p
                };
                let (ga, gb) = if erase || line {
                    (ta, tb)
                } else {
                    (
                        pts.first().map_or(ta, |p| p.0),
                        pts.last().map_or(tb, |p| p.0),
                    )
                };
                cx.emit(Action::BeginGesture("Edit Expression".into()));
                for (id, start, end) in notes {
                    let a = ga.max(start);
                    let b = gb.min(end);
                    if b < a {
                        continue;
                    }
                    let rel: Vec<faderframe_project::ExpressionPoint> = if erase {
                        Vec::new()
                    } else {
                        pts.iter()
                            .filter(|(t, _)| *t >= a && *t <= b)
                            .map(|&(t, value)| faderframe_project::ExpressionPoint {
                                time: t - start,
                                value,
                            })
                            .collect()
                    };
                    cx.emit(Action::SetNoteExpression {
                        clip: clip_id,
                        note: id,
                        kind: k,
                        from: a - start,
                        to: b - start + MusicalTime(1),
                        points: rel,
                    });
                }
                cx.emit(Action::EndGesture);
            }
            Drag::ClipEnd { length } => {
                if length != m.length {
                    cx.emit(Action::SetMidiClipLength {
                        clip: clip_id,
                        length,
                    });
                }
            }
            Drag::Keys { .. } => cx.emit(Action::AuditionOff),
            Drag::Splitter { .. } | Drag::Scrub => {}
        }
        let _ = clip;
        cx.set_cursor(Cursor::Default);
    }

    // --- menus -------------------------------------------------------------------------

    fn lane_menu(
        &self,
        at: Point,
        clip: faderframe_core::ClipId,
        m: &MidiClip,
        model: &Session,
    ) -> HostRequest<Action> {
        let pr = model.editor.piano;
        let set = |lane: Option<(MidiController, u8)>| {
            Action::SetPianoRoll(PianoRollSettings {
                lane,
                expression: None,
                ..pr
            })
        };
        let mut items = vec![
            MenuItem::new("Velocity", set(None))
                .checked(pr.lane.is_none() && pr.expression.is_none()),
        ];
        // Per-note expression: native for plugins, pitch/pressure/timbre
        // also over MPE.
        let mpe = model
            .project()
            .clip(clip)
            .and_then(|c| model.project().track(c.track))
            .is_some_and(|t| t.mpe.is_some());
        for (i, k) in faderframe_project::ExpressionKind::ALL
            .into_iter()
            .enumerate()
        {
            let label = if mpe && !faderframe_project::ExpressionKind::MPE.contains(&k) {
                format!("Note {} (not over MPE)", k.label())
            } else {
                format!("Note {}", k.label())
            };
            let item = MenuItem::new(
                label,
                Action::SetPianoRoll(PianoRollSettings {
                    expression: Some(k),
                    ..pr
                }),
            )
            .checked(pr.expression == Some(k));
            items.push(if i == 0 { item.separated() } else { item });
        }
        if let Some(track) = model.project().clip(clip).map(|c| c.track)
            && let Some(t) = model.project().track(track)
        {
            let on = t.mpe.is_some();
            items.push(
                MenuItem::new(
                    "MPE for this track",
                    Action::Edit(faderframe_project::Command::SetTrackMpe {
                        track,
                        mpe: (!on).then(faderframe_project::MpeConfig::default),
                    }),
                )
                .checked(on),
            );
        }
        let mut listed: Vec<MidiController> = LANES.to_vec();
        for l in &m.controllers {
            if !listed.contains(&l.controller) {
                listed.push(l.controller);
            }
        }
        for (i, c) in listed.into_iter().enumerate() {
            let used = m.controllers.iter().any(|l| l.controller == c);
            let label = if used {
                format!("{} ●", c.label())
            } else {
                c.label()
            };
            let item = MenuItem::new(label, set(Some((c, self.lane_channel))))
                .checked(pr.expression.is_none() && pr.lane.is_some_and(|(x, _)| x == c));
            items.push(if i == 0 { item.separated() } else { item });
        }
        if let Some((c, ch)) = pr.lane {
            for i in 0..16u8 {
                let item =
                    MenuItem::new(format!("Channel {}", i + 1), set(Some((c, i)))).checked(ch == i);
                items.push(if i == 0 { item.separated() } else { item });
            }
            items.push(
                MenuItem::new(
                    format!("Clear {}", c.label()),
                    Action::SetControllerPoints {
                        clip,
                        controller: c,
                        channel: ch,
                        from: MusicalTime::ZERO,
                        to: MusicalTime(i64::MAX / 4),
                        points: Vec::new(),
                    },
                )
                .separated(),
            );
        }
        HostRequest::ContextMenu { at, items }
    }

    fn note_menu(
        &self,
        at: Point,
        clip: faderframe_core::ClipId,
        notes: Vec<NoteId>,
        model: &Session,
    ) -> HostRequest<Action> {
        let op = |label: &str, op: NoteOp| {
            MenuItem::new(
                label,
                Action::NoteOperation {
                    clip,
                    notes: notes.clone(),
                    op,
                },
            )
        };
        let q = model.editor.quantize_settings();
        let start = model
            .project()
            .clip(clip)
            .map_or(MusicalTime::ZERO, |c| c.start);
        let mut items = vec![
            op("Quantize (Q)", NoteOp::Quantize(q)),
            op(
                "Quantize Ends",
                NoteOp::Quantize(QuantizeSettings {
                    starts: false,
                    ends: true,
                    ..q
                }),
            ),
            op("Humanize", model.humanize_op(start)),
            op("Legato (Ctrl+L)", NoteOp::Legato).separated(),
            op("Remove Overlaps", NoteOp::RemoveOverlaps),
            op("Transpose +1 Octave", NoteOp::Transpose(12)).separated(),
            op("Transpose −1 Octave", NoteOp::Transpose(-12)),
            op("Reverse", NoteOp::Reverse),
            op("Invert", NoteOp::Invert),
        ];
        let scale = model.piano_scale_at(Self::notes_at(model, &notes));
        if !scale.is_chromatic() {
            items.push(op(
                &format!("Fold into {}", scale.label()),
                NoteOp::FoldToScale(scale),
            ));
        }
        items.push(op("Double Length", NoteOp::ScaleLength(2.0)).separated());
        items.push(op("Half Length", NoteOp::ScaleLength(0.5)));
        items.push(op("Mute / Unmute (M)", NoteOp::ToggleMuted).separated());
        for (i, v) in [40u8, 64, 90, 110, 127].into_iter().enumerate() {
            let item = op(&format!("Velocity {v}"), NoteOp::SetVelocity(v));
            items.push(if i == 0 { item.separated() } else { item });
        }
        items.push(
            MenuItem::new(
                "Duplicate (Ctrl+D)",
                Action::DuplicateNotes {
                    clip,
                    notes: notes.clone(),
                    offset: None,
                    keys: 0,
                },
            )
            .separated(),
        );
        items.push(MenuItem::new(
            "Copy (Ctrl+C)",
            Action::CopyNotes {
                clip,
                notes: notes.clone(),
            },
        ));
        items.push(MenuItem::new(
            "Cut (Ctrl+X)",
            Action::CutNotes {
                clip,
                notes: notes.clone(),
            },
        ));
        items.push(MenuItem::new("Delete", Action::RemoveNotes { clip, notes }).separated());
        HostRequest::ContextMenu { at, items }
    }

    fn secondary(
        &mut self,
        pos: Point,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some((clip, c, m)) = Self::clip(model) else {
            return false;
        };
        let l = self.layout(size);
        if l.lane.contains(pos) || l.lane_header.contains(pos) {
            cx.request(self.lane_menu(pos, clip, m, model));
            return true;
        }
        if l.ruler.contains(pos) {
            // SysEx here: delete it; anywhere: import a file.
            let at = self.time_at(pos.x).max(MusicalTime::ZERO);
            let mut items = Vec::new();
            if let Some(i) = self.sysex_at(m, pos.x) {
                items.push(MenuItem::new(
                    format!("Delete SysEx {}", m.sysex[i].describe()),
                    Action::RemoveSysex { clip, index: i },
                ));
            }
            items.push(MenuItem::new(
                "Import SysEx File Here…",
                Action::RequestSysexImport { clip, at },
            ));
            cx.request(HostRequest::ContextMenu { at: pos, items });
            return true;
        }
        if !l.grid.contains(pos) {
            return false;
        }
        match self.note_at(m, pos) {
            Some((n, _)) => {
                let notes = if model.selection.notes.contains(&n.id) {
                    Self::selection_ids(m, model)
                } else {
                    Self::select(cx, vec![n.id], SelectMode::Replace);
                    vec![n.id]
                };
                cx.request(self.note_menu(pos, clip, notes, model));
            }
            None => {
                let at =
                    self.snap_floor_rel(self.time_at(pos.x).max(MusicalTime::ZERO), c.start, model);
                let q = model.editor.quantize_settings();
                let mut items = vec![];
                if !model.note_clipboard().is_empty() {
                    items.push(MenuItem::new("Paste Here", Action::PasteNotes { clip, at }));
                }
                items.push(MenuItem::new(
                    "Select All",
                    Action::SelectNotes {
                        notes: m.notes.iter().map(|n| n.id).collect(),
                        mode: SelectMode::Replace,
                    },
                ));
                items.push(MenuItem::new(
                    "Import SysEx File Here…",
                    Action::RequestSysexImport { clip, at },
                ));
                items.push(
                    MenuItem::new(
                        "Quantize All",
                        Action::NoteOperation {
                            clip,
                            notes: vec![],
                            op: NoteOp::Quantize(q),
                        },
                    )
                    .separated(),
                );
                items.push(MenuItem::new(
                    "Legato All",
                    Action::NoteOperation {
                        clip,
                        notes: vec![],
                        op: NoteOp::Legato,
                    },
                ));
                let step = self.new_note_length(model, c.start + at);
                items.push(
                    MenuItem::new(
                        "Step Input from Here",
                        Action::SetStepInput(Some(StepInput {
                            clip,
                            cursor: at,
                            step,
                        })),
                    )
                    .separated(),
                );
                cx.request(HostRequest::ContextMenu { at: pos, items });
            }
        }
        true
    }

    // --- keys, zoom, scroll ------------------------------------------------------------

    fn key(
        &mut self,
        key: Key,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some((clip, c, m)) = Self::clip(model) else {
            return false;
        };
        let sel = Self::selection_ids(m, model);
        let step = self.step_at(model, c.start);
        let pr = model.editor.piano;
        let op = |cx: &mut EventCx<'_, Action>, op: NoteOp| {
            if !sel.is_empty() {
                cx.emit(Action::NoteOperation {
                    clip,
                    notes: sel.clone(),
                    op,
                });
            }
        };
        match key {
            // The MIDI Tools panel's Apply.
            Key::Enter if pr.tool.is_some() => self.apply_tool(model, cx),
            Key::Escape => {
                if self.drag.take().is_some() {
                    cx.emit(Action::AuditionOff);
                } else if model.step_input().is_some() {
                    cx.emit(Action::SetStepInput(None));
                } else {
                    Self::select(cx, Vec::new(), SelectMode::Replace);
                }
            }
            Key::Delete | Key::Backspace => {
                if !sel.is_empty() {
                    cx.emit(Action::RemoveNotes {
                        clip,
                        notes: sel.clone(),
                    });
                }
            }
            Key::Char(ch) if mods.ctrl => match ch.to_ascii_lowercase() {
                'a' => Self::select(
                    cx,
                    m.notes.iter().map(|n| n.id).collect(),
                    SelectMode::Replace,
                ),
                'c' => cx.emit(Action::CopyNotes {
                    clip,
                    notes: sel.clone(),
                }),
                'x' => cx.emit(Action::CutNotes {
                    clip,
                    notes: sel.clone(),
                }),
                'v' => {
                    let ph = model.playhead();
                    let at = if ph >= c.start && ph < c.start + m.length {
                        self.snap_floor_rel(ph - c.start, c.start, model)
                    } else {
                        MusicalTime::ZERO
                    };
                    cx.emit(Action::PasteNotes { clip, at });
                }
                'd' => {
                    if !sel.is_empty() {
                        cx.emit(Action::DuplicateNotes {
                            clip,
                            notes: sel.clone(),
                            offset: None,
                            keys: 0,
                        });
                    }
                }
                'l' => op(cx, NoteOp::Legato),
                _ => return false,
            },
            Key::Char(ch) if !mods.ctrl && !mods.alt => match ch.to_ascii_lowercase() {
                'q' => cx.emit(Action::NoteOperation {
                    clip,
                    notes: sel.clone(),
                    op: NoteOp::Quantize(model.editor.quantize_settings()),
                }),
                'm' => op(cx, NoteOp::ToggleMuted),
                'f' => {
                    self.auto_centre = true;
                    self.centre_on(size, m);
                }
                '+' | '=' => self.zoom_x(1.25, self.layout(size).grid.center().x),
                '-' => self.zoom_x(0.8, self.layout(size).grid.center().x),
                d @ '1'..='5' => {
                    self.tool = Tool::ALL[(d as u8 - b'1') as usize];
                }
                _ => return false,
            },
            Key::Up | Key::Down => {
                let dir = if key == Key::Up { 1 } else { -1 };
                let scale = model.piano_scale_at(Self::notes_at(model, &sel));
                if mods.shift {
                    op(cx, NoteOp::Transpose(12 * dir));
                } else if pr.scale_snap && !scale.is_chromatic() {
                    op(cx, NoteOp::TransposeInScale(dir, scale));
                } else {
                    op(cx, NoteOp::Transpose(dir));
                }
            }
            Key::Left | Key::Right => {
                let dir = if key == Key::Right { 1 } else { -1 };
                let by = if mods.shift {
                    MusicalTime(step.ticks() / 4)
                } else {
                    step
                };
                if let Some(st) = model.step_input()
                    && sel.is_empty()
                {
                    let cursor = MusicalTime((st.cursor.ticks() + dir * st.step.ticks()).max(0));
                    cx.emit(Action::SetStepInput(Some(StepInput { cursor, ..st })));
                } else if mods.ctrl {
                    op(
                        cx,
                        NoteOp::Resize {
                            start: MusicalTime::ZERO,
                            end: MusicalTime(by.ticks() * dir),
                        },
                    );
                } else {
                    op(
                        cx,
                        NoteOp::Move {
                            by: MusicalTime(by.ticks() * dir),
                            keys: 0,
                        },
                    );
                }
            }
            _ => return false,
        }
        cx.redraw();
        true
    }

    fn zoom_x(&mut self, factor: f32, anchor_x: f32) {
        let t = self.time_at(anchor_x);
        self.ppq = (self.ppq * factor).clamp(8.0, 1200.0);
        self.scroll_x = (t.quarters() as f32 * self.ppq - (anchor_x - self.kb_w())).max(0.0);
        self.auto_centre = false;
    }

    #[allow(clippy::too_many_arguments)]
    fn scroll(
        &mut self,
        pos: Point,
        dx: f32,
        dy: f32,
        mods: Modifiers,
        precise: bool,
        size: Size,
        model: &Session,
    ) {
        let len = Self::clip(model).map_or(MusicalTime::QUARTER, |(_, _, m)| m.length);
        self.auto_centre = false;
        let steps = if precise { dy / 30.0 } else { dy };
        if mods.ctrl && mods.shift {
            // Row height around the pointer.
            let key = self.key_at(pos.y);
            let row = self.row_of(key).unwrap_or(0) as f32;
            let offset = pos.y - self.grid_top();
            self.row_h = (self.row_h * 1.15f32.powf(-steps)).clamp(6.0, 36.0);
            self.scroll_y = row * self.row_h - offset + self.row_h / 2.0;
        } else if mods.ctrl {
            self.zoom_x(1.18f32.powf(-steps), pos.x);
        } else {
            let (mut hx, mut vy) = (dx, dy);
            if mods.shift && dx == 0.0 {
                hx = dy;
                vy = 0.0;
            }
            let unit = if precise { 1.0 } else { self.row_h * 3.0 };
            self.scroll_x += hx * unit;
            self.scroll_y += vy * unit;
        }
        self.clamp_scroll(size, len);
    }

    /// The SysEx message drawn at `x` in the ruler.
    fn sysex_at(&self, m: &MidiClip, x: f32) -> Option<usize> {
        m.sysex
            .iter()
            .position(|e| (self.x_of(e.time) - x).abs() <= 5.0)
    }

    pub(crate) fn tooltip_at(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let l = self.layout(size);
        if l.toolbar.contains(pos) {
            return self
                .toolbar_item_at(l.toolbar, pos, model)
                .map(|i| self.toolbar_tooltip(i));
        }
        let (_, clip, m) = Self::clip(model)?;
        if l.ruler.contains(pos) && (pos.x - self.x_of(m.length)).abs() <= 6.0 {
            return Some("Clip end — drag to change the length".into());
        }
        if l.ruler.contains(pos)
            && let Some(i) = self.sysex_at(m, pos.x)
        {
            return Some(format!(
                "SysEx {} · sent to the track's MIDI output · right-click to delete",
                m.sysex[i].describe()
            ));
        }
        if l.lane_header.contains(pos) {
            return Some("Choose the lane: velocity, a controller or a per-note expression".into());
        }
        if l.lane.contains(pos) {
            return Some(match self.lane {
                LaneKind::Velocity => {
                    "Drag a stem · Shift-drag (or drag empty space) draws a velocity line".into()
                }
                LaneKind::Controller(c) => format!(
                    "{}: drag to draw · Shift-drag: line · Alt-drag: erase",
                    c.label()
                ),
                LaneKind::Expression(k) => format!(
                    "{} {} · per note: the selected notes (else those under the pointer) · drag to draw · Shift-drag: line · Alt-drag: erase",
                    k.label(),
                    k.format(crate::expression_value(
                        Self::lane_value_rect(l.lane),
                        k,
                        pos.y
                    ))
                ),
            });
        }
        if l.keys.contains(pos) {
            let key = self.key_at(pos.y);
            return Some(format!(
                "{} · click to listen · Shift/Ctrl-click selects the pitch",
                note_name(key)
            ));
        }
        if !l.grid.contains(pos) {
            return None;
        }
        match self.note_at(m, pos) {
            Some((n, _)) => Some(format!(
                "{} · velocity {} · {}{}",
                note_name(n.key),
                n.velocity,
                model.project().timeline.format_bbt(clip.start + n.start),
                if n.muted { " · muted" } else { "" }
            )),
            None => Some(match self.tool {
                Tool::Pointer => format!(
                    "{} · drag to select · double-click to add",
                    note_name(self.key_at(pos.y))
                ),
                Tool::Pencil => format!("{} · click/drag to draw", note_name(self.key_at(pos.y))),
                t => t.label().into(),
            }),
        }
    }
}
