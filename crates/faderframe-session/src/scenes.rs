//! Mix scenes in the session (see [`faderframe_project::scenes`]):
//! storing and editing them, recalling one at once or crossfaded, the
//! morph between scenes A and B, and comparing two scenes — or the mix
//! with a scene — without losing the mix.
//!
//! **Recalls are ordinary edits.** A recall's commands are the existing
//! mixer commands ([`commands_to`]), so it is one undo step and every
//! view, the engine and automation see it as they see a hand on the
//! console. An instant recall is that batch. A crossfade, a morph and a
//! compare move the project through intermediate values *unrecorded*:
//! each tick's values are applied with their inverses kept (the first
//! per parameter, as the history coalesces a gesture), the views show the
//! faders moving, and when the work ends the session takes those edits
//! back and applies the last value of each as one batch — so the undo
//! step holds exactly where the console ended, and undoing it goes back
//! to where it was before the work began. Anything else that would edit
//! the project first ends the work: a crossfade stops where it is (one
//! step, "(Stopped)"), a morph ends where the slider stands, a compare
//! keeps the side heard (the edit is meant for what is heard). Saving and
//! rendering settle the work first too; opening a project forgets it.
//!
//! **The sound glides on the audio thread.** While a stream runs, a
//! crossfade's continuous values (faders and send levels in fader travel,
//! pans, positions, the console's drive) are glided by the engine
//! (`EngineController::glide_mix`): sample-steady, however the ticks
//! fall; a morph's and a compare's steps glide over a few tens of
//! milliseconds. The ticks' parameter syncs leave those slots alone, and
//! they are let go once the glide has written the values the project
//! ends on. Devices' continuous parameters move per tick, as a knob's
//! do (plugins smooth their own). Switched values — mute, solo,
//! polarity, delay, pan mode, send and cue switches, bypass, stepped
//! parameters, device states — change at the fade's switch point. Without
//! a stream the ticks alone move the values.
//!
//! **Automation:** a recall moves the mix as the controls would. A
//! parameter a lane drives (Read) keeps following its lane; the recall
//! sets the value under it, heard where the lane no longer drives. While
//! playing, lanes in Touch, Latch or Write record the recall as they
//! record a control — a crossfade as it glides, tick by tick; Touch lets
//! go when the recall ends (as at the end of a gesture), Latch and Write
//! go on writing until playback stops.
//!
//! **Control:** MIDI learn targets ([`SceneControl`]) recall a scene,
//! the next or the previous (after the one recalled last), and move the
//! morph — a morph from a controller ends as one undo step once the
//! control has rested for [`MORPH_REST`]. Control surfaces send the same
//! as [`faderframe_control::SurfaceCommand`]s (OSC `/scene/…`). In show
//! mode a song naming a scene recalls it when the show stands on it.

use crate::{Action, NoticeLevel, Result, Session, SessionError};
use faderframe_core::{MixSceneId, ParameterId, PluginInstanceId, TrackId};
use faderframe_plugin_host::ParameterUnit;
use faderframe_project::scenes::{
    DeviceValues, MixScene, ParamGlide, SceneFade, SceneMix, SceneScope, ScopePart, SwitchPoint,
    blend, commands_to, explicit_value,
};
use faderframe_project::{Command, Impact, PluginSlot, SceneControl, Track};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

/// A morph from a controller ends (one undo step) once the control has
/// not moved for this long.
pub const MORPH_REST: Duration = Duration::from_millis(600);
/// How long a morph's step or a compare's switch glides on the audio
/// thread.
const STEP_GLIDE_SECONDS: f64 = 0.03;

/// A side of a scene compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneSide {
    A,
    B,
}

/// What the Scenes view, menus, controllers and scripts ask of the mix
/// scenes.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneOp {
    /// Store the console as a new scene (`plugins`: with the devices'
    /// parameters and states), named `name` or "Scene n".
    Store {
        name: Option<String>,
        plugins: bool,
    },
    /// Store the console into a scene again (`plugins`: keep the devices'
    /// values or not; `None`: as it did).
    Update {
        scene: MixSceneId,
        plugins: Option<bool>,
    },
    Rename(MixSceneId, String),
    /// Ask for a scene's new name (the shell's prompt, then `Rename`).
    PromptRename(MixSceneId),
    /// Move a scene to `to` in the list.
    Move {
        scene: MixSceneId,
        to: usize,
    },
    Delete(MixSceneId),
    /// A part of a scene's recall on or off.
    SetScope {
        scene: MixSceneId,
        part: ScopePart,
        on: bool,
    },
    /// A track safe from a scene's recalls, or not.
    SetSafe {
        scene: MixSceneId,
        track: TrackId,
        safe: bool,
    },
    /// A scene's own recall (`None`: the project's), or the project's
    /// (`scene: None`; `fade: None` is then instant).
    SetFade {
        scene: Option<MixSceneId>,
        fade: Option<SceneFade>,
    },
    /// The scenes A and B (compare, morph).
    SetAB {
        a: Option<MixSceneId>,
        b: Option<MixSceneId>,
    },
    /// Recall a scene: as it says (`fade: None`) or with this recall.
    Recall {
        scene: MixSceneId,
        fade: Option<SceneFade>,
    },
    /// The scene after (before) the one recalled last.
    Next,
    Previous,
    /// A crossfade stops where it is (one undo step).
    StopFade,
    /// A crossfade goes to its end at once.
    FinishFade,
    /// The console at `position` (0 = A … 1 = B) between scenes A and B.
    Morph {
        position: f64,
    },
    /// The morph ends where it is (one undo step).
    EndMorph,
    /// Compare scene `b` with scene `a` (`None`: the mix as it is); B is
    /// heard first.
    CompareStart {
        a: Option<MixSceneId>,
        b: MixSceneId,
    },
    CompareHear(SceneSide),
    CompareFlip,
    /// The compare ends: `keep` keeps the side heard (one undo step),
    /// else the mix is back as it was.
    CompareEnd {
        keep: bool,
    },
}

