//! Edits: every change is a parameter edit of the plugin's slot, so it is
//! undoable and automatable like any other; changes that belong together
//! (a new band, a paste, an A/B switch) are one undo step.

use super::EqView;
use super::geometry::{parse_db, parse_freq, parse_value};
use faderframe_core::PluginInstanceId;
use faderframe_core::{ParameterId, TrackId};
use faderframe_plugin_host::eq::design::{BandShape, BandType};
use faderframe_plugin_host::eq::{
    BANDS, BandParams, Field, GLOBALS, Placement, band_id, band_index, parameters,
};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_project::Command;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{EventCx, HostRequest, Rect};
use std::sync::Arc;

/// The plugin's tap and the bands in use, with their settings.
pub(crate) type UsedBands = (Arc<AnalysisTap>, Vec<(usize, BandParams)>);

/// A band's settings, field by field (what is copied and pasted).
pub(crate) type BandValues = Vec<(Field, f64)>;

/// Where copied bands wait: the session's settings of no plugin in
/// particular, so every EQ editor (and its menus) can reach them.
pub(crate) const CLIPBOARD: PluginInstanceId = PluginInstanceId(u64::MAX);

fn clip_key(band: usize, field: Field) -> String {
    format!("eq.clip.{band}.{}", field.index())
}

/// The bands on the clipboard.
pub(crate) fn clipboard(model: &Session) -> Vec<BandValues> {
    let n = model
        .device_view(CLIPBOARD, "eq.clip.count")
        .unwrap_or(0.0)
        .max(0.0) as usize;
    (0..n.min(BANDS))
        .map(|i| {
            Field::ALL
                .iter()
                .filter_map(|f| {
                    model
                        .device_view(CLIPBOARD, &clip_key(i, *f))
                        .map(|v| (*f, v))
                })
                .collect()
        })
        .collect()
}

/// Every field of a fresh band with `shape` (so nothing of a slot's earlier
/// band carries over).
pub(crate) fn fresh_band(shape: &BandShape) -> BandValues {
    let q = if shape.kind == BandType::Bell || shape.q > 0.0 {
        shape.q
    } else {
        std::f64::consts::FRAC_1_SQRT_2
    };
    vec![
        (Field::Type, shape.kind.index() as f64),
        (Field::Freq, shape.freq.clamp(10.0, 30_000.0)),
        (
            Field::Gain,
            if shape.kind.has_gain() {
                shape.gain.clamp(-30.0, 30.0)
            } else {
                0.0
            },
        ),
        (Field::Q, q.clamp(0.025, 40.0)),
        (Field::Slope, shape.kind.snap_slope(shape.slope)),
        (Field::Placement, 0.0),
        (Field::Range, 0.0),
        (Field::Threshold, 0.0),
        (Field::Key, 0.0),
        (Field::Attack, 0.5),
        (Field::Release, 0.5),
        (Field::Trigger, 0.0),
        (Field::TriggerLow, 10.0),
        (Field::TriggerHigh, 30_000.0),
        (Field::Spectral, 0.0),
        (Field::Density, 0.5),
        (Field::SpectralTilt, 1.0),
        (Field::Dynamics, 0.0),
        (Field::DynBypass, 0.0),
        (Field::Enabled, 1.0),
    ]
}

/// The default shape of a new band of `kind` at `freq` with `gain`.
pub(crate) fn new_shape(kind: BandType, freq: f64, gain: f64) -> BandShape {
    BandShape {
        kind,
        freq,
        gain: if kind.has_gain() { gain } else { 0.0 },
        q: if kind == BandType::Bell {
            1.0
        } else {
            std::f64::consts::FRAC_1_SQRT_2
        },
        slope: if kind.is_cut() { 24.0 } else { 12.0 },
    }
}

impl EqView {
    /// The track the plugin's commands name.
    pub(crate) fn owner(&self, model: &Session) -> Option<TrackId> {
        model.plugin_owner(self.device.plugin).map(|(t, _)| t)
    }

    pub(crate) fn command(&self, track: TrackId, id: ParameterId, value: f64) -> Command {
        Command::SetPluginParameter {
            track,
            plugin: self.device.plugin,
            parameter: id,
            value: Some(value),
        }
    }

    /// Several changes as one undo step.
    pub(crate) fn batch(
        &self,
        model: &Session,
        label: &str,
        changes: &[(ParameterId, f64)],
    ) -> Option<Action> {
        let track = self.owner(model)?;
        Some(Action::Edit(Command::Batch {
            label: label.into(),
            commands: changes
                .iter()
                .map(|(id, v)| self.command(track, *id, *v))
                .collect(),
        }))
    }

    pub(crate) fn apply(
        &self,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
        label: &str,
        changes: &[(ParameterId, f64)],
    ) {
        if let Some(a) = self.batch(model, label, changes) {
            cx.emit(a);
        }
    }

