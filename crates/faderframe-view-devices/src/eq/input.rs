//! What the pointer, the wheel and the keys do.

use super::bars::{BottomItem, OutputItem, TopItem};
use super::edit::{fresh_band, new_shape};
use super::geometry::{NODE_R, freq_of, note_of};
use super::matching::{self, Match, Reference};
use super::panel::{PanelItem, by_ratio, from_normalized, to_normalized};
use super::{Click, Drag, EqView, Hit, NodeButton, Source, ValueField, instances, key, sketch};
use faderframe_plugin_host::eq::design::{self, BandType, SLOPES};
use faderframe_plugin_host::eq::{
    BANDS, BandParams, Field, Placement, band_id, band_index, global, global_id, listen_key,
    parameters,
};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    Cursor, EventCx, HostRequest, Key, MenuItem, Modifiers, Point, PointerButton, Rect, Size,
    ViewEvent,
};
use std::time::Instant;

/// Changes worked out per band (its slot and settings).
type BandChanges<'a> = &'a dyn Fn(usize, &BandParams) -> Vec<(Field, f64)>;

/// Pixels a press may move before it is a drag.
const SLOP: f32 = 3.0;

impl EqView {
    /// What is at `pos`, topmost first.
    pub(crate) fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        let l = self.layout(size, model);
        if let Some(list) = &self.instances
            && let Some(h) = list.hit(pos, &l, model)
        {
            return Some(Hit::Instances(h));
        }
        if l.top.contains(pos) {
            return self
                .top_items(&l.top)
                .into_iter()
                .find(|(_, r)| r.contains(pos))
                .map(|(i, _)| Hit::Top(i));
        }
        if l.bottom.contains(pos) {
            return self
                .bottom_items(&l.bottom)
                .into_iter()
                .find(|(_, r)| r.contains(pos))
                .map(|(i, _)| Hit::Bottom(i));
        }
        if self.output_open {
            let r = self.output_rect(&l);
            if r.contains(pos) {
                return Some(Hit::Output(
                    self.output_items(&r)
                        .into_iter()
                        .find(|(_, ir)| ir.contains(pos))
                        .map_or(OutputItem::Panel, |(i, _)| i),
                ));
            }
        }
        if let Some(m) = &self.matching
            && let Some(h) = m.hit(pos, &l)
        {
            return Some(Hit::Match(h));
        }
        let tap = self.device.tap(model)?;
        if let Some(h) = self.panel_hit(pos, &l, model, &tap) {
            return Some(h);
        }
        // The values beside the band in view.
        if let Some(b) = self.boxed_band() {
            let bp = BandParams::read(&tap.params, b);
            if bp.used && self.grab.is_none() {
                let at = self.node_at(model, &l, &tap, &bp);
                let (r, parts, _) = self.value_box(&l, at, &bp);
                if r.contains(pos) {
                    for (hit, pr) in parts {
                        if pr.contains(pos) {
                            return Some(match hit {
                                Hit::Value(_, f) => Hit::Value(b, f),
                                Hit::Button(_, k) => Hit::Button(b, k),
                                h => h,
                            });
                        }
                    }
                    return Some(Hit::Node(b));
                }
            }
        }
        let bands = self.used(model).map(|(_, b)| b).unwrap_or_default();
        if l.axis.contains(pos) {
            if self.settings(model).piano {
                let axis = self.axis(model);
                let near = bands
                    .iter()
                    .map(|(b, p)| (*b, (axis.x(&l.graph, p.freq) - pos.x).abs()))
                    .filter(|(_, d)| *d < 7.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                return Some(near.map_or(Hit::Piano(pos), |(b, _)| Hit::PianoDot(b)));
            }
            return Some(Hit::Axis(pos));
        }
        if !l.graph.inset_xy(-4.0, -4.0).contains(pos) {
            return None;
        }
        if let Some(peaks) = &self.grab {
            let s = self.settings(model);
            let axis = self.axis(model);
            let near = peaks
                .iter()
                .map(|(f, d)| {
                    let at = Point::new(
                        axis.x(&l.graph, *f),
                        super::geometry::analyser_y(&l.graph, *d, s.range),
                    );
                    (*f, *d, at.distance(pos))
                })
                .filter(|(_, _, d)| *d < 12.0)
                .min_by(|a, b| a.2.total_cmp(&b.2));
            if let Some((f, d, _)) = near {
                return Some(Hit::Peak(f, d));
            }
            return Some(Hit::Graph(pos));
        }
        // The nearest node or range handle within reach, selected first.
        let mut best: Option<(Hit, (bool, f32))> = None;
        for (b, p) in &bands {
            let mut consider = |hit: Hit, at: Point| {
                let d = at.distance(pos);
                if d <= NODE_R + 4.0 {
                    let rank = (!self.is_selected(*b), d);
                    if best.is_none_or(|(_, r)| rank < r) {
                        best = Some((hit, rank));
                    }
                }
            };
            consider(Hit::Node(*b), self.node_at(model, &l, &tap, p));
            if let Some(h) = self.range_handle(model, &l, &tap, p) {
                consider(Hit::Range(*b), h);
            }
        }
        Some(best.map_or(Hit::Graph(pos), |(h, _)| h))
    }

