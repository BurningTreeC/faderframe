//! The edit toolbar: edit modes, tools, grid and nudge values, editing
//! options, the selection counters and zoom — shown full width under the
//! transport (toggled there).

use faderframe_session::{
    Action, CounterUnit, EditFlag, EditMode, EditTool, GridMode, NudgeValue, Session, ZoomRequest,
};
use faderframe_timeline::{GridDivision, MusicalTime, format_seconds};
use faderframe_ui_canvas::{
    CanvasView, EventCx, Flow, FlowMetrics, HostRequest, MenuItem, Paint, Painter, Point,
    PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};

/// One control of the toolbar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Item {
    Mode(EditMode),
    GridModeMenu,
    Tool(EditTool),
    TrimMenu,
    GrabMenu,
    GridValue,
    Triplet,
    Dotted,
    Nudge,
    /// Quantize / Humanize of the selected clips and their settings.
    Groove,
    Flag(EditFlag),
    Sensitivity,
    Counter(usize),
    Zoom(ZoomRequest),
}

pub struct EditToolbarView {
    theme: Theme,
}

/// The grid value menu (shared by the arranger).
pub fn grid_menu(current: GridDivision) -> Vec<MenuItem<Action>> {
    GridDivision::menu(current)
        .into_iter()
        .map(|e| {
            let item = MenuItem::new(e.label, Action::SetGrid(e.division)).checked(e.checked);
            if e.separated { item.separated() } else { item }
        })
        .collect()
}

fn grid_value_label(g: GridDivision) -> String {
    match g {
        GridDivision::Bar => "Bar".into(),
        GridDivision::Beat => "Beat".into(),
        GridDivision::Note(n) | GridDivision::Triplet(n) | GridDivision::Dotted(n) => {
            format!("1/{n}")
        }
    }
}

impl EditToolbarView {
    pub fn new(theme: Theme) -> Self {
        Self { theme }
    }

    /// One row's height.
    pub const ROW: f32 = 36.0;

    const METRICS: FlowMetrics = FlowMetrics {
        row_height: Self::ROW,
        pad_x: 10.0,
        pad_y: 5.0,
    };

    /// The height the toolbar needs at `width` (it wraps into rows).
    pub fn preferred_height(&self, width: f32, s: &Session) -> f32 {
        let (_, rows) = self.layout(width, s);
        rows as f32 * Self::ROW
    }

    /// Controls with their rectangles, labels and on-state.
    fn items(&self, size: Size, s: &Session) -> Vec<(Item, Rect, String, bool)> {
        self.layout(size.w, s).0
    }

