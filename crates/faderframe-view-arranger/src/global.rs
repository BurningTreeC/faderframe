//! The global lanes under the ruler: markers, arrangement sections, the
//! key and the chord track (see [`crate::harmony`]), time signature
//! changes and the tempo map.

use super::*;
use faderframe_core::{MarkerId, SectionId};
use faderframe_project::{Marker, Section};
use faderframe_session::ColorTarget;
use faderframe_session::lanes::{GlobalLane, GlobalLanes};
use faderframe_timeline::{TempoCurve, TempoMap, TimeSignature};

/// Lane heights.
fn lane_height(lane: GlobalLane) -> f32 {
    match lane {
        GlobalLane::Video => 40.0,
        GlobalLane::Markers => 18.0,
        GlobalLane::Arranger => 22.0,
        GlobalLane::Key => 18.0,
        GlobalLane::Chords => 24.0,
        GlobalLane::Lyrics => 22.0,
        GlobalLane::Signature => 17.0,
        GlobalLane::Tempo => 46.0,
    }
}

/// Signatures offered in menus.
const SIGNATURES: [(u8, u8); 9] = [
    (2, 4),
    (3, 4),
    (4, 4),
    (5, 4),
    (6, 4),
    (6, 8),
    (7, 8),
    (9, 8),
    (12, 8),
];

/// Grab width of section edges and tempo points.
const EDGE: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SectionPart {
    Body,
    Start,
    End,
}

/// What a pointer is over in the global lanes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GlobalHit {
    /// The lane's title in the header column.
    Label(GlobalLane),
    Empty(GlobalLane, MusicalTime),
    Marker(MarkerId),
    Section(SectionId, SectionPart),
    /// A time signature change at this bar.
    Signature(i32),
    /// Tempo point index.
    Tempo(usize),
    /// A key change (its index).
    Key(usize),
    /// A chord on the chord track (its index).
    Chord(usize, SectionPart),
    /// A lyric line (its index).
    Lyric(usize),
    /// In the Video lane: a clip, or none.
    Video(Option<faderframe_core::VideoClipId>),
}

#[derive(Clone, Debug)]
pub(crate) enum GlobalDrag {
    Marker {
        marker: Marker,
        origin: Point,
        moved: bool,
    },
    NewSection {
        anchor: MusicalTime,
        to: MusicalTime,
    },
    Section {
        section: Section,
        part: SectionPart,
        /// Time under the pointer when pressed.
        grab: MusicalTime,
        origin: Point,
        moved: bool,
        /// Dragging the body moves the section with its content (`to`: the
        /// drop point; `copy`: Ctrl, insert a copy); Shift, or an edge,
        /// only moves or resizes the section itself.
        content: bool,
        to: Option<MusicalTime>,
        copy: bool,
    },
    Tempo {
        index: usize,
        origin: Point,
        position: MusicalTime,
        bpm: f64,
        /// Locked to vertical (tempo) or horizontal (position) once moved.
        axis: Option<bool>,
    },
    NewChord {
        anchor: MusicalTime,
        to: MusicalTime,
    },
    Chord {
        index: usize,
        part: SectionPart,
        grab: MusicalTime,
        origin: Point,
        start: MusicalTime,
        end: MusicalTime,
        moved: bool,
    },
}

impl ArrangerView {
    pub(crate) fn base_ruler_h(&self) -> f32 {
        self.theme.arranger.ruler_height
    }

    /// Shown lanes with their top and height.
    pub(crate) fn global_lanes(&self) -> Vec<(GlobalLane, f32, f32)> {
        let mut y = self.base_ruler_h();
        self.lanes
            .shown()
            .map(|l| {
                let h = lane_height(l);
                let r = (l, y, h);
                y += h;
                r
            })
            .collect()
    }

    pub(crate) fn global_lanes_h(lanes: &GlobalLanes) -> f32 {
        lanes.shown().map(lane_height).sum()
    }

    pub(crate) fn lane_rect(&self, lane: GlobalLane, size: Size) -> Option<Rect> {
        self.global_lanes()
            .into_iter()
            .find(|(l, ..)| *l == lane)
            .map(|(_, y, h)| Rect::new(self.header_w(), y, size.w - self.header_w(), h))
    }

    /// The tempo lane's BPM range (padded around the map's tempi).
    fn tempo_range(model: &Session) -> (f64, f64) {
        let pts = model.project().timeline.tempo.points();
        let lo = pts.iter().map(|p| p.bpm).fold(f64::MAX, f64::min);
        let hi = pts.iter().map(|p| p.bpm).fold(f64::MIN, f64::max);
        let mid = (lo + hi) * 0.5;
        let half = ((hi - lo) * 0.5 * 1.25).max(15.0);
        (
            (mid - half).max(TempoMap::MIN_BPM),
            (mid + half).min(TempoMap::MAX_BPM),
        )
    }

    fn tempo_y(lane: Rect, range: (f64, f64), bpm: f64) -> f32 {
        let t = ((bpm - range.0) / (range.1 - range.0)).clamp(0.0, 1.0) as f32;
        lane.bottom() - 4.0 - t * (lane.h - 8.0)
    }

    #[cfg(test)]
    pub(crate) fn tempo_y_for_test(lane: Rect, model: &Session, bpm: f64) -> f32 {
        Self::tempo_y(lane, Self::tempo_range(model), bpm)
    }