/// Parameters the unrecorded work changed, by what they set (the first
/// inverse of each restores it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum HeldKey {
    Volume(TrackId),
    Pan(TrackId),
    PanMode(TrackId),
    DualPan(TrackId),
    Surround(TrackId),
    Mute(TrackId),
    Solo(TrackId),
    Phase(TrackId),
    Delay(TrackId),
    SendLevel(TrackId, faderframe_core::SendId),
    SendPan(TrackId, faderframe_core::SendId),
    SendOn(TrackId, faderframe_core::SendId),
    CueSend(TrackId, faderframe_core::CueId),
    Bypass(PluginInstanceId),
    Param(PluginInstanceId, ParameterId),
    State(PluginInstanceId),
    Drive,
    Other(usize),
}

fn held_key(c: &Command, n: usize) -> HeldKey {
    match *c {
        Command::SetTrackVolume { track, .. } => HeldKey::Volume(track),
        Command::SetTrackPan { track, .. } => HeldKey::Pan(track),
        Command::SetTrackPanMode { track, .. } => HeldKey::PanMode(track),
        Command::SetTrackDualPan { track, .. } => HeldKey::DualPan(track),
        Command::SetTrackSurround { track, .. } => HeldKey::Surround(track),
        Command::SetTrackMute { track, .. } => HeldKey::Mute(track),
        Command::SetTrackSolo { track, .. } => HeldKey::Solo(track),
        Command::SetTrackPhaseInvert { track, .. } => HeldKey::Phase(track),
        Command::SetTrackDelay { track, .. } => HeldKey::Delay(track),
        Command::SetSendLevel { track, send, .. } => HeldKey::SendLevel(track, send),
        Command::SetSendPan { track, send, .. } => HeldKey::SendPan(track, send),
        Command::SetSendEnabled { track, send, .. } => HeldKey::SendOn(track, send),
        Command::SetCueSend { track, cue, .. } => HeldKey::CueSend(track, cue),
        Command::SetPluginBypass { plugin, .. } => HeldKey::Bypass(plugin),
        Command::SetPluginParameter {
            plugin, parameter, ..
        } => HeldKey::Param(plugin, parameter),
        Command::SetPluginState { plugin, .. } => HeldKey::State(plugin),
        Command::SetConsoleDrive { .. } => HeldKey::Drive,
        _ => HeldKey::Other(n),
    }
}

/// Edits applied but not recorded: the first inverse of each parameter
/// (in order) and the last command that set it.
#[derive(Debug, Default)]
struct Held {
    inverses: Vec<Command>,
    keys: std::collections::HashSet<HeldKey>,
    latest: HashMap<HeldKey, (u64, Command)>,
    seq: u64,
}

impl Held {
    fn is_empty(&self) -> bool {
        self.inverses.is_empty()
    }

    /// The commands that set what is held, in the order they last did.
    fn forward(&self) -> Vec<Command> {
        let mut v: Vec<&(u64, Command)> = self.latest.values().collect();
        v.sort_by_key(|(seq, _)| *seq);
        v.into_iter().map(|(_, c)| c.clone()).collect()
    }
}

/// A crossfade under way.
#[derive(Debug)]
struct Fade {
    scene: MixSceneId,
    name: String,
    from: SceneMix,
    to: SceneMix,
    scope: SceneScope,
    safe: BTreeSet<TrackId>,
    switch: SwitchPoint,
    seconds: f64,
    started: Instant,
    /// Device parameters where they were (those without explicit values).
    params: HashMap<(PluginInstanceId, ParameterId), f64>,
    /// How each device parameter moves (empty without the devices).
    glides: HashMap<(PluginInstanceId, ParameterId), ParamGlide>,
    /// The engine glides its continuous values.
    glided: bool,
}

/// A morph under way.
#[derive(Debug)]
struct Morph {
    a: MixSceneId,
    b: MixSceneId,
    position: f64,
    moved: Instant,
    /// From a controller: ends once it rests.
    by_control: bool,
    /// How each device parameter moves (empty without the devices).
    glides: HashMap<(PluginInstanceId, ParameterId), ParamGlide>,
}

/// A compare under way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Compare {
    a: Option<MixSceneId>,
    b: MixSceneId,
    hearing: SceneSide,
}

#[derive(Debug)]
enum Work {
    Fade(Box<Fade>),
    Morph(Morph),
    Compare(Compare),
}

/// The mix scenes' state in the session (not saved).
#[derive(Debug, Default)]
pub struct SceneState {
    /// The scene recalled last (Next and Previous go on from it).
    current: Option<MixSceneId>,
    held: Held,
    work: Option<Work>,
}

/// What the Scenes view shows of the work under way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SceneActivity {
    Fading {
        scene: MixSceneId,
        progress: f64,
    },
    Morphing {
        a: MixSceneId,
        b: MixSceneId,
        position: f64,
    },
    Comparing {
        a: Option<MixSceneId>,
        b: MixSceneId,
        hearing: SceneSide,
    },
}

