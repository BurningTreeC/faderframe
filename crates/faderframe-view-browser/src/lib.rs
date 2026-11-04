//! The media browser view: places (the project's media, its Samples
//! folder, recent files, home, folders added), a search field and filter
//! chips (type, length, key, tempo, tag, rating), the files with what they
//! are (length, tempo, key, a waveform, stars and tags), and at the bottom
//! the preview: play/stop, in the project's tempo and key, looped, heard on
//! selection, at its own volume.
//!
//! A click selects (and previews, with Auto on), a double click opens a
//! folder or hears a file, a drag takes the selected files to the arranger,
//! a launcher slot or a sampler (as a [`faderframe_session::browser::PAYLOAD`]
//! the views there take; elsewhere they are plain files), the menu finds
//! similar sounds, tags, rates and places them. Typing while the view has
//! the keyboard searches; the arrows move the selection, Enter hears it.

#![forbid(unsafe_code)]

use faderframe_project::harmony::Key;
use faderframe_session::browser::{
    self, BrowserFilter, BrowserOp, BrowserRow, KeyFilter, KindFilter, LengthFilter, Place,
    PreviewSettings, RowItem, SortBy, TempoFilter,
};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::access::{AccessNode, AccessRole, access_id_str};
use faderframe_ui_canvas::{
    CanvasView, Cursor, EventCx, HostRequest, Key as KeyCode, MenuItem, Paint, Painter, Point,
    PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};
use std::path::{Path, PathBuf};

const PAD: f32 = 6.0;
const HEADER_H: f32 = 30.0;
const SEARCH_H: f32 = 28.0;
const CHIP_H: f32 = 22.0;
/// Two rows of three chips.
const CHIPS_H: f32 = 2.0 * CHIP_H + 6.0;
const BANNER_H: f32 = 22.0;
const COLUMNS_H: f32 = 20.0;
pub const ROW_H: f32 = 34.0;
const STATUS_H: f32 = 18.0;
const BAR_H: f32 = 60.0;
/// Fixed columns (from the right): stars, key, tempo, length.
const STARS_W: f32 = 58.0;
const KEY_W: f32 = 40.0;
const BPM_W: f32 = 38.0;
const LENGTH_W: f32 = 50.0;
/// Pixels a press moves before it drags files.
const DRAG_START: f32 = 6.0;
/// Rows either side of those shown that screen readers get.
const ACCESS_REACH: usize = 200;
/// Volume slider's range (dB).
const VOLUME_MIN: f32 = -40.0;
const VOLUME_MAX: f32 = 6.0;

/// A filter chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    Kind,
    Length,
    Key,
    Tempo,
    Tag,
    Rating,
}

impl Chip {
    pub const ALL: [Chip; 6] = [
        Chip::Kind,
        Chip::Length,
        Chip::Key,
        Chip::Tempo,
        Chip::Tag,
        Chip::Rating,
    ];
}

/// A press: where, how many clicks, with which keys held.
#[derive(Clone, Copy, Debug)]
struct Click {
    pos: Point,
    clicks: u32,
    mods: faderframe_ui_canvas::Modifiers,
}

impl Click {
    fn new(pos: Point, clicks: u32, mods: faderframe_ui_canvas::Modifiers) -> Self {
        Self { pos, clicks, mods }
    }
}

/// What a press hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Place,
    Up,
    Refresh,
    Search,
    ClearSearch,
    Deep,
    Chip(Chip),
    ClearSimilar,
    Column(SortBy),
    Row(usize),
    /// A row's star `n` (1…5).
    Star(usize, u8),
    Play,
    Sync,
    KeyMatch,
    Loop,
    Auto,
    Volume,
}

pub struct BrowserView {
    theme: Theme,
    scroll: f32,
    hover: Option<Hit>,
    /// A press on file rows: where, and the files it would drag.
    press: Option<(Point, Vec<PathBuf>)>,
    /// Files being dragged out (the host takes them as a payload).
    dragging: Option<Vec<PathBuf>>,
    /// The row a Shift-click reaches from.
    anchor: Option<usize>,
    /// The volume slider held: where it started and its dB then.
    volume: Option<(f32, f32)>,
}

impl Default for BrowserView {
    fn default() -> Self {
        Self::new(Theme::default())
    }
}

