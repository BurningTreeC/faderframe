//! The bars above and below the display, their menus, and the output
//! settings panel.

use super::analyser::{RANGES, RESOLUTION_NAMES, SPEEDS, TILTS};
use super::geometry::Layout;
use super::{DISPLAY_RANGES, EqView, Source, below, instances, key};
use faderframe_plugin_host::eq::character::Character;
use faderframe_plugin_host::eq::{PhaseMode, QUALITIES, format_pan, global, global_id, latency};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_project::Command;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::controls::{self, KnobLook};
use faderframe_ui_canvas::{Color, HostRequest, MenuItem, Paint, Painter, Point, Rect, TextStyle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TopItem {
    Undo,
    Redo,
    A,
    B,
    CopyAb,
    Sketch,
    Match,
    Sidechain,
    Range,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BottomItem {
    Piano,
    Mode,
    Instances,
    Analyser,
    Character,
    AutoGain,
    Bypass,
    Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputItem {
    Gain,
    Pan,
    PanMode,
    Invert,
    AutoGain,
    Scale,
    Panel,
}

impl EqView {
    pub(crate) fn top_items(&self, top: &Rect) -> Vec<(TopItem, Rect)> {
        let y = top.y + 4.0;
        let h = top.h - 8.0;
        let mut out = Vec::new();
        let mut x = top.x + 8.0;
        for (item, w) in [
            (TopItem::Undo, 46.0),
            (TopItem::Redo, 46.0),
            (TopItem::A, 26.0),
            (TopItem::B, 26.0),
            (TopItem::CopyAb, 44.0),
            (TopItem::Sketch, 64.0),
            (TopItem::Match, 76.0),
        ] {
            let gap = if matches!(item, TopItem::A | TopItem::Sketch) {
                12.0
            } else {
                4.0
            };
            x += gap - 4.0;
            out.push((item, Rect::new(x, y, w, h)));
            x += w + 4.0;
        }
        let mut rx = top.right() - 8.0;
        for (item, w) in [(TopItem::Range, 74.0), (TopItem::Sidechain, 190.0)] {
            rx -= w;
            out.push((item, Rect::new(rx.max(x), y, w, h)));
            rx -= 6.0;
        }
        out
    }

    pub(crate) fn bottom_items(&self, bottom: &Rect) -> Vec<(BottomItem, Rect)> {
        let y = bottom.y + 5.0;
        let h = bottom.h - 10.0;
        let mut out = vec![
            (BottomItem::Piano, Rect::new(bottom.x + 8.0, y, 52.0, h)),
            (BottomItem::Mode, Rect::new(bottom.x + 66.0, y, 196.0, h)),
        ];
        let w = 210.0;
        out.push((
            BottomItem::Instances,
            Rect::new(bottom.x + (bottom.w - w) / 2.0, y, w, h),
        ));
        let mut rx = bottom.right() - 8.0;
        for (item, w) in [
            (BottomItem::Output, 118.0),
            (BottomItem::Bypass, 62.0),
            (BottomItem::AutoGain, 78.0),
            (BottomItem::Character, 92.0),
            (BottomItem::Analyser, 118.0),
        ] {
            rx -= w;
            out.push((item, Rect::new(rx, y, w, h)));
            rx -= 6.0;
        }
        out
    }

    /// The output panel, over the Output button.
    pub(crate) fn output_rect(&self, l: &Layout) -> Rect {
        let w = 300.0;
        let h = 156.0;
        Rect::new(l.bottom.right() - w - 8.0, l.bottom.y - h - 4.0, w, h)
    }

    pub(crate) fn output_items(&self, r: &Rect) -> Vec<(OutputItem, Rect)> {
        vec![
            (
                OutputItem::Gain,
                Rect::new(r.x + 16.0, r.y + 28.0, 64.0, 74.0),
            ),
            (
                OutputItem::Pan,
                Rect::new(r.x + 92.0, r.y + 28.0, 64.0, 74.0),
            ),
            (
                OutputItem::Invert,
                Rect::new(r.x + 172.0, r.y + 30.0, 116.0, 22.0),
            ),
            (
                OutputItem::AutoGain,
                Rect::new(r.x + 172.0, r.y + 56.0, 116.0, 22.0),
            ),
            (
                OutputItem::PanMode,
                Rect::new(r.x + 172.0, r.y + 82.0, 116.0, 22.0),
            ),
            (
                OutputItem::Scale,
                Rect::new(r.x + 16.0, r.y + 122.0, r.w - 32.0, 18.0),
            ),
        ]
    }

    pub(crate) fn button(&self, p: &mut dyn Painter, r: Rect, label: &str, on: bool) {
        self.button_tinted(p, r, label, on, self.theme.ui.accent);
    }

    pub(crate) fn button_tinted(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        label: &str,
        on: bool,
        tint: Color,
    ) {
        let th = &self.theme;
        let bg = if on {
            tint.with_alpha(0.35)
        } else {
            th.ui.surface_alt
        };
        p.fill_rounded(r, 4.0, &Paint::Solid(bg));
        p.stroke_rounded(
            r,
            4.0,
            1.0,
            if on {
                tint.with_alpha(0.8)
            } else {
                th.ui.border
            },
        );
        p.text(
            label,
            r,
            &TextStyle::new(th.fonts.small, th.ui.text).center(),
        );
    }

    fn sidechain_label(&self, model: &Session) -> String {
        let Some((_, slot)) = model.plugin_slot(self.device.plugin) else {
            return "Sidechain: —".into();
        };
        let name = slot
            .sidechain
            .and_then(|t| model.project().track(t))
            .map_or_else(|| "None".to_string(), |t| t.name.clone());
        format!("Sidechain: {name} ▾")
    }

    pub(crate) fn paint_top(&self, p: &mut dyn Painter, l: &Layout, model: &Session) {
        let th = &self.theme;
        p.fill(l.top, th.ui.surface);
        p.hline(0.0, l.top.w, l.top.bottom() - 0.5, th.ui.border);
        let s = self.settings(model);
        for (item, r) in self.top_items(&l.top) {
            let (label, on) = match item {
                TopItem::Undo => ("Undo".to_string(), false),
                TopItem::Redo => ("Redo".to_string(), false),
                TopItem::A => ("A".into(), self.ab_side == 0),
                TopItem::B => ("B".into(), self.ab_side == 1),
                TopItem::CopyAb => (if self.ab_side == 0 { "A→B" } else { "B→A" }.into(), false),
                TopItem::Sketch => ("Sketch".into(), s.sketch),
                TopItem::Match => ("EQ Match".into(), self.matching.is_some()),
                TopItem::Sidechain => (self.sidechain_label(model), false),
                TopItem::Range => (format!("±{:.0} dB ▾", s.display), false),
            };
            self.button(p, r, &label, on);
        }
    }

    pub(crate) fn paint_bottom(
        &self,
        p: &mut dyn Painter,
        l: &Layout,
        model: &Session,
        tap: &AnalysisTap,
    ) {
        let th = &self.theme;
        p.fill(l.bottom, th.ui.surface);
        p.hline(0.0, l.bottom.w, l.bottom.y + 0.5, th.ui.border);
        let s = self.settings(model);
        let v = |g: usize| tap.params.get(g);
        let mode = Self::mode(tap);
        for (item, r) in self.bottom_items(&l.bottom) {
            match item {
                BottomItem::Piano => self.button(p, r, "Piano", s.piano),
                BottomItem::Mode => {
                    let q = faderframe_plugin_host::eq::quality(&tap.params);
                    let label = match mode {
                        PhaseMode::Linear => format!("Linear · {} ▾", QUALITIES[q]),
                        m => format!("{} ▾", m.name()),
                    };
                    self.button(p, r, &label, false);
                    // The latency it costs.
                    let lat = latency(&tap.params);
                    if lat > 0 {
                        let ms = f64::from(lat) / Self::rate(model) * 1000.0;
                        p.text(
                            &format!("{ms:.1} ms"),
                            Rect::new(r.right() + 6.0, r.y, 70.0, r.h),
                            &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
                        );
                    }
                }
                BottomItem::Instances => {
                    let label = model
                        .plugin_slot(self.device.plugin)
                        .map_or_else(|| "EQ".to_string(), |(t, _)| format!("{} · EQ ▴", t.name));
                    self.button(p, r, &label, self.instances.is_some());
                }
                BottomItem::Analyser => {
                    let mut parts = Vec::new();
                    if s.pre {
                        parts.push("Pre");
                    }
                    if s.post {
                        parts.push("Post");
                    }
                    if s.external {
                        parts.push("Ext");
                    }
                    let label = if parts.is_empty() {
                        "Analyser Off ▾".to_string()
                    } else {
                        format!("{} ▾", parts.join(" "))
                    };
                    self.button(p, r, &label, false);
                    if s.freeze {
                        // A line over the button while frozen.
                        p.fill(Rect::new(r.x + 4.0, r.y, r.w - 8.0, 2.0), th.device.side);
                    }
                }
                BottomItem::Character => {
                    let c = Character::from_index(v(global::CHARACTER).round().max(0.0) as usize);
                    self.button(p, r, &format!("{} ▾", c.name()), c != Character::Clean);
                }
                BottomItem::AutoGain => {
                    self.button(p, r, "Auto Gain", v(global::AUTO_GAIN) >= 0.5);
                }
                BottomItem::Bypass => {
                    self.button_tinted(
                        p,
                        r,
                        "Bypass",
                        v(global::BYPASS) >= 0.5,
                        th.device.dyn_range,
                    );
                }
                BottomItem::Output => {
                    let mut label = format!(
                        "Out {}",
                        super::geometry::db_text(f64::from(v(global::OUTPUT)))
                    );
                    if v(global::INVERT) >= 0.5 {
                        label = format!("Ø {label}");
                    }
                    self.button(p, r, &label, self.output_open);
                }
            }
        }
    }

    pub(crate) fn paint_output(&self, p: &mut dyn Painter, l: &Layout, tap: &AnalysisTap) {
        if !self.output_open {
            return;
        }
        let th = &self.theme;
        let r = self.output_rect(l);
        p.shadow(r, 6.0, Color::rgba(0.0, 0.0, 0.0, 0.45), 0.0, 3.0, 14.0);
        p.fill_rounded(r, 6.0, &Paint::Solid(th.device.panel));
        p.stroke_rounded(r, 6.0, 1.0, th.device.panel_edge);
        p.text(
            "OUTPUT",
            Rect::new(r.x + 14.0, r.y + 6.0, 120.0, 16.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                .bold()
                .tracking(0.8),
        );
        let v = |g: usize| f64::from(tap.params.get(g));
        let mid_side = v(global::PAN_MODE) >= 0.5;
        for (item, ir) in self.output_items(&r) {
            match item {
                OutputItem::Gain | OutputItem::Pan => {
                    let (value, t, label, bipolar) = if item == OutputItem::Gain {
                        let g = v(global::OUTPUT);
                        (
                            format!("{g:+.1} dB").replace('-', "−"),
                            ((g + 36.0) / 72.0) as f32,
                            "GAIN",
                            true,
                        )
                    } else {
                        let pan = v(global::PAN);
                        (
                            format_pan(pan, mid_side),
                            ((pan + 1.0) / 2.0) as f32,
                            "PAN",
                            true,
                        )
                    };
                    self.small_knob(p, ir, label, &value, t, bipolar, th.device.curve);
                }
                OutputItem::Invert => self.button(p, ir, "Phase Invert", v(global::INVERT) >= 0.5),
                OutputItem::AutoGain => {
                    self.button(p, ir, "Auto Gain", v(global::AUTO_GAIN) >= 0.5)
                }
                OutputItem::PanMode => self.button(
                    p,
                    ir,
                    if mid_side {
                        "Pan: Mid/Side"
                    } else {
                        "Pan: Left/Right"
                    },
                    false,
                ),
                OutputItem::Scale => {
                    let scale = v(global::GAIN_SCALE);
                    p.text(
                        &format!("Gain Scale {:.0} %", scale * 100.0),
                        Rect::new(ir.x, ir.y - 15.0, ir.w, 14.0),
                        &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
                    );
                    let track = Rect::new(ir.x, ir.y + ir.h / 2.0 - 2.0, ir.w, 4.0);
                    p.fill_rounded(track, 2.0, &Paint::Solid(th.ui.surface_alt));
                    let t = (scale / 2.0) as f32;
                    p.fill_rounded(
                        Rect::new(track.x, track.y, track.w * t, track.h),
                        2.0,
                        &Paint::Solid(th.device.curve.with_alpha(0.8)),
                    );
                    p.circle(
                        Point::new(track.x + track.w * t, track.y + 2.0),
                        6.0,
                        th.ui.text,
                    );
                }
                OutputItem::Panel => {}
            }
        }
    }

    /// A small knob with its name above and value below.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn small_knob(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        name: &str,
        value: &str,
        t: f32,
        bipolar: bool,
        ring: Color,
    ) {
        let th = &self.theme;
        let size = (r.w - 16.0).min(r.h - 34.0).max(16.0);
        let k = Rect::new(r.x + (r.w - size) / 2.0, r.y + 15.0, size, size);
        controls::knob(
            p,
            k,
            t.clamp(0.0, 1.0),
            bipolar,
            KnobLook {
                cap: th.console.knob.cap_top,
                ring,
            },
            th,
        );
        p.text(
            name,
            Rect::new(r.x - 6.0, r.y, r.w + 12.0, 13.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                .bold()
                .center()
                .tracking(0.6),
        );
        p.text(
            value,
            Rect::new(r.x - 8.0, k.bottom() + 2.0, r.w + 16.0, 14.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center(),
        );
    }

    // --- menus -----------------------------------------------------------------

    fn param_item(
        &self,
        model: &Session,
        label: impl Into<String>,
        g: usize,
        value: f64,
    ) -> MenuItem<Action> {
        let label = label.into();
        match self.action(model, global_id(g), value) {
            Some(a) => MenuItem::new(label, a),
            None => MenuItem::disabled(label),
        }
    }

    fn view_item(&self, label: impl Into<String>, key: &str, value: f64) -> MenuItem<Action> {
        MenuItem::new(label, self.view_action(key, value))
    }

    pub(crate) fn mode_menu(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        r: &Rect,
    ) -> HostRequest<Action> {
        let mode = Self::mode(tap);
        let mut items: Vec<_> = PhaseMode::MENU
            .iter()
            .map(|m| {
                self.param_item(model, m.name(), global::PHASE, m.value())
                    .checked(mode == *m)
            })
            .collect();
        let q = faderframe_plugin_host::eq::quality(&tap.params);
        for (i, name) in QUALITIES.iter().enumerate() {
            let item = self
                .param_item(
                    model,
                    format!("Resolution: {name}"),
                    global::QUALITY,
                    i as f64,
                )
                .checked(q == i);
            items.push(if i == 0 { item.separated() } else { item });
        }
        HostRequest::ContextMenu {
            at: below(r),
            items,
        }
    }

    pub(crate) fn character_menu(
        &self,
        model: &Session,
        tap: &AnalysisTap,
        r: &Rect,
    ) -> HostRequest<Action> {
        let now = tap.params.get(global::CHARACTER).round() as usize;
        let items = Character::ALL
            .iter()
            .map(|c| {
                self.param_item(model, c.name(), global::CHARACTER, c.index() as f64)
                    .checked(now == c.index())
            })
            .collect();
        HostRequest::ContextMenu {
            at: below(r),
            items,
        }
    }

    pub(crate) fn range_menu(&self, model: &Session, r: &Rect) -> HostRequest<Action> {
        let now = self.settings(model).display;
        let items = DISPLAY_RANGES
            .iter()
            .enumerate()
            .map(|(i, d)| {
                self.view_item(format!("±{d:.0} dB"), key::DISPLAY, i as f64)
                    .checked(now == *d)
            })
            .collect();
        HostRequest::ContextMenu {
            at: below(r),
            items,
        }
    }

    pub(crate) fn sidechain_menu(&self, model: &Session, r: &Rect) -> Option<HostRequest<Action>> {
        let (track, slot) = model.plugin_slot(self.device.plugin)?;
        let set = |source| {
            Action::Edit(Command::SetPluginSidechain {
                track: track.id,
                plugin: self.device.plugin,
                source,
            })
        };
        let mut items = vec![MenuItem::new("None", set(None)).checked(slot.sidechain.is_none())];
        for (id, name) in model.sidechain_sources(self.device.plugin) {
            items.push(MenuItem::new(name, set(Some(id))).checked(slot.sidechain == Some(id)));
        }
        Some(HostRequest::ContextMenu {
            at: below(r),
            items,
        })
    }

    pub(crate) fn analyser_menu(&self, model: &Session, r: &Rect) -> HostRequest<Action> {
        let s = self.settings(model);
        let flip = |on: bool| if on { 0.0 } else { 1.0 };
        let mut items = vec![
            self.view_item("Pre-EQ", key::PRE, flip(s.pre))
                .checked(s.pre),
            self.view_item("Post-EQ", key::POST, flip(s.post))
                .checked(s.post),
            self.view_item("External Spectrum", key::EXTERNAL, flip(s.external))
                .checked(s.external),
        ];
        // Where the external spectrum comes from.
        let source = |label: String, src: Source| {
            let a = self.view_actions(&[(key::SOURCE, src.value()), (key::EXTERNAL, 1.0)]);
            MenuItem::new(label, a).checked(s.external && s.source == src)
        };
        items.push(source("External: Side Chain".into(), Source::Sidechain).separated());
        for (id, label, _) in instances::instances(model) {
            if id != self.device.plugin {
                items.push(source(format!("External: {label}"), Source::Instance(id)));
            }
        }
        for (i, range) in RANGES.iter().enumerate() {
            let item = self
                .view_item(format!("Range: {range:.0} dB"), key::RANGE, i as f64)
                .checked(s.range == *range);
            items.push(if i == 0 { item.separated() } else { item });
        }
        for (i, name) in RESOLUTION_NAMES.iter().enumerate() {
            let item = self
                .view_item(format!("Resolution: {name}"), key::RESOLUTION, i as f64)
                .checked(s.resolution == i);
            items.push(if i == 0 { item.separated() } else { item });
        }
        for (i, (name, _)) in SPEEDS.iter().enumerate() {
            let item = self
                .view_item(format!("Speed: {name}"), key::SPEED, i as f64)
                .checked(s.speed == i);
            items.push(if i == 0 { item.separated() } else { item });
        }
        for (i, tilt) in TILTS.iter().enumerate() {
            let item = self
                .view_item(format!("Tilt: {tilt:.1} dB/oct"), key::TILT, i as f64)
                .checked(s.tilt == *tilt);
            items.push(if i == 0 { item.separated() } else { item });
        }
        items.push(
            self.view_item("Freeze", key::FREEZE, flip(s.freeze))
                .checked(s.freeze)
                .separated(),
        );
        items.push(
            self.view_item("Spectrum Grab", key::GRAB, flip(s.grab))
                .checked(s.grab),
        );
        items.push(
            self.view_item("Show Collisions", key::COLLISIONS, flip(s.collisions))
                .checked(s.collisions),
        );
        HostRequest::ContextMenu {
            at: below(r),
            items,
        }
    }
}
