//! The Album view (bottom dock; the Mastering workspace shows it): the
//! songs of a release in order, how loud each one is and how it will be
//! delivered.
//!
//! * The toolbar adds songs (the project's sections, this project, audio
//!   files and other FaderFrame projects), sets the level processing (one
//!   gain for the album or one per song, the loudness target, the
//!   true-peak ceiling and whether peaks over it are limited) and the
//!   format, rate and dither (delivery presets set them together), and runs
//!   Analyse and Export (Cancel while one runs).
//! * A row per song: its number (drag it to reorder), title (double-click
//!   renames), source, length, the pause before it, its trim and fades
//!   (drag up/down or double-click to type), how it measured (loudness,
//!   range, true peak) and how it will be delivered (gain, resulting
//!   loudness, limiting). Right-click for more; Delete removes the
//!   selected song.
//! * The footer sums the album up, shows the export folder (click to
//!   choose another) and the progress of a running job.
//!
//! Everything goes through `AlbumAction`s, each one undo step (drags are
//! one gesture).

use faderframe_audio::{STANDARD_SAMPLE_RATES, format_sample_rate};
use faderframe_audio_files::{Dither, WavFormat};
use faderframe_core::SongId;
use faderframe_project::album::{AlbumLevel, AlbumSettings, Song, SongSource};
use faderframe_session::album::{AlbumAction, AlbumTask};
use faderframe_session::analysis::LOUDNESS_TARGETS;
use faderframe_session::delivery::DELIVERY_PRESETS;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{
    CanvasView, Color, Cursor, EventCx, FileChoice, HostRequest, Key, MenuItem, Paint, Painter,
    Point, PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};

#[cfg(test)]
mod tests;

const TOOLBAR_H: f32 = 32.0;
const HEADER_H: f32 = 22.0;
const ROW_H: f32 = 28.0;
const FOOTER_H: f32 = 28.0;
const PAD: f32 = 8.0;
const DRAG_THRESHOLD: f32 = 4.0;
const FORMATS: [WavFormat; 3] = [WavFormat::Pcm16, WavFormat::Pcm24, WavFormat::Float32];
const CEILINGS: [f32; 6] = [-0.1, -0.3, -0.5, -1.0, -1.5, -2.0];
/// Audio files and projects the "+ Files" chooser offers.
const AUDIO_PATTERNS: [&str; 10] = [
    "*.wav", "*.aif", "*.aiff", "*.flac", "*.mp3", "*.ogg", "*.m4a", "*.caf", "*.WAV", "*.FLAC",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    AddSections,
    AddProject,
    AddFiles,
    Level,
    Target,
    Ceiling,
    Format,
    AlbumFile,
    Analyse,
    Export,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Number,
    Title,
    Source,
    Length,
    Pause,
    Gain,
    Fades,
    Loudness,
    Range,
    TruePeak,
    Delivered,
}

/// Columns with their header and width (0: takes the rest).
const COLUMNS: [(Column, &str, f32); 11] = [
    (Column::Number, "#", 34.0),
    (Column::Title, "Title", 0.0),
    (Column::Source, "Source", 170.0),
    (Column::Length, "Length", 60.0),
    (Column::Pause, "Pause", 58.0),
    (Column::Gain, "Trim", 66.0),
    (Column::Fades, "Fades in / out", 104.0),
    (Column::Loudness, "Loudness", 92.0),
    (Column::Range, "LRA", 58.0),
    (Column::TruePeak, "True peak", 84.0),
    (Column::Delivered, "Delivered", 210.0),
];
const TITLE_MIN: f32 = 140.0;
/// Wider views leave the rest empty instead of stretching the title.
const TITLE_MAX: f32 = 420.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Button(Button),
    Cell(usize, Column),
    /// Below the last song.
    Empty,
    Folder,
    Nothing,
}

struct Layout {
    buttons: Vec<(Button, Rect)>,
    header: Rect,
    list: Rect,
    footer: Rect,
    folder: Rect,
    /// x and width per column.
    columns: Vec<(Column, f32, f32)>,
}

enum Drag {
    Reorder {
        song: SongId,
        from: usize,
        start_y: f32,
        /// Insertion slot (0..=songs).
        slot: usize,
        moved: bool,
    },
    Value {
        song: Song,
        column: Column,
        start_y: f32,
        began: bool,
    },
}

pub struct AlbumView {
    theme: Theme,
    selected: Option<SongId>,
    scroll: f32,
    drag: Option<Drag>,
}

fn minus(s: String) -> String {
    s.replace('-', "−")
}

fn db(v: f64) -> String {
    if v.abs() < 0.05 {
        "0.0 dB".into()
    } else {
        minus(format!("{v:+.1} dB"))
    }
}

fn lufs(v: f64) -> String {
    if v.is_finite() {
        minus(format!("{v:.1} LUFS"))
    } else {
        "silent".into()
    }
}

