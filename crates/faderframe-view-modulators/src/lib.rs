//! Modulators: the selected track's modulators side by side, each a card —
//! its light (on/off) and name (double-click renames), Map (then move a
//! control on the track's devices to have it moved) and remove; a live
//! display (the shape and where its output is, a step sequence drawn with
//! the mouse, a macro's value dragged); its settings as knobs; and its
//! targets, each with a depth bar (drag; double-click: none). Add makes a
//! new one.
//!
//! The wheel over a card turns the knob or depth under it and never
//! scrolls (Shift+wheel or a sideways swipe moves along the cards).

#![forbid(unsafe_code)]

use faderframe_core::{ModulatorId, TrackId};
use faderframe_project::modulation::{
    FollowSource, LfoShape, MAX_STEPS, ModRate, ModRoute, ModSource, Modulator, steps_at,
};
use faderframe_project::{Track, TrackKind};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::controls::{self, KnobLook};
use faderframe_ui_canvas::{
    CanvasView, EventCx, FontFamily, HostRequest, MenuItem, Modifiers, Paint, Painter, Path, Point,
    PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 30.0;
const CARD_W: f32 = 264.0;
const GAP: f32 = 8.0;
const PAD: f32 = 8.0;
const TITLE_H: f32 = 24.0;
const DISPLAY_H: f32 = 50.0;
const CONTROLS_H: f32 = 54.0;
const ROUTE_H: f32 = 20.0;
const ADD_W: f32 = 70.0;
const BUTTONS_W: f32 = 66.0;
/// Free rates (Hz) the rate knob spans.
const HZ: (f32, f32) = (0.02, 40.0);

/// A knob of a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctl {
    Rate,
    Phase,
    Glide,
    Smooth,
    Attack,
    Decay,
    Sustain,
    Release,
    Gain,
    Value,
}

impl Ctl {
    fn label(self) -> &'static str {
        match self {
            Ctl::Rate => "RATE",
            Ctl::Phase => "PHASE",
            Ctl::Glide => "GLIDE",
            Ctl::Smooth => "SMOOTH",
            Ctl::Attack => "ATTACK",
            Ctl::Decay => "DECAY",
            Ctl::Sustain => "SUSTAIN",
            Ctl::Release => "RELEASE",
            Ctl::Gain => "GAIN",
            Ctl::Value => "VALUE",
        }
    }

    fn bipolar(self) -> bool {
        self == Ctl::Gain
    }
}

/// A button of a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Btn {
    Shape,
    Sync,
    Fewer,
    More,
    Source,
}

fn knobs(src: &ModSource) -> &'static [Ctl] {
    match src {
        ModSource::Lfo { .. } => &[Ctl::Rate, Ctl::Phase],
        ModSource::Steps { .. } => &[Ctl::Rate, Ctl::Glide],
        ModSource::Random { .. } => &[Ctl::Rate, Ctl::Smooth],
        ModSource::Follower { .. } => &[Ctl::Attack, Ctl::Release, Ctl::Gain],
        ModSource::Macro { .. } => &[Ctl::Value],
        ModSource::NoteEnvelope { .. } => &[Ctl::Attack, Ctl::Decay, Ctl::Sustain, Ctl::Release],
        ModSource::NoteLfo { .. } => &[Ctl::Rate, Ctl::Phase],
        ModSource::Velocity | ModSource::Key | ModSource::NoteRandom => &[],
    }
}

fn buttons(src: &ModSource) -> &'static [Btn] {
    match src {
        ModSource::Lfo { .. } => &[Btn::Shape, Btn::Sync],
        ModSource::Steps { .. } => &[Btn::Sync, Btn::Fewer, Btn::More],
        ModSource::Random { .. } => &[Btn::Sync],
        ModSource::Follower { .. } => &[Btn::Source],
        ModSource::NoteLfo { .. } => &[Btn::Shape, Btn::Sync],
        _ => &[],
    }
}

fn rate_of(src: &ModSource) -> Option<ModRate> {
    match src {
        ModSource::Lfo { rate, .. }
        | ModSource::Steps { rate, .. }
        | ModSource::Random { rate, .. }
        | ModSource::NoteLfo { rate, .. } => Some(*rate),
        _ => None,
    }
}

fn set_rate(src: &mut ModSource, r: ModRate) {
    if let ModSource::Lfo { rate, .. }
    | ModSource::Steps { rate, .. }
    | ModSource::Random { rate, .. }
    | ModSource::NoteLfo { rate, .. } = src
    {
        *rate = r;
    }
}

