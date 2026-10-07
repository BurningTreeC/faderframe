//! The Guitar Station's editor: the rig as it would stand on a stage, one
//! section beneath the other -- the pedalboard (the line of pedals, each on
//! its footswitch, with a place to add another), the amplifier's head, the
//! cabinet with its microphones, and the output strip with the DI.
//!
//! Drawn like the Program EQ's hardware panel (its rendered knobs, lamps and
//! light), and played like the kit's controls: drag (Shift: finer), scroll,
//! double-click to type a value, Ctrl-click for the default, right-click for
//! the default, the automation lane or MIDI learn. Pedals are dragged along
//! the line to reorder them; every change is an undoable parameter edit.

pub(crate) mod layout;
mod paint;

use crate::common::Device;
use crate::kit::{format_by_unit, parse_for};
use crate::program_eq::Needle;
use faderframe_automation::AutomationTarget;
use faderframe_core::{ParameterId, PluginInstanceId};
use faderframe_guitar::pedal::Stomp;
use faderframe_plugin_host::ParameterInfo;
use faderframe_plugin_host::devices::guitar::{self as station, MAX_PEDALS, id};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_project::{Command, MappingTarget};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, Cursor, EventCx, HostRequest, MenuItem, Modifiers, Point, PointerButton, Rect,
    Size, Theme, ViewEvent,
};
use layout::{Hot, Look, Target};
use std::cell::Cell;
use std::time::Instant;

pub const PANEL_W: f32 = 1240.0;
pub const HEADER_H: f32 = 34.0;
pub const BOARD_Y: f32 = 0.0;
pub const BOARD_H: f32 = 262.0;
pub const AMP_Y: f32 = BOARD_Y + BOARD_H;
pub const AMP_H: f32 = 252.0;
pub const CAB_Y: f32 = AMP_Y + AMP_H;
pub const CAB_H: f32 = 282.0;
pub const OUT_Y: f32 = CAB_Y + CAB_H;
pub const OUT_H: f32 = 86.0;
pub const PANEL_H: f32 = OUT_Y + OUT_H;
pub const TOTAL_H: f32 = PANEL_H + HEADER_H;

/// Panel pixels of drag for a knob's whole range.
const DRAG_RANGE: f32 = 240.0;
const FINE: f32 = 0.15;

/// How a control's travel maps to its parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Travel {
    Linear,
    /// Logarithmic (the microphones' distances).
    Log,
}

impl Travel {
    fn norm(self, info: &ParameterInfo, v: f64) -> f64 {
        let (lo, hi) = (info.min, info.max);
        if hi <= lo {
            return 0.0;
        }
        match self {
            Travel::Log if lo > 0.0 => (v.max(lo) / lo).ln() / (hi / lo).ln(),
            _ => (v - lo) / (hi - lo),
        }
        .clamp(0.0, 1.0)
    }

    fn value(self, info: &ParameterInfo, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        let (lo, hi) = (info.min, info.max);
        match self {
            Travel::Log if lo > 0.0 => lo * (hi / lo).powf(t),
            _ => lo + (hi - lo) * t,
        }
    }
}

enum Drag {
    /// A knob, slider or treadle moved by its parameter's travel.
    Value {
        id: u32,
        travel: Travel,
        norm: f64,
        last: Point,
        /// Vertical (knobs, sliders, the treadle) or horizontal.
        horizontal: bool,
        /// Panel pixels for the whole range.
        range: f32,
    },
    /// A microphone dragged across the cone (its position) or along its
    /// axis (its distance and angle).
    Mic { mic: usize, side: bool, last: Point },
    /// A pedal carried along the line.
    Pedal {
        from: usize,
        grab: f32,
        x: f32,
        moved: bool,
    },
}

pub struct GuitarView {
    device: Device,
    drag: Option<Drag>,
    hover: Option<Target>,
    needles: Cell<[[Needle; 2]; 2]>,
    last_frame: Cell<Option<Instant>>,
}

impl GuitarView {
    pub fn new(plugin: PluginInstanceId, _theme: &Theme) -> Self {
        Self {
            device: Device::new(plugin),
            drag: None,
            hover: None,
            needles: Cell::new([[Needle::default(); 2]; 2]),
            last_frame: Cell::new(None),
        }
    }

