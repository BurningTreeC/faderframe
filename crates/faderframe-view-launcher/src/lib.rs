//! The clip launcher: a grid of clips, a column per track and a row per
//! scene. A clip's ▶ launches it (on the next bar, beat … as the toolbar's
//! quantise says), a scene's ▶ its row; a playing clip shows a green ▶ and
//! how far it is, one waiting to start or stop a yellow one. The row at
//! the bottom stops a track, "Back to Arrangement" lets every track play
//! the arrangement again, "Record to Arrangement" writes what is launched
//! into it. Clips drag between slots (Ctrl copies); a double click on an
//! empty slot of a MIDI or instrument track makes a clip, on a MIDI clip
//! opens it in the piano roll.

#![forbid(unsafe_code)]

use faderframe_core::{ClipId, SceneId, TrackId};
use faderframe_project::launcher::{
    ClipLaunch, FollowAction, FollowKind, LaunchMode, LaunchQuantize, SlotKey,
};
use faderframe_project::{Clip, ClipContent, Track, TrackColor, TrackKind};
use faderframe_session::launcher::LauncherOp;
use faderframe_session::{Action, SelectMode, Session};
use faderframe_ui_canvas::{
    Color, Cursor, EventCx, HostRequest, MenuItem, Painter, Path, Point, PointerButton, Rect,
    ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const TOOLBAR_H: f32 = 34.0;
const HEADER_H: f32 = 28.0;
const STOP_H: f32 = 30.0;
const SCENE_W: f32 = 150.0;
const COL_W: f32 = 124.0;
const ROW_H: f32 = 32.0;
/// The ▶ part at a slot's left.
const PLAY_W: f32 = 24.0;

/// What is under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Quantize,
    Back,
    Record,
    AddScene,
    StopAll,
    Slot {
        track: TrackId,
        scene: SceneId,
        clip: Option<ClipId>,
        /// On its ▶.
        play: bool,
    },
    SceneLaunch(SceneId),
    SceneName(SceneId),
    TrackStop(TrackId),
    Header(TrackId),
}

/// A clip being dragged to another slot.
#[derive(Clone, Copy, Debug)]
struct Drag {
    from: SlotKey,
    start: Point,
    at: Point,
    moved: bool,
}

pub struct LauncherView {
    theme: Theme,
    sx: f32,
    sy: f32,
    hover: Option<Hit>,
    drag: Option<Drag>,
    /// Where clips dragged from another view (the arranger) hover.
    drop_hover: Option<Point>,
    /// The slot whose launch button is held (let go: a release).
    pressed: Option<SlotKey>,
}

fn color_of(c: TrackColor) -> Color {
    Color::rgb8(c.r, c.g, c.b)
}

