//! The performance meter.
//!
//! Shows how much of each audio callback's time budget the engine uses: in
//! total (with a 60-second history and a breakdown into plugins, track
//! mixing and engine work), per track and per plugin instance. Loads are
//! shares of the buffer's duration — 100 % means the callback took as long
//! as the audio it produced. The figures come from
//! [`Session::performance`]; painting this view is what keeps the per-node
//! measurement switched on.

use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_project::{Command, PluginFormat, TrackColor, TrackKind};
use faderframe_session::{
    Action, Load, PerformanceReport, PluginPerformance, SelectMode, Session, TrackPerformance,
};
use faderframe_ui_canvas::{
    CanvasView, Color, Cursor, EventCx, HostRequest, MenuItem, Paint, Painter, Path, Point,
    PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};
use std::collections::HashSet;

#[cfg(test)]
mod tests;

/// Which table is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Tracks with their plugins underneath.
    Tracks,
    /// Every plugin instance, heaviest first.
    Plugins,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    /// Highest average load first.
    Load,
    /// Highest peak first.
    Peak,
    Name,
    /// Project order.
    Order,
}

/// One table row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// Index into `report.tracks`.
    Track(usize),
    /// Track index, plugin index.
    Plugin(usize, usize),
    /// The track's own work (playback, mixing) below its plugins.
    Mixing(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Mode(Mode),
    Reset,
    SortBy(Sort),
    Chevron(usize),
    Row(Row),
}

const PAD: f32 = 12.0;
const TOOLBAR_H: f32 = 34.0;
const HEADER_H: f32 = 24.0;
const BREAKDOWN_H: f32 = 30.0;
const NAME_W: f32 = 250.0;
const NUM_W: f32 = 74.0;
const INFO_W: f32 = 132.0;

pub struct PerformanceView {
    theme: Theme,
    mode: Mode,
    sort: Sort,
    collapsed: HashSet<TrackId>,
    scroll: f32,
    hover: Option<Point>,
}

/// "23 %", "3.4 %", "<0.1 %", "—".
pub fn format_load(load: f64) -> String {
    let pct = load * 100.0;
    if pct <= 0.0 {
        "—".into()
    } else if pct < 0.1 {
        "<0.1 %".into()
    } else if pct < 10.0 {
        format!("{pct:.1} %")
    } else {
        format!("{pct:.0} %")
    }
}

fn track_color(c: TrackColor) -> Color {
    Color::rgb8(c.r, c.g, c.b)
}

fn format_tag(f: PluginFormat) -> &'static str {
    match f {
        PluginFormat::Builtin => "BUILT-IN",
        PluginFormat::Clap => "CLAP",
        PluginFormat::Vst3 => "VST3",
        PluginFormat::AudioUnit => "AU",
    }
}

fn kind_tag(k: TrackKind) -> &'static str {
    match k {
        TrackKind::Audio => "AUDIO",
        TrackKind::Instrument => "INST",
        TrackKind::Midi => "MIDI",
        TrackKind::Bus => "BUS",
        TrackKind::Aux => "AUX",
        TrackKind::Master => "MASTER",
    }
}

/// The smallest of 5/10/25/50/100 % (or more) that fits `peak`: bars use
/// one scale per table so rows compare.
fn bar_scale(peak: f64) -> f64 {
    [0.05, 0.10, 0.25, 0.50, 1.0]
        .into_iter()
        .find(|s| peak <= *s)
        .unwrap_or_else(|| peak.ceil().max(1.0))
}

struct Layout {
    toolbar: Rect,
    summary: Rect,
    breakdown: Rect,
    header: Rect,
    table: Rect,
}