    /// Scale and offset of the panel in the view.
    fn fit(size: Size) -> (f32, f32, f32) {
        let s = (size.w / PANEL_W).min(size.h / TOTAL_H).max(0.01);
        (
            (size.w - PANEL_W * s) / 2.0,
            (size.h - TOTAL_H * s) / 2.0,
            s,
        )
    }

    /// A view point in panel coordinates (the panel's top at 0).
    fn to_panel(size: Size, pos: Point) -> (Point, f32) {
        let (ox, oy, s) = Self::fit(size);
        (Point::new((pos.x - ox) / s, (pos.y - oy) / s - HEADER_H), s)
    }

    /// A panel rectangle in view coordinates.
    fn to_view(size: Size, r: Rect) -> Rect {
        let (ox, oy, s) = Self::fit(size);
        Rect::new(ox + r.x * s, oy + (r.y + HEADER_H) * s, r.w * s, r.h * s)
    }

    fn look(&self, model: &Session) -> Option<(Look, std::sync::Arc<AnalysisTap>)> {
        let tap = self.device.tap(model)?;
        Some((Look::read(&tap), tap))
    }

    fn info(tap: &AnalysisTap, id: u32) -> Option<ParameterInfo> {
        tap.params.infos().get(id as usize).cloned()
    }

    fn value(tap: &AnalysisTap, id: u32) -> f64 {
        f64::from(tap.params.get(id as usize))
    }

    pub(crate) fn text(tap: &AnalysisTap, id: u32, v: f64) -> String {
        station::format(ParameterId(id), v).unwrap_or_else(|| {
            Self::info(tap, id).map_or_else(|| format!("{v:.2}"), |i| format_by_unit(&i, v))
        })
    }

    fn set(&self, model: &Session, cx: &mut EventCx<'_, Action>, id: u32, v: f64) {
        self.device.set(model, cx, ParameterId(id), v);
    }

    fn set_once(&self, model: &Session, cx: &mut EventCx<'_, Action>, id: u32, v: f64) {
        self.device.begin(cx, "Guitar Station");
        self.set(model, cx, id, v);
        self.device.end(cx);
    }

    fn command(&self, model: &Session, id: u32, v: f64) -> Option<Command> {
        let (track, _) = model.plugin_owner(self.device.plugin)?;
        Some(Command::SetPluginParameter {
            track,
            plugin: self.device.plugin,
            parameter: ParameterId(id),
            value: Some(v),
        })
    }

    /// Several parameters as one undo step (`None` when there is nothing to
    /// change).
    fn batch(&self, model: &Session, label: &str, set: Vec<(u32, f64)>) -> Option<Action> {
        let commands: Vec<Command> = set
            .into_iter()
            .filter_map(|(id, v)| self.command(model, id, v))
            .collect();
        (!commands.is_empty()).then(|| {
            Action::Edit(Command::Batch {
                label: label.into(),
                commands,
            })
        })
    }

    /// A choice parameter's values as a menu (with separators where `split`
    /// says a group starts).
    fn choices(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        id: u32,
        at: Point,
        split: impl Fn(usize) -> bool,
    ) -> Option<HostRequest<Action>> {
        let info = Self::info(tap, id)?;
        let now = Self::value(tap, id).round() as usize;
        let n = (info.max - info.min).round().max(0.0) as usize + 1;
        let items = (0..n)
            .filter_map(|i| {
                let a = Action::Edit(self.command(model, id, i as f64)?);
                let item = MenuItem::new(Self::text(tap, id, i as f64), a).checked(i == now);
                Some(if i > 0 && split(i) {
                    item.separated()
                } else {
                    item
                })
            })
            .collect();
        Some(HostRequest::ContextMenu { at, items })
    }

