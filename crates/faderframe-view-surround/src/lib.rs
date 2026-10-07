//! The surround panner (`ViewKind::Surround`): the selected track in the
//! bed it feeds, seen from above. Drag the puck (a stereo track's two
//! channels sit its width apart) or click where it should go; the knobs set
//! its height (formats with top speakers), spread, stereo width and LFE
//! send. Every move is one undo step (`SetTrackSurround` in a gesture) and
//! writes automation like the faders do; automated values are shown as
//! they play. The speakers light up with what the track sends them.

#![forbid(unsafe_code)]

pub mod room;

use faderframe_core::{SurroundPan, SurroundParam, TrackId};
use faderframe_project::{Command, Track};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::controls::{self, KnobLook};
use faderframe_ui_canvas::{
    Align, CanvasView, Color, Cursor, EventCx, Painter, Point, PointerButton, Rect, Size,
    TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 34.0;
const KNOBS_W: f32 = 96.0;
const KNOB: f32 = 40.0;
const KNOB_ROW: f32 = 74.0;
/// Pixels of vertical drag for a knob's whole range.
const KNOB_TRAVEL: f32 = 160.0;
/// The knobs, top to bottom (those that apply).
const KNOBS: [SurroundParam; 4] = [
    SurroundParam::Z,
    SurroundParam::Spread,
    SurroundParam::Width,
    SurroundParam::Lfe,
];

enum Drag {
    /// The puck, grabbed `grab` away from its centre.
    Puck { track: TrackId, grab: Point },
    Knob {
        track: TrackId,
        param: SurroundParam,
        start_y: f32,
        start: f32,
    },
}

pub struct SurroundView {
    theme: Theme,
    drag: Option<Drag>,
}

struct Layout {
    room: Rect,
    knobs: Vec<(SurroundParam, Rect)>,
}

/// A knob's position (0…1) for `v`; the LFE send is off at 0, then
/// −40 … +12 dB.
fn position(param: SurroundParam, v: f32) -> f32 {
    match param {
        SurroundParam::Lfe if v <= faderframe_core::surround::LFE_OFF_DB => 0.0,
        SurroundParam::Lfe => ((v + 40.0) / 52.0).clamp(0.01, 1.0),
        _ => {
            let (lo, hi, _) = param.range();
            ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
        }
    }
}

fn value(param: SurroundParam, pos: f32) -> f32 {
    let pos = pos.clamp(0.0, 1.0);
    match param {
        SurroundParam::Lfe if pos < 0.01 => faderframe_core::surround::LFE_OFF_DB,
        SurroundParam::Lfe => -40.0 + pos * 52.0,
        _ => {
            let (lo, hi, _) = param.range();
            lo + pos * (hi - lo)
        }
    }
}

fn format_value(param: SurroundParam, v: f32) -> String {
    match param {
        SurroundParam::Lfe if v <= faderframe_core::surround::LFE_OFF_DB => "Off".into(),
        SurroundParam::Lfe => format!("{v:+.1} dB"),
        _ => format!("{:.0} %", v * 100.0),
    }
}

fn knob_label(param: SurroundParam) -> &'static str {
    match param {
        SurroundParam::Z => "HEIGHT",
        SurroundParam::Spread => "SPREAD",
        SurroundParam::Width => "WIDTH",
        SurroundParam::Lfe => "LFE",
        SurroundParam::X => "L/R",
        SurroundParam::Y => "F/B",
    }
}

impl SurroundView {
    pub fn new(theme: Theme) -> Self {
        Self { theme, drag: None }
    }

    /// The track shown: the first selected one (in the editors' order).
    pub fn track(model: &Session) -> Option<&Track> {
        let sel = &model.selection.tracks;
        model
            .project()
            .folder_order()
            .into_iter()
            .find(|t| sel.contains(&t.id))
    }

    fn layout(&self, size: Size, model: &Session, t: &Track) -> Layout {
        let area = Rect::new(0.0, HEADER_H, size.w, size.h - HEADER_H).inset(14.0);
        let (left, right) = area.split_right(KNOBS_W);
        let side = left.w.min(left.h).max(40.0);
        let room = Rect::new(
            left.x + (left.w - side) * 0.5,
            left.y + (left.h - side) * 0.5,
            side,
            side,
        );
        let format = model.project().surround_panned(t);
        let mut knobs = Vec::new();
        let mut y = right.y;
        for p in KNOBS {
            if format.is_some_and(|f| p.applies(t.layout, f)) {
                knobs.push((
                    p,
                    Rect::new(right.x + (right.w - KNOB) * 0.5, y + 14.0, KNOB, KNOB),
                ));
                y += KNOB_ROW;
            }
        }
        Layout { room, knobs }
    }

    fn set(cx: &mut EventCx<'_, Action>, track: TrackId, pan: SurroundPan) {
        cx.emit(Action::Edit(Command::SetTrackSurround { track, pan }));
    }
}

