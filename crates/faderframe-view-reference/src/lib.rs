//! Reference tracks: finished records heard instead of the mix — a
//! loudness-matched A/B in the monitor path — and their spectral balance
//! beside the mix's.
//!
//! The list (left) holds the project's references: a click compares the
//! mix with one, its menu sets how it plays (along the song from its start
//! or from the playhead, or looping its region on its own), renames,
//! measures and removes it; files dropped on the view are added. The
//! header flips between **Mix** and **Reference** (also Ctrl+Alt+A
//! anywhere: `app.compare-flip`), matches the loudness, measures again and
//! sends the reference's spectrum to EQ Match on the master or the selected
//! track. Right: the reference's waveform (drag a region; a click plays a
//! looping one from there; a double-click takes the whole file again) and
//! the long-term average spectra of the mix and the reference (as loud as
//! the mix when matched), with their difference under them.

#![forbid(unsafe_code)]

use faderframe_core::ReferenceId;
use faderframe_project::reference::ReferenceSync;
use faderframe_session::compare::{CompareOp, Comparing};
use faderframe_session::references::ReferenceOp;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, Color, Cursor, EventCx, FileChoice, FontFamily, HostRequest, MenuItem, Paint,
    Painter, Path, Point, PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 38.0;
const FOOTER_H: f32 = 24.0;
const LIST_W: f32 = 250.0;
const ROW_H: f32 = 42.0;
const WAVE_H: f32 = 76.0;
const GAP: f32 = 8.0;
/// The spectra's frequency axis (Hz).
const LO_HZ: f64 = 20.0;
const HI_HZ: f64 = 20_000.0;
/// dB shown in the spectra panel, and the difference's ± range.
const SPECTRUM_RANGE: f32 = 60.0;
const DIFF_RANGE: f32 = 12.0;

const AUDIO_PATTERNS: [&str; 10] = [
    "*.wav", "*.aif", "*.aiff", "*.flac", "*.mp3", "*.ogg", "*.m4a", "*.caf", "*.WAV", "*.FLAC",
];

/// What a click hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Add,
    /// The A/B switch's halves.
    Mix,
    Reference,
    Match,
    Sync,
    Measure,
    SendToEq,
    Row(usize),
    Wave,
    Graph,
}

pub struct ReferenceView {
    theme: Theme,
    hover: Option<Hit>,
    /// A region being dragged on the waveform (file frames).
    drag: Option<(i64, i64)>,
    /// Files dragged over the view.
    dropping: bool,
}

fn mmss(seconds: f64) -> String {
    let s = seconds.max(0.0);
    format!("{}:{:04.1}", (s / 60.0).floor() as i64, s % 60.0)
}

fn lufs(v: Option<f64>) -> String {
    match v {
        Some(l) if l.is_finite() => format!("{l:.1} LUFS"),
        _ => "– LUFS".into(),
    }
}

