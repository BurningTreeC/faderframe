//! The Inspector: the selected track's settings in one column, left of
//! the arranger (Studio One's Inspector). Top to bottom:
//!
//! * its name (click to rename), colour (click to choose), kind, format
//!   and number — with several tracks selected, which one it shows (edits
//!   reach every selected track, as everywhere);
//! * **Routing**: input, output (a MIDI track: the instrument it plays and
//!   its MIDI device), monitoring, channel format, pan mode, the surround
//!   panner, object delivery;
//! * **Track**: record arm, MPE, group and VCA, folder, automation, freeze
//!   (the master: listening and the console);
//! * **Devices**: the processing chain in signal-flow order — the same
//!   cards as the Channel Overview, one per line;
//! * **Channel**: the mixer's own strip of the track (fader, meter, pan,
//!   mute, solo, arm, sends, input stage), hosted here.
//!
//! Each section folds by its title. Every value comes from the session
//! (the choice lists the track menus are built from, `shown_*` values
//! while automation drives them) and every control sends the session
//! action the mixer and the arranger send: there is no second copy of a
//! track's settings. The view keeps only its scroll, folds, hover and a
//! drag in progress.

use crate::chain::{self, Device, Role};
use faderframe_automation::AutomationMode;
use faderframe_core::TrackId;
use faderframe_project::{Command, MonitorMode, Track, TrackKind};
use faderframe_session::{Action, InputChoice, Session};
use faderframe_ui_canvas::{
    AccessNode, AccessRole, CanvasView, Color, Cursor, EventCx, HostRequest, MenuItem, Modifiers,
    Paint, Painter, Point, PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme,
    ViewEvent, access_id,
};
use faderframe_view_mixer::MixerView;

const HEADER_H: f32 = 52.0;
const SECTION_H: f32 = 24.0;
const ROW_H: f32 = 24.0;
const DEVICE_H: f32 = 26.0;
const LABEL_W: f32 = 78.0;
const PAD: f32 = 8.0;
/// The channel strip's height at least (the console strip with an input
/// stage, its input, one row of sends and a full fader), each further row
/// of sends more; a folder's strip is short. Its width at most: the
/// mixer's widest strip.
const STRIP_MIN_H: f32 = 500.0;
const SEND_ROW_H: f32 = 44.0;
const FOLDER_STRIP_H: f32 = 170.0;
const STRIP_MAX_W: f32 = faderframe_session::STRIP_WIDTH_RANGE.1;
/// Indent of a device inside a container, per level.
const NEST_W: f32 = 12.0;

const NODE_VIEW: u64 = 0x1101;
const NODE_NAME: u64 = 0x1102;
const NODE_COLOR: u64 = 0x1103;
const NODE_SECTION: u64 = 0x1104;
const NODE_ROW: u64 = 0x1105;
const NODE_CHOICE: u64 = 0x1106;
const NODE_ADD: u64 = 0x1107;

/// The Inspector's sections, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Routing,
    Track,
    Devices,
    Channel,
}

impl Section {
    pub const ALL: [Section; 4] = [
        Section::Routing,
        Section::Track,
        Section::Devices,
        Section::Channel,
    ];

    fn title(self) -> &'static str {
        match self {
            Section::Routing => "ROUTING",
            Section::Track => "TRACK",
            Section::Devices => "DEVICES",
            Section::Channel => "CHANNEL",
        }
    }
}

/// A setting on a line of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Input,
    /// Where it goes (a MIDI track: the instrument track it plays).
    Output,
    /// A MIDI track's external MIDI device.
    MidiOut,
    Monitor,
    Format,
    PanMode,
    Surround,
    Object,
    Record,
    Mpe,
    Group,
    Folder,
    /// A folder track's contents (and open or closed).
    Contents,
    /// A folder's Group switch: what it holds is edited and armed as one.
    FolderGroup,
    /// A folder (or folder bus): whether it is a bus that sums what it
    /// holds (`Action::SetFolderBus`).
    FolderBus,
    Automation,
    /// Whether ripples of edits elsewhere move it (`Track::ripple_excluded`).
    Ripple,
    Freeze,
    /// The master: speakers or headphones, the mono check.
    Listen,
    /// The master: the console's summing.
    Console,
}

/// What a line's value does when pressed.
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    /// Opens a menu of its choices.
    Menu,
    /// On or off.
    Toggle(bool),
    /// One of a few, side by side (label, on).
    Segments(Vec<(&'static str, bool)>),
    /// Does one thing.
    Button,
    /// Only says.
    Info,
}

#[derive(Clone, Debug, PartialEq)]
struct Row {
    field: Field,
    label: &'static str,
    value: String,
    kind: Kind,
}

/// What is under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Color,
    Name,
    /// The header's rest (right-click: the strip's track menu).
    Header,
    Section(Section),
    Field(Field),
    /// One of a segmented field's choices.
    Segment(Field, usize),
    AddDevice,
    Device(usize),
    Power(usize),
    Remove(usize),
    /// The hosted channel strip.
    Strip,
}

/// A device pressed in the list: a click on release, or a drag.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    device: usize,
    origin: Point,
    pos: Point,
    moved: bool,
}

pub struct InspectorView {
    theme: Theme,
    /// The console's strip of the shown track.
    strip: MixerView,
    scroll_y: f32,
    folded: Vec<Section>,
    hover: Option<Hit>,
    drag: Option<Drag>,
    /// A press went to the strip: its drag goes on there.
    strip_grab: bool,
    /// The track shown last (a new one scrolls back up).
    shown: Option<TrackId>,
}

/// A session choice list as menu items.
fn choice_items(list: Vec<InputChoice>) -> Vec<MenuItem<Action>> {
    list.into_iter()
        .map(|c| {
            let item = MenuItem::new(c.label, c.action).checked(c.checked);
            if c.group_start {
                item.separated()
            } else {
                item
            }
        })
        .collect()
}

fn mode_label(m: AutomationMode) -> &'static str {
    match m {
        AutomationMode::Off => "Off",
        AutomationMode::Read => "Read",
        AutomationMode::Touch => "Touch",
        AutomationMode::Latch => "Latch",
        AutomationMode::Write => "Write",
    }
}

const MODES: [AutomationMode; 5] = [
    AutomationMode::Off,
    AutomationMode::Read,
    AutomationMode::Touch,
    AutomationMode::Latch,
    AutomationMode::Write,
];

/// The monitoring choices a track offers (notes "play live").
fn monitor_segments(t: &Track) -> Vec<(&'static str, bool)> {
    let notes = matches!(t.kind, TrackKind::Instrument | TrackKind::Midi);
    vec![
        ("Off", t.monitor == MonitorMode::Off),
        ("Auto", t.monitor == MonitorMode::Auto),
        (
            if notes { "Always" } else { "Input" },
            t.monitor == MonitorMode::Input,
        ),
    ]
}

const MONITOR_MODES: [MonitorMode; 3] = [MonitorMode::Off, MonitorMode::Auto, MonitorMode::Input];

