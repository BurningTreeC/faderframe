//! The Lead Sheet view: the engraved pages of the lead sheet made last
//! (a clip's melody with its chords and words), on paper, scrolled and
//! zoomed (Ctrl + wheel); a strip above to write its rhythms with
//! sixteenths or triplets and to export it as PDF or MusicXML.

#![forbid(unsafe_code)]

use faderframe_leadsheet::Grid;
use faderframe_leadsheet::engrave::{Anchor, Ink, Page};
use faderframe_leadsheet::glyph::Seg;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, Color, EventCx, FileChoice, HostRequest, Paint, Painter, Path, Point,
    PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 36.0;
const GAP: f32 = 24.0;
const PAPER: Color = Color::hex(0xfdfcf8);
const INK: Color = Color::hex(0x141414);

/// The strip's buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Grid(Grid),
    Pdf,
    MusicXml,
}

const GRIDS: [(Grid, &str); 3] = [
    (Grid::Auto, "Auto"),
    (Grid::Straight, "16ths"),
    (Grid::Triplets, "Triplets"),
];

pub struct LeadSheetView {
    theme: Theme,
    scroll: f32,
    /// Times the page width fits the view (1 = fit).
    zoom: f32,
}

impl LeadSheetView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll: 0.0,
            zoom: 1.0,
        }
    }

    /// The strip's buttons and where they are.
    pub fn buttons(size: Size) -> Vec<(Button, Rect)> {
        let mut out = Vec::new();
        let mut x = size.w - 12.0;
        for (b, w) in [(Button::MusicXml, 132.0), (Button::Pdf, 96.0)] {
            x -= w;
            out.push((b, Rect::new(x, 6.0, w, HEADER_H - 12.0)));
            x -= 8.0;
        }
        x -= 16.0;
        for (g, _) in GRIDS.iter().rev() {
            x -= 70.0;
            out.push((Button::Grid(*g), Rect::new(x, 6.0, 68.0, HEADER_H - 12.0)));
        }
        out
    }

    /// Points to pixels, and where the first page starts.
    fn scale(&self, size: Size, page: &Page) -> (f32, f32) {
        // The page's width fits, but no larger than 1.3 times.
        let fit = ((size.w - 2.0 * GAP) / page.width).clamp(0.2, 1.3);
        let s = (fit * self.zoom).clamp(0.2, 4.0);
        let x = ((size.w - page.width * s) / 2.0).max(GAP);
        (s, x)
    }

    fn content_height(&self, size: Size, pages: &[Page]) -> f32 {
        pages.first().map_or(0.0, |p| {
            let (s, _) = self.scale(size, p);
            pages.len() as f32 * (p.height * s + GAP) + GAP
        })
    }

    fn clamp(&mut self, size: Size, model: &Session) {
        let h = model
            .lead_sheet()
            .map_or(0.0, |d| self.content_height(size, &d.pages));
        let max = (h - (size.h - HEADER_H)).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn paint_page(&self, p: &mut dyn Painter, page: &Page, at: Point, s: f32) {
        let paper = Rect::new(at.x, at.y, page.width * s, page.height * s);
        p.shadow(paper, 2.0, Color::BLACK.with_alpha(0.45), 0.0, 4.0, 14.0);
        p.fill(paper, PAPER);
        p.push_transform(at.x, at.y, s);
        for ink in &page.ink {
            match ink {
                Ink::Fill(segs) => {
                    let mut path = Path::new();
                    for seg in segs {
                        match *seg {
                            Seg::M(x, y) => {
                                path.move_to(Point::new(x, y));
                            }
                            Seg::L(x, y) => {
                                path.line_to(Point::new(x, y));
                            }
                            Seg::C(a, b, c, d, e, f) => {
                                path.cubic_to(Point::new(a, b), Point::new(c, d), Point::new(e, f));
                            }
                            Seg::Z => {
                                path.close();
                            }
                        }
                    }
                    p.fill_path_paint(&path, &Paint::Solid(INK));
                }
                Ink::Line { a, b, width } => {
                    p.line(Point::new(a.0, a.1), Point::new(b.0, b.1), *width, INK);
                }
                Ink::Text {
                    text,
                    x,
                    y,
                    size,
                    anchor,
                    font,
                } => {
                    let w = faderframe_leadsheet::engrave::text_width(text, *font, *size) + 4.0;
                    let r = match anchor {
                        Anchor::Start => Rect::new(*x, y - size * 0.8, w + 40.0, size * 1.1),
                        Anchor::Middle => {
                            Rect::new(x - w / 2.0 - 20.0, y - size * 0.8, w + 40.0, size * 1.1)
                        }
                        Anchor::End => {
                            Rect::new(x - w - 40.0, y - size * 0.8, w + 40.0, size * 1.1)
                        }
                    };
                    let style = TextStyle::new(*size, INK);
                    let style = match font {
                        faderframe_leadsheet::engrave::Font::SansBold
                        | faderframe_leadsheet::engrave::Font::SerifBold => style.bold(),
                        _ => style,
                    };
                    let style = match anchor {
                        Anchor::Start => style,
                        Anchor::Middle => style.center(),
                        Anchor::End => style.right(),
                    };
                    p.text(text, r, &style);
                }
            }
        }
        p.pop_transform();
    }
}

