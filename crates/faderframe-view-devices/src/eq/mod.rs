//! The EQ's editor.
//!
//! A large response display with a live analyser fills the window; the
//! selected bands' controls float over it, under the bands; a bar above
//! holds A/B, undo and the sidechain source, a bar below the processing
//! mode, the instance list, the analyser, the character and the output.
//!
//! * The analyser shows what goes in (a line), what comes out (filled) and
//!   an external spectrum (outlined): the sidechain, or any other EQ's
//!   output. Where the output and the external spectrum crowd the same
//!   frequencies they glow red (collisions). Resolution, speed, range,
//!   tilt and freeze are settings; hovering still over the spectrum for a
//!   moment offers its peaks to grab and drag into bells (Spectrum Grab).
//! * Bands are created by clicking or double-clicking (the shape follows
//!   where: cuts at the ends, a notch low down, bells elsewhere), by
//!   dragging the overall curve (shelves at its ends), with Alt for a
//!   dynamic band and Alt+Shift for a spectral one, from the menus, by
//!   drawing a whole curve (EQ Sketch), or by EQ Match. Bands are selected
//!   by clicking (Ctrl adds, Shift a range) or by dragging a rectangle, and
//!   moved together; the wheel sets Q (a cut's slope), with Alt the dynamic
//!   range and with Ctrl the gain. Alt-click bypasses a band, Ctrl+Alt
//!   changes its shape, Alt+Shift its slope; double-click types a value
//!   ("1k", "A4", "C#2+13").
//! * Next to a band its values can be dragged, scrolled or typed, it can be
//!   heard on its own (hold the solo button or the middle button), bypassed
//!   or deleted.
//! * The piano display quantizes band frequencies to notes; the frequency
//!   scale zooms (drag up and down) and scrolls (drag sideways).
//! * Every change is a parameter edit of the plugin slot, so it is
//!   undoable and automatable like any other.

pub(crate) mod analyser;
mod bars;
mod edit;
mod geometry;
mod input;
mod instances;
mod matching;
mod paint;
mod panel;
mod sketch;

use crate::common::Device;
use analyser::Analyser;
use faderframe_core::PluginInstanceId;
use faderframe_plugin_host::eq::design::BandType;
use faderframe_plugin_host::eq::{BandParams, Field, PhaseMode, global};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{CanvasView, Color, Point, Rect, Size, Theme, ViewEvent};
use geometry::{F_MAX, F_MIN, FreqAxis, GainAxis, Layout};
use std::time::Instant;

/// The display's gain ranges.
pub(crate) const DISPLAY_RANGES: [f32; 4] = [3.0, 6.0, 12.0, 30.0];

/// The editor's own settings, kept per instance for the session
/// ([`Action::SetDeviceView`]).
pub(crate) mod key {
    pub const PRE: &str = "eq.analyser.pre";
    pub const POST: &str = "eq.analyser.post";
    pub const EXTERNAL: &str = "eq.analyser.external";
    /// −1: the sidechain; otherwise another EQ's instance id.
    pub const SOURCE: &str = "eq.analyser.source";
    pub const RANGE: &str = "eq.analyser.range";
    pub const RESOLUTION: &str = "eq.analyser.resolution";
    pub const SPEED: &str = "eq.analyser.speed";
    pub const TILT: &str = "eq.analyser.tilt";
    pub const FREEZE: &str = "eq.analyser.freeze";
    pub const GRAB: &str = "eq.analyser.grab";
    pub const COLLISIONS: &str = "eq.analyser.collisions";
    pub const DISPLAY: &str = "eq.display.range";
    pub const PIANO: &str = "eq.display.piano";
    pub const SKETCH: &str = "eq.sketch";
    /// −1: the sidechain; −2: the input, recorded earlier; otherwise
    /// another EQ's instance id.
    pub const MATCH_REFERENCE: &str = "eq.match.reference";
}

/// Where the external spectrum comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Sidechain,
    Instance(PluginInstanceId),
}

impl Source {
    pub fn from_value(v: f64) -> Self {
        if v < 0.0 {
            Source::Sidechain
        } else {
            Source::Instance(PluginInstanceId(v as u64))
        }
    }