    fn layout(&self, width: f32, s: &Session) -> (Vec<(Item, Rect, String, bool)>, usize) {
        let e = &s.editor;
        let mut flow: Flow<(Item, String, bool)> = Flow::new();
        let add = |flow: &mut Flow<(Item, String, bool)>, item, label: String, on, w| {
            flow.item((item, label, on), w);
        };
        for m in EditMode::ALL {
            let w = if m == EditMode::Grid { 46.0 } else { 54.0 };
            add(
                &mut flow,
                Item::Mode(m),
                m.label().into(),
                e.edit_mode == m,
                w,
            );
        }
        add(
            &mut flow,
            Item::GridModeMenu,
            match e.grid_mode {
                GridMode::Absolute => "Abs ▾".into(),
                GridMode::Relative => "Rel ▾".into(),
            },
            false,
            42.0,
        );
        flow.group();
        let is = |t: &[EditTool]| t.contains(&e.tool);
        add(
            &mut flow,
            Item::Tool(EditTool::Smart),
            "Smart".into(),
            is(&[EditTool::Smart]),
            50.0,
        );
        add(
            &mut flow,
            Item::Tool(EditTool::Zoom),
            "Zoom".into(),
            is(&[EditTool::Zoom]),
            46.0,
        );
        let stretch = e.tool == EditTool::TrimStretch;
        add(
            &mut flow,
            Item::Tool(if stretch {
                EditTool::TrimStretch
            } else {
                EditTool::Trim
            }),
            if stretch {
                "Stretch".into()
            } else {
                "Trim".into()
            },
            is(&[EditTool::Trim, EditTool::TrimStretch]),
            52.0,
        );
        add(&mut flow, Item::TrimMenu, "▾".into(), false, 16.0);
        add(
            &mut flow,
            Item::Tool(EditTool::Select),
            "Select".into(),
            is(&[EditTool::Select]),
            50.0,
        );
        let separation = e.tool == EditTool::GrabSeparation;
        add(
            &mut flow,
            Item::Tool(if separation {
                EditTool::GrabSeparation
            } else {
                EditTool::Grab
            }),
            if separation {
                "Separate".into()
            } else {
                "Grab".into()
            },
            is(&[EditTool::Grab, EditTool::GrabSeparation]),
            62.0,
        );
        add(&mut flow, Item::GrabMenu, "▾".into(), false, 16.0);
        add(
            &mut flow,
            Item::Tool(EditTool::Scrub),
            "Scrub".into(),
            is(&[EditTool::Scrub]),
            46.0,
        );
        add(
            &mut flow,
            Item::Tool(EditTool::Pencil),
            "Pencil".into(),
            is(&[EditTool::Pencil]),
            50.0,
        );
        flow.group();
        add(
            &mut flow,
            Item::GridValue,
            format!("Grid {}", grid_value_label(e.grid)),
            false,
            78.0,
        );
        add(
            &mut flow,
            Item::Triplet,
            "T".into(),
            matches!(e.grid, GridDivision::Triplet(_)),
            22.0,
        );
        add(
            &mut flow,
            Item::Dotted,
            "D".into(),
            matches!(e.grid, GridDivision::Dotted(_)),
            22.0,
        );
        add(
            &mut flow,
            Item::Nudge,
            format!("Nudge {}", e.nudge.label()),
            false,
            92.0,
        );
        add(&mut flow, Item::Groove, "Quantize ▾".into(), false, 74.0);
        flow.group();
        for (flag, label, on, w) in [
            (
                EditFlag::TabToTransients,
                "Tab→Trans",
                e.tab_to_transients,
                74.0,
            ),
            (EditFlag::LinkTimeline, "Link", e.link_timeline, 40.0),
            (
                EditFlag::InsertionFollowsPlayback,
                "Ins Follows",
                e.insertion_follows_playback,
                74.0,
            ),
            (EditFlag::FollowPlayhead, "Follow", e.follow_playhead, 52.0),
        ] {
            add(&mut flow, Item::Flag(flag), label.into(), on, w);
        }
        flow.group();
        add(
            &mut flow,
            Item::Flag(EditFlag::ShowTransients),
            "Transients".into(),
            e.show_transients,
            74.0,
        );
        add(
            &mut flow,
            Item::Sensitivity,
            format!("Sens {:.0}%", e.transient_sensitivity * 100.0),
            false,
            62.0,
        );
        add(
            &mut flow,
            Item::Flag(EditFlag::Warp),
            "Warp".into(),
            e.warp,
            44.0,
        );
        flow.group();
        // Counters: start, end, length of the edit selection.
        let (a, b) = s
            .selection
            .range
            .map_or((s.playhead(), s.playhead()), |r| (r.start, r.end));
        for (i, (name, t)) in [("Start", a), ("End", b), ("Length", b - a)]
            .into_iter()
            .enumerate()
        {
            let text = format!("{name} {}", self.format_time(s, t, i == 2, a));
            add(&mut flow, Item::Counter(i), text, false, 118.0);
        }
        flow.group();
        for (z, label, w) in [
            (ZoomRequest::Out, "−", 26.0),
            (ZoomRequest::In, "+", 26.0),
            (ZoomRequest::Selection, "Sel", 34.0),
            (ZoomRequest::Fit, "Fit", 32.0),
        ] {
            add(&mut flow, Item::Zoom(z), label.into(), false, w);
        }
        let (placed, rows) = flow.layout(Rect::new(0.0, 0.0, width, Self::ROW), Self::METRICS);
        (
            placed
                .into_iter()
                .map(|((item, label, on), r)| (item, r, label, on))
                .collect(),
            rows,
        )
    }

