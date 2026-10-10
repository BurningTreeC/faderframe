//! The analogue-console mixer surface.
//!
//! A single custom-rendered view draws every channel strip (no widget per
//! knob or fader). Strips are virtualised horizontally: only strips that
//! intersect the viewport are laid out and painted, so sessions with
//! hundreds of channels cost the same per frame as a dozen. The master strip
//! is pinned to the right edge.
//!
//! The view owns presentation state only (scroll offset, the active drag,
//! hover). All changes go out as [`Action`]s; continuous gestures are
//! wrapped in `BeginGesture`/`EndGesture` so a whole fader move is one undo
//! step.

#![forbid(unsafe_code)]

mod access;
mod layout;
mod preamp;

pub use layout::{INSERT_SLOT_STEP, MAX_SEND_ROWS, SENDS_PER_ROW, StripLayout};

use faderframe_core::gain::{SILENCE_DB, format_db};
use faderframe_core::pan::{format_pan, parse_pan};
use faderframe_core::{FaderLaw, TrackId};
use faderframe_project::{
    Command, InputRouting, MonitorMode, OutputRouting, SendTap, Track, TrackColor, TrackKind,
};
use faderframe_session::MeterMode;
use faderframe_session::{Action, MeterDisplay, SelectMode, Session};
use faderframe_ui_canvas::controls::{self, FaderGeometry, KnobLook, MeterLevel};
use faderframe_ui_canvas::{
    Align, CanvasView, Color, Cursor, EventCx, HostRequest, MenuItem, Painter, Point,
    PointerButton, Rect, ScrollAxis, ScrollInfo, Size, Theme, ViewEvent,
};

/// The widths a strip's menu offers (`None`: the theme's).
const STRIP_WIDTHS: [(&str, Option<f32>); 4] = [
    ("Narrow", Some(64.0)),
    ("Normal", None),
    ("Wide", Some(130.0)),
    ("Extra Wide", Some(190.0)),
];

/// The column right of the last strip with the "+".
const ADD_W: f32 = 44.0;
const MASTER_GAP: f32 = 8.0;
/// Wooden end cheeks (themes with wood).
const CHEEK_W: f32 = 22.0;
/// A folder's strip: narrow (its triangle, mute, solo, name).
const FOLDER_W: f32 = 48.0;

/// The gain-reduction stripe: left of the meter, as tall as its scale.
fn reduction_rect(l: &StripLayout) -> Rect {
    Rect::new(
        l.meter.x - 5.0,
        l.meter.y + 8.0,
        3.0,
        (l.meter.h - 10.0).max(0.0),
    )
}

/// How fast the gain-reduction stripe falls back (per painted frame).
const REDUCTION_FALL: f32 = 0.88;

/// Where a dragged strip lands.
#[derive(Clone, Copy, Debug, PartialEq)]
enum StripDrop {
    /// Between two strips (as `Action::PlaceTrack` takes them), the line at
    /// `x`.
    Between {
        after: Option<TrackId>,
        before: Option<TrackId>,
        x: f32,
    },
    /// Onto a folder's strip: into it.
    Into { folder: TrackId, strip: Rect },
}

/// A folder's strip, laid out.
struct FolderLayout {
    color_bar: Rect,
    fold: Rect,
    mute: Rect,
    solo: Rect,
    count: Rect,
    scribble: Rect,
}

