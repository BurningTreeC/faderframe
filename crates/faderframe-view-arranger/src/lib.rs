//! The arranger: a virtualised viewport onto the project timeline.
//!
//! Only rows and clips intersecting the viewport are visited; waveforms are
//! drawn as one filled path per clip channel from the multi-resolution peak
//! cache, so thousands of clips and millions of peaks stay cheap. Track
//! headers reuse the console controls so the arranger and the mixer share a
//! visual language.

#![forbid(unsafe_code)]

mod access;
mod automation;
mod clip_edit;
pub mod edit_bar;
mod global;
mod harmony;
mod header;
mod scratch;
mod video_lane;

pub use automation::AUTO_LANE_H;
pub use clip_edit::ClipZone;
pub use global::{GlobalHit, SectionPart};
pub use header::HeaderLayout;

use faderframe_core::gain::{SILENCE_DB, format_db};
use faderframe_core::pan::format_pan;
use faderframe_core::{ClipId, FaderLaw, TrackId, db_to_gain};
use faderframe_project::{
    Clip, ClipContent, Command, MonitorMode, MusicalRange, OutputRouting, TakeFolder, Track,
    TrackColor, TrackKind,
};
use faderframe_session::{Action, SelectMode, Session, TransportAction};
use faderframe_timeline::{
    GridDensity, GridDivision, GridLineKind, MusicalTime, bar_stride, snap_floor, visible_lines,
};
use faderframe_ui_canvas::controls::{self, KnobLook, MeterLevel};
use faderframe_ui_canvas::{
    Align, CanvasView, ClipStyle, Color, Cursor, EventCx, FaderKind, FontFamily, GridEmphasis,
    HostRequest, Key, MenuItem, Modifiers, Paint, Painter, Path, Point, PointerButton, Rect,
    ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const MIN_PPQ: f32 = 1.5;
/// A new project's zoom.
const DEFAULT_PPQ: f32 = 34.0;
/// Deep enough to see single samples (≈16 px per sample at 120 BPM and
/// 48 kHz) for redrawing them with the Pencil.
const MAX_PPQ: f32 = 400_000.0;
const LOOP_BAND: f32 = 11.0;
/// Ruler labels are at least this far apart.
const RULER_LABEL_PX: f32 = 44.0;
/// Below the bars, a level of grid lines is labelled in the ruler once its
/// lines (with the stronger ones) are this far apart.
const RULER_FINE_LABEL_PX: f32 = 76.0;

/// A grid line kind's place in the hierarchy (0: bars … 3: fine lines).
fn grid_level(kind: GridLineKind) -> usize {
    match kind {
        GridLineKind::Bar => 0,
        GridLineKind::Beat => 1,
        GridLineKind::Subdivision => 2,
        GridLineKind::Fine => 3,
    }
}

/// The lines below the bars the ruler labels once every bar is labelled
/// (`every` is 1, so zoomed in): beats first, then the grid's lines, then
/// fine lines, each where it is at least [`RULER_FINE_LABEL_PX`] from
/// every label already placed (the bars' included) — so labels never
/// crowd and, zoomed in to single samples, still name the positions.
fn ruler_labels(
    lines: &[(MusicalTime, GridLineKind)],
    x_of: impl Fn(MusicalTime) -> f32,
    every: i32,
) -> Vec<MusicalTime> {
    if every != 1 {
        return Vec::new();
    }
    let mut placed: Vec<f32> = lines
        .iter()
        .filter(|l| l.1 == GridLineKind::Bar)
        .map(|l| x_of(l.0))
        .collect();
    let mut out = Vec::new();
    for kind in [
        GridLineKind::Beat,
        GridLineKind::Subdivision,
        GridLineKind::Fine,
    ] {
        for &(t, k) in lines {
            let x = x_of(t);
            if k == kind && placed.iter().all(|p| (p - x).abs() >= RULER_FINE_LABEL_PX) {
                placed.push(x);
                out.push(t);
            }
        }
    }
    out
}

/// A ruler label below the bars: "bar.beat", or with display ticks (960 a
/// quarter, decimals when finer) for lines between the beats.
fn ruler_label(
    meter: &faderframe_timeline::TimeSignatureMap,
    t: MusicalTime,
    kind: GridLineKind,
) -> String {
    let b = meter.to_bbt(t);
    if kind == GridLineKind::Beat || b.tick == 0 {
        return format!("{}.{}", b.bar + 1, b.beat + 1);
    }
    let ticks = b.tick as f64 * 960.0 / faderframe_timeline::TICKS_PER_QUARTER as f64;
    let text = if ticks.fract().abs() < 1e-9 {
        format!("{:03}", ticks as i64)
    } else {
        let s = format!("{ticks:07.3}");
        s.trim_end_matches('0').to_string()
    };
    format!("{}.{}.{text}", b.bar + 1, b.beat + 1)
}
const DRAG_THRESHOLD: f32 = 3.0;
/// Height of one take lane under an open take folder.
/// Width of the take-folder disclosure triangle in a clip header.
const DISCLOSURE_W: f32 = 15.0;
/// Header controls never spread over more than this height.
const HEADER_MAX_H: f32 = 96.0;
/// Height of the resize grip at a track's bottom edge (header column).
const RESIZE_GRIP: f32 = 4.0;
/// How far a track's header moves right per folder it is in.
const FOLDER_INDENT: f32 = 12.0;
/// A folder's row unless it is sized.
const FOLDER_ROW_H: f32 = 44.0;
/// Track height presets (View → Track Height, track context menu).
pub const TRACK_HEIGHTS: [(&str, f32); 4] = [
    ("Small", 44.0),
    ("Medium", 72.0),
    ("Large", 120.0),
    ("Extra Large", 200.0),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderPart {
    Name,
    Mute,
    Solo,
    Record,
    Monitor,
    Pan,
    Volume,
    Meter,
    /// The bottom edge: drag to change the track height.
    Resize,
    /// Show/hide the automation lanes.
    Automation,
    /// The colour stripe: opens the colour chooser.
    Color,
    /// A folder's triangle: opens or closes it.
    Fold,
    /// A folder's Group switch: what it holds is edited and armed as one.
    Group,
    Body,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Corner,
    /// The arrangement switcher in the corner (the song or a scratch pad).
    Arrangement,
    /// The markers, arranger, signature and tempo lanes.
    Global(GlobalHit),
    LoopBand(MusicalTime),
    LoopStart,
    LoopEnd,
    /// The Play Start Marker's flag (in the loop band).
    PlayStart,
    Ruler(MusicalTime),
    Header(TrackId, HeaderPart),
    Clip {
        clip: ClipId,
        track: TrackId,
        at: MusicalTime,
    },
    Lane {
        track: TrackId,
        at: MusicalTime,
    },
    /// The divider between the track headers and the lanes.
    HeaderEdge,
    /// The disclosure triangle of a take folder.
    TakeToggle(ClipId),
    /// An automation lane row (`header`: in the header column).
    Automation {
        track: TrackId,
        lane: faderframe_core::AutomationLaneId,
        header: bool,
    },
    /// A take lane of an open take folder; `pos` is the folder frame.
    TakeLane {
        clip: ClipId,
        track: TrackId,
        take: usize,
        pos: i64,
    },
    /// A take lane's ▶: hear that take instead of the comp.
    TakeAudition {
        clip: ClipId,
        take: usize,
    },
    /// The bottom edge of a track's take lanes: drag to size them.
    TakeLanesEdge(TrackId),
    Empty(MusicalTime),
    /// The "+" under the last track header.
    AddTrack,
}

/// Where a dragged track row lands.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RowDrop {
    /// Between two rows (as `Action::PlaceTrack` takes them), the line at `y`.
    Between {
        after: Option<TrackId>,
        before: Option<TrackId>,
        y: f32,
    },
    /// Onto a folder: into it.
    Into { folder: TrackId, row: Rect },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// A track's header dragged up or down: to reorder, or onto a folder.
    Row {
        track: TrackId,
        origin_y: f32,
        moved: bool,
        pos: Point,
    },
    /// Moving the playhead; `audible` (the Scrub tool) plays snippets.
    Scrub {
        audible: bool,
    },
    Loop {
        anchor: MusicalTime,
        moved: bool,
    },
    LoopEdge {
        start: bool,
        range: MusicalRange,
        offset: MusicalTime,
    },
    /// The Play Start Marker's flag; `offset`: from the marker to where it
    /// was grabbed.
    PlayStart {
        offset: MusicalTime,
        moved: bool,
        origin: Point,
    },
    Volume {
        track: TrackId,
        origin_x: f32,
        start: f32,
        width: f32,
    },
    Pan {
        track: TrackId,
        origin_y: f32,
        start: f32,
    },
    /// A dual-panned track's header knob: both channels move by as much
    /// (the image as a whole), from where they were at the press.
    DualPan {
        track: TrackId,
        origin_y: f32,
        start: faderframe_core::DualPan,
    },
    Pan2D {
        origin: Point,
        sx: f64,
        sy: f32,
    },
    HeaderWidth {
        start_x: f32,
        start_w: f32,
    },
    Height {
        track: TrackId,
        start_y: f32,
        start_h: f32,
        /// Alt: every track at once.
        all: bool,
    },
    /// A track's take lanes sized by their bottom edge (`lanes` of them).
    TakeLanes {
        track: TrackId,
        start_y: f32,
        start_h: f32,
        lanes: usize,
    },
}

/// Ctrl+drag: a rectangle selecting the clips under it (kept outside
/// `Drag`, which is `Copy`).
struct Marquee {
    origin: Point,
    current: Point,
    moved: bool,
    /// Ctrl+Shift: added to what was selected at the press.
    base: Vec<ClipId>,
    base_tracks: Vec<TrackId>,
    /// Pressed on a clip: a click (no drag) toggles it, as a Ctrl-click
    /// always did.
    click: Option<(ClipId, TrackId)>,
    /// Pressed on a lane or below the tracks: a click moves the playhead.
    at: Option<MusicalTime>,
}

/// Swipe comping in progress (kept outside `Drag`, which is `Copy`).
struct CompDrag {
    clip: ClipId,
    take: usize,
    /// The folder frame pressed (a click swaps the section there).
    at: i64,
    anchor: i64,
    origin: Point,
    moved: bool,
    /// The folder before the swipe; every move recomputes from it.
    base: TakeFolder,
}

/// Audio of one source placed on the timeline, for waveform drawing.
struct WaveSpan {
    /// The clip it is (part of).
    clip: ClipId,
    source: faderframe_core::AudioSourceId,
    /// Engine sample of the first frame.
    start: i64,
    /// Source frame (project rate) at `start`.
    source_offset: i64,
    /// Length in project frames.
    length: i64,
    gain: f32,
    /// Warp anchors (output → source, project frames) of a warped clip.
    warp: Option<Vec<faderframe_project::WarpMarker>>,
}

/// Engine samples of a (non-warped) span ↔ the source's own frames.
/// Width of a waveform's kept tiles (logical pixels).
const WAVE_TILE: f32 = 256.0;

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameMap {
    pub source: faderframe_core::AudioSourceId,
    /// Engine sample of the span's first frame.
    pub start: i64,
    /// Length in engine samples.
    pub len: i64,
    /// Source frame at `start`.
    pub src_off: f64,
    /// Source frames per engine sample.
    pub pr: f64,
    pub gain: f32,
}

impl FrameMap {
    fn of(span: &WaveSpan, model: &Session) -> Self {
        let ratio = model.frame_ratio();
        let pr = model.peak_rate(span.source) / model.sample_rate() as f64;
        Self {
            source: span.source,
            start: span.start,
            len: (span.length as f64 * ratio) as i64,
            src_off: span.source_offset as f64 * ratio * pr,
            pr,
            gain: span.gain,
        }
    }

    /// Source frame at engine sample `s`.
    pub fn frame_at(&self, s: i64) -> i64 {
        (self.src_off + (s - self.start) as f64 * self.pr).round() as i64
    }

    /// Engine sample of source frame `f`.
    pub fn sample_of(&self, f: i64) -> i64 {
        self.start + ((f as f64 - self.src_off) / self.pr).round() as i64
    }

    /// Source frames the span covers.
    pub fn frames(&self) -> (i64, i64) {
        (
            self.frame_at(self.start),
            self.frame_at(self.start + self.len),
        )
    }
}

pub struct ArrangerView {
    theme: Theme,
    /// Zoom: pixels per quarter note.
    ppq: f32,
    /// Pixels from the timeline start (f64: sample-level zoom deep into a
    /// project needs the precision).
    scroll_x: f64,
    /// A scrollbar is held: the view does not follow the playhead.
    bar_held: bool,
    /// The transport's jump count last seen (a jump brings the playhead
    /// into view).
    seen_jumps: Option<u32>,
    scroll_y: f32,
    drag: Option<Drag>,
    hover: Option<Hit>,
    law: FaderLaw,
    /// Files being dragged in: target track (`None` = new tracks) and time.
    drop_at: Option<(Option<TrackId>, MusicalTime)>,
    comp: Option<CompDrag>,
    marquee: Option<Marquee>,
    auto_drag: Option<automation::AutoDrag>,
    /// Track header column width (from the layout, else the theme).
    header_width: f32,
    /// Row tops (content coordinates) of the lane tracks, plus the end;
    /// rows grow while take lanes are open. Refreshed per paint/event.
    rows: Vec<f32>,
    /// Clip editing drag (trims, fades, gain, warp, ranges, moves).
    edit_drag: Option<clip_edit::EditDrag>,
    /// The clip zone under the pointer (cursor and handle highlight).
    zone_hover: Option<(ClipId, ClipZone)>,
    /// Last zoom request handled.
    zoom_seen: u64,
    /// View width at the last paint/event.
    view_w: f32,
    /// Shown global lanes (from the editor settings).
    lanes: faderframe_session::lanes::GlobalLanes,
    global_drag: Option<global::GlobalDrag>,
    video_drag: Option<video_lane::VideoDrag>,
    /// Rows of the Video lane (a video track each).
    video_rows: usize,
    /// The object the running drag snapped to (Snap to Objects): its guide
    /// line is drawn.
    snap_guide: Option<faderframe_session::SnapHit>,
    /// The arrangement shown when last looked (song or scratch pad).
    arrangement_seen: Option<Option<faderframe_core::ScratchPadId>>,
    /// Each arrangement's scroll, zoom and vertical scroll when it was
    /// left.
    places: std::collections::HashMap<Option<faderframe_core::ScratchPadId>, (f64, f32, f32)>,
    /// The project last looked at ([`Session::project_serial`]).
    opened_seen: Option<u64>,
}

/// A folder header's triangle.
fn fold_rect(l: &HeaderLayout) -> Rect {
    Rect::new(l.name.x - 2.0, l.name.y, 16.0, l.name.h)
}

/// A folder's Group switch, right of its solo button (where a track's
/// record button is).
fn folder_group_rect(l: &HeaderLayout) -> Rect {
    l.record
}

/// A folder bus's badge, right of its Group switch (a folder's header has
/// no other buttons there).
fn folder_bus_badge(l: &HeaderLayout) -> Rect {
    let g = folder_group_rect(l);
    Rect::new(g.right() + 6.0, g.center().y - 6.5, 28.0, 13.0)
}

/// The tooltip of a folder's Group switch.
fn group_tip(model: &Session, t: &Track) -> String {
    let p = model.project();
    let outer = p
        .edit_group(t)
        .filter(|g| g.id != t.id)
        .map(|g| g.name.clone());
    let state = match (t.folder_group, outer) {
        (_, Some(o)) => format!("Grouped with the tracks of '{o}' (its Group is on)"),
        (true, None) => "Group on: its tracks are edited and armed as one".into(),
        (false, None) => "Group off".into(),
    };
    format!(
        "{state} · Click to switch · When on, arming, selecting and editing clips or a range on \
         one of its tracks reach the others"
    )
}

/// A folder's name, right of its triangle.
fn folder_name_rect(l: &HeaderLayout) -> Rect {
    Rect::new(
        l.name.x + 16.0,
        l.name.y,
        (l.name.w - 16.0).max(10.0),
        l.name.h,
    )
}

/// The lead sheet entries of a clip's menu: of this clip, and of the
/// selected clips it is one of (those of a track on one staff; on several
/// tracks a score, a staff a track).
fn lead_sheet_items(model: &Session, clip: &Clip, targets: &[ClipId]) -> Vec<MenuItem<Action>> {
    use faderframe_session::leadsheet::{Grid, LeadSheetOf};
    let make = |of| Action::MakeLeadSheet {
        of,
        grid: Grid::Auto,
    };
    let tab = |of| Action::MakeTab { of };
    let mut items = vec![
        MenuItem::new(
            "Lead Sheet (Melody, Chords, Words)",
            make(LeadSheetOf::Clip(clip.id)),
        ),
        MenuItem::new("Guitar Tab", tab(LeadSheetOf::Clip(clip.id))),
    ];
    let p = model.project();
    let mut clips: Vec<ClipId> = targets
        .iter()
        .copied()
        .filter(|c| p.clip(*c).is_some())
        .collect();
    clips.sort();
    if clips.len() > 1 {
        let mut tracks: Vec<TrackId> = clips
            .iter()
            .filter_map(|c| p.clip(*c).map(|c| c.track))
            .collect();
        tracks.sort();
        tracks.dedup();
        let (label, tab_label) = if tracks.len() > 1 {
            (
                format!(
                    "Score of the Selected Clips ({} Tracks, a Staff Each)",
                    tracks.len()
                ),
                format!("Score with Guitar Tab ({} Tracks)", tracks.len()),
            )
        } else {
            (
                format!("Lead Sheet of the {} Selected Clips", clips.len()),
                format!("Guitar Tab of the {} Selected Clips", clips.len()),
            )
        };
        items.push(MenuItem::new(
            label,
            make(LeadSheetOf::Clips(clips.clone())),
        ));
        items.push(MenuItem::new(tab_label, tab(LeadSheetOf::Clips(clips))));
    }
    items
}

/// The tracks a score of the selection has, top to bottom: the selected
/// tracks with clips that hold notes or audio.
fn score_tracks(model: &Session) -> Vec<TrackId> {
    let p = model.project();
    p.folder_order()
        .into_iter()
        .filter(|t| {
            model.selection.tracks.contains(&t.id)
                && matches!(
                    t.kind,
                    TrackKind::Audio | TrackKind::Midi | TrackKind::Instrument
                )
                && !p.clips_of(t.id).is_empty()
        })
        .map(|t| t.id)
        .collect()
}

/// The entries making a sample of a track's selection (audio tracks).
/// A menu entry the session describes, as a menu item.
fn entry_item(e: faderframe_session::GroupMenuEntry) -> MenuItem<Action> {
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

fn sample_items(model: &Session, track: TrackId, clip: Option<ClipId>) -> Vec<MenuItem<Action>> {
    model
        .sample_choices(track, clip)
        .into_iter()
        .map(|(label, action, separated)| {
            let item = MenuItem::new(label, action);
            if separated { item.separated() } else { item }
        })
        .collect()
}

fn color_of(c: TrackColor) -> Color {
    Color::rgb8(c.r, c.g, c.b)
}

fn clip_fits(track: &Track, clip: &Clip) -> bool {
    match clip.content {
        ClipContent::Audio(_) | ClipContent::Takes(_) => track.kind == TrackKind::Audio,
        ClipContent::Midi(_) => matches!(track.kind, TrackKind::Instrument | TrackKind::Midi),
    }
}

impl ArrangerView {
    pub fn new(theme: Theme) -> Self {
        let header_width = theme.arranger.header_width;
        Self {
            theme,
            ppq: DEFAULT_PPQ,
            scroll_x: 0.0,
            bar_held: false,
            seen_jumps: None,
            scroll_y: 0.0,
            drag: None,
            hover: None,
            law: FaderLaw::console(),
            drop_at: None,
            comp: None,
            marquee: None,
            auto_drag: None,
            header_width,
            rows: Vec::new(),
            edit_drag: None,
            zone_hover: None,
            zoom_seen: 0,
            view_w: f32::MAX,
            // No lyrics or video yet: their lanes show once there are some.
            lanes: faderframe_session::lanes::GlobalLanes {
                lyrics: false,
                video: false,
                ..Default::default()
            },
            global_drag: None,
            video_drag: None,
            video_rows: 1,
            snap_guide: None,
            arrangement_seen: None,
            places: std::collections::HashMap::new(),
            opened_seen: None,
        }
    }

    // --- coordinates -------------------------------------------------------------

    fn header_w(&self) -> f32 {
        self.header_width
    }

    /// Pick up the saved header width (called before painting/events).
    fn update_header_width(&mut self, model: &Session) {
        self.follow_arrangement(model);
        self.lanes = model.editor.lanes;
        // The Lyrics lane shows once there are lyrics, the Video lane once
        // there is video.
        if model.project().lyrics.is_empty() {
            self.lanes.lyrics = false;
        }
        if !video_lane::has_video(model) {
            self.lanes.video = false;
        }
        self.video_rows = Self::video_rows(model);
        self.header_width = model
            .header_width()
            .unwrap_or(self.theme.arranger.header_width);
    }

    /// The ruler and the global lanes under it.
    fn ruler_h(&self) -> f32 {
        self.base_ruler_h() + self.global_lanes_h()
    }

    fn row_h(&self) -> f32 {
        self.theme.arranger.track_height
    }

    pub fn x_of(&self, t: MusicalTime) -> f32 {
        self.header_w() + (t.quarters() * self.ppq as f64 - self.scroll_x) as f32
    }

    pub fn time_at(&self, x: f32) -> MusicalTime {
        MusicalTime::from_quarters(((x - self.header_w()) as f64 + self.scroll_x) / self.ppq as f64)
    }

    /// The tracks with lanes: in folder order (a folder's tracks under
    /// it), those in closed folders left out.
    fn lane_tracks(model: &Session) -> Vec<&Track> {
        model
            .project()
            .folder_order()
            .into_iter()
            .filter(|t| t.kind != TrackKind::Master && model.track_shown(t))
            .collect()
    }

    /// A track's header at `y`, moved right by the folders it is in.
    fn header_rect(&self, model: &Session, t: &Track, y: f32) -> Rect {
        let depth = model.project().folder_chain(t).len() as f32;
        let x = (depth * FOLDER_INDENT).min(self.header_w() * 0.4);
        Rect::new(x, y, self.header_w() - x, self.header_h(model, t.id))
    }

    /// Take lanes shown under a track (the most takes of its open folders).
    fn open_lanes(t: &Track, model: &Session) -> usize {
        model
            .project()
            .clips_of(t.id)
            .into_iter()
            .filter(|c| model.takes_open(c.id))
            .filter_map(|c| c.as_takes())
            .map(|f| f.takes.len())
            .max()
            .unwrap_or(0)
    }

    /// Height of a track's main lane (without take lanes).
    fn base_h(&self, model: &Session, track: TrackId) -> f32 {
        model.track_height(track).unwrap_or_else(|| {
            // Folders (folder buses too) are compact unless sized.
            let folder = model.project().track(track).is_some_and(Track::is_folder);
            if folder {
                FOLDER_ROW_H.min(self.row_h())
            } else {
                self.row_h()
            }
        })
    }

    /// Height of the header controls area (tall tracks keep their
    /// controls at the top).
    fn header_h(&self, model: &Session, track: TrackId) -> f32 {
        self.base_h(model, track).min(HEADER_MAX_H)
    }

    /// Where track `i`'s own part ends (its take and automation lanes, or
    /// the next track, follow): the edge that sizes it.
    fn resize_edge(&self, model: &Session, i: usize, track: TrackId, size: Size) -> f32 {
        self.row_rect(i, size).y + self.base_h(model, track)
    }

    /// The track whose resize edge is under `pos` in the header column
    /// (a few pixels either side of it).
    fn resize_hit(&self, model: &Session, pos: Point, size: Size) -> Option<TrackId> {
        if pos.x >= self.header_w() || pos.y < self.ruler_h() {
            return None;
        }
        let tracks = Self::lane_tracks(model);
        self.visible_rows(tracks.len(), size)
            .find(|&i| {
                (pos.y - self.resize_edge(model, i, tracks[i].id, size)).abs() <= RESIZE_GRIP
            })
            .map(|i| tracks[i].id)
    }

    /// The track whose take lanes' bottom edge is under `pos` (lanes
    /// open; anywhere along the row).
    fn take_edge_hit(&self, model: &Session, pos: Point, size: Size) -> Option<TrackId> {
        if pos.y < self.ruler_h() {
            return None;
        }
        let tracks = Self::lane_tracks(model);
        self.visible_rows(tracks.len(), size)
            .find(|&i| {
                let n = Self::open_lanes(tracks[i], model);
                n > 0 && (pos.y - self.take_lanes_bottom(model, i, tracks[i], size)).abs() <= 3.0
            })
            .map(|i| tracks[i].id)
    }

    /// Where row `i`'s take lanes end.
    fn take_lanes_bottom(&self, model: &Session, i: usize, t: &Track, size: Size) -> f32 {
        self.resize_edge(model, i, t.id, size)
            + Self::open_lanes(t, model) as f32 * model.take_lane_height(t.id)
    }

    /// A take lane's ▶ (none on lanes too low for it).
    fn audition_rect(&self, clip_rect: Rect, lane_y: f32, lane_h: f32) -> Option<Rect> {
        (lane_h >= 18.0).then(|| {
            Rect::new(
                clip_rect.x.max(self.header_w()) + 3.0,
                lane_y + 3.0,
                18.0,
                14.0,
            )
        })
    }

    fn update_rows(&mut self, model: &Session) {
        self.rows.clear();
        let mut y = 0.0;
        self.rows.push(y);
        for t in Self::lane_tracks(model) {
            y += self.base_h(model, t.id)
                + Self::open_lanes(t, model) as f32 * model.take_lane_height(t.id)
                + model.shown_lanes(t.id).len() as f32 * AUTO_LANE_H;
            self.rows.push(y);
        }
    }

    /// Top of row `i` in content coordinates (rows past the end continue
    /// at the base height, e.g. for the "new track" drop hint).
    fn row_top(&self, i: usize) -> f32 {
        match self.rows.get(i) {
            Some(y) => *y,
            None => {
                let last = self.rows.len().saturating_sub(1);
                self.rows.last().copied().unwrap_or(0.0) + (i - last) as f32 * self.row_h()
            }
        }
    }

    fn total_rows_height(&self) -> f32 {
        self.rows.last().copied().unwrap_or(0.0)
    }

    fn row_at(&self, y: f32) -> Option<usize> {
        let rel = y - self.ruler_h() + self.scroll_y;
        if rel < 0.0 {
            return None;
        }
        let total = self.total_rows_height();
        if rel >= total {
            let n = self.rows.len().saturating_sub(1);
            return Some(n + ((rel - total) / self.row_h()) as usize);
        }
        Some(self.rows.partition_point(|&o| o <= rel).saturating_sub(1))
    }

    fn row_rect(&self, i: usize, size: Size) -> Rect {
        let top = self.row_top(i);
        let h = match (self.rows.get(i), self.rows.get(i + 1)) {
            (Some(a), Some(b)) => b - a,
            _ => self.row_h(),
        };
        Rect::new(0.0, self.ruler_h() + top - self.scroll_y, size.w, h)
    }

    pub fn visible_rows(&self, count: usize, size: Size) -> std::ops::Range<usize> {
        if self.rows.len() != count + 1 {
            let first = (self.scroll_y / self.row_h()).floor().max(0.0) as usize;
            let last = ((self.scroll_y + size.h - self.ruler_h()) / self.row_h())
                .ceil()
                .max(0.0) as usize;
            return first.min(count)..last.min(count);
        }
        let top = self.scroll_y;
        let bottom = self.scroll_y + size.h - self.ruler_h();
        let first = self.rows.partition_point(|&o| o <= top).saturating_sub(1);
        let last = self.rows.partition_point(|&o| o < bottom);
        first.min(count)..last.min(count)
    }

    fn content_quarters(&self, model: &Session) -> f64 {
        let p = model.project();
        let end = p
            .content_end()
            .max(p.loop_range.map_or(MusicalTime::ZERO, |r| r.end));
        end.quarters() + 32.0
    }

    /// The "+" (add a track) under the last of `count` track headers.
    fn add_track_rect(&self, count: usize, size: Size) -> Rect {
        let top = self.row_rect(count, size).y;
        Rect::new(8.0, top + 8.0, (self.header_w() - 16.0).max(40.0), 30.0)
    }

    /// The tracks a "+" offers, as a menu at `at`.
    fn add_track_menu(model: &Session, at: Point) -> HostRequest<Action> {
        let items = model
            .add_track_choices()
            .into_iter()
            .map(|(label, action, group)| {
                let item = MenuItem::new(label, action);
                if group { item.separated() } else { item }
            })
            .collect();
        HostRequest::ContextMenu { at, items }
    }

    /// Where a track row dragged to `pos` lands: onto a folder's row when
    /// over its middle (not its own, nor one inside it), else between the
    /// rows either side of the nearest gap. None where it would stay put.
    fn row_drop(&self, model: &Session, size: Size, track: TrackId, pos: Point) -> Option<RowDrop> {
        let p = model.project();
        let tracks = Self::lane_tracks(model);
        let moving = p.track(track)?;
        // What moves with it: the track and what it holds.
        let block = |t: &Track| t.id == track || p.in_folder(t, track);
        let n = tracks.len();
        if n == 0 {
            return None;
        }
        let rect = |i: usize| self.row_rect(i, size);
        // The row under the pointer (clamped to the first and last).
        let y = pos.y;
        let i = (0..n).find(|&i| y < rect(i).bottom()).unwrap_or(n - 1);
        let r = rect(i);
        let t = tracks[i];
        if t.is_folder()
            && !block(t)
            && (r.y + r.h * 0.3..r.y + r.h * 0.7).contains(&y)
            && moving.folder != Some(t.id)
        {
            return Some(RowDrop::Into {
                folder: t.id,
                row: r,
            });
        }
        let gap = if y < r.y + r.h * 0.5 { i } else { i + 1 };
        let gap = if y < rect(0).y { 0 } else { gap };
        let after = gap.checked_sub(1).map(|k| tracks[k]);
        let before = tracks.get(gap).copied();
        // Next to itself (or inside what it holds): it stays.
        if after.is_some_and(block) || before.is_some_and(block) {
            return None;
        }
        let line = if gap < n {
            rect(gap).y
        } else {
            rect(n - 1).bottom()
        };
        Some(RowDrop::Between {
            after: after.map(|t| t.id),
            before: before.map(|t| t.id),
            y: line,
        })
    }

    fn clamp_scroll(&mut self, model: &Session, size: Size) {
        let lanes_w = (size.w - self.header_w()).max(1.0);
        let max_x =
            (self.content_quarters(model) * self.ppq as f64 - lanes_w as f64 * 0.5).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
        let rows = Self::lane_tracks(model).len() as f32;
        let total = if self.rows.len() > 1 {
            self.total_rows_height()
        } else {
            rows * self.row_h()
        };
        let max_y = (total - (size.h - self.ruler_h()) + self.row_h()).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
    }

    /// The grid lines drawn over `[start, end)` at this zoom: bars, beats
    /// and the edit grid's lines — the positions snapping goes to — thinned
    /// zoomed out, with finer lines (nothing snaps to) zoomed in, down to
    /// single samples ([`faderframe_timeline::for_each_visible_line`]).
    pub(crate) fn grid_lines(
        &self,
        start: MusicalTime,
        end: MusicalTime,
        model: &Session,
    ) -> Vec<(MusicalTime, GridLineKind)> {
        visible_lines(
            start,
            end,
            model.editor.grid,
            &model.project().timeline.meter,
            f64::from(self.ppq),
            GridDensity::default(),
        )
    }

    /// Bars per ruler label (labels at least [`RULER_LABEL_PX`] apart).
    fn ruler_every(&self, model: &Session) -> i32 {
        bar_stride(
            self.time_at(self.header_w()).max(MusicalTime::ZERO),
            &model.project().timeline.meter,
            f64::from(self.ppq),
            f64::from(RULER_LABEL_PX),
        )
    }

    /// Clip rect in view coordinates (may extend beyond the viewport).
    fn clip_rect(&self, clip: &Clip, row: Rect, model: &Session) -> Rect {
        let p = model.project();
        let end = clip.end(&p.timeline, p.sample_rate);
        let x0 = self.x_of(clip.start);
        let x1 = self.x_of(end);
        Rect::new(
            x0,
            row.y + 3.0,
            (x1 - x0).max(2.0),
            self.base_h(model, clip.track) - 6.0,
        )
    }

    pub fn hit_test(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        let at = self.time_at(pos.x).max(MusicalTime::ZERO);
        if pos.y >= self.ruler_h() && (pos.x - self.header_w()).abs() <= 3.0 {
            return Some(Hit::HeaderEdge);
        }
        if pos.y >= self.base_ruler_h() && pos.y < self.ruler_h() {
            return self.global_hit(pos, size, model).map(Hit::Global);
        }
        if pos.y < self.ruler_h() {
            return Some(if pos.x < self.header_w() {
                if self.arrangement_rect(model).contains(pos) {
                    Hit::Arrangement
                } else {
                    Hit::Corner
                }
            } else if pos.y < LOOP_BAND {
                // The flag (its line and the pennant right of it) before
                // a loop edge, except where the edge is closer than its
                // line.
                let flag = model.project().play_start.map(|m| pos.x - self.x_of(m.at));
                let on_flag = flag.is_some_and(|d| (-3.0..=10.0).contains(&d));
                if let Some(range) = model.project().loop_range {
                    let left = (pos.x - self.x_of(range.start)).abs();
                    let right = (pos.x - self.x_of(range.end)).abs();
                    let edge = left.min(right);
                    let flag_first = on_flag && flag.is_some_and(|d| d > 3.0 || d.abs() <= edge);
                    if edge <= 6.0 && !flag_first {
                        return Some(if left <= right {
                            Hit::LoopStart
                        } else {
                            Hit::LoopEnd
                        });
                    }
                }
                if on_flag {
                    return Some(Hit::PlayStart);
                }
                Hit::LoopBand(at)
            } else {
                Hit::Ruler(at)
            });
        }
        if let Some(track) = self.resize_hit(model, pos, size) {
            return Some(Hit::Header(track, HeaderPart::Resize));
        }
        if let Some(track) = self.take_edge_hit(model, pos, size) {
            return Some(Hit::TakeLanesEdge(track));
        }
        let tracks = Self::lane_tracks(model);
        let Some(i) = self.row_at(pos.y).filter(|&i| i < tracks.len()) else {
            if self.add_track_rect(tracks.len(), size).contains(pos) {
                return Some(Hit::AddTrack);
            }
            return (pos.x >= self.header_w()).then_some(Hit::Empty(at));
        };
        let t = tracks[i];
        let row = self.row_rect(i, size);
        if let Some((g, part)) = self.auto_hit(model, t, row, pos) {
            return Some(Hit::Automation {
                track: t.id,
                lane: g.lane,
                header: part.is_some(),
            });
        }
        if pos.x < self.header_w() {
            let l = HeaderLayout::new(self.header_rect(model, t, row.y));
            if t.is_folder() {
                let part = [
                    (fold_rect(&l), HeaderPart::Fold),
                    (l.mute, HeaderPart::Mute),
                    (l.solo, HeaderPart::Solo),
                    (folder_group_rect(&l), HeaderPart::Group),
                    (folder_name_rect(&l), HeaderPart::Name),
                    (
                        Rect::new(l.stripe.x, l.stripe.y, l.stripe.w + 3.0, l.stripe.h),
                        HeaderPart::Color,
                    ),
                ]
                .into_iter()
                .find(|(r, _)| r.contains(pos))
                .map_or(HeaderPart::Body, |(_, p)| p);
                return Some(Hit::Header(t.id, part));
            }
            // Pan, fader and meter only where they are painted (not on
            // MIDI tracks).
            let audio = t.kind.has_audio();
            let fader = audio || t.kind == TrackKind::Vca;
            let part = [
                (Some(l.mute), HeaderPart::Mute),
                (Some(l.solo), HeaderPart::Solo),
                (Some(l.record), HeaderPart::Record),
                (Some(l.monitor), HeaderPart::Monitor),
                (Some(l.automation), HeaderPart::Automation),
                (l.pan.filter(|_| audio), HeaderPart::Pan),
                (
                    fader.then(|| l.volume.inset_xy(-2.0, -3.0)),
                    HeaderPart::Volume,
                ),
                (fader.then_some(l.meter), HeaderPart::Meter),
                (Some(l.name), HeaderPart::Name),
                (
                    Some(Rect::new(
                        l.stripe.x,
                        l.stripe.y,
                        l.stripe.w + 3.0,
                        l.stripe.h,
                    )),
                    HeaderPart::Color,
                ),
            ]
            .into_iter()
            .find(|(r, _)| r.is_some_and(|r| r.contains(pos)))
            .map_or(HeaderPart::Body, |(_, p)| p);
            return Some(Hit::Header(t.id, part));
        }
        let p = model.project();
        for clip in p.clips_of(t.id).into_iter().rev() {
            let rect = self.clip_rect(clip, row, model);
            if rect.contains(pos) {
                if clip.as_takes().is_some() && self.disclosure_rect(rect).contains(pos) {
                    return Some(Hit::TakeToggle(clip.id));
                }
                return Some(Hit::Clip {
                    clip: clip.id,
                    track: t.id,
                    at,
                });
            }
            // Take lanes below the main lane.
            if let Some(f) = clip.as_takes()
                && model.takes_open(clip.id)
                && pos.x >= rect.x
                && pos.x < rect.right()
            {
                let lanes_top = row.y + self.base_h(model, t.id);
                let lane_h = model.take_lane_height(t.id);
                let k = ((pos.y - lanes_top) / lane_h).floor();
                if k >= 0.0
                    && (k as usize) < f.takes.len()
                    && self
                        .audition_rect(rect, lanes_top + k * lane_h, lane_h)
                        .is_some_and(|b| b.contains(pos))
                {
                    return Some(Hit::TakeAudition {
                        clip: clip.id,
                        take: k as usize,
                    });
                }
                if k >= 0.0 && (k as usize) < f.takes.len() {
                    let rate = p.sample_rate as f64;
                    let frame =
                        p.timeline.to_samples(at, rate) - p.timeline.to_samples(clip.start, rate);
                    return Some(Hit::TakeLane {
                        clip: clip.id,
                        track: t.id,
                        take: k as usize,
                        pos: frame.clamp(0, f.length),
                    });
                }
            }
        }
        Some(Hit::Lane { track: t.id, at })
    }

    fn disclosure_rect(&self, clip: Rect) -> Rect {
        let h = self.theme.arranger.clip_header.min(clip.h * 0.5);
        Rect::new(clip.x + 2.0, clip.y, DISCLOSURE_W, h)
    }

    /// x of an engine sample position.
    fn x_of_sample(&self, model: &Session, sample: i64) -> f32 {
        self.x_of(model.engine().samples_to_musical(model.project(), sample))
    }

    /// Engine sample at view x.
    pub(crate) fn sample_at_x(&self, model: &Session, x: f32) -> i64 {
        model
            .engine()
            .musical_to_samples(model.project(), self.time_at(x))
    }

    /// Pixels per engine sample when zoomed in to sample level (single
    /// samples are drawn and the Pencil redraws them).
    pub(crate) fn sample_zoom(&self, model: &Session) -> Option<f32> {
        let x = self.header_w() + 10.0;
        let n = self.sample_at_x(model, x + 1000.0) - self.sample_at_x(model, x);
        let px = 1000.0 / n.max(1) as f32;
        (px >= 2.0).then_some(px)
    }

    /// Where a clip's waveform is drawn inside its rectangle.
    pub(crate) fn clip_content_rect(&self, rect: Rect) -> Rect {
        let header_h = self.theme.arranger.clip_header.min(rect.h * 0.5);
        Rect::new(rect.x, rect.y + header_h, rect.w, rect.h - header_h).inset_xy(0.0, 2.0)
    }

    /// Waveform lanes of `area`: stereo is stacked when there is room.
    pub(crate) fn wave_lanes(area: Rect, channels: usize) -> Vec<(Rect, Vec<usize>)> {
        let channels = channels.min(2);
        if channels == 2 && area.h >= 40.0 {
            let (top, bottom) = area.split_top(area.h / 2.0);
            vec![(top, vec![0]), (bottom, vec![1])]
        } else {
            vec![(area, (0..channels).collect())]
        }
    }

    /// Single samples as a line (dots when far apart), at sample-level
    /// zoom.
    #[allow(clippy::too_many_arguments)]
    fn paint_samples(
        &self,
        p: &mut dyn Painter,
        area: Rect,
        vis: (f32, f32),
        map: &FrameMap,
        px: f32,
        model: &Session,
        color: Color,
    ) {
        let s0 = self.sample_at_x(model, vis.0).max(map.start);
        let s1 = self.sample_at_x(model, vis.1).min(map.start + map.len);
        if s1 <= s0 || area.h < 6.0 {
            return;
        }
        let (f0, f1) = (map.frame_at(s0) - 1, map.frame_at(s1) + 2);
        let count = (f1 - f0).clamp(0, 20_000) as usize;
        let Some(data) = model.source_frames(map.source, f0, count) else {
            return;
        };
        let (first, last) = map.frames();
        let gain = map.gain.min(4.0);
        let line = color.lighten(0.45);
        for (lane, chans) in Self::wave_lanes(area, data.len()) {
            let mid = lane.center().y;
            let half = lane.h * 0.46;
            p.hline(vis.0, vis.1, mid, color.lighten(0.3).with_alpha(0.25));
            for &ch in &chans {
                let mut path = Path::new();
                let mut started = false;
                for (i, v) in data[ch].iter().enumerate() {
                    let f = f0 + i as i64;
                    if f < first || f >= last {
                        continue;
                    }
                    let x = self.x_of_sample(model, map.sample_of(f));
                    let y = mid - (v * gain).clamp(-1.0, 1.0) * half;
                    let pt = Point::new(x, y);
                    if started {
                        path.line_to(pt);
                    } else {
                        path.move_to(pt);
                        started = true;
                    }
                    if px >= 6.0 {
                        p.circle(pt, 1.8, line);
                    }
                }
                if started {
                    p.stroke_path(&path, 1.3, line);
                }
            }
        }
    }

    // --- painting ------------------------------------------------------------------

    fn paint_grid(&self, p: &mut dyn Painter, lanes: Rect, model: &Session) {
        let a = &self.theme.arranger;
        let look = self.theme.grid;
        let scale = p.scale_factor();
        let start = self.time_at(lanes.x).max(MusicalTime::ZERO);
        let end = self.time_at(lanes.right());
        let styles = [
            (look.color(a.bar_line), GridEmphasis::Bar),
            (look.color(a.beat_line), GridEmphasis::Beat),
            (look.color(a.sub_line), GridEmphasis::Subdivision),
            (look.color(a.fine_line), GridEmphasis::Fine),
        ];
        for (t, kind) in self.grid_lines(start, end, model) {
            let (color, e) = styles[grid_level(kind)];
            p.fill(
                look.line_rect(self.x_of(t), lanes.y, lanes.bottom(), e, scale),
                color,
            );
        }
    }

    /// Waveform of a clip or a part of a take, clipped to `vis`.
    fn paint_waveform(
        &self,
        p: &mut dyn Painter,
        area: Rect,
        vis: (f32, f32),
        span: &WaveSpan,
        model: &Session,
        color: Color,
    ) {
        if span.warp.is_none()
            && let Some(px) = self.sample_zoom(model)
        {
            let map = FrameMap::of(span, model);
            self.paint_samples(p, area, vis, &map, px, model, color);
            return;
        }
        let Some(peaks) = model.peaks(span.source) else {
            return;
        };
        let ratio = model.frame_ratio();
        let len = (span.length as f64 * ratio) as i64;
        // Peak frames per engine frame (streamed files keep their own rate).
        let pr = model.peak_rate(span.source) / model.sample_rate() as f64;
        let src_off = (span.source_offset as f64 * ratio * pr) as i64;
        let channels = peaks.channels();
        // Drawn through kept tiles (unwarped: what a tile shows depends
        // only on these; not while a drag changes them every frame).
        let tiles = (span.warp.is_none() && !self.reshaping(span.clip)).then(|| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            span.source.hash(&mut h);
            (std::sync::Arc::as_ptr(peaks) as usize).hash(&mut h);
            (span.start, span.source_offset, len).hash(&mut h);
            (span.gain.to_bits(), self.ppq.to_bits(), area.h.to_bits()).hash(&mut h);
            [color.r, color.g, color.b, color.a]
                .map(f32::to_bits)
                .hash(&mut h);
            // The skin's part in the fill (cards mix in its text colour).
            let text = self.theme.ui.text;
            (self.theme.arranger.clip_style as u8).hash(&mut h);
            [text.r, text.g, text.b, text.a]
                .map(f32::to_bits)
                .hash(&mut h);
            model.project().timeline.tempo.fingerprint().hash(&mut h);
            model.sample_rate().hash(&mut h);
            h.finish()
        });
        self.fill_wave_tiled(
            p,
            area,
            vis,
            span.start,
            len,
            channels,
            span.gain,
            model,
            color,
            tiles,
            |ch, s0, s1| {
                let (a, b) = match &span.warp {
                    // Through the time map: engine → project → source frames.
                    Some(pts) => {
                        let src = |s: i64| {
                            faderframe_project::Warp::source_at_points(pts, s as f64 / ratio)
                                - span.source_offset as f64
                        };
                        let a = (src(s0) * ratio * pr) as i64;
                        (a, ((src(s1) * ratio * pr) as i64).max(a + 1))
                    }
                    None => {
                        let a = (s0 as f64 * pr) as i64;
                        (a, ((s1 as f64 * pr) as i64).max(a + 1))
                    }
                };
                peaks.min_max(ch, src_off + a, src_off + b)
            },
        );
    }

    /// [`Self::fill_wave`] in tiles of [`WAVE_TILE`] pixels from the
    /// start, each a picture the painter keeps under `tiles` (the key of
    /// all else it depends on) and its place: the playhead passing over a
    /// waveform then redraws a picture, not its path. `None`: drawn as is.
    #[allow(clippy::too_many_arguments)]
    fn fill_wave_tiled(
        &self,
        p: &mut dyn Painter,
        area: Rect,
        vis: (f32, f32),
        start: i64,
        len: i64,
        channels: usize,
        gain: f32,
        model: &Session,
        color: Color,
        tiles: Option<u64>,
        mut peak: impl FnMut(usize, i64, i64) -> Option<(f32, f32)>,
    ) {
        let Some(base) = tiles else {
            return self.fill_wave(p, area, vis, start, len, channels, gain, model, color, peak);
        };
        let xs = self.x_of_sample(model, start);
        let xe = self.x_of_sample(model, start + len);
        let (x0, x1) = (xs.max(vis.0), xe.min(vis.1));
        if area.h < 6.0 || x1 <= x0 || len <= 0 {
            return;
        }
        // Neighbouring tiles overlap by a step (no seam between them).
        let step = (1.0 / p.scale_factor().max(1.0)).max(0.5);
        let first = ((x0 - xs) / WAVE_TILE).floor() as i64;
        let last = ((x1 - xs) / WAVE_TILE).ceil() as i64;
        for i in first..last {
            let tx0 = xs + i as f32 * WAVE_TILE;
            let tx1 = (tx0 + WAVE_TILE).min(xe);
            let rect = Rect::new(tx0, area.y, tx1 - tx0, area.h);
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (base, i, rect.w.to_bits()).hash(&mut h);
                h.finish()
            };
            faderframe_ui_canvas::kept(p, key, rect, |p| {
                self.fill_wave(
                    p,
                    area,
                    ((tx0 - step).max(xs), (tx1 + step).min(xe)),
                    start,
                    len,
                    channels,
                    gain,
                    model,
                    color,
                    &mut peak,
                );
            });
        }
    }

    /// Fill a min/max waveform for engine samples `start..start + len`,
    /// clipped to `vis`. `peak(channel, a, b)` gets sample offsets relative
    /// to `start`. Stereo material is stacked when there is room.
    #[allow(clippy::too_many_arguments)]
    fn fill_wave(
        &self,
        p: &mut dyn Painter,
        area: Rect,
        vis: (f32, f32),
        start: i64,
        len: i64,
        channels: usize,
        gain: f32,
        model: &Session,
        color: Color,
        mut peak: impl FnMut(usize, i64, i64) -> Option<(f32, f32)>,
    ) {
        let x0 = self.x_of_sample(model, start).max(vis.0);
        let x1 = self.x_of_sample(model, start + len).min(vis.1);
        if area.h < 6.0 || x1 <= x0 || len <= 0 {
            return;
        }
        let tl = &model.project().timeline;
        let sr = model.sample_rate() as f64;
        let gain = gain.min(4.0);
        let lanes = Self::wave_lanes(area, channels);
        let step = (1.0 / p.scale_factor().max(1.0)).max(0.5);
        let fill = color.lighten(0.3).with_alpha(0.9);
        for (lane, chans) in lanes {
            let mid = lane.center().y;
            let half = lane.h * 0.46;
            let mut top = Vec::new();
            let mut bottom = Vec::new();
            let mut x = x0;
            while x < x1 {
                let s0 = tl.to_samples(self.time_at(x), sr) - start;
                let s1 = tl.to_samples(self.time_at(x + step), sr) - start;
                let (s0, s1) = (s0.clamp(0, len), s1.clamp(0, len).max(s0.clamp(0, len) + 1));
                let mut lo = 0.0f32;
                let mut hi = 0.0f32;
                for &ch in &chans {
                    if let Some((a, b)) = peak(ch, s0, s1) {
                        lo = lo.min(a);
                        hi = hi.max(b);
                    }
                }
                let (lo, hi) = ((lo * gain).clamp(-1.0, 1.0), (hi * gain).clamp(-1.0, 1.0));
                top.push(Point::new(x, mid - hi * half - 0.25));
                bottom.push(Point::new(x, mid - lo * half + 0.25));
                x += step;
            }
            if top.is_empty() {
                continue;
            }
            let mut path = Path::new();
            path.move_to(top[0]);
            for q in &top[1..] {
                path.line_to(*q);
            }
            for q in bottom.iter().rev() {
                path.line_to(*q);
            }
            path.close();
            if self.theme.arranger.clip_style == ClipStyle::Card {
                // Lighter towards the peaks, the clip's colour at the
                // middle.
                let light = color.mix(self.theme.ui.text, 0.35).with_alpha(0.95);
                p.fill_path_paint(
                    &path,
                    &Paint::vertical_stops(
                        lane,
                        vec![
                            (0.0, light),
                            (0.5, color.lighten(0.08).with_alpha(0.85)),
                            (1.0, light),
                        ],
                    ),
                );
                p.hline(x0, x1, mid, color.with_alpha(0.3));
            } else {
                p.fill_path(&path, fill);
                p.hline(x0, x1, mid, color.lighten(0.3).with_alpha(0.25));
            }
        }
    }

    /// Engine sample where a clip starts.
    fn clip_start_sample(model: &Session, clip: &Clip) -> i64 {
        model
            .engine()
            .musical_to_samples(model.project(), clip.start)
    }

    /// Engine samples of `frames` project frames.
    fn engine_frames(model: &Session, frames: i64) -> i64 {
        (frames as f64 * model.frame_ratio()).round() as i64
    }

    /// The comp of a take folder in its main lane.
    #[allow(clippy::too_many_arguments)]
    fn paint_comp(
        &self,
        p: &mut dyn Painter,
        area: Rect,
        vis: (f32, f32),
        clip: &Clip,
        f: &TakeFolder,
        model: &Session,
        color: Color,
    ) {
        let base = Self::clip_start_sample(model, clip);
        for piece in f.pieces() {
            let take = &f.takes[piece.take];
            let span = WaveSpan {
                clip: clip.id,
                source: take.source,
                start: base + Self::engine_frames(model, piece.start),
                source_offset: take.source_offset + piece.start,
                length: piece.end - piece.start,
                gain: db_to_gain(f.gain_db + take.gain_db),
                warp: None,
            };
            self.paint_waveform(p, area, vis, &span, model, color);
        }
        // Comp boundaries.
        for (a, _, _) in f.segments().skip(1) {
            let x = self.x_of_sample(model, base + Self::engine_frames(model, a));
            if x > vis.0 && x < vis.1 {
                p.vline(x, area.y, area.bottom(), Color::rgba(1.0, 1.0, 1.0, 0.35));
            }
        }
    }

    /// Lanes of an open take folder: every take, comped parts highlighted.
    #[allow(clippy::too_many_arguments)]
    fn paint_take_lanes(
        &self,
        p: &mut dyn Painter,
        clip_rect: Rect,
        (lanes_top, lane_h): (f32, f32),
        lanes: Rect,
        clip: &Clip,
        f: &TakeFolder,
        model: &Session,
        color: Color,
    ) {
        let base = Self::clip_start_sample(model, clip);
        let vis = (
            clip_rect.x.max(lanes.x),
            clip_rect.right().min(lanes.right()),
        );
        if vis.1 <= vis.0 {
            return;
        }
        let pieces = f.pieces();
        let text = TextStyle::new(self.theme.fonts.small, Color::hex(0xe8e4dc));
        let auditioned = model
            .auditioned_take()
            .and_then(|(c, k)| (c == clip.id).then_some(k));
        for (k, take) in f.takes.iter().enumerate() {
            let lane = Rect::new(
                clip_rect.x,
                lanes_top + k as f32 * lane_h,
                clip_rect.w,
                lane_h - 1.0,
            );
            p.fill(
                Rect::new(vis.0, lane.y, vis.1 - vis.0, lane.h),
                color.darken(0.8).with_alpha(0.9),
            );
            let tx0 = self.x_of_sample(model, base + Self::engine_frames(model, take.start));
            let tx1 = self.x_of_sample(model, base + Self::engine_frames(model, take.end));
            let body = Rect::new(tx0, lane.y + 1.0, tx1 - tx0, lane.h - 2.0);
            p.fill_rounded(
                body,
                2.0,
                &Paint::Solid(color.darken(0.62).with_alpha(0.85)),
            );
            let wave = WaveSpan {
                clip: clip.id,
                source: take.source,
                start: base + Self::engine_frames(model, take.start),
                source_offset: take.source_offset + take.start,
                length: take.end - take.start,
                gain: db_to_gain(f.gain_db + take.gain_db),
                warp: None,
            };
            self.paint_waveform(
                p,
                body.inset_xy(0.0, 2.0),
                vis,
                &wave,
                model,
                color.darken(0.35),
            );
            // Comped parts of this take.
            for piece in pieces.iter().filter(|q| q.take == k) {
                let x0 = self
                    .x_of_sample(model, base + Self::engine_frames(model, piece.start))
                    .max(vis.0);
                let x1 = self
                    .x_of_sample(model, base + Self::engine_frames(model, piece.end))
                    .min(vis.1);
                if x1 <= x0 {
                    continue;
                }
                let r = Rect::new(x0, lane.y + 1.0, x1 - x0, lane.h - 2.0);
                p.fill(r, color.with_alpha(0.45));
                let span = WaveSpan {
                    clip: clip.id,
                    source: take.source,
                    start: base + Self::engine_frames(model, piece.start),
                    source_offset: take.source_offset + piece.start,
                    length: piece.end - piece.start,
                    gain: wave.gain,
                    warp: None,
                };
                self.paint_waveform(
                    p,
                    r.inset_xy(0.0, 2.0),
                    (x0, x1),
                    &span,
                    model,
                    color.lighten(0.2),
                );
                p.stroke_rounded(r, 1.5, 1.0, color.lighten(0.35));
            }
            // The ▶ that auditions it; the take heard is outlined.
            let mut label_x = vis.0 + 3.0;
            if let Some(b) = self.audition_rect(clip_rect, lane.y, lane_h)
                && b.right() < vis.1 - 4.0
            {
                let on = auditioned == Some(k);
                let accent = self.theme.ui.accent;
                p.fill_rounded(
                    b,
                    3.0,
                    &Paint::Solid(if on {
                        accent
                    } else {
                        Color::rgba(0.05, 0.05, 0.06, 0.72)
                    }),
                );
                let c = b.center();
                let mut tri = Path::new();
                tri.move_to(Point::new(c.x - 3.0, c.y - 4.0));
                tri.line_to(Point::new(c.x + 4.0, c.y));
                tri.line_to(Point::new(c.x - 3.0, c.y + 4.0));
                tri.close();
                p.fill_path(&tri, if on { Color::hex(0x141414) } else { text.color });
                label_x = b.right() + 3.0;
            }
            if auditioned == Some(k) {
                p.stroke_rounded(
                    Rect::new(vis.0, lane.y, vis.1 - vis.0, lane.h),
                    2.0,
                    1.5,
                    self.theme.ui.accent,
                );
            }
            // Label on a dark chip so it stays readable over comped parts.
            let label_text = format!("{} · {}", k + 1, take.name);
            let w = (p.text_width(&label_text, &text) + 10.0).min(vis.1 - label_x - 1.0);
            if w > 16.0 {
                let chip = Rect::new(label_x, lane.y + 3.0, w, 14.0);
                p.fill_rounded(
                    chip,
                    3.0,
                    &Paint::Solid(Color::rgba(0.05, 0.05, 0.06, 0.72)),
                );
                p.text(&label_text, chip.inset_xy(5.0, 0.0), &text);
            }
            p.hline(vis.0, vis.1, lane.bottom(), Color::rgba(0.0, 0.0, 0.0, 0.5));
        }
    }

    fn paint_midi_preview(
        &self,
        p: &mut dyn Painter,
        area: Rect,
        vis: (f32, f32),
        clip: &Clip,
        color: Color,
    ) {
        let Some(m) = clip.as_midi() else { return };
        if m.notes.is_empty() || area.h < 4.0 {
            return;
        }
        let lo = m.notes.iter().map(|n| n.key).min().unwrap_or(60) as f32;
        let hi = m.notes.iter().map(|n| n.key).max().unwrap_or(60) as f32;
        let range = (hi - lo + 1.0).max(12.0);
        let base = lo - ((range - (hi - lo + 1.0)) / 2.0).floor();
        let key_h = (area.h / range).min(6.0);
        let fill = color.lighten(0.45);
        for n in &m.notes {
            let x0 = self.x_of(clip.start + n.start);
            let x1 = self.x_of(clip.start + n.end());
            if x1 < vis.0 || x0 > vis.1 {
                continue;
            }
            let y = area.bottom() - (n.key as f32 - base + 1.0) * key_h;
            p.fill(
                Rect::new(x0, y, (x1 - x0).max(1.5), (key_h - 1.0).max(1.0)),
                fill,
            );
        }
    }

    fn paint_clip(
        &self,
        p: &mut dyn Painter,
        rect: Rect,
        lanes: Rect,
        clip: &Clip,
        track: &Track,
        model: &Session,
    ) {
        let a = &self.theme.arranger;
        let color = color_of(clip.color.unwrap_or(track.color));
        let r = a.clip_radius;
        let selected = model.selection.clips.contains(&clip.id);
        let card = a.clip_style == ClipStyle::Card;
        let glow = self.theme.console.look.glow;
        let header_h = a.clip_header.min(rect.h * 0.5);
        if card {
            // A translucent card tinted with the clip's colour.
            if selected && glow > 0.0 {
                p.shadow(
                    rect,
                    r,
                    a.selection_outline.with_alpha(0.3 * glow),
                    0.0,
                    0.0,
                    8.0,
                );
            }
            p.fill_rounded(
                rect,
                r,
                &Paint::vertical(
                    rect,
                    color.mix(a.background, 0.7).with_alpha(a.clip_alpha),
                    color.mix(a.background, 0.82).with_alpha(a.clip_alpha),
                ),
            );
        } else {
            p.shadow(rect, r, Color::rgba(0.0, 0.0, 0.0, 0.45), 0.0, 1.5, 4.0);
            // The name strip and the body below it, side by side (no
            // overlap: both translucent, the grid shows through evenly).
            let body = Rect::new(rect.x, rect.y + header_h, rect.w, rect.h - header_h);
            p.fill_path_paint(
                &Path::rounded_rect_ends(body, 0.0, r),
                &Paint::vertical(
                    body,
                    color.darken(0.52).with_alpha(a.clip_alpha),
                    color.darken(0.66).with_alpha(a.clip_alpha),
                ),
            );
        }
        p.push_clip(rect.intersection(&lanes));
        let header = Rect::new(rect.x, rect.y, rect.w, header_h);
        if card {
            // The name strip a step lighter, a fine line of the colour
            // along the rounded top.
            p.fill_path_paint(
                &Path::rounded_rect_ends(header, r, 0.0),
                &Paint::Solid(color.with_alpha(0.14)),
            );
            let rr = r.min(rect.w * 0.5).min(header_h).max(1.0);
            let mut top = Path::new();
            let pi = std::f32::consts::PI;
            top.arc(
                Point::new(rect.x + rr, rect.y + rr),
                rr - 1.0,
                pi,
                pi * 1.5,
                false,
            );
            top.arc(
                Point::new(rect.right() - rr, rect.y + rr),
                rr - 1.0,
                pi * 1.5,
                pi * 2.0,
                false,
            );
            p.stroke_path(&top, 2.0, color);
        } else {
            p.fill_path_paint(
                &Path::rounded_rect_ends(header, r, 0.0),
                &Paint::vertical(
                    header,
                    color.lighten(0.08).with_alpha(a.clip_header_alpha),
                    color.darken(0.05).with_alpha(a.clip_header_alpha),
                ),
            );
        }
        let vis = (rect.x.max(lanes.x), rect.right().min(lanes.right()));
        let text_color = if card {
            self.theme.card_ink(color)
        } else if color.luminance() > 0.42 {
            a.clip_text
        } else {
            Color::hex(0xf2f0ea)
        };
        let is_folder = clip.as_takes().is_some();
        let label_x = rect.x.max(lanes.x) + if is_folder { 5.0 + DISCLOSURE_W } else { 5.0 };
        // The name ends where the gain knob starts.
        let name_end = self
            .gain_badge(clip, rect)
            .map_or(rect.right(), |b| b.x)
            .min(rect.right());
        let name_rect = Rect::new(
            label_x,
            rect.y,
            (name_end - label_x - 4.0).max(0.0),
            header_h,
        );
        if let Some(f) = clip.as_takes() {
            // Disclosure triangle: open/close the take lanes.
            let d = self.disclosure_rect(rect);
            if d.x >= lanes.x - DISCLOSURE_W {
                let c = d.center();
                let mut tri = Path::new();
                if model.takes_open(clip.id) {
                    tri.move_to(Point::new(c.x - 4.0, c.y - 2.0))
                        .line_to(Point::new(c.x + 4.0, c.y - 2.0))
                        .line_to(Point::new(c.x, c.y + 3.0));
                } else {
                    tri.move_to(Point::new(c.x - 2.0, c.y - 4.0))
                        .line_to(Point::new(c.x + 3.0, c.y))
                        .line_to(Point::new(c.x - 2.0, c.y + 4.0));
                }
                tri.close();
                p.fill_path(&tri, text_color);
            }
            if name_rect.w > 12.0 {
                p.text(
                    &format!("{} · {} takes", clip.name, f.takes.len()),
                    name_rect,
                    &TextStyle::new(self.theme.fonts.small, text_color).bold(),
                );
            }
        } else if name_rect.w > 12.0 {
            let mut name_rect = name_rect;
            if track.is_time_locked() && name_rect.w > 30.0 {
                // Placed in time (its track is time-locked): a small
                // padlock before its name.
                let y = name_rect.center().y;
                controls::lock_glyph(
                    p,
                    Rect::new(name_rect.x, y - 4.5, 6.0, 9.0),
                    text_color.with_alpha(0.8),
                );
                name_rect = Rect::new(
                    name_rect.x + 10.0,
                    name_rect.y,
                    name_rect.w - 10.0,
                    name_rect.h,
                );
            }
            if model.is_alias(clip.id) && name_rect.w > 30.0 {
                // An alias: two overlapping frames before its name.
                let y = name_rect.center().y;
                let x = name_rect.x;
                for (dx, dy) in [(0.0, -1.5), (3.0, 1.5)] {
                    p.stroke_rounded(
                        Rect::new(x + dx, y - 3.0 + dy, 7.0, 5.0),
                        1.0,
                        1.0,
                        text_color.with_alpha(0.9),
                    );
                }
                name_rect = Rect::new(x + 14.0, name_rect.y, name_rect.w - 14.0, name_rect.h);
            }
            p.text(
                &clip.name,
                name_rect,
                &TextStyle::new(self.theme.fonts.small, text_color).bold(),
            );
        }
        let content = self.clip_content_rect(rect);
        match &clip.content {
            ClipContent::Audio(audio) => {
                let span = WaveSpan {
                    clip: clip.id,
                    source: audio.source,
                    start: Self::clip_start_sample(model, clip),
                    source_offset: audio.source_offset,
                    length: audio.length,
                    gain: db_to_gain(audio.gain_db),
                    warp: audio
                        .warp
                        .as_ref()
                        .map(|w| w.points(audio.source_offset, audio.length)),
                };
                self.paint_waveform(p, content, vis, &span, model, color);
                self.paint_redraw(p, model, clip.id, content, &span);
                self.paint_warp(p, model, clip, rect);
            }
            ClipContent::Midi(_) => self.paint_midi_preview(p, content, vis, clip, color),
            ClipContent::Takes(f) => self.paint_comp(p, content, vis, clip, f, model, color),
        }
        self.paint_fades(p, model, clip, rect, color);
        self.paint_gain(p, clip, rect, text_color);
        if clip.muted {
            p.fill(rect, Color::rgba(0.08, 0.08, 0.09, 0.6));
        }
        if track.freeze.is_some() {
            // Frozen: frosted, labelled.
            p.fill(rect, Color::rgba(0.62, 0.8, 1.0, 0.28));
            if rect.w > 70.0 {
                let r = Rect::new(rect.x.max(lanes.x) + 5.0, rect.bottom() - 15.0, 70.0, 12.0);
                p.text(
                    "❄ FROZEN",
                    r,
                    &TextStyle::new(self.theme.fonts.tiny, Color::hex(0xdff0ff)).bold(),
                );
            }
        }
        if clip
            .content
            .sources()
            .iter()
            .any(|s| model.is_source_missing(*s))
        {
            // Offline media: hatched, with a label.
            p.fill(content, Color::rgba(0.35, 0.05, 0.05, 0.55));
            let mut x = content.x - content.h;
            while x < content.right() {
                p.line(
                    Point::new(x, content.bottom()),
                    Point::new(x + content.h, content.y),
                    1.0,
                    Color::rgba(1.0, 0.4, 0.35, 0.25),
                );
                x += 9.0;
            }
            if content.w > 50.0 && content.h > 12.0 {
                p.text(
                    "OFFLINE",
                    Rect::new(content.x.max(lanes.x) + 5.0, content.y, 80.0, content.h),
                    &TextStyle::new(self.theme.fonts.small, Color::hex(0xffb4a8)).bold(),
                );
            }
        }
        p.pop_clip();
        if selected {
            p.stroke_rounded(rect, r, 1.6, a.selection_outline);
        } else if card {
            p.stroke_rounded(rect.inset(0.5), r, 1.0, color.with_alpha(0.32));
        } else {
            p.stroke_rounded(rect, r, 1.0, color.darken(0.25).with_alpha(0.8));
        }
    }

    /// A folder's header: its triangle, name, what it holds, mute and solo.
    /// A time-locked track's padlock before its name (in `name`, the
    /// name's rect); returns the rect left for the name.
    fn paint_time_lock(&self, p: &mut dyn Painter, t: &Track, name: Rect) -> Rect {
        if !t.is_time_locked() {
            return name;
        }
        let (glyph, rest) = header::lock_rect(name);
        controls::lock_glyph(p, glyph, self.theme.arranger.time_lock);
        rest
    }

    fn paint_folder_header(
        &self,
        p: &mut dyn Painter,
        l: &HeaderLayout,
        t: &Track,
        model: &Session,
    ) {
        let th = &self.theme;
        let c = &th.console;
        let open = model.folder_open(t.id);
        let fold = fold_rect(l);
        let hot = self.hover == Some(Hit::Header(t.id, HeaderPart::Fold));
        let ink = if hot { th.ui.text } else { th.ui.text_dim };
        // A triangle: right when closed, down when open.
        let (cx, cy) = (fold.center().x, fold.center().y);
        let tri = if open {
            [
                Point::new(cx - 5.0, cy - 2.5),
                Point::new(cx + 5.0, cy - 2.5),
                Point::new(cx, cy + 3.5),
            ]
        } else {
            [
                Point::new(cx - 2.5, cy - 5.0),
                Point::new(cx - 2.5, cy + 5.0),
                Point::new(cx + 3.5, cy),
            ]
        };
        let mut path = faderframe_ui_canvas::Path::new();
        path.move_to(tri[0]);
        path.line_to(tri[1]);
        path.line_to(tri[2]);
        path.close();
        p.fill_path(&path, ink);
        let name = folder_name_rect(l);
        if t.is_folder_bus() {
            // A folder bus: a badge after its mute and solo (what it holds
            // plays through its strip).
            let badge = folder_bus_badge(l);
            p.fill_rounded(badge, 3.0, &Paint::Solid(c.fader_cap_bus.with_alpha(0.85)));
            p.text(
                "BUS",
                badge,
                &TextStyle::new(th.fonts.tiny, th.ui.text)
                    .family(FontFamily::Condensed)
                    .bold()
                    .center(),
            );
        }
        let name = self.paint_time_lock(p, t, name);
        p.text(
            &t.name,
            name,
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        if let Some(info) = l.info {
            let project = model.project();
            let n = model
                .folder_contents(t.id)
                .iter()
                .filter(|id| project.track(**id).is_some_and(|t| !t.is_folder()))
                .count();
            // A folder bus says so: what it holds plays through it.
            let what = if t.is_folder_bus() {
                "Folder Bus"
            } else {
                "Folder"
            };
            let text = format!("{what} · {n} track{}", if n == 1 { "" } else { "s" });
            p.text(
                &text,
                Rect::new(folder_name_rect(l).x, info.y, info.w - 16.0, info.h),
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim).family(FontFamily::Condensed),
            );
        }
        controls::led_button(p, l.mute, "M", model.shown_mute(t), c.led.mute, th);
        controls::led_button(p, l.solo, "S", t.solo, c.led.solo, th);
        // Its Group switch, lit in the folder's colour.
        controls::led_button(
            p,
            folder_group_rect(l),
            "G",
            t.folder_group,
            color_of(t.color).lighten(0.15),
            th,
        );
    }

    /// What a folder holds, at a glance: each track's clips as a thin bar.
    fn paint_folder_lane(
        &self,
        p: &mut dyn Painter,
        row: Rect,
        lanes: Rect,
        t: &Track,
        model: &Session,
    ) {
        let project = model.project();
        let inside: Vec<&Track> = project
            .folder_order()
            .into_iter()
            .filter(|c| c.kind.has_clips() && project.in_folder(c, t.id))
            .collect();
        if inside.is_empty() {
            return;
        }
        let area = Rect::new(
            lanes.x,
            row.y + 5.0,
            lanes.w,
            self.base_h(model, t.id) - 10.0,
        );
        let band = (area.h / inside.len() as f32).clamp(2.0, 8.0);
        for (j, c) in inside.iter().enumerate() {
            let y = area.y + j as f32 * band;
            if y + band > area.bottom() + 0.5 {
                break;
            }
            for clip in project.clips_of(c.id) {
                let r = self.clip_rect(clip, row, model);
                if r.right() < lanes.x || r.x > lanes.right() {
                    continue;
                }
                let bar = Rect::new(r.x, y, r.w.max(2.0), (band - 1.0).max(1.0));
                p.fill_rounded(
                    bar,
                    1.0,
                    &Paint::Solid(color_of(clip.color.unwrap_or(c.color)).with_alpha(0.85)),
                );
            }
        }
    }

    fn paint_header(&self, p: &mut dyn Painter, l: &HeaderLayout, t: &Track, model: &Session) {
        let th = &self.theme;
        let a = &th.arranger;
        let c = &th.console;
        let selected = model.selection.tracks.contains(&t.id);
        let bg = if selected {
            a.header_bg_selected
        } else {
            a.header_bg
        };
        p.fill_rect(
            l.row,
            &Paint::vertical(l.row, bg.lighten(0.035), bg.darken(0.06)),
        );
        p.fill(l.stripe, color_of(t.color));
        p.hline(
            l.row.x,
            l.row.right(),
            l.row.bottom() - 1.0,
            a.header_border,
        );
        p.hline(
            l.row.x,
            l.row.right(),
            l.row.y,
            self.theme.ui.text.with_alpha(0.05),
        );

        if t.is_folder() {
            self.paint_folder_header(p, l, t, model);
            return;
        }
        let name = self.paint_time_lock(p, t, l.name);
        p.text(
            &t.name,
            name,
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        if let Some(info) = l.info {
            let out = match t.output {
                // A MIDI track plays an instrument track or nothing.
                OutputRouting::Master | OutputRouting::None | OutputRouting::Hardware { .. }
                    if t.kind == TrackKind::Midi =>
                {
                    " → no instrument".into()
                }
                OutputRouting::Master => String::new(),
                OutputRouting::Track { track } => model
                    .project()
                    .track(track)
                    .map(|d| format!(" → {}", d.name))
                    .unwrap_or_default(),
                OutputRouting::Hardware { first_channel } => {
                    format!(" → Out {}", first_channel + 1)
                }
                OutputRouting::None => " → none".into(),
            };
            let plug = model
                .instrument_slot(t)
                .map(|s| {
                    let mark = if model.plugin_failed(s.id) {
                        "⚠ "
                    } else {
                        ""
                    };
                    format!(
                        " · {mark}{}",
                        s.plugin.name.trim_start_matches("FaderFrame ")
                    )
                })
                .unwrap_or_default();
            let input = if t.kind == TrackKind::Audio {
                format!(" · {}", model.input_label(t))
            } else {
                String::new()
            };
            // Group and VCA membership.
            let group = t
                .group
                .and_then(|g| model.project().group(g))
                .map(|g| format!(" · {}", g.name))
                .unwrap_or_default();
            let vca = t
                .vca
                .and_then(|v| model.project().track(v))
                .map(|v| format!(" · {}", v.name))
                .unwrap_or_default();
            // Left out of ripples (edits elsewhere never move it): first,
            // so a narrow header still shows it.
            let held = if t.ripple_excluded {
                "No ripple · "
            } else {
                ""
            };
            let text = if t.kind == TrackKind::Vca {
                let n = model.project().vca_members(t.id).len();
                format!(
                    "{held}VCA · {n} track{}{group}{vca}",
                    if n == 1 { "" } else { "s" }
                )
            } else if t.kind == TrackKind::Midi {
                // Notes only: no channel layout.
                format!("{held}{}{plug}{out}{group}{vca}", t.kind.label())
            } else {
                format!(
                    "{held}{} · {}{input}{plug}{out}{group}{vca}",
                    t.kind.label(),
                    t.layout.short_name()
                )
            };
            p.text(
                &text,
                info,
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim).family(FontFamily::Condensed),
            );
        }
        controls::led_button(p, l.mute, "M", model.shown_mute(t), c.led.mute, th);
        controls::led_button(p, l.solo, "S", t.solo, c.led.solo, th);
        let armable = t.kind.has_clips();
        controls::led_button(
            p,
            l.record,
            if armable { "R" } else { "·" },
            t.record_arm,
            c.led.record,
            th,
        );
        let mon = t.kind == TrackKind::Audio;
        controls::led_button(
            p,
            l.monitor,
            if mon { "I" } else { "·" },
            mon && t.monitor != MonitorMode::Off,
            c.led.monitor,
            th,
        );
        let automated = t
            .automation
            .lanes
            .iter()
            .any(|a| !a.curve.is_empty() && a.mode != faderframe_session::AutomationMode::Off);
        controls::led_button(
            p,
            l.automation,
            "A",
            !model.shown_lanes(t.id).is_empty() || automated,
            th.arranger.automation,
            th,
        );
        if let Some(pan) = l.pan
            && t.kind.has_audio()
        {
            // Dual pan: the image's middle, a dot at each channel.
            let dual = model
                .project()
                .dual_panned(t)
                .then(|| model.shown_dual_pan(t));
            let value = dual.map_or(model.shown_pan(t), |d| (d.left + d.right) * 0.5);
            controls::knob(
                p,
                pan,
                (value + 1.0) * 0.5,
                true,
                KnobLook {
                    cap: c.pan_cap,
                    ring: c.panel_label,
                },
                th,
            );
            if let Some(d) = dual {
                let (o, r) = (pan.center(), pan.w.min(pan.h) * 0.5 - 1.6);
                for side in [d.left, d.right] {
                    let a = controls::knob_angle((side + 1.0) * 0.5);
                    let at = Point::new(o.x + r * a.cos(), o.y + r * a.sin());
                    p.circle(at, 2.6, th.ui.background);
                    p.circle(at, 1.7, th.ui.text);
                }
            }
        }
        if t.kind.has_audio() || t.kind == TrackKind::Vca {
            // Mini horizontal fader.
            let v = l.volume;
            let pos = self.law.db_to_position(model.shown_volume_db(t));
            let slot = Rect::new(v.x, v.center().y - 1.5, v.w, 3.0);
            p.fill_rounded(slot, 1.5, &Paint::Solid(c.fader.slot));
            p.fill_rounded(
                Rect::new(slot.x, slot.y, slot.w * pos, slot.h),
                1.5,
                &Paint::Solid(color_of(t.color).with_alpha(0.75)),
            );
            let ux = slot.x + slot.w * self.law.unity_position();
            if c.look.fader_kind == FaderKind::Pill {
                // A round thumb with a dot of the track's colour.
                p.vline(ux, v.y + 1.0, v.bottom() - 1.0, th.ui.text.with_alpha(0.22));
                let d = (v.h + 2.0).min(11.0);
                let cap = Rect::new(
                    slot.x + slot.w * pos - d * 0.5,
                    v.center().y - d * 0.5,
                    d,
                    d,
                );
                p.shadow(cap, d * 0.5, Color::rgba(0.0, 0.0, 0.0, 0.5), 0.0, 1.0, 3.0);
                p.fill_rounded(
                    cap,
                    d * 0.5,
                    &Paint::vertical(cap, c.fader.cap_top, c.fader.cap_bottom),
                );
                let px = 1.0 / p.scale_factor().max(1.0);
                p.stroke_rounded(
                    cap.inset(px * 0.5),
                    d * 0.5,
                    px,
                    th.ui.text.with_alpha(0.16),
                );
                p.circle(cap.center(), 1.8, color_of(t.color));
            } else {
                p.vline(ux, v.y, v.bottom(), Color::rgba(1.0, 1.0, 1.0, 0.25));
                let cap = Rect::new(slot.x + slot.w * pos - 4.0, v.y - 1.0, 8.0, v.h + 2.0);
                p.shadow(cap, 2.0, Color::rgba(0.0, 0.0, 0.0, 0.6), 0.0, 1.0, 2.0);
                p.fill_rounded(
                    cap,
                    2.0,
                    &Paint::vertical(cap, c.fader.cap_top, c.fader.cap_bottom),
                );
            }
            p.text(
                &format_db(model.shown_volume_db(t)),
                l.volume_text,
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim)
                    .family(th.fonts.numerals)
                    .right(),
            );
            let m = model.meter(t.id);
            let lv = |ch: &faderframe_session::MeterChannel| {
                MeterLevel::new(ch.level_db, ch.hold_db, ch.clipped)
            };
            controls::meter(p, l.meter, &[lv(&m.left), lv(&m.right)], th);
        }
    }

    fn paint_ruler(&self, p: &mut dyn Painter, ruler: Rect, model: &Session) {
        let th = &self.theme;
        let a = &th.arranger;
        let project = model.project();
        p.fill_rect(
            ruler,
            &Paint::vertical(ruler, a.ruler_bg.lighten(0.04), a.ruler_bg),
        );
        p.push_clip(ruler);
        let band = Rect::new(ruler.x, ruler.y, ruler.w, LOOP_BAND);
        p.fill(band, Color::rgba(0.0, 0.0, 0.0, 0.18));
        if let Some(lr) = project.loop_range {
            let x0 = self.x_of(lr.start);
            let x1 = self.x_of(lr.end);
            let r = Rect::new(x0, band.y + 1.0, x1 - x0, band.h - 2.0);
            let col = if project.loop_enabled {
                a.loop_on.with_alpha(0.75)
            } else {
                a.loop_off
            };
            p.fill_rounded(r, 2.0, &Paint::Solid(col));
            let handle = a.ruler_text.with_alpha(0.8);
            p.fill(Rect::new(x0, band.y + 2.0, 2.0, band.h - 4.0), handle);
            p.fill(Rect::new(x1 - 2.0, band.y + 2.0, 2.0, band.h - 4.0), handle);
        }
        if let Some(pr) = project.punch_range {
            let x0 = self.x_of(pr.start);
            let x1 = self.x_of(pr.end);
            let r = Rect::new(x0, band.bottom(), x1 - x0, 3.0);
            let col = if project.punch_enabled {
                a.record.with_alpha(0.85)
            } else {
                a.record.with_alpha(0.3)
            };
            p.fill(r, col);
        }
        let meter = &project.timeline.meter;
        let every = self.ruler_every(model);
        let start = self.time_at(ruler.x).max(MusicalTime::ZERO);
        let end = self.time_at(ruler.right());
        let label = TextStyle::new(th.fonts.small, a.ruler_text).family(th.fonts.numerals);
        let lines = self.grid_lines(start, end, model);
        // Zoomed in, beats and finer lines get labels too.
        let labelled = ruler_labels(&lines, |t| self.x_of(t), every);
        let fine_label =
            TextStyle::new(th.fonts.tiny, a.ruler_text.with_alpha(0.8)).family(th.fonts.numerals);
        for &(t, kind) in &lines {
            let x = self.x_of(t);
            if labelled.contains(&t) {
                p.text(
                    &ruler_label(meter, t, kind),
                    Rect::new(x + 3.0, band.bottom(), 90.0, ruler.h - band.h - 2.0),
                    &fine_label,
                );
            }
            match kind {
                GridLineKind::Bar => {
                    let bar = meter.bar_at(t);
                    if bar % every == 0 {
                        p.vline(
                            x,
                            band.bottom(),
                            ruler.bottom(),
                            a.ruler_text.with_alpha(0.45),
                        );
                        p.text(
                            &format!("{}", bar + 1),
                            Rect::new(x + 4.0, band.bottom(), 60.0, ruler.h - band.h - 2.0),
                            &label,
                        );
                    } else {
                        p.vline(
                            x,
                            ruler.bottom() - 6.0,
                            ruler.bottom(),
                            a.ruler_text.with_alpha(0.3),
                        );
                    }
                }
                GridLineKind::Beat => p.vline(
                    x,
                    ruler.bottom() - 5.0,
                    ruler.bottom(),
                    a.ruler_text.with_alpha(0.25),
                ),
                GridLineKind::Subdivision => p.vline(
                    x,
                    ruler.bottom() - 3.0,
                    ruler.bottom(),
                    a.ruler_text.with_alpha(0.15),
                ),
                GridLineKind::Fine => p.vline(
                    x,
                    ruler.bottom() - 2.0,
                    ruler.bottom(),
                    a.ruler_text.with_alpha(0.1),
                ),
            }
        }
        self.paint_play_start(p, band, model);
        // Playhead marker.
        let x = self.x_of(model.playhead());
        let mut tri = Path::new();
        tri.move_to(Point::new(x - 6.0, band.bottom()))
            .line_to(Point::new(x + 6.0, band.bottom()))
            .line_to(Point::new(x, band.bottom() + 8.0))
            .close();
        p.fill_path(&tri, a.playhead);
        // Counting in before playback (the playhead held at the start): a
        // badge by it with the beat counted, as the transport display's
        // counter has it (bar 0 is the bar before bar 1).
        if let Some(at) = model.count_in_position() {
            let bbt = project
                .timeline
                .meter
                .to_bbt(model.engine().samples_to_musical(project, at));
            let label = format!("Count-in {}.{}", bbt.bar + 1, bbt.beat + 1);
            let style = TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).bold();
            let w = p.text_width(&label, &style) + 10.0;
            let h = (ruler.bottom() - band.bottom() - 4.0).clamp(8.0, 15.0);
            let r = Rect::new(x + 8.0, band.bottom() + 2.0, w, h);
            p.fill_rounded(r, 3.0, &Paint::Solid(a.record.with_alpha(0.9)));
            p.text(&label, r, &style.center());
        }
        p.pop_clip();
        p.hline(
            ruler.x,
            ruler.right(),
            ruler.bottom() - 1.0,
            a.header_border,
        );
    }

    /// The Play Start Marker: a flag in the theme's "playing" green over
    /// the loop band, hollow while playback does not start from it.
    fn paint_play_start(&self, p: &mut dyn Painter, band: Rect, model: &Session) {
        let Some(m) = model.project().play_start else {
            return;
        };
        let x = self.x_of(m.at);
        let col = self.theme.arranger.launch_playing;
        let mut flag = Path::new();
        flag.move_to(Point::new(x, band.y))
            .line_to(Point::new(x + 9.0, band.y + band.h * 0.5))
            .line_to(Point::new(x, band.bottom()))
            .close();
        if m.on {
            p.fill_path(&flag, col);
        } else {
            p.stroke_path(&flag, 1.0, col);
        }
        p.vline(x, band.y, band.bottom() + 6.0, col.with_alpha(0.9));
    }

    fn paint_corner(&self, p: &mut dyn Painter, corner: Rect, model: &Session) {
        let th = &self.theme;
        p.fill_rect(
            corner,
            &Paint::vertical(
                corner,
                th.arranger.ruler_bg.lighten(0.06),
                th.arranger.ruler_bg,
            ),
        );
        let ed = &model.editor;
        let on = |b: bool| if b { "ON" } else { "OFF" };
        let grid = ed.grid.label().to_uppercase();
        let (snap, follow) = (on(ed.snap), on(ed.follow_playhead));
        // After the arrangement switcher; shorter when a pad's long name
        // leaves less room.
        let from = self.arrangement_rect(model).right() + 8.0;
        let text_rect = Rect::new(from, corner.y, (corner.right() - from).max(0.0), corner.h);
        // Rippling, where it reaches comes first.
        let ripple = if model.ripples() {
            format!("RIPPLE {}  ·  ", ed.ripple.scope.short().to_uppercase())
        } else {
            String::new()
        };
        let mut text = format!("{ripple}GRID {grid}  ·  SNAP {snap}  ·  FOLLOW {follow}");
        let engraved = TextStyle::new(th.fonts.tiny, th.console.panel_label)
            .family(FontFamily::Condensed)
            .weight(faderframe_ui_canvas::FontWeight::Bold)
            .tracking(0.6);
        if p.text_width(&text, &engraved) > text_rect.w {
            text = format!(
                "{}{grid} · SNAP {snap} · FOLLOW {follow}",
                ripple.replace("  ·  ", " · ")
            );
        }
        p.push_clip(text_rect);
        controls::engraved(p, &text, text_rect, th, Align::Start);
        p.pop_clip();
        p.hline(
            corner.x,
            corner.right(),
            corner.bottom() - 1.0,
            th.arranger.header_border,
        );
    }

    fn follow(&mut self, model: &Session, size: Size) {
        // The playhead jumped (Home, a marker, a locate): back to the start
        // when it went there, else into view when it is out of it (with
        // Follow Playhead).
        let jumps = model.transport().jumps;
        if self.seen_jumps.replace(jumps).is_some_and(|j| j != jumps) && !self.bar_held {
            let at = model.playhead();
            let x = self.x_of(at);
            let lanes_w = size.w - self.header_w();
            if at <= MusicalTime::ZERO {
                self.scroll_x = 0.0;
                return;
            }
            if model.editor.follow_playhead && (x > size.w - 30.0 || x < self.header_w()) {
                self.scroll_x = (at.quarters() * self.ppq as f64 - lanes_w as f64 * 0.1).max(0.0);
                return;
            }
        }
        if !(model.editor.follow_playhead && model.transport().playing) || self.bar_held {
            return;
        }
        let x = self.x_of(model.playhead());
        let lanes_w = size.w - self.header_w();
        if x > size.w - 30.0 || x < self.header_w() {
            self.scroll_x =
                (model.playhead().quarters() * self.ppq as f64 - lanes_w as f64 * 0.1).max(0.0);
        }
    }

    /// The take being recorded: a growing red region on every armed track.
    fn paint_recording(
        &self,
        p: &mut dyn Painter,
        lanes: Rect,
        size: Size,
        tracks: &[&Track],
        model: &Session,
    ) {
        let Some(rec) = model.recording_view() else {
            return;
        };
        let t = model.transport();
        if !t.recording || !t.playing || t.position <= rec.from {
            return;
        }
        let pos = t.position.min(rec.to);
        let mut start = rec.from;
        if let Some(lr) = model
            .project()
            .loop_range
            .filter(|_| model.project().loop_enabled)
        {
            let ls = model.engine().musical_to_samples(model.project(), lr.start);
            if pos >= ls && start < ls && t.looping {
                start = start.max(ls);
            }
        }
        let x0 = self.x_of_sample(model, start).max(lanes.x);
        let x1 = self.x_of_sample(model, pos).min(lanes.right());
        if x1 <= x0 {
            return;
        }
        let red = self.theme.arranger.record;
        for (i, track) in tracks.iter().enumerate() {
            if !rec.tracks.contains(&track.id) {
                continue;
            }
            let row = self.row_rect(i, size);
            if row.bottom() < lanes.y || row.y > lanes.bottom() {
                continue;
            }
            let r = Rect::new(x0, row.y + 3.0, x1 - x0, self.base_h(model, track.id) - 6.0);
            p.fill_rounded(r, 3.0, &Paint::Solid(red.with_alpha(0.22)));
            // The waveform of what has been written so far (current pass),
            // drawn where the take will land (latency compensated).
            let header = 15.0f32.min(r.h * 0.4);
            let wave_area = Rect::new(r.x, r.y + header, r.w, r.h - header).inset_xy(0.0, 2.0);
            p.push_clip(r);
            model.with_live_take(track.id, |live| {
                let Some(seg) = live.segments.last() else {
                    return;
                };
                let start = seg.timeline_start - rec.latency;
                let offset = seg.file_offset as i64 + rec.latency;
                let len = seg.frames as i64 - rec.latency;
                let channels = live.peaks.channels();
                self.fill_wave(
                    p,
                    wave_area,
                    (lanes.x, lanes.right()),
                    start,
                    len,
                    channels,
                    1.0,
                    model,
                    red.lighten(0.1),
                    |ch, a, b| live.peaks.min_max(ch, offset + a, offset + b),
                );
            });
            // MIDI: the notes played so far, where they will land; held
            // notes grow with the playhead.
            let notes = model.live_midi_notes(track.id);
            if !notes.is_empty() {
                let lo = notes.iter().map(|n| n.key).min().unwrap_or(60).min(60 - 6);
                let hi = notes.iter().map(|n| n.key).max().unwrap_or(60).max(lo + 12);
                let span = (hi - lo + 1) as f32;
                let note_h = (wave_area.h / span).clamp(1.5, 8.0);
                for n in &notes {
                    let nx0 = self.x_of_sample(model, n.start);
                    let nx1 = self.x_of_sample(model, n.end.unwrap_or(pos));
                    let y = wave_area.bottom() - (n.key - lo) as f32 / span * wave_area.h - note_h;
                    let alpha = 0.55 + 0.45 * (n.velocity as f32 / 127.0);
                    p.fill_rounded(
                        Rect::new(nx0, y, (nx1 - nx0).max(2.0), note_h.max(2.0)),
                        1.0,
                        &Paint::Solid(red.lighten(0.25).with_alpha(alpha)),
                    );
                }
            }
            p.pop_clip();
            p.stroke_rounded(r, 3.0, 1.2, red.with_alpha(0.9));
            if r.w > 40.0 {
                p.text(
                    "● REC",
                    Rect::new(r.x + 6.0, r.y, 80.0, 16.0),
                    &TextStyle::new(self.theme.fonts.small, red.lighten(0.3)).bold(),
                );
            }
        }
    }

    fn paint_drop_hint(
        &self,
        p: &mut dyn Painter,
        lanes: Rect,
        size: Size,
        tracks: &[&Track],
        target: Option<TrackId>,
        at: MusicalTime,
    ) {
        let accent = self.theme.arranger.selection_outline;
        let row = match target.and_then(|t| tracks.iter().position(|x| x.id == t)) {
            Some(i) => self.row_rect(i, size),
            // New tracks appear below the last one.
            None => self.row_rect(tracks.len(), size),
        };
        let band = Rect::new(lanes.x, row.y, lanes.w, row.h);
        p.fill(band, accent.with_alpha(0.12));
        p.stroke_rounded(band.inset_xy(1.0, 1.0), 3.0, 1.0, accent.with_alpha(0.6));
        let x = self.x_of(at);
        p.fill(Rect::new(x - 1.0, row.y + 2.0, 2.0, row.h - 4.0), accent);
        let label = if target.is_some() {
            "Drop to import here"
        } else {
            "Drop to import on new tracks"
        };
        p.text(
            label,
            Rect::new(x + 8.0, row.y, 260.0, row.h),
            &TextStyle::new(self.theme.fonts.small, accent).bold(),
        );
    }

    /// Where files dropped at `pos` go: an audio track under the pointer,
    /// otherwise new tracks; time snapped to the grid.
    fn drop_target(
        &self,
        pos: Point,
        size: Size,
        model: &Session,
    ) -> Option<(Option<TrackId>, MusicalTime)> {
        if pos.x < self.header_w() || pos.y < self.ruler_h() || pos.x > size.w {
            return None;
        }
        let at = self.snap(
            self.time_at(pos.x).max(MusicalTime::ZERO),
            model,
            Modifiers::default(),
        );
        let tracks = Self::lane_tracks(model);
        let target = self
            .row_at(pos.y)
            .and_then(|i| tracks.get(i))
            .filter(|t| t.kind == TrackKind::Audio)
            .map(|t| t.id);
        Some((target, at))
    }

    /// Where clips dropped at `pos` go: the track under it, the snapped
    /// time.
    fn clip_drop_target(
        &self,
        pos: Point,
        size: Size,
        model: &Session,
    ) -> Option<(TrackId, MusicalTime)> {
        let (_, at) = self.drop_target(pos, size, model)?;
        let tracks = Self::lane_tracks(model);
        let track = self.row_at(pos.y).and_then(|i| tracks.get(i))?.id;
        Some((track, at))
    }

    // --- interaction ----------------------------------------------------------------

    /// To the grid (unless Alt or snapping is off), else to a whole sample
    /// (with Snap to Samples).
    fn snap(&self, t: MusicalTime, model: &Session, mods: Modifiers) -> MusicalTime {
        if mods.alt || !model.editor.snap {
            model.on_sample(t)
        } else {
            model.editor.snap(t, &model.project().timeline.meter)
        }
    }

    /// Snap a folder-relative frame of `clip` to the grid (unless Alt).
    fn snap_frame(&self, frame: i64, clip: ClipId, model: &Session, mods: Modifiers) -> i64 {
        let p = model.project();
        let Some(c) = p.clip(clip) else { return frame };
        let rate = p.sample_rate as f64;
        let base = p.timeline.to_samples(c.start, rate);
        let t = p.timeline.to_musical(base + frame, rate);
        p.timeline.to_samples(self.snap(t, model, mods), rate) - base
    }

    fn header_layout(&self, model: &Session, track: TrackId, size: Size) -> Option<HeaderLayout> {
        let i = Self::lane_tracks(model)
            .iter()
            .position(|t| t.id == track)?;
        let row = self.row_rect(i, size);
        let t = model.project().track(track)?;
        Some(HeaderLayout::new(self.header_rect(model, t, row.y)))
    }

    fn track_menu(model: &Session, t: &Track, at: Point) -> HostRequest<Action> {
        let mut items = vec![
            MenuItem::new("Add Audio Track (Mono)", Action::AddTrack(TrackKind::Audio)),
            MenuItem::new(
                "Add Audio Track (Stereo)",
                Action::AddTrackWithLayout(
                    TrackKind::Audio,
                    faderframe_core::ChannelLayout::Stereo,
                ),
            ),
            MenuItem::new(
                "Add Instrument Track",
                Action::AddTrack(TrackKind::Instrument),
            ),
            MenuItem::new("Add Bus", Action::AddTrack(TrackKind::Bus)),
            MenuItem::new("Add Aux (FX Return)", Action::AddTrack(TrackKind::Aux)),
            MenuItem::new("Add VCA", Action::AddTrack(TrackKind::Vca)),
        ];
        items.extend(model.duplicate_menu(t.id).into_iter().map(entry_item));
        items.push(MenuItem::new(
            "Remove Track",
            Action::Edit(Command::RemoveTrack { track: t.id }),
        ));
        items.push(
            MenuItem::new(
                "Colour…",
                Action::PickColor(faderframe_session::ColorTarget::Track(t.id)),
            )
            .separated(),
        );
        // Groups and VCAs.
        items.extend(model.group_menu(t.id).into_iter().map(entry_item));
        // A sample of the selection (audio tracks).
        items.extend(sample_items(model, t.id, None));
        // Folders: what this one holds; into and out of folders.
        if t.is_folder() {
            let open = model.folder_open(t.id);
            items.push(
                MenuItem::new(
                    if open { "Close Folder" } else { "Open Folder" },
                    Action::ToggleFolder(t.id),
                )
                .separated(),
            );
            items.push(MenuItem::new(
                "Select Its Tracks",
                Action::SelectTracks {
                    tracks: model.folder_contents(t.id),
                    mode: SelectMode::Replace,
                },
            ));
            // The folder itself sums what it holds (or no longer does).
            items.push(
                MenuItem::new(
                    "Folder Is a Bus",
                    Action::SetFolderBus {
                        folder: t.id,
                        on: !t.is_folder_bus(),
                    },
                )
                .checked(t.is_folder_bus()),
            );
            // What it holds is edited and armed as one (or no longer is).
            items.push(
                MenuItem::new(
                    "Group Its Tracks",
                    Action::SetFolderGroup {
                        folder: t.id,
                        on: !t.folder_group,
                    },
                )
                .checked(t.folder_group),
            );
        }
        items.extend(
            model
                .folder_choices(t.id)
                .into_iter()
                .map(|(label, action, separated)| {
                    let item = MenuItem::new(label, action);
                    if separated { item.separated() } else { item }
                }),
        );
        // Ripples elsewhere leave it where it is (a music bed, a
        // reference); its own edits still ripple.
        if !t.is_folder() {
            items.push(
                MenuItem::new(
                    "Leave Out of Ripples",
                    Action::Ripple(faderframe_session::RippleOp::Exclude {
                        tracks: vec![t.id],
                        excluded: !t.ripple_excluded,
                    }),
                )
                .checked(t.ripple_excluded)
                .separated(),
            );
        }
        // Placed with the music or in time (tempo changes leave a
        // time-locked track's material where it sounds).
        let bases: Vec<MenuItem<Action>> = model
            .time_base_choices(t.id)
            .into_iter()
            .map(|c| MenuItem::new(c.label, c.action).checked(c.checked))
            .collect();
        if !bases.is_empty() {
            items.push(MenuItem::submenu("Time Base", bases));
        }
        // Freezing and bouncing (tracks with clips).
        if matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) {
            let busy = model.bouncing().contains(&t.id);
            let frozen = t.freeze.is_some();
            items.push(
                if busy {
                    MenuItem::disabled("Rendering…")
                } else if frozen {
                    MenuItem::new("Unfreeze Track", Action::UnfreezeTrack(t.id))
                } else {
                    MenuItem::new("Freeze Track", Action::FreezeTrack(t.id))
                }
                .separated(),
            );
            if !frozen && !busy {
                items.push(MenuItem::new(
                    "Bounce to New Track",
                    Action::BounceTrack(t.id),
                ));
            }
        }
        // The microphones of one source in time and polarity; the
        // track's own delay back to none.
        items.extend(model.auto_align_menu(t.id).into_iter().map(entry_item));
        // Instrument (instrument tracks): the plugin browser.
        if t.kind == TrackKind::Instrument {
            let current = model
                .instrument_slot(t)
                .map_or_else(|| "none".to_string(), |s| s.plugin.name.clone());
            items.push(
                MenuItem::new(
                    format!("Choose Instrument… (now: {current})"),
                    Action::OpenPluginBrowser {
                        track: t.id,
                        target: faderframe_session::PluginTarget::Instrument,
                    },
                )
                .separated(),
            );
            if let Some(slot) = model.instrument_slot(t) {
                items.push(MenuItem::new(
                    format!("Show {} Editor", t.name_of(slot)),
                    Action::OpenPluginEditor {
                        track: t.id,
                        plugin: slot.id,
                        generic: false,
                    },
                ));
                if slot.plugin.format != faderframe_project::PluginFormat::Builtin {
                    items.push(MenuItem::new(
                        if model.plugin_failed(slot.id) {
                            format!("Reload {} (it stopped working)", t.name_of(slot))
                        } else {
                            format!("Reload {}", t.name_of(slot))
                        },
                        Action::ReloadPlugin(slot.id),
                    ));
                }
            }
        }
        // Editors of the inserts.
        for slot in &t.inserts {
            items.push(MenuItem::new(
                format!("Show {} Editor", t.name_of(slot)),
                Action::OpenPluginEditor {
                    track: t.id,
                    plugin: slot.id,
                    generic: false,
                },
            ));
        }
        if t.kind.has_audio() && t.kind != TrackKind::Master && t.kind != TrackKind::Midi {
            items.push(MenuItem::new(
                "Add Insert Plugin…",
                Action::OpenPluginBrowser {
                    track: t.id,
                    target: faderframe_session::PluginTarget::Insert(t.inserts.len()),
                },
            ));
        }
        // MIDI effects: on a MIDI track at the end of its chain, on an
        // instrument track right before the instrument.
        if matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
            let at = if t.kind == TrackKind::Midi {
                t.inserts.len()
            } else {
                model
                    .instrument_slot(t)
                    .and_then(|i| t.inserts.iter().position(|s| s.id == i.id))
                    .unwrap_or(0)
            };
            items.push(MenuItem::new(
                "Add MIDI Effect…",
                Action::OpenPluginBrowser {
                    track: t.id,
                    target: faderframe_session::PluginTarget::Insert(at),
                },
            ));
        }
        // MIDI input (instrument and MIDI tracks): which device and channel,
        // and when the track plays what the keyboard plays.
        if matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
            for (i, c) in model.midi_input_choices(t.id).into_iter().enumerate() {
                let item =
                    MenuItem::new(format!("MIDI In: {}", c.label), c.action).checked(c.checked);
                items.push(if i == 0 || c.group_start {
                    item.separated()
                } else {
                    item
                });
            }
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
        }
        // The instrument a MIDI track plays.
        for (i, c) in model.midi_instrument_choices(t.id).into_iter().enumerate() {
            let item = MenuItem::new(format!("Plays: {}", c.label), c.action).checked(c.checked);
            items.push(if i == 0 || c.group_start {
                item.separated()
            } else {
                item
            });
        }
        // External MIDI device (MIDI tracks).
        if t.kind == TrackKind::Midi {
            for (i, c) in model.midi_output_choices(t.id).into_iter().enumerate() {
                let item =
                    MenuItem::new(format!("MIDI Out: {}", c.label), c.action).checked(c.checked);
                items.push(if i == 0 || c.group_start {
                    item.separated()
                } else {
                    item
                });
            }
        }
        // MPE: per-note expression on member channels.
        if matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
            let on = t.mpe.is_some();
            items.push(
                MenuItem::new(
                    "MPE (per-note expression)",
                    Action::Edit(Command::SetTrackMpe {
                        track: t.id,
                        mpe: (!on).then(faderframe_project::MpeConfig::default),
                    }),
                )
                .checked(on)
                .separated(),
            );
        }
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
        let modes: Vec<MenuItem<Action>> = model
            .pan_mode_choices(t.id)
            .into_iter()
            .map(|c| MenuItem::new(c.label, c.action).checked(c.checked))
            .collect();
        if !modes.is_empty() {
            items.push(MenuItem::submenu("Pan Mode", modes));
        }
        if let Some(c) = model.object_choice(t.id) {
            items.push(MenuItem::new(c.label, c.action).checked(c.checked));
        }
        // MIDI learn for the strip controls.
        if t.kind != TrackKind::Midi {
            use faderframe_automation::AutomationTarget as A;
            use faderframe_project::MappingTarget as M;
            let param = |target| M::Parameter {
                track: t.id,
                target,
            };
            let mut targets = vec![(param(A::TrackVolume), "Volume")];
            if model.project().dual_panned(t) {
                targets.push((param(A::DualPanLeft), "Left Pan"));
                targets.push((param(A::DualPanRight), "Right Pan"));
            } else if model.project().pans_stereo(t) {
                targets.push((param(A::TrackPan), "Balance"));
            } else {
                targets.push((param(A::TrackPan), "Pan"));
            }
            targets.push((param(A::TrackMute), "Mute"));
            targets.push((M::TrackSolo { track: t.id }, "Solo"));
            if matches!(
                t.kind,
                TrackKind::Audio | TrackKind::Instrument | TrackKind::Midi
            ) {
                targets.push((M::TrackArm { track: t.id }, "Record Arm"));
            }
            for (i, (target, name)) in targets.into_iter().enumerate() {
                let mapped = model
                    .midi_mappings_for(target)
                    .first()
                    .map(|m| format!(" (now {})", m.source.label()))
                    .unwrap_or_default();
                let item = MenuItem::new(
                    format!("MIDI Learn {name}…{mapped}"),
                    Action::MidiLearn(target),
                );
                items.push(if i == 0 { item.separated() } else { item });
                for m in model.midi_mappings_for(target) {
                    items.push(MenuItem::new(
                        format!("Remove MIDI Mapping from {name} ({})", m.source.label()),
                        Action::Edit(Command::RemoveMidiMapping { mapping: m.id }),
                    ));
                }
            }
        }
        // Input source (audio tracks): mono or stereo, which input(s).
        if t.kind == TrackKind::Audio {
            for (i, c) in model.input_choices(t.id).into_iter().enumerate() {
                let item =
                    MenuItem::new(format!("Input: {}", c.label), c.action).checked(c.checked);
                items.push(if i == 0 || c.group_start {
                    item.separated()
                } else {
                    item
                });
            }
        }
        // Multi-output plugins: tracks for their outputs (the main too).
        for slot in t.inserts.iter().chain(t.instrument.iter()) {
            if !model.plugin_has_extra_outputs(slot.id) {
                continue;
            }
            let missing = model
                .plugin_output_buses(slot.id)
                .iter()
                .filter(|o| o.track.is_none())
                .count();
            let label = format!("Create Output Tracks · {} ({missing})", t.name_of(slot));
            items.push(
                if missing == 0 {
                    MenuItem::disabled(label)
                } else {
                    MenuItem::new(
                        label,
                        Action::CreateOutputTracks {
                            plugin: slot.id,
                            buses: None,
                        },
                    )
                }
                .separated(),
            );
        }
        // A lead sheet of the whole track (its clips' melody).
        if matches!(
            t.kind,
            TrackKind::Audio | TrackKind::Midi | TrackKind::Instrument
        ) && !model.project().clips_of(t.id).is_empty()
        {
            items.push(
                MenuItem::new(
                    "Lead Sheet of This Track",
                    Action::MakeLeadSheet {
                        of: faderframe_session::leadsheet::LeadSheetOf::Track(t.id),
                        grid: faderframe_session::leadsheet::Grid::Auto,
                    },
                )
                .separated(),
            );
            items.push(MenuItem::new(
                "Guitar Tab of This Track",
                Action::MakeTab {
                    of: faderframe_session::leadsheet::LeadSheetOf::Track(t.id),
                },
            ));
            // A score of the selected tracks it is one of, a staff each.
            let score = score_tracks(model);
            if score.len() > 1 && score.contains(&t.id) {
                items.push(MenuItem::new(
                    format!(
                        "Score of the {} Selected Tracks (a Staff Each)",
                        score.len()
                    ),
                    Action::MakeLeadSheet {
                        of: faderframe_session::leadsheet::LeadSheetOf::Tracks(score.clone()),
                        grid: faderframe_session::leadsheet::Grid::Auto,
                    },
                ));
                items.push(MenuItem::new(
                    format!("Score with Guitar Tab ({} Tracks)", score.len()),
                    Action::MakeTab {
                        of: faderframe_session::leadsheet::LeadSheetOf::Tracks(score),
                    },
                ));
            }
        }
        // Track presets.
        if t.kind != TrackKind::Master {
            items.push(
                MenuItem::new(
                    "Save as Track Preset",
                    Action::PromptSaveTrackPreset { tracks: vec![t.id] },
                )
                .separated(),
            );
        }
        let presets = model.track_presets();
        for (i, preset) in presets.iter().take(12).enumerate() {
            let mut item = MenuItem::new(
                format!("New Track from “{}”", preset.name),
                Action::AddTrackFromPreset {
                    path: preset.path.clone(),
                },
            );
            if i == 0 {
                item = item.separated();
            }
            items.push(item);
        }
        for (i, preset) in presets.iter().take(12).enumerate() {
            let mut item = MenuItem::new(
                format!("Apply “{}” to {}", preset.name, t.name),
                Action::ApplyTrackPreset {
                    track: t.id,
                    path: preset.path.clone(),
                },
            );
            if i == 0 {
                item = item.separated();
            }
            items.push(item);
        }
        if !presets.is_empty() {
            items.push(MenuItem::submenu(
                "Delete Track Preset",
                presets
                    .iter()
                    .map(|p| {
                        MenuItem::new(
                            p.name.clone(),
                            Action::PromptDeletePreset {
                                path: p.path.clone(),
                            },
                        )
                    })
                    .collect(),
            ));
        }
        let current = model.track_height(t.id);
        for (i, (name, h)) in TRACK_HEIGHTS.iter().enumerate() {
            let mut item = MenuItem::new(
                format!("Height: {name}"),
                Action::SetTrackHeight {
                    track: Some(t.id),
                    height: *h,
                },
            )
            .checked(current.map_or(i == 1, |c| (c - h).abs() < 0.5));
            if i == 0 {
                item = item.separated();
            }
            items.push(item);
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

    fn grid_menu(model: &Session, at: Point) -> HostRequest<Action> {
        let ed = &model.editor;
        let mut items: Vec<MenuItem<Action>> = edit_bar::grid_menu(ed.grid)
            .into_iter()
            .map(|mut i| {
                if !matches!(i.label.as_str(), "Triplet" | "Dotted") {
                    i.label = format!("Grid {}", i.label);
                }
                i
            })
            .collect();
        items.push(
            MenuItem::new("Snap to Grid", Action::ToggleSnap)
                .checked(ed.snap)
                .separated(),
        );
        items.push(
            MenuItem::new("Follow Playhead", Action::ToggleFollowPlayhead)
                .checked(ed.follow_playhead),
        );
        HostRequest::ContextMenu { at, items }
    }

    /// The ruler's Play Start Marker items (`at`: where it was clicked,
    /// snapped).
    fn play_start_items(model: &Session, at: MusicalTime) -> Vec<MenuItem<Action>> {
        let marker = model.project().play_start;
        let mut items = vec![MenuItem::new(
            "Set Play Start Marker Here",
            Action::Transport(TransportAction::SetPlayStart(Some(at))),
        )];
        if let Some(m) = marker {
            items.push(
                MenuItem::new(
                    "Play from Play Start Marker",
                    Action::Transport(TransportAction::TogglePlayStart),
                )
                .checked(m.on),
            );
            items.push(MenuItem::new(
                "Remove Play Start Marker",
                Action::Transport(TransportAction::SetPlayStart(None)),
            ));
        }
        items
    }

    fn clip_menu(model: &Session, clip: &Clip, at: Point) -> HostRequest<Action> {
        // Edits apply to the whole selection when the clip is part of it.
        let targets = Self::targets(model, clip.id);
        let many = targets.len() > 1;
        let mut items = vec![];
        if clip.as_takes().is_some() {
            items.push(MenuItem::new(
                if model.takes_open(clip.id) {
                    "Hide Takes"
                } else {
                    "Show Takes"
                },
                Action::ToggleTakeLanes(clip.id),
            ));
            items.push(MenuItem::new("Flatten Comp", Action::FlattenTakes(clip.id)));
        }
        if clip.as_midi().is_some() {
            items.push(MenuItem::new(
                "Edit in Piano Roll",
                Action::OpenClipEditor(clip.id),
            ));
            items.push(MenuItem::new(
                "Edit in Event List",
                Action::OpenEventList(clip.id),
            ));
            // Every note and controller value of the clips onto a channel.
            let midi: Vec<ClipId> = targets
                .iter()
                .copied()
                .filter(|c| {
                    model
                        .project()
                        .clip(*c)
                        .is_some_and(|c| c.as_midi().is_some())
                })
                .collect();
            let now = model.midi_channel_of(&midi);
            items.push(MenuItem::submenu(
                "MIDI Channel",
                (0..16u8)
                    .map(|c| {
                        MenuItem::new(
                            format!("Channel {}", c + 1),
                            Action::SetMidiChannel {
                                clips: midi.clone(),
                                channel: c,
                            },
                        )
                        .checked(now == Some(c))
                    })
                    .collect(),
            ));
        }
        if let Some(a) = clip.as_audio() {
            items.push(MenuItem::new(
                if a.pitch.is_some() {
                    "Edit Pitch"
                } else {
                    "Edit Pitch (find its notes)"
                },
                Action::OpenPitchEditor(clip.id),
            ));
        }
        {
            // The selected clips when this is one of them.
            let clips = if model.selection.clips.contains(&clip.id) {
                let mut v: Vec<_> = model.selection.clips.iter().copied().collect();
                v.sort();
                v
            } else {
                vec![clip.id]
            };
            items.push(MenuItem::new(
                "Send to Clip Launcher",
                Action::Launcher(faderframe_session::launcher::LauncherOp::SendClips(
                    clips.clone(),
                )),
            ));
            items.extend(scratch::material_items(
                model,
                faderframe_session::scratch::PadMaterial::Clips(clips),
            ));
        }
        {
            use faderframe_session::detect::FromClip;
            let from = |what| Action::FromClip {
                clip: clip.id,
                what,
            };
            // MIDI and take folders: the lead sheet entries apart (an audio
            // clip's come with its other conversions below).
            if !matches!(clip.content, ClipContent::Audio(_)) {
                for (i, item) in lead_sheet_items(model, clip, &targets)
                    .into_iter()
                    .enumerate()
                {
                    items.push(if i == 0 { item.separated() } else { item });
                }
            }
            if clip.as_audio().is_some() {
                use faderframe_session::to_midi::ToMidi;
                for (i, how) in [ToMidi::Melody, ToMidi::Harmony, ToMidi::Drums]
                    .into_iter()
                    .enumerate()
                {
                    let item = MenuItem::new(
                        format!("Convert {} to MIDI", how.label()),
                        Action::ConvertToMidi { clip: clip.id, how },
                    );
                    items.push(if i == 0 { item.separated() } else { item });
                }
                items.push(MenuItem::new(
                    "Clip Effects…",
                    Action::OpenClipEffects(clip.id),
                ));
                items.push(MenuItem::new(
                    "Spectral Editor (Remove Noises, Heal)…",
                    Action::OpenSpectralEditor(clip.id),
                ));
                items.push(MenuItem::new(
                    "Transcribe Words to the Lyrics Lane",
                    Action::Transcribe(clip.id),
                ));
                items.extend(lead_sheet_items(model, clip, &targets));
                // The selected audio clips (a multitrack jam) or this one.
                let audio: Vec<ClipId> = targets
                    .iter()
                    .copied()
                    .filter(|c| {
                        model
                            .project()
                            .clip(*c)
                            .is_some_and(|c| c.as_audio().is_some())
                    })
                    .collect();
                items.push(
                    MenuItem::new(
                        if audio.len() > 1 {
                            "Find the Song's Structure (Sections) in These Recordings"
                        } else {
                            "Find the Song's Structure (Sections)"
                        },
                        Action::SongStructure { clips: audio },
                    )
                    .separated(),
                );
                items.push(MenuItem::new(
                    "Set Project Tempo from Clip",
                    from(FromClip::SetTempo),
                ));
                items.push(MenuItem::new(
                    "Warp Clip to Project Tempo",
                    from(FromClip::WarpToTempo),
                ));
            }
            if clip.as_audio().is_some() || clip.as_midi().is_some() {
                let item = MenuItem::new("Set Key from Clip", from(FromClip::SetKey));
                items.push(if clip.as_audio().is_some() {
                    item
                } else {
                    item.separated()
                });
            }
        }
        items.push(MenuItem::new(
            match (clip.muted, many) {
                (true, false) => "Unmute Clip",
                (true, true) => "Unmute Clips",
                (false, false) => "Mute Clip",
                (false, true) => "Mute Clips",
            },
            Action::SetClipsMuted {
                clips: targets.clone(),
                muted: !clip.muted,
            },
        ));
        items.push(MenuItem::new(
            "Split at Playhead",
            Action::SplitSelectedAtPlayhead,
        ));
        // Two or more clips of one track: one clip.
        let p = model.project();
        let mergeable = targets.iter().any(|a| {
            let track = p.clip(*a).map(|c| c.track);
            targets
                .iter()
                .filter(|b| p.clip(**b).map(|c| c.track) == track)
                .count()
                >= 2
        });
        if mergeable {
            items.push(MenuItem::new(
                "Merge Clips",
                Action::MergeClips(targets.clone()),
            ));
        }
        // Aliases: copies that share their content.
        items.push(
            MenuItem::new(
                if many {
                    "Duplicate as Aliases"
                } else {
                    "Duplicate as Alias"
                },
                Action::DuplicateAsAlias(targets.clone()),
            )
            .separated(),
        );
        if targets.iter().any(|c| model.is_alias(*c)) {
            items.push(MenuItem::new(
                "Make Unique (no longer an alias)",
                Action::MakeClipsUnique(targets.clone()),
            ));
        }
        Self::clip_edit_menu(model, clip, at, &mut items);
        items.extend(sample_items(model, clip.track, Some(clip.id)));
        items.push(
            MenuItem::new(
                if many { "Delete Clips" } else { "Delete" },
                // The clips, even with an edit range (Delete would clear
                // that).
                Action::Edit(Command::Batch {
                    label: if many { "Delete Clips" } else { "Delete Clip" }.into(),
                    commands: targets
                        .iter()
                        .map(|&clip| Command::RemoveClip { clip })
                        .collect(),
                }),
            )
            .separated(),
        );
        HostRequest::ContextMenu { at, items }
    }

    fn take_menu(model: &Session, clip: &Clip, take: usize, at: Point) -> HostRequest<Action> {
        let Some(f) = clip.as_takes() else {
            return HostRequest::ContextMenu {
                at,
                items: Vec::new(),
            };
        };
        let edit = |f: TakeFolder| {
            Action::Edit(Command::SetClipContent {
                clip: clip.id,
                start: clip.start,
                content: Box::new(ClipContent::Takes(f)),
            })
        };
        let mut use_take = f.clone();
        use_take.use_take(take);
        let mut without = f.clone();
        without.remove_take(take);
        let name = f.takes.get(take).map_or("take", |t| t.name.as_str());
        let mut items = vec![
            MenuItem::new(format!("Use “{name}”"), edit(use_take)),
            MenuItem::new(format!("Delete “{name}”"), edit(without)),
        ];
        if f.takes.len() <= 1 {
            items[1] = MenuItem::disabled("Delete take (the only one)");
        }
        let hearing = model.auditioned_take() == Some((clip.id, take));
        items.push(
            MenuItem::new(
                format!("Audition “{name}”"),
                Action::AuditionTake {
                    clip: clip.id,
                    take: (!hearing).then_some(take),
                },
            )
            .checked(hearing)
            .separated(),
        );
        let now = model.take_lane_height(clip.track);
        let heights = [
            ("Small", 20.0),
            ("Normal", 30.0),
            ("Large", 60.0),
            ("Huge", 120.0),
        ]
        .into_iter()
        .map(|(label, h)| {
            MenuItem::new(
                label,
                Action::SetTakeLaneHeight {
                    track: clip.track,
                    height: h,
                },
            )
            .checked((now - h).abs() < 0.5)
        })
        .collect();
        items.push(MenuItem::submenu("Take Lane Height", heights));
        items.push(MenuItem::new("Flatten Comp", Action::FlattenTakes(clip.id)).separated());
        items.push(MenuItem::new(
            "Hide Takes",
            Action::ToggleTakeLanes(clip.id),
        ));
        HostRequest::ContextMenu { at, items }
    }

    fn rename_request(t: &Track, at: Rect) -> HostRequest<Action> {
        let id = t.id;
        HostRequest::TextInput {
            at,
            initial: t.name.clone(),
            commit: Box::new(move |text| {
                let n = text.trim();
                (!n.is_empty()).then(|| {
                    Action::Edit(Command::RenameTrack {
                        track: id,
                        name: n.to_string(),
                    })
                })
            }),
        }
    }

    fn press(
        &mut self,
        pos: Point,
        clicks: u32,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        cx.request(HostRequest::GrabFocus);
        let Some(hit) = self.hit_test(pos, size, model) else {
            return false;
        };
        if let Hit::Automation { track, .. } = hit {
            let tracks = Self::lane_tracks(model);
            let Some((i, t)) = tracks.iter().enumerate().find(|(_, t)| t.id == track) else {
                return false;
            };
            let row = self.row_rect(i, size);
            if let Some((g, part)) = self.auto_hit(model, t, row, pos) {
                return self.auto_press(model, g, part, pos, clicks, mods, size.w, cx);
            }
            return true;
        }
        match hit {
            Hit::TakeAudition { clip, take } => {
                let on = model.auditioned_take() == Some((clip, take));
                cx.emit(Action::AuditionTake {
                    clip,
                    take: (!on).then_some(take),
                });
            }
            Hit::TakeLanesEdge(track) if clicks >= 2 => cx.emit(Action::SetTakeLaneHeight {
                track,
                height: faderframe_workspace::TAKE_LANE_HEIGHT,
            }),
            Hit::TakeLanesEdge(track) => {
                let lanes = model
                    .project()
                    .track(track)
                    .map_or(1, |t| Self::open_lanes(t, model));
                self.drag = Some(Drag::TakeLanes {
                    track,
                    start_y: pos.y,
                    start_h: model.take_lane_height(track),
                    lanes,
                });
                cx.set_cursor(Cursor::ResizeVertical);
            }
            Hit::HeaderEdge if clicks >= 2 => {
                cx.emit(Action::SetHeaderWidth(self.theme.arranger.header_width));
            }
            Hit::HeaderEdge => {
                self.drag = Some(Drag::HeaderWidth {
                    start_x: pos.x,
                    start_w: self.header_w(),
                });
                cx.set_cursor(Cursor::ResizeHorizontal);
            }
            Hit::Automation { .. } => {}
            Hit::Global(g) => return self.global_press(g, pos, clicks, mods, size, model, cx),
            Hit::Corner => cx.request(Self::grid_menu(model, pos)),
            Hit::Arrangement => {
                // Double-click a pad's name: rename it there.
                if let (true, Some(id)) = (clicks >= 2, model.shown_pad()) {
                    cx.request(HostRequest::TextInput {
                        at: self.arrangement_rect(model),
                        initial: model.shown_arrangement_name(),
                        commit: Box::new(move |text| {
                            let name = text.trim();
                            (!name.is_empty()).then(|| {
                                Action::ScratchPad(faderframe_session::scratch::ScratchOp::Rename {
                                    pad: id,
                                    name: name.to_string(),
                                })
                            })
                        }),
                    });
                } else {
                    let r = self.arrangement_rect(model);
                    cx.request(HostRequest::ContextMenu {
                        at: Point::new(r.x, r.bottom()),
                        items: scratch::arrangement_menu(model),
                    });
                }
            }
            Hit::AddTrack => {
                let r = self.add_track_rect(Self::lane_tracks(model).len(), size);
                cx.request(Self::add_track_menu(model, Point::new(r.x, r.bottom())));
            }
            Hit::Ruler(t) => {
                cx.emit(Action::Transport(TransportAction::Locate(
                    self.snap(t, model, mods),
                )));
                self.drag = Some(Drag::Scrub { audible: false });
            }
            Hit::LoopBand(t) => {
                self.drag = Some(Drag::Loop {
                    anchor: self.snap(t, model, mods),
                    moved: false,
                });
            }
            Hit::PlayStart => {
                if let Some(m) = model.project().play_start {
                    self.drag = Some(Drag::PlayStart {
                        offset: self.time_at(pos.x) - m.at,
                        moved: false,
                        origin: pos,
                    });
                    cx.set_cursor(Cursor::ResizeHorizontal);
                }
            }
            Hit::LoopStart | Hit::LoopEnd => {
                if let Some(range) = model.project().loop_range {
                    let start = matches!(hit, Hit::LoopStart);
                    self.drag = Some(Drag::LoopEdge {
                        start,
                        range,
                        offset: self.time_at(pos.x) - if start { range.start } else { range.end },
                    });
                    cx.emit(Action::BeginGesture("Resize Loop".into()));
                    cx.set_cursor(Cursor::ResizeHorizontal);
                }
            }
            Hit::Header(id, part) => {
                let Some(t) = model.project().track(id) else {
                    return false;
                };
                let edit = |cx: &mut EventCx<'_, Action>, c| cx.emit(Action::Edit(c));
                match part {
                    HeaderPart::Automation => cx.emit(Action::ToggleTrackAutomation(id)),
                    HeaderPart::Resize if clicks >= 2 => cx.emit(Action::SetTrackHeight {
                        track: (!mods.alt).then_some(id),
                        height: self.row_h(),
                    }),
                    HeaderPart::Resize => {
                        self.drag = Some(Drag::Height {
                            track: id,
                            start_y: pos.y,
                            start_h: self.base_h(model, id),
                            all: mods.alt,
                        });
                        cx.set_cursor(Cursor::ResizeVertical);
                    }
                    HeaderPart::Mute => edit(
                        cx,
                        Command::SetTrackMute {
                            track: id,
                            on: !t.mute,
                        },
                    ),
                    HeaderPart::Solo => edit(
                        cx,
                        Command::SetTrackSolo {
                            track: id,
                            on: !t.solo,
                        },
                    ),
                    HeaderPart::Record if t.kind.has_clips() => edit(
                        cx,
                        Command::SetTrackRecordArm {
                            track: id,
                            on: !t.record_arm,
                        },
                    ),
                    HeaderPart::Monitor if t.kind == TrackKind::Audio => {
                        let mode = if t.monitor == MonitorMode::Off {
                            MonitorMode::Input
                        } else {
                            MonitorMode::Off
                        };
                        edit(cx, Command::SetTrackMonitor { track: id, mode });
                    }
                    HeaderPart::Volume if t.kind.has_audio() || t.kind == TrackKind::Vca => {
                        if clicks >= 2 {
                            edit(cx, Command::SetTrackVolume { track: id, db: 0.0 });
                        } else if let Some(l) = self.header_layout(model, id, size) {
                            cx.emit(Action::BeginGesture("Volume".into()));
                            self.drag = Some(Drag::Volume {
                                track: id,
                                origin_x: pos.x,
                                start: self.law.db_to_position(model.shown_volume_db(t)),
                                width: l.volume.w,
                            });
                            cx.set_cursor(Cursor::ResizeHorizontal);
                        }
                    }
                    HeaderPart::Pan if t.kind.has_audio() && model.project().dual_panned(t) => {
                        if clicks >= 2 {
                            edit(
                                cx,
                                Command::SetTrackDualPan {
                                    track: id,
                                    pan: faderframe_core::DualPan::ORIGINAL,
                                },
                            );
                        } else {
                            cx.emit(Action::BeginGesture("Pan".into()));
                            self.drag = Some(Drag::DualPan {
                                track: id,
                                origin_y: pos.y,
                                start: model.shown_dual_pan(t),
                            });
                            cx.set_cursor(Cursor::ResizeVertical);
                        }
                    }
                    HeaderPart::Pan if t.kind.has_audio() => {
                        if clicks >= 2 {
                            edit(
                                cx,
                                Command::SetTrackPan {
                                    track: id,
                                    pan: 0.0,
                                },
                            );
                        } else {
                            cx.emit(Action::BeginGesture("Pan".into()));
                            self.drag = Some(Drag::Pan {
                                track: id,
                                origin_y: pos.y,
                                start: (model.shown_pan(t) + 1.0) * 0.5,
                            });
                            cx.set_cursor(Cursor::ResizeVertical);
                        }
                    }
                    HeaderPart::Meter => cx.emit(Action::ResetClipIndicators),
                    HeaderPart::Color => {
                        cx.emit(Action::PickColor(faderframe_session::ColorTarget::Track(
                            id,
                        )));
                    }
                    HeaderPart::Fold => cx.emit(Action::ToggleFolder(id)),
                    HeaderPart::Group if t.is_folder() => cx.emit(Action::SetFolderGroup {
                        folder: id,
                        on: !t.folder_group,
                    }),
                    HeaderPart::Body if clicks >= 2 && t.is_folder() => {
                        cx.emit(Action::ToggleFolder(id));
                    }
                    HeaderPart::Name if clicks >= 2 => {
                        if let Some(l) = self.header_layout(model, id, size) {
                            let at = if t.is_folder() {
                                folder_name_rect(&l)
                            } else {
                                l.name
                            };
                            cx.request(Self::rename_request(t, at));
                        }
                    }
                    _ => {
                        // Shift: the tracks from the one last clicked to
                        // this one, as the rows show them.
                        if mods.shift
                            && !mods.toggle()
                            && let Some(anchor) = model.selection.anchor_track
                        {
                            let rows = Self::lane_tracks(model);
                            let at = |t: TrackId| rows.iter().position(|r| r.id == t);
                            if let (Some(a), Some(b)) = (at(anchor), at(id)) {
                                cx.emit(Action::SelectTracks {
                                    tracks: rows[a.min(b)..=a.max(b)]
                                        .iter()
                                        .map(|t| t.id)
                                        .collect(),
                                    mode: SelectMode::Replace,
                                });
                                return true;
                            }
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
                        // The name and the header's free space carry the
                        // track: drag it to reorder, or onto a folder.
                        if matches!(part, HeaderPart::Name | HeaderPart::Body) && !mods.toggle() {
                            self.drag = Some(Drag::Row {
                                track: id,
                                origin_y: pos.y,
                                moved: false,
                                pos,
                            });
                        }
                    }
                }
            }
            Hit::TakeToggle(clip) => cx.emit(Action::ToggleTakeLanes(clip)),
            Hit::TakeLane {
                clip,
                take,
                pos: frame,
                ..
            } => {
                let Some(f) = model.project().clip(clip).and_then(|c| c.as_takes()) else {
                    return false;
                };
                let anchor = self.snap_frame(frame, clip, model, mods);
                self.comp = Some(CompDrag {
                    clip,
                    take,
                    at: frame,
                    anchor,
                    origin: pos,
                    moved: false,
                    base: f.clone(),
                });
            }
            Hit::Clip { clip, track, at } => {
                return self.press_clip(clip, track, at, pos, clicks, mods, size, model, cx);
            }
            Hit::Lane { track, at } => {
                let Some(t) = model.project().track(track) else {
                    return false;
                };
                // (Not with the Zoom or Scrub tool: their double-clicks are
                // two zooms, two scrubs.)
                if clicks >= 2
                    && matches!(t.kind, TrackKind::Instrument | TrackKind::Midi)
                    && !matches!(
                        model.editor.tool,
                        faderframe_session::EditTool::Zoom | faderframe_session::EditTool::Scrub
                    )
                {
                    let meter = &model.project().timeline.meter;
                    let start = snap_floor(at, GridDivision::Bar, meter);
                    let length = meter.bar_start(meter.bar_at(start) + 1) - start;
                    cx.emit(Action::CreateMidiClip {
                        track,
                        start,
                        length,
                    });
                    return true;
                }
                if mods.toggle() && Self::marquee_tool(model) {
                    self.start_marquee(pos, mods, None, Some(at), model);
                    return true;
                }
                return self.press_lane(track, at, pos, mods, model, cx);
            }
            Hit::Empty(_) if model.editor.tool == faderframe_session::EditTool::Zoom => {
                return self.press_zoom(pos, mods, cx);
            }
            Hit::Empty(at) => {
                if mods.toggle() && Self::marquee_tool(model) {
                    self.start_marquee(pos, mods, None, Some(at), model);
                    return true;
                }
                cx.emit(Action::ClearSelection);
                cx.emit(Action::SetEditRange(None));
                // Below the tracks too, a click puts the playhead there.
                self.click_locate(self.snap(at, model, mods), model, cx);
            }
        }
        true
    }

    /// Tools with which Ctrl+drag draws a selection rectangle.
    fn marquee_tool(model: &Session) -> bool {
        use faderframe_session::EditTool as T;
        matches!(model.editor.tool, T::Smart | T::Select | T::Grab)
    }

    /// Ctrl pressed in the lanes: a rectangle if it is dragged.
    pub(crate) fn start_marquee(
        &mut self,
        pos: Point,
        mods: Modifiers,
        click: Option<(ClipId, TrackId)>,
        at: Option<MusicalTime>,
        model: &Session,
    ) {
        let s = &model.selection;
        let add = mods.shift;
        self.marquee = Some(Marquee {
            origin: pos,
            current: pos,
            moved: false,
            base: if add {
                s.clips.iter().copied().collect()
            } else {
                Vec::new()
            },
            base_tracks: if add {
                s.tracks.iter().copied().collect()
            } else {
                Vec::new()
            },
            click,
            at,
        });
    }

    /// The clips (and their tracks) a rectangle in view coordinates
    /// touches.
    fn clips_in(&self, area: Rect, size: Size, model: &Session) -> (Vec<ClipId>, Vec<TrackId>) {
        let (mut clips, mut tracks) = (Vec::new(), Vec::new());
        for (i, t) in Self::lane_tracks(model).iter().enumerate() {
            let row = self.row_rect(i, size);
            if row.bottom() < area.y || row.y > area.bottom() {
                continue;
            }
            for c in model.project().clips_of(t.id) {
                let r = self.clip_rect(c, row, model);
                let hit = r.x < area.right()
                    && r.right() > area.x
                    && r.y < area.bottom()
                    && r.bottom() > area.y;
                if hit {
                    clips.push(c.id);
                    if !tracks.contains(&t.id) {
                        tracks.push(t.id);
                    }
                }
            }
        }
        (clips, tracks)
    }

    fn marquee_rect(m: &Marquee) -> Rect {
        let (x0, x1) = (m.origin.x.min(m.current.x), m.origin.x.max(m.current.x));
        let (y0, y1) = (m.origin.y.min(m.current.y), m.origin.y.max(m.current.y));
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    fn marquee_move(
        &mut self,
        pos: Point,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(m) = self.marquee.as_mut() else {
            return false;
        };
        if !m.moved && pos.distance(m.origin) < DRAG_THRESHOLD {
            return true;
        }
        m.moved = true;
        m.current = pos;
        let area = Self::marquee_rect(m);
        let (base, base_tracks) = (m.base.clone(), m.base_tracks.clone());
        let (mut clips, mut tracks) = self.clips_in(area, size, model);
        for c in base {
            if !clips.contains(&c) {
                clips.push(c);
            }
        }
        for t in base_tracks {
            if !tracks.contains(&t) {
                tracks.push(t);
            }
        }
        let now: std::collections::BTreeSet<ClipId> = clips.iter().copied().collect();
        if now != model.selection.clips {
            cx.emit(Action::SelectClips {
                clips,
                mode: SelectMode::Replace,
            });
            cx.emit(Action::SelectTracks {
                tracks,
                mode: SelectMode::Replace,
            });
        }
        true
    }

    fn marquee_release(&mut self, model: &Session, cx: &mut EventCx<'_, Action>) -> bool {
        let Some(m) = self.marquee.take() else {
            return false;
        };
        if !m.moved {
            match (m.click, m.at) {
                // A Ctrl-click on a clip: in or out of the selection.
                (Some((clip, track)), _) => {
                    cx.emit(Action::SelectClips {
                        clips: vec![clip],
                        mode: SelectMode::Toggle,
                    });
                    cx.emit(Action::SelectTracks {
                        tracks: vec![track],
                        mode: SelectMode::Add,
                    });
                    Self::clips_not_range(model, cx);
                }
                (None, Some(at)) => self.click_locate(at, model, cx),
                (None, None) => {}
            }
        } else {
            Self::clips_not_range(model, cx);
        }
        cx.set_cursor(Cursor::Default);
        true
    }

    fn drag_move(
        &mut self,
        pos: Point,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        if self.marquee_move(pos, size, model, cx) {
            return;
        }
        if self.global_drag_move(pos, mods, size, model, cx) {
            return;
        }
        if self.auto_drag_move(model, pos, mods, size.w, cx) {
            return;
        }
        // Clips carried over another view (the launcher) go back here
        // meanwhile.
        if let Some(clip_edit::EditDrag::Move {
            clips, moved: true, ..
        }) = &self.edit_drag
            && model.editor.edit_mode != faderframe_session::editing::EditMode::Shuffle
            && !Rect::from_size(size).contains(pos)
        {
            cx.emit(Action::MoveClips {
                clips: clips.clone(),
                by: 0,
                tracks: 0,
            });
            return;
        }
        if self.edit_drag_move(pos, mods, model, cx) {
            return;
        }
        if let Some(comp) = &mut self.comp {
            if !comp.moved {
                if pos.distance(comp.origin) < DRAG_THRESHOLD {
                    return;
                }
                comp.moved = true;
                cx.emit(Action::BeginGesture("Comp Takes".into()));
            }
            let (clip, take, anchor) = (comp.clip, comp.take, comp.anchor);
            let mut f = comp.base.clone();
            let Some(c) = model.project().clip(clip) else {
                return;
            };
            let p = model.project();
            let rate = p.sample_rate as f64;
            let at = self.time_at(pos.x).max(MusicalTime::ZERO);
            let frame = p.timeline.to_samples(at, rate) - p.timeline.to_samples(c.start, rate);
            let frame = self.snap_frame(frame, clip, model, mods);
            f.set_comp(anchor.min(frame), anchor.max(frame), Some(take));
            if c.as_takes() != Some(&f) {
                cx.emit(Action::Edit(Command::SetClipContent {
                    clip,
                    start: c.start,
                    content: Box::new(ClipContent::Takes(f)),
                }));
            }
            return;
        }
        match self.drag {
            Some(Drag::Scrub { audible }) => {
                let t = self.snap(self.time_at(pos.x).max(MusicalTime::ZERO), model, mods);
                cx.emit(Action::Transport(if audible {
                    TransportAction::Scrub(t)
                } else {
                    TransportAction::Locate(t)
                }));
            }
            Some(Drag::Loop { anchor, .. }) => {
                let skip = faderframe_session::SnapSkip::loop_bounds();
                let t = self.snap_point(
                    self.time_at(pos.x).max(MusicalTime::ZERO),
                    model,
                    mods,
                    &skip,
                );
                if t != anchor {
                    if let Some(Drag::Loop { moved, .. }) = &mut self.drag {
                        if !*moved {
                            cx.emit(Action::BeginGesture("Set Loop".into()));
                        }
                        *moved = true;
                    }
                    let range = MusicalRange::new(anchor.min(t), anchor.max(t));
                    cx.emit(Action::Transport(TransportAction::SetLoop(range)));
                }
            }
            Some(Drag::PlayStart {
                offset,
                moved,
                origin,
            }) => {
                let Some(m) = model.project().play_start else {
                    return;
                };
                if !moved && pos.distance(origin) < DRAG_THRESHOLD {
                    return;
                }
                if !moved {
                    cx.emit(Action::BeginGesture("Move Play Start Marker".into()));
                    self.drag = Some(Drag::PlayStart {
                        offset,
                        moved: true,
                        origin,
                    });
                }
                let at = self.snap_point(
                    (self.time_at(pos.x) - offset).max(MusicalTime::ZERO),
                    model,
                    mods,
                    &faderframe_session::SnapSkip::play_start_marker(),
                );
                if at != m.at {
                    // Moving it keeps whether playback starts there.
                    cx.emit(Action::Edit(Command::SetPlayStart {
                        marker: Some(faderframe_project::PlayStart { at, ..m }),
                    }));
                }
            }
            Some(Drag::LoopEdge {
                start,
                range,
                offset,
            }) => {
                let at = self.snap_point(
                    (self.time_at(pos.x) - offset).max(MusicalTime::ZERO),
                    model,
                    mods,
                    &faderframe_session::SnapSkip::loop_bounds(),
                );
                let tick = MusicalTime::from_ticks(1);
                let range = if start {
                    MusicalRange::new(at.min(range.end - tick), range.end)
                } else {
                    MusicalRange::new(range.start, at.max(range.start + tick))
                };
                if range != model.project().loop_range {
                    cx.emit(Action::Transport(TransportAction::SetLoop(range)));
                }
            }
            Some(Drag::Volume {
                track,
                origin_x,
                start,
                width,
            }) => {
                let scale = if mods.fine() { 0.2 } else { 1.0 };
                let pos_new = (start + (pos.x - origin_x) / width.max(1.0) * scale).clamp(0.0, 1.0);
                cx.emit(Action::Edit(Command::SetTrackVolume {
                    track,
                    db: self.law.position_to_db(pos_new),
                }));
            }
            Some(Drag::DualPan {
                track,
                origin_y,
                start,
            }) => {
                // Knob travel 0…1 is the whole −1…1 range.
                let by = controls::drag_delta(pos.y - origin_y, mods.fine()) * 2.0;
                cx.emit(Action::Edit(Command::SetTrackDualPan {
                    track,
                    pan: faderframe_core::DualPan {
                        left: (start.left + by).clamp(-1.0, 1.0),
                        right: (start.right + by).clamp(-1.0, 1.0),
                    },
                }));
            }
            Some(Drag::Pan {
                track,
                origin_y,
                start,
            }) => {
                let v =
                    (start + controls::drag_delta(pos.y - origin_y, mods.fine())).clamp(0.0, 1.0);
                let pan = v * 2.0 - 1.0;
                cx.emit(Action::Edit(Command::SetTrackPan {
                    track,
                    pan: if pan.abs() < 0.01 { 0.0 } else { pan },
                }));
            }
            Some(Drag::Row {
                track,
                origin_y,
                moved,
                ..
            }) => {
                let moved = moved || (pos.y - origin_y).abs() > 4.0;
                self.drag = Some(Drag::Row {
                    track,
                    origin_y,
                    moved,
                    pos,
                });
                if moved {
                    cx.set_cursor(Cursor::Grabbing);
                }
                cx.redraw();
            }
            Some(Drag::HeaderWidth { start_x, start_w }) => {
                let (lo, hi) = faderframe_workspace::HEADER_WIDTH_RANGE;
                let w = (start_w + pos.x - start_x).round().clamp(lo, hi);
                if (w - self.header_w()).abs() >= 1.0 {
                    cx.emit(Action::SetHeaderWidth(w));
                }
            }
            Some(Drag::Height {
                track,
                start_y,
                start_h,
                all,
            }) => {
                let h = (start_h + pos.y - start_y).round();
                if (h - self.base_h(model, track)).abs() >= 1.0 {
                    cx.emit(Action::SetTrackHeight {
                        track: (!all).then_some(track),
                        height: h,
                    });
                }
            }
            Some(Drag::TakeLanes {
                track,
                start_y,
                start_h,
                lanes,
            }) => {
                let h = (start_h + (pos.y - start_y) / lanes.max(1) as f32).round();
                if (h - model.take_lane_height(track)).abs() >= 1.0 {
                    cx.emit(Action::SetTakeLaneHeight { track, height: h });
                }
            }
            Some(Drag::Pan2D { origin, sx, sy }) => {
                self.scroll_x = sx - (pos.x - origin.x) as f64;
                self.scroll_y = sy - (pos.y - origin.y);
                self.clamp_scroll(model, size);
                cx.redraw();
            }
            None => {}
        }
    }

    fn release(&mut self, model: &Session, size: Size, cx: &mut EventCx<'_, Action>) {
        if self.snap_guide.take().is_some() {
            cx.redraw();
        }
        if self.marquee_release(model, cx) {
            return;
        }
        if self.global_release(model, size, cx) {
            return;
        }
        if self.auto_release(model, cx) {
            cx.set_cursor(Cursor::Default);
            return;
        }
        if self.edit_release(model, size, cx) {
            return;
        }
        if let Some(comp) = self.comp.take() {
            if comp.moved {
                cx.emit(Action::EndGesture);
            } else if let Some(c) = model.project().clip(comp.clip) {
                // A click puts the take into the comp section under the
                // pointer only; the whole take is "Use" in the lane's menu.
                let mut f = comp.base;
                f.use_take_at(comp.at, comp.take);
                if c.as_takes() != Some(&f) {
                    cx.emit(Action::Edit(Command::SetClipContent {
                        clip: comp.clip,
                        start: c.start,
                        content: Box::new(ClipContent::Takes(f)),
                    }));
                }
            }
            cx.set_cursor(Cursor::Default);
            return;
        }
        match self.drag.take() {
            Some(Drag::Row {
                track,
                moved: true,
                pos,
                ..
            }) => {
                match self.row_drop(model, size, track, pos) {
                    Some(RowDrop::Between { after, before, .. }) => {
                        cx.emit(Action::PlaceTrack {
                            track,
                            after,
                            before,
                        });
                    }
                    Some(RowDrop::Into { folder, .. }) => cx.emit(Action::MoveToFolder {
                        tracks: vec![track],
                        folder: Some(folder),
                    }),
                    None => {}
                }
                cx.set_cursor(Cursor::Default);
            }
            Some(
                Drag::Volume { .. }
                | Drag::Pan { .. }
                | Drag::DualPan { .. }
                | Drag::LoopEdge { .. }
                | Drag::Loop { moved: true, .. }
                | Drag::PlayStart { moved: true, .. },
            ) => {
                cx.emit(Action::EndGesture);
            }
            Some(Drag::Loop { moved: false, .. }) => {
                cx.emit(Action::Transport(TransportAction::ToggleLoop));
            }
            _ => {}
        }
        let _ = model;
        cx.set_cursor(Cursor::Default);
    }

    fn zoom_at(&mut self, x: f32, factor: f32, model: &Session, size: Size) {
        let t = self.time_at(x);
        self.ppq = (self.ppq * factor).clamp(MIN_PPQ, MAX_PPQ);
        self.scroll_x = t.quarters() * self.ppq as f64 - (x - self.header_w()) as f64;
        self.clamp_scroll(model, size);
    }
}

impl CanvasView<Session, Action> for ArrangerView {
    fn accessible(
        &self,
        size: Size,
        model: &Session,
    ) -> Vec<faderframe_ui_canvas::AccessNode<Action>> {
        self.access_nodes(size, model)
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Arranger".into())
    }

    /// Tab goes to the next transient or clip boundary.
    fn uses_tab(&self) -> bool {
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.update_header_width(model);
        self.update_rows(model);
        self.view_w = size.w;
        self.apply_zoom_request(model, size);
        self.follow(model, size);
        self.clamp_scroll(model, size);
        if model.shown_pad().is_none() {
            model.note_arranger_place(faderframe_session::ViewPlace {
                zoom: self.ppq,
                left: self.scroll_x / f64::from(self.ppq),
                top: self.scroll_y,
            });
        }
        let a = &theme.arranger;
        p.fill(Rect::from_size(size), a.background);
        let lanes = Rect::new(
            self.header_w(),
            self.ruler_h(),
            size.w - self.header_w(),
            size.h - self.ruler_h(),
        );
        let tracks = Self::lane_tracks(model);
        let rows = self.visible_rows(tracks.len(), size);

        // Lanes.
        p.push_clip(lanes);
        for i in rows.clone() {
            let row = self.row_rect(i, size);
            let t = tracks[i];
            let bg = if model.selection.tracks.contains(&t.id) {
                a.lane_selected
            } else if i % 2 == 0 {
                a.lane_a
            } else {
                a.lane_b
            };
            p.fill(Rect::new(lanes.x, row.y, lanes.w, row.h), bg);
            let base = self.base_h(model, t.id);
            if row.h > base + 0.5 {
                // Take-lane area of a track with an open take folder.
                p.fill(
                    Rect::new(lanes.x, row.y + base, lanes.w, row.h - base),
                    Color::rgba(0.0, 0.0, 0.0, 0.22),
                );
            }
            p.hline(
                lanes.x,
                lanes.right(),
                row.bottom() - 1.0,
                a.header_border.with_alpha(0.6),
            );
        }
        self.paint_grid(p, lanes, model);
        self.paint_marker_guides(p, lanes, model);
        if let Some(lr) = model.project().loop_range
            && model.project().loop_enabled
        {
            let x0 = self.x_of(lr.start).max(lanes.x);
            let x1 = self.x_of(lr.end).min(lanes.right());
            if x1 > x0 {
                p.fill(
                    Rect::new(x0, lanes.y, x1 - x0, lanes.h),
                    a.loop_on.with_alpha(0.05),
                );
                p.vline(
                    self.x_of(lr.start),
                    lanes.y,
                    lanes.bottom(),
                    a.loop_on.with_alpha(0.5),
                );
                p.vline(
                    self.x_of(lr.end),
                    lanes.y,
                    lanes.bottom(),
                    a.loop_on.with_alpha(0.5),
                );
            }
        }
        if let Some(pr) = model.project().punch_range
            && model.project().punch_enabled
        {
            for x in [self.x_of(pr.start), self.x_of(pr.end)] {
                p.vline(x, lanes.y, lanes.bottom(), a.record.with_alpha(0.55));
            }
        }
        for i in rows.clone() {
            let row = self.row_rect(i, size);
            let t = tracks[i];
            if t.is_folder() {
                self.paint_folder_lane(p, row, lanes, t, model);
                continue;
            }
            for clip in model.project().clips_of(t.id) {
                let rect = self.clip_rect(clip, row, model);
                if rect.right() < lanes.x || rect.x > lanes.right() {
                    continue;
                }
                self.paint_clip(p, rect, lanes, clip, t, model);
                if let Some(f) = clip.as_takes()
                    && model.takes_open(clip.id)
                {
                    let color = color_of(clip.color.unwrap_or(t.color));
                    let top = row.y + self.base_h(model, t.id);
                    let lane_h = model.take_lane_height(t.id);
                    self.paint_take_lanes(p, rect, (top, lane_h), lanes, clip, f, model, color);
                }
            }
            // The track plays the clip launcher, not the arrangement.
            if model.launch_state(t.id).is_some_and(|s| !s.arrangement) {
                let x = lanes.x.max(row.x);
                p.fill(
                    Rect::new(x, row.y, lanes.right() - x, row.h),
                    a.background.with_alpha(0.6),
                );
            }
        }
        for i in rows.clone() {
            let row = self.row_rect(i, size);
            let t = tracks[i];
            self.paint_automation(p, model, t, row, lanes, false, color_of(t.color));
        }
        self.paint_recording(p, lanes, size, &tracks, model);
        if tracks.is_empty() {
            p.text(
                "Empty project — add a track from the Track menu or right-click the header column",
                lanes,
                &TextStyle::new(theme.fonts.normal, theme.ui.text_faint).center(),
            );
        }
        self.paint_edit_overlay(p, lanes, size, &tracks, model);
        let px = self.x_of(model.playhead());
        if px >= lanes.x {
            let glow = theme.console.look.glow;
            if glow > 0.0 {
                // A soft light either side of the line.
                let halo = Rect::new(px - 6.0, lanes.y, 12.0, lanes.h);
                p.fill_rect(
                    halo,
                    &Paint::Linear {
                        start: Point::new(halo.x, halo.y),
                        end: Point::new(halo.right(), halo.y),
                        stops: vec![
                            (0.0, a.playhead.with_alpha(0.0)),
                            (0.5, a.playhead.with_alpha(0.16 * glow.min(2.0))),
                            (1.0, a.playhead.with_alpha(0.0)),
                        ],
                    },
                );
                // The line itself in the accent's gradient.
                let line = Rect::new(px - 0.75, lanes.y, 1.5, lanes.h);
                p.fill_rect(line, &Paint::vertical(line, a.playhead, theme.ui.accent_2));
            } else {
                p.fill(Rect::new(px - 0.75, lanes.y, 1.5, lanes.h), a.playhead);
            }
        }
        self.paint_snap_guide(p, lanes);
        if let Some((target, at)) = self.drop_at {
            self.paint_drop_hint(p, lanes, size, &tracks, target, at);
        }
        p.pop_clip();
        p.shadow(
            Rect::new(lanes.x, lanes.y - 4.0, lanes.w, 4.0),
            0.0,
            Color::rgba(0.0, 0.0, 0.0, 0.5),
            0.0,
            2.0,
            5.0,
        );

        // Headers.
        let headers = Rect::new(
            0.0,
            self.ruler_h(),
            self.header_w(),
            size.h - self.ruler_h(),
        );
        p.fill(headers, a.header_bg.darken(0.25));
        p.push_clip(headers);
        for i in rows {
            let row = self.row_rect(i, size);
            let l = HeaderLayout::new(self.header_rect(model, tracks[i], row.y));
            // The folders it is in: a stripe in each one's colour.
            if l.row.x > 0.0 {
                let indent = Rect::new(0.0, row.y, l.row.x, row.h);
                p.fill(indent, a.header_bg.darken(0.12));
                let chain = model.project().folder_chain(tracks[i]);
                for (k, f) in chain.iter().rev().enumerate() {
                    let x = (k as f32 * FOLDER_INDENT + 4.0).min(l.row.x - 3.0);
                    p.fill(
                        Rect::new(x, row.y, 3.0, row.h),
                        color_of(f.color).with_alpha(0.75),
                    );
                }
            }
            self.paint_header(p, &l, tracks[i], model);
            self.paint_automation(
                p,
                model,
                tracks[i],
                row,
                lanes,
                true,
                color_of(tracks[i].color),
            );
        }
        // The edge being dragged, or under the pointer, to size a track.
        let sizing = match (self.drag, self.hover) {
            (Some(Drag::Height { track, .. }), _)
            | (_, Some(Hit::Header(track, HeaderPart::Resize))) => Some(track),
            _ => None,
        };
        if let Some(i) = sizing.and_then(|id| tracks.iter().position(|t| t.id == id)) {
            let y = self.resize_edge(model, i, tracks[i].id, size);
            p.fill(
                Rect::new(0.0, y - 1.0, self.header_w(), 2.0),
                theme.ui.accent.with_alpha(0.85),
            );
        }
        // The bottom of take lanes being sized, or under the pointer.
        let take_sizing = match (self.drag, self.hover) {
            (Some(Drag::TakeLanes { track, .. }), _) | (_, Some(Hit::TakeLanesEdge(track))) => {
                Some(track)
            }
            _ => None,
        };
        if let Some(i) = take_sizing.and_then(|id| tracks.iter().position(|t| t.id == id)) {
            let y = self.take_lanes_bottom(model, i, tracks[i], size);
            p.fill(
                Rect::new(0.0, y - 1.0, size.w, 2.0),
                theme.ui.accent.with_alpha(0.85),
            );
        }
        // A dragged track: where it lands, and the rows scroll at the edges.
        if let Some(Drag::Row {
            track,
            moved: true,
            pos,
            ..
        }) = self.drag
        {
            let edge = 28.0;
            let top = self.ruler_h();
            let step = if pos.y < top + edge {
                -(top + edge - pos.y).min(edge) * 0.5
            } else if pos.y > size.h - edge {
                (pos.y - (size.h - edge)).min(edge) * 0.5
            } else {
                0.0
            };
            if step != 0.0 {
                self.scroll_y += step;
                self.clamp_scroll(model, size);
            }
            match self.row_drop(model, size, track, pos) {
                Some(RowDrop::Between { y, .. }) => {
                    p.fill(
                        Rect::new(0.0, y - 1.5, size.w, 3.0),
                        theme.ui.accent.with_alpha(0.9),
                    );
                }
                Some(RowDrop::Into { row, .. }) => {
                    p.fill(row, theme.ui.accent.with_alpha(0.16));
                    p.stroke_rounded(row.inset(1.0), 3.0, 2.0, theme.ui.accent.with_alpha(0.85));
                }
                None => {}
            }
        }
        // The "+" under the last track.
        let add = self.add_track_rect(tracks.len(), size);
        if add.bottom() > self.ruler_h() && add.y < size.h {
            let hot = self.hover == Some(Hit::AddTrack);
            let fill = if hot {
                theme.ui.accent.with_alpha(0.22)
            } else {
                a.header_bg
            };
            p.fill_rounded(add, 5.0, &Paint::Solid(fill));
            p.stroke_rounded(add, 5.0, 1.0, a.header_border);
            p.text(
                "+  Add Track",
                add,
                &TextStyle::new(
                    theme.fonts.small,
                    if hot {
                        theme.ui.text
                    } else {
                        theme.ui.text_dim
                    },
                )
                .bold()
                .center(),
            );
        }
        p.pop_clip();
        p.vline(self.header_w() - 1.0, 0.0, size.h, a.header_border);
        p.shadow(
            Rect::new(self.header_w(), self.ruler_h(), 3.0, size.h),
            0.0,
            Color::rgba(0.0, 0.0, 0.0, 0.45),
            2.0,
            0.0,
            5.0,
        );

        self.paint_ruler(
            p,
            Rect::new(
                self.header_w(),
                0.0,
                size.w - self.header_w(),
                self.base_ruler_h(),
            ),
            model,
        );
        self.paint_corner(
            p,
            Rect::new(0.0, 0.0, self.header_w(), self.base_ruler_h()),
            model,
        );
        self.paint_pad_tint(
            p,
            Rect::new(
                self.header_w(),
                0.0,
                size.w - self.header_w(),
                self.base_ruler_h(),
            ),
            model,
        );
        self.paint_arrangement(p, model);
        self.paint_global_lanes(p, size, model);
        // The selection rectangle being drawn (Ctrl+drag).
        if let Some(m) = self.marquee.as_ref().filter(|m| m.moved) {
            let lanes = Rect::new(
                self.header_w(),
                self.ruler_h(),
                (size.w - self.header_w()).max(0.0),
                (size.h - self.ruler_h()).max(0.0),
            );
            let r = Self::marquee_rect(m).intersection(&lanes);
            p.push_clip(lanes);
            p.fill(r, theme.ui.selection.with_alpha(0.14));
            p.stroke_rounded(r, 0.0, 1.0, theme.ui.selection.with_alpha(0.8));
            p.pop_clip();
        }
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        self.update_header_width(model);
        self.update_rows(model);
        self.view_w = size.w;
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                modifiers,
                clicks,
            } => self.press(pos, clicks, modifiers, size, model, cx),
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Middle,
                ..
            } => {
                self.drag = Some(Drag::Pan2D {
                    origin: pos,
                    sx: self.scroll_x,
                    sy: self.scroll_y,
                });
                cx.set_cursor(Cursor::Move);
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                match self.hit_test(pos, size, model) {
                    Some(Hit::Arrangement) => {
                        let r = self.arrangement_rect(model);
                        cx.request(HostRequest::ContextMenu {
                            at: Point::new(r.x, r.bottom()),
                            items: scratch::arrangement_menu(model),
                        });
                    }
                    Some(Hit::Header(id, _)) => {
                        if let Some(t) = model.project().track(id) {
                            cx.request(Self::track_menu(model, t, pos));
                        }
                    }
                    Some(Hit::Clip { clip, .. }) => {
                        if let Some(c) = model.project().clip(clip) {
                            if let Some((_, ClipZone::Fade(edge) | ClipZone::Bend(edge))) =
                                self.zone_at(model, size, pos)
                            {
                                cx.request(Self::fade_menu(model, c, edge, pos));
                                return true;
                            }
                            if !model.selection.clips.contains(&clip) {
                                cx.emit(Action::SelectClips {
                                    clips: vec![clip],
                                    mode: SelectMode::Replace,
                                });
                            }
                            cx.request(Self::clip_menu(model, c, pos));
                        }
                    }
                    Some(
                        hit @ (Hit::Corner
                        | Hit::Ruler(_)
                        | Hit::LoopBand(_)
                        | Hit::LoopStart
                        | Hit::LoopEnd
                        | Hit::PlayStart),
                    ) => {
                        let mut menu = Self::grid_menu(model, pos);
                        // On the ruler: the Play Start Marker there, first
                        // (the grid's choices are a long list).
                        let t = match hit {
                            Hit::Ruler(t) | Hit::LoopBand(t) => Some(t),
                            Hit::PlayStart => model.project().play_start.map(|m| m.at),
                            _ => None,
                        };
                        if let (Some(t), HostRequest::ContextMenu { items, .. }) = (t, &mut menu) {
                            let at = self.snap(t, model, Modifiers::default());
                            if let Some(first) = items.first_mut() {
                                first.separator_before = true;
                            }
                            items.splice(0..0, Self::play_start_items(model, at));
                        }
                        cx.request(menu);
                    }
                    Some(Hit::Global(g)) => cx.request(self.global_menu(g, model, pos)),
                    Some(Hit::TakeLane { clip, take, .. } | Hit::TakeAudition { clip, take }) => {
                        if let Some(c) = model.project().clip(clip) {
                            cx.request(Self::take_menu(model, c, take, pos));
                        }
                    }
                    Some(Hit::Automation { track, header, .. }) => {
                        let tracks = Self::lane_tracks(model);
                        if let Some((i, t)) = tracks.iter().enumerate().find(|(_, t)| t.id == track)
                        {
                            let row = self.row_rect(i, size);
                            if let Some((g, _)) = self.auto_hit(model, t, row, pos) {
                                cx.request(if header {
                                    Self::parameter_menu(model, track, pos)
                                } else {
                                    self.lane_menu(model, g, pos, size.w)
                                });
                            }
                        }
                    }
                    _ => {}
                }
                true
            }
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } => {
                self.drag_move(pos, modifiers, size, model, cx);
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let hit = self.hit_test(pos, size, model);
                let zone = self.zone_at(model, size, pos);
                if zone != self.zone_hover {
                    self.zone_hover = zone;
                    self.hover = hit;
                    cx.redraw();
                    if let Some((_, z)) = zone {
                        cx.set_cursor(Self::zone_cursor(z));
                        return false;
                    }
                    // Leaving a clip: fall through to the plain cursor.
                    self.hover = None;
                }
                if zone.is_some() {
                    return false;
                }
                if hit != self.hover {
                    // Hover highlights: the "+" and a track's resize edge.
                    let lit = |h: Option<Hit>| {
                        matches!(
                            h,
                            Some(
                                Hit::AddTrack
                                    | Hit::Header(_, HeaderPart::Resize)
                                    | Hit::TakeLanesEdge(_)
                            )
                        )
                    };
                    if lit(hit) || lit(self.hover) {
                        cx.redraw();
                    }
                    self.hover = hit;
                    cx.set_cursor(match hit {
                        Some(Hit::Clip { .. }) => Cursor::Grab,
                        Some(Hit::TakeToggle(_)) => Cursor::Pointer,
                        Some(Hit::HeaderEdge) => Cursor::ResizeHorizontal,
                        Some(Hit::TakeLane { .. }) => Cursor::Crosshair,
                        Some(Hit::TakeAudition { .. }) => Cursor::Pointer,
                        Some(Hit::TakeLanesEdge(_)) => Cursor::ResizeVertical,
                        Some(Hit::Automation { header: false, .. }) => Cursor::Crosshair,
                        Some(Hit::Automation { header: true, .. }) => Cursor::Pointer,
                        Some(Hit::Ruler(_) | Hit::LoopBand(_)) => Cursor::Pointer,
                        Some(Hit::LoopStart | Hit::LoopEnd) => Cursor::ResizeHorizontal,
                        Some(Hit::Global(GlobalHit::Section(
                            _,
                            SectionPart::Start | SectionPart::End,
                        ))) => Cursor::ResizeHorizontal,
                        Some(Hit::Global(GlobalHit::Empty(
                            faderframe_session::lanes::GlobalLane::Arranger,
                            _,
                        ))) => Cursor::Crosshair,
                        Some(Hit::Global(_)) => Cursor::Pointer,
                        Some(Hit::Header(_, HeaderPart::Volume)) => Cursor::ResizeHorizontal,
                        Some(Hit::Header(_, HeaderPart::Pan | HeaderPart::Resize)) => {
                            Cursor::ResizeVertical
                        }
                        Some(Hit::Header(_, HeaderPart::Body | HeaderPart::Name)) | None => {
                            Cursor::Default
                        }
                        Some(Hit::Header(..) | Hit::Corner | Hit::Arrangement) => Cursor::Pointer,
                        _ => Cursor::Default,
                    });
                }
                false
            }
            ViewEvent::PointerUp { .. } => {
                self.release(model, size, cx);
                true
            }
            ViewEvent::PointerLeave => {
                self.hover = None;
                if self.zone_hover.take().is_some() {
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
                // The wheel over a clip's gain knob turns it (0.5 dB steps).
                if !modifiers.alt
                    && !modifiers.ctrl
                    && let Some((clip, ClipZone::Gain)) = self.zone_at(model, size, pos)
                {
                    let steps = if precise { -dy / 30.0 } else { -dy };
                    let clips = if model.selection.clips.contains(&clip) {
                        model.selection.clips.iter().copied().collect()
                    } else {
                        vec![clip]
                    };
                    cx.emit(Action::ClipGain {
                        clips,
                        delta_db: (steps * 0.5 * 10.0).round() / 10.0,
                    });
                    return true;
                }
                if modifiers.alt && !modifiers.ctrl {
                    // Alt+wheel: all track heights.
                    let steps = if precise { dy / 30.0 } else { dy };
                    let current = Self::lane_tracks(model)
                        .first()
                        .map_or(self.row_h(), |t| self.base_h(model, t.id));
                    let height = (current * 1.12f32.powf(-steps)).round();
                    cx.emit(Action::SetTrackHeight {
                        track: None,
                        height,
                    });
                    return true;
                }
                if modifiers.ctrl {
                    let steps = if precise { dy / 30.0 } else { dy };
                    self.zoom_at(
                        pos.x.max(self.header_w()),
                        1.18f32.powf(-steps),
                        model,
                        size,
                    );
                } else if let Some(Hit::Header(id, part @ (HeaderPart::Volume | HeaderPart::Pan))) =
                    self.hit_test(pos, size, model)
                {
                    let Some(t) = model.project().track(id) else {
                        return true;
                    };
                    let steps = if precise { dy / 20.0 } else { dy };
                    if part == HeaderPart::Volume {
                        let base = if t.volume_db <= SILENCE_DB {
                            -80.0
                        } else {
                            t.volume_db
                        };
                        let step = if modifiers.fine() { 0.1 } else { 0.5 };
                        cx.emit(Action::Edit(Command::SetTrackVolume {
                            track: id,
                            db: base - steps * step,
                        }));
                    } else if model.project().dual_panned(t) {
                        let step = if modifiers.fine() { 0.01 } else { 0.05 };
                        let by = -steps * step;
                        cx.emit(Action::Edit(Command::SetTrackDualPan {
                            track: id,
                            pan: faderframe_core::DualPan {
                                left: (t.dual_pan.left + by).clamp(-1.0, 1.0),
                                right: (t.dual_pan.right + by).clamp(-1.0, 1.0),
                            },
                        }));
                    } else {
                        let step = if modifiers.fine() { 0.01 } else { 0.05 };
                        cx.emit(Action::Edit(Command::SetTrackPan {
                            track: id,
                            pan: (t.pan - steps * step).clamp(-1.0, 1.0),
                        }));
                    }
                } else if pos.x < self.header_w() {
                    // Over the track headers the wheel turns their controls
                    // and never scrolls (a wheel meant for a knob or a
                    // fader would move the tracks under the pointer).
                } else {
                    let (mut hx, mut vy) = (dx, dy);
                    if modifiers.shift && dx == 0.0 {
                        hx = dy;
                        vy = 0.0;
                    }
                    let unit = if precise { 1.0 } else { 48.0 };
                    self.scroll_x += (hx * unit) as f64;
                    self.scroll_y += vy * unit;
                    self.clamp_scroll(model, size);
                }
                cx.redraw();
                true
            }
            ViewEvent::Key { key, modifiers } => {
                if let Some(handled) = self.edit_key(key, modifiers, size, model, cx) {
                    return handled;
                }
                let action = match key {
                    // Shift: a ripple delete whatever the mode (Ctrl too:
                    // in all tracks) — the range, else the selected clips.
                    Key::Delete | Key::Backspace if modifiers.shift => {
                        use faderframe_session::{RippleOp, RippleScope};
                        let scope = if modifiers.ctrl || modifiers.meta {
                            RippleScope::All
                        } else {
                            model.editor.ripple.scope
                        };
                        Some(Action::Ripple(
                            if model.selection.range.is_some_and(|r| !r.is_empty()) {
                                RippleOp::DeleteRange(scope)
                            } else {
                                RippleOp::DeleteClips(scope)
                            },
                        ))
                    }
                    Key::Delete | Key::Backspace => Some(Action::DeleteInArranger),
                    Key::Char('s') | Key::Char('S') if !modifiers.ctrl => {
                        Some(Action::SplitSelectedAtPlayhead)
                    }
                    Key::Home => Some(Action::Transport(TransportAction::ReturnToStart)),
                    Key::Left => Some(Action::Transport(TransportAction::NudgeBars(-1))),
                    Key::Right => Some(Action::Transport(TransportAction::NudgeBars(1))),
                    Key::Char('+') | Key::Char('=') => {
                        self.zoom_at(
                            self.x_of(model.playhead()).max(self.header_w()),
                            1.25,
                            model,
                            size,
                        );
                        cx.redraw();
                        return true;
                    }
                    Key::Char('-') => {
                        self.zoom_at(
                            self.x_of(model.playhead()).max(self.header_w()),
                            0.8,
                            model,
                            size,
                        );
                        cx.redraw();
                        return true;
                    }
                    _ => None,
                };
                match action {
                    Some(a) => {
                        cx.emit(a);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.is_animating() || matches!(self.drag, Some(Drag::Row { moved: true, .. }))
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        match self.hit_test(pos, size, model)? {
            Hit::Global(g) => self.global_tooltip(g, model),
            Hit::Clip { clip, .. } => {
                let p = model.project();
                let c = p.clip(clip)?;
                let end = c.end(&p.timeline, p.sample_rate);
                Some(format!(
                    "{}\n{} – {}{}\nCtrl+Alt+drag slips the {} inside the clip",
                    c.name,
                    p.timeline.format_bbt(c.start),
                    p.timeline.format_bbt(end),
                    if c.as_midi().is_some() {
                        "\nDouble-click to edit notes"
                    } else {
                        ""
                    },
                    if c.as_midi().is_some() { "notes" } else { "audio" }
                ))
            }
            Hit::Ruler(_) => Some("Click or drag to move the playhead".into()),
            Hit::LoopBand(_) => Some("Drag to set the loop range · Click to toggle looping".into()),
            Hit::LoopStart | Hit::LoopEnd => Some("Drag to resize the loop range".into()),
            Hit::PlayStart => Some(
                if model.project().play_start.is_some_and(|m| m.on) {
                    "Play Start Marker · Drag to move it · Right-click for its menu"
                } else {
                    "Play Start Marker (off) · Drag to move it · Right-click for its menu"
                }
                .into(),
            ),
            Hit::Corner => Some("Grid, snap and follow settings".into()),
            Hit::Arrangement => Some(format!(
                "Showing {}: click to show the song or a scratch pad, make pads and bring one \
                 back to the song · Double-click a pad's name to rename it",
                if model.shown_pad().is_some() {
                    format!("the scratch pad ‘{}’", model.shown_arrangement_name())
                } else {
                    "the song".into()
                }
            )),
            Hit::AddTrack => Some("Add a track".into()),
            Hit::HeaderEdge => Some("Drag to resize the track headers · Double-click to reset".into()),
            Hit::Header(id, part) => {
                let t = model.project().track(id)?;
                Some(match part {
                    HeaderPart::Volume => format!(
                        "Volume {} dB · Drag · Double-click for 0 dB",
                        format_db(model.shown_volume_db(t))
                    ),
                    HeaderPart::Pan if model.project().dual_panned(t) => format!(
                        "Dual pan {} · Drag moves both channels · Double-click: as recorded · \
                         Each channel in the mixer",
                        faderframe_core::pan::format_dual(model.shown_dual_pan(t))
                    ),
                    HeaderPart::Pan if model.project().pans_stereo(t) => format!(
                        "Balance {} · Double-click to centre",
                        format_pan(model.shown_pan(t))
                    ),
                    HeaderPart::Pan => {
                        format!("Pan {} · Double-click to centre", format_pan(model.shown_pan(t)))
                    }
                    HeaderPart::Mute => "Mute".into(),
                    HeaderPart::Solo => "Solo".into(),
                    HeaderPart::Record => "Record arm".into(),
                    HeaderPart::Monitor => "Input monitoring".into(),
                    HeaderPart::Name => "Double-click to rename".into(),
                    HeaderPart::Fold => if model.folder_open(id) {
                        "Close the folder (hide its tracks)"
                    } else {
                        "Open the folder (show its tracks)"
                    }
                    .into(),
                    HeaderPart::Automation => "Show / hide automation lanes".into(),
                    HeaderPart::Group => group_tip(model, t),
                    HeaderPart::Color => {
                        "Track colour · Click to choose (the selected tracks follow)".into()
                    }
                    HeaderPart::Resize => {
                        "Drag to change the track height (Alt: every track) · Double-click to reset · Alt+wheel: all tracks"
                            .into()
                    }
                    _ => return None,
                })
            }
            Hit::Lane { track, .. } => {
                let t = model.project().track(track)?;
                matches!(t.kind, TrackKind::Instrument | TrackKind::Midi)
                    .then(|| "Double-click to create a MIDI clip".into())
            }
            Hit::TakeToggle(clip) => Some(if model.takes_open(clip) {
                "Hide the take lanes".into()
            } else {
                "Show the take lanes to comp".into()
            }),
            Hit::TakeLane { clip, take, .. } => {
                let f = model.project().clip(clip)?.as_takes()?;
                Some(format!(
                    "{}\nClick: this take in the comp section here · Drag: comp the swiped range · Right-click: use the whole take, more",
                    f.takes.get(take)?.name
                ))
            }
            Hit::TakeAudition { clip, take } => {
                let f = model.project().clip(clip)?.as_takes()?;
                let name = &f.takes.get(take)?.name;
                Some(if model.auditioned_take() == Some((clip, take)) {
                    format!("Hearing “{name}” instead of the comp · Click: the comp again")
                } else {
                    format!("Hear “{name}” whole instead of the comp")
                })
            }
            Hit::TakeLanesEdge(_) => {
                Some("Drag to size the take lanes · Double-click: the default height".into())
            }
            Hit::Automation { header: true, .. } => {
                Some("Click the name to choose parameters · the mode button sets Off/Read/Touch/Latch/Write".into())
            }
            Hit::Automation { header: false, .. } => Some(
                "Click: add a point · Drag: move (Alt: no snap) · Double-click: delete · Ctrl+drag: draw · Right-click: shapes"
                    .into(),
            ),
            Hit::Empty(_) => None,
        }
    }

    fn min_size(&self) -> Size {
        Size::new(self.header_w() + 200.0, self.ruler_h() + self.row_h())
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        Some(match axis {
            ScrollAxis::Horizontal => ScrollInfo {
                content: self.content_quarters(model) as f32 * self.ppq,
                viewport: (size.w - self.header_w()).max(0.0),
                offset: self.scroll_x as f32,
                start: self.header_w(),
                end: 0.0,
            },
            ScrollAxis::Vertical => ScrollInfo {
                content: if self.rows.len() > 1 {
                    self.total_rows_height() + self.row_h()
                } else {
                    Self::lane_tracks(model).len() as f32 * self.row_h() + self.row_h()
                },
                viewport: (size.h - self.ruler_h()).max(0.0),
                offset: self.scroll_y,
                start: self.ruler_h(),
                end: 0.0,
            },
        })
    }

    fn drag_payload(&self, model: &Session) -> Option<String> {
        match &self.edit_drag {
            Some(clip_edit::EditDrag::Move {
                clips, moved: true, ..
            }) if model.editor.edit_mode != faderframe_session::editing::EditMode::Shuffle => {
                Some(faderframe_session::launcher::clips_payload(clips))
            }
            _ => None,
        }
    }

    fn cancel_drag(&mut self, cx: &mut EventCx<'_, Action>) {
        if self.snap_guide.take().is_some() {
            cx.redraw();
        }
        if let Some(
            clip_edit::EditDrag::Move { moved: true, .. }
            | clip_edit::EditDrag::Separate {
                separated: true, ..
            },
        ) = self.edit_drag.take()
        {
            cx.emit(Action::CancelGesture);
        }
    }

    fn hover_payload(
        &mut self,
        payload: Option<(&str, Point)>,
        size: Size,
        model: &Session,
    ) -> bool {
        self.update_header_width(model);
        self.update_rows(model);
        let next = payload
            .filter(|(p, _)| launcher_payload(p, model).is_some())
            .and_then(|(_, at)| self.clip_drop_target(at, size, model))
            .map(|(t, at)| (Some(t), at));
        let changed = next != self.drop_at;
        self.drop_at = next;
        changed
    }

    fn drop_payload(
        &mut self,
        payload: &str,
        pos: Point,
        size: Size,
        model: &Session,
    ) -> Option<Action> {
        self.drop_at = None;
        let clips = launcher_payload(payload, model)?;
        let (track, at) = self.clip_drop_target(pos, size, model)?;
        Some(Action::Launcher(
            faderframe_session::launcher::LauncherOp::ToArrangement { clips, track, at },
        ))
    }

    fn drag_files(&mut self, pos: Option<Point>, size: Size, model: &Session) -> bool {
        self.update_header_width(model);
        self.update_rows(model);
        let next = pos.and_then(|p| self.drop_target(p, size, model));
        self.drop_at = next;
        next.is_some()
    }

    fn drop_files(
        &mut self,
        files: &[std::path::PathBuf],
        pos: Point,
        size: Size,
        model: &Session,
    ) -> Option<Action> {
        self.drop_at = None;
        let (track, at) = self.drop_target(pos, size, model)?;
        Some(Action::ImportFiles {
            files: files.to_vec(),
            track,
            at,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        match axis {
            ScrollAxis::Horizontal => self.scroll_x = offset.max(0.0) as f64,
            ScrollAxis::Vertical => self.scroll_y = offset.max(0.0),
        }
    }

    fn scroll_held(&mut self, _axis: ScrollAxis, held: bool) {
        self.bar_held = held;
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod snap_tests;

#[cfg(test)]
mod audit_tests;

#[cfg(test)]
mod scratch_tests;

#[cfg(test)]
mod ripple_tests;

#[cfg(test)]
mod time_lock_tests;

/// The launcher clips a payload carries (arrangement clips move inside the
/// arranger's own drag).
fn launcher_payload(payload: &str, model: &Session) -> Option<Vec<ClipId>> {
    let clips = faderframe_session::launcher::parse_clips_payload(payload)?;
    (!clips.is_empty() && clips.iter().all(|c| model.project().is_launcher_clip(*c)))
        .then_some(clips)
}
