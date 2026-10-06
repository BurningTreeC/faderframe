//! Clip effects: the devices on one audio clip (rendered offline into the
//! audio it plays, see `faderframe_session::clip_fx`), a card each with
//! its parameters as bars — drag across one to set it (Shift: finely),
//! double-click to reset it. "+ Add Effect" offers the audio effects the
//! session knows (built-ins first); a card's header bypasses, moves or
//! removes its device. The chain renders once it rests; the header says
//! when it is rendering.

#![forbid(unsafe_code)]

use faderframe_core::ClipId;
use faderframe_plugin_host::{ParameterInfo, ParameterUnit};
use faderframe_project::PluginSlot;
use faderframe_session::clip_fx::ClipFxOp;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, Cursor, EventCx, HostRequest, MenuItem, Modifiers, Paint, Painter, Point,
    PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 34.0;
const CARD_W: f32 = 230.0;
const GAP: f32 = 10.0;
const TITLE_H: f32 = 28.0;
const ROW_H: f32 = 30.0;

/// Where a press landed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Add,
    Clear,
    Bypass(usize),
    Left(usize),
    Right(usize),
    Remove(usize),
    Parameter(usize, usize),
}

/// A parameter drag: card, parameter, where it began, the value then.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    card: usize,
    parameter: usize,
    from: f32,
    start: f64,
}

pub struct ClipFxView {
    theme: Theme,
    scroll_x: f32,
    scroll_y: f32,
    drag: Option<Drag>,
    hover: Option<Hit>,
}

/// A parameter's position along its bar (0…1; Hz parameters in octaves).
fn norm(p: &ParameterInfo, v: f64) -> f64 {
    let v = p.clamp(v);
    if p.unit == ParameterUnit::Hertz && p.min > 0.0 && p.max > p.min {
        (v / p.min).ln() / (p.max / p.min).ln()
    } else if p.max > p.min {
        (v - p.min) / (p.max - p.min)
    } else {
        0.0
    }
}

fn denorm(p: &ParameterInfo, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let v = if p.unit == ParameterUnit::Hertz && p.min > 0.0 && p.max > p.min {
        p.min * (p.max / p.min).powf(t)
    } else {
        p.min + t * (p.max - p.min)
    };
    if p.stepped { v.round() } else { v }
}

/// A value as it reads.
pub fn value_text(p: &ParameterInfo, v: f64) -> String {
    match p.unit {
        ParameterUnit::Decibels => format!("{v:+.1} dB"),
        ParameterUnit::Milliseconds => {
            if v >= 1_000.0 {
                format!("{:.2} s", v / 1_000.0)
            } else {
                format!("{v:.1} ms")
            }
        }
        ParameterUnit::Hertz => {
            if v >= 1_000.0 {
                format!("{:.2} kHz", v / 1_000.0)
            } else {
                format!("{v:.0} Hz")
            }
        }
        ParameterUnit::Percent => format!("{:.0}%", v * 100.0),
        ParameterUnit::Samples => format!("{v:.0} smp"),
        ParameterUnit::None if p.stepped => format!("{v:.0}"),
        ParameterUnit::None => format!("{v:.2}"),
    }
}

/// A slot's value of a parameter (as saved, else the default).
fn value_of(slot: &PluginSlot, p: &ParameterInfo) -> f64 {
    slot.parameters
        .iter()
        .find(|s| s.id == p.id)
        .map_or(p.default, |s| s.value)
}