impl ReferenceView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            hover: None,
            drag: None,
            dropping: false,
        }
    }

    /// The header's buttons.
    pub fn buttons(size: Size) -> Vec<(Hit, Rect)> {
        let y = 8.0;
        let h = HEADER_H - 16.0;
        let mut x = 108.0;
        let mut out = Vec::new();
        let mut push = |hit: Hit, w: f32, gap: f32| {
            out.push((hit, Rect::new(x, y, w, h)));
            x += w + gap;
        };
        push(Hit::Add, 64.0, 12.0);
        push(Hit::Mix, 62.0, 0.0);
        push(Hit::Reference, 84.0, 12.0);
        push(Hit::Match, 96.0, 6.0);
        push(Hit::Sync, 150.0, 6.0);
        push(Hit::Measure, 72.0, 6.0);
        push(Hit::SendToEq, 128.0, 6.0);
        out.retain(|(_, r)| r.right() <= size.w - 4.0);
        out
    }

    fn list_rect(size: Size) -> Rect {
        Rect::new(
            0.0,
            HEADER_H,
            LIST_W.min(size.w * 0.4),
            (size.h - HEADER_H - FOOTER_H).max(0.0),
        )
    }

    pub fn row_rect(i: usize, size: Size) -> Rect {
        let l = Self::list_rect(size);
        Rect::new(l.x, l.y + i as f32 * ROW_H, l.w, ROW_H)
    }

    fn right_rect(size: Size) -> Rect {
        let l = Self::list_rect(size);
        let x = l.right() + GAP;
        Rect::new(
            x,
            HEADER_H + GAP,
            (size.w - x - GAP).max(0.0),
            (l.h - 2.0 * GAP).max(0.0),
        )
    }

    pub fn wave_rect(size: Size) -> Rect {
        let r = Self::right_rect(size);
        Rect::new(r.x, r.y, r.w, WAVE_H.min(r.h))
    }

    pub fn graph_rect(size: Size) -> Rect {
        let r = Self::right_rect(size);
        let top = r.y + WAVE_H + GAP;
        Rect::new(r.x, top, r.w, (r.bottom() - top).max(0.0))
    }

    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        if let Some((h, _)) = Self::buttons(size)
            .into_iter()
            .find(|(_, r)| r.contains(pos))
        {
            return Some(h);
        }
        let list = Self::list_rect(size);
        if list.contains(pos) {
            let i = ((pos.y - list.y) / ROW_H).floor() as usize;
            return (i < model.references().tracks.len()).then_some(Hit::Row(i));
        }
        if Self::wave_rect(size).contains(pos) {
            return Some(Hit::Wave);
        }
        Self::graph_rect(size).contains(pos).then_some(Hit::Graph)
    }

    /// The reference the view is about: the one compared, else the first.
    fn shown(model: &Session) -> Option<ReferenceId> {
        model
            .current_reference()
            .or_else(|| model.references().tracks.first().map(|r| r.id))
    }

    /// The file frame under `x` on the waveform.
    fn frame_at(x: f32, wave: Rect, frames: i64) -> i64 {
        (((x - wave.x) / wave.w.max(1.0)).clamp(0.0, 1.0) as f64 * frames as f64).round() as i64
    }

    fn sync_menu(model: &Session, id: ReferenceId) -> Vec<MenuItem<Action>> {
        let Some(r) = model.references().get(id) else {
            return Vec::new();
        };
        let p = model.project();
        let here = p
            .timeline
            .to_samples(model.playhead(), f64::from(p.sample_rate));
        let follows_at = match r.sync {
            ReferenceSync::Follow { at } => Some(at),
            ReferenceSync::Loop => None,
        };
        vec![
            MenuItem::new(
                "Along the Song, from Its Start",
                Action::Reference(ReferenceOp::SetSync(id, ReferenceSync::Follow { at: 0 })),
            )
            .checked(follows_at == Some(0)),
            MenuItem::new(
                format!(
                    "Along the Song, from the Playhead ({})",
                    mmss(here as f64 / f64::from(p.sample_rate.max(1)))
                ),
                Action::Reference(ReferenceOp::SetSync(
                    id,
                    ReferenceSync::Follow { at: here.max(0) },
                )),
            )
            .checked(follows_at.is_some_and(|a| a != 0)),
            MenuItem::new(
                "Loop Its Region on Its Own",
                Action::Reference(ReferenceOp::SetSync(id, ReferenceSync::Loop)),
            )
            .checked(follows_at.is_none()),
        ]
    }

    fn send_menu(model: &Session, id: ReferenceId) -> Vec<MenuItem<Action>> {
        let mut items = vec![MenuItem::new(
            "To the Master's EQ",
            Action::Reference(ReferenceOp::SendToEqMatch {
                reference: id,
                track: None,
            }),
        )];
        if let Some(t) = model
            .selection
            .tracks
            .iter()
            .find_map(|t| model.project().track(*t))
            .filter(|t| t.kind.has_audio() && Some(t.id) != model.project().master_id())
        {
            items.push(MenuItem::new(
                format!("To ‘{}’'s EQ", t.name),
                Action::Reference(ReferenceOp::SendToEqMatch {
                    reference: id,
                    track: Some(t.id),
                }),
            ));
        }
        items
    }

    fn row_menu(model: &Session, id: ReferenceId) -> Vec<MenuItem<Action>> {
        let Some(r) = model.references().get(id) else {
            return Vec::new();
        };
        let mut items = vec![MenuItem::submenu("Plays", Self::sync_menu(model, id))];
        items.push(
            MenuItem::new(
                "Match the Mix's Loudness",
                Action::Reference(ReferenceOp::SetMatch(id, !r.match_loudness)),
            )
            .checked(r.match_loudness),
        );
        if r.region.is_some() {
            items.push(MenuItem::new(
                "Play the Whole File",
                Action::Reference(ReferenceOp::SetRegion(id, None)),
            ));
        }
        items.push(MenuItem::new(
            "Measure Again",
            Action::Reference(ReferenceOp::Measure(id)),
        ));
        items.push(MenuItem::submenu(
            "Send to EQ Match",
            Self::send_menu(model, id),
        ));
        items.push(
            MenuItem::new(
                "Remove Reference",
                Action::Reference(ReferenceOp::Remove(id)),
            )
            .separated(),
        );
        items
    }

    fn button_label(hit: Hit, model: &Session) -> String {
        let shown = Self::shown(model);
        let r = shown.and_then(|id| model.references().get(id));
        match hit {
            Hit::Add => "+ Add…".into(),
            Hit::Mix => "Mix".into(),
            Hit::Reference => "Reference".into(),
            Hit::Match => format!(
                "{} Match",
                if r.is_some_and(|r| r.match_loudness) {
                    "☑"
                } else {
                    "☐"
                }
            ),
            Hit::Sync => match r.map(|r| r.sync) {
                Some(ReferenceSync::Loop) => "Loops Its Region ▾".into(),
                Some(ReferenceSync::Follow { at: 0 }) | None => "Along the Song ▾".into(),
                Some(ReferenceSync::Follow { at }) => format!(
                    "From {} ▾",
                    mmss(at as f64 / f64::from(model.project().sample_rate.max(1)))
                ),
            },
            Hit::Measure => "Measure".into(),
            Hit::SendToEq => "Send to EQ Match ▾".into(),
            _ => String::new(),
        }
    }

    fn paint_header(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let header = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(header, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        p.text(
            "Reference",
            Rect::new(12.0, 0.0, 92.0, HEADER_H),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        let status = model.compare_status();
        let on_reference = status
            .as_ref()
            .is_some_and(|s| matches!(s.what, Comparing::Reference(_)) && s.hearing_other);
        let comparing = status
            .as_ref()
            .is_some_and(|s| matches!(s.what, Comparing::Reference(_)));
        let has = Self::shown(model).is_some();
        let mut right = 108.0;
        for (hit, r) in Self::buttons(size) {
            right = r.right();
            let lit = match hit {
                Hit::Mix => comparing && !on_reference,
                Hit::Reference => on_reference,
                _ => false,
            };
            let enabled = matches!(hit, Hit::Add) || has;
            let fill = if lit {
                th.ui.accent.with_alpha(0.55)
            } else if self.hover == Some(hit) && enabled {
                th.ui.surface_alt.with_alpha(0.8)
            } else {
                th.ui.surface_alt
            };
            // The A/B switch is one control: its halves touch.
            p.fill_rounded(r, 4.0, &Paint::Solid(fill));
            p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
            p.text(
                &Self::button_label(hit, model),
                r,
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
        }
        let text = match &status {
            Some(s) if matches!(s.what, Comparing::Reference(_)) => s.text.clone(),
            Some(_) => "Comparing with the undo history (see History)".into(),
            None if has => "Hearing: Mix — choose a reference to compare".into(),
            None => "Add a finished record to compare the mix with".into(),
        };
        p.text(
            &text,
            Rect::new(
                right + 12.0,
                0.0,
                (size.w - right - 24.0).max(0.0),
                HEADER_H,
            ),
            &TextStyle::new(th.fonts.small, th.ui.text_dim).right(),
        );
    }

    fn paint_list(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let list = Self::list_rect(size);
        p.fill(list, th.ui.background);
        p.vline(list.right() - 0.5, list.y, list.bottom(), th.ui.border);
        let current = model.current_reference();
        let rate = f64::from(model.project().sample_rate.max(1));
        p.push_clip(list);
        for (i, r) in model.references().tracks.iter().enumerate() {
            let row = Self::row_rect(i, size);
            if current == Some(r.id) {
                p.fill(row, th.ui.selection.with_alpha(0.35));
                p.fill(Rect::new(row.x, row.y, 3.0, row.h), th.ui.accent);
            } else if self.hover == Some(Hit::Row(i)) {
                p.fill(row, th.ui.text.with_alpha(0.06));
            }
            p.text(
                &r.name,
                Rect::new(row.x + 12.0, row.y + 3.0, row.w - 20.0, 18.0),
                &TextStyle::new(th.fonts.small, th.ui.text).bold(),
            );
            let length = model
                .reference_frames(r.id)
                .map(|f| {
                    let (a, b) = r.span(f);
                    mmss((b - a) as f64 / rate)
                })
                .unwrap_or_default();
            let how = match r.sync {
                ReferenceSync::Follow { at: 0 } => "along the song".to_string(),
                ReferenceSync::Follow { at } => format!("from {}", mmss(at as f64 / rate)),
                ReferenceSync::Loop => "loops".into(),
            };
            let level = match model.reference_progress(r.id) {
                Some(f) => format!("measuring {:.0} %", f * 100.0),
                None => lufs(model.reference_measure(r.id).and_then(|m| m.reference_lufs)),
            };
            p.text(
                &format!("{length} · {how} · {level}"),
                Rect::new(row.x + 12.0, row.y + 21.0, row.w - 20.0, 16.0),
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
            );
            p.hline(
                row.x,
                row.right(),
                row.bottom() - 0.5,
                th.ui.border.with_alpha(0.5),
            );
        }
        let n = model.references().tracks.len();
        if model.importing_references() {
            let row = Self::row_rect(n, size);
            p.text(
                "Adding…",
                Rect::new(row.x + 12.0, row.y, row.w - 20.0, ROW_H),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
        } else if n == 0 {
            p.text(
                "No references yet: + Add…, or drop files here",
                Rect::new(list.x + 12.0, list.y + 8.0, list.w - 20.0, 20.0),
                &TextStyle::new(th.fonts.small, th.ui.text_faint),
            );
        }
        p.pop_clip();
    }

    fn paint_wave(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let wave = Self::wave_rect(size);
        if wave.w < 4.0 || wave.h < 4.0 {
            return;
        }
        p.fill_rounded(wave, 4.0, &Paint::Solid(th.ui.surface));
        let Some(id) = Self::shown(model) else {
            return;
        };
        let Some(r) = model.references().get(id) else {
            return;
        };
        let frames = model.reference_frames(id).unwrap_or(0).max(1);
        let (a, b) = self
            .drag
            .map_or_else(|| r.span(frames), |(x, y)| (x.min(y), x.max(y)));
        let x_of = |f: i64| wave.x + (f as f64 / frames as f64) as f32 * wave.w;
        // Outside the region: dimmed.
        p.fill(
            Rect::new(x_of(a), wave.y, x_of(b) - x_of(a), wave.h),
            th.ui.accent.with_alpha(0.12),
        );
        if let Some(peaks) = model.peaks(r.source) {
            let mid = wave.y + wave.h / 2.0;
            let half = wave.h / 2.0 - 3.0;
            let columns = wave.w.max(1.0) as usize;
            let mut top = Vec::with_capacity(columns + 1);
            let mut bottom = Vec::with_capacity(columns + 1);
            for c in 0..=columns {
                let f0 = (c as f64 / columns as f64 * frames as f64) as i64;
                let f1 = ((c + 1) as f64 / columns as f64 * frames as f64) as i64;
                let (lo, hi) = (0..peaks.channels().min(2))
                    .filter_map(|ch| peaks.min_max(ch, f0, f1.max(f0 + 1)))
                    .fold((0.0f32, 0.0f32), |(l, h), (a, b)| (l.min(a), h.max(b)));
                let x = wave.x + c as f32 * wave.w / columns as f32;
                top.push(Point::new(x, mid - hi.clamp(-1.0, 1.0) * half));
                bottom.push(Point::new(x, mid - lo.clamp(-1.0, 1.0) * half));
            }
            let mut outline = top;
            outline.extend(bottom.into_iter().rev());
            p.fill_path(&Path::polyline(&outline), th.ui.text_dim.with_alpha(0.7));
        }
        for x in [x_of(a), x_of(b)] {
            p.vline(x, wave.y, wave.bottom(), th.ui.accent.with_alpha(0.8));
        }
        if let Some(f) = model.reference_position(id) {
            p.vline(
                x_of(f.clamp(0, frames)),
                wave.y,
                wave.bottom(),
                th.ui.accent,
            );
        }
        p.text(
            &r.name,
            Rect::new(wave.x + 8.0, wave.y + 2.0, wave.w - 16.0, 16.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
        );
        p.stroke_rounded(wave, 4.0, 1.0, th.ui.border);
    }

    fn paint_graph(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let g = Self::graph_rect(size);
        if g.w < 40.0 || g.h < 40.0 {
            return;
        }
        p.fill_rounded(g, 4.0, &Paint::Solid(th.ui.surface));
        p.stroke_rounded(g, 4.0, 1.0, th.ui.border);
        let spectra = Rect::new(g.x + 36.0, g.y + 20.0, g.w - 44.0, (g.h - 28.0) * 0.64);
        let diff = Rect::new(
            spectra.x,
            spectra.bottom() + 8.0,
            spectra.w,
            (g.bottom() - spectra.bottom() - 16.0).max(0.0),
        );
        let x_of = |hz: f64| {
            spectra.x + ((hz / LO_HZ).log10() / (HI_HZ / LO_HZ).log10()) as f32 * spectra.w
        };
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        for hz in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0] {
            let x = x_of(hz);
            p.vline(x, spectra.y, diff.bottom(), th.ui.border.with_alpha(0.5));
            let text = if hz >= 1000.0 {
                format!("{}k", hz / 1000.0)
            } else {
                format!("{hz}")
            };
            p.text(
                &text,
                Rect::new(x - 20.0, diff.bottom(), 40.0, 14.0),
                &label.center(),
            );
        }
        let Some(id) = Self::shown(model) else {
            return;
        };
        let measure = model.reference_measure(id);
        let mix_color = th.ui.text;
        let ref_color = th.ui.accent;
        p.text(
            "Mix",
            Rect::new(g.x + 10.0, g.y + 3.0, 40.0, 16.0),
            &TextStyle::new(th.fonts.tiny + 0.5, mix_color).bold(),
        );
        p.text(
            "Reference",
            Rect::new(g.x + 44.0, g.y + 3.0, 80.0, 16.0),
            &TextStyle::new(th.fonts.tiny + 0.5, ref_color).bold(),
        );
        p.text(
            "Reference − Mix (balance)",
            Rect::new(g.x + 120.0, g.y + 3.0, 200.0, 16.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
        );
        let Some(m) = measure else {
            let text = match model.reference_progress(id) {
                Some(f) => format!("Measuring the mix and the reference… {:.0} %", f * 100.0),
                None => "Not measured yet: Measure".into(),
            };
            p.text(
                &text,
                spectra,
                &TextStyle::new(th.fonts.small, th.ui.text_faint).center(),
            );
            return;
        };
        let (Some(mix), Some(reference)) = (&m.mix, &m.reference) else {
            p.text(
                "Too short to measure a spectrum",
                spectra,
                &TextStyle::new(th.fonts.small, th.ui.text_faint).center(),
            );
            return;
        };
        // Heard as loud as the mix when matched: the balance compares.
        let shift = model
            .references()
            .get(id)
            .filter(|r| r.match_loudness)
            .and_then(|_| m.match_db())
            .unwrap_or(0.0) as f64;
        let top = mix
            .db
            .iter()
            .chain(&reference.db)
            .fold(f64::NEG_INFINITY, |a, b| a.max(*b))
            .max(-100.0);
        let top = (top / 6.0).ceil() * 6.0 + 6.0;
        let y_of =
            |db: f64| spectra.y + ((top - db) as f32 / SPECTRUM_RANGE).clamp(0.0, 1.0) * spectra.h;
        let line =
            |p: &mut dyn Painter, s: &faderframe_analysis::ltas::Ltas, add: f64, c: Color| {
                let pts: Vec<Point> = s
                    .freqs
                    .iter()
                    .zip(&s.db)
                    .filter(|(f, _)| **f >= LO_HZ && **f <= HI_HZ)
                    .map(|(f, d)| Point::new(x_of(*f), y_of(d + add)))
                    .collect();
                p.stroke_path(&Path::polyline(&pts), 1.6, c);
            };
        line(p, mix, 0.0, mix_color.with_alpha(0.85));
        line(p, reference, shift, ref_color);
        // The difference, its mean taken off (the balance, not the level).
        let d = reference.minus(mix);
        let mean = d.iter().sum::<f64>() / d.len().max(1) as f64;
        let mid = diff.y + diff.h / 2.0;
        p.hline(diff.x, diff.right(), mid, th.ui.border);
        for (db, text) in [(DIFF_RANGE, "+12"), (-DIFF_RANGE, "−12")] {
            let y = mid - db / DIFF_RANGE * diff.h / 2.0;
            p.text(
                text,
                Rect::new(g.x + 2.0, y - 7.0, 30.0, 14.0),
                &label.right(),
            );
        }
        let pts: Vec<Point> = reference
            .freqs
            .iter()
            .zip(&d)
            .filter(|(f, _)| **f >= LO_HZ && **f <= HI_HZ)
            .map(|(f, v)| {
                let v = ((v - mean) as f32).clamp(-DIFF_RANGE, DIFF_RANGE);
                Point::new(x_of(*f), mid - v / DIFF_RANGE * diff.h / 2.0)
            })
            .collect();
        p.stroke_path(&Path::polyline(&pts), 2.0, ref_color.with_alpha(0.9));
    }

    fn paint_footer(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let f = Rect::new(0.0, size.h - FOOTER_H, size.w, FOOTER_H);
        p.fill(f, th.ui.surface);
        p.hline(0.0, size.w, f.y + 0.5, th.ui.border);
        let Some(id) = Self::shown(model) else {
            return;
        };
        let text = match model.reference_measure(id) {
            Some(m) => {
                let played = match (m.match_db(), model.references().get(id)) {
                    (Some(g), Some(r)) if r.match_loudness => {
                        format!(" · played at {:+.1} dB (matched)", g + r.trim_db)
                    }
                    (_, Some(r)) if r.trim_db != 0.0 => {
                        format!(" · played at {:+.1} dB", r.trim_db)
                    }
                    _ => " · played as it is".into(),
                };
                let stale = if model.reference_measure_stale(id) {
                    " · the mix changed since: Measure again"
                } else {
                    ""
                };
                format!(
                    "Mix {} · Reference {}{played}{stale}",
                    lufs(m.mix_lufs),
                    lufs(m.reference_lufs)
                )
            }
            None => "Loudness: not measured yet".into(),
        };
        p.text(
            &text,
            Rect::new(12.0, f.y, size.w - 24.0, FOOTER_H),
            &TextStyle::new(th.fonts.small, th.ui.text_dim).family(FontFamily::Sans),
        );
    }

    fn click(
        &mut self,
        hit: Hit,
        pos: Point,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let shown = Self::shown(model);
        let compared = model.current_reference();
        match hit {
            Hit::Add => cx.request(HostRequest::ChooseFiles {
                choice: FileChoice::Open {
                    title: "Add References".into(),
                    filters: vec![
                        (
                            "Audio files".into(),
                            AUDIO_PATTERNS.iter().map(|p| p.to_string()).collect(),
                        ),
                        ("All files".into(), vec!["*".into()]),
                    ],
                },
                commit: Box::new(|paths| Some(Action::Reference(ReferenceOp::Add(paths)))),
            }),
            Hit::Mix => {
                if compared.is_some() {
                    cx.emit(Action::Compare(CompareOp::Hear(false)));
                }
            }
            Hit::Reference => {
                if let Some(id) = shown {
                    if compared != Some(id) {
                        cx.emit(Action::Reference(ReferenceOp::Choose(Some(id))));
                    }
                    cx.emit(Action::Compare(CompareOp::Hear(true)));
                }
            }
            Hit::Match => {
                if let Some(r) = shown.and_then(|id| model.references().get(id)) {
                    cx.emit(Action::Reference(ReferenceOp::SetMatch(
                        r.id,
                        !r.match_loudness,
                    )));
                }
            }
            Hit::Sync => {
                if let Some(id) = shown {
                    cx.request(HostRequest::ContextMenu {
                        at: pos,
                        items: Self::sync_menu(model, id),
                    });
                }
            }
            Hit::Measure => {
                if let Some(id) = shown {
                    cx.emit(Action::Reference(ReferenceOp::Measure(id)));
                }
            }
            Hit::SendToEq => {
                if let Some(id) = shown {
                    cx.request(HostRequest::ContextMenu {
                        at: pos,
                        items: Self::send_menu(model, id),
                    });
                }
            }
            Hit::Row(i) => {
                if let Some(r) = model.references().tracks.get(i)
                    && compared != Some(r.id)
                {
                    cx.emit(Action::Reference(ReferenceOp::Choose(Some(r.id))));
                }
            }
            Hit::Wave => {
                if let Some(id) = shown {
                    let frames = model.reference_frames(id).unwrap_or(0);
                    let f = Self::frame_at(pos.x, Self::wave_rect(size), frames);
                    self.drag = Some((f, f));
                }
            }
            Hit::Graph => {}
        }
    }
}

impl CanvasView<Session, Action> for ReferenceView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Reference Tracks".into())
    }

    fn accessible(
        &self,
        size: Size,
        model: &Session,
    ) -> Vec<faderframe_ui_canvas::AccessNode<Action>> {
        use faderframe_ui_canvas::{AccessNode, AccessRole, access_id};
        let current = model.current_reference();
        let items = model.references().tracks.iter().enumerate().map(|(i, r)| {
            AccessNode::new(
                access_id(&[41, r.id.raw()]),
                AccessRole::ListItem,
                r.name.clone(),
            )
            .at(Self::row_rect(i, size))
            .selected(current == Some(r.id))
            .on_activate(Action::Reference(ReferenceOp::Choose(Some(r.id))))
        });
        let hearing = model
            .compare_status()
            .is_some_and(|s| s.hearing_other && matches!(s.what, Comparing::Reference(_)));
        vec![
            AccessNode::new(
                access_id(&[40]),
                AccessRole::ToggleButton,
                "Hear the reference",
            )
            .checked(hearing)
            .on_activate(Action::Compare(CompareOp::Flip)),
            AccessNode::new(access_id(&[42]), AccessRole::List, "References")
                .at(Self::list_rect(size))
                .with_children(items),
        ]
    }

    fn wants_frames(&self, model: &Session) -> bool {
        // The playhead on the waveform, measurements running.
        model.current_reference().is_some() && model.transport().playing
            || model
                .references()
                .tracks
                .iter()
                .any(|r| model.reference_progress(r.id).is_some())
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        self.paint_list(p, size, model, th);
        self.paint_wave(p, size, model, th);
        self.paint_graph(p, size, model, th);
        self.paint_header(p, size, model, th);
        self.paint_footer(p, size, model, th);
        if self.dropping {
            let r = Rect::from_size(size);
            p.stroke_rounded(r, 6.0, 2.0, th.ui.accent);
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
                clicks,
                ..
            } => {
                let Some(hit) = self.hit(pos, size, model) else {
                    return false;
                };
                if hit == Hit::Wave && clicks >= 2 {
                    if let Some(id) = Self::shown(model) {
                        cx.emit(Action::Reference(ReferenceOp::SetRegion(id, None)));
                    }
                    self.drag = None;
                    return true;
                }
                self.click(hit, pos, size, model, cx);
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => match self.hit(pos, size, model) {
                Some(Hit::Row(i)) => {
                    if let Some(r) = model.references().tracks.get(i) {
                        cx.request(HostRequest::ContextMenu {
                            at: pos,
                            items: Self::row_menu(model, r.id),
                        });
                    }
                    true
                }
                _ => false,
            },
            ViewEvent::PointerMove { pos, dragging, .. } => {
                if dragging && let Some((a, _)) = self.drag {
                    if let Some(id) = Self::shown(model) {
                        let frames = model.reference_frames(id).unwrap_or(0);
                        let f = Self::frame_at(pos.x, Self::wave_rect(size), frames);
                        self.drag = Some((a, f));
                    }
                    cx.redraw();
                    return true;
                }
                let hover = self.hit(pos, size, model);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                if hover == Some(Hit::Wave) {
                    cx.set_cursor(Cursor::Text);
                }
                false
            }
            ViewEvent::PointerUp { .. } => {
                let Some((a, b)) = self.drag.take() else {
                    return false;
                };
                let Some(id) = Self::shown(model) else {
                    return true;
                };
                let frames = model.reference_frames(id).unwrap_or(0);
                let wave = Self::wave_rect(size);
                let px = (b - a).abs() as f64 / frames.max(1) as f64 * f64::from(wave.w);
                if px >= 3.0 {
                    cx.emit(Action::Reference(ReferenceOp::SetRegion(
                        id,
                        Some((a.min(b), a.max(b))),
                    )));
                } else if model.current_reference() == Some(id)
                    && model
                        .references()
                        .get(id)
                        .is_some_and(|r| r.sync == ReferenceSync::Loop)
                {
                    cx.emit(Action::Reference(ReferenceOp::Seek(a)));
                }
                cx.redraw();
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

    fn drag_files(&mut self, pos: Option<Point>, _size: Size, _model: &Session) -> bool {
        self.dropping = pos.is_some();
        pos.is_some()
    }

    fn drop_files(
        &mut self,
        files: &[std::path::PathBuf],
        _pos: Point,
        _size: Size,
        _model: &Session,
    ) -> Option<Action> {
        self.dropping = false;
        (!files.is_empty()).then(|| Action::Reference(ReferenceOp::Add(files.to_vec())))
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        Some(match self.hit(pos, size, model)? {
            Hit::Add => {
                "Add finished records to compare the mix with (decoded into the project's media)"
                    .into()
            }
            Hit::Mix => "Hear the mix (Ctrl+Alt+A flips)".into(),
            Hit::Reference => {
                "Hear the reference instead of the mix, in step with the song (Ctrl+Alt+A flips)"
                    .into()
            }
            Hit::Match => {
                "Play the reference as loud as the mix (EBU R128 integrated loudness)".into()
            }
            Hit::Sync => {
                "How the reference plays: along the song, or looping its region on its own".into()
            }
            Hit::Measure => "Measure the mix and the reference again (loudness, spectra)".into(),
            Hit::SendToEq => "Give an EQ the reference's spectrum as its EQ Match reference".into(),
            Hit::Row(_) => "Click to compare the mix with it; right-click for more".into(),
            Hit::Wave => {
                "Drag to choose the region that plays; double-click for the whole file".into()
            }
            Hit::Graph => return None,
        })
    }

    fn min_size(&self) -> Size {
        Size::new(520.0, 220.0)
    }
}

#[cfg(test)]
mod tests;
