//! The mixer for screen readers and the keyboard: a group per strip (its
//! track's name) with the volume fader and pan as sliders the arrows move,
//! mute, solo and record arm as toggles, and the inserts as buttons that
//! open their editors.

use crate::MixerView;
use faderframe_core::gain::{SILENCE_DB, format_db};
use faderframe_core::pan::format_pan;
use faderframe_project::{Command, Track, TrackKind};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{AccessNode, AccessRole, Size, access_id};

/// Kinds of node (with the track id: the node's id).
const STRIP: u64 = 1;
const VOLUME: u64 = 2;
const PAN: u64 = 3;
const MUTE: u64 = 4;
const SOLO: u64 = 5;
const ARM: u64 = 6;
const INSERT: u64 = 7;
const PAN_LEFT: u64 = 8;
const PAN_RIGHT: u64 = 9;

/// The fader's floor and top as a screen reader reports them.
const FLOOR_DB: f64 = -80.0;
const TOP_DB: f64 = 12.0;

impl MixerView {
    fn strip_node(
        &self,
        t: &Track,
        rect: faderframe_ui_canvas::Rect,
        model: &Session,
    ) -> AccessNode<Action> {
        let id = t.id;
        let l = self.layout_for(rect, t, model.project());
        let kind = match t.kind {
            TrackKind::Master => "master",
            TrackKind::Bus => "bus",
            TrackKind::Aux => "aux",
            TrackKind::Vca => "VCA",
            TrackKind::Folder => "folder",
            TrackKind::Midi => "MIDI track",
            TrackKind::Instrument => "instrument track",
            _ => "track",
        };
        let mut strip = AccessNode::new(
            access_id(&[STRIP, id.0]),
            AccessRole::Group,
            format!("{} {kind}", t.name),
        )
        .at(rect);
        if t.kind == TrackKind::Folder {
            return strip;
        }
        // In a cue mix the fader is the track's send level there.
        let cue = model.mixer_cue().map(|c| c.name.clone());
        let db = Self::fader_db(model, t);
        let base = if db <= SILENCE_DB { -80.0 } else { db };
        let volume = |db: f32| Self::fader_command(model, t, db).map(Action::Edit);
        let mut fader = AccessNode::new(
            access_id(&[VOLUME, id.0]),
            AccessRole::Slider,
            match (&cue, t.kind) {
                (Some(c), TrackKind::Master) => format!("{c} Level"),
                (Some(c), _) => format!("Level in {c}"),
                (None, _) => "Volume".into(),
            },
        )
        .at(l.fader)
        .value(
            f64::from(db).clamp(FLOOR_DB, TOP_DB),
            FLOOR_DB,
            TOP_DB,
            format_db(db),
        );
        if let (Some(up), Some(down)) = (volume(base + 0.5), volume(base - 0.5)) {
            fader = fader.on_step(up, down);
        }
        strip = strip.child(fader);
        let surround = matches!(t.layout, faderframe_core::ChannelLayout::Surround(_))
            || crate::room_panned(model, t).is_some();
        if t.kind != TrackKind::Vca
            && !surround
            && t.kind != TrackKind::Midi
            && crate::dual_shown(model, t)
        {
            // Dual pan: a slider per input channel.
            let dual = model.shown_dual_pan(t);
            for (i, (knob, _)) in crate::dual_pan_rects(&l).into_iter().enumerate() {
                let right = i == 1;
                let pan = dual.side(right);
                let set = |p: f32| {
                    Action::Edit(Command::SetTrackDualPan {
                        track: id,
                        pan: dual.with_side(right, p.clamp(-1.0, 1.0)),
                    })
                };
                let (kind, name) = if right {
                    (PAN_RIGHT, "Right Pan")
                } else {
                    (PAN_LEFT, "Left Pan")
                };
                strip = strip.child(
                    AccessNode::new(access_id(&[kind, id.0]), AccessRole::Slider, name)
                        .at(knob)
                        .value(f64::from(pan), -1.0, 1.0, format_pan(pan))
                        .on_step(set(pan + 0.05), set(pan - 0.05)),
                );
            }
        } else if t.kind != TrackKind::Vca
            && !surround
            && t.kind != TrackKind::Midi
            && let (Some(up), Some(down)) = (
                Self::pan_command(
                    model,
                    t,
                    (Self::pan_shown(model, t) + 0.05).clamp(-1.0, 1.0),
                ),
                Self::pan_command(
                    model,
                    t,
                    (Self::pan_shown(model, t) - 0.05).clamp(-1.0, 1.0),
                ),
            )
        {
            let pan = Self::pan_shown(model, t);
            strip = strip.child(
                AccessNode::new(
                    access_id(&[PAN, id.0]),
                    AccessRole::Slider,
                    match &cue {
                        Some(c) => format!("Pan in {c}"),
                        None if crate::is_balance(model, t) => "Balance".into(),
                        None => "Pan".into(),
                    },
                )
                .at(l.pan_knob)
                .value(f64::from(pan), -1.0, 1.0, format_pan(pan))
                .on_step(Action::Edit(up), Action::Edit(down)),
            );
        }
        let mute = Self::mute_shown(model, t);
        let mut mute_node = AccessNode::new(
            access_id(&[MUTE, id.0]),
            AccessRole::ToggleButton,
            match (&cue, t.kind) {
                (Some(c), TrackKind::Master) => format!("Mute {c}"),
                (Some(c), _) => format!("Off in {c}"),
                (None, _) => "Mute".into(),
            },
        )
        .at(l.mute)
        .checked(mute);
        if let Some(cmd) = Self::mute_command(model, t) {
            mute_node = mute_node.on_activate(Action::Edit(cmd));
        }
        strip = strip.child(mute_node);
        if t.kind != TrackKind::Master && cue.is_none() {
            strip = strip.child(
                AccessNode::new(access_id(&[SOLO, id.0]), AccessRole::ToggleButton, "Solo")
                    .at(l.solo)
                    .checked(t.solo)
                    .on_activate(Action::Edit(Command::SetTrackSolo {
                        track: id,
                        on: !t.solo,
                    })),
            );
        }
        if t.kind.has_clips() {
            strip = strip.child(
                AccessNode::new(
                    access_id(&[ARM, id.0]),
                    AccessRole::ToggleButton,
                    "Record arm",
                )
                .at(l.record)
                .checked(t.record_arm)
                .on_activate(Action::Edit(Command::SetTrackRecordArm {
                    track: id,
                    on: !t.record_arm,
                })),
            );
        }
        let rects = l.inserts.clone().unwrap_or_default();
        for (i, s) in t.inserts.iter().enumerate() {
            let named = t.name_of(s);
            let name = named.trim_start_matches("FaderFrame ");
            let label = if s.bypass {
                format!("Insert {}: {name}, bypassed", i + 1)
            } else {
                format!("Insert {}: {name}", i + 1)
            };
            let mut n = AccessNode::new(
                access_id(&[INSERT, id.0, s.id.0]),
                AccessRole::Button,
                label,
            )
            .described("Enter opens its editor")
            .on_activate(Action::OpenPluginEditor {
                track: id,
                plugin: s.id,
                generic: false,
            });
            if let Some(r) = rects.get(i) {
                n = n.at(*r);
            }
            strip = strip.child(n);
        }
        strip
    }