impl ClipFxView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll_x: 0.0,
            scroll_y: 0.0,
            drag: None,
            hover: None,
        }
    }

    fn clip(model: &Session) -> Option<(ClipId, String, Vec<PluginSlot>)> {
        let id = model.clip_fx_clip()?;
        let name = model.project().clip(id)?.name.clone();
        Some((id, name, model.clip_fx_chain(id)))
    }

    fn header_buttons(&self) -> [(Hit, Rect, &'static str); 2] {
        [
            (
                Hit::Add,
                Rect::new(8.0, 5.0, 104.0, HEADER_H - 10.0),
                "+ Add Effect",
            ),
            (
                Hit::Clear,
                Rect::new(118.0, 5.0, 86.0, HEADER_H - 10.0),
                "Remove All",
            ),
        ]
    }

    fn card(&self, i: usize, size: Size) -> Rect {
        Rect::new(
            GAP + i as f32 * (CARD_W + GAP) - self.scroll_x,
            HEADER_H + GAP,
            CARD_W,
            (size.h - HEADER_H - 2.0 * GAP).max(TITLE_H),
        )
    }

    /// A card's header controls.
    fn card_buttons(&self, i: usize, size: Size) -> [(Hit, Rect); 4] {
        let c = self.card(i, size);
        let y = c.y + 4.0;
        let h = TITLE_H - 8.0;
        [
            (Hit::Bypass(i), Rect::new(c.right() - 98.0, y, 34.0, h)),
            (Hit::Left(i), Rect::new(c.right() - 60.0, y, 16.0, h)),
            (Hit::Right(i), Rect::new(c.right() - 42.0, y, 16.0, h)),
            (Hit::Remove(i), Rect::new(c.right() - 22.0, y, 18.0, h)),
        ]
    }

    fn row(&self, card: usize, k: usize, size: Size) -> Rect {
        let c = self.card(card, size);
        Rect::new(
            c.x + 8.0,
            c.y + TITLE_H + 6.0 + k as f32 * ROW_H - self.scroll_y,
            c.w - 16.0,
            ROW_H - 4.0,
        )
    }

    /// What is under `pos`.
    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        if pos.y < HEADER_H {
            return self
                .header_buttons()
                .into_iter()
                .find(|(_, r, _)| r.contains(pos))
                .map(|(h, ..)| h);
        }
        let (_, _, chain) = Self::clip(model)?;
        for (i, slot) in chain.iter().enumerate() {
            if !self.card(i, size).contains(pos) {
                continue;
            }
            if let Some((h, _)) = self
                .card_buttons(i, size)
                .into_iter()
                .find(|(_, r)| r.contains(pos))
            {
                return Some(h);
            }
            let infos = model.clip_fx_known_parameters(&slot.plugin)?;
            let body_top = self.card(i, size).y + TITLE_H;
            if pos.y < body_top {
                return None;
            }
            for k in 0..infos.len() {
                if self.row(i, k, size).contains(pos) {
                    return Some(Hit::Parameter(i, k));
                }
            }
        }
        None
    }

    fn content_h(model: &Session) -> f32 {
        Self::clip(model).map_or(0.0, |(_, _, chain)| {
            chain
                .iter()
                .map(|s| {
                    model
                        .clip_fx_known_parameters(&s.plugin)
                        .map_or(0, <[_]>::len)
                })
                .max()
                .unwrap_or(0) as f32
                * ROW_H
                + TITLE_H
                + 2.0 * GAP
        })
    }

    fn add_menu(model: &Session, clip: ClipId, at: Point) -> HostRequest<Action> {
        let mut plugins: Vec<_> = model
            .available_plugins()
            .into_iter()
            .filter(|p| {
                !p.instrument
                    && !p.midi_effect
                    && p.audio_outputs > 0
                    && !p.plugin.is_container()
                    && p.plugin.id != faderframe_core::builtin::TUNER
            })
            .collect();
        plugins.sort_by_key(|p| {
            (
                p.plugin.format != faderframe_project::PluginFormat::Builtin,
                p.plugin.name.clone(),
            )
        });
        let mut items = Vec::new();
        let mut builtin = true;
        for p in plugins {
            let is_builtin = p.plugin.format == faderframe_project::PluginFormat::Builtin;
            let label = if is_builtin {
                p.plugin.name.clone()
            } else {
                format!("{} ({})", p.plugin.name, p.vendor)
            };
            let item = MenuItem::new(
                label,
                Action::ClipEffects {
                    clip,
                    op: ClipFxOp::Add(p.plugin),
                },
            );
            items.push(if builtin && !is_builtin {
                item.separated()
            } else {
                item
            });
            builtin = is_builtin;
        }
        HostRequest::ContextMenu { at, items }
    }

    fn paint_card(
        &self,
        p: &mut dyn Painter,
        i: usize,
        slot: &PluginSlot,
        size: Size,
        model: &Session,
    ) {
        let th = &self.theme;
        let c = self.card(i, size);
        p.fill_rounded(c, 6.0, &Paint::Solid(th.ui.surface));
        p.stroke_rounded(c, 6.0, 1.0, th.ui.border);
        let title = Rect::new(c.x, c.y, c.w, TITLE_H);
        p.fill_rounded(title, 6.0, &Paint::Solid(th.ui.surface_alt));
        p.text(
            &format!(
                "{}  {}",
                i + 1,
                slot.plugin
                    .name
                    .strip_prefix("FaderFrame ")
                    .unwrap_or(&slot.plugin.name)
            ),
            Rect::new(c.x + 10.0, c.y, c.w - 112.0, TITLE_H),
            &TextStyle::new(
                th.fonts.small,
                if slot.bypass {
                    th.ui.text_faint
                } else {
                    th.ui.text
                },
            )
            .bold(),
        );
        for (hit, r) in self.card_buttons(i, size) {
            let (label, on) = match hit {
                Hit::Bypass(_) => (if slot.bypass { "Off" } else { "On" }, !slot.bypass),
                Hit::Left(_) => ("◀", false),
                Hit::Right(_) => ("▶", false),
                _ => ("×", false),
            };
            let bg = if on {
                th.ui.accent.with_alpha(0.35)
            } else if self.hover == Some(hit) {
                th.ui.text.with_alpha(0.12)
            } else {
                th.ui.text.with_alpha(0.05)
            };
            p.fill_rounded(r, 3.0, &Paint::Solid(bg));
            p.text(
                label,
                r,
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
        }
        let Some(infos) = model.clip_fx_known_parameters(&slot.plugin) else {
            p.text(
                "…",
                Rect::new(c.x, c.y + TITLE_H, c.w, ROW_H),
                &TextStyle::new(th.fonts.small, th.ui.text_faint).center(),
            );
            return;
        };
        let body = Rect::new(c.x, c.y + TITLE_H, c.w, c.h - TITLE_H);
        p.push_clip(body);
        for (k, info) in infos.iter().enumerate() {
            let r = self.row(i, k, size);
            if r.bottom() < body.y || r.y > body.bottom() {
                continue;
            }
            let v = value_of(slot, info);
            let t = norm(info, v) as f32;
            let lit = self.hover == Some(Hit::Parameter(i, k))
                || self.drag.is_some_and(|d| d.card == i && d.parameter == k);
            p.fill_rounded(r, 3.0, &Paint::Solid(th.ui.background.with_alpha(0.6)));
            let bar = Rect::new(r.x, r.bottom() - 5.0, r.w * t, 3.0);
            p.fill(
                bar,
                if slot.bypass {
                    th.ui.text_faint
                } else {
                    th.ui.accent.with_alpha(if lit { 1.0 } else { 0.75 })
                },
            );
            let style = TextStyle::new(
                th.fonts.small,
                if lit { th.ui.text } else { th.ui.text_dim },
            );
            p.text(
                &info.name,
                Rect::new(r.x + 6.0, r.y, r.w * 0.6, r.h - 4.0),
                &style,
            );
            p.text(
                &value_text(info, v),
                Rect::new(r.x + r.w * 0.4, r.y, r.w * 0.6 - 6.0, r.h - 4.0),
                &style.right(),
            );
        }
        p.pop_clip();
    }

    fn set(
        &self,
        model: &Session,
        clip: ClipId,
        card: usize,
        parameter: usize,
        value: f64,
        cx: &mut EventCx<'_, Action>,
    ) {
        let chain = model.clip_fx_chain(clip);
        let Some(info) = chain
            .get(card)
            .and_then(|s| model.clip_fx_known_parameters(&s.plugin))
            .and_then(|i| i.get(parameter))
        else {
            return;
        };
        cx.emit(Action::ClipEffects {
            clip,
            op: ClipFxOp::SetParameter {
                index: card,
                parameter: info.id,
                value: info.clamp(value),
            },
        });
    }

    fn press(
        &mut self,
        pos: Point,
        modifiers: Modifiers,
        clicks: u32,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some((clip, _, chain)) = Self::clip(model) else {
            return false;
        };
        let _ = modifiers;
        let Some(hit) = self.hit(pos, size, model) else {
            return false;
        };
        let op = |op| Action::ClipEffects { clip, op };
        match hit {
            Hit::Add => cx.request(Self::add_menu(model, clip, pos)),
            Hit::Clear => cx.emit(op(ClipFxOp::Clear)),
            Hit::Bypass(i) => {
                let on = chain.get(i).is_some_and(|s| s.bypass);
                cx.emit(op(ClipFxOp::Bypass(i, !on)));
            }
            Hit::Left(i) if i > 0 => cx.emit(op(ClipFxOp::Move { from: i, to: i - 1 })),
            Hit::Right(i) if i + 1 < chain.len() => {
                cx.emit(op(ClipFxOp::Move { from: i, to: i + 1 }))
            }
            Hit::Remove(i) => cx.emit(op(ClipFxOp::Remove(i))),
            Hit::Parameter(card, k) => {
                let Some(info) = chain
                    .get(card)
                    .and_then(|s| model.clip_fx_known_parameters(&s.plugin))
                    .and_then(|i| i.get(k))
                else {
                    return true;
                };
                let v = value_of(&chain[card], info);
                if clicks >= 2 {
                    self.set(model, clip, card, k, info.default, cx);
                    return true;
                }
                self.drag = Some(Drag {
                    card,
                    parameter: k,
                    from: pos.x,
                    start: norm(info, v),
                });
                cx.set_cursor(Cursor::ResizeHorizontal);
            }
            _ => {}
        }
        cx.redraw();
        true
    }
}