fn duration(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// "−3.5", "3,5 dB", "−0.5 s" → the number.
fn number(text: &str) -> Option<f32> {
    let t: String = text
        .replace('−', "-")
        .replace(',', ".")
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.'))
        .collect();
    t.parse::<f32>().ok().filter(|v| v.is_finite())
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().to_string(),
    )
}

fn settings_action(settings: AlbumSettings) -> Action {
    Action::Album(AlbumAction::Settings(settings))
}

fn update(song: Song) -> Action {
    Action::Album(AlbumAction::Update(song))
}

impl AlbumView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            selected: None,
            scroll: 0.0,
            drag: None,
        }
    }

    fn button_label(b: Button, model: &Session) -> String {
        let s = &model.project().album.settings;
        match b {
            Button::AddSections => "+ Sections".into(),
            Button::AddProject => "+ This Project".into(),
            Button::AddFiles => "+ Files…".into(),
            Button::Level => match s.level {
                AlbumLevel::Album => "Album levelling ▾".into(),
                AlbumLevel::PerSong => "Per-song levelling ▾".into(),
            },
            Button::Target => match s.loudness {
                Some(l) => minus(format!("Target {l:.1} LUFS ▾")),
                None => "No loudness target ▾".into(),
            },
            Button::Ceiling => match s.ceiling {
                Some(c) => minus(format!(
                    "≤ {c:.1} dBTP, {} ▾",
                    if s.limit { "limit" } else { "less gain" }
                )),
                None => "No ceiling ▾".into(),
            },
            Button::Format => {
                let bits = match s.format {
                    WavFormat::Pcm16 => "16-bit",
                    WavFormat::Pcm24 => "24-bit",
                    WavFormat::Float32 => "32-bit float",
                };
                let dither = if s.format.is_integer() {
                    format!(" · {}", s.dither.label())
                } else {
                    String::new()
                };
                format!(
                    "{bits} · {}{dither} ▾",
                    format_sample_rate(model.album_rate())
                )
            }
            Button::AlbumFile => "Album file + CUE".into(),
            Button::Analyse => "Analyse".into(),
            Button::Export => "Export".into(),
            Button::Cancel => "Cancel".into(),
        }
    }

    fn layout(&self, size: Size, model: &Session) -> Layout {
        let width = |label: &str| label.chars().count() as f32 * 6.4 + 22.0;
        let y = (TOOLBAR_H - 22.0) / 2.0;
        let mut buttons = Vec::new();
        let mut x = PAD;
        for (i, b) in [
            Button::AddSections,
            Button::AddProject,
            Button::AddFiles,
            Button::Level,
            Button::Target,
            Button::Ceiling,
            Button::Format,
            Button::AlbumFile,
        ]
        .into_iter()
        .enumerate()
        {
            if i == 3 {
                x += 10.0;
            }
            let w = width(&Self::button_label(b, model));
            buttons.push((b, Rect::new(x, y, w, 22.0)));
            x += w + 6.0;
        }
        let right: &[Button] = if model.album_progress().is_some() {
            &[Button::Cancel]
        } else {
            &[Button::Analyse, Button::Export]
        };
        let mut rx = size.w - PAD;
        for b in right.iter().rev() {
            let w = width(&Self::button_label(*b, model)).max(70.0);
            rx -= w;
            buttons.push((*b, Rect::new(rx.max(x), y, w, 22.0)));
            rx -= 6.0;
        }
        let header = Rect::new(0.0, TOOLBAR_H, size.w, HEADER_H);
        let list = Rect::new(
            0.0,
            header.bottom(),
            size.w,
            (size.h - header.bottom() - FOOTER_H).max(0.0),
        );
        let footer = Rect::new(0.0, size.h - FOOTER_H, size.w, FOOTER_H);
        let folder_w = (size.w * 0.42).min(520.0);
        let folder = Rect::new(
            size.w - folder_w - PAD,
            footer.y + 3.0,
            folder_w,
            FOOTER_H - 6.0,
        );
        let fixed: f32 = COLUMNS.iter().map(|c| c.2).sum();
        let flex = (size.w - fixed - PAD).clamp(TITLE_MIN, TITLE_MAX);
        let mut x = 0.0;
        let columns = COLUMNS
            .iter()
            .map(|&(c, _, w)| {
                let w = if w == 0.0 { flex } else { w };
                let col = (c, x, w);
                x += w;
                col
            })
            .collect();
        Layout {
            buttons,
            header,
            list,
            footer,
            folder,
            columns,
        }
    }

    fn row_rect(&self, l: &Layout, i: usize) -> Rect {
        Rect::new(
            0.0,
            l.list.y + i as f32 * ROW_H - self.scroll,
            l.list.w,
            ROW_H,
        )
    }

    fn cell(&self, l: &Layout, i: usize, column: Column) -> Rect {
        let r = self.row_rect(l, i);
        let (x, w) = l
            .columns
            .iter()
            .find(|(c, ..)| *c == column)
            .map_or((0.0, 0.0), |(_, x, w)| (*x, *w));
        Rect::new(x, r.y, w, r.h)
    }

    fn max_scroll(&self, l: &Layout, model: &Session) -> f32 {
        (model.project().album.songs.len() as f32 * ROW_H - l.list.h + 8.0).max(0.0)
    }

    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Hit {
        let l = self.layout(size, model);
        if let Some((b, _)) = l.buttons.iter().find(|(_, r)| r.contains(pos)) {
            return Hit::Button(*b);
        }
        if l.folder.contains(pos) {
            return Hit::Folder;
        }
        if !l.list.contains(pos) {
            return Hit::Nothing;
        }
        let i = ((pos.y - l.list.y + self.scroll) / ROW_H).floor();
        let songs = model.project().album.songs.len();
        if i < 0.0 || i as usize >= songs {
            return Hit::Empty;
        }
        l.columns
            .iter()
            .find(|(_, x, w)| pos.x >= *x && pos.x < x + w)
            .map_or(Hit::Empty, |(c, ..)| Hit::Cell(i as usize, *c))
    }

    fn source_text(model: &Session, song: &Song) -> (String, bool) {
        match &song.source {
            SongSource::Section(id) => {
                match model.project().sections.iter().find(|s| s.id == *id) {
                    Some(s) => (format!("Section · {}", s.name), true),
                    None => ("Section (removed)".into(), false),
                }
            }
            SongSource::ThisProject => ("This project".into(), true),
            SongSource::Project(p) => (format!("Project · {}", file_name(p)), p.exists()),
            SongSource::AudioFile(p) => (format!("File · {}", file_name(p)), p.exists()),
        }
    }

    /// Seconds a song lasts: measured, or from its section.
    fn seconds(model: &Session, song: &Song) -> Option<f64> {
        if let Some(a) = model.album_analysis(song) {
            return Some(a.seconds);
        }
        let tempo = &model.project().timeline.tempo;
        let p = model.project();
        let (a, b) = match &song.source {
            SongSource::Section(id) => {
                let s = p.sections.iter().find(|s| s.id == *id)?;
                (s.start, s.end)
            }
            SongSource::ThisProject => (MusicalTime::ZERO, p.content_end()),
            _ => return None,
        };
        Some(tempo.musical_to_seconds(b) - tempo.musical_to_seconds(a))
    }

    // --- menus ---------------------------------------------------------------------

    fn level_menu(s: &AlbumSettings, at: Point) -> HostRequest<Action> {
        let item = |label: &str, level: AlbumLevel| {
            MenuItem::new(label, settings_action(AlbumSettings { level, ..s.clone() }))
                .checked(s.level == level)
        };
        HostRequest::ContextMenu {
            at,
            items: vec![
                item(
                    "Album: one gain for every song (keeps their balance)",
                    AlbumLevel::Album,
                ),
                item(
                    "Per song: each song reaches the target",
                    AlbumLevel::PerSong,
                ),
            ],
        }
    }

    fn target_menu(s: &AlbumSettings, at: Point) -> HostRequest<Action> {
        let mut items = vec![
            MenuItem::new(
                "No loudness target",
                settings_action(AlbumSettings {
                    loudness: None,
                    ..s.clone()
                }),
            )
            .checked(s.loudness.is_none()),
        ];
        for (l, label) in LOUDNESS_TARGETS {
            items.push(
                MenuItem::new(
                    label,
                    settings_action(AlbumSettings {
                        loudness: Some(l),
                        ..s.clone()
                    }),
                )
                .checked(s.loudness == Some(l)),
            );
        }
        HostRequest::ContextMenu { at, items }
    }

    fn ceiling_menu(s: &AlbumSettings, at: Point) -> HostRequest<Action> {
        let set = |ceiling: Option<f32>, limit: bool| {
            settings_action(AlbumSettings {
                ceiling,
                limit,
                ..s.clone()
            })
        };
        let mut items =
            vec![MenuItem::new("No ceiling", set(None, s.limit)).checked(s.ceiling.is_none())];
        for c in CEILINGS {
            items.push(
                MenuItem::new(minus(format!("{c:.1} dBTP")), set(Some(c), s.limit))
                    .checked(s.ceiling == Some(c)),
            );
        }
        items.push(
            MenuItem::new("Limit the peaks (reach the target)", set(s.ceiling, true))
                .checked(s.limit)
                .separated(),
        );
        items.push(
            MenuItem::new(
                "Use less gain (stay under the ceiling)",
                set(s.ceiling, false),
            )
            .checked(!s.limit),
        );
        HostRequest::ContextMenu { at, items }
    }

    fn format_menu(model: &Session, at: Point) -> HostRequest<Action> {
        let s = &model.project().album.settings;
        let mut items = Vec::new();
        for f in FORMATS {
            items.push(
                MenuItem::new(
                    f.label(),
                    settings_action(AlbumSettings {
                        format: f,
                        ..s.clone()
                    }),
                )
                .checked(s.format == f),
            );
        }
        let rates =
            std::iter::once(None).chain(STANDARD_SAMPLE_RATES.iter().take(4).map(|r| Some(*r)));
        for (i, r) in rates.enumerate() {
            let label = match r {
                None => format!(
                    "Project rate ({})",
                    format_sample_rate(model.project().sample_rate)
                ),
                Some(r) => format_sample_rate(r),
            };
            let item = MenuItem::new(
                label,
                settings_action(AlbumSettings {
                    sample_rate: r,
                    ..s.clone()
                }),
            )
            .checked(s.sample_rate == r);
            items.push(if i == 0 { item.separated() } else { item });
        }
        for (i, d) in Dither::ALL.into_iter().enumerate() {
            let label = format!("Dither: {}", d.label());
            let item = if s.format.is_integer() {
                MenuItem::new(
                    label,
                    settings_action(AlbumSettings {
                        dither: d,
                        ..s.clone()
                    }),
                )
                .checked(s.dither == d)
            } else {
                MenuItem::disabled(label)
            };
            items.push(if i == 0 { item.separated() } else { item });
        }
        for (i, p) in DELIVERY_PRESETS.iter().enumerate() {
            let item = MenuItem::new(
                format!("Preset: {}", p.name),
                settings_action(AlbumSettings {
                    loudness: p.finish.loudness,
                    ceiling: p.finish.ceiling,
                    limit: true,
                    format: p.format,
                    sample_rate: p.sample_rate,
                    dither: p.dither,
                    ..s.clone()
                }),
            );
            items.push(if i == 0 { item.separated() } else { item });
        }
        HostRequest::ContextMenu { at, items }
    }

    fn row_menu(model: &Session, i: usize, at: Point) -> Option<HostRequest<Action>> {
        let songs = &model.project().album.songs;
        let song = songs.get(i)?;
        let mv = |to: usize| Action::Album(AlbumAction::Move { song: song.id, to });
        let mut items = Vec::new();
        items.push(if i > 0 {
            MenuItem::new("Move Up", mv(i - 1))
        } else {
            MenuItem::disabled("Move Up")
        });
        items.push(if i + 1 < songs.len() {
            MenuItem::new("Move Down", mv(i + 1))
        } else {
            MenuItem::disabled("Move Down")
        });
        for (k, (label, fade_out)) in [
            ("No Fade Out", 0.0),
            ("Fade Out 2 s", 2.0),
            ("Fade Out 5 s", 5.0),
        ]
        .into_iter()
        .enumerate()
        {
            let item = MenuItem::new(
                label,
                update(Song {
                    fade_out,
                    ..song.clone()
                }),
            )
            .checked(song.fade_out == fade_out);
            items.push(if k == 0 { item.separated() } else { item });
        }
        items.push(
            MenuItem::new(
                "Reset Trim",
                update(Song {
                    gain_db: 0.0,
                    ..song.clone()
                }),
            )
            .separated(),
        );
        items.push(MenuItem::new(
            "Remove from Album",
            Action::Album(AlbumAction::Remove(song.id)),
        ));
        Some(HostRequest::ContextMenu { at, items })
    }

    fn text_input(
        &self,
        model: &Session,
        l: &Layout,
        i: usize,
        column: Column,
    ) -> Option<HostRequest<Action>> {
        let song = model.project().album.songs.get(i)?.clone();
        let at = self.cell(l, i, column);
        let (initial, commit): (String, faderframe_ui_canvas::TextCommit<Action>) = match column {
            Column::Title => (
                song.title.clone(),
                Box::new(move |t: &str| {
                    let t = t.trim();
                    (!t.is_empty()).then(|| {
                        update(Song {
                            title: t.to_string(),
                            ..song.clone()
                        })
                    })
                }),
            ),
            Column::Gain => (
                format!("{:.1}", song.gain_db),
                Box::new(move |t: &str| {
                    number(t).map(|v| {
                        update(Song {
                            gain_db: v.clamp(-24.0, 24.0),
                            ..song.clone()
                        })
                    })
                }),
            ),
            Column::Pause => (
                format!("{:.1}", song.pause),
                Box::new(move |t: &str| {
                    number(t).map(|v| {
                        update(Song {
                            pause: v.clamp(0.0, 60.0),
                            ..song.clone()
                        })
                    })
                }),
            ),
            Column::Fades => (
                format!("{:.1} / {:.1}", song.fade_in, song.fade_out),
                Box::new(move |t: &str| {
                    let mut parts = t.split(['/', ' ']).filter_map(number);
                    let fade_in = parts.next()?;
                    let fade_out = parts.next().unwrap_or(song.fade_out);
                    Some(update(Song {
                        fade_in: fade_in.clamp(0.0, 60.0),
                        fade_out: fade_out.clamp(0.0, 60.0),
                        ..song.clone()
                    }))
                }),
            ),
            _ => return None,
        };
        Some(HostRequest::TextInput {
            at,
            initial,
            commit,
        })
    }

    pub(crate) fn press_button(
        &mut self,
        b: Button,
        model: &Session,
        at: Point,
        cx: &mut EventCx<'_, Action>,
    ) {
        let s = &model.project().album.settings;
        match b {
            Button::AddSections => cx.emit(Action::Album(AlbumAction::AddSections)),
            Button::AddProject => cx.emit(Action::Album(AlbumAction::AddThisProject)),
            Button::AddFiles => cx.request(HostRequest::ChooseFiles {
                choice: FileChoice::Open {
                    title: "Add Songs".into(),
                    filters: vec![
                        (
                            "Audio files and FaderFrame projects".into(),
                            AUDIO_PATTERNS
                                .iter()
                                .map(|p| p.to_string())
                                .chain(std::iter::once("*.ffproj".to_string()))
                                .collect(),
                        ),
                        ("All files".into(), vec!["*".into()]),
                    ],
                },
                commit: Box::new(|paths| Some(Action::Album(AlbumAction::AddFiles(paths)))),
            }),
            Button::Level => cx.request(Self::level_menu(s, at)),
            Button::Target => cx.request(Self::target_menu(s, at)),
            Button::Ceiling => cx.request(Self::ceiling_menu(s, at)),
            Button::Format => cx.request(Self::format_menu(model, at)),
            Button::AlbumFile => cx.emit(settings_action(AlbumSettings {
                album_file: !s.album_file,
                ..s.clone()
            })),
            Button::Analyse => cx.emit(Action::Album(AlbumAction::Analyse)),
            Button::Export => cx.emit(Action::Album(AlbumAction::Export)),
            Button::Cancel => cx.emit(Action::Album(AlbumAction::Cancel)),
        }
    }

    // --- painting --------------------------------------------------------------------

    fn paint_button(&self, p: &mut dyn Painter, r: Rect, label: &str, on: bool, strong: bool) {
        let th = &self.theme;
        let bg = if on {
            th.ui.accent.with_alpha(0.35)
        } else if strong {
            th.ui.accent.with_alpha(0.2)
        } else {
            th.ui.surface_alt
        };
        p.fill_rounded(r, 4.0, &Paint::Solid(bg));
        p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
        p.text(
            label,
            r,
            &TextStyle::new(th.fonts.small, th.ui.text).center(),
        );
    }

    fn paint_toolbar(&self, p: &mut dyn Painter, l: &Layout, size: Size, model: &Session) {
        let th = &self.theme;
        let bar = Rect::new(0.0, 0.0, size.w, TOOLBAR_H);
        p.fill(bar, th.ui.surface);
        p.hline(0.0, size.w, TOOLBAR_H - 0.5, th.ui.border);
        let s = &model.project().album.settings;
        for (b, r) in &l.buttons {
            let label = Self::button_label(*b, model);
            let on = *b == Button::AlbumFile && s.album_file;
            let strong = matches!(b, Button::Export);
            self.paint_button(p, *r, &label, on, strong);
        }
    }

    fn paint_header(&self, p: &mut dyn Painter, l: &Layout) {
        let th = &self.theme;
        p.fill(l.header, th.ui.surface_alt);
        p.hline(0.0, l.header.w, l.header.bottom() - 0.5, th.ui.border);
        for (c, x, w) in &l.columns {
            let label = COLUMNS.iter().find(|k| k.0 == *c).map_or("", |k| k.1);
            let r = Rect::new(x + 6.0, l.header.y, (w - 12.0).max(0.0), l.header.h);
            let style = TextStyle::new(th.fonts.tiny, th.ui.text_dim).bold();
            p.text(
                label,
                r,
                &if right_aligned(*c) {
                    style.right()
                } else {
                    style
                },
            );
        }
    }

    fn paint_rows(&self, p: &mut dyn Painter, l: &Layout, model: &Session) {
        let th = &self.theme;
        let songs = &model.project().album.songs;
        p.fill(l.list, th.ui.background);
        p.push_clip(l.list);
        if songs.is_empty() {
            let r = Rect::new(PAD, l.list.y + 12.0, l.list.w - 2.0 * PAD, 40.0);
            p.text(
                "No songs yet. Add the project's sections (one song each), this whole project, finished mixes or other FaderFrame projects with the buttons above.",
                r,
                &TextStyle::new(th.fonts.small, th.ui.text_faint),
            );
        }
        let progress = model.album_progress();
        let settings = &model.project().album.settings;
        for (i, song) in songs.iter().enumerate() {
            let r = self.row_rect(l, i);
            if r.bottom() < l.list.y || r.y > l.list.bottom() {
                continue;
            }
            let selected = self.selected == Some(song.id);
            let bg = if selected {
                th.ui.selection
            } else if i % 2 == 1 {
                th.ui.surface.with_alpha(0.5)
            } else {
                th.ui.background
            };
            p.fill(r, bg);
            if progress.is_some_and(|pr| pr.song == i) {
                p.fill(Rect::new(0.0, r.y, 3.0, r.h), th.ui.accent);
            }
            p.hline(0.0, r.w, r.bottom() - 0.5, th.ui.border.with_alpha(0.5));
            let text = |p: &mut dyn Painter, c: Column, s: &str, color: Color, bold: bool| {
                let cell = self.cell(l, i, c);
                let inner = Rect::new(cell.x + 6.0, cell.y, (cell.w - 12.0).max(0.0), cell.h);
                let mut style = TextStyle::new(th.fonts.small, color);
                if bold {
                    style = style.bold();
                }
                if right_aligned(c) {
                    style = style.right();
                }
                p.text(s, inner, &style);
            };
            text(
                p,
                Column::Number,
                &format!("{}", i + 1),
                th.ui.text_dim,
                false,
            );
            text(p, Column::Title, &song.title, th.ui.text, true);
            let (source, ok) = Self::source_text(model, song);
            text(
                p,
                Column::Source,
                &source,
                if ok {
                    th.ui.text_dim
                } else {
                    th.tools.level_over
                },
                false,
            );
            let secs = Self::seconds(model, song);
            text(
                p,
                Column::Length,
                &secs.map_or("—".into(), duration),
                th.ui.text_dim,
                false,
            );
            let pause = if i == 0 {
                "—".to_string()
            } else {
                format!("{:.1} s", song.pause)
            };
            text(p, Column::Pause, &pause, th.ui.text, false);
            let trim = db(song.gain_db as f64);
            text(p, Column::Gain, &trim, th.ui.text, false);
            let fades = if song.fade_in == 0.0 && song.fade_out == 0.0 {
                "—".to_string()
            } else {
                format!("{:.1} / {:.1} s", song.fade_in, song.fade_out)
            };
            text(p, Column::Fades, &fades, th.ui.text, false);
            if let Some(err) = model
                .album_error(song.id)
                .filter(|_| model.album_analysis(song).is_none())
            {
                let a = self.cell(l, i, Column::Loudness);
                let b = self.cell(l, i, Column::Delivered);
                let r = Rect::new(a.x + 6.0, a.y, b.right() - a.x - 12.0, a.h);
                p.text(err, r, &TextStyle::new(th.fonts.small, th.tools.level_over));
                continue;
            }
            let Some(a) = model.album_analysis(song) else {
                text(p, Column::Loudness, "not analysed", th.ui.text_faint, false);
                continue;
            };
            let r = &a.report;
            text(p, Column::Loudness, &lufs(r.integrated), th.ui.text, false);
            text(
                p,
                Column::Range,
                &format!("{:.1} LU", r.range),
                th.ui.text_dim,
                false,
            );
            let tp_color = if r.true_peak > -0.1 {
                th.tools.level_over
            } else {
                th.ui.text
            };
            text(
                p,
                Column::TruePeak,
                &minus(format!("{:.1} dBTP", r.true_peak)),
                tp_color,
                false,
            );
            if let Some(d) = model.album_delivered(song.id) {
                let mut s = format!("{} · {}", lufs(d.loudness), db(d.gain_db));
                let limited = settings.limit && d.limiting >= 0.1;
                if limited {
                    s += &format!(" · limits {:.1}", d.limiting);
                }
                let color = if limited && d.limiting > 3.0 {
                    th.tools.level_over
                } else if limited {
                    th.tools.level_warn
                } else {
                    th.ui.text
                };
                text(p, Column::Delivered, &s, color, false);
            }
        }
        // Reordering: where the song will land.
        if let Some(Drag::Reorder {
            slot, moved: true, ..
        }) = &self.drag
        {
            let y = l.list.y + *slot as f32 * ROW_H - self.scroll;
            p.fill(Rect::new(0.0, y - 1.0, l.list.w, 2.0), th.ui.accent);
        }
        p.pop_clip();
    }

    fn paint_footer(&self, p: &mut dyn Painter, l: &Layout, model: &Session) {
        let th = &self.theme;
        p.fill(l.footer, th.ui.surface);
        p.hline(0.0, l.footer.w, l.footer.y + 0.5, th.ui.border);
        let songs = &model.project().album.songs;
        let total: f64 = songs.iter().filter_map(|s| Self::seconds(model, s)).sum();
        let mut summary = format!(
            "{} song{} · {}",
            songs.len(),
            if songs.len() == 1 { "" } else { "s" },
            duration(total + songs.iter().skip(1).map(|s| s.pause as f64).sum::<f64>())
        );
        if let Some(r) = model.album_loudness() {
            summary += &format!(
                " · album {} · highest true peak {}",
                lufs(r.integrated),
                minus(format!("{:.1} dBTP", r.true_peak))
            );
        }
        let left = Rect::new(PAD, l.footer.y, l.folder.x - 2.0 * PAD, l.footer.h);
        if let Some(pr) = model.album_progress() {
            let bar = Rect::new(
                left.x,
                l.footer.y + 6.0,
                left.w.min(360.0),
                l.footer.h - 12.0,
            );
            p.fill_rounded(bar, 3.0, &Paint::Solid(th.ui.surface_alt));
            let fill = Rect::new(bar.x, bar.y, bar.w * pr.fraction as f32, bar.h);
            p.fill_rounded(fill, 3.0, &Paint::Solid(th.ui.accent.with_alpha(0.6)));
            let verb = match pr.task {
                AlbumTask::Analyse => "Analysing",
                AlbumTask::Export => "Exporting",
            };
            p.text(
                &format!("{verb} song {} of {}…", pr.song + 1, pr.songs),
                bar,
                &TextStyle::new(th.fonts.small, th.ui.text).center(),
            );
        } else {
            p.text(
                &summary,
                left,
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
        }
        p.fill_rounded(l.folder, 4.0, &Paint::Solid(th.ui.surface_alt));
        p.stroke_rounded(l.folder, 4.0, 1.0, th.ui.border);
        let inner = Rect::new(l.folder.x + 8.0, l.folder.y, l.folder.w - 16.0, l.folder.h);
        p.text(
            &format!("Export to {}", model.album_folder().display()),
            inner,
            &TextStyle::new(th.fonts.small, th.ui.text),
        );
    }
}

