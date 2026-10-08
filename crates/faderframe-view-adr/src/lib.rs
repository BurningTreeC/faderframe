//! The ADR cue list: every line to record again — its number, who says
//! it, where (timecode in and out), the line, its takes and their stars —
//! with buttons to rehearse a cue (pre-roll, beeps, streamer) or record it
//! (its track, the cue as the punch range). A click locates to a cue, a
//! double-click on its number, character or line edits it, the right
//! button has the rest (its track, rating takes, removing it).

#![forbid(unsafe_code)]

use faderframe_core::{AdrCueId, TrackId};
use faderframe_project::TrackKind;
use faderframe_project::adr::AdrCue;
use faderframe_session::adr::AdrOp;
use faderframe_session::{Action, Session, TransportAction};
use faderframe_ui_canvas::{
    CanvasView, Color, EventCx, FontFamily, HostRequest, MenuItem, Painter, Point, PointerButton,
    Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent, controls,
};

const BAR_H: f32 = 34.0;
const HEAD_H: f32 = 22.0;
const ROW_H: f32 = 30.0;

/// The columns: from the left, their widths (the line takes the rest).
const NUMBER: f32 = 46.0;
const CHARACTER: f32 = 110.0;
const TC: f32 = 92.0;
const TAKES: f32 = 150.0;
const BUTTONS: f32 = 96.0;

/// What is where in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cell {
    Number,
    Character,
    In,
    Out,
    Line,
    Takes,
    Rehearse,
    Record,
    Done,
}

/// The tool bar's buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    FromTranscript,
    CueSelection,
    Beeps,
    OnPicture,
}

pub struct AdrView {
    theme: Theme,
    scroll: f32,
    selected: Option<AdrCueId>,
    /// The track new cues record on.
    track: Option<TrackId>,
}