fn folder_layout(rect: Rect) -> FolderLayout {
    let (x, w) = (rect.x + 5.0, rect.w - 10.0);
    FolderLayout {
        color_bar: Rect::new(rect.x + 2.0, rect.y + 2.0, rect.w - 4.0, 7.0),
        fold: Rect::new(x, rect.y + 16.0, w, 24.0),
        mute: Rect::new(x, rect.y + 48.0, w, 18.0),
        solo: Rect::new(x, rect.y + 70.0, w, 18.0),
        count: Rect::new(rect.x + 2.0, rect.y + 94.0, rect.w - 4.0, 28.0),
        scribble: Rect::new(rect.x + 2.0, rect.bottom() - 24.0, rect.w - 4.0, 20.0),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    PreampChoose(TrackId),
    PreampRemove(TrackId),
    PreampKnob(TrackId, u32),
    FaderCap(TrackId),
    FaderTrack(TrackId),
    Pan(TrackId),
    /// The pan value under the knob (click to type).
    PanValue(TrackId),
    /// A send slot; the index is into the track's sends (bank applied).
    Send(TrackId, usize),
    /// Page the send slots by this many banks.
    SendBank(i32),
    Insert(TrackId, usize),
    Mute(TrackId),
    Solo(TrackId),
    Record(TrackId),
    /// The master's mono check (listening only).
    MonoCheck,
    Phase(TrackId),
    Monitor(TrackId),
    Input(TrackId),
    Output(TrackId),
    Level(TrackId),
    Scribble(TrackId),
    /// A folder's triangle: opens or closes it.
    Fold(TrackId),
    Meter(TrackId),
    /// The gain-reduction stripe left of the meter (shows, does nothing).
    Reduction(TrackId),
    Strip(TrackId),
    /// The rule under the inserts: drag to show more or fewer slots.
    InsertsGrip(TrackId),
    /// The group/VCA tags (click for the group and VCA menu).
    Tags(TrackId),
    /// The colour bar on top: opens the colour chooser.
    Color(TrackId),
    /// The "+" right of the last channel strip.
    AddTrack,
    /// A strip's right edge: drag to make it wider or narrower.
    Width(TrackId),
    /// A MIDI track's instrument: the track it plays.
    Plays(TrackId),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum KnobTarget {
    Preamp(u32),
    Pan,
    Send(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Track {
        track: TrackId,
        origin: Point,
        pos: Point,
        moved: bool,
    },
    Fader {
        track: TrackId,
        start_y: f32,
        start_pos: f32,
    },
    Knob {
        track: TrackId,
        target: KnobTarget,
        start_y: f32,
        start_value: f32,
    },
    /// A track's place in the surround bed it feeds (the strip's mini
    /// panner: moved by as much as the pointer, `size` the room's side).
    Surround {
        track: TrackId,
        start: Point,
        from: faderframe_core::SurroundPan,
        size: f32,
    },
    Scroll {
        start_x: f32,
        start_scroll: f32,
    },
    /// Resizing the inserts section (all strips).
    InsertSlots {
        start_y: f32,
        start: usize,
    },
    /// Resizing a strip (`all`: every strip).
    Width {
        track: TrackId,
        start_x: f32,
        start: f32,
        all: bool,
    },
    /// An insert pressed: a click on release, or dragged to another slot
    /// (reorder; another track: copy, Shift: move; Ctrl: duplicate).
    Insert {
        track: TrackId,
        plugin: faderframe_core::PluginInstanceId,
        origin: Point,
        pos: Point,
        moved: bool,
    },
}

pub struct MixerView {
    theme: Theme,
    /// The skin as chosen (the mixer's own theme is it, or it with the
    /// console's look), and the console family whose look is shown.
    base_theme: Theme,
    look: Option<u8>,
    /// Each strip's gain reduction as shown (falling back by
    /// `REDUCTION_FALL` a frame).
    reduction: std::collections::HashMap<TrackId, f32>,
    scroll_x: f32,
    drag: Option<Drag>,
    hover: Option<Hit>,
    law: FaderLaw,
    /// Send rows shown (from the track with the most sends, +1 free slot).
    send_rows: usize,
    /// Send bank shown (pages of `send_rows * SENDS_PER_ROW` sends).
    send_bank: usize,
    /// Sends of the track with the most sends.
    max_sends: usize,
    /// Insert slots per strip (the session's layout setting).
    insert_slots: usize,
    /// The project has groups or VCAs: strips show a tag row.
    show_tags: bool,
    expanded_preamps: bool,
    /// Only the master strip, filling the view (the side panel).
    master_only: bool,
    /// Where each channel strip starts (content x) and, last, where they
    /// end; and each one's width.
    offsets: Vec<f32>,
    widths: Vec<f32>,
    /// The side panel shows the master: this mixer leaves it out.
    hide_master: bool,
}

fn fader_cap_color(kind: TrackKind, theme: &Theme) -> Color {
    let c = &theme.console;
    match kind {
        TrackKind::Bus => c.fader_cap_bus,
        TrackKind::Aux => c.fader_cap_aux,
        TrackKind::Master => c.fader_cap_master,
        _ => c.fader_cap_audio,
    }
}

pub fn track_color(c: TrackColor) -> Color {
    Color::rgb8(c.r, c.g, c.b)
}

/// A session menu entry as a menu item.
pub fn menu_item(e: faderframe_session::GroupMenuEntry) -> MenuItem<Action> {
    let mut m = match e.action {
        Some(a) => MenuItem::new(e.label, a),
        None => MenuItem::disabled(e.label),
    };
    if let Some(on) = e.checked {
        m = m.checked(on);
    }
    if e.separated {
        m = m.separated();
    }
    m
}

fn kind_tag(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Audio => "AUDIO",
        TrackKind::Instrument => "INST",
        TrackKind::Midi => "MIDI",
        TrackKind::Bus => "BUS",
        TrackKind::Aux => "AUX",
        TrackKind::Master => "MAIN",
        TrackKind::Vca => "VCA",
        TrackKind::Folder => "FOLDER",
    }
}

/// The keys a MIDI strip's key display shows (A0 to C8).
const KEYS_LO: u8 = 21;
const KEYS_HI: u8 = 108;

/// A MIDI strip's input well: its input row without the polarity button
/// (notes have none).
fn midi_input_rect(row: &layout::InputRow) -> Rect {
    Rect::new(
        row.input.x,
        row.input.y,
        row.phase.right() - row.input.x,
        row.input.h,
    )
}

/// Where a MIDI strip names the notes it plays (the pan's place).
fn notes_rect(l: &StripLayout) -> Rect {
    Rect::new(
        l.mute.x,
        l.pan_knob.y,
        l.record.right() - l.mute.x,
        l.pan_readout.bottom() - l.pan_knob.y,
    )
}

/// A MIDI strip's key display (the level readout's and fader's place).
fn keys_rect(l: &StripLayout) -> Rect {
    Rect::new(
        l.fader.x,
        l.level_readout.y,
        l.meter.right() - l.fader.x,
        l.fader.bottom() - l.level_readout.y,
    )
}

/// The sounding keys by name, lowest first, as many as fit `width`
/// ("C4 E4 +2").
fn note_names(keys: u128, width: f32) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let names: Vec<String> = (0u8..128)
        .filter(|k| keys & (1u128 << k) != 0)
        .map(|k| format!("{}{}", NAMES[usize::from(k % 12)], i32::from(k) / 12 - 1))
        .collect();
    if names.is_empty() {
        return "—".into();
    }
    // About 6 px a character in the readout's font.
    let fits = |n: usize| {
        let shown: usize = names[..n].iter().map(|s| s.len() + 1).sum();
        let more = if n < names.len() { 3 } else { 0 };
        (shown + more) as f32 * 6.0 <= width
    };
    let n = (1..=names.len()).rev().find(|&n| fits(n)).unwrap_or(1);
    let mut text = names[..n].join(" ");
    if n < names.len() {
        text += &format!(" +{}", names.len() - n);
    }
    text
}

fn parse_db(text: &str) -> Option<f32> {
    let t = text.trim().trim_end_matches("dB").trim();
    if t.eq_ignore_ascii_case("-inf") || t.eq_ignore_ascii_case("inf") || t == "-∞" {
        return Some(SILENCE_DB);
    }
    t.parse::<f32>().ok().filter(|v| v.is_finite())
}

impl MixerView {
    pub fn new(theme: Theme) -> Self {
        Self {
            base_theme: theme.clone(),
            look: None,
            reduction: std::collections::HashMap::new(),
            theme,
            scroll_x: 0.0,
            drag: None,
            hover: None,
            law: FaderLaw::console(),
            send_rows: 1,
            send_bank: 0,
            max_sends: 0,
            insert_slots: faderframe_session::DEFAULT_INSERT_SLOTS as usize,
            show_tags: false,
            expanded_preamps: false,
            master_only: false,
            hide_master: false,
            offsets: vec![0.0],
            widths: Vec::new(),
        }
    }

    /// The master strip alone, filling the view: the panel at the window's
    /// right edge.
    pub fn master_only(theme: Theme) -> Self {
        Self {
            master_only: true,
            ..Self::new(theme)
        }
    }

    fn pitch(&self) -> f32 {
        self.theme.console.strip_width + self.theme.console.strip_gap
    }

    /// The channel strips: in folder order (a folder's strip, then its
    /// tracks'), the tracks of closed folders left out.
    fn channel_tracks(model: &Session) -> Vec<&Track> {
        model
            .project()
            .folder_order()
            .into_iter()
            .filter(|t| t.kind != TrackKind::Master && model.track_shown(t))
            .collect()
    }

    /// Width of the wooden end cheeks (0 without wood).
    fn cheek(&self) -> f32 {
        if self.theme.console.look.wood.is_some() {
            CHEEK_W
        } else {
            0.0
        }
    }

    fn master_rect(&self, size: Size) -> Rect {
        let cheek = self.cheek();
        if self.master_only {
            return Rect::new(cheek, 0.0, (size.w - 2.0 * cheek).max(0.0), size.h);
        }
        if self.hide_master {
            // Out of sight (and out of reach of the pointer).
            return Rect::new(size.w + MASTER_GAP + 1.0, 0.0, 0.0, size.h);
        }
        let w = self.theme.console.master_width;
        Rect::new(size.w - w - cheek, 0.0, w, size.h)
    }

    fn viewport_w(&self, size: Size) -> f32 {
        let cheeks = 2.0 * self.cheek();
        if self.master_only {
            0.0
        } else if self.hide_master {
            (size.w - cheeks).max(0.0)
        } else {
            (size.w - self.theme.console.master_width - MASTER_GAP - cheeks).max(0.0)
        }
    }

    /// Lay the strips out at their widths (before painting and events).
    fn update_strips(&mut self, model: &Session) {
        let gap = self.theme.console.strip_gap;
        let default = self.theme.console.strip_width;
        self.widths = Self::channel_tracks(model)
            .iter()
            .map(|t| {
                if t.kind == TrackKind::Folder {
                    FOLDER_W
                } else {
                    model.strip_width(t.id).unwrap_or(default)
                }
            })
            .collect();
        self.offsets.clear();
        let mut x = 0.0;
        self.offsets.push(x);
        for w in &self.widths {
            x += w + gap;
            self.offsets.push(x);
        }
    }

    /// Content x where strip `i` starts (past the end: after the last).
    fn offset(&self, i: usize) -> f32 {
        match self.offsets.get(i) {
            Some(x) => *x,
            None => {
                let last = self.offsets.len().saturating_sub(1);
                self.offsets.last().copied().unwrap_or(0.0) + (i - last) as f32 * self.pitch()
            }
        }
    }

    fn width(&self, i: usize) -> f32 {
        self.widths
            .get(i)
            .copied()
            .unwrap_or(self.theme.console.strip_width)
    }

    fn content_w(&self, count: usize) -> f32 {
        self.offset(count)
    }

    /// The "+" (add a track) right of the last of `count` strips.
    fn add_track_rect(&self, count: usize) -> Rect {
        let x = self.cheek() + self.content_w(count) - self.scroll_x;
        Rect::new(x + 6.0, 8.0, ADD_W - 12.0, ADD_W - 12.0)
    }

    fn clamp_scroll(&mut self, count: usize, size: Size) {
        let max = (self.content_w(count) + ADD_W - self.viewport_w(size)).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max);
    }

    /// Index range of strips intersecting the viewport.
    pub fn visible_range(&self, count: usize, size: Size) -> std::ops::Range<usize> {
        let (a, b) = (self.scroll_x, self.scroll_x + self.viewport_w(size));
        let first = (0..count)
            .find(|&i| self.offset(i + 1) > a)
            .unwrap_or(count);
        let last = (first..count)
            .find(|&i| self.offset(i) >= b)
            .unwrap_or(count);
        first..last
    }

    fn strip_rect(&self, index: usize, size: Size) -> Rect {
        Rect::new(
            self.cheek() + self.offset(index) - self.scroll_x,
            0.0,
            self.width(index),
            size.h,
        )
    }

    fn layout_for(
        &self,
        rect: Rect,
        t: &Track,
        project: &faderframe_project::Project,
    ) -> StripLayout {
        let vca = t.kind == TrackKind::Vca;
        // A strip feeding a bed meters each of its channels.
        let meters = match project.destination_layout(t) {
            faderframe_core::ChannelLayout::Surround(f) if t.kind.has_audio() => f.channels(),
            _ => 2,
        };
        // A MIDI strip keeps the rows of the others (its sections line up
        // with theirs): the preamp's place holds the instrument it plays,
        // the send rows stay empty.
        StripLayout::with_preamp(
            rect,
            &self.theme,
            matches!(
                t.kind,
                TrackKind::Audio | TrackKind::Instrument | TrackKind::Midi
            ) || t.input.plugin_output().is_some(),
            t.kind != TrackKind::Master && !vca,
            self.send_rows,
            if vca { 0 } else { self.insert_slots.max(1) },
            self.show_tags,
            if t.kind.has_audio() || t.kind == TrackKind::Midi {
                if self.expanded_preamps { 84.0 } else { 20.0 }
            } else {
                0.0
            },
        )
        .with_meter_channels(meters)
    }

    /// Size the send section for the track with the most sends (always
    /// leaving one free slot to add another).
    fn update_sends(&mut self, model: &Session) {
        self.expanded_preamps = model.project().tracks.iter().any(|t| t.preamp.is_some());
        self.hide_master = !self.master_only && model.master_panel();
        self.insert_slots = model.mixer_insert_slots();
        let p = model.project();
        self.show_tags = !p.groups.is_empty() || p.tracks.iter().any(|t| t.kind == TrackKind::Vca);
        self.max_sends = model
            .project()
            .tracks
            .iter()
            .map(|t| t.sends.len())
            .max()
            .unwrap_or(0);
        self.send_rows = (self.max_sends + 1)
            .div_ceil(SENDS_PER_ROW)
            .clamp(1, MAX_SEND_ROWS);
        let pages = self.send_pages(self.send_rows * SENDS_PER_ROW);
        self.send_bank = self.send_bank.min(pages - 1);
    }

    /// Banks needed to reach every send plus one free slot.
    fn send_pages(&self, per_page: usize) -> usize {
        (self.max_sends + 1).div_ceil(per_page.max(1)).max(1)
    }

    /// Strips with their rects (visible channels + master).
    fn visible_strips<'m>(&self, model: &'m Session, size: Size) -> Vec<(Rect, &'m Track)> {
        let tracks = Self::channel_tracks(model);
        let mut out: Vec<(Rect, &Track)> = self
            .visible_range(tracks.len(), size)
            .map(|i| (self.strip_rect(i, size), tracks[i]))
            .collect();
        if let Some(m) = model.project().master()
            && !self.hide_master
        {
            out.push((self.master_rect(size), m));
        }
        out
    }

    pub fn hit_test(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        let master = self.master_rect(size);
        let channels = Self::channel_tracks(model);
        let add = self.add_track_rect(channels.len());
        let inside = pos.x >= self.cheek() && pos.x < self.cheek() + self.viewport_w(size);
        if !self.master_only && add.contains(pos) && inside {
            return Some(Hit::AddTrack);
        }
        // A strip's right edge (below its colour bar).
        if !self.master_only && inside && pos.y > 4.0 {
            let gap = self.theme.console.strip_gap;
            for i in self.visible_range(channels.len(), size) {
                let edge = self.strip_rect(i, size).right();
                if pos.x >= edge - 3.0 && pos.x <= edge + gap + 3.0 {
                    return Some(Hit::Width(channels[i].id));
                }
            }
        }
        for (rect, t) in self.visible_strips(model, size) {
            if !rect.contains(pos) {
                continue;
            }
            // Channel strips are clipped by the master section.
            if t.kind != TrackKind::Master
                && (pos.x >= master.x - MASTER_GAP || pos.x < self.cheek())
            {
                continue;
            }
            if t.kind == TrackKind::Folder {
                let f = folder_layout(rect);
                let id = t.id;
                return Some(
                    [
                        (f.color_bar, Hit::Color(id)),
                        (f.fold, Hit::Fold(id)),
                        (f.mute, Hit::Mute(id)),
                        (f.solo, Hit::Solo(id)),
                        (f.scribble, Hit::Scribble(id)),
                    ]
                    .into_iter()
                    .find(|(r, _)| r.contains(pos))
                    .map_or(Hit::Scribble(id), |(_, h)| h),
                );
            }
            let l = self.layout_for(rect, t, model.project());
            let id = t.id;
            if t.kind == TrackKind::Midi {
                return Some(Self::midi_hit(&l, id, pos));
            }
            if let Some(area) = l.preamp
                && let Some(hit) = self.preamp_hit(area, t, pos)
            {
                return Some(hit);
            }
            let geo = FaderGeometry::new(l.fader, &self.theme);
            let pos_now = self.law.db_to_position(model.shown_volume_db(t));
            // VCAs have only a fader, mute and solo.
            let audio = (t.kind != TrackKind::Vca).then_some(());
            // A bed's strip has no pan.
            let pans =
                audio.filter(|_| !matches!(t.layout, faderframe_core::ChannelLayout::Surround(_)));
            let color = Rect::new(l.color_bar.x, l.color_bar.y, l.color_bar.w, 7.0);
            let checks: [(Option<Rect>, Hit); 15] = [
                (Some(color), Hit::Color(id)),
                (Some(geo.cap_rect(pos_now).inset(-2.0)), Hit::FaderCap(id)),
                (
                    model.gain_reduction(id).map(|_| reduction_rect(&l)),
                    Hit::Reduction(id),
                ),
                (Some(l.fader), Hit::FaderTrack(id)),
                (audio.map(|_| l.meter), Hit::Meter(id)),
                (pans.map(|_| l.pan_readout), Hit::PanValue(id)),
                (pans.map(|_| l.pan_knob), Hit::Pan(id)),
                (Some(l.mute), Hit::Mute(id)),
                (Some(l.solo), Hit::Solo(id)),
                (
                    audio.map(|_| l.record),
                    if t.kind == TrackKind::Master {
                        Hit::MonoCheck
                    } else {
                        Hit::Record(id)
                    },
                ),
                (Some(l.level_readout), Hit::Level(id)),
                (audio.map(|_| l.output), Hit::Output(id)),
                (Some(l.scribble), Hit::Scribble(id)),
                (l.input.map(|i| i.phase), Hit::Phase(id)),
                (l.input.map(|i| i.monitor), Hit::Monitor(id)),
            ];
            for (r, hit) in checks {
                if r.is_some_and(|r| r.contains(pos)) {
                    return Some(hit);
                }
            }
            if l.input.is_some_and(|i| i.input.contains(pos)) {
                return Some(Hit::Input(id));
            }
            if l.inserts_grip.is_some_and(|g| g.contains(pos)) {
                return Some(Hit::InsertsGrip(id));
            }
            if l.tags.is_some_and(|g| g.contains(pos)) {
                return Some(Hit::Tags(id));
            }
            if let Some(slots) = &l.inserts
                && let Some(i) = slots.iter().position(|r| r.contains(pos))
            {
                return Some(Hit::Insert(id, i));
            }
            if let Some(sends) = &l.sends {
                let per_page = sends.len();
                if self.send_pages(per_page) > 1 {
                    if l.send_prev.is_some_and(|r| r.contains(pos)) {
                        return Some(Hit::SendBank(-1));
                    }
                    if l.send_next.is_some_and(|r| r.contains(pos)) {
                        return Some(Hit::SendBank(1));
                    }
                }
                if let Some(i) = sends
                    .iter()
                    .position(|s| s.knob.contains(pos) || s.label.contains(pos))
                {
                    return Some(Hit::Send(id, self.send_bank * per_page + i));
                }
            }
            return Some(Hit::Strip(id));
        }
        None
    }

    /// What is under `pos` on a MIDI strip.
    fn midi_hit(l: &StripLayout, id: TrackId, pos: Point) -> Hit {
        let color = Rect::new(l.color_bar.x, l.color_bar.y, l.color_bar.w, 7.0);
        let checks: [(Option<Rect>, Hit); 9] = [
            (Some(color), Hit::Color(id)),
            (l.preamp, Hit::Plays(id)),
            (Some(l.mute), Hit::Mute(id)),
            (Some(l.solo), Hit::Solo(id)),
            (Some(l.record), Hit::Record(id)),
            (Some(l.output), Hit::Output(id)),
            (Some(l.scribble), Hit::Scribble(id)),
            (l.input.map(|i| i.monitor), Hit::Monitor(id)),
            (l.input.as_ref().map(midi_input_rect), Hit::Input(id)),
        ];
        if let Some((_, hit)) = checks
            .into_iter()
            .find(|(r, _)| r.is_some_and(|r| r.contains(pos)))
        {
            return hit;
        }
        if l.inserts_grip.is_some_and(|g| g.contains(pos)) {
            return Hit::InsertsGrip(id);
        }
        if l.tags.is_some_and(|g| g.contains(pos)) {
            return Hit::Tags(id);
        }
        if let Some(slots) = &l.inserts
            && let Some(i) = slots.iter().position(|r| r.contains(pos))
        {
            return Hit::Insert(id, i);
        }
        Hit::Strip(id)
    }

    /// The instrument tracks a MIDI track can play.
    fn plays_menu(model: &Session, t: &Track, at: Point) -> HostRequest<Action> {
        let items = model
            .midi_instrument_choices(t.id)
            .into_iter()
            .map(|c| {
                let item = MenuItem::new(c.label, c.action).checked(c.checked);
                if c.group_start {
                    item.separated()
                } else {
                    item
                }
            })
            .collect();
        HostRequest::ContextMenu { at, items }
    }

    /// The external MIDI device (and channel) a MIDI track plays.
    fn midi_out_menu(model: &Session, t: &Track, at: Point) -> HostRequest<Action> {
        let items = model
            .midi_output_choices(t.id)
            .into_iter()
            .map(|c| {
                let item = MenuItem::new(c.label, c.action).checked(c.checked);
                if c.group_start {
                    item.separated()
                } else {
                    item
                }
            })
            .collect();
        HostRequest::ContextMenu { at, items }
    }

    // --- painting --------------------------------------------------------------

    /// A folder's strip: its colour, its triangle (open or closed), mute
    /// and solo (they reach what it holds), how many tracks it holds, and
    /// its name.
    fn paint_folder_strip(&self, p: &mut dyn Painter, rect: Rect, t: &Track, model: &Session) {
        let th = &self.theme;
        let c = &th.console;
        let f = folder_layout(rect);
        controls::panel(
            p,
            rect,
            c.panel_top.darken(0.12),
            c.panel_bottom.darken(0.12),
            th,
        );
        let color = track_color(t.color);
        p.fill(f.color_bar, color);
        if model.selection.tracks.contains(&t.id) {
            p.stroke_rounded(rect.inset(1.0), 2.0, 1.5, c.selected_glow);
        }
        // The triangle: right when closed, down when open.
        let open = model.folder_open(t.id);
        p.fill_rounded(f.fold, 3.0, &faderframe_ui_canvas::Paint::Solid(c.well));
        let (cx, cy, r) = (f.fold.center().x, f.fold.center().y, 5.0);
        let tri = if open {
            [
                Point::new(cx - r, cy - r * 0.55),
                Point::new(cx + r, cy - r * 0.55),
                Point::new(cx, cy + r * 0.65),
            ]
        } else {
            [
                Point::new(cx - r * 0.55, cy - r),
                Point::new(cx - r * 0.55, cy + r),
                Point::new(cx + r * 0.65, cy),
            ]
        };
        let mut path = faderframe_ui_canvas::Path::new();
        path.move_to(tri[0]).line_to(tri[1]).line_to(tri[2]).close();
        p.fill_path(&path, color.lighten(0.2));
        controls::led_button(p, f.mute, "M", t.mute, c.led.mute, th);
        controls::led_button(p, f.solo, "S", t.solo, c.led.solo, th);
        let n = model
            .folder_contents(t.id)
            .iter()
            .filter(|id| {
                model
                    .project()
                    .track(**id)
                    .is_some_and(|x| x.kind != TrackKind::Folder)
            })
            .count();
        let style = faderframe_ui_canvas::TextStyle::new(th.fonts.tiny, c.panel_label).center();
        p.text(
            &n.to_string(),
            Rect::new(f.count.x, f.count.y, f.count.w, 14.0),
            &style.weight(faderframe_ui_canvas::FontWeight::Bold),
        );
        p.text(
            if n == 1 { "track" } else { "tracks" },
            Rect::new(f.count.x, f.count.y + 13.0, f.count.w, 12.0),
            &style,
        );
        controls::scribble(p, f.scribble, &t.name, color, th);
    }

    fn paint_strip(
        &self,
        p: &mut dyn Painter,
        rect: Rect,
        t: &Track,
        number: usize,
        model: &Session,
    ) {
        let th = &self.theme;
        let c = &th.console;
        let is_master = t.kind == TrackKind::Master;
        let l = self.layout_for(rect, t, model.project());
        let (top, bottom) = if is_master {
            (c.master_panel_top, c.master_panel_bottom)
        } else {
            (c.panel_top, c.panel_bottom)
        };
        controls::panel(p, rect, top, bottom, th);
        let color = track_color(t.color);
        p.fill(l.color_bar, color);
        if model.selection.tracks.contains(&t.id) {
            p.stroke_rounded(rect.inset(1.0), 2.0, 1.5, c.selected_glow);
        }
        for &y in &l.dividers {
            controls::section_line(p, rect.x + 4.0, rect.right() - 4.0, y, th);
        }

        if let Some(area) = l.preamp {
            self.paint_preamp(p, area, t, model);
        }

        // Header.
        if is_master {
            controls::screw(p, Point::new(rect.x + 8.0, l.header.center().y), 3.0, th);
            controls::screw(
                p,
                Point::new(rect.right() - 8.0, l.header.center().y),
                3.0,
                th,
            );
            controls::engraved(p, "MASTER", l.header, th, Align::Center);
        } else {
            controls::engraved(p, &format!("{number}"), l.header, th, Align::Start);
            controls::engraved(p, kind_tag(t.kind), l.header, th, Align::End);
        }
        if let Some(tags) = l.tags {
            self.paint_tags(p, tags, t, model);
        }
        if t.kind == TrackKind::Midi {
            self.paint_midi_strip(p, &l, t, model);
            return;
        }

        if let Some(row) = l.input {
            let label = match &t.input {
                InputRouting::None => "IN —".to_string(),
                InputRouting::Hardware { first_channel } => match t.layout.channel_count() {
                    1 => format!("IN {}", first_channel + 1),
                    n => format!("IN {}-{}", first_channel + 1, *first_channel as usize + n),
                },
                InputRouting::Midi { channel: None, .. } => "MIDI".to_string(),
                InputRouting::Midi {
                    channel: Some(c), ..
                } => format!("MIDI {}", c + 1),
                // A plugin's extra output: its name there.
                InputRouting::Plugin { plugin, bus } => model
                    .plugin_output_buses(*plugin)
                    .into_iter()
                    .find(|o| o.bus == *bus)
                    .map_or_else(|| "PLUG —".to_string(), |o| o.name.to_uppercase()),
            };
            controls::well_label(p, row.input, &label, t.input == InputRouting::None, th);
            controls::led_button(p, row.phase, "Ø", t.phase_invert, c.led.phase, th);
            let mon_label = match t.monitor {
                MonitorMode::Auto => "A",
                _ => "I",
            };
            controls::led_button(
                p,
                row.monitor,
                mon_label,
                t.monitor != MonitorMode::Off,
                c.led.monitor,
                th,
            );
        }

        self.paint_inserts(p, &l, t, model);

        if let (Some(label), Some(sends)) = (l.sends_label, &l.sends) {
            let per_page = sends.len();
            let pages = self.send_pages(per_page);
            let first = self.send_bank * per_page;
            if pages > 1 {
                let text = format!("SENDS {}–{}", first + 1, first + per_page);
                controls::engraved(p, &text, label, th, Align::Center);
                for (r, glyph, enabled) in [
                    (l.send_prev, "◂", self.send_bank > 0),
                    (l.send_next, "▸", self.send_bank + 1 < pages),
                ] {
                    if let Some(r) = r {
                        let col = if enabled {
                            c.panel_label
                        } else {
                            c.panel_label.with_alpha(0.3)
                        };
                        p.text(
                            glyph,
                            r,
                            &faderframe_ui_canvas::TextStyle::new(th.fonts.small, col).center(),
                        );
                    }
                }
            } else {
                controls::engraved(p, "SENDS", label, th, Align::Center);
            }
            for (i, slot) in sends.iter().enumerate() {
                let index = first + i;
                if index > t.sends.len() {
                    // Only the next free slot is offered for a new send.
                    continue;
                }
                let send = t.sends.get(index);
                let value =
                    send.map_or(0.0, |s| self.law.db_to_position(model.shown_send_db(t, s)));
                let ring = match send {
                    Some(s) if s.enabled => c.send_cap.lighten(0.35),
                    _ => c.knob.ring_track,
                };
                controls::knob(
                    p,
                    slot.knob,
                    value,
                    false,
                    KnobLook {
                        cap: c.send_cap,
                        ring,
                    },
                    th,
                );
                let name = send
                    .and_then(|s| model.project().track(s.target))
                    .map_or_else(|| "—".to_string(), |dst| dst.name.clone());
                let tap = match send.map(|s| s.tap) {
                    Some(SendTap::PreFx) => "PRE-FX ",
                    Some(SendTap::PreFader) => "PRE ",
                    _ => "",
                };
                controls::engraved(p, &format!("{tap}{name}"), slot.label, th, Align::Center);
            }
        }

        let vca = t.kind == TrackKind::Vca;
        let bed = model.project().surround_panned(t);
        if vca {
            controls::engraved(p, "VCA", l.pan_knob, th, Align::Center);
        } else if let faderframe_core::ChannelLayout::Surround(f) = t.layout {
            // A bed: it passes into its destination as it is (or folded).
            controls::engraved(p, f.name(), l.pan_knob, th, Align::Center);
            controls::readout(p, l.pan_readout, "BED", th);
        } else if let Some(format) = bed {
            // Into a surround bed: where it sits, seen from above.
            let pan = model.shown_surround(t);
            let meter = model.meter(t.id);
            let levels: Vec<f32> = meter.shown().iter().map(|c| c.level_db).collect();
            faderframe_view_surround::room::Room {
                format,
                source: t.layout,
                pan,
                levels: &levels,
                puck: track_color(t.color).lighten(0.2),
                compact: true,
                object: model.project().is_object(t),
            }
            .paint(p, l.pan_knob, th);
            controls::readout(
                p,
                l.pan_readout,
                &faderframe_view_surround::room::format_place(&pan),
                th,
            );
        } else {
            controls::knob(
                p,
                l.pan_knob,
                (model.shown_pan(t) + 1.0) * 0.5,
                true,
                KnobLook {
                    cap: c.pan_cap,
                    ring: c.panel_label,
                },
                th,
            );
            controls::readout(p, l.pan_readout, &format_pan(model.shown_pan(t)), th);
        }
        // Modulated: where the pan and the fader are now (a dot).
        let (travel, pan_mod) = if vca {
            (0.0, 0.0)
        } else {
            model.strip_modulation(t.id)
        };
        let dot = |p: &mut dyn Painter, at: Point| {
            p.circle(at, 3.4, th.ui.background);
            p.circle(at, 2.3, th.ui.text);
        };
        if pan_mod != 0.0 && bed.is_none() {
            let now = ((model.shown_pan(t) + pan_mod).clamp(-1.0, 1.0) + 1.0) * 0.5;
            let a = controls::knob_angle(now);
            let (o, r) = (
                l.pan_knob.center(),
                l.pan_knob.w.min(l.pan_knob.h) * 0.5 - 1.6,
            );
            dot(p, Point::new(o.x + r * a.cos(), o.y + r * a.sin()));
        }

        controls::led_button(p, l.mute, "M", model.shown_mute(t), c.led.mute, th);
        controls::led_button(p, l.solo, "S", t.solo, c.led.solo, th);
        if vca {
            controls::led_button(p, l.record, "·", false, c.led.record, th);
        } else if t.kind == TrackKind::Master {
            controls::led_button_fit(
                p,
                l.record,
                &["MONO", "MO"],
                model.mono_check(),
                c.led.monitor,
                th,
            );
        } else if t.kind.has_clips() {
            controls::led_button(p, l.record, "R", t.record_arm, c.led.record, th);
        } else {
            controls::led_button(p, l.record, "·", false, c.led.record, th);
        }

        controls::readout(p, l.level_readout, &format_db(model.shown_volume_db(t)), th);
        let geo = FaderGeometry::new(l.fader, th);
        let marks: [(f32, &str); 10] = [
            (12.0, "12"),
            (6.0, "6"),
            (0.0, "0"),
            (-5.0, "5"),
            (-10.0, "10"),
            (-20.0, "20"),
            (-30.0, "30"),
            (-40.0, "40"),
            (-60.0, "60"),
            (SILENCE_DB, "∞"),
        ];
        let scale: Vec<(f32, &str)> = marks
            .iter()
            .map(|&(db, s)| (self.law.db_to_position(db), s))
            .collect();
        controls::fader(
            p,
            &geo,
            self.law.db_to_position(model.shown_volume_db(t)),
            fader_cap_color(t.kind, th),
            &scale,
            th,
        );
        if travel != 0.0 {
            let now = self.law.db_to_position(model.shown_volume_db(t)) + travel;
            dot(p, Point::new(geo.slot.center().x, geo.y_for(now)));
        }
        if vca {
            // A VCA has no signal: its well says what it controls.
            let n = model.project().vca_members(t.id).len();
            let label = match n {
                0 => "no tracks".to_string(),
                1 => "1 track".to_string(),
                n => format!("{n} tracks"),
            };
            controls::well_label(p, l.output, &label, n == 0, th);
            controls::scribble(p, l.scribble, &t.name, color, th);
            return;
        }
        let m: MeterDisplay = model.meter(t.id);
        let channels: Vec<faderframe_session::MeterChannel> = if m.count > 2 {
            m.shown().to_vec()
        } else {
            vec![m.left, m.right]
        };
        paint_meter(
            p,
            l.meter,
            &channels,
            model.meter_mode(t.id),
            model.vu_reference(),
            th,
        );
        // The track's compression, from the top down on the meter's own
        // scale (6 dB taken off reaches the meter's −6).
        if model.gain_reduction(t.id).is_some() {
            let r = reduction_rect(&l);
            let shown = self.reduction.get(&t.id).copied().unwrap_or(0.0);
            p.fill_rounded(
                r,
                1.0,
                &faderframe_ui_canvas::Paint::Solid(c.meter.background),
            );
            if shown > 0.05 {
                let depth = r.h * (1.0 - controls::meter_scale(-shown));
                p.fill_rounded(
                    Rect::new(r.x, r.y, r.w, depth.clamp(1.0, r.h)),
                    1.0,
                    &faderframe_ui_canvas::Paint::Solid(c.meter.orange),
                );
            }
        }

        let out = match t.output {
            OutputRouting::Master => "→ Master".to_string(),
            OutputRouting::Track { track } => model
                .project()
                .track(track)
                .map_or_else(|| "→ ?".to_string(), |d| format!("→ {}", d.name)),
            OutputRouting::Hardware { first_channel } => {
                let first = first_channel as usize;
                let outputs = model
                    .stream_info()
                    .map_or(0, |i| i.output_channels as usize);
                // A bed wider than the device is folded down to it.
                let folded = (outputs > 0)
                    .then(|| {
                        faderframe_core::surround::fold_into(
                            t.layout,
                            outputs.saturating_sub(first),
                        )
                    })
                    .flatten();
                let n = folded.unwrap_or(t.layout).channel_count().max(2);
                match folded {
                    Some(_) => format!("→ Out {}-{} folded", first + 1, first + n),
                    None => format!("→ Out {}-{}", first + 1, first + n),
                }
            }
            OutputRouting::None => "→ none".to_string(),
        };
        controls::well_label(p, l.output, &out, t.output == OutputRouting::None, th);
        controls::scribble(p, l.scribble, &t.name, color, th);
    }

    /// A MIDI track's strip. It has no audio, so no pan, fader or meter:
    /// the instrument it plays where the others have their preamp, its MIDI
    /// input, its MIDI effects in the inserts, the notes it plays now (by
    /// name, and lit on a key display where the fader would be) and the
    /// external MIDI device it plays in the output well.
    fn paint_midi_strip(&self, p: &mut dyn Painter, l: &StripLayout, t: &Track, model: &Session) {
        let th = &self.theme;
        let c = &th.console;
        if let Some(area) = l.preamp {
            self.paint_plays(p, area, t, model);
        }
        if let Some(row) = l.input {
            let label = match &t.input {
                InputRouting::Midi {
                    channel: Some(ch), ..
                } => format!("MIDI {}", ch + 1),
                InputRouting::Midi { .. } => "MIDI".to_string(),
                _ => "IN —".to_string(),
            };
            controls::well_label(p, midi_input_rect(&row), &label, !t.input.is_midi(), th);
            let live = match t.monitor {
                MonitorMode::Auto => "A",
                _ => "I",
            };
            controls::led_button(
                p,
                row.monitor,
                live,
                t.monitor != MonitorMode::Off,
                c.led.monitor,
                th,
            );
        }
        self.paint_inserts(p, l, t, model);

        let keys = model.sounding_keys(t.id);
        let notes = notes_rect(l);
        controls::engraved(
            p,
            "NOTES",
            Rect::new(notes.x, notes.y, notes.w, 11.0),
            th,
            Align::Center,
        );
        let readout = Rect::new(notes.x + 2.0, notes.y + 15.0, notes.w - 4.0, 15.0);
        controls::readout(p, readout, &note_names(keys, readout.w), th);

        controls::led_button(p, l.mute, "M", model.shown_mute(t), c.led.mute, th);
        controls::led_button(p, l.solo, "S", t.solo, c.led.solo, th);
        controls::led_button(p, l.record, "R", t.record_arm, c.led.record, th);

        self.paint_keys(p, keys_rect(l), keys, track_color(t.color));

        let out = t.midi_output.as_ref().map_or_else(
            || "MIDI OUT —".to_string(),
            |o| {
                let port = faderframe_project::midi_port_display(&o.port);
                match o.channel {
                    Some(ch) => format!("→ {port} {}", ch + 1),
                    None => format!("→ {port}"),
                }
            },
        );
        controls::well_label(p, l.output, &out, t.midi_output.is_none(), th);
        controls::scribble(p, l.scribble, &t.name, track_color(t.color), th);
    }

    /// The instrument track a MIDI track plays: its colour and name (and,
    /// with room, its instrument).
    fn paint_plays(&self, p: &mut dyn Painter, area: Rect, t: &Track, model: &Session) {
        let th = &self.theme;
        let c = &th.console;
        let target = match t.output {
            OutputRouting::Track { track } => model.project().track(track),
            _ => None,
        };
        let compact = area.h < 40.0;
        let well = if compact {
            area
        } else {
            controls::engraved(
                p,
                "PLAYS",
                Rect::new(area.x, area.y, area.w, 11.0),
                th,
                Align::Center,
            );
            Rect::new(area.x, area.y + 14.0, area.w, (area.h - 14.0).min(34.0))
        };
        let hot = self.hover == Some(Hit::Plays(t.id));
        let inner = controls::well(p, well, th);
        if hot {
            p.stroke_rounded(well.inset(0.5), 2.5, 1.0, c.panel_label.with_alpha(0.6));
        }
        let style = |color: Color| {
            faderframe_ui_canvas::TextStyle::new(th.fonts.tiny + 0.5, color)
                .family(faderframe_ui_canvas::FontFamily::Condensed)
                .center()
        };
        let Some(d) = target else {
            p.text("→ no instrument", inner, &style(c.well_text_empty));
            return;
        };
        p.fill_rounded(
            Rect::new(well.x + 3.0, well.y + 3.0, 3.0, well.h - 6.0),
            1.5,
            &faderframe_ui_canvas::Paint::Solid(track_color(d.color)),
        );
        let text = inner.inset_xy(3.0, 0.0);
        if compact {
            p.text(&format!("→ {}", d.name), text, &style(c.well_text));
            return;
        }
        let half = text.h / 2.0;
        p.text(
            &d.name,
            Rect::new(text.x, text.y + 1.0, text.w, half),
            &style(c.well_text).bold(),
        );
        let instrument = model.instrument_slot(d).map_or("no instrument yet", |s| {
            s.plugin.name.trim_start_matches("FaderFrame ")
        });
        p.text(
            instrument,
            Rect::new(text.x, text.y + half - 1.0, text.w, half),
            &style(c.well_text_empty),
        );
    }

    /// The keys sounding now, lit across a recessed key display (A0 at the
    /// bottom to C8 at the top, a line at every C).
    fn paint_keys(&self, p: &mut dyn Painter, area: Rect, keys: u128, color: Color) {
        let th = &self.theme;
        let c = &th.console;
        if area.h < 30.0 {
            return;
        }
        let inner = controls::well(p, area, th).inset_xy(-2.0, 2.0);
        let count = f32::from(KEYS_HI - KEYS_LO + 1);
        let row = inner.h / count;
        let y_of = |k: u8| inner.bottom() - f32::from(k - KEYS_LO + 1) * row;
        for k in KEYS_LO..=KEYS_HI {
            let y = y_of(k);
            if !matches!(k % 12, 1 | 3 | 6 | 8 | 10) {
                p.fill(
                    Rect::new(inner.x, y, inner.w, row),
                    th.ui.text.with_alpha(0.04),
                );
            }
            if k % 12 == 0 {
                p.hline(inner.x, inner.right(), y + row, th.ui.text.with_alpha(0.16));
                if row * 12.0 >= 16.0 {
                    p.text(
                        &format!("C{}", i32::from(k) / 12 - 1),
                        Rect::new(inner.x + 1.0, y + row - 10.0, inner.w - 2.0, 10.0),
                        &faderframe_ui_canvas::TextStyle::new(
                            th.fonts.tiny - 1.0,
                            c.well_text_empty,
                        )
                        .family(faderframe_ui_canvas::FontFamily::Condensed),
                    );
                }
            }
        }
        for k in (0u8..128).filter(|k| keys & (1u128 << k) != 0) {
            let k = k.clamp(KEYS_LO, KEYS_HI);
            let bar = Rect::new(inner.x + 1.0, y_of(k), inner.w - 2.0, row.max(2.0));
            p.shadow(bar, 1.0, color.with_alpha(0.7), 0.0, 0.0, 4.0);
            p.fill_rounded(bar, 1.0, &faderframe_ui_canvas::Paint::Solid(color));
        }
    }

    /// The inserts section and the grip under it.
    fn paint_inserts(&self, p: &mut dyn Painter, l: &StripLayout, t: &Track, model: &Session) {
        let th = &self.theme;
        let c = &th.console;
        if let (Some(label), Some(slots)) = (l.inserts_label, &l.inserts) {
            let title = if t.freeze.is_some() {
                "INSERTS · FROZEN"
            } else {
                "INSERTS"
            };
            controls::engraved(p, title, label, th, Align::Center);
            // More plugins than slots: the last slot says how many more.
            let overflow = t.inserts.len() > slots.len();
            for (i, slot) in slots.iter().enumerate() {
                if overflow && i + 1 == slots.len() {
                    let more = t.inserts.len() - i;
                    controls::well_label(p, *slot, &format!("+{more} more"), false, th);
                    continue;
                }
                match t.inserts.get(i) {
                    Some(s) => {
                        let name = s.plugin.name.trim_start_matches("FaderFrame ");
                        // Keyed plugins name their sidechain source.
                        let key = s
                            .sidechain
                            .and_then(|k| model.project().track(k))
                            .map_or(String::new(), |k| format!(" ⟵ {}", k.name));
                        let failed = model.plugin_failed(s.id);
                        let text = if failed {
                            format!("⚠ {name}")
                        } else if s.bypass {
                            format!("({name}{key})")
                        } else {
                            format!("{name}{key}")
                        };
                        controls::well_label(p, *slot, &text, s.bypass || failed, th);
                    }
                    None => controls::well_label(p, *slot, "—", true, th),
                }
            }
        }

        if let Some(g) = l.inserts_grip {
            // A grip on the rule: drag it to size the inserts section.
            let hot = matches!(self.hover, Some(Hit::InsertsGrip(_)))
                || matches!(self.drag, Some(Drag::InsertSlots { .. }));
            let pill = Rect::new(g.center().x - 12.0, g.center().y - 1.5, 24.0, 3.0);
            p.fill_rounded(
                pill,
                1.5,
                &faderframe_ui_canvas::Paint::Solid(c.panel_label.with_alpha(if hot {
                    0.9
                } else {
                    0.35
                })),
            );
        }
    }

    /// The group (filled, its colour) and VCA (outlined, the VCA's colour)
    /// a track follows.
    fn paint_tags(&self, p: &mut dyn Painter, area: Rect, t: &Track, model: &Session) {
        let th = &self.theme;
        let project = model.project();
        let half = (area.w - 3.0) * 0.5;
        let left = Rect::new(area.x, area.y, half, area.h);
        let right = Rect::new(area.x + half + 3.0, area.y, half, area.h);
        let style = |c: Color| {
            faderframe_ui_canvas::TextStyle::new(th.fonts.tiny, c)
                .center()
                .bold()
        };
        match t.group.and_then(|g| project.group(g)) {
            Some(g) => {
                let gc = track_color(g.color);
                let fill = if g.active { gc } else { gc.with_alpha(0.35) };
                p.fill_rounded(left, 3.0, &fill.into());
                p.text(
                    &g.name,
                    left.inset_xy(2.0, 0.0),
                    &style(Color::hex(0x101114)),
                );
            }
            None => controls::engraved(p, "—", left, th, Align::Center),
        }
        match t.vca.and_then(|v| project.track(v)) {
            Some(v) => {
                let vc = track_color(v.color);
                p.stroke_rounded(right.inset(0.5), 3.0, 1.0, vc);
                p.text(&v.name, right.inset_xy(2.0, 0.0), &style(vc));
            }
            None => controls::engraved(p, "—", right, th, Align::Center),
        }
    }

    // --- interaction helpers ---------------------------------------------------

    /// The theme to paint with: the skin, with the console's look when
    /// the project asks for it.
    fn looked(&self) -> Theme {
        match self.look {
            Some(f) => self.base_theme.with_console_family(usize::from(f)),
            None => self.base_theme.clone(),
        }
    }

    /// Take (or leave) the console's look as the project's console changes.
    pub(crate) fn follow_console(&mut self, model: &Session) {
        let look = model.console().filter(|c| c.look).map(|c| c.family);
        if look != self.look {
            self.look = look;
            self.theme = self.looked();
        }
    }

    fn track(model: &Session, id: TrackId) -> Option<&Track> {
        model.project().track(id)
    }

    fn knob_value(&self, t: &Track, target: KnobTarget) -> Option<f32> {
        match target {
            KnobTarget::Preamp(id) => t
                .preamp
                .as_ref()
                .map(|slot| preamp::position(&preamp::face(slot), preamp::value(slot, id), id)),
            KnobTarget::Pan => Some((t.pan + 1.0) * 0.5),
            KnobTarget::Send(i) => t.sends.get(i).map(|s| self.law.db_to_position(s.level_db)),
        }
    }

    fn knob_command(&self, t: &Track, target: KnobTarget, value: f32) -> Option<Command> {
        let value = value.clamp(0.0, 1.0);
        match target {
            KnobTarget::Preamp(id) => t.preamp.as_ref().map(|slot| Command::SetPluginParameter {
                track: t.id,
                plugin: slot.id,
                parameter: faderframe_core::ParameterId(id),
                value: Some(preamp::plain(&preamp::face(slot), value, id)),
            }),
            KnobTarget::Pan => {
                // Snap to centre near the middle for convenience.
                let pan = value * 2.0 - 1.0;
                let pan = if pan.abs() < 0.01 { 0.0 } else { pan };
                Some(Command::SetTrackPan { track: t.id, pan })
            }
            KnobTarget::Send(i) => t.sends.get(i).map(|s| Command::SetSendLevel {
                track: t.id,
                send: s.id,
                db: self.law.position_to_db(value),
            }),
        }
    }

    fn output_menu(model: &Session, t: &Track, at: Point) -> HostRequest<Action> {
        let p = model.project();
        let mut items = Vec::new();
        let set = |output| {
            Action::Edit(Command::SetTrackOutput {
                track: t.id,
                output,
            })
        };
        if t.kind != TrackKind::Master {
            items.push(
                MenuItem::new("Master", set(OutputRouting::Master))
                    .checked(t.output == OutputRouting::Master),
            );
            for dst in p.tracks.iter().filter(|d| {
                matches!(d.kind, TrackKind::Bus | TrackKind::Aux)
                    && d.id != t.id
                    && !p.would_cycle(t.id, d.id)
            }) {
                let out = OutputRouting::Track { track: dst.id };
                items.push(
                    MenuItem::new(format!("{} ({})", dst.name, dst.kind.label()), set(out))
                        .checked(t.output == out),
                );
            }
        }
        let width = t.layout.channel_count().max(2) as u16;
        for first in [0u16, 2] {
            let out = OutputRouting::Hardware {
                first_channel: first,
            };
            let mut item = MenuItem::new(
                format!("Hardware Out {}-{}", first + 1, first + width),
                set(out),
            )
            .checked(t.output == out);
            if first == 0 {
                item = item.separated();
            }
            items.push(item);
        }
        items.push(
            MenuItem::new("Not connected", set(OutputRouting::None))
                .checked(t.output == OutputRouting::None),
        );
        HostRequest::ContextMenu { at, items }
    }

    /// A submenu `name` with MIDI learn for `target` and its mappings'
    /// removal.
    fn learn_submenu(
        model: &Session,
        t: &Track,
        target: faderframe_automation::AutomationTarget,
        name: &str,
    ) -> MenuItem<Action> {
        let target = faderframe_project::MappingTarget::Parameter {
            track: t.id,
            target,
        };
        let entries = model
            .midi_learn_menu(target)
            .into_iter()
            .map(|(label, action)| MenuItem::new(label, action))
            .collect();
        let mapped = model.midi_mappings_for(target).len();
        MenuItem::submenu(
            if mapped > 0 {
                format!("{name} (mapped)")
            } else {
                name.to_string()
            },
            entries,
        )
    }

    /// MIDI learn for a control (and removing its mappings).
    fn learn_menu(
        model: &Session,
        t: &Track,
        target: faderframe_automation::AutomationTarget,
        at: Point,
    ) -> HostRequest<Action> {
        let target = faderframe_project::MappingTarget::Parameter {
            track: t.id,
            target,
        };
        Self::learn_target_menu(model, target, at)
    }

    /// MIDI learn for any mapping target (and removing its mappings).
    fn learn_target_menu(
        model: &Session,
        target: faderframe_project::MappingTarget,
        at: Point,
    ) -> HostRequest<Action> {
        let mut items = vec![MenuItem::disabled(model.mapping_target_label(&target))];
        for (i, (label, action)) in model.midi_learn_menu(target).into_iter().enumerate() {
            let item = MenuItem::new(label, action);
            items.push(if i == 0 { item.separated() } else { item });
        }
        HostRequest::ContextMenu { at, items }
    }

    fn input_menu(model: &Session, t: &Track, at: Point) -> HostRequest<Action> {
        let notes = matches!(t.kind, TrackKind::Instrument | TrackKind::Midi);
        let choices = if notes {
            model.midi_input_choices(t.id)
        } else {
            model.input_choices(t.id)
        };
        let mut items: Vec<MenuItem<Action>> = choices
            .into_iter()
            .map(|c| {
                let item = MenuItem::new(c.label, c.action).checked(c.checked);
                if c.group_start {
                    item.separated()
                } else {
                    item
                }
            })
            .collect();
        if notes {
            // Live play: never, when armed or selected, always.
            for (i, (mode, label)) in [
                (MonitorMode::Off, "Play Live: Off"),
                (MonitorMode::Auto, "Play Live: when armed or selected"),
                (MonitorMode::Input, "Play Live: always"),
            ]
            .into_iter()
            .enumerate()
            {
                let item = MenuItem::new(
                    label,
                    Action::Edit(Command::SetTrackMonitor { track: t.id, mode }),
                )
                .checked(t.monitor == mode);
                items.push(if i == 0 { item.separated() } else { item });
            }
            return HostRequest::ContextMenu { at, items };
        }
        items.push(
            MenuItem::new(
                "Monitor: tape-style (auto)",
                Action::Edit(Command::SetTrackMonitor {
                    track: t.id,
                    mode: MonitorMode::Auto,
                }),
            )
            .checked(t.monitor == MonitorMode::Auto)
            .separated(),
        );
        HostRequest::ContextMenu { at, items }
    }

    fn insert_menu(model: &Session, t: &Track, slot: usize, at: Point) -> HostRequest<Action> {
        let mut items = Vec::new();
        match t.inserts.get(slot) {
            Some(s) => {
                items.push(MenuItem::disabled(s.plugin.name.clone()));
                let builtin = s.plugin.format == faderframe_project::PluginFormat::Builtin;
                if model.plugin_failed(s.id) {
                    items.push(MenuItem::new(
                        "Reload Plugin (it stopped working)",
                        Action::ReloadPlugin(s.id),
                    ));
                }
                items.push(MenuItem::new(
                    if builtin {
                        "Show Editor"
                    } else {
                        "Show Plugin GUI"
                    },
                    Action::OpenPluginEditor {
                        track: t.id,
                        plugin: s.id,
                        generic: false,
                    },
                ));
                if !builtin {
                    items.push(MenuItem::new(
                        "Show Parameters",
                        Action::OpenPluginEditor {
                            track: t.id,
                            plugin: s.id,
                            generic: true,
                        },
                    ));
                }
                // A multi-output plugin: tracks for its outputs (the main
                // too).
                if model.plugin_has_extra_outputs(s.id) {
                    let outs = model.plugin_output_buses(s.id);
                    let missing = outs.iter().filter(|o| o.track.is_none()).count();
                    let mut item = MenuItem::new(
                        format!("Create Output Tracks ({missing})"),
                        Action::CreateOutputTracks {
                            plugin: s.id,
                            buses: None,
                        },
                    )
                    .separated();
                    if missing == 0 {
                        item = MenuItem::disabled("Every Output Has a Track").separated();
                    }
                    items.push(item);
                    items.push(MenuItem::submenu(
                        "Outputs",
                        outs.iter()
                            .map(|o| {
                                // Checked: a track takes it; unchecking
                                // removes that track.
                                let action = if o.track.is_some() {
                                    Action::RemoveOutputTrack {
                                        plugin: s.id,
                                        bus: o.bus,
                                    }
                                } else {
                                    Action::CreateOutputTracks {
                                        plugin: s.id,
                                        buses: Some(vec![o.bus]),
                                    }
                                };
                                MenuItem::new(o.name.clone(), action).checked(o.track.is_some())
                            })
                            .collect(),
                    ));
                }
                // Presets: the user's and the plugin format's own.
                let presets = model.plugin_presets(s.id);
                items.push(
                    MenuItem::new("Save Preset…", Action::PromptSavePluginPreset(s.id)).separated(),
                );
                for p in presets.iter().take(24) {
                    items.push(MenuItem::new(
                        format!(
                            "Preset: {}{}",
                            p.name,
                            if p.factory { " (factory)" } else { "" }
                        ),
                        Action::LoadPluginPreset {
                            plugin: s.id,
                            path: p.path.clone(),
                        },
                    ));
                }
                if presets.len() > 24 {
                    items.push(MenuItem::disabled(format!(
                        "… {} more in the parameter window",
                        presets.len() - 24
                    )));
                }
                let own: Vec<MenuItem<Action>> = presets
                    .iter()
                    .filter(|p| !p.factory)
                    .map(|p| {
                        MenuItem::new(
                            p.name.clone(),
                            Action::PromptDeletePreset {
                                path: p.path.clone(),
                            },
                        )
                    })
                    .collect();
                if !own.is_empty() {
                    items.push(MenuItem::submenu("Delete Preset", own));
                }
                // The plugin's own programs (VST3 program lists, the
                // built-ins' factory presets), grouped ones in submenus.
                let programs = model.plugin_programs(s.id);
                let groups = model.plugin_program_groups(s.id);
                let current = model.plugin_current_program(s.id);
                let group_of = |i: usize| groups.get(i).map_or("", String::as_str);
                let program = |i: usize, label: String| {
                    MenuItem::new(
                        label,
                        Action::SelectPluginProgram {
                            plugin: s.id,
                            index: i,
                        },
                    )
                    .checked(current == Some(i))
                };
                let mut shown = 0;
                for (i, name) in programs.iter().enumerate() {
                    if !group_of(i).is_empty() {
                        continue;
                    }
                    if shown == 32 {
                        break;
                    }
                    let item = program(i, format!("Program: {name}"));
                    items.push(if shown == 0 { item.separated() } else { item });
                    shown += 1;
                }
                let top = (0..programs.len())
                    .filter(|&i| group_of(i).is_empty())
                    .count();
                if top > 32 {
                    items.push(MenuItem::disabled(format!(
                        "… {} more programs in the parameter window",
                        top - 32
                    )));
                }
                let mut named: Vec<&str> = Vec::new();
                for i in 0..programs.len() {
                    let g = group_of(i);
                    if !g.is_empty() && !named.contains(&g) {
                        named.push(g);
                    }
                }
                for (n, g) in named.into_iter().enumerate() {
                    let entries = programs
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| group_of(*i) == g)
                        .map(|(i, name)| program(i, name.clone()))
                        .collect();
                    let menu = MenuItem::submenu(format!("Programs: {g}"), entries);
                    items.push(if n == 0 && shown == 0 {
                        menu.separated()
                    } else {
                        menu
                    });
                }
                // Sidechain: which track's pre-fader signal keys the plugin.
                if model.plugin_has_sidechain(s.id) {
                    let set = |source| {
                        Action::Edit(Command::SetPluginSidechain {
                            track: t.id,
                            plugin: s.id,
                            source,
                        })
                    };
                    items.push(
                        MenuItem::new("Sidechain: None", set(None))
                            .checked(s.sidechain.is_none())
                            .separated(),
                    );
                    for (id, name) in model.sidechain_sources(s.id) {
                        items.push(
                            MenuItem::new(format!("Sidechain from {name}"), set(Some(id)))
                                .checked(s.sidechain == Some(id)),
                        );
                    }
                }
                items.push(
                    MenuItem::new(
                        if s.bypass { "Enable" } else { "Bypass" },
                        Action::Edit(Command::SetPluginBypass {
                            track: t.id,
                            plugin: s.id,
                            bypass: !s.bypass,
                        }),
                    )
                    .separated(),
                );
                items.push(Self::learn_submenu(
                    model,
                    t,
                    faderframe_automation::AutomationTarget::PluginBypass(s.id),
                    "Bypass: MIDI",
                ));
                if !builtin && !model.plugin_failed(s.id) {
                    items.push(MenuItem::new(
                        if model.plugin_sandboxed(s.id) {
                            "Reload Plugin (runs in its own process)"
                        } else {
                            "Reload Plugin"
                        },
                        Action::ReloadPlugin(s.id),
                    ));
                }
                items.push(MenuItem::new(
                    "Remove",
                    Action::Edit(Command::RemovePlugin {
                        track: t.id,
                        plugin: s.id,
                    }),
                ));
            }
            None => {
                items.push(MenuItem::new(
                    "Browse Plugins…",
                    Action::OpenPluginBrowser {
                        track: t.id,
                        target: match Self::empty_slot_target(model, t) {
                            faderframe_session::PluginTarget::Insert(_) => {
                                faderframe_session::PluginTarget::Insert(slot.min(t.inserts.len()))
                            }
                            other => other,
                        },
                    },
                ));
                // Every effect: built-ins first, then hosted plugins by vendor.
                let mut effects: Vec<_> = model
                    .available_plugins()
                    .into_iter()
                    .filter(|p| !p.instrument)
                    .collect();
                effects.sort_by(|a, b| {
                    let builtin = |p: &faderframe_session::AvailablePlugin| {
                        p.plugin.format != faderframe_project::PluginFormat::Builtin
                    };
                    (
                        builtin(a),
                        a.vendor.to_lowercase(),
                        a.plugin.name.to_lowercase(),
                    )
                        .cmp(&(
                            builtin(b),
                            b.vendor.to_lowercase(),
                            b.plugin.name.to_lowercase(),
                        ))
                });
                let mut last_vendor = None;
                for p in effects {
                    let label = if p.plugin.format == faderframe_project::PluginFormat::Builtin {
                        format!("Insert {}", p.plugin.name)
                    } else {
                        format!("{} · {}", p.vendor, p.plugin.name)
                    };
                    let mut item = MenuItem::new(
                        label,
                        Action::InsertPlugin {
                            track: t.id,
                            index: slot,
                            plugin: p.plugin.clone(),
                        },
                    );
                    if last_vendor.as_ref() != Some(&p.vendor) {
                        item = item.separated();
                    }
                    last_vendor = Some(p.vendor.clone());
                    items.push(item);
                }
                if items.is_empty() {
                    items.push(MenuItem::disabled("No effect plugins found"));
                }
            }
        }
        HostRequest::ContextMenu { at, items }
    }

    fn send_menu(model: &Session, t: &Track, slot: usize, at: Point) -> HostRequest<Action> {
        let p = model.project();
        let mut items = Vec::new();
        match t.sends.get(slot) {
            Some(s) => {
                for (tap, label) in [
                    (SendTap::PreFx, "Pre-FX"),
                    (SendTap::PreFader, "Pre-fader"),
                    (SendTap::PostFader, "Post-fader"),
                ] {
                    items.push(
                        MenuItem::new(
                            label,
                            Action::Edit(Command::SetSendTap {
                                track: t.id,
                                send: s.id,
                                tap,
                            }),
                        )
                        .checked(s.tap == tap),
                    );
                }
                items.push(
                    MenuItem::new(
                        if s.enabled { "Disable" } else { "Enable" },
                        Action::Edit(Command::SetSendEnabled {
                            track: t.id,
                            send: s.id,
                            enabled: !s.enabled,
                        }),
                    )
                    .separated(),
                );
                items.push(MenuItem::new(
                    "Remove Send",
                    Action::Edit(Command::RemoveSend {
                        track: t.id,
                        send: s.id,
                    }),
                ));
                let target = faderframe_project::MappingTarget::Parameter {
                    track: t.id,
                    target: faderframe_automation::AutomationTarget::SendLevel(s.id),
                };
                for (i, (label, action)) in model.midi_learn_menu(target).into_iter().enumerate() {
                    let item = MenuItem::new(label, action);
                    items.push(if i == 0 { item.separated() } else { item });
                }
            }
            None => {
                for dst in p.tracks.iter().filter(|d| {
                    matches!(d.kind, TrackKind::Bus | TrackKind::Aux)
                        && d.id != t.id
                        && !t.sends.iter().any(|s| s.target == d.id)
                }) {
                    let ok = !p.would_cycle(t.id, dst.id);
                    let label = format!("Send to {}", dst.name);
                    items.push(if ok {
                        MenuItem::new(
                            label,
                            Action::AddSend {
                                track: t.id,
                                target: dst.id,
                                level_db: -10.0,
                                tap: SendTap::PostFader,
                            },
                        )
                    } else {
                        MenuItem::disabled(format!("{label} (feedback)"))
                    });
                }
                if items.is_empty() {
                    items.push(MenuItem::disabled("Add an Aux or Bus track first"));
                }
            }
        }
        HostRequest::ContextMenu { at, items }
    }

    fn track_menu(model: &Session, t: &Track, at: Point) -> HostRequest<Action> {
        let mut items = Vec::new();
        if t.kind != TrackKind::Master {
            items.push(MenuItem::new(
                "Remove Track",
                Action::Edit(Command::RemoveTrack { track: t.id }),
            ));
        }
        items.push(
            MenuItem::new(
                "Colour…",
                Action::PickColor(faderframe_session::ColorTarget::Track(t.id)),
            )
            .separated(),
        );
        items.extend(model.group_menu(t.id).into_iter().map(menu_item));
        let formats: Vec<MenuItem<Action>> = model
            .format_choices(t.id)
            .into_iter()
            .map(|c| {
                let item = MenuItem::new(c.label, c.action).checked(c.checked);
                if c.group_start {
                    item.separated()
                } else {
                    item
                }
            })
            .collect();
        if !formats.is_empty() {
            items.push(MenuItem::submenu("Channel Format", formats).separated());
        }
        if model.project().surround_panned(t).is_some() {
            items.push(MenuItem::new(
                "Surround Panner…",
                Action::ShowSurroundPanner(t.id),
            ));
        }
        if let Some(c) = model.object_choice(t.id) {
            items.push(MenuItem::new(c.label, c.action).checked(c.checked));
        }
        let choices = |list: Vec<faderframe_session::InputChoice>| -> Vec<MenuItem<Action>> {
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
        };
        let render = choices(model.binaural_render_choices(t.id));
        if !render.is_empty() {
            items.push(MenuItem::submenu("Headphone Render (Dolby)", render));
        }
        if t.kind == TrackKind::Master {
            let mut listen = choices(model.listen_choices());
            listen.push(MenuItem::submenu("Head", choices(model.head_choices())).separated());
            items.push(MenuItem::submenu("Listen", listen).separated());
            items.push(MenuItem::submenu(
                "Console",
                choices(model.console_choices()),
            ));
        }
        if t.kind != TrackKind::Master {
            let now = model.strip_width(t.id);
            for (i, (label, w)) in STRIP_WIDTHS.iter().enumerate() {
                let item = MenuItem::new(
                    format!("{label} Strip"),
                    Action::SetStripWidth {
                        track: Some(t.id),
                        width: *w,
                    },
                )
                .checked(now == *w);
                items.push(if i == 0 { item.separated() } else { item });
            }
            items.push(MenuItem::new(
                "Every Strip This Wide",
                Action::SetStripWidth {
                    track: None,
                    width: now,
                },
            ));
        }
        if t.kind.has_audio() {
            items.push(MenuItem::submenu("Meter", meter_menu(model, Some(t.id))).separated());
        }
        for (i, c) in TrackColor::PALETTE.iter().enumerate() {
            let mut item = MenuItem::new(
                format!("Colour {}", i + 1),
                Action::Edit(Command::SetTrackColor {
                    track: t.id,
                    color: *c,
                }),
            )
            .checked(t.color == *c);
            if i == 0 {
                item = item.separated();
            }
            items.push(item);
        }
        HostRequest::ContextMenu { at, items }
    }

    fn rename_request(t: &Track, at: Rect) -> HostRequest<Action> {
        let id = t.id;
        HostRequest::TextInput {
            at,
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
        }
    }

    fn level_request(t: &Track, at: Rect) -> HostRequest<Action> {
        let id = t.id;
        HostRequest::TextInput {
            at,
            initial: format_db(t.volume_db),
            commit: Box::new(move |text| {
                parse_db(text).map(|db| Action::Edit(Command::SetTrackVolume { track: id, db }))
            }),
        }
    }

    /// While an insert is dragged: the slot it would land in and a label
    /// at the pointer.
    fn paint_insert_drag(&self, p: &mut dyn Painter, size: Size, model: &Session) {
        let Some(Drag::Insert {
            track,
            plugin,
            pos,
            moved: true,
            ..
        }) = self.drag
        else {
            return;
        };
        let th = &self.theme;
        let name = Self::track(model, track)
            .and_then(|t| t.inserts.iter().find(|s| s.id == plugin))
            .map_or_else(String::new, |s| {
                s.plugin.name.trim_start_matches("FaderFrame ").to_string()
            });
        let target = match self.hit_test(pos, size, model) {
            Some(Hit::Insert(to, i)) => self
                .layout_of(model, to, size)
                .and_then(|l| l.inserts.and_then(|v| v.get(i).copied()))
                .map(|r| (to, r)),
            _ => None,
        };
        let verb = match target {
            Some((to, _)) if to != track => "Copy",
            _ => "Move",
        };
        if let Some((_, r)) = target {
            p.stroke_rounded(r.inset(-1.0), 3.0, 1.5, th.ui.accent);
        }
        let label = format!("{verb} {name}");
        let style = faderframe_ui_canvas::TextStyle::new(th.fonts.small, th.ui.text);
        let w = p.text_width(&label, &style) + 14.0;
        let r = Rect::new(pos.x + 12.0, pos.y + 6.0, w, 20.0);
        p.fill_rounded(
            r,
            4.0,
            &faderframe_ui_canvas::Paint::Solid(Color::rgba(0.1, 0.11, 0.13, 0.92)),
        );
        p.stroke_rounded(r, 4.0, 1.0, th.ui.accent.with_alpha(0.8));
        p.text(&label, r, &style.center());
    }

    /// What releasing a pressed insert does: a click opens its editor
    /// (Ctrl: toggles bypass); dropped on an insert slot it moves within
    /// the track (Ctrl: duplicates) or copies to another track (Shift:
    /// moves).
    #[allow(clippy::too_many_arguments)]
    fn insert_drop(
        &self,
        model: &Session,
        size: Size,
        track: TrackId,
        plugin: faderframe_core::PluginInstanceId,
        moved: bool,
        pos: Point,
        mods: faderframe_ui_canvas::Modifiers,
    ) -> Option<Action> {
        let t = Self::track(model, track)?;
        let slot = t.inserts.iter().find(|s| s.id == plugin)?;
        if !moved {
            return Some(if mods.toggle() {
                Action::Edit(Command::SetPluginBypass {
                    track,
                    plugin,
                    bypass: !slot.bypass,
                })
            } else {
                Action::OpenPluginEditor {
                    track,
                    plugin,
                    generic: false,
                }
            });
        }
        let Some(Hit::Insert(to, index)) = self.hit_test(pos, size, model) else {
            return None;
        };
        let copy = if to == track {
            mods.toggle()
        } else {
            !mods.shift
        };
        Some(if copy {
            Action::CopyPlugin {
                track,
                plugin,
                to,
                index,
            }
        } else {
            Action::MovePlugin {
                track,
                plugin,
                to,
                index,
            }
        })
    }

    /// What an empty insert slot offers: the instrument for an instrument
    /// track that has none, else the next insert.
    fn empty_slot_target(model: &Session, t: &Track) -> faderframe_session::PluginTarget {
        if t.kind == TrackKind::Instrument && model.instrument_slot(t).is_none() {
            faderframe_session::PluginTarget::Instrument
        } else {
            faderframe_session::PluginTarget::Insert(t.inserts.len())
        }
    }

    fn pan_request(model: &Session, t: &Track, at: Rect) -> HostRequest<Action> {
        let id = t.id;
        HostRequest::TextInput {
            at,
            initial: format_pan(model.shown_pan(t)),
            commit: Box::new(move |text| {
                parse_pan(text).map(|pan| Action::Edit(Command::SetTrackPan { track: id, pan }))
            }),
        }
    }

    /// A strip's name (a folder's strip has its own layout).
    fn scribble_rect(&self, model: &Session, id: TrackId, size: Size) -> Option<Rect> {
        let (rect, t) = self
            .visible_strips(model, size)
            .into_iter()
            .find(|(_, t)| t.id == id)?;
        Some(if t.kind == TrackKind::Folder {
            folder_layout(rect).scribble
        } else {
            self.layout_for(rect, t, model.project()).scribble
        })
    }

    fn layout_of(&self, model: &Session, id: TrackId, size: Size) -> Option<StripLayout> {
        self.visible_strips(model, size)
            .into_iter()
            .find(|(_, t)| t.id == id)
            .map(|(r, t)| self.layout_for(r, t, model.project()))
    }

    /// Insertion boundary in mixer order, and index after removing the
    /// dragged track from the project (which may also contain MIDI tracks).
    /// Where a dragged strip lands: onto a folder's strip when over its
    /// middle (not its own folder, nor itself or a folder inside it), else
    /// between the strips either side of the gap under the pointer (only
    /// across matters, and a pointer beyond the strips takes the first or
    /// last gap). None when it would stay where it is.
    fn track_drop(
        &self,
        model: &Session,
        size: Size,
        track: TrackId,
        pos: Point,
    ) -> Option<StripDrop> {
        if self.master_only {
            return None;
        }
        let p = model.project();
        let moving = p.track(track)?;
        let tracks = Self::channel_tracks(model);
        // What moves with it: the track and what it holds.
        let block = |t: &Track| t.id == track || p.in_folder(t, track);
        let left = self.cheek();
        let x = pos.x.clamp(left, left + self.viewport_w(size)) - left + self.scroll_x;
        if let Some(i) = (0..tracks.len()).find(|&i| {
            let (a, w) = (self.offset(i), self.width(i));
            tracks[i].kind == TrackKind::Folder && x >= a + w * 0.2 && x < a + w * 0.8
        }) && !block(tracks[i])
            && moving.folder != Some(tracks[i].id)
        {
            return Some(StripDrop::Into {
                folder: tracks[i].id,
                strip: self.strip_rect(i, size),
            });
        }
        // The gap before the first strip whose middle is right of the
        // pointer.
        let gap = (0..tracks.len())
            .find(|&i| self.offset(i) + self.width(i) / 2.0 > x)
            .unwrap_or(tracks.len());
        let after = gap.checked_sub(1).map(|i| tracks[i]);
        let before = tracks.get(gap).copied();
        // Either side of the dragged strip (or of what it holds) is where
        // it is now.
        if after.is_some_and(block) || before.is_some_and(block) {
            return None;
        }
        Some(StripDrop::Between {
            after: after.map(|t| t.id),
            before: before.map(|t| t.id),
            x: self.cheek() + self.offset(gap) - self.scroll_x,
        })
    }

    fn press(
        &mut self,
        pos: Point,
        clicks: u32,
        mods: faderframe_ui_canvas::Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(hit) = self.hit_test(pos, size, model) else {
            self.drag = Some(Drag::Scroll {
                start_x: pos.x,
                start_scroll: self.scroll_x,
            });
            return true;
        };
        let toggle = |cx: &mut EventCx<'_, Action>, cmd: Command| cx.emit(Action::Edit(cmd));
        match hit {
            Hit::Width(id) => {
                if clicks >= 2 {
                    // Back to the default (Shift: every strip).
                    cx.emit(Action::SetStripWidth {
                        track: (!mods.shift).then_some(id),
                        width: None,
                    });
                } else {
                    let i = Self::channel_tracks(model).iter().position(|t| t.id == id);
                    self.drag = Some(Drag::Width {
                        track: id,
                        start_x: pos.x,
                        start: i.map_or(self.theme.console.strip_width, |i| self.width(i)),
                        all: mods.shift,
                    });
                }
            }
            Hit::AddTrack => {
                let r = self.add_track_rect(Self::channel_tracks(model).len());
                let items = model
                    .add_track_choices()
                    .into_iter()
                    .map(|(label, action, group)| {
                        let item = MenuItem::new(label, action);
                        if group { item.separated() } else { item }
                    })
                    .collect();
                cx.request(HostRequest::ContextMenu {
                    at: Point::new(r.x, r.bottom()),
                    items,
                });
            }
            Hit::PreampChoose(id) => {
                if let Some(t) = Self::track(model, id) {
                    cx.request(Self::stage_menu(model, t, pos));
                }
            }
            Hit::PreampRemove(id) => {
                // The master's is the console's: off is the console off.
                let master = Self::track(model, id).is_some_and(|t| t.kind == TrackKind::Master);
                cx.emit(if master {
                    Action::SetConsole { family: None }
                } else {
                    Action::SetPreamp {
                        track: id,
                        model: None,
                    }
                });
            }
            Hit::FaderCap(id) | Hit::FaderTrack(id) => {
                let Some(t) = Self::track(model, id) else {
                    return false;
                };
                if clicks >= 2 {
                    cx.emit(Action::Edit(Command::SetTrackVolume { track: id, db: 0.0 }));
                    return true;
                }
                cx.emit(Action::BeginGesture("Volume".into()));
                let mut start = self.law.db_to_position(model.shown_volume_db(t));
                if matches!(hit, Hit::FaderTrack(_))
                    && let Some(l) = self.layout_of(model, id, size)
                {
                    // Clicking the slot jumps the cap there.
                    start = FaderGeometry::new(l.fader, &self.theme).pos_for(pos.y);
                    cx.emit(Action::Edit(Command::SetTrackVolume {
                        track: id,
                        db: self.law.position_to_db(start),
                    }));
                }
                self.drag = Some(Drag::Fader {
                    track: id,
                    start_y: pos.y,
                    start_pos: start,
                });
                cx.set_cursor(Cursor::Grabbing);
            }
            Hit::SendBank(delta) => {
                let pages = self.send_pages(self.send_rows * SENDS_PER_ROW);
                self.send_bank =
                    (self.send_bank as i64 + delta as i64).clamp(0, pages as i64 - 1) as usize;
                cx.redraw();
            }
            Hit::Pan(id)
                if Self::track(model, id)
                    .is_some_and(|t| model.project().surround_panned(t).is_some()) =>
            {
                if clicks >= 2 {
                    cx.emit(Action::ShowSurroundPanner(id));
                    return true;
                }
                let (Some(t), Some(l)) = (Self::track(model, id), self.layout_of(model, id, size))
                else {
                    return false;
                };
                cx.emit(Action::BeginGesture("Surround Pan".into()));
                self.drag = Some(Drag::Surround {
                    track: id,
                    start: pos,
                    from: model.shown_surround(t),
                    size: l.pan_knob.w.max(8.0),
                });
                cx.set_cursor(Cursor::Grabbing);
            }
            Hit::Pan(id) | Hit::Send(id, _) | Hit::PreampKnob(id, _) => {
                let Some(t) = Self::track(model, id) else {
                    return false;
                };
                let target = match hit {
                    Hit::PreampKnob(_, id) => KnobTarget::Preamp(id),
                    Hit::Send(_, i) => KnobTarget::Send(i),
                    _ => KnobTarget::Pan,
                };
                let Some(value) = self.knob_value(t, target) else {
                    if let Hit::Send(_, i) = hit {
                        cx.request(Self::send_menu(model, t, i, pos));
                    }
                    return true;
                };
                if clicks >= 2 {
                    let reset = match target {
                        KnobTarget::Preamp(id) => t.preamp.as_ref().map_or(0.5, |slot| {
                            let f = preamp::face(slot);
                            preamp::position(&f, f.ranges[f.index(id)].2, id)
                        }),
                        KnobTarget::Pan => 0.5,
                        KnobTarget::Send(_) => self.law.unity_position(),
                    };
                    if let Some(cmd) = self.knob_command(t, target, reset) {
                        cx.emit(Action::Edit(cmd));
                    }
                    return true;
                }
                cx.emit(Action::BeginGesture(match target {
                    KnobTarget::Preamp(_) => "Preamp".into(),
                    KnobTarget::Pan => "Pan".into(),
                    KnobTarget::Send(_) => "Send Level".into(),
                }));
                self.drag = Some(Drag::Knob {
                    track: id,
                    target,
                    start_y: pos.y,
                    start_value: value,
                });
                cx.set_cursor(Cursor::ResizeVertical);
            }
            Hit::Mute(id) => {
                if let Some(t) = Self::track(model, id) {
                    toggle(
                        cx,
                        Command::SetTrackMute {
                            track: id,
                            on: !t.mute,
                        },
                    );
                }
            }
            Hit::Solo(id) => {
                if let Some(t) = Self::track(model, id) {
                    toggle(
                        cx,
                        Command::SetTrackSolo {
                            track: id,
                            on: !t.solo,
                        },
                    );
                }
            }
            Hit::MonoCheck => cx.emit(Action::SetMonoCheck(!model.mono_check())),
            Hit::Record(id) => {
                if let Some(t) = Self::track(model, id).filter(|t| t.kind.has_clips()) {
                    toggle(
                        cx,
                        Command::SetTrackRecordArm {
                            track: id,
                            on: !t.record_arm,
                        },
                    );
                }
            }
            Hit::Phase(id) => {
                if let Some(t) = Self::track(model, id) {
                    toggle(
                        cx,
                        Command::SetTrackPhaseInvert {
                            track: id,
                            on: !t.phase_invert,
                        },
                    );
                }
            }
            Hit::Monitor(id) => {
                if let Some(t) = Self::track(model, id) {
                    let mode = if t.monitor == MonitorMode::Off {
                        MonitorMode::Input
                    } else {
                        MonitorMode::Off
                    };
                    toggle(cx, Command::SetTrackMonitor { track: id, mode });
                }
            }
            Hit::Input(id) => {
                if let Some(t) = Self::track(model, id) {
                    cx.request(Self::input_menu(model, t, pos));
                }
            }
            Hit::Output(id) => {
                if let Some(t) = Self::track(model, id) {
                    cx.request(if t.kind == TrackKind::Midi {
                        Self::midi_out_menu(model, t, pos)
                    } else {
                        Self::output_menu(model, t, pos)
                    });
                }
            }
            Hit::Plays(id) => {
                if let Some(t) = Self::track(model, id) {
                    cx.request(Self::plays_menu(model, t, pos));
                }
            }
            Hit::Color(id) => {
                cx.emit(Action::PickColor(faderframe_session::ColorTarget::Track(
                    id,
                )));
            }
            Hit::Tags(id) => {
                let items: Vec<MenuItem<Action>> =
                    model.group_menu(id).into_iter().map(menu_item).collect();
                if !items.is_empty() {
                    cx.request(HostRequest::ContextMenu { at: pos, items });
                }
            }
            Hit::InsertsGrip(_) => {
                if clicks >= 2 {
                    cx.emit(Action::SetMixerInsertSlots(
                        faderframe_session::DEFAULT_INSERT_SLOTS,
                    ));
                } else {
                    self.drag = Some(Drag::InsertSlots {
                        start_y: pos.y,
                        start: self.insert_slots,
                    });
                    cx.set_cursor(Cursor::ResizeVertical);
                }
            }
            Hit::Insert(id, slot) => {
                let shown = self
                    .layout_of(model, id, size)
                    .and_then(|l| l.inserts.map(|v| v.len()))
                    .unwrap_or(0);
                if let Some(t) = Self::track(model, id)
                    && t.inserts.len() > shown
                    && slot + 1 == shown
                {
                    // "+N more": grow the section to show them all.
                    cx.emit(Action::SetMixerInsertSlots((t.inserts.len() + 1) as u16));
                } else if let Some(t) = Self::track(model, id) {
                    if slot >= t.inserts.len() {
                        // Empty slot: the plugin browser (an instrument track
                        // without an instrument gets one first).
                        cx.emit(Action::OpenPluginBrowser {
                            track: id,
                            target: Self::empty_slot_target(model, t),
                        });
                    } else if mods.alt {
                        // Alt-click: remove.
                        cx.emit(Action::Edit(Command::RemovePlugin {
                            track: id,
                            plugin: t.inserts[slot].id,
                        }));
                    } else {
                        // Click (editor; Ctrl: bypass) or drag, decided on
                        // release.
                        self.drag = Some(Drag::Insert {
                            track: id,
                            plugin: t.inserts[slot].id,
                            origin: pos,
                            pos,
                            moved: false,
                        });
                    }
                }
            }
            Hit::Level(id) => {
                if let (Some(t), Some(l)) =
                    (Self::track(model, id), self.layout_of(model, id, size))
                {
                    cx.request(Self::level_request(t, l.level_readout));
                }
            }
            Hit::PanValue(id) => {
                if let (Some(t), Some(l)) =
                    (Self::track(model, id), self.layout_of(model, id, size))
                {
                    if model.project().surround_panned(t).is_some() {
                        cx.emit(Action::ShowSurroundPanner(id));
                    } else {
                        cx.request(Self::pan_request(model, t, l.pan_readout));
                    }
                }
            }
            Hit::Scribble(id) | Hit::Strip(id) => {
                if clicks >= 2
                    && matches!(hit, Hit::Scribble(_))
                    && let (Some(t), Some(at)) =
                        (Self::track(model, id), self.scribble_rect(model, id, size))
                {
                    cx.request(Self::rename_request(t, at));
                    return true;
                }
                let mode = if mods.toggle() {
                    SelectMode::Toggle
                } else {
                    SelectMode::Replace
                };
                cx.emit(Action::SelectTracks {
                    tracks: vec![id],
                    mode,
                });
                if matches!(hit, Hit::Scribble(_))
                    && Self::track(model, id).is_some_and(|t| t.kind != TrackKind::Master)
                {
                    self.drag = Some(Drag::Track {
                        track: id,
                        origin: pos,
                        pos,
                        moved: false,
                    });
                }
            }
            Hit::Meter(_) => cx.emit(Action::ResetClipIndicators),
            Hit::Fold(id) => cx.emit(Action::ToggleFolder(id)),
            Hit::Reduction(_) => {}
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
        let Some(hit) = self.hit_test(pos, size, model) else {
            return false;
        };
        let req = match hit {
            Hit::PreampChoose(id) | Hit::PreampRemove(id) => {
                Self::track(model, id).map(|t| Self::stage_menu(model, t, pos))
            }
            Hit::PreampKnob(id, param) => Self::track(model, id).and_then(|t| {
                t.preamp.as_ref().map(|slot| {
                    Self::learn_menu(
                        model,
                        t,
                        faderframe_automation::AutomationTarget::PluginParameter {
                            plugin: slot.id,
                            parameter: faderframe_core::ParameterId(param),
                        },
                        pos,
                    )
                })
            }),
            Hit::Send(id, i) => Self::track(model, id).map(|t| Self::send_menu(model, t, i, pos)),
            Hit::Insert(id, i) => {
                Self::track(model, id).map(|t| Self::insert_menu(model, t, i, pos))
            }
            Hit::Output(id) => Self::track(model, id).map(|t| {
                if t.kind == TrackKind::Midi {
                    Self::midi_out_menu(model, t, pos)
                } else {
                    Self::output_menu(model, t, pos)
                }
            }),
            Hit::Plays(id) => Self::track(model, id).map(|t| Self::plays_menu(model, t, pos)),
            Hit::Input(id) | Hit::Monitor(id) => {
                Self::track(model, id).map(|t| Self::input_menu(model, t, pos))
            }
            Hit::Scribble(id) | Hit::Strip(id) | Hit::Tags(id) => {
                Self::track(model, id).map(|t| Self::track_menu(model, t, pos))
            }
            Hit::Meter(id) => Some(HostRequest::ContextMenu {
                at: pos,
                items: meter_menu(model, Some(id)),
            }),
            Hit::Level(id) => match (Self::track(model, id), self.layout_of(model, id, size)) {
                (Some(t), Some(l)) => Some(Self::level_request(t, l.level_readout)),
                _ => None,
            },
            Hit::FaderCap(id) | Hit::FaderTrack(id) => Self::track(model, id).map(|t| {
                Self::learn_menu(
                    model,
                    t,
                    faderframe_automation::AutomationTarget::TrackVolume,
                    pos,
                )
            }),
            // Panned into a surround bed: the room's parameters, each its
            // own submenu.
            Hit::Pan(id) => {
                Self::track(model, id).map(|t| match model.project().surround_panned(t) {
                    Some(format) => {
                        let items = faderframe_core::SurroundParam::ALL
                            .into_iter()
                            .filter(|p| p.applies(t.layout, format))
                            .map(|p| {
                                Self::learn_submenu(
                                    model,
                                    t,
                                    faderframe_automation::AutomationTarget::Surround(p),
                                    p.name(),
                                )
                            })
                            .collect();
                        HostRequest::ContextMenu { at: pos, items }
                    }
                    None => Self::learn_menu(
                        model,
                        t,
                        faderframe_automation::AutomationTarget::TrackPan,
                        pos,
                    ),
                })
            }
            Hit::Mute(id) => Self::track(model, id).map(|t| {
                Self::learn_menu(
                    model,
                    t,
                    faderframe_automation::AutomationTarget::TrackMute,
                    pos,
                )
            }),
            Hit::Solo(id) => Some(Self::learn_target_menu(
                model,
                faderframe_project::MappingTarget::TrackSolo { track: id },
                pos,
            )),
            Hit::Record(id) => Some(Self::learn_target_menu(
                model,
                faderframe_project::MappingTarget::TrackArm { track: id },
                pos,
            )),
            _ => None,
        };
        if let Some(r) = req {
            cx.request(r);
            true
        } else {
            false
        }
    }

    fn tooltip_for(&self, hit: Hit, model: &Session) -> Option<String> {
        let name = |id: TrackId| {
            Self::track(model, id)
                .map(|t| t.name.clone())
                .unwrap_or_default()
        };
        Some(match hit {
            Hit::PreampChoose(id) | Hit::PreampRemove(id) => {
                let kind = Self::track(model, id)?.kind;
                let remove = matches!(hit, Hit::PreampRemove(_));
                match (kind, remove) {
                    (TrackKind::Master, false) => "Choose the console the whole mix runs through".into(),
                    (TrackKind::Master, true) => "Console off: the mix in the box".into(),
                    (TrackKind::Bus | TrackKind::Aux, false) => {
                        "This bus's amplifier: follow the console, another console's, drive, in the box".into()
                    }
                    (TrackKind::Bus | TrackKind::Aux, true) => "In the box on this bus".into(),
                    (_, false) => "Choose microphone preamplifier".into(),
                    (_, true) => "Remove microphone preamplifier".into(),
                }
            }
            Hit::PreampKnob(id, param) => {
                let slot = Self::track(model, id)?.preamp.as_ref()?;
                let v = model
                    .display_value(
                        id,
                        faderframe_automation::AutomationTarget::PluginParameter {
                            plugin: slot.id,
                            parameter: faderframe_core::ParameterId(param),
                        },
                    )
                    .unwrap_or_else(|| preamp::value(slot, param));
                let f = preamp::face(slot);
                format!(
                    "{} {} · Drag or wheel · Double-click to reset",
                    f.labels[f.index(param)],
                    preamp::shown(&f, v, param)
                )
            }
            Hit::FaderCap(id) | Hit::FaderTrack(id) => {
                let t = Self::track(model, id)?;
                format!(
                    "{}: {} dB\nDrag · Shift/Ctrl for fine · Double-click for 0 dB · Wheel",
                    t.name,
                    format_db(model.shown_volume_db(t))
                )
            }
            Hit::Pan(id) => {
                let t = Self::track(model, id)?;
                match model.project().surround_panned(t) {
                    Some(f) => format!(
                        "Surround {} ({}) · Drag to move (Shift/Ctrl: fine) · Double-click: the panner",
                        faderframe_view_surround::room::format_place(&model.shown_surround(t)),
                        f.name()
                    ),
                    None => format!(
                        "Pan {} · Double-click to centre",
                        format_pan(model.shown_pan(t))
                    ),
                }
            }
            Hit::Send(id, i) => match Self::track(model, id)?.sends.get(i) {
                Some(s) => format!(
                    "Send level {} dB · Right-click for options",
                    format_db(s.level_db)
                ),
                None => "Click to add a send".into(),
            },
            Hit::SendBank(d) => if d < 0 {
                "Previous sends"
            } else {
                "Next sends"
            }
            .into(),
            Hit::Insert(id, i) => {
                if Self::track(model, id).is_some_and(|t| i < t.inserts.len()) {
                    "Insert · Click: editor · Ctrl-click: bypass · Alt-click: remove · Drag: reorder (Ctrl: duplicate), onto another track: copy (Shift: move) · Right-click: more".into()
                } else {
                    "Empty insert · Click to open the plugin browser · Right-click for a quick list"
                        .into()
                }
            }
            Hit::Mute(id) => format!("Mute {}", name(id)),
            Hit::Reduction(id) => {
                let devices = model.gain_reduction_devices(id);
                format!(
                    "Gain reduction: {:.1} dB ({})",
                    model.gain_reduction(id).unwrap_or(0.0),
                    devices.join(" + ")
                )
            }
            Hit::Fold(id) => format!(
                "{} {} · Drag a strip onto the folder to put it in",
                if model.folder_open(id) { "Close" } else { "Open" },
                name(id)
            ),
            Hit::Solo(id) => format!("Solo {}", name(id)),
            Hit::Record(id) => format!("Record-arm {}", name(id)),
            Hit::MonoCheck => "Mono check: hear the mix summed to mono (listening only, renders stay as mixed) · Right-click the strip: Listen (headphones)".into(),
            Hit::Phase(_) => "Invert polarity".into(),
            Hit::Monitor(id)
                if Self::track(model, id)
                    .is_some_and(|t| matches!(t.kind, TrackKind::Instrument | TrackKind::Midi)) =>
            {
                "Play live: A when armed or selected, I always · Right-click for the choices".into()
            }
            Hit::Monitor(_) => "Input monitoring · Right-click for tape-style auto".into(),
            Hit::Input(id) if Self::track(model, id).is_some_and(|t| t.kind == TrackKind::Midi) => {
                "MIDI input: which keyboard and channel this track plays live".into()
            }
            Hit::Input(_) => "Input routing".into(),
            Hit::Output(id) if Self::track(model, id).is_some_and(|t| t.kind == TrackKind::Midi) => {
                "External MIDI device this track also plays · Click to choose".into()
            }
            Hit::Output(_) => "Output routing".into(),
            Hit::Plays(id) => {
                let t = Self::track(model, id)?;
                let target = match t.output {
                    OutputRouting::Track { track } => model.project().track(track).map(|d| d.name.clone()),
                    _ => None,
                };
                match target {
                    Some(name) => format!("{} plays '{name}' · Click to choose another instrument track", t.name),
                    None => format!("{} plays no instrument · Click to choose an instrument track", t.name),
                }
            }
            Hit::Color(_) => "Track colour · Click to choose (the selected tracks follow)".into(),
            Hit::Tags(id) => {
                let t = Self::track(model, id)?;
                let group = t
                    .group
                    .and_then(|g| model.project().group(g))
                    .map_or("no group".to_string(), |g| format!("group '{}'", g.name));
                let vca = t
                    .vca
                    .and_then(|v| model.project().track(v))
                    .map_or("no VCA".to_string(), |v| format!("VCA '{}'", v.name));
                format!("{} · {group} · {vca} · Click to change", t.name)
            }
            Hit::Level(_) => "Click to type a level".into(),
            Hit::PanValue(id) => {
                let t = Self::track(model, id)?;
                if model.project().surround_panned(t).is_some() {
                    "Click to open the surround panner".into()
                } else {
                    format!(
                        "Pan {} · Click to type (C, L30, R45 or −100…100)",
                        format_pan(model.shown_pan(t))
                    )
                }
            }
            Hit::Scribble(_) => {
                "Drag to reorder · Double-click to rename · Right-click for options".into()
            }
            Hit::Meter(id) => format!(
                "{} · Click to clear clip indicators · Right-click: what the meter shows",
                meter_reading(model, id)
            ),
            Hit::InsertsGrip(_) => format!(
                "Drag to show more or fewer insert slots (now {}) · Double-click for {}",
                self.insert_slots,
                faderframe_session::DEFAULT_INSERT_SLOTS
            ),
            Hit::Strip(_) => return None,
            Hit::AddTrack => "Add a track".into(),
            Hit::Width(_) => "Drag to make the strip wider or narrower (Shift: every strip) · Double-click: the default width".into(),
        })
    }
}

