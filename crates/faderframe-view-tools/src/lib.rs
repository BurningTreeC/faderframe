//! The Tools view: mastering meters for one source (the master unless
//! another track is chosen).
//!
//! * Loudness — EBU R128 integrated, short-term and momentary loudness and
//!   true peak against a delivery target, with a short-term history; the
//!   dynamics of the measurement: loudness range (LRA), peak-to-loudness
//!   ratio (PLR), crest factor and the DR value.
//! * Level — sample peak with hold and RMS per channel, in dBFS or on a
//!   K-System scale.
//! * Phase — goniometer (mid up, side across) and correlation.
//! * Spectrum — FFT of the mid signal on a log axis with peak hold.
//! * Codec (while the codec monitor is on: Audio › Listen › Codec Monitor)
//!   — what is heard (the decoded master, the original, the difference),
//!   the platform's gain and where it comes from, the true peak after
//!   decoding with the frames over full scale, the level of the codec's
//!   artefacts, the delay it adds to listening.
//!
//! The figures come from [`Session::analyzer`], fed every UI tick, and
//! [`Session::codec_status`].
//!
//! "Announce Limits" (a toggle of the view) has screen readers told when
//! a reading crosses a limit: the true peak over −1 dBTP, the integrated
//! loudness reaching or leaving the target (±1 LU), the correlation going
//! below zero, the codec's decoded peaks over full scale ([`access`]).

use faderframe_analysis::{FLOOR_DB, Level};
use faderframe_core::TrackId;
use faderframe_project::TrackKind;
use faderframe_session::analysis::{LOUDNESS_TARGETS, LevelScale};
use faderframe_session::codec::{CodecHear, CodecOp, CodecStatus};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    AccessNode, AccessPress, CanvasView, Color, Cursor, EventCx, HostRequest, MenuItem, Paint,
    Painter, Path, Point, PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};

mod access;
#[cfg(test)]
mod tests;

const PAD: f32 = 10.0;
const GAP: f32 = 8.0;
/// Spectrum axis (Hz) and range (dBFS).
const SPECTRUM_LO: f64 = 20.0;
const SPECTRUM_HI: f64 = 20_000.0;
const SPECTRUM_TOP: f32 = 0.0;
const SPECTRUM_BOTTOM: f32 = -96.0;
/// Level meter range (dBFS).
const LEVEL_FLOOR: f32 = -60.0;
/// Seconds of loudness history shown.
const HISTORY_SECS: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Source,
    Target,
    Scale,
    ResetOnPlay,
    /// Screen readers told when readings cross their limits.
    Announce,
    Reset,
    Loudness,
    Level,
    Phase,
    Spectrum,
    /// The codec pane: what it plays, its codec or platform, a reset.
    CodecHear(CodecHear),
    CodecChoose,
    CodecReset,
    Codec,
}

/// The codec bitrates and platforms the pane's menu offers.
const CODEC_BITRATES: [u32; 6] = [64, 96, 128, 160, 256, 320];

/// Rectangles of the view.
struct Layout {
    toolbar: Rect,
    source: Rect,
    target: Rect,
    scale: Rect,
    reset_on_play: Rect,
    announce: Rect,
    reset: Rect,
    loudness: Rect,
    level: Rect,
    phase: Rect,
    spectrum: Rect,
    /// The codec pane (while the codec monitor is on) and its controls.
    codec: Option<CodecLayout>,
}

struct CodecLayout {
    pane: Rect,
    hear: [(CodecHear, Rect); 3],
    choose: Rect,
    reset: Rect,
    rows: Rect,
}

pub struct ToolsView {
    theme: Theme,
    /// The codec pane is shown (the monitor was on when last painted).
    codec_shown: bool,
    /// Screen readers are told when readings cross their limits.
    announce: bool,
    /// The readings' side of each limit as last seen.
    limits: Option<access::Limits>,
}

/// "−14.2", "−∞".
pub fn format_lufs(v: f64) -> String {
    if v.is_finite() && v > -200.0 {
        format!("{v:.1}").replace('-', "−")
    } else {
        "−∞".into()
    }
}

fn format_hz(hz: f64) -> String {
    if hz >= 1000.0 {
        format!("{}k", (hz / 1000.0 * 10.0).round() / 10.0)
    } else {
        format!("{hz:.0}")
    }
}

