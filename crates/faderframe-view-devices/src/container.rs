//! The Container's face: its chains side by side, each a column — its name
//! (double-click renames), mute, solo and remove; its level and balance
//! (drag; double-click resets); on an instrument track the keys it plays
//! (click to type "C2-B3", double-click: all); its devices in order (the
//! light bypasses, a click opens the editor, the right button offers the
//! sidechain, bypass and Remove); and a row to add a device (instruments
//! too on an instrument track). "+ Chain" adds a chain. The chains run in
//! parallel and are mixed at the container's output.

use crate::values::{note_name, parse_key_range};
use faderframe_core::{PluginInstanceId, TrackId, builtin};
use faderframe_project::container::{Chain, audible};
use faderframe_project::{Command, PluginRef, PluginSlot};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, EventCx, HostRequest, MenuItem, Paint, Painter, Point, PointerButton, Rect,
    ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const TOP: f32 = 34.0;
const COL_W: f32 = 210.0;
const GAP: f32 = 10.0;
const HEAD_H: f32 = 28.0;
const MIX_H: f32 = 46.0;
const ROW_H: f32 = 26.0;
/// The level bar's range (dB).
const DB: (f32, f32) = (-60.0, 12.0);

/// What is under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    AddChain,
    Name(usize),
    Mute(usize),
    Solo(usize),
    RemoveChain(usize),
    Level(usize),
    Pan(usize),
    Keys(usize),
    /// A device: its light (bypass) or its name.
    Bypass(usize, usize),
    Device(usize, usize),
    AddDevice(usize),
}

/// Where a chain's column parts are.
#[derive(Clone, Debug)]
pub struct Column {
    pub rect: Rect,
    pub name: Rect,
    pub mute: Rect,
    pub solo: Rect,
    pub remove: Rect,
    pub level: Rect,
    pub pan: Rect,
    /// The keys it plays (instrument tracks).
    pub keys: Option<Rect>,
    /// (light, row) of each device.
    pub devices: Vec<(Rect, Rect)>,
    pub add: Rect,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Level { chain: usize, from: f32, start: f32 },
    Pan { chain: usize, from: f32, start: f32 },
}

pub struct ContainerView {
    plugin: PluginInstanceId,
    theme: Theme,
    scroll: f32,
    drag: Option<Drag>,
    /// The track plays notes: chains show their keys.
    keys: bool,
}

fn level_norm(db: f32) -> f32 {
    ((db - DB.0) / (DB.1 - DB.0)).clamp(0.0, 1.0)
}

fn level_db(n: f32) -> f32 {
    let db = DB.0 + n.clamp(0.0, 1.0) * (DB.1 - DB.0);
    (db * 10.0).round() / 10.0
}

impl ContainerView {
    pub fn new(plugin: PluginInstanceId, theme: &Theme) -> Self {
        Self {
            plugin,
            theme: theme.clone(),
            scroll: 0.0,
            drag: None,
            keys: false,
        }
    }

