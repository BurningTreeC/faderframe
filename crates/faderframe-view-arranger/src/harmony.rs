//! The Key and Chords lanes: key changes as spans named after their key
//! (click a bar for a change there, drag across for a span in another key,
//! drag a change to move it; click a change for roots and scales,
//! double-click to type one), and the chord
//! track (drag across the lane for a new chord, then type it; drag a chord
//! to move it, its edges to resize; double-click to retype; right-click
//! for the key's chords). Edits are whole-list commands (`SetKeys`,
//! `SetChords`), one undo step each.

use super::global::{GlobalDrag, GlobalHit, SectionPart};
use super::*;
use faderframe_project::harmony::{self, Chord, ChordEvent, Key, Scale};
use faderframe_session::lanes::GlobalLane;

/// Grab width of chord edges.
const EDGE: f32 = 5.0;

/// The scales the key menu offers.
const MENU_SCALES: [Scale; 12] = [
    Scale::Major,
    Scale::Minor,
    Scale::HarmonicMinor,
    Scale::MelodicMinor,
    Scale::Dorian,
    Scale::Phrygian,
    Scale::Lydian,
    Scale::Mixolydian,
    Scale::Locrian,
    Scale::MajorPentatonic,
    Scale::MinorPentatonic,
    Scale::Blues,
];

impl ArrangerView {
    /// A chord's rectangle in the lane.
    fn chord_rect(&self, c: &ChordEvent, lane: Rect) -> Rect {
        let (x0, x1) = (self.x_of(c.start), self.x_of(c.end));
        Rect::new(
            x0 + 1.0,
            lane.y + 2.0,
            (x1 - x0 - 2.0).max(2.0),
            lane.h - 4.0,
        )
    }

