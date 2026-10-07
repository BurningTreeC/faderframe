//! The DDP player (`session::ddp`): a CD master's fileset as a plant sees
//! it — the checks (checksums, the Red Book rules), the disc's text in
//! each CD-Text language, its tracks with their starts, lengths, pregaps,
//! ISRC and flags — an overview of the whole disc with its track marks,
//! and a CD player: play, pause, stop, previous and next track, the track,
//! index and time (counting down through a pregap), a click on the
//! overview or a track plays from there. *Import* adds the tracks to the
//! album as songs.

#![forbid(unsafe_code)]

use faderframe_session::ddp::{DdpAction, DdpDisc, DdpTask};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, EventCx, FileChoice, FontFamily, HostRequest, Paint, Painter, Point, PointerButton,
    Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const TOOLBAR_H: f32 = 36.0;
const HEADER_H: f32 = 70.0;
const OVERVIEW_H: f32 = 84.0;
const LANG_H: f32 = 28.0;
const TABLE_HEAD_H: f32 = 24.0;
const ROW_H: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Open,
    Previous,
    PlayPause,
    Stop,
    Next,
    Import,
    Close,
    Cancel,
}

struct Layout {
    buttons: Vec<(Button, Rect)>,
    header: Rect,
    overview: Rect,
    languages: Vec<(usize, Rect)>,
    table: Rect,
}

pub struct DdpView {
    theme: Theme,
    scroll: f32,
    hover: Option<usize>,
    /// The CD-Text block shown (0: the main one).
    block: usize,
}

/// `MM:SS:FF` of seconds (CD frames).
fn msf(seconds: f64) -> String {
    let neg = seconds < 0.0;
    let f = (seconds.abs() * 75.0).round() as u64;
    format!(
        "{}{:02}:{:02}:{:02}",
        if neg { "−" } else { "" },
        f / 75 / 60,
        f / 75 % 60,
        f % 75
    )
}

