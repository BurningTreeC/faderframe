//! The Score view: MIDI clips engraved as notation (the clip open in the
//! piano roll, a clip, or tracks a staff each — see
//! `faderframe_session::score`) on pages or along one strip, and edited
//! there. The view keeps only where it looks (scroll, zoom) and where the
//! pointer is; the score, its cursor, the palette's settings and every edit
//! are the session's, so the piano roll and the score stay one model.
//!
//! Pointing: a click on a notehead selects its note (Shift adds, Ctrl
//! toggles) and puts the cursor at it; on a rest or an empty staff it puts
//! the cursor there; in note input (N, the palette's first button) a click
//! enters a note of the chosen value where the pointer is (the line or
//! space under it, the key's accidental, the time snapped to the value),
//! a ghost note showing where. The right button opens the selection's menu
//! (durations, voices, accidentals, transposition, dynamics,
//! articulations, words, chord symbols, quantising, delete).
//!
//! Keys (MuseScore's where they exist): A–G enter notes at the cursor
//! (Shift: added to the chord), 3–7 the values sixteenth … whole (outside
//! note input they set the selection's), `.` dot, T triplet, 0 rest, `+`
//! tie, ↑/↓ a semitone (Ctrl: an octave, Alt: a step of the key), J
//! respell, Alt+1/2 the voice, ←/→ the previous/next note (or the cursor),
//! Delete, Escape, Ctrl+L words, Ctrl+K a chord symbol, Ctrl+A every
//! note, Ctrl+C/X/V the notes' clipboard.

#![forbid(unsafe_code)]

mod toolbar;

#[cfg(test)]
mod tests;

pub use toolbar::Item;

use faderframe_core::{ClipId, NoteId};
use faderframe_leadsheet::engrave::{Anchor, Font, Ink, Page, Place, Pointed, diatonic_at};
use faderframe_leadsheet::glyph::Seg;
use faderframe_leadsheet::score::DIVISIONS;
use faderframe_project::notation::{Dynamic, Mark, ScoreGrid};
use faderframe_session::score::{
    Interval, ScoreCursor, ScoreDoc, ScoreKey, ScoreLayout, ScoreOp, ScoreSettings, Value,
};
use faderframe_session::{Action, SelectMode, Session, StepInput};
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{
    CanvasView, Color, EventCx, FileChoice, HostRequest, Key, MenuItem, Modifiers, Paint, Painter,
    Path, Point, PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};
use std::sync::Arc;

/// Room round the pages.
const GAP: f32 = 24.0;
/// A strip's scale at zoom 1 (pixels a point).
const STRIP_SCALE: f32 = 1.5;

/// What the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pointing {
    /// A note's head (and the event it is drawn in).
    Note(NoteId, Pointed),
    /// A rest: its part, and where it starts.
    Rest { part: usize, at: MusicalTime },
    /// A place on a staff.
    Staff(Place),
}

pub struct ScoreView {
    theme: Theme,
    scroll: Point,
    /// Times the fitted size (1 = the page's width fits the view).
    zoom: f32,
    /// Where the pointer is (for note input's ghost note).
    hover: Option<Point>,
}

