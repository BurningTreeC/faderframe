//! The arranger: a virtualised viewport onto the project timeline.
//!
//! Only rows and clips intersecting the viewport are visited; waveforms are
//! drawn as one filled path per clip channel from the multi-resolution peak
//! cache, so thousands of clips and millions of peaks stay cheap. Track
//! headers reuse the console controls so the arranger and the mixer share a
//! visual language.

#![forbid(unsafe_code)]

mod header;

pub use header::HeaderLayout;

use faderframe_core::gain::{SILENCE_DB, format_db};
use faderframe_core::pan::format_pan;
use faderframe_core::{ClipId, FaderLaw, TrackId, db_to_gain};
use faderframe_project::{
    Clip, ClipContent, Command, MonitorMode, MusicalRange, OutputRouting, Track, TrackColor,
    TrackKind,
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
const MAX_PPQ: f32 = 800.0;
const LOOP_BAND: f32 = 11.0;
const DRAG_THRESHOLD: f32 = 3.0;

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
    Body,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Corner,
    LoopBand(MusicalTime),
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
    Empty(MusicalTime),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Clip {
        clip: ClipId,
        grab: MusicalTime,
        origin: Point,
        moved: bool,
    },
    Scrub,
    Loop {
        anchor: MusicalTime,
        moved: bool,
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
        sx: f32,
        sy: f32,
    },
}

pub struct ArrangerView {
    theme: Theme,
    /// Zoom: pixels per quarter note.
    ppq: f32,
    scroll_x: f32,
    scroll_y: f32,
    drag: Option<Drag>,
    hover: Option<Hit>,
    law: FaderLaw,
}

fn color_of(c: TrackColor) -> Color {
    Color::rgb8(c.r, c.g, c.b)
}

fn clip_fits(track: &Track, clip: &Clip) -> bool {
    match clip.content {
        ClipContent::Audio(_) => track.kind == TrackKind::Audio,
        ClipContent::Midi(_) => matches!(track.kind, TrackKind::Instrument | TrackKind::Midi),
    }
}

