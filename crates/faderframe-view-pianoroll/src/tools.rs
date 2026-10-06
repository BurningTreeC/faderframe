//! The MIDI Tools panel on the piano roll's right: transformations of the
//! selected notes (all of them without a selection) and generators of new
//! ones (in the bars the selection spans, or the whole clip), each with
//! its settings, previewed in the grid and applied in one undo step.

use crate::PianoRollView;
use faderframe_project::midi_tools::{
    AccentOn, ArpOrder, BassPattern, Direction, DrumStyle, FACTORS, Hats, Lengths, Ornament,
    Recombine, STEPS, Tool as MidiTool, ToolSettings,
};
use faderframe_project::{Clip, MidiClip};
use faderframe_session::{Action, PianoRollSettings, Session};
use faderframe_ui_canvas::{EventCx, Paint, Painter, Point, Rect, TextStyle};

/// The panel's width.
pub(crate) const TOOLS_W: f32 = 280.0;
const PAD: f32 = 10.0;
const SCROLL_GUTTER: f32 = 18.0;
const HEADER_H: f32 = 26.0;
const TAB_H: f32 = 24.0;
const BUTTON_H: f32 = 24.0;
const ROW_H: f32 = 26.0;
const APPLY_H: f32 = 30.0;
/// Pixels of drag per step of a value.
const DRAG_STEP: f32 = 7.0;

/// How a setting shows and moves.
#[derive(Clone, Copy)]
enum Kind {
    Int {
        min: i32,
        max: i32,
        unit: &'static str,
    },
    Choice(&'static [&'static str]),
    /// An index into [`STEPS`] (`touching`: 0 means "Touching").
    Step {
        min: i32,
        max: i32,
        touching: bool,
    },
    Key,
    Toggle,
    Seed,
}

/// One setting of a tool.
#[derive(Clone, Copy)]
struct Row {
    label: &'static str,
    kind: Kind,
    get: fn(&ToolSettings) -> i32,
    set: fn(&mut ToolSettings, i32),
}

const DIRECTIONS: [&str; 3] = ["Up", "Down", "Alternate"];
const ARP_ORDERS: [&str; 4] = ["Up", "Down", "Up-Down", "Random"];
const RECOMBINE: [&str; 5] = [
    "Shuffle Pitches",
    "Rotate Pitches",
    "Reverse Pitches",
    "Shuffle Velocities",
    "Shuffle Lengths",
];
const ACCENT_ON: [&str; 3] = ["Downbeats", "Beats", "Offbeats"];
const ORNAMENTS: [&str; 4] = ["Grace Below", "Grace Above", "Flam", "Mordent"];
const LENGTHS: [&str; 3] = ["Short", "Mixed", "Long"];
const BASS: [&str; 5] = ["Roots", "Root and Fifth", "Octaves", "Walking", "Pulse"];
const DRUMS: [&str; 5] = [
    "Four on the Floor",
    "Backbeat",
    "Breakbeat",
    "Half-Time",
    "Trap",
];
const HATS: [&str; 3] = ["No Hats", "Eighths", "Sixteenths"];
const FACTOR_LABELS: [&str; 6] = [
    "×½ (double time)",
    "×⅔",
    "×¾",
    "×1½",
    "×2 (half time)",
    "×3",
];

fn index_of<T: PartialEq + Copy>(all: &[T], v: T) -> i32 {
    all.iter().position(|x| *x == v).unwrap_or(0) as i32
}

fn pick<T: Copy>(all: &[T], i: i32) -> T {
    all[(i.max(0) as usize).min(all.len() - 1)]
}

fn velocity_row() -> Row {
    Row {
        label: "Velocity",
        kind: Kind::Int {
            min: 1,
            max: 127,
            unit: "",
        },
        get: |s| i32::from(s.velocity),
        set: |s, v| s.velocity = v as u8,
    }
}

fn seed_row() -> Row {
    Row {
        label: "Seed",
        kind: Kind::Seed,
        get: |s| s.seed as i32,
        set: |s, v| s.seed = v.max(1) as u32,
    }
}