/// Actions that leave scene work going (they do not edit the project).
fn keeps_scene_work(action: &Action) -> bool {
    if let Action::Several(all) = action {
        return all.iter().all(keeps_scene_work);
    }
    // The media browser's own (previews, search, tags) edit nothing.
    if let Action::Browser(op) = action {
        return !op.edits();
    }
    // Looking round a 360° picture edits nothing.
    if let Action::Video(crate::video::VideoOp::Sphere(op)) = action {
        return !op.edits();
    }
    // Nor an interactive score's preview and export.
    if let Action::Adaptive(op) = action {
        return !op.edits();
    }
    // Nor the map editor's own (showing a map, learning, user files).
    if let Action::Maps(op) = action {
        return !op.edits();
    }
    matches!(
        action,
        Action::MixScene(_)
            | Action::Transport(_)
            | Action::Workspace(_)
            | Action::SelectTracks { .. }
            | Action::SelectClips { .. }
            | Action::SelectNotes { .. }
            | Action::ClearSelection
            | Action::Zoom(_)
            | Action::SetEditRange(_)
            | Action::SetMonoCheck(_)
            | Action::SetHeadphones(_)
            | Action::Head(_)
            | Action::OpenPluginEditor { .. }
            | Action::SetDeviceView { .. }
            | Action::SetPluginWindowOnTop { .. }
            | Action::SetAnalysisSource(_)
            | Action::ResetAnalysis
            | Action::ShowGlobalLane(..)
            | Action::SetGrid(_)
            | Action::ToggleSnap
            | Action::SetStripWidth { .. }
            | Action::SetMeterMode { .. }
            | Action::SetVuReference(_)
            | Action::SetMeterBridge(_)
            | Action::SetTrackHeight { .. }
            | Action::ResetClipIndicators
            | Action::AuditionOff
            | Action::ResetPerformance
            | Action::MidiLearn(_)
            | Action::CancelMidiLearn
            | Action::Compare(_)
            | Action::Codec(_)
            | Action::SaveQcReport { .. }
            | Action::NullTestStems
            | Action::KeepPluginLive { .. }
            | Action::DownloadSeparationModel(_)
            | Action::CancelSeparations
            | Action::SetTimingAlign(_)
            | Action::WebRemote(_)
            | Action::Device(
                crate::device_states::DeviceOp::Watch { .. }
                    | crate::device_states::DeviceOp::MatchLevels { .. }
            )
            | Action::Score(
                crate::score::ScoreOp::Show(_)
                    | crate::score::ScoreOp::Settings(_)
                    | crate::score::ScoreOp::Cursor(_)
            )
            | Action::Collab(
                crate::collab::CollabOp::Chat(_)
                    | crate::collab::CollabOp::ListenAlong(_)
                    | crate::collab::CollabOp::NewInvite
            )
    )
}

/// An edit that leaves the console alone (automation written, names …):
/// scene work goes on through it.
fn leaves_the_mix(cmd: &Command) -> bool {
    match cmd {
        Command::Batch { commands, .. } => commands.iter().all(leaves_the_mix),
        Command::AddAutomationLane { .. }
        | Command::RemoveAutomationLane { .. }
        | Command::SetAutomationLane { .. } => true,
        c => c.impact() == Impact::None,
    }
}

impl Session {
    // --- reading -------------------------------------------------------

    /// The project's mix scenes.
    pub fn mix_scenes(&self) -> &faderframe_project::scenes::MixScenes {
        &self.project.mix_scenes
    }

    /// The scene recalled last.
    pub fn current_mix_scene(&self) -> Option<MixSceneId> {
        self.scenes.current
    }

    /// The crossfade, morph or compare under way.
    pub fn scene_activity(&self) -> Option<SceneActivity> {
        Some(match self.scenes.work.as_ref()? {
            Work::Fade(f) => SceneActivity::Fading {
                scene: f.scene,
                progress: fade_progress(f),
            },
            Work::Morph(m) => SceneActivity::Morphing {
                a: m.a,
                b: m.b,
                position: m.position,
            },
            Work::Compare(c) => SceneActivity::Comparing {
                a: c.a,
                b: c.b,
                hearing: c.hearing,
            },
        })
    }

    /// Does scene work move the console (views animate)?
    pub fn scenes_moving(&self) -> bool {
        matches!(self.scenes.work, Some(Work::Fade(_) | Work::Morph(_)))
    }

    /// A scene's name ("(removed scene)" when it is gone).
    pub fn mix_scene_name(&self, id: MixSceneId) -> String {
        self.project
            .mix_scenes
            .get(id)
            .map_or_else(|| "(removed scene)".to_string(), |s| s.name.clone())
    }

    /// Does the console hold what `scene` would recall (nothing left to
    /// do)?
    pub fn mix_scene_matches(&self, scene: MixSceneId) -> bool {
        let Some(s) = self.project.mix_scenes.get(scene) else {
            return false;
        };
        commands_to(&self.project, &s.mix, &s.scope, &s.safe, &|_, _| None)
            .iter()
            .all(|c| matches!(c, Command::SetPluginParameter { .. }))
    }

    /// The Mix Scenes entries of a track's menu: store, recall each, the
    /// track safe from each scene, the view.
    pub fn mix_scene_menu(&self, track: TrackId) -> Vec<crate::GroupMenuEntry> {
        let entry = |label: String, action: Option<Action>, checked: Option<bool>, sep: bool| {
            crate::GroupMenuEntry {
                label,
                action,
                checked,
                separated: sep,
            }
        };
        let mut out = vec![entry(
            "Store Mix Scene".into(),
            Some(Action::MixScene(SceneOp::Store {
                name: None,
                plugins: false,
            })),
            None,
            false,
        )];
        let scenes = &self.project.mix_scenes.scenes;
        for (i, s) in scenes.iter().enumerate() {
            out.push(entry(
                format!("Recall ‘{}’", s.name),
                Some(Action::MixScene(SceneOp::Recall {
                    scene: s.id,
                    fade: None,
                })),
                Some(self.scenes.current == Some(s.id)),
                i == 0,
            ));
        }
        let name = self
            .project
            .track(track)
            .map_or_else(String::new, |t| t.name.clone());
        for (i, s) in scenes.iter().enumerate() {
            let safe = s.safe.contains(&track);
            out.push(entry(
                format!("‘{name}’ Safe from ‘{}’", s.name),
                Some(Action::MixScene(SceneOp::SetSafe {
                    scene: s.id,
                    track,
                    safe: !safe,
                })),
                Some(safe),
                i == 0,
            ));
        }
        out.push(entry(
            "Show Mix Scenes".into(),
            Some(Action::Workspace(crate::WorkspaceAction::ShowView(
                faderframe_workspace::ViewId::mix_scenes(),
            ))),
            None,
            true,
        ));
        out
    }