impl CanvasView<Session, Action> for ClipFxView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, _theme: &Theme) {
        let th = self.theme.clone();
        p.fill(Rect::from_size(size), th.ui.background);
        let header = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(header, th.piano.toolbar);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        let Some((clip, name, chain)) = Self::clip(model) else {
            p.text(
                "Right-click an audio clip and choose Clip Effects",
                Rect::new(0.0, HEADER_H, size.w, size.h - HEADER_H),
                &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
            );
            return;
        };
        for (hit, r, label) in self.header_buttons() {
            let bg = if self.hover == Some(hit) {
                th.piano.button_active
            } else {
                th.piano.button
            };
            p.fill_rounded(r, 3.0, &Paint::Solid(bg));
            p.text(
                label,
                r,
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
        }
        let status = if model.clip_fx_busy(clip) {
            "rendering…".to_string()
        } else if chain.is_empty() {
            "no effects yet".to_string()
        } else {
            format!(
                "{} effect{}, rendered",
                chain.len(),
                if chain.len() == 1 { "" } else { "s" }
            )
        };
        p.text(
            &format!("{name} · {status}"),
            Rect::new(214.0, 0.0, size.w - 224.0, HEADER_H),
            &TextStyle::new(th.fonts.small, th.ui.text_dim).right(),
        );
        if chain.is_empty() {
            p.text(
                "+ Add Effect puts a device on this clip; its audio is rendered through it",
                Rect::new(0.0, HEADER_H, size.w, size.h - HEADER_H),
                &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
            );
            return;
        }
        p.push_clip(Rect::new(0.0, HEADER_H, size.w, size.h - HEADER_H));
        for (i, slot) in chain.iter().enumerate() {
            self.paint_card(p, i, slot, size, model);
        }
        p.pop_clip();
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
            } => self.press(pos, modifiers, clicks, size, model, cx),
            ViewEvent::PointerMove { pos, modifiers, .. } => {
                if let Some(d) = self.drag {
                    let Some((clip, _, chain)) = Self::clip(model) else {
                        return true;
                    };
                    let Some(info) = chain
                        .get(d.card)
                        .and_then(|s| model.clip_fx_known_parameters(&s.plugin))
                        .and_then(|i| i.get(d.parameter))
                    else {
                        return true;
                    };
                    let span = if modifiers.shift { 800.0 } else { 200.0 };
                    let t = d.start + f64::from((pos.x - d.from) / span);
                    let v = denorm(info, t);
                    if (v - value_of(&chain[d.card], info)).abs() > f64::EPSILON {
                        self.set(model, clip, d.card, d.parameter, v, cx);
                    }
                    cx.set_cursor(Cursor::ResizeHorizontal);
                    return true;
                }
                let hover = self.hit(pos, size, model);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                false
            }
            ViewEvent::PointerUp {
                button: PointerButton::Primary,
                ..
            } => self.drag.take().is_some(),
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll {
                dx,
                dy,
                modifiers,
                precise,
                ..
            } => {
                let step = if precise { 1.0 } else { ROW_H };
                if modifiers.shift {
                    self.scroll_x = (self.scroll_x + (dy + dx) * step).max(0.0);
                } else {
                    self.scroll_y = (self.scroll_y + dy * step).max(0.0);
                    self.scroll_x = (self.scroll_x + dx * step).max(0.0);
                }
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.clip_fx_clip().is_some_and(|c| model.clip_fx_busy(c))
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        Some(
            match self.hit(pos, size, model)? {
                Hit::Add => "Put a device on this clip (rendered into its audio)",
                Hit::Clear => "Remove every effect: the clip plays its own audio again",
                Hit::Bypass(_) => "Switch this device off or on",
                Hit::Left(_) | Hit::Right(_) => "Move this device in the chain",
                Hit::Remove(_) => "Remove this device",
                Hit::Parameter(..) => "Drag across to set (Shift: finely) · double-click: default",
            }
            .into(),
        )
    }