    pub(crate) fn handle(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button,
                modifiers,
                clicks,
            } => {
                cx.request(HostRequest::GrabFocus);
                self.rest = None;
                let hit = self.hit(pos, size, model);
                if self.output_open
                    && !matches!(hit, Some(Hit::Output(_) | Hit::Bottom(BottomItem::Output)))
                {
                    self.output_open = false;
                }
                let handled = self.press(hit, pos, button, modifiers, clicks, size, model, cx);
                cx.redraw();
                handled
            }
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging,
            } => {
                self.pointer = Some(pos);
                if !dragging || self.drag.is_none() {
                    return self.hover_at(pos, size, model, cx);
                }
                self.drag_to(pos, modifiers, size, model, cx);
                cx.redraw();
                true
            }
            ViewEvent::PointerUp { pos, button, .. } => self.release(pos, button, size, model, cx),
            ViewEvent::Scroll {
                pos, dy, modifiers, ..
            } => self.wheel(pos, dy, modifiers, size, model, cx),
            ViewEvent::Key { key, modifiers } => self.key(key, modifiers, model, cx),
            ViewEvent::PointerLeave => {
                self.pointer = None;
                self.rest = None;
                if self.hover.take().is_some() || self.grab.take().is_some() {
                    cx.redraw();
                }
                false
            }
            _ => false,
        }
    }

    fn hover_at(
        &mut self,
        pos: Point,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match self.rest {
            Some((at, _)) if at.distance(pos) < 4.0 => {}
            _ => self.rest = Some((pos, Instant::now())),
        }
        let hit = self.hit(pos, size, model);
        if self.grab.is_some() && !matches!(hit, Some(Hit::Graph(_) | Hit::Peak(..))) {
            self.grab = None;
        }
        if hit != self.hover {
            // Keep a band's values in view while the pointer is on them.
            self.hover = hit;
        }
        cx.redraw();
        cx.set_cursor(match hit {
            Some(Hit::Node(_) | Hit::Peak(..) | Hit::PianoDot(_)) => Cursor::Grab,
            Some(Hit::Range(_) | Hit::Value(..)) => Cursor::ResizeVertical,
            Some(Hit::Axis(_)) => Cursor::Move,
            Some(Hit::Graph(_)) if self.settings(model).sketch => Cursor::Crosshair,
            _ => Cursor::Default,
        });
        hit.is_some()
    }

    #[allow(clippy::too_many_arguments)]
    fn press(
        &mut self,
        hit: Option<Hit>,
        pos: Point,
        button: PointerButton,
        mods: Modifiers,
        clicks: u32,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let l = self.layout(size, model);
        let Some(hit) = hit else {
            return false;
        };
        let primary = button == PointerButton::Primary;
        match hit {
            Hit::Instances(h) if primary => self.instances_click(h, model, cx),
            Hit::Instances(_) => {}
            Hit::Match(h) if primary => self.match_click(h, size, model, cx),
            Hit::Match(_) => {}
            Hit::Top(item) if primary => self.top_click(item, size, model, cx),
            Hit::Bottom(item) if primary => self.bottom_click(item, size, model, cx),
            Hit::Output(item) if primary => self.output_press(item, pos, clicks, size, model, cx),
            Hit::Panel(item) if primary => {
                self.panel_press(item, pos, clicks, mods, size, model, cx)
            }
            Hit::Panel(PanelItem::Knob(f)) if button == PointerButton::Secondary => {
                if let Some(band) = self.focus {
                    let at = Rect::new(pos.x - 40.0, pos.y - 10.0, 80.0, 20.0);
                    self.type_value(model, cx, band, f, at);
                }
            }
            Hit::Value(b, f) if primary => {
                if !self.is_selected(b) {
                    self.select_only(b);
                }
                if clicks >= 2 {
                    let at = Rect::new(pos.x - 40.0, pos.y - 10.0, 80.0, 20.0);
                    self.type_value(model, cx, b, f.field(), at);
                } else {
                    self.start_value(model, cx, f.field(), b, pos.y);
                }
            }
            Hit::Button(b, which) if primary => {
                let Some(tap) = self.device.tap(model) else {
                    return true;
                };
                let p = BandParams::read(&tap.params, b);
                match which {
                    NodeButton::Bypass => {
                        self.set_once(
                            model,
                            cx,
                            band_id(b, Field::Enabled),
                            if p.enabled { 2.0 } else { 1.0 },
                        );
                    }
                    NodeButton::Solo => self.listen(model, Some(b)),
                    NodeButton::Delete => self.remove(model, cx, &[b]),
                    NodeButton::Menu => {
                        if !self.is_selected(b) {
                            self.select_only(b);
                        }
                        if let Some(req) = self.band_menu(model, b, pos) {
                            cx.request(req);
                        }
                    }
                }
            }
            Hit::Node(b) | Hit::Range(b) if button == PointerButton::Middle => {
                self.select_only(b);
                self.listen(model, Some(b));
            }
            Hit::Node(b) | Hit::Range(b) if button == PointerButton::Secondary => {
                if !self.is_selected(b) {
                    self.select_only(b);
                }
                if let Some(req) = self.band_menu(model, b, pos) {
                    cx.request(req);
                }
            }
            Hit::Node(b) if primary => self.node_press(b, pos, mods, clicks, model, cx),
            Hit::Range(b) if primary => {
                let Some(tap) = self.device.tap(model) else {
                    return true;
                };
                self.focus = Some(b);
                if !self.is_selected(b) {
                    self.select_only(b);
                }
                cx.emit(Action::BeginGesture("EQ Dynamic Range".into()));
                self.drag = Some(Drag::Range {
                    band: b,
                    start: pos,
                    from: BandParams::read(&tap.params, b).range,
                });
            }
            Hit::Peak(f, _) if primary => self.grab_peak(f, pos, model, cx),
            Hit::Graph(at) if primary => self.graph_press(at, mods, clicks, size, model, cx),
            Hit::Graph(at) if button == PointerButton::Secondary => {
                if let Some(req) = self.background_menu(model, size, at) {
                    cx.request(req);
                }
            }
            Hit::Axis(at) if primary => {
                if clicks >= 2 {
                    self.axis = super::FreqAxis {
                        lo: super::F_MIN,
                        hi: super::F_MAX,
                    };
                } else {
                    let axis = self.axis(model);
                    self.drag = Some(Drag::Axis {
                        start: at,
                        from: axis,
                        at: axis.f(&l.graph, at.x),
                    });
                }
            }
            Hit::PianoDot(b) if primary => {
                let Some(tap) = self.device.tap(model) else {
                    return true;
                };
                let p = BandParams::read(&tap.params, b);
                self.select_only(b);
                cx.emit(Action::BeginGesture("EQ Band".into()));
                // A click puts it on the nearest note.
                let note = freq_of(note_of(p.freq).round());
                self.set(model, cx, band_id(b, Field::Freq), note);
                self.drag = Some(Drag::Piano {
                    band: b,
                    start: pos,
                    from: note,
                });
            }
            Hit::Piano(_) => {}
            _ => return false,
        }
        true
    }

    fn node_press(
        &mut self,
        b: usize,
        pos: Point,
        mods: Modifiers,
        clicks: u32,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        let p = BandParams::read(&tap.params, b);
        if mods.alt && (mods.ctrl || mods.meta) {
            // Next shape.
            let i = BandType::MENU
                .iter()
                .position(|k| *k == p.kind)
                .unwrap_or(0);
            let next = BandType::MENU[(i + 1) % BandType::MENU.len()];
            let mut changes = vec![(band_id(b, Field::Type), next.index() as f64)];
            changes.push((band_id(b, Field::Slope), next.snap_slope(p.slope)));
            self.apply(model, cx, "EQ Band Shape", &changes);
            return;
        }
        if mods.alt && mods.shift {
            // Next slope.
            let (lo, hi) = p.kind.slope_range();
            let now = p.kind.snap_slope(p.slope);
            let steps: Vec<f64> = SLOPES
                .iter()
                .copied()
                .filter(|s| *s >= lo && *s <= hi)
                .collect();
            let next = steps
                .iter()
                .copied()
                .find(|s| *s > now + 0.01)
                .or_else(|| steps.first().copied())
                .unwrap_or(now);
            self.set_once(model, cx, band_id(b, Field::Slope), next);
            return;
        }
        let mut click = None;
        if mods.ctrl || mods.meta {
            let was = self.is_selected(b);
            click = Some(Click::ToggleSelected(was));
            if !was {
                self.selected.push(b);
            }
            self.focus = Some(b);
        } else if mods.shift {
            self.select_range(model, b);
        } else if !self.is_selected(b) {
            self.select_only(b);
        } else {
            self.focus = Some(b);
        }
        if mods.alt {
            click = Some(Click::Bypass);
        }
        if clicks >= 2 && !mods.alt {
            let at = Rect::new(pos.x - 40.0, pos.y - 10.0, 80.0, 20.0);
            self.type_value(model, cx, b, Field::Freq, at);
            return;
        }
        let from = self
            .selected
            .iter()
            .map(|s| (*s, BandParams::read(&tap.params, *s)))
            .collect();
        self.drag = Some(Drag::Bands {
            anchor: b,
            start: pos,
            from,
            constrain: mods.alt.then_some(None),
            q: mods.ctrl || mods.meta,
            moved: false,
            began: false,
            click,
        });
    }

    fn graph_press(
        &mut self,
        at: Point,
        mods: Modifiers,
        clicks: u32,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some((tap, bands)) = self.used(model) else {
            return;
        };
        let s = self.settings(model);
        let l = self.layout(size, model);
        let g = l.graph;
        if s.sketch {
            self.start_sketch(at, size, model, cx);
            return;
        }
        // On the overall curve: a band made there, then dragged.
        let axis = self.axis(model);
        let f = axis.f(&g, at.x);
        let curves = self.curves(&tap, &bands, &[f], Self::rate(model));
        let total: f64 = curves
            .iter()
            .filter(|c| c.params.enabled && c.params.placement == Placement::Stereo)
            .map(|c| c.db[0])
            .sum();
        let gains = self.gain_axis(model);
        let on_curve = (gains.y(&g, total as f32) - at.y).abs() < 7.0;
        let dynamic = mods.alt;
        let spectral = mods.alt && mods.shift;
        if on_curve && clicks == 1 && !bands.is_empty() {
            let tx = (at.x - g.x) / g.w;
            let kind = if tx < 0.07 {
                BandType::LowShelf
            } else if tx > 0.93 {
                BandType::HighShelf
            } else {
                BandType::Bell
            };
            cx.emit(Action::BeginGesture("Add EQ Band".into()));
            let shape = new_shape(kind, f, if dynamic { 0.0 } else { total });
            let Some(band) =
                self.add_band(model, cx, shape, dynamic.then_some(0.0), spectral, true)
            else {
                cx.emit(Action::EndGesture);
                return;
            };
            if dynamic {
                self.drag = Some(Drag::Range {
                    band,
                    start: at,
                    from: 0.0,
                });
            } else {
                let mut p = BandParams::read(&tap.params, band);
                p.kind = kind;
                p.freq = f;
                p.gain = total;
                p.used = true;
                p.enabled = true;
                self.drag = Some(Drag::Bands {
                    anchor: band,
                    start: at,
                    from: vec![(band, p)],
                    constrain: None,
                    q: false,
                    moved: true,
                    began: true,
                    click: None,
                });
            }
            return;
        }
        if clicks >= 2 {
            self.create_at(at, mods, size, model, cx);
            return;
        }
        self.drag = Some(Drag::Pending {
            start: at,
            sketch: bands.is_empty(),
        });
    }

    /// A band where a click or double-click lands.
    fn create_at(
        &mut self,
        at: Point,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let (kind, freq, gain) = self.shape_at(model, size, at);
        if mods.alt && kind.has_gain() {
            // A dynamic band: no static gain, the range where the click is.
            let shape = new_shape(kind, freq, 0.0);
            self.add_band(
                model,
                cx,
                shape,
                Some(if gain.abs() < 1.0 { -6.0 } else { gain }),
                mods.shift,
                false,
            );
        } else {
            self.add_band(model, cx, new_shape(kind, freq, gain), None, false, false);
        }
    }

    fn start_sketch(
        &mut self,
        at: Point,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        cx.emit(Action::BeginGesture("EQ Sketch".into()));
        let slots: Vec<usize> = Self::free_slots(&tap).into_iter().take(10).collect();
        self.selected.clear();
        self.focus = None;
        self.drag = Some(Drag::Sketch {
            stroke: sketch::Stroke::default(),
            slots,
            shown: 0,
        });
        self.sketch_to(at, size, model, cx);
    }

    /// Extend the sketch to `pos` and lay out its bands.
    fn sketch_to(&mut self, pos: Point, size: Size, model: &Session, cx: &mut EventCx<'_, Action>) {
        let g = self.layout(size, model).graph;
        let axis = self.axis(model);
        let gains = self.gain_axis(model);
        let f = axis.f(&g, pos.x.clamp(g.x, g.right()));
        let db = f64::from(gains.db(&g, pos.y)).clamp(-30.0, 30.0);
        let Some(Drag::Sketch {
            stroke,
            slots,
            shown,
        }) = &mut self.drag
        else {
            return;
        };
        stroke.push(f, db);
        let bands = sketch::bands(&stroke.points, slots.len());
        let (slots, before) = (slots.clone(), *shown);
        *shown = bands.len();
        let Some(track) = self.owner(model) else {
            return;
        };
        for (slot, shape) in slots.iter().zip(&bands) {
            for (field, v) in fresh_band(shape) {
                cx.emit(Action::Edit(self.command(track, band_id(*slot, field), v)));
            }
        }
        for slot in slots.iter().take(before).skip(bands.len()) {
            cx.emit(Action::Edit(self.command(
                track,
                band_id(*slot, Field::Enabled),
                0.0,
            )));
        }
    }

    fn start_value(
        &mut self,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
        field: Field,
        anchor: usize,
        y: f32,
    ) {
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        let mut bands = self.selected.clone();
        if !bands.contains(&anchor) {
            bands.push(anchor);
        }
        let from = bands
            .iter()
            .map(|b| (*b, f64::from(tap.params.get(band_index(*b, field)))))
            .collect();
        cx.emit(Action::BeginGesture("EQ Band".into()));
        self.drag = Some(Drag::Value {
            field,
            anchor,
            start_y: y,
            from,
        });
    }

    fn drag_to(
        &mut self,
        pos: Point,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let l = self.layout(size, model);
        let g = l.graph;
        let fine = if mods.shift { 0.2 } else { 1.0 };
        let axis = self.axis(model);
        let gains = self.gain_axis(model);
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        let scale = Self::scale(&tap).max(0.01);
        match &mut self.drag {
            Some(Drag::Pending { start, sketch }) => {
                if start.distance(pos) < SLOP {
                    return;
                }
                let (start, sketch) = (*start, *sketch);
                if sketch {
                    self.start_sketch(start, size, model, cx);
                    self.sketch_to(pos, size, model, cx);
                } else {
                    let base = if mods.ctrl || mods.meta {
                        self.selected.clone()
                    } else {
                        Vec::new()
                    };
                    self.drag = Some(Drag::Lasso {
                        start,
                        now: pos,
                        base,
                    });
                    self.drag_to(pos, mods, size, model, cx);
                }
            }
            Some(Drag::Lasso { start, now, base }) => {
                *now = pos;
                let r = Rect::from_points(*start, pos);
                let mut sel = base.clone();
                for b in 0..BANDS {
                    let p = BandParams::read(&tap.params, b);
                    if p.used && r.contains(self.node_at(model, &l, &tap, &p)) && !sel.contains(&b)
                    {
                        sel.push(b);
                    }
                }
                self.focus = sel.last().copied();
                self.selected = sel;
            }
            Some(Drag::Sketch { .. }) => self.sketch_to(pos, size, model, cx),
            Some(Drag::Bands {
                anchor,
                start,
                from,
                constrain,
                q,
                moved,
                began,
                click,
            }) => {
                let (dx, dy) = ((pos.x - start.x) * fine, (pos.y - start.y) * fine);
                if !*moved {
                    if dx.abs().max(dy.abs()) < SLOP {
                        return;
                    }
                    *moved = true;
                    *click = None;
                }
                if !*began {
                    *began = true;
                    cx.emit(Action::BeginGesture("EQ Band".into()));
                }
                if let Some(c) = constrain
                    && c.is_none()
                {
                    *c = Some(dx.abs() >= dy.abs());
                }
                let only = constrain.flatten();
                let (anchor, q) = (*anchor, *q);
                let from = from.clone();
                let Some((_, a)) = from.iter().find(|(b, _)| *b == anchor).copied() else {
                    return;
                };
                let mut changes = Vec::new();
                if q {
                    let k = 2f64.powf(f64::from(-dy) / 60.0);
                    for (b, p) in &from {
                        changes.push((band_id(*b, Field::Q), (p.q * k).clamp(0.025, 40.0)));
                    }
                } else {
                    if only != Some(false) {
                        let x0 = axis.x(&g, a.freq);
                        let to = axis.f(&g, x0 + dx).clamp(10.0, 30_000.0);
                        let ratio = to / a.freq;
                        for (b, p) in &from {
                            changes.push((
                                band_id(*b, Field::Freq),
                                (p.freq * ratio).clamp(10.0, 30_000.0),
                            ));
                        }
                    }
                    if only != Some(true) && a.kind.has_gain() {
                        let per_px = f64::from(gains.per_px(&g));
                        // Tilts show half their gain.
                        let k = if matches!(a.kind, BandType::TiltShelf | BandType::FlatTilt) {
                            2.0
                        } else {
                            1.0
                        };
                        let to = (a.gain - f64::from(dy) * per_px * k / scale).clamp(-30.0, 30.0);
                        for (b, p) in &from {
                            if !p.kind.has_gain() {
                                continue;
                            }
                            // Relative to each other: scaled when the held
                            // band has a gain, else moved by as much.
                            let v = if a.gain.abs() > 0.5 && from.len() > 1 {
                                p.gain * to / a.gain
                            } else {
                                p.gain + (to - a.gain)
                            };
                            changes.push((band_id(*b, Field::Gain), v.clamp(-30.0, 30.0)));
                        }
                        self.expand_range(to * scale, model, cx);
                    }
                }
                for (id, v) in changes {
                    self.set(model, cx, id, v);
                }
                cx.set_cursor(Cursor::Grabbing);
            }
            Some(Drag::Range { band, start, from }) => {
                let per_px = f64::from(gains.per_px(&g));
                let v = (*from - f64::from(pos.y - start.y) * f64::from(fine) * per_px / scale)
                    .clamp(-30.0, 30.0);
                let band = *band;
                self.set(model, cx, band_id(band, Field::Range), v);
            }
            Some(Drag::Value {
                field,
                anchor,
                start_y,
                from,
            }) => {
                let (field, anchor) = (*field, *anchor);
                let Some(&(_, a)) = from.iter().find(|(b, _)| *b == anchor) else {
                    return;
                };
                let t = to_normalized(field, a) + (*start_y - pos.y) / 200.0 * fine;
                let mut to = from_normalized(field, t);
                if field == Field::Slope {
                    let p = BandParams::read(&tap.params, anchor);
                    to = if mods.shift && p.kind.slope_step().is_none() {
                        (to * 2.0).round() / 2.0
                    } else {
                        nearest_slope(p.kind, to)
                    };
                }
                let from = from.clone();
                for (b, v) in from {
                    let nv = if b == anchor {
                        to
                    } else if by_ratio(field) && a > 0.0 {
                        v * to / a
                    } else {
                        v + (to - a)
                    };
                    let info = &parameters()[band_index(b, field)];
                    self.set(model, cx, band_id(b, field), info.clamp(nv));
                }
            }
            Some(Drag::Slider { field, track }) => {
                let (field, track) = (*field, *track);
                self.slide(field, &track, pos, model, cx);
            }
            Some(Drag::Global {
                index,
                start,
                from,
                horizontal,
            }) => {
                let (index, start, from, horizontal) = (*index, *start, *from, *horizontal);
                let info = &parameters()[index];
                let span = info.max - info.min;
                let v = if horizontal {
                    let r = self
                        .output_items(&self.output_rect(&l))
                        .into_iter()
                        .find(|(i, _)| *i == OutputItem::Scale)
                        .map(|(_, r)| r)
                        .unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
                    info.min + span * f64::from(((pos.x - r.x) / r.w).clamp(0.0, 1.0))
                } else {
                    from + span * f64::from((start.y - pos.y) / 200.0 * fine)
                };
                let id = global_id(index);
                self.set(model, cx, id, info.clamp(v));
            }
            Some(Drag::Axis { start, from, at }) => {
                let top = Self::top(model);
                let factor = 2f64.powf(f64::from(start.y - pos.y) / 60.0);
                let z = from.zoomed(*at, factor, top);
                let octaves = -f64::from((pos.x - start.x) / g.w) * (z.hi / z.lo).log2();
                self.axis = z.panned(octaves, top);
            }
            Some(Drag::Piano { band, start, from }) => {
                let per_note = axis.x(&g, freq_of(note_of(*from) + 1.0)) - axis.x(&g, *from);
                let n = note_of(*from).round()
                    + f64::from(((pos.x - start.x) / per_note.max(1.0)).round());
                let band = *band;
                self.set(
                    model,
                    cx,
                    band_id(band, Field::Freq),
                    freq_of(n).clamp(10.0, 30_000.0),
                );
            }
            None => {}
        }
    }

    /// The display range grows when a gain goes past it.
    fn expand_range(&self, db: f64, model: &Session, cx: &mut EventCx<'_, Action>) {
        let s = self.settings(model);
        if db.abs() as f32 <= s.display {
            return;
        }
        if let Some(i) = super::DISPLAY_RANGES
            .iter()
            .position(|r| f64::from(*r) >= db.abs())
        {
            cx.emit(self.view_action(key::DISPLAY, i as f64));
        }
    }

    /// Set the selected bands' `field` from where the pointer is along a
    /// slider.
    fn slide(
        &self,
        field: Field,
        track: &Rect,
        pos: Point,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let t = ((pos.x - track.x) / track.w).clamp(0.0, 1.0);
        let v = match field {
            // Its top is "auto".
            Field::Threshold if t > 0.985 => 0.0,
            Field::Threshold => from_normalized(Field::Threshold, t).min(-0.1),
            _ => from_normalized(field, t),
        };
        for b in self.selected.clone() {
            self.set(model, cx, band_id(b, field), v);
        }
    }

    fn release(
        &mut self,
        pos: Point,
        button: PointerButton,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        if button == PointerButton::Middle || self.listening {
            self.listen(model, None);
            cx.redraw();
        }
        let Some(drag) = self.drag.take() else {
            return false;
        };
        match drag {
            Drag::Pending { start, .. } => {
                if self.selected.is_empty() {
                    self.create_at(start, Modifiers::NONE, size, model, cx);
                } else {
                    self.selected.clear();
                    self.focus = None;
                }
            }
            Drag::Lasso { .. } | Drag::Axis { .. } => {}
            Drag::Sketch { slots, shown, .. } => {
                cx.emit(Action::EndGesture);
                self.selected = slots.into_iter().take(shown).collect();
                self.focus = self.selected.last().copied();
            }
            Drag::Bands {
                anchor,
                began,
                click,
                ..
            } => {
                if began {
                    cx.emit(Action::EndGesture);
                }
                match click {
                    Some(Click::Bypass) => {
                        if let Some(tap) = self.device.tap(model) {
                            let p = BandParams::read(&tap.params, anchor);
                            self.set_once(
                                model,
                                cx,
                                band_id(anchor, Field::Enabled),
                                if p.enabled { 2.0 } else { 1.0 },
                            );
                        }
                    }
                    // A ctrl-click on a band already selected drops it.
                    Some(Click::ToggleSelected(true)) => self.toggle(anchor),
                    Some(Click::ToggleSelected(false)) => {}
                    None => {}
                }
                self.grab = None;
            }
            Drag::Range { .. }
            | Drag::Value { .. }
            | Drag::Global { .. }
            | Drag::Piano { .. }
            | Drag::Slider { .. } => {
                cx.emit(Action::EndGesture);
            }
        }
        let _ = pos;
        cx.redraw();
        true
    }

    fn wheel(
        &mut self,
        pos: Point,
        dy: f32,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let l = self.layout(size, model);
        let up = -dy.signum();
        if let Some(list) = self.instances.as_mut() {
            list.scroll_by(dy * 30.0, &l, model);
            cx.redraw();
            return true;
        }
        let Some(tap) = self.device.tap(model) else {
            return false;
        };
        let hit = self.hit(pos, size, model);
        let step = |field: Field, b: usize| -> f64 {
            let v = f64::from(tap.params.get(band_index(b, field)));
            let t = to_normalized(field, v) + up * if mods.shift { 0.004 } else { 0.02 };
            from_normalized(field, t)
        };
        match hit {
            Some(Hit::Panel(PanelItem::Knob(field))) => {
                let Some(b) = self.focus else {
                    return false;
                };
                let to = step(field, b);
                let a = f64::from(tap.params.get(band_index(b, field)));
                let changes: Vec<_> = self
                    .selected
                    .iter()
                    .map(|s| {
                        let v = f64::from(tap.params.get(band_index(*s, field)));
                        let nv = if *s == b {
                            to
                        } else if by_ratio(field) && a > 0.0 {
                            v * to / a
                        } else {
                            v + (to - a)
                        };
                        (
                            band_id(*s, field),
                            parameters()[band_index(*s, field)].clamp(nv),
                        )
                    })
                    .collect();
                self.apply(model, cx, "EQ Band", &changes);
            }
            Some(Hit::Value(b, f)) => {
                let v = if f == ValueField::Slope {
                    let p = BandParams::read(&tap.params, b);
                    step_slope(&p, up, mods.shift)
                } else {
                    step(f.field(), b)
                };
                self.set_once(model, cx, band_id(b, f.field()), v);
            }
            Some(Hit::Axis(at)) => {
                let axis = self.axis(model);
                let f = axis.f(&l.graph, at.x);
                self.axis = axis.zoomed(f, 1.25f64.powf(f64::from(up)), Self::top(model));
            }
            Some(Hit::Node(b) | Hit::Range(b)) => self.wheel_band(&[b], up, mods, model, cx),
            Some(Hit::Graph(_)) if !self.selected.is_empty() => {
                let sel = self.selected.clone();
                self.wheel_band(&sel, up, mods, model, cx);
            }
            _ => return false,
        }
        cx.redraw();
        true
    }

    /// The wheel over bands: Q (a cut's slope), with Alt the dynamic range,
    /// with Ctrl the gain, with both gain traded for range.
    fn wheel_band(
        &self,
        bands: &[usize],
        up: f32,
        mods: Modifiers,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        let db = f64::from(up) * if mods.shift { 0.1 } else { 0.5 };
        let ctrl = mods.ctrl || mods.meta;
        let mut changes = Vec::new();
        for &b in bands {
            let p = BandParams::read(&tap.params, b);
            if mods.alt && ctrl {
                if p.kind.has_gain() {
                    changes.push((band_id(b, Field::Gain), (p.gain - db).clamp(-30.0, 30.0)));
                    changes.push((band_id(b, Field::Range), (p.range + db).clamp(-30.0, 30.0)));
                }
            } else if mods.alt {
                if p.kind.has_gain() {
                    changes.push((band_id(b, Field::Range), (p.range + db).clamp(-30.0, 30.0)));
                }
            } else if ctrl {
                if p.kind.has_gain() {
                    changes.push((band_id(b, Field::Gain), (p.gain + db).clamp(-30.0, 30.0)));
                }
            } else if p.kind.is_cut() || !p.kind.has_q(p.slope) {
                if p.kind.has_slope() {
                    changes.push((band_id(b, Field::Slope), step_slope(&p, up, mods.shift)));
                }
            } else {
                let k = if mods.shift { 1.02f64 } else { 1.1 };
                changes.push((
                    band_id(b, Field::Q),
                    (p.q * k.powf(f64::from(up))).clamp(0.025, 40.0),
                ));
            }
        }
        self.apply(model, cx, "EQ Band", &changes);
    }

    fn key(
        &mut self,
        key: Key,
        mods: Modifiers,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let ctrl = mods.ctrl || mods.meta;
        match key {
            Key::Delete | Key::Backspace if !self.selected.is_empty() => {
                let sel = self.selected.clone();
                self.remove(model, cx, &sel);
            }
            Key::Escape => {
                if self.instances.take().is_none() && self.matching.take().is_none() {
                    if self.output_open {
                        self.output_open = false;
                    } else if self.selected.is_empty() {
                        return false;
                    } else {
                        self.selected.clear();
                        self.focus = None;
                    }
                }
            }
            Key::Char('a') if ctrl => {
                if let Some((_, bands)) = self.used(model) {
                    self.selected = bands.iter().map(|(b, _)| *b).collect();
                    self.focus = self.selected.last().copied();
                }
            }
            Key::Char('c') if ctrl && !self.selected.is_empty() => {
                if let Some(a) = self.copy_action(model, &self.selected) {
                    cx.emit(a);
                }
            }
            Key::Char('v') if ctrl => self.paste(model, cx),
            Key::Left | Key::Right if !self.selected.is_empty() => {
                self.step_focus(model, if key == Key::Left { -1 } else { 1 });
            }
            _ => return false,
        }
        cx.redraw();
        true
    }

    /// Focus (and select) the next band by frequency.
    pub(crate) fn step_focus(&mut self, model: &Session, by: i32) {
        let Some((_, mut bands)) = self.used(model) else {
            return;
        };
        bands.sort_by(|a, b| a.1.freq.total_cmp(&b.1.freq));
        let i = self
            .focus
            .and_then(|f| bands.iter().position(|(b, _)| *b == f))
            .unwrap_or(0) as i32;
        let n = bands.len() as i32;
        if n == 0 {
            return;
        }
        let next = bands[(i + by).rem_euclid(n) as usize].0;
        self.select_only(next);
    }

    // --- spectrum grab ---------------------------------------------------------

    /// A bell at a grabbed peak, dragged at once.
    fn grab_peak(&mut self, freq: f64, pos: Point, model: &Session, cx: &mut EventCx<'_, Action>) {
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        cx.emit(Action::BeginGesture("Spectrum Grab".into()));
        let mut shape = new_shape(BandType::Bell, freq, 0.0);
        shape.q = 4.0;
        let Some(band) = self.add_band(model, cx, shape, None, false, true) else {
            cx.emit(Action::EndGesture);
            return;
        };
        let mut p = BandParams::read(&tap.params, band);
        p.kind = BandType::Bell;
        p.freq = freq;
        p.gain = 0.0;
        p.q = 4.0;
        p.used = true;
        p.enabled = true;
        self.drag = Some(Drag::Bands {
            anchor: band,
            start: pos,
            from: vec![(band, p)],
            constrain: None,
            q: false,
            moved: true,
            began: true,
            click: None,
        });
    }

    // --- bars ------------------------------------------------------------------

    fn top_click(
        &mut self,
        item: TopItem,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let l = self.layout(size, model);
        let r = self
            .top_items(&l.top)
            .into_iter()
            .find(|(i, _)| *i == item)
            .map_or(l.top, |(_, r)| r);
        match item {
            TopItem::Undo => cx.emit(Action::Undo),
            TopItem::Redo => cx.emit(Action::Redo),
            TopItem::A => self.ab_switch(model, cx, 0),
            TopItem::B => self.ab_switch(model, cx, 1),
            TopItem::CopyAb => self.ab_copy(model),
            TopItem::Sketch => {
                let on = self.settings(model).sketch;
                cx.emit(self.view_action(key::SKETCH, if on { 0.0 } else { 1.0 }));
            }
            TopItem::Match => {
                self.matching = match self.matching.take() {
                    Some(_) => None,
                    None => Some(Match::new(model.sample_rate())),
                };
                self.instances = None;
            }
            TopItem::Sidechain => {
                if let Some(req) = self.sidechain_menu(model, &r) {
                    cx.request(req);
                }
            }
            TopItem::Range => cx.request(self.range_menu(model, &r)),
        }
    }

    fn bottom_click(
        &mut self,
        item: BottomItem,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let l = self.layout(size, model);
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        let r = self
            .bottom_items(&l.bottom)
            .into_iter()
            .find(|(i, _)| *i == item)
            .map_or(l.bottom, |(_, r)| r);
        // Menus open upwards from the bar.
        let up = Rect::new(r.x, r.y - 1.0, r.w, 0.0);
        let flip = |g: usize| if tap.params.get(g) >= 0.5 { 0.0 } else { 1.0 };
        match item {
            BottomItem::Piano => {
                let on = self.settings(model).piano;
                cx.emit(self.view_action(key::PIANO, if on { 0.0 } else { 1.0 }));
            }
            BottomItem::Mode => cx.request(self.mode_menu(model, &tap, &up)),
            BottomItem::Instances => {
                self.instances = match self.instances.take() {
                    Some(_) => None,
                    None => Some(instances::List::new()),
                };
            }
            BottomItem::Analyser => cx.request(self.analyser_menu(model, &up)),
            BottomItem::Character => cx.request(self.character_menu(model, &tap, &up)),
            BottomItem::AutoGain => self.set_once(
                model,
                cx,
                global_id(global::AUTO_GAIN),
                flip(global::AUTO_GAIN),
            ),
            BottomItem::Bypass => {
                self.set_once(model, cx, global_id(global::BYPASS), flip(global::BYPASS))
            }
            BottomItem::Output => self.output_open = !self.output_open,
        }
    }

    fn output_press(
        &mut self,
        item: OutputItem,
        pos: Point,
        clicks: u32,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        let flip = |g: usize| if tap.params.get(g) >= 0.5 { 0.0 } else { 1.0 };
        let _ = size;
        match item {
            OutputItem::Gain | OutputItem::Pan => {
                let index = if item == OutputItem::Gain {
                    global::OUTPUT
                } else {
                    global::PAN
                };
                if clicks >= 2 {
                    self.set_once(model, cx, global_id(index), 0.0);
                } else {
                    cx.emit(Action::BeginGesture("EQ Output".into()));
                    self.drag = Some(Drag::Global {
                        index,
                        start: pos,
                        from: f64::from(tap.params.get(index)),
                        horizontal: false,
                    });
                }
            }
            OutputItem::Scale => {
                if clicks >= 2 {
                    self.set_once(model, cx, global_id(global::GAIN_SCALE), 1.0);
                } else {
                    cx.emit(Action::BeginGesture("EQ Gain Scale".into()));
                    self.drag = Some(Drag::Global {
                        index: global::GAIN_SCALE,
                        start: pos,
                        from: f64::from(tap.params.get(global::GAIN_SCALE)),
                        horizontal: true,
                    });
                    let l = self.layout(size, model);
                    self.drag_to(
                        pos,
                        Modifiers::NONE,
                        Size::new(l.bottom.w, l.bottom.bottom()),
                        model,
                        cx,
                    );
                }
            }
            OutputItem::PanMode => self.set_once(
                model,
                cx,
                global_id(global::PAN_MODE),
                flip(global::PAN_MODE),
            ),
            OutputItem::Invert => {
                self.set_once(model, cx, global_id(global::INVERT), flip(global::INVERT))
            }
            OutputItem::AutoGain => self.set_once(
                model,
                cx,
                global_id(global::AUTO_GAIN),
                flip(global::AUTO_GAIN),
            ),
            OutputItem::Panel => {}
        }
    }

    // --- the band controls -----------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn panel_press(
        &mut self,
        item: PanelItem,
        pos: Point,
        clicks: u32,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        let Some(band) = self.focus else {
            return;
        };
        let l = self.layout(size, model);
        let p = BandParams::read(&tap.params, band);
        let rect = self
            .panel_rect(&l, model, &tap)
            .map(|(r, _, bp)| self.panel_items(&r, &bp))
            .unwrap_or_default()
            .into_iter()
            .find(|(i, _)| *i == item)
            .map_or(Rect::new(pos.x, pos.y, 1.0, 1.0), |(_, r)| r);
        let toggle = |on: bool| if on { 0.0 } else { 1.0 };
        match item {
            PanelItem::Bypass => {
                self.set_selected(model, cx, Field::Enabled, if p.enabled { 2.0 } else { 1.0 })
            }
            PanelItem::Shape | PanelItem::Slope | PanelItem::Placement | PanelItem::Split => {
                if let Some(req) = self.panel_menu(item, model, &tap, &rect) {
                    cx.request(req);
                }
            }
            PanelItem::Knob(field) => {
                if mods.ctrl || mods.meta {
                    let d = parameters()[band_index(band, field)].default;
                    self.set_selected(model, cx, field, d);
                } else if clicks >= 2 {
                    self.type_value(
                        model,
                        cx,
                        band,
                        field,
                        Rect::new(rect.x, rect.bottom() - 18.0, rect.w, 18.0),
                    );
                } else {
                    self.start_value(model, cx, field, band, pos.y);
                }
            }
            PanelItem::Ring => {
                if clicks >= 2 {
                    self.set_selected(model, cx, Field::Range, 0.0);
                } else {
                    self.start_value(model, cx, Field::Range, band, pos.y);
                }
            }
            PanelItem::Expand => self.set_selected(model, cx, Field::Dynamics, toggle(p.custom)),
            PanelItem::Spectral => {
                let on = !p.spectral;
                let mut changes = Vec::new();
                for &b in &self.selected {
                    let bp = BandParams::read(&tap.params, b);
                    changes.push((band_id(b, Field::Spectral), f64::from(u8::from(on))));
                    if on && !bp.dynamic() {
                        changes.push((band_id(b, Field::Range), -6.0));
                    }
                }
                self.apply(model, cx, "EQ Spectral", &changes);
            }
            PanelItem::DynBypass => {
                self.set_selected(model, cx, Field::DynBypass, toggle(p.dyn_bypass))
            }
            PanelItem::DynClear => {
                let mut changes = Vec::new();
                for &b in &self.selected {
                    for (f, v) in [
                        (Field::Range, 0.0),
                        (Field::Spectral, 0.0),
                        (Field::Dynamics, 0.0),
                        (Field::DynBypass, 0.0),
                    ] {
                        changes.push((band_id(b, f), v));
                    }
                }
                self.apply(model, cx, "Clear EQ Dynamics", &changes);
            }
            PanelItem::GainQ => {
                let on = tap.params.get(global::GAIN_Q) >= 0.5;
                self.set_once(
                    model,
                    cx,
                    global_id(global::GAIN_Q),
                    if on { 0.0 } else { 1.0 },
                );
            }
            PanelItem::Prev => self.step_focus(model, -1),
            PanelItem::Next => self.step_focus(model, 1),
            PanelItem::Solo => self.listen(model, Some(band)),
            PanelItem::Delete => {
                let sel = self.selected.clone();
                self.remove(model, cx, &sel);
            }
            PanelItem::Threshold | PanelItem::Density => {
                let field = if item == PanelItem::Threshold {
                    Field::Threshold
                } else {
                    Field::Density
                };
                cx.emit(Action::BeginGesture("EQ Dynamics".into()));
                self.slide(field, &rect, pos, model, cx);
                self.drag = Some(Drag::Slider { field, track: rect });
            }
            PanelItem::Key => self.set_selected(model, cx, Field::Key, toggle(p.external)),
            PanelItem::Tilt => {
                self.set_selected(model, cx, Field::SpectralTilt, toggle(p.spectral_tilt))
            }
            PanelItem::Trigger => {
                let free = !p.free;
                let mut changes = Vec::new();
                for &b in &self.selected {
                    let bp = BandParams::read(&tap.params, b);
                    changes.push((band_id(b, Field::Trigger), f64::from(u8::from(free))));
                    // A free trigger starts round the band.
                    if free && bp.trigger_low <= 10.5 && bp.trigger_high >= 29_500.0 {
                        changes.push((
                            band_id(b, Field::TriggerLow),
                            (bp.freq / 2.0).clamp(10.0, 30_000.0),
                        ));
                        changes.push((
                            band_id(b, Field::TriggerHigh),
                            (bp.freq * 2.0).clamp(10.0, 30_000.0),
                        ));
                    }
                }
                self.apply(model, cx, "EQ Trigger", &changes);
            }
            PanelItem::Audition => self.listen(model, Some(listen_key(band))),
            PanelItem::Body => {}
        }
    }

    fn panel_menu(
        &self,
        item: PanelItem,
        model: &Session,
        tap: &faderframe_plugin_host::tap::AnalysisTap,
        r: &Rect,
    ) -> Option<HostRequest<Action>> {
        let band = self.focus?;
        let p = BandParams::read(&tap.params, band);
        let at = super::below(r);
        let all = |field: Field, v: f64| -> Option<Action> {
            let changes: Vec<_> = self
                .selected
                .iter()
                .map(|b| (band_id(*b, field), v))
                .collect();
            self.batch(model, "EQ Band", &changes)
        };
        let item_of = |label: String, a: Option<Action>| match a {
            Some(a) => MenuItem::new(label, a),
            None => MenuItem::disabled(label),
        };
        let items = match item {
            PanelItem::Shape => BandType::MENU
                .iter()
                .map(|k| {
                    item_of(k.name().into(), all(Field::Type, k.index() as f64))
                        .checked(p.kind == *k)
                })
                .collect(),
            PanelItem::Slope => slope_items(&p, &all, &item_of),
            PanelItem::Placement => Placement::ALL
                .iter()
                .map(|pl| {
                    item_of(pl.name().into(), all(Field::Placement, pl.index() as f64))
                        .checked(p.placement == *pl)
                })
                .collect(),
            PanelItem::Split => vec![
                item_of(
                    "Split into Left and Right".into(),
                    self.split_action(model, &self.selected, false),
                ),
                item_of(
                    "Split into Mid and Side".into(),
                    self.split_action(model, &self.selected, true),
                ),
            ],
            _ => return None,
        };
        Some(HostRequest::ContextMenu { at, items })
    }

    /// The menu of a band (the selected bands).
    pub(crate) fn band_menu(
        &self,
        model: &Session,
        band: usize,
        at: Point,
    ) -> Option<HostRequest<Action>> {
        let tap = self.device.tap(model)?;
        let p = BandParams::read(&tap.params, band);
        let sel: Vec<usize> = if self.is_selected(band) {
            self.selected.clone()
        } else {
            vec![band]
        };
        let all = |changes: BandChanges<'_>| -> Option<Action> {
            let mut out = Vec::new();
            for &b in &sel {
                let bp = BandParams::read(&tap.params, b);
                out.extend(changes(b, &bp).into_iter().map(|(f, v)| (band_id(b, f), v)));
            }
            self.batch(model, "EQ Band", &out)
        };
        let set = |field: Field, v: f64| all(&move |_, _| vec![(field, v)]);
        let item_of = |label: String, a: Option<Action>| match a {
            Some(a) => MenuItem::new(label, a),
            None => MenuItem::disabled(label),
        };
        let mut items: Vec<MenuItem<Action>> = BandType::MENU
            .iter()
            .map(|k| {
                item_of(k.name().into(), set(Field::Type, k.index() as f64)).checked(p.kind == *k)
            })
            .collect();
        if p.kind.has_slope() {
            let mut slopes = slope_items(&p, &|f, v| set(f, v), &item_of);
            if let Some(first) = slopes.first_mut() {
                first.separator_before = true;
            }
            items.extend(slopes);
        }
        for (i, pl) in Placement::ALL.iter().enumerate() {
            let it = item_of(pl.name().into(), set(Field::Placement, pl.index() as f64))
                .checked(p.placement == *pl);
            items.push(if i == 0 { it.separated() } else { it });
        }
        if p.kind.has_gain() {
            let dynamic = p.dynamic();
            items.push(
                item_of(
                    if dynamic {
                        "Make Static"
                    } else {
                        "Make Dynamic"
                    }
                    .into(),
                    if dynamic {
                        all(&|_, _| vec![(Field::Range, 0.0), (Field::Spectral, 0.0)])
                    } else {
                        set(Field::Range, -6.0)
                    },
                )
                .separated(),
            );
            items.push(
                item_of(
                    if p.spectral {
                        "Normal Dynamics"
                    } else {
                        "Make Spectral"
                    }
                    .into(),
                    if p.spectral {
                        set(Field::Spectral, 0.0)
                    } else {
                        all(&|_, bp: &BandParams| {
                            let mut c = vec![(Field::Spectral, 1.0)];
                            if !bp.dynamic() {
                                c.push((Field::Range, -6.0));
                            }
                            c
                        })
                    },
                )
                .checked(p.spectral),
            );
            if dynamic {
                items.push(
                    item_of(
                        "Custom Dynamics Settings".into(),
                        set(Field::Dynamics, if p.custom { 0.0 } else { 1.0 }),
                    )
                    .checked(p.custom),
                );
                items.push(
                    item_of(
                        "Trigger from the Sidechain".into(),
                        all(&|_, bp: &BandParams| {
                            vec![
                                (Field::Dynamics, 1.0),
                                (Field::Key, if bp.external { 0.0 } else { 1.0 }),
                            ]
                        }),
                    )
                    .checked(p.keyed_externally()),
                );
            }
            items.push(item_of(
                "Invert Gain".into(),
                all(&|_, bp: &BandParams| vec![(Field::Gain, -bp.gain), (Field::Range, -bp.range)]),
            ));
        }
        items.push(
            item_of(
                "Split into Left and Right".into(),
                self.split_action(model, &sel, false),
            )
            .separated(),
        );
        items.push(item_of(
            "Split into Mid and Side".into(),
            self.split_action(model, &sel, true),
        ));
        items.push(item_of("Copy".into(), self.copy_action(model, &sel)).separated());
        items.push(item_of(
            "Paste".into(),
            self.paste_action(model).map(|(a, _)| a),
        ));
        items.push(
            item_of(
                if p.enabled { "Bypass" } else { "Enable" }.into(),
                set(Field::Enabled, if p.enabled { 2.0 } else { 1.0 }),
            )
            .separated(),
        );
        items.push(item_of("Delete".into(), set(Field::Enabled, 0.0)));
        Some(HostRequest::ContextMenu { at, items })
    }

    /// The menu of the display's background: bands to add there, paste,
    /// copy or delete them all.
    fn background_menu(
        &self,
        model: &Session,
        size: Size,
        at: Point,
    ) -> Option<HostRequest<Action>> {
        let tap = self.device.tap(model)?;
        let (_, freq, gain) = self.shape_at(model, size, at);
        let slot = *Self::free_slots(&tap).first()?;
        let mut items = vec![MenuItem::disabled(format!(
            "Add at {}",
            faderframe_plugin_host::eq::format_hz(freq)
        ))];
        for kind in BandType::MENU {
            let shape = new_shape(kind, freq, gain);
            let changes: Vec<_> = fresh_band(&shape)
                .into_iter()
                .map(|(f, v)| (band_id(slot, f), v))
                .collect();
            if let Some(a) = self.batch(model, "Add EQ Band", &changes) {
                items.push(MenuItem::new(kind.name(), a));
            }
        }
        let used: Vec<usize> = self
            .used(model)
            .map(|(_, b)| b.iter().map(|(i, _)| *i).collect())
            .unwrap_or_default();
        if let Some((a, _)) = self.paste_action(model) {
            items.push(MenuItem::new("Paste", a).separated());
        }
        if !used.is_empty() {
            if let Some(a) = self.copy_action(model, &used) {
                items.push(MenuItem::new("Copy All Bands", a).separated());
            }
            let changes: Vec<_> = used
                .iter()
                .map(|b| (band_id(*b, Field::Enabled), 0.0))
                .collect();
            if let Some(a) = self.batch(model, "Delete EQ Bands", &changes) {
                items.push(MenuItem::new("Delete All Bands", a));
            }
        }
        Some(HostRequest::ContextMenu { at, items })
    }

    // --- overlays --------------------------------------------------------------

    fn instances_click(
        &mut self,
        h: instances::Hit,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        match h {
            instances::Hit::Reference(id) => {
                let s = self.settings(model);
                let on = !(s.external && s.source == Source::Instance(id));
                cx.emit(self.view_actions(&[
                    (key::SOURCE, Source::Instance(id).value()),
                    (key::EXTERNAL, f64::from(u8::from(on))),
                ]));
            }
            instances::Hit::Open(id) => {
                if let Some((t, _)) = model.plugin_slot(id) {
                    cx.emit(Action::OpenPluginEditor {
                        track: t.id,
                        plugin: id,
                        generic: false,
                    });
                }
            }
            instances::Hit::Match(id) => {
                cx.emit(self.view_action(key::MATCH_REFERENCE, Reference::Instance(id).value()));
                self.matching = Some(Match::new(model.sample_rate()));
                self.instances = None;
            }
            instances::Hit::Close => self.instances = None,
            instances::Hit::Body => {}
        }
    }

    fn match_click(
        &mut self,
        h: matching::Hit,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let l = self.layout(size, model);
        let budget = self
            .device
            .tap(model)
            .map_or(0, |t| Self::free_slots(&t).len());
        let Some(m) = self.matching.as_mut() else {
            return;
        };
        match h {
            matching::Hit::Reference => {
                let r = Match::rect(&l);
                let now = Reference::of(model, self.device.plugin);
                let mut items = vec![
                    MenuItem::new(
                        "Side Chain",
                        self.view_action(key::MATCH_REFERENCE, Reference::Sidechain.value()),
                    )
                    .checked(now == Reference::Sidechain),
                    MenuItem::new(
                        "Input (recorded at another time)",
                        self.view_action(key::MATCH_REFERENCE, Reference::Input.value()),
                    )
                    .checked(now == Reference::Input),
                ];
                for (id, label, _) in instances::instances(model) {
                    if id != self.device.plugin {
                        items.push(
                            MenuItem::new(
                                label,
                                self.view_action(
                                    key::MATCH_REFERENCE,
                                    Reference::Instance(id).value(),
                                ),
                            )
                            .checked(now == Reference::Instance(id)),
                        );
                    }
                }
                cx.request(HostRequest::ContextMenu {
                    at: Point::new(r.x + 14.0, r.y + 52.0),
                    items,
                });
            }
            matching::Hit::RecordInput => {
                if !m.input.averaging {
                    m.input.reset_average();
                }
                m.input.averaging = !m.input.averaging;
            }
            matching::Hit::RecordReference => {
                if !m.reference.averaging {
                    m.reference.reset_average();
                }
                m.reference.averaging = !m.reference.averaging;
                // Recording the input as the reference: not both at once.
                if m.reference.averaging
                    && Reference::of(model, self.device.plugin) == Reference::Input
                {
                    m.input.averaging = false;
                }
            }
            matching::Hit::Match => {
                if m.ready() {
                    m.count = 12;
                    m.compute(budget);
                }
            }
            matching::Hit::Fewer => {
                m.count = m.count.saturating_sub(1).max(1);
                m.result = None;
                m.compute(budget);
            }
            matching::Hit::More => {
                m.count = (m.count + 1).min(budget.max(1));
                m.result = None;
                m.compute(budget);
            }
            matching::Hit::Apply => {
                if let Some(bands) = m.result.clone() {
                    self.matching = None;
                    let slots = self.add_bands(model, cx, "EQ Match", &bands);
                    self.selected = slots;
                    self.focus = self.selected.last().copied();
                }
            }
            matching::Hit::Back => m.result = None,
            matching::Hit::Cancel => self.matching = None,
            matching::Hit::Body => {}
        }
    }

    // --- tooltips --------------------------------------------------------------

    pub(crate) fn tip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        Some(
            match self.hit(pos, size, model)? {
                Hit::Top(TopItem::Undo) => "Undo the last change",
                Hit::Top(TopItem::Redo) => "Redo",
                Hit::Top(TopItem::A | TopItem::B) => "Switch between two settings (A/B)",
                Hit::Top(TopItem::CopyAb) => "Copy these settings to the other side",
                Hit::Top(TopItem::Sketch) => "EQ Sketch: draw the curve you want, left to right, and bands follow",
                Hit::Top(TopItem::Match) => "EQ Match: make the input sound like a reference",
                Hit::Top(TopItem::Sidechain) => "Which track feeds the sidechain (the trigger of bands keyed externally, the external spectrum)",
                Hit::Top(TopItem::Range) => "The display's gain range",
                Hit::Bottom(BottomItem::Piano) => "Piano display: quantize band frequencies to notes",
                Hit::Bottom(BottomItem::Mode) => "Zero latency, natural phase (the analog phase too, 128 samples) or linear phase (no phase shift)",
                Hit::Bottom(BottomItem::Instances) => "Every EQ in the project: their spectra, collisions, EQ Match",
                Hit::Bottom(BottomItem::Analyser) => "The analyser: pre, post, the external spectrum, and its settings",
                Hit::Bottom(BottomItem::Character) => "Clean, Subtle (transformer) or Warm (tube) colour",
                Hit::Bottom(BottomItem::AutoGain) => "Keep the loudness of pink noise where it was",
                Hit::Bottom(BottomItem::Bypass) => "Bypass the whole EQ (soft and latency compensated)",
                Hit::Bottom(BottomItem::Output) => "Output gain, pan, phase invert, gain scale",
                Hit::Panel(PanelItem::Ring) => "Dynamic range: drag to make the band move with the level (double-click: static)",
                Hit::Panel(PanelItem::Expand) => "The dynamics' own settings (else all automatic)",
                Hit::Panel(PanelItem::Spectral) => "Spectral dynamics: only the frequencies over the threshold move",
                Hit::Panel(PanelItem::DynBypass) => "Bypass the band's dynamics",
                Hit::Panel(PanelItem::DynClear) => "Clear the dynamics",
                Hit::Panel(PanelItem::GainQ) => "Gain-Q interaction: a bell narrows as it boosts, like an analog console",
                Hit::Panel(PanelItem::Threshold) => "Threshold (at the top: automatic); the yellow bar is the trigger's level",
                Hit::Panel(PanelItem::Key) => "Trigger from the sidechain input",
                Hit::Panel(PanelItem::Trigger) => "Trigger on the band's own region, or on free low and high cuts",
                Hit::Panel(PanelItem::Audition) => "Hold to hear the trigger",
                Hit::Panel(PanelItem::Density) => "How selectively spectral dynamics pick frequencies",
                Hit::Panel(PanelItem::Tilt) => "Tilt the trigger by 3 dB/oct (a mix's highs trigger as readily as its lows)",
                Hit::Panel(PanelItem::Solo) => "Hold to hear the band's region on its own",
                Hit::Panel(PanelItem::Split) => "Split into left and right, or mid and side",
                Hit::Value(..) => "Drag or scroll to change, double-click to type (\"1k\", \"A4\", \"2x\")",
                Hit::Button(_, NodeButton::Solo) => "Hold to hear the band's region",
                Hit::Node(_) => "Drag: frequency and gain (Shift finer, Alt one direction, Ctrl the Q) · wheel: Q · Alt-click: bypass · Ctrl+Alt-click: shape · double-click: type",
                Hit::Range(_) => "Drag the dynamic range",
                Hit::Peak(..) => "Drag the peak to make a bell there",
                Hit::Graph(_) => "Click or double-click to add a band (Alt: dynamic, Alt+Shift: spectral) · drag: select · drag the curve: shelves at its ends",
                Hit::Axis(_) => "Drag up and down to zoom, sideways to scroll; double-click for the whole range",
                Hit::PianoDot(_) => "Click to put the band on a note, drag to move it by semitones",
                _ => return None,
            }
            .into(),
        )
    }
}