    /// Default, automation, MIDI learn.
    fn param_menu_request(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        id: u32,
        at: Point,
    ) -> Option<HostRequest<Action>> {
        let info = Self::info(tap, id)?;
        let (track, _) = model.plugin_owner(self.device.plugin)?;
        let target = AutomationTarget::PluginParameter {
            plugin: self.device.plugin,
            parameter: ParameterId(id),
        };
        let mut items = Vec::new();
        if let Some(c) = self.command(model, id, info.default) {
            items.push(MenuItem::new(
                format!("Default ({})", Self::text(tap, id, info.default)),
                Action::Edit(c),
            ));
        }
        if info.automatable {
            items.push(
                MenuItem::new("Show Automation", Action::ShowAutomation { track, target })
                    .separated(),
            );
            items.extend(crate::kit::learn_items(
                model,
                MappingTarget::Parameter { track, target },
            ));
        }
        Some(HostRequest::ContextMenu { at, items })
    }

    fn type_value(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        id: u32,
        at: Rect,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(info) = Self::info(tap, id) else {
            return;
        };
        let Some((track, _)) = model.plugin_owner(self.device.plugin) else {
            return;
        };
        let plugin = self.device.plugin;
        cx.request(HostRequest::TextInput {
            at,
            initial: Self::text(tap, id, Self::value(tap, id)),
            commit: Box::new(move |text| {
                let v = parse_for(&info, text)?;
                Some(Action::Edit(Command::SetPluginParameter {
                    track,
                    plugin,
                    parameter: ParameterId(id),
                    value: Some(v),
                }))
            }),
        });
    }

    // --- the line's edits ------------------------------------------------------

    /// A place's twelve parameters.
    fn place(tap: &AnalysisTap, s: usize) -> [f64; id::STRIDE as usize] {
        std::array::from_fn(|k| Self::value(tap, id::slot(s, k as u32)))
    }

    fn defaults(tap: &AnalysisTap, s: usize) -> [f64; id::STRIDE as usize] {
        std::array::from_fn(|k| Self::info(tap, id::slot(s, k as u32)).map_or(0.0, |i| i.default))
    }

    /// Write the places `0..places.len()` as `places` says (only what changes).
    fn write_places(tap: &AnalysisTap, places: &[[f64; id::STRIDE as usize]]) -> Vec<(u32, f64)> {
        let mut set = Vec::new();
        for (s, values) in places.iter().enumerate().take(MAX_PEDALS) {
            for (k, &v) in values.iter().enumerate() {
                let id = id::slot(s, k as u32);
                if Self::value(tap, id) != v {
                    set.push((id, v));
                }
            }
        }
        set
    }

    /// The line's places in order, empty ones after the pedals.
    fn packed(tap: &AnalysisTap, look: &Look) -> Vec<[f64; id::STRIDE as usize]> {
        let mut places: Vec<_> = look
            .pedals
            .iter()
            .map(|p| Self::place(tap, p.slot))
            .collect();
        while places.len() < MAX_PEDALS {
            let s = places.len();
            places.push(Self::defaults(tap, s));
        }
        places
    }

    fn add_pedal(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        look: &Look,
        stomp: Stomp,
    ) -> Option<Action> {
        let mut places = Self::packed(tap, look);
        let n = look.pedals.len();
        if n >= MAX_PEDALS {
            return None;
        }
        places[n] = Self::defaults(tap, n);
        places[n][id::STOMP as usize] = stomp.index() as f64;
        self.batch(model, "Add Pedal", Self::write_places(tap, &places))
    }

    fn remove_pedal(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        look: &Look,
        at: usize,
    ) -> Option<Action> {
        let mut places = Self::packed(tap, look);
        if at >= look.pedals.len() {
            return None;
        }
        places.remove(at);
        let s = places.len();
        places.push(Self::defaults(tap, s));
        for (s, place) in places.iter_mut().enumerate().skip(look.pedals.len() - 1) {
            *place = Self::defaults(tap, s);
        }
        self.batch(model, "Remove Pedal", Self::write_places(tap, &places))
    }

    fn move_pedal(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        look: &Look,
        from: usize,
        to: usize,
    ) -> Option<Action> {
        let n = look.pedals.len();
        if from >= n || to >= n || from == to {
            return None;
        }
        let mut places = Self::packed(tap, look);
        let p = places.remove(from);
        places.insert(to, p);
        self.batch(model, "Move Pedal", Self::write_places(tap, &places))
    }