/// The meter bridge's height.
const BRIDGE_H: f32 = 78.0;

/// `ev` with its position moved up by `dy` (into the strips' frame).
fn shifted(ev: &ViewEvent, dy: f32) -> ViewEvent {
    let mut e = *ev;
    match &mut e {
        ViewEvent::PointerDown { pos, .. }
        | ViewEvent::PointerMove { pos, .. }
        | ViewEvent::PointerUp { pos, .. }
        | ViewEvent::Scroll { pos, .. } => pos.y -= dy,
        _ => {}
    }
    e
}

/// A strip's VU reading for the bridge's tooltip.
fn meter_reading_vu(model: &Session, track: TrackId) -> String {
    let m = model.meter(track);
    let r = model.vu_reference();
    let name = model
        .project()
        .track(track)
        .map_or(String::new(), |t| t.name.clone());
    let vu = |c: &faderframe_session::MeterChannel| {
        let v = c.vu_db(r);
        if v < -40.0 {
            "−∞".to_string()
        } else {
            format!("{v:+.1}").replace('-', "−")
        }
    };
    if m.count > 2 {
        format!("{name} · VU (loudest channel)")
    } else {
        format!(
            "{name} · {} / {} VU (0 VU = {} dBFS RMS)",
            vu(&m.left),
            vu(&m.right),
            format!("{r:.0}").replace('-', "−")
        )
    }
}