impl ToolsView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            codec_shown: false,
            announce: false,
            limits: None,
        }
    }

    fn layout(&self, size: Size) -> Layout {
        self.layout_with(size, self.codec_shown)
    }

    /// The layout with or without the codec pane.
    fn layout_with(&self, size: Size, codec_shown: bool) -> Layout {
        let t = &self.theme.tools;
        let full = Rect::from_size(size);
        let (toolbar, body) = full.split_top(t.toolbar_height);
        let bar = toolbar.inset_xy(PAD, 5.0);
        let mut x = bar.x;
        let mut button = |w: f32| {
            let r = Rect::new(x, bar.y, w, bar.h);
            x += w + 6.0;
            r
        };
        let source = button(170.0);
        let target = button(200.0);
        let scale = button(90.0);
        let reset_on_play = button(110.0);
        let announce = button(124.0);
        let reset = Rect::new(bar.right() - 70.0, bar.y, 70.0, bar.h);
        let body = body.inset(PAD);
        let w = body.w - 3.0 * GAP;
        let loudness_w = (w * 0.30).clamp(200.0, 340.0);
        let level_w = (w * 0.11).clamp(70.0, 120.0);
        let phase_w = body.h.min(w * 0.22).max(120.0);
        let mut x = body.x;
        let mut pane = |pw: f32| {
            let r = Rect::new(x, body.y, pw.max(0.0), body.h);
            x += pw + GAP;
            r
        };
        let loudness = pane(loudness_w);
        let level = pane(level_w);
        let phase = pane(phase_w);
        let codec = codec_shown.then(|| codec_layout(pane((w * 0.22).clamp(230.0, 300.0))));
        let spectrum = Rect::new(x, body.y, (body.right() - x).max(0.0), body.h);
        Layout {
            toolbar,
            source,
            target,
            scale,
            reset_on_play,
            announce,
            reset,
            loudness,
            level,
            phase,
            spectrum,
            codec,
        }
    }

    pub fn hit(&self, pos: Point, size: Size) -> Option<Hit> {
        let l = self.layout(size);
        if let Some(c) = &l.codec
            && c.pane.contains(pos)
        {
            if let Some((h, _)) = c.hear.iter().find(|(_, r)| r.contains(pos)) {
                return Some(Hit::CodecHear(*h));
            }
            if c.choose.contains(pos) {
                return Some(Hit::CodecChoose);
            }
            if c.reset.contains(pos) {
                return Some(Hit::CodecReset);
            }
            return Some(Hit::Codec);
        }
        [
            (l.source, Hit::Source),
            (l.target, Hit::Target),
            (l.scale, Hit::Scale),
            (l.reset_on_play, Hit::ResetOnPlay),
            (l.announce, Hit::Announce),
            (l.reset, Hit::Reset),
            (l.loudness, Hit::Loudness),
            (l.level, Hit::Level),
            (l.phase, Hit::Phase),
            (l.spectrum, Hit::Spectrum),
        ]
        .into_iter()
        .find(|(r, _)| r.contains(pos))
        .map(|(_, h)| h)
    }

    // --- painting --------------------------------------------------------------

    fn button(&self, p: &mut dyn Painter, r: Rect, label: &str, on: bool) {
        let t = &self.theme;
        let bg = if on {
            t.ui.accent.with_alpha(0.22)
        } else {
            t.tools.panel
        };
        p.fill_rounded(r, 4.0, &Paint::Solid(bg));
        p.stroke_rounded(r, 4.0, 1.0, t.ui.border);
        p.text(
            label,
            r.inset_xy(8.0, 0.0),
            &TextStyle::new(t.fonts.small, if on { t.ui.text } else { t.ui.text_dim }),
        );
    }

    fn paint_toolbar(&self, p: &mut dyn Painter, l: &Layout, model: &Session) {
        let t = &self.theme;
        p.fill(l.toolbar, t.tools.header);
        p.hline(
            l.toolbar.x,
            l.toolbar.right(),
            l.toolbar.bottom() - 0.5,
            t.ui.border,
        );
        let settings = model.analysis_settings();
        let source = model
            .analysis_source()
            .and_then(|s| model.project().track(s))
            .map_or_else(|| "—".to_string(), |t| t.name.clone());
        self.button(p, l.source, &format!("Source: {source} ▾"), false);
        self.button(
            p,
            l.target,
            &format!(
                "Target: {} LUFS ▾",
                format_lufs(settings.target_lufs as f64)
            ),
            false,
        );
        self.button(
            p,
            l.scale,
            &format!("Scale: {} ▾", settings.scale.label()),
            false,
        );
        self.button(p, l.reset_on_play, "Reset on Play", settings.reset_on_play);
        self.button(p, l.announce, "Announce Limits", self.announce);
        self.button(p, l.reset, "Reset", false);
    }

    fn pane(&self, p: &mut dyn Painter, r: Rect, title: &str) -> Rect {
        let t = &self.theme;
        p.fill_rounded(r, 4.0, &Paint::Solid(t.tools.panel));
        let (head, body) = r.split_top(20.0);
        p.text(
            title,
            head.inset_xy(8.0, 0.0),
            &TextStyle::new(t.fonts.tiny, t.ui.text_faint).bold(),
        );
        body.inset_xy(8.0, 4.0)
    }

    fn paint_loudness(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let t = &self.theme;
        let tt = &t.tools;
        let body = self.pane(p, r, "LOUDNESS · EBU R128");
        let l = model.analyzer().loudness.read();
        let target = model.analysis_settings().target_lufs as f64;
        let (big, rest) = body.split_top(46.0);
        p.text(
            &format_lufs(l.integrated),
            Rect::new(big.x, big.y, big.w * 0.62, 32.0),
            &TextStyle::new(t.fonts.display * 1.4, tt.readout).bold(),
        );
        p.text(
            "LUFS INTEGRATED",
            Rect::new(big.x, big.y + 32.0, big.w * 0.62, 12.0),
            &TextStyle::new(t.fonts.tiny, t.ui.text_faint),
        );
        if l.integrated.is_finite() {
            let delta = l.integrated - target;
            let color = if delta.abs() <= 1.0 {
                tt.level_ok
            } else if delta > 0.0 {
                tt.level_over
            } else {
                tt.level_warn
            };
            let sign = if delta >= 0.0 { "+" } else { "−" };
            p.text(
                &format!("{sign}{:.1} LU", delta.abs()),
                Rect::new(big.x + big.w * 0.62, big.y + 4.0, big.w * 0.38, 18.0),
                &TextStyle::new(t.fonts.large, color).bold().right(),
            );
            p.text(
                "vs target",
                Rect::new(big.x + big.w * 0.62, big.y + 22.0, big.w * 0.38, 12.0),
                &TextStyle::new(t.fonts.tiny, t.ui.text_faint).right(),
            );
        }
        let tp_color = if l.true_peak > -1.0 {
            tt.level_over
        } else {
            tt.readout
        };
        let d = model.analyzer().dynamics.read();
        let plr = if l.integrated.is_finite() && l.true_peak.is_finite() {
            format!("{:.1} LU", l.true_peak - l.integrated)
        } else {
            "—".into()
        };
        let lra = if l.integrated.is_finite() {
            format!("{:.1} LU", l.range)
        } else {
            "—".into()
        };
        let crest = d.crest.map_or_else(|| "—".into(), |c| format!("{c:.1} dB"));
        // The DR value in the colours its meters use: 1–7 squashed, 8–13
        // moderate, 14 and up open.
        let (dr, dr_color) = match (d.dr_value(), d.dr) {
            (Some(v), Some(x)) => (
                format!("DR{v}  ({x:.1} dB)"),
                if v <= 7 {
                    tt.level_over
                } else if v <= 13 {
                    tt.level_warn
                } else {
                    tt.level_ok
                },
            ),
            _ => ("—".into(), t.ui.text_dim),
        };
        enum Row {
            Value(&'static str, String, Color),
            Heading(&'static str),
        }
        let rows = [
            Row::Value(
                "Short-term",
                format!("{} LUFS", format_lufs(l.short_term)),
                tt.readout,
            ),
            Row::Value(
                "Momentary",
                format!("{} LUFS", format_lufs(l.momentary)),
                tt.readout,
            ),
            Row::Value(
                "Max short-term",
                format!("{} LUFS", format_lufs(l.max_short_term)),
                t.ui.text_dim,
            ),
            Row::Value(
                "True peak",
                format!("{} dBTP", format_lufs(l.true_peak)),
                tp_color,
            ),
            Row::Heading("DYNAMICS"),
            Row::Value("Loudness range (LRA)", lra, tt.readout),
            Row::Value("Peak to loudness (PLR)", plr, tt.readout),
            Row::Value("Crest factor", crest, tt.readout),
            Row::Value("Dynamic range", dr, dr_color),
        ];
        let row_h = 15.0;
        let (table, graph) = rest.split_top((rows.len() as f32 * row_h + 6.0).min(rest.h));
        for (i, row) in rows.iter().enumerate() {
            let y = table.y + i as f32 * row_h;
            let r = Rect::new(table.x, y, table.w, row_h);
            match row {
                Row::Value(name, value, color) => {
                    p.text(name, r, &TextStyle::new(t.fonts.small, t.ui.text_dim));
                    p.text(
                        value,
                        r,
                        &TextStyle::new(t.fonts.small, *color).right().bold(),
                    );
                }
                Row::Heading(title) => {
                    p.hline(r.x, r.right(), r.y + 3.0, t.ui.border);
                    p.text(
                        title,
                        Rect::new(r.x, r.y + 3.0, r.w, row_h - 3.0),
                        &TextStyle::new(t.fonts.tiny, t.ui.text_faint).bold(),
                    );
                }
            }
        }
        if graph.h > 30.0 {
            self.paint_history(p, graph, model, target, l.integrated);
        }
    }

    /// Short-term loudness of the last minute, with the target.
    fn paint_history(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        model: &Session,
        target: f64,
        integrated: f64,
    ) {
        let tt = &self.theme.tools;
        p.fill_rounded(r, 3.0, &Paint::Solid(tt.well));
        let (top, bottom) = (0.0f32, -48.0f32);
        let y_of = |v: f32| r.y + (top - v.clamp(bottom, top)) / (top - bottom) * r.h;
        for db in [-12.0, -24.0, -36.0] {
            p.hline(r.x, r.right(), y_of(db), tt.grid);
        }
        p.hline(
            r.x,
            r.right(),
            y_of(target as f32),
            tt.target.with_alpha(0.8),
        );
        if integrated.is_finite() {
            p.hline(
                r.x,
                r.right(),
                y_of(integrated as f32),
                tt.readout.with_alpha(0.35),
            );
        }
        let history: Vec<f32> = model.analyzer().loudness.history().collect();
        let n = HISTORY_SECS * 10;
        let shown = &history[history.len().saturating_sub(n)..];
        if shown.len() >= 2 {
            let step = r.w / (n - 1) as f32;
            let x0 = r.right() - (shown.len() - 1) as f32 * step;
            let mut path = Path::new();
            let mut fill = Path::new();
            fill.move_to(Point::new(x0, r.bottom()));
            for (i, v) in shown.iter().enumerate() {
                let pt = Point::new(x0 + i as f32 * step, y_of(*v));
                if i == 0 {
                    path.move_to(pt);
                } else {
                    path.line_to(pt);
                }
                fill.line_to(pt);
            }
            fill.line_to(Point::new(r.right(), r.bottom()));
            p.fill_path(&fill, tt.spectrum.with_alpha(0.18));
            p.stroke_path(&path, 1.4, tt.spectrum);
        }
        let style = TextStyle::new(self.theme.fonts.tiny, self.theme.ui.text_faint);
        p.text(
            &format!("−{HISTORY_SECS} s"),
            Rect::new(r.x + 3.0, r.bottom() - 12.0, 40.0, 11.0),
            &style,
        );
        p.text(
            "short-term",
            Rect::new(r.right() - 63.0, r.y + 2.0, 60.0, 11.0),
            &style.right(),
        );
    }

    fn paint_level(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let t = &self.theme;
        let tt = &t.tools;
        let scale = model.analysis_settings().scale;
        let body = self.pane(p, r, &format!("LEVEL · {}", scale.label()));
        let levels = model.analysis_levels();
        let (bars, readout) = body.split_bottom(30.0);
        let label_w = 22.0;
        let (labels, meters) = bars.split_left(label_w);
        let y_of = |db: f32| {
            meters.y + (0.0 - db.clamp(LEVEL_FLOOR, 0.0)) / (0.0 - LEVEL_FLOOR) * meters.h
        };
        // Scale marks in the chosen units.
        let zero = scale.zero_db();
        let marks: Vec<f32> = match scale {
            LevelScale::Digital => vec![0.0, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0],
            LevelScale::K(n) => {
                let n = n as f32;
                vec![n, 4.0, 0.0, -4.0, -8.0, -12.0, -20.0, -30.0]
                    .into_iter()
                    .filter(|k| k + zero <= 0.0 && k + zero >= LEVEL_FLOOR)
                    .collect()
            }
        };
        for m in &marks {
            let dbfs = match scale {
                LevelScale::Digital => *m,
                LevelScale::K(_) => m + zero,
            };
            let y = y_of(dbfs);
            p.hline(meters.x, meters.right(), y, tt.grid);
            p.text(
                &format!("{m:.0}").replace('-', "−"),
                Rect::new(labels.x, y - 5.5, label_w - 3.0, 11.0),
                &TextStyle::new(t.fonts.tiny, t.ui.text_faint).right(),
            );
        }
        let color_of = |db: f32| match scale {
            LevelScale::Digital if db > -1.0 => tt.level_over,
            LevelScale::Digital if db > -6.0 => tt.level_warn,
            LevelScale::K(_) if db > zero + 4.0 => tt.level_over,
            LevelScale::K(_) if db > zero => tt.level_warn,
            _ => tt.level_ok,
        };
        let w = (meters.w - 6.0) / 2.0;
        for (c, lv) in levels.iter().enumerate() {
            let bar = Rect::new(meters.x + c as f32 * (w + 6.0), meters.y, w, meters.h);
            self.paint_bar(p, bar, lv, &y_of, &color_of);
        }
        let max = model.analyzer().level.max_peak();
        p.text(
            &format!("peak {} dBFS", format_lufs(max as f64)),
            readout.split_top(14.0).0,
            &TextStyle::new(
                t.fonts.tiny,
                if max > -1.0 {
                    tt.level_over
                } else {
                    t.ui.text_dim
                },
            )
            .center(),
        );
        let rms = levels.iter().map(|l| l.rms).fold(-200.0f32, f32::max);
        p.text(
            &format!("RMS {}", format_lufs((rms - zero) as f64)),
            readout.split_bottom(14.0).1,
            &TextStyle::new(t.fonts.tiny, t.ui.text_dim).center(),
        );
    }

    fn paint_bar(
        &self,
        p: &mut dyn Painter,
        bar: Rect,
        lv: &Level,
        y_of: &impl Fn(f32) -> f32,
        color_of: &impl Fn(f32) -> Color,
    ) {
        let tt = &self.theme.tools;
        p.fill(bar, tt.well);
        let peak_y = y_of(lv.peak);
        if lv.peak > LEVEL_FLOOR {
            p.fill(
                Rect::new(bar.x, peak_y, bar.w, bar.bottom() - peak_y),
                color_of(lv.peak).with_alpha(0.55),
            );
        }
        if lv.rms > LEVEL_FLOOR {
            let y = y_of(lv.rms);
            p.fill(
                Rect::new(bar.x + 2.0, y, bar.w - 4.0, bar.bottom() - y),
                color_of(lv.rms),
            );
        }
        if lv.hold > LEVEL_FLOOR {
            p.hline(bar.x, bar.right(), y_of(lv.hold), tt.hold);
        }
    }

    fn paint_phase(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let t = &self.theme;
        let tt = &t.tools;
        let body = self.pane(p, r, "PHASE");
        let (scope, bottom) = body.split_bottom(34.0);
        let side = scope.w.min(scope.h);
        let sq = Rect::new(scope.center().x - side / 2.0, scope.y, side, side);
        p.fill(sq, tt.well);
        let c = sq.center();
        p.line(
            Point::new(sq.x, c.y),
            Point::new(sq.right(), c.y),
            1.0,
            tt.grid,
        );
        p.line(
            Point::new(c.x, sq.y),
            Point::new(c.x, sq.bottom()),
            1.0,
            tt.grid,
        );
        p.line(
            Point::new(sq.x, sq.y),
            Point::new(sq.right(), sq.bottom()),
            1.0,
            tt.grid,
        );
        p.line(
            Point::new(sq.right(), sq.y),
            Point::new(sq.x, sq.bottom()),
            1.0,
            tt.grid,
        );
        let style = TextStyle::new(t.fonts.tiny, t.ui.text_faint);
        p.text("M", Rect::new(c.x + 3.0, sq.y + 2.0, 12.0, 10.0), &style);
        p.text("L", Rect::new(sq.x + 3.0, sq.y + 2.0, 12.0, 10.0), &style);
        p.text(
            "R",
            Rect::new(sq.right() - 12.0, sq.y + 2.0, 10.0, 10.0),
            &style.right(),
        );
        // Mid up, side across: full scale reaches the edge.
        let half = side * 0.5 - 2.0;
        let mut path = Path::new();
        let phase = &model.analyzer().phase;
        for (i, (l, r)) in phase.points().enumerate() {
            let side_v = (r - l) * std::f32::consts::FRAC_1_SQRT_2;
            let mid = (l + r) * std::f32::consts::FRAC_1_SQRT_2;
            let pt = Point::new(
                c.x + side_v.clamp(-1.0, 1.0) * half,
                c.y - mid.clamp(-1.0, 1.0) * half,
            );
            if i == 0 {
                path.move_to(pt);
            } else {
                path.line_to(pt);
            }
        }
        p.push_clip(sq);
        p.stroke_path(&path, 1.0, tt.goniometer.with_alpha(0.55));
        p.pop_clip();
        // Correlation: −1 … +1.
        let corr = phase.correlation();
        let (label, bar) = bottom.inset_xy(0.0, 4.0).split_top(12.0);
        p.text(
            &format!("correlation {corr:+.2}").replace('-', "−"),
            label,
            &TextStyle::new(t.fonts.tiny, t.ui.text_dim).center(),
        );
        let bar = Rect::new(bar.x, bar.y + 2.0, bar.w, 8.0);
        p.fill_rounded(bar, 2.0, &Paint::Solid(tt.well));
        let x = bar.x + (corr + 1.0) * 0.5 * bar.w;
        let color = if corr < 0.0 {
            tt.level_over
        } else {
            tt.level_ok
        };
        let (a, b) = (bar.center().x.min(x), bar.center().x.max(x));
        p.fill(Rect::new(a, bar.y, (b - a).max(2.0), bar.h), color);
        p.vline(bar.center().x, bar.y - 2.0, bar.bottom() + 2.0, tt.grid);
    }

    fn paint_spectrum(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let t = &self.theme;
        let tt = &t.tools;
        let body = self.pane(p, r, "SPECTRUM");
        let (plot, axis) = body.split_bottom(12.0);
        let (scale_w, plot) = (28.0, plot);
        let (labels, plot) = plot.split_left(scale_w);
        p.fill(plot, tt.well);
        let x_of = |hz: f64| {
            plot.x + ((hz / SPECTRUM_LO).ln() / (SPECTRUM_HI / SPECTRUM_LO).ln()) as f32 * plot.w
        };
        let y_of = |db: f32| {
            plot.y
                + (SPECTRUM_TOP - db.clamp(SPECTRUM_BOTTOM, SPECTRUM_TOP))
                    / (SPECTRUM_TOP - SPECTRUM_BOTTOM)
                    * plot.h
        };
        let style = TextStyle::new(t.fonts.tiny, t.ui.text_faint);
        for hz in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0] {
            let x = x_of(hz);
            p.vline(x, plot.y, plot.bottom(), tt.grid);
            p.text(
                &format_hz(hz),
                Rect::new(x - 20.0, axis.y, 40.0, axis.h),
                &style.center(),
            );
        }
        for db in [-12.0, -24.0, -36.0, -48.0, -60.0, -72.0, -84.0] {
            let y = y_of(db);
            p.hline(plot.x, plot.right(), y, tt.grid);
            p.text(
                &format!("{db:.0}").replace('-', "−"),
                Rect::new(labels.x, y - 5.5, scale_w - 4.0, 11.0),
                &style.right(),
            );
        }
        let points = (plot.w / 2.0).max(16.0) as usize;
        let (curve, peaks) = model
            .analyzer()
            .spectrum
            .curve(points, SPECTRUM_LO, SPECTRUM_HI);
        let step = plot.w / (points - 1) as f32;
        let mut line = Path::new();
        let mut fill = Path::new();
        let mut peak = Path::new();
        fill.move_to(Point::new(plot.x, plot.bottom()));
        for (i, (v, pk)) in curve.iter().zip(&peaks).enumerate() {
            let x = plot.x + i as f32 * step;
            let pt = Point::new(x, y_of(v.max(FLOOR_DB)));
            let pp = Point::new(x, y_of(pk.max(FLOOR_DB)));
            if i == 0 {
                line.move_to(pt);
                peak.move_to(pp);
            } else {
                line.line_to(pt);
                peak.line_to(pp);
            }
            fill.line_to(pt);
        }
        fill.line_to(Point::new(plot.right(), plot.bottom()));
        p.push_clip(plot);
        p.fill_path(&fill, tt.spectrum.with_alpha(0.2));
        p.stroke_path(&peak, 1.0, tt.spectrum_peak.with_alpha(0.6));
        p.stroke_path(&line, 1.4, tt.spectrum);
        p.pop_clip();
    }

    fn paint_codec(&self, p: &mut dyn Painter, c: &CodecLayout, st: &CodecStatus) {
        let t = &self.theme;
        let tt = &t.tools;
        self.pane(p, c.pane, "CODEC MONITOR");
        for (h, r) in &c.hear {
            self.button(p, *r, h.label(), st.hear == *h);
        }
        let choose = match st.platform {
            Some(pl) => format!("{} ▾", pl.name),
            None => format!("Opus {} ▾", st.kbps),
        };
        self.button(p, c.choose, &choose, false);
        self.button(p, c.reset, "Reset", false);
        let db = |v: Option<f64>, unit: &str| match v {
            Some(v) if v.is_finite() => format!("{} {unit}", format!("{v:+.1}").replace('-', "−")),
            Some(_) => format!("−∞ {unit}"),
            None => "—".into(),
        };
        let peak_color = match st.true_peak {
            Some(v) if v > 0.0 => tt.level_over,
            Some(v) if v > -1.0 => tt.level_warn,
            _ => tt.readout,
        };
        let state = if !st.ready || st.heard < 1.0 {
            "starting…".to_string()
        } else {
            format!("{:.1} ms later than the mix", st.delay_ms)
        };
        let rows = [
            ("Codec", st.title.clone(), t.ui.text_dim, true),
            (
                "Platform gain",
                db(Some(st.gain_db), "dB"),
                tt.readout,
                false,
            ),
            ("", st.gain_text.clone(), t.ui.text_faint, true),
            (
                "True peak decoded",
                db(st.true_peak, "dBTP"),
                peak_color,
                false,
            ),
            (
                "Over full scale",
                format!("{} frames", st.overs),
                if st.overs > 0 {
                    tt.level_over
                } else {
                    tt.readout
                },
                false,
            ),
            (
                "Artefacts (3 s)",
                db(st.artefacts_now_db, "dB"),
                tt.readout,
                false,
            ),
            (
                "Artefacts since reset",
                db(st.artefacts_db, "dB"),
                tt.readout,
                false,
            ),
            ("Heard", state, t.ui.text_dim, false),
        ];
        let row_h = 15.0;
        let mut y = c.rows.y;
        for (name, value, color, wide) in rows {
            if y + row_h > c.rows.bottom() {
                break;
            }
            let r = Rect::new(c.rows.x, y, c.rows.w, row_h);
            if wide {
                // A line of its own (the codec's name, where the gain
                // comes from).
                p.push_clip(r);
                p.text(&value, r, &TextStyle::new(t.fonts.tiny, color));
                p.pop_clip();
            } else {
                p.text(name, r, &TextStyle::new(t.fonts.small, t.ui.text_dim));
                p.text(
                    &value,
                    r,
                    &TextStyle::new(t.fonts.small, color).right().bold(),
                );
            }
            y += row_h;
        }
        if let Some(e) = &st.error {
            let r = Rect::new(c.rows.x, c.rows.bottom() - row_h, c.rows.w, row_h);
            p.text(e, r, &TextStyle::new(t.fonts.tiny, tt.level_over));
        }
    }

    // --- menus ---------------------------------------------------------------------

    /// The codec pane's menu: off, Opus at a bitrate, a platform, measuring
    /// the song.
    fn codec_menu(model: &Session, at: Point) -> HostRequest<Action> {
        let st = model.codec_status();
        let mut items = vec![MenuItem::new("Off", Action::Codec(CodecOp::Off)).checked(!st.on)];
        for (i, k) in CODEC_BITRATES.iter().enumerate() {
            let item = MenuItem::new(
                format!("Opus {k} kb/s"),
                Action::Codec(CodecOp::Opus {
                    kbps: *k,
                    vbr: st.vbr,
                }),
            )
            .checked(st.on && st.platform.is_none() && st.kbps == *k);
            items.push(if i == 0 { item.separated() } else { item });
        }
        for (i, pl) in faderframe_codec::PLATFORMS.iter().enumerate() {
            let item = MenuItem::new(
                format!("{} · {}", pl.name, pl.codec_text()),
                Action::Codec(CodecOp::Platform(pl.id.to_string())),
            )
            .checked(st.on && st.platform.is_some_and(|p| p.id == pl.id));
            items.push(if i == 0 { item.separated() } else { item });
        }
        items.push(
            MenuItem::new("Variable Bitrate", Action::Codec(CodecOp::Vbr(!st.vbr)))
                .checked(st.vbr)
                .separated(),
        );
        items.push(MenuItem::new(
            "Measure the Song's Loudness",
            Action::Codec(CodecOp::Measure),
        ));
        HostRequest::ContextMenu { at, items }
    }

    fn source_menu(model: &Session, at: Point) -> HostRequest<Action> {
        let current = model.analysis_source();
        let p = model.project();
        let mut tracks: Vec<(TrackId, String)> = p
            .master()
            .map(|m| (m.id, "Master".to_string()))
            .into_iter()
            .collect();
        tracks.extend(
            p.tracks
                .iter()
                .filter(|t| t.kind.has_audio() && t.kind != TrackKind::Master)
                .map(|t| (t.id, t.name.clone())),
        );
        let items = tracks
            .into_iter()
            .enumerate()
            .map(|(i, (id, name))| {
                let item = MenuItem::new(
                    name,
                    Action::SetAnalysisSource((Some(id) != p.master_id()).then_some(id)),
                )
                .checked(current == Some(id));
                if i == 1 { item.separated() } else { item }
            })
            .collect();
        HostRequest::ContextMenu { at, items }
    }

    fn target_menu(model: &Session, at: Point) -> HostRequest<Action> {
        let current = model.analysis_settings().target_lufs;
        let items = LOUDNESS_TARGETS
            .iter()
            .map(|(lufs, label)| {
                MenuItem::new(*label, Action::SetLoudnessTarget(*lufs))
                    .checked((current - lufs).abs() < 0.05)
            })
            .collect();
        HostRequest::ContextMenu { at, items }
    }

    fn scale_menu(model: &Session, at: Point) -> HostRequest<Action> {
        let current = model.analysis_settings().scale;
        let items = LevelScale::ALL
            .iter()
            .map(|s| {
                let label = match s {
                    LevelScale::Digital => "dBFS (digital full scale)".to_string(),
                    LevelScale::K(n) => format!("K-{n} (0 = −{n} dBFS RMS)"),
                };
                MenuItem::new(label, Action::SetLevelScale(*s)).checked(*s == current)
            })
            .collect();
        HostRequest::ContextMenu { at, items }
    }
}