    /// The chords as shown (with a drag in progress applied).
    fn shown_chords(&self, model: &Session) -> Vec<ChordEvent> {
        let chords = &model.project().chords;
        match &self.global_drag {
            Some(GlobalDrag::Chord {
                index,
                start,
                end,
                moved: true,
                ..
            }) => {
                let Some(c) = chords.get(*index) else {
                    return chords.clone();
                };
                let mut rest: Vec<ChordEvent> = chords
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| i != index)
                    .map(|(_, c)| *c)
                    .collect();
                rest = harmony::set_chord(&rest, *start, *end, Some(c.chord));
                rest
            }
            _ => chords.clone(),
        }
    }

    /// A key change is hit on its bar or its name; the rest of its span is
    /// room for new changes.
    pub(crate) fn key_hit(&self, pos: Point, at: MusicalTime, model: &Session) -> GlobalHit {
        let p = model.project();
        let meter = &p.timeline.meter;
        let x = pos.x;
        for (i, k) in p.keys.iter().enumerate() {
            let x0 = self.x_of(k.at);
            let bar_end = self.x_of(meter.bar_start(meter.bar_at(k.at) + 1));
            let label = x0 + k.key.name().chars().count() as f32 * 7.0 + 14.0;
            let x1 = p.keys.get(i + 1).map_or(f32::MAX, |n| self.x_of(n.at));
            if x >= x0 - EDGE && x < bar_end.max(label).min(x1) {
                return GlobalHit::Key(i);
            }
        }
        GlobalHit::Empty(GlobalLane::Key, at)
    }

    /// The key changes as shown (a move in progress applied).
    fn shown_keys(&self, model: &Session) -> Vec<harmony::KeyChange> {
        let keys = &model.project().keys;
        match &self.global_drag {
            Some(GlobalDrag::KeyChange {
                from,
                to,
                moved: true,
                ..
            }) => harmony::move_key(keys, *from, *to),
            _ => keys.clone(),
        }
    }

    pub(crate) fn chord_hit(
        &self,
        pos: Point,
        lane: Rect,
        at: MusicalTime,
        model: &Session,
    ) -> GlobalHit {
        for (i, c) in model.project().chords.iter().enumerate() {
            let r = self.chord_rect(c, lane);
            if (self.x_of(c.start) - pos.x).abs() <= EDGE && pos.x >= self.x_of(c.start) - EDGE {
                return GlobalHit::Chord(i, SectionPart::Start);
            }
            if (self.x_of(c.end) - pos.x).abs() <= EDGE {
                return GlobalHit::Chord(i, SectionPart::End);
            }
            if r.contains(pos) {
                return GlobalHit::Chord(i, SectionPart::Body);
            }
        }
        GlobalHit::Empty(GlobalLane::Chords, at)
    }

    // --- painting ------------------------------------------------------------------

    pub(crate) fn paint_keys(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let keys = &self.shown_keys(model);
        if let Some(GlobalDrag::NewKey { anchor, to, .. }) = &self.global_drag {
            let (a, b) = Self::key_span(model, *anchor, *to);
            let (x0, x1) = (self.x_of(a), self.x_of(b));
            let rr = Rect::new(x0, r.y + 2.0, (x1 - x0).max(1.0), r.h - 4.0);
            p.fill_rounded(rr, 3.0, &Paint::Solid(th.ui.accent.with_alpha(0.3)));
            p.stroke_rounded(rr, 3.0, 1.0, th.ui.accent);
        }
        if keys.is_empty() {
            p.text(
                "No key · click to set one",
                Rect::new(r.x + 6.0, r.y, 220.0, r.h),
                &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
            );
            return;
        }
        for (i, k) in keys.iter().enumerate() {
            let x0 = self.x_of(k.at).max(r.x);
            let x1 = keys
                .get(i + 1)
                .map_or(r.right(), |n| self.x_of(n.at))
                .min(r.right());
            if x1 <= r.x || x0 >= r.right() {
                continue;
            }
            let band = Rect::new(x0, r.y + 2.0, (x1 - x0).max(1.0), r.h - 4.0);
            // Cards keep the accent for the change itself.
            let tint = if th.arranger.clip_style == faderframe_ui_canvas::ClipStyle::Card {
                th.ui.text.with_alpha(if i % 2 == 0 { 0.05 } else { 0.08 })
            } else {
                th.ui
                    .accent
                    .with_alpha(if i % 2 == 0 { 0.14 } else { 0.22 })
            };
            p.fill(band, tint);
            p.fill(Rect::new(self.x_of(k.at), r.y, 2.0, r.h), th.ui.accent);
            p.text(
                &k.key.name(),
                Rect::new(band.x + 6.0, r.y, (band.w - 8.0).max(0.0), r.h),
                &TextStyle::new(th.fonts.small, th.ui.text).bold(),
            );
        }
    }

    pub(crate) fn paint_chords(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let project = model.project();
        let playhead = model.playhead();
        for c in self.shown_chords(model) {
            let rr = self.chord_rect(&c, r);
            if rr.right() < r.x || rr.x > r.right() {
                continue;
            }
            let now = c.start <= playhead && playhead < c.end && model.transport().playing;
            // A colour per root round the circle of fifths.
            let fifths = (usize::from(c.chord.root) * 7) % 12;
            let tc = faderframe_project::TrackColor::palette(fifths);
            let base = Color::rgb8(tc.r, tc.g, tc.b);
            p.fill_rounded(
                rr,
                3.0,
                &Paint::Solid(base.with_alpha(if now { 0.65 } else { 0.32 })),
            );
            p.stroke_rounded(rr, 3.0, 1.0, base.with_alpha(0.8));
            let flats = project.flats_at(c.start);
            let name = c.chord.name(flats);
            let text = Rect::new(
                rr.x.max(r.x) + 5.0,
                rr.y,
                (rr.right() - rr.x.max(r.x) - 7.0).max(0.0),
                rr.h,
            );
            p.text(
                &name,
                text,
                &TextStyle::new(th.fonts.small, th.ui.text).bold(),
            );
            if let Some(numeral) = project.key_at(c.start).and_then(|k| c.chord.numeral(k)) {
                let w = name.chars().count() as f32 * 6.5 + 8.0;
                if text.w > w + 24.0 {
                    p.text(
                        &numeral,
                        Rect::new(text.x + w, text.y, text.w - w, text.h),
                        &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
                    );
                }
            }
        }
        if let Some(GlobalDrag::NewChord { anchor, to }) = &self.global_drag {
            let (x0, x1) = (self.x_of(*anchor.min(to)), self.x_of(*anchor.max(to)));
            let rr = Rect::new(x0, r.y + 2.0, (x1 - x0).max(1.0), r.h - 4.0);
            p.fill_rounded(rr, 3.0, &Paint::Solid(th.ui.accent.with_alpha(0.3)));
            p.stroke_rounded(rr, 3.0, 1.0, th.ui.accent);
        }
    }

    // --- interaction ---------------------------------------------------------------

    /// Where a key change for `t` goes: the start of its bar.
    fn key_position(model: &Session, t: MusicalTime) -> MusicalTime {
        let meter = &model.project().timeline.meter;
        meter.bar_start(meter.bar_at(t))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn harmony_press(
        &mut self,
        hit: GlobalHit,
        pos: Point,
        clicks: u32,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let p = model.project();
        match hit {
            GlobalHit::Empty(GlobalLane::Key, t) => {
                let at = if p.keys.is_empty() {
                    MusicalTime::ZERO
                } else {
                    Self::key_position(model, t)
                };
                if clicks >= 2 {
                    cx.request(Self::type_key(
                        model,
                        at,
                        Rect::new(self.x_of(at), pos.y - 10.0, 140.0, 20.0),
                    ));
                } else {
                    // A click: a change at this bar; a drag: a span.
                    let bar = Self::key_position(model, t);
                    self.global_drag = Some(GlobalDrag::NewKey {
                        anchor: bar,
                        to: bar,
                        origin: pos,
                    });
                }
            }
            GlobalHit::Key(i) => {
                let Some(k) = p.keys.get(i) else { return };
                if clicks >= 2 {
                    cx.request(Self::type_key(
                        model,
                        k.at,
                        Rect::new(
                            self.x_of(k.at).max(self.header_w()),
                            pos.y - 10.0,
                            140.0,
                            20.0,
                        ),
                    ));
                } else {
                    // A click: its menu (on release); a drag moves it.
                    self.global_drag = Some(GlobalDrag::KeyChange {
                        from: k.at,
                        to: k.at,
                        origin: pos,
                        moved: false,
                    });
                }
            }
            GlobalHit::Empty(GlobalLane::Chords, t) => {
                let t = self.snap(t, model, mods);
                if clicks >= 2 {
                    // A chord over this bar (up to the next chord).
                    let meter = &p.timeline.meter;
                    let bar = meter.bar_at(t);
                    let start = meter.bar_start(bar).max(
                        p.chords
                            .iter()
                            .map(|c| c.end)
                            .filter(|e| *e <= t)
                            .max()
                            .unwrap_or(MusicalTime::ZERO),
                    );
                    let end = meter.bar_start(bar + 1).min(
                        p.chords
                            .iter()
                            .map(|c| c.start)
                            .filter(|s| *s > t)
                            .min()
                            .unwrap_or(MusicalTime::MAX),
                    );
                    if let Some(lane) = self.lane_rect(GlobalLane::Chords, size) {
                        cx.request(Self::type_chord(
                            model,
                            start,
                            end,
                            None,
                            Rect::new(self.x_of(start), lane.y, 120.0, lane.h),
                        ));
                    }
                } else {
                    self.global_drag = Some(GlobalDrag::NewChord { anchor: t, to: t });
                }
            }
            GlobalHit::Chord(i, part) => {
                let Some(c) = p.chords.get(i).copied() else {
                    return;
                };
                if clicks >= 2 && part == SectionPart::Body {
                    if let Some(lane) = self.lane_rect(GlobalLane::Chords, size) {
                        let rr = self.chord_rect(&c, lane);
                        cx.request(Self::type_chord(
                            model,
                            c.start,
                            c.end,
                            Some(c.chord),
                            Rect::new(rr.x.max(lane.x), rr.y, rr.w.max(120.0), rr.h),
                        ));
                    }
                } else {
                    self.global_drag = Some(GlobalDrag::Chord {
                        index: i,
                        part,
                        grab: self.time_at(pos.x),
                        origin: pos,
                        start: c.start,
                        end: c.end,
                        moved: false,
                    });
                }
            }
            _ => {}
        }
    }

    pub(crate) fn chord_drag(
        &mut self,
        drag: &mut GlobalDrag,
        pos: Point,
        mods: Modifiers,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let t_at = self.time_at(pos.x).max(MusicalTime::ZERO);
        match drag {
            GlobalDrag::NewKey { to, .. } => *to = Self::key_position(model, t_at),
            GlobalDrag::KeyChange {
                to, origin, moved, ..
            } => {
                if !*moved && pos.distance(*origin) < DRAG_THRESHOLD {
                    return;
                }
                *moved = true;
                *to = Self::key_position(model, t_at);
                cx.set_cursor(Cursor::Grabbing);
            }
            GlobalDrag::NewChord { to, .. } => {
                *to = self.snap_point(t_at, model, mods, &Default::default());
            }
            GlobalDrag::Chord {
                index,
                part,
                grab,
                origin,
                start,
                end,
                moved,
            } => {
                if !*moved && pos.distance(*origin) < DRAG_THRESHOLD {
                    return;
                }
                *moved = true;
                let Some(c) = model.project().chords.get(*index) else {
                    return;
                };
                let (c_start, c_end) = (c.start, c.end);
                let skip = faderframe_session::SnapSkip::default();
                match part {
                    SectionPart::Body => {
                        let len = c_end - c_start;
                        let raw = (c_start + (self.time_at(pos.x) - *grab)).max(MusicalTime::ZERO);
                        // Either edge onto an object near it, else the grid.
                        let s = match self.object_shift(&[raw, raw + len], model, mods, &skip) {
                            Some(shift) => (raw + shift).max(MusicalTime::ZERO),
                            None => self.snap(raw, model, mods),
                        };
                        (*start, *end) = (s, s + len);
                        cx.set_cursor(Cursor::Grabbing);
                    }
                    SectionPart::Start => {
                        *start = self
                            .snap_point(t_at, model, mods, &skip)
                            .min(c_end - MusicalTime::from_ticks(1));
                        cx.set_cursor(Cursor::ResizeHorizontal);
                    }
                    SectionPart::End => {
                        *end = self
                            .snap_point(t_at, model, mods, &skip)
                            .max(c_start + MusicalTime::from_ticks(1));
                        cx.set_cursor(Cursor::ResizeHorizontal);
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn chord_release(
        &self,
        drag: GlobalDrag,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let p = model.project();
        match drag {
            GlobalDrag::NewKey { anchor, to, origin } => {
                if anchor == to {
                    // The first key starts the song.
                    let at = if p.keys.is_empty() {
                        MusicalTime::ZERO
                    } else {
                        anchor
                    };
                    let existing = p.keys.iter().any(|k| k.at == at);
                    cx.request(Self::key_menu(model, at, existing, origin));
                } else {
                    let (start, end) = Self::key_span(model, anchor, to);
                    cx.request(Self::key_span_menu(model, start, end, origin));
                }
            }
            GlobalDrag::KeyChange {
                from,
                to,
                origin,
                moved,
            } => {
                if !moved {
                    cx.request(Self::key_menu(model, from, true, origin));
                } else if to != from {
                    cx.emit(Action::Edit(Command::SetKeys {
                        keys: harmony::move_key(&p.keys, from, to),
                    }));
                }
                cx.set_cursor(Cursor::Default);
            }
            GlobalDrag::NewChord { anchor, to } => {
                let (start, end) = (anchor.min(to), anchor.max(to));
                if end > start {
                    if let Some(lane) = self.lane_rect(GlobalLane::Chords, size) {
                        cx.request(Self::type_chord(
                            model,
                            start,
                            end,
                            None,
                            Rect::new(
                                self.x_of(start),
                                lane.y,
                                (self.x_of(end) - self.x_of(start)).max(120.0),
                                lane.h,
                            ),
                        ));
                    }
                } else {
                    cx.emit(Action::Transport(TransportAction::Locate(anchor)));
                }
            }
            GlobalDrag::Chord {
                index,
                start,
                end,
                moved,
                ..
            } => {
                let Some(c) = p.chords.get(index).copied() else {
                    return;
                };
                if !moved {
                    cx.emit(Action::Transport(TransportAction::Locate(c.start)));
                    return;
                }
                if (start, end) == (c.start, c.end) {
                    return;
                }
                let mut rest: Vec<ChordEvent> = p.chords.clone();
                rest.remove(index);
                cx.emit(Action::Edit(Command::SetChords {
                    chords: harmony::set_chord(&rest, start, end, Some(c.chord)),
                }));
            }
            _ => {}
        }
    }

    // --- menus and text input ---------------------------------------------------------

    /// Roots and scales for the key from `at`.
    pub(crate) fn key_menu(
        model: &Session,
        at: MusicalTime,
        existing: bool,
        pos: Point,
    ) -> HostRequest<Action> {
        let p = model.project();
        let current = p.key_at(at);
        let flats = current.is_some_and(Key::flats);
        let bar = p.timeline.meter.bar_at(at) + 1;
        let set = |key: Option<Key>| {
            Action::Edit(Command::SetKeys {
                keys: harmony::set_key(&p.keys, at, key),
            })
        };
        let mut items = vec![MenuItem::disabled(match current {
            Some(k) if existing => format!("{} · from bar {bar}", k.name()),
            Some(k) => format!("Key change at bar {bar} (now {})", k.name()),
            None => format!("Key from bar {bar}"),
        })];
        let scale = current.map_or(Scale::Major, |k| k.scale);
        for root in 0..12u8 {
            let key = Key::new(root, scale);
            let mut item = MenuItem::new(
                format!("{} {}", harmony::pc_name(root, flats), scale.name()),
                set(Some(key)),
            )
            .checked(current.is_some_and(|k| k.root == root));
            if root == 0 {
                item = item.separated();
            }
            items.push(item);
        }
        let root = current.map_or(0, |k| k.root);
        for (i, s) in MENU_SCALES.iter().enumerate() {
            let mut item = MenuItem::new(s.name().to_string(), set(Some(Key::new(root, *s))))
                .checked(current.is_some_and(|k| k.scale == *s));
            if i == 0 {
                item = item.separated();
            }
            items.push(item);
        }
        items.push(MenuItem::new("Detect Key from Clips", Action::DetectKey).separated());
        if existing {
            items.push(MenuItem::new("Remove Key Change", set(None)));
        }
        HostRequest::ContextMenu { at: pos, items }
    }

    /// The bars a key drag covers: from the earlier bar to the end of the
    /// later one.
    fn key_span(model: &Session, a: MusicalTime, b: MusicalTime) -> (MusicalTime, MusicalTime) {
        let meter = &model.project().timeline.meter;
        let (first, last) = (a.min(b), a.max(b));
        (first, meter.bar_start(meter.bar_at(last) + 1))
    }

    /// The keys for a drawn span (`start..end`): the key there changes and
    /// the one after comes back at its end.
    fn key_span_menu(
        model: &Session,
        start: MusicalTime,
        end: MusicalTime,
        pos: Point,
    ) -> HostRequest<Action> {
        let p = model.project();
        let meter = &p.timeline.meter;
        let current = p.key_at(start);
        let flats = current.is_some_and(Key::flats);
        let set = |key: Key| {
            Action::Edit(Command::SetKeys {
                keys: harmony::set_key_span(&p.keys, start, end, key),
            })
        };
        let mut items = vec![MenuItem::disabled(format!(
            "Key for bars {}–{}",
            meter.bar_at(start) + 1,
            meter.bar_at(end - MusicalTime::from_ticks(1)) + 1
        ))];
        let scale = current.map_or(Scale::Major, |k| k.scale);
        for root in 0..12u8 {
            let mut item = MenuItem::new(
                format!("{} {}", harmony::pc_name(root, flats), scale.name()),
                set(Key::new(root, scale)),
            );
            if root == 0 {
                item = item.separated();
            }
            items.push(item);
        }
        let root = current.map_or(0, |k| k.root);
        for (i, s) in MENU_SCALES.iter().enumerate() {
            let mut item = MenuItem::new(
                format!("{} {}", harmony::pc_name(root, flats), s.name()),
                set(Key::new(root, *s)),
            );
            if i == 0 {
                item = item.separated();
            }
            items.push(item);
        }
        HostRequest::ContextMenu { at: pos, items }
    }

    fn type_key(model: &Session, at: MusicalTime, rect: Rect) -> HostRequest<Action> {
        let keys = model.project().keys.clone();
        let initial = model
            .project()
            .key_at(at)
            .map_or_else(|| "C Major".to_string(), Key::name);
        HostRequest::TextInput {
            at: rect,
            initial,
            commit: Box::new(move |text| {
                Key::parse(text).map(|key| {
                    Action::Edit(Command::SetKeys {
                        keys: harmony::set_key(&keys, at, Some(key)),
                    })
                })
            }),
        }
    }

    fn type_chord(
        model: &Session,
        start: MusicalTime,
        end: MusicalTime,
        current: Option<Chord>,
        rect: Rect,
    ) -> HostRequest<Action> {
        let p = model.project();
        let chords = p.chords.clone();
        let flats = p.flats_at(start);
        // A new chord suggests the key's tonic.
        let initial = current
            .or_else(|| p.key_at(start).and_then(|k| k.chord_on(0, false)))
            .map_or_else(|| "C".to_string(), |c| c.name(flats));
        HostRequest::TextInput {
            at: rect,
            initial,
            commit: Box::new(move |text| {
                Chord::parse(text).map(|chord| {
                    Action::Edit(Command::SetChords {
                        chords: harmony::set_chord(&chords, start, end, Some(chord)),
                    })
                })
            }),
        }
    }

    /// A chord's menu: the key's chords to put there instead, delete.
    pub(crate) fn chord_menu(model: &Session, index: usize, pos: Point) -> HostRequest<Action> {
        let p = model.project();
        let Some(c) = p.chords.get(index).copied() else {
            return HostRequest::ContextMenu {
                at: pos,
                items: Vec::new(),
            };
        };
        let key = p.key_at(c.start).unwrap_or(Key::new(0, Scale::Major));
        let flats = key.flats();
        let replace = |chord: Chord| {
            Action::Edit(Command::SetChords {
                chords: harmony::set_chord(&p.chords, c.start, c.end, Some(chord)),
            })
        };
        let mut items = vec![MenuItem::disabled(match c.chord.numeral(key) {
            Some(n) => format!("{} · {n} in {}", c.chord.name(flats), key.name()),
            None => c.chord.name(flats),
        })];
        for sevenths in [false, true] {
            for d in 0..7 {
                if let Some(ch) = key.chord_on(d, sevenths) {
                    let label = format!(
                        "{}   {}",
                        ch.name(flats),
                        ch.numeral(key).unwrap_or_default()
                    );
                    let mut item = MenuItem::new(label, replace(ch)).checked(ch == c.chord);
                    if d == 0 {
                        item = item.separated();
                    }
                    items.push(item);
                }
            }
        }
        let mut rest = p.chords.clone();
        rest.remove(index);
        items.push(
            MenuItem::new(
                "Delete Chord",
                Action::Edit(Command::SetChords { chords: rest }),
            )
            .separated(),
        );
        items.push(MenuItem::new(
            "Delete All Chords",
            Action::Edit(Command::SetChords { chords: Vec::new() }),
        ));
        items.push(MenuItem::new(
            "Detect Chords from Clips",
            Action::DetectChords,
        ));
        HostRequest::ContextMenu { at: pos, items }
    }

    pub(crate) fn chords_lane_menu(model: &Session, pos: Point) -> HostRequest<Action> {
        let mut items = vec![
            MenuItem::disabled("Drag across the lane for a chord · double-click for one a bar"),
            MenuItem::new("Detect Chords from Clips", Action::DetectChords).separated(),
            MenuItem::new("Detect Key from Clips", Action::DetectKey),
        ];
        if !model.project().chords.is_empty() {
            items.push(
                MenuItem::new(
                    "Delete All Chords",
                    Action::Edit(Command::SetChords { chords: Vec::new() }),
                )
                .separated(),
            );
        }
        HostRequest::ContextMenu { at: pos, items }
    }

    pub(crate) fn harmony_tooltip(&self, hit: GlobalHit, model: &Session) -> Option<String> {
        let p = model.project();
        Some(match hit {
            GlobalHit::Empty(GlobalLane::Key, _) => "Click for a key change at this bar · Drag across for a passage in another key · Double-click to type one".into(),
            GlobalHit::Key(i) => {
                let k = p.keys.get(i)?;
                format!("{} · Click for roots and scales · Drag to move · Double-click to type", k.key.name())
            }
            GlobalHit::Empty(GlobalLane::Chords, _) => {
                "Drag across for a chord · Double-click for one a bar · Right-click to detect chords from clips".into()
            }
            GlobalHit::Chord(i, part) => {
                let c = p.chords.get(i)?;
                let name = c.chord.name(p.flats_at(c.start));
                match part {
                    SectionPart::Body => format!("{name} · Drag to move · Double-click to change · Right-click for the key's chords"),
                    _ => "Drag to resize the chord".into(),
                }
            }
            _ => return None,
        })
    }
}