    fn min_size(&self) -> Size {
        Size::new(300.0, 160.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        let n = Self::clip(model).map_or(0, |(_, _, c)| c.len());
        match axis {
            ScrollAxis::Horizontal => Some(ScrollInfo {
                content: n as f32 * (CARD_W + GAP) + GAP,
                viewport: size.w,
                offset: self.scroll_x,
                start: 0.0,
                end: 0.0,
            }),
            ScrollAxis::Vertical => Some(ScrollInfo {
                content: Self::content_h(model),
                viewport: size.h - HEADER_H,
                offset: self.scroll_y,
                start: HEADER_H,
                end: 0.0,
            }),
        }
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        match axis {
            ScrollAxis::Horizontal => self.scroll_x = offset.max(0.0),
            ScrollAxis::Vertical => self.scroll_y = offset.max(0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use faderframe_engine::EngineConfig;
    use faderframe_project::{PluginFormat, PluginRef};
    use faderframe_ui_canvas::RecordingPainter;

    fn session() -> (Session, ClipId) {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let pad = s
            .project()
            .tracks
            .iter()
            .find(|t| t.name == "Pad")
            .unwrap()
            .clips[0];
        s.dispatch(Action::OpenClipEffects(pad)).unwrap();
        (s, pad)
    }

    fn send(
        view: &mut ClipFxView,
        ev: ViewEvent,
        size: Size,
        s: &mut Session,
    ) -> Vec<HostRequest<Action>> {
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(&ev, size, s, &mut cx);
        for a in actions {
            s.dispatch(a).unwrap();
        }
        requests
    }

    #[test]
    fn devices_are_added_set_by_dragging_and_removed() {
        let (mut s, clip) = session();
        let size = Size::new(900.0, 420.0);
        let mut view = ClipFxView::new(Theme::default());
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        assert!(p.texts().contains(&"+ Add Effect"));
        // The add menu lists audio effects, no instruments.
        let reqs = send(
            &mut view,
            ViewEvent::PointerDown {
                pos: Point::new(20.0, 15.0),
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
                clicks: 1,
            },
            size,
            &mut s,
        );
        let Some(HostRequest::ContextMenu { items, .. }) = reqs.into_iter().next() else {
            panic!("a menu");
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.contains("Compressor")),
            "{labels:?}"
        );
        assert!(!labels.iter().any(|l| l.contains("Synth")), "{labels:?}");
        // A utility on the clip.
        s.dispatch(Action::ClipEffects {
            clip,
            op: ClipFxOp::Add(PluginRef {
                format: PluginFormat::Builtin,
                id: faderframe_core::builtin::GAIN.into(),
                name: "Utility".into(),
            }),
        })
        .unwrap();
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        assert!(p.texts().iter().any(|t| t.contains("Utility")));
        // Dragging the first parameter (gain) to the right raises it.
        let r = view.row(0, 0, size);
        send(
            &mut view,
            ViewEvent::PointerDown {
                pos: r.center(),
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
                clicks: 1,
            },
            size,
            &mut s,
        );
        send(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(r.center().x + 40.0, r.center().y),
                modifiers: Modifiers::NONE,
                dragging: true,
            },
            size,
            &mut s,
        );
        let chain = s.clip_fx_chain(clip);
        let gain = chain[0]
            .parameters
            .iter()
            .find(|p| p.id.0 == 0)
            .unwrap()
            .value;
        assert!(gain > 0.0, "{gain}");
        // Remove it with its ×.
        let (_, x) = view.card_buttons(0, size)[3];
        send(
            &mut view,
            ViewEvent::PointerUp {
                pos: r.center(),
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
            },
            size,
            &mut s,
        );
        send(
            &mut view,
            ViewEvent::PointerDown {
                pos: x.center(),
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
                clicks: 1,
            },
            size,
            &mut s,
        );
        assert!(s.clip_fx_chain(clip).is_empty());
    }

    #[test]
    fn values_read_in_their_units() {
        let info = |unit, min, max| ParameterInfo {
            id: faderframe_core::ParameterId(0),
            name: "x".into(),
            min,
            max,
            default: min,
            unit,
            automatable: true,
            stepped: false,
        };
        assert_eq!(
            value_text(&info(ParameterUnit::Decibels, -60.0, 12.0), -6.0),
            "-6.0 dB"
        );
        assert_eq!(
            value_text(&info(ParameterUnit::Hertz, 20.0, 20_000.0), 2_500.0),
            "2.50 kHz"
        );
        assert_eq!(
            value_text(&info(ParameterUnit::Percent, 0.0, 1.0), 0.25),
            "25%"
        );
        // Hz bars run in octaves: 632 Hz is half way from 20 Hz to 20 kHz.
        let hz = info(ParameterUnit::Hertz, 20.0, 20_000.0);
        assert!((denorm(&hz, 0.5) - 632.46).abs() < 0.1);
        assert!((norm(&hz, denorm(&hz, 0.3)) - 0.3).abs() < 1e-9);
    }
}