/// The codec pane's controls within `pane`.
fn codec_layout(pane: Rect) -> CodecLayout {
    let body = pane.inset_xy(8.0, 4.0);
    let top = body.y + 18.0;
    let w = ((body.w - 8.0) / 3.0).max(0.0);
    let hear = std::array::from_fn(|i| {
        let h = CodecHear::ALL[i];
        (h, Rect::new(body.x + i as f32 * (w + 4.0), top, w, 22.0))
    });
    let bottom = body.bottom() - 22.0;
    let reset = Rect::new(body.right() - 60.0, bottom, 60.0, 22.0);
    let choose = Rect::new(body.x, bottom, (body.w - 66.0).max(0.0), 22.0);
    let rows = Rect::new(body.x, top + 30.0, body.w, (bottom - top - 36.0).max(0.0));
    CodecLayout {
        pane,
        hear,
        choose,
        reset,
        rows,
    }
}

impl CanvasView<Session, Action> for ToolsView {
    /// Analysers, meters and curves redrawn every frame.
    fn dense(&self) -> bool {
        true
    }

    fn accessible(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        self.access_nodes(size, model)
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Tools".into())
    }

    fn access_press(
        &mut self,
        id: u64,
        press: AccessPress,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        self.press(id, press, size, model, cx)
    }