impl ScoreView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll: Point::new(0.0, 0.0),
            zoom: 1.0,
            hover: None,
        }
    }

    /// The palette's height in a view `size` wide.
    pub fn palette_height(size: Size) -> f32 {
        toolbar::layout(size.w).1
    }

    /// The palette's items and where they are.
    pub fn palette(size: Size) -> Vec<(Item, Rect)> {
        toolbar::layout(size.w).0
    }

    /// Each page's top left corner in the view and its scale (pixels a
    /// point).
    pub fn page_frames(
        &self,
        size: Size,
        doc: &ScoreDoc,
        layout: ScoreLayout,
    ) -> Vec<(Point, f32)> {
        let top = Self::palette_height(size);
        match layout {
            ScoreLayout::Strip => {
                let s = (STRIP_SCALE * self.zoom).clamp(0.3, 6.0);
                doc.pages
                    .iter()
                    .map(|_| Point::new(GAP - self.scroll.x, top + GAP - self.scroll.y))
                    .take(1)
                    .map(|p| (p, s))
                    .collect()
            }
            ScoreLayout::Pages => {
                let mut y = top + GAP - self.scroll.y;
                doc.pages
                    .iter()
                    .map(|page| {
                        let fit = ((size.w - 2.0 * GAP) / page.width).clamp(0.2, 1.3);
                        let s = (fit * self.zoom).clamp(0.2, 4.0);
                        let x = ((size.w - page.width * s) / 2.0).max(GAP) - self.scroll.x;
                        let at = Point::new(x, y);
                        y += page.height * s + GAP;
                        (at, s)
                    })
                    .collect()
            }
        }
    }

    /// The pages' extent (pixels).
    fn content(&self, size: Size, doc: &ScoreDoc, layout: ScoreLayout) -> Size {
        let frames = self.page_frames(size, doc, layout);
        let mut w: f32 = 0.0;
        let mut h: f32 = 0.0;
        for ((at, s), page) in frames.iter().zip(&doc.pages) {
            w = w.max(at.x + self.scroll.x + page.width * s + GAP);
            h = h.max(at.y + self.scroll.y + page.height * s + GAP - Self::palette_height(size));
        }
        Size::new(w, h)
    }

    fn clamp(&mut self, size: Size, model: &Session) {
        let Some(doc) = model.score() else {
            self.scroll = Point::new(0.0, 0.0);
            return;
        };
        let c = self.content(size, &doc, model.score_settings().layout);
        let view_h = size.h - Self::palette_height(size);
        self.scroll.x = self.scroll.x.clamp(0.0, (c.w - size.w).max(0.0));
        self.scroll.y = self.scroll.y.clamp(0.0, (c.h - view_h).max(0.0));
    }

    /// A view point on a page: the page and the point there (points).
    pub fn to_page(
        &self,
        pos: Point,
        size: Size,
        doc: &ScoreDoc,
        layout: ScoreLayout,
    ) -> Option<(usize, Point)> {
        if pos.y < Self::palette_height(size) {
            return None;
        }
        self.page_frames(size, doc, layout)
            .into_iter()
            .zip(&doc.pages)
            .enumerate()
            .find_map(|(i, ((at, s), page))| {
                let (x, y) = ((pos.x - at.x) / s, (pos.y - at.y) / s);
                ((0.0..=page.width).contains(&x) && (0.0..=page.height).contains(&y))
                    .then_some((i, Point::new(x, y)))
            })
    }

    /// A page's point in the view.
    pub fn to_view(
        &self,
        page: usize,
        at: Point,
        size: Size,
        doc: &ScoreDoc,
        layout: ScoreLayout,
    ) -> Option<Point> {
        let (o, s) = *self.page_frames(size, doc, layout).get(page)?;
        Some(Point::new(o.x + at.x * s, o.y + at.y * s))
    }

    /// What is under `pos`.
    pub fn pointing(
        &self,
        pos: Point,
        size: Size,
        doc: &ScoreDoc,
        layout: ScoreLayout,
    ) -> Option<Pointing> {
        let (page, at) = self.to_page(pos, size, doc, layout)?;
        if let Some(p) = doc.map.event_at(page, at.x, at.y) {
            match p.pitch {
                Some(_) => {
                    if let Some(n) = doc.note_at(p) {
                        return Some(Pointing::Note(n, p));
                    }
                }
                None => {
                    let e = doc.sheet.parts[p.part].measures[p.bar]
                        .all_events()
                        .nth(p.event)?;
                    return Some(Pointing::Rest {
                        part: p.part,
                        at: doc.time_of(p.bar, e.at as f32),
                    });
                }
            }
        }
        doc.map.place_at(page, at.x, at.y).map(Pointing::Staff)
    }

    /// The time a place stands for, snapped to the chosen value (the
    /// value's place the pointer is in, or the next when it is past most
    /// of it).
    pub fn snapped(place: &Place, doc: &ScoreDoc, settings: &ScoreSettings) -> MusicalTime {
        let unit = settings.duration().divisions().max(1) as f32;
        let len = doc
            .sheet
            .parts
            .get(place.part)
            .and_then(|p| p.measures.get(place.bar))
            .map_or(4 * DIVISIONS, |m| m.length()) as f32;
        let k = ((place.at + unit / 3.0) / unit).floor().max(0.0);
        let at = (k * unit).min((len - unit).max(0.0));
        doc.time_of(place.bar, at)
    }

    /// The selected notes the score shows, in time order.
    fn selected(model: &Session, doc: &ScoreDoc) -> Vec<NoteId> {
        let mut out: Vec<NoteId> = model
            .selection
            .notes
            .iter()
            .copied()
            .filter(|n| doc.clips.contains_key(n))
            .collect();
        out.sort_by(|a, b| {
            let t = |n: &NoteId| doc.written(*n).map_or(0.0, |w| w.start);
            t(a).total_cmp(&t(b))
        });
        out
    }

    /// The time words and chord symbols go to: the first selected note's,
    /// else the cursor's, else the score's start.
    fn here(model: &Session, doc: &ScoreDoc) -> MusicalTime {
        Self::selected(model, doc)
            .first()
            .and_then(|n| doc.written(*n))
            .map(|w| MusicalTime::from_quarters(w.start))
            .or_else(|| model.score_cursor().map(|c| c.at))
            .unwrap_or_else(|| doc.start())
    }

    /// The text entry for words from `from` on.
    fn lyrics_entry(
        model: &Session,
        doc: &ScoreDoc,
        from: NoteId,
        at: Rect,
    ) -> HostRequest<Action> {
        let initial = doc
            .clips
            .get(&from)
            .map(|c| model.project().clip_notation(*c).note(from))
            .and_then(|n| n.lyric)
            .unwrap_or_default();
        HostRequest::TextInput {
            at,
            initial,
            commit: Box::new(move |text| {
                Some(Action::Score(ScoreOp::Lyrics {
                    from,
                    text: text.to_string(),
                }))
            }),
        }
    }

    /// The text entry for a chord symbol at `at`.
    fn chord_entry(model: &Session, at: MusicalTime, rect: Rect) -> HostRequest<Action> {
        let initial = model
            .project()
            .chords
            .iter()
            .find(|c| c.start == at)
            .map(|c| c.chord.name(model.project().flats_at(at)))
            .unwrap_or_default();
        HostRequest::TextInput {
            at: rect,
            initial,
            commit: Box::new(move |text| {
                Some(Action::Score(ScoreOp::ChordSymbol {
                    at,
                    text: text.to_string(),
                }))
            }),
        }
    }

    /// The dynamics menu for `notes`.
    fn dynamics_menu(notes: &[NoteId]) -> Vec<MenuItem<Action>> {
        let mut items: Vec<MenuItem<Action>> = Dynamic::ALL
            .iter()
            .map(|d| {
                MenuItem::new(
                    d.name(),
                    Action::Score(ScoreOp::Dynamic {
                        notes: notes.to_vec(),
                        dynamic: Some(*d),
                    }),
                )
            })
            .collect();
        items.push(
            MenuItem::new(
                "No Dynamic",
                Action::Score(ScoreOp::Dynamic {
                    notes: notes.to_vec(),
                    dynamic: None,
                }),
            )
            .separated(),
        );
        items
    }

    /// The articulations menu for `notes` (ticked where all have it).
    fn marks_menu(model: &Session, doc: &ScoreDoc, notes: &[NoteId]) -> Vec<MenuItem<Action>> {
        Mark::ALL
            .iter()
            .map(|m| {
                let all = !notes.is_empty()
                    && notes.iter().all(|n| {
                        doc.clips.get(n).is_some_and(|c| {
                            m.of(&model.project().clip_notation(*c).note(*n).marks)
                        })
                    });
                MenuItem::new(
                    m.label(),
                    Action::Score(ScoreOp::Mark {
                        notes: notes.to_vec(),
                        mark: *m,
                        on: !all,
                    }),
                )
                .checked(all)
            })
            .collect()
    }

    /// The clips' display quantise menu.
    fn display_menu(model: &Session, doc: &ScoreDoc) -> Vec<MenuItem<Action>> {
        let clips: Vec<ClipId> = doc
            .parts
            .iter()
            .flat_map(|p| p.clips.iter().copied())
            .collect();
        let first = clips
            .first()
            .map(|c| model.project().clip_notation(*c))
            .unwrap_or_default();
        let mut items: Vec<MenuItem<Action>> = ScoreGrid::ALL
            .iter()
            .map(|g| {
                MenuItem::new(
                    g.label(),
                    Action::Score(ScoreOp::Display {
                        clips: clips.clone(),
                        grid: *g,
                        legato: first.legato,
                    }),
                )
                .checked(first.grid == *g)
            })
            .collect();
        items.push(
            MenuItem::new(
                "Read Played Notes Legato",
                Action::Score(ScoreOp::Display {
                    clips: clips.clone(),
                    grid: first.grid,
                    legato: !first.legato,
                }),
            )
            .checked(first.legato)
            .separated(),
        );
        let sel = Self::selected(model, doc);
        items.push(
            MenuItem::new(
                if sel.is_empty() {
                    "Quantize the Notes to the Score"
                } else {
                    "Quantize the Selected Notes to the Score"
                },
                Action::Score(ScoreOp::Quantize(sel)),
            )
            .separated(),
        );
        items
    }

    /// The menu of the selected notes.
    pub fn note_menu(model: &Session, doc: &ScoreDoc, notes: &[NoteId]) -> Vec<MenuItem<Action>> {
        let n = notes.to_vec();
        let op = |o: ScoreOp| Action::Score(o);
        let durations = [
            (Value::Whole, "Whole"),
            (Value::Half, "Half"),
            (Value::Quarter, "Quarter"),
            (Value::Eighth, "Eighth"),
            (Value::Sixteenth, "Sixteenth"),
        ]
        .iter()
        .flat_map(|(v, name)| {
            [false, true].map(|dotted| {
                MenuItem::new(
                    if dotted {
                        format!("Dotted {name}")
                    } else {
                        (*name).to_string()
                    },
                    op(ScoreOp::SetDuration {
                        notes: n.clone(),
                        duration: faderframe_leadsheet::score::Duration {
                            value: *v,
                            dots: u8::from(dotted),
                            triplet: false,
                        },
                    }),
                )
            })
        })
        .collect();
        let transpose = [
            ("Up a Semitone", Interval::Semitones(1)),
            ("Down a Semitone", Interval::Semitones(-1)),
            ("Up a Step", Interval::Steps(1)),
            ("Down a Step", Interval::Steps(-1)),
            ("Up an Octave", Interval::Octaves(1)),
            ("Down an Octave", Interval::Octaves(-1)),
        ]
        .into_iter()
        .map(|(l, by)| {
            MenuItem::new(
                l,
                op(ScoreOp::Transpose {
                    notes: n.clone(),
                    by,
                }),
            )
        })
        .collect();
        let accidentals = [
            ("Sharp ♯", 1),
            ("Natural ♮", 0),
            ("Flat ♭", -1),
            ("Double Sharp", 2),
            ("Double Flat", -2),
        ]
        .into_iter()
        .map(|(l, alter)| {
            MenuItem::new(
                l,
                op(ScoreOp::Accidental {
                    notes: n.clone(),
                    alter,
                }),
            )
        })
        .collect();
        let voices = vec![
            MenuItem::new(
                "Voice 1 (Stems Up)",
                op(ScoreOp::Voice {
                    notes: n.clone(),
                    voice: Some(1),
                }),
            ),
            MenuItem::new(
                "Voice 2 (Stems Down)",
                op(ScoreOp::Voice {
                    notes: n.clone(),
                    voice: Some(2),
                }),
            ),
            MenuItem::new(
                "Automatic",
                op(ScoreOp::Voice {
                    notes: n.clone(),
                    voice: None,
                }),
            ),
        ];
        let mut items = vec![
            MenuItem::submenu("Duration", durations),
            MenuItem::submenu("Transpose", transpose),
            MenuItem::submenu("Accidental", accidentals),
            MenuItem::new("Respell", op(ScoreOp::Respell(n.clone()))),
            MenuItem::submenu("Voice", voices),
            MenuItem::submenu("Dynamic", Self::dynamics_menu(&n)).separated(),
            MenuItem::submenu("Articulation", Self::marks_menu(model, doc, &n)),
            MenuItem::new("Quantize to the Score", op(ScoreOp::Quantize(n.clone()))).separated(),
        ];
        if let Some(clip) = n.first().and_then(|x| doc.clips.get(x)) {
            items.push(MenuItem::new(
                "Open in the Piano Roll",
                Action::OpenClipEditor(*clip),
            ));
        }
        items.push(MenuItem::new("Delete", op(ScoreOp::Delete(n))).separated());
        items
    }

    /// A palette button pressed.
    fn press_item(
        &mut self,
        item: Item,
        rect: Rect,
        model: &Session,
        doc: Option<&ScoreDoc>,
        cx: &mut EventCx<'_, Action>,
    ) {
        let settings = model.score_settings();
        let set = |s: ScoreSettings| Action::Score(ScoreOp::Settings(s));
        let key = |k: ScoreKey| Action::Score(ScoreOp::Key(k));
        let below = Point::new(rect.x, rect.bottom());
        match item {
            Item::Entering => cx.emit(key(ScoreKey::ToggleEntering)),
            Item::Value(v) => cx.emit(key(ScoreKey::Value(v))),
            Item::Dot => cx.emit(key(ScoreKey::Dot)),
            Item::Triplet => cx.emit(key(ScoreKey::Triplet)),
            Item::Rest => cx.emit(key(ScoreKey::Rest)),
            Item::Tie => cx.emit(key(ScoreKey::Tie)),
            Item::Voice(v) => cx.emit(key(ScoreKey::Voice(v))),
            Item::Layout => {
                self.scroll = Point::new(0.0, 0.0);
                cx.emit(set(ScoreSettings {
                    layout: match settings.layout {
                        ScoreLayout::Pages => ScoreLayout::Strip,
                        ScoreLayout::Strip => ScoreLayout::Pages,
                    },
                    ..settings
                }));
            }
            _ => {}
        }
        let Some(doc) = doc else { return };
        let sel = Self::selected(model, doc);
        match item {
            Item::Accidental(alter) if !sel.is_empty() => {
                cx.emit(Action::Score(ScoreOp::Accidental { notes: sel, alter }));
            }
            Item::Dynamics if !sel.is_empty() => cx.request(HostRequest::ContextMenu {
                at: below,
                items: Self::dynamics_menu(&sel),
            }),
            Item::Marks if !sel.is_empty() => cx.request(HostRequest::ContextMenu {
                at: below,
                items: Self::marks_menu(model, doc, &sel),
            }),
            Item::Lyrics => match sel.first() {
                Some(from) => cx.request(Self::lyrics_entry(model, doc, *from, rect)),
                None => cx.request(HostRequest::Announce(
                    "Select the note the words start on".into(),
                )),
            },
            Item::Chord => {
                let at = Self::here(model, doc);
                cx.request(Self::chord_entry(model, at, rect));
            }
            Item::Display => cx.request(HostRequest::ContextMenu {
                at: below,
                items: Self::display_menu(model, doc),
            }),
            Item::Step => {
                if model.step_input().is_some() {
                    cx.emit(Action::SetStepInput(None));
                } else if let Some((clip, cursor)) = Self::step_place(model, doc) {
                    cx.emit(Action::SetStepInput(Some(StepInput {
                        clip,
                        cursor,
                        step: settings.length(),
                    })));
                }
            }
            Item::Pdf | Item::MusicXml => {
                let (title, ext, filters) = if item == Item::Pdf {
                    ("Export the Score as PDF", "pdf", vec!["*.pdf".to_string()])
                } else {
                    (
                        "Export the Score as MusicXML",
                        "musicxml",
                        vec!["*.musicxml".to_string(), "*.xml".to_string()],
                    )
                };
                let name: String = doc
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
                cx.request(HostRequest::ChooseFiles {
                    choice: FileChoice::Save {
                        title: title.into(),
                        name: format!("{} Score.{ext}", name.trim()),
                        filters: vec![(ext.to_uppercase(), filters)],
                    },
                    commit: Box::new(|paths| {
                        paths
                            .first()
                            .map(|p| Action::Score(ScoreOp::Export(p.clone())))
                    }),
                });
            }
            _ => {}
        }
    }

    /// Where step input starts: the clip under the cursor (or the first
    /// shown) and the cursor's place in it.
    fn step_place(model: &Session, doc: &ScoreDoc) -> Option<(ClipId, MusicalTime)> {
        let p = model.project();
        let cursor = model.score_cursor();
        let part = cursor.map_or(0, |c| c.part);
        let at = cursor.map_or_else(|| doc.start(), |c| c.at);
        let clips = &doc.parts.get(part)?.clips;
        let clip = clips
            .iter()
            .copied()
            .find(|c| {
                p.clip(*c)
                    .and_then(|x| Some((x.start, x.as_midi()?.length)))
                    .is_some_and(|(s, l)| s <= at && at < s + l)
            })
            .or_else(|| clips.first().copied())?;
        let start = p.clip(clip)?.start;
        Some((clip, (at - start).max(MusicalTime::ZERO)))
    }

    /// A key typed: the action it makes.
    pub fn key_action(
        key: Key,
        modifiers: Modifiers,
        model: &Session,
        doc: &ScoreDoc,
    ) -> Option<KeyAnswer> {
        let k = |k: ScoreKey| Some(KeyAnswer::Act(Box::new(Action::Score(ScoreOp::Key(k)))));
        let sel = Self::selected(model, doc);
        match key {
            Key::Char(c) if modifiers.ctrl || modifiers.meta => match c.to_ascii_lowercase() {
                'a' => {
                    let mut notes: Vec<NoteId> = doc.clips.keys().copied().collect();
                    notes.sort();
                    Some(KeyAnswer::Act(Box::new(Action::SelectNotes {
                        notes,
                        mode: SelectMode::Replace,
                    })))
                }
                'l' => Some(KeyAnswer::Lyrics),
                'k' => Some(KeyAnswer::Chord),
                'c' | 'x' => {
                    let clip = sel.first().and_then(|n| doc.clips.get(n)).copied()?;
                    let notes: Vec<NoteId> = sel
                        .iter()
                        .copied()
                        .filter(|n| doc.clips.get(n) == Some(&clip))
                        .collect();
                    Some(KeyAnswer::Act(Box::new(if c.eq_ignore_ascii_case(&'c') {
                        Action::CopyNotes { clip, notes }
                    } else {
                        Action::CutNotes { clip, notes }
                    })))
                }
                'v' => {
                    let (clip, at) = Self::step_place(model, doc)?;
                    Some(KeyAnswer::Act(Box::new(Action::PasteNotes { clip, at })))
                }
                _ => None,
            },
            Key::Char(c) if modifiers.alt => match c {
                '1' => k(ScoreKey::Voice(1)),
                '2' => k(ScoreKey::Voice(2)),
                _ => None,
            },
            Key::Char(c) => {
                if let Some(step) = "cdefgab".find(c.to_ascii_lowercase()) {
                    return k(ScoreKey::Letter {
                        step: step as u8,
                        chord: modifiers.shift || c.is_ascii_uppercase(),
                    });
                }
                match c {
                    '3' => k(ScoreKey::Value(Value::Sixteenth)),
                    '4' => k(ScoreKey::Value(Value::Eighth)),
                    '5' => k(ScoreKey::Value(Value::Quarter)),
                    '6' => k(ScoreKey::Value(Value::Half)),
                    '7' => k(ScoreKey::Value(Value::Whole)),
                    '.' => k(ScoreKey::Dot),
                    't' | 'T' => k(ScoreKey::Triplet),
                    '0' => k(ScoreKey::Rest),
                    '+' | '=' => k(ScoreKey::Tie),
                    'j' | 'J' => k(ScoreKey::Respell),
                    'n' | 'N' => k(ScoreKey::ToggleEntering),
                    _ => None,
                }
            }
            Key::Up | Key::Down => {
                let d = if key == Key::Up { 1 } else { -1 };
                k(ScoreKey::Transpose(if modifiers.ctrl {
                    Interval::Octaves(d)
                } else if modifiers.alt {
                    Interval::Steps(d)
                } else {
                    Interval::Semitones(d)
                }))
            }
            Key::Left => k(ScoreKey::Previous),
            Key::Right => k(ScoreKey::Next),
            Key::Delete | Key::Backspace => k(ScoreKey::Delete),
            Key::Escape => k(ScoreKey::Escape),
            _ => None,
        }
    }

    fn paint_page(
        &self,
        p: &mut dyn Painter,
        page: &Page,
        at: Point,
        s: f32,
        ink: Color,
        paper: Color,
    ) {
        let r = Rect::new(at.x, at.y, page.width * s, page.height * s);
        p.shadow(r, 2.0, Color::BLACK.with_alpha(0.4), 0.0, 4.0, 14.0);
        p.fill(r, paper);
        p.push_transform(at.x, at.y, s);
        for i in &page.ink {
            paint_ink(p, i, ink);
        }
        p.pop_transform();
    }
}

