//! The Channel Overview: the selected track's signal path in one row —
//! what feeds it, every device in signal-flow order (a card each), the
//! channel (level, pan, mute, solo), its sends and where it goes. A card
//! shows its device's name, role and format, its program, sidechain,
//! latency, gain reduction and whether it failed; its power button
//! bypasses it, × takes it out, a click opens its editor (Ctrl+click:
//! bypass, Alt+click: remove, as on the mixer's insert slots) and a drag
//! moves an insert along the chain. "+" opens the plugin browser
//! (right-click: the quick list of effects); right-clicking a card gives
//! the mixer's insert menu (presets, programs, sidechain, outputs, MIDI
//! learn of its bypass).

use crate::chain::{self, Device, Role};
use faderframe_core::gain::format_db;
use faderframe_core::pan::{format_pan, parse_pan};
use faderframe_project::{Command, Track, TrackKind};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    AccessNode, AccessRole, CanvasView, Color, Cursor, EventCx, HostRequest, MenuItem, Modifiers,
    Paint, Painter, Point, PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme,
    ViewEvent, access_id,
};
use faderframe_view_mixer::MixerView;

const HEADER_H: f32 = 30.0;
const PAD: f32 = 10.0;
/// A device card, the blocks at the ends and the gaps (an arrow each).
const CARD_W: f32 = 150.0;
const BLOCK_W: f32 = 132.0;
const ADD_W: f32 = 44.0;
const GAP: f32 = 22.0;
const MAX_H: f32 = 150.0;
/// A device inside a container: lower, indented.
const NESTED_DROP: f32 = 14.0;

/// Access ids' kinds.
const NODE_VIEW: u64 = 0x0c01;
const NODE_DEVICE: u64 = 0x0c02;
const NODE_POWER: u64 = 0x0c03;
const NODE_REMOVE: u64 = 0x0c04;
const NODE_ADD: u64 = 0x0c05;
const NODE_IO: u64 = 0x0c06;
const NODE_CHANNEL: u64 = 0x0c07;

/// What is under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// "+": a device after the last.
    Add,
    Input,
    Output,
    /// A MIDI track's external MIDI device.
    MidiOut,
    Device(usize),
    Bypass(usize),
    Remove(usize),
    Volume,
    Pan,
    Mute,
    Solo,
    Arm,
    Send(usize),
    /// "+ Send".
    AddSend,
}

/// A card pressed: a click on release, or a drag along the chain.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    device: usize,
    origin: Point,
    pos: Point,
    moved: bool,
}

/// What the row holds, left to right.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Block {
    Input,
    Device(usize),
    Add,
    Channel,
    Sends,
    Output,
}

pub struct ChannelOverviewView {
    theme: Theme,
    scroll_x: f32,
    hover: Option<Hit>,
    drag: Option<Drag>,
}