/// A tool's settings, in the order the panel shows them.
fn rows(tool: MidiTool, s: &ToolSettings) -> Vec<Row> {
    let mut rows = match tool {
        MidiTool::Strum => vec![
            Row {
                label: "Step",
                kind: Kind::Step {
                    min: 0,
                    max: 5,
                    touching: false,
                },
                get: |s| i32::from(s.strum_step),
                set: |s, v| s.strum_step = v as u8,
            },
            Row {
                label: "Direction",
                kind: Kind::Choice(&DIRECTIONS),
                get: |s| index_of(&Direction::ALL, s.strum_direction),
                set: |s, v| s.strum_direction = pick(&Direction::ALL, v),
            },
        ],
        MidiTool::Chop => vec![
            Row {
                label: "Parts",
                kind: Kind::Int {
                    min: 2,
                    max: 16,
                    unit: "",
                },
                get: |s| i32::from(s.chop_parts),
                set: |s, v| s.chop_parts = v as u8,
            },
            Row {
                label: "Velocity Ramp",
                kind: Kind::Int {
                    min: -30,
                    max: 30,
                    unit: " / part",
                },
                get: |s| i32::from(s.chop_ramp),
                set: |s, v| s.chop_ramp = v as i8,
            },
        ],
        MidiTool::Join => vec![Row {
            label: "Up to a Gap of",
            kind: Kind::Step {
                min: 0,
                max: 9,
                touching: true,
            },
            get: |s| i32::from(s.join_gap),
            set: |s, v| s.join_gap = v as u8,
        }],
        MidiTool::Connect => vec![
            Row {
                label: "Step",
                kind: Kind::Step {
                    min: 3,
                    max: 9,
                    touching: false,
                },
                get: |s| i32::from(s.connect_step),
                set: |s, v| s.connect_step = v as u8,
            },
            Row {
                label: "Velocity",
                kind: Kind::Int {
                    min: 10,
                    max: 100,
                    unit: " %",
                },
                get: |s| i32::from(s.connect_velocity),
                set: |s, v| s.connect_velocity = v as u8,
            },
        ],
        MidiTool::Arpeggiate => {
            let mut v = vec![
                Row {
                    label: "Order",
                    kind: Kind::Choice(&ARP_ORDERS),
                    get: |s| index_of(&ArpOrder::ALL, s.arp_order),
                    set: |s, v| s.arp_order = pick(&ArpOrder::ALL, v),
                },
                Row {
                    label: "Step",
                    kind: Kind::Step {
                        min: 2,
                        max: 9,
                        touching: false,
                    },
                    get: |s| i32::from(s.arp_step),
                    set: |s, v| s.arp_step = v as u8,
                },
                Row {
                    label: "Gate",
                    kind: Kind::Int {
                        min: 10,
                        max: 150,
                        unit: " %",
                    },
                    get: |s| i32::from(s.arp_gate),
                    set: |s, v| s.arp_gate = v as u8,
                },
                Row {
                    label: "Octaves",
                    kind: Kind::Int {
                        min: 1,
                        max: 4,
                        unit: "",
                    },
                    get: |s| i32::from(s.arp_octaves),
                    set: |s, v| s.arp_octaves = v as u8,
                },
            ];
            if s.arp_order == ArpOrder::Random {
                v.push(seed_row());
            }
            v
        }
        MidiTool::Recombine => vec![
            Row {
                label: "What",
                kind: Kind::Choice(&RECOMBINE),
                get: |s| index_of(&Recombine::ALL, s.recombine),
                set: |s, v| s.recombine = pick(&Recombine::ALL, v),
            },
            seed_row(),
        ],
        MidiTool::Conform => vec![Row {
            label: "Notes in the Key Stay",
            kind: Kind::Toggle,
            get: |s| i32::from(s.conform_keep_key),
            set: |s, v| s.conform_keep_key = v != 0,
        }],
        MidiTool::Accent => vec![
            Row {
                label: "On",
                kind: Kind::Choice(&ACCENT_ON),
                get: |s| index_of(&AccentOn::ALL, s.accent_on),
                set: |s, v| s.accent_on = pick(&AccentOn::ALL, v),
            },
            Row {
                label: "Amount",
                kind: Kind::Int {
                    min: 0,
                    max: 60,
                    unit: "",
                },
                get: |s| i32::from(s.accent_amount),
                set: |s, v| s.accent_amount = v as u8,
            },
        ],
        MidiTool::TimeScale => vec![Row {
            label: "Factor",
            kind: Kind::Choice(&FACTOR_LABELS),
            get: |s| i32::from(s.time_factor),
            set: |s, v| s.time_factor = v.clamp(0, FACTORS.len() as i32 - 1) as u8,
        }],
        MidiTool::Ornament => vec![
            Row {
                label: "Kind",
                kind: Kind::Choice(&ORNAMENTS),
                get: |s| index_of(&Ornament::ALL, s.ornament),
                set: |s, v| s.ornament = pick(&Ornament::ALL, v),
            },
            Row {
                label: "Length",
                kind: Kind::Step {
                    min: 0,
                    max: 5,
                    touching: false,
                },
                get: |s| i32::from(s.ornament_length),
                set: |s, v| s.ornament_length = v as u8,
            },
            Row {
                label: "Velocity",
                kind: Kind::Int {
                    min: 10,
                    max: 100,
                    unit: " %",
                },
                get: |s| i32::from(s.ornament_velocity),
                set: |s, v| s.ornament_velocity = v as u8,
            },
        ],
        MidiTool::Euclid => vec![
            Row {
                label: "Hits",
                kind: Kind::Int {
                    min: 1,
                    max: 32,
                    unit: "",
                },
                get: |s| i32::from(s.euclid_hits),
                set: |s, v| s.euclid_hits = v as u8,
            },
            Row {
                label: "Steps",
                kind: Kind::Int {
                    min: 2,
                    max: 32,
                    unit: "",
                },
                get: |s| i32::from(s.euclid_steps),
                set: |s, v| s.euclid_steps = v as u8,
            },
            Row {
                label: "Rotate",
                kind: Kind::Int {
                    min: 0,
                    max: 31,
                    unit: "",
                },
                get: |s| i32::from(s.euclid_rotate),
                set: |s, v| s.euclid_rotate = v as u8,
            },
            Row {
                label: "Step",
                kind: Kind::Step {
                    min: 1,
                    max: 9,
                    touching: false,
                },
                get: |s| i32::from(s.euclid_step),
                set: |s, v| s.euclid_step = v as u8,
            },
            Row {
                label: "Note",
                kind: Kind::Key,
                get: |s| i32::from(s.euclid_key),
                set: |s, v| s.euclid_key = v as u8,
            },
            Row {
                label: "Accent",
                kind: Kind::Int {
                    min: 0,
                    max: 40,
                    unit: "",
                },
                get: |s| i32::from(s.euclid_accent),
                set: |s, v| s.euclid_accent = v as u8,
            },
            velocity_row(),
        ],
        MidiTool::Seed => vec![
            Row {
                label: "Density",
                kind: Kind::Int {
                    min: 5,
                    max: 100,
                    unit: " %",
                },
                get: |s| i32::from(s.seed_density),
                set: |s, v| s.seed_density = v as u8,
            },
            Row {
                label: "Lowest",
                kind: Kind::Key,
                get: |s| i32::from(s.seed_low),
                set: |s, v| s.seed_low = v as u8,
            },
            Row {
                label: "Highest",
                kind: Kind::Key,
                get: |s| i32::from(s.seed_high),
                set: |s, v| s.seed_high = v as u8,
            },
            Row {
                label: "Step",
                kind: Kind::Step {
                    min: 3,
                    max: 10,
                    touching: false,
                },
                get: |s| i32::from(s.seed_step),
                set: |s, v| s.seed_step = v as u8,
            },
            Row {
                label: "Lengths",
                kind: Kind::Choice(&LENGTHS),
                get: |s| index_of(&Lengths::ALL, s.seed_lengths),
                set: |s, v| s.seed_lengths = pick(&Lengths::ALL, v),
            },
            velocity_row(),
            seed_row(),
        ],
        MidiTool::Chords => vec![
            Row {
                label: "Rhythm",
                kind: Kind::Step {
                    min: 5,
                    max: 11,
                    touching: false,
                },
                get: |s| i32::from(s.chords_rhythm),
                set: |s, v| s.chords_rhythm = v as u8,
            },
            Row {
                label: "Around",
                kind: Kind::Key,
                get: |s| i32::from(s.chords_register),
                set: |s, v| s.chords_register = v as u8,
            },
            Row {
                label: "Voice Leading",
                kind: Kind::Toggle,
                get: |s| i32::from(s.chords_lead),
                set: |s, v| s.chords_lead = v != 0,
            },
            velocity_row(),
        ],
        MidiTool::Bassline => vec![
            Row {
                label: "Pattern",
                kind: Kind::Choice(&BASS),
                get: |s| index_of(&BassPattern::ALL, s.bass_pattern),
                set: |s, v| s.bass_pattern = pick(&BassPattern::ALL, v),
            },
            Row {
                label: "Step",
                kind: Kind::Step {
                    min: 3,
                    max: 11,
                    touching: false,
                },
                get: |s| i32::from(s.bass_step),
                set: |s, v| s.bass_step = v as u8,
            },
            Row {
                label: "Octave",
                kind: Kind::Int {
                    min: 0,
                    max: 4,
                    unit: "",
                },
                get: |s| i32::from(s.bass_octave),
                set: |s, v| s.bass_octave = v as u8,
            },
            velocity_row(),
        ],
        MidiTool::Drums => vec![
            Row {
                label: "Style",
                kind: Kind::Choice(&DRUMS),
                get: |s| index_of(&DrumStyle::ALL, s.drum_style),
                set: |s, v| s.drum_style = pick(&DrumStyle::ALL, v),
            },
            Row {
                label: "Hats",
                kind: Kind::Choice(&HATS),
                get: |s| index_of(&Hats::ALL, s.drum_hats),
                set: |s, v| s.drum_hats = pick(&Hats::ALL, v),
            },
            velocity_row(),
        ],
    };
    if tool.is_generator() {
        rows.push(Row {
            label: "Replace the Notes There",
            kind: Kind::Toggle,
            get: |s| i32::from(s.replace),
            set: |s, v| s.replace = v != 0,
        });
    }
    rows
}

