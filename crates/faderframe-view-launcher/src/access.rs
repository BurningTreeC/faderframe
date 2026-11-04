//! The launcher for screen readers and the keyboard: the toolbar (launch
//! quantise and slot recording — Enter: their menu —, Back to Arrangement,
//! Record to Arrangement as a toggle, + Scene), the tracks' headers (Enter
//! selects the track), a group per scene — its launch button, its name
//! (Enter renames it) and a slot per track — and the stop buttons.
//!
//! A slot is read as its track, its clip and what it does (playing,
//! starting, stopping, recording); Enter does what a click on its ▶ does:
//! launch the clip (a held gate or repeat clip: let it go), record into an
//! armed track's empty slot or end the recording, stop the track from an
//! empty slot — or makes a MIDI clip in an empty slot of a MIDI track.
//! Shift+F10 or the Menu key open every slot's, scene's and stop button's
//! menu (launch mode, follow actions, MIDI learn, moving clips).
//!
//! Said by themselves: a clip starting to play, a track stopping or going
//! back to the arrangement, a slot recording starting and ending.

use crate::{Hit, LauncherView, PLAY_W, ROW_H, SCENE_W, STOP_H, TOOLBAR_H, is_midi};
use faderframe_core::{SceneId, TrackId};
use faderframe_project::TrackKind;
use faderframe_project::launcher::{LaunchMode, SlotKey};
use faderframe_session::launcher::LauncherOp;
use faderframe_session::{Action, SelectMode, Session};
use faderframe_ui_canvas::{
    AccessNode, AccessPress, AccessRole, CanvasView, EventCx, HostRequest, Point, Rect, Size,
    access_id,
};

/// Kinds of node (with the track's, scene's ids).
const TOOLBAR: u64 = 0x1A50;
const TOOL: u64 = 0x1A51;
const HEADERS: u64 = 0x1A52;
const HEADER: u64 = 0x1A53;
const SCENE: u64 = 0x1A54;
const SCENE_LAUNCH: u64 = 0x1A55;
const SCENE_NAME: u64 = 0x1A56;
const SLOT: u64 = 0x1A57;
const STOPS: u64 = 0x1A58;
const STOP: u64 = 0x1A59;
const STOP_ALL: u64 = 0x1A5A;

/// The launcher as screen readers were last told it
/// ([`LauncherView::news`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Seen {
    /// Per track: the slot playing (its hash), back in the arrangement.
    pub tracks: Vec<(TrackId, Option<u64>, bool)>,
    /// The slot recording.
    pub recording: Option<(TrackId, SceneId)>,
}

impl Seen {
    pub fn of(model: &Session) -> Self {
        Self {
            tracks: model
                .launch_status()
                .iter()
                .map(|s| (s.track, s.playing.map(|p| p.0), s.arrangement))
                .collect(),
            recording: model.launcher_recording().map(|(t, s, _)| (t, s)),
        }
    }
}

fn hit_id(hit: Hit) -> u64 {
    match hit {
        Hit::Quantize => access_id(&[TOOL, 0]),
        Hit::Back => access_id(&[TOOL, 1]),
        Hit::Record => access_id(&[TOOL, 2]),
        Hit::AddScene => access_id(&[TOOL, 3]),
        Hit::StopAll => access_id(&[STOP_ALL]),
        Hit::Slot { track, scene, .. } => access_id(&[SLOT, track.0, scene.0]),
        Hit::SceneLaunch(s) => access_id(&[SCENE_LAUNCH, s.0]),
        Hit::SceneName(s) => access_id(&[SCENE_NAME, s.0]),
        Hit::TrackStop(t) => access_id(&[STOP, t.0]),
        Hit::Header(t) => access_id(&[HEADER, t.0]),
    }
}

/// The name of the clip in the slot whose hash is `slot`.
fn slot_clip(model: &Session, slot: u64) -> Option<String> {
    let p = model.project();
    let (_, clip) = p.launcher.slots.iter().find(|(k, _)| k.hash() == slot)?;
    p.clip(*clip).map(|c| c.name.clone())
}

fn track_name(model: &Session, track: TrackId) -> String {
    model
        .project()
        .track(track)
        .map_or_else(|| "a track".to_string(), |t| t.name.clone())
}

