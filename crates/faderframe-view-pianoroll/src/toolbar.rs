//! The piano roll's toolbar: tools, grid and snap, note length, scale and
//! folding, chords, quantize, default velocity, ghost notes, auditioning,
//! step input, and an inspector for the selection.

use crate::{PianoRollView, Tool, note_name};
use faderframe_project::MidiNote;
use faderframe_project::midi_ops::{ChordKind, PITCH_NAMES, Scale, ScaleKind};
use faderframe_session::{
    Action, KeyFold, NoteLength, NoteOp, PianoRollSettings, Session, StepInput,
};
use faderframe_timeline::{GridDivision, MusicalTime};
use faderframe_ui_canvas::{
    EventCx, Flow, FlowMetrics, HostRequest, MenuItem, Paint, Painter, Point, Rect, TextStyle,
};

/// A toolbar control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Item {
    Tool(Tool),
    Grid,
    Snap,
    Length,
    Scale,
    Fold,
    Chord,
    Quantize,
    QuantizeMenu,
    Velocity,
    Ghosts,
    Audition,
    Step,
    Inspector,
}

/// Note lengths offered for new notes.
pub(crate) const GRIDS: [GridDivision; 16] = [
    GridDivision::Bar,
    GridDivision::Note(2),
    GridDivision::Beat,
    GridDivision::Note(8),
    GridDivision::Note(16),
    GridDivision::Note(32),
    GridDivision::Note(64),
    GridDivision::Note(128),
    GridDivision::Note(256),
    GridDivision::Triplet(4),
    GridDivision::Triplet(8),
    GridDivision::Triplet(16),
    GridDivision::Triplet(32),
    GridDivision::Dotted(4),
    GridDivision::Dotted(8),
    GridDivision::Dotted(16),
];

fn length_label(l: NoteLength) -> String {
    match l {
        NoteLength::Grid => "Grid".into(),
        NoteLength::Last => "Last".into(),
        NoteLength::Fixed(d) => d.label(),
    }
}

impl PianoRollView {
    /// The toolbar's controls with their rectangles, labels and on-state.
    pub(crate) fn toolbar_items(
        &self,
        r: Rect,
        model: &Session,
    ) -> Vec<(Item, Rect, String, bool)> {
        self.toolbar_layout(r, model).0
    }

    /// The controls wrapped into toolbar rows from the top of `r`, and the
    /// number of rows.
    pub(crate) fn toolbar_layout(
        &self,
        r: Rect,
        model: &Session,
    ) -> (Vec<(Item, Rect, String, bool)>, usize) {
        let pr = &model.editor.piano;
        let mut flow: Flow<(Item, String, bool)> = Flow::new();
        let add = |flow: &mut Flow<(Item, String, bool)>, item, label: String, on, w| {
            flow.item((item, label, on), w);
        };
        for t in Tool::ALL {
            add(
                &mut flow,
                Item::Tool(t),
                t.label().into(),
                self.tool == t,
                48.0,
            );
        }
        flow.group();
        add(
            &mut flow,
            Item::Grid,
            format!("Grid {}", model.editor.grid.label()),
            false,
            78.0,
        );
        add(
            &mut flow,
            Item::Snap,
            "Snap".into(),
            model.editor.snap,
            42.0,
        );
        flow.group();
        add(
            &mut flow,
            Item::Length,
            format!("Len {}", length_label(pr.note_length)),
            false,
            74.0,
        );
        add(
            &mut flow,
            Item::Velocity,
            format!("Vel {}", pr.velocity),
            false,
            54.0,
        );
        flow.group();
        let scale = if pr.scale.is_chromatic() {
            "Scale".to_string()
        } else {
            pr.scale.label()
        };
        add(
            &mut flow,
            Item::Scale,
            scale,
            !pr.scale.is_chromatic(),
            112.0,
        );
        add(
            &mut flow,
            Item::Fold,
            "Fold".into(),
            pr.fold != KeyFold::Off,
            40.0,
        );
        add(
            &mut flow,
            Item::Chord,
            if pr.chord == ChordKind::Single {
                "Chord".into()
            } else {
                pr.chord.label().into()
            },
            pr.chord != ChordKind::Single,
            92.0,
        );
        flow.group();
        add(&mut flow, Item::Quantize, "Quantize".into(), false, 64.0);
        add(&mut flow, Item::QuantizeMenu, "▾".into(), false, 18.0);
        flow.group();
        add(
            &mut flow,
            Item::Ghosts,
            "Ghosts".into(),
            pr.ghost_notes,
            52.0,
        );
        add(
            &mut flow,
            Item::Audition,
            "Listen".into(),
            pr.audition,
            50.0,
        );
        add(
            &mut flow,
            Item::Step,
            "Step In".into(),
            model.step_input().is_some(),
            56.0,
        );
        flow.fill((Item::Inspector, self.inspector_text(model), false), 180.0);
        let metrics = FlowMetrics {
            row_height: self.theme.piano.toolbar_height,
            pad_x: 8.0,
            pad_y: 4.0,
        };
        let (placed, rows) = flow.layout(r, metrics);
        (
            placed
                .into_iter()
                .map(|((item, label, on), rect)| (item, rect, label, on))
                .collect(),
            rows,
        )
    }