    pub fn value(self) -> f64 {
        match self {
            Source::Sidechain => -1.0,
            Source::Instance(p) => p.0 as f64,
        }
    }
}

/// The editor's settings as they stand.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Settings {
    pub pre: bool,
    pub post: bool,
    pub external: bool,
    pub source: Source,
    pub range: f32,
    pub resolution: usize,
    pub speed: usize,
    pub tilt: f32,
    pub freeze: bool,
    pub grab: bool,
    pub collisions: bool,
    pub display: f32,
    pub piano: bool,
    pub sketch: bool,
}

/// What a value next to a band is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueField {
    Freq,
    Gain,
    Q,
    Slope,
}

impl ValueField {
    pub fn field(self) -> Field {
        match self {
            ValueField::Freq => Field::Freq,
            ValueField::Gain => Field::Gain,
            ValueField::Q => Field::Q,
            ValueField::Slope => Field::Slope,
        }
    }
}

/// The buttons next to a band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeButton {
    Bypass,
    Solo,
    Delete,
    Menu,
}

/// What is under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Hit {
    Node(usize),
    /// A dynamic band's range handle.
    Range(usize),
    Value(usize, ValueField),
    Button(usize, NodeButton),
    /// A peak Spectrum Grab offers (frequency, level).
    Peak(f64, f32),
    Graph(Point),
    Axis(Point),
    PianoDot(usize),
    Piano(Point),
    Top(bars::TopItem),
    Bottom(bars::BottomItem),
    Output(bars::OutputItem),
    Panel(panel::PanelItem),
    Instances(instances::Hit),
    Match(matching::Hit),
}

/// What a press on a node does if it turns out to be a click (no move).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Click {
    Bypass,
    /// Ctrl-click: drops the band if it was selected before the press.
    ToggleSelected(bool),
}

/// What a drag does.
pub(crate) enum Drag {
    /// The selected bands from where they were; `anchor` is the one held.
    Bands {
        anchor: usize,
        start: Point,
        from: Vec<(usize, BandParams)>,
        /// Alt: only frequency (`Some(true)`) or only gain, decided by the
        /// first move.
        constrain: Option<Option<bool>>,
        /// Ctrl: Q instead of gain.
        q: bool,
        /// Whether it has moved (the gesture starts then), or already began
        /// (a band made by this press).
        moved: bool,
        began: bool,
        click: Option<Click>,
    },
    /// A slider of the band controls (threshold, density): the selected
    /// bands follow the pointer along `track`.
    Slider { field: Field, track: Rect },
    /// A dynamic band's range handle.
    Range {
        band: usize,
        start: Point,
        from: f64,
    },
    /// A value next to a band, or a knob of the band controls: the anchor
    /// band's value moves with the pointer, the other selected bands by as
    /// much.
    Value {
        field: Field,
        anchor: usize,
        start_y: f32,
        from: Vec<(usize, f64)>,
    },
    /// A global setting dragged (output gain, pan, gain scale).
    Global {
        index: usize,
        start: Point,
        from: f64,
        horizontal: bool,
    },
    Lasso {
        start: Point,
        now: Point,
        base: Vec<usize>,
    },
    Sketch {
        stroke: sketch::Stroke,
        slots: Vec<usize>,
        shown: usize,
    },
    /// A press on the background that is not a drag yet: a click creates
    /// or deselects, a drag selects (or sketches).
    Pending { start: Point, sketch: bool },
    /// The frequency scale: up and down zoom, sideways scrolls.
    Axis {
        start: Point,
        from: FreqAxis,
        at: f64,
    },
    /// A band's dot on the piano: moves by semitones.
    Piano {
        band: usize,
        start: Point,
        from: f64,
    },
}

