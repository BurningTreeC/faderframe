//! The arranger: a virtualised viewport onto the project timeline.
//!
//! Only rows and clips intersecting the viewport are visited; waveforms are
//! drawn as one filled path per clip channel from the multi-resolution peak
//! cache, so thousands of clips and millions of peaks stay cheap. Track
//! headers reuse the console controls so the arranger and the mixer share a
//! visual language.

#![forbid(unsafe_code)]

mod automation;
mod clip_edit;
pub mod edit_bar;
mod global;
mod harmony;
mod header;

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
    GridDivision, GridLineKind, MusicalTime, for_each_grid_line, snap_floor,
};
use faderframe_ui_canvas::controls::{self, KnobLook, MeterLevel};
use faderframe_ui_canvas::{
    Align, CanvasView, Color, Cursor, EventCx, FontFamily, HostRequest, Key, MenuItem, Modifiers,
    Paint, Painter, Path, Point, PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle,
    Theme, ViewEvent,
};

const MIN_PPQ: f32 = 1.5;
/// Deep enough to see single samples (≈16 px per sample at 120 BPM and
/// 48 kHz) for redrawing them with the Pencil.
const MAX_PPQ: f32 = 400_000.0;
const LOOP_BAND: f32 = 11.0;
const DRAG_THRESHOLD: f32 = 3.0;
/// Height of one take lane under an open take folder.
const LANE_H: f32 = 30.0;
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
    Body,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Corner,
    /// The markers, arranger, signature and tempo lanes.
    Global(GlobalHit),
    LoopBand(MusicalTime),
    LoopStart,
    LoopEnd,
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
    Empty(MusicalTime),
    /// The "+" under the last track header.
    AddTrack,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
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
}

/// Swipe comping in progress (kept outside `Drag`, which is `Copy`).
struct CompDrag {
    clip: ClipId,
    take: usize,
    anchor: i64,
    origin: Point,
    moved: bool,
    /// The folder before the swipe; every move recomputes from it.
    base: TakeFolder,
}

/// Audio of one source placed on the timeline, for waveform drawing.
struct WaveSpan {
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
    scroll_y: f32,
    drag: Option<Drag>,
    hover: Option<Hit>,
    law: FaderLaw,
    /// Files being dragged in: target track (`None` = new tracks) and time.
    drop_at: Option<(Option<TrackId>, MusicalTime)>,
    comp: Option<CompDrag>,
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
}

