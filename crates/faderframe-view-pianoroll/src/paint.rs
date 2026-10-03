//! Painting: ruler, keyboard, grid, ghost notes, notes (with drag
//! previews), the velocity/controller lane and the toolbar.

use crate::{Drag, LaneKind, Layout, PianoRollView, is_black, note_name, track_color};
use faderframe_project::{Clip, MidiClip, MidiController, MidiNote};
use faderframe_session::Session;
use faderframe_timeline::{GridDivision, GridLineKind, MusicalTime, for_each_grid_line};
use faderframe_ui_canvas::{
    Align, Color, FontFamily, Paint, Painter, Path, Point, Rect, Size, TextStyle, Theme,
};

impl PianoRollView {
    pub(crate) fn paint_view(
        &mut self,
        p: &mut dyn Painter,
        size: Size,
        model: &Session,
        theme: &Theme,
    ) {
        p.fill(Rect::from_size(size), theme.piano.background);
        let Some((id, clip, m)) = Self::clip(model) else {
            p.text(
                "Double-click a MIDI clip in the arranger to edit it — or double-click an instrument lane to create one",
                Rect::from_size(size),
                &TextStyle::new(theme.fonts.normal, theme.ui.text_faint).center(),
            );
            return;
        };
        self.update_rows(model, m);
        if self.centred_for != Some(id) {
            self.auto_centre = true;
        }
        if self.centred_for != Some(id) || (self.auto_centre && size != self.last_size) {
            self.centre_on(size, m);
            self.centred_for = Some(id);
        }
        self.last_size = size;
        self.follow(size, model, clip, m);
        self.clamp_scroll(size, m.length);
        let l = self.layout(size);
        let color = track_color(model, clip);

        p.push_clip(l.grid);
        self.paint_grid(p, l.grid, clip, m, model);
        if model.editor.piano.ghost_notes {
            self.paint_ghosts(p, l.grid, clip, model);
        }
        self.paint_notes(p, l.grid, m, color, model);
        self.paint_overlays(p, l.grid, clip, m, model);
        p.pop_clip();

        self.paint_keyboard(p, l.keys, clip, model);
        self.paint_ruler(p, l.ruler, clip, m, model);
        p.fill(l.corner, theme.arranger.ruler_bg);
        p.text(
            &clip.name,
            l.corner.inset_xy(6.0, 0.0),
            &TextStyle::new(theme.fonts.small, theme.ui.text).bold(),
        );
        self.paint_lane(p, &l, clip, m, color, model);
        p.fill(l.splitter, theme.ui.border);
        p.fill(
            Rect::new(l.splitter.center().x - 12.0, l.splitter.y + 2.0, 24.0, 1.0),
            theme.ui.text_faint,
        );
        self.paint_toolbar(p, l.toolbar, model);
    }

    /// Page along with the playhead while playing.
    fn follow(&mut self, size: Size, model: &Session, clip: &Clip, m: &MidiClip) {
        if !model.transport().playing || !model.editor.follow_playhead || self.drag.is_some() {
            return;
        }
        let ph = model.playhead();
        if ph < clip.start || ph > clip.start + m.length {
            return;
        }
        let g = self.layout(size).grid;
        let x = self.x_of(ph - clip.start);
        if x > g.right() - 20.0 || x < g.x {
            self.scroll_x += x - g.x - 20.0;
        }
    }

    /// Grid lines dense enough to read, never closer than ~7 px.
    fn grid_division(&self, model: &Session) -> GridDivision {
        let chosen = model.editor.grid;
        let step_px = |d: GridDivision| {
            d.step(faderframe_timeline::TimeSignature::FOUR_FOUR)
                .quarters() as f32
                * self.ppq
        };
        if step_px(chosen) >= 7.0 {
            return chosen;
        }
        [
            GridDivision::Note(16),
            GridDivision::Note(8),
            GridDivision::Beat,
            GridDivision::Bar,
        ]
        .into_iter()
        .find(|d| step_px(*d) >= 7.0)
        .unwrap_or(GridDivision::Bar)
    }