impl MixerView {
    fn bridge_h(model: &Session) -> f32 {
        if model.meter_bridge() { BRIDGE_H } else { 0.0 }
    }

    /// The strips of the bridge: (track, x, width), the master's last.
    fn bridge_columns(&self, body: Size, model: &Session) -> Vec<(TrackId, f32, f32)> {
        let tracks = Self::channel_tracks(model);
        let mut out = Vec::new();
        if !self.master_only {
            for i in self.visible_range(tracks.len(), body) {
                let t = tracks[i];
                if !t.kind.has_audio() {
                    continue;
                }
                let r = self.strip_rect(i, body);
                out.push((t.id, r.x, r.w));
            }
        }
        if !self.hide_master
            && let Some(m) = model.project().master()
        {
            let r = self.master_rect(body);
            out.push((m.id, r.x, r.w));
        }
        out
    }

    fn bridge_track_at(&self, x: f32, body: Size, model: &Session) -> Option<TrackId> {
        self.bridge_columns(body, model)
            .into_iter()
            .find(|(_, x0, w)| x >= *x0 && x < x0 + w)
            .map(|(t, _, _)| t)
    }

    /// The meter bridge: a moving-coil VU meter over every strip (two for a
    /// wide stereo strip, the loudest channel of a bed), on the console's
    /// panel.
    fn paint_bridge(&self, p: &mut dyn Painter, width: f32, h: f32, body: Size, model: &Session) {
        let th = &self.theme;
        let c = &th.console;
        let band = Rect::new(0.0, 0.0, width, h);
        p.fill_rect(
            band,
            &faderframe_ui_canvas::Paint::vertical(band, c.panel_top, c.panel_bottom),
        );
        p.hline(0.0, width, h - 0.5, c.panel_edge_dark);
        let reference = model.vu_reference();
        let viewport = Rect::new(self.cheek(), 0.0, self.viewport_w(body), h);
        for (track, x, w) in self.bridge_columns(body, model) {
            let master = model.project().master().is_some_and(|m| m.id == track);
            if !master {
                p.push_clip(viewport);
            }
            let m = model.meter(track);
            let name = model
                .project()
                .track(track)
                .map_or(String::new(), |t| t.name.clone());
            let cell = Rect::new(x + 2.0, 4.0, (w - 4.0).max(0.0), h - 8.0);
            let deflection = |ch: &faderframe_session::MeterChannel| {
                controls::vu_deflection(ch.vu_db(reference))
            };
            let peak = |ch: &faderframe_session::MeterChannel| ch.level_db >= -2.0;
            if m.count > 2 {
                let shown = m.shown();
                let d = shown.iter().map(deflection).fold(-0.01, f32::max);
                let lit = shown.iter().any(peak);
                controls::vu_arc(p, cell, d, lit, &name, th);
            } else if cell.w >= 128.0 {
                let half = (cell.w - 3.0) / 2.0;
                let left = Rect::new(cell.x, cell.y, half, cell.h);
                let right = Rect::new(cell.x + half + 3.0, cell.y, half, cell.h);
                controls::vu_arc(
                    p,
                    left,
                    deflection(&m.left),
                    peak(&m.left),
                    &format!("{name} L"),
                    th,
                );
                controls::vu_arc(p, right, deflection(&m.right), peak(&m.right), "R", th);
            } else {
                let d = deflection(&m.left).max(deflection(&m.right));
                controls::vu_arc(p, cell, d, peak(&m.left) || peak(&m.right), &name, th);
            }
            if !master {
                p.pop_clip();
            }
        }
    }