    /// A position (or, for `length`, a duration starting at `from`) in the
    /// counter unit.
    fn format_time(&self, s: &Session, t: MusicalTime, length: bool, from: MusicalTime) -> String {
        let p = s.project();
        let rate = p.sample_rate as f64;
        match s.editor.counter_unit {
            CounterUnit::BarsBeats if length => {
                let q = t.quarters();
                let sig = p
                    .timeline
                    .meter
                    .signature_of_bar(p.timeline.meter.bar_at(from));
                let beats_per_bar = sig.numerator as f64 * 4.0 / sig.denominator as f64;
                let bars = (q / beats_per_bar).floor();
                let beats = q - bars * beats_per_bar;
                format!("{}|{:.3}", bars as i64, beats)
            }
            CounterUnit::BarsBeats => p.timeline.format_bbt(t),
            CounterUnit::MinSecs => {
                let (a, b) = if length {
                    (from, from + t)
                } else {
                    (MusicalTime::ZERO, t)
                };
                let frames = p.timeline.to_samples(b, rate) - p.timeline.to_samples(a, rate);
                format_seconds(frames as f64 / rate)
            }
            CounterUnit::Samples => {
                let (a, b) = if length {
                    (from, from + t)
                } else {
                    (MusicalTime::ZERO, t)
                };
                format!(
                    "{}",
                    p.timeline.to_samples(b, rate) - p.timeline.to_samples(a, rate)
                )
            }
        }
    }

    fn item_at(&self, pos: Point, size: Size, s: &Session) -> Option<Item> {
        self.items(size, s)
            .into_iter()
            .find(|(_, r, _, _)| r.contains(pos))
            .map(|(i, ..)| i)
    }