impl AdrView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll: 0.0,
            selected: None,
            track: None,
        }
    }

    fn list(size: Size) -> Rect {
        let top = BAR_H + HEAD_H;
        Rect::new(0.0, top, size.w, (size.h - top).max(0.0))
    }

    fn tools(size: Size) -> Vec<(Tool, Rect, &'static str)> {
        let mut x = 10.0;
        let mut out = Vec::new();
        for (tool, w, label) in [
            (Tool::FromTranscript, 140.0, "CUES FROM TRANSCRIPT"),
            (Tool::CueSelection, 120.0, "CUE THE SELECTION"),
            (Tool::Beeps, 90.0, "MAKE BEEPS"),
            (Tool::OnPicture, 90.0, "ON PICTURE"),
        ] {
            if x + w > size.w - 10.0 {
                break;
            }
            out.push((tool, Rect::new(x, 7.0, w, BAR_H - 14.0), label));
            x += w + 8.0;
        }
        out
    }

    /// The cells of a row at `y`, left to right.
    fn cells(size: Size, y: f32) -> Vec<(Cell, Rect)> {
        let line = (size.w - NUMBER - CHARACTER - 2.0 * TC - TAKES - BUTTONS - 16.0).max(80.0);
        let mut x = 8.0;
        let mut out = Vec::new();
        for (cell, w) in [
            (Cell::Number, NUMBER),
            (Cell::Character, CHARACTER),
            (Cell::In, TC),
            (Cell::Out, TC),
            (Cell::Line, line),
            (Cell::Takes, TAKES),
        ] {
            out.push((cell, Rect::new(x, y, w, ROW_H)));
            x += w;
        }
        let b = Rect::new(x + 4.0, y + 5.0, 26.0, ROW_H - 10.0);
        out.push((Cell::Rehearse, b));
        out.push((Cell::Record, Rect::new(b.x + 30.0, b.y, 26.0, b.h)));
        out.push((Cell::Done, Rect::new(b.x + 60.0, b.y, 26.0, b.h)));
        out
    }

    fn row_at(&self, pos: Point, size: Size, model: &Session) -> Option<(usize, Cell)> {
        let list = Self::list(size);
        if !list.contains(pos) {
            return None;
        }
        let i = ((pos.y - list.y + self.scroll) / ROW_H).floor();
        if i < 0.0 || i as usize >= model.adr().cues.len() {
            return None;
        }
        let y = list.y + i * ROW_H - self.scroll;
        let cell = Self::cells(size, y)
            .into_iter()
            .find(|(_, r)| pos.x >= r.x && pos.x < r.right())
            .map_or(Cell::Line, |(c, _)| c);
        Some((i as usize, cell))
    }

    fn clamp(&mut self, n: usize, size: Size) {
        let max = (n as f32 * ROW_H - Self::list(size).h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// The track new cues record on: the one chosen, else the selected
    /// audio track, else the first.
    fn take_track(&self, model: &Session) -> Option<TrackId> {
        let p = model.project();
        let audio = |t: &TrackId| {
            p.track(*t).is_some_and(|t| {
                t.kind == TrackKind::Audio && t.name != faderframe_session::adr::BEEPS_TRACK
            })
        };
        self.track
            .filter(audio)
            .or_else(|| model.selection.tracks.iter().copied().find(audio))
            .or_else(|| p.tracks.iter().map(|t| t.id).find(audio))
    }

    fn timecode(model: &Session, t: faderframe_timeline::MusicalTime) -> String {
        let p = model.project();
        let s = p.timeline.to_samples(t, p.sample_rate as f64);
        model.timecode().label(s, p.sample_rate)
    }

    fn edit(cue: &AdrCue, cell: Cell, at: Rect) -> Option<HostRequest<Action>> {
        let (initial, field) = match cell {
            Cell::Number => (cue.number.clone(), 0),
            Cell::Character => (cue.character.clone(), 1),
            Cell::Line => (cue.text.clone(), 2),
            _ => return None,
        };
        let cue = cue.clone();
        Some(HostRequest::TextInput {
            at,
            initial,
            commit: Box::new(move |text| {
                let mut c = cue.clone();
                let t = text.trim().to_string();
                match field {
                    0 => c.number = t,
                    1 => c.character = t,
                    _ => c.text = t,
                }
                Some(Action::Adr(AdrOp::Set(c)))
            }),
        })
    }

    fn menu(&self, model: &Session, cue: &AdrCue, at: Point) -> HostRequest<Action> {
        let p = model.project();
        let mut items = vec![
            MenuItem::disabled(format!("Cue {} · {}", cue.number, short(&cue.text, 40))),
            MenuItem::new(
                "Rehearse (Pre-Roll, Beeps, Streamer)",
                Action::Adr(AdrOp::Run {
                    cue: cue.id,
                    record: false,
                }),
            )
            .separated(),
            MenuItem::new(
                "Record",
                Action::Adr(AdrOp::Run {
                    cue: cue.id,
                    record: true,
                }),
            ),
            MenuItem::new(
                if cue.done {
                    "Not Done Yet"
                } else {
                    "Mark Done"
                },
                Action::Adr(AdrOp::Set(AdrCue {
                    done: !cue.done,
                    ..cue.clone()
                })),
            ),
        ];
        let tracks: Vec<MenuItem<Action>> = p
            .tracks
            .iter()
            .filter(|t| {
                t.kind == TrackKind::Audio && t.name != faderframe_session::adr::BEEPS_TRACK
            })
            .map(|t| {
                MenuItem::new(
                    t.name.clone(),
                    Action::Adr(AdrOp::Set(AdrCue {
                        track: Some(t.id),
                        ..cue.clone()
                    })),
                )
                .checked(cue.track == Some(t.id))
            })
            .collect();
        if !tracks.is_empty() {
            items.push(MenuItem::submenu("Takes On", tracks).separated());
        }
        let takes = model.adr_takes(cue.id);
        if !takes.is_empty() {
            let rate: Vec<MenuItem<Action>> = takes
                .iter()
                .map(|(clip, take, name, rating)| {
                    let levels = (0..=5u8)
                        .map(|n| {
                            MenuItem::new(
                                if n == 0 {
                                    "Not Rated".to_string()
                                } else {
                                    stars(n)
                                },
                                Action::Adr(AdrOp::RateTake {
                                    clip: *clip,
                                    take: *take,
                                    rating: n,
                                }),
                            )
                            .checked(*rating == n)
                        })
                        .collect();
                    MenuItem::submenu(format!("{name}  {}", stars(*rating)), levels)
                })
                .collect();
            items.push(MenuItem::submenu("Rate Takes", rate));
        }
        items.push(MenuItem::new("Remove Cue", Action::Adr(AdrOp::Remove(cue.id))).separated());
        HostRequest::ContextMenu { at, items }
    }
}

fn stars(n: u8) -> String {
    (0..5).map(|i| if i < n { '★' } else { '☆' }).collect()
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

impl CanvasView<Session, Action> for AdrView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        let ui = &th.ui;
        p.fill(Rect::from_size(size), ui.background);
        let adr = model.adr();
        // The tool bar.
        p.fill(Rect::new(0.0, 0.0, size.w, BAR_H), ui.surface);
        for (tool, r, label) in Self::tools(size) {
            let on = tool == Tool::OnPicture && adr.settings.on_picture;
            controls::led_button(p, r, label, on, ui.accent, th);
        }
        let track = self
            .take_track(model)
            .and_then(|t| model.project().track(t))
            .map_or("no audio track".into(), |t| {
                format!("new cues record on {}", t.name)
            });
        let used: f32 = Self::tools(size)
            .last()
            .map_or(10.0, |(_, r, _)| r.right() + 12.0);
        p.text(
            &track,
            Rect::new(used, 0.0, (size.w - used - 10.0).max(0.0), BAR_H),
            &TextStyle::new(th.fonts.small, ui.text_dim),
        );
        // The column heads.
        let head = Rect::new(0.0, BAR_H, size.w, HEAD_H);
        p.fill(head, ui.surface.darken(0.04));
        p.hline(0.0, size.w, BAR_H + HEAD_H - 0.5, ui.border);
        let small = TextStyle::new(th.fonts.tiny + 0.5, ui.text_dim).bold();
        for (cell, r) in Self::cells(size, BAR_H) {
            let label = match cell {
                Cell::Number => "#",
                Cell::Character => "CHARACTER",
                Cell::In => "IN",
                Cell::Out => "OUT",
                Cell::Line => "LINE",
                Cell::Takes => "TAKES",
                Cell::Rehearse => "",
                Cell::Record => "",
                Cell::Done => "",
            };
            p.text(label, Rect::new(r.x + 4.0, BAR_H, r.w, HEAD_H), &small);
        }
        let list = Self::list(size);
        if adr.cues.is_empty() {
            p.text(
                "No cues — transcribe the dialogue (a clip's menu → Transcribe), then Cues from Transcript; or select a range and Cue the Selection",
                Rect::new(16.0, list.y + 12.0, size.w - 32.0, 24.0),
                &TextStyle::new(th.fonts.small, ui.text_dim),
            );
            return;
        }
        self.clamp(adr.cues.len(), size);
        let running = model.adr_running();
        p.push_clip(list);
        for (i, cue) in adr.cues.iter().enumerate() {
            let y = list.y + i as f32 * ROW_H - self.scroll;
            if y + ROW_H < list.y || y > list.bottom() {
                continue;
            }
            let row = Rect::new(0.0, y, size.w, ROW_H);
            let run = running.filter(|(c, _)| *c == cue.id);
            if let Some((_, rec)) = run {
                let c = if rec { Color::hex(0xff4b4b) } else { ui.accent };
                p.fill(row, c.with_alpha(0.18));
            } else if self.selected == Some(cue.id) {
                p.fill(row, ui.selection.with_alpha(0.3));
            } else if i % 2 == 1 {
                p.fill(row, ui.text.with_alpha(0.025));
            }
            let dim = if cue.done { ui.text_faint } else { ui.text };
            let text = TextStyle::new(th.fonts.small, dim);
            let mono = TextStyle::new(th.fonts.small, ui.text_dim).family(FontFamily::Mono);
            let takes = model.adr_takes(cue.id);
            for (cell, r) in Self::cells(size, y) {
                let inner = Rect::new(r.x + 4.0, r.y, (r.w - 8.0).max(0.0), r.h);
                match cell {
                    Cell::Number => p.text(&cue.number, inner, &text.bold()),
                    Cell::Character => p.text(&cue.character, inner, &text),
                    Cell::In => p.text(&Self::timecode(model, cue.start), inner, &mono),
                    Cell::Out => p.text(&Self::timecode(model, cue.end), inner, &mono),
                    Cell::Line => p.text(&cue.text, inner, &text),
                    Cell::Takes => {
                        let best = takes.iter().map(|t| t.3).max().unwrap_or(0);
                        let label = match takes.len() {
                            0 => "—".to_string(),
                            n => format!("{n} · {}", stars(best)),
                        };
                        p.text(&label, inner, &TextStyle::new(th.fonts.small, ui.text_dim));
                    }
                    Cell::Rehearse => controls::led_button(
                        p,
                        r,
                        "▶",
                        run.is_some_and(|(_, rec)| !rec),
                        ui.accent,
                        th,
                    ),
                    Cell::Record => controls::led_button(
                        p,
                        r,
                        "●",
                        run.is_some_and(|(_, rec)| rec),
                        Color::hex(0xff4b4b),
                        th,
                    ),
                    Cell::Done => {
                        controls::led_button(p, r, "✓", cue.done, Color::hex(0x5ccf7a), th)
                    }
                }
            }
        }
        p.pop_clip();
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
                clicks,
                ..
            } => {
                if pos.y < BAR_H {
                    if button != PointerButton::Primary {
                        return false;
                    }
                    let Some((tool, _, _)) = Self::tools(size)
                        .into_iter()
                        .find(|(_, r, _)| r.contains(pos))
                    else {
                        return false;
                    };
                    let track = self.take_track(model);
                    match tool {
                        Tool::FromTranscript => {
                            cx.emit(Action::Adr(AdrOp::FromTranscript { track }));
                        }
                        Tool::CueSelection => {
                            // Without a range the session says to select one.
                            let r = model.selection.range;
                            cx.emit(Action::Adr(AdrOp::Add {
                                start: r.map_or(Default::default(), |r| r.start),
                                end: r.map_or(Default::default(), |r| r.end),
                                text: String::new(),
                                track,
                            }));
                        }
                        Tool::Beeps => cx.emit(Action::Adr(AdrOp::MakeBeeps)),
                        Tool::OnPicture => {
                            let mut s = model.adr().settings;
                            s.on_picture = !s.on_picture;
                            cx.emit(Action::Adr(AdrOp::Settings(s)));
                        }
                    }
                    return true;
                }
                let Some((i, cell)) = self.row_at(pos, size, model) else {
                    return false;
                };
                let cue = model.adr().cues[i].clone();
                self.selected = Some(cue.id);
                cx.redraw();
                match button {
                    PointerButton::Secondary => {
                        cx.request(self.menu(model, &cue, pos));
                        return true;
                    }
                    PointerButton::Primary => {}
                    _ => return false,
                }
                match cell {
                    Cell::Rehearse | Cell::Record => cx.emit(Action::Adr(AdrOp::Run {
                        cue: cue.id,
                        record: cell == Cell::Record,
                    })),
                    Cell::Done => cx.emit(Action::Adr(AdrOp::Set(AdrCue {
                        done: !cue.done,
                        ..cue.clone()
                    }))),
                    _ if clicks >= 2 => {
                        let list = Self::list(size);
                        let y = list.y + i as f32 * ROW_H - self.scroll;
                        if let Some((_, r)) =
                            Self::cells(size, y).into_iter().find(|(c, _)| *c == cell)
                            && let Some(req) = Self::edit(&cue, cell, r)
                        {
                            cx.request(req);
                        }
                    }
                    _ => cx.emit(Action::Transport(TransportAction::Locate(cue.start))),
                }
                true
            }
            ViewEvent::Scroll { dy, precise, .. } => {
                self.scroll += if precise { dy } else { dy * ROW_H * 3.0 };
                self.clamp(model.adr().cues.len(), size);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        if pos.y < BAR_H {
            let (tool, _, _) = Self::tools(size)
                .into_iter()
                .find(|(_, r, _)| r.contains(pos))?;
            return Some(
                match tool {
                    Tool::FromTranscript => "A cue for every transcribed line (the Lyrics lane) not cued yet",
                    Tool::CueSelection => "A cue over the arranger's selection",
                    Tool::Beeps => "Beeps before every cue, on a track of their own (route it to the talent's headphones)",
                    Tool::OnPicture => "Streamers, punches and the line over the picture while playing",
                }
                .into(),
            );
        }
        let (_, cell) = self.row_at(pos, size, model)?;
        Some(
            match cell {
                Cell::Rehearse => "Rehearse: pre-roll, beeps and streamer, no recording",
                Cell::Record => "Record the cue on its track (the cue is the punch range)",
                Cell::Done => "Done: recorded to everyone's liking",
                Cell::Takes => "Takes and the best one's stars · right-click to rate them",
                _ => "Click to go there · double-click to edit · right-click for more",
            }
            .into(),
        )
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.adr_running().is_some()
    }

    fn min_size(&self) -> Size {
        Size::new(520.0, 140.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Vertical {
            return None;
        }
        let list = Self::list(size);
        Some(ScrollInfo {
            content: model.adr().cues.len() as f32 * ROW_H,
            viewport: list.h,
            offset: self.scroll,
            start: list.y,
            end: 0.0,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Vertical {
            self.scroll = offset.max(0.0);
        }
    }
}