    fn paint_body(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.follow_console(model);
        // The gain reduction shown: up at once, back slowly.
        let mut shown = std::mem::take(&mut self.reduction);
        for t in Self::channel_tracks(model)
            .into_iter()
            .chain(model.project().master())
        {
            if let Some(now) = model.gain_reduction(t.id) {
                let was = shown.get(&t.id).copied().unwrap_or(0.0);
                self.reduction.insert(t.id, now.max(was * REDUCTION_FALL));
            }
        }
        shown.clear();
        self.update_sends(model);
        self.update_strips(model);
        let tracks = Self::channel_tracks(model);
        if let Some(Drag::Track {
            pos, moved: true, ..
        }) = self.drag
        {
            if pos.x < self.cheek() + 20.0 {
                self.scroll_x -= 8.0;
            } else if pos.x > self.cheek() + self.viewport_w(size) - 20.0 {
                self.scroll_x += 8.0;
            }
        }
        self.clamp_scroll(tracks.len(), size);
        p.fill(Rect::from_size(size), theme.ui.background);
        let cheek = self.cheek();
        let viewport = Rect::new(cheek, 0.0, self.viewport_w(size), size.h);
        p.push_clip(viewport);
        for i in self.visible_range(tracks.len(), size) {
            let rect = self.strip_rect(i, size);
            if tracks[i].kind == TrackKind::Folder {
                self.paint_folder_strip(p, rect, tracks[i], model);
            } else {
                // Channels are numbered, folders not.
                let number = tracks[..=i]
                    .iter()
                    .filter(|t| t.kind != TrackKind::Folder)
                    .count();
                self.paint_strip(p, rect, tracks[i], number, model);
            }
            // What a strip is in: a band of each folder's colour along its
            // foot, the innermost lowest.
            for (k, f) in model.project().folder_chain(tracks[i]).iter().enumerate() {
                p.fill(
                    Rect::new(rect.x, rect.bottom() - 3.0 - 4.0 * k as f32, rect.w, 3.0),
                    track_color(f.color).with_alpha(0.85),
                );
            }
        }
        if let Some(Drag::Track {
            track,
            pos,
            moved: true,
            ..
        }) = self.drag
            && let Some(StripDrop::Into { strip, .. }) = self.track_drop(model, size, track, pos)
        {
            p.fill(strip, theme.ui.accent.with_alpha(0.18));
            p.stroke_rounded(strip.inset(1.0), 3.0, 2.0, theme.ui.accent);
        }
        if let Some(Drag::Track {
            track,
            pos,
            moved: true,
            ..
        }) = self.drag
            && let Some(StripDrop::Between { x, .. }) = self.track_drop(model, size, track, pos)
        {
            p.fill(Rect::new(x - 1.5, 0.0, 3.0, size.h), theme.ui.accent);
        }
        // The edge being dragged (or under the pointer).
        let edge = match (self.drag, self.hover) {
            (Some(Drag::Width { track, .. }), _) | (_, Some(Hit::Width(track))) => Some(track),
            _ => None,
        };
        if let Some(id) = edge
            && let Some(i) = tracks.iter().position(|t| t.id == id)
        {
            let r = self.strip_rect(i, size);
            p.fill(
                Rect::new(
                    r.right(),
                    0.0,
                    self.theme.console.strip_gap.max(2.0),
                    size.h,
                ),
                theme.ui.accent.with_alpha(0.8),
            );
        }
        // The "+" after the last strip.
        if !self.master_only {
            let add = self.add_track_rect(tracks.len());
            let hot = self.hover == Some(Hit::AddTrack);
            let fill = if hot {
                theme.ui.accent.with_alpha(0.25)
            } else {
                theme.ui.surface
            };
            p.fill_rounded(add, add.w / 2.0, &faderframe_ui_canvas::Paint::Solid(fill));
            p.stroke_rounded(add, add.w / 2.0, 1.0, theme.ui.border);
            let c = add.center();
            let arm = add.w * 0.22;
            let ink = if hot {
                theme.ui.text
            } else {
                theme.ui.text_dim
            };
            p.fill(Rect::new(c.x - arm, c.y - 1.0, 2.0 * arm, 2.0), ink);
            p.fill(Rect::new(c.x - 1.0, c.y - arm, 2.0, 2.0 * arm), ink);
        }
        if tracks.is_empty() {
            let style =
                faderframe_ui_canvas::TextStyle::new(theme.fonts.normal, theme.ui.text_faint)
                    .center();
            p.text(
                "No channels — add a track from the Track menu",
                viewport,
                &style,
            );
        }
        p.pop_clip();
        // Gap and master section.
        let master = self.master_rect(size);
        if !self.master_only && !self.hide_master {
            let gap = Rect::new(master.x - MASTER_GAP, 0.0, MASTER_GAP, size.h);
            p.fill(gap, theme.ui.border);
            p.shadow(master, 0.0, Color::rgba(0.0, 0.0, 0.0, 0.6), -2.0, 0.0, 6.0);
        }
        if let Some(m) = model.project().master()
            && !self.hide_master
        {
            self.paint_strip(p, master, m, 0, model);
        }
        if cheek > 0.0 {
            controls::wood_cheek(p, Rect::new(0.0, 0.0, cheek, size.h), theme);
            controls::wood_cheek(p, Rect::new(size.w - cheek, 0.0, cheek, size.h), theme);
        }
        self.paint_insert_drag(p, size, model);
    }