    /// The stomp menu: change the pedal at `at` (or add one at the end).
    fn stomp_menu(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        look: &Look,
        place: Option<usize>,
        pos: Point,
    ) -> Option<HostRequest<Action>> {
        let current = place.and_then(|i| look.pedals.get(i)).map(|p| p.stomp);
        let mut items = Vec::new();
        let mut family = "";
        for s in Stomp::ALL.into_iter().skip(1) {
            let action = match place {
                Some(i) => {
                    let slot = look.pedals.get(i)?.slot;
                    Action::Edit(self.command(
                        model,
                        id::slot(slot, id::STOMP),
                        s.index() as f64,
                    )?)
                }
                None => self.add_pedal(model, tap, look, s)?,
            };
            let mut item = MenuItem::new(s.name(), action).checked(Some(s) == current);
            if s.family() != family {
                if !family.is_empty() {
                    item = item.separated();
                }
                family = s.family();
            }
            items.push(item);
        }
        if let Some(i) = place {
            if let Some(a) = self.remove_pedal(model, tap, look, i) {
                items.push(MenuItem::new("Remove Pedal", a).separated());
            }
            if i > 0
                && let Some(a) = self.move_pedal(model, tap, look, i, i - 1)
            {
                items.push(MenuItem::new("Move Left", a));
            }
            if let Some(a) = self.move_pedal(model, tap, look, i, i + 1) {
                items.push(MenuItem::new("Move Right", a));
            }
        }
        Some(HostRequest::ContextMenu { at: pos, items })
    }

    fn hit(&self, model: &Session, at: Point) -> Option<(Hot, Look)> {
        let (look, tap) = self.look(model)?;
        let mut hots = look.hots();
        hots.extend(look.mic_hots(&tap));
        let hot = hots.into_iter().rev().find(|h| h.rect.contains(at))?;
        Some((hot, look))
    }
}

impl CanvasView<Session, Action> for GuitarView {
    fn dense(&self) -> bool {
        true
    }