impl ArrangerView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            ppq: 34.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            drag: None,
            hover: None,
            law: FaderLaw::console(),
        }
    }

    // --- coordinates -------------------------------------------------------------

    fn header_w(&self) -> f32 {
        self.theme.arranger.header_width
    }

    fn ruler_h(&self) -> f32 {
        self.theme.arranger.ruler_height
    }

    fn row_h(&self) -> f32 {
        self.theme.arranger.track_height
    }

    pub fn x_of(&self, t: MusicalTime) -> f32 {
        self.header_w() + t.quarters() as f32 * self.ppq - self.scroll_x
    }

    pub fn time_at(&self, x: f32) -> MusicalTime {
        MusicalTime::from_quarters(((x - self.header_w() + self.scroll_x) / self.ppq) as f64)
    }

    fn lane_tracks(model: &Session) -> Vec<&Track> {
        model
            .project()
            .tracks
            .iter()
            .filter(|t| t.kind != TrackKind::Master)
            .collect()
    }

    fn row_at(&self, y: f32) -> Option<usize> {
        let rel = y - self.ruler_h() + self.scroll_y;
        (rel >= 0.0).then(|| (rel / self.row_h()) as usize)
    }

    fn row_rect(&self, i: usize, size: Size) -> Rect {
        Rect::new(
            0.0,
            self.ruler_h() + i as f32 * self.row_h() - self.scroll_y,
            size.w,
            self.row_h(),
        )
    }

    pub fn visible_rows(&self, count: usize, size: Size) -> std::ops::Range<usize> {
        let first = (self.scroll_y / self.row_h()).floor().max(0.0) as usize;
        let last = ((self.scroll_y + size.h - self.ruler_h()) / self.row_h())
            .ceil()
            .max(0.0) as usize;
        first.min(count)..last.min(count)
    }

    fn content_quarters(&self, model: &Session) -> f64 {
        let p = model.project();
        let end = p
            .content_end()
            .max(p.loop_range.map_or(MusicalTime::ZERO, |r| r.end));
        end.quarters() + 32.0
    }

    fn clamp_scroll(&mut self, model: &Session, size: Size) {
        let lanes_w = (size.w - self.header_w()).max(1.0);
        let max_x = (self.content_quarters(model) as f32 * self.ppq - lanes_w * 0.5).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
        let rows = Self::lane_tracks(model).len() as f32;
        let max_y = (rows * self.row_h() - (size.h - self.ruler_h()) + self.row_h()).max(0.0);
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
        Rect::new(x0, row.y + 3.0, (x1 - x0).max(2.0), row.h - 6.0)
    }

    pub fn hit_test(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        let at = self.time_at(pos.x).max(MusicalTime::ZERO);
        if pos.y < self.ruler_h() {
            return Some(if pos.x < self.header_w() {
                Hit::Corner
            } else if pos.y < LOOP_BAND {
                Hit::LoopBand(at)
            } else {
                Hit::Ruler(at)
            });
        }
        let tracks = Self::lane_tracks(model);
        let Some(i) = self.row_at(pos.y).filter(|&i| i < tracks.len()) else {
            return (pos.x >= self.header_w()).then_some(Hit::Empty(at));
        };
        let t = tracks[i];
        let row = self.row_rect(i, size);
        if pos.x < self.header_w() {
            let l = HeaderLayout::new(Rect::new(0.0, row.y, self.header_w(), row.h));
            let part = [
                (Some(l.mute), HeaderPart::Mute),
                (Some(l.solo), HeaderPart::Solo),
                (Some(l.record), HeaderPart::Record),
                (Some(l.monitor), HeaderPart::Monitor),
                (l.pan, HeaderPart::Pan),
                (Some(l.volume.inset_xy(-2.0, -3.0)), HeaderPart::Volume),
                (Some(l.meter), HeaderPart::Meter),
                (Some(l.name), HeaderPart::Name),
            ]
            .into_iter()
            .find(|(r, _)| r.is_some_and(|r| r.contains(pos)))
            .map_or(HeaderPart::Body, |(_, p)| p);
            return Some(Hit::Header(t.id, part));
        }
        for clip in model.project().clips_of(t.id).into_iter().rev() {
            if self.clip_rect(clip, row, model).contains(pos) {
                return Some(Hit::Clip {
                    clip: clip.id,
                    track: t.id,
                    at,
                });
            }
        }
        Some(Hit::Lane { track: t.id, at })
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

    fn paint_waveform(
        &self,
        p: &mut dyn Painter,
        area: Rect,
        vis: (f32, f32),
        clip: &Clip,
        model: &Session,
        color: Color,
    ) {
        let Some(audio) = clip.as_audio() else {
            return;
        };
        let Some(peaks) = model.peaks(audio.source) else {
            return;
        };
        if area.h < 6.0 || vis.1 <= vis.0 {
            return;
        }
        let tl = &model.project().timeline;
        let sr = model.sample_rate() as f64;
        let ratio = model.frame_ratio();
        let clip_start = tl.to_samples(clip.start, sr);
        let src_off = (audio.source_offset as f64 * ratio) as i64;
        let len = (audio.length as f64 * ratio) as i64;
        let gain = db_to_gain(audio.gain_db).min(4.0);
        let channels = peaks.channels().min(2);
        let stacked = channels == 2 && area.h >= 40.0;
        let lanes: Vec<(Rect, Vec<usize>)> = if stacked {
            let (top, bottom) = area.split_top(area.h / 2.0);
            vec![(top, vec![0]), (bottom, vec![1])]
        } else {
            vec![(area, (0..channels).collect())]
        };
        let step = (1.0 / p.scale_factor().max(1.0)).max(0.5);
        let fill = color.lighten(0.3).with_alpha(0.9);
        for (lane, chans) in lanes {
            let mid = lane.center().y;
            let half = lane.h * 0.46;
            let mut top = Vec::new();
            let mut bottom = Vec::new();
            let mut x = vis.0;
            while x < vis.1 {
                let s0 = tl.to_samples(self.time_at(x), sr) - clip_start;
                let s1 = tl.to_samples(self.time_at(x + step), sr) - clip_start;
                let (s0, s1) = (s0.clamp(0, len), s1.clamp(0, len).max(s0.clamp(0, len) + 1));
                let mut lo = 0.0f32;
                let mut hi = 0.0f32;
                for &ch in &chans {
                    if let Some((a, b)) = peaks.min_max(ch, src_off + s0, src_off + s1) {
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
            p.hline(vis.0, vis.1, mid, color.lighten(0.3).with_alpha(0.25));
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
        let label_x = rect.x.max(lanes.x) + 5.0;
        let name_rect = Rect::new(
            label_x,
            rect.y,
            (rect.right() - label_x - 4.0).max(0.0),
            header_h,
        );
        if name_rect.w > 12.0 {
            p.text(
                &clip.name,
                name_rect,
                &TextStyle::new(self.theme.fonts.small, text_color).bold(),
            );
        }
        let content =
            Rect::new(rect.x, rect.y + header_h, rect.w, rect.h - header_h).inset_xy(0.0, 2.0);
        match &clip.content {
            ClipContent::Audio(audio) => {
                self.paint_waveform(p, content, vis, clip, model, color);
                // Fade handles.
                let sr = model.project().sample_rate as f64;
                for (frames, at_start) in
                    [(audio.fades.fade_in, true), (audio.fades.fade_out, false)]
                {
                    let w = (frames as f64 / sr * model.project().timeline.tempo.bpm_at(clip.start)
                        / 60.0) as f32
                        * self.ppq;
                    if w > 4.0 {
                        let mut path = Path::new();
                        if at_start {
                            path.move_to(Point::new(rect.x, content.bottom()))
                                .line_to(Point::new(rect.x + w, content.y))
                                .line_to(Point::new(rect.x, content.y))
                                .close();
                        } else {
                            path.move_to(Point::new(rect.right(), content.bottom()))
                                .line_to(Point::new(rect.right() - w, content.y))
                                .line_to(Point::new(rect.right(), content.y))
                                .close();
                        }
                        p.fill_path(&path, Color::rgba(0.0, 0.0, 0.0, 0.28));
                    }
                }
            }
            ClipContent::Midi(_) => self.paint_midi_preview(p, content, vis, clip, color),
        }
        if clip.muted {
            p.fill(rect, Color::rgba(0.08, 0.08, 0.09, 0.6));
        }
        p.pop_clip();
        if selected {
            p.stroke_rounded(rect, r, 1.6, a.selection_outline);
        } else {
            p.stroke_rounded(rect, r, 1.0, color.darken(0.25).with_alpha(0.8));
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
            Color::rgba(1.0, 1.0, 1.0, 0.04),
        );

        p.text(
            &t.name,
            l.name,
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        if let Some(info) = l.info {
            let out = match t.output {
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
            let plug = t
                .instrument
                .as_ref()
                .map(|s| format!(" · {}", s.plugin.name.trim_start_matches("FaderFrame ")))
                .unwrap_or_default();
            p.text(
                &format!("{} · {}{plug}{out}", t.kind.label(), t.layout.short_name()),
                info,
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim).family(FontFamily::Condensed),
            );
        }
        controls::led_button(p, l.mute, "M", t.mute, c.led.mute, th);
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
        if let Some(pan) = l.pan
            && t.kind.has_audio()
        {
            controls::knob(
                p,
                pan,
                (t.pan + 1.0) * 0.5,
                true,
                KnobLook {
                    cap: c.pan_cap,
                    ring: c.panel_label,
                },
                th,
            );
        }
        if t.kind.has_audio() {
            // Mini horizontal fader.
            let v = l.volume;
            let pos = self.law.db_to_position(t.volume_db);
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
                &format_db(t.volume_db),
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
        if !(model.editor.follow_playhead && model.transport().playing) {
            return;
        }
        let x = self.x_of(model.playhead());
        let lanes_w = size.w - self.header_w();
        if x > size.w - 30.0 || x < self.header_w() {
            self.scroll_x =
                (model.playhead().quarters() as f32 * self.ppq - lanes_w * 0.1).max(0.0);
        }
    }

    // --- interaction ----------------------------------------------------------------

    fn snap(&self, t: MusicalTime, model: &Session, mods: Modifiers) -> MusicalTime {
        if mods.alt {
            t
        } else {
            model.editor.snap(t, &model.project().timeline.meter)
        }
    }

    fn header_layout(&self, model: &Session, track: TrackId, size: Size) -> Option<HeaderLayout> {
        let i = Self::lane_tracks(model)
            .iter()
            .position(|t| t.id == track)?;
        let row = self.row_rect(i, size);
        Some(HeaderLayout::new(Rect::new(
            0.0,
            row.y,
            self.header_w(),
            row.h,
        )))
    }

    fn track_menu(model: &Session, t: &Track, at: Point) -> HostRequest<Action> {
        let _ = model;
        let mut items = vec![
            MenuItem::new("Add Audio Track", Action::AddTrack(TrackKind::Audio)),
            MenuItem::new(
                "Add Instrument Track",
                Action::AddTrack(TrackKind::Instrument),
            ),
            MenuItem::new("Add Bus", Action::AddTrack(TrackKind::Bus)),
            MenuItem::new("Add Aux (FX Return)", Action::AddTrack(TrackKind::Aux)),
        ];
        items.push(
            MenuItem::new(
                "Remove Track",
                Action::Edit(Command::RemoveTrack { track: t.id }),
            )
            .separated(),
        );
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
        let mut items: Vec<MenuItem<Action>> = [
            GridDivision::Bar,
            GridDivision::Beat,
            GridDivision::Note(8),
            GridDivision::Note(16),
            GridDivision::Note(32),
            GridDivision::Triplet(8),
            GridDivision::Triplet(16),
        ]
        .into_iter()
        .map(|g| {
            MenuItem::new(format!("Grid {}", g.label()), Action::SetGrid(g)).checked(ed.grid == g)
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

    fn clip_menu(clip: &Clip, at: Point) -> HostRequest<Action> {
        let mut items = vec![];
        if clip.as_midi().is_some() {
            items.push(MenuItem::new(
                "Edit in Piano Roll",
                Action::OpenClipEditor(clip.id),
            ));
        }
        items.push(MenuItem::new(
            if clip.muted {
                "Unmute Clip"
            } else {
                "Mute Clip"
            },
            Action::Edit(Command::SetClipMuted {
                clip: clip.id,
                muted: !clip.muted,
            }),
        ));
        items.push(MenuItem::new(
            "Split at Playhead",
            Action::SplitSelectedAtPlayhead,
        ));
        items.push(
            MenuItem::new(
                "Delete",
                Action::Edit(Command::RemoveClip { clip: clip.id }),
            )
            .separated(),
        );
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
        match hit {
            Hit::Corner => cx.request(Self::grid_menu(model, pos)),
            Hit::Ruler(t) => {
                cx.emit(Action::Transport(TransportAction::Locate(
                    self.snap(t, model, mods),
                )));
                self.drag = Some(Drag::Scrub);
            }
            Hit::LoopBand(t) => {
                self.drag = Some(Drag::Loop {
                    anchor: self.snap(t, model, mods),
                    moved: false,
                });
            }
            Hit::Header(id, part) => {
                let Some(t) = model.project().track(id) else {
                    return false;
                };
                let edit = |cx: &mut EventCx<'_, Action>, c| cx.emit(Action::Edit(c));
                match part {
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
                    HeaderPart::Volume if t.kind.has_audio() => {
                        if clicks >= 2 {
                            edit(cx, Command::SetTrackVolume { track: id, db: 0.0 });
                        } else if let Some(l) = self.header_layout(model, id, size) {
                            cx.emit(Action::BeginGesture("Volume".into()));
                            self.drag = Some(Drag::Volume {
                                track: id,
                                origin_x: pos.x,
                                start: self.law.db_to_position(t.volume_db),
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
                                start: (t.pan + 1.0) * 0.5,
                            });
                            cx.set_cursor(Cursor::ResizeVertical);
                        }
                    }
                    HeaderPart::Meter => cx.emit(Action::ResetClipIndicators),
                    HeaderPart::Name if clicks >= 2 => {
                        if let Some(l) = self.header_layout(model, id, size) {
                            cx.request(Self::rename_request(t, l.name));
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
            Hit::Clip { clip, track, at } => {
                let Some(c) = model.project().clip(clip) else {
                    return false;
                };
                if clicks >= 2 && c.as_midi().is_some() {
                    cx.emit(Action::OpenClipEditor(clip));
                    return true;
                }
                if mods.toggle() {
                    cx.emit(Action::SelectClips {
                        clips: vec![clip],
                        mode: SelectMode::Toggle,
                    });
                } else if !model.selection.clips.contains(&clip) {
                    cx.emit(Action::SelectClips {
                        clips: vec![clip],
                        mode: SelectMode::Replace,
                    });
                }
                cx.emit(Action::SelectTracks {
                    tracks: vec![track],
                    mode: SelectMode::Replace,
                });
                self.drag = Some(Drag::Clip {
                    clip,
                    grab: at - c.start,
                    origin: pos,
                    moved: false,
                });
                cx.set_cursor(Cursor::Grabbing);
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
                cx.emit(Action::SelectClips {
                    clips: vec![],
                    mode: SelectMode::Replace,
                });
                cx.emit(Action::SelectTracks {
                    tracks: vec![track],
                    mode: SelectMode::Replace,
                });
            }
            Hit::Empty(_) => {
                cx.emit(Action::ClearSelection);
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
        match self.drag {
            Some(Drag::Scrub) => {
                let t = self.time_at(pos.x).max(MusicalTime::ZERO);
                cx.emit(Action::Transport(TransportAction::Locate(
                    self.snap(t, model, mods),
                )));
            }
            Some(Drag::Loop { anchor, .. }) => {
                let t = self.snap(self.time_at(pos.x).max(MusicalTime::ZERO), model, mods);
                if t != anchor {
                    if let Some(Drag::Loop { moved, .. }) = &mut self.drag {
                        *moved = true;
                    }
                    let range = MusicalRange::new(anchor.min(t), anchor.max(t));
                    cx.emit(Action::Transport(TransportAction::SetLoop(range)));
                }
            }
            Some(Drag::Clip {
                clip,
                grab,
                origin,
                moved,
            }) => {
                if !moved {
                    if pos.distance(origin) < DRAG_THRESHOLD {
                        return;
                    }
                    cx.emit(Action::BeginGesture("Move Clip".into()));
                    self.drag = Some(Drag::Clip {
                        clip,
                        grab,
                        origin,
                        moved: true,
                    });
                }
                let Some(c) = model.project().clip(clip) else {
                    return;
                };
                let start = self.snap(
                    (self.time_at(pos.x) - grab).max(MusicalTime::ZERO),
                    model,
                    mods,
                );
                let tracks = Self::lane_tracks(model);
                let track = self
                    .row_at(pos.y)
                    .and_then(|i| tracks.get(i))
                    .filter(|t| clip_fits(t, c))
                    .map_or(c.track, |t| t.id);
                if start != c.start || track != c.track {
                    cx.emit(Action::Edit(Command::MoveClip { clip, track, start }));
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
            Some(Drag::Pan2D { origin, sx, sy }) => {
                self.scroll_x = sx - (pos.x - origin.x);
                self.scroll_y = sy - (pos.y - origin.y);
                self.clamp_scroll(model, size);
                cx.redraw();
            }
            None => {}
        }
    }

    fn release(&mut self, model: &Session, cx: &mut EventCx<'_, Action>) {
        match self.drag.take() {
            Some(Drag::Clip { moved: true, .. })
            | Some(Drag::Volume { .. })
            | Some(Drag::Pan { .. }) => {
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
        self.scroll_x = t.quarters() as f32 * self.ppq - (x - self.header_w());
        self.clamp_scroll(model, size);
    }
}

impl CanvasView<Session, Action> for ArrangerView {
    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
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
            p.hline(
                lanes.x,
                lanes.right(),
                row.bottom() - 1.0,
                a.header_border.with_alpha(0.6),
            );
        }
        self.paint_grid(p, lanes, model);
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
        for i in rows.clone() {
            let row = self.row_rect(i, size);
            let t = tracks[i];
            for clip in model.project().clips_of(t.id) {
                let rect = self.clip_rect(clip, row, model);
                if rect.right() < lanes.x || rect.x > lanes.right() {
                    continue;
                }
                self.paint_clip(p, rect, lanes, clip, t, model);
            }
        }
        if tracks.is_empty() {
            p.text(
                "Empty project — add a track from the Track menu or right-click the header column",
                lanes,
                &TextStyle::new(theme.fonts.normal, theme.ui.text_faint).center(),
            );
        }
        let px = self.x_of(model.playhead());
        if px >= lanes.x {
            p.fill(Rect::new(px - 0.75, lanes.y, 1.5, lanes.h), a.playhead);
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
            let l = HeaderLayout::new(Rect::new(0.0, row.y, self.header_w(), row.h));
            self.paint_header(p, &l, tracks[i], model);
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
                self.ruler_h(),
            ),
            model,
        );
        self.paint_corner(
            p,
            Rect::new(0.0, 0.0, self.header_w(), self.ruler_h()),
            model,
        );
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
                            if !model.selection.clips.contains(&clip) {
                                cx.emit(Action::SelectClips {
                                    clips: vec![clip],
                                    mode: SelectMode::Replace,
                                });
                            }
                            cx.request(Self::clip_menu(c, pos));
                        }
                    }
                    Some(Hit::Corner) | Some(Hit::Ruler(_)) | Some(Hit::LoopBand(_)) => {
                        cx.request(Self::grid_menu(model, pos));
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
                if hit != self.hover {
                    self.hover = hit;
                    cx.set_cursor(match hit {
                        Some(Hit::Clip { .. }) => Cursor::Grab,
                        Some(Hit::Ruler(_) | Hit::LoopBand(_)) => Cursor::Pointer,
                        Some(Hit::Header(_, HeaderPart::Volume)) => Cursor::ResizeHorizontal,
                        Some(Hit::Header(_, HeaderPart::Pan)) => Cursor::ResizeVertical,
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
                self.release(model, cx);
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
                } else {
                    let (mut hx, mut vy) = (dx, dy);
                    if modifiers.shift && dx == 0.0 {
                        hx = dy;
                        vy = 0.0;
                    }
                    let unit = if precise { 1.0 } else { 48.0 };
                    self.scroll_x += hx * unit;
                    self.scroll_y += vy * unit;
                    self.clamp_scroll(model, size);
                }
                cx.redraw();
                true
            }
            ViewEvent::Key { key, modifiers } => {
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
            Hit::Corner => Some("Grid, snap and follow settings".into()),
            Hit::Header(id, part) => {
                let t = model.project().track(id)?;
                Some(match part {
                    HeaderPart::Volume => format!(
                        "Volume {} dB · Drag · Double-click for 0 dB",
                        format_db(t.volume_db)
                    ),
                    HeaderPart::Pan => {
                        format!("Pan {} · Double-click to centre", format_pan(t.pan))
                    }
                    HeaderPart::Mute => "Mute".into(),
                    HeaderPart::Solo => "Solo".into(),
                    HeaderPart::Record => "Record arm".into(),
                    HeaderPart::Monitor => "Input monitoring".into(),
                    HeaderPart::Name => "Double-click to rename".into(),
                    _ => return None,
                })
            }
            Hit::Lane { track, .. } => {
                let t = model.project().track(track)?;
                matches!(t.kind, TrackKind::Instrument | TrackKind::Midi)
                    .then(|| "Double-click to create a MIDI clip".into())
            }
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
                offset: self.scroll_x,
            },
            ScrollAxis::Vertical => ScrollInfo {
                content: Self::lane_tracks(model).len() as f32 * self.row_h() + self.row_h(),
                viewport: (size.h - self.ruler_h()).max(0.0),
                offset: self.scroll_y,
            },
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        match axis {
            ScrollAxis::Horizontal => self.scroll_x = offset.max(0.0),
            ScrollAxis::Vertical => self.scroll_y = offset.max(0.0),
        }
    }
}

#[cfg(test)]
mod tests;
