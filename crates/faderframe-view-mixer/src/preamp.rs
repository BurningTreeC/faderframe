//! Dedicated channel input faceplates, drawn by the native canvas: a
//! microphone preamp (Gain, Master) or, on a bus or the master, a console's
//! bus amplifier (Drive, Output).
use super::*;
use faderframe_core::ParameterId;
use faderframe_core::builtin::{CONSOLE_BUSES, PREAMPS, console_bus_index, preamp_index};
use faderframe_project::PluginSlot;
use faderframe_ui_canvas::TextStyle;

/// What an input stage's faceplate shows.
pub(super) struct Face {
    pub name: &'static str,
    pub rgb: [u8; 3],
    /// Dark ink on a light plate.
    pub light: bool,
    pub labels: [&'static str; 2],
    /// Per knob: low, high, at rest (the parameter's units).
    pub ranges: [(f64, f64, f64); 2],
    /// The first knob in percent (else dB).
    pub percent: bool,
    /// The first knob's cap red (the British 73's gain).
    pub red: bool,
}

pub(super) fn face(slot: &PluginSlot) -> Face {
    if let Some(i) = console_bus_index(&slot.plugin.id) {
        let (_, name, rgb) = CONSOLE_BUSES[i];
        // Dark ink on a light plate (the German 76's enamel grey).
        let luma =
            0.2126 * f32::from(rgb[0]) + 0.7152 * f32::from(rgb[1]) + 0.0722 * f32::from(rgb[2]);
        return Face {
            name,
            rgb,
            light: luma > 128.0,
            labels: ["Drive", "Output"],
            ranges: [(-12.0, 12.0, 0.0), (-24.0, 12.0, 0.0)],
            percent: false,
            red: false,
        };
    }
    let i = preamp_index(&slot.plugin.id).unwrap_or(0);
    let (_, name, rgb) = PREAMPS[i];
    Face {
        name,
        rgb,
        light: i >= 4,
        labels: ["Gain", "Master"],
        ranges: [(0.0, 1.0, 0.5), (-60.0, 12.0, 0.0)],
        percent: true,
        red: i == 0,
    }
}

/// The British 73's Output Load parameter.
const LOAD: u32 = 2;

/// Whether a British 73 drives a bridging input.
fn bridging(slot: &PluginSlot) -> bool {
    slot.parameters
        .iter()
        .find(|p| p.id == ParameterId(LOAD))
        .is_some_and(|p| p.value >= 0.5)
}

pub(super) fn value(slot: &PluginSlot, id: u32) -> f64 {
    let rest = face(slot).ranges[(id as usize).min(1)].2;
    slot.parameters
        .iter()
        .find(|p| p.id == ParameterId(id))
        .map_or(rest, |p| p.value)
}
pub(super) fn position(face: &Face, value: f64, id: u32) -> f32 {
    let (lo, hi, _) = face.ranges[(id as usize).min(1)];
    ((value - lo) / (hi - lo)).clamp(0.0, 1.0) as f32
}
pub(super) fn plain(face: &Face, position: f32, id: u32) -> f64 {
    let (lo, hi, _) = face.ranges[(id as usize).min(1)];
    lo + f64::from(position.clamp(0.0, 1.0)) * (hi - lo)
}
/// The knob's value as text.
pub(super) fn shown(face: &Face, value: f64, id: u32) -> String {
    if id == 0 && face.percent {
        format!("{:.1}%", value * 100.0)
    } else {
        format!("{value:+.1} dB")
    }
}
fn knobs(area: Rect) -> [Rect; 2] {
    let size = ((area.w - 12.0) / 2.0).min(30.0);
    [0, 1].map(|i| {
        Rect::new(
            area.x + area.w * (0.25 + 0.5 * i as f32) - size * 0.5,
            area.y + 20.0,
            size,
            size,
        )
    })
}
fn buttons(area: Rect) -> [Rect; 2] {
    [
        Rect::new(area.x + 3.0, area.bottom() - 17.0, area.w - 24.0, 14.0),
        Rect::new(area.right() - 18.0, area.bottom() - 17.0, 15.0, 14.0),
    ]
}
impl MixerView {
    pub(super) fn preamp_hit(&self, area: Rect, t: &Track, pos: Point) -> Option<Hit> {
        if !area.contains(pos) {
            return None;
        }
        if t.preamp.is_some() && area.h >= 80.0 {
            for (id, rect) in knobs(area).into_iter().enumerate() {
                if rect.contains(pos) {
                    return Some(Hit::PreampKnob(t.id, id as u32));
                }
            }
            if buttons(area)[1].contains(pos) {
                return Some(Hit::PreampRemove(t.id));
            }
        }
        Some(Hit::PreampChoose(t.id))
    }
    /// The input stage's menu: on the master the console (the whole mix),
    /// on a bus or return its bus amplifier, on a channel the microphone
    /// preamps.
    pub(super) fn stage_menu(model: &Session, t: &Track, at: Point) -> HostRequest<Action> {
        match t.kind {
            TrackKind::Master => Self::console_menu(model, at),
            TrackKind::Bus | TrackKind::Aux => Self::bus_amp_menu(model, t, at),
            _ => Self::preamp_menu(t, at),
        }
    }