    fn access_news(&mut self, model: &Session) -> Vec<String> {
        self.news(model)
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.codec_shown = model.codec_monitor_on();
        let l = self.layout(size);
        p.fill(Rect::from_size(size), theme.tools.background);
        self.paint_toolbar(p, &l, model);
        if l.loudness.w > 0.0 {
            self.paint_loudness(p, l.loudness, model);
        }
        if l.level.w > 0.0 {
            self.paint_level(p, l.level, model);
        }
        if l.phase.w > 0.0 {
            self.paint_phase(p, l.phase, model);
        }
        if let Some(c) = &l.codec {
            self.paint_codec(p, c, &model.codec_status());
        }
        if l.spectrum.w > 60.0 {
            self.paint_spectrum(p, l.spectrum, model);
        }
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
                ..
            } => {
                self.codec_shown = model.codec_monitor_on();
                let Some(hit) = self.hit(pos, size) else {
                    return false;
                };
                let l = self.layout(size);
                match hit {
                    Hit::Source => cx.request(Self::source_menu(
                        model,
                        Point::new(l.source.x, l.source.bottom()),
                    )),
                    Hit::Target => cx.request(Self::target_menu(
                        model,
                        Point::new(l.target.x, l.target.bottom()),
                    )),
                    Hit::Scale => cx.request(Self::scale_menu(
                        model,
                        Point::new(l.scale.x, l.scale.bottom()),
                    )),
                    Hit::ResetOnPlay => cx.emit(Action::SetResetOnPlay(
                        !model.analysis_settings().reset_on_play,
                    )),
                    Hit::Announce => self.toggle_announce(cx),
                    Hit::Reset | Hit::Loudness => cx.emit(Action::ResetAnalysis),
                    Hit::CodecHear(h) => cx.emit(Action::Codec(CodecOp::Hear(h))),
                    Hit::CodecChoose => {
                        if let Some(c) = &l.codec {
                            cx.request(Self::codec_menu(
                                model,
                                Point::new(c.choose.x, c.choose.bottom()),
                            ));
                        }
                    }
                    Hit::CodecReset => cx.emit(Action::Codec(CodecOp::ResetMeters)),
                    Hit::Level | Hit::Phase | Hit::Spectrum | Hit::Codec => return false,
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let clickable = matches!(
                    self.hit(pos, size),
                    Some(
                        Hit::Source
                            | Hit::Target
                            | Hit::Scale
                            | Hit::ResetOnPlay
                            | Hit::Announce
                            | Hit::Reset
                            | Hit::CodecHear(_)
                            | Hit::CodecChoose
                            | Hit::CodecReset
                    )
                );
                cx.set_cursor(if clickable {
                    Cursor::Pointer
                } else {
                    Cursor::Default
                });
                false
            }
            _ => false,
        }
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.stream_status().is_some_and(|s| s.running)
    }

    fn tooltip(&self, pos: Point, size: Size, _model: &Session) -> Option<String> {
        Some(
            match self.hit(pos, size)? {
                Hit::Source => "The track whose output is analysed (after its fader)",
                Hit::Target => "Delivery loudness target (integrated LUFS)",
                Hit::Scale => "Level meter scale: dBFS or a K-System scale",
                Hit::ResetOnPlay => "Starting playback starts a new measurement",
                Hit::Announce => {
                    "Screen readers say when a reading crosses its limit: the true peak over −1 dBTP, the integrated loudness reaching or leaving the target (±1 LU), the correlation below zero, the codec's peaks over full scale"
                }
                Hit::Reset => "Start a new measurement (integrated, range, maxima)",
                Hit::Loudness => {
                    "EBU R128 / ITU-R BS.1770: integrated (gated), short-term (3 s), momentary (400 ms), loudness range and true peak · Click to reset"
                }
                Hit::Level => "Sample peak (light) and RMS (solid) per channel, with peak hold",
                Hit::Phase => {
                    "Goniometer (mid up, side across) and correlation: +1 mono, 0 wide, below 0 out of phase"
                }
                Hit::Spectrum => "FFT spectrum of the mid signal (8192 points) with peak hold",
                Hit::CodecHear(CodecHear::Codec) => {
                    "Hear the master coded and decoded (at the platform's loudness)"
                }
                Hit::CodecHear(CodecHear::Original) => {
                    "Hear the master as it is, as late as the codec (a lossless tier)"
                }
                Hit::CodecHear(CodecHear::Difference) => {
                    "Hear what the codec removed or added, alone"
                }
                Hit::CodecChoose => "The codec and bitrate, or a streaming platform",
                Hit::CodecReset => "Start the codec's measurements anew",
                Hit::Codec => {
                    "The codec monitor: the master as listeners get it — the platform's gain from \
                     the song's loudness, the true peak after decoding, what the codec changes. \
                     Listening only: renders and meters never hear it"
                }
            }
            .into(),
        )
    }
}