/// What changed from `before` to `now`, as said.
pub(crate) fn launch_news(before: &Seen, now: &Seen, model: &Session) -> Vec<String> {
    let mut out = Vec::new();
    let mut back = Vec::new();
    for (track, playing, arrangement) in &now.tracks {
        let was = before.tracks.iter().find(|(t, ..)| t == track);
        let (was_playing, was_arranged) = was.map_or((None, true), |w| (w.1, w.2));
        let name = track_name(model, *track);
        if *playing != was_playing {
            match playing {
                Some(slot) => {
                    let clip = slot_clip(model, *slot).unwrap_or_else(|| "A clip".into());
                    out.push(format!("{clip} playing on {name}"));
                }
                None if !*arrangement => out.push(format!("{name} stopped")),
                None => {}
            }
        }
        if *arrangement && !was_arranged {
            back.push(name);
        }
    }
    if !back.is_empty() {
        out.push(format!("Back to the arrangement: {}", back.join(", ")));
    }
    if now.recording != before.recording {
        if let Some((t, s)) = before.recording {
            out.push(format!(
                "Recording into {}, {} ended",
                track_name(model, t),
                scene_name(model, s)
            ));
        }
        if let Some((t, s)) = now.recording {
            out.push(format!(
                "Recording into {}, {}",
                track_name(model, t),
                scene_name(model, s)
            ));
        }
    }
    out
}

fn scene_name(model: &Session, scene: SceneId) -> String {
    model
        .project()
        .launcher
        .scenes
        .iter()
        .find(|s| s.id == scene)
        .map_or_else(|| "a scene".to_string(), |s| s.name.clone())
}