    fn event_body(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        self.update_sends(model);
        self.update_strips(model);
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                modifiers,
                clicks,
            } => self.press(pos, clicks, modifiers, size, model, cx),
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => self.secondary(pos, size, model, cx),
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } => {
                match self.drag {
                    Some(Drag::Track {
                        track,
                        origin,
                        moved,
                        ..
                    }) => {
                        let moved = moved || pos.distance(origin) >= 4.0;
                        self.drag = Some(Drag::Track {
                            track,
                            origin,
                            pos,
                            moved,
                        });
                        if moved {
                            cx.set_cursor(Cursor::Grabbing);
                        }
                        cx.redraw();
                    }
                    Some(Drag::Fader {
                        track,
                        start_y,
                        start_pos,
                    }) => {
                        let Some(l) = self.layout_of(model, track, size) else {
                            return true;
                        };
                        let geo = FaderGeometry::new(l.fader, &self.theme);
                        let scale = if modifiers.fine() { 0.2 } else { 1.0 };
                        let pos_new = start_pos + (start_y - pos.y) / geo.travel().max(1.0) * scale;
                        cx.emit(Action::Edit(Command::SetTrackVolume {
                            track,
                            db: self.law.position_to_db(pos_new.clamp(0.0, 1.0)),
                        }));
                    }
                    Some(Drag::Surround {
                        track,
                        start,
                        from,
                        size,
                    }) => {
                        // Fine: a quarter as far.
                        let k = if modifiers.fine() { 0.5 } else { 2.0 } / size;
                        let pan = faderframe_core::SurroundPan {
                            x: (from.x + (pos.x - start.x) * k).clamp(-1.0, 1.0),
                            y: (from.y - (pos.y - start.y) * k).clamp(-1.0, 1.0),
                            ..from
                        };
                        cx.emit(Action::Edit(Command::SetTrackSurround { track, pan }));
                    }
                    Some(Drag::Knob {
                        track,
                        target,
                        start_y,
                        start_value,
                    }) => {
                        if let Some(t) = Self::track(model, track) {
                            let v = start_value
                                + controls::drag_delta(pos.y - start_y, modifiers.fine());
                            if let Some(cmd) = self.knob_command(t, target, v) {
                                cx.emit(Action::Edit(cmd));
                            }
                        }
                    }
                    Some(Drag::Insert {
                        track,
                        plugin,
                        origin,
                        moved,
                        ..
                    }) => {
                        let moved = moved || pos.distance(origin) >= 4.0;
                        if moved {
                            cx.set_cursor(Cursor::Grabbing);
                        }
                        self.drag = Some(Drag::Insert {
                            track,
                            plugin,
                            origin,
                            pos,
                            moved,
                        });
                        cx.redraw();
                    }
                    Some(Drag::Width {
                        track,
                        start_x,
                        start,
                        all,
                    }) => {
                        let (lo, hi) = faderframe_session::STRIP_WIDTH_RANGE;
                        let w = (start + pos.x - start_x).round().clamp(lo, hi);
                        cx.emit(Action::SetStripWidth {
                            track: (!all).then_some(track),
                            width: Some(w),
                        });
                        cx.set_cursor(Cursor::ResizeHorizontal);
                    }
                    Some(Drag::InsertSlots { start_y, start }) => {
                        let (lo, hi) = faderframe_session::INSERT_SLOTS_RANGE;
                        let n = (start as f32 + ((pos.y - start_y) / INSERT_SLOT_STEP).round())
                            .clamp(lo as f32, hi as f32) as usize;
                        if n != self.insert_slots {
                            self.insert_slots = n;
                            cx.emit(Action::SetMixerInsertSlots(n as u16));
                        }
                    }
                    Some(Drag::Scroll {
                        start_x,
                        start_scroll,
                    }) => {
                        self.scroll_x = start_scroll - (pos.x - start_x);
                        let n = Self::channel_tracks(model).len();
                        self.clamp_scroll(n, size);
                        cx.redraw();
                    }
                    None => {}
                }
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let hit = self.hit_test(pos, size, model);
                if hit != self.hover {
                    self.hover = hit;
                    cx.set_cursor(match hit {
                        Some(Hit::FaderCap(_) | Hit::Scribble(_)) => Cursor::Grab,
                        Some(
                            Hit::FaderTrack(_)
                            | Hit::Pan(_)
                            | Hit::PreampKnob(..)
                            | Hit::Send(..)
                            | Hit::InsertsGrip(_),
                        ) => Cursor::ResizeVertical,
                        Some(Hit::Width(_)) => Cursor::ResizeHorizontal,
                        Some(Hit::Strip(_)) | None => Cursor::Default,
                        Some(_) => Cursor::Pointer,
                    });
                }
                false
            }
            ViewEvent::PointerUp {
                pos: up_pos,
                modifiers: up_mods,
                ..
            } => {
                match self.drag.take() {
                    Some(Drag::Track {
                        track, moved: true, ..
                    }) => {
                        match self.track_drop(model, size, track, up_pos) {
                            Some(StripDrop::Between { after, before, .. }) => {
                                cx.emit(Action::PlaceTrack {
                                    track,
                                    after,
                                    before,
                                });
                            }
                            Some(StripDrop::Into { folder, .. }) => {
                                cx.emit(Action::MoveToFolder {
                                    tracks: vec![track],
                                    folder: Some(folder),
                                });
                            }
                            None => {}
                        }
                        cx.set_cursor(Cursor::Default);
                        cx.redraw();
                    }
                    Some(Drag::Fader { .. } | Drag::Knob { .. } | Drag::Surround { .. }) => {
                        cx.emit(Action::EndGesture);
                        cx.set_cursor(Cursor::Default);
                    }
                    Some(Drag::InsertSlots { .. } | Drag::Width { .. }) => {
                        cx.set_cursor(Cursor::Default);
                        cx.redraw();
                    }
                    Some(Drag::Insert {
                        track,
                        plugin,
                        pos: _,
                        moved,
                        ..
                    }) => {
                        cx.set_cursor(Cursor::Default);
                        cx.redraw();
                        if let Some(a) =
                            self.insert_drop(model, size, track, plugin, moved, up_pos, up_mods)
                        {
                            cx.emit(a);
                        }
                    }
                    _ => {}
                }
                true
            }
            ViewEvent::Key {
                key: faderframe_ui_canvas::Key::Escape,
                ..
            }
            | ViewEvent::FocusLost
                if matches!(self.drag, Some(Drag::Track { .. })) =>
            {
                self.drag = None;
                cx.set_cursor(Cursor::Default);
                cx.redraw();
                true
            }
            ViewEvent::PointerLeave => {
                self.hover = None;
                false
            }
            ViewEvent::Scroll {
                pos,
                dx,
                dy,
                modifiers,
                precise,
            } => {
                let steps = if precise { dy / 20.0 } else { dy };
                match self.hit_test(pos, size, model) {
                    Some(Hit::FaderCap(id) | Hit::FaderTrack(id)) if dx == 0.0 => {
                        if let Some(t) = Self::track(model, id) {
                            let step = if modifiers.fine() { 0.1 } else { 0.5 };
                            let base = if t.volume_db <= SILENCE_DB {
                                -80.0
                            } else {
                                t.volume_db
                            };
                            cx.emit(Action::Edit(Command::SetTrackVolume {
                                track: id,
                                db: base - steps * step,
                            }));
                        }
                        true
                    }
                    // Into a bed the wheel moves the track left and right.
                    Some(Hit::Pan(id))
                        if dx == 0.0
                            && Self::track(model, id)
                                .is_some_and(|t| model.project().surround_panned(t).is_some()) =>
                    {
                        if let Some(t) = Self::track(model, id) {
                            let step = if modifiers.fine() { 0.01 } else { 0.05 };
                            let from = model.shown_surround(t);
                            let pan =
                                faderframe_core::SurroundParam::X.set(from, from.x - steps * step);
                            cx.emit(Action::Edit(Command::SetTrackSurround { track: id, pan }));
                        }
                        true
                    }
                    Some(Hit::Pan(id) | Hit::Send(id, _) | Hit::PreampKnob(id, _)) if dx == 0.0 => {
                        let target = match self.hit_test(pos, size, model) {
                            Some(Hit::PreampKnob(_, id)) => KnobTarget::Preamp(id),
                            Some(Hit::Send(_, i)) => KnobTarget::Send(i),
                            _ => KnobTarget::Pan,
                        };
                        if let Some(t) = Self::track(model, id)
                            && let Some(v) = self.knob_value(t, target)
                        {
                            let step = if modifiers.fine() { 0.005 } else { 0.025 };
                            if let Some(cmd) = self.knob_command(t, target, v - steps * step) {
                                cx.emit(Action::Edit(cmd));
                            }
                        }
                        true
                    }
                    _ => {
                        let delta = if dx != 0.0 { dx } else { dy };
                        let px = if precise {
                            delta
                        } else {
                            delta * self.pitch() * 0.5
                        };
                        self.scroll_x += px;
                        let n = Self::channel_tracks(model).len();
                        self.clamp_scroll(n, size);
                        cx.redraw();
                        true
                    }
                }
            }
            _ => false,
        }
    }

    fn tooltip_body(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        self.hit_test(pos, size, model)
            .and_then(|h| self.tooltip_for(h, model))
    }
}

