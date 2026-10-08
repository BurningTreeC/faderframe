//! The setlist and show mode.
//!
//! In the dock it is the show's list: songs (from the project's sections,
//! or the edit selection) with where they are, how long, what comes after
//! each (stop, the next song after a gap, play on) and the performer's
//! notes — renamed and annotated with a double-click, reordered and removed
//! from a row's menu or the keyboard.
//!
//! In show mode it is the stage screen (full screen): the song playing or
//! stood on in large type, its time and what is left, the part of the song
//! now and next (from the sections), the lyric line, the notes, the list
//! with what was played and what is next, and big buttons. Space plays or
//! stops, the arrows (or Page Up/Down) go to the previous or next song,
//! Enter plays the song stood on from its start, Esc leaves the show.

#![forbid(unsafe_code)]

use faderframe_project::setlist::{AfterSong, SetSong};
use faderframe_session::setlist::{SetlistOp, ShowOp};
use faderframe_session::{Action, Session, TransportAction};
use faderframe_ui_canvas::{
    CanvasView, EventCx, FontFamily, FontWeight, HostRequest, Key, MenuItem, Painter, Point,
    PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 36.0;
const COLUMNS_H: f32 = 22.0;
const ROW_H: f32 = 26.0;

/// The list's columns: (title, width; 0 = what is left).
const COLUMNS: [(&str, f32); 6] = [
    ("#", 34.0),
    ("Song", 0.0),
    ("Start", 90.0),
    ("Length", 70.0),
    ("Then", 170.0),
    ("Notes", 0.0),
];

/// What a click in the list or the stage screen hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    FromSections,
    FromSelection,
    EnterShow,
    Column(usize, usize),
    Leave,
    Previous,
    PlayStop,
    Next,
    Song(usize),
}

pub struct SetlistView {
    theme: Theme,
    scroll: f32,
    hover: Option<usize>,
    pub selected: Option<usize>,
}