    fn paint(
        &mut self,
        p: &mut dyn faderframe_ui_canvas::Painter,
        size: Size,
        model: &Session,
        theme: &Theme,
    ) {
        p.fill(Rect::from_size(size), theme.ui.background);
        let Some((look, tap)) = self.look(model) else {
            return;
        };
        tap.watch();
        let now = Instant::now();
        let dt = self
            .last_frame
            .replace(Some(now))
            .map_or(0.0, |last| (now - last).as_secs_f32().min(0.25));
        let mut needles = self.needles.get();
        let dragging = match &self.drag {
            Some(Drag::Pedal {
                from,
                x,
                moved: true,
                grab,
            }) => Some((*from, *x - *grab)),
            _ => None,
        };
        let (ox, oy, s) = Self::fit(size);
        p.push_transform(ox, oy + HEADER_H * s, s);
        paint::all(
            p,
            &paint::Scene {
                look: &look,
                tap: &tap,
                hover: self.hover,
                dragging,
                rate: model.sample_rate() as f32,
            },
            &mut needles,
            dt,
        );
        p.pop_transform();
        self.needles.set(needles);
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match ev {
            ViewEvent::PointerDown {
                pos,
                button,
                modifiers,
                clicks,
            } => self.press(*pos, *button, *modifiers, *clicks, size, model, cx),
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } => self.drag_to(*pos, *modifiers, size, model, cx),
            ViewEvent::PointerMove { pos, .. } => {
                let (at, _) = Self::to_panel(size, *pos);
                let hover = self.hit(model, at).map(|(h, _)| h.target);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                cx.set_cursor(match hover {
                    Some(Target::PedalBody { .. }) => Cursor::Grab,
                    Some(Target::MicFront { .. } | Target::MicSide { .. }) => Cursor::Move,
                    Some(_) => Cursor::Pointer,
                    None => Cursor::Default,
                });
                false
            }
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::PointerUp { pos, .. } => self.release(*pos, size, model, cx),
            ViewEvent::Scroll {
                pos, dy, modifiers, ..
            } => {
                let (at, _) = Self::to_panel(size, *pos);
                let Some((hot, _)) = self.hit(model, at) else {
                    return false;
                };
                let Some(tap) = self.device.tap(model) else {
                    return false;
                };
                let up = -dy.signum() as f64;
                let continuous = match hot.target {
                    Target::Knob { id, travel, .. } => Some((id, travel)),
                    Target::Slider { id } | Target::Treadle { id, .. } => {
                        Some((id, Travel::Linear))
                    }
                    _ => None,
                };
                if let Some((id, travel)) = continuous {
                    let Some(info) = Self::info(&tap, id) else {
                        return false;
                    };
                    let step = if modifiers.shift { 0.005 } else { 0.02 };
                    let n = travel.norm(&info, Self::value(&tap, id)) + up * step;
                    self.set_once(model, cx, id, travel.value(&info, n));
                    return true;
                }
                match hot.target {
                    Target::Switch { id, positions, .. } => {
                        let now = Self::value(&tap, id).round();
                        let v = (now + up).clamp(0.0, (positions - 1) as f64);
                        self.set_once(model, cx, id, v);
                        true
                    }
                    Target::MicSide { mic } => {
                        let angle = if mic == 0 { id::A_ANGLE } else { id::B_ANGLE };
                        let v = (Self::value(&tap, angle) + up * 5.0).clamp(0.0, 90.0);
                        self.set_once(model, cx, angle, v);
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn wants_frames(&self, _model: &Session) -> bool {
        true
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let (at, _) = Self::to_panel(size, pos);
        let (hot, look) = self.hit(model, at)?;
        let tap = self.device.tap(model)?;
        let named = |id: u32| {
            let info = Self::info(&tap, id)?;
            Some(format!(
                "{}: {}",
                info.name,
                Self::text(&tap, id, Self::value(&tap, id))
            ))
        };
        match hot.target {
            Target::Knob { id, .. }
            | Target::Slider { id }
            | Target::Toggle { id }
            | Target::Switch { id, .. }
            | Target::Menu { id }
            | Target::Segment { id, .. } => {
                if id == id::QUALITY {
                    return Some(
                        "Oversampling of every stage. 2x adds its filters' latency to each stage, so the device restarts".into(),
                    );
                }
                named(id)
            }
            Target::Treadle { id, .. } => named(id),
            Target::Footswitch { place } => look.pedals.get(place).map(|p| {
                format!("{}: {} (click to switch)", p.stomp.name(), if p.on { "on" } else { "bypassed" })
            }),
            Target::StompPlate { place } => look
                .pedals
                .get(place)
                .map(|p| format!("{}: click to change the pedal", p.stomp.name())),
            Target::PedalBody { .. } => Some("Drag along the line to move the pedal".into()),
            Target::Remove { .. } => Some("Remove the pedal from the line".into()),
            Target::Add => Some(
                "Add a pedal to the line (each one is a stage of its own and adds one buffer of latency)".into(),
            ),
            Target::MicFront { mic } => Some(format!(
                "Mic {}: drag across the cone (dust cap to edge)",
                if mic == 0 { "A" } else { "B" }
            )),
            Target::MicSide { mic } => Some(format!(
                "Mic {}: drag along its axis for the distance, up and down (or scroll) for the angle",
                if mic == 0 { "A" } else { "B" }
            )),
            Target::Meter { .. } => Some("Click to clear the held peak".into()),
        }
    }

    fn min_size(&self) -> Size {
        Size::new(PANEL_W * 0.45, TOTAL_H * 0.45)
    }
}

impl GuitarView {
    #[allow(clippy::too_many_arguments)]
    fn press(
        &mut self,
        pos: Point,
        button: PointerButton,
        mods: Modifiers,
        clicks: u32,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let (at, _) = Self::to_panel(size, pos);
        let Some((hot, look)) = self.hit(model, at) else {
            return false;
        };
        let Some(tap) = self.device.tap(model) else {
            return false;
        };
        let below = |r: Rect| {
            let v = Self::to_view(size, r);
            Point::new(v.x, v.bottom())
        };
        if button == PointerButton::Secondary {
            match hot.target {
                Target::Knob { id, .. }
                | Target::Slider { id }
                | Target::Toggle { id }
                | Target::Switch { id, .. }
                | Target::Treadle { id, .. }
                | Target::Menu { id } => {
                    if let Some(r) = self.param_menu_request(model, &tap, id, pos) {
                        cx.request(r);
                    }
                }
                Target::Footswitch { place } => {
                    if let Some(p) = look.pedals.get(place)
                        && let Some(r) =
                            self.param_menu_request(model, &tap, id::slot(p.slot, id::ON), pos)
                    {
                        cx.request(r);
                    }
                }
                Target::PedalBody { place }
                | Target::StompPlate { place }
                | Target::Remove { place } => {
                    if let Some(r) = self.stomp_menu(model, &tap, &look, Some(place), pos) {
                        cx.request(r);
                    }
                }
                _ => return false,
            }
            return true;
        }
        if button != PointerButton::Primary {
            return false;
        }
        match hot.target {
            Target::Knob { id, travel, .. } => {
                let Some(info) = Self::info(&tap, id) else {
                    return false;
                };
                if mods.ctrl || mods.meta {
                    self.set_once(model, cx, id, info.default);
                } else if clicks >= 2 {
                    let r = Rect::new(
                        hot.rect.x - 8.0,
                        hot.rect.bottom() - 16.0,
                        hot.rect.w + 16.0,
                        18.0,
                    );
                    self.type_value(model, &tap, id, Self::to_view(size, r), cx);
                } else {
                    self.device.begin(cx, "Guitar Station");
                    self.drag = Some(Drag::Value {
                        id,
                        travel,
                        norm: travel.norm(&info, Self::value(&tap, id)),
                        last: pos,
                        horizontal: false,
                        range: DRAG_RANGE,
                    });
                }
            }
            Target::Slider { id } | Target::Treadle { id, .. } => {
                let Some(info) = Self::info(&tap, id) else {
                    return false;
                };
                if mods.ctrl || mods.meta {
                    self.set_once(model, cx, id, info.default);
                } else {
                    self.device.begin(cx, "Guitar Station");
                    self.drag = Some(Drag::Value {
                        id,
                        travel: Travel::Linear,
                        norm: Travel::Linear.norm(&info, Self::value(&tap, id)),
                        last: pos,
                        horizontal: false,
                        range: hot.rect.h.max(40.0),
                    });
                }
            }
            Target::Toggle { id } => {
                let on = Self::value(&tap, id) >= 0.5;
                self.set_once(model, cx, id, if on { 0.0 } else { 1.0 });
            }
            Target::Switch {
                id,
                positions,
                value,
            } => {
                let v = match value {
                    Some(v) => v as f64,
                    None => (Self::value(&tap, id).round() + 1.0) % positions as f64,
                };
                self.set_once(model, cx, id, v);
            }
            Target::Segment { id, value } => self.set_once(model, cx, id, value as f64),
            Target::Menu { id } => {
                let split = |i: usize| match id {
                    id::AMP => layout::amp_group_starts(i),
                    id::CABINET => i == 14,
                    id::SPEAKER => i == 2,
                    id::MIC_A | id::MIC_B => i == 2,
                    id::POWER => i == 2,
                    _ => false,
                };
                if let Some(r) = self.choices(model, &tap, id, below(hot.rect), split) {
                    cx.request(r);
                }
            }
            Target::Footswitch { place } => {
                if let Some(p) = look.pedals.get(place) {
                    let id = id::slot(p.slot, id::ON);
                    self.set_once(model, cx, id, if p.on { 0.0 } else { 1.0 });
                }
            }
            Target::StompPlate { place } => {
                if let Some(r) = self.stomp_menu(model, &tap, &look, Some(place), below(hot.rect)) {
                    cx.request(r);
                }
            }
            Target::Add => {
                if let Some(r) = self.stomp_menu(model, &tap, &look, None, below(hot.rect)) {
                    cx.request(r);
                }
            }
            Target::Remove { place } => {
                if let Some(a) = self.remove_pedal(model, &tap, &look, place) {
                    cx.emit(a);
                }
            }
            Target::PedalBody { place } => {
                let Some(p) = look.pedals.get(place) else {
                    return false;
                };
                self.drag = Some(Drag::Pedal {
                    from: place,
                    grab: at.x - p.rect.x,
                    x: at.x,
                    moved: false,
                });
                cx.set_cursor(Cursor::Grabbing);
            }
            Target::MicFront { mic } | Target::MicSide { mic } => {
                self.device.begin(cx, "Microphone");
                self.drag = Some(Drag::Mic {
                    mic,
                    side: matches!(hot.target, Target::MicSide { .. }),
                    last: pos,
                });
            }
            Target::Meter { output } => {
                if output {
                    tap.meter_out.clear_held();
                } else {
                    tap.meter_in.clear_held();
                }
                cx.redraw();
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
    ) -> bool {
        let (at, s) = Self::to_panel(size, pos);
        let Some(tap) = self.device.tap(model) else {
            return false;
        };
        let device = self.device;
        match &mut self.drag {
            Some(Drag::Value {
                id,
                travel,
                norm,
                last,
                horizontal,
                range,
            }) => {
                let Some(info) = Self::info(&tap, *id) else {
                    return false;
                };
                let speed = if mods.shift { FINE } else { 1.0 } as f64;
                let delta = if *horizontal {
                    pos.x - last.x
                } else {
                    last.y - pos.y
                };
                *norm = (*norm + f64::from(delta / (*range * s)) * speed).clamp(0.0, 1.0);
                *last = pos;
                let v = travel.value(&info, *norm);
                let v = if info.stepped { v.round() } else { v };
                device.set(model, cx, ParameterId(*id), v);
                true
            }
            Some(Drag::Mic { mic, side, last }) => {
                let (pos_id, dist_id, angle_id) = if *mic == 0 {
                    (id::A_POSITION, id::A_DISTANCE, id::A_ANGLE)
                } else {
                    (id::B_POSITION, id::B_DISTANCE, id::B_ANGLE)
                };
                let fine = if mods.shift { FINE } else { 1.0 };
                if *side {
                    let Some(info) = Self::info(&tap, dist_id) else {
                        return false;
                    };
                    let n = Travel::Log.norm(&info, Self::value(&tap, dist_id))
                        + f64::from((pos.x - last.x) / (layout::SIDE_TRAVEL * s) * fine);
                    device.set(model, cx, ParameterId(dist_id), Travel::Log.value(&info, n));
                    let angle = (Self::value(&tap, angle_id)
                        + f64::from((last.y - pos.y) / (layout::ANGLE_TRAVEL * s) * fine) * 90.0)
                        .clamp(0.0, 90.0);
                    device.set(model, cx, ParameterId(angle_id), angle);
                } else {
                    let sign = if *mic == 0 { -1.0 } else { 1.0 };
                    let v = (Self::value(&tap, pos_id)
                        + f64::from(sign * (pos.x - last.x) / (layout::CONE_R * s) * fine))
                    .clamp(0.0, 1.0);
                    device.set(model, cx, ParameterId(pos_id), v);
                }
                *last = pos;
                true
            }
            Some(Drag::Pedal { x, moved, .. }) => {
                if (at.x - *x).abs() > 3.0 {
                    *moved = true;
                }
                if *moved {
                    *x = at.x;
                }
                cx.set_cursor(Cursor::Grabbing);
                cx.redraw();
                true
            }
            None => false,
        }
    }

    fn release(
        &mut self,
        _pos: Point,
        _size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match self.drag.take() {
            Some(Drag::Value { .. } | Drag::Mic { .. }) => {
                self.device.end(cx);
                true
            }
            Some(Drag::Pedal {
                from,
                grab,
                x,
                moved,
            }) => {
                if moved && let Some((look, tap)) = self.look(model) {
                    let to = layout::place_at(x - grab, look.pedals.len());
                    if let Some(a) = self.move_pedal(model, &tap, &look, from, to) {
                        cx.emit(a);
                    }
                }
                cx.redraw();
                true
            }
            None => false,
        }
    }
}