    fn section_name_rect(&self, s: &Section, lane: Rect) -> Rect {
        let (x0, x1) = (self.x_of(s.start), self.x_of(s.end));
        Rect::new(x0, lane.y + 2.0, (x1 - x0).max(2.0), lane.h - 4.0)
    }

    pub(crate) fn global_hit(&self, pos: Point, size: Size, model: &Session) -> Option<GlobalHit> {
        let (lane, y, h) = self
            .global_lanes()
            .into_iter()
            .find(|(_, y, h)| pos.y >= *y && pos.y < y + h)?;
        if pos.x < self.header_w() {
            return Some(GlobalHit::Label(lane));
        }
        let r = Rect::new(self.header_w(), y, size.w - self.header_w(), h);
        let at = self.time_at(pos.x).max(MusicalTime::ZERO);
        let p = model.project();
        let near = |t: MusicalTime, w: f32| (self.x_of(t) - pos.x).abs() <= w;
        Some(match lane {
            GlobalLane::Video => GlobalHit::Video(self.video_hit(pos, r, model)),
            GlobalLane::Markers => p
                .markers
                .iter()
                .rev()
                .find(|m| {
                    let x = self.x_of(m.position);
                    pos.x >= x - 4.0 && pos.x <= x + self.marker_width(m)
                })
                .map_or(GlobalHit::Empty(lane, at), |m| GlobalHit::Marker(m.id)),
            GlobalLane::Arranger => {
                let hit = p.sections.iter().rev().find_map(|s| {
                    let rr = self.section_name_rect(s, r);
                    if near(s.start, EDGE) {
                        Some(GlobalHit::Section(s.id, SectionPart::Start))
                    } else if near(s.end, EDGE) {
                        Some(GlobalHit::Section(s.id, SectionPart::End))
                    } else if rr.contains(pos) {
                        Some(GlobalHit::Section(s.id, SectionPart::Body))
                    } else {
                        None
                    }
                });
                hit.unwrap_or(GlobalHit::Empty(lane, at))
            }
            GlobalLane::Signature => {
                let meter = &p.timeline.meter;
                meter
                    .changes()
                    .iter()
                    .find(|c| {
                        let x = self.x_of(meter.bar_start(c.bar));
                        pos.x >= x - 3.0 && pos.x <= x + 30.0
                    })
                    .map_or(GlobalHit::Empty(lane, at), |c| GlobalHit::Signature(c.bar))
            }
            GlobalLane::Key => self.key_hit(pos, at, model),
            GlobalLane::Chords => self.chord_hit(pos, r, at, model),
            GlobalLane::Lyrics => p
                .lyrics
                .iter()
                .position(|l| {
                    pos.x >= self.x_of(l.start)
                        && pos.x < self.x_of(l.end).max(self.x_of(l.start) + 8.0)
                })
                .map_or(GlobalHit::Empty(lane, at), GlobalHit::Lyric),
            GlobalLane::Tempo => {
                let range = Self::tempo_range(model);
                p.timeline
                    .tempo
                    .points()
                    .iter()
                    .position(|pt| {
                        let c = Point::new(self.x_of(pt.position), Self::tempo_y(r, range, pt.bpm));
                        c.distance(pos) <= EDGE + 2.0
                    })
                    .map_or(GlobalHit::Empty(lane, at), GlobalHit::Tempo)
            }
        })
    }

    fn marker_width(&self, m: &Marker) -> f32 {
        (m.name.chars().count() as f32 * 5.6 + 14.0).min(160.0)
    }

    // --- painting ------------------------------------------------------------------

    pub(crate) fn paint_global_lanes(&self, p: &mut dyn Painter, size: Size, model: &Session) {
        let th = &self.theme;
        let a = &th.arranger;
        for (lane, y, h) in self.global_lanes() {
            let label = Rect::new(0.0, y, self.header_w(), h);
            let r = Rect::new(self.header_w(), y, size.w - self.header_w(), h);
            // Recessed below the ruler (less so on light skins).
            let depth = if th.dark { 1.0 } else { 0.3 };
            p.fill(label, a.ruler_bg.darken(0.12 * depth));
            controls::engraved(p, lane.title(), label.inset_xy(10.0, 0.0), th, Align::Start);
            p.fill(r, a.ruler_bg.darken(0.22 * depth));
            p.push_clip(r);
            match lane {
                GlobalLane::Video => self.paint_video(p, r, model),
                GlobalLane::Markers => self.paint_markers(p, r, model),
                GlobalLane::Arranger => self.paint_sections(p, r, model),
                GlobalLane::Key => self.paint_keys(p, r, model),
                GlobalLane::Chords => self.paint_chords(p, r, model),
                GlobalLane::Lyrics => self.paint_lyrics(p, r, model),
                GlobalLane::Signature => self.paint_signatures(p, r, model),
                GlobalLane::Tempo => self.paint_tempo(p, r, model),
            }
            p.pop_clip();
            p.hline(0.0, size.w, y + h - 0.5, a.header_border);
        }
        // Where a section dragged with its content goes.
        if let Some(GlobalDrag::Section { to: Some(to), .. }) = &self.global_drag
            && let Some(top) = self.lane_rect(GlobalLane::Arranger, size).map(|r| r.y)
        {
            let x = self.x_of(*to);
            if x >= self.header_w() {
                p.fill(Rect::new(x - 1.0, top, 2.0, size.h - top), th.ui.accent);
            }
        }
    }

