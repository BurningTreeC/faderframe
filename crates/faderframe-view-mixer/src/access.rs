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

/// The fader's floor and top as a screen reader reports them.
const FLOOR_DB: f64 = -80.0;
const TOP_DB: f64 = 12.0;

fn volume(track: faderframe_core::TrackId, db: f32) -> Action {
    Action::Edit(Command::SetTrackVolume { track, db })
}

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
        let db = model.shown_volume_db(t);
        let base = if db <= SILENCE_DB { -80.0 } else { db };
        strip = strip.child(
            AccessNode::new(access_id(&[VOLUME, id.0]), AccessRole::Slider, "Volume")
                .at(l.fader)
                .value(
                    f64::from(db).clamp(FLOOR_DB, TOP_DB),
                    FLOOR_DB,
                    TOP_DB,
                    format_db(db),
                )
                .on_step(volume(id, base + 0.5), volume(id, base - 0.5)),
        );
        let surround = matches!(t.layout, faderframe_core::ChannelLayout::Surround(_))
            || model.project().surround_panned(t).is_some();
        if t.kind != TrackKind::Vca && !surround && t.kind != TrackKind::Midi {
            let pan = model.shown_pan(t);
            let set = |p: f32| {
                Action::Edit(Command::SetTrackPan {
                    track: id,
                    pan: p.clamp(-1.0, 1.0),
                })
            };
            strip = strip.child(
                AccessNode::new(access_id(&[PAN, id.0]), AccessRole::Slider, "Pan")
                    .at(l.pan_knob)
                    .value(f64::from(pan), -1.0, 1.0, format_pan(pan))
                    .on_step(set(pan + 0.05), set(pan - 0.05)),
            );
        }
        let mute = model.shown_mute(t);
        strip = strip.child(
            AccessNode::new(access_id(&[MUTE, id.0]), AccessRole::ToggleButton, "Mute")
                .at(l.mute)
                .checked(mute)
                .on_activate(Action::Edit(Command::SetTrackMute {
                    track: id,
                    on: !t.mute,
                })),
        );
        if t.kind != TrackKind::Master {
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
            let name = s.plugin.name.trim_start_matches("FaderFrame ");
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
        // The first strip: volume, pan, mute, solo, arm.
        let first = &nodes[0];
        let kinds: Vec<&str> = first.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(&kinds[..5], ["Volume", "Pan", "Mute", "Solo", "Record arm"]);
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
