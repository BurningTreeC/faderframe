//! EQ Match in the editor: listen to this EQ's input and to a reference —
//! the sidechain, another EQ's output, or this input recorded earlier
//! (another part of the song, an earlier take) — build both long-term
//! spectra, show their difference, and propose bands that make the input
//! sound like the reference ([`faderframe_plugin_host::eq::matching`]).
//! More or fewer bands follow the difference more or less closely; Apply
//! adds them as one undo step.

use super::analyser::Line;
use super::geometry::{FreqAxis, GainAxis, Layout};
use super::key;
use faderframe_core::PluginInstanceId;
use faderframe_plugin_host::eq::design::{AnalogBand, BandShape};
use faderframe_plugin_host::eq::matching;
use faderframe_plugin_host::tap::{AnalysisTap, RING_FRAMES};
use faderframe_session::Session;
use faderframe_ui_canvas::{Color, Paint, Painter, Path, Point, Rect, TextStyle, Theme};

/// Seconds of both spectra before matching makes sense.
pub(crate) const ENOUGH: f64 = 2.0;
/// Grid points per octave.
const PER_OCTAVE: f64 = 12.0;

/// Where the reference comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reference {
    Sidechain,
    /// This EQ's input, recorded at another time.
    Input,
    Instance(PluginInstanceId),
}

impl Reference {
    pub fn from_value(v: f64) -> Self {
        if v <= -1.5 {
            Reference::Input
        } else if v < 0.0 {
            Reference::Sidechain
        } else {
            Reference::Instance(PluginInstanceId(v as u64))
        }
    }

    pub fn value(self) -> f64 {
        match self {
            Reference::Sidechain => -1.0,
            Reference::Input => -2.0,
            Reference::Instance(p) => p.0 as f64,
        }
    }