    /// Where a section dropped at `t` goes: the nearest section boundary
    /// within reach, else the nearest bar line.
    fn section_drop(&self, model: &Session, t: MusicalTime) -> MusicalTime {
        let p = model.project();
        let x = self.x_of(t);
        let near = p
            .sections
            .iter()
            .flat_map(|s| [s.start, s.end])
            .map(|b| ((self.x_of(b) - x).abs(), b))
            .filter(|(d, _)| *d <= 16.0)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, b)) = near {
            return b;
        }
        let meter = &p.timeline.meter;
        let bar = meter.bar_at(t);
        let (a, b) = (meter.bar_start(bar), meter.bar_start(bar + 1));
        if t - a <= b - t { a } else { b }
    }

    /// Each line a block from where it starts to where it ends, its words
    /// in it (cut where the next begins).
    fn paint_lyrics(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let lines = &model.project().lyrics;
        let playhead = model.playhead();
        for (i, l) in lines.iter().enumerate() {
            let x0 = self.x_of(l.start);
            let x1 = self.x_of(l.end).max(x0 + 8.0);
            if x1 < r.x || x0 > r.right() {
                continue;
            }
            let next = lines
                .get(i + 1)
                .map_or(f32::INFINITY, |n| self.x_of(n.start));
            let block = Rect::new(x0, r.y + 3.0, (x1 - x0).max(2.0), r.h - 6.0);
            let now = l.start <= playhead && playhead < l.end;
            p.fill_rounded(
                block,
                3.0,
                &Paint::Solid(if now {
                    th.ui.accent.with_alpha(0.35)
                } else {
                    th.ui.text.with_alpha(0.08)
                }),
            );
            let text_w = (next.min(r.right()) - x0 - 6.0).max(0.0);
            p.text(
                &l.text,
                Rect::new(x0 + 4.0, r.y, text_w, r.h),
                &TextStyle::new(
                    th.fonts.small,
                    if now { th.ui.text } else { th.ui.text_dim },
                ),
            );
        }
    }

    fn paint_markers(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let dragged = match &self.global_drag {
            Some(GlobalDrag::Marker { marker, .. }) => Some(marker.clone()),
            _ => None,
        };
        for m in &model.project().markers {
            let m = dragged.as_ref().filter(|d| d.id == m.id).unwrap_or(m);
            let x = self.x_of(m.position);
            if x > r.right() || x + 200.0 < r.x {
                continue;
            }
            let w = self.marker_width(m);
            let flag = Rect::new(x, r.y + 2.0, w, r.h - 4.0);
            p.fill_rounded(flag, 2.0, &Paint::Solid(th.ui.accent.with_alpha(0.28)));
            p.fill(Rect::new(x, r.y, 2.0, r.h), th.ui.accent);
            p.text(
                &m.name,
                flag.inset_xy(6.0, 0.0),
                &TextStyle::new(th.fonts.small, th.ui.text).bold(),
            );
        }
    }

    fn paint_sections(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let mut sections: Vec<(Section, f32)> = model
            .project()
            .sections
            .iter()
            .map(|s| (s.clone(), 0.85))
            .collect();
        if let Some(GlobalDrag::Section {
            section,
            moved,
            content,
            copy,
            ..
        }) = &self.global_drag
            && let Some(i) = sections.iter().position(|(s, _)| s.id == section.id)
        {
            if !*content {
                sections[i].0 = section.clone();
            } else if *moved {
                // The section stays (dimmed unless copied); a ghost follows
                // the pointer.
                if !*copy {
                    sections[i].1 = 0.35;
                }
                sections.push((section.clone(), 0.6));
            }
        }
        for (s, alpha) in &sections {
            let rr = self.section_name_rect(s, r);
            if rr.right() < r.x || rr.x > r.right() {
                continue;
            }
            let c = Color::rgb8(s.color.r, s.color.g, s.color.b);
            p.fill_rounded(rr, 3.0, &Paint::Solid(c.with_alpha(*alpha)));
            p.stroke_rounded(rr, 3.0, 1.0, c.lighten(0.25));
            let text = Rect::new(
                rr.x.max(r.x) + 6.0,
                rr.y,
                rr.right() - rr.x.max(r.x) - 8.0,
                rr.h,
            );
            p.text(
                &s.name,
                text,
                &TextStyle::new(th.fonts.small, Color::hex(0x141518)).bold(),
            );
        }
        if let Some(GlobalDrag::NewSection { anchor, to }) = &self.global_drag {
            let (x0, x1) = (self.x_of(*anchor.min(to)), self.x_of(*anchor.max(to)));
            let rr = Rect::new(x0, r.y + 2.0, (x1 - x0).max(1.0), r.h - 4.0);
            p.fill_rounded(rr, 3.0, &Paint::Solid(th.ui.accent.with_alpha(0.3)));
            p.stroke_rounded(rr, 3.0, 1.0, th.ui.accent);
        }
    }

    fn paint_signatures(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let meter = &model.project().timeline.meter;
        for c in meter.changes() {
            let x = self.x_of(meter.bar_start(c.bar));
            if x > r.right() || x + 40.0 < r.x {
                continue;
            }
            p.vline(x, r.y + 2.0, r.bottom() - 2.0, th.ui.text_dim);
            p.text(
                &format!("{}/{}", c.signature.numerator, c.signature.denominator),
                Rect::new(x + 4.0, r.y, 40.0, r.h),
                &TextStyle::new(th.fonts.small, th.ui.text).bold(),
            );
        }
    }

    fn paint_tempo(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let tempo = &model.project().timeline.tempo;
        let range = Self::tempo_range(model);
        let mut pts: Vec<_> = tempo.points().to_vec();
        if let Some(GlobalDrag::Tempo {
            index,
            position,
            bpm,
            ..
        }) = &self.global_drag
            && let Some(pt) = pts.get_mut(*index)
        {
            pt.position = *position;
            pt.bpm = *bpm;
        }
        let y_of = |bpm: f64| Self::tempo_y(r, range, bpm);
        // Guides at round tempi.
        let step = if range.1 - range.0 > 80.0 { 20.0 } else { 10.0 };
        let mut g = (range.0 / step).ceil() * step;
        while g < range.1 {
            p.hline(
                r.x,
                r.right(),
                y_of(g),
                th.arranger.ruler_text.with_alpha(0.08),
            );
            g += step;
        }
        let mut path = Path::new();
        for (i, pt) in pts.iter().enumerate() {
            let x = self.x_of(pt.position);
            let y = y_of(pt.bpm);
            if i == 0 {
                path.move_to(Point::new(x, y));
            } else {
                let prev = pts[i - 1];
                if prev.curve == TempoCurve::Constant {
                    path.line_to(Point::new(x, y_of(prev.bpm)));
                }
                path.line_to(Point::new(x, y));
            }
        }
        if let Some(last) = pts.last() {
            path.line_to(Point::new(
                r.right().max(self.x_of(last.position)),
                y_of(last.bpm),
            ));
        }
        p.stroke_path(&path, 1.6, th.ui.accent);
        let style = TextStyle::new(th.fonts.tiny, th.ui.text_dim);
        for pt in &pts {
            let c = Point::new(self.x_of(pt.position), y_of(pt.bpm));
            p.circle(c, 3.5, th.ui.accent);
            let above = c.y > r.y + 14.0;
            let ty = if above { c.y - 13.0 } else { c.y + 3.0 };
            p.text(
                format!("{:.1}", pt.bpm).trim_end_matches(".0"),
                Rect::new(c.x + 5.0, ty, 50.0, 11.0),
                &style,
            );
        }
    }

    /// Faint marker and section boundary lines across the tracks.
    pub(crate) fn paint_marker_guides(&self, p: &mut dyn Painter, lanes: Rect, model: &Session) {
        let color = self.theme.ui.accent.with_alpha(0.22);
        for m in &model.project().markers {
            let x = self.x_of(m.position);
            if x >= lanes.x && x <= lanes.right() {
                p.vline(x, lanes.y, lanes.bottom(), color);
            }
        }
    }

    // --- interaction -----------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn global_press(
        &mut self,
        hit: GlobalHit,
        pos: Point,
        clicks: u32,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let p = model.project();
        let snap = |t: MusicalTime| self.snap(t, model, mods);
        match hit {
            GlobalHit::Label(lane) => cx.request(Self::lanes_menu(model, lane, pos)),
            GlobalHit::Video(clip) => self.video_press(clip, pos, clicks, model, cx),
            GlobalHit::Empty(GlobalLane::Markers, t) if clicks >= 2 => {
                cx.emit(Action::AddMarker(snap(t)));
            }
            GlobalHit::Empty(GlobalLane::Markers, t) => {
                cx.emit(Action::Transport(TransportAction::Locate(snap(t))));
            }
            GlobalHit::Marker(id) => {
                let Some(m) = p.markers.iter().find(|m| m.id == id) else {
                    return true;
                };
                if clicks >= 2 {
                    let rect = Rect::new(
                        self.x_of(m.position),
                        pos.y - 10.0,
                        self.marker_width(m).max(120.0),
                        20.0,
                    );
                    cx.request(Self::rename_marker(m, rect));
                } else {
                    self.global_drag = Some(GlobalDrag::Marker {
                        marker: m.clone(),
                        origin: pos,
                        moved: false,
                    });
                }
            }
            GlobalHit::Empty(GlobalLane::Arranger, t) => {
                let t = snap(t);
                self.global_drag = Some(GlobalDrag::NewSection { anchor: t, to: t });
            }
            GlobalHit::Section(id, part) => {
                let Some(s) = p.sections.iter().find(|s| s.id == id) else {
                    return true;
                };
                if clicks >= 2 && part == SectionPart::Body {
                    if let Some(lane) = self.lane_rect(GlobalLane::Arranger, size) {
                        let rr = self.section_name_rect(s, lane);
                        cx.request(Self::rename_section(
                            s,
                            Rect::new(rr.x.max(lane.x), rr.y, rr.w.max(120.0), rr.h),
                        ));
                    }
                } else {
                    self.global_drag = Some(GlobalDrag::Section {
                        section: s.clone(),
                        part,
                        grab: self.time_at(pos.x),
                        origin: pos,
                        moved: false,
                        content: part == SectionPart::Body && !mods.shift,
                        to: None,
                        copy: false,
                    });
                }
            }
            GlobalHit::Empty(GlobalLane::Signature, t) if clicks >= 2 => {
                let bar = p.timeline.meter.bar_at(t);
                let x = self.x_of(p.timeline.meter.bar_start(bar));
                cx.request(Self::signature_request(
                    bar,
                    p.timeline.meter.signature_of_bar(bar),
                    Rect::new(x, pos.y - 10.0, 80.0, 20.0),
                ));
            }
            GlobalHit::Signature(bar) => {
                if clicks >= 2 {
                    let x = self.x_of(p.timeline.meter.bar_start(bar));
                    cx.request(Self::signature_request(
                        bar,
                        p.timeline.meter.signature_of_bar(bar),
                        Rect::new(x, pos.y - 10.0, 80.0, 20.0),
                    ));
                } else {
                    cx.request(Self::signature_menu(model, bar, true, pos));
                }
            }
            GlobalHit::Empty(GlobalLane::Tempo, t) if clicks >= 2 => {
                cx.emit(Action::AddTempoPoint(snap(t)));
            }
            GlobalHit::Tempo(index) => {
                let Some(pt) = p.timeline.tempo.points().get(index).copied() else {
                    return true;
                };
                if clicks >= 2 {
                    let c = Point::new(self.x_of(pt.position), pos.y);
                    cx.request(Self::tempo_request(
                        index,
                        pt.position,
                        pt.bpm,
                        Rect::new(c.x, c.y - 10.0, 80.0, 20.0),
                    ));
                } else {
                    self.global_drag = Some(GlobalDrag::Tempo {
                        index,
                        origin: pos,
                        position: pt.position,
                        bpm: pt.bpm,
                        axis: None,
                    });
                }
            }
            GlobalHit::Empty(GlobalLane::Key | GlobalLane::Chords, _)
            | GlobalHit::Key(_)
            | GlobalHit::Chord(..) => self.harmony_press(hit, pos, clicks, mods, size, model, cx),
            GlobalHit::Lyric(i) => {
                let Some(l) = p.lyrics.get(i) else {
                    return true;
                };
                if clicks >= 2 {
                    let x = self.x_of(l.start);
                    let rect = Rect::new(x, pos.y - 10.0, (self.x_of(l.end) - x).max(220.0), 20.0);
                    cx.request(HostRequest::TextInput {
                        at: rect,
                        initial: l.text.clone(),
                        commit: Box::new(move |text| {
                            let t = text.trim();
                            Some(Action::EditLyric {
                                index: i,
                                text: (!t.is_empty()).then(|| t.to_string()),
                            })
                        }),
                    });
                } else {
                    cx.emit(Action::Transport(TransportAction::Locate(l.start)));
                }
            }
            GlobalHit::Empty(..) => {}
        }
        cx.redraw();
        true
    }

    /// Drags in the global lanes (false: none running).
    pub(crate) fn global_drag_move(
        &mut self,
        pos: Point,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        if self.video_drag.is_some() {
            return self.video_drag_move(pos, mods, model, cx);
        }
        let Some(mut drag) = self.global_drag.take() else {
            return false;
        };
        let t_at = self.time_at(pos.x).max(MusicalTime::ZERO);
        match &mut drag {
            GlobalDrag::Marker {
                marker,
                origin,
                moved,
            } => {
                if *moved || pos.distance(*origin) >= DRAG_THRESHOLD {
                    *moved = true;
                    marker.position = self.snap(t_at, model, mods);
                    cx.set_cursor(Cursor::Grabbing);
                }
            }
            GlobalDrag::NewSection { to, .. } => *to = self.snap(t_at, model, mods),
            GlobalDrag::NewChord { .. } | GlobalDrag::Chord { .. } => {
                self.chord_drag(&mut drag, pos, mods, model, cx);
            }
            GlobalDrag::Section {
                section,
                part,
                grab,
                origin,
                moved,
                content,
                to,
                copy,
            } => {
                if *moved || pos.distance(*origin) >= DRAG_THRESHOLD {
                    *moved = true;
                    let Some(orig) = model.project().sections.iter().find(|s| s.id == section.id)
                    else {
                        return true;
                    };
                    if *content {
                        *copy = mods.ctrl;
                        let drop = self.section_drop(model, t_at);
                        // Onto itself: nothing to move (a copy may go anywhere).
                        *to = (*copy || drop < orig.start || drop > orig.end).then_some(drop);
                        cx.set_cursor(Cursor::Grabbing);
                    }
                    match part {
                        SectionPart::Body => {
                            let len = orig.end - orig.start;
                            let start = self.snap(
                                (orig.start + (self.time_at(pos.x) - *grab)).max(MusicalTime::ZERO),
                                model,
                                mods,
                            );
                            section.start = start;
                            section.end = start + len;
                        }
                        SectionPart::Start => {
                            section.start = self
                                .snap(t_at, model, mods)
                                .min(orig.end - MusicalTime::from_ticks(1));
                        }
                        SectionPart::End => {
                            section.end = self
                                .snap(t_at, model, mods)
                                .max(orig.start + MusicalTime::from_ticks(1));
                        }
                    }
                }
            }
            GlobalDrag::Tempo {
                index,
                origin,
                position,
                bpm,
                axis,
            } => {
                let Some(pt) = model.project().timeline.tempo.points().get(*index).copied() else {
                    return true;
                };
                let (dx, dy) = (pos.x - origin.x, pos.y - origin.y);
                if axis.is_none() && (dx.abs() >= DRAG_THRESHOLD || dy.abs() >= DRAG_THRESHOLD) {
                    // The first move decides; the first point never moves.
                    *axis = Some(dy.abs() >= dx.abs() || *index == 0);
                }
                match *axis {
                    Some(true) => {
                        if let Some(lane) = self.lane_rect(GlobalLane::Tempo, size) {
                            let range = Self::tempo_range(model);
                            let per_px = (range.1 - range.0) / (lane.h - 8.0).max(1.0) as f64;
                            let fine = if mods.fine() { 0.1 } else { 1.0 };
                            let v = pt.bpm - dy as f64 * per_px * fine;
                            *bpm = if mods.fine() {
                                (v * 10.0).round() / 10.0
                            } else {
                                v.round()
                            }
                            .clamp(TempoMap::MIN_BPM, TempoMap::MAX_BPM);
                        }
                    }
                    Some(false) => *position = self.snap(t_at, model, mods),
                    None => {}
                }
                cx.set_cursor(match *axis {
                    Some(true) => Cursor::ResizeVertical,
                    Some(false) => Cursor::ResizeHorizontal,
                    None => Cursor::Pointer,
                });
            }
        }
        self.global_drag = Some(drag);
        cx.redraw();
        true
    }

    pub(crate) fn global_release(
        &mut self,
        model: &Session,
        size: Size,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        if self.video_drag.is_some() {
            return self.video_release(model, cx);
        }
        let Some(drag) = self.global_drag.take() else {
            return false;
        };
        match drag {
            d @ (GlobalDrag::NewChord { .. } | GlobalDrag::Chord { .. }) => {
                self.chord_release(d, size, model, cx);
            }
            GlobalDrag::Marker { marker, moved, .. } => {
                if moved {
                    cx.emit(Action::Edit(Command::UpdateMarker { marker }));
                } else {
                    cx.emit(Action::Transport(TransportAction::Locate(marker.position)));
                }
            }
            GlobalDrag::NewSection { anchor, to } => {
                if to != anchor {
                    cx.emit(Action::AddSection {
                        start: anchor.min(to),
                        end: anchor.max(to),
                    });
                } else {
                    cx.emit(Action::Transport(TransportAction::Locate(anchor)));
                }
            }
            GlobalDrag::Section {
                section,
                moved,
                content,
                to,
                copy,
                ..
            } => {
                if !moved {
                    cx.emit(Action::Transport(TransportAction::Locate(section.start)));
                } else if !content {
                    cx.emit(Action::Edit(Command::UpdateSection { section }));
                } else if let Some(to) = to {
                    cx.emit(Action::MoveSection {
                        section: section.id,
                        to,
                        copy,
                    });
                }
            }
            GlobalDrag::Tempo {
                index,
                position,
                bpm,
                axis,
                ..
            } => {
                let unchanged = model
                    .project()
                    .timeline
                    .tempo
                    .points()
                    .get(index)
                    .is_some_and(|p| p.position == position && p.bpm == bpm);
                if axis.is_some() && !unchanged {
                    cx.emit(Action::SetTempoPoint {
                        index,
                        position,
                        bpm,
                    });
                }
            }
        }
        cx.set_cursor(Cursor::Default);
        cx.redraw();
        true
    }

    // --- menus and text input -----------------------------------------------------------

    pub(crate) fn global_menu(
        &self,
        hit: GlobalHit,
        model: &Session,
        pos: Point,
    ) -> HostRequest<Action> {
        let p = model.project();
        match hit {
            GlobalHit::Label(lane) => Self::lanes_menu(model, lane, pos),
            GlobalHit::Video(clip) => self.video_menu(clip, model, pos),
            GlobalHit::Marker(id) => {
                let m = p.markers.iter().find(|m| m.id == id);
                let mut items = Vec::new();
                if let Some(m) = m {
                    items.push(MenuItem::disabled(m.name.clone()));
                    items.push(MenuItem::new(
                        "Go to Marker",
                        Action::Transport(TransportAction::Locate(m.position)),
                    ));
                }
                items.push(MenuItem::new(
                    "Delete Marker",
                    Action::Edit(Command::RemoveMarker { marker: id }),
                ));
                HostRequest::ContextMenu { at: pos, items }
            }
            GlobalHit::Section(id, _) => {
                let Some(s) = p.sections.iter().find(|s| s.id == id) else {
                    return HostRequest::ContextMenu {
                        at: pos,
                        items: Vec::new(),
                    };
                };
                let range = MusicalRange::new(s.start, s.end);
                let mut items = vec![
                    MenuItem::disabled(s.name.clone()),
                    MenuItem::new(
                        "Select Section Range",
                        Action::SetEditRange(Some(faderframe_session::EditRange::new(
                            s.start, s.end,
                        ))),
                    ),
                    MenuItem::new(
                        "Loop Section",
                        Action::Transport(TransportAction::SetLoop(range)),
                    ),
                    MenuItem::new(
                        "Go to Section",
                        Action::Transport(TransportAction::Locate(s.start)),
                    ),
                    MenuItem::new("Colour…", Action::PickColor(ColorTarget::Section(id)))
                        .separated(),
                ];
                for (i, c) in TrackColor::PALETTE.iter().enumerate() {
                    items.push(
                        MenuItem::new(
                            format!("Colour {}", i + 1),
                            Action::Edit(Command::UpdateSection {
                                section: Section {
                                    color: *c,
                                    ..s.clone()
                                },
                            }),
                        )
                        .checked(s.color == *c),
                    );
                }
                let i = p.sections.iter().position(|x| x.id == id).unwrap_or(0);
                let earlier = MenuItem::new(
                    "Move Earlier",
                    Action::SwapSection {
                        section: id,
                        later: false,
                    },
                )
                .separated();
                let later = MenuItem::new(
                    "Move Later",
                    Action::SwapSection {
                        section: id,
                        later: true,
                    },
                );
                items.push(if i == 0 {
                    MenuItem::disabled("Move Earlier").separated()
                } else {
                    earlier
                });
                items.push(if i + 1 >= p.sections.len() {
                    MenuItem::disabled("Move Later")
                } else {
                    later
                });
                items.push(MenuItem::new(
                    "Duplicate Section",
                    Action::DuplicateSection(id),
                ));
                items.push(
                    MenuItem::new(
                        "Delete Section with Content",
                        Action::DeleteSectionContent(id),
                    )
                    .separated(),
                );
                items.push(MenuItem::new(
                    "Remove Section (Keep Content)",
                    Action::Edit(Command::RemoveSection { section: id }),
                ));
                HostRequest::ContextMenu { at: pos, items }
            }
            GlobalHit::Signature(bar) => Self::signature_menu(model, bar, true, pos),
            GlobalHit::Empty(GlobalLane::Signature, t) => {
                Self::signature_menu(model, p.timeline.meter.bar_at(t), false, pos)
            }
            GlobalHit::Tempo(index) => {
                let pt = p.timeline.tempo.points().get(index).copied();
                let ramp = pt.is_some_and(|p| p.curve == TempoCurve::Linear);
                let mut items = Vec::new();
                if let Some(pt) = pt {
                    items.push(MenuItem::disabled(format!("{:.2} BPM", pt.bpm)));
                }
                items.push(
                    MenuItem::new(
                        "Hold Until Next",
                        Action::SetTempoRamp { index, ramp: false },
                    )
                    .checked(!ramp),
                );
                items.push(
                    MenuItem::new("Ramp to Next", Action::SetTempoRamp { index, ramp: true })
                        .checked(ramp),
                );
                if index > 0 {
                    items.push(
                        MenuItem::new("Delete Tempo Change", Action::RemoveTempoPoint(index))
                            .separated(),
                    );
                }
                HostRequest::ContextMenu { at: pos, items }
            }
            GlobalHit::Empty(GlobalLane::Tempo, t) => HostRequest::ContextMenu {
                at: pos,
                items: vec![MenuItem::new(
                    "Add Tempo Change Here",
                    Action::AddTempoPoint(self.snap(t, model, Modifiers::NONE)),
                )],
            },
            GlobalHit::Empty(GlobalLane::Markers, t) => HostRequest::ContextMenu {
                at: pos,
                items: vec![
                    MenuItem::new(
                        "Add Marker Here",
                        Action::AddMarker(self.snap(t, model, Modifiers::NONE)),
                    ),
                    MenuItem::new(
                        "Add Marker at Playhead",
                        Action::AddMarker(model.playhead()),
                    ),
                ],
            },
            GlobalHit::Key(i) => match p.keys.get(i) {
                Some(k) => Self::key_menu(model, k.at, true, pos),
                None => Self::lanes_menu(model, GlobalLane::Key, pos),
            },
            GlobalHit::Empty(GlobalLane::Key, t) => {
                let at = if p.keys.is_empty() {
                    MusicalTime::ZERO
                } else {
                    p.timeline.meter.bar_start(p.timeline.meter.bar_at(t))
                };
                Self::key_menu(model, at, false, pos)
            }
            GlobalHit::Chord(i, _) => Self::chord_menu(model, i, pos),
            GlobalHit::Lyric(i) => HostRequest::ContextMenu {
                at: pos,
                items: vec![
                    MenuItem::new(
                        "Delete Line",
                        Action::EditLyric {
                            index: i,
                            text: None,
                        },
                    ),
                    MenuItem::new("Export Lyrics (LRC and SRT)", Action::ExportLyrics).separated(),
                    MenuItem::new(
                        "Delete All Lyrics",
                        Action::Edit(Command::SetLyrics { lyrics: Vec::new() }),
                    )
                    .separated(),
                ],
            },
            GlobalHit::Empty(GlobalLane::Chords, _) => Self::chords_lane_menu(pos),
            GlobalHit::Empty(lane, _) => Self::lanes_menu(model, lane, pos),
        }
    }

    /// Show or hide lanes.
    fn lanes_menu(model: &Session, _lane: GlobalLane, at: Point) -> HostRequest<Action> {
        let lanes = model.editor.lanes;
        let items = GlobalLane::ALL
            .iter()
            .map(|l| {
                let on = lanes.shows(*l);
                MenuItem::new(
                    format!("{} Lane", l.title()),
                    Action::ShowGlobalLane(*l, !on),
                )
                .checked(on)
            })
            .collect();
        HostRequest::ContextMenu { at, items }
    }

    fn signature_menu(model: &Session, bar: i32, existing: bool, at: Point) -> HostRequest<Action> {
        let current = model.project().timeline.meter.signature_of_bar(bar);
        let mut items = vec![MenuItem::disabled(format!("Bar {}", bar + 1))];
        for (n, d) in SIGNATURES {
            let Some(sig) = TimeSignature::new(n, d) else {
                continue;
            };
            items.push(
                MenuItem::new(
                    format!("{n}/{d}"),
                    Action::Edit(Command::SetTimeSignature {
                        bar,
                        signature: Some(sig),
                    }),
                )
                .checked(existing && current == sig),
            );
        }
        if existing && bar > 0 {
            items.push(
                MenuItem::new(
                    "Delete Signature Change",
                    Action::Edit(Command::SetTimeSignature {
                        bar,
                        signature: None,
                    }),
                )
                .separated(),
            );
        }
        HostRequest::ContextMenu { at, items }
    }

    fn rename_marker(m: &Marker, at: Rect) -> HostRequest<Action> {
        let marker = m.clone();
        HostRequest::TextInput {
            at,
            initial: m.name.clone(),
            commit: Box::new(move |text| {
                let name = text.trim();
                (!name.is_empty()).then(|| {
                    Action::Edit(Command::UpdateMarker {
                        marker: Marker {
                            name: name.to_string(),
                            ..marker.clone()
                        },
                    })
                })
            }),
        }
    }

    fn rename_section(s: &Section, at: Rect) -> HostRequest<Action> {
        let section = s.clone();
        HostRequest::TextInput {
            at,
            initial: s.name.clone(),
            commit: Box::new(move |text| {
                let name = text.trim();
                (!name.is_empty()).then(|| {
                    Action::Edit(Command::UpdateSection {
                        section: Section {
                            name: name.to_string(),
                            ..section.clone()
                        },
                    })
                })
            }),
        }
    }

    fn signature_request(bar: i32, current: TimeSignature, at: Rect) -> HostRequest<Action> {
        HostRequest::TextInput {
            at,
            initial: format!("{}/{}", current.numerator, current.denominator),
            commit: Box::new(move |text| {
                TimeSignature::parse(text.trim()).map(|sig| {
                    Action::Edit(Command::SetTimeSignature {
                        bar,
                        signature: Some(sig),
                    })
                })
            }),
        }
    }

    fn tempo_request(
        index: usize,
        position: MusicalTime,
        bpm: f64,
        at: Rect,
    ) -> HostRequest<Action> {
        HostRequest::TextInput {
            at,
            initial: format!("{bpm:.2}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string(),
            commit: Box::new(move |text| {
                let v: f64 = text.trim().trim_end_matches("bpm").trim().parse().ok()?;
                (v.is_finite() && (TempoMap::MIN_BPM..=TempoMap::MAX_BPM).contains(&v)).then_some(
                    Action::SetTempoPoint {
                        index,
                        position,
                        bpm: v,
                    },
                )
            }),
        }
    }

    pub(crate) fn global_tooltip(&self, hit: GlobalHit, model: &Session) -> Option<String> {
        let p = model.project();
        Some(match hit {
            GlobalHit::Label(lane) => format!("{} lane · Click to show or hide lanes", lane.title()),
            GlobalHit::Empty(GlobalLane::Markers, _) => {
                "Double-click to add a marker · Right-click for more".into()
            }
            GlobalHit::Empty(GlobalLane::Arranger, _) => {
                "Drag to add a section (Intro, Verse, Chorus …)".into()
            }
            GlobalHit::Empty(GlobalLane::Signature, _) => {
                "Double-click or right-click to change the time signature from this bar".into()
            }
            GlobalHit::Empty(GlobalLane::Tempo, _) => {
                "Double-click to add a tempo change · Drag points: up/down tempo (Shift: fine), sideways position"
                    .into()
            }
            GlobalHit::Marker(id) => {
                let m = p.markers.iter().find(|m| m.id == id)?;
                format!(
                    "{} · {} · Click to go there · Drag to move · Double-click to rename",
                    m.name,
                    format_bbt(&p.timeline, m.position)
                )
            }
            GlobalHit::Section(id, part) => {
                let s = p.sections.iter().find(|s| s.id == id)?;
                match part {
                    SectionPart::Body => format!(
                        "{} · {} – {} · Drag to move with its content (Ctrl: copy, Shift: the section only) · Double-click to rename · Right-click for more",
                        s.name,
                        format_bbt(&p.timeline, s.start),
                        format_bbt(&p.timeline, s.end)
                    ),
                    SectionPart::Start | SectionPart::End => "Drag to resize the section".into(),
                }
            }
            GlobalHit::Signature(bar) => format!(
                "{} from bar {} · Click to change · Double-click to type",
                {
                    let s = p.timeline.meter.signature_of_bar(bar);
                    format!("{}/{}", s.numerator, s.denominator)
                },
                bar + 1
            ),
            GlobalHit::Tempo(i) => {
                let pt = p.timeline.tempo.points().get(i)?;
                format!(
                    "{:.2} BPM at {} · Drag up/down (Shift: fine) or sideways · Double-click to type · Right-click: ramp, delete",
                    pt.bpm,
                    format_bbt(&p.timeline, pt.position)
                )
            }
            GlobalHit::Key(_)
            | GlobalHit::Chord(..)
            | GlobalHit::Empty(GlobalLane::Key | GlobalLane::Chords, _) => {
                return self.harmony_tooltip(hit, model);
            }
            GlobalHit::Lyric(i) => {
                let l = p.lyrics.get(i)?;
                format!(
                    "“{}” · {} · Click to go there · Double-click to edit · Right-click to delete",
                    l.text,
                    format_bbt(&p.timeline, l.start)
                )
            }
            GlobalHit::Empty(GlobalLane::Lyrics, _) => {
                "Transcribe an audio clip (its menu) to fill this lane".into()
            }
            GlobalHit::Video(clip) => self.video_tooltip(clip, model),
            GlobalHit::Empty(GlobalLane::Video, _) => self.video_tooltip(None, model),
        })
    }
}

fn format_bbt(tl: &faderframe_timeline::Timeline, t: MusicalTime) -> String {
    let b = tl.meter.to_bbt(t);
    format!("{}.{}", b.bar + 1, b.beat + 1)
}