impl CanvasView<Session, Action> for MixerView {
    fn accessible(
        &self,
        size: Size,
        model: &Session,
    ) -> Vec<faderframe_ui_canvas::AccessNode<Action>> {
        self.access_nodes(size, model)
    }

    fn accessible_name(&self) -> Option<String> {
        Some(if self.master_only { "Master" } else { "Mixer" }.into())
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.base_theme = theme.clone();
        self.theme = self.looked();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let b = Self::bridge_h(model);
        if b <= 0.0 {
            return self.paint_body(p, size, model, theme);
        }
        let body = Size::new(size.w, (size.h - b).max(0.0));
        p.push_transform(0.0, b, 1.0);
        self.paint_body(p, body, model, theme);
        p.pop_transform();
        self.paint_bridge(p, size.w, b, body, model);
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let b = Self::bridge_h(model);
        if b <= 0.0 {
            return self.event_body(ev, size, model, cx);
        }
        let body = Size::new(size.w, (size.h - b).max(0.0));
        // Presses in the bridge are its own; everything else (and every
        // drag) goes to the strips below it.
        if let ViewEvent::PointerDown { pos, button, .. } = *ev
            && pos.y < b
        {
            if button == PointerButton::Secondary {
                let track = self.bridge_track_at(pos.x, body, model);
                cx.request(HostRequest::ContextMenu {
                    at: pos,
                    items: meter_menu(model, track),
                });
            }
            return true;
        }
        self.event_body(&shifted(ev, b), body, model, cx)
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.is_animating() || matches!(self.drag, Some(Drag::Track { moved: true, .. }))
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let b = Self::bridge_h(model);
        let body = Size::new(size.w, (size.h - b).max(0.0));
        if pos.y < b {
            let track = self.bridge_track_at(pos.x, body, model)?;
            return Some(format!(
                "{} · Right-click: meters",
                meter_reading_vu(model, track)
            ));
        }
        self.tooltip_body(Point::new(pos.x, pos.y - b), body, model)
    }