fn mmss(seconds: f64) -> String {
    if seconds < 10.0 {
        format!("{seconds:.2} s")
    } else if seconds < 60.0 {
        format!("{seconds:.1} s")
    } else {
        let s = seconds.round() as i64;
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// "Am", "E♭", "F#m" …
pub fn short_key(k: Key) -> String {
    let name = k.name();
    let mut parts = name.split(' ');
    let root = parts.next().unwrap_or("").to_string();
    if k.scale.is_minor() {
        format!("{root}m")
    } else {
        root
    }
}

fn kind_label(k: KindFilter) -> &'static str {
    match k {
        KindFilter::All => "All Files",
        KindFilter::Audio => "Audio",
        KindFilter::Midi => "MIDI",
        KindFilter::Loops => "Loops",
        KindFilter::OneShots => "One-Shots",
    }
}

const TEMPO_RANGES: [(f64, f64); 5] = [
    (60.0, 90.0),
    (90.0, 110.0),
    (110.0, 130.0),
    (130.0, 150.0),
    (150.0, 200.0),
];

impl BrowserView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll: 0.0,
            hover: None,
            press: None,
            dragging: None,
            anchor: None,
            volume: None,
        }
    }

    // --- geometry -----------------------------------------------------------------

    /// The fixed controls above and below the list.
    pub fn buttons(size: Size, model: &Session) -> Vec<(Hit, Rect)> {
        let w = size.w;
        let mut out = Vec::new();
        let y = PAD;
        let h = HEADER_H - PAD - 2.0;
        out.push((Hit::Refresh, Rect::new(w - PAD - 26.0, y, 26.0, h)));
        out.push((Hit::Up, Rect::new(w - PAD - 56.0, y, 26.0, h)));
        out.push((Hit::Place, Rect::new(PAD, y, (w * 0.5 - PAD).max(80.0), h)));
        let sy = HEADER_H + 2.0;
        let sh = SEARCH_H - 6.0;
        let deep_w = 84.0;
        out.push((Hit::Deep, Rect::new(w - PAD - deep_w, sy, deep_w, sh)));
        let field = Rect::new(PAD, sy, (w - 3.0 * PAD - deep_w).max(40.0), sh);
        out.push((
            Hit::ClearSearch,
            Rect::new(field.right() - 20.0, sy, 20.0, sh),
        ));
        out.push((Hit::Search, field));
        let cy = HEADER_H + SEARCH_H;
        let cw = ((w - 2.0 * PAD - 2.0 * 4.0) / 3.0).max(20.0);
        for (i, c) in Chip::ALL.into_iter().enumerate() {
            let (row, col) = (i / 3, i % 3);
            out.push((
                Hit::Chip(c),
                Rect::new(
                    PAD + col as f32 * (cw + 4.0),
                    cy + row as f32 * (CHIP_H + 3.0),
                    cw,
                    CHIP_H,
                ),
            ));
        }
        if model.browser().similar_to().is_some() {
            let by = HEADER_H + SEARCH_H + CHIPS_H;
            out.push((
                Hit::ClearSimilar,
                Rect::new(w - PAD - 22.0, by + 1.0, 22.0, BANNER_H - 2.0),
            ));
        }
        let cols = Self::columns_rect(size, model);
        for (by, r) in Self::column_spans(cols) {
            out.push((Hit::Column(by), r));
        }
        let bar = Self::bar_rect(size);
        let line1 = Rect::new(bar.x + PAD, bar.y + 4.0, 34.0, 24.0);
        out.push((Hit::Play, line1));
        let ty = bar.y + 32.0;
        let mut x = bar.x + PAD;
        for (hit, tw) in [
            (Hit::Sync, 46.0),
            (Hit::KeyMatch, 36.0),
            (Hit::Loop, 40.0),
            (Hit::Auto, 40.0),
        ] {
            out.push((hit, Rect::new(x, ty, tw, 22.0)));
            x += tw + 3.0;
        }
        out.push((
            Hit::Volume,
            Rect::new(x + 4.0, ty, (bar.right() - PAD - x - 4.0).max(30.0), 22.0),
        ));
        out
    }

    fn top(model: &Session) -> f32 {
        HEADER_H
            + SEARCH_H
            + CHIPS_H
            + if model.browser().similar_to().is_some() {
                BANNER_H
            } else {
                0.0
            }
    }

    fn columns_rect(size: Size, model: &Session) -> Rect {
        Rect::new(0.0, Self::top(model), size.w, COLUMNS_H)
    }

    /// The sortable columns' header cells.
    fn column_spans(cols: Rect) -> Vec<(SortBy, Rect)> {
        let r = cols.right() - PAD;
        let wide = cols.w > 330.0;
        let mut out = vec![(
            SortBy::Rating,
            Rect::new(r - STARS_W, cols.y, STARS_W, cols.h),
        )];
        let mut x = r - STARS_W;
        if wide {
            out.push((SortBy::Key, Rect::new(x - KEY_W, cols.y, KEY_W, cols.h)));
            x -= KEY_W;
            out.push((SortBy::Tempo, Rect::new(x - BPM_W, cols.y, BPM_W, cols.h)));
            x -= BPM_W;
        }
        out.push((
            SortBy::Length,
            Rect::new(x - LENGTH_W, cols.y, LENGTH_W, cols.h),
        ));
        x -= LENGTH_W;
        out.push((
            SortBy::Name,
            Rect::new(cols.x + PAD, cols.y, (x - PAD).max(0.0), cols.h),
        ));
        out
    }

    pub fn list_rect(size: Size, model: &Session) -> Rect {
        let y = Self::top(model) + COLUMNS_H;
        Rect::new(0.0, y, size.w, (size.h - y - STATUS_H - BAR_H).max(0.0))
    }

    pub fn bar_rect(size: Size) -> Rect {
        Rect::new(0.0, (size.h - BAR_H).max(0.0), size.w, BAR_H)
    }

    /// Row `i` (in view coordinates, scrolled).
    pub fn row_rect(&self, i: usize, size: Size, model: &Session) -> Rect {
        let l = Self::list_rect(size, model);
        Rect::new(l.x, l.y + i as f32 * ROW_H - self.scroll, l.w, ROW_H)
    }

    fn star_rect(row: Rect, n: u8) -> Rect {
        let x0 = row.right() - PAD - STARS_W + 2.0;
        Rect::new(x0 + f32::from(n - 1) * 11.0, row.y + 4.0, 11.0, 14.0)
    }

    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        if let Some((h, _)) = Self::buttons(size, model)
            .into_iter()
            .find(|(_, r)| r.contains(pos))
        {
            return Some(h);
        }
        let list = Self::list_rect(size, model);
        if !list.contains(pos) {
            return None;
        }
        let i = ((pos.y - list.y + self.scroll) / ROW_H).floor();
        if i < 0.0 {
            return None;
        }
        let i = i as usize;
        let row = model.browser().rows().get(i)?;
        if row.is_file() {
            let rr = self.row_rect(i, size, model);
            for n in 1..=browser::MAX_RATING {
                if Self::star_rect(rr, n).contains(pos) {
                    return Some(Hit::Star(i, n));
                }
            }
        }
        Some(Hit::Row(i))
    }

    fn clamp(&mut self, size: Size, model: &Session) {
        let n = model.browser().rows().len();
        let view = Self::list_rect(size, model).h;
        let max = (n as f32 * ROW_H - view).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Bring row `i` into view.
    fn show_row(&mut self, i: usize, size: Size, model: &Session) {
        let view = Self::list_rect(size, model).h;
        let top = i as f32 * ROW_H;
        if top < self.scroll {
            self.scroll = top;
        } else if top + ROW_H > self.scroll + view {
            self.scroll = top + ROW_H - view;
        }
        self.clamp(size, model);
    }

    // --- labels -------------------------------------------------------------------

    fn chip_label(chip: Chip, model: &Session) -> String {
        let f = model.browser().filter();
        match chip {
            Chip::Kind => kind_label(f.kind).into(),
            Chip::Length => f.length.label().into(),
            Chip::Key => match f.key {
                KeyFilter::Any => "Any Key".into(),
                KeyFilter::Project => "Project's Key".into(),
                KeyFilter::Key(k) => short_key(k),
            },
            Chip::Tempo => match f.tempo {
                TempoFilter::Any => "Any Tempo".into(),
                TempoFilter::Project => "Project's Tempo".into(),
                TempoFilter::Range(a, b) => format!("{a:.0}–{b:.0} BPM"),
            },
            Chip::Tag => f.tag.clone().unwrap_or_else(|| "Any Tag".into()),
            Chip::Rating => match f.rating {
                0 => "Any Rating".into(),
                n => format!("{}+", "★".repeat(usize::from(n))),
            },
        }
    }

    fn chip_set(chip: Chip, f: &BrowserFilter) -> bool {
        match chip {
            Chip::Kind => f.kind != KindFilter::All,
            Chip::Length => f.length != LengthFilter::Any,
            Chip::Key => f.key != KeyFilter::Any,
            Chip::Tempo => f.tempo != TempoFilter::Any,
            Chip::Tag => f.tag.is_some(),
            Chip::Rating => f.rating > 0,
        }
    }

    fn place_label(model: &Session) -> String {
        format!("{} ▾", model.browser().place().label())
    }

    /// The folder looked in, below its place.
    fn where_label(model: &Session) -> String {
        let b = model.browser();
        if let Some(t) = b.similar_to() {
            return format!("Sounds like {}", file_name(t));
        }
        let Some(folder) = b.folder() else {
            return String::new();
        };
        let root = model.browser_place_root(b.place());
        match root.as_deref().and_then(|r| folder.strip_prefix(r).ok()) {
            Some(rel) if rel.as_os_str().is_empty() => String::new(),
            Some(rel) => rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(" › "),
            None => folder.to_string_lossy().to_string(),
        }
    }

    // --- menus --------------------------------------------------------------------

    fn op(op: BrowserOp) -> Action {
        Action::Browser(op)
    }

    fn place_menu(model: &Session) -> Vec<MenuItem<Action>> {
        let b = model.browser();
        let mut items: Vec<MenuItem<Action>> = model
            .browser_places()
            .into_iter()
            .map(|p| {
                let on = *b.place() == p;
                MenuItem::new(p.label(), Self::op(BrowserOp::Open(p))).checked(on)
            })
            .collect();
        if let Place::Folder(dir) = b.place() {
            items.push(
                MenuItem::new(
                    format!("Remove “{}” from Places", file_name(dir)),
                    Self::op(BrowserOp::RemoveFolder(dir.clone())),
                )
                .separated(),
            );
        }
        if let Some(dir) = b.folder() {
            items.push(
                MenuItem::new(
                    "Add This Folder to Places",
                    Self::op(BrowserOp::AddFolder(dir.to_path_buf())),
                )
                .separated(),
            );
            items.push(MenuItem::new(
                "Read and Analyse Everything Here",
                Self::op(BrowserOp::Analyse(Vec::new())),
            ));
        }
        items
    }

    fn chip_menu(chip: Chip, model: &Session) -> Vec<MenuItem<Action>> {
        let f = model.browser().filter().clone();
        let with = |g: &dyn Fn(&mut BrowserFilter)| {
            let mut n = f.clone();
            g(&mut n);
            Self::op(BrowserOp::Filter(n))
        };
        match chip {
            Chip::Kind => [
                KindFilter::All,
                KindFilter::Audio,
                KindFilter::Midi,
                KindFilter::Loops,
                KindFilter::OneShots,
            ]
            .into_iter()
            .map(|k| MenuItem::new(kind_label(k), with(&|n| n.kind = k)).checked(f.kind == k))
            .collect(),
            Chip::Length => LengthFilter::ALL
                .into_iter()
                .map(|l| MenuItem::new(l.label(), with(&|n| n.length = l)).checked(f.length == l))
                .collect(),
            Chip::Key => {
                let mut items = vec![
                    MenuItem::new("Any Key", with(&|n| n.key = KeyFilter::Any))
                        .checked(f.key == KeyFilter::Any),
                    MenuItem::new(
                        "Keys That Fit the Project's",
                        with(&|n| n.key = KeyFilter::Project),
                    )
                    .checked(f.key == KeyFilter::Project),
                ];
                for (title, scale) in [
                    ("Major", faderframe_project::harmony::Scale::Major),
                    ("Minor", faderframe_project::harmony::Scale::Minor),
                ] {
                    let keys = (0..12u8)
                        .map(|root| {
                            let k = Key::new(root, scale);
                            MenuItem::new(k.name(), with(&|n| n.key = KeyFilter::Key(k)))
                                .checked(f.key == KeyFilter::Key(k))
                        })
                        .collect();
                    items.push(MenuItem::submenu(title, keys));
                }
                items
            }
            Chip::Tempo => {
                let mut items = vec![
                    MenuItem::new("Any Tempo", with(&|n| n.tempo = TempoFilter::Any))
                        .checked(f.tempo == TempoFilter::Any),
                    MenuItem::new(
                        format!(
                            "Near the Project's ({:.0} BPM)",
                            model.project().timeline.tempo.bpm_at(model.playhead())
                        ),
                        with(&|n| n.tempo = TempoFilter::Project),
                    )
                    .checked(f.tempo == TempoFilter::Project),
                ];
                for (a, b) in TEMPO_RANGES {
                    items.push(
                        MenuItem::new(
                            format!("{a:.0}–{b:.0} BPM"),
                            with(&|n| n.tempo = TempoFilter::Range(a, b)),
                        )
                        .checked(f.tempo == TempoFilter::Range(a, b)),
                    );
                }
                items
            }
            Chip::Tag => {
                let mut items = vec![
                    MenuItem::new("Any Tag", with(&|n| n.tag = None)).checked(f.tag.is_none()),
                ];
                for t in model.browser().all_tags() {
                    let on = f.tag.as_deref() == Some(t.as_str());
                    let tag = t.clone();
                    items.push(MenuItem::new(t, with(&|n| n.tag = Some(tag.clone()))).checked(on));
                }
                if items.len() == 1 {
                    items.push(MenuItem::disabled(
                        "No tags yet (tag files from their menu)",
                    ));
                }
                items
            }
            Chip::Rating => (0..=browser::MAX_RATING)
                .map(|n| {
                    let label = if n == 0 {
                        "Any Rating".to_string()
                    } else {
                        format!("{} or More", "★".repeat(usize::from(n)))
                    };
                    MenuItem::new(label, with(&|m| m.rating = n)).checked(f.rating == n)
                })
                .collect(),
        }
    }

    /// The files a menu or drag at row `i` is about: the selection when it
    /// holds the row, else the row.
    fn files_at(i: usize, model: &Session) -> Vec<PathBuf> {
        let b = model.browser();
        let Some(row) = b.rows().get(i).filter(|r| r.is_file()) else {
            return Vec::new();
        };
        if b.selected().contains(&row.path) {
            b.selected()
                .iter()
                .filter(|p| b.row(p).is_some_and(BrowserRow::is_file))
                .cloned()
                .collect()
        } else {
            vec![row.path.clone()]
        }
    }

    pub fn row_menu(i: usize, model: &Session) -> Vec<MenuItem<Action>> {
        Self::menu_for(i, Self::files_at(i, model), model)
    }

    /// The menu of row `i` alone (a right-click on a row not selected
    /// selects it: the menu is about it, whatever the selection was).
    fn row_menu_for(i: usize, model: &Session) -> Vec<MenuItem<Action>> {
        let files = model
            .browser()
            .rows()
            .get(i)
            .filter(|r| r.is_file())
            .map(|r| vec![r.path.clone()])
            .unwrap_or_default();
        Self::menu_for(i, files, model)
    }

    fn menu_for(i: usize, files: Vec<PathBuf>, model: &Session) -> Vec<MenuItem<Action>> {
        let b = model.browser();
        let Some(row) = b.rows().get(i) else {
            return Vec::new();
        };
        if !row.is_file() {
            return vec![MenuItem::new(
                "Open",
                Self::op(match row.item {
                    RowItem::Up => BrowserOp::Up,
                    _ => BrowserOp::Enter(row.path.clone()),
                }),
            )];
        }
        let mut items = vec![MenuItem::new(
            "Hear",
            Self::op(BrowserOp::Preview(row.path.clone())),
        )];
        if b.previewing().is_some() {
            items.push(MenuItem::new("Stop", Self::op(BrowserOp::StopPreview)));
        }
        if row.item == RowItem::Audio {
            items.push(MenuItem::new(
                "Find Similar Sounds",
                Self::op(BrowserOp::FindSimilar(row.path.clone())),
            ));
        }
        // On the selected track (else a new one) at the playhead.
        let track = model.selection.tracks.iter().copied().find(|t| {
            model
                .project()
                .track(*t)
                .is_some_and(|t| t.kind == faderframe_project::TrackKind::Audio)
        });
        items.push(
            MenuItem::new(
                if files.len() == 1 {
                    "Place at the Playhead".to_string()
                } else {
                    format!("Place {} Files at the Playhead", files.len())
                },
                Self::op(BrowserOp::Place {
                    files: files.clone(),
                    track,
                    at: model.playhead(),
                }),
            )
            .separated(),
        );
        // Tags.
        let mut tags: Vec<MenuItem<Action>> = model
            .browser()
            .all_tags()
            .into_iter()
            .map(|t| {
                let on = files.iter().all(|f| b.tags_of(f).contains(&t));
                MenuItem::new(
                    t.clone(),
                    Self::op(BrowserOp::Tag {
                        paths: files.clone(),
                        tag: t,
                        on: !on,
                    }),
                )
                .checked(on)
            })
            .collect();
        tags.push(
            MenuItem::new("New Tag…", Self::op(BrowserOp::AskTag(files.clone()))).separated(),
        );
        items.push(MenuItem::submenu("Tags", tags).separated());
        let rating = b.rating(&row.path);
        let stars = (0..=browser::MAX_RATING)
            .map(|n| {
                MenuItem::new(
                    if n == 0 {
                        "No Rating".to_string()
                    } else {
                        "★".repeat(usize::from(n))
                    },
                    Self::op(BrowserOp::Rate {
                        paths: files.clone(),
                        stars: n,
                    }),
                )
                .checked(rating == n)
            })
            .collect();
        items.push(MenuItem::submenu("Rating", stars));
        if let Some(parent) = row.path.parent()
            && b.folder() != Some(parent)
        {
            items.push(
                MenuItem::new(
                    "Show in Its Folder",
                    Self::op(BrowserOp::Enter(parent.to_path_buf())),
                )
                .separated(),
            );
        }
        items.push(MenuItem::new(
            "Read and Analyse Again",
            Self::op(BrowserOp::Analyse(files)),
        ));
        items
    }

    /// Change the preview settings.
    fn setting(model: &Session, f: impl Fn(&mut PreviewSettings)) -> Action {
        let mut s = model.browser().settings();
        f(&mut s);
        Self::op(BrowserOp::Settings(s))
    }

    fn volume_at(x: f32, r: Rect) -> f32 {
        let t = ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0);
        VOLUME_MIN + t * (VOLUME_MAX - VOLUME_MIN)
    }

    // --- events -------------------------------------------------------------------

    fn select(
        &mut self,
        i: usize,
        mods: faderframe_ui_canvas::Modifiers,
        model: &Session,
    ) -> Option<Action> {
        let b = model.browser();
        let row = b.rows().get(i)?;
        if !row.is_file() {
            return None;
        }
        let mut paths: Vec<PathBuf> = b.selected().to_vec();
        if mods.shift
            && let Some(a) = self.anchor
        {
            let (lo, hi) = (a.min(i), a.max(i));
            paths = b.rows()[lo..=hi.min(b.rows().len() - 1)]
                .iter()
                .filter(|r| r.is_file())
                .map(|r| r.path.clone())
                .collect();
        } else if mods.toggle() {
            if let Some(k) = paths.iter().position(|p| *p == row.path) {
                paths.remove(k);
            } else {
                paths.push(row.path.clone());
            }
            self.anchor = Some(i);
        } else {
            paths = vec![row.path.clone()];
            self.anchor = Some(i);
        }
        Some(Self::op(BrowserOp::Select(paths)))
    }

    /// The row of the (first) selected file.
    fn selected_index(model: &Session) -> Option<usize> {
        let b = model.browser();
        let first = b.selected().first()?;
        b.rows().iter().position(|r| r.path == *first)
    }

    fn open_row(i: usize, model: &Session) -> Option<Action> {
        let row = model.browser().rows().get(i)?;
        Some(Self::op(match row.item {
            RowItem::Up => BrowserOp::Up,
            RowItem::Folder => BrowserOp::Enter(row.path.clone()),
            RowItem::Audio | RowItem::Midi => BrowserOp::Preview(row.path.clone()),
        }))
    }

    fn press(
        &mut self,
        hit: Hit,
        click: Click,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Click { pos, clicks, mods } = click;
        let rect = |h: Hit| {
            Self::buttons(size, model)
                .into_iter()
                .find(|(x, _)| *x == h)
                .map_or(Rect::new(pos.x, pos.y, 1.0, 1.0), |(_, r)| r)
        };
        match hit {
            Hit::Place => cx.request(HostRequest::ContextMenu {
                at: Point::new(rect(hit).x, rect(hit).bottom()),
                items: Self::place_menu(model),
            }),
            Hit::Up => cx.emit(Self::op(BrowserOp::Up)),
            Hit::Refresh => cx.emit(Self::op(BrowserOp::Refresh)),
            Hit::Search => cx.request(HostRequest::TextInput {
                at: rect(hit),
                initial: model.browser().search().to_string(),
                commit: Box::new(|t| Some(Action::Browser(BrowserOp::Search(t.to_string())))),
            }),
            Hit::ClearSearch => {
                if model.browser().search().is_empty() {
                    self.press(Hit::Search, click, size, model, cx);
                } else {
                    cx.emit(Self::op(BrowserOp::Search(String::new())));
                }
            }
            Hit::Deep => cx.emit(Self::op(BrowserOp::Deep(!model.browser().deep()))),
            Hit::Chip(c) => cx.request(HostRequest::ContextMenu {
                at: Point::new(rect(hit).x, rect(hit).bottom()),
                items: Self::chip_menu(c, model),
            }),
            Hit::ClearSimilar => cx.emit(Self::op(BrowserOp::ClearSimilar)),
            Hit::Column(by) => cx.emit(Self::op(BrowserOp::Sort(by))),
            Hit::Star(i, n) => {
                if let Some(row) = model.browser().rows().get(i) {
                    let stars = if row.rating == n { 0 } else { n };
                    cx.emit(Self::op(BrowserOp::Rate {
                        paths: vec![row.path.clone()],
                        stars,
                    }));
                }
            }
            Hit::Row(i) => {
                if clicks >= 2 {
                    if let Some(a) = Self::open_row(i, model) {
                        cx.emit(a);
                    }
                    return;
                }
                if let Some(a) = self.select(i, mods, model) {
                    cx.emit(a);
                }
                let files = Self::files_at(i, model);
                let files = if mods.shift || mods.toggle() || files.is_empty() {
                    files
                } else {
                    // A plain click drags what it selects.
                    model
                        .browser()
                        .rows()
                        .get(i)
                        .map(|r| vec![r.path.clone()])
                        .unwrap_or_default()
                };
                if !files.is_empty() {
                    self.press = Some((pos, files));
                }
            }
            Hit::Play => {
                if model.browser().previewing().is_some() {
                    cx.emit(Self::op(BrowserOp::StopPreview));
                } else if let Some(p) = model.browser().selected().first() {
                    cx.emit(Self::op(BrowserOp::Preview(p.clone())));
                }
            }
            Hit::Sync => cx.emit(Self::setting(model, |s| s.sync = !s.sync)),
            Hit::KeyMatch => cx.emit(Self::setting(model, |s| s.key = !s.key)),
            Hit::Loop => cx.emit(Self::setting(model, |s| s.looped = !s.looped)),
            Hit::Auto => cx.emit(Self::setting(model, |s| s.auto = !s.auto)),
            Hit::Volume => {
                let r = rect(hit);
                if clicks >= 2 {
                    cx.emit(Self::setting(model, |s| s.volume_db = 0.0));
                    return;
                }
                let db = Self::volume_at(pos.x, r);
                self.volume = Some((pos.x, db));
                cx.emit(Self::setting(model, |s| s.volume_db = db));
            }
        }
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.to_string_lossy().to_string(),
        |n| n.to_string_lossy().to_string(),
    )
}