/// A/B: the side in effect and the stored settings.
pub struct EqView {
    device: Device,
    theme: Theme,
    /// Selected bands, in the order they were selected.
    selected: Vec<usize>,
    /// The band the controls show.
    focus: Option<usize>,
    hover: Option<Hit>,
    pointer: Option<Point>,
    drag: Option<Drag>,
    analyser: Option<Analyser>,
    axis: FreqAxis,
    listening: bool,
    /// The output settings panel is open.
    output_open: bool,
    instances: Option<instances::List>,
    matching: Option<matching::Match>,
    /// Spectrum Grab: when the pointer came to rest, and the frozen peaks
    /// while it is on.
    rest: Option<(Point, Instant)>,
    grab: Option<Vec<(f64, f32)>>,
    ab: [Option<Vec<f64>>; 2],
    ab_side: usize,
    /// The output meter's falling peaks.
    meter: [f32; 2],
    last_paint: Option<Instant>,
}

impl EqView {
    pub fn new(plugin: PluginInstanceId, theme: &Theme) -> Self {
        Self {
            device: Device::new(plugin),
            theme: theme.clone(),
            selected: Vec::new(),
            focus: None,
            hover: None,
            pointer: None,
            drag: None,
            analyser: None,
            axis: FreqAxis {
                lo: F_MIN,
                hi: F_MAX,
            },
            listening: false,
            output_open: false,
            instances: None,
            matching: None,
            rest: None,
            grab: None,
            ab: [None, None],
            ab_side: 0,
            meter: [-150.0; 2],
            last_paint: None,
        }
    }

    pub(crate) fn settings(&self, model: &Session) -> Settings {
        let v = |k: &str, d: f64| model.device_view(self.device.plugin, k).unwrap_or(d);
        let on = |k: &str, d: bool| v(k, f64::from(u8::from(d))) >= 0.5;
        let pick =
            |k: &str, d: usize, n: usize| (v(k, d as f64).round().max(0.0) as usize).min(n - 1);
        Settings {
            pre: on(key::PRE, true),
            post: on(key::POST, true),
            external: on(key::EXTERNAL, false),
            source: Source::from_value(v(key::SOURCE, -1.0)),
            range: analyser::RANGES[pick(key::RANGE, 1, analyser::RANGES.len())],
            resolution: pick(key::RESOLUTION, 2, analyser::RESOLUTIONS.len()),
            speed: pick(key::SPEED, 2, analyser::SPEEDS.len()),
            tilt: analyser::TILTS[pick(key::TILT, 3, analyser::TILTS.len())],
            freeze: on(key::FREEZE, false),
            grab: on(key::GRAB, true),
            collisions: on(key::COLLISIONS, true),
            display: DISPLAY_RANGES[pick(key::DISPLAY, 2, DISPLAY_RANGES.len())],
            piano: on(key::PIANO, false),
            sketch: on(key::SKETCH, false),
        }
    }

    /// An action changing one of the editor's settings.
    pub(crate) fn view_action(&self, key: &str, value: f64) -> Action {
        self.view_actions(&[(key, value)])
    }

    /// An action changing several of the editor's settings.
    pub(crate) fn view_actions(&self, values: &[(&str, f64)]) -> Action {
        Action::SetDeviceView {
            plugin: self.device.plugin,
            values: values.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
        }
    }

    pub(crate) fn rate(model: &Session) -> f64 {
        f64::from(model.sample_rate().max(8_000))
    }

    /// The highest frequency shown at this rate.
    pub(crate) fn top(model: &Session) -> f64 {
        F_MAX.min(0.5 * Self::rate(model) * 0.999)
    }

    pub(crate) fn layout(&self, size: Size, model: &Session) -> Layout {
        Layout::new(size, self.settings(model).piano)
    }

    pub(crate) fn gain_axis(&self, model: &Session) -> GainAxis {
        GainAxis {
            range: self.settings(model).display,
        }
    }

    /// The visible axis, inside the range this rate can show.
    pub(crate) fn axis(&self, model: &Session) -> FreqAxis {
        let top = Self::top(model);
        FreqAxis {
            lo: self.axis.lo.max(F_MIN),
            hi: self.axis.hi.min(top),
        }
    }

    pub(crate) fn mode(tap: &AnalysisTap) -> PhaseMode {
        PhaseMode::of(&tap.params)
    }

    pub(crate) fn scale(tap: &AnalysisTap) -> f64 {
        f64::from(tap.params.get(global::GAIN_SCALE))
    }