fn right_aligned(c: Column) -> bool {
    matches!(
        c,
        Column::Length
            | Column::Pause
            | Column::Gain
            | Column::Loudness
            | Column::Range
            | Column::TruePeak
    )
}

impl CanvasView<Session, Action> for AlbumView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, _theme: &Theme) {
        let l = self.layout(size, model);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(&l, model));
        p.fill(Rect::from_size(size), self.theme.ui.background);
        self.paint_rows(p, &l, model);
        self.paint_header(p, &l);
        self.paint_toolbar(p, &l, size, model);
        self.paint_footer(p, &l, model);
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let l = self.layout(size, model);
        let songs = &model.project().album.songs;
        match ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                if let Hit::Cell(i, _) = self.hit(*pos, size, model) {
                    self.selected = songs.get(i).map(|s| s.id);
                    if let Some(req) = Self::row_menu(model, i, *pos) {
                        cx.request(req);
                    }
                    cx.redraw();
                    return true;
                }
                false
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                clicks,
                ..
            } => {
                cx.request(HostRequest::GrabFocus);
                match self.hit(*pos, size, model) {
                    Hit::Button(b) => {
                        let at = l
                            .buttons
                            .iter()
                            .find(|(x, _)| *x == b)
                            .map_or(*pos, |(_, r)| Point::new(r.x, r.bottom()));
                        self.press_button(b, model, at, cx);
                    }
                    Hit::Folder => {
                        let s = model.project().album.settings.clone();
                        cx.request(HostRequest::ChooseFiles {
                            choice: FileChoice::Folder {
                                title: "Export the Album to".into(),
                                initial: Some(model.album_folder()),
                            },
                            commit: Box::new(move |mut paths| {
                                paths.pop().map(|p| {
                                    settings_action(AlbumSettings {
                                        output: Some(p),
                                        ..s.clone()
                                    })
                                })
                            }),
                        });
                    }
                    Hit::Cell(i, column) => {
                        let Some(song) = songs.get(i) else {
                            return false;
                        };
                        self.selected = Some(song.id);
                        if *clicks >= 2 {
                            if let Some(req) = self.text_input(model, &l, i, column) {
                                cx.request(req);
                            }
                        } else {
                            self.drag = Some(match column {
                                Column::Pause | Column::Gain => Drag::Value {
                                    song: song.clone(),
                                    column,
                                    start_y: pos.y,
                                    began: false,
                                },
                                _ => Drag::Reorder {
                                    song: song.id,
                                    from: i,
                                    start_y: pos.y,
                                    slot: i,
                                    moved: false,
                                },
                            });
                        }
                    }
                    Hit::Empty => self.selected = None,
                    Hit::Nothing => return false,
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } => {
                let scroll = self.scroll;
                match &mut self.drag {
                    Some(Drag::Reorder {
                        start_y,
                        slot,
                        moved,
                        ..
                    }) => {
                        if (pos.y - *start_y).abs() > DRAG_THRESHOLD {
                            *moved = true;
                        }
                        let s = ((pos.y - l.list.y + scroll) / ROW_H).round();
                        *slot = (s.max(0.0) as usize).min(songs.len());
                        cx.set_cursor(Cursor::Grab);
                        cx.redraw();
                        true
                    }
                    Some(Drag::Value {
                        song,
                        column,
                        start_y,
                        began,
                    }) => {
                        let dy = *start_y - pos.y;
                        if !*began && dy.abs() < DRAG_THRESHOLD {
                            return true;
                        }
                        if !*began {
                            *began = true;
                            cx.emit(Action::BeginGesture("Album".into()));
                        }
                        let fine = if modifiers.shift { 0.1 } else { 1.0 };
                        let changed = match column {
                            Column::Gain => Song {
                                gain_db: ((song.gain_db + dy * 0.1 * fine) * 10.0).round() / 10.0,
                                ..song.clone()
                            },
                            _ => Song {
                                pause: ((song.pause + dy * 0.05 * fine).max(0.0) * 10.0).round()
                                    / 10.0,
                                ..song.clone()
                            },
                        };
                        let changed = Song {
                            gain_db: changed.gain_db.clamp(-24.0, 24.0),
                            pause: changed.pause.min(60.0),
                            ..changed
                        };
                        cx.emit(update(changed));
                        cx.set_cursor(Cursor::Grab);
                        true
                    }
                    None => false,
                }
            }
            ViewEvent::PointerUp { .. } => match self.drag.take() {
                Some(Drag::Reorder {
                    song,
                    from,
                    slot,
                    moved: true,
                    ..
                }) => {
                    let to = if slot > from { slot - 1 } else { slot };
                    if to != from {
                        cx.emit(Action::Album(AlbumAction::Move { song, to }));
                    }
                    cx.redraw();
                    true
                }
                Some(Drag::Value { began: true, .. }) => {
                    cx.emit(Action::EndGesture);
                    true
                }
                Some(_) => {
                    cx.redraw();
                    true
                }
                None => false,
            },
            ViewEvent::Scroll {
                pos, dy, precise, ..
            } if l.list.contains(*pos) => {
                let step = if *precise { *dy } else { *dy * ROW_H * 2.0 };
                self.scroll = (self.scroll + step).clamp(0.0, self.max_scroll(&l, model));
                cx.redraw();
                true
            }
            ViewEvent::Key { key, .. } => {
                let i = self
                    .selected
                    .and_then(|id| songs.iter().position(|s| s.id == id));
                match (key, i) {
                    (Key::Delete | Key::Backspace, Some(i)) => {
                        cx.emit(Action::Album(AlbumAction::Remove(songs[i].id)));
                        self.selected = songs
                            .get(i + 1)
                            .or(songs.get(i.wrapping_sub(1)))
                            .map(|s| s.id);
                        true
                    }
                    (Key::Up, Some(i)) if i > 0 => {
                        self.selected = Some(songs[i - 1].id);
                        cx.redraw();
                        true
                    }
                    (Key::Down, Some(i)) if i + 1 < songs.len() => {
                        self.selected = Some(songs[i + 1].id);
                        cx.redraw();
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        Some(
            match self.hit(pos, size, model) {
                Hit::Button(b) => match b {
                    Button::AddSections => "Add every section of the arranger's section lane that is not in the album yet (one song each)",
                    Button::AddProject => "Add this whole project as one song",
                    Button::AddFiles => "Add finished mixes (audio files) or other FaderFrame projects",
                    Button::Level => "One gain for the whole album (the songs keep their balance) or one per song",
                    Button::Target => "Integrated loudness the album or each song is brought to",
                    Button::Ceiling => "True-peak ceiling, and whether peaks over it are limited or the gain is lowered",
                    Button::Format => "File format, sample rate and dither (or a delivery preset)",
                    Button::AlbumFile => "Also write the whole album as one file with a CUE sheet (pauses become pregaps)",
                    Button::Analyse => "Render and measure every song",
                    Button::Export => "Write every song (and the album file) to the export folder",
                    Button::Cancel => "Stop the analysis or export",
                },
                Hit::Folder => "Click to choose the export folder",
                Hit::Cell(_, Column::Number) => "Drag to reorder",
                Hit::Cell(_, Column::Title) => "Double-click to rename, drag to reorder",
                Hit::Cell(_, Column::Pause) => "Silence before the song in the album — drag up/down (Shift: fine) or double-click to type",
                Hit::Cell(_, Column::Gain) => "Trim before levelling — drag up/down (Shift: fine) or double-click to type",
                Hit::Cell(_, Column::Fades) => "Fade in / out in seconds — double-click to type",
                Hit::Cell(_, Column::Delivered) => "Loudness after the album's gain, the gain, and how much the limiter will take off the peaks",
                Hit::Cell(_, Column::Range) => "Loudness range (EBU Tech 3342)",
                _ => return None,
            }
            .into(),
        )
    }

    fn min_size(&self) -> Size {
        Size::new(640.0, 160.0)
    }
}
