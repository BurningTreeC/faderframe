//! The kit every device editor but the EQ and the Program EQ is built from:
//! a display at the top that the device paints itself (a transfer curve, a
//! decay, a tuner's strobe), a deck of titled sections below, holding
//! controls bound to the device's parameters, and level meters.
//!
//! A device only describes itself ([`Face`]): where things go, how its
//! display looks (and, if it likes, what dragging in it does). The kit
//! draws the controls alike everywhere and makes them behave alike: drag
//! up and down (Shift: finer), scroll, double-click to type a value
//! ("1k", "-3", "12 ms", "4:1", "50%"), Ctrl-click for the default,
//! right-click for the default, the automation lane or MIDI learn. Every
//! change is a parameter edit of the plugin slot (undoable, automatable),
//! a drag one undo step.

use crate::common::Device;
use crate::values::{ms_text, parse_db, parse_freq, parse_ms, parse_ratio, parse_value};
use faderframe_automation::AutomationTarget;
use faderframe_core::{ParameterId, PluginInstanceId};
use faderframe_plugin_host::tap::{AnalysisTap, db, db_power};
use faderframe_plugin_host::{ParameterInfo, ParameterUnit};
use faderframe_project::{Command, MappingTarget};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::controls::{self, KnobLook};
use faderframe_ui_canvas::{
    CanvasView, Color, Cursor, EventCx, FontWeight, HostRequest, MenuItem, Modifiers, Paint,
    Painter, Point, PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};
use std::time::Instant;

/// How a knob or slider spreads its range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Scale {
    Linear,
    /// Logarithmic (frequencies, times, ratios): the range must be positive.
    Log,
    /// `lo + (hi − lo) t^k`: more resolution low down.
    Skew(f64),
}

impl Scale {
    pub fn to_value(self, info: &ParameterInfo, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        let (lo, hi) = (info.min, info.max);
        match self {
            Scale::Linear => lo + (hi - lo) * t,
            Scale::Log if lo > 0.0 => lo * (hi / lo).powf(t),
            Scale::Log => lo + (hi - lo) * t,
            Scale::Skew(k) => lo + (hi - lo) * t.powf(k),
        }
    }

    pub fn to_norm(self, info: &ParameterInfo, v: f64) -> f64 {
        let (lo, hi) = (info.min, info.max);
        if hi <= lo {
            return 0.0;
        }
        let t = match self {
            Scale::Linear => (v - lo) / (hi - lo),
            Scale::Log if lo > 0.0 => (v.max(lo) / lo).ln() / (hi / lo).ln(),
            Scale::Log => (v - lo) / (hi - lo),
            Scale::Skew(k) => ((v - lo) / (hi - lo)).max(0.0).powf(1.0 / k),
        };
        t.clamp(0.0, 1.0)
    }

    /// The natural scale of a parameter's unit and range.
    pub fn of(info: &ParameterInfo) -> Self {
        match info.unit {
            ParameterUnit::Hertz if info.min > 0.0 => Scale::Log,
            ParameterUnit::Milliseconds if info.min > 0.0 => Scale::Log,
            ParameterUnit::Milliseconds => Scale::Skew(3.0),
            _ => Scale::Linear,
        }
    }
}

/// What a control is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Kind {
    Knob {
        bipolar: bool,
    },
    /// A smaller knob.
    SmallKnob {
        bipolar: bool,
    },
    /// An on/off switch (stepped 0/1).
    Toggle,
    /// A stepped value picked from a menu.
    Choice,
    /// A stepped value with every option in view.
    Segments,
    /// A horizontal slider.
    Slider,
}

/// A control bound to a parameter.
#[derive(Clone, Debug)]
pub(crate) struct Ctl {
    pub id: ParameterId,
    pub rect: Rect,
    pub kind: Kind,
    pub label: &'static str,
    /// `None`: the unit's natural scale.
    pub scale: Option<Scale>,
}

impl Ctl {
    pub fn knob(id: ParameterId, label: &'static str, rect: Rect) -> Self {
        Self {
            id,
            rect,
            kind: Kind::Knob { bipolar: false },
            label,
            scale: None,
        }
    }

    pub fn small(id: ParameterId, label: &'static str, rect: Rect) -> Self {
        Self {
            kind: Kind::SmallKnob { bipolar: false },
            ..Self::knob(id, label, rect)
        }
    }

    pub fn toggle(id: ParameterId, label: &'static str, rect: Rect) -> Self {
        Self {
            kind: Kind::Toggle,
            ..Self::knob(id, label, rect)
        }
    }

    pub fn choice(id: ParameterId, label: &'static str, rect: Rect) -> Self {
        Self {
            kind: Kind::Choice,
            ..Self::knob(id, label, rect)
        }
    }

    pub fn segments(id: ParameterId, label: &'static str, rect: Rect) -> Self {
        Self {
            kind: Kind::Segments,
            ..Self::knob(id, label, rect)
        }
    }

    #[expect(dead_code, reason = "for the faces with sliders still to come")]
    pub fn slider(id: ParameterId, label: &'static str, rect: Rect) -> Self {
        Self {
            kind: Kind::Slider,
            ..Self::knob(id, label, rect)
        }
    }

    pub fn bipolar(mut self) -> Self {
        self.kind = match self.kind {
            Kind::Knob { .. } => Kind::Knob { bipolar: true },
            Kind::SmallKnob { .. } => Kind::SmallKnob { bipolar: true },
            k => k,
        };
        self
    }

    pub fn scaled(mut self, scale: Scale) -> Self {
        self.scale = Some(scale);
        self
    }
}

/// A titled group of controls on the deck.
#[derive(Clone, Debug)]
pub(crate) struct Section {
    pub title: &'static str,
    pub rect: Rect,
}

/// What a level meter shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MeterKind {
    Input,
    Output,
    /// Gain reduction (dB, positive down) from a published value.
    Reduction(usize),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Meter {
    pub rect: Rect,
    pub kind: MeterKind,
    pub label: &'static str,
}

/// A device's layout at a size.
#[derive(Clone, Debug, Default)]
pub(crate) struct Panel {
    pub display: Option<Rect>,
    pub sections: Vec<Section>,
    pub controls: Vec<Ctl>,
    pub meters: Vec<Meter>,
}