impl CanvasView<Session, Action> for BrowserView {
    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.theme = theme.clone();
        let th = theme;
        self.clamp(size, model);
        p.fill(Rect::new(0.0, 0.0, size.w, size.h), th.ui.background);
        let small = TextStyle::new(th.fonts.small, th.ui.text);
        let dim = TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim);
        let faint = TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_faint);
        let b = model.browser();
        let settings = b.settings();
        // Buttons and fields.
        for (hit, r) in Self::buttons(size, model) {
            let hover = self.hover == Some(hit);
            let lit = match hit {
                Hit::Deep => b.deep(),
                Hit::Chip(c) => Self::chip_set(c, b.filter()),
                Hit::Sync => settings.sync,
                Hit::KeyMatch => settings.key,
                Hit::Loop => settings.looped,
                Hit::Auto => settings.auto,
                Hit::Play => b.previewing().is_some(),
                _ => false,
            };
            match hit {
                Hit::Column(by) => {
                    let (sort, back) = b.sort();
                    let label = match by {
                        SortBy::Name => "Name",
                        SortBy::Kind => "Kind",
                        SortBy::Length => "Length",
                        SortBy::Tempo => "BPM",
                        SortBy::Key => "Key",
                        SortBy::Rating => "Rating",
                    };
                    let text = if sort == by {
                        format!("{label} {}", if back { "▾" } else { "▴" })
                    } else {
                        label.to_string()
                    };
                    let st = if sort == by { dim.bold() } else { dim };
                    let st = if by == SortBy::Name { st } else { st.right() };
                    p.text(&text, r, &st);
                }
                Hit::ClearSearch => {
                    if !b.search().is_empty() {
                        p.text("×", r, &small.center());
                    }
                }
                Hit::Search => {
                    p.fill_rounded(r, 4.0, &Paint::Solid(th.ui.surface));
                    p.stroke_rounded(r, 4.0, 1.0, if hover { th.ui.accent } else { th.ui.border });
                    let inner = Rect::new(r.x + 8.0, r.y, (r.w - 30.0).max(0.0), r.h);
                    if b.search().is_empty() {
                        p.text("Search (type to search)", inner, &faint);
                    } else {
                        p.text(b.search(), inner, &small);
                    }
                }
                Hit::Volume => {
                    let db = settings.volume_db.clamp(VOLUME_MIN, VOLUME_MAX);
                    let t = (db - VOLUME_MIN) / (VOLUME_MAX - VOLUME_MIN);
                    let track = Rect::new(r.x, r.y + r.h / 2.0 - 2.0, r.w, 4.0);
                    p.fill_rounded(track, 2.0, &Paint::Solid(th.ui.surface_alt));
                    p.fill_rounded(
                        Rect::new(track.x, track.y, track.w * t, track.h),
                        2.0,
                        &Paint::Solid(th.ui.accent.with_alpha(0.7)),
                    );
                    let x = r.x + r.w * t;
                    p.fill_rounded(
                        Rect::new(x - 3.0, r.y + 4.0, 6.0, r.h - 8.0),
                        2.0,
                        &Paint::Solid(th.ui.text),
                    );
                    p.text(
                        &format!("{db:+.0} dB"),
                        Rect::new(r.x, r.y - 1.0, r.w, 10.0),
                        &faint.right(),
                    );
                }
                _ => {
                    let fill = if lit {
                        th.ui.accent.with_alpha(0.45)
                    } else if hover {
                        th.ui.surface_alt.with_alpha(0.85)
                    } else {
                        th.ui.surface_alt
                    };
                    p.fill_rounded(r, 4.0, &Paint::Solid(fill));
                    p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
                    let label = match hit {
                        Hit::Place => Self::place_label(model),
                        Hit::Up => "↑".into(),
                        Hit::Refresh => "⟳".into(),
                        Hit::Deep => "Subfolders".into(),
                        Hit::Chip(c) => format!("{} ▾", Self::chip_label(c, model)),
                        Hit::ClearSimilar => "✕".into(),
                        Hit::Play => {
                            if b.previewing().is_some() {
                                "■".into()
                            } else {
                                "▶".into()
                            }
                        }
                        Hit::Sync => "Sync".into(),
                        Hit::KeyMatch => "Key".into(),
                        Hit::Loop => "Loop".into(),
                        Hit::Auto => "Auto".into(),
                        _ => String::new(),
                    };
                    p.text(&label, r, &small.center());
                }
            }
        }
        // Where.
        let place = Self::buttons(size, model)
            .into_iter()
            .find(|(h, _)| *h == Hit::Place)
            .map(|(_, r)| r);
        if let Some(pr) = place {
            let x = pr.right() + 8.0;
            p.text(
                &Self::where_label(model),
                Rect::new(x, pr.y, (size.w - x - 64.0).max(0.0), pr.h),
                &dim,
            );
        }
        if b.similar_to().is_some() {
            let by = HEADER_H + SEARCH_H + CHIPS_H;
            let banner = Rect::new(PAD, by, size.w - 2.0 * PAD - 26.0, BANNER_H);
            p.fill_rounded(banner, 4.0, &Paint::Solid(th.ui.accent.with_alpha(0.15)));
            p.text(
                &Self::where_label(model),
                Rect::new(banner.x + 8.0, banner.y, banner.w - 16.0, banner.h),
                &small,
            );
        }
        let cols = Self::columns_rect(size, model);
        p.hline(0.0, size.w, cols.bottom() - 0.5, th.ui.border);
        // Rows.
        let list = Self::list_rect(size, model);
        let previewing = b.previewing().map(Path::to_path_buf);
        let spans = Self::column_spans(cols);
        let span = |by: SortBy| spans.iter().find(|(s, _)| *s == by).map(|(_, r)| *r);
        p.push_clip(list);
        let first = (self.scroll / ROW_H).floor().max(0.0) as usize;
        let shown = (list.h / ROW_H).ceil() as usize + 1;
        for (i, row) in b.rows().iter().enumerate().skip(first).take(shown) {
            let rr = self.row_rect(i, size, model);
            let selected = b.selected().contains(&row.path);
            if selected {
                p.fill(rr, th.ui.selection.with_alpha(0.35));
            } else if self.hover == Some(Hit::Row(i)) {
                p.fill(rr, th.ui.text.with_alpha(0.05));
            }
            if previewing.as_deref() == Some(row.path.as_path()) {
                p.fill(Rect::new(rr.x, rr.y, 3.0, rr.h), th.ui.accent);
            }
            let name_r = span(SortBy::Name).unwrap_or(rr);
            let marker = Rect::new(rr.x + PAD, rr.y + 6.0, 4.0, 10.0);
            let (glyph, colour) = match row.item {
                RowItem::Up => ("", th.ui.text_faint),
                RowItem::Folder => ("▸", th.ui.text_dim),
                RowItem::Audio => ("", th.ui.accent),
                RowItem::Midi => ("", th.ui.accent_2),
            };
            if glyph.is_empty() && row.is_file() {
                p.fill_rounded(marker, 1.5, &Paint::Solid(colour.with_alpha(0.8)));
            }
            let text_x = name_r.x + 10.0;
            let label = if glyph.is_empty() {
                row.name.clone()
            } else {
                format!("{glyph} {}", row.name)
            };
            let name_style = if previewing.as_deref() == Some(row.path.as_path()) {
                small.bold()
            } else {
                small
            };
            let top_line = Rect::new(text_x, rr.y + 2.0, (name_r.right() - text_x).max(0.0), 16.0);
            p.text(&label, top_line, &name_style);
            if row.is_file() {
                // The waveform (or notes) under the name.
                let thumb = Rect::new(
                    text_x,
                    rr.y + 19.0,
                    (name_r.right() - text_x - 4.0).max(0.0),
                    11.0,
                );
                let levels = row.thumb_levels();
                if !levels.is_empty() && thumb.w > 8.0 {
                    let n = levels.len();
                    let step = thumb.w / n as f32;
                    let c = if row.item == RowItem::Midi {
                        th.ui.accent_2.with_alpha(0.55)
                    } else {
                        th.ui.text_dim.with_alpha(0.6)
                    };
                    let mid = thumb.y + thumb.h / 2.0;
                    for (k, v) in levels.iter().enumerate() {
                        let h = (v * thumb.h).max(0.5);
                        p.fill(
                            Rect::new(thumb.x + k as f32 * step, mid - h / 2.0, step.max(1.0), h),
                            c,
                        );
                    }
                } else {
                    let what = if row.format.is_empty() {
                        "…"
                    } else {
                        row.format.as_str()
                    };
                    p.text(what, thumb, &faint);
                }
                if let Some(l) = row.likeness {
                    p.text(
                        &format!("≈ {:.0} %", l * 100.0),
                        Rect::new(name_r.right() - 60.0, rr.y + 2.0, 56.0, 16.0),
                        &dim.right(),
                    );
                }
                let cell =
                    |by: SortBy| span(by).map(|r| Rect::new(r.x, rr.y + 2.0, r.w - 4.0, 16.0));
                if let Some(r) = cell(SortBy::Length) {
                    let t = row.seconds.map(mmss).unwrap_or_else(|| {
                        if row.pending {
                            "…".into()
                        } else {
                            String::new()
                        }
                    });
                    p.text(&t, r, &dim.right());
                }
                if let Some(r) = cell(SortBy::Tempo)
                    && let Some(bpm) = row.bpm
                {
                    let st = if row.is_loop {
                        small.right()
                    } else {
                        dim.right()
                    };
                    p.text(&format!("{bpm:.0}"), r, &st);
                }
                if let Some(r) = cell(SortBy::Key)
                    && let Some(k) = row.key
                {
                    p.text(&short_key(k), r, &dim.right());
                }
                for n in 1..=browser::MAX_RATING {
                    let sr = Self::star_rect(rr, n);
                    let on = n <= row.rating;
                    let c = if on {
                        th.ui.accent
                    } else if self.hover == Some(Hit::Row(i))
                        || matches!(self.hover, Some(Hit::Star(j, _)) if j == i)
                    {
                        th.ui.text_faint
                    } else {
                        th.ui.text_faint.with_alpha(0.35)
                    };
                    p.text("★", sr, &TextStyle::new(th.fonts.tiny + 1.0, c).center());
                }
                if !row.tags.is_empty()
                    && let Some(r) = span(SortBy::Rating)
                {
                    p.text(
                        &row.tags.join(", "),
                        Rect::new(r.x, rr.y + 18.0, r.w, 13.0),
                        &faint.right(),
                    );
                }
            }
            p.hline(
                rr.x,
                rr.right(),
                rr.bottom() - 0.5,
                th.ui.border.with_alpha(0.4),
            );
        }
        if b.rows().is_empty() {
            let text = if let Some(e) = b.error() {
                e.to_string()
            } else if b.similar_to().is_some() {
                "Listening… (sounds analysed so far are compared)".into()
            } else if !b.search().is_empty() || !b.filter().is_any() {
                "Nothing here matches".into()
            } else {
                "No sounds or MIDI files here".into()
            };
            p.text(
                &text,
                Rect::new(list.x + 12.0, list.y + 8.0, list.w - 24.0, 18.0),
                &faint,
            );
        }
        p.pop_clip();
        // Status.
        let status_r = Rect::new(PAD, list.bottom(), size.w - 2.0 * PAD, STATUS_H);
        let (reading, analysing) = b.pending();
        let mut status = format!(
            "{} item{}",
            b.rows().iter().filter(|r| r.is_file()).count(),
            if b.rows().iter().filter(|r| r.is_file()).count() == 1 {
                ""
            } else {
                "s"
            }
        );
        if reading + analysing > 0 {
            status.push_str(&format!(" · analysing {}…", reading.max(analysing)));
        }
        if b.truncated() {
            status.push_str(" · the first 20 000 shown");
        }
        p.text(&status, status_r, &faint);
        // The preview.
        let bar = Self::bar_rect(size);
        p.fill(bar, th.ui.surface);
        p.hline(0.0, size.w, bar.y + 0.5, th.ui.border);
        let info = model.browser_preview();
        let text_x = bar.x + PAD + 40.0;
        let line = Rect::new(text_x, bar.y + 4.0, (bar.w - text_x - PAD).max(0.0), 14.0);
        match &info {
            Some(i) => {
                let fit = if i.fit.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", i.fit)
                };
                p.text(&format!("{}{fit}", i.name), line, &small);
                let wave = Rect::new(text_x, bar.y + 19.0, line.w, 9.0);
                if let Some(row) = b.row(&i.path) {
                    let levels = row.thumb_levels();
                    let n = levels.len().max(1);
                    let step = wave.w / n as f32;
                    for (k, v) in levels.iter().enumerate() {
                        let h = (v * wave.h).max(0.5);
                        p.fill(
                            Rect::new(
                                wave.x + k as f32 * step,
                                wave.y + (wave.h - h) / 2.0,
                                step.max(1.0),
                                h,
                            ),
                            th.ui.text_dim.with_alpha(0.5),
                        );
                    }
                }
                if let Some(pos) = i.position {
                    let x = wave.x + wave.w * pos as f32;
                    p.vline(x, wave.y - 2.0, wave.bottom() + 2.0, th.ui.accent);
                } else {
                    p.text("waiting for the bar…", wave, &faint);
                }
            }
            None => {
                let hint = match b.selected().first() {
                    Some(f) => format!("{} — ▶ to hear it", file_name(f)),
                    None => "Select a file to hear it; drag it to use it".into(),
                };
                p.text(&hint, line, &dim);
            }
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
                button,
                modifiers,
                clicks,
            } => {
                let Some(hit) = self.hit(pos, size, model) else {
                    return false;
                };
                cx.request(HostRequest::GrabFocus);
                match button {
                    PointerButton::Primary => {
                        self.press(hit, Click::new(pos, clicks, modifiers), size, model, cx)
                    }
                    PointerButton::Secondary => {
                        if let Hit::Row(i) | Hit::Star(i, _) = hit {
                            let row = model.browser().rows().get(i);
                            if row.is_some_and(|r| {
                                r.is_file() && !model.browser().selected().contains(&r.path)
                            }) && let Some(a) =
                                self.select(i, faderframe_ui_canvas::Modifiers::NONE, model)
                            {
                                cx.emit(a);
                            }
                            // (The menu reads the selection as it is after
                            // this click.)
                            let mut items = Self::row_menu(i, model);
                            if !model.browser().selected().iter().any(|p| {
                                model.browser().rows().get(i).is_some_and(|r| r.path == *p)
                            }) {
                                items = Self::row_menu_for(i, model);
                            }
                            cx.request(HostRequest::ContextMenu { at: pos, items });
                        } else if let Hit::Chip(_) | Hit::Place = hit {
                            self.press(hit, Click::new(pos, clicks, modifiers), size, model, cx);
                        }
                    }
                    _ => return false,
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerMove { pos, dragging, .. } => {
                if dragging {
                    if let Some((x0, db0)) = self.volume {
                        let r = Self::buttons(size, model)
                            .into_iter()
                            .find(|(h, _)| *h == Hit::Volume)
                            .map(|(_, r)| r);
                        if let Some(r) = r {
                            let db = (db0
                                + (pos.x - x0) / r.w.max(1.0) * (VOLUME_MAX - VOLUME_MIN))
                                .clamp(VOLUME_MIN, VOLUME_MAX);
                            cx.emit(Self::setting(model, |s| s.volume_db = db));
                        }
                        return true;
                    }
                    if let Some((at, files)) = &self.press
                        && self.dragging.is_none()
                        && ((pos.x - at.x).powi(2) + (pos.y - at.y).powi(2)).sqrt() >= DRAG_START
                    {
                        self.dragging = Some(files.clone());
                        cx.set_cursor(Cursor::Grabbing);
                    }
                    return self.dragging.is_some();
                }
                let hover = self.hit(pos, size, model);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                false
            }
            ViewEvent::PointerUp { .. } => {
                self.press = None;
                self.volume = None;
                if self.dragging.take().is_some() {
                    cx.set_cursor(Cursor::Default);
                }
                true
            }
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll { dy, precise, .. } => {
                self.scroll += if precise { dy } else { dy * ROW_H * 3.0 };
                self.clamp(size, model);
                cx.redraw();
                true
            }
            ViewEvent::Key { key, modifiers } => self.key(key, modifiers, size, model, cx),
            ViewEvent::FocusLost => false,
        }
    }

    fn drag_payload(&self, _model: &Session) -> Option<String> {
        self.dragging.as_deref().map(browser::payload)
    }

    fn cancel_drag(&mut self, _cx: &mut EventCx<'_, Action>) {
        self.dragging = None;
        self.press = None;
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let tip = match self.hit(pos, size, model)? {
            Hit::Place => {
                "Where to look: the project's media, its samples, recent files, folders you add"
                    .into()
            }
            Hit::Up => "The folder above".into(),
            Hit::Refresh => "List this folder again".into(),
            Hit::Search => "Search names and tags (typing in the list searches too)".into(),
            Hit::Deep => "Search the folders inside too".into(),
            Hit::Chip(_) => "Show only files like this".into(),
            Hit::ClearSimilar => "Back to the folder".into(),
            Hit::Column(_) => "Sort by this (again: the other way round)".into(),
            Hit::Star(_, n) => format!("Rate it {n} (again: no rating)"),
            Hit::Row(i) => {
                let row = model.browser().rows().get(i)?;
                if !row.is_file() {
                    return Some(format!("{} — double-click to open", row.name));
                }
                let mut parts = vec![row.name.clone()];
                if !row.format.is_empty() {
                    let mut f = row.format.clone();
                    if row.rate > 0 {
                        f.push_str(&format!(", {} Hz, {} ch", row.rate, row.channels));
                    }
                    parts.push(f);
                }
                if let Some(fit) = model.browser_fit(&row.path, model.playhead()) {
                    let t = fit.text();
                    if !t.is_empty() {
                        parts.push(format!("plays {t}"));
                    }
                }
                parts.push("double-click to hear, drag to use".into());
                parts.join(" — ")
            }
            Hit::Play => "Hear the selected file / stop".into(),
            Hit::Sync => {
                "In the project's tempo (loops start on the next bar while playing)".into()
            }
            Hit::KeyMatch => "In the project's key (transposed)".into(),
            Hit::Loop => "Loops and MIDI files go round until stopped".into(),
            Hit::Auto => "Hear a file when it is selected".into(),
            Hit::Volume => "The preview's volume (double-click: 0 dB)".into(),
            Hit::ClearSearch => "Clear the search".into(),
        };
        Some(tip)
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.browser().previewing().is_some()
    }

    fn min_size(&self) -> Size {
        Size::new(260.0, 260.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Vertical {
            return None;
        }
        let list = Self::list_rect(size, model);
        Some(ScrollInfo {
            content: model.browser().rows().len() as f32 * ROW_H,
            viewport: list.h,
            offset: self.scroll,
            start: list.y,
            end: STATUS_H + BAR_H,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Vertical {
            self.scroll = offset.max(0.0);
        }
    }

    fn drag_files(&mut self, pos: Option<Point>, size: Size, model: &Session) -> bool {
        // Folders dropped here become places.
        let _ = (size, model);
        pos.is_some()
    }

    fn drop_files(
        &mut self,
        files: &[PathBuf],
        _pos: Point,
        _size: Size,
        _model: &Session,
    ) -> Option<Action> {
        let dirs: Vec<Action> = files
            .iter()
            .filter(|f| f.is_dir())
            .map(|d| Action::Browser(BrowserOp::AddFolder(d.clone())))
            .collect();
        (!dirs.is_empty()).then_some(Action::Several(dirs))
    }

    fn accessible(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        let b = model.browser();
        let mut out = Vec::new();
        for (hit, r) in Self::buttons(size, model) {
            let (role, label, action, checked) = match hit {
                Hit::Place => (AccessRole::Button, Self::place_label(model), None, None),
                Hit::Up => (
                    AccessRole::Button,
                    "Folder Above".into(),
                    Some(Self::op(BrowserOp::Up)),
                    None,
                ),
                Hit::Refresh => (
                    AccessRole::Button,
                    "List Again".into(),
                    Some(Self::op(BrowserOp::Refresh)),
                    None,
                ),
                Hit::Search => (
                    AccessRole::Button,
                    format!("Search: {}", b.search()),
                    None,
                    None,
                ),
                Hit::Deep => (
                    AccessRole::ToggleButton,
                    "Search Subfolders".into(),
                    Some(Self::op(BrowserOp::Deep(!b.deep()))),
                    Some(b.deep()),
                ),
                Hit::Chip(c) => (
                    AccessRole::Button,
                    format!("Filter: {}", Self::chip_label(c, model)),
                    None,
                    None,
                ),
                Hit::Play => (
                    AccessRole::Button,
                    if b.previewing().is_some() {
                        "Stop".into()
                    } else {
                        "Hear".into()
                    },
                    None,
                    None,
                ),
                Hit::Sync => (
                    AccessRole::ToggleButton,
                    "In the Project's Tempo".into(),
                    Some(Self::setting(model, |s| s.sync = !s.sync)),
                    Some(b.settings().sync),
                ),
                Hit::KeyMatch => (
                    AccessRole::ToggleButton,
                    "In the Project's Key".into(),
                    Some(Self::setting(model, |s| s.key = !s.key)),
                    Some(b.settings().key),
                ),
                Hit::Loop => (
                    AccessRole::ToggleButton,
                    "Loop".into(),
                    Some(Self::setting(model, |s| s.looped = !s.looped)),
                    Some(b.settings().looped),
                ),
                Hit::Auto => (
                    AccessRole::ToggleButton,
                    "Hear on Selection".into(),
                    Some(Self::setting(model, |s| s.auto = !s.auto)),
                    Some(b.settings().auto),
                ),
                _ => continue,
            };
            let mut n = AccessNode::new(access_id_str(&format!("{hit:?}"), &[]), role, label)
                .at(r)
                .fixed();
            if let Some(c) = checked {
                n = n.checked(c);
            }
            n = match action {
                Some(a) => n.on_activate(a),
                None => n.on_press(),
            };
            if matches!(hit, Hit::Place | Hit::Chip(_)) {
                n = n.with_menu();
            }
            out.push(n);
        }
        let list = Self::list_rect(size, model);
        let mut items = Vec::new();
        // The rows round those shown (a long search's thousands are read
        // as the list scrolls to them).
        let first = ((self.scroll / ROW_H) as usize).saturating_sub(ACCESS_REACH);
        for (i, row) in b
            .rows()
            .iter()
            .enumerate()
            .skip(first)
            .take(3 * ACCESS_REACH)
        {
            let mut label = row.name.clone();
            if let Some(s) = row.seconds {
                label.push_str(&format!(", {}", mmss(s)));
            }
            if let Some(bpm) = row.bpm {
                label.push_str(&format!(", {bpm:.0} BPM"));
            }
            if let Some(k) = row.key {
                label.push_str(&format!(", {}", k.name()));
            }
            if row.rating > 0 {
                label.push_str(&format!(", {} stars", row.rating));
            }
            let mut n = AccessNode::new(
                access_id_str(&row.path.to_string_lossy(), &[1]),
                AccessRole::ListItem,
                label,
            )
            .at(self.row_rect(i, size, model))
            .selected(b.selected().contains(&row.path))
            .with_menu();
            if let Some(a) = Self::open_row(i, model) {
                n = n.on_activate(a);
            }
            items.push(n);
        }
        let mut l =
            AccessNode::new(access_id_str("files", &[]), AccessRole::List, "Files").at(list);
        l.children = items;
        out.push(l);
        out
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Browser".into())
    }

    fn access_press(
        &mut self,
        id: u64,
        press: faderframe_ui_canvas::access::AccessPress,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        use faderframe_ui_canvas::access::AccessPress;
        let menu = press == AccessPress::Menu;
        for (hit, r) in Self::buttons(size, model) {
            if access_id_str(&format!("{hit:?}"), &[]) == id {
                let click = Click::new(r.center(), 1, faderframe_ui_canvas::Modifiers::NONE);
                self.press(hit, click, size, model, cx);
                return true;
            }
        }
        if menu {
            for (i, row) in model.browser().rows().iter().enumerate() {
                if access_id_str(&row.path.to_string_lossy(), &[1]) == id {
                    let at = self.row_rect(i, size, model).center();
                    cx.request(HostRequest::ContextMenu {
                        at,
                        items: Self::row_menu(i, model),
                    });
                    return true;
                }
            }
        }
        false
    }
}

impl BrowserView {
    fn key(
        &mut self,
        key: KeyCode,
        mods: faderframe_ui_canvas::Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let b = model.browser();
        let n = b.rows().len();
        match key {
            KeyCode::Up | KeyCode::Down if n > 0 => {
                let at = Self::selected_index(model);
                let next = match (key, at) {
                    (KeyCode::Up, Some(i)) => i.saturating_sub(1),
                    (KeyCode::Down, Some(i)) => (i + 1).min(n - 1),
                    (KeyCode::Up, None) => n - 1,
                    _ => 0,
                };
                // Folders are passed by (Enter opens the one selected… by
                // the mouse); files are selected.
                let mut i = next;
                while b.rows().get(i).is_some_and(|r| !r.is_file()) {
                    if key == KeyCode::Up {
                        if i == 0 {
                            break;
                        }
                        i -= 1;
                    } else {
                        i += 1;
                    }
                }
                if let Some(a) = self.select(
                    i.min(n - 1),
                    faderframe_ui_canvas::Modifiers {
                        shift: mods.shift,
                        ..faderframe_ui_canvas::Modifiers::NONE
                    },
                    model,
                ) {
                    cx.emit(a);
                    self.show_row(i.min(n - 1), size, model);
                }
                true
            }
            KeyCode::Enter => {
                if let Some(i) = Self::selected_index(model)
                    && let Some(a) = Self::open_row(i, model)
                {
                    cx.emit(a);
                }
                true
            }
            KeyCode::Escape => {
                if b.previewing().is_some() {
                    cx.emit(Self::op(BrowserOp::StopPreview));
                } else if !b.search().is_empty() {
                    cx.emit(Self::op(BrowserOp::Search(String::new())));
                } else {
                    return false;
                }
                true
            }
            KeyCode::Backspace => {
                let mut s = b.search().to_string();
                if s.pop().is_some() {
                    cx.emit(Self::op(BrowserOp::Search(s)));
                } else {
                    cx.emit(Self::op(BrowserOp::Up));
                }
                true
            }
            KeyCode::Char(c) if !mods.ctrl && !mods.alt && !mods.meta && !c.is_control() => {
                let mut s = b.search().to_string();
                s.push(c);
                cx.emit(Self::op(BrowserOp::Search(s)));
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