impl CanvasView<Session, Action> for SurroundView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.is_animating()
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        let head = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(head, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        let Some(t) = Self::track(model) else {
            p.text(
                "Select a track that feeds a surround bed.",
                head.inset_xy(14.0, 0.0),
                &TextStyle::new(th.fonts.normal, th.ui.text_dim),
            );
            return;
        };
        let Some(format) = model.project().surround_panned(t) else {
            let why = if matches!(t.layout, faderframe_core::ChannelLayout::Surround(_)) {
                format!(
                    "{} is a bed itself: it passes into its destination as it is, or folds down to it.",
                    t.name
                )
            } else {
                format!(
                    "{} does not feed a surround bed. Give its bus or the master a surround format (Channel Format in the track menu) to pan it there.",
                    t.name
                )
            };
            p.text(
                &t.name,
                head.inset_xy(14.0, 0.0),
                &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
            );
            p.text(
                &why,
                Rect::new(14.0, HEADER_H + 14.0, size.w - 28.0, 40.0),
                &TextStyle::new(th.fonts.normal, th.ui.text_dim),
            );
            return;
        };
        let pan = model.shown_surround(t);
        let dest = model
            .project()
            .output_target(t)
            .and_then(|d| model.project().track(d))
            .map_or_else(String::new, |d| d.name.clone());
        let title = format!("{}  →  {} ({})", t.name, dest, format.name());
        p.text(
            &title,
            head.inset_xy(14.0, 0.0),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        p.text(
            &room::format_place(&pan),
            head.inset_xy(14.0, 0.0),
            &TextStyle::new(th.fonts.normal, th.ui.text_dim)
                .family(faderframe_ui_canvas::FontFamily::Mono)
                .align(Align::End),
        );
        let l = self.layout(size, model, t);
        let meter = model.meter(t.id);
        let levels: Vec<f32> = meter.shown().iter().map(|c| c.level_db).collect();
        let c = t.color;
        room::Room {
            format,
            source: t.layout,
            pan,
            levels: &levels,
            puck: Color::rgb8(c.r, c.g, c.b).lighten(0.15),
            compact: false,
        }
        .paint(p, l.room, th);
        let look = KnobLook {
            cap: th.console.pan_cap,
            ring: th.console.panel_label,
        };
        for (param, r) in &l.knobs {
            let v = param.get(&pan);
            controls::engraved(
                p,
                knob_label(*param),
                Rect::new(r.x - 20.0, r.y - 14.0, r.w + 40.0, 12.0),
                th,
                Align::Center,
            );
            controls::knob(p, *r, position(*param, v), false, look, th);
            controls::readout(
                p,
                Rect::new(r.x - 14.0, r.bottom() + 4.0, r.w + 28.0, 14.0),
                &format_value(*param, v),
                th,
            );
        }
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(t) = Self::track(model) else {
            return false;
        };
        if model.project().surround_panned(t).is_none() {
            return false;
        }
        let l = self.layout(size, model, t);
        let pan = model.shown_surround(t);
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                clicks,
                ..
            } => {
                if let Some((param, _)) = l.knobs.iter().find(|(_, r)| r.inset(-6.0).contains(pos))
                {
                    if clicks >= 2 {
                        Self::set(cx, t.id, param.set(pan, param.range().2));
                        return true;
                    }
                    cx.emit(Action::BeginGesture(param.name().into()));
                    self.drag = Some(Drag::Knob {
                        track: t.id,
                        param: *param,
                        start_y: pos.y,
                        start: position(*param, param.get(&pan)),
                    });
                    cx.set_cursor(Cursor::ResizeVertical);
                    return true;
                }
                let floor = room::floor(l.room, false);
                if floor.inset(-10.0).contains(pos) {
                    if clicks >= 2 {
                        // Back to the front centre.
                        Self::set(
                            cx,
                            t.id,
                            SurroundPan {
                                x: 0.0,
                                y: 1.0,
                                ..pan
                            },
                        );
                        return true;
                    }
                    let at = room::to_view(floor, pan.x, pan.y);
                    // A click on one of a stereo track's channels grabs the
                    // pair; elsewhere the puck jumps there.
                    let grab = room::sources(&pan, t.layout)
                        .into_iter()
                        .chain(std::iter::once((pan.x, pan.y)))
                        .map(|(x, y)| room::to_view(floor, x, y))
                        .find(|p| p.distance(pos) < 14.0)
                        .map_or(Point::new(0.0, 0.0), |p| Point::new(at.x - p.x, at.y - p.y));
                    cx.emit(Action::BeginGesture("Surround Pan".into()));
                    let (x, y) = room::from_view(floor, Point::new(pos.x + grab.x, pos.y + grab.y));
                    Self::set(cx, t.id, SurroundPan { x, y, ..pan });
                    self.drag = Some(Drag::Puck { track: t.id, grab });
                    cx.set_cursor(Cursor::Grabbing);
                    return true;
                }
                false
            }
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } => match self.drag {
                Some(Drag::Puck { track, grab }) if track == t.id => {
                    let (x, y) = room::from_view(
                        room::floor(l.room, false),
                        Point::new(pos.x + grab.x, pos.y + grab.y),
                    );
                    Self::set(cx, track, SurroundPan { x, y, ..pan });
                    true
                }
                Some(Drag::Knob {
                    track,
                    param,
                    start_y,
                    start,
                }) if track == t.id => {
                    let fine = if modifiers.shift { 0.2 } else { 1.0 };
                    let at = start + (start_y - pos.y) / KNOB_TRAVEL * fine;
                    Self::set(cx, track, param.set(pan, value(param, at)));
                    true
                }
                _ => false,
            },
            ViewEvent::PointerUp { .. } => {
                if self.drag.take().is_some() {
                    cx.emit(Action::EndGesture);
                    cx.set_cursor(Cursor::Default);
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let t = Self::track(model)?;
        model.project().surround_panned(t)?;
        let l = self.layout(size, model, t);
        if room::floor(l.room, false).inset(-10.0).contains(pos) {
            return Some(
                "Drag the puck, or click where the track should sit · double-click: front centre"
                    .into(),
            );
        }
        l.knobs.iter().find(|(_, r)| r.contains(pos)).map(|(p, _)| {
            format!(
                "{} · drag up/down (Shift: fine) · double-click: default",
                p.name()
            )
        })
    }

    fn min_size(&self) -> Size {
        Size::new(360.0, 300.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_ui_canvas::{HostRequest, Modifiers, RecordingPainter};

    fn run(view: &mut SurroundView, ev: ViewEvent, size: Size, s: &Session) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut requests: Vec<HostRequest<Action>> = Vec::new();
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(&ev, size, s, &mut cx);
        actions
    }

    fn down(pos: Point, clicks: u32) -> ViewEvent {
        ViewEvent::PointerDown {
            pos,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks,
        }
    }

    /// A click in the room places the selected track there (one gesture),
    /// a double-click on a knob resets it; tracks not feeding a bed are
    /// left alone.
    #[test]
    fn a_click_places_the_track_in_the_room() {
        use faderframe_core::{ChannelLayout, SurroundFormat};
        let mut s = Session::demo(faderframe_engine::EngineConfig::default()).unwrap();
        let pad = s
            .project()
            .tracks
            .iter()
            .find(|t| t.name == "Pad")
            .unwrap()
            .id;
        s.dispatch(Action::SelectTracks {
            tracks: vec![pad],
            mode: faderframe_session::SelectMode::Replace,
        })
        .unwrap();
        let theme = Theme::default();
        let mut view = SurroundView::new(theme.clone());
        let size = Size::new(700.0, 500.0);
        view.paint(&mut RecordingPainter::new(), size, &s, &theme);
        assert!(run(&mut view, down(Point::new(300.0, 250.0), 1), size, &s).is_empty());
        let master = s.project().master_id().unwrap();
        s.dispatch(Action::Edit(Command::SetTrackLayout {
            track: master,
            layout: ChannelLayout::Surround(SurroundFormat::S714),
        }))
        .unwrap();
        view.paint(&mut RecordingPainter::new(), size, &s, &theme);
        let t = s.project().track(pad).unwrap();
        let l = view.layout(size, &s, t);
        // Stereo into 7.1.4: height, spread, width and LFE.
        assert_eq!(l.knobs.len(), 4);
        let floor = room::floor(l.room, false);
        let target = room::to_view(floor, -0.5, -1.0);
        let actions = run(&mut view, down(target, 1), size, &s);
        match &actions[..] {
            [
                Action::BeginGesture(_),
                Action::Edit(Command::SetTrackSurround { track, pan }),
            ] => {
                assert_eq!(*track, pad);
                assert!((pan.x + 0.5).abs() < 1e-4 && (pan.y + 1.0).abs() < 1e-4);
            }
            other => panic!("{other:?}"),
        }
        let up = ViewEvent::PointerUp {
            pos: target,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        };
        assert!(matches!(
            run(&mut view, up, size, &s)[..],
            [Action::EndGesture]
        ));
        let (param, knob) = l.knobs[1];
        assert_eq!(param, SurroundParam::Spread);
        let actions = run(&mut view, down(knob.center(), 2), size, &s);
        assert!(
            matches!(&actions[..], [Action::Edit(Command::SetTrackSurround { pan, .. })] if pan.spread == 0.0)
        );
    }

    #[test]
    fn knob_positions_round_trip() {
        for param in KNOBS {
            for pos in [0.0, 0.25, 0.5, 1.0] {
                let v = value(param, pos);
                assert!((position(param, v) - pos).abs() < 1e-4, "{param:?} {pos}");
            }
        }
        assert_eq!(
            format_value(SurroundParam::Lfe, value(SurroundParam::Lfe, 0.0)),
            "Off"
        );
        assert_eq!(format_value(SurroundParam::Spread, 0.5), "50 %");
    }
}