    fn min_size(&self) -> Size {
        if self.master_only {
            return Size::new(self.theme.console.master_width + 2.0 * self.cheek(), 330.0);
        }
        Size::new(self.theme.console.master_width + self.pitch() * 2.0, 330.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        (axis == ScrollAxis::Horizontal && !self.master_only).then(|| ScrollInfo {
            content: self.content_w(Self::channel_tracks(model).len()),
            viewport: self.viewport_w(size),
            offset: self.scroll_x,
            // The strips: after the cheek, before the pinned master.
            start: self.cheek(),
            end: (size.w - self.cheek() - self.viewport_w(size)).max(0.0),
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Horizontal {
            self.scroll_x = offset.max(0.0);
        }
    }
}

/// A strip's meter as its mode shows it: peaks (with the RMS inside),
/// a VU needle, the EBU quasi-peak or a K-System meter.
fn paint_meter(
    p: &mut dyn Painter,
    rect: Rect,
    channels: &[faderframe_session::MeterChannel],
    mode: MeterMode,
    reference: f32,
    th: &Theme,
) {
    use controls::MeterZones;
    let floor = faderframe_session::METER_FLOOR_DB;
    match mode {
        MeterMode::Vu => {
            // A skin with edgewise meters shows needles; the others light
            // their LEDs, bars or columns on the VU's scale.
            if th.console.look.meter == faderframe_ui_canvas::MeterKind::Edgewise {
                let needles: Vec<(f32, bool)> = channels
                    .iter()
                    .map(|c| (controls::vu_deflection(c.vu_db(reference)), c.clipped))
                    .collect();
                controls::vu_edgewise(p, rect, &needles, th);
            } else {
                let levels: Vec<MeterLevel> = channels
                    .iter()
                    .map(|c| MeterLevel::new(c.vu_db(reference) + reference, floor, c.clipped))
                    .collect();
                controls::meter_zoned(p, rect, &levels, MeterZones::Vu(reference), th);
            }
        }
        MeterMode::Ppm => {
            let levels: Vec<MeterLevel> = channels
                .iter()
                .map(|c| MeterLevel::new(c.ppm_db, floor, c.clipped))
                .collect();
            controls::meter_zoned(p, rect, &levels, MeterZones::Ppm, th);
        }
        MeterMode::K20 | MeterMode::K14 | MeterMode::K12 => {
            // The RMS bar, the peak as a line above it.
            let zero = mode.k_zero().unwrap_or(-20.0);
            let levels: Vec<MeterLevel> = channels
                .iter()
                .map(|c| MeterLevel::new(c.rms_db, c.level_db, c.clipped))
                .collect();
            controls::meter_zoned(p, rect, &levels, MeterZones::K(zero), th);
        }
        MeterMode::Peak | MeterMode::PeakRms => {
            let levels: Vec<MeterLevel> = channels
                .iter()
                .map(|c| MeterLevel {
                    inner_db: (mode == MeterMode::PeakRms).then_some(c.rms_db),
                    ..MeterLevel::new(c.level_db, c.hold_db, c.clipped)
                })
                .collect();
            controls::meter_zoned(p, rect, &levels, MeterZones::Digital, th);
        }
    }
}

/// What a strip's meter reads now, in its mode's terms ("VU · −3.2 VU").
fn meter_reading(model: &Session, track: TrackId) -> String {
    let m = model.meter(track);
    let mode = model.meter_mode(track);
    let channels: Vec<faderframe_session::MeterChannel> = if m.count > 2 {
        m.shown().to_vec()
    } else {
        vec![m.left, m.right]
    };
    let loudest = |f: &dyn Fn(&faderframe_session::MeterChannel) -> f32| {
        channels.iter().map(f).fold(f32::MIN, f32::max)
    };
    let db = |v: f32| {
        if v <= faderframe_session::METER_FLOOR_DB + 0.01 {
            "−∞".to_string()
        } else {
            format!("{v:+.1}").replace('-', "−")
        }
    };
    let reference = model.vu_reference();
    match mode {
        MeterMode::Peak => format!("Peak meter · {} dBFS", db(loudest(&|c| c.level_db))),
        MeterMode::PeakRms => format!(
            "Peak + RMS · peak {} dBFS, RMS {} dBFS",
            db(loudest(&|c| c.level_db)),
            db(loudest(&|c| c.rms_db))
        ),
        MeterMode::Vu => format!(
            "VU meter · {} VU (0 VU = {} dBFS RMS)",
            db(loudest(&|c| c.vu_db(reference)).max(-40.0)),
            db(reference)
        ),
        MeterMode::Ppm => format!(
            "PPM (EBU) · {} (TEST = −18 dBFS)",
            db(loudest(&|c| c.ppm_db) + 18.0)
        ),
        MeterMode::K20 | MeterMode::K14 | MeterMode::K12 => {
            let zero = mode.k_zero().unwrap_or(-20.0);
            format!(
                "{} · {} (RMS), peak {} dBFS",
                mode.label(),
                db(loudest(&|c| c.rms_db) - zero),
                db(loudest(&|c| c.level_db))
            )
        }
    }
}

/// The meter menu: what this strip's meter shows (or every strip's), the
/// 0 VU reference, the meter bridge, the clip indicators.
fn meter_menu(model: &Session, track: Option<TrackId>) -> Vec<MenuItem<Action>> {
    let now = track.map(|t| model.meter_mode(t));
    let mut items: Vec<MenuItem<Action>> = MeterMode::ALL
        .iter()
        .map(|&mode| {
            MenuItem::new(
                mode.label(),
                Action::SetMeterMode {
                    track,
                    mode: Some(mode),
                },
            )
            .checked(now == Some(mode))
        })
        .collect();
    if track.is_some() {
        items.push(
            MenuItem::submenu(
                "Every Strip",
                MeterMode::ALL
                    .iter()
                    .map(|&mode| {
                        MenuItem::new(
                            mode.label(),
                            Action::SetMeterMode {
                                track: None,
                                mode: Some(mode),
                            },
                        )
                    })
                    .collect(),
            )
            .separated(),
        );
    }
    let reference = model.vu_reference();
    items.push(MenuItem::submenu(
        "0 VU Reference",
        [-20.0f32, -18.0, -16.0, -14.0, -12.0]
            .into_iter()
            .map(|r| {
                MenuItem::new(
                    format!("{} dBFS RMS", format!("{r:.0}").replace('-', "−")),
                    Action::SetVuReference(r),
                )
                .checked((reference - r).abs() < 0.01)
            })
            .collect(),
    ));
    items.push(
        MenuItem::new(
            "VU Meter Bridge",
            Action::SetMeterBridge(!model.meter_bridge()),
        )
        .checked(model.meter_bridge())
        .separated(),
    );
    items.push(MenuItem::new("Clear Clip Indicators", Action::ResetClipIndicators).separated());
    items
}

#[cfg(test)]
mod tests;