/// What a face gets to paint and handle its display with.
pub(crate) struct Ctx<'a> {
    pub model: &'a Session,
    pub tap: &'a AnalysisTap,
    pub theme: &'a Theme,
    pub accent: Color,
    /// Seconds since the last frame (for falling meters and histories).
    pub dt: f32,
    /// Files are being dragged over the view here.
    pub drop: Option<Point>,
    /// Samples are being loaded into the device.
    pub loading: bool,
}

impl Ctx<'_> {
    pub fn index(&self, id: ParameterId) -> Option<usize> {
        self.tap.params.infos().iter().position(|i| i.id == id)
    }

    pub fn value(&self, id: ParameterId) -> f64 {
        self.index(id)
            .map_or(0.0, |i| f64::from(self.tap.params.get(i)))
    }

    pub fn on(&self, id: ParameterId) -> bool {
        self.value(id) >= 0.5
    }

    pub fn info(&self, id: ParameterId) -> Option<&ParameterInfo> {
        self.tap.params.infos().iter().find(|i| i.id == id)
    }

    pub fn published(&self, i: usize) -> f32 {
        self.tap.value(i)
    }
}

/// Edits a face makes from its display.
pub(crate) struct Edit<'a, 'b> {
    device: Device,
    model: &'a Session,
    pub cx: &'a mut EventCx<'b, Action>,
}

impl Edit<'_, '_> {
    pub fn begin(&mut self, label: &str) {
        self.device.begin(self.cx, label);
    }

    pub fn end(&mut self) {
        self.device.end(self.cx);
    }

    pub fn set(&mut self, id: ParameterId, v: f64) {
        self.device.set(self.model, self.cx, id, v);
    }

    /// A change of its own undo step.
    pub fn set_once(&mut self, id: ParameterId, v: f64) {
        self.begin("Device");
        self.set(id, v);
        self.end();
    }

    /// Clear one of a sampler's slots (one undo step).
    pub fn clear_sample(&mut self, slot: usize) {
        self.cx.emit(Action::LoadDeviceSamples {
            plugin: self.device.plugin,
            slot,
            files: Vec::new(),
        });
    }

    /// Offer a file chooser; the files picked go into the samples from
    /// `slot` on (one each).
    pub fn choose_samples(&mut self, slot: usize, title: &str, sfz: bool) {
        let plugin = self.device.plugin;
        self.cx.request(HostRequest::ChooseFiles {
            choice: faderframe_ui_canvas::FileChoice::Open {
                title: title.into(),
                filters: sample_filters(sfz),
            },
            commit: Box::new(move |files| {
                Some(Action::LoadDeviceSamples {
                    plugin,
                    slot,
                    files,
                })
            }),
        });
    }
}

/// File chooser filters for samples (and SFZ instruments).
fn sample_filters(sfz: bool) -> Vec<(String, Vec<String>)> {
    let mut patterns: Vec<String> = faderframe_audio_files::decode::SUPPORTED_EXTENSIONS
        .iter()
        .flat_map(|e| [format!("*.{e}"), format!("*.{}", e.to_ascii_uppercase())])
        .collect();
    let mut filters = Vec::new();
    if sfz {
        patterns.extend(["*.sfz".into(), "*.SFZ".into()]);
        filters.push(("Samples and SFZ instruments".to_string(), patterns));
    } else {
        filters.push(("Samples".to_string(), patterns));
    }
    filters.push(("All files".to_string(), vec!["*".to_string()]));
    filters
}

/// A device's part of its editor.
pub(crate) trait Face {
    /// The device's colour (knob rings, its display's highlights).
    fn accent(&self) -> Color;
    fn panel(&self, size: Size) -> Panel;
    /// Paint the display (the kit has filled and framed it).
    fn paint_display(&mut self, _p: &mut dyn Painter, _r: Rect, _cx: &Ctx<'_>) {}
    /// Pointer events in the display; `true` when handled.
    fn display_event(
        &mut self,
        _ev: &ViewEvent,
        _r: Rect,
        _cx: &Ctx<'_>,
        _edit: &mut Edit<'_, '_>,
    ) -> bool {
        false
    }
    /// A value as the device shows it (`None`: by its unit).
    fn format(&self, _id: ParameterId, _value: f64) -> Option<String> {
        None
    }
    /// What a control does, for its tooltip.
    fn tip(&self, _id: ParameterId) -> Option<&'static str> {
        None
    }
    fn min_size(&self) -> Size;
    /// The sample slot files dropped at `pos` in the display go into (and
    /// whether SFZ instruments are taken), or `None` where nothing is.
    fn drop_slot(&self, _pos: Point, _display: Rect) -> Option<(usize, bool)> {
        None
    }
}

/// A value by its parameter's unit.
pub(crate) fn format_by_unit(info: &ParameterInfo, v: f64) -> String {
    match info.unit {
        ParameterUnit::Decibels => {
            if v <= info.min && info.min <= -90.0 {
                "−∞ dB".into()
            } else {
                crate::values::db_text(v)
            }
        }
        ParameterUnit::Hertz => faderframe_plugin_host::eq::format_hz(v),
        ParameterUnit::Milliseconds => ms_text(v),
        ParameterUnit::Percent => format!("{:.0} %", v * 100.0),
        ParameterUnit::Samples => format!("{v:.0} smp"),
        _ if info.stepped => format!("{v:.0}"),
        _ => format!("{v:.2}"),
    }
}

/// A typed value for a parameter.
pub(crate) fn parse_for(info: &ParameterInfo, text: &str) -> Option<f64> {
    let v = match info.unit {
        ParameterUnit::Decibels => parse_db(text),
        ParameterUnit::Hertz => parse_freq(text),
        ParameterUnit::Milliseconds => parse_ms(text),
        ParameterUnit::Percent => {
            let t = text.trim().trim_end_matches('%').trim();
            t.parse::<f64>().ok().map(|p| p / 100.0)
        }
        _ if info.name.contains("Ratio") => parse_ratio(text),
        _ => parse_value(text, info.min, info.max),
    }?;
    Some(info.clamp(v))
}

enum Drag {
    Control {
        id: ParameterId,
        scale: Scale,
        from: f64,
        start: Point,
        horizontal: bool,
        track: Rect,
    },
    Display,
}

/// A device editor: a [`Face`] in the kit's frame.
pub(crate) struct DeviceView<F: Face> {
    device: Device,
    theme: Theme,
    face: F,
    drag: Option<Drag>,
    hover: Option<ParameterId>,
    last: Option<Instant>,
    /// Falling meter peaks per meter and channel.
    peaks: Vec<[f32; 2]>,
    /// Where files are dragged over the view.
    drop: Option<Point>,
}