/// The synced divisions, slowest first.
fn division_of(beats: f64) -> usize {
    ModRate::DIVISIONS
        .iter()
        .enumerate()
        .min_by(|a, b| {
            (a.1.1 - beats)
                .abs()
                .partial_cmp(&(b.1.1 - beats).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map_or(0, |(i, _)| i)
}

fn log_norm(v: f32, (lo, hi): (f32, f32)) -> f32 {
    ((v.max(lo) / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0)
}

fn log_from(n: f32, (lo, hi): (f32, f32)) -> f32 {
    lo * (hi / lo).powf(n.clamp(0.0, 1.0))
}

const ATTACK: (f32, f32) = (0.1, 500.0);
const RELEASE: (f32, f32) = (5.0, 5_000.0);
const DECAY: (f32, f32) = (5.0, 5_000.0);

/// A knob's position (0..1).
pub fn ctl_get(src: &ModSource, ctl: Ctl) -> f32 {
    match (ctl, src) {
        (Ctl::Rate, s) => match rate_of(s) {
            Some(ModRate::Sync { beats }) => {
                division_of(beats) as f32 / (ModRate::DIVISIONS.len() - 1) as f32
            }
            Some(ModRate::Hz { hz }) => log_norm(hz, HZ),
            None => 0.0,
        },
        (Ctl::Phase, ModSource::Lfo { phase, .. } | ModSource::NoteLfo { phase, .. }) => *phase,
        (Ctl::Attack, ModSource::NoteEnvelope { attack_ms, .. }) => log_norm(*attack_ms, ATTACK),
        (Ctl::Decay, ModSource::NoteEnvelope { decay_ms, .. }) => log_norm(*decay_ms, DECAY),
        (Ctl::Sustain, ModSource::NoteEnvelope { sustain, .. }) => *sustain,
        (Ctl::Release, ModSource::NoteEnvelope { release_ms, .. }) => {
            log_norm(*release_ms, RELEASE)
        }
        (Ctl::Glide, ModSource::Steps { glide, .. }) => *glide,
        (Ctl::Smooth, ModSource::Random { smooth, .. }) => *smooth,
        (Ctl::Attack, ModSource::Follower { attack_ms, .. }) => log_norm(*attack_ms, ATTACK),
        (Ctl::Release, ModSource::Follower { release_ms, .. }) => log_norm(*release_ms, RELEASE),
        (Ctl::Gain, ModSource::Follower { gain_db, .. }) => (gain_db + 24.0) / 48.0,
        (Ctl::Value, ModSource::Macro { value }) => *value,
        _ => 0.0,
    }
    .clamp(0.0, 1.0)
}

/// Turn a knob to `n` (0..1).
pub fn ctl_set(src: &mut ModSource, ctl: Ctl, n: f32) {
    let n = n.clamp(0.0, 1.0);
    match (ctl, &mut *src) {
        (Ctl::Rate, s) => match rate_of(s) {
            Some(ModRate::Sync { .. }) => {
                let i = (n * (ModRate::DIVISIONS.len() - 1) as f32).round() as usize;
                let beats = ModRate::DIVISIONS[i.min(ModRate::DIVISIONS.len() - 1)].1;
                set_rate(s, ModRate::Sync { beats });
            }
            Some(ModRate::Hz { .. }) => set_rate(
                s,
                ModRate::Hz {
                    hz: (log_from(n, HZ) * 1000.0).round() / 1000.0,
                },
            ),
            None => {}
        },
        (Ctl::Phase, ModSource::Lfo { phase, .. } | ModSource::NoteLfo { phase, .. }) => *phase = n,
        (Ctl::Attack, ModSource::NoteEnvelope { attack_ms, .. }) => {
            *attack_ms = (log_from(n, ATTACK) * 10.0).round() / 10.0;
        }
        (Ctl::Decay, ModSource::NoteEnvelope { decay_ms, .. }) => {
            *decay_ms = log_from(n, DECAY).round();
        }
        (Ctl::Sustain, ModSource::NoteEnvelope { sustain, .. }) => {
            *sustain = (n * 100.0).round() / 100.0;
        }
        (Ctl::Release, ModSource::NoteEnvelope { release_ms, .. }) => {
            *release_ms = log_from(n, RELEASE).round();
        }
        (Ctl::Glide, ModSource::Steps { glide, .. }) => *glide = n,
        (Ctl::Smooth, ModSource::Random { smooth, .. }) => *smooth = n,
        (Ctl::Attack, ModSource::Follower { attack_ms, .. }) => {
            *attack_ms = (log_from(n, ATTACK) * 10.0).round() / 10.0;
        }
        (Ctl::Release, ModSource::Follower { release_ms, .. }) => {
            *release_ms = log_from(n, RELEASE).round();
        }
        (Ctl::Gain, ModSource::Follower { gain_db, .. }) => {
            *gain_db = (n * 48.0 - 24.0).round();
        }
        (Ctl::Value, ModSource::Macro { value }) => *value = (n * 1000.0).round() / 1000.0,
        _ => {}
    }
}

/// A knob's value as text.
pub fn ctl_text(src: &ModSource, ctl: Ctl) -> String {
    match (ctl, src) {
        (Ctl::Rate, s) => rate_of(s).map_or_else(String::new, ModRate::label),
        (Ctl::Phase, ModSource::Lfo { phase, .. } | ModSource::NoteLfo { phase, .. }) => {
            format!("{:.0}°", phase * 360.0)
        }
        (Ctl::Attack, ModSource::NoteEnvelope { attack_ms: v, .. })
        | (Ctl::Decay, ModSource::NoteEnvelope { decay_ms: v, .. })
        | (Ctl::Release, ModSource::NoteEnvelope { release_ms: v, .. }) => ms(*v),
        (Ctl::Sustain, ModSource::NoteEnvelope { sustain, .. }) => {
            format!("{:.0} %", sustain * 100.0)
        }
        (Ctl::Glide, ModSource::Steps { glide: v, .. })
        | (Ctl::Smooth, ModSource::Random { smooth: v, .. })
        | (Ctl::Value, ModSource::Macro { value: v }) => format!("{:.0} %", v * 100.0),
        (Ctl::Attack, ModSource::Follower { attack_ms, .. }) => ms(*attack_ms),
        (Ctl::Release, ModSource::Follower { release_ms, .. }) => ms(*release_ms),
        (Ctl::Gain, ModSource::Follower { gain_db, .. }) => format!("{gain_db:+.0} dB"),
        _ => String::new(),
    }
}

fn ms(v: f32) -> String {
    if v >= 1000.0 {
        format!("{:.2} s", v / 1000.0)
    } else if v >= 10.0 {
        format!("{v:.0} ms")
    } else {
        format!("{v:.1} ms")
    }
}

/// What a new modulator of the same kind has (double-click resets to it).
fn default_of(src: &ModSource, ctl: Ctl) -> f32 {
    ModSource::defaults()
        .iter()
        .find(|d| d.kind_label() == src.kind_label())
        .map_or(0.0, |d| ctl_get(d, ctl))
}

/// What is under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Add,
    Enable(ModulatorId),
    Name(ModulatorId),
    Map(ModulatorId),
    Remove(ModulatorId),
    Display(ModulatorId),
    Knob(ModulatorId, Ctl),
    Button(ModulatorId, Btn),
    Depth(ModulatorId, usize),
    Unroute(ModulatorId, usize),
    Route(ModulatorId),
}

/// Where a card's parts are.
#[derive(Clone, Debug)]
pub struct CardLayout {
    pub rect: Rect,
    pub enable: Rect,
    pub name: Rect,
    pub map: Rect,
    pub remove: Rect,
    pub display: Rect,
    pub knobs: Vec<(Ctl, Rect)>,
    pub buttons: Vec<(Btn, Rect)>,
    /// (route index, row, depth bar, remove).
    pub routes: Vec<(usize, Rect, Rect, Rect)>,
    /// Routes that do not fit.
    pub hidden: usize,
    pub add_route: Rect,
}

#[derive(Clone, Debug)]
enum DragKind {
    Knob {
        ctl: Ctl,
        start: f32,
    },
    Depth {
        route: usize,
        start: f32,
        width: f32,
    },
    Steps {
        last: Option<usize>,
    },
    Macro,
}

#[derive(Clone, Debug)]
struct Drag {
    track: TrackId,
    /// The modulator as being edited.
    m: Modulator,
    from: Point,
    kind: DragKind,
}

pub struct ModulatorsView {
    theme: Theme,
    scroll: f32,
    drag: Option<Drag>,
}

impl ModulatorsView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll: 0.0,
            drag: None,
        }
    }

    /// The track shown: the first selected one (in the editors' order).
    pub fn track(model: &Session) -> Option<&Track> {
        let sel = &model.selection.tracks;
        model
            .project()
            .folder_order()
            .into_iter()
            .find(|t| sel.contains(&t.id))
    }

    fn takes_modulators(t: &Track) -> bool {
        matches!(
            t.kind,
            TrackKind::Audio
                | TrackKind::Instrument
                | TrackKind::Bus
                | TrackKind::Aux
                | TrackKind::Master
        )
    }

    pub fn add_rect(size: Size) -> Rect {
        Rect::new(size.w - ADD_W - 8.0, 5.0, ADD_W, HEADER_H - 10.0)
    }

    fn content_w(count: usize) -> f32 {
        GAP + count as f32 * (CARD_W + GAP)
    }

    /// Card `i`'s layout for `m`.
    pub fn card(&self, i: usize, m: &Modulator, size: Size) -> CardLayout {
        let rect = Rect::new(
            GAP + i as f32 * (CARD_W + GAP) - self.scroll,
            HEADER_H + GAP,
            CARD_W,
            (size.h - HEADER_H - 2.0 * GAP).max(160.0),
        );
        let (x, y, w) = (rect.x, rect.y, rect.w);
        let enable = Rect::new(x + PAD, y + 6.0, 12.0, 12.0);
        let remove = Rect::new(rect.right() - PAD - 16.0, y + 4.0, 16.0, 16.0);
        let map = Rect::new(remove.x - 46.0, y + 4.0, 42.0, 16.0);
        let name = Rect::new(
            enable.right() + 6.0,
            y,
            map.x - enable.right() - 10.0,
            TITLE_H,
        );
        let display = Rect::new(x + PAD, y + TITLE_H + 2.0, w - 2.0 * PAD, DISPLAY_H);
        let row = Rect::new(x + PAD, display.bottom() + 6.0, w - 2.0 * PAD, CONTROLS_H);
        let bs = buttons(&m.source);
        let mut buttons_at = Vec::new();
        let bw = if bs.is_empty() { 0.0 } else { BUTTONS_W };
        let mut by = row.y + 4.0;
        for b in bs {
            match b {
                Btn::Fewer => buttons_at.push((*b, Rect::new(row.x, by, bw / 2.0 - 2.0, 18.0))),
                Btn::More => {
                    buttons_at.push((*b, Rect::new(row.x + bw / 2.0, by, bw / 2.0 - 2.0, 18.0)));
                    by += 22.0;
                }
                _ => {
                    buttons_at.push((*b, Rect::new(row.x, by, bw - 2.0, 18.0)));
                    by += 22.0;
                }
            }
        }
        let ks = knobs(&m.source);
        let kx = row.x + bw + if bw > 0.0 { 4.0 } else { 0.0 };
        let kw = (row.right() - kx) / ks.len().max(1) as f32;
        let knobs_at = ks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let cx = kx + kw * (i as f32 + 0.5);
                (*c, Rect::new(cx - 14.0, row.y + 12.0, 28.0, 28.0))
            })
            .collect();
        // Routes, then "+ Target" (always shown).
        let top = row.bottom() + 6.0;
        let room = ((rect.bottom() - PAD - top) / ROUTE_H).floor().max(1.0) as usize;
        let fit = m.routes.len().min(room.saturating_sub(1));
        let routes = (0..fit)
            .map(|r| {
                let line = Rect::new(x + PAD, top + r as f32 * ROUTE_H, w - 2.0 * PAD, ROUTE_H);
                let rm = Rect::new(line.right() - 14.0, line.y + 3.0, 14.0, 14.0);
                let depth = Rect::new(rm.x - 88.0, line.y + 4.0, 82.0, 12.0);
                (r, line, depth, rm)
            })
            .collect();
        let add_route = Rect::new(
            x + PAD,
            top + fit as f32 * ROUTE_H,
            w - 2.0 * PAD,
            ROUTE_H - 2.0,
        );
        CardLayout {
            rect,
            enable,
            name,
            map,
            remove,
            display,
            knobs: knobs_at,
            buttons: buttons_at,
            routes,
            hidden: m.routes.len() - fit,
            add_route,
        }
    }

    pub fn hit_test(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        let t = Self::track(model)?;
        if Self::takes_modulators(t) && Self::add_rect(size).contains(pos) {
            return Some(Hit::Add);
        }
        for (i, m) in t.modulators.iter().enumerate() {
            let c = self.card(i, m, size);
            if !c.rect.contains(pos) || pos.y < HEADER_H {
                continue;
            }
            let id = m.id;
            if c.enable.inset(-4.0).contains(pos) {
                return Some(Hit::Enable(id));
            }
            if c.remove.contains(pos) {
                return Some(Hit::Remove(id));
            }
            if c.map.contains(pos) {
                return Some(Hit::Map(id));
            }
            if c.name.contains(pos) {
                return Some(Hit::Name(id));
            }
            if c.display.contains(pos) {
                return Some(Hit::Display(id));
            }
            for (b, r) in &c.buttons {
                if r.contains(pos) {
                    return Some(Hit::Button(id, *b));
                }
            }
            for (k, r) in &c.knobs {
                if r.inset(-6.0).contains(pos) {
                    return Some(Hit::Knob(id, *k));
                }
            }
            for (r, _, depth, rm) in &c.routes {
                if rm.contains(pos) {
                    return Some(Hit::Unroute(id, *r));
                }
                if depth.inset(-3.0).contains(pos) {
                    return Some(Hit::Depth(id, *r));
                }
            }
            if c.add_route.contains(pos) {
                return Some(Hit::Route(id));
            }
            return None;
        }
        None
    }

    fn clamp(&mut self, count: usize, size: Size) {
        let max = (Self::content_w(count) - size.w).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn modulator(model: &Session, id: ModulatorId) -> Option<(TrackId, Modulator)> {
        let t = Self::track(model)?;
        let m = t.modulators.iter().find(|m| m.id == id)?;
        Some((t.id, m.clone()))
    }

    fn set(cx: &mut EventCx<'_, Action>, track: TrackId, m: Modulator) {
        cx.emit(Action::SetModulator {
            track,
            modulator: m,
        });
    }

    /// The step under `pos` in a step display, and the value there.
    fn step_at(display: Rect, n: usize, pos: Point) -> (usize, f32) {
        let i = (((pos.x - display.x) / display.w) * n as f32).floor();
        let i = (i.max(0.0) as usize).min(n.saturating_sub(1));
        let v = 1.0 - 2.0 * (pos.y - display.y) / display.h;
        (i, (v.clamp(-1.0, 1.0) * 100.0).round() / 100.0)
    }

    fn add_menu(track: TrackId) -> Vec<MenuItem<Action>> {
        let mut first_note = true;
        ModSource::defaults()
            .into_iter()
            .map(|source| {
                let notes = source.per_note();
                let mut item = MenuItem::new(
                    if notes {
                        format!("{} (per note)", source.kind_label())
                    } else {
                        source.kind_label().to_string()
                    },
                    Action::AddModulator { track, source },
                );
                item.separator_before = notes && std::mem::take(&mut first_note);
                item
            })
            .collect()
    }

    fn route_menu(model: &Session, track: TrackId, m: &Modulator) -> Vec<MenuItem<Action>> {
        let mut items = Vec::new();
        let mut group = String::new();
        let notes = m.source.per_note();
        for c in model.modulation_targets(track) {
            // Per-note modulators move what gets the notes.
            if notes && !c.takes_notes {
                continue;
            }
            let mut label = if c.group == "Track" {
                c.name.clone()
            } else {
                format!("{} · {}", c.group, c.name)
            };
            if notes && c.per_note {
                label.push_str(" (per voice)");
            }
            let routed = m.routes.iter().any(|r| r.target == c.target);
            let mut item = if routed {
                MenuItem::disabled(label).checked(true)
            } else {
                let mut with = m.clone();
                with.routes.push(ModRoute {
                    target: c.target,
                    depth: faderframe_session::modulators::DEFAULT_DEPTH,
                });
                MenuItem::new(
                    label,
                    Action::SetModulator {
                        track,
                        modulator: with,
                    },
                )
            };
            if c.group != group {
                item.separator_before = !group.is_empty();
                group = c.group.clone();
            }
            items.push(item);
        }
        if notes && items.is_empty() {
            items.push(MenuItem::disabled(
                "(no device on this track gets the notes)",
            ));
        } else if !notes && items.len() <= 2 {
            items.push(MenuItem::disabled("(no device parameters take modulation)"));
        }
        items
    }

    fn source_menu(model: &Session, track: TrackId, m: &Modulator) -> Vec<MenuItem<Action>> {
        let ModSource::Follower { source, .. } = m.source else {
            return Vec::new();
        };
        let with = |s: FollowSource| {
            let mut x = m.clone();
            if let ModSource::Follower { source, .. } = &mut x.source {
                *source = s;
            }
            Action::SetModulator {
                track,
                modulator: x,
            }
        };
        let mut items = vec![
            MenuItem::new("This Track's Input", with(FollowSource::Input))
                .checked(source == FollowSource::Input),
        ];
        for (i, t) in model
            .project()
            .tracks
            .iter()
            .filter(|t| {
                t.id != track
                    && matches!(
                        t.kind,
                        TrackKind::Audio | TrackKind::Instrument | TrackKind::Bus | TrackKind::Aux
                    )
            })
            .enumerate()
        {
            let s = FollowSource::Track { track: t.id };
            let mut item = MenuItem::new(t.name.clone(), with(s)).checked(source == s);
            item.separator_before = i == 0;
            items.push(item);
        }
        items
    }

    fn shape_menu(track: TrackId, m: &Modulator) -> Vec<MenuItem<Action>> {
        let (ModSource::Lfo { shape: now, .. } | ModSource::NoteLfo { shape: now, .. }) = m.source
        else {
            return Vec::new();
        };
        LfoShape::ALL
            .into_iter()
            .map(|shape| {
                let mut x = m.clone();
                if let ModSource::Lfo { shape: s, .. } | ModSource::NoteLfo { shape: s, .. } =
                    &mut x.source
                {
                    *s = shape;
                }
                MenuItem::new(
                    shape.label(),
                    Action::SetModulator {
                        track,
                        modulator: x,
                    },
                )
                .checked(shape == now)
            })
            .collect()
    }

    fn button_text(model: &Session, m: &Modulator, b: Btn) -> String {
        match (b, &m.source) {
            (Btn::Shape, ModSource::Lfo { shape, .. } | ModSource::NoteLfo { shape, .. }) => {
                format!("{} ▾", shape.label())
            }
            (Btn::Sync, s) => match rate_of(s) {
                Some(ModRate::Sync { .. }) => "Synced".into(),
                _ => "Free".into(),
            },
            (Btn::Fewer, _) => "−".into(),
            (Btn::More, _) => "+".into(),
            (Btn::Source, ModSource::Follower { source, .. }) => match source {
                FollowSource::Input => "Input ▾".into(),
                FollowSource::Track { track } => model
                    .project()
                    .track(*track)
                    .map_or_else(|| "(gone) ▾".into(), |t| format!("{} ▾", t.name)),
            },
            _ => String::new(),
        }
    }

    /// Synced ↔ free, keeping the speed at the tempo now.
    fn toggle_sync(model: &Session, m: &mut Modulator) {
        let bpm = model.project().timeline.tempo.bpm_at(model.playhead());
        match rate_of(&m.source) {
            Some(ModRate::Sync { beats }) => {
                let hz = ModRate::Sync { beats }.hz(bpm) as f32;
                set_rate(
                    &mut m.source,
                    ModRate::Hz {
                        hz: (hz.clamp(HZ.0, HZ.1) * 100.0).round() / 100.0,
                    },
                );
            }
            Some(ModRate::Hz { hz }) => {
                let beats = bpm / 60.0 / f64::from(hz.max(1e-3));
                let i = division_of(beats);
                set_rate(
                    &mut m.source,
                    ModRate::Sync {
                        beats: ModRate::DIVISIONS[i].1,
                    },
                );
            }
            None => {}
        }
    }

    fn paint_card(
        &self,
        p: &mut dyn Painter,
        c: &CardLayout,
        m: &Modulator,
        value: f32,
        learning: bool,
        model: &Session,
    ) {
        let th = &self.theme;
        let accent = th.ui.accent;
        let on = m.enabled;
        p.fill_rounded(c.rect, 5.0, &Paint::Solid(th.ui.surface));
        if learning {
            p.stroke_rounded(c.rect, 5.0, 2.0, accent);
        } else {
            p.stroke_rounded(c.rect, 5.0, 1.0, th.ui.border);
        }
        // Title: light, name, kind, Map, remove.
        let light = if on {
            accent
        } else {
            th.ui.text_faint.with_alpha(0.5)
        };
        p.circle(c.enable.center(), 5.0, light);
        if on {
            p.circle(c.enable.center(), 2.0, accent.lighten(0.5));
        }
        let text = if on { th.ui.text } else { th.ui.text_faint };
        p.text(
            &m.name,
            Rect::new(c.name.x, c.name.y, c.name.w, c.name.h),
            &TextStyle::new(th.fonts.normal, text).bold(),
        );
        controls::led_button(p, c.map, "MAP", learning, accent, th);
        p.text(
            "×",
            c.remove,
            &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
        );
        self.paint_display(p, c.display, m, value, learning);
        // Buttons and knobs.
        for (b, r) in &c.buttons {
            let lit = *b == Btn::Sync && matches!(rate_of(&m.source), Some(ModRate::Sync { .. }));
            p.fill_rounded(
                *r,
                3.0,
                &Paint::Solid(if lit {
                    accent.with_alpha(0.25)
                } else {
                    th.ui.background
                }),
            );
            p.stroke_rounded(*r, 3.0, 1.0, th.ui.border);
            p.text(
                &Self::button_text(model, m, *b),
                r.inset_xy(3.0, 0.0),
                &TextStyle::new(th.fonts.small, th.ui.text).center(),
            );
        }
        for (k, r) in &c.knobs {
            p.text(
                k.label(),
                Rect::new(r.x - 20.0, r.y - 13.0, r.w + 40.0, 12.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim).center(),
            );
            controls::knob(
                p,
                *r,
                ctl_get(&m.source, *k),
                k.bipolar(),
                KnobLook {
                    cap: th.console.pan_cap,
                    ring: if on { accent } else { th.ui.text_faint },
                },
                th,
            );
            p.text(
                &ctl_text(&m.source, *k),
                Rect::new(r.x - 24.0, r.bottom() + 1.0, r.w + 48.0, 12.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text).center(),
            );
        }
        // Targets.
        for (i, line, depth, rm) in &c.routes {
            let route = &m.routes[*i];
            let name = Self::track(model)
                .and_then(|t| model.modulation_target_name(t.id, route.target))
                .unwrap_or_else(|| "(gone)".into());
            p.text(
                &name,
                Rect::new(line.x, line.y, depth.x - line.x - 4.0, line.h),
                &TextStyle::new(th.fonts.small, text),
            );
            p.fill_rounded(*depth, 2.0, &Paint::Solid(th.ui.background));
            let mid = depth.x + depth.w / 2.0;
            let end = mid + route.depth.clamp(-1.0, 1.0) * depth.w / 2.0;
            p.fill(
                Rect::new(mid.min(end), depth.y, (end - mid).abs().max(1.0), depth.h),
                if on {
                    accent.with_alpha(0.75)
                } else {
                    th.ui.text_faint
                },
            );
            p.vline(mid, depth.y, depth.bottom(), th.ui.border);
            p.text(
                &format!("{:+.0} %", route.depth * 100.0),
                *depth,
                &TextStyle::new(th.fonts.tiny, th.ui.text)
                    .family(FontFamily::Mono)
                    .center(),
            );
            p.text(
                "×",
                *rm,
                &TextStyle::new(th.fonts.small, th.ui.text_faint).center(),
            );
        }
        let add = if c.hidden > 0 {
            format!("+ Target   ({} more)", c.hidden)
        } else {
            "+ Target".into()
        };
        p.text(
            &add,
            c.add_route,
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
    }

    fn paint_display(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        m: &Modulator,
        value: f32,
        learning: bool,
    ) {
        let th = &self.theme;
        let accent = if m.enabled {
            th.ui.accent
        } else {
            th.ui.text_faint
        };
        p.fill_rounded(r, 3.0, &Paint::Solid(th.ui.background));
        p.stroke_rounded(r, 3.0, 1.0, th.ui.border);
        let plot = Rect::new(r.x + 4.0, r.y + 4.0, r.w - 18.0, r.h - 8.0);
        let bipolar = m.source.bipolar();
        let y_of = |v: f32| {
            if bipolar {
                plot.y + plot.h * (1.0 - (v + 1.0) / 2.0)
            } else {
                plot.y + plot.h * (1.0 - v)
            }
        };
        if bipolar {
            p.hline(plot.x, plot.right(), y_of(0.0), th.ui.border);
        }
        let curve = |p: &mut dyn Painter, f: &dyn Fn(f64) -> f32| {
            let mut path = Path::new();
            let n = 96;
            for i in 0..=n {
                let t = f64::from(i) / f64::from(n);
                let pt = Point::new(plot.x + plot.w * t as f32, y_of(f(t)));
                if i == 0 {
                    path.move_to(pt);
                } else {
                    path.line_to(pt);
                }
            }
            p.stroke_path(&path, 1.6, accent);
        };
        match &m.source {
            ModSource::Lfo { shape, phase, .. } => {
                // Two cycles.
                let (shape, phase) = (*shape, f64::from(*phase));
                curve(p, &|t| shape.at(t * 2.0 + phase));
            }
            ModSource::Steps { steps, glide, .. } => {
                let n = steps.len().max(1);
                let w = plot.w / n as f32;
                for (i, v) in steps.iter().enumerate() {
                    let (a, b) = (y_of(0.0), y_of(*v));
                    p.fill(
                        Rect::new(
                            plot.x + i as f32 * w + 1.0,
                            a.min(b),
                            (w - 2.0).max(1.0),
                            (a - b).abs().max(1.0),
                        ),
                        accent.with_alpha(0.55),
                    );
                }
                if *glide > 0.0 {
                    let steps = steps.clone();
                    let glide = *glide;
                    curve(p, &move |t| steps_at(&steps, t * steps.len() as f64, glide));
                }
            }
            ModSource::Random { smooth, .. } => {
                // A sample of what it does (the real values come with play).
                let s = *smooth;
                let pts = [0.6f32, -0.4, 0.9, -0.8, 0.1, 0.5, -0.2, -0.7];
                curve(p, &move |t| steps_at(&pts, t * pts.len() as f64, s));
            }
            ModSource::NoteLfo { shape, phase, .. } => {
                let (shape, phase) = (*shape, f64::from(*phase));
                curve(p, &|t| shape.at(t * 2.0 + phase));
            }
            ModSource::NoteEnvelope {
                attack_ms,
                decay_ms,
                sustain,
                release_ms,
            } => {
                // Held for as long as attack and decay take, and as long
                // again at the sustain, then released.
                let adsr = (*attack_ms, *decay_ms, *sustain, *release_ms);
                let held = 2.0 * (attack_ms + decay_ms).max(1.0);
                let total = held + release_ms.max(1.0);
                curve(p, &move |t| {
                    let ms = t as f32 * total;
                    let released = (ms > held).then_some((held, ms - held));
                    faderframe_project::modulation::note_envelope(adsr, ms, released)
                });
            }
            ModSource::Velocity => curve(p, &|t| t as f32),
            ModSource::Key => curve(p, &|t| t as f32 * 2.0 - 1.0),
            ModSource::NoteRandom => {
                for (i, v) in [0.6f32, -0.4, 0.9, -0.8, 0.1, 0.5, -0.2, -0.7]
                    .iter()
                    .enumerate()
                {
                    let x = plot.x + plot.w * (i as f32 + 0.5) / 8.0;
                    p.circle(Point::new(x, y_of(*v)), 2.5, accent.with_alpha(0.8));
                }
            }
            ModSource::Follower { .. } | ModSource::Macro { .. } => {
                p.fill(
                    Rect::new(
                        plot.x,
                        plot.y + plot.h * 0.35,
                        plot.w * value.clamp(0.0, 1.0),
                        plot.h * 0.3,
                    ),
                    accent.with_alpha(0.6),
                );
            }
        }
        // The output now: a line across and a meter at the side.
        let y = y_of(value.clamp(-1.0, 1.0));
        p.hline(plot.x, plot.right(), y, accent.with_alpha(0.35));
        let meter = Rect::new(r.right() - 10.0, plot.y, 6.0, plot.h);
        p.fill(meter, th.ui.surface);
        let base = if bipolar { y_of(0.0) } else { meter.bottom() };
        p.fill(
            Rect::new(meter.x, base.min(y), meter.w, (base - y).abs().max(1.0)),
            accent,
        );
        if m.source.per_note() {
            p.text(
                "PER NOTE",
                Rect::new(r.x + 6.0, r.y + 3.0, 80.0, 11.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim).bold(),
            );
        }
        if learning {
            p.text(
                "Move a control on this track's devices",
                Rect::new(r.x, r.bottom() - 14.0, r.w, 12.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text).center(),
            );
        }
    }

    fn emit_menu(cx: &mut EventCx<'_, Action>, at: Point, items: Vec<MenuItem<Action>>) {
        if !items.is_empty() {
            cx.request(HostRequest::ContextMenu { at, items });
        }
    }

    fn press(
        &mut self,
        hit: Hit,
        pos: Point,
        clicks: u32,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(t) = Self::track(model) else { return };
        let track = t.id;
        if hit == Hit::Add {
            Self::emit_menu(cx, pos, Self::add_menu(track));
            return;
        }
        let id = match hit {
            Hit::Enable(id)
            | Hit::Name(id)
            | Hit::Map(id)
            | Hit::Remove(id)
            | Hit::Display(id)
            | Hit::Knob(id, _)
            | Hit::Button(id, _)
            | Hit::Depth(id, _)
            | Hit::Unroute(id, _)
            | Hit::Route(id) => id,
            Hit::Add => return,
        };
        let Some((_, mut m)) = Self::modulator(model, id) else {
            return;
        };
        let index = t.modulators.iter().position(|x| x.id == id).unwrap_or(0);
        match hit {
            Hit::Enable(_) => {
                m.enabled = !m.enabled;
                Self::set(cx, track, m);
            }
            Hit::Name(_) if clicks >= 2 => {
                let c = self.card(index, &m, size);
                cx.request(HostRequest::TextInput {
                    at: c.name,
                    initial: m.name.clone(),
                    commit: Box::new(move |text| {
                        let name = text.trim();
                        (!name.is_empty()).then(|| {
                            let mut x = m.clone();
                            x.name = name.to_string();
                            Action::SetModulator {
                                track,
                                modulator: x,
                            }
                        })
                    }),
                });
            }
            Hit::Map(_) => {
                let learning = model.modulation_learning() == Some((track, id));
                cx.emit(Action::LearnModulation((!learning).then_some((track, id))));
            }
            Hit::Remove(_) => cx.emit(Action::RemoveModulator {
                track,
                modulator: id,
            }),
            Hit::Unroute(_, r) => {
                if r < m.routes.len() {
                    m.routes.remove(r);
                    Self::set(cx, track, m);
                }
            }
            Hit::Route(_) => Self::emit_menu(cx, pos, Self::route_menu(model, track, &m)),
            Hit::Button(_, b) => match b {
                Btn::Shape => Self::emit_menu(cx, pos, Self::shape_menu(track, &m)),
                Btn::Source => Self::emit_menu(cx, pos, Self::source_menu(model, track, &m)),
                Btn::Sync => {
                    Self::toggle_sync(model, &mut m);
                    Self::set(cx, track, m);
                }
                Btn::Fewer | Btn::More => {
                    if let ModSource::Steps { steps, .. } = &mut m.source {
                        if b == Btn::More && steps.len() < MAX_STEPS {
                            steps.push(0.0);
                        } else if b == Btn::Fewer && steps.len() > 1 {
                            steps.pop();
                        }
                        Self::set(cx, track, m);
                    }
                }
            },
            Hit::Knob(_, ctl) if clicks >= 2 => {
                let v = default_of(&m.source, ctl);
                ctl_set(&mut m.source, ctl, v);
                Self::set(cx, track, m);
            }
            Hit::Knob(_, ctl) => {
                cx.emit(Action::BeginGesture(format!("{} {}", m.name, ctl.label())));
                let start = ctl_get(&m.source, ctl);
                self.drag = Some(Drag {
                    track,
                    m,
                    from: pos,
                    kind: DragKind::Knob { ctl, start },
                });
            }
            Hit::Depth(_, r) if clicks >= 2 => {
                if let Some(route) = m.routes.get_mut(r) {
                    route.depth = 0.0;
                    Self::set(cx, track, m);
                }
            }
            Hit::Depth(_, r) => {
                let c = self.card(index, &m, size);
                let width = c.routes.iter().find(|x| x.0 == r).map_or(80.0, |x| x.2.w);
                let start = m.routes.get(r).map_or(0.0, |x| x.depth);
                cx.emit(Action::BeginGesture("Modulation Depth".into()));
                self.drag = Some(Drag {
                    track,
                    m,
                    from: pos,
                    kind: DragKind::Depth {
                        route: r,
                        start,
                        width,
                    },
                });
            }
            Hit::Display(_) => {
                let kind = match m.source {
                    ModSource::Steps { .. } => DragKind::Steps { last: None },
                    ModSource::Macro { .. } => DragKind::Macro,
                    _ => return,
                };
                cx.emit(Action::BeginGesture(format!("{} Display", m.name)));
                self.drag = Some(Drag {
                    track,
                    m,
                    from: pos,
                    kind,
                });
                self.drag_to(pos, Modifiers::NONE, size, model, cx);
            }
            Hit::Name(_) | Hit::Add => {}
        }
    }

    fn drag_to(
        &mut self,
        pos: Point,
        modifiers: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(t) = Self::track(model) else { return };
        // The card's display (where steps are drawn, a macro dragged).
        let display = self.drag.as_ref().and_then(|d| {
            let i = t.modulators.iter().position(|x| x.id == d.m.id)?;
            Some(self.card(i, &d.m, size).display)
        });
        let Some(d) = self.drag.as_mut() else { return };
        let fine = if modifiers.fine() { 0.2 } else { 1.0 };
        match d.kind.clone() {
            DragKind::Knob { ctl, start } => {
                let n = start + (d.from.y - pos.y) / 150.0 * fine;
                ctl_set(&mut d.m.source, ctl, n);
            }
            DragKind::Depth {
                route,
                start,
                width,
            } => {
                let v = start + (pos.x - d.from.x) / (width / 2.0) * fine;
                if let Some(r) = d.m.routes.get_mut(route) {
                    r.depth = (v.clamp(-1.0, 1.0) * 100.0).round() / 100.0;
                }
            }
            DragKind::Steps { last } => {
                let Some(display) = display else { return };
                let display = Rect::new(
                    display.x + 4.0,
                    display.y + 4.0,
                    display.w - 18.0,
                    display.h - 8.0,
                );
                if let ModSource::Steps { steps, .. } = &mut d.m.source {
                    let (at, v) = Self::step_at(display, steps.len(), pos);
                    // Every step the pointer passed over since the last.
                    let from = last.unwrap_or(at);
                    for s in from.min(at)..=from.max(at) {
                        if let Some(x) = steps.get_mut(s) {
                            *x = v;
                        }
                    }
                    d.kind = DragKind::Steps { last: Some(at) };
                }
            }
            DragKind::Macro => {
                let Some(display) = display else { return };
                let n = (pos.x - display.x - 4.0) / (display.w - 18.0);
                ctl_set(&mut d.m.source, Ctl::Value, n);
            }
        }
        let (track, m) = (d.track, d.m.clone());
        Self::set(cx, track, m);
    }
}

impl CanvasView<Session, Action> for ModulatorsView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        let header = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(header, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        p.text(
            "Modulators",
            Rect::new(12.0, 0.0, 100.0, HEADER_H),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        let Some(t) = Self::track(model) else {
            p.text(
                "Select a track to see its modulators",
                Rect::new(112.0, 0.0, size.w - 124.0, HEADER_H),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
            return;
        };
        let learning = model.modulation_learning();
        let summary = if !Self::takes_modulators(t) {
            format!("{} · MIDI tracks, folders and VCAs take none", t.name)
        } else if let Some(m) = learning
            .filter(|l| l.0 == t.id)
            .and_then(|l| t.modulators.iter().find(|m| m.id == l.1))
        {
            format!(
                "{} · mapping ‘{}’: move a control on the track's devices (Map again stops)",
                t.name, m.name
            )
        } else if t.modulators.is_empty() {
            format!(
                "{} · none yet: Add one, then Map it to what it should move",
                t.name
            )
        } else {
            format!(
                "{} · {} modulator{} (they move values without changing them)",
                t.name,
                t.modulators.len(),
                if t.modulators.len() == 1 { "" } else { "s" }
            )
        };
        let add = Self::add_rect(size);
        p.text(
            &summary,
            Rect::new(112.0, 0.0, add.x - 120.0, HEADER_H),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
        if !Self::takes_modulators(t) {
            return;
        }
        p.fill_rounded(add, 3.0, &Paint::Solid(th.ui.accent.with_alpha(0.2)));
        p.stroke_rounded(add, 3.0, 1.0, th.ui.accent);
        p.text(
            "+ Add",
            add,
            &TextStyle::new(th.fonts.small, th.ui.text).bold().center(),
        );
        self.clamp(t.modulators.len(), size);
        let values = model.modulator_values(t.id);
        p.push_clip(Rect::new(0.0, HEADER_H, size.w, size.h - HEADER_H));
        for (i, m) in t.modulators.iter().enumerate() {
            let c = self.card(i, m, size);
            if c.rect.right() < 0.0 || c.rect.x > size.w {
                continue;
            }
            let value = values.iter().find(|v| v.0 == m.id).map_or(0.0, |v| v.1);
            let mapping = learning == Some((t.id, m.id));
            self.paint_card(p, &c, m, value, mapping, model);
        }
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
                clicks,
                ..
            } => {
                let Some(hit) = self.hit_test(pos, size, model) else {
                    return false;
                };
                self.press(hit, pos, clicks, size, model, cx);
                cx.redraw();
                true
            }
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } if self.drag.is_some() => {
                self.drag_to(pos, modifiers, size, model, cx);
                true
            }
            ViewEvent::PointerUp { .. } => {
                if self.drag.take().is_some() {
                    cx.emit(Action::EndGesture);
                    return true;
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
                let Some(t) = Self::track(model) else {
                    return false;
                };
                let count = t.modulators.len();
                let sideways = dx != 0.0 || modifiers.shift;
                if !sideways {
                    // Over a card the wheel turns what is under it, never
                    // scrolls.
                    let step = if precise { -dy / 30.0 } else { -dy };
                    let fine = if modifiers.fine() { 0.2 } else { 1.0 };
                    match self.hit_test(pos, size, model) {
                        Some(Hit::Knob(id, ctl)) => {
                            if let Some((track, mut m)) = Self::modulator(model, id) {
                                let n = ctl_get(&m.source, ctl) + step * 0.02 * fine;
                                ctl_set(&mut m.source, ctl, n);
                                Self::set(cx, track, m);
                            }
                            return true;
                        }
                        Some(Hit::Depth(id, r)) => {
                            if let Some((track, mut m)) = Self::modulator(model, id)
                                && let Some(route) = m.routes.get_mut(r)
                            {
                                route.depth = ((route.depth + step * 0.02 * fine).clamp(-1.0, 1.0)
                                    * 100.0)
                                    .round()
                                    / 100.0;
                                Self::set(cx, track, m);
                            }
                            return true;
                        }
                        _ => {}
                    }
                    let over_card = t
                        .modulators
                        .iter()
                        .enumerate()
                        .any(|(i, m)| self.card(i, m, size).rect.contains(pos));
                    if over_card {
                        return true;
                    }
                }
                let d = if dx != 0.0 { dx } else { dy };
                self.scroll += if precise { d } else { d * 48.0 };
                self.clamp(count, size);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let hit = self.hit_test(pos, size, model)?;
        let t = Self::track(model)?;
        let find = |id: ModulatorId| t.modulators.iter().find(|m| m.id == id);
        Some(match hit {
            Hit::Add => "Add an LFO, an envelope follower, steps, random or a macro".into(),
            Hit::Enable(_) => "On / off".into(),
            Hit::Name(_) => "Double-click to rename".into(),
            Hit::Map(_) => {
                "Map: then move a control on this track's devices, and this modulator moves it"
                    .into()
            }
            Hit::Remove(_) => "Remove the modulator".into(),
            Hit::Display(id) => match find(id).map(|m| &m.source) {
                Some(ModSource::Steps { .. }) => "Draw the steps".into(),
                Some(ModSource::Macro { .. }) => "Drag to set the macro".into(),
                _ => "What it puts out now".into(),
            },
            Hit::Knob(id, ctl) => {
                let m = find(id)?;
                format!(
                    "{}: {} · drag (Shift: finer), double-click resets",
                    ctl.label(),
                    ctl_text(&m.source, ctl)
                )
            }
            Hit::Button(_, b) => match b {
                Btn::Shape => "The LFO's shape".into(),
                Btn::Sync => "Synced to the song's beat, or free in Hz".into(),
                Btn::Fewer => "One step fewer".into(),
                Btn::More => "One step more".into(),
                Btn::Source => "What the follower listens to".into(),
            },
            Hit::Depth(id, r) => {
                let m = find(id)?;
                let route = m.routes.get(r)?;
                format!(
                    "{}: {:+.0} % of its range at full output · drag, double-click: none",
                    model
                        .modulation_target_name(t.id, route.target)
                        .unwrap_or_default(),
                    route.depth * 100.0
                )
            }
            Hit::Unroute(..) => "Stop moving this target".into(),
            Hit::Route(_) => "Choose another target to move".into(),
        })
    }

    fn wants_frames(&self, model: &Session) -> bool {
        Self::track(model).is_some_and(|t| {
            t.modulators
                .iter()
                .any(|m| m.enabled && !matches!(m.source, ModSource::Macro { .. }))
        })
    }

    fn min_size(&self) -> Size {
        Size::new(300.0, 220.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Horizontal {
            return None;
        }
        let count = Self::track(model).map_or(0, |t| t.modulators.len());
        Some(ScrollInfo {
            content: Self::content_w(count),
            viewport: size.w,
            offset: self.scroll,
            start: 0.0,
            end: 0.0,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Horizontal {
            self.scroll = offset.max(0.0);
        }
    }
}

#[cfg(test)]
mod tests;