/// A folder header's triangle.
fn fold_rect(l: &HeaderLayout) -> Rect {
    Rect::new(l.name.x - 2.0, l.name.y, 16.0, l.name.h)
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

/// The entries making a sample of a track's selection (audio tracks).
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
            ppq: 34.0,
            scroll_x: 0.0,
            bar_held: false,
            scroll_y: 0.0,
            drag: None,
            hover: None,
            law: FaderLaw::console(),
            drop_at: None,
            comp: None,
            auto_drag: None,
            header_width,
            rows: Vec::new(),
            edit_drag: None,
            zone_hover: None,
            zoom_seen: 0,
            view_w: f32::MAX,
            lanes: Default::default(),
            global_drag: None,
        }
    }

    // --- coordinates -------------------------------------------------------------

    fn header_w(&self) -> f32 {
        self.header_width
    }

    /// Pick up the saved header width (called before painting/events).
    fn update_header_width(&mut self, model: &Session) {
        self.lanes = model.editor.lanes;
        self.header_width = model
            .header_width()
            .unwrap_or(self.theme.arranger.header_width);
    }

    /// The ruler and the global lanes under it.
    fn ruler_h(&self) -> f32 {
        self.base_ruler_h() + Self::global_lanes_h(&self.lanes)
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
            // Folders are compact unless sized.
            let folder = model
                .project()
                .track(track)
                .is_some_and(|t| t.kind == TrackKind::Folder);
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

    fn update_rows(&mut self, model: &Session) {
        self.rows.clear();
        let mut y = 0.0;
        self.rows.push(y);
        for t in Self::lane_tracks(model) {
            y += self.base_h(model, t.id)
                + Self::open_lanes(t, model) as f32 * LANE_H
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

    /// Grid shown at the current zoom and how many bars per ruler label.
    fn display_grid(&self) -> (GridDivision, i32) {
        let grid = if self.ppq >= 140.0 {
            GridDivision::Note(16)
        } else if self.ppq >= 60.0 {
            GridDivision::Note(8)
        } else if self.ppq >= 16.0 {
            GridDivision::Beat
        } else {
            GridDivision::Bar
        };
        let bar_px = self.ppq * 4.0;
        let mut every = 1;
        while (every as f32) * bar_px < 44.0 && every < 256 {
            every *= 2;
        }
        (grid, every)
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
                Hit::Corner
            } else if pos.y < LOOP_BAND {
                if let Some(range) = model.project().loop_range {
                    let left = (pos.x - self.x_of(range.start)).abs();
                    let right = (pos.x - self.x_of(range.end)).abs();
                    if left.min(right) <= 6.0 {
                        return Some(if left <= right {
                            Hit::LoopStart
                        } else {
                            Hit::LoopEnd
                        });
                    }
                }
                Hit::LoopBand(at)
            } else {
                Hit::Ruler(at)
            });
        }
        if let Some(track) = self.resize_hit(model, pos, size) {
            return Some(Hit::Header(track, HeaderPart::Resize));
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
            if t.kind == TrackKind::Folder {
                let part = [
                    (fold_rect(&l), HeaderPart::Fold),
                    (l.mute, HeaderPart::Mute),
                    (l.solo, HeaderPart::Solo),
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
                let k = ((pos.y - lanes_top) / LANE_H).floor();
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
        let meter = &model.project().timeline.meter;
        let (grid, every) = self.display_grid();
        let start = self.time_at(lanes.x).max(MusicalTime::ZERO);
        let end = self.time_at(lanes.right());
        for_each_grid_line(start, end, grid, meter, |t, kind| {
            let x = self.x_of(t);
            let color = match kind {
                GridLineKind::Bar => {
                    if meter.bar_at(t) % every.max(1) != 0 && self.ppq * 4.0 < 10.0 {
                        return;
                    }
                    a.bar_line
                }
                GridLineKind::Beat => a.beat_line,
                GridLineKind::Subdivision => a.sub_line,
            };
            p.vline(x, lanes.y, lanes.bottom(), color);
        });
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
        self.fill_wave(
            p,
            area,
            vis,
            span.start,
            len,
            channels,
            span.gain,
            model,
            color,
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
            p.fill_path(&path, fill);
            p.hline(x0, x1, mid, color.lighten(0.3).with_alpha(0.25));
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
        lanes_top: f32,
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
        for (k, take) in f.takes.iter().enumerate() {
            let lane = Rect::new(
                clip_rect.x,
                lanes_top + k as f32 * LANE_H,
                clip_rect.w,
                LANE_H - 1.0,
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
            // Label on a dark chip so it stays readable over comped parts.
            let label_text = format!("{} · {}", k + 1, take.name);
            let w = (p.text_width(&label_text, &text) + 10.0).min(vis.1 - vis.0 - 4.0);
            if w > 16.0 {
                let chip = Rect::new(vis.0 + 3.0, lane.y + 3.0, w, 14.0);
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
        p.shadow(rect, r, Color::rgba(0.0, 0.0, 0.0, 0.45), 0.0, 1.5, 4.0);
        p.fill_rounded(
            rect,
            r,
            &Paint::vertical(
                rect,
                color.darken(0.52).with_alpha(0.96),
                color.darken(0.66).with_alpha(0.96),
            ),
        );
        let header_h = a.clip_header.min(rect.h * 0.5);
        p.push_clip(rect.intersection(&lanes));
        let header = Rect::new(rect.x, rect.y, rect.w, header_h);
        p.fill_rounded(
            Rect::new(rect.x, rect.y, rect.w, header_h + r),
            r,
            &Paint::vertical(header, color.lighten(0.08), color.darken(0.05)),
        );
        p.fill(
            Rect::new(rect.x, rect.y + header_h, rect.w, r),
            color.darken(0.55),
        );
        let vis = (rect.x.max(lanes.x), rect.right().min(lanes.right()));
        let text_color = if color.luminance() > 0.42 {
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
                self.paint_redraw(p, model, clip.id, content);
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
        } else {
            p.stroke_rounded(rect, r, 1.0, color.darken(0.25).with_alpha(0.8));
        }
    }

    /// A folder's header: its triangle, name, what it holds, mute and solo.
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
        p.text(
            &t.name,
            folder_name_rect(l),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        if let Some(info) = l.info {
            let project = model.project();
            let n = model
                .folder_contents(t.id)
                .iter()
                .filter(|id| {
                    project
                        .track(**id)
                        .is_some_and(|t| t.kind != TrackKind::Folder)
                })
                .count();
            let text = format!("Folder · {n} track{}", if n == 1 { "" } else { "s" });
            p.text(
                &text,
                Rect::new(folder_name_rect(l).x, info.y, info.w - 16.0, info.h),
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim).family(FontFamily::Condensed),
            );
        }
        controls::led_button(p, l.mute, "M", model.shown_mute(t), c.led.mute, th);
        controls::led_button(p, l.solo, "S", t.solo, c.led.solo, th);
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

        if t.kind == TrackKind::Folder {
            self.paint_folder_header(p, l, t, model);
            return;
        }
        p.text(
            &t.name,
            l.name,
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
            let text = if t.kind == TrackKind::Vca {
                let n = model.project().vca_members(t.id).len();
                format!(
                    "VCA · {n} track{}{group}{vca}",
                    if n == 1 { "" } else { "s" }
                )
            } else if t.kind == TrackKind::Midi {
                // Notes only: no channel layout.
                format!("{}{plug}{out}{group}{vca}", t.kind.label())
            } else {
                format!(
                    "{} · {}{input}{plug}{out}{group}{vca}",
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
            controls::knob(
                p,
                pan,
                (model.shown_pan(t) + 1.0) * 0.5,
                true,
                KnobLook {
                    cap: c.pan_cap,
                    ring: c.panel_label,
                },
                th,
            );
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
            p.vline(ux, v.y, v.bottom(), Color::rgba(1.0, 1.0, 1.0, 0.25));
            let cap = Rect::new(slot.x + slot.w * pos - 4.0, v.y - 1.0, 8.0, v.h + 2.0);
            p.shadow(cap, 2.0, Color::rgba(0.0, 0.0, 0.0, 0.6), 0.0, 1.0, 2.0);
            p.fill_rounded(
                cap,
                2.0,
                &Paint::vertical(cap, c.fader.cap_top, c.fader.cap_bottom),
            );
            p.text(
                &format_db(model.shown_volume_db(t)),
                l.volume_text,
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim)
                    .family(FontFamily::Mono)
                    .right(),
            );
            let m = model.meter(t.id);
            let lv = |ch: &faderframe_session::MeterChannel| MeterLevel {
                level_db: ch.level_db,
                hold_db: ch.hold_db,
                clipped: ch.clipped,
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
        let (grid, every) = self.display_grid();
        let start = self.time_at(ruler.x).max(MusicalTime::ZERO);
        let end = self.time_at(ruler.right());
        let label = TextStyle::new(th.fonts.small, a.ruler_text).family(FontFamily::Mono);
        for_each_grid_line(start, end, grid, meter, |t, kind| {
            let x = self.x_of(t);
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
            }
        });
        // Playhead marker.
        let x = self.x_of(model.playhead());
        let mut tri = Path::new();
        tri.move_to(Point::new(x - 6.0, band.bottom()))
            .line_to(Point::new(x + 6.0, band.bottom()))
            .line_to(Point::new(x, band.bottom() + 8.0))
            .close();
        p.fill_path(&tri, a.playhead);
        p.pop_clip();
        p.hline(
            ruler.x,
            ruler.right(),
            ruler.bottom() - 1.0,
            a.header_border,
        );
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
        let text = format!(
            "GRID {}  ·  SNAP {}  ·  FOLLOW {}",
            ed.grid.label().to_uppercase(),
            if ed.snap { "ON" } else { "OFF" },
            if ed.follow_playhead { "ON" } else { "OFF" }
        );
        controls::engraved(p, &text, corner.inset_xy(10.0, 0.0), th, Align::Start);
        p.hline(
            corner.x,
            corner.right(),
            corner.bottom() - 1.0,
            th.arranger.header_border,
        );
    }

    fn follow(&mut self, model: &Session, size: Size) {
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

    // --- interaction ----------------------------------------------------------------

    fn snap(&self, t: MusicalTime, model: &Session, mods: Modifiers) -> MusicalTime {
        if mods.alt {
            t
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
        items.push(
            MenuItem::new(
                "Remove Track",
                Action::Edit(Command::RemoveTrack { track: t.id }),
            )
            .separated(),
        );
        items.push(
            MenuItem::new(
                "Colour…",
                Action::PickColor(faderframe_session::ColorTarget::Track(t.id)),
            )
            .separated(),
        );
        // Groups and VCAs.
        items.extend(model.group_menu(t.id).into_iter().map(|e| {
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
        }));
        // A sample of the selection (audio tracks).
        items.extend(sample_items(model, t.id, None));
        // Folders: what this one holds; into and out of folders.
        if t.kind == TrackKind::Folder {
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
            items.push(MenuItem::new("Sum into a New Bus", Action::SumFolder(t.id)));
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
                    format!("Show {} Editor", slot.plugin.name),
                    Action::OpenPluginEditor {
                        track: t.id,
                        plugin: slot.id,
                        generic: false,
                    },
                ));
                if slot.plugin.format != faderframe_project::PluginFormat::Builtin {
                    items.push(MenuItem::new(
                        if model.plugin_failed(slot.id) {
                            format!("Reload {} (it stopped working)", slot.plugin.name)
                        } else {
                            format!("Reload {}", slot.plugin.name)
                        },
                        Action::ReloadPlugin(slot.id),
                    ));
                }
            }
        }
        // Editors of the inserts.
        for slot in &t.inserts {
            items.push(MenuItem::new(
                format!("Show {} Editor", slot.plugin.name),
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
        // MIDI learn for the strip controls.
        if t.kind != TrackKind::Midi {
            for (i, (target, name)) in [
                (
                    faderframe_automation::AutomationTarget::TrackVolume,
                    "Volume",
                ),
                (faderframe_automation::AutomationTarget::TrackPan, "Pan"),
                (faderframe_automation::AutomationTarget::TrackMute, "Mute"),
            ]
            .into_iter()
            .enumerate()
            {
                let target = faderframe_project::MappingTarget::Parameter {
                    track: t.id,
                    target,
                };
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
        // Track presets.
        if t.kind != TrackKind::Master {
            items.push(
                MenuItem::new(
                    "Save as Track Preset",
                    Action::SaveTrackPreset { track: t.id },
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

    fn clip_menu(model: &Session, clip: &Clip, at: Point) -> HostRequest<Action> {
        // Edits apply to the whole selection when the clip is part of it.
        let selected = model.selection.clips.contains(&clip.id);
        let targets: Vec<ClipId> = if selected {
            model.selection.clips.iter().copied().collect()
        } else {
            vec![clip.id]
        };
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
            use faderframe_session::detect::FromClip;
            let from = |what| Action::FromClip {
                clip: clip.id,
                what,
            };
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
                items.push(
                    MenuItem::new("Set Project Tempo from Clip", from(FromClip::SetTempo))
                        .separated(),
                );
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
                if selected {
                    Action::DeleteSelection
                } else {
                    Action::Edit(Command::RemoveClip { clip: clip.id })
                },
            )
            .separated(),
        );
        HostRequest::ContextMenu { at, items }
    }

    fn take_menu(clip: &Clip, take: usize, at: Point) -> HostRequest<Action> {
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
                    HeaderPart::Body if clicks >= 2 && t.kind == TrackKind::Folder => {
                        cx.emit(Action::ToggleFolder(id));
                    }
                    HeaderPart::Name if clicks >= 2 => {
                        if let Some(l) = self.header_layout(model, id, size) {
                            let at = if t.kind == TrackKind::Folder {
                                folder_name_rect(&l)
                            } else {
                                l.name
                            };
                            cx.request(Self::rename_request(t, at));
                        }
                    }
                    _ => {
                        let mode = if mods.toggle() {
                            SelectMode::Toggle
                        } else {
                            SelectMode::Replace
                        };
                        cx.emit(Action::SelectTracks {
                            tracks: vec![id],
                            mode,
                        });
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
                if clicks >= 2 && matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
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
                return self.press_lane(track, at, pos, mods, model, cx);
            }
            Hit::Empty(_) if model.editor.tool == faderframe_session::EditTool::Zoom => {
                return self.press_zoom(pos, mods, cx);
            }
            Hit::Empty(_) => {
                cx.emit(Action::ClearSelection);
                cx.emit(Action::SetEditRange(None));
            }
        }
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
        if self.global_drag_move(pos, mods, size, model, cx) {
            return;
        }
        if self.auto_drag_move(model, pos, mods, size.w, cx) {
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
                let t = self.snap(self.time_at(pos.x).max(MusicalTime::ZERO), model, mods);
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
            Some(Drag::LoopEdge {
                start,
                range,
                offset,
            }) => {
                let at = self.snap(
                    (self.time_at(pos.x) - offset).max(MusicalTime::ZERO),
                    model,
                    mods,
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
                // A click uses the whole take.
                let mut f = comp.base;
                f.use_take(comp.take);
                cx.emit(Action::Edit(Command::SetClipContent {
                    clip: comp.clip,
                    start: c.start,
                    content: Box::new(ClipContent::Takes(f)),
                }));
            }
            cx.set_cursor(Cursor::Default);
            return;
        }
        match self.drag.take() {
            Some(
                Drag::Volume { .. }
                | Drag::Pan { .. }
                | Drag::LoopEdge { .. }
                | Drag::Loop { moved: true, .. },
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
            if t.kind == TrackKind::Folder {
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
                    self.paint_take_lanes(p, rect, top, lanes, clip, f, model, color);
                }
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
            p.fill(Rect::new(px - 0.75, lanes.y, 1.5, lanes.h), a.playhead);
        }
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
        self.paint_global_lanes(p, size, model);
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
                        Hit::Corner
                        | Hit::Ruler(_)
                        | Hit::LoopBand(_)
                        | Hit::LoopStart
                        | Hit::LoopEnd,
                    ) => {
                        cx.request(Self::grid_menu(model, pos));
                    }
                    Some(Hit::Global(g)) => cx.request(self.global_menu(g, model, pos)),
                    Some(Hit::TakeLane { clip, take, .. }) => {
                        if let Some(c) = model.project().clip(clip) {
                            cx.request(Self::take_menu(c, take, pos));
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
                        matches!(h, Some(Hit::AddTrack | Hit::Header(_, HeaderPart::Resize)))
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
                        Some(Hit::Header(..)) | Some(Hit::Corner) => Cursor::Pointer,
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
                    Key::Delete | Key::Backspace => Some(Action::DeleteSelection),
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
        model.is_animating()
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        match self.hit_test(pos, size, model)? {
            Hit::Global(g) => self.global_tooltip(g, model),
            Hit::Clip { clip, .. } => {
                let p = model.project();
                let c = p.clip(clip)?;
                let end = c.end(&p.timeline, p.sample_rate);
                Some(format!(
                    "{}\n{} – {}{}",
                    c.name,
                    p.timeline.format_bbt(c.start),
                    p.timeline.format_bbt(end),
                    if c.as_midi().is_some() {
                        "\nDouble-click to edit notes"
                    } else {
                        ""
                    }
                ))
            }
            Hit::Ruler(_) => Some("Click or drag to move the playhead".into()),
            Hit::LoopBand(_) => Some("Drag to set the loop range · Click to toggle looping".into()),
            Hit::LoopStart | Hit::LoopEnd => Some("Drag to resize the loop range".into()),
            Hit::Corner => Some("Grid, snap and follow settings".into()),
            Hit::AddTrack => Some("Add a track".into()),
            Hit::HeaderEdge => Some("Drag to resize the track headers · Double-click to reset".into()),
            Hit::Header(id, part) => {
                let t = model.project().track(id)?;
                Some(match part {
                    HeaderPart::Volume => format!(
                        "Volume {} dB · Drag · Double-click for 0 dB",
                        format_db(model.shown_volume_db(t))
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
                    "{}\nClick: use this take · Drag: comp this section · Right-click: more",
                    f.takes.get(take)?.name
                ))
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