    fn chains<'a>(&self, model: &'a Session) -> Option<(TrackId, &'a [Chain])> {
        let (t, _) = model.plugin_slot(self.plugin)?;
        Some((
            t.id,
            model.container_chains(t.id, self.plugin).unwrap_or(&[]),
        ))
    }

    pub fn add_chain_rect(size: Size) -> Rect {
        Rect::new(size.w - 96.0, 6.0, 86.0, TOP - 12.0)
    }

    /// Chain `i`'s column for `chain`.
    pub fn column(&self, i: usize, chain: &Chain, size: Size, keys: bool) -> Column {
        let rect = Rect::new(
            GAP + i as f32 * (COL_W + GAP) - self.scroll,
            TOP + 4.0,
            COL_W,
            (size.h - TOP - 4.0 - GAP).max(120.0),
        );
        let (x, y, w) = (rect.x, rect.y, rect.w);
        let remove = Rect::new(rect.right() - 22.0, y + 6.0, 16.0, 16.0);
        let solo = Rect::new(remove.x - 24.0, y + 6.0, 20.0, 16.0);
        let mute = Rect::new(solo.x - 24.0, y + 6.0, 20.0, 16.0);
        let name = Rect::new(x + 8.0, y, mute.x - x - 12.0, HEAD_H);
        let level = Rect::new(x + 8.0, y + HEAD_H + 6.0, w - 16.0, 14.0);
        let pan = Rect::new(x + 8.0, level.bottom() + 8.0, w - 16.0, 10.0);
        let keys_row = keys.then(|| Rect::new(x + 8.0, pan.bottom() + 14.0, w - 16.0, 16.0));
        let top = y + HEAD_H + MIX_H + 6.0 + if keys { 22.0 } else { 0.0 };
        let devices = (0..chain.inserts.len())
            .map(|d| {
                let row = Rect::new(x + 6.0, top + d as f32 * ROW_H, w - 12.0, ROW_H - 3.0);
                (Rect::new(row.x + 6.0, row.y + 6.0, 11.0, 11.0), row)
            })
            .collect();
        let add = Rect::new(
            x + 6.0,
            top + chain.inserts.len() as f32 * ROW_H,
            w - 12.0,
            ROW_H - 3.0,
        );
        Column {
            rect,
            name,
            mute,
            solo,
            remove,
            level,
            pan,
            keys: keys_row,
            devices,
            add,
        }
    }

    pub fn hit_test(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        if Self::add_chain_rect(size).contains(pos) {
            return Some(Hit::AddChain);
        }
        let (_, chains) = self.chains(model)?;
        for (i, chain) in chains.iter().enumerate() {
            let c = self.column(i, chain, size, self.keys);
            if !c.rect.contains(pos) {
                continue;
            }
            let found = [
                (c.remove, Hit::RemoveChain(i)),
                (c.mute, Hit::Mute(i)),
                (c.solo, Hit::Solo(i)),
                (c.name, Hit::Name(i)),
                (c.level.inset_xy(0.0, -4.0), Hit::Level(i)),
                (c.pan.inset_xy(0.0, -4.0), Hit::Pan(i)),
                (c.keys.unwrap_or_default(), Hit::Keys(i)),
                (c.add, Hit::AddDevice(i)),
            ]
            .into_iter()
            .find(|(r, _)| r.contains(pos))
            .map(|(_, h)| h);
            if found.is_some() {
                return found;
            }
            for (d, (light, row)) in c.devices.iter().enumerate() {
                if light.inset(-4.0).contains(pos) {
                    return Some(Hit::Bypass(i, d));
                }
                if row.contains(pos) {
                    return Some(Hit::Device(i, d));
                }
            }
            return None;
        }
        None
    }

    fn mix(track: TrackId, container: PluginInstanceId, i: usize, c: &Chain) -> Command {
        Command::SetChainMix {
            track,
            container,
            chain: i,
            gain_db: c.gain_db,
            pan: c.pan,
            mute: c.mute,
            solo: c.solo,
        }
    }

    /// The devices to add: every effect (and instrument, on an instrument
    /// track), built-ins first, and a container.
    fn add_menu(
        model: &Session,
        track: TrackId,
        container: PluginInstanceId,
        chain: usize,
        index: usize,
    ) -> Vec<MenuItem<Action>> {
        let instruments = model
            .project()
            .track(track)
            .is_some_and(|t| t.kind == faderframe_project::TrackKind::Instrument);
        let mut effects: Vec<_> = model
            .available_plugins()
            .into_iter()
            .filter(|p| (instruments || !p.instrument) && !model.is_midi_effect(&p.plugin))
            .collect();
        effects.sort_by(|a, b| {
            let hosted = |p: &faderframe_session::AvailablePlugin| {
                p.plugin.format != faderframe_project::PluginFormat::Builtin
            };
            (
                hosted(a),
                a.vendor.to_lowercase(),
                a.plugin.name.to_lowercase(),
            )
                .cmp(&(
                    hosted(b),
                    b.vendor.to_lowercase(),
                    b.plugin.name.to_lowercase(),
                ))
        });
        let mut items = Vec::new();
        let mut hosted_yet = false;
        let has_container = effects.iter().any(|p| p.plugin.is_container());
        for p in effects {
            let hosted = p.plugin.format != faderframe_project::PluginFormat::Builtin;
            let mut item = MenuItem::new(
                if hosted {
                    format!("{} — {}", p.plugin.name, p.vendor)
                } else {
                    p.plugin.name.clone()
                },
                Action::InsertIntoChain {
                    track,
                    container,
                    chain,
                    index,
                    plugin: p.plugin.clone(),
                },
            );
            if hosted && !hosted_yet {
                item.separator_before = true;
                hosted_yet = true;
            }
            items.push(item);
        }
        if !has_container {
            items.push(MenuItem::new(
                "Container",
                Action::InsertIntoChain {
                    track,
                    container,
                    chain,
                    index,
                    plugin: PluginRef::builtin(builtin::CONTAINER, "Container"),
                },
            ));
        }
        items
    }

    fn device_menu(model: &Session, track: TrackId, slot: &PluginSlot) -> Vec<MenuItem<Action>> {
        let mut items = vec![
            MenuItem::disabled(slot.plugin.name.clone()),
            MenuItem::new(
                "Show Editor",
                Action::OpenPluginEditor {
                    track,
                    plugin: slot.id,
                    generic: false,
                },
            ),
            MenuItem::new(
                if slot.bypass { "Turn On" } else { "Bypass" },
                Action::Edit(Command::SetPluginBypass {
                    track,
                    plugin: slot.id,
                    bypass: !slot.bypass,
                }),
            ),
            MenuItem::new(
                "Remove",
                Action::RemoveFromChain {
                    track,
                    plugin: slot.id,
                },
            ),
        ];
        // Which track's pre-fader signal keys it.
        if model.plugin_has_sidechain(slot.id) {
            let set = |source| {
                Action::Edit(Command::SetPluginSidechain {
                    track,
                    plugin: slot.id,
                    source,
                })
            };
            items.push(
                MenuItem::new("Sidechain: None", set(None))
                    .checked(slot.sidechain.is_none())
                    .separated(),
            );
            for (id, name) in model.sidechain_sources(slot.id) {
                items.push(
                    MenuItem::new(format!("Sidechain from {name}"), set(Some(id)))
                        .checked(slot.sidechain == Some(id)),
                );
            }
        }
        items
    }

    fn content_w(count: usize) -> f32 {
        GAP + count as f32 * (COL_W + GAP)
    }

    fn clamp(&mut self, count: usize, size: Size) {
        let max = (Self::content_w(count) - size.w).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn paint_column(
        &self,
        p: &mut dyn Painter,
        c: &Column,
        chains: &[Chain],
        i: usize,
        model: &Session,
    ) {
        let th = &self.theme;
        let chain = &chains[i];
        let heard = audible(chains, i);
        let accent = th.ui.accent;
        p.fill_rounded(c.rect, 5.0, &Paint::Solid(th.ui.surface));
        p.stroke_rounded(c.rect, 5.0, 1.0, th.ui.border);
        let text = if heard { th.ui.text } else { th.ui.text_faint };
        p.text(
            &chain.name,
            c.name,
            &TextStyle::new(th.fonts.normal, text).bold(),
        );
        for (r, label, on, color) in [
            (c.mute, "M", chain.mute, th.console.led.mute),
            (c.solo, "S", chain.solo, th.console.led.solo),
        ] {
            p.fill_rounded(
                r,
                3.0,
                &Paint::Solid(if on {
                    color.with_alpha(0.8)
                } else {
                    th.ui.background
                }),
            );
            p.stroke_rounded(r, 3.0, 1.0, th.ui.border);
            p.text(
                label,
                r,
                &TextStyle::new(th.fonts.small, th.ui.text).bold().center(),
            );
        }
        p.text(
            "×",
            c.remove,
            &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
        );
        // Level and balance.
        p.fill_rounded(c.level, 3.0, &Paint::Solid(th.ui.background));
        let n = level_norm(chain.gain_db);
        p.fill_rounded(
            Rect::new(c.level.x, c.level.y, (c.level.w * n).max(2.0), c.level.h),
            3.0,
            &Paint::Solid(if heard {
                accent.with_alpha(0.7)
            } else {
                th.ui.text_faint.with_alpha(0.4)
            }),
        );
        let unity = c.level.x + c.level.w * level_norm(0.0);
        p.vline(unity, c.level.y, c.level.bottom(), th.ui.text_dim);
        p.text(
            &format!("{:+.1} dB", chain.gain_db).replace("+0.0", "0.0"),
            c.level,
            &TextStyle::new(th.fonts.tiny, th.ui.text).center(),
        );
        p.fill_rounded(c.pan, 3.0, &Paint::Solid(th.ui.background));
        let mid = c.pan.x + c.pan.w / 2.0;
        let at = mid + chain.pan.clamp(-1.0, 1.0) * c.pan.w / 2.0;
        p.fill(
            Rect::new(mid.min(at), c.pan.y, (at - mid).abs().max(2.0), c.pan.h),
            accent.with_alpha(0.7),
        );
        p.vline(mid, c.pan.y, c.pan.bottom(), th.ui.text_dim);
        p.text(
            &faderframe_core::pan::format_pan(chain.pan),
            Rect::new(c.pan.x, c.pan.bottom(), c.pan.w, 10.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim).center(),
        );
        if let Some(k) = c.keys {
            p.fill_rounded(k, 3.0, &Paint::Solid(th.ui.background));
            let text = if chain.all_keys() {
                "All keys".to_string()
            } else {
                format!(
                    "{} – {}",
                    note_name(i32::from(chain.key_low)),
                    note_name(i32::from(chain.key_high))
                )
            };
            p.text(
                "KEYS",
                k.inset_xy(6.0, 0.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim).bold(),
            );
            p.text(
                &text,
                k.inset_xy(6.0, 0.0),
                &TextStyle::new(th.fonts.small, th.ui.text).right(),
            );
        }
        // Devices.
        for (d, (light, row)) in c.devices.iter().enumerate() {
            let slot = &chain.inserts[d];
            p.fill_rounded(*row, 3.0, &Paint::Solid(th.ui.background));
            let on = !slot.bypass;
            p.circle(
                light.center(),
                4.5,
                if on {
                    accent
                } else {
                    th.ui.text_faint.with_alpha(0.5)
                },
            );
            let name = if slot.plugin.is_container() {
                let n = model
                    .container_chains(
                        model
                            .plugin_slot(self.plugin)
                            .map_or(TrackId(0), |(t, _)| t.id),
                        slot.id,
                    )
                    .map_or(0, <[Chain]>::len);
                format!("{} ({n} chains)", slot.plugin.name)
            } else {
                slot.plugin.name.clone()
            };
            p.text(
                &name,
                Rect::new(
                    light.right() + 8.0,
                    row.y,
                    row.right() - light.right() - 12.0,
                    row.h,
                ),
                &TextStyle::new(
                    th.fonts.small,
                    if on { th.ui.text } else { th.ui.text_faint },
                ),
            );
        }
        p.text(
            "+ Device",
            c.add,
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
    }
}

impl CanvasView<Session, Action> for ContainerView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        p.fill(Rect::new(0.0, 0.0, size.w, TOP), th.ui.surface);
        p.hline(0.0, size.w, TOP - 0.5, th.ui.border);
        self.keys = model
            .plugin_slot(self.plugin)
            .is_some_and(|(t, _)| t.kind == faderframe_project::TrackKind::Instrument);
        let Some((_, chains)) = self.chains(model) else {
            p.text(
                "This container is gone",
                Rect::new(12.0, 0.0, size.w - 24.0, TOP),
                &TextStyle::new(th.fonts.normal, th.ui.text_dim),
            );
            return;
        };
        let soloed = chains.iter().any(|c| c.solo);
        p.text(
            &format!(
                "{} chain{} in parallel{}",
                chains.len(),
                if chains.len() == 1 { "" } else { "s" },
                if soloed { " · soloed" } else { "" }
            ),
            Rect::new(12.0, 0.0, size.w - 120.0, TOP),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
        let add = Self::add_chain_rect(size);
        p.fill_rounded(add, 3.0, &Paint::Solid(th.ui.accent.with_alpha(0.2)));
        p.stroke_rounded(add, 3.0, 1.0, th.ui.accent);
        p.text(
            "+ Chain",
            add,
            &TextStyle::new(th.fonts.small, th.ui.text).bold().center(),
        );
        self.clamp(chains.len(), size);
        p.push_clip(Rect::new(0.0, TOP, size.w, size.h - TOP));
        for (i, chain) in chains.iter().enumerate() {
            let c = self.column(i, chain, size, self.keys);
            if c.rect.right() >= 0.0 && c.rect.x <= size.w {
                self.paint_column(p, &c, chains, i, model);
            }
        }
        if chains.is_empty() {
            p.text(
                "No chains: + Chain adds one (the signal passes through meanwhile)",
                Rect::new(12.0, TOP + 10.0, size.w - 24.0, 20.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
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
        let Some((track, chains)) = self.chains(model) else {
            return false;
        };
        let container = self.plugin;
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button,
                clicks,
                ..
            } => {
                let Some(hit) = self.hit_test(pos, size, model) else {
                    return false;
                };
                let secondary = button == PointerButton::Secondary;
                match hit {
                    Hit::AddChain => cx.emit(Action::AddChain { track, container }),
                    Hit::RemoveChain(i) => cx.emit(Action::RemoveChain {
                        track,
                        container,
                        chain: i,
                    }),
                    Hit::Name(i) if clicks >= 2 => {
                        let c = self.column(i, &chains[i], size, self.keys);
                        cx.request(HostRequest::TextInput {
                            at: c.name,
                            initial: chains[i].name.clone(),
                            commit: Box::new(move |text| {
                                let name = text.trim();
                                (!name.is_empty()).then(|| Action::RenameChain {
                                    track,
                                    container,
                                    chain: i,
                                    name: name.to_string(),
                                })
                            }),
                        });
                    }
                    Hit::Mute(i) | Hit::Solo(i) => {
                        let mut c = chains[i].clone();
                        if hit == Hit::Mute(i) {
                            c.mute = !c.mute;
                        } else {
                            c.solo = !c.solo;
                        }
                        cx.emit(Action::Edit(Self::mix(track, container, i, &c)));
                    }
                    Hit::Keys(i) if clicks >= 2 => cx.emit(Action::SetChainKeys {
                        track,
                        container,
                        chain: i,
                        low: 0,
                        high: 127,
                    }),
                    Hit::Keys(i) => {
                        let c = &chains[i];
                        let at = self.column(i, c, size, self.keys).keys.unwrap_or_default();
                        cx.request(HostRequest::TextInput {
                            at,
                            initial: format!(
                                "{}-{}",
                                note_name(i32::from(c.key_low)),
                                note_name(i32::from(c.key_high))
                            ),
                            commit: Box::new(move |text| {
                                let (low, high) = parse_key_range(text)?;
                                Some(Action::SetChainKeys {
                                    track,
                                    container,
                                    chain: i,
                                    low,
                                    high,
                                })
                            }),
                        });
                    }
                    Hit::Level(i) | Hit::Pan(i) if clicks >= 2 => {
                        let mut c = chains[i].clone();
                        if hit == Hit::Level(i) {
                            c.gain_db = 0.0;
                        } else {
                            c.pan = 0.0;
                        }
                        cx.emit(Action::Edit(Self::mix(track, container, i, &c)));
                    }
                    Hit::Level(i) => {
                        cx.emit(Action::BeginGesture("Chain Level".into()));
                        self.drag = Some(Drag::Level {
                            chain: i,
                            from: pos.x,
                            start: level_norm(chains[i].gain_db),
                        });
                    }
                    Hit::Pan(i) => {
                        cx.emit(Action::BeginGesture("Chain Balance".into()));
                        self.drag = Some(Drag::Pan {
                            chain: i,
                            from: pos.x,
                            start: chains[i].pan,
                        });
                    }
                    Hit::Bypass(i, d) => {
                        let slot = &chains[i].inserts[d];
                        cx.emit(Action::Edit(Command::SetPluginBypass {
                            track,
                            plugin: slot.id,
                            bypass: !slot.bypass,
                        }));
                    }
                    Hit::Device(i, d) if secondary => cx.request(HostRequest::ContextMenu {
                        at: pos,
                        items: Self::device_menu(model, track, &chains[i].inserts[d]),
                    }),
                    Hit::Device(i, d) => cx.emit(Action::OpenPluginEditor {
                        track,
                        plugin: chains[i].inserts[d].id,
                        generic: false,
                    }),
                    Hit::AddDevice(i) => cx.request(HostRequest::ContextMenu {
                        at: pos,
                        items: Self::add_menu(model, track, container, i, chains[i].inserts.len()),
                    }),
                    Hit::Name(_) => {}
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } => {
                let Some(drag) = self.drag else { return false };
                let fine = if modifiers.fine() { 0.2 } else { 1.0 };
                match drag {
                    Drag::Level { chain, from, start } => {
                        let Some(c) = chains.get(chain) else {
                            return true;
                        };
                        let width = self.column(chain, c, size, self.keys).level.w.max(1.0);
                        let mut c = c.clone();
                        c.gain_db = level_db(start + (pos.x - from) / width * fine);
                        cx.emit(Action::Edit(Self::mix(track, container, chain, &c)));
                    }
                    Drag::Pan { chain, from, start } => {
                        let Some(c) = chains.get(chain) else {
                            return true;
                        };
                        let width = self.column(chain, c, size, self.keys).pan.w.max(1.0);
                        let mut c = c.clone();
                        let v = start + (pos.x - from) / (width / 2.0) * fine;
                        c.pan = (v.clamp(-1.0, 1.0) * 100.0).round() / 100.0;
                        cx.emit(Action::Edit(Self::mix(track, container, chain, &c)));
                    }
                }
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
                // The wheel turns a level or balance under it; it scrolls
                // along the chains elsewhere (or sideways anywhere).
                let step = if precise { -dy / 30.0 } else { -dy };
                let sideways = dx != 0.0 || modifiers.shift;
                match self.hit_test(pos, size, model) {
                    Some(Hit::Level(i)) if !sideways => {
                        let mut c = chains[i].clone();
                        c.gain_db = (c.gain_db + step * 0.5).clamp(DB.0, DB.1);
                        cx.emit(Action::Edit(Self::mix(track, container, i, &c)));
                    }
                    Some(Hit::Pan(i)) if !sideways => {
                        let mut c = chains[i].clone();
                        c.pan = (c.pan + step * 0.05).clamp(-1.0, 1.0);
                        cx.emit(Action::Edit(Self::mix(track, container, i, &c)));
                    }
                    _ => {
                        let d = if dx != 0.0 { dx } else { dy };
                        self.scroll += if precise { d } else { d * 48.0 };
                        self.clamp(chains.len(), size);
                        cx.redraw();
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let (_, chains) = self.chains(model)?;
        Some(match self.hit_test(pos, size, model)? {
            Hit::AddChain => "Another chain in parallel".into(),
            Hit::Name(_) => "Double-click to rename".into(),
            Hit::Mute(_) => "Mute the chain".into(),
            Hit::Solo(_) => "Hear this chain alone (with other soloed ones)".into(),
            Hit::RemoveChain(_) => "Remove the chain and its devices".into(),
            Hit::Level(i) => format!(
                "Level {:.1} dB · drag (Shift: finer), double-click: 0 dB",
                chains[i].gain_db
            ),
            Hit::Pan(i) => format!(
                "Balance {} · drag, double-click: centre",
                faderframe_core::pan::format_pan(chains[i].pan)
            ),
            Hit::Keys(_) => {
                "The keys this chain plays: click to type (\"C2-B3\"), double-click for all".into()
            }
            Hit::Bypass(..) => "On / bypassed".into(),
            Hit::Device(..) => "Click to open its editor · right-click for more".into(),
            Hit::AddDevice(_) => "Add a device at the end of the chain".into(),
        })
    }

    fn min_size(&self) -> Size {
        Size::new(460.0, 260.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Horizontal {
            return None;
        }
        let count = self.chains(model).map_or(0, |(_, c)| c.len());
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
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use faderframe_engine::EngineConfig;
    use faderframe_ui_canvas::{Modifiers, RecordingPainter};

    const SIZE: Size = Size::new(900.0, 420.0);

    fn run(
        view: &mut ContainerView,
        ev: ViewEvent,
        s: &Session,
    ) -> (Vec<Action>, Vec<HostRequest<Action>>) {
        let (mut actions, mut requests) = (Vec::new(), Vec::new());
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(&ev, SIZE, s, &mut cx);
        (actions, requests)
    }

    fn down(pos: Point, clicks: u32) -> ViewEvent {
        ViewEvent::PointerDown {
            pos,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
            clicks,
        }
    }

    #[test]
    fn chains_are_columns_that_mix_and_take_devices() {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let bass = s
            .project()
            .tracks
            .iter()
            .find(|t| t.name == "Bass")
            .unwrap()
            .id;
        s.dispatch(Action::InsertPlugin {
            track: bass,
            index: 0,
            plugin: PluginRef::builtin(builtin::CONTAINER, "Container"),
        })
        .unwrap();
        let container = s.project().track(bass).unwrap().inserts[0].id;
        let mut view = ContainerView::new(container, &Theme::default());
        let mut p = RecordingPainter::new();
        view.paint(&mut p, SIZE, &s, &Theme::default());
        let texts = p.texts();
        assert!(texts.contains(&"Dry") && texts.contains(&"Chain 2"));
        // + Device on the second chain: built-in effects, no MIDI effects.
        let chains = s.container_chains(bass, container).unwrap().to_vec();
        let col = view.column(1, &chains[1], SIZE, false);
        let (_, req) = run(&mut view, down(col.add.center(), 1), &s);
        let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
            panic!("no menu")
        };
        assert!(!items.iter().any(|i| i.label == "Arpeggiator"));
        let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
        let reverb = items
            .into_iter()
            .find(|i| i.label == "FaderFrame Reverb")
            .unwrap_or_else(|| panic!("{labels:?}"));
        s.dispatch(reverb.action.unwrap()).unwrap();
        assert_eq!(
            s.container_chains(bass, container).unwrap()[1]
                .inserts
                .len(),
            1
        );
        // The level: dragged a quarter of the bar to the left, one step.
        let col = view.column(0, &chains[0], SIZE, false);
        let at = col.level.center();
        let mut actions = run(&mut view, down(at, 1), &s).0;
        actions.extend(
            run(
                &mut view,
                ViewEvent::PointerMove {
                    pos: Point::new(at.x - col.level.w / 4.0, at.y),
                    modifiers: Modifiers::NONE,
                    dragging: true,
                },
                &s,
            )
            .0,
        );
        actions.extend(
            run(
                &mut view,
                ViewEvent::PointerUp {
                    pos: at,
                    button: PointerButton::Primary,
                    modifiers: Modifiers::NONE,
                },
                &s,
            )
            .0,
        );
        assert!(matches!(actions.first(), Some(Action::BeginGesture(_))));
        assert_eq!(actions.last(), Some(&Action::EndGesture));
        for a in actions {
            s.dispatch(a).unwrap();
        }
        let gain = s.container_chains(bass, container).unwrap()[0].gain_db;
        assert!((gain - (0.0 - 72.0 / 4.0)).abs() < 0.2, "{gain}");
        // Solo the second chain: the first is not heard.
        let solo = Point::new(col.solo.center().x + COL_W + GAP, col.solo.center().y);
        let (a, _) = run(&mut view, down(solo, 1), &s);
        for a in a {
            s.dispatch(a).unwrap();
        }
        let chains = s.container_chains(bass, container).unwrap();
        assert!(chains[1].solo && !audible(chains, 0));
        // + Chain.
        let (a, _) = run(
            &mut view,
            down(ContainerView::add_chain_rect(SIZE).center(), 1),
            &s,
        );
        assert_eq!(
            a,
            [Action::AddChain {
                track: bass,
                container
            }]
        );
    }

    #[test]
    fn chains_on_an_instrument_track_play_key_ranges() {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let lead = s
            .project()
            .tracks
            .iter()
            .find(|t| t.name == "Lead Synth")
            .unwrap()
            .id;
        s.dispatch(Action::InsertPlugin {
            track: lead,
            index: 1,
            plugin: PluginRef::builtin(builtin::CONTAINER, "Container"),
        })
        .unwrap();
        let container = s.project().track(lead).unwrap().inserts[1].id;
        // A synth in its second chain is an instrument of the track.
        s.dispatch(Action::InsertIntoChain {
            track: lead,
            container,
            chain: 1,
            index: 0,
            plugin: PluginRef::builtin(builtin::SYNTH, "Synth"),
        })
        .unwrap();
        let mut view = ContainerView::new(container, &Theme::default());
        view.paint(&mut RecordingPainter::new(), SIZE, &s, &Theme::default());
        let chains = s.container_chains(lead, container).unwrap().to_vec();
        let col = view.column(1, &chains[1], SIZE, true);
        let keys = col.keys.expect("instrument tracks show the keys");
        let (_, req) = run(&mut view, down(keys.center(), 1), &s);
        let Some(HostRequest::TextInput { commit, .. }) = req.into_iter().next() else {
            panic!("a text field")
        };
        let action = commit("C4-C6").unwrap();
        assert_eq!(
            action,
            Action::SetChainKeys {
                track: lead,
                container,
                chain: 1,
                low: 60,
                high: 84
            }
        );
        s.dispatch(action).unwrap();
        let c = &s.container_chains(lead, container).unwrap()[1];
        assert_eq!((c.key_low, c.key_high), (60, 84));
        // Double-click: all keys again.
        let (a, _) = run(&mut view, down(keys.center(), 2), &s);
        assert!(matches!(
            a.as_slice(),
            [Action::SetChainKeys {
                low: 0,
                high: 127,
                ..
            }]
        ));
    }
}