    /// One change (inside a gesture, part of its undo step).
    pub(crate) fn set(
        &self,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
        id: ParameterId,
        v: f64,
    ) {
        self.device.set(model, cx, id, v);
    }

    /// One change as its own undo step.
    pub(crate) fn set_once(
        &self,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
        id: ParameterId,
        v: f64,
    ) {
        self.apply(model, cx, "EQ", &[(id, v)]);
    }

    /// The same field of every selected band set to `v` (one undo step).
    pub(crate) fn set_selected(
        &self,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
        field: Field,
        v: f64,
    ) {
        let changes: Vec<_> = self
            .selected
            .iter()
            .map(|b| (band_id(*b, field), v))
            .collect();
        self.apply(model, cx, "EQ Band", &changes);
    }

    /// An action setting a parameter (for menus).
    pub(crate) fn action(&self, model: &Session, id: ParameterId, value: f64) -> Option<Action> {
        Some(Action::Edit(self.command(self.owner(model)?, id, value)))
    }

    /// The bands in use, with their settings.
    pub(crate) fn used(&self, model: &Session) -> Option<UsedBands> {
        let tap = self.device.tap(model)?;
        let bands = (0..BANDS)
            .map(|b| (b, BandParams::read(&tap.params, b)))
            .filter(|(_, p)| p.used)
            .collect();
        Some((tap, bands))
    }

    /// Unused band slots.
    pub(crate) fn free_slots(tap: &AnalysisTap) -> Vec<usize> {
        (0..BANDS)
            .filter(|b| !BandParams::read(&tap.params, *b).used)
            .collect()
    }

    /// Add a band; `dynamic` makes it dynamic by that range, `spectral`
    /// spectral. Inside a gesture its settings join the gesture's undo
    /// step; otherwise they are one of their own. Returns its slot.
    pub(crate) fn add_band(
        &mut self,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
        shape: BandShape,
        dynamic: Option<f64>,
        spectral: bool,
        in_gesture: bool,
    ) -> Option<usize> {
        let tap = self.device.tap(model)?;
        let band = *Self::free_slots(&tap).first()?;
        let mut values = fresh_band(&shape);
        if let Some(range) = dynamic.filter(|_| shape.kind.has_gain()) {
            values.push((Field::Range, range));
            if spectral {
                values.push((Field::Spectral, 1.0));
            }
        }
        let changes: Vec<_> = values
            .iter()
            .map(|(f, v)| (band_id(band, *f), *v))
            .collect();
        if in_gesture {
            for (id, v) in changes {
                self.set(model, cx, id, v);
            }
        } else {
            self.apply(model, cx, "Add EQ Band", &changes);
        }
        self.select_only(band);
        Some(band)
    }

    /// Set bands in free slots from `shapes` in one undo step (EQ Match);
    /// returns their slots.
    pub(crate) fn add_bands(
        &mut self,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
        label: &str,
        shapes: &[BandShape],
    ) -> Vec<usize> {
        let Some(tap) = self.device.tap(model) else {
            return Vec::new();
        };
        let slots: Vec<usize> = Self::free_slots(&tap)
            .into_iter()
            .take(shapes.len())
            .collect();
        let mut changes = Vec::new();
        for (slot, shape) in slots.iter().zip(shapes) {
            changes.extend(
                fresh_band(shape)
                    .into_iter()
                    .map(|(f, v)| (band_id(*slot, f), v)),
            );
        }
        self.apply(model, cx, label, &changes);
        slots
    }

    pub(crate) fn remove(
        &mut self,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
        bands: &[usize],
    ) {
        let changes: Vec<_> = bands
            .iter()
            .map(|b| (band_id(*b, Field::Enabled), 0.0))
            .collect();
        self.apply(model, cx, "Delete EQ Bands", &changes);
        self.selected.retain(|b| !bands.contains(b));
        if self.focus.is_some_and(|f| bands.contains(&f)) {
            self.focus = self.selected.last().copied();
        }
    }

    /// A band's fields as they are.
    pub(crate) fn values(tap: &AnalysisTap, band: usize) -> BandValues {
        Field::ALL
            .iter()
            .filter(|f| **f != Field::Enabled)
            .map(|f| (*f, f64::from(tap.params.get(band_index(band, *f)))))
            .chain([(
                Field::Enabled,
                f64::from(tap.params.get(band_index(band, Field::Enabled))),
            )])
            .collect()
    }

    /// Copy bands to the clipboard (an action, so menus can do it).
    pub(crate) fn copy_action(&self, model: &Session, bands: &[usize]) -> Option<Action> {
        let tap = self.device.tap(model)?;
        let mut values = vec![("eq.clip.count".to_string(), bands.len() as f64)];
        for (i, b) in bands.iter().enumerate() {
            for (f, v) in Self::values(&tap, *b) {
                values.push((clip_key(i, f), v));
            }
        }
        Some(Action::SetDeviceView {
            plugin: CLIPBOARD,
            values,
        })
    }