    /// The strips as a screen reader and the keyboard see them.
    pub(crate) fn access_nodes(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        let mut out = Vec::new();
        if !self.master_only {
            for (i, t) in Self::channel_tracks(model).into_iter().enumerate() {
                out.push(self.strip_node(t, self.strip_rect(i, size), model));
            }
        }
        if (self.master_only || !self.hide_master)
            && let Some(m) = model.project().master()
        {
            out.push(self.strip_node(m, self.master_rect(size), model));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_engine::EngineConfig;
    use faderframe_ui_canvas::{CanvasView, RecordingPainter, Theme, access};

    #[test]
    fn the_strips_are_groups_of_controls() {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let mut view = MixerView::new(Theme::default());
        let size = Size::new(1400.0, 700.0);
        view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
        let nodes = view.accessible(size, &s);
        let names: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert!(names.iter().any(|n| n.starts_with("Drums")), "{names:?}");
        assert!(names.last().unwrap().ends_with("master"), "{names:?}");
        // The first strip (stereo drums): volume, balance, mute, solo, arm.
        let first = &nodes[0];
        let kinds: Vec<&str> = first.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            &kinds[..5],
            ["Volume", "Balance", "Mute", "Solo", "Record arm"]
        );
        // Up on the fader raises it half a dB.
        let fader = &first.children[0];
        let before = fader.value.as_ref().unwrap().now;
        s.dispatch(fader.increment.clone().unwrap()).unwrap();
        view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
        let nodes = view.accessible(size, &s);
        let after = access::find_node(&nodes, fader.id).unwrap();
        assert!((after.value.as_ref().unwrap().now - before - 0.5).abs() < 1e-6);
        // Enter on Mute mutes; the node says so.
        let mute = &nodes[0].children[2];
        assert_eq!(mute.checked, Some(false));
        s.dispatch(mute.activate.clone().unwrap()).unwrap();
        let nodes = view.accessible(size, &s);
        let mute = access::find_node(&nodes, mute.id).unwrap();
        assert_eq!(mute.spoken(), "Mute, on");
        // Tab order runs strip by strip.
        let order = access::focus_order(&nodes);
        assert_eq!(order[0].label, "Volume");
        assert!(order.len() > 10);
    }
}