impl LauncherView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            sx: 0.0,
            sy: 0.0,
            hover: None,
            drag: None,
            pressed: None,
            drop_hover: None,
        }
    }

    /// The toolbar's buttons.
    fn buttons(size: Size) -> [(Hit, Rect); 5] {
        let y = 5.0;
        let h = TOOLBAR_H - 10.0;
        let x0 = 130.0;
        let quantize = Rect::new(x0, y, 120.0, h);
        let back = Rect::new(quantize.right() + 8.0, y, 150.0, h);
        let record = Rect::new(back.right() + 8.0, y, 170.0, h);
        let add = Rect::new((size.w - 96.0).max(record.right() + 8.0), y, 88.0, h);
        let stop = Rect::new(8.0, size.h - STOP_H + 4.0, SCENE_W - 16.0, STOP_H - 8.0);
        [
            (Hit::Quantize, quantize),
            (Hit::Back, back),
            (Hit::Record, record),
            (Hit::AddScene, add),
            (Hit::StopAll, stop),
        ]
    }

    /// The grid's slot area (scrolled both ways).
    fn grid(size: Size) -> Rect {
        Rect::new(
            SCENE_W,
            TOOLBAR_H + HEADER_H,
            (size.w - SCENE_W).max(0.0),
            (size.h - TOOLBAR_H - HEADER_H - STOP_H).max(0.0),
        )
    }

    fn col_x(&self, i: usize) -> f32 {
        SCENE_W + i as f32 * COL_W - self.sx
    }

    fn row_y(&self, i: usize) -> f32 {
        TOOLBAR_H + HEADER_H + i as f32 * ROW_H - self.sy
    }

    fn tracks(model: &Session) -> Vec<&Track> {
        model.launcher_tracks()
    }

    /// What is at `pos`.
    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        for (hit, r) in Self::buttons(size) {
            if r.contains(pos) {
                return Some(hit);
            }
        }
        let tracks = Self::tracks(model);
        let scenes = &model.project().launcher.scenes;
        let grid = Self::grid(size);
        let col = |x: f32| {
            let i = ((x - SCENE_W + self.sx) / COL_W).floor();
            (x >= SCENE_W && i >= 0.0 && (i as usize) < tracks.len()).then_some(i as usize)
        };
        if pos.y < TOOLBAR_H {
            return None;
        }
        if pos.y < grid.y {
            return col(pos.x).map(|i| Hit::Header(tracks[i].id));
        }
        if pos.y >= grid.bottom() {
            return col(pos.x).map(|i| Hit::TrackStop(tracks[i].id));
        }
        let row = ((pos.y - grid.y + self.sy) / ROW_H).floor();
        if row < 0.0 || row as usize >= scenes.len() {
            return None;
        }
        let scene = scenes[row as usize].id;
        if pos.x < SCENE_W {
            return Some(if pos.x < 8.0 + PLAY_W {
                Hit::SceneLaunch(scene)
            } else {
                Hit::SceneName(scene)
            });
        }
        let i = col(pos.x)?;
        let track = tracks[i].id;
        Some(Hit::Slot {
            track,
            scene,
            clip: model.project().launcher.clip(track, scene),
            play: pos.x - self.col_x(i) < PLAY_W + 4.0,
        })
    }

    fn clamp(&mut self, size: Size, model: &Session) {
        let grid = Self::grid(size);
        let w = Self::tracks(model).len() as f32 * COL_W;
        let h = model.project().launcher.scenes.len() as f32 * ROW_H;
        self.sx = self.sx.clamp(0.0, (w - grid.w).max(0.0));
        self.sy = self.sy.clamp(0.0, (h - grid.h).max(0.0));
    }

    fn button(&self, p: &mut dyn Painter, r: Rect, label: &str, on: bool, color: Color, hit: Hit) {
        let th = &self.theme;
        let bg = if on {
            color.mix(th.ui.surface, 0.55)
        } else if self.hover == Some(hit) {
            th.ui.surface_alt.lighten(0.06)
        } else {
            th.ui.surface_alt
        };
        p.fill_rounded(r, 4.0, &bg.into());
        p.stroke_rounded(r, 4.0, 1.0, if on { color } else { th.ui.border });
        p.text(
            label,
            r,
            &TextStyle::new(th.fonts.small, th.ui.text).center(),
        );
    }

    fn triangle(p: &mut dyn Painter, c: Point, r: f32, color: Color) {
        let mut path = Path::new();
        path.move_to(Point::new(c.x - r * 0.6, c.y - r))
            .line_to(Point::new(c.x + r, c.y))
            .line_to(Point::new(c.x - r * 0.6, c.y + r))
            .close();
        p.fill_path(&path, color);
    }

    fn square(p: &mut dyn Painter, c: Point, r: f32, color: Color) {
        p.fill(Rect::new(c.x - r, c.y - r, 2.0 * r, 2.0 * r), color);
    }

    fn paint_toolbar(&self, p: &mut dyn Painter, size: Size, model: &Session) {
        let th = &self.theme;
        let bar = Rect::new(0.0, 0.0, size.w, TOOLBAR_H);
        p.fill(bar, th.ui.surface);
        p.hline(0.0, size.w, TOOLBAR_H - 0.5, th.ui.border);
        p.text(
            "Clip Launcher",
            Rect::new(12.0, 0.0, 116.0, TOOLBAR_H),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        let launched = model.launch_status().iter().any(|s| !s.arrangement);
        let records = model.launcher_records();
        for (hit, r) in Self::buttons(size) {
            match hit {
                Hit::Quantize => self.button(
                    p,
                    r,
                    &format!("Launch: {}", model.project().launcher.quantize.label()),
                    false,
                    th.ui.accent,
                    hit,
                ),
                Hit::Back => self.button(p, r, "Back to Arrangement", launched, th.ui.accent, hit),
                Hit::Record => self.button(
                    p,
                    r,
                    "● Record to Arrangement",
                    records,
                    th.arranger.record,
                    hit,
                ),
                Hit::AddScene => self.button(p, r, "+ Scene", false, th.ui.accent, hit),
                _ => {}
            }
        }
    }

    fn paint_slot(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        model: &Session,
        track: &Track,
        scene: SceneId,
    ) {
        let th = &self.theme;
        let launcher = &model.project().launcher;
        let cell = r.inset_xy(2.0, 2.0);
        let key = SlotKey {
            track: track.id,
            scene,
        };
        let state = model.launch_state(track.id);
        let hover = matches!(self.hover, Some(Hit::Slot { track: t, scene: s, .. }) if t == track.id && s == scene);
        let dragging_over = self
            .drag
            .is_some_and(|d| d.moved && Rect::contains(&cell, d.at))
            || self.drop_hover.is_some_and(|p| cell.contains(p));
        let Some(clip) = launcher
            .clip(track.id, scene)
            .and_then(|c| model.project().clip(c))
        else {
            let rec = th.arranger.record;
            let recording = model
                .launcher_recording()
                .filter(|(t, s, _)| *t == track.id && *s == scene);
            if let Some((_, _, ending)) = recording {
                // Recording here: red, the dot on, the bar running.
                p.fill_rounded(cell, 3.0, &rec.with_alpha(0.35).into());
                p.stroke_rounded(cell, 3.0, 1.5, rec);
                let play = Rect::new(cell.x, cell.y, PLAY_W, cell.h);
                p.circle(play.center(), 5.0, rec);
                p.text(
                    if ending {
                        "Recording · ending"
                    } else {
                        "Recording"
                    },
                    Rect::new(play.right() + 6.0, cell.y, cell.w - PLAY_W - 10.0, cell.h),
                    &TextStyle::new(th.fonts.small, th.ui.text),
                );
                return;
            }
            let bg = if hover || dragging_over {
                th.ui.text.with_alpha(0.07)
            } else {
                th.ui.text.with_alpha(0.025)
            };
            p.fill_rounded(cell, 3.0, &bg.into());
            if dragging_over {
                p.stroke_rounded(cell, 3.0, 1.5, th.ui.accent);
            }
            if track.record_arm {
                // Armed: the slot records.
                let play = Rect::new(cell.x, cell.y, PLAY_W, cell.h);
                let on = matches!(self.hover, Some(Hit::Slot { track: t, scene: s, play: true, .. }) if t == track.id && s == scene);
                p.circle(
                    play.center(),
                    4.5,
                    if on { rec } else { rec.with_alpha(0.55) },
                );
            } else if state.is_some_and(|s| s.playing.is_some()) {
                // An empty slot stops a track that plays a clip.
                Self::square(p, cell.center(), 3.5, th.ui.text_faint);
            }
            return;
        };
        let base = color_of(clip.color.unwrap_or(track.color));
        let playing = state
            .and_then(|s| s.playing)
            .is_some_and(|(s, _)| s == key.hash());
        let queued = state.and_then(|s| s.queued);
        let starting = queued.is_some_and(|(s, _)| s == Some(key.hash()));
        // The playing clip turns yellow only when it is being stopped (the
        // next clip blinks instead).
        let stopping = playing && queued.is_some_and(|(s, _)| s.is_none());
        let selected = model.selection.clips.contains(&clip.id);
        let fill = if playing { base } else { base.darken(0.18) };
        p.fill_rounded(cell, 3.0, &fill.into());
        if hover && !playing {
            p.fill_rounded(cell, 3.0, &th.ui.text.with_alpha(0.08).into());
        }
        let play = Rect::new(cell.x, cell.y, PLAY_W, cell.h);
        p.fill_rounded(play, 3.0, &Color::rgba(0.0, 0.0, 0.0, 0.25).into());
        let tri = if playing && !stopping {
            th.arranger.launch_playing
        } else if starting || stopping {
            th.arranger.launch_queued
        } else {
            th.arranger.clip_text.with_alpha(0.75)
        };
        let overdubbing = model
            .launcher_recording()
            .is_some_and(|(t, s, _)| t == track.id && s == scene);
        if overdubbing {
            // Overdubbing: the dot instead of the triangle, a red outline.
            p.circle(play.center(), 5.0, th.arranger.record);
        } else {
            Self::triangle(p, play.center(), 6.0, tri);
        }
        p.text(
            &clip.name,
            Rect::new(play.right() + 6.0, cell.y, cell.w - PLAY_W - 10.0, cell.h),
            &TextStyle::new(th.fonts.small, th.arranger.clip_text),
        );
        if playing && let Some(f) = model.launch_progress(track.id) {
            // How far the loop is, inside the outline.
            let w = cell.w - PLAY_W - 8.0;
            let bar = Rect::new(play.right() + 4.0, cell.bottom() - 6.0, w, 3.0);
            p.fill_rounded(bar, 1.5, &th.arranger.clip_text.with_alpha(0.18).into());
            p.fill_rounded(
                Rect::new(bar.x, bar.y, w * f, bar.h),
                1.5,
                &th.arranger.clip_text.with_alpha(0.6).into(),
            );
        }
        if overdubbing {
            p.stroke_rounded(cell, 3.0, 1.5, th.arranger.record);
        } else if playing {
            p.stroke_rounded(cell, 3.0, 1.5, th.arranger.launch_playing);
        } else if starting {
            p.stroke_rounded(cell, 3.0, 1.5, th.arranger.launch_queued);
        } else if selected {
            p.stroke_rounded(cell, 3.0, 1.5, th.arranger.selection_outline);
        }
        if dragging_over {
            p.stroke_rounded(cell, 3.0, 2.0, th.ui.accent);
        }
        // A launch mode other than trigger, and legato: letters at the
        // right (before the follow chevron).
        let settings = launcher.launch_of(key);
        let mark = match settings.mode {
            LaunchMode::Trigger => "",
            LaunchMode::Gate => "G",
            LaunchMode::Toggle => "T",
            LaunchMode::Repeat => "R",
        };
        let mark = format!("{mark}{}", if settings.legato { "L" } else { "" });
        if !mark.is_empty() {
            let right = if launcher.follow.contains_key(&key) {
                16.0
            } else {
                5.0
            };
            p.text(
                &mark,
                Rect::new(cell.right() - right - 24.0, cell.y, 24.0, cell.h),
                &TextStyle::new(th.fonts.small, th.arranger.clip_text.with_alpha(0.7))
                    .align(faderframe_ui_canvas::Align::End)
                    .bold(),
            );
        }
        // A follow action: a chevron at the right.
        if launcher.follow.contains_key(&key) {
            let x = cell.right() - 9.0;
            let y = cell.center().y;
            let mut path = Path::new();
            path.move_to(Point::new(x - 3.0, y - 4.0))
                .line_to(Point::new(x + 1.0, y))
                .line_to(Point::new(x - 3.0, y + 4.0));
            p.stroke_path(&path, 1.6, th.arranger.clip_text.with_alpha(0.75));
        }
    }

    fn paint_grid(&self, p: &mut dyn Painter, size: Size, model: &Session) {
        let th = &self.theme;
        let tracks = Self::tracks(model);
        let scenes = &model.project().launcher.scenes;
        let grid = Self::grid(size);
        // Track headers.
        let header = Rect::new(SCENE_W, TOOLBAR_H, grid.w, HEADER_H);
        p.fill(
            Rect::new(0.0, TOOLBAR_H, size.w, HEADER_H),
            th.arranger.header_bg,
        );
        p.push_clip(header);
        for (i, t) in tracks.iter().enumerate() {
            let x = self.col_x(i);
            let r = Rect::new(x, TOOLBAR_H, COL_W, HEADER_H);
            p.fill(
                Rect::new(x + 2.0, TOOLBAR_H + 3.0, 4.0, HEADER_H - 6.0),
                color_of(t.color),
            );
            let state = model.launch_state(t.id);
            let launched = state.is_some_and(|s| !s.arrangement);
            p.text(
                &t.name,
                Rect::new(x + 10.0, TOOLBAR_H, COL_W - 14.0, HEADER_H),
                &TextStyle::new(
                    th.fonts.small,
                    if launched { th.ui.text } else { th.ui.text_dim },
                )
                .bold(),
            );
            p.vline(r.right() - 0.5, r.y, r.bottom(), th.arranger.header_border);
        }
        p.pop_clip();
        p.hline(0.0, size.w, grid.y - 0.5, th.arranger.header_border);
        p.text(
            "Scenes",
            Rect::new(12.0, TOOLBAR_H, SCENE_W - 16.0, HEADER_H),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
        // Slots.
        p.push_clip(grid);
        let first = (self.sy / ROW_H).floor().max(0.0) as usize;
        let last = (((self.sy + grid.h) / ROW_H).ceil() as usize).min(scenes.len());
        for (row, scene) in scenes.iter().enumerate().take(last).skip(first) {
            let y = self.row_y(row);
            let lane = if row % 2 == 0 {
                th.arranger.lane_a
            } else {
                th.arranger.lane_b
            };
            p.fill(Rect::new(grid.x, y, grid.w, ROW_H), lane);
            for (i, t) in tracks.iter().enumerate() {
                let x = self.col_x(i);
                if x + COL_W < grid.x || x > grid.right() {
                    continue;
                }
                self.paint_slot(p, Rect::new(x, y, COL_W, ROW_H), model, t, scene.id);
            }
        }
        p.pop_clip();
        // Scenes.
        let column = Rect::new(0.0, grid.y, SCENE_W, grid.h);
        p.fill(column, th.arranger.header_bg);
        p.vline(SCENE_W - 0.5, TOOLBAR_H, size.h, th.arranger.header_border);
        p.push_clip(column);
        for (row, scene) in scenes.iter().enumerate().take(last).skip(first) {
            let y = self.row_y(row);
            let r = Rect::new(0.0, y, SCENE_W, ROW_H);
            if matches!(self.hover, Some(Hit::SceneLaunch(s) | Hit::SceneName(s)) if s == scene.id)
            {
                p.fill(r, th.ui.text.with_alpha(0.05));
            }
            // Green while every clip of the row plays.
            let mut slots = model
                .project()
                .launcher
                .slots
                .keys()
                .filter(|k| k.scene == scene.id)
                .peekable();
            let any = slots.peek().is_some();
            let all_playing = any
                && slots.all(|k| {
                    model
                        .launch_state(k.track)
                        .and_then(|s| s.playing)
                        .is_some_and(|(s, _)| s == k.hash())
                });
            let play = Rect::new(8.0, y + 4.0, PLAY_W, ROW_H - 8.0);
            p.fill_rounded(play, 3.0, &th.ui.surface_alt.into());
            Self::triangle(
                p,
                play.center(),
                6.0,
                if all_playing {
                    th.arranger.launch_playing
                } else {
                    th.ui.text_dim
                },
            );
            p.text(
                &scene.name,
                Rect::new(play.right() + 8.0, y, SCENE_W - PLAY_W - 24.0, ROW_H),
                &TextStyle::new(th.fonts.small, th.ui.text),
            );
            p.hline(
                0.0,
                SCENE_W,
                y + ROW_H - 0.5,
                th.arranger.header_border.with_alpha(0.6),
            );
        }
        p.pop_clip();
        if scenes.is_empty() {
            p.text(
                "No scenes yet: “+ Scene”, or Send to Launcher from an arrangement clip's menu",
                grid.inset(12.0),
                &TextStyle::new(th.fonts.small, th.ui.text_faint).center(),
            );
        }
        // Stop buttons.
        let stops = Rect::new(0.0, size.h - STOP_H, size.w, STOP_H);
        p.fill(stops, th.ui.surface);
        p.hline(0.0, size.w, stops.y + 0.5, th.ui.border);
        let [.., (_, all)] = Self::buttons(size);
        self.button(
            p,
            all,
            "■ Stop All Clips",
            false,
            th.ui.accent,
            Hit::StopAll,
        );
        p.push_clip(Rect::new(SCENE_W, stops.y, grid.w, STOP_H));
        for (i, t) in tracks.iter().enumerate() {
            let x = self.col_x(i);
            let r = Rect::new(x + 2.0, stops.y + 4.0, COL_W - 4.0, STOP_H - 8.0);
            let state = model.launch_state(t.id);
            let playing = state.is_some_and(|s| s.playing.is_some());
            let hover = self.hover == Some(Hit::TrackStop(t.id));
            let bg = if hover {
                th.ui.surface_alt.lighten(0.06)
            } else {
                th.ui.surface_alt
            };
            p.fill_rounded(r, 3.0, &bg.into());
            Self::square(
                p,
                Point::new(r.x + 14.0, r.center().y),
                4.0,
                if playing {
                    th.ui.text
                } else {
                    th.ui.text_faint
                },
            );
            let label = match state {
                Some(s) if s.arrangement => "Arrangement",
                Some(s) if s.playing.is_some() => "Launcher",
                Some(_) => "Stopped",
                None => "Arrangement",
            };
            p.text(
                label,
                Rect::new(r.x + 26.0, r.y, r.w - 30.0, r.h),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
            );
        }
        p.pop_clip();
    }

    fn slot_menu(&self, model: &Session, track: TrackId, scene: SceneId) -> Vec<MenuItem<Action>> {
        let l = |op| Action::Launcher(op);
        let mut items = Vec::new();
        let kind = model.project().track(track).map(|t| t.kind);
        match model.project().launcher.clip(track, scene) {
            Some(c) => {
                items.push(MenuItem::new(
                    "Launch",
                    l(LauncherOp::Launch { track, scene }),
                ));
                items.push(MenuItem::new("Stop Track", l(LauncherOp::StopTrack(track))));
                if model.project().clip(c).is_some_and(is_midi) {
                    items.push(
                        MenuItem::new("Edit in Piano Roll", Action::OpenClipEditor(c)).separated(),
                    );
                    let armed = model.project().track(track).is_some_and(|t| t.record_arm);
                    let overdubbing = model
                        .launcher_recording()
                        .is_some_and(|(t, s, _)| t == track && s == scene);
                    if overdubbing {
                        items.push(MenuItem::new(
                            "End Overdub",
                            l(LauncherOp::Record { track, scene }),
                        ));
                    } else if armed {
                        items.push(MenuItem::new(
                            "Overdub",
                            l(LauncherOp::Record { track, scene }),
                        ));
                    } else {
                        items.push(MenuItem::disabled("Overdub (arm the track)"));
                    }
                }
                // Copy to the next free slot below.
                let scenes = &model.project().launcher.scenes;
                let below = scenes
                    .iter()
                    .skip_while(|s| s.id != scene)
                    .skip(1)
                    .find(|s| model.project().launcher.clip(track, s.id).is_none());
                match below {
                    Some(s) => items.push(MenuItem::new(
                        "Duplicate Below",
                        l(LauncherOp::MoveClip {
                            from: SlotKey { track, scene },
                            to: SlotKey { track, scene: s.id },
                            copy: true,
                        }),
                    )),
                    None => items.push(MenuItem::disabled("Duplicate Below")),
                }
                items.push(MenuItem::new(
                    "Delete",
                    l(LauncherOp::ClearSlot { track, scene }),
                ));
                let key = SlotKey { track, scene };
                items.extend(Self::launch_items(model, key));
                items.push(Self::follow_menu(model, key));
            }
            None => {
                if matches!(kind, Some(TrackKind::Instrument | TrackKind::Midi)) {
                    items.push(MenuItem::new(
                        "Create MIDI Clip",
                        l(LauncherOp::CreateClip { track, scene }),
                    ));
                }
                items.push(MenuItem::new("Stop Track", l(LauncherOp::StopTrack(track))));
            }
        }
        items
    }

    /// A slot's launch settings: mode, quantisation, legato.
    fn launch_items(model: &Session, key: SlotKey) -> Vec<MenuItem<Action>> {
        let launcher = &model.project().launcher;
        let now = launcher.launch_of(key);
        let set = |launch: ClipLaunch| {
            Action::Launcher(LauncherOp::SetClipLaunch {
                track: key.track,
                scene: key.scene,
                launch: Some(launch),
            })
        };
        let modes = LaunchMode::ALL
            .iter()
            .map(|m| {
                MenuItem::new(m.label(), set(ClipLaunch { mode: *m, ..now }))
                    .checked(now.mode == *m)
            })
            .collect();
        let mut quantize = vec![
            MenuItem::new(
                format!("The Launcher's ({})", launcher.quantize.label()),
                set(ClipLaunch {
                    quantize: None,
                    ..now
                }),
            )
            .checked(now.quantize.is_none()),
        ];
        for (i, q) in LaunchQuantize::ALL.iter().enumerate() {
            let item = MenuItem::new(
                q.label(),
                set(ClipLaunch {
                    quantize: Some(*q),
                    ..now
                }),
            )
            .checked(now.quantize == Some(*q));
            quantize.push(if i == 0 { item.separated() } else { item });
        }
        vec![
            MenuItem::submenu(format!("Launch Mode: {}", now.mode.label()), modes).separated(),
            MenuItem::submenu(
                format!("Launch Quantize: {}", launcher.quantize_of(key).label()),
                quantize,
            ),
            MenuItem::new(
                "Legato",
                set(ClipLaunch {
                    legato: !now.legato,
                    ..now
                }),
            )
            .checked(now.legato),
        ]
        .into_iter()
        .chain(Self::tempo_item(model, key, now, set))
        .collect()
    }

    /// Audio: follow the project's tempo (the audio as it sounds now is
    /// in time).
    fn tempo_item(
        model: &Session,
        key: SlotKey,
        now: ClipLaunch,
        set: impl Fn(ClipLaunch) -> Action,
    ) -> Option<MenuItem<Action>> {
        let p = model.project();
        let clip = p.clip(*p.launcher.slots.get(&key)?)?;
        if !matches!(clip.content, ClipContent::Audio(_)) {
            return None;
        }
        let item = match now.tempo {
            Some(bpm) => MenuItem::new(
                format!("Follow Tempo (in time at {bpm:.1} BPM)"),
                set(ClipLaunch { tempo: None, ..now }),
            )
            .checked(true),
            None => {
                let here = model.playhead();
                MenuItem::new(
                    "Follow Tempo",
                    set(ClipLaunch {
                        tempo: Some(p.timeline.tempo.bpm_at(here)),
                        ..now
                    }),
                )
                .checked(false)
            }
        };
        Some(item)
    }

    /// A slot's follow action: what then (and otherwise, with a chance),
    /// after how long.
    fn follow_menu(model: &Session, key: SlotKey) -> MenuItem<Action> {
        let launcher = &model.project().launcher;
        let now = launcher.follow.get(&key).copied();
        let set = |follow| {
            Action::Launcher(LauncherOp::SetFollow {
                track: key.track,
                scene: key.scene,
                follow,
            })
        };
        let base = now.unwrap_or_default();
        let kinds = |pick: &dyn Fn(FollowKind) -> Option<FollowAction>,
                     current: Option<FollowKind>| {
            let mut out: Vec<MenuItem<Action>> = FollowKind::ALL
                .iter()
                .map(|k| MenuItem::new(k.label(), set(pick(*k))).checked(current == Some(*k)))
                .collect();
            let jumps: Vec<MenuItem<Action>> = (0..launcher.scenes.len().min(64))
                .map(|i| {
                    let k = FollowKind::Jump(i as u16);
                    MenuItem::new(
                        format!("{} ({})", k.label(), launcher.scenes[i].name),
                        set(pick(k)),
                    )
                    .checked(current == Some(k))
                })
                .collect();
            if !jumps.is_empty() {
                out.push(MenuItem::submenu("Jump to Scene", jumps).separated());
            }
            out
        };
        let mut items = vec![MenuItem::new("None", set(None)).checked(now.is_none())];
        let then = kinds(
            &|k| Some(FollowAction { kind: k, ..base }),
            now.map(|f| f.kind),
        );
        items.push(MenuItem::submenu(
            format!(
                "Then: {}",
                now.map_or_else(|| "—".to_string(), |f| f.kind.label())
            ),
            then,
        ));
        if let Some(f) = now {
            let mut otherwise = vec![
                MenuItem::new("Nothing Else", set(Some(FollowAction { other: None, ..f })))
                    .checked(f.other.is_none()),
            ];
            let more = kinds(
                &|k| {
                    Some(FollowAction {
                        other: Some(k),
                        chance: if f.other.is_none() { 50 } else { f.chance },
                        ..f
                    })
                },
                f.other,
            );
            otherwise.extend(
                more.into_iter()
                    .enumerate()
                    .map(|(i, m)| if i == 0 { m.separated() } else { m }),
            );
            items.push(MenuItem::submenu(
                format!(
                    "Otherwise: {}",
                    f.other.map_or_else(|| "—".to_string(), |k| k.label())
                ),
                otherwise,
            ));
            if f.other.is_some() {
                let chances = [100u8, 90, 75, 50, 25, 10, 0]
                    .into_iter()
                    .map(|c| {
                        MenuItem::new(
                            format!("{c} % Then, {} % Otherwise", 100 - c),
                            set(Some(FollowAction { chance: c, ..f })),
                        )
                        .checked(f.chance == c)
                    })
                    .collect();
                items.push(MenuItem::submenu(
                    format!("Chance: {} %", f.chance),
                    chances,
                ));
            }
            let after = [0u16, 1, 2, 4, 8, 16]
                .into_iter()
                .map(|bars| {
                    let label = match bars {
                        0 => "After the Clip".to_string(),
                        1 => "After 1 Bar".to_string(),
                        n => format!("After {n} Bars"),
                    };
                    MenuItem::new(label, set(Some(FollowAction { bars, ..f })))
                        .checked(f.bars == bars)
                })
                .collect();
            items.push(MenuItem::submenu(
                match f.bars {
                    0 => "When: After the Clip".to_string(),
                    1 => "When: After 1 Bar".to_string(),
                    n => format!("When: After {n} Bars"),
                },
                after,
            ));
        }
        MenuItem::submenu(
            format!(
                "Follow Action: {}",
                now.map_or_else(|| "None".to_string(), |f| f.kind.label())
            ),
            items,
        )
    }

    fn scene_menu(scene: SceneId) -> Vec<MenuItem<Action>> {
        let l = |op| Action::Launcher(op);
        vec![
            MenuItem::new("Launch Scene", l(LauncherOp::LaunchScene(scene))),
            MenuItem::new(
                "Insert Scene Below",
                l(LauncherOp::AddScene { after: Some(scene) }),
            )
            .separated(),
            MenuItem::new("Duplicate Scene", l(LauncherOp::DuplicateScene(scene))),
            MenuItem::new("Delete Scene", l(LauncherOp::RemoveScene(scene))),
        ]
    }

    fn rename_scene(
        &self,
        model: &Session,
        scene: SceneId,
        row: usize,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(s) = model
            .project()
            .launcher
            .scenes
            .iter()
            .find(|s| s.id == scene)
        else {
            return;
        };
        let y = self.row_y(row);
        cx.request(HostRequest::TextInput {
            at: Rect::new(
                8.0 + PLAY_W + 4.0,
                y + 4.0,
                SCENE_W - PLAY_W - 20.0,
                ROW_H - 8.0,
            ),
            initial: s.name.clone(),
            commit: Box::new(move |text: &str| {
                let name = text.trim();
                (!name.is_empty()).then(|| {
                    Action::Launcher(LauncherOp::RenameScene {
                        scene,
                        name: name.to_string(),
                    })
                })
            }),
        });
    }

    fn quantize_menu(model: &Session) -> Vec<MenuItem<Action>> {
        let launcher = &model.project().launcher;
        let now = launcher.quantize;
        let mut items: Vec<MenuItem<Action>> = LaunchQuantize::ALL
            .iter()
            .map(|q| {
                MenuItem::new(q.label(), Action::Launcher(LauncherOp::SetQuantize(*q)))
                    .checked(*q == now)
            })
            .collect();
        // Recording into slots: a fixed length, a count-in from stop.
        let (bars, count_in) = (launcher.record_bars, launcher.count_in);
        let set =
            |bars, count_in| Action::Launcher(LauncherOp::SetRecordOptions { bars, count_in });
        let bars_label = |n: u16| match n {
            0 => "Until Ended".to_string(),
            1 => "1 Bar".to_string(),
            n => format!("{n} Bars"),
        };
        let lengths = [0u16, 1, 2, 4, 8, 16]
            .into_iter()
            .map(|n| MenuItem::new(bars_label(n), set(n, count_in)).checked(bars == n))
            .collect();
        let counts = [0u8, 1, 2, 4]
            .into_iter()
            .map(|n| {
                let label = match n {
                    0 => "None".to_string(),
                    1 => "1 Bar".to_string(),
                    n => format!("{n} Bars"),
                };
                MenuItem::new(label, set(bars, n)).checked(count_in == n)
            })
            .collect();
        items.push(
            MenuItem::submenu(
                format!("Slot Recording Length: {}", bars_label(bars)),
                lengths,
            )
            .separated(),
        );
        items.push(MenuItem::submenu(
            match count_in {
                0 => "Count-in: None".to_string(),
                1 => "Count-in: 1 Bar".to_string(),
                n => format!("Count-in: {n} Bars"),
            },
            counts,
        ));
        items
    }
}

fn is_midi(c: &Clip) -> bool {
    matches!(c.content, ClipContent::Midi(_))
}

impl faderframe_ui_canvas::CanvasView<Session, Action> for LauncherView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.theme = theme.clone();
        self.clamp(size, model);
        p.fill(Rect::from_size(size), theme.arranger.background);
        self.paint_grid(p, size, model);
        self.paint_toolbar(p, size, model);
        // The dragged clip follows the pointer.
        if let Some(d) = self.drag.filter(|d| d.moved)
            && let Some(c) = model
                .project()
                .launcher
                .slots
                .get(&d.from)
                .and_then(|c| model.project().clip(*c))
        {
            let r = Rect::new(
                d.at.x - COL_W / 2.0,
                d.at.y - ROW_H / 2.0,
                COL_W - 4.0,
                ROW_H - 4.0,
            );
            let color = c
                .color
                .or_else(|| model.project().track(c.track).map(|t| t.color))
                .map_or(theme.ui.accent, color_of);
            p.fill_rounded(r, 3.0, &color.with_alpha(0.7).into());
            p.text(
                &c.name,
                r.inset_xy(8.0, 0.0),
                &TextStyle::new(theme.fonts.small, theme.arranger.clip_text),
            );
        }
    }

    fn drag_payload(&self, model: &Session) -> Option<String> {
        let d = self.drag.filter(|d| d.moved)?;
        let clip = model.project().launcher.slots.get(&d.from)?;
        Some(faderframe_session::launcher::clips_payload(&[*clip]))
    }

    fn cancel_drag(&mut self, cx: &mut EventCx<'_, Action>) {
        if self.drag.take().is_some() {
            cx.set_cursor(Cursor::Default);
            cx.redraw();
        }
    }

    fn hover_payload(
        &mut self,
        payload: Option<(&str, Point)>,
        _size: Size,
        _model: &Session,
    ) -> bool {
        let at = payload
            .filter(|(p, _)| faderframe_session::launcher::parse_clips_payload(p).is_some())
            .map(|(_, at)| at);
        let changed = at != self.drop_hover;
        self.drop_hover = at;
        changed
    }

    fn drop_payload(
        &mut self,
        payload: &str,
        pos: Point,
        size: Size,
        model: &Session,
    ) -> Option<Action> {
        self.drop_hover = None;
        let clips = faderframe_session::launcher::parse_clips_payload(payload)?;
        match self.hit(pos, size, model)? {
            Hit::Slot { track, scene, .. } => Some(Action::Launcher(LauncherOp::PlaceClips {
                clips,
                track,
                scene,
            })),
            _ => None,
        }
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model
            .launch_status()
            .iter()
            .any(|s| s.playing.is_some() || s.queued.is_some())
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let l = |op| Action::Launcher(op);
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                modifiers,
                clicks,
            } => {
                let Some(hit) = self.hit(pos, size, model) else {
                    return false;
                };
                match hit {
                    Hit::Quantize => cx.request(HostRequest::ContextMenu {
                        at: pos,
                        items: Self::quantize_menu(model),
                    }),
                    Hit::Back => cx.emit(l(LauncherOp::BackToArrangement)),
                    Hit::Record => cx.emit(l(LauncherOp::SetRecord(!model.launcher_records()))),
                    Hit::AddScene => cx.emit(l(LauncherOp::AddScene { after: None })),
                    Hit::StopAll => cx.emit(l(LauncherOp::StopAll)),
                    Hit::TrackStop(t) => cx.emit(l(LauncherOp::StopTrack(t))),
                    Hit::Header(t) => cx.emit(Action::SelectTracks {
                        tracks: vec![t],
                        mode: if modifiers.toggle() {
                            SelectMode::Toggle
                        } else {
                            SelectMode::Replace
                        },
                    }),
                    Hit::SceneLaunch(s) => cx.emit(l(LauncherOp::LaunchScene(s))),
                    Hit::SceneName(s) => {
                        if clicks >= 2 {
                            let row = model
                                .project()
                                .launcher
                                .scenes
                                .iter()
                                .position(|x| x.id == s)
                                .unwrap_or(0);
                            self.rename_scene(model, s, row, cx);
                        }
                    }
                    Hit::Slot {
                        track,
                        scene,
                        clip: Some(c),
                        play,
                    } => {
                        let overdubbing = model
                            .launcher_recording()
                            .is_some_and(|(t, s, _)| t == track && s == scene);
                        if play && overdubbing {
                            cx.emit(l(LauncherOp::Record { track, scene }));
                        } else if play {
                            cx.emit(l(LauncherOp::Launch { track, scene }));
                            self.pressed = Some(SlotKey { track, scene });
                        } else if clicks >= 2 {
                            if model.project().clip(c).is_some_and(is_midi) {
                                cx.emit(Action::OpenClipEditor(c));
                            }
                        } else {
                            cx.emit(Action::SelectClips {
                                clips: vec![c],
                                mode: if modifiers.toggle() {
                                    SelectMode::Toggle
                                } else {
                                    SelectMode::Replace
                                },
                            });
                            self.drag = Some(Drag {
                                from: SlotKey { track, scene },
                                start: pos,
                                at: pos,
                                moved: false,
                            });
                        }
                    }
                    Hit::Slot {
                        track,
                        scene,
                        clip: None,
                        play,
                    } => {
                        let kind = model.project().track(track).map(|t| t.kind);
                        let armed = model.project().track(track).is_some_and(|t| t.record_arm);
                        let recording = model
                            .launcher_recording()
                            .is_some_and(|(t, s, _)| t == track && s == scene);
                        if recording || (play && armed) {
                            cx.emit(l(LauncherOp::Record { track, scene }));
                        } else if clicks >= 2
                            && matches!(kind, Some(TrackKind::Instrument | TrackKind::Midi))
                        {
                            cx.emit(l(LauncherOp::CreateClip { track, scene }));
                        } else if model
                            .launch_state(track)
                            .is_some_and(|s| s.playing.is_some())
                        {
                            cx.emit(l(LauncherOp::Launch { track, scene }));
                        }
                    }
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                let items = match self.hit(pos, size, model) {
                    Some(Hit::Slot { track, scene, .. }) => self.slot_menu(model, track, scene),
                    Some(Hit::SceneLaunch(s) | Hit::SceneName(s)) => Self::scene_menu(s),
                    Some(Hit::Quantize) => Self::quantize_menu(model),
                    _ => return false,
                };
                cx.request(HostRequest::ContextMenu { at: pos, items });
                true
            }
            ViewEvent::PointerMove { pos, dragging, .. } => {
                if dragging && let Some(d) = &mut self.drag {
                    d.at = pos;
                    if (pos.x - d.start.x).abs() + (pos.y - d.start.y).abs() > 6.0 {
                        d.moved = true;
                        cx.set_cursor(Cursor::Grabbing);
                    }
                    cx.redraw();
                    return true;
                }
                let hover = self.hit(pos, size, model);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                let pointer = matches!(
                    hover,
                    Some(
                        Hit::Slot {
                            play: true,
                            clip: Some(_),
                            ..
                        } | Hit::SceneLaunch(_)
                    )
                );
                cx.set_cursor(if pointer {
                    Cursor::Pointer
                } else {
                    Cursor::Default
                });
                false
            }
            ViewEvent::PointerUp {
                pos,
                button: PointerButton::Primary,
                modifiers,
            } => {
                // A held launch let go (gate and repeat clips stop).
                if let Some(k) = self.pressed.take() {
                    cx.emit(l(LauncherOp::Release {
                        track: k.track,
                        scene: k.scene,
                    }));
                    return true;
                }
                let Some(d) = self.drag.take() else {
                    return false;
                };
                if d.moved
                    && let Some(Hit::Slot { track, scene, .. }) = self.hit(pos, size, model)
                {
                    let to = SlotKey { track, scene };
                    if to != d.from {
                        cx.emit(l(LauncherOp::MoveClip {
                            from: d.from,
                            to,
                            copy: modifiers.ctrl,
                        }));
                    }
                }
                cx.set_cursor(Cursor::Default);
                cx.redraw();
                true
            }
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll {
                dx,
                dy,
                modifiers,
                precise,
                ..
            } => {
                let step = |v: f32, unit: f32| if precise { v } else { v * unit };
                if modifiers.shift {
                    self.sx += step(dy, COL_W);
                } else {
                    self.sy += step(dy, ROW_H * 2.0);
                    self.sx += step(dx, COL_W);
                }
                self.clamp(size, model);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        Some(match self.hit(pos, size, model)? {
            Hit::Quantize => "When launches start: at once, on the next beat or bar(s)".into(),
            Hit::Back => "Every track plays the arrangement again".into(),
            Hit::Record => {
                "Write what the launcher plays into the arrangement as it plays, with the mixer's and devices' moves as automation".into()
            }
            Hit::AddScene => "A new scene (row) at the end".into(),
            Hit::StopAll => "Stop every launched clip".into(),
            Hit::TrackStop(_) => {
                "Stop the track's clip (it stays silent until launched or back to the arrangement)"
                    .into()
            }
            Hit::SceneLaunch(_) => "Launch the scene: its clips, the other tracks stop".into(),
            Hit::SceneName(_) => "Double-click to rename; right-click for more".into(),
            Hit::Slot {
                clip: Some(_),
                play: true,
                ..
            } => "Launch".into(),
            Hit::Slot { clip: Some(_), .. } => {
                "Drag to another slot (Ctrl copies); double-click a MIDI clip to edit it".into()
            }
            Hit::Slot {
                track, scene, play, ..
            } if model
                .launcher_recording()
                .is_some_and(|(t, s, _)| t == track && s == scene)
                || (play && model.project().track(track).is_some_and(|t| t.record_arm)) =>
            {
                if model.launcher_recording().is_some() {
                    "Click to end the recording on the next launch position".into()
                } else {
                    "Record a clip here from the next launch position".into()
                }
            }
            Hit::Slot { track, .. } => {
                let midi = model
                    .project()
                    .track(track)
                    .is_some_and(|t| matches!(t.kind, TrackKind::Instrument | TrackKind::Midi));
                if midi {
                    "Double-click to make a MIDI clip".into()
                } else {
                    "Empty slot: Send to Launcher from an arrangement clip's menu".into()
                }
            }
            Hit::Header(_) => return None,
        })
    }

    fn min_size(&self) -> Size {
        Size::new(420.0, 200.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        let grid = Self::grid(size);
        Some(match axis {
            ScrollAxis::Vertical => ScrollInfo {
                content: model.project().launcher.scenes.len() as f32 * ROW_H,
                viewport: grid.h,
                offset: self.sy,
                start: grid.y,
                end: STOP_H,
            },
            ScrollAxis::Horizontal => ScrollInfo {
                content: Self::tracks(model).len() as f32 * COL_W,
                viewport: grid.w,
                offset: self.sx,
                start: SCENE_W,
                end: 0.0,
            },
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        match axis {
            ScrollAxis::Vertical => self.sy = offset.max(0.0),
            ScrollAxis::Horizontal => self.sx = offset.max(0.0),
        }
    }
}

#[cfg(test)]
mod tests;