    fn tooltip_of(item: Item) -> String {
        match item {
            Item::Mode(EditMode::Shuffle) => {
                "Shuffle (Alt+1): clips stay butted — moving reorders, trimming and deleting close gaps".into()
            }
            Item::Mode(EditMode::Slip) => "Slip (Alt+2): move and trim freely".into(),
            Item::Mode(EditMode::Spot) => "Spot (Alt+3): clicking a clip asks for its position".into(),
            Item::Mode(EditMode::Grid) => "Grid (Alt+4): snap to the grid".into(),
            Item::GridModeMenu => "Grid mode: Absolute (to grid lines) or Relative (by grid steps)".into(),
            Item::Tool(EditTool::Smart) => {
                "Smart tool (Alt+S): select in the upper half, grab in the lower half, trim at the edges, fade at the upper corners, drag the dB readout for clip gain".into()
            }
            Item::Tool(EditTool::Zoom) => "Zoom tool (F5, Alt+5): click zooms in, Alt-click out, drag a range".into(),
            Item::Tool(EditTool::Trim) | Item::Tool(EditTool::TrimStretch) => {
                "Trim tool (F6, Alt+6): drag clip edges (Stretch: time-compress/expand the clip)".into()
            }
            Item::Tool(EditTool::Select) => "Selector (F7, Alt+7): click places the cursor, drag selects a time range".into(),
            Item::Tool(EditTool::Grab) | Item::Tool(EditTool::GrabSeparation) => {
                "Grabber (Alt+8): move clips (Separate: lift the selected range out and move it)".into()
            }
            Item::Tool(EditTool::Scrub) => "Scrubber (F9, Alt+9): drag to move the playhead".into(),
            Item::Tool(EditTool::Pencil) => {
                "Pencil (F10, Alt+0): draw MIDI clips on instrument tracks; zoomed in to single samples, redraw audio (click repair)".into()
            }
            Item::TrimMenu => "Trim tool kind".into(),
            Item::GrabMenu => "Grabber kind".into(),
            Item::GridValue => "Grid value (up to 1/256)".into(),
            Item::Triplet => "Triplet grid".into(),
            Item::Dotted => "Dotted grid".into(),
            Item::Nudge => "Nudge value (, and . nudge the selection; Alt: trim start, Ctrl: trim end)".into(),
            Item::Flag(EditFlag::TabToTransients) => "Tab to Transients: Tab moves to the next detected transient".into(),
            Item::Flag(EditFlag::LinkTimeline) => {
                "Link Timeline and Edit Selection: clicking in clips moves the playhead".into()
            }
            Item::Flag(EditFlag::InsertionFollowsPlayback) => {
                "Insertion Follows Playback: off returns to where playback started when stopping".into()
            }
            Item::Flag(EditFlag::FollowPlayhead) => {
                "Follow Playhead: the arranger and piano roll scroll along while playing".into()
            }
            Item::Flag(EditFlag::ShowTransients) => "Show detected transients in audio clips".into(),
            Item::Flag(EditFlag::Warp) => {
                "Warp view: double-click adds a warp marker; drag a transient to move just that hit (Alt: up to the next markers, Ctrl: telescoping), drag inside a selection to warp only the range, Alt-click a marker to remove it".into()
            }
            Item::Flag(EditFlag::EditToolbar) => String::new(),
            Item::Sensitivity => "Transient detection sensitivity".into(),
            Item::Groove => "Quantize (Q) or Humanize the selected clips — audio by its transients, MIDI by its notes — and their settings".into(),
            Item::Counter(_) => "Edit selection (click: change units)".into(),
            Item::Zoom(ZoomRequest::In) => "Zoom in (Ctrl+])".into(),
            Item::Zoom(ZoomRequest::Out) => "Zoom out (Ctrl+[)".into(),
            Item::Zoom(ZoomRequest::Selection) => "Zoom to the edit selection".into(),
            Item::Zoom(ZoomRequest::Fit) => "Zoom to fit the project".into(),
        }
    }