fn mmss(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn hmmss(seconds: f64) -> String {
    let s = seconds.max(0.0) as i64;
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

impl SetlistView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll: 0.0,
            hover: None,
            selected: None,
        }
    }

    // --- the list ---------------------------------------------------------------

    fn buttons(size: Size) -> [(Hit, Rect, &'static str); 3] {
        let y = 7.0;
        let h = HEADER_H - 14.0;
        let show = Rect::new(size.w - 12.0 - 118.0, y, 118.0, h);
        let sel = Rect::new(show.x - 8.0 - 130.0, y, 130.0, h);
        let sec = Rect::new(sel.x - 6.0 - 150.0, y, 150.0, h);
        [
            (Hit::FromSections, sec, "Songs from Sections"),
            (Hit::FromSelection, sel, "+ Song from Selection"),
            (Hit::EnterShow, show, "▶ Show Mode"),
        ]
    }

    fn list_rect(size: Size) -> Rect {
        let top = HEADER_H + COLUMNS_H;
        Rect::new(0.0, top, size.w, (size.h - top - 24.0).max(0.0))
    }

    pub fn row_rect(&self, i: usize, size: Size) -> Rect {
        let l = Self::list_rect(size);
        Rect::new(l.x, l.y + i as f32 * ROW_H - self.scroll, l.w, ROW_H)
    }

    /// Column `c` of a row (or of the header row).
    pub fn cell(c: usize, r: Rect) -> Rect {
        let fixed: f32 = COLUMNS.iter().map(|(_, w)| w).sum();
        let flexible = COLUMNS.iter().filter(|(_, w)| *w == 0.0).count() as f32;
        let flex = ((r.w - fixed) / flexible.max(1.0)).max(60.0);
        let mut x = r.x;
        for (i, (_, w)) in COLUMNS.iter().enumerate() {
            let w = if *w == 0.0 { flex } else { *w };
            if i == c {
                return Rect::new(x, r.y, w, r.h);
            }
            x += w;
        }
        Rect::new(x, r.y, 0.0, r.h)
    }

    fn column_at(x: f32, r: Rect) -> usize {
        (0..COLUMNS.len())
            .find(|&c| {
                let cell = Self::cell(c, r);
                x >= cell.x && x < cell.x + cell.w
            })
            .unwrap_or(COLUMNS.len() - 1)
    }

    fn row_at(&self, pos: Point, size: Size, count: usize) -> Option<usize> {
        let l = Self::list_rect(size);
        if !l.contains(pos) {
            return None;
        }
        let i = ((pos.y - l.y + self.scroll) / ROW_H).floor();
        (i >= 0.0 && (i as usize) < count).then_some(i as usize)
    }

    fn then_items(i: usize, now: AfterSong) -> Vec<MenuItem<Action>> {
        [
            AfterSong::Stop,
            AfterSong::Next { gap: 0.0 },
            AfterSong::Next { gap: 5.0 },
            AfterSong::Next { gap: 10.0 },
            AfterSong::Next { gap: 30.0 },
            AfterSong::Continue,
        ]
        .into_iter()
        .map(|t| MenuItem::new(t.label(), Action::Setlist(SetlistOp::Then(i, t))).checked(t == now))
        .collect()
    }

    fn row_menu(model: &Session, i: usize) -> Vec<MenuItem<Action>> {
        let songs = &model.setlist().songs;
        let Some(song) = songs.get(i) else {
            return Vec::new();
        };
        let mut items = vec![
            MenuItem::new(
                "Play From Here",
                Action::Transport(TransportAction::Locate(song.start)),
            ),
            MenuItem::submenu("Then", Self::then_items(i, song.then)).separated(),
        ];
        if i > 0 {
            items.push(
                MenuItem::new(
                    "Move Up",
                    Action::Setlist(SetlistOp::Move { from: i, to: i - 1 }),
                )
                .separated(),
            );
        }
        if i + 1 < songs.len() {
            items.push(MenuItem::new(
                "Move Down",
                Action::Setlist(SetlistOp::Move { from: i, to: i + 1 }),
            ));
        }
        items.push(MenuItem::new("Remove Song", Action::Setlist(SetlistOp::Remove(i))).separated());
        items.push(MenuItem::new(
            "Clear the Setlist",
            Action::Setlist(SetlistOp::Clear),
        ));
        items
    }

    fn paint_list(&mut self, p: &mut dyn Painter, size: Size, model: &Session) {
        let th = &self.theme;
        p.fill(Rect::from_size(size), th.ui.background);
        let header = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(header, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        p.text(
            "Setlist",
            Rect::new(12.0, 0.0, 100.0, HEADER_H),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        let songs = &model.setlist().songs;
        for (hit, r, label) in Self::buttons(size) {
            let accent = hit == Hit::EnterShow;
            let enabled = match hit {
                Hit::EnterShow => !songs.is_empty(),
                Hit::FromSections => !model.project().sections.is_empty(),
                Hit::FromSelection => model.selection.range.is_some_and(|r| !r.is_empty()),
                _ => true,
            };
            if accent && enabled {
                p.fill_rounded(r, 4.0, &th.ui.accent.with_alpha(0.3).into());
                p.stroke_rounded(r, 4.0, 1.0, th.ui.accent);
            } else {
                p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
            }
            p.text(
                label,
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
        let names = Rect::new(0.0, HEADER_H, size.w, COLUMNS_H);
        p.fill(names, th.ui.surface.with_alpha(0.6));
        p.hline(0.0, size.w, HEADER_H + COLUMNS_H - 0.5, th.ui.border);
        for (c, (title, _)) in COLUMNS.iter().enumerate() {
            p.text(
                title,
                Self::cell(c, names).inset_xy(8.0, 0.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).bold(),
            );
        }
        let list = Self::list_rect(size);
        if songs.is_empty() {
            p.text(
                "No songs yet: make one of each section (Songs from Sections), or select a range in the arranger and add it",
                list.inset(16.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
        }
        let max = (songs.len() as f32 * ROW_H - list.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
        p.push_clip(list);
        let tl = &model.project().timeline;
        for (i, song) in songs.iter().enumerate() {
            let r = self.row_rect(i, size);
            if r.bottom() < list.y || r.y > list.bottom() {
                continue;
            }
            if self.selected == Some(i) {
                p.fill(r, th.ui.selection.with_alpha(0.35));
            } else if self.hover == Some(i) {
                p.fill(r, th.ui.text.with_alpha(0.05));
            }
            let style = TextStyle::new(th.fonts.small, th.ui.text);
            let dim = TextStyle::new(th.fonts.small, th.ui.text_dim);
            let cells: [(String, &TextStyle); 6] = [
                (format!("{}", i + 1), &dim),
                (song.name.clone(), &style),
                (tl.format_bbt(song.start), &dim),
                (mmss(model.song_seconds(song)), &dim),
                (song.then.label(), &dim),
                (song.notes.lines().next().unwrap_or("").to_string(), &dim),
            ];
            for (c, (text, style)) in cells.iter().enumerate() {
                let s = if c == 2 || c == 3 {
                    (**style).family(FontFamily::Mono)
                } else {
                    **style
                };
                p.text(text, Self::cell(c, r).inset_xy(8.0, 0.0), &s);
            }
            p.hline(
                r.x,
                r.x + r.w,
                r.bottom() - 0.5,
                th.ui.border.with_alpha(0.4),
            );
        }
        p.pop_clip();
        // The show's length.
        let total: f64 = songs.iter().map(|s| model.song_seconds(s)).sum();
        let foot = Rect::new(0.0, size.h - 24.0, size.w, 24.0);
        p.fill(foot, th.ui.surface);
        p.text(
            &format!(
                "{} song{} · {}",
                songs.len(),
                if songs.len() == 1 { "" } else { "s" },
                mmss(total)
            ),
            foot.inset_xy(12.0, 0.0),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
    }

    // --- the stage screen -------------------------------------------------------

    fn stage(size: Size) -> StageLayout {
        let top = 52.0;
        let bottom = 96.0;
        let side = (size.w * 0.34).clamp(220.0, 460.0);
        let body = Rect::new(0.0, top, size.w, (size.h - top - bottom).max(0.0));
        let main = Rect::new(24.0, body.y + 16.0, body.w - side - 48.0, body.h - 32.0);
        let list = Rect::new(body.w - side - 8.0, body.y + 8.0, side, body.h - 16.0);
        let by = size.h - bottom + 14.0;
        let bw = ((size.w - 4.0 * 20.0) / 3.0).min(260.0);
        let mid = size.w / 2.0;
        StageLayout {
            leave: Rect::new(size.w - 16.0 - 150.0, 10.0, 150.0, 32.0),
            main,
            list,
            previous: Rect::new(mid - bw * 1.5 - 20.0, by, bw, bottom - 28.0),
            play: Rect::new(mid - bw / 2.0, by, bw, bottom - 28.0),
            next: Rect::new(mid + bw / 2.0 + 20.0, by, bw, bottom - 28.0),
        }
    }

    fn stage_row(l: &StageLayout, i: usize) -> Rect {
        Rect::new(l.list.x, l.list.y + 34.0 + i as f32 * 40.0, l.list.w, 38.0)
    }

    fn big_button(&self, p: &mut dyn Painter, r: Rect, label: &str, lit: bool) {
        let th = &self.theme;
        p.fill_rounded(
            r,
            8.0,
            &(if lit {
                th.ui.accent.with_alpha(0.85)
            } else {
                th.ui.surface
            })
            .into(),
        );
        p.stroke_rounded(r, 8.0, 1.5, if lit { th.ui.accent } else { th.ui.border });
        p.text(
            label,
            r,
            &TextStyle::new(22.0, th.ui.text)
                .weight(FontWeight::Bold)
                .center(),
        );
    }

    fn paint_stage(&mut self, p: &mut dyn Painter, size: Size, model: &Session) {
        let th = self.theme.clone();
        let l = Self::stage(size);
        p.fill(Rect::from_size(size), th.ui.background.darken(0.35));
        let songs = &model.setlist().songs;
        let show = model.show();
        let i = show.current.min(songs.len().saturating_sub(1));
        // The top line: SHOW, where in the list, how long the show runs.
        let badge = Rect::new(16.0, 12.0, 64.0, 28.0);
        p.fill_rounded(badge, 5.0, &th.ui.accent.into());
        p.text(
            "SHOW",
            badge,
            &TextStyle::new(14.0, th.ui.text)
                .weight(FontWeight::Bold)
                .center(),
        );
        let since = show.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        p.text(
            &format!(
                "Song {} of {}   ·   Show {}",
                i + 1,
                songs.len(),
                hmmss(since)
            ),
            Rect::new(96.0, 0.0, size.w - 300.0, 52.0),
            &TextStyle::new(16.0, th.ui.text_dim),
        );
        p.stroke_rounded(l.leave, 6.0, 1.0, th.ui.border);
        p.text(
            "Leave Show (Esc)",
            l.leave,
            &TextStyle::new(13.0, th.ui.text_dim).center(),
        );
        let Some(song) = songs.get(i) else {
            return;
        };
        // The song: its name, its state, its time.
        let m = l.main;
        p.text(
            &song.name,
            Rect::new(m.x, m.y, m.w, 70.0),
            &TextStyle::new(56.0, th.ui.text).weight(FontWeight::Bold),
        );
        let playing = model.transport().playing;
        let rate = f64::from(model.project().sample_rate.max(1));
        let tl = &model.project().timeline;
        let start = tl.to_samples(song.start, rate);
        let length = model.song_seconds(song);
        let elapsed = ((model.transport().position - start) as f64 / rate).clamp(0.0, length);
        let state = match (playing, model.show_countdown()) {
            (true, _) => "Playing".to_string(),
            (false, Some(c)) => format!("Starts in {:.0} s", c.ceil()),
            (false, None) => "Ready · Space plays".to_string(),
        };
        p.text(
            &state,
            Rect::new(m.x, m.y + 74.0, m.w, 28.0),
            &TextStyle::new(
                20.0,
                if playing {
                    th.console.meter.green
                } else {
                    th.ui.accent
                },
            ),
        );
        let time = Rect::new(m.x, m.y + 112.0, m.w, 84.0);
        p.text(
            &format!("{}  /  −{}", mmss(elapsed), mmss(length - elapsed)),
            time,
            &TextStyle::new(64.0, th.ui.text).family(FontFamily::Mono),
        );
        let bar = Rect::new(m.x, time.bottom() + 8.0, m.w, 10.0);
        p.fill_rounded(bar, 5.0, &th.ui.surface.into());
        if length > 0.0 {
            p.fill_rounded(
                Rect::new(bar.x, bar.y, bar.w * (elapsed / length) as f32, bar.h),
                5.0,
                &th.ui.accent.into(),
            );
        }
        // The part now and next.
        let at = model.playhead();
        let sections = &model.project().sections;
        // (A section that is the song itself is not a part of it.)
        let now = sections.iter().find(|s| {
            s.start <= at
                && at < s.end
                && s.start >= song.start
                && s.end <= song.end
                && (s.start, s.end) != (song.start, song.end)
        });
        let next = sections
            .iter()
            .filter(|s| s.start > at && s.start < song.end)
            .min_by_key(|s| s.start);
        let mut y = bar.bottom() + 24.0;
        if now.is_some() || next.is_some() {
            let text = match (now, next) {
                (Some(a), Some(b)) => format!("{}  →  {}", a.name, b.name),
                (Some(a), None) => a.name.clone(),
                (None, Some(b)) => format!("→  {}", b.name),
                (None, None) => String::new(),
            };
            p.text(
                &text,
                Rect::new(m.x, y, m.w, 34.0),
                &TextStyle::new(26.0, th.ui.text_dim),
            );
            y += 44.0;
        }
        // The lyric line and the next one.
        let lyrics = &model.project().lyrics;
        if let Some(li) = faderframe_project::lyrics::line_at(lyrics, at) {
            p.text(
                &lyrics[li].text,
                Rect::new(m.x, y, m.w, 40.0),
                &TextStyle::new(30.0, th.ui.text),
            );
            if let Some(n) = lyrics.get(li + 1) {
                p.text(
                    &n.text,
                    Rect::new(m.x, y + 40.0, m.w, 30.0),
                    &TextStyle::new(22.0, th.ui.text_faint),
                );
            }
            y += 84.0;
        }
        // The notes.
        if !song.notes.is_empty() {
            let box_ = Rect::new(m.x, y, m.w, (m.bottom() - y).max(0.0));
            if box_.h > 30.0 {
                p.fill_rounded(box_, 8.0, &th.ui.surface.with_alpha(0.6).into());
                for (k, line) in song.notes.lines().enumerate() {
                    let ly = box_.y + 12.0 + k as f32 * 30.0;
                    if ly + 28.0 > box_.bottom() {
                        break;
                    }
                    p.text(
                        line,
                        Rect::new(box_.x + 16.0, ly, box_.w - 32.0, 28.0),
                        &TextStyle::new(22.0, th.ui.lcd_text),
                    );
                }
            }
        }
        // The list.
        p.fill_rounded(l.list, 8.0, &th.ui.surface.with_alpha(0.5).into());
        p.text(
            "SETLIST",
            Rect::new(l.list.x + 14.0, l.list.y + 6.0, 200.0, 22.0),
            &TextStyle::new(13.0, th.ui.text_faint).weight(FontWeight::Bold),
        );
        p.push_clip(l.list);
        for (k, s) in songs.iter().enumerate() {
            let r = Self::stage_row(&l, k);
            if r.y > l.list.bottom() {
                break;
            }
            let ink = if k == i {
                th.ui.text
            } else if k < i {
                th.ui.text_faint
            } else {
                th.ui.text_dim
            };
            if k == i {
                p.fill_rounded(
                    r.inset_xy(6.0, 1.0),
                    6.0,
                    &th.ui.accent.with_alpha(0.3).into(),
                );
            }
            p.text(
                &format!("{}", k + 1),
                Rect::new(r.x + 12.0, r.y, 30.0, r.h),
                &TextStyle::new(16.0, th.ui.text_faint).family(FontFamily::Mono),
            );
            p.text(
                &s.name,
                Rect::new(r.x + 44.0, r.y, r.w - 140.0, r.h),
                &TextStyle::new(18.0, ink).weight(if k == i {
                    FontWeight::Bold
                } else {
                    FontWeight::Normal
                }),
            );
            let tag = if k == i + 1 {
                "NEXT".to_string()
            } else {
                mmss(model.song_seconds(s))
            };
            p.text(
                &tag,
                Rect::new(r.x + r.w - 92.0, r.y, 80.0, r.h),
                &TextStyle::new(14.0, if k == i + 1 { th.ui.accent } else { ink })
                    .family(FontFamily::Mono)
                    .right(),
            );
        }
        p.pop_clip();
        // The buttons.
        self.big_button(p, l.previous, "◀◀  Previous", false);
        self.big_button(
            p,
            l.play,
            if playing { "■  Stop" } else { "▶  Play" },
            !playing,
        );
        self.big_button(p, l.next, "Next  ▶▶", false);
    }

    /// What `pos` hits.
    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        if model.show_mode() {
            let l = Self::stage(size);
            for (r, h) in [
                (l.leave, Hit::Leave),
                (l.previous, Hit::Previous),
                (l.play, Hit::PlayStop),
                (l.next, Hit::Next),
            ] {
                if r.contains(pos) {
                    return Some(h);
                }
            }
            return (0..model.setlist().songs.len())
                .find(|&k| Self::stage_row(&l, k).contains(pos))
                .map(Hit::Song);
        }
        for (hit, r, _) in Self::buttons(size) {
            if r.contains(pos) {
                return Some(hit);
            }
        }
        let i = self.row_at(pos, size, model.setlist().songs.len())?;
        Some(Hit::Column(
            i,
            Self::column_at(pos.x, self.row_rect(i, size)),
        ))
    }

    fn rename(&self, i: usize, song: &SetSong, size: Size) -> HostRequest<Action> {
        HostRequest::TextInput {
            at: Self::cell(1, self.row_rect(i, size)),
            initial: song.name.clone(),
            commit: Box::new(move |text: &str| {
                let t = text.trim();
                (!t.is_empty()).then(|| Action::Setlist(SetlistOp::Rename(i, t.to_string())))
            }),
        }
    }

    fn notes(&self, i: usize, song: &SetSong, size: Size) -> HostRequest<Action> {
        HostRequest::TextInput {
            at: Self::cell(5, self.row_rect(i, size)),
            // Lines typed as "a / b" (one line of entry).
            initial: song.notes.replace('\n', " / "),
            commit: Box::new(move |text: &str| {
                let notes = text
                    .split(" / ")
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(Action::Setlist(SetlistOp::Notes(i, notes)))
            }),
        }
    }
}

struct StageLayout {
    leave: Rect,
    main: Rect,
    list: Rect,
    previous: Rect,
    play: Rect,
    next: Rect,
}

impl CanvasView<Session, Action> for SetlistView {
    /// The list: its buttons and songs (Enter goes to a song). In show
    /// mode: the song stood on, the show's buttons and its songs (Enter
    /// plays one).
    fn accessible(
        &self,
        size: Size,
        model: &Session,
    ) -> Vec<faderframe_ui_canvas::AccessNode<Action>> {
        use faderframe_ui_canvas::{AccessNode, AccessRole, access_id};
        let songs = &model.setlist().songs;
        let tl = &model.project().timeline;
        if model.show_mode() {
            let l = Self::stage(size);
            let show = model.show();
            let i = show.current.min(songs.len().saturating_sub(1));
            let mut out = Vec::new();
            if let Some(song) = songs.get(i) {
                let state = match (model.transport().playing, model.show_countdown()) {
                    (true, _) => "playing".to_string(),
                    (false, Some(c)) => format!("starts in {:.0} seconds", c.ceil()),
                    (false, None) => "ready".to_string(),
                };
                let mut label =
                    format!("Song {} of {}: {}, {state}", i + 1, songs.len(), song.name);
                if !song.notes.is_empty() {
                    label.push_str(&format!(". Notes: {}", song.notes.replace('\n', ", ")));
                }
                out.push(AccessNode::new(access_id(&[41]), AccessRole::Heading, label).at(l.main));
            }
            let playing = model.transport().playing;
            for (k, r, label, op) in [
                (42, l.previous, "Previous song", ShowOp::Previous),
                (
                    43,
                    l.play,
                    if playing { "Stop" } else { "Play" },
                    ShowOp::PlayStop,
                ),
                (44, l.next, "Next song", ShowOp::Next),
                (45, l.leave, "Leave the show", ShowOp::Leave),
            ] {
                out.push(
                    AccessNode::new(access_id(&[k]), AccessRole::Button, label)
                        .at(r)
                        .on_activate(Action::Show(op)),
                );
            }
            let items = songs.iter().enumerate().map(|(k, s)| {
                AccessNode::new(
                    access_id(&[46, k as u64]),
                    AccessRole::ListItem,
                    format!("{}. {}, {}", k + 1, s.name, mmss(model.song_seconds(s))),
                )
                .at(Self::stage_row(&l, k))
                .selected(k == i)
                .on_activate(Action::Show(ShowOp::Go(k)))
            });
            out.push(
                AccessNode::new(access_id(&[47]), AccessRole::List, "Setlist")
                    .at(l.list)
                    .with_children(items),
            );
            return out;
        }
        let mut out: Vec<AccessNode<Action>> = Self::buttons(size)
            .into_iter()
            .map(|(hit, r, label)| {
                let action = match hit {
                    Hit::FromSections => Action::Setlist(SetlistOp::FromSections),
                    Hit::EnterShow => Action::Show(ShowOp::Enter),
                    _ => match model.selection.range.filter(|r| !r.is_empty()) {
                        Some(range) => Action::Setlist(SetlistOp::Add {
                            name: format!("Song {}", songs.len() + 1),
                            start: range.start,
                            end: range.end,
                        }),
                        None => Action::Several(Vec::new()),
                    },
                };
                AccessNode::new(
                    faderframe_ui_canvas::access_id_str(label, &[48]),
                    AccessRole::Button,
                    label.trim_start_matches("▶ ").trim_start_matches("+ "),
                )
                .at(r)
                .on_activate(action)
            })
            .collect();
        let items = songs.iter().enumerate().map(|(k, s)| {
            let mut label = format!(
                "{}. {}, at {}, {}, then {}",
                k + 1,
                s.name,
                tl.format_bbt(s.start),
                mmss(model.song_seconds(s)),
                s.then.label().to_lowercase()
            );
            if !s.notes.is_empty() {
                label.push_str(&format!(", notes: {}", s.notes.replace('\n', ", ")));
            }
            AccessNode::new(access_id(&[49, k as u64]), AccessRole::ListItem, label)
                .at(self.row_rect(k, size))
                .selected(self.selected == Some(k))
                .on_activate(Action::Transport(TransportAction::Locate(s.start)))
        });
        out.push(
            AccessNode::new(access_id(&[50]), AccessRole::List, "Songs")
                .at(Self::list_rect(size))
                .with_children(items),
        );
        out
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Setlist".into())
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.theme = theme.clone();
        if model.show_mode() {
            self.paint_stage(p, size, model);
        } else {
            self.paint_list(p, size, model);
        }
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let songs = &model.setlist().songs;
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                clicks,
                ..
            } => {
                cx.request(HostRequest::GrabFocus);
                let Some(hit) = self.hit(pos, size, model) else {
                    return false;
                };
                match hit {
                    Hit::FromSections => cx.emit(Action::Setlist(SetlistOp::FromSections)),
                    Hit::FromSelection => {
                        if let Some(r) = model.selection.range.filter(|r| !r.is_empty()) {
                            cx.emit(Action::Setlist(SetlistOp::Add {
                                name: format!("Song {}", songs.len() + 1),
                                start: r.start,
                                end: r.end,
                            }));
                        }
                    }
                    Hit::EnterShow => cx.emit(Action::Show(ShowOp::Enter)),
                    Hit::Column(i, c) => {
                        self.selected = Some(i);
                        let song = &songs[i];
                        match (c, clicks) {
                            (1, 2) => cx.request(self.rename(i, song, size)),
                            (5, 2) => cx.request(self.notes(i, song, size)),
                            (4, _) => cx.request(HostRequest::ContextMenu {
                                at: pos,
                                items: Self::then_items(i, song.then),
                            }),
                            (_, 2) => {
                                cx.emit(Action::Transport(TransportAction::Locate(song.start)))
                            }
                            _ => {}
                        }
                        cx.redraw();
                    }
                    Hit::Leave => cx.emit(Action::Show(ShowOp::Leave)),
                    Hit::Previous => cx.emit(Action::Show(ShowOp::Previous)),
                    Hit::PlayStop => cx.emit(Action::Show(ShowOp::PlayStop)),
                    Hit::Next => cx.emit(Action::Show(ShowOp::Next)),
                    Hit::Song(k) => cx.emit(Action::Show(if clicks >= 2 {
                        ShowOp::Go(k)
                    } else {
                        ShowOp::Cue(k)
                    })),
                }
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                if model.show_mode() {
                    return false;
                }
                let Some(i) = self.row_at(pos, size, songs.len()) else {
                    return false;
                };
                self.selected = Some(i);
                cx.request(HostRequest::ContextMenu {
                    at: pos,
                    items: Self::row_menu(model, i),
                });
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let hover = if model.show_mode() {
                    None
                } else {
                    self.row_at(pos, size, songs.len())
                };
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll { dy, precise, .. } if !model.show_mode() => {
                self.scroll += if precise { dy } else { dy * ROW_H * 2.0 };
                self.scroll = self.scroll.max(0.0);
                cx.redraw();
                true
            }
            ViewEvent::Key { key, modifiers } => {
                if model.show_mode() {
                    let op = match key {
                        Key::Space => ShowOp::PlayStop,
                        Key::Right | Key::PageDown | Key::Down | Key::Char('n') => ShowOp::Next,
                        Key::Left | Key::PageUp | Key::Up | Key::Char('p') => ShowOp::Previous,
                        Key::Enter => ShowOp::Go(model.show().current),
                        Key::Escape => ShowOp::Leave,
                        _ => return false,
                    };
                    cx.emit(Action::Show(op));
                    return true;
                }
                let Some(i) = self.selected.filter(|i| *i < songs.len()) else {
                    return false;
                };
                match key {
                    Key::Delete | Key::Backspace => {
                        cx.emit(Action::Setlist(SetlistOp::Remove(i)));
                        self.selected = None;
                    }
                    Key::Up if modifiers.alt && i > 0 => {
                        cx.emit(Action::Setlist(SetlistOp::Move { from: i, to: i - 1 }));
                        self.selected = Some(i - 1);
                    }
                    Key::Down if modifiers.alt && i + 1 < songs.len() => {
                        cx.emit(Action::Setlist(SetlistOp::Move { from: i, to: i + 1 }));
                        self.selected = Some(i + 1);
                    }
                    Key::Up => self.selected = Some(i.saturating_sub(1)),
                    Key::Down => self.selected = Some((i + 1).min(songs.len() - 1)),
                    Key::Enter => {
                        cx.emit(Action::Transport(TransportAction::Locate(songs[i].start)));
                    }
                    _ => return false,
                }
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        Some(
            match self.hit(pos, size, model)? {
                Hit::FromSections => "Make the setlist one song for each section, in order",
                Hit::FromSelection => "Add the arranger's edit selection as a song",
                Hit::EnterShow => {
                    "Play the show: the stage screen full screen; each song stops on its last frame"
                }
                Hit::Column(_, 1) => "Double-click to rename",
                Hit::Column(_, 4) => "What happens when the song ends",
                Hit::Column(_, 5) => "Double-click for the performer's notes (lines with \" / \")",
                Hit::Column(..) => "Double-click to go there · Right-click for more",
                Hit::Song(_) => "Click to stand on it, double-click to play it",
                Hit::Previous => "The previous song (←)",
                Hit::Next => "The next song (→)",
                Hit::PlayStop => "Play or stop (Space)",
                Hit::Leave => "Leave the show (Esc)",
            }
            .to_string(),
        )
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.show_mode() || model.transport().playing
    }

    fn min_size(&self) -> Size {
        Size::new(520.0, 160.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Vertical || model.show_mode() {
            return None;
        }
        let l = Self::list_rect(size);
        Some(ScrollInfo {
            content: model.setlist().songs.len() as f32 * ROW_H,
            viewport: l.h,
            offset: self.scroll,
            start: l.y,
            end: 24.0,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Vertical {
            self.scroll = offset.max(0.0);
        }
    }
}

#[cfg(test)]
mod tests;