impl ChannelOverviewView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll_x: 0.0,
            hover: None,
            drag: None,
        }
    }

    fn track(model: &Session) -> Option<&Track> {
        chain::inspected_track(model).and_then(|t| model.project().track(t))
    }

    fn blocks(t: &Track, devices: &[Device]) -> Vec<Block> {
        let mut out = vec![Block::Input];
        out.extend((0..devices.len()).map(Block::Device));
        if chain::holds_devices(t) {
            out.push(Block::Add);
        }
        if t.kind.has_audio() || t.kind == TrackKind::Vca || t.kind == TrackKind::Folder {
            out.push(Block::Channel);
        }
        if t.kind.has_audio() && t.kind != TrackKind::Master {
            out.push(Block::Sends);
        }
        if t.kind.has_audio() || t.kind == TrackKind::Midi {
            out.push(Block::Output);
        }
        out
    }

    fn width_of(b: Block) -> f32 {
        match b {
            Block::Device(_) => CARD_W,
            Block::Add => ADD_W,
            _ => BLOCK_W,
        }
    }

    /// The body's band (cards keep a sensible height in a tall dock).
    fn band(size: Size) -> (f32, f32) {
        let h = (size.h - HEADER_H - 2.0 * PAD).clamp(60.0, MAX_H);
        (HEADER_H + PAD, h)
    }

    /// Every block's rectangle (scrolled).
    fn layout(&self, t: &Track, devices: &[Device], size: Size) -> Vec<(Block, Rect)> {
        let (y, h) = Self::band(size);
        let mut x = PAD - self.scroll_x;
        let mut out = Vec::new();
        for b in Self::blocks(t, devices) {
            let w = Self::width_of(b);
            let r = match b {
                Block::Device(i) if devices[i].depth > 0 => {
                    let drop = NESTED_DROP * devices[i].depth.min(3) as f32;
                    Rect::new(x, y + drop, w, (h - drop).max(30.0))
                }
                _ => Rect::new(x, y, w, h),
            };
            out.push((b, r));
            x += w + GAP;
        }
        out
    }

    fn content_w(t: &Track, devices: &[Device]) -> f32 {
        Self::blocks(t, devices)
            .into_iter()
            .map(|b| Self::width_of(b) + GAP)
            .sum::<f32>()
            + 2.0 * PAD
            - GAP
    }

    /// A card's buttons: power (bypass) and remove.
    fn card_buttons(r: Rect) -> (Rect, Rect) {
        (
            Rect::new(r.x + 6.0, r.y + 7.0, 18.0, 16.0),
            Rect::new(r.right() - 22.0, r.y + 7.0, 16.0, 16.0),
        )
    }

    /// The channel block's controls: volume, pan, and the M S R buttons.
    fn channel_rects(r: Rect) -> (Rect, Rect, [Rect; 3]) {
        let volume = Rect::new(r.x + 8.0, r.y + 24.0, r.w - 16.0, 16.0);
        let pan = Rect::new(r.x + 8.0, r.y + 44.0, r.w - 16.0, 16.0);
        let bw = (r.w - 16.0 - 8.0) / 3.0;
        let y = (r.y + 66.0).min(r.bottom() - 22.0);
        let b = |i: f32| Rect::new(r.x + 8.0 + i * (bw + 4.0), y, bw, 18.0);
        (volume, pan, [b(0.0), b(1.0), b(2.0)])
    }

    /// The sends block's lines: the sends that fit, then one more line —
    /// "+ Send", or how many more there are. Returns the lines and how
    /// many sends they show.
    fn send_rects(r: Rect, sends: usize) -> (Vec<Rect>, usize) {
        let rows = ((r.h - 24.0) / 18.0).floor().max(1.0) as usize;
        let shown = sends.min(rows - 1);
        let lines = (0..=shown)
            .map(|i| Rect::new(r.x + 6.0, r.y + 22.0 + i as f32 * 18.0, r.w - 12.0, 16.0))
            .collect();
        (lines, shown)
    }

    /// A MIDI track's output block: what it plays, its MIDI device.
    fn midi_out_rect(r: Rect) -> Rect {
        Rect::new(r.x + 6.0, r.y + 58.0, r.w - 12.0, 16.0)
    }

    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        let t = Self::track(model)?;
        let devices = chain::devices(model, t);
        if pos.y < HEADER_H {
            return None;
        }
        for (b, r) in self.layout(t, &devices, size) {
            if !r.contains(pos) {
                continue;
            }
            return Some(match b {
                Block::Input => Hit::Input,
                Block::Add => Hit::Add,
                Block::Output => {
                    if t.kind == TrackKind::Midi && Self::midi_out_rect(r).contains(pos) {
                        Hit::MidiOut
                    } else {
                        Hit::Output
                    }
                }
                Block::Device(i) => {
                    let (power, remove) = Self::card_buttons(r);
                    if power.contains(pos) {
                        Hit::Bypass(i)
                    } else if remove.contains(pos) && chain::remove(t, &devices[i]).is_some() {
                        Hit::Remove(i)
                    } else {
                        Hit::Device(i)
                    }
                }
                Block::Channel => {
                    let (volume, pan, buttons) = Self::channel_rects(r);
                    if volume.contains(pos) && (t.kind.has_audio() || t.kind == TrackKind::Vca) {
                        return Some(Hit::Volume);
                    }
                    if pan.contains(pos) && Self::pans(model, t) {
                        return Some(Hit::Pan);
                    }
                    let hits = [Hit::Mute, Hit::Solo, Hit::Arm];
                    match buttons.iter().position(|b| b.contains(pos)) {
                        Some(2) if !t.kind.has_clips() => return None,
                        Some(i) => hits[i],
                        None => return None,
                    }
                }
                Block::Sends => {
                    let (lines, shown) = Self::send_rects(r, t.sends.len());
                    match lines.iter().position(|l| l.contains(pos)) {
                        Some(i) if i < shown => Hit::Send(i),
                        Some(_) if shown == t.sends.len() => Hit::AddSend,
                        _ => return None,
                    }
                }
            });
        }
        None
    }

    /// Whether the channel block's pan reads as one position to type.
    fn pans(model: &Session, t: &Track) -> bool {
        t.kind.has_audio()
            && !matches!(t.layout, faderframe_core::ChannelLayout::Surround(_))
            && !model.project().dual_panned(t)
            && model.project().surround_panned(t).is_none()
    }

    /// The gap a dragged card would land in (between which cards).
    fn gap_at(&self, x: f32, t: &Track, devices: &[Device], size: Size) -> usize {
        self.layout(t, devices, size)
            .iter()
            .filter(|(b, r)| matches!(b, Block::Device(_)) && r.center().x < x)
            .count()
    }

    fn press(
        &mut self,
        pos: Point,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let (Some(t), Some(hit)) = (Self::track(model), self.hit(pos, size, model)) else {
            return false;
        };
        let devices = chain::devices(model, t);
        let rect_of = |b: Block| {
            self.layout(t, &devices, size)
                .into_iter()
                .find(|(x, _)| *x == b)
                .map(|(_, r)| r)
        };
        match hit {
            Hit::Add => cx.emit(chain::add(model, t)),
            Hit::Input => {
                if chain::has_input(t) {
                    cx.request(MixerView::input_menu(model, t, pos));
                }
            }
            Hit::Output => cx.request(if t.kind == TrackKind::Midi {
                MixerView::plays_menu(model, t, pos)
            } else {
                MixerView::output_menu(model, t, pos)
            }),
            Hit::MidiOut => cx.request(MixerView::midi_out_menu(model, t, pos)),
            Hit::Bypass(i) => cx.emit(chain::toggle_bypass(t.id, &devices[i])),
            Hit::Remove(i) => {
                if let Some(a) = chain::remove(t, &devices[i]) {
                    cx.emit(a);
                }
            }
            Hit::Device(i) => {
                let d = &devices[i];
                if mods.alt {
                    if let Some(a) = chain::remove(t, d) {
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
            Hit::Volume => {
                if let Some(r) = rect_of(Block::Channel) {
                    cx.request(level_request(model, t, Self::channel_rects(r).0));
                }
            }
            Hit::Pan => {
                if let Some(r) = rect_of(Block::Channel) {
                    cx.request(pan_request(model, t, Self::channel_rects(r).1));
                }
            }
            Hit::Mute => cx.emit(Action::Edit(Command::SetTrackMute {
                track: t.id,
                on: !t.mute,
            })),
            Hit::Solo => cx.emit(Action::Edit(Command::SetTrackSolo {
                track: t.id,
                on: !t.solo,
            })),
            Hit::Arm => cx.emit(Action::Edit(Command::SetTrackRecordArm {
                track: t.id,
                on: !t.record_arm,
            })),
            Hit::Send(i) => cx.request(MixerView::send_menu(model, t, i, pos)),
            Hit::AddSend => cx.request(MixerView::send_menu(model, t, t.sends.len(), pos)),
        }
        cx.redraw();
        true
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
        let gap = self.gap_at(pos.x, t, &devices, size);
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
            Hit::Device(i) | Hit::Bypass(i) | Hit::Remove(i) => {
                Some(device_menu(model, t, &devices[i], pos))
            }
            Hit::Add => Some(MixerView::insert_menu(model, t, t.inserts.len(), pos)),
            Hit::Input if chain::has_input(t) => Some(MixerView::input_menu(model, t, pos)),
            Hit::Output => Some(if t.kind == TrackKind::Midi {
                MixerView::plays_menu(model, t, pos)
            } else {
                MixerView::output_menu(model, t, pos)
            }),
            Hit::MidiOut => Some(MixerView::midi_out_menu(model, t, pos)),
            Hit::Send(i) => Some(MixerView::send_menu(model, t, i, pos)),
            _ => Some(MixerView::track_menu(model, t, pos)),
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

    fn paint_header(
        &self,
        p: &mut dyn Painter,
        size: Size,
        t: &Track,
        devices: &[Device],
        model: &Session,
    ) {
        let th = &self.theme;
        p.fill(Rect::new(0.0, 0.0, size.w, HEADER_H), th.piano.toolbar);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        let color = faderframe_view_mixer::track_color(t.color);
        p.fill_rounded(
            Rect::new(8.0, 8.0, 6.0, HEADER_H - 16.0),
            2.0,
            &Paint::Solid(color),
        );
        let name = TextStyle::new(th.fonts.normal, th.ui.text).bold();
        let w = p.text_width(&t.name, &name).min(size.w * 0.5);
        p.text(&t.name, Rect::new(20.0, 0.0, w + 4.0, HEADER_H), &name);
        let total = chain::total_latency(devices);
        let mut summary = format!(
            "{} · {}",
            t.kind.label(),
            match devices.len() {
                0 => "no devices".to_string(),
                1 => "1 device".to_string(),
                n => format!("{n} devices"),
            }
        );
        if total > 0 {
            summary += &format!(
                " · {} latency",
                chain::latency_text(total, model.project().sample_rate)
            );
        }
        let n = chain::selected_count(model);
        if n > 1 {
            summary += &format!(" · 1 of {n} selected");
        }
        p.text(
            &summary,
            Rect::new(30.0 + w, 0.0, size.w - w - 40.0, HEADER_H),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
    }

    fn block_frame(&self, p: &mut dyn Painter, r: Rect, title: &str, hot: bool) {
        let th = &self.theme;
        p.fill_rounded(r, 6.0, &Paint::Solid(th.ui.surface));
        p.stroke_rounded(
            r,
            6.0,
            1.0,
            if hot {
                th.ui.accent.with_alpha(0.7)
            } else {
                th.ui.border
            },
        );
        p.text(
            title,
            Rect::new(r.x + 8.0, r.y + 4.0, r.w - 16.0, 14.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_faint).bold(),
        );
    }

    fn line(&self, p: &mut dyn Painter, r: Rect, text: &str, color: Color, hot: bool) {
        let th = &self.theme;
        if hot {
            p.fill_rounded(r, 3.0, &Paint::Solid(th.ui.text.with_alpha(0.08)));
        }
        p.text(
            text,
            r.inset_xy(3.0, 0.0),
            &TextStyle::new(th.fonts.small, color),
        );
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

    fn paint_card(
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
            6.0,
            &Paint::Solid(if lifted {
                th.ui.surface_alt
            } else {
                th.ui.surface
            }),
        );
        p.stroke_rounded(
            r,
            6.0,
            1.0,
            if d.failed {
                c.led.record
            } else if hot {
                th.ui.accent.with_alpha(0.7)
            } else {
                th.ui.border
            },
        );
        let stripe = self.role_color(d.role);
        p.fill_rounded(
            Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 3.0),
            1.5,
            &Paint::Solid(if d.bypass {
                stripe.with_alpha(0.3)
            } else {
                stripe
            }),
        );
        let (power, remove) = Self::card_buttons(r);
        faderframe_ui_canvas::controls::led_button(p, power, "⏻", !d.bypass, c.led.monitor, th);
        if chain::remove(t, d).is_some() {
            let bg = if self.hover == Some(Hit::Remove(i)) {
                th.ui.text.with_alpha(0.14)
            } else {
                th.ui.text.with_alpha(0.05)
            };
            p.fill_rounded(remove, 3.0, &Paint::Solid(bg));
            p.text(
                "×",
                remove,
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
        }
        let ink = if d.bypass {
            th.ui.text_faint
        } else {
            th.ui.text
        };
        p.text(
            &d.name,
            Rect::new(
                power.right() + 6.0,
                r.y + 5.0,
                remove.x - power.right() - 8.0,
                20.0,
            ),
            &TextStyle::new(th.fonts.small, ink).bold(),
        );
        let dim = TextStyle::new(th.fonts.tiny, th.ui.text_dim);
        let mut y = r.y + 28.0;
        let tag = if d.depth > 0 {
            format!("IN CHAIN · {}", d.format)
        } else if d.role == Role::Container {
            format!(
                "{} · {} chain{}",
                d.role.tag(),
                d.chains,
                if d.chains == 1 { "" } else { "s" }
            )
        } else {
            format!("{} · {}", d.role.tag(), d.format)
        };
        p.text(&tag, Rect::new(r.x + 8.0, y, r.w - 16.0, 14.0), &dim);
        y += 16.0;
        for extra in [
            d.program.as_ref().map(|p| format!("♪ {p}")),
            d.sidechain.as_ref().map(|k| format!("⟵ key: {k}")),
            d.bypass.then(|| "bypassed".to_string()),
        ]
        .into_iter()
        .flatten()
        {
            if y + 14.0 > r.bottom() - 18.0 {
                break;
            }
            p.text(&extra, Rect::new(r.x + 8.0, y, r.w - 16.0, 14.0), &dim);
            y += 15.0;
        }
        // Badges along the foot: failure, latency, gain reduction.
        let mut x = r.x + 8.0;
        let foot = r.bottom() - 18.0;
        let badge = |p: &mut dyn Painter, x: &mut f32, text: &str, color: Color| {
            let style = TextStyle::new(th.fonts.tiny, color).bold();
            let w = p.text_width(text, &style) + 8.0;
            let b = Rect::new(*x, foot, w, 13.0);
            p.fill_rounded(b, 3.0, &Paint::Solid(color.with_alpha(0.16)));
            p.text(text, b, &style.center());
            *x += w + 4.0;
        };
        if d.failed {
            badge(p, &mut x, "⚠ STOPPED", c.led.record);
        }
        if let Some(l) = d.latency {
            badge(
                p,
                &mut x,
                &chain::latency_text(l, model.project().sample_rate),
                th.ui.text_dim,
            );
        }
        if let Some(gr) = d.reduction.filter(|g| *g > 0.05) {
            badge(p, &mut x, &format!("GR {gr:.1}"), th.device.reduction);
        }
    }

    fn paint_arrow(&self, p: &mut dyn Painter, x: f32, y: f32) {
        let th = &self.theme;
        let col = th.ui.text_faint;
        p.hline(x + 4.0, x + GAP - 6.0, y, col);
        let mut path = faderframe_ui_canvas::Path::new();
        path.move_to(Point::new(x + GAP - 4.0, y))
            .line_to(Point::new(x + GAP - 9.0, y - 4.0))
            .line_to(Point::new(x + GAP - 9.0, y + 4.0))
            .close();
        p.fill_path(&path, col);
    }

    fn paint_channel(&self, p: &mut dyn Painter, r: Rect, t: &Track, model: &Session) {
        let th = &self.theme;
        let c = &th.console;
        let (volume, pan, buttons) = Self::channel_rects(r);
        self.block_frame(p, r, "CHANNEL", false);
        if t.kind.has_audio() || t.kind == TrackKind::Vca {
            let db = model.shown_volume_db(t);
            self.line(
                p,
                volume,
                &format!("Level  {} dB", format_db(db)),
                th.ui.text,
                self.hover == Some(Hit::Volume),
            );
        }
        let pan_text = if !t.kind.has_audio() {
            String::new()
        } else if let faderframe_core::ChannelLayout::Surround(f) = t.layout {
            format!("Bed  {}", f.name())
        } else if model.project().surround_panned(t).is_some() {
            "Pan  surround".into()
        } else if model.project().dual_panned(t) {
            let d = model.shown_dual_pan(t);
            format!("Pan  {} / {}", format_pan(d.left), format_pan(d.right))
        } else {
            format!("Pan  {}", format_pan(model.shown_pan(t)))
        };
        if !pan_text.is_empty() {
            self.line(p, pan, &pan_text, th.ui.text, self.hover == Some(Hit::Pan));
        }
        faderframe_ui_canvas::controls::led_button(
            p,
            buttons[0],
            "M",
            model.shown_mute(t),
            c.led.mute,
            th,
        );
        faderframe_ui_canvas::controls::led_button(p, buttons[1], "S", t.solo, c.led.solo, th);
        if t.kind.has_clips() {
            faderframe_ui_canvas::controls::led_button(
                p,
                buttons[2],
                "R",
                t.record_arm,
                c.led.record,
                th,
            );
        }
    }

    fn paint_sends(&self, p: &mut dyn Painter, r: Rect, t: &Track, model: &Session) {
        let th = &self.theme;
        self.block_frame(p, r, "SENDS", false);
        let (lines, shown) = Self::send_rects(r, t.sends.len());
        for (i, l) in lines.iter().enumerate() {
            let hot = self.hover == Some(Hit::Send(i))
                || (i == shown && self.hover == Some(Hit::AddSend));
            match t.sends.get(i).filter(|_| i < shown) {
                Some(s) => {
                    let name = model
                        .project()
                        .track(s.target)
                        .map_or_else(|| "?".to_string(), |d| d.name.clone());
                    let tap = match s.tap {
                        faderframe_project::SendTap::PreFx => " pre-FX",
                        faderframe_project::SendTap::PreFader => " pre",
                        faderframe_project::SendTap::PostFader => "",
                    };
                    let text = format!("→ {name}  {}{tap}", format_db(model.shown_send_db(t, s)));
                    self.line(
                        p,
                        *l,
                        &text,
                        if s.enabled {
                            th.ui.text
                        } else {
                            th.ui.text_faint
                        },
                        hot,
                    );
                }
                None if shown == t.sends.len() => {
                    self.line(p, *l, "+ Send", th.ui.text_dim, hot);
                }
                None => self.line(
                    p,
                    *l,
                    &format!("+{} more (the mixer)", t.sends.len() - shown),
                    th.ui.text_faint,
                    false,
                ),
            }
        }
    }

    fn paint_drop(&self, p: &mut dyn Painter, t: &Track, devices: &[Device], size: Size) {
        let Some(d) = self.drag.filter(|d| d.moved) else {
            return;
        };
        let Some(device) = devices.get(d.device) else {
            return;
        };
        let th = &self.theme;
        let gap = self.gap_at(d.pos.x, t, devices, size);
        let layout = self.layout(t, devices, size);
        let cards: Vec<Rect> = layout
            .iter()
            .filter(|(b, _)| matches!(b, Block::Device(_)))
            .map(|(_, r)| *r)
            .collect();
        let movable =
            chain::drop_index(devices, device, gap).is_some_and(|i| device.insert != Some(i));
        let (y, h) = Self::band(size);
        let x = match (gap, cards.first(), cards.get(gap.wrapping_sub(1))) {
            (0, Some(first), _) => first.x - GAP / 2.0,
            (_, _, Some(prev)) => prev.right() + GAP / 2.0,
            _ => return,
        };
        if movable {
            p.fill(Rect::new(x - 1.5, y, 3.0, h), th.ui.accent);
        }
        let label = format!("{} {}", if movable { "Move" } else { "Keep" }, device.name);
        let style = TextStyle::new(th.fonts.small, th.ui.text);
        let w = p.text_width(&label, &style) + 14.0;
        let r = Rect::new(d.pos.x + 12.0, d.pos.y + 6.0, w, 20.0);
        p.fill_rounded(r, 4.0, &Paint::Solid(th.ui.surface_alt));
        p.stroke_rounded(r, 4.0, 1.0, th.ui.accent.with_alpha(0.8));
        p.text(&label, r, &style.center());
    }
}

/// A device right-clicked: the mixer's insert menu for an insert, the
/// input-stage menu for the input stage, its own short menu inside a
/// container.
pub(crate) fn device_menu(
    model: &Session,
    t: &Track,
    d: &Device,
    at: Point,
) -> HostRequest<Action> {
    if let Some(i) = d.insert {
        return MixerView::insert_menu(model, t, i, at);
    }
    if d.role == Role::InputStage {
        return MixerView::stage_menu(model, t, at);
    }
    let mut items = vec![
        MenuItem::disabled(d.name.clone()),
        MenuItem::new("Show Editor", chain::open(t.id, d)).separated(),
        MenuItem::new(
            if d.bypass { "Enable" } else { "Bypass" },
            chain::toggle_bypass(t.id, d),
        ),
    ];
    if let Some(a) = chain::remove(t, d) {
        items.push(MenuItem::new("Remove", a).separated());
    }
    HostRequest::ContextMenu { at, items }
}

/// Type the track's level (dB).
pub(crate) fn level_request(model: &Session, t: &Track, at: Rect) -> HostRequest<Action> {
    let track = t.id;
    HostRequest::TextInput {
        at,
        initial: format_db(model.shown_volume_db(t)),
        commit: Box::new(move |text| {
            let v = text.trim().trim_end_matches("dB").trim();
            let db = if v.eq_ignore_ascii_case("-inf") || v == "-∞" {
                faderframe_core::gain::SILENCE_DB
            } else {
                v.parse::<f32>().ok().filter(|v| v.is_finite())?
            };
            Some(Action::Edit(Command::SetTrackVolume { track, db }))
        }),
    }
}

/// Type the track's pan ("L 30", "C", "R 100").
pub(crate) fn pan_request(model: &Session, t: &Track, at: Rect) -> HostRequest<Action> {
    let track = t.id;
    HostRequest::TextInput {
        at,
        initial: format_pan(model.shown_pan(t)),
        commit: Box::new(move |text| {
            parse_pan(text).map(|pan| Action::Edit(Command::SetTrackPan { track, pan }))
        }),
    }
}

impl CanvasView<Session, Action> for ChannelOverviewView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, _theme: &Theme) {
        let th = self.theme.clone();
        p.fill(Rect::from_size(size), th.ui.background);
        let Some(t) = Self::track(model) else {
            p.fill(Rect::new(0.0, 0.0, size.w, HEADER_H), th.piano.toolbar);
            p.text(
                "Select a track to see its signal path: input, devices, channel, sends, output",
                Rect::new(0.0, HEADER_H, size.w, size.h - HEADER_H),
                &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
            );
            return;
        };
        let devices = chain::devices(model, t);
        let max = (Self::content_w(t, &devices) - size.w).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max);
        self.paint_header(p, size, t, &devices, model);
        p.push_clip(Rect::new(0.0, HEADER_H, size.w, size.h - HEADER_H));
        let (y, h) = Self::band(size);
        let layout = self.layout(t, &devices, size);
        for (k, (b, r)) in layout.iter().enumerate() {
            if r.right() < 0.0 || r.x > size.w {
                continue;
            }
            match *b {
                Block::Input => {
                    self.block_frame(p, *r, "INPUT", self.hover == Some(Hit::Input));
                    let text = chain::input_label(model, t);
                    p.text(
                        &text,
                        Rect::new(r.x + 8.0, r.y + 22.0, r.w - 16.0, (r.h - 30.0).max(14.0)),
                        &TextStyle::new(th.fonts.small, th.ui.text)
                            .valign(faderframe_ui_canvas::Align::Start),
                    );
                }
                Block::Device(i) => self.paint_card(p, i, *r, &devices[i], t, model),
                Block::Add => {
                    let hot = self.hover == Some(Hit::Add);
                    p.fill_rounded(
                        *r,
                        6.0,
                        &Paint::Solid(if hot {
                            th.ui.accent.with_alpha(0.25)
                        } else {
                            th.ui.surface
                        }),
                    );
                    p.stroke_rounded(*r, 6.0, 1.0, th.ui.border);
                    p.text(
                        "+",
                        *r,
                        &TextStyle::new(
                            th.fonts.large,
                            if hot { th.ui.text } else { th.ui.text_dim },
                        )
                        .center(),
                    );
                }
                Block::Channel => self.paint_channel(p, *r, t, model),
                Block::Sends => self.paint_sends(p, *r, t, model),
                Block::Output => {
                    let title = if t.kind == TrackKind::Midi {
                        "PLAYS"
                    } else {
                        "OUTPUT"
                    };
                    self.block_frame(p, *r, title, self.hover == Some(Hit::Output));
                    p.text(
                        &chain::output_label(model, t),
                        Rect::new(r.x + 8.0, r.y + 22.0, r.w - 16.0, 30.0),
                        &TextStyle::new(th.fonts.small, th.ui.text)
                            .valign(faderframe_ui_canvas::Align::Start),
                    );
                    if t.kind == TrackKind::Midi {
                        let out = chain::checked_label(&model.midi_output_choices(t.id));
                        self.line(
                            p,
                            Self::midi_out_rect(*r),
                            &format!("MIDI out: {out}"),
                            th.ui.text_dim,
                            self.hover == Some(Hit::MidiOut),
                        );
                    }
                }
            }
            if k + 1 < layout.len() {
                self.paint_arrow(p, r.right(), y + h / 2.0);
            }
        }
        self.paint_drop(p, t, &devices, size);
        p.pop_clip();
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
                ..
            } => self.press(pos, modifiers, size, model, cx),
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => self.secondary(pos, size, model, cx),
            ViewEvent::PointerMove { pos, .. } if self.drag.is_some() => {
                if let Some(d) = &mut self.drag {
                    d.moved = d.moved || pos.distance(d.origin) >= 4.0;
                    d.pos = pos;
                    if d.moved {
                        cx.set_cursor(Cursor::Grabbing);
                        // Near an edge the row scrolls along.
                        if pos.x < 30.0 {
                            self.scroll_x = (self.scroll_x - 8.0).max(0.0);
                        } else if pos.x > size.w - 30.0 {
                            self.scroll_x += 8.0;
                        }
                    }
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let hover = self.hit(pos, size, model);
                if hover != self.hover {
                    self.hover = hover;
                    cx.set_cursor(match hover {
                        None => Cursor::Default,
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
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll {
                dx, dy, precise, ..
            } => {
                let step = if precise { 1.0 } else { 40.0 };
                self.scroll_x = (self.scroll_x + (dx + dy) * step).max(0.0);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn wants_frames(&self, model: &Session) -> bool {
        // Levels and gain reduction move while it plays.
        model.is_animating()
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let t = Self::track(model)?;
        let devices = chain::devices(model, t);
        Some(match self.hit(pos, size, model)? {
            Hit::Add => {
                "Add a device after the last (the plugin browser) · Right-click: the quick list"
                    .into()
            }
            Hit::Input => format!("Input: {} · Click to choose", chain::input_label(model, t)),
            Hit::Output => format!("{} · Click to choose", chain::output_label(model, t)),
            Hit::MidiOut => "The external MIDI device this track plays · Click to choose".into(),
            Hit::Device(i) => {
                let d = &devices[i];
                let mut tip = format!(
                    "{} ({}, {})\nClick: editor · Ctrl+click: bypass · Alt+click: remove",
                    d.name,
                    d.role.tag().to_lowercase(),
                    d.format
                );
                if d.insert.is_some() {
                    tip += " · Drag: move it in the chain";
                }
                tip += " · Right-click: presets, sidechain, more";
                tip
            }
            Hit::Bypass(i) => format!(
                "{} {} · Click to {}",
                devices[i].name,
                if devices[i].bypass {
                    "is bypassed"
                } else {
                    "is on"
                },
                if devices[i].bypass {
                    "switch it on"
                } else {
                    "bypass it"
                }
            ),
            Hit::Remove(i) => format!("Remove {}", devices[i].name),
            Hit::Volume => "The track's level · Click to type it".into(),
            Hit::Pan => "The track's pan · Click to type it".into(),
            Hit::Mute => "Mute".into(),
            Hit::Solo => "Solo".into(),
            Hit::Arm => "Record arm".into(),
            Hit::Send(_) => "Its tap, pan, on/off, removal, MIDI learn".into(),
            Hit::AddSend => "Send to a bus or aux".into(),
        })
    }

    fn min_size(&self) -> Size {
        Size::new(320.0, 120.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Horizontal {
            return None;
        }
        let t = Self::track(model)?;
        let devices = chain::devices(model, t);
        Some(ScrollInfo {
            content: Self::content_w(t, &devices),
            viewport: size.w,
            offset: self.scroll_x,
            start: 0.0,
            end: 0.0,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Horizontal {
            self.scroll_x = offset.max(0.0);
        }
    }

    fn accessible(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        let Some(t) = Self::track(model) else {
            return Vec::new();
        };
        let devices = chain::devices(model, t);
        let id = t.id.0;
        let mut root = AccessNode::new(
            access_id(&[NODE_VIEW, id]),
            AccessRole::Group,
            format!("Channel overview of {}", t.name),
        )
        .at(Rect::from_size(size));
        for (b, r) in self.layout(t, &devices, size) {
            match b {
                Block::Input => {
                    root = root.child(
                        AccessNode::new(
                            access_id(&[NODE_IO, id, 0]),
                            AccessRole::Label,
                            format!("Input: {}", chain::input_label(model, t)),
                        )
                        .at(r),
                    );
                }
                Block::Output => {
                    root = root.child(
                        AccessNode::new(
                            access_id(&[NODE_IO, id, 1]),
                            AccessRole::Label,
                            format!("Output: {}", chain::output_label(model, t)),
                        )
                        .at(r),
                    );
                }
                Block::Add => {
                    root = root.child(
                        AccessNode::new(
                            access_id(&[NODE_ADD, id]),
                            AccessRole::Button,
                            "Add device",
                        )
                        .at(r)
                        .on_activate(chain::add(model, t)),
                    );
                }
                Block::Device(i) => {
                    let (power, remove) = Self::card_buttons(r);
                    root = root.child(device_node(t, i, &devices[i], r, power, remove));
                }
                Block::Channel => {
                    let (_, _, buttons) = Self::channel_rects(r);
                    let mut group = AccessNode::new(
                        access_id(&[NODE_CHANNEL, id]),
                        AccessRole::Group,
                        format!("Channel, level {} dB", format_db(model.shown_volume_db(t))),
                    )
                    .at(r);
                    let toggles = [
                        (
                            "Mute",
                            model.shown_mute(t),
                            Command::SetTrackMute {
                                track: t.id,
                                on: !t.mute,
                            },
                        ),
                        (
                            "Solo",
                            t.solo,
                            Command::SetTrackSolo {
                                track: t.id,
                                on: !t.solo,
                            },
                        ),
                    ];
                    for (k, (label, on, cmd)) in toggles.into_iter().enumerate() {
                        group = group.child(
                            AccessNode::new(
                                access_id(&[NODE_CHANNEL, id, k as u64 + 1]),
                                AccessRole::ToggleButton,
                                label,
                            )
                            .checked(on)
                            .at(buttons[k])
                            .on_activate(Action::Edit(cmd)),
                        );
                    }
                    root = root.child(group);
                }
                Block::Sends => {}
            }
        }
        vec![root]
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Channel Overview".into())
    }
}

/// A device for screen readers: its name, role and state; Enter opens
/// its editor; its power button and its removal as controls of their own
/// (`power`, `remove`: where they are).
pub(crate) fn device_node(
    t: &Track,
    i: usize,
    d: &Device,
    r: Rect,
    power: Rect,
    remove: Rect,
) -> AccessNode<Action> {
    let mut label = format!(
        "Device {}: {}, {}",
        i + 1,
        d.name,
        d.role.tag().to_lowercase()
    );
    if d.bypass {
        label += ", bypassed";
    }
    if d.failed {
        label += ", stopped working";
    }
    let mut node = AccessNode::new(
        access_id(&[NODE_DEVICE, d.plugin.0]),
        AccessRole::Button,
        label,
    )
    .described("Enter opens its editor")
    .at(r)
    .on_activate(chain::open(t.id, d))
    .child(
        AccessNode::new(
            access_id(&[NODE_POWER, d.plugin.0]),
            AccessRole::ToggleButton,
            format!("{} on", d.name),
        )
        .checked(!d.bypass)
        .at(power)
        .on_activate(chain::toggle_bypass(t.id, d)),
    );
    if let Some(a) = chain::remove(t, d) {
        node = node.child(
            AccessNode::new(
                access_id(&[NODE_REMOVE, d.plugin.0]),
                AccessRole::Button,
                format!("Remove {}", d.name),
            )
            .at(remove)
            .on_activate(a),
        );
    }
    node
}