    fn press(&self, item: Item, at: Point, s: &Session, cx: &mut EventCx<'_, Action>) {
        let e = &s.editor;
        match item {
            Item::Mode(m) => cx.emit(Action::SetEditMode(m)),
            Item::GridModeMenu => cx.request(HostRequest::ContextMenu {
                at,
                items: vec![
                    MenuItem::new("Absolute Grid", Action::SetGridMode(GridMode::Absolute))
                        .checked(e.grid_mode == GridMode::Absolute),
                    MenuItem::new("Relative Grid", Action::SetGridMode(GridMode::Relative))
                        .checked(e.grid_mode == GridMode::Relative),
                ],
            }),
            Item::Tool(t) => cx.emit(Action::SetEditTool(t)),
            Item::TrimMenu => cx.request(HostRequest::ContextMenu {
                at,
                items: vec![
                    MenuItem::new("Trim", Action::SetEditTool(EditTool::Trim))
                        .checked(e.tool == EditTool::Trim),
                    MenuItem::new(
                        "Time-Stretch Trim",
                        Action::SetEditTool(EditTool::TrimStretch),
                    )
                    .checked(e.tool == EditTool::TrimStretch),
                ],
            }),
            Item::GrabMenu => cx.request(HostRequest::ContextMenu {
                at,
                items: vec![
                    MenuItem::new("Object Grabber", Action::SetEditTool(EditTool::Grab))
                        .checked(e.tool == EditTool::Grab),
                    MenuItem::new(
                        "Separation Grabber",
                        Action::SetEditTool(EditTool::GrabSeparation),
                    )
                    .checked(e.tool == EditTool::GrabSeparation),
                ],
            }),
            Item::GridValue => cx.request(HostRequest::ContextMenu {
                at,
                items: grid_menu(e.grid),
            }),
            Item::Triplet => cx.emit(Action::SetGrid(e.grid.toggled_triplet())),
            Item::Dotted => cx.emit(Action::SetGrid(e.grid.toggled_dotted())),
            Item::Nudge => {
                let items = NudgeValue::all()
                    .into_iter()
                    .map(|v| MenuItem::new(v.label(), Action::SetNudge(v)).checked(e.nudge == v))
                    .collect();
                cx.request(HostRequest::ContextMenu { at, items });
            }
            Item::Flag(f) => {
                let on = match f {
                    EditFlag::TabToTransients => e.tab_to_transients,
                    EditFlag::LinkTimeline => e.link_timeline,
                    EditFlag::InsertionFollowsPlayback => e.insertion_follows_playback,
                    EditFlag::FollowPlayhead => e.follow_playhead,
                    EditFlag::ShowTransients => e.show_transients,
                    EditFlag::Warp => e.warp,
                    EditFlag::EditToolbar => e.show_edit_toolbar,
                };
                cx.emit(Action::SetEditFlag(f, !on));
            }
            Item::Groove => {
                let clips: Vec<_> = s.selection.clips.iter().copied().collect();
                let mut items = if clips.is_empty() {
                    vec![
                        MenuItem::disabled("Quantize Selected Clips (Q)"),
                        MenuItem::disabled("Humanize Selected Clips"),
                    ]
                } else {
                    vec![
                        MenuItem::new(
                            "Quantize Selected Clips (Q)",
                            Action::QuantizeClips(clips.clone()),
                        ),
                        MenuItem::new("Humanize Selected Clips", Action::HumanizeClips(clips)),
                    ]
                };
                for (i, e) in s.groove_settings_menu().into_iter().enumerate() {
                    let item = MenuItem::new(e.label, e.action).checked(e.checked);
                    items.push(if e.separated || i == 0 {
                        item.separated()
                    } else {
                        item
                    });
                }
                cx.request(HostRequest::ContextMenu { at, items });
            }
            Item::Sensitivity => {
                let items = [0.1f32, 0.25, 0.4, 0.5, 0.6, 0.75, 0.9, 1.0]
                    .into_iter()
                    .map(|v| {
                        MenuItem::new(
                            format!("{:.0}%", v * 100.0),
                            Action::SetTransientSensitivity(v),
                        )
                        .checked((e.transient_sensitivity - v).abs() < 0.01)
                    })
                    .collect();
                cx.request(HostRequest::ContextMenu { at, items });
            }
            Item::Counter(_) => {
                let i = CounterUnit::ALL
                    .iter()
                    .position(|u| *u == e.counter_unit)
                    .unwrap_or(0);
                cx.emit(Action::SetCounterUnit(
                    CounterUnit::ALL[(i + 1) % CounterUnit::ALL.len()],
                ));
            }
            Item::Zoom(z) => cx.emit(Action::Zoom(z)),
        }
    }
}