    pub(crate) fn interact(tap: &AnalysisTap) -> bool {
        tap.params.get(global::GAIN_Q) >= 0.5
    }

    pub(crate) fn band_color(band: usize) -> Color {
        let c = faderframe_project::TrackColor::palette(band);
        Color::rgb8(c.r, c.g, c.b)
    }

    // --- selection -------------------------------------------------------------

    pub(crate) fn select_only(&mut self, band: usize) {
        self.selected = vec![band];
        self.focus = Some(band);
    }

    pub(crate) fn is_selected(&self, band: usize) -> bool {
        self.selected.contains(&band)
    }

    pub(crate) fn toggle(&mut self, band: usize) {
        if let Some(i) = self.selected.iter().position(|b| *b == band) {
            self.selected.remove(i);
            if self.focus == Some(band) {
                self.focus = self.selected.last().copied();
            }
        } else {
            self.selected.push(band);
            self.focus = Some(band);
        }
    }

    /// Select from the focused band to `band` (by frequency).
    pub(crate) fn select_range(&mut self, model: &Session, band: usize) {
        let Some((_, bands)) = self.used(model) else {
            return;
        };
        let Some(from) = self.focus else {
            self.select_only(band);
            return;
        };
        let freq = |b: usize| {
            bands
                .iter()
                .find(|(i, _)| *i == b)
                .map_or(0.0, |(_, p)| p.freq)
        };
        let (lo, hi) = {
            let (a, b) = (freq(from), freq(band));
            (a.min(b), a.max(b))
        };
        for (b, p) in &bands {
            if p.freq >= lo && p.freq <= hi && !self.selected.contains(b) {
                self.selected.push(*b);
            }
        }
        self.focus = Some(band);
    }

    /// Drop selected bands that are no longer in use.
    fn tidy(&mut self, tap: &AnalysisTap) {
        self.selected
            .retain(|b| BandParams::read(&tap.params, *b).used);
        if self
            .focus
            .is_some_and(|f| !BandParams::read(&tap.params, f).used)
        {
            self.focus = self.selected.last().copied();
        }
    }

    fn listen(&mut self, model: &Session, setting: Option<usize>) {
        if let Some(tap) = self.device.tap(model) {
            tap.set_listen(setting);
        }
        self.listening = setting.is_some();
    }

    /// Where a new band at a point of the display would be, and its shape.
    pub(crate) fn shape_at(&self, model: &Session, size: Size, pos: Point) -> (BandType, f64, f64) {
        let l = self.layout(size, model);
        let g = &l.graph;
        let axis = self.axis(model);
        let freq = axis.f(g, pos.x).clamp(F_MIN, F_MAX);
        let gain = f64::from(self.gain_axis(model).db(g, pos.y)).clamp(-30.0, 30.0);
        let tx = (pos.x - g.x) / g.w;
        let ty = (pos.y - g.y) / g.h;
        let kind = if freq < 30.0 || tx < 0.04 {
            BandType::LowCut
        } else if freq > 16_000.0 || tx > 0.96 {
            BandType::HighCut
        } else if ty > 0.86 {
            BandType::Notch
        } else {
            BandType::Bell
        };
        (kind, freq, (gain * 10.0).round() / 10.0)
    }
}

impl CanvasView<Session, Action> for EqView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(
        &mut self,
        p: &mut dyn faderframe_ui_canvas::Painter,
        size: Size,
        model: &Session,
        _theme: &Theme,
    ) {
        self.paint_all(p, size, model);
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut faderframe_ui_canvas::EventCx<'_, Action>,
    ) -> bool {
        if let Some(tap) = self.device.tap(model) {
            self.tidy(&tap);
        }
        self.handle(ev, size, model, cx)
    }

    fn wants_frames(&self, _model: &Session) -> bool {
        true
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        self.tip(pos, size, model)
    }

    fn min_size(&self) -> Size {
        Size::new(760.0, 420.0)
    }
}

/// A rectangle's centre-bottom (where menus drop from).
pub(crate) fn below(r: &Rect) -> Point {
    Point::new(r.x, r.bottom())
}

#[cfg(test)]
mod tests;