impl DdpView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll: 0.0,
            hover: None,
            block: 0,
        }
    }

    fn layout(&self, size: Size, model: &Session) -> Layout {
        let mut buttons = Vec::new();
        let mut x = 10.0;
        let y = 6.0;
        let mut add = |b: Button, w: f32| {
            buttons.push((b, Rect::new(x, y, w, TOOLBAR_H - 12.0)));
            x += w + 6.0;
        };
        add(Button::Open, 96.0);
        let disc = model.ddp();
        if disc.is_some() {
            add(Button::Previous, 34.0);
            add(Button::PlayPause, 64.0);
            add(Button::Stop, 50.0);
            add(Button::Next, 34.0);
            add(Button::Import, 150.0);
            add(Button::Close, 56.0);
        }
        if model.ddp_progress().is_some() {
            add(Button::Cancel, 64.0);
        }
        let header = Rect::new(0.0, TOOLBAR_H, size.w, HEADER_H);
        let overview = Rect::new(10.0, header.bottom() + 6.0, size.w - 20.0, OVERVIEW_H);
        let mut languages = Vec::new();
        let mut top = overview.bottom() + 6.0;
        if let Some(d) = disc
            && !d.disc.more_text.is_empty()
        {
            let mut x = 10.0;
            for i in 0..=d.disc.more_text.len() {
                languages.push((i, Rect::new(x, top + 3.0, 104.0, LANG_H - 6.0)));
                x += 110.0;
            }
            top += LANG_H;
        }
        let table = Rect::new(0.0, top, size.w, (size.h - top).max(0.0));
        Layout {
            buttons,
            header,
            overview,
            languages,
            table,
        }
    }

    fn label(b: Button, model: &Session) -> &'static str {
        match b {
            Button::Open => "Open DDP…",
            Button::Previous => "|◀",
            Button::PlayPause => {
                if model.ddp_playback().is_some_and(|p| p.playing) {
                    "Pause"
                } else {
                    "Play"
                }
            }
            Button::Stop => "Stop",
            Button::Next => "▶|",
            Button::Import => "Import as Album Songs",
            Button::Close => "Close",
            Button::Cancel => "Cancel",
        }
    }

    fn texts<'a>(
        &self,
        d: &'a DdpDisc,
    ) -> (
        faderframe_disc::Language,
        &'a faderframe_disc::CdText,
        Vec<&'a faderframe_disc::CdText>,
    ) {
        match self
            .block
            .checked_sub(1)
            .and_then(|i| d.disc.more_text.get(i))
        {
            Some(b) => (b.language, &b.disc, b.tracks.iter().collect()),
            None => (
                d.disc.text_language,
                &d.disc.text,
                d.disc.tracks.iter().map(|t| &t.text).collect(),
            ),
        }
    }

    fn rows_top(&self, l: &Layout) -> f32 {
        l.table.y + TABLE_HEAD_H - self.scroll
    }

    fn row_at(&self, pos: Point, l: &Layout, model: &Session) -> Option<usize> {
        let d = model.ddp()?;
        let body = Rect::new(
            l.table.x,
            l.table.y + TABLE_HEAD_H,
            l.table.w,
            l.table.h - TABLE_HEAD_H,
        );
        if !body.contains(pos) {
            return None;
        }
        let i = ((pos.y - self.rows_top(l)) / ROW_H).floor();
        (i >= 0.0 && (i as usize) < d.disc.tracks.len()).then_some(i as usize)
    }

    fn paint_button(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        label: &str,
        on: bool,
        strong: bool,
        faint: bool,
    ) {
        let th = &self.theme;
        let bg = if on {
            th.ui.accent.with_alpha(0.35)
        } else if strong {
            th.ui.accent.with_alpha(0.2)
        } else {
            th.ui.surface_alt
        };
        p.fill_rounded(
            r,
            4.0,
            &Paint::Solid(if faint { bg.with_alpha(0.5) } else { bg }),
        );
        p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
        p.text(
            label,
            r,
            &TextStyle::new(
                th.fonts.small,
                if faint { th.ui.text_faint } else { th.ui.text },
            )
            .center(),
        );
    }

    fn paint_overview(&self, p: &mut dyn Painter, r: Rect, d: &DdpDisc, model: &Session) {
        let th = &self.theme;
        p.fill_rounded(r, 4.0, &Paint::Solid(th.tools.well));
        let total = f64::from(d.disc.sectors.max(1));
        let x_of = |sector: u32| r.x + (f64::from(sector) / total) as f32 * r.w;
        // Pregaps shaded.
        for t in &d.disc.tracks {
            if let (Some(g), Some(s)) = (t.pregap, t.start()) {
                let (a, b) = (x_of(g), x_of(s));
                p.fill(
                    Rect::new(a, r.y, (b - a).max(1.0), r.h),
                    th.ui.text.with_alpha(0.05),
                );
            }
        }
        // Peaks: a mirrored filled shape.
        let n = d.overview.len().max(1);
        let mid = r.y + r.h * 0.5;
        let mut path = faderframe_ui_canvas::Path::new();
        for (i, v) in d.overview.iter().enumerate() {
            let x = r.x + i as f32 / n as f32 * r.w;
            let y = mid - v.min(1.0) * (r.h * 0.46);
            if i == 0 {
                path.move_to(Point::new(x, y));
            } else {
                path.line_to(Point::new(x, y));
            }
        }
        for (i, v) in d.overview.iter().enumerate().rev() {
            let x = r.x + i as f32 / n as f32 * r.w;
            path.line_to(Point::new(x, mid + v.min(1.0) * (r.h * 0.46)));
        }
        path.close();
        p.fill_path(&path, th.tools.spectrum.with_alpha(0.75));
        // Track marks and numbers.
        for (i, t) in d.disc.tracks.iter().enumerate() {
            let Some(s) = t.start() else { continue };
            let x = x_of(s);
            p.vline(x, r.y, r.bottom(), th.ui.text_dim);
            p.text(
                &format!("{}", i + 1),
                Rect::new(x + 3.0, r.y + 2.0, 30.0, 14.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
            );
        }
        if let Some(pb) = model.ddp_playback() {
            let x = r.x + (pb.position * 75.0 / total) as f32 * r.w;
            p.fill(Rect::new(x - 1.0, r.y, 2.0, r.h), th.ui.accent);
        }
    }

    fn paint_header(&self, p: &mut dyn Painter, r: Rect, d: &DdpDisc, model: &Session) {
        let th = &self.theme;
        p.fill(r, th.ui.surface_alt);
        p.hline(0.0, r.w, r.bottom() - 0.5, th.ui.border);
        let (language, text, _) = self.texts(d);
        let title = if text.title.is_empty() {
            d.dir
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().to_string())
        } else {
            text.title.clone()
        };
        p.text(
            &title,
            Rect::new(12.0, r.y + 8.0, r.w * 0.45, 22.0),
            &TextStyle::new(th.fonts.large, th.ui.text).bold(),
        );
        let mut sub = text.performer.clone();
        if !d.disc.more_text.is_empty() {
            if !sub.is_empty() {
                sub.push_str(" · ");
            }
            sub.push_str(language.name());
        }
        p.text(
            &sub,
            Rect::new(12.0, r.y + 32.0, r.w * 0.45, 16.0),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
        // The checks.
        let mut checks: Vec<(String, faderframe_ui_canvas::Color)> = Vec::new();
        checks.push(match d.checksums {
            Some(true) => ("Checksums match".into(), th.tools.level_ok),
            Some(false) => ("Checksums do NOT match".into(), th.tools.level_over),
            None => ("No checksum file".into(), th.tools.level_warn),
        });
        if d.problems.is_empty() {
            checks.push(("Red Book ✓".into(), th.tools.level_ok));
        } else {
            checks.push((d.problems.join("; "), th.tools.level_over));
        }
        if d.big_endian {
            checks.push(("Big-endian image".into(), th.tools.level_warn));
        }
        let mut x = 12.0;
        let y = r.y + 50.0;
        for (label, color) in &checks {
            let style = TextStyle::new(th.fonts.tiny, *color);
            let w = p.text_width(label, &style) + 14.0;
            let chip = Rect::new(x, y, w.min(r.w * 0.5), 15.0);
            p.fill_rounded(chip, 7.0, &Paint::Solid(color.with_alpha(0.15)));
            p.text(label, chip, &style.center());
            x += chip.w + 6.0;
        }
        // Codes and length.
        let mut facts = vec![format!(
            "{} tracks · {}",
            d.disc.tracks.len(),
            msf(d.seconds())
        )];
        if let Some(upc) = &d.disc.upc {
            facts.push(format!("UPC/EAN {upc}"));
        }
        if !d.disc.master_id.is_empty() {
            facts.push(format!("Master {}", d.disc.master_id));
        }
        p.text(
            &facts.join("  ·  "),
            Rect::new(r.w * 0.47, r.y + 50.0, r.w * 0.3, 16.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
        );
        // The CD player's display.
        let lcd = Rect::new(r.w - 250.0, r.y + 8.0, 238.0, r.h - 16.0);
        p.fill_rounded(lcd, 4.0, &Paint::Solid(th.ui.lcd_bg));
        let (track, index, in_track, disc_time) = match model.ddp_playback() {
            Some(pb) => (
                pb.track.map_or("--".into(), |t| format!("{:02}", t + 1)),
                format!("{:02}", pb.index),
                msf(pb.in_track),
                msf(pb.position),
            ),
            None => ("--".into(), "--".into(), "--:--:--".into(), msf(0.0)),
        };
        let mono = |size: f32, c| TextStyle::new(size, c).family(FontFamily::Mono);
        p.text(
            "TRACK",
            Rect::new(lcd.x + 10.0, lcd.y + 4.0, 50.0, 12.0),
            &mono(th.fonts.tiny, th.ui.lcd_dim),
        );
        p.text(
            "INDEX",
            Rect::new(lcd.x + 62.0, lcd.y + 4.0, 50.0, 12.0),
            &mono(th.fonts.tiny, th.ui.lcd_dim),
        );
        p.text(
            &track,
            Rect::new(lcd.x + 10.0, lcd.y + 16.0, 50.0, 26.0),
            &mono(th.fonts.display, th.ui.lcd_text),
        );
        p.text(
            &index,
            Rect::new(lcd.x + 62.0, lcd.y + 16.0, 50.0, 26.0),
            &mono(th.fonts.display, th.ui.lcd_text),
        );
        p.text(
            &in_track,
            Rect::new(lcd.x + 112.0, lcd.y + 10.0, lcd.w - 120.0, 22.0),
            &mono(th.fonts.large, th.ui.lcd_text).right(),
        );
        p.text(
            &format!("disc {disc_time}"),
            Rect::new(lcd.x + 112.0, lcd.y + 32.0, lcd.w - 120.0, 14.0),
            &mono(th.fonts.tiny, th.ui.lcd_dim).right(),
        );
    }

    fn paint_table(&self, p: &mut dyn Painter, l: &Layout, d: &DdpDisc, model: &Session) {
        let th = &self.theme;
        let t = l.table;
        let head = Rect::new(t.x, t.y, t.w, TABLE_HEAD_H);
        p.fill(head, th.ui.surface);
        p.hline(0.0, t.w, head.bottom() - 0.5, th.ui.border);
        let cols: [(&str, f32); 8] = [
            ("#", 36.0),
            ("Start", 82.0),
            ("Length", 82.0),
            ("Pregap", 70.0),
            ("ISRC", 112.0),
            ("Flags", 70.0),
            ("Title", 0.0),
            ("Performer", 0.0),
        ];
        let fixed: f32 = cols.iter().map(|c| c.1).sum();
        let free = ((t.w - fixed - 20.0) / 2.0).max(80.0);
        let widths: Vec<f32> = cols
            .iter()
            .map(|c| if c.1 > 0.0 { c.1 } else { free })
            .collect();
        let mut x = 10.0;
        for ((name, _), w) in cols.iter().zip(&widths) {
            p.text(
                name,
                Rect::new(x, head.y, *w - 6.0, head.h),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim).bold(),
            );
            x += w;
        }
        let body = Rect::new(t.x, head.bottom(), t.w, (t.h - TABLE_HEAD_H).max(0.0));
        p.push_clip(body);
        let (_, _, texts) = self.texts(d);
        let playing = model.ddp_playback().and_then(|p| p.track);
        for (i, track) in d.disc.tracks.iter().enumerate() {
            let y = self.rows_top(l) + i as f32 * ROW_H;
            if y + ROW_H < body.y || y > body.bottom() {
                continue;
            }
            let row = Rect::new(t.x, y, t.w, ROW_H);
            if playing == Some(i) {
                p.fill(row, th.ui.selection.with_alpha(0.35));
                p.fill(Rect::new(row.x, row.y, 3.0, row.h), th.ui.accent);
            } else if self.hover == Some(i) {
                p.fill(row, th.ui.text.with_alpha(0.06));
            } else if i % 2 == 1 {
                p.fill(row, th.ui.text.with_alpha(0.02));
            }
            let (start, length) = d.track_span(i).unwrap_or((0.0, 0.0));
            let pregap = match (track.pregap, track.start()) {
                (Some(g), Some(s)) => msf(f64::from(s - g) / 75.0),
                _ => "—".into(),
            };
            let f = track.flags;
            let mut flags = Vec::new();
            if f.copy_permitted {
                flags.push("DCP");
            }
            if f.pre_emphasis {
                flags.push("PRE");
            }
            if f.four_channel {
                flags.push("4CH");
            }
            if f.scms {
                flags.push("SCMS");
            }
            let text = texts.get(i);
            let cells = [
                format!("{:02}", i + 1),
                msf(start),
                msf(length),
                pregap,
                track.isrc.clone().unwrap_or_default(),
                flags.join(" "),
                text.map_or_else(String::new, |t| t.title.clone()),
                text.map_or_else(String::new, |t| t.performer.clone()),
            ];
            let mut x = 10.0;
            for (k, (cell, w)) in cells.iter().zip(&widths).enumerate() {
                let style = if k < 6 {
                    TextStyle::new(th.fonts.small, th.ui.text_dim).family(FontFamily::Mono)
                } else {
                    TextStyle::new(th.fonts.small, th.ui.text)
                };
                p.text(cell, Rect::new(x, y, *w - 6.0, ROW_H), &style);
                x += w;
            }
        }
        p.pop_clip();
    }
}

