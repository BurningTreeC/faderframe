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

mod layout;

pub use layout::{INSERT_SLOT_STEP, MAX_SEND_ROWS, SENDS_PER_ROW, StripLayout};

use faderframe_core::gain::{SILENCE_DB, format_db};
use faderframe_core::pan::{format_pan, parse_pan};
use faderframe_core::{FaderLaw, TrackId};
use faderframe_project::{
    Command, InputRouting, MonitorMode, OutputRouting, SendTap, Track, TrackColor, TrackKind,
};
use faderframe_session::{Action, MeterDisplay, SelectMode, Session};
use faderframe_ui_canvas::controls::{self, FaderGeometry, KnobLook, MeterLevel};
use faderframe_ui_canvas::{
    Align, CanvasView, Color, Cursor, EventCx, HostRequest, MenuItem, Painter, Point,
    PointerButton, Rect, ScrollAxis, ScrollInfo, Size, Theme, ViewEvent,
};

const MASTER_GAP: f32 = 8.0;
/// Wooden end cheeks (themes with wood).
const CHEEK_W: f32 = 22.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
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
    Phase(TrackId),
    Monitor(TrackId),
    Input(TrackId),
    Output(TrackId),
    Level(TrackId),
    Scribble(TrackId),
    Meter(TrackId),
    Strip(TrackId),
    /// The rule under the inserts: drag to show more or fewer slots.
    InsertsGrip(TrackId),
    /// The group/VCA tags (click for the group and VCA menu).
    Tags(TrackId),
    /// The colour bar on top: opens the colour chooser.
    Color(TrackId),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum KnobTarget {
    Pan,
    Send(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
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
    Scroll {
        start_x: f32,
        start_scroll: f32,
    },
    /// Resizing the inserts section (all strips).
    InsertSlots {
        start_y: f32,
        start: usize,
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
    /// Only the master strip, filling the view (the side panel).
    master_only: bool,
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
    }
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
            master_only: false,
            hide_master: false,
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

    fn channel_tracks(model: &Session) -> Vec<&Track> {
        model
            .project()
            .tracks
            .iter()
            .filter(|t| t.kind != TrackKind::Master && t.kind != TrackKind::Midi)
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

    fn content_w(&self, count: usize) -> f32 {
        count as f32 * self.pitch()
    }

    fn clamp_scroll(&mut self, count: usize, size: Size) {
        let max = (self.content_w(count) - self.viewport_w(size)).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max);
    }

    /// Index range of strips intersecting the viewport.
    pub fn visible_range(&self, count: usize, size: Size) -> std::ops::Range<usize> {
        let pitch = self.pitch();
        let first = (self.scroll_x / pitch).floor().max(0.0) as usize;
        let last = ((self.scroll_x + self.viewport_w(size)) / pitch).ceil() as usize;
        first.min(count)..last.min(count)
    }

    fn strip_rect(&self, index: usize, size: Size) -> Rect {
        Rect::new(
            self.cheek() + index as f32 * self.pitch() - self.scroll_x,
            0.0,
            self.theme.console.strip_width,
            size.h,
        )
    }

    fn layout_for(&self, rect: Rect, t: &Track) -> StripLayout {
        let vca = t.kind == TrackKind::Vca;
        StripLayout::new(
            rect,
            &self.theme,
            matches!(t.kind, TrackKind::Audio | TrackKind::Instrument),
            t.kind != TrackKind::Master && !vca,
            self.send_rows,
            if vca { 0 } else { self.insert_slots.max(1) },
            self.show_tags,
        )
    }

    /// Size the send section for the track with the most sends (always
    /// leaving one free slot to add another).
    fn update_sends(&mut self, model: &Session) {
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
            let l = self.layout_for(rect, t);
            let id = t.id;
            let geo = FaderGeometry::new(l.fader, &self.theme);
            let pos_now = self.law.db_to_position(model.shown_volume_db(t));
            // VCAs have only a fader, mute and solo.
            let audio = (t.kind != TrackKind::Vca).then_some(());
            let color = Rect::new(l.color_bar.x, l.color_bar.y, l.color_bar.w, 7.0);
            let checks: [(Option<Rect>, Hit); 14] = [
                (Some(color), Hit::Color(id)),
                (Some(geo.cap_rect(pos_now).inset(-2.0)), Hit::FaderCap(id)),
                (Some(l.fader), Hit::FaderTrack(id)),
                (audio.map(|_| l.meter), Hit::Meter(id)),
                (audio.map(|_| l.pan_readout), Hit::PanValue(id)),
                (audio.map(|_| l.pan_knob), Hit::Pan(id)),
                (Some(l.mute), Hit::Mute(id)),
                (Some(l.solo), Hit::Solo(id)),
                (audio.map(|_| l.record), Hit::Record(id)),
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

    // --- painting --------------------------------------------------------------

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
        let l = self.layout_for(rect, t);
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
                        let text = if s.bypass {
                            format!("({name}{key})")
                        } else {
                            format!("{name}{key}")
                        };
                        controls::well_label(p, *slot, &text, s.bypass, th);
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
        if vca {
            controls::engraved(p, "VCA", l.pan_knob, th, Align::Center);
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

        controls::led_button(p, l.mute, "M", model.shown_mute(t), c.led.mute, th);
        controls::led_button(p, l.solo, "S", t.solo, c.led.solo, th);
        if vca {
            controls::led_button(p, l.record, "·", false, c.led.record, th);
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
        let level = |ch: &faderframe_session::MeterChannel| MeterLevel {
            level_db: ch.level_db,
            hold_db: ch.hold_db,
            clipped: ch.clipped,
        };
        controls::meter(p, l.meter, &[level(&m.left), level(&m.right)], th);

        let out = match t.output {
            OutputRouting::Master => "→ Master".to_string(),
            OutputRouting::Track { track } => model
                .project()
                .track(track)
                .map_or_else(|| "→ ?".to_string(), |d| format!("→ {}", d.name)),
            OutputRouting::Hardware { first_channel } => {
                format!("→ Out {}-{}", first_channel + 1, first_channel + 2)
            }
            OutputRouting::None => "→ none".to_string(),
        };
        controls::well_label(p, l.output, &out, t.output == OutputRouting::None, th);
        controls::scribble(p, l.scribble, &t.name, color, th);
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

    fn track(model: &Session, id: TrackId) -> Option<&Track> {
        model.project().track(id)
    }

    fn knob_value(&self, t: &Track, target: KnobTarget) -> Option<f32> {
        match target {
            KnobTarget::Pan => Some((t.pan + 1.0) * 0.5),
            KnobTarget::Send(i) => t.sends.get(i).map(|s| self.law.db_to_position(s.level_db)),
        }
    }

    fn knob_command(&self, t: &Track, target: KnobTarget, value: f32) -> Option<Command> {
        let value = value.clamp(0.0, 1.0);
        match target {
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
        for first in [0u16, 2] {
            let out = OutputRouting::Hardware {
                first_channel: first,
            };
            let mut item = MenuItem::new(
                format!("Hardware Out {}-{}", first + 1, first + 2),
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
        let mut items = vec![MenuItem::disabled(model.mapping_target_label(&target))];
        for (i, (label, action)) in model.midi_learn_menu(target).into_iter().enumerate() {
            let item = MenuItem::new(label, action);
            items.push(if i == 0 { item.separated() } else { item });
        }
        HostRequest::ContextMenu { at, items }
    }

    fn input_menu(model: &Session, t: &Track, at: Point) -> HostRequest<Action> {
        let choices = if t.kind == TrackKind::Instrument {
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
        if t.kind == TrackKind::Instrument {
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
                        target: match Self::empty_slot_target(t) {
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
    fn empty_slot_target(t: &Track) -> faderframe_session::PluginTarget {
        if t.kind == TrackKind::Instrument && t.instrument.is_none() {
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

    fn layout_of(&self, model: &Session, id: TrackId, size: Size) -> Option<StripLayout> {
        self.visible_strips(model, size)
            .into_iter()
            .find(|(_, t)| t.id == id)
            .map(|(r, t)| self.layout_for(r, t))
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
            Hit::Pan(id) | Hit::Send(id, _) => {
                let Some(t) = Self::track(model, id) else {
                    return false;
                };
                let target = match hit {
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
                        KnobTarget::Pan => 0.5,
                        KnobTarget::Send(_) => self.law.unity_position(),
                    };
                    if let Some(cmd) = self.knob_command(t, target, reset) {
                        cx.emit(Action::Edit(cmd));
                    }
                    return true;
                }
                cx.emit(Action::BeginGesture(match target {
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
                    cx.request(Self::output_menu(model, t, pos));
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
                            target: Self::empty_slot_target(t),
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
                    cx.request(Self::pan_request(model, t, l.pan_readout));
                }
            }
            Hit::Scribble(id) | Hit::Strip(id) => {
                if clicks >= 2
                    && matches!(hit, Hit::Scribble(_))
                    && let (Some(t), Some(l)) =
                        (Self::track(model, id), self.layout_of(model, id, size))
                {
                    cx.request(Self::rename_request(t, l.scribble));
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
            }
            Hit::Meter(_) => cx.emit(Action::ResetClipIndicators),
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
            Hit::Send(id, i) => Self::track(model, id).map(|t| Self::send_menu(model, t, i, pos)),
            Hit::Insert(id, i) => {
                Self::track(model, id).map(|t| Self::insert_menu(model, t, i, pos))
            }
            Hit::Output(id) => Self::track(model, id).map(|t| Self::output_menu(model, t, pos)),
            Hit::Input(id) | Hit::Monitor(id) => {
                Self::track(model, id).map(|t| Self::input_menu(model, t, pos))
            }
            Hit::Scribble(id) | Hit::Strip(id) | Hit::Tags(id) => {
                Self::track(model, id).map(|t| Self::track_menu(model, t, pos))
            }
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
            Hit::Pan(id) => Self::track(model, id).map(|t| {
                Self::learn_menu(
                    model,
                    t,
                    faderframe_automation::AutomationTarget::TrackPan,
                    pos,
                )
            }),
            Hit::Mute(id) => Self::track(model, id).map(|t| {
                Self::learn_menu(
                    model,
                    t,
                    faderframe_automation::AutomationTarget::TrackMute,
                    pos,
                )
            }),
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
                format!(
                    "Pan {} · Double-click to centre",
                    format_pan(model.shown_pan(t))
                )
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
            Hit::Solo(id) => format!("Solo {}", name(id)),
            Hit::Record(id) => format!("Record-arm {}", name(id)),
            Hit::Phase(_) => "Invert polarity".into(),
            Hit::Monitor(_) => "Input monitoring · Right-click for tape-style auto".into(),
            Hit::Input(_) => "Input routing".into(),
            Hit::Output(_) => "Output routing".into(),
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
            Hit::PanValue(id) => format!(
                "Pan {} · Click to type (C, L30, R45 or −100…100)",
                format_pan(model.shown_pan(Self::track(model, id)?))
            ),
            Hit::Scribble(_) => "Double-click to rename · Right-click for options".into(),
            Hit::Meter(_) => "Peak meter · Click to clear clip indicators".into(),
            Hit::InsertsGrip(_) => format!(
                "Drag to show more or fewer insert slots (now {}) · Double-click for {}",
                self.insert_slots,
                faderframe_session::DEFAULT_INSERT_SLOTS
            ),
            Hit::Strip(_) => return None,
        })
    }
}

impl CanvasView<Session, Action> for MixerView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.update_sends(model);
        let tracks = Self::channel_tracks(model);
        self.clamp_scroll(tracks.len(), size);
        p.fill(Rect::from_size(size), theme.ui.background);
        let cheek = self.cheek();
        let viewport = Rect::new(cheek, 0.0, self.viewport_w(size), size.h);
        p.push_clip(viewport);
        for i in self.visible_range(tracks.len(), size) {
            self.paint_strip(p, self.strip_rect(i, size), tracks[i], i + 1, model);
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

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        self.update_sends(model);
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
                        Some(Hit::FaderCap(_)) => Cursor::Grab,
                        Some(
                            Hit::FaderTrack(_) | Hit::Pan(_) | Hit::Send(..) | Hit::InsertsGrip(_),
                        ) => Cursor::ResizeVertical,
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
                    Some(Drag::Fader { .. } | Drag::Knob { .. }) => {
                        cx.emit(Action::EndGesture);
                        cx.set_cursor(Cursor::Default);
                    }
                    Some(Drag::InsertSlots { .. }) => {
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
                    Some(Hit::Pan(id) | Hit::Send(id, _)) if dx == 0.0 => {
                        let target = match self.hit_test(pos, size, model) {
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

    fn wants_frames(&self, model: &Session) -> bool {
        model.is_animating()
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        self.hit_test(pos, size, model)
            .and_then(|h| self.tooltip_for(h, model))
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

#[cfg(test)]
mod tests;