impl<F: Face> DeviceView<F> {
    pub fn new(plugin: PluginInstanceId, theme: &Theme, face: F) -> Self {
        Self {
            device: Device::new(plugin),
            theme: theme.clone(),
            face,
            drag: None,
            hover: None,
            last: None,
            peaks: Vec::new(),
            drop: None,
        }
    }

    fn text(&self, tap: &AnalysisTap, id: ParameterId, v: f64) -> String {
        self.face.format(id, v).unwrap_or_else(|| {
            tap.params
                .infos()
                .iter()
                .find(|i| i.id == id)
                .map_or_else(|| format!("{v:.2}"), |info| format_by_unit(info, v))
        })
    }

    fn control_at(&self, panel: &Panel, pos: Point) -> Option<Ctl> {
        panel
            .controls
            .iter()
            .find(|c| c.rect.contains(pos))
            .cloned()
    }

    fn ctx<'a>(
        &self,
        model: &'a Session,
        tap: &'a AnalysisTap,
        theme: &'a Theme,
        dt: f32,
    ) -> Ctx<'a> {
        Ctx {
            model,
            tap,
            theme,
            accent: self.face.accent(),
            dt,
            drop: self.drop,
            loading: model.loading_samples(self.device.plugin),
        }
    }

    // --- painting --------------------------------------------------------------

    fn paint_control(&self, p: &mut dyn Painter, c: &Ctl, cx: &Ctx<'_>) {
        let th = cx.theme;
        let Some(info) = cx.info(c.id) else {
            return;
        };
        let v = cx.value(c.id);
        let scale = c.scale.unwrap_or_else(|| Scale::of(info));
        let hovered = self.hover == Some(c.id);
        let label_style = TextStyle::new(th.fonts.tiny, th.ui.text_dim)
            .bold()
            .center()
            .tracking(0.6);
        let value_style = TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center();
        let r = c.rect;
        match c.kind {
            Kind::Knob { bipolar } | Kind::SmallKnob { bipolar } => {
                let size = (r.w - 12.0).min(r.h - 32.0).max(14.0);
                let k = Rect::new(r.x + (r.w - size) / 2.0, r.y + 15.0, size, size);
                if hovered {
                    p.circle(k.center(), size * 0.5 + 4.0, cx.accent.with_alpha(0.12));
                }
                controls::knob(
                    p,
                    k,
                    scale.to_norm(info, v) as f32,
                    bipolar,
                    KnobLook {
                        cap: th.console.knob.cap_top,
                        ring: cx.accent,
                    },
                    th,
                );
                p.text(
                    c.label,
                    Rect::new(r.x - 8.0, r.y, r.w + 16.0, 13.0),
                    &label_style,
                );
                p.text(
                    &self.text(cx.tap, c.id, v),
                    Rect::new(r.x - 10.0, k.bottom() + 2.0, r.w + 20.0, 14.0),
                    &value_style,
                );
            }
            Kind::Toggle => {
                let on = v >= 0.5;
                let b = Rect::new(r.x, r.y + (r.h - 22.0).max(0.0) / 2.0, r.w, r.h.min(22.0));
                if on {
                    p.shadow(b, 4.0, cx.accent.with_alpha(0.35), 0.0, 0.0, 8.0);
                }
                p.fill_rounded(
                    b,
                    4.0,
                    &Paint::Solid(if on {
                        cx.accent.with_alpha(0.32)
                    } else {
                        th.ui.surface_alt
                    }),
                );
                p.stroke_rounded(
                    b,
                    4.0,
                    1.0,
                    if on {
                        cx.accent.with_alpha(0.9)
                    } else if hovered {
                        th.ui.text_faint
                    } else {
                        th.ui.border
                    },
                );
                // A lamp left of the name.
                p.circle(
                    Point::new(b.x + 10.0, b.center().y),
                    3.0,
                    if on {
                        cx.accent
                    } else {
                        th.ui.text_faint.with_alpha(0.5)
                    },
                );
                p.text(
                    c.label,
                    Rect::new(b.x + 14.0, b.y, b.w - 16.0, b.h),
                    &TextStyle::new(th.fonts.small, th.ui.text).center(),
                );
            }
            Kind::Choice => {
                p.text(c.label, Rect::new(r.x, r.y, r.w, 13.0), &label_style);
                let b = Rect::new(r.x, r.y + 15.0, r.w, (r.h - 15.0).min(22.0));
                p.fill_rounded(b, 4.0, &Paint::Solid(th.ui.surface_alt));
                p.stroke_rounded(
                    b,
                    4.0,
                    1.0,
                    if hovered {
                        cx.accent.with_alpha(0.7)
                    } else {
                        th.ui.border
                    },
                );
                p.text(
                    &format!("{} ▾", self.text(cx.tap, c.id, v)),
                    b,
                    &TextStyle::new(th.fonts.small, th.ui.text).center(),
                );
            }
            Kind::Segments => {
                if !c.label.is_empty() {
                    p.text(c.label, Rect::new(r.x, r.y, r.w, 13.0), &label_style);
                }
                let top = if c.label.is_empty() { r.y } else { r.y + 15.0 };
                let b = Rect::new(r.x, top, r.w, (r.bottom() - top).min(22.0));
                let n = (info.max - info.min).round().max(0.0) as usize + 1;
                let w = b.w / n as f32;
                p.fill_rounded(b, 4.0, &Paint::Solid(th.ui.surface_alt));
                let now = v.round() as i64;
                for i in 0..n {
                    let value = info.min + i as f64;
                    let s = Rect::new(b.x + w * i as f32, b.y, w, b.h);
                    if value.round() as i64 == now {
                        p.fill_rounded(s.inset(1.5), 3.0, &Paint::Solid(cx.accent.with_alpha(0.4)));
                    }
                    if i > 0 {
                        p.vline(s.x, s.y + 4.0, s.bottom() - 4.0, th.ui.border);
                    }
                    p.text(
                        &self.text(cx.tap, c.id, value),
                        s,
                        &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center(),
                    );
                }
                p.stroke_rounded(b, 4.0, 1.0, th.ui.border);
            }
            Kind::Slider => {
                p.text(
                    &format!("{}  {}", c.label, self.text(cx.tap, c.id, v)),
                    Rect::new(r.x, r.y, r.w, 13.0),
                    &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                        .bold()
                        .tracking(0.5),
                );
                let track = Rect::new(r.x, r.y + 20.0, r.w, 5.0);
                p.fill_rounded(track, 2.5, &Paint::Solid(th.device.display));
                let t = scale.to_norm(info, v) as f32;
                p.fill_rounded(
                    Rect::new(track.x, track.y, track.w * t, track.h),
                    2.5,
                    &Paint::Solid(cx.accent.with_alpha(0.85)),
                );
                p.circle(
                    Point::new(track.x + track.w * t, track.center().y),
                    6.0,
                    th.ui.text,
                );
            }
        }
    }

    fn paint_meter(&mut self, p: &mut dyn Painter, i: usize, m: &Meter, cx: &Ctx<'_>) {
        let th = cx.theme;
        if self.peaks.len() <= i {
            self.peaks.resize(i + 1, [-150.0; 2]);
        }
        let r = m.rect;
        p.text(
            m.label,
            Rect::new(r.x - 10.0, r.y - 14.0, r.w + 20.0, 12.0),
            &TextStyle::new(th.fonts.tiny - 0.5, th.ui.text_dim).center(),
        );
        p.fill_rounded(r, 2.0, &Paint::Solid(th.device.display));
        // 0 dB at the top, −60 at the bottom.
        let y_of = |db: f32| r.y + r.h * (-db / 60.0).clamp(0.0, 1.0);
        match m.kind {
            MeterKind::Input | MeterKind::Output => {
                let meter = if m.kind == MeterKind::Input {
                    &cx.tap.meter_in
                } else {
                    &cx.tap.meter_out
                };
                let w = (r.w - 1.0) / 2.0;
                for c in 0..2 {
                    let peak = db(meter.take_peak(c));
                    let held = &mut self.peaks[i][c];
                    *held = if peak > *held {
                        peak
                    } else {
                        (*held - 24.0 * cx.dt).max(-150.0)
                    };
                    let rms = db_power(meter.mean_square(c));
                    let x = r.x + c as f32 * (w + 1.0);
                    let ry = y_of(rms);
                    p.fill(
                        Rect::new(x, ry, w, r.bottom() - ry),
                        th.tools.level_ok.with_alpha(0.8),
                    );
                    let py = y_of(*held);
                    let color = if *held > -0.1 {
                        th.tools.level_over
                    } else if *held > -6.0 {
                        th.tools.level_warn
                    } else {
                        th.tools.level_ok
                    };
                    p.fill(Rect::new(x, py, w, 2.0), color);
                }
            }
            MeterKind::Reduction(v) => {
                // Down from the top: 0 to 24 dB.
                let gr = cx.tap.value(v).max(0.0);
                let h = r.h * (gr / 24.0).clamp(0.0, 1.0);
                p.fill(
                    Rect::new(r.x, r.y, r.w, h),
                    th.device.reduction.with_alpha(0.9),
                );
            }
        }
        for tick in [6.0f32, 12.0, 24.0, 48.0] {
            let y = match m.kind {
                MeterKind::Reduction(_) => {
                    if tick > 24.0 {
                        continue;
                    }
                    r.y + r.h * tick / 24.0
                }
                _ => y_of(-tick),
            };
            p.hline(r.x, r.right(), y, th.device.grid_strong);
        }
    }

    fn paint_all(&mut self, p: &mut dyn Painter, size: Size, model: &Session) {
        let th = self.theme.clone();
        p.fill(Rect::from_size(size), th.device.deck);
        let Some(tap) = self.device.tap(model) else {
            p.text(
                "The device is not loaded",
                Rect::from_size(size),
                &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
            );
            return;
        };
        tap.watch();
        let now = Instant::now();
        let dt = self
            .last
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32())
            .min(0.25);
        self.last = Some(now);
        let panel = self.face.panel(size);
        let accent = self.face.accent();
        if let Some(d) = panel.display {
            p.fill_rounded(d, 6.0, &Paint::Solid(th.device.display));
            p.push_clip(d);
            let cx = self.ctx(model, &tap, &th, dt);
            self.face.paint_display(p, d, &cx);
            p.pop_clip();
            p.stroke_rounded(d, 6.0, 1.0, th.device.section_edge);
        }
        for s in &panel.sections {
            p.fill_rounded(s.rect, 6.0, &Paint::Solid(th.device.section));
            p.stroke_rounded(s.rect, 6.0, 1.0, th.device.section_edge);
            p.text(
                s.title,
                Rect::new(s.rect.x + 10.0, s.rect.y + 5.0, s.rect.w - 20.0, 13.0),
                &TextStyle::new(th.fonts.tiny, accent.mix(th.ui.text_dim, 0.35))
                    .weight(FontWeight::Bold)
                    .tracking(1.0),
            );
        }
        let cx = self.ctx(model, &tap, &th, dt);
        for c in &panel.controls {
            self.paint_control(p, c, &cx);
        }
        for (i, m) in panel.meters.iter().enumerate() {
            self.paint_meter(p, i, m, &cx);
        }
    }

    // --- events ----------------------------------------------------------------

    fn set(&self, model: &Session, cx: &mut EventCx<'_, Action>, id: ParameterId, v: f64) {
        self.device.set(model, cx, id, v);
    }

    fn set_once(&self, model: &Session, cx: &mut EventCx<'_, Action>, id: ParameterId, v: f64) {
        self.device.begin(cx, "Device");
        self.set(model, cx, id, v);
        self.device.end(cx);
    }

    fn action(&self, model: &Session, id: ParameterId, v: f64) -> Option<Action> {
        let (track, _) = model.plugin_owner(self.device.plugin)?;
        Some(Action::Edit(Command::SetPluginParameter {
            track,
            plugin: self.device.plugin,
            parameter: id,
            value: Some(v),
        }))
    }

    fn menu(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        c: &Ctl,
        at: Point,
    ) -> Option<HostRequest<Action>> {
        let info = tap.params.infos().iter().find(|i| i.id == c.id)?.clone();
        let (track, _) = model.plugin_owner(self.device.plugin)?;
        let target = AutomationTarget::PluginParameter {
            plugin: self.device.plugin,
            parameter: c.id,
        };
        let mut items = Vec::new();
        if let Some(a) = self.action(model, c.id, info.default) {
            items.push(MenuItem::new(
                format!("Default ({})", self.text(tap, c.id, info.default)),
                a,
            ));
        }
        if info.automatable {
            items.push(
                MenuItem::new("Show Automation", Action::ShowAutomation { track, target })
                    .separated(),
            );
            items.push(MenuItem::new(
                "Learn MIDI Controller",
                Action::MidiLearn(MappingTarget::Parameter { track, target }),
            ));
        }
        Some(HostRequest::ContextMenu { at, items })
    }

    fn choices(&self, model: &Session, tap: &AnalysisTap, c: &Ctl) -> Option<HostRequest<Action>> {
        let info = tap.params.infos().iter().find(|i| i.id == c.id)?;
        let now = f64::from(
            tap.params
                .get(tap.params.infos().iter().position(|i| i.id == c.id)?),
        );
        let n = (info.max - info.min).round().max(0.0) as usize + 1;
        let items = (0..n)
            .filter_map(|i| {
                let v = info.min + i as f64;
                let a = self.action(model, c.id, v)?;
                Some(MenuItem::new(self.text(tap, c.id, v), a).checked((now - v).abs() < 0.5))
            })
            .collect();
        Some(HostRequest::ContextMenu {
            at: Point::new(c.rect.x, c.rect.bottom()),
            items,
        })
    }

    fn type_value(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        c: &Ctl,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(info) = tap.params.infos().iter().find(|i| i.id == c.id).cloned() else {
            return;
        };
        let Some((track, _)) = model.plugin_owner(self.device.plugin) else {
            return;
        };
        let index = tap
            .params
            .infos()
            .iter()
            .position(|i| i.id == c.id)
            .unwrap_or(0);
        let initial = self.text(tap, c.id, f64::from(tap.params.get(index)));
        let plugin = self.device.plugin;
        let id = c.id;
        cx.request(HostRequest::TextInput {
            at: Rect::new(
                c.rect.x - 6.0,
                c.rect.bottom() - 18.0,
                c.rect.w + 12.0,
                18.0,
            ),
            initial,
            commit: Box::new(move |text| {
                let v = parse_for(&info, text)?;
                Some(Action::Edit(Command::SetPluginParameter {
                    track,
                    plugin,
                    parameter: id,
                    value: Some(v),
                }))
            }),
        });
    }

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
        let Some(tap) = self.device.tap(model) else {
            return false;
        };
        let panel = self.face.panel(size);
        if let Some(c) = self.control_at(&panel, pos) {
            let Some(info) = tap.params.infos().iter().find(|i| i.id == c.id).cloned() else {
                return false;
            };
            let index = tap
                .params
                .infos()
                .iter()
                .position(|i| i.id == c.id)
                .unwrap_or(0);
            let v = f64::from(tap.params.get(index));
            if button == PointerButton::Secondary {
                if let Some(req) = self.menu(model, &tap, &c, pos) {
                    cx.request(req);
                }
                return true;
            }
            if button != PointerButton::Primary {
                return false;
            }
            if (mods.ctrl || mods.meta) && !matches!(c.kind, Kind::Toggle | Kind::Segments) {
                self.set_once(model, cx, c.id, info.default);
                return true;
            }
            match c.kind {
                Kind::Toggle => {
                    self.set_once(model, cx, c.id, if v >= 0.5 { info.min } else { info.max })
                }
                Kind::Choice => {
                    if let Some(req) = self.choices(model, &tap, &c) {
                        cx.request(req);
                    }
                }
                Kind::Segments => {
                    let n = (info.max - info.min).round().max(0.0) as usize + 1;
                    let i = (((pos.x - c.rect.x) / c.rect.w) * n as f32)
                        .floor()
                        .clamp(0.0, (n - 1) as f32);
                    self.set_once(model, cx, c.id, info.min + f64::from(i));
                }
                Kind::Knob { .. } | Kind::SmallKnob { .. } | Kind::Slider => {
                    if clicks >= 2 {
                        self.type_value(model, &tap, &c, cx);
                        return true;
                    }
                    let scale = c.scale.unwrap_or_else(|| Scale::of(&info));
                    self.device.begin(cx, c.label);
                    let horizontal = c.kind == Kind::Slider;
                    let track = Rect::new(c.rect.x, c.rect.y + 20.0, c.rect.w, 5.0);
                    self.drag = Some(Drag::Control {
                        id: c.id,
                        scale,
                        from: scale.to_norm(&info, v),
                        start: pos,
                        horizontal,
                        track,
                    });
                    if horizontal {
                        let t = f64::from((pos.x - track.x) / track.w);
                        self.set(model, cx, c.id, scale.to_value(&info, t));
                    }
                }
            }
            return true;
        }
        if let Some(d) = panel.display.filter(|d| d.contains(pos)) {
            let th = self.theme.clone();
            let ctx = self.ctx(model, &tap, &th, 0.0);
            let mut edit = Edit {
                device: self.device,
                model,
                cx,
            };
            let ev = ViewEvent::PointerDown {
                pos,
                button,
                modifiers: mods,
                clicks,
            };
            if self.face.display_event(&ev, d, &ctx, &mut edit) {
                self.drag = Some(Drag::Display);
                return true;
            }
        }
        false
    }

    fn handle(
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
                let handled = self.press(pos, button, modifiers, clicks, size, model, cx);
                cx.redraw();
                handled
            }
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging,
            } => {
                let Some(tap) = self.device.tap(model) else {
                    return false;
                };
                match &self.drag {
                    Some(Drag::Control {
                        id,
                        scale,
                        from,
                        start,
                        horizontal,
                        track,
                    }) if dragging => {
                        let Some(info) = tap.params.infos().iter().find(|i| i.id == *id) else {
                            return false;
                        };
                        let fine = if modifiers.shift { 0.2 } else { 1.0 };
                        let t = if *horizontal {
                            if modifiers.shift {
                                from + f64::from((pos.x - start.x) / track.w) * 0.2
                            } else {
                                f64::from((pos.x - track.x) / track.w)
                            }
                        } else {
                            from + f64::from((start.y - pos.y) / 200.0 * fine)
                        };
                        let v = scale.to_value(info, t);
                        let v = if info.stepped { v.round() } else { v };
                        self.set(model, cx, *id, v);
                        cx.set_cursor(Cursor::Grabbing);
                        return true;
                    }
                    Some(Drag::Display) if dragging => {
                        let th = self.theme.clone();
                        let panel = self.face.panel(size);
                        if let Some(d) = panel.display {
                            let ctx = self.ctx(model, &tap, &th, 0.0);
                            let mut edit = Edit {
                                device: self.device,
                                model,
                                cx,
                            };
                            self.face.display_event(ev, d, &ctx, &mut edit);
                        }
                        cx.redraw();
                        return true;
                    }
                    _ => {}
                }
                let panel = self.face.panel(size);
                let hover = self.control_at(&panel, pos).map(|c| c.id);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                cx.set_cursor(if hover.is_some() {
                    Cursor::Pointer
                } else {
                    Cursor::Default
                });
                // The display may want to show what is under the pointer.
                if let Some(d) = panel.display.filter(|d| d.contains(pos)) {
                    let th = self.theme.clone();
                    let ctx = self.ctx(model, &tap, &th, 0.0);
                    let mut edit = Edit {
                        device: self.device,
                        model,
                        cx,
                    };
                    if self.face.display_event(ev, d, &ctx, &mut edit) {
                        return true;
                    }
                }
                hover.is_some()
            }
            ViewEvent::PointerUp { .. } => match self.drag.take() {
                Some(Drag::Control { .. }) => {
                    self.device.end(cx);
                    cx.redraw();
                    true
                }
                Some(Drag::Display) => {
                    if let Some(tap) = self.device.tap(model) {
                        let th = self.theme.clone();
                        if let Some(d) = self.face.panel(size).display {
                            let ctx = self.ctx(model, &tap, &th, 0.0);
                            let mut edit = Edit {
                                device: self.device,
                                model,
                                cx,
                            };
                            self.face.display_event(ev, d, &ctx, &mut edit);
                        }
                    }
                    cx.redraw();
                    true
                }
                None => false,
            },
            ViewEvent::Scroll {
                pos, dy, modifiers, ..
            } => {
                let Some(tap) = self.device.tap(model) else {
                    return false;
                };
                let panel = self.face.panel(size);
                let Some(c) = self.control_at(&panel, pos) else {
                    return false;
                };
                let Some(info) = tap.params.infos().iter().find(|i| i.id == c.id).cloned() else {
                    return false;
                };
                let index = tap
                    .params
                    .infos()
                    .iter()
                    .position(|i| i.id == c.id)
                    .unwrap_or(0);
                let v = f64::from(tap.params.get(index));
                let up = f64::from(-dy.signum());
                let to = if info.stepped {
                    (v + up).clamp(info.min, info.max)
                } else {
                    let scale = c.scale.unwrap_or_else(|| Scale::of(&info));
                    let step = if modifiers.shift { 0.004 } else { 0.02 };
                    scale.to_value(&info, scale.to_norm(&info, v) + up * step)
                };
                self.set_once(model, cx, c.id, to);
                true
            }
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            _ => false,
        }
    }
}

