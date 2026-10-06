//! The pitch editor: an audio clip's notes (found by
//! `Session::detect_pitch`) as blobs at the pitch they are heard at, the
//! pitch curve played drawn through them and, where it was moved, the one
//! sung. Notes are selected by clicking or a rubber band and moved by
//! dragging (whole semitones; Alt: freely) or with the arrow keys; a
//! double click splits a note there; the toolbar corrects the selection
//! (or every note) to the key, straightens it, keeps or moves the
//! formants and resets. Time runs left to right as the clip plays on the
//! timeline; pitch bottom to top, like the piano roll.

#![forbid(unsafe_code)]

use faderframe_core::ClipId;
use faderframe_project::pitch::{PitchEdit, PitchNote};
use faderframe_project::{AudioClip, Clip};
use faderframe_session::pitch::PitchOp;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, Color, Cursor, EventCx, FontFamily, HostRequest, Key, MenuItem, Modifiers, Paint,
    Painter, Path, Point, PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme,
    ViewEvent,
};

const TOOLBAR_H: f32 = 34.0;
const RULER_H: f32 = 20.0;
/// Black keys' share of the keyboard's width (as in the piano roll).
const BLACK_KEY_W: f32 = 0.62;
/// The highest and lowest notes shown (C7, C1).
const TOP: i32 = 96;
const BOTTOM: i32 = 24;
const MIN_ROW: f32 = 6.0;
const MAX_ROW: f32 = 40.0;

/// A toolbar control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Detect,
    Correct,
    Amount,
    Drift,
    Formants,
    Reset,
    Remove,
    Info,
}

/// What a press started.
#[derive(Clone, Debug, PartialEq)]
enum Drag {
    /// Moving notes: where the press was, the move sent last, whether the
    /// gesture has begun.
    Move {
        from: f32,
        notes: Vec<usize>,
        sent: f32,
        begun: bool,
    },
    /// Selecting by a rubber band.
    Band { from: Point, to: Point, add: bool },
    /// Changing a toolbar value (pitch amount or straightening).
    Value { item: Item, from: f32, start: f32 },
}

pub struct PitchView {
    theme: Theme,
    /// Pixels a second and the horizontal scroll (pixels).
    px_per_sec: f32,
    scroll_x: f32,
    row_h: f32,
    scroll_y: f32,
    /// Selected notes (indices into the edit's notes).
    selected: Vec<usize>,
    /// The clip and its note count when last painted (a new clip or new
    /// notes reset the selection and the view).
    seen: Option<(ClipId, usize)>,
    drag: Option<Drag>,
    hover: Option<usize>,
    /// Correct Pitch: how far to the key (0…1) and how much straightened.
    amount: f32,
    drift: f32,
    /// The size last painted (for fitting a new clip).
    last_size: Size,
}

/// The clip shown and what places it.
struct Shown<'a> {
    id: ClipId,
    clip: &'a Clip,
    audio: &'a AudioClip,
    /// Project rate.
    rate: f64,
    /// The clip's start on the timeline (samples).
    start: i64,
}

impl Shown<'_> {
    fn edit(&self) -> Option<&PitchEdit> {
        self.audio.pitch.as_ref()
    }

    /// Seconds into the clip where source frame `f` plays.
    fn seconds_of(&self, f: i64) -> f64 {
        let a = self.audio;
        let out = match &a.warp {
            Some(w) => w.output_of(a.source_offset, a.length, f),
            None => f - a.source_offset,
        };
        out as f64 / self.rate
    }

    /// The source frame playing `seconds` into the clip.
    fn source_at(&self, seconds: f64) -> i64 {
        self.audio.source_at(seconds * self.rate).round() as i64
    }

    fn seconds(&self) -> f64 {
        self.audio.length as f64 / self.rate
    }
}

fn shown(model: &Session) -> Option<Shown<'_>> {
    let id = model.pitch_clip()?;
    let p = model.project();
    let clip = p.clip(id)?;
    let audio = clip.as_audio()?;
    let rate = p.sample_rate as f64;
    Some(Shown {
        id,
        clip,
        audio,
        rate,
        start: p.timeline.to_samples(clip.start, rate),
    })
}

/// A note's name (MIDI 60 = C4).
pub fn note_name(n: i32) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[n.rem_euclid(12) as usize],
        n.div_euclid(12) - 1
    )
}