impl PerformanceView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            mode: Mode::Tracks,
            sort: Sort::Load,
            collapsed: HashSet::new(),
            scroll: 0.0,
            hover: None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn sort(&self) -> Sort {
        self.sort
    }

    fn layout(&self, size: Size) -> Layout {
        let mut r = Rect::from_size(size);
        let toolbar = r.take_top(TOOLBAR_H);
        let summary = r.take_top(self.theme.perf.summary_height.min(size.h * 0.45));
        let breakdown = r.take_top(BREAKDOWN_H);
        let header = r.take_top(HEADER_H);
        Layout {
            toolbar,
            summary,
            breakdown,
            header,
            table: r,
        }
    }

    fn toolbar_parts(&self, toolbar: Rect) -> ([(Mode, Rect); 2], Rect) {
        let y = toolbar.y + 6.0;
        let h = toolbar.h - 12.0;
        let tracks = Rect::new(toolbar.x + PAD, y, 84.0, h);
        let plugins = Rect::new(tracks.right(), y, 84.0, h);
        let reset = Rect::new(toolbar.right() - PAD - 92.0, y, 92.0, h);
        ([(Mode::Tracks, tracks), (Mode::Plugins, plugins)], reset)
    }

    /// Columns: name, bar, average, peak, info.
    fn columns(&self, row: Rect) -> [Rect; 5] {
        let mut r = row.inset_xy(PAD, 0.0);
        let name = r.take_left(NAME_W.min(r.w * 0.4));
        let info = r.take_right(INFO_W.min(r.w * 0.25));
        let peak = r.take_right(NUM_W);
        let avg = r.take_right(NUM_W);
        [name, r.inset_xy(10.0, 0.0), avg, peak, info]
    }

    fn sorted_tracks(&self, report: &PerformanceReport) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..report.tracks.len()).collect();
        let t = &report.tracks;
        match self.sort {
            Sort::Load => idx.sort_by(|a, b| t[*b].load.average.total_cmp(&t[*a].load.average)),
            Sort::Peak => idx.sort_by(|a, b| t[*b].load.peak.total_cmp(&t[*a].load.peak)),
            Sort::Name => idx.sort_by_key(|i| t[*i].name.to_lowercase()),
            Sort::Order => {}
        }
        idx
    }

    fn rows(&self, report: &PerformanceReport) -> Vec<Row> {
        match self.mode {
            Mode::Tracks => {
                let mut rows = Vec::new();
                for ti in self.sorted_tracks(report) {
                    let t = &report.tracks[ti];
                    rows.push(Row::Track(ti));
                    if !t.plugins.is_empty() && !self.collapsed.contains(&t.track) {
                        let mut plugins: Vec<usize> = (0..t.plugins.len()).collect();
                        if matches!(self.sort, Sort::Load | Sort::Peak) {
                            let key = |p: &PluginPerformance| match self.sort {
                                Sort::Peak => p.load.peak,
                                _ => p.load.average,
                            };
                            plugins.sort_by(|a, b| {
                                key(&t.plugins[*b]).total_cmp(&key(&t.plugins[*a]))
                            });
                        }
                        rows.extend(plugins.into_iter().map(|pi| Row::Plugin(ti, pi)));
                        rows.push(Row::Mixing(ti));
                    }
                }
                rows
            }
            Mode::Plugins => {
                let mut all: Vec<(usize, usize)> = report
                    .tracks
                    .iter()
                    .enumerate()
                    .flat_map(|(ti, t)| (0..t.plugins.len()).map(move |pi| (ti, pi)))
                    .collect();
                let p = |&(ti, pi): &(usize, usize)| &report.tracks[ti].plugins[pi];
                match self.sort {
                    Sort::Load => {
                        all.sort_by(|a, b| p(b).load.average.total_cmp(&p(a).load.average))
                    }
                    Sort::Peak => all.sort_by(|a, b| p(b).load.peak.total_cmp(&p(a).load.peak)),
                    Sort::Name => all.sort_by_key(|x| p(x).name.to_lowercase()),
                    Sort::Order => {}
                }
                all.into_iter()
                    .map(|(ti, pi)| Row::Plugin(ti, pi))
                    .collect()
            }
        }
    }

    fn row_height(&self, row: Row) -> f32 {
        match row {
            Row::Track(_) => self.theme.perf.row_height,
            Row::Plugin(..) | Row::Mixing(_) => match self.mode {
                Mode::Tracks => self.theme.perf.plugin_row_height,
                Mode::Plugins => self.theme.perf.row_height,
            },
        }
    }

    fn content_height(&self, report: &PerformanceReport) -> f32 {
        self.rows(report).iter().map(|r| self.row_height(*r)).sum()
    }

    /// Rows with their rectangles (in view coordinates, scrolled).
    fn row_rects(&self, report: &PerformanceReport, table: Rect) -> Vec<(Row, Rect)> {
        let mut y = table.y - self.scroll;
        let mut out = Vec::new();
        for row in self.rows(report) {
            let h = self.row_height(row);
            let r = Rect::new(table.x, y, table.w, h);
            y += h;
            if r.bottom() >= table.y && r.y <= table.bottom() {
                out.push((row, r));
            }
        }
        out
    }

    fn max_scroll(&self, report: &PerformanceReport, size: Size) -> f32 {
        let l = self.layout(size);
        (self.content_height(report) - l.table.h).max(0.0)
    }

    fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        let report = model.performance();
        let l = self.layout(size);
        let (modes, reset) = self.toolbar_parts(l.toolbar);
        for (m, r) in modes {
            if r.contains(pos) {
                return Some(Hit::Mode(m));
            }
        }
        if reset.contains(pos) {
            return Some(Hit::Reset);
        }
        if l.header.contains(pos) {
            let [name, bar, avg, peak, _] = self.columns(l.header);
            if name.contains(pos) {
                return Some(Hit::SortBy(if self.sort == Sort::Name {
                    Sort::Order
                } else {
                    Sort::Name
                }));
            }
            if bar.contains(pos) || avg.contains(pos) {
                return Some(Hit::SortBy(Sort::Load));
            }
            if peak.contains(pos) {
                return Some(Hit::SortBy(Sort::Peak));
            }
            return None;
        }
        if !l.table.contains(pos) {
            return None;
        }
        let (row, rect) = self
            .row_rects(report, l.table)
            .into_iter()
            .find(|(_, r)| r.contains(pos))?;
        if let Row::Track(ti) = row
            && self.mode == Mode::Tracks
            && !report.tracks[ti].plugins.is_empty()
            && pos.x < rect.x + PAD + 22.0
        {
            return Some(Hit::Chevron(ti));
        }
        Some(Hit::Row(row))
    }

    fn toggle_collapsed(&mut self, track: TrackId) {
        if !self.collapsed.remove(&track) {
            self.collapsed.insert(track);
        }
    }

    fn plugin_menu(t: &TrackPerformance, p: &PluginPerformance, at: Point) -> HostRequest<Action> {
        let builtin = p.format == PluginFormat::Builtin;
        let mut items = vec![MenuItem::disabled(format!("{} — {}", p.name, t.name))];
        items.push(
            MenuItem::new(
                if builtin {
                    "Show Editor"
                } else {
                    "Show Plugin GUI"
                },
                Action::OpenPluginEditor {
                    track: t.track,
                    plugin: p.plugin,
                    generic: false,
                },
            )
            .separated(),
        );
        if !builtin {
            items.push(MenuItem::new(
                "Show Parameters",
                Action::OpenPluginEditor {
                    track: t.track,
                    plugin: p.plugin,
                    generic: true,
                },
            ));
        }
        items.push(MenuItem::new(
            if p.bypassed { "Enable" } else { "Bypass" },
            Action::Edit(Command::SetPluginBypass {
                track: t.track,
                plugin: p.plugin,
                bypass: !p.bypassed,
            }),
        ));
        HostRequest::ContextMenu { at, items }
    }

    // --- painting ---------------------------------------------------------------

    fn paint_bar(&self, p: &mut dyn Painter, r: Rect, load: Load, scale: f64) {
        let pt = &self.theme.perf;
        let track = Rect::new(r.x, r.center().y - 4.0, r.w, 8.0);
        p.fill_rounded(track, 3.0, &Paint::Solid(pt.bar_track));
        let w = |v: f64| (v / scale).clamp(0.0, 1.0) as f32 * track.w;
        let avg = w(load.average);
        if avg > 0.5 {
            let fill = Rect::new(track.x, track.y, avg.max(2.0), track.h);
            let c = pt.load_color(load.average);
            p.fill_rounded(
                fill,
                3.0,
                &Paint::vertical(fill, c.lighten(0.15), c.darken(0.1)),
            );
        }
        let peak = w(load.peak);
        if peak > 0.5 {
            p.fill(
                Rect::new(track.x + peak - 1.0, track.y - 2.0, 2.0, track.h + 4.0),
                pt.load_color(load.peak).mix(pt.peak_mark, 0.5),
            );
        }
    }

    fn paint_toolbar(&self, p: &mut dyn Painter, r: Rect, report: &PerformanceReport) {
        let t = &self.theme;
        p.fill(r, t.perf.header);
        p.hline(r.x, r.right(), r.bottom() - 0.5, t.ui.border);
        let (modes, reset) = self.toolbar_parts(r);
        for (i, (m, mr)) in modes.iter().enumerate() {
            let on = *m == self.mode;
            let bg = if on {
                t.ui.accent.with_alpha(0.22)
            } else {
                t.perf.panel
            };
            p.fill_rounded(*mr, 4.0, &Paint::Solid(bg));
            if on {
                p.fill(
                    Rect::new(mr.x + 6.0, mr.bottom() - 2.0, mr.w - 12.0, 2.0),
                    t.ui.accent,
                );
            }
            let label = match m {
                Mode::Tracks => "Tracks",
                Mode::Plugins => "Plugins",
            };
            p.text(
                label,
                *mr,
                &TextStyle::new(t.fonts.normal, if on { t.ui.text } else { t.ui.text_dim })
                    .center(),
            );
            if i == 0 {
                p.vline(mr.right(), mr.y + 4.0, mr.bottom() - 4.0, t.ui.border);
            }
        }
        p.fill_rounded(reset, 4.0, &Paint::Solid(t.perf.panel));
        p.stroke_rounded(reset, 4.0, 1.0, t.ui.border);
        p.text(
            "Reset Peaks",
            reset,
            &TextStyle::new(t.fonts.small, t.ui.text_dim).center(),
        );
        let format = if report.sample_rate > 0 && report.buffer_size > 0 {
            format!(
                "{} Hz · {} frames · {:.2} ms per buffer",
                report.sample_rate,
                report.buffer_size,
                report.buffer_size as f64 * 1000.0 / report.sample_rate as f64
            )
        } else {
            "no audio stream".into()
        };
        let mid = Rect::new(
            modes[1].1.right() + 16.0,
            r.y,
            (reset.x - modes[1].1.right() - 32.0).max(0.0),
            r.h,
        );
        p.text(
            &format,
            mid,
            &TextStyle::new(t.fonts.small, t.ui.text_faint).right(),
        );
    }

    fn paint_summary(&self, p: &mut dyn Painter, r: Rect, report: &PerformanceReport) {
        let t = &self.theme;
        let pt = &t.perf;
        p.fill(r, pt.panel);
        let inner = r.inset(PAD);
        let (mut left, graph) = inner.split_left(250.0f32.min(inner.w * 0.42));
        // Big total figure.
        let label = left.take_top(14.0);
        p.text(
            "DSP LOAD",
            label,
            &TextStyle::new(t.fonts.tiny, t.ui.text_faint).bold(),
        );
        let big = left.take_top(36.0);
        let total = report.total;
        p.text(
            &format_load(total.average),
            big,
            &TextStyle::new(30.0, pt.load_color(total.average)).bold(),
        );
        let bar = left.take_top(14.0);
        self.paint_bar(p, Rect::new(bar.x, bar.y, bar.w - 8.0, bar.h), total, 1.0);
        left.take_top(6.0);
        let line = |p: &mut dyn Painter, r: Rect, s: &str, c: Color| {
            p.text(s, r, &TextStyle::new(t.fonts.small, c));
        };
        let l1 = left.take_top(16.0);
        line(
            p,
            l1,
            &format!(
                "peak {} · p99 {} · max {}",
                format_load(total.peak),
                format_load(report.p99_load),
                format_load(report.max_load)
            ),
            t.ui.text_dim,
        );
        let l2 = left.take_top(16.0);
        let problems = report.xruns + report.deadline_misses;
        line(
            p,
            l2,
            &format!(
                "{} xruns · {} late callbacks · {} late disk reads",
                report.xruns, report.deadline_misses, report.late_disk_reads
            ),
            if problems > 0 {
                pt.load_high
            } else {
                t.ui.text_faint
            },
        );
        let l3 = left.take_top(16.0);
        line(
            p,
            l3,
            &format!(
                "{} nodes · {} levels · {} smp output latency",
                report.graph_nodes, report.graph_levels, report.output_latency
            ),
            t.ui.text_faint,
        );
        self.paint_history(p, graph, report);
    }

    fn paint_history(&self, p: &mut dyn Painter, r: Rect, report: &PerformanceReport) {
        let t = &self.theme;
        let pt = &t.perf;
        let (r, axis) = r.split_bottom(14.0);
        p.fill_rounded(r, 4.0, &Paint::Solid(pt.bar_track));
        // Same scale steps as the bars: light loads stay readable.
        let top = bar_scale(
            report
                .history
                .iter()
                .map(|l| l.peak.max(l.average))
                .fold(0.0f64, f64::max),
        );
        let y_of = |v: f64| r.bottom() - (v / top).clamp(0.0, 1.0) as f32 * (r.h - 2.0) - 1.0;
        for q in [0.25, 0.5, 0.75, 1.0] {
            let y = y_of(top * q);
            p.hline(r.x, r.right(), y, pt.graph_grid);
            p.text(
                &format_load(top * q),
                Rect::new(r.right() - 54.0, y, 50.0, 12.0),
                &TextStyle::new(t.fonts.tiny, t.ui.text_faint).right(),
            );
        }
        if top > 1.0 {
            // Over budget: mark 100 %.
            let y = y_of(1.0);
            p.hline(r.x, r.right(), y, pt.load_critical.with_alpha(0.6));
        }
        let n = faderframe_session::performance::PERF_HISTORY;
        let step = r.w / (n.max(2) - 1) as f32;
        let h = &report.history;
        if h.len() >= 2 {
            let x_of = |i: usize| r.right() - (h.len() - 1 - i) as f32 * step;
            let mut area = Path::new();
            area.move_to(Point::new(x_of(0), r.bottom()));
            for (i, l) in h.iter().enumerate() {
                area.line_to(Point::new(x_of(i), y_of(l.average)));
            }
            area.line_to(Point::new(x_of(h.len() - 1), r.bottom()));
            p.fill_path(&area, pt.graph_avg.with_alpha(0.28));
            let mut avg = Path::new();
            let mut peak = Path::new();
            for (i, l) in h.iter().enumerate() {
                let (pa, pp) = (
                    Point::new(x_of(i), y_of(l.average)),
                    Point::new(x_of(i), y_of(l.peak)),
                );
                if i == 0 {
                    avg.move_to(pa);
                    peak.move_to(pp);
                } else {
                    avg.line_to(pa);
                    peak.line_to(pp);
                }
            }
            p.stroke_path(&avg, 1.5, pt.graph_avg);
            p.stroke_path(&peak, 1.0, pt.graph_peak.with_alpha(0.85));
        } else {
            p.text(
                "collecting…",
                r,
                &TextStyle::new(t.fonts.small, t.ui.text_faint).center(),
            );
        }
        let secs = n as f32 * faderframe_session::performance::PERF_POLL.as_secs_f32();
        let style = TextStyle::new(t.fonts.tiny, t.ui.text_faint);
        p.text(&format!("−{secs:.0} s"), axis, &style);
        p.text("now", axis, &style.right());
        p.text(
            "▬ average   ▬ peak",
            axis,
            &TextStyle::new(t.fonts.tiny, t.ui.text_faint).center(),
        );
    }

    fn paint_breakdown(&self, p: &mut dyn Painter, r: Rect, report: &PerformanceReport) {
        let t = &self.theme;
        let pt = &t.perf;
        p.fill(r, pt.panel);
        p.hline(r.x, r.right(), r.bottom() - 0.5, t.ui.border);
        let plugins: f64 = report
            .tracks
            .iter()
            .flat_map(|t| t.plugins.iter())
            .map(|p| p.load.average)
            .sum();
        let mixing = (report.graph.average - plugins).max(0.0);
        let engine = report.engine;
        let used = plugins + mixing + engine;
        let free = (1.0 - used).max(0.0);
        let inner = r.inset_xy(PAD, 0.0);
        let (bar, legend) = inner.split_left(inner.w * 0.5);
        let bar = Rect::new(bar.x, bar.center().y - 6.0, bar.w - 12.0, 12.0);
        p.fill_rounded(bar, 3.0, &Paint::Solid(pt.bar_track));
        let total = used.max(1.0);
        let mut x = bar.x;
        for (v, c) in [
            (plugins, pt.load_ok),
            (mixing, pt.load_ok.darken(0.35)),
            (engine, pt.engine_share),
        ] {
            let w = (v / total) as f32 * bar.w;
            if w >= 0.5 {
                p.fill(Rect::new(x, bar.y, w, bar.h), c);
                x += w;
            }
        }
        p.text(
            &format!(
                "plugins {} · mixing {} · engine {} · free {}",
                format_load(plugins),
                format_load(mixing),
                format_load(engine),
                format_load(free)
            ),
            legend,
            &TextStyle::new(t.fonts.small, t.ui.text_dim),
        );
    }

    fn paint_header(&self, p: &mut dyn Painter, r: Rect, scale: f64) {
        let t = &self.theme;
        p.fill(r, t.perf.header);
        p.hline(r.x, r.right(), r.bottom() - 0.5, t.ui.border);
        let [name, bar, avg, peak, info] = self.columns(r);
        let style = |on: bool| {
            TextStyle::new(t.fonts.tiny, if on { t.ui.text } else { t.ui.text_faint }).bold()
        };
        let arrow = |s: Sort| if self.sort == s { " ▾" } else { "" };
        let name_label = match (self.mode, self.sort) {
            (Mode::Tracks, Sort::Order) => "TRACK (PROJECT ORDER)".to_string(),
            (Mode::Tracks, _) => format!("TRACK{}", arrow(Sort::Name)),
            (Mode::Plugins, Sort::Order) => "PLUGIN (PROJECT ORDER)".to_string(),
            (Mode::Plugins, _) => format!("PLUGIN{}", arrow(Sort::Name)),
        };
        p.text(
            &name_label,
            name,
            &style(matches!(self.sort, Sort::Name | Sort::Order)),
        );
        p.text(
            &format!("LOAD · bar = {}", format_load(scale)),
            bar,
            &style(false),
        );
        p.text(
            &format!("AVG{}", arrow(Sort::Load)),
            avg,
            &style(self.sort == Sort::Load).right(),
        );
        p.text(
            &format!("PEAK{}", arrow(Sort::Peak)),
            peak,
            &style(self.sort == Sort::Peak).right(),
        );
        p.text(
            match self.mode {
                Mode::Tracks => "PLUGINS",
                Mode::Plugins => "LATENCY",
            },
            info,
            &style(false).right(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_numbers(
        &self,
        p: &mut dyn Painter,
        cols: &[Rect; 5],
        load: Load,
        scale: f64,
        info: &str,
        info_color: Color,
        dim: bool,
    ) {
        let t = &self.theme;
        self.paint_bar(p, cols[1], load, scale);
        let num = TextStyle::new(t.fonts.small, if dim { t.ui.text_dim } else { t.ui.text });
        p.text(&format_load(load.average), cols[2], &num.right());
        p.text(
            &format_load(load.peak),
            cols[3],
            &TextStyle::new(t.fonts.small, t.ui.text_dim).right(),
        );
        p.text(
            info,
            cols[4],
            &TextStyle::new(t.fonts.small, info_color).right(),
        );
    }

    fn paint_table(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        model: &Session,
        report: &PerformanceReport,
    ) {
        let t = &self.theme;
        let pt = &t.perf;
        p.fill(r, pt.background);
        if !report.running {
            p.text(
                "Audio is stopped — start the audio engine to measure.",
                r.inset(PAD),
                &TextStyle::new(t.fonts.normal, t.ui.text_faint).center(),
            );
            return;
        }
        let scale = self.scale(report);
        p.push_clip(r);
        for (n, (row, rr)) in self.row_rects(report, r).into_iter().enumerate() {
            let hovered = self.hover.is_some_and(|h| rr.contains(h));
            match row {
                Row::Track(ti) => {
                    let tr = &report.tracks[ti];
                    let selected = model.selection.tracks.contains(&tr.track);
                    let bg = if hovered {
                        pt.row_hover
                    } else if selected {
                        t.ui.selection.with_alpha(0.35)
                    } else if n % 2 == 0 {
                        pt.row_a
                    } else {
                        pt.row_b
                    };
                    p.fill(rr, bg);
                    p.fill(Rect::new(rr.x, rr.y, 4.0, rr.h), track_color(tr.color));
                    let cols = self.columns(rr);
                    let mut name = cols[0];
                    if self.mode == Mode::Tracks {
                        let chev = name.take_left(18.0);
                        if !tr.plugins.is_empty() {
                            let open = !self.collapsed.contains(&tr.track);
                            p.text(
                                if open { "▾" } else { "▸" },
                                chev,
                                &TextStyle::new(t.fonts.normal, t.ui.text_dim),
                            );
                        }
                    }
                    let tag = name.take_right(52.0);
                    p.text(
                        &tr.name,
                        name,
                        &TextStyle::new(t.fonts.normal, t.ui.text).bold(),
                    );
                    p.text(
                        kind_tag(tr.kind),
                        tag,
                        &TextStyle::new(t.fonts.tiny, t.ui.text_faint).right(),
                    );
                    let plugins = match tr.plugins.len() {
                        0 => "—".to_string(),
                        1 => "1 plugin".to_string(),
                        n => format!("{n} plugins"),
                    };
                    self.paint_numbers(p, &cols, tr.load, scale, &plugins, t.ui.text_faint, false);
                }
                Row::Plugin(ti, pi) => {
                    let tr = &report.tracks[ti];
                    let pl = &tr.plugins[pi];
                    p.fill(rr, if hovered { pt.row_hover } else { pt.plugin_row });
                    let cols = self.columns(rr);
                    let mut name = cols[0];
                    if self.mode == Mode::Tracks {
                        name.take_left(30.0);
                        p.vline(
                            rr.x + PAD + 9.0,
                            rr.y,
                            rr.bottom(),
                            track_color(tr.color).with_alpha(0.5),
                        );
                    } else {
                        p.fill(Rect::new(rr.x, rr.y, 4.0, rr.h), track_color(tr.color));
                    }
                    let badge = name.take_left(62.0);
                    let b = Rect::new(badge.x, badge.center().y - 7.0, 56.0, 14.0);
                    p.fill_rounded(b, 3.0, &Paint::Solid(pt.bar_track));
                    p.text(
                        format_tag(pl.format),
                        b,
                        &TextStyle::new(t.fonts.tiny, t.ui.text_dim).center(),
                    );
                    let label = match self.mode {
                        Mode::Tracks if pl.instrument => format!("{} (instrument)", pl.name),
                        Mode::Tracks => pl.name.clone(),
                        Mode::Plugins => format!("{} — {}", pl.name, tr.name),
                    };
                    p.text(
                        &label,
                        name,
                        &TextStyle::new(
                            t.fonts.small,
                            if pl.bypassed {
                                t.ui.text_faint
                            } else {
                                t.ui.text
                            },
                        ),
                    );
                    let (info, color) = if pl.failed {
                        ("failed".to_string(), pt.load_critical)
                    } else if pl.bypassed {
                        ("bypassed".to_string(), t.ui.text_faint)
                    } else if pl.latency > 0 {
                        (format!("{} smp latency", pl.latency), pt.graph_peak)
                    } else {
                        ("no latency".to_string(), t.ui.text_faint)
                    };
                    self.paint_numbers(p, &cols, pl.load, scale, &info, color, true);
                }
                Row::Mixing(ti) => {
                    let tr = &report.tracks[ti];
                    p.fill(rr, pt.plugin_row);
                    let cols = self.columns(rr);
                    let mut name = cols[0];
                    name.take_left(30.0 + 62.0);
                    p.vline(
                        rr.x + PAD + 9.0,
                        rr.y,
                        rr.y + rr.h * 0.5,
                        track_color(tr.color).with_alpha(0.5),
                    );
                    p.text(
                        "playback & mixing",
                        name,
                        &TextStyle::new(t.fonts.small, t.ui.text_faint),
                    );
                    self.paint_numbers(p, &cols, tr.mixing, scale, "", t.ui.text_faint, true);
                }
            }
        }
        p.pop_clip();
        if report.tracks.is_empty() {
            p.text(
                "No tracks.",
                r.inset(PAD),
                &TextStyle::new(t.fonts.normal, t.ui.text_faint).center(),
            );
        } else if self.mode == Mode::Plugins && self.rows(report).is_empty() {
            p.text(
                "No plugins in this project.",
                r.inset(PAD),
                &TextStyle::new(t.fonts.normal, t.ui.text_faint).center(),
            );
        }
    }

    /// One bar scale for the table, from the heaviest row's peak.
    fn scale(&self, report: &PerformanceReport) -> f64 {
        let peak = match self.mode {
            Mode::Tracks => report
                .tracks
                .iter()
                .map(|t| t.load.peak.max(t.load.average))
                .fold(0.0, f64::max),
            Mode::Plugins => report
                .tracks
                .iter()
                .flat_map(|t| t.plugins.iter())
                .map(|p| p.load.peak.max(p.load.average))
                .fold(0.0, f64::max),
        };
        bar_scale(peak)
    }
}

impl CanvasView<Session, Action> for PerformanceView {
    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        // Reading the report keeps per-node measurement on while visible.
        let report = model.performance();
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(report, size));
        let l = self.layout(size);
        p.fill(Rect::from_size(size), theme.perf.background);
        self.paint_toolbar(p, l.toolbar, report);
        self.paint_summary(p, l.summary, report);
        self.paint_breakdown(p, l.breakdown, report);
        self.paint_header(p, l.header, self.scale(report));
        self.paint_table(p, l.table, model, report);
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
                button,
                modifiers,
                clicks,
            } => {
                let Some(hit) = self.hit(pos, size, model) else {
                    return false;
                };
                let report = model.performance();
                if button == PointerButton::Secondary {
                    if let Hit::Row(Row::Plugin(ti, pi)) = hit {
                        let t = &report.tracks[ti];
                        cx.request(Self::plugin_menu(t, &t.plugins[pi], pos));
                        return true;
                    }
                    return false;
                }
                match hit {
                    Hit::Mode(m) => {
                        self.mode = m;
                        self.scroll = 0.0;
                    }
                    Hit::Reset => cx.emit(Action::ResetPerformance),
                    Hit::SortBy(s) => self.sort = s,
                    Hit::Chevron(ti) => self.toggle_collapsed(report.tracks[ti].track),
                    Hit::Row(Row::Track(ti)) => {
                        let track = report.tracks[ti].track;
                        if clicks >= 2 && !report.tracks[ti].plugins.is_empty() {
                            self.toggle_collapsed(track);
                        } else {
                            cx.emit(Action::SelectTracks {
                                tracks: vec![track],
                                mode: if modifiers.toggle() {
                                    SelectMode::Toggle
                                } else {
                                    SelectMode::Replace
                                },
                            });
                        }
                    }
                    Hit::Row(Row::Plugin(ti, pi)) => {
                        if clicks >= 2 {
                            let t = &report.tracks[ti];
                            cx.emit(Action::OpenPluginEditor {
                                track: t.track,
                                plugin: t.plugins[pi].plugin,
                                generic: false,
                            });
                        }
                    }
                    Hit::Row(Row::Mixing(_)) => {}
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                self.hover = Some(pos);
                let clickable = matches!(
                    self.hit(pos, size, model),
                    Some(Hit::Mode(_) | Hit::Reset | Hit::SortBy(_) | Hit::Chevron(_))
                );
                cx.set_cursor(if clickable {
                    Cursor::Pointer
                } else {
                    Cursor::Default
                });
                cx.redraw();
                false
            }
            ViewEvent::PointerLeave => {
                self.hover = None;
                cx.redraw();
                false
            }
            ViewEvent::Scroll { dy, precise, .. } => {
                let step = if precise {
                    dy
                } else {
                    dy * 3.0 * self.theme.perf.row_height
                };
                let max = self.max_scroll(model.performance(), size);
                self.scroll = (self.scroll + step).clamp(0.0, max);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.stream_status().is_some_and(|s| s.running)
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let report = model.performance();
        let ms = |load: f64| {
            if report.sample_rate == 0 {
                0.0
            } else {
                load * report.buffer_size as f64 * 1000.0 / report.sample_rate as f64
            }
        };
        let describe = |what: &str, l: Load| {
            format!(
                "{what}\naverage {} ({:.3} ms per buffer) · peak {} ({:.3} ms)",
                format_load(l.average),
                ms(l.average),
                format_load(l.peak),
                ms(l.peak)
            )
        };
        match self.hit(pos, size, model)? {
            Hit::Mode(Mode::Tracks) => Some("Tracks with their plugins".into()),
            Hit::Mode(Mode::Plugins) => Some("Every plugin instance".into()),
            Hit::Reset => Some("Clear peaks, history and callback statistics".into()),
            Hit::SortBy(_) => Some("Sort by this column".into()),
            Hit::Chevron(_) => Some("Show or hide the track's plugins".into()),
            Hit::Row(Row::Track(ti)) => {
                let t = &report.tracks[ti];
                Some(describe(
                    &format!("{}: everything the track does (incl. plugins)", t.name),
                    t.load,
                ))
            }
            Hit::Row(Row::Plugin(ti, pi)) => {
                let t = &report.tracks[ti];
                let pl = &t.plugins[pi];
                Some(format!(
                    "{}\nDouble-click: editor · right-click: more",
                    describe(&format!("{} on {}", pl.name, t.name), pl.load)
                ))
            }
            Hit::Row(Row::Mixing(ti)) => {
                let t = &report.tracks[ti];
                Some(describe(
                    &format!("{}: clip playback, summing, fader, sends", t.name),
                    t.mixing,
                ))
            }
        }
    }

    fn min_size(&self) -> Size {
        Size::new(420.0, 260.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Vertical {
            return None;
        }
        let report = model.performance();
        let l = self.layout(size);
        Some(ScrollInfo {
            content: self.content_height(report),
            viewport: l.table.h,
            offset: self.scroll,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Vertical {
            self.scroll = offset.max(0.0);
        }
    }
}

/// Track and plugin ids in the order the table shows them (for tests).
#[doc(hidden)]
pub fn visible_rows(
    view: &PerformanceView,
    report: &PerformanceReport,
) -> Vec<(TrackId, Option<PluginInstanceId>)> {
    view.rows(report)
        .into_iter()
        .filter_map(|r| match r {
            Row::Track(ti) => Some((report.tracks[ti].track, None)),
            Row::Plugin(ti, pi) => Some((
                report.tracks[ti].track,
                Some(report.tracks[ti].plugins[pi].plugin),
            )),
            Row::Mixing(_) => None,
        })
        .collect()
}