    pub fn of(model: &Session, plugin: PluginInstanceId) -> Self {
        Self::from_value(
            model
                .device_view(plugin, key::MATCH_REFERENCE)
                .unwrap_or(-1.0),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hit {
    Reference,
    RecordInput,
    RecordReference,
    Match,
    Fewer,
    More,
    Apply,
    Back,
    Cancel,
    Body,
}

pub(crate) struct Match {
    pub input: Line,
    pub reference: Line,
    /// The reference the lines were filled from (they start over when it
    /// changes).
    from: Option<Reference>,
    pub result: Option<Vec<BandShape>>,
    pub count: usize,
    pub freqs: Vec<f64>,
    pub target: Option<Vec<f64>>,
    scratch: [Vec<f32>; 2],
}

impl Match {
    pub fn new(rate: u32) -> Self {
        let r = f64::from(rate.max(8_000));
        let mut input = Line::new(8192, r);
        input.averaging = true;
        let mut reference = Line::new(8192, r);
        reference.averaging = true;
        Self {
            input,
            reference,
            from: None,
            result: None,
            count: 8,
            freqs: matching::grid(25.0, 18_000.0f64.min(0.45 * r), PER_OCTAVE),
            target: None,
            scratch: [vec![0.0; RING_FRAMES], vec![0.0; RING_FRAMES]],
        }
    }

    /// Feed the recordings and keep the target up to date.
    pub fn update(&mut self, model: &Session, tap: &AnalysisTap, plugin: PluginInstanceId) {
        let reference = Reference::of(model, plugin);
        if self.from != Some(reference) {
            // A new reference: start it over (the input keeps its record).
            self.from = Some(reference);
            self.reference.reset_average();
            self.reference.averaging = reference != Reference::Input;
            if reference == Reference::Input {
                self.input.averaging = false;
            }
            self.result = None;
        }
        if self.result.is_some() {
            return;
        }
        if self.input.averaging {
            self.input.feed(&tap.input, &mut self.scratch, 40.0, false);
        }
        if self.reference.averaging {
            match reference {
                Reference::Sidechain => {
                    self.reference
                        .feed(&tap.sidechain, &mut self.scratch, 40.0, false)
                }
                Reference::Input => self
                    .reference
                    .feed(&tap.input, &mut self.scratch, 40.0, false),
                Reference::Instance(id) => {
                    if let Some(other) = model.plugin_tap(id) {
                        other.watch();
                        self.reference
                            .feed(&other.output, &mut self.scratch, 40.0, false);
                    }
                }
            }
        }
        self.target = match (
            self.input.average(&self.freqs),
            self.reference.average(&self.freqs),
        ) {
            (Some(a), Some(b))
                if self.input.averaged_seconds() >= 0.5
                    && self.reference.averaged_seconds() >= 0.5 =>
            {
                let diff: Vec<f64> = b.iter().zip(&a).map(|(r, i)| r - i).collect();
                Some(matching::target(&diff, PER_OCTAVE, 1.0 / 3.0))
            }
            _ => None,
        };
    }

    pub fn ready(&self) -> bool {
        self.target.is_some()
            && self.input.averaged_seconds() >= ENOUGH
            && self.reference.averaged_seconds() >= ENOUGH
    }

    /// Fit bands (`count` of them at most).
    pub fn compute(&mut self, budget: usize) {
        if let Some(t) = &self.target {
            let bands = matching::fit(&self.freqs, t, self.count.min(budget).max(1));
            self.count = bands.len().max(1);
            self.result = Some(bands);
        }
    }

    pub fn rect(l: &Layout) -> Rect {
        let w = 620.0f32.min(l.graph.w - 16.0);
        Rect::new(l.graph.x + (l.graph.w - w) / 2.0, l.graph.y + 8.0, w, 62.0)
    }

    pub fn items(&self, r: &Rect) -> Vec<(Hit, Rect)> {
        let y = r.y + 30.0;
        if self.result.is_some() {
            vec![
                (Hit::Fewer, Rect::new(r.x + 14.0, y, 26.0, 22.0)),
                (Hit::More, Rect::new(r.x + 126.0, y, 26.0, 22.0)),
                (Hit::Apply, Rect::new(r.right() - 270.0, y, 80.0, 22.0)),
                (Hit::Back, Rect::new(r.right() - 182.0, y, 80.0, 22.0)),
                (Hit::Cancel, Rect::new(r.right() - 94.0, y, 80.0, 22.0)),
            ]
        } else {
            vec![
                (Hit::Reference, Rect::new(r.x + 14.0, y, 170.0, 22.0)),
                (Hit::RecordInput, Rect::new(r.x + 192.0, y, 112.0, 22.0)),
                (Hit::RecordReference, Rect::new(r.x + 310.0, y, 124.0, 22.0)),
                (Hit::Match, Rect::new(r.right() - 182.0, y, 80.0, 22.0)),
                (Hit::Cancel, Rect::new(r.right() - 94.0, y, 80.0, 22.0)),
            ]
        }
    }

    pub fn hit(&self, pos: Point, l: &Layout) -> Option<Hit> {
        let r = Self::rect(l);
        if !r.contains(pos) {
            return None;
        }
        Some(
            self.items(&r)
                .into_iter()
                .find(|(_, ir)| ir.contains(pos))
                .map_or(Hit::Body, |(h, _)| h),
        )
    }

    fn reference_name(model: &Session, reference: Reference) -> String {
        match reference {
            Reference::Sidechain => "Side Chain".into(),
            Reference::Input => "Input (recorded)".into(),
            Reference::Instance(id) => super::instances::instances(model)
                .into_iter()
                .find(|(i, _, _)| *i == id)
                .map_or_else(|| "Another EQ".into(), |(_, l, _)| l),
        }
    }

    pub fn paint_panel(
        &self,
        p: &mut dyn Painter,
        l: &Layout,
        model: &Session,
        th: &Theme,
        plugin: PluginInstanceId,
    ) {
        let r = Self::rect(l);
        p.shadow(r, 6.0, Color::rgba(0.0, 0.0, 0.0, 0.45), 0.0, 3.0, 14.0);
        p.fill_rounded(r, 6.0, &Paint::Solid(th.eq.panel.with_alpha(0.97)));
        p.stroke_rounded(r, 6.0, 1.0, th.eq.panel_edge);
        let reference = Reference::of(model, plugin);
        let status = if self.result.is_some() {
            "Matched: adjust the number of bands, then apply".to_string()
        } else if self.ready() {
            "Ready to match".to_string()
        } else if reference == Reference::Input
            && !self.reference.averaging
            && self.reference.averaged_seconds() < ENOUGH
        {
            "Play the part to match to and record it as the reference".to_string()
        } else {
            "Listening… let the music play".to_string()
        };
        p.text(
            "EQ MATCH",
            Rect::new(r.x + 14.0, r.y + 6.0, 100.0, 18.0),
            &TextStyle::new(th.fonts.small, th.ui.text_dim)
                .bold()
                .tracking(0.8),
        );
        p.text(
            &status,
            Rect::new(r.x + 110.0, r.y + 6.0, r.w - 124.0, 18.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
        );
        let button = |p: &mut dyn Painter, ir: Rect, label: &str, on: bool, enabled: bool| {
            p.fill_rounded(
                ir,
                4.0,
                &Paint::Solid(if on {
                    th.eq.dyn_range.with_alpha(0.4)
                } else {
                    th.ui.surface_alt
                }),
            );
            p.stroke_rounded(ir, 4.0, 1.0, th.ui.border);
            p.text(
                label,
                ir,
                &TextStyle::new(
                    th.fonts.small,
                    if enabled {
                        th.ui.text
                    } else {
                        th.ui.text_faint
                    },
                )
                .center(),
            );
        };
        for (hit, ir) in self.items(&r) {
            match hit {
                Hit::Reference => button(
                    p,
                    ir,
                    &format!("Ref: {} ▾", Self::reference_name(model, reference)),
                    false,
                    true,
                ),
                Hit::RecordInput => button(
                    p,
                    ir,
                    &format!("● Input {:.0} s", self.input.averaged_seconds()),
                    self.input.averaging,
                    true,
                ),
                Hit::RecordReference => button(
                    p,
                    ir,
                    &format!("● Reference {:.0} s", self.reference.averaged_seconds()),
                    self.reference.averaging,
                    true,
                ),
                Hit::Match => button(p, ir, "Match", false, self.ready()),
                Hit::Fewer => button(p, ir, "−", false, self.count > 1),
                Hit::More => button(p, ir, "+", false, true),
                Hit::Apply => button(p, ir, "Apply", false, true),
                Hit::Back => button(p, ir, "Back", false, true),
                Hit::Cancel => button(p, ir, "Cancel", false, true),
                Hit::Body => {}
            }
        }
        if self.result.is_some() {
            p.text(
                &format!("{} bands", self.count),
                Rect::new(r.x + 42.0, r.y + 30.0, 82.0, 22.0),
                &TextStyle::new(th.fonts.small, th.ui.text).center(),
            );
        }
    }

    /// The difference to match (thick) and what the proposed bands do.
    pub fn paint_curves(
        &self,
        p: &mut dyn Painter,
        l: &Layout,
        axis: &FreqAxis,
        gains: &GainAxis,
        th: &Theme,
        _freqs: &[f64],
    ) {
        let g = &l.graph;
        let Some(target) = &self.target else {
            return;
        };
        let pts: Vec<Point> = self
            .freqs
            .iter()
            .zip(target)
            .filter(|(f, _)| **f >= axis.lo && **f <= axis.hi)
            .map(|(f, d)| Point::new(axis.x(g, *f), gains.y(g, *d as f32)))
            .collect();
        p.stroke_path(&Path::polyline(&pts), 3.0, th.ui.text.with_alpha(0.75));
        if let Some(bands) = &self.result {
            let analog: Vec<AnalogBand> = bands.iter().map(AnalogBand::new).collect();
            let pts: Vec<Point> = self
                .freqs
                .iter()
                .filter(|f| **f >= axis.lo && **f <= axis.hi)
                .map(|f| {
                    let d: f64 = analog.iter().map(|a| a.db(*f)).sum();
                    Point::new(axis.x(g, *f), gains.y(g, d as f32))
                })
                .collect();
            super::paint::dashed(p, &pts, 2.0, th.eq.curve);
            for b in bands {
                let at = Point::new(
                    axis.x(g, b.freq),
                    gains.y(g, (b.gain as f32).clamp(-gains.range, gains.range)),
                );
                p.stroke_path(&Path::circle(at, 6.0), 1.4, th.eq.curve);
            }
        }
    }
}