/// What a key asks for.
pub enum KeyAnswer {
    Act(Box<Action>),
    /// The words' entry (the host's text field).
    Lyrics,
    /// The chord symbol's entry.
    Chord,
}

/// Draw one piece of ink in `color` (page points; the painter transformed).
fn paint_ink(p: &mut dyn Painter, ink: &Ink, color: Color) {
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
            p.fill_path_paint(&path, &Paint::Solid(color));
        }
        Ink::Line { a, b, width } => {
            p.line(Point::new(a.0, a.1), Point::new(b.0, b.1), *width, color);
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
                Anchor::End => Rect::new(x - w - 40.0, y - size * 0.8, w + 40.0, size * 1.1),
            };
            let style = TextStyle::new(*size, color);
            let style = match font {
                Font::SansBold | Font::SerifBold => style.bold(),
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

impl CanvasView<Session, Action> for ScoreView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        self.clamp(size, model);
        let top = Self::palette_height(size);
        let area = Rect::new(0.0, top, size.w, (size.h - top).max(0.0));
        p.push_clip(area);
        let settings = model.score_settings();
        match model.score() {
            Some(doc) => self.paint_score(p, size, model, &doc, settings, th),
            None => {
                let lines = [
                    "No score yet: open a MIDI clip in the piano roll (the score follows it), or choose Score in a MIDI clip's or a track's menu",
                    "N switches note input on: type A–G (Shift adds to the chord), 3–7 for the values, . for a dot, 0 for a rest, + to tie; or click on the staff",
                ];
                for (i, text) in lines.iter().enumerate() {
                    p.text(
                        text,
                        Rect::new(24.0, top + 24.0 + 24.0 * i as f32, size.w - 48.0, 24.0),
                        &TextStyle::new(th.fonts.normal, th.ui.text_dim),
                    );
                }
            }
        }
        p.pop_clip();
        toolbar::paint(p, size.w, model, th);
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let doc = model.score();
        let settings = model.score_settings();
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button,
                modifiers,
                ..
            } => {
                if let Some((item, r)) = Self::palette(size)
                    .into_iter()
                    .find(|(_, r)| r.contains(pos))
                {
                    if button == PointerButton::Primary {
                        self.press_item(item, r, model, doc.as_deref(), cx);
                    }
                    return true;
                }
                let Some(doc) = doc else { return false };
                cx.request(HostRequest::GrabFocus);
                let pointing = self.pointing(pos, size, &doc, settings.layout);
                match (button, pointing) {
                    (PointerButton::Primary, Some(Pointing::Staff(place))) if settings.entering => {
                        let step = place.step;
                        let clef = doc
                            .parts
                            .get(place.part)
                            .map_or(faderframe_leadsheet::score::Clef::Treble, |p| p.clef);
                        cx.emit(Action::Score(ScoreOp::EnterAt {
                            part: place.part,
                            at: Self::snapped(&place, &doc, &settings),
                            diatonic: diatonic_at(step, clef),
                        }));
                    }
                    (PointerButton::Primary, Some(Pointing::Note(note, pointed))) => {
                        let mode = if modifiers.toggle() {
                            SelectMode::Toggle
                        } else if modifiers.shift {
                            SelectMode::Add
                        } else {
                            SelectMode::Replace
                        };
                        let at = doc
                            .written(note)
                            .map_or_else(|| doc.start(), |w| MusicalTime::from_quarters(w.start));
                        cx.emit(Action::Several(vec![
                            Action::SelectNotes {
                                notes: vec![note],
                                mode,
                            },
                            Action::Score(ScoreOp::Cursor(Some(ScoreCursor {
                                part: pointed.part,
                                at,
                            }))),
                        ]));
                    }
                    (PointerButton::Primary, Some(Pointing::Rest { part, at })) => {
                        cx.emit(Action::Several(vec![
                            Action::SelectNotes {
                                notes: Vec::new(),
                                mode: SelectMode::Replace,
                            },
                            Action::Score(ScoreOp::Cursor(Some(ScoreCursor { part, at }))),
                        ]));
                    }
                    (PointerButton::Primary, Some(Pointing::Staff(place))) => {
                        cx.emit(Action::Several(vec![
                            Action::SelectNotes {
                                notes: Vec::new(),
                                mode: SelectMode::Replace,
                            },
                            Action::Score(ScoreOp::Cursor(Some(ScoreCursor {
                                part: place.part,
                                at: Self::snapped(&place, &doc, &settings),
                            }))),
                        ]));
                    }
                    (PointerButton::Secondary, pointing) => {
                        let mut notes = Self::selected(model, &doc);
                        if let Some(Pointing::Note(n, _)) = pointing
                            && !notes.contains(&n)
                        {
                            notes = vec![n];
                            cx.emit(Action::SelectNotes {
                                notes: vec![n],
                                mode: SelectMode::Replace,
                            });
                        }
                        let items = if notes.is_empty() {
                            // The cursor goes where the menu was asked for
                            // (a chord symbol, Ctrl+K, goes there).
                            let at = match pointing {
                                Some(Pointing::Staff(place)) => {
                                    Some((place.part, Self::snapped(&place, &doc, &settings)))
                                }
                                Some(Pointing::Rest { part, at }) => Some((part, at)),
                                _ => None,
                            };
                            if let Some((part, at)) = at {
                                cx.emit(Action::Score(ScoreOp::Cursor(Some(ScoreCursor {
                                    part,
                                    at,
                                }))));
                            }
                            vec![
                                MenuItem::new(
                                    "Note Input",
                                    Action::Score(ScoreOp::Key(ScoreKey::ToggleEntering)),
                                )
                                .checked(settings.entering),
                                MenuItem::submenu("Display", Self::display_menu(model, &doc)),
                            ]
                        } else {
                            Self::note_menu(model, &doc, &notes)
                        };
                        cx.request(HostRequest::ContextMenu { at: pos, items });
                    }
                    _ => return false,
                }
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let was = self.hover;
                self.hover = Some(pos);
                if settings.entering || was.is_some() != self.hover.is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::PointerLeave => {
                self.hover = None;
                cx.redraw();
                false
            }
            ViewEvent::Key { key, modifiers } => {
                let Some(doc) = doc else { return false };
                match Self::key_action(key, modifiers, model, &doc) {
                    Some(KeyAnswer::Act(a)) => cx.emit(*a),
                    Some(KeyAnswer::Lyrics) => {
                        let Some(from) = Self::selected(model, &doc).first().copied() else {
                            cx.request(HostRequest::Announce(
                                "Select the note the words start on".into(),
                            ));
                            return true;
                        };
                        let r = self.entry_rect(size, model, &doc, Some(from));
                        cx.request(Self::lyrics_entry(model, &doc, from, r));
                    }
                    Some(KeyAnswer::Chord) => {
                        let at = Self::here(model, &doc);
                        let r = self.entry_rect(size, model, &doc, None);
                        cx.request(Self::chord_entry(model, at, r));
                    }
                    None => return false,
                }
                true
            }
            ViewEvent::Scroll {
                dx,
                dy,
                precise,
                modifiers,
                ..
            } => {
                if modifiers.ctrl {
                    let f = if dy < 0.0 { 1.1 } else { 1.0 / 1.1 };
                    self.zoom = (self.zoom * f).clamp(0.3, 4.0);
                } else {
                    let step = if precise { 1.0 } else { 60.0 };
                    let strip = settings.layout == ScoreLayout::Strip;
                    if strip || modifiers.shift {
                        self.scroll.x += (if dx != 0.0 { dx } else { dy }) * step;
                    } else {
                        self.scroll.y += dy * step;
                        self.scroll.x += dx * step;
                    }
                }
                self.clamp(size, model);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, _model: &Session) -> Option<String> {
        Self::palette(size)
            .into_iter()
            .find(|(_, r)| r.contains(pos))
            .map(|(i, _)| i.tip().to_string())
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        let doc = model.score()?;
        let c = self.content(size, &doc, model.score_settings().layout);
        let top = Self::palette_height(size);
        Some(match axis {
            ScrollAxis::Vertical => ScrollInfo {
                content: c.h,
                viewport: size.h - top,
                offset: self.scroll.y,
                start: top,
                end: 0.0,
            },
            ScrollAxis::Horizontal => ScrollInfo {
                content: c.w,
                viewport: size.w,
                offset: self.scroll.x,
                start: 0.0,
                end: 0.0,
            },
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        match axis {
            ScrollAxis::Vertical => self.scroll.y = offset.max(0.0),
            ScrollAxis::Horizontal => self.scroll.x = offset.max(0.0),
        }
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Score".into())
    }

    fn min_size(&self) -> Size {
        Size::new(360.0, 180.0)
    }
}

impl ScoreView {
    /// Where a text entry opens: over the note's staff (or the cursor's),
    /// else under the palette.
    fn entry_rect(
        &self,
        size: Size,
        model: &Session,
        doc: &ScoreDoc,
        note: Option<NoteId>,
    ) -> Rect {
        let layout = model.score_settings().layout;
        let place = note
            .and_then(|n| {
                doc.written(n)
                    .map(|w| (w.part, MusicalTime::from_quarters(w.start)))
            })
            .or_else(|| model.score_cursor().map(|c| (c.part, c.at)));
        place
            .and_then(|(part, at)| {
                let (bar, div) = doc.place_of(at)?;
                let (page, x, staff, h) = doc.map.x_of(part, bar, div)?;
                let pt = self.to_view(page, Point::new(x, staff + h), size, doc, layout)?;
                Some(Rect::new(pt.x - 10.0, pt.y + 18.0, 220.0, 26.0))
            })
            .unwrap_or_else(|| Rect::new(12.0, Self::palette_height(size) + 6.0, 220.0, 26.0))
    }

    fn paint_score(
        &self,
        p: &mut dyn Painter,
        size: Size,
        model: &Session,
        doc: &Arc<ScoreDoc>,
        settings: ScoreSettings,
        th: &Theme,
    ) {
        // Paper and ink from the theme's keyboard (white keys, black keys).
        let (paper, ink) = (th.piano.key_white, th.piano.key_black);
        let marked = th.ui.accent;
        let frames = self.page_frames(size, doc, settings.layout);
        let top = Self::palette_height(size);
        let sp = doc.map.space;
        // Notes selected: their heads (and accidentals) in the accent.
        let selected: std::collections::HashSet<u64> = model
            .selection
            .notes
            .iter()
            .filter(|n| doc.clips.contains_key(n))
            .map(|n| n.raw())
            .collect();
        for (pi, ((at, s), page)) in frames.iter().zip(&doc.pages).enumerate() {
            let h = page.height * s;
            if at.y + h < top || at.y > size.h || at.x > size.w || at.x + page.width * s < 0.0 {
                continue;
            }
            self.paint_page(p, page, *at, *s, ink, paper);
            p.push_transform(at.x, at.y, *s);
            if !selected.is_empty() {
                for (part, bars) in doc.map.events.iter().enumerate() {
                    for (bar, events) in bars.iter().enumerate() {
                        let Some(m) = doc.sheet.parts.get(part).and_then(|x| x.measures.get(bar))
                        else {
                            continue;
                        };
                        for (b, e) in events.iter().zip(m.all_events()) {
                            let Some(b) = b.as_ref().filter(|b| b.page == pi && !b.rest) else {
                                continue;
                            };
                            for (head, src) in b.heads.iter().zip(&e.sources) {
                                if !selected.contains(src) {
                                    continue;
                                }
                                for i in [Some(head.ink), head.accidental].into_iter().flatten() {
                                    if let Some(x) = page.ink.get(i) {
                                        paint_ink(p, x, marked);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // The cursor, and step input's.
            let mut cursor_at = |part: usize, t: MusicalTime, color: Color| {
                if let Some((bar, div)) = doc.place_of(t)
                    && let Some((page, x, staff, h)) = doc.map.x_of(part, bar, div)
                    && page == pi
                {
                    p.fill_rounded(
                        Rect::new(x - 0.9 * sp, staff - 1.5 * sp, 1.8 * sp, h + 3.0 * sp),
                        0.5 * sp,
                        &Paint::Solid(color.with_alpha(0.22)),
                    );
                    p.line(
                        Point::new(x - 0.9 * sp, staff - 1.5 * sp),
                        Point::new(x - 0.9 * sp, staff + h + 1.5 * sp),
                        0.25 * sp,
                        color,
                    );
                }
            };
            if let Some(c) = model.score_cursor() {
                cursor_at(c.part, c.at, th.piano.step_cursor);
            }
            if let Some(st) = model.step_input()
                && let Some(clip) = model.project().clip(st.clip)
                && let Some(part) = doc.parts.iter().position(|x| x.clips.contains(&st.clip))
            {
                cursor_at(part, clip.start + st.cursor, th.ui.accent);
            }
            // Note input's ghost note where a click would put it.
            if settings.entering
                && let Some(pos) = self.hover
                && let Some((page, at_page)) = self.to_page(pos, size, doc, settings.layout)
                && page == pi
                && let Some(place) = doc.map.place_at(page, at_page.x, at_page.y)
            {
                let t = Self::snapped(&place, doc, &settings);
                if let Some((bar, div)) = doc.place_of(t)
                    && let Some((_, x, staff, _)) = doc.map.x_of(place.part, bar, div)
                {
                    let y = staff + 4.0 * sp - place.step as f32 * 0.5 * sp;
                    p.fill_rounded(
                        Rect::new(x - 0.62 * sp, y - 0.5 * sp, 1.24 * sp, sp),
                        0.5 * sp,
                        &Paint::Solid(marked.with_alpha(0.55)),
                    );
                    // Ledger lines to it.
                    let mut l = -2;
                    while l >= place.step {
                        let ly = staff + 4.0 * sp - l as f32 * 0.5 * sp;
                        p.line(
                            Point::new(x - sp, ly),
                            Point::new(x + sp, ly),
                            0.16 * sp,
                            marked.with_alpha(0.55),
                        );
                        l -= 2;
                    }
                    let mut l = 10;
                    while l <= place.step {
                        let ly = staff + 4.0 * sp - l as f32 * 0.5 * sp;
                        p.line(
                            Point::new(x - sp, ly),
                            Point::new(x + sp, ly),
                            0.16 * sp,
                            marked.with_alpha(0.55),
                        );
                        l += 2;
                    }
                }
            }
            p.pop_transform();
        }
        // What is shown, and how notes go in.
        let label = format!(
            "{} · {} bar{} · {}{}",
            doc.sheet.title,
            doc.sheet.bars(),
            if doc.sheet.bars() == 1 { "" } else { "s" },
            if settings.entering {
                "note input"
            } else {
                "select"
            },
            if settings.voice == 2 {
                " · voice 2"
            } else {
                ""
            },
        );
        let style = TextStyle::new(th.fonts.small, th.ui.text_dim).right();
        p.text(
            &label,
            Rect::new(size.w - 420.0, size.h - 22.0, 410.0, 18.0),
            &style,
        );
    }
}