impl<F: Face> CanvasView<Session, Action> for DeviceView<F> {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, _theme: &Theme) {
        self.paint_all(p, size, model);
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        self.handle(ev, size, model, cx)
    }

    fn wants_frames(&self, _model: &Session) -> bool {
        true
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let tap = self.device.tap(model)?;
        let panel = self.face.panel(size);
        let c = self.control_at(&panel, pos)?;
        let info = tap.params.infos().iter().find(|i| i.id == c.id)?;
        let name = info.name.clone();
        Some(match self.face.tip(c.id) {
            Some(tip) => format!("{name}: {tip}"),
            None => format!("{name} — drag, scroll or double-click to type; Ctrl-click: default"),
        })
    }

    fn min_size(&self) -> Size {
        self.face.min_size()
    }

    fn drag_files(&mut self, pos: Option<Point>, size: Size, _model: &Session) -> bool {
        let display = self.face.panel(size).display;
        let target = pos
            .zip(display)
            .and_then(|(p, d)| self.face.drop_slot(p, d));
        self.drop = target.and(pos);
        target.is_some()
    }

    fn drop_files(
        &mut self,
        files: &[std::path::PathBuf],
        pos: Point,
        size: Size,
        _model: &Session,
    ) -> Option<Action> {
        self.drop = None;
        let display = self.face.panel(size).display?;
        let (slot, sfz) = self.face.drop_slot(pos, display)?;
        let files: Vec<std::path::PathBuf> = files
            .iter()
            .filter(|f| {
                faderframe_audio_files::decode::is_supported(f)
                    || (sfz && faderframe_plugin_host::devices::samples::is_sfz(f))
            })
            .cloned()
            .collect();
        (!files.is_empty()).then_some(Action::LoadDeviceSamples {
            plugin: self.device.plugin,
            slot,
            files,
        })
    }
}