    /// "Recall ‘Verse’" for a controller's mapping.
    pub(crate) fn scene_control_label(&self, control: SceneControl) -> String {
        match control {
            SceneControl::Recall { scene } => {
                format!("Mix Scenes · Recall ‘{}’", self.mix_scene_name(scene))
            }
            c => format!("Mix Scenes · {}", c.label()),
        }
    }

    // --- the work's life -------------------------------------------------

    /// Before `action` is dispatched: scene work ends unless the action
    /// leaves the project alone (see the module docs).
    pub(crate) fn scenes_before(&mut self, action: &Action) -> Result<()> {
        if self.scenes.work.is_none() || keeps_scene_work(action) {
            return Ok(());
        }
        self.settle_scenes()
    }

    /// Before an edit reaches the history: scene work ends unless the edit
    /// leaves the console alone.
    pub(crate) fn scenes_before_edit(&mut self, cmd: &Command) {
        if self.scenes.work.is_some()
            && !leaves_the_mix(cmd)
            && let Err(e) = self.settle_scenes()
        {
            self.notify(NoticeLevel::Warning, format!("mix scenes: {e}"));
        }
    }

    /// End the work under way: a crossfade where it is, a morph where it
    /// stands, a compare on the side heard (before saving, rendering and
    /// edits).
    pub(crate) fn settle_scenes(&mut self) -> Result<()> {
        match self.scenes.work.as_ref() {
            None => Ok(()),
            Some(Work::Fade(_)) => self.stop_fade(),
            Some(Work::Morph(_)) => self.end_morph(),
            Some(Work::Compare(_)) => self.end_compare(true),
        }
    }

    /// A new project: the work and what it held are gone with the old one.
    pub(crate) fn forget_scenes(&mut self) {
        self.scenes = SceneState::default();
        if let Err(e) = self.engine.release_glide(true) {
            tracing::warn!("mix scenes: {e}");
        }
    }

    /// From the session tick: move a crossfade on, end a controller's
    /// morph that rests.
    pub(crate) fn tick_scenes(&mut self) {
        let done = match &self.scenes.work {
            Some(Work::Fade(_)) => self.run_fade(),
            Some(Work::Morph(m)) if m.by_control && m.moved.elapsed() >= MORPH_REST => {
                self.end_morph()
            }
            _ => Ok(()),
        };
        if let Err(e) = done {
            self.notify(NoticeLevel::Error, format!("mix scenes: {e}"));
        }
    }

    // --- held edits ------------------------------------------------------

    /// Apply `commands` unrecorded (see the module docs); a command that
    /// does not apply is passed over. Returns how much the engine must
    /// update (not synced here).
    fn hold(&mut self, commands: Vec<Command>) -> Impact {
        let mut impact = Impact::None;
        for c in commands {
            let n = self.scenes.held.inverses.len();
            let key = held_key(&c, n);
            let forward = c.clone();
            let i = c.impact();
            match c.apply(&mut self.project) {
                Ok(inv) => {
                    impact = impact.max(i);
                    let held = &mut self.scenes.held;
                    if held.keys.insert(key) {
                        held.inverses.push(inv);
                    }
                    held.seq += 1;
                    held.latest.insert(key, (held.seq, forward));
                }
                Err(e) => tracing::debug!("mix scenes: {e}"),
            }
        }
        impact
    }

    fn hold_and_sync(&mut self, commands: Vec<Command>) -> Result<()> {
        // While playing, lanes that write record what moves.
        for c in &commands {
            self.capture_automation(c);
        }
        let impact = self.hold(commands);
        if impact > Impact::None {
            self.sync(impact)?;
        }
        Ok(())
    }

    /// Take everything held back (not synced); returns the impact.
    fn let_go(&mut self) -> Impact {
        let held = std::mem::take(&mut self.scenes.held);
        let mut impact = Impact::None;
        for inv in held.inverses.into_iter().rev() {
            impact = impact.max(inv.impact());
            if let Err(e) = inv.apply(&mut self.project) {
                tracing::warn!("mix scenes: could not take an edit back: {e}");
            }
        }
        impact
    }

    /// Make what is held one undo step named `label`: the edits taken
    /// back and their last values applied as one batch — the console
    /// stays exactly where it is, so the engine has it already.
    fn commit_held(&mut self, label: String) -> Result<()> {
        if self.scenes.held.is_empty() {
            return Ok(());
        }
        let forward = self.scenes.held.forward();
        let back = self.let_go();
        let applied = self.history.apply(
            &mut self.project,
            Command::Batch {
                label,
                commands: forward,
            },
        );
        match applied {
            Ok(_) => self.sync(Impact::Params),
            Err(e) => {
                // The console as it was before the work began.
                self.sync(back)?;
                Err(e.into())
            }
        }
    }

    // --- plugins ---------------------------------------------------------

    /// A device's automatable parameter values and state now (the
    /// project's explicit values first, else the plugin's own).
    fn device_values(&mut self, slot: &PluginSlot) -> DeviceValues {
        let parameters = match self.engine.plugin_parameters(slot.id) {
            Some(list) => {
                let ids: Vec<ParameterId> = list
                    .iter()
                    .filter(|p| p.automatable)
                    .map(|p| p.id)
                    .collect();
                ids.into_iter()
                    .filter_map(|id| {
                        let value = explicit_value(slot, id)
                            .or_else(|| self.engine.plugin_parameter_value(slot.id, id))?;
                        Some(faderframe_project::SavedParameter { id, value })
                    })
                    .collect()
            }
            None => slot.parameters.clone(),
        };
        DeviceValues {
            parameters,
            state: slot.state.clone(),
        }
    }