impl LauncherView {
    /// What a track plays, as said.
    fn track_state(model: &Session, track: TrackId) -> &'static str {
        match model.launch_state(track) {
            Some(s) if s.arrangement => "plays the arrangement",
            Some(s) if s.playing.is_some() => "plays the launcher",
            Some(_) => "stopped",
            None => "plays the arrangement",
        }
    }

    /// What Enter does on a slot: what a click on its ▶ does (an empty
    /// slot of a MIDI track: a new clip, as a double click).
    fn slot_action(model: &Session, track: TrackId, scene: SceneId) -> Option<Action> {
        let l = |op| Some(Action::Launcher(op));
        let key = SlotKey { track, scene };
        let recording = model
            .launcher_recording()
            .is_some_and(|(t, s, _)| t == track && s == scene);
        let state = model.launch_state(track);
        let t = model.project().track(track)?;
        match model.project().launcher.clip(track, scene) {
            Some(_) if recording => l(LauncherOp::Record { track, scene }),
            Some(_) => {
                let playing = state
                    .and_then(|s| s.playing)
                    .is_some_and(|(s, _)| s == key.hash());
                let starting = state
                    .and_then(|s| s.queued)
                    .is_some_and(|(s, _)| s == Some(key.hash()));
                let held = matches!(
                    model.project().launcher.launch_of(key).mode,
                    LaunchMode::Gate | LaunchMode::Repeat
                );
                if held && (playing || starting) {
                    l(LauncherOp::Release { track, scene })
                } else {
                    l(LauncherOp::Launch { track, scene })
                }
            }
            None if recording || t.record_arm => l(LauncherOp::Record { track, scene }),
            None if state.is_some_and(|s| s.playing.is_some()) => {
                l(LauncherOp::Launch { track, scene })
            }
            None if matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) => {
                l(LauncherOp::CreateClip { track, scene })
            }
            None => None,
        }
    }

    /// A slot as read: its track, its clip and what it does.
    fn slot_label(model: &Session, track: TrackId, scene: SceneId) -> String {
        let p = model.project();
        let key = SlotKey { track, scene };
        let name = track_name(model, track);
        let recording = model
            .launcher_recording()
            .filter(|(t, s, _)| *t == track && *s == scene);
        let Some(clip) = p.launcher.clip(track, scene).and_then(|c| p.clip(c)) else {
            return match recording {
                Some((.., true)) => format!("{name}: recording, ending"),
                Some(_) => format!("{name}: recording"),
                None if p.track(track).is_some_and(|t| t.record_arm) => {
                    format!("{name}: empty, records")
                }
                None => format!("{name}: empty"),
            };
        };
        let state = model.launch_state(track);
        let playing = state
            .and_then(|s| s.playing)
            .is_some_and(|(s, _)| s == key.hash());
        let queued = state.and_then(|s| s.queued);
        let starting = queued.is_some_and(|(s, _)| s == Some(key.hash()));
        let stopping = playing && queued.is_some_and(|(s, _)| s.is_none());
        let mut text = format!("{name}: {}", clip.name);
        if recording.is_some() {
            text.push_str(", overdubbing");
        } else if stopping {
            text.push_str(", stopping");
        } else if playing {
            text.push_str(", playing");
        } else if starting {
            text.push_str(", starting");
        }
        let launch = p.launcher.launch_of(key);
        if launch.mode != LaunchMode::Trigger {
            text.push_str(&format!(", {}", launch.mode.label().to_lowercase()));
        }
        if launch.legato {
            text.push_str(", legato");
        }
        if p.launcher.follow.contains_key(&key) {
            text.push_str(", with a follow action");
        }
        text
    }

    pub(crate) fn access_nodes(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        let tracks = Self::tracks(model);
        let scenes = &model.project().launcher.scenes;
        let grid = Self::grid(size);
        let [quantize, back, record, add, stop_all] = Self::buttons(size);
        let launched = model.launch_status().iter().any(|s| !s.arrangement);
        let records = model.launcher_records();
        let node = |hit: Hit, role, label: String, r: Rect| {
            let n = AccessNode::new(hit_id(hit), role, label).at(r);
            match self.tooltip(r.center(), size, model) {
                Some(tip) => n.described(tip),
                None => n,
            }
        };
        let l = |op| Action::Launcher(op);
        let mut out = vec![
            AccessNode::new(
                access_id(&[TOOLBAR]),
                AccessRole::Toolbar,
                "Launcher toolbar",
            )
            .at(Rect::new(0.0, 0.0, size.w, TOOLBAR_H))
            .fixed()
            .with_children([
                node(
                    quantize.0,
                    AccessRole::Button,
                    format!(
                        "Launch quantize: {}",
                        model.project().launcher.quantize.label()
                    ),
                    quantize.1,
                )
                .fixed()
                .with_menu(),
                node(
                    back.0,
                    AccessRole::Button,
                    "Back to Arrangement".into(),
                    back.1,
                )
                .fixed()
                .described(if launched {
                    "Some tracks play the launcher; every track plays the arrangement again"
                } else {
                    "Every track plays the arrangement"
                })
                .on_activate(l(LauncherOp::BackToArrangement)),
                node(
                    record.0,
                    AccessRole::ToggleButton,
                    "Record to Arrangement".into(),
                    record.1,
                )
                .fixed()
                .checked(records)
                .on_activate(l(LauncherOp::SetRecord(!records))),
                node(add.0, AccessRole::Button, "Add scene".into(), add.1)
                    .fixed()
                    .on_activate(l(LauncherOp::AddScene { after: None })),
            ]),
        ];
        // The tracks' headers: Enter selects the track.
        let headers = tracks.iter().enumerate().map(|(i, t)| {
            let r = Rect::new(self.col_x(i), TOOLBAR_H, crate::COL_W, grid.y - TOOLBAR_H);
            node(
                Hit::Header(t.id),
                AccessRole::Button,
                format!("{}, {}", t.name, Self::track_state(model, t.id)),
                r,
            )
            .fixed_y()
            .selected(model.selection.tracks.contains(&t.id))
            .on_activate(Action::SelectTracks {
                tracks: vec![t.id],
                mode: SelectMode::Replace,
            })
        });
        out.push(
            AccessNode::new(access_id(&[HEADERS]), AccessRole::Group, "Tracks")
                .at(Rect::new(SCENE_W, TOOLBAR_H, grid.w, grid.y - TOOLBAR_H))
                .fixed_y()
                .with_children(headers),
        );
        // A group per scene: launch, name, slots.
        for (row, scene) in scenes.iter().enumerate() {
            let y = self.row_y(row);
            let any = model
                .project()
                .launcher
                .slots
                .keys()
                .any(|k| k.scene == scene.id);
            let all_playing = any
                && model
                    .project()
                    .launcher
                    .slots
                    .keys()
                    .filter(|k| k.scene == scene.id)
                    .all(|k| {
                        model
                            .launch_state(k.track)
                            .and_then(|s| s.playing)
                            .is_some_and(|(s, _)| s == k.hash())
                    });
            let mut group = AccessNode::new(
                access_id(&[SCENE, scene.id.0]),
                AccessRole::Group,
                format!("Scene {}: {}", row + 1, scene.name),
            )
            .at(Rect::new(0.0, y, size.w, ROW_H))
            .child(
                node(
                    Hit::SceneLaunch(scene.id),
                    AccessRole::Button,
                    format!(
                        "Launch scene {}{}",
                        scene.name,
                        if all_playing { ", playing" } else { "" }
                    ),
                    Rect::new(8.0, y + 4.0, PLAY_W, ROW_H - 8.0),
                )
                .fixed_x()
                .with_menu()
                .on_activate(l(LauncherOp::LaunchScene(scene.id))),
            )
            .child(
                node(
                    Hit::SceneName(scene.id),
                    AccessRole::Button,
                    format!("Scene name: {}", scene.name),
                    Rect::new(8.0 + PLAY_W + 4.0, y, SCENE_W - PLAY_W - 12.0, ROW_H),
                )
                .fixed_x()
                .described("Enter: rename the scene")
                .on_press()
                .with_menu(),
            );
            for (i, t) in tracks.iter().enumerate() {
                let r = Rect::new(self.col_x(i), y, crate::COL_W, ROW_H).inset_xy(2.0, 2.0);
                let clip = model.project().launcher.clip(t.id, scene.id);
                let hit = Hit::Slot {
                    track: t.id,
                    scene: scene.id,
                    clip,
                    play: true,
                };
                let mut n = AccessNode::new(
                    hit_id(hit),
                    AccessRole::Button,
                    Self::slot_label(model, t.id, scene.id),
                )
                .at(r)
                .with_menu();
                if let Some(c) = clip {
                    n = n.selected(model.selection.clips.contains(&c));
                    if model.project().clip(c).is_some_and(is_midi) {
                        n = n.described("Enter: launch; the menu edits it in the piano roll");
                    }
                }
                if let Some(a) = Self::slot_action(model, t.id, scene.id) {
                    n = n.on_activate(a);
                }
                group = group.child(n);
            }
            out.push(group);
        }
        // The stop buttons.
        let stop_y = size.h - STOP_H;
        let mut stops = AccessNode::new(access_id(&[STOPS]), AccessRole::Group, "Stop buttons")
            .at(Rect::new(0.0, stop_y, size.w, STOP_H))
            .fixed_y()
            .child(
                node(
                    stop_all.0,
                    AccessRole::Button,
                    "Stop All Clips".into(),
                    stop_all.1,
                )
                .fixed()
                .with_menu()
                .on_activate(l(LauncherOp::StopAll)),
            );
        for (i, t) in tracks.iter().enumerate() {
            let r = Rect::new(
                self.col_x(i) + 2.0,
                stop_y + 4.0,
                crate::COL_W - 4.0,
                STOP_H - 8.0,
            );
            stops = stops.child(
                node(
                    Hit::TrackStop(t.id),
                    AccessRole::Button,
                    format!("Stop {}, {}", t.name, Self::track_state(model, t.id)),
                    r,
                )
                .fixed_y()
                .with_menu()
                .on_activate(l(LauncherOp::StopTrack(t.id))),
            );
        }
        out.push(stops);
        out
    }

    /// The control a node id names.
    fn hit_of(&self, id: u64, size: Size, model: &Session) -> Option<(Hit, Rect)> {
        for (hit, r) in Self::buttons(size) {
            if hit_id(hit) == id {
                return Some((hit, r));
            }
        }
        let tracks = Self::tracks(model);
        for (row, scene) in model.project().launcher.scenes.iter().enumerate() {
            let y = self.row_y(row);
            let name = Hit::SceneName(scene.id);
            if hit_id(name) == id {
                return Some((name, Rect::new(8.0 + PLAY_W + 4.0, y, SCENE_W, ROW_H)));
            }
            let launch = Hit::SceneLaunch(scene.id);
            if hit_id(launch) == id {
                return Some((launch, Rect::new(8.0, y, PLAY_W, ROW_H)));
            }
            for (i, t) in tracks.iter().enumerate() {
                let hit = Hit::Slot {
                    track: t.id,
                    scene: scene.id,
                    clip: model.project().launcher.clip(t.id, scene.id),
                    play: true,
                };
                if hit_id(hit) == id {
                    return Some((hit, Rect::new(self.col_x(i), y, crate::COL_W, ROW_H)));
                }
            }
        }
        for (i, t) in tracks.iter().enumerate() {
            let hit = Hit::TrackStop(t.id);
            if hit_id(hit) == id {
                let r = Rect::new(self.col_x(i), size.h - STOP_H, crate::COL_W, STOP_H);
                return Some((hit, r));
            }
        }
        None
    }

    pub(crate) fn press(
        &mut self,
        id: u64,
        press: AccessPress,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some((hit, r)) = self.hit_of(id, size, model) else {
            return false;
        };
        let at = Point::new(r.x, r.bottom());
        match (hit, press) {
            (Hit::SceneName(scene), AccessPress::Activate) => {
                let row = model
                    .project()
                    .launcher
                    .scenes
                    .iter()
                    .position(|s| s.id == scene)
                    .unwrap_or(0);
                self.rename_scene(model, scene, row, cx);
                true
            }
            (_, AccessPress::Menu) => match self.menu_for(hit, model) {
                Some(items) => {
                    cx.request(HostRequest::ContextMenu { at, items });
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    /// Launches, stops and recordings since the last call, as said.
    pub(crate) fn news(&mut self, model: &Session) -> Vec<String> {
        let now = Seen::of(model);
        match self.seen.replace(now.clone()) {
            Some(before) if before != now => launch_news(&before, &now, model),
            _ => Vec::new(),
        }
    }
}