    fn paint_grid(&self, p: &mut dyn Painter, g: Rect, clip: &Clip, m: &MidiClip, model: &Session) {
        let pr = &self.theme.piano;
        let scale = model.editor.piano.scale;
        let top = (((g.y - self.grid_top() + self.scroll_y) / self.row_h)
            .floor()
            .max(0.0)) as usize;
        let bottom = (((g.bottom() - self.grid_top() + self.scroll_y) / self.row_h).ceil()
            as usize)
            .min(self.rows.len());
        for i in top..bottom {
            let key = self.rows[i];
            let y = self.grid_top() + i as f32 * self.row_h - self.scroll_y;
            let row = Rect::new(g.x, y, g.w, self.row_h);
            p.fill(
                row,
                if is_black(key) {
                    pr.black_row
                } else {
                    pr.white_row
                },
            );
            if !scale.is_chromatic() {
                if !scale.contains(key) {
                    p.fill(row, pr.off_scale);
                } else if scale.is_root(key) {
                    p.fill(row, pr.root_row);
                }
            }
            if key.is_multiple_of(12) {
                p.hline(g.x, g.right(), row.bottom(), pr.octave_line);
            }
        }
        // Project loop range.
        let project = model.project();
        if let Some(lr) = project.loop_range.filter(|_| project.loop_enabled) {
            let x0 = self.x_of(lr.start - clip.start).max(g.x);
            let x1 = self.x_of(lr.end - clip.start).min(g.right());
            if x1 > x0 {
                p.fill(
                    Rect::new(x0, g.y, x1 - x0, g.h),
                    self.theme.ui.accent.with_alpha(0.035),
                );
            }
        }
        let meter = &project.timeline.meter;
        let start = clip.start + self.time_at(g.x).max(MusicalTime::ZERO);
        let end = clip.start + self.time_at(g.right());
        for_each_grid_line(start, end, self.grid_division(model), meter, |t, kind| {
            let x = self.x_of(t - clip.start);
            let c = match kind {
                GridLineKind::Bar => pr.bar_line,
                GridLineKind::Beat => pr.beat_line,
                GridLineKind::Subdivision => pr.sub_line,
            };
            p.vline(x, g.y, g.bottom(), c);
        });
        // Past the clip end.
        let x_end = self.x_of(m.length);
        if x_end < g.right() {
            let r = Rect::new(x_end.max(g.x), g.y, g.right() - x_end.max(g.x), g.h);
            p.fill(r, Color::rgba(0.0, 0.0, 0.0, 0.38));
            p.vline(x_end, g.y, g.bottom(), self.theme.ui.accent.with_alpha(0.6));
        }
    }

    /// The track's other MIDI clips, translucent behind.
    fn paint_ghosts(&self, p: &mut dyn Painter, g: Rect, clip: &Clip, model: &Session) {
        let pr = &self.theme.piano;
        let (a, b) = (self.time_at(g.x), self.time_at(g.right()));
        for other in model.project().clips_of(clip.track) {
            if other.id == clip.id {
                continue;
            }
            let Some(om) = other.as_midi() else { continue };
            let offset = other.start - clip.start;
            if offset > b || offset + om.length < a {
                continue;
            }
            for n in &om.notes {
                let shifted = MidiNote {
                    start: n.start + offset,
                    ..*n
                };
                if let Some(r) = self.note_rect(&shifted)
                    && r.right() >= g.x
                    && r.x <= g.right()
                {
                    p.stroke_rounded(r, 2.0, 1.0, pr.ghost_note);
                }
            }
        }
    }

    fn note_color(base: Color, velocity: u8) -> Color {
        base.darken(0.3)
            .mix(base.lighten(0.35), velocity as f32 / 127.0)
    }

