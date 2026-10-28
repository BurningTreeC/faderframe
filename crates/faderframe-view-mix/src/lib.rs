//! Mix Intelligence: analyse the mix (the project, a selected range or the
//! selected tracks; as mixed or before processing), the findings ranked by
//! relevance on the left, the one selected on the right — what was
//! measured (with units), how it is interpreted, the rule's limitations
//! and the experiments to audition, compare loudness-matched and apply.
//! Tracks get their roles here too.
//!
//! The view keeps only what it shows (filters, sorting, scroll); the
//! analysis, its results and the audition are the session's
//! (`faderframe_session::mix`). Nothing here changes the project: buttons
//! ask the session, which never applies an experiment without Apply.

#![forbid(unsafe_code)]

use faderframe_core::{TrackId, TrackRole};
use faderframe_mix_intelligence::bands::format_hz;
use faderframe_mix_intelligence::{Category, Finding, Grade, SignalPoint, Subject};
use faderframe_project::Command;
use faderframe_session::mix::{MixOp, MixResults, MixScope, MixStatus};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    AccessNode, AccessRole, CanvasView, Color, EventCx, HostRequest, MenuItem, Painter, Path,
    Point, PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
    access_id,
};

const HEADER_H: f32 = 34.0;
const STATUS_H: f32 = 24.0;
const FILTER_H: f32 = 28.0;
const ROW_H: f32 = 56.0;
const FOOTER_H: f32 = 24.0;
const PAD: f32 = 10.0;
const LINE: f32 = 16.0;
const CHART_H: f32 = 130.0;

/// What a place on the view does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Analyse(MixScope),
    Cancel,
    Signal(SignalPoint),
    Roles,
    Category(Option<Category>),
    Sort,
    TrackFilter,
    Row(u64),
    ShowDismissed,
    Show(u64),
    Dismiss(u64),
    Event(u64, usize),
    Audition(u64, usize),
    Apply(u64, usize),
    Compare(bool),
    Match,
    Reject,
    Role(TrackId),
    Levels,
}

/// What the right side shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    /// The selected finding.
    Details,
    /// Every track's role.
    Roles,
    /// Every track's and the master's levels.
    Levels,
}

/// The list's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Relevance,
    Time,
    Track,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::Relevance => "By relevance",
            Sort::Time => "By time",
            Sort::Track => "By track",
        }
    }

    fn next(self) -> Self {
        match self {
            Sort::Relevance => Sort::Time,
            Sort::Time => Sort::Track,
            Sort::Track => Sort::Relevance,
        }
    }
}

/// A place painted last time: what it does, where, whether it is enabled,
/// its label (screen readers) and tooltip.
#[derive(Clone, Debug)]
struct Spot {
    hit: Hit,
    rect: Rect,
    enabled: bool,
    label: String,
    tip: String,
    on: bool,
}

pub struct MixView {
    theme: Theme,
    signal: SignalPoint,
    category: Option<Category>,
    sort: Sort,
    track: Option<TrackId>,
    panel: Panel,
    list_scroll: f32,
    detail_scroll: f32,
    list: Rect,
    detail: Rect,
    list_content: f32,
    detail_content: f32,
    spots: Vec<Spot>,
    /// Where places can be clicked while a scrolled part is painted (a
    /// place scrolled out of it is not there).
    visible: Option<Rect>,
    hover: Option<Hit>,
}

/// m:ss.s
fn mmss(s: f64) -> String {
    let s = s.max(0.0);
    let m = (s / 60.0).floor();
    format!("{m:.0}:{:04.1}", s - m * 60.0)
}