impl CanvasView<Session, Action> for LeadSheetView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        self.clamp(size, model);
        let doc = model.lead_sheet();
        // The pages.
        let area = Rect::new(0.0, HEADER_H, size.w, (size.h - HEADER_H).max(0.0));
        p.push_clip(area);
        match doc {
            Some(d) => {
                let mut y = HEADER_H + GAP - self.scroll;
                for page in &d.pages {
                    let (s, x) = self.scale(size, page);
                    let h = page.height * s;
                    if y + h > HEADER_H && y < size.h {
                        self.paint_page(p, page, Point::new(x, y), s);
                    }
                    y += h + GAP;
                }
            }
            None => {
                let text = if model.making_lead_sheet() {
                    "Listening to the melody…"
                } else {
                    "No lead sheet yet: right-click a clip in the arranger and choose Lead Sheet (an audio clip's melody is heard, a MIDI clip's notes are read; chords from the chord track, words from the lyrics)"
                };
                p.text(
                    text,
                    Rect::new(24.0, HEADER_H + 24.0, size.w - 48.0, 40.0),
                    &TextStyle::new(th.fonts.normal, th.ui.text_dim),
                );
            }
        }
        p.pop_clip();
        // The strip.
        let header = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(header, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        p.text(
            "Lead Sheet",
            Rect::new(12.0, 0.0, 100.0, HEADER_H),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        if let Some(d) = doc {
            let busy = if model.making_lead_sheet() {
                " · listening…"
            } else {
                ""
            };
            p.text(
                &format!(
                    "{} · {} bars · {} page{}{busy}",
                    d.part,
                    d.sheet.measures.len(),
                    d.pages.len(),
                    if d.pages.len() == 1 { "" } else { "s" }
                ),
                Rect::new(112.0, 0.0, (size.w - 600.0).max(60.0), HEADER_H),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
        }
        for (b, r) in Self::buttons(size) {
            let (label, on) = match b {
                Button::Grid(g) => (
                    GRIDS.iter().find(|(x, _)| *x == g).map_or("", |(_, l)| *l),
                    doc.is_some_and(|d| d.grid == g),
                ),
                Button::Pdf => ("Export PDF…", false),
                Button::MusicXml => ("Export MusicXML…", false),
            };
            let enabled = doc.is_some();
            p.fill_rounded(
                r,
                4.0,
                &Paint::Solid(if on {
                    th.ui.selection
                } else {
                    th.ui.text.with_alpha(0.07)
                }),
            );
            p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
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
                let Some(doc) = model.lead_sheet() else {
                    return false;
                };
                let Some((b, _)) = Self::buttons(size)
                    .into_iter()
                    .find(|(_, r)| r.contains(pos))
                else {
                    return false;
                };
                let name = |ext: &str| {
                    let t: String = doc
                        .sheet
                        .title
                        .chars()
                        .map(|c| {
                            if c.is_alphanumeric() || c == ' ' || c == '-' {
                                c
                            } else {
                                '_'
                            }
                        })
                        .collect();
                    format!("{} Lead Sheet.{ext}", t.trim())
                };
                match b {
                    Button::Grid(grid) => cx.emit(Action::MakeLeadSheet { of: doc.of, grid }),
                    Button::Pdf => cx.request(HostRequest::ChooseFiles {
                        choice: FileChoice::Save {
                            title: "Export the Lead Sheet as PDF".into(),
                            name: name("pdf"),
                            filters: vec![("PDF".into(), vec!["*.pdf".into()])],
                        },
                        commit: Box::new(|paths| {
                            paths.first().map(|p| Action::ExportLeadSheet(p.clone()))
                        }),
                    }),
                    Button::MusicXml => cx.request(HostRequest::ChooseFiles {
                        choice: FileChoice::Save {
                            title: "Export the Lead Sheet as MusicXML".into(),
                            name: name("musicxml"),
                            filters: vec![(
                                "MusicXML".into(),
                                vec!["*.musicxml".into(), "*.xml".into()],
                            )],
                        },
                        commit: Box::new(|paths| {
                            paths.first().map(|p| Action::ExportLeadSheet(p.clone()))
                        }),
                    }),
                }
                true
            }
            ViewEvent::Scroll {
                dy,
                precise,
                modifiers,
                ..
            } => {
                if modifiers.ctrl {
                    let f = if dy < 0.0 { 1.1 } else { 1.0 / 1.1 };
                    self.zoom = (self.zoom * f).clamp(0.3, 4.0);
                } else {
                    self.scroll += if precise { dy } else { dy * 60.0 };
                }
                self.clamp(size, model);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn min_size(&self) -> Size {
        Size::new(320.0, 160.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use faderframe_engine::EngineConfig;
    use faderframe_project::{Clip, ClipContent, Command, MidiClip, MidiNote, TrackKind};
    use faderframe_session::leadsheet::LeadSheetOf;
    use faderframe_timeline::MusicalTime;
    use faderframe_ui_canvas::RecordingPainter;

    #[test]
    fn a_midi_clip_becomes_pages_and_the_strip_rewrites_and_exports_it() {
        let mut s = Session::new(
            faderframe_project::Project::new("Tune", 48_000),
            None,
            EngineConfig::default(),
        )
        .unwrap();
        let t = s.add_track(TrackKind::Midi).unwrap();
        let q = MusicalTime::from_quarters;
        let notes: Vec<MidiNote> = [
            (0.0, 1.0, 60),
            (1.0, 0.5, 62),
            (1.5, 0.5, 64),
            (2.0, 2.0, 67),
        ]
        .iter()
        .enumerate()
        .map(|(i, &(a, l, k))| MidiNote {
            id: faderframe_core::NoteId(9000 + i as u64),
            start: q(a),
            length: q(l),
            key: k,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
        })
        .collect();
        let clip = faderframe_core::ClipId(8000);
        s.dispatch(Action::Edit(Command::AddClip {
            clip: Box::new(Clip {
                id: clip,
                track: t,
                name: "tune".into(),
                color: None,
                start: MusicalTime::ZERO,
                muted: false,
                content: ClipContent::Midi(MidiClip {
                    length: q(8.0),
                    notes,
                    ..MidiClip::default()
                }),
            }),
        }))
        .unwrap();
        s.dispatch(Action::MakeLeadSheet {
            of: LeadSheetOf::Clip(clip),
            grid: Grid::Auto,
        })
        .unwrap();
        let doc = s.lead_sheet().unwrap();
        assert_eq!(doc.sheet.measures.len(), 2);
        let mut view = LeadSheetView::new(Theme::default());
        let size = Size::new(900.0, 700.0);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        assert!(p.balanced_clips());
        assert!(p.texts().contains(&"Tune"));
        // The strip: triplets, then the export's file chooser.
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let click = |b: Button| {
            let r = LeadSheetView::buttons(size)
                .into_iter()
                .find(|(x, _)| *x == b)
                .unwrap()
                .1;
            ViewEvent::PointerDown {
                pos: r.center(),
                button: PointerButton::Primary,
                modifiers: faderframe_ui_canvas::Modifiers::NONE,
                clicks: 1,
            }
        };
        {
            let mut cx = EventCx::new(&mut actions, &mut requests);
            view.event(&click(Button::Grid(Grid::Triplets)), size, &s, &mut cx);
            view.event(&click(Button::Pdf), size, &s, &mut cx);
        }
        assert_eq!(
            actions,
            [Action::MakeLeadSheet {
                of: LeadSheetOf::Clip(clip),
                grid: Grid::Triplets
            }]
        );
        assert!(matches!(requests[0], HostRequest::ChooseFiles { .. }));
        // Exports.
        let dir = std::env::temp_dir().join(format!("ff-leadsheet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["t.pdf", "t.musicxml"] {
            s.dispatch(Action::ExportLeadSheet(dir.join(name))).unwrap();
            assert!(std::fs::metadata(dir.join(name)).unwrap().len() > 500);
        }
        assert!(
            s.dispatch(Action::ExportLeadSheet(dir.join("t.doc")))
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