impl Kind {
    fn range(self) -> (i32, i32) {
        match self {
            Kind::Int { min, max, .. } | Kind::Step { min, max, .. } => (min, max),
            Kind::Choice(names) => (0, names.len() as i32 - 1),
            Kind::Key => (0, 127),
            Kind::Toggle => (0, 1),
            Kind::Seed => (1, 9_999),
        }
    }

    fn show(self, v: i32) -> String {
        match self {
            Kind::Int { unit, .. } => format!("{v}{unit}").replace('-', "−"),
            Kind::Choice(names) => names[(v.max(0) as usize).min(names.len() - 1)].into(),
            Kind::Step { touching, .. } if touching && v == 0 => "Touching".into(),
            Kind::Step { .. } => STEPS[(v.max(0) as usize).min(STEPS.len() - 1)].0.into(),
            Kind::Key => crate::note_name(v.clamp(0, 127) as u8),
            Kind::Toggle => if v != 0 { "On" } else { "Off" }.into(),
            Kind::Seed => format!("#{v}"),
        }
    }
}

/// A drag on a setting's value.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ValueDrag {
    row: usize,
    origin: Point,
    start: i32,
}

/// What a point on the panel is.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Spot {
    Close,
    Tab(bool),
    Tool(MidiTool),
    Value(usize),
    Apply,
    Nothing,
}