impl CanvasView<Session, Action> for DdpView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.ddp_playback().is_some_and(|p| p.playing) || model.ddp_progress().is_some()
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        let l = self.layout(size, model);
        let bar = Rect::new(0.0, 0.0, size.w, TOOLBAR_H);
        p.fill(bar, th.ui.surface);
        p.hline(0.0, size.w, TOOLBAR_H - 0.5, th.ui.border);
        let playing = model.ddp_playback();
        for (b, r) in &l.buttons {
            let on = *b == Button::PlayPause && playing.is_some_and(|p| p.playing);
            let faint =
                matches!(b, Button::Previous | Button::Next | Button::Stop) && playing.is_none();
            self.paint_button(
                p,
                *r,
                Self::label(*b, model),
                on,
                matches!(b, Button::Open | Button::Import),
                faint,
            );
        }
        if let Some((task, f)) = model.ddp_progress() {
            let what = match task {
                DdpTask::Open => "Opening and checking",
                DdpTask::Import => "Importing tracks",
            };
            let x = l.buttons.last().map_or(10.0, |(_, r)| r.right() + 12.0);
            p.text(
                &format!("{what}… {:.0} %", f * 100.0),
                Rect::new(x, 0.0, 260.0, TOOLBAR_H),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
        }
        let Some(d) = model.ddp() else {
            p.text(
                "Open a DDP fileset — the folder with DDPID, DDPMS, the PQ descriptor and the image — to check it as a plant would and play it.",
                Rect::new(20.0, TOOLBAR_H + 20.0, size.w - 40.0, 40.0),
                &TextStyle::new(th.fonts.normal, th.ui.text_dim),
            );
            return;
        };
        self.block = self.block.min(d.disc.more_text.len());
        self.paint_header(p, l.header, d, model);
        self.paint_overview(p, l.overview, d, model);
        for (i, r) in &l.languages {
            let name = if *i == 0 {
                d.disc.text_language.name()
            } else {
                d.disc.more_text[i - 1].language.name()
            };
            self.paint_button(p, *r, name, self.block == *i, false, false);
        }
        let total = d.disc.tracks.len() as f32 * ROW_H;
        self.scroll = self
            .scroll
            .clamp(0.0, (total - (l.table.h - TABLE_HEAD_H)).max(0.0));
        self.paint_table(p, &l, d, model);
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let l = self.layout(size, model);
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                ..
            } => {
                if let Some((b, _)) = l.buttons.iter().find(|(_, r)| r.contains(pos)) {
                    let playing = model.ddp_playback();
                    match b {
                        Button::Open => cx.request(HostRequest::ChooseFiles {
                            choice: FileChoice::Folder {
                                title: "Open a DDP fileset".into(),
                                initial: model.ddp().map(|d| d.dir.clone()),
                            },
                            commit: Box::new(|paths| {
                                paths
                                    .into_iter()
                                    .next()
                                    .map(|p| Action::Ddp(DdpAction::Open(p)))
                            }),
                        }),
                        Button::PlayPause => {
                            cx.emit(Action::Ddp(if playing.is_some_and(|p| p.playing) {
                                DdpAction::Pause
                            } else {
                                DdpAction::Play(None)
                            }))
                        }
                        Button::Stop => cx.emit(Action::Ddp(DdpAction::Stop)),
                        Button::Previous => cx.emit(Action::Ddp(DdpAction::Skip(-1))),
                        Button::Next => cx.emit(Action::Ddp(DdpAction::Skip(1))),
                        Button::Import => cx.emit(Action::Ddp(DdpAction::Import)),
                        Button::Close => cx.emit(Action::Ddp(DdpAction::Close)),
                        Button::Cancel => cx.emit(Action::Ddp(DdpAction::Cancel)),
                    }
                    return true;
                }
                if let Some((i, _)) = l.languages.iter().find(|(_, r)| r.contains(pos)) {
                    self.block = *i;
                    cx.redraw();
                    return true;
                }
                if let Some(d) = model.ddp()
                    && l.overview.contains(pos)
                {
                    let at =
                        f64::from((pos.x - l.overview.x) / l.overview.w.max(1.0)) * d.seconds();
                    if model.ddp_playback().is_none() {
                        cx.emit(Action::Ddp(DdpAction::Play(None)));
                    }
                    cx.emit(Action::Ddp(DdpAction::Seek(at)));
                    return true;
                }
                if let Some(i) = self.row_at(pos, &l, model) {
                    cx.emit(Action::Ddp(DdpAction::Play(Some(i))));
                    return true;
                }
                false
            }
            ViewEvent::PointerMove { pos, .. } => {
                let hover = self.row_at(pos, &l, model);
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
            ViewEvent::Scroll { dy, precise, .. } => {
                self.scroll = (self.scroll + if precise { dy } else { dy * ROW_H * 3.0 }).max(0.0);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Vertical {
            return None;
        }
        let d = model.ddp()?;
        let l = self.layout(size, model);
        Some(ScrollInfo {
            content: d.disc.tracks.len() as f32 * ROW_H,
            viewport: (l.table.h - TABLE_HEAD_H).max(0.0),
            offset: self.scroll,
            start: l.table.y + TABLE_HEAD_H,
            end: 0.0,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Vertical {
            self.scroll = offset.max(0.0);
        }
    }

    fn min_size(&self) -> Size {
        Size::new(520.0, 280.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_cd_frames() {
        assert_eq!(msf(0.0), "00:00:00");
        assert_eq!(msf(2.0), "00:02:00");
        assert_eq!(msf(61.5), "01:01:38");
        assert_eq!(msf(-2.0), "−00:02:00");
    }
}