impl InspectorView {
    pub fn new(theme: Theme) -> Self {
        Self {
            strip: MixerView::single_strip(theme.clone()),
            theme,
            scroll_y: 0.0,
            folded: Vec::new(),
            hover: None,
            drag: None,
            strip_grab: false,
            shown: None,
        }
    }

    fn track(model: &Session) -> Option<&Track> {
        chain::inspected_track(model).and_then(|t| model.project().track(t))
    }

    /// Fold or unfold a section (as its title does).
    pub fn toggle_section(&mut self, s: Section) {
        if let Some(i) = self.folded.iter().position(|x| *x == s) {
            self.folded.remove(i);
        } else {
            self.folded.push(s);
        }
    }

    pub fn is_folded(&self, s: Section) -> bool {
        self.folded.contains(&s)
    }

    /// A section's lines for `t`.
    fn rows(model: &Session, t: &Track, s: Section) -> Vec<Row> {
        let p = model.project();
        let mut out = Vec::new();
        let mut push = |field, label, value: String, kind| {
            out.push(Row {
                field,
                label,
                value,
                kind,
            })
        };
        match s {
            Section::Routing => {
                let notes = matches!(t.kind, TrackKind::Instrument | TrackKind::Midi);
                push(
                    Field::Input,
                    if notes { "MIDI In" } else { "Input" },
                    chain::input_label(model, t),
                    if chain::has_input(t) {
                        Kind::Menu
                    } else {
                        Kind::Info
                    },
                );
                if t.kind == TrackKind::Midi {
                    push(
                        Field::Output,
                        "Plays",
                        chain::output_label(model, t),
                        Kind::Menu,
                    );
                    push(
                        Field::MidiOut,
                        "MIDI Out",
                        chain::checked_label(&model.midi_output_choices(t.id)),
                        Kind::Menu,
                    );
                } else if t.kind.has_audio() {
                    push(
                        Field::Output,
                        "Output",
                        chain::output_label(model, t),
                        Kind::Menu,
                    );
                }
                if t.kind.has_clips() {
                    push(
                        Field::Monitor,
                        if notes { "Play Live" } else { "Monitor" },
                        String::new(),
                        Kind::Segments(monitor_segments(t)),
                    );
                }
                let formats = model.format_choices(t.id);
                if !formats.is_empty() {
                    push(
                        Field::Format,
                        "Format",
                        chain::checked_label(&formats),
                        Kind::Menu,
                    );
                }
                let modes = model.pan_mode_choices(t.id);
                if !modes.is_empty() {
                    push(
                        Field::PanMode,
                        "Pan Mode",
                        chain::checked_label(&modes),
                        Kind::Menu,
                    );
                }
                if let Some(bed) = p.surround_panned(t) {
                    push(
                        Field::Surround,
                        "Surround",
                        format!("into {} · Panner…", bed.name()),
                        Kind::Button,
                    );
                }
                if let Some(c) = model.object_choice(t.id) {
                    push(
                        Field::Object,
                        "Object",
                        if c.checked {
                            "delivered as an object"
                        } else {
                            "mixed into the bed"
                        }
                        .into(),
                        Kind::Toggle(c.checked),
                    );
                }
            }
            Section::Track => {
                if t.kind.has_clips() {
                    push(
                        Field::Record,
                        "Record",
                        if t.record_arm { "armed" } else { "not armed" }.into(),
                        Kind::Toggle(t.record_arm),
                    );
                }
                if matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
                    push(
                        Field::Mpe,
                        "MPE",
                        if t.mpe.is_some() {
                            "per-note expression"
                        } else {
                            "off"
                        }
                        .into(),
                        Kind::Toggle(t.mpe.is_some()),
                    );
                }
                if t.kind != TrackKind::Master && t.kind != TrackKind::Folder {
                    let group = t.group.and_then(|g| p.group(g)).map(|g| g.name.clone());
                    let vca = t.vca.and_then(|v| p.track(v)).map(|v| v.name.clone());
                    let value = match (group, vca) {
                        (None, None) => "—".to_string(),
                        (Some(g), None) => g,
                        (None, Some(v)) => format!("VCA {v}"),
                        (Some(g), Some(v)) => format!("{g} · VCA {v}"),
                    };
                    let kind = if model.group_menu(t.id).is_empty() {
                        Kind::Info
                    } else {
                        Kind::Menu
                    };
                    push(Field::Group, "Group", value, kind);
                }
                if t.kind != TrackKind::Master {
                    let parent = t.folder.and_then(|f| p.track(f)).map(|f| f.name.clone());
                    let kind = if model.folder_choices(t.id).is_empty() {
                        Kind::Info
                    } else {
                        Kind::Menu
                    };
                    push(
                        Field::Folder,
                        "In Folder",
                        parent.unwrap_or_else(|| "—".into()),
                        kind,
                    );
                }
                if t.is_folder() {
                    let open = model.folder_open(t.id);
                    push(
                        Field::Contents,
                        "Contents",
                        format!(
                            "{} · {}",
                            chain::contents_label(model, t),
                            if open { "open" } else { "closed" }
                        ),
                        Kind::Toggle(open),
                    );
                }
                // A folder sums what it holds as a bus (or no longer):
                // then the bus's controls are the track's (its output,
                // format, devices and strip above and below).
                if t.is_folder() {
                    let bus = t.is_folder_bus();
                    push(
                        Field::FolderBus,
                        "Folder Bus",
                        if bus {
                            "sums its tracks"
                        } else {
                            "off · a plain folder"
                        }
                        .into(),
                        Kind::Toggle(bus),
                    );
                }
                if t.is_folder() {
                    // Its own switch; inside a grouped folder it is part
                    // of that folder's group.
                    let outer = p.edit_group(t).filter(|g| g.id != t.id);
                    let value = match (outer, t.folder_group) {
                        (Some(o), _) => format!("with ‘{}’", o.name),
                        (None, true) => "edited and armed as one".into(),
                        (None, false) => "off".into(),
                    };
                    push(
                        Field::FolderGroup,
                        "Grouped",
                        value,
                        Kind::Toggle(t.folder_group),
                    );
                }
                if t.kind.has_audio() || t.kind == TrackKind::Vca {
                    let lanes = &t.automation.lanes;
                    let value = match lanes.first() {
                        None => "no lanes".to_string(),
                        Some(first) if lanes.iter().all(|l| l.mode == first.mode) => format!(
                            "{} · {} lane{}",
                            mode_label(first.mode),
                            lanes.len(),
                            if lanes.len() == 1 { "" } else { "s" }
                        ),
                        Some(_) => format!("mixed · {} lanes", lanes.len()),
                    };
                    push(Field::Automation, "Automation", value, Kind::Menu);
                }
                if !t.is_folder() {
                    push(
                        Field::Ripple,
                        "Ripple",
                        if t.ripple_excluded {
                            "left out · stays put"
                        } else {
                            "follows edits"
                        }
                        .into(),
                        Kind::Toggle(!t.ripple_excluded),
                    );
                }
                if matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) {
                    let value = if model.bouncing().contains(&t.id) {
                        "rendering…"
                    } else if t.freeze.is_some() {
                        "frozen · Unfreeze"
                    } else {
                        "live · Freeze"
                    };
                    push(Field::Freeze, "Freeze", value.into(), Kind::Button);
                }
                if t.kind == TrackKind::Master {
                    push(
                        Field::Listen,
                        "Listen",
                        chain::checked_label(&model.listen_choices()),
                        Kind::Menu,
                    );
                    push(
                        Field::Console,
                        "Console",
                        chain::checked_label(&model.console_choices()),
                        Kind::Menu,
                    );
                }
            }
            Section::Devices | Section::Channel => {}
        }
        out
    }

    /// A field's menu (`None`: it has none).
    fn menu(model: &Session, t: &Track, field: Field, at: Point) -> Option<HostRequest<Action>> {
        let menu = |items: Vec<MenuItem<Action>>| {
            (!items.is_empty()).then_some(HostRequest::ContextMenu { at, items })
        };
        match field {
            Field::Input => chain::has_input(t).then(|| MixerView::input_menu(model, t, at)),
            Field::Output if t.kind == TrackKind::Midi => Some(MixerView::plays_menu(model, t, at)),
            Field::Output => Some(MixerView::output_menu(model, t, at)),
            Field::MidiOut => Some(MixerView::midi_out_menu(model, t, at)),
            Field::Format => menu(choice_items(model.format_choices(t.id))),
            Field::PanMode => menu(choice_items(model.pan_mode_choices(t.id))),
            Field::Group => menu(
                model
                    .group_menu(t.id)
                    .into_iter()
                    .map(faderframe_view_mixer::menu_item)
                    .collect(),
            ),
            Field::Folder => menu(
                model
                    .folder_choices(t.id)
                    .into_iter()
                    .map(|(label, action, separated)| {
                        let item = MenuItem::new(label, action);
                        if separated { item.separated() } else { item }
                    })
                    .collect(),
            ),
            Field::Automation => {
                let lanes: Vec<_> = t.automation.lanes.iter().map(|l| (t.id, l.id)).collect();
                let mut items = vec![MenuItem::new(
                    "Show / Hide Its Lanes",
                    Action::ToggleTrackAutomation(t.id),
                )];
                if !lanes.is_empty() {
                    for (i, m) in MODES.into_iter().enumerate() {
                        let all = t.automation.lanes.iter().all(|l| l.mode == m);
                        let item = MenuItem::new(
                            format!("Every Lane: {}", mode_label(m)),
                            Action::SetAutomationModes {
                                lanes: lanes.clone(),
                                mode: m,
                            },
                        )
                        .checked(all);
                        items.push(if i == 0 { item.separated() } else { item });
                    }
                }
                menu(items)
            }
            Field::Listen => menu(choice_items(model.listen_choices())),
            Field::Console => menu(choice_items(model.console_choices())),
            _ => None,
        }
    }

    /// What pressing a field (not a menu) does.
    fn field_action(model: &Session, t: &Track, field: Field) -> Option<Action> {
        Some(match field {
            Field::Record => Action::Edit(Command::SetTrackRecordArm {
                track: t.id,
                on: !t.record_arm,
            }),
            Field::Mpe => Action::Edit(Command::SetTrackMpe {
                track: t.id,
                mpe: t.mpe.is_none().then(faderframe_project::MpeConfig::default),
            }),
            Field::Object => model.object_choice(t.id)?.action,
            Field::Surround => Action::ShowSurroundPanner(t.id),
            Field::Contents => Action::ToggleFolder(t.id),
            Field::FolderGroup => Action::SetFolderGroup {
                folder: t.id,
                on: !t.folder_group,
            },
            Field::FolderBus => Action::SetFolderBus {
                folder: t.id,
                on: !t.is_folder_bus(),
            },
            Field::Ripple => Action::Ripple(faderframe_session::RippleOp::Exclude {
                tracks: vec![t.id],
                excluded: !t.ripple_excluded,
            }),
            Field::Freeze => {
                if model.bouncing().contains(&t.id) {
                    return None;
                }
                if t.freeze.is_some() {
                    Action::UnfreezeTrack(t.id)
                } else {
                    Action::FreezeTrack(t.id)
                }
            }
            _ => return None,
        })
    }

    /// The channel strip's height for `t` (0 folded).
    fn strip_h(&self, t: &Track, viewport_rest: f32) -> f32 {
        if self.is_folded(Section::Channel) {
            return 0.0;
        }
        if t.kind == TrackKind::Folder {
            return FOLDER_STRIP_H;
        }
        // Room for its input stage, input, every send row and a full
        // fader (the strip drops what does not fit).
        let rows = (t.sends.len() + 1)
            .div_ceil(faderframe_view_mixer::SENDS_PER_ROW)
            .clamp(1, faderframe_view_mixer::MAX_SEND_ROWS);
        viewport_rest.max(STRIP_MIN_H + SEND_ROW_H * (rows - 1) as f32)
    }

    /// Everything laid out, in view coordinates (scrolled).
    fn layout(&self, model: &Session, t: &Track, size: Size) -> Layout {
        let mut y = HEADER_H - self.scroll_y;
        let w = size.w;
        let mut l = Layout::default();
        for s in Section::ALL {
            let title = Rect::new(0.0, y, w, SECTION_H);
            l.sections.push((s, title));
            y += SECTION_H;
            if self.is_folded(s) {
                continue;
            }
            match s {
                Section::Routing | Section::Track => {
                    for row in Self::rows(model, t, s) {
                        l.rows.push((row, Rect::new(0.0, y, w, ROW_H)));
                        y += ROW_H;
                    }
                    y += 4.0;
                }
                Section::Devices => {
                    let devices = chain::devices(model, t);
                    for (i, d) in devices.iter().enumerate() {
                        let indent = NEST_W * d.depth.min(4) as f32;
                        l.devices.push((
                            i,
                            Rect::new(
                                PAD + indent,
                                y + 1.0,
                                w - 2.0 * PAD - indent,
                                DEVICE_H - 2.0,
                            ),
                        ));
                        y += DEVICE_H;
                    }
                    if devices.is_empty() {
                        y += DEVICE_H;
                    }
                    y += 4.0;
                }
                Section::Channel => {
                    let rest = size.h - (y + self.scroll_y).min(size.h);
                    let h = self.strip_h(t, rest.max(0.0));
                    // As wide as the mixer's widest strip at most, centred.
                    let sw = w.min(STRIP_MAX_W);
                    l.strip = Some(Rect::new(((w - sw) / 2.0).floor(), y, sw, h));
                    y += h;
                }
            }
        }
        l.content_h = y + self.scroll_y;
        l
    }

    /// The devices section's "+" (in its title).
    fn add_rect(title: Rect) -> Rect {
        Rect::new(title.right() - 30.0, title.y + 3.0, 24.0, SECTION_H - 6.0)
    }

    /// A device line's power button and ×.
    fn device_buttons(r: Rect) -> (Rect, Rect) {
        (
            Rect::new(r.x + 4.0, r.y + 4.0, 18.0, r.h - 8.0),
            Rect::new(r.right() - 20.0, r.y + 4.0, 16.0, r.h - 8.0),
        )
    }

    fn header_rects(size: Size) -> (Rect, Rect) {
        (
            Rect::new(PAD, 8.0, 10.0, HEADER_H - 16.0),
            Rect::new(PAD + 18.0, 6.0, size.w - PAD - 26.0, 22.0),
        )
    }

    /// A segmented field's choices.
    fn segment_rects(value: Rect, n: usize) -> Vec<Rect> {
        let w = (value.w - 4.0 * (n.saturating_sub(1)) as f32) / n.max(1) as f32;
        (0..n)
            .map(|i| Rect::new(value.x + i as f32 * (w + 4.0), value.y, w, value.h))
            .collect()
    }

    fn value_rect(row: Rect) -> Rect {
        Rect::new(
            row.x + PAD + LABEL_W,
            row.y + 2.0,
            row.w - 2.0 * PAD - LABEL_W,
            row.h - 4.0,
        )
    }

    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        let t = Self::track(model)?;
        if pos.y < HEADER_H {
            let (color, name) = Self::header_rects(size);
            return Some(if color.contains(pos) {
                Hit::Color
            } else if name.contains(pos) {
                Hit::Name
            } else {
                Hit::Header
            });
        }
        let l = self.layout(model, t, size);
        if let Some(s) = l.strip
            && s.contains(pos)
        {
            return Some(Hit::Strip);
        }
        for (s, r) in &l.sections {
            if r.contains(pos) {
                if *s == Section::Devices
                    && chain::holds_devices(t)
                    && Self::add_rect(*r).contains(pos)
                {
                    return Some(Hit::AddDevice);
                }
                return Some(Hit::Section(*s));
            }
        }
        for (row, r) in &l.rows {
            if !r.contains(pos) {
                continue;
            }
            let value = Self::value_rect(*r);
            if let Kind::Segments(segs) = &row.kind {
                return Self::segment_rects(value, segs.len())
                    .iter()
                    .position(|s| s.contains(pos))
                    .map(|i| Hit::Segment(row.field, i));
            }
            if row.kind == Kind::Info || !value.contains(pos) {
                return None;
            }
            return Some(Hit::Field(row.field));
        }
        let devices = chain::devices(model, t);
        for (i, r) in &l.devices {
            if !r.contains(pos) {
                continue;
            }
            let (power, remove) = Self::device_buttons(*r);
            return Some(if power.contains(pos) {
                Hit::Power(*i)
            } else if remove.contains(pos) && chain::remove(t, &devices[*i]).is_some() {
                Hit::Remove(*i)
            } else {
                Hit::Device(*i)
            });
        }
        None
    }

    /// Run `ev` through the hosted strip (its own coordinates), passing on
    /// what it asks for.
    fn strip_event(
        &mut self,
        ev: &ViewEvent,
        strip: Rect,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let (dx, dy) = (strip.x, strip.y);
        let mut e = *ev;
        match &mut e {
            ViewEvent::PointerDown { pos, .. }
            | ViewEvent::PointerMove { pos, .. }
            | ViewEvent::PointerUp { pos, .. }
            | ViewEvent::Scroll { pos, .. } => {
                pos.x -= dx;
                pos.y -= dy;
            }
            _ => {}
        }
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let mut inner = EventCx::new(&mut actions, &mut requests);
        let used = self.strip.event(&e, strip.size(), model, &mut inner);
        let redraw = inner.wants_redraw();
        let cursor = inner.cursor();
        for a in actions {
            cx.emit(a);
        }
        for r in requests {
            cx.request(match r {
                HostRequest::ContextMenu { at, items } => HostRequest::ContextMenu {
                    at: at.offset(dx, dy),
                    items,
                },
                HostRequest::TextInput {
                    at,
                    initial,
                    commit,
                } => HostRequest::TextInput {
                    at: at.translate(dx, dy),
                    initial,
                    commit,
                },
                other => other,
            });
        }
        if let Some(c) = cursor {
            cx.set_cursor(c);
        }
        if redraw {
            cx.redraw();
        }
        used
    }

    fn press(
        &mut self,
        pos: Point,
        mods: Modifiers,
        clicks: u32,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let (Some(t), Some(hit)) = (Self::track(model), self.hit(pos, size, model)) else {
            return false;
        };
        let devices = chain::devices(model, t);
        match hit {
            Hit::Color => cx.emit(Action::PickColor(faderframe_session::ColorTarget::Track(
                t.id,
            ))),
            Hit::Name => {
                let (_, name) = Self::header_rects(size);
                let id = t.id;
                cx.request(HostRequest::TextInput {
                    at: name,
                    initial: t.name.clone(),
                    commit: Box::new(move |text| {
                        let name = text.trim();
                        (!name.is_empty()).then(|| {
                            Action::Edit(Command::RenameTrack {
                                track: id,
                                name: name.to_string(),
                            })
                        })
                    }),
                });
            }
            Hit::Header => {}
            Hit::Section(s) => {
                self.toggle_section(s);
                cx.redraw();
            }
            Hit::Field(f) => {
                let l = self.layout(model, t, size);
                let at = l
                    .rows
                    .iter()
                    .find(|(r, _)| r.field == f)
                    .map_or(pos, |(_, r)| {
                        let v = Self::value_rect(*r);
                        Point::new(v.x, v.bottom())
                    });
                if let Some(m) = Self::menu(model, t, f, at) {
                    cx.request(m);
                } else if let Some(a) = Self::field_action(model, t, f) {
                    cx.emit(a);
                }
            }
            Hit::Segment(Field::Monitor, i) => {
                if let Some(mode) = MONITOR_MODES.get(i) {
                    cx.emit(Action::Edit(Command::SetTrackMonitor {
                        track: t.id,
                        mode: *mode,
                    }));
                }
            }
            Hit::Segment(..) => {}
            Hit::AddDevice => cx.emit(chain::add(model, t)),
            Hit::Power(i) => cx.emit(chain::toggle_bypass(t.id, &devices[i])),
            Hit::Remove(i) => {
                if let Some(a) = chain::remove(t, &devices[i]) {
                    cx.emit(a);
                }
            }
            Hit::Device(i) => {
                if mods.alt {
                    if let Some(a) = chain::remove(t, &devices[i]) {
                        cx.emit(a);
                    }
                } else {
                    self.drag = Some(Drag {
                        device: i,
                        origin: pos,
                        pos,
                        moved: false,
                    });
                }
            }
            Hit::Strip => {
                let _ = clicks;
                return false;
            }
        }
        true
    }

    /// The gap (between device lines) a dragged device would land in.
    fn gap_at(&self, y: f32, model: &Session, t: &Track, size: Size) -> usize {
        self.layout(model, t, size)
            .devices
            .iter()
            .filter(|(_, r)| r.center().y < y)
            .count()
    }

    fn release(
        &mut self,
        pos: Point,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(d) = self.drag.take() else {
            return false;
        };
        cx.set_cursor(Cursor::Default);
        cx.redraw();
        let Some(t) = Self::track(model) else {
            return true;
        };
        let devices = chain::devices(model, t);
        let Some(device) = devices.get(d.device) else {
            return true;
        };
        if !d.moved {
            cx.emit(if mods.toggle() {
                chain::toggle_bypass(t.id, device)
            } else {
                chain::open(t.id, device)
            });
            return true;
        }
        let gap = self.gap_at(pos.y, model, t, size);
        if let Some(a) = chain::drop_index(&devices, device, gap)
            .and_then(|index| chain::move_to(t.id, device, index))
        {
            cx.emit(a);
        }
        true
    }

    fn secondary(
        &mut self,
        pos: Point,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let (Some(t), Some(hit)) = (Self::track(model), self.hit(pos, size, model)) else {
            return false;
        };
        let devices = chain::devices(model, t);
        let request = match hit {
            Hit::Color | Hit::Name | Hit::Header => Some(MixerView::track_menu(model, t, pos)),
            Hit::Device(i) | Hit::Power(i) | Hit::Remove(i) => {
                Some(crate::overview::device_menu(model, t, &devices[i], pos))
            }
            Hit::AddDevice => Some(MixerView::insert_menu(model, t, t.inserts.len(), pos)),
            Hit::Field(f) => Self::menu(model, t, f, pos),
            _ => None,
        };
        match request {
            Some(r) => {
                cx.request(r);
                true
            }
            None => false,
        }
    }

    // --- painting --------------------------------------------------------------

    fn paint_header(&self, p: &mut dyn Painter, size: Size, t: &Track, model: &Session) {
        let th = &self.theme;
        let band = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(band, th.piano.toolbar);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        let (color, name) = Self::header_rects(size);
        let c = faderframe_view_mixer::track_color(t.color);
        p.fill_rounded(color, 3.0, &Paint::Solid(c));
        if self.hover == Some(Hit::Color) {
            p.stroke_rounded(color.inset(-1.5), 3.0, 1.0, th.ui.text);
        }
        if self.hover == Some(Hit::Name) {
            p.fill_rounded(name, 3.0, &Paint::Solid(th.ui.text.with_alpha(0.07)));
        }
        p.text(
            &t.name,
            name.inset_xy(3.0, 0.0),
            &TextStyle::new(th.fonts.normal + 1.0, th.ui.text).bold(),
        );
        let number = model
            .project()
            .folder_order()
            .into_iter()
            .filter(|x| !matches!(x.kind, TrackKind::Master | TrackKind::Folder))
            .position(|x| x.id == t.id);
        let mut line = t.kind_label().to_string();
        if t.kind.has_audio() {
            line += &format!(" · {}", layout_name(t.chain_layout()));
        }
        if let Some(n) = number {
            line += &format!(" · Track {}", n + 1);
        }
        let n = chain::selected_count(model);
        if n > 1 {
            line += &format!(" · 1 of {n} selected");
        }
        p.text(
            &line,
            Rect::new(name.x + 3.0, name.bottom(), name.w - 3.0, 18.0),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
    }

    fn paint_section(&self, p: &mut dyn Painter, s: Section, r: Rect, t: &Track, model: &Session) {
        let th = &self.theme;
        let hot = self.hover == Some(Hit::Section(s));
        p.fill(
            r,
            if hot {
                th.ui.surface_alt
            } else {
                th.ui.surface
            },
        );
        p.hline(r.x, r.right(), r.bottom() - 0.5, th.ui.border);
        let folded = self.is_folded(s);
        let mut title = format!("{} {}", if folded { "▸" } else { "▾" }, s.title());
        if s == Section::Devices {
            let devices = chain::devices(model, t);
            if !devices.is_empty() {
                title += &format!(" ({})", devices.len());
            }
            let total = chain::total_latency(&devices);
            if total > 0 {
                title += &format!(
                    " · {}",
                    chain::latency_text(total, model.project().sample_rate)
                );
            }
        }
        p.text(
            &title,
            Rect::new(r.x + PAD, r.y, r.w - 2.0 * PAD, r.h),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim).bold(),
        );
        if s == Section::Devices && chain::holds_devices(t) {
            let add = Self::add_rect(r);
            let hot = self.hover == Some(Hit::AddDevice);
            p.fill_rounded(
                add,
                3.0,
                &Paint::Solid(if hot {
                    th.ui.accent.with_alpha(0.35)
                } else {
                    th.ui.text.with_alpha(0.07)
                }),
            );
            p.text(
                "+",
                add,
                &TextStyle::new(th.fonts.normal, th.ui.text).bold().center(),
            );
        }
    }

    fn paint_row(&self, p: &mut dyn Painter, row: &Row, r: Rect) {
        let th = &self.theme;
        p.text(
            row.label,
            Rect::new(r.x + PAD, r.y, LABEL_W - 4.0, r.h),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
        let value = Self::value_rect(r);
        let hot = self.hover == Some(Hit::Field(row.field));
        match &row.kind {
            Kind::Segments(segs) => {
                for (i, ((label, on), s)) in segs
                    .iter()
                    .zip(Self::segment_rects(value, segs.len()))
                    .enumerate()
                {
                    let hot = self.hover == Some(Hit::Segment(row.field, i));
                    let bg = if *on {
                        th.ui.accent.with_alpha(0.45)
                    } else if hot {
                        th.ui.text.with_alpha(0.12)
                    } else {
                        th.ui.text.with_alpha(0.05)
                    };
                    p.fill_rounded(s, 3.0, &Paint::Solid(bg));
                    p.text(
                        label,
                        s,
                        &TextStyle::new(
                            th.fonts.small,
                            if *on { th.ui.text } else { th.ui.text_dim },
                        )
                        .center(),
                    );
                }
            }
            Kind::Toggle(on) => {
                let check = Rect::new(value.x, value.y + (value.h - 14.0) / 2.0, 14.0, 14.0);
                p.fill_rounded(
                    check,
                    3.0,
                    &Paint::Solid(if *on {
                        th.ui.accent
                    } else {
                        th.ui.text.with_alpha(if hot { 0.16 } else { 0.07 })
                    }),
                );
                p.stroke_rounded(check, 3.0, 1.0, th.ui.border);
                if *on {
                    p.text(
                        "✓",
                        check,
                        &TextStyle::new(th.fonts.small, th.ui.background)
                            .bold()
                            .center(),
                    );
                }
                p.text(
                    &row.value,
                    Rect::new(check.right() + 6.0, value.y, value.w - 20.0, value.h),
                    &TextStyle::new(th.fonts.small, th.ui.text),
                );
            }
            Kind::Menu | Kind::Button | Kind::Info => {
                if row.kind != Kind::Info {
                    p.fill_rounded(
                        value,
                        3.0,
                        &Paint::Solid(th.ui.text.with_alpha(if hot { 0.12 } else { 0.05 })),
                    );
                }
                let ink = if row.kind == Kind::Info {
                    th.ui.text_dim
                } else {
                    th.ui.text
                };
                let arrow = if row.kind == Kind::Menu { 14.0 } else { 0.0 };
                p.text(
                    &row.value,
                    Rect::new(value.x + 5.0, value.y, value.w - 8.0 - arrow, value.h),
                    &TextStyle::new(th.fonts.small, ink),
                );
                if row.kind == Kind::Menu {
                    p.text(
                        "▾",
                        Rect::new(value.right() - 14.0, value.y, 12.0, value.h),
                        &TextStyle::new(th.fonts.tiny, th.ui.text_dim).center(),
                    );
                }
            }
        }
    }

    fn role_color(&self, role: Role) -> Color {
        use faderframe_ui_canvas::Accent;
        let d = &self.theme.device;
        match role {
            Role::Instrument => d.accent(Accent::Instrument),
            Role::MidiEffect => d.accent(Accent::Midi),
            Role::InputStage => d.accent(Accent::Saturation),
            Role::Container => d.accent(Accent::Modulation),
            Role::Effect => self.theme.ui.accent,
        }
    }

    fn paint_device(
        &self,
        p: &mut dyn Painter,
        i: usize,
        r: Rect,
        d: &Device,
        t: &Track,
        model: &Session,
    ) {
        let th = &self.theme;
        let c = &th.console;
        let lifted = self.drag.is_some_and(|g| g.moved && g.device == i);
        let hot = self.hover == Some(Hit::Device(i)) || lifted;
        p.fill_rounded(
            r,
            4.0,
            &Paint::Solid(if hot {
                th.ui.surface_alt
            } else {
                th.ui.surface
            }),
        );
        p.stroke_rounded(
            r,
            4.0,
            1.0,
            if d.failed { c.led.record } else { th.ui.border },
        );
        let stripe = self.role_color(d.role);
        p.fill_rounded(
            Rect::new(r.x + 1.0, r.y + 2.0, 3.0, r.h - 4.0),
            1.5,
            &Paint::Solid(if d.bypass {
                stripe.with_alpha(0.3)
            } else {
                stripe
            }),
        );
        let (power, remove) = Self::device_buttons(r);
        faderframe_ui_canvas::controls::led_button(p, power, "⏻", !d.bypass, c.led.monitor, th);
        let removable = chain::remove(t, d).is_some();
        if removable && (hot || self.hover == Some(Hit::Remove(i))) {
            let bg = if self.hover == Some(Hit::Remove(i)) {
                th.ui.text.with_alpha(0.16)
            } else {
                th.ui.text.with_alpha(0.06)
            };
            p.fill_rounded(remove, 3.0, &Paint::Solid(bg));
            p.text(
                "×",
                remove,
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
        }
        // Badges right to left: failure, latency, gain reduction.
        let mut right = remove.x - 4.0;
        let mut badge = |p: &mut dyn Painter, text: &str, color: Color| {
            let style = TextStyle::new(th.fonts.tiny, color).bold();
            let w = p.text_width(text, &style) + 8.0;
            let b = Rect::new(right - w, r.y + (r.h - 13.0) / 2.0, w, 13.0);
            p.fill_rounded(b, 3.0, &Paint::Solid(color.with_alpha(0.16)));
            p.text(text, b, &style.center());
            right -= w + 3.0;
        };
        if d.failed {
            badge(p, "⚠", c.led.record);
        }
        if let Some(l) = d.latency {
            badge(
                p,
                &chain::latency_text(l, model.project().sample_rate),
                th.ui.text_dim,
            );
        }
        if let Some(gr) = d.reduction.filter(|g| *g > 0.05) {
            badge(p, &format!("GR {gr:.1}"), th.device.reduction);
        }
        let ink = if d.bypass {
            th.ui.text_faint
        } else {
            th.ui.text
        };
        let name = Rect::new(
            power.right() + 6.0,
            r.y,
            (right - power.right() - 8.0).max(0.0),
            r.h,
        );
        let style = TextStyle::new(th.fonts.small, ink).bold();
        let w = p.text_width(&d.name, &style);
        p.text(&d.name, name, &style);
        let tag = match d.role {
            Role::Effect => String::new(),
            Role::Container => {
                format!("{} chain{}", d.chains, if d.chains == 1 { "" } else { "s" })
            }
            other => other.tag().to_lowercase(),
        };
        if !tag.is_empty() && w + 10.0 < name.w {
            p.text(
                &tag,
                Rect::new(name.x + w + 6.0, r.y, name.w - w - 6.0, r.h),
                &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
            );
        }
    }

    fn paint_drop(&self, p: &mut dyn Painter, l: &Layout, model: &Session, t: &Track, size: Size) {
        let Some(d) = self.drag.filter(|d| d.moved) else {
            return;
        };
        let devices = chain::devices(model, t);
        let Some(device) = devices.get(d.device) else {
            return;
        };
        let th = &self.theme;
        let gap = self.gap_at(d.pos.y, model, t, size);
        let movable =
            chain::drop_index(&devices, device, gap).is_some_and(|i| device.insert != Some(i));
        let y = match (gap, l.devices.first(), l.devices.get(gap.wrapping_sub(1))) {
            (0, Some((_, first)), _) => first.y - 1.0,
            (_, _, Some((_, prev))) => prev.bottom() + 1.0,
            _ => return,
        };
        if movable {
            p.fill(
                Rect::new(PAD, y - 1.5, size.w - 2.0 * PAD, 3.0),
                th.ui.accent,
            );
        }
    }
}

/// A channel layout's name ("Mono", "Stereo", "5.1").
fn layout_name(l: faderframe_core::ChannelLayout) -> String {
    match l {
        faderframe_core::ChannelLayout::Mono => "Mono".into(),
        faderframe_core::ChannelLayout::Stereo => "Stereo".into(),
        faderframe_core::ChannelLayout::Surround(f) => f.name().to_string(),
        faderframe_core::ChannelLayout::Discrete(n) => format!("{n} channels"),
    }
}

/// The view laid out (view coordinates).
#[derive(Default)]
struct Layout {
    sections: Vec<(Section, Rect)>,
    rows: Vec<(Row, Rect)>,
    devices: Vec<(usize, Rect)>,
    strip: Option<Rect>,
    content_h: f32,
}

/// Move a tree of access nodes by (dx, dy).
fn moved(mut n: AccessNode<Action>, dx: f32, dy: f32) -> AccessNode<Action> {
    n.bounds = n.bounds.translate(dx, dy);
    n.children = n.children.into_iter().map(|c| moved(c, dx, dy)).collect();
    n
}

impl CanvasView<Session, Action> for InspectorView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.strip.set_theme(theme);
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = self.theme.clone();
        p.fill(Rect::from_size(size), th.ui.background);
        let Some(t) = Self::track(model) else {
            self.strip.show_track(None);
            p.fill(Rect::new(0.0, 0.0, size.w, HEADER_H), th.piano.toolbar);
            p.text(
                "INSPECTOR",
                Rect::new(PAD, 0.0, size.w - 2.0 * PAD, HEADER_H),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).bold(),
            );
            for (i, line) in [
                "No track selected.",
                "Click a track's header or strip",
                "to see its settings here.",
            ]
            .into_iter()
            .enumerate()
            {
                p.text(
                    line,
                    Rect::new(
                        PAD,
                        HEADER_H + 24.0 + 18.0 * i as f32,
                        size.w - 2.0 * PAD,
                        18.0,
                    ),
                    &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
                );
            }
            return;
        };
        if self.shown != Some(t.id) {
            self.shown = Some(t.id);
            self.scroll_y = 0.0;
        }
        self.strip.show_track(Some(t.id));
        let mut l = self.layout(model, t, size);
        let max = (l.content_h - size.h).max(0.0);
        if self.scroll_y > max {
            self.scroll_y = max;
            l = self.layout(model, t, size);
        }
        let body = Rect::new(0.0, HEADER_H, size.w, (size.h - HEADER_H).max(0.0));
        p.push_clip(body);
        for (s, r) in &l.sections {
            self.paint_section(p, *s, *r, t, model);
        }
        for (row, r) in &l.rows {
            if r.bottom() >= HEADER_H && r.y <= size.h {
                self.paint_row(p, row, *r);
            }
        }
        if !self.is_folded(Section::Devices) {
            let devices = chain::devices(model, t);
            if devices.is_empty()
                && let Some((_, title)) = l.sections.iter().find(|(s, _)| *s == Section::Devices)
            {
                let text = if chain::holds_devices(t) {
                    "No devices · + adds one"
                } else {
                    "This kind of track holds no devices"
                };
                p.text(
                    text,
                    Rect::new(PAD, title.bottom(), size.w - 2.0 * PAD, DEVICE_H),
                    &TextStyle::new(th.fonts.small, th.ui.text_faint),
                );
            }
            for (i, r) in &l.devices {
                if r.bottom() >= HEADER_H && r.y <= size.h {
                    self.paint_device(p, *i, *r, &devices[*i], t, model);
                }
            }
        }
        if let Some(s) = l.strip
            && s.bottom() >= HEADER_H
            && s.y <= size.h
        {
            p.push_clip(s);
            p.push_transform(s.x, s.y, 1.0);
            self.strip.paint(p, s.size(), model, theme);
            p.pop_transform();
            p.pop_clip();
        }
        self.paint_drop(p, &l, model, t, size);
        p.pop_clip();
        self.paint_header(p, size, t, model);
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let strip = Self::track(model).and_then(|t| self.layout(model, t, size).strip);
        // A drag that began on the strip stays the strip's.
        if self.strip_grab
            && let Some(s) = strip
        {
            if let ViewEvent::PointerUp { .. } | ViewEvent::FocusLost = ev {
                self.strip_grab = false;
            }
            return self.strip_event(ev, s, model, cx);
        }
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button,
                modifiers,
                clicks,
            } => {
                if let Some(s) = strip.filter(|s| s.contains(pos) && pos.y >= HEADER_H) {
                    self.strip_grab = button == PointerButton::Primary;
                    return self.strip_event(ev, s, model, cx);
                }
                match button {
                    PointerButton::Primary => self.press(pos, modifiers, clicks, size, model, cx),
                    PointerButton::Secondary => self.secondary(pos, size, model, cx),
                    _ => false,
                }
            }
            ViewEvent::PointerMove { pos, .. } if self.drag.is_some() => {
                if let Some(d) = &mut self.drag {
                    d.moved = d.moved || pos.distance(d.origin) >= 4.0;
                    d.pos = pos;
                    if d.moved {
                        cx.set_cursor(Cursor::Grabbing);
                    }
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let hover = self.hit(pos, size, model);
                if let (Some(Hit::Strip), Some(s)) = (hover, strip) {
                    if self.hover.take().is_some() {
                        cx.redraw();
                    }
                    return self.strip_event(ev, s, model, cx);
                }
                if self.hover == Some(Hit::Strip) || hover != self.hover {
                    if let Some(s) = strip {
                        self.strip_event(&ViewEvent::PointerLeave, s, model, cx);
                    }
                    self.hover = hover;
                    cx.set_cursor(match hover {
                        None | Some(Hit::Header) => Cursor::Default,
                        Some(Hit::Device(_)) => Cursor::Grab,
                        Some(_) => Cursor::Pointer,
                    });
                    cx.redraw();
                }
                false
            }
            ViewEvent::PointerUp {
                pos,
                button: PointerButton::Primary,
                modifiers,
            } => self.release(pos, modifiers, size, model, cx),
            ViewEvent::Key {
                key: faderframe_ui_canvas::Key::Escape,
                ..
            }
            | ViewEvent::FocusLost
                if self.drag.is_some() =>
            {
                self.drag = None;
                cx.set_cursor(Cursor::Default);
                cx.redraw();
                true
            }
            ViewEvent::PointerLeave => {
                if let Some(s) = strip {
                    self.strip_event(ev, s, model, cx);
                }
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll {
                pos, dy, precise, ..
            } => {
                // The wheel on the strip's fader, knobs and sends moves
                // them; anywhere else it scrolls the Inspector.
                if let Some(s) = strip.filter(|s| s.contains(pos)) {
                    use faderframe_view_mixer::Hit as M;
                    let inner = Point::new(pos.x - s.x, pos.y - s.y);
                    if matches!(
                        self.strip.hit_test(inner, s.size(), model),
                        Some(
                            M::FaderCap(_)
                                | M::FaderTrack(_)
                                | M::Pan(_)
                                | M::DualPan(..)
                                | M::Send(..)
                                | M::PreampKnob(..)
                        )
                    ) {
                        return self.strip_event(ev, s, model, cx);
                    }
                }
                let step = if precise { 1.0 } else { 40.0 };
                self.scroll_y = (self.scroll_y + dy * step).max(0.0);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.is_animating() || self.strip.wants_frames(model)
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let t = Self::track(model)?;
        let hit = self.hit(pos, size, model)?;
        if hit == Hit::Strip {
            let s = self.layout(model, t, size).strip?;
            return self
                .strip
                .tooltip(Point::new(pos.x - s.x, pos.y - s.y), s.size(), model);
        }
        let devices = chain::devices(model, t);
        Some(match hit {
            Hit::Color => "Its colour · Click to choose".into(),
            Hit::Name => "Click to rename · Right-click: the track's menu".into(),
            Hit::Header => "Right-click: the track's menu".into(),
            Hit::Section(s) => format!(
                "{} {}",
                if self.is_folded(s) { "Show" } else { "Fold" },
                s.title().to_lowercase()
            ),
            Hit::Field(f) => match f {
                Field::Record | Field::Mpe | Field::Object | Field::Contents => {
                    "Click to switch".into()
                }
                Field::FolderGroup => "Click to switch · When on, arming, selecting and editing \
                                       clips or a range on one of its tracks reach the others"
                    .into(),
                Field::Ripple => "Click to switch · Left out, ripples of edits on other tracks \
                                  never move it (a music bed, a reference); its own edits \
                                  still ripple"
                    .into(),
                Field::FolderBus => {
                    "Click to switch: a folder bus sums the tracks it holds (as the \
                     folder's menu's Folder Is a Bus)"
                        .into()
                }
                Field::Surround => "The surround panner".into(),
                Field::Freeze => "Render the track and play the render (or back)".into(),
                _ => "Click to choose".into(),
            },
            Hit::Segment(..) => "Monitoring: never, tape-style (armed or selected), always".into(),
            Hit::AddDevice => {
                "Add a device after the last (the plugin browser) · Right-click: the quick list"
                    .into()
            }
            Hit::Device(i) => format!(
                "{} ({}, {}) · Click: editor · Ctrl+click: bypass · Alt+click: remove · Drag: move · Right-click: more",
                devices[i].name,
                devices[i].role.tag().to_lowercase(),
                devices[i].format
            ),
            Hit::Power(i) => format!(
                "{} · Click to {}",
                devices[i].name,
                if devices[i].bypass {
                    "switch it on"
                } else {
                    "bypass it"
                }
            ),
            Hit::Remove(i) => format!("Remove {}", devices[i].name),
            Hit::Strip => return None,
        })
    }

    fn min_size(&self) -> Size {
        Size::new(200.0, 200.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Vertical {
            return None;
        }
        let t = Self::track(model)?;
        let l = self.layout(model, t, size);
        Some(ScrollInfo {
            content: (l.content_h - HEADER_H).max(0.0),
            viewport: (size.h - HEADER_H).max(0.0),
            offset: self.scroll_y,
            start: HEADER_H,
            end: 0.0,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Vertical {
            self.scroll_y = offset.max(0.0);
        }
    }

    fn accessible(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        let Some(t) = Self::track(model) else {
            return vec![
                AccessNode::new(
                    access_id(&[NODE_VIEW]),
                    AccessRole::Label,
                    "Inspector: no track selected",
                )
                .at(Rect::from_size(size)),
            ];
        };
        let id = t.id.0;
        let (color, name) = Self::header_rects(size);
        let mut root = AccessNode::new(
            access_id(&[NODE_VIEW, id]),
            AccessRole::Group,
            format!("Inspector: {} ({})", t.name, t.kind_label()),
        )
        .at(Rect::from_size(size))
        .child(
            AccessNode::new(
                access_id(&[NODE_NAME, id]),
                AccessRole::Label,
                t.name.clone(),
            )
            .at(name),
        )
        .child(
            AccessNode::new(access_id(&[NODE_COLOR, id]), AccessRole::Button, "Colour")
                .at(color)
                .on_activate(Action::PickColor(faderframe_session::ColorTarget::Track(
                    t.id,
                ))),
        );
        let l = self.layout(model, t, size);
        let devices = chain::devices(model, t);
        for (s, r) in &l.sections {
            let mut sec = AccessNode::new(
                access_id(&[NODE_SECTION, id, *s as u64]),
                AccessRole::Group,
                s.title().to_lowercase(),
            )
            .at(*r);
            for (row, rr) in l.rows.iter().filter(|(row, _)| {
                Self::rows(model, t, *s)
                    .iter()
                    .any(|x| x.field == row.field)
            }) {
                sec = sec.child(self.row_node(model, t, row, *rr));
            }
            if *s == Section::Devices {
                if chain::holds_devices(t) {
                    sec = sec.child(
                        AccessNode::new(
                            access_id(&[NODE_ADD, id]),
                            AccessRole::Button,
                            "Add device",
                        )
                        .at(Self::add_rect(*r))
                        .on_activate(chain::add(model, t)),
                    );
                }
                for (i, dr) in &l.devices {
                    let (power, remove) = Self::device_buttons(*dr);
                    sec = sec.child(crate::overview::device_node(
                        t,
                        *i,
                        &devices[*i],
                        *dr,
                        power,
                        remove,
                    ));
                }
            }
            if *s == Section::Channel
                && let Some(sr) = l.strip
            {
                sec = sec.with_children(
                    self.strip
                        .accessible(sr.size(), model)
                        .into_iter()
                        .map(|n| moved(n, sr.x, sr.y)),
                );
            }
            root = root.child(sec);
        }
        vec![root]
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Inspector".into())
    }
}

impl InspectorView {
    /// A line for screen readers: its label and value; a menu's choices
    /// as its children (each a toggle Enter chooses), a switch as itself.
    fn row_node(&self, model: &Session, t: &Track, row: &Row, r: Rect) -> AccessNode<Action> {
        let rid = access_id(&[NODE_ROW, t.id.0, row.field as u64]);
        match &row.kind {
            Kind::Toggle(on) => AccessNode::new(rid, AccessRole::CheckBox, row.label)
                .checked(*on)
                .described(row.value.clone())
                .at(r)
                .on_activate_opt(Self::field_action(model, t, row.field)),
            Kind::Segments(segs) => {
                let mut n = AccessNode::new(rid, AccessRole::Group, row.label).at(r);
                for (i, ((label, on), s)) in segs
                    .iter()
                    .zip(Self::segment_rects(Self::value_rect(r), segs.len()))
                    .enumerate()
                {
                    let mut c = AccessNode::new(
                        access_id(&[NODE_CHOICE, t.id.0, row.field as u64, i as u64]),
                        AccessRole::ToggleButton,
                        format!("{}: {label}", row.label),
                    )
                    .checked(*on)
                    .at(s);
                    if let Some(mode) = MONITOR_MODES.get(i) {
                        c = c.on_activate(Action::Edit(Command::SetTrackMonitor {
                            track: t.id,
                            mode: *mode,
                        }));
                    }
                    n = n.child(c);
                }
                n
            }
            Kind::Button => AccessNode::new(
                rid,
                AccessRole::Button,
                format!("{}: {}", row.label, row.value),
            )
            .at(r)
            .on_activate_opt(Self::field_action(model, t, row.field)),
            Kind::Info => AccessNode::new(
                rid,
                AccessRole::Label,
                format!("{}: {}", row.label, row.value),
            )
            .at(r),
            Kind::Menu => {
                let mut n = AccessNode::new(
                    rid,
                    AccessRole::Group,
                    format!("{}: {}", row.label, row.value),
                )
                .at(r);
                if let Some(HostRequest::ContextMenu { items, .. }) =
                    Self::menu(model, t, row.field, Point::new(r.x, r.bottom()))
                {
                    for (i, item) in flatten(items).into_iter().enumerate() {
                        let Some(action) = item.action else { continue };
                        let mut c = AccessNode::new(
                            access_id(&[NODE_CHOICE, t.id.0, row.field as u64, i as u64]),
                            if item.checked.is_some() {
                                AccessRole::ToggleButton
                            } else {
                                AccessRole::Button
                            },
                            item.label,
                        )
                        .at(Self::value_rect(r))
                        .on_activate(action);
                        if let Some(on) = item.checked {
                            c = c.checked(on);
                        }
                        n = n.child(c);
                    }
                }
                n
            }
        }
    }
}

/// A menu's entries with their submenus' in line.
fn flatten(items: Vec<MenuItem<Action>>) -> Vec<MenuItem<Action>> {
    let mut out = Vec::new();
    for mut item in items {
        let children = std::mem::take(&mut item.children);
        if item.action.is_some() {
            out.push(item);
        }
        out.extend(flatten(children));
    }
    out
}

/// `on_activate` when there is something to do.
trait ActivateOpt {
    fn on_activate_opt(self, action: Option<Action>) -> Self;
}

impl ActivateOpt for AccessNode<Action> {
    fn on_activate_opt(self, action: Option<Action>) -> Self {
        match action {
            Some(a) => self.on_activate(a),
            None => self,
        }
    }
}