/// The slopes a band's menu offers.
fn slope_items(
    p: &BandParams,
    set: &dyn Fn(Field, f64) -> Option<Action>,
    item_of: &dyn Fn(String, Option<Action>) -> MenuItem<Action>,
) -> Vec<MenuItem<Action>> {
    let (lo, hi) = p.kind.slope_range();
    let now = p.kind.snap_slope(p.slope);
    SLOPES
        .iter()
        .copied()
        .filter(|s| *s >= lo && *s <= hi)
        .filter(|s| {
            p.kind
                .slope_step()
                .is_none_or(|step| (s / step).fract() == 0.0)
        })
        .map(|s| {
            item_of(design::slope_name(s), set(Field::Slope, s)).checked((now - s).abs() < 0.05)
        })
        .collect()
}

/// The standard slope nearest to `slope` for a shape.
fn nearest_slope(kind: BandType, slope: f64) -> f64 {
    let (lo, hi) = kind.slope_range();
    SLOPES
        .iter()
        .copied()
        .filter(|s| *s >= lo && *s <= hi)
        .filter(|s| {
            kind.slope_step()
                .is_none_or(|step| (s / step).fract() == 0.0)
        })
        .min_by(|a, b| (a - slope).abs().total_cmp(&(b - slope).abs()))
        .unwrap_or(slope)
}

/// One wheel step of a slope: the next standard one, or half a dB/oct
/// (Shift, cuts).
fn step_slope(p: &BandParams, up: f32, fine: bool) -> f64 {
    let now = p.kind.snap_slope(p.slope);
    if fine && p.kind.slope_step().is_none() {
        return p.kind.snap_slope(now + 0.5 * f64::from(up));
    }
    let (lo, hi) = p.kind.slope_range();
    let steps: Vec<f64> = SLOPES
        .iter()
        .copied()
        .filter(|s| *s >= lo && *s <= hi)
        .filter(|s| {
            p.kind
                .slope_step()
                .is_none_or(|step| (s / step).fract() == 0.0)
        })
        .collect();
    if up > 0.0 {
        steps
            .iter()
            .copied()
            .find(|s| *s > now + 0.01)
            .unwrap_or(now)
    } else {
        steps
            .iter()
            .rev()
            .copied()
            .find(|s| *s < now - 0.01)
            .unwrap_or(now)
    }
}