/// The usual frame: a display over a deck of `deck_h`, meters at the right
/// of the display when `meters` (their width).
pub(crate) fn frame(size: Size, deck_h: f32, meters: f32) -> (Rect, Rect, Rect) {
    let pad = 10.0;
    let deck = Rect::new(pad, size.h - deck_h - pad, size.w - 2.0 * pad, deck_h);
    let meter = Rect::new(
        size.w - pad - meters,
        pad + 14.0,
        meters,
        deck.y - pad - pad - 14.0,
    );
    let display = Rect::new(
        pad,
        pad,
        (size.w - 2.0 * pad - if meters > 0.0 { meters + pad } else { 0.0 }).max(10.0),
        deck.y - 2.0 * pad,
    );
    (display, deck, meter)
}

/// Sections across the deck, as wide as `weights` share it.
pub(crate) fn sections(deck: Rect, titles: &[(&'static str, f32)]) -> Vec<Section> {
    let gap = 8.0;
    let total: f32 = titles.iter().map(|(_, w)| w).sum();
    let free = deck.w - gap * (titles.len().saturating_sub(1)) as f32;
    let mut x = deck.x;
    titles
        .iter()
        .map(|(title, w)| {
            let width = free * w / total.max(1e-3);
            let r = Rect::new(x, deck.y, width, deck.h);
            x += width + gap;
            Section { title, rect: r }
        })
        .collect()
}

/// The area inside a section under its title.
pub(crate) fn inside(s: &Section) -> Rect {
    Rect::new(
        s.rect.x + 8.0,
        s.rect.y + 22.0,
        s.rect.w - 16.0,
        s.rect.h - 28.0,
    )
}

/// `n` slots across a row of `r`, each `w` wide, spread evenly.
pub(crate) fn spread(r: Rect, n: usize, w: f32) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let gap = ((r.w - w * n as f32) / n as f32).max(0.0);
    (0..n)
        .map(|i| Rect::new(r.x + gap / 2.0 + i as f32 * (w + gap), r.y, w, r.h))
        .collect()
}

/// Slots of the given widths across a row of `r`, the space left spread
/// evenly round them (all of `r`'s height).
pub(crate) fn row(r: Rect, widths: &[f32]) -> Vec<Rect> {
    let used: f32 = widths.iter().sum();
    let gap = ((r.w - used) / widths.len().max(1) as f32).max(0.0);
    let mut x = r.x + gap / 2.0;
    widths
        .iter()
        .map(|w| {
            let s = Rect::new(x, r.y, *w, r.h);
            x += w + gap;
            s
        })
        .collect()
}

/// Big and small knob sizes (width, height) on the deck.
pub(crate) const KNOB: (f32, f32) = (70.0, 90.0);
pub(crate) const SMALL: (f32, f32) = (58.0, 78.0);
/// A switch's height.
pub(crate) const SWITCH_H: f32 = 22.0;

/// A rect of `w`×`h` at the top-left of `r`'s centre column.
pub(crate) fn at(r: Rect, x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::new(r.x + x, r.y + y, w, h)
}

/// A scrolling history of what goes in, what comes out (peak levels, dB)
/// and how much is taken (dB), sampled from a device's published peaks.
pub(crate) struct History {
    data: Vec<[f32; 3]>,
    head: usize,
    since: f32,
    rate: f32,
}

impl History {
    pub fn new(seconds: f32, rate: f32) -> Self {
        Self {
            data: vec![[-150.0, -150.0, 0.0]; (seconds * rate) as usize],
            head: 0,
            since: 0.0,
            rate,
        }
    }

    /// Take the peaks published since the last frame (input and output
    /// linear, reduction in dB) into the history.
    pub fn record(&mut self, cx: &Ctx<'_>, input: usize, output: usize, reduction: Option<usize>) {
        self.since += cx.dt;
        let to_db = |v: f32| if v > 0.0 { 20.0 * v.log10() } else { -150.0 };
        let moment = [
            to_db(cx.tap.take_value(input)),
            to_db(cx.tap.take_value(output)),
            reduction.map_or(0.0, |r| cx.tap.take_value(r)),
        ];
        let step = 1.0 / self.rate;
        while self.since >= step {
            self.since -= step;
            self.data[self.head] = moment;
            self.head = (self.head + 1) % self.data.len();
        }
    }

    /// Oldest first.
    pub fn ordered(&self) -> Vec<[f32; 3]> {
        let n = self.data.len();
        (0..n).map(|i| self.data[(self.head + i) % n]).collect()
    }

    /// The largest of a field over the last `seconds`.
    pub fn recent_max(&self, field: usize, seconds: f32) -> f32 {
        let n = self.data.len();
        let k = ((seconds * self.rate) as usize).min(n);
        (1..=k)
            .map(|i| self.data[(self.head + n - i) % n][field])
            .fold(f32::NEG_INFINITY, f32::max)
    }

    /// Paint into `r`: the input filled from the bottom, the output as a
    /// line (or filled when `fill_output`), the reduction from the top
    /// (0 to `gr_range` dB), levels over `floor` to 0 dB.
    pub fn paint(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        cx: &Ctx<'_>,
        floor: f32,
        gr_range: f32,
        fill_output: bool,
    ) {
        let th = cx.theme;
        p.fill_rounded(r, 3.0, &Paint::Solid(th.device.display.darken(0.12)));
        let data = self.ordered();
        let n = data.len();
        let x = |i: usize| r.x + r.w * i as f32 / (n - 1).max(1) as f32;
        let y = |d: f32| r.bottom() - r.h * ((d - floor) / -floor).clamp(0.0, 1.0);
        let area = |field: usize| {
            let mut path = faderframe_ui_canvas::Path::new();
            path.move_to(Point::new(r.x, r.bottom()));
            for (i, m) in data.iter().enumerate() {
                path.line_to(Point::new(x(i), y(m[field])));
            }
            path.line_to(Point::new(r.right(), r.bottom())).close();
            path
        };
        p.fill_path(&area(0), th.device.post.with_alpha(0.16));
        if fill_output {
            p.fill_path(&area(1), th.device.wave.with_alpha(0.32));
        }
        let out: Vec<Point> = data
            .iter()
            .enumerate()
            .map(|(i, m)| Point::new(x(i), y(m[1])))
            .collect();
        p.stroke_path(
            &faderframe_ui_canvas::Path::polyline(&out),
            1.2,
            th.device
                .wave
                .with_alpha(if fill_output { 1.0 } else { 0.8 }),
        );
        if gr_range > 0.0 {
            let gy = |d: f32| r.y + r.h * (d / gr_range).clamp(0.0, 1.0);
            let mut gr = faderframe_ui_canvas::Path::new();
            gr.move_to(Point::new(r.x, r.y));
            for (i, m) in data.iter().enumerate() {
                gr.line_to(Point::new(x(i), gy(m[2])));
            }
            gr.line_to(Point::new(r.right(), r.y)).close();
            p.fill_path(&gr, th.device.reduction.with_alpha(0.35));
            let line: Vec<Point> = data
                .iter()
                .enumerate()
                .map(|(i, m)| Point::new(x(i), gy(m[2])))
                .collect();
            p.stroke_path(
                &faderframe_ui_canvas::Path::polyline(&line),
                1.6,
                th.device.reduction,
            );
            for d in [gr_range / 4.0, gr_range / 2.0, gr_range * 0.75] {
                p.text(
                    &format!("−{d:.0}"),
                    Rect::new(r.right() + 4.0, gy(d) - 7.0, 30.0, 14.0),
                    &TextStyle::new(th.fonts.tiny, th.device.reduction.with_alpha(0.8)),
                );
            }
        }
    }

    /// The y of a level in a history painted into `r` (for lines over it).
    pub fn level_y(r: Rect, db: f32, floor: f32) -> f32 {
        r.bottom() - r.h * ((db - floor) / -floor).clamp(0.0, 1.0)
    }
}

/// A large readout: a caption and a value.
pub(crate) fn readout(
    p: &mut dyn Painter,
    r: Rect,
    caption: &str,
    value: &str,
    color: Color,
    th: &Theme,
) {
    p.fill_rounded(r, 4.0, &Paint::Solid(th.device.display.darken(0.2)));
    p.text(
        caption,
        Rect::new(r.x, r.y + 3.0, r.w, 12.0),
        &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
            .bold()
            .center()
            .tracking(0.6),
    );
    p.text(
        value,
        Rect::new(r.x, r.y + 14.0, r.w, r.h - 16.0),
        &TextStyle::new(th.fonts.large, color).bold().center(),
    );
}

/// The spectra of what goes in and what comes out (from the tap's rings,
/// watched while painted), over 20 Hz to 20 kHz.
pub(crate) struct Spectrum {
    pre: crate::eq::analyser::Line,
    post: crate::eq::analyser::Line,
    rate: u32,
    scratch: [Vec<f32>; 2],
}

/// Display tilt (dB/oct round 1 kHz) so programme reads flat.
const TILT: f32 = 3.0;

impl Spectrum {
    pub fn new() -> Self {
        Self::at(48_000)
    }

    fn at(rate: u32) -> Self {
        let r = f64::from(rate.max(8_000));
        Self {
            pre: crate::eq::analyser::Line::new(4096, r),
            post: crate::eq::analyser::Line::new(4096, r),
            rate,
            scratch: [
                vec![0.0; faderframe_plugin_host::tap::RING_FRAMES],
                vec![0.0; faderframe_plugin_host::tap::RING_FRAMES],
            ],
        }
    }

    pub fn update(&mut self, cx: &Ctx<'_>) {
        cx.tap.watch();
        let rate = cx.model.sample_rate();
        if rate != self.rate {
            *self = Self::at(rate);
        }
        self.pre.feed(&cx.tap.input, &mut self.scratch, 36.0, false);
        self.post
            .feed(&cx.tap.output, &mut self.scratch, 36.0, false);
    }

    pub fn x_of(r: Rect, f: f64) -> f32 {
        r.x + r.w * ((f / 20.0).log10() / 3.0).clamp(0.0, 1.0) as f32
    }

    pub fn freq_at(r: Rect, x: f32) -> f64 {
        20.0 * 10f64.powf(3.0 * f64::from(((x - r.x) / r.w).clamp(0.0, 1.0)))
    }

    /// Paint the input (filled) and the output (a line) over `floor` to
    /// 0 dB, with the frequency grid.
    pub fn paint(&self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>, floor: f32, output: bool) {
        let th = cx.theme;
        p.fill_rounded(r, 3.0, &Paint::Solid(th.device.display.darken(0.12)));
        for f in [
            50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0,
        ] {
            let x = Self::x_of(r, f);
            p.vline(x, r.y, r.bottom(), th.device.grid);
            p.text(
                &if f >= 1_000.0 {
                    format!("{}k", f / 1_000.0)
                } else {
                    format!("{f}")
                },
                Rect::new(x + 3.0, r.bottom() - 14.0, 40.0, 12.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
            );
        }
        let n = (r.w / 2.0).max(2.0) as usize;
        let freqs: Vec<f64> = (0..n)
            .map(|i| Self::freq_at(r, r.x + r.w * i as f32 / (n - 1) as f32))
            .collect();
        let y = |d: f32| r.bottom() - r.h * ((d - floor) / -floor).clamp(0.0, 1.0);
        let tilt = |f: f64, d: f32| d + TILT * (f / 1_000.0).log2() as f32;
        let line = |l: &crate::eq::analyser::Line| -> Vec<Point> {
            l.curve(&freqs)
                .iter()
                .zip(&freqs)
                .enumerate()
                .map(|(i, (d, f))| {
                    Point::new(r.x + r.w * i as f32 / (n - 1) as f32, y(tilt(*f, *d)))
                })
                .collect()
        };
        let pre = line(&self.pre);
        let mut fill = faderframe_ui_canvas::Path::polyline(&pre);
        fill.line_to(Point::new(r.right(), r.bottom()))
            .line_to(Point::new(r.x, r.bottom()))
            .close();
        p.fill_path(&fill, th.device.pre.with_alpha(0.22));
        if output {
            p.stroke_path(
                &faderframe_ui_canvas::Path::polyline(&line(&self.post)),
                1.4,
                th.device.post,
            );
        }
    }
}

/// A sample's waveform across `r` (from its overview), the part from
/// `from` to `to` (0–1) of it.
pub(crate) fn waveform(
    p: &mut dyn Painter,
    r: Rect,
    sample: &faderframe_plugin_host::devices::samples::Sample,
    color: Color,
) {
    let o = &sample.overview;
    if o.is_empty() {
        return;
    }
    let mid = r.y + r.h / 2.0;
    let scale = r.h * 0.48 / sample.peak.max(0.05);
    let n = (r.w as usize).max(2);
    let mut top = Vec::with_capacity(n);
    let mut bottom = Vec::with_capacity(n);
    for i in 0..n {
        let a = i * o.len() / n;
        let b = ((i + 1) * o.len() / n).max(a + 1).min(o.len());
        let (lo, hi) = o[a..b]
            .iter()
            .fold((0.0f32, 0.0f32), |(l, h), (x, y)| (l.min(*x), h.max(*y)));
        let x = r.x + r.w * i as f32 / (n - 1) as f32;
        top.push(Point::new(x, mid - hi * scale));
        bottom.push(Point::new(x, mid - lo * scale));
    }
    let mut path = faderframe_ui_canvas::Path::polyline(&top);
    for q in bottom.iter().rev() {
        path.line_to(*q);
    }
    path.close();
    p.fill_path(&path, color.with_alpha(0.55));
    p.hline(r.x, r.right(), mid, color.with_alpha(0.3));
}

/// A button drawn in a display: a rounded box with its label.
pub(crate) fn display_button(p: &mut dyn Painter, r: Rect, label: &str, accent: Color, th: &Theme) {
    p.fill_rounded(r, 4.0, &Paint::Solid(accent.with_alpha(0.18)));
    p.stroke_rounded(r, 4.0, 1.0, accent.with_alpha(0.6));
    p.text(
        label,
        r,
        &TextStyle::new(th.fonts.tiny + 1.0, th.ui.text)
            .bold()
            .center(),
    );
}