/// Lines of `text` that fit `width`.
fn wrap(p: &mut dyn Painter, text: &str, width: f32, style: &TextStyle) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if line.is_empty() || p.text_width(&candidate, style) <= width {
            line = candidate;
        } else {
            lines.push(std::mem::take(&mut line));
            line = word.to_string();
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

impl MixView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            signal: SignalPoint::Processed,
            category: None,
            sort: Sort::Relevance,
            track: None,
            panel: Panel::Details,
            list_scroll: 0.0,
            detail_scroll: 0.0,
            list: Rect::new(0.0, 0.0, 0.0, 0.0),
            detail: Rect::new(0.0, 0.0, 0.0, 0.0),
            list_content: 0.0,
            detail_content: 0.0,
            spots: Vec::new(),
            visible: None,
            hover: None,
        }
    }

    /// Where `hit` was painted last (tests, scripts).
    pub fn rect_of(&self, hit: Hit) -> Option<Rect> {
        self.spots.iter().find(|s| s.hit == hit).map(|s| s.rect)
    }

    /// Whether `hit` was painted enabled.
    pub fn enabled(&self, hit: Hit) -> Option<bool> {
        self.spots.iter().find(|s| s.hit == hit).map(|s| s.enabled)
    }

    fn spot_at(&self, pos: Point) -> Option<&Spot> {
        // Later spots are drawn over earlier ones.
        self.spots.iter().rev().find(|s| s.rect.contains(pos))
    }

    #[allow(clippy::too_many_arguments)]
    fn button(
        &mut self,
        p: &mut dyn Painter,
        r: Rect,
        label: &str,
        hit: Hit,
        on: bool,
        enabled: bool,
        tip: impl Into<String>,
    ) {
        let th = &self.theme;
        if on {
            p.fill_rounded(r, 4.0, &th.ui.selection.with_alpha(0.5).into());
        } else if enabled && self.hover == Some(hit) {
            p.fill_rounded(r, 4.0, &th.ui.text.with_alpha(0.06).into());
        }
        p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
        let ink = if !enabled {
            th.ui.text_faint
        } else if on {
            th.ui.text
        } else {
            th.ui.text_dim
        };
        p.text(label, r, &TextStyle::new(th.fonts.small, ink).center());
        self.register(Spot {
            hit,
            rect: r,
            enabled,
            label: label.to_string(),
            tip: tip.into(),
            on,
        });
    }

    /// A place to click, cut to what is visible of the part painted.
    fn register(&mut self, mut spot: Spot) {
        if let Some(v) = self.visible {
            spot.rect = spot.rect.intersection(&v);
            if spot.rect.is_empty() {
                return;
            }
        }
        self.spots.push(spot);
    }

    /// A button as wide as its label needs, from `x`; returns the next x.
    #[allow(clippy::too_many_arguments)]
    fn button_at(
        &mut self,
        p: &mut dyn Painter,
        x: f32,
        y: f32,
        h: f32,
        label: &str,
        hit: Hit,
        on: bool,
        enabled: bool,
        tip: impl Into<String>,
    ) -> f32 {
        let style = TextStyle::new(self.theme.fonts.small, self.theme.ui.text);
        let w = p.text_width(label, &style) + 18.0;
        self.button(p, Rect::new(x, y, w, h), label, hit, on, enabled, tip);
        x + w + 6.0
    }

    fn grade_color(&self, g: Grade) -> Color {
        let ui = &self.theme.ui;
        match g {
            Grade::High => ui.accent,
            Grade::Moderate => ui.text_dim,
            Grade::Low => ui.text_faint,
        }
    }

    fn name(model: &Session) -> impl Fn(TrackId) -> String + '_ {
        move |t| model.mix_track_name(t)
    }

    fn span_text(model: &Session, r: &MixResults, a: f64, b: f64) -> String {
        let rate = f64::from(r.rate.max(1));
        let base = r.start_sample as f64 / rate;
        let bar = |s: f64| {
            model
                .project()
                .timeline
                .meter
                .bar_at(r.position(model.project(), s))
                + 1
        };
        let (ba, bb) = (bar(a), bar((b - 0.001).max(a)));
        let bars = if ba == bb {
            format!("bar {ba}")
        } else {
            format!("bars {ba}–{bb}")
        };
        format!("{}–{} ({bars})", mmss(base + a), mmss(base + b))
    }

    /// The findings listed: filtered and sorted.
    fn listed<'a>(&self, model: &'a Session) -> Vec<&'a Finding> {
        let mut v: Vec<&Finding> = model
            .mix_findings()
            .into_iter()
            .filter(|f| self.category.is_none_or(|c| f.category() == c))
            .filter(|f| self.track.is_none_or(|t| f.tracks.contains(&t)))
            .collect();
        match self.sort {
            Sort::Relevance => {}
            Sort::Time => v.sort_by(|a, b| {
                let t = |f: &Finding| f.time.map_or(f64::INFINITY, |t| t.start);
                t(a).total_cmp(&t(b)).then(a.id.cmp(&b.id))
            }),
            Sort::Track => v.sort_by(|a, b| {
                let n = |f: &Finding| {
                    f.tracks
                        .first()
                        .map(|t| model.mix_track_name(*t))
                        .unwrap_or_default()
                };
                n(a).cmp(&n(b)).then(a.id.cmp(&b.id))
            }),
        }
        v
    }

    fn paint_header(&mut self, p: &mut dyn Painter, size: Size, model: &Session) {
        let th = self.theme.clone();
        let header = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(header, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        p.text(
            "Mix Intelligence",
            Rect::new(12.0, 0.0, 130.0, HEADER_H),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        let running = model.mix_analysing();
        let (y, h) = (6.0, HEADER_H - 12.0);
        let has_range = model.selection.range.is_some_and(|r| !r.is_empty());
        let has_tracks = !model.selection.tracks.is_empty();
        let mut x = 146.0;
        x = self.button_at(
            p,
            x,
            y,
            h,
            "Analyse Project",
            Hit::Analyse(MixScope::Project),
            false,
            true,
            "Render every track offline over the whole project and compare them",
        );
        x = self.button_at(
            p,
            x,
            y,
            h,
            "Selected Range",
            Hit::Analyse(MixScope::Selection),
            false,
            has_range,
            if has_range {
                "Analyse every track over the edit selection's time range"
            } else {
                "Select a time range in the arranger first"
            },
        );
        x = self.button_at(
            p,
            x,
            y,
            h,
            "Selected Tracks",
            Hit::Analyse(MixScope::SelectedTracks),
            false,
            has_tracks,
            if has_tracks {
                "Analyse the selected tracks against every other track (over the selected range \
                 when there is one)"
            } else {
                "Select tracks first"
            },
        );
        x = self.button_at(
            p,
            x,
            y,
            h,
            "Cancel",
            Hit::Cancel,
            false,
            running,
            "Stop the analysis (the last results stay)",
        );
        x += 10.0;
        x = self.button_at(
            p,
            x,
            y,
            h,
            "Processed",
            Hit::Signal(SignalPoint::Processed),
            self.signal == SignalPoint::Processed,
            true,
            SignalPoint::Processed.describe(),
        );
        x = self.button_at(
            p,
            x,
            y,
            h,
            "Source",
            Hit::Signal(SignalPoint::Source),
            self.signal == SignalPoint::Source,
            true,
            SignalPoint::Source.describe(),
        );
        let w = 70.0;
        let rx = (size.w - 2.0 * w - 16.0).max(x + 4.0);
        self.button(
            p,
            Rect::new(rx, y, w, h),
            "Levels",
            Hit::Levels,
            self.panel == Panel::Levels,
            model.mix_results().is_some(),
            "Every track's and the master's measured levels, side by side",
        );
        self.button(
            p,
            Rect::new(rx + w + 6.0, y, w, h),
            "Roles",
            Hit::Roles,
            self.panel == Panel::Roles,
            true,
            "Say what each track plays: the analysis weighs pairs by it (names are never read \
             as roles)",
        );
    }

    fn paint_status(&mut self, p: &mut dyn Painter, size: Size, model: &Session) {
        let th = self.theme.clone();
        let r = Rect::new(0.0, HEADER_H, size.w, STATUS_H);
        p.fill(r, th.ui.surface_alt);
        p.hline(0.0, size.w, r.bottom() - 0.5, th.ui.border);
        let style = TextStyle::new(th.fonts.small, th.ui.text_dim);
        let text_rect = Rect::new(12.0, r.y, size.w - 24.0, r.h);
        match model.mix_status() {
            MixStatus::Running { what, fraction } => {
                let bar = Rect::new(
                    12.0,
                    r.y + r.h - 4.0,
                    (size.w - 24.0) * fraction as f32,
                    2.0,
                );
                p.fill(bar, th.ui.accent.with_alpha(0.7));
                p.text(
                    &format!("{what} — {:.0} %", fraction * 100.0),
                    text_rect,
                    &style,
                );
                return;
            }
            MixStatus::Failed(e) => {
                p.text(&format!("The analysis failed: {e}"), text_rect, &style);
                return;
            }
            MixStatus::Cancelled if model.mix_results().is_none() => {
                p.text("Cancelled.", text_rect, &style);
                return;
            }
            _ => {}
        }
        if let Some(why) = model.mix_stale() {
            p.fill(r, th.ui.accent.with_alpha(0.12));
            p.text(why, text_rect, &TextStyle::new(th.fonts.small, th.ui.text));
            return;
        }
        let Some(res) = model.mix_results() else {
            p.text(
                "Nothing analysed yet. The analysis renders offline; the project is never \
                 changed by it.",
                text_rect,
                &style,
            );
            return;
        };
        let range = Self::span_text(model, res, 0.0, res.seconds());
        let skipped = if res.skipped.is_empty() {
            String::new()
        } else {
            format!(" · {} left out", res.skipped.len())
        };
        p.text(
            &format!(
                "{} · {} · {} tracks{skipped} · {} rendered, {} reused · algorithm v{}",
                res.signal.label(),
                range,
                res.tracks.len(),
                res.rendered,
                res.reused,
                res.algorithm
            ),
            text_rect,
            &style,
        );
    }

    fn paint_list(&mut self, p: &mut dyn Painter, rect: Rect, model: &Session) {
        let th = self.theme.clone();
        p.fill(rect, th.ui.background);
        // Filters.
        let (y, h) = (rect.y + 4.0, FILTER_H - 8.0);
        let mut x = rect.x + 8.0;
        x = self.button_at(
            p,
            x,
            y,
            h,
            "All",
            Hit::Category(None),
            self.category.is_none(),
            true,
            "Every category",
        );
        for c in Category::ALL {
            let label = match c {
                Category::Stereo => "Stereo",
                _ => c.label(),
            };
            x = self.button_at(
                p,
                x,
                y,
                h,
                label,
                Hit::Category(Some(c)),
                self.category == Some(c),
                true,
                format!("Only {}", c.label().to_lowercase()),
            );
        }
        x = self.button_at(
            p,
            x,
            y,
            h,
            self.sort.label(),
            Hit::Sort,
            false,
            true,
            "Order the list by relevance, time or track",
        );
        let track = self
            .track
            .map_or_else(|| "All tracks".to_string(), |t| model.mix_track_name(t));
        let _ = self.button_at(
            p,
            x,
            y,
            h,
            &track,
            Hit::TrackFilter,
            self.track.is_some(),
            model.mix_results().is_some(),
            "Only the findings of one track (click for the next)",
        );
        p.hline(rect.x, rect.right(), rect.y + FILTER_H - 0.5, th.ui.border);
        let dismissed = model.mix_dismissed_count();
        let footer = dismissed > 0;
        let list = Rect::new(
            rect.x,
            rect.y + FILTER_H,
            rect.w,
            (rect.h - FILTER_H - if footer { FOOTER_H } else { 0.0 }).max(0.0),
        );
        self.list = list;
        let rows = self.listed(model);
        self.list_content = rows.len() as f32 * ROW_H;
        self.list_scroll = self
            .list_scroll
            .clamp(0.0, (self.list_content - list.h).max(0.0));
        p.push_clip(list);
        self.visible = Some(list);
        if rows.is_empty() {
            let text = if model.mix_results().is_none() {
                "Analyse to see findings here."
            } else if model
                .mix_results()
                .is_some_and(|r| r.report.findings.is_empty())
            {
                "No findings: nothing the rules look for was measured. That says nothing \
                 about whether the mix is good."
            } else {
                "No findings match the filters (or they are dismissed)."
            };
            let style = TextStyle::new(th.fonts.small, th.ui.text_dim);
            let mut y = list.y + PAD;
            for l in wrap(p, text, list.w - 2.0 * PAD, &style) {
                p.text(
                    &l,
                    Rect::new(list.x + PAD, y, list.w - 2.0 * PAD, LINE),
                    &style,
                );
                y += LINE;
            }
        }
        let selected = model.mix_selected_id();
        let name = Self::name(model);
        for (i, f) in rows.iter().enumerate() {
            let r = Rect::new(
                list.x,
                list.y + i as f32 * ROW_H - self.list_scroll,
                list.w,
                ROW_H,
            );
            if r.bottom() < list.y || r.y > list.bottom() {
                continue;
            }
            let dismissed = model.mix_dismissed(f.id);
            if selected == Some(f.id) {
                p.fill(r, th.ui.selection.with_alpha(0.35));
            } else if self.hover == Some(Hit::Row(f.id)) {
                p.fill(r, th.ui.text.with_alpha(0.05));
            }
            let sev = Grade::of_severity(f.severity);
            p.fill(
                Rect::new(r.x, r.y + 6.0, 3.0, r.h - 12.0),
                self.grade_color(sev),
            );
            let ink = if dismissed {
                th.ui.text_faint
            } else {
                th.ui.text
            };
            let tx = r.x + 12.0;
            let tw = r.w - 20.0;
            p.text(
                &f.title(&name),
                Rect::new(tx, r.y + 4.0, tw, LINE),
                &TextStyle::new(th.fonts.small, ink).bold(),
            );
            let mut second = Vec::new();
            let names: Vec<String> = f.tracks.iter().map(|t| name(*t)).collect();
            if !names.is_empty() {
                second.push(names.join(", "));
            }
            if let (Some(t), Some(res)) = (f.time, model.mix_results()) {
                second.push(Self::span_text(model, res, t.start, t.end));
            }
            if let Some(fr) = f.frequency {
                second.push(fr.label());
            }
            p.text(
                &second.join(" · "),
                Rect::new(tx, r.y + 4.0 + LINE, tw, LINE),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
            let n = f
                .recommendations
                .iter()
                .filter(|r| r.experiment.previewable())
                .count();
            let pattern = f
                .pattern
                .map(|p| format!(" · {}", p.label()))
                .unwrap_or_default();
            p.text(
                &format!(
                    "Severity {} · Confidence {} · {} experiment{}{pattern}{}",
                    sev.label(),
                    Grade::of_confidence(f.confidence).label(),
                    n,
                    if n == 1 { "" } else { "s" },
                    if dismissed { " · dismissed" } else { "" }
                ),
                Rect::new(tx, r.y + 4.0 + 2.0 * LINE, tw, LINE),
                &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
            );
            p.hline(
                r.x,
                r.right(),
                r.bottom() - 0.5,
                th.ui.border.with_alpha(0.5),
            );
            self.register(Spot {
                hit: Hit::Row(f.id),
                rect: r,
                enabled: true,
                label: format!(
                    "{}, severity {}",
                    f.title(&name),
                    sev.label().to_lowercase()
                ),
                tip: f.explanation(&name),
                on: selected == Some(f.id),
            });
        }
        p.pop_clip();
        self.visible = None;
        if footer {
            let fr = Rect::new(rect.x, rect.bottom() - FOOTER_H, rect.w, FOOTER_H);
            p.hline(fr.x, fr.right(), fr.y, th.ui.border);
            let shows = model.mix_shows_dismissed();
            let label = format!(
                "{dismissed} dismissed — {}",
                if shows { "hide them" } else { "show them" }
            );
            self.button(
                p,
                fr.inset(3.0),
                &label,
                Hit::ShowDismissed,
                shows,
                true,
                "Dismissed findings stay hidden until the project is closed",
            );
        }
    }

    /// Heading of a details section; returns the next y.
    fn heading(&self, p: &mut dyn Painter, x: f32, y: f32, w: f32, text: &str) -> f32 {
        let th = &self.theme;
        p.text(
            text,
            Rect::new(x, y + 6.0, w, LINE),
            &TextStyle::new(th.fonts.small, th.ui.text).bold(),
        );
        p.hline(x, x + w, y + 6.0 + LINE + 1.0, th.ui.border.with_alpha(0.6));
        y + 6.0 + LINE + 6.0
    }

    /// Wrapped text; returns the next y.
    #[allow(clippy::too_many_arguments)]
    fn para(
        &self,
        p: &mut dyn Painter,
        x: f32,
        y: f32,
        w: f32,
        text: &str,
        size: f32,
        color: Color,
    ) -> f32 {
        let style = TextStyle::new(size, color);
        let mut y = y;
        for l in wrap(p, text, w, &style) {
            p.text(&l, Rect::new(x, y, w, LINE), &style);
            y += LINE;
        }
        y
    }

    fn paint_chart(&self, p: &mut dyn Painter, r: Rect, f: &Finding, model: &Session) {
        let th = &self.theme;
        let Some(s) = &f.spectrum else {
            return;
        };
        p.fill(r, th.ui.surface_alt);
        p.stroke_rounded(r, 3.0, 1.0, th.ui.border);
        let (lo, hi) = (20f64, 20_000f64);
        let x_of = |hz: f64| r.x + (r.w * ((hz / lo).ln() / (hi / lo).ln()) as f32).clamp(0.0, r.w);
        let top = s
            .series
            .iter()
            .flat_map(|(_, v)| v.iter().copied())
            .fold(f64::NEG_INFINITY, f64::max);
        if !top.is_finite() {
            return;
        }
        let range_db = 60.0;
        let plot = Rect::new(r.x, r.y + 18.0, r.w, r.h - 32.0);
        let y_of = |db: f64| plot.y + (plot.h * (((top - db) / range_db).clamp(0.0, 1.0)) as f32);
        // The range the finding is about.
        let (a, b) = s.highlight;
        let edge = |i: usize, upper: bool| {
            let c = s.centres.get(i).copied().unwrap_or(lo);
            // Third-octave band edges: a sixth of an octave either side.
            c * 2f64.powf(if upper { 1.0 / 6.0 } else { -1.0 / 6.0 })
        };
        let (ha, hb) = (x_of(edge(a, false)), x_of(edge(b, true)));
        p.fill(
            Rect::new(ha, plot.y, (hb - ha).max(1.0), plot.h),
            th.ui.selection.with_alpha(0.3),
        );
        for hz in [100.0, 1000.0, 10_000.0] {
            let x = x_of(hz);
            p.vline(x, plot.y, plot.bottom(), th.ui.border.with_alpha(0.5));
            p.text(
                &format_hz(hz),
                Rect::new(x + 2.0, plot.bottom(), 50.0, 14.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
            );
        }
        let colors = [th.ui.accent, th.ui.text_dim];
        let mut lx = r.x + 6.0;
        for ((track, levels), color) in s.series.iter().zip(colors) {
            let mut path = Path::new();
            let mut started = false;
            for (c, db) in s.centres.iter().zip(levels) {
                let pt = Point::new(x_of(*c), y_of(*db));
                if started {
                    path.line_to(pt);
                } else {
                    path.move_to(pt);
                    started = true;
                }
            }
            p.stroke_path(&path, 1.5, color);
            p.fill(Rect::new(lx, r.y + 7.0, 10.0, 3.0), color);
            let label = model.mix_track_name(*track);
            let style = TextStyle::new(th.fonts.tiny, th.ui.text_dim);
            p.text(&label, Rect::new(lx + 14.0, r.y + 2.0, 160.0, 14.0), &style);
            lx += 24.0 + p.text_width(&label, &style);
        }
        p.text(
            &format!("mean band level while covered, top {top:.0} dB, 60 dB shown"),
            Rect::new(r.x, r.y + 2.0, r.w - 6.0, 14.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_faint).right(),
        );
    }

    fn paint_details(&mut self, p: &mut dyn Painter, rect: Rect, model: &Session) {
        let th = self.theme.clone();
        p.fill(rect, th.ui.background);
        self.detail = rect;
        let Some(f) = model.mix_selected().cloned() else {
            let style = TextStyle::new(th.fonts.small, th.ui.text_dim);
            let text = if model.mix_results().is_some() {
                "Select a finding to see what was measured, how it is read and what could be \
                 tried."
            } else {
                "Mix Intelligence renders each track offline (as mixed, or before processing), \
                 measures it in third-octave bands over time and compares the tracks with \
                 explicit rules: where one may mask another, levels and peaks, how the mix \
                 folds to mono. Every finding shows its measurements and the rule's limits; \
                 experiments are auditioned in the engine and applied only when you say so, \
                 as one undo step."
            };
            let mut y = rect.y + PAD;
            for l in wrap(p, text, rect.w - 2.0 * PAD, &style) {
                p.text(
                    &l,
                    Rect::new(rect.x + PAD, y, rect.w - 2.0 * PAD, LINE),
                    &style,
                );
                y += LINE;
            }
            self.detail_content = y - rect.y;
            return;
        };
        let name = Self::name(model);
        let res = model.mix_results();
        let (x, w) = (rect.x + PAD, (rect.w - 2.0 * PAD).max(40.0));
        p.push_clip(rect);
        self.visible = Some(rect);
        let mut y = rect.y + PAD - self.detail_scroll;
        y = self.para(p, x, y, w, &f.title(&name), th.fonts.normal, th.ui.text);
        p.text(
            &format!(
                "{} · rule “{}” v{} · {}",
                f.category().label(),
                f.rule.name(),
                f.rule.version(),
                f.signal.label()
            ),
            Rect::new(x, y, w, LINE),
            &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
        );
        y += LINE + 4.0;
        let mut bx = x;
        bx = self.button_at(
            p,
            bx,
            y,
            22.0,
            "Show in Arranger",
            Hit::Show(f.id),
            false,
            !f.tracks.is_empty() || f.time.is_some(),
            "Select its tracks and its time",
        );
        let dismissed = model.mix_dismissed(f.id);
        let _ = self.button_at(
            p,
            bx,
            y,
            22.0,
            "Dismiss",
            Hit::Dismiss(f.id),
            dismissed,
            !dismissed,
            "Hide it from the list (nothing else changes)",
        );
        y += 30.0;
        // Measured: facts with units.
        y = self.heading(p, x, y, w, "Measured");
        let label_w = (w * 0.55).min(300.0);
        for e in &f.evidence {
            let on = e
                .track
                .map(|t| format!(" ({})", name(t)))
                .unwrap_or_default();
            p.text(
                &format!("{}{on}", e.label),
                Rect::new(x, y, label_w, LINE),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
            p.text(
                &e.value_text(),
                Rect::new(x + label_w, y, w - label_w, LINE),
                &TextStyle::new(th.fonts.small, th.ui.text),
            );
            y += LINE;
        }
        if let (Some(t), Some(res)) = (f.time, res) {
            p.text(
                &format!("Time: {}", Self::span_text(model, res, t.start, t.end)),
                Rect::new(x, y, w, LINE),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
            y += LINE;
        }
        if !f.sections.is_empty() {
            p.text(
                &format!("Sections: {}", f.sections.join(", ")),
                Rect::new(x, y, w, LINE),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
            y += LINE;
        }
        if let Some(res) = res
            && !f.events.is_empty()
        {
            p.text(
                "Events (click to show):",
                Rect::new(x, y, w, LINE),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
            y += LINE + 2.0;
            let mut ex = x;
            for (i, e) in f.events.iter().enumerate().take(24) {
                let label = format!(
                    "{}–{}",
                    mmss(res.start_sample as f64 / f64::from(res.rate) + e.start),
                    mmss(res.start_sample as f64 / f64::from(res.rate) + e.end)
                );
                let style = TextStyle::new(th.fonts.small, th.ui.text);
                let bw = p.text_width(&label, &style) + 14.0;
                if ex + bw > x + w {
                    ex = x;
                    y += 24.0;
                }
                self.button(
                    p,
                    Rect::new(ex, y, bw, 20.0),
                    &label,
                    Hit::Event(f.id, i),
                    false,
                    true,
                    "Select this event's time",
                );
                ex += bw + 4.0;
            }
            y += 26.0;
        }
        if f.spectrum.is_some() {
            self.paint_chart(p, Rect::new(x, y + 4.0, w, CHART_H), &f, model);
            y += CHART_H + 10.0;
        }
        // Interpretation.
        y = self.heading(p, x, y, w, "Interpretation");
        y = self.para(
            p,
            x,
            y,
            w,
            &f.explanation(&name),
            th.fonts.small,
            th.ui.text,
        );
        let terms = |t: &[faderframe_mix_intelligence::finding::Term]| {
            t.iter()
                .map(|t| format!("{} {:.2}", t.name, t.value))
                .collect::<Vec<_>>()
                .join(" × ")
        };
        let sev = format!(
            "Severity {} ({:.2}{}{}): how much the measured interaction weighs, not a verdict \
             on the mix.",
            Grade::of_severity(f.severity).label(),
            f.severity,
            if f.severity_terms.is_empty() {
                ""
            } else {
                " = "
            },
            terms(&f.severity_terms)
        );
        y = self.para(p, x, y + 4.0, w, &sev, th.fonts.small, th.ui.text_dim);
        let conf = format!(
            "Confidence {} ({:.2}{}{}): how strong and complete the evidence is.",
            Grade::of_confidence(f.confidence).label(),
            f.confidence,
            if f.confidence_terms.is_empty() {
                ""
            } else {
                " = "
            },
            terms(&f.confidence_terms)
        );
        y = self.para(p, x, y, w, &conf, th.fonts.small, th.ui.text_dim);
        y = self.para(
            p,
            x,
            y + 4.0,
            w,
            &format!("How the rule decides: {}", f.rule.method()),
            th.fonts.tiny,
            th.ui.text_faint,
        );
        // Limitations.
        y = self.heading(p, x, y, w, "Limitations and caveats");
        for l in f
            .rule
            .limitations()
            .iter()
            .map(|s| s.to_string())
            .chain(f.caveats.clone())
        {
            y = self.para(
                p,
                x,
                y,
                w,
                &format!("• {l}"),
                th.fonts.small,
                th.ui.text_dim,
            );
        }
        y = self.para(
            p,
            x,
            y,
            w,
            &format!("• {}", f.signal.describe()),
            th.fonts.small,
            th.ui.text_dim,
        );
        // Experiments.
        y = self.heading(p, x, y, w, "Experiments (proposed, not applied)");
        let audition = model.mix_audition();
        for (i, r) in f.recommendations.iter().enumerate() {
            let card_top = y;
            y = self.para(
                p,
                x + 8.0,
                y + 4.0,
                w - 16.0,
                &r.experiment.title(&name),
                th.fonts.small,
                th.ui.text,
            );
            y = self.para(
                p,
                x + 8.0,
                y,
                w - 16.0,
                &format!("Purpose: {}", r.purpose),
                th.fonts.small,
                th.ui.text_dim,
            );
            y = self.para(
                p,
                x + 8.0,
                y,
                w - 16.0,
                &format!("Side effects: {}", r.side_effects),
                th.fonts.small,
                th.ui.text_dim,
            );
            if r.experiment.previewable() {
                let can = model.mix_can_try(f.id, i);
                let why = can.clone().err().unwrap_or_default();
                let enabled = can.is_ok();
                let applied = model.mix_applied(f.id, i);
                let here = audition
                    .as_ref()
                    .is_some_and(|a| a.finding == f.id && a.recommendation == i);
                let mut bx = x + 8.0;
                y += 4.0;
                bx = self.button_at(
                    p,
                    bx,
                    y,
                    22.0,
                    if here { "Auditioning" } else { "Audition" },
                    Hit::Audition(f.id, i),
                    here,
                    enabled,
                    if enabled {
                        "Play it in the engine (the project is not changed: it is taken out \
                         again unless you apply it)"
                            .to_string()
                    } else {
                        why.clone()
                    },
                );
                bx = self.button_at(
                    p,
                    bx,
                    y,
                    22.0,
                    "Apply",
                    Hit::Apply(f.id, i),
                    false,
                    enabled,
                    if enabled {
                        "Make it one step of the project's history (Undo takes it back)".to_string()
                    } else {
                        why.clone()
                    },
                );
                if applied {
                    p.text(
                        "Applied",
                        Rect::new(bx, y, 80.0, 22.0),
                        &TextStyle::new(th.fonts.small, th.ui.accent),
                    );
                }
                y += 28.0;
                if !enabled && !why.is_empty() {
                    y = self.para(
                        p,
                        x + 8.0,
                        y,
                        w - 16.0,
                        &why,
                        th.fonts.tiny,
                        th.ui.text_faint,
                    );
                }
                if here && let Some(a) = &audition {
                    let mut ax = x + 8.0;
                    ax = self.button_at(p, ax, y, 22.0, "A: Original", Hit::Compare(false), !a.candidate, true, "Hear the project without the experiment (its EQ band bypassed, or the fader as it was)");
                    ax = self.button_at(
                        p,
                        ax,
                        y,
                        22.0,
                        "B: Candidate",
                        Hit::Compare(true),
                        a.candidate,
                        true,
                        "Hear the experiment",
                    );
                    ax = self.button_at(p, ax, y, 22.0, "Loudness matched", Hit::Match, a.matching, true, "Play the candidate at the original's loudness (a gain on the monitor only, never in the project)");
                    let _ = self.button_at(
                        p,
                        ax,
                        y,
                        22.0,
                        "Reject",
                        Hit::Reject,
                        false,
                        true,
                        "Take the experiment out: the project as it was",
                    );
                    y += 28.0;
                    let text = match (&a.measured, a.measuring) {
                        (_, true) => "Measuring the loudness with and without it…".to_string(),
                        (Some(Ok(m)), _) => {
                            let band = m
                                .band_change_db
                                .map(|d| format!(", {d:+.1} dB in the range"))
                                .unwrap_or_default();
                            let played = if a.matching && a.candidate {
                                format!(
                                    " Played {:+.1} dB on the monitor to match (the project is not changed).",
                                    a.trim_db
                                )
                            } else {
                                " Played at its own level.".to_string()
                            };
                            format!(
                                "The candidate measures {:+.1} LU against the original (BS.1770 integrated, master, {:.1} s{band}).{played}",
                                m.change(),
                                m.seconds
                            )
                        }
                        (Some(Err(e)), _) => format!("No loudness match: {e}."),
                        (None, false) => String::new(),
                    };
                    y = self.para(
                        p,
                        x + 8.0,
                        y,
                        w - 16.0,
                        &text,
                        th.fonts.small,
                        th.ui.text_dim,
                    );
                }
            } else {
                y = self.para(
                    p,
                    x + 8.0,
                    y + 2.0,
                    w - 16.0,
                    "Manual: nothing in the project is changed by it.",
                    th.fonts.tiny,
                    th.ui.text_faint,
                );
            }
            p.stroke_rounded(
                Rect::new(x, card_top + 2.0, w, (y - card_top).max(10.0) + 4.0),
                4.0,
                1.0,
                th.ui.border.with_alpha(0.6),
            );
            y += 10.0;
        }
        p.pop_clip();
        self.visible = None;
        self.detail_content = y + self.detail_scroll - rect.y + PAD;
        self.detail_scroll = self
            .detail_scroll
            .clamp(0.0, (self.detail_content - rect.h).max(0.0));
    }

    fn paint_roles(&mut self, p: &mut dyn Painter, rect: Rect, model: &Session) {
        let th = self.theme.clone();
        p.fill(rect, th.ui.background);
        self.detail = rect;
        let (x, w) = (rect.x + PAD, rect.w - 2.0 * PAD);
        p.push_clip(rect);
        self.visible = Some(rect);
        let mut y = rect.y + PAD - self.detail_scroll;
        y = self.para(
            p,
            x,
            y,
            w,
            "What each track plays. The analysis weighs pairs by roles (a lead vocal over its \
             accompaniment, a kick against its bass; parts of one drum kit are not compared). \
             Track names are never read as roles; tracks without one are analysed neutrally.",
            th.fonts.small,
            th.ui.text_dim,
        );
        y += 6.0;
        for (t, name, role) in model.mix_tracks() {
            let r = Rect::new(x, y, w, 24.0);
            p.text(
                &name,
                Rect::new(x, y, w * 0.5, 24.0),
                &TextStyle::new(th.fonts.small, th.ui.text),
            );
            let label = role.map_or("No role", TrackRole::label);
            self.button(
                p,
                Rect::new(x + w * 0.5, y + 2.0, w * 0.5, 20.0),
                label,
                Hit::Role(t),
                role.is_some(),
                true,
                "Choose what the track plays",
            );
            p.hline(r.x, r.right(), r.bottom(), th.ui.border.with_alpha(0.4));
            y += 26.0;
        }
        p.pop_clip();
        self.visible = None;
        self.detail_content = y + self.detail_scroll - rect.y + PAD;
    }

    /// Every subject's levels in a table, the master's sections, the
    /// tracks left out: measurements, no judgement.
    fn paint_levels(&mut self, p: &mut dyn Painter, rect: Rect, model: &Session) {
        let th = self.theme.clone();
        p.fill(rect, th.ui.background);
        self.detail = rect;
        let Some(r) = model.mix_results() else {
            self.detail_content = 0.0;
            return;
        };
        let (x, w) = (rect.x + PAD, rect.w - 2.0 * PAD);
        p.push_clip(rect);
        self.visible = Some(rect);
        let mut y = rect.y + PAD - self.detail_scroll;
        y = self.para(
            p,
            x,
            y,
            w,
            &format!(
                "{} Peaks in dBFS, true peak in dBTP (4× oversampled), RMS in dBFS (a \
                 full-scale sine reads −3), crest = peak − RMS, loudness BS.1770-4.",
                r.signal.describe()
            ),
            th.fonts.small,
            th.ui.text_dim,
        );
        y += 6.0;
        const COLUMNS: [&str; 8] = [
            "Peak",
            "True peak",
            "RMS",
            "Crest",
            "LUFS",
            "LRA",
            "Max ST",
            "Corr.",
        ];
        let name_w = (w * 0.28).max(90.0);
        let col = (w - name_w) / COLUMNS.len() as f32;
        let head = TextStyle::new(th.fonts.tiny, th.ui.text_faint).right();
        for (i, c) in COLUMNS.iter().enumerate() {
            p.text(
                c,
                Rect::new(x + name_w + i as f32 * col, y, col - 4.0, LINE),
                &head,
            );
        }
        y += LINE;
        p.hline(x, x + w, y, th.ui.border);
        let num = |v: f64, digits: usize| {
            if v.is_finite() {
                format!("{v:.digits$}")
            } else {
                "—".to_string()
            }
        };
        for sm in &r.report.summaries {
            let label = match sm.subject {
                Subject::Track(t) => model.mix_track_name(t),
                Subject::Master(_) => "Master (mix)".to_string(),
                Subject::Pair { target, .. } => model.mix_track_name(target),
            };
            let silent = !sm.sample_peak.is_finite();
            let style = TextStyle::new(
                th.fonts.small,
                if silent { th.ui.text_faint } else { th.ui.text },
            );
            p.text(
                &if silent {
                    format!("{label} (silent)")
                } else {
                    label
                },
                Rect::new(x, y + 2.0, name_w, LINE),
                &style,
            );
            let cells = [
                num(sm.sample_peak, 1),
                num(sm.true_peak, 1),
                num(sm.rms, 1),
                num(sm.crest, 1),
                sm.integrated.map_or("—".into(), |v| num(v, 1)),
                sm.loudness_range.map_or("—".into(), |v| num(v, 1)),
                sm.max_short_term.map_or("—".into(), |v| num(v, 1)),
                num(sm.correlation, 2),
            ];
            for (i, c) in cells.iter().enumerate() {
                p.text(
                    c,
                    Rect::new(x + name_w + i as f32 * col, y + 2.0, col - 4.0, LINE),
                    &style.right(),
                );
            }
            y += LINE + 4.0;
        }
        if let Some(master) = r
            .report
            .summaries
            .iter()
            .find(|x| matches!(x.subject, Subject::Master(_)))
            && !master.sections.is_empty()
        {
            y = self.heading(
                p,
                x,
                y,
                w,
                "Master loudness by section (BS.1770 integrated)",
            );
            for (n, l) in &master.sections {
                p.text(
                    n,
                    Rect::new(x, y, name_w, LINE),
                    &TextStyle::new(th.fonts.small, th.ui.text_dim),
                );
                p.text(
                    &l.map_or("too short or silent".into(), |v| format!("{v:.1} LUFS")),
                    Rect::new(x + name_w, y, col * 2.0, LINE),
                    &TextStyle::new(th.fonts.small, th.ui.text),
                );
                y += LINE;
            }
        }
        if !r.skipped.is_empty() {
            y = self.heading(p, x, y, w, "Left out");
            for (t, why) in &r.skipped {
                y = self.para(
                    p,
                    x,
                    y,
                    w,
                    &format!("{}: {why}", model.mix_track_name(*t)),
                    th.fonts.small,
                    th.ui.text_dim,
                );
            }
        }
        p.pop_clip();
        self.visible = None;
        self.detail_content = y + self.detail_scroll - rect.y + PAD;
        self.detail_scroll = self
            .detail_scroll
            .clamp(0.0, (self.detail_content - rect.h).max(0.0));
    }

    fn role_menu(track: TrackId, now: Option<TrackRole>) -> Vec<MenuItem<Action>> {
        let set = |role| Action::Edit(Command::SetTrackRole { track, role });
        std::iter::once(MenuItem::new("No Role", set(None)).checked(now.is_none()))
            .chain(
                TrackRole::ALL
                    .iter()
                    .map(|r| MenuItem::new(r.label(), set(Some(*r))).checked(now == Some(*r))),
            )
            .collect()
    }

    fn activate(&mut self, hit: Hit, pos: Point, model: &Session, cx: &mut EventCx<'_, Action>) {
        match hit {
            Hit::Analyse(scope) => cx.emit(Action::Mix(MixOp::Analyse {
                scope,
                signal: self.signal,
            })),
            Hit::Cancel => cx.emit(Action::Mix(MixOp::Cancel)),
            Hit::Signal(s) => {
                self.signal = s;
                cx.redraw();
            }
            Hit::Roles | Hit::Levels => {
                let want = if hit == Hit::Roles {
                    Panel::Roles
                } else {
                    Panel::Levels
                };
                self.panel = if self.panel == want {
                    Panel::Details
                } else {
                    want
                };
                self.detail_scroll = 0.0;
                cx.redraw();
            }
            Hit::Category(c) => {
                self.category = c;
                self.list_scroll = 0.0;
                cx.redraw();
            }
            Hit::Sort => {
                self.sort = self.sort.next();
                cx.redraw();
            }
            Hit::TrackFilter => {
                let tracks: Vec<TrackId> = model
                    .mix_results()
                    .map(|r| r.tracks.clone())
                    .unwrap_or_default();
                self.track = match self.track.and_then(|t| tracks.iter().position(|x| *x == t)) {
                    None => tracks.first().copied(),
                    Some(i) => tracks.get(i + 1).copied(),
                };
                self.list_scroll = 0.0;
                cx.redraw();
            }
            Hit::Row(id) => {
                self.detail_scroll = 0.0;
                self.panel = Panel::Details;
                cx.emit(Action::Mix(MixOp::Select(Some(id))));
                cx.emit(Action::Mix(MixOp::Show(id)));
            }
            Hit::ShowDismissed => cx.emit(Action::Mix(MixOp::ShowDismissed(
                !model.mix_shows_dismissed(),
            ))),
            Hit::Show(id) => cx.emit(Action::Mix(MixOp::Show(id))),
            Hit::Dismiss(id) => cx.emit(Action::Mix(MixOp::Dismiss(id))),
            Hit::Event(finding, event) => cx.emit(Action::Mix(MixOp::ShowEvent { finding, event })),
            Hit::Audition(finding, recommendation) => {
                let here = model
                    .mix_audition()
                    .is_some_and(|a| a.finding == finding && a.recommendation == recommendation);
                cx.emit(Action::Mix(if here {
                    MixOp::Reject
                } else {
                    MixOp::Audition {
                        finding,
                        recommendation,
                    }
                }));
            }
            Hit::Apply(finding, recommendation) => cx.emit(Action::Mix(MixOp::Apply {
                finding,
                recommendation,
            })),
            Hit::Compare(on) => cx.emit(Action::Mix(MixOp::Compare(on))),
            Hit::Match => {
                let on = model.mix_audition().is_some_and(|a| a.matching);
                cx.emit(Action::Mix(MixOp::LoudnessMatch(!on)));
            }
            Hit::Reject => cx.emit(Action::Mix(MixOp::Reject)),
            Hit::Role(t) => {
                let now = model.project().track(t).and_then(|t| t.role);
                cx.request(HostRequest::ContextMenu {
                    at: pos,
                    items: Self::role_menu(t, now),
                });
            }
        }
    }
}

impl CanvasView<Session, Action> for MixView {
    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.theme = theme.clone();
        self.spots.clear();
        p.fill(Rect::from_size(size), theme.ui.background);
        self.paint_header(p, size, model);
        self.paint_status(p, size, model);
        let top = HEADER_H + STATUS_H;
        let body = Rect::new(0.0, top, size.w, (size.h - top).max(0.0));
        let split = (size.w * 0.42).clamp(240.0_f32.min(size.w), (size.w - 260.0).max(240.0));
        let list = Rect::new(0.0, body.y, split, body.h);
        let detail = Rect::new(split + 1.0, body.y, (size.w - split - 1.0).max(0.0), body.h);
        self.paint_list(p, list, model);
        p.vline(split, body.y, body.bottom(), theme.ui.border);
        // Scrolled as far as the content went last time (set_scroll may
        // ask for more).
        self.detail_scroll = self
            .detail_scroll
            .clamp(0.0, (self.detail_content - detail.h).max(0.0));
        match self.panel {
            Panel::Roles => self.paint_roles(p, detail, model),
            Panel::Levels => self.paint_levels(p, detail, model),
            Panel::Details => self.paint_details(p, detail, model),
        }
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        _size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary | PointerButton::Secondary,
                ..
            } => {
                let Some(s) = self.spot_at(pos).cloned() else {
                    return false;
                };
                if s.enabled {
                    self.activate(s.hit, pos, model, cx);
                }
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let hover = self.spot_at(pos).map(|s| s.hit);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                false
            }
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll {
                pos, dy, precise, ..
            } => {
                let d = if precise { dy } else { dy * LINE * 3.0 };
                if self.list.contains(pos) {
                    self.list_scroll = (self.list_scroll + d)
                        .clamp(0.0, (self.list_content - self.list.h).max(0.0));
                } else if self.detail.contains(pos) {
                    self.detail_scroll = (self.detail_scroll + d)
                        .clamp(0.0, (self.detail_content - self.detail.h).max(0.0));
                } else {
                    return false;
                }
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, _size: Size, _model: &Session) -> Option<String> {
        self.spot_at(pos)
            .map(|s| s.tip.clone())
            .filter(|t| !t.is_empty())
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn wants_frames(&self, model: &Session) -> bool {
        // Progress, and the audition's measurement.
        model.mix_analysing() || model.mix_audition().is_some_and(|a| a.measuring)
    }

    fn min_size(&self) -> Size {
        Size::new(520.0, 220.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, _size: Size, _model: &Session) -> Option<ScrollInfo> {
        (axis == ScrollAxis::Vertical).then_some(ScrollInfo {
            content: self.detail_content,
            viewport: self.detail.h,
            offset: self.detail_scroll,
            start: self.detail.y,
            end: 0.0,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Vertical {
            self.detail_scroll = offset.max(0.0);
        }
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Mix Intelligence".into())
    }

    /// The buttons, the findings as a list (the selected one selected), the
    /// experiments' buttons — what was painted last.
    fn accessible(&self, _size: Size, _model: &Session) -> Vec<AccessNode<Action>> {
        let mut rows = Vec::new();
        let mut rest = Vec::new();
        for (i, s) in self.spots.iter().enumerate() {
            let action = match s.hit {
                Hit::Analyse(scope) => Some(Action::Mix(MixOp::Analyse {
                    scope,
                    signal: self.signal,
                })),
                Hit::Cancel => Some(Action::Mix(MixOp::Cancel)),
                Hit::Row(id) => Some(Action::Mix(MixOp::Select(Some(id)))),
                Hit::Show(id) => Some(Action::Mix(MixOp::Show(id))),
                Hit::Dismiss(id) => Some(Action::Mix(MixOp::Dismiss(id))),
                Hit::Event(finding, event) => {
                    Some(Action::Mix(MixOp::ShowEvent { finding, event }))
                }
                Hit::Audition(finding, recommendation) => Some(Action::Mix(MixOp::Audition {
                    finding,
                    recommendation,
                })),
                Hit::Apply(finding, recommendation) => Some(Action::Mix(MixOp::Apply {
                    finding,
                    recommendation,
                })),
                Hit::Compare(on) => Some(Action::Mix(MixOp::Compare(on))),
                Hit::Reject => Some(Action::Mix(MixOp::Reject)),
                _ => None,
            };
            let role = match s.hit {
                Hit::Row(_) => AccessRole::ListItem,
                Hit::Signal(_) | Hit::Category(_) | Hit::Match | Hit::Roles | Hit::Levels => {
                    AccessRole::ToggleButton
                }
                _ => AccessRole::Button,
            };
            let mut node = AccessNode::new(access_id(&[78, i as u64]), role, s.label.clone())
                .at(s.rect)
                .described(s.tip.clone());
            node = if role == AccessRole::ListItem {
                node.selected(s.on)
            } else if role == AccessRole::ToggleButton {
                node.checked(s.on)
            } else {
                node
            };
            if s.enabled
                && let Some(a) = action
            {
                node = node.on_activate(a);
            }
            if role == AccessRole::ListItem {
                rows.push(node);
            } else {
                rest.push(node);
            }
        }
        let mut out = vec![
            AccessNode::new(
                access_id(&[77]),
                AccessRole::List,
                "Mix Intelligence findings",
            )
            .at(self.list)
            .with_children(rows),
        ];
        out.extend(rest);
        out
    }
}

#[cfg(test)]
mod tests;