    /// The console for the whole mix (the master's input stage).
    fn console_menu(model: &Session, at: Point) -> HostRequest<Action> {
        let items = model
            .console_choices()
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
        HostRequest::ContextMenu { at, items }
    }

    /// A bus's own amplifier: following the console, through another
    /// console's (hybrid summing), its drive, bypass, in the box, or a
    /// microphone preamp's colour instead.
    fn bus_amp_menu(model: &Session, t: &Track, at: Point) -> HostRequest<Action> {
        use faderframe_project::console::FAMILIES;
        let track = t.id;
        let amp = t
            .preamp
            .as_ref()
            .and_then(|s| console_bus_index(&s.plugin.id));
        let mut items = Vec::new();
        let console = model.console();
        if let Some(c) = console {
            items.push(
                MenuItem::new(
                    format!("Follow the Console ({})", c.name()),
                    Action::SetBusAmplifier {
                        track,
                        family: Some(c.family),
                    },
                )
                .checked(amp == Some(usize::from(c.family))),
            );
        }
        let families: Vec<_> = FAMILIES
            .iter()
            .enumerate()
            .map(|(f, name)| {
                MenuItem::new(
                    format!("{name} Bus"),
                    Action::SetBusAmplifier {
                        track,
                        family: Some(f as u8),
                    },
                )
                .checked(amp == Some(f))
            })
            .collect();
        items.push(MenuItem::submenu(
            if console.is_some() {
                "Through Another Console's Bus Amplifier"
            } else {
                "Bus Amplifier"
            },
            families,
        ));
        if let Some(slot) = t.preamp.as_ref().filter(|_| amp.is_some()) {
            let now = value(slot, 0);
            let drive: Vec<_> = [-6.0, -3.0, 0.0, 3.0, 6.0, 9.0, 12.0]
                .into_iter()
                .map(|db| {
                    MenuItem::new(
                        format!("{db:+} dB"),
                        Action::Edit(Command::SetPluginParameter {
                            track,
                            plugin: slot.id,
                            parameter: ParameterId(0),
                            value: Some(db),
                        }),
                    )
                    .checked((now - db).abs() < 0.05)
                })
                .collect();
            items.push(MenuItem::submenu("Drive", drive).separated());
            items.push(
                MenuItem::new(
                    "Bypass",
                    Action::Edit(Command::SetPluginBypass {
                        track,
                        plugin: slot.id,
                        bypass: !slot.bypass,
                    }),
                )
                .checked(slot.bypass),
            );
        }
        if t.preamp.is_some() {
            items.push(
                MenuItem::new(
                    "In the Box on This Bus",
                    Action::SetBusAmplifier {
                        track,
                        family: None,
                    },
                )
                .separated(),
            );
        }
        let preamps: Vec<_> = PREAMPS
            .iter()
            .enumerate()
            .map(|(i, &(id, name, _))| {
                MenuItem::new(
                    name,
                    Action::SetPreamp {
                        track,
                        model: Some(i),
                    },
                )
                .checked(t.preamp.as_ref().is_some_and(|p| p.plugin.id == id))
            })
            .collect();
        items.push(MenuItem::submenu("Colour Through a Microphone Preamp", preamps).separated());
        HostRequest::ContextMenu { at, items }
    }

