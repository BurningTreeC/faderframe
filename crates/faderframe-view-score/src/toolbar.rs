//! The score's palette: note input, the value notes are entered as (dot,
//! triplet), rest and tie, accidentals, the voice, dynamics and
//! articulations, words and chord symbols, the clip's display quantise,
//! pages or a strip, step input from a MIDI keyboard, and export. Laid out
//! in rows that wrap with the view's width.

use faderframe_session::Session;
use faderframe_session::score::{ScoreLayout, Value};
use faderframe_ui_canvas::{Paint, Painter, Rect, TextStyle, Theme};

/// A row's height.
pub const ROW_H: f32 = 30.0;
const PAD: f32 = 4.0;
const GROUP_GAP: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    /// Note input on or off.
    Entering,
    Value(Value),
    Dot,
    Triplet,
    Rest,
    Tie,
    /// ♭ (−1), ♮ (0), ♯ (+1) on the selected notes.
    Accidental(i8),
    Voice(u8),
    /// Dynamics for the selection (a menu).
    Dynamics,
    /// Articulations for the selection (a menu).
    Marks,
    /// Words from the selected note on.
    Lyrics,
    /// A chord symbol at the cursor.
    Chord,
    /// The clips' display quantise (a menu).
    Display,
    /// Pages or a strip.
    Layout,
    /// Step input from a MIDI keyboard.
    Step,
    Pdf,
    MusicXml,
}

/// The palette's groups, in order.
const GROUPS: [&[Item]; 9] = [
    &[Item::Entering],
    &[
        Item::Value(Value::Whole),
        Item::Value(Value::Half),
        Item::Value(Value::Quarter),
        Item::Value(Value::Eighth),
        Item::Value(Value::Sixteenth),
        Item::Dot,
        Item::Triplet,
    ],
    &[Item::Rest, Item::Tie],
    &[
        Item::Accidental(-1),
        Item::Accidental(0),
        Item::Accidental(1),
    ],
    &[Item::Voice(1), Item::Voice(2)],
    &[Item::Dynamics, Item::Marks],
    &[Item::Lyrics, Item::Chord],
    &[Item::Display, Item::Layout, Item::Step],
    &[Item::Pdf, Item::MusicXml],
];

impl Item {
    /// What it says (`layout`: the score's now).
    pub fn label(self, layout: ScoreLayout) -> &'static str {
        match self {
            Item::Entering => "✎ Note Input",
            Item::Value(Value::Whole) => "1",
            Item::Value(Value::Half) => "1/2",
            Item::Value(Value::Quarter) => "1/4",
            Item::Value(Value::Eighth) => "1/8",
            Item::Value(Value::Sixteenth) => "1/16",
            Item::Dot => "Dot",
            Item::Triplet => "3",
            Item::Rest => "Rest",
            Item::Tie => "Tie",
            Item::Accidental(-1) => "♭",
            Item::Accidental(0) => "♮",
            Item::Accidental(_) => "♯",
            Item::Voice(1) => "V1",
            Item::Voice(_) => "V2",
            Item::Dynamics => "Dynamics ▾",
            Item::Marks => "Articulation ▾",
            Item::Lyrics => "Lyrics…",
            Item::Chord => "Chord…",
            Item::Display => "Display ▾",
            Item::Layout => match layout {
                ScoreLayout::Pages => "Pages",
                ScoreLayout::Strip => "Strip",
            },
            Item::Step => "Step Input",
            Item::Pdf => "PDF…",
            Item::MusicXml => "MusicXML…",
        }
    }

    /// Its tooltip (with its key).
    pub fn tip(self) -> &'static str {
        match self {
            Item::Entering => "Note input (N): letters A–G and clicks on the staff enter notes",
            Item::Value(Value::Whole) => "Whole note (7)",
            Item::Value(Value::Half) => "Half note (6)",
            Item::Value(Value::Quarter) => "Quarter note (5)",
            Item::Value(Value::Eighth) => "Eighth note (4)",
            Item::Value(Value::Sixteenth) => "Sixteenth note (3)",
            Item::Dot => "Dotted (.)",
            Item::Triplet => "Triplet (T)",
            Item::Rest => "Rest at the cursor (0)",
            Item::Tie => "Hold the note on (+)",
            Item::Accidental(-1) => "Flat on the selected notes' letters",
            Item::Accidental(0) => "Natural on the selected notes' letters",
            Item::Accidental(_) => "Sharp on the selected notes' letters",
            Item::Voice(1) => "Voice 1, stems up (Alt+1)",
            Item::Voice(_) => "Voice 2, stems down (Alt+2)",
            Item::Dynamics => {
                "A dynamic on the selected note: its velocity and the notes' after it"
            }
            Item::Marks => "Staccato, accent, tenuto, marcato: played as length and velocity",
            Item::Lyrics => "Words from the selected note on, a syllable a note (Ctrl+L)",
            Item::Chord => "A chord symbol at the cursor, on the chord track (Ctrl+K)",
            Item::Display => {
                "How the clips' rhythms are written (the MIDI keeps its timing), and quantising to it"
            }
            Item::Layout => "Pages as printed, or one long strip",
            Item::Step => "Step input: play notes on a MIDI keyboard to enter them at the cursor",
            Item::Pdf => "Export the score as PDF",
            Item::MusicXml => "Export the score as MusicXML",
        }
    }

    fn width(self) -> f32 {
        match self {
            Item::Entering => 104.0,
            Item::Value(_) | Item::Triplet | Item::Accidental(_) | Item::Voice(_) => 36.0,
            Item::Dot | Item::Rest | Item::Tie => 44.0,
            Item::Dynamics => 92.0,
            Item::Marks => 104.0,
            Item::Lyrics | Item::Chord => 66.0,
            Item::Display => 80.0,
            Item::Layout => 56.0,
            Item::Step => 80.0,
            Item::Pdf => 52.0,
            Item::MusicXml => 88.0,
        }
    }

    /// Lit now.
    fn on(self, model: &Session) -> bool {
        let s = model.score_settings();
        match self {
            Item::Entering => s.entering,
            Item::Value(v) => s.value == v,
            Item::Dot => s.dots > 0,
            Item::Triplet => s.triplet,
            Item::Voice(v) => s.voice == v,
            Item::Step => model.step_input().is_some(),
            _ => false,
        }
    }
}

/// The items and where they are in a view `width` wide, and the palette's
/// height.
pub fn layout(width: f32) -> (Vec<(Item, Rect)>, f32) {
    let mut out = Vec::new();
    let (mut x, mut y) = (8.0f32, PAD);
    for group in GROUPS {
        let w: f32 = group.iter().map(|i| i.width() + 2.0).sum();
        if x > 8.0 && x + w > width - 8.0 {
            x = 8.0;
            y += ROW_H;
        }
        for item in group {
            out.push((*item, Rect::new(x, y, item.width(), ROW_H - 2.0 * PAD)));
            x += item.width() + 2.0;
        }
        x += GROUP_GAP;
    }
    (out, y + ROW_H)
}

pub fn paint(p: &mut dyn Painter, width: f32, model: &Session, th: &Theme) {
    let (items, h) = layout(width);
    p.fill(Rect::new(0.0, 0.0, width, h), th.ui.surface);
    p.hline(0.0, width, h - 0.5, th.ui.border);
    let layout = model.score_settings().layout;
    let has_doc = model.score().is_some();
    for (item, r) in items {
        let on = item.on(model);
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
        let enabled = has_doc || matches!(item, Item::Layout | Item::Value(_));
        p.text(
            item.label(layout),
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