/// Lines of `text` that fit `width` (an estimate from the font size).
fn wrap(text: &str, width: f32, size: f32) -> Vec<String> {
    let per = ((width / (size * 0.52)).floor() as usize).max(8);
    let mut lines = Vec::new();
    let mut line = String::new();
    for w in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + w.chars().count() > per {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(w);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

pub(crate) struct Geometry {
    pub close: Rect,
    pub tabs: [Rect; 2],
    pub tools: Vec<(MidiTool, Rect)>,
    pub about: Rect,
    pub rows: Vec<Rect>,
    pub summary: Rect,
    pub apply: Rect,
}

impl PianoRollView {
    pub(crate) fn tools_geometry(&self, r: Rect, model: &Session) -> Option<Geometry> {
        let pr = &model.editor.piano;
        let tool = pr.tool?;
        // A gutter on the right for the view's scroll bar.
        let x = r.x + PAD;
        let w = r.w - PAD - SCROLL_GUTTER;
        let mut y = r.y + 4.0;
        let close = Rect::new(x + w - 22.0, y + 2.0, 22.0, 22.0);
        y += HEADER_H;
        let half = (w - 4.0) / 2.0;
        let tabs = [
            Rect::new(x, y, half, TAB_H),
            Rect::new(x + half + 4.0, y, half, TAB_H),
        ];
        y += TAB_H + 8.0;
        let list: &[MidiTool] = if tool.is_generator() {
            &MidiTool::GENERATORS
        } else {
            &MidiTool::TRANSFORMS
        };
        let mut tools = Vec::new();
        for (i, t) in list.iter().enumerate() {
            let col = (i % 2) as f32;
            let row = (i / 2) as f32;
            tools.push((
                *t,
                Rect::new(
                    x + col * (half + 4.0),
                    y + row * (BUTTON_H + 4.0),
                    half,
                    BUTTON_H,
                ),
            ));
        }
        y += list.len().div_ceil(2) as f32 * (BUTTON_H + 4.0) + 6.0;
        let about = Rect::new(x, y, w, 46.0);
        y += about.h + 4.0;
        let rows = rows(tool, &pr.tools)
            .iter()
            .enumerate()
            .map(|(i, _)| Rect::new(x, y + i as f32 * ROW_H, w, ROW_H - 4.0))
            .collect();
        let apply = Rect::new(x, r.bottom() - PAD - APPLY_H, w, APPLY_H);
        let summary = Rect::new(x, apply.y - 20.0, w, 16.0);
        Some(Geometry {
            close,
            tabs,
            tools,
            about,
            rows,
            summary,
            apply,
        })
    }

    fn tools_spot(&self, r: Rect, pos: Point, model: &Session) -> Spot {
        let Some(g) = self.tools_geometry(r, model) else {
            return Spot::Nothing;
        };
        if g.close.contains(pos) {
            return Spot::Close;
        }
        if let Some(i) = g.tabs.iter().position(|t| t.contains(pos)) {
            return Spot::Tab(i == 1);
        }
        if let Some((t, _)) = g.tools.iter().find(|(_, r)| r.contains(pos)) {
            return Spot::Tool(*t);
        }
        if let Some(i) = g.rows.iter().position(|r| r.contains(pos)) {
            return Spot::Value(i);
        }
        if g.apply.contains(pos) {
            return Spot::Apply;
        }
        Spot::Nothing
    }

    fn set_tools(cx: &mut EventCx<'_, Action>, pr: PianoRollSettings) {
        cx.emit(Action::SetPianoRoll(pr));
        cx.redraw();
    }

    /// Apply the tool to the open clip's selection (or all of it).
    pub(crate) fn apply_tool(&self, model: &Session, cx: &mut EventCx<'_, Action>) {
        if let Some((clip, _, m)) = Self::clip(model) {
            cx.emit(Action::ApplyMidiTool {
                clip,
                notes: Self::selection_ids(m, model),
            });
        }
    }

    /// A press on the panel.
    pub(crate) fn tools_press(
        &mut self,
        r: Rect,
        pos: Point,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let mut pr = model.editor.piano;
        let Some(tool) = pr.tool else { return false };
        match self.tools_spot(r, pos, model) {
            Spot::Close => {
                self.last_midi_tool = tool;
                pr.tool = None;
                Self::set_tools(cx, pr);
            }
            Spot::Tab(generate) => {
                if generate != tool.is_generator() {
                    pr.tool = Some(if generate {
                        self.last_generator
                    } else {
                        self.last_transform
                    });
                    Self::set_tools(cx, pr);
                }
            }
            Spot::Tool(t) => {
                if t.is_generator() {
                    self.last_generator = t;
                } else {
                    self.last_transform = t;
                }
                pr.tool = Some(t);
                Self::set_tools(cx, pr);
            }
            Spot::Value(i) => {
                let all = rows(tool, &pr.tools);
                let row = all[i];
                let v = (row.get)(&pr.tools);
                match row.kind {
                    // Clicks cycle choices and switches, and draw a seed.
                    Kind::Choice(_) | Kind::Toggle => {
                        let (lo, hi) = row.kind.range();
                        (row.set)(&mut pr.tools, if v >= hi { lo } else { v + 1 });
                        Self::set_tools(cx, pr);
                    }
                    Kind::Seed => {
                        (row.set)(&mut pr.tools, (v * 7919 + 13) % 9_999 + 1);
                        Self::set_tools(cx, pr);
                    }
                    _ => {
                        self.value_drag = Some(ValueDrag {
                            row: i,
                            origin: pos,
                            start: v,
                        });
                    }
                }
            }
            Spot::Apply => self.apply_tool(model, cx),
            Spot::Nothing => {}
        }
        true
    }

    /// A drag on a value: up (or right) for more.
    pub(crate) fn tools_drag(
        &mut self,
        pos: Point,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(d) = self.value_drag else {
            return false;
        };
        let mut pr = model.editor.piano;
        let Some(tool) = pr.tool else { return false };
        let all = rows(tool, &pr.tools);
        let Some(row) = all.get(d.row) else {
            return false;
        };
        let moved = ((d.origin.y - pos.y) + (pos.x - d.origin.x)) / DRAG_STEP;
        let (lo, hi) = row.kind.range();
        let v = (d.start + moved.round() as i32).clamp(lo, hi);
        if v != (row.get)(&pr.tools) {
            (row.set)(&mut pr.tools, v);
            Self::set_tools(cx, pr);
        }
        true
    }

    /// The wheel over a value: a step a notch.
    pub(crate) fn tools_scroll(
        &mut self,
        r: Rect,
        pos: Point,
        dy: f32,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let mut pr = model.editor.piano;
        let Some(tool) = pr.tool else { return false };
        if let Spot::Value(i) = self.tools_spot(r, pos, model) {
            let row = rows(tool, &pr.tools)[i];
            let (lo, hi) = row.kind.range();
            let v = ((row.get)(&pr.tools) + if dy < 0.0 { 1 } else { -1 }).clamp(lo, hi);
            (row.set)(&mut pr.tools, v);
            Self::set_tools(cx, pr);
        }
        true
    }

    pub(crate) fn paint_tools(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let Some(g) = self.tools_geometry(r, model) else {
            return;
        };
        let pr = &model.editor.piano;
        let Some(tool) = pr.tool else { return };
        p.fill(r, th.ui.surface);
        p.vline(r.x + 0.5, r.y, r.bottom(), th.ui.border);
        let title = TextStyle::new(th.fonts.small, th.ui.text_dim)
            .bold()
            .tracking(0.8);
        p.text(
            "MIDI TOOLS",
            Rect::new(r.x + PAD, r.y + 4.0, 160.0, HEADER_H),
            &title,
        );
        p.text(
            "×",
            g.close,
            &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
        );
        let button = |p: &mut dyn Painter, r: Rect, label: &str, on: bool| {
            let bg = if on {
                th.ui.accent.with_alpha(0.32)
            } else {
                th.ui.surface_alt
            };
            p.fill_rounded(r, 4.0, &Paint::Solid(bg));
            p.stroke_rounded(
                r,
                4.0,
                1.0,
                if on {
                    th.ui.accent.with_alpha(0.8)
                } else {
                    th.ui.border
                },
            );
            p.text(
                label,
                r,
                &TextStyle::new(th.fonts.small, th.ui.text).center(),
            );
        };
        button(p, g.tabs[0], "Transform", !tool.is_generator());
        button(p, g.tabs[1], "Generate", tool.is_generator());
        for (t, rect) in &g.tools {
            button(p, *rect, short_label(*t), *t == tool);
        }
        let about = TextStyle::new(th.fonts.small, th.ui.text_dim);
        for (i, line) in wrap(tool.about(), g.about.w, th.fonts.small)
            .iter()
            .take(3)
            .enumerate()
        {
            p.text(
                line,
                Rect::new(g.about.x, g.about.y + i as f32 * 15.0, g.about.w, 15.0),
                &about,
            );
        }
        let all = rows(tool, &pr.tools);
        for (row, rect) in all.iter().zip(&g.rows) {
            p.text(
                row.label,
                Rect::new(rect.x, rect.y, rect.w * 0.5, rect.h),
                &TextStyle::new(th.fonts.small, th.ui.text),
            );
            let value = Rect::new(rect.x + rect.w * 0.5, rect.y, rect.w * 0.5, rect.h);
            p.fill_rounded(value, 3.0, &Paint::Solid(th.ui.lcd_bg));
            let v = (row.get)(&pr.tools);
            let on = matches!(row.kind, Kind::Toggle) && v != 0;
            p.text(
                &row.kind.show(v),
                value,
                &TextStyle::new(
                    th.fonts.small,
                    if on { th.ui.accent } else { th.ui.lcd_text },
                )
                .bold()
                .center(),
            );
        }
        // What applying would do.
        let summary = self.tool_summary(model);
        p.text(
            &summary,
            g.summary,
            &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
        );
        p.fill_rounded(g.apply, 5.0, &Paint::Solid(th.ui.accent.with_alpha(0.85)));
        p.text(
            &format!("Apply {}  (Enter)", tool.label()),
            g.apply,
            &TextStyle::new(th.fonts.small, th.ui.background)
                .bold()
                .center(),
        );
    }

    /// "12 notes become 48", "adds 32 notes in bars 1–4".
    fn tool_summary(&self, model: &Session) -> String {
        let Some((clip, c, m)) = Self::clip(model) else {
            return String::new();
        };
        let ids = Self::selection_ids(m, model);
        let Some(p) = model.preview_midi_tool(clip, &ids) else {
            return "Nothing to work on".into();
        };
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        match p.range {
            Some((a, b)) => {
                let meter = &model.project().timeline.meter;
                let first = meter.bar_at(c.start + a) + 1;
                let last = meter.bar_at(c.start + b - faderframe_timeline::MusicalTime(1)) + 1;
                let bars = if first == last {
                    format!("bar {first}")
                } else {
                    format!("bars {first}–{last}")
                };
                let replaced = if p.removed.is_empty() {
                    String::new()
                } else {
                    format!(", replacing {}", p.removed.len())
                };
                if p.notes.is_empty() {
                    let why = if p.tool == MidiTool::Chords && model.project().chords.is_empty() {
                        ": the chord track is empty"
                    } else {
                        ""
                    };
                    format!("Nothing in {bars}{why}")
                } else {
                    format!(
                        "Adds {} note{} in {bars}{replaced}",
                        p.notes.len(),
                        plural(p.notes.len())
                    )
                }
            }
            None => {
                let what = if ids.is_empty() { "All" } else { "Selected:" };
                format!(
                    "{what} {} note{} → {}",
                    p.removed.len(),
                    plural(p.removed.len()),
                    p.notes.len()
                )
            }
        }
    }

    /// The tool's result over the grid: the notes it takes away dimmed,
    /// what it leaves or makes outlined.
    pub(crate) fn paint_tool_preview(
        &self,
        p: &mut dyn Painter,
        g: Rect,
        _clip: &Clip,
        m: &MidiClip,
        model: &Session,
    ) {
        let Some((clip, _, _)) = Self::clip(model) else {
            return;
        };
        let ids = Self::selection_ids(m, model);
        let Some(preview) = model.preview_midi_tool(clip, &ids) else {
            return;
        };
        let th = &self.theme;
        let accent = th.ui.accent;
        if let Some((a, b)) = preview.range {
            let x0 = self.x_of(a).max(g.x);
            let x1 = self.x_of(b).min(g.right());
            if x1 > x0 {
                p.fill(Rect::new(x0, g.y, x1 - x0, g.h), accent.with_alpha(0.05));
            }
        }
        for n in m.notes.iter().filter(|n| preview.removed.contains(&n.id)) {
            if let Some(r) = self.note_rect(n) {
                p.fill_rounded(r, 2.0, &Paint::Solid(th.piano.background.with_alpha(0.6)));
            }
        }
        for n in &preview.notes {
            let Some(r) = self.note_rect(n) else { continue };
            if r.right() < g.x || r.x > g.right() {
                continue;
            }
            p.fill_rounded(r, 2.0, &Paint::Solid(accent.with_alpha(0.22)));
            p.stroke_rounded(r, 2.0, 1.5, accent.with_alpha(0.95));
        }
    }
}

/// A tool's name on its button.
fn short_label(t: MidiTool) -> &'static str {
    match t {
        MidiTool::Conform => "Conform",
        MidiTool::Euclid => "Euclidean",
        MidiTool::Seed => "Seed",
        MidiTool::Drums => "Drums",
        t => t.label(),
    }
}