    /// Notes as they will be after the drag in progress.
    pub(crate) fn previewed(&self, m: &MidiClip) -> Vec<(MidiNote, bool)> {
        let mut out: Vec<(MidiNote, bool)> = m.notes.iter().map(|n| (*n, false)).collect();
        match &self.drag {
            Some(Drag::Move {
                notes,
                dt,
                dk,
                moved: true,
                copy,
                ..
            }) => {
                let shift = |n: &MidiNote| MidiNote {
                    start: (n.start + *dt).max(MusicalTime::ZERO),
                    key: (n.key as i32 + dk).clamp(0, 127) as u8,
                    ..*n
                };
                if *copy {
                    let copies: Vec<(MidiNote, bool)> = m
                        .notes
                        .iter()
                        .filter(|n| notes.contains(&n.id))
                        .map(|n| (shift(n), true))
                        .collect();
                    out.extend(copies);
                } else {
                    for (n, moved) in &mut out {
                        if notes.contains(&n.id) {
                            *n = shift(n);
                            *moved = true;
                        }
                    }
                }
            }
            Some(Drag::Resize {
                notes,
                start,
                delta,
                ..
            }) => {
                for (n, moved) in &mut out {
                    if notes.contains(&n.id) {
                        if *start {
                            let s = (n.start + *delta)
                                .max(MusicalTime::ZERO)
                                .min(n.end() - MusicalTime(1));
                            n.length = n.end() - s;
                            n.start = s;
                        } else {
                            n.length = (n.length + *delta).max(MusicalTime(1));
                        }
                        *moved = true;
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn paint_notes(
        &self,
        p: &mut dyn Painter,
        g: Rect,
        m: &MidiClip,
        color: Color,
        model: &Session,
    ) {
        let th = &self.theme;
        let sweep: &[faderframe_core::NoteId] = match &self.drag {
            Some(Drag::Sweep { hit }) => hit,
            _ => &[],
        };
        for (n, preview) in self.previewed(m) {
            let Some(r) = self.note_rect(&n) else {
                continue;
            };
            if r.right() < g.x || r.x > g.right() || r.bottom() < g.y || r.y > g.bottom() {
                continue;
            }
            let selected = model.selection.notes.contains(&n.id);
            let swept = sweep.contains(&n.id);
            let c = Self::note_color(color, n.velocity);
            if n.muted {
                p.fill_rounded(r, 2.0, &Paint::Solid(c.with_alpha(0.18)));
                p.stroke_rounded(r, 2.0, 1.0, c.with_alpha(0.7));
                p.hline(r.x + 2.0, r.right() - 2.0, r.center().y, c.with_alpha(0.7));
            } else {
                p.shadow(r, 2.0, Color::rgba(0.0, 0.0, 0.0, 0.4), 0.0, 1.0, 2.0);
                p.fill_rounded(r, 2.0, &Paint::vertical(r, c.lighten(0.12), c.darken(0.08)));
                // Velocity as a bar along the bottom.
                let vw = (r.w - 4.0).max(0.0) * n.velocity as f32 / 127.0;
                p.fill(
                    Rect::new(r.x + 2.0, r.bottom() - 2.5, vw, 1.5),
                    Color::rgba(1.0, 1.0, 1.0, 0.45),
                );
            }
            if swept {
                p.fill_rounded(r, 2.0, &Paint::Solid(Color::rgba(1.0, 0.3, 0.2, 0.45)));
            }
            if selected || preview {
                p.stroke_rounded(r, 2.0, 1.5, th.ui.selection.lighten(0.45));
            } else if !n.muted {
                p.stroke_rounded(r, 2.0, 1.0, c.darken(0.45));
            }
            if r.w > 28.0 && self.row_h >= 11.0 {
                let text_c = if n.muted || c.luminance() < 0.45 {
                    Color::hex(0xf0eee8)
                } else {
                    Color::hex(0x141414)
                };
                p.text(
                    &note_name(n.key),
                    r.inset_xy(3.0, 0.0),
                    &TextStyle::new(th.fonts.tiny, text_c).family(FontFamily::Condensed),
                );
            }
        }
        // A note (or chord) being drawn.
        if let Some(Drag::Draw { start, key, length }) = self.drag {
            let pr = &model.editor.piano;
            let root = if pr.scale_snap {
                pr.scale.nearest(key)
            } else {
                key
            };
            for k in pr.chord.keys(root, &pr.scale) {
                let n = MidiNote {
                    id: faderframe_core::NoteId(0),
                    start,
                    length,
                    key: k,
                    velocity: pr.velocity,
                    channel: 0,
                    muted: false,
                };
                if let Some(r) = self.note_rect(&n) {
                    let c = Self::note_color(color, pr.velocity);
                    p.fill_rounded(r, 2.0, &Paint::Solid(c.with_alpha(0.75)));
                    p.stroke_rounded(r, 2.0, 1.5, th.ui.selection.lighten(0.45));
                }
            }
        }
    }

    /// Rubber band, step cursor, playhead.
    fn paint_overlays(
        &self,
        p: &mut dyn Painter,
        g: Rect,
        clip: &Clip,
        m: &MidiClip,
        model: &Session,
    ) {
        let pr = &self.theme.piano;
        if let Some(Drag::Select { from, to, .. }) = self.drag {
            let r = Rect::from_points(from, to);
            p.fill(r, pr.rubber_band);
            p.stroke_rounded(r, 0.0, 1.0, pr.rubber_band.with_alpha(0.8));
        }
        if let Some(st) = model.step_input()
            && st.clip == clip.id
        {
            let x = self.x_of(st.cursor);
            let w = (self.x_of(st.cursor + st.step) - x).max(2.0);
            p.fill(Rect::new(x, g.y, w, g.h), pr.step_cursor.with_alpha(0.08));
            p.fill(Rect::new(x - 1.0, g.y, 2.0, g.h), pr.step_cursor);
        }
        let ph = model.playhead();
        if ph >= clip.start && ph <= clip.start + m.length {
            let x = self.x_of(ph - clip.start);
            p.fill(
                Rect::new(x - 0.75, g.y, 1.5, g.h),
                self.theme.arranger.playhead,
            );
        }
    }

    fn paint_keyboard(&self, p: &mut dyn Painter, rect: Rect, clip: &Clip, model: &Session) {
        let pr = &self.theme.piano;
        let th = &self.theme;
        p.fill(rect, pr.key_white_shade);
        p.push_clip(rect);
        let held = model.held_midi_keys();
        let live = model.midi_live_tracks().contains(&clip.track);
        let pressed = match self.drag {
            Some(Drag::Keys { key }) => Some(key),
            _ => None,
        };
        let hover = self
            .hover
            .filter(|h| rect.contains(*h))
            .map(|h| self.key_at(h.y));
        let scale = model.editor.piano.scale;
        let top = (((rect.y - self.grid_top() + self.scroll_y) / self.row_h)
            .floor()
            .max(0.0)) as usize;
        let bottom = (((rect.bottom() - self.grid_top() + self.scroll_y) / self.row_h).ceil()
            as usize)
            .min(self.rows.len());
        for i in top..bottom {
            let key = self.rows[i];
            let y = self.grid_top() + i as f32 * self.row_h - self.scroll_y;
            let row = Rect::new(rect.x, y, rect.w, self.row_h);
            let played = (live && held & (1u128 << key) != 0) || pressed == Some(key);
            let lit = played || hover == Some(key);
            let in_scale = scale.is_chromatic() || scale.contains(key);
            if is_black(key) {
                p.fill(row, pr.key_white);
                let black = Rect::new(rect.x, y + 1.0, rect.w * 0.62, self.row_h - 2.0);
                let c = if played {
                    th.ui.accent
                } else if lit {
                    th.ui.selection.darken(0.3)
                } else {
                    pr.key_black
                };
                p.fill_rounded(black, 1.5, &Paint::horizontal(black, c.lighten(0.15), c));
            } else {
                let c = if played {
                    th.ui.accent.lighten(0.3)
                } else if lit {
                    th.ui.selection.lighten(0.5)
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
            if !in_scale {
                p.fill(
                    Rect::new(rect.right() - 6.0, y, 6.0, self.row_h),
                    Color::rgba(0.0, 0.0, 0.0, 0.25),
                );
            } else if !scale.is_chromatic() && scale.is_root(key) {
                p.fill(
                    Rect::new(rect.right() - 6.0, y + 1.0, 4.0, self.row_h - 2.0),
                    th.ui.accent,
                );
            }
            let label = key.is_multiple_of(12) || self.row_h >= 15.0 || self.rows.len() < 60;
            if label {
                p.text(
                    &note_name(key),
                    Rect::new(rect.x, y, rect.w - 8.0, self.row_h),
                    &TextStyle::new(th.fonts.tiny, pr.key_text).right(),
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

    fn paint_ruler(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        clip: &Clip,
        m: &MidiClip,
        model: &Session,
    ) {
        let th = &self.theme;
        p.fill_rect(
            r,
            &Paint::vertical(r, th.arranger.ruler_bg.lighten(0.04), th.arranger.ruler_bg),
        );
        p.push_clip(r);
        let project = model.project();
        if let Some(lr) = project.loop_range {
            let x0 = self.x_of(lr.start - clip.start);
            let x1 = self.x_of(lr.end - clip.start);
            let c = if project.loop_enabled {
                th.ui.accent.with_alpha(0.55)
            } else {
                th.ui.text_faint.with_alpha(0.3)
            };
            p.fill(Rect::new(x0, r.y, (x1 - x0).max(0.0), 3.0), c);
        }
        let meter = &project.timeline.meter;
        let start = clip.start + self.time_at(r.x).max(MusicalTime::ZERO);
        let end = clip.start + self.time_at(r.right());
        let style = TextStyle::new(th.fonts.small, th.arranger.ruler_text).family(FontFamily::Mono);
        let beats = self.ppq >= 14.0;
        for_each_grid_line(start, end, GridDivision::Beat, meter, |t, kind| {
            let x = self.x_of(t - clip.start);
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
                _ if beats => p.vline(
                    x,
                    r.bottom() - 5.0,
                    r.bottom(),
                    th.arranger.ruler_text.with_alpha(0.25),
                ),
                _ => {}
            }
        });
        // Clip end handle (drag to change the clip length).
        let length = match self.drag {
            Some(Drag::ClipEnd { length }) => length,
            _ => m.length,
        };
        let xe = self.x_of(length);
        let mut tri = Path::new();
        tri.move_to(Point::new(xe, r.bottom()));
        tri.line_to(Point::new(xe - 7.0, r.bottom() - 9.0));
        tri.line_to(Point::new(xe, r.bottom() - 9.0));
        p.fill_path(&tri, th.ui.accent);
        p.vline(xe, r.y + 6.0, r.bottom(), th.ui.accent);
        // Step input cursor and playhead.
        if let Some(st) = model.step_input()
            && st.clip == clip.id
        {
            let x = self.x_of(st.cursor);
            p.fill(Rect::new(x - 1.0, r.y, 2.0, r.h), th.piano.step_cursor);
        }
        let ph = model.playhead();
        if ph >= clip.start {
            let x = self.x_of(ph - clip.start);
            let mut head = Path::new();
            head.move_to(Point::new(x - 5.0, r.y + 6.0));
            head.line_to(Point::new(x + 5.0, r.y + 6.0));
            head.line_to(Point::new(x, r.y + 13.0));
            p.fill_path(&head, th.arranger.playhead);
        }
        p.pop_clip();
        p.hline(r.x, r.right(), r.bottom() - 1.0, th.arranger.header_border);
    }

    /// Lane value area (inset from the edges).
    pub(crate) fn lane_value_rect(lane: Rect) -> Rect {
        Rect::new(lane.x, lane.y + 6.0, lane.w, (lane.h - 10.0).max(1.0))
    }

    fn paint_lane(
        &self,
        p: &mut dyn Painter,
        l: &Layout,
        clip: &Clip,
        m: &MidiClip,
        color: Color,
        model: &Session,
    ) {
        let th = &self.theme;
        let pr = &th.piano;
        let v = l.lane;
        p.fill(v, pr.velocity_bg);
        let area = Self::lane_value_rect(v);
        for frac in [0.25f32, 0.5, 0.75] {
            p.hline(
                v.x,
                v.right(),
                area.bottom() - area.h * frac,
                Color::rgba(1.0, 1.0, 1.0, 0.04),
            );
        }
        p.push_clip(v);
        match self.lane {
            LaneKind::Velocity => self.paint_velocity(p, area, m, color, model),
            LaneKind::Controller(c) => self.paint_controller(p, area, clip, m, c),
        }
        p.pop_clip();
        // Header: lane name and a hint that it is a menu.
        p.fill(l.lane_header, pr.velocity_bg.darken(0.2));
        let name = match self.lane {
            LaneKind::Controller(c) if self.lane_channel != 0 => {
                format!("{} ch{}", c.label(), self.lane_channel + 1)
            }
            k => k.label(),
        };
        faderframe_ui_canvas::controls::engraved(
            p,
            &format!("{} ▾", name.to_uppercase()),
            l.lane_header.inset_xy(6.0, 0.0),
            th,
            Align::Start,
        );
    }

    fn paint_velocity(
        &self,
        p: &mut dyn Painter,
        area: Rect,
        m: &MidiClip,
        color: Color,
        model: &Session,
    ) {
        let th = &self.theme;
        let value_of = |n: &MidiNote| match &self.drag {
            Some(Drag::Velocity { note, value, .. }) if note.id == n.id => *value,
            _ => n.velocity,
        };
        for (n, _) in self.previewed(m) {
            let x = self.x_of(n.start);
            if x < area.x - 4.0 || x > area.right() + 4.0 {
                continue;
            }
            let vel = value_of(&n);
            let h = area.h * vel as f32 / 127.0;
            let selected = model.selection.notes.contains(&n.id);
            let c = if selected {
                th.ui.selection
            } else if n.muted {
                Self::note_color(color, vel).with_alpha(0.4)
            } else {
                Self::note_color(color, vel)
            };
            p.fill(Rect::new(x - 1.0, area.bottom() - h, 3.0, h), c);
            p.circle(Point::new(x + 0.5, area.bottom() - h), 3.0, c.lighten(0.2));
        }
        if let Some(Drag::VelocityLine { from, to }) = self.drag {
            p.line(from, to, 2.0, th.piano.lane_curve);
        }
    }

    fn paint_controller(
        &self,
        p: &mut dyn Painter,
        area: Rect,
        _clip: &Clip,
        m: &MidiClip,
        c: MidiController,
    ) {
        let pr = &self.theme.piano;
        let max = c.max() as f32;
        let y_of = |v: u16| area.bottom() - area.h * v as f32 / max;
        if c == MidiController::PitchBend {
            p.hline(
                area.x,
                area.right(),
                y_of(8192),
                Color::rgba(1.0, 1.0, 1.0, 0.12),
            );
        }
        let lane = m.lane(c, self.lane_channel);
        let (a, b) = (self.time_at(area.x), self.time_at(area.right()));
        if let Some(lane) = lane {
            let mut path = Path::new();
            let mut fill = Path::new();
            let first = lane.value_at(a).unwrap_or(c.rest());
            let mut y = y_of(first);
            path.move_to(Point::new(area.x, y));
            fill.move_to(Point::new(area.x, area.bottom()));
            fill.line_to(Point::new(area.x, y));
            for pt in lane.points.iter().filter(|pt| pt.time > a && pt.time < b) {
                let x = self.x_of(pt.time);
                path.line_to(Point::new(x, y));
                fill.line_to(Point::new(x, y));
                y = y_of(pt.value);
                path.line_to(Point::new(x, y));
                fill.line_to(Point::new(x, y));
            }
            path.line_to(Point::new(area.right(), y));
            fill.line_to(Point::new(area.right(), y));
            fill.line_to(Point::new(area.right(), area.bottom()));
            p.fill_path(&fill, pr.lane_curve.with_alpha(0.16));
            p.stroke_path(&path, 1.5, pr.lane_curve);
            if self.ppq > 30.0 {
                for pt in lane.points.iter().filter(|pt| pt.time >= a && pt.time <= b) {
                    p.circle(
                        Point::new(self.x_of(pt.time), y_of(pt.value)),
                        2.0,
                        pr.lane_curve,
                    );
                }
            }
        } else {
            p.text(
                &format!("No {} — draw to add", c.label()),
                area,
                &TextStyle::new(self.theme.fonts.small, self.theme.ui.text_faint).center(),
            );
        }
        if let Some(Drag::Controller {
            points,
            from,
            to,
            line,
            erase,
        }) = &self.drag
        {
            if *erase {
                let r =
                    Rect::from_points(Point::new(from.x, area.y), Point::new(to.x, area.bottom()));
                p.fill(r, Color::rgba(1.0, 0.3, 0.2, 0.2));
            } else if *line {
                p.line(*from, *to, 2.0, pr.lane_curve.lighten(0.3));
            } else {
                for (t, v) in points {
                    p.circle(
                        Point::new(self.x_of(*t), y_of(*v)),
                        2.0,
                        pr.lane_curve.lighten(0.3),
                    );
                }
            }
        }
    }
}