    /// Paste the clipboard's bands into free slots: the action and the
    /// slots.
    pub(crate) fn paste_action(&self, model: &Session) -> Option<(Action, Vec<usize>)> {
        let tap = self.device.tap(model)?;
        let copied = clipboard(model);
        if copied.is_empty() {
            return None;
        }
        let mut changes = Vec::new();
        let mut placed = Vec::new();
        for (slot, values) in Self::free_slots(&tap).iter().zip(&copied) {
            changes.extend(values.iter().map(|(f, v)| (band_id(*slot, *f), *v)));
            placed.push(*slot);
        }
        Some((self.batch(model, "Paste EQ Bands", &changes)?, placed))
    }

    pub(crate) fn paste(&mut self, model: &Session, cx: &mut EventCx<'_, Action>) {
        if let Some((a, placed)) = self.paste_action(model) {
            cx.emit(a);
            self.selected = placed;
            self.focus = self.selected.last().copied();
        }
    }

    /// Split `bands` into left and right (or mid and side) halves: each
    /// keeps one side, a copy takes the other.
    pub(crate) fn split_action(
        &self,
        model: &Session,
        bands: &[usize],
        mid_side: bool,
    ) -> Option<Action> {
        let tap = self.device.tap(model)?;
        let (a, b) = if mid_side {
            (Placement::Mid, Placement::Side)
        } else {
            (Placement::Left, Placement::Right)
        };
        let mut free = Self::free_slots(&tap).into_iter();
        let mut changes = Vec::new();
        for &band in bands {
            let Some(slot) = free.next() else { break };
            let mut values = Self::values(&tap, band);
            changes.push((band_id(band, Field::Placement), a.index() as f64));
            for (f, v) in &mut values {
                if *f == Field::Placement {
                    *v = b.index() as f64;
                }
            }
            changes.extend(values.iter().map(|(f, v)| (band_id(slot, *f), *v)));
        }
        self.batch(model, "Split EQ Bands", &changes)
    }

    /// Ask for a typed value of a band's field.
    pub(crate) fn type_value(
        &self,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
        band: usize,
        field: Field,
        at: Rect,
    ) {
        let Some(track) = self.owner(model) else {
            return;
        };
        let index = band_index(band, field);
        let info = parameters()[index].clone();
        let current = self.device.value(model, index);
        let initial = match field {
            Field::Freq | Field::TriggerLow | Field::TriggerHigh => {
                faderframe_plugin_host::eq::format_hz(current)
            }
            Field::Q => format!("{current:.2}"),
            Field::Slope => format!("{current:.1}"),
            _ => format!("{current:.1}"),
        };
        let plugin = self.device.plugin;
        let id = band_id(band, field);
        cx.request(HostRequest::TextInput {
            at,
            initial,
            commit: Box::new(move |text| {
                let v = match field {
                    Field::Freq | Field::TriggerLow | Field::TriggerHigh => parse_freq(text),
                    Field::Gain | Field::Range | Field::Threshold => parse_db(text),
                    _ => parse_value(text, info.min, info.max),
                }?;
                Some(Action::Edit(Command::SetPluginParameter {
                    track,
                    plugin,
                    parameter: id,
                    value: Some(info.clamp(v)),
                }))
            }),
        });
    }

    /// Every parameter's value (A/B).
    pub(crate) fn snapshot(tap: &AnalysisTap) -> Vec<f64> {
        (0..GLOBALS + BANDS * faderframe_plugin_host::eq::FIELDS)
            .map(|i| f64::from(tap.params.get(i)))
            .collect()
    }

    /// Switch between the A and B settings.
    pub(crate) fn ab_switch(&mut self, model: &Session, cx: &mut EventCx<'_, Action>, to: usize) {
        if to == self.ab_side {
            return;
        }
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        let now = Self::snapshot(&tap);
        let target = self.ab[to].clone().unwrap_or_else(|| now.clone());
        self.ab[self.ab_side] = Some(now.clone());
        self.ab_side = to;
        let infos = parameters();
        let changes: Vec<_> = infos
            .iter()
            .zip(now.iter().zip(&target))
            .filter(|(_, (a, b))| (*a - *b).abs() > 1e-9)
            .map(|(info, (_, b))| (info.id, *b))
            .collect();
        if !changes.is_empty() {
            self.apply(model, cx, "EQ A/B", &changes);
        }
    }

    /// Copy the current settings to the other side.
    pub(crate) fn ab_copy(&mut self, model: &Session) {
        if let Some(tap) = self.device.tap(model) {
            self.ab[1 - self.ab_side] = Some(Self::snapshot(&tap));
        }
    }
}
