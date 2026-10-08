//! The arranger for screen readers and the keyboard: a group per track
//! (its name and kind) with mute, solo, record arm and input monitoring as
//! toggles, the volume as a slider the arrows move, and its clips as items
//! (where they start and end; Enter selects one and puts the playhead on
//! it, so the edit commands apply to it).
//!
//! Tab is the arranger's own (tab to transient / clip boundary): Ctrl+Tab
//! moves the keyboard onto these controls, then Tab moves along them.

use crate::ArrangerView;
use faderframe_core::gain::{SILENCE_DB, format_db};
use faderframe_project::{Command, MonitorMode, TrackKind};
use faderframe_session::{Action, SelectMode, Session, TransportAction};
use faderframe_ui_canvas::{AccessNode, AccessRole, Rect, Size, access_id};

const TRACK: u64 = 11;
const MUTE: u64 = 12;
const SOLO: u64 = 13;
const ARM: u64 = 14;
const MONITOR: u64 = 15;
const VOLUME: u64 = 16;
const CLIP: u64 = 17;

impl ArrangerView {
    pub(crate) fn access_nodes(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        let p = model.project();
        let tl = &p.timeline;
        let mut out = Vec::new();
        for (i, t) in Self::lane_tracks(model).into_iter().enumerate() {
            let id = t.id;
            let row = self.row_rect(i, size);
            let header = crate::header::HeaderLayout::new(self.header_rect(model, t, row.y));
            let kind = match t.kind {
                TrackKind::Audio => "audio track",
                TrackKind::Instrument => "instrument track",
                TrackKind::Midi => "MIDI track",
                TrackKind::Bus => "bus",
                TrackKind::Aux => "aux",
                TrackKind::Vca => "VCA",
                TrackKind::Folder => "folder",
                _ => "track",
            };
            let clips = p.clips_of(id);
            let mut node = AccessNode::new(
                access_id(&[TRACK, id.0]),
                AccessRole::Group,
                format!("{} {kind}", t.name),
            )
            .at(row)
            .described(format!(
                "{} clip{}",
                clips.len(),
                if clips.len() == 1 { "" } else { "s" }
            ));
            let toggle = |kind: u64, label: &str, r: Rect, on: bool, cmd: Command| {
                AccessNode::new(access_id(&[kind, id.0]), AccessRole::ToggleButton, label)
                    .at(r)
                    .checked(on)
                    .on_activate(Action::Edit(cmd))
            };
            if t.kind != TrackKind::Folder {
                node = node.child(toggle(
                    MUTE,
                    "Mute",
                    header.mute,
                    model.shown_mute(t),
                    Command::SetTrackMute {
                        track: id,
                        on: !t.mute,
                    },
                ));
                node = node.child(toggle(
                    SOLO,
                    "Solo",
                    header.solo,
                    t.solo,
                    Command::SetTrackSolo {
                        track: id,
                        on: !t.solo,
                    },
                ));
            }
            if t.kind.has_clips() {
                node = node.child(toggle(
                    ARM,
                    "Record arm",
                    header.record,
                    t.record_arm,
                    Command::SetTrackRecordArm {
                        track: id,
                        on: !t.record_arm,
                    },
                ));
            }
            if t.kind == TrackKind::Audio {
                let on = t.monitor != MonitorMode::Off;
                node = node.child(toggle(
                    MONITOR,
                    "Input monitoring",
                    header.monitor,
                    on,
                    Command::SetTrackMonitor {
                        track: id,
                        mode: if on {
                            MonitorMode::Off
                        } else {
                            MonitorMode::Input
                        },
                    },
                ));
            }
            if t.kind.has_audio() || t.kind == TrackKind::Vca {
                let db = model.shown_volume_db(t);
                let base = if db <= SILENCE_DB { -80.0 } else { db };
                let set = |db: f32| Action::Edit(Command::SetTrackVolume { track: id, db });
                node = node.child(
                    AccessNode::new(access_id(&[VOLUME, id.0]), AccessRole::Slider, "Volume")
                        .at(header.volume)
                        .value(f64::from(db).max(-80.0), -80.0, 12.0, format_db(db))
                        .on_step(set(base + 0.5), set(base - 0.5)),
                );
            }
            let mut clips = clips;
            clips.sort_by_key(|c| c.start);
            for c in clips {
                let end = c.end(tl, p.sample_rate);
                let mut label = format!(
                    "{}, {} to {}",
                    c.name,
                    tl.format_bbt(c.start),
                    tl.format_bbt(end)
                );
                if c.muted {
                    label.push_str(", muted");
                }
                let selected = model.selection.clips.contains(&c.id);
                node = node.child(
                    AccessNode::new(access_id(&[CLIP, c.id.0]), AccessRole::ListItem, label)
                        .at(self.clip_rect(c, row, model))
                        .selected(selected)
                        .described("Enter selects it and puts the playhead on it")
                        .on_activate(Action::Several(vec![
                            Action::SelectClips {
                                clips: vec![c.id],
                                mode: SelectMode::Replace,
                            },
                            Action::Transport(TransportAction::Locate(c.start)),
                        ])),
                );
            }
            out.push(node);
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
    fn tracks_list_their_controls_and_clips() {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let mut view = ArrangerView::new(Theme::default());
        let size = Size::new(1400.0, 800.0);
        view.paint(&mut RecordingPainter::new(), size, &s, &Theme::default());
        let nodes = view.accessible(size, &s);
        let drums = &nodes[0];
        assert_eq!(drums.label, "Drums audio track");
        let labels: Vec<&str> = drums.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            &labels[..5],
            ["Mute", "Solo", "Record arm", "Input monitoring", "Volume"]
        );
        let clip = drums
            .children
            .iter()
            .find(|c| c.role == AccessRole::ListItem)
            .unwrap();
        assert!(clip.label.starts_with("Drum Loop, 1.1"), "{}", clip.label);
        // Enter on a clip selects it.
        s.dispatch(clip.activate.clone().unwrap()).unwrap();
        let nodes = view.accessible(size, &s);
        assert_eq!(
            access::find_node(&nodes, clip.id).unwrap().selected,
            Some(true)
        );
        assert!(view.uses_tab());
    }
}