    fn inspector_text(&self, model: &Session) -> String {
        let Some((_, clip, m)) = Self::clip(model) else {
            return String::new();
        };
        let sel = Self::selected(m, model);
        let tl = &model.project().timeline;
        match sel.as_slice() {
            [] => format!("{} · {} notes", clip.name, m.notes.len()),
            [n] => format!(
                "{} · {} · len {:.3} q · vel {}{}",
                note_name(n.key),
                tl.format_bbt(clip.start + n.start),
                n.length.quarters(),
                n.velocity,
                if n.muted { " · muted" } else { "" }
            ),
            many => {
                let lo = many.iter().map(|n| n.key).min().unwrap_or(0);
                let hi = many.iter().map(|n| n.key).max().unwrap_or(0);
                let vlo = many.iter().map(|n| n.velocity).min().unwrap_or(0);
                let vhi = many.iter().map(|n| n.velocity).max().unwrap_or(0);
                format!(
                    "{} notes · {}–{} · vel {}–{}",
                    many.len(),
                    note_name(lo),
                    note_name(hi),
                    vlo,
                    vhi
                )
            }
        }
    }

    pub(crate) fn paint_toolbar(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let pr = &th.piano;
        p.fill(r, pr.toolbar);
        p.hline(r.x, r.right(), r.bottom() - 0.5, th.ui.border);
        for (item, rect, label, on) in self.toolbar_items(r, model) {
            if item == Item::Inspector {
                p.text(
                    &label,
                    rect,
                    &TextStyle::new(th.fonts.small, th.ui.text_dim).right(),
                );
                continue;
            }
            let bg = if on { pr.button_active } else { pr.button };
            p.fill_rounded(rect, 3.0, &Paint::Solid(bg));
            if on {
                p.fill(
                    Rect::new(rect.x + 4.0, rect.bottom() - 2.0, rect.w - 8.0, 2.0),
                    th.ui.accent,
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

    pub(crate) fn toolbar_item_at(&self, r: Rect, pos: Point, model: &Session) -> Option<Item> {
        self.toolbar_items(r, model)
            .into_iter()
            .find(|(_, rect, _, _)| rect.contains(pos))
            .map(|(i, ..)| i)
    }

    pub(crate) fn toolbar_tooltip(&self, item: Item) -> String {
        match item {
            Item::Tool(t) => format!("{} tool ({})", t.label(), t.key()),
            Item::Grid => "Grid division (Shift while dragging: no snap)".into(),
            Item::Snap => "Snap to the grid".into(),
            Item::Length => "Length of new notes".into(),
            Item::Velocity => "Velocity of new notes".into(),
            Item::Scale => "Key and scale: highlight, snap, fold".into(),
            Item::Fold => "Show all keys, only the scale, or only used keys".into(),
            Item::Chord => "Draw chords instead of single notes".into(),
            Item::Quantize => "Quantize the selection (or all notes) — Q".into(),
            Item::QuantizeMenu => "Quantize and Humanize settings".into(),
            Item::Ghosts => "Show the track's other clips behind".into(),
            Item::Audition => "Hear notes while drawing, moving and on the keys".into(),
            Item::Step => "Step input: play notes on your MIDI keyboard to enter them".into(),
            Item::Inspector => "Selection — click to type a velocity".into(),
        }
    }

    fn settings_menu(
        at: Point,
        entries: Vec<(String, PianoRollSettings, bool, bool)>,
    ) -> HostRequest<Action> {
        HostRequest::ContextMenu {
            at,
            items: entries
                .into_iter()
                .map(|(label, s, checked, sep)| {
                    let item = MenuItem::new(label, Action::SetPianoRoll(s)).checked(checked);
                    if sep { item.separated() } else { item }
                })
                .collect(),
        }
    }

    pub(crate) fn toolbar_press(
        &mut self,
        item: Item,
        rect: Rect,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let pr = model.editor.piano;
        let at = Point::new(rect.x, rect.bottom());
        let clip = Self::clip(model);
        match item {
            Item::Tool(t) => self.tool = t,
            Item::Grid => cx.request(HostRequest::ContextMenu {
                at,
                items: GridDivision::menu(model.editor.grid)
                    .into_iter()
                    .map(|e| {
                        let item =
                            MenuItem::new(e.label, Action::SetGrid(e.division)).checked(e.checked);
                        if e.separated { item.separated() } else { item }
                    })
                    .collect(),
            }),
            Item::Snap => cx.emit(Action::ToggleSnap),
            Item::Length => {
                let mut entries = vec![
                    (
                        "Grid step".to_string(),
                        PianoRollSettings {
                            note_length: NoteLength::Grid,
                            ..pr
                        },
                        pr.note_length == NoteLength::Grid,
                        false,
                    ),
                    (
                        "Last note".to_string(),
                        PianoRollSettings {
                            note_length: NoteLength::Last,
                            ..pr
                        },
                        pr.note_length == NoteLength::Last,
                        false,
                    ),
                ];
                for (i, g) in GRIDS.iter().enumerate() {
                    entries.push((
                        g.label(),
                        PianoRollSettings {
                            note_length: NoteLength::Fixed(*g),
                            ..pr
                        },
                        pr.note_length == NoteLength::Fixed(*g),
                        i == 0 || i == 9 || i == 13,
                    ));
                }
                cx.request(Self::settings_menu(at, entries));
            }
            Item::Velocity => {
                let entries = [32u8, 64, 80, 100, 110, 127]
                    .into_iter()
                    .map(|v| {
                        (
                            format!("Velocity {v}"),
                            PianoRollSettings { velocity: v, ..pr },
                            pr.velocity == v,
                            false,
                        )
                    })
                    .collect();
                cx.request(Self::settings_menu(at, entries));
            }
            Item::Scale => {
                let mut entries = Vec::new();
                for (i, kind) in ScaleKind::ALL.into_iter().enumerate() {
                    entries.push((
                        kind.label().to_string(),
                        PianoRollSettings {
                            scale: Scale::new(pr.scale.root, kind),
                            ..pr
                        },
                        pr.scale.kind == kind,
                        i == 1,
                    ));
                }
                for (i, name) in PITCH_NAMES.iter().enumerate() {
                    entries.push((
                        format!("Root {name}"),
                        PianoRollSettings {
                            scale: Scale::new(i as u8, pr.scale.kind),
                            ..pr
                        },
                        pr.scale.root == i as u8,
                        i == 0,
                    ));
                }
                entries.push((
                    "Snap notes to the scale".into(),
                    PianoRollSettings {
                        scale_snap: !pr.scale_snap,
                        ..pr
                    },
                    pr.scale_snap,
                    true,
                ));
                cx.request(Self::settings_menu(at, entries));
            }
            Item::Fold => {
                let entries = [
                    (KeyFold::Off, "All keys"),
                    (KeyFold::Scale, "Scale keys only"),
                    (KeyFold::Used, "Used keys only"),
                ]
                .into_iter()
                .map(|(f, l)| {
                    (
                        l.to_string(),
                        PianoRollSettings { fold: f, ..pr },
                        pr.fold == f,
                        false,
                    )
                })
                .collect();
                cx.request(Self::settings_menu(at, entries));
            }
            Item::Chord => {
                let entries = ChordKind::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| {
                        (
                            c.label().to_string(),
                            PianoRollSettings { chord: c, ..pr },
                            pr.chord == c,
                            i == 1 || i == 3,
                        )
                    })
                    .collect();
                cx.request(Self::settings_menu(at, entries));
            }
            Item::Quantize => {
                if let Some((clip, _, m)) = clip {
                    let notes: Vec<_> = Self::selected(m, model).iter().map(|n| n.id).collect();
                    cx.emit(Action::NoteOperation {
                        clip,
                        notes,
                        op: NoteOp::Quantize(model.editor.quantize_settings()),
                    });
                }
            }
            Item::QuantizeMenu => {
                let mut items = groove_settings(model);
                if let Some((clip, c, m)) = clip {
                    let notes: Vec<_> = Self::selected(m, model).iter().map(|n| n.id).collect();
                    items.push(
                        MenuItem::new(
                            if notes.is_empty() {
                                "Humanize All Notes"
                            } else {
                                "Humanize Selected Notes"
                            },
                            Action::NoteOperation {
                                clip,
                                notes,
                                op: model.humanize_op(c.start),
                            },
                        )
                        .separated(),
                    );
                }
                cx.request(HostRequest::ContextMenu { at, items });
            }
            Item::Ghosts => cx.emit(Action::SetPianoRoll(PianoRollSettings {
                ghost_notes: !pr.ghost_notes,
                ..pr
            })),
            Item::Audition => cx.emit(Action::SetPianoRoll(PianoRollSettings {
                audition: !pr.audition,
                ..pr
            })),
            Item::Step => match (model.step_input(), clip) {
                (Some(_), _) => cx.emit(Action::SetStepInput(None)),
                (None, Some((clip, c, _))) => {
                    let ph = model.playhead();
                    let cursor = if ph >= c.start {
                        ph - c.start
                    } else {
                        MusicalTime::ZERO
                    };
                    let step = self.new_note_length(model, c.start + cursor);
                    let cursor = self.snap_floor_rel(cursor, c.start, model);
                    cx.emit(Action::SetStepInput(Some(StepInput { clip, cursor, step })));
                }
                _ => {}
            },
            Item::Inspector => {
                if let Some((clip, _, m)) = clip {
                    let sel: Vec<&MidiNote> = Self::selected(m, model);
                    if !sel.is_empty() {
                        let ids: Vec<_> = sel.iter().map(|n| n.id).collect();
                        let initial = sel[0].velocity.to_string();
                        cx.request(HostRequest::TextInput {
                            at: rect,
                            initial,
                            commit: Box::new(move |text| {
                                let v: u8 = text.trim().parse().ok()?;
                                Some(Action::NoteOperation {
                                    clip,
                                    notes: ids.clone(),
                                    op: NoteOp::SetVelocity(v.clamp(1, 127)),
                                })
                            }),
                        });
                    }
                }
            }
        }
        cx.redraw();
    }
}

/// Quantize and Humanize settings as menu items.
pub fn groove_settings(model: &Session) -> Vec<MenuItem<Action>> {
    model
        .groove_settings_menu()
        .into_iter()
        .map(|e| {
            let item = MenuItem::new(e.label, e.action).checked(e.checked);
            if e.separated { item.separated() } else { item }
        })
        .collect()
}