impl CanvasView<Session, Action> for EditToolbarView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, s: &Session, _theme: &Theme) {
        let th = &self.theme;
        let pr = &th.piano;
        let r = Rect::from_size(size);
        p.fill(r, pr.toolbar);
        p.hline(r.x, r.right(), r.bottom() - 0.5, th.ui.border);
        for (item, rect, label, on) in self.items(size, s) {
            if let Item::Counter(_) = item {
                p.fill_rounded(rect, 3.0, &Paint::Solid(th.ui.lcd_bg));
                p.text(
                    &label,
                    rect.inset_xy(6.0, 0.0),
                    &TextStyle::new(th.fonts.small, th.ui.lcd_text)
                        .family(faderframe_ui_canvas::FontFamily::Mono),
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

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        s: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        if let ViewEvent::PointerDown {
            pos,
            button: PointerButton::Primary,
            ..
        } = *ev
            && let Some(item) = self.item_at(pos, size, s)
        {
            self.press(item, pos, s, cx);
            return true;
        }
        false
    }

    fn tooltip(&self, pos: Point, size: Size, s: &Session) -> Option<String> {
        self.item_at(pos, size, s).map(Self::tooltip_of)
    }

    fn min_size(&self) -> Size {
        Size::new(200.0, 34.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_engine::EngineConfig;
    use faderframe_ui_canvas::{Modifiers, RecordingPainter};

    fn click(view: &mut EditToolbarView, s: &mut Session, item: Item) -> Vec<HostRequest<Action>> {
        let size = Size::new(1900.0, 36.0);
        let r = view
            .items(size, s)
            .into_iter()
            .find(|(i, ..)| *i == item)
            .unwrap()
            .1;
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        {
            let mut cx = EventCx::new(&mut actions, &mut requests);
            view.event(
                &ViewEvent::PointerDown {
                    pos: r.center(),
                    button: PointerButton::Primary,
                    modifiers: Modifiers::NONE,
                    clicks: 1,
                },
                size,
                s,
                &mut cx,
            );
        }
        for a in actions {
            s.dispatch(a).unwrap();
        }
        requests
    }

    #[test]
    fn modes_tools_grid_and_flags() {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let mut v = EditToolbarView::new(Theme::default());
        click(&mut v, &mut s, Item::Mode(EditMode::Shuffle));
        assert_eq!(s.editor.edit_mode, EditMode::Shuffle);
        assert!(!s.editor.snap, "only Grid mode snaps");
        click(&mut v, &mut s, Item::Mode(EditMode::Grid));
        assert!(s.editor.snap);
        click(&mut v, &mut s, Item::Tool(EditTool::Select));
        assert_eq!(s.editor.tool, EditTool::Select);
        s.dispatch(Action::SetGrid(GridDivision::Note(16))).unwrap();
        click(&mut v, &mut s, Item::Triplet);
        assert_eq!(s.editor.grid, GridDivision::Triplet(16));
        click(&mut v, &mut s, Item::Dotted);
        assert_eq!(s.editor.grid, GridDivision::Dotted(16));
        let menus = click(&mut v, &mut s, Item::GridValue);
        let Some(HostRequest::ContextMenu { items, .. }) = menus.first() else {
            panic!("grid menu")
        };
        assert!(items.iter().any(|i| i.label == "1/256"));
        click(&mut v, &mut s, Item::Flag(EditFlag::Warp));
        assert!(s.editor.warp);
        click(&mut v, &mut s, Item::Counter(0));
        assert_eq!(s.editor.counter_unit, CounterUnit::MinSecs);
        let before = s.editor.zoom_request.0;
        click(&mut v, &mut s, Item::Zoom(ZoomRequest::In));
        assert_eq!(s.editor.zoom_request, (before + 1, ZoomRequest::In));
        let mut p = RecordingPainter::new();
        v.paint(&mut p, Size::new(1900.0, 36.0), &s, &Theme::default());
        assert!(p.texts().iter().any(|t| t.starts_with("Start ")));
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use faderframe_engine::EngineConfig;

    #[test]
    fn narrow_windows_wrap_into_rows() {
        let s = Session::demo(EngineConfig::default()).unwrap();
        let v = EditToolbarView::new(Theme::default());
        assert_eq!(v.preferred_height(2400.0, &s), EditToolbarView::ROW);
        let mut last = EditToolbarView::ROW;
        for w in [1300.0, 900.0, 600.0, 420.0] {
            let h = v.preferred_height(w, &s);
            assert!(h >= last, "{w}: {h}");
            last = h;
            let items = v.items(Size::new(w, h), &s);
            assert_eq!(
                items.len(),
                v.items(Size::new(2400.0, 36.0), &s).len(),
                "nothing dropped"
            );
            assert!(
                items
                    .iter()
                    .all(|(_, r, _, _)| r.right() <= w && r.bottom() <= h)
            );
        }
        assert!(
            last >= 4.0 * EditToolbarView::ROW,
            "narrow windows use 4+ rows"
        );
    }
}
