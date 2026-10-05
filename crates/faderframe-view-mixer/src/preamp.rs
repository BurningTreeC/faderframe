//! Dedicated channel input faceplates, drawn by the native canvas.
use super::*;
use faderframe_core::{ParameterId, builtin::PREAMPS};
use faderframe_project::PluginSlot;
use faderframe_ui_canvas::TextStyle;

pub(super) fn value(slot: &PluginSlot, id: u32) -> f64 {
    slot.parameters
        .iter()
        .find(|p| p.id == ParameterId(id))
        .map_or(if id == 0 { 0.5 } else { 0.0 }, |p| p.value)
}
pub(super) fn position(value: f64, id: u32) -> f32 {
    (if id == 0 {
        value
    } else {
        (value + 60.0) / 72.0
    })
    .clamp(0.0, 1.0) as f32
}
pub(super) fn plain(position: f32, id: u32) -> f64 {
    if id == 0 {
        f64::from(position)
    } else {
        f64::from(position) * 72.0 - 60.0
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
            controls::well_label(p, area, "+ PREAMP", true, th);
            return;
        };
        let i = faderframe_core::builtin::preamp_index(&slot.plugin.id).unwrap_or(0);
        let (_, name, rgb) = PREAMPS[i];
        let base = Color::rgb8(rgb[0], rgb[1], rgb[2]);
        controls::panel(p, area, base.lighten(0.14), base.darken(0.16), th);
        let ink = if i >= 4 {
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
                position(v, id as u32),
                false,
                KnobLook {
                    cap: if i == 0 && id == 0 {
                        Color::rgb8(154, 46, 41)
                    } else {
                        Color::rgb8(40, 42, 42)
                    },
                    ring: ink,
                },
                th,
            );
            p.text(
                if id == 0 { "Gain" } else { "Master" },
                Rect::new(rect.x - 4.0, rect.bottom(), rect.w + 8.0, 13.0),
                &label,
            );
        }
        let [change, remove] = buttons(area);
        controls::well_label(p, change, "Change", false, th);
        controls::well_label(p, remove, "×", false, th);
    }
}