    pub(super) fn preamp_menu(t: &Track, at: Point) -> HostRequest<Action> {
        let mut items: Vec<_> = PREAMPS
            .iter()
            .enumerate()
            .map(|(i, &(id, name, _))| {
                MenuItem::new(
                    name,
                    Action::SetPreamp {
                        track: t.id,
                        model: Some(i),
                    },
                )
                .checked(t.preamp.as_ref().is_some_and(|p| p.plugin.id == id))
            })
            .collect();
        // The British 73's line: terminated (600 ohms) or bridging (10 k).
        if let Some(slot) = t
            .preamp
            .as_ref()
            .filter(|s| preamp_index(&s.plugin.id) == Some(0))
        {
            let bridging = bridging(slot);
            let load = |label: &str, value: f64, on: bool| {
                MenuItem::new(
                    label,
                    Action::Edit(Command::SetPluginParameter {
                        track: t.id,
                        plugin: slot.id,
                        parameter: ParameterId(LOAD),
                        value: Some(value),
                    }),
                )
                .checked(on)
            };
            items.push(
                MenuItem::submenu(
                    "Output Load",
                    vec![
                        load("600 Ω (Terminated)", 0.0, !bridging),
                        load("10 kΩ (Bridging: More Headroom, Open Top)", 1.0, bridging),
                    ],
                )
                .separated(),
            );
        }
        if t.preamp.is_some() {
            items.push(
                MenuItem::new(
                    "Remove preamp",
                    Action::SetPreamp {
                        track: t.id,
                        model: None,
                    },
                )
                .separated(),
            );
        }
        HostRequest::ContextMenu { at, items }
    }
    pub(super) fn paint_preamp(&self, p: &mut dyn Painter, area: Rect, t: &Track, model: &Session) {
        let th = &self.theme;
        let Some(slot) = &t.preamp else {
            let label = match t.kind {
                TrackKind::Master => "+ CONSOLE",
                TrackKind::Bus | TrackKind::Aux => "+ BUS AMP",
                _ => "+ PREAMP",
            };
            controls::well_label(p, area, label, true, th);
            return;
        };
        let f = face(slot);
        let rgb = f.rgb;
        let bridged = format!("{} · 10 k", f.name);
        let name = if preamp_index(&slot.plugin.id) == Some(0) && bridging(slot) {
            bridged.as_str()
        } else {
            f.name
        };
        let base = Color::rgb8(rgb[0], rgb[1], rgb[2]);
        controls::panel(p, area, base.lighten(0.14), base.darken(0.16), th);
        let ink = if f.light {
            Color::rgb8(20, 23, 21)
        } else {
            Color::rgb8(242, 233, 214)
        };
        let label = TextStyle::new(9.0, ink).center();
        p.text(
            name,
            Rect::new(area.x + 2.0, area.y, area.w - 4.0, 18.0),
            &label,
        );
        if area.h < 80.0 {
            return;
        }
        for (id, rect) in knobs(area).into_iter().enumerate() {
            let v = model
                .display_value(
                    t.id,
                    faderframe_automation::AutomationTarget::PluginParameter {
                        plugin: slot.id,
                        parameter: ParameterId(id as u32),
                    },
                )
                .unwrap_or_else(|| value(slot, id as u32));
            controls::knob(
                p,
                rect,
                position(&f, v, id as u32),
                false,
                KnobLook {
                    cap: if f.red && id == 0 {
                        Color::rgb8(154, 46, 41)
                    } else {
                        Color::rgb8(40, 42, 42)
                    },
                    ring: ink,
                },
                th,
            );
            p.text(
                f.labels[id.min(1)],
                Rect::new(rect.x - 4.0, rect.bottom(), rect.w + 8.0, 13.0),
                &label,
            );
        }
        let [change, remove] = buttons(area);
        controls::well_label(p, change, "Change", false, th);
        controls::well_label(p, remove, "×", false, th);
    }
}