    /// The console now; with the devices' values when `plugins`.
    fn capture_mix(&mut self, plugins: bool) -> SceneMix {
        if !plugins {
            return SceneMix::capture(&self.project, &mut |_, _| None);
        }
        // Third-party plugins' states as they are now.
        self.capture_plugin_states();
        let slots: Vec<PluginSlot> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| t.slots().into_iter().cloned())
            .collect();
        let mut values: HashMap<PluginInstanceId, DeviceValues> = HashMap::new();
        for s in &slots {
            let v = self.device_values(s);
            values.insert(s.id, v);
        }
        SceneMix::capture(&self.project, &mut |_: &Track, s: &PluginSlot| {
            values.get(&s.id).cloned()
        })
    }

    /// How each device parameter moves in a blend.
    fn param_glides(&self) -> HashMap<(PluginInstanceId, ParameterId), ParamGlide> {
        let mut out = HashMap::new();
        for t in &self.project.tracks {
            for s in t.slots() {
                let Some(list) = self.engine.plugin_parameters(s.id) else {
                    continue;
                };
                for p in list {
                    let glide = if p.stepped || !p.automatable {
                        ParamGlide::Switch
                    } else if p.unit == ParameterUnit::Hertz && p.min > 0.0 {
                        ParamGlide::Log
                    } else {
                        ParamGlide::Linear
                    };
                    out.insert((s.id, p.id), glide);
                }
            }
        }
        out
    }

    /// The commands that set the console to `target` (frozen tracks'
    /// devices left alone: their plugins are not running).
    fn scene_commands(
        &self,
        target: &SceneMix,
        scope: &SceneScope,
        safe: &BTreeSet<TrackId>,
        params: &HashMap<(PluginInstanceId, ParameterId), f64>,
    ) -> Vec<Command> {
        let now = |slot: &PluginSlot, id: ParameterId| params.get(&(slot.id, id)).copied();
        commands_to(&self.project, target, scope, safe, &now)
            .into_iter()
            .filter(|c| self.frozen_target(c).is_none())
            .filter(|c| {
                !matches!(
                    c,
                    Command::SetPluginBypass { track, .. }
                    | Command::SetPluginParameter { track, .. }
                        if self.project.track(*track).is_some_and(|t| t.freeze.is_some())
                )
            })
            .collect()
    }

    /// Device parameters as they are now (those `mix` holds), for
    /// comparing with a scene's.
    fn params_of(mix: &SceneMix) -> HashMap<(PluginInstanceId, ParameterId), f64> {
        let mut out = HashMap::new();
        for t in mix.tracks.values() {
            for d in &t.devices {
                for p in d.values.iter().flat_map(|v| &v.parameters) {
                    out.insert((d.plugin, p.id), p.value);
                }
            }
        }
        out
    }

    // --- ops -------------------------------------------------------------

    pub(crate) fn scene_op(&mut self, op: SceneOp) -> Result<()> {
        match op {
            SceneOp::Store { name, plugins } => self.store_scene(name, plugins)?,
            SceneOp::Update { scene, plugins } => self.update_scene(scene, plugins)?,
            SceneOp::Rename(scene, name) => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Err(SessionError::Other("a scene needs a name".into()));
                }
                self.change_scene(scene, "Rename Mix Scene", |s| s.name = name)?;
            }
            SceneOp::PromptRename(scene) => {
                if self.project.mix_scenes.get(scene).is_some() {
                    self.ui_requests
                        .push(crate::UiRequest::RenameMixScene(scene));
                }
            }
            SceneOp::Move { scene, to } => {
                let Some(s) = self.project.mix_scenes.get(scene).cloned() else {
                    return Ok(());
                };
                self.edit(Command::Batch {
                    label: "Move Mix Scene".into(),
                    commands: vec![Command::SetMixScene {
                        scene,
                        value: Some(Box::new(s)),
                        index: to,
                    }],
                })?;
            }
            SceneOp::Delete(scene) => {
                if self.project.mix_scenes.get(scene).is_none() {
                    return Ok(());
                }
                self.settle_scenes()?;
                let mut commands = vec![Command::SetMixScene {
                    scene,
                    value: None,
                    index: 0,
                }];
                // Songs that recalled it recall nothing now (same step).
                if self
                    .project
                    .setlist
                    .songs
                    .iter()
                    .any(|s| s.scene == Some(scene))
                {
                    let mut list = self.project.setlist.clone();
                    for s in &mut list.songs {
                        if s.scene == Some(scene) {
                            s.scene = None;
                        }
                    }
                    commands.push(Command::SetSetlist {
                        setlist: Box::new(list),
                    });
                }
                self.edit(Command::Batch {
                    label: format!("Delete Mix Scene ‘{}’", self.mix_scene_name(scene)),
                    commands,
                })?;
                if self.scenes.current == Some(scene) {
                    self.scenes.current = None;
                }
            }
            SceneOp::SetScope { scene, part, on } => {
                self.change_scene(scene, "Mix Scene Scope", |s| {
                    s.scope = s.scope.with(part, on)
                })?;
            }
            SceneOp::SetSafe { scene, track, safe } => {
                self.change_scene(scene, "Mix Scene Safe Tracks", |s| {
                    if safe {
                        s.safe.insert(track);
                    } else {
                        s.safe.remove(&track);
                    }
                })?;
            }
            SceneOp::SetFade {
                scene: Some(id),
                fade,
            } => {
                self.change_scene(id, "Mix Scene Crossfade", |s| s.fade = fade)?;
            }
            SceneOp::SetFade { scene: None, fade } => {
                let m = &self.project.mix_scenes;
                let (a, b) = (m.a, m.b);
                self.edit(Command::SetMixSceneOptions {
                    fade: fade.unwrap_or_default(),
                    a,
                    b,
                })?;
            }
            SceneOp::SetAB { a, b } => {
                let fade = self.project.mix_scenes.fade;
                self.edit(Command::SetMixSceneOptions { fade, a, b })?;
            }
            SceneOp::Recall { scene, fade } => self.recall_scene(scene, fade)?,
            SceneOp::Next | SceneOp::Previous => {
                let scenes = &self.project.mix_scenes.scenes;
                if scenes.is_empty() {
                    return Ok(());
                }
                let at = self
                    .scenes
                    .current
                    .and_then(|c| self.project.mix_scenes.index_of(c));
                let i = match (op, at) {
                    (SceneOp::Next, Some(i)) => (i + 1).min(scenes.len() - 1),
                    (SceneOp::Next, None) => 0,
                    (_, Some(i)) => i.saturating_sub(1),
                    (_, None) => scenes.len() - 1,
                };
                let id = scenes[i].id;
                if Some(id) != self.scenes.current || at.is_none() {
                    self.recall_scene(id, None)?;
                }
            }
            SceneOp::StopFade => self.stop_fade()?,
            SceneOp::FinishFade => self.finish_fade()?,
            SceneOp::Morph { position } => self.morph(position, false)?,
            SceneOp::EndMorph => self.end_morph()?,
            SceneOp::CompareStart { a, b } => self.start_compare(a, b)?,
            SceneOp::CompareHear(side) => self.compare_hear(side)?,
            SceneOp::CompareFlip => {
                if let Some(Work::Compare(c)) = &self.scenes.work {
                    let side = match c.hearing {
                        SceneSide::A => SceneSide::B,
                        SceneSide::B => SceneSide::A,
                    };
                    self.compare_hear(side)?;
                }
            }
            SceneOp::CompareEnd { keep } => self.end_compare(keep)?,
        }
        self.revision += 1;
        Ok(())
    }

    /// Replace a scene with what `change` makes of it (one undo step).
    fn change_scene(
        &mut self,
        id: MixSceneId,
        label: &str,
        change: impl FnOnce(&mut MixScene),
    ) -> Result<()> {
        let Some(index) = self.project.mix_scenes.index_of(id) else {
            return Ok(());
        };
        let mut s = self.project.mix_scenes.scenes[index].clone();
        change(&mut s);
        if Some(&s) == self.project.mix_scenes.get(id) {
            return Ok(());
        }
        self.edit(Command::Batch {
            label: label.into(),
            commands: vec![Command::SetMixScene {
                scene: id,
                value: Some(Box::new(s)),
                index,
            }],
        })
    }

    fn store_scene(&mut self, name: Option<String>, plugins: bool) -> Result<()> {
        // What is stored is what is heard: a compare's side, a fade's end.
        self.settle_scenes()?;
        let n = self.project.mix_scenes.scenes.len();
        let name = name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                (n + 1..)
                    .map(|i| format!("Scene {i}"))
                    .find(|s| !self.project.mix_scenes.scenes.iter().any(|x| x.name == *s))
                    .unwrap_or_default()
            });
        let mix = self.capture_mix(plugins);
        let id: MixSceneId = self.project.ids.allocate();
        let scene = MixScene {
            id,
            name: name.clone(),
            mix,
            scope: SceneScope::default().with(ScopePart::Plugins, plugins),
            safe: BTreeSet::new(),
            fade: None,
        };
        self.edit(Command::Batch {
            label: format!("Store Mix Scene ‘{name}’"),
            commands: vec![Command::SetMixScene {
                scene: id,
                value: Some(Box::new(scene)),
                index: n,
            }],
        })?;
        self.scenes.current = Some(id);
        Ok(())
    }

    fn update_scene(&mut self, id: MixSceneId, plugins: Option<bool>) -> Result<()> {
        let Some(old) = self.project.mix_scenes.get(id).cloned() else {
            return Ok(());
        };
        self.settle_scenes()?;
        let plugins = plugins.unwrap_or_else(|| old.mix.keeps_plugins());
        let mix = self.capture_mix(plugins);
        self.change_scene(id, &format!("Update Mix Scene ‘{}’", old.name), |s| {
            s.mix = mix;
            if plugins != old.mix.keeps_plugins() {
                s.scope = s.scope.with(ScopePart::Plugins, plugins);
            }
        })?;
        self.scenes.current = Some(id);
        Ok(())
    }

    // --- recall and crossfade -----------------------------------------------

    fn recall_scene(&mut self, id: MixSceneId, fade: Option<SceneFade>) -> Result<()> {
        let Some(scene) = self.project.mix_scenes.get(id).cloned() else {
            return Err(SessionError::Other("that mix scene is gone".into()));
        };
        // A morph or a compare ends first; a crossfade under way goes on
        // into this one from where it is (still one undo step).
        match self.scenes.work {
            Some(Work::Morph(_)) => self.end_morph()?,
            Some(Work::Compare(_)) => self.end_compare(false)?,
            _ => {}
        }
        let fade = fade.unwrap_or_else(|| self.project.mix_scenes.fade_of(&scene));
        let bpm = self.project.timeline.tempo.bpm_at(self.playhead());
        let seconds = fade.length.seconds(bpm);
        let plugins = scene.scope.plugins && scene.mix.keeps_plugins();
        let label = format!("Recall Mix Scene ‘{}’", scene.name);
        let from = self.capture_mix(plugins);
        let params = Self::params_of(&from);
        let commands = self.scene_commands(&scene.mix, &scene.scope, &scene.safe, &params);
        self.scenes.current = Some(id);
        if seconds <= 0.0 || commands.is_empty() {
            if matches!(self.scenes.work, Some(Work::Fade(_))) {
                // The fade under way ends here, at this scene, at once.
                self.scenes.work = None;
                self.hold_and_sync(commands)?;
                self.engine.release_glide(true)?;
                self.commit_held(label)?;
                self.automation_gesture_ended();
                return Ok(());
            }
            for c in &commands {
                self.capture_automation(c);
            }
            self.batch(&label, commands)?;
            self.automation_gesture_ended();
            return Ok(());
        }
        // The sound glides on the audio thread while a stream runs: its
        // targets are the scene's values (worked out on the project with
        // them, which is then put back as it was).
        let glided = self.pending.is_none();
        if glided {
            let mut back = Vec::new();
            for c in commands {
                if let Ok(inv) = c.apply(&mut self.project) {
                    back.push(inv);
                }
            }
            let glide = self.engine.glide_mix(&self.project, seconds);
            for inv in back.into_iter().rev() {
                let _ = inv.apply(&mut self.project);
            }
            glide?;
        }
        self.scenes.work = Some(Work::Fade(Box::new(Fade {
            scene: id,
            name: scene.name.clone(),
            from,
            to: scene.mix.clone(),
            scope: scene.scope,
            safe: scene.safe.clone(),
            switch: fade.switch,
            seconds,
            started: Instant::now(),
            params,
            glides: if plugins {
                self.param_glides()
            } else {
                HashMap::new()
            },
            glided,
        })));
        self.run_fade()
    }

    /// Move the crossfade to where its time says (from the tick).
    fn run_fade(&mut self) -> Result<()> {
        let Some(Work::Fade(f)) = &self.scenes.work else {
            return Ok(());
        };
        let t = fade_progress(f);
        if t >= 1.0 {
            return self.finish_fade();
        }
        let glide = |p: PluginInstanceId, q: ParameterId| {
            f.glides.get(&(p, q)).copied().unwrap_or(ParamGlide::Switch)
        };
        let state = blend(&f.from, &f.to, t, f.switch, &glide);
        let commands = self.scene_commands(&state, &f.scope, &f.safe, &f.params);
        self.hold_and_sync(commands)
    }

    /// The crossfade at its end at once (from the tick when its time is
    /// up, or asked for): the scene's values, one undo step.
    fn finish_fade(&mut self) -> Result<()> {
        let Some(Work::Fade(f)) = self.scenes.work.take() else {
            return Ok(());
        };
        let commands = self.scene_commands(&f.to, &f.scope, &f.safe, &f.params);
        self.hold_and_sync(commands)?;
        // The audio thread's glide ends on these values: let go once it
        // has (at once without a stream).
        self.engine.release_glide(!f.glided)?;
        self.commit_held(format!("Recall Mix Scene ‘{}’", f.name))?;
        self.automation_gesture_ended();
        Ok(())
    }

    /// The crossfade stops where it is: one undo step.
    fn stop_fade(&mut self) -> Result<()> {
        let Some(Work::Fade(f)) = self.scenes.work.take() else {
            return Ok(());
        };
        self.engine.release_glide(true)?;
        let t = fade_progress(&f);
        let glide = |p: PluginInstanceId, q: ParameterId| {
            f.glides.get(&(p, q)).copied().unwrap_or(ParamGlide::Switch)
        };
        let state = blend(&f.from, &f.to, t, f.switch, &glide);
        let commands = self.scene_commands(&state, &f.scope, &f.safe, &f.params);
        self.hold_and_sync(commands)?;
        self.commit_held(format!("Recall Mix Scene ‘{}’ (Stopped)", f.name))?;
        self.automation_gesture_ended();
        Ok(())
    }

    // --- morph ---------------------------------------------------------------

    /// The scenes A and B, or why there are none.
    fn scenes_ab(&self) -> Result<(MixScene, MixScene)> {
        let m = &self.project.mix_scenes;
        let get = |id: Option<MixSceneId>| id.and_then(|id| m.get(id)).cloned();
        match (get(m.a), get(m.b)) {
            (Some(a), Some(b)) => Ok((a, b)),
            _ => Err(SessionError::Other(
                "choose the scenes A and B first (Scenes view)".into(),
            )),
        }
    }

    /// The console `position` of the way from scene A to scene B
    /// (unrecorded until the morph ends).
    pub(crate) fn morph(&mut self, position: f64, by_control: bool) -> Result<()> {
        let (a, b) = self.scenes_ab()?;
        let position = if position.is_nan() {
            0.0
        } else {
            position.clamp(0.0, 1.0)
        };
        let same = matches!(&self.scenes.work, Some(Work::Morph(m)) if m.a == a.id && m.b == b.id);
        if !same {
            self.settle_scenes()?;
        }
        let plugins = a.scope.plugins && b.scope.plugins;
        let switch = self.project.mix_scenes.fade_of(&b).switch;
        let glides = match self.scenes.work.take() {
            Some(Work::Morph(m)) if same => m.glides,
            _ if plugins => self.param_glides(),
            _ => HashMap::new(),
        };
        let glide = |p: PluginInstanceId, q: ParameterId| {
            glides.get(&(p, q)).copied().unwrap_or(ParamGlide::Switch)
        };
        let state = blend(&a.mix, &b.mix, position, switch, &glide);
        let scope = a.scope.and(b.scope).with(ScopePart::Plugins, plugins);
        let safe: BTreeSet<TrackId> = a.safe.union(&b.safe).copied().collect();
        let params = if plugins {
            self.current_params(&state)
        } else {
            HashMap::new()
        };
        let commands = self.scene_commands(&state, &scope, &safe, &params);
        for c in &commands {
            self.capture_automation(c);
        }
        let impact = self.hold(commands);
        self.scenes.work = Some(Work::Morph(Morph {
            a: a.id,
            b: b.id,
            position,
            moved: Instant::now(),
            by_control,
            glides,
        }));
        if self.pending.is_none() {
            self.engine.glide_mix(&self.project, STEP_GLIDE_SECONDS)?;
        }
        if impact > Impact::None {
            self.sync(impact)?;
        }
        Ok(())
    }

    /// Device parameters `mix` holds as the devices have them now.
    fn current_params(&mut self, mix: &SceneMix) -> HashMap<(PluginInstanceId, ParameterId), f64> {
        let mut out = HashMap::new();
        for (track, m) in &mix.tracks {
            for d in &m.devices {
                let Some(v) = &d.values else { continue };
                let slot = self
                    .project
                    .track(*track)
                    .and_then(|t| t.plugin(d.plugin))
                    .cloned();
                let Some(slot) = slot else { continue };
                for p in &v.parameters {
                    if explicit_value(&slot, p.id).is_none()
                        && let Some(value) = self.engine.plugin_parameter_value(slot.id, p.id)
                    {
                        out.insert((slot.id, p.id), value);
                    }
                }
            }
        }
        out
    }

    /// The morph ends where it stands: one undo step.
    fn end_morph(&mut self) -> Result<()> {
        let Some(Work::Morph(m)) = self.scenes.work.take() else {
            return Ok(());
        };
        self.engine.release_glide(false)?;
        let label = format!(
            "Morph ‘{}’ → ‘{}’ ({:.0} %)",
            self.mix_scene_name(m.a),
            self.mix_scene_name(m.b),
            m.position * 100.0
        );
        self.commit_held(label)?;
        if m.position >= 1.0 {
            self.scenes.current = Some(m.b);
        } else if m.position <= 0.0 {
            self.scenes.current = Some(m.a);
        }
        self.automation_gesture_ended();
        Ok(())
    }

    // --- compare -------------------------------------------------------------

    fn start_compare(&mut self, a: Option<MixSceneId>, b: MixSceneId) -> Result<()> {
        let m = &self.project.mix_scenes;
        if m.get(b).is_none() || a.is_some_and(|a| m.get(a).is_none()) {
            return Err(SessionError::Other("that mix scene is gone".into()));
        }
        self.settle_scenes()?;
        self.scenes.work = Some(Work::Compare(Compare {
            a,
            b,
            hearing: SceneSide::A,
        }));
        self.compare_hear(SceneSide::B)
    }

    /// Hear a side of the compare: the mix as it was, then (a scene's side)
    /// that scene recalled over it — unrecorded.
    fn compare_hear(&mut self, side: SceneSide) -> Result<()> {
        let Some(Work::Compare(mut c)) = self.scenes.work.take() else {
            return Ok(());
        };
        let scene = match side {
            SceneSide::A => c.a,
            SceneSide::B => Some(c.b),
        }
        .and_then(|id| self.project.mix_scenes.get(id).cloned());
        let back = self.let_go();
        let mut impact = back;
        if let Some(s) = scene {
            let plugins = s.scope.plugins && s.mix.keeps_plugins();
            if plugins {
                self.capture_plugin_states();
            }
            let params = if plugins {
                self.current_params(&s.mix)
            } else {
                HashMap::new()
            };
            let commands = self.scene_commands(&s.mix, &s.scope, &s.safe, &params);
            impact = impact.max(self.hold(commands));
        }
        c.hearing = side;
        self.scenes.work = Some(Work::Compare(c));
        if self.pending.is_none() {
            self.engine.glide_mix(&self.project, STEP_GLIDE_SECONDS)?;
        }
        if impact > Impact::None {
            self.sync(impact)?;
        }
        Ok(())
    }

    fn end_compare(&mut self, keep: bool) -> Result<()> {
        let Some(Work::Compare(c)) = self.scenes.work.take() else {
            return Ok(());
        };
        self.engine.release_glide(false)?;
        let heard = match c.hearing {
            SceneSide::A => c.a,
            SceneSide::B => Some(c.b),
        };
        match heard {
            Some(id) if keep => {
                self.commit_held(format!("Recall Mix Scene ‘{}’", self.mix_scene_name(id)))?;
                self.scenes.current = Some(id);
            }
            _ => {
                let back = self.let_go();
                // The sound glides back to the mix as it was.
                if self.pending.is_none() {
                    self.engine.glide_mix(&self.project, STEP_GLIDE_SECONDS)?;
                    self.engine.release_glide(false)?;
                }
                self.sync(back.max(Impact::Params))?;
            }
        }
        Ok(())
    }

    // --- controllers and show mode -----------------------------------------

    /// A controller's scene control: a press of a button, or the morph's
    /// value (0…1).
    pub(crate) fn scene_control(&mut self, control: SceneControl, value: f64) -> Result<()> {
        match control {
            SceneControl::Recall { scene } => {
                self.dispatch(Action::MixScene(SceneOp::Recall { scene, fade: None }))
            }
            SceneControl::Next => self.dispatch(Action::MixScene(SceneOp::Next)),
            SceneControl::Previous => self.dispatch(Action::MixScene(SceneOp::Previous)),
            SceneControl::Morph => self.morph(value, true),
        }
    }

    /// Show mode stands on a song: its scene, if it names one.
    pub(crate) fn song_scene(&mut self, song: usize) {
        let Some(id) = self.project.setlist.songs.get(song).and_then(|s| s.scene) else {
            return;
        };
        if self.project.mix_scenes.get(id).is_none() {
            return;
        }
        if let Err(e) = self.recall_scene(id, None) {
            self.notify(NoticeLevel::Warning, format!("show: {e}"));
        }
    }
}

/// How far a crossfade is by its time (0…1).
fn fade_progress(f: &Fade) -> f64 {
    if f.seconds <= 0.0 {
        return 1.0;
    }
    (f.started.elapsed().as_secs_f64() / f.seconds).clamp(0.0, 1.0)
}