fn is_black(n: i32) -> bool {
    matches!(n.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

impl PitchView {
    pub fn new(theme: Theme) -> Self {
        let row_h = theme.piano.row_height.clamp(MIN_ROW, MAX_ROW);
        Self {
            theme,
            px_per_sec: 120.0,
            scroll_x: 0.0,
            row_h,
            scroll_y: (TOP - 72) as f32 * row_h,
            selected: Vec::new(),
            seen: None,
            drag: None,
            hover: None,
            amount: 1.0,
            drift: 0.5,
            last_size: Size::new(0.0, 0.0),
        }
    }

    /// The keyboard's width (the piano roll's).
    fn keys_w(&self) -> f32 {
        self.theme.piano.keyboard_width
    }

    fn grid(&self, size: Size) -> Rect {
        let k = self.keys_w();
        Rect::new(
            k,
            TOOLBAR_H + RULER_H,
            (size.w - k).max(0.0),
            (size.h - TOOLBAR_H - RULER_H).max(0.0),
        )
    }

    fn x_of(&self, seconds: f64, g: Rect) -> f32 {
        g.x + seconds as f32 * self.px_per_sec - self.scroll_x
    }

    fn seconds_at(&self, x: f32, g: Rect) -> f64 {
        f64::from((x - g.x + self.scroll_x) / self.px_per_sec)
    }

    /// The centre of note `n`'s row.
    fn y_of(&self, n: f32, g: Rect) -> f32 {
        g.y + (TOP as f32 - n + 0.5) * self.row_h - self.scroll_y
    }

    fn note_at_y(&self, y: f32, g: Rect) -> f32 {
        TOP as f32 + 0.5 - (y - g.y + self.scroll_y) / self.row_h
    }

    fn content_h(&self) -> f32 {
        (TOP - BOTTOM + 1) as f32 * self.row_h
    }

    fn clamp(&mut self, size: Size, model: &Session) {
        let g = self.grid(size);
        self.scroll_y = self.scroll_y.clamp(0.0, (self.content_h() - g.h).max(0.0));
        let w = shown(model).map_or(0.0, |s| s.seconds() as f32 * self.px_per_sec);
        self.scroll_x = self.scroll_x.clamp(0.0, (w - g.w * 0.5).max(0.0));
    }

    /// Fit a newly shown clip: its length across, its notes in the middle.
    fn fit(&mut self, size: Size, s: &Shown<'_>) {
        let g = self.grid(size);
        if g.w > 10.0 {
            self.px_per_sec = ((g.w - 24.0) / s.seconds().max(0.1) as f32).clamp(10.0, 2_000.0);
        }
        self.scroll_x = 0.0;
        let mut heard: Vec<f32> = s
            .edit()
            .map(|e| e.notes.iter().map(PitchNote::heard).collect())
            .unwrap_or_default();
        heard.sort_by(f32::total_cmp);
        let mid = heard.get(heard.len() / 2).copied().unwrap_or(60.0);
        self.scroll_y = (TOP as f32 - mid + 0.5) * self.row_h - g.h / 2.0;
    }

    /// The blob of note `n`.
    fn note_rect(&self, s: &Shown<'_>, n: &PitchNote, g: Rect) -> Rect {
        let x0 = self.x_of(s.seconds_of(n.start), g);
        let x1 = self.x_of(s.seconds_of(n.end), g);
        let h = (self.row_h * 0.8).max(4.0);
        Rect::new(x0, self.y_of(n.heard(), g) - h / 2.0, (x1 - x0).max(2.0), h)
    }

    /// The note under `pos`.
    pub fn note_at(&self, pos: Point, size: Size, model: &Session) -> Option<usize> {
        let g = self.grid(size);
        if !g.contains(pos) {
            return None;
        }
        let s = shown(model)?;
        s.edit()?
            .notes
            .iter()
            .enumerate()
            .rev()
            .find(|(_, n)| self.note_rect(&s, n, g).contains(pos))
            .map(|(i, _)| i)
    }

    /// The toolbar's controls and their rectangles.
    pub fn toolbar(&self, size: Size, model: &Session) -> Vec<(Item, Rect, String, bool)> {
        let s = shown(model);
        let edit = s.as_ref().and_then(Shown::edit);
        let detecting = model.detecting_pitch();
        let mut items = vec![(
            Item::Detect,
            if detecting {
                "Finding Notes…".to_string()
            } else if edit.is_some() {
                "Detect Again".into()
            } else {
                "Detect Pitch".into()
            },
            false,
            98.0,
        )];
        if edit.is_some() {
            let keep = edit.is_some_and(|e| e.keep_formants);
            items.extend([
                (Item::Correct, "Correct".to_string(), false, 64.0),
                (
                    Item::Amount,
                    format!("Pitch {:.0}%", self.amount * 100.0),
                    false,
                    78.0,
                ),
                (
                    Item::Drift,
                    format!("Straighten {:.0}%", self.drift * 100.0),
                    false,
                    106.0,
                ),
                (
                    Item::Formants,
                    if keep {
                        "Formants Kept".into()
                    } else {
                        "Formants Move".into()
                    },
                    keep,
                    104.0,
                ),
                (Item::Reset, "Reset".into(), false, 52.0),
                (Item::Remove, "Remove".into(), false, 62.0),
            ]);
        }
        let mut x = 8.0;
        let mut out: Vec<(Item, Rect, String, bool)> = items
            .into_iter()
            .map(|(item, label, on, w)| {
                let r = Rect::new(x, 5.0, w, TOOLBAR_H - 10.0);
                x += w + if matches!(item, Item::Detect | Item::Drift) {
                    14.0
                } else {
                    4.0
                };
                (item, r, label, on)
            })
            .collect();
        out.push((
            Item::Info,
            Rect::new(x + 8.0, 0.0, (size.w - x - 16.0).max(0.0), TOOLBAR_H),
            self.info(model),
            false,
        ));
        out
    }

    /// The toolbar's right side: the clip and the selection.
    fn info(&self, model: &Session) -> String {
        let Some(s) = shown(model) else {
            return String::new();
        };
        let Some(e) = s.edit() else {
            return s.clip.name.clone();
        };
        let picked: Vec<&PitchNote> = self
            .selected
            .iter()
            .filter_map(|i| e.notes.get(*i))
            .collect();
        match picked.as_slice() {
            [] => format!("{} · {} notes", s.clip.name, e.notes.len()),
            [n] => {
                let heard = n.heard();
                let cents = ((heard - heard.round()) * 100.0).round() as i32;
                format!(
                    "{} {:+}¢ · moved {:+.2} · straightened {:.0}% · formants {:+.1}",
                    note_name(heard.round() as i32),
                    cents,
                    n.shift,
                    n.drift * 100.0,
                    n.formant
                )
            }
            many => format!("{} notes selected", many.len()),
        }
    }

    fn item_at(&self, pos: Point, size: Size, model: &Session) -> Option<Item> {
        self.toolbar(size, model)
            .into_iter()
            .find(|(i, r, ..)| *i != Item::Info && r.contains(pos))
            .map(|(i, ..)| i)
    }

    /// The notes an edit applies to: the selection, or all.
    fn targets(&self) -> Vec<usize> {
        self.selected.clone()
    }

    fn click_item(&mut self, item: Item, model: &Session, cx: &mut EventCx<'_, Action>) {
        let Some(s) = shown(model) else {
            return;
        };
        let op = match item {
            Item::Detect => {
                if !model.detecting_pitch() {
                    cx.emit(Action::DetectPitch { clips: vec![s.id] });
                }
                return;
            }
            Item::Correct => PitchOp::Correct {
                notes: self.targets(),
                amount: self.amount,
                drift: self.drift,
            },
            Item::Formants => PitchOp::KeepFormants(!s.edit().is_some_and(|e| e.keep_formants)),
            Item::Reset => PitchOp::Reset {
                notes: self.targets(),
            },
            Item::Remove => {
                self.selected.clear();
                PitchOp::Remove
            }
            Item::Amount | Item::Drift | Item::Info => return,
        };
        cx.emit(Action::EditPitch { clip: s.id, op });
    }

    fn note_menu(&self, at: Point, s: &Shown<'_>, note: usize) -> HostRequest<Action> {
        let notes = if self.selected.contains(&note) {
            self.selected.clone()
        } else {
            vec![note]
        };
        let op = |op: PitchOp| Action::EditPitch { clip: s.id, op };
        let formant = s
            .edit()
            .and_then(|e| e.notes.get(note))
            .map_or(0.0, |n| n.formant);
        let mut items = vec![
            MenuItem::new(
                "Correct to the Key",
                op(PitchOp::Correct {
                    notes: notes.clone(),
                    amount: 1.0,
                    drift: self.drift,
                }),
            ),
            MenuItem::new(
                "Straighten",
                op(PitchOp::Set {
                    notes: notes.clone(),
                    shift: None,
                    drift: Some(1.0),
                    formant: None,
                }),
            ),
            MenuItem::new(
                "Formants Up a Semitone",
                op(PitchOp::Set {
                    notes: notes.clone(),
                    shift: None,
                    drift: None,
                    formant: Some(formant + 1.0),
                }),
            )
            .separated(),
            MenuItem::new(
                "Formants Down a Semitone",
                op(PitchOp::Set {
                    notes: notes.clone(),
                    shift: None,
                    drift: None,
                    formant: Some(formant - 1.0),
                }),
            ),
        ];
        if notes.len() > 1 {
            items.push(
                MenuItem::new(
                    "Join Notes",
                    op(PitchOp::Join {
                        notes: notes.clone(),
                    }),
                )
                .separated(),
            );
        }
        items.push(MenuItem::new("Reset", op(PitchOp::Reset { notes })).separated());
        HostRequest::ContextMenu { at, items }
    }

    fn paint_grid(&self, p: &mut dyn Painter, g: Rect, model: &Session, s: Option<&Shown<'_>>) {
        let th = &self.theme;
        let pr = &th.piano;
        p.fill(g, pr.background);
        let first = self.note_at_y(g.bottom(), g).floor() as i32;
        let last = self.note_at_y(g.y, g).ceil() as i32;
        // The key's scale along the clip (as the piano roll shows it).
        let spans: Vec<(f32, f32, faderframe_project::harmony::Key)> = match s {
            Some(s) => {
                let proj = model.project();
                let tl = &proj.timeline;
                let x_at = |t: faderframe_timeline::MusicalTime| {
                    self.x_of((tl.to_samples(t, s.rate) - s.start) as f64 / s.rate, g)
                };
                proj.keys
                    .iter()
                    .enumerate()
                    .map(|(i, k)| {
                        let x0 = x_at(k.at).max(g.x);
                        let x1 = proj
                            .keys
                            .get(i + 1)
                            .map_or(g.right(), |n| x_at(n.at))
                            .min(g.right());
                        (x0, x1, k.key)
                    })
                    .filter(|(x0, x1, _)| x1 > x0)
                    .collect()
            }
            None => Vec::new(),
        };
        for n in first.max(BOTTOM)..=last.min(TOP) {
            let y = self.y_of(n as f32, g) - self.row_h / 2.0;
            let row = Rect::new(g.x, y, g.w, self.row_h);
            p.fill(
                row,
                if is_black(n) {
                    pr.black_row
                } else {
                    pr.white_row
                },
            );
            for (x0, x1, key) in &spans {
                let part = Rect::new(*x0, y, x1 - x0, self.row_h);
                if !key.contains(n) {
                    p.fill(part, pr.off_scale);
                } else if n.rem_euclid(12) == i32::from(key.root % 12) {
                    p.fill(part, pr.root_row);
                }
            }
            if n.rem_euclid(12) == 0 {
                p.hline(g.x, g.right(), y + self.row_h - 0.5, pr.octave_line);
            }
        }
        let Some(s) = s else {
            return;
        };
        // Outside the clip: darker.
        let x0 = self.x_of(0.0, g);
        let x1 = self.x_of(s.seconds(), g);
        let shade = th.ui.background.with_alpha(0.5);
        if x0 > g.x {
            p.fill(Rect::new(g.x, g.y, x0 - g.x, g.h), shade);
        }
        if x1 < g.right() {
            p.fill(Rect::new(x1, g.y, g.right() - x1, g.h), shade);
        }
        // Bar lines where the clip plays.
        let proj = model.project();
        let tl = &proj.timeline;
        let first = tl.meter.bar_at(s.clip.start);
        for bar in (first..).take(10_000) {
            let at = tl.to_samples(tl.meter.bar_start(bar), s.rate);
            let x = self.x_of((at - s.start) as f64 / s.rate, g);
            if x > g.right() {
                break;
            }
            if x >= g.x {
                p.line(
                    Point::new(x, g.y),
                    Point::new(x, g.bottom()),
                    1.0,
                    pr.bar_line,
                );
            }
        }
    }

    fn paint_notes(&self, p: &mut dyn Painter, g: Rect, model: &Session, s: &Shown<'_>) {
        let th = &self.theme;
        let Some(e) = s.edit() else {
            return;
        };
        let color = model
            .project()
            .track(s.clip.track)
            .map_or(th.ui.accent, |t| {
                Color::rgb8(t.color.r, t.color.g, t.color.b)
            });
        p.push_clip(g);
        for (i, n) in e.notes.iter().enumerate() {
            let r = self.note_rect(s, n, g);
            if r.right() < g.x || r.x > g.right() {
                continue;
            }
            let selected = self.selected.contains(&i);
            // Where it was sung, when moved.
            if n.shift.abs() > 0.005 {
                let orig = Rect::new(r.x, self.y_of(n.pitch, g) - r.h / 2.0, r.w, r.h);
                p.stroke_rounded(orig, 4.0, 1.0, th.ui.text_faint.with_alpha(0.6));
            }
            let fill = if selected {
                th.ui.selection.mix(color, 0.3)
            } else if self.hover == Some(i) {
                color.lighten(0.15)
            } else {
                color
            };
            p.fill_rounded(r, 4.0, &Paint::Solid(fill.with_alpha(0.55)));
            p.stroke_rounded(
                r,
                4.0,
                if selected { 2.0 } else { 1.0 },
                if selected {
                    th.ui.accent
                } else {
                    color.darken(0.3)
                },
            );
            // The curve: played, and (faint) sung where it differs.
            let curve = |played: bool| {
                let mut path = Path::new();
                let mut open = false;
                let mut last = f32::NAN;
                let step = (e.hop as usize).max(1);
                for at in (n.start..n.end).step_by(step) {
                    let v = if played {
                        n.played_at(at, e.hop)
                    } else {
                        n.sung_at(at, e.hop)
                    };
                    match v {
                        Some(m) => {
                            let pt = Point::new(self.x_of(s.seconds_of(at), g), self.y_of(m, g));
                            // A jump is not a line.
                            if (m - last).abs() > 1.0 {
                                open = false;
                            }
                            last = m;
                            if open {
                                path.line_to(pt);
                            } else {
                                path.move_to(pt);
                                open = true;
                            }
                        }
                        None => open = false,
                    }
                }
                path
            };
            if n.edited() {
                p.stroke_path(&curve(false), 1.0, th.ui.text_faint.with_alpha(0.7));
            }
            p.stroke_path(&curve(true), 1.5, th.ui.text.with_alpha(0.85));
            if r.w > 34.0 && self.row_h >= 10.0 {
                let heard = n.heard();
                let cents = ((heard - heard.round()) * 100.0).round() as i32;
                let label = if cents == 0 {
                    note_name(heard.round() as i32)
                } else {
                    format!("{} {cents:+}", note_name(heard.round() as i32))
                };
                p.text(
                    &label,
                    Rect::new(r.x + 4.0, r.y - 13.0, r.w.max(60.0), 12.0),
                    &TextStyle::new(th.fonts.small, th.ui.text_dim),
                );
            }
        }
        if let Some(Drag::Band { from, to, .. }) = &self.drag {
            let band = Rect::from_points(*from, *to);
            p.fill(band, th.piano.rubber_band);
            p.stroke_rounded(band, 0.0, 1.0, th.ui.accent.with_alpha(0.7));
        }
        // The playhead.
        let pos = model.transport().position;
        let x = self.x_of((pos - s.start) as f64 / s.rate, g);
        if x >= g.x && x <= g.right() {
            p.line(
                Point::new(x, g.y),
                Point::new(x, g.bottom()),
                1.5,
                th.arranger.playhead,
            );
        }
        p.pop_clip();
    }

    /// The keyboard, drawn as the piano roll's: white keys reaching
    /// halfway under the black ones beside them, the keys of the hovered
    /// and selected notes lit.
    fn paint_keys(&self, p: &mut dyn Painter, g: Rect, s: Option<&Shown<'_>>) {
        let th = &self.theme;
        let pr = &th.piano;
        let rect = Rect::new(0.0, g.y, self.keys_w(), g.h);
        p.fill(rect, pr.key_white_shade);
        p.push_clip(rect);
        let notes = s
            .and_then(Shown::edit)
            .map(|e| e.notes.as_slice())
            .unwrap_or(&[]);
        let heard = |i: usize| notes.get(i).map(|n| n.heard().round() as i32);
        let hover = self.hover.and_then(heard);
        let lit: Vec<i32> = self.selected.iter().filter_map(|i| heard(*i)).collect();
        let half = self.row_h / 2.0;
        let black_w = rect.w * BLACK_KEY_W;
        let first = (self.note_at_y(g.bottom(), g).floor() as i32).max(BOTTOM);
        let last = (self.note_at_y(g.y, g).ceil() as i32).min(TOP);
        let top_of = |n: i32| self.y_of(n as f32, g) - half;
        // White keys first: each reaches halfway under the black keys next
        // to it, where the two white keys meet, as on a real keyboard.
        for n in (first - 1).max(BOTTOM)..=(last + 1).min(TOP) {
            if is_black(n) {
                continue;
            }
            let y = top_of(n);
            let up = n < TOP && is_black(n + 1);
            let down = n > BOTTOM && is_black(n - 1);
            let top_y = if up { y - half } else { y };
            let bottom_y = y + self.row_h + if down { half } else { 0.0 };
            let shape = Rect::new(rect.x, top_y, rect.w, bottom_y - top_y);
            let c = if lit.contains(&n) {
                th.ui.accent.lighten(0.3)
            } else if hover == Some(n) {
                th.ui.selection.lighten(0.5)
            } else {
                pr.key_white
            };
            p.fill_rect(shape, &Paint::horizontal(shape, c.darken(0.06), c));
            p.hline(
                rect.x,
                rect.right(),
                bottom_y - 0.5,
                pr.key_white_shade.darken(0.2),
            );
        }
        for n in first..=last {
            let y = top_of(n);
            if is_black(n) {
                let black = Rect::new(rect.x, y + 1.0, black_w, self.row_h - 2.0);
                let c = if lit.contains(&n) {
                    th.ui.accent
                } else if hover == Some(n) {
                    th.ui.selection.darken(0.3)
                } else {
                    pr.key_black
                };
                p.fill_rounded(black, 1.5, &Paint::horizontal(black, c.lighten(0.15), c));
            }
            if n.rem_euclid(12) == 0 || self.row_h >= 15.0 {
                // A black key's name on the key itself.
                let (area, color) = if is_black(n) {
                    (
                        Rect::new(rect.x, y, black_w - 3.0, self.row_h),
                        pr.key_white.with_alpha(0.75),
                    )
                } else {
                    (Rect::new(rect.x, y, rect.w - 8.0, self.row_h), pr.key_text)
                };
                p.text(
                    &note_name(n),
                    area,
                    &TextStyle::new(th.fonts.tiny, color).right(),
                );
            }
        }
        p.pop_clip();
        p.vline(
            rect.right() - 1.0,
            rect.y,
            rect.bottom(),
            Color::rgba(0.0, 0.0, 0.0, 0.6),
        );
    }

    fn paint_ruler(&self, p: &mut dyn Painter, size: Size, model: &Session, s: Option<&Shown<'_>>) {
        let th = &self.theme;
        let r = Rect::new(0.0, TOOLBAR_H, size.w, RULER_H);
        p.fill(r, th.arranger.ruler_bg);
        p.hline(0.0, size.w, r.bottom() - 0.5, th.ui.border);
        let Some(s) = s else {
            return;
        };
        let g = self.grid(size);
        let tl = &model.project().timeline;
        let first = tl.meter.bar_at(s.clip.start);
        p.push_clip(Rect::new(g.x, r.y, g.w, r.h));
        for bar in (first..).take(10_000) {
            let at = tl.to_samples(tl.meter.bar_start(bar), s.rate);
            let x = self.x_of((at - s.start) as f64 / s.rate, g);
            if x > g.right() {
                break;
            }
            if x + 40.0 >= g.x {
                p.line(
                    Point::new(x, r.bottom() - 6.0),
                    Point::new(x, r.bottom()),
                    1.0,
                    th.arranger.ruler_text,
                );
                p.text(
                    &format!("{}", bar + 1),
                    Rect::new(x + 3.0, r.y, 40.0, r.h),
                    &TextStyle::new(th.fonts.small, th.arranger.ruler_text)
                        .family(FontFamily::Mono),
                );
            }
        }
        p.pop_clip();
    }

    fn paint_toolbar(&self, p: &mut dyn Painter, size: Size, model: &Session) {
        let th = &self.theme;
        let pr = &th.piano;
        let r = Rect::new(0.0, 0.0, size.w, TOOLBAR_H);
        p.fill(r, pr.toolbar);
        p.hline(0.0, size.w, TOOLBAR_H - 0.5, th.ui.border);
        for (item, rect, label, on) in self.toolbar(size, model) {
            if item == Item::Info {
                p.text(
                    &label,
                    rect,
                    &TextStyle::new(th.fonts.small, th.ui.text_dim).right(),
                );
                continue;
            }
            p.fill_rounded(
                rect,
                3.0,
                &Paint::Solid(if on { pr.button_active } else { pr.button }),
            );
            if on {
                p.fill(
                    Rect::new(rect.x + 4.0, rect.bottom() - 2.0, rect.w - 8.0, 2.0),
                    th.ui.accent,
                );
            }
            // Values show how much of the way they are.
            if let Item::Amount | Item::Drift = item {
                let v = if item == Item::Amount {
                    self.amount
                } else {
                    self.drift
                };
                p.fill(
                    Rect::new(rect.x + 3.0, rect.bottom() - 4.0, (rect.w - 6.0) * v, 2.0),
                    th.ui.accent.with_alpha(0.7),
                );
            }
            p.text(
                &label,
                rect,
                &TextStyle::new(th.fonts.small, if on { th.ui.text } else { th.ui.text_dim })
                    .center(),
            );
        }
    }

    fn select_band(&mut self, from: Point, to: Point, add: bool, size: Size, model: &Session) {
        let g = self.grid(size);
        let band = Rect::from_points(from, to);
        if !add {
            self.selected.clear();
        }
        let Some(s) = shown(model) else {
            return;
        };
        let Some(e) = s.edit() else {
            return;
        };
        for (i, n) in e.notes.iter().enumerate() {
            if self.note_rect(&s, n, g).intersects(&band) && !self.selected.contains(&i) {
                self.selected.push(i);
            }
        }
        self.selected.sort_unstable();
    }

    fn pointer_down(
        &mut self,
        pos: Point,
        modifiers: Modifiers,
        clicks: u32,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        if pos.y < TOOLBAR_H {
            let Some(item) = self.item_at(pos, size, model) else {
                return false;
            };
            match item {
                Item::Amount | Item::Drift => {
                    let start = if item == Item::Amount {
                        self.amount
                    } else {
                        self.drift
                    };
                    self.drag = Some(Drag::Value {
                        item,
                        from: pos.x,
                        start,
                    });
                }
                _ => self.click_item(item, model, cx),
            }
            cx.redraw();
            return true;
        }
        let g = self.grid(size);
        if !g.contains(pos) {
            return false;
        }
        cx.request(HostRequest::GrabFocus);
        let Some(s) = shown(model) else {
            return true;
        };
        match self.note_at(pos, size, model) {
            Some(i) if clicks >= 2 => {
                // Split the note here.
                let at = s.source_at(self.seconds_at(pos.x, g));
                cx.emit(Action::EditPitch {
                    clip: s.id,
                    op: PitchOp::Split { note: i, at },
                });
                self.selected.clear();
            }
            Some(i) => {
                if modifiers.toggle() || modifiers.shift {
                    if let Some(k) = self.selected.iter().position(|x| *x == i) {
                        self.selected.remove(k);
                        cx.redraw();
                        return true;
                    }
                    self.selected.push(i);
                    self.selected.sort_unstable();
                } else if !self.selected.contains(&i) {
                    self.selected = vec![i];
                }
                self.drag = Some(Drag::Move {
                    from: pos.y,
                    notes: self.selected.clone(),
                    sent: 0.0,
                    begun: false,
                });
            }
            None => {
                self.drag = Some(Drag::Band {
                    from: pos,
                    to: pos,
                    add: modifiers.shift || modifiers.toggle(),
                });
            }
        }
        cx.redraw();
        true
    }

    fn pointer_move(
        &mut self,
        pos: Point,
        modifiers: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let row_h = self.row_h;
        match &mut self.drag {
            Some(Drag::Move {
                from,
                notes,
                sent,
                begun,
            }) => {
                let Some(s) = shown(model) else {
                    return true;
                };
                let moved = (*from - pos.y) / row_h;
                let by = if modifiers.alt {
                    (moved * 100.0).round() / 100.0
                } else {
                    moved.round()
                };
                if by != *sent {
                    if !*begun {
                        cx.emit(Action::BeginGesture("Move Notes".into()));
                        *begun = true;
                    }
                    cx.emit(Action::EditPitch {
                        clip: s.id,
                        op: PitchOp::Move {
                            notes: notes.clone(),
                            by,
                        },
                    });
                    *sent = by;
                }
                cx.set_cursor(Cursor::ResizeVertical);
                true
            }
            Some(Drag::Band { to, .. }) => {
                *to = pos;
                cx.redraw();
                true
            }
            Some(Drag::Value { item, from, start }) => {
                let v = (*start + (pos.x - *from) / 200.0).clamp(0.0, 1.0);
                let v = (v * 100.0).round() / 100.0;
                if *item == Item::Amount {
                    self.amount = v;
                } else {
                    self.drift = v;
                }
                cx.redraw();
                true
            }
            None => {
                let hover = self.note_at(pos, size, model);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                if hover.is_some() {
                    cx.set_cursor(Cursor::Pointer);
                }
                false
            }
        }
    }

    fn pointer_up(&mut self, size: Size, model: &Session, cx: &mut EventCx<'_, Action>) -> bool {
        match self.drag.take() {
            Some(Drag::Move { begun, .. }) => {
                if begun {
                    cx.emit(Action::EndGesture);
                }
                true
            }
            Some(Drag::Band { from, to, add }) => {
                self.select_band(from, to, add, size, model);
                cx.redraw();
                true
            }
            Some(Drag::Value { .. }) => {
                cx.redraw();
                true
            }
            None => false,
        }
    }

    fn key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(s) = shown(model) else {
            return false;
        };
        let count = s.edit().map_or(0, |e| e.notes.len());
        let op = match key {
            Key::Up | Key::Down if !self.selected.is_empty() => {
                let step = if modifiers.shift { 12.0 } else { 1.0 };
                PitchOp::Move {
                    notes: self.selected.clone(),
                    by: if key == Key::Up { step } else { -step },
                }
            }
            Key::Delete | Key::Backspace if !self.selected.is_empty() => PitchOp::Reset {
                notes: self.selected.clone(),
            },
            Key::Char('a') if modifiers.toggle() => {
                self.selected = (0..count).collect();
                cx.redraw();
                return true;
            }
            Key::Char('j') if self.selected.len() > 1 => {
                let notes = std::mem::take(&mut self.selected);
                PitchOp::Join { notes }
            }
            Key::Escape => {
                self.selected.clear();
                cx.redraw();
                return true;
            }
            _ => return false,
        };
        cx.emit(Action::EditPitch { clip: s.id, op });
        true
    }
}

impl CanvasView<Session, Action> for PitchView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, _theme: &Theme) {
        let s = shown(model);
        let now = s
            .as_ref()
            .map(|s| (s.id, s.edit().map_or(0, |e| e.notes.len())));
        if now != self.seen || size != self.last_size && self.seen.is_none() {
            let new_clip = now.map(|n| n.0) != self.seen.map(|n| n.0);
            let notes_came = now.map(|n| n.1) != self.seen.map(|n| n.1);
            self.selected.retain(|i| now.is_some_and(|(_, c)| *i < c));
            if new_clip {
                self.selected.clear();
            }
            if let Some(s) = &s
                && (new_clip || notes_came && self.seen.is_some_and(|n| n.1 == 0))
            {
                self.fit(size, s);
            }
            self.seen = now;
        }
        self.last_size = size;
        self.clamp(size, model);
        let th = self.theme.clone();
        p.fill(Rect::from_size(size), th.ui.background);
        let g = self.grid(size);
        self.paint_grid(p, g, model, s.as_ref());
        if let Some(s) = &s {
            self.paint_notes(p, g, model, s);
        }
        self.paint_keys(p, g, s.as_ref());
        self.paint_ruler(p, size, model, s.as_ref());
        self.paint_toolbar(p, size, model);
        let hint = match &s {
            None => Some("Right-click an audio clip and choose Edit Pitch".to_string()),
            Some(s) if s.edit().is_none() => Some(if model.detecting_pitch() {
                "Finding the notes…".to_string()
            } else {
                "Detect Pitch finds the notes of this clip".to_string()
            }),
            Some(s) if s.edit().is_some_and(|e| e.notes.is_empty()) => {
                Some("No notes found: pitch editing needs a single voice or instrument".into())
            }
            _ => None,
        };
        if let Some(text) = hint {
            p.text(
                &text,
                Rect::new(g.x, g.y + g.h / 2.0 - 12.0, g.w, 24.0),
                &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
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
                modifiers,
                clicks,
            } => self.pointer_down(pos, modifiers, clicks, size, model, cx),
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                let Some(i) = self.note_at(pos, size, model) else {
                    return false;
                };
                let Some(s) = shown(model) else {
                    return false;
                };
                if !self.selected.contains(&i) {
                    self.selected = vec![i];
                }
                cx.request(self.note_menu(pos, &s, i));
                cx.redraw();
                true
            }
            ViewEvent::PointerMove { pos, modifiers, .. } => {
                self.pointer_move(pos, modifiers, size, model, cx)
            }
            ViewEvent::PointerUp {
                button: PointerButton::Primary,
                ..
            } => self.pointer_up(size, model, cx),
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll {
                pos,
                dx,
                dy,
                modifiers,
                precise,
            } => {
                let g = self.grid(size);
                let step = if precise { 1.0 } else { 40.0 };
                if modifiers.toggle() {
                    // Zoom in time around the pointer.
                    let at = self.seconds_at(pos.x, g);
                    let f = if dy < 0.0 { 1.25 } else { 0.8 };
                    self.px_per_sec = (self.px_per_sec * f).clamp(10.0, 2_000.0);
                    self.scroll_x = at as f32 * self.px_per_sec - (pos.x - g.x);
                } else if modifiers.alt {
                    // Zoom in pitch around the pointer.
                    let at = self.note_at_y(pos.y, g);
                    let f = if dy < 0.0 { 1.15 } else { 1.0 / 1.15 };
                    self.row_h = (self.row_h * f).clamp(MIN_ROW, MAX_ROW);
                    self.scroll_y = (TOP as f32 + 0.5 - at) * self.row_h - (pos.y - g.y);
                } else if modifiers.shift {
                    self.scroll_x += (dy + dx) * step;
                } else {
                    self.scroll_y += dy * step;
                    self.scroll_x += dx * step;
                }
                self.clamp(size, model);
                cx.redraw();
                true
            }
            ViewEvent::Key { key, modifiers } => self.key(key, modifiers, model, cx),
            _ => false,
        }
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.transport().playing || model.detecting_pitch()
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        if pos.y < TOOLBAR_H {
            return Some(
                match self.item_at(pos, size, model)? {
                    Item::Detect => "Find the clip's notes (a single voice or instrument)",
                    Item::Correct => {
                        "Bring the selected notes (or all) to the key's notes and straighten them"
                    }
                    Item::Amount => "How far Correct moves notes to the key — drag",
                    Item::Drift => "How much Correct straightens the pitch inside notes — drag",
                    Item::Formants => {
                        "Kept: a moved voice keeps its character; Move: formants follow the pitch"
                    }
                    Item::Reset => "The selected notes (or all) back to as sung",
                    Item::Remove => "Remove the pitch edit: the clip plays as recorded",
                    Item::Info => return None,
                }
                .into(),
            );
        }
        self.note_at(pos, size, model)?;
        Some(
            "Drag to move (Alt: freely) · double-click to split · ↑↓ move · J join · Delete reset"
                .into(),
        )
    }

    fn min_size(&self) -> Size {
        Size::new(420.0, 200.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        let g = self.grid(size);
        match axis {
            ScrollAxis::Vertical => Some(ScrollInfo {
                content: self.content_h(),
                viewport: g.h,
                offset: self.scroll_y,
                start: g.y,
                end: 0.0,
            }),
            ScrollAxis::Horizontal => {
                let w = shown(model).map_or(0.0, |s| s.seconds() as f32 * self.px_per_sec);
                Some(ScrollInfo {
                    content: w + g.w * 0.5,
                    viewport: g.w,
                    offset: self.scroll_x,
                    start: g.x,
                    end: 0.0,
                })
            }
        }
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        match axis {
            ScrollAxis::Vertical => self.scroll_y = offset.max(0.0),
            ScrollAxis::Horizontal => self.scroll_x = offset.max(0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use faderframe_engine::EngineConfig;
    use faderframe_project::pitch::PitchNote;
    use faderframe_project::{ClipContent, Command};
    use faderframe_ui_canvas::RecordingPainter;

    /// The demo with its Pad clip given three notes (A3, C4, E4).
    fn session() -> (Session, ClipId) {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let pad = s
            .project()
            .tracks
            .iter()
            .find(|t| t.name == "Pad")
            .unwrap()
            .clips[0];
        let c = s.project().clip(pad).unwrap().clone();
        let mut a = c.as_audio().unwrap().clone();
        let off = a.source_offset;
        let note = |k: i64, pitch: f32| PitchNote {
            start: off + k * 24_000,
            end: off + (k + 1) * 24_000 - 2_400,
            pitch,
            shift: 0.0,
            drift: 0.0,
            formant: 0.0,
            curve: vec![0; 90],
        };
        a.pitch = Some(PitchEdit {
            hop: 240,
            notes: vec![note(0, 57.0), note(1, 60.0), note(2, 64.0)],
            keep_formants: true,
        });
        s.dispatch(Action::Edit(Command::SetClipContent {
            clip: pad,
            start: c.start,
            content: Box::new(ClipContent::Audio(a)),
        }))
        .unwrap();
        s.dispatch(Action::OpenPitchEditor(pad)).unwrap();
        (s, pad)
    }

    fn shifts(s: &Session, clip: ClipId) -> Vec<f32> {
        s.project()
            .clip(clip)
            .unwrap()
            .as_audio()
            .unwrap()
            .pitch
            .as_ref()
            .unwrap()
            .notes
            .iter()
            .map(|n| n.shift)
            .collect()
    }

    fn send(
        view: &mut PitchView,
        ev: ViewEvent,
        size: Size,
        s: &mut Session,
    ) -> Vec<HostRequest<Action>> {
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(&ev, size, s, &mut cx);
        for a in actions {
            s.dispatch(a).unwrap();
        }
        requests
    }

    fn down(pos: Point, clicks: u32) -> ViewEvent {
        ViewEvent::PointerDown {
            pos,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks,
        }
    }

    #[test]
    fn notes_are_shown_moved_by_dragging_and_corrected() {
        let (mut s, clip) = session();
        let size = Size::new(1000.0, 600.0);
        let mut view = PitchView::new(Theme::default());
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        let texts = p.texts();
        for t in ["A3", "C4", "E4", "Detect Again", "Correct", "Formants Kept"] {
            assert!(texts.contains(&t), "{t} in {texts:?}");
        }
        // Drag the C4 up two rows: one gesture, two semitones.
        let shown = shown(&s).unwrap();
        let g = view.grid(size);
        let e = shown.edit().unwrap().clone();
        let r = view.note_rect(&shown, &e.notes[1], g);
        let steps = s.history_steps().0.len();
        send(&mut view, down(r.center(), 1), size, &mut s);
        for dy in [8.0, 15.0, 2.0 * view.row_h + 1.0] {
            send(
                &mut view,
                ViewEvent::PointerMove {
                    pos: Point::new(r.center().x, r.center().y - dy),
                    modifiers: Modifiers::NONE,
                    dragging: true,
                },
                size,
                &mut s,
            );
        }
        send(
            &mut view,
            ViewEvent::PointerUp {
                pos: r.center(),
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
            },
            size,
            &mut s,
        );
        assert_eq!(shifts(&s, clip), [0.0, 2.0, 0.0]);
        assert_eq!(s.history_steps().0.len(), steps + 1);
        // Arrow down: one semitone back.
        send(
            &mut view,
            ViewEvent::Key {
                key: Key::Down,
                modifiers: Modifiers::NONE,
            },
            size,
            &mut s,
        );
        assert_eq!(shifts(&s, clip), [0.0, 1.0, 0.0]);
        // Correct (nothing selected after Escape: every note), in A minor
        // the C# goes back to C (the demo is in A minor).
        send(
            &mut view,
            ViewEvent::Key {
                key: Key::Escape,
                modifiers: Modifiers::NONE,
            },
            size,
            &mut s,
        );
        let correct = view
            .toolbar(size, &s)
            .into_iter()
            .find(|(i, ..)| *i == Item::Correct)
            .unwrap()
            .1;
        send(&mut view, down(correct.center(), 1), size, &mut s);
        // C# is as near C as D: a tie goes down.
        assert_eq!(shifts(&s, clip), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn a_double_click_splits_and_the_menu_joins() {
        let (mut s, clip) = session();
        let size = Size::new(1000.0, 600.0);
        let mut view = PitchView::new(Theme::default());
        view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
        let g = view.grid(size);
        let r = {
            let sh = shown(&s).unwrap();
            let e = sh.edit().unwrap().clone();
            view.note_rect(&sh, &e.notes[0], g)
        };
        send(&mut view, down(r.center(), 2), size, &mut s);
        let count = |s: &Session| {
            s.project()
                .clip(clip)
                .unwrap()
                .as_audio()
                .unwrap()
                .pitch
                .as_ref()
                .unwrap()
                .notes
                .len()
        };
        assert_eq!(count(&s), 4);
        // Select both halves with a rubber band and join them (J).
        view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
        send(
            &mut view,
            down(Point::new(r.x + 1.0, r.y - 4.0), 1),
            size,
            &mut s,
        );
        send(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(r.right() + 2.0, r.bottom() + 4.0),
                modifiers: Modifiers::NONE,
                dragging: true,
            },
            size,
            &mut s,
        );
        send(
            &mut view,
            ViewEvent::PointerUp {
                pos: Point::new(r.right() + 2.0, r.bottom() + 4.0),
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
            },
            size,
            &mut s,
        );
        assert_eq!(view.selected, [0, 1]);
        send(
            &mut view,
            ViewEvent::Key {
                key: Key::Char('j'),
                modifiers: Modifiers::NONE,
            },
            size,
            &mut s,
        );
        assert_eq!(count(&s), 3);
        // The note menu offers corrections.
        let reqs = send(
            &mut view,
            ViewEvent::PointerDown {
                pos: r.center(),
                button: PointerButton::Secondary,
                modifiers: Modifiers::NONE,
                clicks: 1,
            },
            size,
            &mut s,
        );
        let Some(HostRequest::ContextMenu { items, .. }) = reqs.into_iter().next() else {
            panic!("a menu");
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Correct to the Key") && labels.contains(&"Straighten"));
    }

    #[test]
    fn without_a_clip_it_says_how_to_get_one() {
        let s = Session::demo(EngineConfig::default()).unwrap();
        let mut view = PitchView::new(Theme::default());
        let mut p = RecordingPainter::new();
        view.paint(&mut p, Size::new(800.0, 400.0), &s, &Theme::default());
        assert!(
            p.texts()
                .contains(&"Right-click an audio clip and choose Edit Pitch")
        );
    }
}
